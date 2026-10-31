use clap::{Parser, Subcommand};
use ry::cli::{self, CommandError, Outcome, OutputFormat};
use ry::load;
use ry::server;
use ry::stats::analysis_stats;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::prelude::*;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
    let experimental_features =
        cli::parse_experimental_flags(cli.experimental_features.as_deref().unwrap_or_default());

    // Embedded R assumes it owns the thread it initializes on, so it must stay on `main`.
    let embedded_options = match &cli.command {
        Command::Repl {
            keybindings,
            file,
            no_load,
        } => {
            let mut startup = vec![load::embedded_runtime()];
            let package = (!no_load).then(load::current_package).flatten();
            if let Some(root) = &package {
                startup.push(load::load_statement(root, true));
            }
            Some(repl::RunOptions {
                keybindings: match keybindings {
                    ReplKeybindings::Emacs => repl::Keybindings::Emacs,
                    ReplKeybindings::Vi => repl::Keybindings::Vi,
                },
                routines: load::call_routines(),
                startup,
                file: file.clone(),
                completer: Some(Box::new(ry::repl_completer::AnalysisCompleter::new())),
                hooks: Some(Box::new(load::ReloadWatcher)),
                ..repl::RunOptions::default()
            })
        }
        Command::Run { file } => Some(repl::RunOptions {
            routines: load::call_routines(),
            startup: vec![load::embedded_runtime()],
            file: Some(file.clone()),
            batch: true,
            ..repl::RunOptions::default()
        }),
        Command::Load {
            path,
            expressions,
            file,
            emit_plan: None,
            r_entry: false,
            ..
        } => {
            let root = match load::package_root(path) {
                Ok(root) => root,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::from(2);
                }
            };
            // A new session has nothing loaded, so its first load is always a full one.
            let mut input = load::load_statement(&root, false);
            for expression in expressions {
                input.push('\n');
                input.push_str(expression);
            }
            Some(repl::RunOptions {
                routines: load::call_routines(),
                startup: vec![load::embedded_runtime()],
                input,
                file: file.clone(),
                batch: true,
                ..repl::RunOptions::default()
            })
        }
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

    // Recursive tree walks can exceed a constrained `ulimit -s` main stack.
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
            let debug = debug || matches!(env::var("RY_DEBUG").as_deref(), Ok("1"));
            server::run(experimental_features, debug);
            ExitCode::SUCCESS
        }
        Command::Debug(debug) => match debug {
            Debug::Ast { path } => exit_code(cli::ast(&path).map(|()| Outcome::Clean)),
            Debug::AnalysisStats { path } => {
                exit_code(analysis_stats(path.as_deref()).map(|()| Outcome::Clean))
            }
        },
        Command::Load {
            path,
            full,
            emit_plan: Some(plan_file),
            state,
            ..
        } => {
            let state = state.unwrap_or_else(|| plan_file.with_extension("state.json"));
            match load::emit_plan(&path, &plan_file, &state, full) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(2)
                }
            }
        }
        Command::Load { r_entry: true, .. } => {
            println!("{}", load::process_runtime());
            ExitCode::SUCCESS
        }
        Command::Repl { .. } | Command::Run { .. } | Command::Load { .. } => {
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

/// Documented contract: 0 clean, 1 findings, 2 usage/configuration/IO errors
/// (clap's own usage errors also exit 2).
fn exit_code(result: Result<Outcome, CommandError>) -> ExitCode {
    match result {
        Ok(Outcome::Clean) => ExitCode::SUCCESS,
        Ok(Outcome::Findings) => ExitCode::from(1),
        Err(CommandError) => ExitCode::from(2),
    }
}

#[derive(Parser)]
// Clap defaults to the package name, `ry-lang`; the command is `ry`.
#[command(name = "ry", version, after_help = experimental_features_help())]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Ignored ... here only to please VS Code
    #[clap(long, default_value_t = true)]
    stdio: bool,
    // Documented only in the root help's trailing section: a clap global cannot be
    // hidden from subcommand help selectively.
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
        /// Do not load the package the working directory belongs to
        #[clap(long, default_value_t = false)]
        no_load: bool,
    },
    /// Load an R package from source into the embedded R, lazily, and run code against it
    ///
    /// Function definitions are bound lazily, so each is parsed and evaluated on first use,
    /// and other top-level code runs in collation order. The exit status is 0 when the
    /// package loaded and the code ran without a top-level error.
    Load {
        /// The package, or any path inside it
        #[clap(default_value = ".")]
        path: PathBuf,
        /// With `--emit-plan`, plan a full load whatever the state says
        #[clap(long, hide = true, default_value_t = false)]
        full: bool,
        /// R code to run after loading (repeatable)
        #[clap(long = "expr", short = 'e')]
        expressions: Vec<String>,
        /// An R file to run after loading
        #[clap(long, short)]
        file: Option<PathBuf>,
        /// Write a load plan for a plain R session to this file instead of loading
        #[clap(long, hide = true)]
        emit_plan: Option<PathBuf>,
        /// Where `--emit-plan` keeps what the session has loaded
        #[clap(long, hide = true, requires = "emit_plan")]
        state: Option<PathBuf>,
        /// Print R code that defines `load_all()` for a plain R session, then exit
        #[clap(long, default_value_t = false, conflicts_with_all = ["emit_plan", "expressions", "file"])]
        r_entry: bool,
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

#[derive(Debug, Subcommand)]
enum Debug {
    /// Print the syntax tree for the given file
    Ast { path: PathBuf },
    /// Analyze a workspace and report where time and memory go
    AnalysisStats { path: Option<PathBuf> },
}
