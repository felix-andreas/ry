//! Embeds the runtime identity that `repl` expects `libR.so` to export. The hashing is shared
//! with the `libr` build script, which stamps the library itself.
//!
//! With `RY_EMBED_RUNTIME` naming a built runtime (`libR.so`, `libR.dylib` or `R.dll`), it also
//! embeds that library, compressed, and on Windows the import library `libR.dll.a` beside it, so
//! the `ry` that links this crate runs without any file beside it (`repl::ry_runtime_library`).
//!
//! With `RY_EMBED_BASE` naming ry's built base R tree (`rcmd::base`), it embeds that tree as one
//! zstd-compressed archive, which `repl::bundled_base` extracts on first use.

#[path = "../libr/build_identity.rs"]
mod build_identity;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|error| panic!("CARGO_MANIFEST_DIR: {error}"));
    let crates_directory = PathBuf::from(manifest).join("..");
    let identity = build_identity::runtime_identity(&crates_directory);
    println!("cargo:rerun-if-changed=../libr/build_identity.rs");
    for path in &identity.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=RY_RUNTIME_ID={}", identity.hash);
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").unwrap_or_else(|error| panic!("OUT_DIR: {error}")));
    embed_runtime(&out_dir);
    embed_base(&out_dir);
}

/// Writes `embedded_base.rs`, which defines `EMBEDDED_BASE`: `None`, or the base R tree that
/// `RY_EMBED_BASE` names as an archive `repl::extract_base` reads. The archive is a sequence of
/// entries in path order, each a kind byte (0 for a directory, 1 for a file, 2 for an executable
/// file), the path's length as a little-endian `u32`, the path with `/` separators, and for a file
/// its length as a little-endian `u64` and its bytes. It is compressed as a whole, which shrinks
/// the many similar help and lazy-load files far better than compressing each.
fn embed_base(out_dir: &Path) {
    println!("cargo:rerun-if-env-changed=RY_EMBED_BASE");
    let source = match env::var_os("RY_EMBED_BASE").filter(|path| !path.is_empty()) {
        None => "pub const EMBEDDED_BASE: Option<EmbeddedBase> = None;\n".to_owned(),
        Some(root) => {
            let root = PathBuf::from(root);
            let manifest = root.join("ry-base.txt");
            if !manifest.is_file() {
                panic!(
                    "RY_EMBED_BASE: {} is not a complete base R build (no ry-base.txt)",
                    root.display()
                );
            }
            println!("cargo:rerun-if-changed={}", manifest.display());
            // `ry-rcmd start-image` rewrites the start-up image without touching the manifest.
            println!(
                "cargo:rerun-if-changed={}",
                root.join("library/base/R/ry-start.image").display()
            );
            let mut archive = Vec::new();
            append_tree(&root, "", &mut archive);
            let level = if env::var("PROFILE").is_ok_and(|profile| profile == "release") {
                19
            } else {
                3
            };
            let compressed = zstd::encode_all(archive.as_slice(), level)
                .unwrap_or_else(|error| panic!("compressing the base R archive: {error}"));
            let mut key: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in &compressed {
                key = (key ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
            }
            let destination = out_dir.join("base-r.zst");
            fs::write(&destination, &compressed)
                .unwrap_or_else(|error| panic!("writing {}: {error}", destination.display()));
            format!(
                "pub const EMBEDDED_BASE: Option<EmbeddedBase> = Some(EmbeddedBase {{ key: \"{key:016x}\", size: {}, compressed: include_bytes!({destination:?}) }});\n",
                archive.len()
            )
        }
    };
    let path = out_dir.join("embedded_base.rs");
    fs::write(&path, source).unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
}

fn append_tree(directory: &Path, prefix: &str, archive: &mut Vec<u8>) {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()))
                .path()
        })
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} has a name that is not UTF-8", path.display()));
        let relative = format!("{prefix}{name}");
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let push_path = |archive: &mut Vec<u8>, kind: u8| {
            archive.push(kind);
            let length = u32::try_from(relative.len())
                .unwrap_or_else(|_| panic!("{relative} is too long a path"));
            archive.extend_from_slice(&length.to_le_bytes());
            archive.extend_from_slice(relative.as_bytes());
        };
        if metadata.is_dir() {
            push_path(archive, 0);
            append_tree(&path, &format!("{relative}/"), archive);
        } else if metadata.is_file() {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            push_path(archive, if executable { 2 } else { 1 });
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            archive.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            archive.extend_from_slice(&bytes);
        } else {
            panic!(
                "RY_EMBED_BASE: {} is neither a file nor a directory",
                path.display()
            );
        }
    }
}

/// Writes `embedded_runtime.rs`, which defines `EMBEDDED_RUNTIME`: `None`, or the runtime
/// `RY_EMBED_RUNTIME` names with the files it needs, each zstd-compressed, keyed by a hash of
/// their contents so that every build extracts into a directory of its own.
fn embed_runtime(out_dir: &Path) {
    println!("cargo:rerun-if-env-changed=RY_EMBED_RUNTIME");
    let source = match env::var_os("RY_EMBED_RUNTIME").filter(|path| !path.is_empty()) {
        None => "pub const EMBEDDED_RUNTIME: Option<EmbeddedRuntime> = None;\n".to_owned(),
        Some(library) => {
            let library = PathBuf::from(library);
            // Debug runtimes are about a gigabyte; release builds are worth the slower level.
            let level = if env::var("PROFILE").is_ok_and(|profile| profile == "release") {
                12
            } else {
                1
            };
            let mut files = vec![library.clone()];
            let import_library = library.with_file_name("libR.dll.a");
            if import_library.is_file() {
                files.push(import_library);
            }
            let mut key: u64 = 0xcbf2_9ce4_8422_2325;
            let mut entries = String::new();
            for path in &files {
                println!("cargo:rerun-if-changed={}", path.display());
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_else(|| {
                        panic!("RY_EMBED_RUNTIME: {} has no file name", path.display())
                    });
                let bytes = fs::read(path).unwrap_or_else(|error| {
                    panic!("RY_EMBED_RUNTIME: reading {}: {error}", path.display())
                });
                let compressed = zstd::encode_all(bytes.as_slice(), level)
                    .unwrap_or_else(|error| panic!("compressing {}: {error}", path.display()));
                for byte in name.bytes().chain(compressed.iter().copied()) {
                    key = (key ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
                }
                let destination = out_dir.join(format!("{name}.zst"));
                fs::write(&destination, &compressed)
                    .unwrap_or_else(|error| panic!("writing {}: {error}", destination.display()));
                entries.push_str(&format!(
                    "        EmbeddedFile {{ name: {name:?}, size: {}, compressed: include_bytes!({:?}) }},\n",
                    bytes.len(),
                    destination
                ));
            }
            format!(
                "pub const EMBEDDED_RUNTIME: Option<EmbeddedRuntime> = Some(EmbeddedRuntime {{\n    key: \"{key:016x}\",\n    files: &[\n{entries}    ],\n}});\n"
            )
        }
    };
    let path = out_dir.join("embedded_runtime.rs");
    fs::write(&path, source).unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
}
