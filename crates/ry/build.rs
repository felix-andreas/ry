//! Windows link settings for `ry.exe`, and for `ry-rcmd`, whose package names this script as its
//! build script too, because its copies run as an R home's `R.exe` and `Rscript.exe`. The builds
//! embed `ry.exe.manifest`, so Windows runs them with a UTF-8 code page, which embedded R inherits as its native encoding: MSVC through linker
//! flags, gnullvm through a resource object that `windres` compiles. Every Windows target reserves a
//! 64 MiB main-thread stack, because R sessions run on `main` and Windows' default of 1 MiB
//! overflows while the default packages load (Linux and macOS give `main` 8 MiB). For `rcmd` it
//! also embeds ry's C headers, from which ry assembles its R home at run time.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const STACK_SIZE: usize = 64 * 1024 * 1024;

/// Included rather than read from the package directory, so the script finds the manifest from
/// either package, and a change to it rebuilds the script.
const MANIFEST: &str = include_str!("ry.exe.manifest");

/// `WINDRES` names the tool explicitly; otherwise the first of these on `PATH` that runs. MSYS2's
/// clang64 environment and llvm-mingw ship `llvm-windres` and `windres`; Debian's mingw-w64 ships the
/// prefixed GNU one.
const WINDRES_CANDIDATES: [&str; 3] = ["llvm-windres", "windres", "x86_64-w64-mingw32-windres"];

fn main() {
    // `RY_BUILD` names a nightly build (`just nightly-name`) and is appended to `ry --version`.
    println!("cargo:rerun-if-env-changed=RY_BUILD");
    let version = env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    match env::var("RY_BUILD") {
        Ok(build) if !build.is_empty() => println!("cargo:rustc-env=RY_VERSION={version} ({build})"),
        _ => println!("cargo:rustc-env=RY_VERSION={version}"),
    }
    println!("cargo:rerun-if-env-changed=WINDRES");
    let target = env::var("TARGET").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    if env::var("CARGO_PKG_NAME").is_ok_and(|name| name == "rcmd") {
        embed_headers(&out_dir);
    }
    // The script and a copy of the manifest sit side by side in `OUT_DIR`, so the resource
    // script names the manifest by a relative path that no quoting rule of a Windows path can
    // break.
    let manifest = out_dir.join("ry.exe.manifest");
    if target.contains("windows") {
        fs::write(&manifest, MANIFEST).expect("writing ry.exe.manifest into OUT_DIR");
    }
    if target.ends_with("windows-gnu") || target.ends_with("windows-gnullvm") {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_SIZE}");
    }
    if target.ends_with("windows-gnullvm") {
        let object = compile_manifest_resource(&out_dir);
        println!("cargo:rustc-link-arg-bins={}", object.display());
    }
    if target.ends_with("windows-msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:{STACK_SIZE}");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}

/// Writes `headers.rs`, which embeds ry's C headers (`crates/libr/include`) as `HEADERS`, so
/// ry can assemble an R home wherever it runs.
fn embed_headers(out_dir: &Path) {
    let include =
        Path::new(&env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
            .join("../libr/include");
    println!("cargo:rerun-if-changed={}", include.display());
    let mut files = Vec::new();
    let mut pending = vec![include.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("reading crates/libr/include") {
            let path = entry.expect("reading crates/libr/include").path();
            if path.is_dir() {
                println!("cargo:rerun-if-changed={}", path.display());
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut source = String::from("pub const HEADERS: &[(&str, &[u8])] = &[\n");
    for path in files {
        let relative = path
            .strip_prefix(&include)
            .expect("a header under include/");
        let relative = relative.to_string_lossy().replace('\\', "/");
        let absolute = fs::canonicalize(&path).expect("canonicalizing a header path");
        source.push_str(&format!(
            "    ({relative:?}, include_bytes!({:?})),\n",
            absolute
        ));
    }
    source.push_str("];\n");
    fs::write(out_dir.join("headers.rs"), source).expect("writing headers.rs into OUT_DIR");
}

/// Compiles a resource script that embeds the manifest as resource 1 of type 24 (`RT_MANIFEST`),
/// the slot Windows reads an executable's manifest from.
fn compile_manifest_resource(out_dir: &Path) -> PathBuf {
    let script = out_dir.join("ry-manifest.rc");
    let object = out_dir.join("ry-manifest.o");
    fs::write(&script, "1 24 \"ry.exe.manifest\"\n").expect("writing the manifest resource script");
    let candidates: Vec<String> = match env::var("WINDRES") {
        Ok(tool) => vec![tool],
        Err(_) => WINDRES_CANDIDATES
            .iter()
            .map(|tool| tool.to_string())
            .collect(),
    };
    for tool in &candidates {
        let status = Command::new(tool)
            .current_dir(out_dir)
            .args([
                "--target=pe-x86-64",
                "-i",
                "ry-manifest.rc",
                "-o",
                "ry-manifest.o",
            ])
            .status();
        match status {
            Ok(status) if status.success() => return object,
            Ok(status) => panic!("{tool} failed to compile the manifest resource: {status}"),
            Err(_) => continue,
        }
    }
    panic!(
        "building ry.exe for gnullvm needs windres to embed its UTF-8 manifest; none of {} runs (set WINDRES)",
        candidates.join(", ")
    );
}
