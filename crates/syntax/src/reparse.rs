//! Top-level newlines close statements (`else` never continues across one), so
//! untouched top-level children are shared and only the edited middle is
//! reparsed. Anything unanchorable falls back to a full parse.

use crate::Parse;
use crate::kind::SyntaxKind;
use rowan::{GreenNode, NodeOrToken, TextRange, TextSize};

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
    let delta = i64::from(u32::from(inserted)) - i64::from(u32::from(deleted.len()));
    let new_suffix_start = (i64::from(u32::from(old_suffix_start)) + delta).max(0) as usize;
    let suffix_len = usize::from(root.text_range().end() - old_suffix_start);
    if usize::from(prefix_len) + suffix_len > new_text.len() {
        return None;
    }
    if new_suffix_start != new_text.len() - suffix_len {
        return None;
    }

    if prefix_end == 0 && suffix_start == children.len() {
        return None;
    }

    let middle_text = &new_text[usize::from(prefix_len)..new_suffix_start];
    if middle_touches_annotation(middle_text) {
        return None;
    }
    // An unterminated string or backtick at the middle's end would swallow the
    // suffix in a full parse.
    let (middle_tokens, middle_lexer_errors) = crate::lexer::lex(middle_text);
    if middle_lexer_errors
        .iter()
        .any(|error| u32::from(error.range.end()) as usize == middle_text.len())
    {
        return None;
    }
    // Kind-matched, not a counter: recovery never lets a stray closer close
    // another kind's group (`( ]` leaves the paren open).
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
    // An error at the middle's end (trailing operator, dangling `else`) means a
    // full parse would continue across the suffix newline.
    if middle
        .errors()
        .iter()
        .any(|error| u32::from(error.range.end()) as usize == middle_text.len())
    {
        return None;
    }
    let middle_root = middle.syntax_node();

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

    let mut errors = Vec::new();
    for error in old.errors() {
        if error.range.end() <= prefix_len {
            errors.push(error.clone());
        } else if suffix_start < children.len() && error.range.start() >= old_suffix_start {
            // The non-empty-suffix guard drops a zero-width end-of-file error:
            // it belongs to the replaced region, whose parse re-derives it.
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

/// A NEWLINE next to an annotation is not a statement boundary: consecutive
/// `#:` lines stitch across newlines.
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

/// A `#:` anywhere on an edge line counts, since mid-line markers open trailing
/// annotations.
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
