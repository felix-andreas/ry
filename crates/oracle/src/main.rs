use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use oracle::bench::{self, Summary};
use oracle::{
    Engine, EngineError, ProcessEngine, diff, engine_from_environment, expected_path, find_cases,
    gnu_r_available, parse, render,
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
        /// The second engine: `ry` for ry's runtime, or a command line (default: `$ORACLE_ENGINE`,
        /// then the stub).
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
                Some("ry") => Box::new(ProcessEngine::ry()),
                Some(command) => match ProcessEngine::from_command(command) {
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
            let options = bench::Options {
                ry: ry.unwrap_or_else(default_ry),
                filter,
                timeout: Duration::from_secs(timeout),
                skip_ry: gnu_r_only,
                keep,
            };
            run_bench(&packages, &options, verbose, json.as_deref())
        }
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

fn run_bench(packages: &[PathBuf], options: &bench::Options, verbose: bool, json: Option<&Path>) -> ExitCode {
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
        "package", "tests", "match", "rate", "differs", "missing", "ry pass", "GNU R s", "ry s", "ry/R"
    );
    for summary in &summaries {
        let suite = |seconds: Option<f64>| seconds.map_or_else(|| "-".to_string(), |s| format!("{s:.2}"));
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
    println!("(times are the suites' own; load and startup add {})", summaries
        .iter()
        .map(|summary| format!(
            "{}: GNU R {:.2} s, ry {:.2} s wall",
            summary.package, summary.gnu_r_wall_seconds, summary.ry_wall_seconds
        ))
        .collect::<Vec<_>>()
        .join("; "));
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
    let all_match = summaries.iter().all(|summary| summary.matching == summary.tests);
    if all_match { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
