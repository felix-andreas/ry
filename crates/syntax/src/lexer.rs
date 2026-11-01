//! Tokens cover the input exactly and in order, so offsets are implicit. `#:`
//! switches to annotation mode for the rest of the line, so annotation bodies
//! arrive as ordinary tokens.

use crate::SyntaxError;
use crate::kind::SyntaxKind;
use SyntaxKind::*;
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
    while lexer.pos < text.len() {
        let start = lexer.pos;
        let kind = lexer.next_token();
        debug_assert!(lexer.pos > start, "lexer must always make progress");
        let len = TextSize::from((lexer.pos - start) as u32);
        lexer.tokens.push(Token { kind, len });
        if kind == NEWLINE {
            lexer.in_annotation = false;
        }
    }
    (lexer.tokens, lexer.errors)
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
    chars.all(is_ident_continue) && SyntaxKind::from_keyword(text).is_none()
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

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn eat(&mut self, expected: &str) -> bool {
        if self.text[self.pos..].starts_with(expected) {
            self.pos += expected.len();
            true
        } else {
            false
        }
    }

    fn eat_while(&mut self, predicate: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&predicate) {
            self.bump();
        }
    }

    /// The first suffix that follows wins, so a longer one must come first.
    fn operator(&mut self, suffixes: &[(&str, SyntaxKind)], otherwise: SyntaxKind) -> SyntaxKind {
        suffixes
            .iter()
            .find(|(suffix, _)| self.eat(suffix))
            .map_or(otherwise, |(_, kind)| *kind)
    }

    fn next_token(&mut self) -> SyntaxKind {
        let start = self.pos;
        let first = self.bump().expect("next_token called at end of input");
        match first {
            ' ' | '\t' | '\u{c}' => {
                self.eat_while(|c| c == ' ' || c == '\t' || c == '\u{c}');
                WHITESPACE
            }
            '\n' => NEWLINE,
            '\r' => {
                self.eat("\n");
                NEWLINE
            }
            '#' => {
                if !self.in_annotation && self.eat(":") {
                    self.in_annotation = true;
                    ANNOTATION_MARKER
                } else {
                    self.eat_while(|c| c != '\n' && c != '\r');
                    COMMENT
                }
            }
            '\'' | '"' => self.quoted(
                start,
                first,
                STRING,
                "unterminated string; expected a closing quote",
            ),
            'r' | 'R' if matches!(self.peek(), Some('"' | '\'')) => self.raw_string(start),
            c if is_ident_start(c) => self.ident_or_number(start, c),
            c if c.is_ascii_digit() => self.number(start),
            '`' => self.quoted(
                start,
                '`',
                IDENT,
                "unterminated backtick name; expected a closing `` ` ``",
            ),
            '%' => self.special(start),
            '_' => UNDERSCORE,
            '+' => PLUS,
            '-' => self.operator(&[(">>", MINUS_GREATER2), (">", MINUS_GREATER)], MINUS),
            // R accepts `**` as a synonym for `^`.
            '*' => self.operator(&[("*", CARET)], STAR),
            '/' => SLASH,
            '^' => CARET,
            '~' => TILDE,
            '?' => QUESTION,
            '(' => L_PAREN,
            ')' => R_PAREN,
            '{' => L_BRACE,
            '}' => R_BRACE,
            '[' => self.operator(&[("[", L_BRACKET2)], L_BRACKET),
            ']' => R_BRACKET,
            ',' => COMMA,
            ';' => SEMICOLON,
            '\\' => BACKSLASH,
            '$' => DOLLAR,
            '@' => AT,
            '<' => self.operator(
                &[("<-", LESS2_MINUS), ("-", LESS_MINUS), ("=", LESS_EQ)],
                LESS,
            ),
            '>' => self.operator(&[("=", GREATER_EQ)], GREATER),
            '=' => self.operator(&[("=", EQ2), (">", FAT_ARROW)], EQ),
            '!' => self.operator(&[("=", BANG_EQ)], BANG),
            '&' => self.operator(&[("&", AMP2)], AMP),
            '|' => self.operator(&[("|", PIPE2), (">", PIPE_GREATER)], PIPE),
            ':' => self.operator(&[("::", COLON3), (":", COLON2), ("=", COLON_EQ)], COLON),
            _ => {
                self.error(unexpected_character_message(first), start);
                ERROR_TOKEN
            }
        }
    }

    fn ident_or_number(&mut self, start: usize, first: char) -> SyntaxKind {
        if first == '.' {
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return self.number(start);
            }
            if self.peek() == Some('.')
                && self.text[self.pos..]
                    .chars()
                    .nth(1)
                    .is_some_and(|c| c.is_ascii_digit())
            {
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
        let hex = self.text[start..].starts_with("0x") || self.text[start..].starts_with("0X");
        if hex {
            self.bump();
            let digits_start = self.pos;
            self.eat_while(|c| c.is_ascii_hexdigit());
            let digits = self.pos - digits_start;
            let fraction = self.eat(".");
            if fraction {
                self.eat_while(|c| c.is_ascii_hexdigit());
            }
            if digits == 0 && !fraction {
                self.error("missing digits after `0x`", start);
                return ERROR_TOKEN;
            }
            // Hex-float exponent: `0x1Fp3`, which a hexadecimal fraction like `0x1.8p1` requires.
            let before_exponent = self.pos;
            self.exponent(['p', 'P']);
            if fraction && self.pos == before_exponent {
                self.error("a hexadecimal fraction needs a `p` exponent", start);
                return ERROR_TOKEN;
            }
        } else {
            self.eat_while(|c| c.is_ascii_digit());
            if self.eat(".") {
                self.eat_while(|c| c.is_ascii_digit());
            }
            // `1e` lexes as `1` then the name `e`.
            self.exponent(['e', 'E']);
        }
        if self.eat("L") {
            INTEGER
        } else if self.eat("i") {
            COMPLEX
        } else {
            DOUBLE
        }
    }

    /// An exponent marker with no digits after it is left for the next token.
    fn exponent(&mut self, markers: [char; 2]) {
        if !self.peek().is_some_and(|c| markers.contains(&c)) {
            return;
        }
        let before = self.pos;
        self.bump();
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        let digits_start = self.pos;
        self.eat_while(|c| c.is_ascii_digit());
        if self.pos == digits_start {
            self.pos = before;
        }
    }

    /// R strings may span lines, except inside an annotation, which ends with
    /// its line. An unterminated string would swallow the rest of the file; cutting it at its first line break confines a stray quote's
    /// damage to its own line.
    fn end_unterminated_at_line_break(&mut self, start: usize) {
        if let Some(offset) = self.text[start..self.pos].find(['\n', '\r']) {
            self.pos = start + offset;
        }
    }

    /// Backtick names follow string escape rules: `` \` `` does not end the name.
    fn quoted(
        &mut self,
        start: usize,
        quote: char,
        kind: SyntaxKind,
        unterminated: &str,
    ) -> SyntaxKind {
        loop {
            match self.bump() {
                Some('\\') => {
                    self.bump();
                }
                Some(c) if c == quote => return kind,
                Some('\n' | '\r') if self.in_annotation => break,
                Some(_) => {}
                None => break,
            }
        }
        self.end_unterminated_at_line_break(start);
        self.error(unterminated, start);
        kind
    }

    fn raw_string(&mut self, start: usize) -> SyntaxKind {
        let quote = self.bump().expect("raw_string called at a quote");
        let dash_start = self.pos;
        self.eat_while(|c| c == '-');
        let dashes = self.pos - dash_start;
        let close = match self.peek() {
            Some('(') => ')',
            Some('[') => ']',
            Some('{') => '}',
            _ => {
                self.error(
                    "malformed raw string: expected `(`, `[`, or `{` after the quote",
                    start,
                );
                return ERROR_TOKEN;
            }
        };
        self.bump();
        let closer = format!("{close}{}{quote}", "-".repeat(dashes));
        let line_end = self.text[self.pos..]
            .find(['\n', '\r'])
            .unwrap_or(self.text.len() - self.pos);
        match self.text[self.pos..]
            .find(&closer)
            .filter(|offset| !self.in_annotation || *offset < line_end)
        {
            Some(offset) => {
                self.pos += offset + closer.len();
                RAW_STRING
            }
            None => {
                self.pos = self.text.len();
                self.end_unterminated_at_line_break(start);
                self.error(
                    format!("unterminated raw string; expected `{closer}`"),
                    start,
                );
                RAW_STRING
            }
        }
    }

    fn special(&mut self, start: usize) -> SyntaxKind {
        self.eat_while(|c| !matches!(c, '%' | '\n' | '\r'));
        if self.eat("%") {
            SPECIAL
        } else {
            self.error("unterminated `%` operator; expected a closing `%`", start);
            ERROR_TOKEN
        }
    }
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
