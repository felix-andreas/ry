//! `.Rtypes` stubs: `name : <type>` lines whose type half goes through the `#:` annotation
//! grammar. Stub text is a set-once input, so it never participates in per-edit invalidation.

use {
    crate::{
        Db,
        annotations::{NamedDefinition, lower_annotation},
        hir::Symbol,
        metadata::namespace_active,
        types::{Name, Ty, TyKind, TypeScheme, for_each_child_type},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    std::{collections::hash_map::Entry, sync::LazyLock},
    syntax::{SyntaxKind, TextRange},
};

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
    pub schemes: SymbolMap<Vec<TypeScheme<'db>>>,
    pub nominals: SymbolSet,
    /// `@alias` and generic `@type` declarations, which the checker resolves exactly like a
    /// project's own; a project definition of the same name shadows one.
    pub type_definitions: FxHashMap<Name<'db>, NamedDefinition<'db>>,
    /// Data-masking variadics, mapped to the formals before `...` (which resolve normally). An
    /// empty list masks every argument.
    pub masked: SymbolMap<Vec<String>>,
    /// `@opaque(name, ...)` parameters: the callee forces the argument and may hand it back,
    /// but never inspects it as an R object, so a ry value may pass there.
    pub opaque: SymbolMap<Vec<String>>,
    /// `@pure` names per declaring namespace. A bare call takes its purity from the namespace
    /// of the name's winning declaration (`declarations`), the same one its type comes from.
    pub pure_by_namespace: SymbolMap<SymbolSet>,
    /// Declaration-level: a later source overriding a name's type does not un-export it from the
    /// namespace that declared it.
    pub exports_by_namespace: SymbolMap<SymbolSet>,
    /// The winning declaration site per name; for an overload set, the first candidate.
    pub declarations: SymbolMap<StubDeclaration>,
    /// Manifest-listed exports of the active namespaces; these always resolve, untyped ones as
    /// `Unknown`.
    pub known_exports: SymbolSet,
    /// The `#'` lines written directly above a declaration, joined as Markdown, which hover and
    /// completion show beside its type.
    pub documentation: SymbolMap<String>,
}

/// `source_index` indexes the `StubSources` order the host fed in.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct StubDeclaration {
    pub source_index: usize,
    pub range: TextRange,
}

/// A map keyed by interned identifiers. A probe by spelling looks the symbol up without
/// interning it, so asking about a name nothing has interned misses without growing the
/// process-wide symbol table.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct SymbolMap<V>(FxHashMap<Symbol, V>);

/// A set of interned identifiers, probed like a [`SymbolMap`].
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct SymbolSet(FxHashSet<Symbol>);

/// A name to probe a [`SymbolMap`] or [`SymbolSet`] with: a symbol as it is, a spelling through
/// [`Symbol::lookup`].
pub trait SymbolKey {
    fn symbol(self) -> Option<Symbol>;
}

impl SymbolKey for Symbol {
    fn symbol(self) -> Option<Symbol> {
        Some(self)
    }
}

impl SymbolKey for &Symbol {
    fn symbol(self) -> Option<Symbol> {
        Some(*self)
    }
}

impl SymbolKey for &str {
    fn symbol(self) -> Option<Symbol> {
        Symbol::lookup(self)
    }
}

impl SymbolKey for &String {
    fn symbol(self) -> Option<Symbol> {
        Symbol::lookup(self)
    }
}

impl<V> Default for SymbolMap<V> {
    fn default() -> SymbolMap<V> {
        SymbolMap(FxHashMap::default())
    }
}

impl<V> SymbolMap<V> {
    pub fn get(&self, name: impl SymbolKey) -> Option<&V> {
        self.0.get(&name.symbol()?)
    }

    pub fn get_mut(&mut self, name: impl SymbolKey) -> Option<&mut V> {
        self.0.get_mut(&name.symbol()?)
    }

    pub fn contains_key(&self, name: impl SymbolKey) -> bool {
        name.symbol().is_some_and(|name| self.0.contains_key(&name))
    }

    pub fn insert(&mut self, name: Symbol, value: V) -> Option<V> {
        self.0.insert(name, value)
    }

    pub fn remove(&mut self, name: impl SymbolKey) -> Option<V> {
        self.0.remove(&name.symbol()?)
    }

    pub fn entry(&mut self, name: Symbol) -> Entry<'_, Symbol, V> {
        self.0.entry(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Symbol, &V)> {
        self.0.iter().map(|(&name, value)| (name, value))
    }

    pub fn keys(&self) -> impl Iterator<Item = Symbol> {
        self.0.keys().copied()
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.0.values()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl SymbolSet {
    pub fn contains(&self, name: impl SymbolKey) -> bool {
        name.symbol().is_some_and(|name| self.0.contains(&name))
    }

    pub fn insert(&mut self, name: Symbol) -> bool {
        self.0.insert(name)
    }

    pub fn remove(&mut self, name: impl SymbolKey) -> bool {
        name.symbol().is_some_and(|name| self.0.remove(&name))
    }

    pub fn iter(&self) -> impl Iterator<Item = Symbol> {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<Symbol> for SymbolSet {
    fn from_iter<I: IntoIterator<Item = Symbol>>(names: I) -> SymbolSet {
        SymbolSet(names.into_iter().collect())
    }
}

impl Extend<Symbol> for SymbolSet {
    fn extend<I: IntoIterator<Item = Symbol>>(&mut self, names: I) {
        self.0.extend(names);
    }
}

pub fn stub_declaration<'db>(db: &'db dyn Db, name: &str) -> Option<&'db StubDeclaration> {
    stubs(db)?.declarations.get(name)
}

/// `None` when no corpus is installed, so callers skip validation.
pub fn namespace_known(db: &dyn Db, package: &str) -> Option<bool> {
    stubs(db).map(|library| library.knows_namespace(package))
}

impl StubLibrary<'_> {
    pub fn knows_namespace(&self, package: &str) -> bool {
        self.exports_by_namespace.contains_key(package)
    }

    pub fn exports(&self, package: impl SymbolKey, name: impl SymbolKey) -> bool {
        self.exports_by_namespace
            .get(package)
            .is_some_and(|names| names.contains(name))
    }
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
    stubs(db).is_some_and(|library| library.exports(package, name))
}

/// Whether a shipped namespace the project has not activated exports `name`, for validating
/// `pkg::name`: `::` loads a package without attaching it, so the read is legal R, but its name
/// must still exist. `None` when no export manifest ships for the package, because typed stubs
/// alone may describe only part of it.
pub fn dormant_namespace_exports(db: &dyn Db, package: &str, name: &str) -> Option<bool> {
    let sources = StubSources::try_get(db)?;
    dormant_exports(db, sources, Symbol::new(package))
        .as_ref()
        .map(|names| names.contains(name))
}

/// Every name a shipped namespace's manifest lists or its stubs declare, read without
/// activating it. Stub sources change only with the stub files, so this is computed once.
#[salsa::tracked(returns(ref))]
fn dormant_exports(db: &dyn Db, sources: StubSources, package: Symbol) -> Option<SymbolSet> {
    let package = package.as_str();
    let (_, manifest) = sources
        .manifests(db)
        .iter()
        .find(|(namespace, _)| namespace == package)?;
    let mut names: SymbolSet = manifest
        .lines()
        .map(|line| strip_comment(line).trim())
        .filter(|name| !name.is_empty())
        .map(Symbol::new)
        .collect();
    for (_, text) in sources
        .sources(db)
        .iter()
        .filter(|(namespace, _)| namespace == package)
    {
        for line in text.lines() {
            if line.trim_start().starts_with("#'") {
                continue;
            }
            let content = strip_comment(line).trim();
            if let Some(definition) = type_definition_line(db, content) {
                names.insert(Symbol::new(definition.name.text(db)));
            } else if let Some(rest) = content.strip_prefix("@type") {
                names.insert(Symbol::new(rest.trim()));
            } else if let Some(separator) = top_level_colon(content) {
                let name = content[..separator].trim();
                if is_stub_name(name) {
                    names.insert(Symbol::new(name));
                }
            }
        }
    }
    Some(names)
}

/// The namespace of ry's standard library (`Result.map`, `Dict.get`, ...). Its names are in scope
/// bare unless `[check] prelude` is off, and always as `std::Name`.
pub const STANDARD_LIBRARY: &str = "std";

/// Whether `name` is a function a native namespace declares, such as `List.map`. R's own
/// dotted names (`Sys.time`) and S3 methods (`Ops.money`) share the spelling but are not.
pub fn is_native_function(name: &str) -> bool {
    NATIVE_FUNCTIONS.binary_search(&name).is_ok()
}

/// Whether `[check] prelude` keeps the bare `name` out of scope: the standard library exports it
/// and the prelude is off. The setting is read only for a standard library name, so turning the
/// prelude off rechecks only the code that uses one.
pub fn outside_prelude(db: &dyn Db, library: &StubLibrary<'_>, name: impl SymbolKey) -> bool {
    static STANDARD_LIBRARY_NAME: LazyLock<Symbol> =
        LazyLock::new(|| Symbol::new(STANDARD_LIBRARY));
    library.exports(*STANDARD_LIBRARY_NAME, name) && !crate::prelude_enabled(db)
}

/// Whether `name` is a type ry itself declares, rather than one a project or package spells the
/// same way.
pub fn is_ry_type(db: &dyn Db, name: &str) -> bool {
    declaring_namespace(db, name).is_some_and(|namespace| NATIVE_NAMESPACES.contains(&namespace))
}

// Generated by `build.rs` from every file in top-level `types/`, not `stubs/`: a workspace-root
// `stubs/` is the project-override convention and would load as overrides when ry analyzes this
// repository. The generated `CONDITIONAL_NAMESPACES` lists every shipped namespace except R's own
// packages and the standard library: those join the library only when the project declares or
// attaches the package, so their names never resolve (or mask a typo) elsewhere. A manifest-only
// entry still matters, because attaching a package with an unknowable export set disables
// unresolved-name detection project-wide.
include!(concat!(env!("OUT_DIR"), "/shipped.rs"));

pub fn shipped_stub_sources() -> Vec<(String, String)> {
    owned_pairs(SHIPPED_STUBS)
}

pub fn shipped_export_manifests() -> Vec<(String, String)> {
    owned_pairs(SHIPPED_MANIFESTS)
}

fn owned_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(namespace, text)| ((*namespace).to_owned(), (*text).to_owned()))
        .collect()
}

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
        // A qualified-only namespace's typed declarations would also resolve bare names, so they
        // join only once the package is attached or declared, like a conditional namespace.
        if (CONDITIONAL_NAMESPACES.contains(&namespace.as_str())
            || QUALIFIED_ONLY_NAMESPACES.contains(&namespace.as_str()))
            && !project_overridden.contains(namespace.as_str())
            && !namespace_active(db, namespace)
        {
            continue;
        }
        let namespace_symbol = Symbol::new(namespace);
        let namespace_exports = library
            .exports_by_namespace
            .entry(namespace_symbol)
            .or_default();
        // A name repeated within this source appends candidates; its first occurrence here
        // replaces any earlier source's set.
        let mut seen_here: FxHashSet<Symbol> = FxHashSet::default();
        let mut line_start = 0usize;
        let mut pending_documentation: Vec<&str> = Vec::new();
        for raw_segment in text.split_inclusive('\n') {
            let raw_line = raw_segment.trim_end_matches(['\n', '\r']);
            let this_line_start = line_start;
            line_start += raw_segment.len();
            if let Some(line) = raw_line.trim_start().strip_prefix("#'") {
                pending_documentation.push(line.strip_prefix(' ').unwrap_or(line));
                continue;
            }
            let documentation = std::mem::take(&mut pending_documentation);
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
                let name = Symbol::new(definition.name.text(db));
                library.nominals.insert(name);
                namespace_exports.insert(name);
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
                    let name = Symbol::new(name);
                    library.nominals.insert(name);
                    namespace_exports.insert(name);
                    if let Some(range) = name_range(&name) {
                        library.declarations.insert(
                            name,
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
            let spelling = content[..separator].trim();
            let mut type_text = content[separator + 1..].trim();
            if spelling.is_empty() || !is_stub_name(spelling) {
                continue;
            }
            let markers = declaration_markers(type_text);
            type_text = markers.type_text;
            let Some(scheme) = lower_type_text(db, type_text) else {
                continue;
            };
            let name = Symbol::new(spelling);
            if markers.masked
                && let Some(leading) = masked_leading_formals(db, &scheme)
            {
                library.masked.insert(name, leading);
            }
            namespace_exports.insert(name);
            let first_here = seen_here.insert(name);
            if first_here {
                library.opaque.remove(name);
            }
            if let Some(parameters) = markers.opaque {
                library.opaque.insert(name, parameters);
            }
            // Purity belongs to the name within its namespace: any candidate's marker marks it,
            // and a later source that redeclares the name decides afresh, so an override stub
            // can take the marker away.
            let pure = library
                .pure_by_namespace
                .entry(namespace_symbol)
                .or_default();
            if first_here {
                pure.remove(name);
            }
            if markers.pure {
                pure.insert(name);
            }
            if first_here {
                if documentation.is_empty() {
                    library.documentation.remove(name);
                } else {
                    library.documentation.insert(name, documentation.join("\n"));
                }
                library.schemes.insert(name, vec![scheme]);
                if let Some(range) = name_range(spelling) {
                    library.declarations.insert(
                        name,
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
            .entry(Symbol::new(namespace))
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
            let name = Symbol::new(name);
            namespace_exports.insert(name);
            if bare_visible {
                library.known_exports.insert(name);
            }
        }
    }
    library
}

struct DeclarationMarkers<'text> {
    masked: bool,
    pure: bool,
    opaque: Option<Vec<String>>,
    type_text: &'text str,
}

/// The `@masked`, `@pure` and `@opaque(...)` markers that may precede a declaration's type, in
/// any order.
fn declaration_markers(mut type_text: &str) -> DeclarationMarkers<'_> {
    let mut markers = DeclarationMarkers {
        masked: false,
        pure: false,
        opaque: None,
        type_text,
    };
    loop {
        if let Some(rest) = type_text.strip_prefix("@opaque(")
            && let Some((list, rest)) = rest.split_once(')')
            && rest.starts_with(char::is_whitespace)
        {
            let parameters = list
                .split(',')
                .map(str::trim)
                .filter(|parameter| !parameter.is_empty())
                .map(str::to_owned);
            markers.opaque.get_or_insert_with(Vec::new).extend(parameters);
            type_text = rest.trim_start();
            continue;
        }
        let (marker, rest) = if let Some(rest) = type_text.strip_prefix("@masked") {
            (&mut markers.masked, rest)
        } else if let Some(rest) = type_text.strip_prefix("@pure") {
            (&mut markers.pure, rest)
        } else {
            break;
        };
        if !rest.starts_with(char::is_whitespace) {
            break;
        }
        *marker = true;
        type_text = rest.trim_start();
    }
    markers.type_text = type_text;
    markers
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

/// For each name, every namespace whose export manifest lists it, whether or not the project
/// uses that package, in namespace order. An unresolved name looks up here which packages could
/// provide it.
#[salsa::tracked(returns(ref))]
pub fn exporting_namespaces(db: &dyn Db, sources: StubSources) -> ExportingNamespaces {
    let mut index: FxHashMap<String, Vec<String>> = FxHashMap::default();
    for (namespace, text) in sources.manifests(db) {
        for line in text.lines() {
            let name = strip_comment(line).trim();
            if name.is_empty() {
                continue;
            }
            let namespaces = index.entry(name.to_owned()).or_default();
            if !namespaces.contains(namespace) {
                namespaces.push(namespace.clone());
            }
        }
    }
    for namespaces in index.values_mut() {
        namespaces.sort_unstable();
    }
    ExportingNamespaces(index)
}

#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct ExportingNamespaces(pub FxHashMap<String, Vec<String>>);

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
    let library_nominals = stubs(db).map(|library| &library.nominals);
    let mut own_nominals: FxHashSet<&str> = FxHashSet::default();
    for raw_line in text.lines() {
        let content = strip_comment(raw_line).trim();
        if let Some(name) = definition_line_name(content) {
            if type_definition_line(db, content).is_some() {
                own_nominals.insert(name);
            }
        } else if let Some(rest) = content.strip_prefix("@type") {
            let name = rest.trim();
            if is_nominal_name(name) {
                own_nominals.insert(name);
            }
        }
    }
    let is_known = |name: &str| {
        own_nominals.contains(name)
            || library_nominals.is_some_and(|nominals| nominals.contains(name))
    };

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
                    collect_unknown_nominals(db, definition.body, &is_known, &mut unknown);
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
                    message: "this type declaration does not load: write `@alias NAME<T> {TYPE}`, `@type NAME {TYPE}` or `@type NAME<T>`.".to_owned(),
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
        let markers = declaration_markers(type_text);
        type_text = markers.type_text;
        let masked = markers.masked;
        let opaque = markers.opaque;
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
        let formals = match scheme.body.kind(db) {
            TyKind::Function(function) => function
                .named
                .iter()
                .map(|field| field.name.text(db))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        for parameter in opaque.iter().flatten() {
            if !formals.contains(&parameter.as_str()) {
                problems.push(StubProblem {
                    line,
                    message: format!(
                        "`@opaque({parameter})` on `{name}` names no parameter of its function type."
                    ),
                });
            }
        }
        let mut unknown = Vec::new();
        collect_unknown_nominals(db, scheme.body, &is_known, &mut unknown);
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
    is_known: &impl Fn(&str) -> bool,
    out: &mut Vec<String>,
) {
    if let TyKind::Named(name, _) = ty.kind(db) {
        let text = name.text(db);
        if !is_known(text) && !out.iter().any(|seen| seen == text) {
            out.push(text.to_owned());
        }
    }
    for_each_child_type(db, ty, &mut |child| {
        collect_unknown_nominals(db, child, is_known, out)
    });
}

/// The declared name of an `@alias` line, a generic `@type NAME<T, ...>` line, or a `@type NAME
/// {TYPE}` line with a representation. A plain `@type NAME` is the older opaque form, handled on
/// its own.
fn definition_line_name(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("@alias").or_else(|| {
        content
            .strip_prefix("@type")
            .filter(|rest| rest.contains(['<', '{']))
    })?;
    let rest = rest.trim_start();
    let end = rest.find(['<', ' ', '{']).unwrap_or(rest.len());
    let name = &rest[..end];
    is_nominal_name(name).then_some(name)
}

/// An `@alias NAME<T> {TYPE}`, `@type NAME {TYPE}` or `@type NAME<T>` line, lowered through the
/// annotation grammar a project's own `#: @alias` uses. A generic opaque type is a `@type` whose
/// representation is `Unknown`, which the checker already treats as having none.
fn type_definition_line<'db>(db: &'db dyn Db, content: &str) -> Option<NamedDefinition<'db>> {
    definition_line_name(content)?;
    let text = if content.starts_with("@type") && !content.contains('{') {
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
pub(crate) fn lower_type_text<'db>(db: &'db dyn Db, type_text: &str) -> Option<TypeScheme<'db>> {
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

/// Depth-zero only, so `list[a: b]` keeps its interior colon. The brackets of a subset method's
/// name (`[.factor`) are part of the name, not a nesting level.
fn top_level_colon(content: &str) -> Option<usize> {
    let name_brackets = SUBSET_METHOD_PREFIXES
        .iter()
        .find(|prefix| content.starts_with(&format!("{prefix}.")))
        .map_or(0, |prefix| prefix.len());
    let mut depth: usize = 0;
    for (byte_index, character) in content.char_indices().skip(name_brackets) {
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
        .chain(&SUBSET_METHOD_PREFIXES)
        .filter_map(|operator| name.strip_prefix(operator))
        .any(|suffix| suffix.strip_prefix('.').is_some_and(is_nominal_name))
        // `Class$member` declares what `$member` reads on a value of that class, as for an
        // environment-backed object such as a polars frame.
        || name
            .split_once('$')
            .is_some_and(|(class, member)| is_nominal_name(class) && is_nominal_name(member))
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

/// `[.factor` and `[[.factor` say which class a subset of the nominal keeps. Longest first.
const SUBSET_METHOD_PREFIXES: [&str; 3] = ["[[", "[", "$"];

pub fn install_shipped_stubs(db: &dyn Db) -> StubSources {
    StubSources::new(db, shipped_stub_sources(), shipped_export_manifests())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            DocumentKind, ProjectFiles, RootDatabase, SourceFile,
            check::{ItemCheck, TypeErrorKind},
            item_check, item_tree,
            metadata::PackageMetadata,
            types::Atomic,
        },
    };

    #[test]
    fn standard_library_stub_loads_cleanly() {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        for text in [
            include_str!("../../../types/std.Rtypes"),
            include_str!("../../../types/ry.Rtypes"),
        ] {
            let problems = stub_source_problems(&db, text);
            assert!(problems.is_empty(), "{problems:?}");
        }
        let library = stubs(&db).expect("the shipped stubs are installed");
        assert!(library.schemes.contains_key("Result.map"));
        assert!(
            library
                .type_definitions
                .keys()
                .any(|name| name.text(&db) == "Result")
        );
        assert_eq!(declaring_namespace(&db, "Dict"), Some(STANDARD_LIBRARY));
    }

    /// A dispatch target for operator lookup (`+.Date`, `Compare.Date`), not an exported name.
    fn is_operator_method_name(name: &str) -> bool {
        const GROUP_GENERICS: [&str; 4] = ["Arith.", "Compare.", "Ops.", "Summary."];
        GROUP_GENERICS
            .iter()
            .any(|group| name.starts_with(group) && name.len() > group.len())
            || OPERATOR_METHOD_PREFIXES
                .iter()
                .chain(&SUBSET_METHOD_PREFIXES)
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
                .chain(QUALIFIED_ONLY_NAMESPACES)
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
        for (namespace, declared) in library.exports_by_namespace.iter() {
            let Some(manifest) = manifests.get(namespace.as_str()) else {
                continue;
            };
            for name in declared.iter() {
                if library.nominals.contains(name) || manifest.contains(name.as_str()) {
                    continue;
                }
                // R registers operator methods like `Compare.Date` without exporting them, and a
                // `class$member` declaration types a member of an object, not a binding.
                if is_operator_method_name(&name) || name.contains('$') {
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

    /// A whole-number literal passes as an integer to a parameter outside any overload set, and
    /// a fractional one does not.
    #[test]
    fn a_whole_number_literal_passes_as_an_integer_argument() {
        for (call, accepted) in [("g(3)", true), ("g(2.5)", false)] {
            let db = RootDatabase::default();
            StubSources::new(
                &db,
                vec![(
                    "test".to_owned(),
                    "g : fn(n: integer) -> integer[]\n".to_owned(),
                )],
                Vec::new(),
            );
            let check = first_item_check(&db, &format!("f <- function() {call}\n"));
            let mismatch = check
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::Mismatch { .. }));
            assert_eq!(!mismatch, accepted, "{call}: {:?}", check.errors);
            if accepted {
                assert!(check.errors.is_empty(), "{call}: {:?}", check.errors);
            }
        }
    }

    fn first_item_check<'db>(db: &'db RootDatabase, source: &str) -> &'db ItemCheck<'db> {
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
            scheme_return(&db, check).kind(&db),
            TyKind::Scalar(Atomic::Integer)
        ));
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
            scheme_return(&db, check).kind(&db),
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
            scheme_return(&db, check).kind(&db),
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
            scheme_return(&db, check).kind(&db),
            TyKind::Scalar(Atomic::Character)
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
        let candidates = library.schemes.get("f").expect("the stub declares f");
        assert_eq!(candidates.len(), 1, "a later source replaces the whole set");
    }
}
