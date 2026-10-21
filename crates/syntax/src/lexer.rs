//! Tokens cover the input exactly and in order, so offsets are implicit. `#:`
//! switches to annotation mode for the rest of the line, so annotation bodies
//! arrive as ordinary tokens.

use crate::SyntaxError;
use crate::kind::SyntaxKind;
use rowan::{TextRange, TextSize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub len: TextSize,
}

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
    /// Inside an annotation, `#` starts a plain comment: annotations do not nest.
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
                // R accepts `**` as a synonym for `^`.
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

    fn ident_or_number(&mut self, start: usize, first: char) -> SyntaxKind {
        use SyntaxKind::*;
        if first == '.' {
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos = start;
                self.bump();
                return self.number(start);
            }
            if self.peek() == Some('.') && self.peek_second().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
                self.eat_while(|c| c.is_ascii_digit());
                // R has no dot-dot token (`..2` is an ordinary symbol), so a
                // longer `..2dge` is one plain name.
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
            self.bump();
            let digits_start = self.pos;
            self.eat_while(|c| c.is_ascii_hexdigit());
            if self.pos == digits_start {
                self.error("missing digits after `0x`", start);
                return ERROR_TOKEN;
            }
            // Hex-float exponent: `0x1Fp3`.
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
                // `1e` lexes as `1` then the name `e`.
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

    /// R strings may span lines, but an unterminated one would swallow the rest
    /// of the file; cutting it at its first line break confines a stray quote's
    /// damage to its own line.
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
                    self.bump();
                }
                Some(c) if c == quote => break,
                Some(_) => {}
            }
        }
        SyntaxKind::STRING
    }

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
                // Backtick names use string escape rules: `` \` `` does not end
                // the name.
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

/// Invisible characters are named rather than quoted: quoting one shows an
/// empty pair of backticks.
fn unexpected_character_message(character: char) -> String {
    let ascii = |what: &str, instead: char| {
        format!("`{character}` is {what}, not R syntax. Use `{instead}` instead")
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
            "there is an invisible character here, a zero-width space. Delete it".to_owned()
        }
        _ => format!("unexpected character `{character}`"),
    }
}

fn invisible(what: &str) -> String {
    format!("this is {what}, not an ordinary space. R needs a plain space here")
}
