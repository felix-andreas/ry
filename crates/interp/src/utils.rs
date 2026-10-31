//! The utils package's native routines: reading and writing delimited data, type conversion,
//! object sizes, hash tables and checksums, and the routines that need a console, a network
//! or an editor, which fail with a clear error in a non-interactive session.
//!
//! Base's `scan` lives here too, because `read.table` drives it together with
//! `readtablehead` and `countfields`, and all three share one tokenizer.

use crate::{
    builtin_libraries::{NativeCall, NativeFn},
    builtins::{Arg, BuiltinDef, Call, internal},
    coerce::{coerce_vector, parse_real},
    conditions::warning,
    format::{format_complex_alone, format_real_alone_scipen},
    unwind::{R, error},
    values::{
        self, alloc, length, na_string, nil, scalar_bool, scalar_integer, scalar_string,
        string_vector, symbols,
    },
    write_console,
};
use heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType};

pub const BUILTINS: &[BuiltinDef] = &[internal("scan", 19, do_scan)];

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("readtablehead", do_read_table_head),
    ("countfields", do_count_fields),
    ("typeconvert", do_type_convert),
    ("writetable", do_write_table),
    ("crc64", do_crc64),
    ("objectSize", do_object_size),
    ("octsize", do_octal_size),
    ("charClass", do_char_class),
    ("flushconsole", do_flush_console),
    ("processevents", |_| Ok(nil())),
    ("tzcode_type", |_| unsafe { scalar_string("internal") }),
    ("nsl", do_nsl),
    ("menu", |_| {
        Err(error("menu() cannot be used non-interactively"))
    }),
    ("hashtab_Ext", do_hashtab),
    ("gethash_Ext", do_gethash),
    ("sethash_Ext", do_sethash),
    ("remhash_Ext", do_remhash),
    ("numhash_Ext", do_numhash),
    ("typhash_Ext", do_typhash),
    ("maphash_Ext", do_maphash),
    ("clrhash_Ext", do_clrhash),
    ("ishashtab_Ext", do_is_hashtab),
    ("Rprof", do_rprof),
    ("Rprofmem", do_rprofmem),
    ("unzip", do_unzip),
    ("download", |_| {
        Err(error(
            "the \"internal\" download method is not supported in ry; use method = \"libcurl\"",
        ))
    }),
    ("sockconnect", no_sockets),
    ("sockread", no_sockets),
    ("sockclose", no_sockets),
    ("sockopen", no_sockets),
    ("socklisten", no_sockets),
    ("sockwrite", no_sockets),
    ("addhistory", |_| Ok(nil())),
    // The `.External2` routines report errors against their own call, as GNU R's do.
    ("loadhistory", |call| {
        Err(call.error("no history mechanism available"))
    }),
    ("savehistory", |call| {
        Err(call.error("no history available to save"))
    }),
    ("dataentry", |call| {
        Err(call.error("data entry cannot be used non-interactively"))
    }),
    ("dataviewer", |call| {
        Err(call.error("View() cannot be used non-interactively"))
    }),
    ("edit", |call| {
        Err(call.error("edit() cannot be used non-interactively"))
    }),
    ("fileedit", |call| {
        Err(call.error("file.edit() cannot be used non-interactively"))
    }),
    ("selectlist", |call| {
        Err(call.error("select.list() cannot be used non-interactively"))
    }),
];

unsafe fn no_sockets(_: &NativeCall<'_>) -> R<Sexp> {
    Err(error("sockets are not supported in ry yet"))
}

// The tokenizer.

/// How fields are delimited and quoted.
struct Syntax {
    /// `None` separates fields by white space.
    separator: Option<u8>,
    quotes: Vec<u8>,
    comment: Option<u8>,
    strip_white: bool,
    allow_escapes: bool,
}

impl Syntax {
    unsafe fn from_arguments(separator: Sexp, quotes: Sexp, comment: Sexp) -> R<Syntax> {
        let separator = match values::first_string(separator).as_deref() {
            None | Some("") => None,
            Some(text) if text.len() == 1 => Some(text.as_bytes()[0]),
            Some(_) => return Err(error("invalid 'sep' value: must be one byte")),
        };
        let quotes = values::first_string(quotes)
            .map(|text| text.as_bytes().to_vec())
            .unwrap_or_default();
        let comment = match values::first_string(comment).as_deref() {
            None | Some("") => None,
            Some(text) if text.len() == 1 => Some(text.as_bytes()[0]),
            Some(_) => return Err(error("invalid 'comment.char' argument")),
        };
        Ok(Syntax {
            separator,
            quotes,
            comment,
            strip_white: false,
            allow_escapes: false,
        })
    }
}

/// What ended a field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum End {
    Separator,
    Line,
    File,
}

struct Field {
    text: String,
}

/// Reads fields from input bytes, keeping its position so the caller can store it back.
struct Scanner<'a> {
    input: &'a [u8],
    position: usize,
    syntax: &'a Syntax,
    /// The line being read, from 1.
    line: usize,
    eof_in_quote: bool,
    at_line_start: bool,
}

impl<'a> Scanner<'a> {
    fn new(input: &'a [u8], position: usize, syntax: &'a Syntax) -> Scanner<'a> {
        Scanner {
            input,
            position,
            syntax,
            line: 1,
            eof_in_quote: false,
            at_line_start: true,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    /// Consumes a line end (`\n`, `\r\n` or a lone `\r`) if one is next.
    fn take_line_end(&mut self) -> bool {
        match self.peek() {
            Some(b'\n') => {
                self.position += 1;
                self.line += 1;
                true
            }
            Some(b'\r') => {
                self.position += 1;
                if self.peek() == Some(b'\n') {
                    self.position += 1;
                }
                self.line += 1;
                true
            }
            _ => false,
        }
    }

    fn skip_rest_of_line(&mut self) -> End {
        while let Some(byte) = self.peek() {
            if byte == b'\n' || byte == b'\r' {
                self.take_line_end();
                return End::Line;
            }
            self.position += 1;
        }
        End::File
    }

    /// Skips `count` lines without looking at their content.
    fn skip_lines(&mut self, count: usize) {
        for _ in 0..count {
            if self.peek().is_none() {
                break;
            }
            self.skip_rest_of_line();
        }
        self.line = 1;
    }

    /// An escape sequence after a backslash, as `allowEscapes` reads it.
    fn escape(&mut self) -> Vec<u8> {
        let Some(byte) = self.peek() else {
            return vec![b'\\'];
        };
        self.position += 1;
        let simple = match byte {
            b'n' => Some(b'\n'),
            b't' => Some(b'\t'),
            b'r' => Some(b'\r'),
            b'a' => Some(7),
            b'b' => Some(8),
            b'f' => Some(12),
            b'v' => Some(11),
            b'\\' => Some(b'\\'),
            b'"' => Some(b'"'),
            b'\'' => Some(b'\''),
            b' ' => Some(b' '),
            b'\n' => Some(b'\n'),
            _ => None,
        };
        if let Some(simple) = simple {
            return vec![simple];
        }
        let (radix, limit) = match byte {
            b'0'..=b'7' => {
                self.position -= 1;
                (8, 3)
            }
            b'x' => (16, 2),
            b'u' => (16, 4),
            b'U' => (16, 8),
            other => return vec![b'\\', other],
        };
        let mut code = 0u32;
        let mut digits = 0;
        while digits < limit {
            let Some(digit) = self.peek().and_then(|byte| (byte as char).to_digit(radix)) else {
                break;
            };
            code = code * radix + digit;
            self.position += 1;
            digits += 1;
        }
        if matches!(byte, b'u' | b'U') {
            let character = char::from_u32(code).unwrap_or('\u{FFFD}');
            return character.to_string().into_bytes();
        }
        vec![code as u8]
    }

    /// The next field and what ended it. `None` means the terminator came before any field: a
    /// blank line, the end of the input, or (between fields) only white space.
    fn next_field(&mut self) -> (Option<Field>, End) {
        let field = match self.syntax.separator {
            None => self.next_white_field(),
            Some(separator) => self.next_separated_field(separator),
        };
        self.at_line_start = field.1 != End::Separator;
        field
    }

    fn next_white_field(&mut self) -> (Option<Field>, End) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.position += 1;
        }
        let Some(first) = self.peek() else {
            return (None, End::File);
        };
        if first == b'\n' || first == b'\r' {
            self.take_line_end();
            return (None, End::Line);
        }
        if Some(first) == self.syntax.comment {
            return (None, self.skip_rest_of_line());
        }
        let mut text = Vec::new();
        if self.syntax.quotes.contains(&first) {
            self.position += 1;
            loop {
                let Some(byte) = self.peek() else {
                    self.eof_in_quote = true;
                    break;
                };
                self.position += 1;
                if byte == first {
                    break;
                }
                if byte == b'\\' {
                    if self.syntax.allow_escapes {
                        text.extend(self.escape());
                        continue;
                    }
                    if self.peek() == Some(first) {
                        text.push(first);
                        self.position += 1;
                        continue;
                    }
                }
                if byte == b'\n' {
                    self.line += 1;
                }
                text.push(byte);
            }
        } else {
            while let Some(byte) = self.peek() {
                if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') || Some(byte) == self.syntax.comment
                {
                    break;
                }
                self.position += 1;
                if byte == b'\\' && self.syntax.allow_escapes {
                    let escaped = self.escape();
                    // An escaped blank delimits the field, as the character itself would.
                    if matches!(escaped.as_slice(), [b' ' | b'\t' | b'\n']) {
                        break;
                    }
                    text.extend(escaped);
                    continue;
                }
                text.push(byte);
            }
        }
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.position += 1;
        }
        let end = match self.peek() {
            None => End::File,
            Some(b'\n' | b'\r') => {
                self.take_line_end();
                End::Line
            }
            Some(byte) if Some(byte) == self.syntax.comment => self.skip_rest_of_line(),
            Some(_) => End::Separator,
        };
        (
            Some(Field {
                text: String::from_utf8_lossy(&text).into_owned(),
            }),
            end,
        )
    }

    fn next_separated_field(&mut self, separator: u8) -> (Option<Field>, End) {
        let line_start = self.at_line_start;
        if self.peek().is_none() {
            return (None, End::File);
        }
        if self.syntax.strip_white {
            while matches!(self.peek(), Some(b' ' | b'\t')) && Some(b' ') != Some(separator) {
                self.position += 1;
            }
        }
        let mut text = Vec::new();
        let mut quoted = false;
        let end = loop {
            let Some(byte) = self.peek() else {
                break End::File;
            };
            if byte == b'\n' || byte == b'\r' {
                self.take_line_end();
                break End::Line;
            }
            if byte == separator {
                self.position += 1;
                break End::Separator;
            }
            if Some(byte) == self.syntax.comment {
                break self.skip_rest_of_line();
            }
            self.position += 1;
            if self.syntax.quotes.contains(&byte) {
                quoted = true;
                loop {
                    let Some(inner) = self.peek() else {
                        self.eof_in_quote = true;
                        break;
                    };
                    self.position += 1;
                    if inner == byte {
                        if self.peek() == Some(byte) {
                            text.push(byte);
                            self.position += 1;
                            continue;
                        }
                        break;
                    }
                    if inner == b'\\' && self.syntax.allow_escapes {
                        text.extend(self.escape());
                        continue;
                    }
                    if inner == b'\n' {
                        self.line += 1;
                    }
                    text.push(inner);
                }
                continue;
            }
            if byte == b'\\' && self.syntax.allow_escapes {
                text.extend(self.escape());
                continue;
            }
            text.push(byte);
        };
        if self.syntax.strip_white {
            while matches!(text.last(), Some(b' ' | b'\t')) {
                text.pop();
            }
        }
        if line_start && text.is_empty() && !quoted && end != End::Separator {
            return (None, end);
        }
        (
            Some(Field {
                text: String::from_utf8_lossy(&text).into_owned(),
            }),
            end,
        )
    }
}

// scan.

/// The type a field converts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
}

impl Kind {
    fn of(kind: SexpType) -> Option<Kind> {
        Some(match kind {
            SexpType::Logical => Kind::Logical,
            SexpType::Integer => Kind::Integer,
            SexpType::Real => Kind::Double,
            SexpType::Complex => Kind::Complex,
            SexpType::String => Kind::Character,
            SexpType::Raw => Kind::Raw,
            _ => return None,
        })
    }

    fn sexp_type(self) -> SexpType {
        match self {
            Kind::Logical => SexpType::Logical,
            Kind::Integer => SexpType::Integer,
            Kind::Double => SexpType::Real,
            Kind::Complex => SexpType::Complex,
            Kind::Character => SexpType::String,
            Kind::Raw => SexpType::Raw,
        }
    }

    fn expected(self) -> &'static str {
        match self {
            Kind::Logical => "a logical",
            Kind::Integer => "an integer",
            Kind::Double => "a real",
            Kind::Complex => "a complex",
            Kind::Character => "a string",
            Kind::Raw => "a raw",
        }
    }
}

/// A converted field.
enum Value {
    Missing,
    Logical(i32),
    Integer(i32),
    Double(f64),
    Complex(Rcomplex),
    Text(String),
    Raw(u8),
}

/// One output column of `scan`: its type, or `None` for a column `what` skips with `NULL`.
struct Column {
    kind: Option<Kind>,
    values: Vec<Value>,
}

/// `scan(file, what, nmax, sep, dec, quote, skip, nlines, na.strings, flush, fill,
/// strip.white, quiet, blank.lines.skip, multi.line, comment.char, allowEscapes, encoding,
/// skipNul)`.
unsafe fn do_scan(call: &Call<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let what = call.arg(1);
    let max_items = values::as_integer_scalar(call.arg(2));
    let mut syntax = Syntax::from_arguments(call.arg(3), call.arg(5), call.arg(15))?;
    let decimal = decimal_mark(call.arg(4))?;
    let skip = values::as_integer_scalar(call.arg(6)).max(0) as usize;
    let max_lines = values::as_integer_scalar(call.arg(7));
    let na_strings = na_strings(call.arg(8));
    let flush = values::as_logical_scalar(call.arg(9)) == 1;
    let fill = values::as_logical_scalar(call.arg(10)) == 1;
    let strip = call.arg(11);
    syntax.strip_white = strip.kind() == SexpType::Logical && values::ints(strip).contains(&1);
    let quiet = values::as_logical_scalar(call.arg(12)) == 1;
    let blank_lines_skip = values::as_logical_scalar(call.arg(13)) != 0;
    let multi_line = values::as_logical_scalar(call.arg(14)) != 0;
    syntax.allow_escapes = values::as_logical_scalar(call.arg(16)) == 1;
    let is_list = what.kind() == SexpType::List;
    let mut columns: Vec<Column> = if is_list {
        let mut columns = Vec::with_capacity(length(what));
        for index in 0..length(what) {
            let element = what.element(index);
            let kind = if element == nil() {
                None
            } else {
                Some(Kind::of(element.kind()).ok_or_else(|| error("invalid 'what' specified"))?)
            };
            columns.push(Column {
                kind,
                values: Vec::new(),
            });
        }
        if columns.is_empty() {
            return Err(error("empty 'what' specified"));
        }
        columns
    } else {
        let kind = Kind::of(what.kind()).ok_or_else(|| error("invalid 'what' specified"))?;
        vec![Column {
            kind: Some(kind),
            values: Vec::new(),
        }]
    };
    let settings = Conversion {
        decimal,
        na_strings: &na_strings,
    };
    let outcome = crate::connections::with_input(file, |input, position| {
        let mut scanner = Scanner::new(input, *position, &syntax);
        scanner.skip_lines(skip);
        let limits = Limits {
            max_items,
            max_lines,
        };
        let result = if is_list {
            scan_records(
                &mut scanner,
                &mut columns,
                &settings,
                limits,
                RecordRules {
                    fill,
                    flush,
                    multi_line,
                    blank_lines_skip,
                },
            )
        } else {
            scan_items(&mut scanner, &mut columns[0], &settings, limits, flush)
        };
        *position = scanner.position;
        result.map(|count| (count, scanner.eof_in_quote))
    })?;
    let (count, eof_in_quote) = outcome?;
    if eof_in_quote {
        warning("EOF within quoted string")?;
    }
    if !quiet {
        let noun = match (is_list, count) {
            (true, 1) => "record",
            (true, _) => "records",
            (false, 1) => "item",
            (false, _) => "items",
        };
        write_console(format!("Read {count} {noun}\n").as_bytes(), true);
    }
    if !is_list {
        return column_vector(&columns[0]);
    }
    let mut vectors = Vec::with_capacity(columns.len());
    for column in &columns {
        vectors.push(column_vector(column)?);
        crate::heap().root(*vectors.last().unwrap_or(&nil()));
    }
    let result = values::list_vector(&vectors)?;
    let names = values::get_attr(what, symbols().names);
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

#[derive(Clone, Copy)]
struct Limits {
    /// Items, or records for a list `what`; zero or less for no limit.
    max_items: i32,
    /// Lines; zero or less for no limit.
    max_lines: i32,
}

#[derive(Clone, Copy)]
struct RecordRules {
    fill: bool,
    flush: bool,
    multi_line: bool,
    blank_lines_skip: bool,
}

struct Conversion<'a> {
    decimal: u8,
    na_strings: &'a [String],
}

/// Reads fields into one vector; returns the number of items.
fn scan_items(
    scanner: &mut Scanner<'_>,
    column: &mut Column,
    settings: &Conversion<'_>,
    limits: Limits,
    flush: bool,
) -> Result<usize, crate::unwind::Unwind> {
    let kind = column.kind.unwrap_or(Kind::Character);
    let mut lines = 0;
    loop {
        if limits.max_items > 0 && column.values.len() >= limits.max_items as usize {
            break;
        }
        let (field, mut end) = scanner.next_field();
        if let Some(field) = field {
            column
                .values
                .push(convert_field(&field.text, kind, settings)?);
            if flush && end == End::Separator {
                end = scanner.skip_rest_of_line();
            }
        }
        if end != End::Separator {
            lines += 1;
            if end == End::File || (limits.max_lines > 0 && lines >= limits.max_lines) {
                break;
            }
        }
    }
    Ok(column.values.len())
}

/// Reads fields into records of one field per column; returns the number of records.
fn scan_records(
    scanner: &mut Scanner<'_>,
    columns: &mut [Column],
    settings: &Conversion<'_>,
    limits: Limits,
    rules: RecordRules,
) -> Result<usize, crate::unwind::Unwind> {
    let width = columns.len();
    let mut records = 0;
    let mut filled = 0;
    let mut lines = 0;
    let mut fields_on_line = 0;
    loop {
        if limits.max_items > 0 && records >= limits.max_items as usize && filled == 0 {
            break;
        }
        let line = scanner.line;
        let (field, mut end) = scanner.next_field();
        if let Some(field) = field {
            fields_on_line += 1;
            let column = &mut columns[filled];
            if let Some(kind) = column.kind {
                column
                    .values
                    .push(convert_field(&field.text, kind, settings)?);
            }
            filled += 1;
            if filled == width {
                records += 1;
                filled = 0;
                if rules.flush && end == End::Separator {
                    end = scanner.skip_rest_of_line();
                }
            }
        }
        if end == End::Separator {
            continue;
        }
        if fields_on_line == 0 {
            if end == End::File {
                break;
            }
            // A blank line that is kept reads as one empty field.
            if !rules.blank_lines_skip {
                let column = &mut columns[filled];
                if let Some(kind) = column.kind {
                    column.values.push(convert_field("", kind, settings)?);
                }
                filled += 1;
                if filled == width {
                    records += 1;
                    filled = 0;
                }
                if filled > 0 && rules.fill {
                    pad_record(columns, filled);
                    records += 1;
                    filled = 0;
                }
            }
        } else if filled > 0 {
            if rules.fill {
                pad_record(columns, filled);
                records += 1;
                filled = 0;
            } else if !rules.multi_line {
                return Err(error(format!("line {line} did not have {width} elements")));
            }
        }
        fields_on_line = 0;
        lines += 1;
        if end == End::File || (limits.max_lines > 0 && lines >= limits.max_lines) {
            break;
        }
    }
    if filled > 0 {
        pad_record(columns, filled);
        records += 1;
    }
    Ok(records)
}

fn pad_record(columns: &mut [Column], from: usize) {
    for column in &mut columns[from..] {
        if column.kind.is_some() {
            column.values.push(Value::Missing);
        }
    }
}

/// A field converted to `kind`, or the error `scan` reports for it.
fn convert_field(text: &str, kind: Kind, settings: &Conversion<'_>) -> R<Value> {
    if kind == Kind::Character {
        if settings.na_strings.iter().any(|na| na == text) {
            return Ok(Value::Missing);
        }
        return Ok(Value::Text(text.to_owned()));
    }
    let trimmed = text.trim_matches(|character| character == ' ' || character == '\t');
    if trimmed.is_empty()
        || settings
            .na_strings
            .iter()
            .any(|na| na == trimmed || na == text)
    {
        return Ok(Value::Missing);
    }
    let value = match kind {
        Kind::Logical => match trimmed {
            "T" | "TRUE" | "true" | "True" => Some(Value::Logical(1)),
            "F" | "FALSE" | "false" | "False" => Some(Value::Logical(0)),
            _ => None,
        },
        Kind::Integer => parse_integer(trimmed).map(Value::Integer),
        Kind::Double => parse_double(trimmed, settings.decimal).map(Value::Double),
        Kind::Complex => parse_complex(trimmed, settings.decimal).map(Value::Complex),
        Kind::Raw => {
            let valid = trimmed.len() <= 2 && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit());
            valid
                .then(|| u8::from_str_radix(trimmed, 16).ok())
                .flatten()
                .map(Value::Raw)
        }
        Kind::Character => None,
    };
    value.ok_or_else(|| {
        error(format!(
            "scan() expected '{}', got '{}'",
            kind.expected(),
            text
        ))
    })
}

/// A decimal integer that fits an R integer.
fn parse_integer(text: &str) -> Option<i32> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: i64 = text.trim_start_matches('+').parse().ok()?;
    if value > i32::MAX as i64 || value <= i32::MIN as i64 {
        return None;
    }
    Some(value as i32)
}

/// A double as R reads one, with `decimal` as the decimal mark.
fn parse_double(text: &str, decimal: u8) -> Option<f64> {
    if decimal == b'.' {
        return parse_real(text);
    }
    if text.contains('.') {
        return None;
    }
    parse_real(&text.replace(decimal as char, "."))
}

/// A complex number written `a`, `bi` or `a+bi`.
fn parse_complex(text: &str, decimal: u8) -> Option<Rcomplex> {
    let Some(body) = text.strip_suffix('i') else {
        return parse_double(text, decimal).map(|r| Rcomplex { r, i: 0.0 });
    };
    let bytes = body.as_bytes();
    let split = (1..bytes.len()).rev().find(|&index| {
        matches!(bytes[index], b'+' | b'-') && !matches!(bytes[index - 1], b'e' | b'E')
    });
    match split {
        Some(index) => {
            let r = parse_double(&body[..index], decimal)?;
            let i = parse_double(&body[index..], decimal)?;
            Some(Rcomplex { r, i })
        }
        None => parse_double(body, decimal).map(|i| Rcomplex { r: 0.0, i }),
    }
}

unsafe fn column_vector(column: &Column) -> R<Sexp> {
    let Some(kind) = column.kind else {
        return Ok(nil());
    };
    let count = column.values.len();
    let vector = alloc(kind.sexp_type(), count)?;
    for (index, value) in column.values.iter().enumerate() {
        match kind {
            Kind::Logical | Kind::Integer => {
                values::ints_mut(vector)[index] = match value {
                    Value::Logical(value) | Value::Integer(value) => *value,
                    _ => NA_INTEGER,
                }
            }
            Kind::Double => {
                values::reals_mut(vector)[index] = if let Value::Double(value) = value {
                    *value
                } else {
                    NA_REAL
                }
            }
            Kind::Complex => {
                values::complexes_mut(vector)[index] = if let Value::Complex(value) = value {
                    *value
                } else {
                    Rcomplex {
                        r: NA_REAL,
                        i: NA_REAL,
                    }
                }
            }
            Kind::Raw => {
                values::raws_mut(vector)[index] = if let Value::Raw(value) = value {
                    *value
                } else {
                    0
                }
            }
            Kind::Character => {
                let charsxp = match value {
                    Value::Text(text) => values::mkchar(text)?,
                    _ => na_string(),
                };
                values::set_elt(vector, index, charsxp);
            }
        }
    }
    Ok(vector)
}

unsafe fn decimal_mark(dec: Sexp) -> R<u8> {
    match values::first_string(dec).as_deref() {
        Some(text) if text.len() == 1 => Ok(text.as_bytes()[0]),
        _ => Err(error("invalid decimal separator")),
    }
}

unsafe fn na_strings(strings: Sexp) -> Vec<String> {
    if strings.kind() != SexpType::String {
        return Vec::new();
    }
    values::strings(strings)
        .into_iter()
        .map(|text| text.unwrap_or_else(|| "NA".to_owned()))
        .collect()
}

// readtablehead and countfields.

/// `readtablehead(file, nlines, comment.char, blank.lines.skip, quote, sep, skipNul)`: the
/// first `nlines` lines that are not blank, a quoted field running over line ends joining its
/// lines into one.
unsafe fn do_read_table_head(call: &NativeCall<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let count = values::as_integer_scalar(call.arg(1));
    if count == NA_INTEGER || count <= 0 {
        return Err(error("invalid 'nlines' argument"));
    }
    let blank_lines_skip = values::as_logical_scalar(call.arg(3)) != 0;
    let syntax = Syntax::from_arguments(call.arg(5), call.arg(4), call.arg(2))?;
    let (lines, incomplete, in_quote) = crate::connections::with_input(file, |input, position| {
        let mut lines = Vec::new();
        let mut incomplete = false;
        let mut in_quote = false;
        while lines.len() < count as usize && *position < input.len() {
            let (line, ended, quoted) = head_line(input, position, &syntax);
            incomplete = !ended;
            in_quote = quoted;
            if quoted {
                break;
            }
            let content_empty = match syntax.comment {
                Some(comment) => line.first().is_none_or(|byte| *byte == comment),
                None => line.is_empty(),
            };
            if blank_lines_skip && content_empty {
                continue;
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        (lines, incomplete, in_quote)
    })?;
    let description = crate::connections::description(file).unwrap_or_default();
    if in_quote {
        return Err(error(format!(
            "incomplete final line found by readTableHeader on '{description}'"
        )));
    }
    if incomplete {
        warning(format!(
            "incomplete final line found by readTableHeader on '{description}'"
        ))?;
    }
    string_vector(&lines)
}

/// One logical line: the bytes up to a line end outside quotes, whether a line end ended it,
/// and whether the input ran out inside a quote.
fn head_line(input: &[u8], position: &mut usize, syntax: &Syntax) -> (Vec<u8>, bool, bool) {
    let mut line = Vec::new();
    let mut quote: Option<u8> = None;
    let mut field_start = true;
    let mut in_comment = false;
    while let Some(&byte) = input.get(*position) {
        *position += 1;
        if quote.is_none() && (byte == b'\n' || byte == b'\r') {
            if byte == b'\r' && input.get(*position) == Some(&b'\n') {
                *position += 1;
            }
            return (line, true, false);
        }
        line.push(byte);
        if in_comment {
            continue;
        }
        match quote {
            Some(open) => {
                if byte == b'\\' && syntax.separator.is_none() {
                    if let Some(&next) = input.get(*position) {
                        line.push(next);
                        *position += 1;
                    }
                } else if byte == open {
                    quote = None;
                }
            }
            None => {
                if Some(byte) == syntax.comment {
                    in_comment = true;
                } else if syntax.quotes.contains(&byte)
                    && (syntax.separator.is_some() || field_start)
                {
                    quote = Some(byte);
                }
                field_start = match syntax.separator {
                    Some(separator) => byte == separator,
                    None => byte == b' ' || byte == b'\t',
                };
            }
        }
    }
    let quoted = quote.is_some();
    (line, false, quoted)
}

/// `countfields(file, sep, quote, skip, blank.lines.skip, comment.char)`: the number of fields
/// on each line, `NA` for a line whose last field continues on the next one.
unsafe fn do_count_fields(call: &NativeCall<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let syntax = Syntax::from_arguments(call.arg(1), call.arg(2), call.arg(5))?;
    let skip = values::as_integer_scalar(call.arg(3)).max(0) as usize;
    let blank_lines_skip = values::as_logical_scalar(call.arg(4)) != 0;
    let (counts, eof_in_quote) = crate::connections::with_input(file, |input, position| {
        let mut scanner = Scanner::new(input, *position, &syntax);
        scanner.skip_lines(skip);
        let mut counts = Vec::new();
        let mut fields = 0;
        let mut record_line = scanner.line;
        loop {
            let (field, end) = scanner.next_field();
            if field.is_some() {
                fields += 1;
            }
            if end == End::Separator {
                continue;
            }
            if fields == 0 && end == End::File {
                break;
            }
            // The lines a quoted field ran over count as unknown.
            let spanned = scanner
                .line
                .saturating_sub(record_line + usize::from(end == End::Line));
            counts.extend(std::iter::repeat_n(NA_INTEGER, spanned));
            if fields > 0 || !blank_lines_skip {
                counts.push(fields);
            }
            fields = 0;
            record_line = scanner.line;
            if end == End::File {
                break;
            }
        }
        *position = scanner.position;
        (counts, scanner.eof_in_quote)
    })?;
    if eof_in_quote {
        warning("EOF within quoted string")?;
    }
    values::integer_vector(&counts)
}

// type.convert.

/// `typeconvert(x, na.strings, as.is, dec, numerals, tryLogical)`: the narrowest of logical,
/// integer, double and complex that reads every element, or character (a factor unless
/// `as.is`). Blank strings and `na.strings` are missing.
unsafe fn do_type_convert(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("the first argument must be of mode character"));
    }
    let na = na_strings(call.arg(1));
    let as_is = values::as_logical_scalar(call.arg(2)) == 1;
    let decimal = decimal_mark(call.arg(3))?;
    let numerals = values::first_string(call.arg(4))
        .map(|text| text.into_owned())
        .unwrap_or_else(|| "allow.loss".to_owned());
    let try_logical = values::as_logical_scalar(call.arg(5)) != 0;
    let strings = values::strings(x);
    let missing = |text: &Option<String>| match text {
        None => true,
        Some(text) => na.iter().any(|candidate| candidate == text) || text.trim().is_empty(),
    };
    let present: Vec<&str> = strings
        .iter()
        .filter(|text| !missing(text))
        .filter_map(|text| text.as_deref())
        .collect();
    let result = if present
        .iter()
        .all(|text| try_logical && matches!(*text, "T" | "F" | "TRUE" | "FALSE"))
    {
        let result = alloc(SexpType::Logical, strings.len())?;
        for (index, text) in strings.iter().enumerate() {
            values::ints_mut(result)[index] = match text.as_deref() {
                _ if missing(text) => NA_LOGICAL,
                Some("T" | "TRUE") => 1,
                _ => 0,
            };
        }
        result
    } else if present
        .iter()
        .all(|text| parse_integer(text.trim_start()).is_some() && !text.ends_with([' ', '\t']))
    {
        let result = alloc(SexpType::Integer, strings.len())?;
        for (index, text) in strings.iter().enumerate() {
            values::ints_mut(result)[index] = if missing(text) {
                NA_INTEGER
            } else {
                text.as_deref()
                    .and_then(|text| parse_integer(text.trim_start()))
                    .unwrap_or(NA_INTEGER)
            };
        }
        result
    } else if present.iter().all(|text| convertible_double(text, decimal))
        && !loses_accuracy(&present, &numerals)?
    {
        let result = alloc(SexpType::Real, strings.len())?;
        for (index, text) in strings.iter().enumerate() {
            values::reals_mut(result)[index] = if missing(text) {
                NA_REAL
            } else {
                text.as_deref()
                    .and_then(|text| parse_double(text, decimal))
                    .unwrap_or(NA_REAL)
            };
        }
        result
    } else if !present.iter().all(|text| convertible_double(text, decimal))
        && present
            .iter()
            .all(|text| text.trim() != "NA" && parse_complex(text.trim(), decimal).is_some())
    {
        let result = alloc(SexpType::Complex, strings.len())?;
        for (index, text) in strings.iter().enumerate() {
            let na = Rcomplex {
                r: NA_REAL,
                i: NA_REAL,
            };
            values::complexes_mut(result)[index] = if missing(text) {
                na
            } else {
                text.as_deref()
                    .and_then(|text| parse_complex(text.trim(), decimal))
                    .unwrap_or(na)
            };
        }
        result
    } else {
        let is_na = |text: &Option<String>| match text {
            None => true,
            Some(text) => na.iter().any(|candidate| candidate == text),
        };
        if as_is {
            let result = alloc(SexpType::String, strings.len())?;
            for (index, text) in strings.iter().enumerate() {
                let charsxp = match text {
                    Some(text) if !is_na(&Some(text.clone())) => values::mkchar(text)?,
                    _ => na_string(),
                };
                values::set_elt(result, index, charsxp);
            }
            result
        } else {
            let mut levels: Vec<&str> = strings
                .iter()
                .filter(|text| !is_na(text))
                .filter_map(|text| text.as_deref())
                .collect();
            levels.sort_by(|left, right| crate::sort::collate(left, right));
            levels.dedup();
            let result = alloc(SexpType::Integer, strings.len())?;
            for (index, text) in strings.iter().enumerate() {
                values::ints_mut(result)[index] = match text.as_deref() {
                    Some(text) if !is_na(&Some(text.to_owned())) => levels
                        .binary_search_by(|level| crate::sort::collate(level, text))
                        .map_or(NA_INTEGER, |position| position as i32 + 1),
                    _ => NA_INTEGER,
                };
            }
            let levels = string_vector(&levels)?;
            values::set_attr(result, symbols().levels, levels)?;
            let class = scalar_string("factor")?;
            values::set_attr(result, symbols().class, class)?;
            result
        }
    };
    for attribute in [symbols().names, symbols().dim, symbols().dimnames] {
        let value = values::get_attr(x, attribute);
        if value != nil() {
            values::set_attr(result, attribute, value)?;
        }
    }
    Ok(result)
}

/// Whether `text` reads as a double for `type.convert`, where `NA` is only missing when
/// `na.strings` says so.
fn convertible_double(text: &str, decimal: u8) -> bool {
    text.trim() != "NA" && parse_double(text, decimal).is_some()
}

/// Whether reading `texts` as doubles loses digits under `numerals`, warning for
/// `"warn.loss"`; only `"no.loss"` makes the loss refuse the conversion.
unsafe fn loses_accuracy(texts: &[&str], numerals: &str) -> R<bool> {
    if numerals == "allow.loss" {
        return Ok(false);
    }
    for text in texts {
        let mantissa = text.trim().trim_start_matches(['+', '-']);
        let mantissa = mantissa.split(['e', 'E']).next().unwrap_or("");
        if mantissa.starts_with("0x") || mantissa.starts_with("0X") {
            continue;
        }
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        let significant = digits.trim_start_matches('0').trim_end_matches('0');
        if significant.len() > 15 {
            if numerals == "no.loss" {
                return Ok(true);
            }
            warning(format!(
                "accuracy loss in conversion from \"{text}\" to numeric"
            ))?;
        }
    }
    Ok(false)
}

// write.table.

/// `writetable(x, file, nrow, ncol, rnames, sep, eol, na, dec, quote, qmethod)`: the rows of a
/// data frame or matrix as delimited text. Numbers are written to 15 significant digits,
/// strings in the quoted columns between double quotes, with embedded quotes escaped by a
/// backslash when `qmethod` is `TRUE` and doubled otherwise.
unsafe fn do_write_table(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let file = call.arg(1);
    let rows = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let columns = values::as_integer_scalar(call.arg(3)).max(0) as usize;
    let row_names = call.arg(4);
    let separator = values::first_string(call.arg(5))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let eol = values::first_string(call.arg(6))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let na = values::first_string(call.arg(7))
        .map(|text| text.into_owned())
        .unwrap_or_else(|| "NA".to_owned());
    let decimal = values::first_string(call.arg(8))
        .map(|text| text.into_owned())
        .unwrap_or_else(|| ".".to_owned());
    if decimal.chars().count() != 1 {
        return Err(error("'dec' must be a single character"));
    }
    let quote = coerce_vector(call.arg(9), SexpType::Integer)?;
    let quoted: Vec<i32> = values::ints(quote).to_vec();
    let escape = values::as_logical_scalar(call.arg(10)) == 1;
    let quote_text = |text: &str| -> String {
        let inner = if escape {
            text.replace('"', "\\\"")
        } else {
            text.replace('"', "\"\"")
        };
        format!("\"{inner}\"")
    };
    let is_frame = x.kind() == SexpType::List;
    let mut cells: Vec<Cell> = Vec::with_capacity(columns);
    for column in 0..columns {
        let vector = if is_frame { x.element(column) } else { x };
        cells.push(Cell::new(vector)?);
    }
    let mut output = String::new();
    for row in 0..rows {
        if row_names != nil() {
            let name = values::string_at(row_names, row)
                .map_or_else(|| na.clone(), |name| name.into_owned());
            output.push_str(&if quoted.contains(&0) {
                quote_text(&name)
            } else {
                name
            });
            output.push_str(&separator);
        }
        for (column, cell) in cells.iter().enumerate() {
            if column > 0 {
                output.push_str(&separator);
            }
            let index = if is_frame { row } else { row + column * rows };
            match cell.text(index, &decimal)? {
                None => output.push_str(&na),
                Some((text, true)) if quoted.contains(&(column as i32 + 1)) => {
                    output.push_str(&quote_text(&text))
                }
                Some((text, _)) => output.push_str(&text),
            }
        }
        output.push_str(&eol);
        if output.len() > 1 << 16 {
            crate::connections::write_bytes(file, output.as_bytes())?;
            output.clear();
        }
    }
    crate::connections::write_bytes(file, output.as_bytes())?;
    Ok(nil())
}

/// A column being written: the vector and, for a factor, its levels.
struct Cell {
    vector: Sexp,
    levels: Option<Sexp>,
}

impl Cell {
    unsafe fn new(vector: Sexp) -> R<Cell> {
        let levels =
            values::inherits(vector, "factor").then(|| values::get_attr(vector, symbols().levels));
        Ok(Cell { vector, levels })
    }

    /// Element `index` as text, and whether it is a string (which quoting applies to); `None`
    /// for a missing value.
    unsafe fn text(&self, index: usize, decimal: &str) -> R<Option<(String, bool)>> {
        let vector = self.vector;
        if index >= length(vector) {
            return Ok(None);
        }
        let with_decimal = |text: String| {
            if decimal == "." {
                text
            } else {
                text.replace('.', decimal)
            }
        };
        Ok(match vector.kind() {
            _ if self.levels.is_some() => {
                let code = values::ints(vector)[index];
                match self.levels {
                    Some(levels)
                        if code != NA_INTEGER && code >= 1 && (code as usize) <= length(levels) =>
                    {
                        values::string_at(levels, code as usize - 1)
                            .map(|level| (level.into_owned(), true))
                    }
                    _ => None,
                }
            }
            SexpType::String => {
                values::string_at(vector, index).map(|text| (text.into_owned(), true))
            }
            SexpType::Logical => match values::ints(vector)[index] {
                NA_LOGICAL => None,
                0 => Some(("FALSE".to_owned(), false)),
                _ => Some(("TRUE".to_owned(), false)),
            },
            SexpType::Integer => match values::ints(vector)[index] {
                NA_INTEGER => None,
                value => Some((value.to_string(), false)),
            },
            SexpType::Real => {
                let value = values::reals(vector)[index];
                if value.is_nan() {
                    None
                } else {
                    Some((with_decimal(format_real_alone_scipen(value, 15)), false))
                }
            }
            SexpType::Complex => {
                let value = values::complexes(vector)[index];
                if value.r.is_nan() || value.i.is_nan() {
                    None
                } else {
                    Some((with_decimal(format_complex_alone(value, 15)), false))
                }
            }
            SexpType::Raw => Some((format!("{:02x}", values::raws(vector)[index]), false)),
            _ => {
                let element = crate::subset::list_or_vector_element(vector, index)?;
                let text = coerce_vector(element, SexpType::String)?;
                values::first_string(text).map(|text| (text.into_owned(), false))
            }
        })
    }
}

// Checksums and sizes.

/// `crc64(x)`: the CRC-64 of the first string, in lower-case hexadecimal.
unsafe fn do_crc64(call: &NativeCall<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(error("input must be a character string"));
    }
    let bytes = values::first_string(text)
        .map(|text| text.into_owned())
        .unwrap_or_default();
    scalar_string(&crate::system::crc64_hex(bytes.as_bytes()))
}

/// `objectSize(x)`: the bytes GNU R would use for `x` on a 64-bit platform, which is what
/// `object.size` reports.
unsafe fn do_object_size(call: &NativeCall<'_>) -> R<Sexp> {
    values::scalar_real(object_size(call.arg(0)) as f64)
}

const NODE_SIZE: usize = 56;
const VECTOR_HEADER: usize = 48;

/// The data part of a vector of `bytes`, as GNU R allocates it: small vectors come in classes
/// of 8, 16, 32, 48, 64 and 128 bytes, larger ones in multiples of 8.
fn vector_data_size(bytes: usize) -> usize {
    match bytes {
        0 => 0,
        1..=8 => 8,
        9..=16 => 16,
        17..=32 => 32,
        33..=48 => 48,
        49..=64 => 64,
        65..=128 => 128,
        _ => bytes.div_ceil(8) * 8,
    }
}

unsafe fn object_size(object: Sexp) -> usize {
    let size = match object.kind() {
        SexpType::Nil => return 0,
        SexpType::Symbol
        | SexpType::Environment
        | SexpType::Builtin
        | SexpType::Special
        | SexpType::ExternalPtr
        | SexpType::WeakRef
        | SexpType::Bytecode
        | SexpType::Object => NODE_SIZE,
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            NODE_SIZE
                + object_size(object.tag())
                + object_size(object.car())
                + object_size(object.cdr())
        }
        SexpType::Closure => {
            NODE_SIZE
                + object_size(object.car())
                + object_size(crate::bytecode::closure_body(object))
        }
        SexpType::Promise => NODE_SIZE + object_size(object.car()) + object_size(object.cdr()),
        SexpType::Char => VECTOR_HEADER + vector_data_size(object.bytes().len() + 1),
        SexpType::String => {
            let count = length(object);
            let mut seen = rustc_hash::FxHashSet::default();
            let mut strings = 0;
            for index in 0..count {
                let charsxp = object.element(index);
                if charsxp != na_string() && seen.insert(charsxp.as_ptr() as usize) {
                    strings += object_size(charsxp);
                }
            }
            VECTOR_HEADER + vector_data_size(count * 8) + strings
        }
        SexpType::List | SexpType::Expression => {
            let count = length(object);
            let elements: usize = (0..count)
                .map(|index| object_size(object.element(index)))
                .sum();
            VECTOR_HEADER + vector_data_size(count * 8) + elements
        }
        kind => {
            let element = kind.element_size().unwrap_or(8);
            VECTOR_HEADER + vector_data_size(length(object) * element)
        }
    };
    size + object_size(object.attrib())
}

/// `octsize(size)`: the low 11 octal digits of the size, as the raw bytes of a tar header
/// field. A missing size leaves the field zeroed.
unsafe fn do_octal_size(call: &NativeCall<'_>) -> R<Sexp> {
    let size = values::as_real_scalar(call.arg(0));
    let result = alloc(SexpType::Raw, 11)?;
    if !size.is_nan() {
        let bits = size as i64;
        for (index, byte) in values::raws_mut(result).iter_mut().enumerate() {
            *byte = b'0' + ((bits >> (3 * (10 - index))) & 7) as u8;
        }
    }
    Ok(result)
}

/// `charClass(x, class)`: whether each character of `x` (a string, or code points) belongs to
/// the named character class.
unsafe fn do_char_class(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let characters: Vec<Option<char>> = match x.kind() {
        SexpType::String if length(x) == 1 => values::string_at(x, 0)
            .map(|text| text.chars().map(Some).collect())
            .unwrap_or_default(),
        SexpType::Integer | SexpType::Real => {
            let codes = coerce_vector(x, SexpType::Integer)?;
            values::ints(codes)
                .iter()
                .map(|code| u32::try_from(*code).ok().and_then(char::from_u32))
                .collect()
        }
        _ => {
            return Err(error(
                "argument 'x' must be a length-1 character vector or an integer vector",
            ));
        }
    };
    let class = values::require_string(call.arg(1), "class")?;
    let test: fn(char) -> bool = match class.as_str() {
        "alnum" => char::is_alphanumeric,
        "alpha" => char::is_alphabetic,
        "blank" => |character| character == ' ' || character == '\t',
        "cntrl" => char::is_control,
        "digit" => |character| character.is_ascii_digit(),
        "graph" => |character| !character.is_control() && !character.is_whitespace(),
        "lower" => char::is_lowercase,
        "print" => |character| !character.is_control(),
        "punct" => |character| {
            !character.is_control() && !character.is_whitespace() && !character.is_alphanumeric()
        },
        "space" => char::is_whitespace,
        "upper" => char::is_uppercase,
        "xdigit" => |character| character.is_ascii_hexdigit(),
        _ => {
            return Err(error(
                "character class \"{class}\" is invalid".replace("{class}", &class),
            ));
        }
    };
    let result = alloc(SexpType::Logical, characters.len())?;
    for (index, character) in characters.iter().enumerate() {
        values::ints_mut(result)[index] =
            character.map_or(NA_LOGICAL, |character| test(character) as i32);
    }
    Ok(result)
}

// The console, the network and profiling.

unsafe fn do_flush_console(_: &NativeCall<'_>) -> R<Sexp> {
    use std::io::Write;
    std::io::stdout()
        .flush()
        .map_err(|failure| error(format!("cannot flush the console: {failure}")))?;
    Ok(nil())
}

/// `nsl(hostname)`: the host's IPv4 address, or `NULL` with a warning when it does not resolve.
unsafe fn do_nsl(call: &NativeCall<'_>) -> R<Sexp> {
    use std::net::ToSocketAddrs;
    let host = values::require_string(call.arg(0), "hostname")?;
    let address = (host.as_str(), 0)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.find(std::net::SocketAddr::is_ipv4));
    match address {
        Some(address) => scalar_string(&address.ip().to_string()),
        None => {
            warning(format!("nsl() was unable to resolve host '{host}'"))?;
            Ok(nil())
        }
    }
}

/// `Rprof(filename, ...)`: ry has no sampling profiler, so starting one warns and does
/// nothing; `Rprof(NULL)` stops nothing silently.
unsafe fn do_rprof(call: &NativeCall<'_>) -> R<Sexp> {
    if values::first_string(call.arg(0)).is_some_and(|name| !name.is_empty()) {
        warning("profiling is not supported in ry yet; Rprof() does nothing")?;
    }
    Ok(nil())
}

unsafe fn do_rprofmem(call: &NativeCall<'_>) -> R<Sexp> {
    if values::first_string(call.arg(0)).is_some_and(|name| !name.is_empty()) {
        warning("memory profiling is not supported in ry yet; Rprofmem() does nothing")?;
    }
    Ok(nil())
}

// Hash tables.

/// A hash table is a list of class `hashtab` holding one external pointer, as in GNU R. The
/// pointer's protected value holds the table: its type, the keys, the values, the key hashes
/// and the entry count. Entries live in insertion order, and a lookup compares hashes before
/// running `identical()` on the candidates.
const HASHTAB_MARKER: &str = "ry hashtab";

struct Table {
    pointer: Sexp,
    state: Sexp,
}

const STATE_TYPE: usize = 0;
const STATE_KEYS: usize = 1;
const STATE_VALUES: usize = 2;
const STATE_HASHES: usize = 3;
const STATE_COUNT: usize = 4;

/// `hashtab_Ext(type, K)`: a table for `2^K` entries before it grows.
unsafe fn do_hashtab(call: &NativeCall<'_>) -> R<Sexp> {
    let kind = values::require_string(call.arg(0), "type")?;
    if kind != "identical" && kind != "address" {
        return Err(error(format!("bad hash table type: {kind}")));
    }
    let exponent = values::as_integer_scalar(call.arg(1)).clamp(0, 24);
    let capacity = 1usize << exponent;
    let state = new_state(&kind, capacity)?;
    let marker = values::install(HASHTAB_MARKER)?;
    let pointer = crate::heap().alloc_external_ptr(std::ptr::null_mut(), marker, state)?;
    crate::heap().root(pointer);
    let table = values::list_vector(&[pointer])?;
    let class = scalar_string("hashtab")?;
    values::set_attr(table, symbols().class, class)?;
    Ok(table)
}

unsafe fn new_state(kind: &str, capacity: usize) -> R<Sexp> {
    let kind = scalar_string(kind)?;
    let keys = alloc(SexpType::List, capacity)?;
    let entries = alloc(SexpType::List, capacity)?;
    let hashes = alloc(SexpType::Real, capacity)?;
    let count = scalar_integer(0)?;
    values::list_vector(&[kind, keys, entries, hashes, count])
}

unsafe fn table_of(object: Sexp) -> Option<Table> {
    if object.kind() != SexpType::List
        || length(object) != 1
        || !values::inherits(object, "hashtab")
    {
        return None;
    }
    let pointer = object.element(0);
    if pointer.kind() != SexpType::ExternalPtr {
        return None;
    }
    let tag = pointer.external_tag();
    if tag.kind() != SexpType::Symbol || values::symbol_name(tag) != HASHTAB_MARKER {
        return None;
    }
    Some(Table {
        pointer,
        state: pointer.external_protected(),
    })
}

unsafe fn require_table(object: Sexp) -> R<Table> {
    table_of(object).ok_or_else(|| error("not a proper hash table object"))
}

impl Table {
    unsafe fn by_address(&self) -> bool {
        values::first_string(self.state.element(STATE_TYPE)).as_deref() == Some("address")
    }

    unsafe fn count(&self) -> usize {
        values::as_integer_scalar(self.state.element(STATE_COUNT)) as usize
    }

    unsafe fn set_count(&self, count: usize) {
        values::ints_mut(self.state.element(STATE_COUNT))[0] = count as i32;
    }

    unsafe fn key(&self, index: usize) -> Sexp {
        self.state.element(STATE_KEYS).element(index)
    }

    unsafe fn value(&self, index: usize) -> Sexp {
        self.state.element(STATE_VALUES).element(index)
    }

    unsafe fn hash_at(&self, index: usize) -> u64 {
        values::reals(self.state.element(STATE_HASHES))[index].to_bits()
    }

    unsafe fn hash(&self, key: Sexp) -> u64 {
        if self.by_address() {
            key.as_ptr() as u64
        } else {
            structural_hash(key, 3)
        }
    }

    unsafe fn find(&self, key: Sexp) -> Option<usize> {
        let hash = self.hash(key);
        let by_address = self.by_address();
        let flags = crate::sort::IdenticalFlags::default();
        (0..self.count()).find(|&index| {
            self.hash_at(index) == hash
                && if by_address {
                    self.key(index) == key
                } else {
                    crate::sort::identical(self.key(index), key, &flags)
                }
        })
    }

    unsafe fn insert(&self, key: Sexp, value: Sexp) -> R<()> {
        if let Some(index) = self.find(key) {
            values::set_elt(self.state.element(STATE_VALUES), index, value);
            return Ok(());
        }
        let count = self.count();
        let capacity = length(self.state.element(STATE_KEYS));
        if count == capacity {
            let kind = values::first_string(self.state.element(STATE_TYPE))
                .map(|text| text.into_owned())
                .unwrap_or_default();
            let grown = new_state(&kind, (capacity * 2).max(8))?;
            for index in 0..count {
                values::set_elt(grown.element(STATE_KEYS), index, self.key(index));
                values::set_elt(grown.element(STATE_VALUES), index, self.value(index));
                values::reals_mut(grown.element(STATE_HASHES))[index] =
                    values::reals(self.state.element(STATE_HASHES))[index];
            }
            crate::heap().set_external_protected(self.pointer, grown);
            let table = Table {
                pointer: self.pointer,
                state: grown,
            };
            table.set_count(count);
            return table.insert(key, value);
        }
        values::set_elt(self.state.element(STATE_KEYS), count, key);
        values::set_elt(self.state.element(STATE_VALUES), count, value);
        values::reals_mut(self.state.element(STATE_HASHES))[count] = f64::from_bits(self.hash(key));
        self.set_count(count + 1);
        Ok(())
    }

    unsafe fn remove(&self, index: usize) {
        let last = self.count() - 1;
        for slot in [STATE_KEYS, STATE_VALUES] {
            let vector = self.state.element(slot);
            values::set_elt(vector, index, vector.element(last));
            values::set_elt(vector, last, nil());
        }
        let hashes = values::reals_mut(self.state.element(STATE_HASHES));
        hashes[index] = hashes[last];
        self.set_count(last);
    }
}

/// A hash of `object` that agrees with `identical()`: equal objects hash equally, so only
/// the contents of vectors (to `depth` levels of lists) are mixed in.
unsafe fn structural_hash(object: Sexp, depth: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    let kind = object.kind();
    kind.code().hash(&mut hasher);
    match kind {
        SexpType::Logical | SexpType::Integer => values::ints(object).hash(&mut hasher),
        SexpType::Real => {
            for value in values::reals(object) {
                // Zero's sign and NaN payloads other than NA's do not matter to identical().
                let canonical = if *value == 0.0 {
                    0
                } else if value.is_nan() {
                    u64::from(values::is_na(*value))
                } else {
                    value.to_bits()
                };
                canonical.hash(&mut hasher);
            }
        }
        SexpType::String => {
            for index in 0..length(object) {
                object.element(index).bytes().hash(&mut hasher);
            }
        }
        SexpType::Raw => values::raws(object).hash(&mut hasher),
        SexpType::Symbol => (object.as_ptr() as usize).hash(&mut hasher),
        SexpType::List | SexpType::Expression if depth > 0 => {
            for index in 0..length(object) {
                structural_hash(object.element(index), depth - 1).hash(&mut hasher);
            }
        }
        _ => length(object).hash(&mut hasher),
    }
    hasher.finish()
}

/// `gethash_Ext(h, key, nomatch)`.
unsafe fn do_gethash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    Ok(table
        .find(call.arg(1))
        .map_or_else(|| call.arg(2), |index| table.value(index)))
}

/// `sethash_Ext(h, key, value)`: the value.
unsafe fn do_sethash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    table.insert(call.arg(1), call.arg(2))?;
    Ok(call.arg(2))
}

/// `remhash_Ext(h, key)`: whether there was an entry to remove.
unsafe fn do_remhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    let found = table.find(call.arg(1));
    if let Some(index) = found {
        table.remove(index);
    }
    scalar_bool(found.is_some())
}

unsafe fn do_numhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    scalar_integer(table.count() as i32)
}

unsafe fn do_typhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    Ok(table.state.element(STATE_TYPE))
}

/// `maphash_Ext(h, FUN)`: calls `FUN(key, value)` for every entry, newest first.
unsafe fn do_maphash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    let function = call.arg(1);
    if !values::is_function(function) {
        return Err(error("FUN must be a function"));
    }
    let entries: Vec<(Sexp, Sexp)> = (0..table.count())
        .rev()
        .map(|index| (table.key(index), table.value(index)))
        .collect();
    for (key, value) in entries {
        let args = [
            Arg {
                tag: nil(),
                value: key,
            },
            Arg { tag: nil(), value },
        ];
        crate::eval::apply(nil(), function, &args, call.env)?;
    }
    Ok(nil())
}

unsafe fn do_clrhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    for index in 0..table.count() {
        values::set_elt(table.state.element(STATE_KEYS), index, nil());
        values::set_elt(table.state.element(STATE_VALUES), index, nil());
    }
    table.set_count(0);
    Ok(nil())
}

unsafe fn do_is_hashtab(call: &NativeCall<'_>) -> R<Sexp> {
    scalar_bool(table_of(call.arg(0)).is_some())
}

// unzip.

/// `unzip(zipfile, files, exdir, list, overwrite, junkpaths, setTimes)`: lists or extracts the
/// members of a zip archive (stored or deflated). Listing returns the names, sizes and
/// modification times; extracting returns 0 with the extracted paths as `extracted`.
unsafe fn do_unzip(call: &NativeCall<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "zipfile")?;
    let path = crate::connections::expand_path(&path);
    let wanted = call.arg(1);
    let wanted: Option<Vec<String>> =
        (wanted != nil()).then(|| values::strings(wanted).into_iter().flatten().collect());
    let directory = values::require_string(call.arg(2), "exdir")?;
    let list = values::as_logical_scalar(call.arg(3)) == 1;
    let overwrite = values::as_logical_scalar(call.arg(4)) == 1;
    let junk_paths = values::as_logical_scalar(call.arg(5)) == 1;
    let archive =
        std::fs::read(&path).map_err(|_| error(format!("zip file '{path}' cannot be opened")))?;
    let entries = zip_entries(&archive)
        .ok_or_else(|| error(format!("zip file '{path}' cannot be opened")))?;
    if list {
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        let sizes: Vec<f64> = entries.iter().map(|entry| entry.size as f64).collect();
        let dates: Vec<String> = entries.iter().map(|entry| entry.date.clone()).collect();
        return values::list_vector(&[
            string_vector(&names)?,
            values::real_vector(&sizes)?,
            string_vector(&dates)?,
        ]);
    }
    let mut extracted = Vec::new();
    for entry in &entries {
        if wanted
            .as_ref()
            .is_some_and(|wanted| !wanted.contains(&entry.name))
        {
            continue;
        }
        let relative = if junk_paths {
            entry.name.rsplit('/').next().unwrap_or(&entry.name)
        } else {
            &entry.name
        };
        if relative.is_empty() {
            continue;
        }
        let target =
            std::path::Path::new(&crate::connections::expand_path(&directory)).join(relative);
        if entry.name.ends_with('/') {
            std::fs::create_dir_all(&target).map_err(|failure| {
                error(format!(
                    "cannot create directory '{}': {failure}",
                    target.display()
                ))
            })?;
            continue;
        }
        if target.exists() && !overwrite {
            warning(format!("not overwriting file '{}'", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|failure| {
                error(format!(
                    "cannot create directory '{}': {failure}",
                    parent.display()
                ))
            })?;
        }
        let data = zip_member(&archive, entry)
            .ok_or_else(|| error("error 1 in extracting from zip file"))?;
        std::fs::write(&target, data).map_err(|failure| {
            error(format!(
                "cannot open file '{}': {failure}",
                target.display()
            ))
        })?;
        extracted.push(target.display().to_string());
    }
    if let Some(wanted) = &wanted
        && wanted
            .iter()
            .any(|name| !entries.iter().any(|entry| &entry.name == name))
    {
        warning("requested file not found in the zip file")?;
    }
    let result = scalar_integer(0)?;
    let paths = string_vector(&extracted)?;
    values::set_attr(result, values::install("extracted")?, paths)?;
    Ok(result)
}

struct ZipEntry {
    name: String,
    size: u64,
    compressed_size: u64,
    method: u16,
    local_header: usize,
    /// `%Y-%m-%d %H:%M`.
    date: String,
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The central directory of a zip archive.
fn zip_entries(archive: &[u8]) -> Option<Vec<ZipEntry>> {
    const END_SIGNATURE: u32 = 0x0605_4b50;
    const ENTRY_SIGNATURE: u32 = 0x0201_4b50;
    let end = (0..archive.len().saturating_sub(21))
        .rev()
        .find(|&at| read_u32(archive, at) == Some(END_SIGNATURE))?;
    let count = read_u16(archive, end + 10)? as usize;
    let mut at = read_u32(archive, end + 16)? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if read_u32(archive, at)? != ENTRY_SIGNATURE {
            return None;
        }
        let method = read_u16(archive, at + 10)?;
        let time = read_u16(archive, at + 12)?;
        let day = read_u16(archive, at + 14)?;
        let compressed_size = read_u32(archive, at + 20)? as u64;
        let size = read_u32(archive, at + 24)? as u64;
        let name_length = read_u16(archive, at + 28)? as usize;
        let extra_length = read_u16(archive, at + 30)? as usize;
        let comment_length = read_u16(archive, at + 32)? as usize;
        let local_header = read_u32(archive, at + 42)? as usize;
        let name =
            String::from_utf8_lossy(archive.get(at + 46..at + 46 + name_length)?).into_owned();
        let date = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            1980 + (day >> 9),
            (day >> 5) & 0xF,
            day & 0x1F,
            time >> 11,
            (time >> 5) & 0x3F
        );
        entries.push(ZipEntry {
            name,
            size,
            compressed_size,
            method,
            local_header,
            date,
        });
        at += 46 + name_length + extra_length + comment_length;
    }
    Some(entries)
}

/// The uncompressed bytes of one member.
fn zip_member(archive: &[u8], entry: &ZipEntry) -> Option<Vec<u8>> {
    use std::io::Read;
    let at = entry.local_header;
    if read_u32(archive, at)? != 0x0403_4b50 {
        return None;
    }
    let name_length = read_u16(archive, at + 26)? as usize;
    let extra_length = read_u16(archive, at + 28)? as usize;
    let start = at + 30 + name_length + extra_length;
    let data = archive.get(start..start + entry.compressed_size as usize)?;
    match entry.method {
        0 => Some(data.to_vec()),
        8 => {
            let mut output = Vec::with_capacity(entry.size as usize);
            flate2::read::DeflateDecoder::new(data)
                .read_to_end(&mut output)
                .ok()?;
            Some(output)
        }
        _ => None,
    }
}
