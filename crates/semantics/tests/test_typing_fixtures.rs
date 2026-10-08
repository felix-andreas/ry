//! Typing fixture suite: each case runs the full semantic pipeline on one
//! package file (shipped stubs installed) and renders every named top-level
//! definition's exported scheme followed by the file's diagnostics.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one
//! case.

use semantics::diagnostics::{Severity, TypeRenderer, file_diagnostics, strict_diagnostics};
use semantics::testing::with_fixture_project;
use semantics::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, file_typing_mode, item_check,
    item_hir, item_tree,
};
use std::path::Path;

fn render(source: &str) -> String {
    render_as(source, DocumentKind::Package)
}

fn render_script(source: &str) -> String {
    render_as(source, DocumentKind::Script)
}

fn render_as(source: &str, kind: DocumentKind) -> String {
    render_case(source, kind, &render_file)
}

/// Renders every file of the case: one unnamed file, or each `#~~~~ path`
/// section of a multi-file case under an `== path` line — package files under
/// `R/`, scripts elsewhere.
fn render_case(
    source: &str,
    kind: DocumentKind,
    render: &dyn Fn(&RootDatabase, SourceFile) -> String,
) -> String {
    let files: Vec<(Option<String>, String, DocumentKind)> =
        match syntax::testing::split_files(source) {
            None => vec![(None, source.to_owned(), kind)],
            Some(files) => files
                .into_iter()
                .map(|(path, text)| {
                    let kind = if path.starts_with("R/") {
                        DocumentKind::Package
                    } else {
                        DocumentKind::Script
                    };
                    (Some(path), text, kind)
                })
                .collect(),
        };
    let inputs = files
        .iter()
        .map(|(_, text, kind)| (text.clone(), *kind))
        .collect();
    with_fixture_project(inputs, |db, sources| {
        let mut output = String::new();
        for ((path, _, _), file) in files.iter().zip(sources) {
            if let Some(path) = path {
                output.push_str(&format!("== {path}\n"));
            }
            output.push_str(&render(db, *file));
        }
        output
    })
}

/// Package-metadata cases: leading `#namespace ` lines form the NAMESPACE
/// source and `#description ` lines the DESCRIPTION source (both stay in the
/// analyzed text as ordinary comments, so ranges are honest).
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
        let Some(check) = item_check(db, item) else {
            continue;
        };
        let mut renderer = TypeRenderer::default();
        let definition = matches!(*item.kind(db), ItemKind::Function | ItemKind::Value);
        if definition && let (Some(name), Some(scheme)) = (item.name(db), &check.scheme) {
            output.push_str(&format!("{name}: {}\n", renderer.render_scheme(db, scheme)));
        } else if let Some(root) = item_hir(db, item).as_ref().and_then(|module| module.root)
            && let Some(&ty) = check.expression_types.get(&root)
        {
            output.push_str(&renderer.render(db, ty));
            output.push('\n');
        }
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
    render_case(source, DocumentKind::Package, &|db, file| {
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

/// A stub source's declarations, overload candidates in declaration order,
/// then the problems the loader drops lines for — the wording the editor and
/// the override report show.
fn render_stub_source(source: &str) -> String {
    let db = RootDatabase::default();
    let sources = semantics::stubs::StubSources::new(
        &db,
        vec![("base".to_owned(), source.to_owned())],
        vec![],
    );
    let library = semantics::stubs::stub_library(&db, sources);
    let mut names: Vec<(&String, _)> = library
        .declarations
        .iter()
        .map(|(name, declaration)| (name, declaration.range.start()))
        .collect();
    names.sort_by_key(|(_, start)| *start);
    let mut nominals: Vec<&String> = library.nominals.iter().collect();
    nominals.sort();
    let mut output = String::new();
    for nominal in nominals {
        output.push_str(&format!("@type {nominal}\n"));
    }
    for (name, _) in names {
        for scheme in library.schemes.get(name).into_iter().flatten() {
            let rendered = TypeRenderer::default().render_scheme(&db, scheme);
            let masked = if library.masked.contains_key(name) {
                "@masked "
            } else {
                ""
            };
            output.push_str(&format!("{name} : {masked}{rendered}\n"));
        }
    }
    for problem in semantics::stubs::stub_source_problems(&db, source) {
        output.push_str(&format!("line {}: {}\n", problem.line, problem.message));
    }
    output
}

#[test]
fn stub_source_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stubs");
    syntax::testing::run_fixture_suite(&suite, &render_stub_source);
}

/// One `#:` block on its own: the case's lines become the block, and the
/// expectation is what it lowers to — the coercion kind, the declared type,
/// the `@new` target, each `@type`/`@alias` definition — then the grammar's
/// and the lowering's errors.
fn render_annotation_block(source: &str) -> String {
    let text: String = source.lines().map(|line| format!("#: {line}\n")).collect();
    let db = RootDatabase::default();
    let parse = syntax::parse(&format!("{text}x <- NULL\n"));
    let Some(node) = parse
        .syntax_node()
        .descendants()
        .find(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    else {
        return "no annotation".to_owned();
    };
    let annotation = semantics::annotations::lower_annotation(&db, &node);
    let mut renderer = TypeRenderer::default();
    let mut output = String::new();
    if let Some(declared) = &annotation.declared {
        let kind = match (annotation.trusted, annotation.if_unknown) {
            (true, _) => "trust ",
            (_, true) => "if-unknown ",
            _ => "",
        };
        output.push_str(&format!(
            "{kind}{}\n",
            renderer.render_scheme(&db, declared)
        ));
    }
    if let Some((name, arguments, _)) = &annotation.new_nominal {
        let arguments: Vec<String> = arguments
            .iter()
            .map(|ty| renderer.render(&db, *ty))
            .collect();
        let arguments = if arguments.is_empty() {
            String::new()
        } else {
            format!("<{}>", arguments.join(", "))
        };
        output.push_str(&format!("new {}{arguments}\n", name.text(&db)));
    }
    for definition in &annotation.definitions {
        let parameters: Vec<&str> = definition
            .parameters
            .iter()
            .map(|name| name.text(&db))
            .collect();
        let parameters = if parameters.is_empty() {
            String::new()
        } else {
            format!("<{}>", parameters.join(", "))
        };
        output.push_str(&format!(
            "{} {}{parameters} = {}\n",
            if definition.alias { "alias" } else { "type" },
            definition.name.text(&db),
            renderer.render(&db, definition.body)
        ));
    }
    for error in parse.errors() {
        output.push_str(&format!("syntax error: {}\n", error.message));
    }
    for (message, _) in annotation.errors.iter().chain(&annotation.typing_errors) {
        output.push_str(&format!("error: {message}\n"));
    }
    output
}

#[test]
fn annotation_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/annotations");
    syntax::testing::run_fixture_suite(&suite, &render_annotation_block);
}
