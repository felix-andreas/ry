//! Formatting numbers and strings the way R prints them. Every vector printed together shares a
//! format: one width, and for doubles one choice between fixed and scientific notation and one
//! number of decimals, decided by GNU R's rule (fixed unless it is wider than scientific by more
//! than `scipen`).

use crate::{
    builtins::{BuiltinDef, Call, internal},
    unwind::{R, error},
    values::{self, alloc, length, nil, symbols},
};
use heap::object::{NA_INTEGER, Rcomplex, Sexp, SexpType, gp, is_na_real};
use std::cell::RefCell;
use unicode_width::UnicodeWidthChar;

pub const BUILTINS: &[BuiltinDef] = &[
    internal("format", 9, do_format),
    internal("formatC", 7, do_format_c),
    internal("encodeString", 5, do_encode_string),
    internal("prettyNum", -1, |call| unsafe { Ok(call.arg(0)) }),
    internal("format.info", 3, do_format_info),
];

/// The print settings a format is computed under.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub digits: usize,
    pub scipen: i32,
    pub na_width: usize,
    pub nsmall: usize,
}

impl Settings {
    pub unsafe fn current() -> Settings {
        let digits = crate::envir::get_option("digits");
        let digits = if digits == nil() {
            7
        } else {
            values::as_integer_scalar(digits).clamp(1, 22) as usize
        };
        let scipen = crate::envir::get_option("scipen");
        let scipen = if scipen == nil() {
            0
        } else {
            values::as_integer_scalar(scipen)
        };
        Settings {
            digits,
            scipen,
            na_width: 2,
            nsmall: 0,
        }
    }

    pub unsafe fn with_digits(digits: usize) -> Settings {
        Settings {
            digits,
            ..Settings::current()
        }
    }
}

/// How a group of doubles is printed: total width, decimals, and whether scientific.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealFormat {
    pub width: usize,
    pub decimals: usize,
    /// Exponent digits for scientific notation (2 or 3), or 0 for fixed notation.
    pub exponent: usize,
}

/// Decimal significance of one finite value at `digits` significant digits: whether it is
/// negative, its decimal exponent, and how many significant digits it needs.
pub fn scientific(value: f64, digits: usize) -> (bool, i32, usize) {
    if value == 0.0 {
        return (value.is_sign_negative() && false, 0, 1);
    }
    let digits = digits.clamp(1, 22);
    let text = format!("{:.*e}", digits - 1, value.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let significant: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let trailing = significant
        .iter()
        .rev()
        .take_while(|&&digit| digit == b'0')
        .count();
    let needed = (significant.len() - trailing).max(1);
    (value < 0.0, exponent, needed)
}

/// GNU R's `formatReal`: the shared format of `values`.
pub fn format_real_common(values: &[f64], settings: Settings) -> RealFormat {
    let candidates = real_format_candidates(values, settings);
    let mut format = match candidates.notations {
        Some((fixed, scientific))
            if fixed.width as i64 <= scientific.width as i64 + settings.scipen as i64 =>
        {
            fixed
        }
        Some((_, scientific)) => scientific,
        None => RealFormat {
            width: 0,
            decimals: 0,
            exponent: 0,
        },
    };
    format.width = format.width.max(candidates.special_width);
    format
}

/// The fixed and the scientific format `values` would share, `None` when no value is finite,
/// and the width of the widest non-finite value.
struct RealCandidates {
    notations: Option<(RealFormat, RealFormat)>,
    special_width: usize,
}

fn real_format_candidates(values: &[f64], settings: Settings) -> RealCandidates {
    let mut negative = false;
    let mut max_sleft = i32::MIN;
    let mut right_max = i32::MIN;
    let mut max_significant = 0usize;
    let mut max_exponent = i32::MIN;
    let mut min_exponent = i32::MAX;
    let mut any_finite = false;
    let mut special_width = 0usize;
    for &value in values {
        if is_na_real(value) {
            special_width = special_width.max(settings.na_width);
            continue;
        }
        if value.is_nan() {
            special_width = special_width.max(3);
            continue;
        }
        if value.is_infinite() {
            special_width = special_width.max(if value < 0.0 { 4 } else { 3 });
            continue;
        }
        any_finite = true;
        let (is_negative, exponent, significant) = scientific(value, settings.digits);
        let left = exponent + 1;
        let sleft = is_negative as i32 + if left <= 0 { 1 } else { left };
        let right = significant as i32 - left;
        if is_negative {
            negative = true;
        }
        right_max = right_max.max(right);
        max_sleft = max_sleft.max(sleft);
        max_significant = max_significant.max(significant);
        max_exponent = max_exponent.max(exponent);
        min_exponent = min_exponent.min(exponent);
    }
    if !any_finite {
        return RealCandidates {
            notations: None,
            special_width,
        };
    }
    let exponent_digits = if max_exponent >= 100 || min_exponent <= -100 {
        3
    } else {
        2
    };
    let mantissa_decimals = max_significant.saturating_sub(1);
    let scientific_width = negative as usize
        + (mantissa_decimals > 0) as usize
        + mantissa_decimals
        + 3
        + exponent_digits;
    let mut right = right_max.max(0) as usize;
    if settings.nsmall > right {
        right = settings.nsmall;
    }
    let fixed_width = max_sleft as usize + right + (right != 0) as usize;
    RealCandidates {
        notations: Some((
            RealFormat {
                width: fixed_width,
                decimals: right,
                exponent: 0,
            },
            RealFormat {
                width: scientific_width,
                decimals: mantissa_decimals,
                exponent: exponent_digits,
            },
        )),
        special_width,
    }
}

/// One double in a given format, without padding.
pub fn format_real_with(value: f64, format: RealFormat, na: &str) -> String {
    if is_na_real(value) {
        return na.to_owned();
    }
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-Inf".into()
        } else {
            "Inf".into()
        };
    }
    if format.exponent == 0 {
        let text = format!("{:.*}", format.decimals, value);
        // `-0` prints as `0`.
        if text.starts_with('-') && text[1..].bytes().all(|byte| byte == b'0' || byte == b'.') {
            return text[1..].to_owned();
        }
        return text;
    }
    scientific_text(value, format.decimals)
}

/// `value` in scientific notation with `decimals` mantissa decimals and at least two exponent
/// digits. A vector whose exponents need three digits pads its shorter elements to the common
/// width with spaces, not with exponent zeros, as GNU R does.
pub fn scientific_text(value: f64, decimals: usize) -> String {
    let text = format!("{:.*e}", decimals, value);
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if exponent < 0 { '-' } else { '+' };
    format!(
        "{mantissa}e{sign}{:0width$}",
        exponent.unsigned_abs(),
        width = 2
    )
}

/// One double formatted on its own at `digits` significant digits, as `as.character` and
/// `paste` show it (15 digits for those).
pub fn format_real_alone(value: f64, digits: usize) -> String {
    let settings = Settings {
        digits,
        scipen: 0,
        na_width: 2,
        nsmall: 0,
    };
    let format = format_real_common(&[value], settings);
    format_real_with(value, format, "NA")
}

/// As `format_real_alone`, honoring `options(scipen)`.
pub unsafe fn format_real_alone_scipen(value: f64, digits: usize) -> String {
    let settings = Settings {
        digits,
        scipen: Settings::current().scipen,
        na_width: 2,
        nsmall: 0,
    };
    let format = format_real_common(&[value], settings);
    format_real_with(value, format, "NA")
}

pub fn format_integer(value: i32) -> String {
    if value == NA_INTEGER {
        "NA".into()
    } else {
        value.to_string()
    }
}

thread_local! {
    /// The decimal mark `options(OutDec)` names. Printing, `format`, `cat` and `as.character`
    /// write it, while deparsing and `sprintf` keep `.`, so it applies where those callers
    /// finish a number rather than inside the shared formatter.
    static OUTPUT_DECIMAL: RefCell<String> = RefCell::new(".".to_owned());
}

pub fn set_output_decimal(mark: &str) {
    OUTPUT_DECIMAL.with(|decimal| *decimal.borrow_mut() = mark.to_owned());
}

/// `text`, a formatted number, with `options(OutDec)` as its decimal mark.
pub fn localize_decimal(text: String) -> String {
    OUTPUT_DECIMAL.with(|decimal| {
        let decimal = decimal.borrow();
        if decimal.as_str() == "." || !text.contains('.') {
            return text;
        }
        // The common width was measured with a one-character mark, so a shorter mark gives
        // back the padding it would have taken.
        let shortfall = 1usize.saturating_sub(decimal.chars().count());
        let padding = text.len() - text.trim_start_matches(' ').len();
        text[padding.min(shortfall)..].replace('.', &decimal)
    })
}

pub fn format_logical(value: i32) -> &'static str {
    match value {
        NA_INTEGER => "NA",
        0 => "FALSE",
        _ => "TRUE",
    }
}

/// A complex number's parts formatted together.
pub fn format_complex_with(
    value: Rcomplex,
    real: RealFormat,
    imaginary: RealFormat,
    digits: usize,
) -> String {
    if is_na_real(value.r) || is_na_real(value.i) {
        return "NA".into();
    }
    // GNU R rounds only to decide which parts print as zero and which sign the imaginary part
    // shows; a nonzero part prints from its unrounded value.
    let rounded = round_complex(value, digits);
    let real_part = if rounded.r == 0.0 { 0.0 } else { value.r };
    let imaginary_part = if rounded.i == 0.0 { 0.0 } else { value.i };
    let real_text = format_real_with(real_part, real, "NA");
    let sign = if imaginary_part < 0.0 { '-' } else { '+' };
    let imaginary_text = format_real_with(imaginary_part.abs(), imaginary, "NA");
    // Each part is right-justified to its own common width, as in GNU R's `1+ 1i 1+10i`.
    format!(
        "{}{sign}{}i",
        pad(&real_text, real.width, true),
        pad(&imaginary_text, imaginary.width, true)
    )
}

pub fn format_complex_common(values: &[Rcomplex], settings: Settings) -> (RealFormat, RealFormat) {
    // GNU R first rounds each number to `digits` significant digits of its larger part, so
    // a small part shows no more decimals than that precision supports.
    let rounded: Vec<Rcomplex> = values
        .iter()
        .filter(|value| !is_na_real(value.r) && !is_na_real(value.i))
        .map(|value| round_complex(*value, settings.digits))
        .collect();
    let reals: Vec<f64> = rounded.iter().map(|value| value.r).collect();
    let imaginaries: Vec<f64> = rounded.iter().map(|value| value.i.abs()).collect();
    let real = format_real_common(&reals, settings);
    let imaginary = format_real_common(&imaginaries, settings);
    // The notation is chosen for both parts together: when either part alone would print in
    // scientific notation, both print fixed only if that is narrower than both in scientific.
    // A part that is exactly zero throughout keeps its own format, so `1e80+0i` prints its zero
    // imaginary part plainly.
    let finite = || {
        values
            .iter()
            .filter(|value| !is_na_real(value.r) && !is_na_real(value.i))
    };
    if (real.exponent == 0 && imaginary.exponent == 0)
        || finite().all(|value| value.r == 0.0)
        || finite().all(|value| value.i == 0.0)
    {
        return (real, imaginary);
    }
    let real_candidates = real_format_candidates(&reals, settings);
    let imaginary_candidates = real_format_candidates(&imaginaries, settings);
    let (Some((real_fixed, real_scientific)), Some((imaginary_fixed, imaginary_scientific))) =
        (real_candidates.notations, imaginary_candidates.notations)
    else {
        return (real, imaginary);
    };
    let fixed = (real_fixed.width + imaginary_fixed.width) as i64;
    let scientific = (real_scientific.width + imaginary_scientific.width) as i64;
    let (mut real, mut imaginary) = if fixed < scientific + settings.scipen as i64 {
        (real_fixed, imaginary_fixed)
    } else {
        (real_scientific, imaginary_scientific)
    };
    real.width = real.width.max(real_candidates.special_width);
    imaginary.width = imaginary.width.max(imaginary_candidates.special_width);
    (real, imaginary)
}

pub fn format_complex_alone(value: Rcomplex, digits: usize) -> String {
    let settings = Settings {
        digits,
        scipen: 0,
        na_width: 2,
        nsmall: 0,
    };
    let (real, imaginary) = format_complex_common(&[value], settings);
    format_complex_with(value, real, imaginary, digits)
}

/// `value` rounded to `digits` significant digits relative to its larger part, as GNU R's
/// `z_prec_r` rounds before printing: a part far smaller than the other one prints as zero.
fn round_complex(value: Rcomplex, digits: usize) -> Rcomplex {
    let magnitude = [value.r.abs(), value.i.abs()]
        .into_iter()
        .filter(|part| part.is_finite())
        .fold(0.0, f64::max);
    if magnitude == 0.0 || digits > 22 {
        return value;
    }
    let decimals = digits.max(1) as i32 - magnitude.log10().floor() as i32 - 1;
    let round = |part: f64, decimals: i32| {
        if !part.is_finite() {
            return part;
        }
        if decimals > 306 {
            let scale = 1.0e4;
            return crate::arith::round_digits(part * scale, decimals - 4) / scale;
        }
        crate::arith::round_digits(part, decimals)
    };
    Rcomplex {
        r: round(value.r, decimals),
        i: round(value.i, decimals),
    }
}

/// The display width of text in a terminal.
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| UnicodeWidthChar::width(character).unwrap_or(0))
        .sum()
}

/// A string as `print` shows it: quoted, with escapes for quotes, backslashes and control
/// characters.
pub fn encode_string(text: &str, quote: Option<char>) -> String {
    let mut result = String::with_capacity(text.len() + 2);
    if let Some(quote) = quote {
        result.push(quote);
    }
    encode_into(&mut result, text, quote);
    if let Some(quote) = quote {
        result.push(quote);
    }
    result
}

/// A string's bytes as `print` shows them. A byte that is not part of valid UTF-8 prints as
/// `\xNN`, as GNU R prints it in a UTF-8 locale.
pub fn encode_bytes(bytes: &[u8], quote: Option<char>) -> String {
    let mut result = String::with_capacity(bytes.len() + 2);
    if let Some(quote) = quote {
        result.push(quote);
    }
    for chunk in bytes.utf8_chunks() {
        encode_into(&mut result, chunk.valid(), quote);
        for byte in chunk.invalid() {
            result.push_str(&format!("\\x{byte:02x}"));
        }
    }
    if let Some(quote) = quote {
        result.push(quote);
    }
    result
}

/// An element of a character vector as `print` shows it, from its bytes unless it is marked
/// as Latin-1.
pub unsafe fn encode_element(charsxp: Sexp, quote: Option<char>) -> String {
    if charsxp.has_gp(gp::LATIN1) || charsxp.has_gp(gp::BYTES) {
        encode_string(&values::char_str(charsxp), quote)
    } else {
        encode_bytes(charsxp.bytes(), quote)
    }
}

fn encode_into(result: &mut String, text: &str, quote: Option<char>) {
    for character in text.chars() {
        match character {
            '\n' => result.push_str("\\n"),
            '\t' => result.push_str("\\t"),
            '\r' => result.push_str("\\r"),
            '\u{7}' => result.push_str("\\a"),
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            '\u{b}' => result.push_str("\\v"),
            '\\' => result.push_str("\\\\"),
            '\0' => result.push_str("\\0"),
            character if Some(character) == quote => {
                result.push('\\');
                result.push(character);
            }
            character if (character as u32) < 0x20 || character as u32 == 0x7f => {
                result.push_str(&format!("\\{:03o}", character as u32));
            }
            character if ('\u{80}'..='\u{9f}').contains(&character) => {
                result.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => result.push(character),
        }
    }
}

/// Pads `text` to `width` display columns, on the left (right-justified) or the right.
pub fn pad(text: &str, width: usize, left: bool) -> String {
    let current = display_width(text);
    if current >= width {
        return text.to_owned();
    }
    let padding = " ".repeat(width - current);
    if left {
        padding + text
    } else {
        text.to_owned() + &padding
    }
}

/// Every element of an atomic vector formatted to a common width, as `print` and `format`
/// render them. Strings are quoted when `quote` is set.
pub unsafe fn format_vector(
    vector: Sexp,
    settings: Settings,
    quote: bool,
    na_string: &str,
) -> R<Vec<String>> {
    let vector = values::materialize(vector)?;
    let count = length(vector);
    Ok(match vector.kind() {
        SexpType::Logical => {
            let items: Vec<String> = values::ints(vector)
                .iter()
                .map(|&value| {
                    if value == NA_INTEGER {
                        na_string.to_owned()
                    } else {
                        format_logical(value).to_owned()
                    }
                })
                .collect();
            justify(items, true)
        }
        SexpType::Integer => {
            let items: Vec<String> = values::ints(vector)
                .iter()
                .map(|&value| {
                    if value == NA_INTEGER {
                        na_string.to_owned()
                    } else {
                        value.to_string()
                    }
                })
                .collect();
            justify(items, true)
        }
        SexpType::Real => {
            let data = values::reals(vector);
            let settings = Settings {
                na_width: display_width(na_string),
                ..settings
            };
            let format = format_real_common(data, settings);
            // The common width can exceed every element's own: a negative mantissa and a
            // three-digit exponent each widen it, even when they occur in different elements.
            let items: Vec<String> = data
                .iter()
                .map(|&value| {
                    let text = format_real_with(value, format, na_string);
                    localize_decimal(pad(&text, format.width, true))
                })
                .collect();
            justify(items, true)
        }
        SexpType::Complex => {
            let data = values::complexes(vector);
            let (real, imaginary) = format_complex_common(data, settings);
            let items: Vec<String> = data
                .iter()
                .map(|&value| {
                    if is_na_real(value.r) || is_na_real(value.i) {
                        na_string.to_owned()
                    } else {
                        localize_decimal(format_complex_with(
                            value,
                            real,
                            imaginary,
                            settings.digits,
                        ))
                    }
                })
                .collect();
            justify(items, true)
        }
        SexpType::String => {
            let items: Vec<String> = (0..count)
                .map(|index| match values::string_at(vector, index) {
                    Some(text) => {
                        if quote {
                            encode_element(vector.element(index), Some('"'))
                        } else {
                            text.into_owned()
                        }
                    }
                    None => {
                        if quote {
                            "NA".to_owned()
                        } else {
                            na_string.to_owned()
                        }
                    }
                })
                .collect();
            items
        }
        SexpType::Raw => values::raws(vector)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        _ => vec![String::new(); count],
    })
}

/// Pads every item to the widest one; right-justified when `right`.
pub fn justify(items: Vec<String>, right: bool) -> Vec<String> {
    let width = items
        .iter()
        .map(|item| display_width(item))
        .max()
        .unwrap_or(0);
    items
        .into_iter()
        .map(|item| pad(&item, width, right))
        .collect()
}

// The internals.

/// `format(x, trim, digits, nsmall, width, justify, na.encode, scientific, decimal.mark)`.
unsafe fn do_format(call: &Call<'_>) -> R<Sexp> {
    let vector = values::materialize(call.arg(0))?;
    let trim = values::as_logical_scalar(call.arg(1)) == 1;
    let digits = call.arg(2);
    let nsmall = values::as_integer_scalar(call.arg(3)).max(0) as usize;
    let width = call.arg(4);
    let justify_mode = values::as_integer_scalar(call.arg(5));
    let na_encode = values::as_logical_scalar(call.arg(6)) != 0;
    let scientific = call.arg(7);
    let mut settings = if digits == nil() {
        Settings::current()
    } else {
        Settings::with_digits(values::as_integer_scalar(digits).clamp(1, 22) as usize)
    };
    settings.nsmall = nsmall;
    if scientific != nil() {
        if scientific.kind() == SexpType::Logical {
            // `NA`, the default of `format.default`, keeps `options(scipen)`. The penalties
            // outweigh any width, so even `.Machine$double.xmax` prints all 309 digits.
            match values::as_logical_scalar(scientific) {
                1 => settings.scipen = -10_000,
                0 => settings.scipen = 10_000,
                _ => {}
            }
        } else {
            settings.scipen = values::as_integer_scalar(scientific);
        }
    }
    let min_width = if width == nil() {
        0
    } else {
        values::as_integer_scalar(width).max(0) as usize
    };
    let items: Vec<Option<String>> = match vector.kind() {
        SexpType::String => {
            let strings = values::strings(vector);
            let texts: Vec<String> = strings
                .iter()
                .map(|item| item.clone().unwrap_or_else(|| "NA".into()))
                .collect();
            let common = texts
                .iter()
                .map(|text| display_width(text))
                .max()
                .unwrap_or(0)
                .max(min_width);
            strings
                .iter()
                .zip(texts)
                .map(|(original, text)| {
                    if original.is_none() && !na_encode {
                        return None;
                    }
                    Some(match justify_mode {
                        0 => pad(&text, common, false),
                        1 => pad(&text, common, true),
                        2 => {
                            let total = common.saturating_sub(display_width(&text));
                            let left = total / 2;
                            format!("{}{}{}", " ".repeat(left), text, " ".repeat(total - left))
                        }
                        _ => text,
                    })
                })
                .collect()
        }
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex => {
            let mark = values::first_string(call.arg(8));
            let saved = OUTPUT_DECIMAL.with(|decimal| decimal.borrow().clone());
            if let Some(mark) = &mark {
                set_output_decimal(mark);
            }
            let formatted = format_vector(vector, settings, false, "NA");
            set_output_decimal(&saved);
            let mut items = formatted?;
            if trim {
                items = items
                    .into_iter()
                    .map(|item| item.trim_start().to_owned())
                    .collect();
            }
            items
                .into_iter()
                .map(|item| Some(pad(&item, min_width, true)))
                .collect()
        }
        SexpType::Raw => format_vector(vector, settings, false, "NA")?
            .into_iter()
            .map(Some)
            .collect(),
        SexpType::Nil => Vec::new(),
        SexpType::Environment => vec![Some(crate::print::environment_label(vector)?)],
        _ => {
            return Err(call.error(
                "Found no format() method for class \"".to_string() + vector.kind().name() + "\"",
            ));
        }
    };
    let result = alloc(SexpType::String, items.len())?;
    for (index, item) in items.into_iter().enumerate() {
        let charsxp = match item {
            Some(text) => values::mkchar(&text)?,
            None => values::na_string(),
        };
        values::set_elt(result, index, charsxp);
    }
    for symbol in [symbols().names, symbols().dim, symbols().dimnames] {
        let value = values::get_attr(vector, symbol);
        if value != nil() {
            values::set_attr(result, symbol, value)?;
        }
    }
    Ok(result)
}

/// `formatC(x, mode, width, digits, format, flag, i.strlen)`: C-style formatting per element.
unsafe fn do_format_c(call: &Call<'_>) -> R<Sexp> {
    let vector = call.arg(0);
    let mode = values::require_string(call.arg(1), "mode")?;
    let width = values::as_integer_scalar(call.arg(2));
    let digits = values::as_integer_scalar(call.arg(3));
    let format = values::require_string(call.arg(4), "format")?;
    let flag = values::require_string(call.arg(5), "flag")?;
    let count = length(vector);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let text = if mode == "character" {
            values::string_at(vector, index)
                .unwrap_or_default()
                .into_owned()
        } else {
            let value = crate::subset::numeric_element(vector, index);
            c_format(value, &format, digits, &flag, mode == "integer")
        };
        // GNU R writes an `fg` zero as a bare "0", whatever the width.
        let text = if format == "fg" && text == "0" {
            text
        } else {
            apply_width(&text, width, &flag)
        };
        values::set_elt(result, index, values::mkchar(&text)?);
    }
    Ok(result)
}

fn apply_width(text: &str, width: i32, flag: &str) -> String {
    let width = width.unsigned_abs() as usize;
    if display_width(text) >= width {
        return text.to_owned();
    }
    if flag.contains('-') {
        pad(text, width, false)
    } else if flag.contains('0') && !text.contains(|c: char| c.is_alphabetic() && c != 'e') {
        let (sign, digits) = match text.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", text),
        };
        format!("{sign}{}{digits}", "0".repeat(width - display_width(text)))
    } else {
        pad(text, width, true)
    }
}

/// One number in C `printf` style: `d`, `f`, `e`, `E`, `g`, `G`, `fg`.
pub fn c_format(value: f64, format: &str, digits: i32, flag: &str, integer: bool) -> String {
    if is_na_real(value) {
        return "NA".into();
    }
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-Inf".into()
        } else {
            "Inf".into()
        };
    }
    let plus = if flag.contains('+') && value >= 0.0 {
        "+"
    } else if flag.contains(' ') && value >= 0.0 {
        " "
    } else {
        ""
    };
    let precision = if digits < 0 {
        None
    } else {
        Some(digits as usize)
    };
    let text = match format {
        "d" => format!("{}", value.round() as i64),
        "f" => format!("{:.*}", precision.unwrap_or(6), value),
        "e" => scientific_text(value, precision.unwrap_or(6)),
        "E" => scientific_text(value, precision.unwrap_or(6)).to_uppercase(),
        // formatC passes a negative `digits` to ask `fg` to keep trailing zeros.
        "fg" => fixed_significant(
            value,
            digits.unsigned_abs().max(1) as usize,
            digits < 0 || flag.contains('#'),
        ),
        "g" | "G" => {
            let significant = precision.unwrap_or(6).max(1);
            let text = c_general(value, significant, flag.contains('#'));
            if format == "G" {
                text.to_uppercase()
            } else {
                text
            }
        }
        _ => {
            if integer {
                format!("{}", value as i64)
            } else {
                c_general(value, precision.unwrap_or(6).max(1), false)
            }
        }
    };
    format!("{plus}{text}")
}

/// C's `%g`: the shorter of `%e` and `%f` at `significant` digits, without trailing zeros.
pub fn c_general(value: f64, significant: usize, keep_zeros: bool) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let text = format!("{:.*e}", significant.saturating_sub(1), value.abs());
    let exponent: i32 = text
        .split_once('e')
        .map_or(0, |(_, exponent)| exponent.parse().unwrap_or(0));
    let text = if exponent < -4 || exponent >= significant as i32 {
        let text = scientific_text(value, significant.saturating_sub(1));
        if keep_zeros {
            text
        } else {
            let (mantissa, rest) = text.split_once('e').unwrap_or((&text, ""));
            let mantissa = trim_zeros(mantissa);
            format!("{mantissa}e{rest}")
        }
    } else {
        let decimals = (significant as i32 - 1 - exponent).max(0) as usize;
        let text = format!("{value:.decimals$}");
        if keep_zeros {
            text
        } else {
            trim_zeros(&text).to_owned()
        }
    };
    text
}

/// `formatC`'s `fg`: `significant` digits in fixed notation, never in scientific. Digits left of
/// the decimal point are all kept, and `keep_zeros` keeps trailing zeros and a bare point.
fn fixed_significant(value: f64, significant: usize, keep_zeros: bool) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let text = format!("{:.*e}", significant.saturating_sub(1), value.abs());
    let exponent: i32 = text
        .split_once('e')
        .map_or(0, |(_, exponent)| exponent.parse().unwrap_or(0));
    let decimals = (significant as i32 - 1 - exponent).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if !keep_zeros {
        trim_zeros(&text).to_owned()
    } else if decimals == 0 {
        format!("{text}.")
    } else {
        text
    }
}

fn trim_zeros(text: &str) -> &str {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    }
}

/// `encodeString(x, width, quote, na.encode, justify)`.
unsafe fn do_encode_string(call: &Call<'_>) -> R<Sexp> {
    let vector = call.arg(0);
    if vector.kind() != SexpType::String {
        return Err(error("a character vector argument expected"));
    }
    let width = call.arg(1);
    let quote = values::first_string(call.arg(2)).and_then(|text| text.chars().next());
    let justify_mode = values::as_integer_scalar(call.arg(3));
    let na_encode = values::as_logical_scalar(call.arg(4)) != 0;
    let count = length(vector);
    let encoded: Vec<Option<String>> = (0..count)
        .map(|index| match values::string_at(vector, index) {
            Some(_) => Some(encode_element(vector.element(index), quote)),
            // GNU R marks an unquoted NA as `<NA>`, so that it cannot be mistaken for the string "NA".
            None => na_encode.then(|| if quote.is_some() { "NA" } else { "<NA>" }.to_owned()),
        })
        .collect();
    let target = if width == nil() {
        0
    } else if values::as_integer_scalar(width) == NA_INTEGER {
        encoded
            .iter()
            .flatten()
            .map(|text| display_width(text))
            .max()
            .unwrap_or(0)
    } else {
        values::as_integer_scalar(width).max(0) as usize
    };
    let result = alloc(SexpType::String, count)?;
    for (index, item) in encoded.into_iter().enumerate() {
        let charsxp = match item {
            Some(text) => {
                let text = match justify_mode {
                    0 => pad(&text, target, false),
                    1 => pad(&text, target, true),
                    2 => {
                        let total = target.saturating_sub(display_width(&text));
                        format!(
                            "{}{}{}",
                            " ".repeat(total / 2),
                            text,
                            " ".repeat(total - total / 2)
                        )
                    }
                    _ => text,
                };
                values::mkchar(&text)?
            }
            None => values::na_string(),
        };
        values::set_elt(result, index, charsxp);
    }
    values::copy_most_attributes(vector, result, true)?;
    Ok(result)
}

unsafe fn do_format_info(call: &Call<'_>) -> R<Sexp> {
    let vector = values::materialize(call.arg(0))?;
    let digits = call.arg(1);
    let nsmall = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let mut settings = if digits == nil() {
        Settings::current()
    } else {
        Settings::with_digits(values::as_integer_scalar(digits).clamp(1, 22) as usize)
    };
    settings.nsmall = nsmall;
    match vector.kind() {
        SexpType::Real => {
            let format = format_real_common(values::reals(vector), settings);
            values::integer_vector(&[
                format.width as i32,
                format.decimals as i32,
                if format.exponent == 0 {
                    0
                } else {
                    format.exponent as i32 - 1
                },
            ])
        }
        // The real and the imaginary part's width, digits and exponent width.
        SexpType::Complex => {
            let (real, imaginary) = format_complex_common(values::complexes(vector), settings);
            let exponent = |format: &RealFormat| {
                if format.exponent == 0 {
                    0
                } else {
                    format.exponent as i32 - 1
                }
            };
            values::integer_vector(&[
                real.width as i32,
                real.decimals as i32,
                exponent(&real),
                imaginary.width as i32,
                imaginary.decimals as i32,
                exponent(&imaginary),
            ])
        }
        _ => {
            let items = format_vector(vector, settings, false, "NA")?;
            let width = items
                .iter()
                .map(|item| display_width(item))
                .max()
                .unwrap_or(0);
            values::integer_vector(&[width as i32])
        }
    }
}

/// The width option, for printing.
pub unsafe fn width_option() -> usize {
    let width = crate::envir::get_option("width");
    if width == nil() {
        80
    } else {
        values::as_integer_scalar(width).clamp(10, 10000) as usize
    }
}
