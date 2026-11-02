//! The syntax tree of one file, stored flat.
//!
//! Every node and token is an element, and the elements sit in preorder in a few parallel
//! arrays: a kind, a start offset, the index one past the element's subtree, and the parent's
//! index. A subtree is therefore a contiguous index range, a node's text is a contiguous slice
//! of the file text, and a whole-tree walk is a linear scan. A cursor ([`SyntaxNode`],
//! [`SyntaxToken`]) is the shared tree plus an index, so moving one allocates nothing, and a
//! borrowed cursor ([`NodeRef`]) is two words copied.

use {
    crate::kind::SyntaxKind,
    std::{
        fmt,
        hash::{Hash, Hasher},
        iter,
        ops::Range,
        sync::Arc,
    },
    text_size::{TextRange, TextSize},
};

/// An immutable syntax tree. Element 0 is the root, and offsets count from the root's start.
///
/// A tree is a window onto a file's shared columns, so a subtree is the same columns with a
/// different first element: taking one copies nothing, and it reads its offsets and indices
/// relative to its own root. Two trees are equal when their windows hold the same elements over
/// the same text, wherever each sits in its file, which is what makes an item's syntax
/// position independent.
#[derive(Clone)]
pub struct Tree {
    columns: Arc<Columns>,
    /// The column index of this tree's root.
    first: u32,
    len: u32,
    /// The absolute offset of this tree's root.
    base: TextSize,
}

/// One file's elements in preorder, with absolute offsets and indices.
struct Columns {
    text: Box<str>,
    kinds: Box<[SyntaxKind]>,
    starts: Box<[TextSize]>,
    /// One past the last element of each element's subtree, so `index + 1` for a token.
    ends: Box<[u32]>,
    /// Each element's parent, [`NO_PARENT`] for the root. Derivable from `ends`, but kept so
    /// that climbing to a parent is one load instead of a search.
    parents: Box<[u32]>,
}

const NO_PARENT: u32 = u32::MAX;

impl Tree {
    fn from_columns(columns: Columns) -> Tree {
        let len =
            u32::try_from(columns.kinds.len()).expect("a syntax tree has fewer than 2^32 elements");
        Tree {
            columns: Arc::new(columns),
            first: 0,
            len,
            base: TextSize::new(0),
        }
    }

    #[inline]
    pub fn text(&self) -> &str {
        let start = usize::from(self.base);
        &self.columns.text[start..start + usize::from(self.end_offset(0))]
    }

    /// The text of `range`, which is relative to this window.
    #[inline]
    fn slice(&self, range: TextRange) -> &str {
        &self.columns.text[range + self.base]
    }

    /// The number of elements, nodes and tokens together.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn root(&self) -> NodeRef<'_> {
        NodeRef {
            tree: self,
            index: 0,
        }
    }

    /// The subtree at `node` as a tree of its own, with offsets counted from its start. It
    /// shares this tree's columns, and two equal subtrees anywhere in any file compare equal.
    pub fn subtree(&self, node: NodeRef<'_>) -> Tree {
        Tree {
            columns: self.columns.clone(),
            first: self.first + node.index,
            len: self.end(node.index) - node.index,
            base: self.base + self.start_offset(node.index),
        }
    }

    /// This window copied into columns of its own, so that keeping it does not keep the rest of
    /// its file's tree alive. It compares equal to the window it was copied from.
    pub fn detach(&self) -> Tree {
        let window = self.column(0)..self.column(self.len);
        let text_start = usize::from(self.base);
        let first = self.first;
        let base = self.base;
        let columns = &self.columns;
        Tree::from_columns(Columns {
            text: columns.text[text_start..text_start + usize::from(self.end_offset(0))].into(),
            kinds: columns.kinds[window.clone()].into(),
            starts: columns.starts[window.clone()]
                .iter()
                .map(|&start| start - base)
                .collect(),
            ends: columns.ends[window.clone()]
                .iter()
                .map(|&end| end - first)
                .collect(),
            parents: iter::once(NO_PARENT)
                .chain(
                    columns.parents[window]
                        .iter()
                        .skip(1)
                        .map(|&parent| parent - first),
                )
                .collect(),
        })
    }

    #[inline]
    fn column(&self, index: u32) -> usize {
        (self.first + index) as usize
    }

    #[inline]
    fn kind(&self, index: u32) -> SyntaxKind {
        self.columns.kinds[self.column(index)]
    }

    #[inline]
    fn is_node(&self, index: u32) -> bool {
        self.kind(index).is_node()
    }

    #[inline]
    fn end(&self, index: u32) -> u32 {
        self.columns.ends[self.column(index)] - self.first
    }

    #[inline]
    fn start_offset(&self, index: u32) -> TextSize {
        self.columns.starts[self.column(index)] - self.base
    }

    /// An element ends where the next element in preorder starts, which for the window's last
    /// element is the next element in the file, or the end of the file.
    #[inline]
    fn end_offset(&self, index: u32) -> TextSize {
        let end = self.columns.ends[self.column(index)] as usize;
        let absolute = match self.columns.starts.get(end) {
            Some(&start) => start,
            None => TextSize::of(&*self.columns.text),
        };
        absolute - self.base
    }

    #[inline]
    fn range(&self, index: u32) -> TextRange {
        TextRange::new(self.start_offset(index), self.end_offset(index))
    }

    /// How many elements in `first..end` start before the cut `before` draws: starts never
    /// decrease in preorder, so this is a binary search.
    #[inline]
    fn partition_starts(&self, first: u32, end: u32, before: impl Fn(TextSize) -> bool) -> u32 {
        let base = self.base;
        self.columns.starts[self.column(first)..self.column(end)]
            .partition_point(|&start| before(start - base)) as u32
    }

    #[inline]
    fn parent(&self, index: u32) -> Option<u32> {
        (index != 0).then(|| self.columns.parents[self.column(index)] - self.first)
    }

    #[inline]
    fn first_child_or_token(&self, index: u32) -> Option<u32> {
        (index + 1 < self.end(index)).then_some(index + 1)
    }

    fn last_child_or_token(&self, index: u32) -> Option<u32> {
        let last = self.end(index) - 1;
        (last != index).then(|| self.child_containing(index, last))
    }

    #[inline]
    fn next_sibling_or_token(&self, index: u32) -> Option<u32> {
        let parent = self.parent(index)?;
        let next = self.end(index);
        (next < self.end(parent)).then_some(next)
    }

    /// The element before `index` in preorder is the last element of the previous sibling's
    /// subtree, so the sibling is that element's ancestor under the shared parent.
    fn prev_sibling_or_token(&self, index: u32) -> Option<u32> {
        let parent = self.parent(index)?;
        (index > parent + 1).then(|| self.child_containing(parent, index - 1))
    }

    /// The child of `node` whose subtree holds the descendant `element`.
    fn child_containing(&self, node: u32, element: u32) -> u32 {
        let mut child = element;
        while let Some(parent) = self.parent(child)
            && parent != node
        {
            child = parent;
        }
        child
    }

    /// The first token found by descending into first children, so an empty first child node
    /// ends the search, as a token search over a nested tree finds it.
    fn first_token(&self, index: u32) -> Option<u32> {
        let mut element = index;
        while self.is_node(element) {
            element = self.first_child_or_token(element)?;
        }
        Some(element)
    }

    /// [`Tree::first_token`] from the other end.
    fn last_token(&self, index: u32) -> Option<u32> {
        let mut element = index;
        while self.is_node(element) {
            element = self.last_child_or_token(element)?;
        }
        Some(element)
    }

    fn next_token(&self, token: u32) -> Option<u32> {
        let mut element = token;
        loop {
            if let Some(next) = self.next_sibling_or_token(element) {
                return self.first_token(next);
            }
            element = self.parent(element)?;
        }
    }

    fn prev_token(&self, token: u32) -> Option<u32> {
        let mut element = token;
        loop {
            if let Some(previous) = self.prev_sibling_or_token(element) {
                return self.last_token(previous);
            }
            element = self.parent(element)?;
        }
    }

    /// The position of `index` among its parent's children, tokens included.
    fn index_in_parent(&self, index: u32) -> usize {
        let Some(parent) = self.parent(index) else {
            return 0;
        };
        let mut child = parent + 1;
        let mut position = 0;
        while child != index {
            child = self.end(child);
            position += 1;
        }
        position
    }

    /// The non-empty tokens under `node` whose closed range holds `offset`. Starts never
    /// decrease in preorder, so a binary search finds where `offset` falls, and the tokens on
    /// either side of it are the answer.
    fn token_at_offset(&self, node: u32, offset: TextSize) -> TokenAtOffset<u32> {
        let range = self.range(node);
        assert!(
            range.start() <= offset && offset <= range.end(),
            "token_at_offset: {offset:?} is outside {range:?}"
        );
        if range.is_empty() {
            return TokenAtOffset::None;
        }
        if !self.is_node(node) {
            return TokenAtOffset::Single(node);
        }
        let first = node + 1;
        let end = self.end(node);
        let split = first + self.partition_starts(first, end, |start| start < offset);
        let left = (first..split)
            .rev()
            .find(|&element| !self.is_node(element) && !self.range(element).is_empty())
            .filter(|&token| self.end_offset(token) >= offset);
        let right = (split..end)
            .take_while(|&element| self.start_offset(element) == offset)
            .find(|&element| !self.is_node(element) && !self.range(element).is_empty());
        match (left, right) {
            (Some(left), Some(right)) => TokenAtOffset::Between(left, right),
            (Some(token), None) | (None, Some(token)) => TokenAtOffset::Single(token),
            (None, None) => TokenAtOffset::None,
        }
    }

    /// The child of `node` that holds `range`. An empty range strictly inside a child picks
    /// that child; on a boundary it picks the last child that ends there, or the first child
    /// when that one starts there and nothing ends there. This is what a binary search over
    /// the children's ranges finds, which is how a nested tree answers the same question.
    fn child_or_token_at_range(&self, node: u32, range: TextRange) -> Option<u32> {
        if !self.is_node(node) {
            return None;
        }
        let first = self.first_child_or_token(node)?;
        let end = self.end(node);
        let start = range.start();
        // The child holding the last element that starts before the range (or at its start,
        // for a non-empty range) is the only child that can contain it.
        let before = self.partition_starts(first, end, |element| {
            if range.is_empty() {
                element < start
            } else {
                element <= start
            }
        });
        let candidate = (before > 0).then(|| self.child_containing(node, first + before - 1));
        if !range.is_empty() {
            return candidate.filter(|&child| self.range(child).contains_range(range));
        }
        if let Some(child) = candidate {
            let child_range = self.range(child);
            if child_range.start() < start && start < child_range.end() {
                return Some(child);
            }
        }
        // Empty children sitting at the offset end there too, and the last of them wins.
        let mut last_ending = candidate;
        let mut next = match candidate {
            Some(child) => self.next_sibling_or_token(child),
            None => Some(first),
        };
        while let Some(child) = next
            && self.range(child) == range
        {
            last_ending = Some(child);
            next = self.next_sibling_or_token(child);
        }
        match last_ending {
            Some(child) => (self.end_offset(child) == start).then_some(child),
            None => (self.start_offset(first) == start).then_some(first),
        }
    }
}

impl Tree {
    /// Whether every element has the same parent in both trees. Equality leaves parents out
    /// because they follow from kinds and ends, so this checks code that stores them by hand.
    pub fn same_parents(&self, other: &Tree) -> bool {
        self.len == other.len
            && (0..self.len).all(|index| self.parent(index) == other.parent(index))
    }
}

impl PartialEq for Tree {
    fn eq(&self, other: &Tree) -> bool {
        if Arc::ptr_eq(&self.columns, &other.columns) && self.first == other.first {
            return self.len == other.len;
        }
        self.len == other.len
            && self.text() == other.text()
            && (0..self.len).all(|index| {
                self.kind(index) == other.kind(index)
                    && self.end(index) == other.end(index)
                    && self.start_offset(index) == other.start_offset(index)
            })
    }
}

impl Eq for Tree {}

impl fmt::Debug for Tree {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:#?}",
            SyntaxNode::new_root(Arc::new(self.clone()))
        )
    }
}

/// Builds a [`Tree`] from a parser's events. A node is only known to wrap the elements after a
/// checkpoint once the parser opens it there, so elements arrive in postorder (children before
/// their parent) and [`TreeBuilder::finish`] lays them out in preorder in one linear pass.
#[derive(Default)]
pub(crate) struct TreeBuilder {
    kinds: Vec<SyntaxKind>,
    starts: Vec<TextSize>,
    /// The postorder index of the first element of each element's subtree.
    firsts: Vec<u32>,
    /// Each open node's kind and the postorder index of its first element.
    open: Vec<(SyntaxKind, u32)>,
    offset: TextSize,
}

/// A position in the element list where a node can later be opened around what follows.
#[derive(Clone, Copy)]
pub(crate) struct Checkpoint(u32);

impl TreeBuilder {
    pub(crate) fn token(&mut self, kind: SyntaxKind, len: TextSize) {
        let index = self.next_index();
        self.kinds.push(kind);
        self.starts.push(self.offset);
        self.firsts.push(index);
        self.offset += len;
    }

    pub(crate) fn start_node(&mut self, kind: SyntaxKind) {
        let first = self.next_index();
        self.open.push((kind, first));
    }

    pub(crate) fn start_node_at(&mut self, Checkpoint(first): Checkpoint, kind: SyntaxKind) {
        assert!(
            first <= self.next_index(),
            "checkpoint is past the current element"
        );
        if let Some(&(_, parent_first)) = self.open.last() {
            assert!(
                first >= parent_first,
                "checkpoint is outside the current node"
            );
        }
        self.open.push((kind, first));
    }

    pub(crate) fn finish_node(&mut self) {
        let (kind, first) = self.open.pop().expect("finish_node without an open node");
        let start = match self.starts.get(first as usize) {
            Some(&start) => start,
            None => self.offset,
        };
        self.kinds.push(kind);
        self.starts.push(start);
        self.firsts.push(first);
    }

    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.next_index())
    }

    fn next_index(&self) -> u32 {
        u32::try_from(self.kinds.len()).expect("a syntax tree has fewer than 2^32 elements")
    }

    /// Element `p` in postorder covers `firsts[p]..=p`, and the elements before it inside its
    /// parent are exactly its earlier siblings' subtrees, which come before it in preorder too.
    /// So `pre(p) = pre(parent) + 1 + (firsts[p] - firsts[parent])`, and parents come later in
    /// postorder, so one backward pass places everything.
    pub(crate) fn finish(self, text: &str) -> Tree {
        assert!(self.open.is_empty(), "every node is finished");
        assert_eq!(
            usize::from(self.offset),
            text.len(),
            "the tokens cover the text"
        );
        let count = self.kinds.len();
        assert!(
            count > 0 && self.firsts[count - 1] == 0,
            "a parse builds exactly one root node"
        );
        let mut parents = vec![NO_PARENT; count];
        for (node, &first) in self.firsts.iter().enumerate() {
            let mut child = node;
            while child > first as usize {
                child -= 1;
                parents[child] = node as u32;
                child = self.firsts[child] as usize;
            }
        }
        let mut preorder = vec![0u32; count];
        for element in (0..count - 1).rev() {
            let parent = parents[element] as usize;
            preorder[element] = preorder[parent] + 1 + (self.firsts[element] - self.firsts[parent]);
        }
        let mut kinds = vec![SyntaxKind::ERROR; count];
        let mut starts = vec![TextSize::new(0); count];
        let mut ends = vec![0u32; count];
        let mut preorder_parents = vec![NO_PARENT; count];
        for element in 0..count {
            let position = preorder[element] as usize;
            kinds[position] = self.kinds[element];
            starts[position] = self.starts[element];
            ends[position] = preorder[element] + (element as u32 - self.firsts[element]) + 1;
            if let Some(&parent) = parents.get(element).filter(|&&parent| parent != NO_PARENT) {
                preorder_parents[position] = preorder[parent as usize];
            }
        }
        Tree::from_columns(Columns {
            text: text.into(),
            kinds: kinds.into(),
            starts: starts.into(),
            ends: ends.into(),
            parents: preorder_parents.into(),
        })
    }
}

/// A tree of `kind` over `text` whose children are runs of top-level elements taken from other
/// trees, in order. Each run is an element range under its tree's root, so it holds whole
/// subtrees, and the runs' texts must add up to `text`.
pub(crate) fn splice(kind: SyntaxKind, text: &str, runs: &[(&Tree, Range<u32>)]) -> Tree {
    let count = 1 + runs
        .iter()
        .map(|(_, elements)| elements.len())
        .sum::<usize>();
    let mut kinds = Vec::with_capacity(count);
    let mut starts = Vec::with_capacity(count);
    let mut ends = Vec::with_capacity(count);
    let mut parents = Vec::with_capacity(count);
    kinds.push(kind);
    starts.push(TextSize::new(0));
    ends.push(u32::try_from(count).expect("a syntax tree has fewer than 2^32 elements"));
    parents.push(NO_PARENT);
    let mut offset = TextSize::new(0);
    for (tree, elements) in runs {
        if elements.is_empty() {
            continue;
        }
        let first_offset = tree.start_offset(elements.start);
        let base = kinds.len() as u32;
        let shift = |index: u32| index - elements.start + base;
        for element in elements.clone() {
            kinds.push(tree.kind(element));
            starts.push(tree.start_offset(element) - first_offset + offset);
            ends.push(shift(tree.end(element)));
            parents.push(match tree.parent(element) {
                Some(parent) if parent >= elements.start => shift(parent),
                _ => 0,
            });
        }
        offset += tree.end_offset(elements.end - 1) - first_offset;
    }
    assert_eq!(
        usize::from(offset),
        text.len(),
        "the spliced runs cover the text"
    );
    Tree::from_columns(Columns {
        text: text.into(),
        kinds: kinds.into(),
        starts: starts.into(),
        ends: ends.into(),
        parents: parents.into(),
    })
}

/// A node or a token, generic so the owned and borrowed cursors share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeOrToken<N, T> {
    Node(N),
    Token(T),
}

impl<N, T> NodeOrToken<N, T> {
    pub fn into_node(self) -> Option<N> {
        match self {
            NodeOrToken::Node(node) => Some(node),
            NodeOrToken::Token(_) => None,
        }
    }

    pub fn into_token(self) -> Option<T> {
        match self {
            NodeOrToken::Node(_) => None,
            NodeOrToken::Token(token) => Some(token),
        }
    }

    pub fn as_node(&self) -> Option<&N> {
        match self {
            NodeOrToken::Node(node) => Some(node),
            NodeOrToken::Token(_) => None,
        }
    }

    pub fn as_token(&self) -> Option<&T> {
        match self {
            NodeOrToken::Node(_) => None,
            NodeOrToken::Token(token) => Some(token),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Next,
    Prev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WalkEvent<T> {
    Enter(T),
    Leave(T),
}

impl<T> WalkEvent<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> WalkEvent<U> {
        match self {
            WalkEvent::Enter(value) => WalkEvent::Enter(f(value)),
            WalkEvent::Leave(value) => WalkEvent::Leave(f(value)),
        }
    }
}

/// The tokens at an offset: none in an empty tree, one inside a token, and two on the
/// boundary between tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAtOffset<T> {
    None,
    Single(T),
    Between(T, T),
}

impl<T> TokenAtOffset<T> {
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> TokenAtOffset<U> {
        match self {
            TokenAtOffset::None => TokenAtOffset::None,
            TokenAtOffset::Single(token) => TokenAtOffset::Single(f(token)),
            TokenAtOffset::Between(left, right) => TokenAtOffset::Between(f(left), f(right)),
        }
    }

    pub fn left_biased(self) -> Option<T> {
        match self {
            TokenAtOffset::None => None,
            TokenAtOffset::Single(token) | TokenAtOffset::Between(token, _) => Some(token),
        }
    }

    pub fn right_biased(self) -> Option<T> {
        match self {
            TokenAtOffset::None => None,
            TokenAtOffset::Single(token) | TokenAtOffset::Between(_, token) => Some(token),
        }
    }
}

impl<T> Iterator for TokenAtOffset<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        match std::mem::replace(self, TokenAtOffset::None) {
            TokenAtOffset::None => None,
            TokenAtOffset::Single(token) => Some(token),
            TokenAtOffset::Between(left, right) => {
                *self = TokenAtOffset::Single(right);
                Some(left)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = match self {
            TokenAtOffset::None => 0,
            TokenAtOffset::Single(_) => 1,
            TokenAtOffset::Between(_, _) => 2,
        };
        (count, Some(count))
    }
}

impl<T> ExactSizeIterator for TokenAtOffset<T> {}

/// A node of a shared tree. Cloning one bumps the tree's reference count and nothing else.
#[derive(Clone)]
pub struct SyntaxNode {
    tree: Arc<Tree>,
    index: u32,
}

/// A token of a shared tree.
#[derive(Clone)]
pub struct SyntaxToken {
    tree: Arc<Tree>,
    index: u32,
}

pub type SyntaxElement = NodeOrToken<SyntaxNode, SyntaxToken>;

fn element(tree: &Arc<Tree>, index: u32) -> SyntaxElement {
    if tree.is_node(index) {
        NodeOrToken::Node(SyntaxNode {
            tree: tree.clone(),
            index,
        })
    } else {
        NodeOrToken::Token(SyntaxToken {
            tree: tree.clone(),
            index,
        })
    }
}

/// The element's index in its tree, for building trees out of runs of another's elements.
pub(crate) fn element_index(element: &SyntaxElement) -> u32 {
    match element {
        NodeOrToken::Node(node) => node.index,
        NodeOrToken::Token(token) => token.index,
    }
}

fn node(tree: &Arc<Tree>, index: u32) -> SyntaxNode {
    SyntaxNode {
        tree: tree.clone(),
        index,
    }
}

fn token(tree: &Arc<Tree>, index: u32) -> SyntaxToken {
    SyntaxToken {
        tree: tree.clone(),
        index,
    }
}

impl SyntaxNode {
    pub fn new_root(tree: Arc<Tree>) -> SyntaxNode {
        SyntaxNode { tree, index: 0 }
    }

    pub fn tree(&self) -> &Arc<Tree> {
        &self.tree
    }

    /// This node as a borrowed cursor, for walks that should not touch the reference count.
    pub fn node_ref(&self) -> NodeRef<'_> {
        NodeRef {
            tree: &self.tree,
            index: self.index,
        }
    }

    /// The owned cursor for a borrowed one into this node's tree.
    ///
    /// # Panics
    ///
    /// If `node` belongs to another tree.
    pub fn at(&self, node: NodeRef<'_>) -> SyntaxNode {
        assert!(
            std::ptr::eq(node.tree, &*self.tree),
            "a node of another tree"
        );
        self::node(&self.tree, node.index)
    }

    /// The node as a tree of its own, with offsets counted from its start.
    pub fn subtree(&self) -> Tree {
        self.tree.subtree(self.node_ref())
    }

    /// The node as a tree with columns of its own; see [`Tree::detach`].
    pub fn detached_subtree(&self) -> Tree {
        self.subtree().detach()
    }

    pub fn kind(&self) -> SyntaxKind {
        self.tree.kind(self.index)
    }

    pub fn text_range(&self) -> TextRange {
        self.tree.range(self.index)
    }

    pub fn text(&self) -> &str {
        self.tree.slice(self.text_range())
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        Some(node(&self.tree, self.tree.parent(self.index)?))
    }

    /// This node and then its ancestors, innermost first.
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        iter::successors(Some(self.clone()), SyntaxNode::parent)
    }

    pub fn children(&self) -> SyntaxNodeChildren {
        SyntaxNodeChildren(self.children_with_tokens())
    }

    pub fn children_with_tokens(&self) -> SyntaxElementChildren {
        SyntaxElementChildren {
            tree: self.tree.clone(),
            next: self.index + 1,
            end: self.tree.end(self.index),
        }
    }

    pub fn first_child(&self) -> Option<SyntaxNode> {
        self.children().next()
    }

    pub fn last_child(&self) -> Option<SyntaxNode> {
        self.children().last()
    }

    pub fn first_child_or_token(&self) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.first_child_or_token(self.index)?,
        ))
    }

    pub fn last_child_or_token(&self) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.last_child_or_token(self.index)?,
        ))
    }

    pub fn next_sibling(&self) -> Option<SyntaxNode> {
        self.siblings(Direction::Next).nth(1)
    }

    pub fn prev_sibling(&self) -> Option<SyntaxNode> {
        self.siblings(Direction::Prev).nth(1)
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.next_sibling_or_token(self.index)?,
        ))
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.prev_sibling_or_token(self.index)?,
        ))
    }

    /// This node and then its sibling nodes in `direction`.
    pub fn siblings(&self, direction: Direction) -> impl Iterator<Item = SyntaxNode> + use<> {
        let tree = self.tree.clone();
        let step = move |&index: &u32| match direction {
            Direction::Next => tree.next_sibling_or_token(index),
            Direction::Prev => tree.prev_sibling_or_token(index),
        };
        let tree = self.tree.clone();
        iter::successors(Some(self.index), step)
            .filter(move |&index| tree.is_node(index))
            .map({
                let tree = self.tree.clone();
                move |index| node(&tree, index)
            })
    }

    /// This node and then its siblings in `direction`, tokens included.
    pub fn siblings_with_tokens(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = SyntaxElement> + use<> {
        siblings_with_tokens(&self.tree, self.index, direction)
    }

    /// This node and every node under it, in preorder.
    pub fn descendants(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        let tree = self.tree.clone();
        (self.index..self.tree.end(self.index))
            .filter(move |&index| tree.is_node(index))
            .map({
                let tree = self.tree.clone();
                move |index| node(&tree, index)
            })
    }

    /// This node and every element under it, in preorder.
    pub fn descendants_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> + use<> {
        let tree = self.tree.clone();
        (self.index..self.tree.end(self.index)).map(move |index| element(&tree, index))
    }

    pub fn preorder(&self) -> Preorder {
        Preorder(Walk::new(&self.tree, self.index, false))
    }

    pub fn preorder_with_tokens(&self) -> PreorderWithTokens {
        PreorderWithTokens(Walk::new(&self.tree, self.index, true))
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        Some(token(&self.tree, self.tree.first_token(self.index)?))
    }

    pub fn last_token(&self) -> Option<SyntaxToken> {
        Some(token(&self.tree, self.tree.last_token(self.index)?))
    }

    /// # Panics
    ///
    /// If `offset` is outside the node.
    pub fn token_at_offset(&self, offset: TextSize) -> TokenAtOffset<SyntaxToken> {
        self.tree
            .token_at_offset(self.index, offset)
            .map(|index| token(&self.tree, index))
    }

    /// The innermost element that holds `range`.
    ///
    /// # Panics
    ///
    /// If `range` is outside the node.
    pub fn covering_element(&self, range: TextRange) -> SyntaxElement {
        assert!(
            self.text_range().contains_range(range),
            "covering_element: {range:?} is outside {:?}",
            self.text_range()
        );
        let mut index = self.index;
        while let Some(child) = self.tree.child_or_token_at_range(index, range) {
            index = child;
        }
        element(&self.tree, index)
    }

    pub fn child_or_token_at_range(&self, range: TextRange) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.child_or_token_at_range(self.index, range)?,
        ))
    }

    /// The node's position among its parent's children, tokens included.
    pub fn index(&self) -> usize {
        self.tree.index_in_parent(self.index)
    }
}

impl PartialEq for SyntaxNode {
    fn eq(&self, other: &SyntaxNode) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.tree, &other.tree)
    }
}

impl Eq for SyntaxNode {}

impl Hash for SyntaxNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.tree).hash(state);
        self.index.hash(state);
    }
}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !formatter.alternate() {
            return write!(formatter, "{:?}@{:?}", self.kind(), self.text_range());
        }
        let mut depth = 0usize;
        for event in self.preorder_with_tokens() {
            match event {
                WalkEvent::Enter(element) => {
                    write!(formatter, "{:indent$}", "", indent = depth * 2)?;
                    match element {
                        NodeOrToken::Node(node) => writeln!(formatter, "{node:?}")?,
                        NodeOrToken::Token(token) => writeln!(formatter, "{token:?}")?,
                    }
                    depth += 1;
                }
                WalkEvent::Leave(_) => depth -= 1,
            }
        }
        Ok(())
    }
}

impl fmt::Display for SyntaxNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}

impl SyntaxToken {
    pub fn tree(&self) -> &Arc<Tree> {
        &self.tree
    }

    pub fn kind(&self) -> SyntaxKind {
        self.tree.kind(self.index)
    }

    pub fn text_range(&self) -> TextRange {
        self.tree.range(self.index)
    }

    pub fn text(&self) -> &str {
        self.tree.slice(self.text_range())
    }

    /// The token as a borrowed value, for code that reads it without keeping it.
    pub fn token_ref(&self) -> TokenRef<'_> {
        token_ref(&self.tree, self.index)
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        Some(node(&self.tree, self.tree.parent(self.index)?))
    }

    /// The token's ancestors, innermost first.
    pub fn parent_ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        iter::successors(self.parent(), SyntaxNode::parent)
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.next_sibling_or_token(self.index)?,
        ))
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        Some(element(
            &self.tree,
            self.tree.prev_sibling_or_token(self.index)?,
        ))
    }

    /// This token and then its siblings in `direction`, nodes included.
    pub fn siblings_with_tokens(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = SyntaxElement> + use<> {
        siblings_with_tokens(&self.tree, self.index, direction)
    }

    pub fn next_token(&self) -> Option<SyntaxToken> {
        Some(token(&self.tree, self.tree.next_token(self.index)?))
    }

    pub fn prev_token(&self) -> Option<SyntaxToken> {
        Some(token(&self.tree, self.tree.prev_token(self.index)?))
    }

    /// The token's position among its parent's children, nodes included.
    pub fn index(&self) -> usize {
        self.tree.index_in_parent(self.index)
    }
}

impl PartialEq for SyntaxToken {
    fn eq(&self, other: &SyntaxToken) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.tree, &other.tree)
    }
}

impl Eq for SyntaxToken {}

impl Hash for SyntaxToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.tree).hash(state);
        self.index.hash(state);
    }
}

impl fmt::Debug for SyntaxToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}@{:?}", self.kind(), self.text_range())?;
        let text = self.text();
        if text.len() < 25 {
            return write!(formatter, " {text:?}");
        }
        let cut = (21..25)
            .find(|&index| text.is_char_boundary(index))
            .unwrap_or(text.len());
        write!(formatter, " {:?}", format!("{} ...", &text[..cut]))
    }
}

impl fmt::Display for SyntaxToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}

impl SyntaxElement {
    pub fn kind(&self) -> SyntaxKind {
        match self {
            NodeOrToken::Node(node) => node.kind(),
            NodeOrToken::Token(token) => token.kind(),
        }
    }

    pub fn text_range(&self) -> TextRange {
        match self {
            NodeOrToken::Node(node) => node.text_range(),
            NodeOrToken::Token(token) => token.text_range(),
        }
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        match self {
            NodeOrToken::Node(node) => node.parent(),
            NodeOrToken::Token(token) => token.parent(),
        }
    }

    /// The element's ancestors, starting at the element itself for a node and at the parent
    /// for a token.
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        let first = match self {
            NodeOrToken::Node(node) => Some(node.clone()),
            NodeOrToken::Token(token) => token.parent(),
        };
        iter::successors(first, SyntaxNode::parent)
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        match self {
            NodeOrToken::Node(node) => node.next_sibling_or_token(),
            NodeOrToken::Token(token) => token.next_sibling_or_token(),
        }
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        match self {
            NodeOrToken::Node(node) => node.prev_sibling_or_token(),
            NodeOrToken::Token(token) => token.prev_sibling_or_token(),
        }
    }

    pub fn index(&self) -> usize {
        match self {
            NodeOrToken::Node(node) => node.index(),
            NodeOrToken::Token(token) => token.index(),
        }
    }
}

impl From<SyntaxNode> for SyntaxElement {
    fn from(node: SyntaxNode) -> SyntaxElement {
        NodeOrToken::Node(node)
    }
}

impl From<SyntaxToken> for SyntaxElement {
    fn from(token: SyntaxToken) -> SyntaxElement {
        NodeOrToken::Token(token)
    }
}

impl fmt::Display for SyntaxElement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeOrToken::Node(node) => node.fmt(formatter),
            NodeOrToken::Token(token) => token.fmt(formatter),
        }
    }
}

fn siblings_with_tokens(
    tree: &Arc<Tree>,
    index: u32,
    direction: Direction,
) -> impl Iterator<Item = SyntaxElement> + use<> {
    let stepper = tree.clone();
    let step = move |&index: &u32| match direction {
        Direction::Next => stepper.next_sibling_or_token(index),
        Direction::Prev => stepper.prev_sibling_or_token(index),
    };
    let tree = tree.clone();
    iter::successors(Some(index), step).map(move |index| element(&tree, index))
}

/// The child nodes of a [`SyntaxNode`].
#[derive(Clone)]
pub struct SyntaxNodeChildren(SyntaxElementChildren);

impl Iterator for SyntaxNodeChildren {
    type Item = SyntaxNode;

    fn next(&mut self) -> Option<SyntaxNode> {
        let children = &mut self.0;
        while children.next < children.end {
            let index = children.next;
            children.next = children.tree.end(index);
            if children.tree.is_node(index) {
                return Some(node(&children.tree, index));
            }
        }
        None
    }
}

/// The children of a [`SyntaxNode`], tokens included.
#[derive(Clone)]
pub struct SyntaxElementChildren {
    tree: Arc<Tree>,
    next: u32,
    end: u32,
}

impl Iterator for SyntaxElementChildren {
    type Item = SyntaxElement;

    fn next(&mut self) -> Option<SyntaxElement> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next = self.tree.end(index);
        Some(element(&self.tree, index))
    }
}

/// A preorder walk with enter and leave events. Every element is entered and then left, a
/// token immediately.
struct Walk {
    tree: Arc<Tree>,
    end: u32,
    next: u32,
    open: Vec<u32>,
    with_tokens: bool,
}

impl Walk {
    fn new(tree: &Arc<Tree>, root: u32, with_tokens: bool) -> Walk {
        Walk {
            tree: tree.clone(),
            end: tree.end(root),
            next: root,
            open: Vec::new(),
            with_tokens,
        }
    }

    fn next(&mut self) -> Option<WalkEvent<u32>> {
        loop {
            if let Some(&top) = self.open.last()
                && self.next >= self.tree.end(top)
            {
                self.open.pop();
                return Some(WalkEvent::Leave(top));
            }
            if self.next >= self.end {
                return None;
            }
            let index = self.next;
            self.next += 1;
            if self.with_tokens || self.tree.is_node(index) {
                self.open.push(index);
                return Some(WalkEvent::Enter(index));
            }
        }
    }

    /// Skips what is left of the innermost open element, so its leave event comes next.
    fn skip_subtree(&mut self) {
        if let Some(&top) = self.open.last() {
            self.next = self.tree.end(top);
        }
    }
}

pub struct Preorder(Walk);

impl Preorder {
    pub fn skip_subtree(&mut self) {
        self.0.skip_subtree();
    }
}

impl Iterator for Preorder {
    type Item = WalkEvent<SyntaxNode>;

    fn next(&mut self) -> Option<WalkEvent<SyntaxNode>> {
        let tree = self.0.tree.clone();
        Some(self.0.next()?.map(|index| node(&tree, index)))
    }
}

pub struct PreorderWithTokens(Walk);

impl PreorderWithTokens {
    pub fn skip_subtree(&mut self) {
        self.0.skip_subtree();
    }
}

impl Iterator for PreorderWithTokens {
    type Item = WalkEvent<SyntaxElement>;

    fn next(&mut self) -> Option<WalkEvent<SyntaxElement>> {
        let tree = self.0.tree.clone();
        Some(self.0.next()?.map(|index| element(&tree, index)))
    }
}

/// A node borrowed from a tree: navigating one copies two words and touches no reference count.
#[derive(Clone, Copy)]
pub struct NodeRef<'a> {
    tree: &'a Tree,
    index: u32,
}

/// A token borrowed from a tree. It reads nothing until asked: most tokens a child walk passes
/// over are only checked for their kind, so slicing every token's text up front would cost more
/// than the walk itself.
#[derive(Clone, Copy)]
pub struct TokenRef<'a> {
    tree: &'a Tree,
    index: u32,
}

pub type ElementRef<'a> = NodeOrToken<NodeRef<'a>, TokenRef<'a>>;

fn token_ref(tree: &Tree, index: u32) -> TokenRef<'_> {
    TokenRef { tree, index }
}

impl<'a> TokenRef<'a> {
    #[inline]
    pub fn kind(self) -> SyntaxKind {
        self.tree.kind(self.index)
    }

    #[inline]
    pub fn range(self) -> TextRange {
        self.tree.range(self.index)
    }

    #[inline]
    pub fn text(self) -> &'a str {
        self.tree.slice(self.range())
    }
}

impl PartialEq for TokenRef<'_> {
    fn eq(&self, other: &TokenRef<'_>) -> bool {
        self.kind() == other.kind() && self.range() == other.range() && self.text() == other.text()
    }
}

impl Eq for TokenRef<'_> {}

impl fmt::Debug for TokenRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenRef")
            .field("kind", &self.kind())
            .field("text", &self.text())
            .field("range", &self.range())
            .finish()
    }
}

fn element_ref(tree: &Tree, index: u32) -> ElementRef<'_> {
    if tree.is_node(index) {
        NodeOrToken::Node(NodeRef { tree, index })
    } else {
        NodeOrToken::Token(token_ref(tree, index))
    }
}

impl<'a> NodeRef<'a> {
    pub fn tree(self) -> &'a Tree {
        self.tree
    }

    pub fn kind(self) -> SyntaxKind {
        self.tree.kind(self.index)
    }

    pub fn range(self) -> TextRange {
        self.tree.range(self.index)
    }

    pub fn text(self) -> &'a str {
        self.tree.slice(self.range())
    }

    pub fn parent(self) -> Option<NodeRef<'a>> {
        Some(NodeRef {
            tree: self.tree,
            index: self.tree.parent(self.index)?,
        })
    }

    /// Children in source order, tokens included.
    pub fn children(self) -> ChildrenRef<'a> {
        ChildrenRef {
            tree: self.tree,
            parent: self.index,
            next: self.index + 1,
            end: self.tree.end(self.index),
        }
    }

    pub fn child_nodes(self) -> impl DoubleEndedIterator<Item = NodeRef<'a>> + use<'a> {
        self.children().filter_map(ElementRef::into_node)
    }

    pub fn child_tokens(self) -> impl DoubleEndedIterator<Item = TokenRef<'a>> + use<'a> {
        self.children().filter_map(ElementRef::into_token)
    }

    /// The kinds of the node's children, tokens included.
    pub fn child_kinds(self) -> impl Iterator<Item = SyntaxKind> + use<'a> {
        let tree = self.tree;
        let mut next = self.index + 1;
        let end = tree.end(self.index);
        iter::from_fn(move || {
            (next < end).then(|| {
                let kind = tree.kind(next);
                next = tree.end(next);
                kind
            })
        })
    }

    pub fn has_child(self, kind: SyntaxKind) -> bool {
        kind.is_node() && self.child_kinds().any(|child| child == kind)
    }

    /// The text of a node that is a single token, such as most names.
    pub fn single_token_text(self) -> Option<&'a str> {
        let child = self.tree.first_child_or_token(self.index)?;
        (!self.tree.is_node(child) && self.tree.end(child) == self.tree.end(self.index))
            .then(|| self.tree.slice(self.tree.range(child)))
    }

    /// The first token found by descending into first children, so an empty first child node
    /// ends the search.
    pub fn first_token(self) -> Option<TokenRef<'a>> {
        Some(token_ref(self.tree, self.tree.first_token(self.index)?))
    }

    /// Every node under this one, this one included, in preorder.
    pub fn descendants(self) -> impl Iterator<Item = NodeRef<'a>> + use<'a> {
        let tree = self.tree;
        (self.index..tree.end(self.index))
            .filter(move |&index| tree.is_node(index))
            .map(move |index| NodeRef { tree, index })
    }

    /// The first node in preorder under this one, this one included, with `kind` and exactly
    /// `range`. Starts never decrease in preorder, so every element starting where `range`
    /// starts sits in one run that a binary search finds.
    pub fn find(self, kind: SyntaxKind, range: TextRange) -> Option<NodeRef<'a>> {
        let tree = self.tree;
        let first = self.index;
        let end = tree.end(self.index);
        let run = first + tree.partition_starts(first, end, |start| start < range.start());
        (run..end)
            .take_while(|&index| tree.start_offset(index) == range.start())
            .find(|&index| tree.kind(index) == kind && tree.end_offset(index) == range.end())
            .map(|index| NodeRef { tree, index })
    }

    /// Every element under this one, this one included, in preorder, tokens included.
    pub fn descendants_with_tokens(self) -> impl Iterator<Item = ElementRef<'a>> + use<'a> {
        let tree = self.tree;
        (self.index..tree.end(self.index)).map(move |index| element_ref(tree, index))
    }

    /// Whether `element` is this node or lies under it, from the elements' positions in the
    /// tree, so an empty element on this node's boundary is placed exactly.
    pub fn encloses(self, element: ElementRef<'_>) -> bool {
        let (tree, index) = match element {
            NodeOrToken::Node(node) => (node.tree, node.index),
            NodeOrToken::Token(token) => (token.tree, token.index),
        };
        std::ptr::eq(tree, self.tree) && (self.index..self.tree.end(self.index)).contains(&index)
    }

    /// Every token under this node, in source order.
    pub fn tokens(self) -> impl Iterator<Item = TokenRef<'a>> + use<'a> {
        let tree = self.tree;
        (self.index..tree.end(self.index))
            .filter(move |&index| !tree.is_node(index))
            .map(move |index| token_ref(tree, index))
    }
}

impl PartialEq for NodeRef<'_> {
    fn eq(&self, other: &NodeRef<'_>) -> bool {
        self.index == other.index && std::ptr::eq(self.tree, other.tree)
    }
}

impl Eq for NodeRef<'_> {}

impl fmt::Debug for NodeRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}@{:?}", self.kind(), self.range())
    }
}

impl<'a> ElementRef<'a> {
    pub fn kind(self) -> SyntaxKind {
        match self {
            NodeOrToken::Node(node) => node.kind(),
            NodeOrToken::Token(token) => token.kind(),
        }
    }

    pub fn range(self) -> TextRange {
        match self {
            NodeOrToken::Node(node) => node.range(),
            NodeOrToken::Token(token) => token.range(),
        }
    }
}

/// The children of a [`NodeRef`], from either end.
#[derive(Clone)]
pub struct ChildrenRef<'a> {
    tree: &'a Tree,
    parent: u32,
    next: u32,
    /// One past the last element of the last child not yet yielded.
    end: u32,
}

impl<'a> Iterator for ChildrenRef<'a> {
    type Item = ElementRef<'a>;

    fn next(&mut self) -> Option<ElementRef<'a>> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next = self.tree.end(index);
        Some(element_ref(self.tree, index))
    }
}

impl DoubleEndedIterator for ChildrenRef<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.tree.child_containing(self.parent, self.end - 1);
        self.end = index;
        Some(element_ref(self.tree, index))
    }
}
