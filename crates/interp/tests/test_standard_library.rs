//! ry's standard library at runtime: the functions' results and errors, the agreement between the
//! runtime's table and the declarations in `types/ry.Rtypes`, and a property test that holds the
//! dictionary and set against a reference model. The interpreter is global to the process, so the
//! interpreter cases run as one test.
//!
//! The interpreter runs here without base R, so the setup defines the few functions base R would.

use std::{cell::RefCell, collections::BTreeSet, fs, path::Path};

use heap::{Heap, object::Sexp};
use interp::{eval, frame, lower, print, standard_library, unwind::Unwind, values};
use syntax::testing::{SplitMix64, fuzz_iterations};

const SETUP: &str = r#"
print <- function(x, ...) UseMethod("print")
print.default <- function(x, ...) .Internal(print.default(x, NULL, NULL))
as.list <- function(x, ...) UseMethod("as.list")
identical <- function(x, y) .Internal(identical(x, y, TRUE, TRUE, TRUE, TRUE, FALSE, TRUE, FALSE))
deparse <- function(expr) .Internal(deparse(expr, 500L, TRUE, 1089L, -1L))
typeof <- function(x) .Internal(typeof(x))
xs <- list(3L, 1L, 2L)
words <- list("pear", "fig", "apple")
d <- Dict.from_list(list(("a", 1), ("b", 2)))
s <- Set.from_list(list(1L, 2L, 3L))
"#;

/// Each case's code and what printing its value shows.
const PRINTS: &[(&str, &str)] = &[
    // Result.
    ("Result.is_ok(:Ok(1L))", "[1] TRUE\n"),
    ("Result.is_err(:Ok(1L))", "[1] FALSE\n"),
    ("Result.with_default(:Err(:Oops), 0L)", "[1] 0\n"),
    ("Result.map(:Ok(2L), function(x) x * 10L)", ":Ok(20L)\n"),
    (
        "Result.map(:Err(:Oops), function(x) x * 10L)",
        ":Err(:Oops)\n",
    ),
    (
        "Result.map_err(:Err(:Oops), function(e) :Wrapped(e))",
        ":Err(:Wrapped(:Oops))\n",
    ),
    (
        "Result.map_both(:Err(1L), function(x) x, function(e) e + 1L)",
        ":Err(2L)\n",
    ),
    (
        "Result.and_then(:Ok(4L), function(x) :Err(:TooBig))",
        ":Err(:TooBig)\n",
    ),
    (
        "Result.on_err(:Err(:Oops), function(e) :Ok(0L))",
        ":Ok(0L)\n",
    ),
    // List: construction and access.
    ("List.to_vector(List.from_vector(c(1, 2)))", "[1] 1 2\n"),
    ("List.to_vector(list())", "logical(0)\n"),
    ("List.len(xs)", "[1] 3\n"),
    ("List.is_empty(list())", "[1] TRUE\n"),
    ("List.get(xs, 2L)", ":Ok(1L)\n"),
    ("List.get(xs, 4L)", ":Err(:OutOfBounds)\n"),
    ("List.get(xs, 0L)", ":Err(:OutOfBounds)\n"),
    ("List.to_vector(List.set(xs, 1L, 9L))", "[1] 9 1 2\n"),
    ("List.to_vector(List.set(xs, 9L, 9L))", "[1] 3 1 2\n"),
    (
        "List.to_vector(List.update(xs, 2L, function(x) x + 1L))",
        "[1] 3 2 2\n",
    ),
    ("List.to_vector(List.append(xs, 4L))", "[1] 3 1 2 4\n"),
    ("List.to_vector(List.prepend(xs, 0L))", "[1] 0 3 1 2\n"),
    ("List.to_vector(List.concat(xs, list(7L)))", "[1] 3 1 2 7\n"),
    ("List.single(\"a\")", "[[1]]\n[1] \"a\"\n\n"),
    ("List.to_vector(List.repeat(0L, 3L))", "[1] 0 0 0\n"),
    ("List.to_vector(List.reverse(xs))", "[1] 2 1 3\n"),
    (
        "List.to_vector(List.join(list(list(1L), list(), list(2L, 3L))))",
        "[1] 1 2 3\n",
    ),
    // List: transformations.
    (
        "List.to_vector(List.map(xs, function(x) x * 2L))",
        "[1] 6 2 4\n",
    ),
    (
        "List.to_vector(List.map_with_index(xs, function(x, i) x * i))",
        "[1] 3 2 6\n",
    ),
    (
        "List.to_vector(List.map2(xs, list(10L, 20L), function(a, b) a + b))",
        "[1] 13 21\n",
    ),
    (
        "List.to_vector(List.join_map(xs, function(x) list(x, x)))",
        "[1] 3 3 1 1 2 2\n",
    ),
    (
        "List.to_vector(List.keep_if(xs, function(x) x > 1L))",
        "[1] 3 2\n",
    ),
    (
        "List.to_vector(List.drop_if(xs, function(x) x > 1L))",
        "[1] 1\n",
    ),
    (
        "List.to_vector(List.keep_oks(list(\"1\", \"x\", \"3\"), Str.to_integer))",
        "[1] 1 3\n",
    ),
    (
        "List.keep_errs(list(\"1\", \"x\"), Str.to_integer)",
        "[[1]]\n:InvalidNumStr\n\n",
    ),
    (
        "List.map_try(list(\"1\", \"x\"), Str.to_integer)",
        ":Err(:InvalidNumStr)\n",
    ),
    (
        "Result.map(List.map_try(list(\"1\", \"2\"), Str.to_integer), List.to_vector)",
        ":Ok(1:2)\n",
    ),
    // List: folds and searches.
    (
        "List.walk(words, \"\", function(s, w) Str.concat(s, w))",
        "[1] \"pearfigapple\"\n",
    ),
    (
        "List.walk_backwards(words, \"\", function(s, w) Str.concat(s, w))",
        "[1] \"applefigpear\"\n",
    ),
    (
        "List.walk_until(list(1L, 2L, 3L, 4L), 0L, function(s, x) if (s >= 3L) :Break(s) else :Continue(s + x))",
        "[1] 3\n",
    ),
    ("List.any(xs, function(x) x > 2L)", "[1] TRUE\n"),
    ("List.all(xs, function(x) x > 2L)", "[1] FALSE\n"),
    ("List.all(list(), function(x) FALSE)", "[1] TRUE\n"),
    ("List.contains(words, \"fig\")", "[1] TRUE\n"),
    ("List.contains(xs, 1)", "[1] FALSE\n"),
    ("List.count_if(xs, function(x) x < 3L)", "[1] 2\n"),
    ("List.find_first(xs, function(x) x < 3L)", ":Ok(1L)\n"),
    ("List.find_last(xs, function(x) x < 3L)", ":Ok(2L)\n"),
    ("List.find_first_index(xs, function(x) x < 3L)", ":Ok(2L)\n"),
    (
        "List.find_last_index(xs, function(x) x > 5L)",
        ":Err(:NotFound)\n",
    ),
    ("List.first(xs)", ":Ok(3L)\n"),
    ("List.last(list())", ":Err(:ListWasEmpty)\n"),
    // List: slices.
    ("List.to_vector(List.take_first(xs, 2L))", "[1] 3 1\n"),
    ("List.to_vector(List.take_last(xs, 5L))", "[1] 3 1 2\n"),
    ("List.to_vector(List.drop_first(xs, 1L))", "[1] 1 2\n"),
    ("List.to_vector(List.drop_last(xs, 1L))", "[1] 3 1\n"),
    ("List.to_vector(List.drop_at(xs, 1L))", "[1] 1 2\n"),
    ("List.to_vector(List.sublist(xs, 2L, 5L))", "[1] 1 2\n"),
    (
        "List.split_at(xs, 1L)",
        "(before = list(3L), others = list(1L, 2L))\n",
    ),
    (
        "List.chunks_of(xs, 2L)",
        "[[1]]\n[[1]][[1]]\n[1] 3\n\n[[1]][[2]]\n[1] 1\n\n\n[[2]]\n[[2]][[1]]\n[1] 2\n\n\n",
    ),
    (
        "List.to_vector(List.intersperse(xs, 0L))",
        "[1] 3 0 1 0 2\n",
    ),
    (
        "List.to_vector(List.sort_with(xs, Num.compare))",
        "[1] 1 2 3\n",
    ),
    (
        "List.to_vector(List.sort_with(words, function(a, b) Num.compare(Str.count_utf8_bytes(a), Str.count_utf8_bytes(b))))",
        "[1] \"fig\"   \"pear\"  \"apple\"\n",
    ),
    ("List.sum(xs)", "[1] 6\n"),
    ("List.sum(list())", "[1] 0\n"),
    ("List.product(list(2, 2.5))", "[1] 5\n"),
    ("List.min(xs)", ":Ok(1L)\n"),
    ("List.max(list())", ":Err(:ListWasEmpty)\n"),
    ("List.starts_with(xs, list(3L, 1L))", "[1] TRUE\n"),
    ("List.ends_with(xs, list(1L))", "[1] FALSE\n"),
    // Str.
    ("Str.is_empty(\"\")", "[1] TRUE\n"),
    ("Str.concat(\"ab\", \"cd\")", "[1] \"abcd\"\n"),
    (
        "Str.join_with(list(\"a\", \"b\", \"c\"), \", \")",
        "[1] \"a, b, c\"\n",
    ),
    (
        "List.to_vector(Str.split_on(\"a,b,,c\", \",\"))",
        "[1] \"a\" \"b\" \"\"  \"c\"\n",
    ),
    ("Str.repeat(\"ab\", 3L)", "[1] \"ababab\"\n"),
    ("Str.starts_with(\"report.csv\", \"report\")", "[1] TRUE\n"),
    ("Str.ends_with(\"report.csv\", \".R\")", "[1] FALSE\n"),
    ("Str.contains(\"a.b\", \".\")", "[1] TRUE\n"),
    ("Str.trim(\"  hi  \")", "[1] \"hi\"\n"),
    ("Str.trim_start(\"  hi  \")", "[1] \"hi  \"\n"),
    ("Str.trim_end(\"  hi  \")", "[1] \"  hi\"\n"),
    ("Str.drop_prefix(\"v1.2\", \"v\")", "[1] \"1.2\"\n"),
    ("Str.drop_suffix(\"v1.2\", \"x\")", "[1] \"v1.2\"\n"),
    (
        "Str.with_prefix(\"world\", \"hello \")",
        "[1] \"hello world\"\n",
    ),
    (
        "Str.replace_each(\"a.b.c\", \".\", \"-\")",
        "[1] \"a-b-c\"\n",
    ),
    (
        "Str.replace_first(\"a.b.c\", \".\", \"-\")",
        "[1] \"a-b.c\"\n",
    ),
    (
        "Str.replace_last(\"a.b.c\", \".\", \"-\")",
        "[1] \"a.b-c\"\n",
    ),
    (
        "Str.split_first(\"k=v=w\", \"=\")",
        ":Ok((after = \"v=w\", before = \"k\"))\n",
    ),
    (
        "Str.split_last(\"k=v=w\", \"=\")",
        ":Ok((after = \"w\", before = \"k=v\"))\n",
    ),
    ("Str.split_first(\"kv\", \"=\")", ":Err(:NotFound)\n"),
    ("Str.count_utf8_bytes(\"h\u{e9}\")", "[1] 3\n"),
    (
        "List.to_vector(Str.to_utf8(\"h\u{e9}\"))",
        "[1] 104 195 169\n",
    ),
    ("Str.from_utf8(list(104L, 105L))", ":Ok(\"hi\")\n"),
    ("Str.from_utf8(list(255L))", ":Err(:BadUtf8)\n"),
    ("Str.to_integer(\"42\")", ":Ok(42L)\n"),
    ("Str.to_integer(\"4.2\")", ":Err(:InvalidNumStr)\n"),
    ("Str.to_integer(\"99999999999\")", ":Err(:InvalidNumStr)\n"),
    ("Str.to_double(\"4.5e1\")", ":Ok(45)\n"),
    ("Str.to_double(\"NA\")", ":Err(:InvalidNumStr)\n"),
    ("Str.with_ascii_lowercased(\"MiXeD\")", "[1] \"mixed\"\n"),
    ("Str.with_ascii_uppercased(\"MiXeD\")", "[1] \"MIXED\"\n"),
    // Num.
    ("Num.to_str(1.5)", "[1] \"1.5\"\n"),
    ("Num.to_str(7L)", "[1] \"7\"\n"),
    ("Num.compare(2L, 1L)", ":GT\n"),
    ("Num.compare(1, 1L)", ":EQ\n"),
    ("Num.min(2L, 1L)", "[1] 1\n"),
    ("Num.max(2, 1)", "[1] 2\n"),
    ("Num.is_zero(-0)", "[1] TRUE\n"),
    ("Num.is_positive(0L)", "[1] FALSE\n"),
    ("Num.is_negative(-1)", "[1] TRUE\n"),
    ("Num.is_even(4L)", "[1] TRUE\n"),
    ("Num.is_odd(-3L)", "[1] TRUE\n"),
    ("Num.add_checked(2147483647L, 1L)", ":Err(:Overflow)\n"),
    ("Num.add_checked(1L, 1L)", ":Ok(2L)\n"),
    ("Num.sub_checked(-2147483647L, 1L)", ":Err(:Overflow)\n"),
    ("Num.mul_checked(1e308, 10)", ":Err(:Overflow)\n"),
    ("Num.div_checked(7L, 2L)", ":Ok(3.5)\n"),
    ("Num.div_checked(1, 0)", ":Err(:DivByZero)\n"),
    ("Num.div_trunc_checked(-7L, 2L)", ":Ok(-3L)\n"),
    ("Num.rem_checked(-7L, 2L)", ":Ok(-1L)\n"),
    ("Num.rem_checked(7L, 0L)", ":Err(:DivByZero)\n"),
    ("Num.sqrt_checked(9)", ":Ok(3)\n"),
    ("Num.sqrt_checked(-1)", ":Err(:SqrtOfNegative)\n"),
    ("Num.log_checked(1)", ":Ok(0)\n"),
    ("Num.log_checked(0)", ":Err(:LogNeedsPositive)\n"),
    // Dict.
    ("Dict.empty()", "Dict.empty()\n"),
    (
        "Dict.single(1L, \"one\")",
        "Dict.from_list(list((1L, \"one\")))\n",
    ),
    ("d", "Dict.from_list(list((\"a\", 1), (\"b\", 2)))\n"),
    (
        "Dict.to_list(d)",
        "[[1]]\n(\"a\", 1)\n\n[[2]]\n(\"b\", 2)\n\n",
    ),
    ("Dict.len(d)", "[1] 2\n"),
    ("length(d)", "[1] 2\n"),
    ("typeof(d)", "[1] \"dict\"\n"),
    ("class(d)", "[1] \"ry_dict\"\n"),
    ("Dict.is_empty(d)", "[1] FALSE\n"),
    ("Dict.get(d, \"b\")", ":Ok(2)\n"),
    ("Dict.get(d, \"z\")", ":Err(:KeyNotFound)\n"),
    ("Dict.get(d, 1L)", ":Err(:KeyNotFound)\n"),
    ("Dict.contains(d, \"a\")", "[1] TRUE\n"),
    (
        "Dict.insert(d, \"a\", 10)",
        "Dict.from_list(list((\"a\", 10), (\"b\", 2)))\n",
    ),
    (
        "Dict.remove(d, \"a\")",
        "Dict.from_list(list((\"b\", 2)))\n",
    ),
    (
        "Dict.remove(d, \"z\")",
        "Dict.from_list(list((\"a\", 1), (\"b\", 2)))\n",
    ),
    (
        "Dict.update(d, \"c\", function(r) :Ok(Result.with_default(r, 0) + 1))",
        "Dict.from_list(list((\"a\", 1), (\"b\", 2), (\"c\", 1)))\n",
    ),
    (
        "Dict.update(d, \"a\", function(r) :Err(:Missing))",
        "Dict.from_list(list((\"b\", 2)))\n",
    ),
    ("List.to_vector(Dict.keys(d))", "[1] \"a\" \"b\"\n"),
    ("List.to_vector(Dict.values(d))", "[1] 1 2\n"),
    ("Dict.walk(d, 0, function(s, k, v) s + v)", "[1] 3\n"),
    (
        "Dict.map(d, function(k, v) Str.concat(k, Num.to_str(v)))",
        "Dict.from_list(list((\"a\", \"a1\"), (\"b\", \"b2\")))\n",
    ),
    (
        "Dict.keep_if(d, function(k, v) v > 1)",
        "Dict.from_list(list((\"b\", 2)))\n",
    ),
    (
        "Dict.drop_if(d, function(k, v) v > 1)",
        "Dict.from_list(list((\"a\", 1)))\n",
    ),
    (
        "Dict.insert_all(d, Dict.single(\"a\", 5))",
        "Dict.from_list(list((\"a\", 5), (\"b\", 2)))\n",
    ),
    (
        "Dict.keep_shared(d, Dict.single(\"b\", 2))",
        "Dict.from_list(list((\"b\", 2)))\n",
    ),
    (
        "Dict.keep_shared(d, Dict.single(\"b\", 0))",
        "Dict.empty()\n",
    ),
    (
        "Dict.remove_all(d, Dict.single(\"b\", 0))",
        "Dict.from_list(list((\"a\", 1)))\n",
    ),
    (
        "identical(d, Dict.from_list(list((\"b\", 2), (\"a\", 1))))",
        "[1] TRUE\n",
    ),
    ("identical(d, Dict.insert(d, \"a\", 1L))", "[1] FALSE\n"),
    (
        "Dict.from_list(list((1L, \"a\"), (2, \"b\")))",
        "Dict.from_list(list((1, \"a\"), (2, \"b\")))\n",
    ),
    ("Dict.get(Dict.single(1L, \"a\"), 1)", ":Ok(\"a\")\n"),
    ("Dict.get(Dict.single(-0, \"zero\"), 0)", ":Ok(\"zero\")\n"),
    (
        "Dict.from_list(list((\"a\", 1), (\"a\", 2)))",
        "Dict.from_list(list((\"a\", 2)))\n",
    ),
    ("list(d = Dict.empty())", "$d\nDict.empty()\n\n"),
    // Set.
    ("Set.empty()", "Set.empty()\n"),
    ("Set.single(\"a\")", "Set.from_list(list(\"a\"))\n"),
    (
        "Set.from_list(list(2L, 1L, 2L))",
        "Set.from_list(list(2L, 1L))\n",
    ),
    ("List.to_vector(Set.to_list(s))", "[1] 1 2 3\n"),
    ("Set.len(s)", "[1] 3\n"),
    ("Set.is_empty(Set.empty())", "[1] TRUE\n"),
    ("Set.contains(s, 2L)", "[1] TRUE\n"),
    ("Set.insert(s, 4L)", "Set.from_list(list(1L, 2L, 3L, 4L))\n"),
    ("Set.remove(s, 1L)", "Set.from_list(list(2L, 3L))\n"),
    (
        "Set.union(s, Set.from_list(list(5L, 1L)))",
        "Set.from_list(list(1L, 2L, 3L, 5L))\n",
    ),
    (
        "Set.intersection(s, Set.from_list(list(3L, 2L, 9L)))",
        "Set.from_list(list(2L, 3L))\n",
    ),
    (
        "Set.difference(s, Set.from_list(list(3L)))",
        "Set.from_list(list(1L, 2L))\n",
    ),
    ("Set.walk(s, 0L, function(acc, x) acc + x)", "[1] 6\n"),
    (
        "Set.map(s, function(x) x %% 2L)",
        "Set.from_list(list(1L, 0L))\n",
    ),
    (
        "Set.keep_if(s, function(x) x > 1L)",
        "Set.from_list(list(2L, 3L))\n",
    ),
    (
        "Set.drop_if(s, function(x) x > 1L)",
        "Set.from_list(list(1L))\n",
    ),
    (
        "identical(s, Set.from_list(list(3L, 2L, 1L)))",
        "[1] TRUE\n",
    ),
    ("typeof(s)", "[1] \"set\"\n"),
    // A program's own definition comes before the library's.
    (
        "(function() { List.len <- function(list) -1L; List.len(xs) })()",
        "[1] -1\n",
    ),
    // An emptied collection is `Dict.empty()` again, key type included.
    (
        "Dict.insert(Dict.remove(Dict.single(1L, \"a\"), 1L), \"x\", 2)",
        "Dict.from_list(list((\"x\", 2)))\n",
    ),
    (
        "Set.insert(Set.remove(Set.single(1L), 1L), \"x\")",
        "Set.from_list(list(\"x\"))\n",
    ),
    (
        "identical(Set.from_list(list(1L)), Set.from_list(list(1)))",
        "[1] FALSE\n",
    ),
    (
        "all.equal.ry_dict(Dict.single(\"a\", 1), Dict.single(\"a\", 2))",
        "[1] \"Key \\\"a\\\": target and current differ\"\n",
    ),
    (
        "all.equal.ry_set(Set.single(1L), Set.single(2L))",
        "[1] \"target and current hold different keys\"\n",
    ),
];

/// Each case's code and the error message it raises.
const ERRORS: &[(&str, &str)] = &[
    (
        "List.len(c(1, 2))",
        "`list` must be a list, not a double vector of length 2",
    ),
    ("List.len()", "`List.len()` needs the argument `list`"),
    ("List.get(xs, 1L, 2L)", "unused argument (2L)"),
    (
        "List.get(xs, 1.5)",
        "`index` must be a whole number, not a double value",
    ),
    (
        "List.get(xs, NA_integer_)",
        "`index` must be a whole number, not `NA`",
    ),
    (
        "List.map(xs, 1)",
        "`f` must be a function, not a double value",
    ),
    (
        "List.keep_if(xs, function(x) NA)",
        "`predicate` must return `TRUE` or `FALSE`, not `NA`",
    ),
    (
        "List.walk_until(xs, 0L, function(s, x) s)",
        "`f` must return `:Continue(state)` or `:Break(state)`, not an integer value",
    ),
    (
        "List.sort_with(xs, function(a, b) 0L)",
        "`compare` must return `:LT`, `:EQ` or `:GT`, not an integer value",
    ),
    ("List.sum(list(1, \"a\"))", "must be a single number"),
    (
        "List.to_vector(list(1L, \"a\"))",
        "`list` must hold elements of one atomic type, and it mixes integer and character",
    ),
    (
        "Result.map(5, function(x) x)",
        "`result` must be a result, `:Ok(value)` or `:Err(error)`",
    ),
    (
        "Str.trim(1L)",
        "`string` must be a single string, not an integer value",
    ),
    (
        "Str.trim(NA_character_)",
        "`string` must be a single string, not `NA`",
    ),
    ("Str.repeat(\"a\", -1L)", "`count` must be"),
    (
        "Num.to_str(\"1\")",
        "`number` must be a single number, not a character value",
    ),
    (
        "Num.is_zero(NaN)",
        "`number` must be a single number, not `NaN`",
    ),
    (
        "Dict.insert(d, 1L, 3)",
        "the dict's keys are character, and this one is integer",
    ),
    (
        "Dict.insert(d, NA_character_, 1)",
        "`key` must be a single logical, number or string, not `NA`",
    ),
    (
        "Dict.insert(d, 1i, 1)",
        "`key` must be a single logical, number or string, not a complex value",
    ),
    ("Dict.get(s, 1L)", "`dict` must be a dict, not a ry set"),
    (
        "Dict.from_list(list(1))",
        "`pairs` must be a list of `(key, value)` tuples, not a double value",
    ),
    (
        "Set.insert(s, NaN)",
        "`element` must be a single logical, number or string, not `NaN`",
    ),
    (
        "d$a",
        "a dict has no fields; read a value with `Dict.get(dict, key)`",
    ),
    (
        "d[[1]]",
        "a dict cannot be indexed; read a value with `Dict.get(dict, key)`",
    ),
    (
        "s[[1]]",
        "a set cannot be indexed; test an element with `Set.contains(set, element)`",
    ),
    (
        "sum(d)",
        "`d` is a ry dict, not an R object; convert it with `as.list()`",
    ),
    (
        "c(s, 1)",
        "`s` is a ry set, not an R object; convert it with `as.list()`",
    ),
    (
        "List.sum(list(2147483647L, 1L))",
        "the integer result overflows; convert the elements to doubles first",
    ),
];

thread_local! {
    static OUTPUT: RefCell<String> = const { RefCell::new(String::new()) };
}

unsafe fn capture(text: &[u8], _is_error: bool) {
    OUTPUT.with(|output| output.borrow_mut().push_str(&String::from_utf8_lossy(text)));
}

fn take_output() -> String {
    OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()))
}

/// The declarations in `types/ry.Rtypes`: each function's name and its parameter names.
fn declarations() -> Vec<(String, Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../types/ry.Rtypes");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|failure| panic!("cannot read {}: {failure}", path.display()));
    text.lines()
        .filter_map(|line| {
            let (name, signature) = line.split_once(" : ")?;
            // The list conversions are base builtins, declared here only for the checker.
            if name.starts_with("as.") {
                return None;
            }
            let parameters = &signature[signature.find("fn(")? + 3..];
            let mut depth = 0usize;
            let mut names = Vec::new();
            let mut current = String::new();
            for character in parameters.chars() {
                match character {
                    '(' | '[' | '{' | '<' => depth += 1,
                    '>' if current.ends_with('-') => {}
                    ')' | ']' | '}' | '>' if depth == 0 => break,
                    ')' | ']' | '}' | '>' => depth -= 1,
                    ',' if depth == 0 => {
                        names.push(std::mem::take(&mut current));
                        continue;
                    }
                    _ => {}
                }
                current.push(character);
            }
            names.push(current);
            let names = names
                .iter()
                .filter_map(|parameter| parameter.split_once(':'))
                .map(|(name, _)| name.trim().to_owned())
                .collect();
            Some((name.to_owned(), names))
        })
        .collect()
}

#[test]
fn declarations_and_runtime_bind_the_same_names() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../types/ry.exports");
    let manifest = fs::read_to_string(&manifest)
        .unwrap_or_else(|failure| panic!("cannot read {}: {failure}", manifest.display()));
    // The list conversions are base builtins, listed in the manifest only for the checker.
    let exported: BTreeSet<String> = manifest
        .lines()
        .filter(|line| {
            !line.starts_with('#') && !matches!(*line, "as.record" | "as.tuple" | "as.tag")
        })
        .map(str::to_owned)
        .collect();
    let declared: BTreeSet<String> = declarations().into_iter().map(|(name, _)| name).collect();
    let mut bound: BTreeSet<String> = standard_library::names().map(str::to_owned).collect();
    bound.extend(frame::package_functions().map(str::to_owned));
    let unexported: Vec<_> = declared.difference(&exported).collect();
    let unlisted: Vec<_> = bound.difference(&exported).collect();
    let unbound: Vec<_> = exported.difference(&bound).collect();
    assert!(
        unexported.is_empty() && unlisted.is_empty() && unbound.is_empty(),
        "declared in types/ry.Rtypes but not in types/ry.exports: {unexported:?}; \
         bound but not in types/ry.exports: {unlisted:?}; in types/ry.exports but not bound: {unbound:?}"
    );
}

/// The design promises that no library name shadows a name any shipped package exports, so a
/// program that defines or attaches one never meets the library by accident.
#[test]
fn no_shipped_package_exports_a_library_name() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../types");
    let library: BTreeSet<String> = standard_library::names().map(str::to_owned).collect();
    let mut collisions = Vec::new();
    let entries = fs::read_dir(&directory)
        .unwrap_or_else(|failure| panic!("cannot list {}: {failure}", directory.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|failure| panic!("cannot read an entry: {failure}"))
            .path();
        // `ry.exports` is the library's own list.
        if path
            .extension()
            .is_none_or(|extension| extension != "exports")
            || path.file_stem().is_some_and(|stem| stem == "ry")
        {
            continue;
        }
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|failure| panic!("cannot read {}: {failure}", path.display()));
        for name in text.lines().filter(|line| !line.starts_with('#')) {
            if library.contains(name) {
                collisions.push(format!("{} exports `{name}`", path.display()));
            }
        }
    }
    assert!(collisions.is_empty(), "{}", collisions.join("\n"));
}

#[test]
fn standard_library_at_runtime() {
    // SAFETY: the heap is leaked, so it outlives the interpreter, and this is the only test in
    // its process that uses the interpreter.
    unsafe {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        let mut hooks = interp::default_hooks();
        hooks.write_console = capture;
        interp::install(heap, hooks);
        interp::interp().show_error_messages = false;
        evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
        let mut failures = Vec::new();
        for (code, expected) in PRINTS {
            let printed = printed(code);
            if printed != *expected {
                failures.push(format!(
                    "`{code}`\n  printed  {printed:?}\n  expected {expected:?}"
                ));
            }
        }
        for (code, expected) in ERRORS {
            let reported = printed(code);
            if !reported.contains(expected) {
                failures.push(format!(
                    "`{code}`\n  reported {reported:?}\n  expected {expected:?}"
                ));
            }
        }
        failures.extend(named_argument_failures());
        failures.extend(collection_model_failures());
        assert!(
            failures.is_empty(),
            "{} cases failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Calls every function with each declared parameter named. A name the runtime does not know is
/// an "unused argument" error, so this keeps the runtime's formals equal to the declarations,
/// which hover and completion show.
unsafe fn named_argument_failures() -> Vec<String> {
    let mut failures = Vec::new();
    for (name, parameters) in declarations() {
        let arguments: Vec<String> = parameters
            .iter()
            .map(|parameter| format!("{parameter} = NULL"))
            .collect();
        let code = format!("`{name}`({})", arguments.join(", "));
        // SAFETY: the interpreter is installed.
        let reported = unsafe { printed(&code) };
        if reported.contains("unused argument") || reported.contains("needs the argument") {
            failures.push(format!("`{code}`: {}", reported.trim_end()));
        }
    }
    failures
}

/// Random insertions and removals on a dictionary and a set, checked step by step against a
/// model that keeps entries in insertion order. Enough distinct keys pass through each collection
/// that its hash index is rebuilt several times.
unsafe fn collection_model_failures() -> Vec<String> {
    let mut failures = Vec::new();
    let mut random = SplitMix64(0x5EED_D1C7);
    for round in 0..fuzz_iterations(24) {
        let mut model: Vec<(i64, i64)> = Vec::new();
        let mut code = String::from("{ m <- Dict.empty(); t <- Set.empty()\n");
        let mut probes = Vec::new();
        for step in 0..120 {
            let key = random.below(48) as i64;
            if random.chance(2, 3) {
                let value = random.below(1000) as i64;
                code.push_str(&format!(
                    "m <- Dict.insert(m, {key}L, {value}); t <- Set.insert(t, \"k{key}\")\n"
                ));
                match model.iter_mut().find(|(existing, _)| *existing == key) {
                    Some(entry) => entry.1 = value,
                    None => model.push((key, value)),
                }
            } else {
                code.push_str(&format!(
                    "m <- Dict.remove(m, {key}L); t <- Set.remove(t, \"k{key}\")\n"
                ));
                model.retain(|(existing, _)| *existing != key);
            }
            if step % 20 == 19 {
                let probe = random.below(48) as i64;
                let expected = match model.iter().find(|(existing, _)| *existing == probe) {
                    Some((_, value)) => format!(":Ok({value})"),
                    None => ":Err(:KeyNotFound)".to_owned(),
                };
                let index = probes.len() + 1;
                code.push_str(&format!("p{index} <- Dict.get(m, {probe}L)\n"));
                probes.push((format!("p{index}"), expected));
            }
        }
        let keys: Vec<String> = model.iter().map(|(key, _)| format!("{key}L")).collect();
        let values: Vec<String> = model.iter().map(|(_, value)| value.to_string()).collect();
        let elements: Vec<String> = model.iter().map(|(key, _)| format!("\"k{key}\"")).collect();
        code.push_str(&format!(
            "c(identical(Dict.keys(m), list({})), identical(Dict.values(m), list({})), \
             identical(Set.to_list(t), list({})), Dict.len(m) == {count}L, Set.len(t) == {count}L) }}",
            keys.join(", "),
            values.join(", "),
            elements.join(", "),
            count = model.len(),
        ));
        // SAFETY: the interpreter is installed.
        let checks = unsafe { printed(&code) };
        if checks != "[1] TRUE TRUE TRUE TRUE TRUE\n" {
            failures.push(format!(
                "round {round}: the collections disagree with the model: {checks:?}"
            ));
        }
        for (variable, expected) in probes {
            // SAFETY: the interpreter is installed.
            let printed = unsafe { printed(&variable) };
            if printed.trim_end() != expected {
                failures.push(format!(
                    "round {round}: `{variable}` printed {printed:?}, expected {expected}"
                ));
            }
        }
    }
    failures
}

/// What evaluating `code` and printing a visible result shows, or the error message.
unsafe fn printed(code: &str) -> String {
    take_output();
    // SAFETY: the interpreter is installed.
    unsafe {
        match evaluate(code) {
            Ok(value) => {
                if interp::interp().visible {
                    if let Err(failure) = print::print_value_top(value, values::global_env()) {
                        return format!("printing failed: {}", message(failure));
                    }
                }
                take_output()
            }
            Err(failure) => format!("{}\n", failure.trim_end()),
        }
    }
}

/// Evaluates `text` in the global environment and returns the last value, or the error message.
unsafe fn evaluate(text: &str) -> Result<Sexp, String> {
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let expressions = lower::parse_text(text, "<test>").map_err(|failure| {
            let reported = message(failure);
            if reported.is_empty() {
                interp::interp().error_message.clone()
            } else {
                reported
            }
        })?;
        let mut value = values::nil();
        for index in 0..values::length(expressions) {
            interp::interp().error_message.clear();
            value = eval::eval(expressions.element(index), values::global_env()).map_err(
                |failure| {
                    let reported = message(failure);
                    format!("{reported}{}", interp::interp().error_message)
                },
            )?;
        }
        Ok(value)
    }
}

fn message(unwind: Unwind) -> String {
    match unwind {
        Unwind::Error(failure) => failure.message,
        _ => String::new(),
    }
}
