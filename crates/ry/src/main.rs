use {
    clap::Parser,
    loader::package_cache_path,
    ry::{
        cli::{self, CommandError, Outcome},
        command_line::{Cli, Command, Debug, Keybindings, Mode, Runtime, resolve_runtime},
        config::Config,
        debug_adapter, load,
        preflight::{self, ConsoleFilter, Erase, Preflight, Verdict},
        repl_completer::AnalysisCompleter,
        server,
        stats::analysis_stats,
    },
    std::{
        env,
        ffi::c_int,
        fs,
        path::{Path, PathBuf},
        process::ExitCode,
        sync::{Mutex, OnceLock, PoisonError},
        thread::{self, JoinHandle},
    },
    tracing_subscriber::prelude::*,
};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

/// mimalloc, entered through its unaligned functions whenever they already give the alignment.
/// `mimalloc::MiMalloc` calls the aligned ones for every request, and a cold check makes about
/// twenty million allocations, each of which then pays two extra calls that `mi_malloc` inlines.
/// Like the system allocator's own shortcut, this relies on `mi_malloc` aligning every block of
/// at least 16 bytes to 16, so an alignment of at most 16 that is no larger than the size holds.
/// That is a property of mimalloc's 64-bit bin layout (even word counts and 16-aligned page
/// starts), not of its documented API, so a `libmimalloc-sys` upgrade must re-check it.
#[cfg(feature = "mimalloc")]
struct Allocator;

#[cfg(feature = "mimalloc")]
use std::alloc::{GlobalAlloc, Layout};

#[cfg(feature = "mimalloc")]
const MI_MAX_ALIGN_SIZE: usize = 16;

#[cfg(feature = "mimalloc")]
fn plain(align: usize, size: usize) -> bool {
    align <= MI_MAX_ALIGN_SIZE && align <= size
}

// SAFETY: every block comes from mimalloc and goes back to it, and `plain` only routes a layout
// to the unaligned functions when their blocks are aligned enough for it.
#[cfg(feature = "mimalloc")]
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded to mimalloc with the caller's layout.
        unsafe {
            if plain(layout.align(), layout.size()) {
                libmimalloc_sys::mi_malloc(layout.size()).cast()
            } else {
                libmimalloc_sys::mi_malloc_aligned(layout.size(), layout.align()).cast()
            }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded to mimalloc with the caller's layout.
        unsafe {
            if plain(layout.align(), layout.size()) {
                libmimalloc_sys::mi_zalloc(layout.size()).cast()
            } else {
                libmimalloc_sys::mi_zalloc_aligned(layout.size(), layout.align()).cast()
            }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, _layout: Layout) {
        // SAFETY: `pointer` came from one of the allocation functions above.
        unsafe { libmimalloc_sys::mi_free(pointer.cast()) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `pointer` came from one of the allocation functions above, and the block
        // keeps the caller's alignment.
        unsafe {
            if plain(layout.align(), new_size) {
                libmimalloc_sys::mi_realloc(pointer.cast(), new_size).cast()
            } else {
                libmimalloc_sys::mi_realloc_aligned(pointer.cast(), new_size, layout.align()).cast()
            }
        }
    }
}

/// `mi_option_arena_eager_commit` in mimalloc v3's `mi_option_e`, which `libmimalloc-sys` does
/// not export as a constant.
#[cfg(all(feature = "mimalloc", target_os = "linux"))]
const MI_OPTION_ARENA_EAGER_COMMIT: libmimalloc_sys::mi_option_t = 4;

/// Eagerly committed arenas are backed by transparent huge pages, which the kernel zeroes in full
/// on first touch. Analysis frees and reallocates far more memory than it keeps, so on Linux that
/// zeroing cost a cold check more time than parsing did. The first arena is reserved before
/// `main`, so the option is set from a constructor, as a default that
/// `MIMALLOC_ARENA_EAGER_COMMIT` can still override. `libR.so` has its own allocator and sets the
/// same option in `crates/libr/src/libr.rs`.
#[cfg(all(feature = "mimalloc", target_os = "linux"))]
#[used]
#[unsafe(link_section = ".init_array")]
static LAZY_ARENA_COMMIT: extern "C" fn() = {
    extern "C" fn lazy_arena_commit() {
        // SAFETY: setting an option has no preconditions, and no thread exists yet.
        unsafe { libmimalloc_sys::mi_option_set_default(MI_OPTION_ARENA_EAGER_COMMIT, 0) }
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
    cli::check_experimental_features(cli.experimental_features.as_deref().unwrap_or_default());

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
        return run_load(path, "", expressions, file.as_deref(), *runtime);
    }

    if let Command::Test {
        path,
        filter,
        reporter,
        watch,
        runtime,
    } = &cli.command
    {
        return run_test(
            path,
            filter.as_deref(),
            reporter.as_deref(),
            *watch,
            *runtime,
        );
    }

    if let Command::Document {
        path,
        roclets,
        runtime,
    } = &cli.command
    {
        return run_document(path, roclets, *runtime);
    }

    if let Command::DebugAdapter = &cli.command {
        return run_debug_adapter();
    }

    // Recursive tree walks can exceed a constrained `ulimit -s` main stack.
    let command = move || match cli.command {
        Command::Check {
            files,
            output,
            min_severity,
            fix,
            unsafe_fixes,
            annotate,
            no_cache,
        } => {
            let fix = match (fix, unsafe_fixes) {
                (false, _) => cli::FixMode::Off,
                (true, false) => cli::FixMode::Safe,
                (true, true) => cli::FixMode::All,
            };
            let cache = match no_cache {
                true => ry::check_cache::CacheUse::Off,
                false => ry::check_cache::CacheUse::ReadWrite,
            };
            exit_code(cli::check(
                files.as_deref(),
                output,
                min_severity,
                fix,
                annotate,
                cache,
            ))
        }
        Command::Fmt {
            files,
            check,
            diff,
            range,
            verbose,
        } => exit_code(cli::fmt(files.as_deref(), check, diff, range, verbose)),
        Command::Server {
            stdio: _,
            verbose: _,
            debug,
        } => {
            let debug = debug || matches!(env::var("RY_DEBUG").as_deref(), Ok("1"));
            server::run(debug);
            ExitCode::SUCCESS
        }
        Command::Debug(debug) => match debug {
            Debug::Ast { path } => exit_code(cli::ast(&path).map(|()| Outcome::Clean)),
            Debug::AnalysisStats { path } => {
                exit_code(analysis_stats(path.as_deref()).map(|()| Outcome::Clean))
            }
            Debug::Erase { paths } => exit_code(cli::erase(&paths).map(|()| Outcome::Clean)),
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
        Command::Run { .. }
        | Command::R { .. }
        | Command::Repl { .. }
        | Command::Load { .. }
        | Command::Test { .. }
        | Command::Document { .. }
        | Command::DebugAdapter => {
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
    // R reads a script in the session's encoding, so a Latin-1 script is valid R; the runtime
    // reads the bytes itself, and the check sees them with invalid UTF-8 replaced.
    let source = match fs::read(file) {
        Ok(source) => source,
        Err(error) => {
            cli::error(&format!("cannot read {}: {error}", file.display()));
            return ExitCode::from(2);
        }
    };
    let Some(mut preflight) = Preflight::discover(file, file.to_path_buf()) else {
        return ExitCode::from(2);
    };
    if preflight.check(&String::from_utf8_lossy(&source)) == Verdict::Refuse {
        return ExitCode::from(1);
    }
    match runtime {
        Runtime::Ry => runtime_exit(checking_loads().and_then(|()| {
            repl::run_on_ry_runtime(
                Some(file),
                None,
                &arguments,
                runtime_flags(preflight.config()),
                None,
                None,
            )
        })),
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
    let package = load_package.then(load::current_package).flatten();
    let load = package
        .as_ref()
        .map(|root| load::load_statement(root, true));
    match runtime {
        // ry's runtime carries its own loader, so it needs only the load statement.
        Runtime::Ry => runtime_exit(checking_loads().and_then(|()| {
            repl::run_console_on_ry_runtime(
                repl::RunOptions {
                    keybindings,
                    startup: load.into_iter().collect(),
                    input,
                    filter: Some(filter),
                    completer,
                    package,
                    ..repl::RunOptions::default()
                },
                flags,
            )
        })),
        Runtime::GnuR => {
            if let Some(root) = package {
                load::prepare(root);
            }
            let mut startup = vec![load::embedded_runtime()];
            startup.extend(load);
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

/// The debugged program runs with the flags of the project the working directory belongs to,
/// which is the workspace an editor starts the adapter in.
fn run_debug_adapter() -> ExitCode {
    let working_directory = match env::current_dir() {
        Ok(directory) => directory,
        Err(error) => {
            cli::error(&format!("cannot read the working directory: {error}"));
            return ExitCode::from(2);
        }
    };
    let Some(preflight) = Preflight::discover(&working_directory, working_directory.clone()) else {
        return ExitCode::from(2);
    };
    runtime_exit(debug_adapter::run(runtime_flags(preflight.config())))
}

/// `ry load` with the code that runs the tests, which quits with status 1 when one failed.
fn run_test(
    path: &Path,
    filter: Option<&str>,
    reporter: Option<&str>,
    watch: bool,
    runtime: Option<Runtime>,
) -> ExitCode {
    let root = match load::package_root(path) {
        Ok(root) => root,
        Err(error) => {
            cli::error(&error.to_string());
            return ExitCode::from(2);
        }
    };
    let argument = |value: Option<&str>| value.map_or_else(|| "NULL".to_owned(), load::r_string);
    let run = format!(
        "if (!({driver})({root}, {filter}, {reporter}, {watch})) quit(status = 1L)",
        driver = include_str!("test.R").trim_end(),
        root = load::r_string(&root.to_string_lossy()),
        filter = argument(filter),
        reporter = argument(reporter),
        watch = if watch { "TRUE" } else { "FALSE" },
    );
    run_load(path, "", &[run], None, runtime)
}

/// `ry load` with the code that runs roxygen2 against the lazily loaded namespace.
fn run_document(path: &Path, roclets: &[String], runtime: Option<Runtime>) -> ExitCode {
    let root = match load::package_root(path) {
        Ok(root) => root,
        Err(error) => {
            cli::error(&error.to_string());
            return ExitCode::from(2);
        }
    };
    let roclets = if roclets.is_empty() {
        "NULL".to_owned()
    } else {
        let quoted: Vec<String> = roclets
            .iter()
            .map(|roclet| load::r_string(roclet))
            .collect();
        format!("c({})", quoted.join(", "))
    };
    let cache = package_cache_path(&root, "roxygen").map_or_else(
        || "NULL".to_owned(),
        |path| load::r_string(&path.to_string_lossy()),
    );
    let run = format!(
        "({driver})({root}, {roclets}, {cache})",
        driver = include_str!("document.R").trim_end(),
        root = load::r_string(&root.to_string_lossy()),
    );
    // roxygen2 reads R6 methods' source references, so the load keeps source, as pkgload's does.
    run_load(path, "options(keep.source = TRUE)", &[run], None, runtime)
}

/// Runs `prelude`, loads the package at `path` into a fresh session, runs `expressions` and then
/// `file` against it, and exits with the session's status.
fn run_load(
    path: &Path,
    prelude: &str,
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
    let mut input = format!("{prelude}\n{}", load::load_statement(&root, false));
    for expression in expressions {
        input.push('\n');
        input.push_str(expression);
    }
    let Some(preflight) = Preflight::discover(&root, root.clone()) else {
        return ExitCode::from(2);
    };
    let runtime_variable = env::var("RY_RUNTIME").ok();
    match resolve_runtime(runtime, runtime_variable.as_deref(), Runtime::Ry) {
        Runtime::Ry => {
            // The strict check runs while the runtime starts and the package's definitions
            // bind; the loader waits for it before anything with an effect.
            let config = preflight.config().clone();
            let check_root = root.clone();
            let check = thread::Builder::new()
                .name("ry-strict-check".to_owned())
                .spawn(move || {
                    lower_priority();
                    preflight::check_package(&check_root, &config)
                });
            match check {
                Ok(check) => {
                    *PACKAGE_CHECK.lock().unwrap_or_else(PoisonError::into_inner) = Some(check);
                }
                Err(error) => panic!("the strict check's thread should spawn: {error}"),
            }
            let status = checking_loads().and_then(|()| {
                repl::run_on_ry_runtime(
                    file,
                    Some(&input),
                    &[],
                    runtime_flags(preflight.config()),
                    Some(&root),
                    Some(package_verdict),
                )
            });
            // A load that failed before it asked still reports the check, and a refusal
            // decides the status.
            match package_verdict() {
                0 => runtime_exit(status),
                refused => ExitCode::from(refused as u8),
            }
        }
        Runtime::GnuR => {
            match preflight::check_package(&root, preflight.config()) {
                Ok(Verdict::Run) => {}
                Ok(Verdict::Refuse) => return ExitCode::from(1),
                Err(CommandError) => return ExitCode::from(2),
            }
            gnu_r_exit(repl::run(repl::RunOptions {
                routines: load::call_routines(),
                startup: vec![load::embedded_runtime()],
                input,
                file: file.map(Path::to_path_buf),
                batch: true,
                ..repl::RunOptions::default()
            }))
        }
    }
}

/// Lowers the calling thread's CPU priority, so the strict check yields to the runtime start it
/// overlaps with. The analysis workers it spawns inherit the priority.
#[cfg(target_os = "linux")]
fn lower_priority() {
    // SAFETY: gettid has no preconditions, and setpriority on the calling thread only changes
    // its scheduling. A failure leaves the priority as it was, which is still correct.
    unsafe {
        let thread = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, thread, 10);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_priority() {}

/// The strict check of the package `ry load` is loading, which [`package_verdict`] joins.
static PACKAGE_CHECK: Mutex<Option<JoinHandle<Result<Verdict, CommandError>>>> = Mutex::new(None);

/// [`repl::PackageVerdict`] for `ry load`: waits for the strict check, which has printed any
/// errors by then, and answers 0 to run, 1 to refuse and 2 when the check could not run. A
/// panic in the check ends the process with status 101. The answer is kept, so asking again
/// returns at once.
extern "C" fn package_verdict() -> c_int {
    static STATUS: OnceLock<c_int> = OnceLock::new();
    *STATUS.get_or_init(|| {
        let check = PACKAGE_CHECK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(check) = check else {
            return 0;
        };
        // Unwinding out of an `extern "C"` function aborts, so a panic in the check, whose
        // message the panic hook has printed, ends the process with a panic's usual status.
        let Ok(verdict) = check.join() else {
            std::process::exit(101);
        };
        match verdict {
            Ok(Verdict::Run) => 0,
            Ok(Verdict::Refuse) => 1,
            Err(CommandError) => 2,
        }
    })
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

/// Makes ry's runtime run strict mode's check before every load of a strict package, as GNU
/// R's loader in `ry R` does: `load_all()`, a console's reload, and a script's own loads.
fn checking_loads() -> Result<(), repl::ReplError> {
    repl::set_package_check_on_ry_runtime(
        load::strict_config_on_ry_runtime,
        load::strict_check_on_ry_runtime,
    )
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
