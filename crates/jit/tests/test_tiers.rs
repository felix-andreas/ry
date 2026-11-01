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
        // An inlined typed closure keeps its checks: a call reached only after the loop compiled
        // fails on its argument, or on its value, exactly as in tier 0.
        (
            "flag <- function(a: logical) -> logical !a\ns <- 0\nfor (i in 1:n) if (i > 300L) s <- s + flag(1.5)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "half <- function(a: double) -> integer a / 2\ns <- 0\nfor (i in 1:n) if (i > 300L) s <- s + half(2.5)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "half <- function(a: double) -> double a / 2\ns <- 0\nfor (i in 1:n) if (i > 300L) s <- s + half(i)\ns",
            "(n = 600L)",
            false,
        ),
        // A variable forgotten across an opaque call keeps its type as a guess, which an
        // opaque call that rebinds it to another type must fail.
        (
            "g <- function() total <<- 5L\ntotal <- 0\nfor (i in 1:n) { if (i == 450L) g(); if (i > 300L) total <- total + 1L }\ntotal",
            "(n = 600L)",
            false,
        ),
        // Reading Array items, which a double position past the last item cannot do.
        (
            "x <- as.Array(as.numeric(1:n) / n)\ns <- 0\nfor (i in 1:n) s <- s + x[[i]]\ns",
            "(n = 600L)",
            false,
        ),
        (
            "x <- as.Array(c(1, 2, 3))\ns <- 0\nfor (i in 1:n) s <- s + x[[if (i > 300L) 3.5 else 2]]\ns",
            "(n = 600L)",
            true,
        ),
        // Storing items keeps an Array an Array, through the copy a shared one needs.
        (
            "out <- as.Array(rep(0, n))\nfor (i in 1:n) out[[i]] <- i * 2\nlist(out[[n]], is.Array(out))",
            "(n = 600L)",
            false,
        ),
        (
            "a <- as.Array(rep(0, n))\nb <- a\nfor (i in 1:n) b[[i]] <- 1\nlist(is.Array(b), sum(b), is.Array(a), sum(a))",
            "(n = 600L)",
            false,
        ),
        // A missing value cannot be an item, so storing one leaves the Array to tier 0.
        (
            "out <- as.Array(rep(0, n))\nfor (i in 1:n) out[[i]] <- if (i == 400L) 0 / 0 else i * 2\nlist(is.Array(out), out[[400]], out[[n]])",
            "(n = 600L)",
            false,
        ),
        (
            "out <- as.Array(rep(0L, n))\nfor (i in 1:n) out[[i]] <- if (i == 400L) NA_integer_ else i\nlist(is.Array(out), out[[400]], out[[n]])",
            "(n = 600L)",
            false,
        ),
        // A loop compiled before the one after it has run: the second loop guesses an integer
        // sequence, and a double one deoptimizes.
        (
            "s <- 0\nfor (x in 1:n) s <- s + x\nfor (y in as.numeric(1:n) / 2) s <- s + y\ns",
            "(n = 600L)",
            false,
        ),
        // A promise forced by a load whose type guard then fails keeps the visibility it set.
        (
            "wv <- function(x) .Internal(withVisible(x))\nseen <- 0L\nfor (i in 1:n) if (wv(if (i < 400L) i else y <- 2.5)$visible) seen <- seen + 1L\nseen",
            "(n = 600L)",
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

#[test]
fn native_functions_agree_across_tiers() {
    // Scalar functions called with unboxed arguments run as native functions, recursion
    // included, and every way R would warn or fail leaves the call to tier 0. Each case defines
    // its functions globally, then runs a body that calls them in a loop.
    let cases = [
        (
            "fi <- function(x) if (x < 2L) x else fi(x - 1L) + fi(x - 2L)",
            "s <- 0L\nfor (i in 1:n) s <- s + fi(i %% 15L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "g <- function(x) if (x > 0L) x * x else -x",
            "s <- 0\nfor (i in 1:n) s <- s + g(i * 100L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "g <- function(x) if (x > 1) x else 0",
            "s <- 0\nfor (i in 1:n) s <- s + g(if (i == 400L) NA_real_ else i)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "deep <- function(x) if (x <= 0) 0 else 1 + deep(x - 1)",
            "s <- 0\nfor (i in 1:n) s <- s + deep(if (i == 500L) 1e5 else 3)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "r <- function(x) if (x > -1000) sqrt(x) + log(x + 400) else 0",
            "s <- 0\nfor (i in 1:n) s <- s + r(i - 300L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "h <- function(x) { y <- x * 2; if (y > 900) return(y - 1); z <- 0; if (y > 400) z <- 1; y + z }",
            "s <- 0\nfor (i in 1:n) s <- s + h(i)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "q <- function(x) (x > 3L) || (x < 0L)\np <- function(x) if (x > 3 && x < 8) 1L else 2L",
            "s <- 0L\nfor (i in 1:n) s <- s + q(i - 300L) + p(i %% 10L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "q <- function(x) (x > 3L) && (x < 500L)",
            "s <- 0L\nfor (i in 1:n) s <- s + q(if (i == 450L) NA_integer_ else i)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "half <- function(a: double) -> double if (a > 0) a / 2 else 0",
            "s <- 0\nfor (i in 1:n) s <- s + half(i)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "bad <- function(a: double) -> integer if (a > 0) a / 2 else 0",
            "s <- 0\nfor (i in 1:n) s <- s + bad(i)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "sq <- function(x) if (x > 0) x * x else 0L\nh2 <- function(x) if (x > 1) sq(x) + 1 else 0",
            "sq <<- function(x) if (x > 0) x * x else 0L\ns <- 0\nfor (i in 1:n) { if (i == 400L) sq <<- function(x) if (x > 0) -x else 1L; s <- s + h2(i) }\ns",
            "(n = 600L)",
            false,
        ),
        (
            "m <- function(x, y) if (x > y) x %/% y + x %% y + 0.5 else floor(y / 3) + abs(x) - !x",
            "s <- 0\nfor (i in 1:n) s <- s + m(i, 7L) + m(-i, 1000L)\ns",
            "(n = 600L)",
            false,
        ),
    ];
    with_interpreter(|| {
        for (case, (definitions, body, call, fails)) in cases.iter().enumerate() {
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe {
                run(definitions);
                outcomes(2_000_000 + case, body, call)
            };
            assert_eq!(reference == "error", *fails, "case {case} gave {reference}");
            for (run, outcome) in compiled.iter().enumerate() {
                assert_eq!(
                    &reference,
                    outcome,
                    "case {case}, call {}: f{call} after\n{definitions}\nwith body\n{body}",
                    run + 1
                );
            }
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
