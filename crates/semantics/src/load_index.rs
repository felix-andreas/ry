//! The package-load index: what a loader needs to know about a package's top-level items to
//! load it lazily and to reload exactly what changed.
//!
//! Every fact comes from a per-item query (`item_load`, `item_top_level_names`,
//! `item_immediate_reads`) or from one file's own text (`file_load_index`), so an edit inside
//! one item re-derives that item alone. `package_index` only joins those per-item results.

use crate::metadata::{
    NamespaceSpec, argument_name, description_field, parse_description_depends,
    parse_namespace_spec,
};
use crate::{Db, Item, ItemKind, SourceFile, item_naming, item_spans, item_syntax, parse};
use rowan::{NodeOrToken, WalkEvent};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use syntax::ast::{self, BinaryExpr, is_expression_kind};
use syntax::{SyntaxKind, SyntaxNode, TextRange, TextSize};

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
            registrations: Vec::new(),
        };
    };
    let node = syntax.syntax_node();
    let (text, _) = erase_for_r(&node, &[]);
    ItemLoad {
        hash: fnv1a(text.as_bytes()),
        function: function_definition(&node).map(|(name, _)| name),
        registrations: registrations(&node),
    }
}

/// Names an item reads while it runs, so not reads inside a function it defines: those happen
/// when the function is called and find the current binding then. A reload reruns a statement
/// whose immediate reads changed, because it captured their old values.
#[salsa::tracked(returns(ref))]
pub fn item_immediate_reads<'db>(db: &'db dyn Db, item: Item<'db>) -> BTreeSet<String> {
    let Some(naming) = item_naming(db, item) else {
        return BTreeSet::new();
    };
    naming
        .non_locals
        .iter()
        .filter(|(expression, _)| !naming.deferred_non_locals.contains(expression))
        .map(|(_, name)| name.clone())
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
    pub start: u32,
    pub end: u32,
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
    let position = |offset: TextSize| offsets.get(&offset).copied().unwrap_or(0);
    let span_of = |range: TextRange| {
        let start = position(range.start());
        let end = position(range.end()).max(start);
        lines.span(&text, start, end)
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
    /// Collation order.
    pub files: Vec<IndexedFile>,
    /// Collation order, which is the order R runs top-level code in.
    pub items: Vec<IndexedItem>,
    /// `(path, line, message)` for R syntax errors, which R would refuse to source.
    pub syntax_errors: Vec<(String, u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub path: String,
    /// Of the text R reads, so an edit anywhere in the file changes it.
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedItem {
    pub path: String,
    /// Stable across insertions elsewhere: the analysis' item identity without the file handle.
    pub key: ItemKey,
    pub hash: u64,
    pub span: SourceSpan,
    pub binding: Binding,
    pub registrations: Vec<Registration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub kind: String,
    pub name: Option<String>,
    pub disambiguator: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    /// A function definition whose name no other item writes: the loader binds it lazily,
    /// and nothing runs until the first use.
    Lazy { name: String, function: SourceSpan },
    /// Top-level code that runs at load time, in collation order.
    Eager {
        /// Top-level names it writes.
        binds: Vec<String>,
        /// Names it reads while running (see [`item_immediate_reads`]).
        reads: Vec<String>,
    },
}

#[salsa::tracked(returns(ref))]
pub fn namespace_spec(db: &dyn Db, sources: PackageSources) -> NamespaceSpec {
    parse_namespace_spec(sources.namespace(db))
}

#[salsa::tracked(returns(ref))]
pub fn package_index(db: &dyn Db, sources: PackageSources) -> PackageIndex {
    let description = sources.description(db);
    let files = sources.files(db);
    // A name with one writer that defines a function is lazy; any other writer, including
    // conditional or replacement code (`f <- memoise(f)`), makes every writer eager.
    let mut writers: FxHashMap<&str, u32> = FxHashMap::default();
    let mut loads = Vec::new();
    for package_file in files {
        let index = file_load_index(db, package_file.file);
        for loaded in &index.items {
            let load = item_load(db, loaded.item);
            let binds: Vec<&str> = match &load.function {
                Some(name) => vec![name.as_str()],
                None => top_level_writes(db, loaded.item),
            };
            for name in &binds {
                *writers.entry(name).or_default() += 1;
            }
            loads.push((package_file, loaded, load, binds));
        }
    }
    let mut items = Vec::with_capacity(loads.len());
    for (package_file, loaded, load, binds) in &loads {
        let lazy = load
            .function
            .as_deref()
            .zip(loaded.function)
            .filter(|(name, _)| writers.get(name) == Some(&1));
        let binding = match lazy {
            Some((name, function)) => Binding::Lazy {
                name: name.to_owned(),
                function,
            },
            None => Binding::Eager {
                binds: binds.iter().map(|name| (*name).to_owned()).collect(),
                reads: item_immediate_reads(db, loaded.item)
                    .iter()
                    .cloned()
                    .collect(),
            },
        };
        let item = loaded.item;
        items.push(IndexedItem {
            path: package_file.path.clone(),
            key: ItemKey {
                kind: format!("{:?}", item.kind(db)).to_lowercase(),
                name: item.name(db).clone(),
                disambiguator: *item.disambiguator(db),
            },
            hash: load.hash,
            span: loaded.span,
            binding,
            registrations: load.registrations.clone(),
        });
    }
    let files_index = files
        .iter()
        .map(|package_file| IndexedFile {
            path: package_file.path.clone(),
            hash: fnv1a(file_load_index(db, package_file.file).text.as_bytes()),
        })
        .collect();
    let syntax_errors = files
        .iter()
        .flat_map(|package_file| {
            file_load_index(db, package_file.file)
                .syntax_errors
                .iter()
                .map(|(line, message)| (package_file.path.clone(), *line, message.clone()))
        })
        .collect();
    PackageIndex {
        package: description_field(description, "Package").unwrap_or_default(),
        version: description_field(description, "Version").unwrap_or_else(|| "0.0.0".to_owned()),
        depends: parse_description_depends(description),
        namespace: namespace_spec(db, sources).clone(),
        files: files_index,
        items,
        syntax_errors,
    }
}

/// The names a statement writes at top level. A `setGeneric` item counts its generic, which
/// R binds as a side effect.
fn top_level_writes<'db>(db: &'db dyn Db, item: Item<'db>) -> Vec<&'db str> {
    let mut names: Vec<&str> = crate::item_top_level_names(db, item)
        .iter()
        .map(String::as_str)
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
    let text = token.text();
    let inner = text.get(1..text.len().checked_sub(1)?)?;
    Some(inner.to_owned())
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
    let Some((callee, arguments)) = call_parts(&call) else {
        return Vec::new();
    };
    let argument = |position: usize, name: &str| -> Option<&SyntaxNode> {
        arguments
            .iter()
            .find(|(argument_name, _)| argument_name.as_deref() == Some(name))
            .or_else(|| {
                arguments
                    .iter()
                    .filter(|(argument_name, _)| argument_name.is_none())
                    .nth(position)
            })
            .map(|(_, value)| value)
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
                                .map(|(_, arguments)| {
                                    arguments
                                        .iter()
                                        .filter_map(|(_, value)| string_literal(value))
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

/// A call's callee name (the last part of `pkg::name`) and its arguments.
fn call_parts(node: &SyntaxNode) -> Option<(String, Vec<(Option<String>, SyntaxNode)>)> {
    if node.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let callee = node.children().next()?;
    let callee_name = match callee.kind() {
        SyntaxKind::NAME => ast::Name::cast(callee)?.text()?,
        SyntaxKind::NAMESPACE_EXPR => callee
            .children()
            .filter(|child| child.kind() == SyntaxKind::NAME)
            .last()
            .and_then(ast::Name::cast)?
            .text()?,
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
                    Some((argument_name(&argument), value))
                })
                .collect()
        })
        .unwrap_or_default();
    Some((callee_name, arguments))
}

/// The text R evaluates for `node`: inline annotations erased, with their line breaks kept so
/// line numbers survive. Also maps each offset in `marks` (token boundaries of `node`) to its
/// offset in the erased text.
fn erase_for_r(node: &SyntaxNode, marks: &[TextSize]) -> (String, FxHashMap<TextSize, u32>) {
    let wanted: FxHashSet<TextSize> = marks.iter().copied().collect();
    let mut offsets = FxHashMap::default();
    let mut out = String::with_capacity(usize::from(node.text_range().len()));
    let mut record = |offset: TextSize, out: &String| {
        if wanted.contains(&offset) {
            offsets.entry(offset).or_insert(out.len() as u32);
        }
    };
    let mut preorder = node.preorder_with_tokens();
    while let Some(event) = preorder.next() {
        match event {
            WalkEvent::Enter(NodeOrToken::Token(token)) => {
                record(token.text_range().start(), &out);
                out.push_str(token.text());
                record(token.text_range().end(), &out);
            }
            WalkEvent::Enter(NodeOrToken::Node(child)) if syntax::is_inline_annotation(&child) => {
                record(child.text_range().start(), &out);
                let breaks = child.text().to_string().matches('\n').count();
                out.extend(std::iter::repeat_n('\n', breaks));
                record(child.text_range().end(), &out);
                preorder.skip_subtree();
            }
            WalkEvent::Enter(NodeOrToken::Node(child)) => {
                record(child.text_range().start(), &out);
            }
            WalkEvent::Leave(NodeOrToken::Node(child)) => {
                record(child.text_range().end(), &out);
            }
            WalkEvent::Leave(NodeOrToken::Token(_)) => {}
        }
    }
    (out, offsets)
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
        }
    }
}

/// FNV-1a: stable across processes and Rust versions, which a persisted index needs.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}
