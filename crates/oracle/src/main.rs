use {
    clap::{Parser, Subcommand},
    oracle::{
        Engine, EngineError, ProcessEngine,
        bench::{self, Summary},
        diff, engine_from_environment, expected_path, find_cases, gnu_r_available, install,
        packages, parse, render,
    },
    packages::{PackageOutcome, Runtime, Settings},
    std::{
        path::{Path, PathBuf},
        process::ExitCode,
        time::Duration,
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
    Verify { paths: Vec<PathBuf> },
    /// Run files under GNU R and write their `.expected` files.
    Record { paths: Vec<PathBuf> },
    /// Run files under GNU R and under a second engine, and compare the two.
    Compare {
        /// The second engine: `ry` for ry's runtime, `ry-jit` for it with the JIT forced on, or
        /// a command line (default: `$ORACLE_ENGINE`, then the stub).
        #[arg(long)]
        engine: Option<String>,
        paths: Vec<PathBuf>,
    },
    /// Print the outcome of one file under GNU R.
    Show { path: PathBuf },
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
    if !gnu_r_available() {
        eprintln!("GNU R was not found; install R or set ORACLE_R to its path");
        return ExitCode::from(2);
    }
    let gnu_r = ProcessEngine::gnu_r();
    match cli.command {
        Command::Verify { paths } => verify(&gnu_r, &cases(paths)),
        Command::Record { paths } => record(&gnu_r, &cases(paths)),
        Command::Compare { engine, paths } => {
            let second: Box<dyn Engine> = match engine.as_deref() {
                Some(name) => match ProcessEngine::named(name) {
                    Some(engine) => Box::new(engine),
                    None => {
                        eprintln!("--engine needs a command");
                        return ExitCode::from(2);
                    }
                },
                None => engine_from_environment(),
            };
            compare(&gnu_r, second.as_ref(), &cases(paths))
        }
        Command::Bench {
            ry,
            filter,
            timeout,
            gnu_r_only,
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

fn cases(paths: Vec<PathBuf>) -> Vec<PathBuf> {
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
    found
}

fn verify(gnu_r: &ProcessEngine, cases: &[PathBuf]) -> ExitCode {
    let mut failures = 0;
    for case in cases {
        let expected = std::fs::read_to_string(expected_path(case))
            .ok()
            .and_then(|text| parse(&text));
        let Some(expected) = expected else {
            println!("MISSING {}", case.display());
            failures += 1;
            continue;
        };
        match gnu_r.run(case) {
            Ok(actual) => match diff("expected", &expected, "GNU R", &actual) {
                None => println!("ok      {}", case.display()),
                Some(difference) => {
                    println!("DIFFERS {}\n{difference}", case.display());
                    failures += 1;
                }
            },
            Err(error) => {
                println!("ERROR   {}: {error}", case.display());
                failures += 1;
            }
        }
    }
    summarize(cases.len(), failures)
}

fn record(gnu_r: &ProcessEngine, cases: &[PathBuf]) -> ExitCode {
    let mut failures = 0;
    for case in cases {
        match gnu_r.run(case) {
            Ok(outcome) => match std::fs::write(expected_path(case), render(&outcome)) {
                Ok(()) => println!("wrote   {}", expected_path(case).display()),
                Err(error) => {
                    println!("ERROR   {}: {error}", case.display());
                    failures += 1;
                }
            },
            Err(error) => {
                println!("ERROR   {}: {error}", case.display());
                failures += 1;
            }
        }
    }
    summarize(cases.len(), failures)
}

fn compare(gnu_r: &ProcessEngine, second: &dyn Engine, cases: &[PathBuf]) -> ExitCode {
    let mut failures = 0;
    let mut skipped = 0;
    for case in cases {
        let expected = match gnu_r.run(case) {
            Ok(outcome) => outcome,
            Err(error) => {
                println!("ERROR   {}: {error}", case.display());
                failures += 1;
                continue;
            }
        };
        match second.run(case) {
            Ok(actual) => match diff("GNU R", &expected, &second.name(), &actual) {
                None => println!("ok      {}", case.display()),
                Some(difference) => {
                    println!("DIFFERS {}\n{difference}", case.display());
                    failures += 1;
                }
            },
            Err(EngineError::Unavailable(_)) => skipped += 1,
            Err(error) => {
                println!("ERROR   {}: {error}", case.display());
                failures += 1;
            }
        }
    }
    if skipped > 0 {
        println!(
            "skipped {skipped} case(s): engine `{}` is unavailable",
            second.name()
        );
    }
    if skipped == cases.len() {
        return ExitCode::SUCCESS;
    }
    summarize(cases.len() - skipped, failures)
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

fn summarize(total: usize, failures: usize) -> ExitCode {
    println!("{} of {total} passed", total - failures);
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn default_ry() -> PathBuf {
    std::env::var_os("ORACLE_RY").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ry"),
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
