//! Deterministic simulation of the language server (`ry::server::simulation`).
//!
//! Each seed drives one editor session (edits, typing bursts, saves, file-system changes with
//! late, duplicated and reordered watcher events, requests at valid and invalid positions,
//! abandoned requests, and edits that land while a query is in flight) and holds it against a
//! server started from scratch on the same disk and buffers. Every failure prints its seed;
//! `SIMULATION_SEED=<seed>` replays exactly that session, and `FUZZ_ITERS` sets how many seeds
//! run (`just simulate` runs many).

use {
    ry::server::simulation::run_session,
    std::path::PathBuf,
    syntax::testing::{SplitMix64, fuzz_iterations},
};

const ACTIONS: usize = 60;

fn workspace(seed: u64) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("simulation")
        .join(format!("seed-{seed}"))
}

fn seeds() -> Vec<u64> {
    if let Some(seed) = std::env::var("SIMULATION_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return vec![seed];
    }
    let base: u64 = std::env::var("SIMULATION_BASE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (base..base + fuzz_iterations(6) as u64).collect()
}

fn actions() -> usize {
    std::env::var("SIMULATION_ACTIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(ACTIONS)
}

#[test]
fn simulated_sessions_match_a_fresh_start() {
    for seed in seeds() {
        let root = workspace(seed);
        let outcome =
            std::panic::catch_unwind(|| run_session(&mut SplitMix64(seed), &root, actions()));
        match outcome {
            Ok(Ok(report)) => eprintln!(
                "seed {seed}: {} actions, {} steps, {} requests, {} interrupted, {} checkpoints",
                report.actions,
                report.steps,
                report.requests,
                report.interrupted_steps,
                report.checkpoints
            ),
            Ok(Err(failure)) => panic!(
                "simulation seed {seed} failed; replay with SIMULATION_SEED={seed}\n{failure}"
            ),
            Err(_) => panic!("simulation seed {seed} panicked; replay with SIMULATION_SEED={seed}"),
        }
    }
}

/// The same seed must produce the same transcript: every publish, message and response.
#[test]
fn a_seed_replays_the_same_transcript() {
    let seed = std::env::var("SIMULATION_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
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
