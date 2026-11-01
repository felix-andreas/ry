//! `.Rtypes` stubs: `name : <type>` lines whose type half goes through the `#:` annotation
//! grammar. Stub text is a set-once input, so it never participates in per-edit invalidation.

use crate::Db;
use crate::annotations::{NamedDefinition, lower_annotation};
use crate::metadata::namespace_active;
use crate::types::{Name, Ty, TyKind, TypeScheme, for_each_child_type};
use rustc_hash::{FxHashMap, FxHashSet};
use syntax::{SyntaxKind, TextRange};

/// Sources are in precedence order: a later source replaces a name's whole overload set.
/// Manifest names resolve even without a typed declaration, as `Unknown`.
#[salsa::input(singleton, debug)]
pub struct StubSources {
    #[returns(ref)]
    pub sources: Vec<(String, String)>,
    #[returns(ref)]
    pub manifests: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct StubLibrary<'db> {
    pub schemes: FxHashMap<String, Vec<TypeScheme<'db>>>,
    pub nominals: FxHashSet<String>,
    /// `@alias` and generic `@type` declarations, which the checker resolves exactly like a
    /// project's own; a project definition of the same name shadows one.
    pub type_definitions: FxHashMap<Name<'db>, NamedDefinition<'db>>,
    /// Data-masking variadics, mapped to the formals before `...` (which resolve normally). An
    /// empty list masks every argument.
    pub masked: FxHashMap<String, Vec<String>>,
    /// Declaration-level: a later source overriding a name's type does not un-export it from the
    /// namespace that declared it.
    pub exports_by_namespace: FxHashMap<String, FxHashSet<String>>,
    /// The winning declaration site per name; for an overload set, the first candidate.
    pub declarations: FxHashMap<String, StubDeclaration>,
    /// Manifest-listed exports of the active namespaces; these always resolve, untyped ones as
    /// `Unknown`.
    pub known_exports: FxHashSet<String>,
}

/// `source_index` indexes the `StubSources` order the host fed in.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct StubDeclaration {
    pub source_index: usize,
    pub range: TextRange,
}

pub fn stub_declaration<'db>(db: &'db dyn Db, name: &str) -> Option<&'db StubDeclaration> {
    stubs(db)?.declarations.get(name)
}

/// `None` when no corpus is installed, so callers skip validation.
pub fn namespace_known(db: &dyn Db, package: &str) -> Option<bool> {
    stubs(db).map(|library| library.exports_by_namespace.contains_key(package))
}

/// The last declaring source in precedence order owns the name.
pub fn declaring_namespace<'db>(db: &'db dyn Db, name: &str) -> Option<&'db str> {
    let sources = StubSources::try_get(db)?;
    let library = stub_library(db, sources);
    sources.sources(db).iter().rev().find_map(|(namespace, _)| {
        library
            .exports_by_namespace
            .get(namespace)
            .is_some_and(|names| names.contains(name))
            .then_some(namespace.as_str())
    })
}

pub fn namespace_exports(db: &dyn Db, package: &str, name: &str) -> bool {
    stubs(db).is_some_and(|library| {
        library
            .exports_by_namespace
            .get(package)
            .is_some_and(|names| names.contains(name))
    })
}

/// The namespace of ry's standard library (`Result.map`, `Dict.get`, ...). Its functions are
/// written for ry's records, tuples and tags, so passing one of those values to them is not the
/// boundary error it is for an R function, and its `Dict` and `Set` types are native values.
pub const STANDARD_LIBRARY: &str = "ry";

/// Whether `name` is a type the standard library declares, rather than one a project or package
/// spells the same way.
pub fn is_ry_type(db: &dyn Db, name: &str) -> bool {
    declaring_namespace(db, name) == Some(STANDARD_LIBRARY)
}

/// Embedded from top-level `types/`, not `stubs/`: a workspace-root `stubs/` is the
/// project-override convention and would load as overrides when ry analyzes this repository.
pub fn shipped_stub_sources() -> Vec<(String, String)> {
    [
        ("base", include_str!("../../../types/base.Rtypes")),
        ("stats", include_str!("../../../types/stats.Rtypes")),
        ("utils", include_str!("../../../types/utils.Rtypes")),
        ("methods", include_str!("../../../types/methods.Rtypes")),
        ("graphics", include_str!("../../../types/graphics.Rtypes")),
        ("grDevices", include_str!("../../../types/grDevices.Rtypes")),
        ("datasets", include_str!("../../../types/datasets.Rtypes")),
        (
            "data.table",
            include_str!("../../../types/data.table.Rtypes"),
        ),
        ("dplyr", include_str!("../../../types/dplyr.Rtypes")),
        ("ggplot2", include_str!("../../../types/ggplot2.Rtypes")),
        ("testthat", include_str!("../../../types/testthat.Rtypes")),
        (STANDARD_LIBRARY, include_str!("../../../types/ry.Rtypes")),
    ]
    .into_iter()
    .map(|(namespace, text)| (namespace.to_owned(), text.to_owned()))
    .collect()
}

/// Generated from a real R session by `scripts/export-manifests.R`.
pub fn shipped_export_manifests() -> Vec<(String, String)> {
    [
        ("base", include_str!("../../../types/base.exports")),
        ("stats", include_str!("../../../types/stats.exports")),
        ("utils", include_str!("../../../types/utils.exports")),
        ("methods", include_str!("../../../types/methods.exports")),
        ("graphics", include_str!("../../../types/graphics.exports")),
        (
            "grDevices",
            include_str!("../../../types/grDevices.exports"),
        ),
        ("datasets", include_str!("../../../types/datasets.exports")),
        ("ry", include_str!("../../../types/ry.exports")),
        ("tools", include_str!("../../../types/tools.exports")),
        ("parallel", include_str!("../../../types/parallel.exports")),
        ("compiler", include_str!("../../../types/compiler.exports")),
        ("grid", include_str!("../../../types/grid.exports")),
        ("splines", include_str!("../../../types/splines.exports")),
        ("stats4", include_str!("../../../types/stats4.exports")),
        ("tcltk", include_str!("../../../types/tcltk.exports")),
        (
            "data.table",
            include_str!("../../../types/data.table.exports"),
        ),
        ("dplyr", include_str!("../../../types/dplyr.exports")),
        ("ggplot2", include_str!("../../../types/ggplot2.exports")),
        ("testthat", include_str!("../../../types/testthat.exports")),
        // Manifest-only: every name is `Unknown`, but a knowable export set keeps unresolved-name
        // detection alive in projects that attach them.
        ("tibble", include_str!("../../../types/tibble.exports")),
        ("tidyr", include_str!("../../../types/tidyr.exports")),
        ("readr", include_str!("../../../types/readr.exports")),
        ("purrr", include_str!("../../../types/purrr.exports")),
        ("stringr", include_str!("../../../types/stringr.exports")),
        ("forcats", include_str!("../../../types/forcats.exports")),
        (
            "lubridate",
            include_str!("../../../types/lubridate.exports"),
        ),
        ("magrittr", include_str!("../../../types/magrittr.exports")),
        ("rlang", include_str!("../../../types/rlang.exports")),
        ("glue", include_str!("../../../types/glue.exports")),
        ("scales", include_str!("../../../types/scales.exports")),
        ("knitr", include_str!("../../../types/knitr.exports")),
        ("jsonlite", include_str!("../../../types/jsonlite.exports")),
        ("R6", include_str!("../../../types/R6.exports")),
        (
            "tidyverse",
            include_str!("../../../types/tidyverse.exports"),
        ),
    ]
    .into_iter()
    .map(|(namespace, text)| (namespace.to_owned(), text.to_owned()))
    .collect()
}

/// Join the library only when the project declares or attaches the package, so their names never
/// resolve (or mask a typo) elsewhere. A manifest-only entry still matters: attaching a package
/// with an unknowable export set disables unresolved-name detection project-wide.
pub const CONDITIONAL_NAMESPACES: &[&str] = &[
    "data.table",
    "dplyr",
    "ggplot2",
    "testthat",
    "tibble",
    "tidyr",
    "readr",
    "purrr",
    "stringr",
    "forcats",
    "lubridate",
    "magrittr",
    "rlang",
    "glue",
    "scales",
    "knitr",
    "jsonlite",
    "R6",
    "tidyverse",
];

/// Meta-packages and the packages they attach rather than re-export, so `library(tidyverse)`
/// activates the members' typed declarations. `scripts/export-manifests.R` prints the membership.
pub const META_PACKAGE_MEMBERS: &[(&str, &[&str])] = &[(
    "tidyverse",
    &[
        "dplyr",
        "forcats",
        "ggplot2",
        "lubridate",
        "purrr",
        "readr",
        "stringr",
        "tibble",
        "tidyr",
    ],
)];

/// Shipped with R but not on the default search path: `pkg::` reads always validate, bare reads
/// need the package attached or declared.
pub const QUALIFIED_ONLY_NAMESPACES: &[&str] = &[
    "compiler", "grid", "parallel", "splines", "stats4", "tcltk", "tools",
];

#[salsa::tracked(returns(ref))]
pub fn stub_library<'db>(db: &'db dyn Db, sources: StubSources) -> StubLibrary<'db> {
    // A namespace declared by several sources has a project override, and writing
    // `stubs/dplyr.Rtypes` declares use of the package, so it activates like metadata would.
    let mut seen_namespaces: FxHashSet<&str> = FxHashSet::default();
    let mut project_overridden: FxHashSet<&str> = FxHashSet::default();
    for (namespace, _) in sources.sources(db) {
        if !seen_namespaces.insert(namespace.as_str()) {
            project_overridden.insert(namespace.as_str());
        }
    }
    let mut library = StubLibrary::default();
    for (source_index, (namespace, text)) in sources.sources(db).iter().enumerate() {
        if CONDITIONAL_NAMESPACES.contains(&namespace.as_str())
            && !project_overridden.contains(namespace.as_str())
            && !namespace_active(db, namespace)
        {
            continue;
        }
        let namespace_exports = library
            .exports_by_namespace
            .entry(namespace.clone())
            .or_default();
        // A name repeated within this source appends candidates; its first occurrence here
        // replaces any earlier source's set.
        let mut seen_here: FxHashSet<&str> = FxHashSet::default();
        let mut line_start = 0usize;
        for raw_segment in text.split_inclusive('\n') {
            let raw_line = raw_segment.trim_end_matches(['\n', '\r']);
            let this_line_start = line_start;
            line_start += raw_segment.len();
            let content = strip_comment(raw_line).trim();
            if content.is_empty() {
                continue;
            }
            // Only whitespace or `@type` precede the name, so its first occurrence is the name token.
            let name_range = |name: &str| {
                strip_comment(raw_line).find(name).map(|at| {
                    let start = (this_line_start + at) as u32;
                    TextRange::new(start.into(), (start + name.len() as u32).into())
                })
            };
            if let Some(definition) = type_definition_line(db, content) {
                let name = definition.name.text(db).to_owned();
                library.nominals.insert(name.clone());
                namespace_exports.insert(name.clone());
                if let Some(range) = name_range(&name) {
                    library.declarations.insert(
                        name,
                        StubDeclaration {
                            source_index,
                            range,
                        },
                    );
                }
                library.type_definitions.insert(definition.name, definition);
                continue;
            }
            if let Some(rest) = content.strip_prefix("@type") {
                let name = rest.trim();
                if is_nominal_name(name) {
                    library.nominals.insert(name.to_owned());
                    namespace_exports.insert(name.to_owned());
                    if let Some(range) = name_range(name) {
                        library.declarations.insert(
                            name.to_owned(),
                            StubDeclaration {
                                source_index,
                                range,
                            },
                        );
                    }
                }
                continue;
            }
            let Some(separator) = top_level_colon(content) else {
                continue;
            };
            let name = content[..separator].trim();
            let mut type_text = content[separator + 1..].trim();
            if name.is_empty() || !is_stub_name(name) {
                continue;
            }
            let masked = if let Some(rest) = type_text.strip_prefix("@masked") {
                type_text = rest.trim_start();
                true
            } else {
                false
            };
            let Some(scheme) = lower_type_text(db, type_text) else {
                continue;
            };
            if masked && let Some(leading) = masked_leading_formals(db, &scheme) {
                library.masked.insert(name.to_owned(), leading);
            }
            namespace_exports.insert(name.to_owned());
            if seen_here.insert(name) {
                library.schemes.insert(name.to_owned(), vec![scheme]);
                if let Some(range) = name_range(name) {
                    library.declarations.insert(
                        name.to_owned(),
                        StubDeclaration {
                            source_index,
                            range,
                        },
                    );
                }
            } else if let Some(candidates) = library.schemes.get_mut(name) {
                candidates.push(scheme);
            }
        }
    }
    for (namespace, text) in sources.manifests(db) {
        let active =
            project_overridden.contains(namespace.as_str()) || namespace_active(db, namespace);
        if CONDITIONAL_NAMESPACES.contains(&namespace.as_str()) && !active {
            continue;
        }
        // Qualified-only namespaces always validate `pkg::` reads, but their names become bare
        // reads only once the project attaches or declares the package, as in R.
        let bare_visible = !QUALIFIED_ONLY_NAMESPACES.contains(&namespace.as_str()) || active;
        let namespace_exports = library
            .exports_by_namespace
            .entry(namespace.clone())
            .or_default();
        for line in text.lines() {
            let name = strip_comment(line).trim();
            // Control keywords are exported primitives that can never be referenced as names, so
            // keeping them would only pollute typo suggestions.
            const CONTROL_KEYWORDS: &[&str] = &[
                "if", "else", "repeat", "while", "function", "for", "next", "break", "in",
            ];
            if name.is_empty() || CONTROL_KEYWORDS.contains(&name) {
                continue;
            }
            namespace_exports.insert(name.to_owned());
            if bare_visible {
                library.known_exports.insert(name.to_owned());
            }
        }
    }
    library
}

/// `None` when the scheme is not variadic: `stub_source_problems` reports that, and the loader
/// skips the mask.
fn masked_leading_formals(db: &dyn Db, scheme: &TypeScheme<'_>) -> Option<Vec<String>> {
    let TyKind::Function(function) = scheme.body.kind(db) else {
        return None;
    };
    let variadic = function.variadic.as_ref()?;
    let leading = function.named.get(..variadic.preceding_named)?;
    Some(
        leading
            .iter()
            .map(|field| field.name.text(db).to_owned())
            .collect(),
    )
}

pub fn stubs<'db>(db: &'db dyn Db) -> Option<&'db StubLibrary<'db>> {
    StubSources::try_get(db).map(|sources| stub_library(db, sources))
}

/// `line` is zero-based. Editor diagnostics and `ry check` both render from this, so the wording
/// stays identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubProblem {
    pub line: usize,
    pub message: String,
}

/// The vocabulary includes this source's own `@type` declarations: it may be an unsaved buffer
/// that is not installed yet.
pub fn stub_source_problems(db: &dyn Db, text: &str) -> Vec<StubProblem> {
    let mut known_nominals: FxHashSet<String> = stubs(db)
        .map(|library| library.nominals.iter().cloned().collect())
        .unwrap_or_default();
    for raw_line in text.lines() {
        let content = strip_comment(raw_line).trim();
        if let Some(name) = definition_line_name(content) {
            known_nominals.insert(name.to_owned());
        } else if let Some(rest) = content.strip_prefix("@type") {
            let name = rest.trim();
            if is_nominal_name(name) {
                known_nominals.insert(name.to_owned());
            }
        }
    }

    let mut problems = Vec::new();
    for (line, raw_line) in text.lines().enumerate() {
        let content = strip_comment(raw_line).trim();
        if content.is_empty() {
            continue;
        }
        if definition_line_name(content).is_some() {
            match type_definition_line(db, content) {
                Some(definition) => {
                    let mut unknown = Vec::new();
                    collect_unknown_nominals(db, definition.body, &known_nominals, &mut unknown);
                    for unknown_name in unknown {
                        if !definition
                            .parameters
                            .iter()
                            .any(|parameter| parameter.text(db) == unknown_name)
                        {
                            problems.push(StubProblem {
                                line,
                                message: format!(
                                    "this declaration does not load: I do not know the type `{unknown_name}`."
                                ),
                            });
                        }
                    }
                }
                None => problems.push(StubProblem {
                    line,
                    message: "this type declaration does not load: write `@alias NAME<T> {TYPE}` or `@type NAME<T>`.".to_owned(),
                }),
            }
            continue;
        }
        if let Some(rest) = content.strip_prefix("@type") {
            let name = rest.trim();
            if name.is_empty() {
                problems.push(StubProblem {
                    line,
                    message: "expected a type name after `@type`.".to_owned(),
                });
            } else if !is_nominal_name(name) {
                problems.push(StubProblem {
                    line,
                    message: format!("`{name}` is not a valid type name."),
                });
            }
            continue;
        }
        let Some(separator) = top_level_colon(content) else {
            problems.push(StubProblem {
                line,
                message: "this line is not a declaration (`name : TYPE`).".to_owned(),
            });
            continue;
        };
        let name = content[..separator].trim();
        let mut type_text = content[separator + 1..].trim();
        if name.is_empty() || !is_stub_name(name) {
            problems.push(StubProblem {
                line,
                message: if name.is_empty() {
                    "expected a name before `:`.".to_owned()
                } else {
                    format!("`{name}` is not a valid declaration name.")
                },
            });
            continue;
        }
        if type_text.is_empty() {
            problems.push(StubProblem {
                line,
                message: format!("expected a type after `{name} :`."),
            });
            continue;
        }
        let mut masked = false;
        if let Some(rest) = type_text.strip_prefix("@masked") {
            type_text = rest.trim_start();
            masked = true;
        }
        let Some(scheme) = lower_type_text(db, type_text) else {
            problems.push(StubProblem {
                line,
                message: format!(
                    "this declaration does not load: `{type_text}` is not a valid type."
                ),
            });
            continue;
        };
        if masked
            && !matches!(
                scheme.body.kind(db),
                TyKind::Function(function) if function.variadic.is_some()
            )
        {
            problems.push(StubProblem {
                line,
                message: format!(
                    "`@masked` on `{name}` requires a variadic function type. The mask covers \
                     the arguments the `...` rest parameter absorbs."
                ),
            });
            continue;
        }
        let mut unknown = Vec::new();
        collect_unknown_nominals(db, scheme.body, &known_nominals, &mut unknown);
        for unknown_name in unknown {
            problems.push(StubProblem {
                line,
                message: format!(
                    "this declaration does not load: I do not know the type `{unknown_name}`."
                ),
            });
        }
    }
    problems
}

fn collect_unknown_nominals<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    known: &FxHashSet<String>,
    out: &mut Vec<String>,
) {
    if let TyKind::Named(name, _) = ty.kind(db) {
        let text = name.text(db);
        if !known.contains(text) && !out.iter().any(|seen| seen == text) {
            out.push(text.to_owned());
        }
    }
    for_each_child_type(db, ty, &mut |child| {
        collect_unknown_nominals(db, child, known, out)
    });
}

/// The declared name of an `@alias` line or a generic `@type NAME<T, ...>` line. A plain
/// `@type NAME` is the older opaque form, handled on its own.
fn definition_line_name(content: &str) -> Option<&str> {
    let rest = content
        .strip_prefix("@alias")
        .or_else(|| content.strip_prefix("@type").filter(|rest| rest.contains('<')))?;
    let rest = rest.trim_start();
    let end = rest.find(['<', ' ', '{']).unwrap_or(rest.len());
    let name = &rest[..end];
    is_nominal_name(name).then_some(name)
}

/// An `@alias NAME<T> {TYPE}` or `@type NAME<T>` line, lowered through the annotation grammar a
/// project's own `#: @alias` uses. A generic opaque type is a `@type` whose representation is
/// `Unknown`, which the checker already treats as having none.
fn type_definition_line<'db>(db: &'db dyn Db, content: &str) -> Option<NamedDefinition<'db>> {
    definition_line_name(content)?;
    let text = if content.starts_with("@type") {
        format!("#: {content} {{Unknown}}")
    } else {
        format!("#: {content}")
    };
    let parse = syntax::parse(&text);
    if !parse.errors().is_empty() {
        return None;
    }
    let annotation = parse
        .syntax_node()
        .children()
        .find(|node| node.kind() == SyntaxKind::ANNOTATION)?;
    let mut lowered = lower_annotation(db, &annotation);
    if !lowered.errors.is_empty() || lowered.definitions.len() != 1 {
        return None;
    }
    lowered.definitions.pop()
}

/// Routes through the annotation pipeline, the single type grammar.
fn lower_type_text<'db>(db: &'db dyn Db, type_text: &str) -> Option<TypeScheme<'db>> {
    let parse = syntax::parse(&format!("#: {type_text}"));
    if !parse.errors().is_empty() {
        return None;
    }
    let root = parse.syntax_node();
    let annotation = root
        .children()
        .find(|node| node.kind() == SyntaxKind::ANNOTATION)?;
    lower_annotation(db, &annotation).declared
}

fn strip_comment(line: &str) -> &str {
    // `#` cannot occur inside a declaration type.
    match line.find('#') {
        Some(at) => &line[..at],
        None => line,
    }
}

/// Depth-zero only, so `list[a: b]` keeps its interior colon.
fn top_level_colon(content: &str) -> Option<usize> {
    let mut depth: usize = 0;
    for (byte_index, character) in content.char_indices() {
        match character {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some(byte_index),
            _ => {}
        }
    }
    None
}

/// S3 operator methods (`+.Date`) are declarable: they are how a stub says a nominal supports an
/// operator.
fn is_stub_name(name: &str) -> bool {
    if is_nominal_name(name) {
        return true;
    }
    if let Some(body) = name
        .strip_prefix('%')
        .and_then(|rest| rest.strip_suffix('%'))
    {
        return !body.is_empty() && !body.contains('%');
    }
    OPERATOR_METHOD_PREFIXES
        .iter()
        .filter_map(|operator| name.strip_prefix(operator))
        .any(|suffix| suffix.strip_prefix('.').is_some_and(is_nominal_name))
}

/// A nominal's name is a plain R identifier. The operator spellings a method
/// declaration may carry are names for values, never for types.
fn is_nominal_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| !first.is_ascii_digit())
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '.' || character == '_')
}

/// Longest first, so `%/%.difftime` is not read as `%%` followed by `/`.
const OPERATOR_METHOD_PREFIXES: [&str; 13] = [
    "%/%", "%%", "<=", ">=", "==", "!=", "+", "-", "*", "/", "^", "<", ">",
];

pub fn install_shipped_stubs(db: &dyn Db) -> StubSources {
    StubSources::new(db, shipped_stub_sources(), shipped_export_manifests())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{ItemCheck, TypeErrorKind};
    use crate::diagnostics::file_diagnostics;
    use crate::metadata::{PackageMetadata, attached_union};
    use crate::types::Atomic;
    use crate::{
        DocumentKind, ProjectFiles, RootDatabase, SourceFile, item_check, item_tree,
        package_scheme_exists,
    };
    use salsa::Setter;

    #[test]
    fn shipped_corpus_loads() {
        let db = RootDatabase::default();
        let sources = install_shipped_stubs(&db);
        let library = stub_library(&db, sources);
        assert!(
            library.schemes.len() > 400,
            "expected the full shipped corpus, got {} names",
            library.schemes.len()
        );
        assert!(library.nominals.contains("data.frame"));
        assert_eq!(
            library.schemes["sum"].len(),
            3,
            "sum keeps its ordered overload candidates"
        );
        assert_eq!(library.schemes["length"].len(), 1);
    }

    #[test]
    fn standard_library_stub_loads_cleanly() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let text = include_str!("../../../types/ry.Rtypes");
        let problems = stub_source_problems(&db, text);
        assert!(problems.is_empty(), "{problems:?}");
        let library = stubs(&db).expect("the shipped stubs are installed");
        assert!(library.schemes.contains_key("Result.map"));
        assert!(library.type_definitions.keys().any(|name| name.text(&db) == "Result"));
        assert_eq!(declaring_namespace(&db, "Dict"), Some(STANDARD_LIBRARY));
    }

    #[test]
    fn a_type_name_must_be_an_identifier() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let problems = stub_source_problems(
            &db,
            "@type structural { a: integer }\nmk : fn() -> structural\n",
        );
        assert_eq!(problems[0].line, 0);
        assert_eq!(
            problems[0].message,
            "`structural { a: integer }` is not a valid type name."
        );
        // The nominal is refused, so the use below reports too, and an author
        // who reads only the second error is not sent to a correct line.
        assert_eq!(problems[1].line, 1);
    }

    /// A dispatch target for operator lookup (`+.Date`, `Compare.Date`), not an exported name.
    fn is_operator_method_name(name: &str) -> bool {
        const GROUP_GENERICS: [&str; 3] = ["Arith.", "Compare.", "Ops."];
        GROUP_GENERICS
            .iter()
            .any(|group| name.starts_with(group) && name.len() > group.len())
            || OPERATOR_METHOD_PREFIXES
                .iter()
                .filter_map(|operator| name.strip_prefix(operator))
                .any(|suffix| suffix.starts_with('.') && suffix.len() > 1)
    }

    #[test]
    fn every_declaration_is_a_real_export() {
        // `@type` nominals name classes, not exported bindings, and conditional namespaces may
        // override base names (data.table's class-preserving `merge`).
        let db = RootDatabase::default();
        PackageMetadata::new(
            &db,
            Vec::new(),
            CONDITIONAL_NAMESPACES
                .iter()
                .map(|namespace| (*namespace).to_owned())
                .collect(),
            Default::default(),
            None,
            Default::default(),
        );
        let sources = StubSources::new(&db, shipped_stub_sources(), Vec::new());
        let library = stub_library(&db, sources);
        let manifests: FxHashMap<String, FxHashSet<String>> = shipped_export_manifests()
            .into_iter()
            .map(|(namespace, text)| {
                (
                    namespace,
                    text.lines()
                        .map(|line| strip_comment(line).trim().to_owned())
                        .filter(|name| !name.is_empty())
                        .collect(),
                )
            })
            .collect();
        let empty = FxHashSet::default();
        let base_manifest = manifests.get("base").unwrap_or(&empty);
        let mut misplaced = Vec::new();
        for (namespace, declared) in &library.exports_by_namespace {
            let Some(manifest) = manifests.get(namespace) else {
                continue;
            };
            for name in declared {
                if library.nominals.contains(name) || manifest.contains(name.as_str()) {
                    continue;
                }
                // R registers operator methods like `Compare.Date` without exporting them.
                if is_operator_method_name(name) {
                    continue;
                }
                if CONDITIONAL_NAMESPACES.contains(&namespace.as_str())
                    && base_manifest.contains(name.as_str())
                {
                    continue;
                }
                misplaced.push(format!("{namespace}::{name}"));
            }
        }
        misplaced.sort();
        assert!(
            misplaced.is_empty(),
            "stub declarations that are not exports of their namespace: {misplaced:?}"
        );
    }

    #[test]
    fn manifest_exports_resolve_without_a_typed_declaration() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        // `bitwAnd` (base) and `aggregate` (stats) are manifest-only names.
        assert!(package_scheme_exists(&db, "bitwAnd"));
        assert!(package_scheme_exists(&db, "aggregate"));
        let file = SourceFile::new(
            &db,
            "f <- function(x, y) bitwAnd(x, y)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        let diagnostics = file_diagnostics(&db, file);
        assert!(
            diagnostics.is_empty(),
            "manifest exports must resolve cleanly: {diagnostics:?}"
        );
    }

    #[test]
    fn qualified_only_namespaces_gate_bare_reads() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        assert!(namespace_exports(&db, "tools", "file_ext"));
        let file = SourceFile::new(
            &db,
            "f <- function(p) tools::file_ext(p)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        assert!(file_diagnostics(&db, file).is_empty());
        assert!(!package_scheme_exists(&db, "mclapply"));
        let active = RootDatabase::default();
        install_shipped_stubs(&active);
        PackageMetadata::new(
            &active,
            Vec::new(),
            Default::default(),
            ["parallel".to_owned()].into(),
            None,
            Default::default(),
        );
        assert!(package_scheme_exists(&active, "mclapply"));
    }

    #[test]
    fn datasets_objects_are_bare_visible() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        assert!(package_scheme_exists(&db, "iris"));
        assert!(package_scheme_exists(&db, "state.x77"));
        let file = SourceFile::new(
            &db,
            "widths <- function() iris\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        assert!(file_diagnostics(&db, file).is_empty());
    }

    #[test]
    fn conditional_manifest_names_stay_dark_without_activation() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        // `alloc.col` is a data.table manifest name with no typed declaration.
        assert!(!package_scheme_exists(&db, "alloc.col"));
        let active = RootDatabase::default();
        install_shipped_stubs(&active);
        PackageMetadata::new(
            &active,
            Vec::new(),
            ["data.table".to_owned()].into(),
            Default::default(),
            None,
            Default::default(),
        );
        assert!(package_scheme_exists(&active, "alloc.col"));
    }

    #[test]
    fn declarations_record_winning_sites() {
        let db = RootDatabase::default();
        let sources = install_shipped_stubs(&db);
        let library = stub_library(&db, sources);
        let declaration = &library.declarations["print"];
        let (namespace, text) = &sources.sources(&db)[declaration.source_index];
        assert_eq!(namespace, "base");
        let range = usize::from(declaration.range.start())..usize::from(declaration.range.end());
        assert_eq!(&text[range], "print");
        let nominal = &library.declarations["data.frame"];
        let (_, nominal_text) = &sources.sources(&db)[nominal.source_index];
        let range = usize::from(nominal.range.start())..usize::from(nominal.range.end());
        assert_eq!(&nominal_text[range], "data.frame");
    }

    #[test]
    fn conditional_namespace_is_dark_without_activation() {
        let db = RootDatabase::default();
        let sources = install_shipped_stubs(&db);
        let library = stub_library(&db, sources);
        assert!(!library.schemes.contains_key("fread"));
        assert!(!library.nominals.contains("data.table"));
        assert!(!library.exports_by_namespace.contains_key("data.table"));
    }

    #[test]
    fn declared_dependency_activates_a_conditional_namespace() {
        let db = RootDatabase::default();
        let sources = install_shipped_stubs(&db);
        PackageMetadata::new(
            &db,
            Vec::new(),
            ["data.table".to_owned()].into(),
            Default::default(),
            None,
            Default::default(),
        );
        let library = stub_library(&db, sources);
        assert!(library.schemes.contains_key("fread"));
        assert!(library.nominals.contains("data.table"));
    }

    #[test]
    fn library_call_attaches_a_conditional_namespace() {
        let mut db = RootDatabase::default();
        let sources = install_shipped_stubs(&db);
        let metadata = PackageMetadata::new(
            &db,
            Vec::new(),
            Default::default(),
            Default::default(),
            None,
            Default::default(),
        );
        let file = SourceFile::new(
            &db,
            "library(data.table)\nprint(fread(\"x.csv\"))\n".to_owned(),
            DocumentKind::Script,
        );
        ProjectFiles::new(&db, vec![file]);
        let attached = attached_union(&db, [file]);
        assert_eq!(
            attached.iter().collect::<Vec<_>>(),
            ["data.table"],
            "the library() call must be scanned"
        );
        metadata.set_attached(&mut db).to(attached);
        let library = stub_library(&db, sources);
        assert!(library.schemes.contains_key("fread"));
        let diagnostics = file_diagnostics(&db, file);
        assert!(
            diagnostics.is_empty(),
            "an attached conditional namespace resolves its exports: {diagnostics:?}"
        );
    }

    #[test]
    fn stub_names_resolve_in_package_files() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let file = SourceFile::new(
            &db,
            "total <- function(x) sum(x, na.rm = TRUE)\nsize <- function(x) length(x)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        let diagnostics = file_diagnostics(&db, file);
        assert!(
            diagnostics.is_empty(),
            "stub names must resolve cleanly: {diagnostics:?}"
        );
    }

    #[test]
    fn stub_signature_mismatch_reports() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let file = SourceFile::new(
            &db,
            "find_x <- function(x) grepl(1L, x)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        let diagnostics = file_diagnostics(&db, file);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "type-mismatch" && d.message.contains("character")),
            "expected a mismatch against grepl's character pattern: {diagnostics:?}"
        );
    }

    #[test]
    fn nominal_names_count_as_resolvable() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        assert!(package_scheme_exists(&db, "data.frame"));
        assert!(package_scheme_exists(&db, "sum"));
        assert!(!package_scheme_exists(&db, "definitely_not_a_name"));
    }

    fn first_item_check<'db>(db: &'db RootDatabase, source: &str) -> ItemCheck<'db> {
        let file = SourceFile::new(db, source.to_owned(), DocumentKind::Package);
        ProjectFiles::new(db, vec![file]);
        let item = item_tree(db, file)[0];
        item_check(db, item).expect("item check")
    }

    fn scheme_return<'db>(db: &'db RootDatabase, check: &ItemCheck<'db>) -> Ty<'db> {
        let scheme = check.scheme.as_ref().expect("definition scheme");
        let TyKind::Function(function) = scheme.body.kind(db) else {
            panic!("expected a function scheme");
        };
        function.ret
    }

    #[test]
    fn overload_selection_picks_the_specific_candidate() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let check = first_item_check(&db, "f <- function() cumsum(1L)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let ret = scheme_return(&db, &check);
        let TyKind::Vector(element) = ret.kind(&db) else {
            panic!("expected integer[], got {:?}", ret.kind(&db));
        };
        assert!(matches!(element.kind(&db), TyKind::Scalar(Atomic::Integer)));
    }

    #[test]
    fn strict_round_keeps_doubles_double() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let check = first_item_check(&db, "f <- function() sum(1, 2)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(matches!(
            scheme_return(&db, &check).kind(&db),
            TyKind::Scalar(Atomic::Double)
        ));
    }

    #[test]
    fn courtesy_round_admits_whole_double_literals() {
        let db = RootDatabase::default();
        StubSources::new(
            &db,
            vec![(
                "test".to_owned(),
                "f : fn(x: integer) -> integer\nf : fn(x: integer[]) -> integer[]\n".to_owned(),
            )],
            Vec::new(),
        );
        let check = first_item_check(&db, "g <- function() f(1)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(matches!(
            scheme_return(&db, &check).kind(&db),
            TyKind::Scalar(Atomic::Integer)
        ));
    }

    #[test]
    fn no_matching_overload_reports_with_first_failure() {
        let db = RootDatabase::default();
        StubSources::new(
            &db,
            vec![(
                "test".to_owned(),
                "f : fn(x: integer) -> integer\nf : fn(x: double) -> double\n".to_owned(),
            )],
            Vec::new(),
        );
        let check = first_item_check(&db, "g <- function() f(\"a\")\n");
        assert!(
            check.errors.iter().any(|error| matches!(
                &error.kind,
                TypeErrorKind::NoMatchingOverload {
                    name,
                    candidates: 2,
                    first: Some(_),
                } if name == "f"
            )),
            "expected a no-matching-overload report, got {:?}",
            check.errors
        );
    }

    #[test]
    fn unresolved_argument_falls_back_to_the_general_candidate() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let check = first_item_check(&db, "f <- function(x) sum(x)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.clone().expect("scheme");
        assert_eq!(
            scheme.binders.len(),
            1,
            "the parameter must stay generic: {scheme:?}"
        );
        assert!(matches!(scheme_return(&db, &check).kind(&db), TyKind::Any));
    }

    #[test]
    fn a_discriminating_argument_selects_despite_a_flexible_one() {
        let db = RootDatabase::default();
        StubSources::new(
            &db,
            vec![(
                "test".to_owned(),
                "f : fn(callback: fn(integer) -> integer, x: integer) -> integer\n\
                 f : fn(callback: fn(character) -> character, x: character) -> character\n"
                    .to_owned(),
            )],
            Vec::new(),
        );
        let check = first_item_check(&db, "g <- function() f(function(v) v, 1L)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(matches!(
            scheme_return(&db, &check).kind(&db),
            TyKind::Scalar(Atomic::Integer)
        ));
    }

    #[test]
    fn arity_selects_the_only_candidate_a_flexible_call_can_fit() {
        let db = RootDatabase::default();
        StubSources::new(
            &db,
            vec![(
                "test".to_owned(),
                "f : fn(x: integer) -> integer\n\
                 f : fn(x: integer, y: integer) -> double\n\
                 f : fn(x: character) -> character\n"
                    .to_owned(),
            )],
            Vec::new(),
        );
        let check = first_item_check(&db, "g <- function(n) f(n, 2L)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(matches!(
            scheme_return(&db, &check).kind(&db),
            TyKind::Scalar(Atomic::Double)
        ));
    }

    #[test]
    fn a_candidate_that_costs_the_caller_nothing_wins() {
        let db = RootDatabase::default();
        StubSources::new(
            &db,
            vec![(
                "test".to_owned(),
                "f : fn(x: Any) -> character\nf : fn(x: integer) -> integer\n".to_owned(),
            )],
            Vec::new(),
        );
        // The second candidate fits only by pinning `n` to `integer`.
        let check = first_item_check(&db, "g <- function(n) f(n)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.clone().expect("scheme");
        assert_eq!(
            scheme.binders.len(),
            1,
            "the parameter must stay generic: {scheme:?}"
        );
        assert!(matches!(
            scheme_return(&db, &check).kind(&db),
            TyKind::Scalar(Atomic::Character)
        ));
    }

    #[test]
    fn courtesy_applies_outside_overload_sets_too() {
        let db = RootDatabase::default();
        StubSources::new(
            &db,
            vec![(
                "test".to_owned(),
                "g : fn(n: integer) -> integer[]\n".to_owned(),
            )],
            Vec::new(),
        );
        let ok = first_item_check(&db, "f <- function() g(3)\n");
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
        let db2 = RootDatabase::default();
        StubSources::new(
            &db2,
            vec![(
                "test".to_owned(),
                "g : fn(n: integer) -> integer[]\n".to_owned(),
            )],
            Vec::new(),
        );
        let bad = first_item_check(&db2, "f <- function() g(2.5)\n");
        assert!(
            bad.errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::Mismatch { .. })),
            "a fractional double must not pass as integer: {:?}",
            bad.errors
        );
    }

    #[test]
    fn namespace_access_resolves_through_globals() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let check = first_item_check(&db, "f <- function(x) base::length(x)\n");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(matches!(
            scheme_return(&db, &check).kind(&db),
            TyKind::Scalar(Atomic::Integer)
        ));
    }

    #[test]
    fn overloads_and_replacement_across_sources() {
        let db = RootDatabase::default();
        let sources = StubSources::new(
            &db,
            vec![
                (
                    "first".to_owned(),
                    "f : fn(x: integer) -> integer\nf : fn(x: Any) -> Any\n".to_owned(),
                ),
                (
                    "second".to_owned(),
                    "f : fn(x: double) -> double\n".to_owned(),
                ),
            ],
            Vec::new(),
        );
        let library = stub_library(&db, sources);
        let candidates = &library.schemes["f"];
        assert_eq!(candidates.len(), 1, "a later source replaces the whole set");
    }
}
