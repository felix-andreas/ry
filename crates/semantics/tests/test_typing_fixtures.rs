//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use salsa::Setter;
use semantics::annotations::lower_annotation;
use semantics::diagnostics::{Diagnostic, TypeRenderer, file_diagnostics, strict_diagnostics};
use semantics::metadata::{
    PackageMetadata, attached_union, normalized_imports, parse_description_dependencies,
    parse_description_package, parse_namespace_imports,
};
use semantics::stubs::{StubSources, install_shipped_stubs, stub_library, stub_source_problems};
use semantics::testing::with_fixture_project;
use semantics::{
    AnalysisSettings, DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, file_typing_mode, item_check,
    item_hir, item_tree,
};
use std::path::Path;
use syntax::SyntaxKind;
use syntax::testing::{run_fixture_suite, split_files};

fn render_case(
    source: &str,
    kind: DocumentKind,
    render: &dyn Fn(&RootDatabase, SourceFile) -> String,
) -> String {
    let files: Vec<(Option<String>, String, DocumentKind)> = match split_files(source) {
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

/// The `#namespace `/`#description ` lines stay in the analyzed text, so ranges stay honest.
fn render_with_metadata(source: &str) -> String {
    let mut db = RootDatabase::default();
    install_shipped_stubs(&db);
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
    let imports = parse_namespace_imports(&namespace_source);
    let metadata = PackageMetadata::new(
        &db,
        normalized_imports(&imports),
        parse_description_dependencies(&description_source),
        Default::default(),
        parse_description_package(&description_source),
    );
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);
    // Attach facts are scanned from the sources exactly as the hosts do.
    let attached = attached_union(&db, [file]);
    if !attached.is_empty() {
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
    render_diagnostics(&mut output, &file_diagnostics(db, file));
    output
}

fn render_diagnostics(output: &mut String, diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        output.push_str(&format!(
            "{}..{} {}[{}] {}\n",
            u32::from(diagnostic.range.start()),
            u32::from(diagnostic.range.end()),
            format!("{:?}", diagnostic.severity).to_lowercase(),
            diagnostic.code,
            diagnostic.message
        ));
        for related in &diagnostic.related {
            output.push_str(&format!(
                "  = {}..{} {}\n",
                u32::from(related.range.start()),
                u32::from(related.range.end()),
                related.message
            ));
        }
    }
}

#[test]
fn typing_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing");
    run_fixture_suite(&suite, &|source| {
        render_case(source, DocumentKind::Package, &render_file)
    });
}

#[test]
fn typing_import_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-imports");
    run_fixture_suite(&suite, &render_with_metadata);
}

#[test]
fn typing_script_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-scripts");
    run_fixture_suite(&suite, &|source| {
        render_case(source, DocumentKind::Script, &render_file)
    });
}

fn render_with_strict(source: &str) -> String {
    render_case(source, DocumentKind::Package, &|db, file| {
        let mut output = String::new();
        if let Some(mode) = file_typing_mode(db, file) {
            output.push_str(&format!("typing mode: {mode:?}\n"));
        }
        output.push_str(&render_file(db, file));
        render_diagnostics(&mut output, &strict_diagnostics(db, file));
        output
    })
}

#[test]
fn typing_strict_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-strict");
    run_fixture_suite(&suite, &render_with_strict);
}

/// A fresh database, because the shared fixture database keeps its settings input.
fn render_with_inline_annotations_off(source: &str) -> String {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    AnalysisSettings::new(&db, false);
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);
    render_file(&db, file)
}

#[test]
fn typing_inline_off_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typing-inline-off");
    run_fixture_suite(&suite, &render_with_inline_annotations_off);
}

fn render_stub_source(source: &str) -> String {
    let db = RootDatabase::default();
    let sources = StubSources::new(&db, vec![("base".to_owned(), source.to_owned())], vec![]);
    let library = stub_library(&db, sources);
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
    for problem in stub_source_problems(&db, source) {
        output.push_str(&format!("line {}: {}\n", problem.line, problem.message));
    }
    output
}

#[test]
fn stub_source_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stubs");
    run_fixture_suite(&suite, &render_stub_source);
}

/// Each case line becomes one line of a single `#:` block.
fn render_annotation_block(source: &str) -> String {
    let text: String = source.lines().map(|line| format!("#: {line}\n")).collect();
    let db = RootDatabase::default();
    let parse = syntax::parse(&format!("{text}x <- NULL\n"));
    let Some(node) = parse
        .syntax_node()
        .descendants()
        .find(|node| node.kind() == SyntaxKind::ANNOTATION)
    else {
        return "no annotation".to_owned();
    };
    let annotation = lower_annotation(&db, &node);
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
    run_fixture_suite(&suite, &render_annotation_block);
}
