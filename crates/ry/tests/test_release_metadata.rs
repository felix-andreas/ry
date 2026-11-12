//! The VS Code extension manifest version must equal the workspace version minus
//! any prerelease suffix (VS Code requires plain `major.minor.patch`). Zed is
//! absent: its extension only locates an installed binary.
//!
//! The benchmark runner records the release archive it measured, so its tests run here too.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the repository root")
        .to_owned()
}

fn read(relative: &str) -> String {
    let path = repository_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

fn workspace_version() -> String {
    let manifest: toml::Value = read("Cargo.toml").parse().expect("the workspace manifest");
    manifest["workspace"]["package"]["version"]
        .as_str()
        .expect("workspace.package.version is a string")
        .to_owned()
}

#[test]
fn the_vs_code_extension_carries_the_release_without_its_prerelease_suffix() {
    let version = workspace_version();
    let expected = version
        .split_once('-')
        .map_or(&version[..], |(release, _)| release);
    let manifest: serde_json::Value =
        serde_json::from_str(&read("editors/code/package.json")).expect("the VS Code manifest");
    let found = manifest["version"].as_str().expect("version is a string");
    assert_eq!(
        found, expected,
        "editors/code/package.json is stale: write `\"version\": \"{expected}\"` \
         (the workspace is at {version}; this manifest drops the prerelease suffix)"
    );
}

/// `tools/bench/bench.py --release` records the archive's tag and sha256 and refuses any image the
/// archive's own runtime did not write; `tools/bench/test_bench.py` pins both on a fake archive.
#[cfg(unix)]
#[test]
fn the_benchmark_runner_records_the_release_archive_and_refuses_foreign_images() {
    let output = Command::new("python3")
        .arg("-I")
        .arg(repository_root().join("tools/bench/test_bench.py"))
        .output()
        .expect("python3 runs tools/bench/test_bench.py");
    assert!(
        output.status.success(),
        "tools/bench/test_bench.py failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
