//! Reads the R packages installed on this machine. Each test returns early when the package
//! it needs is missing, so the suite passes where R is not installed.

use {
    rdb::{find_package, library_paths, package_source, topic_documentation},
    std::path::{Path, PathBuf},
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
