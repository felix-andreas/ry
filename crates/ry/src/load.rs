//! Package loading for R sessions: the `.Call` routines and file watcher behind `ry repl`, the
//! plan emitter behind `ry load --emit-plan`, and the R code that installs `load_all()`.

use {
    crate::{ANALYSIS_STACK_SIZE, config::Config},
    loader::{
        LoadError, LoadState, Loader,
        r::{RUNTIME_GNUR, RValue, error_value, plan_value, rds},
    },
    repl::{
        ConsoleHooks,
        libr::{self, CallRoutine, Sexp},
    },
    std::{
        fmt, fs, io,
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        sync::{LazyLock, Mutex, PoisonError},
        thread,
    },
};

/// The loader of this process's embedded R session, shared by the `.Call` routines and the
/// watcher. R calls the routines on its one thread, so the lock is never contended.
static LOADER: LazyLock<Mutex<Loader>> = LazyLock::new(Mutex::default);

/// R statements that install `load_all()` in `tools:ry`, talking to this process.
pub fn embedded_runtime() -> String {
    runtime(
        r#".ry_request <- function(root, full) {
  plan_file <- tempfile(fileext = ".rds")
  on.exit(unlink(plan_file))
  failure <- .Call("ry_loader_plan", root, plan_file, full, PACKAGE = "(embedding)")
  if (!is.null(failure)) stop(failure, call. = FALSE)
  readRDS(plan_file)
}
.ry_commit <- function(root) {
  invisible(.Call("ry_loader_commit", root, PACKAGE = "(embedding)"))
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
.ry_request <- function(root, full) {{
  plan_file <- tempfile(fileext = ".rds")
  on.exit(unlink(plan_file))
  arguments <- c("load", "--emit-plan", shQuote(plan_file), "--state",
                 shQuote(.ry_state_file(root)), if (full) "--full", shQuote(root))
  status <- system2(.ry_executable(), arguments)
  if (!identical(status, 0L)) stop("`ry load` failed with status ", status, call. = FALSE)
  readRDS(plan_file)
}}
.ry_commit <- function(root) {{
  state <- .ry_state_file(root)
  pending <- paste0(state, ".pending")
  if (file.exists(pending)) file.rename(pending, state)
  invisible(TRUE)
}}"#,
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
fn r_string(text: &str) -> String {
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
            arguments: 3,
        },
        CallRoutine {
            name: c"ry_loader_commit",
            function: ry_loader_commit as *const (),
            arguments: 1,
        },
    ]
}

/// Plans a load into `plan_file`, returning `NULL`, or an error message R raises.
extern "C" fn ry_loader_plan(root: Sexp, plan_file: Sexp, full: Sexp) -> Sexp {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: R passes R objects to a `.Call` routine.
        let arguments = unsafe {
            (
                libr::string_argument(root),
                libr::string_argument(plan_file),
                libr::logical_argument(full),
            )
        };
        let (Some(root), Some(plan_file), full) = arguments else {
            return Err("ry_loader_plan needs a root and a plan file".to_owned());
        };
        let root = PathBuf::from(root);
        let value = on_analysis_thread(|| {
            let mut loader = LOADER.lock().unwrap_or_else(PoisonError::into_inner);
            loader.set_strict(strict_config(&root));
            match loader.plan(&root, full) {
                Ok(plan) => plan_value(&plan, true),
                Err(error) => error_value(&error.to_string()),
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
        let mut loader = LOADER.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = !loader.changed_roots().is_empty();
        changed.then(|| ".ry_autoreload()".to_owned())
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
    full: bool,
) -> Result<(), EmitError> {
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
    let value = match loader.plan(path, full) {
        Ok(plan) => {
            if let Some(state) = loader.pending(&plan.root) {
                let pending = pending_state_path(state_file);
                let text = serde_json::to_string(state)
                    .map_err(|error| EmitError::State(pending.clone(), error))?;
                fs::write(&pending, text).map_err(|error| EmitError::Io(pending, error))?;
            }
            plan_value(&plan, true)
        }
        Err(error) => error_value(&error.to_string()),
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

/// Whether the configuration that governs `root` turns strict mode on. A broken config counts as
/// not strict here, because the preflight that runs before every load reports it.
fn strict_config(root: &Path) -> bool {
    Config::discover(root).is_ok_and(|config| config.check.strict)
}
