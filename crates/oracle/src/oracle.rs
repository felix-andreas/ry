//! A differential oracle for R: run one file under GNU R and under a second engine, and
//! compare stdout, messages, warnings, errors and the last value.
//!
//! An engine is anything that implements [`Engine`]. [`ProcessEngine`] covers every engine
//! with a command line: it is invoked as `<command…> <script> <report>`, must print the
//! script's own output to stdout, and must write the JSON report that `driver.R` defines.
//! GNU R is one such engine, and ry's runtime will be another.

pub mod bench;
pub mod fuzz;
pub mod gallery;
pub mod install;

use {
    serde::Deserialize,
    similar::TextDiff,
    std::{
        ffi::{OsStr, OsString},
        io::Read,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::{
            OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    },
};

pub mod packages;

const DRIVER: &str = include_str!("driver.R");

pub trait Engine {
    fn name(&self) -> String;
    fn run(&self, script: &Path) -> Result<Outcome, EngineError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The engine does not exist yet, or cannot run here. A comparison skips the case.
    #[error("{0}")]
    Unavailable(String),
    /// The engine started but produced no usable report.
    #[error("{0}")]
    Failed(String),
    /// The engine was killed by a signal before it wrote a report. Some hardened builds of GNU R
    /// abort with "buffer overflow detected" in graphics devices that a plain source build runs,
    /// so a GNU R crash skips the case, naming the build, instead of counting as a difference.
    #[error("{0}")]
    Crashed(String),
}

/// Everything observable about one run, and the package libraries it searched.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub stdout: String,
    pub messages: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub value: Option<Value>,
    /// `R_LIBS` as the script saw it, when the engine reports it. It is not part of the
    /// behavior: [`diff`] refuses to compare runs that searched different libraries, because a
    /// package installed by one engine makes the other warn that it was built under another R.
    /// [`library_environment`] gives every engine the same value, so only a wrapper that sets
    /// `R_LIBS` itself trips it.
    pub libraries: Option<String>,
}

impl PartialEq for Outcome {
    fn eq(&self, other: &Self) -> bool {
        self.stdout == other.stdout
            && self.messages == other.messages
            && self.warnings == other.warnings
            && self.errors == other.errors
            && self.value == other.value
    }
}

impl Eq for Outcome {}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Value {
    pub kind: String,
    pub deparsed: String,
}

#[derive(Deserialize)]
struct Report {
    messages: Vec<String>,
    warnings: Vec<String>,
    errors: Vec<String>,
    value: Option<Value>,
    #[serde(default)]
    libraries: Option<String>,
}

/// The modes of ry's runtime that `--engine` names, which [`KnownFailure::engine`] must be one of.
pub const ENGINES: [&str; 6] = [
    "ry",
    "ry-tier0",
    "ry-jit",
    "ry-gctorture",
    "ry-gctorture-young",
    "ry-jit-gctorture",
];

/// A case that fails on main under one engine, listed in [`KNOWN_FAILURES_FILE`] so that a run
/// tells it apart from a failure a change introduced.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownFailure {
    /// The case's `group__name`, the file name without `.R`.
    pub case: String,
    pub engine: String,
    pub area: String,
    pub reason: String,
    /// Seen failing on a branch but not yet on main.
    #[serde(default)]
    pub provisional: bool,
}

/// How one case's result under one engine counts against [`KNOWN_FAILURES_FILE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    /// A failure the file lists.
    Known,
    /// A failure the file does not list.
    New,
    /// A listed case that passes, so its entry is stale.
    ListedButPassing,
}

/// The verdict for a case that `failed` or passed, given its entry in the known failures.
pub fn verdict(failed: bool, entry: Option<&KnownFailure>) -> Verdict {
    match (failed, entry) {
        (false, None) => Verdict::Passed,
        (false, Some(_)) => Verdict::ListedButPassing,
        (true, Some(_)) => Verdict::Known,
        (true, None) => Verdict::New,
    }
}

/// Whether `verdict` fails the run: a new failure does, and so does a stale entry, so the change
/// that fixes a case also deletes its entry.
pub fn fails_run(verdict: Verdict) -> bool {
    matches!(verdict, Verdict::New | Verdict::ListedButPassing)
}

/// The known failures, beside `Cargo.toml` in this crate.
pub const KNOWN_FAILURES_FILE: &str = "known_failures.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KnownFailuresFile {
    #[serde(default)]
    failure: Vec<KnownFailure>,
}

/// Reads [`KNOWN_FAILURES_FILE`] beside this crate's `Cargo.toml`, as [`parse_known_failures`]
/// checks it.
pub fn known_failures() -> Result<Vec<KnownFailure>, String> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = directory.join(KNOWN_FAILURES_FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    parse_known_failures(&text, directory)
}

/// Parses the known failures in `text`, the file in `directory`, refusing an entry that names an
/// unknown engine, a case missing from `directory/cases` or the same case and engine twice, so a
/// stale or mistyped entry cannot hide.
pub fn parse_known_failures(text: &str, directory: &Path) -> Result<Vec<KnownFailure>, String> {
    let path = directory.join(KNOWN_FAILURES_FILE);
    let file: KnownFailuresFile =
        toml::from_str(text).map_err(|error| format!("{}: {error}", path.display()))?;
    for (index, entry) in file.failure.iter().enumerate() {
        if !ENGINES.contains(&entry.engine.as_str()) {
            return Err(format!(
                "{}: `{}` names engine `{}`, which is not one of {}",
                path.display(),
                entry.case,
                entry.engine,
                ENGINES.join(", ")
            ));
        }
        if !directory
            .join("cases")
            .join(format!("{}.R", entry.case))
            .is_file()
        {
            return Err(format!(
                "{}: there is no case `{}` in cases/",
                path.display(),
                entry.case
            ));
        }
        if file.failure[..index]
            .iter()
            .any(|earlier| earlier.case == entry.case && earlier.engine == entry.engine)
        {
            return Err(format!(
                "{}: `{}` under `{}` is listed twice",
                path.display(),
                entry.case,
                entry.engine
            ));
        }
    }
    Ok(file.failure)
}

/// The entry in `known` for `case` under the engine named `engine`.
pub fn known_failure<'a>(
    known: &'a [KnownFailure],
    case: &Path,
    engine: &str,
) -> Option<&'a KnownFailure> {
    let name = case.file_stem()?.to_str()?;
    known
        .iter()
        .find(|entry| entry.case == name && entry.engine == engine)
}

/// The stand-in until ry's runtime exists. It reports itself as unavailable for every file.
pub struct StubEngine;

impl Engine for StubEngine {
    fn name(&self) -> String {
        "stub".to_string()
    }

    fn run(&self, _script: &Path) -> Result<Outcome, EngineError> {
        Err(EngineError::Unavailable(
            "the ry runtime does not exist yet".to_string(),
        ))
    }
}

pub struct ProcessEngine {
    name: String,
    program: String,
    arguments: Vec<String>,
    /// Written next to the report and passed before the script, for engines that need a driver.
    driver: Option<Driver>,
    /// GNU R cannot parse inline type annotations, so it runs the plain program an annotated
    /// case means. Comparing that against an engine that reads the annotations is what shows
    /// they never change what correct code does.
    erases_annotations: bool,
    /// Variables set for the engine's process on top of the locale the oracle fixes.
    environment: Vec<(String, String)>,
    /// A run that takes longer is killed and fails, so a hang under fuzzing is a finding
    /// rather than a stuck harness.
    timeout: Option<Duration>,
}

/// How an engine is handed `driver.R`.
#[derive(Clone, Copy)]
enum Driver {
    /// GNU R's command line: `-f driver.R --args <script> <report>`.
    GnuR,
    /// A script runner's: `driver.R <script> <report>`.
    Script,
}

impl ProcessEngine {
    pub fn gnu_r() -> Self {
        ProcessEngine {
            name: "GNU R".to_string(),
            program: gnu_r_program(),
            arguments: ["--vanilla", "--slave", "--no-restore"]
                .map(String::from)
                .to_vec(),
            driver: Some(Driver::GnuR),
            erases_annotations: true,
            environment: Vec::new(),
            timeout: None,
        }
    }

    /// The engine a name stands for: one of [`ENGINES`], `gnu-r`, or a command line.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "gnu-r" => Some(ProcessEngine::gnu_r()),
            "ry" => Some(ProcessEngine::ry()),
            "ry-tier0" => Some(ProcessEngine::ry_tier0()),
            "ry-jit" => Some(ProcessEngine::ry_jit()),
            "ry-gctorture" => Some(ProcessEngine::ry_gc_torture()),
            "ry-gctorture-young" => Some(ProcessEngine::ry_gc_torture_young()),
            "ry-jit-gctorture" => Some(ProcessEngine::ry_jit_gc_torture()),
            command => ProcessEngine::from_command(command),
        }
    }

    /// Kills a run that takes longer than `timeout` and reports it as failed.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// ry's runtime with the JIT off, so every body runs on the tier-0 bytecode interpreter.
    pub fn ry_tier0() -> Self {
        let mut engine = ProcessEngine::ry();
        engine.name = "ry-tier0".to_string();
        engine
            .environment
            .push(("RY_JIT".to_string(), "off".to_string()));
        engine
    }

    /// ry's runtime with collection stress on from the first line of the script
    /// (`RY_GC_STRESS`): every allocation collects first, so an object the runtime forgot to
    /// root is freed at the allocation that exposes it rather than at some later collection.
    /// The JIT stays at its default, so hot code is stressed compiled as well.
    pub fn ry_gc_torture() -> Self {
        let mut engine = ProcessEngine::ry();
        engine.name = "ry-gctorture".to_string();
        engine
            .environment
            .push(("RY_GC_STRESS".to_string(), "1".to_string()));
        engine
    }

    /// [`ProcessEngine::ry_gc_torture`] with survivors kept collectable (`RY_GC_STRESS=young`):
    /// every collection traces again what it has not tenured, so an object that survived earlier
    /// collections and is then held by a Rust local alone is freed, as a minor collection frees
    /// it outside stress while it is still young. Plain stress promotes it first and hides that.
    pub fn ry_gc_torture_young() -> Self {
        let mut engine = ProcessEngine::ry();
        engine.name = "ry-gctorture-young".to_string();
        engine
            .environment
            .push(("RY_GC_STRESS".to_string(), "young".to_string()));
        engine
    }

    /// [`ProcessEngine::ry_gc_torture`] with the JIT forced on, so compiled code runs under
    /// collection stress from its first call rather than only once the default JIT finds it hot.
    pub fn ry_jit_gc_torture() -> Self {
        let mut engine = ProcessEngine::ry_gc_torture();
        engine.name = "ry-jit-gctorture".to_string();
        engine
            .environment
            .push(("RY_JIT".to_string(), "force".to_string()));
        engine
    }

    /// ry's runtime with its JIT forced on, so every body that runs twice and every loop runs
    /// compiled. Comparing it against GNU R checks the compiled tier on every case.
    pub fn ry_jit() -> Self {
        let mut engine = ProcessEngine::ry();
        engine.name = "ry-jit".to_string();
        engine
            .environment
            .push(("RY_JIT".to_string(), "force".to_string()));
        engine
    }

    /// ry's own runtime, `ry run --runtime ry`. The binary is `$ORACLE_RY`, or the workspace's
    /// debug build.
    /// It loads base R from the GNU R engine's installation when [`gnu_r_home`] says so, so
    /// both sides run the same R release.
    pub fn ry() -> Self {
        ProcessEngine {
            name: "ry".to_string(),
            program: ry_program(),
            arguments: ["run", "--runtime", "ry"].map(String::from).to_vec(),
            driver: Some(Driver::Script),
            erases_annotations: false,
            environment: gnu_r_home()
                .map(|home| vec![("R_HOME".to_string(), home)])
                .unwrap_or_default(),
            timeout: None,
        }
    }

    /// An engine given as a whitespace-separated command line, such as `ry run --oracle`.
    pub fn from_command(command: &str) -> Option<Self> {
        let mut parts = command.split_whitespace().map(String::from);
        let program = parts.next()?;
        Some(ProcessEngine {
            name: command.to_string(),
            program,
            arguments: parts.collect(),
            driver: None,
            erases_annotations: false,
            environment: Vec::new(),
            timeout: None,
        })
    }
}

/// The `ry` binary: `$ORACLE_RY`, else the workspace's debug build.
/// A relative `ORACLE_RY` resolves against the current directory, because the engines run
/// each case from the case's own directory.
pub fn ry_program() -> String {
    let program = std::env::var("ORACLE_RY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ry"));
    std::fs::canonicalize(&program)
        .unwrap_or(program)
        .display()
        .to_string()
}

/// Whether ry's runtime can run here: the `ry` binary exists, and so does the `libR.so` it
/// loads (from `RY_LIBR`, or beside the binary as `ry` looks for it).
pub fn ry_runtime_available() -> bool {
    let program = PathBuf::from(ry_program());
    if !program.is_file() {
        return false;
    }
    if let Some(library) = std::env::var_os("RY_LIBR") {
        return Path::new(&library).is_file();
    }
    let directory = program.parent().unwrap_or(Path::new("."));
    [
        directory.join("libR.so"),
        directory.join("deps").join("libR.so"),
        directory.join("../lib/ry/libR.so"),
    ]
    .iter()
    .any(|candidate| candidate.is_file())
}

impl Engine for ProcessEngine {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn run(&self, script: &Path) -> Result<Outcome, EngineError> {
        let scratch = Scratch::new(script).map_err(EngineError::Failed)?;
        let report_path = scratch.path().join("report.json");
        let script = script
            .canonicalize()
            .map_err(|error| EngineError::Failed(format!("{}: {error}", script.display())))?;
        let directory = script.parent().unwrap_or(Path::new(".")).to_path_buf();
        let script = match self.erases_annotations {
            true => erased_copy(&script, scratch.path())?,
            false => script,
        };

        let mut command = Command::new(&self.program);
        command.args(&self.arguments);
        if let Some(driver) = self.driver {
            let driver_path = scratch.path().join("driver.R");
            std::fs::write(&driver_path, DRIVER)
                .map_err(|error| EngineError::Failed(error.to_string()))?;
            match driver {
                Driver::GnuR => {
                    command.arg("-f").arg(&driver_path).arg("--args");
                }
                Driver::Script => {
                    command.arg(&driver_path);
                }
            }
        }
        command.arg(&script).arg(&report_path);
        command
            .current_dir(&directory)
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("TZ", "UTC")
            .env(
                "R_DEFAULT_PACKAGES",
                "datasets,utils,grDevices,graphics,stats,methods",
            )
            .envs(library_environment().iter().cloned())
            .envs(self.environment.iter().map(|(name, value)| (name, value)));

        let output = run_command(command, self.timeout).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                EngineError::Unavailable(format!("`{}` was not found", self.program))
            }
            _ => EngineError::Failed(format!("`{}`: {error}", self.program)),
        })?;
        if output.timed_out {
            return Err(EngineError::Failed(format!(
                "{} was killed after {} seconds",
                self.name,
                self.timeout.map_or(0, |timeout| timeout.as_secs())
            )));
        }
        let report = std::fs::read_to_string(&report_path);
        let report = report.map_err(|_| {
            let message = format!(
                "{} wrote no report ({}): {}",
                self.name,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            match killed_by_signal(output.status) {
                true => EngineError::Crashed(message),
                false => EngineError::Failed(message),
            }
        })?;
        let report: Report = serde_json::from_str(&report).map_err(|error| {
            EngineError::Failed(format!("{} wrote an unreadable report: {error}", self.name))
        })?;
        Ok(Outcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            messages: report.messages,
            warnings: report.warnings,
            errors: report.errors,
            value: report.value,
            libraries: report.libraries,
        })
    }
}

struct CommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

/// Runs `command` to completion, or kills it once `timeout` has passed. The pipes are drained
/// on their own threads, because a child that fills a pipe nobody reads never exits.
///
/// The child starts with SIGINT and SIGQUIT at their defaults. A shell starts a background job
/// (`&`) with both ignored, an ignored disposition survives `exec`, and GNU R keeps an ignored
/// SIGINT instead of installing its handler, so a case that interrupts itself with
/// `tools::pskill` would never see the interrupt under a detached run.
fn run_command(mut command: Command, timeout: Option<Duration>) -> std::io::Result<CommandOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    // SAFETY: the closure runs between fork and exec, and calls only `signal`, which is
    // async-signal-safe.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, || {
            for signal in [libc::SIGINT, libc::SIGQUIT] {
                if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_end(&mut bytes).map(|_| bytes)
            } else {
                Ok(bytes)
            }
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            timed_out = true;
            child.kill()?;
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let joined = |handle: std::thread::JoinHandle<std::io::Result<Vec<u8>>>| {
        handle
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("a pipe reader panicked")))
    };
    Ok(CommandOutput {
        status,
        stdout: joined(stdout)?,
        stderr: joined(stderr)?,
        timed_out,
    })
}

#[cfg(unix)]
fn killed_by_signal(status: std::process::ExitStatus) -> bool {
    std::os::unix::process::ExitStatusExt::signal(&status).is_some()
}

#[cfg(not(unix))]
fn killed_by_signal(_status: std::process::ExitStatus) -> bool {
    false
}

/// The script itself when it has no inline annotation, else its erasure written to `scratch`
/// under the same file name.
fn erased_copy(script: &Path, scratch: &Path) -> Result<PathBuf, EngineError> {
    let text = std::fs::read_to_string(script)
        .map_err(|error| EngineError::Failed(format!("{}: {error}", script.display())))?;
    let parse = syntax::parse(&text);
    let annotated = parse
        .syntax_node()
        .descendants()
        .any(|node| syntax::is_inline_annotation(&node));
    if !annotated {
        return Ok(script.to_path_buf());
    }
    let copy = scratch.join(script.file_name().unwrap_or("case.R".as_ref()));
    std::fs::write(&copy, syntax::erase_inline_annotations(&parse))
        .map_err(|error| EngineError::Failed(format!("{}: {error}", copy.display())))?;
    Ok(copy)
}

/// A scratch directory that is removed when it goes out of scope, on every exit path.
struct Scratch(PathBuf);

impl Scratch {
    fn new(script: &Path) -> Result<Self, String> {
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        let stem = script
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("case");
        let run = RUNS.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("ry-oracle-{}-{run}-{stem}", std::process::id()));
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        Ok(Scratch(directory))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

pub(crate) fn gnu_r_program() -> String {
    std::env::var("ORACLE_R").unwrap_or_else(|_| "R".to_string())
}

/// The R home ry's runtime loads base R from when it is compared with GNU R: the home of
/// `ORACLE_R` when that is set, and otherwise the home of the GNU R on `PATH` unless ry's default
/// base R is known to be the same release. ry emulates the release it loads base R from, so
/// comparing two releases would report every behavior GNU R changed between them as a failure,
/// and a default release that cannot be read (a slow or failing start) must not be taken to
/// match. `None` keeps ry on its bundled base R, which is what a user runs.
pub fn gnu_r_home() -> Option<String> {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    HOME.get_or_init(|| {
        if std::env::var_os("ORACLE_R").is_none() {
            let gnu_r = gnu_r_version()?;
            if ry_default_version().is_some_and(|default| default == gnu_r) {
                return None;
            }
        }
        let output = Command::new(gnu_r_program()).arg("RHOME").output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|home| !home.is_empty())
    })
    .clone()
}

/// The package libraries every engine searches: `R_LIBS` set to the reference GNU R's
/// `.libPaths()`, with `R_LIBS_SITE` and `R_LIBS_USER` set empty so that no engine adds its own
/// defaults. ry's defaults differ from GNU R's in both directions: the library of its bundled base
/// R holds none of the reference R's packages, and it also searches the user and site libraries of
/// any other GNU R it finds, such as Debian's `/usr/lib/R/site-library`, whose older data.table,
/// rlang and withr then load in place of the reference R's. The libraries
/// [`libraries_ahead_of_reference`] picks come first, because the reference R puts `R_LIBS`
/// first. Empty when GNU R cannot be asked, which leaves the inherited environment as it is.
pub fn library_environment() -> &'static [(String, String)] {
    static ENVIRONMENT: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ENVIRONMENT.get_or_init(|| {
        let mut command = Command::new(gnu_r_program());
        command
            .args(["--vanilla", "--slave", "--no-restore", "-e"])
            .arg("cat(.libPaths(), sep = .Platform$path.sep)")
            .env("R_LIBS_SITE", "")
            .env("R_LIBS_USER", "");
        let explicit = std::env::var_os("R_LIBS");
        match libraries_ahead_of_reference(explicit.as_deref(), corpus_library().as_deref()) {
            Some(libraries) => command.env("R_LIBS", libraries),
            None => command.env_remove("R_LIBS"),
        };
        let Ok(output) = command.output() else {
            return Vec::new();
        };
        let paths = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || paths.is_empty() {
            return Vec::new();
        }
        vec![
            ("R_LIBS".to_string(), paths),
            ("R_LIBS_SITE".to_string(), String::new()),
            ("R_LIBS_USER".to_string(), String::new()),
        ]
    })
}

/// The libraries searched ahead of the reference R's own: an explicit `R_LIBS` first, then the
/// corpus library. Many cases need corpus packages (rlang, data.table, xts, knitr, lattice), and a
/// run that does not find them skips those cases, so the corpus library is found without being
/// named. An `R_LIBS` set to the empty string turns the corpus library off.
pub fn libraries_ahead_of_reference(
    explicit: Option<&OsStr>,
    corpus: Option<&Path>,
) -> Option<OsString> {
    let mut libraries: Vec<PathBuf> = match explicit {
        Some(explicit) if explicit.is_empty() => return None,
        Some(explicit) => std::env::split_paths(explicit).collect(),
        None => Vec::new(),
    };
    if let Some(corpus) = corpus
        && !libraries.iter().any(|library| library == corpus)
    {
        libraries.push(corpus.to_path_buf());
    }
    match libraries.is_empty() {
        true => None,
        false => std::env::join_paths(libraries).ok(),
    }
}

/// The corpus library `just corpus` installs into, `$RY_CORPUS_DIR/library` or else
/// `.corpus/library` in the workspace, when that directory exists.
pub fn corpus_library() -> Option<PathBuf> {
    let corpus = std::env::var_os("RY_CORPUS_DIR")
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.corpus"));
    let library = corpus.join("library");
    library.is_dir().then_some(library)
}

/// The release of GNU R, such as `4.3.3`, from the first line `R --version` prints.
fn gnu_r_version() -> Option<String> {
    let output = Command::new(gnu_r_program())
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?;
    line.strip_prefix("R version ")?
        .split_whitespace()
        .next()
        .map(str::to_owned)
}

/// The release of the base R the `ry` binary runs on in this environment (its bundled base R,
/// or the one an inherited `R_HOME` names), as `getRversion()` reports it.
fn ry_default_version() -> Option<String> {
    let scratch = Scratch::new(Path::new("version.R")).ok()?;
    let script = scratch.path().join("version.R");
    std::fs::write(&script, "cat(format(getRversion()))").ok()?;
    let mut command = Command::new(ry_program());
    command.args(["run", "--runtime", "ry"]).arg(&script);
    let output = run_command(command, Some(Duration::from_secs(60))).ok()?;
    if output.timed_out {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !version.is_empty()).then_some(version)
}

/// The GNU R build a suite's outcomes come from. The release decides which behaviors a case
/// shows, so outcomes from two releases never mix. The linear-algebra libraries change the last
/// digits of numerical results, so a different BLAS or LAPACK is worth a warning but not a
/// refusal. The libraries are named by file, not path, so the same build installed in another
/// prefix is the same build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub release: String,
    pub blas: String,
    pub lapack: String,
}

impl std::fmt::Display for Build {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} (BLAS {}, LAPACK {})",
            self.release, self.blas, self.lapack
        )
    }
}

/// The file in a case directory that names the build its `.expected` files were recorded with.
pub const BUILD_FILE: &str = "gnu-r.txt";

/// How the GNU R at hand relates to the build a suite was recorded with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildCheck {
    Same,
    /// Only the linear-algebra libraries differ: compare, but expect last-digit differences.
    LibrariesDiffer(String),
    /// Another release: its outcomes are not this suite's, so comparing them is refused.
    ReleaseDiffers(String),
    /// The suite names no build yet.
    Unrecorded,
}

/// The build of the GNU R that `ORACLE_R` or `PATH` names, asked from R itself.
pub fn gnu_r_build() -> Result<Build, String> {
    static BUILD: OnceLock<Result<Build, String>> = OnceLock::new();
    BUILD
        .get_or_init(|| {
            let program = gnu_r_program();
            let output = Command::new(&program)
                .args(["--vanilla", "--slave", "--no-restore", "-e"])
                .arg(
                    "cat(R.version$major, \".\", R.version$minor, \"\\n\", \
                     basename(extSoftVersion()[[\"BLAS\"]]), \"\\n\", \
                     basename(La_library()), \" \", La_version(), \"\\n\", sep = \"\")",
                )
                .env("LC_ALL", "C.UTF-8")
                .output()
                .map_err(|error| format!("`{program}`: {error}"))?;
            let text = String::from_utf8_lossy(&output.stdout);
            let mut lines = text.lines().map(str::trim);
            match (lines.next(), lines.next(), lines.next()) {
                (Some(release), Some(blas), Some(lapack))
                    if output.status.success() && !release.is_empty() =>
                {
                    Ok(Build {
                        release: release.to_owned(),
                        blas: blas.to_owned(),
                        lapack: lapack.to_owned(),
                    })
                }
                _ => Err(format!(
                    "`{program}` did not report its build ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
            }
        })
        .clone()
}

/// The build file that governs `case`: the nearest one in its directory or an ancestor.
pub fn build_file(case: &Path) -> Option<PathBuf> {
    let start = match case.is_dir() {
        true => case,
        false => case.parent()?,
    };
    start
        .ancestors()
        .map(|directory| directory.join(BUILD_FILE))
        .find(|path| path.is_file())
}

/// The build `file` names, or an error that says what is wrong with it.
pub fn read_build(file: &Path) -> Result<Build, String> {
    let text =
        std::fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let mut release = None;
    let mut blas = None;
    let mut lapack = None;
    for line in text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
    {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let slot = match key {
            "release" => &mut release,
            "blas" => &mut blas,
            "lapack" => &mut lapack,
            _ => return Err(format!("{}: unknown line `{line}`", file.display())),
        };
        *slot = Some(value.trim().to_owned());
    }
    match (release, blas, lapack) {
        (Some(release), Some(blas), Some(lapack)) => Ok(Build {
            release,
            blas,
            lapack,
        }),
        _ => Err(format!(
            "{}: needs `release`, `blas` and `lapack` lines",
            file.display()
        )),
    }
}

pub fn write_build(file: &Path, build: &Build) -> std::io::Result<()> {
    std::fs::write(
        file,
        format!(
            "# The GNU R build the .expected files here were recorded with; `oracle record` writes \
             it.\n# `oracle verify` refuses another release and warns on other BLAS or LAPACK \
             libraries.\nrelease {}\nblas {}\nlapack {}\n",
            build.release, build.blas, build.lapack
        ),
    )
}

/// Compares the GNU R at hand with the build the suite around `case` was recorded with.
pub fn check_build(case: &Path, actual: &Build) -> Result<BuildCheck, String> {
    let Some(file) = build_file(case) else {
        return Ok(BuildCheck::Unrecorded);
    };
    let recorded = read_build(&file)?;
    Ok(if recorded.release != actual.release {
        BuildCheck::ReleaseDiffers(format!(
            "{} were recorded with GNU R {recorded}, but this is GNU R {actual}; set ORACLE_R to \
             an R {} build",
            file.parent().unwrap_or(Path::new(".")).display(),
            recorded.release
        ))
    } else if recorded != *actual {
        BuildCheck::LibrariesDiffer(format!(
            "{} were recorded with GNU R {recorded}, but this is GNU R {actual}; the last digits \
             of numerical results may differ",
            file.parent().unwrap_or(Path::new(".")).display()
        ))
    } else {
        BuildCheck::Same
    })
}

pub fn gnu_r_available() -> bool {
    Command::new(gnu_r_program())
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// The engine named by `ORACLE_ENGINE`, or the stub.
pub fn engine_from_environment() -> Box<dyn Engine> {
    match std::env::var("ORACLE_ENGINE")
        .ok()
        .as_deref()
        .and_then(ProcessEngine::named)
    {
        Some(engine) => Box::new(engine),
        None => Box::new(StubEngine),
    }
}

/// Every `.R` file under `root`, sorted. A file argument yields itself.
pub fn find_cases(root: &Path) -> Vec<PathBuf> {
    let mut cases = Vec::new();
    collect_cases(root, &mut cases);
    cases.sort();
    cases
}

fn collect_cases(path: &Path, cases: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                collect_cases(&entry.path(), cases);
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "R") {
        cases.push(path.to_path_buf());
    }
}

/// Splits `cases` into those that can run here and those that cannot, each with what it needs
/// and this machine lacks. A case names its packages in a leading `# oracle: requires knitr,
/// rmarkdown` comment, because without them it fails in an environment-specific way (a missing
/// vignette engine reports a temporary path) rather than in a way two engines can agree on. A
/// locale it switches to is a requirement too (`# oracle: requires locale en_US.ISO-8859-1`), so
/// a machine that never generated it refuses the run by naming the locale instead of reporting a
/// diff. When GNU R cannot list its packages, package requirements count as met.
pub fn runnable_cases(
    cases: Vec<PathBuf>,
    installed: impl FnOnce() -> Option<Vec<String>>,
) -> (Vec<PathBuf>, Vec<(PathBuf, Vec<String>)>) {
    let required: Vec<Vec<String>> = cases.iter().map(|case| case_requirements(case)).collect();
    let needs_packages = required
        .iter()
        .flatten()
        .any(|requirement| !requirement.starts_with(LOCALE_REQUIREMENT));
    let installed = match needs_packages {
        true => installed(),
        false => None,
    };
    let mut runnable = Vec::new();
    let mut missing = Vec::new();
    for (case, requirements) in cases.into_iter().zip(required) {
        let absent: Vec<String> = requirements
            .into_iter()
            .filter(
                |requirement| match requirement.strip_prefix(LOCALE_REQUIREMENT) {
                    Some(locale) => !locale_installed(locale),
                    None => installed
                        .as_ref()
                        .is_some_and(|installed| !installed.iter().any(|name| name == requirement)),
                },
            )
            .collect();
        match absent.is_empty() {
            true => runnable.push(case),
            false => missing.push((case, absent)),
        }
    }
    (runnable, missing)
}

const LOCALE_REQUIREMENT: &str = "locale ";

/// Whether `case` declares, in a leading `# oracle: library-dependent` comment, that its recorded
/// last digits are those of the libraries GNU R ran on: BLAS and LAPACK (`lm`, `optim`, matrix
/// products) or the C math library. Another build of those libraries moves the digits, and an
/// optimizer's iteration count or warnings with them, so only a GNU R on the recorded libraries
/// can confirm such a case's expectation.
pub fn library_dependent(case: &Path) -> bool {
    std::fs::read_to_string(case).is_ok_and(|text| {
        text.lines()
            .take_while(|line| line.starts_with('#'))
            .any(|line| line.trim_end() == LIBRARY_DEPENDENT)
    })
}

const LIBRARY_DEPENDENT: &str = "# oracle: library-dependent";

fn case_requirements(case: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(case) else {
        return Vec::new();
    };
    text.lines()
        .take_while(|line| line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("# oracle: requires "))
        .flat_map(|list| list.split(','))
        .map(|requirement| requirement.trim().to_string())
        .filter(|requirement| !requirement.is_empty())
        .collect()
}

/// Whether the C library can load `locale`. `newlocale` builds a locale object without touching
/// this process's own locale, which a global `setlocale` would change under other threads.
#[cfg(unix)]
fn locale_installed(locale: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(locale) else {
        return false;
    };
    // SAFETY: `name` is a valid C string for the call, and a non-null result is freed once.
    unsafe {
        let handle = libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut());
        if handle.is_null() {
            return false;
        }
        libc::freelocale(handle);
        true
    }
}

#[cfg(not(unix))]
fn locale_installed(_locale: &str) -> bool {
    false
}

/// The packages GNU R can load from the libraries [`library_environment`] gives every engine.
pub fn installed_packages() -> Option<Vec<String>> {
    let output = Command::new(gnu_r_program())
        .args([
            "--vanilla",
            "-s",
            "-e",
            "cat(rownames(installed.packages()), sep = '\\n')",
        ])
        .envs(library_environment().iter().cloned())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    })
}

/// The packages `engine` can load, asked of the engine itself by running a probe script. A
/// comparison against the recorded outcomes uses it, since no GNU R is there to ask.
pub fn engine_packages(engine: &dyn Engine) -> Option<Vec<String>> {
    let scratch = Scratch::new(Path::new("packages")).ok()?;
    let probe = scratch.path().join("packages.R");
    std::fs::write(&probe, "cat(rownames(installed.packages()), sep = '\\n')\n").ok()?;
    let outcome = engine.run(&probe).ok()?;
    Some(outcome.stdout.lines().map(str::to_string).collect())
}

pub fn expected_path(case: &Path) -> PathBuf {
    case.with_extension("expected")
}

/// The line-oriented text form of an outcome, which is what expectation files hold.
/// Every section header starts in column zero, and every content line is prefixed, so no
/// script output can be mistaken for structure.
pub fn render(outcome: &Outcome) -> String {
    let mut text = String::from("[stdout]\n");
    for line in outcome.stdout.split_inclusive('\n') {
        text.push_str(&format!("| {}\n", line.strip_suffix('\n').unwrap_or(line)));
    }
    if !outcome.stdout.is_empty() && !outcome.stdout.ends_with('\n') {
        text.push_str("\\ no newline at end of output\n");
    }
    for (header, entries) in [
        ("messages", &outcome.messages),
        ("warnings", &outcome.warnings),
        ("errors", &outcome.errors),
    ] {
        text.push_str(&format!("[{header}]\n"));
        for entry in entries {
            text.push_str(&format!("- {}\n", escape(entry)));
        }
    }
    text.push_str("[value]\n");
    if let Some(value) = &outcome.value {
        text.push_str(&format!("{}\n", value.kind));
        for line in value.deparsed.split('\n') {
            text.push_str(&format!("| {line}\n"));
        }
    }
    text
}

pub fn parse(text: &str) -> Option<Outcome> {
    let mut outcome = Outcome::default();
    let mut section = "";
    let mut value_lines: Vec<&str> = Vec::new();
    let mut kind = None;
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = match header {
                "stdout" => "stdout",
                "messages" => "messages",
                "warnings" => "warnings",
                "errors" => "errors",
                "value" => "value",
                _ => return None,
            };
        } else if line == "\\ no newline at end of output" {
            if outcome.stdout.ends_with('\n') {
                outcome.stdout.pop();
            }
        } else if section == "stdout" {
            outcome
                .stdout
                .push_str(line.strip_prefix("| ").unwrap_or(line));
            outcome.stdout.push('\n');
        } else if section == "value" {
            match line.strip_prefix("| ") {
                Some(content) => value_lines.push(content),
                None => kind = Some(line.to_string()),
            }
        } else {
            let entry = unescape(line.strip_prefix("- ")?);
            match section {
                "messages" => outcome.messages.push(entry),
                "warnings" => outcome.warnings.push(entry),
                "errors" => outcome.errors.push(entry),
                _ => return None,
            }
        }
    }
    outcome.value = kind.map(|kind| Value {
        kind,
        deparsed: value_lines.join("\n"),
    });
    Some(outcome)
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unescape(text: &str) -> String {
    let mut result = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some(other) => result.push(other),
                None => result.push('\\'),
            }
        } else {
            result.push(character);
        }
    }
    result
}

/// A unified diff of two outcomes, or `None` when they are equal.
pub fn diff(
    expected_label: &str,
    expected: &Outcome,
    actual_label: &str,
    actual: &Outcome,
) -> Option<String> {
    if let (Some(left), Some(right)) = (&expected.libraries, &actual.libraries)
        && left != right
    {
        return Some(format!(
            "the engines searched different package libraries, so their runs are not comparable: \
             {expected_label} had R_LIBS={left:?} and {actual_label} had R_LIBS={right:?}; give \
             both the same library, installed by GNU R"
        ));
    }
    if expected == actual {
        return None;
    }
    let diff = TextDiff::from_lines(&render(expected), &render(actual))
        .unified_diff()
        .header(expected_label, actual_label)
        .to_string();
    Some(diff)
}
