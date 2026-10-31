//! Package loading for R sessions: the `.Call` routines and file watcher behind `ry repl`, the
//! plan emitter behind `ry load --emit-plan`, and the R code that installs `load_all()`.

use crate::ANALYSIS_STACK_SIZE;
use loader::gnur::{RUNTIME, error_value, plan_value, rds};
use loader::{LoadError, LoadState, Loader};
use repl::ConsoleHooks;
use repl::libr::{self, CallRoutine, Sexp};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;
use std::time::UNIX_EPOCH;

/// The loader of this process's embedded R session, shared by the `.Call` routines and the
/// watcher. R calls the routines on its one thread, so the lock is never contended.
static SESSION: LazyLock<Mutex<Session>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Session {
    loader: Loader,
    /// Per root: the fingerprint of the sources a pending plan read, and of the ones the
    /// session has loaded.
    fingerprints: HashMap<PathBuf, (Option<u64>, Option<u64>)>,
}

/// R statements that install `load_all()` in `tools:ry`, talking to this process.
pub fn embedded_runtime() -> String {
    runtime(
        r#".ry_request <- function(root, plan_file, full) {
  failure <- .Call("ry_loader_plan", root, plan_file, full, PACKAGE = "(embedding)")
  if (!is.null(failure)) stop(failure, call. = FALSE)
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
        r#".ry_executable <- Sys.getenv("RY_EXECUTABLE", {executable})
.ry_state_file <- function(root) {{
  file.path(tempdir(), paste0("ry-load-", gsub("[^A-Za-z0-9]", "_", root), ".json"))
}}
.ry_request <- function(root, plan_file, full) {{
  arguments <- c("load", "--emit-plan", shQuote(plan_file), "--state",
                 shQuote(.ry_state_file(root)), if (full) "--full", shQuote(root))
  status <- system2(.ry_executable, arguments)
  if (!identical(status, 0L)) stop("`ry load` failed with status ", status, call. = FALSE)
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

fn runtime(transport: &str) -> String {
    format!(
        r#"local({{
  if ("tools:ry" %in% search()) detach("tools:ry", character.only = TRUE)
  environment <- attach(NULL, name = "tools:ry", warn.conflicts = FALSE)
  eval(parse(text = {transport}, keep.source = FALSE), environment)
  eval(parse(text = {runtime}, keep.source = FALSE), environment)
  invisible()
}})"#,
        transport = r_string(transport),
        runtime = r_string(RUNTIME),
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
        format!(
            "tryCatch({call}, error = function(error) message(\"ry: \", conditionMessage(error)))"
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
        let (Some(root), Some(plan_file)) = (
            libr::string_argument(root),
            libr::string_argument(plan_file),
        ) else {
            return Err("ry_loader_plan needs a root and a plan file".to_owned());
        };
        let full = libr::logical_argument(full);
        let root = PathBuf::from(root);
        let value = on_analysis_thread(|| {
            let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
            let fingerprint = source_fingerprint(&root);
            match session.loader.plan(&root, full) {
                Ok(plan) => {
                    let entry = session.fingerprints.entry(plan.root.clone()).or_default();
                    entry.0 = fingerprint;
                    plan_value(&plan)
                }
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
        let root = libr::string_argument(root)?;
        let root = loader::find_package_root(Path::new(&root)).ok()?;
        let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
        if session.loader.commit(&root)
            && let Some(entry) = session.fingerprints.get_mut(&root)
        {
            entry.1 = entry.0;
        }
        Some(())
    });
    if committed.is_err() {
        return libr::string_value("ry's loader panicked; please report this");
    }
    libr::null()
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
        let session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
        let stale = session
            .fingerprints
            .iter()
            .any(|(root, (_, loaded))| loaded.is_some() && source_fingerprint(root) != *loaded);
        stale.then(|| ".ry_autoreload()".to_owned())
    }
}

/// Of the names, sizes and modification times of everything a load reads, so a change is
/// noticed without reading a file.
fn source_fingerprint(root: &Path) -> Option<u64> {
    let mut records = Vec::new();
    let mut record = |path: &Path| {
        if let Ok(metadata) = fs::metadata(path) {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos());
            records.push(format!(
                "{}\0{}\0{modified}",
                path.display(),
                metadata.len()
            ));
        }
    };
    record(&root.join("DESCRIPTION"));
    record(&root.join("NAMESPACE"));
    let entries = fs::read_dir(root.join("R")).ok()?;
    for entry in entries.flatten() {
        record(&entry.path());
    }
    records.sort();
    Some(semantics::load_index::fnv1a(records.join("\n").as_bytes()))
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
            plan_value(&plan)
        }
        Err(error) => error_value(&error.to_string()),
    };
    write_plan(plan_file, &value)
}

fn write_plan(plan_file: &Path, value: &loader::gnur::RValue) -> Result<(), EmitError> {
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
