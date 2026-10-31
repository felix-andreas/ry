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
use std::{collections::BTreeMap, io::Write, path::Path, process::Command};

const CHILD_PACKAGE: &str = "RY_CORPUS_CHILD_PACKAGE";
const CHILD_LIBRARIES: &str = "RY_CORPUS_CHILD_LIBRARIES";

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
        // An error in a library's static initializer leaves the dynamic loader's lock with this
        // test thread, and the exit handlers on the main thread would wait for it forever.
        std::io::stdout().flush().expect("flush the report");
        unsafe { libc::_exit(0) };
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
        // ry implements the base packages' internals itself, so GNU R's libraries for them
        // (which import GNU R internals) are loaded only when they are the package under test.
        let paths: Vec<String> = order
            .iter()
            .filter(|dependency| *dependency == name || !packages[*dependency].base)
            .filter_map(|name| packages.get(name))
            .map(|package| package.library.to_string_lossy().into_owned())
            .collect();
        let output = Command::new(&executable)
            .args([
                "corpus_binaries_load_and_register",
                "--exact",
                "--nocapture",
            ])
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
                    outcome = Some(Outcome::DependencyFailed(format!(
                        "{dependency}: {message}"
                    )));
                }
                _ => {}
            }
        }
        let outcome = outcome.unwrap_or_else(|| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last = stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
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
                (
                    "loaded",
                    packages[name].directory.to_string_lossy().into_owned(),
                )
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
