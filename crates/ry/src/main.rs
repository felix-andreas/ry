use clap::Parser;
use ry::cli::{self, CommandError, Outcome};
use ry::command_line::{Cli, Command, Debug, Keybindings, Mode, Runtime, resolve_runtime};
use ry::config::Config;
use ry::load;
use ry::preflight::{self, ConsoleFilter, Erase, Preflight, Verdict};
use ry::repl_completer::AnalysisCompleter;
use ry::server;
use ry::stats::analysis_stats;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs};
use tracing_subscriber::prelude::*;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `mi_option_arena_eager_commit` in mimalloc v3's `mi_option_e`, which `libmimalloc-sys` does
/// not export as a constant.
#[cfg(all(feature = "mimalloc", target_os = "linux"))]
const MI_OPTION_ARENA_EAGER_COMMIT: libmimalloc_sys::mi_option_t = 4;

/// Eagerly committed arenas are backed by transparent huge pages, which the kernel zeroes in full
/// on first touch. Analysis frees and reallocates far more memory than it keeps, so on Linux that
/// zeroing cost a cold check more time than parsing did. mimalloc reads the option when it
/// creates its first arena, which happens before `main`, so the option is set from a constructor.
#[cfg(all(feature = "mimalloc", target_os = "linux"))]
#[used]
#[unsafe(link_section = ".init_array")]
static LAZY_ARENA_COMMIT: extern "C" fn() = {
    extern "C" fn lazy_arena_commit() {
        // SAFETY: setting an option has no preconditions, and no thread exists yet.
        unsafe { libmimalloc_sys::mi_option_set(MI_OPTION_ARENA_EAGER_COMMIT, 0) }
    }
    lazy_arena_commit
};

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

    if let Some(session) = cli.command.session(env::var("RY_RUNTIME").ok().as_deref()) {
        // Embedded R assumes it owns the thread it initializes on, so sessions stay on `main`.
        return match session.mode {
            Mode::Script { file, arguments } => run_script(session.runtime, &file, arguments),
            Mode::Console {
                file,
                keybindings,
                load_package,
            } => run_console(session.runtime, file, keybindings, load_package),
        };
    }

    if let Command::Load {
        path,
        expressions,
        file,
        runtime,
        emit_plan: None,
        r_entry: false,
        ..
    } = &cli.command
    {
        return run_load(path, expressions, file.as_deref(), *runtime);
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
                    cli::error(&error.to_string());
                    ExitCode::from(2)
                }
            }
        }
        Command::Load { r_entry: true, .. } => {
            println!("{}", load::process_runtime());
            ExitCode::SUCCESS
        }
        Command::Run { .. } | Command::R { .. } | Command::Repl { .. } | Command::Load { .. } => {
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

/// Runs `file` and exits with its status: 1 for a top-level error or a script strict mode
/// refuses, 2 when the script cannot be read or the runtime cannot start.
fn run_script(runtime: Runtime, file: &Path, arguments: Vec<String>) -> ExitCode {
    let source = match fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => {
            cli::error(&format!("cannot read {}: {error}", file.display()));
            return ExitCode::from(2);
        }
    };
    let Some(mut preflight) = Preflight::discover(file, file.to_path_buf()) else {
        return ExitCode::from(2);
    };
    if preflight.check(&source) == Verdict::Refuse {
        return ExitCode::from(1);
    }
    match runtime {
        Runtime::Ry => runtime_exit(repl::run_on_ry_runtime(
            Some(file),
            None,
            &arguments,
            runtime_flags(preflight.config()),
        )),
        Runtime::GnuR => gnu_r_exit(repl::run(repl::RunOptions {
            routines: load::call_routines(),
            startup: vec![load::embedded_runtime()],
            file: Some(file.to_path_buf()),
            arguments,
            batch: true,
            filter: Some(Box::new(Erase)),
            ..repl::RunOptions::default()
        })),
    }
}

fn run_console(
    runtime: Runtime,
    file: Option<PathBuf>,
    keybindings: Keybindings,
    load_package: bool,
) -> ExitCode {
    let script = match &file {
        Some(file) => match fs::read_to_string(file) {
            Ok(source) => Some(source),
            Err(error) => {
                cli::error(&format!("cannot read {}: {error}", file.display()));
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let working_directory = match env::current_dir() {
        Ok(directory) => directory,
        Err(error) => {
            cli::error(&format!("cannot read the working directory: {error}"));
            return ExitCode::from(2);
        }
    };
    let console = PathBuf::from("<console>");
    let Some(mut preflight) =
        Preflight::discover(&working_directory, file.clone().unwrap_or(console.clone()))
    else {
        return ExitCode::from(2);
    };
    // The script is checked under its own name and then fed as console input, so the console's
    // filter does not check it a second time, and later commands see its definitions.
    let input = match script {
        Some(script) => {
            if preflight.check(&script) == Verdict::Refuse {
                return ExitCode::from(1);
            }
            preflight.rename(console);
            match runtime {
                Runtime::GnuR => preflight::erase(&script),
                Runtime::Ry => script,
            }
        }
        None => String::new(),
    };
    let flags = runtime_flags(preflight.config());
    let keybindings = match keybindings {
        Keybindings::Emacs => repl::Keybindings::Emacs,
        Keybindings::Vi => repl::Keybindings::Vi,
    };
    let filter: Box<dyn repl::InputFilter> =
        Box::new(ConsoleFilter::new(preflight, runtime == Runtime::GnuR));
    let completer = Some(Box::new(AnalysisCompleter::new()) as Box<dyn repl::SessionCompleter>);
    match runtime {
        // ry's runtime has no package loader in its console yet, so the console starts bare.
        Runtime::Ry => runtime_exit(repl::run_console_on_ry_runtime(
            repl::RunOptions {
                keybindings,
                input,
                filter: Some(filter),
                completer,
                ..repl::RunOptions::default()
            },
            flags,
        )),
        Runtime::GnuR => {
            let mut startup = vec![load::embedded_runtime()];
            if let Some(root) = load_package.then(load::current_package).flatten() {
                startup.push(load::load_statement(&root, true));
            }
            gnu_r_exit(repl::run(repl::RunOptions {
                keybindings,
                routines: load::call_routines(),
                startup,
                input,
                completer,
                hooks: Some(Box::new(load::ReloadWatcher)),
                filter: Some(filter),
                ..repl::RunOptions::default()
            }))
        }
    }
}

/// Loads the package at `path` into a fresh session, runs `expressions` and then `file` against
/// it, and exits with the session's status.
fn run_load(
    path: &Path,
    expressions: &[String],
    file: Option<&Path>,
    runtime: Option<Runtime>,
) -> ExitCode {
    let root = match load::package_root(path) {
        Ok(root) => root,
        Err(error) => {
            cli::error(&error.to_string());
            return ExitCode::from(2);
        }
    };
    // A new session has nothing loaded, so its first load is always a full one.
    let mut input = load::load_statement(&root, false);
    for expression in expressions {
        input.push('\n');
        input.push_str(expression);
    }
    let Some(preflight) = Preflight::discover(&root, root.clone()) else {
        return ExitCode::from(2);
    };
    match preflight::check_package(&root, preflight.config()) {
        Ok(Verdict::Run) => {}
        Ok(Verdict::Refuse) => return ExitCode::from(1),
        Err(CommandError) => return ExitCode::from(2),
    }
    let runtime_variable = env::var("RY_RUNTIME").ok();
    match resolve_runtime(runtime, runtime_variable.as_deref(), Runtime::Ry) {
        Runtime::Ry => runtime_exit(repl::run_on_ry_runtime(
            file,
            Some(&input),
            &[],
            runtime_flags(preflight.config()),
        )),
        Runtime::GnuR => gnu_r_exit(repl::run(repl::RunOptions {
            routines: load::call_routines(),
            startup: vec![load::embedded_runtime()],
            input,
            file: file.map(Path::to_path_buf),
            batch: true,
            ..repl::RunOptions::default()
        })),
    }
}

/// With inline annotations turned off they are erased and nothing checks them, at run time
/// as in analysis. With object systems turned off, user code cannot define S4, R6 or Reference
/// Classes. In strict mode, `ry load` binds a package's pure constants lazily.
fn runtime_flags(config: &Config) -> repl::RuntimeFlags {
    repl::RuntimeFlags {
        annotation_checks: config.check.inline_annotations,
        object_systems: config.check.object_systems,
        strict: config.check.strict,
    }
}

/// ry's runtime returns the script's status; failing to start it is ry's failure, so 2.
fn runtime_exit(result: Result<i32, repl::ReplError>) -> ExitCode {
    match result {
        Ok(status) => ExitCode::from(status.clamp(0, 255) as u8),
        Err(error) => {
            cli::error(&error.to_string());
            ExitCode::from(2)
        }
    }
}

/// GNU R quits the process itself with the script's status, so returning means success.
fn gnu_r_exit(result: Result<(), repl::ReplError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            cli::error(&error.to_string());
            ExitCode::from(2)
        }
    }
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
