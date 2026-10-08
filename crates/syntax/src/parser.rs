//! Hand-written recursive-descent / Pratt parser for R.
//!
//! Grammar shape and precedence follow R's own `gram.y`. Newline significance is
//! tracked with a context stack: inside `(`/`[`/`[[` groups newlines are plain
//! trivia; at the top level and directly inside `{ }` a newline ends a syntactically
//! complete statement. `else` may follow a newline only when some group or brace
//! encloses the `if` (exactly R's rule).
//!
//! Every parse produces a lossless tree. Every token is emitted in order,
//! including trivia and the tokens of a malformed region. Recovery wraps an
//! unparsable stretch in an `ERROR` node local to the break.

use crate::kind::SyntaxKind;
use crate::lexer::{Token, lex};
use crate::{Parse, SyntaxError};
use rowan::{Checkpoint, GreenNodeBuilder, TextRange, TextSize};

pub(crate) fn parse(text: &str) -> Parse {
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
        annotation_reported: false,
        annotation_unclosed_reported: false,
        statement_had_lexer_error: false,
        statement_left_group_open: false,
    };
    parser.source_file();
    let green = parser.builder.finish();
    Parse::new(green, parser.errors)
}

/// Expression nesting beyond this refuses further recursion (with a diagnostic)
/// instead of overflowing the stack on adversarial input.
const MAX_DEPTH: u32 = 500;

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    /// Byte offset of every token (one extra entry: total length).
    offsets: Vec<usize>,
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<SyntaxError>,
    /// Newline-significance stack: entering `{` pushes 0, `(`/`[`/`[[` increments
    /// the top, closers undo. Newlines are significant iff the top is 0.
    groups: Vec<u32>,
    depth: u32,
    /// Whether errors are currently raised by the `#:` annotation grammar.
    in_annotation: bool,
    /// Whether the `#:` region being parsed has already reported. The type
    /// grammar cannot resynchronize the way the statement loop can, because a
    /// region is one expression with no boundary inside it to restart at. It
    /// therefore reported every token it could not use, which was nine findings
    /// for a single mistyped optional parameter, all on one line. The first is
    /// the mistake, and the rest are its consequences.
    annotation_reported: bool,
    /// Whether the `#:` region has already reported an unclosed opener. Those
    /// are discovered at the END of the construct they name, so the suppression
    /// above would drop the outermost truth in favour of an inner consequence.
    /// `@type Point {list{x: double` would say only "expected `,` or `}` in
    /// this list type" and never that the `@type` brace is open. One such
    /// structural report gets through regardless.
    annotation_unclosed_reported: bool,
    /// Whether the statement being parsed consumed a lexer `ERROR_TOKEN`.
    /// The lexer's diagnostic is already precise, so the statement loop
    /// suppresses its own boundary report on top of it.
    statement_had_lexer_error: bool,
    /// Whether the statement being parsed left an argument or parameter list
    /// open. Its unclosed-opener error is the whole story, and the newline that
    /// would have ended the statement was consumed inside the list as trivia.
    /// The boundary check therefore neither reports again nor recovers, and the
    /// next line is parsed as the statement it is instead of being swallowed
    /// with its definitions.
    statement_left_group_open: bool,
}

impl Parser<'_> {
    // ---- token access ----

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

    /// The next significant token at or after `pos`, looking through trivia.
    /// `across_newlines` controls whether newlines are looked through.
    fn peek_significant(
        &self,
        mut pos: usize,
        across_newlines: bool,
    ) -> Option<(SyntaxKind, usize)> {
        loop {
            let token = self.tokens.get(pos)?;
            match token.kind {
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => pos += 1,
                SyntaxKind::NEWLINE if across_newlines => pos += 1,
                // An annotation region is trivia for grammar purposes.
                SyntaxKind::ANNOTATION_MARKER => pos = self.annotation_region_end(pos),
                kind => return Some((kind, pos)),
            }
        }
    }

    /// Token index just past the stitched annotation region whose marker is at `pos`.
    fn annotation_region_end(&self, mut pos: usize) -> usize {
        debug_assert_eq!(self.tokens[pos].kind, SyntaxKind::ANNOTATION_MARKER);
        loop {
            // Consume the marker's line.
            while let Some(token) = self.tokens.get(pos) {
                if token.kind == SyntaxKind::NEWLINE {
                    break;
                }
                pos += 1;
            }
            // Stitch when the next line (past one newline and indentation) is `#:` again.
            let Some(token) = self.tokens.get(pos) else {
                return pos;
            };
            debug_assert_eq!(token.kind, SyntaxKind::NEWLINE);
            let mut lookahead = pos + 1;
            while self
                .tokens
                .get(lookahead)
                .is_some_and(|t| t.kind == SyntaxKind::WHITESPACE)
            {
                lookahead += 1;
            }
            if self
                .tokens
                .get(lookahead)
                .is_some_and(|t| t.kind == SyntaxKind::ANNOTATION_MARKER)
            {
                pos = lookahead + 1;
            } else {
                return pos;
            }
        }
    }

    fn newlines_significant(&self) -> bool {
        *self.groups.last().expect("group stack is never empty") == 0
    }

    fn else_allowed_across_newline(&self) -> bool {
        !self.newlines_significant() || self.groups.len() > 1
    }

    // ---- tree building ----

    fn bump(&mut self) {
        let kind = self.current().expect("bump at end of input");
        let text = &self.text[self.offsets[self.pos]..self.offsets[self.pos + 1]];
        self.builder.token(kind.into(), text);
        self.pos += 1;
    }

    fn start(&mut self, kind: SyntaxKind) {
        self.builder.start_node(kind.into());
    }

    fn start_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        self.builder.start_node_at(checkpoint, kind.into());
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    fn checkpoint(&mut self) -> Checkpoint {
        self.builder.checkpoint()
    }

    fn wrap_name(&mut self) {
        self.start(SyntaxKind::NAME);
        self.bump();
        self.finish();
    }

    fn wrap_literal(&mut self) {
        self.start(SyntaxKind::LITERAL);
        self.bump();
        self.finish();
    }

    // ---- errors ----

    fn error_here(&mut self, message: impl Into<String>) {
        let range = self.token_range(self.pos);
        self.error_at(range, message);
    }

    fn error_at(&mut self, range: TextRange, message: impl Into<String>) {
        if self.in_annotation {
            if self.annotation_reported {
                return;
            }
            self.annotation_reported = true;
        }
        self.push_error(range, message);
    }

    /// An unclosed opener inside a `#:` region. It is a structural fact about
    /// the construct it names, so it reports even after the region's first
    /// finding, as `annotation_unclosed_reported` describes. Outside a region
    /// this is an ordinary error.
    fn error_unclosed(&mut self, range: TextRange, message: impl Into<String>) {
        if self.in_annotation {
            if self.annotation_unclosed_reported {
                return;
            }
            self.annotation_unclosed_reported = true;
            self.annotation_reported = true;
            self.push_error(range, message);
            return;
        }
        self.error_at(range, message);
    }

    fn push_error(&mut self, range: TextRange, message: impl Into<String>) {
        let mut error = SyntaxError::new(message, self.on_one_line(range));
        error.in_annotation = self.in_annotation;
        self.errors.push(error);
    }

    /// A blame range never crosses a line break. An error reported *at* the
    /// current token blames that token. At the end of a line, which is most
    /// often the end of a `#:` region, the current token is the newline itself,
    /// whose span runs from the end of one line to the start of the next. An
    /// editor draws that as a squiggle across the break, pointing at neither
    /// line.
    ///
    /// Such a range collapses onto the last character of code on its own line,
    /// which is where the reader has to look anyway. Trailing whitespace is
    /// skipped so the caret lands on the code and not beside it.
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

    /// The empty range just past the last significant token before the current
    /// position, which is where a missing separator belongs. Anchoring there
    /// keeps the report on the line of the element it follows, instead of on
    /// whatever line the next element starts.
    fn separator_gap(&self) -> TextRange {
        let end = (0..self.pos)
            .rev()
            .find(|&index| {
                !matches!(
                    self.tokens[index].kind,
                    SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE
                )
            })
            .map(|index| self.token_range(index).end())
            .unwrap_or_else(|| self.token_range(self.pos).start());
        TextRange::empty(end)
    }

    /// Describe the current token for an "expected …, found …" message.
    fn describe_current(&self) -> String {
        match self.current() {
            None => "end of file".to_owned(),
            Some(SyntaxKind::NEWLINE) => "end of line".to_owned(),
            Some(kind) => kind.display().to_owned(),
        }
    }

    // ---- trivia ----

    /// Whether the current token opens what is unmistakably a new statement: a
    /// name bound by `<-`/`<<-`, or a keyword that can only begin one.
    ///
    /// This is what separates an unclosed opener from a forgotten separator
    /// when a list runs onto the next line. A genuine continuation reads as a
    /// fragment, such as `beta)` or `y) x`. A line that *assigns* is the next
    /// statement instead, and adopting it into the list swallows its definition
    /// along with every line after.
    ///
    /// `=` is deliberately not an assignment here. Inside an argument or
    /// parameter list `name = value` is a named argument, which is what a line
    /// missing its comma looks like. That is the far likelier reading, and it
    /// is the one R itself takes, because R keeps consuming until the opener
    /// closes.
    fn starts_new_statement(&self) -> bool {
        match self.current() {
            Some(
                SyntaxKind::IF_KW
                | SyntaxKind::FOR_KW
                | SyntaxKind::WHILE_KW
                | SyntaxKind::REPEAT_KW,
            ) => true,
            Some(SyntaxKind::IDENT | SyntaxKind::STRING) => matches!(
                self.peek_significant(self.pos + 1, false)
                    .map(|(kind, _)| kind),
                Some(SyntaxKind::LESS_MINUS | SyntaxKind::LESS2_MINUS)
            ),
            _ => false,
        }
    }

    /// Whether a line break separates the current token from the significant
    /// one before it. Scans backwards: the newline is consumed while parsing
    /// the element that precedes the loop head asking this, not at the head.
    fn crossed_line(&self) -> bool {
        for index in (0..self.pos).rev() {
            match self.tokens[index].kind {
                SyntaxKind::NEWLINE => return true,
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => continue,
                _ => return false,
            }
        }
        false
    }

    /// Emit trivia into the tree. Newlines are consumed only when insignificant
    /// in the current context or explicitly allowed by `across_newlines`.
    fn eat_trivia(&mut self, across_newlines: bool) {
        loop {
            match self.current() {
                Some(SyntaxKind::WHITESPACE | SyntaxKind::COMMENT) => self.bump(),
                Some(SyntaxKind::NEWLINE) if across_newlines || !self.newlines_significant() => {
                    self.bump()
                }
                Some(SyntaxKind::ANNOTATION_MARKER) => self.annotation(),
                _ => return,
            }
        }
    }

    /// One stitched `#:` annotation region as a first-class node containing the
    /// parsed annotation grammar: `@` directives and type expressions, with
    /// markers, newlines, and comments inside the region acting as trivia (so a
    /// type may continue across stitched `#:` lines).
    fn annotation(&mut self) {
        self.start(SyntaxKind::ANNOTATION);
        self.in_annotation = true;
        self.annotation_reported = false;
        self.annotation_unclosed_reported = false;
        let end = self.annotation_region_end(self.pos);
        loop {
            self.ann_trivia(end);
            if self.pos >= end {
                break;
            }
            let before = self.pos;
            if self.at(SyntaxKind::AT) {
                self.ann_directive(end);
            } else {
                self.ann_type(end, true);
            }
            // Malformed bodies may consume nothing; never spin in place.
            if self.pos == before {
                self.bump();
            }
        }
        self.in_annotation = false;
        self.finish();
    }

    // ---- annotation grammar ----
    //
    // The type-notation grammar inside `#:` regions: an optional outermost
    // binder list `<T, U: numeric>`, unions `A | B`, `(TYPE)` groups,
    // `list[...]` / `list{...}`, `fn(...) -> T`, named/generic types with
    // dotted names, `T[]` / `T[named]` vector suffixes, and the `@` directives
    // (`@type`, `@alias`, `@param`, `@return(s)`, `@forall`, `@trust`, `@new`,
    // `@if-unknown`, `@strict`). Structure and spans are the parser's job;
    // vocabulary semantics (constraint names, duplicate checks) belong to the
    // semantics layer.

    /// Trivia inside an annotation region: whitespace, newlines, further `#:`
    /// markers, and comments.
    fn ann_trivia(&mut self, end: usize) {
        while self.pos < end {
            match self.current() {
                Some(
                    SyntaxKind::WHITESPACE
                    | SyntaxKind::NEWLINE
                    | SyntaxKind::ANNOTATION_MARKER
                    | SyntaxKind::COMMENT,
                ) => self.bump(),
                _ => return,
            }
        }
    }

    /// True when the token at `pos` starts exactly where the previous one ended
    /// with no interleaving trivia. This joins a multi-token directive name.
    fn adjacent(&self, pos: usize) -> bool {
        pos > 0
            && pos < self.tokens.len()
            && self.offsets[pos] == self.offsets[pos - 1] + usize::from(self.tokens[pos - 1].len)
    }

    fn ann_directive(&mut self, end: usize) {
        self.start(SyntaxKind::ANNOTATION_DIRECTIVE);
        debug_assert!(self.at(SyntaxKind::AT));
        self.bump();
        // Directive names may span several tokens when adjacent (`@if-unknown`
        // lexes as `if` `-` `unknown`); every name token must touch its
        // predecessor (the first one touches the `@`).
        let name_start = self.pos;
        let mut name = String::new();
        while self.pos < end {
            let joinable = match self.current() {
                Some(SyntaxKind::IDENT | SyntaxKind::MINUS) => true,
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
                if matches!(self.current(), Some(SyntaxKind::IDENT)) && self.pos < end {
                    self.wrap_name();
                } else {
                    self.error_here(format!("expected a type name after `@{name}`"));
                }
                self.ann_trivia(end);
                // Optional `<T, U>` parameter list.
                if self.at(SyntaxKind::LESS) && self.pos < end {
                    self.ann_binder_list(end);
                }
                self.ann_trivia(end);
                self.ann_braced_type(end, &name);
            }
            "param" => {
                self.ann_trivia(end);
                match self.current() {
                    Some(SyntaxKind::IDENT | SyntaxKind::DOTS) if self.pos < end => {
                        self.wrap_name()
                    }
                    Some(SyntaxKind::L_BRACKET) if self.pos < end => {
                        // Optional parameter: `[name]`.
                        self.bump();
                        self.ann_trivia(end);
                        if matches!(self.current(), Some(SyntaxKind::IDENT)) && self.pos < end {
                            self.wrap_name();
                        } else {
                            self.error_here("expected a parameter name inside `[...]`");
                        }
                        self.ann_trivia(end);
                        if self.at(SyntaxKind::R_BRACKET) && self.pos < end {
                            self.bump();
                        } else {
                            self.error_here("expected `]` after the optional parameter name");
                        }
                    }
                    // JSDoc writes the type first. This directive writes the
                    // name first, so say which form to use instead of only
                    // reporting the missing name.
                    Some(SyntaxKind::L_BRACE) if self.pos < end => self.error_here(
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
                if !matches!(self.current(), Some(SyntaxKind::IDENT)) || self.pos >= end {
                    self.error_here("expected a type parameter name after `@forall`");
                    break;
                }
                self.ann_binder(end);
                self.ann_trivia(end);
                if self.at(SyntaxKind::COMMA) && self.pos < end {
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
                // Optional `off`.
                if matches!(self.current(), Some(SyntaxKind::IDENT))
                    && self.pos < end
                    && self.token_str(self.pos) == "off"
                {
                    self.bump();
                }
            }
            _ => {
                let range = TextRange::new(
                    self.token_range(name_start).start(),
                    self.token_range(self.pos.saturating_sub(1)).end(),
                );
                self.error_at(range, format!("unknown annotation directive `@{name}`"));
                // Preserve the rest of the region inside the directive node.
                while self.pos < end {
                    self.bump();
                }
            }
        }
        self.finish();
    }

    /// `{ TYPE }` after a directive head.
    fn ann_braced_type(&mut self, end: usize, directive: &str) {
        if self.at(SyntaxKind::L_BRACE) && self.pos < end {
            let open_range = self.token_range(self.pos);
            self.bump();
            self.ann_trivia(end);
            if self.at(SyntaxKind::R_BRACE) || self.pos >= end {
                self.error_here(format!(
                    "expected a type inside `{{...}}` of `@{directive}`"
                ));
            } else {
                // A directive payload is not the outermost level of the
                // annotation, so a binder here is refused like any other
                // higher-rank position: `@forall` is where the expanded form
                // declares type parameters, and `@type Name<T>` puts them on
                // the name. Allowing them here accepted `@param f {<T> fn(T) ->
                // T}` in silence and bound nothing.
                self.ann_type(end, false);
            }
            self.ann_trivia(end);
            if self.at(SyntaxKind::R_BRACE) && self.pos < end {
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

    /// A full type expression; `allow_binders` permits an outermost
    /// `<T, U: numeric>` binder list.
    fn ann_type(&mut self, end: usize, allow_binders: bool) {
        if allow_binders && self.at(SyntaxKind::LESS) && self.pos < end {
            self.ann_binder_list(end);
            self.ann_trivia(end);
        }
        self.ann_union(end);
    }

    fn ann_binder_list(&mut self, end: usize) {
        self.start(SyntaxKind::TYPE_BINDER_LIST);
        debug_assert!(self.at(SyntaxKind::LESS));
        self.bump();
        loop {
            self.ann_trivia(end);
            match self.current() {
                Some(SyntaxKind::GREATER) if self.pos < end => {
                    self.bump();
                    break;
                }
                Some(SyntaxKind::IDENT) if self.pos < end => {
                    self.ann_binder(end);
                    self.ann_trivia(end);
                    if self.at(SyntaxKind::COMMA) && self.pos < end {
                        self.bump();
                    } else if self.at(SyntaxKind::GREATER) && self.pos < end {
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

    /// `T` or `T: constraint` inside binder lists and `@forall`.
    fn ann_binder(&mut self, end: usize) {
        self.start(SyntaxKind::TYPE_BINDER);
        self.wrap_name();
        self.ann_trivia(end);
        if self.at(SyntaxKind::COLON) && self.pos < end {
            self.bump();
            self.ann_trivia(end);
            if matches!(self.current(), Some(SyntaxKind::IDENT)) && self.pos < end {
                // A constraint may be spelled in more than one word (`scalar
                // numeric`), so take the whole run. Naming the constraints is
                // the lowering's job; the grammar only has to deliver them.
                // Trivia is consumed only *between* words. Taking it after
                // the last word would pull the block's `#:` continuation
                // marker into the binder.
                while self.ann_next_significant(end) == Some(SyntaxKind::IDENT) {
                    self.wrap_name();
                    self.ann_trivia(end);
                }
                self.wrap_name();
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
            if self.at(SyntaxKind::PIPE) && self.pos < end {
                if members == 1 {
                    self.start_at(checkpoint, SyntaxKind::TYPE_UNION);
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
            Some(SyntaxKind::LESS) => {
                self.error_here("higher-rank polymorphism is not supported: type parameter binders may only appear at the outermost level");
                // Recover by reading the type as if the binder were not there:
                // consuming the binder alone leaves this position with no type
                // at all, so the enclosing parameter list then fails on the
                // type that follows and the one real finding arrives buried
                // under three consequences of it. The refused binder keeps an
                // `ERROR` wrapper so lowering can tell that this block was
                // refused. Its names bind nothing, and reporting each as an
                // unknown type would blame the author twice for one
                // mistake.
                self.start(SyntaxKind::ERROR);
                self.ann_binder_list(end);
                self.finish();
                self.ann_trivia(end);
                self.ann_union(end);
                return;
            }
            Some(SyntaxKind::L_PAREN) => {
                self.start(SyntaxKind::TYPE_PAREN);
                let open_range = self.token_range(self.pos);
                self.bump();
                self.ann_trivia(end);
                self.ann_type(end, false);
                self.ann_trivia(end);
                if self.at(SyntaxKind::R_PAREN) && self.pos < end {
                    self.bump();
                } else {
                    self.error_unclosed(
                        open_range,
                        "unclosed `(` in this type; expected a matching `)`",
                    );
                }
                self.finish();
            }
            Some(SyntaxKind::IDENT | SyntaxKind::NULL_KW) => match self.token_str(self.pos) {
                "list" if self.ann_next_significant(end) == Some(SyntaxKind::L_BRACKET) => {
                    self.ann_list_brackets(end);
                }
                "list" if self.ann_next_significant(end) == Some(SyntaxKind::L_BRACE) => {
                    self.ann_list_braces(end);
                }
                "fn" if self.ann_next_significant(end) == Some(SyntaxKind::L_PAREN) => {
                    self.ann_function_type(end);
                }
                _ => {
                    if self.ann_next_significant(end) == Some(SyntaxKind::LESS) {
                        self.start(SyntaxKind::TYPE_APPLY);
                        self.wrap_name();
                        self.ann_trivia(end);
                        self.ann_type_arg_list(end);
                        self.finish();
                    } else {
                        self.start(SyntaxKind::TYPE_REF);
                        self.bump();
                        self.finish();
                    }
                }
            },
            _ => {
                let description = self.describe_current();
                self.error_here(format!("expected a type, found {description}"));
                return;
            }
        }
        // Vector suffixes: `T[]` and `T[named]`, repeatable.
        loop {
            self.ann_trivia(end);
            if !self.at(SyntaxKind::L_BRACKET) || self.pos >= end {
                break;
            }
            // Only `[]` and `[named]` are suffixes; anything else is not ours.
            let after = self.peek_significant(self.pos + 1, true);
            let is_suffix = match after {
                Some((SyntaxKind::R_BRACKET, _)) => true,
                Some((SyntaxKind::IDENT, ident_pos)) => {
                    self.token_str(ident_pos) == "named"
                        && matches!(
                            self.peek_significant(ident_pos + 1, true),
                            Some((SyntaxKind::R_BRACKET, _))
                        )
                }
                _ => false,
            };
            if !is_suffix {
                break;
            }
            self.start_at(checkpoint, SyntaxKind::TYPE_VECTOR);
            self.bump();
            self.ann_trivia(end);
            if matches!(self.current(), Some(SyntaxKind::IDENT)) {
                self.bump();
                self.ann_trivia(end);
            }
            if self.at(SyntaxKind::R_BRACKET) && self.pos < end {
                self.bump();
            }
            self.finish();
        }
    }

    fn ann_next_significant(&self, end: usize) -> Option<SyntaxKind> {
        let (kind, pos) = self.peek_significant(self.pos + 1, true)?;
        (pos < end).then_some(kind)
    }

    fn ann_type_arg_list(&mut self, end: usize) {
        self.start(SyntaxKind::TYPE_ARG_LIST);
        debug_assert!(self.at(SyntaxKind::LESS));
        self.bump();
        loop {
            self.ann_trivia(end);
            match self.current() {
                Some(SyntaxKind::GREATER) if self.pos < end => {
                    self.bump();
                    break;
                }
                _ if self.pos >= end => {
                    self.error_here("expected `>` to close the type argument list");
                    break;
                }
                _ => {
                    self.ann_type(end, false);
                    self.ann_trivia(end);
                    if self.at(SyntaxKind::COMMA) && self.pos < end {
                        self.bump();
                    } else if self.at(SyntaxKind::GREATER) && self.pos < end {
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

    /// `list[T]` / `list[named: T]`.
    fn ann_list_brackets(&mut self, end: usize) {
        self.start(SyntaxKind::TYPE_LIST);
        self.wrap_name();
        self.ann_trivia(end);
        debug_assert!(self.at(SyntaxKind::L_BRACKET));
        let open_range = self.token_range(self.pos);
        self.bump();
        self.ann_trivia(end);
        // `named:` prefix.
        if matches!(self.current(), Some(SyntaxKind::IDENT))
            && self.pos < end
            && self.token_str(self.pos) == "named"
            && self.ann_next_significant(end) == Some(SyntaxKind::COLON)
        {
            self.bump();
            self.ann_trivia(end);
            self.bump();
            self.ann_trivia(end);
        }
        self.ann_type(end, false);
        self.ann_trivia(end);
        if self.at(SyntaxKind::R_BRACKET) && self.pos < end {
            self.bump();
        } else {
            self.error_unclosed(
                open_range,
                "unclosed `[` in this list type; expected a matching `]`",
            );
        }
        self.finish();
    }

    /// `list{A, B}` (tuple) / `list{a: A, b: B}` (record).
    fn ann_list_braces(&mut self, end: usize) {
        let checkpoint = self.checkpoint();
        self.wrap_name();
        self.ann_trivia(end);
        debug_assert!(self.at(SyntaxKind::L_BRACE));
        let open_range = self.token_range(self.pos);
        self.bump();
        let mut is_record = false;
        let mut first = true;
        loop {
            self.ann_trivia(end);
            if self.at(SyntaxKind::R_BRACE) && self.pos < end {
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
            let named_field = matches!(self.current(), Some(SyntaxKind::IDENT))
                && self.ann_next_significant(end) == Some(SyntaxKind::COLON);
            if first {
                is_record = named_field;
                first = false;
            }
            if named_field {
                self.start(SyntaxKind::TYPE_FIELD);
                self.wrap_name();
                self.ann_trivia(end);
                self.bump();
                self.ann_trivia(end);
                self.ann_type(end, false);
                self.finish();
            } else {
                self.ann_type(end, false);
            }
            self.ann_trivia(end);
            if self.at(SyntaxKind::COMMA) && self.pos < end {
                self.bump();
            } else if self.at(SyntaxKind::R_BRACE) && self.pos < end {
                self.bump();
                break;
            } else {
                self.error_here("expected `,` or `}` in this list type");
                break;
            }
        }
        self.start_at(
            checkpoint,
            if is_record {
                SyntaxKind::TYPE_RECORD
            } else {
                SyntaxKind::TYPE_TUPLE
            },
        );
        self.finish();
    }

    /// `fn(params) -> RET` (the return type is optional).
    fn ann_function_type(&mut self, end: usize) {
        self.start(SyntaxKind::TYPE_FUNCTION);
        self.wrap_name();
        self.ann_trivia(end);
        debug_assert!(self.at(SyntaxKind::L_PAREN));
        self.start(SyntaxKind::TYPE_PARAMETER_LIST);
        let open_range = self.token_range(self.pos);
        self.bump();
        loop {
            self.ann_trivia(end);
            if self.at(SyntaxKind::R_PAREN) && self.pos < end {
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
            if self.at(SyntaxKind::COMMA) && self.pos < end {
                self.bump();
            } else if self.at(SyntaxKind::R_PAREN) && self.pos < end {
                self.bump();
                break;
            } else {
                self.error_here("expected `,` or `)` in this function type's parameters");
                break;
            }
        }
        self.finish();
        self.ann_trivia(end);
        if self.at(SyntaxKind::MINUS_GREATER) && self.pos < end {
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
        self.start(SyntaxKind::TYPE_PARAMETER);
        match self.current() {
            // Rest parameter: `...`, `...name`, `...: TYPE`, `...name: TYPE`.
            Some(SyntaxKind::DOTS) => {
                self.bump();
                if matches!(self.current(), Some(SyntaxKind::IDENT))
                    && self.adjacent(self.pos)
                    && self.pos < end
                {
                    self.wrap_name();
                }
                self.ann_trivia(end);
                if self.at(SyntaxKind::COLON) && self.pos < end {
                    self.bump();
                    self.ann_trivia(end);
                    self.ann_type(end, false);
                }
            }
            // Optional named parameter: `[name]: TYPE`.
            Some(SyntaxKind::L_BRACKET) => {
                self.bump();
                self.ann_trivia(end);
                if matches!(self.current(), Some(SyntaxKind::IDENT)) && self.pos < end {
                    self.wrap_name();
                } else {
                    self.error_here("expected `[name]: TYPE` for an optional parameter");
                }
                self.ann_trivia(end);
                if self.at(SyntaxKind::R_BRACKET) && self.pos < end {
                    self.bump();
                } else {
                    self.error_here("expected `]` after the optional parameter name");
                }
                self.ann_trivia(end);
                if self.at(SyntaxKind::COLON) && self.pos < end {
                    self.bump();
                    self.ann_trivia(end);
                    self.ann_type(end, false);
                } else {
                    self.error_here("expected `:` after `[name]` in an optional parameter");
                }
            }
            // Named parameter `name: TYPE` or a positional type.
            Some(SyntaxKind::IDENT)
                if self.ann_next_significant(end) == Some(SyntaxKind::COLON) =>
            {
                self.wrap_name();
                self.ann_trivia(end);
                self.bump();
                self.ann_trivia(end);
                self.ann_type(end, false);
            }
            _ => self.ann_type(end, false),
        }
        self.finish();
    }

    // ---- statements ----

    fn source_file(&mut self) {
        self.start(SyntaxKind::SOURCE_FILE);
        self.statements(None);
        // Defensive: anything the statement loop could not consume.
        if self.pos < self.tokens.len() {
            self.start(SyntaxKind::ERROR);
            while self.pos < self.tokens.len() {
                self.bump();
            }
            self.finish();
        }
        self.finish();
    }

    /// Statement sequence until `terminator` (or end of file). Used for the
    /// source file (`None`) and brace bodies (`Some(R_BRACE)`).
    fn statements(&mut self, terminator: Option<SyntaxKind>) {
        loop {
            self.eat_trivia(true);
            self.statement_had_lexer_error = false;
            self.statement_left_group_open = false;
            match self.current() {
                Some(SyntaxKind::SEMICOLON) => {
                    self.bump();
                    continue;
                }
                None => return,
                Some(kind) if Some(kind) == terminator => return,
                _ => {}
            }

            let statement_start = self.pos;
            if !self.expression(0) {
                match self.current() {
                    Some(SyntaxKind::ELSE_KW) => self.error_here(
                        "unexpected `else`: it must stay on the same line as the `if` branch it belongs to (or the `if` must be inside braces)",
                    ),
                    Some(SyntaxKind::R_BRACE) if terminator.is_none() => {
                        self.error_here("unmatched `}`: no `{` is open here")
                    }
                    _ => {
                        let description = self.describe_current();
                        self.error_here(format!("expected a statement, found {description}"));
                    }
                }
                // Consume through the end of the line so one broken construct
                // stays local.
                self.start(SyntaxKind::ERROR);
                self.recover_to_statement_boundary(terminator);
                self.finish();
                continue;
            }

            // After a complete statement: a newline, `;`, the terminator, or EOF.
            self.eat_trivia(false);
            match self.current() {
                None | Some(SyntaxKind::NEWLINE | SyntaxKind::SEMICOLON) => {}
                Some(kind) if Some(kind) == terminator => {}
                Some(_) if self.statement_left_group_open => continue,
                Some(kind) => {
                    // A statement that already contains a lexer error owns
                    // this whole broken region; re-reporting its tail as
                    // "unexpected ..." would double up on one mistake.
                    if kind != SyntaxKind::ERROR_TOKEN && !self.statement_had_lexer_error {
                        // `return x` deserves better than the generic
                        // boundary error: `return` reads like a keyword but
                        // is a function in R, and the fix is parentheses.
                        if self.statement_ends_with_bare_return(statement_start)
                            && Self::starts_expression(kind)
                        {
                            self.error_here("`return` needs parentheses: `return(x)`");
                        } else {
                            let description = self.describe_current();
                            self.error_here(format!(
                                "unexpected {description} after this expression; expected a newline or `;`"
                            ));
                        }
                    }
                    self.start(SyntaxKind::ERROR);
                    self.recover_to_statement_boundary(terminator);
                    self.finish();
                }
            }
        }
    }

    /// Whether the statement whose tokens began at `start` ENDS in the bare
    /// name `return`, which is the classic missing-parentheses mistake.
    /// `return x` and `f <- function() return TRUE` are such statements. It
    /// earns a targeted message because
    /// `return` reads like a keyword but is a function in R. A `return`
    /// reached through an access operator (`x$return`, `pkg::return`) is an
    /// ordinary field or export, not the builtin, and keeps the generic
    /// error.
    fn statement_ends_with_bare_return(&self, start: usize) -> bool {
        let mut last: Option<(usize, SyntaxKind)> = None;
        let mut before: Option<SyntaxKind> = None;
        for (offset, token) in self
            .tokens
            .get(start..self.pos)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if matches!(
                token.kind,
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE
            ) {
                continue;
            }
            before = last.map(|(_, kind)| kind);
            last = Some((offset, token.kind));
        }
        let Some((last_offset, last_kind)) = last else {
            return false;
        };
        last_kind == SyntaxKind::IDENT
            && self.token_str(start + last_offset) == "return"
            && !matches!(
                before,
                Some(SyntaxKind::DOLLAR | SyntaxKind::AT | SyntaxKind::COLON2 | SyntaxKind::COLON3)
            )
    }

    fn recover_to_statement_boundary(&mut self, terminator: Option<SyntaxKind>) {
        while let Some(kind) = self.current() {
            if kind == SyntaxKind::NEWLINE
                || kind == SyntaxKind::SEMICOLON
                || Some(kind) == terminator
            {
                return;
            }
            self.bump();
        }
    }

    // ---- expressions ----

    /// Pratt loop. It returns false, emitting nothing and consuming nothing,
    /// when the current token cannot start an expression. The caller reports
    /// the context-specific error.
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

        loop {
            self.eat_trivia(false);
            let Some(kind) = self.current() else { break };
            match kind {
                // Postfix: calls and subscripts (highest precedence).
                SyntaxKind::L_PAREN if CALL_BP >= min_bp => {
                    self.start_at(checkpoint, SyntaxKind::CALL_EXPR);
                    self.argument_list(SyntaxKind::L_PAREN);
                    self.finish();
                }
                SyntaxKind::L_BRACKET if CALL_BP >= min_bp => {
                    self.start_at(checkpoint, SyntaxKind::SUBSET_EXPR);
                    self.argument_list(SyntaxKind::L_BRACKET);
                    self.finish();
                }
                SyntaxKind::L_BRACKET2 if CALL_BP >= min_bp => {
                    self.start_at(checkpoint, SyntaxKind::SUBSET2_EXPR);
                    self.argument_list(SyntaxKind::L_BRACKET2);
                    self.finish();
                }
                // `$` / `@` and `::` / `:::` take a name (or string) on the right.
                SyntaxKind::DOLLAR | SyntaxKind::AT if FIELD_BP >= min_bp => {
                    let node = if kind == SyntaxKind::DOLLAR {
                        SyntaxKind::DOLLAR_EXPR
                    } else {
                        SyntaxKind::AT_EXPR
                    };
                    self.start_at(checkpoint, node);
                    self.bump();
                    self.eat_trivia(true);
                    self.field_name(kind);
                    self.finish();
                }
                SyntaxKind::COLON2 | SyntaxKind::COLON3 if NAMESPACE_BP >= min_bp => {
                    self.start_at(checkpoint, SyntaxKind::NAMESPACE_EXPR);
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
                    self.start_at(checkpoint, SyntaxKind::BINARY_EXPR);
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
        true
    }

    /// A name or string after `$`, `@`, `::`, `:::`.
    fn field_name(&mut self, operator: SyntaxKind) {
        match self.current() {
            Some(SyntaxKind::IDENT | SyntaxKind::DOTS | SyntaxKind::DOTDOTI) => self.wrap_name(),
            Some(SyntaxKind::STRING) => self.wrap_literal(),
            _ => {
                let description = self.describe_current();
                self.error_here(format!(
                    "expected a name after {}, found {description}",
                    operator.display()
                ));
            }
        }
    }

    /// Prefix operators and primary expressions. Quiet on failure: consumes and
    /// emits nothing, returns false.
    /// Whether `kind` can begin an expression. This is kept in lockstep with
    /// the entry set of `primary`, plus the sign, not, and formula prefixes
    /// that `primary` dispatches to `unary`.
    fn starts_expression(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::IDENT
                | SyntaxKind::DOTS
                | SyntaxKind::DOTDOTI
                | SyntaxKind::UNDERSCORE
                | SyntaxKind::INTEGER
                | SyntaxKind::DOUBLE
                | SyntaxKind::COMPLEX
                | SyntaxKind::STRING
                | SyntaxKind::RAW_STRING
                | SyntaxKind::TRUE_KW
                | SyntaxKind::FALSE_KW
                | SyntaxKind::NULL_KW
                | SyntaxKind::INF_KW
                | SyntaxKind::NAN_KW
                | SyntaxKind::NA_KW
                | SyntaxKind::NA_INTEGER_KW
                | SyntaxKind::NA_REAL_KW
                | SyntaxKind::NA_COMPLEX_KW
                | SyntaxKind::NA_CHARACTER_KW
                | SyntaxKind::MINUS
                | SyntaxKind::PLUS
                | SyntaxKind::BANG
                | SyntaxKind::TILDE
                | SyntaxKind::QUESTION
                | SyntaxKind::L_PAREN
                | SyntaxKind::L_BRACE
                | SyntaxKind::IF_KW
                | SyntaxKind::FOR_KW
                | SyntaxKind::WHILE_KW
                | SyntaxKind::REPEAT_KW
                | SyntaxKind::FUNCTION_KW
                | SyntaxKind::BACKSLASH
                | SyntaxKind::BREAK_KW
                | SyntaxKind::NEXT_KW
        )
    }

    fn primary(&mut self) -> bool {
        let Some(kind) = self.current() else {
            return false;
        };
        match kind {
            SyntaxKind::IDENT | SyntaxKind::DOTS | SyntaxKind::DOTDOTI | SyntaxKind::UNDERSCORE => {
                self.wrap_name()
            }
            // The lexer already diagnosed this token precisely; consume it as
            // a placeholder atom so no second report stacks on top.
            SyntaxKind::ERROR_TOKEN => {
                self.statement_had_lexer_error = true;
                self.start(SyntaxKind::ERROR);
                self.bump();
                self.finish();
            }
            SyntaxKind::INTEGER
            | SyntaxKind::DOUBLE
            | SyntaxKind::COMPLEX
            | SyntaxKind::STRING
            | SyntaxKind::RAW_STRING
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
            | SyntaxKind::NULL_KW
            | SyntaxKind::INF_KW
            | SyntaxKind::NAN_KW
            | SyntaxKind::NA_KW
            | SyntaxKind::NA_INTEGER_KW
            | SyntaxKind::NA_REAL_KW
            | SyntaxKind::NA_COMPLEX_KW
            | SyntaxKind::NA_CHARACTER_KW => self.wrap_literal(),
            SyntaxKind::MINUS | SyntaxKind::PLUS => self.unary(UNARY_SIGN_BP),
            SyntaxKind::BANG => self.unary(UNARY_NOT_BP),
            SyntaxKind::TILDE => self.unary(TILDE_BP + 1),
            SyntaxKind::QUESTION => self.unary(HELP_BP + 1),
            SyntaxKind::L_PAREN => {
                self.start(SyntaxKind::PAREN_EXPR);
                let open_range = self.token_range(self.pos);
                self.open_group();
                self.bump();
                self.eat_trivia(true);
                if !self.expression(0) {
                    let description = self.describe_current();
                    self.error_here(format!(
                        "expected an expression inside `(`, found {description}"
                    ));
                }
                self.eat_trivia(true);
                self.end_group();
                if self.at(SyntaxKind::R_PAREN) {
                    self.bump();
                } else {
                    self.error_at(open_range, "unclosed `(`; expected a matching `)`");
                }
                self.finish();
            }
            SyntaxKind::L_BRACE => {
                self.start(SyntaxKind::BRACE_EXPR);
                let open_range = self.token_range(self.pos);
                self.groups.push(0);
                self.bump();
                self.statements(Some(SyntaxKind::R_BRACE));
                self.groups.pop();
                if self.at(SyntaxKind::R_BRACE) {
                    self.bump();
                } else {
                    self.error_at(open_range, "unclosed `{`; expected a matching `}`");
                }
                self.finish();
            }
            SyntaxKind::IF_KW => self.if_expr(),
            SyntaxKind::FOR_KW => self.for_expr(),
            SyntaxKind::WHILE_KW => self.while_expr(),
            SyntaxKind::REPEAT_KW => {
                self.start(SyntaxKind::REPEAT_EXPR);
                self.bump();
                self.eat_trivia(true);
                if !self.expression(0) {
                    self.error_here("expected a body after `repeat`");
                }
                self.finish();
            }
            SyntaxKind::FUNCTION_KW | SyntaxKind::BACKSLASH => self.function_def(),
            SyntaxKind::BREAK_KW => {
                self.start(SyntaxKind::BREAK_EXPR);
                self.bump();
                self.finish();
            }
            SyntaxKind::NEXT_KW => {
                self.start(SyntaxKind::NEXT_EXPR);
                self.bump();
                self.finish();
            }
            _ => return false,
        }
        true
    }

    fn unary(&mut self, right_bp: u8) {
        self.start(SyntaxKind::UNARY_EXPR);
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
        self.start(SyntaxKind::IF_EXPR);
        self.bump();
        self.eat_trivia(true);
        self.condition_parens("if");
        self.eat_trivia(true);
        if !self.expression(0) {
            let description = self.describe_current();
            self.error_here(format!(
                "expected a branch after the `if` condition, found {description}"
            ));
        }
        // `else` may follow a newline only inside a group or brace context.
        let across = self.else_allowed_across_newline();
        if let Some((SyntaxKind::ELSE_KW, _)) = self.peek_significant(self.pos, across) {
            self.eat_trivia(across);
            debug_assert!(self.at(SyntaxKind::ELSE_KW));
            self.bump();
            self.eat_trivia(true);
            if !self.expression(0) {
                let description = self.describe_current();
                self.error_here(format!(
                    "expected an expression after `else`, found {description}"
                ));
            }
        }
        self.finish();
    }

    fn for_expr(&mut self) {
        self.start(SyntaxKind::FOR_EXPR);
        self.bump();
        self.eat_trivia(true);
        if self.at(SyntaxKind::L_PAREN) {
            let open_range = self.token_range(self.pos);
            self.open_group();
            self.bump();
            self.eat_trivia(true);
            if self.at(SyntaxKind::IDENT) {
                self.wrap_name();
            } else {
                let description = self.describe_current();
                self.error_here(format!(
                    "expected a loop variable after `for (`, found {description}"
                ));
            }
            self.eat_trivia(true);
            if self.at(SyntaxKind::IN_KW) {
                self.bump();
            } else {
                let description = self.describe_current();
                self.error_here(format!(
                    "expected `in` in the `for` head, found {description}"
                ));
            }
            self.eat_trivia(true);
            if !self.expression(0) {
                let description = self.describe_current();
                self.error_here(format!(
                    "expected a sequence expression in the `for` head, found {description}"
                ));
            }
            self.eat_trivia(true);
            self.end_group();
            if self.at(SyntaxKind::R_PAREN) {
                self.bump();
            } else {
                self.error_at(
                    open_range,
                    "unclosed `(` in the `for` head; expected a matching `)`",
                );
            }
        } else {
            let description = self.describe_current();
            self.error_here(format!("expected `(` after `for`, found {description}"));
        }
        self.eat_trivia(true);
        if !self.expression(0) {
            self.error_here("expected a body for the `for` loop");
        }
        self.finish();
    }

    fn while_expr(&mut self) {
        self.start(SyntaxKind::WHILE_EXPR);
        self.bump();
        self.eat_trivia(true);
        self.condition_parens("while");
        self.eat_trivia(true);
        if !self.expression(0) {
            self.error_here("expected a body for the `while` loop");
        }
        self.finish();
    }

    /// `( expr )` after `if` / `while`.
    fn condition_parens(&mut self, construct: &str) {
        if !self.at(SyntaxKind::L_PAREN) {
            let description = self.describe_current();
            self.error_here(format!(
                "expected `(` after `{construct}`, found {description}"
            ));
            return;
        }
        let open_range = self.token_range(self.pos);
        self.open_group();
        self.bump();
        self.eat_trivia(true);
        if !self.expression(0) {
            let description = self.describe_current();
            self.error_here(format!(
                "expected a condition inside `{construct} (…)`, found {description}"
            ));
        }
        self.eat_trivia(true);
        self.end_group();
        if self.at(SyntaxKind::R_PAREN) {
            self.bump();
        } else {
            self.error_at(
                open_range,
                format!("unclosed `(` in the `{construct}` condition; expected a matching `)`"),
            );
        }
    }

    fn function_def(&mut self) {
        self.start(SyntaxKind::FUNCTION_DEF);
        let keyword_range = self.token_range(self.pos);
        self.bump();
        self.eat_trivia(true);
        if self.at(SyntaxKind::L_PAREN) {
            self.parameter_list();
        } else {
            let description = self.describe_current();
            self.error_here(format!(
                "expected `(` to open the parameter list, found {description}"
            ));
        }
        self.eat_trivia(true);
        if !self.expression(0) && !self.statement_left_group_open {
            // Blamed on the `function` keyword, not wherever the search for a
            // body stopped: a header running to end of file left this on the
            // blank line past the last statement, zero characters wide,
            // underlining nothing and pointing away from the construct that is
            // incomplete. A parameter list that never closed is also the whole
            // story, because a missing body is only its consequence.
            self.error_at(keyword_range, "expected a function body");
        }
        self.finish();
    }

    fn parameter_list(&mut self) {
        self.start(SyntaxKind::PARAMETER_LIST);
        let open_range = self.token_range(self.pos);
        self.open_group();
        self.bump();
        loop {
            self.eat_trivia(true);
            match self.current() {
                None | Some(SyntaxKind::R_PAREN) => break,
                Some(SyntaxKind::IDENT | SyntaxKind::DOTS | SyntaxKind::DOTDOTI) => {
                    self.start(SyntaxKind::PARAMETER);
                    self.bump();
                    self.eat_trivia(true);
                    if self.at(SyntaxKind::EQ) {
                        self.bump();
                        self.eat_trivia(true);
                        match self.current() {
                            Some(SyntaxKind::COMMA | SyntaxKind::R_PAREN) | None => {
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
                    if self.at(SyntaxKind::COMMA) {
                        self.bump();
                    } else if self.crossed_line() && self.starts_new_statement() {
                        break;
                    } else if matches!(
                        self.current(),
                        Some(SyntaxKind::IDENT | SyntaxKind::DOTS | SyntaxKind::DOTDOTI)
                    ) {
                        // A parameter name right after a complete parameter:
                        // the separator was forgotten. Report the missing `,`
                        // in the gap and let the loop parse the name as the
                        // next parameter.
                        self.error_at(self.separator_gap(), "missing `,` between these parameters");
                    } else if !matches!(self.current(), None | Some(SyntaxKind::R_PAREN)) {
                        let description = self.describe_current();
                        self.error_here(format!(
                            "expected `,` or `)` in the parameter list, found {description}"
                        ));
                        self.recover_argument(SyntaxKind::R_PAREN);
                        if self.at(SyntaxKind::COMMA) {
                            self.bump();
                        }
                    }
                }
                _ => {
                    let description = self.describe_current();
                    self.error_here(format!("expected a parameter name, found {description}"));
                    self.recover_argument(SyntaxKind::R_PAREN);
                    if self.at(SyntaxKind::COMMA) {
                        self.bump();
                    }
                }
            }
        }
        self.end_group();
        if self.at(SyntaxKind::R_PAREN) {
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

    /// Call / subset argument lists. `open` is `(`, `[`, or `[[`; `[[` closes
    /// with two `]` tokens (as in R's grammar).
    fn argument_list(&mut self, open: SyntaxKind) {
        self.start(SyntaxKind::ARGUMENT_LIST);
        let open_range = self.token_range(self.pos);
        self.open_group();
        self.bump();
        let closer = match open {
            SyntaxKind::L_PAREN => SyntaxKind::R_PAREN,
            _ => SyntaxKind::R_BRACKET,
        };
        let mut expect_argument = true;
        loop {
            self.eat_trivia(true);
            match self.current() {
                None => break,
                Some(kind) if kind == closer => break,
                Some(SyntaxKind::COMMA) => {
                    if expect_argument {
                        // A positional hole: `f(, x)` / `m[, 1]`.
                        self.start(SyntaxKind::ARGUMENT);
                        self.finish();
                    }
                    self.bump();
                    expect_argument = true;
                }
                Some(kind) => {
                    if !expect_argument {
                        // A token that starts a new argument right after a
                        // complete one means the separator was forgotten:
                        // report the missing `,` in the gap and keep parsing
                        // the list normally.
                        if Self::starts_expression(kind) {
                            // A new statement on a later line is not a
                            // forgotten comma: the opener was never closed, and
                            // reading on adopts that statement and every one
                            // after it as further arguments. One missing `)`
                            // put a confident "missing `,`" on every following
                            // line, scaling with the file, and it cost the
                            // adopted lines their definitions.
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
                        let description = self.describe_current();
                        self.error_here(format!(
                            "expected `,` or {} in this argument list, found {description}",
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
            if open == SyntaxKind::L_BRACKET2 {
                // The second `]` of `]]`.
                self.eat_trivia(true);
                if self.at(SyntaxKind::R_BRACKET) {
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
        self.start(SyntaxKind::ARGUMENT);
        // Tagged argument: `name = value`, `"name" = value`, `... = value`,
        // and `NULL = value`. R's grammar admits NULL_CONST as a tag, which is
        // the `switch(x, NULL = , …)` idiom.
        let tagged = matches!(
            self.current(),
            Some(
                SyntaxKind::IDENT
                    | SyntaxKind::STRING
                    | SyntaxKind::DOTS
                    | SyntaxKind::DOTDOTI
                    | SyntaxKind::NULL_KW
            )
        ) && self
            .peek_significant(self.pos + 1, true)
            .is_some_and(|(kind, _)| kind == SyntaxKind::EQ);
        if tagged {
            match self.current() {
                Some(SyntaxKind::STRING | SyntaxKind::NULL_KW) => self.wrap_literal(),
                _ => self.wrap_name(),
            }
            self.eat_trivia(true);
            debug_assert!(self.at(SyntaxKind::EQ));
            self.bump();
            self.eat_trivia(true);
            match self.current() {
                // `f(x = )` and `f(x = , y)` leave the value empty.
                Some(SyntaxKind::COMMA) | None => {}
                Some(kind) if kind == closer => {}
                _ => {
                    if !self.expression(0) {
                        let description = self.describe_current();
                        self.error_here(format!("expected an argument value, found {description}"));
                        self.recover_argument(closer);
                    }
                }
            }
        } else if !self.expression(0) {
            let description = self.describe_current();
            self.error_here(format!("expected an argument, found {description}"));
            self.recover_argument(closer);
        }
        self.finish();
    }

    /// Recovery inside a delimited list, wrapped in an `ERROR` node and
    /// guaranteed to make progress: a mismatched closing delimiter sitting at
    /// the recovery point is consumed (it can belong to nothing here), so the
    /// surrounding list loop can never spin in place.
    fn recover_argument(&mut self, closer: SyntaxKind) {
        let start = self.pos;
        self.start(SyntaxKind::ERROR);
        self.recover_in_list(closer);
        if self.pos == start
            && !matches!(self.current(), None | Some(SyntaxKind::COMMA))
            && self.current() != Some(closer)
        {
            self.bump();
            self.recover_in_list(closer);
        }
        self.finish();
    }

    /// Consume tokens until `,`, the closing delimiter, or end of file, keeping
    /// broken argument regions local.
    fn recover_in_list(&mut self, closer: SyntaxKind) {
        let mut depth = 0u32;
        while let Some(kind) = self.current() {
            match kind {
                SyntaxKind::COMMA if depth == 0 => return,
                kind if kind == closer && depth == 0 => return,
                SyntaxKind::L_PAREN
                | SyntaxKind::L_BRACKET
                | SyntaxKind::L_BRACKET2
                | SyntaxKind::L_BRACE => {
                    depth += 1;
                    self.bump();
                }
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET | SyntaxKind::R_BRACE => {
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

// Binding powers follow R's `gram.y` precedence declarations, low to high.
// Left-associative operators parse their right side at `left + 1`; right-
// associative ones at `left`.

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
    use SyntaxKind::*;
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
