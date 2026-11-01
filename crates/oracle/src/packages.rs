//! Runs each corpus package's own test suite under GNU R and under ry's runtime, the way
//! `R CMD check` runs `tests/`: every top-level `tests/*.R` file runs as its own script with
//! `tests/` as the working directory, and a file passes when its process exits with status 0.
//! A package passes when every file passes. testthat's summary line
//! (`[ FAIL 0 | WARN 1 | SKIP 2 | PASS 30 ]`) is read from the output, so a report can compare
//! expectation counts and not only exit codes.
//!
//! Layout under the corpus directory: `sources/<pkg>_<version>.tar.gz` (from
//! `tools/corpus/sources.R`), `closure.txt`, and the output `tests/<pkg>/<engine>/` (the copied
//! `tests/` directory, one `.Rout` per file and `result.json`) plus `tests/summary.md`.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The runtime a package's tests run under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Runtime {
    GnuR,
    Ry,
}

impl Runtime {
    pub fn directory(self) -> &'static str {
        match self {
            Runtime::GnuR => "gnur",
            Runtime::Ry => "ry",
        }
    }
}

/// Where things are and how to run them.
pub struct Settings {
    pub corpus: PathBuf,
    pub rscript: String,
    pub ry: String,
    pub jobs: usize,
    /// The wall-clock limit for one package's whole suite under one runtime.
    pub timeout: Duration,
    /// Reuse a recorded GNU R result for the same package version instead of rerunning it.
    pub reuse_gnu_r: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Timeout,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct TestthatCounts {
    pub fail: u32,
    pub warn: u32,
    pub skip: u32,
    pub pass: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileResult {
    pub file: String,
    pub status: Status,
    pub seconds: f64,
    pub testthat: Option<TestthatCounts>,
    /// The first error the output reports, for triage.
    pub first_error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SuiteResult {
    pub version: String,
    pub status: Status,
    pub files: Vec<FileResult>,
}

/// What happened to one package.
#[derive(Clone, Debug)]
pub enum PackageOutcome {
    NoSources,
    NoTests,
    SetupFailed(String),
    Ran {
        gnu_r: Option<SuiteResult>,
        ry: Option<SuiteResult>,
    },
}

/// The packages to test: `closure.txt`, written by `tools/corpus/sources.R`.
pub fn closure(corpus: &Path) -> io::Result<Vec<String>> {
    Ok(fs::read_to_string(corpus.join("closure.txt"))?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}

/// Runs every package's tests under `runtimes`, `settings.jobs` packages at a time, calling
/// `report` as each package finishes.
pub fn run_all(
    settings: &Settings,
    packages: &[String],
    runtimes: &[Runtime],
    report: &(dyn Fn(&str, &PackageOutcome) + Sync),
) -> Vec<(String, PackageOutcome)> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..settings.jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(package) = packages.get(index) else {
                        break;
                    };
                    let outcome = run_package(settings, package, runtimes);
                    report(package, &outcome);
                    results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push((package.clone(), outcome));
                }
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    results.sort_by(|left, right| left.0.cmp(&right.0));
    results
}

/// Runs one package's tests under each runtime in `runtimes`. A runtime not asked for reports
/// its recorded result, if any.
pub fn run_package(settings: &Settings, package: &str, runtimes: &[Runtime]) -> PackageOutcome {
    let Some((tarball, version)) = find_tarball(&settings.corpus, package) else {
        return PackageOutcome::NoSources;
    };
    let root = settings.corpus.join("tests").join(package);
    let source = root.join("source");
    if let Err(error) = extract(&tarball, &source, package) {
        return PackageOutcome::SetupFailed(error);
    }
    let tests = source.join(package).join("tests");
    if !tests.is_dir() {
        return PackageOutcome::NoTests;
    }
    let mut gnu_r = None;
    let mut ry = None;
    for runtime in [Runtime::GnuR, Runtime::Ry] {
        let work = root.join(runtime.directory());
        let recorded = read_result(&work).filter(|result| result.version == version);
        let rerun = runtimes.contains(&runtime)
            && !(runtime == Runtime::GnuR && settings.reuse_gnu_r && recorded.is_some());
        let result = if rerun {
            match run_suite(settings, runtime, &tests, &work, &version, package) {
                Ok(result) => Some(result),
                Err(error) => return PackageOutcome::SetupFailed(error),
            }
        } else {
            recorded
        };
        match runtime {
            Runtime::GnuR => gnu_r = result,
            Runtime::Ry => ry = result,
        }
    }
    PackageOutcome::Ran { gnu_r, ry }
}

fn find_tarball(corpus: &Path, package: &str) -> Option<(PathBuf, String)> {
    let prefix = format!("{package}_");
    let entries = fs::read_dir(corpus.join("sources")).ok()?;
    entries.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        let version = name
            .strip_prefix(&prefix)?
            .strip_suffix(".tar.gz")?
            .to_owned();
        Some((entry.path(), version))
    })
}

fn extract(tarball: &Path, source: &Path, package: &str) -> Result<(), String> {
    if source.join(package).join("DESCRIPTION").exists() {
        return Ok(());
    }
    if source.exists() {
        fs::remove_dir_all(source)
            .map_err(|error| format!("cannot clear {}: {error}", source.display()))?;
    }
    fs::create_dir_all(source)
        .map_err(|error| format!("cannot create {}: {error}", source.display()))?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(source)
        .status()
        .map_err(|error| format!("cannot run tar: {error}"))?;
    if !status.success() {
        return Err(format!("tar failed on {}", tarball.display()));
    }
    Ok(())
}

fn read_result(work: &Path) -> Option<SuiteResult> {
    let text = fs::read_to_string(work.join("result.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn run_suite(
    settings: &Settings,
    runtime: Runtime,
    tests: &Path,
    work: &Path,
    version: &str,
    package: &str,
) -> Result<SuiteResult, String> {
    if work.exists() {
        fs::remove_dir_all(work)
            .map_err(|error| format!("cannot clear {}: {error}", work.display()))?;
    }
    let status = Command::new("cp")
        .arg("-r")
        .arg(tests)
        .arg(work)
        .status()
        .map_err(|error| format!("cannot run cp: {error}"))?;
    if !status.success() {
        return Err(format!("cannot copy {}", tests.display()));
    }
    let mut scripts: Vec<String> = fs::read_dir(work)
        .map_err(|error| format!("cannot list {}: {error}", work.display()))?
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".R") || name.ends_with(".r"))
        .collect();
    scripts.sort();
    let library = settings.corpus.join("library");
    let library = library.canonicalize().unwrap_or(library);
    let deadline = Instant::now() + settings.timeout;
    let mut files = Vec::new();
    for script in scripts {
        let output_path = work.join(format!("{script}out"));
        let output = File::create(&output_path)
            .map_err(|error| format!("cannot create {}: {error}", output_path.display()))?;
        let errors = output
            .try_clone()
            .map_err(|error| format!("cannot duplicate {}: {error}", output_path.display()))?;
        let mut command = match runtime {
            Runtime::GnuR => {
                let mut command = Command::new(&settings.rscript);
                command.arg("--vanilla").arg(&script);
                command
            }
            Runtime::Ry => {
                let mut command = Command::new(&settings.ry);
                command.args(["run", "--runtime", "ry"]).arg(&script);
                command
            }
        };
        command
            .current_dir(work)
            .env("R_LIBS", &library)
            .env("R_LIBS_USER", &library)
            .env("R_LIBS_SITE", "")
            .env("LANGUAGE", "en")
            .env("LC_ALL", "C.UTF-8")
            .env("R_BROWSER", "false")
            .env("R_PDFVIEWER", "false")
            .env("_R_CHECK_PACKAGE_NAME_", package)
            .env("TZ", "UTC")
            .env_remove("NOT_CRAN")
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(errors);
        let started = Instant::now();
        let status = run_with_deadline(command, deadline)
            .map_err(|error| format!("cannot run {script}: {error}"))?;
        let seconds = started.elapsed().as_secs_f64();
        let text = fs::read(&output_path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        files.push(FileResult {
            file: script,
            status,
            seconds,
            testthat: testthat_counts(&text),
            first_error: first_error(&text),
        });
        if status == Status::Timeout {
            break;
        }
    }
    let status = if files.iter().any(|file| file.status == Status::Timeout) {
        Status::Timeout
    } else if files.iter().all(|file| file.status == Status::Pass) {
        Status::Pass
    } else {
        Status::Fail
    };
    let result = SuiteResult {
        version: version.to_owned(),
        status,
        files,
    };
    let json = serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?;
    fs::write(work.join("result.json"), json)
        .map_err(|error| format!("cannot write result.json: {error}"))?;
    Ok(result)
}

/// Runs `command` in its own process group and kills the group at `deadline`, or when the
/// script exits, because test suites start workers (callr, parallel) that would outlive it.
fn run_with_deadline(mut command: Command, deadline: Instant) -> io::Result<Status> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let mut child = command.spawn()?;
    let group = format!("-{}", child.id());
    let status = loop {
        if let Some(exit) = child.try_wait()? {
            break if exit.success() {
                Status::Pass
            } else {
                Status::Fail
            };
        }
        if Instant::now() >= deadline {
            break Status::Timeout;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Killing a group whose members all exited fails with "no such process", which is the
    // common case and means there is nothing left to stop.
    match Command::new("kill")
        .args(["-KILL", "--", &group])
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) | Err(_) => {}
    }
    if status == Status::Timeout {
        child.wait()?;
    }
    Ok(status)
}

/// The last testthat summary line, `[ FAIL 0 | WARN 1 | SKIP 2 | PASS 30 ]`.
pub fn testthat_counts(text: &str) -> Option<TestthatCounts> {
    let line = text
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with("[ FAIL "))?;
    let mut counts = TestthatCounts::default();
    for part in line
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split('|')
    {
        let mut words = part.split_whitespace();
        let (Some(label), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(value) = value.parse() else {
            continue;
        };
        match label {
            "FAIL" => counts.fail = value,
            "WARN" => counts.warn = value,
            "SKIP" => counts.skip = value,
            "PASS" => counts.pass = value,
            _ => {}
        }
    }
    Some(counts)
}

/// The first line that reports an error, with the line after it when the message continues.
fn first_error(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let index = lines.iter().position(|line| {
        line.starts_with("Error") || line.contains("── Error") || line.contains("── Failure")
    })?;
    let mut message = lines[index].trim().to_owned();
    if let Some(next) = lines.get(index + 1)
        && (message.ends_with(':') || message.len() < 40 || message.contains("── "))
    {
        message.push(' ');
        message.push_str(next.trim());
    }
    message.truncate(message.floor_char_boundary(240));
    Some(message)
}

/// The Markdown report: totals first, then one row per package.
pub fn summary(results: &[(String, PackageOutcome)]) -> String {
    let mut with_tests = 0;
    let mut gnu_r_pass = 0;
    let mut ry_pass = 0;
    let mut ry_pass_where_gnu_r_passes = 0;
    let mut rows = String::new();
    for (package, outcome) in results {
        let (gnu_r, ry, note) = match outcome {
            PackageOutcome::NoSources => ("-".to_owned(), "-".to_owned(), "no sources".to_owned()),
            PackageOutcome::NoTests => ("-".to_owned(), "-".to_owned(), "no tests".to_owned()),
            PackageOutcome::SetupFailed(error) => ("-".to_owned(), "-".to_owned(), error.clone()),
            PackageOutcome::Ran { gnu_r, ry } => {
                with_tests += 1;
                let gnu_r_passed = gnu_r
                    .as_ref()
                    .is_some_and(|result| result.status == Status::Pass);
                let ry_passed = ry
                    .as_ref()
                    .is_some_and(|result| result.status == Status::Pass);
                gnu_r_pass += usize::from(gnu_r_passed);
                ry_pass += usize::from(ry_passed);
                ry_pass_where_gnu_r_passes += usize::from(gnu_r_passed && ry_passed);
                let note = ry
                    .as_ref()
                    .and_then(|result| {
                        result
                            .files
                            .iter()
                            .find(|file| file.status != Status::Pass)
                            .map(|file| {
                                format!(
                                    "{}: {}",
                                    file.file,
                                    file.first_error.clone().unwrap_or_default()
                                )
                            })
                    })
                    .unwrap_or_default();
                (cell(gnu_r.as_ref()), cell(ry.as_ref()), note)
            }
        };
        let note = note.replace('|', "\\|").replace('`', "'");
        rows.push_str(&format!("| {package} | {gnu_r} | {ry} | {note} |\n"));
    }
    format!(
        "{} packages, {with_tests} with tests. GNU R passes {gnu_r_pass}; ry passes {ry_pass}, \
         {ry_pass_where_gnu_r_passes} of them where GNU R passes ({:.1}%).\n\n\
         | Package | GNU R | ry | First ry failure |\n| --- | --- | --- | --- |\n{rows}",
        results.len(),
        percent(ry_pass_where_gnu_r_passes, gnu_r_pass),
    )
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn cell(result: Option<&SuiteResult>) -> String {
    let Some(result) = result else {
        return "not run".to_owned();
    };
    let status = match result.status {
        Status::Pass => "pass",
        Status::Fail => "FAIL",
        Status::Timeout => "TIMEOUT",
    };
    let counts = result
        .files
        .iter()
        .filter_map(|file| file.testthat.as_ref())
        .fold(None::<TestthatCounts>, |total, counts| {
            let mut total = total.unwrap_or_default();
            total.fail += counts.fail;
            total.pass += counts.pass;
            total.skip += counts.skip;
            total.warn += counts.warn;
            Some(total)
        });
    match counts {
        Some(counts) => format!("{status} ({} ok, {} failed)", counts.pass, counts.fail),
        None => status.to_owned(),
    }
}
