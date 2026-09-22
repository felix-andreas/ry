//! Semantic analysis over `syntax` trees: the salsa database and all queries.
//!
//! The pipeline is a set of memoized, dependency-tracked queries: parse →
//! per-item item tree → HIR → naming → inference, with interned types and a
//! symbol-granular package interface resolved through salsa fixpoint cycles.
//! Analysis units are *items* (top-level definitions and nested definitions in
//! class-constructor calls and function bodies), never whole files, so an edit
//! recomputes only the items whose derived values actually changed.

pub mod annotations;
pub mod check;
pub mod diagnostics;
pub mod hir;
pub mod infer;
pub mod lints;
pub mod metadata;
pub mod naming;
pub mod stubs;
pub mod testing;
pub mod types;

use rustc_hash::FxHashSet;
use std::collections::BTreeSet;
use syntax::Parse;
use syntax::ast::AstNode as _;

#[salsa::db]
pub trait Db: salsa::Database {
    /// The splice-reparse acceleration cache consulted by `parse`. Purely an
    /// acceleration: the spliced tree is byte- and error-identical to a
    /// from-scratch parse, so cache state never changes any query value.
    fn splice_cache(&self) -> &SpliceCache;
}

#[salsa::db]
#[derive(Clone, Default)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
    /// Shared across storage-handle clones so the server's fresh handles
    /// (cancellation refreshes, worker fan-out) keep the warm entries.
    splice_cache: std::sync::Arc<SpliceCache>,
}

#[salsa::db]
impl salsa::Database for RootDatabase {}

#[salsa::db]
impl Db for RootDatabase {
    fn splice_cache(&self) -> &SpliceCache {
        &self.splice_cache
    }
}

/// Whether a file participates in the package interface or is a standalone
/// script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum DocumentKind {
    Package,
    Script,
}

/// One source document: the text is the ground-truth input; everything else is
/// derived.
#[salsa::input(debug)]
pub struct SourceFile {
    #[returns(deref)]
    pub text: String,
    pub kind: DocumentKind,
}

/// The lossless parse of a file. Per text revision the query consults the
/// splice cache: when the file's previous text and tree are at hand, only the
/// edited statement region is reparsed and the untouched green subtrees are
/// shared by pointer through `syntax::reparse`. Otherwise, and whenever the
/// splice refuses, the query does a full parse from scratch. The two paths are
/// byte-identical and error-identical, so the cache is invisible to every
/// downstream query. The syntax edit-stream fuzzer pins that equivalence, and
/// the semantics fuzzer's setter-edit-equals-fresh-database invariant crosses
/// this cache.
#[salsa::tracked(returns(clone))]
pub fn parse(db: &dyn Db, file: SourceFile) -> ParseResult {
    ParseResult(db.splice_cache().parse(file, file.text(db)))
}

/// The previous `(text, parse)` per file, keyed by the salsa input id.
///
/// Correctness never depends on an entry: a stale or mismatched text only
/// yields a larger derived edit region, and `syntax::reparse` falls back to a
/// full parse whenever splicing is not provably equivalent. The map is
/// bounded. At capacity, inserting an unknown file clears the whole map. That
/// is crude, but a cold pass cycling thousands of files then costs one clear
/// instead of an eviction policy, while an editing session's open set stays
/// resident.
#[derive(Default)]
pub struct SpliceCache {
    entries: std::sync::Mutex<rustc_hash::FxHashMap<SourceFile, (std::sync::Arc<str>, Parse)>>,
}

const SPLICE_CACHE_CAPACITY: usize = 64;

impl SpliceCache {
    fn parse(&self, file: SourceFile, new_text: &str) -> Parse {
        // Clone the entry out and compute unlocked, so parallel workers
        // parsing different files never serialize on the parse itself.
        let previous = {
            let entries = self.entries.lock().expect("splice cache lock");
            entries.get(&file).cloned()
        };
        let parse = match &previous {
            Some((old_text, old_parse)) => {
                if **old_text == *new_text {
                    old_parse.clone()
                } else {
                    let (deleted, inserted) = derived_edit(old_text, new_text);
                    syntax::reparse(old_parse, new_text, deleted, inserted)
                }
            }
            None => syntax::parse(new_text),
        };
        let mut entries = self.entries.lock().expect("splice cache lock");
        if entries.len() >= SPLICE_CACHE_CAPACITY && !entries.contains_key(&file) {
            entries.clear();
        }
        entries.insert(file, (std::sync::Arc::from(new_text), parse.clone()));
        parse
    }
}

/// The single edit turning `old` into `new`: the byte range replaced in `old`
/// and the length of its replacement, from the longest common prefix and
/// suffix (backed off to character boundaries). Several accumulated edits
/// collapse into one region spanning them all.
fn derived_edit(old: &str, new: &str) -> (syntax::TextRange, syntax::TextSize) {
    let mut prefix = old
        .as_bytes()
        .iter()
        .zip(new.as_bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let limit = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .as_bytes()
        .iter()
        .rev()
        .zip(new.as_bytes().iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(limit);
    while !old.is_char_boundary(old.len() - suffix) {
        suffix -= 1;
    }
    (
        syntax::TextRange::new(
            syntax::TextSize::from(prefix as u32),
            syntax::TextSize::from((old.len() - suffix) as u32),
        ),
        syntax::TextSize::from((new.len() - suffix - prefix) as u32),
    )
}

/// Newtype giving `syntax::Parse` the salsa value plumbing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseResult(pub Parse);

impl std::ops::Deref for ParseResult {
    type Target = Parse;

    fn deref(&self) -> &Parse {
        &self.0
    }
}

/// The analysis unit, which is one top-level definition or statement. A nested
/// definition inside a class-constructor call or a function body becomes an
/// item in a later slice, and the identity scheme already carries `parent` for
/// those.
///
/// Identity is **insertion-stable**. It is the kind, the name, the parent, and
/// a disambiguator among same-identity siblings, and never a bare position or
/// index. Inserting an unrelated item must not shift the identity of the items
/// after it, or every downstream memo for them would invalidate.
#[salsa::interned(debug)]
pub struct Item<'db> {
    pub file: SourceFile,
    pub kind: ItemKind,
    #[returns(ref)]
    pub name: Option<String>,
    pub parent: Option<Item<'db>>,
    pub disambiguator: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ItemKind {
    /// `name <- function(...) ...` (any assignment spelling).
    Function,
    /// `name <- <expr>` for a non-function right-hand side.
    Value,
    /// Any other top-level statement (an expression statement, a side-effecting
    /// call, a broken region).
    Statement,
}

/// The ordered items of a file. This is the invalidation barrier between a
/// file's text and per-item work. It carries structure and identity only, with
/// no spans and no bodies, so an edit inside one body leaves it equal and cuts
/// off.
///
/// It is borrowed rather than cloned. Per-item work consults it once per item,
/// so cloning the whole vector here allocated a copy of the file's item list
/// for every item in the file. That is quadratic in a file's top-level
/// items.
#[salsa::tracked(returns(ref))]
pub fn item_tree<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<Item<'db>> {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut counts: rustc_hash::FxHashMap<(ItemKind, Option<String>), u32> =
        rustc_hash::FxHashMap::default();
    let mut items = Vec::new();
    for node in root.children() {
        if !syntax::ast::is_expression_kind(node.kind()) && node.kind() != syntax::SyntaxKind::ERROR
        {
            continue;
        }
        let (kind, name) = classify_top_level(&node);
        let disambiguator = {
            let counter = counts.entry((kind, name.clone())).or_insert(0);
            let current = *counter;
            *counter += 1;
            current
        };
        items.push(Item::new(db, file, kind, name, None, disambiguator));
    }
    items
}

/// The current green subtree of an item (with its item-tree position), or
/// `None` when the item no longer exists in the file.
///
/// The green subtree is width-only and therefore **position-independent**: an
/// edit elsewhere in the file reproduces a structurally equal value here, and
/// salsa's early cutoff prunes all downstream per-item work. Every consumer of
/// item innards must read THIS query (never `parse` directly), and every span
/// it derives must stay item-relative; absolute positions are reintroduced only
/// at the final rendering edge.
#[salsa::tracked(returns(clone))]
pub fn item_syntax<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<ItemSyntax> {
    resolve_item_node(db, item).map(|node| ItemSyntax(node.green().into()))
}

/// One item's current absolute span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemSpan<'db> {
    pub item: Item<'db>,
    pub range: syntax::TextRange,
}

/// Every item's current absolute span, in one memoized walk. A per-item lookup
/// would otherwise re-classify the whole file once per item, which is
/// quadratic on a statement-heavy script.
#[salsa::tracked(returns(ref))]
pub fn item_spans<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<ItemSpan<'db>> {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut counts: rustc_hash::FxHashMap<(ItemKind, Option<String>), u32> =
        rustc_hash::FxHashMap::default();
    let mut spans = Vec::new();
    for node in root.children() {
        if !syntax::ast::is_expression_kind(node.kind()) && node.kind() != syntax::SyntaxKind::ERROR
        {
            continue;
        }
        let (kind, name) = classify_top_level(&node);
        let disambiguator = {
            let counter = counts.entry((kind, name.clone())).or_insert(0);
            let current = *counter;
            *counter += 1;
            current
        };
        spans.push(ItemSpan {
            item: Item::new(db, file, kind, name, None, disambiguator),
            range: node.text_range(),
        });
    }
    spans
}

/// The item's current red node inside the FILE tree, with absolute offsets.
/// This is an EDGE-ONLY view. The rendering edge and the position-addressed
/// IDE features use it to convert between absolute and item-relative offsets.
/// Everything that computes a derived per-item value must go through the
/// position-independent `item_syntax`, or an edit elsewhere in the file stops
/// cutting off.
pub fn item_node<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<syntax::SyntaxNode> {
    resolve_item_node(db, item)
}

pub(crate) fn resolve_item_node<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
) -> Option<syntax::SyntaxNode> {
    let range = item_span_range(db, item)?;
    let parse = parse(db, *item.file(db));
    // Descend by range rather than scanning the root's children: every
    // per-item query lands here, so a linear child walk makes a file of many
    // top-level statements quadratic.
    parse
        .syntax_node()
        .child_or_token_at_range(range)
        .and_then(|element| element.into_node())
        .filter(|node| node.text_range() == range)
}

/// One item's current absolute span, as a hash probe. Every per-item query
/// needs it, so scanning `item_spans` here would make a file of many
/// top-level statements quadratic.
pub(crate) fn item_span_range<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<syntax::TextRange> {
    let file = *item.file(db);
    let index = *item_span_positions(db, file).get(&item)?;
    item_spans(db, file).get(index).map(|span| span.range)
}

/// Each item's index in `item_tree`. This is a lookup index over that one
/// source of truth, so a per-item read rule can ask where an item sits without
/// scanning the file's item list once per item.
#[salsa::tracked(returns(ref))]
fn item_tree_positions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> rustc_hash::FxHashMap<Item<'db>, usize> {
    item_tree(db, file)
        .iter()
        .enumerate()
        .map(|(index, &item)| (item, index))
        .collect()
}

/// Each item's index in `item_spans`. This is a lookup index over that one
/// source of truth, not a second copy of the spans.
#[salsa::tracked(returns(ref))]
fn item_span_positions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> rustc_hash::FxHashMap<Item<'db>, usize> {
    item_spans(db, file)
        .iter()
        .enumerate()
        .map(|(index, span)| (span.item, index))
        .collect()
}

/// A position-independent green subtree; equality is structural.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemSyntax(pub rowan::GreenNode);

impl ItemSyntax {
    /// A fresh red root over the item's subtree. Offsets start at 0, so they
    /// are item-relative, which is exactly what a per-item consumer must work
    /// in.
    pub fn syntax_node(&self) -> syntax::SyntaxNode {
        syntax::SyntaxNode::new_root(self.0.clone())
    }
}

/// The lowered HIR of one item. It is derived from the item's
/// position-independent green subtree alone, and never from the whole file, so
/// it stays equal and cuts off across an edit elsewhere in the file. It is
/// `None` when the item no longer exists.
///
/// Borrowed, not cloned: a check, the diagnostics passes and every IDE read all
/// fetch this, roughly five times per item over a run, and cloning a `Module`
/// walks its whole expression arena each time.
#[salsa::tracked(returns(ref))]
pub fn item_hir<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<hir::Module> {
    let syntax = item_syntax(db, item)?;
    Some(hir::lower_item(&syntax.syntax_node()))
}

/// The naming facts of one item (position-independent, like the HIR they are
/// derived from). Borrowed for the same reason as [`item_hir`], and more so:
/// `ItemNaming` is maps and sets throughout, so a clone is a node-by-node
/// allocation walk.
#[salsa::tracked(returns(ref))]
pub fn item_naming<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<naming::ItemNaming> {
    let module = item_hir(db, item).as_ref()?;
    let masked_verbs = stubs::stubs(db)
        .map(|library| library.masked.clone())
        .unwrap_or_default();
    Some(naming::resolve_item_with_masked_verbs(
        module,
        &masked_verbs,
    ))
}

/// Names the checker recognizes structurally rather than through the stub
/// corpus. They are the shape-constructing builtins and the control-flow
/// constructs.
const BUILTIN_GLOBAL_NAMES: &[&str] = &["c", "list", "switch", "return", "stop"];

/// Whether any global definition with this name exists. That is a package
/// definition, a standard-library stub declaration, or a checker builtin. It
/// silences the could-not-resolve finding on a name the interface will
/// serve.
pub fn package_scheme_exists(db: &dyn Db, name: &str) -> bool {
    if BUILTIN_GLOBAL_NAMES.contains(&name) {
        return true;
    }
    if ProjectFiles::try_get(db)
        .map(|files| package_definitions(db, files).contains_key(name))
        .unwrap_or(false)
    {
        return true;
    }
    stubs::stubs(db).is_some_and(|library| {
        library.schemes.contains_key(name)
            || library.nominals.contains(name)
            || library.known_exports.contains(name)
    })
}

/// Where a `#:` annotation's target expression is, if it has one. An
/// annotation applies only to the expression starting on the very next
/// line: a blank line, an interposed plain comment, or the end of the
/// statement sequence break the association (and earn a diagnostic when the
/// block needed a target).
pub enum AnnotationTarget {
    Attached(syntax::SyntaxNode),
    /// An element follows, but only after one or more blank lines.
    BlankLineSeparated,
    /// Nothing attachable follows: end of the sequence, or a plain comment
    /// or another annotation region interposes.
    Dangling,
}

/// Each `ANNOTATION` child of one statement sequence, with its attachment. The
/// sequence is the file root or a braced block. This is the single source of
/// the association rule. Annotation application at the top level, through
/// `item_annotation_syntax`, expression-level attachment inside an item,
/// through `item_expression_annotations`, and the dangling-annotation
/// diagnostics all read it.
pub fn statement_annotations(
    parent: &syntax::SyntaxNode,
) -> Vec<(syntax::SyntaxNode, AnnotationTarget)> {
    let mut associations: Vec<(syntax::SyntaxNode, AnnotationTarget)> = Vec::new();
    let mut pending: Option<(syntax::SyntaxNode, usize)> = None;
    for child in parent.children_with_tokens() {
        match child {
            rowan::NodeOrToken::Node(node) if node.kind() == syntax::SyntaxKind::ANNOTATION => {
                if let Some((previous, newlines)) = pending.take() {
                    let target = if newlines >= 2 {
                        AnnotationTarget::BlankLineSeparated
                    } else {
                        AnnotationTarget::Dangling
                    };
                    associations.push((previous, target));
                }
                pending = Some((node, 0));
            }
            rowan::NodeOrToken::Node(node) => {
                if let Some((annotation, newlines)) = pending.take() {
                    let attachable = syntax::ast::is_expression_kind(node.kind())
                        || node.kind() == syntax::SyntaxKind::ERROR;
                    let target = if newlines >= 2 {
                        AnnotationTarget::BlankLineSeparated
                    } else if attachable {
                        AnnotationTarget::Attached(node)
                    } else {
                        AnnotationTarget::Dangling
                    };
                    associations.push((annotation, target));
                }
            }
            rowan::NodeOrToken::Token(token) => match token.kind() {
                syntax::SyntaxKind::WHITESPACE => {}
                syntax::SyntaxKind::NEWLINE => {
                    if let Some((_, newlines)) = pending.as_mut() {
                        *newlines += 1;
                    }
                }
                syntax::SyntaxKind::COMMENT => {
                    if let Some((annotation, newlines)) = pending.take() {
                        let target = if newlines >= 2 {
                            AnnotationTarget::BlankLineSeparated
                        } else {
                            AnnotationTarget::Dangling
                        };
                        associations.push((annotation, target));
                    }
                }
                _ => {
                    if let Some((annotation, _)) = pending.take() {
                        associations.push((annotation, AnnotationTarget::Dangling));
                    }
                }
            },
        }
    }
    if let Some((annotation, _)) = pending {
        associations.push((annotation, AnnotationTarget::Dangling));
    }
    associations
}

/// The annotation region attached to an item (see [`statement_annotations`]
/// for the association rule), as a position-independent green subtree.
/// Annotations are siblings of the item statement in the file tree, so
/// attachment happens here, not inside `item_syntax`.
///
/// This is a probe into the file's one association walk. Deriving the
/// association per item instead re-walks every top-level statement and token of
/// the file for each item, which is quadratic on any file with many top-level
/// statements, annotated or not. The per-item query survives as the
/// incrementality firewall. An edit re-runs the file walk once, and every
/// untouched item's green annotation subtree compares equal, so its dependents
/// cut off.
#[salsa::tracked(returns(clone))]
pub fn item_annotation_syntax<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<ItemSyntax> {
    file_item_annotations(db, *item.file(db))
        .get(&item)
        .cloned()
}

/// Every item's attached annotation region, from one walk over the file.
/// Item identity comes from `item_spans`. The association matches an
/// annotation's attachment target to a span by range, so the top-level
/// classification rule lives in exactly one place.
#[salsa::tracked(returns(ref))]
fn file_item_annotations<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> rustc_hash::FxHashMap<Item<'db>, ItemSyntax> {
    let item_by_range: rustc_hash::FxHashMap<syntax::TextRange, Item<'db>> = item_spans(db, file)
        .iter()
        .map(|span| (span.range, span.item))
        .collect();
    let parse = parse(db, file);
    statement_annotations(&parse.syntax_node())
        .into_iter()
        .filter_map(|(annotation, target)| match target {
            AnnotationTarget::Attached(node) => item_by_range
                .get(&node.text_range())
                .map(|&item| (item, ItemSyntax(annotation.green().into()))),
            _ => None,
        })
        .collect()
}

/// Annotations attached to a statement BELOW the item root, keyed by the
/// annotated expression's HIR id. A block statement, a block-final expression,
/// and a `#: @new` inside a function body are such statements. The item's own
/// annotation is a top-level sibling and arrives separately, through
/// `item_annotation_syntax`, and `statement_annotations` is the association
/// rule for both. Only a payload-bearing annotation attaches here. A
/// definition, a toggle, and a refused block each have their own reporting.
///
/// This is a plain function rather than a tracked query. `Annotation` carries
/// `TextRange`s, which need no salsa value plumbing, and the callers are
/// tracked queries whose dependencies flow through `item_syntax` and
/// `item_hir` anyway.
pub fn item_expression_annotations<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
) -> Vec<(hir::ExprId, annotations::Annotation<'db>)> {
    let Some(syntax) = item_syntax(db, item) else {
        return Vec::new();
    };
    let Some(module) = item_hir(db, item) else {
        return Vec::new();
    };
    let root = syntax.syntax_node();
    let mut parents: Vec<syntax::SyntaxNode> = Vec::new();
    for node in root
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        if let Some(parent) = node.parent()
            && !parents.contains(&parent)
        {
            parents.push(parent);
        }
    }
    let mut attachments = Vec::new();
    for parent in parents {
        for (annotation, target) in statement_annotations(&parent) {
            let AnnotationTarget::Attached(target) = target else {
                continue;
            };
            let Some(index) = module
                .expressions
                .iter()
                .position(|expression| expression.range == target.text_range())
            else {
                continue;
            };
            let lowered = annotations::lower_annotation(db, &annotation);
            if lowered.declared.is_none() && lowered.new_nominal.is_none() && !lowered.trusted {
                continue;
            }
            attachments.push((hir::ExprId(index as u32), lowered));
        }
    }
    attachments
}

/// The ordered project file set, package files first, in path order. That
/// order decides the last-writer-wins winners. It is a singleton input the host
/// keeps current.
#[salsa::input(singleton, debug)]
pub struct ProjectFiles {
    #[returns(ref)]
    pub files: Vec<SourceFile>,
}

/// A file's typing mode, set by its own directives: `# typing: off|on|strict`
/// plain comments and `#: @strict` / `#: @strict off` annotation directives,
/// the last one in the file winning. `None` when the file sets nothing (the
/// configured `[check]` switches apply). The mode changes only which
/// diagnostics a host publishes. Inference itself is untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum TypingMode {
    Off,
    On,
    Strict,
}

#[salsa::tracked]
pub fn file_typing_mode(db: &dyn Db, file: SourceFile) -> Option<TypingMode> {
    file_typing_directives(db, file).0
}

/// The mode plus the ranges of `typing:` comments with an unrecognized value
/// (reported as errors rather than silently ignored).
#[salsa::tracked(returns(clone))]
pub fn file_typing_directives(
    db: &dyn Db,
    file: SourceFile,
) -> (Option<TypingMode>, Vec<(syntax::TextRange, String)>) {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut mode = None;
    let mut invalid = Vec::new();
    for element in root.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(token) if token.kind() == syntax::SyntaxKind::COMMENT => {
                let Some(rest) = token
                    .text()
                    .trim_start_matches('#')
                    .trim()
                    .strip_prefix("typing:")
                else {
                    continue;
                };
                // The whole remainder is the value, so `typing: on gely` is a
                // typo'd directive rather than `on`.
                match rest.trim() {
                    "off" => mode = Some(TypingMode::Off),
                    "on" => mode = Some(TypingMode::On),
                    "strict" => mode = Some(TypingMode::Strict),
                    other => invalid.push((token.text_range(), other.to_owned())),
                }
            }
            rowan::NodeOrToken::Node(node) if node.kind() == syntax::SyntaxKind::ANNOTATION => {
                if let Some(strict) = annotations::lower_annotation(db, &node).strict {
                    mode = Some(if strict {
                        TypingMode::Strict
                    } else {
                        TypingMode::On
                    });
                }
            }
            _ => {}
        }
    }
    (mode, invalid)
}

/// All `@type` / `@alias` definitions in a file's top-level annotations.
/// Definition blocks below the top level are refused (diagnosed at the
/// block), so they never enter the vocabulary.
#[salsa::tracked(returns(clone))]
pub fn file_type_definitions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<annotations::NamedDefinition<'db>> {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut definitions = Vec::new();
    for node in root
        .children()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        definitions.extend(annotations::lower_annotation(db, &node).definitions);
    }
    definitions
}

/// The project-wide type-definition environment: `@type` / `@alias` by name,
/// later files (and later definitions within one file) winning.
#[salsa::tracked(returns(ref))]
pub fn project_type_definitions<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> rustc_hash::FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
    let mut definitions = rustc_hash::FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != DocumentKind::Package {
            continue;
        }
        for definition in file_type_definitions(db, file) {
            definitions.insert(definition.name, definition);
        }
    }
    definitions
}

/// Statement items binding each top-level name, in project order: a
/// conditional write at a document's top level (inside a top-level
/// `if`/`for`/`while`/`repeat` or a bare block) creates the document's
/// variable slot, and cross-item reads of a name with no unconditional
/// winner resolve here. The slot's type is the join of every writer.
#[salsa::tracked(returns(ref))]
pub fn conditional_slot_items<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> rustc_hash::FxHashMap<String, Vec<Item<'db>>> {
    let mut writers: rustc_hash::FxHashMap<String, Vec<Item<'db>>> =
        rustc_hash::FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != DocumentKind::Package {
            continue;
        }
        for &item in item_tree(db, file) {
            if *item.kind(db) != ItemKind::Statement {
                continue;
            }
            for name in item_top_level_names(db, item) {
                writers.entry(name.clone()).or_default().push(item);
            }
        }
    }
    writers
}

/// The settled scheme a statement item exports for one of its top-level
/// bindings (see `ItemCheck::top_level_bindings`). A tracked projection so
/// readers cut off when the binding's scheme is unchanged even though the
/// item re-checked. Cycle recovery mirrors `global_scheme`: a statement
/// item reading its own conditionally-written name (`while (b > 0L)
/// b <- b - 1L`) routes back into its own check.
#[salsa::tracked(
    returns(clone),
    cycle_fn = statement_binding_recover,
    cycle_initial = statement_binding_initial
)]
pub fn statement_binding_scheme<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    name: types::Name<'db>,
) -> Option<types::TypeScheme<'db>> {
    let check = item_check(db, item)?;
    let name = name.text(db);
    check
        .top_level_bindings
        .iter()
        .find(|(binding, _)| binding == name)
        .map(|(_, scheme)| scheme.clone())
}

fn statement_binding_initial<'db>(
    db: &'db dyn Db,
    _id: salsa::Id,
    _item: Item<'db>,
    _name: types::Name<'db>,
) -> Option<types::TypeScheme<'db>> {
    Some(types::TypeScheme::monomorphic(types::unknown(db)))
}

fn statement_binding_recover<'db>(
    db: &'db dyn Db,
    cycle: &salsa::Cycle,
    last_provisional: &Option<types::TypeScheme<'db>>,
    value: Option<types::TypeScheme<'db>>,
    _item: Item<'db>,
    _name: types::Name<'db>,
) -> Option<types::TypeScheme<'db>> {
    if &value == last_provisional {
        return value;
    }
    if cycle.iteration() >= SCHEME_ROUND_CAP {
        return Some(types::TypeScheme::monomorphic(types::unknown(db)));
    }
    value
}

/// The winning definition item per package-exported name: later files (and
/// later assignments within one file) override earlier ones.
#[salsa::tracked(returns(ref))]
pub fn package_definitions<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> rustc_hash::FxHashMap<String, Item<'db>> {
    let mut winners = rustc_hash::FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != DocumentKind::Package {
            continue;
        }
        for &item in item_tree(db, file) {
            if matches!(*item.kind(db), ItemKind::Function | ItemKind::Value)
                && let Some(name) = item.name(db).clone()
            {
                winners.insert(name, item);
            }
        }
    }
    winners
}

/// Whether a name is an S3 method for a known generic: `generic.class`, where
/// the part before the LAST dot names a generic (so `as.character.myclass`
/// splits at `as.character`, and an ordinary dotted name like `my.helper` does
/// not qualify). A generic is one the stub corpus declares or one `generics`
/// names. A project's own `speak` is as real a generic as `print`.
///
/// Dispatch is not a read and not a call the checker can see, so this is what
/// keeps a method from looking dead (`unused`) and its mandated formals from
/// looking ignored (`unused-parameter`).
pub fn is_s3_method_name(db: &dyn Db, name: &str, generics: &FxHashSet<String>) -> bool {
    let Some((generic, class)) = name.rsplit_once('.') else {
        return false;
    };
    if generic.is_empty() || class.is_empty() {
        return false;
    }
    generics.contains(generic)
        || crate::stubs::stubs(db).is_some_and(|library| {
            library.schemes.contains_key(generic)
                || library
                    .exports_by_namespace
                    .values()
                    .any(|exports| exports.contains(generic))
        })
}

/// The S3 generics a file can see: every top-level definition whose body hands
/// the call to `UseMethod`. It covers the file itself, and anywhere else in
/// the package, because package files share one namespace.
pub fn s3_generics(db: &dyn Db, file: SourceFile) -> FxHashSet<String> {
    let mut generics = file_s3_generics(db, file).clone();
    if let Some(files) = ProjectFiles::try_get(db) {
        for &other in files.files(db) {
            if other != file && *other.kind(db) == DocumentKind::Package {
                generics.extend(file_s3_generics(db, other).iter().cloned());
            }
        }
    }
    generics
}

/// One file's S3 generics: a top-level definition whose body reads `UseMethod`,
/// which is how R's own generics are written
/// (`print <- function(x, ...) UseMethod("print")`). The dispatched name is not
/// read out of the call's argument. A generic naming something other than its
/// own binding is a bug R reports when the call runs, rather than a shape to
/// model.
/// Riding the read-set projection keeps this off item ranges, so editing one
/// body does not re-derive the set.
#[salsa::tracked(returns(ref))]
fn file_s3_generics(db: &dyn Db, file: SourceFile) -> FxHashSet<String> {
    let mut generics = FxHashSet::default();
    for &item in item_tree(db, file) {
        if let Some(name) = item.name(db).clone()
            && item_interface_reads(db, item).contains("UseMethod")
        {
            generics.insert(name);
        }
    }
    generics
}

/// The non-local names an item's body reads (bare and `pkg::`-qualified),
/// as a small per-item projection: whole-project graph walks depend on each
/// item's read *set* instead of its full naming, so a body edit that shifts
/// ranges without changing any read name backdates here and the walks stay
/// green instead of re-executing per keystroke.
#[salsa::tracked(returns(ref))]
pub fn item_interface_reads<'db>(db: &'db dyn Db, item: Item<'db>) -> BTreeSet<String> {
    let Some(naming) = item_naming(db, item) else {
        return BTreeSet::new();
    };
    let mut reads: BTreeSet<String> = naming.non_locals.values().cloned().collect();
    reads.extend(
        naming
            .namespace_reads
            .values()
            .filter_map(|read| read.name.clone()),
    );
    reads
}

/// An item's top-level binding names. This is the projection cross-item slot
/// resolution keys on, with the same backdating firewall as
/// `item_interface_reads`.
#[salsa::tracked(returns(ref))]
pub fn item_top_level_names<'db>(db: &'db dyn Db, item: Item<'db>) -> BTreeSet<String> {
    let Some(naming) = item_naming(db, item) else {
        return BTreeSet::new();
    };
    naming
        .bindings
        .values()
        .filter(|binding| binding.kind == naming::BindingKind::TopLevel)
        .map(|binding| binding.name.clone())
        .collect()
}

/// The interface-reference SCCs of the package's definition items, from the
/// static name graph. An edge runs from each item to the winner of every
/// global name its body reads. Only a *cyclic* group is recorded, which is a
/// group with several members or a single member referencing itself. Those are
/// the groups whose schemes must resolve through one canonical fixpoint.
/// Iterating a cycle from whichever member happened to be queried first would
/// make the round-cap pins depend on query order.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct InterfaceSccs<'db> {
    /// Cyclic-group id per member item.
    pub membership: rustc_hash::FxHashMap<Item<'db>, u32>,
    /// Each cyclic group's members in canonical order (project file order,
    /// then item order within the file).
    pub groups: Vec<Vec<Item<'db>>>,
}

#[salsa::tracked(returns(ref))]
pub fn interface_sccs<'db>(db: &'db dyn Db, files: ProjectFiles) -> InterfaceSccs<'db> {
    let winners = package_definitions(db, files);
    // Nodes in canonical order, with edges item -> referenced winner.
    let mut nodes: Vec<Item<'db>> = Vec::new();
    let mut position: rustc_hash::FxHashMap<Item<'db>, usize> = rustc_hash::FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != DocumentKind::Package {
            continue;
        }
        for &item in item_tree(db, file) {
            if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value)
                || item.name(db).is_none()
            {
                continue;
            }
            position.insert(item, nodes.len());
            nodes.push(item);
        }
    }
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, &item) in nodes.iter().enumerate() {
        for name in item_interface_reads(db, item) {
            if let Some(target) = winners.get(name)
                && let Some(&target_index) = position.get(target)
            {
                edges[index].push(target_index);
            }
        }
    }

    // Iterative Tarjan (the graph nests as deep as the package is large).
    let mut result = InterfaceSccs::default();
    let mut index_of: Vec<Option<u32>> = vec![None; nodes.len()];
    let mut low: Vec<u32> = vec![0; nodes.len()];
    let mut on_stack: Vec<bool> = vec![false; nodes.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0u32;
    for start in 0..nodes.len() {
        if index_of[start].is_some() {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&mut (node, ref mut edge_cursor)) = work.last_mut() {
            if *edge_cursor == 0 {
                index_of[node] = Some(next_index);
                low[node] = next_index;
                next_index += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            if let Some(&target) = edges[node].get(*edge_cursor) {
                *edge_cursor += 1;
                match index_of[target] {
                    None => work.push((target, 0)),
                    Some(target_index) => {
                        if on_stack[target] {
                            low[node] = low[node].min(target_index);
                        }
                    }
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if Some(low[node]) == index_of[node] {
                let mut members = Vec::new();
                loop {
                    let member = stack.pop().expect("tarjan stack holds the component");
                    on_stack[member] = false;
                    members.push(member);
                    if member == node {
                        break;
                    }
                }
                let cyclic = members.len() > 1 || edges[node].contains(&node);
                if cyclic {
                    members.sort_unstable();
                    let group = result.groups.len() as u32;
                    let items: Vec<Item<'db>> =
                        members.iter().map(|&member| nodes[member]).collect();
                    for &item in &items {
                        result.membership.insert(item, group);
                    }
                    result.groups.push(items);
                }
            }
        }
    }
    result
}

/// The canonical fixpoint of one cyclic interface group, independent of which
/// member was queried first. Every member starts at the tolerant `Unknown`
/// scheme. Each round re-checks every member in canonical order against the
/// *previous* round's table, which is one propagation hop per round, so
/// within-round order cannot matter either. A member still changing at the
/// round cap pins to `Unknown`. A member check runs directly, and never through
/// `item_check`, so no salsa cycle forms.
#[salsa::tracked(
    returns(ref),
    cycle_fn = scc_schemes_recover,
    cycle_initial = scc_schemes_initial
)]
pub fn scc_schemes<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    group: u32,
) -> rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    let sccs = interface_sccs(db, files);
    let Some(members) = sccs.groups.get(group as usize) else {
        return rustc_hash::FxHashMap::default();
    };
    let winners = package_definitions(db, files);
    // Only the winner of a name is reachable through reads, so the overlay
    // carries one entry per member that IS its name's winner.
    let member_names: Vec<(String, Item<'db>)> = members
        .iter()
        .filter_map(|&item| {
            let name = item.name(db).clone()?;
            (winners.get(&name) == Some(&item)).then_some((name, item))
        })
        .collect();

    let unknown_scheme = types::TypeScheme::monomorphic(types::unknown(db));
    let mut table: rustc_hash::FxHashMap<String, types::TypeScheme<'db>> = member_names
        .iter()
        .map(|(name, _)| (name.clone(), unknown_scheme.clone()))
        .collect();
    let mut schemes: rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>> = members
        .iter()
        .map(|&item| (item, unknown_scheme.clone()))
        .collect();
    for _round in 0..SCHEME_ROUND_CAP {
        let mut next_schemes = rustc_hash::FxHashMap::default();
        for &item in members {
            let scheme =
                check_member_scheme(db, item, &table).unwrap_or_else(|| unknown_scheme.clone());
            next_schemes.insert(item, scheme);
        }
        let next_table: rustc_hash::FxHashMap<String, types::TypeScheme<'db>> = member_names
            .iter()
            .map(|(name, item)| (name.clone(), next_schemes[item].clone()))
            .collect();
        let converged = next_schemes == schemes;
        schemes = next_schemes;
        table = next_table;
        if converged {
            return schemes;
        }
    }
    // Still changing at the cap: sound-by-refusal for every member (the
    // group's growth or oscillation makes no member's value trustworthy, and
    // pinning all of them is the only entry-order-free choice).
    for scheme in schemes.values_mut() {
        *scheme = unknown_scheme.clone();
    }
    schemes
}

/// Every member pinned to `Unknown`. This is the seed the internal fixpoint
/// starts from, and the answer it settles on when the members will not
/// converge.
fn unknown_group_schemes<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    group: u32,
) -> rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    let unknown = types::TypeScheme::monomorphic(types::unknown(db));
    interface_sccs(db, files)
        .groups
        .get(group as usize)
        .map(|members| {
            members
                .iter()
                .map(|&item| (item, unknown.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn scc_schemes_initial<'db>(
    db: &'db dyn Db,
    _id: salsa::Id,
    files: ProjectFiles,
    group: u32,
) -> rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    unknown_group_schemes(db, files, group)
}

/// The backstop the group's own fixpoint cannot provide. `scc_schemes` assumes
/// its group is maximal, so a member check only ever reads other members
/// through the overlay. A reference edge the static graph did not see sends the
/// check out through `global_scheme` and back into this same group.
/// `interface_sccs` builds its edges from names that appear in the source, so a
/// name the checker *constructs*, such as an S3 method, is invisible to it.
/// Without recovery salsa aborts the process; with it the group settles on the
/// same `Unknown` the round cap already uses.
fn scc_schemes_recover<'db>(
    db: &'db dyn Db,
    cycle: &salsa::Cycle,
    last_provisional: &rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>>,
    value: rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>>,
    files: ProjectFiles,
    group: u32,
) -> rustc_hash::FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    // Refuse on the first disagreement rather than iterating. The group
    // already runs its own bounded fixpoint internally, so letting salsa
    // iterate this query too multiplies those rounds by its own cap. On a large
    // group that is enough passes to exhaust memory, which is a worse failure
    // than the panic this recovery exists to prevent.
    if &value == last_provisional {
        return value;
    }
    let _ = cycle;
    unknown_group_schemes(db, files, group)
}

/// One member's exported scheme under the group's current-round table.
fn check_member_scheme<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    table: &rustc_hash::FxHashMap<String, types::TypeScheme<'db>>,
) -> Option<types::TypeScheme<'db>> {
    let module = item_hir(db, item).as_ref()?;
    let naming = item_naming(db, item).as_ref()?;
    let annotation = item_annotation_syntax(db, item)
        .map(|syntax| annotations::lower_annotation(db, &syntax.syntax_node()));
    let base = SalsaGlobals::for_item(db, item);
    let globals = SccGlobals {
        base: &base,
        members: table,
    };
    check::check_item_with_annotation(
        db,
        module,
        naming,
        annotation.as_ref(),
        &item_expression_annotations(db, item),
        Some(&globals),
    )
    .scheme
}

/// The exported scheme of one definition item. It is always `item_check`'s
/// scheme. For a cyclic-group member that is the group's canonical fixpoint
/// value, adopted inside `item_check`, which keeps the export and hover one
/// source of truth. The salsa cycle recovery below stays as a backstop for a
/// reference edge the static graph cannot see.
#[salsa::tracked(
    returns(clone),
    cycle_fn = global_scheme_recover,
    cycle_initial = global_scheme_initial
)]
pub fn global_scheme<'db>(db: &'db dyn Db, item: Item<'db>) -> types::TypeScheme<'db> {
    item_check(db, item)
        .and_then(|check| check.scheme)
        .unwrap_or_else(|| types::TypeScheme::monomorphic(types::unknown(db)))
}

fn global_scheme_initial<'db>(
    db: &'db dyn Db,
    _id: salsa::Id,
    _item: Item<'db>,
) -> types::TypeScheme<'db> {
    types::TypeScheme::monomorphic(types::unknown(db))
}

/// Bound fixpoint rounds mirroring the legacy interface round cap: a scheme
/// still changing after the cap pins to `Unknown` (sound-by-refusal) rather
/// than iterating toward salsa's panic limit.
const SCHEME_ROUND_CAP: u32 = 16;

fn global_scheme_recover<'db>(
    db: &'db dyn Db,
    cycle: &salsa::Cycle,
    last_provisional: &types::TypeScheme<'db>,
    value: types::TypeScheme<'db>,
    _item: Item<'db>,
) -> types::TypeScheme<'db> {
    if &value == last_provisional {
        return value;
    }
    if cycle.iteration() >= SCHEME_ROUND_CAP {
        return types::TypeScheme::monomorphic(types::unknown(db));
    }
    value
}

/// The full per-item check: HIR + naming + annotation lowering + inference,
/// with cross-item reads resolved through `global_scheme` (the cycle-aware
/// interface edge). Derived from position-independent per-item values only,
/// so it cuts off whenever the item (and its annotation) are untouched.
#[salsa::tracked(
    returns(clone),
    cycle_fn = item_check_recover,
    cycle_initial = item_check_initial
)]
pub fn item_check<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<check::ItemCheck<'db>> {
    let module = item_hir(db, item).as_ref()?;
    let naming = item_naming(db, item).as_ref()?;
    let annotation = item_annotation_syntax(db, item)
        .map(|syntax| annotations::lower_annotation(db, &syntax.syntax_node()));
    let globals = SalsaGlobals::for_item(db, item);
    let mut check = check::check_item_with_annotation(
        db,
        module,
        naming,
        annotation.as_ref(),
        &item_expression_annotations(db, item),
        Some(&globals),
    );
    // A cyclic-group member's export is the group's canonical fixpoint value,
    // not the one-step re-derivation this check just computed (which would
    // run one propagation hop ahead of what every reader sees).
    if check.scheme.is_some()
        && let Some(files) = ProjectFiles::try_get(db)
        && let Some(&group) = interface_sccs(db, files).membership.get(&item)
    {
        check.scheme = scc_schemes(db, files, group).get(&item).cloned();
        // A member whose body checked clean but whose exported scheme still
        // carries `Unknown` owes that `Unknown` to the reference cycle
        // itself. Nothing inside the body attributes it, so strict mode marks
        // the whole binding.
        if check.errors.is_empty()
            && check.strict_origins.is_empty()
            && let Some(scheme) = &check.scheme
            && types::contains_unknown(db, scheme.body)
            && let Some(root) = module.root
            && let Some(name) = item.name(db).clone()
        {
            check.strict_origins.push(check::StrictOrigin {
                expression: root,
                range: module.expression(root).range,
                kind: check::StrictOriginKind::RecursiveUnknown(name),
            });
        }
    }
    Some(check)
}

fn item_check_initial<'db>(
    _db: &'db dyn Db,
    _id: salsa::Id,
    _item: Item<'db>,
) -> Option<check::ItemCheck<'db>> {
    None
}

fn item_check_recover<'db>(
    db: &'db dyn Db,
    cycle: &salsa::Cycle,
    last_provisional: &Option<check::ItemCheck<'db>>,
    value: Option<check::ItemCheck<'db>>,
    _item: Item<'db>,
) -> Option<check::ItemCheck<'db>> {
    if &value == last_provisional {
        return value;
    }
    // `item_check`, and not `global_scheme`, is the head salsa iterates when a
    // definition's check reads its own exported scheme, because the re-entered
    // query drives the cycle. The round cap must therefore live here too. A
    // value still changing at the cap is non-converging, so both export
    // surfaces pin to the sound refusal. A self-referential definition whose
    // type grows a level per round, such as `x <- list(v = x)`, and an
    // oscillation are both non-converging.
    //
    // What the pin must NOT do is derive from this round's recomputation. A
    // check carries six fields besides the scheme, and every one of them keeps
    // moving while the cycle does. Pinning only the scheme and taking the rest
    // from `value` returns a different check every round, so the equality test
    // above can never succeed. Salsa then iterates to its own limit and panics
    // with `too many cycle iterations`, or exhausts memory first, whichever the
    // machine reaches. Re-pinning what was already returned is a fixed point by
    // construction, because the next round produces it unchanged and the test
    // passes.
    // The sibling recoveries are safe from this because a bare `TypeScheme` pin
    // is already a constant.
    if cycle.iteration() >= SCHEME_ROUND_CAP {
        return last_provisional
            .clone()
            .or(value)
            .map(|check| refuse_check(db, check));
    }
    value
}

/// A check with both export surfaces cut to `Unknown`: what a non-converging
/// cycle member exports, so downstream items check against an absent fact
/// rather than an untrustworthy shape. Findings inside the item are kept.
///
/// This must be **idempotent**, so that `refuse_check(refuse_check(c))` equals
/// `refuse_check(c)`. That is what lets the recovery above terminate.
/// Re-pinning an already-pinned value reproduces it, so the round after the cap
/// compares equal and the fixpoint stops. `refusal_is_idempotent` pins it.
fn refuse_check<'db>(db: &'db dyn Db, check: check::ItemCheck<'db>) -> check::ItemCheck<'db> {
    let refused = types::TypeScheme::monomorphic(types::unknown(db));
    check::ItemCheck {
        scheme: Some(refused.clone()),
        top_level_bindings: check
            .top_level_bindings
            .iter()
            .map(|(name, _)| (name.clone(), refused.clone()))
            .collect(),
        ..check
    }
}

/// The salsa-backed cross-item resolver handed to the checker.
struct SalsaGlobals<'db> {
    db: &'db dyn Db,
    definitions: Option<&'db rustc_hash::FxHashMap<String, Item<'db>>>,
    /// This item's own position in its file, for both document kinds. A file is
    /// sourced top-down whichever kind it is.
    ///
    /// An **immediate** read therefore sees the nearest EARLIER writer in this
    /// file, ahead of the project-wide winner. That covers a top-level
    /// statement rewriting a name the file defined above it, which the
    /// project-wide map cannot express: it holds definition items only, so a
    /// later `record$age <- …` was invisible and the read answered from the
    /// pre-write type.
    ///
    /// A **deferred** read, which comes from inside a closure, differs by
    /// kind, because what has finished running when the body executes
    /// differs. In a script
    /// the closure runs once the file's frame has settled, so it sees the last
    /// writer anywhere in that file, its own binding included (self-recursion).
    /// In a package the function runs after the *whole package* is sourced, so
    /// the answer is the project-wide winner and this file must stand aside.
    /// A later file's override would otherwise be lost.
    frame_index: Option<usize>,
    /// Whether this item's file is a script, which decides the deferred-read
    /// rule above.
    is_script: bool,
    /// The item's own file, for the facts that are per-file rather than
    /// per-item: its type declarations and its arithmetic classes.
    file: SourceFile,
}

/// How many conditional writers of one name still get joined. Past this the
/// slot is `Unknown`, the sound-by-refusal move used for a loop variable whose
/// type keeps growing.
///
/// Two reasons for a bound. The join costs one item check per writer, and it is
/// reached from a per-item read, so a name written at many documents' top level
/// makes every read of it pay for all of them. And the answer stops being worth
/// paying for: a union of dozens of unrelated types is not a fact any diagnostic
/// can use. A real conditional slot, such as a top-level `if`/`else` picking a
/// default, has a handful of writers.
/// The `@type` / `@alias` definitions one file sees: the project's, with the
/// file's own layered on top when it is a script, because a script's
/// declarations are visible to itself alone and shadow project-global names.
///
/// This is per file and memoized, so the merge runs once instead of once per
/// item checked in the file. The caller is a per-item check, and handing it an
/// owned copy of the project table is what made a heavily annotated project
/// quadratic.
#[salsa::tracked(returns(ref))]
fn visible_type_definitions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> rustc_hash::FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
    let mut definitions = ProjectFiles::try_get(db)
        .map(|files| project_type_definitions(db, files).clone())
        .unwrap_or_default();
    if *file.kind(db) == DocumentKind::Script {
        for definition in file_type_definitions(db, file) {
            definitions.insert(definition.name, definition);
        }
    }
    definitions
}

/// The classes with an arithmetic operator method that one file sees: the
/// shipped stubs, the project's own definitions, and the file's own top level.
/// Memoized per file for the same reason as [`visible_type_definitions`].
#[salsa::tracked(returns(ref))]
fn visible_arithmetic_classes(db: &dyn Db, file: SourceFile) -> rustc_hash::FxHashSet<String> {
    let mut classes = stub_arithmetic_classes(db);
    if let Some(files) = ProjectFiles::try_get(db) {
        classes.extend(project_arithmetic_classes(db, files).iter().cloned());
    }
    classes.extend(file_arithmetic_classes(db, file).iter().cloned());
    classes
}

const CONDITIONAL_SLOT_JOIN_CAP: usize = 8;

/// Every item that binds a name at a file's top level, by name, in file order.
///
/// This is the index behind [`SalsaGlobals::frame_definition`], which answers
/// "which item of this file binds this name" once per name a check looks up.
/// Scanning the item list for that answer is linear per lookup, so a file with
/// many items *and* many distinct cross-item references costs their product.
/// That is quadratic, and it is invisible when only one of the two grows. Each
/// axis alone stays linear, which is why it hid. 20,000 items referencing 200
/// names cost 576 ms and 200 items referencing 20,000 names cost 245 ms, while
/// 20,000 of each cost 4,305 ms, five times their sum.
#[salsa::tracked(returns(ref))]
fn file_binders<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> rustc_hash::FxHashMap<String, Vec<(usize, Item<'db>)>> {
    let mut binders: rustc_hash::FxHashMap<String, Vec<(usize, Item<'db>)>> =
        rustc_hash::FxHashMap::default();
    for (index, &item) in item_tree(db, file).iter().enumerate() {
        match *item.kind(db) {
            ItemKind::Function | ItemKind::Value => {
                if let Some(name) = item.name(db).clone() {
                    binders.entry(name).or_default().push((index, item));
                }
            }
            // A statement item binds through a conditional top-level write
            // (the document-slot model), and may bind several names.
            ItemKind::Statement => {
                for name in item_top_level_names(db, item) {
                    binders.entry(name.clone()).or_default().push((index, item));
                }
            }
        }
    }
    binders
}

impl<'db> SalsaGlobals<'db> {
    fn for_item(db: &'db dyn Db, item: Item<'db>) -> SalsaGlobals<'db> {
        let definitions = ProjectFiles::try_get(db).map(|files| package_definitions(db, files));
        let file = *item.file(db);
        let is_script = *file.kind(db) == DocumentKind::Script;
        // Looked up, not scanned for: this runs once per checked item, so a
        // linear search here is quadratic in a file's top-level items.
        let index = item_tree_positions(db, file)
            .get(&item)
            .copied()
            .unwrap_or_else(|| item_tree(db, file).len());
        SalsaGlobals {
            db,
            definitions,
            frame_index: Some(index),
            is_script,
            file,
        }
    }

    fn frame_definition(&self, name: &str, deferred: bool) -> Option<Item<'db>> {
        let index = self.frame_index?;
        // A package function body runs after every file is sourced, so the
        // project-wide winner owns that answer, not this file's last writer.
        if deferred && !self.is_script {
            return None;
        }
        let binders = file_binders(self.db, self.file).get(name)?;
        // A deferred read in a script sees the whole settled frame, its own
        // binding included; an immediate one sees only what ran above it.
        let visible = match deferred {
            true => &binders[..],
            false => &binders[..binders.partition_point(|&(at, _)| at < index)],
        };
        visible.last().map(|&(_, item)| item)
    }

    /// The joined scheme of a package-level conditional slot: every
    /// statement item writing the name contributes its settled binding type,
    /// up to [`CONDITIONAL_SLOT_JOIN_CAP`].
    fn conditional_slot_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        let files = ProjectFiles::try_get(self.db)?;
        let writers = conditional_slot_items(self.db, files).get(name)?;
        let interned = types::Name::new(self.db, name.to_owned());
        if writers.len() > CONDITIONAL_SLOT_JOIN_CAP {
            return Some(types::TypeScheme::monomorphic(types::unknown(self.db)));
        }
        let mut schemes: Vec<types::TypeScheme<'db>> = writers
            .iter()
            .filter_map(|&item| statement_binding_scheme(self.db, item, interned))
            .collect();
        match schemes.len() {
            0 => None,
            1 => schemes.pop(),
            _ => {
                let bodies: Vec<types::Ty<'db>> =
                    schemes.into_iter().map(|scheme| scheme.body).collect();
                Some(types::TypeScheme::monomorphic(types::union_of(
                    self.db, bodies,
                )))
            }
        }
    }
}

impl<'db> check::GlobalEnv<'db> for SalsaGlobals<'db> {
    fn scheme(&self, name: &str, deferred: bool) -> Option<types::TypeScheme<'db>> {
        if let Some(item) = self.frame_definition(name, deferred) {
            if *item.kind(self.db) == ItemKind::Statement {
                let interned = types::Name::new(self.db, name.to_owned());
                if let Some(scheme) = statement_binding_scheme(self.db, item, interned) {
                    return Some(scheme);
                }
            } else {
                return Some(global_scheme(self.db, item));
            }
        }
        if let Some(item) = self
            .definitions
            .as_ref()
            .and_then(|winners| winners.get(name))
        {
            return Some(global_scheme(self.db, *item));
        }
        if let Some(scheme) = self.conditional_slot_scheme(name) {
            return Some(scheme);
        }
        // Reading an overloaded stub name as a plain value (not a call)
        // resolves to its last candidate: the corpus orders candidates
        // specific-first, so the last one is the most general.
        let library = stubs::stubs(self.db)?;
        library.schemes.get(name)?.last().cloned()
    }

    fn defined_in_project(&self, name: &str, deferred: bool) -> bool {
        self.frame_definition(name, deferred).is_some()
            || self
                .definitions
                .as_ref()
                .is_some_and(|winners| winners.contains_key(name))
    }

    fn overloads(&self, name: &str, deferred: bool) -> Option<Vec<types::TypeScheme<'db>>> {
        // A script-local or package definition wins over the stub set,
        // disabling per-call overload selection for that name.
        if self.frame_definition(name, deferred).is_some() {
            return None;
        }
        if self
            .definitions
            .as_ref()
            .is_some_and(|winners| winners.contains_key(name))
        {
            return None;
        }
        let candidates = stubs::stubs(self.db)?.schemes.get(name)?;
        (candidates.len() > 1).then(|| candidates.clone())
    }

    fn type_definitions(
        &self,
    ) -> &'db rustc_hash::FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
        visible_type_definitions(self.db, self.file)
    }

    fn arithmetic_classes(&self) -> &'db rustc_hash::FxHashSet<String> {
        visible_arithmetic_classes(self.db, self.file)
    }
}

/// Classes the standard library gives an arithmetic operator method. Static for
/// a given corpus, so it is computed once rather than per checked item.
#[salsa::tracked(returns(clone))]
fn stub_arithmetic_classes(db: &dyn Db) -> rustc_hash::FxHashSet<String> {
    let Some(library) = stubs::stubs(db) else {
        return rustc_hash::FxHashSet::default();
    };
    arithmetic_classes_among(library.schemes.keys().map(String::as_str))
}

/// The same, for the project's own sources: a package that defines `+.Money`
/// makes `Money` arithmetic exactly as a stub would, and operator dispatch
/// already resolves it through the global scope.
#[salsa::tracked(returns(clone))]
fn project_arithmetic_classes(db: &dyn Db, files: ProjectFiles) -> rustc_hash::FxHashSet<String> {
    arithmetic_classes_among(package_definitions(db, files).keys().map(String::as_str))
}

/// The same, for one file's own top-level definitions. A script's definitions
/// are visible only to itself. A package file's are already in the project-wide
/// set, and they are cheap to include, which keeps this correct even where
/// `ProjectFiles` is not set. Memoized per file because the caller runs once per checked item.
#[salsa::tracked(returns(ref))]
fn file_arithmetic_classes(db: &dyn Db, file: SourceFile) -> rustc_hash::FxHashSet<String> {
    arithmetic_classes_among(
        item_tree(db, file)
            .iter()
            .filter_map(|item| item.name(db).as_deref()),
    )
}

/// The class each arithmetic method name is declared for.
fn arithmetic_classes_among<'a>(
    names: impl Iterator<Item = &'a str>,
) -> rustc_hash::FxHashSet<String> {
    let prefixes: Vec<String> = check::arithmetic_method_prefixes().collect();
    let mut classes = rustc_hash::FxHashSet::default();
    for name in names {
        for prefix in &prefixes {
            if let Some(class) = name.strip_prefix(prefix.as_str())
                && !class.is_empty()
            {
                classes.insert(class.to_owned());
            }
        }
    }
    classes
}

/// A cyclic group's view of the world during its canonical fixpoint: member
/// names resolve from the current round table (never through `global_scheme`,
/// which would re-enter the group), everything else through the ordinary
/// salsa-backed resolver.
struct SccGlobals<'db, 'a> {
    base: &'a SalsaGlobals<'db>,
    members: &'a rustc_hash::FxHashMap<String, types::TypeScheme<'db>>,
}

impl<'db> check::GlobalEnv<'db> for SccGlobals<'db, '_> {
    fn scheme(&self, name: &str, deferred: bool) -> Option<types::TypeScheme<'db>> {
        self.members
            .get(name)
            .cloned()
            .or_else(|| self.base.scheme(name, deferred))
    }

    fn overloads(&self, name: &str, deferred: bool) -> Option<Vec<types::TypeScheme<'db>>> {
        if self.members.contains_key(name) {
            return None;
        }
        self.base.overloads(name, deferred)
    }

    fn defined_in_project(&self, name: &str, deferred: bool) -> bool {
        self.members.contains_key(name) || self.base.defined_in_project(name, deferred)
    }

    fn type_definitions(
        &self,
    ) -> &'db rustc_hash::FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
        self.base.type_definitions()
    }

    /// Which classes are arithmetic does not depend on the group being solved,
    /// so this is the base answer. It still has to be *given* rather than
    /// inherited. An empty set here makes `Date + 1` inside any recursive
    /// function fail the numeric constraint and collapse the whole scheme to
    /// `Unknown`.
    fn arithmetic_classes(&self) -> &'db rustc_hash::FxHashSet<String> {
        self.base.arithmetic_classes()
    }
}

/// Kind + name of one top-level statement, mirroring R assignment spellings:
/// `<-`, `=`, `<<-`, `:=` bind on the left, `->`, `->>` on the right.
fn classify_top_level(node: &syntax::SyntaxNode) -> (ItemKind, Option<String>) {
    use syntax::SyntaxKind;
    if let Some(name) = set_generic_target(node) {
        return (ItemKind::Function, Some(name));
    }
    if node.kind() != SyntaxKind::BINARY_EXPR {
        return (ItemKind::Statement, None);
    }
    let binary = syntax::ast::BinaryExpr::cast(node.clone());
    let Some(binary) = binary else {
        return (ItemKind::Statement, None);
    };
    let Some(operator) = binary.operator() else {
        return (ItemKind::Statement, None);
    };
    let (target, value) = match operator.kind() {
        SyntaxKind::LESS_MINUS
        | SyntaxKind::EQ
        | SyntaxKind::LESS2_MINUS
        | SyntaxKind::COLON_EQ => (binary.lhs(), binary.rhs()),
        SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => (binary.rhs(), binary.lhs()),
        _ => return (ItemKind::Statement, None),
    };
    let name = target
        .clone()
        .and_then(syntax::ast::Name::cast)
        .and_then(|name| name.text())
        .or_else(|| {
            // A string target (`"name" <- ...`) defines the unquoted name.
            target.as_ref().and_then(|node| {
                (node.kind() == SyntaxKind::LITERAL
                    && node
                        .first_token()
                        .is_some_and(|t| t.kind() == SyntaxKind::STRING))
                .then(|| {
                    let text = node.text().to_string();
                    text.trim_matches(['"', '\'']).to_owned()
                })
            })
        });
    if name.is_none() {
        return (ItemKind::Statement, None);
    }
    let kind = match value.map(|value| value.kind()) {
        Some(SyntaxKind::FUNCTION_DEF) => ItemKind::Function,
        _ => ItemKind::Value,
    };
    (kind, name)
}

/// The name a top-level `setGeneric("name", ...)` call binds. It is the one S4
/// registration call that creates a bare-name binding in the global
/// environment. `setClass` and `setMethod` register class metadata under
/// internal names, referenced through strings, so they bind nothing.
fn set_generic_target(node: &syntax::SyntaxNode) -> Option<String> {
    use syntax::SyntaxKind;
    if node.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let callee = node.children().next()?;
    let callee_name = match callee.kind() {
        SyntaxKind::NAME => callee.text().to_string(),
        SyntaxKind::NAMESPACE_EXPR => callee
            .children()
            .filter(|child| child.kind() == SyntaxKind::NAME)
            .last()?
            .text()
            .to_string(),
        _ => return None,
    };
    if callee_name != "setGeneric" {
        return None;
    }
    let arguments = node
        .children()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)?;
    let is_named_argument = |argument: &syntax::SyntaxNode| {
        argument.children_with_tokens().any(|element| {
            element
                .as_token()
                .is_some_and(|t| t.kind() == SyntaxKind::EQ)
        })
    };
    // The generic's name: the `name =` argument, else the first positional.
    let argument = arguments
        .children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .find(|argument| {
            is_named_argument(argument)
                && argument
                    .children()
                    .find(|child| child.kind() == SyntaxKind::NAME)
                    .is_some_and(|name| name.text() == "name")
        })
        .or_else(|| {
            arguments
                .children()
                .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
                .find(|argument| !is_named_argument(argument))
        })?;
    let string = argument
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::STRING)?;
    let text = string.text();
    if text.len() < 2 || !(text.starts_with('"') || text.starts_with('\'')) {
        return None;
    }
    let name = text[1..text.len() - 1].to_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use salsa::Setter as _;

    /// The property the cycle recovery's termination rests on. Pinning a check
    /// that is already pinned has to reproduce it exactly, or the round after
    /// the cap compares unequal and salsa iterates to its own limit. A whole
    /// CRAN package used to hit that, panicking with "too many cycle
    /// iterations" or exhausting memory first. Only the two export surfaces may
    /// be rewritten, and the findings inside the item must survive untouched.
    #[test]
    fn refusal_is_idempotent() {
        let db = RootDatabase::default();
        let integer = types::scalar(&db, types::Atomic::Integer);
        let check = check::ItemCheck {
            expression_types: Default::default(),
            errors: Vec::new(),
            strict_origins: Vec::new(),
            scheme: Some(types::TypeScheme::monomorphic(integer)),
            selected_overloads: Default::default(),
            masked_reads: Default::default(),
            top_level_bindings: vec![("value".to_owned(), types::TypeScheme::monomorphic(integer))],
        };

        let once = refuse_check(&db, check);
        let twice = refuse_check(&db, once.clone());
        assert_eq!(once, twice, "re-pinning a pinned check must reproduce it");

        let unknown = types::TypeScheme::monomorphic(types::unknown(&db));
        assert_eq!(once.scheme.as_ref(), Some(&unknown));
        assert_eq!(once.top_level_bindings, vec![("value".to_owned(), unknown)]);
    }

    #[test]
    fn parse_tracks_text_edits() {
        let mut db = RootDatabase::default();
        let file = SourceFile::new(&db, "x <- 1".to_owned(), DocumentKind::Package);
        let first = parse(&db, file);
        assert!(first.errors().is_empty());
        assert_eq!(first.text(), "x <- 1");

        file.set_text(&mut db).to("x <- ".to_owned());
        let second = parse(&db, file);
        assert_eq!(second.text(), "x <- ");
        assert!(!second.errors().is_empty());
    }

    #[test]
    fn item_identity_and_position_independence() {
        let mut db = RootDatabase::default();
        let file = SourceFile::new(
            &db,
            "f <- function(x) x\ng <- function(y) y\n".to_owned(),
            DocumentKind::Package,
        );
        let g_before = {
            let items = item_tree(&db, file);
            assert_eq!(items.len(), 2);
            assert_eq!(items[1].name(&db).as_deref(), Some("g"));
            item_syntax(&db, items[1]).expect("g exists")
        };

        // An edit inside `f`'s body shifts `g`. Both its item identity, which
        // is the interned id re-minted from the same fields, and its green
        // subtree must survive unchanged. Structural equality across shifted
        // offsets is what early cutoff rests on.
        file.set_text(&mut db)
            .to("f <- function(x) x + 100\ng <- function(y) y\n".to_owned());
        {
            let items = item_tree(&db, file);
            assert_eq!(items.len(), 2);
            let g = Item::new(&db, file, ItemKind::Function, Some("g".to_owned()), None, 0);
            assert_eq!(items[1], g);
            let g_after = item_syntax(&db, g).expect("g still exists");
            assert_eq!(g_before, g_after);
        }

        // Deleting `g` resolves its item to nothing.
        file.set_text(&mut db).to("f <- function(x) x\n".to_owned());
        let g = Item::new(&db, file, ItemKind::Function, Some("g".to_owned()), None, 0);
        assert_eq!(item_syntax(&db, g), None);
    }

    #[test]
    fn hir_lowers_and_survives_shifts() {
        let mut db = RootDatabase::default();
        let file = SourceFile::new(
            &db,
            "pad <- 1\nadd <- function(x, y = 2) x + y\n".to_owned(),
            DocumentKind::Package,
        );
        let add = Item::new(
            &db,
            file,
            ItemKind::Function,
            Some("add".to_owned()),
            None,
            0,
        );
        let before = item_hir(&db, add).clone().expect("add lowers");
        let root = before.expression(before.root.expect("has root"));
        let hir::ExpressionKind::Assign { value, .. } = &root.kind else {
            panic!("expected an assignment root, got {root:?}");
        };
        let hir::ExpressionKind::Function { parameters, body } = &before.expression(*value).kind
        else {
            panic!("expected a function value");
        };
        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].name, "x");
        assert_eq!(parameters[1].name, "y");
        assert!(parameters[1].default.is_some());
        let hir::ExpressionKind::Binary { operator, .. } = &before.expression(*body).kind else {
            panic!("expected a binary body");
        };
        assert_eq!(*operator, hir::BinaryOperator::Add);

        // Shifting `add` (an edit in the item before it) reproduces an equal
        // Module: spans are item-relative, so nothing downstream re-runs.
        file.set_text(&mut db)
            .to("pad <- 100000\nadd <- function(x, y = 2) x + y\n".to_owned());
        let add = Item::new(
            &db,
            file,
            ItemKind::Function,
            Some("add".to_owned()),
            None,
            0,
        );
        let after = item_hir(&db, add).clone().expect("add still lowers");
        assert_eq!(before, after);
    }

    #[test]
    fn cross_file_reads_resolve_through_the_interface() {
        let mut db = RootDatabase::default();
        let util = SourceFile::new(
            &db,
            "add <- function(x, y) x + y\n".to_owned(),
            DocumentKind::Package,
        );
        let main = SourceFile::new(
            &db,
            "use <- function() add(1L, 2L)\nbad <- function() add(\"a\", 2L)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![util, main]);

        let use_item = Item::new(
            &db,
            main,
            ItemKind::Function,
            Some("use".to_owned()),
            None,
            0,
        );
        let use_check = item_check(&db, use_item).expect("use checks");
        assert!(use_check.errors.is_empty(), "{:?}", use_check.errors);

        let bad_item = Item::new(
            &db,
            main,
            ItemKind::Function,
            Some("bad".to_owned()),
            None,
            0,
        );
        let bad_check = item_check(&db, bad_item).expect("bad checks");
        assert!(
            !bad_check.errors.is_empty(),
            "character into numeric parameter must report"
        );

        // Editing the callee body (same shape) leaves the caller check equal.
        util.set_text(&mut db)
            .to("add <- function(x, y) y + x\n".to_owned());
        let use_item = Item::new(
            &db,
            main,
            ItemKind::Function,
            Some("use".to_owned()),
            None,
            0,
        );
        let again = item_check(&db, use_item).expect("use still checks");
        assert!(again.errors.is_empty());
    }

    #[test]
    fn mutual_recursion_converges_through_the_fixpoint() {
        let db = RootDatabase::default();
        let file = SourceFile::new(
            &db,
            "is_even <- function(n) if (n == 0L) TRUE else is_odd(n - 1L)\nis_odd <- function(n) if (n == 0L) FALSE else is_even(n - 1L)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        let even = Item::new(
            &db,
            file,
            ItemKind::Function,
            Some("is_even".to_owned()),
            None,
            0,
        );
        let check = item_check(&db, even).expect("checks");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = global_scheme(&db, even);
        assert!(
            matches!(scheme.body.kind(&db), types::TyKind::Function(_)),
            "mutual recursion must converge to a function scheme, got {:?}",
            scheme.body.kind(&db)
        );
    }

    #[test]
    fn self_recursion_stays_tolerant() {
        let db = RootDatabase::default();
        let file = SourceFile::new(
            &db,
            "count <- function(n) if (n == 0L) 0L else count(n - 1L)\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![file]);
        let item = Item::new(
            &db,
            file,
            ItemKind::Function,
            Some("count".to_owned()),
            None,
            0,
        );
        // Must not panic; the self-cycle iterates to a stable scheme.
        let check = item_check(&db, item).expect("checks");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
    }

    #[test]
    fn later_file_wins_the_definition() {
        let db = RootDatabase::default();
        let first = SourceFile::new(&db, "limit <- \"low\"\n".to_owned(), DocumentKind::Package);
        let second = SourceFile::new(&db, "limit <- 10L\n".to_owned(), DocumentKind::Package);
        let files = ProjectFiles::new(&db, vec![first, second]);
        let winners = package_definitions(&db, files);
        let winner = winners.get("limit").expect("winner exists");
        assert_eq!(*winner.file(&db), second);
        let scheme = global_scheme(&db, *winner);
        assert_eq!(
            scheme.body,
            types::scalar(&db, types::Atomic::Integer),
            "the later file's integer wins"
        );
    }
}
