//! Installs corpus packages from their source tarballs through ry's `R CMD INSTALL`, on ry's
//! runtime, and then loads each installed package in a fresh ry session. A package installs once
//! every package it depends on (`Depends`, `Imports`, `LinkingTo`) has installed, and is skipped
//! when one of them failed.
//!
//! Layout under the corpus directory: `sources/<pkg>_<version>.tar.gz` and `closure.txt` (from
//! `tools/corpus/sources.R`) as input; `ry-install/logs/<pkg>.{install,load}.log` and
//! `ry-install/summary.tsv` as output.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Where things are and how to run them.
pub struct Settings {
    pub corpus: PathBuf,
    /// ry's R home, assembled by `ry-rcmd assemble`.
    pub home: PathBuf,
    /// The library packages install into.
    pub library: PathBuf,
    pub jobs: usize,
    /// The limit for one package's installation, and separately for its load check.
    pub timeout: Duration,
}

/// What happened to one package.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Installed, and `loaded` says whether `library()` then succeeded.
    Installed { seconds: f64, loaded: Result<(), String> },
    Failed { seconds: f64, error: String },
    /// A dependency failed or is missing, so the package was not attempted.
    Skipped(String),
    NoSources,
}

/// Installs `packages` (and nothing else), returning one outcome per package in input order.
/// `report` is called as each package finishes.
pub fn run(
    settings: &Settings,
    packages: &[String],
    report: &(dyn Fn(&str, &Outcome) + Sync),
) -> Vec<(String, Outcome)> {
    let wanted: BTreeSet<&str> = packages.iter().map(String::as_str).collect();
    let mut dependencies: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut tarballs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut outcomes: BTreeMap<String, Outcome> = BTreeMap::new();
    let base = base_packages(&settings.home);
    for package in packages {
        let Some(tarball) = find_tarball(&settings.corpus.join("sources"), package) else {
            outcomes.insert(package.clone(), Outcome::NoSources);
            continue;
        };
        let needs = description_dependencies(&tarball, package)
            .into_iter()
            .filter(|dependency| !base.contains(dependency))
            .collect();
        dependencies.insert(package.clone(), needs);
        tarballs.insert(package.clone(), tarball);
    }
    for (package, needs) in &dependencies {
        if let Some(missing) = needs.iter().find(|dependency| !wanted.contains(dependency.as_str())) {
            outcomes.insert(package.clone(), Outcome::Skipped(format!("{missing} is not in the corpus")));
        }
    }
    for (package, outcome) in &outcomes {
        report(package, outcome);
    }
    let logs = settings.corpus.join("ry-install").join("logs");
    if let Err(error) = fs::create_dir_all(&logs).and_then(|()| fs::create_dir_all(&settings.library)) {
        eprintln!("cannot create {}: {error}", logs.display());
        return Vec::new();
    }
    let state = Mutex::new(Schedule { outcomes, running: BTreeSet::new() });
    let changed = Condvar::new();
    std::thread::scope(|scope| {
        for _ in 0..settings.jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let package = {
                        let Ok(mut schedule) = state.lock() else { return };
                        loop {
                            let pending: Vec<&String> = tarballs
                                .keys()
                                .filter(|package| !schedule.outcomes.contains_key(*package) && !schedule.running.contains(*package))
                                .collect();
                            if pending.is_empty() {
                                changed.notify_all();
                                return;
                            }
                            let mut ready = None;
                            let mut newly_skipped = Vec::new();
                            for package in pending {
                                let needs = &dependencies[package];
                                if let Some(failed) = needs.iter().find(|dependency| {
                                    schedule.outcomes.get(*dependency).is_some_and(|outcome| !matches!(outcome, Outcome::Installed { .. }))
                                }) {
                                    newly_skipped.push((package.clone(), failed.clone()));
                                } else if ready.is_none()
                                    && needs.iter().all(|dependency| matches!(schedule.outcomes.get(dependency), Some(Outcome::Installed { .. })))
                                {
                                    ready = Some(package.clone());
                                }
                            }
                            let any_skipped = !newly_skipped.is_empty();
                            for (package, failed) in newly_skipped {
                                let outcome = Outcome::Skipped(format!("dependency {failed} did not install"));
                                report(&package, &outcome);
                                schedule.outcomes.insert(package, outcome);
                            }
                            if let Some(package) = ready {
                                schedule.running.insert(package.clone());
                                break package;
                            }
                            if any_skipped {
                                continue;
                            }
                            if schedule.running.is_empty() {
                                // A dependency cycle: nothing runs and nothing can start.
                                let stuck: Vec<String> = tarballs
                                    .keys()
                                    .filter(|package| !schedule.outcomes.contains_key(*package))
                                    .cloned()
                                    .collect();
                                for package in stuck {
                                    let outcome = Outcome::Skipped("a dependency cycle".into());
                                    report(&package, &outcome);
                                    schedule.outcomes.insert(package, outcome);
                                }
                                continue;
                            }
                            schedule = match changed.wait(schedule) {
                                Ok(schedule) => schedule,
                                Err(_) => return,
                            };
                        }
                    };
                    let outcome = install_one(settings, &package, &tarballs[&package], &logs);
                    report(&package, &outcome);
                    let Ok(mut schedule) = state.lock() else { return };
                    schedule.running.remove(&package);
                    schedule.outcomes.insert(package, outcome);
                    changed.notify_all();
                }
            });
        }
    });
    let outcomes = state.into_inner().map(|schedule| schedule.outcomes).unwrap_or_default();
    packages
        .iter()
        .map(|package| (package.clone(), outcomes.get(package).cloned().unwrap_or(Outcome::NoSources)))
        .collect()
}

struct Schedule {
    outcomes: BTreeMap<String, Outcome>,
    running: BTreeSet<String>,
}

/// `summary.tsv`: one row per package, then the counts.
pub fn summary(outcomes: &[(String, Outcome)]) -> String {
    let mut text = String::from("package\tinstall\tload\tseconds\tdetail\n");
    for (package, outcome) in outcomes {
        let row = match outcome {
            Outcome::Installed { seconds, loaded: Ok(()) } => format!("installed\tloaded\t{seconds:.1}\t"),
            Outcome::Installed { seconds, loaded: Err(error) } => format!("installed\tfailed\t{seconds:.1}\t{error}"),
            Outcome::Failed { seconds, error } => format!("failed\t-\t{seconds:.1}\t{error}"),
            Outcome::Skipped(reason) => format!("skipped\t-\t\t{reason}"),
            Outcome::NoSources => "no sources\t-\t\t".to_owned(),
        };
        text.push_str(&format!("{package}\t{row}\n"));
    }
    text
}

/// `(installed, loaded)` counts.
pub fn counts(outcomes: &[(String, Outcome)]) -> (usize, usize) {
    let installed = outcomes.iter().filter(|(_, outcome)| matches!(outcome, Outcome::Installed { .. })).count();
    let loaded = outcomes
        .iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Installed { loaded: Ok(()), .. }))
        .count();
    (installed, loaded)
}

fn install_one(settings: &Settings, package: &str, tarball: &Path, logs: &Path) -> Outcome {
    let started = Instant::now();
    let mut install = Command::new(settings.home.join("bin/R"));
    install
        .args(["CMD", "INSTALL", "--no-test-load", "-l"])
        .arg(&settings.library)
        .arg(tarball);
    let status = run_logged(install, settings, &logs.join(format!("{package}.install.log")));
    let seconds = started.elapsed().as_secs_f64();
    if let Err(error) = status {
        return Outcome::Failed { seconds, error };
    }
    let mut load = Command::new(settings.home.join("bin/Rscript"));
    load.args(["-e", &format!("suppressPackageStartupMessages(library({package}))")]);
    let loaded = run_logged(load, settings, &logs.join(format!("{package}.load.log")));
    Outcome::Installed { seconds, loaded }
}

/// Runs `command` with the corpus environment, its output going to `log`. A failure returns the
/// first line of the log that reads like an error.
fn run_logged(mut command: Command, settings: &Settings, log: &Path) -> Result<(), String> {
    let file = fs::File::create(log).map_err(|error| format!("cannot create {}: {error}", log.display()))?;
    let errors = file.try_clone().map_err(|error| error.to_string())?;
    let library = settings.library.display().to_string();
    command
        .env("R_LIBS", &library)
        .env("R_LIBS_USER", &library)
        .env("R_LIBS_SITE", "")
        .env("LC_ALL", "C.UTF-8")
        .env("LANGUAGE", "en")
        .env("MAKEFLAGS", "-j2")
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(errors);
    let status = crate::packages::run_with_deadline(command, Instant::now() + settings.timeout)
        .map_err(|error| error.to_string())?;
    match status {
        crate::packages::Status::Pass => Ok(()),
        crate::packages::Status::Timeout => Err(format!("timed out after {}s", settings.timeout.as_secs())),
        crate::packages::Status::Fail => {
            let text = fs::read(log).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
            Err(first_error(&text))
        }
    }
}

fn first_error(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| {
            let lower = line.to_lowercase();
            lower.contains("error") && !lower.starts_with("error: installing the r code")
        })
        .or_else(|| text.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("failed without output");
    let mut line: String = line.trim().chars().take(300).collect();
    line = line.replace('\t', " ");
    line
}

/// The packages ry's R home links from GNU R, which every package can use without installing.
fn base_packages(home: &Path) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = ["R"].iter().map(|name| (*name).to_owned()).collect();
    if let Ok(entries) = fs::read_dir(home.join("library")) {
        for entry in entries.flatten() {
            if entry.path().is_symlink() {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names
}

fn find_tarball(sources: &Path, package: &str) -> Option<PathBuf> {
    let prefix = format!("{package}_");
    fs::read_dir(sources).ok()?.flatten().map(|entry| entry.path()).find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".tar.gz"))
    })
}

/// The packages a source package needs installed first, read from the `DESCRIPTION` in its
/// tarball.
fn description_dependencies(tarball: &Path, package: &str) -> Vec<String> {
    let Ok(output) = Command::new("tar")
        .arg("-xzOf")
        .arg(tarball)
        .arg(format!("{package}/DESCRIPTION"))
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            fields.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let mut needs = Vec::new();
    for (name, value) in &fields {
        if matches!(name.as_str(), "Depends" | "Imports" | "LinkingTo") {
            for entry in value.split(',') {
                let name = entry.trim().split(|character: char| character.is_whitespace() || character == '(').next().unwrap_or("");
                if !name.is_empty() && !needs.iter().any(|known| known == name) {
                    needs.push(name.to_owned());
                }
            }
        }
    }
    needs
}
