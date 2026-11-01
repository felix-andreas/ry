//! Fuzz and property coverage for the interpreter's stages, run without base R or libR:
//!
//! - lowering never panics on mutated R source, and a lowered expression deparses to text that
//!   lowers back to an identical expression;
//! - the tier-0 bytecode agrees with the AST evaluator on random arithmetic, comparison, logic,
//!   branch and loop programs, including NA, integer overflow and errors.
//!
//! `FUZZ_ITERS` raises the budget.

use heap::{Heap, object::Sexp};
use interp::{deparse, eval, lower, unwind::Unwind, values};
use std::sync::Once;
use syntax::testing::{SplitMix64, fuzz_iterations, mutate_bytes};

static INSTALL: Once = Once::new();

fn install() {
    INSTALL.call_once(|| {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        // SAFETY: the heap lives for the rest of the process, and the tests in this file run on
        // one thread (see `with_interpreter`).
        unsafe {
            interp::install(heap, interp::default_hooks());
            interp::interp().show_error_messages = false;
        }
    });
}

/// The interpreter is single-threaded, so the tests take turns.
fn with_interpreter<T>(body: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    install();
    body()
}

const SEEDS: &[&str] = &[
    "f <- function(x, y = 2) { if (x > y) x - y else y * 2 }",
    "for (i in 1:10) { s <- s + i^2; if (s > 50) break }",
    "x[[\"a\"]] <- list(b = 1L, c = NA_character_)",
    "while (TRUE) { n <- n %/% 2L; if (n == 0) break }",
    "g <- function(...) ..1 + length(list(...))",
    "y <- -3:3 * 1e-3 + 0x1Fp2",
    "`my var` <- c(a = TRUE, b = FALSE) && !is.na(z)",
    "repeat { k <- k + 1; next }",
    "h <- \\(a) a |> sum(na.rm = TRUE)",
    "if (a) b else if (c) d else { e; f }",
];

#[test]
fn fuzz_lowering_never_panics() {
    with_interpreter(|| {
        let mut rng = SplitMix64(0x5eed_1a7e);
        for _ in 0..fuzz_iterations(400) {
            let seed = SEEDS[rng.below(SEEDS.len())];
            let mutated = String::from_utf8_lossy(&mutate_bytes(&mut rng, seed)).into_owned();
            let mark = interp::heap().handle_mark();
            // A parse error is a fine outcome; only a panic fails the test.
            // SAFETY: the interpreter is installed.
            drop(unsafe { lower::parse_text(&mutated, "<fuzz>") });
            interp::heap().release_handles(mark);
        }
    });
}

#[test]
fn deparse_round_trips_lowered_seeds() {
    with_interpreter(|| unsafe {
        for seed in SEEDS {
            let Ok(expressions) = lower::parse_text(seed, "<seed>") else {
                panic!("seed does not parse: {seed}");
            };
            for index in 0..values::length(expressions) {
                let expression = expressions.element(index);
                let text = deparse::deparse_lines(expression, 60, deparse::options::KEEP_INTEGER).join("\n");
                let Ok(again) = lower::parse_text(&text, "<deparsed>") else {
                    panic!("{seed}: deparsed text does not parse: {text}");
                };
                let again = again.element(0);
                let text_again = deparse::deparse_lines(again, 60, deparse::options::KEEP_INTEGER).join("\n");
                assert_eq!(text, text_again, "{seed}: deparse is not stable");
            }
        }
    });
}

#[test]
fn fuzz_bytecode_agrees_with_the_ast_evaluator() {
    with_interpreter(|| {
        let mut rng = SplitMix64(0xb17e_c0de);
        for case in 0..fuzz_iterations(300) {
            let body = program(&mut rng);
            let arguments = ["3L", "-2.5", "NA", "TRUE", "2147483647L", "0", "NA_integer_"];
            let a = arguments[rng.below(arguments.len())];
            let b = arguments[rng.below(arguments.len())];
            // SAFETY: the interpreter is installed.
            let (direct, compiled) = unsafe { outcomes(&body, a, b) };
            for (call, outcome) in compiled.iter().enumerate() {
                assert_eq!(
                    &direct, outcome,
                    "case {case}, call {}: `{body}` with a = {a}, b = {b}",
                    call + 1
                );
            }
        }
    });
}

/// The program's value evaluated directly by the AST evaluator, and as a closure body on its
/// first three calls (the second and later run compiled bytecode).
unsafe fn outcomes(body: &str, a: &str, b: &str) -> (String, Vec<String>) {
    unsafe {
        // The direct run assigns its locals globally, which the closure would then read, so
        // every run starts from the same globals.
        let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
        run(&reset);
        let direct = run(&format!("local_value <- {{ {body} }}\nlocal_value"));
        run(&format!("f <- function(a, b) {{ {body} }}"));
        let compiled = (0..3)
            .map(|_| {
                run(&reset);
                run(&format!("f({a}, {b})"))
            })
            .collect();
        (direct, compiled)
    }
}

/// The deparsed value of the last expression of `text` in the global environment, or `error`.
unsafe fn run(text: &str) -> String {
    unsafe {
        let mark = interp::heap().handle_mark();
        let outcome = (|| -> Result<Sexp, Unwind> {
            let expressions = lower::parse_text(text, "<fuzz>")?;
            let mut value = values::nil();
            for index in 0..values::length(expressions) {
                value = eval::eval(expressions.element(index), values::global_env())?;
            }
            Ok(value)
        })();
        let rendered = match outcome {
            Ok(value) => deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n"),
            Err(_) => "error".to_owned(),
        };
        interp::interp().frames.clear();
        interp::heap().release_handles(mark);
        rendered
    }
}

/// A random program over `a` and `b`: assignments, a loop, and a final expression.
fn program(rng: &mut SplitMix64) -> String {
    let mut statements = vec![format!("s <- {}", expression(rng, 2))];
    match rng.below(4) {
        0 => statements.push(format!(
            "for (i in 1:{}) {{ s <- s + {}; if ({}) next; if ({}) break }}",
            rng.below(5) + 1,
            expression(rng, 2),
            condition(rng),
            condition(rng)
        )),
        1 => statements.push(format!(
            "k <- 0L\nwhile (k < {}L) {{ k <- k + 1L; s <- {} }}",
            rng.below(4),
            expression(rng, 2)
        )),
        2 => statements.push(format!("s <- if ({}) {} else {}", condition(rng), expression(rng, 2), expression(rng, 2))),
        _ => {}
    }
    statements.push(expression(rng, 3));
    statements.join("\n")
}

fn expression(rng: &mut SplitMix64, depth: usize) -> String {
    if depth == 0 || rng.below(3) == 0 {
        return leaf(rng);
    }
    match rng.below(5) {
        0 => format!("-({})", expression(rng, depth - 1)),
        1 => format!("(if ({}) {} else {})", condition(rng), expression(rng, depth - 1), expression(rng, depth - 1)),
        _ => {
            let operators = ["+", "-", "*", "/", "^", "%/%", "%%", "==", "<", ">=", "!="];
            format!(
                "({} {} {})",
                expression(rng, depth - 1),
                operators[rng.below(operators.len())],
                expression(rng, depth - 1)
            )
        }
    }
}

fn condition(rng: &mut SplitMix64) -> String {
    let comparison = |rng: &mut SplitMix64| {
        let operators = ["<", ">", "==", "!=", "<=", ">="];
        format!("{} {} {}", leaf(rng), operators[rng.below(operators.len())], leaf(rng))
    };
    match rng.below(4) {
        0 => format!("{} && {}", comparison(rng), comparison(rng)),
        1 => format!("{} || {}", comparison(rng), comparison(rng)),
        _ => comparison(rng),
    }
}

fn leaf(rng: &mut SplitMix64) -> String {
    let leaves = ["a", "b", "s", "1L", "2", "0.5", "NA", "TRUE", "-1L", "1e308", "0L", "3L"];
    leaves[rng.below(leaves.len())].to_owned()
}
