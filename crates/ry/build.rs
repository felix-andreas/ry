//! Windows link settings for `ry.exe`. The release builds embed `ry.exe.manifest`, so Windows runs
//! ry with a UTF-8 code page, which embedded R inherits as its native encoding: MSVC through linker
//! flags, gnullvm through a resource object that `windres` compiles. Every Windows target reserves a
//! 64 MiB main-thread stack, because R sessions run on `main` and Windows' default of 1 MiB
//! overflows while the default packages load (Linux and macOS give `main` 8 MiB).

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const STACK_SIZE: usize = 64 * 1024 * 1024;

/// `WINDRES` names the tool explicitly; otherwise the first of these on `PATH` that runs. MSYS2's
/// clang64 environment and llvm-mingw ship `llvm-windres` and `windres`; Debian's mingw-w64 ships the
/// prefixed GNU one.
const WINDRES_CANDIDATES: [&str; 3] = ["llvm-windres", "windres", "x86_64-w64-mingw32-windres"];

fn main() {
    println!("cargo:rerun-if-changed=ry.exe.manifest");
    println!("cargo:rerun-if-env-changed=WINDRES");
    let target = env::var("TARGET").unwrap_or_default();
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let manifest = manifest_dir.join("ry.exe.manifest");
    if target.ends_with("windows-gnu") || target.ends_with("windows-gnullvm") {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_SIZE}");
    }
    if target.ends_with("windows-gnullvm") {
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
        let object = compile_manifest_resource(&manifest, &out_dir);
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

/// Compiles a resource script that embeds the manifest as resource 1 of type 24 (`RT_MANIFEST`),
/// the slot Windows reads an executable's manifest from. The script and a copy of the manifest sit
/// side by side in `OUT_DIR`, so the script names the manifest by a relative path that no quoting
/// rule of a Windows path can break.
fn compile_manifest_resource(manifest: &Path, out_dir: &Path) -> PathBuf {
    let copy = out_dir.join("ry.exe.manifest");
    let script = out_dir.join("ry-manifest.rc");
    let object = out_dir.join("ry-manifest.o");
    fs::copy(manifest, &copy).expect("copying ry.exe.manifest into OUT_DIR");
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
