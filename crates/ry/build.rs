//! Embeds `ry.exe.manifest` so Windows runs ry with a UTF-8 code page, which embedded R
//! inherits as its native encoding. MSVC linker flags avoid a build dependency; the GNU
//! Windows target is only compile-checked, and `cargo check` never links.

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
