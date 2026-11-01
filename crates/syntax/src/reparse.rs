//! Top-level newlines close statements (`else` never continues across one), so
//! untouched top-level children are shared and only the edited middle is
//! reparsed. Anything unanchorable falls back to a full parse.

use {
    crate::{
        Parse, SyntaxElement,
        kind::SyntaxKind::{self, *},
        lexer::lex,
        parse,
    },
    rowan::{GreenNode, NodeOrToken, TextRange, TextSize},
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

    let mut prefix_end = children
        .iter()
        .take_while(|child| child.text_range().end() <= deleted.start())
        .count();
    while prefix_end > 0 && !anchor_ok(&children, prefix_end - 1) {
        prefix_end -= 1;
    }
    let mut suffix_start = children
        .iter()
        .position(|child| child.text_range().start() >= deleted.end())
        .unwrap_or(children.len());
    while suffix_start < children.len() && !anchor_ok(&children, suffix_start) {
        suffix_start += 1;
    }
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
    // An unterminated string or backtick in the middle stops at its line break only because
    // no closing quote follows inside the middle; a full parse would close it in the suffix.
    // Any other lexer error at the middle's end would swallow the suffix too.
    let (middle_tokens, middle_lexer_errors) = lex(middle_text);
    if middle_lexer_errors.iter().any(|error| {
        error.message.starts_with("unterminated")
            || u32::from(error.range.end()) as usize == middle_text.len()
    }) {
        return None;
    }
    // Kind-matched, not a counter: recovery never lets a stray closer close
    // another kind's group (`( ]` leaves the paren open).
    let mut open_delimiters: Vec<SyntaxKind> = Vec::new();
    for token in &middle_tokens {
        match token.kind {
            L_PAREN | L_BRACKET | L_BRACE => open_delimiters.push(token.kind),
            L_BRACKET2 => open_delimiters.extend([L_BRACKET, L_BRACKET]),
            R_PAREN if open_delimiters.last() == Some(&L_PAREN) => {
                open_delimiters.pop();
            }
            R_BRACKET if open_delimiters.last() == Some(&L_BRACKET) => {
                open_delimiters.pop();
            }
            R_BRACE if open_delimiters.last() == Some(&L_BRACE) => {
                open_delimiters.pop();
            }
            _ => {}
        }
    }
    if !open_delimiters.is_empty() {
        return None;
    }
    let middle = parse(middle_text);
    // An error at the middle's end (trailing operator, dangling `else`), or one the middle's
    // end cut off wherever it is reported (`function(x)` whose body is on the next line), means
    // a full parse would continue across the suffix newline.
    if middle
        .errors()
        .iter()
        .any(|error| error.incomplete || u32::from(error.range.end()) as usize == middle_text.len())
    {
        return None;
    }
    let new_children: Vec<_> = children[..prefix_end]
        .iter()
        .cloned()
        .chain(middle.syntax_node().children_with_tokens())
        .chain(children[suffix_start..].iter().cloned())
        .map(|element| match element {
            NodeOrToken::Node(node) => NodeOrToken::Node(node.green().into_owned()),
            NodeOrToken::Token(token) => NodeOrToken::Token(token.green().to_owned()),
        })
        .collect();
    let green = GreenNode::new(SOURCE_FILE.into(), new_children);

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
