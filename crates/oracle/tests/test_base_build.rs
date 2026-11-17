//! ry builds its own base R: the base packages of GNU R's pinned source release, installed by
//! `ry-rcmd build-base` on this checkout's runtime (`tools/base-r/build.sh`).
//!
//! The other suites cannot stand in for this one. They run on a base R that is already built,
//! often GNU R's own, and building runs code no session runs afterwards: the methods package
//! defines its basic classes and their relations once, at installation, and saves the result.
//!
//! The test builds `ry`, `libR.so` and `ry-rcmd` first unless `ORACLE_RY` names another build. It
//! skips without the source tarball in the download cache (`tools/base-r/build.sh --fetch`, which
//! `just gate` and `just base-r` run).

#![cfg(unix)]

use {
    oracle::{ry_program, target_directory},
    std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    },
    syntax::testing::cargo_build,
};

/// S4 dispatch, a class over a basic class, and the default packages, on the base R just built.
const SESSION: &str = r#"
library(methods)
setClass("Point", representation(x = "numeric"))
setGeneric("size", function(object) standardGeneric("size"))
setMethod("size", "Point", function(object) abs(object@x))
cat(size(new("Point", x = -3)), is(quote(a), "language"), extends("name", "language"), "\n")
setClass("Wrapper", contains = "name")
cat(is(new("Wrapper", as.name("y")), "language"), "\n")
for (package in c("stats", "utils", "graphics", "grDevices", "datasets", "tools", "compiler",
                  "parallel", "splines", "stats4", "grid"))
    if (!requireNamespace(package, quietly = TRUE)) cat("missing:", package, "\n")
cat(stats::median(c(1, 5, 2)), nrow(datasets::iris), "\n")
"#;

const EXPECTED: &str = "[1] \"size\"\n3 TRUE TRUE \nTRUE \n2 150 \n";

#[test]
fn base_packages_build_on_this_runtime() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let version = include_str!("../../../tools/base-r/VERSION").trim();
    // Where `build.sh` keeps its downloads; `just` resolves a relative cache from the root.
    let cache = match std::env::var_os("R_SOURCE_CACHE") {
        Some(cache) => workspace.join(cache),
        None => workspace.join("target/base-r-source"),
    };
    let tarball = cache.join(format!("R-{version}.tar.gz"));
    if !tarball.is_file() {
        eprintln!(
            "skipped: {} is not in the download cache; fetch it with `tools/base-r/build.sh --fetch`",
            tarball.display()
        );
        return;
    }
    if std::env::var_os("ORACLE_RY").is_none() {
        let built = cargo_build(&target_directory().join("debug"))
            .args(["-p", "ry-lang", "-p", "libr", "-p", "rcmd"])
            .current_dir(&workspace)
            .status()
            .expect("cargo runs");
        assert!(
            built.success(),
            "cargo build -p ry-lang -p libr -p rcmd failed"
        );
    }
    let ry = PathBuf::from(ry_program());
    let directory = ry.parent().unwrap_or(Path::new("."));
    let rcmd = directory.join("ry-rcmd");
    let libr = match std::env::var_os("RY_LIBR") {
        Some(library) => PathBuf::from(library),
        None => directory.join("libR.so"),
    };
    assert!(
        ry.is_file() && rcmd.is_file() && libr.is_file(),
        "{} holds no built runtime (ry, ry-rcmd and libR.so)",
        directory.display()
    );
    let scratch = std::env::temp_dir().join(format!("ry-base-build-{}", std::process::id()));
    let output = scratch.join("base-r");
    fs::create_dir_all(&scratch).expect("the scratch directory is created");
    let build = Command::new(workspace.join("tools/base-r/build.sh"))
        .args([&output, &rcmd, &libr])
        .env("R_SOURCE_TARBALL", &tarball)
        .env("R_BASE_PACKAGES_ONLY", "1")
        .env_remove("R_HOME")
        .output()
        .expect("tools/base-r/build.sh runs");
    // A failed build names the partial tree it keeps under `scratch`.
    assert!(
        build.status.success(),
        "the base packages do not build on this runtime ({}):\n{}{}",
        build.status,
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let script = scratch.join("session.R");
    fs::write(&script, SESSION).expect("the session script is written");
    let session = Command::new(&ry)
        .args(["run", "--runtime", "ry"])
        .arg(&script)
        .env("R_HOME", &output)
        .env("LC_ALL", "C.UTF-8")
        .output()
        .expect("ry runs");
    let printed = String::from_utf8_lossy(&session.stdout);
    fs::remove_dir_all(&scratch).expect("the scratch directory is removed");
    assert!(
        session.status.success() && printed == EXPECTED,
        "a session on the base R just built went wrong ({}):\n{printed}{}",
        session.status,
        String::from_utf8_lossy(&session.stderr)
    );
}
