//! Deterministic simulation of the language server (`ry::server::simulation`).
//!
//! Each seed drives one editor session (edits, typing bursts, saves, file-system changes with
//! late, duplicated and reordered watcher events, requests at valid and invalid positions,
//! abandoned requests, and edits that land while a query is in flight) and holds it against a
//! server started from scratch on the same disk and buffers. Seeds run as `for_each_seed` runs
//! them: `FUZZ_SEED` picks the first, `FUZZ_ITERS` how many, and a failure prints the command
//! that replays it. `SIMULATION_SEED=<seed>` still runs just that seed and `SIMULATION_BASE`
//! still moves the first, and `SIMULATION_ACTIONS` sets each session's length (`just simulate`
//! runs many).

use {
    ry::server::simulation::run_session,
    std::path::PathBuf,
    syntax::testing::{SplitMix64, fuzz_iterations, fuzz_seeds, replay_command},
};

const ACTIONS: usize = 60;
/// The default number of seeds.
const SEEDS: usize = 6;

fn workspace(seed: u64) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("simulation")
        .join(format!("seed-{seed}"))
}

/// `SIMULATION_SEED` and `SIMULATION_BASE` predate `FUZZ_SEED` and still name seeds: the one
/// seed, or the first of `FUZZ_ITERS` seeds.
fn legacy_seeds() -> Option<std::ops::Range<u64>> {
    let variable = |name: &str| -> Option<u64> {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
    };
    if let Some(seed) = variable("SIMULATION_SEED") {
        return Some(seed..seed + 1);
    }
    let base = variable("SIMULATION_BASE")?;
    Some(base..base + fuzz_iterations(SEEDS) as u64)
}

fn actions() -> usize {
    std::env::var("SIMULATION_ACTIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(ACTIONS)
}

fn simulate(seed: u64) {
    let root = workspace(seed);
    match run_session(&mut SplitMix64(seed), &root, actions()) {
        Ok(report) => eprintln!(
            "seed {seed}: {} actions, {} steps, {} requests, {} interrupted, {} checkpoints",
            report.actions,
            report.steps,
            report.requests,
            report.interrupted_steps,
            report.checkpoints
        ),
        Err(failure) => panic!("{failure}"),
    }
}

/// The replay names the action count too, because a session's actions are part of its input.
fn replay(seed: u64) -> String {
    let command = replay_command(seed);
    match std::env::var("SIMULATION_ACTIONS") {
        Ok(actions) => format!("SIMULATION_ACTIONS={actions} {command}"),
        Err(_) => command,
    }
}

#[test]
fn simulated_sessions_match_a_fresh_start() {
    for seed in legacy_seeds().unwrap_or_else(|| fuzz_seeds(SEEDS)) {
        if std::panic::catch_unwind(|| simulate(seed)).is_err() {
            panic!(
                "simulation seed {seed} failed; replay with\n    {}",
                replay(seed)
            );
        }
    }
}

/// Sessions that once failed, at the length that found them. 1000034 asked for the calls of a
/// call-hierarchy item whose name resolved to a function in another document, and 1000130 saved
/// a file no buffer had open without republishing the open documents an earlier edit changed.
const REGRESSIONS: [(u64, usize); 2] = [(1_000_034, 150), (1_000_130, 150)];

#[test]
fn sessions_that_once_failed_match_a_fresh_start() {
    for (seed, actions) in REGRESSIONS {
        let root = workspace(seed);
        if let Err(failure) = run_session(&mut SplitMix64(seed), &root, actions) {
            panic!(
                "simulation seed {seed} failed again; replay with\n    \
                 SIMULATION_ACTIONS={actions} FUZZ_SEED={seed} cargo test -p ry-lang \
                 --test test_simulation simulated_sessions_match_a_fresh_start -- --exact\n{failure}"
            );
        }
    }
}

/// The same seed must produce the same transcript: every publish, message and response.
#[test]
fn a_seed_replays_the_same_transcript() {
    let seed = ["SIMULATION_SEED", "FUZZ_SEED"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok()?.parse().ok())
        .unwrap_or(7);
    let root = workspace(1_000_000 + seed);
    let first = run_session(&mut SplitMix64(seed), &root, 40)
        .unwrap_or_else(|failure| panic!("seed {seed}: {failure}"));
    let second = run_session(&mut SplitMix64(seed), &root, 40)
        .unwrap_or_else(|failure| panic!("seed {seed}: {failure}"));
    assert!(
        !first.transcript.is_empty(),
        "the session produced no traffic"
    );
    for (index, (left, right)) in first.transcript.iter().zip(&second.transcript).enumerate() {
        assert_eq!(
            left, right,
            "seed {seed}: transcripts diverge at message {index}"
        );
    }
    assert_eq!(
        first.transcript.len(),
        second.transcript.len(),
        "seed {seed}"
    );
}
