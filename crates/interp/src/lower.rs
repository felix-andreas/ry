//! R source to R language objects. ry's parser (the `syntax` crate) builds a lossless tree; this
//! lowers it to exactly the calls, symbols and constants GNU R's parser produces, because R code
//! inspects them (`quote`, `substitute`, `deparse`, `body`, `match.call`). The pipe `|>` is
//! rewritten at parse time and `->` becomes `<-`, as in GNU R. ry's inline type annotations are
//! dropped from the language objects, and a function's parameter and return types are registered
//! as checks at its boundary instead (`annotations`).
//!
//! When a parse keeps source (`keep.source`), the lowering also attaches GNU R's source
//! references: a `srcref` list on the expression vector and on every `{` call, and a srcref as
//! the fourth element of every `function` call. Without it, none of that work happens.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    unwind::{R, error},
    values::{self, alloc, install, missing_arg, nil, symbols},
};
use heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType};
use syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("parse", 6, do_parse),
    internal("str2lang", 1, do_str2lang),
    internal("str2expression", 1, do_str2expression),
];

/// Parses `text` into an expression vector, or reports the first syntax error in GNU R's form.
pub unsafe fn parse_text(text: &str, source_name: &str) -> R<Sexp> {
    parse_with_srcfile(text, source_name, nil())
}

/// Parses `text` like [`parse_text`], and keeps source references against `srcfile` when it is a
/// srcfile environment, as GNU R's parser does when `parse` is given one.
pub unsafe fn parse_with_srcfile(text: &str, source_name: &str, srcfile: Sexp) -> R<Sexp> {
    let script = parse_script(text, source_name)?;
    let source = if srcfile.kind() == SexpType::Environment {
        Some(SourceMap::new(text, 0, srcfile)?)
    } else {
        None
    };
    let count = script.expressions.len();
    let result = alloc(SexpType::Expression, count)?;
    let srcrefs = match &source {
        Some(_) => alloc(SexpType::List, count)?,
        None => nil(),
    };
    for (index, node) in script.expressions.iter().enumerate() {
        values::set_elt(result, index, lower(node, source.as_ref())?);
        if let Some(source) = &source {
            values::set_elt(srcrefs, index, srcref(source, significant_range(node))?);
        }
    }
    if let Some(source) = &source {
        // GNU R reads each line of the text with its newline, so the whole parse ends at the
        // start of the line after the last one.
        let end_line = text.matches('\n').count() as i32 + 2;
        let whole = srcref_object(source, [1, 0, end_line, 0, 0, 0, 1, end_line])?;
        values::set_attr(result, symbols().srcref, srcrefs)?;
        values::set_attr(result, symbols().srcfile, srcfile)?;
        values::set_attr(result, symbols().whole_srcref, whole)?;
    }
    Ok(result)
}

/// Source text that parsed without a syntax error, with its top-level expressions still to be
/// lowered. A console lowers them one at a time, because `options(keep.source)` may change
/// between two of them.
pub struct Script<'text> {
    text: &'text str,
    pub expressions: Vec<SyntaxNode>,
}

/// Parses `text`, or reports the first syntax error in GNU R's form.
pub fn parse_script<'text>(text: &'text str, source_name: &str) -> R<Script<'text>> {
    let parse = syntax::parse(text);
    // A broken `#:` comment is a comment to R, so only R's own grammar can fail a parse.
    if let Some(failure) = parse.errors().iter().find(|failure| !failure.in_annotation) {
        let offset = usize::from(failure.range.start()).min(text.len());
        let before = &text[..offset];
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count());
        return Err(error(format!(
            "{source_name}:{line}:{column}: {}",
            failure.message
        )));
    }
    let may_hold_placeholder = text.contains('_');
    let mut expressions = Vec::new();
    for child in parse.syntax_node().children() {
        if is_skipped(&child) {
            continue;
        }
        if may_hold_placeholder && has_stray_placeholder(&child) {
            let offset = usize::from(child.text_range().start()).min(text.len());
            let line = text[..offset].matches('\n').count() + 1;
            return Err(error(format!(
                "invalid use of pipe placeholder ({source_name}:{line}:0)"
            )));
        }
        expressions.push(child);
    }
    Ok(Script { text, expressions })
}

/// Lowers one top-level expression of `script`. With `keep_source`, it keeps source references
/// against a srcfile of its own that holds the lines it spans, as GNU R's console keeps them for
/// each input it reads.
pub unsafe fn lower_script_expression(
    script: &Script<'_>,
    node: &SyntaxNode,
    keep_source: bool,
) -> R<Sexp> {
    if !keep_source {
        return lower(node, None);
    }
    let text = script.text;
    let range = significant_range(node);
    let start = text[..usize::from(range.start())]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let end = text[usize::from(range.end())..]
        .find('\n')
        .map_or(text.len(), |newline| usize::from(range.end()) + newline);
    let lines: Vec<&str> = text[start..end].split('\n').collect();
    let srcfile = values::new_env(values::empty_env())?;
    let heap = crate::heap();
    heap.define(srcfile, install("filename")?, values::scalar_string("")?)?;
    heap.define(srcfile, install("lines")?, values::string_vector(&lines)?)?;
    heap.define(
        srcfile,
        install("fixedNewlines")?,
        values::scalar_bool(true)?,
    )?;
    values::set_attr(
        srcfile,
        symbols().class,
        values::string_vector(&["srcfilecopy", "srcfile"])?,
    )?;
    let first_line = text[..start].matches('\n').count();
    let source = SourceMap::new(text, first_line, srcfile)?;
    lower(node, Some(&source))
}

/// What the lowering needs to build source references: the text with an index of its line
/// starts, the line that is line 1 of the srcfile, and the srcfile environment itself.
pub struct SourceMap<'text> {
    text: &'text str,
    line_starts: Vec<usize>,
    first_line: usize,
    srcfile: Sexp,
    /// The `srcref` class vector every srcref of this parse shares.
    class: Sexp,
}

impl<'text> SourceMap<'text> {
    unsafe fn new(text: &'text str, first_line: usize, srcfile: Sexp) -> R<SourceMap<'text>> {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Ok(SourceMap {
            text,
            line_starts,
            first_line,
            srcfile,
            class: values::scalar_string("srcref")?,
        })
    }
}

/// The srcref of the text in `range`: an integer vector of the first line and byte, the last line
/// and byte, the first and last column, then the first and last line again (GNU R's "parsed"
/// lines, which differ only after a `#line` directive). All are 1-based and the last ones
/// inclusive; bytes count UTF-8 bytes and columns count characters.
unsafe fn srcref(source: &SourceMap, range: TextRange) -> R<Sexp> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let last = source.text[..end]
        .chars()
        .next_back()
        .map_or(start, |character| end - character.len_utf8())
        .max(start);
    let (first_line, first_byte, first_column) = position(source, start);
    let (last_line, last_byte, last_column) = position(source, last);
    let last_byte = last_byte + (end - last).max(1) as i32 - 1;
    srcref_object(
        source,
        [
            first_line,
            first_byte,
            last_line,
            last_byte,
            first_column,
            last_column,
            first_line,
            last_line,
        ],
    )
}

unsafe fn srcref_object(source: &SourceMap, fields: [i32; 8]) -> R<Sexp> {
    let object = values::integer_vector(&fields)?;
    values::set_attr(object, symbols().srcfile, source.srcfile)?;
    values::set_attr(object, symbols().class, source.class)?;
    Ok(object)
}

/// The srcfile line, byte and column of the character at byte `offset`. A tab advances the column
/// to the next multiple of 8, as in GNU R's parser.
fn position(source: &SourceMap, offset: usize) -> (i32, i32, i32) {
    let line = source
        .line_starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1);
    let line_start = source.line_starts.get(line).copied().unwrap_or(0);
    let through = source.text[offset..]
        .chars()
        .next()
        .map_or(offset, |character| offset + character.len_utf8());
    let mut column = 0;
    for character in source.text[line_start..through].chars() {
        column = if character == '\t' {
            (column + 8) & !7
        } else {
            column + 1
        };
    }
    let line_number = line as i32 - source.first_line as i32 + 1;
    (line_number, (offset - line_start) as i32 + 1, column)
}

/// The range of a node from its first to its last token of R code, without the comments and
/// whitespace around it or `#:` annotations at its edges.
fn significant_range(node: &SyntaxNode) -> TextRange {
    let range = node.text_range();
    let significant = |token: &SyntaxToken| {
        !token.kind().is_trivia()
            && !token
                .parent_ancestors()
                .take_while(|ancestor| ancestor != node)
                .any(|ancestor| is_skipped(&ancestor))
    };
    let first = std::iter::successors(node.first_token(), SyntaxToken::next_token)
        .take_while(|token| range.contains_range(token.text_range()))
        .find(significant);
    let last = std::iter::successors(node.last_token(), SyntaxToken::prev_token)
        .take_while(|token| range.contains_range(token.text_range()))
        .find(significant);
    match (first, last) {
        (Some(first), Some(last)) => {
            TextRange::new(first.text_range().start(), last.text_range().end())
        }
        _ => range,
    }
}

/// Lowers one expression node of a tree that parsed without errors, such as a top-level item
/// of a file the analysis already parsed, so a host that holds the tree needs no second parse.
pub unsafe fn lower_node(node: &SyntaxNode) -> R<Sexp> {
    lower(node, None)
}

fn is_skipped(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::ANNOTATION
            | SyntaxKind::TYPE_ASCRIPTION
            | SyntaxKind::RETURN_TYPE
            | SyntaxKind::TYPE_BINDER_LIST
            | SyntaxKind::TYPE_DEFINITION
            | SyntaxKind::NEW_TYPE
    )
}

/// The expression children of a node, without trivia, tokens or annotations.
fn expression_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children().filter(|child| !is_skipped(child)).collect()
}

fn significant_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| !token.kind().is_trivia())
        .collect()
}

unsafe fn lower(node: &SyntaxNode, source: Option<&SourceMap>) -> R<Sexp> {
    match node.kind() {
        SyntaxKind::NAME => name_symbol(node),
        SyntaxKind::LITERAL => {
            let token = significant_tokens(node)
                .into_iter()
                .next()
                .ok_or_else(|| error("empty literal"))?;
            literal(&token)
        }
        SyntaxKind::PAREN_EXPR => {
            let inner = expression_children(node);
            let value = match inner.first() {
                Some(inner) => lower(inner, source)?,
                None => return Err(error("unexpected ')'")),
            };
            values::call(symbols().paren, &[value])
        }
        // `@Name(value)` runs as its `(value)`: the nominal type exists only for the checker.
        SyntaxKind::NEW_EXPR => match expression_children(node).first() {
            Some(inner) => lower(inner, source),
            None => Err(error("expected `(` after the nominal type")),
        },
        SyntaxKind::BRACE_EXPR => {
            let mut items = vec![(nil(), symbols().brace)];
            let statements = expression_children(node);
            for child in &statements {
                items.push((nil(), lower(child, source)?));
            }
            let call = values::pairlist(&items, SexpType::Language)?;
            if let Some(source) = source {
                block_srcrefs(call, node, &statements, source)?;
            }
            Ok(call)
        }
        SyntaxKind::BINARY_EXPR => binary(node, source),
        SyntaxKind::UNARY_EXPR => {
            let operator = significant_tokens(node)
                .into_iter()
                .next()
                .ok_or_else(|| error("missing operator"))?;
            let operand = expression_children(node)
                .into_iter()
                .next()
                .ok_or_else(|| error("missing operand"))?;
            let operand = lower(&operand, source)?;
            let symbol = install(operator.text())?;
            values::call(symbol, &[operand])
        }
        SyntaxKind::CALL_EXPR => {
            let children = expression_children(node);
            let (function, arguments) = match children.as_slice() {
                [function, arguments, ..] if arguments.kind() == SyntaxKind::ARGUMENT_LIST => {
                    (function, arguments)
                }
                _ => return Err(error("malformed call")),
            };
            let mut function = lower(function, source)?;
            // R's parser turns a string in function position into a symbol: `"f<-"(x)`.
            if function.kind() == SexpType::String && values::length(function) == 1 {
                function = values::install_bytes(function.element(0).bytes())?;
            }
            let mut items = vec![(nil(), function)];
            items.extend(argument_list(arguments, source)?);
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
            let children = expression_children(node);
            let (object, arguments) = match children.as_slice() {
                [object, arguments, ..] => (object, arguments),
                _ => return Err(error("malformed subset")),
            };
            let operator = if node.kind() == SyntaxKind::SUBSET_EXPR {
                symbols().bracket
            } else {
                symbols().bracket2
            };
            let mut items = vec![(nil(), operator), (nil(), lower(object, source)?)];
            items.extend(argument_list(arguments, source)?);
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR | SyntaxKind::NAMESPACE_EXPR => {
            let children = expression_children(node);
            let operator = significant_tokens(node)
                .into_iter()
                .find(|token| {
                    matches!(
                        token.kind(),
                        SyntaxKind::DOLLAR
                            | SyntaxKind::AT
                            | SyntaxKind::COLON2
                            | SyntaxKind::COLON3
                    )
                })
                .ok_or_else(|| error("missing operator"))?;
            let (left, right) = match children.as_slice() {
                [left, right, ..] => (lower(left, source)?, lower(right, source)?),
                [left] => {
                    // A keyword or string on the right is a token, not a node.
                    let token = significant_tokens(node)
                        .into_iter()
                        .last()
                        .ok_or_else(|| error("missing name"))?;
                    (lower(left, source)?, literal(&token)?)
                }
                _ => return Err(error("malformed extraction")),
            };
            let symbol = install(operator.text())?;
            values::call(symbol, &[left, right])
        }
        SyntaxKind::FUNCTION_DEF => function_definition(node, source),
        SyntaxKind::IF_EXPR => {
            let children = expression_children(node);
            let mut items = vec![(nil(), symbols().if_)];
            for child in &children {
                items.push((nil(), lower(child, source)?));
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::FOR_EXPR => {
            let children = expression_children(node);
            let mut items = vec![(nil(), symbols().for_)];
            for child in &children {
                items.push((nil(), lower(child, source)?));
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::WHILE_EXPR => {
            let children = expression_children(node);
            let mut items = vec![(nil(), symbols().while_)];
            for child in &children {
                items.push((nil(), lower(child, source)?));
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::REPEAT_EXPR => {
            let children = expression_children(node);
            let mut items = vec![(nil(), symbols().repeat)];
            for child in &children {
                items.push((nil(), lower(child, source)?));
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::RECORD_EXPR | SyntaxKind::TUPLE_EXPR => {
            let head = if node.kind() == SyntaxKind::RECORD_EXPR {
                ".ry_record"
            } else {
                ".ry_tuple"
            };
            let mut items = vec![(nil(), install(head)?)];
            if let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                items.extend(literal_items(&arguments, source)?);
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::TAG_EXPR => {
            let name =
                child_of_kind(node, SyntaxKind::NAME).ok_or_else(|| error("a tag needs a name"))?;
            let mut items = vec![(nil(), install(".ry_tag")?), (nil(), tag_name(&name)?)];
            if let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                items.extend(literal_items(&arguments, source)?);
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::MATCH_EXPR => match_expression(node, source),
        SyntaxKind::BREAK_EXPR => values::call(symbols().break_, &[]),
        SyntaxKind::NEXT_EXPR => values::call(symbols().next, &[]),
        SyntaxKind::ERROR => Err(error(format!("unexpected '{}'", node.text()))),
        kind => Err(error(format!("cannot lower {}", kind.display()))),
    }
}

/// Attaches a `{` call's source references: a `srcref` list of the brace itself and then each
/// statement, the srcfile, and a `wholeSrcref` from the start of the srcfile to the closing brace.
unsafe fn block_srcrefs(
    call: Sexp,
    node: &SyntaxNode,
    statements: &[SyntaxNode],
    source: &SourceMap,
) -> R<()> {
    let range = significant_range(node);
    let srcrefs = alloc(SexpType::List, statements.len() + 1)?;
    let open = TextRange::at(range.start(), 1.into());
    values::set_elt(srcrefs, 0, srcref(source, open)?);
    for (index, statement) in statements.iter().enumerate() {
        values::set_elt(
            srcrefs,
            index + 1,
            srcref(source, significant_range(statement))?,
        );
    }
    let close = srcref(source, range)?;
    let fields = values::ints(close);
    let (last_line, last_byte, last_column) = (fields[2], fields[3], fields[5]);
    let whole = srcref_object(
        source,
        [1, 0, last_line, last_byte, 0, last_column, 1, last_line],
    )?;
    values::set_attr(call, symbols().srcref, srcrefs)?;
    values::set_attr(call, symbols().srcfile, source.srcfile)?;
    values::set_attr(call, symbols().whole_srcref, whole)?;
    Ok(())
}

unsafe fn binary(node: &SyntaxNode, source: Option<&SourceMap>) -> R<Sexp> {
    let children = expression_children(node);
    let operator = significant_tokens(node)
        .into_iter()
        .find(|token| !matches!(token.kind(), SyntaxKind::L_PAREN | SyntaxKind::R_PAREN))
        .ok_or_else(|| error("missing operator"))?;
    let (left, right) = match children.as_slice() {
        [left, right, ..] => (left, Some(right)),
        [left] => (left, None),
        _ => return Err(error("malformed binary expression")),
    };
    let left_value = lower(left, source)?;
    let Some(right) = right else {
        // A formula with only a left side is `~` applied to it.
        let symbol = install(operator.text())?;
        return values::call(symbol, &[left_value]);
    };
    let right_value = lower(right, source)?;
    match operator.kind() {
        SyntaxKind::MINUS_GREATER => values::call(symbols().assign, &[right_value, left_value]),
        SyntaxKind::MINUS_GREATER2 => values::call(symbols().assign2, &[right_value, left_value]),
        SyntaxKind::PIPE_GREATER => pipe(left_value, right_value, placeholder_name(right)?),
        _ => {
            let symbol = install(operator.text())?;
            values::call(symbol, &[left_value, right_value])
        }
    }
}

/// `lhs |> f(args)` is `f(lhs, args)`, and `lhs |> f(x, y = _)` is `f(x, y = lhs)`, where
/// `placeholder` names the argument that holds the placeholder.
unsafe fn pipe(left: Sexp, right: Sexp, placeholder: Option<Sexp>) -> R<Sexp> {
    if right.kind() != SexpType::Language {
        return Err(error("The pipe operator requires a function call as RHS"));
    }
    let head = right.car();
    if head == symbols().function {
        return Err(error(
            "function 'function' not supported in RHS call of a pipe",
        ));
    }
    if let Some(tag) = placeholder {
        let mut argument = right.cdr();
        while argument != nil() {
            if argument.tag() == tag && argument.car() == install("_")? {
                crate::heap().set_car(argument, left);
                return Ok(right);
            }
            argument = argument.cdr();
        }
    }
    let rest = values::cons(left, right.cdr())?;
    values::lcons(head, rest)
}

/// The placeholder argument of a pipe's right side: the first named argument of the call whose
/// value is a bare `_`. A backquoted `` `_` `` is an ordinary name, so only the token counts.
fn placeholder_argument(right: &SyntaxNode) -> Option<SyntaxNode> {
    if right.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let arguments = right
        .children()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)?;
    arguments
        .children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .find(|argument| {
            let mut after_equals = false;
            argument
                .children_with_tokens()
                .any(|element| match element {
                    SyntaxElement::Token(token) => {
                        after_equals |= token.kind() == SyntaxKind::EQ;
                        false
                    }
                    SyntaxElement::Node(value) => after_equals && is_bare_placeholder(&value),
                })
        })
}

unsafe fn placeholder_name(right: &SyntaxNode) -> R<Option<Sexp>> {
    let Some(argument) = placeholder_argument(right) else {
        return Ok(None);
    };
    match argument.children().next() {
        Some(name) => tag_from_node(&name).map(Some),
        None => Ok(None),
    }
}

fn is_bare_placeholder(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::NAME
        && significant_tokens(node)
            .first()
            .is_some_and(|token| token.kind() == SyntaxKind::UNDERSCORE)
}

/// Whether a `_` in `node` is not the placeholder argument of a pipe, which R rejects.
fn has_stray_placeholder(node: &SyntaxNode) -> bool {
    let placeholders = node.descendants().filter(is_bare_placeholder).count();
    let consumed = node
        .descendants()
        .filter(|expression| {
            expression.kind() == SyntaxKind::BINARY_EXPR
                && significant_tokens(expression)
                    .iter()
                    .any(|token| token.kind() == SyntaxKind::PIPE_GREATER)
                && expression_children(expression)
                    .get(1)
                    .is_some_and(|right| placeholder_argument(right).is_some())
        })
        .count();
    placeholders > consumed
}

fn child_of_kind(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|child| child.kind() == kind)
}

/// The items of a record, tuple or tag payload. Unlike a call's arguments, a trailing comma adds
/// no empty item: `(1L,)` has one.
unsafe fn literal_items(node: &SyntaxNode, source: Option<&SourceMap>) -> R<Vec<(Sexp, Sexp)>> {
    node.children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .map(|argument| argument_value(&argument, source))
        .collect()
}

/// A tag's name as the string `.ry_tag` takes first.
unsafe fn tag_name(name: &SyntaxNode) -> R<Sexp> {
    let token = significant_tokens(name)
        .into_iter()
        .next()
        .ok_or_else(|| error("empty tag name"))?;
    values::scalar_string(&identifier(token.text()))
}

/// `match (x) { arms }` becomes `.ry_match(x, `=>`(pattern, body), `=>`(pattern, guard, body))`.
/// A parenthesized scrutinee lowers to its inner expression, because the parentheses belong to
/// the `match` syntax, as they do for `if`.
unsafe fn match_expression(node: &SyntaxNode, source: Option<&SourceMap>) -> R<Sexp> {
    let children = expression_children(node);
    let (scrutinee, arms) = children
        .split_first()
        .ok_or_else(|| error("a `match` needs a value"))?;
    let scrutinee = if scrutinee.kind() == SyntaxKind::PAREN_EXPR {
        let inner = expression_children(scrutinee);
        lower(
            inner
                .first()
                .ok_or_else(|| error("a `match` needs a value"))?,
            source,
        )?
    } else {
        lower(scrutinee, source)?
    };
    let arrow = install("=>")?;
    let mut items = vec![(nil(), install(".ry_match")?), (nil(), scrutinee)];
    for arm in arms
        .iter()
        .filter(|arm| arm.kind() == SyntaxKind::MATCH_ARM)
    {
        let parts = expression_children(arm);
        let mut arm_items = vec![(nil(), arrow)];
        let mut parts = parts.iter();
        let pattern_node = parts
            .next()
            .ok_or_else(|| error("a `match` arm needs a pattern"))?;
        arm_items.push((nil(), pattern(pattern_node)?));
        for part in parts {
            if part.kind() == SyntaxKind::MATCH_GUARD {
                let condition = expression_children(part);
                let condition = condition
                    .first()
                    .ok_or_else(|| error("an arm guard needs a condition"))?;
                arm_items.push((nil(), lower(condition, source)?));
            } else {
                arm_items.push((nil(), lower(part, source)?));
            }
        }
        if arm_items.len() < 3 {
            return Err(error("a `match` arm needs a value after `=>`"));
        }
        items.push((nil(), values::pairlist(&arm_items, SexpType::Language)?));
    }
    values::pairlist(&items, SexpType::Language)
}

/// A pattern stays unevaluated in the arm, in the encoding of the values it matches: `_` and a
/// binding are symbols, a literal is its constant (a negative number folded), and tag, tuple
/// and record patterns are `.ry_tag`, `.ry_tuple` and `.ry_record` calls, with the symbol `..`
/// for the rest and `|` and `(` calls for alternatives and grouping.
unsafe fn pattern(node: &SyntaxNode) -> R<Sexp> {
    match node.kind() {
        SyntaxKind::PATTERN_WILDCARD => install("_"),
        SyntaxKind::PATTERN_REST => install(".."),
        SyntaxKind::PATTERN_BIND => {
            let name = child_of_kind(node, SyntaxKind::NAME)
                .ok_or_else(|| error("empty binding pattern"))?;
            name_symbol(&name)
        }
        SyntaxKind::PATTERN_LITERAL => {
            let negative = significant_tokens(node)
                .iter()
                .any(|token| token.kind() == SyntaxKind::MINUS);
            let literal_node = child_of_kind(node, SyntaxKind::LITERAL)
                .ok_or_else(|| error("empty literal pattern"))?;
            let token = significant_tokens(&literal_node)
                .into_iter()
                .next()
                .ok_or_else(|| error("empty literal pattern"))?;
            let value = literal(&token)?;
            if negative { negate(value) } else { Ok(value) }
        }
        SyntaxKind::PATTERN_TAG => {
            let name = child_of_kind(node, SyntaxKind::NAME)
                .ok_or_else(|| error("a tag pattern needs a name"))?;
            let mut items = vec![(nil(), install(".ry_tag")?), (nil(), tag_name(&name)?)];
            if let Some(list) = child_of_kind(node, SyntaxKind::PATTERN_LIST) {
                items.extend(pattern_items(&list)?);
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::PATTERN_LIST => {
            let entries = pattern_items(node)?;
            let record = entries.iter().any(|(tag, _)| *tag != nil());
            let head = install(if record { ".ry_record" } else { ".ry_tuple" })?;
            let mut items = vec![(nil(), head)];
            items.extend(entries);
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::PATTERN_OR => {
            let mut alternatives = node.children().map(|child| pattern(&child));
            let mut result = alternatives
                .next()
                .ok_or_else(|| error("empty alternative pattern"))??;
            let pipe = install("|")?;
            for alternative in alternatives {
                result = values::call(pipe, &[result, alternative?])?;
            }
            Ok(result)
        }
        SyntaxKind::PATTERN_PAREN => {
            let inner = node
                .children()
                .next()
                .ok_or_else(|| error("empty pattern group"))?;
            values::call(symbols().paren, &[pattern(&inner)?])
        }
        _ => Err(error(format!("unexpected '{}' in a pattern", node.text()))),
    }
}

unsafe fn pattern_items(list: &SyntaxNode) -> R<Vec<(Sexp, Sexp)>> {
    let mut items = Vec::new();
    for child in list.children() {
        if child.kind() == SyntaxKind::PATTERN_FIELD {
            let name = child_of_kind(&child, SyntaxKind::NAME)
                .ok_or_else(|| error("a field pattern needs a name"))?;
            let value = child
                .children()
                .find(|part| part.kind() != SyntaxKind::NAME)
                .ok_or_else(|| error("a field pattern needs a pattern"))?;
            items.push((name_symbol(&name)?, pattern(&value)?));
        } else {
            items.push((nil(), pattern(&child)?));
        }
    }
    Ok(items)
}

/// A negative literal pattern is stored as the negative constant, because a pattern is never
/// evaluated.
unsafe fn negate(value: Sexp) -> R<Sexp> {
    match value.kind() {
        SexpType::Integer => {
            let number = values::ints(value)[0];
            values::scalar_integer(if number == NA_INTEGER {
                number
            } else {
                -number
            })
        }
        SexpType::Real => values::scalar_real(-values::reals(value)[0]),
        SexpType::Complex => {
            let number = values::complexes(value)[0];
            values::scalar_complex(Rcomplex {
                r: -number.r,
                i: -number.i,
            })
        }
        _ => Err(error("only a number can be negated in a pattern")),
    }
}

/// The arguments of a call or subset, with empty arguments as `R_MissingArg`.
unsafe fn argument_list(node: &SyntaxNode, source: Option<&SourceMap>) -> R<Vec<(Sexp, Sexp)>> {
    // An argument slot sits between separators; R keeps empty slots (`x[1, ]`).
    let mut slots: Vec<Option<SyntaxNode>> = vec![None];
    let mut saw_any = false;
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => {
                slots.push(None);
                saw_any = true;
            }
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::ARGUMENT => {
                if let Some(slot) = slots.last_mut() {
                    *slot = Some(child);
                }
                saw_any = true;
            }
            _ => {}
        }
    }
    if !saw_any {
        return Ok(Vec::new());
    }
    let mut arguments = Vec::new();
    for slot in slots {
        match slot {
            None => arguments.push((nil(), missing_arg())),
            Some(argument) => arguments.push(argument_value(&argument, source)?),
        }
    }
    Ok(arguments)
}

unsafe fn argument_value(node: &SyntaxNode, source: Option<&SourceMap>) -> R<(Sexp, Sexp)> {
    let has_equals = significant_tokens(node)
        .iter()
        .any(|token| token.kind() == SyntaxKind::EQ);
    let mut elements = node
        .children_with_tokens()
        .filter(|element| !element.kind().is_trivia());
    if !has_equals {
        return match elements.next() {
            Some(SyntaxElement::Node(child)) => Ok((nil(), lower(&child, source)?)),
            Some(SyntaxElement::Token(token)) => Ok((nil(), literal(&token)?)),
            None => Ok((nil(), missing_arg())),
        };
    }
    let mut tag = nil();
    let mut value = missing_arg();
    let mut after_equals = false;
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::EQ && !after_equals => {
                after_equals = true;
            }
            SyntaxElement::Token(token) if token.kind().is_trivia() => {}
            SyntaxElement::Node(child) if !after_equals => {
                tag = tag_from_node(&child)?;
            }
            SyntaxElement::Token(token) if !after_equals => {
                tag = install(&token_name(&token)?)?;
            }
            SyntaxElement::Node(child) => value = lower(&child, source)?,
            SyntaxElement::Token(token) => value = literal(&token)?,
        }
    }
    Ok((tag, value))
}

/// The tag of a named argument or parameter: a name, a string, or `NULL`.
unsafe fn tag_from_node(node: &SyntaxNode) -> R<Sexp> {
    match node.kind() {
        SyntaxKind::NAME => name_symbol(node),
        SyntaxKind::LITERAL => {
            let token = significant_tokens(node)
                .into_iter()
                .next()
                .ok_or_else(|| error("empty literal"))?;
            install(&token_name(&token)?)
        }
        _ => Err(error("invalid argument name")),
    }
}

unsafe fn token_name(token: &SyntaxToken) -> R<String> {
    match token.kind() {
        SyntaxKind::STRING => string_literal(token.text()),
        SyntaxKind::RAW_STRING => Ok(raw_string(token.text())),
        SyntaxKind::IDENT => Ok(identifier(token.text())),
        _ => Ok(token.text().to_owned()),
    }
}

unsafe fn name_symbol(node: &SyntaxNode) -> R<Sexp> {
    let token = significant_tokens(node)
        .into_iter()
        .next()
        .ok_or_else(|| error("empty name"))?;
    match token.kind() {
        SyntaxKind::IDENT => install(&identifier(token.text())),
        SyntaxKind::DOTS => Ok(symbols().dots),
        _ => install(token.text()),
    }
}

/// An identifier's name, without backticks.
fn identifier(text: &str) -> String {
    match text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
    {
        Some(inner) => unescape(inner).unwrap_or_else(|_| inner.to_owned()),
        None => text.to_owned(),
    }
}

unsafe fn function_definition(node: &SyntaxNode, source: Option<&SourceMap>) -> R<Sexp> {
    let mut formals = Vec::new();
    let mut body = None;
    for child in expression_children(node) {
        if child.kind() == SyntaxKind::PARAMETER_LIST {
            for parameter in child.children() {
                if parameter.kind() == SyntaxKind::PARAMETER {
                    formals.push(parameter_value(&parameter, source)?);
                }
            }
        } else {
            body = Some(lower(&child, source)?);
        }
    }
    let formals = values::pairlist(&formals, SexpType::Pairlist)?;
    let body = body.unwrap_or(nil());
    crate::annotations::register_definition(node, formals, body)?;
    let srcref = match source {
        Some(source) => srcref(source, significant_range(node))?,
        None => nil(),
    };
    let items = [
        (nil(), symbols().function),
        (nil(), formals),
        (nil(), body),
        (nil(), srcref),
    ];
    values::pairlist(&items, SexpType::Language)
}

unsafe fn parameter_value(node: &SyntaxNode, source: Option<&SourceMap>) -> R<(Sexp, Sexp)> {
    let mut tag = nil();
    let mut default = missing_arg();
    let mut after_equals = false;
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind().is_trivia() => {}
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::EQ => after_equals = true,
            SyntaxElement::Token(token) if !after_equals => {
                tag = match token.kind() {
                    SyntaxKind::DOTS => symbols().dots,
                    _ => install(&token_name(&token)?)?,
                };
            }
            SyntaxElement::Node(child) if is_skipped(&child) => {}
            SyntaxElement::Node(child) if !after_equals => tag = tag_from_node(&child)?,
            SyntaxElement::Node(child) => default = lower(&child, source)?,
            SyntaxElement::Token(token) => default = literal(&token)?,
        }
    }
    Ok((tag, default))
}

unsafe fn literal(token: &SyntaxToken) -> R<Sexp> {
    let text = token.text();
    let known = crate::heap().known();
    match token.kind() {
        SyntaxKind::DOUBLE => values::scalar_real(parse_number(text)),
        SyntaxKind::INTEGER => {
            let body = &text[..text.len() - 1];
            let value = parse_number(body);
            if value.fract() == 0.0 && value.abs() <= i32::MAX as f64 {
                values::scalar_integer(value as i32)
            } else {
                values::scalar_real(value)
            }
        }
        SyntaxKind::COMPLEX => {
            let body = &text[..text.len() - 1];
            values::scalar_complex(Rcomplex {
                r: 0.0,
                i: parse_number(body),
            })
        }
        SyntaxKind::STRING => {
            let content = string_literal(text)?;
            values::scalar_string(&content)
        }
        SyntaxKind::RAW_STRING => values::scalar_string(&raw_string(text)),
        SyntaxKind::TRUE_KW => Ok(values::fresh_logical(1)?),
        SyntaxKind::FALSE_KW => Ok(values::fresh_logical(0)?),
        SyntaxKind::NULL_KW => Ok(known.nil),
        SyntaxKind::NA_KW => Ok(values::fresh_logical(NA_LOGICAL)?),
        SyntaxKind::NA_INTEGER_KW => values::scalar_integer(NA_INTEGER),
        SyntaxKind::NA_REAL_KW => values::scalar_real(NA_REAL),
        SyntaxKind::NA_CHARACTER_KW => {
            let result = alloc(SexpType::String, 1)?;
            values::set_elt(result, 0, values::na_string());
            Ok(result)
        }
        SyntaxKind::NA_COMPLEX_KW => values::scalar_complex(Rcomplex {
            r: NA_REAL,
            i: NA_REAL,
        }),
        SyntaxKind::INF_KW => values::scalar_real(f64::INFINITY),
        SyntaxKind::NAN_KW => values::scalar_real(f64::NAN),
        SyntaxKind::IDENT => install(&identifier(text)),
        SyntaxKind::DOTS => Ok(symbols().dots),
        SyntaxKind::DOTDOTI => install(text),
        _ => install(text),
    }
}

/// A numeric literal: decimal, hexadecimal, or hexadecimal with a binary exponent.
pub fn parse_number(text: &str) -> f64 {
    let lower = text.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        let (mantissa, exponent) = match hex.split_once('p') {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
            None => (hex, 0),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let mut value = 0.0f64;
        for digit in whole.chars() {
            value = value * 16.0 + digit.to_digit(16).unwrap_or(0) as f64;
        }
        let mut scale = 1.0 / 16.0;
        for digit in fraction.chars() {
            value += digit.to_digit(16).unwrap_or(0) as f64 * scale;
            scale /= 16.0;
        }
        // Zero stays zero under any exponent, where 2^exponent alone would overflow to Inf.
        return if value == 0.0 {
            0.0
        } else {
            value * 2f64.powi(exponent)
        };
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// The content of a quoted string literal, with escapes resolved.
pub fn string_literal(text: &str) -> R<String> {
    let inner = &text[1..text.len().saturating_sub(1).max(1)];
    unescape(inner).map_err(|message| error(message))
}

fn raw_string(text: &str) -> String {
    // r"---( ... )---": the prefix up to the opening bracket, mirrored at the end.
    let after_r = &text[1..];
    let rest = &after_r[1..];
    let dashes = rest
        .chars()
        .take_while(|&character| character == '-')
        .count();
    let open = rest[dashes..].chars().next().unwrap_or('(');
    let body_start = 1 + 1 + dashes + open.len_utf8();
    let body_end = text.len() - (1 + dashes + 1);
    text.get(body_start..body_end).unwrap_or("").to_owned()
}

/// Resolves R's string escapes.
pub fn unescape(text: &str) -> Result<String, String> {
    let mut result = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        let Some(escape) = characters.next() else {
            result.push('\\');
            break;
        };
        match escape {
            'n' => result.push('\n'),
            't' => result.push('\t'),
            'r' => result.push('\r'),
            'a' => result.push('\u{7}'),
            'b' => result.push('\u{8}'),
            'f' => result.push('\u{c}'),
            'v' => result.push('\u{b}'),
            '\\' => result.push('\\'),
            '"' => result.push('"'),
            '\'' => result.push('\''),
            '`' => result.push('`'),
            ' ' => result.push(' '),
            '\n' => result.push('\n'),
            '0'..='7' => {
                let mut value = escape.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match characters.peek().and_then(|next| next.to_digit(8)) {
                        Some(digit) => {
                            value = value * 8 + digit;
                            characters.next();
                        }
                        None => break,
                    }
                }
                if value == 0 {
                    return Err("nul character not allowed".into());
                }
                result.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            }
            'x' => {
                let mut value = 0;
                let mut count = 0;
                while count < 2 {
                    match characters.peek().and_then(|next| next.to_digit(16)) {
                        Some(digit) => {
                            value = value * 16 + digit;
                            characters.next();
                            count += 1;
                        }
                        None => break,
                    }
                }
                if count == 0 {
                    return Err("'\\x' used without hex digits in character string".into());
                }
                if value == 0 {
                    return Err("nul character not allowed".into());
                }
                result.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            }
            'u' | 'U' => {
                let limit = if escape == 'u' { 4 } else { 8 };
                let braced = characters.peek() == Some(&'{');
                if braced {
                    characters.next();
                }
                let mut value = 0u32;
                let mut count = 0;
                while count < limit {
                    match characters.peek().and_then(|next| next.to_digit(16)) {
                        Some(digit) => {
                            value = value * 16 + digit;
                            characters.next();
                            count += 1;
                        }
                        None => break,
                    }
                }
                if braced && characters.peek() == Some(&'}') {
                    characters.next();
                }
                if count == 0 {
                    return Err(format!(
                        "'\\{escape}' used without hex digits in character string"
                    ));
                }
                result.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            }
            other => {
                return Err(format!(
                    "'\\{other}' is an unrecognized escape in character string"
                ));
            }
        }
    }
    Ok(result)
}

// The internals.

/// `parse(file, n, text, prompt, srcfile, encoding)`.
unsafe fn do_parse(call: &Call<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let count = values::as_integer_scalar(call.arg(1));
    let text = call.arg(2);
    let (source, name) = if text != nil() {
        let lines: Vec<String> =
            values::strings(crate::coerce::coerce_vector(text, SexpType::String)?)
                .into_iter()
                .map(|line| line.unwrap_or_else(|| "NA".into()))
                .collect();
        (lines.join("\n"), "<text>".to_owned())
    } else {
        let content = crate::connections::read_all_text(file)?;
        let name = crate::connections::description(file).unwrap_or_else(|| "<file>".into());
        (content, name)
    };
    let expressions = parse_with_srcfile(&source, &name, call.arg(4))?;
    truncate_parse(expressions, count)
}

/// The first `count` expressions of a parse with their source references, or all of them when
/// `count` is negative.
pub unsafe fn truncate_parse(expressions: Sexp, count: i32) -> R<Sexp> {
    if count < 0 || count as usize >= values::length(expressions) {
        return Ok(expressions);
    }
    let count = count as usize;
    let truncated = alloc(SexpType::Expression, count)?;
    for index in 0..count {
        values::set_elt(truncated, index, expressions.element(index));
    }
    let srcrefs = values::get_attr(expressions, symbols().srcref);
    if srcrefs != nil() {
        let kept = alloc(SexpType::List, count)?;
        for index in 0..count {
            values::set_elt(kept, index, srcrefs.element(index));
        }
        for (name, value) in [
            (symbols().srcref, kept),
            (
                symbols().srcfile,
                values::get_attr(expressions, symbols().srcfile),
            ),
            (
                symbols().whole_srcref,
                values::get_attr(expressions, symbols().whole_srcref),
            ),
        ] {
            values::set_attr(truncated, name, value)?;
        }
    }
    Ok(truncated)
}

unsafe fn do_str2lang(call: &Call<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String || values::length(text) != 1 {
        return Err(call.error("argument must be a character string"));
    }
    let source = values::string_at(text, 0).unwrap_or_default().into_owned();
    let expressions = parse_text(&source, "<text>")?;
    match values::length(expressions) {
        1 => Ok(expressions.element(0)),
        count => Err(call.error(format!("parsing result not of length one, but {count}"))),
    }
}

unsafe fn do_str2expression(call: &Call<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(call.error("argument must be a character vector"));
    }
    let lines: Vec<String> = values::strings(text)
        .into_iter()
        .map(|line| line.unwrap_or_else(|| "NA".into()))
        .collect();
    parse_text(&lines.join("\n"), "<text>")
}
