//! Lossless R syntax trees: every byte, including trivia and `#:` annotations,
//! reprints exactly, and malformed input still yields a tree with local `ERROR`
//! nodes.

pub mod ast;
pub mod kind;
pub mod literate;
pub mod native_values;
pub mod testing;

mod lexer;
mod parser;
mod reparse;

pub use {
    kind::SyntaxKind,
    lexer::{Token, is_syntactic_name, lex},
    parser::parse,
    rowan::{TextRange, TextSize},
};
use {
    rowan::{GreenNode, GreenNodeData, Language, NodeOrToken, WalkEvent},
    std::{mem, ptr, sync::Arc},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RLanguage {}

impl rowan::Language for RLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        assert!(
            raw.0 <= SyntaxKind::ERROR as u16,
            "invalid syntax kind {}",
            raw.0
        );
        // SAFETY: `SyntaxKind` is `repr(u16)` with dense discriminants `0..=ERROR`,
        // and the bound was just checked.
        unsafe { mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<RLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<RLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<RLanguage>;
pub type SyntaxNodeChildren = rowan::SyntaxNodeChildren<RLanguage>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub range: TextRange,
    /// Raised by the `#:` annotation grammar; the formatter keys on this to keep
    /// the block verbatim instead of refusing the file.
    pub in_annotation: bool,
    /// Caused by the input ending too early: an opener it never closes, or a construct the end
    /// of the input cuts off. A console reads more input instead of reporting it.
    pub incomplete: bool,
}

impl SyntaxError {
    pub fn new(message: impl Into<String>, range: TextRange) -> SyntaxError {
        SyntaxError {
            message: message.into(),
            range,
            in_annotation: false,
            incomplete: false,
        }
    }
}

/// R's console rule: input is unfinished when every syntax error in it says that it ended too
/// early, so the console reads another line instead of reporting an error. A broken `#:`
/// comment is only a comment to R, so it never holds input back.
pub fn input_is_unfinished(text: &str) -> bool {
    let parse = parse(text);
    let end = TextSize::of(text.trim_end());
    let mut errors = parse
        .errors()
        .iter()
        .filter(|error| !error.in_annotation)
        .peekable();
    errors.peek().is_some() && errors.all(|error| error.incomplete || error.range.start() >= end)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    green: GreenNode,
    errors: Arc<Vec<SyntaxError>>,
}

impl Parse {
    pub fn new(green: GreenNode, mut errors: Vec<SyntaxError>) -> Parse {
        // Canonical order, so a splice reparse compares equal to a full parse.
        errors.sort_by_key(|error| (error.range.start(), error.range.end()));
        Parse {
            green,
            errors: Arc::new(errors),
        }
    }

    pub fn green(&self) -> &GreenNode {
        &self.green
    }

    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    pub fn text(&self) -> String {
        self.syntax_node().text().to_string()
    }

    pub fn debug_dump(&self) -> String {
        let mut out = String::new();
        dump_node(&mut out, &self.syntax_node(), 0);
        for error in self.errors.iter() {
            out.push_str(&format!(
                "err: {}..{}: {}\n",
                u32::from(error.range.start()),
                u32::from(error.range.end()),
                error.message
            ));
        }
        out
    }
}

/// The plain R program an annotated one means: every inline annotation removed,
/// which is a binding's or parameter's `: TYPE`, a function's `<...>` type
/// parameters, its `-> TYPE`, and the `@Name` of `@Name(value)`. An inline type
/// definition becomes its `#: @type` or `#: @alias` comment, so the erased program
/// still declares the type. Comments and `#:` annotations are kept.
pub fn erase_inline_annotations(parse: &Parse) -> String {
    let root = parse.syntax_node();
    let mut out = String::with_capacity(usize::from(root.text_range().len()));
    let mut preorder = root.preorder_with_tokens();
    while let Some(event) = preorder.next() {
        let WalkEvent::Enter(element) = event else {
            continue;
        };
        match element {
            NodeOrToken::Token(token) => out.push_str(token.text()),
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::TYPE_DEFINITION => {
                // A paired definition already has its comment spelling right above it.
                if paired_comment_definition(&node).is_none() {
                    out.push_str(&type_definition_comment(&node));
                }
                preorder.skip_subtree();
            }
            NodeOrToken::Node(node) if is_inline_annotation(&node) => preorder.skip_subtree(),
            NodeOrToken::Node(_) => {}
        }
    }
    out
}

/// `type Name<T> = TYPE` as `#: @type Name<T> {TYPE}`, each further line of a
/// multi-line type behind its own `#: ` marker.
fn type_definition_comment(node: &SyntaxNode) -> String {
    let mut out = String::from("#: @");
    let mut after_equals = false;
    for element in node.children_with_tokens() {
        match element {
            NodeOrToken::Token(token) => match token.kind() {
                SyntaxKind::EQ => {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                    out.push('{');
                    after_equals = true;
                }
                SyntaxKind::IDENT => out.push_str(token.text()),
                SyntaxKind::COMMENT => out.push_str(token.text()),
                SyntaxKind::NEWLINE => out.push_str("\n#: "),
                // Indentation on a continued line is kept; other spacing collapses, and the
                // type opens the braces directly.
                SyntaxKind::WHITESPACE if out.ends_with("\n#: ") => out.push_str(token.text()),
                _ if out.ends_with([' ', '{']) => {}
                _ => out.push(' '),
            },
            NodeOrToken::Node(child) => {
                for token in child
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    out.push_str(token.text());
                    if token.kind() == SyntaxKind::NEWLINE {
                        out.push_str("#: ");
                    }
                }
            }
        }
    }
    if after_equals {
        out.push('}');
    }
    out
}

/// The `#:` block directly above an inline type definition when it defines the same
/// name: then the two are one definition written two ways, not a duplicate.
pub fn paired_comment_definition(definition: &SyntaxNode) -> Option<SyntaxNode> {
    let name = definition_name(definition)?;
    let mut newlines = 0;
    let mut previous = definition.prev_sibling_or_token();
    while let Some(element) = previous {
        match element.kind() {
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::NEWLINE if newlines == 0 => newlines += 1,
            SyntaxKind::ANNOTATION => {
                let comment = element.into_node()?;
                let defines = comment
                    .children()
                    .filter(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
                    .filter(|directive| {
                        directive
                            .children_with_tokens()
                            .nth(1)
                            .and_then(|element| element.into_token())
                            .is_some_and(|word| matches!(word.text(), "type" | "alias"))
                    })
                    .any(|directive| definition_name(&directive).as_deref() == Some(name.as_str()));
                return defines.then_some(comment);
            }
            _ => return None,
        }
        previous = element.prev_sibling_or_token();
    }
    None
}

/// The name a `#: @type` or `@alias` directive or an inline type definition declares.
fn definition_name(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::NAME)
        .and_then(ast::Name::cast)
        .and_then(|name| name.text())
}

/// Inline annotation syntax: R's own grammar has no reading for these nodes.
pub fn is_inline_annotation(node: &SyntaxNode) -> bool {
    is_green_inline_annotation(node.kind(), node.parent().map(|parent| parent.kind()))
}

/// [`is_inline_annotation`] from a node's kind and its parent's.
pub fn is_green_inline_annotation(kind: SyntaxKind, parent: Option<SyntaxKind>) -> bool {
    match kind {
        SyntaxKind::TYPE_ASCRIPTION
        | SyntaxKind::RETURN_TYPE
        | SyntaxKind::TYPE_DEFINITION
        | SyntaxKind::NEW_TYPE
        | SyntaxKind::PURE_MARKER => true,
        SyntaxKind::TYPE_BINDER_LIST => parent == Some(SyntaxKind::FUNCTION_DEF),
        _ => false,
    }
}

/// A node of the immutable green tree. Unlike a [`SyntaxNode`], reading one allocates
/// nothing, because it carries neither a parent link nor an offset.
#[derive(Clone, Copy)]
pub struct GreenRef<'a>(&'a GreenNodeData);

impl<'a> GreenRef<'a> {
    /// The green node behind a red one, for asking the green predicates about it:
    /// `GreenRef::new(&node.green())`.
    pub fn new(green: &'a GreenNodeData) -> GreenRef<'a> {
        GreenRef(green)
    }

    pub fn kind(self) -> SyntaxKind {
        RLanguage::kind_from_raw(self.0.kind())
    }

    pub fn child_nodes(self) -> impl Iterator<Item = GreenRef<'a>> {
        self.0
            .children()
            .filter_map(|child| child.into_node().map(GreenRef))
    }

    /// The kinds of the node's children, tokens included.
    pub fn child_kinds(self) -> impl Iterator<Item = SyntaxKind> + 'a {
        self.0.children().map(|child| match child {
            NodeOrToken::Node(node) => RLanguage::kind_from_raw(node.kind()),
            NodeOrToken::Token(token) => RLanguage::kind_from_raw(token.kind()),
        })
    }

    pub fn has_child(self, kind: SyntaxKind) -> bool {
        self.child_nodes().any(|child| child.kind() == kind)
    }

    /// The text of a node that is a single token, such as most names.
    pub fn single_token_text(self) -> Option<&'a str> {
        let mut children = self.0.children();
        match (children.next(), children.next()) {
            (Some(NodeOrToken::Token(token)), None) => Some(token.text()),
            _ => None,
        }
    }
}

/// Every node under `root`, `root` included, that `wanted` accepts, in preorder, exactly as
/// `root.descendants().filter(..)` would yield them. `wanted` sees each node and its parent
/// (`None` for `root`). The search reads the green tree, so a full walk allocates nothing
/// and only the matches and their ancestors become [`SyntaxNode`]s, which makes a search
/// for rare kinds several times cheaper than a red-tree walk.
pub fn matching_nodes(
    root: &SyntaxNode,
    mut wanted: impl FnMut(GreenRef<'_>, Option<GreenRef<'_>>) -> bool,
) -> Vec<SyntaxNode> {
    let root_green = root.green();
    let mut matches = Vec::new();
    if wanted(GreenRef(&root_green), None) {
        matches.push(root.clone());
    }
    // One frame per open node: its remaining children, where the next one starts, and
    // the red node once a match beneath it needed it.
    let mut stack = vec![GreenFrame {
        green: &root_green,
        children: root_green.children(),
        index: 0,
        start: root.text_range().start(),
        next_start: root.text_range().start(),
        red: Some(root.clone()),
    }];
    while let Some(top) = stack.last_mut() {
        let Some(child) = top.children.next() else {
            stack.pop();
            continue;
        };
        let start = top.next_start;
        top.next_start += child.text_len();
        let NodeOrToken::Node(green) = child else {
            continue;
        };
        let parent = top.green;
        let index = top.children_seen();
        let accepted = wanted(GreenRef(green), Some(GreenRef(parent)));
        stack.push(GreenFrame {
            green,
            children: green.children(),
            index,
            start,
            next_start: start,
            red: None,
        });
        if accepted {
            // Every green node here was reached through its parent's children, so its red
            // node exists; a miss is a broken tree, not an input the caller can handle.
            let Some(node) = materialize(&mut stack) else {
                panic!("matching_nodes: a green node at {start:?} has no red counterpart");
            };
            matches.push(node);
        }
    }
    matches
}

/// One step of [`walk_green`].
#[derive(Clone, Copy)]
pub enum GreenEvent<'a> {
    /// A node, with its parent (`None` for the root) and its range.
    Enter {
        node: GreenRef<'a>,
        parent: Option<GreenRef<'a>>,
        range: TextRange,
    },
    /// The end of a node, after its last child.
    Leave(GreenRef<'a>),
    Token {
        kind: SyntaxKind,
        text: &'a str,
        range: TextRange,
    },
}

/// The tree under `root`, `root` included, in preorder, read from the green tree, so the
/// walk builds no [`SyntaxNode`]s. Pair it with [`node_at`] to build red nodes only for the
/// few sites a pass needs.
pub fn walk_green(root: &SyntaxNode, mut visit: impl FnMut(GreenEvent<'_>)) {
    let root_green = root.green();
    visit(GreenEvent::Enter {
        node: GreenRef(&root_green),
        parent: None,
        range: root.text_range(),
    });
    let mut stack = vec![(
        &*root_green,
        root_green.children(),
        root.text_range().start(),
    )];
    while let Some((parent, children, next_start)) = stack.last_mut() {
        let Some(child) = children.next() else {
            visit(GreenEvent::Leave(GreenRef(parent)));
            stack.pop();
            continue;
        };
        let start = *next_start;
        *next_start += child.text_len();
        let range = TextRange::at(start, child.text_len());
        match child {
            NodeOrToken::Node(green) => {
                visit(GreenEvent::Enter {
                    node: GreenRef(green),
                    parent: Some(GreenRef(parent)),
                    range,
                });
                stack.push((green, green.children(), start));
            }
            NodeOrToken::Token(token) => visit(GreenEvent::Token {
                kind: RLanguage::kind_from_raw(token.kind()),
                text: token.text(),
                range,
            }),
        }
    }
}

/// The first node in preorder under `root` with this kind and exactly this range. The search
/// only descends into children whose range contains `range`, so it touches one path of the
/// tree, plus the neighbors of an empty range.
pub fn node_at(root: &SyntaxNode, kind: SyntaxKind, range: TextRange) -> Option<SyntaxNode> {
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if node.kind() == kind && node.text_range() == range {
            return Some(node);
        }
        let start = stack.len();
        stack.extend(
            node.children()
                .filter(|child| child.text_range().contains_range(range)),
        );
        stack[start..].reverse();
    }
    None
}

/// Every token under `node` in source order, read from the green tree.
pub fn for_each_token(node: &SyntaxNode, mut visit: impl FnMut(SyntaxKind, &str)) {
    let green = node.green();
    let mut stack = vec![green.children()];
    while let Some(top) = stack.last_mut() {
        match top.next() {
            None => {
                stack.pop();
            }
            Some(NodeOrToken::Node(child)) => stack.push(child.children()),
            Some(NodeOrToken::Token(token)) => {
                visit(RLanguage::kind_from_raw(token.kind()), token.text())
            }
        }
    }
}

struct GreenFrame<'a> {
    green: &'a GreenNodeData,
    children: rowan::Children<'a>,
    /// The node's position among its parent's children, tokens included.
    index: usize,
    start: TextSize,
    next_start: TextSize,
    red: Option<SyntaxNode>,
}

impl GreenFrame<'_> {
    /// The position of the child `children` yielded last.
    fn children_seen(&self) -> usize {
        self.green.children().len() - self.children.len() - 1
    }
}

/// The red node for the top frame, building only the ancestors no earlier match built.
/// `None` only if a green child had no red counterpart under its red parent, which would
/// mean the two trees disagree.
fn materialize(stack: &mut [GreenFrame<'_>]) -> Option<SyntaxNode> {
    let built = stack.iter().rposition(|frame| frame.red.is_some())?;
    let mut parent = stack[built].red.clone()?;
    for frame in &mut stack[built + 1..] {
        let range = TextRange::at(frame.start, frame.green.text_len());
        // A binary search by range finds the child unless it is empty, when its range
        // is shared with a neighbor.
        let found = parent
            .child_or_token_at_range(range)
            .and_then(SyntaxElement::into_node)
            .filter(|node| !range.is_empty() && ptr::eq(&*node.green(), frame.green));
        let node = match found {
            Some(node) => node,
            None => parent
                .children_with_tokens()
                .nth(frame.index)
                .and_then(SyntaxElement::into_node)?,
        };
        frame.red = Some(node.clone());
        parent = node;
    }
    Some(parent)
}

/// `deleted` is a range in `old`'s text and `inserted` the replacement's length
/// in `new_text`; the result always equals `parse(new_text)`.
pub fn reparse(old: &Parse, new_text: &str, deleted: TextRange, inserted: TextSize) -> Parse {
    reparse::try_splice(old, new_text, deleted, inserted).unwrap_or_else(|| parse(new_text))
}

fn dump_node(out: &mut String, node: &SyntaxNode, depth: usize) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!(
        "{indent}{:?}@{:?}\n",
        node.kind(),
        node.text_range()
    ));
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(child) => dump_node(out, &child, depth + 1),
            NodeOrToken::Token(token) => {
                let indent = "  ".repeat(depth + 1);
                out.push_str(&format!(
                    "{indent}{:?}@{:?} {:?}\n",
                    token.kind(),
                    token.text_range(),
                    token.text()
                ));
            }
        }
    }
}
