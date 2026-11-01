//! A differential oracle for R: run one file under GNU R and under a second engine, and
//! compare stdout, messages, warnings, errors and the last value.
//!
//! An engine is anything that implements [`Engine`]. [`ProcessEngine`] covers every engine
//! with a command line: it is invoked as `<command…> <script> <report>`, must print the
//! script's own output to stdout, and must write the JSON report that `driver.R` defines.
//! GNU R is one such engine, and ry's runtime will be another.

pub mod bench;
pub mod fuzz;
pub mod install;

use {
    serde::Deserialize,
    similar::TextDiff,
    std::{
        io::Read,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::atomic::{AtomicUsize, Ordering},
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
}

/// Everything observable about one run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    pub stdout: String,
    pub messages: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub value: Option<Value>,
}

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

    /// The engine a name stands for: `gnu-r`, `ry`, `ry-tier0`, `ry-jit`, `ry-gctorture`, or a
    /// command line.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "gnu-r" => Some(ProcessEngine::gnu_r()),
            "ry" => Some(ProcessEngine::ry()),
            "ry-tier0" => Some(ProcessEngine::ry_tier0()),
            "ry-jit" => Some(ProcessEngine::ry_jit()),
            "ry-gctorture" => Some(ProcessEngine::ry_gc_torture()),
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
    /// It loads base R from the same installation as the GNU R engine ([`gnu_r_home`]), so one
    /// `ORACLE_R` selects the R version both sides run.
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
            EngineError::Failed(format!(
                "{} wrote no report ({}): {}",
                self.name,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
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
fn run_command(mut command: Command, timeout: Option<Duration>) -> std::io::Result<CommandOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

fn gnu_r_program() -> String {
    std::env::var("ORACLE_R").unwrap_or_else(|_| "R".to_string())
}

/// The R home of `ORACLE_R`, as its `R RHOME` prints it, when `ORACLE_R` is set. Without it,
/// ry finds base R the way it does for a user (`R_HOME`, then the usual places).
pub fn gnu_r_home() -> Option<String> {
    std::env::var_os("ORACLE_R")?;
    let output = Command::new(gnu_r_program()).arg("RHOME").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|home| !home.is_empty())
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
    if expected == actual {
        return None;
    }
    let diff = TextDiff::from_lines(&render(expected), &render(actual))
        .unified_diff()
        .header(expected_label, actual_label)
        .to_string();
    Some(diff)
}
