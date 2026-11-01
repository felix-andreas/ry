//! Embeds every shipped stub (`types/*.Rtypes`) and export manifest (`types/*.exports`), so adding
//! a package's types is a matter of dropping its files into `types/`.

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{Error, ErrorKind},
    path::{Path, PathBuf},
};

/// R's own packages, in the order their stubs load, so later sources override earlier ones the way
/// R's search path does. They are always present, which keeps them unconditional.
const R_PACKAGES: &[&str] = &[
    "base",
    "stats",
    "utils",
    "methods",
    "graphics",
    "grDevices",
    "datasets",
    "tools",
    "parallel",
    "compiler",
    "grid",
    "splines",
    "stats4",
    "tcltk",
];

/// The standard library loads last, and like R's packages it is always present.
const STANDARD_LIBRARY: &str = "ry";

fn main() -> Result<(), Error> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(env_error)?);
    let types = manifest_dir.join("../../types");
    println!("cargo:rerun-if-changed={}", types.display());
    let mut stubs = Vec::new();
    let mut manifests = Vec::new();
    for entry in fs::read_dir(&types)? {
        let path = entry?.path();
        println!("cargo:rerun-if-changed={}", path.display());
        let (Some(stem), Some(extension)) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            path.extension().and_then(|extension| extension.to_str()),
        ) else {
            continue;
        };
        match extension {
            "Rtypes" => stubs.push((stem.to_owned(), path.clone())),
            "exports" => manifests.push((stem.to_owned(), path.clone())),
            _ => {}
        }
    }
    stubs.sort_by_key(|(namespace, _)| load_rank(namespace));
    manifests.sort_by_key(|(namespace, _)| load_rank(namespace));

    let mut conditional: Vec<&str> = stubs
        .iter()
        .chain(&manifests)
        .map(|(namespace, _)| namespace.as_str())
        .filter(|namespace| *namespace != STANDARD_LIBRARY && !R_PACKAGES.contains(namespace))
        .collect();
    conditional.sort_unstable();
    conditional.dedup();

    let mut out = String::new();
    write_table(&mut out, "SHIPPED_STUBS", &stubs)?;
    write_table(&mut out, "SHIPPED_MANIFESTS", &manifests)?;
    out.push_str("pub const CONDITIONAL_NAMESPACES: &[&str] = &[\n");
    for namespace in conditional {
        writeln!(out, "    {namespace:?},").map_err(format_error)?;
    }
    out.push_str("];\n");
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(env_error)?);
    fs::write(out_dir.join("shipped.rs"), out)
}

fn load_rank(namespace: &str) -> (usize, String) {
    let position = R_PACKAGES.iter().position(|package| *package == namespace);
    match position {
        Some(position) => (position, String::new()),
        None if namespace == STANDARD_LIBRARY => (R_PACKAGES.len() + 1, String::new()),
        None => (R_PACKAGES.len(), namespace.to_owned()),
    }
}

fn write_table(out: &mut String, name: &str, files: &[(String, PathBuf)]) -> Result<(), Error> {
    writeln!(out, "const {name}: &[(&str, &str)] = &[").map_err(format_error)?;
    for (namespace, path) in files {
        let path = canonical(path)?;
        writeln!(out, "    ({namespace:?}, include_str!({path:?})),").map_err(format_error)?;
    }
    out.push_str("];\n");
    Ok(())
}

fn canonical(path: &Path) -> Result<String, Error> {
    let path = fs::canonicalize(path)?;
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "non-UTF-8 path under types/"))
}

fn env_error(error: env::VarError) -> Error {
    Error::new(ErrorKind::NotFound, error)
}

fn format_error(error: std::fmt::Error) -> Error {
    Error::other(error)
}
