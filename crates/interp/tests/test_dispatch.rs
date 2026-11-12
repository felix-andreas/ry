//! S3 method rows across the wrap of a symbol's 32-bit stamp share. The wrap needs four billion
//! definitions of one name, so the test raises the share to just below it and then defines and
//! removes a method: each stamp must keep growing, and the row resolved before the wrap must go
//! stale when the method is removed after it.
//!
//! The interpreter runs here without base R, so the setup calls the internals directly.

use {
    heap::object::Sexp,
    interp::{eval, lower, testing::with_interpreter, unwind::Unwind, values},
};

const SETUP: &str = r#"
gen <- function(x) UseMethod("gen")
gen.default <- function(x) "default"
obj <- 1
class(obj) <- "w"
"#;

#[test]
fn a_method_row_goes_stale_across_a_stamp_wrap() {
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every handle comes from its heap.
        unsafe {
            evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
            let method =
                values::install("gen.w").unwrap_or_else(|failure| panic!("{}", message(failure)));
            let other = values::install("unrelated")
                .unwrap_or_else(|failure| panic!("{}", message(failure)));
            interp::heap().raise_symbol_epoch(method, u32::MAX - 1);
            let before = interp::heap().lookup_stamp(method);
            let other_before = interp::heap().lookup_stamp(other);
            assert_eq!(call(), "default");
            evaluate(r#"gen.w <- function(x) "method""#)
                .unwrap_or_else(|failure| panic!("{failure}"));
            let at_max = interp::heap().lookup_stamp(method);
            assert!(at_max > before);
            assert_eq!(call(), "method");
            assert_eq!(call(), "method");
            evaluate(r#".Internal(remove("gen.w", globalenv(), FALSE))"#)
                .unwrap_or_else(|failure| panic!("{failure}"));
            let wrapped = interp::heap().lookup_stamp(method);
            assert!(wrapped > at_max);
            // The carry moved the heap's share, so every other symbol's stamp moved too.
            assert!(interp::heap().lookup_stamp(other) >= other_before + (1 << 32));
            assert_eq!(call(), "default");
            evaluate(r#"gen.w <- function(x) "again""#)
                .unwrap_or_else(|failure| panic!("{failure}"));
            assert!(interp::heap().lookup_stamp(method) > wrapped);
            assert_eq!(call(), "again");
        }
    });
}

/// Dispatches `gen(obj)` and returns the one string it gives.
unsafe fn call() -> String {
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let value = evaluate("gen(obj)").unwrap_or_else(|failure| panic!("{failure}"));
        string(value)
    }
}

unsafe fn string(value: Sexp) -> String {
    // SAFETY: `value` is a live character vector from the interpreter's heap.
    unsafe { String::from_utf8_lossy(value.element(0).bytes()).into_owned() }
}

/// Evaluates `text` in the global environment and returns the last value, or the error message.
unsafe fn evaluate(text: &str) -> Result<Sexp, String> {
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let expressions = lower::parse_text(text, "<test>").map_err(message)?;
        let mut value = values::nil();
        for index in 0..values::length(expressions) {
            interp::interp().error_message.clear();
            value = eval::eval(expressions.element(index), values::global_env()).map_err(
                |failure| format!("{}{}", message(failure), interp::interp().error_message),
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
