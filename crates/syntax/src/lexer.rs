//! Tokens cover the input exactly and in order, so offsets are implicit. `#:`
//! switches to annotation mode for the rest of the line, so annotation bodies
//! arrive as ordinary tokens.

use {
    crate::{Classed, SyntaxError, Unexpected, kind::SyntaxKind},
    SyntaxKind::*,
    memchr::{memchr, memchr2},
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
        let reported = lexer.errors.len();
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
        // A token R reads as part of a comment cannot fail R's parse.
        if lexer.in_annotation {
            for error in lexer.errors.iter_mut().skip(reported) {
                error.in_annotation = true;
            }
        }
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
    // No R string holds a NUL. R cuts a name at one, and reads past one in a comment.
    if memchr(0, text.as_bytes()).is_some() {
        let mut start = 0;
        let mut in_annotation = false;
        for token in &lexer.tokens {
            let end = start + usize::from(token.len);
            match token.kind {
                ANNOTATION_MARKER => in_annotation = true,
                NEWLINE => in_annotation = false,
                _ => {}
            }
            if !in_annotation
                && matches!(token.kind, STRING | RAW_STRING)
                && let Some(nul) = memchr(0, &text.as_bytes()[start..end])
            {
                let nul = TextSize::from((start + nul) as u32);
                let mut error = SyntaxError::new(
                    "a NUL byte, which no R string can hold; remove it",
                    TextRange::at(nul, TextSize::from(1)),
                );
                error.unexpected = Unexpected::Classed(Classed::EmbeddedNul);
                lexer.errors.push(error);
            }
            start = end;
        }
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

/// The content of a raw string literal such as `r"-(text)-"`: what lies between its opening
/// bracket and the closing one that mirrors it.
pub fn raw_string_content(text: &str) -> &str {
    let rest = text.get(2..).unwrap_or_default();
    let dashes = rest.bytes().take_while(|&byte| byte == b'-').count();
    let open = rest[dashes..].chars().next().map_or(1, char::len_utf8);
    text.get(2 + dashes + open..text.len().saturating_sub(dashes + 2))
        .unwrap_or_default()
}

/// One piece of a quoted token's content, as R's lexer decodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapePiece {
    /// A character written as itself, or the one a simple escape such as `\n` names.
    Text(char),
    /// The byte an octal or `\x` escape names, which need not be UTF-8.
    Byte(u8),
    /// The character a `\u` or `\U` escape names. R keeps a lone surrogate, which is not a
    /// `char`, so one arrives as U+FFFD.
    Unicode(char),
}

/// An escape R's lexer rejects in a string or a backquoted name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeError {
    pub kind: EscapeErrorKind,
    /// The offset of the escape's backslash in the content, or 0 for [`EscapeErrorKind::Mixed`].
    pub backslash: usize,
    /// The offset of the last character R's lexer read before it raised the error, which is
    /// where R reports it. The content's length when that character is the closing quote.
    pub last: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeErrorKind {
    /// An escape that names the NUL character, which an R string cannot hold.
    Nul,
    /// An octal escape above `\377`, with its value.
    OctalTooLarge(u32),
    /// `\x` without a hex digit.
    HexWithoutDigits,
    /// `\u` (or `\U` when `upper`) without a hex digit.
    UnicodeWithoutDigits { upper: bool },
    /// `\u{` or `\U{` whose digits no `}` follows.
    UnicodeUnclosed { upper: bool },
    /// A `\u` or `\U` escape whose value is no Unicode code point.
    UnicodeInvalid {
        upper: bool,
        braced: bool,
        value: u32,
    },
    /// A backslash before a character that starts no escape.
    Unrecognized(char),
    /// A `\u` or `\U` escape in one literal with an octal or `\x` escape.
    Mixed,
}

/// Decodes the content of a string literal or a backquoted name (the text between its quotes),
/// handing each piece to `piece`, or stops at the first escape R's lexer rejects. This is the
/// one reading of R's escapes: the lexer validates with it, and the runtime builds its strings
/// with it.
pub fn unescape_quoted(text: &str, mut piece: impl FnMut(EscapePiece)) -> Result<(), EscapeError> {
    let mut byte_escape = false;
    let mut unicode = false;
    let mut characters = text.char_indices().peekable();
    while let Some((backslash, character)) = characters.next() {
        if character != '\\' {
            piece(EscapePiece::Text(character));
            continue;
        }
        // Only a token that lacks its closing quote ends in a backslash.
        let Some((escape_offset, escape)) = characters.next() else {
            piece(EscapePiece::Text('\\'));
            break;
        };
        let mut last = escape_offset;
        let failure = |kind, last| EscapeError {
            kind,
            backslash,
            last,
        };
        match escape {
            'n' => piece(EscapePiece::Text('\n')),
            't' => piece(EscapePiece::Text('\t')),
            'r' => piece(EscapePiece::Text('\r')),
            'a' => piece(EscapePiece::Text('\u{7}')),
            'b' => piece(EscapePiece::Text('\u{8}')),
            'f' => piece(EscapePiece::Text('\u{c}')),
            'v' => piece(EscapePiece::Text('\u{b}')),
            '\\' | '"' | '\'' | '`' | ' ' | '\n' => piece(EscapePiece::Text(escape)),
            '0'..='7' => {
                let (rest, count) = escape_digits(&mut characters, 8, 2, &mut last);
                let value = escape.to_digit(8).unwrap_or(0) * 8u32.pow(count as u32) + rest;
                if value == 0 {
                    return Err(failure(EscapeErrorKind::Nul, last));
                }
                if value > 0xFF {
                    return Err(failure(EscapeErrorKind::OctalTooLarge(value), last));
                }
                piece(EscapePiece::Byte(value as u8));
                byte_escape = true;
            }
            'x' => {
                let (value, count) = escape_digits(&mut characters, 16, 2, &mut last);
                if count == 0 {
                    return Err(failure(EscapeErrorKind::HexWithoutDigits, last));
                }
                if value == 0 {
                    return Err(failure(EscapeErrorKind::Nul, last));
                }
                piece(EscapePiece::Byte(value as u8));
                byte_escape = true;
            }
            'u' | 'U' => {
                let upper = escape == 'U';
                let limit = if upper { 8 } else { 4 };
                let braced = characters.next_if(|&(_, next)| next == '{');
                if let Some((offset, _)) = braced {
                    last = offset;
                }
                let braced = braced.is_some();
                let (value, count) = escape_digits(&mut characters, 16, limit, &mut last);
                if count == 0 {
                    return Err(failure(
                        EscapeErrorKind::UnicodeWithoutDigits { upper },
                        last,
                    ));
                }
                if braced {
                    // R reads the character after the digits to see whether it closes the
                    // brace, so that character is where it reports one that does not.
                    match characters.next() {
                        Some((offset, '}')) => last = offset,
                        unclosed => {
                            return Err(failure(
                                EscapeErrorKind::UnicodeUnclosed { upper },
                                unclosed.map_or(text.len(), |(offset, _)| offset),
                            ));
                        }
                    }
                }
                let surrogate = (0xD800..0xE000).contains(&value).then_some('\u{fffd}');
                let Some(decoded) = char::from_u32(value).or(surrogate) else {
                    return Err(failure(
                        EscapeErrorKind::UnicodeInvalid {
                            upper,
                            braced,
                            value,
                        },
                        last,
                    ));
                };
                if value == 0 {
                    return Err(failure(EscapeErrorKind::Nul, last));
                }
                unicode = true;
                piece(EscapePiece::Unicode(decoded));
            }
            other => return Err(failure(EscapeErrorKind::Unrecognized(other), last)),
        }
    }
    if unicode && byte_escape {
        return Err(EscapeError {
            kind: EscapeErrorKind::Mixed,
            backslash: 0,
            last: text.len(),
        });
    }
    Ok(())
}

/// Reads up to `limit` digits in `radix`, moving `last` to the offset of the last one read.
fn escape_digits(
    characters: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    radix: u32,
    limit: usize,
    last: &mut usize,
) -> (u32, usize) {
    let mut value = 0u32;
    let mut count = 0;
    while count < limit {
        let Some((offset, digit)) = characters
            .peek()
            .and_then(|&(offset, next)| Some((offset, next.to_digit(radix)?)))
        else {
            break;
        };
        value = value * radix + digit;
        *last = offset;
        characters.next();
        count += 1;
    }
    (value, count)
}

fn escape_message(kind: EscapeErrorKind) -> String {
    let letter = |upper| if upper { 'U' } else { 'u' };
    match kind {
        EscapeErrorKind::Nul => {
            "this escape names the NUL character, which an R string cannot hold".to_owned()
        }
        EscapeErrorKind::OctalTooLarge(value) => {
            format!("`\\{value:o}` is above `\\377`, the largest octal escape")
        }
        EscapeErrorKind::HexWithoutDigits => "`\\x` needs a hex digit after it".to_owned(),
        EscapeErrorKind::UnicodeWithoutDigits { upper } => {
            format!("`\\{}` needs a hex digit after it", letter(upper))
        }
        EscapeErrorKind::UnicodeUnclosed { upper } => {
            format!(
                "`\\{}{{` needs a closing `}}` after its digits",
                letter(upper)
            )
        }
        EscapeErrorKind::UnicodeInvalid { value, .. } => {
            format!("`{value:x}` is not a Unicode code point; the largest is `10ffff`")
        }
        EscapeErrorKind::Unrecognized(character) => {
            format!("`\\{character}` is not an escape R knows. Write `\\\\` for a backslash")
        }
        EscapeErrorKind::Mixed => {
            "a string cannot mix `\\u` or `\\U` escapes with octal or `\\x` escapes".to_owned()
        }
    }
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

    /// An error on a token R's own lexer refuses, which R's parser then names as what it did
    /// not expect.
    fn refused_token(&mut self, message: impl Into<String>, start: usize) -> SyntaxKind {
        self.error(message, start);
        if let Some(error) = self.errors.last_mut() {
            error.unexpected = Unexpected::Token(error.range);
        }
        ERROR_TOKEN
    }

    /// An error on text R reads as something else and only ry's rules refuse.
    fn error_rule(&mut self, message: impl Into<String>, start: usize) {
        self.error(message, start);
        if let Some(error) = self.errors.last_mut() {
            error.unexpected = Unexpected::Nothing;
        }
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
            _ => self.refused_token(unexpected_character_message(first), start),
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
                return self.refused_token("missing digits after `0x`", start);
            }
            if self.peek() == Some('.') {
                return self.refused_token("a hex literal has at most one `.`", start);
            }
            if !self.exponent(['p', 'P']) {
                return self.refused_token("missing digits after the exponent's `p`", start);
            }
        } else {
            self.eat_while(|c| c.is_ascii_digit());
            // A number that starts with its `.` has read its fraction: `.5.5` is two numbers.
            if !self.text[start..].starts_with('.') && self.eat(".") {
                self.eat_while(|c| c.is_ascii_digit());
            }
            if !self.exponent(['e', 'E']) {
                return self.refused_token("missing digits after the exponent's `e`", start);
            }
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
        self.error_rule(
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
        let start = self.pos;
        let mut escaped = false;
        // The scan stops only at ASCII bytes, so it never splits a character.
        while let Some(&byte) = bytes.get(self.pos) {
            match byte {
                _ if byte == quote => break,
                b'\\' => {
                    escaped = true;
                    self.pos += 1;
                    self.bump();
                }
                b'{' | b'}' if bytes.get(self.pos + 1) == Some(&byte) => self.pos += 2,
                b'{' => break,
                b'}' => {
                    let brace = self.pos;
                    self.pos += 1;
                    self.error_rule("a `}` in an f-string's text is written `}}`", brace);
                }
                _ => self.pos += 1,
            }
        }
        // An f-string is not R, so R has no report for its escapes and ry's message stands.
        if escaped && let Err(error) = unescape_quoted(&self.text[start..self.pos], |_| {}) {
            self.error_rule(escape_message(error.kind), start + error.backslash);
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

    /// Reads an exponent if one starts here. `false` for a marker without digits, which R's
    /// lexer refuses: `1e` is not the number `1` and the name `e`.
    fn exponent(&mut self, markers: [char; 2]) -> bool {
        if !self.peek().is_some_and(|c| markers.contains(&c)) {
            return true;
        }
        self.bump();
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        let digits_start = self.pos;
        self.eat_while(|c| c.is_ascii_digit());
        self.pos != digits_start
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
        let mut escaped = false;
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
                // An escaped line break belongs to a string, and ends an annotation's line.
                b'\\' if annotation && matches!(self.peek(), Some('\n' | '\r')) => break,
                b'\\' => {
                    escaped = true;
                    self.bump();
                }
                byte if byte == quote => {
                    if escaped {
                        self.check_escapes(start);
                    }
                    if kind == IDENT && self.pos == start + 2 {
                        let mut error = SyntaxError::new(
                            "a name cannot be empty: write at least one character between the backquotes",
                            TextRange::new(
                                TextSize::from(start as u32),
                                TextSize::from(self.pos as u32),
                            ),
                        );
                        error.unexpected = Unexpected::Classed(Classed::EmptyName);
                        self.errors.push(error);
                    }
                    return kind;
                }
                _ => break,
            }
        }
        self.end_unterminated_at_line_break(start);
        self.error(unterminated, start);
        kind
    }

    /// Reports the first escape R's lexer rejects in the quoted token from `start` to here.
    fn check_escapes(&mut self, start: usize) {
        let inner = &self.text[start + 1..self.pos - 1];
        let Err(error) = unescape_quoted(inner, |_| {}) else {
            return;
        };
        let range = match error.kind {
            EscapeErrorKind::Mixed => start..self.pos,
            _ => {
                let last = start + 1 + error.last;
                let end = self.text[last..]
                    .chars()
                    .next()
                    .map_or(last, |character| last + character.len_utf8());
                start + 1 + error.backslash..end.min(self.pos - 1)
            }
        };
        let mut error = SyntaxError::new(
            escape_message(error.kind),
            TextRange::new(
                TextSize::from(range.start as u32),
                TextSize::from(range.end as u32),
            ),
        );
        error.unexpected = Unexpected::Classed(Classed::Escape);
        self.errors.push(error);
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
            self.refused_token("unterminated `%` operator; expected a closing `%`", start)
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
