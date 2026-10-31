//! A case carries at most one `$0` cursor marker; the output renders each feature at it, then
//! the inlay hints of the file holding it.

use ide::{
    DefinitionTarget, HoverDefinition, code_actions, completion, definition, document_symbols,
    hover, inlay_hints, references, rename, signature_help, type_definition, workspace_symbols,
};
use semantics::stubs::{StubSources, install_shipped_stubs};
use semantics::testing::{ProbeDatabase, with_fixture_project};
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::path::Path;
use syntax::testing::{run_fixture_suite, split_files};
use syntax::{TextRange, TextSize};

fn render(source: &str) -> String {
    let files: Vec<(Option<String>, String)> = match split_files(source) {
        None => vec![(None, source.to_owned())],
        Some(files) => files
            .into_iter()
            .map(|(path, text)| (Some(path), text))
            .collect(),
    };
    let mut cursor = None;
    let mut inputs = Vec::new();
    for (index, (path, text)) in files.iter().enumerate() {
        let mut text = text.clone();
        if let Some(at) = text.find("$0") {
            text.replace_range(at..at + 2, "");
            cursor = Some((index, TextSize::from(at as u32)));
        }
        let kind = match path {
            Some(path) if !path.starts_with("R/") => DocumentKind::Script,
            _ => DocumentKind::Package,
        };
        inputs.push((text, kind));
    }
    with_fixture_project(inputs, |db, sources| {
        let Some(project) = ProjectFiles::try_get(db) else {
            return "no project".to_owned();
        };
        let place = |file: SourceFile, range: TextRange| {
            let path = sources
                .iter()
                .position(|source| *source == file)
                .and_then(|index| files[index].0.as_ref())
                .map(|path| format!("{path}:"))
                .unwrap_or_default();
            format!(
                "{path}{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            )
        };
        let (index, offset) = cursor.unwrap_or((0, TextSize::from(0)));
        let file = sources[index];
        let mut output = String::new();
        if cursor.is_some() {
            render_at(db, project, file, offset, &place, &mut output);
        }
        for hint in inlay_hints(db, file, None) {
            output.push_str(&format!("hint @{}{}\n", u32::from(hint.offset), hint.label));
        }
        output
    })
}

/// Renders the covered stub text instead of byte offsets, which shift whenever an unrelated stub
/// is added.
fn stub_text(db: &RootDatabase, source_index: usize, range: TextRange) -> String {
    let Some(sources) = StubSources::try_get(db) else {
        return "<no stub sources>".to_owned();
    };
    let Some((_, text)) = sources.sources(db).get(source_index) else {
        return "<no such stub source>".to_owned();
    };
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    match text.get(start..end) {
        Some(slice) => format!("`{slice}`"),
        None => "<range outside the stub source>".to_owned(),
    }
}

fn render_at(
    db: &RootDatabase,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
    place: &dyn Fn(SourceFile, TextRange) -> String,
    output: &mut String,
) {
    match hover(db, files, file, offset) {
        Some(hover) => {
            for line in &hover.lines {
                output.push_str(&format!(
                    "hover {}..{}: {line}\n",
                    u32::from(hover.range.start()),
                    u32::from(hover.range.end()),
                ));
            }
            match hover.definition {
                Some(HoverDefinition::Local {
                    target,
                    maybe_undefined,
                }) => output.push_str(&format!(
                    "hover-definition: local {}{}\n",
                    place(target.file, target.range),
                    if maybe_undefined {
                        " (maybe undefined)"
                    } else {
                        ""
                    },
                )),
                Some(HoverDefinition::Global { target }) => output.push_str(&format!(
                    "hover-definition: global {}\n",
                    place(target.file, target.range),
                )),
                Some(HoverDefinition::Stub {
                    namespace,
                    overloads,
                    declaration,
                }) => {
                    let declared = declaration
                        .map(|target| {
                            format!(
                                ", declared in stub source {} at {}",
                                target.source_index,
                                stub_text(db, target.source_index, target.range),
                            )
                        })
                        .unwrap_or_default();
                    output.push_str(&format!(
                        "hover-definition: package {namespace} ({overloads} declaration(s){declared})\n"
                    ))
                }
                None => {}
            }
        }
        None => output.push_str("hover: none\n"),
    }
    let render_target = |label: &str, target: Option<DefinitionTarget>| match target {
        Some(DefinitionTarget::Project(target)) => {
            format!("{label} {}\n", place(target.file, target.range))
        }
        Some(DefinitionTarget::Stub(target)) => format!(
            "{label}: stub source {} at {}\n",
            target.source_index,
            stub_text(db, target.source_index, target.range),
        ),
        None => format!("{label}: none\n"),
    };
    output.push_str(&render_target(
        "definition",
        definition(db, files, file, offset),
    ));
    let references = references(db, files, file, offset, true);
    if references.is_empty() {
        output.push_str("references: none\n");
    } else {
        let ranges: Vec<String> = references
            .iter()
            .map(|occurrence| {
                format!(
                    "{}{}",
                    place(occurrence.file, occurrence.range),
                    if occurrence.is_declaration { "*" } else { "" }
                )
            })
            .collect();
        output.push_str(&format!("references: {}\n", ranges.join(", ")));
    }
    match rename(db, files, file, offset) {
        Some(edits) => output.push_str(&format!("rename: {} edit(s)\n", edits.len())),
        None => output.push_str("rename: none\n"),
    }
    if let Some(type_target) = type_definition(db, files, file, offset) {
        output.push_str(&render_target("type-definition", Some(type_target)));
    }
    match signature_help(db, file, offset) {
        Some(help) => {
            for (signature_index, signature) in help.signatures.iter().enumerate() {
                let marker =
                    if help.signatures.len() > 1 && signature_index == help.active_signature {
                        " (active)"
                    } else {
                        ""
                    };
                output.push_str(&format!("signature: {}{marker}\n", signature.label));
                let parameters: Vec<String> = signature
                    .parameters
                    .iter()
                    .enumerate()
                    .map(|(index, span)| {
                        let text =
                            &signature.label[usize::from(span.start())..usize::from(span.end())];
                        if Some(index) == signature.active_parameter {
                            format!("[{text}]")
                        } else {
                            text.to_owned()
                        }
                    })
                    .collect();
                output.push_str(&format!("parameters: {}\n", parameters.join(" | ")));
            }
        }
        None => output.push_str("signature: none\n"),
    }
    for action in code_actions(db, file, TextRange::new(offset, offset)) {
        output.push_str(&format!("action: {}\n", action.title));
        for edit in &action.edits {
            output.push_str(&format!(
                "  edit {}..{} -> {:?}\n",
                u32::from(edit.range.start()),
                u32::from(edit.range.end()),
                edit.replacement
            ));
        }
    }
    match completion(db, files, file, offset) {
        Some(result) => {
            let labels: Vec<String> = result
                .items
                .iter()
                .take(8)
                .map(|item| item.label.clone())
                .collect();
            output.push_str(&format!(
                "completion ({}{}): {}\n",
                result.items.len(),
                if result.is_incomplete { "+" } else { "" },
                labels.join(", ")
            ));
        }
        None => output.push_str("completion: none\n"),
    }
}

#[test]
fn workspace_symbols_ranked() {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let first = SourceFile::new(
        &db,
        "alpha_one <- function() 1\nbeta <- 2\n".to_owned(),
        DocumentKind::Package,
    );
    let second = SourceFile::new(
        &db,
        "alpha_two <- function() 3\nnot_matching <- 4\n".to_owned(),
        DocumentKind::Package,
    );
    let files = ProjectFiles::new(&db, vec![first, second]);

    assert_eq!(
        symbol_names(&db, files, "alpha"),
        ["alpha_one", "alpha_two"]
    );
    assert_eq!(symbol_names(&db, files, "aone"), ["alpha_one"]);
}

#[test]
fn document_symbols_expose_s4_and_r6_hierarchy() {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let source = "\
setClass(\"Person\", representation(name = \"character\"))
setGeneric(\"greet\", function(object) standardGeneric(\"greet\"))
setMethod(\"greet\", \"Person\", function(object) object@name)
Account <- R6Class(\"Account\",
  public = list(
    balance = 0,
    deposit = function(amount) invisible(self)
  ),
  private = list(audit = function() NULL),
  active = list(status = function() \"open\")
)
plain <- function(x, ...) x
setMethod(f = \"baz\", signature = \"Person\", definition = function(x) x)
setMethod(\"qux\", c(\"Person\", \"Other\"), function(x, y) x)
setMethod(f = \"foo\", signature = list(x = \"Person\", y = \"Other\"), definition = function(x, y) x)
setMethod(\"bar\", signature(\"Person\", y = \"Other\"), function(x, y) x)
";
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    let files = ProjectFiles::new(&db, vec![file]);

    let symbols = document_symbols(&db, file);
    let rendered: Vec<String> = symbols
        .iter()
        .map(|symbol| {
            format!(
                "{} ({:?}{})",
                symbol.name,
                symbol.kind,
                symbol
                    .detail
                    .as_deref()
                    .map(|detail| format!(", {detail}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        rendered,
        vec![
            "Person (S4Class)",
            "greet (S4Generic)",
            "greet (S4Method, Person)",
            "Account (R6Class)",
            "plain (Function, fn(x, ...))",
            "baz (S4Method, Person)",
            "qux (S4Method, Person, Other)",
            "foo (S4Method, Person, Other)",
            "bar (S4Method, Person, Other)",
        ],
        "{symbols:#?}"
    );

    let account = &symbols[3];
    let members: Vec<String> = account
        .children
        .iter()
        .map(|child| format!("{} ({:?})", child.name, child.kind))
        .collect();
    assert_eq!(
        members,
        vec![
            "balance (R6Field)",
            "deposit (R6Method)",
            "audit (R6Method)",
            "status (R6Field)",
        ],
        "{account:#?}"
    );

    assert_eq!(symbol_names(&db, files, "deposit"), ["deposit"]);
}

fn symbol_names(db: &RootDatabase, files: ProjectFiles, query: &str) -> Vec<String> {
    workspace_symbols(db, files, query)
        .into_iter()
        .map(|symbol| symbol.name)
        .collect()
}

// Completion renders every global candidate's type, so unlike the point queries it re-checks the
// one global an edit changed.
#[test]
fn point_queries_recheck_only_what_an_edit_changed() {
    let mut db = ProbeDatabase::default();
    let source = "helper <- function(x) x + 1L\nuse <- function() helper(2L)\n";
    let files = db.project(&[source, "other <- function() 3L\n"]);
    let project = ProjectFiles::get(&db);
    let at = |needle: &str| TextSize::from(source.find(needle).expect("needle") as u32);
    let point_queries = |db: &ProbeDatabase| {
        let file = files[0];
        hover(db, project, file, at("helper(2L)"));
        signature_help(db, file, at("2L)"));
        inlay_hints(db, file, None);
        definition(db, project, file, at("helper(2L)"));
    };
    let complete = |db: &ProbeDatabase| {
        completion(db, project, files[0], at("helper(2L)"));
    };
    point_queries(&db);
    complete(&db);
    db.checked_since();

    point_queries(&db);
    complete(&db);
    assert_eq!(db.checked_since(), Vec::<String>::new());

    db.edit(files[1], "other <- function() 4L\n");
    point_queries(&db);
    assert_eq!(db.checked_since(), Vec::<String>::new());
    complete(&db);
    assert_eq!(db.checked_since(), ["other"]);

    db.edit(files[0], source.replace("2L", "3L"));
    point_queries(&db);
    complete(&db);
    assert_eq!(db.checked_since(), ["use"]);
}

#[test]
fn ide_fixtures() {
    run_fixture_suite(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ide"),
        &render,
    );
}
