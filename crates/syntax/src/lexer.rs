//! Hand-written R lexer.
//!
//! Produces a token stream that covers the input exactly (the concatenation of
//! all token texts equals the source). Never fails: bytes that cannot start a
//! token become `ERROR_TOKEN`s with a diagnostic. `#:` comments switch the
//! lexer into annotation mode for the rest of the line, so annotation bodies
//! arrive as ordinary tokens with real spans instead of an opaque comment.

use crate::SyntaxError;
use crate::kind::SyntaxKind;
use rowan::{TextRange, TextSize};

/// One lexed token: its kind and byte length. Offsets are implicit (tokens
/// cover the input in order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub len: TextSize,
}

/// Lex the whole input. The returned tokens always cover `text` exactly.
pub fn lex(text: &str) -> (Vec<Token>, Vec<SyntaxError>) {
    let mut lexer = Lexer {
        text,
        pos: 0,
        tokens: Vec::new(),
        errors: Vec::new(),
        in_annotation: false,
    };
    lexer.run();
    (lexer.tokens, lexer.errors)
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<SyntaxError>,
    /// True between a `#:` marker and the next newline: `#` starts a plain
    /// comment again (annotation bodies cannot nest annotations).
    in_annotation: bool,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.pos < self.text.len() {
            let start = self.pos;
            let kind = self.next_token();
            debug_assert!(self.pos > start, "lexer must always make progress");
            self.push(kind, start);
        }
    }

    fn push(&mut self, kind: SyntaxKind, start: usize) {
        let len = TextSize::from((self.pos - start) as u32);
        self.tokens.push(Token { kind, len });
        if kind == SyntaxKind::NEWLINE {
            self.in_annotation = false;
        }
    }

    fn error(&mut self, message: impl Into<String>, start: usize) {
        let range = TextRange::new(
            TextSize::from(start as u32),
            TextSize::from(self.pos as u32),
        );
        self.errors.push(SyntaxError::new(message, range));
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut chars = self.text[self.pos..].chars();
        chars.next();
        chars.next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, expected: &str) -> bool {
        if self.text[self.pos..].starts_with(expected) {
            self.pos += expected.len();
            true
        } else {
            false
        }
    }

    fn eat_while(&mut self, predicate: impl Fn(char) -> bool) {
        while let Some(ch) = self.peek() {
            if predicate(ch) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> SyntaxKind {
        use SyntaxKind::*;
        let start = self.pos;
        let first = self.bump().expect("next_token called at end of input");
        match first {
            ' ' | '\t' | '\u{c}' => {
                self.eat_while(|c| c == ' ' || c == '\t' || c == '\u{c}');
                WHITESPACE
            }
            '\n' => NEWLINE,
            '\r' => {
                self.eat('\n');
                NEWLINE
            }
            '#' => {
                if !self.in_annotation && self.eat(':') {
                    self.in_annotation = true;
                    ANNOTATION_MARKER
                } else {
                    self.eat_while(|c| c != '\n' && c != '\r');
                    COMMENT
                }
            }
            '\'' | '"' => self.string(start, first),
            'r' | 'R' if matches!(self.peek(), Some('"' | '\'')) => self.raw_string(start),
            c if is_ident_start(c) => self.ident_or_number(start, c),
            c if c.is_ascii_digit() => self.number(start),
            '`' => self.backtick_name(start),
            '%' => self.special(start),
            '_' => UNDERSCORE,
            '+' => PLUS,
            '-' => {
                if self.eat_str(">>") {
                    MINUS_GREATER2
                } else if self.eat('>') {
                    MINUS_GREATER
                } else {
                    MINUS
                }
            }
            '*' => {
                // R's parser accepts `**` as a synonym for `^` and translates
                // it; one CARET-kind token keeps the grammar unchanged while
                // the text stays lossless.
                if self.eat('*') { CARET } else { STAR }
            }
            '/' => SLASH,
            '^' => CARET,
            '~' => TILDE,
            '?' => QUESTION,
            '(' => L_PAREN,
            ')' => R_PAREN,
            '{' => L_BRACE,
            '}' => R_BRACE,
            '[' => {
                if self.eat('[') {
                    L_BRACKET2
                } else {
                    L_BRACKET
                }
            }
            ']' => R_BRACKET,
            ',' => COMMA,
            ';' => SEMICOLON,
            '\\' => BACKSLASH,
            '$' => DOLLAR,
            '@' => AT,
            '<' => {
                if self.eat_str("<-") {
                    LESS2_MINUS
                } else if self.eat('-') {
                    LESS_MINUS
                } else if self.eat('=') {
                    LESS_EQ
                } else {
                    LESS
                }
            }
            '>' => {
                if self.eat('=') {
                    GREATER_EQ
                } else {
                    GREATER
                }
            }
            '=' => {
                if self.eat('=') {
                    EQ2
                } else {
                    EQ
                }
            }
            '!' => {
                if self.eat('=') {
                    BANG_EQ
                } else {
                    BANG
                }
            }
            '&' => {
                if self.eat('&') {
                    AMP2
                } else {
                    AMP
                }
            }
            '|' => {
                if self.eat('|') {
                    PIPE2
                } else if self.eat('>') {
                    PIPE_GREATER
                } else {
                    PIPE
                }
            }
            ':' => {
                if self.eat_str("::") {
                    COLON3
                } else if self.eat(':') {
                    COLON2
                } else if self.eat('=') {
                    COLON_EQ
                } else {
                    COLON
                }
            }
            _ => {
                self.error(unexpected_character_message(first), start);
                ERROR_TOKEN
            }
        }
    }

    /// Identifiers, keywords, `...`, `..N`, and numbers starting with `.`.
    fn ident_or_number(&mut self, start: usize, first: char) -> SyntaxKind {
        use SyntaxKind::*;
        if first == '.' {
            // `.5` is a number; `..1` is a dot-dot index; `...` and `..`/`.x` are names.
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos = start;
                self.bump();
                return self.number(start);
            }
            if self.peek() == Some('.') && self.peek_second().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
                self.eat_while(|c| c.is_ascii_digit());
                // Only a bare `..N` is the dot-dot index. R's lexer has no
                // dot-dot token at all (`..2` is an ordinary symbol resolved
                // at evaluation), so a longer `..2dge` is one plain name.
                if !self.peek().is_some_and(is_ident_continue) {
                    return DOTDOTI;
                }
            }
        }
        self.eat_while(is_ident_continue);
        let text = &self.text[start..self.pos];
        if text == "..." {
            return DOTS;
        }
        SyntaxKind::from_keyword(text).unwrap_or(IDENT)
    }

    fn number(&mut self, start: usize) -> SyntaxKind {
        use SyntaxKind::*;
        let hex = self.text[start..].starts_with("0x") || self.text[start..].starts_with("0X");
        if hex {
            // `0` already consumed; consume `x`/`X` then hex digits.
            self.bump();
            let digits_start = self.pos;
            self.eat_while(|c| c.is_ascii_hexdigit());
            if self.pos == digits_start {
                self.error("missing digits after `0x`", start);
                return ERROR_TOKEN;
            }
            // C99 hex-float exponent: 0x1Fp3.
            if matches!(self.peek(), Some('p' | 'P')) {
                let before = self.pos;
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                let exp_start = self.pos;
                self.eat_while(|c| c.is_ascii_digit());
                if self.pos == exp_start {
                    self.pos = before;
                }
            }
        } else {
            self.eat_while(|c| c.is_ascii_digit());
            if self.peek() == Some('.') {
                self.bump();
                self.eat_while(|c| c.is_ascii_digit());
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                // Consume the exponent only when digits (optionally signed) follow,
                // so `1e` lexes as `1` then the name `e`.
                let before = self.pos;
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                let exp_start = self.pos;
                self.eat_while(|c| c.is_ascii_digit());
                if self.pos == exp_start {
                    self.pos = before;
                }
            }
        }
        if self.eat('L') {
            INTEGER
        } else if self.eat('i') {
            COMPLEX
        } else {
            DOUBLE
        }
    }

    /// End a quoted token that never closes at its first line break.
    ///
    /// R lets a string or a backtick-quoted name span lines, so the scan can
    /// only give up at end of file — and a token that ran that far would
    /// otherwise swallow every statement below it, which is what a stray quote
    /// in a large file does while it is being typed: naming, diagnostics and
    /// every editor feature go dark from the quote to the end of the file for
    /// one missing character. Cutting the token at the line break confines the
    /// damage to the line that is actually wrong, and costs nothing on input
    /// that closes, which never reaches here.
    fn end_unterminated_at_line_break(&mut self, start: usize) {
        if let Some(offset) = self.text[start..self.pos].find(['\n', '\r']) {
            self.pos = start + offset;
        }
    }

    fn string(&mut self, start: usize, quote: char) -> SyntaxKind {
        loop {
            match self.bump() {
                None => {
                    self.end_unterminated_at_line_break(start);
                    self.error("unterminated string; expected a closing quote", start);
                    break;
                }
                Some('\\') => {
                    // Any escaped character is skipped wholesale; escape validity is
                    // a semantic concern, termination is the lexer's only job here.
                    self.bump();
                }
                Some(c) if c == quote => break,
                Some(_) => {}
            }
        }
        SyntaxKind::STRING
    }

    /// `r"(...)"`, `R"---[...]---"`, and friends. Called with `pos` on the quote.
    fn raw_string(&mut self, start: usize) -> SyntaxKind {
        let quote = self.bump().expect("raw_string called at a quote");
        let dash_start = self.pos;
        self.eat_while(|c| c == '-');
        let dashes = self.pos - dash_start;
        let open = self.peek();
        let close = match open {
            Some('(') => ')',
            Some('[') => ']',
            Some('{') => '}',
            _ => {
                self.error(
                    "malformed raw string: expected `(`, `[`, or `{` after the quote",
                    start,
                );
                return SyntaxKind::ERROR_TOKEN;
            }
        };
        self.bump();
        let mut closer = String::with_capacity(dashes + 2);
        closer.push(close);
        for _ in 0..dashes {
            closer.push('-');
        }
        closer.push(quote);
        match self.text[self.pos..].find(&closer) {
            Some(offset) => {
                self.pos += offset + closer.len();
                SyntaxKind::RAW_STRING
            }
            None => {
                self.pos = self.text.len();
                self.end_unterminated_at_line_break(start);
                self.error(
                    format!("unterminated raw string; expected `{closer}`"),
                    start,
                );
                SyntaxKind::RAW_STRING
            }
        }
    }

    fn backtick_name(&mut self, start: usize) -> SyntaxKind {
        loop {
            match self.bump() {
                None => {
                    self.end_unterminated_at_line_break(start);
                    self.error(
                        "unterminated backtick name; expected a closing `` ` ``",
                        start,
                    );
                    break;
                }
                // R reads a backtick-quoted name with the same escape rules as
                // a string, so a name may contain a literal backtick as `` \` ``
                // and a literal backslash as `\\`. Treating the escaped backtick
                // as the terminator would end the name early and turn the rest
                // of the line into a cascade of parse errors.
                Some('\\') => {
                    if self.bump().is_none() {
                        self.end_unterminated_at_line_break(start);
                        self.error(
                            "unterminated backtick name; expected a closing `` ` ``",
                            start,
                        );
                        break;
                    }
                }
                Some('`') => break,
                Some(_) => {}
            }
        }
        SyntaxKind::IDENT
    }

    /// `%…%` operators; a newline before the closing `%` is malformed (as in R).
    fn special(&mut self, start: usize) -> SyntaxKind {
        loop {
            match self.peek() {
                None | Some('\n') | Some('\r') => {
                    self.error("unterminated `%` operator; expected a closing `%`", start);
                    return SyntaxKind::ERROR_TOKEN;
                }
                Some('%') => {
                    self.bump();
                    return SyntaxKind::SPECIAL;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }
}

/// Whether `text` is a syntactic R name — usable bare, without backticks.
/// A name starting with `.` followed by a digit (`.1`) lexes as a number,
/// and the reserved words are not names.
pub fn is_syntactic_name(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_ident_start(first) {
        return false;
    }
    if first == '.' && text.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    if !chars.all(is_ident_continue) {
        return false;
    }
    !matches!(
        text,
        "if" | "else"
            | "repeat"
            | "while"
            | "function"
            | "for"
            | "next"
            | "break"
            | "TRUE"
            | "FALSE"
            | "NULL"
            | "Inf"
            | "NaN"
            | "NA"
            | "NA_integer_"
            | "NA_real_"
            | "NA_complex_"
            | "NA_character_"
            | "in"
    )
}

fn is_ident_start(c: char) -> bool {
    c == '.' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '.' || c == '_' || c.is_alphanumeric()
}

/// What to say about a character R's grammar has no use for.
///
/// Most of these arrive by paste — from a document, a chat window, an editor
/// with smart quotes on — and the lexer knows exactly which ASCII character was
/// meant, so it says so instead of naming the codepoint and stopping. The
/// invisible ones are named rather than quoted: printing a zero-width space
/// between backticks shows the reader an empty pair and a caret over nothing.
fn unexpected_character_message(character: char) -> String {
    let ascii = |what: &str, instead: char| {
        format!("`{character}` is {what}, not R syntax — use `{instead}` instead")
    };
    match character {
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{00ab}' | '\u{00bb}' => {
            ascii("a typographic quote", '"')
        }
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' => ascii("a typographic quote", '\''),
        '\u{2013}' => ascii("an en dash", '-'),
        '\u{2014}' => ascii("an em dash", '-'),
        '\u{2212}' => ascii("a minus sign", '-'),
        '\u{ff08}' => ascii("a full-width parenthesis", '('),
        '\u{ff09}' => ascii("a full-width parenthesis", ')'),
        '\u{ff0c}' => ascii("a full-width comma", ','),
        '\u{ff1b}' => ascii("a full-width semicolon", ';'),
        '\u{ff1a}' => ascii("a full-width colon", ':'),
        '\u{00a0}' => invisible("a non-breaking space"),
        '\u{202f}' => invisible("a narrow non-breaking space"),
        '\u{2007}' | '\u{2009}' | '\u{200a}' | '\u{2002}' | '\u{2003}' => {
            invisible("a typographic space")
        }
        '\u{3000}' => invisible("an ideographic space"),
        '\u{200b}' | '\u{feff}' => {
            "there is an invisible character here (a zero-width space) — delete it".to_owned()
        }
        _ => format!("unexpected character `{character}`"),
    }
}

/// Whitespace that is not whitespace to R. Quoting it would show the reader a
/// blank between backticks.
fn invisible(what: &str) -> String {
    format!("this is {what}, not an ordinary space — R needs a plain space here")
}
