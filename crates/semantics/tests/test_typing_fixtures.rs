//! Typing fixture suite: each case runs the full semantic pipeline on one
//! package file (shipped stubs installed) and renders every named top-level
//! definition's exported scheme followed by the file's diagnostics.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one
//! case.

use semantics::diagnostics::{Severity, TypeRenderer, file_diagnostics, strict_diagnostics};
use semantics::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, file_typing_mode, item_check,
    item_tree,
};
use std::path::Path;

fn render(source: &str) -> String {
    render_as(source, DocumentKind::Package)
}

fn render_script(source: &str) -> String {
    render_as(source, DocumentKind::Script)
}

fn render_as(source: &str, kind: DocumentKind) -> String {
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    render_project(&db, &project_files(&db, source, kind), &render_file)
}

/// The case's files: one unnamed file, or every `#~~~~ path` section of a
/// multi-file case — package files under `R/`, scripts elsewhere.
fn project_files(
    db: &RootDatabase,
    source: &str,
    kind: DocumentKind,
) -> Vec<(Option<String>, SourceFile)> {
    let files: Vec<(Option<String>, SourceFile)> = match syntax::testing::split_files(source) {
        None => vec![(None, SourceFile::new(db, source.to_owned(), kind))],
        Some(files) => files
            .into_iter()
            .map(|(path, text)| {
                let kind = if path.starts_with("R/") {
                    DocumentKind::Package
                } else {
                    DocumentKind::Script
                };
                (Some(path), SourceFile::new(db, text, kind))
            })
            .collect(),
    };
    ProjectFiles::new(db, files.iter().map(|(_, file)| *file).collect());
    files
}

fn render_project(
    db: &RootDatabase,
    files: &[(Option<String>, SourceFile)],
    render: &dyn Fn(&RootDatabase, SourceFile) -> String,
) -> String {
    let mut output = String::new();
    for (path, file) in files {
        if let Some(path) = path {
            output.push_str(&format!("== {path}\n"));
        }
        output.push_str(&render(db, *file));
    }
    output
}

/// Package-metadata cases: leading `#namespace ` lines form the NAMESPACE
/// source and `#description ` lines the DESCRIPTION source (both stay in the
/// analyzed text as ordinary comments, so ranges are honest). The suite is
/// new-stack only — the oracle has no package-metadata concept — so it is
/// deliberately absent from the differential fixture arm.
fn render_with_metadata(source: &str) -> String {
    let mut db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let mut namespace_source = String::new();
    let mut description_source = String::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("#namespace ") {
            namespace_source.push_str(rest);
            namespace_source.push('\n');
        } else if let Some(rest) = line.strip_prefix("#description ") {
            description_source.push_str(rest);
            description_source.push('\n');
        }
    }
    let imports = semantics::metadata::parse_namespace_imports(&namespace_source);
    let metadata = semantics::metadata::PackageMetadata::new(
        &db,
        semantics::metadata::normalized_imports(&imports),
        semantics::metadata::parse_description_dependencies(&description_source),
        Default::default(),
        semantics::metadata::parse_description_package(&description_source),
    );
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);
    // Attach facts flow exactly as in the hosts: scanned from the analyzed
    // sources, so `library(data.table)` cases activate the conditional
    // namespace with no fixture-only side channel.
    let attached = semantics::metadata::attached_union(&db, [file]);
    if !attached.is_empty() {
        use salsa::Setter;
        metadata.set_attached(&mut db).to(attached);
    }
    render_file(&db, file)
}

fn render_file(db: &RootDatabase, file: SourceFile) -> String {
    let mut output = String::new();
    for &item in item_tree(db, file) {
        if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value) {
            continue;
        }
        let Some(name) = item.name(db).clone() else {
            continue;
        };
        let Some(check) = item_check(db, item) else {
            continue;
        };
        let Some(scheme) = check.scheme else {
            continue;
        };
        let mut renderer = TypeRenderer::default();
        output.push_str(&name);
        output.push_str(": ");
        output.push_str(&renderer.render_scheme(db, &scheme));
        output.push('\n');
    }
    output.push_str(&render_diagnostics(db, file));
    output
}

fn render_diagnostics(db: &RootDatabase, file: SourceFile) -> String {
    let mut output = String::new();
    for diagnostic in file_diagnostics(db, file) {
        let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        output.push_str(&format!(
            "{}..{} {severity}[{}] {}\n",
            u32::from(diagnostic.range.start()),
            u32::from(diagnostic.range.end()),
            diagnostic.code,
            diagnostic.message
        ));
    }
    output
}

#[test]
fn typing_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing");
    syntax::testing::run_fixture_suite(&suite, &render);
}

#[test]
fn typing_import_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-imports");
    syntax::testing::run_fixture_suite(&suite, &render_with_metadata);
}

/// The same pipeline over script documents: one sequential top-down scope.
#[test]
fn typing_script_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-scripts");
    syntax::testing::run_fixture_suite(&suite, &render_script);
}

/// The strict stream: the per-file typing mode (when set) and the
/// `strict`-code diagnostics appended after the ordinary rendering.
fn render_with_strict(source: &str) -> String {
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files = project_files(&db, source, DocumentKind::Package);
    render_project(&db, &files, &|db, file| {
        let mut output = String::new();
        if let Some(mode) = file_typing_mode(db, file) {
            output.push_str(&format!("typing mode: {mode:?}\n"));
        }
        output.push_str(&render_file(db, file));
        for diagnostic in strict_diagnostics(db, file) {
            output.push_str(&format!(
                "{}..{} error[{}] {}\n",
                u32::from(diagnostic.range.start()),
                u32::from(diagnostic.range.end()),
                diagnostic.code,
                diagnostic.message
            ));
        }
        output
    })
}

#[test]
fn typing_strict_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-strict");
    syntax::testing::run_fixture_suite(&suite, &render_with_strict);
}
