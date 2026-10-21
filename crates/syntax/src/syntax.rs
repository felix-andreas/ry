//! Lossless R syntax: a hand-written lexer and recursive-descent parser producing
//! rowan green/red trees.
//!
//! Every byte of the input lives in the tree and reprints exactly, so
//! `syntax_node().text()` equals the input. That includes whitespace, comments,
//! and `#:` type annotations. An annotation is lexed as structured trivia and
//! parsed into first-class nodes with real spans. Every parse yields a tree.
//! Malformed input produces `ERROR` nodes local to the break, plus
//! `SyntaxError`s with precise ranges.

pub mod ast;
pub mod kind;
pub mod literate;
pub mod testing;

mod lexer;
mod parser;
mod reparse;

use std::sync::Arc;

pub use kind::SyntaxKind;
pub use lexer::{Token, is_syntactic_name, lex};
pub use rowan::{TextRange, TextSize};

/// The rowan language tag for R.
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
        // Sound: `SyntaxKind` is `repr(u16)` with dense discriminants `0..=ERROR`,
        // and the bound was just checked.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<RLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<RLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<RLanguage>;
pub type SyntaxNodeChildren = rowan::SyntaxNodeChildren<RLanguage>;

/// A parse-time diagnostic with a precise source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub range: TextRange,
    /// Raised by the `#:` annotation type grammar rather than the R grammar.
    /// Consumers that treat annotation problems more leniently (the formatter
    /// preserves the block verbatim instead of refusing the file) key on this
    /// instead of guessing from positions.
    pub in_annotation: bool,
}

impl SyntaxError {
    pub fn new(message: impl Into<String>, range: TextRange) -> SyntaxError {
        SyntaxError {
            message: message.into(),
            range,
            in_annotation: false,
        }
    }
}

/// The result of parsing: a green tree plus its syntax errors.
///
/// Cheap to clone; the green tree is the position-independent, structurally
/// shared representation downstream layers key their early cutoffs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    green: rowan::GreenNode,
    errors: Arc<Vec<SyntaxError>>,
}

impl Parse {
    pub fn new(green: rowan::GreenNode, mut errors: Vec<SyntaxError>) -> Parse {
        // Canonical position order (stable: discovery order breaks ties), so
        // error lists compare structurally whichever pipeline produced them.
        // The lexer and the parser both produce errors, and so do a
        // from-scratch parse and a splice reparse.
        errors.sort_by_key(|error| (error.range.start(), error.range.end()));
        Parse {
            green,
            errors: Arc::new(errors),
        }
    }

    pub fn green(&self) -> &rowan::GreenNode {
        &self.green
    }

    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    /// The exact source text the tree reprints to.
    pub fn text(&self) -> String {
        self.syntax_node().text().to_string()
    }

    /// Debug rendering of the tree (and errors), used by golden tests.
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

/// Parse R source text into a lossless syntax tree.
pub fn parse(text: &str) -> Parse {
    parser::parse(text)
}

/// Statement-splice incremental reparse: reuse `old`'s untouched top-level
/// subtrees (shared by pointer) and reparse only the edited region. `deleted`
/// is the replaced range in the OLD text, `inserted` the replacement's length
/// in `new_text`. Falls back to a full parse whenever no clean statement
/// anchor exists; the result is always equal to `parse(new_text)`.
pub fn reparse(old: &Parse, new_text: &str, deleted: TextRange, inserted: TextSize) -> Parse {
    reparse::reparse(old, new_text, deleted, inserted)
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
            rowan::NodeOrToken::Node(child) => dump_node(out, &child, depth + 1),
            rowan::NodeOrToken::Token(token) => {
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
