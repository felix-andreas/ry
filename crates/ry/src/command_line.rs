//! ry's command line: the clap model of every subcommand, and how `ry run`, `ry R` and the
//! legacy `ry repl` resolve to one [`Session`] shape, so `main` dispatches on what to run rather
//! than on how it was spelled.

use {
    crate::cli::{MinSeverity, OutputFormat},
    clap::{Args, Parser, Subcommand, ValueEnum},
    std::path::PathBuf,
};

#[derive(Debug, Parser)]
// Clap defaults to the package name, `ry-lang`; the command is `ry`.
#[command(name = "ry", version, after_help = experimental_features_help())]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Ignored ... here only to please VS Code
    #[arg(long, default_value_t = true, hide = true)]
    pub stdio: bool,
    // Documented only in the root help's trailing section: a clap global cannot be
    // hidden from subcommand help selectively.
    #[arg(long, global = true, hide = true)]
    pub experimental_features: Option<Vec<String>>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
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
        #[arg(long, value_enum, default_value = "human")]
        output: OutputFormat,
        /// Only report diagnostics at or above this severity (and only they
        /// affect the exit code)
        #[arg(long, value_enum, default_value = "warning")]
        min_severity: MinSeverity,
    },
    /// Run the formatter on the given files or directories
    #[command(alias = "format")]
    Fmt {
        /// R files to format
        files: Option<Vec<PathBuf>>,
        /// Exit with error if files would be modified without making changes
        #[arg(long, default_value_t = false)]
        check: bool,
        /// Show diff instead of modifying files; exit with error if changes needed
        #[arg(long, default_value_t = false)]
        diff: bool,
        /// Print verbose output
        #[arg(short, long, default_value_t = false)]
        verbose: bool,
    },
    /// Run an R script on ry's runtime, or open ry's console without one
    ///
    /// Everything after FILE goes to the script. A top-level error in the script exits 1,
    /// and so does a script strict mode refuses.
    Run {
        #[command(flatten)]
        session: SessionArguments,
        /// Kept for scripts written before `ry R` existed: `gnu-r` runs on GNU R
        #[arg(long, value_enum, hide = true)]
        runtime: Option<Runtime>,
        /// Do not load the package the working directory belongs to into the console
        #[arg(long, conflicts_with = "script")]
        no_load: bool,
    },
    /// Run an R script on GNU R, or open a GNU R console without one
    ///
    /// The console has ry's highlighting and completion, and both the console and scripts
    /// run with inline type annotations erased. Everything after FILE goes to the script.
    #[command(name = "R")]
    R {
        #[command(flatten)]
        session: SessionArguments,
        /// Do not load the package the working directory belongs to into the console
        #[arg(long, conflicts_with = "script")]
        no_load: bool,
    },
    /// The console's earlier name: `ry R`, or with `--runtime ry`, `ry run`
    #[command(hide = true)]
    Repl {
        #[arg(long, value_enum, default_value_t)]
        keybindings: Keybindings,
        /// Execute this R file before the first prompt
        #[arg(long, short)]
        file: Option<PathBuf>,
        #[arg(long)]
        no_load: bool,
        #[arg(long, value_enum)]
        runtime: Option<Runtime>,
    },
    /// Load an R package from source, lazily, and run code against it
    ///
    /// Function definitions, and in strict mode pure constants, are bound lazily, so each is
    /// parsed and evaluated on first use, and other top-level code runs in collation order. The
    /// exit status is 0 when the package loaded and the code ran without a top-level error.
    Load {
        /// The package, or any path inside it
        #[arg(default_value = ".")]
        path: PathBuf,
        /// With `--emit-plan`, plan a full load whatever the state says
        #[arg(long, hide = true, default_value_t = false)]
        full: bool,
        /// `gnu-r` loads on GNU R instead of ry's runtime
        #[arg(long, value_enum, hide = true)]
        runtime: Option<Runtime>,
        /// R code to run after loading (repeatable)
        #[arg(long = "expr", short = 'e')]
        expressions: Vec<String>,
        /// An R file to run after loading
        #[arg(long, short)]
        file: Option<PathBuf>,
        /// Write a load plan for a plain R session to this file instead of loading
        #[arg(long, hide = true)]
        emit_plan: Option<PathBuf>,
        /// Where `--emit-plan` keeps what the session has loaded
        #[arg(long, hide = true, requires = "emit_plan")]
        state: Option<PathBuf>,
        /// Print R code that defines `load_all()` for a plain R session, then exit
        #[arg(long, default_value_t = false, conflicts_with_all = ["emit_plan", "expressions", "file"])]
        r_entry: bool,
    },
    /// Run the language server
    #[command(alias = "lsp")]
    Server {
        /// Ignored ... here only to please VS Code
        #[arg(long, default_value_t = true, hide = true)]
        stdio: bool,
        /// Enable verbose logging (ignored for now)
        #[arg(short, long, default_value_t = false, hide = true)]
        verbose: bool,
        /// Surface internal analysis facts in hover debug sections. This is a
        /// developer aid for working on ry itself. Setting `RY_DEBUG=1` in the
        /// environment turns on the same thing.
        #[arg(long, default_value_t = false)]
        debug: bool,
    },
    /// Debugging and development commands
    #[command(subcommand)]
    Debug(Debug),
}

/// What `ry run` and `ry R` share. Options come before FILE, because everything after it
/// belongs to the script, as with `Rscript`.
#[derive(Debug, Args)]
pub struct SessionArguments {
    /// The R script to run, then the arguments it reads with `commandArgs(trailingOnly =
    /// TRUE)`; without a script, the console opens
    // One positional for both: clap stops reading options only once a trailing positional has
    // its first value, so a separate FILE would let `script.R --help` print ry's help.
    #[arg(trailing_var_arg = true, value_name = "FILE] [ARGS")]
    pub script: Vec<String>,
    /// Run FILE, then open the console instead of exiting
    #[arg(short, long, value_name = "FILE", conflicts_with = "script")]
    pub interactive: Option<PathBuf>,
    /// Editing keybindings for the console
    #[arg(long, value_enum, default_value_t, conflicts_with = "script")]
    pub keybindings: Keybindings,
}

#[derive(Debug, Subcommand)]
pub enum Debug {
    /// Print the syntax tree for the given file
    Ast { path: PathBuf },
    /// Analyze a workspace and report where time and memory go
    AnalysisStats { path: Option<PathBuf> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Runtime {
    /// GNU R, loaded from its installation
    #[value(name = "gnu-r")]
    GnuR,
    /// ry's own interpreter
    Ry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Keybindings {
    #[default]
    Emacs,
    Vi,
}

/// What `ry run`, `ry R` and `ry repl` were asked to do, whatever the spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub runtime: Runtime,
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Run the script and exit with its status.
    Script {
        file: PathBuf,
        arguments: Vec<String>,
    },
    /// Open the console, after running `file` when there is one.
    Console {
        file: Option<PathBuf>,
        keybindings: Keybindings,
        /// Load the package the working directory belongs to; only GNU R's console can yet.
        load_package: bool,
    },
}

impl Command {
    /// The session a running command asks for, or `None` for the commands that only read
    /// source. `runtime_variable` is the value of `RY_RUNTIME`, which a `--runtime` flag
    /// overrides and `ry R` ignores.
    pub fn session(&self, runtime_variable: Option<&str>) -> Option<Session> {
        let session = match self {
            Command::Run {
                session,
                runtime,
                no_load,
            } => Session {
                runtime: resolve_runtime(*runtime, runtime_variable, Runtime::Ry),
                mode: session_mode(session, !no_load),
            },
            Command::R { session, no_load } => Session {
                runtime: Runtime::GnuR,
                mode: session_mode(session, !no_load),
            },
            Command::Repl {
                keybindings,
                file,
                no_load,
                runtime,
            } => Session {
                runtime: resolve_runtime(*runtime, runtime_variable, Runtime::GnuR),
                mode: Mode::Console {
                    file: file.clone(),
                    keybindings: *keybindings,
                    load_package: !no_load,
                },
            },
            _ => return None,
        };
        Some(session)
    }
}

/// The flag wins, then `RY_RUNTIME`, then the command's default. An unknown `RY_RUNTIME`
/// value is ignored, as an unset one is.
pub fn resolve_runtime(
    flag: Option<Runtime>,
    runtime_variable: Option<&str>,
    default: Runtime,
) -> Runtime {
    flag.or(match runtime_variable {
        Some("ry") => Some(Runtime::Ry),
        Some("gnu-r") => Some(Runtime::GnuR),
        _ => None,
    })
    .unwrap_or(default)
}

fn session_mode(session: &SessionArguments, load_package: bool) -> Mode {
    match session.script.split_first() {
        Some((file, arguments)) => Mode::Script {
            file: PathBuf::from(file),
            arguments: arguments.to_vec(),
        },
        None => Mode::Console {
            file: session.interactive.clone(),
            keybindings: session.keybindings,
            load_package,
        },
    }
}

fn experimental_features_help() -> String {
    let features = crate::config::ExperimentalFeatures::KNOWN
        .iter()
        .map(|feature| format!("            {}: {}", feature.name, feature.description))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Global options:\n      --experimental-features <FEATURES>\n          \
         Enable experimental features (space-separated, or \"all\" for every one):\n{features}"
    )
}
