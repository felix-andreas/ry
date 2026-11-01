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

use rowan::{GreenNode, NodeOrToken, WalkEvent};
use std::mem;
use std::sync::Arc;

pub use kind::SyntaxKind;
pub use lexer::{Token, is_syntactic_name, lex};
pub use parser::parse;
pub use rowan::{TextRange, TextSize};

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
    match node.kind() {
        SyntaxKind::TYPE_ASCRIPTION
        | SyntaxKind::RETURN_TYPE
        | SyntaxKind::TYPE_DEFINITION
        | SyntaxKind::NEW_TYPE => true,
        SyntaxKind::TYPE_BINDER_LIST => {
            node.parent().map(|parent| parent.kind()) == Some(SyntaxKind::FUNCTION_DEF)
        }
        _ => false,
    }
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
