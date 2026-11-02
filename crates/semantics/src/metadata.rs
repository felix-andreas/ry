//! `NAMESPACE` and `DESCRIPTION` facts. `NAMESPACE` is R call syntax and parses with the
//! ordinary grammar; `DESCRIPTION` is DCF.

use {
    crate::{
        Db, SiteRole, SourceFile, file_syntax_sites, parse,
        stubs::{META_PACKAGE_MEMBERS, namespace_exports, namespace_known},
    },
    std::{collections::BTreeSet, num::NonZero, thread},
    syntax::{GreenRef, SyntaxKind, SyntaxNode, TextRange, TextSize, ast::is_expression_kind},
};

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
    #[returns(ref)]
    pub native_symbols: NativeSymbols,
}

/// The bindings `NAMESPACE`'s `useDynLib` directives create in the package namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NativeSymbols {
    /// Each listed symbol's binding, its `.fixes` prefix included: `useDynLib(pkg, foo,
    /// .fixes = "C_")` binds `C_foo`.
    pub names: BTreeSet<String>,
    /// The non-empty `.fixes` prefixes of registering directives. The routines a package
    /// registers live in its compiled code, so any name with one of these prefixes may be one.
    pub registered_prefixes: BTreeSet<String>,
    /// Whether any directive has `.registration = TRUE`, which binds every registered routine.
    pub registers: bool,
}

/// What the checker reads from `NAMESPACE`, all from one parse: a generated `NAMESPACE` can
/// run to a megabyte, and parsing it is serial work before and after the parallel passes.
#[derive(Default)]
pub struct NamespaceFile {
    pub imports: Vec<NamespaceImport>,
    /// Only explicit `export()` names: `exportPattern` is a load-time regex, and
    /// `exportClasses`/`exportMethods`/`S3method` name S4 and S3 entities, not bindings.
    pub exports: Vec<(String, TextRange)>,
    pub native_symbols: NativeSymbols,
}

/// A large file parses in chunks on separate threads. Every directive is self-contained, so
/// the chunks' facts concatenate in order to exactly what one parse would give.
pub fn parse_namespace(source: &str) -> NamespaceFile {
    let workers = thread::available_parallelism().map_or(1, NonZero::get);
    if source.len() < PARALLEL_NAMESPACE_BYTES || workers == 1 {
        return parse_namespace_chunk(source, TextSize::from(0));
    }
    parse_namespace_in_chunks(source, &call_boundaries(source, workers))
}

/// Below this, one parse is faster than starting threads.
const PARALLEL_NAMESPACE_BYTES: usize = 64 * 1024;

fn parse_namespace_in_chunks(source: &str, chunks: &[usize]) -> NamespaceFile {
    if chunks.len() == 1 {
        return parse_namespace_chunk(source, TextSize::from(0));
    }
    let ends = chunks.iter().skip(1).copied().chain([source.len()]);
    let parts: Vec<NamespaceFile> = thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .iter()
            .zip(ends)
            .map(|(&start, end)| {
                let offset = TextSize::from(start as u32);
                scope.spawn(move || parse_namespace_chunk(&source[start..end], offset))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(part) => part,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    });
    let mut file = NamespaceFile::default();
    for part in parts {
        file.imports.extend(part.imports);
        file.exports.extend(part.exports);
        let symbols = part.native_symbols;
        file.native_symbols.names.extend(symbols.names);
        file.native_symbols
            .registered_prefixes
            .extend(symbols.registered_prefixes);
        file.native_symbols.registers |= symbols.registers;
    }
    file
}

fn parse_namespace_chunk(source: &str, offset: TextSize) -> NamespaceFile {
    let root = syntax::parse(source).syntax_node();
    let directives = namespace_directives(&root);
    let mut imports = namespace_imports(&directives);
    for import in &mut imports {
        import.range += offset;
    }
    NamespaceFile {
        imports,
        exports: directives
            .iter()
            .filter(|(directive, _)| directive == "export")
            .flat_map(|(_, values)| values)
            .filter_map(name_argument)
            .map(|(name, range)| (name, range + offset))
            .collect(),
        native_symbols: native_symbols(namespace_spec_of(&root)),
    }
}

/// Start offsets of up to `parts` chunks, the first at 0. A chunk may start only on a line
/// that opens with `name(` right after a line holding nothing but a complete top-level
/// `name(...)` call. The parser ends a top-level expression at such a newline whatever came
/// before it, while a looser rule would split an `if (cond)` from its body on the next line.
fn call_boundaries(source: &str, parts: usize) -> Vec<usize> {
    use SyntaxKind::*;
    let (tokens, _) = syntax::lex(source);
    let significant: Vec<(SyntaxKind, usize)> = tokens
        .iter()
        .scan(0usize, |offset, token| {
            let start = *offset;
            *offset += usize::from(token.len);
            Some((token.kind, start))
        })
        .filter(|(kind, _)| !matches!(kind, WHITESPACE | COMMENT))
        .collect();
    let target = source.len() / parts;
    let mut boundaries = vec![0];
    // `simple` holds while the current line is exactly one call: a name, then a parenthesis
    // that `open` follows back to empty.
    let mut open: Vec<SyntaxKind> = Vec::new();
    let mut simple = true;
    let mut closed = false;
    for (index, &(kind, _)) in significant.iter().enumerate() {
        let at_line_start = index == 0 || significant[index - 1].0 == NEWLINE;
        match kind {
            NEWLINE if open.is_empty() => {
                let next = significant.get(index + 1..index + 3);
                if let (true, Some([(IDENT, start), (L_PAREN, _)])) = (simple && closed, next) {
                    let due = boundaries.len() * target;
                    if *start >= due && boundaries.len() < parts {
                        boundaries.push(*start);
                    }
                }
                simple = true;
                closed = false;
                continue;
            }
            NEWLINE => continue,
            L_PAREN | L_BRACE | L_BRACKET => open.push(kind),
            L_BRACKET2 => open.extend([L_BRACKET, L_BRACKET]),
            R_PAREN | R_BRACE | R_BRACKET => {
                let opener = match kind {
                    R_PAREN => L_PAREN,
                    R_BRACE => L_BRACE,
                    _ => L_BRACKET,
                };
                // A stray or mismatched closer leaves the parser's recovery, not bracket
                // matching, to decide where a call ends, so only one parse is exact.
                if open.pop() != Some(opener) {
                    return vec![0];
                }
            }
            _ => {}
        }
        let depth = open.len();
        let call_start = at_line_start && kind == IDENT;
        let call_open = index > 0
            && kind == L_PAREN
            && depth == 1
            && significant[index - 1].0 == IDENT
            && (index == 1 || significant[index - 2].0 == NEWLINE);
        if closed || !(call_start || call_open || depth > 0 || kind == R_PAREN) {
            simple = false;
        }
        if kind == R_PAREN && depth == 0 {
            closed = true;
        }
    }
    boundaries
}

fn native_symbols(spec: NamespaceSpec) -> NativeSymbols {
    let mut symbols = NativeSymbols::default();
    for library in spec.dynamic_libraries {
        for (binding, _) in &library.symbols {
            symbols.names.insert(format!("{}{binding}", library.fixes));
        }
        if library.registration {
            symbols.registers = true;
            if !library.fixes.is_empty() {
                symbols.registered_prefixes.insert(library.fixes);
            }
        }
    }
    symbols
}

/// Whether a `useDynLib` directive may bind `name`. A registering directive binds names that
/// only the compiled code knows, so a bare name read as a native interface's routine resolves
/// (`native_routine`), and so does any name with a registered `.fixes` prefix.
pub fn native_symbol(db: &dyn Db, name: &str, native_routine: bool) -> bool {
    let Some(metadata) = PackageMetadata::try_get(db) else {
        return false;
    };
    let symbols = metadata.native_symbols(db);
    symbols.names.contains(name)
        || (native_routine && symbols.registers)
        || symbols
            .registered_prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
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
    let root = parse(db, file).syntax_node();
    let calls = file_syntax_sites(db, file).nodes(&root, SiteRole::AttachCall);
    for node in calls {
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

pub(crate) fn is_green_attach_call(node: GreenRef<'_>) -> bool {
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
}

pub struct NamespaceImport {
    pub namespace: String,
    /// `None` for a whole-namespace `import(pkg)`.
    pub name: Option<String>,
    pub range: TextRange,
}

/// Directives R would reject are skipped, not reported: R itself validates NAMESPACE syntax.
fn namespace_imports(directives: &[(String, Vec<SyntaxNode>)]) -> Vec<NamespaceImport> {
    let mut imports = Vec::new();
    for (directive, values) in directives {
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

/// Each top-level call with a bare-name callee, with its argument values. A named argument
/// (`except = ...`) puts its name first; the value is the last expression child.
fn namespace_directives(root: &SyntaxNode) -> Vec<(String, Vec<SyntaxNode>)> {
    let mut directives = Vec::new();
    for node in root.children() {
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
    namespace_spec_of(&syntax::parse(source).syntax_node())
}

fn namespace_spec_of(root: &SyntaxNode) -> NamespaceSpec {
    let mut spec = NamespaceSpec::default();
    for node in root.children() {
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

/// The operating system's name as R's `.Platform$OS.type` gives it, which names both the
/// `R/` subdirectory whose code files a package loads and its `Collate.<os>` field.
pub const OS_TYPE: &str = if cfg!(windows) { "windows" } else { "unix" };

/// The collation field R CMD INSTALL reads: `Collate.unix` (or `Collate.windows`) when present,
/// else `Collate`. Entries are paths relative to `R/`, so an entry may name `unix/a.R`.
pub fn parse_description_collate(source: &str) -> Vec<String> {
    let os_field = format!("Collate.{OS_TYPE}");
    let mut collate = None;
    let mut os_collate = None;
    for (field, text) in dcf_fields(source) {
        let target = if field == os_field {
            &mut os_collate
        } else if field == "Collate" {
            &mut collate
        } else {
            continue;
        };
        *target = Some(
            text.split_whitespace()
                .map(|entry| entry.trim_matches(['"', '\'']).to_owned())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>(),
        );
    }
    os_collate.or(collate).unwrap_or_default()
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

    fn namespace_facts(file: &NamespaceFile) -> String {
        let imports: Vec<_> = file
            .imports
            .iter()
            .map(|import| (&import.namespace, &import.name, import.range))
            .collect();
        format!("{imports:?} {:?} {:?}", file.exports, file.native_symbols)
    }

    #[test]
    fn chunked_namespace_parse_matches_one_parse() {
        let mut source = String::from("# generated\nimport(stats)\n");
        for index in 0..200 {
            source.push_str(&format!("export(f{index})\n"));
            match index % 25 {
                3 => {
                    source.push_str("if (getRversion() >= \"4.0\")\nexport(late)\nexport(after)\n")
                }
                7 => source.push_str("importFrom(utils,\n  head, tail)\n"),
                11 => source.push_str("x <- function()\nexport(body)\n"),
                13 => source.push_str("useDynLib(pkg, foo, .fixes = \"C_\")\n"),
                17 => source.push_str("export(\"quoted\n name\")\n\n# note\n"),
                19 => source.push_str("f(a)(b)\nexport(g)\n"),
                _ => {}
            }
        }
        let whole = namespace_facts(&parse_namespace_chunk(&source, TextSize::from(0)));
        for parts in [2, 3, 7, 50] {
            let chunks = call_boundaries(&source, parts);
            assert!(chunks.len() > 1, "{parts} parts found no boundary");
            assert_eq!(
                namespace_facts(&parse_namespace_in_chunks(&source, &chunks)),
                whole
            );
        }
    }

    #[test]
    fn unbalanced_namespace_parses_in_one_chunk() {
        for prefix in ["}\nf(\n", "f(]\n", "g(a))\nf(\n"] {
            let mut source = String::from(prefix);
            for index in 0..200 {
                source.push_str(&format!("export(f{index})\n"));
            }
            assert_eq!(call_boundaries(&source, 4), [0], "{prefix:?}");
        }
    }

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
    #[cfg(not(windows))]
    fn the_os_collation_field_takes_precedence_over_plain_collate() {
        let unix_only = parse_description_collate("Collate.unix: 'b.R' 'a.R'\n");
        assert_eq!(unix_only, ["b.R", "a.R"]);
        let both = parse_description_collate("Collate: 'x.R'\nCollate.unix: 'unix/y.R' 'x.R'\n");
        assert_eq!(both, ["unix/y.R", "x.R"]);
        let other_os = parse_description_collate("Collate: 'x.R'\nCollate.windows: 'w.R'\n");
        assert_eq!(other_os, ["x.R"]);
    }
}
