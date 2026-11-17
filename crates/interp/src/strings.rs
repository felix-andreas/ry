//! Character vectors: `paste`, `nchar`, `substr`, regular expressions, `sprintf` and friends.
//!
//! Text is processed as UTF-8. Positions that R reports, such as `regexpr`'s, count characters
//! unless `useBytes = TRUE` asks for bytes.
//!
//! `perl = TRUE` patterns run on PCRE2, compiled as GNU R compiles them. POSIX extended patterns
//! (R's default, which GNU R runs on TRE) run on `fancy-regex` and `regex-automata` after a
//! translation into their syntax ([`translate`]), because TRE disagrees with them on
//! bracket expressions, on what `\w` and `[[:alpha:]]` match, and on which characters are special.

use {
    crate::{
        approximate::{
            ApproximateMatch, ApproximateRegex, ApproximateSettings, SCOPE_GROUP, UNBOUNDED,
        },
        builtins::{BuiltinDef, Call, builtin, internal},
        coerce::{coerce_vector, integer_digits},
        conditions::warning,
        eval,
        format::{display_width, push_fixed, push_general, push_scientific},
        handles::{
            self, Args, Chars, Cx, Fresh, Gc, Integer, List, Local, Logical, Raw, Str, StringFill,
            safe,
        },
        pcre::{CompileError, PerlRegex},
        unwind::{R, Unwind, error},
        values::{
            self, alloc, get_attr, length, na_string, nil, set_attr, set_elt, string_at, strings,
            symbols,
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
    smallvec::{SmallVec, smallvec},
    std::{
        borrow::Cow,
        cell::{OnceCell, RefCell},
        fmt::Write,
        rc::Rc,
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("paste", 4, do_paste),
    internal("paste0", 3, do_paste0),
    internal("nchar", 4, safe!(do_nchar)),
    builtin("nzchar", -1, safe!(do_nzchar)),
    internal("substr", 3, safe!(do_substr)),
    internal("substr<-", 4, safe!(do_set_substr)),
    internal("strsplit", 5, do_strsplit),
    internal("sprintf", -1, do_sprintf),
    internal("toupper", 1, safe!(do_toupper)),
    internal("tolower", 1, safe!(do_tolower)),
    internal("chartr", 3, safe!(do_chartr)),
    internal("strtrim", 2, safe!(do_strtrim)),
    internal("startsWith", 2, safe!(do_starts_with)),
    internal("endsWith", 2, safe!(do_ends_with)),
    internal("strrep", 2, safe!(do_strrep)),
    internal("strtoi", 2, safe!(do_strtoi)),
    internal("make.names", 2, safe!(do_make_names)),
    internal("make.unique", 2, do_make_unique),
    internal("abbreviate", 3, safe!(do_abbreviate)),
    internal("utf8ToInt", 1, safe!(do_utf8_to_int)),
    internal("intToUtf8", 3, safe!(do_int_to_utf8)),
    internal("charToRaw", 1, safe!(do_char_to_raw)),
    internal("rawToChar", 2, safe!(do_raw_to_char)),
    internal("pmatch", 4, safe!(do_pmatch)),
    internal("charmatch", 3, safe!(do_charmatch)),
    internal("grepl", 8, safe!(do_grepl)),
    internal("grep", 8, safe!(do_grep)),
    internal("sub", 7, safe!(do_sub)),
    internal("gsub", 7, safe!(do_gsub)),
    internal("regexpr", 6, safe!(do_regexpr)),
    internal("grepRaw", 8, safe!(do_grep_raw)),
    internal("gregexpr", 6, safe!(do_gregexpr)),
    internal("regexec", 5, safe!(do_regexec)),
];

/// The pattern `list.files` filters names by: an extended regular expression, or the literal
/// text when `fixed`. It is compiled as `grepl` compiles its pattern and kept the same way,
/// because a session lists directories with the same pattern over and over.
pub(crate) struct NamePattern(Matcher);

impl NamePattern {
    pub(crate) fn compile(pattern: &str, ignore_case: bool, fixed: bool) -> R<NamePattern> {
        // A pattern compiled for wide text fails without a warning.
        let invalid = |_: CompileFailure| error("invalid 'pattern' regular expression");
        let compiled = if fixed && ignore_case {
            // A literal has no case-insensitive search, so the escaped text is a pattern.
            let options = PatternOptions {
                ignore_case,
                perl: false,
                fixed: false,
            };
            Matcher::compile(&regex::escape(pattern), options, CompileErrors::Wide)
        } else {
            let options = PatternOptions {
                ignore_case,
                perl: false,
                fixed,
            };
            Matcher::compile(pattern, options, CompileErrors::Wide)
        };
        compiled.map(NamePattern).map_err(invalid)
    }

    pub(crate) fn matches(&self, name: &str) -> R<bool> {
        Ok(self.0.find_at(name, 0)?.is_some())
    }
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
    // The converted columns go into a separate list rather than back into `list`, as GNU R's
    // `do_paste` does, because `list` can be the caller's own: `.Internal(paste(args, ...))`
    // would see its non-character elements replaced.
    let mut columns: SmallVec<[(Sexp, usize); 8]> = SmallVec::with_capacity(length(list));
    let mut count = 0;
    let mut any_empty = false;
    for index in 0..length(list) {
        // Every element of a column is read, so a deferred column converts in one batch.
        let column = values::materialize(as_character(list.element(index), || call.env())?)?;
        let column_length = length(column);
        count = count.max(column_length);
        any_empty |= column_length == 0;
        columns.push((column, column_length));
    }
    if any_empty && flag(recycle_zero) {
        count = 0;
    }
    let Some(collapse) = collapse else {
        let result = alloc(SexpType::String, count)?;
        with_byte_buffer(|bytes| {
            for row in 0..count {
                bytes.clear();
                let encoding = join_row(bytes, &columns, separator, row);
                values::set_mkchar_bytes(result, row, bytes, encoding)?;
            }
            Ok(())
        })?;
        return Ok(result);
    };
    // With `collapse`, each row's bytes go straight into the one result instead of becoming a
    // string of their own, and the buffer is sized up front, so the peak is the joined text
    // and its copy in the result. The bytes are those the row's string would hold, and the
    // result's encoding follows from what each row's string would be marked: ASCII text is
    // marked ASCII whatever its pieces were.
    let mut estimate = 0;
    for row in 0..count {
        row_pieces(&columns, separator, row, |charsxp| {
            estimate += charsxp.bytes().len();
        });
    }
    let collapse_bytes = collapse.bytes();
    estimate += count.saturating_sub(1) * collapse_bytes.len();
    let collapsed = alloc(SexpType::String, 1)?;
    with_byte_buffer(|bytes| {
        bytes.reserve(estimate);
        let mut encoding = if count > 1 {
            piece_encoding(Encoding::Native, collapse)
        } else {
            Encoding::Native
        };
        let collapse_latin1 = collapse.has_gp(gp::LATIN1);
        let mut latin1_collapses = Vec::new();
        for row in 0..count {
            if row > 0 {
                if collapse_latin1 {
                    // Whether the collapse is translated depends on the result's encoding, which is
                    // known only at the end.
                    latin1_collapses.push(bytes.len());
                }
                bytes.extend_from_slice(collapse_bytes);
            }
            let start = bytes.len();
            match join_row(bytes, &columns, separator, row) {
                Encoding::Bytes => encoding = Encoding::Bytes,
                Encoding::Utf8 if encoding == Encoding::Native && !bytes[start..].is_ascii() => {
                    encoding = Encoding::Utf8;
                }
                _ => {}
            }
        }
        if !latin1_collapses.is_empty() && encoding != Encoding::Bytes {
            let translated = values::char_str(collapse);
            let mut joined = Vec::with_capacity(bytes.len());
            let mut from = 0;
            for &at in &latin1_collapses {
                joined.extend_from_slice(&bytes[from..at]);
                joined.extend_from_slice(translated.as_bytes());
                from = at + collapse_bytes.len();
            }
            joined.extend_from_slice(&bytes[from..]);
            *bytes = joined;
        }
        values::set_mkchar_bytes(collapsed, 0, bytes, encoding)
    })?;
    Ok(collapsed)
}

/// Appends one row of `paste` and returns its encoding, decided from all pieces before any is
/// joined, as in GNU R: a bytes piece keeps latin1 pieces untranslated. `NA` needs no case of its
/// own, because `NA_STRING` holds the bytes "NA" and no encoding mark.
unsafe fn join_row(
    bytes: &mut Vec<u8>,
    columns: &[(Sexp, usize)],
    separator: Option<Sexp>,
    row: usize,
) -> Encoding {
    let mut encoding = Encoding::Native;
    row_pieces(columns, separator, row, |charsxp| {
        encoding = piece_encoding(encoding, charsxp)
    });
    row_pieces(columns, separator, row, |charsxp| {
        if charsxp.has_gp(gp::LATIN1) && encoding != Encoding::Bytes {
            bytes.extend_from_slice(values::char_str(charsxp).as_bytes());
        } else {
            bytes.extend_from_slice(charsxp.bytes());
        }
    });
    encoding
}

/// Visits one row's pieces in order, with the separator between columns, without collecting them.
unsafe fn row_pieces(
    columns: &[(Sexp, usize)],
    separator: Option<Sexp>,
    row: usize,
    mut visit: impl FnMut(Sexp),
) {
    for (position, &(column, column_length)) in columns.iter().enumerate() {
        if position > 0
            && let Some(separator) = separator
        {
            visit(separator);
        }
        if row < column_length {
            visit(column.element(row));
        } else if column_length > 0 {
            visit(column.element(row % column_length));
        }
    }
}

/// The encoding a `paste` result has so far, after one more piece: bytes wins over everything,
/// and UTF-8 or latin1 makes the result UTF-8.
unsafe fn piece_encoding(encoding: Encoding, charsxp: Sexp) -> Encoding {
    match encoding {
        Encoding::Bytes => encoding,
        _ if charsxp.has_gp(gp::BYTES) => Encoding::Bytes,
        _ if charsxp.has_gp(gp::LATIN1 | gp::UTF8) => Encoding::Utf8,
        _ => encoding,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CountType {
    Bytes,
    Chars,
    Width,
}

fn do_nchar<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let x = arguments.value(0);
    if handles::inherits(context, x, "factor") {
        return Err(error("'nchar()' requires a character vector"));
    }
    let count_type = match handles::first_string(context, arguments.value(1)).as_deref() {
        Some(name) if !name.is_empty() && "bytes".starts_with(name) => CountType::Bytes,
        Some(name) if !name.is_empty() && "chars".starts_with(name) => CountType::Chars,
        Some(name) if !name.is_empty() && "width".starts_with(name) => CountType::Width,
        _ => return Err(error("invalid 'type' argument")),
    };
    let keep_na = handles::logical_scalar(context, arguments.value(3));
    let na_count = match (keep_na, count_type) {
        (1, _) | (NA_LOGICAL, CountType::Bytes | CountType::Chars) => NA_INTEGER,
        _ => 2,
    };
    let allow_na = handles::logical_scalar(context, arguments.value(2)) == 1;
    let text = handles::character_vector(context, x)?;
    let mut result = context.alloc::<Integer>(text.len())?;
    let counts = result.as_mut_slice().iter_mut();
    for (index, (slot, chars)) in counts.zip(text.iter(context)).enumerate() {
        if count_type != CountType::Bytes && handles::is_bytes(chars) && !chars.bytes().is_ascii() {
            if !allow_na {
                let what = if count_type == CountType::Chars {
                    "number of characters"
                } else {
                    "width"
                };
                return Err(error(format!(
                    "{what} is not computable in \"bytes\" encoding, element {}",
                    index + 1
                )));
            }
            *slot = NA_INTEGER;
            continue;
        }
        let invalid = count_type != CountType::Bytes
            && !handles::is_latin1(chars)
            && std::str::from_utf8(chars.bytes()).is_err();
        *slot = if handles::is_na(chars) {
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
                CountType::Bytes => chars.bytes().len() as i32,
                CountType::Chars => handles::text(chars).chars().count() as i32,
                CountType::Width => display_width(&handles::text(chars)) as i32,
            }
        };
    }
    handles::copy_shape(context, x, &mut result)?;
    Ok(result.finish().value())
}

fn do_nzchar<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let matched = arguments.matched_as_passed(context, ["x", "keepNA"])?;
    // The empty symbol, as `lapply` hands over an empty argument taken from a call, evaluates to
    // `R_MissingArg`, which GNU R coerces to `""` rather than reporting a missing argument.
    if matched[0].is_some_and(handles::is_missing_argument) {
        return handles::logical_constant(context, 0);
    }
    let Some(x) = matched[0] else {
        return Err(error("argument \"x\" is missing, with no default"));
    };
    let keep_na = matched[1].is_some_and(|value| is_true(context, value));
    if handles::inherits(context, x, "factor") {
        return Err(error("'nzchar()' requires a character vector"));
    }
    let text = handles::character_vector(context, x)?;
    let mut result = context.alloc::<Logical>(text.len())?;
    for (slot, chars) in result.as_mut_slice().iter_mut().zip(text.iter(context)) {
        *slot = if handles::is_na(chars) {
            if keep_na { NA_LOGICAL } else { 1 }
        } else {
            (!chars.bytes().is_empty()) as i32
        };
    }
    Ok(result.finish().value())
}

fn do_substr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let x = arguments.value(0);
    let Some(text) = handles::dense::<Str>(context, x)? else {
        return Err(error("extracting substrings from a non-character object"));
    };
    let count = text.len();
    let mut result = StringFill::new(context, count)?;
    if count > 0 {
        let starts = handles::integer_vector(context, arguments.value(1))?;
        // From R 4.5 on, `substr(x, start, NULL)` and `substring(x, first)` pass a NULL stop,
        // which means the end of each string.
        let stops = if arguments.value(2).r_type() == SexpType::Nil {
            None
        } else {
            Some(handles::integer_vector(context, arguments.value(2))?)
        };
        if starts.is_empty() || stops.is_some_and(|stops| stops.is_empty()) {
            return Err(error("invalid substring arguments"));
        }
        let (start_count, stop_count) = (starts.len(), stops.map_or(1, |stops| stops.len()));
        let mut buffer = String::new();
        // Recycling by counters rather than `index % len` keeps two divisions out of every
        // element, which on short strings cost as much as the extraction.
        let (mut start_at, mut stop_at) = (0, 0);
        for index in 0..count {
            let start = starts.as_slice(context)[start_at];
            let stop = stops.map_or(i32::MAX, |stops| stops.as_slice(context)[stop_at]);
            start_at = if start_at + 1 == start_count {
                0
            } else {
                start_at + 1
            };
            stop_at = if stop_at + 1 == stop_count {
                0
            } else {
                stop_at + 1
            };
            let chars = text.get(context, index);
            if handles::is_na(chars) || start == NA_INTEGER || stop == NA_INTEGER {
                result.push(context, handles::na());
                continue;
            }
            // The intern table marks ASCII strings, whose characters are bytes, so the span
            // needs no decoding; neither does a string of bytes.
            let ascii = chars.has_mark(gp::ASCII);
            if ascii || handles::encoding(chars) == Encoding::Bytes {
                let length = chars.bytes().len();
                let first = (start.max(1) as usize - 1).min(length);
                let last = (stop.max(0) as usize).min(length).max(first);
                if ascii && first == 0 && last == length {
                    result.push(context, chars);
                } else {
                    let encoding = if ascii {
                        Encoding::Native
                    } else {
                        Encoding::Bytes
                    };
                    result.push_part(context, text, index, first..last, encoding)?;
                }
                continue;
            }
            // The span is copied out because making its string allocates, which the text,
            // borrowed from the heap, cannot outlive.
            let whole = {
                let decoded = handles::text(chars);
                let (first, last) = char_span(&decoded, start, stop);
                let whole = first == 0 && last == decoded.len();
                if !whole {
                    buffer.clear();
                    buffer.push_str(&decoded[first..last]);
                }
                whole
            };
            if whole {
                result.push(context, chars);
            } else {
                result.push_text(context, &buffer)?;
            }
        }
    }
    let mut result = result.finish();
    handles::copy_attributes(context, x, &mut result)?;
    Ok(result.finish().value())
}

fn do_set_substr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(text) = handles::dense::<Str>(context, arguments.value(0))? else {
        return Err(error("replacing substrings in a non-character object"));
    };
    let mut result = handles::shallow_duplicate(context, text)?;
    if text.is_empty() {
        return Ok(result.finish().value());
    }
    let starts = handles::integer_vector(context, arguments.value(1))?;
    // From R 4.5 on, `substring(x, first) <- value` passes a NULL stop, which means the end of
    // each string.
    let stops = if arguments.value(2).r_type() == SexpType::Nil {
        None
    } else {
        Some(handles::integer_vector(context, arguments.value(2))?)
    };
    if starts.is_empty() || stops.is_some_and(|stops| stops.is_empty()) {
        return Err(error("invalid substring arguments"));
    }
    let value = match handles::dense::<Str>(context, arguments.value(3))? {
        Some(value) if !value.is_empty() => value,
        _ => return Err(error("invalid value")),
    };
    for index in 0..text.len() {
        let start = starts.as_slice(context)[index % starts.len()];
        let stop = stops.map_or(i32::MAX, |stops| {
            stops.as_slice(context)[index % stops.len()]
        });
        let chars = text.get(context, index);
        let replacement = value.get(context, index % value.len());
        if handles::is_na(chars)
            || handles::is_na(replacement)
            || start == NA_INTEGER
            || stop == NA_INTEGER
        {
            result.set(context, index, handles::na());
            continue;
        }
        let current = handles::text(chars);
        let replacement = handles::text(replacement);
        let length_in_chars = current.chars().count() as i64;
        let first = i64::from(start).max(1);
        let last = i64::from(stop)
            .min(length_in_chars)
            .min(first + replacement.chars().count() as i64 - 1);
        if first > last {
            continue;
        }
        let mut edited = String::with_capacity(current.len());
        let mut replacement_chars = replacement.chars();
        for (position, character) in (1..).zip(current.chars()) {
            if (first..=last).contains(&position) {
                edited.extend(replacement_chars.next());
            } else {
                edited.push(character);
            }
        }
        handles::set_text(context, &mut result, index, &edited)?;
    }
    Ok(result.finish().value())
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
    let (perl, fixed) = (flag(call.arg(3)), flag(call.arg(2)));
    for message in overridden_flags(false, perl, fixed) {
        warning(message)?;
    }
    let options = effective_options(false, perl, fixed);
    let forced_bytes = [x, split].iter().any(|&vector| {
        vector.kind() == SexpType::String
            && (0..length(vector))
                .any(|index| Encoding::of(vector.element(index)) == Encoding::Bytes)
    });
    let use_bytes = flag(call.arg(4)) || forced_bytes;
    let splits: SmallVec<[Option<Cow<str>>; 1]> = if split.kind() == SexpType::String {
        (0..length(split))
            .map(|index| subject_at(split, index, use_bytes))
            .collect()
    } else {
        SmallVec::new()
    };
    let mut matchers: SmallVec<[Option<Matcher>; 1]> = (0..splits.len()).map(|_| None).collect();
    let count = length(x);
    let result = alloc(SexpType::List, count)?;
    let mut pieces = Pieces::new();
    // Pieces repeat (a code or a suffix of each id), so ASCII ones go through the call's memo.
    let mut memo = values::StringMemo::default();
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
            Some(Some(separator)) if let Some(byte) = literal_byte(separator, options) => {
                split_byte(byte, &text, &mut pieces)
            }
            Some(Some(separator)) => {
                if matchers[position].is_none() {
                    let compiled = Matcher::compile(separator, options, CompileErrors::Quoted);
                    matchers[position] = Some(compiled.or_else(|failure| {
                        if let Some(message) = failure.warning {
                            warning(message)?;
                        }
                        Err(failure.error)
                    })?);
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
            // With `useBytes`, the text carries a stand-in character for each byte past ASCII,
            // which `regex_result` turns back into the byte.
            let piece = &text.as_bytes()[start..end];
            let charsxp = if use_bytes {
                regex_result(&text[start..end], forced_bytes)?
            } else if piece.is_ascii() {
                memo.mk_char(piece)?
            } else {
                heap.mk_char(piece, Encoding::Utf8)?
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

/// The byte ranges of the pieces `strsplit` cuts one string into.
type Pieces = SmallVec<[(usize, usize); 16]>;

/// Pushes the byte range of each character of `text` onto `pieces`.
fn split_chars(text: &str, pieces: &mut Pieces) {
    pieces.extend(
        text.char_indices()
            .map(|(offset, character)| (offset, offset + character.len_utf8())),
    );
}

/// The byte a one-byte separator matches, when it matches only itself: any byte with
/// `fixed = TRUE`, and otherwise one that is not a metacharacter in either regex flavor.
fn literal_byte(separator: &str, options: PatternOptions) -> Option<u8> {
    let &[byte] = separator.as_bytes() else {
        return None;
    };
    if options.fixed {
        return Some(byte);
    }
    let special = options.ignore_case && byte.is_ascii_alphabetic();
    (!special && !b".\\|()[]{}^$*+?".contains(&byte)).then_some(byte)
}

/// [`split_text`] for a separator that is one byte, which a byte search finds without the
/// matcher. The separator is ASCII, so it never matches inside a multibyte character.
fn split_byte(byte: u8, text: &str, pieces: &mut Pieces) {
    let mut start = 0;
    for position in memchr::memchr_iter(byte, text.as_bytes()) {
        pieces.push((start, position));
        start = position + 1;
    }
    if start < text.len() {
        pieces.push((start, text.len()));
    }
}

/// Splits as GNU R does: each search runs on what is left of the text, so `^` matches at every
/// piece, and an empty match at the start of the rest splits off one character. Pushes the byte
/// range of each piece onto `pieces`.
fn split_text(matcher: &Matcher, text: &str, pieces: &mut Pieces) -> R<()> {
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

/// Runs `body` with the interpreter's text buffer, empty. A nested call (a `%s` conversion that
/// runs an `as.character` method that calls `sprintf`, say) finds the buffer taken and starts
/// its own, and the larger of the two is kept.
#[inline(always)]
pub fn with_byte_buffer<T>(body: impl FnOnce(&mut Vec<u8>) -> R<T>) -> R<T> {
    let mut buffer = std::mem::take(&mut crate::interp().text_buffer);
    buffer.clear();
    let result = body(&mut buffer);
    let kept = &mut crate::interp().text_buffer;
    if buffer.capacity() > kept.capacity() {
        *kept = buffer;
    }
    result
}

/// [`with_byte_buffer`] for text that is UTF-8.
#[inline(always)]
pub fn with_text_buffer<T>(body: impl FnOnce(&mut String) -> R<T>) -> R<T> {
    with_byte_buffer(|bytes| {
        // SAFETY: the buffer arrives empty, and an empty buffer is valid UTF-8.
        let mut text = unsafe { String::from_utf8_unchecked(std::mem::take(bytes)) };
        let result = body(&mut text);
        *bytes = text.into_bytes();
        result
    })
}

/// One `%` conversion of a format string.
struct Conversion<'a> {
    /// The conversion as written, for error messages, which show it the way GNU R hands it to C
    /// (see [`spec_as_run`]).
    text: &'a str,
    argument: u32,
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    alternate: bool,
    width: Option<Amount>,
    precision: Option<Amount>,
    kind: char,
}

/// A width or precision, written out or taken from an argument by `*`. The payloads are 32-bit
/// so a conversion stays small enough that the pieces of a typical format fit inline.
#[derive(Clone, Copy)]
enum Amount {
    Fixed(u32),
    Argument(u32),
}

/// A conversion and the literal text before it. A `%%`, and the text after a format's last
/// conversion, end a piece with no conversion, so a format with three conversions fits the inline
/// pieces.
struct FormatPiece<'a> {
    literal: &'a str,
    conversion: Option<Conversion<'a>>,
}

/// The pieces of every format string of a call, one format after another.
type FormatPieces<'a> = SmallVec<[FormatPiece<'a>; 4]>;

/// An argument of `sprintf` and the conversions it needs, computed once per call.
struct FormatArgument {
    value: Sexp,
    kind: SexpType,
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
    if call.len() > 100 {
        return Err(error("only 100 arguments are allowed"));
    }
    values::refuse_bytes(formats)?;
    for arg in call.args.iter().skip(1) {
        values::refuse_bytes(arg.value)?;
    }
    let mut arguments: SmallVec<[FormatArgument; 8]> =
        SmallVec::with_capacity(call.len().saturating_sub(1));
    for (position, arg) in call.args.iter().enumerate().skip(1) {
        let value = arg.value;
        let kind = value.kind();
        if matches!(kind, SexpType::Symbol | SexpType::Language) {
            return Err(error(format!(
                "invalid type of argument[{position}]: '{}'",
                kind.name()
            )));
        }
        arguments.push(FormatArgument {
            value,
            kind,
            length: length(value),
            text: None,
            whole: None,
        });
    }
    let format_count = length(formats);
    if format_count == 0 || arguments.iter().any(|argument| argument.length == 0) {
        return alloc(SexpType::String, 0);
    }
    // One bit per argument, set once a conversion reads it.
    let mut used: SmallVec<[u64; 2]> = smallvec![0; arguments.len().div_ceil(64)];
    // An `NA` format is the text "NA", as in GNU R.
    let texts: SmallVec<[Cow<'_, str>; 1]> = (0..format_count)
        .map(|index| string_at(formats, index).unwrap_or(Cow::Borrowed("NA")))
        .collect();
    // The pieces of format `i` are `pieces[ends[i - 1]..ends[i]]`.
    let mut pieces = FormatPieces::new();
    let mut ends: SmallVec<[usize; 2]> = SmallVec::with_capacity(format_count);
    for text in &texts {
        parse_format(text, &arguments, &mut used, &mut pieces)?;
        ends.push(pieces.len());
    }
    // `%s` converts a classed argument with its `as.character` method first, and the result's
    // length is what recycles, so a condition formats as one string.
    for piece in &pieces {
        if let Some(conversion) = &piece.conversion
            && conversion.kind == 's'
        {
            let argument = &mut arguments[conversion.argument as usize];
            if argument.text.is_none() && argument.value.is_object() {
                let text = as_character(argument.value, || call.env())?;
                argument.text = Some(text);
                argument.length = length(text);
            }
        }
    }
    let mut count = format_count;
    for argument in &arguments {
        if argument.length == 0 {
            return alloc(SexpType::String, 0);
        }
        count = count.max(argument.length);
    }
    if !count.is_multiple_of(format_count)
        || arguments
            .iter()
            .any(|argument| !count.is_multiple_of(argument.length))
    {
        return Err(error("arguments cannot be recycled to the same length"));
    }
    let result = alloc(SexpType::String, count)?;
    with_text_buffer(|text| {
        for row in 0..count {
            text.clear();
            let format = row % format_count;
            let start = if format == 0 { 0 } else { ends[format - 1] };
            for piece in &pieces[start..ends[format]] {
                if !piece.literal.is_empty() {
                    text.push_str(piece.literal);
                }
                if let Some(conversion) = &piece.conversion {
                    format_conversion(conversion, &mut arguments, row, call.env()?, text)?;
                }
            }
            values::set_mkchar(result, row, text)?;
        }
        Ok(())
    })?;
    let read: usize = used.iter().map(|word| word.count_ones() as usize).sum();
    let unused = arguments.len() - read;
    if unused > 0 {
        let what = if unused == 1 {
            "one argument".to_owned()
        } else {
            format!("{unused} arguments")
        };
        // GNU R names the format only when there is one.
        if format_count == 1 {
            warning(format!("{what} not used by format '{}'", texts[0]))?;
        } else {
            warning(format!("{what} not used by format"))?;
        }
    }
    Ok(result)
}

/// The conversion letters `sprintf` accepts.
const CONVERSIONS: &[u8] = b"dioxXfeEgGaAs";

/// Splits a format string into pieces, resolving which argument each conversion (and each `*`)
/// reads. `used` gets a bit set for each argument read.
fn parse_format<'a>(
    format: &'a str,
    arguments: &[FormatArgument],
    used: &mut [u64],
    pieces: &mut FormatPieces<'a>,
) -> R<()> {
    // Every byte the parser looks for is ASCII, which never occurs inside a multibyte UTF-8
    // character, so it steps through bytes and slices only at ASCII positions.
    let bytes = format.as_bytes();
    let mut literal_start = 0;
    let mut next_argument = 0;
    let mut index = 0;
    let mut take = |explicit: Option<usize>, used: &mut [u64]| -> R<u32> {
        let argument = match explicit {
            Some(argument) => argument,
            None => {
                next_argument += 1;
                next_argument - 1
            }
        };
        if argument < arguments.len() {
            used[argument / 64] |= 1 << (argument % 64);
            // `sprintf` takes at most 100 arguments.
            return Ok(argument as u32);
        }
        match explicit {
            Some(argument) => Err(error(format!(
                "reference to non-existent argument {}",
                argument + 1
            ))),
            None => Err(error("too few arguments")),
        }
    };
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'%') {
            // The literal runs through the first `%`, and the second is skipped.
            pieces.push(FormatPiece {
                literal: &format[literal_start..=index],
                conversion: None,
            });
            index += 2;
            literal_start = index;
            continue;
        }
        let start = index;
        index += 1;
        let explicit = positional_reference(bytes, &mut index);
        let mut conversion = Conversion {
            text: "",
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
        while let Some(&byte) = bytes.get(index) {
            match byte {
                b'-' => conversion.left = true,
                b'+' => conversion.plus = true,
                b' ' => conversion.space = true,
                b'0' => conversion.zero = true,
                b'#' => conversion.alternate = true,
                _ => break,
            }
            index += 1;
        }
        if bytes.get(index) == Some(&b'*') {
            index += 1;
            let star = positional_reference(bytes, &mut index);
            conversion.width = Some(Amount::Argument(take(star, used)?));
        } else if let Some(width) = read_number(bytes, &mut index) {
            conversion.width = Some(Amount::Fixed(width));
        }
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            if bytes.get(index) == Some(&b'*') {
                if conversion
                    .width
                    .is_some_and(|width| matches!(width, Amount::Argument(_)))
                {
                    return Err(error(
                        "at most one asterisk '*' is supported in each conversion specification",
                    ));
                }
                index += 1;
                let star = positional_reference(bytes, &mut index);
                conversion.precision = Some(Amount::Argument(take(star, used)?));
            } else {
                conversion.precision =
                    Some(Amount::Fixed(read_number(bytes, &mut index).unwrap_or(0)));
            }
        }
        let Some(&kind) = bytes.get(index).filter(|kind| CONVERSIONS.contains(kind)) else {
            // GNU R reads a specification up to the next conversion letter, and blames it on
            // the type of the argument it would have formatted.
            let Some(letter) = bytes[start + 1..]
                .iter()
                .position(|byte| CONVERSIONS.contains(byte))
            else {
                let rest = &format[start..];
                return Err(error(format!("unrecognised format specification '{rest}'")));
            };
            let chunk = &format[start..=start + 1 + letter];
            if arguments.is_empty() {
                return Err(error("too few arguments"));
            }
            let advice = match arguments[take(explicit, used)? as usize].kind {
                SexpType::Real => "use format %f, %e, %g or %a for numeric objects",
                SexpType::Integer => "use format %d, %i, %o, %x or %X for integer objects",
                SexpType::Logical => "use format %d or %i for logical objects",
                _ => "use format %s for character objects",
            };
            return Err(error(format!("invalid format '{chunk}'; {advice}")));
        };
        conversion.kind = char::from(kind);
        conversion.text = &format[start..=index];
        if arguments.is_empty() {
            return Err(error("too few arguments"));
        }
        conversion.argument = take(explicit, used)?;
        pieces.push(FormatPiece {
            literal: &format[literal_start..start],
            conversion: Some(conversion),
        });
        index += 1;
        literal_start = index;
    }
    if literal_start < bytes.len() {
        pieces.push(FormatPiece {
            literal: &format[literal_start..],
            conversion: None,
        });
    }
    Ok(())
}

/// Reads an `n$` argument reference, which is 1-based, if one is written at `index`.
fn positional_reference(bytes: &[u8], index: &mut usize) -> Option<usize> {
    let mut cursor = *index;
    let number = read_number(bytes, &mut cursor)?;
    if bytes.get(cursor) == Some(&b'$') && number > 0 {
        *index = cursor + 1;
        Some(number as usize - 1)
    } else {
        None
    }
}

fn read_number(bytes: &[u8], index: &mut usize) -> Option<u32> {
    let start = *index;
    let mut value: u32 = 0;
    while let Some(&digit) = bytes.get(*index).filter(|byte| byte.is_ascii_digit()) {
        value = value
            .saturating_mul(10)
            .saturating_add(u32::from(digit - b'0'));
        *index += 1;
    }
    (*index > start).then_some(value)
}

/// Appends one conversion of row `row` to `out`.
unsafe fn format_conversion(
    conversion: &Conversion<'_>,
    arguments: &mut [FormatArgument],
    row: usize,
    env: Sexp,
    out: &mut String,
) -> R<()> {
    let mut left = conversion.left;
    // The value of this row's `*`, which error messages splice into the specification.
    let mut star = None;
    let width = match conversion.width {
        None => 0,
        Some(Amount::Fixed(width)) => width as usize,
        Some(Amount::Argument(position)) => {
            let value = star_value(&arguments[position as usize], row)?;
            star = Some(value);
            if value < 0 {
                left = true;
            }
            value.unsigned_abs() as usize
        }
    };
    // GNU R writes a `*` precision into the specification as digits, so a negative one becomes
    // `%.-3f`, which glibc cannot parse and echoes instead of a value.
    let mut negative_precision = None;
    let precision = match conversion.precision {
        None => None,
        Some(Amount::Fixed(precision)) => Some(precision as usize),
        Some(Amount::Argument(position)) => {
            let value = star_value(&arguments[position as usize], row)?;
            star = Some(value);
            if value < 0 {
                negative_precision = Some(value);
            }
            (value >= 0).then_some(value as usize)
        }
    };
    let argument = &mut arguments[conversion.argument as usize];
    let index = row % argument.length;
    let value = argument.value;
    let begin = out.len();
    if let Some(negative) = negative_precision
        && conversion.kind == 's'
    {
        push_unparsed(out, conversion, width, negative, 's');
        return Ok(());
    }
    if conversion.kind == 's' {
        let text = match argument.text {
            Some(text) => text,
            None => {
                let text = as_character(value, || Ok(env))?;
                argument.text = Some(text);
                text
            }
        };
        out.push_str(&string_at(text, index).unwrap_or(Cow::Borrowed("NA")));
        // The precision cuts the string to at most that many bytes, inside a character too, as
        // GNU R does. The bytes of a cut character travel as their stand-ins, which count one
        // byte each toward the width.
        let mut stand_ins = 0;
        if let Some(precision) = precision
            && out.len() - begin > precision
        {
            let cut = begin + precision;
            let mut end = cut;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            let mut partial = [0u8; 4];
            let partial = &mut partial[..cut - end];
            partial.copy_from_slice(&out.as_bytes()[end..cut]);
            out.truncate(end);
            for &byte in partial.iter() {
                out.push(values::stand_in(byte));
            }
            stand_ins = partial.len() * 3;
        }
        pad_from(out, begin, width + stand_ins, left);
        return Ok(());
    }
    let integer_conversion = matches!(conversion.kind, 'd' | 'i' | 'o' | 'x' | 'X');
    match value.kind() {
        SexpType::String => {
            return Err(error(format!(
                "invalid format '{}'; use format %s for character objects",
                spec_as_run(conversion.text, star)
            )));
        }
        SexpType::Logical | SexpType::Integer | SexpType::Real => {}
        _ => return Err(error("unsupported type")),
    }
    if integer_conversion {
        let integer = match value.kind() {
            SexpType::Real => {
                let whole = match argument.whole {
                    Some(whole) => whole,
                    None => {
                        // GNU R takes a double for `%d` when C's `(int) x == x` holds for every
                        // element or it is `NA`, so NaN, the infinities and 2^31 fail while -2^31
                        // passes and then becomes NA in the coercion, which warns once.
                        let numbers = values::reals(value);
                        let whole = numbers.iter().all(|&number| {
                            is_na_real(number)
                                || (number == number.trunc()
                                    && (-2147483648.0..2147483648.0).contains(&number))
                        });
                        argument.whole = Some(whole);
                        if whole && numbers.contains(&-2147483648.0) {
                            warning("NAs introduced by coercion to integer range")?;
                        }
                        whole
                    }
                };
                if !whole {
                    return Err(error(format!(
                        "invalid format '{}'; use format %f, %e, %g or %a for numeric objects",
                        spec_as_run(conversion.text, star)
                    )));
                }
                let number = values::reals(value)[index];
                if is_na_real(number) || number == -2147483648.0 {
                    NA_INTEGER
                } else {
                    number as i32
                }
            }
            _ => values::ints(value)[index],
        };
        if let Some(negative) = negative_precision {
            // An integer NA goes through `%s`, so the echo names that conversion.
            let kind = if integer == NA_INTEGER {
                's'
            } else {
                conversion.kind
            };
            push_unparsed(out, conversion, width, negative, kind);
            return Ok(());
        }
        if integer == NA_INTEGER {
            // GNU R prints an integer NA through `%s` with the same width and precision, so the
            // precision cuts the text.
            out.push_str(&"NA"[..precision.unwrap_or(2).min(2)]);
            pad_from(out, begin, width, left);
            return Ok(());
        }
        return push_integer(out, integer, conversion, width, left, precision);
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
        // GNU R prints the specials through `%s`, which keeps the width but not the precision,
        // after putting the sign or space the flags ask for in front.
        out.push_str(if is_na_real(number) {
            if conversion.space { " NA" } else { "NA" }
        } else if number.is_nan() {
            if conversion.space { " NaN" } else { "NaN" }
        } else if number < 0.0 {
            "-Inf"
        } else if conversion.plus {
            "+Inf"
        } else if conversion.space {
            " Inf"
        } else {
            "Inf"
        });
        pad_from(out, begin, width, left);
        return Ok(());
    }
    if let Some(negative) = negative_precision {
        push_unparsed(out, conversion, width, negative, conversion.kind);
        return Ok(());
    }
    push_real(out, number, conversion, width, left, precision)
}

/// A specification the way GNU R hands it to C, which is how its error messages show it: without
/// the `n$` reference, and with the value of the `*` (and its own `n$`) written in as a number.
fn spec_as_run(text: &str, star: Option<i32>) -> String {
    let bytes = text.as_bytes();
    let mut index = 1;
    positional_reference(bytes, &mut index);
    let mut spec = String::with_capacity(text.len() + 8);
    spec.push('%');
    let rest = &text[index..];
    match (star, rest.find('*')) {
        (Some(value), Some(at)) => {
            spec.push_str(&rest[..at]);
            spec.push_str(&value.to_string());
            let mut after = index + at + 1;
            positional_reference(bytes, &mut after);
            spec.push_str(&text[after..]);
        }
        _ => spec.push_str(rest),
    }
    spec
}

/// Appends what glibc prints for a specification it cannot parse, here one with a negative
/// precision: the flags in glibc's own order (with `0` dropped under `-`), the width, `.0` for the
/// empty precision, and then the rest of the specification as text.
fn push_unparsed(
    out: &mut String,
    conversion: &Conversion<'_>,
    width: usize,
    negative: i32,
    kind: char,
) {
    out.push('%');
    if conversion.alternate {
        out.push('#');
    }
    if conversion.plus {
        out.push('+');
    } else if conversion.space {
        out.push(' ');
    }
    if conversion.left {
        out.push('-');
    } else if conversion.zero {
        out.push('0');
    }
    if width > 0 {
        out.push_str(&width.to_string());
    }
    out.push_str(".0");
    out.push_str(&negative.to_string());
    out.push(kind);
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

fn push_integer(
    out: &mut String,
    value: i32,
    conversion: &Conversion<'_>,
    width: usize,
    left: bool,
    precision: Option<usize>,
) -> R<()> {
    let begin = out.len();
    // `%o`, `%x` and `%X` print the bits as unsigned, as C does.
    let bits = value as u32;
    match conversion.kind {
        'x' if conversion.alternate && value != 0 => out.push_str("0x"),
        'X' if conversion.alternate && value != 0 => out.push_str("0X"),
        'o' | 'x' | 'X' => {}
        _ if value < 0 => out.push('-'),
        _ if conversion.plus => out.push('+'),
        _ if conversion.space => out.push(' '),
        _ => {}
    }
    let digits_at = out.len();
    match conversion.kind {
        'o' => write!(out, "{bits:o}"),
        'x' => write!(out, "{bits:x}"),
        'X' => write!(out, "{bits:X}"),
        _ => {
            crate::format::push_decimal(out, i64::from(value.unsigned_abs()));
            Ok(())
        }
    }
    .map_err(|_| error("formatting a number failed"))?;
    if let Some(precision) = precision {
        let digits = out.len() - digits_at;
        if precision == 0 && value == 0 {
            out.truncate(digits_at);
        } else if digits < precision {
            insert_fill(out, digits_at, '0', precision - digits);
        }
    }
    // `#` makes octal start with a zero, adding one only when the digits (padded to the
    // precision) do not already.
    if conversion.kind == 'o'
        && conversion.alternate
        && out.as_bytes().get(digits_at) != Some(&b'0')
    {
        out.insert(digits_at, '0');
    }
    let zero_pad = conversion.zero && !left && precision.is_none();
    finish_number(out, begin, digits_at, width, left, zero_pad);
    Ok(())
}

fn push_real(
    out: &mut String,
    value: f64,
    conversion: &Conversion<'_>,
    width: usize,
    left: bool,
    precision: Option<usize>,
) -> R<()> {
    let begin = out.len();
    let magnitude = value.abs();
    let alternate = conversion.alternate;
    if value.is_sign_negative() {
        out.push('-');
    } else if conversion.plus {
        out.push('+');
    } else if conversion.space {
        out.push(' ');
    }
    let mut digits_at = out.len();
    match conversion.kind {
        'f' => {
            push_fixed(out, magnitude, precision.unwrap_or(6));
            if alternate && !out[digits_at..].contains('.') {
                out.push('.');
            }
        }
        'e' | 'E' => {
            let precision = precision.unwrap_or(6);
            push_scientific(out, magnitude, precision);
            if alternate
                && precision == 0
                && let Some(marker) = out[digits_at..].find('e')
            {
                out.insert(digits_at + marker, '.');
            }
        }
        'g' | 'G' => push_general(out, magnitude, precision.unwrap_or(6).max(1), alternate),
        _ => {
            // The `0x` prefix goes before any zero padding.
            out.push_str(&hexadecimal_form(magnitude, precision, alternate));
            digits_at += 2;
        }
    }
    if conversion.kind.is_ascii_uppercase() {
        out[begin..].make_ascii_uppercase();
    }
    finish_number(out, begin, digits_at, width, left, conversion.zero && !left);
    Ok(())
}

/// Pads the number written from `begin` to `width`: zeros go between the sign (and `0x`
/// prefix) and the digits at `digits_at`, spaces go outside.
fn finish_number(
    out: &mut String,
    begin: usize,
    digits_at: usize,
    width: usize,
    left: bool,
    zero_pad: bool,
) {
    let length = out.len() - begin;
    if zero_pad && length < width {
        insert_fill(out, digits_at, '0', width - length);
    } else {
        pad_from(out, begin, width, left);
    }
}

/// Pads the text written from `begin` with spaces to `width` bytes, on the left unless `left`.
fn pad_from(out: &mut String, begin: usize, width: usize, left: bool) {
    let length = out.len() - begin;
    if length >= width {
        return;
    }
    if left {
        out.extend(std::iter::repeat_n(' ', width - length));
    } else {
        insert_fill(out, begin, ' ', width - length);
    }
}

/// Inserts `count` copies of the ASCII `fill` at byte `at`, without a temporary string.
pub(crate) fn insert_fill(out: &mut String, at: usize, fill: char, count: usize) {
    debug_assert!(fill.is_ascii());
    out.extend(std::iter::repeat_n(fill, count));
    // SAFETY: the rotation moves the ASCII fill bytes, which are whole characters, to `at`, a
    // character boundary, and keeps every other character's bytes together and in order.
    unsafe { out.as_mut_vec()[at..].rotate_right(count) };
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

fn do_toupper<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    change_case(context, arguments, true)
}

fn do_tolower<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    change_case(context, arguments, false)
}

fn change_case<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>, upper: bool) -> R<Local<'s>> {
    let x = arguments.value(0);
    let Some(text) = handles::dense::<Str>(context, x)? else {
        return Err(error("non-character argument"));
    };
    if all_ascii(context, text) {
        return change_ascii_case(context, x, text, upper);
    }
    refuse_bytes(context, text)?;
    for (index, chars) in text.iter(context).enumerate() {
        if !handles::is_na(chars)
            && !chars.has_mark(gp::LATIN1)
            && std::str::from_utf8(chars.bytes()).is_err()
        {
            return Err(error(format!("invalid multibyte string {}", index + 1)));
        }
    }
    map_strings(context, x, text, |text| {
        Some(
            text.chars()
                .map(|character| change_char_case(character, upper))
                .collect(),
        )
    })
}

/// Whether every non-missing string of `text` is ASCII, where case mapping is a byte map.
fn all_ascii(context: &Cx<'_>, text: Local<'_, Str>) -> bool {
    text.iter(context)
        .all(|chars| handles::is_na(chars) || chars.has_mark(gp::ASCII))
}

fn change_ascii_case<'s>(
    context: &mut Cx<'s>,
    x: Local<'s>,
    text: Local<'s, Str>,
    upper: bool,
) -> R<Local<'s>> {
    let mut result = context.alloc::<Str>(text.len())?;
    let mut buffer = Vec::new();
    for index in 0..text.len() {
        let chars = text.get(context, index);
        let bytes = chars.bytes();
        let changes = |byte: &u8| {
            if upper {
                byte.is_ascii_lowercase()
            } else {
                byte.is_ascii_uppercase()
            }
        };
        if handles::is_na(chars) || !bytes.iter().any(changes) {
            result.set(context, index, chars);
            continue;
        }
        buffer.clear();
        buffer.extend(bytes.iter().map(|byte| {
            if upper {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            }
        }));
        handles::set_bytes(context, &mut result, index, &buffer, Encoding::Native)?;
    }
    handles::copy_attributes(context, x, &mut result)?;
    Ok(result.finish().value())
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

fn do_chartr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let old = expand_character_ranges(&translation_spec(context, arguments.value(0), "old")?);
    let new = expand_character_ranges(&translation_spec(context, arguments.value(1), "new")?);
    let x = arguments.value(2);
    if old.len() > new.len() {
        return Err(error("'old' is longer than 'new'"));
    }
    let Some(text) = handles::dense::<Str>(context, x)? else {
        return Err(error("invalid 'x' argument"));
    };
    refuse_bytes(context, text)?;
    map_strings(context, x, text, |text| {
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

fn translation_spec(context: &mut Cx<'_>, value: Local<'_>, what: &str) -> R<String> {
    match handles::first_string(context, value) {
        Some(text) if value.r_type() == SexpType::String => Ok(text.into_owned()),
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

fn do_strtrim<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let x = arguments.value(0);
    let Some(text) = handles::dense::<Str>(context, x)? else {
        return Err(error("strtrim() requires a character vector"));
    };
    let widths = handles::integer_vector(context, arguments.value(1))?;
    if !text.is_empty()
        && (widths.is_empty()
            || widths
                .as_slice(context)
                .iter()
                .any(|&width| width == NA_INTEGER || width < 0))
    {
        return Err(error("invalid 'width' argument"));
    }
    let mut result = context.alloc::<Str>(text.len())?;
    for index in 0..text.len() {
        let chars = text.get(context, index);
        if handles::is_na(chars) {
            result.set(context, index, chars);
            continue;
        }
        let limit = widths.as_slice(context)[index % widths.len()] as usize;
        let current = handles::text(chars);
        let mut used = 0;
        let mut end = current.len();
        for (offset, character) in current.char_indices() {
            used += display_width(character.encode_utf8(&mut [0; 4]));
            if used > limit {
                end = offset;
                break;
            }
        }
        if end == current.len() {
            result.set(context, index, chars);
        } else {
            handles::set_text_range(context, &mut result, index, text, index, 0..end)?;
        }
    }
    handles::copy_attributes(context, x, &mut result)?;
    Ok(result.finish().value())
}

fn do_starts_with<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    starts_or_ends_with(context, arguments, true)
}

fn do_ends_with<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    starts_or_ends_with(context, arguments, false)
}

fn starts_or_ends_with<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
    starts: bool,
) -> R<Local<'s>> {
    let (x, affix) = (arguments.value(0), arguments.value(1));
    if x.r_type() != SexpType::String || affix.r_type() != SexpType::String {
        return Err(error("non-character object(s)"));
    }
    let (Some(x), Some(affix)) = (
        handles::dense::<Str>(context, x)?,
        handles::dense::<Str>(context, affix)?,
    ) else {
        return Err(error("non-character object(s)"));
    };
    let (text_count, affix_count) = (x.len(), affix.len());
    let count = if text_count == 0 || affix_count == 0 {
        0
    } else {
        text_count.max(affix_count)
    };
    let mut result = context.alloc::<Logical>(count)?;
    let affixes: Vec<Option<&[u8]>> = affix
        .iter(context)
        .map(|chars| {
            (!handles::is_na(chars))
                .then(|| utf8_bytes(chars))
                .flatten()
        })
        .collect();
    let test = |text: Gc<'_, Chars>, affix_index: usize| -> i32 {
        let affix_chars = affix.get(context, affix_index);
        if handles::is_na(text) || handles::is_na(affix_chars) {
            return NA_LOGICAL;
        }
        // Valid UTF-8 matches a prefix or suffix byte for byte exactly when it matches
        // character for character. An affix without NUL matches a text's prefix exactly when it
        // matches the text cut at its first NUL, as `handles::text` reads it, but a suffix match
        // must check the text holds no NUL.
        if let (Some(affix), true) = (affixes[affix_index], text.is_valid_utf8())
            && !text.has_mark(gp::LATIN1 | gp::BYTES)
        {
            let bytes = text.bytes();
            return if starts {
                bytes.starts_with(affix) as i32
            } else {
                (bytes.ends_with(affix) && memchr::memchr(0, bytes).is_none()) as i32
            };
        }
        let (text, affix) = (handles::text(text), handles::text(affix_chars));
        if starts {
            text.starts_with(affix.as_ref()) as i32
        } else {
            text.ends_with(affix.as_ref()) as i32
        }
    };
    let out = result.as_mut_slice();
    if affixes.len() == 1 && text_count == count {
        for (slot, text) in out.iter_mut().zip(x.iter(context)) {
            *slot = test(text, 0);
        }
    } else {
        for (index, slot) in out.iter_mut().enumerate() {
            *slot = test(x.get(context, index % text_count), index % affix_count);
        }
    }
    Ok(result.finish().value())
}

/// The bytes of `chars` when they are its text as UTF-8 as they stand: valid UTF-8 not declared
/// Latin-1 or bytes, and without the NUL that [`handles::text`] would end the text at.
fn utf8_bytes(chars: Gc<'_, Chars>) -> Option<&[u8]> {
    if !chars.is_valid_utf8() || chars.has_mark(gp::LATIN1 | gp::BYTES) {
        return None;
    }
    let bytes = chars.bytes();
    memchr::memchr(0, bytes).is_none().then_some(bytes)
}

fn do_strrep<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let x = arguments.value(0);
    let Some(text) = handles::dense::<Str>(context, x)? else {
        return Err(error("invalid 'x' argument"));
    };
    let times = handles::integer_vector(context, arguments.value(1))?;
    let (text_count, times_count) = (text.len(), times.len());
    let count = if text_count == 0 || times_count == 0 {
        0
    } else {
        text_count.max(times_count)
    };
    let mut result = context.alloc::<Str>(count)?;
    for index in 0..count {
        let repeat = times.as_slice(context)[index % times_count];
        let chars = text.get(context, index % text_count);
        if repeat == NA_INTEGER || handles::is_na(chars) {
            result.set(context, index, handles::na());
            continue;
        }
        if repeat < 0 {
            return Err(error("invalid 'times' value"));
        }
        let repeated = handles::text(chars).repeat(repeat as usize);
        handles::set_text(context, &mut result, index, &repeated)?;
    }
    if text_count >= times_count {
        handles::copy_names(context, x, &mut result)?;
    }
    Ok(result.finish().value())
}

fn do_strtoi<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let base = handles::integer_scalar(context, arguments.value(1));
    if base == NA_INTEGER || base == 1 || !(0..=36).contains(&base) {
        return Err(error("invalid 'base' argument"));
    }
    let Some(text) = handles::dense::<Str>(context, arguments.value(0))? else {
        return Err(error("invalid 'x' argument"));
    };
    let mut result = context.alloc::<Integer>(text.len())?;
    let numbers = result.as_mut_slice().iter_mut();
    for (slot, chars) in numbers.zip(text.iter(context)) {
        *slot = if handles::is_na(chars) {
            NA_INTEGER
        } else {
            parse_c_integer(&handles::text(chars), base as u32).unwrap_or(NA_INTEGER)
        };
    }
    Ok(result.finish().value())
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

fn do_make_names<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(names) = handles::dense::<Str>(context, arguments.value(0))? else {
        return Err(error("non-character names"));
    };
    let allow_underscore = handles::logical_scalar(context, arguments.value(1));
    if allow_underscore == NA_LOGICAL {
        return Err(error("invalid 'allow_' value"));
    }
    let allow_underscore = allow_underscore == 1;
    refuse_bytes(context, names)?;
    let made: Vec<String> = names
        .iter(context)
        .map(|chars| {
            let name = if handles::is_na(chars) {
                Cow::Borrowed("NA")
            } else {
                handles::text(chars)
            };
            let mut characters = name.chars();
            let needs_prefix = match (characters.next(), characters.next()) {
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
    let mut result = context.alloc::<Str>(made.len())?;
    for (index, name) in made.iter().enumerate() {
        handles::set_text(context, &mut result, index, name)?;
    }
    Ok(result.finish().value())
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
    let ascii = separator.is_ascii()
        && (0..length(names)).all(|index| {
            let charsxp = names.element(index);
            charsxp == na_string() || charsxp.has_gp(gp::ASCII)
        });
    if ascii {
        return make_unique_interned(names, separator.as_bytes());
    }
    // NA is a name of its own, distinct from the text "NA": its first occurrence stays NA, and
    // a repeat is made unique from the text "NA", as in GNU R.
    let original: Vec<Option<String>> = strings(names);
    let mut taken: FxHashSet<String> = original.iter().flatten().cloned().collect();
    let mut seen: FxHashSet<Option<&str>> = FxHashSet::default();
    let mut counters: FxHashMap<Option<&str>, usize> = FxHashMap::default();
    let mut made: Vec<Option<String>> = Vec::with_capacity(original.len());
    for name in &original {
        let key = name.as_deref();
        if seen.insert(key) {
            made.push(name.clone());
            continue;
        }
        let text = key.unwrap_or("NA");
        let counter = counters.entry(key).or_insert(1);
        let mut candidate = format!("{text}{separator}{counter}");
        while taken.contains(&candidate) {
            *counter += 1;
            candidate = format!("{text}{separator}{counter}");
        }
        *counter += 1;
        taken.insert(candidate.clone());
        made.push(Some(candidate));
    }
    values::string_vector_na(&made)
}

/// `make.unique` over ASCII names, which works on the strings themselves rather than copies of
/// their text. R interns its strings and ASCII text has one CHARSXP, so a name is taken exactly
/// when its CHARSXP is marked. The mark is the CHARSXP's truelength, negated, as data.table's C
/// marks strings: it numbers the string's slot in `marked`, which keeps the truelength to put
/// back and, for a name of the input, the next suffix to try (0 until its first occurrence has
/// been kept). Copying every name into owned text took about 150 MB for a million names, which
/// `[.data.frame` builds row names from whenever its row index repeats.
unsafe fn make_unique_interned(names: Sexp, separator: &[u8]) -> R<Sexp> {
    struct Marked {
        strings: Vec<Sexp>,
        truelengths: Vec<i32>,
        counters: Vec<u32>,
    }
    impl Marked {
        /// The slot of `charsxp` if this call marked it; a negative truelength another
        /// caller left behind names no slot of ours.
        unsafe fn slot(&self, charsxp: Sexp) -> Option<usize> {
            let mark = charsxp.char_truelength();
            let slot = (mark < 0).then(|| mark.unsigned_abs() as usize - 1)?;
            (self.strings.get(slot) == Some(&charsxp)).then_some(slot)
        }

        unsafe fn mark(&mut self, charsxp: Sexp) -> usize {
            let slot = self.strings.len();
            self.strings.push(charsxp);
            self.truelengths.push(charsxp.char_truelength());
            self.counters.push(0);
            charsxp.set_char_truelength(-(slot as i32 + 1));
            slot
        }

        unsafe fn restore(&self) {
            for (&charsxp, &truelength) in self.strings.iter().zip(&self.truelengths) {
                charsxp.set_char_truelength(truelength);
            }
        }
    }
    let na = na_string();
    let count = length(names);
    // Each name marks at most one string, its own or the one made for it, so the slots never
    // grow past the names and never reallocate.
    let mut marked = Marked {
        strings: Vec::with_capacity(count),
        truelengths: Vec::with_capacity(count),
        counters: Vec::with_capacity(count),
    };
    for index in 0..count {
        let charsxp = names.element(index);
        if charsxp != na && marked.slot(charsxp).is_none() {
            marked.mark(charsxp);
        }
    }
    let result = made_unique(names, separator, &mut marked);
    marked.restore();
    return result;

    unsafe fn made_unique(names: Sexp, separator: &[u8], marked: &mut Marked) -> R<Sexp> {
        let na = na_string();
        let count = length(names);
        let result = alloc(SexpType::String, count)?;
        // NA is a name of its own, distinct from the text "NA": its first occurrence stays NA,
        // and a repeat is made unique from the text "NA", as in GNU R.
        let mut na_counter = 0;
        let mut candidate = Vec::new();
        for index in 0..count {
            let charsxp = names.element(index);
            let (counter, text): (&mut u32, &[u8]) = if charsxp == na {
                (&mut na_counter, b"NA")
            } else {
                let slot = marked
                    .slot(charsxp)
                    .ok_or_else(|| error("a name lost its mark while 'make.unique' ran"))?;
                (&mut marked.counters[slot], charsxp.bytes())
            };
            if *counter == 0 {
                *counter = 1;
                set_elt(result, index, charsxp);
                continue;
            }
            let mut next = *counter;
            let made = loop {
                candidate.clear();
                candidate.extend_from_slice(text);
                candidate.extend_from_slice(separator);
                // A suffix counts the names, which fit an `i32`.
                candidate.extend_from_slice(integer_digits(next as i32, &mut [0; 11]));
                next += 1;
                let made = crate::heap().mk_char(&candidate, Encoding::Native)?;
                if marked.slot(made).is_none() {
                    break made;
                }
            };
            set_elt(result, index, made);
            marked.mark(made);
            // `mark` may have grown the counters, so the slot is looked up again.
            let counter = if charsxp == na {
                &mut na_counter
            } else {
                let slot = marked
                    .slot(charsxp)
                    .ok_or_else(|| error("a name lost its mark while 'make.unique' ran"))?;
                &mut marked.counters[slot]
            };
            *counter = next;
        }
        Ok(result)
    }
}

fn do_abbreviate<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(text) = handles::dense::<Str>(context, arguments.value(0))? else {
        return Err(error("the first argument must be a character vector"));
    };
    let minimum = handles::integer_scalar(context, arguments.value(1));
    if minimum == NA_INTEGER {
        return Err(error("invalid 'minlength' argument"));
    }
    let use_classes = is_true(context, arguments.value(2));
    refuse_bytes(context, text)?;
    let mut warned = false;
    let mut result = context.alloc::<Str>(text.len())?;
    for index in 0..text.len() {
        let chars = text.get(context, index);
        if handles::is_na(chars) {
            result.set(context, index, handles::na());
            continue;
        }
        let (abbreviation, ascii) = {
            let current = handles::text(chars);
            let abbreviation = abbreviate(&current, minimum.max(0) as usize, use_classes);
            (abbreviation, current.is_ascii())
        };
        if !ascii && !warned {
            handles::warning(context, "abbreviate used with non-ASCII chars")?;
            warned = true;
        }
        handles::set_text(context, &mut result, index, &abbreviation)?;
    }
    Ok(result.finish().value())
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

fn do_utf8_to_int<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let text = single_string(context, arguments)?;
    let first = text.get(context, 0);
    let count = match std::str::from_utf8(first.bytes()) {
        Ok(decoded) if !handles::is_na(first) => decoded.chars().count(),
        _ => return handles::integer(context, NA_INTEGER),
    };
    let mut result = context.alloc::<Integer>(count)?;
    let decoded = std::str::from_utf8(text.get(context, 0).bytes()).unwrap_or_default();
    for (slot, character) in result.as_mut_slice().iter_mut().zip(decoded.chars()) {
        *slot = character as i32;
    }
    Ok(result.finish().value())
}

/// The argument of `utf8ToInt` and `charToRaw`, which read its first element.
fn single_string<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s, Str>> {
    let text = match handles::dense::<Str>(context, arguments.value(0))? {
        Some(text) if !text.is_empty() => text,
        _ => return Err(error("argument must be a character vector of length 1")),
    };
    if text.len() > 1 {
        handles::warning(
            context,
            "argument should be a character vector of length 1\nall but the first element will be ignored",
        )?;
    }
    Ok(text)
}

fn do_int_to_utf8<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let points = handles::integer_vector(context, arguments.value(0))?;
    let multiple = is_true(context, arguments.value(1));
    let surrogate_pairs = is_true(context, arguments.value(2));
    if multiple {
        let mut result = context.alloc::<Str>(points.len())?;
        let mut encoded = [0; 4];
        for index in 0..points.len() {
            let point = points.as_slice(context)[index];
            let bytes: Option<&[u8]> = match point {
                0 => Some(&[]),
                NA_INTEGER => None,
                _ => match char::from_u32(point as u32) {
                    Some(character) => Some(character.encode_utf8(&mut encoded).as_bytes()),
                    None => None,
                },
            };
            match bytes {
                Some(bytes) => {
                    handles::set_bytes(context, &mut result, index, bytes, Encoding::Utf8)?
                }
                None => result.set(context, index, handles::na()),
            }
        }
        return Ok(result.finish().value());
    }
    let text = 'text: {
        let points = points.as_slice(context);
        let mut text = String::with_capacity(points.len());
        let mut index = 0;
        while index < points.len() {
            let point = points[index];
            index += 1;
            if point == 0 {
                continue;
            }
            if point == NA_INTEGER {
                break 'text None;
            }
            let high_surrogate = (0xD800..0xDC00).contains(&point);
            let next = points.get(index).copied().unwrap_or(0);
            let character = if surrogate_pairs && high_surrogate && (0xDC00..0xE000).contains(&next)
            {
                index += 1;
                char::from_u32((((point - 0xD800) << 10) | (next - 0xDC00)) as u32 + 0x10000)
            } else {
                char::from_u32(point as u32)
            };
            match character {
                Some(character) => text.push(character),
                None => break 'text None,
            }
        }
        Some(text)
    };
    let mut result = context.alloc::<Str>(1)?;
    match text {
        // The code points are the text, so one in the range that stands in for a byte elsewhere
        // (`values::escape_invalid_bytes`) stays that character rather than turning into the byte.
        Some(text) => handles::set_bytes(context, &mut result, 0, text.as_bytes(), Encoding::Utf8)?,
        None => result.set(context, 0, handles::na()),
    }
    Ok(result.finish().value())
}

fn do_char_to_raw<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let text = single_string(context, arguments)?;
    let length = text.get(context, 0).bytes().len();
    let mut result = context.alloc::<Raw>(length)?;
    result
        .as_mut_slice()
        .copy_from_slice(text.get(context, 0).bytes());
    Ok(result.finish().value())
}

fn do_raw_to_char<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(x) = handles::dense::<Raw>(context, arguments.value(0))? else {
        return Err(error("argument 'x' must be a raw vector"));
    };
    let multiple = handles::logical_scalar(context, arguments.value(1));
    if multiple == NA_LOGICAL {
        return Err(error("argument 'multiple' must be TRUE or FALSE"));
    }
    if multiple == 1 {
        let mut result = context.alloc::<Str>(x.len())?;
        for index in 0..x.len() {
            let byte = x.as_slice(context)[index];
            let piece: &[u8] = if byte == 0 {
                &[]
            } else {
                std::slice::from_ref(&byte)
            };
            handles::set_bytes(context, &mut result, index, piece, Encoding::Native)?;
        }
        return Ok(result.finish().value());
    }
    let end = {
        let bytes = x.as_slice(context);
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
        end
    };
    let mut result = context.alloc::<Str>(1)?;
    handles::set_raw_bytes(context, &mut result, 0, x, 0..end, Encoding::Native)?;
    Ok(result.finish().value())
}

fn do_pmatch<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let targets = match_vector(context, arguments.value(0))?;
    let table = match_vector(context, arguments.value(1))?;
    let no_match = handles::integer_scalar(context, arguments.value(2));
    let duplicates = is_true(context, arguments.value(3));
    // A match is its 1-based table position, so 0 marks a target without one.
    let mut matched: SmallVec<[i32; 8]> = smallvec![0; targets.len()];
    {
        let targets = match_texts(context, targets);
        let table = match_texts(context, table);
        let mut used: SmallVec<[u64; 2]> = smallvec![0; table.len().div_ceil(64)];
        let is_free = |used: &[u64], position: usize| {
            duplicates || used[position / 64] & (1 << (position % 64)) == 0
        };
        let claim = |used: &mut [u64], slot: &mut i32, position: usize| {
            *slot = position as i32 + 1;
            if !duplicates {
                used[position / 64] |= 1 << (position % 64);
            }
        };
        for (target, slot) in targets.iter().zip(matched.iter_mut()) {
            if target.is_empty() {
                continue;
            }
            if let Some(position) = (0..table.len())
                .find(|&position| is_free(&used, position) && table[position] == *target)
            {
                claim(&mut used, slot, position);
            }
        }
        for (target, slot) in targets.iter().zip(matched.iter_mut()) {
            if target.is_empty() || *slot != 0 {
                continue;
            }
            let mut candidates = (0..table.len()).filter(|&position| {
                is_free(&used, position) && table[position].starts_with(target.as_ref())
            });
            if let (Some(position), None) = (candidates.next(), candidates.next()) {
                claim(&mut used, slot, position);
            }
        }
    }
    for slot in &mut matched {
        if *slot == 0 {
            *slot = no_match;
        }
    }
    Ok(integers(context, &matched)?.finish().value())
}

fn do_charmatch<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let targets = match_vector(context, arguments.value(0))?;
    let table = match_vector(context, arguments.value(1))?;
    let no_match = handles::integer_scalar(context, arguments.value(2));
    let indices: SmallVec<[i32; 8]> = {
        let table = match_texts(context, table);
        match_texts(context, targets)
            .iter()
            .map(|target| {
                let (mut exact, mut exact_count, mut partial, mut partial_count) = (0, 0, 0, 0);
                for (position, entry) in table.iter().enumerate() {
                    if entry.starts_with(target.as_ref()) {
                        partial = position as i32 + 1;
                        partial_count += 1;
                        if entry.len() == target.len() {
                            exact = partial;
                            exact_count += 1;
                        }
                    }
                }
                match (exact_count, partial_count) {
                    (1, _) => exact,
                    (0, 1) => partial,
                    (0, 0) => no_match,
                    _ => 0,
                }
            })
            .collect()
    };
    Ok(integers(context, &indices)?.finish().value())
}

/// An argument of `pmatch` or `charmatch`: a character vector, with `NULL` as an empty one.
fn match_vector<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Str>> {
    strings_or_null(context, value, "argument is not of mode character")
}

/// The strings of a matching argument, with `NA` read as the text `"NA"`, as GNU R does. Text
/// that is already UTF-8 is borrowed from its string.
fn match_texts<'a, 's: 'a>(
    context: &'a Cx<'_>,
    value: Local<'s, Str>,
) -> SmallVec<[Cow<'a, str>; 8]> {
    value
        .iter(context)
        .map(|chars| {
            if handles::is_na(chars) {
                Cow::Borrowed("NA")
            } else {
                handles::text(chars)
            }
        })
        .collect()
}

fn do_grepl<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    grep(context, arguments, true)
}

fn do_grep<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    grep(context, arguments, false)
}

fn grep<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>, logical: bool) -> R<Local<'s>> {
    let value = is_true(context, arguments.value(3));
    let invert = is_true(context, arguments.value(7));
    let ignore_case = is_true(context, arguments.value(2));
    let perl = is_true(context, arguments.value(4));
    let fixed = is_true(context, arguments.value(5));
    let options = pattern_options(context, ignore_case, perl, fixed)?;
    let pattern = handles::dense::<Str>(context, arguments.value(0))?;
    let text = handles::dense::<Str>(context, arguments.value(1))?;
    let use_bytes = is_true(context, arguments.value(6)) || has_bytes(context, &[pattern, text]);
    let pattern = pattern_argument(context, pattern)?;
    let text = text_argument(context, arguments.value(1), text)?;
    let count = text.len();
    if handles::is_na(pattern.get(context, 0)) {
        return match (logical, value) {
            (true, _) => {
                let mut result = context.alloc::<Logical>(count)?;
                result.as_mut_slice().fill(NA_LOGICAL);
                Ok(result.finish().value())
            }
            (false, true) => {
                let mut result = context.alloc::<Str>(count)?;
                for index in 0..count {
                    result.set(context, index, handles::na());
                }
                Ok(result.finish().value())
            }
            (false, false) => {
                let mut result = context.alloc::<Integer>(count)?;
                result.as_mut_slice().fill(NA_INTEGER);
                Ok(result.finish().value())
            }
        };
    }
    let compiled = {
        let pattern = pattern_text(context, pattern, use_bytes);
        let errors = CompileErrors::for_text(context, &pattern, text, use_bytes);
        Matcher::compile(&pattern, options, errors)
    };
    let matcher = compiled.map_err(|failure| compile_failure(context, failure))?;
    let hit = |context: &mut Cx<'_>, index: usize| -> R<bool> {
        let found = if handles::is_na(text.get(context, index))
            || !valid_subject(context, text, index, use_bytes, options, false)?
        {
            false
        } else {
            let element = subject(text.get(context, index), use_bytes).unwrap_or_default();
            matcher.find_at(&element, 0)?.is_some()
        };
        Ok(found != invert)
    };
    if logical {
        let mut result = context.alloc::<Logical>(count)?;
        for index in 0..count {
            let found = hit(context, index)?;
            result.as_mut_slice()[index] = i32::from(found);
        }
        return Ok(result.finish().value());
    }
    let mut selected: Vec<usize> = Vec::new();
    for index in 0..count {
        if hit(context, index)? {
            selected.push(index);
        }
    }
    if !value {
        let indices: Vec<i32> = selected.iter().map(|&index| index as i32 + 1).collect();
        return Ok(integers(context, &indices)?.finish().value());
    }
    let mut result = context.alloc::<Str>(selected.len())?;
    for (position, &index) in selected.iter().enumerate() {
        result.set(context, position, text.get(context, index));
    }
    let names = handles::names_attribute(context, arguments.value(1));
    if let Some(names) = handles::dense::<Str>(context, names)? {
        let mut selected_names = context.alloc::<Str>(selected.len())?;
        for (position, &index) in selected.iter().enumerate() {
            selected_names.set(context, position, names.get(context, index));
        }
        handles::set_attribute_named(
            context,
            &mut result,
            "names",
            selected_names.finish().value(),
        )?;
    }
    Ok(result.finish().value())
}

fn do_sub<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    substitute(context, arguments, false)
}

fn do_gsub<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    substitute(context, arguments, true)
}

fn substitute<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>, global: bool) -> R<Local<'s>> {
    let ignore_case = is_true(context, arguments.value(3));
    let perl = is_true(context, arguments.value(4));
    let fixed = is_true(context, arguments.value(5));
    let options = pattern_options(context, ignore_case, perl, fixed)?;
    let pattern = handles::dense::<Str>(context, arguments.value(0))?;
    let replacement = handles::dense::<Str>(context, arguments.value(1))?;
    let text = handles::dense::<Str>(context, arguments.value(2))?;
    let forced_bytes = has_bytes(context, &[pattern, replacement, text]);
    let use_bytes = is_true(context, arguments.value(6)) || forced_bytes;
    let pattern = pattern_argument(context, pattern)?;
    let replacement = match replacement {
        Some(replacement) if !replacement.is_empty() => replacement,
        _ => return Err(error("invalid 'replacement' argument")),
    };
    if replacement.len() > 1 {
        handles::warning(
            context,
            "argument 'replacement' has length > 1 and only the first element will be used",
        )?;
    }
    let text = text_argument(context, arguments.value(2), text)?;
    let count = text.len();
    let mut result = context.alloc::<Str>(count)?;
    handles::copy_attributes(context, arguments.value(2), &mut result)?;
    if handles::is_na(pattern.get(context, 0)) {
        for index in 0..count {
            result.set(context, index, handles::na());
        }
        return Ok(result.finish().value());
    }
    let compiled = {
        let pattern = pattern_text(context, pattern, use_bytes);
        if options.fixed && pattern.is_empty() {
            return Err(error("zero-length pattern"));
        }
        Matcher::compile(&pattern, options, CompileErrors::Quoted)
    };
    let matcher = compiled.map_err(|failure| compile_failure(context, failure))?;
    // The replacement is read for every element, across the allocations that make the results,
    // so its string is rooted.
    let replacement_text = subject(
        context.root(replacement.get(context, 0)).rooted(),
        use_bytes,
    );
    let replacement = replacement_text
        .as_deref()
        .map(|text| parse_replacement(text, options));
    with_text_buffer(|output| {
        for index in 0..count {
            if handles::is_na(text.get(context, index)) {
                result.set(context, index, handles::na());
                continue;
            }
            valid_subject(context, text, index, use_bytes, options, true)?;
            let Some(replacement) = &replacement else {
                let found = {
                    let element = subject(text.get(context, index), use_bytes).unwrap_or_default();
                    matcher.find_at(&element, 0)?.is_some()
                };
                let substituted = if found {
                    handles::na()
                } else {
                    text.get(context, index)
                };
                result.set(context, index, substituted);
                continue;
            };
            output.clear();
            let matched = {
                let element = subject(text.get(context, index), use_bytes).unwrap_or_default();
                replace(&matcher, &element, replacement, global, output)?
            };
            if matched {
                set_regex_result(context, &mut result, index, output, forced_bytes)?;
            } else {
                result.set(context, index, text.get(context, index));
            }
        }
        Ok(())
    })?;
    Ok(result.finish().value())
}

/// One part of a replacement string.
enum ReplacementPiece<'a> {
    Text(&'a str),
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

/// The pieces of one replacement string, each literal borrowed from it.
type Replacement<'a> = SmallVec<[ReplacementPiece<'a>; 4]>;

fn parse_replacement(text: &str, options: PatternOptions) -> Replacement<'_> {
    if options.fixed {
        return smallvec![ReplacementPiece::Text(text)];
    }
    let mut pieces = Replacement::new();
    let mut literal_start = 0;
    let mut characters = text.char_indices();
    while let Some((offset, character)) = characters.next() {
        if character != '\\' {
            continue;
        }
        if literal_start < offset {
            pieces.push(ReplacementPiece::Text(&text[literal_start..offset]));
        }
        let Some((escaped_offset, escaped)) = characters.next() else {
            literal_start = text.len();
            break;
        };
        literal_start = escaped_offset + escaped.len_utf8();
        pieces.push(match escaped {
            '1'..='9' => ReplacementPiece::Group(escaped as usize - '0' as usize),
            'U' if options.perl => ReplacementPiece::Case(Case::Upper),
            'L' if options.perl => ReplacementPiece::Case(Case::Lower),
            'E' if options.perl => ReplacementPiece::Case(Case::Keep),
            _ => ReplacementPiece::Text(&text[escaped_offset..literal_start]),
        });
    }
    if literal_start < text.len() {
        pieces.push(ReplacementPiece::Text(&text[literal_start..]));
    }
    pieces
}

/// Writes `text` to `output` with the first match replaced, or every match when `global`, and
/// returns whether anything matched. An empty match right where the previous match ended is skipped, as GNU R does.
fn replace(
    matcher: &Matcher,
    text: &str,
    replacement: &[ReplacementPiece],
    global: bool,
    output: &mut String,
) -> R<bool> {
    let need_groups = replacement
        .iter()
        .any(|piece| matches!(piece, ReplacementPiece::Group(_)));
    output.reserve(text.len());
    let mut position = 0;
    let mut previous_end = None;
    let mut matched = false;
    loop {
        let groups = if need_groups {
            matcher.captures_at(text, position)?
        } else {
            matcher
                .find_at(text, position)?
                .map(|span| smallvec![Some(span)])
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
    if matched {
        output.push_str(&text[position.min(text.len())..]);
    }
    Ok(matched)
}

fn do_regexpr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    regexpr(context, arguments, false)
}

fn do_gregexpr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    regexpr(context, arguments, true)
}

/// `regexpr` and `gregexpr`: match positions, with the attributes base R's `regmatches` reads.
fn regexpr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>, global: bool) -> R<Local<'s>> {
    let ignore_case = is_true(context, arguments.value(2));
    let perl = is_true(context, arguments.value(3));
    let fixed = is_true(context, arguments.value(4));
    let options = pattern_options(context, ignore_case, perl, fixed)?;
    let pattern = handles::dense::<Str>(context, arguments.value(0))?;
    let text = handles::dense::<Str>(context, arguments.value(1))?;
    let use_bytes = is_true(context, arguments.value(5)) || has_bytes(context, &[pattern, text]);
    let pattern = pattern_argument(context, pattern)?;
    if handles::is_na(pattern.get(context, 0)) {
        return Err(error("invalid 'pattern' argument"));
    }
    let text = text_argument(context, arguments.value(1), text)?;
    let (compiled, report) = {
        let pattern = pattern_text(context, pattern, use_bytes);
        let errors = CompileErrors::for_text(context, &pattern, text, use_bytes);
        (
            Matcher::compile(&pattern, options, errors),
            MatchReport::new(context, &pattern, text, use_bytes),
        )
    };
    let matcher = compiled.map_err(|failure| compile_failure(context, failure))?;
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
                .map(|span| smallvec![Some(span)]))
        } else {
            matcher.captures_at(element, position)
        }
    };
    let count = text.len();
    if !global {
        let mut table = MatchTable::new(groups);
        for index in 0..count {
            if handles::is_na(text.get(context, index)) {
                table.push_na();
                continue;
            }
            if !valid_subject(context, text, index, use_bytes, options, false)? {
                table.push_none();
                continue;
            }
            let element = subject(text.get(context, index), use_bytes).unwrap_or_default();
            match search(&element, 0)? {
                Some(found) => table.push(&element, &found),
                None => table.push_none(),
            }
        }
        return table.finish(context, &report, &names);
    }
    let mut result = context.alloc::<List>(count)?;
    for index in 0..count {
        if handles::is_na(text.get(context, index)) {
            let positions = context.scope(na_match)?;
            result.set(context, index, positions);
            continue;
        }
        let mut table = MatchTable::new(groups);
        if valid_subject(context, text, index, use_bytes, options, false)? {
            let element = subject(text.get(context, index), use_bytes).unwrap_or_default();
            let mut position = 0;
            // PCRE and fixed matching find the empty match in an empty string, and TRE does not.
            let searches_empty = options.perl || options.fixed;
            while position < element.len()
                || (searches_empty && element.is_empty() && table.rows == 0)
            {
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
        }
        if table.rows == 0 {
            table.push_none();
        }
        // Each text's positions are stored before the next are made, so a scope releases the
        // handles that made them rather than holding them for the whole call.
        let positions = context.scope(|inner: &mut Cx<'_>| table.finish(inner, &report, &names))?;
        result.set(context, index, positions);
    }
    Ok(result.finish().value())
}

/// `grepRaw(pattern, x, offset, ignore.case, fixed, value, all, invert)`, matching bytes. The
/// matches are non-overlapping and start at the 1-based `offset`; `value` returns the matched
/// bytes, or with `invert` the bytes around them.
fn do_grep_raw<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let (Some(pattern), Some(x)) = (
        handles::dense::<Raw>(context, arguments.value(0))?,
        handles::dense::<Raw>(context, arguments.value(1))?,
    ) else {
        return Err(arguments.error("invalid 'pattern' or 'x' argument"));
    };
    let offset = handles::integer_scalar(context, arguments.value(2));
    if offset == NA_INTEGER || offset < 1 {
        return Err(arguments.error("invalid 'offset' argument"));
    }
    let ignore_case = is_true(context, arguments.value(3));
    let fixed = is_true(context, arguments.value(4));
    let value = is_true(context, arguments.value(5));
    let all = is_true(context, arguments.value(6));
    let mut invert = is_true(context, arguments.value(7));
    if invert && !value {
        handles::warning(context, "argument 'invert = TRUE' will be ignored")?;
        invert = false;
    }
    let needle = pattern.as_slice(context).to_vec();
    let haystack = x.as_slice(context).to_vec();
    let start = offset as usize - 1;
    let regex = if fixed {
        if ignore_case {
            handles::warning(context, "argument 'ignore.case = TRUE' will be ignored")?;
        }
        None
    } else {
        let text = translate_byte_regex(&String::from_utf8_lossy(&needle));
        let flags = if ignore_case { "(?i-u)" } else { "(?-u)" };
        let compiled = regex::bytes::Regex::new(&format!("{flags}{text}")).map_err(|failure| {
            arguments.error(format!(
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
    if !value {
        let positions: Vec<i32> = matches.iter().map(|&(begin, _)| begin as i32 + 1).collect();
        return Ok(integers(context, &positions)?.finish().value());
    }
    let single = |context: &mut Cx<'s>, bytes: &[u8]| -> R<Local<'s>> {
        Ok(raw_vector(context, bytes)?.finish().value())
    };
    match (all, invert) {
        (false, false) => match matches.first() {
            Some(&(begin, end)) => single(context, &haystack[begin..end]),
            None => single(context, &[]),
        },
        (false, true) => match matches.first() {
            Some(&(begin, end)) => {
                single(context, &[&haystack[..begin], &haystack[end..]].concat())
            }
            None => single(context, &haystack),
        },
        (true, false) => {
            let mut result = context.alloc::<List>(matches.len())?;
            for (index, &(begin, end)) in matches.iter().enumerate() {
                let piece = raw_vector(context, &haystack[begin..end])?.finish().value();
                result.set(context, index, piece);
            }
            Ok(result.finish().value())
        }
        (true, true) => {
            let mut result = context.alloc::<List>(matches.len() + 1)?;
            let mut previous = 0;
            for (index, &(begin, end)) in matches.iter().enumerate() {
                let piece = raw_vector(context, &haystack[previous..begin])?
                    .finish()
                    .value();
                result.set(context, index, piece);
                previous = end;
            }
            let piece = raw_vector(context, &haystack[previous..])?.finish().value();
            result.set(context, matches.len(), piece);
            Ok(result.finish().value())
        }
    }
}

fn do_regexec<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let ignore_case = is_true(context, arguments.value(2));
    let fixed = is_true(context, arguments.value(3));
    let options = pattern_options(context, ignore_case, false, fixed)?;
    let pattern = handles::dense::<Str>(context, arguments.value(0))?;
    let text = handles::dense::<Str>(context, arguments.value(1))?;
    let use_bytes = is_true(context, arguments.value(4)) || has_bytes(context, &[pattern, text]);
    let pattern = pattern_argument(context, pattern)?;
    if handles::is_na(pattern.get(context, 0)) {
        return Err(error("invalid 'pattern' argument"));
    }
    let text = text_argument(context, arguments.value(1), text)?;
    let (compiled, report) = {
        let pattern = pattern_text(context, pattern, use_bytes);
        (
            Matcher::compile(&pattern, options, CompileErrors::Regexec),
            MatchReport::new(context, &pattern, text, use_bytes),
        )
    };
    let matcher = compiled.map_err(|failure| compile_failure(context, failure))?;
    let count = text.len();
    let mut result = context.alloc::<List>(count)?;
    for index in 0..count {
        if handles::is_na(text.get(context, index)) {
            let positions = context.scope(na_match)?;
            result.set(context, index, positions);
            continue;
        }
        let (starts, lengths): (Vec<i32>, Vec<i32>) = {
            let element = subject(text.get(context, index), use_bytes).unwrap_or_default();
            match matcher.captures_at(&element, 0)? {
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
            }
        };
        let positions = context.scope(|inner: &mut Cx<'_>| {
            let mut positions = integers(inner, &starts)?;
            let lengths = integers(inner, &lengths)?.finish().value();
            handles::set_attribute_named(inner, &mut positions, "match.length", lengths)?;
            report.annotate(inner, &mut positions)?;
            Ok::<_, Unwind>(positions.finish().value())
        })?;
        result.set(context, index, positions);
    }
    Ok(result.finish().value())
}

/// How match positions are reported: in characters, or in bytes when `useBytes = TRUE`. GNU R
/// also matches byte-wise, and says so in the `useBytes` attribute, when the pattern and every
/// text are ASCII; the attributes are left off otherwise.
struct MatchReport {
    bytes: bool,
    annotated: bool,
}

impl MatchReport {
    fn new(context: &Cx<'_>, pattern: &str, text: Local<'_, Str>, use_bytes: bool) -> MatchReport {
        let ascii = pattern.is_ascii()
            && text
                .iter(context)
                .all(|chars| handles::is_na(chars) || chars.bytes().is_ascii());
        MatchReport {
            bytes: use_bytes,
            annotated: use_bytes || ascii,
        }
    }

    fn annotate(&self, context: &mut Cx<'_>, vector: &mut Fresh<'_, Integer>) -> R<()> {
        if self.annotated {
            let mut index_type = context.alloc::<Str>(1)?;
            let kind = if self.bytes { "bytes" } else { "chars" };
            handles::set_text(context, &mut index_type, 0, kind)?;
            handles::set_attribute_named(
                context,
                vector,
                "index.type",
                index_type.finish().value(),
            )?;
            let use_bytes = handles::logical_constant(context, 1)?;
            handles::set_attribute_named(context, vector, "useBytes", use_bytes)?;
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

    fn finish<'s>(
        &self,
        context: &mut Cx<'s>,
        report: &MatchReport,
        names: &[String],
    ) -> R<Local<'s>> {
        let mut positions = integers(context, &self.starts)?;
        let lengths = integers(context, &self.lengths)?.finish().value();
        handles::set_attribute_named(context, &mut positions, "match.length", lengths)?;
        report.annotate(context, &mut positions)?;
        if self.groups > 0 {
            let mut column_names = context.alloc::<Str>(names.len())?;
            for (index, name) in names.iter().enumerate() {
                handles::set_text(context, &mut column_names, index, name)?;
            }
            let column_names = column_names.finish().value();
            let mut dimnames = context.alloc::<List>(2)?;
            dimnames.set(context, 1, column_names);
            let dimnames = dimnames.finish().value();
            let dim = integers(context, &[self.rows as i32, self.groups as i32])?
                .finish()
                .value();
            for (attribute, row_major) in [
                ("capture.start", &self.capture_starts),
                ("capture.length", &self.capture_lengths),
            ] {
                let mut matrix = context.alloc::<Integer>(self.rows * self.groups)?;
                let cells = matrix.as_mut_slice();
                for row in 0..self.rows {
                    for group in 0..self.groups {
                        cells[group * self.rows + row] = row_major[row * self.groups + group];
                    }
                }
                handles::set_attribute_named(context, &mut matrix, "dim", dim)?;
                handles::set_attribute_named(context, &mut matrix, "dimnames", dimnames)?;
                let matrix = matrix.finish().value();
                handles::set_attribute_named(context, &mut positions, attribute, matrix)?;
            }
            handles::set_attribute_named(context, &mut positions, "capture.names", column_names)?;
        }
        Ok(positions.finish().value())
    }
}

/// The result for an `NA` text in `gregexpr` and `regexec`: `NA` with only a match length.
fn na_match<'s>(context: &mut Cx<'s>) -> R<Local<'s>> {
    let mut positions = integers(context, &[NA_INTEGER])?;
    let lengths = integers(context, &[NA_INTEGER])?.finish().value();
    handles::set_attribute_named(context, &mut positions, "match.length", lengths)?;
    Ok(positions.finish().value())
}

/// A new integer vector holding `values`.
fn integers<'s>(context: &mut Cx<'s>, values: &[i32]) -> R<Fresh<'s, Integer>> {
    let mut vector = context.alloc::<Integer>(values.len())?;
    vector.as_mut_slice().copy_from_slice(values);
    Ok(vector)
}

/// A new raw vector holding `bytes`.
fn raw_vector<'s>(context: &mut Cx<'s>, bytes: &[u8]) -> R<Fresh<'s, Raw>> {
    let mut vector = context.alloc::<Raw>(bytes.len())?;
    vector.as_mut_slice().copy_from_slice(bytes);
    Ok(vector)
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
fn pattern_options(
    context: &mut Cx<'_>,
    ignore_case: bool,
    perl: bool,
    fixed: bool,
) -> R<PatternOptions> {
    for message in overridden_flags(ignore_case, perl, fixed) {
        handles::warning(context, message)?;
    }
    Ok(effective_options(ignore_case, perl, fixed))
}

/// The warnings for the flags `fixed = TRUE` overrides.
fn overridden_flags(
    ignore_case: bool,
    perl: bool,
    fixed: bool,
) -> impl Iterator<Item = &'static str> {
    [
        (fixed && ignore_case).then_some("argument 'ignore.case = TRUE' will be ignored"),
        (fixed && perl).then_some("argument 'perl = TRUE' will be ignored"),
    ]
    .into_iter()
    .flatten()
}

fn effective_options(ignore_case: bool, perl: bool, fixed: bool) -> PatternOptions {
    PatternOptions {
        ignore_case: ignore_case && !fixed,
        perl: perl && !fixed,
        fixed,
    }
}

/// Whether any string in `vectors` is marked as bytes. GNU R then matches every input byte by
/// byte, as if `useBytes = TRUE` had been given, because bytes have no characters to match.
fn has_bytes(context: &Cx<'_>, vectors: &[Option<Local<'_, Str>>]) -> bool {
    vectors.iter().flatten().any(|vector| {
        vector
            .iter(context)
            .any(|chars| handles::encoding(chars) == Encoding::Bytes)
    })
}

/// The bytes of a string built from matched text when it is marked as bytes: when bytes-marked
/// input forced byte matching, GNU R marks a result that is not ASCII as bytes too. `None`
/// means the string is made from the text as [`values::mkchar`] makes it.
fn regex_bytes(text: &str, forced_bytes: bool) -> Option<Cow<'_, [u8]>> {
    match (forced_bytes, values::unescape_invalid_bytes(text)) {
        (true, Some(bytes)) => Some(Cow::Owned(bytes)),
        (true, None) if !text.is_ascii() => Some(Cow::Borrowed(text.as_bytes())),
        _ => None,
    }
}

/// [`regex_bytes`] as a new string.
unsafe fn regex_result(text: &str, forced_bytes: bool) -> R<Sexp> {
    match regex_bytes(text, forced_bytes) {
        Some(bytes) => values::mkchar_bytes(&bytes, Encoding::Bytes),
        None => values::mkchar(text),
    }
}

/// [`regex_bytes`] stored as element `index` of `vector`.
fn set_regex_result(
    context: &mut Cx<'_>,
    vector: &mut Fresh<'_, Str>,
    index: usize,
    text: &str,
    forced_bytes: bool,
) -> R<()> {
    match regex_bytes(text, forced_bytes) {
        Some(bytes) => handles::set_bytes(context, vector, index, &bytes, Encoding::Bytes),
        None => handles::set_text(context, vector, index, text),
    }
}

/// The `pattern` argument, warning when it has more than the one element that is used.
fn pattern_argument<'s>(
    context: &mut Cx<'_>,
    pattern: Option<Local<'s, Str>>,
) -> R<Local<'s, Str>> {
    let pattern = match pattern {
        Some(pattern) if !pattern.is_empty() => pattern,
        _ => return Err(error("invalid 'pattern' argument")),
    };
    if pattern.len() > 1 {
        handles::warning(
            context,
            "argument 'pattern' has length > 1 and only the first element will be used",
        )?;
    }
    Ok(pattern)
}

/// The text a regular expression function searches: a character vector, with `NULL` as an
/// empty one.
fn text_argument<'s>(
    context: &mut Cx<'s>,
    value: Local<'s>,
    text: Option<Local<'s, Str>>,
) -> R<Local<'s, Str>> {
    match text {
        Some(text) => Ok(text),
        None => strings_or_null(context, value, "invalid 'text' argument"),
    }
}

/// `value` as a character vector when it is one, an empty one for `NULL`, and the error
/// `message` otherwise.
fn strings_or_null<'s>(context: &mut Cx<'s>, value: Local<'s>, message: &str) -> R<Local<'s, Str>> {
    if value.r_type() == SexpType::Nil {
        return Ok(context.alloc::<Str>(0)?.finish());
    }
    handles::dense::<Str>(context, value)?.ok_or_else(|| error(message))
}

/// The first element of a pattern, read as [`subject`] reads text, and empty for `NA`.
fn pattern_text<'a, 's: 'a>(
    context: &'a Cx<'_>,
    pattern: Local<'s, Str>,
    use_bytes: bool,
) -> Cow<'a, str> {
    subject(pattern.get(context, 0), use_bytes).unwrap_or_default()
}

/// A string as the regular expression engine sees it, as [`subject_at`] reads it.
fn subject(chars: Gc<'_, Chars>, use_bytes: bool) -> Option<Cow<'_, str>> {
    if handles::is_na(chars) {
        None
    } else if use_bytes {
        Some(values::escape_high_bytes(chars.bytes()))
    } else {
        Some(handles::text(chars))
    }
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
fn valid_subject(
    context: &mut Cx<'_>,
    text: Local<'_, Str>,
    index: usize,
    use_bytes: bool,
    options: PatternOptions,
    fatal: bool,
) -> R<bool> {
    let (translation, message) = {
        let chars = text.get(context, index);
        if use_bytes
            || handles::encoding(chars) != Encoding::Utf8
            || std::str::from_utf8(chars.bytes()).is_ok()
        {
            return Ok(true);
        }
        if options.fixed || options.perl {
            (None, format!("input string {} is invalid UTF-8", index + 1))
        } else {
            let mut shown = String::new();
            for chunk in chars.bytes().utf8_chunks() {
                shown.push_str(chunk.valid());
                for byte in chunk.invalid() {
                    shown.push_str(&format!("<{byte:02x}>"));
                }
            }
            (
                Some(format!("unable to translate '{shown}' to a wide string")),
                format!("input string {} is invalid", index + 1),
            )
        }
    };
    if let Some(translation) = translation {
        handles::warning(context, translation)?;
    }
    if fatal {
        return Err(error(message));
    }
    handles::warning(context, message)?;
    Ok(false)
}

/// The byte ranges of a match (group 0) and of its capture groups, `None` for a group that took
/// no part in the match.
type Groups = SmallVec<[Option<(usize, usize)>; 4]>;

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
    fn for_text(
        context: &Cx<'_>,
        pattern: &str,
        text: Local<'_, Str>,
        use_bytes: bool,
    ) -> CompileErrors {
        let wide = !use_bytes
            && (!pattern.is_ascii() || text.iter(context).any(|chars| !chars.bytes().is_ascii()));
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
    Literal(Rc<memmem::Finder<'static>>),
    /// `fresh_start` as in [`Translation`], here and in `Automaton`.
    Pattern { regex: Rc<Regex>, fresh_start: bool },
    /// A POSIX pattern without backreferences, which finite automata can match in linear time.
    /// `regex` (leftmost-longest) still serves capture groups, but `fancy-regex` runs every
    /// leftmost-longest pattern of variable width on its backtracker, which takes exponential
    /// time on patterns such as `(.*z.*){10}` that TRE matches instantly. The leftmost match
    /// starts where the leftmost-first match does, and `longest` (which reports every match end)
    /// searched anchored at that start finds where the longest one ends.
    ///
    /// A pattern that repeats minimally takes the earliest of those ends instead (`shortest`), or
    /// with groups the leftmost-first match, whose capture groups come from `first`.
    ///
    /// The automata are built on first use, since a `sub` or `gsub` whose replacement refers to
    /// groups only ever runs `regex`. A pattern they cannot express runs on `regex` alone.
    Automaton {
        regex: Rc<Regex>,
        automata: Rc<LazyAutomata>,
        shortest: bool,
        fresh_start: bool,
    },
    /// `perl = TRUE`, compiled by PCRE2 itself as GNU R does.
    Perl(Rc<PerlRegex>),
    /// A POSIX pattern with approximate settings such as `{+}`, run as TRE's approximate
    /// matcher runs it.
    Approximate(Rc<ApproximateRegex>),
}

/// The `first` and `longest` automata of [`Matcher::Automaton`], built from the translated
/// pattern the first time a search needs them.
struct LazyAutomata {
    pattern: String,
    ignore_case: bool,
    built: OnceCell<Option<Automata>>,
}

struct Automata {
    first: meta::Regex,
    longest: meta::Regex,
    /// Whether `first`'s capture groups are TRE's whenever its match spans as far as the
    /// longest one; see [`greedy_single_width`].
    greedy_groups: bool,
}

impl LazyAutomata {
    /// The automata, or `None` when the engine cannot build them for this pattern.
    fn get(&self) -> Option<&Automata> {
        self.built
            .get_or_init(|| {
                // The full DFA is built eagerly for every small pattern and was most of what
                // compiling one cost, about a third of a millisecond, while the lazy DFA it would
                // replace searches R's typically short strings about as fast.
                let build = |kind: MatchKind| {
                    meta::Regex::builder()
                        .configure(meta::Regex::config().match_kind(kind).dfa(false))
                        .syntax(syntax::Config::new().case_insensitive(self.ignore_case))
                        .build(&self.pattern)
                        .ok()
                };
                let greedy_groups = regex_syntax::ParserBuilder::new()
                    .case_insensitive(self.ignore_case)
                    .build()
                    .parse(&self.pattern)
                    .is_ok_and(|hir| greedy_single_width(&hir));
                Some(Automata {
                    first: build(MatchKind::LeftmostFirst)?,
                    longest: build(MatchKind::All)?,
                    greedy_groups,
                })
            })
            .as_ref()
    }
}

/// Whether every repetition in `hir` is greedy and repeats a single character, with no
/// alternation anywhere. A backtracking match of such a pattern makes each repetition, in
/// pattern order, as long as the rest of the pattern allows, so when it ends where the longest
/// match ends its groups are the POSIX ones TRE reports. A repeated group or an alternation
/// breaks this: `(a)?(ab)?b*` on `"abb"` spans the same text either way, but TRE gives the
/// second group `"ab"` and a backtracker gives the first one `"a"`.
fn greedy_single_width(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::HirKind;
    match hir.kind() {
        HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) | HirKind::Look(_) => true,
        HirKind::Repetition(repetition) => {
            repetition.greedy
                && match repetition.sub.kind() {
                    HirKind::Class(_) => true,
                    HirKind::Literal(literal) => {
                        std::str::from_utf8(&literal.0).is_ok_and(|text| text.chars().count() == 1)
                    }
                    _ => false,
                }
        }
        HirKind::Capture(capture) => greedy_single_width(&capture.sub),
        HirKind::Concat(items) => items.iter().all(greedy_single_width),
        HirKind::Alternation(_) => false,
    }
}

/// The groups of the leftmost match from `first`, or `None` (inner) when there is no match;
/// `None` (outer) when the match ends before the longest one does, so only the backtracker
/// can tell TRE's groups.
fn greedy_captures(
    first: &meta::Regex,
    longest: &meta::Regex,
    text: &str,
    from: usize,
) -> Option<Option<Groups>> {
    let mut captures = first.create_captures();
    first.search_captures(&Input::new(text).range(from..), &mut captures);
    let Some(found) = captures.get_match() else {
        return Some(None);
    };
    let anchored = Input::new(text)
        .range(found.start()..)
        .anchored(Anchored::Yes);
    let end = longest
        .search(&anchored)
        .map_or(found.end(), |longest| longest.end());
    (end == found.end()).then(|| {
        Some(
            (0..captures.group_len())
                .map(|group| captures.get_group(group).map(|span| (span.start, span.end)))
                .collect(),
        )
    })
}

/// How many compiled patterns [`Matcher::compile`] keeps. R code calls `grepl` and `sub` with the
/// same few patterns over and over, and building the automaton costs far more than matching one
/// short string.
const PATTERN_CACHE_SIZE: usize = 256;

thread_local! {
    /// Compiled patterns by text, with a slot for `fixed = TRUE` and one per combination of
    /// `ignore.case` and `perl`, so a lookup borrows the pattern instead of building a key.
    static PATTERN_CACHE: RefCell<FxHashMap<Box<str>, [Option<Matcher>; 5]>> = RefCell::default();
}

impl Matcher {
    fn compile(
        pattern: &str,
        options: PatternOptions,
        errors: CompileErrors,
    ) -> Result<Matcher, CompileFailure> {
        let slot = if options.fixed {
            4
        } else {
            usize::from(options.ignore_case) | usize::from(options.perl) << 1
        };
        let key = (pattern, slot);
        if let Some(matcher) = PATTERN_CACHE.with_borrow(|cache| cache.get(pattern)?[slot].clone())
        {
            return Ok(matcher);
        }
        if options.fixed {
            let matcher = Matcher::Literal(Rc::new(memmem::Finder::new(pattern).into_owned()));
            remember(key, &matcher);
            return Ok(matcher);
        }
        if options.perl {
            let matcher = compile_perl(pattern, options.ignore_case)?;
            remember(key, &matcher);
            return Ok(matcher);
        }
        let translated = translate(pattern);
        let failure = |reason: String| -> CompileFailure {
            match errors {
                CompileErrors::Quoted => CompileFailure {
                    warning: Some(format!("TRE pattern compilation error '{reason}'")),
                    error: error(format!(
                        "invalid regular expression '{pattern}', reason '{reason}'"
                    )),
                },
                CompileErrors::Wide => CompileFailure::error(error(format!(
                    "invalid regular expression, reason '{reason}'"
                ))),
                CompileErrors::Regexec => {
                    CompileFailure::error(error(format!("regcomp error: '{reason}'")))
                }
            }
        };
        if !translated.scopes.is_empty() && translated.problem.is_none() {
            let regex = ApproximateRegex::compile(
                &translated.pattern,
                &translated.scopes,
                options.ignore_case,
            )
            .map_err(failure)?;
            let matcher = Matcher::Approximate(Rc::new(regex));
            remember(key, &matcher);
            return Ok(matcher);
        }
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
        let (compiled, longest) = match build(true) {
            Ok(regex) => (Ok(regex), true),
            Err(_) => (build(false), false),
        };
        let matcher = match compiled {
            Ok(regex) if longest || translated.minimal => Matcher::Automaton {
                regex: Rc::new(regex),
                automata: Rc::new(LazyAutomata {
                    pattern: translated.pattern,
                    ignore_case: options.ignore_case,
                    built: OnceCell::new(),
                }),
                shortest: translated.minimal,
                fresh_start: translated.fresh_start,
            },
            Ok(regex) => Matcher::Pattern {
                regex: Rc::new(regex),
                fresh_start: translated.fresh_start,
            },
            Err(reason) => return Err(failure(reason)),
        };
        remember(key, &matcher);
        Ok(matcher)
    }

    /// The first match at or after byte `from`, as a byte range.
    fn find_at(&self, text: &str, from: usize) -> R<Option<(usize, usize)>> {
        if from > 0 && self.fresh_start() {
            let found = self.find_at(&text[from..], 0)?;
            return Ok(found.map(|(start, end)| (start + from, end + from)));
        }
        match self {
            Matcher::Literal(finder) => {
                let haystack = &text.as_bytes()[from..];
                let needle = finder.needle();
                // R strings are mostly short, where the searcher's setup outweighs its scan and
                // comparing at each candidate first byte is several times faster.
                let start = if haystack.len() < 64 {
                    match needle.split_first() {
                        Some((&first, rest)) => {
                            haystack.iter().enumerate().position(|(start, &byte)| {
                                byte == first
                                    && haystack
                                        .get(start + 1..start + needle.len())
                                        .is_some_and(|tail| tail == rest)
                            })
                        }
                        None => Some(0),
                    }
                } else {
                    finder.find(haystack)
                };
                Ok(start.map(|start| (from + start, from + start + needle.len())))
            }
            Matcher::Pattern { regex, .. } => regex
                .find_from_pos(text, from)
                .map(|found| found.map(|found| (found.start(), found.end())))
                .map_err(matching_error),
            Matcher::Automaton {
                regex,
                automata,
                shortest,
                ..
            } => {
                let Some(Automata { first, longest, .. }) = automata.get() else {
                    return regex
                        .find_from_pos(text, from)
                        .map(|found| found.map(|found| (found.start(), found.end())))
                        .map_err(matching_error);
                };
                let Some(found) = first.search(&Input::new(text).range(from..)) else {
                    return Ok(None);
                };
                let anchored = Input::new(text)
                    .range(found.start()..)
                    .anchored(Anchored::Yes)
                    .earliest(*shortest);
                let end = longest
                    .search(&anchored)
                    .map_or(found.end(), |longest| longest.end());
                Ok(Some((found.start(), end)))
            }
            Matcher::Perl(regex) => regex.find_at(text, from).map_err(perl_matching_error),
            Matcher::Approximate(regex) => Ok(approximate_search(regex, text, from)?
                .map(|groups| groups[0].unwrap_or((from, from)))),
        }
    }

    /// The first match at or after byte `from` with its capture groups; group 0 is the match.
    fn captures_at(&self, text: &str, from: usize) -> R<Option<Groups>> {
        if from > 0 && self.fresh_start() {
            let groups = self.captures_at(&text[from..], 0)?;
            return Ok(groups.map(|groups| {
                groups
                    .into_iter()
                    .map(|group| group.map(|(start, end)| (start + from, end + from)))
                    .collect()
            }));
        }
        match self {
            Matcher::Literal(_) => Ok(self.find_at(text, from)?.map(|span| smallvec![Some(span)])),
            Matcher::Approximate(regex) => approximate_search(regex, text, from),
            Matcher::Automaton { regex, .. } if regex.captures_len() == 1 => {
                Ok(self.find_at(text, from)?.map(|span| smallvec![Some(span)]))
            }
            Matcher::Automaton {
                automata,
                shortest: true,
                ..
            } if let Some(Automata { first, .. }) = automata.get() => {
                // Only a plain find minimizes over the top-level alternatives. With submatches
                // GNU R's TRE keeps the leftmost-first match: `sub("(a).*?Z|(a)(.*?)Y", ...)`
                // takes the first alternative even though the second ends sooner.
                let mut captures = first.create_captures();
                first.search_captures(&Input::new(text).range(from..), &mut captures);
                if !captures.is_match() {
                    return Ok(None);
                }
                Ok(Some(
                    (0..captures.group_len())
                        .map(|group| captures.get_group(group).map(|span| (span.start, span.end)))
                        .collect(),
                ))
            }
            Matcher::Automaton {
                automata,
                shortest: false,
                ..
            } if let Some(Automata {
                first,
                longest,
                greedy_groups: true,
            }) = automata.get()
                && let Some(groups) = greedy_captures(first, longest, text, from) =>
            {
                Ok(groups)
            }
            Matcher::Pattern { regex, .. } | Matcher::Automaton { regex, .. } => {
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
            Matcher::Perl(regex) => regex.captures_at(text, from).map_err(perl_matching_error),
        }
    }

    fn fresh_start(&self) -> bool {
        match self {
            Matcher::Pattern { fresh_start, .. } | Matcher::Automaton { fresh_start, .. } => {
                *fresh_start
            }
            Matcher::Literal(_) | Matcher::Perl(_) | Matcher::Approximate(_) => false,
        }
    }

    /// The number of capture groups, not counting the whole match.
    fn group_count(&self) -> usize {
        match self {
            Matcher::Literal(_) => 0,
            Matcher::Pattern { regex, .. } | Matcher::Automaton { regex, .. } => {
                regex.captures_len().saturating_sub(1)
            }
            Matcher::Perl(regex) => regex.groups().saturating_sub(1),
            Matcher::Approximate(regex) => regex.group_count(),
        }
    }

    /// The capture groups' names, with `""` for an unnamed group.
    fn group_names(&self) -> Vec<String> {
        match self {
            Matcher::Literal(_) => Vec::new(),
            Matcher::Pattern { regex, .. } | Matcher::Automaton { regex, .. } => regex
                .capture_names()
                .skip(1)
                .map(|name| name.unwrap_or_default().to_owned())
                .collect(),
            Matcher::Perl(regex) => regex
                .names()
                .iter()
                .skip(1)
                .map(|name| name.clone().unwrap_or_default())
                .collect(),
            Matcher::Approximate(regex) => vec![String::new(); regex.group_count()],
        }
    }
}

/// Searches `text` from byte `from` with an approximate pattern, and returns the match and its
/// groups as byte ranges. TRE gets only the rest of the text for every search after the first,
/// and with `REG_NOTBOL`, so `^` cannot match there and `\b` sees no character before it.
fn approximate_search(regex: &ApproximateRegex, text: &str, from: usize) -> R<Option<Groups>> {
    let rest = &text[from..];
    let characters: Vec<char> = rest.chars().collect();
    let Some((start, end, groups)): Option<ApproximateMatch> =
        regex.search(&characters, from > 0)?
    else {
        return Ok(None);
    };
    let mut offsets: Vec<usize> = rest
        .char_indices()
        .map(|(offset, _)| from + offset)
        .collect();
    offsets.push(text.len());
    let span = |(start, end): (usize, usize)| (offsets[start], offsets[end]);
    Ok(Some(
        std::iter::once(Some(span((start, end))))
            .chain(groups.into_iter().map(|group| group.map(span)))
            .collect(),
    ))
}

fn remember((pattern, slot): (&str, usize), matcher: &Matcher) {
    PATTERN_CACHE.with_borrow_mut(|cache| {
        if let Some(slots) = cache.get_mut(pattern) {
            slots[slot] = Some(matcher.clone());
            return;
        }
        if cache.len() >= PATTERN_CACHE_SIZE {
            cache.clear();
        }
        let mut slots: [Option<Matcher>; 5] = Default::default();
        slots[slot] = Some(matcher.clone());
        cache.insert(pattern.into(), slots);
    });
}

/// Compiles a `perl = TRUE` pattern as GNU R's `R_pcre2_prepare` does: UTF mode, caseless when
/// asked, and JIT-compiled when PCRE2 supports it. Text is always valid UTF-8 here, so UTF mode
/// is right even for ASCII input, where it makes no difference.
fn compile_perl(pattern: &str, ignore_case: bool) -> Result<Matcher, CompileFailure> {
    match PerlRegex::new(pattern, ignore_case) {
        Ok(regex) => Ok(Matcher::Perl(Rc::new(regex))),
        Err(CompileError { message, offset }) => {
            // GNU R prints the message and the rest of the pattern from the error's byte offset.
            let rest = pattern.get(offset..).unwrap_or_default();
            Err(CompileFailure {
                warning: Some(format!(
                    "PCRE pattern compilation error\n\t'{message}'\n\tat '{rest}'"
                )),
                error: error(format!("invalid regular expression '{pattern}'")),
            })
        }
    }
}

fn matching_error(failure: fancy_regex::Error) -> crate::unwind::Unwind {
    error(format!("regular expression matching failed: {failure}"))
}

fn perl_matching_error(failure: String) -> crate::unwind::Unwind {
    error(format!("regular expression matching failed: {failure}"))
}

/// A translated pattern, with the error TRE reports for a pattern it rejects but the engine
/// would accept.
pub(crate) struct Translation {
    pub(crate) pattern: String,
    pub(crate) problem: Option<&'static str>,
    /// Whether a POSIX pattern repeats minimally (`*?`, `{1,3}?`) outside every capture group,
    /// with no capture group opening after it. TRE then ends a match at the first end it
    /// reaches, so the whole match is the shortest one at the leftmost start, and even a greedy
    /// `.*` in such a pattern matches as little as it can: `a.*?b.*` and `(a).*?b.*` match `aXb`
    /// of `aXbXb`. A minimal repetition inside a capture group, or followed by one, runs on
    /// TRE's tagged matcher, which follows the leftmost-first rule: `a(.*?)b.*` and `a.*?(b).*`
    /// match all of it.
    minimal: bool,
    /// Whether every search after the first should see its start as the start of the text, which
    /// is how TRE runs the follow-on searches of `gsub` and `gregexpr`: `\b`, `\B`, `\<` and `\>`
    /// then see no character before that start (`gsub("\\b", "|", "ab")` is `"|a|b|"`), while
    /// `^` still cannot match there. Only patterns without `^` qualify, since searching from a
    /// fresh start would let `^` match too.
    fresh_start: bool,
    /// The settings of each approximate scope, such as the `{+}` of `a{+}`, which the pattern
    /// writes as a group named [`SCOPE_GROUP`] and the scope's number. A pattern with scopes
    /// runs on [`ApproximateRegex`], as TRE runs it on its approximate matcher.
    pub(crate) scopes: Vec<ApproximateSettings>,
}

pub(crate) fn translate(pattern: &str) -> Translation {
    translate_classes(pattern, true)
}

/// A pattern for `grepRaw`, which GNU R compiles with TRE's byte interface, so its classes
/// cover ASCII only.
pub fn translate_byte_regex(pattern: &str) -> String {
    translate_classes(pattern, false).pattern
}

/// Translates a POSIX pattern whose classes (`\s`, `[[:space:]]`) follow Unicode, or only ASCII.
///
/// TRE runs a pattern with approximate settings on its approximate matcher, unless the pattern
/// has backreferences, which only its backtracking matcher supports and which ignores the
/// settings. The first pass finds out, and a pattern for the approximate matcher is translated
/// again with its scopes.
fn translate_classes(pattern: &str, unicode: bool) -> Translation {
    let chars: Vec<char> = pattern.chars().collect();
    let (translation, approximate) = translate_pass(&chars, unicode, false);
    if approximate {
        translate_pass(&chars, unicode, true).0
    } else {
        translation
    }
}

/// One translation of `chars`, with approximate scopes when `approximate`, and whether the
/// pattern needs TRE's approximate matcher.
fn translate_pass(chars: &[char], unicode: bool, approximate: bool) -> (Translation, bool) {
    let mut problem = None;
    let mut minimal = false;
    let mut word_assertion = false;
    let mut start_anchor = false;
    let mut settings_seen = false;
    let mut backreference = false;
    let mut scopes = Vec::new();
    let mut output = String::with_capacity(chars.len() + 8);
    // In POSIX syntax `.` matches a newline too.
    output.push_str("(?s)");
    // Where each open group starts in `output`, and whether it captures.
    let mut groups: Vec<(usize, bool)> = Vec::new();
    // Where the latest atom starts in `output`, while a quantifier may still apply to it.
    let mut atom: Option<usize> = None;
    let mut quantified = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        let start = output.len();
        match character {
            // TRE reads `\Q...\E` as literal text, each character its own atom, so a quantifier
            // after `\E` repeats the last one.
            '\\' if chars.get(index + 1) == Some(&'Q') => {
                index += 2;
                atom = None;
                while index < chars.len() {
                    if chars[index] == '\\' && chars.get(index + 1) == Some(&'E') {
                        index += 2;
                        break;
                    }
                    atom = Some(output.len());
                    output.push_str(&regex::escape(chars[index].encode_utf8(&mut [0; 4])));
                    index += 1;
                }
                quantified = false;
                continue;
            }
            '\\' => {
                word_assertion |= matches!(chars.get(index + 1), Some('b' | 'B' | '<' | '>'));
                backreference |= matches!(chars.get(index + 1), Some('1'..='9'));
                index = translate_escape(
                    chars,
                    index,
                    unicode,
                    approximate,
                    &mut output,
                    &mut problem,
                );
                atom = Some(start);
                quantified = false;
                continue;
            }
            '[' => {
                index = translate_class(chars, index, unicode, &mut output, &mut problem);
                atom = Some(start);
                quantified = false;
                continue;
            }
            // TRE reads `(?:...)` as a group that does not capture, `(?i)` or `(?i:...)` as a
            // case-insensitive region and `(?#...)` as a comment, as PCRE does, and rejects
            // every other `(?` construct.
            '(' if chars.get(index + 1) == Some(&'?') => {
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
            '(' => {
                groups.push((start, true));
                minimal = false;
                output.push('(');
                atom = None;
                quantified = false;
            }
            // TRE takes a `)` that closes no group as a literal.
            ')' => {
                match groups.pop() {
                    Some((group_start, _)) => {
                        output.push(')');
                        atom = Some(group_start);
                    }
                    None => {
                        output.push_str("\\)");
                        atom = Some(start);
                    }
                }
                quantified = false;
            }
            '|' => {
                output.push('|');
                atom = None;
            }
            '*' | '+' | '?' => match atom {
                // TRE drops a repetition operator with nothing to repeat, at the start of the
                // pattern or right after `(`, `|` or `^`: `grepl("+ab", "xab")` is `TRUE`.
                None => {}
                Some(_) => {
                    output.push(character);
                    quantified = true;
                    if chars.get(index + 1) == Some(&'?') {
                        if !approximate {
                            minimal |= !groups.iter().any(|&(_, capturing)| capturing);
                            output.push('?');
                        }
                        index += 1;
                    }
                }
            },
            '{' => {
                let bound = match parse_bound(chars, index) {
                    Ok(bound) => bound,
                    Err(reason) => {
                        // TRE stops at the first error, which is the one it reports.
                        problem.get_or_insert(reason);
                        break;
                    }
                };
                index = bound.end;
                let repetition = match bound.repetition() {
                    Ok(repetition) => repetition,
                    Err(reason) => {
                        problem.get_or_insert(reason);
                        break;
                    }
                };
                let Some(repetition) = repetition else {
                    // `{0}` and `{0,0}` match the empty string, settings and all.
                    if let Some(atom_start) = atom {
                        output.truncate(atom_start);
                    }
                    atom = None;
                    quantified = false;
                    continue;
                };
                settings_seen |= bound.settings.is_some();
                match (bound.settings, atom) {
                    (Some(settings), atom_start) if approximate => {
                        // A bound with nothing before it repeats the empty string, which still
                        // opens a scope that counts toward TRE's nesting.
                        let body = atom_start.map(|start| output.split_off(start));
                        let scope = scopes.len();
                        scopes.push(settings);
                        output.push_str(&format!("(?P<{SCOPE_GROUP}{scope}>"));
                        if let Some(body) = body {
                            output.push_str(&format!("(?:{body}){repetition}"));
                        }
                        output.push(')');
                        quantified = true;
                    }
                    (_, Some(atom_start)) => {
                        if quantified {
                            // `x{2}{3}` repeats the repetition, which the `regex` crate only
                            // accepts with an explicit group.
                            output.insert_str(atom_start, "(?:");
                            output.push(')');
                        }
                        output.push_str(&repetition);
                        quantified = true;
                        if bound.minimal && !approximate {
                            minimal |= !groups.iter().any(|&(_, capturing)| capturing);
                            output.push('?');
                        }
                    }
                    (_, None) => {}
                }
                continue;
            }
            '}' | ']' => {
                output.push('\\');
                output.push(character);
                atom = Some(start);
                quantified = false;
            }
            '^' | '$' => {
                start_anchor |= character == '^';
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
    let translation = Translation {
        pattern: output,
        problem,
        minimal,
        fresh_start: word_assertion && !start_anchor,
        scopes,
    };
    (translation, settings_seen && !backreference)
}

/// What TRE's `\s` and `[[:space:]]` match: glibc's `iswspace`, which leaves out the
/// non-breaking spaces.
const POSIX_SPACE: &str =
    r"\t\n\x0B\f\r \x{1680}\x{2000}-\x{2006}\x{2008}-\x{200A}\x{2028}\x{2029}\x{205F}\x{3000}";
const POSIX_BLANK: &str = r"\t \x{1680}\x{2000}-\x{2006}\x{2008}-\x{200A}\x{205F}\x{3000}";
const WORD: &str = r"\p{Alphabetic}\p{Nd}_";
/// TRE's `\d` is ASCII even though its other classes follow the locale.
const DIGIT: &str = "0-9";
const ASCII_WORD: &str = "0-9A-Za-z_";
const ASCII_SPACE: &str = r"\t\n\x0B\f\r ";

/// Translates the escape at `index` (outside a bracket expression) and returns the index after
/// it.
fn translate_escape(
    chars: &[char],
    index: usize,
    unicode: bool,
    approximate: bool,
    output: &mut String,
    problem: &mut Option<&'static str>,
) -> usize {
    let Some(&escaped) = chars.get(index + 1) else {
        problem.get_or_insert("Trailing backslash");
        // The compiler rejects a trailing backslash too.
        output.push('\\');
        return index + 1;
    };
    let next = index + 2;
    if let Some(class) = escape_class(escaped, unicode) {
        output.push('[');
        output.push_str(&class);
        output.push(']');
        return next;
    }
    match escaped {
        // The approximate matcher checks `\b` and `\B` as TRE does.
        'b' | 'B' if approximate => {
            output.push('\\');
            output.push(escaped);
            next
        }
        // TRE's `\b` matches at the start and the end of the subject whatever the characters
        // there are, so `regexpr("\\b", " ")` is 1.
        'b' => {
            output.push_str(r"(?:\A|\z|\b)");
            next
        }
        // Its `\B` never matches at either end.
        'B' => {
            output.push_str(r"(?:(?!\A)(?!\z)\B)");
            next
        }
        'n' | 't' | 'r' | 'f' | 'v' | '1'..='9' | '<' | '>' => {
            output.push('\\');
            output.push(escaped);
            next
        }
        'e' => {
            output.push_str(r"\x1B");
            next
        }
        'a' => {
            output.push_str(r"\x07");
            next
        }
        'x' => {
            let (point, end) = hex_escape(chars, next);
            output.push_str(&format!(r"\x{{{point:X}}}"));
            end
        }
        // TRE quotes `\Q...\E` as PCRE does.
        'Q' => {
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
        character => {
            output.push_str(&regex::escape(character.encode_utf8(&mut [0; 4])));
            next
        }
    }
}

/// The class contents an escape such as `\d` stands for, written to sit inside brackets.
fn escape_class(escaped: char, unicode: bool) -> Option<String> {
    let contents = match (escaped.to_ascii_lowercase(), unicode) {
        ('d', _) => DIGIT,
        ('w', true) => WORD,
        ('w', false) => ASCII_WORD,
        ('s', true) => POSIX_SPACE,
        ('s', false) => ASCII_SPACE,
        _ => return None,
    };
    Some(if escaped.is_ascii_uppercase() {
        format!("[^{contents}]")
    } else {
        contents.to_owned()
    })
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

/// Translates a bracket expression starting at `index` and returns the index after it. An
/// unterminated one is left unterminated, so that compiling it fails.
fn translate_class(
    chars: &[char],
    index: usize,
    unicode: bool,
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
        let (item, next) = class_item(chars, cursor, unicode, problem);
        let range_end = match item {
            ClassItem::Literal(_) if chars.get(next) == Some(&'-') => chars
                .get(next + 1)
                .filter(|&&end| end != ']')
                .map(|_| class_item(chars, next + 1, unicode, problem)),
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
    problem.get_or_insert("Missing ']'");
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
    unicode: bool,
    problem: &mut Option<&'static str>,
) -> (ClassItem, usize) {
    let character = chars[index];
    if character == '['
        && let Some((kind, name, end)) = bracket_term(chars, index)
    {
        let set = match (kind, posix_class(&name, unicode)) {
            (':', Some(set)) => set,
            (':', None) => {
                problem.get_or_insert("Unknown character class name");
                String::new()
            }
            // TRE supports neither equivalence classes nor collating symbols.
            _ => {
                problem.get_or_insert("Unknown collating element");
                String::new()
            }
        };
        return (ClassItem::Set(set), end);
    }
    // In POSIX brackets a backslash is an ordinary character.
    (ClassItem::Literal(character), index + 1)
}

/// Reads `[:name:]`, `[=c=]` or `[.c.]` at `index`: the delimiter, the name, and the index
/// after the term.
///
/// TRE ends a class name at the first `:` and skips the character after it without checking
/// that it is `]`, so `[[:lower::]]+` reads as `[[:lower:]]]+`. GNU R's own `install.packages`
/// relies on this, with exactly that pattern.
fn bracket_term(chars: &[char], index: usize) -> Option<(char, String, usize)> {
    let kind = *chars.get(index + 1)?;
    if !matches!(kind, ':' | '=' | '.') {
        return None;
    }
    let mut cursor = index + 2;
    let mut name = String::new();
    while cursor + 1 < chars.len() {
        if chars[cursor] == kind && (kind == ':' || chars[cursor + 1] == ']') {
            return Some((kind, name, cursor + 2));
        }
        name.push(chars[cursor]);
        cursor += 1;
    }
    None
}

/// The class contents for `[:name:]`, or `None` for an unknown name. The classes follow the
/// Unicode properties, as glibc's do in a UTF-8 locale, or are ASCII, which the `regex`
/// crate's own `[:name:]` classes match.
fn posix_class(name: &str, unicode: bool) -> Option<String> {
    const NAMES: &[&str] = &[
        "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
        "upper", "xdigit", "word",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    if !unicode {
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
            _ => WORD,
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

/// A bound `{...}` as TRE reads it: a repetition count, approximate-matching settings, or both.
struct Bound {
    /// The counts, `-1` where the bound leaves one out.
    minimum: i64,
    maximum: i64,
    settings: Option<ApproximateSettings>,
    /// Whether a `?` follows, asking for minimal repetition.
    minimal: bool,
    /// The index after the bound and its `?`.
    end: usize,
}

const INVALID_BOUND: &str = "Invalid contents of {}";
const MISSING_BRACE: &str = "Missing '}'";
const INVALID_REPETITION: &str = "Invalid use of repetition operators";
/// TRE's `RE_DUP_MAX`.
const MOST_REPEATS: i64 = 255;

impl Bound {
    /// The quantifier in the `regex` crate's syntax, or `None` when the bound matches only the
    /// empty string. TRE expands a bound into copies of its atom, and two quirks of that
    /// expansion decide what some bounds mean: `{,n}` makes `n + 1` optional copies (and `{,1}`
    /// exactly one), and an approximate bound that needs copies (`{2+}`, `{1,2~}`) is rejected,
    /// because its settings end up attached to the copies' sequence instead of a repetition.
    fn repetition(&self) -> Result<Option<String>, &'static str> {
        let (minimum, maximum) = match (self.minimum, self.maximum) {
            (0, 0) => return Ok(None),
            (minimum, maximum) if minimum < 0 && maximum < 0 => (1, 1),
            counts => counts,
        };
        if self.settings.is_some() && (minimum > 1 || maximum > 1) {
            return Err(INVALID_BOUND);
        }
        Ok(Some(match (minimum, maximum) {
            (-1, 0) => return Err(INVALID_BOUND),
            (-1, 1) => "{1}".to_owned(),
            (-1, maximum) => format!("{{0,{}}}", maximum + 1),
            (minimum, -1) => format!("{{{minimum},}}"),
            (minimum, maximum) if minimum == maximum => format!("{{{minimum}}}"),
            (minimum, maximum) => format!("{{{minimum},{maximum}}}"),
        }))
    }
}

/// Reads the bound whose `{` is at `open`, as TRE's `tre_parse_bound` does. After the counts
/// come the settings: `+`, `-`, `#` and `~` with an optional number limit insertions,
/// deletions, substitutions and all edits (no number means no limit), and an equation such as
/// `2i + 1d < 5` sets the costs of insertions, deletions and substitutions and keeps the total
/// below a limit. Commas and spaces between the settings are skipped.
fn parse_bound(chars: &[char], open: usize) -> Result<Bound, &'static str> {
    let end = chars.len();
    let contents = open + 1;
    let mut cursor = contents;
    let mut minimum = -1;
    if chars.get(cursor).is_some_and(char::is_ascii_digit) {
        minimum = tre_integer(chars, &mut cursor);
    }
    let mut maximum = minimum;
    if chars.get(cursor) == Some(&',') {
        cursor += 1;
        maximum = tre_integer(chars, &mut cursor);
    }
    if (maximum >= 0 && minimum > maximum) || maximum > MOST_REPEATS {
        return Err(INVALID_BOUND);
    }
    let unlimited = |limit: i64| if limit < 0 { UNBOUNDED } else { limit };
    let (mut insertion_cost, mut deletion_cost, mut substitution_cost) = (None, None, None);
    let (mut insertions, mut deletions, mut substitutions) = (None, None, None);
    let (mut cost_limit, mut edits) = (None, None);
    let (mut approximate, mut counts_set, mut costs_set) = (false, false, false);
    loop {
        let start = cursor;
        // Like TRE, each run of counts or costs is read once, and both stop one character
        // before the end of the pattern.
        let counts_read = counts_set;
        while !counts_read && cursor + 1 < end {
            let operator = chars[cursor];
            let limit = match operator {
                '+' => &mut insertions,
                '-' => &mut deletions,
                '#' => &mut substitutions,
                '~' => &mut edits,
                ',' | ' ' => {
                    cursor += 1;
                    continue;
                }
                _ => break,
            };
            cursor += 1;
            *limit = Some(unlimited(tre_integer(chars, &mut cursor)));
            if operator == '~' {
                approximate = true;
            } else {
                counts_set = true;
            }
        }
        let costs_read = costs_set;
        while !costs_read && cursor + 1 < end {
            match chars[cursor] {
                '+' | ' ' => cursor += 1,
                '<' => {
                    cursor += 1;
                    while chars.get(cursor) == Some(&' ') {
                        cursor += 1;
                    }
                    let limit = tre_integer(chars, &mut cursor);
                    cost_limit = Some(if limit < 0 { UNBOUNDED } else { limit - 1 });
                    approximate = true;
                }
                ',' => {
                    cursor += 1;
                    break;
                }
                character if character.is_ascii_digit() => {
                    let cost = tre_integer(chars, &mut cursor);
                    let Some(&kind) = chars.get(cursor) else {
                        return Err(MISSING_BRACE);
                    };
                    *match kind {
                        'i' => &mut insertion_cost,
                        'd' => &mut deletion_cost,
                        's' => &mut substitution_cost,
                        _ => return Err(INVALID_BOUND),
                    } = Some(cost);
                    cursor += 1;
                    costs_set = true;
                }
                _ => break,
            }
        }
        if cursor == start {
            break;
        }
    }
    if cursor >= end {
        return Err(MISSING_BRACE);
    }
    if cursor == contents || chars[cursor] != '}' {
        return Err(INVALID_BOUND);
    }
    cursor += 1;
    let minimal = match chars.get(cursor) {
        Some('?') => {
            cursor += 1;
            true
        }
        Some('*' | '+') => return Err(INVALID_REPETITION),
        _ => false,
    };
    let settings = (approximate || costs_set || counts_set).then(|| {
        // Once a count or a cost is given, an edit with neither is not allowed at all.
        let limit = |limit: Option<i64>, cost: Option<i64>| {
            if !(costs_set || counts_set) || limit.is_some() {
                limit
            } else if cost.is_some() {
                Some(UNBOUNDED)
            } else {
                Some(0)
            }
        };
        ApproximateSettings {
            insertion_cost,
            deletion_cost,
            substitution_cost,
            cost_limit: cost_limit.unwrap_or(UNBOUNDED),
            insertions: limit(insertions, insertion_cost),
            deletions: limit(deletions, deletion_cost),
            substitutions: limit(substitutions, substitution_cost),
            edits: edits.unwrap_or(UNBOUNDED),
        }
    });
    Ok(Bound {
        minimum,
        maximum,
        settings,
        minimal,
        end: cursor,
    })
}

/// TRE's `tre_parse_int`: the decimal number at `cursor`, or `-1` when there are no digits or
/// the number does not fit in an `int`.
fn tre_integer(chars: &[char], cursor: &mut usize) -> i64 {
    let mut number: i64 = -1;
    let mut overflow = false;
    while let Some(digit) = chars
        .get(*cursor)
        .and_then(|character| character.to_digit(10))
    {
        if number < 0 {
            number = 0;
        }
        if number <= (UNBOUNDED - 9) / 10 {
            number = number * 10 + i64::from(digit);
        } else {
            overflow = true;
        }
        *cursor += 1;
    }
    if overflow { -1 } else { number }
}

#[inline]
pub(crate) unsafe fn flag(value: Sexp) -> bool {
    values::as_logical_scalar(value) == 1
}

/// `as.character(value)`, dispatching for objects, as `paste` and `sprintf` convert their
/// arguments. A classed character vector is already text and is used as it is, which matters
/// for vctrs, whose `as.character` method refuses a class with no cast to character. Only that
/// dispatch needs the caller's environment, so `env` is asked for then, and a slot frame keeps
/// its slots otherwise.
unsafe fn as_character(value: Sexp, env: impl FnOnce() -> R<Sexp>) -> R<Sexp> {
    if value.is_object() && value.kind() != SexpType::String {
        let function = values::install("as.character")?;
        let call = values::call(function, &[eval::quote_value(value)?])?;
        let converted = eval::eval(call, env()?)?;
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

/// Whether a flag argument is `TRUE`.
#[inline]
fn is_true(context: &mut Cx<'_>, value: Local<'_>) -> bool {
    handles::logical_scalar(context, value) == 1
}

/// Fails as GNU R does when a string marked as bytes and not ASCII would need a character
/// reading.
fn refuse_bytes(context: &Cx<'_>, text: Local<'_, Str>) -> R<()> {
    if text
        .iter(context)
        .any(|chars| handles::is_bytes(chars) && !chars.bytes().is_ascii())
    {
        return Err(error(
            "translating strings with \"bytes\" encoding is not allowed",
        ));
    }
    Ok(())
}

/// Maps every non-`NA` element of the character vector `text`, which is `x` or its standard
/// form, through `edit` (`None` for `NA`), keeping the attributes of `x`.
fn map_strings<'s>(
    context: &mut Cx<'s>,
    x: Local<'s>,
    text: Local<'s, Str>,
    edit: impl Fn(&str) -> Option<String>,
) -> R<Local<'s>> {
    let mut result = context.alloc::<Str>(text.len())?;
    for index in 0..text.len() {
        let chars = text.get(context, index);
        if handles::is_na(chars) {
            result.set(context, index, chars);
            continue;
        }
        let edited = {
            let current = handles::text(chars);
            match edit(&current) {
                Some(edited) if edited == *current => Ok(()),
                other => Err(other),
            }
        };
        match edited {
            Ok(()) => result.set(context, index, chars),
            Err(Some(edited)) => handles::set_text(context, &mut result, index, &edited)?,
            Err(None) => result.set(context, index, handles::na()),
        }
    }
    handles::copy_attributes(context, x, &mut result)?;
    Ok(result.finish().value())
}

/// A pattern that does not compile: the error, and the warning GNU R gives first on some paths.
/// The warning is raised by the caller, which holds the context a warning needs.
struct CompileFailure {
    warning: Option<String>,
    error: Unwind,
}

impl CompileFailure {
    fn error(error: Unwind) -> CompileFailure {
        CompileFailure {
            warning: None,
            error,
        }
    }
}

/// Raises the failure's warning, then returns the error, or the unwind a handler of the
/// warning started.
fn compile_failure(context: &mut Cx<'_>, failure: CompileFailure) -> Unwind {
    match failure
        .warning
        .map(|message| handles::warning(context, message))
    {
        Some(Err(unwind)) => unwind,
        _ => failure.error,
    }
}

/// The byte index after the character at `index`, or `index + 1` at the end of the text.
fn next_boundary(text: &str, index: usize) -> usize {
    text.get(index..)
        .and_then(|rest| rest.chars().next())
        .map_or(index + 1, |character| index + character.len_utf8())
}
