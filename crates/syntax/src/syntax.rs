//! Lossless R syntax trees: every byte, including trivia and `#:` annotations,
//! reprints exactly, and malformed input still yields a tree with local `ERROR`
//! nodes.

pub mod ast;
pub mod highlight;
pub mod kind;
pub mod literate;
pub mod native_values;
pub mod testing;

mod lexer;
mod parser;
mod reparse;
mod tree;

use std::{borrow::Cow, sync::Arc};
pub use {
    kind::SyntaxKind,
    lexer::{NumberType, Token, is_syntactic_name, lex, split_number_suffix},
    parser::{infix_binding_power, parse},
    text_size::{TextLen, TextRange, TextSize},
    tree::{
        ChildrenRef, Direction, ElementRef, NodeOrToken, NodeRef, Preorder, PreorderWithTokens,
        SyntaxElement, SyntaxElementChildren, SyntaxNode, SyntaxNodeChildren, SyntaxToken,
        TokenAtOffset, TokenRef, Tree, WalkEvent,
    },
};

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
    /// What R's own parser reports for this error in place of the message.
    pub unexpected: Unexpected,
}

/// What R's own parser stops on when it meets a syntax error. R names that token ("unexpected
/// symbol"), not the construct ry's message describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unexpected {
    /// The token the parser stood on when it found the error.
    Token(TextRange),
    /// The end of the input.
    EndOfInput,
    /// Nothing: the error breaks one of ry's own rules for a construct that parsed in full,
    /// such as a tuple item with a name, so R's grammar has no report for it and ry's message
    /// is the only one.
    Nothing,
}

impl SyntaxError {
    pub fn new(message: impl Into<String>, range: TextRange) -> SyntaxError {
        SyntaxError {
            message: message.into(),
            range,
            in_annotation: false,
            incomplete: false,
            unexpected: Unexpected::EndOfInput,
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
    tree: Arc<Tree>,
    errors: Arc<Vec<SyntaxError>>,
}

impl Parse {
    pub fn new(tree: Arc<Tree>, mut errors: Vec<SyntaxError>) -> Parse {
        // Canonical order, so a splice reparse compares equal to a full parse.
        errors.sort_by_key(|error| (error.range.start(), error.range.end()));
        Parse {
            tree,
            errors: Arc::new(errors),
        }
    }

    pub fn tree(&self) -> &Arc<Tree> {
        &self.tree
    }

    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.tree.clone())
    }

    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    pub fn text(&self) -> &str {
        self.tree.text()
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
    erase_with(parse, |text, _| {
        out.push_str(text);
        true
    });
    out
}

/// Where in the source the erased program's first `lines` lines end: the source those lines
/// were erased from is the text before this offset. A line that ends inside a type definition
/// counts as the definition not yet read, so the offset falls before it.
pub fn erased_lines_end(parse: &Parse, lines: usize) -> TextSize {
    let mut remaining = lines;
    let mut end = TextSize::from(0);
    if remaining == 0 {
        return end;
    }
    erase_with(parse, |text, source| {
        let newlines = text.matches('\n').count();
        if newlines < remaining {
            remaining -= newlines;
            end = source.end();
            return true;
        }
        end = match source.len() == TextSize::of(text) {
            // Copied verbatim, so the line ends right after the `remaining`th newline.
            true => text
                .match_indices('\n')
                .nth(remaining - 1)
                .map_or(source.end(), |(at, _)| {
                    source.start() + TextSize::of(&text[..=at])
                }),
            false => source.start(),
        };
        false
    });
    end
}

/// Walks the program [`erase_inline_annotations`] writes, handing `emit` each piece with the
/// source range it stands for, until `emit` returns false.
fn erase_with(parse: &Parse, mut emit: impl FnMut(&str, TextRange) -> bool) {
    let root = parse.syntax_node();
    let mut preorder = root.preorder_with_tokens();
    while let Some(event) = preorder.next() {
        let WalkEvent::Enter(element) = event else {
            continue;
        };
        let more = match element {
            NodeOrToken::Token(token) => emit(token.text(), token.text_range()),
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::TYPE_DEFINITION => {
                preorder.skip_subtree();
                // A paired definition already has its comment spelling right above it.
                match paired_comment_definition(&node) {
                    Some(_) => emit("", node.text_range()),
                    None => emit(&type_definition_comment(&node), node.text_range()),
                }
            }
            NodeOrToken::Node(node) if is_inline_annotation(&node) => {
                preorder.skip_subtree();
                emit("", node.text_range())
            }
            NodeOrToken::Node(_) => true,
        };
        if !more {
            return;
        }
    }
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
    is_inline_annotation_kind(node.kind(), node.parent().map(|parent| parent.kind()))
}

/// [`is_inline_annotation`] from a node's kind and its parent's.
pub fn is_inline_annotation_kind(kind: SyntaxKind, parent: Option<SyntaxKind>) -> bool {
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

/// Every node under `root`, `root` included, that `wanted` accepts, in preorder, exactly as
/// `root.descendants().filter(..)` would yield them. `wanted` sees each node and its parent
/// (`None` for `root`) as borrowed cursors, so rejected nodes cost no reference count.
pub fn matching_nodes(
    root: &SyntaxNode,
    mut wanted: impl FnMut(NodeRef<'_>, Option<NodeRef<'_>>) -> bool,
) -> Vec<SyntaxNode> {
    let top = root.node_ref();
    top.descendants()
        .filter(|&node| wanted(node, (node != top).then(|| node.parent()).flatten()))
        .map(|node| root.at(node))
        .collect()
}

/// The first node in preorder under `root` with this kind and exactly this range.
pub fn node_at(root: &SyntaxNode, kind: SyntaxKind, range: TextRange) -> Option<SyntaxNode> {
    Some(root.at(root.node_ref().find(kind, range)?))
}

/// The source text of `node`: one slice of the file text, so it never allocates.
pub fn node_text(node: &SyntaxNode) -> Cow<'_, str> {
    Cow::Borrowed(node.text())
}

/// `deleted` is a range in `old`'s text and `inserted` the replacement's length
/// in `new_text`; the result always equals `parse(new_text)`.
pub fn reparse(old: &Parse, new_text: &str, deleted: TextRange, inserted: TextSize) -> Parse {
    let Some(spliced) = reparse::try_splice(old, new_text, deleted, inserted) else {
        return parse(new_text);
    };
    if testing::invariants_enabled() {
        let full = parse(new_text);
        assert!(
            spliced == full && spliced.tree().same_parents(full.tree()),
            "a splice reparse differs from a full parse\nold text: {:?}\nnew text: {:?}\ndeleted: {deleted:?}\ninserted: {inserted:?}",
            old.text(),
            new_text,
        );
    }
    spliced
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
