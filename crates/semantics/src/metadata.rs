//! `NAMESPACE` and `DESCRIPTION` facts. `NAMESPACE` is R call syntax and parses with the
//! ordinary grammar; `DESCRIPTION` is DCF.

use crate::Db;
use std::collections::BTreeSet;
use syntax::{SyntaxKind, SyntaxNode, TextRange};

/// Absent (no metadata files, single-file analysis) means no imports and no dependencies.
#[salsa::input(singleton, debug)]
pub struct PackageMetadata {
    /// `None` for `import(pkg)`, `Some(name)` for `importFrom(pkg, name)`. Hosts sort and dedupe
    /// it so formatting edits do not invalidate.
    #[returns(ref)]
    pub imports: Vec<(String, Option<String>)>,
    #[returns(ref)]
    pub dependencies: BTreeSet<String>,
    /// Namespaces any project file attaches with a `library()`-family call: the activation
    /// signal for scripts, which have no metadata files.
    #[returns(ref)]
    pub attached: BTreeSet<String>,
    /// Attaching or importing the project's own package tolerates nothing: its exports are the
    /// definitions already in view.
    #[returns(ref)]
    pub package: Option<String>,
}

pub fn imported_bare(db: &dyn Db, name: &str) -> bool {
    imported_by_name(db, name) || imports_every_name(db)
}

/// Knowledge about `name` itself, unlike the blanket tolerance of [`imports_every_name`].
pub fn imported_by_name(db: &dyn Db, name: &str) -> bool {
    let Some(metadata) = PackageMetadata::try_get(db) else {
        return false;
    };
    metadata
        .imports(db)
        .iter()
        .any(|(namespace, imported)| match imported {
            Some(imported) => imported == name,
            None => {
                crate::stubs::namespace_known(db, namespace) == Some(true)
                    && crate::stubs::namespace_exports(db, namespace, name)
            }
        })
}

/// A whole-namespace import or `library()` of a package no stub describes makes every bare read
/// tolerable. The project's own package is excluded: testthat's `library(yourpkg)` would otherwise
/// disable unresolved detection, and its exports are already in view.
pub fn imports_every_name(db: &dyn Db) -> bool {
    let Some(metadata) = PackageMetadata::try_get(db) else {
        return false;
    };
    let own = metadata.package(db).as_deref();
    let unknowable = |namespace: &String| {
        Some(namespace.as_str()) != own
            && crate::stubs::namespace_known(db, namespace) != Some(true)
    };
    metadata
        .imports(db)
        .iter()
        .any(|(namespace, imported)| imported.is_none() && unknowable(namespace))
        || metadata.attached(db).iter().any(unknowable)
}

pub fn is_own_package(db: &dyn Db, package: &str) -> bool {
    PackageMetadata::try_get(db)
        .is_some_and(|metadata| metadata.package(db).as_deref() == Some(package))
}

pub fn declared_dependency(db: &dyn Db, package: &str) -> bool {
    let Some(metadata) = PackageMetadata::try_get(db) else {
        return false;
    };
    metadata.dependencies(db).contains(package)
        || metadata
            .imports(db)
            .iter()
            .any(|(namespace, _)| namespace == package)
}

/// Whether a conditional stub namespace applies. Reads only the metadata input, so stub assembly
/// stays free of per-file dependencies.
pub fn namespace_active(db: &dyn Db, package: &str) -> bool {
    let named = |name: &str| {
        declared_dependency(db, name)
            || PackageMetadata::try_get(db)
                .is_some_and(|metadata| metadata.attached(db).contains(name))
    };
    named(package)
        // A meta-package attaches its members rather than re-exporting them, so
        // `library(tidyverse)` activates them too.
        || crate::stubs::META_PACKAGE_MEMBERS
            .iter()
            .any(|(meta, members)| members.contains(&package) && named(meta))
}

/// Parses every file passed in, so call it where the workspace is parsed anyway.
pub fn attached_union(
    db: &dyn Db,
    files: impl IntoIterator<Item = crate::SourceFile>,
) -> BTreeSet<String> {
    files
        .into_iter()
        .flat_map(|file| file_attached_namespaces(db, file))
        .collect()
}

/// Purely syntactic: a local binding shadowing `library` is not honored, which can only
/// over-activate, and activation only adds resolution.
#[salsa::tracked(returns(clone))]
pub fn file_attached_namespaces(db: &dyn Db, file: crate::SourceFile) -> BTreeSet<String> {
    let parse = crate::parse(db, file);
    let mut attached = BTreeSet::new();
    for node in parse.syntax_node().descendants() {
        if node.kind() != SyntaxKind::CALL_EXPR {
            continue;
        }
        let Some(callee) = node
            .children()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST)
        else {
            continue;
        };
        if callee.kind() != SyntaxKind::NAME
            || !matches!(
                callee.text().to_string().as_str(),
                "library" | "require" | "requireNamespace" | "loadNamespace"
            )
        {
            continue;
        }
        let first_positional = node
            .children()
            .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
            .and_then(|list| {
                list.children()
                    .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
                    .find(|argument| {
                        !argument
                            .children_with_tokens()
                            .filter_map(|element| element.into_token())
                            .any(|token| token.kind() == SyntaxKind::EQ)
                    })
            });
        if let Some(argument) = first_positional
            && let Some(value) = argument
                .children()
                .filter(|child| syntax::ast::is_expression_kind(child.kind()))
                .last()
            && let Some((namespace, _)) = name_argument(&value)
        {
            attached.insert(namespace);
        }
    }
    attached
}

pub struct NamespaceImport {
    pub namespace: String,
    /// `None` for a whole-namespace `import(pkg)`.
    pub name: Option<String>,
    pub range: TextRange,
}

/// Directives R would reject are skipped, not reported: R itself validates NAMESPACE syntax.
pub fn parse_namespace_imports(source: &str) -> Vec<NamespaceImport> {
    let parse = syntax::parse(source);
    let mut imports = Vec::new();
    for node in parse.syntax_node().children() {
        if node.kind() != SyntaxKind::CALL_EXPR {
            continue;
        }
        let Some(callee) = node
            .children()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST)
        else {
            continue;
        };
        if callee.kind() != SyntaxKind::NAME {
            continue;
        }
        // A named argument (`except = ...`) puts its name first; the value is the last
        // expression child.
        let values: Vec<SyntaxNode> = node
            .children()
            .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
            .map(|list| {
                list.children()
                    .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
                    .filter_map(|argument| {
                        argument
                            .children()
                            .filter(|child| syntax::ast::is_expression_kind(child.kind()))
                            .last()
                    })
                    .collect()
            })
            .unwrap_or_default();
        match callee.text().to_string().as_str() {
            "import" => {
                for value in values {
                    if let Some((namespace, range)) = name_argument(&value) {
                        imports.push(NamespaceImport {
                            namespace,
                            name: None,
                            range,
                        });
                    }
                }
            }
            "importFrom" => {
                let mut values = values.into_iter();
                let Some((namespace, _)) = values.next().and_then(|value| name_argument(&value))
                else {
                    continue;
                };
                for value in values {
                    if let Some((name, range)) = name_argument(&value) {
                        imports.push(NamespaceImport {
                            namespace: namespace.clone(),
                            name: Some(name),
                            range,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    imports
}

/// Only explicit `export()` names: `exportPattern` is a load-time regex, and
/// `exportClasses`/`exportMethods`/`S3method` name S4 and S3 entities, not bindings.
pub fn parse_namespace_exports(source: &str) -> Vec<(String, TextRange)> {
    let parse = syntax::parse(source);
    let mut exports = Vec::new();
    for node in parse.syntax_node().children() {
        if node.kind() != SyntaxKind::CALL_EXPR {
            continue;
        }
        let Some(callee) = node
            .children()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST)
        else {
            continue;
        };
        if callee.kind() != SyntaxKind::NAME || callee.text() != "export" {
            continue;
        }
        let values = node
            .children()
            .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
            .map(|list| {
                list.children()
                    .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
                    .filter_map(|argument| {
                        argument
                            .children()
                            .filter(|child| syntax::ast::is_expression_kind(child.kind()))
                            .last()
                    })
                    .collect::<Vec<SyntaxNode>>()
            })
            .unwrap_or_default();
        for value in values {
            if let Some((name, range)) = name_argument(&value) {
                exports.push((name, range));
            }
        }
    }
    exports
}

/// Sorted and deduplicated, so reordering or reformatting directives does not invalidate queries.
pub fn normalized_imports(imports: &[NamespaceImport]) -> Vec<(String, Option<String>)> {
    let set: BTreeSet<(String, Option<String>)> = imports
        .iter()
        .map(|import| (import.namespace.clone(), import.name.clone()))
        .collect();
    set.into_iter().collect()
}

/// `R` in `Depends` is a version pin, not a package.
pub fn parse_description_dependencies(source: &str) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    for (field, text) in dcf_fields(source) {
        if !matches!(field, "Depends" | "Imports" | "Suggests" | "Enhances") {
            continue;
        }
        for entry in text.split(',') {
            let name = entry.split('(').next().unwrap_or_default().trim();
            if name.is_empty() || name == "R" {
                continue;
            }
            dependencies.insert(name.to_owned());
        }
    }
    dependencies
}

pub fn parse_description_package(source: &str) -> Option<String> {
    dcf_fields(source)
        .into_iter()
        .find(|(field, _)| *field == "Package")
        .map(|(_, name)| name)
        .filter(|name| !name.is_empty())
}

/// `Collate.unix` applies only when plain `Collate` is absent; Windows collation is not modeled.
pub fn parse_description_collate(source: &str) -> Vec<String> {
    let mut collate = Vec::new();
    let mut unix_collate = Vec::new();
    for (field, text) in dcf_fields(source) {
        let target = match field {
            "Collate" => &mut collate,
            "Collate.unix" => &mut unix_collate,
            _ => continue,
        };
        target.extend(
            text.split_whitespace()
                .map(|entry| entry.trim_matches(['"', '\'']).to_owned())
                .filter(|name| !name.is_empty()),
        );
    }
    if collate.is_empty() {
        unix_collate
    } else {
        collate
    }
}

fn dcf_fields(source: &str) -> Vec<(&str, String)> {
    let mut fields: Vec<(&str, String)> = Vec::new();
    for line in source.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((field, rest)) = line.split_once(':') {
            fields.push((field.trim(), rest.trim().to_owned()));
        }
    }
    fields
}

/// R accepts a bare name or a string literal for a NAMESPACE directive argument.
fn name_argument(value: &SyntaxNode) -> Option<(String, TextRange)> {
    match value.kind() {
        SyntaxKind::NAME => Some((value.text().to_string(), value.text_range())),
        SyntaxKind::LITERAL => {
            let token = value
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.kind() == SyntaxKind::STRING)?;
            let text = token.text();
            let content = text
                .trim_start_matches(['"', '\''])
                .trim_end_matches(['"', '\'']);
            Some((content.to_owned(), value.text_range()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_dependencies_parse_dcf_fields() {
        let dependencies = parse_description_dependencies(
            "Package: demo\nDepends:\n    R (>= 4.0),\n    data.table (>= 1.14)\nImports: dplyr,\n    rlang (>= 1.0)\nSuggests: testthat\nTitle: A demo\n",
        );
        let names: Vec<&str> = dependencies.iter().map(String::as_str).collect();
        assert_eq!(names, ["data.table", "dplyr", "rlang", "testthat"]);
    }

    #[test]
    fn description_without_dependency_fields_is_empty() {
        let dependencies =
            parse_description_dependencies("Package: demo\nTitle: Imports: not-a-field\n");
        assert!(dependencies.is_empty());
    }

    #[test]
    fn collate_preserves_declared_order() {
        let collate = parse_description_collate(
            "Package: demo\nCollate:\n    'zzz.R'\n    \"aaa.R\"\n    mmm.R\nTitle: demo\n",
        );
        assert_eq!(collate, ["zzz.R", "aaa.R", "mmm.R"]);
    }

    #[test]
    fn collate_unix_applies_only_without_plain_collate() {
        let unix_only = parse_description_collate("Collate.unix: 'b.R' 'a.R'\n");
        assert_eq!(unix_only, ["b.R", "a.R"]);
        let both = parse_description_collate("Collate: 'x.R'\nCollate.unix: 'y.R'\n");
        assert_eq!(both, ["x.R"]);
    }
}
