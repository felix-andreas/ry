//! Differential fuzzing of ry's runtime: random programs from `oracle::fuzz` run under GNU R,
//! under ry's tier-0 interpreter (`RY_JIT=off`) and under the JIT forced on (`RY_JIT=force`),
//! and all three must agree on stdout, messages, warnings, errors and the last value. A smaller
//! pass runs the same programs under collection stress and compares them with ry unstressed.
//!
//! The seeds start at `FUZZ_SEED` (default 0) and `FUZZ_ITERS` sets how many run. Both passes
//! skip when GNU R or ry's runtime (`target/debug/ry` with `libR.so` beside it) is missing, so
//! build them first with `cargo build -p ry-lang -p libr`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use oracle::fuzz::{SeedReport, check_seed, program, render_divergences};
use oracle::{Engine, ProcessEngine, gnu_r_available, ry_runtime_available};
use syntax::testing::fuzz_iterations;

/// A debug build runs a fuzz program in a few seconds, so a run this long is a hang.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Collecting on every allocation makes a fuzz program take up to a few minutes on a busy machine.
const TORTURE_TIMEOUT: Duration = Duration::from_secs(900);

#[test]
fn programs_are_deterministic() {
    for seed in 0..64 {
        assert_eq!(
            program(seed),
            program(seed),
            "seed {seed} generated two programs"
        );
    }
    assert_ne!(program(0), program(1));
}

#[test]
fn fuzz_programs_agree_across_tiers() {
    if !gnu_r_available() {
        eprintln!("skipped: GNU R is not installed");
        return;
    }
    if !ry_runtime_available() {
        eprintln!("skipped: ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return;
    }
    let gnu_r = ProcessEngine::gnu_r().with_timeout(TIMEOUT);
    let tier0 = ProcessEngine::ry_tier0().with_timeout(TIMEOUT);
    let jit = ProcessEngine::ry_jit().with_timeout(TIMEOUT);
    let reports = run_seeds(&gnu_r, &[&tier0, &jit], fuzz_iterations(8));
    // A generator that drifts into erroring early would compare almost nothing, so most
    // programs must run to the end under GNU R.
    let completed = reports
        .iter()
        .filter(|report| report.reference.errors.is_empty())
        .count();
    assert!(
        completed * 10 >= reports.len() * 6,
        "only {completed} of {} programs ran without an error under GNU R",
        reports.len()
    );
    assert_agree(&reports);
}

#[test]
fn fuzz_programs_survive_gc_torture() {
    if !gnu_r_available() || !ry_runtime_available() {
        eprintln!("skipped: GNU R or ry's runtime is not available");
        return;
    }
    // Torture is compared with ry unstressed, so it reports what collection stress changes and
    // not the divergences from GNU R that the pass above already reports.
    let ry = ProcessEngine::ry().with_timeout(TIMEOUT);
    let torture = ProcessEngine::ry_gc_torture().with_timeout(TORTURE_TIMEOUT);
    let reports = run_seeds(&ry, &[&torture], fuzz_iterations(8).div_ceil(4));
    assert_agree(&reports);
}

fn run_seeds(reference: &dyn Engine, engines: &[&dyn Engine], count: usize) -> Vec<SeedReport> {
    let first: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let directory = scratch_directory();
    let started = Instant::now();
    let reports: Vec<SeedReport> = (first..first + count as u64)
        .map(|seed| {
            check_seed(seed, reference, engines, &directory)
                .unwrap_or_else(|error| panic!("seed {seed}: {} failed: {error}", reference.name()))
        })
        .collect();
    std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
    eprintln!(
        "{count} programs from seed {first} under {} and {} in {:.1} s",
        reference.name(),
        engines
            .iter()
            .map(|engine| engine.name())
            .collect::<Vec<_>>()
            .join(", "),
        started.elapsed().as_secs_f64()
    );
    reports
}

fn assert_agree(reports: &[SeedReport]) {
    let failures: Vec<String> = reports
        .iter()
        .filter(|report| !report.divergences.is_empty())
        .map(render_divergences)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} programs diverged:\n{}",
        failures.len(),
        reports.len(),
        failures.join("\n")
    );
}

fn scratch_directory() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "ry-oracle-differential-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&directory).expect("create the fuzz scratch directory");
    directory
}
