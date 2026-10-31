//! `ry load`, `load_all()` and the plain-R entry point against a real GNU R. They need a
//! local R and skip (loudly, but green) without one; CI has no R, so run them locally:
//! `cargo test -p ry-lang --test test_load -- --nocapture`.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn r_available() -> bool {
    let available = Command::new("R")
        .arg("RHOME")
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!("skipped: no R installation on this machine");
    }
    available
}

/// A package directory holding `files` (paths relative to its root).
fn package(files: &[(&str, &str)]) -> TempDir {
    let directory = tempfile::tempdir().expect("temp dir");
    for (path, text) in files {
        let path = directory.path().join(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("create dirs");
        fs::write(path, text).expect("write file");
    }
    directory
}

fn ry_load(root: &Path, code: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ry"));
    command.arg("load").arg(root);
    for expression in code {
        command.arg("-e").arg(expression);
    }
    command.output().expect("run ry load")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const DEMO: &[(&str, &str)] = &[
    (
        "DESCRIPTION",
        "Package: demo\nVersion: 0.1.0\nCollate: 'second.R' 'first.R'\n",
    ),
    (
        "NAMESPACE",
        "export(greet)\nS3method(print, money)\nimportFrom(stats, median)\n",
    ),
    (
        "R/first.R",
        "greet <- function(name) paste(\"hello\", name, suffix())\n\
         money <- function(x) structure(list(amount = x), class = \"money\")\n\
         print.money <- function(x, ...) cat(\"$\", x$amount, \"\\n\", sep = \"\")\n\
         middle <- function(x) median(x)\n\
         doubled <- BASE * 2\n",
    ),
    (
        "R/second.R",
        "suffix <- function() \"!\"\nBASE <- 21\n\
         setClass(\"Point\", representation(x = \"numeric\"))\n\
         setGeneric(\"norm1\", function(p) standardGeneric(\"norm1\"))\n\
         setMethod(\"norm1\", \"Point\", function(p) abs(p@x))\n",
    ),
];

#[test]
fn a_package_loads_with_collate_imports_s3_and_s4() {
    if !r_available() {
        return;
    }
    let root = package(DEMO);
    let output = ry_load(
        root.path(),
        &[
            "cat(greet('ry'), '\\n')",
            "print(money(5))",
            "cat(doubled, middle(c(1, 9, 3)), norm1(new('Point', x = -4)), '\\n')",
            "cat(environmentName(environment(greet)), isNamespaceLoaded('demo'), '\\n')",
            "cat('suffix' %in% ls('package:demo'), demo::greet('pkg'), '\\n')",
        ],
    );
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(
        printed.contains("Loaded demo: 5 functions bound lazily, 5 top-level statements run"),
        "{printed}"
    );
    assert!(printed.contains("hello ry !"), "{printed}");
    assert!(printed.contains("$5"), "{printed}");
    assert!(printed.contains("42 3 4"), "{printed}");
    assert!(printed.contains("demo TRUE"), "{printed}");
    assert!(printed.contains("TRUE hello pkg !"), "{printed}");
}

#[test]
fn a_reload_replaces_changed_items_and_removes_deleted_ones() {
    if !r_available() {
        return;
    }
    let root = package(DEMO);
    let first = root.path().join("R/first.R");
    let edited = fs::read_to_string(&first)
        .expect("read")
        .replace("\"hello\"", "\"howdy\"")
        .replace(
            "middle <- function(x) median(x)\n",
            "added <- function() 42\n",
        );
    let script = format!(
        "writeLines({edited:?}, {path:?})\n\
         load_all({root:?})\n\
         cat(greet('ry'), added(), exists('middle', envir = asNamespace('demo')), '\\n')\n\
         load_all({root:?})\n",
        path = first.display().to_string(),
        root = root.path().display().to_string(),
    );
    let output = ry_load(root.path(), &[&script]);
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("Reloaded demo:"), "{printed}");
    assert!(printed.contains("howdy ry ! 42 FALSE"), "{printed}");
    assert!(printed.contains("binding removed"), "{printed}");
    assert!(printed.contains("demo is up to date."), "{printed}");
}

#[test]
fn a_statement_that_read_a_changed_function_reruns() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: cascade\nVersion: 1.0\n"),
        (
            "R/a.R",
            "base <- function() 1\ncaptured <- base()\nwrapped <- function() captured\n",
        ),
    ]);
    let file = root.path().join("R/a.R");
    let script = format!(
        "writeLines(c('base <- function() 2', 'captured <- base()', 'wrapped <- function() captured'), {path:?})\n\
         load_all({root:?})\ncat('captured:', wrapped(), '\\n')\n",
        path = file.display().to_string(),
        root = root.path().display().to_string(),
    );
    let output = ry_load(root.path(), &[&script]);
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("1 statement rerun"), "{printed}");
    assert!(printed.contains("captured: 2"), "{printed}");
}

#[test]
fn a_syntax_error_refuses_the_load_with_its_location() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: broken\nVersion: 1.0\n"),
        ("R/a.R", "fine <- function() 1\nbroken <- function( {\n"),
    ]);
    let output = ry_load(root.path(), &[]);
    let printed = text(&output);
    assert!(!output.status.success(), "{printed}");
    assert!(printed.contains("R/a.R:2:"), "{printed}");
}

#[test]
fn a_failing_statement_names_its_file_and_line() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: failing\nVersion: 1.0\n"),
        ("R/a.R", "fine <- function() 1\n\nvalue <- stop(\"boom\")\n"),
    ]);
    let output = ry_load(root.path(), &[]);
    let printed = text(&output);
    assert!(!output.status.success(), "{printed}");
    assert!(printed.contains("Error while loading R/a.R:3"), "{printed}");
    assert!(printed.contains("boom"), "{printed}");
}

#[test]
fn sysdata_loads_into_the_namespace() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: withdata\nVersion: 1.0\n"),
        ("R/a.R", "lookup <- function() internal_table$value\n"),
    ]);
    let rda = root.path().join("R/sysdata.rda");
    let saved = Command::new("Rscript")
        .arg("-e")
        .arg(format!(
            "internal_table <- list(value = 7); save(internal_table, file = {:?})",
            rda.display().to_string()
        ))
        .output()
        .expect("run Rscript");
    assert!(saved.status.success(), "{}", text(&saved));
    let output = ry_load(root.path(), &["cat('value:', lookup(), '\\n')"]);
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("value: 7"), "{printed}");
}

#[test]
fn plain_r_gets_load_all_from_the_entry_point() {
    if !r_available() {
        return;
    }
    let root = package(DEMO);
    let first = root.path().join("R/first.R");
    let edited = fs::read_to_string(&first)
        .expect("read")
        .replace("\"hello\"", "\"hi\"");
    let script = format!(
        "source(pipe({entry:?}))\n\
         load_all({root:?})\n\
         cat(greet('a'), '\\n')\n\
         writeLines({edited:?}, {path:?})\n\
         load_all({root:?})\n\
         cat(greet('b'), '\\n')\n",
        entry = format!("{} load --r-entry", env!("CARGO_BIN_EXE_ry")),
        root = root.path().display().to_string(),
        path = first.display().to_string(),
    );
    let output = Command::new("Rscript")
        .arg("-e")
        .arg(&script)
        .output()
        .expect("run Rscript");
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("hello a !"), "{printed}");
    assert!(
        printed.contains("Reloaded demo: 1 function rebound"),
        "{printed}"
    );
    assert!(printed.contains("hi b !"), "{printed}");
}
