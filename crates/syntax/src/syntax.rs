//! Lossless R syntax trees: every byte, including trivia and `#:` annotations,
//! reprints exactly, and malformed input still yields a tree with local `ERROR`
//! nodes.

pub mod annotation;
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
mod validation;

use std::{borrow::Cow, sync::Arc};
pub use {
    kind::SyntaxKind,
    lexer::{
        EscapeError, EscapeErrorKind, EscapePiece, NumberType, Token, is_syntactic_name, lex,
        raw_string_content, split_number_suffix, unescape_quoted,
    },
    parser::{infix_binding_power, parse},
    text_size::{TextLen, TextRange, TextSize},
    tree::{
        ChildrenRef, Direction, ElementRef, NodeOrToken, NodeRef, Preorder, PreorderWithTokens,
        SyntaxElement, SyntaxElementChildren, SyntaxNode, SyntaxNodeChildren, SyntaxToken,
        TokenAtOffset, TokenRef, Tree, WalkEvent,
    },
    validation::{called_function, pipe_placeholder},
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
    /// The line break that ends the last line, which R reads whether or not the text has one,
    /// at a place where R's grammar does not skip line breaks.
    EndOfLine,
    /// Nothing: the error breaks one of ry's own rules for a construct that parsed in full,
    /// such as a tuple item with a name, so R's grammar has no report for it and ry's message
    /// is the only one.
    Nothing,
    /// One of the errors that leave the tree whole: a check R's lexer or its grammar actions
    /// make on text the grammar accepts, or a `;` the grammar refuses.
    Classed(Classed),
}

/// The errors of text whose tree is whole, which the formatter formats. R reports each at a
/// position of its own, which the runtime derives from the tree and the error's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classed {
    /// An escape R's lexer rejects in a string or a backquoted name. The range starts in the
    /// token that holds it.
    Escape,
    /// A function that declares one parameter name twice. The range is the second name.
    RepeatedFormal,
    /// A `|>` whose right side, at `target`, R's pipe refuses.
    Pipe { check: PipeCheck, target: TextRange },
    /// A `_` that no pipe replaces. The range is the `_`.
    StrayPlaceholder,
    /// An empty name, which R cannot make a symbol of: a backquoted name, or an empty string
    /// that names the function of a call or an argument. The range is the token.
    EmptyName,
    /// A `;` at the top level that ends no statement, which R's grammar refuses. The range is
    /// the `;`.
    EmptyStatement,
    /// A NUL byte in a string, which R cannot hold. The range is the byte.
    EmbeddedNul,
}

/// Why R's pipe refuses its right side. The first two blame the whole right side, the others
/// one placeholder in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeCheck {
    /// The right side is a name or a constant, not a call.
    NotCall,
    /// The right side calls a function with a syntax of its own, such as `+` or `if`.
    Special,
    /// A placeholder inside the function the right side calls, as in `x |> _$f()`.
    PlaceholderInFunction,
    /// A placeholder passed as an argument without a name.
    PlaceholderNotNamed,
    /// A second placeholder.
    PlaceholderRepeated,
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

    /// This error with every range it holds moved by `by` bytes.
    pub(crate) fn shifted(&self, by: i64) -> SyntaxError {
        let shift = |range: TextRange| {
            let start = (i64::from(u32::from(range.start())) + by) as u32;
            let end = (i64::from(u32::from(range.end())) + by) as u32;
            TextRange::new(TextSize::from(start), TextSize::from(end))
        };
        let mut shifted = self.clone();
        shifted.range = shift(self.range);
        match &mut shifted.unexpected {
            Unexpected::Token(found) => *found = shift(*found),
            Unexpected::Classed(Classed::Pipe { target, .. }) => *target = shift(*target),
            _ => {}
        }
        shifted
    }
}

/// The console's rule for reading another line instead of running what it has.
///
/// R's rule: input is unfinished when every grammar error says the input ended too early. Checks
/// R makes when a construct closes (a repeated parameter, a stray placeholder) refuse nothing
/// until the command is whole, and a broken `#:` comment never holds input back.
///
/// ry adds one case: input ending in a `#:` block on its own lines is unfinished, because the
/// block annotates the next expression. A blank line after it ends the wait, as it detaches the
/// block in a file, so `text` must end with its last line break. A `@type` or `@alias` block
/// declares a type by itself and is finished.
pub fn input_is_unfinished(text: &str) -> bool {
    let parse = parse(text);
    let end = TextSize::of(text.trim_end());
    let mut refused_when_whole = false;
    let mut grammar = parse
        .errors()
        .iter()
        .filter(|error| {
            let deferred = match error.unexpected {
                Unexpected::Classed(
                    Classed::RepeatedFormal | Classed::Pipe { .. } | Classed::StrayPlaceholder,
                ) => true,
                // The lexer refuses an empty backquoted name at once.
                Unexpected::Classed(Classed::EmptyName) => !text[error.range].starts_with('`'),
                _ => false,
            };
            refused_when_whole |= deferred && !error.in_annotation;
            !error.in_annotation && !deferred
        })
        .peekable();
    if grammar.peek().is_some() {
        return grammar.all(|error| error.incomplete || error.range.start() >= end);
    }
    !refused_when_whole
        && parse.errors().is_empty()
        && ends_in_an_unattached_annotation(&parse.syntax_node())
}

fn ends_in_an_unattached_annotation(root: &SyntaxNode) -> bool {
    let mut newlines = 0;
    let mut last = root.last_child_or_token();
    let block = loop {
        let Some(element) = last else {
            return false;
        };
        match element.kind() {
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::NEWLINE => newlines += 1,
            SyntaxKind::ANNOTATION => match element.clone().into_node() {
                Some(block) => break block,
                None => return false,
            },
            _ => return false,
        }
        last = element.prev_sibling_or_token();
    };
    if newlines >= 2 || annotation::block_refusal(&block).is_some() {
        return false;
    }
    let declares_a_type = block.children().any(|child| {
        child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE
            && annotation::is_definition_keyword(&annotation::directive_name(&child))
    });
    let mut before = block.prev_sibling_or_token();
    while let Some(element) = &before
        && element.kind() == SyntaxKind::WHITESPACE
    {
        before = element.prev_sibling_or_token();
    }
    let on_its_own_lines = before.is_none_or(|element| element.kind() == SyntaxKind::NEWLINE);
    !declares_a_type && on_its_own_lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    tree: Arc<Tree>,
    errors: Arc<Vec<SyntaxError>>,
    lookahead: Arc<[Lookahead]>,
}

/// A top-level statement, or a `#:` annotation between two of them, whose parse read text past
/// the token it stops on: a type that looked for a `|` on the next line, recovery that asked
/// whether the next line starts a statement, an annotation that looked for a `#:` line to
/// continue on, or any of them that read the end of the text. A statement that stops on the
/// line break it read last has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lookahead {
    /// Where the statement starts.
    pub statement: TextSize,
    /// The end of the last token it read, or one past the text when it read the end of file.
    pub end: TextSize,
}

impl Parse {
    pub fn new(tree: Arc<Tree>, mut errors: Vec<SyntaxError>, lookahead: Vec<Lookahead>) -> Parse {
        // Canonical order, so a splice reparse compares equal to a full parse.
        errors.sort_by_key(|error| (error.range.start(), error.range.end()));
        Parse {
            tree,
            errors: Arc::new(errors),
            lookahead: lookahead.into(),
        }
    }

    /// In statement order.
    pub fn lookahead(&self) -> &[Lookahead] {
        &self.lookahead
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
    erase_with(parse, |text, _, _| {
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
    erase_with(parse, |text, source, verbatim| {
        let newlines = text.matches('\n').count();
        if newlines < remaining {
            remaining -= newlines;
            end = source.end();
            return true;
        }
        end = match verbatim {
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

/// [`erase_inline_annotations`]'s program with each statement's inline annotation moved into a
/// `#:` block above the statement, so the program keeps its types and GNU R reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentedProgram {
    pub text: String,
    /// Inline annotations no `#:` block can carry, in source order: those on a function that is
    /// not a statement's value, a `@Name(value)` inside an expression, and those on a statement
    /// that already has a `#:` block. They are erased.
    pub dropped: Vec<TextRange>,
}

/// Rewrites every statement that binds a name with inline types into the same statement under
/// a `#:` block: `count: integer <- 0L` becomes `#: integer` above `count <- 0L`, a function's
/// inline signature becomes `#: <T> fn(x: T, [y]: double) -> T`, `@pure` a `#: @pure` line, and
/// a value `@Name(value)` a `#: @new Name` line. The rest erases as in
/// [`erase_inline_annotations`].
pub fn inline_annotations_to_comments(parse: &Parse) -> CommentedProgram {
    let root = parse.syntax_node();
    let source = parse.text();
    let mut blocks: Vec<(TextSize, String)> = Vec::new();
    let mut carried: Vec<TextRange> = Vec::new();
    for statement in matching_nodes(&root, |node, parent| {
        node.kind() == SyntaxKind::BINARY_EXPR
            && parent.is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
                )
            })
    }) {
        if let Some((lines, statement_carried)) = statement_comment(&statement, source) {
            let start = statement.text_range().start();
            let line_start = source[..usize::from(start)]
                .rfind('\n')
                .map_or(0, |at| at + 1);
            let before = &source[line_start..usize::from(start)];
            let leading = before.len() - before.trim_start_matches([' ', '\t']).len();
            let indent = &before[..leading];
            let mut block = match leading == before.len() {
                true => String::new(),
                // Behind other code on its line, the block opens a line of its own.
                false => format!("\n{indent}"),
            };
            for line in lines {
                block.push_str("#: ");
                block.push_str(&line.replace('\n', &format!("\n{indent}#: ")));
                block.push('\n');
                block.push_str(indent);
            }
            blocks.push((start, block));
            carried.extend(statement_carried);
        }
    }
    let dropped: Vec<TextRange> = matching_nodes(&root, |node, parent| {
        node.kind() != SyntaxKind::TYPE_DEFINITION
            && is_inline_annotation_kind(node.kind(), parent.map(|parent| parent.kind()))
    })
    .into_iter()
    .map(|node| node.text_range())
    .filter(|range| !carried.contains(range))
    .collect();
    let dropped = dropped
        .iter()
        .copied()
        .filter(|range| {
            !dropped
                .iter()
                .any(|outer| outer != range && outer.contains_range(*range))
        })
        .collect();
    let mut text = String::with_capacity(source.len() + blocks.len() * 16);
    let mut next = blocks.iter().peekable();
    erase_with(parse, |piece, range, _| {
        while let Some((_, block)) = next.next_if(|(at, _)| *at <= range.start()) {
            text.push_str(block);
        }
        text.push_str(piece);
        true
    });
    CommentedProgram { text, dropped }
}

/// The `#:` lines that say what `statement`'s inline annotations say, and the annotation nodes
/// they carry; `None` when it binds no name, has none, or already has a `#:` block.
fn statement_comment(
    statement: &SyntaxNode,
    source: &str,
) -> Option<(Vec<String>, Vec<TextRange>)> {
    let text = |range: TextRange| source[range].trim().to_owned();
    let child =
        |node: &SyntaxNode, kind: SyntaxKind| node.children().find(|child| child.kind() == kind);
    let token = |node: &SyntaxNode, kind: SyntaxKind| {
        node.children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .find(|token| token.kind() == kind)
    };
    let assigns = statement
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .any(|token| matches!(token.kind(), SyntaxKind::LESS_MINUS | SyntaxKind::EQ));
    let mut nodes = statement.children();
    let target = nodes.next()?;
    if !assigns || target.kind() != SyntaxKind::NAME || paired_comment_block(statement) {
        return None;
    }
    let value = statement.children().last()?;
    let ascription = child(statement, SyntaxKind::TYPE_ASCRIPTION);
    let after_colon = |ascription: &SyntaxNode| {
        let colon = token(ascription, SyntaxKind::COLON)?;
        Some(text(TextRange::new(
            colon.text_range().end(),
            ascription.text_range().end(),
        )))
    };
    let mut lines = Vec::new();
    let mut carried = Vec::new();
    match value.kind() {
        SyntaxKind::FUNCTION_DEF => {
            if let Some(pure) = child(&value, SyntaxKind::PURE_MARKER) {
                lines.push(text(pure.text_range()));
                carried.push(pure.text_range());
            }
            let binders = child(&value, SyntaxKind::TYPE_BINDER_LIST);
            let result = child(&value, SyntaxKind::RETURN_TYPE);
            let mut parameters = Vec::new();
            if let Some(list) = child(&value, SyntaxKind::PARAMETER_LIST) {
                for parameter in list
                    .children()
                    .filter(|node| node.kind() == SyntaxKind::PARAMETER)
                {
                    let Some(parameter_ascription) = child(&parameter, SyntaxKind::TYPE_ASCRIPTION)
                    else {
                        continue;
                    };
                    let name = parameter
                        .children_with_tokens()
                        .filter_map(NodeOrToken::into_token)
                        .find(|token| matches!(token.kind(), SyntaxKind::IDENT | SyntaxKind::DOTS))
                        .or_else(|| child(&parameter, SyntaxKind::NAME)?.first_token())?;
                    let type_text = after_colon(&parameter_ascription)?;
                    parameters.push(match token(&parameter, SyntaxKind::EQ) {
                        Some(_) => format!("[{}]: {type_text}", name.text()),
                        None => format!("{}: {type_text}", name.text()),
                    });
                    carried.push(parameter_ascription.text_range());
                }
            }
            let signed = binders.is_some() || result.is_some() || !parameters.is_empty();
            if signed {
                let mut signature = String::new();
                if let Some(binders) = &binders {
                    signature.push_str(&text(binders.text_range()));
                    signature.push(' ');
                    carried.push(binders.text_range());
                }
                signature.push_str(&format!("fn({})", parameters.join(", ")));
                if let Some(result) = &result {
                    let arrow = token(result, SyntaxKind::MINUS_GREATER)?;
                    signature.push_str(" -> ");
                    signature.push_str(&text(TextRange::new(
                        arrow.text_range().end(),
                        result.text_range().end(),
                    )));
                    carried.push(result.text_range());
                }
                lines.push(signature);
            } else if let Some(ascription) = &ascription {
                lines.push(after_colon(ascription)?);
                carried.push(ascription.text_range());
            }
        }
        SyntaxKind::NEW_EXPR => {
            let nominal = child(&value, SyntaxKind::NEW_TYPE)?;
            let at = token(&nominal, SyntaxKind::AT)?;
            lines.push(format!(
                "@new {}",
                text(TextRange::new(
                    at.text_range().end(),
                    nominal.text_range().end()
                ))
            ));
            carried.push(nominal.text_range());
        }
        _ => {
            let ascription = ascription.as_ref()?;
            lines.push(after_colon(ascription)?);
            carried.push(ascription.text_range());
        }
    }
    (!lines.is_empty()).then_some((lines, carried))
}

/// Whether a `#:` block sits directly above `statement`, on the line before it.
fn paired_comment_block(statement: &SyntaxNode) -> bool {
    let mut newlines = 0;
    let mut element = statement.prev_sibling_or_token();
    while let Some(current) = element {
        match current.kind() {
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::NEWLINE if newlines == 0 => newlines += 1,
            SyntaxKind::ANNOTATION => return true,
            _ => return false,
        }
        element = current.prev_sibling_or_token();
    }
    false
}

/// Walks the program [`erase_inline_annotations`] writes, handing `emit` each piece with the
/// source range it stands for and whether it is that source copied as written, until `emit`
/// returns false.
fn erase_with(parse: &Parse, mut emit: impl FnMut(&str, TextRange, bool) -> bool) {
    let root = parse.syntax_node();
    let mut preorder = root.preorder_with_tokens();
    let mut after_definition = false;
    while let Some(event) = preorder.next() {
        let WalkEvent::Enter(element) = event else {
            continue;
        };
        let more = match element {
            // A `#:` behind a type definition annotates nothing. Behind the definition's
            // comment it would be a second annotation on the line, and alone on the line it
            // would annotate the next statement, so it is erased to a plain comment.
            NodeOrToken::Token(token)
                if after_definition && token.kind() == SyntaxKind::ANNOTATION_MARKER =>
            {
                emit("#", token.text_range(), false)
            }
            NodeOrToken::Token(token) => {
                after_definition &= token.kind() != SyntaxKind::NEWLINE;
                emit(token.text(), token.text_range(), true)
            }
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::TYPE_DEFINITION => {
                preorder.skip_subtree();
                after_definition = true;
                // A paired definition already has its comment spelling right above it.
                match paired_comment_definition(&node) {
                    Some(_) => emit("", node.text_range(), false),
                    None => emit(&type_definition_comment(&node), node.text_range(), false),
                }
            }
            NodeOrToken::Node(node) if is_inline_annotation(&node) => {
                preorder.skip_subtree();
                emit("", node.text_range(), false)
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
                            .is_some_and(|word| annotation::is_definition_keyword(word.text()))
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
