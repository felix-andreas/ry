//! Builds the C side of the native boundary and exports its symbols from the shared library.
//!
//! rustc gives a cdylib a version script that exports only Rust-defined symbols, so the C
//! functions native code calls (the variadic `Rf_error` family, `R_UnwindProtect` and the other
//! context functions) would stay hidden. A second version script lists them; the linker merges
//! the two. The list comes from the `RY_EXPORT` markers in `src/native.c`, so it cannot drift.

#[path = "build_identity.rs"]
mod build_identity;

use std::{env, fs, path::PathBuf};

fn main() {
    emit_runtime_identity();
    println!("cargo:rerun-if-changed=build_identity.rs");
    let source = "src/native.c";
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-changed=include");
    cc::Build::new()
        .file(source)
        .include("include")
        .flag_if_supported("-fexceptions")
        .flag_if_supported("-Wno-unused-parameter")
        // `DL_FUNC` is cast to the routine's real signature, as GNU R itself does.
        .flag_if_supported("-Wno-cast-function-type")
        .warnings(true)
        .compile("ry_native");

    let text =
        fs::read_to_string(source).unwrap_or_else(|error| panic!("cannot read {source}: {error}"));
    // `RY_EXPORT` marks a function (the name precedes the first parenthesis) and
    // `RY_EXPORT_DATA` a variable (the name precedes the initializer or the semicolon, or an
    // array bound).
    let functions = text
        .lines()
        .filter_map(|line| line.strip_prefix("RY_EXPORT "))
        .filter_map(|declaration| {
            let name_end = declaration.find('(')?;
            let name = declaration[..name_end].trim().rsplit([' ', '*']).next()?;
            Some(name.to_owned())
        });
    let variables = text
        .lines()
        .filter_map(|line| line.strip_prefix("RY_EXPORT_DATA "))
        .filter_map(|declaration| {
            // A function pointer names itself inside `(*name)`.
            if let Some(start) = declaration.find("(*") {
                let rest = &declaration[start + 2..];
                return Some(rest[..rest.find(')')?].trim().to_owned());
            }
            let name_end = declaration.find(['=', ';', '['])?;
            let name = declaration[..name_end].trim().rsplit([' ', '*']).next()?;
            Some(name.to_owned())
        });
    let exported: Vec<String> = functions.chain(variables).collect();
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").unwrap_or_else(|error| panic!("OUT_DIR: {error}")));
    let script = out_dir.join("exports.map");
    let body: String = exported
        .iter()
        .map(|name| format!("{name};"))
        .collect::<Vec<_>>()
        .join(" ");
    fs::write(&script, format!("{{ global: {body} }};\n"))
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", script.display()));

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,--version-script={}",
            script.display()
        );
        // Packages built against GNU R record `libR.so` as a needed library. With this soname, the
        // dynamic linker satisfies that from ry's already loaded libR instead of searching for
        // GNU R's.
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libR.so");
        // GNU R's libR depends on zlib, bzip2 and xz, and package binaries that call zlib
        // without linking it themselves (gert, httpuv) find it through that dependency. ry's
        // libR records the same dependencies, which the linker would otherwise drop as unused.
        for library in ["z", "bz2", "lzma"] {
            println!("cargo:rustc-cdylib-link-arg=-Wl,--no-as-needed,-l{library},--as-needed");
        }
    }
}

/// `ry` compares this value with its own after loading the library, so a `libR.so` left over
/// from older sources is refused instead of silently running them.
fn emit_runtime_identity() {
    let manifest = env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|error| panic!("CARGO_MANIFEST_DIR: {error}"));
    let crates_directory = PathBuf::from(manifest).join("..");
    let identity = build_identity::runtime_identity(&crates_directory);
    for path in &identity.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=RY_RUNTIME_ID={}", identity.hash);
}
