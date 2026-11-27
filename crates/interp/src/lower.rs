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

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        connections::{multibyte_length, utf8_sequence_length},
        format::{encode_bytes, encode_string},
        unwind::{R, Unwind, error},
        values::{self, alloc, escaped_byte, holds_stand_in, install, missing_arg, nil, symbols},
    },
    heap::{
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType},
        strings::Encoding,
    },
    numeric::long_double::{Exactness, strtod},
    smallvec::SmallVec,
    std::{borrow::Cow, sync::OnceLock},
    syntax::{
        Classed, ElementRef, EscapeErrorKind, EscapePiece, NodeOrToken, NodeRef, NumberType,
        PipeCheck, SyntaxElement, SyntaxError, SyntaxKind, SyntaxNode, SyntaxToken, TextRange,
        TextSize, TokenRef, Unexpected, raw_string_content, split_number_suffix, unescape_quoted,
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("parse", 6, do_parse),
    internal("str2lang", 1, do_str2lang),
    internal("str2expression", 1, do_str2expression),
];

/// The host's rewrite of the text of a file that `source()` or `parse(file = )` reads, given
/// the file's name and text, into the program the checker elaborates (`preflight::elaborate` in
/// the `ry` crate): `Ok(None)` when nothing changes, and `Err` when the host could not check the
/// file, after it reported why.
pub type SourceElaborator = fn(&str, &str) -> Result<Option<String>, ()>;

static SOURCE_ELABORATOR: OnceLock<SourceElaborator> = OnceLock::new();

/// Makes every parse of a file run the text `elaborator` gives for it. A host that checks code
/// sets it once per session (`libr::loader_backend::set_check`); without one, a file parses as
/// written.
pub fn set_source_elaborator(elaborator: SourceElaborator) {
    if SOURCE_ELABORATOR.set(elaborator).is_err() {
        panic!("the host sets the source elaborator once per session");
    }
}

/// Parses `text` into an expression vector, or reports the first syntax error in GNU R's form.
pub unsafe fn parse_text(text: &str, source_name: &str) -> R<Sexp> {
    parse_with_srcfile(text, source_name, nil())
}

/// Parses `text` like [`parse_text`], and keeps source references against `srcfile` when it is a
/// srcfile environment, as GNU R's parser does when `parse` is given one.
pub unsafe fn parse_with_srcfile(text: &str, source_name: &str, srcfile: Sexp) -> R<Sexp> {
    let condition_file = if srcfile.kind() == SexpType::Environment {
        let filename = crate::eval::find_var_in_frame(install("filename")?, srcfile)?;
        match filename.kind() {
            SexpType::String => values::string_at(filename, 0)
                .map_or_else(|| "<input>".to_owned(), |name| name.into_owned()),
            _ => "<input>".to_owned(),
        }
    } else {
        "<input>".to_owned()
    };
    let script = parse_script(text, source_name, &condition_file)?;
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
    // A function's signature can come from a `#:` block beside its statement, so the whole
    // tree decides whether any function needs one looked up.
    let typed = script.root.node_ref().any_kind(is_type_syntax);
    for (index, node) in script.expressions.iter().enumerate() {
        let context = Lowering {
            source: source.as_ref(),
            owner: node,
            typed,
            placeholder: None,
        };
        let cursor = node.node_ref();
        values::set_elt(result, index, lower(cursor, context)?);
        if let Some(source) = &source {
            values::set_elt(srcrefs, index, srcref(source, significant_range(cursor))?);
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
        // Package loaders and installers turn parse data off; building it costs more than
        // the parse itself on a large file.
        let keep_data = crate::envir::get_option("keep.parse.data");
        if (keep_data == nil() || values::as_logical_scalar(keep_data) == 1)
            && let Some(data) = crate::parse_data::parse_data(script.root.node_ref(), source)?
        {
            crate::heap().define(srcfile, install("parseData")?, data)?;
        }
    }
    Ok(result)
}

/// Source text that parsed without a syntax error, with its top-level expressions still to be
/// lowered. A console lowers them one at a time, because `options(keep.source)` may change
/// between two of them.
pub struct Script<'text> {
    text: &'text str,
    root: SyntaxNode,
    pub expressions: Vec<SyntaxNode>,
}

/// A script's first syntax error: where it is in the text, and GNU R's message for it without
/// the location. Most of GNU R's parse errors are plain errors, but the ones its grammar actions
/// and lexer raise (a repeated formal argument, a misused pipe, a bad string escape) are
/// conditions of class `parseError`, which `report` describes.
pub struct SyntaxFailure {
    pub offset: usize,
    pub message: String,
    pub report: FailureReport,
}

/// The form GNU R reports a syntax error in.
pub enum FailureReport {
    /// A message of ry's own, for text GNU R has no report for, with the location of `offset`.
    Located,
    /// An error of R's grammar, with the token GNU R's parser stops on (an empty range at the
    /// end of the input), whose location and preceding lines GNU R's message quotes. `offset`
    /// is where ry's parser anchors the error, which decides how much of a script runs first.
    Grammar(std::ops::Range<usize>),
    /// A condition of class `parseError`.
    Condition(ParseCondition),
    /// A plain error without a location, as GNU R raises when it makes a symbol of an empty name.
    Plain,
}

/// The class and fields of a `parseError` condition.
pub struct ParseCondition {
    pub class: &'static str,
    pub value: ConditionValue,
    pub position: ConditionPosition,
    /// The expression the condition names as its call.
    pub call: Option<SyntaxNode>,
}

/// Where a `parseError` condition says it is, as a place in the text.
pub enum ConditionPosition {
    /// The offset of the last character GNU R's lexer read before it raised the condition,
    /// whose line and column the message reports.
    Read(usize),
    /// The offset of the line break, or the end of the text, that ends the top-level
    /// expression the condition is about. GNU R reports column 0 of the line it ends.
    LineBreak(usize),
}

/// The `value` field of a `parseError` condition.
pub enum ConditionValue {
    Null,
    Text(String),
    Integer(i32),
}

/// Parses `text`, or reports the first syntax error in GNU R's form. `condition_file` is the
/// file name a `parseError` condition reports.
pub unsafe fn parse_script<'text>(
    text: &'text str,
    source_name: &str,
    condition_file: &str,
) -> R<Script<'text>> {
    let (script, failure) = parse_script_prefix(text);
    match failure {
        None => Ok(script),
        Some(failure) => Err(syntax_failure_unwind(
            text,
            &failure,
            source_name,
            condition_file,
            None,
        )),
    }
}

/// Parses `text` as far as it goes: the expressions a newline or `;` ends before the first
/// syntax error, and that error. GNU R runs each expression of a script once it is complete,
/// so those run before the error stops the script.
///
/// The verdict is the `syntax` crate's: the text fails exactly when its parse holds an error
/// outside a `#:` comment. GNU R stops at the first error it meets where ry's parser reports
/// every one, so the error reported is the one GNU R meets first ([`error_order`]), in GNU R's
/// words.
pub unsafe fn parse_script_prefix(text: &str) -> (Script<'_>, Option<SyntaxFailure>) {
    let parse = syntax::parse(text);
    let root = parse.syntax_node();
    // A broken `#:` comment is a comment to R, so only R's own grammar can fail a parse.
    let errors = || parse.errors().iter().filter(|error| !error.in_annotation);
    let open_from = errors()
        .filter(|error| {
            matches!(
                error.unexpected,
                Unexpected::EndOfInput | Unexpected::EndOfLine
            )
        })
        .map(|error| usize::from(error.range.start()))
        .max();
    let refused: SmallVec<[usize; 4]> = errors()
        .filter_map(|error| match error.unexpected {
            Unexpected::Token(found) => Some(found.start().into()),
            Unexpected::Nothing | Unexpected::Classed(Classed::EmptyStatement) => {
                Some(error.range.start().into())
            }
            _ => None,
        })
        .collect();
    let reported = errors()
        .map(|error| (error_order(&root, text, error, open_from, &refused), error))
        .min_by_key(|(order, _)| *order);
    // What GNU R's lexer rejects and ry's accepts: a NUL, a byte that starts no character,
    // and a string the input ends inside, which GNU R reads through the end of the input
    // where ry's lexer ends it with its line. Each is placed by the token it is in. GNU R's
    // lexer hands the unended string to the grammar as a token, so an action that runs on
    // reading it comes first.
    let unterminated = if parse.errors().is_empty() {
        None
    } else {
        unterminated_failure(&root, text)
    };
    let lexical = [
        unterminated.map(|(start, failure)| {
            let rank = if matches!(failure.report, FailureReport::Grammar(_)) {
                3
            } else {
                0
            };
            ((start, rank, 0), failure)
        }),
        nul_failure(&root, text).map(|(start, failure)| ((start, 0, failure.offset), failure)),
        multibyte_failure(&root, text)
            .map(|(start, failure)| ((start, 0, failure.offset), failure)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(order, _)| *order);
    let failure = match (lexical, reported) {
        (Some((lexical_order, lexical)), Some((order, _))) if lexical_order <= order => {
            Some(lexical)
        }
        (_, Some((_, error))) => Some(error_failure(&root, text, error)),
        (lexical, None) => lexical.map(|(_, lexical)| lexical),
    };
    let mut expressions = Vec::new();
    for child in root.children() {
        if child.kind().is_erased() {
            continue;
        }
        if let Some(failure) = &failure {
            // GNU R runs an expression once it reads the newline or `;` that ends it.
            let ended = std::iter::successors(child.next_sibling_or_token(), |element| {
                element.next_sibling_or_token()
            })
            .find(|element| !matches!(element.kind(), SyntaxKind::WHITESPACE | SyntaxKind::COMMENT))
            .is_some_and(|element| {
                matches!(element.kind(), SyntaxKind::NEWLINE | SyntaxKind::SEMICOLON)
                    && usize::from(element.text_range().start()) < failure.offset
            });
            if !ended {
                break;
            }
        }
        expressions.push(child);
    }
    (
        Script {
            text,
            root,
            expressions,
        },
        failure,
    )
}

/// Where GNU R's parser stands when it raises `error`, as a key that sorts the errors of one
/// parse in the order GNU R would meet them: the offset of the last token it has read by
/// then, then which part raises it, then a tie-break among errors of one part.
///
/// Ranks: 0, the lexer's error as it reads a token; 1, a grammar action, run when the token
/// after its construct is read, innermost first; 2 and 3, the grammar refusing a token (the
/// final line break, then the end of input) after the actions that token completes; 4, a
/// placeholder no pipe replaces; 5, a construct holding `open_from`, which the end of input
/// cuts short, so its action never runs. Nor does the action of a construct that is not whole
/// when the grammar refuses a token (`c("" = -)`); `refused` holds those tokens' offsets.
fn error_order(
    root: &SyntaxNode,
    text: &str,
    error: &SyntaxError,
    open_from: Option<usize>,
    refused: &[usize],
) -> (usize, u8, usize) {
    let start = usize::from(error.range.start()).min(text.len());
    // A node's range takes its trailing blanks, so a construct that ends where the cut starts
    // holds it only at the end of the text.
    let is_open = |range: TextRange| {
        let end = usize::from(range.end());
        open_from.is_some_and(|open| {
            usize::from(range.start()) <= open && (open < end || open == end && end == text.len())
        })
    };
    // GNU R reduces a construct before it refuses the token after it, so the construct is
    // whole when the grammar accepts its text up to the first refused token, where such a
    // construct may stand. The line break keeps a trailing comment from taking the closing
    // parenthesis.
    let rank_of = |range: TextRange, before: &str, after: &str| {
        let from = usize::from(range.start());
        let to = refused
            .iter()
            .copied()
            .filter(|refused| *refused > from)
            .min()
            .map_or(range.end().into(), |refused| {
                refused.min(range.end().into())
            });
        let alone = format!("{before}{}\n{after}", &text[from..to]);
        let whole = syntax::parse(&alone).errors().iter().all(|error| {
            error.in_annotation
                || matches!(
                    error.unexpected,
                    Unexpected::Classed(class) if !matches!(class, Classed::EmptyStatement)
                )
        });
        if is_open(range) || !whole { 5 } else { 1 }
    };
    match error.unexpected {
        Unexpected::Token(found) => {
            let found = usize::from(found.start()).min(text.len());
            // GNU R's lexer refuses `=>` itself, so before any action of the grammar.
            let rank = if text.as_bytes()[found..].starts_with(b"=>") {
                0
            } else {
                3
            };
            (found, rank, 0)
        }
        Unexpected::EndOfInput => (text.len(), 3, 0),
        Unexpected::EndOfLine => (text.len(), 2, 0),
        Unexpected::Nothing | Unexpected::Classed(Classed::EmptyStatement) => (start, 3, 0),
        Unexpected::Classed(Classed::Escape) => {
            let token =
                token_at(root, start).map_or(start, |token| token.text_range().start().into());
            // GNU R reports mixed escapes, whose range is the whole token, at the closing quote.
            let read = if token == start {
                usize::from(error.range.end())
            } else {
                start
            };
            (token, 0, read)
        }
        Unexpected::Classed(Classed::EmbeddedNul) => {
            // GNU R refuses the NUL once it has read the closing quote, so after every escape
            // of the string.
            let token = token_at(root, start).map_or(start..start, |token| {
                token.text_range().start().into()..token.text_range().end().into()
            });
            (token.start, 0, token.end + 1)
        }
        Unexpected::Classed(Classed::RepeatedFormal) => {
            let parameter = token_at(root, start)
                .and_then(|token| token.parent())
                .map_or(error.range, |parameter| parameter.text_range());
            let rank = rank_of(parameter, "function(", ") NULL");
            (
                next_token_start(root, text, parameter.end().into()),
                rank,
                usize::MAX - start,
            )
        }
        Unexpected::Classed(Classed::Pipe { target, .. }) => {
            let rank = rank_of(target, "(", ")");
            (
                next_token_start(root, text, target.end().into()),
                rank,
                usize::MAX - usize::from(target.start()),
            )
        }
        Unexpected::Classed(Classed::StrayPlaceholder) => {
            let (statement, terminator) = statement_at(root, text, start);
            let rank = if is_open(statement.text_range()) {
                5
            } else {
                4
            };
            (terminator.start, rank, 0)
        }
        Unexpected::Classed(Classed::EmptyName) => {
            // GNU R's lexer makes the symbol of a backquoted name. A string becomes one in the
            // action that builds its call or its argument.
            let construct = token_at(root, start)
                .filter(|token| token.kind() != SyntaxKind::IDENT)
                .and_then(|token| token.parent())
                .and_then(|literal| literal.parent());
            match construct {
                None => (start, 0, start),
                Some(construct) => {
                    let range = construct.text_range();
                    let is_call = construct.kind() == SyntaxKind::CALL_EXPR;
                    let rank = if is_call {
                        rank_of(range, "(", ")")
                    } else {
                        rank_of(range, "f(", ")")
                    };
                    // A call's action runs as GNU R reads its `)`, an argument's once it has
                    // read the token after the argument.
                    let read = match construct.last_token() {
                        Some(close) if is_call && close.kind() == SyntaxKind::R_PAREN => {
                            close.text_range().start().into()
                        }
                        _ => next_token_start(root, text, range.end().into()),
                    };
                    (read, rank, usize::MAX - usize::from(range.start()))
                }
            }
        }
    }
}

/// GNU R's report for `error`, an error of the parse of `text` outside a `#:` comment.
unsafe fn error_failure(root: &SyntaxNode, text: &str, error: &SyntaxError) -> SyntaxFailure {
    let offset = usize::from(error.range.start()).min(text.len());
    let classed = |offset, message: String, class, value, position, call| SyntaxFailure {
        offset,
        message,
        report: FailureReport::Condition(ParseCondition {
            class,
            value,
            position,
            call,
        }),
    };
    let found = match error.unexpected {
        Unexpected::Token(found) => Some(found),
        Unexpected::Classed(Classed::EmptyStatement) => Some(error.range),
        Unexpected::EndOfInput => None,
        Unexpected::EndOfLine => {
            return SyntaxFailure {
                message: END_OF_LINE.to_owned(),
                report: FailureReport::Grammar(text.len()..text.len()),
                offset,
            };
        }
        Unexpected::Nothing => {
            return SyntaxFailure {
                message: error.message.clone(),
                offset,
                report: FailureReport::Located,
            };
        }
        Unexpected::Classed(Classed::Escape) => {
            let token = token_at(root, offset);
            let failure = token.as_ref().and_then(|token| {
                let token_text = token.text();
                let inner = token_text.get(1..token_text.len().saturating_sub(1))?;
                unescape_bytes(inner).err()
            });
            return match (token, failure) {
                (Some(token), Some(failure)) => {
                    escape_syntax_failure(token.text_range().start().into(), failure)
                }
                // The lexer and the runtime read escapes with one scanner, so this arm needs
                // the two to disagree.
                _ => SyntaxFailure {
                    message: error.message.clone(),
                    offset,
                    report: FailureReport::Located,
                },
            };
        }
        Unexpected::Classed(Classed::RepeatedFormal) => {
            let name = identifier_name(&text[std::ops::Range::<usize>::from(error.range)]);
            return classed(
                offset,
                format!("repeated formal argument '{name}'"),
                "repeatedFormal",
                ConditionValue::Text(name.into_owned()),
                ConditionPosition::Read(offset),
                None,
            );
        }
        Unexpected::Classed(Classed::Pipe { check, target }) => {
            let right = root
                .node_ref()
                .descendants_of_kind(|kind| kind == SyntaxKind::BINARY_EXPR)
                .filter(|pipe| pipe.range().contains_range(target))
                .filter_map(|pipe| expression_children(pipe).get(1).copied())
                .find(|right| right.range() == target);
            let Some(right) = right else {
                return SyntaxFailure {
                    message: error.message.clone(),
                    offset,
                    report: FailureReport::Located,
                };
            };
            let offset = usize::from(significant_range(right).start());
            let (message, class, value, call) = match check {
                PipeCheck::NotCall => (
                    "The pipe operator requires a function call as RHS".to_owned(),
                    "RHSnotFnCall",
                    ConditionValue::Null,
                    true,
                ),
                PipeCheck::PlaceholderRepeated => (
                    "pipe placeholder may only appear once".to_owned(),
                    "tooManyPlaceholders",
                    ConditionValue::Null,
                    true,
                ),
                PipeCheck::PlaceholderInFunction => (
                    "pipe placeholder cannot be used in the RHS function".to_owned(),
                    "placeholderInRHSFn",
                    ConditionValue::Null,
                    false,
                ),
                PipeCheck::PlaceholderNotNamed => (
                    "pipe placeholder can only be used as a named argument".to_owned(),
                    "placeholderNotNamed",
                    ConditionValue::Null,
                    true,
                ),
                PipeCheck::Special => {
                    let name = syntax::called_function(right).unwrap_or_default();
                    let name = String::from_utf8_lossy(&name).into_owned();
                    (
                        format!("function '{name}' not supported in RHS call of a pipe"),
                        "unsupportedInPipe",
                        ConditionValue::Text(name),
                        true,
                    )
                }
            };
            return classed(
                offset,
                message,
                class,
                value,
                ConditionPosition::Read(offset),
                call.then(|| root.at(right)),
            );
        }
        Unexpected::Classed(Classed::EmptyName) => {
            return SyntaxFailure {
                offset,
                message: "attempt to use zero-length variable name".to_owned(),
                report: FailureReport::Plain,
            };
        }
        Unexpected::Classed(Classed::EmbeddedNul) => {
            // GNU R refuses the string as it builds it, and shows the decoded text.
            let shown = token_at(root, offset).map_or_else(String::new, |token| {
                let token_text = token.text();
                match token.kind() {
                    SyntaxKind::RAW_STRING => encode_string(raw_string_content(token_text), None),
                    _ => token_text
                        .get(1..token_text.len().saturating_sub(1))
                        .and_then(|inner| unescape_bytes(inner).ok())
                        .map_or_else(String::new, |(bytes, _)| encode_bytes(&bytes, None)),
                }
            });
            return SyntaxFailure {
                offset,
                message: format!("embedded nul in string: '{shown}'"),
                report: FailureReport::Plain,
            };
        }
        Unexpected::Classed(Classed::StrayPlaceholder) => {
            let (statement, terminator) = statement_at(root, text, offset);
            let position = if text[terminator.clone()].starts_with(';') {
                ConditionPosition::Read(terminator.start)
            } else {
                ConditionPosition::LineBreak(terminator.start)
            };
            return classed(
                usize::from(statement.text_range().start()),
                "invalid use of pipe placeholder".to_owned(),
                "invalidPlaceholder",
                ConditionValue::Null,
                position,
                Some(statement),
            );
        }
    };
    if let Some(failure) = pipe_bind_failure(text, offset) {
        return failure;
    }
    let unexpected = found.map_or(text.len()..text.len(), |found| {
        let start = usize::from(found.start()).min(text.len());
        let mut end = usize::from(found.end()).min(text.len());
        // GNU R's lexer has read the character that shows a number to be malformed,
        // so the lines it quotes reach through that character.
        let refused_number = text[start..]
            .starts_with(|first: char| first == '.' || first.is_ascii_digit())
            && root
                .token_at_offset(found.start())
                .right_biased()
                .is_some_and(|token| token.kind() == SyntaxKind::ERROR_TOKEN);
        if refused_number
            && let Some(next) = text[end..].chars().next()
            && next != '\n'
        {
            end += next.len_utf8();
        }
        start..end
    });
    SyntaxFailure {
        message: unexpected_token(root, text, &unexpected),
        report: FailureReport::Grammar(unexpected),
        offset,
    }
}

/// The token that holds the character at `offset`, or the last token for the end of the text.
fn token_at(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
    let end = usize::from(root.text_range().end());
    root.token_at_offset(TextSize::from(offset.min(end) as u32))
        .right_biased()
}

/// The start of the first token of R code at or after `offset`, or the end of the text. A line
/// break counts where R's grammar reads it as a token: between the expressions of a file or
/// of a `{` block.
fn next_token_start(root: &SyntaxNode, text: &str, offset: usize) -> usize {
    let mut next = token_at(root, offset).filter(|_| offset < text.len());
    while let Some(token) = next {
        let significant = match token.kind() {
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => false,
            SyntaxKind::NEWLINE => token.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
                )
            }),
            _ => !token
                .parent_ancestors()
                .any(|ancestor| ancestor.kind().is_erased()),
        };
        if significant {
            return token.text_range().start().into();
        }
        next = token.next_token();
    }
    text.len()
}

/// The top-level expression that holds `offset`, and the line break or `;` that ends it: an
/// empty range at the end of the text when nothing does.
fn statement_at(
    root: &SyntaxNode,
    text: &str,
    offset: usize,
) -> (SyntaxNode, std::ops::Range<usize>) {
    let statement = root
        .children()
        .find(|child| usize::from(child.text_range().end()) > offset)
        .or_else(|| root.last_child())
        .unwrap_or_else(|| root.clone());
    let terminator = std::iter::successors(statement.next_sibling_or_token(), |element| {
        element.next_sibling_or_token()
    })
    .find(|element| matches!(element.kind(), SyntaxKind::NEWLINE | SyntaxKind::SEMICOLON))
    .map_or(text.len()..text.len(), |element| {
        element.text_range().start().into()..element.text_range().end().into()
    });
    (statement, terminator)
}

/// GNU R's message for a plain `failure` in `text`, in the form `parse()` reports it: the location, then the last one or two
/// lines read, with a caret under the column.
pub fn syntax_failure_message(text: &str, failure: &SyntaxFailure, source_name: &str) -> String {
    // GNU R leaves the name and its colon out when the parse has no srcfile.
    let prefix = if source_name.is_empty() {
        String::new()
    } else {
        format!("{source_name}:")
    };
    if matches!(failure.report, FailureReport::Plain) {
        return failure.message.clone();
    }
    let FailureReport::Grammar(unexpected) = &failure.report else {
        let before = &text[..failure.offset.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count())
            + 1;
        return format!("{prefix}{line}:{column}: {}", failure.message);
    };
    let report = grammar_error_report(text, unexpected, 0, failure.message == INCOMPLETE_STRING);
    let location = format!(
        "{prefix}{}:{}: {}",
        report.line, report.column, failure.message
    );
    match report.context.as_slice() {
        [] => location,
        [only] => {
            let prefix = format!("{}: ", report.context_line);
            let width = prefix.len() + report.column + 1;
            format!("{location}\n{prefix}{only}\n{:>width$}", "^")
        }
        [.., previous, last] => {
            let width = format!("{}:", report.context_line).len() + report.column + 1;
            format!(
                "{location}\n{}: {previous}\n{}: {last}\n{:>width$}",
                report.context_line - 1,
                report.context_line,
                "^"
            )
        }
    }
}

/// GNU R's message for a plain `failure` in a script that `Rscript` runs: the message and the
/// last one or two lines of the failing top-level expression, without a location. GNU R reads
/// a script one top-level expression at a time, so the quoted lines start after the newline or
/// `;` that ended the expression before, and an error at the end of the input quotes nothing.
pub fn script_syntax_failure_message(script: &Script<'_>, failure: &SyntaxFailure) -> String {
    let text = script.text;
    let FailureReport::Grammar(unexpected) = &failure.report else {
        return syntax_failure_message(text, failure, "<text>");
    };
    // GNU R quotes no text for the end of the input, and the last line for its line break.
    if unexpected.is_empty() && unexpected.start >= text.len() && failure.message != END_OF_LINE {
        return failure.message.clone();
    }
    // A string the script's end cuts short leaves GNU R's console waiting for more input.
    if failure.message == INCOMPLETE_STRING {
        return "unexpected end of input".to_owned();
    }
    let context_start = script
        .root
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::SEMICOLON))
        .map(|token| usize::from(token.text_range().end()))
        .take_while(|&end| end <= unexpected.start)
        .last()
        .unwrap_or(0);
    let report = grammar_error_report(text, unexpected, context_start, false);
    match report.context.as_slice() {
        [] => failure.message.clone(),
        [only] => format!("{} in \"{only}\"", failure.message),
        [.., previous, last] => format!("{} in:\n\"{previous}\n{last}\"", failure.message),
    }
}

/// Where GNU R reports a grammar error and the lines it quotes with it.
struct GrammarErrorReport {
    line: usize,
    /// GNU R's 1-based column of the token's first character, with tabs advancing to the next
    /// multiple of 8, or 0 at the end of the input and at a line break.
    column: usize,
    /// The line number of the last quoted line.
    context_line: usize,
    context: Vec<String>,
}

/// GNU R's location and context for a grammar error at `unexpected` in `text`. GNU R's lexer
/// keeps the last 255 bytes it read in a ring buffer, from `context_start` on, and quotes them
/// through the failing token split into lines, each with its tabs expanded and cut at 192
/// bytes. GNU R reads every line with its newline, and the text here has the last one dropped,
/// as the whole-parse source reference assumes, so the end of the input is the start of the
/// line after the last. With `through_end` the lexer read all of the input for the token.
fn grammar_error_report(
    text: &str,
    unexpected: &std::ops::Range<usize>,
    context_start: usize,
    through_end: bool,
) -> GrammarErrorReport {
    let at_end = unexpected.is_empty() && unexpected.start >= text.len();
    let read = if at_end || through_end {
        format!("{text}\n")
    } else {
        text[..unexpected.end.min(text.len())].to_owned()
    };
    // GNU R's lexer has read past a line break when its parser refuses it, so the error is at
    // column 0 of the next line.
    let at_line_break = text[unexpected.start.min(text.len())..].starts_with('\n');
    let line = if at_end || at_line_break {
        read.matches('\n').count()
    } else {
        text[..unexpected.start].matches('\n').count()
    } + 1;
    let column = if at_end || at_line_break {
        0
    } else {
        let line_start = text[..unexpected.start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        text[line_start..unexpected.start]
            .chars()
            .fold(0, |column, character| {
                if character == '\t' {
                    (column + 8) & !7
                } else {
                    column + 1
                }
            })
            + 1
    };
    let mut start = context_start
        .min(read.len())
        .max(read.len().saturating_sub(255));
    while !read.is_char_boundary(start) {
        start += 1;
    }
    // GNU R keeps what it read as a C string, so it quotes only what follows the last NUL.
    if let Some(nul) = read[start..].rfind('\0') {
        start += nul + 1;
    }
    let mut context: Vec<&str> = read[start..].split('\n').collect();
    let mut context_line = read.matches('\n').count() + 1;
    if context.last().is_some_and(|last| last.is_empty()) {
        context.pop();
        context_line -= 1;
    }
    let context = context
        .into_iter()
        .map(|line| {
            let mut expanded = String::new();
            for character in line.chars() {
                if expanded.len() >= 192 {
                    break;
                }
                if character == '\t' {
                    expanded.push(' ');
                    while expanded.len() % 8 != 0 {
                        expanded.push(' ');
                    }
                } else {
                    expanded.push(character);
                }
            }
            expanded
        })
        .collect();
    GrammarErrorReport {
        line,
        column,
        context_line,
        context,
    }
}

/// GNU R's message for its parser stopping on the token at `unexpected`, an empty range at the
/// end of the input, named as GNU R's grammar names its tokens.
fn unexpected_token(root: &SyntaxNode, text: &str, unexpected: &std::ops::Range<usize>) -> String {
    let description = if unexpected.is_empty() {
        "end of input".to_owned()
    } else {
        let offset = TextSize::from(unexpected.start as u32);
        let token = root.token_at_offset(offset).right_biased();
        match token {
            None => "input".to_owned(),
            Some(token) => token_description(&token, &text[unexpected.clone()]),
        }
    };
    format!("unexpected {description}")
}

fn token_description(token: &SyntaxToken, text: &str) -> String {
    let name = match token.kind() {
        SyntaxKind::INTEGER
        | SyntaxKind::SUFFIXED_NUMBER
        | SyntaxKind::DOUBLE
        | SyntaxKind::COMPLEX
        | SyntaxKind::TRUE_KW
        | SyntaxKind::FALSE_KW
        | SyntaxKind::INF_KW
        | SyntaxKind::NAN_KW
        | SyntaxKind::NA_KW
        | SyntaxKind::NA_INTEGER_KW
        | SyntaxKind::NA_REAL_KW
        | SyntaxKind::NA_COMPLEX_KW => "numeric constant",
        SyntaxKind::STRING if is_terminated_string(text) => "string constant",
        SyntaxKind::STRING => "INCOMPLETE_STRING",
        SyntaxKind::RAW_STRING | SyntaxKind::NA_CHARACTER_KW => "string constant",
        SyntaxKind::IDENT | SyntaxKind::DOTS | SyntaxKind::DOTDOTI => "symbol",
        SyntaxKind::LESS_MINUS | SyntaxKind::LESS2_MINUS | SyntaxKind::COLON_EQ => "assignment",
        SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => "'->'",
        SyntaxKind::SPECIAL => "SPECIAL",
        SyntaxKind::NEWLINE => "end of line",
        SyntaxKind::UNDERSCORE => "input",
        // GNU R's lexer hands a control character to the grammar as a token of its own.
        SyntaxKind::ERROR_TOKEN if text.starts_with(|first: char| first.is_ascii_control()) => {
            "invalid token"
        }
        SyntaxKind::ERROR_TOKEN => "input",
        SyntaxKind::BACKSLASH => "'\\\\'",
        SyntaxKind::CARET => "'^'",
        _ => return format!("'{text}'"),
    };
    name.to_owned()
}

/// Whether a string literal's text ends with its closing quote, which an escape does not hide.
fn is_terminated_string(text: &str) -> bool {
    let mut characters = text.chars();
    let Some(quote) = characters.next() else {
        return false;
    };
    let mut escaped = false;
    for character in characters {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return true;
        }
    }
    false
}

/// The error `failure` raises: a plain error with GNU R's message, or the signalled
/// `parseError` condition, whose location names `condition_file`. GNU R's console parses one
/// top-level expression at a time, so for a failure in `console` text the lines count from the
/// one the failing expression starts on.
pub unsafe fn syntax_failure_unwind(
    text: &str,
    failure: &SyntaxFailure,
    source_name: &str,
    condition_file: &str,
    console: Option<&Script<'_>>,
) -> Unwind {
    let FailureReport::Condition(condition) = &failure.report else {
        return error(syntax_failure_message(text, failure, source_name));
    };
    // A console parses one top-level expression at a time, so its lexer counts lines and
    // columns from the newline or `;` that ended the expression before the failing one.
    let first = console.map_or(0, |script| {
        let expression = script
            .root
            .children()
            .filter(|child| !child.kind().is_erased())
            .map(|child| usize::from(child.text_range().start()))
            .take_while(|&start| start <= failure.offset)
            .last()
            .unwrap_or(0);
        script
            .root
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::SEMICOLON))
            .map(|token| usize::from(token.text_range().end()))
            .take_while(|&end| end <= expression)
            .last()
            .unwrap_or(0)
    });
    // GNU R's lexer counts a column per byte of text it does not know to be UTF-8, which text
    // holding a byte that starts no character never is outside the console.
    let bytes = console.is_none() && holds_stand_in(text);
    let (line, column) = match condition.position {
        ConditionPosition::Read(offset) => lexer_position(text, first, offset, bytes),
        // The lexer's position after a line break is column 0 of the next line.
        ConditionPosition::LineBreak(offset) => {
            (lexer_position(text, first, offset, bytes).0 - 1, 0)
        }
    };
    let message = format!("{} ({condition_file}:{line}:{column})", failure.message);
    match signal_parse_condition(condition, &message, condition_file, line, column) {
        Ok(()) => error(message),
        Err(unwind) => unwind,
    }
}

/// The line and column GNU R's lexer stands at once it has read the character at `offset`,
/// having started at `first` on line 1, column 0. A newline starts the next line at column 0, a tab
/// advances to the next multiple of 8, and a continuation byte that follows no lead byte takes
/// no column. With `bytes`, every byte takes one. The end of the text reads as the newline GNU
/// R ends it with.
fn lexer_position(text: &str, first: usize, offset: usize, bytes: bool) -> (usize, usize) {
    let offset = offset.min(text.len());
    let line = text[first.min(offset)..offset].matches('\n').count() + 1;
    let line_start = text[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1)
        .max(first.min(offset));
    let Some(through) = text[offset..]
        .chars()
        .next()
        .filter(|&through| through != '\n')
    else {
        return (line + 1, 0);
    };
    let read = offset + through.len_utf8();
    let mut column = 0;
    for character in text[line_start..read].chars() {
        column = match (character, escaped_byte(character)) {
            ('\t', _) => (column + 8) & !7,
            (_, Some(0x80..=0xBF)) if !bytes => column,
            (_, None) if bytes => column + character.len_utf8(),
            _ => column + 1,
        };
    }
    (line, column)
}

unsafe fn signal_parse_condition(
    condition: &ParseCondition,
    message: &str,
    file: &str,
    line: usize,
    column: usize,
) -> R<()> {
    let call = match &condition.call {
        Some(node) => lower(node.node_ref(), Lowering::new(node, None))?,
        None => nil(),
    };
    let value = match &condition.value {
        ConditionValue::Null => nil(),
        ConditionValue::Text(text) => values::scalar_string(text)?,
        ConditionValue::Integer(number) => values::scalar_integer(*number)?,
    };
    let object = values::named_list(&[
        ("message", values::scalar_string(message)?),
        ("call", call),
        ("value", value),
        ("filename", values::scalar_string(file)?),
        ("lineno", values::scalar_integer(line as i32)?),
        ("colno", values::scalar_integer(column as i32)?),
    ])?;
    values::set_attr(
        object,
        symbols().class,
        values::string_vector(&[condition.class, "parseError", "error", "condition"])?,
    )?;
    let stop = values::call(install("stop")?, &[object])?;
    crate::eval::eval(stop, values::base_namespace())?;
    Ok(())
}

/// `=>` lexes in GNU R as the pipe bind operator, which is disabled unless `_R_USE_PIPEBIND_`
/// is set, so a grammar error at it is that parse error.
fn pipe_bind_failure(text: &str, offset: usize) -> Option<SyntaxFailure> {
    text[offset..].starts_with("=>").then(|| SyntaxFailure {
        offset,
        message: "'=>' is disabled; set '_R_USE_PIPEBIND_' envvar to a true value to enable it"
            .to_owned(),
        report: FailureReport::Condition(ParseCondition {
            class: "pipebindDisabled",
            value: ConditionValue::Null,
            position: ConditionPosition::Read(offset),
            call: None,
        }),
    })
}

/// GNU R's lexer hands a NUL outside a string or comment to the grammar as the end-of-file
/// token, which no rule accepts, so the first one read stops the parse. The result pairs the
/// failure with the offset of its token.
fn nul_failure(root: &SyntaxNode, text: &str) -> Option<(usize, SyntaxFailure)> {
    memchr::memchr(0, text.as_bytes())?;
    root.node_ref()
        .tokens_outside(SyntaxKind::is_erased, |kind| {
            kind == SyntaxKind::ERROR_TOKEN
        })
        .find(|token| token.text().starts_with('\0'))
        .map(|token| {
            let offset = usize::from(token.range().start());
            (
                offset,
                SyntaxFailure {
                    message: "unexpected end of file".to_owned(),
                    report: FailureReport::Grammar(offset..offset + 1),
                    offset,
                },
            )
        })
}

/// The first byte outside a comment and a `%` operator that starts no character. GNU R's lexer
/// reads as many bytes as that lead byte announces before it decodes them, and reports the
/// place it stands at then. A sequence glibc decodes, such as a code point past U+10FFFF, is
/// kept as the bytes it is.
fn multibyte_failure(root: &SyntaxNode, text: &str) -> Option<(usize, SyntaxFailure)> {
    if !holds_stand_in(text) {
        return None;
    }
    let tokens = root
        .node_ref()
        .tokens_outside(SyntaxKind::is_erased, |kind| {
            kind != SyntaxKind::SPECIAL && !kind.is_trivia()
        });
    for token in tokens {
        let start = usize::from(token.range().start());
        let mut characters = token.text().char_indices();
        while let Some((index, character)) = characters.next() {
            let Some(lead) = escaped_byte(character) else {
                continue;
            };
            let length = utf8_sequence_length(lead);
            let mut sequence = SmallVec::<[u8; 8]>::new();
            let mut last = start + index;
            for (offset, following) in text[start + index..].char_indices() {
                if sequence.len() >= length {
                    break;
                }
                match escaped_byte(following) {
                    Some(byte) => sequence.push(byte),
                    None => {
                        sequence.extend_from_slice(following.encode_utf8(&mut [0u8; 4]).as_bytes())
                    }
                }
                last = start + index + offset;
            }
            // GNU R ends the text with a newline of its own, which the lexer reads too.
            if sequence.len() < length && !text.ends_with('\n') {
                sequence.push(b'\n');
                last = text.len();
            }
            if multibyte_length(&sequence).is_some_and(|decoded| decoded == length) {
                // Every byte of a sequence only glibc decodes is a stand-in of its own.
                for _ in 1..length {
                    characters.next();
                }
                continue;
            }
            // GNU R reads the end of the input inside a sequence as the end of the input, which
            // the grammar error reports.
            if sequence.len() < length {
                return None;
            }
            return Some((
                start,
                SyntaxFailure {
                    offset: start + index,
                    message: "invalid multibyte character in parser".to_owned(),
                    report: FailureReport::Condition(ParseCondition {
                        class: "invalidMBCS",
                        value: ConditionValue::Null,
                        position: ConditionPosition::Read(last),
                        call: None,
                    }),
                },
            ));
        }
    }
    None
}

/// The parse error for an escape GNU R's lexer rejects in the quoted token at `start`.
fn escape_syntax_failure(start: usize, failure: EscapeFailure) -> SyntaxFailure {
    let offset = start + 1 + failure.offset;
    SyntaxFailure {
        offset,
        message: failure.message,
        report: FailureReport::Condition(ParseCondition {
            class: failure.class,
            value: failure.value,
            position: ConditionPosition::Read(offset),
            call: None,
        }),
    }
}

/// GNU R's error for the first token that lacks a delimiter, with the offset where the token
/// starts. GNU R's lexer reads a string, a raw string or a backquoted name through to the end
/// of the input, so the token it reports reaches there, where ry's lexer ends it with its
/// line. An escape GNU R rejects anywhere in that stretch is the error instead. A raw string
/// without its opening bracket is an error of GNU R's lexer.
fn unterminated_failure(root: &SyntaxNode, text: &str) -> Option<(usize, SyntaxFailure)> {
    root.node_ref()
        .tokens_outside(SyntaxKind::is_erased, |kind| {
            matches!(
                kind,
                SyntaxKind::STRING
                    | SyntaxKind::RAW_STRING
                    | SyntaxKind::IDENT
                    | SyntaxKind::ERROR_TOKEN
            )
        })
        .find_map(|token| {
            let token_text = token.text();
            let start = usize::from(token.range().start());
            let escapes = match token.kind() {
                SyntaxKind::STRING if !is_terminated_string(token_text) => true,
                SyntaxKind::IDENT
                    if token_text.starts_with('`') && !is_terminated_string(token_text) =>
                {
                    true
                }
                SyntaxKind::RAW_STRING if !is_terminated_raw_string(token_text) => false,
                SyntaxKind::ERROR_TOKEN
                    if token_text.starts_with(['r', 'R'])
                        && token_text[1..].starts_with(['"', '\'']) =>
                {
                    // The token ends before the character that should open the raw string's
                    // body, which is the last one GNU R reads.
                    let failure = SyntaxFailure {
                        message: "malformed raw string literal".to_owned(),
                        offset: start,
                        report: FailureReport::Condition(ParseCondition {
                            class: "invalidRawLiteral",
                            value: ConditionValue::Null,
                            position: ConditionPosition::Read(start + token_text.len()),
                            call: None,
                        }),
                    };
                    return Some((start, failure));
                }
                _ => return None,
            };
            // GNU R reports mixed escapes at the closing quote, which this literal lacks.
            if escapes
                && let Err(failure) = unescape_bytes(&text[start + 1..])
                && failure.class != "mixedEscapes"
            {
                return Some((start, escape_syntax_failure(start, failure)));
            }
            let failure = SyntaxFailure {
                message: INCOMPLETE_STRING.to_owned(),
                report: FailureReport::Grammar(start..text.len()),
                offset: start,
            };
            Some((start, failure))
        })
}

/// GNU R's message for a string, raw string or backquoted name the input ends inside.
const INCOMPLETE_STRING: &str = "unexpected INCOMPLETE_STRING";
const END_OF_LINE: &str = "unexpected end of line";

/// Whether a raw string literal's text ends with the delimiter that mirrors its opening one.
fn is_terminated_raw_string(text: &str) -> bool {
    let mut characters = text.chars().skip(1);
    let Some(quote) = characters.next() else {
        return false;
    };
    let dashes = characters
        .clone()
        .take_while(|&character| character == '-')
        .count();
    let close = match characters.nth(dashes) {
        Some('(') => ')',
        Some('[') => ']',
        Some('{') => '}',
        _ => return false,
    };
    let closer = format!("{close}{}{quote}", "-".repeat(dashes));
    text.len() >= 2 + dashes + 1 + closer.len() && text.ends_with(&closer)
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
        return lower(node.node_ref(), Lowering::new(node, None));
    }
    let text = script.text;
    let range = significant_range(node.node_ref());
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
    let source = SourceMap::new(text, first_line as isize, srcfile)?;
    lower(node.node_ref(), Lowering::new(node, Some(&source)))
}

/// What the lowering needs to build source references: the text with an index of its line
/// starts, the line of the text that is line 1 of the srcfile (negative when the text starts
/// further into the file), and the srcfile environment itself.
pub struct SourceMap<'text> {
    pub(crate) text: &'text str,
    line_starts: Vec<usize>,
    /// One bit per line, set when the line is ASCII without a tab, so a column on it is its
    /// byte offset and needs no scan.
    plain_lines: Vec<u64>,
    first_line: isize,
    srcfile: Sexp,
    /// The `srcref` class vector every srcref of this parse shares.
    class: Sexp,
}

impl<'text> SourceMap<'text> {
    unsafe fn new(text: &'text str, first_line: isize, srcfile: Sexp) -> R<SourceMap<'text>> {
        let mut line_starts = vec![0];
        let mut plain_lines = Vec::new();
        let mut plain = true;
        for (index, &byte) in text.as_bytes().iter().enumerate() {
            if byte == b'\n' {
                let line = line_starts.len() - 1;
                if line % 64 == 0 {
                    plain_lines.push(0);
                }
                if plain && let Some(word) = plain_lines.last_mut() {
                    *word |= 1 << (line % 64);
                }
                line_starts.push(index + 1);
                plain = true;
            } else if byte == b'\t' || !byte.is_ascii() {
                plain = false;
            }
        }
        let line = line_starts.len() - 1;
        if line % 64 == 0 {
            plain_lines.push(0);
        }
        if plain && let Some(word) = plain_lines.last_mut() {
            *word |= 1 << (line % 64);
        }
        Ok(SourceMap {
            text,
            line_starts,
            plain_lines,
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
pub(crate) fn position(source: &SourceMap, offset: usize) -> (i32, i32, i32) {
    position_near(source, offset, &mut 0)
}

/// [`position`] for a caller that asks about nearby offsets in turn: `line` starts as the line of
/// the previous offset, which is checked before the search, and ends as this offset's line.
pub(crate) fn position_near(
    source: &SourceMap,
    offset: usize,
    line: &mut usize,
) -> (i32, i32, i32) {
    let starts = &source.line_starts;
    let on_line = starts.get(*line).is_some_and(|&start| start <= offset)
        && starts.get(*line + 1).is_none_or(|&next| offset < next);
    if !on_line {
        *line = starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
    }
    let line = *line;
    let line_start = source.line_starts.get(line).copied().unwrap_or(0);
    let line_number = (line as isize - source.first_line + 1) as i32;
    let byte = (offset - line_start) as i32 + 1;
    // The column counts the character at `offset` too, unless the offset is the end of the text.
    let plain = offset < source.text.len()
        && source
            .plain_lines
            .get(line / 64)
            .is_some_and(|word| word & (1 << (line % 64)) != 0);
    if plain {
        return (line_number, byte, byte);
    }
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
    (line_number, byte, column)
}

/// The range of a node from its first to its last token of R code, without the comments and
/// whitespace around it or `#:` annotations at its edges.
fn significant_range(node: NodeRef<'_>) -> TextRange {
    let range = node.range();
    let first = node
        .tokens_outside(SyntaxKind::is_erased, |kind| !kind.is_trivia())
        .next();
    // A token under a skipped node is not R code, and a backward walk meets a token before the
    // nodes above it, so each candidate checks its ancestors up to `node`.
    let last = node
        .descendants_with_tokens()
        .rev()
        .filter_map(ElementRef::into_token)
        .find(|token| {
            !token.kind().is_trivia()
                && !std::iter::successors(token.parent(), |ancestor| ancestor.parent())
                    .take_while(|ancestor| *ancestor != node)
                    .any(|ancestor| ancestor.kind().is_erased())
        });
    match (first, last) {
        (Some(first), Some(last)) => TextRange::new(first.range().start(), last.range().end()),
        _ => range,
    }
}

/// Lowers one expression node of a tree that parsed without errors, such as a top-level item
/// of a file the analysis already parsed, so a host that holds the tree needs no second parse.
pub unsafe fn lower_node(node: &SyntaxNode) -> R<Sexp> {
    lower(node.node_ref(), Lowering::new(node, None))
}

/// Lowers `node` like [`lower_node`], keeping source references against `srcfile`. `text` is
/// the whole text of the node's tree, a piece of the srcfile's file that starts on its line
/// `line` (1-based), so a host can parse one definition alone and still count lines as the file
/// does.
pub unsafe fn lower_node_at_line(
    node: &SyntaxNode,
    text: &str,
    line: usize,
    srcfile: Sexp,
) -> R<Sexp> {
    let source = SourceMap::new(text, 1 - line as isize, srcfile)?;
    lower(node.node_ref(), Lowering::new(node, Some(&source)))
}

/// A srcfile environment for the whole of a file, as GNU R's `srcfilecopy` makes one, so source
/// references name the file and count lines from its start.
pub unsafe fn file_srcfile(path: &str, text: &str) -> R<Sexp> {
    let heap = crate::heap();
    let srcfile = values::new_env(values::empty_env())?;
    heap.root(srcfile);
    let lines: Vec<&str> = text.lines().collect();
    heap.define(srcfile, install("filename")?, values::scalar_string(path)?)?;
    heap.define(srcfile, install("lines")?, values::string_vector(&lines)?)?;
    values::set_attr(
        srcfile,
        symbols().class,
        values::string_vector(&["srcfilecopy", "srcfile"])?,
    )?;
    Ok(srcfile)
}

/// What the lowering of one tree carries along: the source map when it keeps source references,
/// and an owned handle on the tree for registering a typed function's signature, which a tree
/// without any type syntax does not need.
#[derive(Clone, Copy)]
struct Lowering<'a> {
    source: Option<&'a SourceMap<'a>>,
    owner: &'a SyntaxNode,
    typed: bool,
    /// Inside the right side of a pipe that passes its left side through a placeholder: the
    /// tree position of that `_` and the lowered left side, which stands in its place.
    placeholder: Option<(u32, Sexp)>,
}

impl<'a> Lowering<'a> {
    fn new(owner: &'a SyntaxNode, source: Option<&'a SourceMap<'a>>) -> Lowering<'a> {
        Lowering {
            source,
            owner,
            typed: true,
            placeholder: None,
        }
    }
}

/// Whether `kind` can make a function typed: a function's signature is written only in `#:`
/// blocks, type ascriptions and return types.
fn is_type_syntax(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::ANNOTATION | SyntaxKind::TYPE_ASCRIPTION | SyntaxKind::RETURN_TYPE
    )
}

/// The expression children of a node, without trivia, tokens or annotations.
fn expression_children(node: NodeRef<'_>) -> SmallVec<[NodeRef<'_>; 4]> {
    node.child_nodes()
        .filter(|child| !child.kind().is_erased())
        .collect()
}

fn significant_tokens(node: NodeRef<'_>) -> impl DoubleEndedIterator<Item = TokenRef<'_>> {
    node.child_tokens()
        .filter(|token| !token.kind().is_trivia())
}

unsafe fn lower(node: NodeRef<'_>, context: Lowering<'_>) -> R<Sexp> {
    match node.kind() {
        SyntaxKind::NAME => match context.placeholder {
            Some((position, left)) if position == node.position() => Ok(left),
            // GNU R's parser holds a placeholder as the string `"_"` until a pipe replaces it,
            // which is what a parameter's default and a condition's call show.
            _ if significant_tokens(node)
                .next()
                .is_some_and(|token| token.kind() == SyntaxKind::UNDERSCORE) =>
            {
                values::scalar_string("_")
            }
            _ => name_symbol(node),
        },
        SyntaxKind::LITERAL => {
            let token = significant_tokens(node)
                .next()
                .ok_or_else(|| error("empty literal"))?;
            literal(token)
        }
        SyntaxKind::PAREN_EXPR => {
            let value = match expression_children(node).first() {
                Some(&inner) => lower(inner, context)?,
                None => return Err(error("unexpected ')'")),
            };
            values::call(symbols().paren, &[value])
        }
        // `@Name(value)` runs as its `(value)`: the nominal type exists only for the checker.
        SyntaxKind::NEW_EXPR => match expression_children(node).first() {
            Some(&inner) => lower(inner, context),
            None => Err(error("expected `(` after the nominal type")),
        },
        SyntaxKind::BRACE_EXPR => {
            let statements = expression_children(node);
            let mut items: SmallVec<[(Sexp, Sexp); 8]> = SmallVec::new();
            items.push((nil(), symbols().brace));
            for &child in &statements {
                items.push((nil(), lower(child, context)?));
            }
            let call = values::pairlist(&items, SexpType::Language)?;
            if let Some(source) = context.source {
                block_srcrefs(call, node, &statements, source)?;
            }
            Ok(call)
        }
        SyntaxKind::BINARY_EXPR => binary(node, context),
        SyntaxKind::UNARY_EXPR => {
            let operator = significant_tokens(node)
                .next()
                .ok_or_else(|| error("missing operator"))?;
            let operand = *expression_children(node)
                .first()
                .ok_or_else(|| error("missing operand"))?;
            // `-` written before a suffixed literal is part of it, so `-128i8` is the `I8` it
            // denotes rather than `128i8`, which does not fit, negated.
            if operator.text() == "-"
                && operand.kind() == SyntaxKind::LITERAL
                && let Some(token) = significant_tokens(operand).next()
                && token.kind() == SyntaxKind::SUFFIXED_NUMBER
            {
                return suffixed_number_value(token.text(), true);
            }
            let operand = lower(operand, context)?;
            let symbol = install(operator.text())?;
            values::call(symbol, &[operand])
        }
        SyntaxKind::CALL_EXPR => {
            let children = expression_children(node);
            let (function, arguments) = match children.as_slice() {
                &[function, arguments, ..] if arguments.kind() == SyntaxKind::ARGUMENT_LIST => {
                    (function, arguments)
                }
                _ => return Err(error("malformed call")),
            };
            let mut function = lower(function, context)?;
            // R's parser turns a string in function position into a symbol: `"f<-"(x)`.
            if function.kind() == SexpType::String && values::length(function) == 1 {
                function = values::install_charsxp(function.element(0))?;
            }
            let mut items: Items = SmallVec::new();
            items.push((nil(), function));
            argument_list(arguments, context, &mut items)?;
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
            let children = expression_children(node);
            let (object, arguments) = match children.as_slice() {
                &[object, arguments, ..] => (object, arguments),
                _ => return Err(error("malformed subset")),
            };
            let operator = if node.kind() == SyntaxKind::SUBSET_EXPR {
                symbols().bracket
            } else {
                symbols().bracket2
            };
            let mut items: Items = SmallVec::new();
            items.extend([(nil(), operator), (nil(), lower(object, context)?)]);
            argument_list(arguments, context, &mut items)?;
            // R's grammar gives `x[]` and `x[[]]` one empty subscript, unlike `f()`, so
            // `nargs()` in a `[<-` method counts it (`[<-.data.frame` relies on that).
            if items.len() == 2 {
                items.push((nil(), missing_arg()));
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR | SyntaxKind::NAMESPACE_EXPR => {
            let children = expression_children(node);
            let operator = significant_tokens(node)
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
            let (left, right) = match *children.as_slice() {
                [left, right, ..] => (lower(left, context)?, lower(right, context)?),
                [left] => {
                    // A keyword or string on the right is a token, not a node.
                    let token = significant_tokens(node)
                        .next_back()
                        .ok_or_else(|| error("missing name"))?;
                    (lower(left, context)?, literal(token)?)
                }
                _ => return Err(error("malformed extraction")),
            };
            let symbol = install(operator.text())?;
            values::call(symbol, &[left, right])
        }
        SyntaxKind::FUNCTION_DEF => function_definition(node, context),
        SyntaxKind::IF_EXPR => keyword_call(symbols().if_, node, context),
        SyntaxKind::FOR_EXPR => keyword_call(symbols().for_, node, context),
        SyntaxKind::WHILE_EXPR => keyword_call(symbols().while_, node, context),
        SyntaxKind::REPEAT_EXPR => keyword_call(symbols().repeat, node, context),
        SyntaxKind::FSTRING => format_string(node, context),
        SyntaxKind::TRY_EXPR => match expression_children(node).first() {
            Some(&operand) => values::call(install(".ry_try")?, &[lower(operand, context)?]),
            None => Err(error("expected an expression after `try`")),
        },
        SyntaxKind::RECORD_EXPR | SyntaxKind::TUPLE_EXPR => {
            if let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST)
                && let Some(spread) = child_of_kind(arguments, SyntaxKind::RECORD_SPREAD)
            {
                let base = spread
                    .text()
                    .trim()
                    .strip_prefix("..")
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| error("a record update needs a record after `..`"))?;
                let mut items = vec![
                    (nil(), install(".ry_update")?),
                    (nil(), install(&identifier(base))?),
                ];
                items.extend(literal_items(arguments, context)?);
                return values::pairlist(&items, SexpType::Language);
            }
            let head = if node.kind() == SyntaxKind::RECORD_EXPR {
                ".ry_record"
            } else {
                ".ry_tuple"
            };
            let mut items = vec![(nil(), install(head)?)];
            if let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                items.extend(literal_items(arguments, context)?);
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::ARRAY_EXPR => {
            let mut items = vec![(nil(), install(".ry_array")?)];
            if let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                items.extend(literal_items(arguments, context)?);
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::TAG_EXPR => {
            let name =
                child_of_kind(node, SyntaxKind::NAME).ok_or_else(|| error("a tag needs a name"))?;
            let mut items = vec![(nil(), install(".ry_tag")?), (nil(), tag_name(name)?)];
            if let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                items.extend(literal_items(arguments, context)?);
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::MATCH_EXPR => match_expression(node, context),
        SyntaxKind::BREAK_EXPR => values::call(symbols().break_, &[]),
        SyntaxKind::NEXT_EXPR => values::call(symbols().next, &[]),
        SyntaxKind::ERROR => Err(error(format!("unexpected '{}'", node.text()))),
        kind => Err(error(format!("cannot lower {}", kind.display()))),
    }
}

/// A call's items: its head, then each argument with its tag. Most calls have few arguments.
type Items = SmallVec<[(Sexp, Sexp); 6]>;

/// `if`, `for`, `while` or `repeat`: the keyword's function applied to the expression children.
unsafe fn keyword_call(head: Sexp, node: NodeRef<'_>, context: Lowering<'_>) -> R<Sexp> {
    let mut items: Items = SmallVec::new();
    items.push((nil(), head));
    for child in node.child_nodes().filter(|child| !child.kind().is_erased()) {
        items.push((nil(), lower(child, context)?));
    }
    values::pairlist(&items, SexpType::Language)
}

/// Attaches a `{` call's source references: a `srcref` list of the brace itself and then each
/// statement, the srcfile, and a `wholeSrcref` from the start of the srcfile to the closing brace.
unsafe fn block_srcrefs(
    call: Sexp,
    node: NodeRef<'_>,
    statements: &[NodeRef<'_>],
    source: &SourceMap,
) -> R<()> {
    let range = significant_range(node);
    let srcrefs = alloc(SexpType::List, statements.len() + 1)?;
    let open = TextRange::at(range.start(), 1.into());
    values::set_elt(srcrefs, 0, srcref(source, open)?);
    for (index, &statement) in statements.iter().enumerate() {
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

unsafe fn binary(node: NodeRef<'_>, context: Lowering<'_>) -> R<Sexp> {
    let children = expression_children(node);
    let operator = significant_tokens(node)
        .find(|token| !matches!(token.kind(), SyntaxKind::L_PAREN | SyntaxKind::R_PAREN))
        .ok_or_else(|| error("missing operator"))?;
    let (left, right) = match *children.as_slice() {
        [left, right, ..] => (left, Some(right)),
        [left] => (left, None),
        _ => return Err(error("malformed binary expression")),
    };
    let left_value = lower(left, context)?;
    let Some(right) = right else {
        // A formula with only a left side is `~` applied to it.
        let symbol = install(operator.text())?;
        return values::call(symbol, &[left_value]);
    };
    if operator.kind() == SyntaxKind::PIPE_GREATER {
        // `lhs |> f(x, y = _)` is `f(x, y = lhs)` and `lhs |> _$a[[1]]` is `lhs$a[[1]]`: the
        // left side stands where the placeholder is written.
        if let Some(placeholder) = syntax::pipe_placeholder(right) {
            let context = Lowering {
                placeholder: Some((placeholder.position(), left_value)),
                ..context
            };
            return lower(right, context);
        }
        // Without one, `lhs |> f(args)` is `f(lhs, args)`.
        let right_value = lower(right, context)?;
        if right_value.kind() != SexpType::Language {
            return Err(error("The pipe operator requires a function call as RHS"));
        }
        let rest = values::cons(left_value, right_value.cdr())?;
        return values::lcons(right_value.car(), rest);
    }
    let mut right_value = lower(right, context)?;
    if matches!(
        operator.kind(),
        SyntaxKind::LESS_MINUS | SyntaxKind::LESS2_MINUS | SyntaxKind::EQ
    ) && let Some(ascription) = child_of_kind(node, SyntaxKind::TYPE_ASCRIPTION)
    {
        right_value = with_annotated_literals(ascription, right, right_value)?;
    }
    match operator.kind() {
        SyntaxKind::MINUS_GREATER => values::call(symbols().assign, &[right_value, left_value]),
        SyntaxKind::MINUS_GREATER2 => values::call(symbols().assign2, &[right_value, left_value]),
        // `**` lexes as `^` and calls it, as in GNU R.
        SyntaxKind::CARET => values::call(install("^")?, &[left_value, right_value]),
        SyntaxKind::SPECIAL => {
            let symbol = install(before_nul(operator.text()))?;
            values::call(symbol, &[left_value, right_value])
        }
        _ => {
            let symbol = install(operator.text())?;
            values::call(symbol, &[left_value, right_value])
        }
    }
}

/// An unsuffixed number literal bound to a name annotated `I64`, as in `n: I64 <- 5`, is an
/// `I64`, because the checker types the binding so and the literal has no operand to convert it
/// later; so are the items of a list literal bound to a `List<I64>`, as in
/// `xs: List<I64> <- [1, 2]`. Any other value runs unchanged, as bindings are not checked.
unsafe fn with_annotated_literals(
    ascription: NodeRef<'_>,
    node: NodeRef<'_>,
    value: Sexp,
) -> R<Sexp> {
    let Some(annotation) = ascription.child_nodes().next() else {
        return Ok(value);
    };
    match annotation.kind() {
        SyntaxKind::TYPE_REF => match (names_i64(annotation), number_literal_text(node)) {
            (true, Some(text)) => crate::numbers::from_text(&text),
            _ => Ok(value),
        },
        SyntaxKind::TYPE_APPLY if node.kind() == SyntaxKind::ARRAY_EXPR => {
            let is_list = child_of_kind(annotation, SyntaxKind::NAME)
                .is_some_and(|name| name.text() == "List");
            let item = child_of_kind(annotation, SyntaxKind::TYPE_ARG_LIST)
                .and_then(|arguments| child_of_kind(arguments, SyntaxKind::TYPE_REF))
                .is_some_and(names_i64);
            if !(is_list && item) {
                return Ok(value);
            };
            let Some(arguments) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) else {
                return Ok(value);
            };
            // The lowered call holds one cell per argument, after its head.
            let mut cell = value.cdr();
            for argument in arguments
                .child_nodes()
                .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
            {
                if cell == nil() {
                    break;
                }
                let text = match expression_children(argument).as_slice() {
                    &[item] => number_literal_text(item),
                    _ => None,
                };
                if let Some(text) = text {
                    crate::heap().set_car(cell, crate::numbers::from_text(&text)?);
                }
                cell = cell.cdr();
            }
            Ok(value)
        }
        _ => Ok(value),
    }
}

/// Whether a type reference names `I64`, the one ry number that is not R's `double` at run time.
fn names_i64(annotation: NodeRef<'_>) -> bool {
    annotation.text().trim() == "I64"
}

/// The text of an unsuffixed integer or double literal, with its sign when a `-` is written
/// directly before it, and without an `L` suffix.
fn number_literal_text(node: NodeRef<'_>) -> Option<String> {
    match node.kind() {
        SyntaxKind::LITERAL => {
            let token = significant_tokens(node).next()?;
            match token.kind() {
                SyntaxKind::INTEGER => Some(token.text().trim_end_matches('L').to_owned()),
                SyntaxKind::DOUBLE => Some(token.text().to_owned()),
                _ => None,
            }
        }
        SyntaxKind::UNARY_EXPR => {
            let operator = significant_tokens(node).next()?;
            match (operator.kind(), expression_children(node).as_slice()) {
                (SyntaxKind::MINUS, &[operand]) => {
                    number_literal_text(operand).map(|text| format!("-{text}"))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn child_of_kind(node: NodeRef<'_>, kind: SyntaxKind) -> Option<NodeRef<'_>> {
    node.child_nodes().find(|child| child.kind() == kind)
}

/// The items of a record, tuple or tag payload. Unlike a call's arguments, a trailing comma adds
/// no empty item: `(1L,)` has one.
unsafe fn literal_items(node: NodeRef<'_>, context: Lowering<'_>) -> R<Vec<(Sexp, Sexp)>> {
    node.child_nodes()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .map(|argument| argument_value(argument, context))
        .collect()
}

/// `f"a{x}b"` runs as `paste0("a", .ry_text(x), "b")`. A text run has its escapes decoded
/// and its doubled braces halved.
unsafe fn format_string(node: NodeRef<'_>, context: Lowering<'_>) -> R<Sexp> {
    let text = install(".ry_text")?;
    let mut items = vec![(nil(), install("paste0")?)];
    for child in node.children() {
        match child {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::FSTRING_TEXT => {
                let text = token.text().replace("{{", "{").replace("}}", "}");
                let (bytes, native) =
                    unescape_bytes(&text).map_err(|failure| error(failure.message))?;
                let encoding = if native {
                    Encoding::Native
                } else {
                    Encoding::Utf8
                };
                let piece = alloc(SexpType::String, 1)?;
                values::set_elt(piece, 0, values::mkchar_bytes(&bytes, encoding)?);
                items.push((nil(), piece));
            }
            NodeOrToken::Node(interpolation)
                if interpolation.kind() == SyntaxKind::FSTRING_INTERPOLATION =>
            {
                let inner = *expression_children(interpolation)
                    .first()
                    .ok_or_else(|| error("this interpolation is empty"))?;
                let value = lower(inner, context)?;
                items.push((nil(), values::call(text, &[value])?));
            }
            _ => {}
        }
    }
    if items.len() == 1 {
        items.push((nil(), values::scalar_string("")?));
    }
    values::pairlist(&items, SexpType::Language)
}

/// A tag's name as the string `.ry_tag` takes first.
unsafe fn tag_name(name: NodeRef<'_>) -> R<Sexp> {
    let token = significant_tokens(name)
        .next()
        .ok_or_else(|| error("empty tag name"))?;
    values::scalar_string(&identifier(token.text()))
}

/// `match (x) { arms }` becomes `.ry_match(x, `=>`(pattern, body), `=>`(pattern, guard, body))`.
/// A parenthesized scrutinee lowers to its inner expression, because the parentheses belong to
/// the `match` syntax, as they do for `if`.
unsafe fn match_expression(node: NodeRef<'_>, context: Lowering<'_>) -> R<Sexp> {
    let children = expression_children(node);
    let (&scrutinee, arms) = children
        .split_first()
        .ok_or_else(|| error("a `match` needs a value"))?;
    let scrutinee = if scrutinee.kind() == SyntaxKind::PAREN_EXPR {
        let inner = *expression_children(scrutinee)
            .first()
            .ok_or_else(|| error("a `match` needs a value"))?;
        lower(inner, context)?
    } else {
        lower(scrutinee, context)?
    };
    let arrow = install("=>")?;
    let mut items = vec![(nil(), install(".ry_match")?), (nil(), scrutinee)];
    for &arm in arms
        .iter()
        .filter(|arm| arm.kind() == SyntaxKind::MATCH_ARM)
    {
        let parts = expression_children(arm);
        let mut arm_items = vec![(nil(), arrow)];
        let mut parts = parts.iter();
        let &pattern_node = parts
            .next()
            .ok_or_else(|| error("a `match` arm needs a pattern"))?;
        arm_items.push((nil(), pattern(pattern_node)?));
        for &part in parts {
            if part.kind() == SyntaxKind::MATCH_GUARD {
                let condition = *expression_children(part)
                    .first()
                    .ok_or_else(|| error("an arm guard needs a condition"))?;
                arm_items.push((nil(), lower(condition, context)?));
            } else {
                arm_items.push((nil(), lower(part, context)?));
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
unsafe fn pattern(node: NodeRef<'_>) -> R<Sexp> {
    match node.kind() {
        SyntaxKind::PATTERN_WILDCARD => install("_"),
        SyntaxKind::PATTERN_REST if node.text() == ".." => install(".."),
        SyntaxKind::PATTERN_BIND => {
            let name = child_of_kind(node, SyntaxKind::NAME)
                .ok_or_else(|| error("empty binding pattern"))?;
            name_symbol(name)
        }
        SyntaxKind::PATTERN_LITERAL => {
            let negative = significant_tokens(node).any(|token| token.kind() == SyntaxKind::MINUS);
            let literal_node = child_of_kind(node, SyntaxKind::LITERAL)
                .ok_or_else(|| error("empty literal pattern"))?;
            let token = significant_tokens(literal_node)
                .next()
                .ok_or_else(|| error("empty literal pattern"))?;
            let value = literal(token)?;
            if negative { negate(value) } else { Ok(value) }
        }
        SyntaxKind::PATTERN_TAG => {
            let name = child_of_kind(node, SyntaxKind::NAME)
                .ok_or_else(|| error("a tag pattern needs a name"))?;
            let mut items = vec![(nil(), install(".ry_tag")?), (nil(), tag_name(name)?)];
            if let Some(list) = child_of_kind(node, SyntaxKind::PATTERN_LIST) {
                items.extend(pattern_items(list)?);
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
            let mut alternatives = node.child_nodes().map(|child| pattern(child));
            let mut result = alternatives
                .next()
                .ok_or_else(|| error("empty alternative pattern"))??;
            let pipe = install("|")?;
            for alternative in alternatives {
                result = values::call(pipe, &[result, alternative?])?;
            }
            Ok(result)
        }
        SyntaxKind::PATTERN_REST => install(&identifier(node.text().trim())),
        SyntaxKind::PATTERN_ARRAY => {
            let mut items = vec![(nil(), install(".ry_array")?)];
            for child in node.child_nodes() {
                items.push((nil(), pattern(child)?));
            }
            values::pairlist(&items, SexpType::Language)
        }
        SyntaxKind::PATTERN_AS => {
            let inner = node
                .child_nodes()
                .find(|child| child.kind() != SyntaxKind::NAME)
                .ok_or_else(|| error("`as` needs a pattern before it"))?;
            let name = node
                .child_nodes()
                .rfind(|child| child.kind() == SyntaxKind::NAME)
                .ok_or_else(|| error("`as` needs a name after it"))?;
            values::call(install(".ry_as")?, &[pattern(inner)?, name_symbol(name)?])
        }
        SyntaxKind::PATTERN_PAREN => {
            let inner = node
                .child_nodes()
                .next()
                .ok_or_else(|| error("empty pattern group"))?;
            values::call(symbols().paren, &[pattern(inner)?])
        }
        _ => Err(error(format!("unexpected '{}' in a pattern", node.text()))),
    }
}

unsafe fn pattern_items(list: NodeRef<'_>) -> R<Vec<(Sexp, Sexp)>> {
    let mut items = Vec::new();
    for child in list.child_nodes() {
        if child.kind() == SyntaxKind::PATTERN_FIELD {
            let name = child_of_kind(child, SyntaxKind::NAME)
                .ok_or_else(|| error("a field pattern needs a name"))?;
            // `(x =, y =)` puns: a field without a pattern binds a name of its own spelling.
            let value = match child
                .child_nodes()
                .find(|part| part.kind() != SyntaxKind::NAME)
            {
                Some(value) => pattern(value)?,
                None => name_symbol(name)?,
            };
            items.push((name_symbol(name)?, value));
        } else {
            items.push((nil(), pattern(child)?));
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

/// Appends the arguments of a call or subset to `items`, with empty arguments as
/// `R_MissingArg`. An argument slot sits between separators, and R keeps empty slots
/// (`x[1, ]`), but a list with neither an argument nor a comma has no slot at all.
unsafe fn argument_list(node: NodeRef<'_>, context: Lowering<'_>, items: &mut Items) -> R<()> {
    let mut slot: Option<NodeRef<'_>> = None;
    let mut saw_any = false;
    for element in node.children() {
        match element {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMA => {
                items.push(match slot.take() {
                    None => (nil(), missing_arg()),
                    Some(argument) => argument_value(argument, context)?,
                });
                saw_any = true;
            }
            NodeOrToken::Node(child) if child.kind() == SyntaxKind::ARGUMENT => {
                slot = Some(child);
                saw_any = true;
            }
            _ => {}
        }
    }
    if saw_any {
        items.push(match slot {
            None => (nil(), missing_arg()),
            Some(argument) => argument_value(argument, context)?,
        });
    }
    Ok(())
}

unsafe fn argument_value(node: NodeRef<'_>, context: Lowering<'_>) -> R<(Sexp, Sexp)> {
    let has_equals = significant_tokens(node).any(|token| token.kind() == SyntaxKind::EQ);
    if !has_equals {
        return match node.children().find(|element| !element.kind().is_trivia()) {
            Some(NodeOrToken::Node(child)) => Ok((nil(), lower(child, context)?)),
            Some(NodeOrToken::Token(token)) => Ok((nil(), literal(token)?)),
            None => Ok((nil(), missing_arg())),
        };
    }
    let mut tag = nil();
    let mut value = missing_arg();
    let mut after_equals = false;
    for element in node.children() {
        match element {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::EQ && !after_equals => {
                after_equals = true;
            }
            NodeOrToken::Token(token) if token.kind().is_trivia() => {}
            NodeOrToken::Node(child) if !after_equals => {
                tag = tag_from_node(child)?;
            }
            NodeOrToken::Token(token) if !after_equals => {
                tag = tag_from_token(token)?;
            }
            NodeOrToken::Node(child) => value = lower(child, context)?,
            NodeOrToken::Token(token) => value = literal(token)?,
        }
    }
    Ok((tag, value))
}

/// The tag of a named argument or parameter: a name, a string, or `NULL`.
unsafe fn tag_from_node(node: NodeRef<'_>) -> R<Sexp> {
    match node.kind() {
        SyntaxKind::NAME => name_symbol(node),
        SyntaxKind::LITERAL => {
            let token = significant_tokens(node)
                .next()
                .ok_or_else(|| error("empty literal"))?;
            tag_from_token(token)
        }
        _ => Err(error("invalid argument name")),
    }
}

/// The tag a name token or a string literal gives. A string installs as GNU R's `installTrChar`
/// installs it, so a non-ASCII one keeps its UTF-8 mark in names such as `c("a\u00e4" = 1)`.
unsafe fn tag_from_token(token: TokenRef<'_>) -> R<Sexp> {
    match token.kind() {
        SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
            values::install_charsxp(values::mkchar(&token_name(token)?)?)
        }
        _ => install(&token_name(token)?),
    }
}

unsafe fn token_name(token: TokenRef<'_>) -> R<String> {
    match token.kind() {
        SyntaxKind::STRING => string_literal(token.text()),
        SyntaxKind::RAW_STRING => Ok(raw_string_content(token.text()).to_owned()),
        SyntaxKind::IDENT => Ok(identifier(token.text())),
        _ => Ok(token.text().to_owned()),
    }
}

unsafe fn name_symbol(node: NodeRef<'_>) -> R<Sexp> {
    let token = significant_tokens(node)
        .next()
        .ok_or_else(|| error("empty name"))?;
    match token.kind() {
        SyntaxKind::IDENT => install(&identifier_name(token.text())),
        SyntaxKind::DOTS => Ok(symbols().dots),
        _ => install(token.text()),
    }
}

/// An identifier's name, without backticks, borrowed unless backticks need removing.
fn identifier_name(text: &str) -> Cow<'_, str> {
    if text.starts_with('`') {
        Cow::Owned(identifier(text))
    } else {
        Cow::Borrowed(text)
    }
}

/// An identifier's name, without backticks.
fn identifier(text: &str) -> String {
    match text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
    {
        Some(inner) => {
            let name = unescape(inner).unwrap_or_else(|_| inner.to_owned());
            before_nul(&name).to_owned()
        }
        None => text.to_owned(),
    }
}

/// GNU R holds a name as a C string, so a NUL byte ends it.
fn before_nul(name: &str) -> &str {
    name.split('\0').next().unwrap_or(name)
}

unsafe fn function_definition(node: NodeRef<'_>, context: Lowering<'_>) -> R<Sexp> {
    let mut formals: Items = SmallVec::new();
    let mut body = None;
    for child in node.child_nodes().filter(|child| !child.kind().is_erased()) {
        if child.kind() == SyntaxKind::PARAMETER_LIST {
            for parameter in child.child_nodes() {
                if parameter.kind() == SyntaxKind::PARAMETER {
                    formals.push(parameter_value(parameter, context)?);
                }
            }
        } else {
            body = Some(lower(child, context)?);
        }
    }
    let formals = values::pairlist(&formals, SexpType::Pairlist)?;
    let body = body.unwrap_or(nil());
    if context.typed {
        crate::annotations::register_definition(&context.owner.at(node), formals, body)?;
    }
    let srcref = match context.source {
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

unsafe fn parameter_value(node: NodeRef<'_>, context: Lowering<'_>) -> R<(Sexp, Sexp)> {
    let mut tag = nil();
    let mut default = missing_arg();
    let mut after_equals = false;
    for element in node.children() {
        match element {
            NodeOrToken::Token(token) if token.kind().is_trivia() => {}
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::EQ => after_equals = true,
            NodeOrToken::Token(token) if !after_equals => {
                tag = match token.kind() {
                    SyntaxKind::DOTS => symbols().dots,
                    _ => install(&token_name(token)?)?,
                };
            }
            NodeOrToken::Node(child) if child.kind().is_erased() => {}
            NodeOrToken::Node(child) if !after_equals => tag = tag_from_node(child)?,
            NodeOrToken::Node(child) => default = lower(child, context)?,
            NodeOrToken::Token(token) => default = literal(token)?,
        }
    }
    Ok((tag, default))
}

unsafe fn literal(token: TokenRef<'_>) -> R<Sexp> {
    let text = token.text();
    let known = crate::heap().known();
    match token.kind() {
        SyntaxKind::DOUBLE => values::scalar_real(parse_number(text)),
        SyntaxKind::SUFFIXED_NUMBER => suffixed_number_value(text, false),
        SyntaxKind::INTEGER => {
            let body = &text[..text.len() - 1];
            let value = parse_number(body);
            // GNU R's lexer words both warnings by whether it saw a point and no exponent, and a
            // hexadecimal literal's exponent starts with `p`.
            let hex = body.starts_with("0x") || body.starts_with("0X");
            let exponent = if hex { ['p', 'P'] } else { ['e', 'E'] };
            let bare_point = body.contains('.') && !body.contains(exponent);
            if value.fract() == 0.0 && value.abs() <= i32::MAX as f64 {
                if bare_point && !crate::ahead::holds_back_warning() {
                    crate::conditions::warning(format!(
                        "integer literal {text} contains unnecessary decimal point"
                    ))?;
                }
                values::scalar_integer(value as i32)
            } else {
                if !crate::ahead::holds_back_warning() {
                    crate::conditions::warning(if bare_point {
                        format!("integer literal {text} contains decimal; using numeric value")
                    } else {
                        format!("non-integer value {text} qualified with L; using numeric value")
                    })?;
                }
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
            let inner = &text[1..text.len().saturating_sub(1).max(1)];
            let (bytes, native) =
                unescape_bytes(inner).map_err(|failure| error(failure.message))?;
            let encoding = if native {
                Encoding::Native
            } else {
                Encoding::Utf8
            };
            let result = alloc(SexpType::String, 1)?;
            values::set_elt(result, 0, values::mkchar_bytes(&bytes, encoding)?);
            Ok(result)
        }
        SyntaxKind::RAW_STRING => values::scalar_string(raw_string_content(text)),
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
        SyntaxKind::IDENT => install(&identifier_name(text)),
        SyntaxKind::DOTS => Ok(symbols().dots),
        SyntaxKind::DOTDOTI => install(text),
        _ => install(text),
    }
}

/// A numeric literal: decimal, hexadecimal, or hexadecimal with a binary exponent.
/// The value a suffixed number runs as: `1.5f64` is an R double and `5i64` a native `I64`.
unsafe fn suffixed_number_value(text: &str, negative: bool) -> R<Sexp> {
    let (number, suffix) =
        split_number_suffix(text).ok_or_else(|| error(format!("malformed number `{text}`")))?;
    match (suffix, negative) {
        (NumberType::F64, true) => values::scalar_real(-parse_number(number)),
        (NumberType::F64, false) => values::scalar_real(parse_number(number)),
        (NumberType::I64, true) => crate::numbers::from_text(&format!("-{number}")),
        (NumberType::I64, false) => crate::numbers::from_text(number),
    }
}

/// A numeric literal's value, read as GNU R's lexer reads it.
pub fn parse_number(text: &str) -> f64 {
    strtod(text.as_bytes(), b'.', false, Exactness::Allow).value
}

/// The content of a quoted string literal, with escapes resolved.
pub fn string_literal(text: &str) -> R<String> {
    let inner = &text[1..text.len().saturating_sub(1).max(1)];
    unescape(inner).map_err(error)
}

/// Resolves R's string escapes, with each byte a byte escape leaves outside UTF-8 as its
/// stand-in.
pub fn unescape(text: &str) -> Result<String, String> {
    let (bytes, _) = unescape_bytes(text).map_err(|failure| failure.message)?;
    Ok(values::escape_invalid_bytes(&bytes).into_owned())
}

/// A string escape GNU R's lexer rejects: the class of its parse error, the message, and the
/// byte offset of the last character the lexer read, which is where GNU R reports it.
pub struct EscapeFailure {
    pub class: &'static str,
    pub message: String,
    pub offset: usize,
    pub value: ConditionValue,
}

/// Resolves R's string escapes. Octal and `\x` escapes give single bytes, so the result need
/// not be UTF-8; the flag says whether the literal holds such an escape or a byte outside UTF-8,
/// which leaves the string in the native encoding instead of marked as UTF-8, as in GNU R.
pub fn unescape_bytes(text: &str) -> Result<(Vec<u8>, bool), EscapeFailure> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut native = false;
    let decoded = unescape_quoted(text, |piece| match piece {
        EscapePiece::Text(character) => match escaped_byte(character) {
            Some(byte) => {
                bytes.push(byte);
                native = true;
            }
            None => bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes()),
        },
        EscapePiece::Byte(byte) => {
            bytes.push(byte);
            native = true;
        }
        EscapePiece::Unicode(character) => {
            bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        }
    });
    let Err(refused) = decoded else {
        return Ok((bytes, native));
    };
    let letter = |upper| if upper { 'U' } else { 'u' };
    let (class, message, value) = match refused.kind {
        EscapeErrorKind::Nul => (
            "nulNotAllowed",
            "nul character not allowed".to_owned(),
            ConditionValue::Null,
        ),
        EscapeErrorKind::OctalTooLarge(value) => (
            "invalidOctal",
            format!("\\{value:o} exceeds maximum allowed octal value \\377"),
            ConditionValue::Integer(value as i32),
        ),
        EscapeErrorKind::HexWithoutDigits => (
            "badHex",
            "'\\x' used without hex digits in character string".to_owned(),
            ConditionValue::Null,
        ),
        EscapeErrorKind::UnicodeWithoutDigits { upper } => (
            "badUnicodeHex",
            format!(
                "'\\{}' used without hex digits in character string",
                letter(upper)
            ),
            ConditionValue::Null,
        ),
        EscapeErrorKind::UnicodeUnclosed { upper } => (
            "invalidUnicode",
            format!(
                "invalid {} sequence",
                if upper { "\\U{xxxxxxxx}" } else { "\\u{xxxx}" }
            ),
            ConditionValue::Null,
        ),
        EscapeErrorKind::UnicodeInvalid {
            upper,
            braced,
            value,
        } => {
            let form = match (upper, braced) {
                (false, true) => "\\u{xxxx}",
                (false, false) => "\\uxxxx",
                (true, true) => "\\U{xxxxxxxx}",
                (true, false) => "\\Uxxxxxxxx",
            };
            (
                "invalidUnicode",
                format!("invalid {form} value {value:x}"),
                ConditionValue::Integer(value as i32),
            )
        }
        EscapeErrorKind::Unrecognized(other) => (
            "unrecognizedEscape",
            format!("'\\{other}' is an unrecognized escape in character string"),
            ConditionValue::Text(other.to_string()),
        ),
        EscapeErrorKind::Mixed => (
            "mixedEscapes",
            "mixing Unicode and octal/hex escapes in a string is not allowed".to_owned(),
            ConditionValue::Null,
        ),
    };
    Err(EscapeFailure {
        class,
        message,
        offset: refused.last,
        value,
    })
}

/// `parse(file, n, text, prompt, srcfile, encoding)`.
unsafe fn do_parse(call: &Call<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let count = values::as_integer_scalar(call.arg(1));
    let text = call.arg(2);
    let encoding = call.arg(5);
    if encoding.kind() != SexpType::String || values::length(encoding) != 1 {
        return Err(error("invalid 'encoding' value"));
    }
    // The session's text is UTF-8 whatever the argument says, as in GNU R's UTF-8 locales.
    match &*values::char_str(encoding.element(0)) {
        "unknown" | "native.enc" | "UTF-8" => {}
        "latin1" => {
            crate::conditions::warning("argument encoding=\"latin1\" is ignored in MBCS locales")?
        }
        other => crate::conditions::warning(format!(
            "argument 'encoding = \"{other}\"' will be ignored"
        ))?,
    }
    let source = if text != nil() {
        let lines: Vec<String> =
            values::strings(crate::coerce::coerce_vector(text, SexpType::String)?)
                .into_iter()
                .map(|line| line.unwrap_or_else(|| "NA".into()))
                .collect();
        lines.join("\n")
    } else {
        let mut content = crate::connections::read_all_text(file)?;
        // A text argument comes without the newline of its last line, and the error locations
        // count lines the same way for a file.
        if content.ends_with('\n') {
            content.pop();
        }
        content
    };
    // GNU R names the srcfile in a grammar error, and nothing without one: the name `parse`
    // passes, or the `filename` of a srcfile environment.
    let srcfile = call.arg(4);
    let filename = match srcfile.kind() {
        SexpType::Environment => crate::eval::find_var_in_frame(install("filename")?, srcfile)?,
        _ => srcfile,
    };
    let name = match filename.kind() {
        SexpType::String if values::length(filename) > 0 => values::string_at(filename, 0)
            .map_or_else(|| "NA".to_owned(), |filename| filename.into_owned()),
        _ => String::new(),
    };
    // A file that `source()` or `parse(file = )` reads runs as the checker elaborates it, as
    // `ry run` runs a script: `source()` passes the file's name, or a srcfile copy of its lines.
    let reads_file = match srcfile.kind() {
        SexpType::Environment => {
            values::is_true(crate::eval::find_var_in_frame(install("isFile")?, srcfile)?)
        }
        SexpType::String => text == nil(),
        _ => false,
    };
    let source = match SOURCE_ELABORATOR.get() {
        Some(elaborate)
            if reads_file
                && std::path::Path::new(&crate::connections::expand_path(&name)).is_file() =>
        {
            match elaborate(&crate::connections::expand_path(&name), &source) {
                // Source references count columns in the text they were parsed from, so the
                // srcfile copy holds the program's lines, as `ry run` shows a script's.
                Ok(Some(program)) if srcfile.kind() == SexpType::Environment => {
                    let lines: Vec<&str> = program.lines().collect();
                    crate::heap().define(
                        srcfile,
                        install("lines")?,
                        values::string_vector(&lines)?,
                    )?;
                    program
                }
                Ok(Some(program)) => program,
                Ok(None) => source,
                Err(()) => {
                    return Err(error(format!(
                        "ry could not elaborate the typed code in {name}"
                    )));
                }
            }
        }
        _ => source,
    };
    let expressions = parse_with_srcfile(&source, &name, srcfile)?;
    if text == nil() && count < 0 {
        crate::heap().root(expressions);
        crate::ahead::parsed_file(expressions, call.env()?)?;
    }
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
