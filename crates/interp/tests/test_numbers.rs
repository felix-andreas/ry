//! ry's native numbers in the interpreter: arithmetic and comparison in each type, overflow and
//! conversion errors, the standard library's conversions and overflow behaviors, the R boundary,
//! printing as the literal that rebuilds the value, and numbers as dictionary keys and set
//! elements. The interpreter is global to the process, so everything runs as one test.
//!
//! The interpreter runs here without base R, so the setup defines the few generics base R would
//! and wrappers for the internals the cases call.

use {
    heap::{Heap, object::Sexp},
    interp::{
        eval, lower,
        numbers::{self, NumberType},
        print,
        unwind::Unwind,
        values,
    },
    std::cell::RefCell,
};

const SETUP: &str = r#"
print <- function(x, ...) UseMethod("print")
print.default <- function(x, ...) .Internal(print.default(x, NULL, NULL))
format <- function(x, ...) UseMethod("format")
all.equal <- function(target, current, ...) UseMethod("all.equal")
identical <- function(x, y) .Internal(identical(x, y, TRUE, TRUE, TRUE, TRUE, FALSE, TRUE, FALSE))
deparse <- function(expr) .Internal(deparse(expr, 60L, TRUE, 1089L, -1L))
typeof <- function(x) .Internal(typeof(x))
gc <- function() invisible(.Internal(gc(FALSE, FALSE, TRUE)))
n <- Num.to_i64(5)
"#;

/// Each case's code and what printing its value shows.
const PRINTS: &[(&str, &str)] = &[
    // Printing shows the literal that rebuilds the value.
    ("n", "5i64\n"),
    ("Num.to_u8(255)", "255u8\n"),
    ("Num.to_i8(-128)", "-128i8\n"),
    ("Num.to_dec(1.5)", "1.5dec\n"),
    ("Num.to_dec(0.1)", "0.1dec\n"),
    ("Num.to_f32(0.5)", "0.5f32\n"),
    (
        "Num.sub_wrap(Num.to_u128(0), 1)",
        "340282366920938463463374607431768211455u128\n",
    ),
    ("format(n)", "[1] \"5i64\"\n"),
    ("deparse(Num.to_dec(2))", "[1] \"2dec\"\n"),
    ("list(n, Num.to_u8(1))", "[[1]]\n5i64\n\n[[2]]\n1u8\n\n"),
    (
        "(id = n, amount = Num.to_dec(9.99))",
        "(amount = 9.99dec, id = 5i64)\n",
    ),
    // typeof and class.
    ("typeof(n)", "[1] \"I64\"\n"),
    ("typeof(Num.to_dec(1))", "[1] \"Dec\"\n"),
    ("class(n)", "[1] \"ry_number\"\n"),
    ("length(n)", "[1] 1\n"),
    // Arithmetic keeps the type, and R numbers convert exactly.
    ("n + n", "10i64\n"),
    ("n + 1", "6i64\n"),
    ("1L - n", "-4i64\n"),
    ("n * 3", "15i64\n"),
    ("-n", "-5i64\n"),
    ("+n", "5i64\n"),
    ("n ^ 3", "125i64\n"),
    ("n ^ 0", "1i64\n"),
    // `%/%` and `%%` are floored, as in R.
    ("n %/% 2", "2i64\n"),
    ("-n %/% 2", "-3i64\n"),
    ("-n %% 3", "1i64\n"),
    ("n %% -3", "-1i64\n"),
    ("Num.to_u8(200) + 55", "255u8\n"),
    // Dec is fixed point with 18 decimals.
    (
        "Num.to_dec(0.1) + Num.to_dec(0.2) == Num.to_dec(0.3)",
        "[1] TRUE\n",
    ),
    ("Num.to_dec(1) / 3", "0.333333333333333333dec\n"),
    ("Num.to_dec(1.5) * 2", "3dec\n"),
    ("Num.to_dec(-7.5) %/% 2", "-4dec\n"),
    // F32 rounds after each operation.
    ("Num.to_f32(0.1) + Num.to_f32(0.2)", "0.3f32\n"),
    ("Num.to_f32(1) / 3", "0.33333334f32\n"),
    ("Num.to_f64(Num.to_f32(0.1))", "[1] 0.1\n"),
    // Comparisons give R logicals.
    ("n == 5", "[1] TRUE\n"),
    ("n != 5", "[1] FALSE\n"),
    ("n < 6", "[1] TRUE\n"),
    ("n >= n", "[1] TRUE\n"),
    ("Num.to_u8(3) > 4", "[1] FALSE\n"),
    // Conversions and their checked forms.
    ("Num.to_i64_checked(5)", ":Ok(5i64)\n"),
    ("Num.to_u8_checked(256)", ":Err(:OutOfBounds)\n"),
    ("Num.to_i8_checked(-1.5)", ":Err(:OutOfBounds)\n"),
    ("Num.to_i32(Num.to_u8(7))", "7i32\n"),
    ("Num.to_f64(n)", "[1] 5\n"),
    ("Num.to_dec(n)", "5dec\n"),
    // Overflow behaviors.
    ("Num.add_wrap(Num.to_u8(250), 10)", "4u8\n"),
    ("Num.sub_wrap(Num.to_i8(-128), 1)", "127i8\n"),
    ("Num.mul_wrap(Num.to_u8(16), 16)", "0u8\n"),
    ("Num.add_saturated(Num.to_u8(250), 10)", "255u8\n"),
    ("Num.sub_saturated(Num.to_u8(3), 10)", "0u8\n"),
    ("Num.add_checked(Num.to_u8(250), 10)", ":Err(:Overflow)\n"),
    ("Num.add_checked(n, 1)", ":Ok(6i64)\n"),
    ("Num.abs(Num.to_i64(-3))", "3i64\n"),
    ("Num.min(n, 3)", "3i64\n"),
    ("Num.max(n, 3)", "5i64\n"),
    // Rounding gives I64.
    ("Num.round(2.5)", "3i64\n"),
    ("Num.round(-2.5)", "-3i64\n"),
    ("Num.floor(-1.2)", "-2i64\n"),
    ("Num.ceiling(Num.to_dec(1.2))", "2i64\n"),
    ("Num.truncate(Num.to_f32(-3.7))", "-3i64\n"),
    ("Num.to_str(n)", "[1] \"5\"\n"),
    ("Num.to_str(Num.to_dec(1.25))", "[1] \"1.25\"\n"),
    ("Num.max_i64", "9223372036854775807i64\n"),
    // The R boundary.
    ("Num.from_r(3L)", "3i32\n"),
    ("Num.from_r(2.5)", "[1] 2.5\n"),
    ("Num.to_r(n)", "[1] 5\n"),
    ("typeof(Num.to_r(n))", "[1] \"integer\"\n"),
    ("Num.to_r(Num.to_i64(4294967296))", "[1] 4294967296\n"),
    ("Bool.from_r(TRUE)", "[1] TRUE\n"),
    ("Str.from_r(\"a\")", "[1] \"a\"\n"),
    ("List.from_r(c(1L, 2L))", "[1i32, 2i32]\n"),
    ("List.from_r(as.raw(255))", "[255u8]\n"),
    ("List.to_r(List.from_r(c(1L, 2L)))", "[1] 1 2\n"),
    ("List.to_r(list(n, Num.to_i64(7)))", "[1] 5 7\n"),
    ("List.to_r(List.range(1, 3))", "[1] 1 2 3\n"),
    ("List.range(1, 3)", "[1i64, 2i64, 3i64]\n"),
    (
        "List.range(Num.to_u8(254), Num.to_u8(255))",
        "[254u8, 255u8]\n",
    ),
    ("List.range(3, 2)", "[]\n"),
    ("Str.to_i64(\"42\")", ":Ok(42i64)\n"),
    // Equality is structural, and numbers are keys and set elements.
    ("identical(n, Num.to_i64(5))", "[1] TRUE\n"),
    ("identical(n, Num.to_i32(5))", "[1] FALSE\n"),
    ("identical(list(n), list(Num.to_i64(5)))", "[1] TRUE\n"),
    ("identical((a = n), (a = Num.to_i64(5)))", "[1] TRUE\n"),
    ("all.equal(n, Num.to_i64(5))", "[1] TRUE\n"),
    (
        "{ d <- Dict.insert(Dict.single(n, \"five\"), Num.to_i64(6), \"six\"); Dict.get(d, Num.to_i64(5)) }",
        ":Ok(\"five\")\n",
    ),
    (
        "Dict.get(Dict.single(n, \"five\"), Num.to_i32(5))",
        "the dict's keys are I64, and this one is I32",
    ),
    (
        "{ d <- Dict.empty(); for (i in 1:50) d <- Dict.insert(d, Num.to_i64(i), i); [Dict.len(d), Result.with_default(Dict.get(d, Num.to_i64(37)), 0L)] }",
        "[50i64, 37i64]\n",
    ),
    ("Dict.keys(Dict.single(Num.to_u8(1), TRUE))", "[1u8]\n"),
    (
        "Set.contains(Set.from_list(list(n, Num.to_i64(6))), Num.to_i64(6))",
        "[1] TRUE\n",
    ),
    (
        "Set.len(Set.from_list(list(n, n, Num.to_i64(6))))",
        "2i64\n",
    ),
    (
        "{ s <- Set.from_list(List.range(1, 100)); gc(); Set.contains(s, Num.to_i64(99)) }",
        "[1] TRUE\n",
    ),
    (
        "match (n) { 5 => \"five\"\n _ => \"other\" }",
        "[1] \"five\"\n",
    ),
];

/// Each case's code and a piece of the error it raises.
const ERRORS: &[(&str, &str)] = &[
    ("n + 1.5", "1.5 does not fit I64"),
    ("Num.to_i64(1.5)", "1.5 does not fit I64"),
    ("Num.to_u8(256)", "256 does not fit U8"),
    ("Num.to_u8(-1)", "-1 does not fit U8"),
    (
        "Num.max_i64 + 1",
        "integer overflow in `+`: the result does not fit I64",
    ),
    (
        "Num.to_u8(255) + 1",
        "integer overflow in `+`: the result does not fit U8",
    ),
    ("Num.to_u8(0) - 1", "the result does not fit U8"),
    ("-Num.min_i64", "does not fit I64"),
    ("n / 2", "use `%/%`"),
    ("n %/% 0", "division by zero"),
    ("n %% 0", "division by zero"),
    ("n ^ -1", "exponent"),
    ("n + Num.to_i32(1)", "I32"),
    ("n + c(1, 2)", "I64"),
    ("n + NA", "I64"),
    ("Num.to_dec(1e30)", "does not fit Dec"),
    ("Num.to_dec(1e-19)", "does not fit Dec"),
    ("Num.add_wrap(Num.to_f32(1), 1)", "needs an integer type"),
    // Builtins that do not understand native numbers refuse them by name.
    (
        "sum(n)",
        "`n` is a ry I64, not an R object; convert it with `Num.to_r()`",
    ),
    ("c(n, 1)", "convert it with `Num.to_r()`"),
    ("if (n) 1", "`Num.to_r()`"),
    ("n$x", "I64 is a number and has no field `x`"),
    ("n[[1L]]", "I64 is a number and cannot be indexed"),
    ("Num.to_r(Num.sub_wrap(Num.to_u64(0), 1))", "past 2^53"),
    ("Bool.from_r(NA)", "`value`"),
    ("List.from_r(c(1, NA))", "`NA` at position 2"),
    ("Num.round(n)", "a fractional number"),
    ("List.range(Num.to_f32(1), 2)", "integer type"),
];

#[test]
fn native_numbers() {
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
        // The entry point the suffixed-literal lowering calls.
        for (ty, text, expected) in [
            (NumberType::I64, "5", "5i64"),
            (NumberType::U8, "0xff", "255u8"),
            (NumberType::Dec, "1.50", "1.5dec"),
            (
                NumberType::I128,
                "-170141183460469231731687303715884105728",
                "-170141183460469231731687303715884105728i128",
            ),
            (NumberType::F32, "0.1", "0.1f32"),
        ] {
            match numbers::from_text(ty, text) {
                Ok(value) => {
                    let shown = numbers::read(value).map(numbers::literal);
                    if shown.as_deref() != Some(expected) {
                        failures.push(format!("`{text}` as {}: built {shown:?}", ty.name()));
                    }
                }
                Err(failure) => failures.push(format!("`{text}`: {}", message(failure))),
            }
        }
        for (ty, text) in [
            (NumberType::U8, "256"),
            (NumberType::I8, "1.5"),
            (NumberType::Dec, "1e-19"),
        ] {
            if numbers::parse(ty, text).is_ok() {
                failures.push(format!("`{text}` as {} should not fit", ty.name()));
            }
        }
        for (code, expected) in PRINTS {
            take_output();
            let printed = match evaluate(code) {
                Ok(value) => {
                    if interp::interp().visible
                        && let Err(failure) = print::print_value_top(value, values::global_env())
                    {
                        failures.push(format!("`{code}`: printing failed: {}", message(failure)));
                        continue;
                    }
                    take_output()
                }
                Err(failure) => format!("{}\n", failure.trim_end()),
            };
            let matched =
                printed == *expected || (!expected.ends_with('\n') && printed.contains(expected));
            if !matched {
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
                    format!("no error: {}", take_output())
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

thread_local! {
    static OUTPUT: RefCell<String> = const { RefCell::new(String::new()) };
}

unsafe fn capture(text: &[u8], _is_error: bool) {
    OUTPUT.with(|output| output.borrow_mut().push_str(&String::from_utf8_lossy(text)));
}

fn take_output() -> String {
    OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()))
}
