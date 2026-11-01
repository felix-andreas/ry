//! Reads the layout of a delimited text file: its separator, whether its first row is a header,
//! and each column's name and R vector type.
//!
//! The rules follow data.table's `fread`, so a file reads into the same column types R users
//! expect, and they live here so that the table engine's reader and the checker agree on every
//! file. The engine sniffs the whole file before it parses it, and the checker sniffs the head
//! of a project's data file to type a literal `fread("prices.csv")` call without running it.
//!
//! A field's type is the narrowest of these that reads it, tried in order:
//!
//! - missing: `NA`, or an empty field outside a character column;
//! - logical: one spelling family per column, `true`/`false`, `True`/`False` or `TRUE`/`FALSE`;
//! - integer: a whole number that fits R's 32-bit integer;
//! - double: a decimal or scientific number, `Inf`, `-Inf` or `NaN` in any case;
//! - character: anything else.
//!
//! A column takes the join of its fields' types (see [`Guess::join`]), and a column of nothing
//! but missing values is logical, as in R.

use std::borrow::Cow;

/// The separators a file may use, in the order a tie between equally consistent ones is broken.
pub const SEPARATORS: [u8; 5] = *b",\t|; ";

/// The one quote character. A quoted field may hold separators, line breaks, and quotes written
/// twice (`""`), as RFC 4180 has it.
pub const QUOTE: u8 = b'"';

/// How many records the head sample reads, and how many each jump point reads after it.
const HEAD_RECORDS: usize = 1000;
const JUMP_RECORDS: usize = 100;
const JUMP_POINTS: usize = 100;

/// How many records decide the separator. A separator that splits these consistently splits the
/// rest the same way in any file a person would call delimited.
const SEPARATOR_RECORDS: usize = 100;

/// The R vector type of a column. It matches the table engine's column types other than
/// `factor`, which a text file cannot declare.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ColumnType {
    Logical,
    Integer,
    Double,
    Character,
}

impl ColumnType {
    /// The name the checker prints, as in `frame{id: integer[]}`.
    pub fn name(self) -> &'static str {
        match self {
            ColumnType::Logical => "logical[]",
            ColumnType::Integer => "integer[]",
            ColumnType::Double => "double[]",
            ColumnType::Character => "character[]",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Column {
    pub name: String,
    pub kind: ColumnType,
}

/// A file's layout: everything a reader needs besides the bytes.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Layout {
    pub separator: u8,
    /// Whether the first record names the columns rather than holding data.
    pub header: bool,
    /// The columns, with unique names: a missing header name becomes `V` and the column's
    /// position, and a repeated one gets a `.1`, `.2` suffix as R's `make.unique` gives it.
    pub columns: Vec<Column>,
}

/// Sniffs a whole file. Besides its head it samples records at evenly spaced points through the
/// file, so a column whose type only widens deep into a large file usually reads right the first
/// time. A reader still has to handle a value the sample missed.
pub fn sniff(text: &[u8]) -> Layout {
    sniff_records(text, true)
}

/// Sniffs the first bytes of a file, such as the few kilobytes the checker reads. A last line
/// without its line break may be cut short, so it is ignored.
pub fn sniff_head(text: &[u8]) -> Layout {
    let complete = match text.iter().rposition(|byte| *byte == b'\n') {
        Some(end) if end + 1 < text.len() => &text[..=end],
        _ => text,
    };
    sniff_records(complete, false)
}

/// The type a column of fields reads as so far. Joining moves only up, and a column's final type
/// is [`Guess::column_type`] of the join of all its fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Guess {
    Missing,
    Logical(Spelling),
    Integer,
    Double,
    Text,
}

/// The spelling family of a logical column. R's `fread` refuses to mix families, so `TRUE` above
/// `false` reads as text.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Spelling {
    Lower,
    Title,
    Upper,
}

impl Guess {
    /// The type of one field, given its text with the quotes already removed. An unquoted field's
    /// surrounding spaces do not count, so ` NA` is missing and ` 3` is an integer.
    pub fn of(field: &str) -> Guess {
        let field = field.trim_matches([' ', '\t']);
        if is_missing(field) {
            return Guess::Missing;
        }
        if let Some(spelling) = logical_spelling(field) {
            return Guess::Logical(spelling);
        }
        if parse_integer(field).is_some() {
            return Guess::Integer;
        }
        if parse_double(field).is_some() {
            return Guess::Double;
        }
        Guess::Text
    }

    /// The narrowest type that reads both. An integer widens to a double, but a logical and a
    /// number meet only at text, because `fread` reads neither `1` as `TRUE` nor `TRUE` as `1`.
    pub fn join(self, other: Guess) -> Guess {
        match (self, other) {
            (Guess::Missing, guess) | (guess, Guess::Missing) => guess,
            (Guess::Logical(left), Guess::Logical(right)) if left == right => self,
            (Guess::Integer, Guess::Integer) => Guess::Integer,
            (Guess::Integer | Guess::Double, Guess::Integer | Guess::Double) => Guess::Double,
            _ => Guess::Text,
        }
    }

    pub fn column_type(self) -> ColumnType {
        match self {
            Guess::Missing | Guess::Logical(_) => ColumnType::Logical,
            Guess::Integer => ColumnType::Integer,
            Guess::Double => ColumnType::Double,
            Guess::Text => ColumnType::Character,
        }
    }
}

/// Whether a field is `NA`. An empty field is missing too, except in a character column, where it
/// is the empty string; the caller knows which column it reads.
pub fn is_missing(field: &str) -> bool {
    field.is_empty() || field == "NA"
}

pub fn parse_logical(field: &str) -> Option<bool> {
    match field {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// A whole number that R's integer type holds. `i32::MIN` is R's `NA_integer_`, so it does not
/// fit, and a larger number reads as a double.
pub fn parse_integer(field: &str) -> Option<i32> {
    let digits = field.strip_prefix(['+', '-']).unwrap_or(field);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if digits.trim_start_matches('0').len() > 10 {
        return None;
    }
    let value: i64 = digits.parse().ok()?;
    let value = if field.starts_with('-') {
        -value
    } else {
        value
    };
    i32::try_from(value).ok().filter(|value| *value != i32::MIN)
}

/// A decimal or scientific number (`1.5`, `.5`, `5.`, `1e3`), or `Inf`, `Infinity` or `NaN` in
/// any case and with an optional sign. Rust's float parser would also take forms such as
/// `infinite` or a bare `e5`, so the shape is checked first.
pub fn parse_double(field: &str) -> Option<f64> {
    let unsigned = field.strip_prefix(['+', '-']).unwrap_or(field);
    let word = unsigned.to_ascii_lowercase();
    if matches!(word.as_str(), "inf" | "infinity" | "nan") {
        return field.parse().ok();
    }
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
        None => (unsigned, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    if whole.len() + fraction.len() == 0 || !digits(whole) || !digits(fraction) {
        return None;
    }
    if let Some(exponent) = exponent {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if exponent.is_empty() || !digits(exponent) {
            return None;
        }
    }
    field.parse().ok()
}

fn logical_spelling(field: &str) -> Option<Spelling> {
    match field {
        "true" | "false" => Some(Spelling::Lower),
        "True" | "False" => Some(Spelling::Title),
        "TRUE" | "FALSE" => Some(Spelling::Upper),
        _ => None,
    }
}

fn sniff_records(text: &[u8], whole: bool) -> Layout {
    let text = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
    let (separator, width) = detect_separator(text);
    let mut records = Records::new(text, 0, separator);
    let mut fields = Vec::new();
    let mut first: Vec<String> = Vec::new();
    if records.next_record(&mut fields) {
        first = fields.iter().map(|field| field.to_string()).collect();
    }
    let mut guesses = vec![Guess::Missing; width];
    let mut data_rows = 0usize;
    let sample = |fields: &[Cow<str>], guesses: &mut [Guess]| {
        for (guess, field) in guesses.iter_mut().zip(fields) {
            *guess = guess.join(Guess::of(field));
        }
    };
    for _ in 1..HEAD_RECORDS {
        if !records.next_record(&mut fields) {
            break;
        }
        sample(&fields, &mut guesses);
        data_rows += 1;
    }
    let head_end = records.position;
    if whole && head_end < text.len() {
        for point in 1..=JUMP_POINTS {
            let jump = head_end + (text.len() - head_end) * point / (JUMP_POINTS + 1);
            let Some(start) = record_start(text, jump, separator, width) else {
                continue;
            };
            let mut records = Records::new(text, start, separator);
            for _ in 0..JUMP_RECORDS {
                if !records.next_record(&mut fields) {
                    break;
                }
                sample(&fields, &mut guesses);
                data_rows += 1;
            }
        }
    }
    let first_guesses: Vec<Guess> = first.iter().map(|field| Guess::of(field)).collect();
    let header = if data_rows == 0 {
        !first_guesses.is_empty() && first_guesses.iter().all(|guess| *guess == Guess::Text)
    } else {
        guesses.iter().all(|guess| *guess == Guess::Text)
            || first_guesses
                .iter()
                .zip(&guesses)
                .any(|(field, column)| *field == Guess::Text && *column != Guess::Text)
    };
    if !header {
        for (guess, field) in guesses.iter_mut().zip(&first_guesses) {
            *guess = guess.join(*field);
        }
    }
    let names = (0..width).map(|position| {
        let name = if header {
            first.get(position).map(|name| name.trim()).unwrap_or("")
        } else {
            ""
        };
        if name.is_empty() {
            format!("V{}", position + 1)
        } else {
            name.to_string()
        }
    });
    let columns = unique_names(names.collect())
        .into_iter()
        .zip(guesses)
        .map(|(name, guess)| Column {
            name,
            kind: guess.column_type(),
        })
        .collect();
    Layout {
        separator,
        header,
        columns,
    }
}

/// The separator that splits the head's records into the same number of fields most often, with
/// more fields breaking a tie. A file with no such separator is one column, read with a
/// separator its sample never contains.
fn detect_separator(text: &[u8]) -> (u8, usize) {
    let mut best: Option<(u8, usize, usize)> = None;
    let mut fields = Vec::new();
    for separator in SEPARATORS {
        let mut counts: Vec<usize> = Vec::new();
        let mut records = Records::new(text, 0, separator);
        while counts.len() < SEPARATOR_RECORDS && records.next_record(&mut fields) {
            counts.push(fields.len());
        }
        let Some((width, frequency)) = mode(&counts) else {
            continue;
        };
        if width < 2 {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, best_width, best_frequency)) => {
                (frequency, width) > (best_frequency, best_width)
            }
        };
        if better {
            best = Some((separator, width, frequency));
        }
    }
    if let Some((separator, width, _)) = best {
        return (separator, width);
    }
    let separator = SEPARATORS
        .into_iter()
        .find(|separator| !text.contains(separator))
        .unwrap_or(b'\x1f');
    (separator, 1)
}

/// The most frequent value and its count, preferring the larger value on a tie.
fn mode(counts: &[usize]) -> Option<(usize, usize)> {
    let mut sorted = counts.to_vec();
    sorted.sort_unstable();
    let mut best: Option<(usize, usize)> = None;
    for run in sorted.chunk_by(|left, right| left == right) {
        let candidate = (run[0], run.len());
        if best.is_none_or(|(_, frequency)| candidate.1 >= frequency) {
            best = Some(candidate);
        }
    }
    best
}

/// The start of a record at or after `position`. A line break may sit inside a quoted field, so
/// a candidate counts only if the next few records it starts all have the file's width.
fn record_start(text: &[u8], position: usize, separator: u8, width: usize) -> Option<usize> {
    const ATTEMPTS: usize = 5;
    const CONFIRMATIONS: usize = 5;
    let mut candidate = position;
    let mut fields = Vec::new();
    for _ in 0..ATTEMPTS {
        let newline = text
            .get(candidate..)?
            .iter()
            .position(|byte| *byte == b'\n')?;
        candidate += newline + 1;
        let mut records = Records::new(text, candidate, separator);
        let mut confirmed = 0;
        while confirmed < CONFIRMATIONS && records.next_record(&mut fields) {
            if fields.len() != width {
                break;
            }
            confirmed += 1;
        }
        if confirmed == CONFIRMATIONS || (confirmed > 0 && records.position >= text.len()) {
            return Some(candidate);
        }
    }
    None
}

/// Make the names unique as R's `make.unique` does: the second `a` becomes `a.1`, skipping any
/// suffix another column already uses.
fn unique_names(names: Vec<String>) -> Vec<String> {
    let mut taken: std::collections::HashSet<String> = names.iter().cloned().collect();
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::with_capacity(names.len());
    for name in names {
        if seen.insert(name.clone()) {
            unique.push(name);
            continue;
        }
        let mut suffix = 1;
        let renamed = loop {
            let candidate = format!("{name}.{suffix}");
            if !taken.contains(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        taken.insert(renamed.clone());
        seen.insert(renamed.clone());
        unique.push(renamed);
    }
    unique
}

/// Splits text into records of fields. Blank lines are skipped, a `\r` before a line break is
/// dropped, and quotes are removed from quoted fields.
struct Records<'a> {
    text: &'a [u8],
    position: usize,
    separator: u8,
}

impl<'a> Records<'a> {
    fn new(text: &'a [u8], position: usize, separator: u8) -> Records<'a> {
        Records {
            text,
            position,
            separator,
        }
    }

    /// Reads the next non-blank record into `fields`, or returns false at the end of the text.
    fn next_record(&mut self, fields: &mut Vec<Cow<'a, str>>) -> bool {
        fields.clear();
        loop {
            let rest = self.text.get(self.position..).unwrap_or_default();
            if rest.is_empty() {
                return false;
            }
            let line_end = rest.iter().position(|byte| *byte == b'\n');
            let line = &rest[..line_end.unwrap_or(rest.len())];
            if line.iter().all(|byte| matches!(byte, b' ' | b'\t' | b'\r')) {
                self.position += line_end.map_or(rest.len(), |end| end + 1);
                continue;
            }
            break;
        }
        loop {
            let (field, ended) = self.next_field();
            fields.push(field);
            if ended {
                return true;
            }
        }
    }

    /// Reads one field and reports whether it ended its record.
    fn next_field(&mut self) -> (Cow<'a, str>, bool) {
        let text = self.text;
        let start = self.position;
        if text.get(start) == Some(&QUOTE) {
            let mut content = Vec::new();
            let mut position = start + 1;
            let mut escaped = false;
            while let Some(byte) = text.get(position) {
                if *byte == QUOTE {
                    if text.get(position + 1) == Some(&QUOTE) {
                        escaped = true;
                        content.push(QUOTE);
                        position += 2;
                        continue;
                    }
                    position += 1;
                    break;
                }
                content.push(*byte);
                position += 1;
            }
            // Anything between the closing quote and the separator is malformed; it is dropped
            // rather than glued to the field.
            while let Some(byte) = text.get(position) {
                if *byte == self.separator || *byte == b'\n' {
                    break;
                }
                position += 1;
            }
            self.position = position;
            let ended = self.finish_field();
            let field = if escaped {
                Cow::Owned(String::from_utf8_lossy(&content).into_owned())
            } else {
                String::from_utf8_lossy(&text[start + 1..start + 1 + content.len()])
            };
            return (field, ended);
        }
        let mut position = start;
        while let Some(byte) = text.get(position) {
            if *byte == self.separator || *byte == b'\n' {
                break;
            }
            position += 1;
        }
        let mut raw = &text[start..position];
        if let Some(stripped) = raw.strip_suffix(b"\r") {
            raw = stripped;
        }
        self.position = position;
        let ended = self.finish_field();
        (String::from_utf8_lossy(raw), ended)
    }

    /// Steps past the separator or line break after a field, reporting whether the record ended.
    fn finish_field(&mut self) -> bool {
        match self.text.get(self.position) {
            Some(byte) if *byte == self.separator => {
                self.position += 1;
                false
            }
            Some(_) => {
                self.position += 1;
                true
            }
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(text: &str) -> (char, bool, Vec<(String, &'static str)>) {
        let layout = sniff(text.as_bytes());
        (
            layout.separator as char,
            layout.header,
            layout
                .columns
                .into_iter()
                .map(|column| (column.name, column.kind.name()))
                .collect(),
        )
    }

    fn columns(text: &str) -> Vec<(String, &'static str)> {
        layout(text).2
    }

    fn named(pairs: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        pairs
            .iter()
            .map(|(name, kind)| (name.to_string(), *kind))
            .collect()
    }

    #[test]
    fn fields_take_the_narrowest_type_that_reads_them() {
        assert_eq!(
            columns("a,b,c,d,e\n1,1.5,TRUE,x,NA\n2,3,FALSE,y,\n"),
            named(&[
                ("a", "integer[]"),
                ("b", "double[]"),
                ("c", "logical[]"),
                ("d", "character[]"),
                ("e", "logical[]"),
            ])
        );
    }

    #[test]
    fn numbers_read_as_fread_reads_them() {
        for (field, expected) in [
            ("01", Guess::Integer),
            ("+5", Guess::Integer),
            (" 3 ", Guess::Integer),
            ("2147483647", Guess::Integer),
            ("-2147483648", Guess::Double),
            ("3000000000", Guess::Double),
            ("1e3", Guess::Double),
            (".5", Guess::Double),
            ("5.", Guess::Double),
            ("Inf", Guess::Double),
            ("-inf", Guess::Double),
            ("NaN", Guess::Double),
            ("e5", Guess::Text),
            ("1e", Guess::Text),
            ("0x1A", Guess::Text),
            ("infinite", Guess::Text),
            (" NA", Guess::Missing),
            ("T", Guess::Text),
        ] {
            assert_eq!(Guess::of(field), expected, "{field:?}");
        }
    }

    #[test]
    fn logical_spellings_do_not_mix_and_do_not_meet_numbers() {
        assert_eq!(
            columns("a,b,c\nTRUE,true,TRUE\nFALSE,False,1\n"),
            named(&[
                ("a", "logical[]"),
                ("b", "character[]"),
                ("c", "character[]"),
            ])
        );
    }

    #[test]
    fn the_first_record_is_a_header_if_it_holds_text_above_a_typed_column() {
        assert_eq!(
            layout("1,2\n3,4\n"),
            (
                ',',
                false,
                named(&[("V1", "integer[]"), ("V2", "integer[]")])
            )
        );
        assert_eq!(
            layout("x,y\na,b\n"),
            (
                ',',
                true,
                named(&[("x", "character[]"), ("y", "character[]")])
            )
        );
        assert_eq!(
            layout("1,y\n1.5,2\n"),
            (',', true, named(&[("1", "double[]"), ("y", "integer[]")]))
        );
        assert_eq!(
            layout("a,b\n"),
            (',', true, named(&[("a", "logical[]"), ("b", "logical[]")]))
        );
    }

    #[test]
    fn data_in_the_first_record_widens_its_columns() {
        assert_eq!(
            layout("1.5,x\n2,y\n"),
            (
                ',',
                false,
                named(&[("V1", "double[]"), ("V2", "character[]")])
            )
        );
    }

    #[test]
    fn missing_and_repeated_names_are_made_unique() {
        assert_eq!(
            columns("a,,a,a.1,a\n1,2,3,4,5\n")
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["a", "V2", "a.2", "a.1", "a.3"]
        );
    }

    #[test]
    fn the_separator_is_the_one_that_splits_records_consistently() {
        assert_eq!(layout("a\tb\n1\t2,5\n").0, '\t');
        assert_eq!(layout("a|b|c\n1|2|3\n").0, '|');
        assert_eq!(layout("a;b\n1;2\n").0, ';');
        assert_eq!(layout("a b\n1 2\n").0, ' ');
        assert_eq!(layout("a,b\n\"1,5\",2\n").0, ',');
    }

    #[test]
    fn a_file_without_a_separator_is_one_column() {
        let layout = sniff(b"price\n1.5\n2\n");
        assert_eq!(layout.columns.len(), 1);
        assert_eq!(layout.columns[0].name, "price");
        assert_eq!(layout.columns[0].kind, ColumnType::Double);
        assert!(!b"price\n1.5\n2\n".contains(&layout.separator));
    }

    #[test]
    fn quoted_fields_hold_separators_line_breaks_and_quotes() {
        assert_eq!(
            columns("id,note\n1,\"a, b\"\n2,\"line\nbreak\"\n3,\"say \"\"hi\"\"\"\n\"4\",x\n"),
            named(&[("id", "integer[]"), ("note", "character[]")])
        );
        let mut fields = Vec::new();
        let mut records = Records::new(b"\"say \"\"hi\"\"\",\"\"\r\n", 0, b',');
        assert!(records.next_record(&mut fields));
        assert_eq!(fields, ["say \"hi\"", ""]);
    }

    #[test]
    fn blank_lines_carriage_returns_and_a_byte_order_mark_are_ignored() {
        assert_eq!(
            columns("\u{feff}a,b\r\n\r\n1,x\r\n2,y\r\n"),
            named(&[("a", "integer[]"), ("b", "character[]")])
        );
    }

    #[test]
    fn jump_points_find_a_type_that_widens_deep_in_the_file() {
        let mut text = String::from("id,value,note\n");
        for row in 0..40_000 {
            let value = if (30_000..30_500).contains(&row) {
                format!("{row}.5")
            } else {
                row.to_string()
            };
            let note = if row % 1000 == 999 {
                "\"spans\na line\""
            } else {
                "x"
            };
            text.push_str(&format!("{row},{value},{note}\n"));
        }
        assert_eq!(
            columns(&text),
            named(&[
                ("id", "integer[]"),
                ("value", "double[]"),
                ("note", "character[]")
            ])
        );
        assert_eq!(
            sniff_head(&text.as_bytes()[..4096]).columns[1].kind,
            ColumnType::Integer
        );
    }

    #[test]
    fn the_head_ignores_a_line_cut_short() {
        assert_eq!(
            sniff_head(b"a,b\n1,2\n3,x").columns[1].kind,
            ColumnType::Integer
        );
    }
}
