//! A splice reparse shares the top-level children an edit cannot have changed and parses only
//! the lines between. Two line breaks bound that middle. The statements before the first one
//! read nothing past it ([`Lookahead`]), so they parse the same whatever follows, and the
//! middle, parsed with the second one, reads nothing past it either, so the text after it
//! starts a statement exactly as it did. Anything else falls back to a full parse.

use {
    crate::{
        Lookahead, NodeOrToken, Parse, SyntaxElement, Unexpected,
        kind::SyntaxKind::*,
        lexer::lex,
        parse,
        tree::{element_index, splice},
    },
    std::sync::Arc,
    text_size::{TextRange, TextSize},
};

pub fn try_splice(
    old: &Parse,
    new_text: &str,
    deleted: TextRange,
    inserted: TextSize,
) -> Option<Parse> {
    let root = old.syntax_node();
    if root.kind() != SOURCE_FILE {
        return None;
    }
    let children: Vec<SyntaxElement> = root.children_with_tokens().collect();

    // The prefix ends with a line break no statement before it read across.
    let read_across = |end: TextSize| {
        old.lookahead()
            .iter()
            .any(|lookahead| lookahead.statement < end && lookahead.end > end)
    };
    let mut prefix_end = children
        .iter()
        .take_while(|child| child.text_range().end() <= deleted.start())
        .count();
    while prefix_end > 0
        && !(anchor_ok(&children, prefix_end - 1)
            && !read_across(children[prefix_end - 1].text_range().end()))
    {
        prefix_end -= 1;
    }
    // The middle ends with the first line break after the edit that no statement read across,
    // so every error past it belongs to a statement past it, and the suffix starts behind it.
    let mut middle_end = children
        .iter()
        .position(|child| child.text_range().start() >= deleted.end())
        .unwrap_or(children.len());
    while middle_end < children.len()
        && !(anchor_ok(&children, middle_end)
            && !read_across(children[middle_end].text_range().end()))
    {
        middle_end += 1;
    }
    let suffix_start = (middle_end + 1).min(children.len());
    if suffix_start <= prefix_end || (prefix_end == 0 && suffix_start == children.len()) {
        return None;
    }

    let prefix_len = match prefix_end.checked_sub(1) {
        Some(last) => children[last].text_range().end(),
        None => TextSize::from(0),
    };
    let old_suffix_start = match children.get(suffix_start) {
        Some(child) => child.text_range().start(),
        None => root.text_range().end(),
    };
    let delta = i64::from(u32::from(inserted)) - i64::from(u32::from(deleted.len()));
    let new_suffix_start = (i64::from(u32::from(old_suffix_start)) + delta).max(0) as usize;
    let suffix_len = usize::from(root.text_range().end() - old_suffix_start);
    if usize::from(prefix_len) + suffix_len > new_text.len() {
        return None;
    }
    if new_suffix_start != new_text.len() - suffix_len {
        return None;
    }

    // An unterminated string in the prefix ends at its line break only because no closing quote
    // follows it; a quote anywhere after it would extend it across the edit.
    if old
        .errors()
        .iter()
        .any(|error| error.range.start() < prefix_len && error.message.starts_with("unterminated"))
    {
        return None;
    }
    let middle_text = &new_text[usize::from(prefix_len)..new_suffix_start];
    if middle_touches_annotation(middle_text) {
        return None;
    }
    // A lone `\r` that ends the prefix and a `\n` the edit brought next to it are one line
    // break to the lexer.
    if new_text[..usize::from(prefix_len)].ends_with('\r') && middle_text.starts_with('\n') {
        return None;
    }
    // An unterminated string or backtick in the middle stops at its line break only because
    // no closing quote follows inside the middle; a full parse would close it in the suffix.
    // Any other lexer error at the middle's end would swallow the suffix too.
    let (_, middle_lexer_errors) = lex(middle_text);
    let middle_line_end = middle_text.trim_end_matches(['\n', '\r']).len();
    if middle_lexer_errors.iter().any(|error| {
        error.message.starts_with("unterminated")
            || u32::from(error.range.end()) as usize >= middle_line_end
    }) {
        return None;
    }
    let middle = parse(middle_text);
    // A statement of the middle that read past the middle's last line break read the end of
    // the text, where a full parse reads the suffix.
    let middle_len = TextSize::from(middle_text.len() as u32);
    if suffix_len > 0
        && middle
            .lookahead()
            .iter()
            .any(|lookahead| lookahead.end > middle_len)
    {
        return None;
    }
    let old_tree = old.tree();
    let prefix = 1..match children.get(prefix_end) {
        Some(first) => element_index(first),
        None => old_tree.len() as u32,
    };
    let suffix = match children.get(suffix_start) {
        Some(first) => element_index(first)..old_tree.len() as u32,
        None => 0..0,
    };
    let tree = splice(
        SOURCE_FILE,
        new_text,
        &[
            (old_tree, prefix),
            (middle.tree(), 1..middle.tree().len() as u32),
            (old_tree, suffix),
        ],
    );

    let shift = |range: TextRange, by: i64| {
        let start = (i64::from(u32::from(range.start())) + by) as u32;
        let end = (i64::from(u32::from(range.end())) + by) as u32;
        TextRange::new(TextSize::from(start), TextSize::from(end))
    };
    let prefix = i64::from(u32::from(prefix_len));
    let mut errors = Vec::new();
    let mut lookahead = Vec::new();
    // An error of the old middle is dropped, and so is a zero-width one at the old end of file
    // when no suffix is shared: the middle's parse derives both again.
    for error in old.errors() {
        if error.range.end() <= prefix_len {
            errors.push(error.clone());
        } else if suffix_len > 0 && error.range.start() >= old_suffix_start {
            let mut rebased = error.clone();
            rebased.range = shift(error.range, delta);
            if let Unexpected::Token(found) = &mut rebased.unexpected {
                *found = shift(*found, delta);
            }
            errors.push(rebased);
        }
    }
    for error in middle.errors() {
        let mut rebased = error.clone();
        rebased.range = shift(error.range, prefix);
        if let Unexpected::Token(found) = &mut rebased.unexpected {
            *found = shift(*found, prefix);
        }
        errors.push(rebased);
    }
    let rebase = |lookahead: &Lookahead, by: i64| {
        let range = shift(TextRange::new(lookahead.statement, lookahead.end), by);
        Lookahead {
            statement: range.start(),
            end: range.end(),
        }
    };
    lookahead.extend(
        old.lookahead()
            .iter()
            .filter(|lookahead| lookahead.statement < prefix_len),
    );
    lookahead.extend(
        middle
            .lookahead()
            .iter()
            .map(|lookahead| rebase(lookahead, prefix)),
    );
    lookahead.extend(
        old.lookahead()
            .iter()
            .filter(|lookahead| suffix_len > 0 && lookahead.statement >= old_suffix_start)
            .map(|lookahead| rebase(lookahead, delta)),
    );

    Some(Parse::new(Arc::new(tree), errors, lookahead))
}

/// A NEWLINE next to an annotation is not a statement boundary: consecutive
/// `#:` lines stitch across newlines.
fn anchor_ok(children: &[SyntaxElement], index: usize) -> bool {
    let Some(child) = children.get(index) else {
        return false;
    };
    if child.kind() != NEWLINE {
        return false;
    }
    let annotationish = |element: &SyntaxElement| match element {
        NodeOrToken::Node(node) => node
            .descendants()
            .any(|descendant| descendant.kind() == ANNOTATION),
        NodeOrToken::Token(token) => token.kind() == ANNOTATION_MARKER,
    };
    let mut previous = index;
    while previous > 0 && children[previous - 1].kind() == WHITESPACE {
        previous -= 1;
    }
    if previous > 0 && annotationish(&children[previous - 1]) {
        return false;
    }
    let mut next = index + 1;
    while children
        .get(next)
        .is_some_and(|child| child.kind() == WHITESPACE)
    {
        next += 1;
    }
    if let Some(next) = children.get(next)
        && annotationish(next)
    {
        return false;
    }
    true
}

/// A `#:` anywhere on an edge line counts, since mid-line markers open trailing
/// annotations.
fn middle_touches_annotation(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("");
    let last = text.lines().next_back().unwrap_or("");
    first.contains("#:") || last.contains("#:") || text.trim_start().starts_with("#:")
}
