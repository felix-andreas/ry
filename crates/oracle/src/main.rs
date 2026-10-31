use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
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
