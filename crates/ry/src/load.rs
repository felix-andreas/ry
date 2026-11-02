//! Package loading for R sessions: the `.Call` routines and file watcher behind `ry repl`, the
//! plan emitter behind `ry load --emit-plan`, and the R code that installs `load_all()`.

use {
    crate::{
        ANALYSIS_STACK_SIZE,
        cli::{self, CommandError},
        config::Config,
        preflight::{self, Verdict},
    },
    loader::{
        LoadError, LoadState, Loader, NamespaceConditions, Plan, parse_namespace_conditions,
        r::{RUNTIME_GNUR, RValue, error_value, failure_value, plan_value, rds},
    },
    repl::{
        ConsoleHooks,
        libr::{self, CallRoutine, Sexp},
    },
    std::{
        ffi::{CStr, c_char, c_int},
        fmt, fs, io,
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        sync::{LazyLock, Mutex, PoisonError},
        thread,
        time::Duration,
    },
};

/// The loader of this process's embedded R session, shared by the `.Call` routines and the
/// watcher. R calls the routines on its one thread, so the lock is never contended.
static LOADER: LazyLock<Mutex<Loader>> = LazyLock::new(Mutex::default);

/// R statements that install `load_all()` in `tools:ry`, talking to this process.
pub fn embedded_runtime() -> String {
    runtime(
        r#".ry_request <- function(root, full, conditions) {
  plan_file <- tempfile(fileext = ".rds")
  on.exit(unlink(plan_file))
  failure <- .Call("ry_loader_plan", root, plan_file, full, conditions, PACKAGE = "(embedding)")
  if (!is.null(failure)) stop(failure, call. = FALSE)
  readRDS(plan_file)
}
.ry_commit <- function(root) {
  invisible(.Call("ry_loader_commit", root, PACKAGE = "(embedding)"))
}
.ry_callers <- function(root, names) {
  callers <- .Call("ry_loader_callers", root, paste(names, collapse = "\n"), PACKAGE = "(embedding)")
  if (is.null(callers)) stop("ry's loader panicked; please report this", call. = FALSE)
  if (!nzchar(callers)) character() else strsplit(callers, "\n", fixed = TRUE)[[1L]]
}
.ry_wait_edits <- function(root, timeout) {
  edits <- .Call("ry_loader_wait_edits", root, as.character(round(timeout * 1000)),
                 PACKAGE = "(embedding)")
  if (is.null(edits)) stop("ry's loader panicked; please report this", call. = FALSE)
  if (startsWith(edits, "!")) stop(substring(edits, 2L), call. = FALSE)
  if (!nzchar(edits)) return(character())
  edits <- strsplit(edits, "\n", fixed = TRUE)[[1L]]
  edits[edits == "*"] <- NA_character_
  edits
}"#,
    )
}

/// R statements that install `load_all()` in `tools:ry` for a plain R session, running the
/// `ry` executable to plan each load.
pub fn process_runtime() -> String {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| "ry".to_owned());
    runtime(&format!(
        r#".ry_executable <- function() Sys.getenv("RY_EXECUTABLE", {executable})
.ry_state_file <- function(root) {{
  file.path(tempdir(), paste0("ry-load-", gsub("[^A-Za-z0-9]", "_", root), ".json"))
}}
.ry_request <- function(root, full, conditions) {{
  plan_file <- tempfile(fileext = ".rds")
  conditions_file <- tempfile(fileext = ".txt")
  on.exit(unlink(c(plan_file, conditions_file)))
  writeLines(enc2utf8(conditions), conditions_file, useBytes = TRUE)
  arguments <- c("load", "--emit-plan", shQuote(plan_file), "--state",
                 shQuote(.ry_state_file(root)), "--conditions", shQuote(conditions_file),
                 if (full) "--full", shQuote(root))
  status <- system2(.ry_executable(), arguments)
  if (!identical(status, 0L)) stop("`ry load` failed with status ", status, call. = FALSE)
  readRDS(plan_file)
}}
.ry_commit <- function(root) {{
  state <- .ry_state_file(root)
  pending <- paste0(state, ".pending")
  if (file.exists(pending)) file.rename(pending, state)
  invisible(TRUE)
}}
.ry_callers <- function(root, names) names"#,
        executable = r_string(&executable)
    ))
}

/// Installs the runtime in `tools:ry`. Its functions are byte-compiled once per runtime and R
/// version and cached, because R would otherwise compile each one the first time it runs,
/// which costs more than a typical reload.
///
/// The runtime also binds each name of ry's standard library that GNU R lacks to a function that
/// explains the library runs only on ry's runtime, which reads better than GNU R's "could not
/// find function". A constant such as `Bool.true` becomes an active binding that stops with the
/// same explanation when it is read. A name the session already has, such as `col` or
/// data.table's `copy`, keeps its meaning.
fn runtime(transport: &str) -> String {
    let names: Vec<String> = include_str!("../../../types/ry.exports")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|name| format!("\"{name}\""))
        .collect();
    // A declaration whose type is not a function type declares a constant.
    let constants: Vec<String> = include_str!("../../../types/ry.Rtypes")
        .lines()
        .filter_map(|line| line.split_once(" : "))
        .filter(|(name, declared)| !name.contains(' ') && !declared.contains("fn("))
        .map(|(name, _)| format!("\"{name}\""))
        .collect();
    let source = format!(
        r#"{transport}
{RUNTIME_GNUR}
.ry_standard_library_message <- function(name) paste0(
  "`", name, "` is part of ry's standard library, which only ry's runtime provides, ",
  "and this session runs on GNU R; run the code with `ry run` or `ry load` instead"
)
.ry_standard_library_name <- function(name) {{
  force(name)
  function(...) stop(.ry_standard_library_message(paste0(name, "()")), call. = FALSE)
}}
.ry_standard_library_names <- c({names})
.ry_standard_library_constants <- c({constants})"#,
        names = names.join(", "),
        constants = constants.join(", ")
    );
    let cache = dirs::cache_dir()
        .map(|directory| directory.join("ry").to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        r#"local({{
  if ("tools:ry" %in% search()) detach("tools:ry", character.only = TRUE)
  cache <- {cache}
  cache <- if (nzchar(cache)) file.path(cache, paste0("runtime-{hash:016x}-", getRversion(), ".rds"))
  runtime <- if (length(cache) && file.exists(cache)) tryCatch(readRDS(cache), error = function(error) NULL)
  if (!is.environment(runtime)) {{
    runtime <- new.env(parent = baseenv())
    eval(parse(text = {source}, keep.source = FALSE), runtime)
    for (name in ls(runtime, all.names = TRUE)) {{
      if (is.function(runtime[[name]])) runtime[[name]] <- compiler::cmpfun(runtime[[name]])
    }}
    if (length(cache)) {{
      dir.create(dirname(cache), recursive = TRUE, showWarnings = FALSE)
      partial <- tempfile(tmpdir = dirname(cache))
      saved <- tryCatch({{ saveRDS(runtime, partial); TRUE }}, error = function(error) FALSE)
      if (saved) file.rename(partial, cache) else unlink(partial)
    }}
  }}
  # Decided per session, not cached: a name an attached package exports keeps its meaning.
  for (name in runtime$.ry_standard_library_names) {{
    if (exists(name)) next
    if (name %in% runtime$.ry_standard_library_constants) {{
      makeActiveBinding(name, local({{
        message <- runtime$.ry_standard_library_message(name)
        function() stop(message, call. = FALSE)
      }}), runtime)
    }} else {{
      runtime[[name]] <- runtime$.ry_standard_library_name(name)
    }}
  }}
  runtime$.ry_packages <- new.env(parent = emptyenv())
  runtime$.ry_timing <- new.env(parent = emptyenv())
  runtime$.ry_timing$last <- 0
  attach(runtime, name = "tools:ry", warn.conflicts = FALSE)
  invisible()
}})"#,
        cache = r_string(&cache),
        hash = semantics::load_index::fnv1a(source.as_bytes()),
        source = r_string(&source),
    )
}

/// A raw R string literal (R >= 4.0) holding `text`, with a delimiter `text` never contains.
pub fn r_string(text: &str) -> String {
    let mut dashes = String::from("-");
    while text.contains(&format!("]{dashes}\"")) {
        dashes.push('-');
    }
    format!("r\"{dashes}[{text}]{dashes}\"")
}

/// The statement `ry repl` and `ry load` run to load the package at `root`.
pub fn load_statement(root: &Path, catch: bool) -> String {
    let call = format!("load_all({})", r_string(&root.to_string_lossy()));
    if catch {
        // Startup code runs outside R's top-level loop, so nothing may escape it: an interrupt
        // there would jump to a context that no longer exists and end R.
        format!(
            "tryCatch({call}, error = function(error) message(\"ry: \", conditionMessage(error)), \
             interrupt = function(interrupt) message(\"ry: loading interrupted\"))"
        )
    } else {
        call
    }
}

pub fn call_routines() -> Vec<CallRoutine> {
    vec![
        CallRoutine {
            name: c"ry_loader_plan",
            function: ry_loader_plan as *const (),
            arguments: 4,
        },
        CallRoutine {
            name: c"ry_loader_commit",
            function: ry_loader_commit as *const (),
            arguments: 1,
        },
        CallRoutine {
            name: c"ry_loader_callers",
            function: ry_loader_callers as *const (),
            arguments: 2,
        },
        CallRoutine {
            name: c"ry_loader_wait_edits",
            function: ry_loader_wait_edits as *const (),
            arguments: 2,
        },
    ]
}

/// Plans a load into `plan_file`, returning `NULL`, or an error message R raises.
extern "C" fn ry_loader_plan(root: Sexp, plan_file: Sexp, full: Sexp, conditions: Sexp) -> Sexp {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: R passes R objects to a `.Call` routine.
        let arguments = unsafe {
            (
                libr::string_argument(root),
                libr::string_argument(plan_file),
                libr::logical_argument(full),
                libr::string_argument(conditions),
            )
        };
        let (Some(root), Some(plan_file), full, Some(conditions)) = arguments else {
            return Err("ry_loader_plan needs a root, a plan file and conditions".to_owned());
        };
        let conditions = parse_namespace_conditions(&conditions);
        let root = PathBuf::from(root);
        let value = on_analysis_thread(|| {
            let mut loader = LOADER.lock().unwrap_or_else(PoisonError::into_inner);
            loader.set_strict(strict_config(&root));
            match loader.plan(&root, full, &conditions) {
                Ok(plan) => strict_value(&plan).unwrap_or_else(|| plan_value(&plan, true)),
                Err(failure) => failure_value(&failure),
            }
        });
        fs::write(&plan_file, rds(&value))
            .map_err(|error| format!("cannot write {plan_file}: {error}"))
    }));
    match outcome {
        Ok(Ok(())) => libr::null(),
        Ok(Err(message)) => libr::string_value(&message),
        Err(_) => libr::string_value("ry's loader panicked; please report this"),
    }
}

extern "C" fn ry_loader_commit(root: Sexp) -> Sexp {
    let committed = catch_unwind(|| {
        // SAFETY: R passes R objects to a `.Call` routine.
        let root = unsafe { libr::string_argument(root) }?;
        let root = loader::find_package_root(Path::new(&root)).ok()?;
        LOADER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit(&root);
        Some(())
    });
    if committed.is_err() {
        return libr::string_value("ry's loader panicked; please report this");
    }
    libr::null()
}

/// [`Loader::callers`], with the names in and out joined by line breaks, which spares the
/// embedding a character-vector API.
extern "C" fn ry_loader_callers(root: Sexp, names: Sexp) -> Sexp {
    let callers = catch_unwind(|| {
        // SAFETY: R passes R objects to a `.Call` routine.
        let (root, names) = unsafe { (libr::string_argument(root), libr::string_argument(names)) };
        let root = loader::find_package_root(Path::new(&root?)).ok()?;
        let changed: Vec<String> = names?.lines().map(str::to_owned).collect();
        let callers = LOADER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .callers(&root, &changed);
        Some(callers.join("\n"))
    });
    match callers {
        Ok(Some(callers)) => libr::string_value(&callers),
        Ok(None) => libr::string_value(""),
        Err(_) => libr::null(),
    }
}

/// [`Loader::wait_edits`], with the timeout in milliseconds as text and the edited paths joined
/// by line breaks. A line `*` means notifications were lost, and a leading `!` marks an error
/// message instead.
extern "C" fn ry_loader_wait_edits(root: Sexp, timeout: Sexp) -> Sexp {
    let edits = catch_unwind(|| {
        // SAFETY: R passes R objects to a `.Call` routine.
        let (root, timeout) =
            unsafe { (libr::string_argument(root), libr::string_argument(timeout)) };
        let (Some(root), Some(timeout)) = (root, timeout) else {
            return "!ry_loader_wait_edits needs a root and a timeout".to_owned();
        };
        let Ok(milliseconds) = timeout.parse::<u64>() else {
            return format!("!{timeout} is not a timeout in milliseconds");
        };
        let root = match loader::find_package_root(Path::new(&root)) {
            Ok(root) => root,
            Err(error) => return format!("!{error}"),
        };
        let edits = LOADER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wait_edits(&root, Duration::from_millis(milliseconds));
        match edits {
            Ok(edits) => {
                let mut lines: Vec<String> = edits
                    .paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                if edits.lost {
                    lines.push("*".to_owned());
                }
                lines.join("\n")
            }
            Err(error) => format!("!{error}"),
        }
    });
    match edits {
        Ok(edits) => libr::string_value(&edits),
        Err(_) => libr::null(),
    }
}

/// Starts analyzing the package at `root` on a thread of its own, so the analysis overlaps
/// starting R and the `load_all()` that follows finds it done; that load waits for the
/// loader while this holds it.
pub fn prepare(root: PathBuf) {
    let spawned = thread::Builder::new()
        .name("ry-loader-prepare".to_owned())
        .stack_size(ANALYSIS_STACK_SIZE)
        .spawn(move || {
            let mut loader = LOADER.lock().unwrap_or_else(PoisonError::into_inner);
            loader.set_strict(strict_config(&root));
            loader.prepare(&root);
        });
    if let Err(error) = spawned {
        // Preparing only saves time: without the thread, the load derives everything itself.
        drop(error);
    }
}

/// Analysis recurses as deeply as the code nests, so it gets the analysis stack rather than
/// R's thread's.
fn on_analysis_thread<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    thread::scope(|scope| {
        thread::Builder::new()
            .name("ry-loader".to_owned())
            .stack_size(ANALYSIS_STACK_SIZE)
            .spawn_scoped(scope, work)
            .expect("the loader thread should spawn")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Reloads changed packages before a command runs, so the command sees the code on disk.
pub struct ReloadWatcher;

impl ConsoleHooks for ReloadWatcher {
    fn before_command(&mut self) -> Option<String> {
        let changed = LOADER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .changed_roots();
        let roots: Vec<String> = changed
            .iter()
            .map(|root| r_string(&root.to_string_lossy()))
            .collect();
        (!roots.is_empty()).then(|| format!(".ry_autoreload(c({}))", roots.join(", ")))
    }
}

#[derive(Debug)]
pub enum EmitError {
    Io(PathBuf, io::Error),
    State(PathBuf, serde_json::Error),
}

impl fmt::Display for EmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::Io(path, error) => write!(formatter, "{}: {error}", path.display()),
            EmitError::State(path, error) => {
                write!(
                    formatter,
                    "{} is not a ry load state: {error}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for EmitError {}

/// `ry load --emit-plan`: plans a load against the state a previous call saved, writes the
/// plan, and saves the new state beside the old one as `<state>.pending`, which the R side
/// renames once the plan applied. A planning failure is written into the plan for R to raise.
pub fn emit_plan(
    path: &Path,
    plan_file: &Path,
    state_file: &Path,
    conditions_file: Option<&Path>,
    full: bool,
) -> Result<(), EmitError> {
    let conditions = match conditions_file {
        Some(file) => parse_namespace_conditions(
            &fs::read_to_string(file).map_err(|error| EmitError::Io(file.to_path_buf(), error))?,
        ),
        None => NamespaceConditions::default(),
    };
    let mut loader = Loader::default();
    loader.set_strict(strict_config(path));
    if !full {
        match fs::read_to_string(state_file) {
            Ok(text) => {
                let state: LoadState = serde_json::from_str(&text)
                    .map_err(|error| EmitError::State(state_file.to_path_buf(), error))?;
                // A state whose package moved or vanished is no state at all.
                if let Err(error) = loader.restore(state) {
                    return write_plan(plan_file, &error_value(&error.to_string()));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(EmitError::Io(state_file.to_path_buf(), error)),
        }
    }
    let value = match loader.plan(path, full, &conditions) {
        Ok(plan) => {
            if let Some(state) = loader.pending(&plan.root) {
                let pending = pending_state_path(state_file);
                let text = serde_json::to_string(state)
                    .map_err(|error| EmitError::State(pending.clone(), error))?;
                fs::write(&pending, text).map_err(|error| EmitError::Io(pending, error))?;
            }
            strict_value(&plan).unwrap_or_else(|| plan_value(&plan, true))
        }
        Err(failure) => failure_value(&failure),
    };
    write_plan(plan_file, &value)
}

fn write_plan(plan_file: &Path, value: &RValue) -> Result<(), EmitError> {
    fs::write(plan_file, rds(value)).map_err(|error| EmitError::Io(plan_file.to_path_buf(), error))
}

fn pending_state_path(state_file: &Path) -> PathBuf {
    let mut name = state_file.as_os_str().to_owned();
    name.push(".pending");
    PathBuf::from(name)
}

/// The package root `ry repl` loads: the package at or above the working directory, if any.
pub fn current_package() -> Option<PathBuf> {
    loader::find_package_root(Path::new("."))
        .ok()
        .filter(|root| root.join("R").is_dir())
}

/// For `ry load`'s error reporting.
pub fn package_root(path: &Path) -> Result<PathBuf, LoadError> {
    loader::find_package_root(path)
}

/// The error a load of a strict package raises instead of running `plan`, when strict mode
/// refuses the package. A plan that changes nothing needs no check.
fn strict_value(plan: &Plan) -> Option<RValue> {
    if !plan.strict || plan.is_noop() {
        return None;
    }
    let reason = match check_strict_package(&plan.root) {
        StrictCheck::Passed => return None,
        StrictCheck::Refused => "strict mode refused it",
        StrictCheck::Failed => "its strict check failed",
    };
    Some(error_value(&format!(
        "{} was not loaded, because {reason}",
        plan.package
    )))
}

/// The outcome of [`check_strict_package`], as ry's runtime receives it from
/// [`strict_check_on_ry_runtime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrictCheck {
    Passed = 0,
    Refused = 1,
    Failed = 2,
}

/// Runs strict mode's check of the package at `root`, as `ry load` does before its first
/// load, printing what it finds. A package that passed unchanged is remembered, so checking it
/// again costs a hash of its files.
fn check_strict_package(root: &Path) -> StrictCheck {
    let config = match Config::discover(root) {
        Ok(config) => config,
        Err(error) => {
            cli::report(&error);
            return StrictCheck::Failed;
        }
    };
    match preflight::check_package(root, &config) {
        Ok(Verdict::Run) => StrictCheck::Passed,
        Ok(Verdict::Refuse) => StrictCheck::Refused,
        Err(CommandError) => StrictCheck::Failed,
    }
}

/// The host side of `loader::ry_runtime::set_check`: ry's runtime calls it before a load of a
/// strict package runs anything.
pub extern "C" fn strict_check_on_ry_runtime(root: *const c_char) -> c_int {
    let Some(root) = runtime_path(root) else {
        return StrictCheck::Failed as c_int;
    };
    // Unwinding into the runtime's frames aborts, so a panic in the check, whose message the
    // panic hook has printed, counts as a failed check.
    catch_unwind(|| check_strict_package(&root)).unwrap_or(StrictCheck::Failed) as c_int
}

/// The host side of the strict query of `loader::ry_runtime::set_check`: 1 when the
/// configuration that governs the package at `root` turns strict mode on, as [`strict_config`]
/// decides for GNU R's loader.
pub extern "C" fn strict_config_on_ry_runtime(root: *const c_char) -> c_int {
    runtime_path(root)
        .and_then(|root| catch_unwind(|| strict_config(&root)).ok())
        .map_or(0, c_int::from)
}

fn runtime_path(path: *const c_char) -> Option<PathBuf> {
    if path.is_null() {
        return None;
    }
    // SAFETY: the runtime passes a NUL-terminated path that outlives the call.
    let path = unsafe { CStr::from_ptr(path) };
    Some(PathBuf::from(path.to_string_lossy().into_owned()))
}

/// Whether the configuration that governs `root` turns strict mode on. A broken config counts as
/// not strict here, because the preflight that runs before every load reports it.
fn strict_config(root: &Path) -> bool {
    Config::discover(root).is_ok_and(|config| config.check.strict)
}
