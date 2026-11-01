//! Windows link settings for `ry.exe`. The MSVC build embeds `ry.exe.manifest`, so Windows runs
//! ry with a UTF-8 code page, which embedded R inherits as its native encoding; linker flags
//! avoid a build dependency. Both Windows targets reserve a 64 MiB main-thread stack, because R
//! sessions run on `main` and Windows' default of 1 MiB overflows while the default packages
//! load (Linux and macOS give `main` 8 MiB).

use std::env;

const STACK_SIZE: usize = 64 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=ry.exe.manifest");
    let target = env::var("TARGET").unwrap_or_default();
    if target.ends_with("windows-gnu") || target.ends_with("windows-gnullvm") {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_SIZE}");
    }
    if target.ends_with("windows-msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:{STACK_SIZE}");
        let manifest = std::path::PathBuf::from(
            env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
        )
        .join("ry.exe.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
