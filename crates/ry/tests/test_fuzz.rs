//! The config loader, the position conversions and the reader of an executable's payload under
//! their exported contracts (`ry::config::check_config_invariants`,
//! `ry::position::check_position_invariants`, `ry::executable::testing`), over seeds, mutations
//! of them, and random bytes. Each generated case is one seed of
//! [`for_each_seed`], so `FUZZ_SEED` replays a failure and `FUZZ_ITERS` sets the budget.

use {
    ry::{
        config::check_config_invariants,
        executable::{self, testing as executable_testing},
        position::check_position_invariants,
    },
    syntax::testing::{SplitMix64, for_each_seed, mutate_seed, random_bytes, token_soup},
};

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
    for_each_seed(2000, |seed| {
        let mut rng = SplitMix64(0xc0f1_9000 ^ seed);
        let input = match rng.below(3) {
            0 => random_bytes(&mut rng),
            1 => token_soup(&mut rng, CONFIG_ALPHABET),
            _ => mutate_seed(&mut rng, CONFIG_SEEDS, 24),
        };
        check_config_invariants(&input);
    });
}

#[test]
fn position_seeds_hold_invariants() {
    for input in POSITION_SEEDS.iter().chain(POSITION_REGRESSIONS) {
        check_position_invariants(input);
    }
}

#[test]
fn fuzz_positions() {
    for_each_seed(2000, |seed| {
        let mut rng = SplitMix64(0x000f_f5e7 ^ seed);
        let input = match rng.below(2) {
            0 => random_bytes(&mut rng),
            _ => mutate_seed(&mut rng, POSITION_SEEDS, 8),
        };
        check_position_invariants(&input);
    });
}

#[test]
fn fuzz_executable_payloads() {
    for_each_seed(2000, |seed| {
        let mut rng = SplitMix64(0x0e8e_c0de ^ seed);
        let bytes = random_bytes(&mut rng).into_bytes();
        executable_testing::check_round_trip(&bytes);
        // A payload with some of its bytes changed: the reader must find it damaged, find none,
        // or find one that lies inside the file, and never panic.
        let mut file = bytes.clone();
        let metadata = executable::Metadata {
            flags: 64,
            script: String::from("main.R"),
            source: bytes.clone(),
            runtime: None,
            base: None,
        };
        executable::append(&mut file, bytes.len() as u64, &bytes, &metadata);
        let tail = file.len().saturating_sub(96);
        for _ in 0..=rng.below(4) {
            let at = tail + rng.below(file.len() - tail);
            if let Some(byte) = file.get_mut(at) {
                *byte = rng.next_u64() as u8;
            }
        }
        executable_testing::check_parse_input(&file);
        executable_testing::check_parse_input(&bytes);
    });
}
