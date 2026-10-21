//! Embeds `ry.exe.manifest` into Windows binaries so the process runs
//! with UTF-8 as its active code page, because embedded R takes its native
//! encoding from the host process's code page (the manifest explains the
//! full why). Done through MSVC linker flags to avoid a build dependency;
//! the GNU Windows target is only ever compile-checked, and `cargo check`
//! never links.

fn main() {
    println!("cargo:rerun-if-changed=ry.exe.manifest");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("windows-msvc") {
        let manifest = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
        )
        .join("ry.exe.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
