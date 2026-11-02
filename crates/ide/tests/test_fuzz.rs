//! Every feature runs at sampled offsets under the contract in `ide::testing`. `FUZZ_ITERS`
//! scales the mutation budget.

use {
    ide::testing::{check_ide_invariants as sweep, check_refactor_invariants},
    syntax::testing::{SplitMix64, fuzz_iterations, mutate_seed},
};

const SEEDS: &[&str] = &[
    "x <- 1L\ny <- x + 1\n",
    "f <- function(a, b = 2) a + b\ng <- function() f(1)\n",
    "#: fn(x: integer) -> double\nhalf <- function(x) x / 2\n",
    "make <- function() {\n  total <- 0\n  add <- function(n) total <<- total + n\n  add\n}\n",
    "lst <- list(a = 1, b = \"s\")\nvalue <- lst$a\n",
    "for (i in 1:10) print(i)\n",
    "if (x > 1) y <- 2 else y <- 3\n",
    "`odd name` <- 5\nuse <- `odd name`\n",
    "broken <- function( {\n",
    "s <- \"unterminated\n",
];

/// Inputs the coverage-guided `ide` target has broken; only the invariants must hold.
const REGRESSIONS: &[&str] = &[];

#[test]
fn seeds_never_panic() {
    for seed in SEEDS {
        sweep(seed);
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        sweep(input);
    }
}

#[test]
fn fuzz_seed_mutations() {
    let mut rng = SplitMix64(0x01DE_F022);
    for _ in 0..fuzz_iterations(300) {
        sweep(&mutate_seed(&mut rng, SEEDS, 24));
    }
}

/// `FUZZ_ITERS=5000 cargo test -p ide --release --test test_fuzz -- --ignored fuzz_deep`
#[test]
#[ignore = "deep fuzz; run explicitly with a large FUZZ_ITERS"]
fn fuzz_deep() {
    let mut rng = SplitMix64(0x01DE_F023);
    for _ in 0..fuzz_iterations(300).max(5000) {
        let pieces = 1 + rng.below(12);
        let mut text = String::new();
        for _ in 0..pieces {
            text.push_str(SEEDS[rng.below(SEEDS.len())]);
        }
        sweep(&text);
    }
}

const REFACTOR_SEEDS: &[&str] = &[
    "f <- function(a, b) {\n  total <- sum(a * b, na.rm = TRUE)\n  total / length(a)\n}\n",
    "summarise <- function(values, weights) {\n  scaled <- values * weights\n  total <- sum(scaled)\n  total / length(values)\n}\n",
    "#: fn(price: double, [rate]: double) -> double\ndiscount <- function(price, rate = 0.1) {\n  price * (1 - rate)\n}\n",
    "discount <- function(price: double, rate: double = 0.1) -> double price * (1 - rate)\n",
    "#: <T> fn(x: T) -> T\nidentity2 <- function(x) x\ncount: integer <- 0L\n",
    "f <- function(xs) {\n  limit <- 10\n  out <- c()\n  for (x in xs) if (x > limit) out <- c(out, x)\n  out\n}\n",
    "g <- function(x) {\n  if (x < 0) return(NA)\n  y <- sqrt(x)\n  z <- y + 1\n  y * z\n}\n",
    "make <- function() {\n  total <- 0\n  add <- function(n) total <<- total + n\n  add\n}\n",
    "rate <- 0.05\ngrow <- function(x, years) x * (1 + rate)^years\nprint(grow(100, 2))\n",
    "h <- function(df) {\n  n <- nrow(df)\n  m <- quote(a + b)\n  ok <- is.null(df) || n == 0\n  df |> head(n)\n}\n",
];

#[test]
fn refactor_seeds_keep_code_valid() {
    for seed in REFACTOR_SEEDS {
        check_refactor_invariants(seed);
    }
}

#[test]
fn refactor_seed_mutations_keep_code_valid() {
    let mut rng = SplitMix64(0x0EF0_C7A1);
    for _ in 0..fuzz_iterations(40) {
        check_refactor_invariants(&mutate_seed(&mut rng, REFACTOR_SEEDS, 8));
    }
}
