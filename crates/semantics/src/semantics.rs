pub mod annotations;
pub mod check;
pub mod dense;
pub mod diagnostics;
pub mod exhaustiveness;
pub mod hir;
pub mod infer;
pub mod lints;
pub mod load_index;
pub mod metadata;
pub mod naming;
pub mod native;
pub mod session;
pub mod sorted;
pub mod stubs;
pub mod testing;
pub mod top_level;
pub mod types;

use {
    crate::{
        check::Empty,
        hir::{ExpressionKind, Symbol},
        naming::RefClassKey,
        sorted::SortedSet,
    },
    rustc_hash::{FxHashMap, FxHashSet},
    salsa::plumbing::AsId,
    std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        num::NonZero,
        sync::{Arc, Mutex},
    },
    syntax::{
        NodeOrToken, NodeRef, Parse, SyntaxKind, SyntaxNode, TextRange, TextSize, Tree,
        annotation::{AnnotationTarget, formals, statement_annotations},
        ast::{is_expression_kind, name_text},
        matching_nodes, node_text, paired_comment_definition,
    },
    thin_vec::ThinVec,
};

/// Analysis walks recurse as deeply as the source nests, so commands and analysis workers
/// run on threads with this stack rather than `ulimit -s`'s.
pub const ANALYSIS_STACK_SIZE: usize = 64 * 1024 * 1024;

#[salsa::db]
pub trait Db: salsa::Database {
    /// The one home of the host's syntax trees ([`parse`]).
    fn trees(&self) -> &TreeStore;

    /// Whether an item's check keeps the per-expression facts that only editor features and
    /// rewrites read (expression and pattern types, chosen overloads, callee targets). A host
    /// that only reports diagnostics leaves them out, because resolving every recorded type is a
    /// large share of a check's cost.
    fn editor_facts(&self) -> bool {
        true
    }
}

#[salsa::db]
#[derive(Clone, Default)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
    /// Shared across storage-handle clones, so every handle reads and fills the same trees.
    trees: Arc<TreeStore>,
    without_editor_facts: bool,
}

impl RootDatabase {
    /// For a command that analyses a project once and exits, such as `ry check`. It reads each
    /// file's tree in one pass, which holds the tree, and afterwards mostly what was derived
    /// from it, so it keeps a few trees per core rather than one per file. `editor_facts` is
    /// [`Db::editor_facts`].
    pub fn for_command(editor_facts: bool) -> RootDatabase {
        RootDatabase {
            storage: salsa::Storage::default(),
            trees: Arc::new(TreeStore::bounded(
                std::thread::available_parallelism().map_or(1, NonZero::get) * 4,
            )),
            without_editor_facts: !editor_facts,
        }
    }
}

#[salsa::db]
impl salsa::Database for RootDatabase {}

#[salsa::db]
impl Db for RootDatabase {
    fn trees(&self) -> &TreeStore {
        &self.trees
    }

    fn editor_facts(&self) -> bool {
        !self.without_editor_facts
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
    /// The file's index in a console session, whose earlier commands it reads (see
    /// [`session`]).
    #[default]
    pub command: Option<u32>,
}

/// The file's syntax tree, from the host's [`TreeStore`]. No query memoizes a tree: a query that
/// reads one records the read of the file's text, and keeps only what it derives.
pub fn parse(db: &dyn Db, file: SourceFile) -> Parse {
    db.trees()
        .parse(file, file.text(db), salsa::plumbing::current_revision(db))
}

/// The syntax trees a host holds, one per file at most. Trees live here rather than in query
/// memos for two reasons. An edit reparses by splicing the file's previous tree, which a memo
/// has already dropped by the time the query runs again. And how long a tree stays is the
/// host's choice, not the analysis's: an editor session keeps every tree, because position
/// requests read them at any time, while a command that checks once keeps the tree of each file
/// a worker holds ([`TreeStore::hold`]) and a few more, which bounds its memory by its thread
/// count instead of the project.
///
/// A dropped tree is parsed again on the next read, so capacity changes time and memory only:
/// a spliced or repeated parse is byte- and error-identical to a fresh one.
pub struct TreeStore {
    trees: Mutex<StoredTrees>,
    capacity: usize,
}

#[derive(Default)]
struct StoredTrees {
    entries: FxHashMap<SourceFile, StoredTree>,
    /// How many [`TreeHold`]s are open on a file. A held file's tree is never dropped.
    holds: FxHashMap<SourceFile, u32>,
    /// Counts reads, so the entry with the smallest stamp is the least recently read.
    clock: u64,
    parses: u64,
}

/// Keeps one file's tree in a bounded store until dropped. A worker that derives everything it
/// needs from a file in one pass holds the file for that pass, so the pass parses once however
/// many trees other workers read meanwhile.
pub struct TreeHold<'a> {
    store: &'a TreeStore,
    file: SourceFile,
}

impl Drop for TreeHold<'_> {
    fn drop(&mut self) {
        let mut trees = self.store.trees.lock().expect("tree store lock");
        if let Some(count) = trees.holds.get_mut(&self.file) {
            *count -= 1;
            if *count == 0 {
                trees.holds.remove(&self.file);
            }
        }
    }
}

struct StoredTree {
    parse: Parse,
    /// The revision at which the tree was last compared with the file's text. A text cannot
    /// change within a revision, so a read at the same revision skips the comparison.
    confirmed: salsa::Revision,
    last_read: u64,
}

impl Default for TreeStore {
    /// A store that keeps every tree.
    fn default() -> TreeStore {
        TreeStore {
            trees: Mutex::default(),
            capacity: usize::MAX,
        }
    }
}

impl TreeStore {
    /// A store for a host that reads each file's tree in one pass per worker. It keeps every
    /// held tree and, up to `capacity` in total, the most recently read others, which serve
    /// the reads a pass makes of a file it does not hold without a parse for each.
    pub fn bounded(capacity: usize) -> TreeStore {
        TreeStore {
            trees: Mutex::default(),
            capacity,
        }
    }

    /// Keeps `file`'s tree until the hold is dropped.
    pub fn hold(&self, file: SourceFile) -> TreeHold<'_> {
        let mut trees = self.trees.lock().expect("tree store lock");
        *trees.holds.entry(file).or_default() += 1;
        TreeHold { store: self, file }
    }

    /// How many trees the store holds.
    pub fn stored(&self) -> usize {
        self.trees.lock().expect("tree store lock").entries.len()
    }

    /// How many texts the store has parsed, which pins that an edit parses one file.
    pub fn parses(&self) -> u64 {
        self.trees.lock().expect("tree store lock").parses
    }

    fn parse(&self, file: SourceFile, text: &str, revision: salsa::Revision) -> Parse {
        let previous = {
            let mut trees = self.trees.lock().expect("tree store lock");
            trees.clock += 1;
            let clock = trees.clock;
            match trees.entries.get_mut(&file) {
                Some(stored) if stored.confirmed == revision || stored.parse.text() == text => {
                    stored.confirmed = revision;
                    stored.last_read = clock;
                    return stored.parse.clone();
                }
                Some(stored) => Some(stored.parse.clone()),
                None => None,
            }
        };
        // Parse unlocked so parallel workers never serialize on the parse itself.
        let parse = match &previous {
            Some(old) => {
                let (deleted, inserted) = derived_edit(old.text(), text);
                syntax::reparse(old, text, deleted, inserted)
            }
            None => syntax::parse(text),
        };
        let mut trees = self.trees.lock().expect("tree store lock");
        trees.parses += 1;
        while trees.entries.len() >= self.capacity && !trees.entries.contains_key(&file) {
            let oldest = trees
                .entries
                .iter()
                .filter(|(stored_file, _)| !trees.holds.contains_key(stored_file))
                .min_by_key(|(_, stored)| stored.last_read)
                .map(|(&file, _)| file);
            let Some(oldest) = oldest else {
                break;
            };
            trees.entries.remove(&oldest);
        }
        let last_read = trees.clock;
        trees.entries.insert(
            file,
            StoredTree {
                parse: parse.clone(),
                confirmed: revision,
                last_read,
            },
        );
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

/// The item's subtree with offsets counted from its start, so what is derived from it is the
/// same wherever the item sits in its file. Item innards must be read through this (never
/// `parse`), and spans derived from it stay item-relative until the rendering edge.
pub fn item_syntax<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<ItemSyntax> {
    item_node(db, item).map(|node| ItemSyntax(Arc::new(node.subtree())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemSpan<'db> {
    pub item: Item<'db>,
    pub range: TextRange,
    /// The defined name's range, read with the item's identity from the same walk.
    pub name_range: Option<TextRange>,
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
        let (kind, name, name_range) = match top_level_definition(&node) {
            Some(definition) => (
                definition.kind,
                Some(definition.name),
                Some(definition.name_range),
            ),
            None => (ItemKind::Statement, None, None),
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
            name_range,
        });
    }
    spans
}

/// The sparse nodes the per-file passes look for, in preorder, from one walk of the tree:
/// each pass reading the tree itself would walk every node once per pass.
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
    /// A node carrying an inline annotation, as [`annotations::is_inline_site_ref`].
    InlineSite,
    /// An inline annotation itself, as [`syntax::is_inline_annotation_kind`].
    InlineAnnotation,
    /// The file or a brace body with a `#:` block among its statements.
    AnnotatedSequence,
    /// A call that attaches a namespace, as [`metadata::is_attach_call_ref`].
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
    let parse = parse(db, file);
    let root = parse.tree().root();
    let mut sites = Vec::new();
    // Almost no node is a site, so the walk visits only the kinds that can carry a role, and
    // a kind that needs a child to qualify only in a file that has such a child somewhere.
    // An untyped file then visits little more than its calls.
    let typed = root.any_kind(|kind| {
        matches!(
            kind,
            SyntaxKind::TYPE_ASCRIPTION | SyntaxKind::TYPE_BINDER_LIST | SyntaxKind::RETURN_TYPE
        )
    });
    let annotated = root.any_kind(|kind| kind == SyntaxKind::ANNOTATION);
    let candidate = |kind: SyntaxKind| match kind {
        SyntaxKind::BINARY_EXPR | SyntaxKind::FUNCTION_DEF => typed,
        SyntaxKind::BRACE_EXPR | SyntaxKind::SOURCE_FILE => annotated,
        SyntaxKind::TYPE_DEFINITION
        | SyntaxKind::NEW_EXPR
        | SyntaxKind::CALL_EXPR
        | SyntaxKind::ANNOTATION
        | SyntaxKind::TYPE_ASCRIPTION
        | SyntaxKind::RETURN_TYPE
        | SyntaxKind::NEW_TYPE
        | SyntaxKind::PURE_MARKER
        | SyntaxKind::TYPE_BINDER_LIST => true,
        _ => is_ry_syntax(kind),
    };
    for node in root.descendants_of_kind(candidate) {
        let kind = node.kind();
        let mut roles = 0;
        let site = match kind {
            SyntaxKind::TYPE_DEFINITION
            | SyntaxKind::NEW_EXPR
            | SyntaxKind::BINARY_EXPR
            | SyntaxKind::FUNCTION_DEF
                if annotations::is_inline_site_ref(node) =>
            {
                roles |= SiteRole::InlineSite.bit();
                true
            }
            SyntaxKind::CALL_EXPR if metadata::is_attach_call_ref(node) => {
                roles |= SiteRole::AttachCall.bit();
                true
            }
            SyntaxKind::ANNOTATION => true,
            _ => is_ry_syntax(kind),
        };
        if syntax::is_inline_annotation_kind(kind, node.parent().map(NodeRef::kind)) {
            roles |= SiteRole::InlineAnnotation.bit();
        }
        if (node == root || kind == SyntaxKind::BRACE_EXPR)
            && node.has_child(SyntaxKind::ANNOTATION)
        {
            roles |= SiteRole::AnnotatedSequence.bit();
        }
        if site || roles != 0 {
            sites.push(SyntaxSite {
                kind,
                range: node.range(),
                roles,
            });
        }
    }
    SyntaxSites(sites)
}

/// Every `#:` block of the file lowered once, in source order. The diagnostics, the type
/// declaration walks and the typing directive all read lowered blocks, and lowering is the
/// costly step, so they share this memo instead of each lowering the same block again.
#[salsa::tracked(returns(ref))]
pub fn file_comment_annotations<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<(u32, annotations::Annotation<'db>)> {
    let root = parse(db, file).syntax_node();
    file_syntax_sites(db, file)
        .nodes(&root, SiteRole::Annotation)
        .iter()
        .map(|node| {
            (
                u32::from(node.text_range().start()),
                annotations::lower_annotation(db, node),
            )
        })
        .collect()
}

/// The lowering of a `#:` block from the file's own tree, through [`file_comment_annotations`].
pub fn comment_annotation<'db>(
    db: &'db dyn Db,
    file: SourceFile,
    node: &SyntaxNode,
) -> Option<&'db annotations::Annotation<'db>> {
    let annotations = file_comment_annotations(db, file);
    let start = u32::from(node.text_range().start());
    let index = annotations
        .binary_search_by_key(&start, |(offset, _)| *offset)
        .ok()?;
    annotations.get(index).map(|(_, annotation)| annotation)
}

/// Whether a node of `kind` builds a ry value: a record, tuple, list, tag or `match`.
pub fn is_ry_syntax(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::RECORD_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::ARRAY_EXPR
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
pub struct ItemSyntax(pub Arc<Tree>);

impl ItemSyntax {
    /// Offsets start at 0 (item-relative).
    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.0.clone())
    }
}

/// Derived from the item's subtree only. An edit elsewhere in the file lowers the item again,
/// the module comes out equal, and everything downstream of it cuts off.
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
    let file = *item.file(db);
    let files = ProjectFiles::try_get(db);
    let is_member = |class: RefClassKey, member: Symbol| match files {
        _ if class == RefClassKey::R6 => ref_class_binds(&RefClassIndex::default(), class, member),
        Some(files) => *project_ref_class_binds(db, files, class, member),
        None => ref_class_binds(file_ref_classes(db, file), class, member),
    };
    Some(naming::resolve_item(module, masked_verbs, &is_member))
}

/// The Reference Classes a file defines, keyed by class name, with the members that
/// `Gen$methods(...)` and `Gen$fields(...)` add kept under the generator's name until a
/// `Gen <- setRefClass("Name", ...)` ties the two together.
#[derive(Debug, Default, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct RefClassIndex {
    classes: BTreeMap<Symbol, RefClassMembers>,
    generators: BTreeMap<Symbol, Symbol>,
    generator_members: BTreeMap<Symbol, BTreeSet<Symbol>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct RefClassMembers {
    members: BTreeSet<Symbol>,
    contains: Vec<Symbol>,
}

impl RefClassIndex {
    fn extend(&mut self, other: &RefClassIndex) {
        for (class, members) in &other.classes {
            let entry = self.classes.entry(*class).or_default();
            entry.members.extend(members.members.iter().copied());
            entry.contains.extend(members.contains.iter().copied());
        }
        self.generators.extend(
            other
                .generators
                .iter()
                .map(|(generator, class)| (*generator, *class)),
        );
        for (generator, members) in &other.generator_members {
            self.generator_members
                .entry(*generator)
                .or_default()
                .extend(members.iter().copied());
        }
    }
}

#[salsa::tracked(returns(ref))]
pub fn file_ref_classes(db: &dyn Db, file: SourceFile) -> RefClassIndex {
    let mut index = RefClassIndex::default();
    for span in item_spans(db, file) {
        let Some(module) = item_hir(db, span.item) else {
            continue;
        };
        for id in module.expression_ids() {
            if let ExpressionKind::Assign { target, value, .. } = module.kind(id)
                && let ExpressionKind::NameRef(generator) = module.kind(*target)
                && let Some(naming::RefClassDefinition {
                    key: RefClassKey::Class(class),
                    ..
                }) = naming::ref_class_definition(module, *value)
            {
                index.generators.insert(*generator, class);
            }
            let Some(definition) = naming::ref_class_definition(module, id) else {
                continue;
            };
            let members = definition
                .fields
                .iter()
                .copied()
                .chain(definition.methods.iter().map(|(name, _)| *name));
            match definition.key {
                RefClassKey::R6 => {}
                RefClassKey::Class(class) => {
                    let entry = index.classes.entry(class).or_default();
                    entry.members.extend(members);
                    entry.contains.extend(definition.contains);
                }
                RefClassKey::Generator(generator) => {
                    index
                        .generator_members
                        .entry(generator)
                        .or_default()
                        .extend(members);
                }
            }
        }
    }
    index
}

/// Unioned once per project; reads go through [`project_ref_class_binds`], so an edit to a
/// class re-checks only the items whose answers change.
#[salsa::tracked(returns(ref))]
fn project_ref_classes(db: &dyn Db, files: ProjectFiles) -> RefClassIndex {
    let mut index = RefClassIndex::default();
    for &file in files.files(db).iter() {
        index.extend(file_ref_classes(db, file));
    }
    index
}

#[salsa::tracked]
fn project_ref_class_binds(
    db: &dyn Db,
    files: ProjectFiles,
    class: RefClassKey,
    member: Symbol,
) -> bool {
    ref_class_binds(project_ref_classes(db, files), class, member)
}

/// Every Reference Class object inherits these from `envRefClass`, and `.self` is the object.
const REF_CLASS_BUILTINS: [&str; 14] = [
    ".self",
    "callSuper",
    "copy",
    "export",
    "field",
    "getClass",
    "getRefClass",
    "import",
    "initFields",
    "methods",
    "show",
    "trace",
    "untrace",
    "usingMethods",
];

/// A class whose generator or superclass this project does not define binds every name,
/// because its members are out of sight and reporting them would be guesswork.
fn ref_class_binds(index: &RefClassIndex, class: RefClassKey, member: Symbol) -> bool {
    let (start, generator) = match class {
        RefClassKey::R6 => return matches!(member.as_str(), "self" | "private" | "super"),
        _ if REF_CLASS_BUILTINS.contains(&member.as_str()) => return true,
        RefClassKey::Class(class) => (class, None),
        RefClassKey::Generator(generator) => match index.generators.get(&generator) {
            Some(&class) => (class, Some(generator)),
            None => return true,
        },
    };
    if generator
        .and_then(|generator| index.generator_members.get(&generator))
        .is_some_and(|members| members.contains(&member))
    {
        return true;
    }
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(class) = pending.pop() {
        if !visited.insert(class) {
            continue;
        }
        let Some(members) = index.classes.get(&class) else {
            if class != "envRefClass" {
                return true;
            }
            continue;
        };
        if members.members.contains(&member)
            || index
                .generators
                .iter()
                .filter(|(_, generated)| **generated == class)
                .any(|(generator, _)| {
                    index
                        .generator_members
                        .get(generator)
                        .is_some_and(|members| members.contains(&member))
                })
        {
            return true;
        }
        pending.extend(members.contains.iter().copied());
    }
    false
}

/// Recognized structurally by the checker rather than through the stub corpus.
const BUILTIN_GLOBAL_NAMES: &[&str] = &["c", "list", "switch", "return", "stop"];

/// Includes stub declarations and checker builtins, not just package definitions.
pub fn package_scheme_exists(db: &dyn Db, name: &str) -> bool {
    let files = ProjectFiles::try_get(db);
    package_scheme_exists_in(db, stubs::stubs(db), name, |name| {
        files.is_some_and(|files| package_binds(db, files, name))
    })
}

/// [`package_scheme_exists`] for a caller that already holds the stubs and answers whether the
/// package binds a name from maps it fetched once.
pub fn package_scheme_exists_in(
    db: &dyn Db,
    library: Option<&stubs::StubLibrary<'_>>,
    name: &str,
    package_binds: impl FnOnce(&str) -> bool,
) -> bool {
    if BUILTIN_GLOBAL_NAMES.contains(&name) || package_binds(name) {
        return true;
    }
    library.is_some_and(|library| {
        library.nominals.contains(name)
            || ((library.schemes.contains_key(name) || library.known_exports.contains(name))
                && !stubs::outside_prelude(db, library, name))
    })
}

/// Annotations are siblings of the item in the file tree, so they attach here, not in
/// `item_syntax`. A per-item firewall over one file-wide walk: untouched items'
/// annotation subtrees compare equal and cut off.
#[salsa::tracked(returns(clone))]
pub fn item_annotation_syntax<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<ItemSyntax> {
    file_item_annotations(db, *item.file(db))
        .get(&item)
        .cloned()
}

/// Matches targets to `item_spans` by range, so top-level classification lives in one place.
#[salsa::tracked(returns(ref))]
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
                .map(|&item| (item, ItemSyntax(Arc::new(annotation.detached_subtree())))),
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
/// lowering is done, nothing on the check path needs syntax.
#[salsa::tracked(returns(ref))]
pub fn item_annotations<'db>(db: &'db dyn Db, item: Item<'db>) -> ItemAnnotations<'db> {
    ItemAnnotations {
        root: root_annotation(db, item).map(Box::new),
        expressions: expression_annotations(db, item).into_boxed_slice(),
    }
}

fn root_annotation<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<annotations::Annotation<'db>> {
    let written = written_root_annotation(db, item);
    if written
        .as_ref()
        .is_some_and(|annotation| annotation.declared.is_some() || !annotation.errors.is_empty())
    {
        return written;
    }
    let Some(declared) = package_hook_signature(db, item) else {
        return written;
    };
    Some(annotations::Annotation {
        declared: Some(declared),
        ..written.unwrap_or_default()
    })
}

fn written_root_annotation<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
) -> Option<annotations::Annotation<'db>> {
    let block = item_annotation_syntax(db, item).map(|syntax| syntax.syntax_node());
    let comment = block
        .as_ref()
        .map(|block| annotations::lower_annotation(db, block));
    let root = item_syntax(db, item)?.syntax_node();
    let site = annotations::statement_inline_site(&root);
    let inline_enabled = site.is_some() && inline_annotations_enabled(db);
    let mut annotation = match site.filter(|_| inline_enabled) {
        Some(site) => {
            let inline = annotations::lower_inline_annotation(db, &site);
            annotations::merge_annotations(db, comment, Some(inline), Some(&site))
        }
        None => comment,
    }?;
    annotation.written_signature =
        annotations::has_written_signature(&root, block.as_ref(), inline_enabled);
    Some(annotation)
}

/// R calls a package file's `.onLoad` and `.onAttach` with two character arguments, and
/// `.onUnload`, `.onDetach` and `.Last.lib` with one, so an unannotated hook is typed as if it
/// declared that. R passes them by position, so the hook's own formal names are kept, `...`
/// takes whatever is left, and a formal past the passed ones is optional.
fn package_hook_signature<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<types::TypeScheme<'db>> {
    if *item.kind(db) != ItemKind::Function
        || item.parent(db).is_some()
        || *item.file(db).kind(db) != DocumentKind::Package
    {
        return None;
    }
    let passed = match item.name(db).as_deref()? {
        ".onLoad" | ".onAttach" => 2,
        ".onUnload" | ".onDetach" | ".Last.lib" => 1,
        _ => return None,
    };
    let root = item_syntax(db, item)?.syntax_node();
    let function = root
        .descendants()
        .find(|node| node.kind() == SyntaxKind::FUNCTION_DEF)?;
    let mut parameters = Vec::new();
    let mut takes_rest = false;
    for (position, formal) in formals(&function).iter().enumerate() {
        if formal.name == "..." {
            parameters.push("...: Any".to_owned());
            takes_rest = true;
            break;
        }
        let plain = formal
            .name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '.');
        if !plain {
            return None;
        }
        // `if (!missing(pkgname))` lets a developer call the hook by hand without arguments.
        let optional = formal.has_default || tests_missing(&function, &formal.name);
        parameters.push(if position < passed && !optional {
            format!("{}: character", formal.name)
        } else if position < passed {
            format!("[{}]: character", formal.name)
        } else {
            format!("[{}]: Any", formal.name)
        });
    }
    // A hook that cannot take the arguments R passes fails when R calls it, not when it is typed.
    if !takes_rest && parameters.len() < passed {
        return None;
    }
    stubs::lower_type_text(db, &format!("fn({}) -> Any", parameters.join(", ")))
}

/// Whether the function's text contains `missing(name)`.
fn tests_missing(function: &SyntaxNode, name: &str) -> bool {
    let tokens: Vec<_> = function
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .collect();
    tokens.windows(4).any(|window| {
        let [missing, open, argument, close] = window else {
            return false;
        };
        missing.text() == "missing"
            && open.text() == "("
            && argument.text().trim_matches('`') == name
            && close.text() == ")"
    })
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
    // Most items carry no annotation at all, and a scan of the kind column proves that far
    // faster than the walk below.
    if !root.node_ref().any_kind(annotations::may_mark_site) {
        return Vec::new();
    }
    // One walk for both kinds of site; the item tree is position-free, so the file's
    // recorded sites cannot stand in for it.
    let (comments, inline_sites): (Vec<SyntaxNode>, Vec<SyntaxNode>) =
        matching_nodes(&root, |node, _| {
            node.kind() == SyntaxKind::ANNOTATION || annotations::is_inline_site_ref(node)
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
        let Some(index) = module.ranges().position(|candidate| candidate == range) else {
            continue;
        };
        let written_signature =
            annotations::has_written_signature(&statement, comment.as_ref(), inline_enabled);
        let comment = comment.map(|comment| annotations::lower_annotation(db, &comment));
        let site = annotations::statement_inline_site(&statement).filter(|_| inline_enabled);
        let inline = site
            .as_ref()
            .map(|site| annotations::lower_inline_annotation(db, site));
        let Some(mut lowered) = annotations::merge_annotations(db, comment, inline, site.as_ref())
        else {
            continue;
        };
        lowered.written_signature = written_signature;
        if lowered.declared.is_none() && lowered.new_nominal.is_none() && !lowered.trusted {
            continue;
        }
        attachments.push((hir::ExprId::new(index), lowered));
    }
    attachments
}

/// Settings that change what the analysis means rather than which findings a host
/// publishes. Unset, every setting takes its default.
#[salsa::input(singleton, debug)]
pub struct AnalysisSettings {
    /// `[check] inline-annotations`. Off, inline annotations type nothing.
    pub inline_annotations: bool,
    /// `[check] strict`: whether a file without a `# typing:` directive is strict. Only a
    /// strict file's checks record the undetermined origins strict mode reports, which in
    /// untyped code are about one per expression.
    pub strict: bool,
    /// `[check] prelude`. Off, the standard library's names resolve only as `std::Name`.
    pub prelude: bool,
}

/// Whether the standard library's names are in scope bare. Unset settings keep the prelude.
pub fn prelude_enabled(db: &dyn Db) -> bool {
    AnalysisSettings::try_get(db).is_none_or(|settings| *settings.prelude(db))
}

/// Read only where an inline annotation exists, so a project without any never
/// depends on the setting.
pub fn inline_annotations_enabled(db: &dyn Db) -> bool {
    AnalysisSettings::try_get(db).is_none_or(|settings| *settings.inline_annotations(db))
}

/// A file's `# typing:` directive wins over `[check] strict`. Unset settings count as strict,
/// so a host that installs none still sees every origin [`diagnostics::strict_diagnostics`]
/// reports.
pub fn file_is_strict(db: &dyn Db, file: SourceFile) -> bool {
    match file_typing_mode(db, file) {
        Some(TypingMode::Strict) => true,
        Some(TypingMode::On | TypingMode::Off) => false,
        None => AnalysisSettings::try_get(db).is_none_or(|settings| *settings.strict(db)),
    }
}

/// Whether a literal bound outside any function in `file` is a ry value: the file is strict,
/// or written with ry syntax (an inline annotation, a record, tuple, tag, `List` or `match`).
#[salsa::tracked]
pub fn file_holds_ry_values(db: &dyn Db, file: SourceFile) -> bool {
    // Unlike `file_is_strict`, a host with no settings does not count as strict here, because
    // this changes types rather than only reporting origins.
    let strict = match file_typing_mode(db, file) {
        Some(mode) => *mode == TypingMode::Strict,
        None => AnalysisSettings::try_get(db).is_some_and(|settings| *settings.strict(db)),
    };
    if strict {
        return true;
    }
    let sites = file_syntax_sites(db, file);
    sites.any(SiteRole::RySyntax)
        || sites.any(SiteRole::InlineSite)
        || sites.any(SiteRole::InlineAnnotation)
        || session::holds_ry_values(db, file)
}

/// One rewrite the code ry's runtime runs needs, because R would read the text with a type other
/// than the one the checker gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elaboration {
    /// An unsuffixed number literal the checker typed as `I64`, which R reads as a `double`.
    I64Literal(TextRange),
    /// A list expression the runtime may build empty, whose item storage only the checker
    /// knows (`ItemCheck::typed_empties`), or a dictionary built from no entries.
    TypedEmpty(TextRange, Empty),
}

impl Elaboration {
    pub fn range(self) -> TextRange {
        match self {
            Elaboration::I64Literal(range) | Elaboration::TypedEmpty(range, _) => range,
        }
    }

    /// The same rewrite at `range`.
    pub fn at(self, range: TextRange) -> Elaboration {
        match self {
            Elaboration::I64Literal(_) => Elaboration::I64Literal(range),
            Elaboration::TypedEmpty(_, items) => Elaboration::TypedEmpty(range, items),
        }
    }
}

/// The rewrites ry's runtime needs in `file`, by start offset and, for nested lists, outermost
/// first.
pub fn file_elaborations(db: &dyn Db, file: SourceFile) -> Vec<Elaboration> {
    let mut elaborations = Vec::new();
    for span in item_spans(db, file) {
        let Some(check) = item_check(db, span.item) else {
            continue;
        };
        if check.i64_literals.is_empty() && check.typed_empties.is_empty() {
            continue;
        }
        let Some(module) = item_hir(db, span.item) else {
            continue;
        };
        let offset = span.range.start();
        elaborations.extend(
            check
                .i64_literals
                .iter()
                .map(|&id| Elaboration::I64Literal(module.range(id) + offset)),
        );
        elaborations.extend(
            check
                .typed_empties
                .iter()
                .map(|&(id, items)| Elaboration::TypedEmpty(module.range(id) + offset, items)),
        );
    }
    elaborations.sort_unstable_by_key(|elaboration| {
        let range = elaboration.range();
        (range.start(), std::cmp::Reverse(range.end()))
    });
    elaborations
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

/// Every project file by its path relative to the project root, with `/` separators. Hosts set
/// it so a literal `source("utils.R")` names the file it reads; without it nothing is followed.
#[salsa::input(singleton, debug)]
pub struct SourcePaths {
    #[returns(ref)]
    pub paths: BTreeMap<String, SourceFile>,
}

/// The literal paths a file passes to `source()` or `sys.source()`, in source order. A call
/// inside a function counts too, because `source()` evaluates into the global environment
/// unless told otherwise.
#[salsa::tracked(returns(ref))]
pub fn file_source_calls(db: &dyn Db, file: SourceFile) -> Vec<String> {
    let mut paths = Vec::new();
    for span in item_spans(db, file) {
        let Some(module) = item_hir(db, span.item) else {
            continue;
        };
        for id in module.expression_ids() {
            let ExpressionKind::Call { callee, arguments } = module.kind(id) else {
                continue;
            };
            let callee = match module.kind(*callee) {
                ExpressionKind::NameRef(name) => *name,
                ExpressionKind::Namespace {
                    package: Some(package),
                    name: Some(name),
                    ..
                } if package == "base" => *name,
                _ => continue,
            };
            if callee != "source" && callee != "sys.source" {
                continue;
            }
            let arguments = &module[arguments];
            let path = arguments
                .iter()
                .find(|argument| module.argument_name(argument).as_deref() == Some("file"))
                .or_else(|| arguments.iter().find(|argument| argument.name.is_none()))
                .and_then(|argument| argument.value);
            if let Some(path) = path
                && let ExpressionKind::Literal(hir::LiteralKind::String(text)) = module.kind(path)
            {
                paths.push(module[text].to_string());
            }
        }
    }
    paths
}

/// The project files a script or test reads through `source()` with a literal path,
/// transitively, in the order the calls appear. R resolves the path against the working
/// directory, which is usually the project root or the sourcing file's own directory, so both
/// are tried. A package file is skipped: the namespace already holds every `R/` file.
#[salsa::tracked(returns(ref))]
pub fn sourced_files(db: &dyn Db, file: SourceFile) -> Vec<SourceFile> {
    if *file.kind(db) == DocumentKind::Package || file_source_calls(db, file).is_empty() {
        return Vec::new();
    }
    let Some(paths) = SourcePaths::try_get(db) else {
        return Vec::new();
    };
    let paths = paths.paths(db);
    let path_of = |target: SourceFile| {
        paths
            .iter()
            .find(|(_, candidate)| **candidate == target)
            .map(|(path, _)| path.as_str())
    };
    let mut sourced = Vec::new();
    let mut pending = vec![file];
    let mut visited = FxHashSet::from_iter([file]);
    while let Some(current) = pending.pop() {
        let directory = path_of(current)
            .and_then(|path| path.rsplit_once('/'))
            .map(|(directory, _)| directory);
        for literal in file_source_calls(db, current) {
            let candidates = directory
                .map(|directory| normalize_relative_path(&format!("{directory}/{literal}")))
                .into_iter()
                .chain([normalize_relative_path(literal)]);
            let Some(target) = candidates
                .flatten()
                .find_map(|candidate| paths.get(&candidate).copied())
            else {
                continue;
            };
            if visited.insert(target) {
                sourced.push(target);
                pending.push(target);
            }
        }
    }
    sourced
}

/// `a/./b/../c.R` as `a/c.R`, or `None` for a path that leaves the project root.
fn normalize_relative_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The item a sourced file binds `name` with; the last sourced file to bind it wins, as at run
/// time.
pub fn sourced_definition<'db>(
    db: &'db dyn Db,
    file: SourceFile,
    name: Symbol,
) -> Option<Item<'db>> {
    // Most files source nothing, and interning a per-name query for every name such a file
    // reads costs a check more time and memory than the answer is worth.
    if sourced_files(db, file).is_empty() {
        return None;
    }
    *sourced_name_definition(db, file, name)
}

/// Per name, so a sourced file's edit re-checks only the reads whose definition changes.
#[salsa::tracked]
fn sourced_name_definition<'db>(
    db: &'db dyn Db,
    file: SourceFile,
    name: Symbol,
) -> Option<Item<'db>> {
    sourced_files(db, file).iter().rev().find_map(|&sourced| {
        file_definitions(db, sourced)
            .iter()
            .rev()
            .find(|(defined, _)| *defined == name)
            .map(|(_, item)| *item)
    })
}

/// Whether some project file sources `file`, whose bindings are then read elsewhere.
#[salsa::tracked]
pub fn file_is_sourced(db: &dyn Db, files: ProjectFiles, file: SourceFile) -> bool {
    files
        .files(db)
        .iter()
        .any(|&other| sourced_files(db, other).contains(&file))
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
    file_typing_directives(db, file)
        .0
        .or_else(|| session::typing_mode(db, file))
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
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMENT => {
                let Some(rest) = token
                    .text()
                    .trim_start_matches('#')
                    .trim()
                    .strip_prefix("typing:")
                else {
                    continue;
                };
                // The whole remainder up to a trailing `# reason` comment is the value:
                // `typing: on gely` is a typo, not `on`.
                let value = rest.split_once('#').map_or(rest, |(value, _)| value);
                match value.trim() {
                    "off" => mode = Some(TypingMode::Off),
                    "on" => mode = Some(TypingMode::On),
                    "strict" => mode = Some(TypingMode::Strict),
                    other => invalid.push((token.text_range(), other.to_owned())),
                }
            }
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::ANNOTATION => {
                if let Some(strict) =
                    comment_annotation(db, file, &node).and_then(|annotation| annotation.strict)
                {
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

#[salsa::tracked(returns(ref))]
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
                if let Some(annotation) = comment_annotation(db, file, &node) {
                    definitions.extend(annotation.definitions.iter().cloned());
                }
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
            definitions.insert(definition.name, definition.clone());
        }
    }
    definitions
}

/// Whether any package file binds `name` at top level, including the inner targets of
/// `a <- b <- value` and conditional writes.
pub fn package_binds(db: &dyn Db, files: ProjectFiles, name: &str) -> bool {
    let name = Symbol::new(name);
    package_definitions(db, files).contains_key(&name)
        || conditional_slot_items(db, files).contains_key(&name)
}

/// Every name a project file of any kind declares at top level: its named definitions and its
/// other top-level writes. Editor requests consult it before treating a name as a stub's, and it
/// folds the per-file memos the cold check already derives, so a lookup is one hash probe.
#[salsa::tracked(returns(ref))]
pub fn project_global_names(db: &dyn Db, files: ProjectFiles) -> FxHashSet<Symbol> {
    let mut names = FxHashSet::default();
    for &file in files.files(db) {
        names.extend(file_definitions(db, file).iter().map(|&(name, _)| name));
        names.extend(
            file_conditional_writers(db, file)
                .iter()
                .map(|&(name, _)| name),
        );
    }
    names
}

/// Items writing a top-level name other than their own (conditionally, or nested as in
/// `y <- (x <- 1L)`); cross-item reads with no unconditional winner join their types.
#[salsa::tracked(returns(ref))]
pub fn conditional_slot_items<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> FxHashMap<Symbol, Vec<Item<'db>>> {
    conditional_writers_of(db, files, DocumentKind::Package)
}

/// The helper files' counterpart of [`conditional_slot_items`].
#[salsa::tracked(returns(ref))]
pub fn test_conditional_slot_items<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> FxHashMap<Symbol, Vec<Item<'db>>> {
    conditional_writers_of(db, files, DocumentKind::TestHelper)
}

fn conditional_writers_of<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    kind: DocumentKind,
) -> FxHashMap<Symbol, Vec<Item<'db>>> {
    let mut writers: FxHashMap<Symbol, Vec<Item<'db>>> = FxHashMap::default();
    for &file in files.files(db) {
        if *file.kind(db) != kind {
            continue;
        }
        for &(name, item) in file_conditional_writers(db, file) {
            writers.entry(name).or_default().push(item);
        }
    }
    writers
}

/// The file's share of [`conditional_slot_items`]. The project-wide walks fold these per-file
/// memos rather than reading per-item ones: after an edit, salsa then verifies one memo per
/// untouched file by durability instead of one edge per item in the project.
///
/// Every package file is asked on the first check, so the items that cannot write a name other
/// than their own are screened on the file's tree here instead of each through per-item
/// queries. Reading the tree re-runs this on any edit to the file, which costs one walk of it,
/// and the value still backdates when the writers stay the same.
#[salsa::tracked(returns(ref))]
pub fn file_conditional_writers<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<(Symbol, Item<'db>)> {
    let root = parse(db, file).syntax_node();
    let mut writers = Vec::new();
    for span in item_spans(db, file) {
        let node = root
            .child_or_token_at_range(span.range)
            .and_then(|element| element.into_node())
            .filter(|node| node.text_range() == span.range);
        if node.is_some_and(|node| binds_only_own_name(db, span.item, &node)) {
            continue;
        }
        writers.extend(
            item_side_names(db, span.item)
                .into_iter()
                .map(|name| (name, span.item)),
        );
    }
    writers
}

/// The top-level names an item binds besides its own, such as the bindings a branch or a loop
/// leaves behind.
fn item_side_names<'db>(db: &'db dyn Db, item: Item<'db>) -> Vec<Symbol> {
    let own_name = item.name(db);
    item_top_level_names(db, item)
        .iter()
        .copied()
        .filter(|name| own_name.as_deref() != Some(name.as_str()))
        .collect()
}

/// A per-name projection of the project-wide maps, so a check depends only on the names
/// it reads. `conditional_writers` serve the name when there is no winner.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct PackageName<'db> {
    pub winner: Option<Item<'db>>,
    pub conditional_writers: Vec<Item<'db>>,
}

#[salsa::tracked(returns(ref))]
pub fn package_name<'db>(db: &'db dyn Db, name: types::QueryName<'db>) -> PackageName<'db> {
    let Some(files) = ProjectFiles::try_get(db) else {
        return PackageName::default();
    };
    let text = name.name(db).symbol();
    PackageName {
        winner: package_definitions(db, files).get(&text).copied(),
        conditional_writers: conditional_slot_items(db, files)
            .get(&text)
            .cloned()
            .unwrap_or_default(),
    }
}

/// The project's files, one bit per file id. It reads the file list alone, so a reader does not
/// depend on what any file binds.
#[salsa::tracked(returns(ref))]
fn project_file_set(db: &dyn Db, files: ProjectFiles) -> Box<[u64]> {
    let mut members: Vec<u64> = Vec::new();
    for &file in files.files(db) {
        set_bit(&mut members, file.as_id().index() as usize);
    }
    members.into_boxed_slice()
}

fn bit(words: &[u64], index: usize) -> bool {
    words
        .get(index / 64)
        .is_some_and(|word| word & (1 << (index % 64)) != 0)
}

fn set_bit(words: &mut Vec<u64>, index: usize) {
    if index / 64 >= words.len() {
        words.resize(index / 64 + 1, 0);
    }
    words[index / 64] |= 1 << (index % 64);
}

/// One namespace layer's binding of a name, borrowed from wherever it was looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NameLayer<'a, 'db> {
    pub winner: Option<Item<'db>>,
    pub conditional_writers: &'a [Item<'db>],
}

impl NameLayer<'_, '_> {
    pub fn binds(self) -> bool {
        self.winner.is_some() || !self.conditional_writers.is_empty()
    }
}

/// The package namespace's binding of `name`, or with `test` set, the helper files', through
/// the name's own projection, so a reader depends on that one name. Nothing binds a name when
/// there is no project. A check reads the names its item spells from [`item_links`] instead.
pub fn name_layer<'db>(db: &'db dyn Db, name: Symbol, test: bool) -> NameLayer<'db, 'db> {
    let name = types::QueryName::new(db, types::Name::new(db, name));
    let projection = match test {
        true => test_name(db, name),
        false => package_name(db, name),
    };
    NameLayer {
        winner: projection.winner,
        conditional_writers: &projection.conditional_writers,
    }
}

/// The test files' counterpart of [`package_name`].
#[salsa::tracked(returns(ref))]
pub fn test_name<'db>(db: &'db dyn Db, name: types::QueryName<'db>) -> PackageName<'db> {
    let Some(files) = ProjectFiles::try_get(db) else {
        return PackageName::default();
    };
    let text = name.name(db).symbol();
    PackageName {
        winner: test_definitions(db, files).get(&text).copied(),
        conditional_writers: test_conditional_slot_items(db, files)
            .get(&text)
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

/// The last top-level definition of each name across the package files.
pub fn package_definitions<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
) -> &'db FxHashMap<Symbol, Item<'db>> {
    &package_definition_index(db, files).winners
}

/// The package files' definitions, folded once: a new top-level name re-runs this fold on
/// every keystroke until the name is complete, so the duplicate check shares its pass.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct DefinitionIndex<'db> {
    pub winners: FxHashMap<Symbol, Item<'db>>,
    /// Every file that defines a name the package defines more than once, one entry per
    /// definition, in project order. Near-empty in a healthy project.
    pub duplicates: FxHashMap<Symbol, Vec<SourceFile>>,
}

#[salsa::tracked(returns(ref))]
pub fn package_definition_index<'db>(db: &'db dyn Db, files: ProjectFiles) -> DefinitionIndex<'db> {
    winners_of(db, files, DocumentKind::Package)
}

/// The last top-level definition of each name across the helper files, which testthat
/// sources into the one environment every test file runs in.
#[salsa::tracked(returns(ref))]
pub fn test_definitions<'db>(db: &'db dyn Db, files: ProjectFiles) -> FxHashMap<Symbol, Item<'db>> {
    winners_of(db, files, DocumentKind::TestHelper).winners
}

fn winners_of<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    kind: DocumentKind,
) -> DefinitionIndex<'db> {
    let mut index = DefinitionIndex::default();
    for &file in files.files(db) {
        if *file.kind(db) != kind {
            continue;
        }
        let definitions = file_definitions(db, file);
        index.winners.reserve(definitions.len());
        for &(name, item) in definitions {
            if let Some(previous) = index.winners.insert(name, item) {
                index
                    .duplicates
                    .entry(name)
                    .or_insert_with(|| vec![*previous.file(db)])
                    .push(file);
            }
        }
    }
    index
}

/// The file's named function and value items in item order, the candidates for
/// [`package_definitions`]. Per file for the same reason as [`file_conditional_writers`].
/// Names are symbols so the project-wide folds hash and copy four bytes per name, which is
/// what a new top-level binding costs on every keystroke until it is named.
#[salsa::tracked(returns(ref))]
pub fn file_definitions<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<(Symbol, Item<'db>)> {
    item_tree(db, file)
        .iter()
        .filter(|item| matches!(*item.kind(db), ItemKind::Function | ItemKind::Value))
        .filter_map(|&item| Some((Symbol::new(item.name(db).as_deref()?), item)))
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

/// The `UseMethod` argument is ignored: a mismatch is a runtime error R reports itself.
#[salsa::tracked(returns(ref))]
pub(crate) fn file_s3_generics(db: &dyn Db, file: SourceFile) -> FxHashSet<String> {
    let mut generics = FxHashSet::default();
    for &item in item_tree(db, file) {
        if let Some(name) = item.name(db).clone()
            && *reads_use_method(db, item)
        {
            generics.insert(name);
        }
    }
    generics
}

/// A firewall, so a body edit does not re-derive the file's generics.
#[salsa::tracked]
fn reads_use_method<'db>(db: &'db dyn Db, item: Item<'db>) -> bool {
    item_naming(db, item)
        .as_ref()
        .is_some_and(|naming| naming.non_locals.values().any(|name| *name == "UseMethod"))
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

/// Where a read past the item's own scope takes its scheme from, nearest first.
#[derive(Clone, Copy)]
enum Source<'a, 'db> {
    /// A definition of the name: its scheme always answers, which ends the search.
    Definition(Item<'db>),
    /// A statement that binds the name on the side, which answers only when its check
    /// writes the name.
    Statement(Item<'db>),
    /// The writers of a name no definition binds, which answer when any of them writes it.
    Conditional(&'a [Item<'db>]),
}

fn binder_source<'a, 'db>(db: &'db dyn Db, binder: Item<'db>, name: &str) -> Source<'a, 'db> {
    match binder.name(db).as_deref() == Some(name) {
        true => Source::Definition(binder),
        false => Source::Statement(binder),
    }
}

fn layer_source<'a, 'db>(
    winner: Option<Item<'db>>,
    conditional_writers: &'a [Item<'db>],
) -> Option<Source<'a, 'db>> {
    match winner {
        Some(winner) => Some(Source::Definition(winner)),
        None => {
            (!conditional_writers.is_empty()).then_some(Source::Conditional(conditional_writers))
        }
    }
}

#[salsa::tracked(returns(ref))]
pub fn item_top_level_names<'db>(db: &'db dyn Db, item: Item<'db>) -> SortedSet<Symbol> {
    if item_binds_only_own_name(db, item) {
        return item
            .name(db)
            .as_deref()
            .map(Symbol::new)
            .into_iter()
            .collect();
    }
    let Some(naming) = item_naming(db, item) else {
        return SortedSet::default();
    };
    naming
        .bindings
        .iter()
        .filter(|(slot, binding)| {
            binding.kind == naming::BindingKind::TopLevel
                || naming.extras().loop_bound_slots.contains(*slot)
        })
        .map(|(_, binding)| binding.name)
        .collect()
}

/// The items an item's check can read exports from: the binders in its own file, and for the
/// names it reads past its file, the sourced definition and the helper and package layers in
/// the order `SalsaGlobals` resolves them. Every read `SalsaGlobals` can make for a name the
/// item spells is here, so a cycle through them never closes inside salsa. They are part of the
/// item's [`item_links`], which a body edit that only shifts ranges backdates, so the walks
/// that find cyclic groups do not re-run for it.
pub fn item_interface_edges<'db>(db: &'db dyn Db, item: Item<'db>) -> &'db [Item<'db>] {
    &item_links(db, item).edges
}

fn interface_edges<'db>(
    db: &'db dyn Db,
    item: Item<'db>,
    naming: &naming::ItemNaming,
    links: &ItemLinks<'db>,
) -> ThinVec<Item<'db>> {
    let package = *item.file(db).kind(db) == DocumentKind::Package;
    // How the item reads each listed name, as bits at the name's position in its links: through
    // its file's frame first (immediately or deferred), straight from the layers
    // (`enclosing_scheme`, and `namespace_scheme` outside a package file), or as a side binding.
    const IMMEDIATE: u8 = 1;
    const DEFERRED: u8 = 2;
    const LAYERS: u8 = 4;
    const SIDE: u8 = 8;
    let through_frame = |deferred: bool| if deferred { DEFERRED } else { IMMEDIATE };
    let past_file = if package { 0 } else { LAYERS };
    let mut reads = vec![0_u8; links.names.len()];
    let mut mark = |name: Symbol, how: u8| {
        if let Some(read) = links
            .position(name)
            .and_then(|position| reads.get_mut(position))
        {
            *read |= how;
        }
    };
    for (id, &name) in naming.non_locals.iter() {
        mark(
            name,
            through_frame(naming.deferred_non_locals.contains(id)) | past_file,
        );
    }
    for (id, &name) in naming.quiet_reads.iter() {
        mark(
            name,
            through_frame(naming.extras().deferred_quiet_reads.contains(id)),
        );
    }
    for name in naming.namespace_reads.values().filter_map(|read| read.name) {
        mark(name, if package { IMMEDIATE } else { LAYERS });
    }
    let top_level = |slot: &naming::BindingId| {
        naming
            .bindings
            .get(slot)
            .is_some_and(|binding| binding.kind == naming::BindingKind::TopLevel)
    };
    for (callee, fallback) in naming.callee_fallbacks.iter() {
        let Some(slot) = naming.resolutions.get(callee) else {
            continue;
        };
        let Some(binding) = naming.bindings.get(slot) else {
            continue;
        };
        match top_level(slot) || fallback.outer.iter().any(top_level) {
            true => mark(binding.name, past_file),
            false => mark(binding.name, through_frame(fallback.deferred)),
        }
    }
    // A top-level slot read before the item writes it reads the binding that stood before
    // the item.
    for &name in naming.used_top_level_names.iter() {
        mark(name, IMMEDIATE);
    }
    for name in item_side_names(db, item) {
        mark(name, SIDE);
    }
    let mut targets: Vec<Item<'db>> = Vec::new();
    for (position, (&name, &how)) in links.names.iter().zip(&reads).enumerate() {
        if how == 0 {
            continue;
        }
        let mut layered = how & LAYERS != 0;
        let mut earlier = false;
        // A side binding a branch or loop may leave unwritten reads the earlier binding.
        if how & SIDE != 0 {
            match links.bindings(position, false).frame {
                Some(binder) => push_unique(&mut targets, binder),
                None => earlier = true,
            }
        }
        for deferred in [false, true] {
            if how & through_frame(deferred) == 0 {
                continue;
            }
            let binder = links.bindings(position, deferred).frame;
            match binder.map(|binder| binder_source(db, binder, name.as_str())) {
                Some(Source::Definition(binder)) => push_unique(&mut targets, binder),
                Some(Source::Statement(binder)) => {
                    push_unique(&mut targets, binder);
                    layered = true;
                }
                Some(Source::Conditional(_)) | None => layered = true,
            }
        }
        let bindings = links.bindings(position, false);
        if layered {
            for layer in bindings.layers() {
                match layer_source(layer.winner, layer.conditional_writers) {
                    Some(Source::Definition(item) | Source::Statement(item)) => {
                        push_unique(&mut targets, item);
                        break;
                    }
                    Some(Source::Conditional(writers))
                        if writers.len() > CONDITIONAL_SLOT_JOIN_CAP =>
                    {
                        break;
                    }
                    Some(Source::Conditional(writers)) => {
                        for &writer in writers {
                            push_unique(&mut targets, writer);
                        }
                    }
                    None => {}
                }
            }
        }
        if earlier && let Some(winner) = bindings.layers().find_map(|layer| layer.winner) {
            push_unique(&mut targets, winner);
        }
    }
    ThinVec::from(targets)
}

/// Where the project binds each name an item's naming lists, outside the item's own scopes:
/// the binders in the item's file, a sourced file's definition, and the helper and package
/// namespaces. One query per item resolves them all, so a check reads its globals from one
/// array. It is the item's projection of the project-wide name maps: a new top-level name
/// anywhere re-runs it, and it backdates unless one of the item's own names moved, so the
/// item's check and edges depend on exactly the names the item reads.
///
/// The layout is one row per name and one per binding, in parallel arrays. Every item keeps
/// its links for the whole run, most names have one binding or none, and a row of five
/// optional items per name would cost four times the memory.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct ItemLinks<'db> {
    /// The listed names, sorted by symbol index.
    names: Box<[Symbol]>,
    /// Per name, where its run of bindings ends.
    ends: Box<[u32]>,
    /// Per binding, what it is to the reader. A name's run is sorted by kind, so the
    /// conditional writers of one layer are adjacent.
    kinds: Box<[LinkKind]>,
    items: Box<[Item<'db>]>,
    /// [`item_interface_edges`].
    edges: ThinVec<Item<'db>>,
    /// Whether the item's file is one of the project's, so that the package layer holds every
    /// top-level name a package file binds.
    file_in_project: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
enum LinkKind {
    /// The binder in the item's file an immediate read sees ([`FrameBinders::binder`]).
    Immediate,
    /// The binder in the item's file a deferred read sees.
    Deferred,
    /// The definition in a file the item's file sources.
    Sourced,
    TestWinner,
    TestWriter,
    PackageWinner,
    PackageWriter,
}

/// What a read of one name sees outside the reading item, nearest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct NameBindings<'a, 'db> {
    /// The binder in the reader's own file.
    frame: Option<Item<'db>>,
    sourced: Option<Item<'db>>,
    test: NameLayer<'a, 'db>,
    package: NameLayer<'a, 'db>,
}

#[salsa::tracked(returns(ref))]
fn item_links<'db>(db: &'db dyn Db, item: Item<'db>) -> ItemLinks<'db> {
    let Some(naming) = item_naming(db, item) else {
        return ItemLinks::default();
    };
    let mut names: Vec<Symbol> = naming
        .non_locals
        .values()
        .chain(naming.quiet_reads.values())
        .chain(naming.extras().quiet_operator_reads.iter())
        .chain(naming.used_top_level_names.iter())
        .copied()
        .chain(naming.namespace_reads.values().filter_map(|read| read.name))
        .chain(naming.callee_fallbacks.iter().filter_map(|(callee, _)| {
            let slot = naming.resolutions.get(callee)?;
            Some(naming.bindings.get(slot)?.name)
        }))
        .chain(item_side_names(db, item))
        .collect();
    names.sort_unstable_by_key(|name| name.index());
    names.dedup();
    let file = *item.file(db);
    let kind = *file.kind(db);
    let frame = FrameBinders::of(db, item);
    let sources_files = !sourced_files(db, file).is_empty();
    let project = ProjectFiles::try_get(db);
    let test = project
        .filter(|_| matches!(kind, DocumentKind::Test | DocumentKind::TestHelper))
        .map(|files| {
            (
                test_definitions(db, files),
                test_conditional_slot_items(db, files),
            )
        });
    let package = project.map(|files| {
        (
            package_definitions(db, files),
            conditional_slot_items(db, files),
        )
    });
    let mut ends: Vec<u32> = Vec::with_capacity(names.len());
    let mut kinds: Vec<LinkKind> = Vec::new();
    let mut items: Vec<Item<'db>> = Vec::new();
    for &name in &names {
        let mut link = |kind: LinkKind, item: Item<'db>| {
            kinds.push(kind);
            items.push(item);
        };
        if let Some(binder) = frame.binder(name, false) {
            link(LinkKind::Immediate, binder);
        }
        // A deferred read in a package file sees the project-wide winner, never the file's.
        if kind != DocumentKind::Package
            && let Some(binder) = frame.binder(name, true)
        {
            link(LinkKind::Deferred, binder);
        }
        if sources_files && let Some(definition) = *sourced_name_definition(db, file, name) {
            link(LinkKind::Sourced, definition);
        }
        for (layer, winner_kind, writer_kind) in [
            (test, LinkKind::TestWinner, LinkKind::TestWriter),
            (package, LinkKind::PackageWinner, LinkKind::PackageWriter),
        ] {
            let Some((winners, writers)) = layer else {
                continue;
            };
            // The writers serve a name only when no definition wins it, and past the join cap
            // only their count is read, so a run keeps at most one more than the cap.
            match winners.get(&name) {
                Some(&winner) => link(winner_kind, winner),
                None => {
                    for &writer in writers
                        .get(&name)
                        .into_iter()
                        .flatten()
                        .take(CONDITIONAL_SLOT_JOIN_CAP + 1)
                    {
                        link(writer_kind, writer);
                    }
                }
            }
        }
        let Ok(end) = u32::try_from(items.len()) else {
            panic!("item_links: an item links more than u32::MAX bindings");
        };
        ends.push(end);
    }
    let mut links = ItemLinks {
        names: names.into_boxed_slice(),
        ends: ends.into_boxed_slice(),
        kinds: kinds.into_boxed_slice(),
        items: items.into_boxed_slice(),
        edges: ThinVec::new(),
        file_in_project: project
            .is_some_and(|files| bit(project_file_set(db, files), file.as_id().index() as usize)),
    };
    links.edges = interface_edges(db, item, naming, &links);
    links
}

impl<'db> ItemLinks<'db> {
    fn position(&self, name: Symbol) -> Option<usize> {
        self.names
            .binary_search_by_key(&name.index(), |listed| listed.index())
            .ok()
    }

    /// What a read of the name at `position` sees, immediately or deferred.
    fn bindings(&self, position: usize, deferred: bool) -> NameBindings<'_, 'db> {
        let start = position
            .checked_sub(1)
            .and_then(|previous| self.ends.get(previous))
            .map_or(0, |&end| end as usize);
        let end = self.ends.get(position).map_or(start, |&end| end as usize);
        let kinds = self.kinds.get(start..end).unwrap_or_default();
        let items = self.items.get(start..end).unwrap_or_default();
        let writers = |kind: LinkKind| {
            let first = kinds.partition_point(|&other| (other as u8) < kind as u8);
            let count = kinds[first..].partition_point(|&other| other == kind);
            items.get(first..first + count).unwrap_or_default()
        };
        let one = |kind: LinkKind| writers(kind).first().copied();
        NameBindings {
            frame: one(match deferred {
                true => LinkKind::Deferred,
                false => LinkKind::Immediate,
            }),
            sourced: one(LinkKind::Sourced),
            test: NameLayer {
                winner: one(LinkKind::TestWinner),
                conditional_writers: writers(LinkKind::TestWriter),
            },
            package: NameLayer {
                winner: one(LinkKind::PackageWinner),
                conditional_writers: writers(LinkKind::PackageWriter),
            },
        }
    }
}

impl<'a, 'db> NameBindings<'a, 'db> {
    /// The namespaces a read searches past the reader's own file, nearest first: a sourced
    /// file's definition, then the helpers' bindings, which a test or helper file sees before
    /// the package namespace.
    fn layers(self) -> impl Iterator<Item = NameLayer<'a, 'db>> {
        let sourced = NameLayer {
            winner: self.sourced,
            conditional_writers: &[],
        };
        [sourced, self.test, self.package].into_iter()
    }

    /// Whether every read resolves the same through both: a layer's writers count only when
    /// it has no winner, and past the join cap only as that many.
    fn reads_like(self, other: NameBindings<'_, 'db>) -> bool {
        fn source<'a, 'db>(layer: NameLayer<'a, 'db>) -> (Option<Item<'db>>, &'a [Item<'db>]) {
            match layer.winner {
                Some(winner) => (Some(winner), &[]),
                None => {
                    let writers = layer.conditional_writers;
                    let kept = writers.len().min(CONDITIONAL_SLOT_JOIN_CAP + 1);
                    (None, &writers[..kept])
                }
            }
        }
        self.frame == other.frame && self.layers().map(source).eq(other.layers().map(source))
    }
}

/// Facts read off an item's own syntax that passes need after a diagnostics-only host has
/// dropped the file's tree. They share one memo because each is a `bool` that rarely changes, and a
/// memo per fact costs more to keep than the fact costs to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct ItemSyntaxFlags {
    /// The item's top-level writes are at most its own binding, so the common case needs no
    /// naming pass.
    pub binds_only_own_name: bool,
    /// Its `#:` block carries `@pure`.
    pub comment_pure: bool,
    /// It binds a function marked `@pure function`, which counts only while inline
    /// annotations are enabled.
    pub inline_pure: bool,
}

#[salsa::tracked(returns(copy))]
pub fn item_syntax_flags<'db>(db: &'db dyn Db, item: Item<'db>) -> ItemSyntaxFlags {
    let comment_pure = item_annotation_syntax(db, item)
        .is_some_and(|syntax| annotations::declares_pure(&syntax.syntax_node()));
    let Some(syntax) = item_syntax(db, item) else {
        return ItemSyntaxFlags {
            comment_pure,
            ..ItemSyntaxFlags::default()
        };
    };
    let root = syntax.syntax_node();
    let inline_pure = root.kind() == SyntaxKind::BINARY_EXPR
        && root.children().any(|child| {
            child.kind() == SyntaxKind::FUNCTION_DEF
                && child
                    .children()
                    .any(|part| part.kind() == SyntaxKind::PURE_MARKER)
        });
    ItemSyntaxFlags {
        binds_only_own_name: binds_only_own_name(db, item, &root),
        comment_pure,
        inline_pure,
    }
}

pub fn item_binds_only_own_name<'db>(db: &'db dyn Db, item: Item<'db>) -> bool {
    item_syntax_flags(db, item).binds_only_own_name
}

fn binds_only_own_name(db: &dyn Db, item: Item<'_>, node: &SyntaxNode) -> bool {
    // `name <- function(...)` binds only its own name at top level, since the body is a scope
    // of its own. Most items are functions, and skipping them keeps whole-project walks, such
    // as the loader's strict-mode classification, from naming everything.
    if *item.kind(db) == ItemKind::Function && node.kind() == SyntaxKind::BINARY_EXPR {
        return true;
    }
    // Only a write evaluated in the top-level frame binds a top-level name, so an item whose
    // text holds no write besides its own binds its own name alone. Most value and statement
    // items are of this shape (`x <- list(...)`, `library(pkg)`). A named item binds its name
    // with a token only when it is an assignment: `setGeneric("f", ...)` writes none.
    let own_write = item.name(db).is_some() && node.kind() == SyntaxKind::BINARY_EXPR;
    write_tokens(node) <= usize::from(own_write)
}

/// The tokens outside nested functions that can start a write in naming: an assignment
/// operator, `for`, a destructuring `%<-%` or `%->%`, and the by-reference `:=` called by
/// name, `setnames` and `setcolorder` (backticked or not). It may overcount, which only costs a naming pass, but a write naming records
/// that this misses would drop a top-level binding.
fn write_tokens(node: &SyntaxNode) -> usize {
    node.node_ref()
        .tokens_outside(
            |kind| kind == SyntaxKind::FUNCTION_DEF,
            |kind| {
                matches!(
                    kind,
                    SyntaxKind::LESS_MINUS
                        | SyntaxKind::LESS2_MINUS
                        | SyntaxKind::MINUS_GREATER
                        | SyntaxKind::MINUS_GREATER2
                        | SyntaxKind::COLON_EQ
                        | SyntaxKind::FOR_KW
                        | SyntaxKind::EQ
                        | SyntaxKind::SPECIAL
                        | SyntaxKind::IDENT
                )
            },
        )
        .filter(|token| match token.kind() {
            SyntaxKind::EQ => token.parent().map(NodeRef::kind) == Some(SyntaxKind::BINARY_EXPR),
            SyntaxKind::SPECIAL => matches!(token.text(), "%<-%" | "%->%"),
            SyntaxKind::IDENT => matches!(
                token.text().trim_matches('`'),
                ":=" | "setnames" | "setcolorder"
            ),
            _ => true,
        })
        .count()
}

/// Every other file whose items a check of `file` can read exports from, transitively through
/// the interface edges, in no particular order. A file's checked findings can change only
/// when its own text, one of these files, or the project's top-level declarations change.
pub fn file_dependency_files(db: &dyn Db, file: SourceFile) -> Vec<SourceFile> {
    let mut seen: FxHashSet<Item<'_>> = FxHashSet::default();
    let mut stack: Vec<Item<'_>> = item_tree(db, file).clone();
    let mut files: FxHashSet<SourceFile> = FxHashSet::default();
    while let Some(item) = stack.pop() {
        if !seen.insert(item) {
            continue;
        }
        let owner = *item.file(db);
        if owner != file {
            files.insert(owner);
        }
        stack.extend(item_interface_edges(db, item).iter().copied());
    }
    files.into_iter().collect()
}

/// The cyclic group an item belongs to, members in canonical order (project file order, then
/// item order), or `None` when no read cycle runs through the item. A group must resolve
/// through one canonical fixpoint, since iterating from whichever member was queried first
/// would make the round-cap pins depend on query order.
pub fn item_group<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    item: Item<'db>,
) -> Option<&'db [Item<'db>]> {
    let group = file_group(db, files, item);
    if syntax::testing::invariants_enabled() {
        assert_eq!(
            group.map(|members| members.iter().copied().collect::<FxHashSet<_>>()),
            item_group_from_scratch(db, item),
            "the file's cyclic groups disagree with the item's own reachability"
        );
    }
    group
}

fn file_group<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    item: Item<'db>,
) -> Option<&'db [Item<'db>]> {
    file_interface_groups(db, files, *item.file(db))
        .members
        .iter()
        .find(|members| members.contains(&item))
        .map(|members| &**members)
}

/// The members of the item's cyclic group from reachability alone: every item the item reaches
/// that reaches it back. It shares only the edges with [`file_interface_groups`], so it checks
/// the component walk as well as the per-file bookkeeping.
fn item_group_from_scratch<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<FxHashSet<Item<'db>>> {
    let mut reached: FxHashSet<Item<'db>> = FxHashSet::default();
    let mut readers: FxHashMap<Item<'db>, Vec<Item<'db>>> = FxHashMap::default();
    let mut expanded: FxHashSet<Item<'db>> = FxHashSet::from_iter([item]);
    let mut stack = vec![item];
    while let Some(reader) = stack.pop() {
        for &target in item_interface_edges(db, reader) {
            readers.entry(target).or_default().push(reader);
            reached.insert(target);
            if expanded.insert(target) {
                stack.push(target);
            }
        }
    }
    if !reached.contains(&item) {
        return None;
    }
    // Every reader recorded above is reachable from the item, so walking the reversed edges
    // from the item collects exactly the items on a cycle through it.
    let mut members: FxHashSet<Item<'db>> = FxHashSet::from_iter([item]);
    let mut stack = vec![item];
    while let Some(target) = stack.pop() {
        for &reader in readers.get(&target).into_iter().flatten() {
            if members.insert(reader) {
                stack.push(reader);
            }
        }
    }
    Some(members)
}

/// The cyclic groups the items of one file belong to, in the canonical order of their first
/// members. Most files have none. The value names items and no positions, so a check that reads
/// it is unaffected by an edit that moves the file's items without changing a cycle.
#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct FileGroups<'db> {
    members: Vec<Box<[Item<'db>]>>,
}

/// One walk per file finds the groups of all its items. It visits only what the file's items
/// can read, transitively, because a strongly connected component lies inside the reachable
/// set of each of its members. A check therefore depends on the edges its file can reach
/// rather than on every item in the project, and an edit re-walks only the files that reach
/// an edge it changed.
#[salsa::tracked(returns(ref))]
pub fn file_interface_groups<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    file: SourceFile,
) -> FileGroups<'db> {
    let mut order: Option<FxHashMap<SourceFile, usize>> = None;
    let mut keyed: Vec<(CanonicalPosition, Box<[Item<'db>]>)> = Vec::new();
    strongly_connected(db, item_tree(db, file).iter().copied(), |members| {
        if !members.iter().any(|member| *member.file(db) == file) || !cyclic(db, members) {
            return;
        }
        let order = order.get_or_insert_with(|| file_order(db, files));
        let group = canonical_order(db, order, members);
        let first = group
            .first()
            .map(|&member| canonical_position(db, order, member));
        keyed.push((first.unwrap_or_default(), group));
    });
    // Components arrive in discovery order, which a new item elsewhere in the file can change,
    // and sorting keeps the value equal across such an edit.
    keyed.sort_by_key(|&(first, _)| first);
    FileGroups {
        members: keyed.into_iter().map(|(_, group)| group).collect(),
    }
}

/// Tarjan's algorithm over the interface edges reachable from `roots`, reporting each strongly
/// connected component once, callees before callers. Iterative, because recursion depth would
/// scale with the longest read chain.
fn strongly_connected<'db>(
    db: &'db dyn Db,
    roots: impl IntoIterator<Item = Item<'db>>,
    mut component: impl FnMut(&[Item<'db>]),
) {
    let mut nodes: Vec<Item<'db>> = Vec::new();
    let mut index_of: FxHashMap<Item<'db>, usize> = FxHashMap::default();
    // A node's low link, or `OFF_STACK` once its component is reported.
    const OFF_STACK: usize = usize::MAX;
    let mut low: Vec<usize> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut work: Vec<(usize, &'db [Item<'db>], usize)> = Vec::new();
    let mut members: Vec<Item<'db>> = Vec::new();
    for root in roots {
        if index_of.contains_key(&root) {
            continue;
        }
        let mut discovered = Some(root);
        loop {
            if let Some(target) = discovered.take() {
                let target_index = nodes.len();
                nodes.push(target);
                index_of.insert(target, target_index);
                low.push(target_index);
                stack.push(target_index);
                work.push((target_index, item_interface_edges(db, target), 0));
            }
            let Some(&mut (node, edges, ref mut edge_cursor)) = work.last_mut() else {
                break;
            };
            if let Some(&target) = edges.get(*edge_cursor) {
                *edge_cursor += 1;
                match index_of.get(&target) {
                    Some(&target_index) => {
                        if low[target_index] != OFF_STACK {
                            low[node] = low[node].min(target_index);
                        }
                    }
                    None => discovered = Some(target),
                }
                continue;
            }
            work.pop();
            let node_low = low[node];
            if let Some(&(parent, _, _)) = work.last() {
                low[parent] = low[parent].min(node_low);
            }
            // Discovery order doubles as the Tarjan index, so a root is a node whose low link
            // is its own position.
            if node_low == node {
                members.clear();
                while let Some(member) = stack.pop() {
                    low[member] = OFF_STACK;
                    members.push(nodes[member]);
                    if member == node {
                        break;
                    }
                }
                component(&members);
            }
        }
    }
}

fn cyclic<'db>(db: &'db dyn Db, members: &[Item<'db>]) -> bool {
    match members {
        [item] => item_interface_edges(db, *item).contains(item),
        _ => true,
    }
}

fn file_order(db: &dyn Db, files: ProjectFiles) -> FxHashMap<SourceFile, usize> {
    files
        .files(db)
        .iter()
        .enumerate()
        .map(|(index, &file)| (file, index))
        .collect()
}

fn canonical_order<'db>(
    db: &'db dyn Db,
    file_order: &FxHashMap<SourceFile, usize>,
    members: &[Item<'db>],
) -> Box<[Item<'db>]> {
    let mut group = members.to_vec();
    group.sort_by_cached_key(|&member| canonical_position(db, file_order, member));
    group.into_boxed_slice()
}

/// Project file order, then item order, with an item outside the project first.
type CanonicalPosition = (Option<usize>, Option<usize>);

fn canonical_position<'db>(
    db: &'db dyn Db,
    file_order: &FxHashMap<SourceFile, usize>,
    member: Item<'db>,
) -> CanonicalPosition {
    let file = *member.file(db);
    let item_order = item_tree(db, file)
        .iter()
        .position(|&candidate| candidate == member);
    (file_order.get(&file).copied(), item_order)
}

/// What a cyclic-group member exports to its readers: the scheme `global_scheme` serves and
/// the bindings `statement_binding_scheme` serves.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct MemberExports<'db> {
    pub scheme: types::TypeScheme<'db>,
    pub top_level_bindings: ThinVec<(Symbol, types::TypeScheme<'db>)>,
}

/// Independent of which member was queried first: each round re-checks every member
/// against the previous round's exports, and members still changing at the cap pin to
/// `Unknown`. Members are checked directly, never via `item_check`, so no salsa cycle forms.
#[salsa::tracked(
    returns(ref),
    cycle_fn = scc_exports_recover,
    cycle_initial = scc_exports_initial
)]
pub fn scc_exports<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    representative: Item<'db>,
) -> FxHashMap<Item<'db>, MemberExports<'db>> {
    let mut exports = unknown_group_exports(db, files, representative);
    let Some(members) = item_group(db, files, representative) else {
        return exports;
    };
    for _round in 0..SCHEME_ROUND_CAP {
        let next_exports: FxHashMap<Item<'db>, MemberExports<'db>> = members
            .iter()
            .map(|&item| {
                let globals = SalsaGlobals::for_item(db, item, Some(&exports));
                (
                    item,
                    member_exports(db, check_item_against(db, item, &globals)),
                )
            })
            .collect();
        if next_exports == exports {
            return exports;
        }
        exports = next_exports;
    }
    // Pinning every member is the only entry-order-free refusal.
    unknown_group_exports(db, files, representative)
}

fn member_exports<'db>(
    db: &'db dyn Db,
    check: Option<check::ItemCheck<'db>>,
) -> MemberExports<'db> {
    let unknown = || types::TypeScheme::monomorphic(types::unknown(db));
    match check {
        Some(check) => MemberExports {
            scheme: check.scheme.unwrap_or_else(unknown),
            top_level_bindings: check.top_level_bindings,
        },
        None => MemberExports {
            scheme: unknown(),
            top_level_bindings: ThinVec::new(),
        },
    }
}

/// The fixpoint's starting point and its refusal: every surface a member exports is `Unknown`.
fn unknown_group_exports<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    representative: Item<'db>,
) -> FxHashMap<Item<'db>, MemberExports<'db>> {
    let unknown = types::TypeScheme::monomorphic(types::unknown(db));
    item_group(db, files, representative)
        .map(|members| {
            members
                .iter()
                .map(|&item| {
                    let top_level_bindings = item_top_level_names(db, item)
                        .iter()
                        .map(|&name| (name, unknown.clone()))
                        .collect();
                    let exports = MemberExports {
                        scheme: unknown.clone(),
                        top_level_bindings,
                    };
                    (item, exports)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn scc_exports_initial<'db>(
    db: &'db dyn Db,
    _id: salsa::Id,
    files: ProjectFiles,
    representative: Item<'db>,
) -> FxHashMap<Item<'db>, MemberExports<'db>> {
    unknown_group_exports(db, files, representative)
}

/// Backstop for edges the static graph cannot see (a name the checker constructs, such
/// as an S3 method), which re-enter this group through `global_scheme`.
fn scc_exports_recover<'db>(
    db: &'db dyn Db,
    _cycle: &salsa::Cycle,
    last_provisional: &FxHashMap<Item<'db>, MemberExports<'db>>,
    value: FxHashMap<Item<'db>, MemberExports<'db>>,
    files: ProjectFiles,
    representative: Item<'db>,
) -> FxHashMap<Item<'db>, MemberExports<'db>> {
    // Refuse on the first disagreement: salsa iterating this query would multiply the
    // group's own bounded rounds and can exhaust memory.
    if &value == last_provisional {
        return value;
    }
    unknown_group_exports(db, files, representative)
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
        file_is_strict(db, *item.file(db)),
        *file_holds_ry_values(db, *item.file(db)),
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
        .and_then(|check| check.scheme.clone())
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
    returns(as_ref),
    cycle_fn = item_check_recover,
    cycle_initial = item_check_initial
)]
pub fn item_check<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<check::ItemCheck<'db>> {
    // Recursion depth follows the longest chain of definitions each reading the next
    // (thousands of `df <- step(df)` rebindings), so no fixed stack bounds it.
    stacker::maybe_grow(256 * 1024, 16 * 1024 * 1024, || check_item(db, item))
}

fn check_item<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<check::ItemCheck<'db>> {
    // A cyclic-group member reads its fellow members' exports from the group's fixpoint,
    // never through `global_scheme`: that read would close a salsa cycle, and salsa backdates
    // a cycle's head but not its other queries, so an edit that breaks the cycle while
    // keeping a member's value re-executes that member with an older `changed_at` than its
    // memo holds, which salsa rejects.
    let group = ProjectFiles::try_get(db).and_then(|files| {
        let representative = *item_group(db, files, item)?.first()?;
        Some(scc_exports(db, files, representative))
    });
    let mut check = check_item_against(db, item, &SalsaGlobals::for_item(db, item, group))?;
    // The exports are the group's fixpoint values, not this check's, which differ once the
    // round cap pins the group.
    if let Some(exports) = group.and_then(|group| group.get(&item)) {
        check.top_level_bindings = exports.top_level_bindings.clone();
        if check.scheme.is_some() {
            check.scheme = Some(exports.scheme.clone());
            // A clean body whose export still carries `Unknown` owes it to the cycle itself,
            // so strict mode marks the whole binding.
            if check.errors.is_empty()
                && check.strict_origins.is_empty()
                && file_is_strict(db, *item.file(db))
                && types::contains_unknown(db, exports.scheme.body)
                && let Some(module) = item_hir(db, item)
                && let Some(root) = module.root
                && let Some(name) = item.name(db).clone()
            {
                check.strict_origins.push(check::StrictOrigin {
                    expression: root,
                    range: module.range(root),
                    kind: check::StrictOriginKind::RecursiveUnknown(name),
                });
            }
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

/// The globals one item's check reads. The names the item spells resolve through its
/// [`item_links`], and a name the check constructs (a synthesized S3 method's, say) through a
/// per-name query, so the check depends on exactly the names it reads.
struct SalsaGlobals<'db, 'a> {
    db: &'db dyn Db,
    item: Item<'db>,
    /// The exports of the item's cyclic group, which stand in for the queries that serve a
    /// member's: reading a member through salsa would re-enter the group.
    group: Option<&'a FxHashMap<Item<'db>, MemberExports<'db>>>,
    files: Option<ProjectFiles>,
    file: SourceFile,
    // Read once: a body looks up hundreds of names, and each salsa read of an input field or a
    // memo records a dependency edge.
    kind: DocumentKind,
    command: Option<u32>,
    sources_files: bool,
    definitions: TypeDefinitions<'db>,
    library: Option<&'db stubs::StubLibrary<'db>>,
    links: &'db ItemLinks<'db>,
    /// The scheme each linked name resolved to in this check, at twice the name's position
    /// plus one for a deferred read. A body reads the same few globals many times, and
    /// resolving one reads the binder's check through salsa and copies its scheme.
    schemes: RefCell<Vec<Option<Option<types::TypeScheme<'db>>>>>,
}

/// The type definitions a file sees, looked up one name at a time: a script's own
/// declarations, which are visible to itself alone, shadow the project's, and the project's
/// shadow the stubs'. A merged map per file would copy every visible definition into every
/// file that reads one, which costs time and memory in proportion to files times types.
#[derive(Clone, Copy)]
pub struct TypeDefinitions<'db> {
    file: SourceFile,
    command: Option<u32>,
    package_file: bool,
    files: Option<ProjectFiles>,
    library: Option<&'db stubs::StubLibrary<'db>>,
}

impl std::fmt::Debug for TypeDefinitions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypeDefinitions")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl<'db> TypeDefinitions<'db> {
    /// Fetches what every lookup needs once, because a checker asks about hundreds of names
    /// per item and each salsa fetch records a dependency edge.
    pub fn of(db: &'db dyn Db, file: SourceFile) -> Self {
        Self {
            file,
            command: *file.command(db),
            package_file: *file.kind(db) == DocumentKind::Package,
            files: ProjectFiles::try_get(db),
            library: stubs::stubs(db),
        }
    }

    pub fn library(self) -> Option<&'db stubs::StubLibrary<'db>> {
        self.library
    }

    pub fn get(
        self,
        db: &'db dyn Db,
        name: types::Name<'db>,
    ) -> Option<&'db annotations::NamedDefinition<'db>> {
        // A package file's definitions are the project's, unless there is no project.
        if (self.files.is_none() || !self.package_file)
            && let Some(definition) = file_type_definitions(db, self.file)
                .iter()
                .rev()
                .find(|definition| definition.name == name)
        {
            return Some(definition);
        }
        if let Some(command) = self.command
            && let Some(definition) = session::type_definition(db, command, name)
        {
            return Some(definition);
        }
        if let Some(files) = self.files
            && let Some(definition) = project_type_definition(db, files, name)
        {
            return Some(definition);
        }
        self.library?.type_definitions.get(&name)
    }
}

/// One name's project definition, so that an edit to one `@type` re-verifies only the
/// readers of that name.
#[salsa::tracked(returns(as_ref))]
fn project_type_definition<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    name: types::Name<'db>,
) -> Option<annotations::NamedDefinition<'db>> {
    project_type_definitions(db, files).get(&name).cloned()
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
    let definitions = TypeDefinitions::of(db, file);
    let tracked = |class: &String| {
        definitions
            .get(db, types::Name::new(db, class.as_str()))
            .is_some()
    };
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

/// Every top-level name the file's items bind, as `(name, position in the item tree, item)`
/// sorted by the name's symbol index and then position, so a lookup is a binary search over
/// integers rather than a scan of the item list. Only each name's run is ever read, so the
/// order between names may differ between runs. One exactly sized array, because every file keeps it for the whole run, and a
/// map of per-name vectors cost four times the memory.
#[salsa::tracked(returns(ref))]
fn file_binders<'db>(db: &'db dyn Db, file: SourceFile) -> ThinVec<(Symbol, u32, Item<'db>)> {
    let mut binders = Vec::new();
    for (index, &item) in item_tree(db, file).iter().enumerate() {
        let index = u32::try_from(index).expect("a file holds fewer than u32::MAX items");
        let own_name = item.name(db).as_deref().map(Symbol::new);
        if let Some(name) = own_name {
            binders.push((name, index, item));
        }
        for &name in item_top_level_names(db, item) {
            if own_name != Some(name) {
                binders.push((name, index, item));
            }
        }
    }
    binders.sort_unstable_by_key(|&(name, index, _)| (name.index(), index));
    ThinVec::from(binders)
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
    FrameBinders::of(db, item).binder(name.symbol(), deferred)
}

/// What every binder lookup of one item reads, fetched once: each salsa read records a
/// dependency edge, and an item looks up dozens of names.
struct FrameBinders<'db> {
    item: Item<'db>,
    binders: &'db [(Symbol, u32, Item<'db>)],
    test: bool,
    /// The item's position in its file's item tree.
    position: usize,
}

impl<'db> FrameBinders<'db> {
    fn of(db: &'db dyn Db, item: Item<'db>) -> FrameBinders<'db> {
        let file = *item.file(db);
        FrameBinders {
            item,
            binders: file_binders(db, file),
            test: matches!(
                *file.kind(db),
                DocumentKind::Test | DocumentKind::TestHelper
            ),
            position: item_tree_positions(db, file)
                .get(&item)
                .copied()
                .unwrap_or_else(|| item_tree(db, file).len()),
        }
    }

    fn binder(&self, name: Symbol, deferred: bool) -> Option<Item<'db>> {
        let key = name.index();
        let start = self
            .binders
            .partition_point(|&(binder, _, _)| binder.index() < key);
        let length = self.binders[start..].partition_point(|&(binder, _, _)| binder == name);
        let binders = &self.binders[start..start + length];
        if deferred && self.test && binders.iter().any(|&(_, _, binder)| binder == self.item) {
            return Some(self.item);
        }
        let visible = match deferred {
            true => binders,
            false => {
                &binders[..binders.partition_point(|&(_, at, _)| (at as usize) < self.position)]
            }
        };
        visible.last().map(|&(_, _, item)| item)
    }
}

impl<'db, 'a> SalsaGlobals<'db, 'a> {
    fn for_item(
        db: &'db dyn Db,
        item: Item<'db>,
        group: Option<&'a FxHashMap<Item<'db>, MemberExports<'db>>>,
    ) -> SalsaGlobals<'db, 'a> {
        let file = *item.file(db);
        SalsaGlobals {
            db,
            item,
            group,
            files: ProjectFiles::try_get(db),
            file,
            kind: *file.kind(db),
            command: *file.command(db),
            sources_files: !sourced_files(db, file).is_empty(),
            definitions: TypeDefinitions::of(db, file),
            library: stubs::stubs(db),
            links: item_links(db, item),
            schemes: RefCell::default(),
        }
    }

    /// What a read of `name` sees outside the item.
    fn bindings(&self, name: Symbol, deferred: bool) -> NameBindings<'db, 'db> {
        let Some(position) = self.links.position(name) else {
            return self.unlinked_bindings(name, deferred);
        };
        let bindings = self.links.bindings(position, deferred);
        if syntax::testing::invariants_enabled() {
            assert!(
                bindings.reads_like(self.unlinked_bindings(name, deferred)),
                "the link of `{name}` disagrees with its per-name projections"
            );
        }
        bindings
    }

    /// [`ItemLinks::bindings`] for a name the item's naming does not list, each binding
    /// through its own per-name query.
    fn unlinked_bindings(&self, name: Symbol, deferred: bool) -> NameBindings<'db, 'db> {
        let layer = |test: bool| match self.files {
            Some(_) => name_layer(self.db, name, test),
            None => NameLayer::default(),
        };
        let frame_binder = || {
            frame_binder(
                self.db,
                self.item,
                types::Name::new(self.db, name),
                deferred,
            )
        };
        let package = layer(false);
        // A name a check constructs for itself (the S3 method `Ops.Date` of an operand's
        // class) is bound almost nowhere, and an item asks for dozens of them, so a
        // `frame_binder` memo for each would outnumber every other memo. A package file
        // needs none: each top-level name it binds is a definition or a conditional write of
        // the package layer, so a name that layer does not bind has no binder in the file.
        // Another kind of file has no such layer over its own names and asks.
        let unbound =
            self.kind == DocumentKind::Package && self.links.file_in_project && !package.binds();
        if unbound && syntax::testing::invariants_enabled() {
            assert_eq!(
                frame_binder(),
                None,
                "the package layer does not bind `{name}`, yet the item's package file does"
            );
        }
        NameBindings {
            frame: match unbound || (deferred && self.kind == DocumentKind::Package) {
                true => None,
                false => frame_binder(),
            },
            sourced: match self.sources_files {
                true => *sourced_name_definition(self.db, self.file, name),
                false => None,
            },
            test: match self.kind {
                DocumentKind::Test | DocumentKind::TestHelper => layer(true),
                _ => NameLayer::default(),
            },
            package,
        }
    }

    fn item_scheme(&self, item: Item<'db>) -> types::TypeScheme<'db> {
        match self.group.and_then(|group| group.get(&item)) {
            Some(exports) => exports.scheme.clone(),
            None => global_scheme(self.db, item),
        }
    }

    fn statement_scheme(&self, item: Item<'db>, name: Symbol) -> Option<types::TypeScheme<'db>> {
        let Some(exports) = self.group.and_then(|group| group.get(&item)) else {
            return statement_binding_scheme(self.db, item, types::Name::new(self.db, name));
        };
        exports
            .top_level_bindings
            .iter()
            .find(|(binding, _)| *binding == name)
            .map(|(_, scheme)| scheme.clone())
    }

    fn source_scheme(
        &self,
        source: Source<'_, 'db>,
        name: Symbol,
    ) -> Option<types::TypeScheme<'db>> {
        match source {
            Source::Definition(item) => Some(self.item_scheme(item)),
            Source::Statement(item) => self.statement_scheme(item, name),
            Source::Conditional(writers) if writers.len() > CONDITIONAL_SLOT_JOIN_CAP => {
                Some(types::TypeScheme::monomorphic(types::unknown(self.db)))
            }
            Source::Conditional(writers) => {
                let mut schemes: Vec<types::TypeScheme<'db>> = writers
                    .iter()
                    .filter_map(|&item| self.statement_scheme(item, name))
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
    }

    fn layer_scheme(
        &self,
        layer: NameLayer<'_, 'db>,
        name: Symbol,
    ) -> Option<types::TypeScheme<'db>> {
        self.source_scheme(layer_source(layer.winner, layer.conditional_writers)?, name)
    }

    /// The binder in the item's own file, or for a console command, in the session before it.
    fn frame_definition(
        &self,
        name: Symbol,
        deferred: bool,
        bindings: NameBindings<'_, 'db>,
    ) -> Option<Item<'db>> {
        match self.command {
            Some(command) => session::binder(self.db, command, name, deferred, bindings.frame),
            None => bindings.frame,
        }
    }

    fn binder_scheme(&self, item: Item<'db>, name: Symbol) -> Option<types::TypeScheme<'db>> {
        self.source_scheme(binder_source(self.db, item, name.as_str()), name)
    }

    fn lookup_scheme(&self, name: Symbol, deferred: bool) -> Option<types::TypeScheme<'db>> {
        let bindings = self.bindings(name, deferred);
        if let Some(item) = self.frame_definition(name, deferred, bindings)
            && let Some(scheme) = self.binder_scheme(item, name)
        {
            return Some(scheme);
        }
        if let Some(scheme) = bindings
            .layers()
            .find_map(|layer| self.layer_scheme(layer, name))
        {
            return Some(scheme);
        }
        // A plain-value read of an overloaded stub takes the last candidate: the corpus orders
        // candidates specific-first, so the last is the most general.
        self.bare_library_candidates(name.as_str())?.last().cloned()
    }

    /// The shipped declarations a bare `name` reaches, which leave out the standard library's
    /// when `[check] prelude` is off.
    fn bare_library_candidates(&self, name: &str) -> Option<&'db [types::TypeScheme<'db>]> {
        let library = self.library?;
        if stubs::outside_prelude(self.db, library, name) {
            return None;
        }
        library.schemes.get(name).map(Vec::as_slice)
    }
}

impl<'db> check::GlobalEnv<'db> for SalsaGlobals<'db, '_> {
    fn scheme(&self, name: Symbol, deferred: bool) -> Option<types::TypeScheme<'db>> {
        let slot = self
            .links
            .position(name)
            .map(|position| 2 * position + usize::from(deferred));
        if let Some(slot) = slot
            && let Some(Some(scheme)) = self.schemes.borrow().get(slot)
        {
            return scheme.clone();
        }
        let scheme = self.lookup_scheme(name, deferred);
        if let Some(slot) = slot {
            let mut schemes = self.schemes.borrow_mut();
            if schemes.is_empty() {
                schemes.resize(2 * self.links.names.len(), None);
            }
            if let Some(resolved) = schemes.get_mut(slot) {
                *resolved = Some(scheme.clone());
            }
        }
        scheme
    }

    fn defined_in_project(&self, name: Symbol, deferred: bool) -> bool {
        let bindings = self.bindings(name, deferred);
        self.frame_definition(name, deferred, bindings).is_some()
            || bindings.layers().any(NameLayer::binds)
    }

    fn earlier_binding(&self, name: Symbol) -> Option<types::TypeScheme<'db>> {
        let bindings = self.bindings(name, false);
        if let Some(item) = self.frame_definition(name, false, bindings) {
            return self.binder_scheme(item, name);
        }
        Some(self.item_scheme(bindings.layers().find_map(|layer| layer.winner)?))
    }

    fn namespace_scheme(&self, name: Symbol) -> Option<types::TypeScheme<'db>> {
        if self.kind == DocumentKind::Package {
            return self.scheme(name, false);
        }
        if let Some(scheme) = self.layer_scheme(self.bindings(name, false).package, name) {
            return Some(scheme);
        }
        self.library?.schemes.get(name.as_str())?.last().cloned()
    }

    fn enclosing_scheme(&self, name: Symbol) -> Option<types::TypeScheme<'db>> {
        if self.kind == DocumentKind::Package {
            return None;
        }
        self.bindings(name, false)
            .layers()
            .find_map(|layer| self.layer_scheme(layer, name))
    }

    fn library_candidates(&self, name: &str) -> Option<&'db [types::TypeScheme<'db>]> {
        let library = self.library?;
        if stubs::outside_prelude(self.db, library, name) {
            return None;
        }
        match library.schemes.get(name) {
            Some(candidates) => Some(candidates),
            None => library.known_exports.contains(name).then_some(&[]),
        }
    }

    fn overloads(&self, name: Symbol, deferred: bool) -> Option<&'db [types::TypeScheme<'db>]> {
        let candidates = self.bare_library_candidates(name.as_str())?;
        if candidates.len() < 2 {
            return None;
        }
        // A project definition shadows the stub set, disabling per-call overload selection.
        let bindings = self.bindings(name, deferred);
        if self.frame_definition(name, deferred, bindings).is_some()
            || bindings.layers().any(|layer| layer.winner.is_some())
        {
            return None;
        }
        Some(candidates)
    }

    fn type_definitions(&self) -> TypeDefinitions<'db> {
        self.definitions
    }

    fn arithmetic_classes(&self) -> &'db FxHashSet<String> {
        visible_arithmetic_classes(self.db, self.file)
    }

    fn has_untracked_arithmetic_class(&self) -> bool {
        *has_untracked_arithmetic_class(self.db, self.file)
    }

    fn defines_dollar_method(&self) -> bool {
        *file_defines_dollar_method(self.db, self.file)
            || ProjectFiles::try_get(self.db)
                .is_some_and(|files| *project_defines_dollar_method(self.db, files))
    }
}

#[salsa::tracked(returns(clone))]
fn stub_arithmetic_classes(db: &dyn Db) -> FxHashSet<String> {
    let Some(library) = stubs::stubs(db) else {
        return FxHashSet::default();
    };
    arithmetic_classes_among(library.schemes.keys().map(String::as_str))
}

/// A union of the per-file sets, which are almost always empty, rather than a scan of every
/// project name: a new top-level name then costs one file's scan.
#[salsa::tracked(returns(clone))]
fn project_arithmetic_classes(db: &dyn Db, files: ProjectFiles) -> FxHashSet<String> {
    let mut classes = FxHashSet::default();
    for &file in files.files(db) {
        if *file.kind(db) == DocumentKind::Package {
            classes.extend(file_arithmetic_classes(db, file).iter().cloned());
        }
    }
    classes
}

#[salsa::tracked]
fn project_defines_dollar_method(db: &dyn Db, files: ProjectFiles) -> bool {
    files.files(db).iter().any(|&file| {
        *file.kind(db) == DocumentKind::Package && *file_defines_dollar_method(db, file)
    })
}

#[salsa::tracked]
fn file_defines_dollar_method(db: &dyn Db, file: SourceFile) -> bool {
    file_definitions(db, file).iter().any(|(name, _)| {
        name.as_str()
            .strip_prefix("$.")
            .is_some_and(|class| !class.is_empty())
    })
}

#[salsa::tracked(returns(ref))]
fn file_arithmetic_classes(db: &dyn Db, file: SourceFile) -> FxHashSet<String> {
    arithmetic_classes_among(
        file_definitions(db, file)
            .iter()
            .map(|(name, _)| name.as_str()),
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

/// File-absolute (edge-only, like [`item_node`]): the assignment target or the
/// `setGeneric` name string, recorded by [`item_spans`]. Memoized so that a check reading
/// it from another file depends on this one range, not on every span of that file.
#[salsa::tracked(returns(copy))]
pub fn item_name_range<'db>(db: &'db dyn Db, item: Item<'db>) -> Option<TextRange> {
    let file = *item.file(db);
    let index = *item_span_positions(db, file).get(&item)?;
    item_spans(db, file).get(index)?.name_range
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
    let binary = node.node_ref();
    let mut operator = None;
    let mut operands = binary
        .child_nodes()
        .filter(|child| is_expression_kind(child.kind()));
    let (lhs, rhs) = (operands.next(), operands.next());
    for child in binary.child_tokens() {
        if !child.kind().is_trivia() {
            operator = Some(child.kind());
            break;
        }
    }
    // `:=` binds nothing: R calls a function of that name (data.table, rlang).
    let (target, value) = match operator? {
        SyntaxKind::LESS_MINUS | SyntaxKind::EQ | SyntaxKind::LESS2_MINUS => (lhs, rhs),
        SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => (rhs, lhs),
        _ => return None,
    };
    let target = target?;
    let name = match target.kind() {
        SyntaxKind::NAME => name_text(target.first_token()?.text()).into_owned(),
        SyntaxKind::LITERAL
            if target
                .first_token()
                .is_some_and(|token| token.kind() == SyntaxKind::STRING) =>
        {
            let text: String = target.child_tokens().map(|token| token.text()).collect();
            text.trim_matches(['"', '\'']).to_owned()
        }
        _ => return None,
    };
    let kind = match value.map(|value| value.kind()) {
        Some(SyntaxKind::FUNCTION_DEF) => ItemKind::Function,
        _ => ItemKind::Value,
    };
    Some(TopLevelDefinition {
        kind,
        name,
        name_range: target.range(),
    })
}

/// The name a top-level `setGeneric` binds, with its range inside the string: the only S4
/// registration that creates a bare-name binding (`setClass`/`setMethod` bind nothing).
fn set_generic_target(node: &SyntaxNode) -> Option<(String, TextRange)> {
    use syntax::SyntaxKind;
    // `if (!isGeneric("f")) { ...; setGeneric("f", fun) }` makes the generic only when no
    // package made it first, so either way `f` is a function once the statement has run
    // (kernlab's `type`).
    if node.kind() == SyntaxKind::IF_EXPR {
        if node
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::ELSE_KW)
        {
            return None;
        }
        let body = node.children().nth(1)?;
        if body.kind() != SyntaxKind::BRACE_EXPR {
            return set_generic_target(&body);
        }
        return body
            .children()
            .find_map(|statement| set_generic_target(&statement));
    }
    if node.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let callee = node.children().next()?;
    let callee_name = match callee.kind() {
        SyntaxKind::NAME => node_text(&callee).into_owned(),
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
    use {super::*, salsa::Setter as _};

    fn function_item<'db>(db: &'db RootDatabase, file: SourceFile, name: &str) -> Item<'db> {
        Item::new(db, file, ItemKind::Function, Some(name.to_owned()), None, 0)
    }

    #[test]
    fn refusal_is_idempotent() {
        let db = RootDatabase::default();
        let integer = types::scalar(&db, types::Atomic::Integer);
        let check = check::ItemCheck {
            editor: None,
            errors: ThinVec::new(),
            strict_origins: ThinVec::new(),
            native_blockers: ThinVec::new(),
            scheme: Some(types::TypeScheme::monomorphic(integer)),
            masked_reads: Default::default(),
            column_misses: Default::default(),
            match_findings: ThinVec::new(),
            i64_literals: ThinVec::new(),
            typed_empties: ThinVec::new(),
            top_level_bindings: vec![(
                hir::Symbol::new("value"),
                types::TypeScheme::monomorphic(integer),
            )]
            .into(),
        };

        let once = refuse_check(&db, check);
        let twice = refuse_check(&db, once.clone());
        assert_eq!(once, twice, "re-pinning a pinned check must reproduce it");

        let unknown = types::TypeScheme::monomorphic(types::unknown(&db));
        assert_eq!(once.scheme.as_ref(), Some(&unknown));
        assert_eq!(
            once.top_level_bindings,
            thin_vec::thin_vec![(hir::Symbol::new("value"), unknown)]
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
        let root = before.root.expect("has root");
        let hir::ExpressionKind::Assign { value, .. } = before.kind(root) else {
            panic!("expected an assignment root, got {root:?}");
        };
        let hir::ExpressionKind::Function { parameters, body } = &before.kind(*value) else {
            panic!("expected a function value");
        };
        let parameters = &before[parameters];
        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].name, "x");
        assert_eq!(parameters[1].name, "y");
        assert!(parameters[1].default.is_some());
        let hir::ExpressionKind::Binary { operator, .. } = &before.kind(*body) else {
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
        let winner = winners.get(&Symbol::new("limit")).expect("winner exists");
        assert_eq!(*winner.file(&db), second);
        let scheme = global_scheme(&db, *winner);
        assert_eq!(
            scheme.body,
            types::scalar(&db, types::Atomic::Integer),
            "the later file's integer wins"
        );
    }
}
