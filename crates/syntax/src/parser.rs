//! Precedence follows R's `gram.y`. Newlines are trivia inside `(`/`[`/`[[`
//! groups and end a complete statement at top level and directly inside `{}`;
//! `else` may follow a newline only when a group or brace encloses the `if`.

use crate::kind::SyntaxKind::{self, *};
use crate::lexer::{Token, lex};
use crate::{Parse, SyntaxError};
use rowan::{Checkpoint, GreenNodeBuilder, TextRange, TextSize};

pub fn parse(text: &str) -> Parse {
    let (tokens, lexer_errors) = lex(text);
    let mut offsets = Vec::with_capacity(tokens.len() + 1);
    let mut offset = 0usize;
    for token in &tokens {
        offsets.push(offset);
        offset += usize::from(token.len);
    }
    offsets.push(offset);

    let mut parser = Parser {
        text,
        tokens,
        offsets,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        errors: lexer_errors,
        groups: vec![0],
        depth: 0,
        in_annotation: false,
        type_region: false,
        inline_type: false,
        deferred_trivia: None,
        annotation_reported: false,
        annotation_unclosed_reported: false,
        statement_had_lexer_error: false,
        statement_left_group_open: false,
    };
    parser.source_file();
    let green = parser.builder.finish();
    Parse::new(green, parser.errors)
}

/// Deeper nesting reports an error instead of overflowing the stack.
const MAX_DEPTH: u32 = 500;

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    /// One extra trailing entry: the total length.
    offsets: Vec<usize>,
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<SyntaxError>,
    /// `{` pushes 0, `(`/`[`/`[[` increment the top, closers undo; newlines are
    /// significant iff the top is 0.
    groups: Vec<u32>,
    depth: u32,
    in_annotation: bool,
    /// Set while parsing type grammar, in a `#:` region or inline. The type grammar
    /// cannot resynchronize, so only a region's first error is reported; the rest
    /// are consequences.
    type_region: bool,
    /// Inline type grammar sits in R code, so its trivia follows R's newline rules
    /// instead of a `#:` region's.
    inline_type: bool,
    /// Where trivia skipped by inline type grammar starts. It is emitted only when the
    /// type continues past it, so trivia after the type stays outside its node.
    deferred_trivia: Option<usize>,
    annotation_reported: bool,
    /// Unclosed openers are found only at the end of their construct, after the
    /// inner consequences, so one such report gets through regardless.
    annotation_unclosed_reported: bool,
    /// The lexer's diagnostic is already precise, so the statement boundary
    /// does not report again.
    statement_had_lexer_error: bool,
    /// The unclosed-opener error is the whole story and the ending newline was
    /// eaten as trivia, so the boundary check neither reports nor recovers.
    statement_left_group_open: bool,
}

impl Parser<'_> {
    fn current(&self) -> Option<SyntaxKind> {
        self.tokens.get(self.pos).map(|token| token.kind)
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == Some(kind)
    }

    fn token_str(&self, pos: usize) -> &str {
        &self.text[self.offsets[pos]..self.offsets[pos + 1]]
    }

    fn token_range(&self, pos: usize) -> TextRange {
        let (start, end) = if pos < self.tokens.len() {
            (self.offsets[pos], self.offsets[pos + 1])
        } else {
            (self.text.len(), self.text.len())
        };
        TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32))
    }

    fn peek_significant(
        &self,
        mut pos: usize,
        across_newlines: bool,
    ) -> Option<(SyntaxKind, usize)> {
        loop {
            let token = self.tokens.get(pos)?;
            match token.kind {
                WHITESPACE | COMMENT => pos += 1,
                NEWLINE if across_newlines => pos += 1,
                ANNOTATION_MARKER => pos = self.annotation_region_end(pos),
                kind => return Some((kind, pos)),
            }
        }
    }

    fn annotation_region_end(&self, mut pos: usize) -> usize {
        let kind_at = |pos: usize| self.tokens.get(pos).map(|token| token.kind);
        loop {
            while kind_at(pos).is_some_and(|kind| kind != NEWLINE) {
                pos += 1;
            }
            if kind_at(pos).is_none() {
                return pos;
            }
            let mut lookahead = pos + 1;
            while kind_at(lookahead) == Some(WHITESPACE) {
                lookahead += 1;
            }
            if kind_at(lookahead) != Some(ANNOTATION_MARKER) {
                return pos;
            }
            pos = lookahead + 1;
        }
    }

    fn newlines_significant(&self) -> bool {
        *self.groups.last().expect("group stack is never empty") == 0
    }

    fn else_allowed_across_newline(&self) -> bool {
        !self.newlines_significant() || self.groups.len() > 1
    }

    fn bump(&mut self) {
        self.flush_deferred_trivia();
        let kind = self.current().expect("bump at end of input");
        let text = &self.text[self.offsets[self.pos]..self.offsets[self.pos + 1]];
        self.builder.token(kind.into(), text);
        self.pos += 1;
    }

    fn start(&mut self, kind: SyntaxKind) {
        self.flush_deferred_trivia();
        self.builder.start_node(kind.into());
    }

    fn flush_deferred_trivia(&mut self) {
        if let Some(start) = self.deferred_trivia.take() {
            for pos in start..self.pos {
                let kind = self.tokens[pos].kind;
                let text = &self.text[self.offsets[pos]..self.offsets[pos + 1]];
                self.builder.token(kind.into(), text);
            }
        }
    }

    fn start_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        self.builder.start_node_at(checkpoint, kind.into());
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    fn checkpoint(&mut self) -> Checkpoint {
        self.flush_deferred_trivia();
        self.builder.checkpoint()
    }

    /// Wraps the current token alone in a `kind` node.
    fn wrap(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump();
        self.finish();
    }

    /// Annotation parsing must not run past the `#:` region ending at `end`.
    fn ann_at(&self, kind: SyntaxKind, end: usize) -> bool {
        self.pos < end && self.at(kind)
    }

    fn error_here(&mut self, message: impl Into<String>) {
        let range = self.token_range(self.pos);
        self.error_at(range, message);
    }

    fn error_at(&mut self, range: TextRange, message: impl Into<String>) {
        if self.type_region {
            if self.annotation_reported {
                return;
            }
            self.annotation_reported = true;
        }
        self.push_error(range, message);
    }

    fn error_unclosed(&mut self, range: TextRange, message: impl Into<String>) {
        if !self.type_region {
            self.error_at(range, message);
        } else if !self.annotation_unclosed_reported {
            self.annotation_unclosed_reported = true;
            self.annotation_reported = true;
            self.push_error(range, message);
        }
    }

    fn error_expected(&mut self, what: &str) {
        let found = self.describe_current();
        self.error_here(format!("expected {what}, found {found}"));
    }

    fn push_error(&mut self, range: TextRange, message: impl Into<String>) {
        let mut error = SyntaxError::new(message, self.on_one_line(range));
        error.in_annotation = self.in_annotation;
        self.errors.push(error);
    }

    /// A range crossing a line break (an error at a newline token) collapses onto
    /// the last code character of its first line.
    fn on_one_line(&self, range: TextRange) -> TextRange {
        let text = &self.text[..usize::from(range.end()).min(self.text.len())];
        let Some(break_offset) = text[usize::from(range.start())..]
            .find('\n')
            .map(|index| usize::from(range.start()) + index)
        else {
            return range;
        };
        let end = self.text[..break_offset].trim_end().len();
        let start = self.text[..end]
            .char_indices()
            .next_back()
            .map_or(end, |(index, _)| index);
        TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32))
    }

    /// Anchored after the previous element so the report stays on its line.
    fn separator_gap(&self) -> TextRange {
        let end = (0..self.pos)
            .rev()
            .find(|&index| !matches!(self.tokens[index].kind, WHITESPACE | COMMENT | NEWLINE))
            .map(|index| self.token_range(index).end())
            .unwrap_or_else(|| self.token_range(self.pos).start());
        TextRange::empty(end)
    }

    fn describe_current(&self) -> String {
        match self.current() {
            None => "end of file".to_owned(),
            Some(NEWLINE) => "end of line".to_owned(),
            Some(kind) => kind.display().to_owned(),
        }
    }

    /// Tells an unclosed opener from a forgotten separator: an assigning line is
    /// the next statement. `=` is excluded because `name = value` in a list is a
    /// named argument missing its comma, which is also R's reading.
    fn starts_new_statement(&self) -> bool {
        match self.current() {
            Some(IF_KW | FOR_KW | WHILE_KW | REPEAT_KW) => true,
            Some(IDENT | STRING) => {
                matches!(
                    self.peek_significant(self.pos + 1, false)
                        .map(|(kind, _)| kind),
                    Some(LESS_MINUS | LESS2_MINUS)
                ) || self.annotated_binding_ahead().is_some()
            }
            _ => false,
        }
    }

    /// Scans backwards: the newline was already consumed by the preceding element.
    fn crossed_line(&self) -> bool {
        self.tokens[..self.pos]
            .iter()
            .rev()
            .find(|token| !matches!(token.kind, WHITESPACE | COMMENT))
            .is_some_and(|token| token.kind == NEWLINE)
    }

    fn eat_trivia(&mut self, across_newlines: bool) {
        loop {
            match self.current() {
                Some(WHITESPACE | COMMENT) => self.bump(),
                Some(NEWLINE) if across_newlines || !self.newlines_significant() => self.bump(),
                Some(ANNOTATION_MARKER) => self.annotation(),
                _ => return,
            }
        }
    }

    fn annotation(&mut self) {
        self.start(ANNOTATION);
        self.in_annotation = true;
        self.type_region = true;
        self.annotation_reported = false;
        self.annotation_unclosed_reported = false;
        let end = self.annotation_region_end(self.pos);
        loop {
            self.ann_trivia(end);
            if self.pos >= end {
                break;
            }
            let before = self.pos;
            if self.at(AT) {
                self.ann_directive(end);
            } else {
                self.ann_type(end, true);
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.in_annotation = false;
        self.type_region = false;
        self.finish();
    }

    fn ann_trivia(&mut self, end: usize) {
        if self.inline_type {
            let start = self.pos;
            loop {
                match self.current() {
                    Some(WHITESPACE) => self.pos += 1,
                    Some(NEWLINE | COMMENT) if !self.newlines_significant() => self.pos += 1,
                    _ => break,
                }
            }
            if self.pos > start && self.deferred_trivia.is_none() {
                self.deferred_trivia = Some(start);
            }
            return;
        }
        while self.pos < end
            && matches!(
                self.current(),
                Some(WHITESPACE | NEWLINE | ANNOTATION_MARKER | COMMENT)
            )
        {
            self.bump();
        }
    }

    fn adjacent(&self, pos: usize) -> bool {
        pos > 0
            && pos < self.tokens.len()
            && self.offsets[pos] == self.offsets[pos - 1] + usize::from(self.tokens[pos - 1].len)
    }

    fn ann_directive(&mut self, end: usize) {
        self.start(ANNOTATION_DIRECTIVE);
        debug_assert!(self.at(AT));
        self.bump();
        // `@if-unknown` lexes as `if` `-` `unknown`; adjacent tokens join.
        let name_start = self.pos;
        let mut name = String::new();
        while self.pos < end {
            let joinable = match self.current() {
                Some(IDENT | MINUS) => true,
                Some(kind) => kind.is_keyword(),
                None => false,
            };
            if !joinable || !self.adjacent(self.pos) {
                break;
            }
            name.push_str(self.token_str(self.pos));
            self.bump();
        }
        if name.is_empty() {
            self.error_here("expected a directive name after `@`");
            self.finish();
            return;
        }
        match name.as_str() {
            "type" | "alias" => {
                self.ann_trivia(end);
                if self.ann_at(IDENT, end) {
                    self.wrap(NAME);
                } else {
                    self.error_here(format!("expected a type name after `@{name}`"));
                }
                self.ann_trivia(end);
                if self.ann_at(LESS, end) {
                    self.ann_binder_list(end);
                }
                self.ann_trivia(end);
                self.ann_braced_type(end, &name);
                // Trailing text would otherwise parse as a second annotation and
                // silently refuse the block.
                while self.ann_at(WHITESPACE, end) {
                    self.bump();
                }
                if self.pos < end && !matches!(self.current(), Some(NEWLINE | COMMENT) | None) {
                    self.error_here(format!("unexpected text after the `@{name}` definition"));
                    while self.pos < end && !self.at(NEWLINE) {
                        self.bump();
                    }
                }
            }
            "param" => {
                self.ann_trivia(end);
                match self.current() {
                    Some(IDENT | DOTS) if self.pos < end => {
                        self.wrap(NAME)
                    }
                    Some(L_BRACKET) if self.pos < end => {
                        self.bump();
                        self.ann_trivia(end);
                        if self.ann_at(IDENT, end) {
                            self.wrap(NAME);
                        } else {
                            self.error_here("expected a parameter name inside `[...]`");
                        }
                        self.ann_trivia(end);
                        if self.ann_at(R_BRACKET, end) {
                            self.bump();
                        } else {
                            self.error_here("expected `]` after the optional parameter name");
                        }
                    }
                    // JSDoc writes the type first. This directive writes the
                    // name first, so say which form to use instead of only
                    // reporting the missing name.
                    Some(L_BRACE) if self.pos < end => self.error_here(
                        "expected a parameter name after `@param`. The name comes before the type, as `@param name {TYPE}`",
                    ),
                    _ => self.error_here("expected a parameter name after `@param`"),
                }
                self.ann_trivia(end);
                self.ann_braced_type(end, "param");
            }
            "return" | "returns" => {
                self.ann_trivia(end);
                self.ann_braced_type(end, &name);
            }
            "forall" => loop {
                self.ann_trivia(end);
                if !self.ann_at(IDENT, end) {
                    self.error_here("expected a type parameter name after `@forall`");
                    break;
                }
                self.ann_binder(end);
                self.ann_trivia(end);
                if self.ann_at(COMMA, end) {
                    self.bump();
                } else {
                    break;
                }
            },
            "trust" | "new" | "if-unknown" => {
                self.ann_trivia(end);
                if self.pos < end {
                    self.ann_type(end, false);
                } else {
                    self.error_here(format!("expected a type after `@{name}`"));
                }
            }
            "strict" => {
                self.ann_trivia(end);
                if self.ann_at(IDENT, end) && self.token_str(self.pos) == "off" {
                    self.bump();
                }
            }
            _ => {
                let range = TextRange::new(
                    self.token_range(name_start).start(),
                    self.token_range(self.pos.saturating_sub(1)).end(),
                );
                self.error_at(range, format!("unknown annotation directive `@{name}`"));
                while self.pos < end {
                    self.bump();
                }
            }
        }
        self.finish();
    }

    fn ann_braced_type(&mut self, end: usize, directive: &str) {
        if self.ann_at(L_BRACE, end) {
            let open_range = self.token_range(self.pos);
            self.bump();
            self.ann_trivia(end);
            if self.at(R_BRACE) || self.pos >= end {
                self.error_here(format!(
                    "expected a type inside `{{...}}` of `@{directive}`"
                ));
            } else {
                // Binders are refused here like any higher-rank position; type
                // parameters belong on `@forall` or `@type Name<T>`.
                self.ann_type(end, false);
            }
            self.ann_trivia(end);
            if self.ann_at(R_BRACE, end) {
                self.bump();
            } else {
                self.error_unclosed(
                    open_range,
                    format!("unclosed `{{` in `@{directive}`; expected a matching `}}`"),
                );
            }
        } else {
            self.error_here(format!("expected `{{TYPE}}` after `@{directive}`"));
        }
    }

    fn ann_type(&mut self, end: usize, allow_binders: bool) {
        if allow_binders && self.ann_at(LESS, end) {
            self.ann_binder_list(end);
            self.ann_trivia(end);
        }
        self.ann_union(end);
    }

    fn ann_binder_list(&mut self, end: usize) {
        self.start(TYPE_BINDER_LIST);
        debug_assert!(self.at(LESS));
        self.bump();
        loop {
            self.ann_trivia(end);
            match self.current() {
                // Only an empty list or a trailing comma reaches `>` here.
                Some(GREATER) if self.pos < end => {
                    self.error_here("expected a type parameter name in `<...>`");
                    self.bump();
                    break;
                }
                Some(IDENT) if self.pos < end => {
                    self.ann_binder(end);
                    self.ann_trivia(end);
                    if self.ann_at(COMMA, end) {
                        self.bump();
                    } else if self.ann_at(GREATER, end) {
                        self.bump();
                        break;
                    } else {
                        self.error_here("expected `,` or `>` in the type parameter list");
                        break;
                    }
                }
                _ => {
                    self.error_here("expected a type parameter name in `<...>`");
                    break;
                }
            }
        }
        self.finish();
    }

    fn ann_binder(&mut self, end: usize) {
        self.start(TYPE_BINDER);
        self.wrap(NAME);
        self.ann_trivia(end);
        if self.ann_at(COLON, end) {
            self.bump();
            self.ann_trivia(end);
            if self.ann_at(IDENT, end) {
                // A constraint may span words (`scalar numeric`). Trivia is taken
                // only between words: after the last it would pull the next `#:`
                // marker into the binder.
                while self.ann_next_significant(end) == Some(IDENT) {
                    self.wrap(NAME);
                    self.ann_trivia(end);
                }
                self.wrap(NAME);
            } else {
                self.error_here("expected a constraint name after `:`");
            }
        }
        self.finish();
    }

    fn ann_union(&mut self, end: usize) {
        let checkpoint = self.checkpoint();
        self.ann_primary(end);
        let mut members = 1usize;
        loop {
            self.ann_trivia(end);
            if self.ann_at(PIPE, end) {
                if members == 1 {
                    self.start_at(checkpoint, TYPE_UNION);
                }
                members += 1;
                self.bump();
                self.ann_trivia(end);
                self.ann_primary(end);
            } else {
                break;
            }
        }
        if members > 1 {
            self.finish();
        }
    }

    fn ann_primary(&mut self, end: usize) {
        if self.pos >= end {
            self.error_here("expected a type");
            return;
        }
        if self.depth >= MAX_DEPTH {
            self.error_here("type nesting is too deep");
            return;
        }
        self.depth += 1;
        self.ann_primary_inner(end);
        self.depth -= 1;
    }

    fn ann_primary_inner(&mut self, end: usize) {
        let checkpoint = self.checkpoint();
        match self.current() {
            Some(LESS) => {
                self.error_here("higher-rank polymorphism is not supported: type parameter binders may only appear at the outermost level");
                // Read the type past the binder so the one finding is not
                // buried under consequences. The `ERROR` wrapper tells lowering
                // the binder's names bind nothing, so they are not reported again.
                self.start(ERROR);
                self.ann_binder_list(end);
                self.finish();
                self.ann_trivia(end);
                self.ann_union(end);
                return;
            }
            Some(L_PAREN) => {
                self.start(TYPE_PAREN);
                let open_range = self.token_range(self.pos);
                self.bump();
                self.ann_trivia(end);
                self.ann_type(end, false);
                self.ann_trivia(end);
                if self.ann_at(R_PAREN, end) {
                    self.bump();
                } else {
                    self.error_unclosed(
                        open_range,
                        "unclosed `(` in this type; expected a matching `)`",
                    );
                }
                self.finish();
            }
            Some(IDENT | NULL_KW) => match self.token_str(self.pos) {
                "list" if self.ann_next_significant(end) == Some(L_BRACKET) => {
                    self.ann_list_brackets(end);
                }
                "list" if self.ann_next_significant(end) == Some(L_BRACE) => {
                    self.ann_list_braces(end);
                }
                "fn" if self.ann_next_significant(end) == Some(L_PAREN) => {
                    self.ann_function_type(end);
                }
                _ => {
                    if self.ann_next_significant(end) == Some(LESS) {
                        self.start(TYPE_APPLY);
                        self.wrap(NAME);
                        self.ann_trivia(end);
                        self.ann_type_arg_list(end);
                        self.finish();
                    } else {
                        self.wrap(TYPE_REF);
                    }
                }
            },
            _ => {
                self.error_expected("a type");
                return;
            }
        }
        loop {
            self.ann_trivia(end);
            if !self.ann_at(L_BRACKET, end) {
                break;
            }
            let after = self.peek_significant(self.pos + 1, true);
            let is_suffix = match after {
                Some((R_BRACKET, _)) => true,
                Some((IDENT, ident_pos)) => {
                    self.token_str(ident_pos) == "named"
                        && matches!(
                            self.peek_significant(ident_pos + 1, true),
                            Some((R_BRACKET, _))
                        )
                }
                _ => false,
            };
            if !is_suffix {
                break;
            }
            self.start_at(checkpoint, TYPE_VECTOR);
            self.bump();
            self.ann_trivia(end);
            if matches!(self.current(), Some(IDENT)) {
                self.bump();
                self.ann_trivia(end);
            }
            if self.ann_at(R_BRACKET, end) {
                self.bump();
            }
            self.finish();
        }
    }

    fn ann_next_significant(&self, end: usize) -> Option<SyntaxKind> {
        let across_newlines = !(self.inline_type && self.newlines_significant());
        let (kind, pos) = self.peek_significant(self.pos + 1, across_newlines)?;
        (pos < end).then_some(kind)
    }

    fn ann_type_arg_list(&mut self, end: usize) {
        self.start(TYPE_ARG_LIST);
        debug_assert!(self.at(LESS));
        self.bump();
        let mut arguments = 0;
        loop {
            self.ann_trivia(end);
            match self.current() {
                // An empty list is left to the arity check, which names the
                // expected count.
                Some(GREATER) if self.pos < end => {
                    if arguments > 0 {
                        self.error_here("expected a type in the type argument list");
                    }
                    self.bump();
                    break;
                }
                _ if self.pos >= end => {
                    self.error_here("expected `>` to close the type argument list");
                    break;
                }
                _ => {
                    self.ann_type(end, false);
                    arguments += 1;
                    self.ann_trivia(end);
                    if self.ann_at(COMMA, end) {
                        self.bump();
                    } else if self.ann_at(GREATER, end) {
                        self.bump();
                        break;
                    } else {
                        self.error_here("expected `,` or `>` in the type argument list");
                        break;
                    }
                }
            }
        }
        self.finish();
    }

    fn ann_list_brackets(&mut self, end: usize) {
        self.start(TYPE_LIST);
        self.wrap(NAME);
        self.ann_trivia(end);
        debug_assert!(self.at(L_BRACKET));
        let open_range = self.token_range(self.pos);
        self.bump();
        self.ann_trivia(end);
        if self.ann_at(IDENT, end)
            && self.token_str(self.pos) == "named"
            && self.ann_next_significant(end) == Some(COLON)
        {
            self.bump();
            self.ann_trivia(end);
            self.bump();
            self.ann_trivia(end);
        }
        self.ann_type(end, false);
        self.ann_trivia(end);
        if self.ann_at(R_BRACKET, end) {
            self.bump();
        } else {
            self.error_unclosed(
                open_range,
                "unclosed `[` in this list type; expected a matching `]`",
            );
        }
        self.finish();
    }

    fn ann_list_braces(&mut self, end: usize) {
        let checkpoint = self.checkpoint();
        self.wrap(NAME);
        self.ann_trivia(end);
        debug_assert!(self.at(L_BRACE));
        let open_range = self.token_range(self.pos);
        self.bump();
        let mut is_record = None;
        loop {
            self.ann_trivia(end);
            if self.ann_at(R_BRACE, end) {
                self.bump();
                break;
            }
            if self.pos >= end {
                self.error_unclosed(
                    open_range,
                    "unclosed `{` in this list type; expected a matching `}`",
                );
                break;
            }
            let named_field = matches!(self.current(), Some(IDENT))
                && self.ann_next_significant(end) == Some(COLON);
            is_record.get_or_insert(named_field);
            if named_field {
                self.start(TYPE_FIELD);
                self.wrap(NAME);
                self.ann_trivia(end);
                self.bump();
                self.ann_trivia(end);
                self.ann_type(end, false);
                self.finish();
            } else {
                self.ann_type(end, false);
            }
            self.ann_trivia(end);
            if self.ann_at(COMMA, end) {
                self.bump();
            } else if self.ann_at(R_BRACE, end) {
                self.bump();
                break;
            } else {
                self.error_here("expected `,` or `}` in this list type");
                break;
            }
        }
        self.start_at(
            checkpoint,
            if is_record == Some(true) {
                TYPE_RECORD
            } else {
                TYPE_TUPLE
            },
        );
        self.finish();
    }

    fn ann_function_type(&mut self, end: usize) {
        self.start(TYPE_FUNCTION);
        self.wrap(NAME);
        self.ann_trivia(end);
        debug_assert!(self.at(L_PAREN));
        self.start(TYPE_PARAMETER_LIST);
        let open_range = self.token_range(self.pos);
        self.bump();
        loop {
            self.ann_trivia(end);
            if self.ann_at(R_PAREN, end) {
                self.bump();
                break;
            }
            if self.pos >= end {
                self.error_unclosed(
                    open_range,
                    "unclosed `(` in this function type; expected a matching `)`",
                );
                break;
            }
            self.ann_function_parameter(end);
            self.ann_trivia(end);
            if self.ann_at(COMMA, end) {
                self.bump();
            } else if self.ann_at(R_PAREN, end) {
                self.bump();
                break;
            } else {
                self.error_here("expected `,` or `)` in this function type's parameters");
                break;
            }
        }
        self.finish();
        self.ann_trivia(end);
        if self.ann_at(MINUS_GREATER, end) {
            self.bump();
            self.ann_trivia(end);
            if self.pos < end {
                self.ann_type(end, false);
            } else {
                self.error_here("expected a return type after `->`");
            }
        }
        self.finish();
    }

    fn ann_function_parameter(&mut self, end: usize) {
        self.start(TYPE_PARAMETER);
        match self.current() {
            Some(DOTS) => {
                self.bump();
                if self.ann_at(IDENT, end) && self.adjacent(self.pos) {
                    self.wrap(NAME);
                }
                self.ann_trivia(end);
                if self.ann_at(COLON, end) {
                    self.bump();
                    self.ann_trivia(end);
                    self.ann_type(end, false);
                }
            }
            Some(L_BRACKET) => {
                self.bump();
                self.ann_trivia(end);
                if self.ann_at(IDENT, end) {
                    self.wrap(NAME);
                } else {
                    self.error_here("expected `[name]: TYPE` for an optional parameter");
                }
                self.ann_trivia(end);
                if self.ann_at(R_BRACKET, end) {
                    self.bump();
                } else {
                    self.error_here("expected `]` after the optional parameter name");
                }
                self.ann_trivia(end);
                if self.ann_at(COLON, end) {
                    self.bump();
                    self.ann_trivia(end);
                    self.ann_type(end, false);
                } else {
                    self.error_here("expected `:` after `[name]` in an optional parameter");
                }
            }
            Some(IDENT) if self.ann_next_significant(end) == Some(COLON) => {
                self.wrap(NAME);
                self.ann_trivia(end);
                self.bump();
                self.ann_trivia(end);
                self.ann_type(end, false);
            }
            _ => self.ann_type(end, false),
        }
        self.finish();
    }

    fn source_file(&mut self) {
        self.start(SOURCE_FILE);
        self.statements(None);
        if self.pos < self.tokens.len() {
            self.start(ERROR);
            while self.pos < self.tokens.len() {
                self.bump();
            }
            self.finish();
        }
        self.finish();
    }

    fn statements(&mut self, terminator: Option<SyntaxKind>) {
        loop {
            self.eat_trivia(true);
            self.statement_had_lexer_error = false;
            self.statement_left_group_open = false;
            match self.current() {
                Some(SEMICOLON) => {
                    self.bump();
                    continue;
                }
                None => return,
                Some(kind) if Some(kind) == terminator => return,
                _ => {}
            }

            let statement_start = self.pos;
            if !self.statement() {
                match self.current() {
                    Some(ELSE_KW) => self.error_here(
                        "unexpected `else`: it must stay on the same line as the `if` branch it belongs to (or the `if` must be inside braces)",
                    ),
                    Some(R_BRACE) if terminator.is_none() => {
                        self.error_here("unmatched `}`: no `{` is open here")
                    }
                    _ => {
                        self.error_expected("a statement");
                    }
                }
                self.start(ERROR);
                self.recover_to_statement_boundary(terminator);
                self.finish();
                continue;
            }

            self.eat_trivia(false);
            match self.current() {
                None | Some(NEWLINE | SEMICOLON) => {}
                Some(kind) if Some(kind) == terminator => {}
                Some(_) if self.statement_left_group_open => continue,
                Some(kind) => {
                    if kind != ERROR_TOKEN && !self.statement_had_lexer_error {
                        if self.statement_ends_with_bare_return(statement_start)
                            && starts_expression(kind)
                        {
                            self.error_here("`return` needs parentheses: `return(x)`");
                        } else {
                            let description = self.describe_current();
                            self.error_here(format!(
                                "unexpected {description} after this expression; expected a newline or `;`"
                            ));
                        }
                    }
                    self.start(ERROR);
                    self.recover_to_statement_boundary(terminator);
                    self.finish();
                }
            }
        }
    }

    /// `return x`: `return` reads like a keyword but is a function in R. Reached
    /// through an access operator (`x$return`) it is an ordinary name.
    fn statement_ends_with_bare_return(&self, start: usize) -> bool {
        let mut significant = (start..self.pos)
            .filter(|&index| {
                self.tokens
                    .get(index)
                    .is_some_and(|token| !matches!(token.kind, WHITESPACE | COMMENT | NEWLINE))
            })
            .rev();
        let Some(last) = significant.next() else {
            return false;
        };
        let before = significant.next().and_then(|index| self.tokens.get(index));
        self.tokens
            .get(last)
            .is_some_and(|token| token.kind == IDENT)
            && self.token_str(last) == "return"
            && !before.is_some_and(|token| matches!(token.kind, DOLLAR | AT | COLON2 | COLON3))
    }

    fn recover_to_statement_boundary(&mut self, terminator: Option<SyntaxKind>) {
        while let Some(kind) = self.current() {
            if matches!(kind, NEWLINE | SEMICOLON) || Some(kind) == terminator {
                return;
            }
            self.bump();
        }
    }

    /// A statement is an expression, or an annotated binding `name: TYPE <- value`.
    /// R reads that binding as `` `:<-` `` on `name:TYPE`, a replacement function
    /// base R does not define, so no working R program has this shape.
    fn statement(&mut self) -> bool {
        let Some(assignment) = self.annotated_binding_ahead() else {
            return self.expression(0);
        };
        if self.depth >= MAX_DEPTH {
            self.error_here("expression nesting is too deep");
            return false;
        }
        self.depth += 1;
        let checkpoint = self.checkpoint();
        self.wrap(NAME);
        while self.at(WHITESPACE) {
            self.bump();
        }
        self.start(TYPE_ASCRIPTION);
        self.bump();
        self.inline_type(|parser, end| parser.ann_type(end, false));
        let mut next = self.pos;
        while self.tokens.get(next).is_some_and(|token| token.kind == WHITESPACE) {
            next += 1;
        }
        if next < assignment {
            while self.at(WHITESPACE) {
                self.bump();
            }
            if !self.annotation_reported {
                let found = self.describe_current();
                self.error_here(format!(
                    "unexpected {found} in this type annotation; expected `<-` after the type"
                ));
            }
            self.start(ERROR);
            while self.pos < assignment {
                self.bump();
            }
            self.finish();
        }
        self.finish();
        self.operators(checkpoint, 0);
        self.depth -= 1;
        true
    }

    /// The position of the `<-` or `=` closing an annotated binding that starts
    /// here. The type is scanned shallowly: the grammar reports its own errors once
    /// the statement is committed to being a binding.
    fn annotated_binding_ahead(&self) -> Option<usize> {
        if !self.at(IDENT) {
            return None;
        }
        let kind_at = |pos: usize| self.tokens.get(pos).map(|token| token.kind);
        let mut pos = self.pos + 1;
        while kind_at(pos) == Some(WHITESPACE) {
            pos += 1;
        }
        if kind_at(pos) != Some(COLON) {
            return None;
        }
        pos += 1;
        let mut depth = 0u32;
        loop {
            match kind_at(pos)? {
                WHITESPACE | COMMENT => {}
                NEWLINE if depth > 0 => {}
                L_PAREN | L_BRACKET | L_BRACE | LESS => depth += 1,
                R_PAREN | R_BRACKET | R_BRACE | GREATER => depth = depth.checked_sub(1)?,
                LESS_MINUS | EQ if depth == 0 => return Some(pos),
                IDENT | NULL_KW | PIPE | COMMA | COLON | MINUS_GREATER | DOTS => {}
                _ => return None,
            }
            pos += 1;
        }
    }

    /// Runs type grammar on R's token stream: errors are R syntax errors, and
    /// trivia follows R's newline rules.
    fn inline_type(&mut self, parse: impl FnOnce(&mut Self, usize)) {
        let saved = (self.inline_type, self.type_region);
        self.inline_type = true;
        self.type_region = true;
        self.annotation_reported = false;
        self.annotation_unclosed_reported = false;
        let end = self.tokens.len();
        self.ann_trivia(end);
        parse(self, end);
        // Trivia after the type was never emitted: hand it back to R's grammar.
        if let Some(start) = self.deferred_trivia.take() {
            self.pos = start;
        }
        (self.inline_type, self.type_region) = saved;
    }

    /// Returns false, consuming nothing, when no expression starts here; the
    /// caller reports the context-specific error.
    fn expression(&mut self, min_bp: u8) -> bool {
        if self.depth >= MAX_DEPTH {
            self.error_here("expression nesting is too deep");
            return false;
        }
        self.depth += 1;
        let produced = self.expression_inner(min_bp);
        self.depth -= 1;
        produced
    }

    fn expression_inner(&mut self, min_bp: u8) -> bool {
        let checkpoint = self.checkpoint();
        if !self.primary() {
            return false;
        }
        self.operators(checkpoint, min_bp);
        true
    }

    /// The postfix and infix operators applying to the operand that starts at
    /// `checkpoint`.
    fn operators(&mut self, checkpoint: Checkpoint, min_bp: u8) {
        loop {
            self.eat_trivia(false);
            let Some(kind) = self.current() else { break };
            match kind {
                L_PAREN if CALL_BP >= min_bp => {
                    self.start_at(checkpoint, CALL_EXPR);
                    self.argument_list(L_PAREN);
                    self.finish();
                }
                L_BRACKET if CALL_BP >= min_bp => {
                    self.start_at(checkpoint, SUBSET_EXPR);
                    self.argument_list(L_BRACKET);
                    self.finish();
                }
                L_BRACKET2 if CALL_BP >= min_bp => {
                    self.start_at(checkpoint, SUBSET2_EXPR);
                    self.argument_list(L_BRACKET2);
                    self.finish();
                }
                DOLLAR | AT if FIELD_BP >= min_bp => {
                    let node = if kind == DOLLAR { DOLLAR_EXPR } else { AT_EXPR };
                    self.start_at(checkpoint, node);
                    self.bump();
                    self.eat_trivia(true);
                    self.field_name(kind);
                    self.finish();
                }
                COLON2 | COLON3 if NAMESPACE_BP >= min_bp => {
                    self.start_at(checkpoint, NAMESPACE_EXPR);
                    self.bump();
                    self.eat_trivia(true);
                    self.field_name(kind);
                    self.finish();
                }
                _ => {
                    let Some((left_bp, right_bp)) = infix_binding_power(kind) else {
                        break;
                    };
                    if left_bp < min_bp {
                        break;
                    }
                    self.start_at(checkpoint, BINARY_EXPR);
                    let operator_range = self.token_range(self.pos);
                    let operator = self.describe_current();
                    self.bump();
                    self.eat_trivia(true);
                    if !self.expression(right_bp) {
                        self.error_at(
                            operator_range,
                            format!("expected an expression after {operator}"),
                        );
                    }
                    self.finish();
                }
            }
        }
    }

    fn field_name(&mut self, operator: SyntaxKind) {
        match self.current() {
            Some(IDENT | DOTS | DOTDOTI) => self.wrap(NAME),
            Some(STRING) => self.wrap(LITERAL),
            _ => {
                self.error_expected(&format!("a name after {}", operator.display()));
            }
        }
    }

    fn primary(&mut self) -> bool {
        let Some(kind) = self.current() else {
            return false;
        };
        match kind {
            IDENT | DOTS | DOTDOTI | UNDERSCORE => self.wrap(NAME),
            ERROR_TOKEN => {
                self.statement_had_lexer_error = true;
                self.wrap(ERROR);
            }
            INTEGER | DOUBLE | COMPLEX | STRING | RAW_STRING => self.wrap(LITERAL),
            MINUS | PLUS => self.unary(UNARY_SIGN_BP),
            BANG => self.unary(UNARY_NOT_BP),
            TILDE => self.unary(TILDE_BP + 1),
            QUESTION => self.unary(HELP_BP + 1),
            L_PAREN => {
                self.start(PAREN_EXPR);
                let open_range = self.token_range(self.pos);
                self.open_group();
                self.bump();
                self.eat_trivia(true);
                if !self.expression(0) {
                    self.error_expected("an expression inside `(`");
                }
                self.eat_trivia(true);
                self.end_group();
                if self.at(R_PAREN) {
                    self.bump();
                } else {
                    self.error_at(open_range, "unclosed `(`; expected a matching `)`");
                }
                self.finish();
            }
            L_BRACE => {
                self.start(BRACE_EXPR);
                let open_range = self.token_range(self.pos);
                self.groups.push(0);
                self.bump();
                self.statements(Some(R_BRACE));
                self.groups.pop();
                if self.at(R_BRACE) {
                    self.bump();
                } else {
                    self.error_at(open_range, "unclosed `{`; expected a matching `}`");
                }
                self.finish();
            }
            IF_KW => self.if_expr(),
            FOR_KW => self.for_expr(),
            WHILE_KW => self.while_expr(),
            REPEAT_KW => {
                self.start(REPEAT_EXPR);
                self.bump();
                self.eat_trivia(true);
                if !self.expression(0) {
                    self.error_here("expected a body after `repeat`");
                }
                self.finish();
            }
            FUNCTION_KW | BACKSLASH => self.function_def(),
            BREAK_KW => self.wrap(BREAK_EXPR),
            NEXT_KW => self.wrap(NEXT_EXPR),
            _ if (TRUE_KW..=NA_CHARACTER_KW).contains(&kind) => self.wrap(LITERAL),
            _ => return false,
        }
        true
    }

    fn unary(&mut self, right_bp: u8) {
        self.start(UNARY_EXPR);
        let operator = self.describe_current();
        let operator_range = self.token_range(self.pos);
        self.bump();
        self.eat_trivia(true);
        if !self.expression(right_bp) {
            self.error_at(
                operator_range,
                format!("expected an expression after {operator}"),
            );
        }
        self.finish();
    }

    fn if_expr(&mut self) {
        self.start(IF_EXPR);
        self.bump();
        self.eat_trivia(true);
        self.condition_parens("if");
        self.eat_trivia(true);
        if !self.expression(0) {
            self.error_expected("a branch after the `if` condition");
        }
        let across = self.else_allowed_across_newline();
        if let Some((ELSE_KW, _)) = self.peek_significant(self.pos, across) {
            self.eat_trivia(across);
            debug_assert!(self.at(ELSE_KW));
            self.bump();
            self.eat_trivia(true);
            if !self.expression(0) {
                self.error_expected("an expression after `else`");
            }
        }
        self.finish();
    }

    fn for_expr(&mut self) {
        self.start(FOR_EXPR);
        self.bump();
        self.eat_trivia(true);
        if self.at(L_PAREN) {
            let open_range = self.token_range(self.pos);
            self.open_group();
            self.bump();
            self.eat_trivia(true);
            if self.at(IDENT) {
                self.wrap(NAME);
            } else {
                self.error_expected("a loop variable after `for (`");
            }
            self.eat_trivia(true);
            if self.at(IN_KW) {
                self.bump();
            } else {
                self.error_expected("`in` in the `for` head");
            }
            self.eat_trivia(true);
            if !self.expression(0) {
                self.error_expected("a sequence expression in the `for` head");
            }
            self.eat_trivia(true);
            self.end_group();
            if self.at(R_PAREN) {
                self.bump();
            } else {
                self.error_at(
                    open_range,
                    "unclosed `(` in the `for` head; expected a matching `)`",
                );
            }
        } else {
            self.error_expected("`(` after `for`");
        }
        self.eat_trivia(true);
        if !self.expression(0) {
            self.error_here("expected a body for the `for` loop");
        }
        self.finish();
    }

    fn while_expr(&mut self) {
        self.start(WHILE_EXPR);
        self.bump();
        self.eat_trivia(true);
        self.condition_parens("while");
        self.eat_trivia(true);
        if !self.expression(0) {
            self.error_here("expected a body for the `while` loop");
        }
        self.finish();
    }

    fn condition_parens(&mut self, construct: &str) {
        if !self.at(L_PAREN) {
            self.error_expected(&format!("`(` after `{construct}`"));
            return;
        }
        let open_range = self.token_range(self.pos);
        self.open_group();
        self.bump();
        self.eat_trivia(true);
        if !self.expression(0) {
            self.error_expected(&format!("a condition inside `{construct} (…)`"));
        }
        self.eat_trivia(true);
        self.end_group();
        if self.at(R_PAREN) {
            self.bump();
        } else {
            self.error_at(
                open_range,
                format!("unclosed `(` in the `{construct}` condition; expected a matching `)`"),
            );
        }
    }

    fn function_def(&mut self) {
        self.start(FUNCTION_DEF);
        let keyword_range = self.token_range(self.pos);
        self.bump();
        self.eat_trivia(true);
        if self.at(LESS) {
            self.inline_type(|parser, end| parser.ann_binder_list(end));
            self.eat_trivia(true);
        }
        if self.at(L_PAREN) {
            self.parameter_list();
        } else {
            self.error_expected("`(` to open the parameter list");
        }
        self.eat_trivia(true);
        if self.at(MINUS_GREATER) {
            self.start(RETURN_TYPE);
            self.bump();
            self.inline_type(|parser, end| parser.ann_type(end, false));
            self.finish();
            self.eat_trivia(true);
        }
        if !self.expression(0) && !self.statement_left_group_open {
            // Blamed on the keyword: where the body search stopped can be a
            // zero-width spot past the last statement.
            self.error_at(keyword_range, "expected a function body");
        }
        self.finish();
    }

    fn parameter_list(&mut self) {
        self.start(PARAMETER_LIST);
        let open_range = self.token_range(self.pos);
        self.open_group();
        self.bump();
        loop {
            self.eat_trivia(true);
            match self.current() {
                None | Some(R_PAREN) => break,
                Some(IDENT | DOTS | DOTDOTI) => {
                    self.start(PARAMETER);
                    self.bump();
                    self.eat_trivia(true);
                    if self.at(COLON) {
                        self.start(TYPE_ASCRIPTION);
                        self.bump();
                        self.inline_type(|parser, end| parser.ann_type(end, false));
                        self.finish();
                        self.eat_trivia(true);
                    }
                    if self.at(EQ) {
                        self.bump();
                        self.eat_trivia(true);
                        match self.current() {
                            Some(COMMA | R_PAREN) | None => {
                                self.error_here("expected a default value after `=`");
                            }
                            _ => {
                                if !self.expression(0) {
                                    self.error_here("expected a default value after `=`");
                                }
                            }
                        }
                    }
                    self.finish();
                    self.eat_trivia(true);
                    if self.at(COMMA) {
                        self.bump();
                    } else if self.crossed_line() && self.starts_new_statement() {
                        break;
                    } else if matches!(self.current(), Some(IDENT | DOTS | DOTDOTI)) {
                        self.error_at(self.separator_gap(), "missing `,` between these parameters");
                    } else if !matches!(self.current(), None | Some(R_PAREN)) {
                        self.error_expected("`,` or `)` in the parameter list");
                        self.recover_argument(R_PAREN);
                        if self.at(COMMA) {
                            self.bump();
                        }
                    }
                }
                _ => {
                    self.error_expected("a parameter name");
                    self.recover_argument(R_PAREN);
                    if self.at(COMMA) {
                        self.bump();
                    }
                }
            }
        }
        self.end_group();
        if self.at(R_PAREN) {
            self.bump();
        } else {
            self.error_at(
                open_range,
                "unclosed `(`; expected `)` to close the parameter list",
            );
            self.statement_left_group_open = true;
        }
        self.finish();
    }

    fn argument_list(&mut self, open: SyntaxKind) {
        self.start(ARGUMENT_LIST);
        let open_range = self.token_range(self.pos);
        self.open_group();
        self.bump();
        let closer = match open {
            L_PAREN => R_PAREN,
            _ => R_BRACKET,
        };
        let mut expect_argument = true;
        loop {
            self.eat_trivia(true);
            match self.current() {
                None => break,
                Some(kind) if kind == closer => break,
                Some(COMMA) => {
                    if expect_argument {
                        self.start(ARGUMENT);
                        self.finish();
                    }
                    self.bump();
                    expect_argument = true;
                }
                Some(kind) => {
                    if !expect_argument {
                        if starts_expression(kind) {
                            // The opener was never closed; reading on would
                            // adopt every later statement as an argument.
                            if self.crossed_line() && self.starts_new_statement() {
                                break;
                            }
                            self.error_at(
                                self.separator_gap(),
                                "missing `,` between these arguments",
                            );
                            self.argument(closer);
                            continue;
                        }
                        self.error_expected(&format!(
                            "`,` or {} in this argument list",
                            closer.display()
                        ));
                        self.recover_argument(closer);
                        continue;
                    }
                    self.argument(closer);
                    expect_argument = false;
                }
            }
        }
        self.end_group();
        if self.at(closer) {
            self.bump();
            if open == L_BRACKET2 {
                self.eat_trivia(true);
                if self.at(R_BRACKET) {
                    self.bump();
                } else {
                    self.error_at(open_range, "expected `]]` to close `[[`");
                }
            }
        } else {
            self.error_at(
                open_range,
                format!(
                    "unclosed {}; expected {} to close it",
                    open.display(),
                    closer.display()
                ),
            );
            self.statement_left_group_open = true;
        }
        self.finish();
    }

    fn argument(&mut self, closer: SyntaxKind) {
        self.start(ARGUMENT);
        // R's grammar admits `NULL` as a tag (the `switch(x, NULL = , …)` idiom).
        let tagged = matches!(
            self.current(),
            Some(IDENT | STRING | DOTS | DOTDOTI | NULL_KW)
        ) && self
            .peek_significant(self.pos + 1, true)
            .is_some_and(|(kind, _)| kind == EQ);
        if tagged {
            match self.current() {
                Some(STRING | NULL_KW) => self.wrap(LITERAL),
                _ => self.wrap(NAME),
            }
            self.eat_trivia(true);
            debug_assert!(self.at(EQ));
            self.bump();
            self.eat_trivia(true);
            match self.current() {
                Some(COMMA) | None => {}
                Some(kind) if kind == closer => {}
                _ => {
                    if !self.expression(0) {
                        self.error_expected("an argument value");
                        self.recover_argument(closer);
                    }
                }
            }
        } else if !self.expression(0) {
            self.error_expected("an argument");
            self.recover_argument(closer);
        }
        self.finish();
    }

    /// Always makes progress: a mismatched closer at the recovery point belongs
    /// to nothing here and is consumed, so the list loop cannot spin.
    fn recover_argument(&mut self, closer: SyntaxKind) {
        let start = self.pos;
        self.start(ERROR);
        self.recover_in_list(closer);
        if self.pos == start
            && !matches!(self.current(), None | Some(COMMA))
            && self.current() != Some(closer)
        {
            self.bump();
            self.recover_in_list(closer);
        }
        self.finish();
    }

    fn recover_in_list(&mut self, closer: SyntaxKind) {
        let mut depth = 0u32;
        while let Some(kind) = self.current() {
            match kind {
                COMMA if depth == 0 => return,
                kind if kind == closer && depth == 0 => return,
                L_PAREN | L_BRACKET | L_BRACKET2 | L_BRACE => {
                    depth += 1;
                    self.bump();
                }
                R_PAREN | R_BRACKET | R_BRACE => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.bump();
                }
                _ => self.bump(),
            }
        }
    }

    fn open_group(&mut self) {
        if let Some(depth) = self.groups.last_mut() {
            *depth += 1;
        }
    }

    fn end_group(&mut self) {
        if let Some(depth) = self.groups.last_mut() {
            *depth = depth.saturating_sub(1);
        }
    }
}

const CALL_BP: u8 = 40;
const NAMESPACE_BP: u8 = 38;
const FIELD_BP: u8 = 36;
const POW_BP: u8 = 34;
const UNARY_SIGN_BP: u8 = 32;
const COLON_BP: u8 = 30;
const SPECIAL_BP: u8 = 28;
const MUL_BP: u8 = 26;
const ADD_BP: u8 = 24;
const COMPARE_BP: u8 = 22;
const UNARY_NOT_BP: u8 = 20;
const AND_BP: u8 = 18;
const OR_BP: u8 = 16;
const TILDE_BP: u8 = 14;
const RIGHT_ASSIGN_BP: u8 = 12;
const EQ_ASSIGN_BP: u8 = 10;
const LEFT_ASSIGN_BP: u8 = 8;
const HELP_BP: u8 = 2;

fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    let (left, right) = match kind {
        QUESTION => (HELP_BP, HELP_BP + 1),
        LESS_MINUS | LESS2_MINUS | COLON_EQ => (LEFT_ASSIGN_BP, LEFT_ASSIGN_BP),
        EQ => (EQ_ASSIGN_BP, EQ_ASSIGN_BP),
        MINUS_GREATER | MINUS_GREATER2 => (RIGHT_ASSIGN_BP, RIGHT_ASSIGN_BP + 1),
        TILDE => (TILDE_BP, TILDE_BP + 1),
        PIPE2 | PIPE => (OR_BP, OR_BP + 1),
        AMP2 | AMP => (AND_BP, AND_BP + 1),
        EQ2 | BANG_EQ | LESS | GREATER | LESS_EQ | GREATER_EQ => (COMPARE_BP, COMPARE_BP + 1),
        PLUS | MINUS => (ADD_BP, ADD_BP + 1),
        STAR | SLASH => (MUL_BP, MUL_BP + 1),
        SPECIAL | PIPE_GREATER => (SPECIAL_BP, SPECIAL_BP + 1),
        COLON => (COLON_BP, COLON_BP + 1),
        CARET => (POW_BP, POW_BP - 1),
        _ => return None,
    };
    Some((left, right))
}

/// Must stay in lockstep with the tokens `primary` accepts.
fn starts_expression(kind: SyntaxKind) -> bool {
    kind.is_keyword() && !matches!(kind, ELSE_KW | IN_KW)
        || matches!(
            kind,
            IDENT
                | DOTS
                | DOTDOTI
                | UNDERSCORE
                | INTEGER
                | DOUBLE
                | COMPLEX
                | STRING
                | RAW_STRING
                | MINUS
                | PLUS
                | BANG
                | TILDE
                | QUESTION
                | L_PAREN
                | L_BRACE
                | BACKSLASH
        )
}
