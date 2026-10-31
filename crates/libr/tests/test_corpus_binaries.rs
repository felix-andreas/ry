//! The benchmark closure's compiled packages, installed as GNU R binaries, load into ry's libR
//! and register their routines. Each package loads in a child process of this test binary,
//! together with the packages it depends on (so cross-package C callables resolve), which keeps
//! a crash in one package's initialization from hiding the rest.
//!
//! The corpus is the library `tools/corpus/install.R` builds (`RY_CORPUS_LIBRARY`, default
//! `.corpus/library`), plus GNU R's own library (`R RHOME`/library) for the base packages.
//! Without a corpus the test reports that and passes. The full report goes to
//! `target/tmp/corpus-binaries.tsv`.

mod harness;

use harness::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

const CHILD_PACKAGE: &str = "RY_CORPUS_CHILD_PACKAGE";
const CHILD_LIBRARIES: &str = "RY_CORPUS_CHILD_LIBRARIES";

/// An installed package that ships a shared object.
#[derive(Clone, Debug)]
struct Package {
    directory: PathBuf,
    /// A base package (`Priority: base`) ships with GNU R and is built from R's own sources
    /// against its internal headers, so it is reported apart from the benchmark closure.
    base: bool,
    library: PathBuf,
    dependencies: Vec<String>,
}

fn corpus_libraries() -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    let corpus = std::env::var_os("RY_CORPUS_LIBRARY").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.corpus/library"),
        PathBuf::from,
    );
    if corpus.is_dir() {
        libraries.push(corpus);
    }
    if let Ok(output) = Command::new("R").arg("RHOME").output()
        && output.status.success()
    {
        let home = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let system = Path::new(&home).join("library");
        if system.is_dir() {
            libraries.push(system);
        }
    }
    libraries
}

/// The DESCRIPTION fields that name packages whose code must be loaded first.
fn dependencies(description: &str) -> Vec<String> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in description.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(field) = &current {
                fields.entry(field.clone()).or_default().push_str(line);
            }
        } else if let Some((field, value)) = line.split_once(':') {
            current = Some(field.trim().to_owned());
            fields.insert(field.trim().to_owned(), value.to_owned());
        }
    }
    let mut names = Vec::new();
    for field in ["Depends", "Imports", "LinkingTo"] {
        if let Some(value) = fields.get(field) {
            for entry in value.split(',') {
                let name = entry
                    .trim()
                    .split(|character: char| character.is_whitespace() || character == '(')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                if !name.is_empty() && name != "R" {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn scan(libraries: &[PathBuf]) -> BTreeMap<String, Package> {
    let mut packages = BTreeMap::new();
    for library in libraries {
        let Ok(entries) = std::fs::read_dir(library) else {
            continue;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if packages.contains_key(&name) {
                continue;
            }
            let Ok(description) = std::fs::read_to_string(directory.join("DESCRIPTION")) else {
                continue;
            };
            let Ok(objects) = std::fs::read_dir(directory.join("libs")) else {
                continue;
            };
            let mut objects: Vec<PathBuf> = objects
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "so"))
                .collect();
            objects.sort();
            // The package's own object is named after it, with dots as underscores in a few
            // packages (data.table builds data_table.so).
            let wanted = [format!("{name}.so"), format!("{}.so", name.replace('.', "_"))];
            let Some(library) = objects
                .iter()
                .find(|path| {
                    path.file_name()
                        .is_some_and(|file| wanted.iter().any(|wanted| file == wanted.as_str()))
                })
                .or(objects.first())
                .cloned()
            else {
                continue;
            };
            let base = description
                .lines()
                .any(|line| line.trim() == "Priority: base");
            packages.insert(
                name,
                Package {
                    base,
                    directory,
                    library,
                    dependencies: dependencies(&description),
                },
            );
        }
    }
    packages
}

/// `name` and the packages it depends on, dependencies first.
fn load_order(name: &str, packages: &BTreeMap<String, Package>) -> Vec<String> {
    fn visit(
        name: &str,
        packages: &BTreeMap<String, Package>,
        seen: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !seen.insert(name.to_owned()) {
            return;
        }
        if let Some(package) = packages.get(name) {
            for dependency in &package.dependencies {
                visit(dependency, packages, seen, order);
            }
            order.push(name.to_owned());
        }
    }
    let mut order = Vec::new();
    visit(name, packages, &mut BTreeSet::new(), &mut order);
    order
}

/// The child: loads each library in order and prints one line per library.
fn child(libraries: &str, target: &str) {
    let r = r();
    for path in libraries.split('\n').filter(|path| !path.is_empty()) {
        let path_value = r.string(path);
        // RcppParallel's .onLoad loads its library with `local = FALSE`, so the packages
        // linking to it resolve its functions; until the interpreter runs .onLoad, the harness
        // does the same.
        let local: Sexp = if path.contains("/RcppParallel/libs/") {
            r.data("R_FalseValue")
        } else {
            r.data("R_TrueValue")
        };
        let outcome = r.eval("dyn.load", &[(None, path_value), (Some("local"), local)]);
        let is_target = Path::new(path)
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == target);
        match outcome {
            Ok(_) => println!("RY-LOADED\t{path}"),
            Err(message) => {
                let message = message.replace(['\n', '\t'], " ");
                println!("RY-FAILED\t{path}\t{}", message.trim());
                if is_target {
                    return;
                }
            }
        }
    }
}

#[derive(Debug)]
enum Outcome {
    Loaded,
    Failed(String),
    DependencyFailed(String),
    Crashed(String),
}

#[test]
fn corpus_binaries_load_and_register() {
    if let (Ok(target), Ok(libraries)) =
        (std::env::var(CHILD_PACKAGE), std::env::var(CHILD_LIBRARIES))
    {
        child(&libraries, &target);
        return;
    }
    let libraries = corpus_libraries();
    let packages = scan(&libraries);
    if packages.is_empty() {
        eprintln!("no corpus library found; run `just corpus` to install it");
        return;
    }
    let executable = std::env::current_exe().expect("the test executable");
    let mut outcomes: BTreeMap<String, Outcome> = BTreeMap::new();
    for name in packages.keys() {
        let order = load_order(name, &packages);
        let paths: Vec<String> = order
            .iter()
            .filter_map(|name| packages.get(name))
            .map(|package| package.library.to_string_lossy().into_owned())
            .collect();
        let output = Command::new(&executable)
            .args(["corpus_binaries_load_and_register", "--exact", "--nocapture"])
            .env(CHILD_PACKAGE, name)
            .env(CHILD_LIBRARIES, paths.join("\n"))
            .output()
            .expect("run the child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let target = packages[name].library.to_string_lossy().into_owned();
        let mut outcome = None;
        for line in stdout.lines() {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next(), fields.next()) {
                (Some("RY-LOADED"), Some(path), _) if path == target => {
                    outcome = Some(Outcome::Loaded);
                }
                (Some("RY-FAILED"), Some(path), Some(message)) if path == target => {
                    outcome = Some(Outcome::Failed(message.to_owned()));
                }
                (Some("RY-FAILED"), Some(path), Some(message)) if outcome.is_none() => {
                    let dependency = Path::new(path)
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::file_name)
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    outcome = Some(Outcome::DependencyFailed(format!("{dependency}: {message}")));
                }
                _ => {}
            }
        }
        let outcome = outcome.unwrap_or_else(|| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("");
            Outcome::Crashed(format!("{} {}", output.status, last.trim()))
        });
        outcomes.insert(name.clone(), outcome);
    }
    let mut report = String::from("package\tkind\toutcome\tdetail\n");
    let mut loaded = 0;
    let mut closure = 0;
    for (name, outcome) in &outcomes {
        let base = packages[name].base;
        if !base {
            closure += 1;
        }
        let (label, detail) = match outcome {
            Outcome::Loaded => {
                if !base {
                    loaded += 1;
                }
                ("loaded", packages[name].directory.to_string_lossy().into_owned())
            }
            Outcome::Failed(message) => ("failed", message.clone()),
            Outcome::DependencyFailed(message) => ("dependency-failed", message.clone()),
            Outcome::Crashed(message) => ("crashed", message.clone()),
        };
        let kind = if base { "base" } else { "closure" };
        report.push_str(&format!("{name}\t{kind}\t{label}\t{detail}\n"));
    }
    let report_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus-binaries.tsv");
    std::fs::write(&report_path, &report).expect("write the report");
    eprintln!(
        "{loaded} of {closure} compiled packages outside base load under ry; report: {}",
        report_path.display()
    );
    for (name, outcome) in &outcomes {
        if !matches!(outcome, Outcome::Loaded) {
            eprintln!("  {name}: {outcome:?}");
        }
    }
}
