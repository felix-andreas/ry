//! Checks ry's public C headers the way packages consume them: every header compiles warning
//! free as C11 and C++17, with and without `R_NO_REMAP`, together and one at a time; every
//! function and variable they declare is one libR exports; the BLAS and LAPACK declarations
//! name real routines and call them correctly; and the graphics structures have GNU R's layout.
//!
//! The header list is read from `include/`, so a header added later is covered without
//! touching this file.

mod harness;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

const C_FLAGS: &[&str] = &["-x", "c", "-std=c11"];
const CXX_FLAGS: &[&str] = &["-x", "c++", "-std=c++17"];
const STRICT_FLAGS: &[&str] = &["-Wall", "-Wextra", "-pedantic", "-Werror"];

/// A labelled compile-and-check step; the label names the failing combination.
type Job = (String, Box<dyn FnOnce() -> Result<(), String> + Send>);

/// R_ext/GraphicsDevice.h refuses to be included before R_ext/GraphicsEngine.h, which includes
/// it, so the engine header always goes first.
const BEFORE: &[(&str, &str)] = &[("R_ext/GraphicsEngine.h", "R_ext/GraphicsDevice.h")];

/// BLAS and LAPACK live in the system libraries, not in libR.
/// The stats headers declare routines the stats package provides through R_GetCCallable, and
/// stats_stubs.h defines them package-locally.
const NOT_IN_LIBR: &[&str] = &[
    "R_ext/BLAS.h",
    "R_ext/Lapack.h",
    "R_ext/stats_package.h",
    "R_ext/stats_stubs.h",
];

/// Names `R_ext/Random.h` declares for a package to define: a user-supplied generator.
const DEFINED_BY_PACKAGES: &[&str] = &[
    "user_unif_rand",
    "user_unif_init",
    "user_unif_nseed",
    "user_unif_seedloc",
    "user_norm_rand",
];

/// Routines R's Lapack.h declares that no LAPACK library exports: the dlamc helpers live in
/// LAPACK's installation-time sources (only R's bundled LAPACK builds them), and dlasd9 left
/// LAPACK long ago.
const NOT_IN_SYSTEM_LAPACK: &[&str] = &["dlamc1", "dlamc2", "dlamc4", "dlamc5", "dlasd9"];

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn include_dir() -> PathBuf {
    manifest_dir().join("include")
}

fn probe_dir() -> PathBuf {
    manifest_dir().join("tests").join("headers")
}

fn work_dir(name: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("test_headers")
        .join(name);
    fs::create_dir_all(&directory).expect("the scratch directory can be created");
    directory
}

/// Every header under `include/` as an include path (`R.h`, `R_ext/Arith.h`), in an order
/// that satisfies the dependencies GNU R's headers have too: R.h and Rinternals.h first.
fn headers() -> Vec<String> {
    let mut found = Vec::new();
    for (directory, prefix) in [(include_dir(), ""), (include_dir().join("R_ext"), "R_ext/")] {
        for entry in fs::read_dir(&directory).expect("the include directory is readable") {
            let path = entry.expect("a directory entry").path();
            if path.extension().is_some_and(|extension| extension == "h") {
                let name = path.file_name().expect("a file name").to_string_lossy();
                found.push(format!("{prefix}{name}"));
            }
        }
    }
    let rank = |header: &String| match header.as_str() {
        "R.h" => 0,
        "Rinternals.h" => 1,
        _ if header.starts_with("R_ext/") => 3,
        _ => 2,
    };
    found.sort_by(|left, right| rank(left).cmp(&rank(right)).then(left.cmp(right)));
    for (first, second) in BEFORE {
        if let (Some(first_at), Some(second_at)) = (
            found.iter().position(|header| header == first),
            found.iter().position(|header| header == second),
        ) && first_at > second_at
        {
            let header = found.remove(first_at);
            found.insert(second_at, header);
        }
    }
    found
}

fn translation_unit(defines: &[&str], headers: &[&str]) -> String {
    let mut source = String::new();
    for define in defines {
        source.push_str(&format!("#define {define}\n"));
    }
    for header in headers {
        // Rconn's members `class` and `private` are C++ keywords, so C++ packages rename them
        // around the include (readr, vroom and cpp11 do), as GNU R's header requires too.
        if *header == "R_ext/Connections.h" {
            source.push_str(
                "#ifdef __cplusplus\n#define class class_name\n#define private private_ptr\n#endif\n",
            );
            source.push_str(&format!("#include <{header}>\n"));
            source.push_str("#ifdef __cplusplus\n#undef class\n#undef private\n#endif\n");
            continue;
        }
        source.push_str(&format!("#include <{header}>\n"));
    }
    source.push_str("int main(void) { return 0; }\n");
    source
}

/// Compiles `source` with the strict flags and returns the compiler's diagnostics on failure.
fn compile(
    source: &str,
    language: &[&str],
    extra: &[&str],
    output: Option<&Path>,
) -> Result<(), String> {
    let compiler = if language == CXX_FLAGS { "c++" } else { "cc" };
    let mut command = Command::new(compiler);
    command
        .args(language)
        .args(STRICT_FLAGS)
        .arg("-I")
        .arg(include_dir());
    match output {
        Some(path) => command.arg("-o").arg(path),
        None => command.arg("-fsyntax-only"),
    };
    command
        .arg("-")
        .args(extra)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("cannot run {compiler}: {error}"));
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("the compiler's stdin is piped");
        stdin
            .write_all(source.as_bytes())
            .expect("the compiler reads its source");
    }
    let result = child
        .wait_with_output()
        .expect("the compiler runs to completion");
    if result.status.success() && result.stderr.is_empty() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&result.stderr).into_owned())
    }
}

/// Runs every job on its own thread and reports all failures at once.
fn run_all(jobs: Vec<Job>) {
    let failures: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(label, job)| {
                scope.spawn(move || job().err().map(|error| format!("{label}:\n{error}")))
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().expect("a compile job does not panic"))
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn language_name(language: &[&str]) -> &'static str {
    if language == CXX_FLAGS {
        "C++17"
    } else {
        "C11"
    }
}

#[test]
fn every_header_compiles_cleanly_together() {
    let headers = headers();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    let mut jobs: Vec<Job> = Vec::new();
    for language in [C_FLAGS, CXX_FLAGS] {
        for defines in [
            &[][..],
            &["R_NO_REMAP"][..],
            &["STRICT_R_HEADERS", "R_NO_REMAP"][..],
        ] {
            let source = translation_unit(defines, &headers);
            jobs.push((
                format!("all headers, {} {defines:?}", language_name(language)),
                Box::new(move || compile(&source, language, &[], None)),
            ));
        }
    }
    run_all(jobs);
}

#[test]
fn every_header_compiles_after_r_and_rinternals() {
    let mut jobs: Vec<Job> = Vec::new();
    for header in headers() {
        if BEFORE.iter().any(|(_, second)| *second == header) {
            continue;
        }
        let mut included = vec!["R.h", "Rinternals.h"];
        if !included.contains(&header.as_str()) {
            included.push(&header);
        }
        for language in [C_FLAGS, CXX_FLAGS] {
            for defines in [&[][..], &["R_NO_REMAP"][..]] {
                let source = translation_unit(defines, &included);
                jobs.push((
                    format!("{header}, {} {defines:?}", language_name(language)),
                    Box::new(move || compile(&source, language, &[], None)),
                ));
            }
        }
    }
    run_all(jobs);
}

#[test]
fn graphics_macros_expand_cleanly() {
    let source =
        fs::read_to_string(probe_dir().join("macro_probe.c")).expect("the macro probe is readable");
    let mut jobs: Vec<Job> = Vec::new();
    for language in [C_FLAGS, CXX_FLAGS] {
        for defines in [&[][..], &["R_NO_REMAP"][..]] {
            let mut source_with_defines = String::new();
            for define in defines {
                source_with_defines.push_str(&format!("#define {define}\n"));
            }
            source_with_defines.push_str(&source);
            jobs.push((
                format!("macro probe, {} {defines:?}", language_name(language)),
                Box::new(move || compile(&source_with_defines, language, &[], None)),
            ));
        }
    }
    run_all(jobs);
}

#[test]
fn r_and_rinternals_compile_on_their_own() {
    let mut jobs: Vec<Job> = Vec::new();
    for header in ["R.h", "Rinternals.h"] {
        for language in [C_FLAGS, CXX_FLAGS] {
            let source = translation_unit(&[], &[header]);
            jobs.push((
                format!("{header} alone, {}", language_name(language)),
                Box::new(move || compile(&source, language, &[], None)),
            ));
        }
    }
    run_all(jobs);
}

/// The functions a set of headers declares, read from the compiler's own record of every
/// prototype (`-aux-info`), so macros such as `#define curDevice Rf_curDevice` are resolved
/// exactly as a package's compiler resolves them.
fn declared_functions(headers: &[&str], directory: &Path) -> BTreeMap<String, String> {
    let info = directory.join("declarations.txt");
    let source = directory.join("declarations.c");
    fs::write(&source, translation_unit(&[], headers)).expect("the probe source is written");
    let status = Command::new("cc")
        .args(["-std=c11", "-fsyntax-only", "-aux-info"])
        .arg(&info)
        .arg("-I")
        .arg(include_dir())
        .arg(&source)
        .status()
        .expect("cc runs");
    assert!(status.success(), "the headers compile for -aux-info");
    let include = include_dir();
    let include = include.to_string_lossy();
    let mut functions = BTreeMap::new();
    for line in fs::read_to_string(&info)
        .expect("cc writes the -aux-info file")
        .lines()
    {
        // `/* <file>:<line>:NC */ extern int Rf_curDevice (void);` where C marks a declaration.
        let Some(rest) = line.strip_prefix("/* ") else {
            continue;
        };
        let Some((location, declaration)) = rest.split_once(" */ ") else {
            continue;
        };
        if !location.starts_with(include.as_ref()) || !location.ends_with('C') {
            continue;
        }
        let file = location
            .trim_start_matches(include.as_ref())
            .trim_start_matches('/')
            .split(':')
            .next()
            .unwrap_or_default()
            .to_owned();
        let Some(before_parameters) = declaration.split(" (").next() else {
            continue;
        };
        let name = before_parameters
            .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
            .next()
            .unwrap_or_default();
        if !name.is_empty() {
            functions.insert(name.to_owned(), file);
        }
    }
    functions
}

/// The variables a set of headers declares `extern` (GNU R spells it `LibExtern`), read from
/// the preprocessed source so macros are resolved, and attributed to their header through the
/// preprocessor's line markers.
fn declared_variables(headers: &[&str], directory: &Path) -> BTreeMap<String, String> {
    let source = directory.join("variables.c");
    fs::write(&source, translation_unit(&[], headers)).expect("the probe source is written");
    let output = Command::new("cc")
        .args(["-std=c11", "-E"])
        .arg("-I")
        .arg(include_dir())
        .arg(&source)
        .output()
        .expect("cc runs");
    assert!(output.status.success(), "the headers preprocess");
    let include = include_dir().to_string_lossy().into_owned();
    // Each statement of our headers' text, with the header it came from.
    let mut statements: Vec<(String, String)> = Vec::new();
    let mut file = String::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(marker) = line.strip_prefix("# ") {
            let path = marker.split('"').nth(1).unwrap_or_default();
            file = path
                .strip_prefix(include.as_str())
                .map(|relative| relative.trim_start_matches('/').to_owned())
                .unwrap_or_default();
            continue;
        }
        if file.is_empty() {
            continue;
        }
        for character in line.replace("extern \"C\" {", "").chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                ';' if depth == 0 => {
                    statements.push((std::mem::take(&mut current), file.clone()));
                    continue;
                }
                _ => {}
            }
            if depth == 0 && character != '}' {
                current.push(character);
            }
        }
        current.push(' ');
    }
    let mut variables = BTreeMap::new();
    for (statement, file) in statements {
        let statement = statement.trim();
        let Some(declaration) = statement.strip_prefix("extern ") else {
            continue;
        };
        // A function pointer variable names itself inside `(*name)`; any other parenthesis
        // makes the statement a function, which -aux-info covers.
        let name = if let Some((_, rest)) = declaration.split_once("(*") {
            rest.split(')').next().unwrap_or_default().trim()
        } else if declaration.contains('(') {
            continue;
        } else {
            declaration
                .split('[')
                .next()
                .unwrap_or_default()
                .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
                .find(|word| !word.is_empty())
                .unwrap_or_default()
        };
        if !name.is_empty() {
            variables.insert(name.to_owned(), file);
        }
    }
    variables
}

fn dynamic_symbols(library: &Path) -> BTreeSet<String> {
    let output = Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(library)
        .output()
        .expect("nm runs");
    assert!(output.status.success(), "nm reads {}", library.display());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .map(|symbol| symbol.split('@').next().unwrap_or(symbol).to_owned())
        .collect()
}

fn libr_path() -> PathBuf {
    harness::library_dir().join("libR.so")
}

/// Every function and variable the headers declare is exported, so a package that compiles
/// against them also links.
#[test]
fn every_declared_name_is_exported_by_libr() {
    let headers: Vec<String> = headers()
        .into_iter()
        .filter(|header| !NOT_IN_LIBR.contains(&header.as_str()))
        .collect();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    let directory = work_dir("exports");
    let mut declared = declared_functions(&headers, &directory);
    assert!(
        declared.len() > 300
            && declared.contains_key("Rf_curDevice")
            && declared.contains_key("Rf_allocVector"),
        "the declaration scan finds the headers' functions"
    );
    let variables = declared_variables(&headers, &directory);
    assert!(
        variables.len() > 50
            && variables.contains_key("R_NilValue")
            && variables.contains_key("mbcslocale"),
        "the declaration scan finds the headers' variables: {variables:?}"
    );
    declared.extend(variables);
    let exported = dynamic_symbols(&libr_path());
    let missing: Vec<String> = declared
        .iter()
        // Applic.h and Linpack.h include BLAS.h, so its names reach the scan transitively.
        .filter(|(name, file)| {
            !exported.contains(*name)
                && !NOT_IN_LIBR.contains(&file.as_str())
                && !DEFINED_BY_PACKAGES.contains(&name.as_str())
        })
        .map(|(name, file)| format!("{name} ({file})"))
        .collect();
    assert!(
        missing.is_empty(),
        "declared in ry's headers but not exported by libR:\n{}",
        missing.join("\n")
    );
}

/// The path of a system library the C compiler would link, or `None` when it has none.
fn system_library(file: &str) -> Option<PathBuf> {
    let output = Command::new("cc")
        .arg(format!("-print-file-name={file}"))
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_absolute().then_some(path)
}

fn blas_and_lapack() -> Option<(PathBuf, PathBuf)> {
    let found = system_library("libblas.so").zip(system_library("liblapack.so"));
    if found.is_none() {
        eprintln!("skipped: the system has no libblas.so and liblapack.so to link against");
    }
    found
}

fn fortran_names(header: &str) -> Vec<String> {
    let text = fs::read_to_string(include_dir().join(header)).expect("the header is readable");
    text.split("F77_NAME(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_blas_and_lapack_declaration_names_a_system_routine() {
    let Some((blas, lapack)) = blas_and_lapack() else {
        return;
    };
    let mut exported = dynamic_symbols(&blas);
    exported.extend(dynamic_symbols(&lapack));
    let mut declared = BTreeSet::new();
    for header in ["R_ext/BLAS.h", "R_ext/Lapack.h"] {
        for name in fortran_names(header) {
            declared.insert(name);
        }
    }
    assert!(
        declared.len() > 500,
        "the declaration scan finds the routines"
    );
    let missing: Vec<&String> = declared
        .iter()
        .filter(|name| {
            !exported.contains(&format!("{name}_"))
                && !NOT_IN_SYSTEM_LAPACK.contains(&name.as_str())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "declared but not in the system BLAS or LAPACK (a typo?): {missing:?}"
    );
    for name in NOT_IN_SYSTEM_LAPACK {
        assert!(declared.contains(*name), "{name} is still declared");
    }
}

#[test]
fn blas_and_lapack_calls_compute_the_right_answers() {
    if blas_and_lapack().is_none() {
        return;
    }
    let source = fs::read_to_string(probe_dir().join("lapack_probe.c"))
        .expect("the LAPACK probe is readable");
    let directory = work_dir("lapack");
    let mut jobs: Vec<Job> = Vec::new();
    for language in [C_FLAGS, CXX_FLAGS] {
        for define in [
            None,
            Some("DONT_USE_FC_LEN_T"),
            Some("USE_FC_LEN_T"),
            Some("usePR18534fix"),
        ] {
            let label = format!(
                "{} {}",
                language_name(language),
                define.unwrap_or("default")
            );
            let executable = directory.join(label.replace(' ', "_").replace('+', "x"));
            let source = match define {
                Some(define) => format!("#define {define}\n{source}"),
                None => source.clone(),
            };
            jobs.push((
                label,
                Box::new(move || {
                    compile(
                        &source,
                        language,
                        &["-llapack", "-lblas", "-lm"],
                        Some(&executable),
                    )?;
                    let output = Command::new(&executable)
                        .output()
                        .map_err(|error| error.to_string())?;
                    if output.status.success() {
                        Ok(())
                    } else {
                        Err(String::from_utf8_lossy(&output.stdout).into_owned())
                    }
                }),
            ));
        }
    }
    run_all(jobs);
}

/// Compiles the layout probe against `include` (ry's headers when `None`) and runs it.
fn graphics_layout(include: Option<&Path>, defines: &[&str], name: &str) -> String {
    let directory = work_dir("layout");
    let executable = directory.join(name);
    let mut command = Command::new("cc");
    command.arg("-std=c11");
    for define in defines {
        command.arg(format!("-D{define}"));
    }
    command
        .arg("-I")
        .arg(include.map_or_else(include_dir, Path::to_path_buf))
        .arg(probe_dir().join("graphics_layout.c"))
        .arg("-o")
        .arg(&executable);
    let output = command.output().expect("cc runs");
    assert!(
        output.status.success(),
        "the layout probe compiles:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(&executable)
        .output()
        .expect("the layout probe runs");
    String::from_utf8(output.stdout).expect("the probe prints ASCII")
}

#[test]
fn graphics_structures_have_r_4_6_layout() {
    let ry = graphics_layout(None, &["PROBE_LOCK"], "ry");
    let expected = fs::read_to_string(probe_dir().join("graphics_layout_r46.txt"))
        .expect("the recorded R 4.6 layout is readable");
    let differences: Vec<String> = expected
        .lines()
        .zip(ry.lines())
        .filter(|(expected, ry)| expected != ry)
        .map(|(expected, ry)| format!("R 4.6: {expected}\nry:    {ry}"))
        .collect();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert_eq!(expected.lines().count(), ry.lines().count());
}

/// Fields GNU R 4.3 lays out differently by design: GEDevDesc gained its lock field after
/// recordGraphics, and the engine version moved on.
fn differs_from_r_4_3(line: &str) -> bool {
    [
        "GEDevDesc ",
        "GEDevDesc.gesd ",
        "GEDevDesc.ask ",
        "GEDevDesc.appending ",
        "R_GE_version ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

#[test]
fn graphics_structures_match_the_installed_gnu_r() {
    let gnu_include = Path::new("/usr/share/R/include");
    if !gnu_include.join("R_ext").join("GraphicsEngine.h").exists() {
        eprintln!("skipped: GNU R's headers are not installed");
        return;
    }
    let gnu = graphics_layout(Some(gnu_include), &[], "gnu");
    let ry = graphics_layout(None, &[], "ry_without_lock");
    let shared = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !differs_from_r_4_3(line))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(shared(&gnu), shared(&ry));
}

/// `Rf_ucstoutf8`, which GraphicsDevice.h declares, writes the same bytes as Rust's encoder for
/// every valid code point and returns their count.
#[test]
fn ucstoutf8_encodes_every_valid_code_point() {
    let r = harness::r();
    let encode = unsafe {
        *r.api()
            .library
            .get::<unsafe extern "C" fn(*mut std::ffi::c_char, u32) -> usize>(b"Rf_ucstoutf8\0")
            .expect("libR exports Rf_ucstoutf8")
    };
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let edges = [
        0, 1, 0x7F, 0x80, 0x7FF, 0x800, 0xD7FF, 0xE000, 0xFFFF, 0x1_0000, 0x10_FFFF,
    ];
    let random = (0..4000).map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 0x11_0000) as u32
    });
    for code in edges.into_iter().chain(random) {
        let Some(character) = char::from_u32(code) else {
            continue;
        };
        let mut expected = [0u8; 4];
        let expected = character.encode_utf8(&mut expected).as_bytes();
        let mut buffer = [0xAAu8; 8];
        let written = unsafe { encode(buffer.as_mut_ptr().cast(), code) };
        assert_eq!(written, expected.len(), "length for U+{code:04X}");
        assert_eq!(&buffer[..written], expected, "bytes for U+{code:04X}");
        assert_eq!(buffer[written], 0, "terminator for U+{code:04X}");
    }
}
