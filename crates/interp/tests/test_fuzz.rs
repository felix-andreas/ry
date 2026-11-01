//! Fuzz and property coverage for the interpreter's stages, run without base R or libR:
//!
//! - lowering never panics on mutated R source, and a lowered expression deparses to text that
//!   lowers back to an identical expression;
//! - the tier-0 bytecode agrees with the AST evaluator on random arithmetic, comparison, logic,
//!   branch and loop programs, including NA, integer overflow and errors;
//! - reading a corrupted serialized stream fails with an R error, never a panic.
//!
//! The contracts live in `interp::testing`, shared with the coverage-guided `fuzz/` targets.
//!
//! `FUZZ_ITERS` raises the budget.

use interp::testing::{
    check_bytecode_agreement, check_lowering_input, check_unserialize_input, with_interpreter,
};
use interp::{deparse, lower, values};
use syntax::testing::{SplitMix64, fuzz_iterations, mutate_bytes};

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
    let mut rng = SplitMix64(0x5eed_1a7e);
    for _ in 0..fuzz_iterations(400) {
        let seed = SEEDS[rng.below(SEEDS.len())];
        check_lowering_input(&String::from_utf8_lossy(&mutate_bytes(&mut rng, seed)));
    }
}

/// Mutated streams of serialized seeds must read or fail with an R error, never panic.
#[test]
fn fuzz_unserialize_never_panics() {
    let seeds = serialized_seeds();
    assert!(!seeds.is_empty(), "no serialized seeds");
    let mut rng = SplitMix64(0x5e71_a112);
    for _ in 0..fuzz_iterations(400) {
        let mut bytes = seeds[rng.below(seeds.len())].clone();
        for _ in 0..1 + rng.below(4) {
            if bytes.is_empty() {
                break;
            }
            let at = rng.below(bytes.len());
            match rng.below(4) {
                0 => bytes[at] ^= 1 << rng.below(8),
                1 => bytes.truncate(at),
                2 => {
                    bytes.remove(at);
                }
                _ => bytes.insert(at, rng.next_u64() as u8),
            }
        }
        check_unserialize_input(&bytes);
    }
}

/// The streams GNU R wrote, recorded for libR's conformance tests.
fn serialized_seeds() -> Vec<Vec<u8>> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../libr/tests/data/serialize/cases.tsv");
    let text = std::fs::read_to_string(path).expect("the recorded serialize cases");
    text.lines()
        .filter_map(|line| line.split('\t').nth(3))
        .map(|hex| {
            (0..hex.len() / 2)
                .map(|at| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).expect("hex"))
                .collect()
        })
        .collect()
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
    let mut rng = SplitMix64(0xb17e_c0de);
    for _ in 0..fuzz_iterations(300) {
        check_bytecode_agreement(&mut rng);
    }
}
