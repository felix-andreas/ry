//! Embeds the runtime identity that `repl` expects `libR.so` to export. The hashing is shared
//! with the `libr` build script, which stamps the library itself.
//!
//! With `RY_EMBED_RUNTIME` naming a built runtime (`libR.so`, `libR.dylib` or `R.dll`), it also
//! embeds that library, compressed, and on Windows the import library `libR.dll.a` beside it, so
//! the `ry` that links this crate runs without any file beside it (`repl::ry_runtime_library`).

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
