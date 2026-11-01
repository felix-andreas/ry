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
                 sort(getGenerics(asNamespace('shapes'))), '\\n')";
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
        let mut printed = Vec::new();
        for lazy in ["TRUE", "FALSE"] {
            let root = package(&files);
            let reload = format!(
                "options(ry.load.lazy_s4 = {lazy}); load_all({:?}, full = TRUE)",
                root.path().display().to_string()
            );
            let output = ry_load(root.path(), &[&reload, probe]);
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
        let (lazy, eager) = (&printed[0], &printed[1]);
        assert_eq!(lazy.1, eager.1, "lazy:\n{}\neager:\n{}", lazy.0, eager.0);
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
            "first <- function() 1\n\n\nlater <- function() {\n  2\n}\n",
        ),
    ]);
    let script = format!(
        "options(keep.source = TRUE)\nload_all({root:?}, full = TRUE)\n\
         cat('at', utils::getSrcLocation(later), basename(utils::getSrcFilename(later)), '\\n')\n",
        root = root.path().display().to_string(),
    );
    // ry's interpreter keeps no source references yet.
    let output = ry_load_on("gnu-r", root.path(), &[&script]);
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("at 4 a.R"), "{printed}");
}
