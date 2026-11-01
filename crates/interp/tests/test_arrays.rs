//! ry's `Array<T>` at runtime: the `[1, 2, 3]` constructor and its errors, the strict operator,
//! indexing and assignment rules, printing and deparsing, and properties of the conversions to
//! and from R vectors. The interpreter is global to the process, so everything runs as one test.
//!
//! The interpreter runs here without base R, so the setup defines the few closures base R would.

use {
    heap::{Heap, object::Sexp},
    interp::{eval, lower, print, unwind::Unwind, values},
    std::cell::RefCell,
};

const SETUP: &str = r#"
print <- function(x, ...) UseMethod("print")
print.default <- function(x, ...) .Internal(print.default(x, NULL, NULL))
identical <- function(x, y) .Internal(identical(x, y, TRUE, TRUE, TRUE, TRUE, FALSE, TRUE, FALSE))
deparse <- function(expr) .Internal(deparse(expr, 60L, TRUE, 1089L, -1L))
typeof <- function(x) .Internal(typeof(x))
as.vector <- function(x, mode = "any") .Internal(as.vector(x, mode))
sum <- function(...) .Primitive("sum")(...)
xs <- [1, 2, 3]
ns <- [1L, 2L, 3L]
flags <- [TRUE, FALSE, TRUE]
words <- ["a", "b"]
points <- [(x = 1, y = 2), (x = 3, y = 4)]
"#;

/// Each case's code and what printing its value shows.
const PRINTS: &[(&str, &str)] = &[
    // Construction, printing and the R vector underneath.
    ("xs", "[1, 2, 3]\n"),
    ("ns", "[1L, 2L, 3L]\n"),
    ("flags", "[TRUE, FALSE, TRUE]\n"),
    ("words", "[\"a\", \"b\"]\n"),
    ("points", "[(x = 1, y = 2), (x = 3, y = 4)]\n"),
    ("[]", "[]\n"),
    ("[[1, 2], [3]]", "[[1, 2], [3]]\n"),
    ("is.Array(xs)", "[1] TRUE\n"),
    ("is.Array(c(1, 2, 3))", "[1] FALSE\n"),
    ("typeof(xs)", "[1] \"double\"\n"),
    ("typeof(points)", "[1] \"list\"\n"),
    ("length(xs)", "[1] 3\n"),
    ("sum(xs)", "[1] 6\n"),
    ("deparse(quote([1, x]))", "[1] \"[1, x]\"\n"),
    // Conversions.
    ("as.vector(xs)", "[1] 1 2 3\n"),
    ("identical(as.vector(xs), c(1, 2, 3))", "[1] TRUE\n"),
    ("identical(xs, c(1, 2, 3))", "[1] FALSE\n"),
    ("as.Array(c(4L, 5L))", "[4L, 5L]\n"),
    ("as.Array(c(a = 1, b = 2))", "[1, 2]\n"),
    ("as.Array(xs)", "[1, 2, 3]\n"),
    ("as.Array(list((1,), (2,)))", "[(1,), (2,)]\n"),
    (
        "names(xs) <- c(\"a\", \"b\", \"c\"); is.Array(xs)",
        "[1] FALSE\n",
    ),
    ("xs <- [1, 2, 3]; is.Array(xs)", "[1] TRUE\n"),
    // Operators.
    ("xs * 2", "[2, 4, 6]\n"),
    ("xs + [10, 20, 30]", "[11, 22, 33]\n"),
    ("-xs", "[-1, -2, -3]\n"),
    ("ns %/% 2L", "[0L, 1L, 1L]\n"),
    ("ns / 2L", "[0.5, 1, 1.5]\n"),
    ("xs > 1", "[FALSE, TRUE, TRUE]\n"),
    ("words == \"a\"", "[TRUE, FALSE]\n"),
    ("flags & [TRUE, TRUE, FALSE]", "[TRUE, FALSE, FALSE]\n"),
    ("!flags", "[FALSE, TRUE, FALSE]\n"),
    ("is.Array(xs > 1)", "[1] TRUE\n"),
    ("[1] == 1", "[TRUE]\n"),
    ("is.Array(1 == 1)", "[1] FALSE\n"),
    // Indexing.
    ("xs[[2]]", "[1] 2\n"),
    ("is.Array(xs[[2]])", "[1] FALSE\n"),
    ("xs[2:3]", "[2, 3]\n"),
    ("xs[xs > 1]", "[2, 3]\n"),
    ("positions <- [3L, 1L]; xs[positions]", "[3, 1]\n"),
    ("xs[]", "[1, 2, 3]\n"),
    ("points[[2]]", "(x = 3, y = 4)\n"),
    ("ys <- xs; ys[[1]] <- 10; ys", "[10, 2, 3]\n"),
    ("xs", "[1, 2, 3]\n"),
    ("ys <- xs; ys[2:3] <- [20, 30]; ys", "[1, 20, 30]\n"),
    ("ys <- xs; ys[xs > 1] <- 0; ys", "[1, 0, 0]\n"),
    (
        "total <- 0; for (x in xs) total <- total + x; total",
        "[1] 6\n",
    ),
    ("v <- c(1, 2, 3); v[2:3] <- [9, 8]; v", "[1] 1 9 8\n"),
    // Inline annotations check the Array bit at a function's boundary.
    (
        "total <- function(x: Array<double>) -> double { sum(x) }; total(xs)",
        "[1] 6\n",
    ),
    (
        "wants_vector <- function(x: double[]) length(x); wants_vector(xs)",
        "[1] 3\n",
    ),
    // The standard library's List functions take an Array and give one back.
    ("List.map(xs, function(x) x * 2)", "[2, 4, 6]\n"),
    ("List.keep_if(xs, function(x) x > 1)", "[2, 3]\n"),
    ("List.sum(xs)", "[1] 6\n"),
    ("List.len(xs)", "[1] 3\n"),
    ("List.map(xs, function(x) x > 1)", "[FALSE, TRUE, TRUE]\n"),
    ("List.map(points, function(p) p$x)", "[1, 3]\n"),
    ("List.first(xs)", ":Ok(1)\n"),
    ("List.reverse(ns)", "[3L, 2L, 1L]\n"),
    ("List.concat(xs, [4])", "[1, 2, 3, 4]\n"),
    ("List.contains(words, \"a\")", "[1] TRUE\n"),
    ("List.chunks_of(xs, 2L)", "[[1, 2], [3]]\n"),
    ("List.join([[1], [2, 3]])", "[1, 2, 3]\n"),
    // R's own positions where they give no NA.
    ("xs[-1]", "[2, 3]\n"),
    ("xs[0]", "[]\n"),
    ("xs[c(0, 2)]", "[2]\n"),
    ("xs[-5]", "[1, 2, 3]\n"),
    // A value an Array cannot hold makes the result the plain R vector, so R code keeps working.
    ("ys <- xs; ys[[1]] <- NA_real_; ys", "[1] NA  2  3\n"),
    ("ys <- flags; ys[1:2] <- c(1, 2); ys", "[1] 1 2 1\n"),
    ("ys <- xs; ys[[1]] <- \"a\"; ys", "[1] \"a\" \"2\" \"3\"\n"),
    ("ys <- xs; ys[[1]] <- NA_real_; xs", "[1, 2, 3]\n"),
    ("ys <- xs; ys[2] <- 9; is.Array(ys)", "[1] TRUE\n"),
    // A copy that a builtin writes into is a plain vector, never a stale Array.
    ("ys <- xs; ys[[1]] <- NA_real_; is.Array(ys)", "[1] FALSE\n"),
    // Only the collection argument makes a `List` result an Array.
    (
        "List.append(list(\"a\"), [1])",
        "[[1]]\n[1] \"a\"\n\n[[2]]\n[1]\n\n",
    ),
    ("is.Array(List.single([1, 2]))", "[1] FALSE\n"),
    // Empty Arrays deparse to text that rebuilds them with their item type.
    (
        "deparse(as.Array(character(0)))",
        "[1] \"as.Array(character(0))\"\n",
    ),
    ("deparse(xs[0])", "[1] \"as.Array(numeric(0))\"\n"),
];

/// Each case's code and a fragment of the error it raises.
const ERRORS: &[(&str, &str)] = &[
    (
        "[1, \"a\"]",
        "Array items must all have one type, but item 1 is double and item 2 is character",
    ),
    ("[1, 2L]", "item 1 is double and item 2 is integer"),
    ("[1, NA]", "an Array cannot hold NA"),
    (
        "[c(1, 2)]",
        "an Array item must be a single value, not a double vector of length 2",
    ),
    ("[NULL]", "not NULL"),
    ("[list(1)]", "not a list"),
    (
        "as.Array(c(1, NA))",
        "an Array cannot hold NA, but element 2 is NA",
    ),
    (
        "as.Array(list(1, 2))",
        "element 1 is a double vector of length 1",
    ),
    (
        "m <- 1; class(m) <- \"money\"; as.Array(m)",
        "has class \"money\"",
    ),
    (
        "xs + c(1, 2)",
        "combines an Array only with another Array or a single value",
    ),
    (
        "xs + [1, 2]",
        "needs Arrays of the same length, but they have 3 and 2 items",
    ),
    ("xs + ns", "one is double and the other is integer"),
    ("xs + 1L", "one is double and the other is integer"),
    (
        "words + \"b\"",
        "`+` is not defined for an Array of character items",
    ),
    (
        "flags & TRUE & xs",
        "one is logical and the other is double",
    ),
    (
        "points == points",
        "`==` is not defined for an Array of ry value items",
    ),
    ("[2000000000L] * 2L", "produced a missing value at item 1"),
    ("[0] / [0]", "produced a missing value"),
    (
        "xs[[4]]",
        "position 4 is out of bounds for an Array of 3 items",
    ),
    ("xs[NA_integer_]", "position NA is out of bounds"),
    ("xs[c(-1, 2)]", "cannot mix positive and negative positions"),
    ("xs[[0]]", "takes one whole-number position"),
    (
        "xs[\"a\"]",
        "an Array is indexed by positions or a logical mask, not a character vector",
    ),
    (
        "xs[c(TRUE, FALSE)]",
        "a logical mask needs one entry per item",
    ),
    ("xs[1, 2]", "an Array has one dimension"),
    ("xs[[1:2]]", "takes one whole-number position"),
    (
        "ys <- xs; ys[1:2] <- [1, 2, 3]",
        "assigns 2 positions but the replacement Array has 3 items",
    ),
    ("ys <- xs; ys[[5]] <- 1", "position 5 is out of bounds"),
    (
        "total(c(1, 2))",
        "argument `x` must be `Array<double>`, not `double`",
    ),
    (
        "total(words)",
        "argument `x` must be `Array<double>`, not `Array<character>`",
    ),
    (
        "List.map(xs, function(x) c(x, x))",
        "`List.map()` received an Array, so its result must be one: an Array item must be a single value",
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
fn arrays_at_runtime() {
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
                    if let Err(failure) = print::print_value_top(value, values::global_env()) {
                        failures.push(format!("`{code}`: printing failed: {}", message(failure)));
                        continue;
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
        failures.extend(round_trips());
        assert!(
            failures.is_empty(),
            "{} cases failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Properties of the conversions, over generated vectors of every item type: `as.Array` then
/// `as.vector` gives back an identical R vector, `as.Array` of that vector gives back an identical
/// Array, and evaluating an Array's deparse rebuilds it.
unsafe fn round_trips() -> Vec<String> {
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let mut failures = Vec::new();
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..400 {
            let count = (next() % 12) as usize + 1;
            let items: Vec<String> = (0..count)
                .map(|_| {
                    let number = next();
                    match case % 4 {
                        0 => {
                            if number % 2 == 0 {
                                "TRUE".to_owned()
                            } else {
                                "FALSE".to_owned()
                            }
                        }
                        1 => format!("{}L", (number % 2_000_001) as i64 - 1_000_000),
                        2 => format!("{}", ((number % 20_001) as f64 - 10_000.0) / 8.0),
                        _ => format!("\"s{}\"", number % 1000),
                    }
                })
                .collect();
            let vector = format!("c({})", items.join(", "));
            let code = format!(
                "v <- {vector}; a <- as.Array(v); \
             identical(as.vector(a), v) && is.Array(a) && identical(as.Array(as.vector(a)), a) && \
             identical(a, [{items}])",
                items = items.join(", ")
            );
            match evaluate(&code) {
                Ok(value) if values::as_logical_scalar(value) == 1 => {}
                Ok(_) => failures.push(format!("round trip failed for {vector}")),
                Err(failure) => failures.push(format!("round trip of {vector} raised: {failure}")),
            }
            let deparsed = match evaluate("deparse(a)") {
                Ok(value) => (0..values::length(value))
                    .filter_map(|index| {
                        values::string_at(value, index).map(|line| line.into_owned())
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(failure) => {
                    failures.push(format!("deparse of {vector} raised: {failure}"));
                    continue;
                }
            };
            match evaluate(&format!("identical({deparsed}, a)")) {
                Ok(value) if values::as_logical_scalar(value) == 1 => {}
                Ok(_) => failures.push(format!("deparse round trip failed: {deparsed}")),
                Err(failure) => failures.push(format!(
                    "deparse round trip of {deparsed} raised: {failure}"
                )),
            }
        }
        failures
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
