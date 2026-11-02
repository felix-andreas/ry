//! The package-load index: what a loader needs to know about a package's top-level items to
//! load it lazily and to reload exactly what changed.
//!
//! Every fact comes from one file's own items (`file_items`, `file_binds`) or text
//! (`file_load_index`), so an edit inside one file re-derives that file alone. `package_bindings`
//! joins the per-file binding facts and `package_index` joins the rest; each join is a firewall
//! that returns the same value, and stops invalidation, when an edit changes no binding.

use {
    crate::{
        Db, Item, ItemKind, SourceFile, TypingMode, file_typing_mode,
        hir::Symbol,
        item_naming, item_spans,
        metadata::{
            Call, NamespaceSpec, call_parts, description_field, parse_description_depends,
            parse_namespace_spec, string_content,
        },
        parse,
        top_level::{TopLevel, item_super_assigned_names, item_top_level},
    },
    rowan::{GreenNodeData, Language, NodeOrToken},
    rustc_hash::FxHashMap,
    serde::{Deserialize, Serialize},
    std::{collections::BTreeSet, sync::Arc},
    syntax::{
        RLanguage, SyntaxKind, SyntaxNode, TextRange, TextSize,
        ast::{self, BinaryExpr},
    },
};

/// A package as the loader sees it: its `R/` files in collation order, plus `NAMESPACE` and
/// `DESCRIPTION`. Separate fields, so a `NAMESPACE` edit never re-derives an item.
#[salsa::input(debug)]
pub struct PackageSources {
    #[returns(ref)]
    pub files: Vec<PackageFile>,
    #[returns(ref)]
    pub namespace: String,
    #[returns(ref)]
    pub description: String,
    /// `[check] strict`, which holds for each file without its own `# typing:` directive.
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageFile {
    /// Relative to the package root, with `/` separators (`R/utils.R`).
    pub path: String,
    pub file: SourceFile,
}

/// One item's load facts, derived from its own syntax only.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemLoad {
    /// Of the text R evaluates (inline annotations erased), so an annotation-only edit
    /// still counts as a change while a shift in position does not.
    pub hash: u64,
    /// `Some(name)` for `name <- value`, the shape a loader may bind lazily.
    pub binding: Option<String>,
    /// Whether the bound value is a function literal.
    pub function: bool,
    /// For a binding, every name its value mentions: a superset of what evaluating it, or
    /// calling the function it defines, reads, found without name resolution.
    pub mentions: Vec<String>,
    pub registrations: Vec<Registration>,
    pub s4: Option<S4Definition>,
}

/// A top-level S4 definition in the shape a loader may defer until the class or generic it
/// defines is first used: the call is bare (or binds a class generator), and every name and
/// class in it is a string literal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum S4Definition {
    /// `setClass("Name", ...)`.
    Class { class: String },
    /// `setGeneric("name", function(...) standardGeneric("name"))`. Without its own
    /// definition, `setGeneric` wraps an existing function and says so, which cannot wait.
    Generic { generic: String },
    /// `setMethod("generic", signature, ...)` or `setReplaceMethod`, whose generic is named as
    /// R stores it (`name<-` for a replacement).
    Method {
        generic: String,
        signature: Vec<String>,
    },
    /// `setValidity("Class", ...)`.
    Validity { class: String },
}

/// A class or method an item registers as a side effect, which a reload must undo when the
/// item disappears.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Registration {
    S4Class {
        class: String,
    },
    S4Generic {
        generic: String,
    },
    S4Method {
        generic: String,
        signature: Vec<String>,
    },
    R6Class {
        class: String,
    },
    S3Method {
        generic: String,
        class: String,
    },
}

/// What a load needs of one item, read from its syntax node. It is computed inside
/// [`file_items`] rather than memoized per item: the first edit of a file lowers its
/// durability, which makes salsa re-verify every memo the package index reaches, and one memo
/// per file is far cheaper to re-verify than one per item.
fn item_load(node: Option<&SyntaxNode>, text: &str) -> ItemLoad {
    let Some(node) = node else {
        return ItemLoad {
            hash: 0,
            binding: None,
            function: false,
            mentions: Vec::new(),
            registrations: Vec::new(),
            s4: None,
        };
    };
    let definition = binding_definition(node);
    let mentions = definition.as_ref().map_or_else(Vec::new, |(_, value)| {
        let green = value.green();
        let mut names = Vec::new();
        collect_identifiers(green.as_ref(), &mut names);
        names.sort_unstable();
        names.dedup();
        names.into_iter().map(str::to_owned).collect()
    });
    ItemLoad {
        hash: fnv1a(text.as_bytes()),
        function: definition
            .as_ref()
            .is_some_and(|(_, value)| value.kind() == SyntaxKind::FUNCTION_DEF),
        binding: definition.map(|(name, _)| name),
        mentions,
        registrations: registrations(node),
        s4: s4_definition(node),
    }
}

/// Names an item reads while it runs, so not reads inside a function it defines: those happen
/// when the function is called and find the current binding then. A reload reruns a statement
/// whose immediate reads changed, because it captured their old values.
#[salsa::tracked(returns(ref))]
pub fn item_immediate_reads<'db>(db: &'db dyn Db, item: Item<'db>) -> BTreeSet<Symbol> {
    let Some(naming) = item_naming(db, item) else {
        return BTreeSet::new();
    };
    naming
        .non_locals
        .iter()
        .filter(|(expression, _)| !naming.deferred_non_locals.contains(expression))
        .map(|(_, name)| *name)
        .collect()
}

/// One file as R reads it, with every item's position in that text.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct FileLoadIndex<'db> {
    /// The file with inline annotations erased and every line break kept, so line numbers
    /// match the file on disk.
    pub text: String,
    pub items: Vec<LoadedItem<'db>>,
    pub syntax_errors: Vec<(u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct LoadedItem<'db> {
    pub item: Item<'db>,
    pub span: SourceSpan,
    /// The value of `name <- value`, which is what a lazy binding evaluates.
    pub value: Option<SourceSpan>,
}

/// A range of [`FileLoadIndex::text`] with the position R's source references need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    /// 1-based.
    pub line: u32,
    /// Characters before the start on its line.
    pub column: u32,
    /// Byte offsets.
    pub start: u32,
    pub end: u32,
    /// Character offsets, which is how R's `substr` counts.
    pub char_start: u32,
    pub char_end: u32,
    /// Byte offsets into the file as written, where the analysis's syntax tree holds the node,
    /// so a runtime that lowers that tree needs no second parse.
    #[serde(default)]
    pub source_start: u32,
    #[serde(default)]
    pub source_end: u32,
}

impl SourceSpan {
    pub fn slice<'a>(&self, text: &'a str) -> &'a str {
        text.get(self.start as usize..self.end as usize)
            .unwrap_or_default()
    }
}

#[salsa::tracked(returns(ref))]
pub fn file_load_index<'db>(db: &'db dyn Db, file: SourceFile) -> FileLoadIndex<'db> {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let spans = item_spans(db, file);
    let mut marks: Vec<TextSize> = Vec::with_capacity(spans.len() * 4);
    let mut value_ranges: Vec<Option<TextRange>> = Vec::with_capacity(spans.len());
    for span in spans {
        marks.push(span.range.start());
        marks.push(span.range.end());
        let value = root
            .child_or_token_at_range(span.range)
            .and_then(NodeOrToken::into_node)
            .filter(|node| node.text_range() == span.range)
            .and_then(|node| binding_definition(&node).map(|(_, value)| value.text_range()));
        if let Some(range) = value {
            marks.push(range.start());
            marks.push(range.end());
        }
        value_ranges.push(value);
    }
    let (text, marks) = erase_for_r(&root, marks);
    let lines = LineStarts::new(&text);
    let characters = character_offsets(&text, marks.erased.iter().copied());
    let position = |offset: TextSize| marks.erased_offset(offset);
    let span_of = |range: TextRange| {
        let start = position(range.start());
        let end = position(range.end()).max(start);
        let mut span = lines.span(&text, start, end);
        span.char_start = characters.get(&start).copied().unwrap_or(start);
        span.char_end = characters.get(&end).copied().unwrap_or(end);
        span.source_start = u32::from(range.start());
        span.source_end = u32::from(range.end());
        span
    };
    let items = spans
        .iter()
        .zip(value_ranges)
        .map(|(span, value)| LoadedItem {
            item: span.item,
            span: span_of(span.range),
            value: value.map(span_of),
        })
        .collect();
    let original = LineStarts::new(file.text(db));
    let syntax_errors = parse
        .errors()
        .iter()
        .filter(|error| !error.in_annotation)
        .map(|error| {
            let line = original.line_of(u32::from(error.range.start()));
            (line, error.message.clone())
        })
        .collect();
    FileLoadIndex {
        text,
        items,
        syntax_errors,
    }
}

/// The whole package's load index. It owns no database handles, so a host can keep it after
/// the database moves on and diff it against the next one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageIndex {
    pub package: String,
    pub version: String,
    /// `Depends` packages, which a load attaches.
    pub depends: Vec<String>,
    pub namespace: NamespaceSpec,
    /// Collation order, which is the order R runs top-level code in.
    pub files: Vec<IndexedFile>,
    /// Which names are lazy and where their definitions are. An edit inside a function body
    /// leaves it equal, so the analysis hands back the same `Arc`.
    pub bindings: Arc<PackageBindings>,
}

/// The package's top-level names and how they load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageBindings {
    /// Every top-level name the package binds, mapped to whether it is bound lazily: a name is
    /// lazy when its one writer defines a function or, in a strict file, a constant whose value
    /// calls only functions declared pure. Any other writer, including conditional or
    /// replacement code (`f <- memoise(f)`), makes every writer of the name run eagerly.
    pub names: FxHashMap<String, bool>,
    /// The item that defines each lazy name, as a position in [`PackageIndex::files`] and a
    /// position among that file's items.
    pub lazy: FxHashMap<String, (u32, u32)>,
    /// The items that run at load, in collation order, positioned as in `lazy`.
    pub eager: Vec<(u32, u32)>,
    /// The lazy constants of strict files, in collation order, positioned as in `lazy`.
    pub constants: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub path: String,
    pub items: Arc<FileItems>,
}

/// One file's items. They depend on that file alone, so a reload compares a file by its hash
/// and looks inside only the files that changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileItems {
    /// Of the text R reads, so an edit anywhere in the file changes it.
    pub hash: u64,
    pub items: Vec<IndexedItem>,
    /// `(line, message)` for R syntax errors, which R would refuse to source.
    pub syntax_errors: Vec<(u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedItem {
    /// The analysis' item identity without the file handle, for display; a reload matches
    /// items by name or content instead.
    pub key: ItemKey,
    pub hash: u64,
    pub span: SourceSpan,
    /// For `name <- value`, the name and the value. A function definition is bound lazily
    /// unless another item writes the name too, and so is a pure constant in a strict file
    /// ([`PackageIndex::names`]).
    pub binding: Option<Binding>,
    /// Top-level names it writes.
    pub binds: Vec<String>,
    /// Names it reads while running (see [`item_immediate_reads`]). A function definition
    /// reads nothing until it is called.
    pub reads: Vec<String>,
    /// For a binding, the names its value mentions, which is what forcing a lazy constant, or
    /// calling a lazy function, may read.
    pub mentions: Vec<String>,
    /// Names its `<<-` assignments write. The namespace locks once loading finishes, so these
    /// writes can only land while `.onLoad`, or something it calls, runs.
    pub load_writes: Vec<String>,
    pub registrations: Vec<Registration>,
    pub s4: Option<S4Definition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub name: String,
    pub value: SourceSpan,
    /// Whether `value` is a function literal. Otherwise the binding is a constant, lazy only
    /// when the package index says so.
    pub function: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub kind: ItemKind,
    pub name: Option<String>,
    pub disambiguator: u32,
}

impl PackageIndex {
    /// The name `item` binds lazily, if it is a lazy definition no other item writes.
    pub fn lazy_name<'a>(&self, item: &'a IndexedItem) -> Option<&'a str> {
        item.binding
            .as_ref()
            .map(|binding| binding.name.as_str())
            .filter(|name| self.is_lazy(name))
    }

    pub fn is_lazy(&self, name: &str) -> bool {
        self.bindings.names.get(name).copied().unwrap_or(false)
    }

    /// Whether the package binds `name` at top level.
    pub fn binds(&self, name: &str) -> bool {
        self.bindings.names.contains_key(name)
    }

    /// The item that defines the lazy name `name`.
    pub fn lazy_item(&self, name: &str) -> Option<&IndexedItem> {
        let &(file, item) = self.bindings.lazy.get(name)?;
        self.files
            .get(file as usize)?
            .items
            .items
            .get(item as usize)
    }

    /// The item at a position of [`PackageBindings`], with its file's path.
    pub fn item_at(&self, (file, item): (u32, u32)) -> Option<(&str, &IndexedItem)> {
        let file = self.files.get(file as usize)?;
        Some((file.path.as_str(), file.items.items.get(item as usize)?))
    }

    /// Every item with its file's path, in collation order.
    pub fn items(&self) -> impl Iterator<Item = (&str, &IndexedItem)> {
        self.files.iter().flat_map(|file| {
            file.items
                .items
                .iter()
                .map(move |item| (file.path.as_str(), item))
        })
    }

    /// `(path, line, message)` for every R syntax error.
    pub fn syntax_errors(&self) -> impl Iterator<Item = (&str, u32, &str)> {
        self.files.iter().flat_map(|file| {
            file.items
                .syntax_errors
                .iter()
                .map(move |(line, message)| (file.path.as_str(), *line, message.as_str()))
        })
    }
}

#[salsa::tracked(returns(ref))]
pub fn namespace_spec(db: &dyn Db, sources: PackageSources) -> NamespaceSpec {
    parse_namespace_spec(sources.namespace(db))
}

/// The per-file layer between the per-item queries and [`package_index`], so re-deriving the
/// package touches one memo per file rather than one per item.
#[salsa::tracked(returns(clone))]
pub fn file_items(db: &dyn Db, file: SourceFile) -> Arc<FileItems> {
    let index = file_load_index(db, file);
    let root = parse(db, file).syntax_node();
    let items = index
        .items
        .iter()
        .map(|loaded| {
            let item = loaded.item;
            let range = TextRange::new(
                TextSize::from(loaded.span.source_start),
                TextSize::from(loaded.span.source_end),
            );
            let node = root
                .child_or_token_at_range(range)
                .and_then(NodeOrToken::into_node)
                .filter(|node| node.text_range() == range);
            let load = item_load(node.as_ref(), loaded.span.slice(&index.text));
            let binding = load
                .binding
                .clone()
                .zip(loaded.value)
                .map(|(name, value)| Binding {
                    name,
                    value,
                    function: load.function,
                });
            let load_writes = item_super_assigned_names(db, item)
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect();
            let (binds, reads) = match &binding {
                Some(binding) if binding.function => (vec![binding.name.clone()], Vec::new()),
                _ => (
                    top_level_writes(db, item)
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    item_immediate_reads(db, item)
                        .iter()
                        .map(|name| name.to_string())
                        .collect(),
                ),
            };
            IndexedItem {
                key: ItemKey {
                    kind: *item.kind(db),
                    name: item.name(db).clone(),
                    disambiguator: *item.disambiguator(db),
                },
                hash: load.hash,
                span: loaded.span,
                binding,
                binds,
                reads,
                mentions: load.mentions,
                load_writes,
                registrations: load.registrations,
                s4: load.s4,
            }
        })
        .collect();
    Arc::new(FileItems {
        hash: fnv1a(index.text.as_bytes()),
        items,
        syntax_errors: index.syntax_errors.clone(),
    })
}

/// Shared, so a host keeps the index it loaded without copying it.
#[salsa::tracked(returns(clone))]
pub fn package_index(db: &dyn Db, sources: PackageSources) -> Arc<PackageIndex> {
    let description = sources.description(db);
    let files: Vec<IndexedFile> = sources
        .files(db)
        .iter()
        .map(|package_file| IndexedFile {
            path: package_file.path.clone(),
            items: file_items(db, package_file.file),
        })
        .collect();
    let bindings = package_bindings(db, sources);
    Arc::new(PackageIndex {
        package: description_field(description, "Package").unwrap_or_default(),
        version: description_field(description, "Version").unwrap_or_else(|| "0.0.0".to_owned()),
        depends: parse_description_depends(description),
        namespace: namespace_spec(db, sources).clone(),
        bindings,
        files,
    })
}

/// The package's [`PackageBindings`]. It reads each file's [`file_binds`], which an edit inside
/// a function body leaves equal, so such an edit reuses the table instead of rebuilding it.
#[salsa::tracked(returns(clone))]
pub fn package_bindings(db: &dyn Db, sources: PackageSources) -> Arc<PackageBindings> {
    let files: Vec<Arc<FileBinds>> = sources
        .files(db)
        .iter()
        .map(|package_file| file_binds(db, sources, package_file.file))
        .collect();
    // Each name's writer count and the position of its first writer, in load order.
    let mut writers: FxHashMap<&str, (u32, usize)> = FxHashMap::default();
    let items = files.iter().flat_map(|file| &file.items);
    for (position, item) in items.enumerate() {
        for name in &item.binds {
            writers.entry(name.as_str()).or_insert((0, position)).0 += 1;
        }
    }
    // `.onLoad` runs after every top-level statement, so its writes come after them all, and
    // a lazy constant forced after it would see the new value where an eager one saw the old.
    for item in files.iter().flat_map(|file| &file.items) {
        for name in &item.load_writes {
            writers.entry(name.as_str()).or_insert((0, usize::MAX)).0 += 1;
        }
    }
    // Seen once and from a lazy definition: lazy. Seen again: eager.
    let mut names: FxHashMap<String, bool> = FxHashMap::default();
    let mut lazy_items: Vec<(&str, (u32, u32))> = Vec::new();
    let items = files.iter().enumerate().flat_map(|(file_position, file)| {
        file.items
            .iter()
            .enumerate()
            .map(move |(item_position, item)| ((file_position as u32, item_position as u32), item))
    });
    let mut located = Vec::new();
    for (position, (at, item)) in items.enumerate() {
        // Strict mode refuses a top-level statement with an effect, so evaluating a constant's
        // value later, at first use, observes the same thing as evaluating it at load,
        // provided every package name the value mentions is written once and before it: a
        // later or second write would change what a late evaluation reads.
        let lazy = match &item.kind {
            BindKind::Function => true,
            BindKind::Constant { mentions } => mentions.iter().all(|name| {
                writers
                    .get(name.as_str())
                    .is_none_or(|&(count, first)| count == 1 && first < position)
            }),
            BindKind::Other => false,
        };
        for name in &item.binds {
            match names.get_mut(name.as_str()) {
                Some(lazy) => *lazy = false,
                None => {
                    names.insert(name.clone(), lazy);
                }
            }
        }
        if let Some(name) = item.binding.as_deref() {
            lazy_items.push((name, at));
        }
        located.push((
            at,
            item.binding.as_deref(),
            matches!(item.kind, BindKind::Constant { .. }),
        ));
    }
    // A binding is lazy when its name is, which only the whole package decides.
    let is_lazy = |name: Option<&str>| name.is_some_and(|name| names.get(name) == Some(&true));
    let eager = located
        .iter()
        .filter(|(_, name, _)| !is_lazy(*name))
        .map(|(at, _, _)| *at)
        .collect();
    let constants = located
        .iter()
        .filter(|(_, name, constant)| *constant && is_lazy(*name))
        .map(|(at, _, _)| *at)
        .collect();
    let lazy = lazy_items
        .into_iter()
        .filter(|(name, _)| is_lazy(Some(name)))
        .map(|(name, at)| (name.to_owned(), at))
        .collect();
    Arc::new(PackageBindings {
        names,
        lazy,
        eager,
        constants,
    })
}

/// What [`package_bindings`] reads of one file's items, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBinds {
    items: Vec<ItemBinds>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemBinds {
    binds: Vec<String>,
    /// The name of a `name <- value` binding, which is lazy when [`PackageBindings::names`]
    /// says so.
    binding: Option<String>,
    /// Names its `<<-` assignments write while `.onLoad` runs.
    load_writes: Vec<String>,
    kind: BindKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindKind {
    /// A function definition, lazy unless another item writes its name.
    Function,
    /// A constant of a strict file, lazy when the names its value mentions allow it.
    Constant {
        mentions: Vec<String>,
    },
    Other,
}

#[salsa::tracked(returns(clone))]
pub fn file_binds(db: &dyn Db, sources: PackageSources, file: SourceFile) -> Arc<FileBinds> {
    let strict = file_is_strict(db, sources, file);
    let loaded = &file_load_index(db, file).items;
    let items = file_items(db, file)
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let function = item
                .binding
                .as_ref()
                .is_some_and(|binding| binding.function);
            let constant = strict
                && is_constant_candidate(item)
                && loaded
                    .get(index)
                    .is_some_and(|loaded| *item_top_level(db, loaded.item) == TopLevel::Constant);
            ItemBinds {
                binds: item.binds.clone(),
                binding: item.binding.as_ref().map(|binding| binding.name.clone()),
                load_writes: item.load_writes.clone(),
                kind: if function {
                    BindKind::Function
                } else if constant {
                    BindKind::Constant {
                        mentions: item.mentions.clone(),
                    }
                } else {
                    BindKind::Other
                },
            }
        })
        .collect();
    Arc::new(FileBinds { items })
}

/// Whether `file` follows strict mode: its own typing directive, else the package default.
pub fn file_is_strict(db: &dyn Db, sources: PackageSources, file: SourceFile) -> bool {
    match file_typing_mode(db, file) {
        Some(TypingMode::Strict) => true,
        Some(TypingMode::On | TypingMode::Off) => false,
        None => *sources.strict(db),
    }
}

/// Classifies the items of a strict `file` that `package_index` may bind as lazy constants,
/// so a host can derive those facts across cores before it joins them.
pub fn classify_constants(db: &dyn Db, sources: PackageSources, file: SourceFile) {
    if !file_is_strict(db, sources, file) {
        return;
    }
    let loaded = &file_load_index(db, file).items;
    for (item, loaded) in file_items(db, file).items.iter().zip(loaded) {
        if is_constant_candidate(item) {
            item_top_level(db, loaded.item);
        }
    }
}

fn is_constant_candidate(item: &IndexedItem) -> bool {
    item.binds.len() == 1
        && item
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.function)
}

/// The names a statement writes at top level. A `setGeneric` item counts its generic, which
/// R binds as a side effect.
fn top_level_writes<'db>(db: &'db dyn Db, item: Item<'db>) -> Vec<&'db str> {
    let mut names: Vec<&str> = crate::item_top_level_names(db, item)
        .iter()
        .map(|name| name.as_str())
        .collect();
    if matches!(*item.kind(db), ItemKind::Function | ItemKind::Value)
        && let Some(name) = item.name(db).as_deref()
        && !names.contains(&name)
    {
        names.push(name);
    }
    names
}

/// `name <- value` (also `=`, `->`, and a quoted name), with the value. `<<-` is excluded: at
/// a namespace's top level it assigns outside the namespace.
pub fn binding_definition(node: &SyntaxNode) -> Option<(String, SyntaxNode)> {
    let binary = BinaryExpr::cast(node.clone())?;
    let (target, value) = match binary.operator()?.kind() {
        SyntaxKind::LESS_MINUS | SyntaxKind::EQ => (binary.lhs()?, binary.rhs()?),
        SyntaxKind::MINUS_GREATER => (binary.rhs()?, binary.lhs()?),
        _ => return None,
    };
    Some((target_name(&target)?, value))
}

fn target_name(target: &SyntaxNode) -> Option<String> {
    if let Some(name) = ast::Name::cast(target.clone()) {
        return name.text();
    }
    string_literal(target)
}

fn string_literal(node: &SyntaxNode) -> Option<String> {
    if node.kind() != SyntaxKind::LITERAL {
        return None;
    }
    let token = node.first_token()?;
    if token.kind() != SyntaxKind::STRING {
        return None;
    }
    Some(string_content(token.text()))
}

/// Registrations by a top-level call, bare or assigned (`Gen <- setClass("Gen", ...)`).
fn registrations(node: &SyntaxNode) -> Vec<Registration> {
    let (call, bound_name) = match BinaryExpr::cast(node.clone()) {
        Some(binary) => {
            let Some(operator) = binary.operator() else {
                return Vec::new();
            };
            let (target, value) = match operator.kind() {
                SyntaxKind::LESS_MINUS | SyntaxKind::EQ | SyntaxKind::LESS2_MINUS => {
                    (binary.lhs(), binary.rhs())
                }
                SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => {
                    (binary.rhs(), binary.lhs())
                }
                _ => return Vec::new(),
            };
            let (Some(target), Some(value)) = (target, value) else {
                return Vec::new();
            };
            (value, target_name(&target))
        }
        None => (node.clone(), None),
    };
    let Some(Call {
        callee, arguments, ..
    }) = call_parts(&call)
    else {
        return Vec::new();
    };
    let argument = |position: usize, name: &str| -> Option<&SyntaxNode> {
        arguments
            .iter()
            .find(|argument| argument.name.as_deref() == Some(name))
            .or_else(|| {
                arguments
                    .iter()
                    .filter(|argument| argument.name.is_none())
                    .nth(position)
            })
            .map(|argument| &argument.value)
    };
    let string_argument =
        |position: usize, name: &str| argument(position, name).and_then(string_literal);
    let registration = match callee.as_str() {
        "setClass" | "setRefClass" | "setClassUnion" => {
            string_argument(0, "Class").map(|class| Registration::S4Class { class })
        }
        "setGeneric" => {
            string_argument(0, "name").map(|generic| Registration::S4Generic { generic })
        }
        "setMethod" | "setReplaceMethod" => string_argument(0, "f").map(|generic| {
            let signature = argument(1, "signature")
                .map(|value| {
                    string_literal(value).map_or_else(
                        || {
                            call_parts(value)
                                .map(|call| {
                                    call.arguments
                                        .iter()
                                        .filter_map(|argument| string_literal(&argument.value))
                                        .collect()
                                })
                                .unwrap_or_default()
                        },
                        |class| vec![class],
                    )
                })
                .unwrap_or_default();
            let generic = if callee == "setReplaceMethod" {
                format!("{generic}<-")
            } else {
                generic
            };
            Registration::S4Method { generic, signature }
        }),
        "R6Class" => string_argument(0, "classname")
            .or(bound_name)
            .map(|class| Registration::R6Class { class }),
        ".S3method" | "registerS3method" => {
            match (string_argument(0, "generic"), string_argument(1, "class")) {
                (Some(generic), Some(class)) => Some(Registration::S3Method { generic, class }),
                _ => None,
            }
        }
        _ => None,
    };
    registration.into_iter().collect()
}

/// The [`S4Definition`] `node` is, if it has that shape.
fn s4_definition(node: &SyntaxNode) -> Option<S4Definition> {
    let (call, generator) = match binding_definition(node) {
        Some((_, value)) => (value, true),
        None => (node.clone(), false),
    };
    let Call {
        callee, arguments, ..
    } = call_parts(&call)?;
    let argument = |position: usize, name: &str| -> Option<&SyntaxNode> {
        arguments
            .iter()
            .find(|argument| argument.name.as_deref() == Some(name))
            .or_else(|| {
                arguments
                    .iter()
                    .filter(|argument| argument.name.is_none())
                    .nth(position)
            })
            .map(|argument| &argument.value)
    };
    let string_argument =
        |position: usize, name: &str| argument(position, name).and_then(string_literal);
    if generator && callee != "setClass" {
        return None;
    }
    match callee.as_str() {
        "setClass" => Some(S4Definition::Class {
            class: string_argument(0, "Class")?,
        }),
        "setGeneric" => {
            let generic = string_argument(0, "name")?;
            (argument(1, "def")?.kind() == SyntaxKind::FUNCTION_DEF)
                .then_some(S4Definition::Generic { generic })
        }
        "setMethod" | "setReplaceMethod" => {
            let generic = string_argument(0, "f")?;
            let signature = argument(1, "signature")?;
            let signature = match string_literal(signature) {
                Some(class) => vec![class],
                None => {
                    let call = call_parts(signature)?;
                    if !matches!(call.callee.as_str(), "signature" | "c") {
                        return None;
                    }
                    call.arguments
                        .iter()
                        .map(|argument| string_literal(&argument.value))
                        .collect::<Option<Vec<String>>>()?
                }
            };
            let generic = if callee == "setReplaceMethod" {
                format!("{generic}<-")
            } else {
                generic
            };
            Some(S4Definition::Method { generic, signature })
        }
        "setValidity" => Some(S4Definition::Validity {
            class: string_argument(0, "Class")?,
        }),
        _ => None,
    }
}

/// The text R evaluates for `node`: inline annotations erased, with their line breaks kept so
/// line numbers survive. Also maps each offset in `marks` (token boundaries of `node`) to its
/// offset in the erased text. Walks the green tree, which allocates nothing per node.
fn erase_for_r(node: &SyntaxNode, mut marks: Vec<TextSize>) -> (String, Marks) {
    marks.sort_unstable();
    marks.dedup();
    let mut eraser = Eraser {
        erased: vec![0; marks.len()],
        marks: Marks {
            source: marks,
            erased: Vec::new(),
        },
        next: 0,
        out: String::with_capacity(usize::from(node.text_range().len())),
    };
    eraser.node(node.green().as_ref(), None, node.text_range().start());
    eraser.marks.erased = eraser.erased;
    (eraser.out, eraser.marks)
}

/// Sorted source offsets and, at the same index, where each landed in the erased text.
struct Marks {
    source: Vec<TextSize>,
    erased: Vec<u32>,
}

impl Marks {
    fn erased_offset(&self, offset: TextSize) -> u32 {
        self.source
            .binary_search(&offset)
            .ok()
            .and_then(|index| self.erased.get(index).copied())
            .unwrap_or(0)
    }
}

/// The walk visits offsets in increasing order, so the marks are matched with one cursor
/// instead of a lookup per token boundary.
struct Eraser {
    marks: Marks,
    erased: Vec<u32>,
    next: usize,
    out: String,
}

impl Eraser {
    fn record(&mut self, offset: TextSize) {
        while let Some(&mark) = self.marks.source.get(self.next) {
            if mark > offset {
                return;
            }
            if mark == offset
                && let Some(slot) = self.erased.get_mut(self.next)
            {
                *slot = self.out.len() as u32;
            }
            self.next += 1;
        }
    }

    fn node(&mut self, green: &GreenNodeData, parent: Option<SyntaxKind>, start: TextSize) {
        let kind = RLanguage::kind_from_raw(green.kind());
        self.record(start);
        let end = start + green.text_len();
        if syntax::is_green_inline_annotation(kind, parent) {
            let breaks = count_breaks(green);
            self.out.extend(std::iter::repeat_n('\n', breaks));
            self.record(end);
            return;
        }
        let mut offset = start;
        for child in green.children() {
            match child {
                NodeOrToken::Node(child) => {
                    self.node(child, Some(kind), offset);
                    offset += child.text_len();
                }
                NodeOrToken::Token(token) => {
                    self.record(offset);
                    self.out.push_str(token.text());
                    offset += token.text_len();
                    self.record(offset);
                }
            }
        }
        self.record(end);
    }
}

fn collect_identifiers<'a>(green: &'a GreenNodeData, names: &mut Vec<&'a str>) {
    for child in green.children() {
        match child {
            NodeOrToken::Node(child) => collect_identifiers(child, names),
            NodeOrToken::Token(token)
                if RLanguage::kind_from_raw(token.kind()) == SyntaxKind::IDENT =>
            {
                let text = token.text();
                let name = text
                    .strip_prefix('`')
                    .and_then(|text| text.strip_suffix('`'))
                    .unwrap_or(text);
                names.push(name);
            }
            NodeOrToken::Token(_) => {}
        }
    }
}

fn count_breaks(green: &GreenNodeData) -> usize {
    green
        .children()
        .map(|child| match child {
            NodeOrToken::Node(child) => count_breaks(child),
            NodeOrToken::Token(token) => token.text().matches('\n').count(),
        })
        .sum()
}

struct LineStarts(Vec<u32>);

impl LineStarts {
    fn new(text: &str) -> LineStarts {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index as u32 + 1),
        );
        LineStarts(starts)
    }

    /// 1-based.
    fn line_of(&self, offset: u32) -> u32 {
        self.0.partition_point(|&start| start <= offset) as u32
    }

    fn span(&self, text: &str, start: u32, end: u32) -> SourceSpan {
        let line = self.line_of(start);
        let line_start = self.0.get(line as usize - 1).copied().unwrap_or(0);
        let column = text
            .get(line_start as usize..start as usize)
            .map_or(0, |prefix| prefix.chars().count() as u32);
        SourceSpan {
            line,
            column,
            start,
            end,
            char_start: start,
            char_end: end,
            source_start: start,
            source_end: end,
        }
    }
}

/// The character offset of each byte offset, in one pass over the text.
fn character_offsets(text: &str, bytes: impl Iterator<Item = u32>) -> FxHashMap<u32, u32> {
    let mut wanted: Vec<u32> = bytes.collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut result = FxHashMap::default();
    if text.is_ascii() {
        result.extend(wanted.into_iter().map(|offset| (offset, offset)));
        return result;
    }
    let mut next = wanted.into_iter().peekable();
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain([text.len()]);
    for (characters, index) in boundaries.enumerate() {
        while let Some(&offset) = next.peek() {
            if offset as usize > index {
                break;
            }
            result.insert(offset, characters as u32);
            next.next();
        }
    }
    result
}

/// FNV-1a: stable across processes and Rust versions, which a persisted index needs.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}
