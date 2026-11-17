use {
    clap::{Parser, Subcommand},
    oracle::{
        BUILD_FILE, Build, BuildCheck, CaseRun, Engine, EngineError, KNOWN_FAILURES_FILE,
        ProcessEngine, Verdict,
        bench::{self, Summary},
        build_file, check_build,
        corpus::Snippet,
        diff, engine_packages, engines_named, expected_path, fails_run, find_cases,
        fuzz::{Divergence, MEMORY_LIMIT, ProgramKind},
        gallery, gnu_r_available, gnu_r_build, install, installed_packages, known_failure,
        known_failures,
        metamorphic::{api_functions, called_function, called_names},
        packages, parse, read_build,
        reduce::{Effort, Signature, reduce_divergence},
        render, runnable_cases, verdict, write_build,
    },
    packages::{PackageOutcome, Runtime, Settings},
    std::{
        collections::{BTreeMap, BTreeSet},
        path::{Path, PathBuf},
        process::ExitCode,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
};

/// Differential testing of R engines against GNU R.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run files under GNU R and compare with their recorded `.expected` files.
    Verify {
        /// List every passing case, not only the failures.
        #[arg(long)]
        verbose: bool,
        paths: Vec<PathBuf>,
    },
    /// Run files under GNU R and write their `.expected` files.
    Record { paths: Vec<PathBuf> },
    /// Run files under GNU R and under ry's engines, and compare them.
    Compare {
        /// The engines compared with GNU R, comma separated: `ry`, `ry-tier0`, `ry-jit`,
        /// `ry-gctorture`, `ry-gctorture-young`, `ry-jit-gctorture`, or a command line
        /// (default: `$ORACLE_ENGINE`, then `ry-tier0,ry-jit`). Unless `ORACLE_RY` names a
        /// binary, a ry engine first builds the workspace's `ry` and `libR.so`, so no run sees a
        /// stale build.
        #[arg(long)]
        engine: Option<String>,
        /// Run even when cases need packages that are not installed, skip those cases, and
        /// mark the summary PARTIAL. Without it, such a run fails before it starts.
        #[arg(long)]
        allow_skips: bool,
        /// List every passing and known failing case, not only what needs attention.
        #[arg(long)]
        verbose: bool,
        paths: Vec<PathBuf>,
    },
    /// Print the outcome of one file under GNU R.
    Show { path: PathBuf },
    /// Render the plot gallery (`gallery/*.R`) under GNU R and ry's runtime, compare the
    /// images device by device, and time every render on both.
    Gallery {
        /// The seed that picks the sample and seeds R's random numbers in every figure.
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// How many figures to sample (default: every figure).
        #[arg(long)]
        count: Option<usize>,
        /// Figures to render instead of a sample, as `file:figure`, such as `base:persp_surface`.
        #[arg(long, value_delimiter = ',')]
        figures: Vec<String>,
        /// The devices, comma separated: `agg`, `png`, `pdf` and `svg`.
        #[arg(long, default_value = "agg,png,pdf,svg")]
        devices: String,
        /// Renders per figure and device; the report keeps the fastest.
        #[arg(long, default_value_t = 1)]
        repeats: u32,
        /// Where the images go (default: a `gallery` directory in the system's temporary
        /// directory).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Generate random R programs from consecutive seeds and run each under GNU R and the
    /// given engines, reporting every divergence with its seed and program.
    Fuzz {
        /// The first seed.
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// How many consecutive seeds to run.
        #[arg(long, default_value_t = 1)]
        count: u64,
        /// The engines compared with GNU R, comma separated: `ry-tier0`, `ry-jit`, `ry`,
        /// `ry-gctorture`, or command lines.
        #[arg(long, default_value = "ry-tier0,ry-jit")]
        engines: String,
        /// Seconds one engine may take on one program.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Print the program for `--seed` and exit, without running anything.
        #[arg(long)]
        print: bool,
        /// The programs to run: `grammar` (generated blocks of language features), `metamorphic`
        /// (one value, one body, and the forms that must agree), `builtins` (builtins on a
        /// zoo of values), `api` (calls drawn from every function of base, stats and utils),
        /// `corpus` (mutated snippets of real R code, `oracle::corpus`), a comma-separated
        /// list of them, or `all` for each of them on every seed.
        #[arg(long, default_value = "grammar")]
        kind: String,
        /// Compare only what a program prints, without the deep observer's dump of every
        /// top-level value.
        #[arg(long)]
        no_observe: bool,
        /// How many programs run at once (default: one per core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Keep running consecutive seeds for this many minutes instead of `--count` seeds.
        #[arg(long)]
        minutes: Option<u64>,
        /// A directory that receives each diverging program and its report.
        #[arg(long)]
        findings: Option<PathBuf>,
        /// Reduce every diverging program against the first engine that diverges on it, and
        /// print the reduced program under the divergence.
        #[arg(long)]
        reduce: bool,
        /// Run nothing: generate the programs of `--seed` and `--count` and report how many of
        /// the functions base, stats and utils export each kind names in a call.
        #[arg(long)]
        coverage: bool,
    },
    /// Shrink a program on which an engine diverges from GNU R to a small one on which it
    /// diverges the same way, and print it with the command that replays it.
    Reduce {
        file: PathBuf,
        /// The engine that diverges, named as for `fuzz --engines`.
        #[arg(long)]
        engine: String,
        /// Seconds one engine may take on one candidate.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// The most candidates to run.
        #[arg(long, default_value_t = 2000)]
        budget: usize,
        /// Stop after this many minutes with the smallest program found by then.
        #[arg(long)]
        minutes: Option<u64>,
        /// Candidates to run side by side (default: one per core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Where to write the reduced program. It holds the smallest program so far while the
        /// reduction runs.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Run packages' testthat suites under GNU R and ry's runtime (both through `ry load`) and
    /// compare them test by test.
    Bench {
        /// The `ry` binary (default: `$ORACLE_RY`, then the workspace's debug build).
        #[arg(long)]
        ry: Option<PathBuf>,
        /// Only test files whose name matches this regular expression.
        #[arg(long, default_value = "")]
        filter: String,
        /// Seconds each runtime may take per package.
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
        /// Run GNU R only, to time it or to check the suite itself.
        #[arg(long)]
        gnu_r_only: bool,
        /// Run ry only, to time it: every test counts as matching, and GNU R's columns repeat ry's.
        #[arg(long, conflicts_with = "gnu_r_only")]
        ry_only: bool,
        /// List matching tests too, not only the differing ones.
        #[arg(long)]
        verbose: bool,
        /// Keep the package copy and each runtime's report and stderr, and print where.
        #[arg(long)]
        keep: bool,
        /// Write the full comparison as JSON to this file.
        #[arg(long)]
        json: Option<PathBuf>,
        packages: Vec<PathBuf>,
    },
    /// Run corpus packages' own tests under GNU R and ry's runtime and report per package.
    Packages {
        /// The runtimes to run: `both`, `ry` or `gnur`. A runtime not run reports its recorded
        /// result.
        #[arg(long, default_value = "both")]
        runtime: String,
        /// Packages run in parallel.
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// The limit in seconds for one package's suite under one runtime.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Rerun GNU R even when a result for the same package version is recorded.
        #[arg(long)]
        rerun_gnu_r: bool,
        /// The corpus directory (default: `$RY_CORPUS_DIR`, then `.corpus` in the repository).
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// Run ry's side on the packages `oracle install` built (`.corpus/ry-library`), through
        /// ry's R home (`.corpus/ry-home`), recording results under `ry-installed`.
        #[arg(long)]
        installed: bool,
        /// The packages to run (default: every package in `closure.txt`).
        packages: Vec<String>,
    },
    /// Install corpus packages from source through ry's `R CMD INSTALL` on ry's runtime, in
    /// dependency order, and load each one.
    Install {
        /// ry's R home, from `ry-rcmd assemble` (default: `.corpus/ry-home`).
        #[arg(long)]
        home: Option<PathBuf>,
        /// The library to install into (default: `.corpus/ry-library`).
        #[arg(long)]
        library: Option<PathBuf>,
        /// Packages installed in parallel.
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// The limit in seconds for one package's installation.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// The corpus directory (default: `$RY_CORPUS_DIR`, then `.corpus` in the repository).
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// The packages to install (default: every package in `closure.txt`).
        packages: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Fuzz {
        seed,
        print: true,
        kind,
        ..
    } = &cli.command
    {
        return match fuzz_kinds(kind).as_deref() {
            Some([FuzzKind::Generated(kind)]) => {
                print!("{}", kind.program(*seed));
                ExitCode::SUCCESS
            }
            Some([FuzzKind::Corpus]) => print_corpus_program(*seed),
            _ => {
                eprintln!(
                    "--print takes one --kind: grammar, metamorphic, builtins, api, io or corpus"
                );
                ExitCode::from(2)
            }
        };
    }
    if uses_workspace_ry(&cli.command)
        && let Err(error) = oracle::build_workspace_ry()
    {
        eprintln!("{error}");
        return ExitCode::from(2);
    }
    // Without GNU R, `compare` checks ry's engines against the recorded outcomes, which is
    // how ry's runtime is tested on a machine that has only ry.
    if let Command::Compare {
        engine,
        allow_skips,
        verbose,
        paths,
    } = &cli.command
        && !gnu_r_available()
    {
        let Some(engines) = compare_engines(engine.as_deref()) else {
            return ExitCode::from(2);
        };
        eprintln!("GNU R was not found, so the engines are compared with the recorded outcomes");
        let selection = cases(paths.clone(), || engine_packages(engines[0].as_ref()));
        return compare(None, &engines, &selection, *allow_skips, *verbose);
    }
    if !gnu_r_available() {
        eprintln!("GNU R was not found; install R or set ORACLE_R to its path");
        return ExitCode::from(2);
    }
    let gnu_r = ProcessEngine::gnu_r();
    match cli.command {
        Command::Verify { verbose, paths } => {
            verify(&gnu_r, &cases(paths, installed_packages), verbose)
        }
        Command::Record { paths } => record(&gnu_r, &cases(paths, installed_packages)),
        Command::Compare {
            engine,
            allow_skips,
            verbose,
            paths,
        } => {
            let Some(engines) = compare_engines(engine.as_deref()) else {
                return ExitCode::from(2);
            };
            compare(
                Some(&gnu_r),
                &engines,
                &cases(paths, installed_packages),
                allow_skips,
                verbose,
            )
        }
        Command::Bench {
            ry,
            filter,
            timeout,
            gnu_r_only,
            ry_only,
            verbose,
            keep,
            json,
            packages,
        } => {
            let ry = ry.unwrap_or_else(default_ry);
            // The suites run with the package as their working directory.
            let ry = match ry.canonicalize() {
                Ok(ry) => ry,
                Err(error) => {
                    eprintln!("{}: {error}", ry.display());
                    return ExitCode::from(2);
                }
            };
            let options = bench::Options {
                ry,
                filter,
                timeout: Duration::from_secs(timeout),
                skip_ry: gnu_r_only,
                skip_gnu_r: ry_only,
                keep,
            };
            run_bench(&packages, &options, verbose, json.as_deref())
        }
        Command::Packages {
            runtime,
            jobs,
            timeout,
            rerun_gnu_r,
            corpus,
            installed,
            packages,
        } => run_packages(
            runtime,
            jobs,
            timeout,
            rerun_gnu_r,
            corpus,
            installed,
            packages,
        ),
        Command::Install {
            home,
            library,
            jobs,
            timeout,
            corpus,
            packages,
        } => run_install(home, library, jobs, timeout, corpus, packages),
        Command::Fuzz {
            seed,
            count,
            engines,
            timeout,
            print: _,
            kind,
            no_observe,
            jobs,
            minutes,
            findings,
            reduce,
            coverage,
        } => run_fuzz(FuzzRun {
            first: seed,
            count,
            engines: &engines,
            timeout: Duration::from_secs(timeout),
            kind: &kind,
            observe: !no_observe,
            jobs,
            budget: minutes.map(|minutes| Duration::from_secs(minutes * 60)),
            findings,
            reduce,
            coverage,
        }),
        Command::Reduce {
            file,
            engine,
            timeout,
            budget,
            minutes,
            jobs,
            output,
        } => run_reduce(
            &file,
            &engine,
            Duration::from_secs(timeout),
            &Effort {
                budget,
                // A number of minutes too large for the clock is no deadline.
                deadline: minutes.and_then(|minutes| {
                    Instant::now().checked_add(Duration::from_secs(minutes.saturating_mul(60)))
                }),
                jobs: jobs
                    .or_else(|| std::thread::available_parallelism().ok().map(usize::from))
                    .unwrap_or(1),
                checkpoint: output.as_deref(),
            },
        ),
        Command::Gallery {
            seed,
            count,
            figures,
            devices,
            repeats,
            output,
        } => run_gallery(gallery::Options {
            seed,
            count,
            figures,
            devices: devices.split(',').map(str::to_string).collect(),
            repeats,
            output: output.unwrap_or_else(|| std::env::temp_dir().join("gallery")),
        }),
        Command::Show { path } => match gnu_r.run(&path) {
            Ok(outcome) => {
                print!("{}", render(&outcome));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Whether the command runs the workspace's own debug `ry`, which then has to be built first.
fn uses_workspace_ry(command: &Command) -> bool {
    let names_ry = |names: &str| names.split(',').any(|name| name.trim().starts_with("ry"));
    match command {
        Command::Compare { engine, .. } => match engine {
            Some(names) => names_ry(names),
            None => std::env::var("ORACLE_ENGINE")
                .ok()
                .filter(|names| !names.trim().is_empty())
                .is_none_or(|names| names_ry(&names)),
        },
        Command::Fuzz { engines, .. } => names_ry(engines),
        Command::Reduce { engine, .. } => names_ry(engine),
        Command::Gallery { .. } => true,
        Command::Bench { ry, .. } => ry.is_none(),
        _ => false,
    }
}

fn run_gallery(options: gallery::Options) -> ExitCode {
    if !oracle::ry_runtime_available() {
        eprintln!("ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return ExitCode::from(2);
    }
    match gallery::run(&options) {
        Ok(renders) => {
            print!("{}", gallery::render_report(&renders));
            println!("images: {}", options.output.display());
            match renders.iter().any(gallery::Render::fails) {
                true => ExitCode::FAILURE,
                false => ExitCode::SUCCESS,
            }
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The cases a run takes: those that can run, and how many were left out because they need a
/// package the engine lacks, which the summary line counts.
struct Selection {
    runnable: Vec<PathBuf>,
    missing: usize,
}

/// The cases under `paths` that can run, given the packages `installed` lists; each case that
/// needs a missing package or locale is reported as skipped.
fn cases(paths: Vec<PathBuf>, installed: impl FnOnce() -> Option<Vec<String>>) -> Selection {
    let paths = if paths.is_empty() {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cases")]
    } else {
        paths
    };
    let mut found = Vec::new();
    for path in &paths {
        let cases = find_cases(path);
        if cases.is_empty() {
            eprintln!("no .R files under {}", path.display());
            std::process::exit(2);
        }
        found.extend(cases);
    }
    let (runnable, missing) = runnable_cases(found, installed);
    for (case, requirements) in &missing {
        println!(
            "skip    {}: needs {}",
            case.display(),
            requirements.join(", ")
        );
    }
    Selection {
        runnable,
        missing: missing.len(),
    }
}

/// Runs the cases on one worker per core and reports the failures in case order.
fn verify(gnu_r: &ProcessEngine, selection: &Selection, verbose: bool) -> ExitCode {
    let cases = &selection.runnable;
    let Some(build) = announce_build() else {
        return ExitCode::from(2);
    };
    if check_suites(cases, &build, "verify").is_err() {
        return ExitCode::from(2);
    }
    let runs = syntax::testing::in_parallel(cases, |case| {
        let expected = std::fs::read_to_string(expected_path(case))
            .ok()
            .and_then(|text| parse(&text));
        expected.map(|expected| (expected, gnu_r.run(case)))
    });
    let mut failures = 0;
    let mut crashed = Vec::new();
    for (case, run) in cases.iter().zip(runs) {
        let run = match run {
            Ok(run) => run,
            Err(message) => {
                println!("PANICKED {}: {message}", case.display());
                failures += 1;
                continue;
            }
        };
        let Some((expected, actual)) = run else {
            println!("MISSING {}", case.display());
            failures += 1;
            continue;
        };
        match actual {
            Ok(actual) => match diff("expected", &expected, "GNU R", &actual) {
                None if verbose => println!("ok      {}", case.display()),
                None => {}
                Some(difference) => {
                    println!("DIFFERS {}\n{difference}", case.display());
                    failures += 1;
                }
            },
            Err(EngineError::Crashed(error)) => {
                println!("skip    {}: this GNU R crashed: {error}", case.display());
                crashed.push(case.as_path());
            }
            Err(error) => {
                println!("ERROR   {}: {error}", case.display());
                failures += 1;
            }
        }
    }
    summarize(
        cases.len() - crashed.len(),
        failures,
        selection.missing,
        &crashed,
    )
}

fn record(gnu_r: &ProcessEngine, selection: &Selection) -> ExitCode {
    let cases = &selection.runnable;
    let Some(build) = announce_build() else {
        return ExitCode::from(2);
    };
    if check_suites(cases, &build, "record").is_err() {
        return ExitCode::from(2);
    }
    let mut failures = 0;
    let mut crashed = Vec::new();
    for case in cases {
        if build_file(case).is_none()
            && let Some(directory) = case.parent()
        {
            let file = directory.join(BUILD_FILE);
            match write_build(&file, &build) {
                Ok(()) => println!("wrote   {}", file.display()),
                Err(error) => {
                    eprintln!("{}: {error}", file.display());
                    return ExitCode::FAILURE;
                }
            }
        }
        match gnu_r.run(case) {
            Ok(outcome) => match std::fs::write(expected_path(case), render(&outcome)) {
                Ok(()) => println!("wrote   {}", expected_path(case).display()),
                Err(error) => {
                    println!("ERROR   {}: {error}", case.display());
                    failures += 1;
                }
            },
            Err(EngineError::Crashed(error)) => {
                println!("skip    {}: this GNU R crashed: {error}", case.display());
                crashed.push(case.as_path());
            }
            Err(error) => {
                println!("ERROR   {}: {error}", case.display());
                failures += 1;
            }
        }
    }
    summarize(
        cases.len() - crashed.len(),
        failures,
        selection.missing,
        &crashed,
    )
}

/// Prints the build of the GNU R at hand, so every report says which R it ran.
fn announce_build() -> Option<Build> {
    match gnu_r_build() {
        Ok(build) => {
            println!("GNU R {build}");
            Some(build)
        }
        Err(error) => {
            eprintln!("{error}");
            None
        }
    }
}

/// Checks the build each suite among `cases` was recorded with against `build`. Another release
/// refuses `action`; other linear-algebra libraries only warn.
fn check_suites(cases: &[PathBuf], build: &Build, action: &str) -> Result<(), ()> {
    let mut seen = Vec::new();
    for case in cases {
        let key = build_file(case);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key.clone());
        match check_build(case, build) {
            Ok(BuildCheck::Same) => {}
            Ok(BuildCheck::LibrariesDiffer(message)) => println!("warning: {message}"),
            Ok(BuildCheck::ReleaseDiffers(message)) => {
                eprintln!("refusing to {action}: {message}");
                if action == "record"
                    && let Some(file) = key
                {
                    eprintln!(
                        "to move the suite to another release, delete {} and record every case",
                        file.display()
                    );
                }
                return Err(());
            }
            Ok(BuildCheck::Unrecorded) if action == "verify" => println!(
                "warning: no {BUILD_FILE} names the GNU R build {} was recorded with",
                case.parent().unwrap_or(Path::new(".")).display()
            ),
            Ok(BuildCheck::Unrecorded) => {}
            Err(error) => {
                eprintln!("{error}");
                return Err(());
            }
        }
    }
    Ok(())
}

/// The engines `--engine` names, else those [`engines_named`] falls back to.
fn compare_engines(names: Option<&str>) -> Option<Vec<Box<dyn Engine>>> {
    match engines_named(names) {
        Ok(engines) => Some(
            engines
                .into_iter()
                .map(|engine| Box::new(engine) as Box<dyn Engine>)
                .collect(),
        ),
        Err(error) => {
            eprintln!("--engine: {error}");
            None
        }
    }
}

/// Compares each of `engines` with GNU R, or with each case's recorded outcome when `gnu_r` is
/// `None`, and sorts every failure into known (listed in `known_failures.toml`, so main fails it
/// too) or new. The run fails on a new failure, and on a listed case that passes, so the change
/// that fixes a case also deletes its entry.
///
/// The cases run on one worker per core. The report lists what needs attention (new failures,
/// crashes, listed cases that pass) in case order and ends with one summary line per engine;
/// `verbose` adds a line for every passing and every known failing case.
fn compare(
    gnu_r: Option<&ProcessEngine>,
    engines: &[Box<dyn Engine>],
    selection: &Selection,
    allow_skips: bool,
    verbose: bool,
) -> ExitCode {
    let known = match known_failures() {
        Ok(known) => known,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if selection.missing > 0 && !allow_skips {
        eprintln!(
            "{} need packages or locales that are not installed (listed above), so the run would \
             not cover the suite. Install packages into the corpus library (`just corpus`) or set \
             R_LIBS to a library that has them, and generate locales with `sudo \
             tools/dev-setup.sh`; pass --allow-skips for a partial run.",
            plural(selection.missing, "case", "cases")
        );
        return ExitCode::FAILURE;
    }
    let cases = &selection.runnable;
    match gnu_r {
        Some(_) => {
            if announce_build().is_none() {
                return ExitCode::from(2);
            }
        }
        None => {
            let mut seen = Vec::new();
            for file in cases.iter().filter_map(|case| build_file(case)) {
                if seen.contains(&file) {
                    continue;
                }
                match read_build(&file) {
                    Ok(build) => println!(
                        "recorded with GNU R {build} ({})",
                        file.parent().unwrap_or(Path::new(".")).display()
                    ),
                    Err(error) => eprintln!("{error}"),
                }
                seen.push(file);
            }
        }
    }
    let runs = syntax::testing::in_parallel(cases, |case| run_case(gnu_r, engines, case));
    let mut tallies = vec![Tally::default(); engines.len()];
    let mut crashed = Vec::new();
    for (case, runs) in cases.iter().zip(runs) {
        let runs = match runs {
            Ok(CaseRuns::Compared(runs)) => runs,
            Err(message) => {
                println!("PANICKED {}: {message}", case.display());
                for tally in &mut tallies {
                    tally.new += 1;
                    tally.fails = true;
                }
                continue;
            }
            Ok(CaseRuns::GnuRCrashed(error)) => {
                println!("skip    {}: this GNU R crashed: {error}", case.display());
                crashed.push(case.as_path());
                continue;
            }
            Ok(CaseRuns::NoExpectation(error)) => {
                println!("ERROR   {}: {error}", case.display());
                for tally in &mut tallies {
                    tally.new += 1;
                    tally.fails = true;
                }
                continue;
            }
        };
        for ((engine, tally), run) in engines.iter().zip(&mut tallies).zip(runs) {
            let name = engine.name();
            let shown = match engines.len() {
                1 => case.display().to_string(),
                _ => format!("{} [{name}]", case.display()),
            };
            let Some((run, failure)) = run else {
                tally.unavailable += 1;
                continue;
            };
            let failure = failure.map(|(label, detail)| format!("{label} {shown}{detail}"));
            let entry = known_failure(&known, case, &name);
            let verdict = verdict(run, entry);
            tally.fails |= fails_run(verdict);
            match verdict {
                Verdict::Passed => {
                    if verbose {
                        println!("ok      {shown}");
                    }
                    tally.passed += 1;
                }
                Verdict::ListedButPassing => {
                    println!(
                        "PASSES  {shown}: listed in {KNOWN_FAILURES_FILE}, so delete its entry"
                    );
                    tally.passed += 1;
                    tally.listed_but_passing += 1;
                }
                Verdict::Known => {
                    if let Some(entry) = entry
                        && verbose
                    {
                        let mut notes = vec![format!("area {}", entry.area)];
                        if entry.provisional {
                            notes.push("provisional".to_string());
                        }
                        println!("known   {shown}: {} ({})", entry.reason, notes.join(", "));
                    }
                    tally.known += 1;
                }
                Verdict::New => {
                    if let Some(failure) = &failure {
                        println!("{failure}");
                    }
                    if entry.is_some() {
                        println!(
                            "        {shown}: listed in {KNOWN_FAILURES_FILE}, but a crash is \
                             never a known failure"
                        );
                    }
                    tally.new += 1;
                }
            }
        }
    }
    let mut failed = false;
    let mut ran = false;
    let partial = if selection.missing > 0 {
        "PARTIAL "
    } else {
        ""
    };
    for (engine, tally) in engines.iter().zip(&tallies) {
        if cases.len() == crashed.len() + tally.unavailable {
            if tally.unavailable > 0 {
                println!("{}: unavailable, so every case was skipped", engine.name());
            }
            continue;
        }
        ran = true;
        let skipped = selection.missing + crashed.len() + tally.unavailable;
        let mut line = format!(
            "{partial}{}: {} passed, {} failed ({} known, {} new), {skipped} skipped",
            engine.name(),
            tally.passed,
            tally.known + tally.new,
            tally.known,
            tally.new,
        );
        if skipped > 0 {
            let mut reasons = Vec::new();
            if selection.missing > 0 {
                reasons.push(format!("{} missing a package or locale", selection.missing));
            }
            if !crashed.is_empty() {
                reasons.push(plural(crashed.len(), "GNU R crash", "GNU R crashes"));
            }
            if tally.unavailable > 0 {
                reasons.push(format!("{} with the engine unavailable", tally.unavailable));
            }
            line.push_str(&format!(" ({})", reasons.join(", ")));
        }
        if tally.listed_but_passing > 0 {
            line.push_str(&format!(
                ", {} listed in {KNOWN_FAILURES_FILE} but passing",
                tally.listed_but_passing
            ));
        }
        println!("{line}");
        failed |= tally.fails;
    }
    if selection.missing > 0 {
        println!(
            "PARTIAL: --allow-skips left out {}, so these counts do not cover the suite",
            plural(selection.missing, "case", "cases")
        );
    }
    for case in &crashed {
        println!("  GNU R crashed: {}", case.display());
    }
    if failed && !verbose {
        println!("pass --verbose to list every passing and known failing case too");
    }
    match failed || (!ran && !crashed.is_empty() && crashed.len() < cases.len()) {
        true => ExitCode::FAILURE,
        false => ExitCode::SUCCESS,
    }
}

/// What one case gave: the reference outcome's failure, or each engine's run (`None` when the
/// engine is unavailable) with a label and detail to print when it fails.
enum CaseRuns {
    Compared(Vec<Option<EngineRun>>),
    GnuRCrashed(String),
    NoExpectation(String),
}

/// One engine's run of a case, and the label and detail printed when it fails.
type EngineRun = (CaseRun, Option<(&'static str, String)>);

/// Runs one case under GNU R (or reads its recorded outcome) and under every engine.
fn run_case(gnu_r: Option<&ProcessEngine>, engines: &[Box<dyn Engine>], case: &Path) -> CaseRuns {
    let expected = match gnu_r {
        Some(gnu_r) => gnu_r.run(case),
        None => std::fs::read_to_string(expected_path(case))
            .ok()
            .and_then(|text| parse(&text))
            .ok_or_else(|| EngineError::Failed("no recorded outcome".to_string())),
    };
    let expected = match expected {
        Ok(outcome) => outcome,
        Err(EngineError::Crashed(error)) => return CaseRuns::GnuRCrashed(error),
        Err(error) => return CaseRuns::NoExpectation(error.to_string()),
    };
    let runs = engines
        .iter()
        .map(|engine| match engine.run(case) {
            Ok(actual) => Some(match diff("GNU R", &expected, &engine.name(), &actual) {
                Some(difference) => (
                    CaseRun::Failed,
                    Some(("DIFFERS", format!("\n{difference}"))),
                ),
                None => (CaseRun::Passed, None),
            }),
            Err(EngineError::Unavailable(_)) => None,
            Err(EngineError::Crashed(error)) => {
                Some((CaseRun::Crashed, Some(("CRASHED", format!(": {error}")))))
            }
            Err(error) => Some((CaseRun::Failed, Some(("ERROR  ", format!(": {error}"))))),
        })
        .collect();
    CaseRuns::Compared(runs)
}

/// One engine's results in a `compare` run.
#[derive(Clone, Default)]
struct Tally {
    passed: usize,
    known: usize,
    new: usize,
    listed_but_passing: usize,
    unavailable: usize,
    /// Whether any case's [`Verdict`] fails the run.
    fails: bool,
}

struct FuzzRun<'a> {
    first: u64,
    count: u64,
    engines: &'a str,
    timeout: Duration,
    kind: &'a str,
    observe: bool,
    jobs: Option<usize>,
    budget: Option<Duration>,
    findings: Option<PathBuf>,
    reduce: bool,
    coverage: bool,
}

/// What a fuzz run draws one program from.
#[derive(Clone, Copy, PartialEq)]
enum FuzzKind {
    Generated(ProgramKind),
    /// A mutated snippet of real R code ([`oracle::corpus`]).
    Corpus,
}

/// The kinds `--kind` names, or `None` for a name that is none of them.
fn fuzz_kinds(name: &str) -> Option<Vec<FuzzKind>> {
    if name == "all" {
        let mut kinds: Vec<FuzzKind> = oracle::fuzz::KINDS
            .into_iter()
            .map(FuzzKind::Generated)
            .collect();
        kinds.push(FuzzKind::Corpus);
        return Some(kinds);
    }
    name.split(',')
        .map(|name| match name.trim() {
            "corpus" => Some(FuzzKind::Corpus),
            name => ProgramKind::named(name).map(FuzzKind::Generated),
        })
        .collect()
}

/// What one seed did, in the terms a fuzz run reports and counts.
struct Checked {
    /// `metamorphic seed 7`, or `corpus seed 7 (from a snippet's name)`.
    label: String,
    program: String,
    divergences: Vec<Divergence>,
    /// The report to print when `divergences` is not empty.
    rendered: String,
    unavailable: Vec<String>,
    /// The error that ended the program under GNU R.
    stopped: Option<String>,
    /// Why a seed without divergences was not a comparison either.
    skipped: Option<&'static str>,
    /// `engine<TAB>snippet` for each engine that differs on the unmutated snippet.
    base_differs: Vec<String>,
}

fn check_fuzz_seed(
    kind: FuzzKind,
    seed: u64,
    snippets: &[Snippet],
    gnu_r: &dyn Engine,
    engines: &[&dyn Engine],
    directory: &Path,
) -> Result<Checked, EngineError> {
    match kind {
        FuzzKind::Generated(kind) => {
            let report = oracle::fuzz::check_seed(kind, seed, gnu_r, engines, directory)?;
            Ok(Checked {
                label: format!("{} seed {seed}", kind.name()),
                rendered: oracle::fuzz::render_divergences(&report),
                stopped: report.reference.errors.first().cloned(),
                program: report.program,
                divergences: report.divergences,
                unavailable: report.unavailable,
                skipped: None,
                base_differs: Vec::new(),
            })
        }
        FuzzKind::Corpus => {
            let report =
                oracle::corpus::check_corpus_seed(seed, snippets, gnu_r, engines, directory)?;
            let skipped = if report.noisy {
                Some("GNU R disagrees with itself")
            } else if !report.base_differs.is_empty()
                && report.base_differs.len() + report.unavailable.len() == engines.len()
            {
                Some("the snippet differs unmutated")
            } else if report.unchanged {
                Some("no mutation applied")
            } else {
                None
            };
            Ok(Checked {
                label: format!("corpus seed {seed} (from {})", report.source),
                rendered: oracle::corpus::render_corpus_divergences(&report),
                base_differs: report
                    .base_differs
                    .iter()
                    .map(|divergence| format!("{}\t{}", divergence.engine, report.source))
                    .collect(),
                stopped: report.stopped,
                skipped,
                program: report.mutant,
                divergences: report.divergences,
                unavailable: report.unavailable,
            })
        }
    }
}

fn run_fuzz(run: FuzzRun) -> ExitCode {
    let Some(kinds) = fuzz_kinds(run.kind) else {
        eprintln!(
            "unknown --kind `{}`: grammar, metamorphic, builtins, api, io, corpus or all",
            run.kind
        );
        return ExitCode::from(2);
    };
    let Some((gnu_r, parsed)) = fuzz_engines(run.engines, run.timeout, run.observe) else {
        return ExitCode::from(2);
    };
    let engines: Vec<&dyn Engine> = parsed.iter().map(|engine| engine as &dyn Engine).collect();
    let snippets = if kinds.contains(&FuzzKind::Corpus) {
        let corpus = match oracle::corpus::load_corpus(true) {
            Ok(corpus) => corpus,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        for note in &corpus.notes {
            eprintln!("{note}");
        }
        for (source, count) in oracle::corpus::source_counts(&corpus.snippets) {
            println!("corpus: {count} snippets from {source}");
        }
        corpus.snippets
    } else {
        Vec::new()
    };
    if run.coverage {
        print_fuzz_coverage(&kinds, &snippets, run.first, run.count);
        return ExitCode::SUCCESS;
    }
    let directory = std::env::temp_dir().join(format!("ry-oracle-fuzz-{}", std::process::id()));
    if let Err(error) = std::fs::create_dir_all(&directory) {
        eprintln!("{}: {error}", directory.display());
        return ExitCode::FAILURE;
    }
    if let Some(findings) = &run.findings
        && let Err(error) = std::fs::create_dir_all(findings)
    {
        eprintln!("{}: {error}", findings.display());
        return ExitCode::FAILURE;
    }
    let started = Instant::now();
    let jobs = run
        .jobs
        .or_else(|| std::thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .max(1);
    // Workers take the next (seed, kind) pair from one counter, so a run with a time budget
    // covers a prefix of the seeds whatever the number of workers.
    let next = AtomicU64::new(0);
    let total = match run.budget {
        Some(_) => u64::MAX,
        None => run.count.saturating_mul(kinds.len() as u64),
    };
    // Set when no engine can run here, which no later seed changes.
    let stop = AtomicBool::new(false);
    let deadline = run.budget.map(|budget| started + budget);
    let state = Mutex::new(FuzzTally::default());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    if stop.load(Ordering::Relaxed)
                        || deadline.is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        return;
                    }
                    let position = next.fetch_add(1, Ordering::Relaxed);
                    if position >= total {
                        return;
                    }
                    let kind = kinds[(position % kinds.len() as u64) as usize];
                    let seed = run.first.saturating_add(position / kinds.len() as u64);
                    let name = match kind {
                        FuzzKind::Generated(kind) => kind.name(),
                        FuzzKind::Corpus => "corpus",
                    };
                    let stem = format!("{name}_{seed}");
                    let result =
                        check_fuzz_seed(kind, seed, &snippets, &gnu_r, &engines, &directory);
                    // A reduction runs hundreds of candidates, so it happens before this
                    // worker takes the lock the others print under.
                    let reduced = match &result {
                        Ok(checked) if run.reduce && !checked.divergences.is_empty() => {
                            reduce_seed(
                                checked,
                                &parsed,
                                run.timeout,
                                &directory.join(&stem),
                                deadline,
                            )
                        }
                        _ => None,
                    };
                    let mut tally = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    tally.ran += 1;
                    match result {
                        Ok(checked) => {
                            let label = &checked.label;
                            if let Some(error) = &checked.stopped {
                                tally.ended_in_error += 1;
                                // Every kind but the grammar's catches its own errors, so
                                // a program that stops GNU R says the generator wrote
                                // something it did not mean.
                                if matches!(
                                    kind,
                                    FuzzKind::Generated(
                                        ProgramKind::Metamorphic
                                            | ProgramKind::Builtins
                                            | ProgramKind::Api
                                            | ProgramKind::Io
                                            | ProgramKind::Language
                                    )
                                ) {
                                    println!("STOPS   {label} under GNU R: {error}");
                                }
                            }
                            for reason in &checked.unavailable {
                                eprintln!("unavailable: {reason}");
                            }
                            tally.base_differs.extend(checked.base_differs);
                            if !checked.unavailable.is_empty()
                                && checked.unavailable.len() == engines.len()
                            {
                                tally.unavailable = true;
                                stop.store(true, Ordering::Relaxed);
                            } else if let Some(reason) =
                                checked.skipped.filter(|_| checked.divergences.is_empty())
                            {
                                tally.skipped += 1;
                                println!("skipped {label}: {reason}");
                            } else if checked.divergences.is_empty() {
                                println!("ok      {label}");
                            } else {
                                let mut rendered = checked.rendered;
                                if let Some(reduced) = &reduced {
                                    rendered.push_str(reduced);
                                }
                                let crashed = checked
                                    .divergences
                                    .iter()
                                    .any(|divergence| divergence.crashed);
                                println!(
                                    "{} {rendered}",
                                    if crashed { "CRASHES" } else { "DIFFERS" }
                                );
                                for name in checked
                                    .divergences
                                    .iter()
                                    .flat_map(|divergence| divergence.kinds())
                                {
                                    *tally.kinds.entry(name).or_default() += 1;
                                }
                                for divergence in &checked.divergences {
                                    for expression in &divergence.expressions {
                                        if let Some(function) = called_function(&expression.source)
                                        {
                                            *tally
                                                .functions
                                                .entry((function, expression.difference.name()))
                                                .or_default() += 1;
                                        }
                                    }
                                }
                                if let Some(findings) = &run.findings {
                                    for (extension, text) in
                                        [("R", &checked.program), ("report", &rendered)]
                                    {
                                        let path = findings.join(format!("{stem}.{extension}"));
                                        if let Err(error) = std::fs::write(&path, text) {
                                            eprintln!("{}: {error}", path.display());
                                        }
                                    }
                                }
                                tally.diverging.push((name, seed));
                            }
                        }
                        Err(error) => {
                            println!("ERROR   {name} seed {seed}: GNU R: {error}");
                            tally.diverging.push((name, seed));
                        }
                    }
                }
            });
        }
    });
    let mut tally = state
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(error) = std::fs::remove_dir_all(&directory) {
        eprintln!("could not remove {}: {error}", directory.display());
    }
    if tally.unavailable {
        eprintln!("no engine besides GNU R can run here");
        return ExitCode::from(2);
    }
    if !tally.base_differs.is_empty() {
        // Read last, so runs that end at about the same time keep each other's entries.
        let file = oracle::corpus::corpus_directory().join("base-differs.txt");
        if let Ok(text) = std::fs::read_to_string(&file) {
            tally.base_differs.extend(text.lines().map(str::to_string));
        }
        let listing: String = tally
            .base_differs
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();
        match std::fs::write(&file, listing) {
            Ok(()) => println!(
                "snippets that differ unmutated are listed in {}",
                file.display()
            ),
            Err(error) => eprintln!("{}: {error}", file.display()),
        }
    }
    println!(
        "{} of {} programs agree ({} ended in an error under GNU R, {} skipped) in {:.1} s",
        tally.ran - tally.diverging.len() - tally.skipped,
        tally.ran,
        tally.ended_in_error,
        tally.skipped,
        started.elapsed().as_secs_f64()
    );
    if tally.diverging.is_empty() {
        ExitCode::SUCCESS
    } else {
        tally.diverging.sort();
        let listed: Vec<String> = tally
            .diverging
            .iter()
            .map(|(name, seed)| format!("{name}:{seed}"))
            .collect();
        println!("diverging: {}", listed.join(" "));
        let kinds: Vec<String> = tally
            .kinds
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect();
        println!("by kind: {}", kinds.join(", "));
        if !tally.functions.is_empty() {
            let mut functions: Vec<_> = tally.functions.iter().collect();
            functions.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
            let listing: String = functions
                .iter()
                .map(|((function, kind), count)| format!("{count}\t{kind}\t{function}\n"))
                .collect();
            match &run.findings {
                Some(findings) => {
                    let file = findings.join("functions.txt");
                    match std::fs::write(&file, listing) {
                        Ok(()) => println!(
                            "{} differing functions are listed in {}",
                            functions.len(),
                            file.display()
                        ),
                        Err(error) => eprintln!("{}: {error}", file.display()),
                    }
                }
                None => print!("by function:\n{listing}"),
            }
        }
        ExitCode::FAILURE
    }
}

/// Prints, for each kind and for all of them together, how many of the callable functions of the
/// api table (`oracle::metamorphic::api_functions`) the programs of `count` seeds name in a call.
/// It runs no engine, so it measures what the generators can reach, not what ry gets right.
fn print_fuzz_coverage(kinds: &[FuzzKind], snippets: &[Snippet], first: u64, count: u64) {
    const PACKAGES: [&str; 3] = ["base", "stats", "utils"];
    let callable: BTreeMap<&str, &str> = api_functions()
        .iter()
        .filter(|function| function.positional.is_some())
        .map(|function| (function.name, function.package))
        .collect();
    let by_package = |names: &BTreeSet<&str>| -> String {
        let mut text = format!("{:>6}", names.len());
        for package in PACKAGES {
            let covered = names
                .iter()
                .filter(|name| callable.get(*name) == Some(&package))
                .count();
            text.push_str(&format!("{covered:>7}"));
        }
        text
    };
    println!("functions named in a call by {count} seeds of each kind, from seed {first}:");
    println!("{:<12}   all   base  stats  utils", "kind");
    let universe: BTreeSet<&str> = callable.keys().copied().collect();
    println!("{:<12}{}", "(the table)", by_package(&universe));
    let mut together = BTreeSet::new();
    for &kind in kinds {
        let mut covered = BTreeSet::new();
        for seed in first..first.saturating_add(count) {
            let program = match kind {
                FuzzKind::Generated(kind) => kind.program(seed),
                FuzzKind::Corpus => match oracle::corpus::corpus_program(snippets, seed) {
                    Some((_, program)) => program,
                    None => continue,
                },
            };
            for name in called_names(&program) {
                if let Some((&name, _)) = callable.get_key_value(name) {
                    covered.insert(name);
                }
            }
        }
        let name = match kind {
            FuzzKind::Generated(kind) => kind.name(),
            FuzzKind::Corpus => "corpus",
        };
        println!("{name:<12}{}", by_package(&covered));
        together.extend(covered);
    }
    if kinds.len() > 1 {
        println!("{:<12}{}", "together", by_package(&together));
    }
}

/// GNU R and the engines a comma-separated `--engines` names, each with the timeout and, when
/// `observe`, the deep observer. `None` after naming an unknown engine on standard error.
fn fuzz_engines(
    names: &str,
    timeout: Duration,
    observe: bool,
) -> Option<(ProcessEngine, Vec<ProcessEngine>)> {
    let prepare = |engine: ProcessEngine| {
        let engine = engine.with_timeout(timeout).with_memory_limit(MEMORY_LIMIT);
        if observe { engine.observing() } else { engine }
    };
    let mut engines = Vec::new();
    for name in names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        match ProcessEngine::named(name) {
            Some(engine) => engines.push(prepare(engine)),
            None => {
                eprintln!("unknown engine `{name}`");
                return None;
            }
        }
    }
    Some((prepare(ProcessEngine::gnu_r()), engines))
}

fn print_corpus_program(seed: u64) -> ExitCode {
    let corpus = match oracle::corpus::load_corpus(true) {
        Ok(corpus) => corpus,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    match oracle::corpus::corpus_program(&corpus.snippets, seed) {
        Some((snippet, mutant)) => {
            print!("# from {}\n{mutant}", snippet.name);
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("the fuzz corpus is empty");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct FuzzTally {
    ran: usize,
    ended_in_error: usize,
    /// The kind's name and the seed of every program with a divergence.
    diverging: Vec<(&'static str, u64)>,
    /// Seeds that compared nothing: a corpus snippet that differs unmutated, did not mutate, or
    /// depends on the run.
    skipped: usize,
    /// The corpus snippets that differ before mutation, as `engine<TAB>snippet`.
    base_differs: BTreeSet<String>,
    /// How many engine runs differ in each way ([`oracle::fuzz::Divergence::kinds`]).
    kinds: BTreeMap<&'static str, usize>,
    /// How many expressions of api programs differ, by the function called and the way.
    functions: BTreeMap<(String, &'static str), usize>,
    unavailable: bool,
}

/// Reduces a diverging seed's program against the first engine that diverges on it, and
/// returns the section a report ends with.
fn reduce_seed(
    report: &Checked,
    engines: &[ProcessEngine],
    timeout: Duration,
    directory: &Path,
    deadline: Option<Instant>,
) -> Option<String> {
    let divergence = report.divergences.first()?;
    let engine = engines
        .iter()
        .find(|engine| engine.name() == divergence.engine)?;
    // Every candidate that still hangs costs the whole timeout.
    if divergence.detail.contains("was killed after") {
        return Some(
            "--- not reduced: a timeout, which `oracle reduce` shrinks on request ---\n"
                .to_string(),
        );
    }
    if let Err(error) = std::fs::create_dir_all(directory) {
        return Some(format!(
            "--- not reduced: {}: {error} ---\n",
            directory.display()
        ));
    }
    let gnu_r = ProcessEngine::gnu_r().with_timeout(timeout).observing();
    let observing = engine.clone().observing();
    Some(
        match reduce_divergence(
            &report.program,
            &gnu_r,
            &observing,
            directory,
            // The fuzz run's own workers fill the cores.
            &Effort {
                budget: 2000,
                deadline,
                jobs: 1,
                checkpoint: None,
            },
        ) {
            Ok(reduction) => format!(
                "--- reduced under {} ({}, {} candidates) ---\n{}",
                engine.name(),
                reduction.signature,
                reduction.calls,
                reduction.program
            ),
            Err(error) => format!("--- not reduced: {error} ---\n"),
        },
    )
}

/// Reduces `file` against the engine `name`, prints the result and the command that replays it,
/// and writes it to `output`.
fn run_reduce(file: &Path, name: &str, timeout: Duration, effort: &Effort) -> ExitCode {
    let output = effort.checkpoint;
    let Some(engine) = ProcessEngine::named(name) else {
        eprintln!("unknown engine `{name}`");
        return ExitCode::from(2);
    };
    let engine = engine.with_timeout(timeout).observing();
    let gnu_r = ProcessEngine::gnu_r().with_timeout(timeout).observing();
    let program = match std::fs::read_to_string(file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{}: {error}", file.display());
            return ExitCode::from(2);
        }
    };
    let directory = std::env::temp_dir().join(format!("ry-oracle-reduce-{}", std::process::id()));
    if let Err(error) = std::fs::create_dir_all(&directory) {
        eprintln!("{}: {error}", directory.display());
        return ExitCode::FAILURE;
    }
    let started = Instant::now();
    let reduction = reduce_divergence(&program, &gnu_r, &engine, &directory, effort);
    if let Err(error) = std::fs::remove_dir_all(&directory) {
        eprintln!("could not remove {}: {error}", directory.display());
    }
    let reduction = match reduction {
        Ok(reduction) => reduction,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", reduction.program);
    println!(
        "--- {name} {}: {} to {} bytes, {} candidates in {:.1} s ---",
        reduction.signature,
        reduction.bytes_before,
        reduction.bytes_after,
        reduction.calls,
        started.elapsed().as_secs_f64()
    );
    if let Some(output) = output
        && let Err(error) = std::fs::write(output, &reduction.program)
    {
        eprintln!("{}: {error}", output.display());
        return ExitCode::FAILURE;
    }
    let observe = match reduction.signature {
        Signature::ObservedDiffers(_) => "ORACLE_OBSERVE=1 ",
        _ => "",
    };
    println!(
        "replay: {observe}cargo run -q -p oracle -- compare --engine {name} {}",
        output.unwrap_or(Path::new("reduced.R")).display()
    );
    ExitCode::SUCCESS
}

fn corpus_directory(corpus: Option<PathBuf>) -> PathBuf {
    corpus
        .or_else(|| std::env::var_os("RY_CORPUS_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.corpus"))
}

fn run_install(
    home: Option<PathBuf>,
    library: Option<PathBuf>,
    jobs: usize,
    timeout: u64,
    corpus: Option<PathBuf>,
    packages: Vec<String>,
) -> ExitCode {
    let corpus = corpus_directory(corpus);
    let absolute = |path: PathBuf| std::path::absolute(&path).unwrap_or(path);
    let everything = match packages::closure(&corpus) {
        Ok(everything) => everything,
        Err(error) => {
            eprintln!(
                "cannot read {}: {error}; run `just corpus-sources` first",
                corpus.join("closure.txt").display()
            );
            return ExitCode::from(2);
        }
    };
    let packages = if packages.is_empty() {
        everything
    } else {
        packages
    };
    let settings = install::Settings {
        home: absolute(home.unwrap_or_else(|| corpus.join("ry-home"))),
        library: absolute(library.unwrap_or_else(|| corpus.join("ry-library"))),
        corpus: absolute(corpus),
        jobs,
        timeout: Duration::from_secs(timeout),
    };
    if !settings.home.join("bin/R").exists() {
        eprintln!(
            "{} is not an R home; run `just ry-home` first",
            settings.home.display()
        );
        return ExitCode::from(2);
    }
    let report = |package: &str, outcome: &install::Outcome| {
        let line = match outcome {
            install::Outcome::Installed {
                seconds,
                loaded: Ok(()),
            } => format!("installed {seconds:>6.1}s, loads"),
            install::Outcome::Installed {
                seconds,
                loaded: Err(error),
            } => {
                format!("installed {seconds:>6.1}s, load failed: {error}")
            }
            install::Outcome::Failed { seconds, error } => {
                format!("failed    {seconds:>6.1}s: {error}")
            }
            install::Outcome::Skipped(reason) => format!("skipped: {reason}"),
            install::Outcome::NoSources => "no sources".to_owned(),
        };
        println!("{package:<24} {line}");
    };
    let outcomes = install::run(&settings, &packages, &report);
    let path = settings.corpus.join("ry-install").join("summary.tsv");
    // A run over some packages replaces their rows and keeps the rest of the summary.
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let summary = install::merge_summary(&previous, &outcomes);
    if let Err(error) = std::fs::write(&path, &summary) {
        eprintln!("cannot write {}: {error}", path.display());
        return ExitCode::FAILURE;
    }
    let (installed, loaded) = install::counts(&outcomes);
    println!(
        "{} packages: {installed} installed, {loaded} load; summary in {}",
        outcomes.len(),
        path.display()
    );
    let (total, installed, loaded) = install::summary_counts(&summary);
    println!("summary: {total} packages, {installed} installed, {loaded} load");
    ExitCode::SUCCESS
}

fn run_packages(
    runtime: String,
    jobs: usize,
    timeout: u64,
    rerun_gnu_r: bool,
    corpus: Option<PathBuf>,
    installed: bool,
    packages: Vec<String>,
) -> ExitCode {
    let runtimes = match runtime.as_str() {
        "both" => vec![Runtime::GnuR, Runtime::Ry],
        "ry" => vec![Runtime::Ry],
        "gnur" => vec![Runtime::GnuR],
        other => {
            eprintln!("--runtime must be `both`, `ry` or `gnur`, not `{other}`");
            return ExitCode::from(2);
        }
    };
    let corpus = corpus_directory(corpus);
    let everything = match packages::closure(&corpus) {
        Ok(everything) => everything,
        Err(error) => {
            eprintln!(
                "cannot read {}: {error}; run `just corpus-sources` first",
                corpus.join("closure.txt").display()
            );
            return ExitCode::from(2);
        }
    };
    let packages = if packages.is_empty() {
        everything.clone()
    } else {
        packages
    };
    let settings = Settings {
        corpus: corpus.clone(),
        rscript: std::env::var("ORACLE_RSCRIPT").unwrap_or_else(|_| "Rscript".to_owned()),
        // Scripts run with `tests/` as the working directory, so a relative path must resolve now.
        ry: std::fs::canonicalize(oracle::ry_program())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| oracle::ry_program()),
        jobs,
        timeout: Duration::from_secs(timeout),
        reuse_gnu_r: !rerun_gnu_r,
        installed: installed.then(|| packages::Installed {
            home: std::path::absolute(corpus.join("ry-home"))
                .unwrap_or_else(|_| corpus.join("ry-home")),
            library: std::path::absolute(corpus.join("ry-library"))
                .unwrap_or_else(|_| corpus.join("ry-library")),
        }),
    };
    let report = |package: &str, outcome: &PackageOutcome| {
        let line = match outcome {
            PackageOutcome::NoSources => "no sources".to_owned(),
            PackageOutcome::NoTests => "no tests".to_owned(),
            PackageOutcome::SetupFailed(error) => format!("setup failed: {error}"),
            PackageOutcome::Ran { gnu_r, ry } => {
                let status = |result: &Option<packages::SuiteResult>| match result {
                    Some(result) => format!("{:?}", result.status).to_lowercase(),
                    None => "-".to_owned(),
                };
                format!("gnur {:<8} ry {}", status(gnu_r), status(ry))
            }
        };
        println!("{package:<24} {line}");
    };
    packages::run_all(&settings, &packages, &runtimes, &report);
    // The summary always covers the whole closure, reading recorded results for the rest.
    let all = packages::run_all(&settings, &everything, &[], &|_, _| {});
    let summary = packages::summary(&all);
    let path = corpus.join("tests").join(if installed {
        "summary-installed.md"
    } else {
        "summary.md"
    });
    if let Err(error) = std::fs::write(&path, &summary) {
        eprintln!("cannot write {}: {error}", path.display());
        return ExitCode::FAILURE;
    }
    println!("{}", summary.lines().next().unwrap_or_default());
    println!("report: {}", path.display());
    ExitCode::SUCCESS
}

/// Prints the summary line, which counts the cases left out of `total` beside the result so a
/// short run cannot pass for a complete one, and names each case GNU R crashed on. A crash of
/// the reference is reported but does not fail the run, since it says nothing about the engine.
fn summarize(total: usize, failures: usize, missing: usize, crashed: &[&Path]) -> ExitCode {
    let mut line = format!("{} of {total} passed", total - failures);
    if missing + crashed.len() > 0 {
        let mut reasons = Vec::new();
        if missing > 0 {
            reasons.push(format!("{missing} missing a package or locale"));
        }
        if !crashed.is_empty() {
            reasons.push(plural(crashed.len(), "GNU R crash", "GNU R crashes"));
        }
        line.push_str(&format!(
            ", {} skipped ({})",
            missing + crashed.len(),
            reasons.join(", ")
        ));
    }
    println!("{line}");
    for case in crashed {
        println!("  GNU R crashed: {}", case.display());
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn default_ry() -> PathBuf {
    std::env::var_os("ORACLE_RY").map_or_else(
        || oracle::target_directory().join("debug/ry"),
        PathBuf::from,
    )
}

fn run_bench(
    packages: &[PathBuf],
    options: &bench::Options,
    verbose: bool,
    json: Option<&Path>,
) -> ExitCode {
    if packages.is_empty() {
        eprintln!("name at least one package directory");
        return ExitCode::from(2);
    }
    let mut summaries: Vec<Summary> = Vec::new();
    for package in packages {
        match bench::bench_package(package, options) {
            Ok(summary) => {
                print!("{}", bench::render_summary(&summary, verbose));
                summaries.push(summary);
            }
            Err(error) => {
                eprintln!("{}: {error}", package.display());
                return ExitCode::FAILURE;
            }
        }
    }
    println!();
    println!(
        "{:<10} {:>6} {:>8} {:>6} {:>8} {:>8} {:>8} {:>10} {:>10} {:>7}",
        "package",
        "tests",
        "match",
        "rate",
        "differs",
        "missing",
        "ry pass",
        "GNU R s",
        "ry s",
        "ry/R"
    );
    for summary in &summaries {
        let suite = |seconds: Option<f64>| {
            seconds.map_or_else(|| "-".to_string(), |seconds| format!("{seconds:.2}"))
        };
        let ratio = match (summary.gnu_r_suite_seconds, summary.ry_suite_seconds) {
            (Some(gnu_r), Some(ry)) if gnu_r > 0.0 => format!("{:.2}x", ry / gnu_r),
            _ => "-".to_string(),
        };
        println!(
            "{:<10} {:>6} {:>8} {:>5.1}% {:>8} {:>8} {:>8} {:>10} {:>10} {:>7}",
            summary.package,
            summary.tests,
            summary.matching,
            summary.pass_rate() * 100.0,
            summary.differing,
            summary.missing,
            summary.ry_passing,
            suite(summary.gnu_r_suite_seconds),
            suite(summary.ry_suite_seconds),
            ratio
        );
    }
    println!(
        "Suite times count only the tests. Process wall time, which adds startup and loading the package:"
    );
    for summary in &summaries {
        println!(
            "  {}: GNU R {:.2} s, ry {:.2} s",
            summary.package, summary.gnu_r_wall_seconds, summary.ry_wall_seconds
        );
    }
    println!("Time in the collector during the suite, as gc.time() reports it:");
    for summary in &summaries {
        let share = |gc: Option<f64>, suite: Option<f64>| match (gc, suite) {
            (Some(gc), Some(suite)) if suite > 0.0 => {
                format!("{gc:.2} s ({:.1}%)", gc / suite * 100.0)
            }
            _ => "-".to_string(),
        };
        println!(
            "  {}: GNU R {}, ry {}",
            summary.package,
            share(summary.gnu_r_gc_seconds, summary.gnu_r_suite_seconds),
            share(summary.ry_gc_seconds, summary.ry_suite_seconds)
        );
    }
    if let Some(path) = json {
        let text = match serde_json::to_string_pretty(&summaries) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("could not encode the comparison: {error}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(error) = std::fs::write(path, text) {
            eprintln!("{}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    }
    let all_match = summaries
        .iter()
        .all(|summary| summary.matching == summary.tests);
    if all_match {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
