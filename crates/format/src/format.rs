//! The ry formatter: a preserving formatter over the lossless syntax
//! tree.
//!
//! The user's layout intent survives — single-line constructs stay single
//! line, multiline constructs keep their line structure ("hug" detection
//! reads the intent off the original bracket placement) — while spacing,
//! indentation, quoting, comment shape, and `#:` annotation blocks
//! normalize. `# fmt: skip` exempts one node byte-exactly, `# fmt: off` /
//! `# fmt: on` toggle whole regions, and `# fmt: skip-file` at the head
//! leaves the file untouched. A file with syntax errors refuses to format
//! (errors inside `#:` annotations do not refuse the file — the affected
//! block is preserved verbatim instead).

use syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineEnding {
    Auto,
    Lf,
    CrLf,
}

/// A syntax error refusing the format, at a zero-based line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Syntax error: {} at line {}, column {}",
            self.message,
            self.line + 1,
            self.column + 1
        )
    }
}

impl std::error::Error for FormatError {}

pub fn format(source: &str, config: Config) -> Result<String, FormatError> {
    let parse = syntax::parse(source);
    let root = parse.syntax_node();
    let line_starts = line_starts(source);

    // Errors from the `#:` annotation grammar do not refuse the file: the
    // affected block goes down the verbatim path (the oracle formatter never
    // even parsed annotations). Any R-grammar syntax error refuses.
    for error in parse.errors() {
        if !error.in_annotation {
            let offset = u32::from(error.range.start()) as usize;
            let line = line_of(&line_starts, offset);
            return Err(FormatError {
                message: error.message.clone(),
                line,
                column: offset - line_starts[line],
            });
        }
    }

    // A `#:` region opened mid-line with the statement CONTINUING past it
    // (`f <-#: T ...` — the region swallows the rest of the line while the
    // expression resumes below) has no meaningful layout, so line-based join
    // decisions cannot be stable. Refuse it like an R-grammar error. A
    // statement's leading annotation and a TRAILING annotation after a
    // complete statement (nothing significant follows within the statement)
    // are unaffected.
    for node in root.descendants() {
        if node.kind() != syntax::SyntaxKind::ANNOTATION {
            continue;
        }
        let offset = u32::from(node.text_range().start()) as usize;
        let line = line_of(&line_starts, offset);
        let mid_line = source[line_starts[line]..offset]
            .chars()
            .any(|c| !c.is_whitespace());
        if !mid_line {
            continue;
        }
        // The statement within the nearest sequence: climb until the parent
        // is a braced block or the file root (blocks are expressions in R,
        // so expression-kind alone cannot be the boundary).
        let statement = |mut candidate: syntax::SyntaxNode| {
            while let Some(parent) = candidate.parent() {
                if parent.kind() == syntax::SyntaxKind::BRACE_EXPR || parent.parent().is_none() {
                    break;
                }
                candidate = parent;
            }
            candidate
        };
        let continues = node
            .last_token()
            .and_then(|token| token.next_token())
            .into_iter()
            .flat_map(|token| std::iter::successors(Some(token), |token| token.next_token()))
            .find(|token| {
                !matches!(
                    token.kind(),
                    syntax::SyntaxKind::WHITESPACE | syntax::SyntaxKind::NEWLINE
                )
            })
            .and_then(|token| token.parent())
            .is_some_and(|parent| statement(parent) == statement(node.clone()));
        if continues {
            return Err(FormatError {
                message:
                    "a `#:` annotation cannot interrupt an expression. Move it to its own line"
                        .to_owned(),
                line,
                column: offset - line_starts[line],
            });
        }
    }

    let line_ending = match config.line_ending {
        LineEnding::Auto => {
            if source
                .find('\n')
                .is_some_and(|at| source[..at].ends_with('\r'))
            {
                "\r\n"
            } else {
                "\n"
            }
        }
        LineEnding::Lf => "\n",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Directive {
    Skip,
    SkipFile,
    On,
    Off,
}

/// A node or token child, with trivia (whitespace and newlines) dropped —
/// the walk re-derives all layout. Comments, annotations, and semicolons
/// stay: containers place them.
type Element = SyntaxElement;

struct Formatter<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    /// Ranges of annotation-grammar errors: the blocks containing them render
    /// verbatim (R-grammar errors refuse the whole file before this point).
    error_ranges: Vec<TextRange>,
    indent: String,
    line_ending: &'static str,
    out: String,
    /// Output length right after the last emitted comment: anything written
    /// on the same line after a comment would become comment text, so the
    /// element walk forces a line break first (see `element`).
    comment_end: Option<usize>,
}

impl Formatter<'_> {
    // ---- layout primitives ----

    fn space(&mut self) {
        self.out.push(' ');
    }

    fn newline(&mut self, level: usize) {
        self.out.push_str(self.line_ending);
        for _ in 0..level {
            self.out.push_str(&self.indent);
        }
    }

    /// Between-sibling separation preserving the author's blank line (at
    /// most one): 1 or 2 line endings, then indentation.
    fn newlines_between(&mut self, previous: Option<&Element>, element: &Element, level: usize) {
        let count = previous.map_or(0, |previous| {
            let gap = self
                .line(self.significant_range(element).start())
                .saturating_sub(self.line(self.significant_range(previous).end()));
            gap.clamp(1, 2)
        });
        for _ in 0..count {
            self.out.push_str(self.line_ending);
        }
        for _ in 0..level {
            self.out.push_str(&self.indent);
        }
    }

    fn line(&self, offset: syntax::TextSize) -> usize {
        line_of(&self.line_starts, u32::from(offset) as usize)
    }

    fn same_line(&self, left: TextRange, right: TextRange) -> bool {
        self.line(left.end()) == self.line(right.start())
    }

    /// An element's range trimmed to its significant tokens: some nodes
    /// swallow trailing trivia (an argument keeps the newline after it), and
    /// line math must never see it.
    fn significant_range(&self, element: &Element) -> TextRange {
        match element {
            SyntaxElement::Token(token) => token.text_range(),
            SyntaxElement::Node(node) => {
                let full = node.text_range();
                let start = self
                    .first_token_after(node, full.start())
                    .map_or(full.start(), |token| token.text_range().start());
                let end = self
                    .last_token_before(node, full.end())
                    .map_or(full.end(), |token| token.text_range().end());
                if start <= end {
                    TextRange::new(start, end)
                } else {
                    full
                }
            }
        }
    }

    fn raw(&mut self, range: TextRange) {
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        self.out.push_str(&self.source[start..end]);
    }

    // ---- shared walk helpers ----

    /// The last significant (non-whitespace, non-newline) descendant token
    /// ending at or before `offset`. Comments attach deep inside expression
    /// nodes, so closer-placement decisions must look at tokens, not
    /// siblings.
    fn last_token_before(
        &self,
        node: &SyntaxNode,
        offset: syntax::TextSize,
    ) -> Option<SyntaxToken> {
        let mut last = None;
        for element in node.descendants_with_tokens() {
            if let Some(token) = element.as_token() {
                if token.text_range().end() > offset {
                    break;
                }
                if !matches!(token.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) {
                    last = Some(token.clone());
                }
            }
        }
        last
    }

    /// Whether the last significant token before `element` (inside `node`) is
    /// a comment or part of a `#:` annotation. Element-level `previous` checks
    /// miss comments swallowed into the previous sibling's subtree, and
    /// anything glued after such a token would become comment text.
    fn after_comment(&self, node: &SyntaxNode, element: &Element) -> bool {
        self.last_token_before(node, element.text_range().start())
            .is_some_and(|token| {
                token.kind() == SyntaxKind::COMMENT
                    || token
                        .parent_ancestors()
                        .any(|ancestor| ancestor.kind() == SyntaxKind::ANNOTATION)
            })
    }

    /// The first significant descendant token starting at or after `offset`.
    fn first_token_after(
        &self,
        node: &SyntaxNode,
        offset: syntax::TextSize,
    ) -> Option<SyntaxToken> {
        node.descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| {
                token.text_range().start() >= offset
                    && !matches!(token.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE)
            })
    }

    /// The non-trivia children (nodes, plus comment/semicolon/operator/
    /// punctuation tokens) in order.
    fn elements(node: &SyntaxNode) -> Vec<Element> {
        node.children_with_tokens()
            .filter(|element| {
                !matches!(element.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE)
            })
            .collect()
    }

    fn is_comment(element: Option<&Element>) -> bool {
        element.is_some_and(|element| {
            matches!(element.kind(), SyntaxKind::COMMENT | SyntaxKind::ANNOTATION)
        })
    }

    /// Loop bodies are always brace-wrapped onto their own lines, so a
    /// `for`/`while`/`repeat` with a non-empty body renders multiline even
    /// when written on one line. Single-line layout choices must consult
    /// this: input geometry alone would pick a layout the rendered body
    /// immediately breaks, and a second pass would then format differently.
    fn forces_multiline(node: &SyntaxNode) -> bool {
        node.descendants().any(|descendant| {
            if !matches!(
                descendant.kind(),
                SyntaxKind::FOR_EXPR | SyntaxKind::WHILE_EXPR | SyntaxKind::REPEAT_EXPR
            ) {
                return false;
            }
            descendant.children().last().is_some_and(|body| {
                body.kind() != SyntaxKind::BRACE_EXPR
                    || body.children_with_tokens().any(|child| {
                        !matches!(
                            child.kind(),
                            SyntaxKind::L_BRACE
                                | SyntaxKind::R_BRACE
                                | SyntaxKind::WHITESPACE
                                | SyntaxKind::NEWLINE
                                | SyntaxKind::SEMICOLON
                        )
                    })
            })
        })
    }

    fn comment_text(&self, element: &Element) -> &str {
        let range = element.text_range();
        &self.source[u32::from(range.start()) as usize..u32::from(range.end()) as usize]
    }

    fn directive(&self, element: &Element) -> Option<Directive> {
        if element.kind() != SyntaxKind::COMMENT {
            return None;
        }
        parse_directive(self.comment_text(element))
    }

    fn previous_sibling(element: &Element) -> Option<Element> {
        match element {
            SyntaxElement::Node(node) => node.prev_sibling_or_token(),
            SyntaxElement::Token(token) => token.prev_sibling_or_token(),
        }
    }

    fn next_sibling(element: &Element) -> Option<Element> {
        match element {
            SyntaxElement::Node(node) => node.next_sibling_or_token(),
            SyntaxElement::Token(token) => token.next_sibling_or_token(),
        }
    }

    /// Whether a `# fmt: skip` comment exempts this node: a skip comment
    /// alone on the line directly above, or one trailing on the node's last
    /// line. Trailing comments usually attach as the node's own last token
    /// in this tree, so both the internal and the following-sibling
    /// attachment count.
    fn skip_exempt(&self, node: &SyntaxNode) -> bool {
        let is_skip = |token: &SyntaxToken| {
            token.kind() == SyntaxKind::COMMENT
                && parse_directive(token.text()) == Some(Directive::Skip)
        };

        let mut previous = node.prev_sibling_or_token();
        while let Some(current) = &previous {
            if !matches!(current.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) {
                break;
            }
            previous = Self::previous_sibling(current);
        }
        if let Some(SyntaxElement::Token(token)) = &previous
            && is_skip(token)
        {
            let mut before = token.prev_sibling_or_token();
            while let Some(current) = &before {
                if !matches!(current.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) {
                    break;
                }
                before = Self::previous_sibling(current);
            }
            let alone = before
                .is_none_or(|before| !self.same_line(before.text_range(), token.text_range()));
            if alone {
                return true;
            }
        }

        let mut next = node.next_sibling_or_token();
        while let Some(current) = &next {
            if current.kind() != SyntaxKind::WHITESPACE {
                break;
            }
            next = Self::next_sibling(current);
        }
        if let Some(SyntaxElement::Token(token)) = &next
            && is_skip(token)
        {
            return true;
        }

        if let Some(token) = self.last_token_before(node, node.text_range().end())
            && is_skip(&token)
        {
            return self
                .last_token_before(node, token.text_range().start())
                .is_some_and(|code| {
                    self.line(code.text_range().end()) == self.line(token.text_range().start())
                });
        }
        false
    }

    /// `# fmt: skip` preserves the node byte-exactly, including the
    /// column of its first line: when only whitespace precedes it on its
    /// line, the indentation just emitted is replaced by the original
    /// leading whitespace, so column-aligned constructs survive.
    fn emit_skipped(&mut self, node: &SyntaxNode) {
        let range = self.significant_range(&SyntaxElement::Node(node.clone()));
        let start = u32::from(range.start()) as usize;
        let line_start = self.line_starts[line_of(&self.line_starts, start)];
        let leading = &self.source[line_start..start];
        if leading.chars().all(char::is_whitespace) {
            let content_length = self.out.trim_end_matches(' ').len();
            if content_length == 0 || self.out[..content_length].ends_with(self.line_ending) {
                self.out.truncate(content_length);
                self.out.push_str(leading);
            }
        }
        self.raw(range);
    }

    fn element(&mut self, element: &Element, level: usize, make_multiline: bool) {
        // Nothing may follow a comment on its line — it would become comment
        // text. Walks normally break the line themselves; this guard covers
        // the joins that do not know a comment interposed (an operand
        // continuing an operator across a commented line break).
        if let Some(comment_end) = self.comment_end.take()
            && element.kind() != SyntaxKind::COMMENT
            && !self.out[comment_end..].contains('\n')
        {
            self.newline(level);
        }
        match element {
            SyntaxElement::Node(node) => self.node(node, level, make_multiline),
            SyntaxElement::Token(token) => self.token(token, level),
        }
    }

    fn token(&mut self, token: &SyntaxToken, level: usize) {
        match token.kind() {
            SyntaxKind::COMMENT => self.comment(token),
            SyntaxKind::STRING => self.string(token),
            _ => self.raw(token.text_range()),
        }
        let _ = level;
    }

    fn node(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        if self.skip_exempt(node) {
            self.emit_skipped(node);
            return;
        }
        match node.kind() {
            SyntaxKind::LITERAL | SyntaxKind::NAME => {
                for element in Self::elements(node) {
                    self.element(&element, level, false);
                }
            }
            SyntaxKind::BINARY_EXPR => self.binary(node, level),
            SyntaxKind::UNARY_EXPR => self.unary(node, level),
            SyntaxKind::PAREN_EXPR => self.paren(node, level),
            SyntaxKind::BRACE_EXPR => self.braces(node, level, make_multiline),
            SyntaxKind::CALL_EXPR | SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                self.call_like(node, level)
            }
            SyntaxKind::ARGUMENT_LIST => self.argument_list(node, level, false),
            SyntaxKind::PARAMETER_LIST => self.argument_list(node, level, false),
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => self.field_access(node, level),
            SyntaxKind::NAMESPACE_EXPR => {
                for element in Self::elements(node) {
                    self.element(&element, level, false);
                }
            }
            SyntaxKind::FUNCTION_DEF => self.function_definition(node, level),
            SyntaxKind::IF_EXPR => self.if_expression(node, level, make_multiline),
            SyntaxKind::FOR_EXPR => self.for_expression(node, level),
            SyntaxKind::WHILE_EXPR => self.while_expression(node, level),
            SyntaxKind::REPEAT_EXPR => self.repeat_expression(node, level),
            SyntaxKind::BREAK_EXPR | SyntaxKind::NEXT_EXPR => {
                for element in Self::elements(node) {
                    self.element(&element, level, false);
                }
            }
            SyntaxKind::ANNOTATION => {
                self.annotation(node, level);
                // Like a comment, nothing may follow an annotation on its
                // line — it would become annotation text.
                self.comment_end = Some(self.out.len());
            }
            _ => {
                // Every other node (argument, parameter, type nodes reached
                // outside an annotation) prints its children with canonical
                // single spacing decisions made by the caller.
                for element in Self::elements(node) {
                    self.element(&element, level, false);
                }
            }
        }
    }

    // ---- the top level ----

    fn source_file(&mut self, root: &SyntaxNode) {
        let elements = Self::elements(root);

        if elements
            .first()
            .is_some_and(|first| self.directive(first) == Some(Directive::SkipFile))
        {
            self.out.push_str(self.source);
            return;
        }

        self.item_sequence(&elements, 0);

        // An empty file stays empty; anything else ends with one newline.
        if !elements.is_empty() || !self.source.is_empty() {
            self.newline(0);
        }
    }

    /// A sequence of statements at one level: the top level, or a multiline
    /// brace body. Handles `# fmt: off` regions, `# fmt: skip`, same-line
    /// trailing comments, and semicolon-separated statements (normalized to
    /// newlines).
    fn item_sequence(&mut self, elements: &[Element], level: usize) {
        let mut enabled = true;
        let mut pending_directive = None;
        let mut previous: Option<&Element> = None;
        let mut first = true;

        for element in elements {
            match pending_directive {
                Some(Directive::On) => enabled = true,
                Some(Directive::Off) => enabled = false,
                _ => {}
            }
            pending_directive = self.directive(element);

            if element.kind() == SyntaxKind::SEMICOLON {
                continue;
            }

            if !enabled {
                // A `# fmt: off` region is preserved byte-exactly; only the
                // directive comments themselves are formatted.
                let start = previous.map_or_else(
                    || u32::from(element.text_range().start()) as usize,
                    |previous| u32::from(previous.text_range().end()) as usize,
                );
                let end = u32::from(element.text_range().end()) as usize;
                if pending_directive.is_some() {
                    let element_start = u32::from(element.text_range().start()) as usize;
                    self.out.push_str(&self.source[start..element_start]);
                    self.element(element, level, false);
                } else {
                    self.out.push_str(&self.source[start..end]);
                }
                previous = Some(element);
                first = false;
                continue;
            }

            let trailing_comment =
                matches!(element.kind(), SyntaxKind::COMMENT | SyntaxKind::ANNOTATION)
                    && previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    });
            if trailing_comment {
                self.space();
            } else if first {
                for _ in 0..level {
                    self.out.push_str(&self.indent);
                }
            } else {
                self.newlines_between(previous, element, level);
            }

            self.element(element, level, false);
            previous = Some(element);
            first = false;
        }
    }

    // ---- comments and strings ----

    fn comment(&mut self, token: &SyntaxToken) {
        self.comment_end = Some(self.out.len());
        let raw = self
            .comment_text(&SyntaxElement::Token(token.clone()))
            .trim_end()
            .to_owned();
        let mut chars = raw.chars();
        let _ = chars.next();
        match chars.next() {
            Some(char @ ('\'' | '*')) => match chars.next() {
                Some(' ') | None => self.out.push_str(&raw),
                Some(_) if char == '\'' && chars.clone().any(|c| c == '\'') => {
                    self.out.push_str(&raw)
                }
                Some(other) => {
                    self.out.push('#');
                    self.out.push(char);
                    self.out.push(' ');
                    self.out.push(other);
                    self.out.extend(chars);
                }
            },
            // `#!` shebangs, `#|` Quarto/knitr cell options, `##`, and
            // already-spaced comments stay untouched.
            Some('#' | '!' | '|' | ' ') | None => self.out.push_str(&raw),
            Some(other) => {
                self.out.push_str("# ");
                self.out.push(other);
                self.out.extend(chars);
            }
        }
    }

    fn string(&mut self, token: &SyntaxToken) {
        let text = {
            let range = token.text_range();
            &self.source[u32::from(range.start()) as usize..u32::from(range.end()) as usize]
        };
        // Raw strings are preserved byte-for-byte: re-quoting would corrupt
        // embedded quotes and backslashes.
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

    // ---- operators ----

    fn binary(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let operator = elements.iter().position(|element| {
            element
                .as_token()
                .is_some_and(|token| is_operator(token.kind()))
        });
        let Some(operator_index) = operator else {
            for element in &elements {
                self.element(element, level, false);
            }
            return;
        };
        let operator_kind = elements[operator_index].kind();
        let has_spacing = !matches!(operator_kind, SyntaxKind::COLON | SyntaxKind::CARET);
        let break_after_operator = elements
            .get(operator_index + 1..)
            .and_then(|rest| rest.iter().find(|element| element.as_node().is_some()))
            .is_some_and(|rhs| {
                !self.same_line(elements[operator_index].text_range(), rhs.text_range())
            });

        let mut seen_operator = false;
        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    let trailing = elements
                        .iter()
                        .skip_while(|other| other.text_range() != element.text_range())
                        .skip(1)
                        .all(|rest| {
                            matches!(rest.kind(), SyntaxKind::COMMENT | SyntaxKind::ANNOTATION)
                        });
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                        self.element(element, level, false);
                    } else if trailing {
                        // An own-line comment after the last operand belongs
                        // to the construct around this expression, at its
                        // level — not to the continuation.
                        self.newline(level);
                        self.element(element, level, false);
                    } else {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    }
                }
                kind if element.as_token().is_some() && is_operator(kind) => {
                    // Token-level: the comment may sit as the previous
                    // operand's own last token, and gluing the operator after
                    // it would comment the operator out.
                    if self.after_comment(node, element) {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        if has_spacing {
                            self.space();
                        }
                        self.element(element, level, false);
                    }
                    seen_operator = true;
                }
                _ => {
                    if !seen_operator {
                        self.element(element, level, false);
                    } else if break_after_operator || self.after_comment(node, element) {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        if has_spacing {
                            self.space();
                        }
                        self.element(element, level, false);
                    }
                }
            }
            previous = Some(element);
        }
    }

    fn unary(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        // `...` also parses as a NAME node; the tilde hugs identifiers only
        // (`~x` but `~ ...`), so look at the wrapped token.
        let operand_is_name = elements
            .iter()
            .rev()
            .find(|element| element.as_node().is_some())
            .is_some_and(|operand| {
                operand.kind() == SyntaxKind::NAME
                    && operand.as_node().is_some_and(|name| {
                        name.children_with_tokens()
                            .filter_map(|child| child.into_token())
                            .any(|token| token.kind() == SyntaxKind::IDENT)
                    })
            });
        let has_space = elements
            .first()
            .is_some_and(|operator| operator.kind() == SyntaxKind::TILDE)
            && !operand_is_name;

        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                        self.element(element, level, false);
                    } else {
                        self.newlines_between(previous, element, level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    }
                }
                _ if element.as_node().is_some() => {
                    if Self::is_comment(previous) {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        if previous.is_some() && has_space {
                            self.space();
                        }
                        self.element(element, level, false);
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn field_access(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let operator = elements
            .iter()
            .position(|element| matches!(element.kind(), SyntaxKind::DOLLAR | SyntaxKind::AT));
        // The multiline probe must skip comments trailing the operator: the
        // field name is the element whose line placement matters.
        let is_multiline = operator.is_some_and(|operator_index| {
            elements
                .get(operator_index + 1..)
                .and_then(|rest| {
                    rest.iter()
                        .find(|element| element.kind() != SyntaxKind::COMMENT)
                })
                .is_some_and(|rhs| {
                    !self.same_line(
                        elements[..operator_index]
                            .iter()
                            .next_back()
                            .map_or(elements[operator_index].text_range(), |lhs| {
                                lhs.text_range()
                            }),
                        rhs.text_range(),
                    )
                })
        });

        let mut seen_operator = false;
        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::DOLLAR | SyntaxKind::AT => {
                    self.element(element, level, false);
                    seen_operator = true;
                }
                _ => {
                    if seen_operator && is_multiline {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        self.element(element, level, false);
                    }
                }
            }
            previous = Some(element);
        }
    }

    // ---- groups ----

    fn paren(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let close = elements
            .iter()
            .rposition(|element| element.kind() == SyntaxKind::R_PAREN);
        let hug = close.is_none_or(|close_index| {
            let close_start = elements[close_index].text_range().start();
            let comment_before_close =
                self.last_token_before(node, close_start)
                    .is_some_and(|token| {
                        token.kind() == SyntaxKind::COMMENT
                            || token
                                .parent_ancestors()
                                .any(|ancestor| ancestor.kind() == SyntaxKind::ANNOTATION)
                    });
            close_index
                .checked_sub(1)
                .map(|body| &elements[body])
                .is_none_or(|body| {
                    !comment_before_close
                        && self.line(body.text_range().start())
                            == self.line(node.text_range().start())
                })
        });

        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::L_PAREN => self.element(element, level, false),
                SyntaxKind::R_PAREN => {
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                    }
                    self.element(element, level, false);
                }
                _ => {
                    if !hug {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        self.element(element, level, false);
                    }
                }
            }
            previous = Some(element);
        }
    }

    fn braces(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        let elements = Self::elements(node);
        let close = elements
            .iter()
            .rposition(|element| element.kind() == SyntaxKind::R_BRACE);
        let hug = close.is_none_or(|close_index| {
            let close_start = elements[close_index].text_range().start();
            let comment_before_close =
                self.last_token_before(node, close_start)
                    .is_some_and(|token| {
                        token.kind() == SyntaxKind::COMMENT
                            || token
                                .parent_ancestors()
                                .any(|ancestor| ancestor.kind() == SyntaxKind::ANNOTATION)
                    });
            close_index
                .checked_sub(1)
                .map(|body| &elements[body])
                .is_none_or(|body| {
                    !comment_before_close
                        && self.line(body.text_range().start())
                            == self.line(node.text_range().start())
                        && self.line(body.text_range().end())
                            == self.line(node.text_range().start())
                })
        });
        let is_multiline = !hug || make_multiline || Self::forces_multiline(node);
        let body_elements: Vec<Element> = elements
            .iter()
            .filter(|element| !matches!(element.kind(), SyntaxKind::L_BRACE | SyntaxKind::R_BRACE))
            .cloned()
            .collect();
        // Stray semicolons render nothing on their own, so a body of only
        // semicolons is an empty block — emitting its (dropped) lines would
        // leave a bare blank interior line that a second pass collapses,
        // breaking idempotence.
        let body_elements: Vec<Element> = if body_elements
            .iter()
            .all(|element| element.kind() == SyntaxKind::SEMICOLON)
        {
            Vec::new()
        } else {
            body_elements
        };
        let is_empty = body_elements.is_empty();

        self.out.push('{');
        if is_multiline {
            // Comments trailing the `{` on its own line stay inline.
            let open_line = self.line(node.text_range().start());
            let mut body_start = 0;
            while body_start < body_elements.len()
                && body_elements[body_start].kind() == SyntaxKind::COMMENT
                && self.line(body_elements[body_start].text_range().start()) == open_line
            {
                self.space();
                let element = body_elements[body_start].clone();
                self.element(&element, level, false);
                body_start += 1;
            }
            let rest = &body_elements[body_start..];
            if !rest.is_empty() {
                self.out.push_str(self.line_ending);
                self.item_sequence(rest, level + 1);
                self.newline(level);
            } else if !is_empty {
                self.newline(level);
            }
            self.out.push('}');
        } else {
            let mut first = true;
            let mut previous: Option<&Element> = None;
            for element in &body_elements {
                if element.kind() == SyntaxKind::SEMICOLON {
                    previous = Some(element);
                    continue;
                }
                if element.kind() == SyntaxKind::COMMENT {
                    self.space();
                    self.element(element, level, false);
                    previous = Some(element);
                    continue;
                }
                if first {
                    self.space();
                } else {
                    self.out.push_str("; ");
                }
                self.element(element, level, false);
                first = false;
                previous = Some(element);
            }
            let _ = previous;
            if !is_empty {
                self.space();
            }
            self.out.push('}');
        }
    }

    // ---- calls and argument lists ----

    fn call_like(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let additional_indent = elements.first().is_some_and(|target| {
            matches!(target.kind(), SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR)
                && target.as_node().is_some_and(|target| {
                    let parts = Self::elements(target);
                    let operator = parts.iter().position(|part| {
                        matches!(part.kind(), SyntaxKind::DOLLAR | SyntaxKind::AT)
                    });
                    operator.is_some_and(|operator_index| {
                        parts.get(operator_index + 1).is_some_and(|rhs| {
                            parts.first().is_some_and(|lhs| {
                                !self.same_line(lhs.text_range(), rhs.text_range())
                            })
                        })
                    })
                })
        });

        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                        self.element(element, level, false);
                    } else {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    }
                }
                SyntaxKind::ARGUMENT_LIST => {
                    if additional_indent {
                        self.argument_list(
                            element.as_node().expect("argument list is a node"),
                            level + 1,
                            true,
                        );
                    } else {
                        self.argument_list(
                            element.as_node().expect("argument list is a node"),
                            level,
                            false,
                        );
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn argument_list(&mut self, node: &SyntaxNode, level: usize, _skip_first_indent: bool) {
        let elements = Self::elements(node);
        // `]]` closes with two separate `]` tokens: the FIRST is the
        // placement anchor, the second glues after it.
        let close = elements.iter().position(|element| {
            matches!(element.kind(), SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET)
        });
        let list_range = node.text_range();
        let is_multiline = self.line(list_range.start()) != self.line(list_range.end());
        let arguments: Vec<&Element> = elements
            .iter()
            .filter(|element| {
                element.kind() == SyntaxKind::ARGUMENT || element.kind() == SyntaxKind::PARAMETER
            })
            .collect();
        let is_empty = arguments
            .iter()
            .all(|argument| argument.text_range().is_empty());

        // Closer placement looks at the last significant token before the
        // closer (comments attach deep inside argument expressions, and
        // zero-width hole arguments are invisible).
        let close_previous_token = close.and_then(|close_index| {
            self.last_token_before(node, elements[close_index].text_range().start())
        });
        let trailing_space = close_previous_token
            .as_ref()
            .is_none_or(|token| token.kind() == SyntaxKind::COMMA);
        // Hugged layout: some argument begins on the open line and ends on
        // the closer's line, chaining the lines together. Parameter lists
        // never hug — a multiline signature always expands one per line.
        let hug = node.kind() != SyntaxKind::PARAMETER_LIST
            && close_previous_token.as_ref().is_none_or(|close_previous| {
                close_previous.kind() != SyntaxKind::COMMENT
                    && arguments.iter().any(|argument| {
                        let range = self.significant_range(argument);
                        !range.is_empty()
                            && self.line(range.start()) == self.line(list_range.start())
                            && self.line(range.end())
                                == self.line(close_previous.text_range().end())
                    })
            });

        let mut argument_index = 0usize;
        let mut seen_close = false;
        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET | SyntaxKind::L_BRACKET2 => {
                    self.element(element, level, false);
                }
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET => {
                    if !seen_close {
                        if is_multiline && !hug && !is_empty {
                            self.newline(level);
                        } else if trailing_space {
                            // Also with only empty arguments: `alist(, )`
                            // keeps the space after its comma. This applies
                            // whenever the closer shares its line with the
                            // comma — including a multiline list that
                            // collapsed (hugged or all-empty), which must
                            // match what its single-line output reformats to.
                            self.space();
                        }
                        seen_close = true;
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                        self.element(element, level, false);
                    } else {
                        self.newlines_between(previous, element, level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    }
                }
                SyntaxKind::COMMA => {
                    if is_multiline
                        && !hug
                        && (self.after_comment(node, element)
                            || previous.is_none_or(|previous| {
                                previous.kind() == SyntaxKind::COMMA
                                    || previous.text_range().is_empty()
                            }))
                    {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                    } else if self
                        .last_token_before(node, element.text_range().start())
                        .is_some_and(|token| token.kind() == SyntaxKind::EQ)
                    {
                        // A fall-through argument (`z = ,`) keeps a space
                        // between its `=` and the comma.
                        self.space();
                    }
                    self.out.push(',');
                }
                SyntaxKind::ARGUMENT | SyntaxKind::PARAMETER => {
                    let empty = element.text_range().is_empty();
                    if !empty {
                        if argument_index == 0 {
                            if is_multiline && !hug {
                                self.newline(level);
                                self.out.push_str(&self.indent);
                            }
                        } else if is_multiline && !hug {
                            self.newlines_between(previous, element, level);
                            self.out.push_str(&self.indent);
                        } else {
                            self.space();
                        }
                        let argument_level = if is_multiline && !hug {
                            level + 1
                        } else {
                            level
                        };
                        self.argument(
                            element.as_node().expect("argument is a node"),
                            argument_level,
                        );
                    }
                    argument_index += 1;
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn argument(&mut self, node: &SyntaxNode, level: usize) {
        if self.skip_exempt(node) {
            self.emit_skipped(node);
            return;
        }
        let elements = Self::elements(node);
        let mut previous: Option<&Element> = None;
        for element in &elements {
            match element.kind() {
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::EQ => self.out.push_str(" ="),
                _ => {
                    if Self::is_comment(previous) {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        if previous.is_some_and(|previous| previous.kind() == SyntaxKind::EQ) {
                            self.space();
                        }
                        self.element(element, level, false);
                    }
                }
            }
            previous = Some(element);
        }
    }

    // ---- definitions and control flow ----

    fn function_definition(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        // Trailing trivia swallowed into the node must not force multiline.
        let range = self.significant_range(&SyntaxElement::Node(node.clone()));
        let is_multiline = self.line(range.start()) != self.line(range.end());
        let head = elements.first();
        let body = elements.iter().rev().find(|element| {
            element.as_node().is_some() && element.kind() != SyntaxKind::PARAMETER_LIST
        });
        let is_same_line = match (head, body) {
            (Some(head), Some(body)) => {
                self.line(head.text_range().start()) == self.line(body.text_range().start())
            }
            _ => true,
        };
        let body_range = body.map(|body| body.text_range());

        let mut previous: Option<&Element> = None;
        for element in &elements {
            let is_body = body_range == Some(element.text_range()) && element.as_node().is_some();
            match element.kind() {
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::PARAMETER_LIST => {
                    if Self::is_comment(previous) {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                _ if is_body => {
                    if Self::is_comment(previous) {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    if is_multiline {
                        if element.kind() == SyntaxKind::BRACE_EXPR || is_same_line {
                            self.element(element, level, true);
                        } else {
                            self.wrap_braces(element, level);
                        }
                    } else {
                        self.element(element, level, false);
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn wrap_braces(&mut self, element: &Element, level: usize) {
        self.out.push('{');
        self.newline(level + 1);
        self.element(element, level + 1, false);
        self.newline(level);
        self.out.push('}');
    }

    fn if_expression(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        let elements = Self::elements(node);
        // The node may swallow trailing trivia (an if as the last argument
        // keeps the newline before the closing paren) — the single/multiline
        // choice must not see it.
        let range = self.significant_range(&SyntaxElement::Node(node.clone()));
        let is_multiline = make_multiline
            || self.line(range.start()) != self.line(range.end())
            || Self::forces_multiline(node);

        let open = elements
            .iter()
            .position(|element| element.kind() == SyntaxKind::L_PAREN);
        let condition = open.and_then(|open_index| {
            elements
                .get(open_index + 1..)
                .and_then(|rest| rest.iter().find(|element| element.as_node().is_some()))
        });
        let hug = match (open, condition) {
            (Some(open_index), Some(condition)) => {
                let open_element = &elements[open_index];
                let condition_position = elements
                    .iter()
                    .position(|element| element.text_range() == condition.text_range())
                    .unwrap_or(open_index + 1);
                let no_comments = !Self::is_comment(
                    condition_position
                        .checked_sub(1)
                        .map(|index| &elements[index]),
                ) && !Self::is_comment(elements.get(condition_position + 1));
                self.same_line(open_element.text_range(), condition.text_range()) && no_comments
            }
            _ => true,
        };

        let close = elements
            .iter()
            .position(|element| element.kind() == SyntaxKind::R_PAREN);
        let mut seen_close = false;
        let mut indent_comments = false;
        let mut previous: Option<&Element> = None;
        let mut is_alternative = false;
        for (index, element) in elements.iter().enumerate() {
            let previous_is_comment = Self::is_comment(previous);
            match element.kind() {
                SyntaxKind::IF_KW => self.element(element, level, false),
                SyntaxKind::ELSE_KW => {
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                    is_alternative = true;
                }
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                        if indent_comments {
                            self.out.push_str(&self.indent);
                        }
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::L_PAREN => {
                    indent_comments = true;
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::R_PAREN => {
                    indent_comments = false;
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                _ if element.as_node().is_some() => {
                    if !seen_close && Some(index) != close {
                        // The condition.
                        if !hug {
                            self.newline(level);
                            self.out.push_str(&self.indent);
                            self.element(element, level + 1, false);
                        } else {
                            self.element(element, level, false);
                        }
                    } else {
                        // A branch.
                        if previous_is_comment {
                            self.newline(level);
                        } else {
                            self.space();
                        }
                        if is_multiline {
                            if element.kind() == SyntaxKind::BRACE_EXPR
                                || (element.kind() == SyntaxKind::IF_EXPR && is_alternative)
                            {
                                self.element(element, level, true);
                            } else {
                                self.wrap_braces(element, level);
                            }
                        } else {
                            self.element(element, level, false);
                        }
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn for_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let open = elements
            .iter()
            .find(|element| element.kind() == SyntaxKind::L_PAREN);
        let variable = elements
            .iter()
            .find(|element| element.kind() == SyntaxKind::NAME);
        let in_index = elements
            .iter()
            .position(|element| element.kind() == SyntaxKind::IN_KW);
        let sequence = in_index.and_then(|index| {
            elements
                .get(index + 1..)
                .and_then(|rest| rest.iter().find(|element| element.as_node().is_some()))
        });

        let condition_is_multiline = match (open, sequence) {
            (Some(open), Some(sequence)) => {
                !self.same_line(open.text_range(), sequence.text_range())
            }
            _ => false,
        };
        let loop_header_is_multiline = match (variable, sequence) {
            (Some(variable), Some(sequence)) => {
                !self.same_line(variable.text_range(), sequence.text_range())
            }
            _ => false,
        };
        let sequence_range = sequence.map(|sequence| sequence.text_range());
        let variable_range = variable.map(|variable| variable.text_range());

        let mut seen_close = false;
        let mut indent_comments = false;
        let mut previous: Option<&Element> = None;
        for element in &elements {
            let previous_is_comment = Self::is_comment(previous);
            match element.kind() {
                SyntaxKind::FOR_KW => self.element(element, level, false),
                SyntaxKind::IN_KW => {
                    if previous_is_comment || loop_header_is_multiline {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                        if indent_comments {
                            self.out.push_str(&self.indent);
                        }
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::L_PAREN => {
                    indent_comments = true;
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::R_PAREN => {
                    indent_comments = false;
                    if previous_is_comment || condition_is_multiline {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                SyntaxKind::NAME if Some(element.text_range()) == variable_range && !seen_close => {
                    if previous_is_comment || condition_is_multiline {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        self.element(element, level, false);
                    }
                }
                _ if element.as_node().is_some()
                    && Some(element.text_range()) == sequence_range =>
                {
                    if previous_is_comment {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        self.space();
                        if condition_is_multiline {
                            self.element(element, level + 1, false);
                        } else {
                            self.element(element, level, false);
                        }
                    }
                }
                _ if element.as_node().is_some() && seen_close => {
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    if element.kind() == SyntaxKind::BRACE_EXPR {
                        self.element(element, level, true);
                    } else {
                        self.wrap_braces(element, level);
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn while_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let open = elements
            .iter()
            .position(|element| element.kind() == SyntaxKind::L_PAREN);
        let condition = open.and_then(|open_index| {
            elements
                .get(open_index + 1..)
                .and_then(|rest| rest.iter().find(|element| element.as_node().is_some()))
        });
        let hug = match (open, condition) {
            (Some(open_index), Some(condition)) => {
                let condition_position = elements
                    .iter()
                    .position(|element| element.text_range() == condition.text_range())
                    .unwrap_or(open_index + 1);
                let no_comments = !Self::is_comment(
                    condition_position
                        .checked_sub(1)
                        .map(|index| &elements[index]),
                ) && !Self::is_comment(elements.get(condition_position + 1));
                self.same_line(elements[open_index].text_range(), condition.text_range())
                    && no_comments
            }
            _ => true,
        };

        let mut seen_close = false;
        let mut indent_comments = false;
        let mut previous: Option<&Element> = None;
        for element in &elements {
            let previous_is_comment = Self::is_comment(previous);
            match element.kind() {
                SyntaxKind::WHILE_KW => self.element(element, level, false),
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                        if indent_comments {
                            self.out.push_str(&self.indent);
                        }
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::L_PAREN => {
                    indent_comments = true;
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                }
                SyntaxKind::R_PAREN => {
                    indent_comments = false;
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                _ if element.as_node().is_some() && !seen_close => {
                    if !hug {
                        self.newline(level);
                        self.out.push_str(&self.indent);
                        self.element(element, level + 1, false);
                    } else {
                        self.element(element, level, false);
                    }
                }
                _ if element.as_node().is_some() => {
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    if element.kind() == SyntaxKind::BRACE_EXPR {
                        self.element(element, level, true);
                    } else {
                        self.wrap_braces(element, level);
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    fn repeat_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = Self::elements(node);
        let mut previous: Option<&Element> = None;
        for element in &elements {
            let previous_is_comment = Self::is_comment(previous);
            match element.kind() {
                SyntaxKind::REPEAT_KW => self.element(element, level, false),
                SyntaxKind::COMMENT | SyntaxKind::ANNOTATION => {
                    if previous.is_some_and(|previous| {
                        self.same_line(previous.text_range(), element.text_range())
                    }) {
                        self.space();
                    } else {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                _ if element.as_node().is_some() => {
                    if previous_is_comment {
                        self.newline(level);
                    } else {
                        self.space();
                    }
                    if element.kind() == SyntaxKind::BRACE_EXPR {
                        self.element(element, level, true);
                    } else {
                        self.wrap_braces(element, level);
                    }
                }
                _ => self.element(element, level, false),
            }
            previous = Some(element);
        }
    }

    // ---- `#:` annotations ----

    /// Renders one stitched annotation region. When the block parsed cleanly
    /// it is re-rendered canonically (author line breaks kept, token spacing
    /// and bracket hugging normalized); a block with a parse error inside is
    /// preserved line-by-line verbatim, apart from a single space after
    /// every `#:`.
    fn annotation(&mut self, node: &SyntaxNode, level: usize) {
        let range = node.text_range();
        // Inclusive end: an annotation-grammar error reported exactly at the
        // block's end offset (an unterminated type at end of region) still
        // belongs to this block.
        let has_error = self
            .error_ranges
            .iter()
            .any(|error| error.start() >= range.start() && error.start() <= range.end());
        // A head trailing code on its line is its own annotation: the
        // continuation lines form a separate block rendered from a fresh
        // delimiter state, so they never indent relative to a head column
        // the formatter does not control.
        let start = u32::from(range.start()) as usize;
        let line_start = self.line_starts[line_of(&self.line_starts, start)];
        let trailing_head = self.source[line_start..start]
            .chars()
            .any(|character| !character.is_whitespace());

        let lines = if has_error {
            None
        } else if trailing_head {
            self.annotation_line_tokens(node)
                .map(|groups| match groups.split_first() {
                    Some((head, continuation)) => {
                        let mut lines = self.render_annotation_lines(std::slice::from_ref(head));
                        if !continuation.is_empty() {
                            lines.extend(self.render_annotation_lines(continuation));
                        }
                        lines
                    }
                    None => Vec::new(),
                })
        } else {
            self.render_annotation(node)
        };
        let lines = lines.unwrap_or_else(|| self.verbatim_annotation(node));

        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                self.newline(level);
            }
            self.out.push_str(line);
        }
    }

    /// The annotation region's original lines, kept verbatim after the `#:`.
    fn verbatim_annotation(&self, node: &SyntaxNode) -> Vec<String> {
        let range = node.text_range();
        let text = &self.source[u32::from(range.start()) as usize..u32::from(range.end()) as usize];
        text.lines()
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
            .collect()
    }

    /// Canonical annotation rendering: tokens re-spaced per the type
    /// notation's rules, author line breaks kept, bracket hug bits read off
    /// the original layout (an opener ending its line expands: its closer
    /// gets its own line at the opener's indent; anything else hugs).
    fn render_annotation(&self, node: &SyntaxNode) -> Option<Vec<String>> {
        let line_tokens = self.annotation_line_tokens(node)?;
        Some(self.render_annotation_lines(&line_tokens))
    }

    /// The annotation's significant tokens grouped by source line, or `None`
    /// when a token has no display mapping (the block then goes verbatim).
    fn annotation_line_tokens(&self, node: &SyntaxNode) -> Option<Vec<Vec<AnnotationToken>>> {
        let mut line_tokens: Vec<Vec<AnnotationToken>> = Vec::new();
        let mut current_line = usize::MAX;
        for element in node.descendants_with_tokens() {
            let Some(token) = element.as_token() else {
                continue;
            };
            match token.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => continue,
                SyntaxKind::ANNOTATION_MARKER => {
                    let line = self.line(token.text_range().start());
                    if line != current_line {
                        line_tokens.push(Vec::new());
                        current_line = line;
                    }
                    continue;
                }
                _ => {}
            }
            let line = self.line(token.text_range().start());
            if line != current_line {
                line_tokens.push(Vec::new());
                current_line = line;
            }
            let kind = annotation_token_kind(token.kind(), token.text())?;
            let last = line_tokens.last_mut().expect("marker precedes tokens");
            // Directive names lex as `@` plus adjacent word/`-` tokens; glue
            // them back into one display token.
            if kind == AnnotationTokenKind::Word
                && last.last().is_some_and(|previous| {
                    previous.kind == AnnotationTokenKind::Directive
                        && previous.end == u32::from(token.text_range().start())
                })
            {
                let previous = last.last_mut().expect("directive precedes");
                previous.text.push_str(token.text());
                previous.end = u32::from(token.text_range().end());
                continue;
            }
            last.push(AnnotationToken {
                kind,
                text: token.text().to_owned(),
                end: u32::from(token.text_range().end()),
            });
        }
        Some(line_tokens)
    }

    fn render_annotation_lines(&self, line_tokens: &[Vec<AnnotationToken>]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::new();
        let mut current_level = 0usize;
        let mut open_delimiters: Vec<OpenDelimiter> = Vec::new();
        let mut previous_token: Option<AnnotationTokenKind> = None;
        let mut previous_text = String::new();
        let mut pending_empty_lines = 0usize;
        let mut started = false;

        for tokens in line_tokens {
            if tokens.is_empty() {
                if started {
                    pending_empty_lines += 1;
                } else {
                    lines.push("#:".to_owned());
                }
                continue;
            }
            for (index, token) in tokens.iter().enumerate() {
                let closer = is_closer(token.kind);
                let closes_expanded = closer
                    && open_delimiters
                        .last()
                        .is_none_or(|delimiter| delimiter.expanded);

                let break_before = if !started {
                    false
                } else if index == 0 && pending_empty_lines > 0 {
                    true
                } else if index == 0 {
                    !closer || closes_expanded
                } else {
                    closes_expanded
                };

                if break_before {
                    lines.push(annotation_line(current_level, &current, &self.indent));
                    current.clear();
                    for _ in 0..pending_empty_lines {
                        lines.push("#:".to_owned());
                    }
                    current_level = match open_delimiters.last() {
                        Some(delimiter) if closer => delimiter.opener_level,
                        Some(delimiter) => delimiter.opener_level + usize::from(delimiter.expanded),
                        None => 0,
                    };
                    previous_token = None;
                    previous_text.clear();
                }
                pending_empty_lines = 0;

                if let Some(previous) = previous_token
                    && annotation_space_between(previous, &previous_text, token.kind)
                {
                    current.push(' ');
                }
                current.push_str(&token.text);
                previous_token = Some(token.kind);
                previous_text = token.text.clone();
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
            lines.push(annotation_line(current_level, &current, &self.indent));
        }
        lines
    }
}

// ---- free helpers ----

fn line_starts(source: &str) -> Vec<usize> {
    // A bare `\r` terminates a line exactly like the lexer says it does;
    // `\r\n` counts once. Diverging from the lexer here makes two tokens
    // separated by a lone `\r` look same-line and glue into comment text.
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

fn is_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PLUS
            | SyntaxKind::MINUS
            | SyntaxKind::STAR
            | SyntaxKind::SLASH
            | SyntaxKind::CARET
            | SyntaxKind::SPECIAL
            | SyntaxKind::PIPE_GREATER
            | SyntaxKind::LESS
            | SyntaxKind::GREATER
            | SyntaxKind::LESS_EQ
            | SyntaxKind::GREATER_EQ
            | SyntaxKind::EQ2
            | SyntaxKind::BANG_EQ
            | SyntaxKind::AMP
            | SyntaxKind::AMP2
            | SyntaxKind::PIPE
            | SyntaxKind::PIPE2
            | SyntaxKind::TILDE
            | SyntaxKind::QUESTION
            | SyntaxKind::LESS_MINUS
            | SyntaxKind::LESS2_MINUS
            | SyntaxKind::MINUS_GREATER
            | SyntaxKind::MINUS_GREATER2
            | SyntaxKind::EQ
            | SyntaxKind::COLON_EQ
            | SyntaxKind::COLON
    )
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

// ---- annotation token model ----

struct OpenDelimiter {
    expanded: bool,
    opener_level: usize,
}

struct AnnotationToken {
    kind: AnnotationTokenKind,
    text: String,
    end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotationTokenKind {
    Word,
    Directive,
    Dots,
    Arrow,
    Pipe,
    Comma,
    Colon,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    OpenAngle,
    CloseAngle,
}

fn annotation_token_kind(kind: SyntaxKind, text: &str) -> Option<AnnotationTokenKind> {
    use AnnotationTokenKind::*;
    Some(match kind {
        SyntaxKind::L_PAREN => OpenParen,
        SyntaxKind::R_PAREN => CloseParen,
        SyntaxKind::L_BRACKET => OpenBracket,
        SyntaxKind::R_BRACKET => CloseBracket,
        SyntaxKind::L_BRACE => OpenBrace,
        SyntaxKind::R_BRACE => CloseBrace,
        SyntaxKind::LESS => OpenAngle,
        SyntaxKind::GREATER => CloseAngle,
        SyntaxKind::COMMA => Comma,
        SyntaxKind::COLON => Colon,
        SyntaxKind::PIPE => Pipe,
        SyntaxKind::MINUS_GREATER => Arrow,
        SyntaxKind::DOTS => Dots,
        SyntaxKind::AT => Directive,
        SyntaxKind::MINUS => {
            // Interior of a joined directive name (`@if-unknown`); anything
            // else is outside the annotation alphabet.
            return Some(Word);
        }
        _ if kind.is_keyword() => Word,
        SyntaxKind::IDENT | SyntaxKind::DOTDOTI => Word,
        _ => {
            let _ = text;
            return None;
        }
    })
}

fn annotation_line(level: usize, content: &str, indent: &str) -> String {
    format!("#: {}{content}", indent.repeat(level))
}

/// The canonical surface-type spacing: `,` and `:` hug their left neighbor,
/// `|` and `->` are surrounded by spaces, and brackets hug what they apply
/// to.
fn annotation_space_between(
    previous: AnnotationTokenKind,
    previous_text: &str,
    current: AnnotationTokenKind,
) -> bool {
    use AnnotationTokenKind::*;
    match current {
        Comma | Colon | CloseParen | CloseBracket | CloseBrace | CloseAngle => return false,
        // `(` hugs only what it APPLIES to — `fn(`, `Box<T>(`. After a `:`,
        // `|` or `->` it opens a grouped type, and the surrounding-space rule
        // for that neighbour governs: `x: (integer | character)`, not
        // `x:(integer | character)`.
        OpenParen
            if matches!(
                previous,
                Word | Dots | CloseParen | CloseBracket | CloseBrace | CloseAngle
            ) =>
        {
            return false;
        }
        OpenAngle if previous == Word => return false,
        OpenBracket if matches!(previous, Word | CloseAngle | CloseBracket) => return false,
        OpenBrace if previous == Word && previous_text == "list" => return false,
        Word if previous == Dots => return false,
        _ => {}
    }
    !matches!(previous, OpenParen | OpenBracket | OpenBrace | OpenAngle)
}

fn is_opener(kind: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    matches!(kind, OpenParen | OpenBracket | OpenBrace | OpenAngle)
}

fn is_closer(kind: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    matches!(kind, CloseParen | CloseBracket | CloseBrace | CloseAngle)
}

/// The fuzz invariant battery for one input: formatting never panics, is
/// deterministic, preserves the code, and is idempotent whenever it succeeds
/// (the output formats again, byte-identically). Shared by the in-tree property
/// fuzzer and the coverage-guided `fuzz/` targets so the contract has one home.
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

/// The tokens formatting must preserve exactly, in order, as `(kind, spelling)`.
/// This is the only invariant that can notice the formatter *changing* the code
/// — determinism, idempotence and "the output formats again" all hold for a
/// formatter that silently drops a statement or misspells a token.
///
/// Comparing kinds alone is not enough, and the gap is a miscompile rather than
/// a cosmetic one: R is case-sensitive, so a formatter that emits the wrong
/// bytes for an identifier — what a stale or off-by-one source range produces —
/// preserves every kind while renaming the user's variables.
///
/// Two allowances make the spelling comparable. Four kinds are dropped entirely
/// because the formatter may introduce or move them: it braces a
/// single-statement body, splits a `;` chain into lines, and re-lays-out a `#:`
/// block across markers. And a string is compared by its content rather than its
/// text, because the formatter chooses the quote character — but a *raw* string
/// it copies byte-for-byte, so that one keeps its full spelling.
fn significant_tokens(text: &str) -> Vec<(SyntaxKind, &str)> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    for token in syntax::lex(text).0 {
        let end = (start + u32::from(token.len) as usize).min(text.len());
        let spelling = text.get(start..end).unwrap_or_default();
        start = end;
        if token.kind.is_trivia()
            || matches!(
                token.kind,
                SyntaxKind::L_BRACE
                    | SyntaxKind::R_BRACE
                    | SyntaxKind::SEMICOLON
                    | SyntaxKind::ANNOTATION_MARKER
            )
        {
            continue;
        }
        tokens.push((token.kind, comparable_spelling(token.kind, spelling)));
    }
    tokens
}

fn comparable_spelling(kind: SyntaxKind, spelling: &str) -> &str {
    if kind != SyntaxKind::STRING || matches!(spelling.chars().next(), Some('r' | 'R')) {
        return spelling;
    }
    // An unterminated string reaches here as its opening quote plus whatever
    // followed, so trim the closer only when there is one to trim.
    let inner = spelling.strip_prefix(['"', '\'']).unwrap_or(spelling);
    match spelling.len() >= 2 {
        true => inner.strip_suffix(['"', '\'']).unwrap_or(inner),
        false => inner,
    }
}
