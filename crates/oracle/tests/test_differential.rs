//! Differential fuzzing of ry's runtime: random programs from `oracle::fuzz` run under GNU R,
//! under ry's tier-0 interpreter (`RY_JIT=off`) and under the JIT forced on (`RY_JIT=force`),
//! and all three must agree on stdout, messages, warnings, errors and the last value. A smaller
//! pass runs the same programs under collection stress and compares them with ry unstressed.
//!
//! The seeds start at `FUZZ_SEED` (default 0) and `FUZZ_ITERS` sets how many run. Both passes
//! skip when GNU R or ry's runtime (`target/debug/ry` with `libR.so` beside it) is missing, so
//! build them first with `cargo build -p ry-lang -p libr`.

use {
    oracle::{
        Engine, ProcessEngine,
        fuzz::{
            SeedReport, check_seed, native_helpers_program, parse_data_program, program,
            render_divergences, trace_back_program,
        },
        gnu_r_available, ry_runtime_available,
    },
    std::{
        path::PathBuf,
        time::{Duration, Instant},
    },
    syntax::testing::{fuzz_iterations, fuzz_seeds},
};

/// A debug build runs a fuzz program in a few seconds, so a run this long is a hang.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Collecting on every allocation makes a fuzz program take up to a few minutes on a busy machine.
const TORTURE_TIMEOUT: Duration = Duration::from_secs(900);
/// Seeds outside the default range whose programs once diverged, run on every pass:
/// 24 reported an unbound variable against the wrong call once a closure ran compiled.
const REGRESSION_SEEDS: &[u64] = &[24];

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
    let mut reports = run_seeds(&gnu_r, &[&tier0, &jit], fuzz_iterations(3));
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
    let directory = scratch_directory();
    for &seed in REGRESSION_SEEDS {
        reports.push(
            check_seed(seed, &gnu_r, &[&tier0, &jit], &directory)
                .unwrap_or_else(|error| panic!("seed {seed}: {} failed: {error}", gnu_r.name())),
        );
    }
    std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
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
    let young = ProcessEngine::ry_gc_torture_young().with_timeout(TORTURE_TIMEOUT);
    let reports = run_seeds(&ry, &[&torture, &young], fuzz_iterations(3).div_ceil(3));
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

/// The native withr helpers and base fast paths must give what their own R bodies give from the
/// same state, at tier 0 and under the JIT.
#[test]
fn fuzz_native_helpers_match_their_r_bodies() {
    if !ry_runtime_available() {
        eprintln!("skipped: ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return;
    }
    let engines = [
        ProcessEngine::ry_tier0().with_timeout(TIMEOUT),
        ProcessEngine::ry_jit().with_timeout(TIMEOUT),
    ];
    let directory = scratch_directory();
    for seed in fuzz_seeds(8) {
        let script = directory.join(format!("native_helpers_{seed}.R"));
        std::fs::write(&script, native_helpers_program(seed)).expect("write the fuzz program");
        for engine in &engines {
            let outcome = engine
                .run(&script)
                .unwrap_or_else(|error| panic!("seed {seed}: {} failed: {error}", engine.name()));
            if outcome.stdout.contains("withr 3.0.3 is not installed") {
                eprintln!("skipped: withr 3.0.3 is not installed for ry");
                std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
                return;
            }
            let checks = outcome
                .stdout
                .lines()
                .filter(|line| line.starts_with("check "))
                .count();
            let differing = outcome
                .stdout
                .lines()
                .any(|line| line.starts_with("check ") && !line.ends_with(": TRUE"));
            assert!(
                checks == 12 && !differing && outcome.errors.is_empty(),
                "seed {seed} under {}: {:?}\n{}\nreplay with FUZZ_SEED={seed} cargo test -p oracle --test test_differential fuzz_native_helpers",
                engine.name(),
                outcome.errors,
                outcome.stdout
            );
        }
    }
    std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
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

/// rlang's `trace_back()` runs natively on ry, and the native trace must be `identical()` to the
/// one the function's R body builds from the same frame, on random stacks, at tier 0 and under
/// the JIT. `FUZZ_SEED` replays a failing program.
#[test]
fn fuzz_native_trace_back_matches_its_r_body() {
    if !ry_runtime_available() {
        eprintln!("skipped: ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return;
    }
    let engines = [
        ProcessEngine::ry_tier0().with_timeout(TIMEOUT),
        ProcessEngine::ry_jit().with_timeout(TIMEOUT),
    ];
    let directory = scratch_directory();
    for seed in fuzz_seeds(4) {
        let script = directory.join(format!("trace_back_{seed}.R"));
        std::fs::write(&script, trace_back_program(seed)).expect("write the fuzz program");
        for engine in &engines {
            let outcome = engine
                .run(&script)
                .unwrap_or_else(|error| panic!("seed {seed}: {} failed: {error}", engine.name()));
            if outcome.stdout.contains("rlang is not installed") {
                eprintln!("skipped: rlang is not installed for ry");
                std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
                return;
            }
            let lines: Vec<&str> = outcome.stdout.lines().collect();
            let differing: Vec<&&str> = lines
                .iter()
                .filter(|line| !line.ends_with(": TRUE"))
                .collect();
            assert!(
                lines.len() == 8 && differing.is_empty() && outcome.errors.is_empty(),
                "seed {seed} under {}: {differing:?} {:?}\n{}\nreplay with FUZZ_SEED={seed} cargo test -p oracle --test test_differential fuzz_native_trace_back",
                engine.name(),
                outcome.errors,
                outcome.stdout
            );
        }
    }
    std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
}

/// `parse(keep.source = TRUE)` on random programs with comments, blank lines and tabs must give
/// GNU R's parse data (ids and parents included) and srcrefs exactly. Parsing happens before any
/// tier runs code, so tier 0 alone covers it.
#[test]
fn fuzz_parse_data_matches_gnu_r() {
    if !gnu_r_available() || !ry_runtime_available() {
        eprintln!("skipped: GNU R or ry's runtime is not available");
        return;
    }
    let gnu_r = ProcessEngine::gnu_r().with_timeout(TIMEOUT);
    let ry = ProcessEngine::ry_tier0().with_timeout(TIMEOUT);
    let directory = scratch_directory();
    for seed in fuzz_seeds(4) {
        let script = directory.join(format!("parse_data_{seed}.R"));
        std::fs::write(&script, parse_data_program(seed)).expect("write the fuzz program");
        let expected = gnu_r
            .run(&script)
            .unwrap_or_else(|error| panic!("seed {seed}: GNU R failed: {error}"));
        let actual = ry
            .run(&script)
            .unwrap_or_else(|error| panic!("seed {seed}: ry failed: {error}"));
        assert!(
            expected.errors.is_empty() && expected.stdout.lines().count() > 100,
            "seed {seed}: GNU R did not parse the program: {:?}",
            expected.errors
        );
        let differing = expected
            .stdout
            .lines()
            .zip(actual.stdout.lines())
            .position(|(left, right)| left != right);
        assert!(
            differing.is_none()
                && expected.stdout.lines().count() == actual.stdout.lines().count()
                && actual.errors.is_empty(),
            "seed {seed}: parse data differs from GNU R at line {differing:?} {:?}\nreplay with FUZZ_SEED={seed} cargo test -p oracle --test test_differential fuzz_parse_data",
            actual.errors
        );
    }
    std::fs::remove_dir_all(&directory).expect("remove the fuzz scratch directory");
}
