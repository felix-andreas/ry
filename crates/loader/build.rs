//! Fingerprints the code a load index is derived from, so a load index cached by one build of
//! ry is never read by another: the parser, the analysis, the shipped stubs, and the loader.

use std::{
    env, fs,
    io::{Error, ErrorKind},
    path::{Path, PathBuf},
};

fn main() -> Result<(), Error> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(Error::other)?);
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "the loader is not in a workspace"))?;
    let mut hash = FNV_OFFSET;
    for directory in [
        "crates/syntax/src",
        "crates/semantics/src",
        "crates/loader/src",
        "types",
    ] {
        let directory = workspace.join(directory);
        println!("cargo:rerun-if-changed={}", directory.display());
        let mut files = Vec::new();
        collect(&directory, &mut files)?;
        files.sort();
        for file in files {
            hash = fnv1a(
                hash,
                file.strip_prefix(workspace)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .as_bytes(),
            );
            hash = fnv1a(hash, &fs::read(&file)?);
        }
    }
    hash = fnv1a(
        hash,
        env::var("CARGO_PKG_VERSION")
            .map_err(Error::other)?
            .as_bytes(),
    );
    println!("cargo:rustc-env=RY_LOAD_INDEX_BUILD={hash:016x}");
    Ok(())
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}
