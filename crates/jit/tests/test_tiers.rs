//! Differential and fuzz coverage between tiers: every random program must give tier 0 and the
//! JIT the same value or error. The programs cover the JIT's specialized shapes (scalar and
//! vector loops, element reads and writes, branches, `break` and `next`, integer overflow, `NA`)
//! and its slow paths (defaults that read or assign the body's variables, failed speculation).
//! The JIT runs with `Mode::Force`, which compiles a body on its second call and a loop on its
//! first backward jump, so both entry and on-stack replacement are exercised.
//!
//! `FUZZ_ITERS` raises the budget.

use {
    heap::{Heap, object::Sexp},
    interp::{deparse, eval, lower, unwind::Unwind, values},
    jit::Mode,
    std::sync::Once,
    syntax::testing::{SplitMix64, fuzz_iterations},
};

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

const ARGUMENTS: &[&str] = &[
    "3L",
    "-2.5",
    "NA",
    "TRUE",
    "2147483647L",
    "0",
    "NA_integer_",
    "7",
    "-1L",
    "4.25",
    "FALSE",
    "10L",
];
const VECTORS: &[&str] = &[
    "c(1, 2.5, -3, 4)",
    "1:6",
    "c(TRUE, NA, FALSE)",
    "c(0.5, NA, 2)",
    "numeric(0)",
    "c(-2L, 5L, NA)",
];

#[test]
fn fuzz_jit_agrees_with_tier_zero() {
    with_interpreter(|| {
        let mut rng = SplitMix64(0x7135_0f75);
        let iterations = fuzz_iterations(200);
        let mut errors = 0;
        for case in 0..iterations {
            let body = program(&mut rng);
            let call = format!(
                "(a = {}, b = {}, v = {})",
                ARGUMENTS[rng.below(ARGUMENTS.len())],
                ARGUMENTS[rng.below(ARGUMENTS.len())],
                VECTORS[rng.below(VECTORS.len())]
            );
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe { outcomes(case, &body, &call) };
            errors += usize::from(reference == "error");
            for (run, outcome) in compiled.iter().enumerate() {
                assert_eq!(
                    &reference,
                    outcome,
                    "case {case}, call {}: f{call} with body\n{body}",
                    run + 1
                );
            }
        }
        // Programs that fail early compare nothing, so most must run to a value.
        eprintln!("{errors} of {iterations} programs failed");
        assert!(
            errors * 2 < iterations,
            "{errors} of {iterations} programs failed"
        );
    });
}

#[test]
fn seeds_agree_across_tiers() {
    // Each seed says whether tier 0 fails on it, since a seed that fails by accident (a base R
    // closure the test interpreter lacks, say) compares nothing.
    let seeds = [
        // A default that reads a variable the loop made dirty.
        (
            "total <- 0L\nfor (i in 1:n) total <- total + i\nk",
            "(n = 10L)",
            false,
        ),
        // A default that assigns a variable the body caches.
        ("s <- 1\nfor (i in 1:3) s <- s + 1\nd\ns", "(n = 1L)", false),
        // Integer overflow becomes NA, in the loop and after it.
        (
            "x <- 2147483600L\nfor (i in 1:n) x <- x + 10L\nx",
            "(n = 10L)",
            false,
        ),
        // A vector that turns from double to list mid-loop, which fails a speculation.
        (
            "out <- rep(0, n)\nfor (i in 1:n) { if (i == 4L) out <- c(list(), out); out[[i]] <- i * 2 }\nout",
            "(n = 6L)",
            false,
        ),
        // Nested loops with break and next.
        (
            "s <- 0\nfor (i in 1:n) { for (j in 1:i) { if (j == 3L) next; if (s > 40) break; s <- s + j } }\ns",
            "(n = 9L)",
            false,
        ),
        // A loop condition that becomes NA, which is an error.
        (
            "x <- 1\nwhile (x < 10) { x <- x * 2; if (x > 6) x <- NA }\nx",
            "(n = 1L)",
            true,
        ),
        // Growing a vector past its end, which tier 0 does with the frame written back.
        (
            "x <- c()\nfor (i in 1:n) x[i] <- i * 2\nx",
            "(n = 5L)",
            false,
        ),
        // A `next` inside `switch`, which leaves compiled code from a nested evaluation.
        (
            "s <- 0\nfor (i in 1:n) { s <- s + 1; if (i %% 2 == 0) switch(\"a\", a = next) }\ns",
            "(n = 3000L)",
            false,
        ),
        // Subsetting a plain vector, which reaches the element intrinsic.
        (
            "s <- 0\nfor (i in 1:n) s <- s + v[(i %% 2L) + 1L] * v[[1L]]\ns",
            "(n = 50L)",
            false,
        ),
        // A subject that gains a class mid-loop dispatches, and the method sees its index
        // unevaluated.
        (
            "`[.tagged` <- function(x, i) substitute(i)\nx <- 1:3\nout <- list()\nfor (j in 1:n) { if (j == 3L) class(x) <- \"tagged\"; out[[j]] <- x[j + 1L] }\nout",
            "(n = 4L)",
            false,
        ),
        // A missing formal as the index selects everything.
        (
            "g <- function(i) v[i]\nfor (j in 1:n) r <- g()\nr",
            "(n = 3L)",
            false,
        ),
        // A formal that is only called shadows a function of the same name in the enclosure.
        (
            "h <- function(x) -1\ng <- function(h) { s <- 0; for (i in 1:n) s <- s + h(i); s }\nc(g(function(x) x * 2), g(function(x) x * 3))",
            "(n = 4L)",
            false,
        ),
    ];
    with_interpreter(|| {
        for (case, (body, call, fails)) in seeds.iter().enumerate() {
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe { outcomes(1_000_000 + case, body, call) };
            assert_eq!(reference == "error", *fails, "seed {case} gave {reference}");
            for (run, outcome) in compiled.iter().enumerate() {
                assert_eq!(
                    &reference,
                    outcome,
                    "seed {case}, call {}: f{call} with body\n{body}",
                    run + 1
                );
            }
        }
    });
}

/// A speculation that fails after a load forced its promise must not make tier 0 load the
/// variable again: the forced promise would read as visible, though forcing `n <- 0` left the
/// value invisible. The integer call makes the compiled code speculate on an integer `x`.
#[test]
fn a_failed_speculation_after_forcing_keeps_the_value_invisible() {
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        unsafe {
            jit::install_mode(Mode::Force);
            run("visibility <- function(x) .Internal(withVisible(x))[[2L]]");
            let outcomes: Vec<String> = ["n <- 0", "3L", "3L", "n <- 0", "3L", "n <- 0"]
                .iter()
                .map(|argument| run(&format!("visibility({argument})")))
                .collect();
            jit::install_mode(Mode::Off);
            assert_eq!(
                outcomes,
                ["FALSE", "TRUE", "TRUE", "FALSE", "TRUE", "FALSE"]
            );
        }
    });
}

/// The outcome of four calls of a closure with `body` under tier 0, and of four calls of a
/// fresh closure with the same body under the forced JIT. Each closure is new, so the JIT's run
/// starts from its own profile and compiles from the second call on.
unsafe fn outcomes(case: usize, body: &str, call: &str) -> (String, Vec<String>) {
    unsafe {
        let formals = "a = 1L, b = 2, v = c(1, 2), n = 5L, k = total * 2L, d = (s <- 100)";
        let define = |name: &str| run(&format!("{name} <- function({formals}) {{\n{body}\n}}"));
        let calls = |name: &str| {
            (0..4)
                .map(|_| run(&format!("{name}{call}")))
                .collect::<Vec<_>>()
        };
        jit::install_mode(Mode::Off);
        let reference_name = format!("reference_{case}");
        define(&reference_name);
        let reference = calls(&reference_name);
        jit::install_mode(Mode::Force);
        let compiled_name = format!("compiled_{case}");
        define(&compiled_name);
        let compiled = calls(&compiled_name);
        jit::install_mode(Mode::Off);
        for outcome in &reference[1..] {
            assert_eq!(
                &reference[0], outcome,
                "tier 0 is not deterministic on case {case}:\n{body}"
            );
        }
        (reference[0].clone(), compiled)
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
            Ok(value) => {
                deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n")
            }
            Err(_) => "error".to_owned(),
        };
        interp::interp().frames.clear();
        interp::heap().release_handles(mark);
        rendered
    }
}

/// A random body over the formals `a`, `b`, `v`, `n`, `k` and `d`: a setup, one or two loops or
/// branches, and a final expression.
fn program(rng: &mut SplitMix64) -> String {
    let mut statements = vec![
        "total <- 0L".to_owned(),
        "i <- 1L".to_owned(),
        "s <- 1L".to_owned(),
        format!("s <- {}", expression(rng, 2)),
    ];
    for _ in 0..rng.below(2) + 1 {
        statements.push(match rng.below(7) {
            0 => format!(
                "for (i in 1:{}) {{ s <- s + {}; if ({}) next; if ({}) break }}",
                rng.below(6) + 1,
                expression(rng, 2),
                condition(rng),
                condition(rng)
            ),
            1 => format!(
                "t <- 0L\nwhile (t < {}L) {{ t <- t + 1L; s <- {} }}",
                rng.below(5),
                expression(rng, 2)
            ),
            2 => "for (x in v) s <- s + x".to_owned(),
            3 => format!("for (i in seq_along(v)) v[i] <- v[i] * {}", leaf(rng)),
            4 => format!(
                "w <- rep(0, n)\nfor (i in 1:n) w[i] <- i + {}\ns <- s + w[n]",
                leaf(rng)
            ),
            5 => format!(
                "for (i in 1:n) total <- total + {}\ns <- {}",
                ["i", "1L", "2147483000L"][rng.below(3)],
                ["k", "s", "d"][rng.below(3)]
            ),
            _ => format!(
                "s <- if ({}) {} else {}",
                condition(rng),
                expression(rng, 2),
                expression(rng, 2)
            ),
        });
    }
    statements.push(["s", "v", "list(s, v, total)", "c(s, total)"][rng.below(4)].to_owned());
    statements.join("\n")
}

fn expression(rng: &mut SplitMix64, depth: usize) -> String {
    if depth == 0 || rng.below(3) == 0 {
        return leaf(rng);
    }
    match rng.below(6) {
        0 => format!("-({})", expression(rng, depth - 1)),
        1 => format!(
            "(if ({}) {} else {})",
            condition(rng),
            expression(rng, depth - 1),
            expression(rng, depth - 1)
        ),
        2 => format!("v[{}]", ["1L", "2", "i", "length(v)"][rng.below(4)]),
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
        // `NA` in a condition is an error, which ends the program before it compares anything,
        // so conditions avoid literal `NA` and leave it to the arguments.
        let leaf = |rng: &mut SplitMix64| loop {
            let leaf = leaf(rng);
            if leaf != "NA" {
                break leaf;
            }
        };
        format!(
            "{} {} {}",
            leaf(rng),
            operators[rng.below(operators.len())],
            leaf(rng)
        )
    };
    match rng.below(4) {
        0 => format!("{} && {}", comparison(rng), comparison(rng)),
        1 => format!("{} || {}", comparison(rng), comparison(rng)),
        _ => comparison(rng),
    }
}

fn leaf(rng: &mut SplitMix64) -> String {
    let leaves = [
        "a", "b", "s", "i", "total", "1L", "2", "0.5", "NA", "TRUE", "-1L", "1e308", "0L", "3L",
    ];
    leaves[rng.below(leaves.len())].to_owned()
}
