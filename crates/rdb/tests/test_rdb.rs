//! Reads the R packages installed on this machine. Each test returns early when the package
//! it needs is missing, so the suite passes where R is not installed.

use {
    flate2::{Compression, write::GzEncoder},
    rdb::{dataset_names, find_package, library_paths, package_source, topic_documentation},
    std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
    },
};

fn installed(package: &str) -> Option<PathBuf> {
    let found = find_package(
        &library_paths(Path::new(env!("CARGO_MANIFEST_DIR"))),
        package,
    );
    if found.is_none() {
        eprintln!("skipping: R package {package} is not installed");
    }
    found
}

/// Parses `text` with ry's parser and fails with the first errors and their context.
fn assert_parses(package: &str, text: &str) {
    let parse = syntax::parse(text);
    let errors: Vec<String> = parse
        .errors()
        .iter()
        .take(5)
        .map(|error| {
            let start = usize::from(error.range.start());
            let context_start = text.floor_char_boundary(start.saturating_sub(120));
            let context_end = text.ceil_char_boundary((start + 80).min(text.len()));
            format!(
                "{error:?}\n--- context ---\n{}\n---------------",
                &text[context_start..context_end]
            )
        })
        .collect();
    assert!(
        errors.is_empty(),
        "the generated source of {package} has syntax errors:\n{}",
        errors.join("\n")
    );
}

#[test]
fn base_source_has_lapply() {
    let Some(base) = installed("base") else {
        return;
    };
    let source = package_source(&base).expect("base's lazy-load database reads");
    assert!(
        source.text.contains("lapply <- function(X, FUN, ...)"),
        "lapply is missing from base's source"
    );
    let (_, range) = source
        .definitions
        .iter()
        .find(|(name, _)| name == "lapply")
        .expect("lapply is defined");
    assert_eq!(&source.text[range.clone()], "lapply");
    assert!(
        source
            .definitions
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0),
        "definitions are sorted by name"
    );
    assert!(
        source
            .definitions
            .iter()
            .all(|(name, _)| !name.starts_with(".__")),
        "internal objects are skipped"
    );
    assert_parses("base", &source.text);
}

#[test]
fn base_help_for_lapply() {
    let Some(base) = installed("base") else {
        return;
    };
    let markdown = topic_documentation(&base, "lapply").expect("lapply has a help page");
    assert!(
        markdown.starts_with("**Apply a Function over a List or Vector**"),
        "unexpected help page:\n{markdown}"
    );
    assert!(markdown.contains("```r\nlapply(X, FUN, ...)"));
    assert!(markdown.contains("- `X`: "));
    // `sapply` is an alias on the same page.
    assert_eq!(topic_documentation(&base, "sapply"), Some(markdown));
    assert_eq!(topic_documentation(&base, "no such topic at all"), None);
}

#[test]
fn rlang_source_has_as_function() {
    let Some(rlang) = installed("rlang") else {
        return;
    };
    let source = package_source(&rlang).expect("rlang's lazy-load database reads");
    assert!(source.text.contains("\nas_function <- function("));
    assert!(rdb::defines(&rlang, "as_function"));
    assert!(!rdb::defines(&rlang, "no_such_function_anywhere"));
    assert_parses("rlang", &source.text);
}

#[test]
fn lifecycle_source_parses() {
    let Some(lifecycle) = installed("lifecycle") else {
        return;
    };
    let source = package_source(&lifecycle).expect("lifecycle's lazy-load database reads");
    assert!(source.text.contains("\ndeprecate_warn <- function("));
    assert_parses("lifecycle", &source.text);
}

/// A saved image holding `names`, as `save(lung, cancer, file = "cancer.rda")` writes one:
/// the `RDX2` magic, then an XDR stream of a pairlist tagged with each name.
fn saved_image(names: &[&str]) -> Vec<u8> {
    const SYMSXP: i32 = 1;
    const LISTSXP: i32 = 2;
    const HAS_TAG: i32 = 1 << 10;
    const CHARSXP_ASCII: i32 = 9 | (64 << 12);
    const NILVALUE_SXP: i32 = 254;
    let mut bytes = b"RDX2\nX\n".to_vec();
    let mut word = |bytes: &mut Vec<u8>, value: i32| bytes.extend_from_slice(&value.to_be_bytes());
    for header in [2, 0x040300, 0x020300] {
        word(&mut bytes, header);
    }
    for name in names {
        word(&mut bytes, LISTSXP | HAS_TAG);
        word(&mut bytes, SYMSXP);
        word(&mut bytes, CHARSXP_ASCII);
        word(&mut bytes, name.len() as i32);
        bytes.extend_from_slice(name.as_bytes());
        word(&mut bytes, NILVALUE_SXP);
    }
    word(&mut bytes, NILVALUE_SXP);
    bytes
}

fn package_with_data(name: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
    let package = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let data = package.join("data");
    if package.exists() {
        fs::remove_dir_all(&package).expect("the old test package is removable");
    }
    fs::create_dir_all(&data).expect("the data directory is creatable");
    for (file, contents) in files {
        fs::write(data.join(file), contents).expect("the data file is writable");
    }
    package
}

#[test]
fn dataset_names_read_every_object_of_a_saved_image() {
    let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
    compressed
        .write_all(&saved_image(&["lung", "cancer"]))
        .expect("gzip writes to memory");
    let image = compressed.finish().expect("gzip finishes in memory");
    let package = package_with_data(
        "datasets_from_files",
        &[
            ("cancer.rda", image),
            ("sales.csv.gz", Vec::new()),
            ("README", Vec::new()),
        ],
    );
    assert_eq!(
        dataset_names(&package).expect("the data directory is readable"),
        ["cancer", "lung", "sales"]
    );
}

#[test]
fn dataset_names_are_unknowable_from_r_code() {
    let package = package_with_data("datasets_from_code", &[("made.R", b"made <- 1".to_vec())]);
    assert!(dataset_names(&package).is_err());
}

#[test]
fn dataset_names_of_a_package_without_data_are_empty() {
    let package = Path::new(env!("CARGO_TARGET_TMPDIR")).join("datasets_absent");
    fs::create_dir_all(&package).expect("the package directory is creatable");
    assert_eq!(
        dataset_names(&package).expect("a missing data directory is no error"),
        Vec::<String>::new()
    );
}
