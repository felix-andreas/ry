//! Statement-level incremental reparse: splice-parse only the edited region.
//!
//! An edit at the top level can only affect the statements it touches: R is
//! parsed left-to-right, top-level newline boundaries close statements (`else`
//! never continues across a top-level newline), so the untouched prefix and
//! suffix children of `SOURCE_FILE` remain exactly as they were and their
//! green subtrees are **shared by pointer** into the new tree. Only the text
//! between the chosen newline anchors is reparsed.
//!
//! One construct can join statements across a newline: consecutive `#:` lines
//! stitch into a single annotation region, so anchors never sit against an
//! annotation. Anything unanchorable (no clean newline boundary, tiny files)
//! falls back to a full parse. The splice is an optimization and never a
//! correctness dependency, and the edit-stream fuzzer holds it equal to the
//! from-scratch parse, in both bytes and structure.

use crate::Parse;
use crate::kind::SyntaxKind;
use rowan::{GreenNode, NodeOrToken, TextRange, TextSize};

/// Reparse after one edit: `deleted` is the replaced range in the OLD text,
/// `inserted` the length of its replacement in `new_text`.
pub fn reparse(old: &Parse, new_text: &str, deleted: TextRange, inserted: TextSize) -> Parse {
    match try_splice(old, new_text, deleted, inserted) {
        Some(parse) => parse,
        None => crate::parse(new_text),
    }
}

fn try_splice(
    old: &Parse,
    new_text: &str,
    deleted: TextRange,
    inserted: TextSize,
) -> Option<Parse> {
    let root = old.syntax_node();
    if root.kind() != SyntaxKind::SOURCE_FILE {
        return None;
    }
    let children: Vec<_> = root.children_with_tokens().collect();
    if children.is_empty() {
        return None;
    }

    // Prefix: children entirely before the deleted range, cut back to the last
    // top-level NEWLINE anchor not adjacent to an annotation.
    let mut prefix_end = 0usize;
    for (index, child) in children.iter().enumerate() {
        if child.text_range().end() <= deleted.start() {
            prefix_end = index + 1;
        } else {
            break;
        }
    }
    while prefix_end > 0 && !anchor_ok(&children, prefix_end - 1) {
        prefix_end -= 1;
    }

    // Suffix: children entirely after the deleted range, advanced to the first
    // NEWLINE anchor (the newline stays with the suffix).
    let mut suffix_start = children.len();
    for (index, child) in children.iter().enumerate() {
        if child.text_range().start() >= deleted.end() {
            suffix_start = index;
            break;
        }
    }
    while suffix_start < children.len() && !anchor_ok(&children, suffix_start) {
        suffix_start += 1;
    }
    if suffix_start <= prefix_end {
        return None;
    }

    let prefix_len = if prefix_end == 0 {
        TextSize::from(0)
    } else {
        children[prefix_end - 1].text_range().end()
    };
    let old_suffix_start = if suffix_start == children.len() {
        root.text_range().end()
    } else {
        children[suffix_start].text_range().start()
    };
    // The suffix shifts by the edit delta.
    let delta = i64::from(u32::from(inserted)) - i64::from(u32::from(deleted.len()));
    let new_suffix_start = (i64::from(u32::from(old_suffix_start)) + delta).max(0) as usize;
    let suffix_len = usize::from(root.text_range().end() - old_suffix_start);
    if usize::from(prefix_len) + suffix_len > new_text.len() {
        return None;
    }
    if new_suffix_start != new_text.len() - suffix_len {
        return None;
    }

    // Nothing usable to splice around?
    if prefix_end == 0 && suffix_start == children.len() {
        return None;
    }

    let middle_text = &new_text[usize::from(prefix_len)..new_suffix_start];
    // A middle beginning or ending with an annotation marker could stitch
    // across the anchors in a from-scratch parse; refuse (rare).
    if middle_touches_annotation(middle_text) {
        return None;
    }
    // The suffix reparses identically only when the middle leaves the parser
    // in the same state a top-level statement boundary has: (a) no lexical
    // construct running off the middle's end (an unterminated string / raw
    // string / backtick would swallow the suffix in a from-scratch parse) and
    // (b) no delimiter still open (inside a group, suffix newlines would stop
    // being significant).
    let (middle_tokens, middle_lexer_errors) = crate::lexer::lex(middle_text);
    if middle_lexer_errors
        .iter()
        .any(|error| u32::from(error.range.end()) as usize == middle_text.len())
    {
        return None;
    }
    // Kind-matched delimiter stack: a closer only closes its own opener. A
    // mismatched or stray closer is consumed locally by recovery either way
    // and never closes the group. `( ]` leaves the paren open, and a flat
    // counter would wrongly call that balanced. Anything still open at the
    // middle's end would swallow the suffix in a from-scratch parse.
    let mut open_delimiters: Vec<SyntaxKind> = Vec::new();
    for token in &middle_tokens {
        match token.kind {
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET | SyntaxKind::L_BRACE => {
                open_delimiters.push(token.kind);
            }
            SyntaxKind::L_BRACKET2 => {
                open_delimiters.push(SyntaxKind::L_BRACKET);
                open_delimiters.push(SyntaxKind::L_BRACKET);
            }
            SyntaxKind::R_PAREN if open_delimiters.last() == Some(&SyntaxKind::L_PAREN) => {
                open_delimiters.pop();
            }
            SyntaxKind::R_BRACKET if open_delimiters.last() == Some(&SyntaxKind::L_BRACKET) => {
                open_delimiters.pop();
            }
            SyntaxKind::R_BRACE if open_delimiters.last() == Some(&SyntaxKind::L_BRACE) => {
                open_delimiters.pop();
            }
            _ => {}
        }
    }
    if !open_delimiters.is_empty() {
        return None;
    }
    let middle = crate::parse(middle_text);
    // A parse error touching the middle's end means the parser is
    // mid-construct at the boundary. A trailing `$`, a trailing infix
    // operator, and a dangling `else` all leave it there. A from-scratch parse
    // of the whole text would continue across the suffix newline, giving a
    // different statement split and different error positions. Refuse and fall
    // back.
    if middle
        .errors()
        .iter()
        .any(|error| u32::from(error.range.end()) as usize == middle_text.len())
    {
        return None;
    }
    let middle_root = middle.syntax_node();

    // Assemble the new root: shared prefix greens + reparsed middle greens +
    // shared suffix greens.
    let mut new_children: Vec<NodeOrToken<GreenNode, rowan::GreenToken>> = Vec::new();
    for child in &children[..prefix_end] {
        new_children.push(to_green(child));
    }
    for child in middle_root.children_with_tokens() {
        new_children.push(to_green(&child));
    }
    for child in &children[suffix_start..] {
        new_children.push(to_green(child));
    }
    let green = GreenNode::new(SyntaxKind::SOURCE_FILE.into(), new_children);

    // Errors: prefix errors verbatim, middle errors rebased right, suffix
    // errors shifted by the delta.
    let mut errors = Vec::new();
    for error in old.errors() {
        if error.range.end() <= prefix_len {
            errors.push(error.clone());
        } else if suffix_start < children.len() && error.range.start() >= old_suffix_start {
            // With an empty suffix a zero-width end-of-file error would
            // satisfy the start bound too, but it belongs to the replaced
            // region, and the middle's own parse re-derives the end state.
            let start = (i64::from(u32::from(error.range.start())) + delta) as u32;
            let end = (i64::from(u32::from(error.range.end())) + delta) as u32;
            let mut rebased = error.clone();
            rebased.range = TextRange::new(TextSize::from(start), TextSize::from(end));
            errors.push(rebased);
        }
    }
    for error in middle.errors() {
        let mut rebased = error.clone();
        rebased.range = TextRange::new(
            error.range.start() + prefix_len,
            error.range.end() + prefix_len,
        );
        errors.push(rebased);
    }

    Some(Parse::new(green, errors))
}

/// A child index is a valid anchor when it is a top-level NEWLINE token and no
/// non-whitespace neighbor on either side is (or contains) an annotation
/// region. Consecutive `#:` lines stitch across newlines, so an anchor next to
/// one is not a real statement boundary.
fn anchor_ok(children: &[rowan::SyntaxElement<crate::RLanguage>], index: usize) -> bool {
    let Some(child) = children.get(index) else {
        return false;
    };
    if child.kind() != SyntaxKind::NEWLINE {
        return false;
    }
    let annotationish = |element: &rowan::SyntaxElement<crate::RLanguage>| match element {
        NodeOrToken::Node(node) => node
            .descendants()
            .any(|descendant| descendant.kind() == SyntaxKind::ANNOTATION),
        NodeOrToken::Token(token) => token.kind() == SyntaxKind::ANNOTATION_MARKER,
    };
    let mut previous = index;
    while previous > 0 && children[previous - 1].kind() == SyntaxKind::WHITESPACE {
        previous -= 1;
    }
    if previous > 0 && annotationish(&children[previous - 1]) {
        return false;
    }
    let mut next = index + 1;
    while children
        .get(next)
        .is_some_and(|c| c.kind() == SyntaxKind::WHITESPACE)
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

/// Annotations at the middle's edges can stitch across the anchors: refuse
/// when the first line might continue a prefix annotation or the last line
/// might continue into a suffix one. A `#:` anywhere on the edge line is
/// enough (mid-line markers open trailing annotations); a string containing
/// the characters only costs an unnecessary fallback.
fn middle_touches_annotation(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("");
    let last = text.lines().next_back().unwrap_or("");
    first.contains("#:") || last.contains("#:") || text.trim_start().starts_with("#:")
}

fn to_green(
    element: &rowan::SyntaxElement<crate::RLanguage>,
) -> NodeOrToken<GreenNode, rowan::GreenToken> {
    match element {
        NodeOrToken::Node(node) => NodeOrToken::Node(node.green().into()),
        NodeOrToken::Token(token) => NodeOrToken::Token(token.green().to_owned()),
    }
}
