//! Character vectors: `paste`, `nchar`, `substr`, regular expressions, `sprintf` and friends.
//!
//! Text is processed as UTF-8. Positions that R reports, such as `regexpr`'s, count characters
//! unless `useBytes = TRUE` asks for bytes.
//!
//! Regular expressions run on `fancy-regex`, which adds backreferences and lookaround to the
//! `regex` crate's engine. R's two dialects, POSIX extended expressions as TRE reads them (the
//! default) and PCRE (`perl = TRUE`), are translated into its syntax first ([`translate_regex`]),
//! because the dialects disagree on bracket expressions, on what `\w` and `[[:alpha:]]` match,
//! and on which characters are special.

use {
    crate::{
        builtins::{BuiltinDef, Call, builtin, internal, require},
        coerce::coerce_vector,
        conditions::warning,
        eval,
        format::{display_width, scientific_text},
        unwind::{R, error},
        values::{
            self, alloc, get_attr, length, na_string, nil, scalar_integer, scalar_string, set_attr,
            set_elt, string_at, string_vector, string_vector_na, strings, symbols,
        },
    },
    fancy_regex::{Regex, RegexBuilder},
    heap::{
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp, is_na_real},
        strings::Encoding,
    },
    memchr::memmem,
    regex_automata::{Anchored, Input, MatchKind, meta, util::syntax},
    rustc_hash::{FxHashMap, FxHashSet},
    std::{borrow::Cow, cell::RefCell, rc::Rc},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("paste", 4, do_paste),
    internal("paste0", 3, do_paste0),
    internal("nchar", 4, do_nchar),
    builtin("nzchar", -1, do_nzchar),
    internal("substr", 3, do_substr),
    internal("substr<-", 4, do_set_substr),
    internal("strsplit", 5, do_strsplit),
    internal("sprintf", -1, do_sprintf),
    internal("toupper", 1, |call| unsafe { change_case(call, true) }),
    internal("tolower", 1, |call| unsafe { change_case(call, false) }),
    internal("chartr", 3, do_chartr),
    internal("strtrim", 2, do_strtrim),
    internal("startsWith", 2, |call| unsafe {
        starts_or_ends_with(call, true)
    }),
    internal("endsWith", 2, |call| unsafe {
        starts_or_ends_with(call, false)
    }),
    internal("strrep", 2, do_strrep),
    internal("strtoi", 2, do_strtoi),
    internal("make.names", 2, do_make_names),
    internal("make.unique", 2, do_make_unique),
    internal("abbreviate", 3, do_abbreviate),
    internal("utf8ToInt", 1, do_utf8_to_int),
    internal("intToUtf8", 3, do_int_to_utf8),
    internal("charToRaw", 1, do_char_to_raw),
    internal("rawToChar", 2, do_raw_to_char),
    internal("pmatch", 4, do_pmatch),
    internal("charmatch", 3, do_charmatch),
    internal("grepl", 8, |call| unsafe { grep(call, true) }),
    internal("grep", 8, |call| unsafe { grep(call, false) }),
    internal("sub", 7, |call| unsafe { substitute(call, false) }),
    internal("gsub", 7, |call| unsafe { substitute(call, true) }),
    internal("regexpr", 6, |call| unsafe { regexpr(call, false) }),
    internal("grepRaw", 8, do_grep_raw),
    internal("gregexpr", 6, |call| unsafe { regexpr(call, true) }),
    internal("regexec", 5, do_regexec),
];

/// Translates an R regular expression into the syntax of the `regex` crate, which
/// `fancy-regex` extends. `extended` selects POSIX extended syntax as TRE reads it (R's
/// default); otherwise the pattern is PCRE, as `perl = TRUE` reads it.
///
/// The translation never fails: a construct with no equivalent is passed through, so that
/// compiling the result reports it.
pub fn translate_regex(pattern: &str, extended: bool) -> String {
    let syntax = if extended {
        Syntax::Extended
    } else {
        Syntax::Perl
    };
    translate(pattern, syntax).pattern
}

unsafe fn do_paste(call: &Call<'_>) -> R<Sexp> {
    let separator = call.arg(1);
    if separator.kind() != SexpType::String
        || length(separator) == 0
        || separator.element(0) == na_string()
    {
        return Err(error("invalid separator"));
    }
    paste(call, Some(separator.element(0)), call.arg(2), call.arg(3))
}

unsafe fn do_paste0(call: &Call<'_>) -> R<Sexp> {
    paste(call, None, call.arg(1), call.arg(2))
}

/// `paste` joins bytes, not text, because a native string may hold bytes that are not UTF-8.
/// As in GNU R, the result is marked "bytes" when any piece is, UTF-8 when any piece is UTF-8
/// or latin1 (which is translated), and native otherwise.
unsafe fn paste(
    call: &Call<'_>,
    separator: Option<Sexp>,
    collapse: Sexp,
    recycle_zero: Sexp,
) -> R<Sexp> {
    let list = call.arg(0);
    if list.kind() != SexpType::List {
        return Err(error("invalid first argument"));
    }
    let collapse = if collapse == nil() {
        None
    } else if collapse.kind() == SexpType::String
        && length(collapse) > 0
        && collapse.element(0) != na_string()
    {
        Some(collapse.element(0))
    } else {
        return Err(error("invalid 'collapse' argument"));
    };
    let recycle_zero = flag(recycle_zero);
    let mut columns = Vec::with_capacity(length(list));
    for index in 0..length(list) {
        columns.push(as_character(list.element(index), call.env)?);
    }
    let lengths: Vec<usize> = columns.iter().map(|&column| length(column)).collect();
    let mut count = lengths.iter().copied().max().unwrap_or(0);
    if recycle_zero && lengths.contains(&0) {
        count = 0;
    }
    // As in GNU R, each result decides its encoding from all of its pieces before joining any,
    // so a bytes piece keeps every other piece untranslated.
    let encoding_of = |charsxps: &[Sexp]| {
        let mut encoding = Encoding::Native;
        for &charsxp in charsxps {
            if charsxp == na_string() {
                continue;
            }
            if charsxp.has_gp(gp::BYTES) {
                return Encoding::Bytes;
            }
            if charsxp.has_gp(gp::LATIN1) || charsxp.has_gp(gp::UTF8) {
                encoding = Encoding::Utf8;
            }
        }
        encoding
    };
    let join = |charsxps: &[Sexp], encoding: Encoding| {
        let mut bytes = Vec::new();
        for &charsxp in charsxps {
            if charsxp == na_string() {
                bytes.extend_from_slice(b"NA");
            } else if charsxp.has_gp(gp::LATIN1) && encoding != Encoding::Bytes {
                bytes.extend_from_slice(values::char_str(charsxp).as_bytes());
            } else {
                bytes.extend_from_slice(charsxp.bytes());
            }
        }
        bytes
    };
    let result = alloc(SexpType::String, count)?;
    let mut parts = Vec::new();
    for row in 0..count {
        parts.clear();
        for (position, (&column, &column_length)) in columns.iter().zip(&lengths).enumerate() {
            if position > 0
                && let Some(separator) = separator
            {
                parts.push(separator);
            }
            if column_length > 0 {
                parts.push(column.element(row % column_length));
            }
        }
        let encoding = encoding_of(&parts);
        set_elt(
            result,
            row,
            values::mkchar_bytes(&join(&parts, encoding), encoding)?,
        );
    }
    let Some(collapse) = collapse else {
        return Ok(result);
    };
    parts.clear();
    for row in 0..count {
        if row > 0 {
            parts.push(collapse);
        }
        parts.push(result.element(row));
    }
    let encoding = encoding_of(&parts);
    let collapsed = alloc(SexpType::String, 1)?;
    set_elt(
        collapsed,
        0,
        values::mkchar_bytes(&join(&parts, encoding), encoding)?,
    );
    Ok(collapsed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CountType {
    Bytes,
    Chars,
    Width,
}

unsafe fn do_nchar(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if values::inherits(x, "factor") {
        return Err(error("'nchar()' requires a character vector"));
    }
    let count_type = match values::first_string(call.arg(1)).as_deref() {
        Some(name) if !name.is_empty() && "bytes".starts_with(name) => CountType::Bytes,
        Some(name) if !name.is_empty() && "chars".starts_with(name) => CountType::Chars,
        Some(name) if !name.is_empty() && "width".starts_with(name) => CountType::Width,
        _ => return Err(error("invalid 'type' argument")),
    };
    let keep_na = values::as_logical_scalar(call.arg(3));
    let na_count = match (keep_na, count_type) {
        (1, _) | (NA_LOGICAL, CountType::Bytes | CountType::Chars) => NA_INTEGER,
        _ => 2,
    };
    let allow_na = values::as_logical_scalar(call.arg(2)) == 1;
    let text = character_vector(x)?;
    let count = length(text);
    let result = alloc(SexpType::Integer, count)?;
    let counts = values::ints_mut(result);
    for (index, slot) in counts.iter_mut().enumerate() {
        let charsxp = text.element(index);
        let invalid = count_type != CountType::Bytes
            && !charsxp.has_gp(gp::LATIN1)
            && std::str::from_utf8(charsxp.bytes()).is_err();
        *slot = if charsxp == na_string() {
            na_count
        } else if invalid {
            if !allow_na {
                return Err(error(format!(
                    "invalid multibyte string, element {}",
                    index + 1
                )));
            }
            NA_INTEGER
        } else {
            match count_type {
                CountType::Bytes => charsxp.bytes().len() as i32,
                CountType::Chars => values::char_str(charsxp).chars().count() as i32,
                CountType::Width => display_width(&values::char_str(charsxp)) as i32,
            }
        };
    }
    copy_shape(x, result)?;
    Ok(result)
}

unsafe fn do_nzchar(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "keepNA"])?;
    let x = require(matched[0], "x")?;
    let keep_na = matched[1].is_some_and(|value| flag(value));
    if values::inherits(x, "factor") {
        return Err(error("'nzchar()' requires a character vector"));
    }
    let text = character_vector(x)?;
    let count = length(text);
    let result = alloc(SexpType::Logical, count)?;
    for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
        let charsxp = text.element(index);
        *slot = if charsxp == na_string() {
            if keep_na { NA_LOGICAL } else { 1 }
        } else {
            (!charsxp.bytes().is_empty()) as i32
        };
    }
    Ok(result)
}

unsafe fn do_substr(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("extracting substrings from a non-character object"));
    }
    let count = length(x);
    let result = alloc(SexpType::String, count)?;
    if count > 0 {
        let starts = integer_argument(call.arg(1))?;
        // From R 4.5 on, `substr(x, start, NULL)` and `substring(x, first)` pass a NULL stop,
        // which means the end of each string.
        let stops = if call.arg(2) == nil() {
            vec![i32::MAX]
        } else {
            integer_argument(call.arg(2))?
        };
        if starts.is_empty() || stops.is_empty() {
            return Err(error("invalid substring arguments"));
        }
        for index in 0..count {
            let start = starts[index % starts.len()];
            let stop = stops[index % stops.len()];
            let charsxp = x.element(index);
            if charsxp == na_string() || start == NA_INTEGER || stop == NA_INTEGER {
                set_elt(result, index, na_string());
                continue;
            }
            if Encoding::of(charsxp) == Encoding::Bytes {
                let bytes = charsxp.bytes();
                let first = (start.max(1) as usize - 1).min(bytes.len());
                let last = (stop.max(0) as usize).min(bytes.len()).max(first);
                set_elt(
                    result,
                    index,
                    values::mkchar_bytes(&bytes[first..last], Encoding::Bytes)?,
                );
                continue;
            }
            let text = values::char_str(charsxp);
            let (first, last) = char_span(&text, start, stop);
            if first == 0 && last == text.len() {
                set_elt(result, index, charsxp);
            } else {
                set_elt(result, index, values::mkchar(&text[first..last])?);
            }
        }
    }
    values::copy_attributes(x, result)?;
    Ok(result)
}

unsafe fn do_set_substr(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let value = call.arg(3);
    if x.kind() != SexpType::String {
        return Err(error("replacing substrings in a non-character object"));
    }
    let count = length(x);
    let result = values::shallow_duplicate(x)?;
    if count == 0 {
        return Ok(result);
    }
    let starts = integer_argument(call.arg(1))?;
    let stops = integer_argument(call.arg(2))?;
    if starts.is_empty() || stops.is_empty() {
        return Err(error("invalid substring arguments"));
    }
    if value.kind() != SexpType::String || length(value) == 0 {
        return Err(error("invalid value"));
    }
    let value_count = length(value);
    for index in 0..count {
        let start = starts[index % starts.len()];
        let stop = stops[index % stops.len()];
        let charsxp = x.element(index);
        let replacement = value.element(index % value_count);
        if charsxp == na_string()
            || replacement == na_string()
            || start == NA_INTEGER
            || stop == NA_INTEGER
        {
            set_elt(result, index, na_string());
            continue;
        }
        let text = values::char_str(charsxp);
        let replacement = values::char_str(replacement);
        let length_in_chars = text.chars().count() as i64;
        let first = i64::from(start).max(1);
        let last = i64::from(stop)
            .min(length_in_chars)
            .min(first + replacement.chars().count() as i64 - 1);
        if first > last {
            continue;
        }
        let mut edited = String::with_capacity(text.len());
        let mut replacement_chars = replacement.chars();
        for (position, character) in (1..).zip(text.chars()) {
            if (first..=last).contains(&position) {
                edited.extend(replacement_chars.next());
            } else {
                edited.push(character);
            }
        }
        set_elt(result, index, values::mkchar(&edited)?);
    }
    Ok(result)
}

/// The byte range of characters `start..=stop` (1-based, clamped to the text).
fn char_span(text: &str, start: i32, stop: i32) -> (usize, usize) {
    let first = start.max(1) as usize;
    if stop < start || stop < 1 {
        return (0, 0);
    }
    let last = stop as usize;
    let mut begin = text.len();
    let mut end = text.len();
    for (position, (offset, _)) in (1..).zip(text.char_indices()) {
        if position == first {
            begin = offset;
        }
        if position == last + 1 {
            end = offset;
            break;
        }
    }
    if begin > end { (0, 0) } else { (begin, end) }
}

unsafe fn do_strsplit(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let split = call.arg(1);
    if x.kind() != SexpType::String {
        return Err(error("non-character argument"));
    }
    let options = pattern_options(false, flag(call.arg(3)), flag(call.arg(2)))?;
    let forced_bytes = has_bytes(&[x, split]);
    let use_bytes = flag(call.arg(4)) || forced_bytes;
    let splits: Vec<Option<String>> = if split.kind() == SexpType::String {
        (0..length(split))
            .map(|index| subject_at(split, index, use_bytes).map(Cow::into_owned))
            .collect()
    } else {
        Vec::new()
    };
    let mut matchers: Vec<Option<Matcher>> = (0..splits.len()).map(|_| None).collect();
    let count = length(x);
    let result = alloc(SexpType::List, count)?;
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    for index in 0..count {
        let Some(text) = subject_at(x, index, use_bytes) else {
            set_elt(result, index, values::scalar_string_charsxp(na_string())?);
            continue;
        };
        let position = index % splits.len().max(1);
        pieces.clear();
        match splits.get(position) {
            None => split_chars(&text, &mut pieces),
            Some(None) => pieces.push((0, text.len())),
            Some(Some(separator)) if separator.is_empty() => split_chars(&text, &mut pieces),
            Some(Some(separator)) => {
                if matchers[position].is_none() {
                    matchers[position] =
                        Some(Matcher::compile(separator, options, CompileErrors::Quoted)?);
                }
                match &matchers[position] {
                    Some(matcher) => split_text(matcher, &text, &mut pieces)?,
                    None => pieces.push((0, text.len())),
                }
            }
        }
        // Each piece vector is stored in `result` before its strings are made, so neither needs
        // a handle; rooting them would grow the handle stack with the input, and every
        // collection scans that stack.
        let heap = crate::heap();
        let vector = heap.alloc_vector(SexpType::String, pieces.len() as isize)?;
        heap.set_element(result, index, vector);
        for (position, &(start, end)) in pieces.iter().enumerate() {
            let charsxp = if forced_bytes {
                regex_result(&text[start..end], true)?
            } else {
                heap.mk_char(&text.as_bytes()[start..end], Encoding::Utf8)?
            };
            heap.set_element(vector, position, charsxp);
        }
    }
    let names = get_attr(x, symbols().names);
    if names != nil() {
        set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// Pushes the byte range of each character of `text` onto `pieces`.
fn split_chars(text: &str, pieces: &mut Vec<(usize, usize)>) {
    pieces.extend(
        text.char_indices()
            .map(|(offset, character)| (offset, offset + character.len_utf8())),
    );
}

/// Splits as GNU R does: each search runs on what is left of the text, so `^` matches at every
/// piece, and an empty match at the start of the rest splits off one character. Pushes the byte
/// range of each piece onto `pieces`.
fn split_text(matcher: &Matcher, text: &str, pieces: &mut Vec<(usize, usize)>) -> R<()> {
    let mut offset = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        match matcher.find_at(rest, 0)? {
            Some((_, 0)) => {
                let next = next_boundary(rest, 0);
                pieces.push((offset, offset + next));
                offset += next;
            }
            Some((start, end)) => {
                pieces.push((offset, offset + start));
                offset += end;
            }
            None => {
                pieces.push((offset, text.len()));
                offset = text.len();
            }
        }
    }
    Ok(())
}

/// One `%` conversion of a format string.
struct Conversion {
    /// The conversion as written, for error messages.
    text: String,
    argument: usize,
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    alternate: bool,
    width: Option<Amount>,
    precision: Option<Amount>,
    kind: char,
}

/// A width or precision, written out or taken from an argument by `*`.
#[derive(Clone, Copy)]
enum Amount {
    Fixed(usize),
    Argument(usize),
}

enum FormatPiece {
    Text(String),
    Conversion(Conversion),
}

/// An argument of `sprintf` and the conversions it needs, computed once per call.
struct FormatArgument {
    value: Sexp,
    length: usize,
    /// `as.character(value)`, for `%s`.
    text: Option<Sexp>,
    /// Whether a double vector holds only whole numbers, which `%d` requires.
    whole: Option<bool>,
}

unsafe fn do_sprintf(call: &Call<'_>) -> R<Sexp> {
    let formats = call.arg(0);
    if formats.kind() != SexpType::String {
        return Err(error("'fmt' is not a character vector"));
    }
    let mut arguments = Vec::with_capacity(call.len().saturating_sub(1));
    for (position, arg) in call.args.iter().enumerate().skip(1) {
        let value = arg.value;
        if matches!(value.kind(), SexpType::Symbol | SexpType::Language) {
            return Err(error(format!(
                "invalid type of argument[{position}]: '{}'",
                value.kind().name()
            )));
        }
        arguments.push(FormatArgument {
            value,
            length: length(value),
            text: None,
            whole: None,
        });
    }
    let format_count = length(formats);
    if format_count == 0 || arguments.iter().any(|argument| argument.length == 0) {
        return alloc(SexpType::String, 0);
    }
    let kinds: Vec<SexpType> = arguments
        .iter()
        .map(|argument| argument.value.kind())
        .collect();
    let mut used = vec![false; arguments.len()];
    let mut parsed = Vec::with_capacity(format_count);
    for index in 0..format_count {
        // An `NA` format is the text "NA", as in GNU R.
        let format = string_at(formats, index).unwrap_or(Cow::Borrowed("NA"));
        parsed.push(parse_format(&format, &kinds, &mut used)?);
    }
    // `%s` converts a classed argument with its `as.character` method first, and the result's
    // length is what recycles, so a condition formats as one string.
    for piece in parsed.iter().flatten() {
        if let FormatPiece::Conversion(conversion) = piece
            && conversion.kind == 's'
        {
            let argument = &mut arguments[conversion.argument];
            if argument.text.is_none() && argument.value.is_object() {
                let text = as_character(argument.value, call.env)?;
                argument.text = Some(text);
                argument.length = length(text);
            }
        }
    }
    let lengths: Vec<usize> = std::iter::once(format_count)
        .chain(arguments.iter().map(|argument| argument.length))
        .collect();
    if lengths.contains(&0) {
        return alloc(SexpType::String, 0);
    }
    let count = lengths.iter().copied().max().unwrap_or(0);
    if lengths.iter().any(|&each| count % each != 0) {
        return Err(error("arguments cannot be recycled to the same length"));
    }
    let result = alloc(SexpType::String, count)?;
    for row in 0..count {
        let mut text = String::new();
        for piece in &parsed[row % format_count] {
            match piece {
                FormatPiece::Text(literal) => text.push_str(literal),
                FormatPiece::Conversion(conversion) => {
                    text.push_str(&format_conversion(
                        conversion,
                        &mut arguments,
                        row,
                        call.env,
                    )?);
                }
            }
        }
        set_elt(result, row, values::mkchar(&text)?);
    }
    let unused = used.iter().filter(|&&used| !used).count();
    if unused > 0 {
        let format = string_at(formats, 0).unwrap_or(Cow::Borrowed("NA"));
        let what = if unused == 1 {
            "one argument".to_owned()
        } else {
            format!("{unused} arguments")
        };
        warning(format!("{what} not used by format '{format}'"))?;
    }
    Ok(result)
}

/// Splits a format string into literal text and conversions, resolving which argument each
/// conversion (and each `*`) reads.
fn parse_format(format: &str, kinds: &[SexpType], used: &mut [bool]) -> R<Vec<FormatPiece>> {
    let chars: Vec<char> = format.chars().collect();
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut next_argument = 0;
    let mut index = 0;
    let mut take = |explicit: Option<usize>, used: &mut [bool]| -> R<usize> {
        let argument = match explicit {
            Some(argument) => argument,
            None => {
                next_argument += 1;
                next_argument - 1
            }
        };
        match used.get_mut(argument) {
            Some(slot) => {
                *slot = true;
                Ok(argument)
            }
            None => match explicit {
                Some(argument) => Err(error(format!(
                    "reference to non-existent argument {}",
                    argument + 1
                ))),
                None => Err(error("too few arguments")),
            },
        }
    };
    while index < chars.len() {
        if chars[index] != '%' {
            literal.push(chars[index]);
            index += 1;
            continue;
        }
        if chars.get(index + 1) == Some(&'%') {
            literal.push('%');
            index += 2;
            continue;
        }
        let start = index;
        index += 1;
        let explicit = positional_reference(&chars, &mut index);
        let mut conversion = Conversion {
            text: String::new(),
            argument: 0,
            left: false,
            plus: false,
            space: false,
            zero: false,
            alternate: false,
            width: None,
            precision: None,
            kind: '\0',
        };
        while let Some(&character) = chars.get(index) {
            match character {
                '-' => conversion.left = true,
                '+' => conversion.plus = true,
                ' ' => conversion.space = true,
                '0' => conversion.zero = true,
                '#' => conversion.alternate = true,
                _ => break,
            }
            index += 1;
        }
        if chars.get(index) == Some(&'*') {
            index += 1;
            let star = positional_reference(&chars, &mut index);
            conversion.width = Some(Amount::Argument(take(star, used)?));
        } else if let Some(width) = read_number(&chars, &mut index) {
            conversion.width = Some(Amount::Fixed(width));
        }
        if chars.get(index) == Some(&'.') {
            index += 1;
            if chars.get(index) == Some(&'*') {
                index += 1;
                let star = positional_reference(&chars, &mut index);
                conversion.precision = Some(Amount::Argument(take(star, used)?));
            } else {
                conversion.precision =
                    Some(Amount::Fixed(read_number(&chars, &mut index).unwrap_or(0)));
            }
        }
        let kind = chars.get(index).copied();
        let end = (index + 1).min(chars.len());
        conversion.text = chars[start..end].iter().collect();
        const CONVERSIONS: &[char] = &[
            'd', 'i', 'o', 'x', 'X', 'f', 'e', 'E', 'g', 'G', 'a', 'A', 's',
        ];
        if !kind.is_some_and(|kind| CONVERSIONS.contains(&kind)) {
            // GNU R reads a specification up to the next conversion letter, and blames it on
            // the type of the argument it would have formatted.
            let Some(letter) = chars[start + 1..]
                .iter()
                .position(|c| CONVERSIONS.contains(c))
            else {
                let rest: String = chars[start..].iter().collect();
                return Err(error(format!("unrecognised format specification '{rest}'")));
            };
            let chunk: String = chars[start..=start + 1 + letter].iter().collect();
            if kinds.is_empty() {
                return Err(error("too few arguments"));
            }
            let advice = match kinds[take(explicit, used)?] {
                SexpType::Real => "use format %f, %e, %g or %a for numeric objects",
                SexpType::Integer => "use format %d, %i, %o, %x or %X for integer objects",
                SexpType::Logical => "use format %d or %i for logical objects",
                _ => "use format %s for character objects",
            };
            return Err(error(format!("invalid format '{chunk}'; {advice}")));
        }
        conversion.kind = kind.unwrap_or('s');
        if kinds.is_empty() {
            return Err(error("too few arguments"));
        }
        conversion.argument = take(explicit, used)?;
        index = end;
        if !literal.is_empty() {
            pieces.push(FormatPiece::Text(std::mem::take(&mut literal)));
        }
        pieces.push(FormatPiece::Conversion(conversion));
    }
    if !literal.is_empty() {
        pieces.push(FormatPiece::Text(literal));
    }
    Ok(pieces)
}

/// Reads an `n$` argument reference, which is 1-based, if one is written at `index`.
fn positional_reference(chars: &[char], index: &mut usize) -> Option<usize> {
    let mut cursor = *index;
    let number = read_number(chars, &mut cursor)?;
    if chars.get(cursor) == Some(&'$') && number > 0 {
        *index = cursor + 1;
        Some(number - 1)
    } else {
        None
    }
}

fn read_number(chars: &[char], index: &mut usize) -> Option<usize> {
    let start = *index;
    let mut value: usize = 0;
    while let Some(digit) = chars
        .get(*index)
        .and_then(|character| character.to_digit(10))
    {
        value = value.saturating_mul(10).saturating_add(digit as usize);
        *index += 1;
    }
    (*index > start).then_some(value)
}

unsafe fn format_conversion(
    conversion: &Conversion,
    arguments: &mut [FormatArgument],
    row: usize,
    env: Sexp,
) -> R<String> {
    let mut left = conversion.left;
    let width = match conversion.width {
        None => None,
        Some(Amount::Fixed(width)) => Some(width),
        Some(Amount::Argument(position)) => {
            let value = star_value(&arguments[position], row)?;
            if value < 0 {
                left = true;
            }
            Some(value.unsigned_abs() as usize)
        }
    };
    let precision = match conversion.precision {
        None => None,
        Some(Amount::Fixed(precision)) => Some(precision),
        Some(Amount::Argument(position)) => {
            let value = star_value(&arguments[position], row)?;
            (value >= 0).then_some(value as usize)
        }
    };
    let argument = &mut arguments[conversion.argument];
    let index = row % argument.length;
    let value = argument.value;
    let pad = |text: String| pad_bytes(text, width.unwrap_or(0), left);
    if conversion.kind == 's' {
        let text = match argument.text {
            Some(text) => text,
            None => {
                let text = as_character(value, env)?;
                argument.text = Some(text);
                text
            }
        };
        let mut text = string_at(text, index).map_or_else(|| "NA".to_owned(), Cow::into_owned);
        if let Some(precision) = precision {
            truncate_bytes(&mut text, precision);
        }
        return Ok(pad(text));
    }
    let integer_conversion = matches!(conversion.kind, 'd' | 'i' | 'o' | 'x' | 'X');
    match value.kind() {
        SexpType::String => {
            return Err(error(format!(
                "invalid format '{}'; use format %s for character objects",
                conversion.text
            )));
        }
        SexpType::Logical | SexpType::Integer | SexpType::Real => {}
        _ => return Err(error("unsupported type")),
    }
    if integer_conversion {
        let integer = match value.kind() {
            SexpType::Real => {
                let whole = *argument.whole.get_or_insert_with(|| {
                    values::reals(value).iter().all(|&number| {
                        number.is_nan() || (number == number.trunc() && number.abs() < 2147483648.0)
                    })
                });
                if !whole {
                    return Err(error(format!(
                        "invalid format '{}'; use format %f, %e, %g or %a for numeric objects",
                        conversion.text
                    )));
                }
                let number = values::reals(value)[index];
                if number.is_nan() {
                    NA_INTEGER
                } else {
                    number as i32
                }
            }
            _ => values::ints(value)[index],
        };
        if integer == NA_INTEGER {
            return Ok(pad("NA".to_owned()));
        }
        return Ok(format_integer(
            i64::from(integer),
            conversion,
            width,
            left,
            precision,
        ));
    }
    let number = match value.kind() {
        SexpType::Real => values::reals(value)[index],
        _ => {
            let integer = values::ints(value)[index];
            if integer == NA_INTEGER {
                values::na_real()
            } else {
                f64::from(integer)
            }
        }
    };
    if !number.is_finite() {
        let text = if is_na_real(number) {
            "NA".to_owned()
        } else if number.is_nan() {
            "NaN".to_owned()
        } else if number < 0.0 {
            "-Inf".to_owned()
        } else if conversion.plus {
            "+Inf".to_owned()
        } else if conversion.space {
            " Inf".to_owned()
        } else {
            "Inf".to_owned()
        };
        return Ok(pad(text));
    }
    Ok(format_real(number, conversion, width, left, precision))
}

unsafe fn star_value(argument: &FormatArgument, row: usize) -> R<i32> {
    let value = argument.value;
    let index = row % argument.length;
    let number = match value.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(value)[index],
        SexpType::Real => {
            let number = values::reals(value)[index];
            if number.is_nan() {
                NA_INTEGER
            } else {
                number as i32
            }
        }
        _ => NA_INTEGER,
    };
    if number == NA_INTEGER {
        return Err(error(
            "argument for '*' conversion specification must be a number",
        ));
    }
    Ok(number)
}

fn format_integer(
    value: i64,
    conversion: &Conversion,
    width: Option<usize>,
    left: bool,
    precision: Option<usize>,
) -> String {
    let (sign, mut digits, prefix) = match conversion.kind {
        'o' => {
            let digits = format!("{:o}", value as i32 as u32);
            let prefix = if conversion.alternate && !digits.starts_with('0') {
                "0"
            } else {
                ""
            };
            ("", digits, prefix)
        }
        'x' | 'X' => {
            let mut digits = format!("{:x}", value as i32 as u32);
            if conversion.kind == 'X' {
                digits.make_ascii_uppercase();
            }
            let prefix = match (conversion.alternate && value != 0, conversion.kind) {
                (false, _) => "",
                (true, 'x') => "0x",
                (true, _) => "0X",
            };
            ("", digits, prefix)
        }
        _ => {
            let sign = if value < 0 {
                "-"
            } else if conversion.plus {
                "+"
            } else if conversion.space {
                " "
            } else {
                ""
            };
            (sign, value.unsigned_abs().to_string(), "")
        }
    };
    if let Some(precision) = precision {
        if precision == 0 && value == 0 {
            digits.clear();
        } else if digits.len() < precision {
            digits.insert_str(0, &"0".repeat(precision - digits.len()));
        }
    }
    let zero_pad = conversion.zero && !left && precision.is_none();
    join_number(sign, prefix, &digits, width.unwrap_or(0), left, zero_pad)
}

fn format_real(
    value: f64,
    conversion: &Conversion,
    width: Option<usize>,
    left: bool,
    precision: Option<usize>,
) -> String {
    let magnitude = value.abs();
    let alternate = conversion.alternate;
    let mut body = match conversion.kind {
        'f' => {
            let mut text = format!("{:.*}", precision.unwrap_or(6), magnitude);
            if alternate && !text.contains('.') {
                text.push('.');
            }
            text
        }
        'e' | 'E' => exponent_form(magnitude, precision.unwrap_or(6), alternate),
        'g' | 'G' => general_form(magnitude, precision.unwrap_or(6), alternate),
        _ => hexadecimal_form(magnitude, precision, alternate),
    };
    if conversion.kind.is_ascii_uppercase() {
        body.make_ascii_uppercase();
    }
    let (prefix, body) = match conversion.kind {
        'a' | 'A' => body.split_at(2),
        _ => ("", body.as_str()),
    };
    let sign = if value.is_sign_negative() {
        "-"
    } else if conversion.plus {
        "+"
    } else if conversion.space {
        " "
    } else {
        ""
    };
    join_number(
        sign,
        prefix,
        body,
        width.unwrap_or(0),
        left,
        conversion.zero && !left,
    )
}

/// Assembles a formatted number: zeros pad between the sign (and `0x` prefix) and the digits,
/// spaces pad outside.
fn join_number(
    sign: &str,
    prefix: &str,
    digits: &str,
    width: usize,
    left: bool,
    zero_pad: bool,
) -> String {
    let length = sign.len() + prefix.len() + digits.len();
    if zero_pad && length < width {
        return format!("{sign}{prefix}{}{digits}", "0".repeat(width - length));
    }
    pad_bytes(format!("{sign}{prefix}{digits}"), width, left)
}

/// C's `%e`: one digit before the point and an exponent of at least two digits.
fn exponent_form(magnitude: f64, precision: usize, alternate: bool) -> String {
    let text = scientific_text(magnitude, precision);
    if alternate && precision == 0 {
        text.replacen('e', ".e", 1)
    } else {
        text
    }
}

/// C's `%g`: `%e` when the exponent is below -4 or at least the precision, `%f` otherwise,
/// with trailing zeros removed unless `#` is given.
fn general_form(magnitude: f64, precision: usize, alternate: bool) -> String {
    let precision = precision.max(1);
    let exponent = if magnitude == 0.0 {
        0
    } else {
        let text = format!("{:.*e}", precision - 1, magnitude);
        text.split_once('e')
            .and_then(|(_, exponent)| exponent.parse::<i32>().ok())
            .unwrap_or(0)
    };
    let text = if exponent < -4 || exponent >= precision as i32 {
        scientific_text(magnitude, precision - 1)
    } else {
        format!(
            "{:.*}",
            (precision as i32 - 1 - exponent).max(0) as usize,
            magnitude
        )
    };
    if alternate {
        if text.contains('.') {
            return text;
        }
        return match text.split_once('e') {
            Some((mantissa, exponent)) => format!("{mantissa}.e{exponent}"),
            None => format!("{text}."),
        };
    }
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (text.as_str(), None),
    };
    let mantissa = if mantissa.contains('.') {
        mantissa.trim_end_matches('0').trim_end_matches('.')
    } else {
        mantissa
    };
    match exponent {
        Some(exponent) => format!("{mantissa}e{exponent}"),
        None => mantissa.to_owned(),
    }
}

/// C's `%a`: a hexadecimal significand with one leading digit and a binary exponent.
pub fn hexadecimal_form(magnitude: f64, precision: Option<usize>, alternate: bool) -> String {
    const FRACTION_DIGITS: usize = 13;
    let bits = magnitude.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let mut fraction = bits & ((1u64 << 52) - 1);
    let (mut lead, exponent) = match (biased, fraction) {
        (0, 0) => (0u64, 0),
        (0, _) => (0, -1022),
        _ => (1, biased - 1023),
    };
    let mut digits = FRACTION_DIGITS;
    if let Some(precision) = precision.filter(|&precision| precision < FRACTION_DIGITS) {
        let dropped = 4 * (FRACTION_DIGITS - precision) as u32;
        let remainder = fraction & ((1u64 << dropped) - 1);
        let half = 1u64 << (dropped - 1);
        fraction >>= dropped;
        // Ties go to the even digit, which is the leading one when no fraction digit is kept.
        let last_digit = if precision == 0 { lead } else { fraction };
        if remainder > half || (remainder == half && last_digit & 1 == 1) {
            fraction += 1;
            if fraction >> (4 * precision as u32) != 0 {
                fraction &= (1u64 << (4 * precision as u32)) - 1;
                lead += 1;
            }
        }
        digits = precision;
    }
    let mut hex = if digits == 0 {
        String::new()
    } else {
        format!("{fraction:0digits$x}")
    };
    if precision.is_none() {
        hex.truncate(hex.trim_end_matches('0').len());
    }
    let point = if hex.is_empty() && !alternate {
        ""
    } else {
        "."
    };
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("0x{lead}{point}{hex}p{sign}{}", exponent.unsigned_abs())
}

/// Pads to `width` bytes, as C's `printf` counts a field width.
fn pad_bytes(text: String, width: usize, left: bool) -> String {
    if text.len() >= width {
        return text;
    }
    let padding = " ".repeat(width - text.len());
    if left {
        text + &padding
    } else {
        padding + &text
    }
}

/// Cuts `text` to at most `limit` bytes without splitting a character.
fn truncate_bytes(text: &mut String, limit: usize) {
    if text.len() <= limit {
        return;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

unsafe fn change_case(call: &Call<'_>, upper: bool) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("non-character argument"));
    }
    map_strings(x, |text| {
        Some(
            text.chars()
                .map(|character| change_char_case(character, upper))
                .collect(),
        )
    })
}

/// `towupper` and `towlower` map one character to one character, so a character whose case
/// mapping takes several (`ß` to `SS`) is left alone, as in GNU R.
fn change_char_case(character: char, upper: bool) -> char {
    if upper {
        single_char(character.to_uppercase(), character)
    } else {
        single_char(character.to_lowercase(), character)
    }
}

fn single_char(mut mapped: impl Iterator<Item = char>, fallback: char) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(single), None) => single,
        _ => fallback,
    }
}

unsafe fn do_chartr(call: &Call<'_>) -> R<Sexp> {
    let old = expand_character_ranges(&translation_spec(call.arg(0), "old")?);
    let new = expand_character_ranges(&translation_spec(call.arg(1), "new")?);
    let x = call.arg(2);
    if old.len() > new.len() {
        return Err(error("'old' is longer than 'new'"));
    }
    if x.kind() != SexpType::String {
        return Err(error("invalid 'x' argument"));
    }
    map_strings(x, |text| {
        Some(
            text.chars()
                .map(|character| {
                    // A character listed twice translates by its last listing, as GNU R's table
                    // is filled in order.
                    old.iter()
                        .rposition(|&from| from == character)
                        .map_or(character, |position| new[position])
                })
                .collect(),
        )
    })
}

unsafe fn translation_spec(value: Sexp, what: &str) -> R<String> {
    match values::first_string(value) {
        Some(text) if value.kind() == SexpType::String => Ok(text.into_owned()),
        _ => Err(error(format!("invalid '{what}' argument"))),
    }
}

/// Expands `a-z` ranges in a `chartr` specification.
fn expand_character_ranges(spec: &str) -> Vec<char> {
    let chars: Vec<char> = spec.chars().collect();
    let mut expanded = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        match (chars.get(index + 1), chars.get(index + 2)) {
            (Some('-'), Some(&high)) if chars[index] <= high => {
                expanded.extend(chars[index]..=high);
                index += 3;
            }
            _ => {
                expanded.push(chars[index]);
                index += 1;
            }
        }
    }
    expanded
}

unsafe fn do_strtrim(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("strtrim() requires a character vector"));
    }
    let widths = integer_argument(call.arg(1))?;
    if length(x) > 0
        && (widths.is_empty() || widths.iter().any(|&width| width == NA_INTEGER || width < 0))
    {
        return Err(error("invalid 'width' argument"));
    }
    let count = length(x);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let charsxp = x.element(index);
        if charsxp == na_string() {
            set_elt(result, index, charsxp);
            continue;
        }
        let limit = widths[index % widths.len()] as usize;
        let text = values::char_str(charsxp);
        let mut used = 0;
        let mut end = text.len();
        for (offset, character) in text.char_indices() {
            used += display_width(character.encode_utf8(&mut [0; 4]));
            if used > limit {
                end = offset;
                break;
            }
        }
        if end == text.len() {
            set_elt(result, index, charsxp);
        } else {
            set_elt(result, index, values::mkchar(&text[..end])?);
        }
    }
    values::copy_attributes(x, result)?;
    Ok(result)
}

unsafe fn starts_or_ends_with(call: &Call<'_>, starts: bool) -> R<Sexp> {
    let x = call.arg(0);
    let affix = call.arg(1);
    if x.kind() != SexpType::String || affix.kind() != SexpType::String {
        return Err(error("non-character object(s)"));
    }
    let (text_count, affix_count) = (length(x), length(affix));
    let count = if text_count == 0 || affix_count == 0 {
        0
    } else {
        text_count.max(affix_count)
    };
    let result = alloc(SexpType::Logical, count)?;
    for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
        *slot = match (
            string_at(x, index % text_count),
            string_at(affix, index % affix_count),
        ) {
            (Some(text), Some(affix)) => {
                let found = if starts {
                    text.starts_with(affix.as_ref())
                } else {
                    text.ends_with(affix.as_ref())
                };
                found as i32
            }
            _ => NA_LOGICAL,
        };
    }
    Ok(result)
}

unsafe fn do_strrep(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("invalid 'x' argument"));
    }
    let times = integer_argument(call.arg(1))?;
    let (text_count, times_count) = (length(x), times.len());
    let count = if text_count == 0 || times_count == 0 {
        0
    } else {
        text_count.max(times_count)
    };
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let repeat = times[index % times_count];
        let charsxp = x.element(index % text_count);
        if repeat == NA_INTEGER || charsxp == na_string() {
            set_elt(result, index, na_string());
            continue;
        }
        if repeat < 0 {
            return Err(error("invalid 'times' value"));
        }
        set_elt(
            result,
            index,
            values::mkchar(&values::char_str(charsxp).repeat(repeat as usize))?,
        );
    }
    if text_count >= times_count {
        let names = get_attr(x, symbols().names);
        if names != nil() {
            set_attr(result, symbols().names, names)?;
        }
    }
    Ok(result)
}

unsafe fn do_strtoi(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let base = values::as_integer_scalar(call.arg(1));
    if base == NA_INTEGER || base == 1 || !(0..=36).contains(&base) {
        return Err(error("invalid 'base' argument"));
    }
    if x.kind() != SexpType::String {
        return Err(error("invalid 'x' argument"));
    }
    let count = length(x);
    let result = alloc(SexpType::Integer, count)?;
    for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
        *slot = string_at(x, index)
            .and_then(|text| parse_c_integer(&text, base as u32))
            .unwrap_or(NA_INTEGER);
    }
    Ok(result)
}

/// C's `strtol`, requiring the whole text to be a number that fits an R integer. Base 0 reads
/// a `0x` prefix as hexadecimal and a leading `0` as octal.
fn parse_c_integer(text: &str, base: u32) -> Option<i32> {
    let trimmed = text.trim_start_matches(|character: char| character.is_ascii_whitespace());
    let (negative, unsigned) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let hex_digits = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
        .filter(|digits| !digits.is_empty());
    let (base, digits) = match (base, hex_digits) {
        (16 | 0, Some(digits)) => (16, digits),
        (0, None) if unsigned.len() > 1 && unsigned.starts_with('0') => (8, unsigned),
        (0, None) => (10, unsigned),
        (base, _) => (base, unsigned),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: i64 = 0;
    for character in digits.chars() {
        value = value * i64::from(base) + i64::from(character.to_digit(base)?);
        if value > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    let value = if negative { -value } else { value };
    // `i32::MIN` is `NA_integer_`, so it is out of range too.
    (value > i64::from(i32::MIN) && value <= i64::from(i32::MAX)).then_some(value as i32)
}

const RESERVED_WORDS: &[&str] = &[
    "if",
    "else",
    "repeat",
    "while",
    "function",
    "for",
    "next",
    "break",
    "TRUE",
    "FALSE",
    "NULL",
    "Inf",
    "NaN",
    "NA",
    "NA_integer_",
    "NA_real_",
    "NA_character_",
    "NA_complex_",
    "in",
];

unsafe fn do_make_names(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    if names.kind() != SexpType::String {
        return Err(error("non-character names"));
    }
    let allow_underscore = values::as_logical_scalar(call.arg(1));
    if allow_underscore == NA_LOGICAL {
        return Err(error("invalid 'allow_' value"));
    }
    let allow_underscore = allow_underscore == 1;
    let made: Vec<String> = strings(names)
        .into_iter()
        .map(|name| {
            let name = name.unwrap_or_else(|| "NA".to_owned());
            let mut chars = name.chars();
            let needs_prefix = match (chars.next(), chars.next()) {
                (None, _) => true,
                (Some('.'), Some(second)) => second.is_ascii_digit(),
                (Some(first), _) => !(first.is_alphabetic() || first == '.'),
            };
            let mut made: String = if needs_prefix {
                "X".to_owned()
            } else {
                String::new()
            };
            made.extend(name.chars().map(|character| {
                if character.is_alphanumeric()
                    || character == '.'
                    || (allow_underscore && character == '_')
                {
                    character
                } else {
                    '.'
                }
            }));
            if RESERVED_WORDS.contains(&made.as_str()) {
                made.push('.');
            }
            made
        })
        .collect();
    string_vector(&made)
}

unsafe fn do_make_unique(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    if names.kind() != SexpType::String {
        return Err(error("'names' must be a character vector"));
    }
    let separator = match values::first_string(call.arg(1)) {
        Some(text) if call.arg(1).kind() == SexpType::String && length(call.arg(1)) == 1 => {
            text.into_owned()
        }
        _ => return Err(error("'sep' must be a character string")),
    };
    let original: Vec<String> = strings(names)
        .into_iter()
        .map(|name| name.unwrap_or_else(|| "NA".to_owned()))
        .collect();
    let mut taken: FxHashSet<String> = original.iter().cloned().collect();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    let mut counters: FxHashMap<&str, usize> = FxHashMap::default();
    let mut made = Vec::with_capacity(original.len());
    for name in &original {
        if seen.insert(name.as_str()) {
            made.push(name.clone());
            continue;
        }
        let counter = counters.entry(name.as_str()).or_insert(1);
        let mut candidate = format!("{name}{separator}{counter}");
        while taken.contains(&candidate) {
            *counter += 1;
            candidate = format!("{name}{separator}{counter}");
        }
        *counter += 1;
        taken.insert(candidate.clone());
        made.push(candidate);
    }
    string_vector(&made)
}

unsafe fn do_abbreviate(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("the first argument must be a character vector"));
    }
    let minimum = values::as_integer_scalar(call.arg(1));
    if minimum == NA_INTEGER {
        return Err(error("invalid 'minlength' argument"));
    }
    let use_classes = flag(call.arg(2));
    let mut warned = false;
    let count = length(x);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let Some(text) = string_at(x, index) else {
            set_elt(result, index, na_string());
            continue;
        };
        if !text.is_ascii() && !warned {
            warning("abbreviate used with non-ASCII chars")?;
            warned = true;
        }
        let abbreviation = abbreviate(&text, minimum.max(0) as usize, use_classes);
        set_elt(result, index, values::mkchar(&abbreviation)?);
    }
    Ok(result)
}

/// Shortens `text` to `minimum` characters as `?abbreviate` documents it: lower-case vowels go
/// first, then lower-case letters, then anything else, each time from the right, and the first
/// character of every word is always kept. Spaces separate the words and are dropped last.
fn abbreviate(text: &str, minimum: usize, use_classes: bool) -> String {
    let mut chars: Vec<char> = text.trim_matches(' ').chars().collect();
    if chars.len() <= minimum {
        return chars.into_iter().collect();
    }
    let visible = |chars: &[char]| chars.iter().filter(|&&character| character != ' ').count();
    let word_start = |chars: &[char], index: usize| index == 0 || chars[index - 1] == ' ';
    let mut classes: Vec<fn(char) -> bool> = Vec::new();
    if use_classes {
        classes.push(|character| matches!(character, 'a' | 'e' | 'i' | 'o' | 'u'));
        classes.push(|character| character.is_lowercase());
    }
    classes.push(|character| character != ' ');
    'classes: for removable in classes {
        let mut index = chars.len();
        while index > 1 {
            index -= 1;
            if visible(&chars) <= minimum {
                break 'classes;
            }
            if removable(chars[index]) && !word_start(&chars, index) {
                chars.remove(index);
            }
        }
    }
    if chars.len() > minimum {
        chars.retain(|&character| character != ' ');
    }
    chars.into_iter().collect()
}

unsafe fn do_utf8_to_int(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String || length(x) == 0 {
        return Err(error("argument must be a character vector of length 1"));
    }
    if length(x) > 1 {
        warning(
            "argument should be a character vector of length 1\nall but the first element will be ignored",
        )?;
    }
    let charsxp = x.element(0);
    if charsxp == na_string() {
        return scalar_integer(NA_INTEGER);
    }
    match std::str::from_utf8(charsxp.bytes()) {
        Ok(text) => {
            let points: Vec<i32> = text.chars().map(|character| character as i32).collect();
            values::integer_vector(&points)
        }
        Err(_) => scalar_integer(NA_INTEGER),
    }
}

unsafe fn do_int_to_utf8(call: &Call<'_>) -> R<Sexp> {
    let points = coerce_vector(call.arg(0), SexpType::Integer)?;
    let multiple = flag(call.arg(1));
    let surrogate_pairs = flag(call.arg(2));
    let points = values::ints(points);
    if multiple {
        let texts: Vec<Option<String>> = points
            .iter()
            .map(|&point| match point {
                0 => Some(String::new()),
                NA_INTEGER => None,
                _ => char::from_u32(point as u32).map(String::from),
            })
            .collect();
        return string_vector_na(&texts);
    }
    let mut text = String::with_capacity(points.len());
    let mut index = 0;
    while index < points.len() {
        let point = points[index];
        index += 1;
        if point == 0 {
            continue;
        }
        if point == NA_INTEGER {
            return values::scalar_string_charsxp(na_string());
        }
        let high_surrogate = (0xD800..0xDC00).contains(&point);
        let next = points.get(index).copied().unwrap_or(0);
        let character = if surrogate_pairs && high_surrogate && (0xDC00..0xE000).contains(&next) {
            index += 1;
            char::from_u32((((point - 0xD800) << 10) | (next - 0xDC00)) as u32 + 0x10000)
        } else {
            char::from_u32(point as u32)
        };
        match character {
            Some(character) => text.push(character),
            None => return values::scalar_string_charsxp(na_string()),
        }
    }
    scalar_string(&text)
}

unsafe fn do_char_to_raw(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String || length(x) == 0 {
        return Err(error("argument must be a character vector of length 1"));
    }
    if length(x) > 1 {
        warning(
            "argument should be a character vector of length 1\nall but the first element will be ignored",
        )?;
    }
    let bytes = x.element(0).bytes();
    let result = alloc(SexpType::Raw, bytes.len())?;
    values::raws_mut(result).copy_from_slice(bytes);
    Ok(result)
}

unsafe fn do_raw_to_char(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::Raw {
        return Err(error("argument 'x' must be a raw vector"));
    }
    let multiple = values::as_logical_scalar(call.arg(1));
    if multiple == NA_LOGICAL {
        return Err(error("argument 'multiple' must be TRUE or FALSE"));
    }
    let bytes = values::raws(x);
    if multiple == 1 {
        let result = alloc(SexpType::String, bytes.len())?;
        for (index, &byte) in bytes.iter().enumerate() {
            let piece: &[u8] = if byte == 0 {
                &[]
            } else {
                std::slice::from_ref(&byte)
            };
            set_elt(
                result,
                index,
                values::mkchar_bytes(piece, Encoding::Native)?,
            );
        }
        return Ok(result);
    }
    let end = bytes
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last + 1);
    let bytes = &bytes[..end];
    if bytes.contains(&0) {
        let shown: String = bytes
            .iter()
            .map(|&byte| {
                if byte == 0 {
                    "\\0".to_owned()
                } else {
                    (byte as char).to_string()
                }
            })
            .collect();
        return Err(error(format!("embedded nul in string: '{shown}'")));
    }
    values::scalar_string_charsxp(values::mkchar_bytes(bytes, Encoding::Native)?)
}

unsafe fn do_pmatch(call: &Call<'_>) -> R<Sexp> {
    let targets = match_strings(call.arg(0))?;
    let table = match_strings(call.arg(1))?;
    let no_match = values::as_integer_scalar(call.arg(2));
    let duplicates = flag(call.arg(3));
    let mut matched: Vec<Option<usize>> = vec![None; targets.len()];
    let mut used = vec![false; table.len()];
    for (target, slot) in targets.iter().zip(matched.iter_mut()) {
        if target.is_empty() {
            continue;
        }
        if let Some(position) = (0..table.len())
            .find(|&position| (duplicates || !used[position]) && table[position] == *target)
        {
            *slot = Some(position);
            used[position] = !duplicates;
        }
    }
    for (target, slot) in targets.iter().zip(matched.iter_mut()) {
        if target.is_empty() || slot.is_some() {
            continue;
        }
        let mut candidates = (0..table.len()).filter(|&position| {
            (duplicates || !used[position]) && table[position].starts_with(target.as_str())
        });
        if let (Some(position), None) = (candidates.next(), candidates.next()) {
            *slot = Some(position);
            used[position] = !duplicates;
        }
    }
    let indices: Vec<i32> = matched
        .into_iter()
        .map(|slot| slot.map_or(no_match, |position| position as i32 + 1))
        .collect();
    values::integer_vector(&indices)
}

unsafe fn do_charmatch(call: &Call<'_>) -> R<Sexp> {
    let targets = match_strings(call.arg(0))?;
    let table = match_strings(call.arg(1))?;
    let no_match = values::as_integer_scalar(call.arg(2));
    let indices: Vec<i32> = targets
        .iter()
        .map(|target| {
            let exact: Vec<usize> = (0..table.len())
                .filter(|&position| table[position] == *target)
                .collect();
            let partial: Vec<usize> = (0..table.len())
                .filter(|&position| table[position].starts_with(target.as_str()))
                .collect();
            match (exact.as_slice(), partial.as_slice()) {
                ([position], _) => *position as i32 + 1,
                ([_, _, ..], _) => 0,
                ([], [position]) => *position as i32 + 1,
                ([], [_, _, ..]) => 0,
                ([], []) => no_match,
            }
        })
        .collect();
    values::integer_vector(&indices)
}

/// The strings of a matching argument, with `NA` read as the text `"NA"`, as GNU R does.
unsafe fn match_strings(value: Sexp) -> R<Vec<String>> {
    if value.kind() != SexpType::String && value != nil() {
        return Err(error("argument is not of mode character"));
    }
    Ok(strings(value)
        .into_iter()
        .map(|text| text.unwrap_or_else(|| "NA".to_owned()))
        .collect())
}

unsafe fn grep(call: &Call<'_>, logical: bool) -> R<Sexp> {
    let text = call.arg(1);
    let value = flag(call.arg(3));
    let invert = flag(call.arg(7));
    let options = pattern_options(flag(call.arg(2)), flag(call.arg(4)), flag(call.arg(5)))?;
    let use_bytes = flag(call.arg(6)) || has_bytes(&[call.arg(0), text]);
    let pattern = pattern_argument(call.arg(0), use_bytes)?;
    if text.kind() != SexpType::String && text != nil() {
        return Err(error("invalid 'text' argument"));
    }
    let count = length(text);
    let Some(pattern) = pattern else {
        return match (logical, value) {
            (true, _) => values::logical_vector(&vec![NA_LOGICAL; count]),
            (false, true) => string_vector_na(&vec![None; count]),
            (false, false) => values::integer_vector(&vec![NA_INTEGER; count]),
        };
    };
    let matcher = Matcher::compile(
        &pattern,
        options,
        CompileErrors::for_text(&pattern, text, use_bytes),
    )?;
    let mut hits = Vec::with_capacity(count);
    for index in 0..count {
        let found = match subject_at(text, index, use_bytes) {
            Some(_) if !valid_subject(text, index, use_bytes, options, false)? => false,
            Some(element) => matcher.find_at(&element, 0)?.is_some(),
            None => false,
        };
        hits.push(found != invert);
    }
    if logical {
        let hits: Vec<i32> = hits.into_iter().map(i32::from).collect();
        return values::logical_vector(&hits);
    }
    let selected: Vec<usize> = (0..count).filter(|&index| hits[index]).collect();
    if !value {
        let indices: Vec<i32> = selected.iter().map(|&index| index as i32 + 1).collect();
        return values::integer_vector(&indices);
    }
    let result = alloc(SexpType::String, selected.len())?;
    for (position, &index) in selected.iter().enumerate() {
        set_elt(result, position, text.element(index));
    }
    let names = get_attr(text, symbols().names);
    if names != nil() {
        let selected_names = alloc(SexpType::String, selected.len())?;
        for (position, &index) in selected.iter().enumerate() {
            set_elt(selected_names, position, names.element(index));
        }
        set_attr(result, symbols().names, selected_names)?;
    }
    Ok(result)
}

unsafe fn substitute(call: &Call<'_>, global: bool) -> R<Sexp> {
    let replacement = call.arg(1);
    let text = call.arg(2);
    let options = pattern_options(flag(call.arg(3)), flag(call.arg(4)), flag(call.arg(5)))?;
    let forced_bytes = has_bytes(&[call.arg(0), replacement, text]);
    let use_bytes = flag(call.arg(6)) || forced_bytes;
    let pattern = pattern_argument(call.arg(0), use_bytes)?;
    if replacement.kind() != SexpType::String || length(replacement) == 0 {
        return Err(error("invalid 'replacement' argument"));
    }
    if length(replacement) > 1 {
        warning("argument 'replacement' has length > 1 and only the first element will be used")?;
    }
    if text.kind() != SexpType::String && text != nil() {
        return Err(error("invalid 'text' argument"));
    }
    let count = length(text);
    let result = alloc(SexpType::String, count)?;
    values::copy_attributes(text, result)?;
    let Some(pattern) = pattern else {
        for index in 0..count {
            set_elt(result, index, na_string());
        }
        return Ok(result);
    };
    if options.fixed && pattern.is_empty() {
        return Err(error("zero-length pattern"));
    }
    let matcher = Matcher::compile(&pattern, options, CompileErrors::Quoted)?;
    let replacement =
        subject_at(replacement, 0, use_bytes).map(|text| parse_replacement(&text, options));
    for index in 0..count {
        let charsxp = text.element(index);
        if charsxp == na_string() {
            set_elt(result, index, na_string());
            continue;
        }
        valid_subject(text, index, use_bytes, options, true)?;
        let element = if use_bytes {
            values::escape_high_bytes(charsxp.bytes())
        } else {
            values::char_str(charsxp)
        };
        let substituted = match &replacement {
            Some(replacement) => replace(&matcher, &element, replacement, global)?,
            None => matcher.find_at(&element, 0)?.map(|_| String::new()),
        };
        match (substituted, &replacement) {
            (None, _) => set_elt(result, index, charsxp),
            (Some(_), None) => set_elt(result, index, na_string()),
            (Some(substituted), Some(_)) => {
                set_elt(result, index, regex_result(&substituted, forced_bytes)?)
            }
        }
    }
    Ok(result)
}

/// One part of a replacement string.
enum ReplacementPiece {
    Text(String),
    /// `\\1` to `\\9`.
    Group(usize),
    /// `\\U`, `\\L` and `\\E` (with `perl = TRUE`): how later groups are cased.
    Case(Case),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Upper,
    Lower,
    Keep,
}

fn parse_replacement(text: &str, options: PatternOptions) -> Vec<ReplacementPiece> {
    if options.fixed {
        return vec![ReplacementPiece::Text(text.to_owned())];
    }
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            literal.push(character);
            continue;
        }
        let piece = match chars.next() {
            Some(digit @ '1'..='9') => ReplacementPiece::Group(digit as usize - '0' as usize),
            Some('U') if options.perl => ReplacementPiece::Case(Case::Upper),
            Some('L') if options.perl => ReplacementPiece::Case(Case::Lower),
            Some('E') if options.perl => ReplacementPiece::Case(Case::Keep),
            Some(escaped) => {
                literal.push(escaped);
                continue;
            }
            None => break,
        };
        if !literal.is_empty() {
            pieces.push(ReplacementPiece::Text(std::mem::take(&mut literal)));
        }
        pieces.push(piece);
    }
    if !literal.is_empty() {
        pieces.push(ReplacementPiece::Text(literal));
    }
    pieces
}

/// Replaces the first match, or every match when `global`, or returns `None` when nothing
/// matches. An empty match right where the previous match ended is skipped, as GNU R does.
fn replace(
    matcher: &Matcher,
    text: &str,
    replacement: &[ReplacementPiece],
    global: bool,
) -> R<Option<String>> {
    let need_groups = replacement
        .iter()
        .any(|piece| matches!(piece, ReplacementPiece::Group(_)));
    let mut output = String::with_capacity(text.len());
    let mut position = 0;
    let mut previous_end = None;
    let mut matched = false;
    loop {
        let groups = if need_groups {
            matcher.captures_at(text, position)?
        } else {
            matcher
                .find_at(text, position)?
                .map(|span| vec![Some(span)])
        };
        let Some(groups) = groups else {
            break;
        };
        let Some((start, end)) = groups.first().copied().flatten() else {
            break;
        };
        if start == end && previous_end == Some(start) {
            if start >= text.len() {
                break;
            }
            let next = next_boundary(text, start);
            output.push_str(&text[position..next]);
            position = next;
            continue;
        }
        matched = true;
        output.push_str(&text[position..start]);
        let mut case = Case::Keep;
        for piece in replacement {
            match piece {
                ReplacementPiece::Text(literal) => output.push_str(literal),
                ReplacementPiece::Case(new_case) => case = *new_case,
                ReplacementPiece::Group(group) => {
                    if let Some(Some((group_start, group_end))) = groups.get(*group) {
                        let captured = &text[*group_start..*group_end];
                        match case {
                            Case::Keep => output.push_str(captured),
                            Case::Upper | Case::Lower => {
                                output.extend(captured.chars().map(|character| {
                                    change_char_case(character, case == Case::Upper)
                                }))
                            }
                        }
                    }
                }
            }
        }
        previous_end = Some(end);
        if start == end {
            if end >= text.len() {
                position = end;
                break;
            }
            let next = next_boundary(text, end);
            output.push_str(&text[end..next]);
            position = next;
        } else {
            position = end;
        }
        if !global || (start != end && position >= text.len()) {
            break;
        }
    }
    if !matched {
        return Ok(None);
    }
    output.push_str(&text[position.min(text.len())..]);
    Ok(Some(output))
}

/// `regexpr` and `gregexpr`: match positions, with the attributes base R's `regmatches` reads.
unsafe fn regexpr(call: &Call<'_>, global: bool) -> R<Sexp> {
    let text = call.arg(1);
    let options = pattern_options(flag(call.arg(2)), flag(call.arg(3)), flag(call.arg(4)))?;
    let use_bytes = flag(call.arg(5)) || has_bytes(&[call.arg(0), text]);
    let Some(pattern) = pattern_argument(call.arg(0), use_bytes)? else {
        return Err(error("invalid 'pattern' argument"));
    };
    if text.kind() != SexpType::String && text != nil() {
        return Err(error("invalid 'text' argument"));
    }
    let matcher = Matcher::compile(
        &pattern,
        options,
        CompileErrors::for_text(&pattern, text, use_bytes),
    )?;
    let report = MatchReport::new(&pattern, text, use_bytes);
    let groups = if options.perl {
        matcher.group_count()
    } else {
        0
    };
    let names = matcher.group_names();
    // Only PCRE reports capture groups here, and finding the bare match keeps TRE patterns on
    // the automaton rather than the backtracker.
    let search = |element: &str, position: usize| -> R<Option<Groups>> {
        if groups == 0 {
            Ok(matcher
                .find_at(element, position)?
                .map(|span| vec![Some(span)]))
        } else {
            matcher.captures_at(element, position)
        }
    };
    let count = length(text);
    if !global {
        let mut table = MatchTable::new(groups);
        for index in 0..count {
            match subject_at(text, index, use_bytes) {
                None => table.push_na(),
                Some(_) if !valid_subject(text, index, use_bytes, options, false)? => {
                    table.push_none()
                }
                Some(element) => match search(&element, 0)? {
                    Some(found) => table.push(&element, &found),
                    None => table.push_none(),
                },
            }
        }
        return table.finish(&report, &names);
    }
    let result = alloc(SexpType::List, count)?;
    for index in 0..count {
        let mut table = MatchTable::new(groups);
        let Some(element) = subject_at(text, index, use_bytes) else {
            set_elt(result, index, na_match()?);
            continue;
        };
        if !valid_subject(text, index, use_bytes, options, false)? {
            table.push_none();
            set_elt(result, index, table.finish(&report, &names)?);
            continue;
        }
        let mut position = 0;
        while position < element.len() {
            let Some(found) = search(&element, position)? else {
                break;
            };
            let Some((start, end)) = found.first().copied().flatten() else {
                break;
            };
            table.push(&element, &found);
            position = if start == end {
                next_boundary(&element, end)
            } else {
                end
            };
        }
        if table.rows == 0 {
            table.push_none();
        }
        set_elt(result, index, table.finish(&report, &names)?);
    }
    Ok(result)
}

/// `grepRaw(pattern, x, offset, ignore.case, fixed, value, all, invert)`, matching bytes. The
/// matches are non-overlapping and start at the 1-based `offset`; `value` returns the matched
/// bytes, or with `invert` the bytes around them.
unsafe fn do_grep_raw(call: &Call<'_>) -> R<Sexp> {
    let (pattern, x) = (call.arg(0), call.arg(1));
    if pattern.kind() != SexpType::Raw || x.kind() != SexpType::Raw {
        return Err(call.error("invalid 'pattern' or 'x' argument"));
    }
    let offset = values::as_integer_scalar(call.arg(2));
    if offset == NA_INTEGER || offset < 1 {
        return Err(call.error("invalid 'offset' argument"));
    }
    let flag = |index: usize| values::as_logical_scalar(call.arg(index)) == 1;
    let (ignore_case, fixed, value, all, mut invert) =
        (flag(3), flag(4), flag(5), flag(6), flag(7));
    if invert && !value {
        warning("argument 'invert = TRUE' will be ignored")?;
        invert = false;
    }
    let needle = values::raws(pattern).to_vec();
    let haystack = values::raws(x).to_vec();
    let start = offset as usize - 1;
    let regex = if fixed {
        if ignore_case {
            warning("argument 'ignore.case = TRUE' will be ignored")?;
        }
        None
    } else {
        let text = translate_regex(&String::from_utf8_lossy(&needle), true);
        let flags = if ignore_case { "(?i-u)" } else { "(?-u)" };
        let compiled = regex::bytes::Regex::new(&format!("{flags}{text}")).map_err(|failure| {
            call.error(format!(
                "invalid regular expression '{}': {failure}",
                String::from_utf8_lossy(&needle)
            ))
        })?;
        Some(compiled)
    };
    let find = |from: usize| -> Option<(usize, usize)> {
        if from > haystack.len() {
            return None;
        }
        match &regex {
            Some(regex) => regex
                .find_at(&haystack, from)
                .map(|found| (found.start(), found.end())),
            None if needle.is_empty() => Some((from, from)),
            None => haystack[from..]
                .windows(needle.len())
                .position(|window| window == needle.as_slice())
                .map(|position| (from + position, from + position + needle.len())),
        }
    };
    let mut matches = Vec::new();
    let mut from = start;
    while let Some((begin, end)) = find(from) {
        matches.push((begin, end));
        if !all {
            break;
        }
        // An empty match moves on by one byte, or the search would never advance.
        from = if end > begin { end } else { end + 1 };
    }
    let raw = |bytes: &[u8]| -> R<Sexp> {
        let result = alloc(SexpType::Raw, bytes.len())?;
        values::raws_mut(result).copy_from_slice(bytes);
        Ok(result)
    };
    if !value {
        let positions: Vec<i32> = matches.iter().map(|&(begin, _)| begin as i32 + 1).collect();
        return values::integer_vector(&positions);
    }
    match (all, invert) {
        (false, false) => match matches.first() {
            Some(&(begin, end)) => raw(&haystack[begin..end]),
            None => raw(&[]),
        },
        (false, true) => match matches.first() {
            Some(&(begin, end)) => raw(&[&haystack[..begin], &haystack[end..]].concat()),
            None => raw(&haystack),
        },
        (true, false) => {
            let pieces = matches
                .iter()
                .map(|&(begin, end)| raw(&haystack[begin..end]))
                .collect::<R<Vec<_>>>()?;
            values::list_vector(&pieces)
        }
        (true, true) => {
            let mut pieces = Vec::with_capacity(matches.len() + 1);
            let mut previous = 0;
            for &(begin, end) in &matches {
                pieces.push(raw(&haystack[previous..begin])?);
                previous = end;
            }
            pieces.push(raw(&haystack[previous..])?);
            values::list_vector(&pieces)
        }
    }
}

unsafe fn do_regexec(call: &Call<'_>) -> R<Sexp> {
    let text = call.arg(1);
    let options = pattern_options(flag(call.arg(2)), false, flag(call.arg(3)))?;
    let use_bytes = flag(call.arg(4)) || has_bytes(&[call.arg(0), text]);
    let Some(pattern) = pattern_argument(call.arg(0), use_bytes)? else {
        return Err(error("invalid 'pattern' argument"));
    };
    if text.kind() != SexpType::String && text != nil() {
        return Err(error("invalid 'text' argument"));
    }
    let matcher = Matcher::compile(&pattern, options, CompileErrors::Regexec)?;
    let report = MatchReport::new(&pattern, text, use_bytes);
    let count = length(text);
    let result = alloc(SexpType::List, count)?;
    for index in 0..count {
        let Some(element) = subject_at(text, index, use_bytes) else {
            set_elt(result, index, na_match()?);
            continue;
        };
        let (starts, lengths) = match matcher.captures_at(&element, 0)? {
            Some(found) => found
                .iter()
                .map(|group| match group {
                    Some((start, end)) => (
                        position_of(&element, *start) + 1,
                        position_of(&element, *end) - position_of(&element, *start),
                    ),
                    None => (0, 0),
                })
                .unzip(),
            None => (vec![-1], vec![-1]),
        };
        let positions = values::integer_vector(&starts)?;
        set_attr(
            positions,
            values::install("match.length")?,
            values::integer_vector(&lengths)?,
        )?;
        report.annotate(positions)?;
        set_elt(result, index, positions);
    }
    Ok(result)
}

/// How match positions are reported: in characters, or in bytes when `useBytes = TRUE`. GNU R
/// also matches byte-wise, and says so in the `useBytes` attribute, when the pattern and every
/// text are ASCII; the attributes are left off otherwise.
struct MatchReport {
    bytes: bool,
    annotated: bool,
}

impl MatchReport {
    unsafe fn new(pattern: &str, text: Sexp, use_bytes: bool) -> MatchReport {
        let ascii = pattern.is_ascii()
            && (0..length(text)).all(|index| {
                let charsxp = text.element(index);
                charsxp == na_string() || charsxp.bytes().is_ascii()
            });
        MatchReport {
            bytes: use_bytes,
            annotated: use_bytes || ascii,
        }
    }

    unsafe fn annotate(&self, vector: Sexp) -> R<()> {
        if self.annotated {
            let index_type = if self.bytes { "bytes" } else { "chars" };
            set_attr(
                vector,
                values::install("index.type")?,
                scalar_string(index_type)?,
            )?;
            set_attr(
                vector,
                values::install("useBytes")?,
                values::scalar_logical(1)?,
            )?;
        }
        Ok(())
    }
}

/// The rows of a `regexpr` result: one per text (or per match, for `gregexpr`), with the capture
/// groups as matrix columns.
struct MatchTable {
    groups: usize,
    rows: usize,
    starts: Vec<i32>,
    lengths: Vec<i32>,
    /// Row-major while collecting; transposed into R's column-major order at the end.
    capture_starts: Vec<i32>,
    capture_lengths: Vec<i32>,
}

impl MatchTable {
    fn new(groups: usize) -> MatchTable {
        MatchTable {
            groups,
            rows: 0,
            starts: Vec::new(),
            lengths: Vec::new(),
            capture_starts: Vec::new(),
            capture_lengths: Vec::new(),
        }
    }

    fn push(&mut self, text: &str, found: &[Option<(usize, usize)>]) {
        let span = |group: Option<&Option<(usize, usize)>>| match group {
            Some(Some((start, end))) => {
                let first = position_of(text, *start);
                (first + 1, position_of(text, *end) - first)
            }
            _ => (0, 0),
        };
        let (start, length) = span(found.first());
        self.starts.push(start);
        self.lengths.push(length);
        for group in 1..=self.groups {
            let (start, length) = span(found.get(group));
            self.capture_starts.push(start);
            self.capture_lengths.push(length);
        }
        self.rows += 1;
    }

    fn push_none(&mut self) {
        self.push_filled(-1);
    }

    fn push_na(&mut self) {
        self.push_filled(NA_INTEGER);
    }

    fn push_filled(&mut self, value: i32) {
        self.starts.push(value);
        self.lengths.push(value);
        self.capture_starts
            .extend(std::iter::repeat_n(value, self.groups));
        self.capture_lengths
            .extend(std::iter::repeat_n(value, self.groups));
        self.rows += 1;
    }

    unsafe fn finish(&self, report: &MatchReport, names: &[String]) -> R<Sexp> {
        let positions = values::integer_vector(&self.starts)?;
        set_attr(
            positions,
            values::install("match.length")?,
            values::integer_vector(&self.lengths)?,
        )?;
        report.annotate(positions)?;
        if self.groups > 0 {
            let column_names = string_vector(names)?;
            let dimnames = values::list_vector(&[nil(), column_names])?;
            let dim = values::integer_vector(&[self.rows as i32, self.groups as i32])?;
            for (attribute, row_major) in [
                ("capture.start", &self.capture_starts),
                ("capture.length", &self.capture_lengths),
            ] {
                let matrix = alloc(SexpType::Integer, self.rows * self.groups)?;
                let cells = values::ints_mut(matrix);
                for row in 0..self.rows {
                    for group in 0..self.groups {
                        cells[group * self.rows + row] = row_major[row * self.groups + group];
                    }
                }
                set_attr(matrix, symbols().dim, dim)?;
                set_attr(matrix, symbols().dimnames, dimnames)?;
                set_attr(positions, values::install(attribute)?, matrix)?;
            }
            set_attr(positions, values::install("capture.names")?, column_names)?;
        }
        Ok(positions)
    }
}

/// The result for an `NA` text in `gregexpr` and `regexec`: `NA` with only a match length.
unsafe fn na_match() -> R<Sexp> {
    let positions = scalar_integer(NA_INTEGER)?;
    set_attr(
        positions,
        values::install("match.length")?,
        scalar_integer(NA_INTEGER)?,
    )?;
    Ok(positions)
}

/// The character position of byte `offset` in `text`. With `useBytes = TRUE` the text holds one
/// character per byte (see [`subject_at`]), so this counts bytes then.
fn position_of(text: &str, offset: usize) -> i32 {
    if text.is_ascii() {
        offset as i32
    } else {
        text[..offset].chars().count() as i32
    }
}

/// How a pattern is read, from the `ignore.case`, `perl` and `fixed` arguments.
#[derive(Clone, Copy)]
struct PatternOptions {
    ignore_case: bool,
    perl: bool,
    fixed: bool,
}

/// Reads the pattern flags, warning as GNU R does about the ones `fixed = TRUE` overrides.
unsafe fn pattern_options(ignore_case: bool, perl: bool, fixed: bool) -> R<PatternOptions> {
    if fixed && ignore_case {
        warning("argument 'ignore.case = TRUE' will be ignored")?;
    }
    if fixed && perl {
        warning("argument 'perl = TRUE' will be ignored")?;
    }
    Ok(PatternOptions {
        ignore_case: ignore_case && !fixed,
        perl: perl && !fixed,
        fixed,
    })
}

/// Whether any string in `vectors` is marked as bytes. GNU R then matches every input byte by
/// byte, as if `useBytes = TRUE` had been given, because bytes have no characters to match.
unsafe fn has_bytes(vectors: &[Sexp]) -> bool {
    vectors.iter().any(|&vector| {
        vector.kind() == SexpType::String
            && (0..length(vector))
                .any(|index| Encoding::of(vector.element(index)) == Encoding::Bytes)
    })
}

/// A string built from matched text. When bytes-marked input forced byte matching, GNU R marks
/// a result that is not ASCII as bytes too.
unsafe fn regex_result(text: &str, forced_bytes: bool) -> R<Sexp> {
    match (forced_bytes, values::unescape_invalid_bytes(text)) {
        (true, Some(bytes)) => values::mkchar_bytes(&bytes, Encoding::Bytes),
        (true, None) if !text.is_ascii() => values::mkchar_bytes(text.as_bytes(), Encoding::Bytes),
        _ => values::mkchar(text),
    }
}

/// The pattern's first element, or `None` for `NA`, read as [`subject_at`] reads text.
unsafe fn pattern_argument(pattern: Sexp, bytes: bool) -> R<Option<String>> {
    if pattern.kind() != SexpType::String || length(pattern) == 0 {
        return Err(error("invalid 'pattern' argument"));
    }
    if length(pattern) > 1 {
        warning("argument 'pattern' has length > 1 and only the first element will be used")?;
    }
    Ok(subject_at(pattern, 0, bytes).map(Cow::into_owned))
}

/// Element `index` of a character vector as the regular expression engine sees it, or `None`
/// for `NA`. With `useBytes = TRUE` each byte past ASCII becomes a character of its own (see
/// [`values::escape_high_bytes`]), so the engine matches bytes: `.` takes one byte, case folding
/// stops at ASCII, and a pattern byte can match inside a multibyte character. Strings made from
/// the result turn those characters back into the bytes.
pub(crate) unsafe fn subject_at<'a>(
    object: Sexp,
    index: usize,
    bytes: bool,
) -> Option<Cow<'a, str>> {
    if !bytes {
        return string_at(object, index);
    }
    if object.kind() != SexpType::String || index >= length(object) {
        return None;
    }
    let charsxp = object.element(index);
    (charsxp != na_string()).then(|| values::escape_high_bytes(charsxp.bytes()))
}

/// Whether element `index` of `text` can be matched. GNU R refuses a string marked as UTF-8
/// whose bytes are not UTF-8: the regex functions warn and treat it as unmatched, and `sub`
/// and `gsub` (`fatal`) fail. TRE first fails to convert it to wide characters.
unsafe fn valid_subject(
    text: Sexp,
    index: usize,
    use_bytes: bool,
    options: PatternOptions,
    fatal: bool,
) -> R<bool> {
    let charsxp = text.element(index);
    if use_bytes
        || Encoding::of(charsxp) != Encoding::Utf8
        || std::str::from_utf8(charsxp.bytes()).is_ok()
    {
        return Ok(true);
    }
    let message = if options.fixed || options.perl {
        format!("input string {} is invalid UTF-8", index + 1)
    } else {
        let mut shown = String::new();
        for chunk in charsxp.bytes().utf8_chunks() {
            shown.push_str(chunk.valid());
            for byte in chunk.invalid() {
                shown.push_str(&format!("<{byte:02x}>"));
            }
        }
        warning(format!("unable to translate '{shown}' to a wide string"))?;
        format!("input string {} is invalid", index + 1)
    };
    if fatal {
        return Err(error(message));
    }
    warning(message)?;
    Ok(false)
}

/// The byte ranges of a match (group 0) and of its capture groups, `None` for a group that took
/// no part in the match.
type Groups = Vec<Option<(usize, usize)>>;

/// How a TRE pattern that does not compile is reported. GNU R words the error differently on
/// each path: the wide-character path of `regexpr`, `gregexpr` and `grep` (taken when the
/// pattern or a text is not ASCII) and `regexec` raise the error alone, while every other path
/// warns first and quotes the pattern.
#[derive(Clone, Copy)]
enum CompileErrors {
    Quoted,
    Wide,
    Regexec,
}

impl CompileErrors {
    unsafe fn for_text(pattern: &str, text: Sexp, use_bytes: bool) -> CompileErrors {
        let wide = !use_bytes
            && (!pattern.is_ascii()
                || (text.kind() == SexpType::String
                    && (0..length(text)).any(|index| !text.element(index).bytes().is_ascii())));
        if wide {
            CompileErrors::Wide
        } else {
            CompileErrors::Quoted
        }
    }
}

#[derive(Clone)]
enum Matcher {
    /// A fixed pattern, with its searcher built once rather than on every search.
    Literal(memmem::Finder<'static>),
    Pattern(Rc<Regex>),
    /// A POSIX pattern without backreferences, which finite automata can match in linear time.
    /// `regex` (leftmost-longest) still serves capture groups, but `fancy-regex` runs every
    /// leftmost-longest pattern of variable width on its backtracker, which takes exponential
    /// time on patterns such as `(.*z.*){10}` that TRE matches instantly. The leftmost match
    /// starts where the leftmost-first match does, and `longest` (which reports every match end)
    /// searched anchored at that start finds where the longest one ends.
    Automaton {
        regex: Rc<Regex>,
        first: meta::Regex,
        longest: meta::Regex,
    },
}

/// How many compiled patterns [`Matcher::compile`] keeps. R code calls `grepl` and `sub` with the
/// same few patterns over and over, and building the automaton costs far more than matching one
/// short string.
const PATTERN_CACHE_SIZE: usize = 256;

thread_local! {
    static PATTERN_CACHE: RefCell<FxHashMap<(String, bool, bool), Matcher>> = RefCell::default();
}

impl Matcher {
    unsafe fn compile(pattern: &str, options: PatternOptions, errors: CompileErrors) -> R<Matcher> {
        if options.fixed {
            return Ok(Matcher::Literal(memmem::Finder::new(pattern).into_owned()));
        }
        let key = (pattern.to_owned(), options.ignore_case, options.perl);
        if let Some(matcher) = PATTERN_CACHE.with_borrow(|cache| cache.get(&key).cloned()) {
            return Ok(matcher);
        }
        let syntax = if options.perl {
            Syntax::Perl
        } else {
            Syntax::Extended
        };
        let translated = translate(pattern, syntax);
        let build = |longest: bool| {
            if let Some(problem) = translated.problem {
                return Err(problem.to_owned());
            }
            let mut builder = RegexBuilder::new(&translated.pattern);
            builder.case_insensitive(options.ignore_case);
            // The step budget counts over a whole scan, so the default of a million fails a
            // plain `\s+` on a megabyte of text that TRE scans without complaint.
            builder.backtrack_limit(usize::MAX);
            if longest {
                builder.leftmost_longest(true);
            }
            builder.build().map_err(|failure| failure.to_string())
        };
        // POSIX picks the longest of the matches that start leftmost. The engine cannot do that
        // for every pattern (lazy quantifiers, for one), and those fall back to PCRE's rule.
        let (compiled, longest) = if options.perl {
            (build(false), false)
        } else {
            match build(true) {
                Ok(regex) => (Ok(regex), true),
                Err(_) => (build(false), false),
            }
        };
        let automaton = |kind: MatchKind| {
            meta::Regex::builder()
                .configure(meta::Regex::config().match_kind(kind))
                .syntax(syntax::Config::new().case_insensitive(options.ignore_case))
                .build(&translated.pattern)
        };
        let matcher = match compiled {
            Ok(regex) if longest => {
                let regex = Rc::new(regex);
                match (
                    automaton(MatchKind::LeftmostFirst),
                    automaton(MatchKind::All),
                ) {
                    (Ok(first), Ok(longest)) => Matcher::Automaton {
                        regex,
                        first,
                        longest,
                    },
                    _ => Matcher::Pattern(regex),
                }
            }
            Ok(regex) => Matcher::Pattern(Rc::new(regex)),
            Err(reason) => {
                if options.perl {
                    warning(format!("PCRE pattern compilation error\n\t'{reason}'"))?;
                    return Err(error(format!("invalid regular expression '{pattern}'")));
                }
                return Err(match errors {
                    CompileErrors::Quoted => {
                        warning(format!("TRE pattern compilation error '{reason}'"))?;
                        error(format!(
                            "invalid regular expression '{pattern}', reason '{reason}'"
                        ))
                    }
                    CompileErrors::Wide => {
                        error(format!("invalid regular expression, reason '{reason}'"))
                    }
                    CompileErrors::Regexec => error(format!("regcomp error: '{reason}'")),
                });
            }
        };
        PATTERN_CACHE.with_borrow_mut(|cache| {
            if cache.len() >= PATTERN_CACHE_SIZE {
                cache.clear();
            }
            cache.insert(key, matcher.clone());
        });
        Ok(matcher)
    }

    /// The first match at or after byte `from`, as a byte range.
    fn find_at(&self, text: &str, from: usize) -> R<Option<(usize, usize)>> {
        match self {
            Matcher::Literal(finder) => Ok(finder
                .find(&text.as_bytes()[from..])
                .map(|start| (from + start, from + start + finder.needle().len()))),
            Matcher::Pattern(regex) => regex
                .find_from_pos(text, from)
                .map(|found| found.map(|found| (found.start(), found.end())))
                .map_err(matching_error),
            Matcher::Automaton { first, longest, .. } => {
                let Some(found) = first.search(&Input::new(text).range(from..)) else {
                    return Ok(None);
                };
                let anchored = Input::new(text)
                    .range(found.start()..)
                    .anchored(Anchored::Yes);
                let end = longest
                    .search(&anchored)
                    .map_or(found.end(), |longest| longest.end());
                Ok(Some((found.start(), end)))
            }
        }
    }

    /// The first match at or after byte `from` with its capture groups; group 0 is the match.
    fn captures_at(&self, text: &str, from: usize) -> R<Option<Groups>> {
        match self {
            Matcher::Literal(_) => Ok(self.find_at(text, from)?.map(|span| vec![Some(span)])),
            Matcher::Automaton { regex, .. } if regex.captures_len() == 1 => {
                Ok(self.find_at(text, from)?.map(|span| vec![Some(span)]))
            }
            Matcher::Pattern(regex) | Matcher::Automaton { regex, .. } => {
                let Some(captures) = regex
                    .captures_from_pos(text, from)
                    .map_err(matching_error)?
                else {
                    return Ok(None);
                };
                Ok(Some(
                    (0..captures.len())
                        .map(|group| {
                            captures
                                .get(group)
                                .map(|found| (found.start(), found.end()))
                        })
                        .collect(),
                ))
            }
        }
    }

    /// The number of capture groups, not counting the whole match.
    fn group_count(&self) -> usize {
        match self {
            Matcher::Literal(_) => 0,
            Matcher::Pattern(regex) | Matcher::Automaton { regex, .. } => {
                regex.captures_len().saturating_sub(1)
            }
        }
    }

    /// The capture groups' names, with `""` for an unnamed group.
    fn group_names(&self) -> Vec<String> {
        match self {
            Matcher::Literal(_) => Vec::new(),
            Matcher::Pattern(regex) | Matcher::Automaton { regex, .. } => regex
                .capture_names()
                .skip(1)
                .map(|name| name.unwrap_or_default().to_owned())
                .collect(),
        }
    }
}

fn matching_error(failure: fancy_regex::Error) -> crate::unwind::Unwind {
    error(format!("regular expression matching failed: {failure}"))
}

/// A translated pattern, with the error TRE reports for a pattern it rejects but the engine
/// would accept.
pub(crate) struct Translation {
    pub(crate) pattern: String,
    pub(crate) problem: Option<&'static str>,
}

pub(crate) fn translate(pattern: &str, syntax: Syntax) -> Translation {
    let chars: Vec<char> = pattern.chars().collect();
    let dialect = Dialect {
        syntax,
        unicode: syntax == Syntax::Extended || declares_ucp(&chars),
    };
    let mut problem = None;
    let mut output = String::with_capacity(pattern.len() + 8);
    if syntax == Syntax::Extended {
        // In POSIX syntax `.` matches a newline too.
        output.push_str("(?s)");
    }
    // TRE runs a pattern with approximate-matching settings on its approximate matcher, which
    // ignores minimal repetition, so such a pattern's `*?` matches as `*` does.
    let approximate = syntax == Syntax::Extended
        && (0..chars.len()).any(|index| {
            (index == 0 || chars[index - 1] != '\\')
                && approximate_settings(&chars, index).is_some()
        });
    // Where each open group starts in `output`, and whether it is a lookaround assertion.
    let mut groups: Vec<(usize, bool)> = Vec::new();
    // Where the assertion that just closed starts, while a quantifier may still apply to it.
    let mut assertion: Option<usize> = None;
    // Where the latest atom starts in `output`, while a quantifier may still apply to it.
    let mut atom: Option<usize> = None;
    let mut quantified = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        let start = output.len();
        let quantifiable_assertion = assertion.take();
        match character {
            '\\' => {
                index = translate_escape(&chars, index, dialect, &mut output, &mut problem);
                atom = Some(start);
                quantified = false;
                continue;
            }
            '[' => {
                index = translate_class(&chars, index, dialect, &mut output, &mut problem);
                atom = Some(start);
                quantified = false;
                continue;
            }
            '(' if syntax == Syntax::Perl && chars.get(index + 1) == Some(&'?') => {
                let (next, opens_group) = translate_group_prefix(&chars, index, &mut output);
                if opens_group {
                    let lookaround = matches!(chars.get(index + 2), Some('=' | '!'))
                        || (chars.get(index + 2) == Some(&'<')
                            && matches!(chars.get(index + 3), Some('=' | '!')));
                    groups.push((start, lookaround));
                }
                atom = None;
                quantified = false;
                index = next;
                continue;
            }
            // TRE reads `(?:...)` as a group that does not capture, `(?i)` or `(?i:...)` as a
            // case-insensitive region and `(?#...)` as a comment, as PCRE does, and rejects
            // every other `(?` construct.
            '(' if syntax == Syntax::Extended && chars.get(index + 1) == Some(&'?') => {
                if chars.get(index + 2) == Some(&'#') {
                    index = (index..chars.len())
                        .find(|&position| chars[position] == ')')
                        .map_or(chars.len(), |close| close + 1);
                    continue;
                }
                let flags_end = (index + 2..chars.len())
                    .find(|&position| !matches!(chars[position], 'i' | '-'));
                match flags_end.map(|position| (position, chars[position])) {
                    Some((position, close @ (':' | ')'))) => {
                        output.extend(&chars[index..=position]);
                        if close == ':' {
                            groups.push((start, false));
                        }
                        atom = None;
                        quantified = false;
                        index = position + 1;
                        continue;
                    }
                    _ => {
                        problem.get_or_insert("Invalid regexp");
                        groups.push((start, false));
                        output.push('(');
                        atom = None;
                        quantified = false;
                    }
                }
            }
            '(' if syntax == Syntax::Perl && chars.get(index + 1) == Some(&'*') => {
                index = translate_verb(&chars, index, &mut output);
                continue;
            }
            '(' => {
                groups.push((start, false));
                output.push('(');
                atom = None;
                quantified = false;
            }
            ')' => {
                output.push(')');
                let (group_start, lookaround) = groups.pop().unwrap_or((start, false));
                atom = Some(group_start);
                assertion = lookaround.then_some(group_start);
                quantified = false;
            }
            // PCRE obeys a repeated assertion at most once, and one that may repeat zero times
            // can never fail the match, so it is dropped; fancy-regex refuses either.
            '*' | '+' | '?' if quantifiable_assertion.is_some() => {
                if character != '+'
                    && let Some(assertion_start) = quantifiable_assertion
                {
                    output.truncate(assertion_start);
                    atom = None;
                }
                if matches!(chars.get(index + 1), Some('?' | '+')) {
                    index += 1;
                }
            }
            '|' => {
                output.push('|');
                atom = None;
            }
            '*' | '+' | '?' => match atom {
                // POSIX takes a repetition operator with nothing to repeat literally.
                None if syntax == Syntax::Extended => {
                    output.push('\\');
                    output.push(character);
                    atom = Some(start);
                    quantified = false;
                }
                _ => {
                    output.push(character);
                    quantified = true;
                    let modifier = chars.get(index + 1).copied();
                    if approximate && modifier == Some('?') {
                        index += 1;
                    } else if modifier == Some('?')
                        || (syntax == Syntax::Perl && modifier == Some('+'))
                    {
                        output.extend(modifier);
                        index += 1;
                    }
                }
            },
            '{' if approximate && approximate_settings(&chars, index).is_some() => {
                // Settings without a `~` cost limit allow no errors, so the atom matches
                // exactly; a real cost limit needs an approximate matcher ry does not have.
                if let Some((next, exact)) = approximate_settings(&chars, index) {
                    if !exact {
                        problem.get_or_insert("approximate matching is not supported");
                    }
                    index = next;
                }
                continue;
            }
            '{' => match (parse_interval(&chars, index, syntax), atom) {
                (Some((interval, next)), Some(atom_start)) => {
                    if quantified {
                        // `x{2}{3}` repeats the repetition, which the `regex` crate only
                        // accepts with an explicit group.
                        output.insert_str(atom_start, "(?:");
                        output.push(')');
                    }
                    output.push_str(&interval);
                    quantified = true;
                    index = next;
                    if chars.get(index) == Some(&'?') {
                        if !approximate {
                            output.push('?');
                        }
                        index += 1;
                    }
                    continue;
                }
                (None, _) if syntax == Syntax::Extended => {
                    // TRE rejects a malformed interval, which the engine would read literally.
                    problem.get_or_insert(if chars[index..].contains(&'}') {
                        "Invalid contents of {}"
                    } else {
                        "Missing '}'"
                    });
                    output.push('{');
                }
                _ => {
                    output.push_str("\\{");
                    atom = Some(start);
                    quantified = false;
                }
            },
            '}' | ']' => {
                output.push('\\');
                output.push(character);
                atom = Some(start);
                quantified = false;
            }
            '^' | '$' => {
                output.push(character);
                atom = None;
            }
            _ => {
                output.push(character);
                atom = Some(start);
                quantified = false;
            }
        }
        index += 1;
    }
    Translation {
        pattern: output,
        problem,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// POSIX extended syntax as TRE reads it in a UTF-8 locale.
    Extended,
    Perl,
}

/// A syntax, and whether its classes (`\w`, `[[:alpha:]]`) are Unicode classes. TRE's always
/// are; PCRE's are ASCII, as R compiles it, unless the pattern starts with `(*UCP)`.
#[derive(Clone, Copy)]
struct Dialect {
    syntax: Syntax,
    unicode: bool,
}

/// Whether the pattern's leading PCRE options include `(*UCP)`.
fn declares_ucp(chars: &[char]) -> bool {
    let mut index = 0;
    while chars.get(index) == Some(&'(') && chars.get(index + 1) == Some(&'*') {
        let Some(close) = (index..chars.len()).find(|&cursor| chars[cursor] == ')') else {
            return false;
        };
        if chars[index + 2..close].iter().copied().eq("UCP".chars()) {
            return true;
        }
        index = close + 1;
    }
    false
}

/// What TRE's `\s` and `[[:space:]]` match: glibc's `iswspace`, which leaves out the
/// non-breaking spaces.
const POSIX_SPACE: &str =
    r"\t\n\x0B\f\r \x{1680}\x{2000}-\x{2006}\x{2008}-\x{200A}\x{2028}\x{2029}\x{205F}\x{3000}";
const POSIX_BLANK: &str = r"\t \x{1680}\x{2000}-\x{2006}\x{2008}-\x{200A}\x{205F}\x{3000}";
/// PCRE's ASCII classes.
const UNICODE_WORD: &str = r"\p{Alphabetic}\p{Nd}_";
const PERL_DIGIT: &str = "0-9";
const PERL_WORD: &str = "0-9A-Za-z_";
const PERL_SPACE: &str = r"\t\n\x0B\f\r ";
const PERL_HORIZONTAL_SPACE: &str =
    r"\t \xA0\x{1680}\x{180E}\x{2000}-\x{200A}\x{202F}\x{205F}\x{3000}";
const PERL_VERTICAL_SPACE: &str = r"\n\x0B\f\r\x{85}\x{2028}\x{2029}";

/// Translates the escape at `index` (outside a bracket expression) and returns the index after
/// it.
fn translate_escape(
    chars: &[char],
    index: usize,
    dialect: Dialect,
    output: &mut String,
    problem: &mut Option<&'static str>,
) -> usize {
    let syntax = dialect.syntax;
    let Some(&escaped) = chars.get(index + 1) else {
        if syntax == Syntax::Extended {
            problem.get_or_insert("Trailing backslash");
        }
        // The compiler rejects a trailing backslash too.
        output.push('\\');
        return index + 1;
    };
    let next = index + 2;
    if let Some(class) = escape_class(escaped, dialect) {
        output.push('[');
        output.push_str(&class);
        output.push(']');
        return next;
    }
    match (syntax, escaped) {
        // Without `(*UCP)` PCRE's word boundary looks at ASCII word characters only, and the
        // engine's own `\b` cannot be limited to ASCII.
        (Syntax::Perl, 'b') if !dialect.unicode => {
            output.push_str(&format!(
                "(?:(?<=[{PERL_WORD}])(?![{PERL_WORD}])|(?<![{PERL_WORD}])(?=[{PERL_WORD}]))"
            ));
            next
        }
        (Syntax::Perl, 'B') if !dialect.unicode => {
            output.push_str(&format!(
                "(?:(?<=[{PERL_WORD}])(?=[{PERL_WORD}])|(?<![{PERL_WORD}])(?![{PERL_WORD}]))"
            ));
            next
        }
        (_, 'b' | 'B' | 'n' | 't' | 'r' | 'f' | 'v' | '1'..='9')
        | (Syntax::Extended, '<' | '>') => {
            output.push('\\');
            output.push(escaped);
            next
        }
        (_, 'e') => {
            output.push_str(r"\x1B");
            next
        }
        (_, 'a') => {
            output.push_str(r"\x07");
            next
        }
        (_, 'x') => {
            let (point, end) = hex_escape(chars, next);
            output.push_str(&format!(r"\x{{{point:X}}}"));
            end
        }
        (Syntax::Perl, 'Q') => {
            let mut cursor = next;
            while cursor < chars.len() {
                if chars[cursor] == '\\' && chars.get(cursor + 1) == Some(&'E') {
                    return cursor + 2;
                }
                output.push_str(&regex::escape(chars[cursor].encode_utf8(&mut [0; 4])));
                cursor += 1;
            }
            cursor
        }
        (Syntax::Perl, 'E') => next,
        (Syntax::Perl, 'c') => match chars.get(next) {
            Some(&control) => {
                output.push_str(&format!(
                    r"\x{{{:X}}}",
                    (control.to_ascii_uppercase() as u32) ^ 0x40
                ));
                next + 1
            }
            None => {
                output.push_str(r"\c");
                next
            }
        },
        (Syntax::Perl, '0') => {
            let (point, end) = octal_escape(chars, next, 2);
            output.push_str(&format!(r"\x{{{point:X}}}"));
            end
        }
        (Syntax::Perl, 'g' | 'k') => translate_backreference(chars, index, output),
        (Syntax::Perl, 'p' | 'P' | 'N' | 'o') => copy_delimited_escape(chars, index, output),
        (Syntax::Perl, character) if character.is_ascii_alphanumeric() => {
            output.push('\\');
            output.push(character);
            next
        }
        (_, character) => {
            output.push_str(&regex::escape(character.encode_utf8(&mut [0; 4])));
            next
        }
    }
}

/// The class contents an escape such as `\d` stands for, written to sit inside brackets.
fn escape_class(escaped: char, dialect: Dialect) -> Option<String> {
    let contents = match (
        escaped.to_ascii_lowercase(),
        dialect.syntax,
        dialect.unicode,
    ) {
        // TRE's `\d` is ASCII even though its other classes follow the locale.
        ('d', Syntax::Extended, _) | ('d', Syntax::Perl, false) => PERL_DIGIT,
        ('d', Syntax::Perl, true) => r"\p{Nd}",
        ('w', _, true) => UNICODE_WORD,
        ('w', _, false) => PERL_WORD,
        ('s', Syntax::Extended, _) => POSIX_SPACE,
        ('s', Syntax::Perl, true) => r"\s",
        ('s', Syntax::Perl, false) => PERL_SPACE,
        ('h', Syntax::Perl, _) => PERL_HORIZONTAL_SPACE,
        ('v', Syntax::Perl, _) => PERL_VERTICAL_SPACE,
        _ => return None,
    };
    Some(if escaped.is_ascii_uppercase() {
        format!("[^{contents}]")
    } else {
        contents.to_owned()
    })
}

/// Rewrites PCRE's backreference forms (`\g1`, `\g-1`, `\g{name}`, `\k{name}`) as `\k<...>`,
/// because the engine reads `\g` as an Oniguruma subroutine call and knows no braced `\k`.
/// PCRE's own subroutine calls, `\g<name>` and `\g'name'`, are copied unchanged.
fn translate_backreference(chars: &[char], index: usize, output: &mut String) -> usize {
    let cursor = index + 2;
    let reference: String = match chars.get(cursor) {
        Some('{') => match chars[cursor + 1..]
            .iter()
            .position(|character| *character == '}')
        {
            Some(length) => chars[cursor + 1..cursor + 1 + length].iter().collect(),
            None => return copy_delimited_escape(chars, index, output),
        },
        Some(character)
            if chars[index + 1] == 'g' && (character.is_ascii_digit() || *character == '-') =>
        {
            let digits = chars[cursor + 1..]
                .iter()
                .take_while(|character| character.is_ascii_digit())
                .count();
            if *character == '-' && digits == 0 {
                return copy_delimited_escape(chars, index, output);
            }
            chars[cursor..cursor + 1 + digits].iter().collect()
        }
        _ => return copy_delimited_escape(chars, index, output),
    };
    output.push_str(&format!(r"\k<{reference}>"));
    match chars.get(cursor) {
        Some('{') => cursor + reference.chars().count() + 2,
        _ => cursor + reference.chars().count(),
    }
}

/// Copies an escape with a delimited argument (`\p{L}`, `\k<name>`, `\g{-1}`) or a short one
/// (`\pL`, `\g1`) unchanged.
fn copy_delimited_escape(chars: &[char], index: usize, output: &mut String) -> usize {
    output.push('\\');
    output.push(chars[index + 1]);
    let mut cursor = index + 2;
    let closer = match chars.get(cursor) {
        Some('{') => '}',
        Some('<') => '>',
        Some('\'') => '\'',
        Some(_) => {
            output.push(chars[cursor]);
            cursor += 1;
            while let Some(&digit) = chars
                .get(cursor)
                .filter(|character| character.is_ascii_digit())
            {
                output.push(digit);
                cursor += 1;
            }
            return cursor;
        }
        None => return cursor,
    };
    output.push(chars[cursor]);
    cursor += 1;
    while let Some(&character) = chars.get(cursor) {
        output.push(character);
        cursor += 1;
        if character == closer {
            break;
        }
    }
    cursor
}

/// Reads the code point of `\xhh` or `\x{hhhh}`, starting after the `x`.
fn hex_escape(chars: &[char], start: usize) -> (u32, usize) {
    if chars.get(start) == Some(&'{') {
        let mut value: u32 = 0;
        let mut cursor = start + 1;
        while let Some(digit) = chars
            .get(cursor)
            .and_then(|character| character.to_digit(16))
        {
            value = value.saturating_mul(16).saturating_add(digit);
            cursor += 1;
        }
        if chars.get(cursor) == Some(&'}') {
            cursor += 1;
        }
        return (value, cursor);
    }
    let mut value = 0;
    let mut cursor = start;
    while cursor < start + 2 {
        let Some(digit) = chars
            .get(cursor)
            .and_then(|character| character.to_digit(16))
        else {
            break;
        };
        value = value * 16 + digit;
        cursor += 1;
    }
    (value, cursor)
}

fn octal_escape(chars: &[char], start: usize, limit: usize) -> (u32, usize) {
    let mut value = 0;
    let mut cursor = start;
    while cursor < start + limit {
        let Some(digit) = chars
            .get(cursor)
            .and_then(|character| character.to_digit(8))
        else {
            break;
        };
        value = value * 8 + digit;
        cursor += 1;
    }
    (value, cursor)
}

/// Translates a bracket expression starting at `index` and returns the index after it. An
/// unterminated one is left unterminated, so that compiling it fails.
fn translate_class(
    chars: &[char],
    index: usize,
    dialect: Dialect,
    output: &mut String,
    problem: &mut Option<&'static str>,
) -> usize {
    let mut cursor = index + 1;
    output.push('[');
    if chars.get(cursor) == Some(&'^') {
        output.push('^');
        cursor += 1;
    }
    let mut first = true;
    while let Some(&character) = chars.get(cursor) {
        if character == ']' && !first {
            output.push(']');
            return cursor + 1;
        }
        first = false;
        let (item, next) = class_item(chars, cursor, dialect, problem);
        let range_end = match item {
            ClassItem::Literal(_) if chars.get(next) == Some(&'-') => chars
                .get(next + 1)
                .filter(|&&end| end != ']')
                .map(|_| class_item(chars, next + 1, dialect, problem)),
            _ => None,
        };
        match (item, range_end) {
            (ClassItem::Literal(low), Some((ClassItem::Literal(high), after))) => {
                push_class_literal(output, low);
                output.push('-');
                push_class_literal(output, high);
                cursor = after;
            }
            (ClassItem::Literal(literal), _) => {
                push_class_literal(output, literal);
                cursor = next;
            }
            (ClassItem::Set(set), _) => {
                output.push_str(&set);
                cursor = next;
            }
        }
    }
    problem.get_or_insert(match dialect.syntax {
        Syntax::Extended => "Missing ']'",
        Syntax::Perl => "missing terminating ] for character class",
    });
    cursor
}

enum ClassItem {
    Literal(char),
    /// Class contents to splice in, such as `0-9` or `\p{Alphabetic}`.
    Set(String),
}

fn class_item(
    chars: &[char],
    index: usize,
    dialect: Dialect,
    problem: &mut Option<&'static str>,
) -> (ClassItem, usize) {
    let syntax = dialect.syntax;
    let character = chars[index];
    if character == '['
        && let Some((kind, name, end)) = bracket_term(chars, index)
    {
        let set = match (kind, posix_class(&name, dialect)) {
            (':', Some(set)) => set,
            (':', None) => {
                problem.get_or_insert(match syntax {
                    Syntax::Extended => "Unknown character class name",
                    Syntax::Perl => "unknown POSIX class name",
                });
                String::new()
            }
            // Neither R engine supports equivalence classes or collating symbols.
            _ => {
                problem.get_or_insert(match syntax {
                    Syntax::Extended => "Unknown collating element",
                    Syntax::Perl => "POSIX collating elements are not supported",
                });
                String::new()
            }
        };
        return (ClassItem::Set(set), end);
    }
    if character != '\\' || syntax == Syntax::Extended {
        // In POSIX brackets a backslash is an ordinary character.
        return (ClassItem::Literal(character), index + 1);
    }
    let Some(&escaped) = chars.get(index + 1) else {
        return (ClassItem::Literal('\\'), index + 1);
    };
    let next = index + 2;
    if let Some(class) = escape_class(escaped, dialect) {
        return (ClassItem::Set(class), next);
    }
    match escaped {
        'n' => (ClassItem::Literal('\n'), next),
        't' => (ClassItem::Literal('\t'), next),
        'r' => (ClassItem::Literal('\r'), next),
        'f' => (ClassItem::Literal('\x0C'), next),
        'v' => (ClassItem::Literal('\x0B'), next),
        'a' => (ClassItem::Literal('\x07'), next),
        'e' => (ClassItem::Literal('\x1B'), next),
        'b' => (ClassItem::Literal('\x08'), next),
        'x' => {
            let (point, end) = hex_escape(chars, next);
            (
                ClassItem::Literal(char::from_u32(point).unwrap_or('\u{FFFD}')),
                end,
            )
        }
        '0' => {
            let (point, end) = octal_escape(chars, next, 2);
            (
                ClassItem::Literal(char::from_u32(point).unwrap_or('\0')),
                end,
            )
        }
        'p' | 'P' => {
            let mut copied = String::new();
            let end = copy_delimited_escape(chars, index, &mut copied);
            (ClassItem::Set(copied), end)
        }
        other => (ClassItem::Literal(other), next),
    }
}

/// Reads `[:name:]`, `[=c=]` or `[.c.]` at `index`: the delimiter, the name, and the index
/// after the term.
fn bracket_term(chars: &[char], index: usize) -> Option<(char, String, usize)> {
    let kind = *chars.get(index + 1)?;
    if !matches!(kind, ':' | '=' | '.') {
        return None;
    }
    let mut cursor = index + 2;
    let mut name = String::new();
    while cursor + 1 < chars.len() {
        if chars[cursor] == kind && chars[cursor + 1] == ']' {
            return Some((kind, name, cursor + 2));
        }
        name.push(chars[cursor]);
        cursor += 1;
    }
    None
}

/// The class contents for `[:name:]`, or `None` for an unknown name. In a Unicode dialect the
/// classes follow the Unicode properties, as glibc's do in a UTF-8 locale; otherwise they are
/// ASCII, which the `regex` crate's own `[:name:]` classes match.
fn posix_class(name: &str, dialect: Dialect) -> Option<String> {
    const NAMES: &[&str] = &[
        "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
        "upper", "xdigit", "word",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    if !dialect.unicode {
        return Some(format!("[:{name}:]"));
    }
    Some(
        match name {
            "alpha" => r"\p{Alphabetic}",
            "digit" => "0-9",
            "alnum" => r"\p{Alphabetic}\p{Nd}",
            "upper" => r"\p{Uppercase}",
            "lower" => r"\p{Lowercase}",
            "space" => POSIX_SPACE,
            "blank" => POSIX_BLANK,
            "punct" => r"\p{P}\p{S}",
            "cntrl" => r"\p{Cc}",
            "print" => r"\P{C}",
            "graph" => r"[^\p{C}\p{Z}]",
            "xdigit" => "0-9A-Fa-f",
            _ => UNICODE_WORD,
        }
        .to_owned(),
    )
}

fn push_class_literal(output: &mut String, character: char) {
    if matches!(character, '[' | ']' | '\\' | '-' | '^' | '&' | '~') {
        output.push('\\');
    }
    output.push(character);
}

/// Translates a PCRE group opener `(?...` at `index`. Returns the index after it and whether it
/// opens a group (a comment or a bare flag setting does not).
fn translate_group_prefix(chars: &[char], index: usize, output: &mut String) -> (usize, bool) {
    let after = index + 2;
    let read_name = |start: usize, closer: char| -> Option<(String, usize)> {
        let end = (start..chars.len()).find(|&cursor| chars[cursor] == closer)?;
        Some((chars[start..end].iter().collect(), end + 1))
    };
    match chars.get(after).copied() {
        Some('#') => {
            let end = (after..chars.len())
                .find(|&cursor| chars[cursor] == ')')
                .map_or(chars.len(), |end| end + 1);
            (end, false)
        }
        Some(':' | '=' | '!' | '>' | '|') => {
            output.push_str("(?");
            output.push(chars[after]);
            (after + 1, true)
        }
        Some('<') if matches!(chars.get(after + 1), Some('=' | '!')) => {
            output.push_str("(?<");
            output.push(chars[after + 1]);
            (after + 2, true)
        }
        Some(opener @ ('<' | '\'' | 'P'))
            if opener != 'P' || chars.get(after + 1) == Some(&'<') =>
        {
            let (start, closer) = match opener {
                '<' => (after + 1, '>'),
                '\'' => (after + 1, '\''),
                _ => (after + 2, '>'),
            };
            match read_name(start, closer) {
                Some((name, end)) => {
                    output.push_str(&format!("(?P<{name}>"));
                    (end, true)
                }
                None => {
                    output.push_str("(?");
                    (after, true)
                }
            }
        }
        _ => {
            let mut cursor = after;
            while chars.get(cursor).is_some_and(|character| {
                character.is_ascii_alphabetic() || *character == '-' || *character == '^'
            }) {
                cursor += 1;
            }
            let flags: String = chars[after..cursor].iter().collect();
            match chars.get(cursor) {
                Some(':') => {
                    output.push_str(&format!("(?{flags}:"));
                    (cursor + 1, true)
                }
                Some(')') => {
                    output.push_str(&format!("(?{flags})"));
                    (cursor + 1, false)
                }
                _ => {
                    // `(?P=name)`, `(?R)`, `(?1)` and the like: passed through for the engine.
                    output.push_str("(?");
                    (after, true)
                }
            }
        }
    }
}

/// Drops a PCRE start-of-pattern option such as `(*UCP)` that the translation already accounts
/// for, and passes any other verb through for the engine to accept or reject.
fn translate_verb(chars: &[char], index: usize, output: &mut String) -> usize {
    let close = (index..chars.len()).find(|&cursor| chars[cursor] == ')');
    if let Some(close) = close {
        let verb: String = chars[index + 2..close].iter().collect();
        if matches!(
            verb.as_str(),
            "UTF"
                | "UTF8"
                | "UCP"
                | "CR"
                | "LF"
                | "CRLF"
                | "ANYCRLF"
                | "ANY"
                | "NO_START_OPT"
                | "NO_AUTO_POSSESS"
                | "NO_JIT"
        ) {
            return close + 1;
        }
    }
    output.push_str("(*");
    index + 2
}

/// Reads an interval `{m}`, `{m,}` or `{m,n}` at `index` into the `regex` crate's form, with
/// POSIX's `{,n}` written as `{0,n}`. Returns `None` when the brace does not start an interval.
/// TRE's approximate-matching settings at a `{`, such as `{+}`, `{-2}` or `{~1,+2}`: each
/// item is one of `+`, `-`, `#` or `~` with an optional count. Returns the index after the
/// closing brace and whether the settings allow no errors (no `~` limit, or `~0`).
fn approximate_settings(chars: &[char], index: usize) -> Option<(usize, bool)> {
    if chars.get(index) != Some(&'{') {
        return None;
    }
    let mut cursor = index + 1;
    let mut exact = true;
    let mut items = 0;
    loop {
        match chars.get(cursor)? {
            '}' if items > 0 => return Some((cursor + 1, exact)),
            ',' | ' ' if items > 0 => cursor += 1,
            &operator @ ('+' | '-' | '#' | '~') => {
                cursor += 1;
                let count = read_number(chars, &mut cursor);
                if operator == '~' && count != Some(0) {
                    exact = false;
                }
                items += 1;
            }
            _ => return None,
        }
    }
}

fn parse_interval(chars: &[char], index: usize, syntax: Syntax) -> Option<(String, usize)> {
    let mut cursor = index + 1;
    let minimum = read_number(chars, &mut cursor);
    let mut maximum = None;
    let comma = chars.get(cursor) == Some(&',');
    if comma {
        cursor += 1;
        maximum = read_number(chars, &mut cursor);
    }
    if chars.get(cursor) != Some(&'}') {
        return None;
    }
    if syntax == Syntax::Extended
        && let (Some(minimum), Some(maximum)) = (minimum, maximum)
        && minimum > maximum
    {
        return None;
    }
    let text = match (minimum, comma, maximum) {
        (Some(minimum), false, _) => format!("{{{minimum}}}"),
        (Some(minimum), true, None) => format!("{{{minimum},}}"),
        (Some(minimum), true, Some(maximum)) => format!("{{{minimum},{maximum}}}"),
        (None, true, Some(maximum)) if syntax == Syntax::Extended => format!("{{0,{maximum}}}"),
        _ => return None,
    };
    Some((text, cursor + 1))
}

#[inline]
pub(crate) unsafe fn flag(value: Sexp) -> bool {
    values::as_logical_scalar(value) == 1
}

/// `as.character(value)`, dispatching for objects, as `paste` and `sprintf` convert their
/// arguments.
unsafe fn as_character(value: Sexp, env: Sexp) -> R<Sexp> {
    if value.is_object() {
        let function = values::install("as.character")?;
        let call = values::call(function, &[eval::quote_value(value)?])?;
        let converted = eval::eval(call, env)?;
        if converted.kind() != SexpType::String {
            return coerce_vector(converted, SexpType::String);
        }
        return Ok(converted);
    }
    match value.kind() {
        SexpType::String => Ok(value),
        SexpType::Symbol => values::scalar_string_charsxp(value.car()),
        _ => coerce_vector(value, SexpType::String),
    }
}

/// A character version of `x`, as `nchar` and `nzchar` read their argument.
unsafe fn character_vector(x: Sexp) -> R<Sexp> {
    if x.kind() == SexpType::String {
        Ok(x)
    } else {
        coerce_vector(x, SexpType::String)
    }
}

/// Copies the names, dim and dimnames of `from` onto `to`.
unsafe fn copy_shape(from: Sexp, to: Sexp) -> R<()> {
    let symbols = symbols();
    for symbol in [symbols.names, symbols.dim, symbols.dimnames] {
        let value = get_attr(from, symbol);
        if value != nil() {
            set_attr(to, symbol, value)?;
        }
    }
    Ok(())
}

unsafe fn integer_argument(value: Sexp) -> R<Vec<i32>> {
    let integers = coerce_vector(value, SexpType::Integer)?;
    Ok(values::ints(integers).to_vec())
}

/// Maps every non-`NA` element of a character vector through `edit` (`None` for `NA`), keeping
/// the attributes.
unsafe fn map_strings(x: Sexp, edit: impl Fn(&str) -> Option<String>) -> R<Sexp> {
    let count = length(x);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let charsxp = x.element(index);
        if charsxp == na_string() {
            set_elt(result, index, charsxp);
            continue;
        }
        let text = values::char_str(charsxp);
        match edit(&text) {
            Some(edited) if edited == *text => set_elt(result, index, charsxp),
            Some(edited) => set_elt(result, index, values::mkchar(&edited)?),
            None => set_elt(result, index, na_string()),
        }
    }
    values::copy_attributes(x, result)?;
    Ok(result)
}

/// The byte index after the character at `index`, or `index + 1` at the end of the text.
fn next_boundary(text: &str, index: usize) -> usize {
    text.get(index..)
        .and_then(|rest| rest.chars().next())
        .map_or(index + 1, |character| index + character.len_utf8())
}
