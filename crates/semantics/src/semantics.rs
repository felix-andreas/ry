pub mod annotations;
pub mod check;
pub mod diagnostics;
pub mod exhaustiveness;
pub mod hir;
pub mod infer;
pub mod lints;
pub mod load_index;
pub mod metadata;
pub mod naming;
pub mod sorted;
pub mod stubs;
pub mod testing;
pub mod types;

use crate::hir::Symbol;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use syntax::ast::is_expression_kind;
use syntax::ast::{self, BinaryExpr};
use syntax::{
    GreenEvent, Parse, SyntaxKind, SyntaxNode, TextRange, TextSize, matching_nodes,
    paired_comment_definition,
};

/// Analysis walks recurse as deeply as the source nests, so commands and analysis workers
/// run on threads with this stack rather than `ulimit -s`'s.
pub const ANALYSIS_STACK_SIZE: usize = 64 * 1024 * 1024;

#[salsa::db]
pub trait Db: salsa::Database {
    /// Acceleration only: a spliced parse is byte- and error-identical to a fresh one.
    fn splice_cache(&self) -> &SpliceCache;
}

#[salsa::db]
#[derive(Clone, Default)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
    /// Shared across storage-handle clones so fresh handles keep the warm entries.
    splice_cache: Arc<SpliceCache>,
}

impl RootDatabase {
    /// For a single cold pass, such as `ry check`. Trees are evicted whenever the host calls
    /// `trigger_lru_eviction`, and no file is parsed twice from an edit, so splice caching
    /// would only hold a second copy of every text and of recent trees.
    ///
    /// The default database keeps every tree: an editor host pays for eviction in latency
    /// (about 10 ms per settled re-check on the benchmark code bases) more than it gains in
    /// memory.
    pub fn one_shot() -> RootDatabase {
        let mut db = RootDatabase {
            storage: salsa::Storage::default(),
            splice_cache: Arc::new(SpliceCache {
                entries: Mutex::default(),
                enabled: false,
            }),
        };
        limit_resident_trees(&mut db);
        db
    }
}

/// Lets `trigger_lru_eviction` drop all but the most recently used syntax trees.
pub fn limit_resident_trees(db: &mut dyn Db) {
    parse::set_lru_capacity(db, 128);
    file_item_annotations::set_lru_capacity(db, 128);
    item_syntax::set_lru_capacity(db, 4096);
    item_annotation_syntax::set_lru_capacity(db, 4096);
}

#[salsa::db]
impl salsa::Database for RootDatabase {}

#[salsa::db]
impl Db for RootDatabase {
    fn splice_cache(&self) -> &SpliceCache {
        &self.splice_cache
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum DocumentKind {
    /// A file of the package namespace (`R/`).
    Package,
    /// A testthat test file (`tests/testthat/test-*.R`). testthat runs each one in its own
    /// environment, so it reads its own bindings, the helpers and the package namespace, and
    /// contributes to none of them.
    Test,
    /// A testthat helper or setup file (`helper-*.R`, `setup-*.R`), which testthat sources
    /// into the environment every test file runs in.
    TestHelper,
    /// A standalone script, which reads the package namespace and its own bindings only.
    Script,
}

/// The kind of a file directly in `tests/testthat/`, from its name alone.
pub fn test_document_kind(file_name: &str) -> DocumentKind {
    if file_name.starts_with("helper") || file_name.starts_with("setup") {
        DocumentKind::TestHelper
    } else {
        DocumentKind::Test
    }
}

#[salsa::input(debug)]
pub struct SourceFile {
    #[returns(deref)]
    pub text: String,
    pub kind: DocumentKind,
}

/// Trees are the largest memos and cheap to rebuild, so a cold pass keeps only recent ones
/// resident (see [`RootDatabase::one_shot`]); everything derived from them is memoized
/// item-relative and survives eviction.
#[salsa::tracked(returns(clone), lru = 0)]
pub fn parse(db: &dyn Db, file: SourceFile) -> Parse {
    db.splice_cache().parse(file, file.text(db))
}

/// The previous `(text, parse)` per file. At capacity, inserting an unknown file
/// clears the map wholesale: a cold pass then costs one clear, while an editing
/// session's open set stays resident.
pub struct SpliceCache {
    entries: Mutex<FxHashMap<SourceFile, (Arc<str>, Parse)>>,
    enabled: bool,
}

impl Default for SpliceCache {
    fn default() -> SpliceCache {
        SpliceCache {
            entries: Mutex::default(),
            enabled: true,
        }
    }
}

const SPLICE_CACHE_CAPACITY: usize = 64;

impl SpliceCache {
    fn parse(&self, file: SourceFile, new_text: &str) -> Parse {
        if !self.enabled {
            return syntax::parse(new_text);
        }
        // Parse unlocked so parallel workers never serialize on the parse itself.
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
        entries.insert(file, (Arc::from(new_text), parse.clone()));
        parse
    }
}

/// The one edit turning `old` into `new` (replaced range in `old`, replacement
/// length); several accumulated edits collapse into one region.
fn derived_edit(old: &str, new: &str) -> (TextRange, TextSize) {
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
        TextRange::new(
            TextSize::from(prefix as u32),
            TextSize::from((old.len() - suffix) as u32),
        ),
        TextSize::from((new.len() - suffix - prefix) as u32),
    )
}

/// Identity is insertion-stable (kind, name, parent, disambiguator), never a
/// position: inserting an unrelated item must not invalidate later items' memos.
#[salsa::interned(debug)]
pub struct Item<'db> {
    pub file: SourceFile,
    pub kind: ItemKind,
    #[returns(ref)]
    pub name: Option<String>,
    pub parent: Option<Item<'db>>,
    pub disambiguator: u32,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue, serde::Serialize, serde::Deserialize,
)]
pub enum ItemKind {
    Function,
    Value,
    Statement,
}

/// Structure and identity only, with no spans or bodies, so an edit inside one body
/// leaves it equal and cuts off. Borrowed: per-item work reads it once per item.
#[salsa::tracked(returns(ref))]
pub fn item_tree<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<Item<'db>> {
    item_spans(db, file).iter().map(|span| span.item).collect()
}

/// Width-only, so position-independent: an edit elsewhere in the file cuts off
/// here. Item innards must be read through this (never `parse`), and spans derived
/// from it stay item-relative until the rendering edge. Holds part of the file's tree, so
/// it is evicted alongside `parse`.
#[salsa::tracked(returns(clone), lru = 0)]
pub fn item_syntax<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<ItemSyntax> {
    item_node(db, item).map(|node| ItemSyntax(node.green().into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemSpan<'db> {
    pub item: Item<'db>,
    pub range: TextRange,
}

/// One memoized walk; a per-item lookup would re-classify the file per item.
#[salsa::tracked(returns(ref))]
pub fn item_spans<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<ItemSpan<'db>> {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut counts: FxHashMap<(ItemKind, Option<String>), u32> = FxHashMap::default();
    let mut spans = Vec::new();
    for node in root.children() {
        if !is_expression_kind(node.kind()) && node.kind() != SyntaxKind::ERROR {
            continue;
        }
        let (kind, name) = match top_level_definition(&node) {
            Some(definition) => (definition.kind, Some(definition.name)),
            None => (ItemKind::Statement, None),
        };
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

/// The sparse nodes the per-file passes look for, in preorder, from one walk of the green
/// tree: each pass reading the tree itself would walk every node once per pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SyntaxSites(Vec<SyntaxSite>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SyntaxSite {
    kind: SyntaxKind,
    range: TextRange,
    roles: u8,
}

/// What a recorded site is to the passes that read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteRole {
    /// A `#:` block.
    Annotation,
    TypeDefinition,
    /// A `#:` block or an inline type definition: everything that can declare a type.
    Declaration,
    /// A record, tuple, tag or `match`, which R cannot run.
    RySyntax,
    /// A node carrying an inline annotation, as [`annotations::is_green_inline_site`].
    InlineSite,
    /// An inline annotation itself, as [`syntax::is_green_inline_annotation`].
    InlineAnnotation,
    /// The file or a brace body with a `#:` block among its statements.
    AnnotatedSequence,
    /// A call that attaches a namespace, as [`metadata::is_green_attach_call`].
    AttachCall,
}

impl SiteRole {
    fn bit(self) -> u8 {
        match self {
            SiteRole::InlineSite => 1,
            SiteRole::InlineAnnotation => 2,
            SiteRole::AnnotatedSequence => 4,
            SiteRole::AttachCall => 8,
            SiteRole::Annotation
            | SiteRole::TypeDefinition
            | SiteRole::Declaration
            | SiteRole::RySyntax => 0,
        }
    }

    fn holds(self, site: &SyntaxSite) -> bool {
        match self {
            SiteRole::Annotation => site.kind == SyntaxKind::ANNOTATION,
            SiteRole::TypeDefinition => site.kind == SyntaxKind::TYPE_DEFINITION,
            SiteRole::Declaration => matches!(
                site.kind,
                SyntaxKind::ANNOTATION | SyntaxKind::TYPE_DEFINITION
            ),
            SiteRole::RySyntax => is_ry_syntax(site.kind),
            SiteRole::InlineSite
            | SiteRole::InlineAnnotation
            | SiteRole::AnnotatedSequence
            | SiteRole::AttachCall => site.roles & self.bit() != 0,
        }
    }
}

impl SyntaxSites {
    pub fn any(&self, role: SiteRole) -> bool {
        self.0.iter().any(|site| role.holds(site))
    }

    /// The sites with `role` as nodes of `root`, the tree the sites were recorded from.
    pub fn nodes(&self, root: &SyntaxNode, role: SiteRole) -> Vec<SyntaxNode> {
        self.0
            .iter()
            .filter(|site| role.holds(site))
            .map(|site| match syntax::node_at(root, site.kind, site.range) {
                Some(node) => node,
                None => panic!(
                    "a recorded syntax site at {:?} is not in its tree",
                    site.range
                ),
            })
            .collect()
    }
}

#[salsa::tracked(returns(ref))]
pub fn file_syntax_sites(db: &dyn Db, file: SourceFile) -> SyntaxSites {
    let mut sites = Vec::new();
    syntax::walk_green(&parse(db, file).syntax_node(), |event| {
        let GreenEvent::Enter {
            node,
            parent,
            range,
        } = event
        else {
            return;
        };
        let kind = node.kind();
        let mut roles = 0;
        if annotations::is_green_inline_site(node) {
            roles |= SiteRole::InlineSite.bit();
        }
        if syntax::is_green_inline_annotation(kind, parent.map(syntax::GreenRef::kind)) {
            roles |= SiteRole::InlineAnnotation.bit();
        }
        if (parent.is_none() || kind == SyntaxKind::BRACE_EXPR)
            && node.has_child(SyntaxKind::ANNOTATION)
        {
            roles |= SiteRole::AnnotatedSequence.bit();
        }
        if metadata::is_green_attach_call(node) {
            roles |= SiteRole::AttachCall.bit();
        }
        if roles != 0
            || matches!(kind, SyntaxKind::ANNOTATION | SyntaxKind::TYPE_DEFINITION)
            || is_ry_syntax(kind)
        {
            sites.push(SyntaxSite { kind, range, roles });
        }
    });
    SyntaxSites(sites)
}

fn is_ry_syntax(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::RECORD_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::TAG_EXPR
            | SyntaxKind::MATCH_EXPR
    )
}

/// Absolute offsets, for the rendering edge and position-addressed IDE features
/// only; derived per-item values must use `item_syntax` to keep cutting off.
pub fn item_node<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<SyntaxNode> {
    let file = *item.file(db);
    let index = *item_span_positions(db, file).get(&item)?;
    let range = item_spans(db, file).get(index)?.range;
    let parse = parse(db, file);
    // Descend by range: a linear child walk per item is quadratic in statements.
    parse
        .syntax_node()
        .child_or_token_at_range(range)
        .and_then(|element| element.into_node())
        .filter(|node| node.text_range() == range)
}

#[salsa::tracked(returns(ref))]
fn item_tree_positions<'db>(db: &'db dyn Db, file: SourceFile) -> FxHashMap<Item<'db>, usize> {
    item_tree(db, file)
        .iter()
        .enumerate()
        .map(|(index, &item)| (item, index))
        .collect()
}

#[salsa::tracked(returns(ref))]
fn item_span_positions<'db>(db: &'db dyn Db, file: SourceFile) -> FxHashMap<Item<'db>, usize> {
    item_spans(db, file)
        .iter()
        .enumerate()
        .map(|(index, span)| (span.item, index))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemSyntax(pub rowan::GreenNode);

impl ItemSyntax {
    /// Offsets start at 0 (item-relative).
    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.0.clone())
    }
}

/// Derived from the item's green subtree only, so it cuts off across edits
/// elsewhere in the file.
#[salsa::tracked(returns(ref))]
pub fn item_hir<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<hir::Module> {
    let syntax = item_syntax(db, item)?;
    Some(hir::lower_item(&syntax.syntax_node()))
}

#[salsa::tracked(returns(ref))]
pub fn item_naming<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<naming::ItemNaming> {
    let module = item_hir(db, item).as_ref()?;
    let no_masked_verbs = FxHashMap::default();
    let masked_verbs = stubs::stubs(db).map_or(&no_masked_verbs, |library| &library.masked);
    Some(naming::resolve_item(module, masked_verbs))
}

/// Recognized structurally by the checker rather than through the stub corpus.
const BUILTIN_GLOBAL_NAMES: &[&str] = &["c", "list", "switch", "return", "stop"];

/// Includes stub declarations and checker builtins, not just package definitions.
pub fn package_scheme_exists(db: &dyn Db, name: &str) -> bool {
    if BUILTIN_GLOBAL_NAMES.contains(&name) {
        return true;
    }
    if ProjectFiles::try_get(db).is_some_and(|files| package_binds(db, files, name)) {
        return true;
    }
    stubs::stubs(db).is_some_and(|library| {
        library.schemes.contains_key(name)
            || library.nominals.contains(name)
            || library.known_exports.contains(name)
    })
}

/// An annotation applies only to the expression starting on the very next line;
/// a blank line, a plain comment, or the end of the sequence breaks the association.
pub enum AnnotationTarget {
    Attached(SyntaxNode),
    BlankLineSeparated,
    Dangling,
}

/// The single source of the annotation association rule for a statement sequence.
pub fn statement_annotations(parent: &SyntaxNode) -> Vec<(SyntaxNode, AnnotationTarget)> {
    let mut associations = Vec::new();
    let mut pending: Option<(SyntaxNode, usize)> = None;
    for child in parent.children_with_tokens() {
        let kind = child.kind();
        if kind == SyntaxKind::WHITESPACE {
            continue;
        }
        if kind == SyntaxKind::NEWLINE {
            if let Some((_, newlines)) = pending.as_mut() {
                *newlines += 1;
            }
            continue;
        }
        if let Some((annotation, newlines)) = pending.take() {
            let target = if child.as_token().is_some() && kind != SyntaxKind::COMMENT {
                AnnotationTarget::Dangling
            } else if newlines >= 2 {
                AnnotationTarget::BlankLineSeparated
            } else if let Some(node) = child.as_node()
                && (is_expression_kind(kind) || kind == SyntaxKind::ERROR)
            {
                AnnotationTarget::Attached(node.clone())
            } else {
                AnnotationTarget::Dangling
            };
            associations.push((annotation, target));
        }
        if let rowan::NodeOrToken::Node(node) = child
            && kind == SyntaxKind::ANNOTATION
        {
            pending = Some((node, 0));
        }
    }
    if let Some((annotation, _)) = pending {
        associations.push((annotation, AnnotationTarget::Dangling));
    }
    associations
}

/// Annotations are siblings of the item in the file tree, so they attach here, not in
/// `item_syntax`. A per-item firewall over one file-wide walk: untouched items'
/// annotation subtrees compare equal and cut off.
#[salsa::tracked(returns(clone), lru = 0)]
pub fn item_annotation_syntax<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<ItemSyntax> {
    file_item_annotations(db, *item.file(db))
        .get(&item)
        .cloned()
}

/// Matches targets to `item_spans` by range, so top-level classification lives in one place.
#[salsa::tracked(returns(ref), lru = 0)]
fn file_item_annotations<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> FxHashMap<Item<'db>, ItemSyntax> {
    let item_by_range: FxHashMap<TextRange, Item<'db>> = item_spans(db, file)
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

/// An item's lowered annotations, item-relative like `item_syntax`.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct ItemAnnotations<'db> {
    /// The root's `#:` block merged with the root statement's inline annotation.
    // Boxed: most items carry no annotation, and the memo would otherwise reserve a full
    // `Annotation` per item.
    pub root: Option<Box<annotations::Annotation<'db>>>,
    /// Annotations attached below the root, keyed by HIR id: each annotated statement's
    /// `#:` block merged with its inline annotation, plus the signature of every function
    /// literal no statement claims, such as one passed as an argument.
    pub expressions: Box<[(hir::ExprId, annotations::Annotation<'db>)]>,
}

/// Memoized so the checker reads lowered annotations instead of the item's tree: once
/// lowering is done, nothing on the check path needs syntax, and trees can be evicted.
#[salsa::tracked(returns(ref))]
pub fn item_annotations<'db>(db: &'db dyn Db, item: Item<'db>) -> ItemAnnotations<'db> {
    ItemAnnotations {
        root: root_annotation(db, item).map(Box::new),
        expressions: expression_annotations(db, item).into_boxed_slice(),
    }
}

fn root_annotation<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<annotations::Annotation<'db>> {
    let comment = item_annotation_syntax(db, item)
        .map(|syntax| annotations::lower_annotation(db, &syntax.syntax_node()));
    let root = item_syntax(db, item)?.syntax_node();
    let Some(site) = annotations::statement_inline_site(&root) else {
        return comment;
    };
    if !inline_annotations_enabled(db) {
        return comment;
    }
    let inline = annotations::lower_inline_annotation(db, &site);
    annotations::merge_annotations(db, comment, Some(inline), Some(&site))
}

fn expression_annotations<'db>(
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
    // Each annotated statement once, in source order, with its `#:` block if it has one.
    let mut targets: Vec<(SyntaxNode, Option<SyntaxNode>)> = Vec::new();
    let mut parents: Vec<SyntaxNode> = Vec::new();
    // One walk for both kinds of site; the item tree is position-free, so the file's
    // recorded sites cannot stand in for it.
    let (comments, inline_sites): (Vec<SyntaxNode>, Vec<SyntaxNode>) =
        matching_nodes(&root, |node, _| {
            node.kind() == SyntaxKind::ANNOTATION || annotations::is_green_inline_site(node)
        })
        .into_iter()
        .partition(|node| node.kind() == SyntaxKind::ANNOTATION);
    for node in comments {
        if let Some(parent) = node.parent()
            && !parents.contains(&parent)
        {
            parents.push(parent);
        }
    }
    for parent in parents {
        for (annotation, target) in statement_annotations(&parent) {
            if let AnnotationTarget::Attached(target) = target {
                targets.push((target, Some(annotation)));
            }
        }
    }
    let inline_enabled = !inline_sites.is_empty() && inline_annotations_enabled(db);
    if inline_enabled {
        for site in inline_sites {
            // A signature belongs to the statement assigning its function, unless that
            // statement types its binding itself.
            let statement = site
                .parent()
                .filter(|parent| annotations::statement_inline_site(parent).as_ref() == Some(&site))
                .unwrap_or(site);
            if statement == root || targets.iter().any(|(target, _)| *target == statement) {
                continue;
            }
            targets.push((statement, None));
        }
    }
    let mut attachments = Vec::new();
    for (statement, comment) in targets {
        let range = hir::significant_range(&statement);
        let Some(index) = module
            .expressions
            .iter()
            .position(|expression| expression.range == range)
        else {
            continue;
        };
        let comment = comment.map(|comment| annotations::lower_annotation(db, &comment));
        let site = annotations::statement_inline_site(&statement).filter(|_| inline_enabled);
        let inline = site
            .as_ref()
            .map(|site| annotations::lower_inline_annotation(db, site));
        let Some(lowered) = annotations::merge_annotations(db, comment, inline, site.as_ref())
        else {
            continue;
        };
        if lowered.declared.is_none() && lowered.new_nominal.is_none() && !lowered.trusted {
            continue;
        }
        attachments.push((hir::ExprId(index as u32), lowered));
    }
    attachments
}

/// Settings that change what the analysis means rather than which findings a host
/// publishes. Unset, every setting takes its default.
#[salsa::input(singleton, debug)]
pub struct AnalysisSettings {
    /// `[check] inline-annotations`. Off, inline annotations type nothing.
    pub inline_annotations: bool,
}

/// Read only where an inline annotation exists, so a project without any never
/// depends on the setting.
pub fn inline_annotations_enabled(db: &dyn Db) -> bool {
    AnalysisSettings::try_get(db).is_none_or(|settings| *settings.inline_annotations(db))
}

/// Package files first, in path order; the order decides last-writer-wins winners.
#[salsa::input(singleton, debug)]
pub struct ProjectFiles {
    #[returns(ref)]
    pub files: Vec<SourceFile>,
}

/// The layouts of the project's delimited data files (`.csv`, `.tsv`), keyed by their path
/// relative to the project root with `/` separators. Hosts sniff each file's head with
/// [`sniff::sniff_head`], so `fread("data/prices.csv")` types with the columns the file has.
#[salsa::input(singleton, debug)]
pub struct DataFiles {
    #[returns(ref)]
    pub layouts: BTreeMap<String, sniff::Layout>,
}

#[salsa::interned(debug)]
pub struct DataPath<'db> {
    #[returns(ref)]
    pub text: String,
}

/// Per path, so editing one data file re-checks only the reads of that file: salsa backdates
/// every other path's unchanged layout.
#[salsa::tracked(returns(ref))]
pub fn data_file_layout<'db>(db: &'db dyn Db, path: DataPath<'db>) -> Option<sniff::Layout> {
    let files = DataFiles::try_get(db)?;
    files.layouts(db).get(path.text(db).as_str()).cloned()
}

/// Set by a file's `# typing:` comments and `#: @strict` directives, the last winning.
/// It changes only which diagnostics a host publishes, never inference.
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

/// The mode plus the `typing:` comments with an unrecognized value.
#[salsa::tracked(returns(clone))]
pub fn file_typing_directives(
    db: &dyn Db,
    file: SourceFile,
) -> (Option<TypingMode>, Vec<(TextRange, String)>) {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut mode = None;
    let mut invalid = Vec::new();
    for element in root.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMENT => {
                let Some(rest) = token
                    .text()
                    .trim_start_matches('#')
                    .trim()
                    .strip_prefix("typing:")
                else {
                    continue;
                };
                // The whole remainder is the value: `typing: on gely` is a typo, not `on`.
                match rest.trim() {
                    "off" => mode = Some(TypingMode::Off),
                    "on" => mode = Some(TypingMode::On),
                    "strict" => mode = Some(TypingMode::Strict),
                    other => invalid.push((token.text_range(), other.to_owned())),
                }
            }
            rowan::NodeOrToken::Node(node) if node.kind() == SyntaxKind::ANNOTATION => {
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

#[salsa::tracked(returns(clone))]
pub fn file_type_definitions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<annotations::NamedDefinition<'db>> {
    let parse = parse(db, file);
    let root = parse.syntax_node();
    let mut definitions: Vec<annotations::NamedDefinition<'db>> = Vec::new();
    let mut inline_enabled = None;
    for node in root.children() {
        match node.kind() {
            SyntaxKind::ANNOTATION => {
                definitions.extend(annotations::lower_annotation(db, &node).definitions);
            }
            SyntaxKind::TYPE_DEFINITION => {
                let enabled = *inline_enabled.get_or_insert_with(|| inline_annotations_enabled(db));
                let paired = paired_comment_definition(&node).is_some();
                for mut definition in annotations::lower_inline_annotation(db, &node).definitions {
                    if paired {
                        // One definition written two ways: on, the inline one wins; off, the
                        // comment applies alone.
                        if !enabled {
                            continue;
                        }
                        definitions.retain(|known| known.name != definition.name);
                    } else if !enabled {
                        // Off, an inline definition types nothing, but its name stays declared
                        // so uses of it are not also reported as unknown types.
                        definition.body = types::unknown(db);
                    }
                    definitions.push(definition);
                }
            }
            _ => {}
        }
    }
    definitions
}

#[salsa::tracked(returns(ref))]
pub fn project_type_definitions<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
    let mut definitions = FxHashMap::default();
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

/// Whether any package file binds `name` at top level, including the inner targets of
/// `a <- b <- value` and conditional writes.
pub fn package_binds(db: &dyn Db, files: ProjectFiles, name: &str) -> bool {
    package_definitions(db, files).contains_key(name)
        || conditional_slot_items(db, files).contains_key(name)
}

/// Items writing a top-level name other than their own (conditionally, or nested as in
/// `y <- (x <- 1L)`); cross-item reads with no unconditional winner join their types.
#[salsa::tracked(returns(ref))]
pub fn conditional_slot_items<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> FxHashMap<String, Vec<Item<'db>>> {
    conditional_writers_of(db, files, DocumentKind::Package)
}

/// The helper files' counterpart of [`conditional_slot_items`].
#[salsa::tracked(returns(ref))]
pub fn test_conditional_slot_items<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> FxHashMap<String, Vec<Item<'db>>> {
    conditional_writers_of(db, files, DocumentKind::TestHelper)
}

fn conditional_writers_of<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    kind: DocumentKind,
) -> FxHashMap<String, Vec<Item<'db>>> {
    let mut writers: FxHashMap<String, Vec<Item<'db>>> = FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != kind {
            continue;
        }
        for &(name, item) in file_conditional_writers(db, file) {
            writers.entry(name.to_string()).or_default().push(item);
        }
    }
    writers
}

/// The file's share of [`conditional_slot_items`]. The project-wide walks fold these per-file
/// memos rather than reading per-item ones: after an edit, salsa then verifies one memo per
/// untouched file by durability instead of one edge per item in the project.
#[salsa::tracked(returns(ref))]
pub fn file_conditional_writers<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<(Symbol, Item<'db>)> {
    let mut writers = Vec::new();
    for &item in item_tree(db, file) {
        let own_name = item.name(db).clone();
        for &name in item_top_level_names(db, item) {
            if own_name.as_deref() != Some(name.as_str()) {
                writers.push((name, item));
            }
        }
    }
    writers
}

/// A per-name projection of the project-wide maps, so a check depends only on the names
/// it reads. `conditional_writers` serve the name when there is no winner.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct PackageName<'db> {
    pub winner: Option<Item<'db>>,
    pub conditional_writers: Vec<Item<'db>>,
}

#[salsa::tracked(returns(ref))]
pub fn package_name<'db>(db: &'db dyn Db, name: types::Name<'db>) -> PackageName<'db> {
    let Some(files) = ProjectFiles::try_get(db) else {
        return PackageName::default();
    };
    let text = name.text(db);
    PackageName {
        winner: package_definitions(db, files).get(text).copied(),
        conditional_writers: conditional_slot_items(db, files)
            .get(text)
            .cloned()
            .unwrap_or_default(),
    }
}

/// The test files' counterpart of [`package_name`].
#[salsa::tracked(returns(ref))]
pub fn test_name<'db>(db: &'db dyn Db, name: types::Name<'db>) -> PackageName<'db> {
    let Some(files) = ProjectFiles::try_get(db) else {
        return PackageName::default();
    };
    let text = name.text(db);
    PackageName {
        winner: test_definitions(db, files).get(text).copied(),
        conditional_writers: test_conditional_slot_items(db, files)
            .get(text)
            .cloned()
            .unwrap_or_default(),
    }
}

/// Tracked so readers cut off when the binding's scheme is unchanged. Cycles arise when a
/// statement reads its own conditionally-written name (`while (b > 0L) b <- b - 1L`).
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

#[salsa::tracked(returns(ref))]
pub fn package_definitions<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> FxHashMap<String, Item<'db>> {
    winners_of(db, files, DocumentKind::Package)
}

/// The last top-level definition of each name across the helper files, which testthat
/// sources into the one environment every test file runs in.
#[salsa::tracked(returns(ref))]
pub fn test_definitions<'db>(db: &'db dyn Db, files: ProjectFiles) -> FxHashMap<String, Item<'db>> {
    winners_of(db, files, DocumentKind::TestHelper)
}

fn winners_of<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    kind: DocumentKind,
) -> FxHashMap<String, Item<'db>> {
    let mut winners = FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != kind {
            continue;
        }
        for (name, item) in file_definitions(db, file) {
            winners.insert(name.clone(), *item);
        }
    }
    winners
}

/// The file's named function and value items in item order, the candidates for
/// [`package_definitions`]. Per file for the same reason as [`file_conditional_writers`].
#[salsa::tracked(returns(ref))]
pub fn file_definitions<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<(String, Item<'db>)> {
    item_tree(db, file)
        .iter()
        .filter(|item| matches!(*item.kind(db), ItemKind::Function | ItemKind::Value))
        .filter_map(|&item| Some((item.name(db).clone()?, item)))
        .collect()
}

/// The read set of each of [`file_definitions`], in the same order: the file's share of the
/// edges [`interface_sccs`] walks.
#[salsa::tracked(returns(ref))]
pub fn file_interface_reads(db: &dyn Db, file: SourceFile) -> Vec<Vec<Symbol>> {
    file_definitions(db, file)
        .iter()
        .map(|&(_, item)| item_interface_reads(db, item).iter().copied().collect())
        .collect()
}

/// `generic.class` split at the last dot (`as.character.myclass`), the generic being
/// stub-declared or in `generics`. Dispatch is invisible to the checker, so this keeps
/// methods and their mandated formals from looking unused.
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

/// Reads the read-set projection so a body edit does not re-derive the set. The
/// `UseMethod` argument is ignored: a mismatch is a runtime error R reports itself.
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

/// A firewall: whole-project walks depend on this read set rather than the full naming,
/// so a body edit that only shifts ranges backdates here.
#[salsa::tracked(returns(ref))]
pub fn item_interface_reads<'db>(db: &'db dyn Db, item: Item<'db>) -> BTreeSet<Symbol> {
    let Some(naming) = item_naming(db, item) else {
        return BTreeSet::new();
    };
    let mut reads: BTreeSet<Symbol> = naming.non_locals.values().copied().collect();
    reads.extend(naming.namespace_reads.values().filter_map(|read| read.name));
    // A call head bound locally to a non-function falls through to the project global of its
    // name, unless the chain already holds the top-level slot, which is that global.
    reads.extend(
        naming
            .callee_fallbacks
            .iter()
            .filter_map(|(callee, fallback)| {
                let slot = naming.resolutions.get(callee)?;
                let top_level = |slot: &naming::BindingId| {
                    naming
                        .bindings
                        .get(slot)
                        .is_some_and(|binding| binding.kind == naming::BindingKind::TopLevel)
                };
                if top_level(slot) || fallback.outer.iter().any(top_level) {
                    return None;
                }
                Some(naming.bindings.get(slot)?.name)
            }),
    );
    reads
}

#[salsa::tracked(returns(ref))]
pub fn item_top_level_names<'db>(db: &'db dyn Db, item: Item<'db>) -> BTreeSet<Symbol> {
    let Some(naming) = item_naming(db, item) else {
        return BTreeSet::new();
    };
    naming
        .bindings
        .values()
        .filter(|binding| binding.kind == naming::BindingKind::TopLevel)
        .map(|binding| binding.name)
        .collect()
}

/// Only cyclic groups are recorded: their schemes must resolve through one canonical
/// fixpoint, since iterating from whichever member was queried first would make the
/// round-cap pins depend on query order.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct InterfaceSccs<'db> {
    pub membership: FxHashMap<Item<'db>, u32>,
    /// Members in canonical order: project file order, then item order.
    pub groups: Vec<Vec<Item<'db>>>,
}

#[salsa::tracked(returns(ref))]
pub fn interface_sccs<'db>(db: &'db dyn Db, files: ProjectFiles) -> InterfaceSccs<'db> {
    let winners = package_definitions(db, files);
    let package_files: Vec<SourceFile> = files
        .files(db)
        .iter()
        .copied()
        .filter(|file| *file.kind(db) == DocumentKind::Package)
        .collect();
    let mut nodes: Vec<Item<'db>> = Vec::new();
    let mut position: FxHashMap<Item<'db>, usize> = FxHashMap::default();
    for &file in &package_files {
        for &(_, item) in file_definitions(db, file) {
            position.insert(item, nodes.len());
            nodes.push(item);
        }
    }
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(nodes.len());
    for &file in &package_files {
        for reads in file_interface_reads(db, file) {
            edges.push(
                reads
                    .iter()
                    .filter_map(|name| position.get(winners.get(name.as_str())?).copied())
                    .collect(),
            );
        }
    }

    // Iterative: recursion depth would scale with package size.
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

/// A per-item projection, so reshaping the graph elsewhere leaves acyclic items' checks green.
#[salsa::tracked(returns(copy))]
fn interface_group<'db>(db: &'db dyn Db, files: ProjectFiles, item: Item<'db>) -> Option<u32> {
    interface_sccs(db, files).membership.get(&item).copied()
}

/// Independent of which member was queried first: each round re-checks every member
/// against the previous round's table, and members still changing at the cap pin to
/// `Unknown`. Members are checked directly, never via `item_check`, so no salsa cycle forms.
#[salsa::tracked(
    returns(ref),
    cycle_fn = scc_schemes_recover,
    cycle_initial = scc_schemes_initial
)]
pub fn scc_schemes<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    group: u32,
) -> FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    let sccs = interface_sccs(db, files);
    let Some(members) = sccs.groups.get(group as usize) else {
        return FxHashMap::default();
    };
    let winners = package_definitions(db, files);
    // Only a name's winner is reachable through reads.
    let member_names: Vec<(String, Item<'db>)> = members
        .iter()
        .filter_map(|&item| {
            let name = item.name(db).clone()?;
            (winners.get(&name) == Some(&item)).then_some((name, item))
        })
        .collect();

    let unknown_scheme = types::TypeScheme::monomorphic(types::unknown(db));
    let mut table: FxHashMap<String, types::TypeScheme<'db>> = member_names
        .iter()
        .map(|(name, _)| (name.clone(), unknown_scheme.clone()))
        .collect();
    let mut schemes: FxHashMap<Item<'db>, types::TypeScheme<'db>> = members
        .iter()
        .map(|&item| (item, unknown_scheme.clone()))
        .collect();
    for _round in 0..SCHEME_ROUND_CAP {
        let mut next_schemes = FxHashMap::default();
        for &item in members {
            let scheme =
                check_member_scheme(db, item, &table).unwrap_or_else(|| unknown_scheme.clone());
            next_schemes.insert(item, scheme);
        }
        let next_table: FxHashMap<String, types::TypeScheme<'db>> = member_names
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
    // Pinning every member is the only entry-order-free refusal.
    for scheme in schemes.values_mut() {
        *scheme = unknown_scheme.clone();
    }
    schemes
}

fn unknown_group_schemes<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    group: u32,
) -> FxHashMap<Item<'db>, types::TypeScheme<'db>> {
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
) -> FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    unknown_group_schemes(db, files, group)
}

/// Backstop for edges the static graph cannot see (a name the checker constructs, such
/// as an S3 method), which re-enter this group through `global_scheme`.
fn scc_schemes_recover<'db>(
    db: &'db dyn Db,
    _cycle: &salsa::Cycle,
    last_provisional: &FxHashMap<Item<'db>, types::TypeScheme<'db>>,
    value: FxHashMap<Item<'db>, types::TypeScheme<'db>>,
    files: ProjectFiles,
    group: u32,
) -> FxHashMap<Item<'db>, types::TypeScheme<'db>> {
    // Refuse on the first disagreement: salsa iterating this query would multiply the
    // group's own bounded rounds and can exhaust memory.
    if &value == last_provisional {
        return value;
    }
    unknown_group_schemes(db, files, group)
}

fn check_member_scheme<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    table: &FxHashMap<String, types::TypeScheme<'db>>,
) -> Option<types::TypeScheme<'db>> {
    let base = SalsaGlobals::for_item(db, item);
    let globals = SccGlobals {
        base: &base,
        members: table,
    };
    check_item_against(db, item, &globals)?.scheme
}

fn check_item_against<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    globals: &dyn check::GlobalEnv<'db>,
) -> Option<check::ItemCheck<'db>> {
    let module = item_hir(db, item).as_ref()?;
    let naming = item_naming(db, item).as_ref()?;
    let annotations = item_annotations(db, item);
    Some(check::check_item_with_annotation(
        db,
        module,
        naming,
        annotations.root.as_deref(),
        &annotations.expressions,
        Some(globals),
    ))
}

/// Always `item_check`'s scheme, so export and hover share one source of truth.
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

/// A scheme still changing after this many rounds pins to `Unknown`.
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

#[salsa::tracked(
    returns(clone),
    cycle_fn = item_check_recover,
    cycle_initial = item_check_initial
)]
pub fn item_check<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<check::ItemCheck<'db>> {
    // Recursion depth follows the longest chain of definitions each reading the next
    // (thousands of `df <- step(df)` rebindings), so no fixed stack bounds it.
    stacker::maybe_grow(256 * 1024, 16 * 1024 * 1024, || check_item(db, item))
}

fn check_item<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<check::ItemCheck<'db>> {
    let mut check = check_item_against(db, item, &SalsaGlobals::for_item(db, item))?;
    // A cyclic-group member exports the group's fixpoint value, not this one-step
    // re-derivation, which runs one propagation hop ahead of what readers see.
    if check.scheme.is_some()
        && let Some(files) = ProjectFiles::try_get(db)
        && let Some(group) = interface_group(db, files, item)
    {
        check.scheme = scc_schemes(db, files, group).get(&item).cloned();
        // A clean body whose export still carries `Unknown` owes it to the cycle itself, so
        // strict mode marks the whole binding.
        if check.errors.is_empty()
            && check.strict_origins.is_empty()
            && let Some(scheme) = &check.scheme
            && types::contains_unknown(db, scheme.body)
            && let Some(module) = item_hir(db, item)
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
    // `item_check` is the head salsa iterates when a definition reads its own scheme, so the
    // round cap lives here. Pin from the previous value, never this round's: the check's
    // other fields keep moving, so the equality test above would never pass.
    if cycle.iteration() >= SCHEME_ROUND_CAP {
        return last_provisional
            .clone()
            .or(value)
            .map(|check| refuse_check(db, check));
    }
    value
}

/// Both export surfaces cut to `Unknown`; findings inside the item are kept. Must be
/// idempotent so the recovery above terminates (`refusal_is_idempotent` pins it).
fn refuse_check<'db>(db: &'db dyn Db, check: check::ItemCheck<'db>) -> check::ItemCheck<'db> {
    let refused = types::TypeScheme::monomorphic(types::unknown(db));
    check::ItemCheck {
        scheme: Some(refused.clone()),
        top_level_bindings: check
            .top_level_bindings
            .iter()
            .map(|(name, _)| (*name, refused.clone()))
            .collect(),
        ..check
    }
}

/// Every lookup goes through a per-name query, so a check depends on exactly the names it reads.
struct SalsaGlobals<'db> {
    db: &'db dyn Db,
    item: Item<'db>,
    files: Option<ProjectFiles>,
    file: SourceFile,
    // A body reads the same few globals many times, and each salsa read pays for interning
    // its key and verifying the memo, so a check looks each one up once.
    names: RefCell<FxHashMap<Box<str>, types::Name<'db>>>,
    schemes: RefCell<FxHashMap<(types::Name<'db>, bool), Option<types::TypeScheme<'db>>>>,
    item_frame_binders: OnceCell<&'db FxHashMap<Symbol, (Option<Item<'db>>, Option<Item<'db>>)>>,
    frame_binders: RefCell<FxHashMap<(types::Name<'db>, bool), Option<Item<'db>>>>,
    package_names: RefCell<FxHashMap<types::Name<'db>, &'db PackageName<'db>>>,
    test_names: RefCell<FxHashMap<types::Name<'db>, &'db PackageName<'db>>>,
}

/// A script's own declarations are visible to itself alone and shadow project names.
/// Memoized per file so the merge runs once per file, not once per checked item.
#[salsa::tracked(returns(ref))]
pub fn visible_type_definitions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
    let mut definitions = stubs::stubs(db)
        .map(|library| library.type_definitions.clone())
        .unwrap_or_default();
    if let Some(files) = ProjectFiles::try_get(db) {
        for (name, definition) in project_type_definitions(db, files) {
            definitions.insert(*name, definition.clone());
        }
    }
    if *file.kind(db) != DocumentKind::Package {
        for definition in file_type_definitions(db, file) {
            definitions.insert(definition.name, definition);
        }
    }
    definitions
}

#[salsa::tracked(returns(ref))]
fn visible_arithmetic_classes(db: &dyn Db, file: SourceFile) -> FxHashSet<String> {
    let mut classes = stub_arithmetic_classes(db);
    if let Some(files) = ProjectFiles::try_get(db) {
        classes.extend(project_arithmetic_classes(db, files).iter().cloned());
    }
    classes.extend(file_arithmetic_classes(db, file).iter().cloned());
    classes
}

/// A project's `+.string` method dispatches on a class attribute that `class(x) <- "string"`
/// sets on a plain list, which no type tracks, so arithmetic on a list may be that method.
#[salsa::tracked]
fn has_untracked_arithmetic_class(db: &dyn Db, file: SourceFile) -> bool {
    let definitions = visible_type_definitions(db, file);
    let tracked = |class: &String| definitions.keys().any(|name| name.text(db) == class);
    let project_classes = ProjectFiles::try_get(db)
        .map(|files| project_arithmetic_classes(db, files))
        .unwrap_or_default();
    project_classes
        .iter()
        .chain(file_arithmetic_classes(db, file).iter())
        .any(|class| !tracked(class))
}

/// Past this many conditional writers a slot is `Unknown`: each writer costs an item
/// check per read, and a union of dozens of unrelated types helps no diagnostic.
const CONDITIONAL_SLOT_JOIN_CAP: usize = 8;

/// Indexed by name: scanning the item list per lookup is quadratic in items × references.
#[salsa::tracked(returns(ref))]
fn file_binders<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> FxHashMap<String, Vec<(usize, Item<'db>)>> {
    let mut binders: FxHashMap<String, Vec<(usize, Item<'db>)>> = FxHashMap::default();
    for (index, &item) in item_tree(db, file).iter().enumerate() {
        let own_name = item.name(db).clone();
        if let Some(name) = &own_name {
            binders.entry(name.clone()).or_default().push((index, item));
        }
        for name in item_top_level_names(db, item) {
            if own_name.as_deref() != Some(name.as_str()) {
                binders
                    .entry(name.to_string())
                    .or_default()
                    .push((index, item));
            }
        }
    }
    binders
}

/// An immediate read sees the nearest earlier writer in the file (R sources top-down).
/// A deferred read (inside a closure) sees the file's last writer in a script; in a
/// package it sees the project-wide winner instead, so callers skip this query. In a test
/// file it sees the reading statement's own writer first: each `test_that` block runs in its
/// own environment, so a helper it defines is the one its closures call.
#[salsa::tracked(returns(copy))]
fn frame_binder<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    name: types::Name<'db>,
    deferred: bool,
) -> Option<Item<'db>> {
    frame_binder_of(db, item, name.text(db), deferred)
}

/// [`frame_binder`] for every name the item's naming reads from outside its own frame, as
/// `(immediate, deferred)`. One query per item replaces a query per item and name, each of
/// which interned a key and kept a memo, and its value still changes only when a binder of
/// one of the item's own names does.
#[salsa::tracked(returns(ref))]
fn item_frame_binders<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
) -> FxHashMap<Symbol, (Option<Item<'db>>, Option<Item<'db>>)> {
    let mut binders = FxHashMap::default();
    let Some(naming) = item_naming(db, item) else {
        return binders;
    };
    let names = naming
        .non_locals
        .values()
        .chain(naming.quiet_reads.values())
        .chain(naming.quiet_operator_reads.iter());
    for &name in names {
        binders.entry(name).or_insert_with(|| {
            (
                frame_binder_of(db, item, name.as_str(), false),
                frame_binder_of(db, item, name.as_str(), true),
            )
        });
    }
    binders
}

fn frame_binder_of<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    name: &str,
    deferred: bool,
) -> Option<Item<'db>> {
    let file = *item.file(db);
    let binders = file_binders(db, file).get(name)?;
    if deferred
        && matches!(
            *file.kind(db),
            DocumentKind::Test | DocumentKind::TestHelper
        )
        && binders.iter().any(|&(_, binder)| binder == item)
    {
        return Some(item);
    }
    let visible = match deferred {
        true => &binders[..],
        false => {
            let index = item_tree_positions(db, file)
                .get(&item)
                .copied()
                .unwrap_or_else(|| item_tree(db, file).len());
            &binders[..binders.partition_point(|&(at, _)| at < index)]
        }
    };
    visible.last().map(|&(_, item)| item)
}

impl<'db> SalsaGlobals<'db> {
    fn for_item(db: &'db dyn Db, item: Item<'db>) -> SalsaGlobals<'db> {
        SalsaGlobals {
            db,
            item,
            files: ProjectFiles::try_get(db),
            file: *item.file(db),
            names: RefCell::default(),
            schemes: RefCell::default(),
            item_frame_binders: OnceCell::new(),
            frame_binders: RefCell::default(),
            package_names: RefCell::default(),
            test_names: RefCell::default(),
        }
    }

    fn intern(&self, name: &str) -> types::Name<'db> {
        if let Some(&interned) = self.names.borrow().get(name) {
            return interned;
        }
        let interned = types::Name::new(self.db, name);
        self.names.borrow_mut().insert(name.into(), interned);
        interned
    }

    fn frame_definition(&self, name: types::Name<'db>, deferred: bool) -> Option<Item<'db>> {
        if deferred && *self.file.kind(self.db) == DocumentKind::Package {
            return None;
        }
        let text = name.text(self.db);
        if let Some(&(immediate, later)) = self
            .item_frame_binders
            .get_or_init(|| item_frame_binders(self.db, self.item))
            .get(&Symbol::new(text))
        {
            return if deferred { later } else { immediate };
        }
        *self
            .frame_binders
            .borrow_mut()
            .entry((name, deferred))
            .or_insert_with(|| frame_binder(self.db, self.item, name, deferred))
    }

    /// The namespaces a read of `name` searches past the item's own frame, nearest first: a
    /// test or helper file sees the helpers' bindings before the package namespace.
    fn layers(&self, name: types::Name<'db>) -> impl Iterator<Item = &'db PackageName<'db>> {
        let files = self.files;
        let test = files
            .filter(|_| {
                matches!(
                    *self.file.kind(self.db),
                    DocumentKind::Test | DocumentKind::TestHelper
                )
            })
            .map(|_| {
                *self
                    .test_names
                    .borrow_mut()
                    .entry(name)
                    .or_insert_with(|| test_name(self.db, name))
            });
        let package = files.map(|_| {
            *self
                .package_names
                .borrow_mut()
                .entry(name)
                .or_insert_with(|| package_name(self.db, name))
        });
        test.into_iter().chain(package)
    }

    fn conditional_slot_scheme(
        &self,
        layer: &PackageName<'db>,
        name: types::Name<'db>,
    ) -> Option<types::TypeScheme<'db>> {
        let writers = &layer.conditional_writers;
        if writers.len() > CONDITIONAL_SLOT_JOIN_CAP {
            return Some(types::TypeScheme::monomorphic(types::unknown(self.db)));
        }
        let mut schemes: Vec<types::TypeScheme<'db>> = writers
            .iter()
            .filter_map(|&item| statement_binding_scheme(self.db, item, name))
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

    fn binder_scheme(
        &self,
        item: Item<'db>,
        name: types::Name<'db>,
    ) -> Option<types::TypeScheme<'db>> {
        if item.name(self.db).as_deref() == Some(name.text(self.db)) {
            return Some(global_scheme(self.db, item));
        }
        statement_binding_scheme(self.db, item, name)
    }

    fn lookup_scheme(
        &self,
        name: &str,
        interned: types::Name<'db>,
        deferred: bool,
    ) -> Option<types::TypeScheme<'db>> {
        if let Some(item) = self.frame_definition(interned, deferred)
            && let Some(scheme) = self.binder_scheme(item, interned)
        {
            return Some(scheme);
        }
        for layer in self.layers(interned) {
            if let Some(item) = layer.winner {
                return Some(global_scheme(self.db, item));
            }
            if let Some(scheme) = self.conditional_slot_scheme(layer, interned) {
                return Some(scheme);
            }
        }
        // A plain-value read of an overloaded stub takes the last candidate: the corpus orders
        // candidates specific-first, so the last is the most general.
        let library = stubs::stubs(self.db)?;
        library.schemes.get(name)?.last().cloned()
    }

    fn winner(&self, name: types::Name<'db>) -> Option<Item<'db>> {
        self.layers(name).find_map(|layer| layer.winner)
    }
}

impl<'db> check::GlobalEnv<'db> for SalsaGlobals<'db> {
    fn scheme(&self, name: &str, deferred: bool) -> Option<types::TypeScheme<'db>> {
        let interned = self.intern(name);
        if let Some(scheme) = self.schemes.borrow().get(&(interned, deferred)) {
            return scheme.clone();
        }
        let scheme = self.lookup_scheme(name, interned, deferred);
        self.schemes
            .borrow_mut()
            .insert((interned, deferred), scheme.clone());
        scheme
    }

    fn defined_in_project(&self, name: &str, deferred: bool) -> bool {
        let interned = self.intern(name);
        self.frame_definition(interned, deferred).is_some()
            || self
                .layers(interned)
                .any(|binding| binding.winner.is_some() || !binding.conditional_writers.is_empty())
    }

    fn earlier_binding(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        let interned = self.intern(name);
        if let Some(item) = self.frame_definition(interned, false) {
            return self.binder_scheme(item, interned);
        }
        Some(global_scheme(self.db, self.winner(interned)?))
    }

    fn namespace_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        if *self.file.kind(self.db) == DocumentKind::Package {
            return self.scheme(name, false);
        }
        let interned = self.intern(name);
        if let Some(layer) = self.files.map(|_| package_name(self.db, interned)) {
            if let Some(item) = layer.winner {
                return Some(global_scheme(self.db, item));
            }
            if let Some(scheme) = self.conditional_slot_scheme(layer, interned) {
                return Some(scheme);
            }
        }
        stubs::stubs(self.db)?.schemes.get(name)?.last().cloned()
    }

    fn enclosing_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        if *self.file.kind(self.db) == DocumentKind::Package {
            return None;
        }
        let interned = self.intern(name);
        self.layers(interned).find_map(|layer| {
            layer
                .winner
                .map(|item| global_scheme(self.db, item))
                .or_else(|| self.conditional_slot_scheme(layer, interned))
        })
    }

    fn library_candidates(&self, name: &str) -> Option<Vec<types::TypeScheme<'db>>> {
        let library = stubs::stubs(self.db)?;
        match library.schemes.get(name) {
            Some(candidates) => Some(candidates.clone()),
            None => library.known_exports.contains(name).then(Vec::new),
        }
    }

    fn overloads(&self, name: &str, deferred: bool) -> Option<Vec<types::TypeScheme<'db>>> {
        let candidates = stubs::stubs(self.db)?.schemes.get(name)?;
        if candidates.len() < 2 {
            return None;
        }
        // A project definition shadows the stub set, disabling per-call overload selection.
        let interned = self.intern(name);
        if self.frame_definition(interned, deferred).is_some() || self.winner(interned).is_some() {
            return None;
        }
        Some(candidates.clone())
    }

    fn type_definitions(
        &self,
    ) -> &'db FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
        visible_type_definitions(self.db, self.file)
    }

    fn arithmetic_classes(&self) -> &'db FxHashSet<String> {
        visible_arithmetic_classes(self.db, self.file)
    }

    fn has_untracked_arithmetic_class(&self) -> bool {
        *has_untracked_arithmetic_class(self.db, self.file)
    }

    fn stub_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        stubs::stubs(self.db)?.schemes.get(name)?.last().cloned()
    }
}

#[salsa::tracked(returns(clone))]
fn stub_arithmetic_classes(db: &dyn Db) -> FxHashSet<String> {
    let Some(library) = stubs::stubs(db) else {
        return FxHashSet::default();
    };
    arithmetic_classes_among(library.schemes.keys().map(String::as_str))
}

#[salsa::tracked(returns(clone))]
fn project_arithmetic_classes(db: &dyn Db, files: ProjectFiles) -> FxHashSet<String> {
    arithmetic_classes_among(package_definitions(db, files).keys().map(String::as_str))
}

#[salsa::tracked(returns(ref))]
fn file_arithmetic_classes(db: &dyn Db, file: SourceFile) -> FxHashSet<String> {
    arithmetic_classes_among(
        item_tree(db, file)
            .iter()
            .filter_map(|item| item.name(db).as_deref()),
    )
}

fn arithmetic_classes_among<'a>(names: impl Iterator<Item = &'a str>) -> FxHashSet<String> {
    let prefixes: Vec<String> = check::arithmetic_method_prefixes().collect();
    let mut classes = FxHashSet::default();
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

/// Member names resolve from the round table, never through `global_scheme`, which would
/// re-enter the group.
struct SccGlobals<'db, 'a> {
    base: &'a SalsaGlobals<'db>,
    members: &'a FxHashMap<String, types::TypeScheme<'db>>,
}

impl<'db> check::GlobalEnv<'db> for SccGlobals<'db, '_> {
    fn scheme(&self, name: &str, deferred: bool) -> Option<types::TypeScheme<'db>> {
        self.members
            .get(name)
            .cloned()
            .or_else(|| self.base.scheme(name, deferred))
    }

    fn earlier_binding(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        self.members
            .get(name)
            .cloned()
            .or_else(|| self.base.earlier_binding(name))
    }

    fn library_candidates(&self, name: &str) -> Option<Vec<types::TypeScheme<'db>>> {
        self.base.library_candidates(name)
    }

    /// Only a package file's group can hold a namespace member.
    fn namespace_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        match self.members.get(name) {
            Some(scheme) if *self.base.file.kind(self.base.db) == DocumentKind::Package => {
                Some(scheme.clone())
            }
            _ => self.base.namespace_scheme(name),
        }
    }

    /// A member is read from the round table here too: through the base it would re-enter
    /// the group.
    fn enclosing_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        match self.members.get(name) {
            Some(scheme) if *self.base.file.kind(self.base.db) != DocumentKind::Package => {
                Some(scheme.clone())
            }
            _ => self.base.enclosing_scheme(name),
        }
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
    ) -> &'db FxHashMap<types::Name<'db>, annotations::NamedDefinition<'db>> {
        self.base.type_definitions()
    }

    /// Must delegate, not default: an empty set makes `Date + 1` in any recursive function
    /// collapse the whole scheme to `Unknown`.
    fn arithmetic_classes(&self) -> &'db FxHashSet<String> {
        self.base.arithmetic_classes()
    }

    fn has_untracked_arithmetic_class(&self) -> bool {
        self.base.has_untracked_arithmetic_class()
    }

    fn stub_scheme(&self, name: &str) -> Option<types::TypeScheme<'db>> {
        self.base.stub_scheme(name)
    }
}

/// File-absolute (edge-only, like [`item_node`]): the assignment target or the
/// `setGeneric` name string. Memoized so that a cold check reads it from another file without
/// reparsing that file's evicted tree.
#[salsa::tracked(returns(copy))]
pub fn item_name_range<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<TextRange> {
    let node = item_node(db, item)?;
    top_level_definition(&node).map(|definition| definition.name_range)
}

/// The single reading of a statement's shape for both item identity and name range.
struct TopLevelDefinition {
    kind: ItemKind,
    name: String,
    name_range: TextRange,
}

fn top_level_definition(node: &SyntaxNode) -> Option<TopLevelDefinition> {
    use syntax::SyntaxKind;
    if let Some((name, name_range)) = set_generic_target(node) {
        return Some(TopLevelDefinition {
            kind: ItemKind::Function,
            name,
            name_range,
        });
    }
    if node.kind() != SyntaxKind::BINARY_EXPR {
        return None;
    }
    let binary = BinaryExpr::cast(node.clone())?;
    let operator = binary.operator()?;
    let (lhs, rhs) = (binary.lhs(), binary.rhs());
    // `:=` binds nothing: R calls a function of that name (data.table, rlang).
    let (target, value) = match operator.kind() {
        SyntaxKind::LESS_MINUS | SyntaxKind::EQ | SyntaxKind::LESS2_MINUS => (lhs, rhs),
        SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => (rhs, lhs),
        _ => return None,
    };
    let target = target?;
    let name = ast::Name::cast(target.clone())
        .and_then(|name| name.text())
        .or_else(|| {
            (target.kind() == SyntaxKind::LITERAL
                && target
                    .first_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::STRING))
            .then(|| {
                let text = target.text().to_string();
                text.trim_matches(['"', '\'']).to_owned()
            })
        })?;
    let kind = match value.map(|value| value.kind()) {
        Some(SyntaxKind::FUNCTION_DEF) => ItemKind::Function,
        _ => ItemKind::Value,
    };
    Some(TopLevelDefinition {
        kind,
        name,
        name_range: target.text_range(),
    })
}

/// The name a top-level `setGeneric` binds, with its range inside the string: the only S4
/// registration that creates a bare-name binding (`setClass`/`setMethod` bind nothing).
fn set_generic_target(node: &SyntaxNode) -> Option<(String, TextRange)> {
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
    let is_named_argument = |argument: &SyntaxNode| {
        argument.children_with_tokens().any(|element| {
            element
                .as_token()
                .is_some_and(|t| t.kind() == SyntaxKind::EQ)
        })
    };
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
    // Without the quotes: the span a rename rewrites.
    let range = string.text_range();
    let content = TextRange::new(
        range.start() + TextSize::from(1),
        range.end() - TextSize::from(1),
    );
    (!name.is_empty()).then_some((name, content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use salsa::Setter as _;

    fn function_item<'db>(db: &'db RootDatabase, file: SourceFile, name: &str) -> Item<'db> {
        Item::new(db, file, ItemKind::Function, Some(name.to_owned()), None, 0)
    }

    #[test]
    fn refusal_is_idempotent() {
        let db = RootDatabase::default();
        let integer = types::scalar(&db, types::Atomic::Integer);
        let check = check::ItemCheck {
            expression_types: Default::default(),
            pattern_types: Default::default(),
            literal_expectations: Default::default(),
            errors: Vec::new(),
            strict_origins: Vec::new(),
            scheme: Some(types::TypeScheme::monomorphic(integer)),
            selected_overloads: Default::default(),
            masked_reads: Default::default(),
            column_misses: Default::default(),
            match_findings: Vec::new(),
            callee_targets: Default::default(),
            top_level_bindings: vec![(
                hir::Symbol::new("value"),
                types::TypeScheme::monomorphic(integer),
            )],
        };

        let once = refuse_check(&db, check);
        let twice = refuse_check(&db, once.clone());
        assert_eq!(once, twice, "re-pinning a pinned check must reproduce it");

        let unknown = types::TypeScheme::monomorphic(types::unknown(&db));
        assert_eq!(once.scheme.as_ref(), Some(&unknown));
        assert_eq!(
            once.top_level_bindings,
            vec![(hir::Symbol::new("value"), unknown)]
        );
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

        file.set_text(&mut db)
            .to("f <- function(x) x + 100\ng <- function(y) y\n".to_owned());
        {
            let items = item_tree(&db, file);
            assert_eq!(items.len(), 2);
            let g = function_item(&db, file, "g");
            assert_eq!(items[1], g);
            let g_after = item_syntax(&db, g).expect("g still exists");
            assert_eq!(g_before, g_after);
        }

        file.set_text(&mut db).to("f <- function(x) x\n".to_owned());
        let g = function_item(&db, file, "g");
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
        let add = function_item(&db, file, "add");
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

        file.set_text(&mut db)
            .to("pad <- 100000\nadd <- function(x, y = 2) x + y\n".to_owned());
        let add = function_item(&db, file, "add");
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

        let use_item = function_item(&db, main, "use");
        let use_check = item_check(&db, use_item).expect("use checks");
        assert!(use_check.errors.is_empty(), "{:?}", use_check.errors);

        let bad_item = function_item(&db, main, "bad");
        let bad_check = item_check(&db, bad_item).expect("bad checks");
        assert!(
            !bad_check.errors.is_empty(),
            "character into numeric parameter must report"
        );

        util.set_text(&mut db)
            .to("add <- function(x, y) y + x\n".to_owned());
        let use_item = function_item(&db, main, "use");
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
        let even = function_item(&db, file, "is_even");
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
        let item = function_item(&db, file, "count");
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
