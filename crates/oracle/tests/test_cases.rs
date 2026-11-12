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
        BUILD_FILE, BuildCheck, Engine, EngineError, KNOWN_FAILURES_FILE, ProcessEngine, Verdict,
        check_build, diff, engine_from_environment, expected_path, fails_run, find_cases,
        gnu_r_available, gnu_r_build, installed_packages, known_failure, known_failures,
        libraries_ahead_of_reference, parse, parse_known_failures, render, runnable_cases,
        ry_runtime_available, verdict, write_build,
    },
    std::{
        ffi::OsStr,
        path::{Path, PathBuf},
        process::Command,
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
    let known = known_failures().unwrap_or_else(|error| panic!("{error}"));
    let name = second.name();
    let mut failures = Vec::new();
    for case in cases() {
        let failure = match second.run(&case) {
            Err(EngineError::Unavailable(_)) => return,
            Err(error) => Some(format!("{}: {error}", case.display())),
            Ok(actual) => {
                let expected = match gnu_r.run(&case) {
                    Ok(expected) => expected,
                    Err(EngineError::Crashed(error)) => {
                        eprintln!("skipped {}: this GNU R crashed: {error}", case.display());
                        continue;
                    }
                    Err(error) => panic!("{}: {error}", case.display()),
                };
                diff("GNU R", &expected, &name, &actual)
                    .map(|difference| format!("{}\n{difference}", case.display()))
            }
        };
        let entry = known_failure(&known, &case, &name);
        match verdict(failure.is_some(), entry) {
            Verdict::Known => {
                if let Some(entry) = entry {
                    eprintln!("known {}: {}", case.display(), entry.reason);
                }
            }
            Verdict::New => failures.extend(failure),
            Verdict::ListedButPassing => failures.push(format!(
                "{} passes under {name}, so delete its entry from {KNOWN_FAILURES_FILE}",
                case.display()
            )),
            Verdict::Passed | Verdict::IntermittentPassed => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every entry in `known_failures.toml` names an existing case and engine, once.
#[test]
fn known_failures_are_valid() {
    if let Err(error) = known_failures() {
        panic!("{error}");
    }
}

/// A crash that depends on the host or on timing is listed as intermittent, and its case passing
/// does not fail the run, while any other listed case that passes does, so its fix deletes it.
#[test]
fn a_passing_intermittent_entry_does_not_fail_the_run() {
    let known = parse_known_failures(
        r#"
[[failure]]
case = "base__match_arg_native"
engine = "ry-jit-gctorture"
area = "jit"
reason = "aborts under the collector on some hosts"
intermittent = true

[[failure]]
case = "base__match_arg_native"
engine = "ry-gctorture"
area = "gc"
reason = "aborts under the collector"
"#,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let case = case_directory().join("base__match_arg_native.R");
    let intermittent = known_failure(&known, &case, "ry-jit-gctorture");
    let plain = known_failure(&known, &case, "ry-gctorture");

    assert_eq!(verdict(false, intermittent), Verdict::IntermittentPassed);
    assert!(!fails_run(verdict(false, intermittent)));
    assert_eq!(verdict(true, intermittent), Verdict::Known);
    assert!(!fails_run(verdict(true, intermittent)));

    assert_eq!(verdict(false, plain), Verdict::ListedButPassing);
    assert!(fails_run(verdict(false, plain)));
    assert_eq!(verdict(true, plain), Verdict::Known);
    assert!(!fails_run(verdict(true, plain)));

    assert!(fails_run(verdict(true, None)));
    assert!(!fails_run(verdict(false, None)));
}

/// Only a collector-stress engine's crash may be intermittent: a failure under any other engine
/// reproduces on every host, so the file refuses the mark there.
#[test]
fn intermittent_is_refused_outside_the_gctorture_engines() {
    let entry = |engine: &str| {
        format!(
            "[[failure]]\ncase = \"base__match_arg_native\"\nengine = \"{engine}\"\n\
             area = \"jit\"\nreason = \"crashes\"\nintermittent = true\n"
        )
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    for engine in ["ry", "ry-tier0", "ry-jit"] {
        let error = parse_known_failures(&entry(engine), directory)
            .expect_err("an intermittent entry under an engine without gctorture");
        assert!(
            error.contains(&format!(
                "`base__match_arg_native` under `{engine}` is marked intermittent, which only an \
                 entry under ry-gctorture, ry-gctorture-young, ry-jit-gctorture may be"
            )),
            "{error}"
        );
    }
    for engine in ["ry-gctorture", "ry-gctorture-young", "ry-jit-gctorture"] {
        if let Err(error) = parse_known_failures(&entry(engine), directory) {
            panic!("{error}");
        }
    }
}

/// An explicit `R_LIBS` comes before the corpus library, the corpus library fills in when
/// `R_LIBS` is unset, and an empty `R_LIBS` turns it off.
#[test]
fn corpus_library_follows_an_explicit_r_libs() {
    let corpus = Path::new("/work/.corpus/library");
    let found = |explicit: Option<&str>| {
        libraries_ahead_of_reference(explicit.map(OsStr::new), Some(corpus))
            .map(|libraries| libraries.to_string_lossy().into_owned())
    };
    assert_eq!(found(None).as_deref(), Some("/work/.corpus/library"));
    assert_eq!(
        found(Some("/mine:/other")).as_deref(),
        Some("/mine:/other:/work/.corpus/library")
    );
    assert_eq!(
        found(Some("/work/.corpus/library:/mine")).as_deref(),
        Some("/work/.corpus/library:/mine")
    );
    assert_eq!(found(Some("")), None);
    assert_eq!(libraries_ahead_of_reference(None, None), None);
}

/// A case that needs a package the engine lacks fails `oracle compare` before anything runs,
/// and `--allow-skips` runs the rest and marks every summary line PARTIAL. Without GNU R the
/// oracle compares with the recorded outcomes and asks the engine which packages it has, so a
/// shell script stands in for both and the test needs no R.
#[test]
fn compare_fails_on_a_missing_package_unless_skips_are_allowed() {
    let directory = std::env::temp_dir().join(format!("oracle-skips-{}", std::process::id()));
    let cases = directory.join("cases");
    std::fs::create_dir_all(&cases).unwrap_or_else(|error| panic!("{error}"));
    let engine = directory.join("engine.sh");
    let write = |path: &Path, text: &str| {
        std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    // The package probe sees `stats` only; every other script prints `hello`.
    write(
        &engine,
        "case \"$1\" in *packages.R) echo stats ;; *) echo hello ;; esac\n\
         printf '{\"messages\":[],\"warnings\":[],\"errors\":[],\"value\":null}' > \"$2\"\n",
    );
    let engine = format!("sh {}", engine.display());
    let plain = cases.join("plain.R");
    write(&plain, "cat('hello\\n')\n");
    write(
        &cases.join("needs.R"),
        "# oracle: requires nosuchpackage\ncat('hello\\n')\n",
    );
    let outcome = ProcessEngine::from_command(&engine)
        .and_then(|engine| engine.run(&plain).ok())
        .unwrap_or_else(|| panic!("`{engine}` did not run"));
    write(&expected_path(&plain), &render(&outcome));

    let compare = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_oracle"))
            .args(["compare", "--engine", &engine])
            .args(extra)
            .arg(&cases)
            .env("ORACLE_R", directory.join("no-r"))
            .output()
            .unwrap_or_else(|error| panic!("oracle: {error}"));
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        (output.status.code(), text)
    };
    let (status, text) = compare(&[]);
    assert_eq!(status, Some(1), "{text}");
    assert!(text.contains("needs.R: needs nosuchpackage"), "{text}");
    assert!(
        text.contains("pass --allow-skips for a partial run"),
        "{text}"
    );
    assert!(
        !text.contains("ok "),
        "a refused run must not run any case:\n{text}"
    );

    let (status, text) = compare(&["--allow-skips"]);
    assert_eq!(status, Some(0), "{text}");
    assert!(
        text.contains(&format!(
            "PARTIAL {engine}: 1 passed, 0 failed (0 known, 0 new), 1 skipped (1 missing a package)"
        )),
        "{text}"
    );
    assert!(
        text.contains("PARTIAL: --allow-skips left out 1 case"),
        "{text}"
    );

    std::fs::remove_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
}

/// A shell starts a background job (`&`) with SIGINT and SIGQUIT ignored, and GNU R keeps an
/// ignored SIGINT, so an oracle run started that way must hand its engines the default
/// dispositions, or a case that interrupts itself sees no interrupt.
#[test]
fn a_case_can_interrupt_itself_when_the_oracle_inherits_an_ignored_sigint() {
    if !gnu_r_available() {
        eprintln!("skipped: GNU R is not installed");
        return;
    }
    let case = case_directory().join("conditions__suspended_interrupts.R");
    let output = Command::new("sh")
        .arg("-c")
        .arg("trap '' INT QUIT; exec \"$0\" verify \"$1\"")
        .arg(env!("CARGO_BIN_EXE_oracle"))
        .arg(&case)
        .output()
        .unwrap_or_else(|error| panic!("sh: {error}"));
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("1 of 1 passed"), "{text}");
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
