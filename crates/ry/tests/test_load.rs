//! `ry load`, `load_all()` and the plain-R entry point, on ry's runtime (the default, which
//! reads base R from a GNU R installation) and on GNU R. They need a local R and skip (loudly,
//! but green) without one; CI has no R, so run them locally:
//! `cargo test -p ry-lang --test test_load -- --nocapture`.

#![cfg(unix)]

use {
    std::{
        fs,
        path::Path,
        process::{Command, Output},
    },
    tempfile::TempDir,
};

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

fn installed(package: &str) -> bool {
    let installed = Command::new("Rscript")
        .arg("-e")
        .arg(format!(
            "quit(status = !requireNamespace('{package}', quietly = TRUE))"
        ))
        .status()
        .is_ok_and(|status| status.success());
    if !installed {
        eprintln!("skipped: {package} is not installed");
    }
    installed
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

/// The runtimes a test that names them runs on, by their `--runtime` names.
const RUNTIMES: &[&str] = &["ry", "gnu-r"];

fn ry_load(root: &Path, code: &[&str]) -> Output {
    ry_load_on("ry", root, code)
}

fn ry_load_on(runtime: &str, root: &Path, code: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ry"));
    command.arg("load").arg("--runtime").arg(runtime).arg(root);
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
    for runtime in RUNTIMES {
        let output = ry_load_on(
            runtime,
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
        // ry's runtime holds the three S4 definitions back until `norm1` is first called.
        let summary = if *runtime == "ry" {
            "Loaded demo: 5 definitions bound lazily, 2 top-level statements run, \
             3 S4 definitions held back until first use"
        } else {
            "Loaded demo: 5 definitions bound lazily, 5 top-level statements run"
        };
        assert!(printed.contains(summary), "{printed}");
        assert!(printed.contains("hello ry !"), "{printed}");
        assert!(printed.contains("$5"), "{printed}");
        assert!(printed.contains("42 3 4"), "{printed}");
        assert!(printed.contains("demo TRUE"), "{printed}");
        assert!(printed.contains("TRUE hello pkg !"), "{printed}");
    }
}

#[test]
fn a_reload_replaces_changed_items_and_removes_deleted_ones() {
    if !r_available() {
        return;
    }
    for runtime in RUNTIMES {
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
        let output = ry_load_on(runtime, root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{printed}");
        assert!(printed.contains("Reloaded demo:"), "{printed}");
        assert!(printed.contains("howdy ry ! 42 FALSE"), "{printed}");
        assert!(printed.contains("binding removed"), "{printed}");
        assert!(printed.contains("demo is up to date."), "{printed}");
    }
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
fn strict_mode_refuses_an_untyped_package_on_both_runtimes() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: strictpkg\nVersion: 1.0\n"),
        ("ry.toml", "[check]\ntyping = true\nstrict = true\n"),
        ("R/a.R", "describe <- function(x) cat(\"a value\\n\")\n"),
        // A name another file defines resolves, so only `describe` is reported.
        ("R/b.R", "helper <- function(x: double) -> double x + 1\n"),
        ("R/c.R", "uses <- function(y: double) -> double helper(y)\n"),
    ]);
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, root.path(), &["cat(\"ran\\n\")"]);
        let printed = text(&output);
        assert_eq!(output.status.code(), Some(1), "{runtime}: {printed}");
        assert!(printed.contains("R/a.R:1:22"), "{runtime}: {printed}");
        assert!(
            printed.contains("strict mode found 1 error, so this code does not run"),
            "{runtime}: {printed}"
        );
        assert!(!printed.contains("ran"), "{runtime}: {printed}");
    }
    fs::write(
        root.path().join("R/a.R"),
        "describe <- function(x: double) cat(\"a value\\n\")\n",
    )
    .expect("write file");
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, root.path(), &["describe(1)"]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(printed.contains("a value"), "{runtime}: {printed}");
    }
}

#[test]
fn a_loaded_package_cannot_define_classes_when_object_systems_are_off() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: classless\nVersion: 1.0\n"),
        ("ry.toml", "[check]\nobject-systems = false\n"),
        (
            "R/a.R",
            "make_account <- function() setRefClass(\"Account\")\n",
        ),
    ]);
    let output = ry_load(root.path(), &["cat(\"loaded\\n\")", "make_account()"]);
    let printed = text(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(printed.contains("loaded"), "{printed}");
    assert!(
        printed.contains(
            "Reference Classes are turned off in this project (`[check] object-systems = false`), so `setRefClass` cannot define a Reference Class"
        ),
        "{printed}"
    );
}

#[test]
fn a_strict_project_binds_pure_constants_lazily_on_both_runtimes() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: tally\nVersion: 1.0\n"),
        ("ry.toml", "[check]\nstrict = true\n"),
        (
            "R/a.R",
            "#: fn(x: double[]) -> double[]\nscale_up <- function(x) x * 2\n#: double[]\nbase <- c(1, 2)\n",
        ),
    ]);
    for runtime in ["ry", "gnu-r"] {
        let output = ry_load_on(runtime, root.path(), &["print(base)"]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("2 definitions bound lazily, 0 top-level statements run"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn r6_generators_bind_lazily_and_build_the_same_classes_on_both_runtimes() {
    if !r_available() {
        return;
    }
    if !installed("R6") {
        return;
    }
    // `Child` inherits from a class of an earlier file, and a method calls a function that
    // comes later, which only the method's call reads.
    let root = package(&[
        (
            "DESCRIPTION",
            "Package: counters\nVersion: 1.0\nImports: R6\n",
        ),
        ("NAMESPACE", "importFrom(R6, R6Class)\n"),
        (
            "R/a.R",
            "Base <- R6Class(\"Base\", public = list(n = 0, add = function() { self$n <- self$n + step(); invisible(self) }))\n",
        ),
        (
            "R/b.R",
            "Child <- R6::R6Class(\"Child\", inherit = Base, public = list(twice = function() { self$add(); self$add() }))\n\
             step <- function() 10\n",
        ),
    ]);
    let probe =
        "x <- Child$new(); x$twice(); cat(x$n, class(x), environmentName(Child$parent_env), '\\n')";
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, root.path(), &[probe]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("3 definitions bound lazily, 0 top-level statements run"),
            "{runtime}: {printed}"
        );
        assert!(
            printed.contains("20 Child Base R6 counters"),
            "{runtime}: {printed}"
        );
    }
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
        printed.contains("Reloaded demo: 1 definition rebound"),
        "{printed}"
    );
    assert!(printed.contains("hi b !"), "{printed}");
}

#[test]
fn depends_attach_and_deleted_s4_definitions_are_undone() {
    if !r_available() {
        return;
    }
    let root = package(&[
        (
            "DESCRIPTION",
            "Package: sfour\nVersion: 1.0\nDepends: stats4\n",
        ),
        (
            "R/a.R",
            "setClass(\"Pt\", representation(x = \"numeric\"))\n\
             setGeneric(\"mag\", function(p) standardGeneric(\"mag\"))\n\
             setMethod(\"mag\", \"Pt\", function(p) abs(p@x))\n",
        ),
    ]);
    let file = root.path().join("R/a.R");
    let script = format!(
        "cat('attached:', 'package:stats4' %in% search(), mag(new('Pt', x = -3)), '\\n')\n\
         writeLines('setGeneric(\"mag\", function(p) standardGeneric(\"mag\"))', {path:?})\n\
         load_all({root:?})\n\
         cat('left:', existsMethod('mag', 'Pt'), isClass('Pt'), '\\n')\n",
        path = file.display().to_string(),
        root = root.path().display().to_string(),
    );
    let output = ry_load(root.path(), &[&script]);
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("attached: TRUE 3"), "{printed}");
    assert!(printed.contains("2 registrations undone"), "{printed}");
    assert!(printed.contains("left: FALSE FALSE"), "{printed}");
}

/// A package whose S4 definitions ry's runtime may hold back.
const SHAPES: &[(&str, &str)] = &[
    (
        "DESCRIPTION",
        "Package: shapes\nVersion: 1.0\nCollate: 'a.R' 'b.R'\n",
    ),
    ("NAMESPACE", "exportPattern(\".\")\n"),
    (
        "R/a.R",
        "SIDES <- 4\n\
         Sq <- setClass(\"Sq\", representation(side = \"numeric\"))\n\
         setValidity(\"Sq\", function(object) if (object@side < 0) \"negative side\" else TRUE)\n\
         setGeneric(\"area\", function(shape) standardGeneric(\"area\"))\n\
         setMethod(\"area\", \"Sq\", function(shape) shape@side^2)\n",
    ),
    (
        "R/b.R",
        "helper <- function() SIDES\n\
         setGeneric(\"perimeter\", function(shape) standardGeneric(\"perimeter\"))\n\
         setMethod(\"perimeter\", \"Sq\", function(shape) SIDES * shape@side)\n",
    ),
];

#[test]
fn held_back_s4_definitions_behave_as_if_run_eagerly() {
    if !r_available() {
        return;
    }
    let probe = "cat('probe:', isVirtualClass('Sq'), existsMethod('area', 'Sq'), \
                 area(Sq(side = 3)), perimeter(new('Sq', side = 2)), \
                 tryCatch(validObject(Sq(side = -1)), error = function(e) 'invalid'), \
                 is(Sq(side = 1), 'Sq'), if (exists('ORIGIN')) ORIGIN@side, \
                 sort(getGenerics(asNamespace('shapes'))), \
                 capture.output(showMethods('area')), names(getClass('Sq')@slots), \
                 names(getClass('Sq')@contains), '\\n')";
    // Without `ORIGIN`, every S4 definition stays held back after the load; with it, the load
    // itself needs the class and runs the definitions placed before `ORIGIN` at that point.
    let origin = "ORIGIN <- new(\"Sq\", side = 0)\n";
    for (uses_class, lazy_summary, expected) in [
        (
            false,
            "1 top-level statement run, 6 S4 definitions held back until first use",
            "FALSE TRUE 9 8 invalid TRUE ",
        ),
        (
            true,
            "8 top-level statements run, in",
            "FALSE TRUE 9 8 invalid TRUE 0 ",
        ),
    ] {
        let mut files = SHAPES.to_vec();
        let second = if uses_class {
            files[3]
                .1
                .replacen("setMethod", &format!("{origin}setMethod"), 1)
        } else {
            files[3].1.to_owned()
        };
        files[3].1 = &second;
        // The second lazy run starts from the package caches the first one wrote.
        let root = package(&files);
        let cache = tempfile::tempdir().expect("temp dir");
        let mut printed = Vec::new();
        for lazy in ["TRUE", "TRUE", "FALSE"] {
            let reload = format!(
                "options(ry.load.lazy_s4 = {lazy}); load_all({:?}, full = TRUE)",
                root.path().display().to_string()
            );
            let output = Command::new(env!("CARGO_BIN_EXE_ry"))
                .arg("load")
                .arg(root.path())
                .args(["-e", &reload, "-e", probe])
                .env("XDG_CACHE_HOME", cache.path())
                .output()
                .expect("run ry load");
            let text = text(&output);
            assert!(output.status.success(), "{text}");
            let line = text
                .lines()
                .find(|line| line.starts_with("probe:"))
                .unwrap_or_else(|| panic!("no probe line in {text}"))
                .to_owned();
            let summary = text
                .lines()
                .rfind(|line| line.starts_with("Loaded shapes"))
                .unwrap_or_else(|| panic!("no load summary in {text}"))
                .to_owned();
            printed.push((text, line, summary));
        }
        let (lazy, warm, eager) = (&printed[0], &printed[1], &printed[2]);
        assert_eq!(lazy.1, eager.1, "lazy:\n{}\neager:\n{}", lazy.0, eager.0);
        assert_eq!(warm.1, eager.1, "warm:\n{}\neager:\n{}", warm.0, eager.0);
        assert!(lazy.1.contains(expected), "{}", lazy.0);
        assert!(lazy.2.contains(lazy_summary), "{}", lazy.0);
        let statements = if uses_class { 8 } else { 7 };
        assert!(
            eager
                .2
                .contains(&format!("{statements} top-level statements run, in")),
            "{}",
            eager.0
        );
    }
}

#[test]
fn a_held_back_method_for_another_packages_generic_is_there_when_that_generic_needs_it() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: squares\nVersion: 1.0\n"),
        (
            "NAMESPACE",
            "exportPattern(\".\")\nexportMethods(as.list, toString)\n",
        ),
        (
            "R/a.R",
            "Sq <- setClass(\"Sq\", representation(side = \"numeric\"))\n\
             setMethod(\"show\", \"Sq\", function(object) cat(\"square\", object@side, \"\\n\"))\n\
             setMethod(\"length\", \"Sq\", function(x) 4L)\n\
             setMethod(\"[\", \"Sq\", function(x, i, ...) x@side[i])\n\
             setMethod(\"as.list\", \"Sq\", function(x, ...) list(side = x@side))\n\
             setMethod(\"toString\", \"Sq\", function(x, ...) paste(\"Sq\", x@side))\n\
             setMethod(\"initialize\", \"Sq\", function(.Object, ...) {\n\
               .Object <- callNextMethod(.Object, ...)\n\
               if (length(.Object@side) == 0) .Object@side <- 1\n\
               .Object\n\
             })\n\
             setClassUnion(\"Shape\", \"Sq\")\n\
             setClassUnion(\"NumOrNull\", c(\"numeric\", \"NULL\"))\n",
        ),
    ]);
    let saved = root.path().join("sq.rds");
    let load = |lazy: &str, probe: &str| {
        let reload = format!(
            "options(ry.load.lazy_s4 = {lazy}); load_all({:?}, full = TRUE)",
            root.path().display().to_string()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_ry"))
            .arg("load")
            .arg(root.path())
            .args(["-e", &reload, "-e", probe])
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .output()
            .expect("run ry load");
        let text = text(&output);
        assert!(output.status.success(), "{text}");
        text
    };
    load("FALSE", &format!("saveRDS(Sq(side = 3), {saved:?})"));
    // Each probe reaches the definitions first through a different door: a lookup of a
    // generic, dispatch on an object read back from disk (which needs no class lookup) through
    // a standard generic, a primitive, or a function the methods made generic, printing, a
    // union, and a new object whose initializer calls the default one.
    let read = format!("x <- readRDS({saved:?}); cat('probe: ')");
    for probe in [
        "cat('probe:', existsMethod('show', 'Sq'), existsMethod('length', 'Sq'), '\\n')".to_owned(),
        format!("{read}; show(x)"),
        format!("{read}; cat(length(x), x[1], '\\n')"),
        format!("{read}; cat(x[1], '\\n')"),
        format!("{read}; cat(unlist(as.list(x)), toString(x), '\\n')"),
        "cat('probe: '); print(Sq(side = 2))".to_owned(),
        "cat('probe:', is(1, 'NumOrNull'), extends('numeric'), isVirtualClass('Shape'), '\\n')"
            .to_owned(),
        "cat('probe:', new('Sq')@side, length(Sq(side = 5)), '\\n')".to_owned(),
    ] {
        let lazy = load("TRUE", &probe);
        let eager = load("FALSE", &probe);
        assert!(
            lazy.contains("9 S4 definitions held back until first use"),
            "{lazy}"
        );
        let line = |text: &str| {
            text.lines()
                .find(|line| line.starts_with("probe:"))
                .unwrap_or_else(|| panic!("no probe line in {text}"))
                .to_owned()
        };
        assert_eq!(line(&lazy), line(&eager), "lazy:\n{lazy}\neager:\n{eager}");
    }
}

/// A held-back definition that fails when a lazy generic's first use runs it leaves that
/// generic reporting the failure on every later use, not missing.
#[test]
fn a_lazy_generic_whose_held_back_definitions_fail_reports_the_failure_on_every_use() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: failing\nVersion: 1.0\n"),
        ("NAMESPACE", "exportPattern(\".\")\n"),
        (
            "R/a.R",
            "setClass(\"A\", representation(x = \"numeric\"))\n\
             setGeneric(\"foo\", function(x) standardGeneric(\"foo\"))\n\
             setMethod(\"foo\", \"A\", function(x) x@x)\n\
             setClass(\"B\", contains = \"Missing\")\n",
        ),
    ]);
    let reload = format!(
        "options(ry.load.lazy_s4 = TRUE); load_all({:?}, full = TRUE)",
        root.path().display().to_string()
    );
    let probe = "first <- tryCatch(foo(1), error = conditionMessage); \
                 later <- tryCatch(foo(1), error = conditionMessage); \
                 cat('probe:', grepl('Missing', first), later, '\\n')";
    let output = Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("load")
        .arg(root.path())
        .args(["-e", &reload, "-e", probe])
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .output()
        .expect("run ry load");
    let text = text(&output);
    assert!(
        text.contains(
            "probe: TRUE `foo` is unavailable because loading the S4 definitions of failing \
             failed; fix the error above and run `load_all()` again"
        ),
        "{text}"
    );
}

/// Unions that name basic classes, with or without one of the package's classes, and coercions
/// with `setAs()`, are held back with the classes they name, and each first use, in a process of
/// its own, sees what an eager load shows: union membership of the basic classes, everything a
/// basic class extends once a union was named, in the order the unions were defined, coercions
/// in both directions and their replacement form, method lookups, validity, and dispatch on an
/// object read back from disk. `worth` waits for its own first use, after the classes.
#[test]
fn unions_with_basic_classes_and_coercions_are_held_back_and_behave_as_if_run_eagerly() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: coins\nVersion: 1.0\n"),
        (
            "NAMESPACE",
            "exportPattern(\".\")\nexportClasses(Coin, Purse, CoinOrNull)\n",
        ),
        (
            "R/a.R",
            "Coin <- setClass(\"Coin\", representation(value = \"numeric\"))\n\
             setClass(\"Purse\", representation(coins = \"list\"))\n\
             setClassUnion(\"CoinOrNull\", c(\"Coin\", \"NULL\"))\n\
             setClassUnion(\"AmountOrNull\", c(\"numeric\", \"NULL\"))\n\
             setGeneric(\"worth\", function(x) standardGeneric(\"worth\"))\n\
             setMethod(\"worth\", \"CoinOrNull\", function(x) if (is.null(x)) 0 else x@value)\n\
             setMethod(\"worth\", \"AmountOrNull\", function(x) if (is.null(x)) 0 else sum(x))\n\
             setAs(\"Coin\", \"character\", function(from) paste(from@value, \"coins\"))\n\
             setAs(\"numeric\", \"Coin\", function(from) new(\"Coin\", value = from))\n\
             setAs(\"Coin\", \"Purse\", function(from) new(\"Purse\", coins = list(from)),\n\
               replace = function(from, value) { from@value <- length(value@coins); from })\n\
             setValidity(\"Coin\", function(object) if (length(object@value) == 1) TRUE else \"one value\")\n",
        ),
    ]);
    let saved = root.path().join("coin.rds");
    let load = |lazy: &str, probe: &str| {
        let reload = format!(
            "options(ry.load.lazy_s4 = {lazy}); load_all({:?}, full = TRUE)",
            root.path().display().to_string()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_ry"))
            .arg("load")
            .arg(root.path())
            .args(["-e", &reload, "-e", probe])
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .output()
            .expect("run ry load");
        let text = text(&output);
        assert!(output.status.success(), "{text}");
        text
    };
    load("FALSE", &format!("saveRDS(Coin(value = 3), {saved:?})"));
    let line = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("probe:"))
            .unwrap_or_else(|| panic!("no probe line in {text}"))
            .to_owned()
    };
    let probes = [
        "cat('probe:', is(NULL, 'CoinOrNull'), extends('NULL', 'CoinOrNull'), isVirtualClass('CoinOrNull'), '\\n')".to_owned(),
        "cat('probe:', worth(NULL), worth(Coin(value = 2)), is(Coin(value = 1), 'CoinOrNull'), '\\n')".to_owned(),
        "cat('probe:', is(1, 'AmountOrNull'), is('a', 'AmountOrNull'), extends('integer', 'AmountOrNull'), worth(c(1, 2)), '\\n')".to_owned(),
        "cat('probe:', is(NULL, 'CoinOrNull'), is(1, 'AmountOrNull'), extends('NULL'), is(1), '\\n')".to_owned(),
        "cat('probe:', as(3, 'Coin')@value, '\\n')".to_owned(),
        "cat('probe:', as(Coin(value = 2), 'character'), '\\n')".to_owned(),
        "cat('probe:', existsMethod('coerce', c('numeric', 'Coin')), existsMethod('coerce<-', c('Coin', 'Purse')), '\\n')".to_owned(),
        "x <- Coin(value = 1); as(x, 'Purse') <- new('Purse', coins = list(1, 2)); cat('probe:', x@value, '\\n')".to_owned(),
        "cat('probe:', tryCatch(new('Coin', value = 1:2), error = conditionMessage), '\\n')".to_owned(),
        "cat('probe:', capture.output(showMethods('worth')), '\\n')".to_owned(),
        format!("x <- readRDS({saved:?}); cat('probe:', as(x, 'character'), worth(x), '\\n')"),
    ];
    for probe in &probes {
        let lazy = load("TRUE", probe);
        let eager = load("FALSE", probe);
        assert!(
            lazy.contains("11 S4 definitions held back until first use"),
            "{lazy}"
        );
        assert_eq!(line(&lazy), line(&eager), "lazy:\n{lazy}\neager:\n{eager}");
    }
}

#[test]
fn package_functions_used_or_made_generic_around_held_back_methods_load_as_eagerly() {
    if !r_available() {
        return;
    }
    // `as.list` is used at load time before its method is defined, and `describe` is the
    // package's own function, which its method makes generic in place.
    let root = package(&[
        ("DESCRIPTION", "Package: listing\nVersion: 1.0\n"),
        (
            "NAMESPACE",
            "exportPattern(\".\")\nexportMethods(as.list)\n",
        ),
        (
            "R/a.R",
            "LISTED <- as.list(c(a = 1))\n\
             describe <- function(x) \"plain\"\n\
             Sq <- setClass(\"Sq\", representation(side = \"numeric\"))\n\
             setMethod(\"as.list\", \"Sq\", function(x, ...) list(side = x@side))\n\
             setMethod(\"describe\", \"Sq\", function(x) \"square\")\n",
        ),
    ]);
    let probe = "cat('probe:', names(LISTED), describe(1), describe(Sq(side = 1)), unlist(as.list(Sq(side = 2))), '\\n')";
    let load = |lazy: &str| {
        let reload = format!(
            "options(ry.load.lazy_s4 = {lazy}); load_all({:?}, full = TRUE)",
            root.path().display().to_string()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_ry"))
            .arg("load")
            .arg(root.path())
            .args(["-e", &reload, "-e", probe])
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .output()
            .expect("run ry load");
        let text = text(&output);
        assert!(output.status.success(), "{text}");
        text
    };
    let line = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("probe:"))
            .unwrap_or_else(|| panic!("no probe line in {text}"))
            .to_owned()
    };
    let lazy = load("TRUE");
    assert_eq!(line(&lazy), "probe: a plain square 2 ", "{lazy}");
    assert_eq!(line(&lazy), line(&load("FALSE")));
}

#[test]
fn a_reload_while_s4_definitions_are_held_back_keeps_them_working() {
    if !r_available() {
        return;
    }
    for (edit, expected) in [
        // A reload that leaves the S4 definitions alone keeps them held back.
        (("R/b.R", "SIDES", "SIDES + 0"), "after: 9 8"),
        // A reload that reruns one of them runs them all first.
        (("R/a.R", "shape@side^2", "shape@side^3"), "after: 27 8"),
    ] {
        let root = package(SHAPES);
        let (file, from, to) = edit;
        let path = root.path().join(file);
        let edited = fs::read_to_string(&path)
            .expect("read")
            .replacen(from, to, 1);
        let script = format!(
            "writeLines({edited:?}, {path:?})\n\
             load_all({root:?})\n\
             cat('after:', area(Sq(side = 3)), perimeter(Sq(side = 2)), '\\n')\n",
            path = path.display().to_string(),
            root = root.path().display().to_string(),
        );
        let output = ry_load(root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{printed}");
        assert!(printed.contains(expected), "{printed}");
    }
}

#[test]
fn a_second_load_reads_the_cached_index_and_an_edit_misses_it() {
    if !r_available() {
        return;
    }
    let root = package(SHAPES);
    let cache = tempfile::tempdir().expect("temp dir");
    let probe = "cat('probe:', area(Sq(side = 3)), perimeter(Sq(side = 2)), helper(), '\\n')";
    let run = |code: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_ry"))
            .arg("load")
            .arg(root.path())
            .arg("-e")
            .arg(code)
            .env("XDG_CACHE_HOME", cache.path())
            .output()
            .expect("run ry load");
        let printed = text(&output);
        assert!(output.status.success(), "{printed}");
        printed
            .lines()
            .find(|line| line.starts_with("probe:"))
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("no probe line in {printed}"))
    };
    let cold = run(probe);
    let entries = || {
        fs::read_dir(cache.path().join("ry").join("load-index"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    };
    assert_eq!(entries(), 1, "the first load writes one cache entry");
    assert_eq!(run(probe), cold, "a cache hit behaves like the cold load");

    let path = root.path().join("R/a.R");
    let edited =
        fs::read_to_string(&path)
            .expect("read")
            .replacen("shape@side^2", "shape@side^3", 1);
    let reload = format!(
        "writeLines({edited:?}, {path:?}); load_all({root:?}); {probe}",
        path = path.display().to_string(),
        root = root.path().display().to_string(),
    );
    let reloaded = run(&reload);
    assert_ne!(reloaded, cold, "a reload after a cache hit sees the edit");
    assert_eq!(run(probe), reloaded, "an edited package misses the cache");
}

#[test]
fn source_references_point_into_the_file() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: located\nVersion: 1.0\n"),
        (
            "R/a.R",
            "first <- function() 1\n\n\nlater <- function() {\n  2\n}\n\
             m <- function() \"\u{e9}\"; n <- function() 3\n",
        ),
    ]);
    // `n` starts after a two-byte character on its line, so its byte offset is one past its
    // column.
    let script = format!(
        "options(keep.source = TRUE)\nload_all({root:?}, full = TRUE)\n\
         cat('at', utils::getSrcLocation(later), basename(utils::getSrcFilename(later)), '\\n')\n\
         print(later)\ncat('byte', utils::getSrcLocation(n, 'byte'), '\\n')\n\
         cat('column', utils::getSrcLocation(n, 'column'), '\\n')\nprint(n)\n",
        root = root.path().display().to_string(),
    );
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(printed.contains("at 4 a.R"), "{runtime}: {printed}");
        // The function prints as written, not deparsed.
        assert!(
            printed.contains("later <- function() {") || printed.contains("function() {\n  2\n}"),
            "{runtime}: {printed}"
        );
        assert!(
            printed.contains("byte 28 \ncolumn 27 \nfunction() 3\n"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn system_file_finds_inst_and_helpers_are_sourced() {
    // As with pkgload, helpers are sourced only for a package whose testthat is installed.
    if !r_available() || !installed("testthat") {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: located\nVersion: 1.0\n"),
        ("NAMESPACE", "export(where)\n"),
        ("inst/extdata/a.txt", "hello\n"),
        (
            "R/a.R",
            ".onLoad <- function(lib, pkg) {\n\
             \x20 assign(\"at_load\", system.file(\"extdata\", \"a.txt\", package = \"located\"), envir = topenv())\n\
             }\n\
             where <- function() system.file(\"extdata\", \"a.txt\", package = \"located\")\n",
        ),
        (
            "tests/testthat/helper-values.R",
            "helper_value <- function() 42\n",
        ),
    ]);
    let code = "cat(readLines(where()), readLines(at_load), \
                readLines(system.file('extdata/a.txt', package = 'located')), helper_value(), '\\n')";
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, root.path(), &[code]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("hello hello hello 42"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn helpers_run_again_after_a_code_change_and_after_they_failed() {
    if !r_available() || !installed("testthat") {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: helped\nVersion: 1.0\n"),
        ("NAMESPACE", "export(f)\n"),
        ("R/a.R", ""),
        ("tests/testthat/helper-a.R", ""),
    ]);
    let code = root.path().join("R/a.R").display().to_string();
    let helper = root
        .path()
        .join("tests/testthat/helper-a.R")
        .display()
        .to_string();
    // A helper's value follows the package's code, and a helper that failed is sourced again by
    // the next load even when only package code changed.
    let script = format!(
        "cat(helper_value, '\\n')\n\
         writeLines('f <- function(x) x * 3', {code:?}); load_all({root:?}, quiet = TRUE)\n\
         cat(helper_value, '\\n')\n\
         writeLines('helper_value <- f(1); stop(\"helper broke\")', {helper:?})\n\
         cat(tryCatch(load_all({root:?}, quiet = TRUE), error = conditionMessage), '\\n')\n\
         writeLines('helper_value <- f(1)', {helper:?});\n\
         writeLines('f <- function(x) x * 5', {code:?}); load_all({root:?}, quiet = TRUE)\n\
         cat(helper_value, '\\n')\n",
        root = root.path().display().to_string(),
    );
    for runtime in RUNTIMES {
        fs::write(root.path().join("R/a.R"), "f <- function(x) x + 1\n").expect("write file");
        fs::write(&helper, "helper_value <- f(1)\n").expect("write file");
        let output = ry_load_on(runtime, root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("2 \n3 \nhelper broke \n5 \n"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn testthat_is_attached_and_helpers_sourced_as_pkgload_does() {
    if !r_available() || !installed("testthat") {
        return;
    }
    // pkgload's test directory is `inst/tests` when there is no `tests/testthat`, and its helpers
    // run with `NOT_CRAN` set, in the attached package environment rather than the namespace.
    let tested = package(&[
        ("DESCRIPTION", "Package: tested\nVersion: 1.0\n"),
        ("NAMESPACE", "export(f)\n"),
        ("R/a.R", "f <- function() 1\n"),
        (
            "inst/tests/helper-a.R",
            "helper_cran <- Sys.getenv(\"NOT_CRAN\")\n",
        ),
        (
            "inst/tests/test-a.R",
            "test_that(\"f\", expect_equal(f(), 1))\n",
        ),
    ]);
    let code = "cat(search()[2:3], helper_cran, \
                exists('helper_cran', asNamespace('tested'), inherits = FALSE), '\\n')";
    // A package without tests gets neither.
    let untested = package(&[
        ("DESCRIPTION", "Package: untested\nVersion: 1.0\n"),
        ("NAMESPACE", "export(f)\n"),
        ("R/a.R", "f <- function() 1\n"),
    ]);
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, tested.path(), &[code]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("package:tested package:testthat true FALSE"),
            "{runtime}: {printed}"
        );
        let output = ry_load_on(runtime, untested.path(), &["cat(search()[3], '\\n')"]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            !printed.contains("package:testthat"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn a_latin1_package_loads() {
    if !r_available() {
        return;
    }
    let directory = package(&[
        (
            "DESCRIPTION",
            "Package: accents\nVersion: 1.0\nEncoding: latin1\n",
        ),
        ("NAMESPACE", "export(greet)\n"),
    ]);
    fs::create_dir_all(directory.path().join("R")).expect("create R/");
    fs::write(
        directory.path().join("R/a.R"),
        b"greet <- function() \"caf\xe9\"\n",
    )
    .expect("write file");
    for runtime in RUNTIMES {
        let output = ry_load_on(
            runtime,
            directory.path(),
            &["cat(greet() == paste0('caf', '\\u00e9'), Encoding(greet()), '\\n')"],
        );
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(printed.contains("TRUE UTF-8"), "{runtime}: {printed}");
    }
    // Strict mode reads the file as latin1 too, so it finds the untyped parameter.
    fs::write(directory.path().join("ry.toml"), "[check]\nstrict = true\n").expect("write file");
    fs::write(
        directory.path().join("R/a.R"),
        b"#: fn() -> character\ngreet <- function() \"caf\xe9\"\nloose <- function(x) cat('a value')\n",
    )
    .expect("write file");
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, directory.path(), &["cat('ran\\n')"]);
        let printed = text(&output);
        assert_eq!(output.status.code(), Some(1), "{runtime}: {printed}");
        assert!(
            printed.contains("parameter `x` has no type"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn an_importer_sees_the_changed_bindings_of_a_reloaded_package() {
    if !r_available() {
        return;
    }
    let base = package(&[
        ("DESCRIPTION", "Package: basepkg\nVersion: 1.0\n"),
        ("NAMESPACE", "export(value)\n"),
        ("R/a.R", "value <- function() 1\n"),
    ]);
    let user = package(&[
        (
            "DESCRIPTION",
            "Package: userpkg\nVersion: 1.0\nImports: basepkg\n",
        ),
        ("NAMESPACE", "export(uses)\nimportFrom(basepkg, value)\n"),
        ("R/b.R", "uses <- function() value()\n"),
    ]);
    let script = format!(
        "load_all({user:?}, quiet = TRUE)\ncat(userpkg::uses(), '\\n')\n\
         writeLines('value <- function() 2', file.path({base:?}, 'R', 'a.R'))\n\
         load_all({base:?}, quiet = TRUE)\ncat(userpkg::uses(), '\\n')\n\
         writeLines('other <- function() 3', file.path({base:?}, 'R', 'a.R'))\n\
         writeLines('export(other)', file.path({base:?}, 'NAMESPACE'))\n\
         withCallingHandlers(load_all({base:?}, quiet = TRUE), \
           warning = function(w) {{ cat(conditionMessage(w), '\\n'); invokeRestart('muffleWarning') }})\n\
         cat(basepkg::other(), userpkg::uses(), '\\n')\n",
        base = base.path().display().to_string(),
        user = user.path().display().to_string(),
    );
    for runtime in RUNTIMES {
        fs::write(base.path().join("R/a.R"), "value <- function() 1\n").expect("write file");
        fs::write(base.path().join("NAMESPACE"), "export(value)\n").expect("write file");
        let output = ry_load_on(runtime, base.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(printed.contains("1 \n2 \n"), "{runtime}: {printed}");
        // An importer whose directives no longer fit keeps its old imports, and the reload
        // that removed the name still succeeds.
        assert!(
            printed.contains("userpkg's imports could not be rebuilt, so it keeps the old ones"),
            "{runtime}: {printed}"
        );
        assert!(printed.contains("3 2 \n"), "{runtime}: {printed}");
    }
}

#[test]
fn strict_mode_refuses_a_reload_on_both_runtimes() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: strictreload\nVersion: 1.0\n"),
        ("NAMESPACE", "export(g)\n"),
        (
            "R/a.R",
            "# typing: strict\n#: fn(x: double) -> double\ng <- function(x) x + 1\n",
        ),
    ]);
    let file = root.path().join("R/a.R").display().to_string();
    let script = format!(
        "writeLines(c('# typing: strict', '#: fn(x: double) -> double', \
         'g <- function(x) x + \"a\"'), {file:?})\n\
         cat(tryCatch(load_all({root:?}), error = conditionMessage), '\\n')\n\
         cat(g(1), '\\n')\n\
         writeLines(c('# typing: strict', '#: fn(x: double) -> double', \
         'g <- function(x) x + 2'), {file:?})\n\
         writeLines(c('# typing: strict', '#: fn() -> double', 'h <- function() \"a\"'), \
         {added:?})\n\
         cat(tryCatch(load_all({root:?}), error = conditionMessage), '\\n')\n\
         cat(g(1), '\\n')\n\
         invisible(file.remove({added:?}))\n\
         invisible(load_all({root:?}))\n\
         cat('fixed', g(1), '\\n')\n",
        root = root.path().display().to_string(),
        added = root.path().join("R/b.R").display().to_string(),
    );
    for runtime in RUNTIMES {
        fs::write(
            root.path().join("R/a.R"),
            "# typing: strict\n#: fn(x: double) -> double\ng <- function(x) x + 1\n",
        )
        .expect("write file");
        if let Err(error) = fs::remove_file(root.path().join("R/b.R")) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
        }
        let output = ry_load_on(runtime, root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("strictreload was not loaded, because strict mode refused it"),
            "{runtime}: {printed}"
        );
        // The refused reload ran nothing, so the old definition stays.
        assert!(printed.contains("\n2 \n"), "{runtime}: {printed}");
        // The check of each reload keeps the previous analysis: a file added with an error
        // refuses the next reload, and deleting it lets the fixed code load.
        assert_eq!(
            printed
                .matches("strictreload was not loaded, because strict mode refused it")
                .count(),
            2,
            "{runtime}: {printed}"
        );
        assert!(printed.contains("fixed 3 \n"), "{runtime}: {printed}");
    }
}

#[test]
fn a_strict_package_is_checked_when_a_lax_project_loads_it() {
    if !r_available() {
        return;
    }
    let lax = package(&[
        ("DESCRIPTION", "Package: laxpkg\nVersion: 1.0\n"),
        ("R/a.R", "lax <- function(x) x\n"),
    ]);
    let strict = package(&[
        ("DESCRIPTION", "Package: strictpkg\nVersion: 1.0\n"),
        ("NAMESPACE", "export(s)\n"),
        ("ry.toml", "[check]\nstrict = true\n"),
        ("R/a.R", "s <- function(x) cat('a value')\n"),
    ]);
    let script = format!(
        "cat(tryCatch(load_all({strict:?}, quiet = TRUE), error = conditionMessage), '\\n')\n",
        strict = strict.path().display().to_string(),
    );
    for runtime in RUNTIMES {
        let output = ry_load_on(runtime, lax.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("strictpkg was not loaded, because strict mode refused it"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn s3_registrations_refuse_a_missing_method_and_keep_one_delayed_hook() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: methodical\nVersion: 1.0\n"),
        (
            "NAMESPACE",
            "S3method(print, foo)\nS3method(notinstalledpkg::frob, foo)\nexport(make)\n",
        ),
        ("R/a.R", ""),
    ]);
    let file = root.path().join("R/a.R").display().to_string();
    let original = "make <- function() structure(1, class = 'foo')\n\
                    print.foo <- function(x, ...) cat('a foo\\n')\n\
                    frob.foo <- function(x) 1\n";
    // Each reload rebinds `frob.foo`, which re-registers its delayed hook.
    let script = format!(
        "print(make())\n\
         writeLines(c(\"make <- function() structure(1, class = 'foo')\", \
         \"print.foo <- function(x, ...) cat('a foo!\\\\n')\", 'frob.foo <- function(x) 2'), {file:?})\n\
         load_all({root:?}, quiet = TRUE)\nprint(make())\n\
         cat('hooks', length(getHook(packageEvent('notinstalledpkg', 'onLoad'))), '\\n')\n\
         writeLines(\"make <- function() structure(1, class = 'foo')\", {file:?})\n\
         cat(tryCatch(load_all({root:?}, quiet = TRUE), error = conditionMessage), '\\n')\n\
         print(make())\n",
        root = root.path().display().to_string(),
    );
    for runtime in RUNTIMES {
        fs::write(root.path().join("R/a.R"), original).expect("write file");
        let output = ry_load_on(runtime, root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("a foo\na foo!\nhooks 1"),
            "{runtime}: {printed}"
        );
        assert!(
            printed.contains("object 'print.foo' not found whilst loading namespace 'methodical'"),
            "{runtime}: {printed}"
        );
        // The refusal came before the reload changed anything.
        assert!(
            printed.contains("'methodical' \na foo!\n"),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn s3_refusal_comes_first_when_sysdata_changes_and_after_code_that_may_define_the_method() {
    if !r_available() {
        return;
    }
    let root = package(&[
        ("DESCRIPTION", "Package: timely\nVersion: 1.0\n"),
        ("NAMESPACE", "S3method(print, foo)\nexport(make)\n"),
        ("R/a.R", ""),
    ]);
    let original = "make <- function() structure(1, class = 'foo')\n\
                    print.foo <- function(x, ...) cat('a foo', limit, '\\n')\n";
    let file = root.path().join("R/a.R").display().to_string();
    let sysdata = root.path().join("R/sysdata.rda").display().to_string();
    // The first reload drops `print.foo` and rewrites `sysdata.rda`, which is refused before
    // `limit` changes. The second defines the method with a computed name, which only running
    // the statement can show.
    let script = format!(
        "limit <- 1; save(limit, file = {sysdata:?}); load_all({root:?}, quiet = TRUE)\n\
         print(make())\n\
         writeLines(\"make <- function() structure(1, class = 'foo')\", {file:?})\n\
         limit <- 2; save(limit, file = {sysdata:?})\n\
         cat(tryCatch({{ load_all({root:?}, quiet = TRUE); 'loaded' }}, error = conditionMessage), '\\n')\n\
         print(make())\n\
         writeLines(c(\"make <- function() structure(1, class = 'foo')\", \
         \"assign(paste0('print', '.foo'), function(x, ...) cat('computed foo', limit, '\\\\n'))\"), {file:?})\n\
         load_all({root:?}, quiet = TRUE)\nprint(make())\n",
        root = root.path().display().to_string(),
    );
    for runtime in RUNTIMES {
        fs::write(root.path().join("R/a.R"), original).expect("write file");
        let output = ry_load_on(runtime, root.path(), &[&script]);
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains(
                "a foo 1 \nobject 'print.foo' not found whilst loading namespace 'timely' \n\
                 a foo 1 \ncomputed foo 2 \n"
            ),
            "{runtime}: {printed}"
        );
    }
}

#[test]
fn ry_test_runs_testthat_against_the_loaded_package_on_both_runtimes() {
    if !r_available() || !installed("testthat") {
        return;
    }
    // `scale` is internal, so only a test run inside the namespace sees it, and `make` comes
    // from a helper file.
    let root = package(&[
        ("DESCRIPTION", "Package: counters\nVersion: 1.0\n"),
        ("NAMESPACE", "export(step)\n"),
        ("R/a.R", "step <- function(k = 1) k * scale\nscale <- 10\n"),
        (
            "tests/testthat/helper-make.R",
            "make <- function() step(2)\n",
        ),
        (
            "tests/testthat/test-step.R",
            "test_that(\"step scales\", expect_equal(make(), 20))\n",
        ),
        (
            "tests/testthat/test-broken.R",
            "test_that(\"a failure\", expect_equal(scale, 11))\n",
        ),
    ]);
    for runtime in RUNTIMES {
        let run = |filter: Option<&str>| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_ry"));
            command.args(["test", "--reporter", "summary", "--runtime", runtime]);
            if let Some(filter) = filter {
                command.args(["--filter", filter]);
            }
            command.arg(root.path()).output().expect("run ry test")
        };
        let output = run(None);
        let printed = text(&output);
        assert_eq!(output.status.code(), Some(1), "{runtime}: {printed}");
        assert!(printed.contains("broken: 1"), "{runtime}: {printed}");
        assert!(printed.contains("step: ."), "{runtime}: {printed}");
        // The wording of a failed expectation changes between testthat versions.
        assert!(
            printed.contains("`scale`") && printed.contains("11"),
            "{runtime}: {printed}"
        );
        let output = run(Some("step"));
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(!printed.contains("broken"), "{runtime}: {printed}");
    }
}

/// A package whose `src/` registers one `.Call` routine, `C_twice` under `.fixes = "C_"`.
const COMPILED: &[(&str, &str)] = &[
    ("DESCRIPTION", "Package: cpkg\nVersion: 1.0\n"),
    (
        "NAMESPACE",
        "useDynLib(cpkg, .registration = TRUE, .fixes = \"C_\")\nexport(twice)\n",
    ),
    ("R/twice.R", "twice <- function(x) .Call(C_twice, x)\n"),
    (
        "src/init.c",
        "#include <R.h>\n#include <Rinternals.h>\n#include <R_ext/Rdynload.h>\n\
         SEXP twice(SEXP x) { return Rf_ScalarReal(2 * Rf_asReal(x)); }\n\
         static const R_CallMethodDef calls[] = {{\"twice\", (DL_FUNC) &twice, 1}, {NULL, NULL, 0}};\n\
         void R_init_cpkg(DllInfo *dll) { R_registerRoutines(dll, NULL, calls, NULL, NULL); \
         R_useDynamicSymbols(dll, FALSE); }\n",
    ),
];

#[test]
fn compiled_code_builds_with_rys_toolchain_and_a_rebuild_reloads_it() {
    if !r_available() {
        return;
    }
    // `ry load` compiles with the `ry-rcmd` beside ry's runtime, and cargo test does not build
    // another package's binary.
    let profile_directory = Path::new(env!("CARGO_BIN_EXE_ry"))
        .parent()
        .expect("the ry binary has a directory");
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "-q", "-p", "rcmd", "--bin", "ry-rcmd"]);
    if profile_directory
        .file_name()
        .is_some_and(|name| name == "release")
    {
        build.arg("--release");
    }
    assert!(
        build.status().expect("run cargo build").success(),
        "building ry-rcmd failed"
    );

    let root = package(COMPILED);
    let cache = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("src/init.c");
    let edited =
        fs::read_to_string(&source)
            .expect("read")
            .replacen("2 * Rf_asReal", "3 * Rf_asReal", 1);
    let reload = format!(
        "cat('before:', twice(21), '\\n'); writeLines({edited:?}, {source:?}); \
         load_all({root:?}); cat('after:', twice(21), '\\n')",
        source = source.display().to_string(),
        root = root.path().display().to_string(),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("load")
        .arg(root.path())
        .arg("-e")
        .arg(&reload)
        .env("XDG_CACHE_HOME", cache.path())
        .output()
        .expect("run ry load");
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("before: 42"), "{printed}");
    assert!(printed.contains("after: 63"), "{printed}");
}

#[test]
fn namespace_if_directives_are_evaluated_at_every_load_on_both_runtimes() {
    if !r_available() {
        return;
    }
    let namespace = "export(always)\n\
         if (getRversion() >= \"3.0.0\") {\n  export(modern)\n  \
         if (Sys.getenv(\"RY_FLAG\") == \"on\") export(flagged) else export(unflagged)\n\
         } else export(ancient)\n\
         if (FALSE) export(never)\n";
    let root = package(&[
        ("DESCRIPTION", "Package: ifpkg\nVersion: 1.0\n"),
        ("NAMESPACE", namespace),
        (
            "R/a.R",
            "always <- function() 1\nmodern <- function() 2\nflagged <- function() 3\n\
             unflagged <- function() 4\nancient <- function() 5\nnever <- function() 6\n\
             later <- function() 7\n",
        ),
    ]);
    let path = root.path().join("NAMESPACE");
    let probe = |label: &str| {
        format!(
            "load_all({root:?}, export_all = FALSE); \
             cat('{label}:', sort(getNamespaceExports('ifpkg')), '\\n')",
            root = root.path().display().to_string(),
        )
    };
    let edited = format!("{namespace}if (TRUE) export(later)\n");
    // A condition the file no longer holds is not evaluated, even one that would now fail,
    // and an `if` inside a top-level `{` block is resolved like any other.
    let braced = "{\n  export(always)\n  if (FALSE) export(never) else export(modern)\n}\n";
    for runtime in RUNTIMES {
        fs::write(&path, namespace).expect("write NAMESPACE");
        let output = ry_load_on(
            runtime,
            root.path(),
            &[
                &probe("first"),
                "Sys.setenv(RY_FLAG = 'on')",
                &probe("flag"),
                &format!(
                    "writeLines({edited:?}, {path:?})",
                    path = path.display().to_string()
                ),
                &probe("edited"),
                "Sys.getenv <- function(...) stop('a stale condition ran')",
                &format!(
                    "writeLines({braced:?}, {path:?})",
                    path = path.display().to_string()
                ),
                &probe("braced"),
            ],
        );
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(
            printed.contains("first: always modern unflagged"),
            "{runtime}: {printed}"
        );
        assert!(
            printed.contains("flag: always flagged modern"),
            "{runtime}: {printed}"
        );
        assert!(
            printed.contains("edited: always flagged later modern"),
            "{runtime}: {printed}"
        );
        assert!(
            printed.contains("braced: always modern"),
            "{runtime}: {printed}"
        );
        assert!(!printed.contains("ignored"), "{runtime}: {printed}");
    }
}

const ROXYGEN: &[(&str, &str)] = &[
    (
        "DESCRIPTION",
        "Package: roxy\nTitle: Roxygen Test\nVersion: 0.1.0\nDescription: A test.\n\
         License: MIT\nImports: R6\nEncoding: UTF-8\nRoxygen: list(markdown = TRUE)\n",
    ),
    (
        "R/account.R",
        "#' An account\n#'\n#' Holds a balance.\n#' @export\n\
         Account <- R6::R6Class(\"Account\", public = list(\n\
         #' @field balance The balance.\nbalance = 0,\n\
         #' @description Deposit money.\n#' @param x How much.\n\
         deposit = function(x) {\nself$balance <- self$balance + x\ninvisible(self)\n}\n))\n",
    ),
    (
        "R/add.R",
        "#' Add two numbers\n#'\n#' Adds `x` and `y`.\n#' @param x A number.\n\
         #' @param y A number.\n#' @return The sum.\n#' @export\n#' @examples\n#' add(1, 2)\n\
         add <- function(x, y = 1) x + y\n\n#' @rdname add\n#' @export\n\
         sub2 <- function(x, y) x - y\n\n#' Print a thing\n#' @param x Thing.\n\
         #' @param ... Unused.\n#' @export\nprint.thing <- function(x, ...) cat(\"thing\\n\")\n",
    ),
    (
        "R/scale.R",
        "#' Scale\n#' @inheritParams add\n#' @param factor How much.\n#' @importFrom stats median\n\
         #' @export\nscale_by <- function(x, factor) x * factor\n",
    ),
    (
        "R/family.R",
        "#' Double\n#' @param x A number.\n#' @family arithmetic\n#' @export\n\
         double_it <- function(x) x * 2\n",
    ),
    (
        "R/half.R",
        "#' Half\n#' @inheritParams double_it\n#' @family arithmetic\n#' @export\n\
         halve <- function(x) x / 2\n",
    ),
];

/// Every file under `root` except `.git`, by relative path, with its bytes.
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).expect("under root");
                files.push((
                    relative.display().to_string(),
                    fs::read(&path).expect("read"),
                ));
            }
        }
    }
    files.sort();
    files
}

/// `ry document` writes what `devtools::document()` writes, byte for byte, on a cold run and on
/// each run after an edit. The later edits change only topics that unchanged files read, through
/// `@inheritParams` and `@family`, which the cached topics of those files must not hide.
#[test]
fn ry_document_matches_devtools_on_both_runtimes() {
    if !r_available() || !installed("roxygen2") || !installed("devtools") || !installed("R6") {
        return;
    }
    let edits: [(&str, &str, &str); 4] = [
        ("R/scale.R", "How much.", "How much to scale by."),
        (
            "R/add.R",
            "#' @param x A number.",
            "#' @param x A real number.",
        ),
        ("R/family.R", "@family arithmetic", "@family maths"),
        ("R/family.R", "#' Double", "#' Double a number"),
    ];
    let edit = |root: &Path, (file, from, to): (&str, &str, &str)| {
        let path = root.join(file);
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains(from), "{file} lacks {from}");
        fs::write(&path, text.replacen(from, to, 1)).expect("write");
    };
    let reference = package(ROXYGEN);
    let devtools = || {
        let output = Command::new("Rscript")
            .args(["-e", "devtools::document()"])
            .current_dir(reference.path())
            .output()
            .expect("run devtools");
        assert!(output.status.success(), "{}", text(&output));
        tree(reference.path())
    };
    let mut expected = vec![devtools()];
    for step in edits {
        edit(reference.path(), step);
        expected.push(devtools());
    }
    assert!(
        expected[0].iter().any(|(path, _)| path == "man/add.Rd"),
        "{:?}",
        expected[0]
    );
    for runtime in RUNTIMES {
        let root = package(ROXYGEN);
        let cache = tempfile::tempdir().expect("temp dir");
        let document = || {
            let output = Command::new(env!("CARGO_BIN_EXE_ry"))
                .args(["document", "--runtime", runtime])
                .arg(root.path())
                .env("XDG_CACHE_HOME", cache.path())
                .output()
                .expect("run ry document");
            assert!(output.status.success(), "{runtime}: {}", text(&output));
        };
        document();
        assert!(
            tree(root.path()) == expected[0],
            "{runtime}: the cold run differs"
        );
        for (index, step) in edits.into_iter().enumerate() {
            edit(root.path(), step);
            document();
            assert!(
                tree(root.path()) == expected[index + 1],
                "{runtime}: the run after editing {} differs",
                step.0
            );
        }
    }
}

/// A reload after an edit under `src/` rebuilds the shared library and the session calls the
/// new code, which on ry's runtime needs `dyn.unload` to retire the old library so the next
/// load maps the rebuilt file instead of returning the one already open.
#[test]
fn a_reload_rebuilds_compiled_code_on_both_runtimes() {
    if !r_available() || !installed("pkgbuild") {
        return;
    }
    let source = |answer: u32| {
        format!(
            "#include <R.h>\n#include <Rinternals.h>\n#include <R_ext/Rdynload.h>\n\
             SEXP c_answer(void) {{ return ScalarInteger({answer}); }}\n\
             static const R_CallMethodDef calls[] = {{{{\"c_answer\", (DL_FUNC) &c_answer, 0}}, {{NULL, NULL, 0}}}};\n\
             void R_init_cpkg(DllInfo *info) {{ R_registerRoutines(info, NULL, calls, NULL, NULL); R_useDynamicSymbols(info, FALSE); }}\n"
        )
    };
    for runtime in RUNTIMES {
        let original = source(41);
        let root = package(&[
            ("DESCRIPTION", "Package: cpkg\nVersion: 1.0\n"),
            (
                "NAMESPACE",
                "useDynLib(cpkg, .registration = TRUE)\nexport(answer)\n",
            ),
            ("R/answer.R", "answer <- function() .Call(c_answer)\n"),
            ("src/a.c", &original),
        ]);
        let path = root.path().join("src/a.c").display().to_string();
        let edit = format!(
            "Sys.sleep(1.1); writeLines({edited:?}, {path:?}); load_all({root:?})",
            edited = source(44),
            root = root.path().display().to_string(),
        );
        let output = ry_load_on(
            runtime,
            root.path(),
            &[
                "cat('before:', answer(), '\\n')",
                &edit,
                "cat('after:', answer(), '\\n')",
            ],
        );
        let printed = text(&output);
        assert!(output.status.success(), "{runtime}: {printed}");
        assert!(printed.contains("before: 41"), "{runtime}: {printed}");
        assert!(printed.contains("after: 44"), "{runtime}: {printed}");
    }
}
