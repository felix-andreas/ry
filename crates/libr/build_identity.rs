//! The runtime identity, computed the same way by the build scripts of `libr` (which exports it
//! from `libR.so`) and of `repl` (which compares it after loading the library).
//!
//! The identity is a hash of the source files of `libr` and of every workspace crate it links,
//! found by following the `path` dependencies in each `Cargo.toml`. Hashing contents instead of
//! timestamps or a git commit means a rebuild that changes nothing keeps the identity, and an
//! `ry` and a `libR.so` built from the same tree always agree, with or without git. `Cargo.toml`
//! files are left out on purpose: adding a test target must not look like a runtime change.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub struct Identity {
    pub hash: String,
    /// What cargo must watch so the build script reruns when the identity can change.
    pub watched: Vec<PathBuf>,
}

pub fn runtime_identity(crates_directory: &Path) -> Identity {
    let mut crates = BTreeSet::from(["libr".to_owned()]);
    let mut pending = vec!["libr".to_owned()];
    while let Some(name) = pending.pop() {
        let manifest = crates_directory.join(&name).join("Cargo.toml");
        let text = fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", manifest.display()));
        for dependency in path_dependencies(&text) {
            if crates.insert(dependency.clone()) {
                pending.push(dependency);
            }
        }
    }

    let mut files = Vec::new();
    let mut watched = Vec::new();
    for name in &crates {
        let directory = crates_directory.join(name);
        for part in ["src", "include"] {
            let part_directory = directory.join(part);
            if part_directory.is_dir() {
                collect_files(&part_directory, &mut files);
                watched.push(part_directory);
            }
        }
        let build_script = directory.join("build.rs");
        if build_script.is_file() {
            files.push(build_script.clone());
            watched.push(build_script);
        }
    }
    files.sort();

    let mut hash = FNV_OFFSET;
    for file in &files {
        let relative = file.strip_prefix(crates_directory).unwrap_or(file);
        let bytes = fs::read(file)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()));
        hash = fnv1a(hash, relative.to_string_lossy().as_bytes());
        hash = fnv1a(hash, &(bytes.len() as u64).to_le_bytes());
        hash = fnv1a(hash, &bytes);
    }
    Identity {
        hash: format!("{hash:016x}"),
        watched,
    }
}

/// The `path = "../name"` dependencies in the `[dependencies]`, `[build-dependencies]` and
/// target-specific tables. Dev-dependencies are not linked into the library.
fn path_dependencies(manifest: &str) -> Vec<String> {
    let mut linked = false;
    let mut found = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            let header = header.trim_end_matches(']');
            linked = header.ends_with("dependencies") && !header.ends_with("dev-dependencies");
        } else if linked
            && let Some((_, rest)) = line.split_once("path = \"../")
            && let Some((name, _)) = rest.split_once('"')
        {
            found.push(name.to_owned());
        }
    }
    found
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
            .path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}
