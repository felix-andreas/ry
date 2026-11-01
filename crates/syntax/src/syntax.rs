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
/// parameters, and its `-> TYPE`. Comments and `#:` annotations are kept.
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
            NodeOrToken::Node(node) if is_inline_annotation(&node) => preorder.skip_subtree(),
            NodeOrToken::Node(_) => {}
        }
    }
    out
}

/// Inline annotation syntax: R's own grammar has no reading for these nodes.
pub fn is_inline_annotation(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::TYPE_ASCRIPTION | SyntaxKind::RETURN_TYPE => true,
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
