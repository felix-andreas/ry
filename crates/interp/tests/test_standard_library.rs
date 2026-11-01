//! ry's standard library at runtime: the functions' results and errors, the agreement between the
//! runtime's table and the declarations in `types/ry.Rtypes`, and a property test that holds the
//! dictionary and set against a reference model. The interpreter is global to the process, so the
//! interpreter cases run as one test.
//!
//! The interpreter runs here without base R, so the setup defines the few functions base R would.

use {
    heap::{Heap, object::Sexp},
    interp::{dataframe, eval, frame, lower, print, standard_library, unwind::Unwind, values},
    std::{cell::RefCell, collections::BTreeSet, fs, path::Path},
    syntax::testing::{SplitMix64, fuzz_iterations},
};

const SETUP: &str = r#"
print <- function(x, ...) UseMethod("print")
print.default <- function(x, ...) .Internal(print.default(x, NULL, NULL))
as.list <- function(x, ...) UseMethod("as.list")
identical <- function(x, y) .Internal(identical(x, y, TRUE, TRUE, TRUE, TRUE, FALSE, TRUE, FALSE))
deparse <- function(expr) .Internal(deparse(expr, 500L, TRUE, 1089L, -1L))
typeof <- function(x) .Internal(typeof(x))
environment <- function(fun = NULL) .Internal(environment(fun))
parent.frame <- function(n = 1) .Internal(parent.frame(n))
eval <- function(expr, envir) .Internal(eval(expr, envir, NULL))
eval.parent <- function(expr, n = 1) { p <- parent.frame(n + 1); eval(expr, p) }
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
    (
        "identical(List.sum(as.Array((0:999999) / 999999 * 2 + 1)), sum((0:999999) / 999999 * 2 + 1))",
        "[1] TRUE\n",
    ),
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
    ("Dict.get(Dict.empty(), 1L)", ":Err(:KeyNotFound)\n"),
    (
        "Dict.get(Dict.from_list(list((1, \"one\"))), 1L)",
        ":Ok(\"one\")\n",
    ),
    ("Dict.contains(d, \"a\")", "[1] TRUE\n"),
    // Updates in place keep value semantics. Each case also runs in a body with a loop, which
    // compiles to bytecode on its first call, so the register checks are exercised too.
    (
        "{ st <- list(seen = Dict.empty()); for (i in 1:5) st$seen <- Dict.insert(st$seen, i, i); Dict.len(st$seen) }",
        "[1] 5\n",
    ),
    (
        "{ a <- list(seen = Dict.empty()); b <- a; a$seen <- Dict.insert(a$seen, 1L, 1); c(Dict.len(a$seen), Dict.len(b$seen)) }",
        "[1] 1 0\n",
    ),
    (
        "{ a <- list(list(Set.empty())); b <- a[[1]]; a[[1]][[1]] <- Set.insert(a[[1]][[1]], 1L); c(Set.len(a[[1]][[1]]), Set.len(b[[1]])) }",
        "[1] 1 0\n",
    ),
    (
        "{ add <- function(m, k) { m <- Dict.insert(m, k, k); m }; m <- Dict.empty(); for (i in 1:5) m <- add(m, i); Dict.len(m) }",
        "[1] 5\n",
    ),
    (
        "{ add <- function(m, k) { m <- Dict.insert(m, k, k); m }; m <- Dict.empty(); keep <- m; m <- add(m, 1L); c(Dict.len(m), Dict.len(keep)) }",
        "[1] 1 0\n",
    ),
    (
        "{ add <- function(m, k) { m <- Dict.insert(m, k, k); m }; m <- Dict.empty(); n <- add(m, 1L); c(Dict.len(m), Dict.len(n)) }",
        "[1] 0 1\n",
    ),
    (
        "{ add <- function(m, k) { for (i in 1) NULL; m <- Dict.insert(m, k, k); m }; run <- function() { for (i in 1) NULL; m <- Dict.empty(); keep <- m; m <- add(m, 1L); m <- add(m, 2L); c(Dict.len(m), Dict.len(keep)) }; run() }",
        "[1] 2 0\n",
    ),
    (
        "{ kept <- NULL; add <- function(m) { m <- Dict.insert(m, 1L, 1); kept <<- environment(); m }; m <- Dict.empty(); m <- add(m); m <- Dict.insert(m, 2L, 2); c(Dict.len(m), Dict.len(kept$m)) }",
        "[1] 2 1\n",
    ),
    (
        "{ main <- function() { d <- Dict.single(\"z\", 0); h <- function(x, k, y) { x <- Dict.insert(x, k, 1); Dict.len(y) }; n <- 0L; d <- { n <- h(d, \"a\", d); d }; inner <- function(x) { x <- Dict.insert(x, \"b\", 1); Dict.len(d) }; c(n, inner(d)) }; main() }",
        "[1] 1 1\n",
    ),
    (
        "{ main <- function() { st <- list(seen = Dict.single(\"z\", 0)); st$seen <- Dict.insert(st$seen, \"k\", st); old <- Result.with_default(Dict.get(st$seen, \"k\"), NULL); Dict.len(old$seen) }; main() }",
        "[1] 1\n",
    ),
    (
        "{ kept <- NULL; add <- function(m, k) { m <- Dict.insert(m, k, k); g <- function() m; kept <<- g; m }; f <- function() { for (i in 1) NULL; m <- Dict.empty(); for (i in 1:3) m <- add(m, i); m <- Dict.insert(m, 9L, 9L); c(Dict.len(m), Dict.len(kept())) }; f() }",
        "[1] 4 3\n",
    ),
    (
        "{ add <- function(m, k) Dict.insert(m, k, k); f <- function() { for (i in 1) NULL; m <- Dict.empty(); for (i in 1:5) m <- add(m, i); keep <- m; m <- add(m, 9L); c(Dict.len(m), Dict.len(keep)) }; f() }",
        "[1] 6 5\n",
    ),
    (
        "{ saved <- NULL; grab <- function(k) { saved <<- parent.frame(); k }; add <- function(m, k) Dict.insert(m, grab(k), k); f <- function() { for (i in 1) NULL; m <- Dict.empty(); for (i in 1:4) m <- add(m, i); c(Dict.len(m), Dict.len(saved$m)) }; f() }",
        "[1] 4 3\n",
    ),
    (
        "{ h <- function(p, k) { g <- function() Dict.len(p); Dict.insert(p, k, g) }; f <- function() { for (i in 1) NULL; out <- NULL; for (i in 1:4) { d <- Dict.insert(Dict.empty(), \"a\", 0L); d <- h(d, \"z\"); out <- c(out, Result.with_default(Dict.get(d, \"z\"), NULL)()) }; out }; f() }",
        "[1] 1 1 1 1\n",
    ),
    (
        "{ f <- function() { me <- environment(); d <- Dict.insert(Dict.empty(), 0L, 1); h <- function(p, k, env) Dict.insert(p, k, length(env[[\"d\"]])); d <- h(d, 1L, me); Dict.get(d, 1L) }; f() }",
        ":Ok(1L)\n",
    ),
    (
        "{ moved_d <- Dict.single(0L, 1); h <- function(p, k) { n <- is.null(moved_d); moved_d <- 0; Dict.insert(p, k, n) }; for (i in 1:3) moved_d <- h(moved_d, i); Result.with_default(Dict.get(moved_d, 3L), NA) }",
        "[1] FALSE\n",
    ),
    (
        "{ add <- function(xs, x) List.append(xs, x); f <- function() { for (i in 1) NULL; xs <- list(); for (i in 1:5) xs <- add(xs, i); keep <- xs; xs <- add(xs, 9L); c(length(xs), length(keep)) }; f() }",
        "[1] 6 5\n",
    ),
    (
        "{ put <- function(xs, v) { xs <- List.set(xs, 1L, v); xs }; f <- function() { for (i in 1) NULL; xs <- List.append(List.append(list(), 1), 2); for (i in 1:3) xs <- put(xs, i); keep <- xs; xs <- put(xs, 7); c(xs[[1]], keep[[1]], length(xs)) }; f() }",
        "[1] 7 3 2\n",
    ),
    (
        "{ h <- function(xs, k) { g <- function() length(xs); List.append(xs, g) }; f <- function() { for (i in 1) NULL; xs <- list(); for (i in 1:3) xs <- h(xs, i); c(xs[[1]](), xs[[2]](), xs[[3]]()) }; f() }",
        "[1] 0 1 2\n",
    ),
    (
        "{ h <- function(d, k) { inc <- function(x) x + 1L; d <- Dict.insert(d, k, inc(k)); d }; f <- function() { for (i in 1) NULL; d <- Dict.empty(); for (i in 1:5) d <- h(d, i); keep <- d; d <- h(d, 9L); c(Dict.len(d), Dict.len(keep), Result.with_default(Dict.get(d, 9L), 0L)) }; f() }",
        "[1]  6  5 10\n",
    ),
    (
        "{ f <- function() { m <- Dict.empty(); h <- function(p, k) { g <- function() Dict.len(m); Dict.insert(p, k, g()) }; for (i in 1:3) m <- h(m, i); Dict.get(m, 3L) }; f() }",
        ":Ok(2L)\n",
    ),
    (
        "{ kept <- NULL; h <- function(d, k) { g <- function() Dict.len(d); d <- Dict.insert(d, k, 1); list(d, g) }; f <- function() { for (i in 1) NULL; d <- Dict.empty(); for (i in 1:3) { r <- h(d, i); d <- r[[1]]; kept <- r[[2]] }; c(Dict.len(d), kept()) }; f() }",
        "[1] 3 3\n",
    ),
    (
        "{ h <- function(d, k) { each <- function(x) Dict.insert(x, k, 1); d <- each(d); d }; f <- function() { for (i in 1) NULL; d <- Dict.empty(); for (i in 1:4) d <- h(d, i); keep <- d; d <- h(d, 8L); c(Dict.len(d), Dict.len(keep)) }; f() }",
        "[1] 5 4\n",
    ),
    (
        "{ add <- function(s, k) Set.insert(s, k); f <- function() { for (i in 1) NULL; s <- Set.empty(); for (i in 1:4) s <- add(s, i); other <- add(s, 7L); c(Set.len(s), Set.len(other)) }; f() }",
        "[1] 4 5\n",
    ),
    (
        "{ add <- function(s, k) { s <- Set.insert(s, k); s }; f <- function() { for (i in 1) NULL; s <- Set.empty(); for (i in 1:6) { if (i == 4L) old <- s; s <- add(s, i %% 5L) }; c(Set.len(old), Set.len(s)) }; f() }",
        "[1] 3 5\n",
    ),
    (
        "{ f <- function(m) { r <- 0L; for (i in 1:50) r <- r + Dict.len(m) + Dict.contains(m, i) + Set.len(Set.single(i)); r }; f(Dict.from_list(list((1L, 1), (2L, 2)))) }",
        "[1] 152\n",
    ),
    (
        "{ f <- function(m) { r <- NULL; for (i in 1:50) r <- Dict.get(m, if (i > 40L) 2 else 1L); r }; f(Dict.from_list(list((1L, \"a\"), (2L, \"b\")))) }",
        ":Ok(\"b\")\n",
    ),
    (
        "{ h <- function() eval.parent(quote(x[[1]] <- 99)); f <- function() { for (i in 1) NULL; x <- list(1); r <- list(x, h()); c(r[[1]][[1]], x[[1]]) }; f() }",
        "[1]  1 99\n",
    ),
    (
        "{ h <- function() eval.parent(quote(s <- Set.insert(s, 9L))); f <- function() { for (i in 1) NULL; s <- Set.empty(); r <- list(s, h()); c(Set.len(r[[1]]), Set.len(s)) }; f() }",
        "[1] 0 1\n",
    ),
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
    // An update reuses its first argument only when nothing else can observe it.
    (
        "{ a <- Dict.from_list(list((\"x\", 1))); b <- a; a <- Dict.insert(a, \"y\", 2); c(Dict.len(a), Dict.len(b)) }",
        "[1] 2 1\n",
    ),
    (
        "{ p <- list(1L); q <- p; for (i in 1:3) p <- List.append(p, i); c(List.len(p), List.len(q)) }",
        "[1] 4 1\n",
    ),
    (
        "{ p <- list(1L); holder <- list(p); p <- List.append(p, 2L); c(List.len(p), List.len(holder[[1]])) }",
        "[1] 2 1\n",
    ),
    (
        "{ p <- list(1L, 2L); q <- p; p <- List.set(p, 1L, 9L); c(p[[1]], q[[1]]) }",
        "[1] 9 1\n",
    ),
    (
        "{ p <- list(1L); for (i in 1:3) p <- List.concat(p, list(i, i)); List.to_vector(p) }",
        "[1] 1 1 1 2 2 3 3\n",
    ),
    (
        "{ grow <- function(n) { s <- Set.empty(); keep <- NULL; for (i in 1:n) { s <- Set.insert(s, i); if (i == 3L) keep <- s }; c(Set.len(s), Set.len(keep)) }; grow(10L); grow(10L) }",
        "[1] 10  3\n",
    ),
    (
        "{ g <- function(p) { p <- List.append(p, 0L); List.len(p) }; z <- list(1L); c(g(z), g(z), List.len(z)) }",
        "[1] 2 2 1\n",
    ),
    (
        "{ u <- Set.single(1L); v <- u; u <- Set.union(u, Set.single(2L)); c(Set.len(u), Set.len(v)) }",
        "[1] 2 1\n",
    ),
    (
        "{ a <- Dict.single(\"k\", 1); a <- Dict.insert(a, \"self\", a); c(Dict.len(a), Dict.len(Result.with_default(Dict.get(a, \"self\"), NULL))) }",
        "[1] 2 1\n",
    ),
    (
        "{ e <- Dict.single(1L, \"a\"); e <- Dict.insert(e, 2L, \"b\"); e <- Dict.insert(e, 2.5, \"c\"); List.to_vector(Dict.keys(e)) }",
        "[1] 1.0 2.0 2.5\n",
    ),
    (
        "{ m <- Dict.empty(); for (i in 1:100) m <- Dict.insert(m, i %% 10L, i); List.to_vector(Dict.values(m)) }",
        " [1]  91  92  93  94  95  96  97  98  99 100\n",
    ), // Breadth: Result.
    (
        "Result.map2(:Ok(2L), :Ok(3L), function(a, b) a * b)",
        ":Ok(6L)\n",
    ),
    (
        "Result.map2(:Ok(2L), :Err(:Second), function(a, b) a * b)",
        ":Err(:Second)\n",
    ),
    (
        "Result.map2(:Err(:First), :Err(:Second), function(a, b) a * b)",
        ":Err(:First)\n",
    ),
    // Breadth: List walks.
    (
        "List.walk_backwards_until(xs, 0L, function(s, x) if (x == 3L) :Break(s) else :Continue(s + x))",
        "[1] 3\n",
    ),
    (
        "List.walk_with_index(words, \"\", function(s, w, i) Str.concat(s, Str.concat(Num.to_str(i), w)))",
        "[1] \"1pear2fig3apple\"\n",
    ),
    (
        "List.walk_with_index_until(xs, 0L, function(s, x, i) if (i == 3L) :Break(s) else :Continue(s + x))",
        "[1] 4\n",
    ),
    (
        "List.walk_from(xs, 2L, 0L, function(s, x) s + x)",
        "[1] 3\n",
    ),
    (
        "List.walk_from(xs, 9L, 0L, function(s, x) s + x)",
        "[1] 0\n",
    ),
    (
        "List.walk_from_until(xs, 2L, 0L, function(s, x) :Break(x))",
        "[1] 1\n",
    ),
    (
        "List.walk_try(xs, 0L, function(s, x) :Ok(s + x))",
        ":Ok(6L)\n",
    ),
    (
        "List.walk_try(xs, 0L, function(s, x) if (x == 1L) :Err(:One) else :Ok(s + x))",
        ":Err(:One)\n",
    ),
    ("List.for_each(xs, function(x) x)", ""),
    (
        "List.for_each_try(xs, function(x) if (x < 3L) :Err(x) else :Ok(NULL))",
        ":Err(1L)\n",
    ),
    ("List.for_each_try(xs, function(x) :Ok(x))", ":Ok(NULL)\n"),
    // Breadth: List mapping, sorting and reshaping.
    (
        "List.to_vector(List.map3(xs, xs, list(1L, 1L), function(a, b, c) a + b + c))",
        "[1] 7 3\n",
    ),
    (
        "List.to_vector(List.map4(xs, xs, xs, xs, function(a, b, c, d) a + b + c + d))",
        "[1] 12  4  8\n",
    ),
    ("List.to_vector(List.sort_asc(xs))", "[1] 1 2 3\n"),
    (
        "List.to_vector(List.sort_desc(list(2, 10L, 1.5)))",
        "[1] 10.0  2.0  1.5\n",
    ),
    ("List.to_vector(List.swap(xs, 1L, 3L))", "[1] 2 1 3\n"),
    ("List.to_vector(List.swap(xs, 1L, 4L))", "[1] 3 1 2\n"),
    (
        "{ r <- List.replace(xs, 2L, 9L); c(List.to_vector(r$list), r$value) }",
        "[1] 3 9 2 1\n",
    ),
    ("List.replace(xs, 5L, 9L)$value", "[1] 9\n"),
    (
        "List.split_first(list(1L, 0L, 2L, 0L, 3L), 0L)",
        ":Ok((after = list(2L, 0L, 3L), before = list(1L)))\n",
    ),
    (
        "List.split_last(list(1L, 0L, 2L, 0L, 3L), 0L)",
        ":Ok((after = list(3L), before = list(1L, 0L, 2L)))\n",
    ),
    ("List.split_first(xs, 9L)", ":Err(:NotFound)\n"),
    ("length(List.split_on(list(1L, 0L, 2L, 0L), 0L))", "[1] 3\n"),
    (
        "List.to_vector(List.split_on(list(1L, 0L, 2L, 3L), 0L)[[2]])",
        "[1] 2 3\n",
    ),
    ("length(List.split_on(list(), 0L))", "[1] 1\n"),
    (
        "List.to_vector(List.split_on_list(list(1L, 0L, 0L, 2L, 0L, 3L), list(0L, 0L))[[2]])",
        "[1] 2 0 3\n",
    ),
    ("length(List.split_on_list(xs, list()))", "[1] 1\n"),
    (
        "List.to_vector(List.append_if_ok(xs, :Ok(4L)))",
        "[1] 3 1 2 4\n",
    ),
    (
        "List.to_vector(List.append_if_ok(xs, :Err(:No)))",
        "[1] 3 1 2\n",
    ),
    (
        "List.to_vector(List.prepend_if_ok(xs, :Ok(0L)))",
        "[1] 0 3 1 2\n",
    ),
    (
        "List.to_vector(List.concat_utf8(list(104L), \"i!\"))",
        "[1] 104 105  33\n",
    ),
    ("List.len(List.with_capacity(10L))", "[1] 0\n"),
    (
        "{ p <- List.with_capacity(4L); for (i in 1:6) p <- List.append(p, i); List.to_vector(p) }",
        "[1] 1 2 3 4 5 6\n",
    ),
    ("List.to_vector(List.reserve(xs, 100L))", "[1] 3 1 2\n"),
    (
        "{ p <- list(1L, 2L); q <- p; p <- List.swap(p, 1L, 2L); c(List.to_vector(p), List.to_vector(q)) }",
        "[1] 2 1 1 2\n",
    ),
    // Breadth: Str.
    (
        "Str.caseless_ascii_equals(\"Hello\", \"hELLO\")",
        "[1] TRUE\n",
    ),
    (
        "Str.caseless_ascii_equals(\"Hello\", \"Help\")",
        "[1] FALSE\n",
    ),
    (
        "Str.walk_utf8(\"abc\", 0L, function(s, b) s + b)",
        "[1] 294\n",
    ),
    (
        "Str.walk_utf8_with_index(\"ab\", 0L, function(s, b, i) s + b * i)",
        "[1] 293\n",
    ),
    (
        "Str.from_utf8_lossy(list(104L, 255L, 105L))",
        "[1] \"h�i\"\n",
    ),
    ("Str.from_utf8_lossy(list(104L, 0L))", "[1] \"h�\"\n"),
    // Breadth: Num.
    (
        "c(Num.is_lt(1L, 2L), Num.is_gt(1L, 2L), Num.is_lte(2, 2L), Num.is_gte(1, 2))",
        "[1]  TRUE FALSE  TRUE FALSE\n",
    ),
    ("Num.is_approx_eq(1, 1 + 1e-9)", "[1] TRUE\n"),
    ("Num.is_approx_eq(1, 1.1)", "[1] FALSE\n"),
    ("Num.is_approx_eq(1, 1.1, rtol = 0.2)", "[1] TRUE\n"),
    ("Num.is_approx_eq(0, 1e-3, atol = 1e-2)", "[1] TRUE\n"),
    (
        "c(Num.is_multiple_of(12L, 4L), Num.is_multiple_of(12L, 5L), Num.is_multiple_of(0L, 0L), Num.is_multiple_of(3L, 0L))",
        "[1]  TRUE FALSE  TRUE FALSE\n",
    ),
    (
        "c(Num.is_finite(1), Num.is_finite(Inf), Num.is_infinite(-Inf), Num.is_infinite(3L))",
        "[1]  TRUE FALSE  TRUE FALSE\n",
    ),
    ("Num.abs(-3L)", "[1] 3\n"),
    ("Num.abs(-2.5)", "[1] 2.5\n"),
    ("Num.abs_diff(3L, 10L)", "[1] 7\n"),
    ("Num.neg(4L)", "[1] -4\n"),
    (
        "c(Num.ceiling(1.2), Num.floor(-1.2), Num.round(2.5), Num.round(-2.5))",
        "[1]  2 -2  3 -3\n",
    ),
    ("typeof(Num.floor(3L))", "[1] \"integer\"\n"),
    ("Num.pow(2L, 10L)", "[1] 1024\n"),
    (
        "c(Num.sin(0), Num.cos(0), Num.tan(0), Num.atan(1) * 4)",
        "[1] 0.000000 1.000000 0.000000 3.141593\n",
    ),
    ("Num.add_saturated(2147483647L, 1L)", "[1] 2147483647\n"),
    ("Num.sub_saturated(-2147483647L, 1L)", "[1] -2147483647\n"),
    ("Num.mul_saturated(100000L, 100000L)", "[1] 2147483647\n"),
    ("Num.add_saturated(1.5, 1L)", "[1] 2.5\n"),
    ("Num.div_ceil_checked(7L, 2L)", ":Ok(4L)\n"),
    ("Num.div_ceil_checked(-7L, 2L)", ":Ok(-3L)\n"),
    ("Num.div_ceil_checked(7, 2)", ":Ok(4)\n"),
    ("Num.div_ceil_checked(7L, 0L)", ":Err(:DivByZero)\n"),
    ("Num.to_double(3L)", "[1] 3\n"),
    ("typeof(Num.to_double(3L))", "[1] \"double\"\n"),
    ("Num.to_integer_checked(-3.7)", ":Ok(-3L)\n"),
    ("Num.to_integer_checked(1e10)", ":Err(:OutOfBounds)\n"),
    ("Num.add(2L, 3L)", "[1] 5\n"),
    ("Num.sub(2.5, 1L)", "[1] 1.5\n"),
    ("Num.mul(6L, 7L)", "[1] 42\n"),
    ("List.walk(xs, 0L, Num.add)", "[1] 6\n"),
    ("Num.div(7L, 2L)", "[1] 3.5\n"),
    ("Num.div_trunc(-7L, 2L)", "[1] -3\n"),
    ("typeof(Num.div_trunc(7L, 2L))", "[1] \"integer\"\n"),
    ("Num.div_ceil(7L, 2L)", "[1] 4\n"),
    ("Num.div_ceil(-7.5, 2)", "[1] -3\n"),
    ("Num.rem(-7L, 2L)", "[1] -1\n"),
    ("Num.pow_int(3L, 4L)", "[1] 81\n"),
    ("typeof(Num.pow_int(2L, 0L))", "[1] \"integer\"\n"),
    ("Num.sqrt(16L)", "[1] 4\n"),
    ("Num.log(1)", "[1] 0\n"),
    (
        "c(Num.asin(1) * 2, Num.acos(-1))",
        "[1] 3.141593 3.141593\n",
    ),
    (
        "c(Num.is_nan(0 / 0), Num.is_nan(1), Num.is_nan(Inf), Num.is_nan(1L))",
        "[1]  TRUE FALSE FALSE FALSE\n",
    ),
    (
        "c(Num.bitwise_and(12L, 10L), Num.bitwise_or(12L, 10L), Num.bitwise_xor(12L, 10L))",
        "[1]  8 14  6\n",
    ),
    (
        "c(Num.bitwise_not(0L), Num.bitwise_not(-6L))",
        "[1] -1  5\n",
    ),
    ("Num.bitwise_and(-1L, 255L)", "[1] 255\n"),
    ("Num.shift_left_by(3L, 30L)", "[1] -1073741824\n"),
    (
        "c(Num.shift_right_by(-16L, 2L), Num.shift_right_zf_by(-16L, 28L), Num.shift_right_zf_by(-16L, 0L))",
        "[1]  -4  15 -16\n",
    ),
    (
        "c(Num.count_one_bits(255L), Num.count_one_bits(-1L), Num.count_leading_zero_bits(1L), Num.count_leading_zero_bits(0L), Num.count_trailing_zero_bits(8L), Num.count_trailing_zero_bits(0L))",
        "[1]  8 32 31 32  3 32\n",
    ),
    (
        "c(Num.pi, Num.tau / 2, Num.e)",
        "[1] 3.141593 3.141593 2.718282\n",
    ),
    // Bool.
    ("c(Bool.true, Bool.false)", "[1]  TRUE FALSE\n"),
    (
        "c(Bool.and(TRUE, FALSE), Bool.or(TRUE, FALSE), Bool.not(FALSE))",
        "[1] FALSE  TRUE  TRUE\n",
    ),
    (
        "c(Bool.is_eq(list(1L, \"a\"), list(1L, \"a\")), Bool.is_eq(1L, 1), Bool.is_not_eq(:A, :B))",
        "[1]  TRUE FALSE  TRUE\n",
    ),
    (
        "Bool.is_eq(Set.from_list(list(1L, 2L)), Set.from_list(list(2L, 1L)))",
        "[1] TRUE\n",
    ),
    (
        "List.to_vector(List.keep_if(xs, function(x) Bool.not(Num.is_even(x))))",
        "[1] 3 1\n",
    ),
    // Str: UTF-16 and UTF-32.
    (
        "List.to_vector(Str.to_utf16(\"h\\u00e9\\U0001F600\"))",
        "[1]   104   233 55357 56832\n",
    ),
    (
        "Result.with_default(Str.from_utf16(list(104L, 55357L, 56832L)), \"\") == \"h\\U0001F600\"",
        "[1] TRUE\n",
    ),
    ("Str.from_utf16(list(104L, 55357L))", ":Err(:BadUtf16)\n"),
    ("Str.from_utf16(list(0L))", ":Err(:BadUtf16)\n"),
    (
        "Str.from_utf16_lossy(list(104L, 56832L, 105L)) == \"h\\uFFFDi\"",
        "[1] TRUE\n",
    ),
    (
        "List.to_vector(Str.to_utf32(\"h\\u00e9\"))",
        "[1] 104 233\n",
    ),
    ("Str.from_utf32(list(104L, 105L))", ":Ok(\"hi\")\n"),
    (
        "List.all(list(55296L, 1114112L, -1L, 0L), function(c) Result.is_err(Str.from_utf32(list(c))))",
        "[1] TRUE\n",
    ),
    (
        "Str.from_utf32_lossy(list(104L, -1L, 105L)) == \"h\\uFFFDi\"",
        "[1] TRUE\n",
    ),
    // Breadth: Dict and Set.
    (
        "Dict.walk_until(d, 0, function(s, k, v) if (k == \"b\") :Break(s) else :Continue(s + v))",
        "[1] 1\n",
    ),
    (
        "Dict.join_map(d, function(k, v) Dict.single(v, k))",
        "Dict.from_list(list((1, \"a\"), (2, \"b\")))\n",
    ),
    ("Dict.len(Dict.reserve(d, 100L))", "[1] 2\n"),
    (
        "{ r <- Dict.reserve(d, 100L); r <- Dict.insert(r, \"c\", 3); c(Dict.len(r), Dict.len(d)) }",
        "[1] 3 2\n",
    ),
    ("Dict.len(Dict.reserve(Dict.empty(), 10L))", "[1] 0\n"),
    ("Dict.clear(d)", "Dict.empty()\n"),
    (
        "Set.walk_until(s, 0L, function(t, e) if (e == 3L) :Break(t) else :Continue(t + e))",
        "[1] 3\n",
    ),
    (
        "Set.join_map(s, function(e) Set.from_list(list(e, e * 10L)))",
        "Set.from_list(list(1L, 10L, 2L, 20L, 3L, 30L))\n",
    ),
    (
        "{ r <- Set.reserve(s, 50L); for (i in 4:60) r <- Set.insert(r, i); c(Set.len(r), Set.len(s)) }",
        "[1] 60  3\n",
    ),
];

/// Each case's code and the error message it raises.
const ERRORS: &[(&str, &str)] = &[
    // Compiled loops hand a failing call back to tier 0, which raises the same error.
    (
        "{ f <- function(m) { for (i in 1:50) r <- Dict.len(if (i > 40L) 1 else m); r }; f(d) }",
        "`dict` must be a dict, not a double value",
    ),
    (
        "{ f <- function(m) { for (i in 1:50) r <- Dict.get(m, if (i > 40L) NA_character_ else \"a\"); r }; f(d) }",
        "`key` must be a single logical, number or string, not `NA`",
    ),
    (
        "{ f <- function(m) { for (i in 1:50) r <- Dict.contains(m, if (i > 40L) 1L else \"a\"); r }; f(d) }",
        "the dict's keys are character, and this one is integer; a collection holds one type",
    ),
    (
        "Dict.get(d, 1L)",
        "the dict's keys are character, and this one is integer; a collection holds one type",
    ),
    (
        "Set.contains(s, \"a\")",
        "the set's elements are integer, and this one is character; a collection holds one type",
    ),
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
        "Num.to_integer_checked(NaN)",
        "`number` must be a single number, not `NaN`",
    ),
    (
        "List.walk_from(xs, 0L, 0L, function(s, x) s)",
        "`index` must be a position of 1 or more",
    ),
    (
        "Num.abs_diff(2147483647L, -2147483647L)",
        "the integer result overflows",
    ),
    (
        "List.walk_try(xs, 0L, function(s, x) s)",
        "`f` must return a result",
    ),
    (
        "Dict.join_map(d, function(k, v) v)",
        "`f` must return a dict, not a double value",
    ),
    (
        "List.sum(list(2147483647L, 1L))",
        "the integer result overflows; convert the elements to doubles first",
    ),
    (
        "Num.add(2147483647L, 1L)",
        "the integer result overflows; `Num.add_checked()` returns `:Err(:Overflow)` instead",
    ),
    (
        "Num.mul(65536L, 65536L)",
        "the integer result overflows; `Num.mul_checked()` returns `:Err(:Overflow)` instead",
    ),
    ("Num.sub(Inf, Inf)", "the result is undefined (`NaN`)"),
    (
        "Num.div(1, 0)",
        "`second` is zero; `Num.div_checked()` returns `:Err(:DivByZero)` instead",
    ),
    (
        "Num.rem(7L, 0L)",
        "`second` is zero; `Num.rem_checked()` returns `:Err(:DivByZero)` instead",
    ),
    (
        "Num.div_trunc(7L, 0L)",
        "`second` is zero; `Num.div_trunc_checked()` returns `:Err(:DivByZero)` instead",
    ),
    (
        "Num.pow_int(2L, 31L)",
        "the integer result overflows; `Num.pow()` returns a double instead",
    ),
    (
        "Num.pow_int(2L, -1L)",
        "`exponent` must be zero or more, not -1",
    ),
    (
        "Num.sqrt(-4)",
        "`number` must not be negative, not -4; `Num.sqrt_checked()` returns `:Err(:SqrtOfNegative)` instead",
    ),
    (
        "Num.log(0L)",
        "`number` must be positive, not 0; `Num.log_checked()` returns `:Err(:LogNeedsPositive)` instead",
    ),
    ("Num.asin(2)", "`number` must be from -1 to 1, not 2"),
    (
        "Num.is_nan(NA_real_)",
        "`number` must be a single number, not `NA`",
    ),
    (
        "Num.bitwise_not(2147483647L)",
        "the result is -2147483648, which an R integer cannot hold because it stands for `NA`",
    ),
    ("Num.bitwise_and(1.5, 1L)", "`first` must be a whole number"),
    (
        "Num.shift_left_by(1L, 32L)",
        "`bits` must be from 0 to 31, not 32",
    ),
    (
        "Bool.not(NA)",
        "`value` must be `TRUE` or `FALSE`, not `NA`",
    ),
    (
        "Bool.and(1, TRUE)",
        "`first` must be `TRUE` or `FALSE`, not a double value",
    ),
    (
        "Str.from_utf16(list(70000L))",
        "`units` must be a list of numbers from 0 to 65535",
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
            // The list conversions are base builtins and `Ops.Expr` is an S3 method of a group
            // generic, declared here only for the checker.
            if name.starts_with("as.") || name.starts_with("Ops.") {
                return None;
            }
            // A declaration without a function type is a constant such as `Bool.true`.
            let Some(start) = signature.find("fn(") else {
                return Some((name.to_owned(), Vec::new()));
            };
            let parameters = &signature[start + 3..];
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
    // The list and Array conversions are base builtins, listed in the manifest only for the
    // checker.
    let outside = |name: &str| {
        matches!(
            name,
            "as.record" | "as.tuple" | "as.tag" | "as.Array" | "is.Array"
        )
    };
    let exported: BTreeSet<String> = manifest
        .lines()
        .filter(|line| !line.starts_with('#') && !outside(line))
        .map(str::to_owned)
        .collect();
    // An S3 method such as `Ops.Expr` is declared for the checker but registered, not exported.
    let registered: BTreeSet<&str> = dataframe::METHODS
        .lines()
        .filter_map(|line| line.trim().split_once(" = function"))
        .map(|(name, _)| name)
        .collect();
    let declared: BTreeSet<String> = declarations()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !outside(name) && !registered.contains(name.as_str()))
        .collect();
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
        failures.extend(move_undo_failures());
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

/// A helper that updates a moved dictionary and then fails leaves the caller's variable as it
/// was before the call. The error comes from a builtin: one raised by a closure such as `stop`
/// would end the move as the closure starts, which restores the variable before the journal is
/// needed. The failure unwinds past the caller, so each case keeps the caller's
/// frame in `frame` and reads the variable from it afterwards.
unsafe fn move_undo_failures() -> Vec<String> {
    let cases = [
        (
            "{ bad <- function(m, k) { m <- Dict.insert(m, k, 1); m <- Dict.insert(m, 1L, 1) }; f <- function() { for (i in 1) NULL; m <- Dict.empty(); frame <<- environment(); m <- bad(m, \"a\") }; f() }",
            "Dict.len(frame$m)",
            "[1] 0\n",
        ),
        (
            "{ bad <- function(m, k) { m <- Dict.insert(m, \"new\", 1); m <- Dict.insert(m, k, 2); m <- Dict.insert(m, 1L, 1) }; f <- function() { for (i in 1) NULL; m <- Dict.single(\"a\", 0); m <- Dict.insert(m, \"b\", 0); frame <<- environment(); m <- bad(m, \"a\") }; f() }",
            "c(Dict.len(frame$m), Result.with_default(Dict.get(frame$m, \"a\"), -1))",
            "[1] 2 0\n",
        ),
        (
            "{ bad <- function(s, k) { s <- Set.insert(s, k); s <- Set.insert(s, \"x\") }; f <- function() { for (i in 1) NULL; s <- Set.from_list(list(1L, 2L)); frame <<- environment(); s <- bad(s, 3L) }; f() }",
            "Set.len(frame$s)",
            "[1] 2\n",
        ),
        (
            "{ bad <- function(xs, v) { xs <- List.set(xs, 1L, v); xs <- List.append(xs, v); xs <- List.swap(xs, 1L, 2L); xs <- Dict.insert(Dict.single(1L, 1), \"k\", xs) }; f <- function() { for (i in 1) NULL; xs <- List.append(List.append(list(), 1), 2); frame <<- environment(); xs <- bad(xs, 5) }; f() }",
            "c(frame$xs[[1]], frame$xs[[2]], length(frame$xs))",
            "[1] 1 2 2\n",
        ),
    ];
    let mut failures = Vec::new();
    for (failing, check, expected) in cases {
        // SAFETY: the interpreter is installed.
        let reported = unsafe { printed(failing) };
        if !reported.contains("a collection holds one type") {
            failures.push(format!(
                "`{failing}`\n  reported {reported:?}\n  expected a key type error"
            ));
        }
        // SAFETY: as above.
        let printed = unsafe { printed(check) };
        if printed != expected {
            failures.push(format!(
                "`{check}` after `{failing}`\n  printed  {printed:?}\n  expected {expected:?}"
            ));
        }
    }
    failures
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
