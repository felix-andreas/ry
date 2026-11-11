//! Replays the recorded cases under GNU R, and under the engine in `ORACLE_ENGINE` when set.
//! `RY_BLESS=1` re-records the expectations. The test passes without R installed, and skips
//! when GNU R is another release than the one `cases/gnu-r.txt` names.
//!
//! When ry's runtime is built, a rotating subset of the cases also runs under each collection
//! stress (`ry-gctorture` and `ry-gctorture-young`) and must give what ry gives unstressed.
//! `FUZZ_SEED` picks the subset and `FUZZ_ITERS` its size; `ORACLE_ENGINE=ry-gctorture` runs
//! every case against GNU R.

use {
    oracle::{
        BUILD_FILE, BuildCheck, Engine, EngineError, ProcessEngine, check_build, diff,
        engine_from_environment, expected_path, find_cases, gnu_r_available, gnu_r_build,
        installed_packages, parse, render, runnable_cases, ry_runtime_available, write_build,
    },
    std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    },
    syntax::testing::{SplitMix64, fuzz_iterations},
};

fn case_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cases")
}

/// Cases that once read a freed argument reference after it left the pool, run on every stress
/// pass whatever the seed picks. `apply__lapply_closure_results_stay_rooted` crashed only under
/// collection stress; `datetime__year_overflow` crashed unstressed in S3 dispatch from a slot
/// builtin call; the other two crashed now and then with the JIT forced.
const REGRESSION_CASES: &[&str] = &[
    "apply__lapply_closure_results_stay_rooted",
    "datetime__year_overflow",
    "assignment__dollar_in_place",
    "environments__unread_loop_variable",
];

fn cases() -> Vec<PathBuf> {
    runnable_cases(find_cases(&case_directory()), installed_packages).0
}

#[test]
fn recorded_outcomes_match_gnu_r() {
    if !gnu_r_available() {
        eprintln!("skipped: GNU R is not installed");
        return;
    }
    let gnu_r = ProcessEngine::gnu_r();
    let bless = std::env::var_os("RY_BLESS").is_some();
    let build = gnu_r_build().unwrap_or_else(|error| panic!("{error}"));
    eprintln!("GNU R {build}");
    match check_build(&case_directory(), &build).unwrap_or_else(|error| panic!("{error}")) {
        BuildCheck::Same => {}
        BuildCheck::LibrariesDiffer(message) => eprintln!("warning: {message}"),
        BuildCheck::ReleaseDiffers(message) if bless => panic!(
            "refusing to bless: {message}; to move the suite to another release, delete \
             cases/{BUILD_FILE} and bless again"
        ),
        BuildCheck::ReleaseDiffers(message) => {
            eprintln!("skipped: {message}");
            return;
        }
        BuildCheck::Unrecorded if bless => {
            write_build(&case_directory().join(BUILD_FILE), &build).expect("write the build file")
        }
        BuildCheck::Unrecorded => panic!("cases/{BUILD_FILE} is missing; bless to write it"),
    }
    let mut failures = Vec::new();
    assert!(!cases().is_empty(), "no oracle cases found");
    for case in cases() {
        let actual = match gnu_r.run(&case) {
            Ok(actual) => actual,
            Err(EngineError::Crashed(error)) => {
                eprintln!("skipped {}: this GNU R crashed: {error}", case.display());
                continue;
            }
            Err(error) => panic!("{}: {error}", case.display()),
        };
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
    match gnu_r_build() {
        Ok(build) => eprintln!("GNU R {build}"),
        Err(error) => panic!("{error}"),
    }
    let mut failures = Vec::new();
    for case in cases() {
        match second.run(&case) {
            Err(EngineError::Unavailable(_)) => return,
            Err(error) => failures.push(format!("{}: {error}", case.display())),
            Ok(actual) => {
                let expected = match gnu_r.run(&case) {
                    Ok(expected) => expected,
                    Err(EngineError::Crashed(error)) => {
                        eprintln!("skipped {}: this GNU R crashed: {error}", case.display());
                        continue;
                    }
                    Err(error) => panic!("{}: {error}", case.display()),
                };
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
    cases_survive(ProcessEngine::ry_gc_torture());
}

#[test]
fn cases_survive_young_gc_torture() {
    cases_survive(ProcessEngine::ry_gc_torture_young());
}

/// A seeded subset of the cases under `torture` gives what ry gives unstressed.
fn cases_survive(torture: ProcessEngine) {
    if !ry_runtime_available() {
        eprintln!("skipped: ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return;
    }
    let seed: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let (mut chosen, mut all): (Vec<PathBuf>, Vec<PathBuf>) =
        cases().into_iter().partition(|case| {
            case.file_stem()
                .is_some_and(|stem| REGRESSION_CASES.iter().any(|name| stem == *name))
        });
    let mut rng = SplitMix64(seed);
    for _ in 0..fuzz_iterations(1).min(all.len()) {
        chosen.push(all.swap_remove(rng.below(all.len())));
    }
    let timeout = Duration::from_secs(600);
    let ry = ProcessEngine::ry().with_timeout(timeout);
    let torture = torture.with_timeout(timeout);
    let started = Instant::now();
    // Each case is two independent processes, so the cases run on every core; the failures are
    // reported in the order the seed chose the cases, whichever thread finished first.
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(1, |count| count.get());
    let mut failures: Vec<(usize, String)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(case) = chosen.get(index) else {
                            return found;
                        };
                        let unstressed = match ry.run(case) {
                            Ok(outcome) => outcome,
                            // A case ry cannot run at all is the plain oracle's finding, not
                            // torture's.
                            Err(error) => {
                                eprintln!("{}: ry: {error}", case.display());
                                continue;
                            }
                        };
                        match torture.run(case) {
                            Ok(stressed) => {
                                if let Some(difference) =
                                    diff("ry", &unstressed, &torture.name(), &stressed)
                                {
                                    found
                                        .push((index, format!("{}\n{difference}", case.display())));
                                }
                            }
                            Err(error) => {
                                found.push((index, format!("{}: {error}", case.display())))
                            }
                        }
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    failures.sort_by_key(|(index, _)| *index);
    let failures: Vec<String> = failures.into_iter().map(|(_, failure)| failure).collect();
    eprintln!(
        "{} case(s) from seed {seed} under {} in {:.1} s",
        chosen.len(),
        torture.name(),
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
        libraries: None,
    };
    assert_eq!(parse(&render(&outcome)), Some(outcome));
}

/// A run against a live GNU R never reads the recorded outcomes, so a malformed one surfaces only
/// in `oracle verify` or a run without R. This reads every case's file without running R.
#[test]
fn every_case_has_a_readable_expectation() {
    let cases = find_cases(&case_directory());
    assert!(!cases.is_empty(), "no oracle cases found");
    let mut failures = Vec::new();
    for case in &cases {
        let path = expected_path(case);
        let Ok(text) = std::fs::read_to_string(&path) else {
            failures.push(format!("{}: no expectation file", path.display()));
            continue;
        };
        match parse(&text) {
            None => failures.push(format!(
                "{}: not in the expectation format; record it with `oracle record {}`",
                path.display(),
                case.display()
            )),
            Some(outcome) if render(&outcome) != text => failures.push(format!(
                "{}: reads, but does not match what `oracle record` writes",
                path.display()
            )),
            Some(_) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} expectations are unreadable:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
