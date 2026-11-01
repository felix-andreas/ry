//! ry's records, tuples, tags and `match` at runtime: construction and printing, the language
//! objects the syntax lowers to and their deparse, field access and its errors, `match` with
//! guards and bindings, the conversions to and from lists, the error a builtin raises for a value
//! it does not understand, and the collector tracing record slots. The interpreter is global to
//! the process, so everything runs as one test.
//!
//! The interpreter runs here without base R, so the setup defines the few generics base R would
//! (`print`, `format`, `as.list`, `all.equal`) and wrappers for the internals the cases call.

use std::cell::RefCell;

use heap::{Heap, object::Sexp};
use interp::{eval, lower, print, unwind::Unwind, values};

const SETUP: &str = r#"
print <- function(x, ...) UseMethod("print")
print.default <- function(x, ...) .Internal(print.default(x, NULL, NULL))
format <- function(x, ...) UseMethod("format")
as.list <- function(x, ...) UseMethod("as.list")
all.equal <- function(target, current, ...) UseMethod("all.equal")
identical <- function(x, y) .Internal(identical(x, y, TRUE, TRUE, TRUE, TRUE, FALSE, TRUE, FALSE))
deparse <- function(expr) .Internal(deparse(expr, 60L, TRUE, 1089L, -1L))
typeof <- function(x) .Internal(typeof(x))
inherits <- function(x, what) .Internal(inherits(x, what, FALSE))
gc <- function() invisible(.Internal(gc(FALSE, FALSE, TRUE)))
lapply <- function(X, FUN, ...) .Internal(lapply(X, FUN))
parent.frame <- function(n = 1) .Internal(parent.frame(n))
assign <- function(x, value, envir = parent.frame()) .Internal(assign(x, value, envir, FALSE))
exists <- function(x) .Internal(exists(x, globalenv(), "any", FALSE))
all.equal.default <- function(target, current, ...) if (identical(target, current)) TRUE else "values differ"
point <- (y = 2, x = 1)
pair <- (1L, "a")
circle <- :Circle(radius = 2)
type UserId = integer
alias Shape = :Circle(radius: double) | :Empty
type Point = record{x: double, y: double}
type OrderId = record{id: integer}
type Pair = tuple{integer, character}
type Figure = :Circle(radius: double) | :Empty
type Tree<T> = :Leaf | :Node(left: Tree<T>, value: T, right: Tree<T>)
size <- function(t: Tree<integer>) -> integer {
  match (t) {
    :Leaf => 0L
    :Node(left = l, value = _, right = r) => size(l) + 1L + size(r)
  }
}
"#;

/// Each case's code and what printing its value shows.
const PRINTS: &[(&str, &str)] = &[
    // Construction and printing: fields print sorted by name.
    ("point", "(x = 1, y = 2)\n"),
    ("pair", "(1L, \"a\")\n"),
    ("(1L,)", "(1L,)\n"),
    ("(x = 1,)", "(x = 1,)\n"),
    ("()", "()\n"),
    (":None", ":None\n"),
    ("circle", ":Circle(radius = 2)\n"),
    (":Some(5L)", ":Some(5L)\n"),
    (
        "(inner = (1, c(2, 3)), tag = :Ok(\"done\"))",
        "(inner = (1, c(2, 3)), tag = :Ok(\"done\"))\n",
    ),
    ("list(a = point)", "$a\n(x = 1, y = 2)\n\n"),
    // R's own forms keep their meaning.
    ("(x = 1)", "[1] 1\n"),
    ("{a:b}", "Error: object 'a' not found\n"),
    ("{ a <- 2; b <- 3; {a:b} }", "[1] 2 3\n"),
    // typeof, class and inherits.
    ("typeof(point)", "[1] \"record\"\n"),
    ("typeof(pair)", "[1] \"tuple\"\n"),
    ("typeof(circle)", "[1] \"tag\"\n"),
    ("class(point)", "[1] \"ry_record\"\n"),
    ("class(circle)", "[1] \"ry_tag\"\n"),
    ("inherits(pair, \"ry_tuple\")", "[1] TRUE\n"),
    ("is.list(point)", "[1] FALSE\n"),
    ("is.null(point)", "[1] FALSE\n"),
    ("length(point)", "[1] 2\n"),
    ("length(()) ", "[1] 0\n"),
    // Access and update.
    ("point$x", "[1] 1\n"),
    ("point[[\"y\"]]", "[1] 2\n"),
    ("pair[[2L]]", "[1] \"a\"\n"),
    ("pair[[1]]", "[1] 1\n"),
    ("point[[1L]]", "[1] 1\n"),
    ("{ p <- point; p$x <- 10; p }", "(x = 10, y = 2)\n"),
    ("{ p <- point; p$x <- 10; point }", "(x = 1, y = 2)\n"),
    ("{ t <- pair; t[[2]] <- \"b\"; t }", "(1L, \"b\")\n"),
    (
        "{ p <- (a = (b = 1,),); p$a$b <- 5; p }",
        "(a = (b = 5,),)\n",
    ),
    // Structural equality, with field order not part of the value.
    ("identical(point, (x = 1, y = 2))", "[1] TRUE\n"),
    ("identical(point, (x = 1, y = 3))", "[1] FALSE\n"),
    ("identical((1L,), (1,))", "[1] FALSE\n"),
    ("identical(:A(1), :B(1))", "[1] FALSE\n"),
    ("identical((x = c(1, 2),), (x = c(1, 2),))", "[1] TRUE\n"),
    ("all.equal(point, (x = 1, y = 2))", "[1] TRUE\n"),
    (
        "all.equal(point, (x = 1, y = 3))",
        "[1] \"Field `y`: values differ\"\n",
    ),
    (
        "all.equal(point, pair)",
        "[1] \"target is a ry record, current is a ry tuple\"\n",
    ),
    (
        "all.equal(:A, :B)",
        "[1] \"target is tag `:A`, current is tag `:B`\"\n",
    ),
    // format and deparse use ry syntax.
    ("format(circle)", "[1] \":Circle(radius = 2)\"\n"),
    ("deparse(point)", "[1] \"(x = 1, y = 2)\"\n"),
    // Inline type definitions and nominal constructors exist only for the checker.
    ("@UserId(7L)", "[1] 7\n"),
    ("deparse(quote(@UserId(7L)))", "[1] \"(7L)\"\n"),
    ("@Shape(:Circle(radius = 1))", ":Circle(radius = 1)\n"),
    ("{ id: @new UserId <- 5L; id }", "[1] 5\n"),
    // A nominal record, tuple or tag union is its representation at run time: `@Name(...)`'s
    // parentheses are the value's own.
    ("@Point(y = 2, x = 1)", "(x = 1, y = 2)\n"),
    ("@OrderId(id = 3L,)", "(id = 3L,)\n"),
    ("@Pair(1L, \"a\")", "(1L, \"a\")\n"),
    ("@Point(x = 1, y = 2)$y", "[1] 2\n"),
    ("{ p <- @Point(x = 1, y = 2); p$x <- 5; p }", "(x = 5, y = 2)\n"),
    (
        "match (@Figure(:Circle(radius = 2))) { :Circle(radius = r) => r; :Empty => 0 }",
        "[1] 2\n",
    ),
    (
        "size(@Tree(:Node(left = @Tree(:Leaf), value = 1L, right = @Tree(:Node(left = :Leaf, value = 2L, right = :Leaf)))))",
        "[1] 2\n",
    ),
    ("as.list(@Point(x = 1, y = 2))$y", "[1] 2\n"),
    ("{ type <- 3; alias <- 4; type + alias }", "[1] 7\n"),
    ("deparse(quote((x = 1, y = 2)))", "[1] \"(x = 1, y = 2)\"\n"),
    ("deparse(quote((b = 1, a = 2)))", "[1] \"(b = 1, a = 2)\"\n"),
    ("deparse(quote((1L,)))", "[1] \"(1L,)\"\n"),
    ("deparse(quote(()))", "[1] \"()\"\n"),
    ("deparse(quote(:None))", "[1] \":None\"\n"),
    (
        "deparse(quote(:Circle(radius = 2)))",
        "[1] \":Circle(radius = 2)\"\n",
    ),
    ("quote((x = 1, y = 2))[[1]]", ".ry_record\n"),
    ("quote(:Some(1))[[2]]", "[1] \"Some\"\n"),
    (
        "quote(match (s) { :Circle(radius = r) => pi * r^2; :Rect(width = w, ..) if (w > 0) => w; (a, _) | (_, a) => a; -1L => 0; (k = v,) => v; _ => NULL })",
        "match (s) {\n    :Circle(radius = r) => pi * r^2\n    :Rect(width = w, ..) if (w > 0) => w\n    (a, _) | (_, a) => a\n    -1L => 0\n    (k = v,) => v\n    _ => NULL\n}\n",
    ),
    (
        "quote(match (p) { (x = a, ..) if (a > 0) => a })",
        "match (p) {\n    (x = a, ..) if (a > 0) => a\n}\n",
    ),
    (
        "quote(match (a, b) { (1, _) => TRUE; _ => FALSE })",
        "match (a, b) {\n    (1, _) => TRUE\n    _ => FALSE\n}\n",
    ),
    (
        "quote(match (x) { :Some(..) => 1 })[[3]][[2]]",
        ":Some(..)\n",
    ),
    (
        "quote(match (x) { :Some(..) => 1 })[[3]][[2]][[1]]",
        ".ry_tag\n",
    ),
    ("quote(match (x) { :Some(..) => 1 })[[3]][[2]][[3]]", "..\n"),
    ("quote(match (x) { 1 => 2 })[[1]]", ".ry_match\n"),
    // Conversions.
    ("as.list(point)", "$x\n[1] 1\n\n$y\n[1] 2\n\n"),
    ("as.list(pair)", "[[1]]\n[1] 1\n\n[[2]]\n[1] \"a\"\n\n"),
    (
        "as.list(circle)",
        "$tag\n[1] \"Circle\"\n\n$payload\n$payload$radius\n[1] 2\n\n\n",
    ),
    ("as.record(list(b = 1, a = \"x\"))", "(a = \"x\", b = 1)\n"),
    ("as.tuple(list(1, 2L))", "(1, 2L)\n"),
    ("as.tag(\"Ok\", list(value = 3))", ":Ok(value = 3)\n"),
    ("as.tag(\"None\")", ":None\n"),
    (
        "as.tag(as.list(:Circle(radius = 2)))",
        ":Circle(radius = 2)\n",
    ),
    ("as.tag(as.list(:None))", ":None\n"),
    ("identical(as.record(as.list(point)), point)", "[1] TRUE\n"),
    // A list of records works, because storing a value does not look inside it.
    ("list(point, pair)[[2]]", "(1L, \"a\")\n"),
    (
        "{ l <- list(); l[[1]] <- circle; l[[1]] }",
        ":Circle(radius = 2)\n",
    ),
    ("lapply(list(point), function(p) p$y)[[1]]", "[1] 2\n"),
    // Builtins that pass values along.
    ("invisible(point)", ""),
    ("{ assign(\"q\", point); q }", "(x = 1, y = 2)\n"),
    ("(function(v) v)(pair)", "(1L, \"a\")\n"),
    // S3 dispatch on the implicit class.
    (
        "{ format.ry_tag <- function(x, ...) \"a tag\"; format(circle) }",
        "[1] \"a tag\"\n",
    ),
    // `match`.
    (
        "match (circle) { :Rect(width = w, height = h) => w * h; :Circle(radius = r) => r * 10 }",
        "[1] 20\n",
    ),
    (
        "match (:Some(5L)) { :Some(x) if (x > 10L) => \"big\"; :Some(x) => x; :None => 0L }",
        "[1] 5\n",
    ),
    ("{ match (:Some(3)) { :Some(n) => n }; n }", "[1] 3\n"),
    (
        "{ g <- 0; match (2) { v if (v > 5) => 1; _ => 2 }; exists(\"v\") }",
        "[1] FALSE\n",
    ),
    (
        "match (1L) { 1 => \"one\"; _ => \"other\" }",
        "[1] \"one\"\n",
    ),
    (
        "match (NA_real_) { NA => \"missing\"; _ => \"other\" }",
        "[1] \"missing\"\n",
    ),
    (
        "match (NaN) { NA => \"missing\"; NaN => \"nan\"; _ => \"other\" }",
        "[1] \"nan\"\n",
    ),
    ("match (NA_character_) { \"a\" => 1; NA => 2 }", "[1] 2\n"),
    (
        "match (TRUE) { 1 => \"one\"; TRUE => \"true\" }",
        "[1] \"true\"\n",
    ),
    (
        "match (c(1, 2)) { 1 => \"one\"; _ => \"vector\" }",
        "[1] \"vector\"\n",
    ),
    (
        "match (NULL) { NULL => \"null\"; _ => \"other\" }",
        "[1] \"null\"\n",
    ),
    (
        "match (-2) { -2 => \"minus two\"; _ => \"other\" }",
        "[1] \"minus two\"\n",
    ),
    (
        "match ((1, \"b\")) { (1, \"a\") => 1; (_, s) => s }",
        "[1] \"b\"\n",
    ),
    (
        "match (point) { (x = a,) => 1; (x = a, ..) => a + 100 }",
        "[1] 101\n",
    ),
    ("match (point) { (y = b, x = a) => b - a }", "[1] 1\n"),
    (
        "match ((1, 2)) { (a, 9) | (9, a) => a; (a, b) => a + b }",
        "[1] 3\n",
    ),
    ("match ((9, 4)) { (a, 9) | (9, a) => a; _ => 0 }", "[1] 4\n"),
    (
        "match (:Rect(width = 2, height = 3)) { :Rect(..) => \"rect\" }",
        "[1] \"rect\"\n",
    ),
    (
        "match (:None) { :Some(x) => x; :None => \"none\" }",
        "[1] \"none\"\n",
    ),
    ("match (pair) { (_,) => 1; (_, _) => 2 }", "[1] 2\n"),
    (
        "{ f <- function(r) match (r) { :Ok(v) => v; :Err(e) => paste0 }; f(:Ok(7)) }",
        "[1] 7\n",
    ),
    ("{ x <- 1; match (x) { y => { x <- 2 } }; x }", "[1] 2\n"),
    ("match (5) { ((x)) => x }", "[1] 5\n"),
    // The collector traces handle slots and leaves raw ones alone: records built before a run of
    // full collections, with garbage allocated in between, keep every slot.
    (
        "{ keep <- lapply(1:300, function(i) (d = i / 2, s = :S((i, list(i))), v = c(i, i))); for (k in 1:4) { junk <- lapply(1:400, function(j) c(j, j)); gc() }; keep[[250]] }",
        "(d = 125, s = :S((250L, list(250L))), v = c(250L, 250L))\n",
    ),
    (
        "{ l <- list(:Circle(radius = 3)); gc(); l[[1]] }",
        ":Circle(radius = 3)\n",
    ),
];

/// Each case's code and the error message it raises.
const ERRORS: &[(&str, &str)] = &[
    (
        "point$z",
        "the record has no field `z`; its fields are `x`, `y`",
    ),
    ("point$ x", "[1] 1"),
    (
        "{ p <- (alpha = 1,); p$al }",
        "the record has no field `al`; its fields are `alpha`",
    ),
    ("pair$x", "a tuple has no field `x`"),
    (
        "circle$radius",
        "a tag has no fields; read its payload with `match`",
    ),
    (
        "pair[[3L]]",
        "index 3L is out of range for a tuple of length 2",
    ),
    (
        "pair[[0]]",
        "index 0 is out of range for a tuple of length 2",
    ),
    ("{ p <- point; p$z <- 1 }", "the record has no field `z`"),
    (
        "sum(point)",
        "`point` is a ry record, not an R object; convert it with `as.list()`",
    ),
    (
        "c(pair, 1)",
        "`pair` is a ry tuple, not an R object; convert it with `as.list()`",
    ),
    (
        "names(circle)",
        "`circle` is a ry tag, not an R object; convert it with `as.list()`",
    ),
    ("point[1]", "`point` is a ry record, not an R object"),
    (
        "(function(...) sum(...))(point)",
        "argument 1 is a ry record, not an R object",
    ),
    (
        "for (v in point) v",
        "`point` is a ry record, not an R object",
    ),
    ("if (point) 1", "`point` is a ry record, not an R object"),
    (
        "as.character(point)",
        "`point` is a ry record, not an R object",
    ),
    (
        "match (3) { 1 => 1; 2 => 2 }",
        "no arm of this `match` matches 3",
    ),
    (
        "match (:Triangle(sides = 3L)) { :Circle(radius = r) => r }",
        "no arm of this `match` matches :Triangle(sides = 3L)",
    ),
    (
        "match (1) { v if (NA) => v }",
        "missing value where TRUE/FALSE needed",
    ),
    (
        ".ry_match(1, `=>`(T, 1))",
        "`T` in a pattern would bind any value to a variable; write `TRUE`",
    ),
    ("(1L, y = 2)", "this item cannot have a name"),
    (
        ".ry_tuple(1L, y = 2)",
        "a tuple's items have no names; use a record for named fields",
    ),
    (
        ".ry_record(a = 1, 2)",
        "every field of a record needs a name",
    ),
    (".ry_record(a = 1, a = 2)", "the field `a` appears twice"),
    (
        "as.record(list(1, 2))",
        "`as.record()`: element 1 has no name",
    ),
    (
        "as.tuple(list(a = 1))",
        "`as.tuple()`: element 1 is named `a`",
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

#[test]
fn records_tuples_tags_and_match() {
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
            take_output();
            let printed = match evaluate(code) {
                Ok(value) => {
                    if interp::interp().visible {
                        if let Err(failure) = print::print_value_top(value, values::global_env()) {
                            failures
                                .push(format!("`{code}`: printing failed: {}", message(failure)));
                            continue;
                        }
                    }
                    take_output()
                }
                Err(failure) => format!("{}\n", failure.trim_end()),
            };
            if printed != *expected {
                failures.push(format!(
                    "`{code}`\n  printed  {printed:?}\n  expected {expected:?}"
                ));
            }
        }
        for (code, expected) in ERRORS {
            take_output();
            let reported = match evaluate(code) {
                Ok(value) => {
                    if print::print_value_top(value, values::global_env()).is_err() {
                        failures.push(format!("`{code}`: printing failed"));
                    }
                    take_output()
                }
                Err(failure) => failure,
            };
            if !reported.contains(expected) {
                failures.push(format!(
                    "`{code}`\n  reported {reported:?}\n  expected {expected:?}"
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} cases failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
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
