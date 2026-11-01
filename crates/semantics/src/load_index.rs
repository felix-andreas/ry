//! The package-load index: what a loader needs to know about a package's top-level items to
//! load it lazily and to reload exactly what changed.
//!
//! Every fact comes from a per-item query (`item_load`, `item_top_level_names`,
//! `item_immediate_reads`) or from one file's own text (`file_load_index`), so an edit inside
//! one item re-derives that item alone. `package_index` only joins those per-item results.

use crate::hir::Symbol;
use crate::metadata::{
    Call, NamespaceSpec, call_parts, description_field, parse_description_depends,
    parse_namespace_spec, string_content,
};
use crate::{Db, Item, ItemKind, SourceFile, item_naming, item_spans, item_syntax, parse};
use rowan::{GreenNodeData, Language, NodeOrToken};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use syntax::ast::{self, BinaryExpr};
use syntax::{RLanguage, SyntaxKind, SyntaxNode, TextRange, TextSize};

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
    /// `Some(name)` for `name <- function(...) ...`, the shape a loader may bind lazily.
    pub function: Option<String>,
    /// For a function definition, every name its body mentions: a superset of what calling
    /// it reads, found without name resolution.
    pub mentions: Vec<String>,
    pub registrations: Vec<Registration>,
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

#[salsa::tracked(returns(ref))]
pub fn item_load<'db>(db: &'db dyn Db, item: Item<'db>) -> ItemLoad {
    let Some(syntax) = item_syntax(db, item) else {
        return ItemLoad {
            hash: 0,
            function: None,
            mentions: Vec::new(),
            registrations: Vec::new(),
        };
    };
    let node = syntax.syntax_node();
    let (text, _) = erase_for_r(&node, &[]);
    let definition = function_definition(&node);
    let mentions = definition.as_ref().map_or_else(Vec::new, |(_, value)| {
        let mut names = BTreeSet::new();
        collect_identifiers(value.green().as_ref(), &mut names);
        names.into_iter().collect()
    });
    ItemLoad {
        hash: fnv1a(text.as_bytes()),
        function: definition.map(|(name, _)| name),
        mentions,
        registrations: registrations(&node),
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
    /// The function literal of a function definition, which is what a lazy binding evaluates.
    pub function: Option<SourceSpan>,
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
    let mut function_ranges: Vec<Option<TextRange>> = Vec::with_capacity(spans.len());
    for span in spans {
        marks.push(span.range.start());
        marks.push(span.range.end());
        let function = root
            .child_or_token_at_range(span.range)
            .and_then(NodeOrToken::into_node)
            .filter(|node| node.text_range() == span.range)
            .and_then(|node| function_definition(&node).map(|(_, value)| value.text_range()));
        if let Some(range) = function {
            marks.push(range.start());
            marks.push(range.end());
        }
        function_ranges.push(function);
    }
    let (text, offsets) = erase_for_r(&root, &marks);
    let lines = LineStarts::new(&text);
    let characters = character_offsets(&text, offsets.values().copied());
    let position = |offset: TextSize| offsets.get(&offset).copied().unwrap_or(0);
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
        .zip(function_ranges)
        .map(|(span, function)| LoadedItem {
            item: span.item,
            span: span_of(span.range),
            function: function.map(span_of),
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
    /// Every top-level name the package binds, mapped to whether it is bound lazily: a name is
    /// lazy when its one writer defines a function. Any other writer, including conditional or
    /// replacement code (`f <- memoise(f)`), makes every writer of the name run eagerly.
    pub names: FxHashMap<String, bool>,
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
    /// For `name <- function(...) ...`, the name and the function literal. Such an item is
    /// bound lazily unless another item writes the name too ([`PackageIndex::names`]).
    pub function: Option<(String, SourceSpan)>,
    /// Top-level names it writes.
    pub binds: Vec<String>,
    /// Names it reads while running (see [`item_immediate_reads`]). A function definition
    /// reads nothing until it is called.
    pub reads: Vec<String>,
    /// For a function definition, the names its body mentions, which is what a call to it
    /// may read.
    pub mentions: Vec<String>,
    pub registrations: Vec<Registration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub kind: ItemKind,
    pub name: Option<String>,
    pub disambiguator: u32,
}

impl PackageIndex {
    /// The name `item` binds lazily, if it is a definition no other item writes.
    pub fn lazy_name<'a>(&self, item: &'a IndexedItem) -> Option<&'a str> {
        item.function
            .as_ref()
            .map(|(name, _)| name.as_str())
            .filter(|name| self.is_lazy(name))
    }

    pub fn is_lazy(&self, name: &str) -> bool {
        self.names.get(name).copied().unwrap_or(false)
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
    let items = index
        .items
        .iter()
        .map(|loaded| {
            let item = loaded.item;
            let load = item_load(db, item);
            let function = load.function.clone().zip(loaded.function);
            let (binds, reads) = match &function {
                Some((name, _)) => (vec![name.clone()], Vec::new()),
                None => (
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
                function,
                binds,
                reads,
                mentions: load.mentions.clone(),
                registrations: load.registrations.clone(),
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
    // Seen once and from a function definition: lazy. Seen again: eager.
    let mut names: FxHashMap<String, bool> = FxHashMap::default();
    for file in &files {
        for item in &file.items.items {
            for name in &item.binds {
                match names.get_mut(name.as_str()) {
                    Some(lazy) => *lazy = false,
                    None => {
                        names.insert(name.clone(), item.function.is_some());
                    }
                }
            }
        }
    }
    Arc::new(PackageIndex {
        package: description_field(description, "Package").unwrap_or_default(),
        version: description_field(description, "Version").unwrap_or_else(|| "0.0.0".to_owned()),
        depends: parse_description_depends(description),
        namespace: namespace_spec(db, sources).clone(),
        names,
        files,
    })
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

/// `name <- function(...) ...` (also `=`, `->`, and a quoted name), with the function literal.
/// `<<-` is excluded: at a namespace's top level it assigns outside the namespace.
pub fn function_definition(node: &SyntaxNode) -> Option<(String, SyntaxNode)> {
    let binary = BinaryExpr::cast(node.clone())?;
    let (target, value) = match binary.operator()?.kind() {
        SyntaxKind::LESS_MINUS | SyntaxKind::EQ => (binary.lhs()?, binary.rhs()?),
        SyntaxKind::MINUS_GREATER => (binary.rhs()?, binary.lhs()?),
        _ => return None,
    };
    if value.kind() != SyntaxKind::FUNCTION_DEF {
        return None;
    }
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

/// The text R evaluates for `node`: inline annotations erased, with their line breaks kept so
/// line numbers survive. Also maps each offset in `marks` (token boundaries of `node`) to its
/// offset in the erased text. Walks the green tree, which allocates nothing per node.
fn erase_for_r(node: &SyntaxNode, marks: &[TextSize]) -> (String, FxHashMap<TextSize, u32>) {
    let mut eraser = Eraser {
        wanted: marks.iter().copied().collect(),
        offsets: FxHashMap::default(),
        out: String::with_capacity(usize::from(node.text_range().len())),
    };
    eraser.node(node.green().as_ref(), None, node.text_range().start());
    (eraser.out, eraser.offsets)
}

struct Eraser {
    wanted: FxHashSet<TextSize>,
    offsets: FxHashMap<TextSize, u32>,
    out: String,
}

impl Eraser {
    fn record(&mut self, offset: TextSize) {
        if self.wanted.contains(&offset) {
            self.offsets.entry(offset).or_insert(self.out.len() as u32);
        }
    }

    fn node(&mut self, green: &GreenNodeData, parent: Option<SyntaxKind>, start: TextSize) {
        let kind = RLanguage::kind_from_raw(green.kind());
        self.record(start);
        let end = start + green.text_len();
        // Mirrors `syntax::is_inline_annotation`, on kinds.
        let annotation = match kind {
            SyntaxKind::TYPE_ASCRIPTION
            | SyntaxKind::RETURN_TYPE
            | SyntaxKind::TYPE_DEFINITION
            | SyntaxKind::NEW_TYPE => true,
            SyntaxKind::TYPE_BINDER_LIST => parent == Some(SyntaxKind::FUNCTION_DEF),
            _ => false,
        };
        if annotation {
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

fn collect_identifiers(green: &GreenNodeData, names: &mut BTreeSet<String>) {
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
                names.insert(name.to_owned());
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
