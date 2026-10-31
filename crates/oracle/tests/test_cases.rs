//! Replays the recorded cases under GNU R, and under the engine in `ORACLE_ENGINE` when set.
//! `RY_BLESS=1` re-records the expectations. The test passes without R installed.

use std::path::PathBuf;

use oracle::{
    Engine, EngineError, ProcessEngine, diff, engine_from_environment, expected_path, find_cases,
    gnu_r_available, parse, render,
};

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
