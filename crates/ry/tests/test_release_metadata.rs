//! The VS Code extension manifest version must equal the workspace version minus
//! any prerelease suffix (VS Code requires plain `major.minor.patch`). Zed is
//! absent: its extension only locates an installed binary.

use std::fs;
use std::path::{Path, PathBuf};

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
