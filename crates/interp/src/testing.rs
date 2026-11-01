//! The interpreter's fuzz contracts, shared by the in-tree harness (`tests/test_fuzz.rs`) and the
//! coverage-guided `fuzz/` targets so both check exactly the same properties. Each one runs
//! without base R or libR, on an interpreter it installs on first use.

use crate::{deparse, eval, lower, serialize, unwind::Unwind, values};
use heap::{Heap, object::Sexp};
use std::sync::{Mutex, Once};
use syntax::testing::Entropy;

/// Runs `body` with the interpreter installed. The interpreter is single-threaded, so callers on
/// different threads take turns.
pub fn with_interpreter<T>(body: impl FnOnce() -> T) -> T {
    static INSTALL: Once = Once::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    INSTALL.call_once(|| {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        // SAFETY: the heap lives for the rest of the process, and the lock above keeps every
        // use of the interpreter on one thread at a time.
        unsafe {
            crate::install(heap, crate::default_hooks());
            crate::interp().show_error_messages = false;
        }
    });
    body()
}

/// Lowering any text must not panic: it either fails with a parse error or produces expressions,
/// and every expression it produces must deparse without panicking.
pub fn check_lowering_input(text: &str) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            if let Ok(expressions) = lower::parse_text(text, "<fuzz>") {
                for index in 0..values::length(expressions) {
                    deparse::deparse_lines(
                        expressions.element(index),
                        60,
                        deparse::options::KEEP_INTEGER,
                    );
                }
            }
        }
        crate::heap().release_handles(mark);
    });
}

/// Reading any byte stream as a serialized object must fail with an R error or succeed, never
/// panic.
pub fn check_unserialize_input(bytes: &[u8]) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            drop(serialize::unserialize(bytes, values::nil()));
        }
        crate::interp().frames.clear();
        crate::heap().release_handles(mark);
    });
}

/// Generates a program over `a` and `b` and checks that the tier-0 bytecode agrees with the AST
/// evaluator on it: the program's value evaluated directly must equal its value as a closure body
/// on each of the first three calls, the second and later of which run compiled bytecode.
pub fn check_bytecode_agreement(entropy: &mut impl Entropy) {
    let body = program(entropy);
    let arguments = [
        "3L",
        "-2.5",
        "NA",
        "TRUE",
        "2147483647L",
        "0",
        "NA_integer_",
    ];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        let (direct, compiled) = unsafe { outcomes(&body, a, b) };
        for (call, outcome) in compiled.iter().enumerate() {
            assert_eq!(
                &direct,
                outcome,
                "call {}: `{body}` with a = {a}, b = {b}",
                call + 1
            );
        }
    });
}

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
        let mark = crate::heap().handle_mark();
        let outcome = (|| -> Result<Sexp, Unwind> {
            let expressions = lower::parse_text(text, "<fuzz>")?;
            let mut value = values::nil();
            for index in 0..values::length(expressions) {
                value = eval::eval(expressions.element(index), values::global_env())?;
            }
            Ok(value)
        })();
        let rendered = match outcome {
            Ok(value) => {
                deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n")
            }
            Err(_) => "error".to_owned(),
        };
        crate::interp().frames.clear();
        crate::heap().release_handles(mark);
        rendered
    }
}

/// Assignments, a loop, and a final expression.
fn program(entropy: &mut impl Entropy) -> String {
    let mut statements = vec![format!("s <- {}", expression(entropy, 2))];
    match entropy.below(4) {
        0 => statements.push(format!(
            "for (i in 1:{}) {{ s <- s + {}; if ({}) next; if ({}) break }}",
            entropy.below(5) + 1,
            expression(entropy, 2),
            condition(entropy),
            condition(entropy)
        )),
        1 => statements.push(format!(
            "k <- 0L\nwhile (k < {}L) {{ k <- k + 1L; s <- {} }}",
            entropy.below(4),
            expression(entropy, 2)
        )),
        2 => statements.push(format!(
            "s <- if ({}) {} else {}",
            condition(entropy),
            expression(entropy, 2),
            expression(entropy, 2)
        )),
        _ => {}
    }
    statements.push(expression(entropy, 3));
    statements.join("\n")
}

fn expression(entropy: &mut impl Entropy, depth: usize) -> String {
    if depth == 0 || entropy.below(3) == 0 {
        return leaf(entropy);
    }
    match entropy.below(5) {
        0 => format!("-({})", expression(entropy, depth - 1)),
        1 => format!(
            "(if ({}) {} else {})",
            condition(entropy),
            expression(entropy, depth - 1),
            expression(entropy, depth - 1)
        ),
        _ => {
            let operators = ["+", "-", "*", "/", "^", "%/%", "%%", "==", "<", ">=", "!="];
            format!(
                "({} {} {})",
                expression(entropy, depth - 1),
                operators[entropy.below(operators.len())],
                expression(entropy, depth - 1)
            )
        }
    }
}

fn condition(entropy: &mut impl Entropy) -> String {
    match entropy.below(4) {
        0 => format!("{} && {}", comparison(entropy), comparison(entropy)),
        1 => format!("{} || {}", comparison(entropy), comparison(entropy)),
        _ => comparison(entropy),
    }
}

fn comparison(entropy: &mut impl Entropy) -> String {
    let operators = ["<", ">", "==", "!=", "<=", ">="];
    format!(
        "{} {} {}",
        leaf(entropy),
        operators[entropy.below(operators.len())],
        leaf(entropy)
    )
}

const LEAVES: [&str; 12] = [
    "a", "b", "s", "1L", "2", "0.5", "NA", "TRUE", "-1L", "1e308", "0L", "3L",
];

fn leaf(entropy: &mut impl Entropy) -> String {
    LEAVES[entropy.below(LEAVES.len())].to_owned()
}
