//! Packages install into ry's runtime with `R CMD INSTALL` semantics: ry's R home answers
//! `R CMD config`, a source package compiles with its `Makevars` against ry's headers and
//! `libR`, the R-level half (lazy-load database, `Meta/`, help, lazy data) runs on ry's
//! interpreter, and the installed package loads under ry. `bin/R` and `bin/Rscript` run R code
//! on ry. With `RY_SOURCE_PACKAGES` set to a directory of source tarballs, every one is
//! installed that way and the outcome is reported in `target/tmp/source-packages.tsv`.

mod harness;

use {
    harness::*,
    std::{
        path::{Path, PathBuf},
        process::Command,
        sync::OnceLock,
    },
};

/// ry's R home, assembled once per test binary from this build's libR and headers.
fn home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ry-home");
        let libr = library_path();
        // `ry-rcmd` is another package's binary, so it is built here rather than by cargo test,
        // in the profile directory the test itself runs from.
        let profile_directory = profile_directory();
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "-q", "-p", "rcmd", "--bin", "ry-rcmd"]);
        if profile_directory
            .file_name()
            .is_some_and(|name| name == "release")
        {
            build.arg("--release");
        }
        assert!(
            build.status().expect("cargo build").success(),
            "building ry-rcmd failed"
        );
        let rcmd = profile_directory.join("ry-rcmd");
        rcmd::assemble(
            &root,
            &rcmd::HomeSources {
                include: &Path::new(env!("CARGO_MANIFEST_DIR")).join("include"),
                libr: &libr,
                rcmd: &rcmd,
                gnu_r_home: &gnu_r_home(),
                toolchain: rcmd::Toolchain::default(),
            },
        )
        .expect("assemble the R home");
        root
    })
}

/// The GNU R installation whose base packages ry's R home links: `R RHOME`.
fn gnu_r_home() -> PathBuf {
    let output = Command::new("R")
        .arg("RHOME")
        .output()
        .expect("GNU R on PATH");
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

fn no_test_load() -> rcmd::InstallOptions {
    rcmd::InstallOptions {
        test_load: false,
        ..rcmd::InstallOptions::default()
    }
}

/// Runs `bin/<program>` of ry's R home and returns its stdout, asserting success.
fn run_home(program: &str, arguments: &[&str], directory: &Path) -> String {
    let output = Command::new(home().join("bin").join(program))
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run the front end");
    assert!(
        output.status.success(),
        "{program} {arguments:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn r_cmd_config_reports_ry_s_toolchain() {
    let output = Command::new(home().join("bin/R"))
        .args(["CMD", "config", "CC"])
        .output()
        .expect("run bin/R");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "gcc");
    let include = rcmd::config(home(), "R_INCLUDE_DIR").expect("R_INCLUDE_DIR");
    assert_eq!(include, home().join("include").display().to_string());
    let blas = rcmd::config(home(), "BLAS_LIBS").expect("BLAS_LIBS");
    assert_eq!(blas, "-lblas");
}

/// rytest as a source package: DESCRIPTION, NAMESPACE and its src/.
fn rytest_source() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rytest-source");
    drop(std::fs::remove_dir_all(&root));
    std::fs::create_dir_all(root.join("src")).expect("a source directory");
    std::fs::write(
        root.join("DESCRIPTION"),
        "Package: rytest\nVersion: 0.1\nTitle: ry's native boundary tests\nLicense: UPL-1.0\n\
         NeedsCompilation: yes\n",
    )
    .expect("DESCRIPTION");
    std::fs::write(
        root.join("NAMESPACE"),
        "useDynLib(rytest, .registration = TRUE)\n",
    )
    .expect("NAMESPACE");
    for entry in std::fs::read_dir(package_sources("rytest")).expect("rytest sources") {
        let path = entry.expect("an entry").path();
        std::fs::copy(
            &path,
            root.join("src").join(path.file_name().expect("a name")),
        )
        .expect("copy a source");
    }
    root
}

#[test]
fn a_source_package_installs_and_runs_under_ry() {
    let library = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ry-library");
    let installed =
        rcmd::install(home(), &rytest_source(), &library, &no_test_load()).expect("install rytest");
    let description =
        std::fs::read_to_string(installed.directory.join("DESCRIPTION")).expect("DESCRIPTION");
    assert!(description.contains("RyBuilt: "), "{description}");
    assert!(description.contains("Built: R "), "{description}");
    assert!(
        description.contains("; x86_64-pc-linux-gnu;"),
        "{description}"
    );
    assert!(installed.directory.join("NAMESPACE").exists());
    assert!(installed.directory.join("Meta/package.rds").exists());
    assert!(installed.directory.join("Meta/nsInfo.rds").exists());
    let shared = installed.library.expect("rytest has compiled code");
    let r = r();
    r.load(&shared);
    let values = r.keep(unsafe { (r.api().Rf_allocVector)(REALSXP, 3) });
    unsafe { std::slice::from_raw_parts_mut((r.api().REAL)(values), 3) }
        .copy_from_slice(&[1.0, 2.0, 3.5]);
    let sum = r.call("rytest_sum", &[values]).expect("rytest_sum");
    assert_eq!(r.reals(sum), vec![6.5]);
}

#[test]
fn a_binary_package_from_an_older_r_installs_unchanged() {
    let source = Path::new(env!("CARGO_TARGET_TMPDIR")).join("binary-package");
    drop(std::fs::remove_dir_all(&source));
    std::fs::create_dir_all(source.join("R")).expect("a package directory");
    std::fs::write(
        source.join("DESCRIPTION"),
        "Package: oldbuild\nVersion: 1.0\nBuilt: R 4.3.3; ; 2024-03-01 00:00:00 UTC; unix\n",
    )
    .expect("DESCRIPTION");
    let library = Path::new(env!("CARGO_TARGET_TMPDIR")).join("binary-library");
    let installed =
        rcmd::install(home(), &source, &library, &no_test_load()).expect("install a binary");
    let description =
        std::fs::read_to_string(installed.directory.join("DESCRIPTION")).expect("DESCRIPTION");
    assert!(
        !description.contains("RyBuilt"),
        "a binary is copied as it is"
    );

    std::fs::write(
        source.join("DESCRIPTION"),
        "Package: ancient\nVersion: 1.0\nBuilt: R 2.15.3; ; 2013-03-01 00:00:00 UTC; unix\n",
    )
    .expect("DESCRIPTION");
    let error =
        rcmd::install(home(), &source, &library, &no_test_load()).expect_err("a pre-3.0.0 build");
    assert!(
        error.to_string().contains("built before R 3.0.0"),
        "{error}"
    );
}

/// A package with R code, an S4 class, documentation, lazy data, `sysdata.rda` and `inst/`
/// files, written out as a source directory.
fn rcode_source() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rcode-source");
    drop(std::fs::remove_dir_all(&root));
    for directory in ["R", "man", "data", "inst/extdata"] {
        std::fs::create_dir_all(root.join(directory)).expect("a source directory");
    }
    let files = [
        (
            "DESCRIPTION",
            "Package: rcode\nVersion: 0.2\nTitle: R code installed by ry\nDescription: Tests ry's installer.\n\
             License: UPL-1.0\nImports: methods\nLazyData: true\nCollate: 'b.R' 'a.R'\n",
        ),
        (
            "NAMESPACE",
            "export(greet, area, scaled)\nexportClasses(Square)\nimportFrom(methods, new, setClass, setGeneric, setMethod)\n\
             exportMethods(area)\n",
        ),
        // Collated after b.R, so it can use the class b.R defines.
        (
            "R/a.R",
            "greet <- function(name) paste0(prefix, \", \", name, \"!\")\n\
             setMethod(\"area\", \"Square\", function(shape) shape@side^2)\n\
             scaled <- function() factor * nrow(sizes)\n",
        ),
        (
            "R/b.R",
            "prefix <- \"Hello\"\nsetClass(\"Square\", representation(side = \"numeric\"))\n\
             setGeneric(\"area\", function(shape) standardGeneric(\"area\"))\n",
        ),
        (
            "man/greet.Rd",
            "\\name{greet}\n\\alias{greet}\n\\title{Greet someone}\n\\usage{greet(name)}\n\
             \\arguments{\\item{name}{who to greet}}\n\\value{A greeting.}\n\\description{Says hello.}\n",
        ),
        ("inst/extdata/notes.txt", "installed from inst/\n"),
    ];
    for (path, text) in files {
        std::fs::write(root.join(path), text).expect("a source file");
    }
    let r_home = gnu_r_home();
    let status = Command::new(r_home.join("bin/Rscript"))
        .args([
            "-e",
            "sizes <- data.frame(n = 1:3); save(sizes, file = 'data/sizes.rda'); \
             factor <- 10; save(factor, file = 'R/sysdata.rda')",
        ])
        .current_dir(&root)
        .status()
        .expect("GNU R writes the fixture's data files");
    assert!(status.success());
    root
}

#[test]
fn the_r_level_half_runs_on_ry() {
    let library = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rcode-library");
    drop(std::fs::remove_dir_all(&library));
    let installed = rcmd::install(
        home(),
        &rcode_source(),
        &library,
        &rcmd::InstallOptions::default(),
    )
    .expect("install rcode");
    let directory = &installed.directory;
    for file in [
        "Meta/package.rds",
        "Meta/nsInfo.rds",
        "Meta/Rd.rds",
        "Meta/hsearch.rds",
        "Meta/links.rds",
        "Meta/data.rds",
        "R/rcode",
        "R/rcode.rdb",
        "R/rcode.rdx",
        "R/sysdata.rdb",
        "R/sysdata.rdx",
        "data/Rdata.rdb",
        "data/Rdata.rdx",
        "help/rcode.rdb",
        "help/rcode.rdx",
        "help/AnIndex",
        "help/aliases.rds",
        "extdata/notes.txt",
    ] {
        assert!(directory.join(file).exists(), "{file} was not installed");
    }
    let description = std::fs::read_to_string(directory.join("DESCRIPTION")).expect("DESCRIPTION");
    assert!(description.contains("RyBuilt: "), "{description}");
    let script = format!(
        ".libPaths('{}')\nlibrary(rcode)\ncat(greet('ry'), area(new('Square', side = 3)), scaled(), '\\n')\n\
         cat(nrow(sizes), readLines(system.file('extdata', 'notes.txt', package = 'rcode')), '\\n')\n\
         cat(names(tools::Rd_db('rcode')), '\\n')\n",
        library.display()
    );
    let output = run_home("Rscript", &["-e", &script], &library);
    assert_eq!(
        output,
        "Hello, ry! 9 30 \n3 installed from inst/ \ngreet.Rd \n"
    );
}

#[test]
fn the_front_ends_run_r_code_on_ry() {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let output = run_home(
        "Rscript",
        &[
            "-e",
            "cat(R.home() == Sys.getenv('R_HOME'), commandArgs(TRUE))",
            "a",
            "b",
        ],
        directory,
    );
    assert_eq!(output, "TRUE a b");
    let output = run_home(
        "R",
        &[
            "--vanilla",
            "--no-echo",
            "-e",
            "cat(nzchar(R.home('share')))",
        ],
        directory,
    );
    assert_eq!(output, "TRUE");
    let script = directory.join("front-end.R");
    std::fs::write(&script, "cat(commandArgs(TRUE), sep = ',')\n").expect("a script");
    let output = run_home(
        "R",
        &[
            "--no-echo",
            "-f",
            &script.display().to_string(),
            "--args",
            "x",
            "y",
        ],
        directory,
    );
    assert_eq!(output, "x,y");
    // GNU R's own `R CMD` scripts call back into bin/R, so they run on ry too.
    let page = rcode_source().join("man/greet.Rd").display().to_string();
    let output = run_home("R", &["CMD", "Rdconv", "-t", "txt", &page], directory);
    let gnu_r = Command::new("R")
        .args(["CMD", "Rdconv", "-t", "txt", &page])
        .output()
        .expect("GNU R's Rdconv");
    assert_eq!(output, String::from_utf8_lossy(&gnu_r.stdout));
    assert!(output.contains("Says hello."), "{output}");
}

/// Builds every source tarball in `RY_SOURCE_PACKAGES` against ry's headers into
/// `target/tmp/source-library`, dependencies of `LinkingTo` resolving from `R_LIBS`.
#[test]
fn source_packages_build_against_ry_s_headers() {
    let Some(directory) = std::env::var_os("RY_SOURCE_PACKAGES") else {
        return;
    };
    let library = Path::new(env!("CARGO_TARGET_TMPDIR")).join("source-library");
    std::fs::create_dir_all(&library).expect("a library");
    let mut tarballs: Vec<PathBuf> = std::fs::read_dir(&directory)
        .expect("the tarball directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.to_string_lossy().ends_with(".tar.gz"))
        .collect();
    tarballs.sort();
    let mut report = String::from("tarball\toutcome\tdetail\n");
    let mut built = 0;
    for tarball in &tarballs {
        let name = tarball
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        match rcmd::install(home(), tarball, &library, &rcmd::InstallOptions::default()) {
            Ok(installed) => {
                built += 1;
                report.push_str(&format!(
                    "{name}\tbuilt\t{}\n",
                    installed
                        .library
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ));
            }
            Err(error) => {
                let detail: String = error
                    .to_string()
                    .lines()
                    .filter(|line| line.contains("error") || line.starts_with("ERROR"))
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" | ");
                report.push_str(&format!("{name}\tfailed\t{detail}\n"));
            }
        }
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("source-packages.tsv");
    std::fs::write(&path, report).expect("the report");
    eprintln!(
        "{built} of {} source packages built; report: {}",
        tarballs.len(),
        path.display()
    );
}
