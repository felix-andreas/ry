//! The config loader and the position conversions under their exported contracts
//! (`ry::config::check_config_invariants`, `ry::position::check_position_invariants`), over
//! seeds, mutations of them, and random bytes. `FUZZ_ITERS` scales the budget.

use ry::config::check_config_invariants;
use ry::position::check_position_invariants;
use syntax::testing::{SplitMix64, fuzz_iterations, mutate_seed, random_bytes, token_soup};

const CONFIG_SEEDS: &[&str] = &[
    "",
    "[format]\nindent-width = 4\nline-width = 100\n",
    "[check]\ntyping = true\nstrict = true\nexclude = [\"scripts/\", \"!scripts/keep.R\"]\n",
    "[lint]\nnaming-style = \"snake_case\"\n",
    "case = \"camelCase\"\nspaces = 2\n",
    "typing = true\n",
    "[check]\ninline-annotations = false\nobject-systems = false\n",
    "[format]\nindent-width = \"four\"\n",
    "[check\nexclude = [\"[\"]\n",
    "a.b.c = 1\n[x.y]\nz = { w = [1, 2] }\n",
];

const CONFIG_ALPHABET: &[&str] = &[
    "[",
    "]",
    "[[",
    "]]",
    "=",
    "\"",
    "'",
    "\n",
    "\r\n",
    "#",
    ".",
    ",",
    "{",
    "}",
    "format",
    "lint",
    "check",
    "exclude",
    "typing",
    "strict",
    "indent-width",
    "line-width",
    "true",
    "false",
    "1",
    "-1",
    "1e400",
    "\"**\"",
    "\"[\"",
    "\"!\"",
    "é",
    "😀",
];

const POSITION_SEEDS: &[&str] = &[
    "",
    "\n",
    "abc\ndef\n",
    "x <- \"café\" ; y = 2L\n",
    "é😀x\n",
    "a\r\nb\r\n",
    "\r\r\n\n",
    "😀\n😀😀\r\n\u{feff}z",
];

/// Inputs the coverage-guided `config` and `position` targets have broken.
const CONFIG_REGRESSIONS: &[&str] = &[];
const POSITION_REGRESSIONS: &[&str] = &[];

#[test]
fn config_seeds_hold_invariants() {
    for input in CONFIG_SEEDS.iter().chain(CONFIG_REGRESSIONS) {
        check_config_invariants(input);
    }
}

#[test]
fn fuzz_config() {
    let mut rng = SplitMix64(0xc0f1_9000);
    for _ in 0..fuzz_iterations(2000) {
        let input = match rng.below(3) {
            0 => random_bytes(&mut rng),
            1 => token_soup(&mut rng, CONFIG_ALPHABET),
            _ => mutate_seed(&mut rng, CONFIG_SEEDS, 24),
        };
        check_config_invariants(&input);
    }
}

#[test]
fn position_seeds_hold_invariants() {
    for input in POSITION_SEEDS.iter().chain(POSITION_REGRESSIONS) {
        check_position_invariants(input);
    }
}

#[test]
fn fuzz_positions() {
    let mut rng = SplitMix64(0x0ff5_e7);
    for _ in 0..fuzz_iterations(2000) {
        let input = match rng.below(2) {
            0 => random_bytes(&mut rng),
            _ => mutate_seed(&mut rng, POSITION_SEEDS, 8),
        };
        check_position_invariants(&input);
    }
}
