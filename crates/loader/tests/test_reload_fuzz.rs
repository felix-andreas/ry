//! Reload property and index robustness under the contracts in `loader::testing`. Packages and
//! edits are random; `FUZZ_ITERS` scales the budget.

use loader::testing::{check_load_index_input, check_random_reload};
use syntax::testing::{SplitMix64, fuzz_iterations, mutate_seed, random_bytes};

#[test]
fn a_reload_matches_a_fresh_load() {
    for seed in 0..fuzz_iterations(400) as u64 {
        check_random_reload(&mut SplitMix64(seed));
    }
}

#[test]
fn arbitrary_text_never_breaks_the_index() {
    let seeds = [
        "f <- function(x: int) -> int { x }\n",
        "setMethod(\"show\", \"A\", function(object) cat(\"é\"))\n",
        "x <- 1; y <- function() x\nif (TRUE) z <- 2\n",
    ];
    for seed in 0..fuzz_iterations(300) as u64 {
        let mut rng = SplitMix64(seed);
        let text = match rng.below(3) {
            0 => random_bytes(&mut rng),
            _ => mutate_seed(&mut rng, &seeds, 24),
        };
        check_load_index_input(&text);
    }
}
