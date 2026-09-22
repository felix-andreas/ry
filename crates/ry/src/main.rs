use clap::{Parser, Subcommand};
use ry::cli::{self, CommandError, Outcome, OutputFormat};
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::prelude::*;

fn main() -> ExitCode {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(
            tracing_tree::HierarchicalLayer::new(2)
                .with_ansi(false)
                .with_targets(true)
                .with_writer(std::io::stderr),
        )
        .init();

    let cli = Cli::parse();
    let experimental_features = cli::parse_experimental_flags(
        &cli.experimental_features
            .as_ref()
            .map(|flags| {
                flags
                    .iter()
                    .flat_map(|flag| flag.split(' '))
                    .collect::<Vec<&str>>()
            })
            .unwrap_or_default(),
    );

    // Embedded R must run on the MAIN thread: it assumes it owns the thread
    // it initializes on (stack checking, signal expectations).
    let embedded_options = match &cli.command {
        Command::Repl { keybindings, file } => Some(repl::RunOptions {
            keybindings: (*keybindings).into(),
            file: file.clone(),
            batch: false,
            completer: Some(Box::new(ry::repl_completer::AnalysisCompleter::new())),
        }),
        Command::Run { file } => Some(repl::RunOptions {
            keybindings: repl::Keybindings::Emacs,
            file: Some(file.clone()),
            batch: true,
            completer: None,
        }),
        _ => None,
    };
    if let Some(options) = embedded_options {
        return match repl::run(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        };
    }

    // Commands run on a thread with an explicit large stack rather than the
    // main thread: recursive tree walks need more than a constrained
    // `ulimit -s` main stack provides on the deepest trees the grammar
    // produces.
    let command = move || match cli.command {
        Command::Check {
            files,
            output,
            min_severity,
        } => exit_code(cli::check(files.as_deref(), output, min_severity)),
        Command::Fmt {
            files,
            check,
            diff,
            verbose,
        } => exit_code(cli::fmt(files.as_deref(), check, diff, verbose)),
        Command::Server {
            stdio: _,
            verbose: _,
            debug,
        } => {
            // The flag wins; the env var is the ambient fallback for setups
            // where editing server arguments is awkward.
            let debug = debug || matches!(std::env::var("RY_DEBUG").as_deref(), Ok("1"));
            cli::server(experimental_features, debug);
            ExitCode::SUCCESS
        }
        Command::Debug(debug) => match debug {
            Debug::Ast { path } => exit_code(cli::ast(&path).map(|()| Outcome::Clean)),
            Debug::AnalysisStats { path } => {
                exit_code(ry::stats::analysis_stats(path.as_deref()).map(|()| Outcome::Clean))
            }
        },
        Command::Repl { .. } | Command::Run { .. } => {
            unreachable!("handled on the main thread above")
        }
    };
    std::thread::Builder::new()
        .name("ry-command".to_owned())
        .stack_size(ry::ANALYSIS_STACK_SIZE)
        .spawn(command)
        .expect("command thread should spawn")
        .join()
        .expect("command thread should not panic")
}

/// The exit codes are a documented contract: 0 clean, 1 findings
/// (diagnostics, or files a `fmt --check`/`--diff` run would change), 2
/// usage/configuration/IO errors. Clap's own usage errors also exit 2.
fn exit_code(result: Result<Outcome, CommandError>) -> ExitCode {
    match result {
        Ok(Outcome::Clean) => ExitCode::SUCCESS,
        Ok(Outcome::Findings) => ExitCode::from(1),
        Err(CommandError) => ExitCode::from(2),
    }
}

#[derive(Parser)]
// `name` is set explicitly: clap defaults to the Cargo package name, which is
// `ry-lang` because the registry name `ry` was taken. The command the user
// typed, and the one every diagnostic and docs page names, is `ry`.
#[command(name = "ry", version, after_help = experimental_features_help())]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Ignored ... here only to please VS Code
    #[clap(long, default_value_t = true)]
    stdio: bool,
    // Accepted anywhere but documented only in the root help's trailing
    // section (a clap global cannot be hidden from subcommand help
    // selectively, and repeating it under every subcommand is noise).
    #[clap(long, global = true, hide = true)]
    experimental_features: Option<Vec<String>>,
}

fn experimental_features_help() -> String {
    let features = ry::config::ExperimentalFeatures::KNOWN
        .iter()
        .map(|feature| format!("            {}: {}", feature.name, feature.description))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Global options:\n      --experimental-features <FEATURES>\n          \
         Enable experimental features (space-separated, or \"all\" for every one):\n{features}"
    )
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Lint and type-check the given files or directories
    ///
    /// Analysis covers the whole project the paths belong to, which is the
    /// directory holding `ry.toml` or `DESCRIPTION`. Cross-file names therefore
    /// resolve the same however the paths are named, and only reporting is
    /// scoped to what was named. The exit status is 0 for no findings, 1 for
    /// findings, and 2 for a usage or IO failure.
    /// Type errors are opt-in via `[check] typing` in `ry.toml`.
    Check {
        /// R files to check
        files: Option<Vec<PathBuf>>,
        /// Diagnostic output format
        #[clap(long, value_enum, default_value = "human")]
        output: OutputFormat,
        /// Only report diagnostics at or above this severity (and only they
        /// affect the exit code)
        #[clap(long, value_enum, default_value = "warning")]
        min_severity: cli::MinSeverity,
    },
    /// Run the formatter on the given files or directories
    #[clap(alias = "format")]
    Fmt {
        /// R files to format
        files: Option<Vec<PathBuf>>,
        /// Exit with error if files would be modified without making changes
        #[clap(long, default_value_t = false)]
        check: bool,
        /// Show diff instead of modifying files; exit with error if changes needed
        #[clap(long, default_value_t = false)]
        diff: bool,
        /// Print verbose output
        #[clap(short, long, default_value_t = false)]
        verbose: bool,
    },
    /// Run the language server
    #[clap(alias = "lsp")]
    Server {
        /// Ignored ... here only to please VS Code
        #[clap(long, default_value_t = true)]
        stdio: bool,
        /// Enable verbose logging (ignored for now)
        #[clap(short, long, default_value_t = false)]
        verbose: bool,
        /// Surface internal analysis facts in hover debug sections. This is a
        /// developer aid for working on ry itself. Setting `RY_DEBUG=1` in the
        /// environment turns on the same thing.
        #[clap(long, default_value_t = false)]
        debug: bool,
    },
    /// Start an interactive R console (finds and embeds the system R)
    Repl {
        /// Editing keybindings for the console
        #[clap(long, value_enum, default_value_t = ReplKeybindings::Emacs)]
        keybindings: ReplKeybindings,
        /// Execute this R file before the first prompt
        #[clap(long, short)]
        file: Option<PathBuf>,
    },
    /// Run an R file through the embedded R and exit (a top-level error
    /// exits with a failing status)
    Run {
        /// The R file to execute
        file: PathBuf,
    },
    /// Debugging and development commands
    #[command(subcommand)]
    Debug(Debug),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ReplKeybindings {
    Emacs,
    Vi,
}

impl From<ReplKeybindings> for repl::Keybindings {
    fn from(keybindings: ReplKeybindings) -> repl::Keybindings {
        match keybindings {
            ReplKeybindings::Emacs => repl::Keybindings::Emacs,
            ReplKeybindings::Vi => repl::Keybindings::Vi,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Debug {
    /// Print the syntax tree for the given file
    Ast { path: PathBuf },
    /// Analyze a workspace and report where time and memory go
    AnalysisStats { path: Option<PathBuf> },
}
