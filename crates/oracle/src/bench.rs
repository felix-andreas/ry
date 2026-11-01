//! Runs a package's testthat suite under GNU R and under ry's runtime and compares the two test
//! by test.
//!
//! Both runs go through `ry load <package> --file bench.R`, once with `--runtime gnu-r` and once
//! with `--runtime ry`, so the package is loaded by the same loader plan on both sides and only
//! evaluation differs. `bench.R` writes one JSON line per `test_that()` block: its status, the
//! problems it reported (failed expectations, errors, warnings, skips), a deparse of its value
//! and its run time. A test matches when all of those but the time agree. GNU R runs twice, and
//! a test whose value or problems differ between those runs is compared by status alone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const BENCH_DRIVER: &str = include_str!("bench.R");

/// One `test_that()` block, or a pseudo-test: `<file>` for an error outside any block,
/// `<setup>` for a driver that could not start, and `<total>` for the end of the run.
#[derive(Debug, Clone, Deserialize)]
pub struct TestResult {
    pub file: String,
    pub test: String,
    pub status: String,
    pub expectations: u32,
    pub problems: Vec<String>,
    pub value: String,
    pub seconds: f64,
}

/// One runtime's run of a whole suite.
#[derive(Debug, Clone)]
pub struct Run {
    pub results: Vec<TestResult>,
    /// Process wall time, including starting the runtime and loading the package.
    pub wall: Duration,
    /// The suite's own time as the driver measured it, or `None` when the run never finished.
    pub suite_seconds: Option<f64>,
    pub exit: String,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Same status, problems and value under both runtimes.
    Match,
    /// Both ran it, and something differs.
    Differs,
    /// GNU R ran it and ry never reported it, because ry's run ended early.
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub file: String,
    pub test: String,
    pub verdict: Verdict,
    pub gnu_r_status: String,
    pub ry_status: String,
    /// The first difference, in a few words, for the report.
    pub detail: String,
    pub gnu_r_seconds: f64,
    pub ry_seconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub package: String,
    pub tests: usize,
    pub matching: usize,
    pub differing: usize,
    pub missing: usize,
    pub gnu_r_passing: usize,
    pub ry_passing: usize,
    pub gnu_r_wall_seconds: f64,
    pub ry_wall_seconds: f64,
    pub gnu_r_suite_seconds: Option<f64>,
    pub ry_suite_seconds: Option<f64>,
    pub ry_exit: String,
    /// False for a GNU R-only run, whose report lists GNU R's own failures instead.
    pub ry_ran: bool,
    pub comparisons: Vec<Comparison>,
}

impl Summary {
    pub fn pass_rate(&self) -> f64 {
        if self.tests == 0 {
            0.0
        } else {
            self.matching as f64 / self.tests as f64
        }
    }
}

pub struct Options {
    /// The `ry` binary that loads the package under both runtimes.
    pub ry: PathBuf,
    /// A regular expression a test file's name must match (empty for all).
    pub filter: String,
    pub timeout: Duration,
    /// Run only GNU R, to record a baseline or time it alone.
    pub skip_ry: bool,
    /// Leave the package copy, the driver and each runtime's report and stderr in place.
    pub keep: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("{0}")]
    Setup(String),
}

/// Compares one package's suite under both runtimes.
pub fn bench_package(package_dir: &Path, options: &Options) -> Result<Summary, BenchError> {
    let package_dir = package_dir
        .canonicalize()
        .map_err(|error| BenchError::Setup(format!("{}: {error}", package_dir.display())))?;
    let name = package_name(&package_dir)?;

    let scratch = std::env::temp_dir().join(format!("ry-bench-{}-{name}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).map_err(setup_error(&scratch))?;
    }
    std::fs::create_dir_all(&scratch).map_err(setup_error(&scratch))?;
    let copy = scratch.join(&name);
    copy_tree(&package_dir, &copy)?;
    drop_missing_imports(&copy)?;
    let driver = scratch.join("bench.R");
    std::fs::write(&driver, BENCH_DRIVER).map_err(setup_error(&driver))?;

    let gnu_r = run_suite(&copy, &name, &driver, "gnu-r", &scratch, options)?;
    // A second GNU R run finds the values GNU R does not reproduce itself (a fresh UUID, a
    // temporary path, a timestamp), which are then compared by status only.
    let gnu_r_again = run_suite(&copy, &name, &driver, "gnu-r-again", &scratch, options)?;
    let ry = match options.skip_ry {
        true => None,
        false => Some(run_suite(&copy, &name, &driver, "ry", &scratch, options)?),
    };
    if options.keep {
        eprintln!("kept {}", scratch.display());
    } else if let Err(error) = std::fs::remove_dir_all(&scratch) {
        eprintln!("could not remove {}: {error}", scratch.display());
    }
    Ok(compare_runs(&name, &gnu_r, &gnu_r_again, ry.as_ref()))
}

/// Matches tests by file, name and occurrence, because a generated suite may repeat a name.
pub fn compare_runs(package: &str, gnu_r: &Run, gnu_r_again: &Run, ry: Option<&Run>) -> Summary {
    let tests = keyed(gnu_r);
    let again: HashMap<Key, &TestResult> = keyed(gnu_r_again).into_iter().collect();
    let ry_by_key: HashMap<Key, &TestResult> =
        ry.map(keyed).unwrap_or_default().into_iter().collect();
    let mut comparisons = Vec::with_capacity(tests.len());
    for (key, expected) in &tests {
        let actual = ry_by_key.get(key);
        let stable = again.get(key).is_some_and(|other| {
            other.value == expected.value && other.problems == expected.problems
        });
        let (verdict, detail) = match actual {
            None => (Verdict::Missing, String::new()),
            Some(actual) => match difference(expected, actual, stable) {
                None => (Verdict::Match, String::new()),
                Some(detail) => (Verdict::Differs, detail),
            },
        };
        comparisons.push(Comparison {
            file: expected.file.clone(),
            test: expected.test.clone(),
            verdict,
            gnu_r_status: expected.status.clone(),
            ry_status: actual.map_or_else(|| "missing".to_string(), |actual| actual.status.clone()),
            detail,
            gnu_r_seconds: expected.seconds,
            ry_seconds: actual.map_or(0.0, |actual| actual.seconds),
        });
    }
    // A test only ry reported (a file GNU R could not run at all) counts against ry too.
    let gnu_r_keys: HashSet<(String, String)> = tests
        .iter()
        .map(|(_, test)| (test.file.clone(), test.test.clone()))
        .collect();
    if let Some(ry) = ry {
        for extra in ry.results.iter().filter(|result| is_test(result)) {
            if !gnu_r_keys.contains(&(extra.file.clone(), extra.test.clone())) {
                comparisons.push(Comparison {
                    file: extra.file.clone(),
                    test: extra.test.clone(),
                    verdict: Verdict::Differs,
                    gnu_r_status: "absent".to_string(),
                    ry_status: extra.status.clone(),
                    detail: format!("only ry reported it: {}", first_problem(extra)),
                    gnu_r_seconds: 0.0,
                    ry_seconds: extra.seconds,
                });
            }
        }
    }
    let count = |verdict| {
        comparisons
            .iter()
            .filter(|comparison| comparison.verdict == verdict)
            .count()
    };
    let passing = |run: &Run| {
        run.results
            .iter()
            .filter(|result| is_test(result) && result.status == "pass")
            .count()
    };
    Summary {
        package: package.to_string(),
        tests: comparisons.len(),
        matching: count(Verdict::Match),
        differing: count(Verdict::Differs),
        missing: count(Verdict::Missing),
        gnu_r_passing: passing(gnu_r),
        ry_passing: ry.map_or(0, passing),
        gnu_r_wall_seconds: gnu_r.wall.as_secs_f64(),
        ry_wall_seconds: ry.map_or(0.0, |ry| ry.wall.as_secs_f64()),
        gnu_r_suite_seconds: gnu_r.suite_seconds,
        ry_suite_seconds: ry.and_then(|ry| ry.suite_seconds),
        ry_exit: ry.map_or_else(String::new, |ry| match ry.suite_seconds {
            Some(_) => ry.exit.clone(),
            None => format!("{} before the suite finished: {}", ry.exit, ry.stderr_tail),
        }),
        ry_ran: ry.is_some(),
        comparisons,
    }
}

/// The report: every non-matching test grouped by file, then the totals.
pub fn render_summary(summary: &Summary, verbose: bool) -> String {
    let mut text = String::new();
    let mut by_file: BTreeMap<&str, Vec<&Comparison>> = BTreeMap::new();
    for comparison in &summary.comparisons {
        by_file
            .entry(&comparison.file)
            .or_default()
            .push(comparison);
    }
    for (file, comparisons) in &by_file {
        let matching = comparisons
            .iter()
            .filter(|comparison| match summary.ry_ran {
                true => comparison.verdict == Verdict::Match,
                false => comparison.gnu_r_status == "pass",
            })
            .count();
        text.push_str(&format!(
            "{} {file}: {matching}/{} {}\n",
            summary.package,
            comparisons.len(),
            if summary.ry_ran {
                "match"
            } else {
                "pass under GNU R"
            }
        ));
        for comparison in comparisons {
            if !summary.ry_ran {
                if comparison.gnu_r_status != "pass" || verbose {
                    text.push_str(&format!(
                        "  {:<8} {}\n",
                        comparison.gnu_r_status, comparison.test
                    ));
                }
                continue;
            }
            if comparison.verdict == Verdict::Match && !verbose {
                continue;
            }
            text.push_str(&format!(
                "  {:<8} {} (GNU R {}, ry {}){}\n",
                match comparison.verdict {
                    Verdict::Match => "ok",
                    Verdict::Differs => "DIFFERS",
                    Verdict::Missing => "MISSING",
                },
                comparison.test,
                comparison.gnu_r_status,
                comparison.ry_status,
                match comparison.detail.is_empty() {
                    true => String::new(),
                    false => format!("\n           {}", comparison.detail),
                }
            ));
        }
    }
    if !summary.ry_exit.is_empty() && summary.ry_suite_seconds.is_none() {
        text.push_str(&format!("{}: ry {}\n", summary.package, summary.ry_exit));
    }
    text
}

/// A test's identity: its file, its name, and which occurrence of that name in the file it is,
/// because a generated suite may repeat a name.
type Key = (String, String, usize);

fn keyed(run: &Run) -> Vec<(Key, &TestResult)> {
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    let mut tests = Vec::new();
    for result in run.results.iter().filter(|result| is_test(result)) {
        let count = seen
            .entry((result.file.clone(), result.test.clone()))
            .or_default();
        tests.push(((result.file.clone(), result.test.clone(), *count), result));
        *count += 1;
    }
    tests
}

fn is_test(result: &TestResult) -> bool {
    !matches!(result.test.as_str(), "<total>")
}

/// The first way `actual` differs from `expected`. An unstable test (one GNU R does not
/// reproduce itself) is compared by status alone.
fn difference(expected: &TestResult, actual: &TestResult, stable: bool) -> Option<String> {
    if expected.status != actual.status {
        return Some(format!(
            "GNU R: {} | ry: {}",
            first_problem(expected),
            first_problem(actual)
        ));
    }
    if !stable {
        return None;
    }
    if expected.problems != actual.problems {
        let index = expected
            .problems
            .iter()
            .zip(&actual.problems)
            .position(|(left, right)| left != right)
            .unwrap_or(expected.problems.len().min(actual.problems.len()));
        let at = |problems: &[String]| {
            problems
                .get(index)
                .cloned()
                .unwrap_or_else(|| "(none)".to_string())
        };
        return Some(format!(
            "GNU R: {} | ry: {}",
            at(&expected.problems),
            at(&actual.problems)
        ));
    }
    if expected.expectations != actual.expectations {
        return Some(format!(
            "{} expectations under GNU R, {} under ry",
            expected.expectations, actual.expectations
        ));
    }
    if expected.value != actual.value {
        return Some(format!(
            "value: GNU R {} | ry {}",
            shorten(&expected.value),
            shorten(&actual.value)
        ));
    }
    None
}

fn first_problem(result: &TestResult) -> String {
    result
        .problems
        .first()
        .cloned()
        .unwrap_or_else(|| result.status.clone())
}

fn shorten(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    match line.char_indices().nth(120) {
        Some((index, _)) => format!("{}...", &line[..index]),
        None if text.contains('\n') => format!("{line}..."),
        None => line.to_string(),
    }
}

fn run_suite(
    package: &Path,
    name: &str,
    driver: &Path,
    runtime: &str,
    scratch: &Path,
    options: &Options,
) -> Result<Run, BenchError> {
    let report = scratch.join(format!("{runtime}.jsonl"));
    let stderr_path = scratch.join(format!("{runtime}.stderr"));
    let stderr = std::fs::File::create(&stderr_path).map_err(setup_error(&stderr_path))?;
    let started = Instant::now();
    let mut child = Command::new(&options.ry)
        .arg("load")
        .arg(package)
        .arg("--runtime")
        .arg(runtime)
        .arg("--file")
        .arg(driver)
        .current_dir(package)
        .env("RY_BENCH_PACKAGE", name)
        .env("RY_BENCH_TESTS", package.join("tests").join("testthat"))
        .env("RY_BENCH_REPORT", &report)
        .env("RY_BENCH_FILTER", &options.filter)
        .env("NOT_CRAN", "true")
        .env("TESTTHAT_CPUS", "1")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|error| BenchError::Setup(format!("{}: {error}", options.ry.display())))?;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.to_string(),
            Ok(None) if started.elapsed() > options.timeout => {
                if let Err(error) = child.kill() {
                    eprintln!("could not stop {runtime}: {error}");
                }
                if let Err(error) = child.wait() {
                    eprintln!("could not reap {runtime}: {error}");
                }
                break format!("timed out after {} s", options.timeout.as_secs());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => break format!("could not wait: {error}"),
        }
    };
    let wall = started.elapsed();
    let text = read_output(&report, runtime);
    let mut results = Vec::new();
    for line in text.lines() {
        match serde_json::from_str::<TestResult>(line) {
            Ok(result) => results.push(result),
            Err(error) => eprintln!("{runtime}: unreadable report line ({error}): {line}"),
        }
    }
    let suite_seconds = results
        .iter()
        .find(|result| result.test == "<total>")
        .map(|result| result.seconds);
    let stderr_text = read_output(&stderr_path, runtime);
    let stderr_tail = stderr_text
        .lines()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Run {
        results,
        wall,
        suite_seconds,
        exit,
        stderr_tail,
    })
}

fn package_name(package_dir: &Path) -> Result<String, BenchError> {
    let path = package_dir.join("DESCRIPTION");
    let text = std::fs::read_to_string(&path).map_err(setup_error(&path))?;
    text.lines()
        .find_map(|line| line.strip_prefix("Package:"))
        .map(|name| name.trim().to_string())
        .ok_or_else(|| BenchError::Setup(format!("{} has no Package field", path.display())))
}

/// A few benchmark dependencies cannot be installed everywhere (`polars` needs a newer R than
/// some hosts have, `Rblpapi` needs the proprietary Bloomberg SDK). The code reaches them only
/// through `::` inside guarded or injectable branches, so the copy under test leaves the missing
/// ones out of `Imports`, as `tools/bench/run-tests.R` does.
fn drop_missing_imports(package: &Path) -> Result<(), BenchError> {
    let installed = installed_packages()?;
    let path = package.join("DESCRIPTION");
    let text = std::fs::read_to_string(&path).map_err(setup_error(&path))?;
    let mut output = String::new();
    let mut in_imports = false;
    let mut dropped = Vec::new();
    for line in text.lines() {
        if line.starts_with("Imports:") {
            in_imports = true;
            output.push_str("Imports:\n");
            let rest = line.trim_start_matches("Imports:").trim();
            if !rest.is_empty() {
                keep_import(rest, &installed, &mut output, &mut dropped);
            }
            continue;
        }
        if in_imports && line.starts_with(char::is_whitespace) {
            keep_import(line.trim(), &installed, &mut output, &mut dropped);
            continue;
        }
        if in_imports {
            in_imports = false;
            trim_trailing_comma(&mut output);
        }
        output.push_str(line);
        output.push('\n');
    }
    if in_imports {
        trim_trailing_comma(&mut output);
    }
    if !dropped.is_empty() {
        eprintln!("not installed, left out of Imports: {}", dropped.join(", "));
    }
    std::fs::write(&path, output).map_err(setup_error(&path))
}

/// Drops the comma after the last kept import, wherever the field ends.
fn trim_trailing_comma(output: &mut String) {
    if output.ends_with(",\n") {
        output.truncate(output.len() - 2);
        output.push('\n');
    }
}

/// A runtime's report or stderr. A runtime that died before writing has no file, which reads as
/// empty; invalid UTF-8 is replaced rather than losing the whole report.
fn read_output(path: &Path, runtime: &str) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            eprintln!("{runtime}: could not read {}: {error}", path.display());
            String::new()
        }
    }
}

fn keep_import(
    entries: &str,
    installed: &HashSet<String>,
    output: &mut String,
    dropped: &mut Vec<String>,
) {
    for entry in entries
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let name = entry.split(['(', ' ']).next().unwrap_or(entry).trim();
        if installed.contains(name) {
            output.push_str(&format!("    {entry},\n"));
        } else {
            dropped.push(name.to_string());
        }
    }
}

/// Asks GNU R, because it knows its own library paths and `R_LIBS`.
fn installed_packages() -> Result<HashSet<String>, BenchError> {
    let output =
        Command::new(std::env::var("ORACLE_RSCRIPT").unwrap_or_else(|_| "Rscript".to_string()))
            .args([
                "--vanilla",
                "-e",
                "cat(rownames(installed.packages()), sep = '\\n')",
            ])
            .output()
            .map_err(|error| BenchError::Setup(format!("Rscript: {error}")))?;
    if !output.status.success() {
        return Err(BenchError::Setup(format!(
            "Rscript could not list the installed packages: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), BenchError> {
    std::fs::create_dir_all(to).map_err(setup_error(to))?;
    for entry in std::fs::read_dir(from).map_err(setup_error(from))? {
        let entry = entry.map_err(setup_error(from))?;
        let target = to.join(entry.file_name());
        if entry
            .file_type()
            .map_err(setup_error(&entry.path()))?
            .is_dir()
        {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(setup_error(&target))?;
        }
    }
    Ok(())
}

fn setup_error(path: &Path) -> impl Fn(std::io::Error) -> BenchError + '_ {
    move |error| BenchError::Setup(format!("{}: {error}", path.display()))
}
