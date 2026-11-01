//! Load-index fixtures: which items a loader binds lazily, what eager code reads and writes,
//! the registrations it makes, and the `NAMESPACE` directives a load honors.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use {
    semantics::{
        DocumentKind, RootDatabase, SourceFile,
        load_index::{PackageFile, PackageIndex, PackageSources, package_index},
        metadata::NamespaceImportDirective,
    },
    std::path::PathBuf,
    syntax::testing::{run_fixture_suite, split_files},
};

#[test]
fn load_index_fixtures() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/load_index");
    run_fixture_suite(&suite, &render);
}

const DEFAULT_DESCRIPTION: &str = "Package: demo\nVersion: 0.1.0\n";

fn render(source: &str) -> String {
    let files =
        split_files(source).unwrap_or_else(|| vec![("R/a.R".to_owned(), source.to_owned())]);
    let db = RootDatabase::default();
    let mut namespace = String::new();
    let mut description = DEFAULT_DESCRIPTION.to_owned();
    let mut package_files = Vec::new();
    for (path, text) in files {
        match path.as_str() {
            "NAMESPACE" => namespace = text,
            "DESCRIPTION" => description = text,
            _ => package_files.push(PackageFile {
                file: SourceFile::new(&db, text, DocumentKind::Package),
                path,
            }),
        }
    }
    let sources = PackageSources::new(&db, package_files, namespace, description);
    render_index(&package_index(&db, sources))
}

fn render_index(index: &PackageIndex) -> String {
    let mut output = format!("package {} {}\n", index.package, index.version);
    if !index.depends.is_empty() {
        output.push_str(&format!("depends {}\n", index.depends.join(" ")));
    }
    let namespace = &index.namespace;
    for import in &namespace.imports {
        output.push_str(&match import {
            NamespaceImportDirective::Namespace { package, except } if except.is_empty() => {
                format!("import {package}\n")
            }
            NamespaceImportDirective::Namespace { package, except } => {
                format!("import {package} except {}\n", except.join(" "))
            }
            NamespaceImportDirective::Names { package, names } => {
                format!("import from {package}: {}\n", names.join(" "))
            }
            NamespaceImportDirective::Classes { package, names } => {
                format!("import classes from {package}: {}\n", names.join(" "))
            }
            NamespaceImportDirective::Methods { package, names } => {
                format!("import methods from {package}: {}\n", names.join(" "))
            }
        });
    }
    let lists = [
        ("export", &namespace.exports),
        ("export pattern", &namespace.export_patterns),
        ("export classes", &namespace.export_classes),
        ("export class pattern", &namespace.export_class_patterns),
        ("export methods", &namespace.export_methods),
    ];
    for (label, names) in lists {
        if !names.is_empty() {
            output.push_str(&format!("{label} {}\n", names.join(" ")));
        }
    }
    for method in &namespace.s3_methods {
        output.push_str(&format!(
            "S3 method {} for {} is {}\n",
            method.generic, method.class, method.function
        ));
    }
    for library in &namespace.dynamic_libraries {
        let symbols: Vec<String> = library
            .symbols
            .iter()
            .map(|(binding, symbol)| format!("{binding}={symbol}"))
            .collect();
        output.push_str(&format!(
            "dynamic library {} registration={} fixes={:?} symbols [{}]\n",
            library.library,
            library.registration,
            library.fixes,
            symbols.join(" ")
        ));
    }
    for directive in &namespace.unsupported {
        output.push_str(&format!("unsupported {directive}\n"));
    }
    for file in &index.files {
        output.push_str(&format!("== {}\n", file.path));
        for (line, message) in &file.items.syntax_errors {
            output.push_str(&format!("syntax error {line}: {message}\n"));
        }
        for item in &file.items.items {
            let position = format!("{}:{}", item.span.line, item.span.column);
            let mut line = match index.lazy_name(item) {
                Some(name) => {
                    let function = item.function.as_ref().map(|(_, span)| span);
                    let at = function.map_or(String::new(), |span| {
                        format!(" function at {}:{}", span.line, span.column)
                    });
                    format!("{position} lazy {name}{at}")
                }
                None => {
                    let mut line = format!("{position} eager");
                    if !item.binds.is_empty() {
                        line.push_str(&format!(" binds [{}]", item.binds.join(" ")));
                    }
                    if !item.reads.is_empty() {
                        line.push_str(&format!(" reads [{}]", item.reads.join(" ")));
                    }
                    line
                }
            };
            for registration in &item.registrations {
                line.push_str(&format!(" registers {registration:?}"));
            }
            output.push_str(&line);
            output.push('\n');
        }
    }
    output
}
