//! Embeds the runtime identity that `repl` expects `libR.so` to export. The hashing is shared
//! with the `libr` build script, which stamps the library itself.

#[path = "../libr/build_identity.rs"]
mod build_identity;

use std::{env, path::PathBuf};

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
}
