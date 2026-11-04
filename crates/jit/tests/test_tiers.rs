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
    interp::{
        deparse, eval, lower,
        unwind::{Jump, Unwind},
        values,
    },
    jit::Mode,
    std::sync::Once,
    syntax::testing::{SplitMix64, for_each_seed},
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
    // The test interpreter has no `numeric`, and this is the same empty double vector.
    "0[0L]",
    "c(-2L, 5L, NA)",
];

#[test]
fn fuzz_jit_agrees_with_tier_zero() {
    with_interpreter(|| {
        let (mut runs, mut errors) = (0, 0);
        for_each_seed(300, |seed| {
            let mut rng = SplitMix64(seed);
            let body = program(&mut rng);
            let call = format!(
                "(a = {}, b = {}, v = {})",
                ARGUMENTS[rng.below(ARGUMENTS.len())],
                ARGUMENTS[rng.below(ARGUMENTS.len())],
                VECTORS[rng.below(VECTORS.len())]
            );
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe { outcomes(seed as usize, &body, &call) };
            runs += 1;
            errors += usize::from(reference.starts_with("error"));
            for (run, outcome) in compiled.iter().enumerate() {
                assert_eq!(
                    &reference,
                    outcome,
                    "seed {seed}, call {}: f{call} with body\n{body}",
                    run + 1
                );
            }
        });
        // Programs that fail early compare nothing, so most must run to a value. A replay of one
        // seed says nothing about the share.
        eprintln!("{errors} of {runs} programs failed");
        assert!(
            runs < 50 || errors * 2 < runs,
            "{errors} of {runs} programs failed"
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
        // Num and Str functions, which compiled code calls directly as leaves, and an
        // unchecked division by zero, which is an error.
        (
            "s <- 0\nfor (i in 1:n) { s <- Num.add(s, Num.sqrt(i * 1)); if (Str.is_empty(\"\")) s <- Num.add(s, 1) }\nNum.div_trunc(s, 0L)",
            "(n = 30L)",
            true,
        ),
        (
            "s <- 0L\nfor (i in 1:n) s <- Num.bitwise_xor(s, Num.shift_left_by(i, 2L))\ns",
            "(n = 30L)",
            false,
        ),
        // Record fields of every slot kind, which reach the field intrinsic.
        (
            "r <- (x = 1.5, y = 2L, z = TRUE, w = \"s\")\ns <- 0\nfor (i in 1:n) { s <- s + r$x * r$y; if (r$z) s <- s + 1 }\nlist(s, r$w)",
            "(n = 50L)",
            false,
        ),
        // A record of another layout mid-loop fails the layout guard.
        (
            "r <- (x = 1, y = 2)\ns <- 0\nfor (i in 1:n) { if (i == 3L) r <- (y = 5, x = 10); s <- s + r$x }\ns",
            "(n = 8L)",
            false,
        ),
        // The same layout with an integer where a double was fails the slot check.
        (
            "r <- (x = 1, y = 2)\ns <- 0\nfor (i in 1:n) { if (i == 3L) r <- (x = 4L, y = 2); s <- s + r$x }\ns",
            "(n = 8L)",
            false,
        ),
        // A list where a record was takes `$`'s own path.
        (
            "r <- (x = 1, y = 2)\ns <- 0\nfor (i in 1:n) { if (i == 3L) r <- list(x = 7); s <- s + r$x }\ns",
            "(n = 8L)",
            false,
        ),
        // Tuple items with constant indices reach the slot intrinsic; a list where the tuple was
        // hands the read back to `[[`'s path.
        (
            "t <- (1.5, 2L, \"s\")\ns <- 0\nfor (i in 1:n) { if (i == 6L) t <- list(10, 20L, \"l\"); s <- s + t[[1L]] * t[[2]] }\nlist(s, t[[3L]])",
            "(n = 9L)",
            false,
        ),
        // A list element read speculates on the doubles it saw, and an integer, a length-two
        // vector and a logical each fail the check and take tier 0's path.
        (
            "l <- list(1.5, 2.5, 3.5)\ns <- 0\nfor (i in 1:n) { if (i == 5L) l[[2L]] <- 7L; if (i == 7L) l[[2L]] <- c(1, 2); if (i == 9L) l[[2L]] <- TRUE; s <- s + l[[2L]] * l[[(i %% 3L) + 1L]] }\ns",
            "(n = 12L)",
            false,
        ),
        // A compiled `match` over tags, tuples, records and literals, with nested payloads.
        (
            "s <- 0\nfor (i in 1:n) { v <- if (i %% 4L == 0L) :Some((i, 2)) else if (i %% 4L == 1L) :None else if (i %% 4L == 2L) (x = i, y = 1L) else \"k\"\ns <- s + match (v) { :Some((a, b)) => a * b, :None => 1, (x = a, y = 1L) => a, \"k\" | \"j\" => 0.5, _ => 100 } }\ns",
            "(n = 40L)",
            false,
        ),
        // Guarded arms in a compiled loop: the guard reads the pattern's names and a loop
        // variable, and a failing guard falls through to the next arm.
        (
            "s <- 0\nfor (i in 1:n) { v <- if (i %% 3L == 0L) :Some(i) else (i, 2L)\ns <- s + match (v) { :Some(x) if (x > 10L) => x, :Some(x) => -1, (a, b) if (a * b > i + 5L) => a, (a, b) => 0.5 } }\ns",
            "(n = 30L)",
            false,
        ),
        // A guard that calls a closure on a bound name evaluates that call in a scratch frame,
        // beside an inlined guard, in every tier.
        (
            "big <- function(z) z > 4\ns <- 0\nfor (i in 1:n) { s <- s + match (:Some(i %% 7L)) { :Some(x) if (big(x)) => 1, :Some(x) if (abs(x - 2L) < 1L) => 10, _ => 0.5 } }\ns",
            "(n = 30L)",
            false,
        ),
        // A guard on a string binding, and a tuple wider than the unboxed slot mask, whose
        // trailing numbers are boxed while earlier ones wait to be stored.
        (
            "s <- 0\nfor (i in 1:n) { t <- (i + 1.5, i + 2.5, i + 3.5, i + 4.5, i + 5.5, i + 6.5, i + 7.5, i + 8.5, i + 9.5, i + 10.5, i + 11.5, i + 12.5, i + 13.5, i + 14.5, i + 15.5, i + 16.5, i + 17.5, i + 18.5, i + 19.5, i + 20.5, i + 21.5, i + 22.5, i + 23.5, i + 24.5, i + 25.5, i + 26.5, i + 27.5, i + 28.5, i + 29.5, i + 30.5, i + 31.5, i + 32.5, i + 33.5, i + 34.5, i + 35.5, i + 36.5, i + 37.5, i + 38.5, i + 39.5, i + 40.5, i + 41.5, i + 42.5, i + 43.5, i + 44.5, i + 45.5, i + 46.5, i + 47.5, i + 48.5, i + 49.5, i + 50.5, i + 51.5, i + 52.5, i + 53.5, i + 54.5, i + 55.5, i + 56.5, i + 57.5, i + 58.5, i + 59.5, i + 60.5, i + 61.5, i + 62.5, i + 63.5, i + 64.5, i + 65.5, i + 66.5, i + 67.5, i + 68.5, i + 69.5, i + 70.5)\ns <- s + t[[66L]] + t[[70L]]\ns <- s + match (:Some(if (i %% 3L == 0L) \"aaa\" else \"b\")) { :Some(x) if (x == \"aaa\") => 1, _ => 0.5 } }\ns",
            "(n = 30L)",
            false,
        ),
        // Records, tuples and tags built in compiled code from unboxed doubles, integers,
        // logicals and NA, and from a boxed vector, read back through fields and `match`.
        (
            "s <- 0\nk <- 0L\nfor (i in 1:n) { r <- (a = i * 0.5, b = i, c = i > 3L, d = if (i == 7L) NA_real_ else 1, e = c(i, 2L))\nt <- (r$a, i + 1L)\ng <- :Pt(x = r$b, y = r$a)\ns <- s + r$a + r$b + r$d + t[[1L]] + match (g) { :Pt(x = x, y = y) => x * y }\nif (r$c) k <- k + r$e[[2L]] }\nlist(s, k)",
            "(n = 12L)",
            false,
        ),
        // A tag pattern with `..`, constructors in the loop, and no arm matching at the end.
        (
            "s <- 0L\nfor (i in 1:n) { r <- if (i < n) :Ok(i) else :Other(i)\ns <- s + match (r) { :Ok(..) => 1L, :Err(e) => e } }\ns",
            "(n = 12L)",
            true,
        ),
        // A boxed field, then a missing one, which is an error.
        (
            "r <- (x = c(1, 2), y = 2)\ns <- 0\nfor (i in 1:n) { if (i == 5L) r <- (y = 1,); s <- s + r$x[[1L]] }\ns",
            "(n = 8L)",
            true,
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
            "x <- List.from_r(as.numeric(1:n) / n)\ns <- 0\nfor (i in 1:n) s <- s + x[[i]]\ns",
            "(n = 600L)",
            false,
        ),
        (
            "x <- List.from_r(c(1, 2, 3))\ns <- 0\nfor (i in 1:n) s <- s + x[[if (i > 300L) 3.5 else 2]]\ns",
            "(n = 600L)",
            true,
        ),
        // Storing items keeps an Array an Array, through the copy a shared one needs.
        (
            "out <- List.from_r(rep(0, n))\nfor (i in 1:n) out[[i]] <- i * 2\nlist(out[[n]], .ry_is_list(out))",
            "(n = 600L)",
            false,
        ),
        (
            "a <- List.from_r(rep(0, n))\nb <- a\nfor (i in 1:n) b[[i]] <- 1\nlist(.ry_is_list(b), sum(b), .ry_is_list(a), sum(a))",
            "(n = 600L)",
            false,
        ),
        // A missing value cannot be an item, so storing one leaves the Array to tier 0.
        (
            "out <- List.from_r(rep(0, n))\nfor (i in 1:n) out[[i]] <- if (i == 400L) 0 / 0 else i * 2\nlist(.ry_is_list(out), out[[400]], out[[n]])",
            "(n = 600L)",
            false,
        ),
        // An R integer becomes `I64` in a list, and a `List<I64>` has no R vector to fall back
        // to, so storing an R value in one is an error in every tier.
        (
            "out <- List.from_r(rep(0L, n))\nfor (i in 1:n) out[[i]] <- if (i == 400L) NA_integer_ else i\nlist(.ry_is_list(out), out[[400]], out[[n]])",
            "(n = 600L)",
            true,
        ),
        // A loop compiled before the one after it has run: the second loop guesses an integer
        // sequence, and a double one deoptimizes.
        (
            "s <- 0\nfor (x in 1:n) s <- s + x\nfor (y in as.numeric(1:n) / 2) s <- s + y\ns",
            "(n = 600L)",
            false,
        ),
        // An integer and a double joined stay what they were: integer arithmetic on the
        // integer side, `typeof` and `NA` all see the original value.
        (
            "out <- NULL\nfor (i in 1:n) { r <- if (i %% 2L == 0L) i else 0.5; out <- c(out, .Internal(typeof(r)), r + 1L, r < 3, r / 2) }\nout",
            "(n = 300L)",
            false,
        ),
        (
            "out <- NULL\nfor (i in 1:n) { r <- if (i == 2L) NA_integer_ else if (i == 3L) NA_real_ else i; out <- c(out, r * 2, .Internal(typeof(r))) }\nout",
            "(n = 300L)",
            false,
        ),
        // A loop-carried variable that alternates between integer and double.
        (
            "x <- 0L\nfor (i in 1:n) x <- if (i %% 2L == 0L) x + 1L else x * 1.5\nc(x, .Internal(typeof(x)))",
            "(n = 40L)",
            false,
        ),
        // A modulus too large to be accurate warns, which compiled code leaves to tier 0.
        (
            "s <- 0\nfor (i in 1:n) { x <- if (i == 20L) 1e308 else i + 0.5; s <- s + x %% 7 }\ns",
            "(n = 40L)",
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
            assert_eq!(
                reference.starts_with("error"),
                *fails,
                "seed {case} gave {reference}"
            );
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
            // Closure arguments are promises: `b` is forced first and finds the global `y`.
            "y <- 100L\nlz2 <- function(a, b) b + a\nlz <- function(x) lz2(y <- x, y)",
            "s <- 0L\nfor (i in 1:n) s <- s + lz(1L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "lz1 <- function(a) 1L\nlzr <- function(x) { lz1(return(5L)); x }",
            "s <- 0L\nfor (i in 1:n) s <- s + lzr(1L)\ns",
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
            "md <- function(x) if (x > 0) x %% 7 else 0",
            "s <- 0\nfor (i in 1:n) s <- s + md(if (i == 400L) 1e300 else i + 0.5)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "m <- function(x, y) if (x > y) x %/% y + x %% y + 0.5 else floor(y / 3) + abs(x) - !x",
            "s <- 0\nfor (i in 1:n) s <- s + m(i, 7L) + m(-i, 1000L)\ns",
            "(n = 600L)",
            false,
        ),
        // Small callees compile into their callers: their locals stay their own, a `break`
        // inside one leaves only its own loop, and a failure inside one fails the caller.
        (
            "ia <- function(x) { s <- x * 2; s + 1 }\nib <- function(x) { s <- 10; for (k in 1:3) { if (k > 2) break; s <- s + ia(k + x) }; s }",
            "s <- 0\nfor (i in 1:n) s <- s + ib(i)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "ic <- function(x) { t <- 0L; for (k in 1:x) { if (k > 4L) break; t <- t + k }; t }\nid <- function(x) { t <- 0L; for (j in 1:3) t <- t + ic(x + j); t }",
            "s <- 0L\nfor (i in 1:n) s <- s + id(i %% 9L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "ie <- function(x) x + 1L\nif2 <- function(x) ie(ie(x)) * 2L\nig <- function(x) if2(x) - ie(x)",
            "s <- 0L\nfor (i in 1:n) s <- s + ig(if (i == 400L) 2147483646L else i)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "fr <- function(x) if (x < 2L) x else fr(x - 1L) + fr(x - 2L)\nih <- function(x) fr(x) + fr(x + 1L)",
            "s <- 0L\nfor (i in 1:n) s <- s + ih(i %% 12L)\ns",
            "(n = 600L)",
            false,
        ),
        // Loops and vector reads: a native function takes a plain vector as its object.
        (
            "dot <- function(x, y) { s <- 0; for (i in seq_along(x)) s <- s + x[[i]] * y[i]; s }",
            "p <- c(1, 2.5, -3)\nq <- c(2, 0.5, 4)\ns <- 0\nfor (i in 1:n) s <- s + dot(p, q) + i\ns",
            "(n = 600L)",
            false,
        ),
        (
            "el <- function(x, i) x[[i]] + 1L",
            "p <- c(4L, NA, 6L)\ns <- 0L\nfor (i in 1:n) s <- s + el(p, if (i == 500L) 4L else 1L + i %% 3L)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "ed <- function(x, i) x[i] * 2",
            "p <- c(0.5, 1.5)\ns <- 0\nfor (i in 1:n) s <- s + ed(p, if (i == 500L) 2.9 else 1.2)\ns",
            "(n = 600L)",
            false,
        ),
        // `x[TRUE]` recycles the logical over the whole vector; `x[[TRUE]]` is the first element.
        (
            "lg <- function(x) length(x[TRUE]) * 10 + x[[TRUE]]",
            "p <- c(0.5, 1.5, 2)\ns <- 0\nfor (i in 1:n) s <- s + lg(p)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "cnt <- function(n) { k <- 0L; i <- 0L; while (TRUE) { i <- i + 1L; if (i > n) break; if (i %% 3L == 0L) next; k <- k + i }; k }",
            "s <- 0L\nfor (i in 1:n) s <- s + cnt(i %% 40L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "rp <- function(n) { i <- 0; repeat { i <- i + 1; if (i >= n) return(i * 2) } }",
            "s <- 0\nfor (i in 1:n) s <- s + rp(i %% 30L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "cs <- function(a, b) { s <- 0L; for (j in a:b) s <- s + j; s }",
            "s <- 0L\nfor (i in 1:n) s <- s + cs(i %% 7L, 3) + cs(4L, -i %% 5L)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "cd <- function(a) { s <- 0; for (j in a:3) s <- s + j; s }",
            "s <- 0\nfor (i in 1:n) s <- s + cd(if (i == 300L) 0.5 else 1)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "sm <- function(x) { s <- 0; for (v in x) { if (v > 2) next; s <- s + v }; s }",
            "p <- c(1, 2.5, 0.25)\ns <- 0\nfor (i in 1:n) s <- s + sm(p) + length(p)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "sl <- function(n) { s <- 0L; for (i in seq_len(n)) s <- s + i; s }",
            "s <- 0L\nfor (i in 1:n) s <- s + sl(if (i == 450L) -1L else i %% 9L)\ns",
            "(n = 600L)",
            true,
        ),
        (
            "sc <- function(x) { if (x > 0) { t <- x * 2; x <- t }; for (j in 1:3) { u <- j; x <- x + u }; x }\nln <- function(x) length(x) + 0.5",
            "p <- c(TRUE, NA)\ns <- 0\nfor (i in 1:n) s <- s + sc(i - 300) + ln(p) + ln(i)\ns",
            "(n = 600L)",
            false,
        ),
        (
            "spin <- function(n) { i <- 0L; while (i < n) i <- i + 1L; i }",
            "s <- 0L\nfor (i in 1:n) s <- s + spin(if (i == 300L) NA_integer_ else i)\ns",
            "(n = 600L)",
            true,
        ),
    ];
    with_interpreter(|| {
        for (case, (definitions, body, call, fails)) in cases.iter().enumerate() {
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe {
                run(definitions);
                outcomes(2_000_000 + case, body, call)
            };
            assert_eq!(
                reference.starts_with("error"),
                *fails,
                "case {case} gave {reference}"
            );
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

#[test]
fn traversals_agree_across_tiers() {
    // A library traversal whose callback inlines runs as a compiled loop. A list or an element
    // the loop does not expect, an `NA` answer or an integer overflow hands the call back to
    // tier 0, which must then give the same value or error.
    // `List.from_r` turns R integers into boxed `I64`s, so the list of R integers the compiled
    // integer loops read is written as a literal.
    let integers = (1..=50)
        .map(|item| format!("{item}L"))
        .collect::<Vec<_>>()
        .join(", ");
    let definitions = format!(
        "xs <- List.from_r(1:50 + 0)\nys <- [{integers}]\nflip <- 0L\n\
         add <- function(acc, x) acc + x\nstep <- add\n\
         ds <- Dict.insert(Dict.insert(Dict.insert(Dict.empty(), 1L, 10), 2L, 20), 3L, 30)\n\
         sd <- Dict.insert(Dict.insert(Dict.empty(), \"a\", 1), \"b\", 2)\n\
         st <- Set.from_list(List.from_r(1:20 + 0.5))"
    );
    let cases = [
        ("List.walk(xs, 0, function(acc, x) acc + x * b)", false),
        ("List.walk(ys, 0L, function(acc, x) acc + x)", false),
        (
            "List.walk(list(2147483647L, 1L), 0L, function(acc, x) acc + x)",
            false,
        ),
        ("List.walk(xs, 0, add) + List.walk(xs, 1, add)", false),
        (
            "flip <<- flip + 1L\nif (flip %% 3L == 0L) step <<- function(acc, x) x + acc else step <<- add\nList.walk(xs, 0, step)",
            false,
        ),
        (
            "b <- b + 0\nList.walk(xs, 0, function(acc, x) acc + x * b)",
            false,
        ),
        (
            "s <- 0\nfor (l in list(xs, list(), ys)) s <- s + List.walk(l, 1, function(acc, x) acc * 0.5 + x * b)\ns",
            false,
        ),
        (
            "flip <<- flip + 1L\nl <- if (flip %% 2L == 0L) list(1, 2) else list(1, 2L)\nList.walk(l, 0, function(acc, x) acc + x)",
            false,
        ),
        (
            "List.walk(list(1, \"a\"), 0, function(acc, x) acc + x)",
            true,
        ),
        ("List.walk(c(1, 2), 0, function(acc, x) acc + x)", true),
        ("List.map(xs, function(x) x * 2)", false),
        ("List.map(ys, function(x) x %/% 3L)", false),
        ("List.map(ys, function(x) x > 25L)", false),
        ("List.map(list(), function(x) x * 2)", false),
        ("List.keep_if(xs, function(x) x > 20)", false),
        ("List.drop_if(ys, function(x) x %% 3L == 0L)", false),
        ("List.keep_if(list(a = 1, b = 5), function(x) x > 2)", false),
        ("List.any(xs, function(x) x > 40)", false),
        ("List.any(xs, function(x) x < 0)", false),
        ("List.all(ys, function(x) x > 0L)", false),
        ("List.all(ys, function(x) x < 10L)", false),
        ("List.count_if(xs, function(x) x > b * 10)", false),
        ("List.any(list(1, NA_real_, 3), function(x) x > 2)", true),
        ("List.count_if(list(1, 2), function(x) x)", true),
        (
            "List.walk(xs, 0, function(acc: double, x: double) -> double acc + x)",
            false,
        ),
        ("List.find_first(xs, function(x) x > 20)", false),
        ("List.find_last(ys, function(x) x %% 7L == 0L)", false),
        ("List.find_first_index(xs, function(x) x > 20)", false),
        ("List.find_last_index(xs, function(x) x < 0)", false),
        ("List.find_first(list(), function(x) x > 0)", false),
        ("List.find_first(list(1, NA, 3), function(x) x > 2)", true),
        (
            "List.walk_backwards(xs, 0, function(acc, x) acc * 0.5 + x)",
            false,
        ),
        (
            "List.walk_with_index(xs, 0, function(acc, x, i) acc + x * i)",
            false,
        ),
        (
            "List.walk_from(ys, 10L, 0L, function(acc, x) acc + x)",
            false,
        ),
        (
            "List.walk_from(ys, 60, 0L, function(acc, x) acc + x)",
            false,
        ),
        (
            "List.walk_from(ys, 2.5, 0L, function(acc, x) acc + x)",
            true,
        ),
        ("List.walk_from(ys, 0L, 0L, function(acc, x) acc + x)", true),
        (
            "List.walk_until(xs, 0, function(acc, x) if (acc > 100) :Break(acc) else :Continue(acc + x))",
            false,
        ),
        (
            "List.walk_backwards_until(ys, 0L, function(acc, x) if (x < 10L) :Break(acc) else :Continue(acc + x))",
            false,
        ),
        (
            "List.walk_with_index_until(xs, 0, function(acc, x, i) { if (i > 5L) :Break(acc) else :Continue(acc + x) })",
            false,
        ),
        (
            "List.walk_from_until(ys, 3L, 0L, function(acc, x) :Continue(acc + x))",
            false,
        ),
        (
            "List.walk_until(list(1, NA, 3), 0, function(acc, x) if (x > 1) :Break(acc) else :Continue(acc + x))",
            true,
        ),
        (
            "List.walk_until(xs, 0, function(acc, x) if (acc > 10) :Stop(acc) else :Continue(acc + x))",
            true,
        ),
        ("List.map_with_index(xs, function(x, i) x * i)", false),
        ("List.map(xs, function(x) if (x > 25) x else -1)", false),
        ("Dict.walk(ds, 0, function(acc, k, v) acc + k * v)", false),
        ("Dict.walk(sd, 0, function(acc, k, v) acc + v)", false),
        (
            "Dict.walk(Dict.empty(), 0, function(acc, k, v) acc + v)",
            false,
        ),
        (
            "Dict.walk_until(ds, 0L, function(acc, k, v) if (k > 1L) :Break(acc) else :Continue(acc + k))",
            false,
        ),
        ("Set.walk(st, 0, function(acc, x) acc + x)", false),
        (
            "Set.walk_until(st, 0, function(acc, x) if (acc > 20) :Break(acc) else :Continue(acc + x))",
            false,
        ),
        ("Dict.walk(xs, 0, function(acc, k, v) acc + v)", true),
    ];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        unsafe { run(&definitions) };
        for (case, (body, fails)) in cases.iter().enumerate() {
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe { outcomes(3_000_000 + case, body, "()") };
            assert_eq!(
                reference.starts_with("error: "),
                *fails,
                "case {case} gave {reference}"
            );
            for (run, outcome) in compiled.iter().enumerate() {
                assert_eq!(&reference, outcome, "case {case}, call {}: {body}", run + 1);
            }
        }
    });
}

#[test]
fn run_time_binding_shadows_cached_global_function() {
    // Once compiled code has cached the global `g`, a `g` bound in the activation's own frame at
    // run time, in ways the compiler cannot see, must shadow it. The oracle case
    // `environments__hidden_function_bindings` covers `assign` into `parent.frame()`, which needs
    // base R's closures.
    with_interpreter(|| unsafe {
        jit::install_mode(Mode::Force);
        run("g <- function() \"global\"");
        let definitions = [
            "via_default <- function(set, x = if (set) (g <- function() \"local\")) { x; g() }",
            "gen <- function(x, set) { if (set) g <- function() \"local\"; UseMethod(\"gen\") }\ngen.default <- function(x, set) g()",
        ];
        let calls = ["via_default", "gen"];
        let mut outcomes = Vec::new();
        for (definition, name) in definitions.iter().zip(calls) {
            run(definition);
            let call = |set: &str| {
                if name == "gen" {
                    format!("gen(1, {set})")
                } else {
                    format!("{name}({set})")
                }
            };
            for _ in 0..3 {
                run(&call("FALSE"));
            }
            outcomes.push((name, run(&call("TRUE"))));
        }
        jit::install_mode(Mode::Off);
        for (name, outcome) in outcomes {
            assert_eq!(outcome, "\"local\"", "{name}");
        }
    });
}

#[test]
fn closures_from_one_site_share_code_but_not_lookups() {
    // Each call of `make` makes a new closure from one `function` expression, so the closures
    // share one compiled body. Code whose functions all live in long-lived environments runs
    // for every closure, but each new enclosure's lookups are checked again: one that binds its
    // own `g` must reach it, and a closure that calls a function local to its enclosure must
    // call its own.
    with_interpreter(|| unsafe {
        jit::install_mode(Mode::Force);
        run("g <- function(v) v + 1\napply1 <- function(f, x) f(x)");
        run(
            "make <- function(shadow) { if (shadow) g <- function(v) \"local\"; apply1(function(v) g(v), 1) }",
        );
        run("scaled <- function(k) { h <- function(v) v * k; apply1(function(v) h(v), 1) }");
        let mut outcomes = Vec::new();
        for _ in 0..4 {
            outcomes.push(("make(FALSE)", run("make(FALSE)"), "2"));
        }
        outcomes.push(("make(TRUE)", run("make(TRUE)"), "\"local\""));
        outcomes.push(("make(FALSE)", run("make(FALSE)"), "2"));
        for k in 1..6 {
            outcomes.push((
                "scaled",
                run(&format!("scaled({k})")),
                ["1", "2", "3", "4", "5"][k - 1],
            ));
        }
        jit::install_mode(Mode::Off);
        for (call, outcome, expected) in outcomes {
            assert_eq!(outcome, expected, "{call}");
        }
    });
}

#[test]
fn formal_shadows_enclosing_function() {
    // `sum` names a base function, but the call must reach the function passed in its place.
    with_interpreter(|| unsafe {
        jit::install_mode(Mode::Force);
        run("shadowing <- function(sum, n) {\nfor (i in 1:n) NULL\nsum(1, 2)\n}");
        let outcomes: Vec<String> = (0..4)
            .map(|_| run("shadowing(function(...) 99, 3L)"))
            .collect();
        jit::install_mode(Mode::Off);
        for outcome in outcomes {
            assert_eq!(outcome, "99");
        }
    });
}

#[test]
fn fully_typed_closure_compiles_at_its_first_call() {
    // The signature stands in for the profile, so a first call runs compiled code: a typed
    // scalar function runs as a native function from its first call, whose values fix the
    // argument types, unless an argument is a promise of a computation (`c(1, 2.5)`), which
    // only the frame may force; a straight-line body then waits until it is hot. A call whose
    // values the declared types do not predict still gets tier 0's answer.
    with_interpreter(|| unsafe {
        jit::install_mode(Mode::On);
        let cases = [
            (
                "ft <- function(x: double, y: double) -> double x * y + 1",
                "ft(2, 3)",
                "7",
                Some(true),
            ),
            (
                "fi <- function(x: double) -> double x * 2",
                "fi(2L)",
                "4",
                None,
            ),
            (
                "fv <- function(v: double[], k: integer) -> double v[2L] * k",
                "fv(c(1, 2.5), 2L)",
                "5",
                Some(false),
            ),
            (
                "fe <- function(x: double) -> double x + 1",
                "fe(\"a\")",
                "error",
                None,
            ),
            (
                "fu <- function(x: double, y) -> double x + y",
                "fu(1, 2)",
                "3",
                Some(true),
            ),
            (
                "fl <- function(v: double[]) -> double { s <- 0; for (i in seq_along(v)) s <- s + v[i]; s }",
                "fl(c(1, 2.5))",
                "3.5",
                Some(true),
            ),
        ];
        let mut outcomes = Vec::new();
        for (index, (definition, call, _, _)) in cases.iter().enumerate() {
            run(definition);
            let outcome = run(call);
            let name = format!("f{}", ["t", "i", "v", "e", "u", "l"][index]);
            let compiled = has_compiled_code(&name);
            outcomes.push((outcome, compiled));
        }
        // A straight-line body the first call left alone still compiles once it is hot.
        run("for (i in 1:5000) ft(2, 3)");
        let hot = has_compiled_code("ft");
        jit::install_mode(Mode::Off);
        assert!(hot, "ft never compiled after 5000 calls");
        for ((definition, call, expected, eager), (outcome, compiled)) in cases.iter().zip(outcomes)
        {
            let agrees = if *expected == "error" {
                outcome.starts_with("error: ")
            } else {
                outcome == *expected
            };
            assert!(agrees, "{definition}; {call} gave {outcome}");
            // A call the seeds mispredict deoptimizes and discards the code, so only the others
            // say whether the first call compiled.
            if let Some(eager) = eager {
                assert_eq!(compiled, *eager, "{definition}; {call}");
            }
        }
    });
}

/// A type a random typed closure may declare for a formal: its annotation, expressions over the
/// formal (written `@`) that give a number, arguments that pass its check, and arguments that
/// fail it.
struct Formal {
    annotation: &'static str,
    uses: &'static [&'static str],
    passing: &'static [&'static str],
    failing: &'static [&'static str],
}

const FORMALS: &[Formal] = &[
    Formal {
        annotation: "double",
        uses: &["@", "@ * 2", "@ + 1L", "-@"],
        // An integer and a logical pass a `double` check; a length-two double passes the
        // storage check, though the compiled code expects a scalar.
        passing: &["2.5", "-1", "3L", "TRUE", "NA_real_", "c(1, 2)", "1e308"],
        failing: &["\"a\"", "list(1)", "NULL"],
    },
    Formal {
        annotation: "integer",
        uses: &["@", "@ * 2L", "@ %/% 2L", "@ + 0.5"],
        // A whole-number double passes an `integer` check.
        passing: &[
            "3L",
            "NA_integer_",
            "TRUE",
            "4",
            "-7L",
            "c(1L, 2L)",
            "2147483647L",
        ],
        failing: &["2.5", "\"a\"", "NA_real_"],
    },
    Formal {
        annotation: "logical",
        uses: &["(if (@) 1L else 2.5)", "@ + 1L", "!@"],
        passing: &["TRUE", "FALSE", "NA", "c(TRUE, FALSE)"],
        failing: &["1L", "0.5", "\"TRUE\""],
    },
    Formal {
        annotation: "character",
        uses: &["(@ == \"a\")", "(if (@ == \"b\") 1 else 0L)"],
        passing: &["\"a\"", "\"b\"", "NA_character_"],
        failing: &["1", "TRUE"],
    },
    Formal {
        annotation: "double[]",
        uses: &["@[1L]", "@[[2L]]", "length(@)", "@[2] * 2"],
        passing: &["c(1, 2.5, 3)", "numeric(0)", "1:3", "c(NA, 1)", "5"],
        failing: &["c(\"a\", \"b\")", "list(1, 2)"],
    },
    Formal {
        annotation: "integer[]",
        uses: &["@[1L]", "@[[2L]] + 1L", "length(@)"],
        passing: &["1:4", "c(NA, 2L)", "c(1, 2)", "integer(0)"],
        failing: &["c(1.5, 2)", "\"a\""],
    },
    Formal {
        annotation: "list[double]",
        uses: &["@[[1L]]", "@[[2]] * 2", "length(@)"],
        passing: &[
            "list(1, 2.5)",
            "list(1L, 2)",
            "list(\"a\", 1)",
            "list(c(1, 2), 3)",
        ],
        failing: &["c(1, 2)", "(1, 2)"],
    },
    Formal {
        annotation: "Record{a: double, b: integer}",
        uses: &["@$a", "@$b * 2L", "@$a + @$b"],
        passing: &[
            "(a = 1.5, b = 2L)",
            "(b = 2L, a = 1.5)",
            "(a = 1L, b = 2L)",
            "(a = c(1, 2), b = NA_integer_)",
        ],
        failing: &["list(a = 1, b = 2L)", "1"],
    },
    Formal {
        annotation: "Tuple{double, integer}",
        uses: &["@[[1L]]", "@[[2L]] + 1L", "@[[1]] * @[[2]]"],
        passing: &["(1.5, 2L)", "(1L, 2L)", "(NA_real_, 3L)", "(\"s\", 1L)"],
        failing: &["list(1, 2L)", "c(1, 2)"],
    },
];

/// A return annotation and how the body's number becomes a value of that type (written `@`),
/// which the number's own type may still violate.
const RETURNS: &[(&str, &str)] = &[
    ("double", "@"),
    ("integer", "@"),
    ("logical", "@ > 0"),
    ("double[]", "c(@, 1)"),
    ("list[double]", "list(@)"),
    ("Record{v: double}", "(v = @,)"),
    ("Tuple{double, double}", "(@, 2)"),
    ("", "@"),
];

/// Random fully typed closures, half of which loop and so compile at their first call, called with arguments that
/// pass and fail their checks, too many and too few, and returning values that may fail the
/// return check, against the same calls with the JIT off. The calls repeat, so compiled code
/// that deoptimizes after its first call is exercised too.
#[test]
fn typed_natives_keep_tier_zero_semantics_at_their_edges() {
    // `$` and `[[` of a tag are errors, so no native reads a payload that way. A promise a
    // native call read is forced, so a later reader through `...` sees the same value.
    // A loop that rebinds a record runs natively and leaves nothing rooted once it returns.
    with_interpreter(|| unsafe {
        jit::install_mode(Mode::On);
        let cases = [
            (
                "type Pos = :Pos(double)\nfp <- function(t: Pos) -> double t[[1]]\nfp(:Pos(1))",
                "error: a tag has no items; read its payload with `match`",
            ),
            (
                "type Opt = :Some(v: double) | :None\nfo <- function(t: Opt) -> double t$v\nfo(:Some(v = 1))",
                "error: a tag has no fields; read its payload with `match`",
            ),
            (
                "fd <- function(y: double) -> double y + 1\n\
                 hd <- function(...) { fd(...); xd <<- 2; list(...)[[1]] }\n\
                 xd <- 1\nhd(xd)",
                "1",
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(run(text), expected, "{text}");
        }
        run("fg <- function(n: integer) -> double {\n\
             s <- (x = 0, y = 1)\n\
             i <- 0L\n\
             while (i < n) { s <- (x = s$x + 1, y = s$y); i <- i + 1L }\n\
             s$x\n}");
        // The first call compiles, which roots the closure for the session.
        assert_eq!(run("fg(10L)"), "10");
        let before = interp::heap().handle_mark();
        assert_eq!(run("fg(100000L)"), "1e+05");
        assert_eq!(interp::heap().handle_mark(), before);
        assert!(has_compiled_code("fg"), "the record loop runs natively");
        jit::install_mode(Mode::Off);
    });
}

#[test]
fn fuzz_typed_closures_agree_with_tier_zero() {
    with_interpreter(|| {
        let (mut runs, mut eager) = (0, 0);
        for_each_seed(400, |seed| {
            let mut rng = SplitMix64(seed);
            let names = ["x", "y", "z"];
            let formals: Vec<&Formal> = (0..rng.below(3) + 1)
                .map(|_| &FORMALS[rng.below(FORMALS.len())])
                .collect();
            let uses = |rng: &mut SplitMix64| {
                let index = rng.below(formals.len());
                let template = formals[index].uses[rng.below(formals[index].uses.len())];
                format!("({})", template.replace('@', names[index]))
            };
            let mut number = uses(&mut rng);
            for _ in 0..rng.below(3) {
                let operator = ["+", "-", "*", "/"][rng.below(4)];
                number = format!("{number} {operator} {}", uses(&mut rng));
            }
            // Only a body that can run long compiles at its first call under `Mode::On`, so
            // half the bodies loop to keep the harness reaching first-call code.
            let number = match rng.below(4) {
                0 | 2 => format!(
                    "{{ acc <- 0L; for (i in 1:{}) acc <- acc + {number}; acc }}",
                    rng.below(4) + 1
                ),
                1 => format!(
                    "if ({} > 0) {number} else {}",
                    uses(&mut rng),
                    uses(&mut rng)
                ),
                _ => number,
            };
            let (returned, wrap) = RETURNS[rng.below(RETURNS.len())];
            let body = wrap.replace('@', &format!("({number})"));
            let signature = formals
                .iter()
                .zip(names)
                .map(|(formal, name)| format!("{name}: {}", formal.annotation))
                .collect::<Vec<_>>()
                .join(", ");
            let arrow = if returned.is_empty() {
                String::new()
            } else {
                format!(" -> {returned}")
            };
            let definition = format!("function({signature}){arrow} {{ {body} }}");
            let calls: Vec<String> = (0..6)
                .map(|_| {
                    let mut arguments: Vec<String> = formals
                        .iter()
                        .map(|formal| {
                            let values = if rng.chance(1, 4) {
                                formal.failing
                            } else {
                                formal.passing
                            };
                            values[rng.below(values.len())].to_owned()
                        })
                        .collect();
                    match rng.below(10) {
                        0 => {
                            arguments.pop();
                        }
                        1 => arguments.push("1".to_owned()),
                        2 => {
                            // By name, in reverse order.
                            arguments = arguments
                                .into_iter()
                                .zip(names)
                                .map(|(argument, name)| format!("{name} = {argument}"))
                                .rev()
                                .collect();
                        }
                        _ => {}
                    }
                    format!("({})", arguments.join(", "))
                })
                .collect();
            let mode = if rng.chance(1, 4) {
                Mode::Force
            } else {
                Mode::On
            };
            // SAFETY: the interpreter is installed.
            let (reference, compiled, compiled_at_first_call) = unsafe {
                let run_all = |name: &str, mode: Mode| {
                    jit::install_mode(mode);
                    run(&format!("{name} <- {definition}"));
                    let first = run(&format!("{name}{}", calls[0]));
                    let compiled = has_compiled_code(name);
                    let mut outcomes = vec![first];
                    outcomes.extend(calls[1..].iter().map(|call| run(&format!("{name}{call}"))));
                    jit::install_mode(Mode::Off);
                    (outcomes, compiled)
                };
                let (reference, _) = run_all(&format!("typed_reference_{seed}"), Mode::Off);
                let (compiled, eager) = run_all(&format!("typed_compiled_{seed}"), mode);
                (reference, compiled, eager)
            };
            runs += 1;
            eager += usize::from(compiled_at_first_call);
            for (index, (expected, outcome)) in reference.iter().zip(&compiled).enumerate() {
                assert_eq!(
                    expected,
                    outcome,
                    "seed {seed}, call {}: f{} with\nf <- {definition}",
                    index + 1,
                    calls[index]
                );
            }
        });
        // The harness is about first-call compilation, so it must reach it.
        eprintln!("{eager} of {runs} closures compiled at their first call");
        assert!(
            runs < 50 || eager * 5 > runs,
            "only {eager} of {runs} closures compiled at their first call"
        );
    });
}

/// Whether the global closure `name` holds compiled code right now, or runs as a native
/// function.
unsafe fn has_compiled_code(name: &str) -> bool {
    unsafe {
        let function = lower::parse_text(name, "<test>")
            .and_then(|parsed| eval::eval(parsed.element(0), values::global_env()));
        function.is_ok_and(|function| {
            function.kind() == heap::object::SexpType::Closure
                && (jit::has_native(function)
                    || function.cdr().kind() == heap::object::SexpType::Bytecode
                        && function.cdr().cdr().element(interp::bytecode::TIER1) != values::nil())
        })
    }
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
        // A message names no call, because the tiers run closures of different names.
        let mut rendered = match outcome {
            Ok(value) => {
                deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n")
            }
            Err(Unwind::Error(error)) => format!("error: {}", error.message),
            // An error no handler caught ran the default handler, which recorded the message
            // after a head that names the call.
            Err(Unwind::Jump(jump)) if matches!(*jump, Jump::TopLevel) => {
                let text = &interp::interp().error_message;
                // The head breaks the line when the call is long, so the break moves with the
                // closure's name and is normalized away.
                let message = match text.strip_prefix("Error: ") {
                    Some(message) => message,
                    None => text
                        .split_once(" :")
                        .map_or(text.as_str(), |(_, message)| message),
                };
                let message: Vec<&str> = message.split_whitespace().collect();
                format!("error: {}", message.join(" "))
            }
            Err(_) => "error: a jump to the top level".to_owned(),
        };
        for (_, message) in std::mem::take(&mut interp::interp().warnings) {
            rendered.push_str(&format!("\nwarning: {message}"));
        }
        interp::interp().frames.clear();
        interp::heap().release_handles(mark);
        rendered
    }
}

/// A random body over the formals `a`, `b`, `v`, `n`, `k` and `d`: a setup that also builds a
/// record `r`, a tuple `u`, a list `l` and a tag `g`, one or two loops or branches, and a final
/// expression.
fn program(rng: &mut SplitMix64) -> String {
    let mut statements = vec![
        "total <- 0L".to_owned(),
        "i <- 1L".to_owned(),
        "s <- 1L".to_owned(),
        // The generated setup may read any of these before it assigns it.
        "r <- (x = 1, y = 2L)".to_owned(),
        "u <- (1L, 2)".to_owned(),
        "l <- list(1, 2L, 3)".to_owned(),
        "g <- :None".to_owned(),
        format!("s <- {}", expression(rng, 2)),
        format!("r <- {}", record(rng)),
        format!("u <- {}", tuple(rng)),
        format!(
            "l <- list({}, {}, {})",
            expression(rng, 1),
            expression(rng, 1),
            expression(rng, 1)
        ),
        format!("g <- {}", tag(rng)),
    ];
    for _ in 0..rng.below(2) + 1 {
        statements.push(match rng.below(10) {
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
            // Values rebuilt every iteration, sometimes in another layout or as a plain list,
            // and read back through `$`, `[[` and `match`.
            6 | 7 => {
                let mut body = Vec::new();
                for _ in 0..rng.below(3) + 1 {
                    body.push(match rng.below(5) {
                        0 => format!("r <- {}", record(rng)),
                        1 => format!("u <- {}", tuple(rng)),
                        2 => format!(
                            "g <- if (i %% {}L == 0L) :Some({}) else {}",
                            rng.below(3) + 1,
                            expression(rng, 1),
                            tag(rng)
                        ),
                        3 => format!(
                            "l[[{}L]] <- {}",
                            rng.below(3) + 1,
                            if rng.chance(1, 6) {
                                "c(1, 2)".to_owned()
                            } else {
                                expression(rng, 1)
                            }
                        ),
                        _ => format!("s <- s + {}", structured(rng, 1)),
                    });
                }
                body.push(format!("s <- s + {}", structured(rng, 1)));
                format!(
                    "for (i in 1:{}) {{ {} }}",
                    rng.below(6) + 1,
                    body.join("; ")
                )
            }
            // A loop-carried variable that alternates between integer and double.
            8 => format!(
                "m <- {}\nfor (i in 1:{}) m <- if (i %% 2L == 0L) m + {} else m * {}\ns <- s + m",
                ["0L", "1", "a", "b"][rng.below(4)],
                rng.below(8) + 1,
                ["1L", "i", "a", "0.5"][rng.below(4)],
                ["1.5", "2L", "b", "i"][rng.below(4)]
            ),
            _ => format!(
                "s <- if ({}) {} else {}",
                condition(rng),
                expression(rng, 2),
                expression(rng, 2)
            ),
        });
    }
    statements.push(
        [
            "s",
            "v",
            "list(s, v, total)",
            "c(s, total)",
            "c(.Internal(typeof(s)), .Internal(typeof(total)))",
            "list(s, r, u, l, g)",
        ][rng.below(6)]
        .to_owned(),
    );
    statements.join("\n")
}

fn expression(rng: &mut SplitMix64, depth: usize) -> String {
    if depth == 0 || rng.below(3) == 0 {
        return leaf(rng);
    }
    match rng.below(8) {
        0 => format!("-({})", expression(rng, depth - 1)),
        1 => format!(
            "(if ({}) {} else {})",
            condition(rng),
            expression(rng, depth - 1),
            expression(rng, depth - 1)
        ),
        2 => format!("v[{}]", ["1L", "2", "i", "length(v)"][rng.below(4)]),
        3 => structured(rng, depth - 1),
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

/// A read of `r`, `u`, `l` or `g`: a field, an item, or a `match` over them or over a number.
fn structured(rng: &mut SplitMix64, depth: usize) -> String {
    match rng.below(9) {
        0 => format!("r${}", ["x", "y"][rng.below(2)]),
        1 => format!("u[[{}]]", ["1L", "2L", "2"][rng.below(3)]),
        2 => format!("l[[{}]]", ["1L", "3L", "(i %% 3L) + 1L", "2"][rng.below(4)]),
        3 | 4 => {
            let mut arms = vec![
                format!("  :Some(z) if (z > {}) => z", leaf(rng)),
                "  :Some(z) => -z".to_owned(),
                "  :Pt(x = px, y = py) => px * py".to_owned(),
                "  :None => 0.5".to_owned(),
                "  :Pt(..) => 2L".to_owned(),
            ];
            shuffle(rng, &mut arms);
            arms.truncate(rng.below(arms.len()) + 1);
            if rng.below(4) != 0 {
                arms.push("  _ => 1L".to_owned());
            }
            format!("(match (g) {{\n{}\n}})", arms.join(",\n"))
        }
        5 => {
            let mut arms = vec![
                "  (z, 1L) => z".to_owned(),
                format!("  (z, w) if (w < {}) => w", leaf(rng)),
                "  (z, w) => z + w".to_owned(),
                "  (x = z, y = w) => z - w".to_owned(),
            ];
            shuffle(rng, &mut arms);
            arms.push("  _ => 3".to_owned());
            format!(
                "(match ({}) {{\n{}\n}})",
                ["u", "r"][rng.below(2)],
                arms.join(",\n")
            )
        }
        6 => {
            let mut arms = vec![
                "  1 => 10".to_owned(),
                "  2L => 20L".to_owned(),
                "  TRUE => 30".to_owned(),
                "  0 | 3 => 0.25".to_owned(),
                format!("  z if (z > {}) => z", leaf(rng)),
            ];
            shuffle(rng, &mut arms);
            arms.push("  _ => 0.5".to_owned());
            format!(
                "(match ({}) {{\n{}\n}})",
                expression(rng, depth),
                arms.join(",\n")
            )
        }
        7 => format!(
            "(match ({}) {{ (x = z, y = w) => z * w, _ => 0 }})",
            record(rng)
        ),
        _ => format!(
            "(match ({}) {{ :Pt(x = z, y = w) => z + w, :Some(z) => z, _ => -1L }})",
            tag(rng)
        ),
    }
}

/// A record with fields `x` and `y`, in either order, or sometimes a list with those names.
fn record(rng: &mut SplitMix64) -> String {
    let (x, y) = (expression(rng, 1), expression(rng, 1));
    match rng.below(6) {
        0 => format!("(y = {y}, x = {x})"),
        1 => format!("list(x = {x}, y = {y})"),
        2 => format!("(x = {x}, y = {y}, z = v)"),
        _ => format!("(x = {x}, y = {y})"),
    }
}

/// A pair, sometimes as a plain list.
fn tuple(rng: &mut SplitMix64) -> String {
    let (first, second) = (expression(rng, 1), expression(rng, 1));
    if rng.chance(1, 6) {
        format!("list({first}, {second})")
    } else {
        format!("({first}, {second})")
    }
}

fn tag(rng: &mut SplitMix64) -> String {
    match rng.below(3) {
        0 => format!(":Some({})", expression(rng, 1)),
        1 => ":None".to_owned(),
        _ => format!(
            ":Pt(x = {}, y = {})",
            expression(rng, 1),
            expression(rng, 1)
        ),
    }
}

fn shuffle<T>(rng: &mut SplitMix64, items: &mut [T]) {
    for index in (1..items.len()).rev() {
        items.swap(index, rng.below(index + 1));
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

/// A scalar of the stdlib fuzz model: `None` is `NA`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Scalar {
    Int(Option<i32>),
    Dbl(Option<f64>),
    Lgl(Option<bool>),
}

/// What a callback-taking library call gives in the model: a value, or an error.
#[derive(Clone, PartialEq, Debug)]
enum Model {
    Scalar(Scalar),
    List(Vec<Scalar>),
    Found(Option<Scalar>),
    /// A count or a position, which the library gives as an `I64`.
    Whole(i64),
    FoundWhole(i64),
    Error,
}

#[test]
fn fuzz_stdlib_traversals_agree_with_model() {
    // Random lists (integer, double, mixed, with `NA` and values near the integer limit) through
    // every library traversal the JIT compiles into a loop, each checked against a model of the
    // library in Rust. Tier 0 and compiled code must both give the model's value, so a hand-off
    // from the loop (an `NA` answer, an overflow, an element of another type, a collection that
    // is not one, a list, which fails the loop's type guard as an ALTREP list does) must still
    // end where the model says.
    with_interpreter(|| {
        let (mut runs, mut errors) = (0, 0);
        for_each_seed(300, |seed| {
            let mut rng = SplitMix64(seed);
            let (body, model) = stdlib_case(&mut rng);
            // The model's value is rendered as R source, and what that evaluates to is the
            // value both tiers must give.
            let wanted = match &model {
                Model::Error => "error".to_owned(),
                // SAFETY: the interpreter is installed.
                value => unsafe { run(&render(value)) },
            };
            runs += 1;
            errors += usize::from(model == Model::Error);
            // SAFETY: the interpreter is installed.
            let (reference, compiled) = unsafe { outcomes(4_000_000 + seed as usize, &body, "()") };
            // The model knows values only, so tier 0's warnings are left out of that
            // comparison; compiled code must match tier 0 warnings included.
            let value: Vec<&str> = reference
                .lines()
                .filter(|line| !line.starts_with("warning: "))
                .collect();
            let value = value.join("\n");
            // The model says that a call fails, not with which message.
            let value = if value.starts_with("error") {
                "error".to_owned()
            } else {
                value
            };
            assert_eq!(value, wanted, "tier 0 on {body}");
            for (run, outcome) in compiled.iter().enumerate() {
                assert_eq!(outcome, &reference, "compiled call {} on {body}", run + 1);
            }
        });
        // Cases that expect an error compare less, so most must give a value. A replay of one
        // seed says nothing about the share.
        eprintln!("{errors} of {runs} stdlib cases expected an error");
        assert!(
            runs < 50 || errors * 2 < runs,
            "{errors} of {runs} stdlib cases expected an error"
        );
    });
}

/// A random library traversal call and what the model says it gives.
fn stdlib_case(rng: &mut SplitMix64) -> (String, Model) {
    let items = random_items(rng);
    let all_int = items.iter().all(|item| matches!(item, Scalar::Int(_)));
    let has_na = items.iter().any(|&item| is_na(item));
    let list = if rng.chance(1, 25) {
        // Not a list: the library's own argument error.
        let rendered: Vec<String> = items.iter().map(|&item| scalar(item)).collect();
        return (
            format!(
                "List.walk(c({}), 0, function(acc, x) acc + x)",
                rendered.join(", ")
            ),
            Model::Error,
        );
    } else {
        render(&Model::List(items.clone()))
    };
    let initial = if rng.chance(1, 2) {
        Scalar::Int(Some(0))
    } else {
        Scalar::Dbl(Some(0.0))
    };
    let threshold = if rng.chance(1, 2) {
        Scalar::Int(Some(rng.below(40) as i32))
    } else {
        Scalar::Dbl(Some(rng.below(160) as f64 / 4.0 - 10.0))
    };
    let (init, limit) = (scalar(initial), scalar(threshold));
    let forwards: Vec<usize> = (0..items.len()).collect();
    let backwards: Vec<usize> = (0..items.len()).rev().collect();
    let add = |acc: Scalar, x: Scalar| arith(acc, x, '+');
    let array = homogeneous_array(rng, &items);
    let (call, model) = match rng.below(14) {
        0 => {
            let (name, order) = if rng.chance(1, 2) {
                ("walk", &forwards)
            } else {
                ("walk_backwards", &backwards)
            };
            if rng.chance(1, 2) {
                let model = fold(&items, order, initial, |acc, x, _| Ok(add(acc, x)));
                (
                    format!("List.{name}({list}, {init}, function(acc, x) acc + x)"),
                    model,
                )
            } else {
                let model = fold(&items, order, initial, |acc, x, _| {
                    Ok(add(arith(acc, Scalar::Dbl(Some(0.5)), '*'), x))
                });
                (
                    format!("List.{name}({list}, {init}, function(acc, x) acc * 0.5 + x)"),
                    model,
                )
            }
        }
        1 => {
            let model = fold(&items, &forwards, initial, |acc, x, i| {
                Ok(add(acc, arith(x, Scalar::Int(Some(i as i32 + 1)), '*')))
            });
            (
                format!(
                    "List.walk_with_index({list}, {init}, function(acc, x, i) acc + x * Num.to_r(i))"
                ),
                model,
            )
        }
        2 => {
            let (start, text) = random_start(rng, items.len());
            let model = match start {
                Some(start) => fold(
                    &items,
                    &forwards[start.min(items.len())..],
                    initial,
                    |acc, x, _| Ok(add(acc, x)),
                ),
                None => Model::Error,
            };
            (
                format!("List.walk_from({list}, {text}, {init}, function(acc, x) acc + x)"),
                model,
            )
        }
        3 => {
            let (name, order, start) = match rng.below(3) {
                0 => ("walk_until".to_owned(), forwards.clone(), String::new()),
                1 => (
                    "walk_backwards_until".to_owned(),
                    backwards.clone(),
                    String::new(),
                ),
                _ => {
                    let (start, text) = random_start(rng, items.len());
                    match start {
                        Some(start) => (
                            "walk_from_until".to_owned(),
                            forwards[start.min(items.len())..].to_vec(),
                            format!("{text}, "),
                        ),
                        None => {
                            return (
                                format!(
                                    "List.walk_from_until({list}, {text}, {init}, function(acc, x) :Continue(acc + x))"
                                ),
                                Model::Error,
                            );
                        }
                    }
                }
            };
            let model = walk_until(&items, &order, initial, |acc, x, _| {
                Ok(match truth(compare(acc, threshold))? {
                    true => (true, acc),
                    false => (false, add(acc, x)),
                })
            });
            (
                format!(
                    "List.{name}({list}, {start}{init}, function(acc, x) if (acc > {limit}) :Break(acc) else :Continue(acc + x))"
                ),
                model,
            )
        }
        4 => {
            let model = walk_until(&items, &forwards, initial, |acc, x, i| {
                Ok(match i as f64 + 1.0 > number(threshold) {
                    true => (true, acc),
                    false => (false, add(acc, x)),
                })
            });
            (
                format!(
                    "List.walk_with_index_until({list}, {init}, function(acc, x, i) {{ if (Num.to_r(i) > {limit}) :Break(acc) else :Continue(acc + x) }})"
                ),
                model,
            )
        }
        5 => {
            let (text, mapped): (String, Result<Vec<Scalar>, ()>) = match rng.below(4) {
                0 => (
                    format!("x * {limit}"),
                    Ok(items.iter().map(|&x| arith(x, threshold, '*')).collect()),
                ),
                1 => (
                    format!("x + {limit}"),
                    Ok(items.iter().map(|&x| arith(x, threshold, '+')).collect()),
                ),
                2 => (
                    format!("x > {limit}"),
                    Ok(items.iter().map(|&x| compare(x, threshold)).collect()),
                ),
                _ => (
                    format!("if (x > {limit}) x else {init}"),
                    items
                        .iter()
                        .map(|&x| {
                            Ok(if truth(compare(x, threshold))? {
                                x
                            } else {
                                initial
                            })
                        })
                        .collect(),
                ),
            };
            let model = mapped.map_or(Model::Error, Model::List);
            (format!("List.map({list}, function(x) {text})"), model)
        }
        6 => {
            let mapped = items
                .iter()
                .enumerate()
                .map(|(i, &x)| arith(x, Scalar::Int(Some(i as i32 + 1)), '*'))
                .collect();
            (
                format!("List.map_with_index({list}, function(x, i) x * Num.to_r(i))"),
                Model::List(mapped),
            )
        }
        7 if all_int && !has_na && !items.is_empty() => {
            // Integer and double keys are one key type, so the keys are distinct integers and
            // the values the list's elements.
            let mut dict = "Dict.empty()".to_owned();
            for (at, &value) in items.iter().enumerate() {
                dict = format!("Dict.insert({dict}, {}L, {})", at + 1, scalar(value));
            }
            let model = fold(&items, &forwards, initial, |acc, x, i| {
                Ok(add(acc, arith(Scalar::Int(Some(i as i32 + 1)), x, '*')))
            });
            (
                format!("Dict.walk({dict}, {init}, function(acc, k, v) acc + k * v)"),
                model,
            )
        }
        8 if !has_na && (all_int || items.iter().all(|item| matches!(item, Scalar::Dbl(_)))) => {
            let mut distinct: Vec<Scalar> = Vec::new();
            for &item in &items {
                if !distinct.contains(&item) {
                    distinct.push(item);
                }
            }
            let order: Vec<usize> = (0..distinct.len()).collect();
            let model = walk_until(&distinct, &order, initial, |acc, x, _| {
                Ok(match truth(compare(acc, threshold))? {
                    true => (true, acc),
                    false => (false, add(acc, x)),
                })
            });
            (
                format!(
                    "Set.walk_until(Set.from_list({list}), {init}, function(acc, x) if (acc > {limit}) :Break(acc) else :Continue(acc + x))"
                ),
                model,
            )
        }
        _ => {
            let modulus = rng.below(5) + 2;
            let remainder = rng.below(modulus);
            let (text, predicate): (String, Box<dyn Fn(Scalar) -> Scalar>) =
                if all_int && rng.chance(1, 2) {
                    (
                        format!("x %% {modulus}L == {remainder}L"),
                        Box::new(move |x| match x {
                            Scalar::Int(Some(x)) => {
                                Scalar::Lgl(Some(x.rem_euclid(modulus as i32) == remainder as i32))
                            }
                            _ => Scalar::Lgl(None),
                        }),
                    )
                } else {
                    (
                        format!("x > {limit}"),
                        Box::new(move |x| compare(x, threshold)),
                    )
                };
            let answers = |order: &[usize]| -> Vec<(usize, Scalar)> {
                order.iter().map(|&at| (at, predicate(items[at]))).collect()
            };
            let (name, model) = match rng.below(9) {
                0 | 1 => {
                    let keep = rng.chance(1, 2);
                    let mut kept = Vec::new();
                    let mut model = None;
                    for (at, answer) in answers(&forwards) {
                        match truth(answer) {
                            Ok(passes) if passes == keep => kept.push(items[at]),
                            Ok(_) => {}
                            Err(()) => model = Some(Model::Error),
                        }
                        if model.is_some() {
                            break;
                        }
                    }
                    let name = if keep { "keep_if" } else { "drop_if" };
                    (name, model.unwrap_or(Model::List(kept)))
                }
                2 => (
                    "any",
                    search(
                        &answers(&forwards),
                        true,
                        |_| Model::Scalar(Scalar::Lgl(Some(true))),
                        Model::Scalar(Scalar::Lgl(Some(false))),
                    ),
                ),
                3 => (
                    "all",
                    search(
                        &answers(&forwards),
                        false,
                        |_| Model::Scalar(Scalar::Lgl(Some(false))),
                        Model::Scalar(Scalar::Lgl(Some(true))),
                    ),
                ),
                4 => {
                    let mut count = 0;
                    let mut model = None;
                    for (_, answer) in answers(&forwards) {
                        match truth(answer) {
                            Ok(passes) => count += i32::from(passes),
                            Err(()) => {
                                model = Some(Model::Error);
                                break;
                            }
                        }
                    }
                    ("count_if", model.unwrap_or(Model::Whole(i64::from(count))))
                }
                choice => {
                    let last = choice >= 7;
                    let index = choice % 2 == 0;
                    let order = if last { &backwards } else { &forwards };
                    let model = search(
                        &answers(order),
                        true,
                        |at| match index {
                            true => Model::FoundWhole(at as i64 + 1),
                            false => Model::Found(Some(items[at])),
                        },
                        Model::Found(None),
                    );
                    let name = match (last, index) {
                        (false, false) => "find_first",
                        (false, true) => "find_first_index",
                        (true, false) => "find_last",
                        (true, true) => "find_last_index",
                    };
                    (name, model)
                }
            };
            (format!("List.{name}({list}, function(x) {text})"), model)
        }
    };
    // An Array holds the same items, but the compiled loop must hand it back to tier 0, the
    // path an ALTREP list takes too. A traversal that returns a collection would return an
    // Array, which the model does not describe, so only the other ones use it.
    match array {
        Some(array) if call.starts_with("List.") && !matches!(model, Model::List(_)) => {
            (call.replacen(&list, &array, 1), model)
        }
        _ => (call, model),
    }
}

/// The items as a list literal, now and then, when they share one type and none is `NA`.
fn homogeneous_array(rng: &mut SplitMix64, items: &[Scalar]) -> Option<String> {
    // An Array holds no `NA`, so a list with one stays a list.
    let all_int = items
        .iter()
        .all(|item| matches!(item, Scalar::Int(Some(_))));
    let all_dbl = items
        .iter()
        .all(|item| matches!(item, Scalar::Dbl(Some(_))));
    if items.is_empty() || !(all_int || all_dbl) || !rng.chance(1, 4) {
        return None;
    }
    let rendered: Vec<String> = items.iter().map(|&item| scalar(item)).collect();
    Some(format!("[{}]", rendered.join(", ")))
}

/// Up to 24 numbers: integers, doubles or a mix, now and then `NA` or an integer near the limit,
/// which overflows when added.
fn random_items(rng: &mut SplitMix64) -> Vec<Scalar> {
    let length = rng.below(25);
    let kind = rng.below(3);
    let with_na = rng.chance(1, 4);
    let large = rng.chance(1, 8);
    (0..length)
        .map(|_| {
            let int = kind == 0 || (kind == 2 && rng.chance(1, 2));
            if with_na && rng.chance(1, 8) {
                return if int {
                    Scalar::Int(None)
                } else {
                    Scalar::Dbl(None)
                };
            }
            match int {
                true if large && rng.chance(1, 3) => {
                    Scalar::Int(Some(i32::MAX - rng.below(100) as i32))
                }
                true => Scalar::Int(Some(rng.below(80) as i32 - 20)),
                false => Scalar::Dbl(Some(rng.below(320) as f64 / 4.0 - 20.0)),
            }
        })
        .collect()
}

/// A `walk_from` start position: a whole number, or now and then one the library refuses.
fn random_start(rng: &mut SplitMix64, length: usize) -> (Option<usize>, String) {
    let start = rng.below(length + 4);
    match rng.below(6) {
        0 => (None, format!("{start}.5")),
        1 => (
            start.checked_sub(1).filter(|_| start > 0),
            format!("{start}L"),
        ),
        2 => (Some(start), format!("{}", start + 1)),
        _ => (Some(start), format!("{}L", start + 1)),
    }
}

fn fold(
    items: &[Scalar],
    order: &[usize],
    initial: Scalar,
    step: impl Fn(Scalar, Scalar, usize) -> Result<Scalar, ()>,
) -> Model {
    let mut state = initial;
    for &at in order {
        match step(state, items[at], at) {
            Ok(next) => state = next,
            Err(()) => return Model::Error,
        }
    }
    Model::Scalar(state)
}

fn walk_until(
    items: &[Scalar],
    order: &[usize],
    initial: Scalar,
    step: impl Fn(Scalar, Scalar, usize) -> Result<(bool, Scalar), ()>,
) -> Model {
    let mut state = initial;
    for &at in order {
        match step(state, items[at], at) {
            Ok((broke, next)) => {
                state = next;
                if broke {
                    break;
                }
            }
            Err(()) => return Model::Error,
        }
    }
    Model::Scalar(state)
}

/// Stops at the first answer equal to `stop`, an `NA` answer being an error.
fn search(
    answers: &[(usize, Scalar)],
    stop: bool,
    stopped: impl Fn(usize) -> Model,
    otherwise: Model,
) -> Model {
    for &(at, answer) in answers {
        match truth(answer) {
            Ok(passes) if passes == stop => return stopped(at),
            Ok(_) => {}
            Err(()) => return Model::Error,
        }
    }
    otherwise
}

fn is_na(value: Scalar) -> bool {
    matches!(
        value,
        Scalar::Int(None) | Scalar::Dbl(None) | Scalar::Lgl(None)
    )
}

/// A number as a double, `NaN` for `NA`.
fn number(value: Scalar) -> f64 {
    match value {
        Scalar::Int(Some(x)) => f64::from(x),
        Scalar::Dbl(Some(x)) => x,
        Scalar::Lgl(Some(x)) => f64::from(u8::from(x)),
        _ => f64::NAN,
    }
}

/// R's `+`, `-` and `*`: integer when both sides are, with overflow giving `NA`.
fn arith(left: Scalar, right: Scalar, operator: char) -> Scalar {
    match (left, right) {
        (Scalar::Int(a), Scalar::Int(b)) => Scalar::Int(a.zip(b).and_then(|(a, b)| {
            let value = match operator {
                '+' => a.checked_add(b),
                '-' => a.checked_sub(b),
                _ => a.checked_mul(b),
            };
            value.filter(|&value| value != i32::MIN)
        })),
        _ if is_na(left) || is_na(right) => Scalar::Dbl(None),
        _ => {
            let (a, b) = (number(left), number(right));
            Scalar::Dbl(Some(match operator {
                '+' => a + b,
                '-' => a - b,
                _ => a * b,
            }))
        }
    }
}

fn compare(left: Scalar, right: Scalar) -> Scalar {
    if is_na(left) || is_na(right) {
        return Scalar::Lgl(None);
    }
    Scalar::Lgl(Some(number(left) > number(right)))
}

/// A condition's value, an `NA` one being an error.
fn truth(value: Scalar) -> Result<bool, ()> {
    match value {
        Scalar::Lgl(Some(flag)) => Ok(flag),
        _ => Err(()),
    }
}

fn scalar(value: Scalar) -> String {
    match value {
        Scalar::Int(Some(x)) => format!("{x}L"),
        Scalar::Int(None) => "NA_integer_".to_owned(),
        Scalar::Dbl(Some(x)) => format!("{x:?}"),
        Scalar::Dbl(None) => "NA_real_".to_owned(),
        Scalar::Lgl(Some(true)) => "TRUE".to_owned(),
        Scalar::Lgl(Some(false)) => "FALSE".to_owned(),
        Scalar::Lgl(None) => "NA".to_owned(),
    }
}

fn render(model: &Model) -> String {
    match model {
        Model::Scalar(value) => scalar(*value),
        Model::List(items) => {
            let rendered: Vec<String> = items.iter().map(|&item| scalar(item)).collect();
            format!("list({})", rendered.join(", "))
        }
        Model::Found(Some(value)) => format!(":Ok({})", scalar(*value)),
        Model::Found(None) => ":Err(:NotFound)".to_owned(),
        Model::Whole(value) => format!("{value}i64"),
        Model::FoundWhole(value) => format!(":Ok({value}i64)"),
        Model::Error => "stop(\"the model fails\")".to_owned(),
    }
}

/// Typed closures over records, tuples and tags run as native functions once the JIT is on:
/// construction, field reads, `match` and recursion over a tagged union. Each program must give
/// the same values and errors with the JIT off, on, and forced, including where a field holds
/// `NA`, an integer overflows, or no arm matches.
#[test]
fn fuzz_typed_record_natives_agree_with_tier_zero() {
    with_interpreter(|| {
        let mut runs = 0;
        for_each_seed(150, |seed| {
            let mut rng = SplitMix64(seed);
            let program = record_program(&mut rng);
            let calls: Vec<String> = (0..5)
                .map(|_| {
                    let n = rng.below(12);
                    let x = ["1.5", "-2", "0", "NA_real_", "3.25", "1e308"][rng.below(6)];
                    let k = ["3L", "0L", "NA_integer_", "2147483647L", "-4L"][rng.below(5)];
                    format!("({n}L, {x}, {k})")
                })
                .collect();
            let modes = [Mode::Off, Mode::On, Mode::Force];
            // SAFETY: the interpreter is installed.
            let outcomes: Vec<Vec<String>> = unsafe {
                modes
                    .iter()
                    .enumerate()
                    .map(|(index, &mode)| {
                        jit::install_mode(mode);
                        let suffix = format!("_{seed}_{index}");
                        let text = program.replace('@', &suffix);
                        run(&text);
                        let outcomes = calls
                            .iter()
                            .map(|call| run(&format!("total{suffix}{call}")))
                            .collect();
                        jit::install_mode(Mode::Off);
                        outcomes
                    })
                    .collect()
            };
            runs += 1;
            for (mode, outcome) in modes.iter().zip(&outcomes).skip(1) {
                for (index, (expected, got)) in outcomes[0].iter().zip(outcome).enumerate() {
                    assert_eq!(
                        expected, got,
                        "seed {seed}, {mode:?}, call total{}:\n{program}",
                        calls[index]
                    );
                }
            }
        });
        assert!(runs > 0);
    });
}

/// A program of typed functions over a record, a tagged union and a recursive tree, ending in
/// `total@(n, x, k)`, with `@` standing for a suffix that keeps each run's functions apart.
fn record_program(rng: &mut SplitMix64) -> String {
    let scalar = |rng: &mut SplitMix64| {
        [
            "x",
            "x * 2",
            "x - i",
            "i * k",
            "x / i",
            "max(x, i)",
            "min(k, i)",
            "abs(x) + 1",
            "k + 0.5",
            "i %/% 2L + k",
            "-x",
        ][rng.below(11)]
        .to_owned()
    };
    let mut arms = vec![
        format!(
            "    :Circle(r = r) => r * {}",
            ["r", "2", "x"][rng.below(3)]
        ),
        "    :Rect(w = w, h = h) => w * h".to_owned(),
        format!("    :Dot => {}", ["0", "x", "1L"][rng.below(3)]),
        "    :Pair(a = a, b = b) => a - b".to_owned(),
    ];
    shuffle(rng, &mut arms);
    arms.truncate(rng.below(arms.len()) + 1);
    if rng.chance(1, 2) {
        arms.push(format!(
            "    _ => {}",
            ["-1", "x", "NA_real_"][rng.below(3)]
        ));
    }
    let point_fields = if rng.chance(1, 2) {
        format!("(x = {}, y = {})", scalar(rng), scalar(rng))
    } else {
        format!("(y = {}, x = {})", scalar(rng), scalar(rng))
    };
    let accumulate = match rng.below(4) {
        0 => "acc <- acc + area@(s, x)",
        1 => "acc <- acc + area@(s, x) + p$x * p$y",
        2 => "acc <- acc + p[[\"y\"]] + depth@(tree@(i %% 4L, x))",
        _ => "acc <- acc + max(area@(s, x), p$x) + sum@(tree@(i %% 3L, x))",
    };
    format!(
        "shape@ <- function(i: integer, x: double, k: integer) {{
  m <- i %% 5L
  if (m == 0L) :Circle(r = {circle}) else if (m == 1L) :Rect(w = {width}, h = {height}) else if (m == 2L) :Dot else if (m == 3L) :Pair(a = {left}, b = {right}) else :Other(v = i)
}}
area@ <- function(s, x: double) -> double {{
  match (s) {{
{arms}
  }}
}}
point@ <- function(i: integer, x: double, k: integer) {point_fields}
tree@ <- function(d: integer, x: double) {{
  if (d <= 0L) return(:Leaf(value = x))
  :Node(left = tree@(d - 1L, x + 1), right = tree@(d - 2L, x * 2))
}}
depth@ <- function(t) -> integer {{
  match (t) {{
    :Leaf(value = v) => 1L
    :Node(left = l, right = r) => max(depth@(l), depth@(r)) + 1L
  }}
}}
sum@ <- function(t) -> double {{
  match (t) {{
    :Leaf(value = v) => v
    :Node(left = l, right = r) => sum@(l) + sum@(r)
  }}
}}
total@ <- function(n: integer, x: double, k: integer) -> double {{
  acc <- 0
  for (i in seq_len(n)) {{
    s <- shape@(i, x, k)
    p <- point@(i, x, k)
    {accumulate}
  }}
  acc
}}",
        circle = scalar(rng),
        width = scalar(rng),
        height = scalar(rng),
        left = scalar(rng),
        right = scalar(rng),
        arms = arms.join("\n"),
    )
}
