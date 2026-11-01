//! Replays the recorded cases under GNU R, and under the engine in `ORACLE_ENGINE` when set.
//! `RY_BLESS=1` re-records the expectations. The test passes without R installed.
//!
//! When ry's runtime is built, a rotating subset of the cases also runs under collection
//! stress (`ry-gctorture`) and must give what ry gives unstressed. `FUZZ_SEED` picks the
//! subset and `FUZZ_ITERS` its size; `ORACLE_ENGINE=ry-gctorture` runs every case against GNU R.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use oracle::{
    Engine, EngineError, ProcessEngine, diff, engine_from_environment, expected_path, find_cases,
    gnu_r_available, parse, render, ry_runtime_available,
};
use syntax::testing::{SplitMix64, fuzz_iterations};

fn cases() -> Vec<PathBuf> {
    find_cases(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cases"))
}

#[test]
fn recorded_outcomes_match_gnu_r() {
    if !gnu_r_available() {
        eprintln!("skipped: GNU R is not installed");
        return;
    }
    let gnu_r = ProcessEngine::gnu_r();
    let bless = std::env::var_os("RY_BLESS").is_some();
    let mut failures = Vec::new();
    assert!(!cases().is_empty(), "no oracle cases found");
    for case in cases() {
        let actual = gnu_r
            .run(&case)
            .unwrap_or_else(|error| panic!("{}: {error}", case.display()));
        if bless {
            std::fs::write(expected_path(&case), render(&actual)).expect("write expectation");
            continue;
        }
        let expected = std::fs::read_to_string(expected_path(&case))
            .ok()
            .and_then(|text| parse(&text));
        match expected {
            None => failures.push(format!("{}: no readable expectation", case.display())),
            Some(expected) => {
                if let Some(difference) = diff("expected", &expected, "GNU R", &actual) {
                    failures.push(format!("{}\n{difference}", case.display()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn second_engine_matches_gnu_r() {
    if !gnu_r_available() {
        return;
    }
    let gnu_r = ProcessEngine::gnu_r();
    let second = engine_from_environment();
    let mut failures = Vec::new();
    for case in cases() {
        match second.run(&case) {
            Err(EngineError::Unavailable(_)) => return,
            Err(error) => failures.push(format!("{}: {error}", case.display())),
            Ok(actual) => {
                let expected = gnu_r
                    .run(&case)
                    .unwrap_or_else(|error| panic!("{}: {error}", case.display()));
                if let Some(difference) = diff("GNU R", &expected, &second.name(), &actual) {
                    failures.push(format!("{}\n{difference}", case.display()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn cases_survive_gc_torture() {
    if !ry_runtime_available() {
        eprintln!("skipped: ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return;
    }
    let seed: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut all = cases();
    let mut rng = SplitMix64(seed);
    let mut chosen = Vec::new();
    for _ in 0..fuzz_iterations(4).min(all.len()) {
        chosen.push(all.swap_remove(rng.below(all.len())));
    }
    let timeout = Duration::from_secs(600);
    let ry = ProcessEngine::ry().with_timeout(timeout);
    let torture = ProcessEngine::ry_gc_torture().with_timeout(timeout);
    let started = Instant::now();
    let mut failures = Vec::new();
    for case in &chosen {
        let unstressed = match ry.run(case) {
            Ok(outcome) => outcome,
            // A case ry cannot run at all is the plain oracle's finding, not torture's.
            Err(error) => {
                eprintln!("{}: ry: {error}", case.display());
                continue;
            }
        };
        match torture.run(case) {
            Ok(stressed) => {
                if let Some(difference) = diff("ry", &unstressed, "ry-gctorture", &stressed) {
                    failures.push(format!("{}\n{difference}", case.display()));
                }
            }
            Err(error) => failures.push(format!("{}: {error}", case.display())),
        }
    }
    eprintln!(
        "{} case(s) from seed {seed} under collection stress in {:.1} s",
        chosen.len(),
        started.elapsed().as_secs_f64()
    );
    assert!(
        failures.is_empty(),
        "collection stress changed {} case(s); rerun with FUZZ_SEED={seed}:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn rendering_round_trips() {
    let outcome = oracle::Outcome {
        stdout: "a\n\nb".to_string(),
        messages: vec!["multi\nline \\ text".to_string()],
        warnings: vec!["w".to_string()],
        errors: vec![],
        value: Some(oracle::Value {
            kind: "double".to_string(),
            deparsed: "c(1,\n2)".to_string(),
        }),
    };
    assert_eq!(parse(&render(&outcome)), Some(outcome));
}
