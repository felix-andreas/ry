//! Tokens cover the input exactly and in order, so offsets are implicit. `#:`
//! switches to annotation mode for the rest of the line, so annotation bodies
//! arrive as ordinary tokens.

use {
    crate::{SyntaxError, kind::SyntaxKind},
    SyntaxKind::*,
    memchr::memchr2,
    text_size::{TextRange, TextSize},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub len: TextSize,
}

pub fn lex(text: &str) -> (Vec<Token>, Vec<SyntaxError>) {
    let mut lexer = Lexer {
        text,
        pos: 0,
        // Counting whitespace runs as tokens, R averages under three bytes a token and rarely
        // fewer than two and a half.
        tokens: Vec::with_capacity(text.len() / 2),
        errors: Vec::new(),
        in_annotation: false,
        modes: Vec::new(),
    };
    while lexer.pos < text.len() {
        let start = lexer.pos;
        let kind = match lexer.modes.last() {
            Some(&Mode::FormatText { quote, .. }) => lexer.format_text(quote),
            _ => {
                let kind = lexer.next_token();
                lexer.track_interpolation_brace(kind);
                kind
            }
        };
        debug_assert!(lexer.pos > start, "lexer must always make progress");
        let len = TextSize::from((lexer.pos - start) as u32);
        lexer.tokens.push(Token { kind, len });
        if kind == NEWLINE {
            lexer.in_annotation = false;
        }
    }
    if let Some(start) = lexer.modes.iter().find_map(|mode| match mode {
        Mode::FormatText { start, .. } => Some(*start),
        Mode::Interpolation { .. } => None,
    }) {
        lexer.error("unterminated f-string; expected a closing quote", start);
    }
    (lexer.tokens, lexer.errors)
}

/// A ry number type that a literal's suffix names, as in `5i64` or `1.5f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberType {
    I64,
    F64,
}

impl NumberType {
    pub const SUFFIXES: [(&'static str, NumberType); 2] =
        [("i64", NumberType::I64), ("f64", NumberType::F64)];

    pub fn from_suffix(suffix: &str) -> Option<NumberType> {
        NumberType::SUFFIXES
            .iter()
            .find(|(spelling, _)| *spelling == suffix)
            .map(|(_, number)| *number)
    }

    pub fn suffix(self) -> &'static str {
        match self {
            NumberType::I64 => "i64",
            NumberType::F64 => "f64",
        }
    }

    pub fn is_integer(self) -> bool {
        self == NumberType::I64
    }
}

/// A `SUFFIXED_NUMBER` token split into its number and the type its suffix names, so `0xFFi64`
/// is `("0xFF", I64)` and `1e3f64` is `("1e3", F64)`.
pub fn split_number_suffix(token: &str) -> Option<(&str, NumberType)> {
    let hex = token.starts_with("0x") || token.starts_with("0X");
    let suffix_start = if hex {
        token.rfind('i')?
    } else {
        token.find(['i', 'f'])?
    };
    let number = NumberType::from_suffix(&token[suffix_start..])?;
    Some((&token[..suffix_start], number))
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
    /// The f-strings open around the current position, innermost last. An interpolation can
    /// hold another f-string, so one flag would not do.
    modes: Vec<Mode>,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    /// Between an f-string's quotes and outside any interpolation.
    FormatText { quote: u8, start: usize },
    /// Inside an f-string's `{}`, where ordinary tokens are lexed. `depth` counts the braces
    /// the expression itself opened, so only a `}` at depth 0 closes the interpolation.
    Interpolation { depth: u32 },
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
        match *self.text.as_bytes().get(self.pos)? {
            byte if byte.is_ascii() => Some(char::from(byte)),
            _ => self.text[self.pos..].chars().next(),
        }
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
                self.pos += self.text.as_bytes()[self.pos..]
                    .iter()
                    .position(|&byte| !matches!(byte, b' ' | b'\t' | b'\x0c'))
                    .unwrap_or(self.text.len() - self.pos);
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
                    // A line break is ASCII, so a byte scan cannot stop inside a character.
                    self.pos += memchr2(b'\n', b'\r', &self.text.as_bytes()[self.pos..])
                        .unwrap_or(self.text.len() - self.pos);
                    COMMENT
                }
            }
            '\'' | '"' => self.quoted(
                start,
                first as u8,
                STRING,
                "unterminated string; expected a closing quote",
            ),
            'r' | 'R' if matches!(self.peek(), Some('"' | '\'')) => self.raw_string(start),
            // R reads `f"..."` as a name followed by a string, a syntax error, so the
            // f-string takes no R text.
            'f' if !self.in_annotation && matches!(self.peek(), Some('"' | '\'')) => {
                let quote = self.bump().expect("a quote follows") as u8;
                self.modes.push(Mode::FormatText { quote, start });
                FSTRING_START
            }
            c if is_ident_start(c) => self.ident_or_number(start, c),
            c if c.is_ascii_digit() => self.number(start),
            '`' => self.quoted(
                start,
                b'`',
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
        let ascii = self.text.as_bytes()[self.pos..]
            .iter()
            .position(|&byte| !(byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_'))
            .unwrap_or(self.text.len() - self.pos);
        self.pos += ascii;
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
        if let Some(kind) = self.number_suffix(start, hex) {
            return kind;
        }
        if self.eat("L") {
            INTEGER
        } else if self.eat("i") {
            COMPLEX
        } else {
            DOUBLE
        }
    }

    /// A ry type suffix glued to the number, `5i64` or `1.5f64`. R reads each as a number
    /// directly followed by a name (and `5i64` as `5i` then `64`), a syntax error in R, so no R
    /// text changes meaning. A run that starts like a suffix (`i`, `u` or `f` then a digit, or
    /// `dec`) is one, so `5i`, `5L` and `5x` keep their R lexing, and a suffix of a number type
    /// ry does not have, such as `5i32` or `1.5dec`, is an error that names the two it has. A hex
    /// literal takes only `i64`, because `f`, `d`, `e` and `c` are hex digits.
    fn number_suffix(&mut self, start: usize, hex: bool) -> Option<SyntaxKind> {
        let starts_suffix = match self.text.as_bytes()[self.pos..] {
            [b'i' | b'u', digit, ..] => digit.is_ascii_digit(),
            [b'f', digit, ..] => !hex && digit.is_ascii_digit(),
            [b'd', b'e', b'c', ..] => !hex,
            _ => false,
        };
        if !starts_suffix {
            return None;
        }
        let suffix_start = self.pos;
        self.eat_while(is_ident_continue);
        let suffix = &self.text[suffix_start..self.pos];
        if NumberType::from_suffix(suffix).is_some_and(|number| !hex || number.is_integer()) {
            return Some(SUFFIXED_NUMBER);
        }
        let expected = if hex {
            "a hex literal takes only i64"
        } else {
            "use i64 or f64, the two number types ry has"
        };
        self.error(
            format!("`{suffix}` is not a number type suffix; {expected}"),
            start,
        );
        Some(ERROR_TOKEN)
    }

    /// An f-string's text up to the next interpolation or the closing quote, or else that
    /// quote or the `{` that opens the interpolation. `{{` and `}}` are literal braces and
    /// belong to the text.
    fn format_text(&mut self, quote: u8) -> SyntaxKind {
        let bytes = self.text.as_bytes();
        match bytes[self.pos] {
            byte if byte == quote => {
                self.pos += 1;
                self.modes.pop();
                return FSTRING_END;
            }
            b'{' if bytes.get(self.pos + 1) != Some(&b'{') => {
                self.pos += 1;
                self.modes.push(Mode::Interpolation { depth: 0 });
                return L_BRACE;
            }
            _ => {}
        }
        // The scan stops only at ASCII bytes, so it never splits a character.
        while let Some(&byte) = bytes.get(self.pos) {
            match byte {
                _ if byte == quote => break,
                b'\\' => {
                    self.pos += 1;
                    self.bump();
                }
                b'{' | b'}' if bytes.get(self.pos + 1) == Some(&byte) => self.pos += 2,
                b'{' => break,
                b'}' => {
                    let brace = self.pos;
                    self.pos += 1;
                    self.error("a `}` in an f-string's text is written `}}`", brace);
                }
                _ => self.pos += 1,
            }
        }
        FSTRING_TEXT
    }

    /// Keeps count of the braces inside an interpolation, so the `}` that closes it returns
    /// the lexer to the f-string's text.
    fn track_interpolation_brace(&mut self, kind: SyntaxKind) {
        let Some(Mode::Interpolation { depth }) = self.modes.last_mut() else {
            return;
        };
        match kind {
            L_BRACE => *depth += 1,
            R_BRACE if *depth == 0 => {
                self.modes.pop();
            }
            R_BRACE => *depth -= 1,
            _ => {}
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
    ///
    /// The quote, the backslash and the line breaks are ASCII, so a byte scan for them never
    /// stops inside a character.
    fn quoted(
        &mut self,
        start: usize,
        quote: u8,
        kind: SyntaxKind,
        unterminated: &str,
    ) -> SyntaxKind {
        let bytes = self.text.as_bytes();
        let annotation = self.in_annotation;
        loop {
            let rest = &bytes[self.pos..];
            // Outside an annotation a line break belongs to the string, so only the quote and the
            // backslash stop the scan. In an annotation a line break before them stops it too.
            let stop = match memchr2(quote, b'\\', rest) {
                Some(offset) if annotation => {
                    memchr2(b'\n', b'\r', &rest[..offset]).or(Some(offset))
                }
                None if annotation => memchr2(b'\n', b'\r', rest),
                found => found,
            };
            let Some(offset) = stop else {
                self.pos = self.text.len();
                break;
            };
            self.pos += offset + 1;
            match bytes[self.pos - 1] {
                b'\\' => {
                    self.bump();
                }
                byte if byte == quote => return kind,
                _ => break,
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
