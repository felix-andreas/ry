//! Packages install into ry's runtime with `R CMD INSTALL` semantics: ry's R home answers
//! `R CMD config`, a source package compiles with its `Makevars` against ry's headers and
//! `libR`, and the installed shared object loads under ry. With `RY_SOURCE_PACKAGES` set to a
//! directory of source tarballs, every one is built that way and the outcome is reported in
//! `target/tmp/source-packages.tsv`.

mod harness;

use harness::*;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

/// ry's R home, assembled once per test binary from this build's libR and headers.
fn home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ry-home");
        let deps = std::env::current_exe()
            .expect("the test executable")
            .parent()
            .expect("its directory")
            .to_path_buf();
        // `ry-rcmd` is another package's binary, so it is built here rather than by cargo test.
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "rcmd", "--bin", "ry-rcmd"])
            .status()
            .expect("cargo build");
        assert!(status.success(), "building ry-rcmd failed");
        let rcmd = deps
            .parent()
            .expect("the profile directory")
            .join("ry-rcmd");
        let gnu_r_bin =
            which("Rscript").and_then(|rscript| rscript.parent().map(Path::to_path_buf));
        rcmd::assemble(
            &root,
            &rcmd::HomeSources {
                include: &Path::new(env!("CARGO_MANIFEST_DIR")).join("include"),
                libr: &deps.join("libR.so"),
                rcmd: &rcmd,
                gnu_r_bin: gnu_r_bin.as_deref(),
                toolchain: rcmd::Toolchain::default(),
            },
        )
        .expect("assemble the R home");
        root
    })
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
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
    let installed = rcmd::install(home(), &rytest_source(), &library).expect("install rytest");
    let description =
        std::fs::read_to_string(installed.directory.join("DESCRIPTION")).expect("DESCRIPTION");
    assert!(description.contains("RyBuilt: "), "{description}");
    assert!(
        description.contains("Built: R 4.6.2; x86_64-pc-linux-gnu;"),
        "{description}"
    );
    assert!(installed.directory.join("NAMESPACE").exists());
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
    let installed = rcmd::install(home(), &source, &library).expect("install a binary");
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
    let error = rcmd::install(home(), &source, &library).expect_err("a pre-3.0.0 build");
    assert!(
        error.to_string().contains("built before R 3.0.0"),
        "{error}"
    );
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
        match rcmd::install(home(), tarball, &library) {
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
