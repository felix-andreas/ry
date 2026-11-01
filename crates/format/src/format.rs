//! A preserving formatter: single-line constructs stay single-line and
//! multiline ones keep their line structure. Syntax errors refuse the file,
//! except inside `#:` annotations, whose block is kept verbatim.

use {
    serde::Deserialize,
    std::{
        error::Error,
        fmt::{self, Display},
        iter::{once, repeat_n, successors},
    },
    syntax::{
        SyntaxElement,
        SyntaxKind::{self, *},
        SyntaxNode, SyntaxToken, TextRange, TextSize, lex, parse,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub indent_width: usize,
    pub line_ending: LineEnding,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            indent_width: 2,
            line_ending: LineEnding::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineEnding {
    Auto,
    Lf,
    CrLf,
}

/// `line` and `column` are zero-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Syntax error: {} at line {}, column {}",
            self.message,
            self.line + 1,
            self.column + 1
        )
    }
}

impl Error for FormatError {}

pub fn format(source: &str, config: Config) -> Result<String, FormatError> {
    let parse = parse(source);
    let root = parse.syntax_node();
    let line_starts = line_starts(source);
    let error_at = |message: String, offset: TextSize| {
        let offset = usize::from(offset);
        let line = line_of(&line_starts, offset);
        FormatError {
            message,
            line,
            column: offset - line_starts[line],
        }
    };

    if let Some(error) = parse.errors().iter().find(|error| !error.in_annotation) {
        return Err(error_at(error.message.clone(), error.range.start()));
    }

    // A mid-line `#:` region with its statement continuing below (`f <-#: T`)
    // has no stable line layout, so it refuses like a syntax error.
    for node in root.descendants().filter(|node| node.kind() == ANNOTATION) {
        let offset = node.text_range().start();
        if line_prefix(source, &line_starts, usize::from(offset))
            .chars()
            .all(char::is_whitespace)
        {
            continue;
        }
        // Blocks are expressions in R, so the statement boundary is the
        // nearest brace block or the root, not the nearest expression.
        let statement = |mut candidate: SyntaxNode| {
            while let Some(parent) = candidate.parent() {
                if parent.kind() == BRACE_EXPR || parent.parent().is_none() {
                    break;
                }
                candidate = parent;
            }
            candidate
        };
        let continues = successors(
            node.last_token().and_then(|token| token.next_token()),
            SyntaxToken::next_token,
        )
        .find(|token| !is_blank(token.kind()))
        .and_then(|token| token.parent())
        .is_some_and(|parent| statement(parent) == statement(node.clone()));
        if continues {
            return Err(error_at(
                "a `#:` annotation cannot interrupt an expression. Move it to its own line"
                    .to_owned(),
                offset,
            ));
        }
    }

    let line_ending = match config.line_ending {
        LineEnding::Auto
            if source
                .find('\n')
                .is_some_and(|at| source[..at].ends_with('\r')) =>
        {
            "\r\n"
        }
        LineEnding::Auto | LineEnding::Lf => "\n",
        LineEnding::CrLf => "\r\n",
    };

    let mut formatter = Formatter {
        source,
        line_starts,
        error_ranges: parse
            .errors()
            .iter()
            .filter(|error| error.in_annotation)
            .map(|error| error.range)
            .collect(),
        indent: " ".repeat(config.indent_width),
        line_ending,
        out: String::with_capacity(source.len() * 3 / 2),
        comment_end: None,
    };
    formatter.source_file(&root);
    Ok(formatter.out)
}

/// Shared with the coverage-guided `fuzz/` targets so the contract has one home.
/// The edits that format each R chunk of a literate document (`.Rmd`, `.qmd`, `.Rnw`): the
/// byte range of a chunk's code and its formatted text, for the chunks that change. Prose and
/// fences are never touched. A chunk whose fence is indented (inside a list item) is left alone,
/// because its lines carry the fence's indentation, which the formatter would remove. An error
/// names its line in the document.
pub fn format_chunks(
    document: &str,
    config: Config,
) -> Result<Vec<(std::ops::Range<usize>, String)>, FormatError> {
    let mut edits = Vec::new();
    for region in syntax::literate::r_regions(document) {
        if region.kind != (syntax::literate::RegionKind::Chunk { indented: false }) {
            continue;
        }
        let code = &document[region.range.clone()];
        if code.trim().is_empty() {
            continue;
        }
        let formatted = format(code, config).map_err(|error| FormatError {
            line: error.line + document[..region.range.start].matches('\n').count(),
            ..error
        })?;
        if formatted != code {
            edits.push((region.range, formatted));
        }
    }
    Ok(edits)
}

/// A literate document with every R chunk formatted, as [`format_chunks`] describes.
pub fn format_literate(document: &str, config: Config) -> Result<String, FormatError> {
    let mut formatted = document.to_owned();
    for (range, text) in format_chunks(document, config)?.into_iter().rev() {
        formatted.replace_range(range, &text);
    }
    Ok(formatted)
}

pub fn check_format_invariants(input: &str) {
    let first = format(input, Config::default());
    let again = format(input, Config::default());
    assert_eq!(first, again, "non-deterministic format for input {input:?}");

    if let Ok(output) = first {
        assert_eq!(
            significant_tokens(input),
            significant_tokens(&output),
            "formatting changed the code for input {input:?} (output {output:?})"
        );
        match format(&output, Config::default()) {
            Ok(second) => assert_eq!(second, output, "format not idempotent for input {input:?}"),
            Err(error) => panic!(
                "formatted output no longer formats for input {input:?}: {error} (output {output:?})"
            ),
        }
    }
}

struct Formatter<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    /// Annotation-grammar errors: the blocks containing them render verbatim.
    error_ranges: Vec<TextRange>,
    indent: String,
    line_ending: &'static str,
    out: String,
    /// Output length right after the last comment: anything written on the
    /// same line after it would become comment text.
    comment_end: Option<usize>,
}

impl Formatter<'_> {
    fn source_file(&mut self, root: &SyntaxNode) {
        let elements = child_elements(root);
        if elements.first().and_then(directive) == Some(Directive::SkipFile) {
            self.out.push_str(self.source);
            return;
        }
        self.item_sequence(&elements, 0);
        if !elements.is_empty() || !self.source.is_empty() {
            self.newline(0);
        }
    }

    fn item_sequence(&mut self, elements: &[SyntaxElement], level: usize) {
        let mut enabled = true;
        let mut pending_directive = None;
        let mut previous: Option<&SyntaxElement> = None;
        for element in elements {
            match pending_directive {
                Some(Directive::On) => enabled = true,
                Some(Directive::Off) => enabled = false,
                _ => {}
            }
            // A trailing comment attaches inside the code it follows once the
            // formatter drops the `;` between them, so only a comment that
            // starts its line is stable as a directive.
            pending_directive = directive(element).filter(|_| !self.trails(previous, element));
            if element.kind() == SEMICOLON {
                continue;
            }
            if enabled {
                if is_comment(element) && self.trails(previous, element) {
                    self.space();
                } else {
                    self.newlines_between(previous, element, level);
                }
                self.element(element, level, false);
            } else {
                // Only the directive comments inside an off region are formatted.
                let range = element.text_range();
                let start = previous.map_or(range.start(), |previous| previous.text_range().end());
                let end = if pending_directive.is_some() {
                    range.start()
                } else {
                    range.end()
                };
                self.out.push_str(&self.source[TextRange::new(start, end)]);
                if pending_directive.is_some() {
                    self.element(element, level, false);
                }
            }
            previous = Some(element);
        }
    }

    fn element(&mut self, element: &SyntaxElement, level: usize, make_multiline: bool) {
        // Catches joins that do not know a comment interposed (an operand
        // continuing an operator across a commented line break).
        if let Some(comment_end) = self.comment_end.take()
            && element.kind() != COMMENT
            && !self.out[comment_end..].contains('\n')
        {
            self.newline(level);
        }
        match element {
            SyntaxElement::Node(node) => self.node(node, level, make_multiline),
            SyntaxElement::Token(token) => match token.kind() {
                COMMENT => self.comment(token),
                STRING => self.string(token),
                _ => self.out.push_str(token.text()),
            },
        }
    }

    fn node(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        if self.skip_exempt(node) {
            self.emit_skipped(node);
            return;
        }
        match node.kind() {
            BINARY_EXPR => self.binary(node, level),
            UNARY_EXPR => self.unary(node, level),
            PAREN_EXPR => self.paren(node, level),
            BRACE_EXPR => self.braces(node, level, make_multiline),
            CALL_EXPR | SUBSET_EXPR | SUBSET2_EXPR => self.call_like(node, level),
            ARGUMENT_LIST | PARAMETER_LIST => self.argument_list(node, level),
            DOLLAR_EXPR | AT_EXPR | NAMESPACE_EXPR => self.field_access(node, level),
            FUNCTION_DEF => self.function_definition(node, level),
            IF_EXPR => self.conditional(node, level, make_multiline),
            WHILE_EXPR => self.conditional(node, level, true),
            FOR_EXPR => self.for_expression(node, level),
            REPEAT_EXPR => self.repeat_expression(node, level),
            MATCH_EXPR => self.match_expression(node, level),
            MATCH_ARM => self.match_arm(node, level),
            MATCH_GUARD => self.match_guard(node, level),
            PATTERN_WILDCARD | PATTERN_BIND | PATTERN_LITERAL | PATTERN_TAG | PATTERN_LIST
            | PATTERN_REST | PATTERN_OR | PATTERN_PAREN => self.pattern(node),
            ANNOTATION => {
                self.annotation(node, level);
                // Nothing may follow an annotation on its line either.
                self.comment_end = Some(self.out.len());
            }
            TYPE_ASCRIPTION | RETURN_TYPE | TYPE_BINDER_LIST => self.inline_annotation(node),
            TYPE_DEFINITION => self.type_definition(node, level),
            NEW_TYPE => {
                self.out.push('@');
                for child in node.children() {
                    self.inline_annotation(&child);
                }
            }
            _ => {
                for element in child_elements(node) {
                    self.element(&element, level, false);
                }
            }
        }
    }

    /// A skip comment alone on the line above, or trailing on the node's last
    /// line, where it may attach inside the node or as its next sibling.
    fn skip_exempt(&self, node: &SyntaxNode) -> bool {
        let is_skip = |token: &SyntaxToken| {
            token.kind() == COMMENT && parse_directive(token.text()) == Some(Directive::Skip)
        };
        let previous_significant = |element: Option<SyntaxElement>| {
            successors(element, SyntaxElement::prev_sibling_or_token)
                .find(|element| !is_blank(element.kind()))
        };
        if let Some(SyntaxElement::Token(comment)) =
            previous_significant(node.prev_sibling_or_token())
            && is_skip(&comment)
            && previous_significant(comment.prev_sibling_or_token())
                .is_none_or(|before| !self.same_line(before.text_range(), comment.text_range()))
        {
            return true;
        }
        if let Some(SyntaxElement::Token(comment)) = successors(
            node.next_sibling_or_token(),
            SyntaxElement::next_sibling_or_token,
        )
        .find(|element| element.kind() != WHITESPACE)
            && is_skip(&comment)
        {
            return true;
        }
        if let Some(token) = last_token_before(node, node.text_range().end())
            && is_skip(&token)
        {
            return last_token_before(node, token.text_range().start())
                .is_some_and(|code| self.same_line(code.text_range(), token.text_range()));
        }
        false
    }

    /// When only whitespace precedes the node on its line, the original
    /// indentation replaces the emitted one, so column alignment survives.
    fn emit_skipped(&mut self, node: &SyntaxNode) {
        let range = significant_range(&SyntaxElement::Node(node.clone()));
        let leading = line_prefix(self.source, &self.line_starts, usize::from(range.start()));
        if leading.chars().all(char::is_whitespace) {
            let content_length = self.out.trim_end_matches(' ').len();
            if content_length == 0 || self.out[..content_length].ends_with(self.line_ending) {
                self.out.truncate(content_length);
                self.out.push_str(leading);
            }
        }
        self.out.push_str(&self.source[range]);
    }

    fn comment(&mut self, token: &SyntaxToken) {
        self.comment_end = Some(self.out.len());
        let text = token.text().trim_end();
        let mut chars = text.chars();
        match chars.nth(1) {
            Some(char @ ('\'' | '*')) => match chars.next() {
                Some(' ') | None => self.out.push_str(text),
                Some(_) if char == '\'' && chars.clone().any(|c| c == '\'') => {
                    self.out.push_str(text)
                }
                Some(other) => {
                    self.out.push('#');
                    self.out.push(char);
                    self.out.push(' ');
                    self.out.push(other);
                    self.out.extend(chars);
                }
            },
            Some('#' | '!' | '|' | ' ') | None => self.out.push_str(text),
            Some(other) => {
                self.out.push_str("# ");
                self.out.push(other);
                self.out.extend(chars);
            }
        }
    }

    fn string(&mut self, token: &SyntaxToken) {
        let text = token.text();
        if matches!(text.chars().next(), Some('r' | 'R')) {
            self.out.push_str(text);
            return;
        }
        let content = &text[1..text.len().saturating_sub(1)];
        let mut all_quotes_escaped = true;
        let mut previous_was_escape = false;
        for char in content.chars() {
            if char == '"' {
                all_quotes_escaped &= previous_was_escape;
            }
            previous_was_escape = char == '\\' && !previous_was_escape;
        }
        let quote = if all_quotes_escaped { '"' } else { '\'' };
        self.out.push(quote);
        self.out.push_str(content);
        self.out.push(quote);
    }

    fn binary(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let Some(operator_index) = elements
            .iter()
            .position(|element| is_operator(element.kind()))
        else {
            for element in &elements {
                self.element(element, level, false);
            }
            return;
        };
        let operator = &elements[operator_index];
        // `x : :None` keeps its spaces: `x::None` would lex as `::`. The tag can
        // also start a larger operand, as in `x : :None$field`.
        let has_spacing = !matches!(operator.kind(), COLON | CARET)
            || (operator.kind() == COLON
                && elements[operator_index + 1..].iter().any(|element| {
                    element
                        .as_node()
                        .and_then(SyntaxNode::first_token)
                        .is_some_and(|token| token.kind() == COLON)
                }));
        let break_after_operator = elements[operator_index + 1..]
            .iter()
            .find(|element| element.as_node().is_some())
            .is_some_and(|rhs| !self.same_line(operator.text_range(), rhs.text_range()));

        let mut seen_operator = false;
        for (index, (previous, element)) in with_previous(&elements).enumerate() {
            match element.kind() {
                COMMENT | ANNOTATION => {
                    // An own-line comment after the last operand belongs to the
                    // enclosing construct's level.
                    if self.trails(previous, element)
                        || elements[index + 1..].iter().all(is_comment)
                    {
                        self.place_comment(previous, element, level, level);
                    } else {
                        self.continuation(element, level);
                    }
                }
                kind => {
                    let is_operator = is_operator(kind);
                    if !seen_operator && !is_operator {
                        self.element(element, level, false);
                    } else if comment_before(node, element.text_range().start())
                        || (!is_operator && break_after_operator)
                    {
                        // The comment may be the previous operand's own last
                        // token; gluing after it would comment it out.
                        self.continuation(element, level);
                    } else {
                        if has_spacing {
                            self.space();
                        }
                        self.element(element, level, false);
                    }
                    seen_operator |= is_operator;
                }
            }
        }
    }

    fn unary(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        // `...` also parses as NAME, but the tilde hugs identifiers only
        // (`~x` but `~ ...`).
        let operand_is_name = elements
            .iter()
            .rev()
            .find_map(SyntaxElement::as_node)
            .is_some_and(|operand| {
                operand.kind() == NAME
                    && operand
                        .children_with_tokens()
                        .any(|child| child.kind() == IDENT)
            });
        let has_space = elements
            .first()
            .is_some_and(|operator| operator.kind() == TILDE)
            && !operand_is_name;

        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION if self.trails(previous, element) => {
                    self.space();
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => {
                    self.newlines_between(previous, element, level + 1);
                    self.element(element, level + 1, false);
                }
                _ if element.as_node().is_none() => self.element(element, level, false),
                _ if previous.is_some_and(is_comment) => self.continuation(element, level),
                _ => {
                    if previous.is_some() && has_space {
                        self.space();
                    }
                    self.element(element, level, false);
                }
            }
        }
    }

    fn field_access(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let operator = elements
            .iter()
            .position(|element| matches!(element.kind(), DOLLAR | AT | COLON2 | COLON3));
        // Skip comments trailing the operator: the field name's line decides.
        let is_multiline = operator.is_some_and(|operator| {
            let lhs = &elements[operator.saturating_sub(1)];
            elements[operator + 1..]
                .iter()
                .find(|element| element.kind() != COMMENT)
                .is_some_and(|rhs| !self.same_line(lhs.text_range(), rhs.text_range()))
        });

        let mut seen_operator = false;
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level),
                DOLLAR | AT | COLON2 | COLON3 => {
                    self.element(element, level, false);
                    seen_operator = true;
                }
                _ if seen_operator && is_multiline => self.continuation(element, level),
                _ => self.element(element, level, false),
            }
        }
    }

    fn paren(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let hug = self.hugs(node, &elements, R_PAREN, false);
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                L_PAREN => self.element(element, level, false),
                R_PAREN => {
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level),
                _ if hug => self.element(element, level, false),
                _ => self.continuation(element, level),
            }
        }
    }

    fn braces(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        let elements = child_elements(node);
        let is_multiline =
            !self.hugs(node, &elements, R_BRACE, true) || make_multiline || forces_multiline(node);
        let mut body: Vec<SyntaxElement> = elements
            .into_iter()
            .filter(|element| !matches!(element.kind(), L_BRACE | R_BRACE))
            .collect();
        // A body of only semicolons is empty: emitting its lines would leave a
        // blank interior line that a second pass collapses.
        if body.iter().all(|element| element.kind() == SEMICOLON) {
            body.clear();
        }

        self.out.push('{');
        if is_multiline {
            let open_line = self.line(node.text_range().start());
            let leading = body
                .iter()
                .take_while(|element| {
                    element.kind() == COMMENT
                        && self.line(element.text_range().start()) == open_line
                })
                .count();
            for element in &body[..leading] {
                self.space();
                self.element(element, level, false);
            }
            let rest = &body[leading..];
            if !rest.is_empty() {
                self.out.push_str(self.line_ending);
                self.item_sequence(rest, level + 1);
            }
            if !body.is_empty() {
                self.newline(level);
            }
        } else {
            let mut first = true;
            for element in &body {
                match element.kind() {
                    SEMICOLON => {}
                    COMMENT => {
                        self.space();
                        self.element(element, level, false);
                    }
                    _ => {
                        self.out.push_str(if first { " " } else { "; " });
                        self.element(element, level, false);
                        first = false;
                    }
                }
            }
            if !body.is_empty() {
                self.space();
            }
        }
        self.out.push('}');
    }

    fn call_like(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let additional_indent = elements
            .first()
            .and_then(SyntaxElement::as_node)
            .filter(|target| matches!(target.kind(), DOLLAR_EXPR | AT_EXPR))
            .is_some_and(|target| {
                let parts = child_elements(target);
                parts
                    .iter()
                    .position(|part| matches!(part.kind(), DOLLAR | AT))
                    .and_then(|operator| parts.get(operator + 1))
                    .zip(parts.first())
                    .is_some_and(|(rhs, lhs)| !self.same_line(lhs.text_range(), rhs.text_range()))
            });

        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION if self.trails(previous, element) => {
                    self.space();
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => self.continuation(element, level),
                ARGUMENT_LIST => {
                    // `:None (x)` calls a tag; hugged, `:None(x)` would be its payload.
                    if let Some(tag) = previous.and_then(SyntaxElement::as_node)
                        && tag.kind() == TAG_EXPR
                        && tag.children().all(|child| child.kind() != ARGUMENT_LIST)
                    {
                        self.space();
                    }
                    if let Some(list) = element.as_node() {
                        self.argument_list(list, level + usize::from(additional_indent));
                    }
                }
                _ => self.element(element, level, false),
            }
        }
    }

    fn argument_list(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let list_range = node.text_range();
        let is_multiline = self.line(list_range.start()) != self.line(list_range.end());
        let arguments: Vec<&SyntaxElement> = elements
            .iter()
            .filter(|element| matches!(element.kind(), ARGUMENT | PARAMETER))
            .collect();
        let is_empty = arguments
            .iter()
            .all(|argument| argument.text_range().is_empty());

        // Tokens, not elements: comments attach deep inside arguments, and hole
        // arguments are zero-width. For `]]` the first `]` is the placement
        // anchor; the second glues on.
        let close_previous_token = elements
            .iter()
            .find(|element| matches!(element.kind(), R_PAREN | R_BRACKET))
            .and_then(|close| last_token_before(node, close.text_range().start()));
        // A one-item literal keeps its comma hugged, `(x = 1,)`: it is a
        // marker, not a hole argument.
        let is_literal = node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), RECORD_EXPR | TUPLE_EXPR | ARRAY_EXPR));
        let trailing_space = !is_literal
            && close_previous_token
                .as_ref()
                .is_none_or(|token| token.kind() == COMMA);
        // Hug: some argument starts on the open line and ends on the closer's
        // line. A multiline signature always expands one parameter per line.
        let hug = node.kind() != PARAMETER_LIST
            && close_previous_token.as_ref().is_none_or(|close_previous| {
                close_previous.kind() != COMMENT
                    && arguments.iter().any(|argument| {
                        let range = significant_range(argument);
                        !range.is_empty()
                            && self.line(range.start()) == self.line(list_range.start())
                            && self.line(range.end())
                                == self.line(close_previous.text_range().end())
                    })
            });
        let expand = is_multiline && !hug;

        let mut seen_argument = false;
        let mut seen_close = false;
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                R_PAREN | R_BRACKET => {
                    if !seen_close {
                        if expand && !is_empty {
                            self.newline(level);
                        } else if trailing_space {
                            // Also for all-empty lists (`alist(, )`) and
                            // collapsed multiline lists, which must match what
                            // their single-line output reformats to.
                            self.space();
                        }
                        seen_close = true;
                    }
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION if self.trails(previous, element) => {
                    self.space();
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => {
                    self.newlines_between(previous, element, level + 1);
                    self.element(element, level + 1, false);
                }
                COMMA => {
                    let start = element.text_range().start();
                    if expand
                        && (comment_before(node, start)
                            || previous.is_none_or(|previous| {
                                previous.kind() == COMMA || previous.text_range().is_empty()
                            }))
                    {
                        self.newline(level + 1);
                    } else if last_token_before(node, start).is_some_and(|token| token.kind() == EQ)
                    {
                        // A fall-through argument (`z = ,`) keeps its space.
                        self.space();
                    }
                    self.out.push(',');
                }
                ARGUMENT | PARAMETER => {
                    if let Some(argument) = element
                        .as_node()
                        .filter(|argument| !argument.text_range().is_empty())
                    {
                        if expand {
                            if seen_argument {
                                self.newlines_between(previous, element, level + 1);
                            } else {
                                self.newline(level + 1);
                            }
                        } else if seen_argument {
                            self.space();
                        }
                        self.argument(argument, level + usize::from(expand));
                    }
                    seen_argument = true;
                }
                _ => self.element(element, level, false),
            }
        }
    }

    /// Arms go one per line, whatever separated them. A `;` separator is
    /// dropped like a statement's, and a `,` stays glued to its arm.
    fn match_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let Some(open) = elements
            .iter()
            .position(|element| element.kind() == L_BRACE)
        else {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        };
        for (previous, element) in with_previous(&elements[..open]) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level),
                _ => {
                    if previous.is_some() {
                        self.space();
                    }
                    self.element(element, level, false);
                }
            }
        }
        self.out.push_str(" {");
        let body: Vec<&SyntaxElement> = elements[open + 1..]
            .iter()
            .filter(|element| !matches!(element.kind(), R_BRACE | SEMICOLON))
            .collect();
        let open_line = self.line(elements[open].text_range().start());
        let leading = body
            .iter()
            .take_while(|element| {
                element.kind() == COMMENT && self.line(element.text_range().start()) == open_line
            })
            .count();
        for element in &body[..leading] {
            self.space();
            self.element(element, level, false);
        }
        let rest = &body[leading..];
        if !rest.is_empty() {
            self.out.push_str(self.line_ending);
        }
        let mut previous: Option<&SyntaxElement> = None;
        for element in rest.iter().copied() {
            if element.kind() == COMMA {
                if previous.is_some_and(is_comment) {
                    self.newline(level + 1);
                }
                self.out.push(',');
                continue;
            }
            if is_comment(element) && self.trails(previous, element) {
                self.space();
            } else {
                self.newlines_between(previous, element, level + 1);
            }
            self.element(element, level + 1, false);
            previous = Some(element);
        }
        if !body.is_empty() {
            self.newline(level);
        }
        self.out.push('}');
    }

    fn match_arm(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level + 1),
                FAT_ARROW => self.out.push_str(" =>"),
                _ if previous.is_some() => {
                    self.space();
                    self.element(element, level, false);
                }
                _ => self.element(element, level, false),
            }
        }
    }

    fn match_guard(&mut self, node: &SyntaxNode, level: usize) {
        for (previous, element) in with_previous(&child_elements(node)) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level + 1),
                IF_KW => self.out.push_str("if "),
                _ => self.element(element, level, false),
            }
        }
    }

    /// A pattern prints on one line: `, ` between items, ` = ` in a field and
    /// ` | ` between alternatives. One holding a comment is copied as written,
    /// because joining its lines would comment out the rest of the pattern.
    fn pattern(&mut self, node: &SyntaxNode) {
        if node
            .descendants_with_tokens()
            .any(|element| matches!(element.kind(), COMMENT | ANNOTATION))
        {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        }
        let mut pending_space = false;
        for token in node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia())
        {
            match token.kind() {
                R_PAREN => pending_space = false,
                EQ | PIPE => pending_space = true,
                _ => {}
            }
            if pending_space {
                self.space();
            }
            match token.kind() {
                STRING => self.string(&token),
                _ => self.out.push_str(token.text()),
            }
            pending_space = matches!(token.kind(), COMMA | EQ | PIPE);
        }
    }

    fn argument(&mut self, node: &SyntaxNode, level: usize) {
        if self.skip_exempt(node) {
            self.emit_skipped(node);
            return;
        }
        let elements = child_elements(node);
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level, level),
                _ if previous.is_some_and(is_comment) => self.continuation(element, level),
                EQ => self.out.push_str(" ="),
                _ => {
                    if previous.is_some_and(|previous| previous.kind() == EQ) {
                        self.space();
                    }
                    self.element(element, level, false);
                }
            }
        }
    }

    /// An inline annotation renders on one line with the spacing a `#:` type gets, so a
    /// type reads the same written either way.
    fn inline_annotation(&mut self, node: &SyntaxNode) {
        if self.keeps_verbatim(node) {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        }
        let mut previous: Option<AnnotationToken> = None;
        for token in node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| !token.kind().is_trivia())
        {
            let Some(kind) = annotation_token_kind(&token) else {
                self.out.push_str(token.text());
                previous = None;
                continue;
            };
            // A directive name lexes as `@` plus adjacent tokens (`@if-unknown`); glue it back.
            if kind == AnnotationTokenKind::Word
                && let Some(previous) = previous.as_mut()
                && previous.kind == AnnotationTokenKind::Directive
                && previous.end == token.text_range().start()
            {
                self.out.push_str(token.text());
                previous.end = token.text_range().end();
                continue;
            }
            if let Some(previous) = &previous
                && annotation_space_between(previous, kind)
            {
                self.out.push(' ');
            }
            self.out.push_str(token.text());
            previous = Some(AnnotationToken {
                kind,
                text: token.text().to_owned(),
                end: token.text_range().end(),
            });
        }
    }

    /// A type definition lays out like the `#:` block it stands for, without the markers:
    /// author line breaks stay, a line inside brackets indents one level per bracket, and
    /// any other continuation line, such as a union member, hangs one level in.
    fn type_definition(&mut self, node: &SyntaxNode, level: usize) {
        let lines = if self.keeps_verbatim(node) {
            None
        } else {
            self.annotation_line_tokens(node)
        };
        let Some(lines) = lines else {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        };
        let rendered = render_annotation_lines(&lines, &self.indent, "", 1);
        for (index, line) in rendered.iter().enumerate() {
            if index > 0 {
                self.newline(level);
            }
            self.out.push_str(line);
        }
    }

    /// An inline annotation holding a comment is copied as written, because joining it onto
    /// one line would comment out the code after it. So is one with a syntax error, whose
    /// tokens may glue into different ones.
    fn keeps_verbatim(&self, node: &SyntaxNode) -> bool {
        let range = node.text_range();
        node.descendants_with_tokens()
            .any(|element| element.kind() == COMMENT)
            || self
                .error_ranges
                .iter()
                .any(|error| range.contains_inclusive(error.start()))
    }

    fn function_definition(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let is_multiline = self.spans_lines(node);
        let body = elements.iter().rev().find(|element| {
            element.as_node().is_some()
                && !matches!(
                    element.kind(),
                    PARAMETER_LIST | RETURN_TYPE | TYPE_BINDER_LIST
                )
        });
        let is_same_line = elements.first().zip(body).is_none_or(|(head, body)| {
            self.line(head.text_range().start()) == self.line(body.text_range().start())
        });

        for (previous, element) in with_previous(&elements) {
            let after_comment = previous.is_some_and(is_comment);
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level, level),
                PURE_MARKER => {
                    self.element(element, level, false);
                    self.out.push(' ');
                }
                PARAMETER_LIST => {
                    if after_comment {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                RETURN_TYPE => {
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                }
                _ if Some(element) == body => {
                    self.space_or_newline(after_comment, level);
                    if !is_multiline {
                        self.element(element, level, false);
                    } else if is_same_line {
                        self.element(element, level, true);
                    } else {
                        self.braced_body(element, level);
                    }
                }
                _ => self.element(element, level, false),
            }
        }
    }

    /// `if` and `while` share one layout. A loop body always renders braced,
    /// so `while` passes `make_multiline`.
    fn conditional(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        let elements = child_elements(node);
        let is_multiline = make_multiline || self.spans_lines(node) || forces_multiline(node);
        let hug = self.condition_hugs(&elements);

        let mut seen_close = false;
        let mut indent_comments = false;
        let mut is_alternative = false;
        for (previous, element) in with_previous(&elements) {
            let after_comment = previous.is_some_and(is_comment);
            match element.kind() {
                ELSE_KW => {
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                    is_alternative = true;
                }
                COMMENT | ANNOTATION => self.place_comment(
                    previous,
                    element,
                    level + usize::from(indent_comments),
                    level,
                ),
                L_PAREN => {
                    indent_comments = true;
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                }
                R_PAREN => {
                    indent_comments = false;
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                _ if element.as_node().is_none() => self.element(element, level, false),
                _ if !seen_close => {
                    if hug {
                        self.element(element, level, false);
                    } else {
                        self.continuation(element, level);
                    }
                }
                _ => {
                    self.space_or_newline(after_comment, level);
                    if !is_multiline {
                        self.element(element, level, false);
                    } else if element.kind() == IF_EXPR && is_alternative {
                        self.element(element, level, true);
                    } else {
                        self.braced_body(element, level);
                    }
                }
            }
        }
    }

    fn for_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let open = elements.iter().find(|element| element.kind() == L_PAREN);
        let variable = elements.iter().find(|element| element.kind() == NAME);
        let sequence = elements
            .iter()
            .skip_while(|element| element.kind() != IN_KW)
            .skip(1)
            .find(|element| element.as_node().is_some());
        let condition_is_multiline = open.zip(sequence).is_some_and(|(open, sequence)| {
            !self.same_line(open.text_range(), sequence.text_range())
        });
        let header_is_multiline = variable.zip(sequence).is_some_and(|(variable, sequence)| {
            !self.same_line(variable.text_range(), sequence.text_range())
        });

        let mut seen_close = false;
        let mut indent_comments = false;
        for (previous, element) in with_previous(&elements) {
            let after_comment = previous.is_some_and(is_comment);
            match element.kind() {
                IN_KW => {
                    if after_comment || header_is_multiline {
                        self.newline(level + 1);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => self.place_comment(
                    previous,
                    element,
                    level + usize::from(indent_comments),
                    level,
                ),
                L_PAREN => {
                    indent_comments = true;
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                }
                R_PAREN => {
                    indent_comments = false;
                    if after_comment || condition_is_multiline {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                _ if Some(element) == variable && !seen_close => {
                    if after_comment || condition_is_multiline {
                        self.continuation(element, level);
                    } else {
                        self.element(element, level, false);
                    }
                }
                _ if Some(element) == sequence => {
                    if after_comment {
                        self.continuation(element, level);
                    } else {
                        self.space();
                        self.element(element, level + usize::from(condition_is_multiline), false);
                    }
                }
                _ if element.as_node().is_some() && seen_close => {
                    self.space_or_newline(after_comment, level);
                    self.braced_body(element, level);
                }
                _ => self.element(element, level, false),
            }
        }
    }

    fn repeat_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        for (previous, element) in with_previous(&elements) {
            if is_comment(element) {
                self.place_comment(previous, element, level, level);
            } else if element.as_node().is_some() {
                self.space_or_newline(previous.is_some_and(is_comment), level);
                self.braced_body(element, level);
            } else {
                self.element(element, level, false);
            }
        }
    }

    /// A clean block re-renders canonically, keeping author line breaks; a block
    /// with an error stays verbatim apart from one space after each `#:`.
    fn annotation(&mut self, node: &SyntaxNode, level: usize) {
        let range = node.text_range();
        // Inclusive end: an error at the block's end offset still belongs to it.
        let has_error = self
            .error_ranges
            .iter()
            .any(|error| range.contains_inclusive(error.start()));
        let groups = if has_error {
            None
        } else {
            self.annotation_line_tokens(node)
        };
        let lines: Vec<String> = match groups {
            Some(groups) => {
                // A leading `;` is dropped, so it does not make the head trail code.
                // A head trailing code renders apart from its continuation
                // lines, so they never indent relative to a column the
                // formatter does not control.
                let trailing_head =
                    !line_prefix(self.source, &self.line_starts, usize::from(range.start()))
                        .chars()
                        .all(|character| character.is_whitespace() || character == ';');
                let (head, rest) = groups.split_at(usize::from(trailing_head).min(groups.len()));
                let mut lines = render_annotation_lines(head, &self.indent, "#: ", 0);
                lines.extend(render_annotation_lines(rest, &self.indent, "#: ", 0));
                lines
            }
            None => self.source[range]
                .lines()
                .map(|line| {
                    let body = line
                        .trim_start()
                        .strip_prefix("#:")
                        .unwrap_or(line)
                        .trim_end();
                    match body.chars().next() {
                        None => "#:".to_owned(),
                        Some(character) if character.is_whitespace() => format!("#:{body}"),
                        Some(_) => format!("#: {}", body.trim_start()),
                    }
                })
                .collect(),
        };
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                self.newline(level);
            }
            self.out.push_str(line);
        }
    }

    /// `None` when a token has no display mapping; the block then goes verbatim.
    fn annotation_line_tokens(&self, node: &SyntaxNode) -> Option<Vec<Vec<AnnotationToken>>> {
        let mut line_tokens: Vec<Vec<AnnotationToken>> = Vec::new();
        let mut current_line = usize::MAX;
        for token in node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            if is_blank(token.kind()) {
                continue;
            }
            let line = self.line(token.text_range().start());
            if line != current_line {
                line_tokens.push(Vec::new());
                current_line = line;
            }
            if token.kind() == ANNOTATION_MARKER {
                continue;
            }
            let kind = annotation_token_kind(&token)?;
            let tokens = line_tokens.last_mut()?;
            // Directive names lex as `@` plus adjacent tokens; glue them back.
            if kind == AnnotationTokenKind::Word
                && let Some(previous) = tokens.last_mut()
                && previous.kind == AnnotationTokenKind::Directive
                && previous.end == token.text_range().start()
            {
                previous.text.push_str(token.text());
                previous.end = token.text_range().end();
                continue;
            }
            tokens.push(AnnotationToken {
                kind,
                text: token.text().to_owned(),
                end: token.text_range().end(),
            });
        }
        Some(line_tokens)
    }

    fn braced_body(&mut self, body: &SyntaxElement, level: usize) {
        if body.kind() == BRACE_EXPR {
            self.element(body, level, true);
        } else {
            self.out.push('{');
            self.newline(level + 1);
            self.element(body, level + 1, false);
            self.newline(level);
            self.out.push('}');
        }
    }

    fn continuation(&mut self, element: &SyntaxElement, level: usize) {
        self.newline(level + 1);
        self.element(element, level + 1, false);
    }

    fn place_comment(
        &mut self,
        previous: Option<&SyntaxElement>,
        comment: &SyntaxElement,
        line_level: usize,
        level: usize,
    ) {
        if self.trails(previous, comment) {
            self.space();
        } else {
            self.newline(line_level);
        }
        self.element(comment, level, false);
    }

    fn space_or_newline(&mut self, break_line: bool, level: usize) {
        if break_line {
            self.newline(level);
        } else {
            self.space();
        }
    }

    fn hugs(
        &self,
        node: &SyntaxNode,
        elements: &[SyntaxElement],
        closer: SyntaxKind,
        whole_body: bool,
    ) -> bool {
        let Some(close) = elements
            .iter()
            .rposition(|element| element.kind() == closer)
        else {
            return true;
        };
        let Some(body) = close.checked_sub(1).and_then(|index| elements.get(index)) else {
            return true;
        };
        let open_line = self.line(node.text_range().start());
        !comment_before(node, elements[close].text_range().start())
            && self.line(body.text_range().start()) == open_line
            && (!whole_body || self.line(body.text_range().end()) == open_line)
    }

    fn condition_hugs(&self, elements: &[SyntaxElement]) -> bool {
        let Some(open) = elements
            .iter()
            .position(|element| element.kind() == L_PAREN)
        else {
            return true;
        };
        let Some(condition) = elements[open + 1..]
            .iter()
            .position(|element| element.as_node().is_some())
        else {
            return true;
        };
        let condition = open + 1 + condition;
        self.same_line(
            elements[open].text_range(),
            elements[condition].text_range(),
        ) && !is_comment(&elements[condition - 1])
            && !elements.get(condition + 1).is_some_and(is_comment)
    }

    fn spans_lines(&self, node: &SyntaxNode) -> bool {
        let range = significant_range(&SyntaxElement::Node(node.clone()));
        self.line(range.start()) != self.line(range.end())
    }

    fn newlines_between(
        &mut self,
        previous: Option<&SyntaxElement>,
        element: &SyntaxElement,
        level: usize,
    ) {
        let count = previous.map_or(0, |previous| {
            self.line(significant_range(element).start())
                .saturating_sub(self.line(significant_range(previous).end()))
                .clamp(1, 2)
        });
        for _ in 0..count {
            self.out.push_str(self.line_ending);
        }
        for _ in 0..level {
            self.out.push_str(&self.indent);
        }
    }

    fn newline(&mut self, level: usize) {
        self.out.push_str(self.line_ending);
        for _ in 0..level {
            self.out.push_str(&self.indent);
        }
    }

    fn space(&mut self) {
        self.out.push(' ');
    }

    fn trails(&self, previous: Option<&SyntaxElement>, element: &SyntaxElement) -> bool {
        previous.is_some_and(|previous| self.same_line(previous.text_range(), element.text_range()))
    }

    fn same_line(&self, left: TextRange, right: TextRange) -> bool {
        self.line(left.end()) == self.line(right.start())
    }

    fn line(&self, offset: TextSize) -> usize {
        line_of(&self.line_starts, usize::from(offset))
    }
}

fn with_previous(
    elements: &[SyntaxElement],
) -> impl Iterator<Item = (Option<&SyntaxElement>, &SyntaxElement)> {
    once(None).chain(elements.iter().map(Some)).zip(elements)
}

fn child_elements(node: &SyntaxNode) -> Vec<SyntaxElement> {
    node.children_with_tokens()
        .filter(|element| !is_blank(element.kind()))
        .collect()
}

fn line_starts(source: &str) -> Vec<usize> {
    // A bare `\r` ends a line, as in the lexer; diverging would make tokens
    // split by a lone `\r` look same-line and glue into comment text.
    let mut starts = vec![0];
    let bytes = source.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' || (byte == b'\r' && bytes.get(index + 1) != Some(&b'\n')) {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_of(line_starts: &[usize], offset: usize) -> usize {
    line_starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1)
}

fn line_prefix<'source>(
    source: &'source str,
    line_starts: &[usize],
    offset: usize,
) -> &'source str {
    &source[line_starts[line_of(line_starts, offset)]..offset]
}

fn is_blank(kind: SyntaxKind) -> bool {
    matches!(kind, WHITESPACE | NEWLINE)
}

fn is_comment(element: &SyntaxElement) -> bool {
    matches!(element.kind(), COMMENT | ANNOTATION)
}

fn is_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        PLUS | MINUS
            | STAR
            | SLASH
            | CARET
            | SPECIAL
            | PIPE_GREATER
            | LESS
            | GREATER
            | LESS_EQ
            | GREATER_EQ
            | EQ2
            | BANG_EQ
            | AMP
            | AMP2
            | PIPE
            | PIPE2
            | TILDE
            | QUESTION
            | LESS_MINUS
            | LESS2_MINUS
            | MINUS_GREATER
            | MINUS_GREATER2
            | EQ
            | COLON_EQ
            | COLON
    )
}

/// Non-empty loop bodies always render braced and multiline, so single-line
/// layout choices must consult this or a second pass formats differently.
fn forces_multiline(node: &SyntaxNode) -> bool {
    node.descendants()
        .filter(|descendant| matches!(descendant.kind(), FOR_EXPR | WHILE_EXPR | REPEAT_EXPR))
        .any(|descendant| {
            descendant.children().last().is_some_and(|body| {
                body.kind() != BRACE_EXPR
                    || body.children_with_tokens().any(|child| {
                        !matches!(
                            child.kind(),
                            L_BRACE | R_BRACE | WHITESPACE | NEWLINE | SEMICOLON
                        )
                    })
            })
        })
}

/// Some nodes swallow trailing trivia (an argument keeps its newline), and
/// line math must never see it.
fn significant_range(element: &SyntaxElement) -> TextRange {
    match element {
        SyntaxElement::Token(token) => token.text_range(),
        SyntaxElement::Node(node) => {
            let full = node.text_range();
            let start = node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|token| !is_blank(token.kind()))
                .map_or(full.start(), |token| token.text_range().start());
            let end = last_token_before(node, full.end())
                .map_or(full.end(), |token| token.text_range().end());
            TextRange::new(start, end)
        }
    }
}

/// Comments attach deep inside expression nodes, so closer placement must
/// look at tokens, not siblings.
fn last_token_before(node: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .take_while(|token| token.text_range().end() <= offset)
        .filter(|token| !is_blank(token.kind()))
        .last()
}

/// Sibling checks miss a comment swallowed into the previous sibling's
/// subtree, and anything glued after it would become comment text.
fn comment_before(node: &SyntaxNode, offset: TextSize) -> bool {
    last_token_before(node, offset).is_some_and(|token| {
        token.kind() == COMMENT
            || token
                .parent_ancestors()
                .any(|ancestor| ancestor.kind() == ANNOTATION)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Directive {
    Skip,
    SkipFile,
    On,
    Off,
}

fn directive(element: &SyntaxElement) -> Option<Directive> {
    element
        .as_token()
        .filter(|token| token.kind() == COMMENT)
        .and_then(|token| parse_directive(token.text()))
}

fn parse_directive(text: &str) -> Option<Directive> {
    text.trim_start_matches(|c: char| c.is_whitespace() || c == '#')
        .strip_prefix("fmt:")
        .and_then(|rest| match rest.trim() {
            "skip" => Some(Directive::Skip),
            "skip-file" | "skip file" => Some(Directive::SkipFile),
            "on" => Some(Directive::On),
            "off" => Some(Directive::Off),
            _ => None,
        })
}

/// An opener ending its line expands (its closer gets its own line at the
/// opener's indent); anything else hugs. Each line starts with `prefix`, and a
/// continuation line outside every bracket indents `hanging` levels.
fn render_annotation_lines(
    line_tokens: &[Vec<AnnotationToken>],
    indent: &str,
    prefix: &str,
    hanging: usize,
) -> Vec<String> {
    let blank = prefix.trim_end();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_level = 0usize;
    let mut open_delimiters: Vec<OpenDelimiter> = Vec::new();
    let mut previous: Option<&AnnotationToken> = None;
    let mut pending_empty_lines = 0usize;
    let mut started = false;

    for tokens in line_tokens {
        if tokens.is_empty() {
            if started {
                pending_empty_lines += 1;
            } else {
                lines.push(blank.to_owned());
            }
            continue;
        }
        for (index, token) in tokens.iter().enumerate() {
            let closer = is_closer(token.kind);
            let closes_expanded = closer
                && open_delimiters
                    .last()
                    .is_none_or(|delimiter| delimiter.expanded);
            let break_before = started
                && if index == 0 {
                    pending_empty_lines > 0 || !closer || closes_expanded
                } else {
                    closes_expanded
                };
            if break_before {
                lines.push(annotation_line(prefix, current_level, &current, indent));
                current.clear();
                lines.extend(repeat_n(blank.to_owned(), pending_empty_lines));
                current_level = match open_delimiters.last() {
                    Some(delimiter) if closer => delimiter.opener_level,
                    Some(delimiter) => delimiter.opener_level + usize::from(delimiter.expanded),
                    None => hanging,
                };
                previous = None;
            }
            pending_empty_lines = 0;

            if let Some(previous) = previous
                && annotation_space_between(previous, token.kind)
            {
                current.push(' ');
            }
            current.push_str(&token.text);
            previous = Some(token);
            started = true;

            if is_opener(token.kind) {
                open_delimiters.push(OpenDelimiter {
                    expanded: index + 1 == tokens.len(),
                    opener_level: current_level,
                });
            } else if closer {
                open_delimiters.pop();
            }
        }
    }
    if started {
        lines.push(annotation_line(prefix, current_level, &current, indent));
    }
    lines
}

fn annotation_line(prefix: &str, level: usize, content: &str, indent: &str) -> String {
    format!("{prefix}{}{content}", indent.repeat(level))
}

struct OpenDelimiter {
    expanded: bool,
    opener_level: usize,
}

struct AnnotationToken {
    kind: AnnotationTokenKind,
    text: String,
    end: TextSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotationTokenKind {
    Word,
    Directive,
    /// The `=` of an inline type definition.
    Equals,
    Dots,
    Arrow,
    Pipe,
    Comma,
    Colon,
    /// The `:` that starts a tag type, `:Some(integer)`.
    TagMarker,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    OpenAngle,
    CloseAngle,
}

fn annotation_token_kind(token: &SyntaxToken) -> Option<AnnotationTokenKind> {
    use AnnotationTokenKind::*;
    let kind = token.kind();
    Some(match kind {
        COLON
            if token
                .parent()
                .is_some_and(|parent| parent.kind() == TYPE_TAG) =>
        {
            TagMarker
        }
        L_PAREN => OpenParen,
        R_PAREN => CloseParen,
        L_BRACKET => OpenBracket,
        R_BRACKET => CloseBracket,
        L_BRACE => OpenBrace,
        R_BRACE => CloseBrace,
        LESS => OpenAngle,
        GREATER => CloseAngle,
        COMMA => Comma,
        COLON => Colon,
        PIPE => Pipe,
        EQ => Equals,
        MINUS_GREATER => Arrow,
        DOTS => Dots,
        AT => Directive,
        // `-` is interior to a joined directive name (`@if-unknown`).
        IDENT | DOTDOTI | MINUS => Word,
        _ if kind.is_keyword() => Word,
        _ => return None,
    })
}

fn annotation_space_between(previous: &AnnotationToken, current: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    match current {
        Comma | Colon | CloseParen | CloseBracket | CloseBrace | CloseAngle => return false,
        // `(` hugs only what it applies to (`fn(`); after `:`, `|` or `->` it
        // opens a grouped type: `x: (integer | character)`.
        OpenParen
            if matches!(
                previous.kind,
                Word | Dots | CloseParen | CloseBracket | CloseBrace | CloseAngle
            ) =>
        {
            return false;
        }
        OpenAngle if previous.kind == Word => return false,
        OpenBracket if matches!(previous.kind, Word | CloseAngle | CloseBracket) => return false,
        OpenBrace
            if previous.kind == Word
                && matches!(
                    previous.text.as_str(),
                    "list"
                        | "Record"
                        | "Tuple"
                        | "DataFrame"
                        | "LazyFrame"
                        | "GroupBy"
                        | "LazyGroupBy"
                        | "data.table"
                ) =>
        {
            return false;
        }
        Word if matches!(previous.kind, Dots | TagMarker) => return false,
        _ => {}
    }
    !is_opener(previous.kind)
}

fn is_opener(kind: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    matches!(kind, OpenParen | OpenBracket | OpenBrace | OpenAngle)
}

fn is_closer(kind: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    matches!(kind, CloseParen | CloseBracket | CloseBrace | CloseAngle)
}

/// Spellings, not just kinds: a wrong source range renames identifiers while
/// keeping every kind. Braces, `;` and `#:` markers may be added or moved, and
/// non-raw strings compare by content because the formatter picks the quote.
fn significant_tokens(text: &str) -> Vec<(SyntaxKind, &str)> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    for token in lex(text).0 {
        let end = (start + usize::from(token.len)).min(text.len());
        let spelling = text.get(start..end).unwrap_or_default();
        start = end;
        if token.kind.is_trivia()
            || matches!(
                token.kind,
                L_BRACE | R_BRACE | SEMICOLON | ANNOTATION_MARKER
            )
        {
            continue;
        }
        tokens.push((token.kind, comparable_spelling(token.kind, spelling)));
    }
    tokens
}

fn comparable_spelling(kind: SyntaxKind, spelling: &str) -> &str {
    if kind != STRING || matches!(spelling.chars().next(), Some('r' | 'R')) {
        return spelling;
    }
    // An unterminated string has no closer to trim.
    let inner = spelling.strip_prefix(['"', '\'']).unwrap_or(spelling);
    if spelling.len() >= 2 {
        inner.strip_suffix(['"', '\'']).unwrap_or(inner)
    } else {
        inner
    }
}
