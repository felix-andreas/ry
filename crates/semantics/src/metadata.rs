//! `NAMESPACE` and `DESCRIPTION` facts. `NAMESPACE` is R call syntax and parses with the
//! ordinary grammar; `DESCRIPTION` is DCF.

use crate::stubs::{META_PACKAGE_MEMBERS, namespace_exports, namespace_known};
use crate::{Db, SourceFile, parse};
use std::collections::BTreeSet;
use syntax::ast::is_expression_kind;
use syntax::{SyntaxKind, SyntaxNode, TextRange, matching_nodes};

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
                namespace_known(db, namespace) == Some(true)
                    && namespace_exports(db, namespace, name)
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
        Some(namespace.as_str()) != own && namespace_known(db, namespace) != Some(true)
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
        || META_PACKAGE_MEMBERS
            .iter()
            .any(|(meta, members)| members.contains(&package) && named(meta))
}

/// Parses every file passed in, so call it where the workspace is parsed anyway.
pub fn attached_union(
    db: &dyn Db,
    files: impl IntoIterator<Item = SourceFile>,
) -> BTreeSet<String> {
    files
        .into_iter()
        .flat_map(|file| file_attached_namespaces(db, file))
        .collect()
}

/// Purely syntactic: a local binding shadowing `library` is not honored, which can only
/// over-activate, and activation only adds resolution.
#[salsa::tracked(returns(clone))]
pub fn file_attached_namespaces(db: &dyn Db, file: SourceFile) -> BTreeSet<String> {
    let mut attached = BTreeSet::new();
    let calls = matching_nodes(&parse(db, file).syntax_node(), |node, _| {
        node.kind() == SyntaxKind::CALL_EXPR
            && node
                .child_nodes()
                .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST)
                .is_some_and(|callee| {
                    callee.kind() == SyntaxKind::NAME
                        && matches!(
                            callee.single_token_text(),
                            Some("library" | "require" | "requireNamespace" | "loadNamespace")
                        )
                })
    });
    for node in calls {
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
                .filter(|child| is_expression_kind(child.kind()))
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
    let mut imports = Vec::new();
    for (directive, values) in namespace_directives(source) {
        match directive.as_str() {
            "import" => imports.extend(values.iter().filter_map(name_argument).map(
                |(namespace, range)| NamespaceImport {
                    namespace,
                    name: None,
                    range,
                },
            )),
            "importFrom" => {
                let Some((namespace, _)) = values.first().and_then(name_argument) else {
                    continue;
                };
                imports.extend(values[1..].iter().filter_map(name_argument).map(
                    |(name, range)| NamespaceImport {
                        namespace: namespace.clone(),
                        name: Some(name),
                        range,
                    },
                ));
            }
            _ => {}
        }
    }
    imports
}

/// Only explicit `export()` names: `exportPattern` is a load-time regex, and
/// `exportClasses`/`exportMethods`/`S3method` name S4 and S3 entities, not bindings.
pub fn parse_namespace_exports(source: &str) -> Vec<(String, TextRange)> {
    namespace_directives(source)
        .into_iter()
        .filter(|(directive, _)| directive == "export")
        .flat_map(|(_, values)| values)
        .filter_map(|value| name_argument(&value))
        .collect()
}

/// Each top-level call with a bare-name callee, with its argument values. A named argument
/// (`except = ...`) puts its name first; the value is the last expression child.
fn namespace_directives(source: &str) -> Vec<(String, Vec<SyntaxNode>)> {
    let mut directives = Vec::new();
    for node in syntax::parse(source).syntax_node().children() {
        if node.kind() != SyntaxKind::CALL_EXPR {
            continue;
        }
        let Some(callee) = node
            .children()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST)
            .filter(|callee| callee.kind() == SyntaxKind::NAME)
        else {
            continue;
        };
        let values = node
            .children()
            .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
            .map(|list| {
                list.children()
                    .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
                    .filter_map(|argument| {
                        argument
                            .children()
                            .filter(|child| is_expression_kind(child.kind()))
                            .last()
                    })
                    .collect()
            })
            .unwrap_or_default();
        directives.push((callee.text().to_string(), values));
    }
    directives
}

/// Everything a loader reads from `NAMESPACE`, in directive order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NamespaceSpec {
    pub imports: Vec<NamespaceImportDirective>,
    pub exports: Vec<String>,
    pub export_patterns: Vec<String>,
    pub export_classes: Vec<String>,
    pub export_class_patterns: Vec<String>,
    pub export_methods: Vec<String>,
    pub s3_methods: Vec<S3MethodDirective>,
    pub dynamic_libraries: Vec<DynamicLibraryDirective>,
    /// Directives a loader cannot honor statically, such as an `if` around other directives
    /// (R evaluates the condition at load time), rendered as their source text.
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NamespaceImportDirective {
    /// `import(pkg, except = c(...))`.
    Namespace {
        package: String,
        except: Vec<String>,
    },
    /// `importFrom(pkg, ...)`.
    Names { package: String, names: Vec<String> },
    /// `importClassesFrom(pkg, ...)`.
    Classes { package: String, names: Vec<String> },
    /// `importMethodsFrom(pkg, ...)`.
    Methods { package: String, names: Vec<String> },
}

/// `S3method(generic, class, function)`. The generic may be qualified (`pkg::generic`), which
/// R registers once that package loads.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct S3MethodDirective {
    pub generic: String,
    pub class: String,
    /// The method's binding, `generic.class` unless the directive names another.
    pub function: String,
}

/// `useDynLib(library, symbols..., .registration = , .fixes = )`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DynamicLibraryDirective {
    pub library: String,
    pub registration: bool,
    pub fixes: String,
    /// `(binding, symbol)`: `useDynLib(lib, alias = symbol)` binds `alias`.
    pub symbols: Vec<(String, String)>,
}

/// Directives R would reject are skipped: R validates `NAMESPACE` syntax itself when it
/// installs the package.
pub fn parse_namespace_spec(source: &str) -> NamespaceSpec {
    let mut spec = NamespaceSpec::default();
    for node in syntax::parse(source).syntax_node().children() {
        if !is_expression_kind(node.kind()) {
            continue;
        }
        let Some(Call {
            callee: directive,
            qualified: false,
            arguments,
        }) = call_parts(&node)
        else {
            spec.unsupported.push(node.text().to_string());
            continue;
        };
        let positional = || {
            arguments
                .iter()
                .filter(|argument| argument.name.is_none())
                .filter_map(|argument| name_argument(&argument.value).map(|(name, _)| name))
        };
        let named = |wanted: &str| {
            arguments
                .iter()
                .find(|argument| argument.name.as_deref() == Some(wanted))
                .map(|argument| &argument.value)
        };
        match directive.as_str() {
            "import" => {
                let except = named("except").map(name_list).unwrap_or_default();
                for package in positional() {
                    spec.imports.push(NamespaceImportDirective::Namespace {
                        package,
                        except: except.clone(),
                    });
                }
            }
            "importFrom" | "importClassesFrom" | "importMethodsFrom" => {
                let mut values = positional();
                let Some(package) = values.next() else {
                    continue;
                };
                let names: Vec<String> = values.collect();
                spec.imports.push(match directive.as_str() {
                    "importFrom" => NamespaceImportDirective::Names { package, names },
                    "importClassesFrom" => NamespaceImportDirective::Classes { package, names },
                    _ => NamespaceImportDirective::Methods { package, names },
                });
            }
            "export" => spec.exports.extend(positional()),
            "exportPattern" => spec.export_patterns.extend(positional()),
            "exportClasses" | "exportClass" => spec.export_classes.extend(positional()),
            "exportClassPattern" => spec.export_class_patterns.extend(positional()),
            "exportMethods" => spec.export_methods.extend(positional()),
            "S3method" => {
                let values: Vec<String> = positional().collect();
                let (Some(generic), Some(class)) = (values.first(), values.get(1)) else {
                    continue;
                };
                let bare_generic = generic.rsplit("::").next().unwrap_or(generic);
                spec.s3_methods.push(S3MethodDirective {
                    generic: generic.clone(),
                    class: class.clone(),
                    function: values
                        .get(2)
                        .cloned()
                        .unwrap_or_else(|| format!("{bare_generic}.{class}")),
                });
            }
            "useDynLib" => {
                let Some((library, symbols)) = arguments.split_first() else {
                    continue;
                };
                let Some((library, _)) = name_argument(&library.value) else {
                    continue;
                };
                let flag = |wanted: &str| {
                    named(wanted).is_some_and(|value| {
                        matches!(value.text().to_string().trim(), "TRUE" | "T")
                    })
                };
                let fixes = named(".fixes")
                    .and_then(name_argument)
                    .map(|(fixes, _)| fixes)
                    .unwrap_or_default();
                let symbols = symbols
                    .iter()
                    .filter(|argument| {
                        !argument
                            .name
                            .as_deref()
                            .is_some_and(|name| name.starts_with('.'))
                    })
                    .filter_map(|argument| {
                        let (symbol, _) = name_argument(&argument.value)?;
                        let binding = argument.name.clone().unwrap_or_else(|| symbol.clone());
                        Some((binding, symbol))
                    })
                    .collect();
                spec.dynamic_libraries.push(DynamicLibraryDirective {
                    library,
                    registration: flag(".registration"),
                    fixes,
                    symbols,
                });
            }
            _ => spec.unsupported.push(node.text().to_string()),
        }
    }
    spec
}

/// A call as the loader and `NAMESPACE` read it.
pub struct Call {
    /// The function name, without a `pkg::` qualifier.
    pub callee: String,
    pub qualified: bool,
    pub arguments: Vec<CallArgument>,
}

pub struct CallArgument {
    pub name: Option<String>,
    pub value: SyntaxNode,
}

/// `callee(...)` or `pkg::callee(...)`; anything else is not a call this reads.
pub fn call_parts(node: &SyntaxNode) -> Option<Call> {
    if node.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let callee = node.children().next()?;
    let (callee, qualified) = match callee.kind() {
        SyntaxKind::NAME => (callee, false),
        SyntaxKind::NAMESPACE_EXPR => (
            callee
                .children()
                .filter(|child| child.kind() == SyntaxKind::NAME)
                .last()?,
            true,
        ),
        _ => return None,
    };
    let arguments = node
        .children()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
        .map(|list| {
            list.children()
                .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
                .filter_map(|argument| {
                    let value = argument
                        .children()
                        .filter(|child| is_expression_kind(child.kind()))
                        .last()?;
                    Some(CallArgument {
                        name: argument_name(&argument),
                        value,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Call {
        callee: unquote(&callee.text().to_string()),
        qualified,
        arguments,
    })
}

/// The `name` of a `name = value` argument, unquoted. A bare `value` has none.
pub fn argument_name(argument: &SyntaxNode) -> Option<String> {
    let mut seen_name = None;
    for element in argument.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::EQ => {
                return seen_name;
            }
            rowan::NodeOrToken::Token(token) if token.kind().is_trivia() => {}
            rowan::NodeOrToken::Token(token) => {
                seen_name = Some(unquote(token.text()));
            }
            rowan::NodeOrToken::Node(node) => {
                seen_name = name_argument(&node).map(|(name, _)| name);
            }
        }
    }
    None
}

/// `c(a, "b")` or a single name, as `except =` takes.
fn name_list(value: &SyntaxNode) -> Vec<String> {
    if let Some((name, _)) = name_argument(value) {
        return vec![name];
    }
    match call_parts(value) {
        Some(call) if call.callee == "c" => call
            .arguments
            .iter()
            .filter_map(|argument| name_argument(&argument.value).map(|(name, _)| name))
            .collect(),
        _ => Vec::new(),
    }
}

fn unquote(text: &str) -> String {
    let stripped = text
        .strip_prefix('`')
        .and_then(|text| text.strip_suffix('`'))
        .or_else(|| {
            text.strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
        })
        .or_else(|| {
            text.strip_prefix('\'')
                .and_then(|text| text.strip_suffix('\''))
        });
    stripped.unwrap_or(text).to_owned()
}

/// One `DESCRIPTION` field, with continuation lines folded into single spaces.
pub fn description_field(source: &str, field: &str) -> Option<String> {
    dcf_fields(source)
        .into_iter()
        .find(|(name, _)| *name == field)
        .map(|(_, value)| value)
}

/// The packages `Depends` attaches, without `R` and without version requirements.
pub fn parse_description_depends(source: &str) -> Vec<String> {
    description_field(source, "Depends")
        .map(|depends| {
            depends
                .split(',')
                .map(|entry| {
                    entry
                        .split('(')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                })
                .filter(|name| !name.is_empty() && name != "R")
                .collect()
        })
        .unwrap_or_default()
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
        SyntaxKind::NAME => Some((unquote(&value.text().to_string()), value.text_range())),
        SyntaxKind::LITERAL => {
            let token = value
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.kind() == SyntaxKind::STRING)?;
            Some((string_content(token.text()), value.text_range()))
        }
        // `S3method(pkg::generic, class)` names a generic in another namespace.
        SyntaxKind::NAMESPACE_EXPR => Some((value.text().to_string(), value.text_range())),
        _ => None,
    }
}

/// A string literal's value: quotes removed and R's escapes resolved, so `"^[^\\.]"` is the
/// regex `^[^\.]`. An escape this does not know keeps its backslash.
pub fn string_content(literal: &str) -> String {
    let inner = literal
        .strip_prefix(['"', '\''])
        .and_then(|inner| inner.strip_suffix(['"', '\'']))
        .unwrap_or(literal);
    let mut out = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(escaped @ ('\\' | '"' | '\'' | '`' | ' ')) => out.push(escaped),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
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
