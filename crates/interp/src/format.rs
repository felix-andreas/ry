//! Formatting numbers and strings the way R prints them. Every vector printed together shares a
//! format: one width, and for doubles one choice between fixed and scientific notation and one
//! number of decimals, decided by GNU R's rule (fixed unless it is wider than scientific by more
//! than `scipen`).

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        unwind::{R, error},
        values::{self, alloc, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, Rcomplex, Sexp, SexpType, gp, is_na_real},
    std::{cell::RefCell, fmt::Write},
    unicode_width::UnicodeWidthChar,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("format", 9, do_format),
    internal("formatC", 7, do_format_c),
    internal("encodeString", 5, do_encode_string),
    internal("prettyNum", -1, |call| Ok(call.arg(0))),
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
        Settings {
            digits,
            scipen: Settings::current_scipen(),
            na_width: 2,
            nsmall: 0,
        }
    }

    /// `options(scipen)` alone. Callers formatting many values read it once, because
    /// `.Options` can be reassigned from R code, which rules out caching it here.
    pub unsafe fn current_scipen() -> i32 {
        let scipen = crate::envir::get_option("scipen");
        if scipen == nil() {
            0
        } else {
            values::as_integer_scalar(scipen)
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
        return (false, 0, 1);
    }
    let digits = digits.clamp(1, 22);
    let mut text = StackText::new();
    // The shortest digits that read back as `value` are what rounding to `digits` significant
    // digits leaves once its trailing zeros go, whenever they are no longer than `digits`: the
    // value is within half an ulp of them, far less than half a unit in the last place kept.
    if digits <= SHORTEST_EXACT_DIGITS
        && value.is_normal()
        && write!(text, "{:e}", value.abs()).is_ok()
        && let Some((needed, exponent)) = mantissa_digits(text.as_str())
        && needed <= digits
    {
        return (value < 0.0, exponent, needed);
    }
    text.clear();
    if write!(text, "{:.*e}", digits - 1, value.abs()).is_err() {
        return (value < 0.0, 0, digits);
    }
    let (needed, exponent) = mantissa_digits(text.as_str()).unwrap_or((digits, 0));
    (value < 0.0, exponent, needed)
}

/// The most significant digits at which a normal double's shortest round-trip digits, when they
/// fit, are also its correctly rounded digits. A normal double carries 53 bits, so it lies within
/// about 1.1e-16 of its shortest digits relative to its size, far inside half a unit in the 15th
/// digit; a subnormal has fewer bits (`5e-324` is shortest for 4.94065645841247e-324), and at 16
/// or more digits the half unit is no longer larger than the gap.
const SHORTEST_EXACT_DIGITS: usize = 15;

/// The significant digits of a `{:e}` rendering without its trailing zeros, and its exponent.
fn mantissa_digits(text: &str) -> Option<(usize, i32)> {
    let (mantissa, exponent) = text.split_once('e')?;
    let exponent: i32 = exponent.parse().ok()?;
    let digits = mantissa.bytes().filter(u8::is_ascii_digit);
    let count = digits.clone().count();
    let trailing = mantissa
        .bytes()
        .rev()
        .filter(u8::is_ascii_digit)
        .take_while(|&digit| digit == b'0')
        .count();
    Some(((count - trailing).max(1), exponent))
}

/// A short string built on the stack, for the formatting a hot loop does per element.
struct StackText {
    bytes: [u8; 64],
    length: usize,
}

impl StackText {
    fn new() -> StackText {
        StackText {
            bytes: [0; 64],
            length: 0,
        }
    }

    fn clear(&mut self) {
        self.length = 0;
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length]).unwrap_or("")
    }
}

impl std::fmt::Write for StackText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.length + text.len();
        let slot = self
            .bytes
            .get_mut(self.length..end)
            .ok_or(std::fmt::Error)?;
        slot.copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
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
    // GNU R's `encodeReal0` writes a negative zero as a positive one.
    let value = if value == 0.0 { 0.0 } else { value };
    if format.exponent == 0 {
        let text = format!("{:.*}", format.decimals, value);
        // `-0` prints as `0`.
        if text.starts_with('-') && text[1..].bytes().all(|byte| byte == b'0' || byte == b'.') {
            return text[1..].to_owned();
        }
        return text;
    }
    // Adding zero turns `-0` into `0`, which is how it prints.
    scientific_text(value + 0.0, format.decimals)
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
    format_real_alone_scipen(value, digits, 0)
}

/// As `format_real_alone`, with a scientific-notation penalty (`options(scipen)`).
pub fn format_real_alone_scipen(value: f64, digits: usize, scipen: i32) -> String {
    if value.is_finite()
        && let Some(text) = shortest_alone(value, digits, scipen)
    {
        return text;
    }
    let settings = Settings {
        digits,
        scipen,
        na_width: 2,
        nsmall: 0,
    };
    let format = format_real_common(&[value], settings);
    format_real_with(value, format, "NA")
}

/// `format_real_alone_scipen` for a finite value whose shortest round-trip digits fit in
/// `digits` (see `SHORTEST_EXACT_DIGITS`), built straight from those digits. Those digits are exactly what `scientific` counts
/// and what either notation prints, so one shortest conversion replaces the significance pass
/// and the second, fixed-precision conversion. `None` when the digits do not fit.
fn shortest_alone(value: f64, digits: usize, scipen: i32) -> Option<String> {
    // Zero goes through the width comparison too: under `scipen <= -5` GNU R writes `0e+00`.
    if digits > SHORTEST_EXACT_DIGITS || !(value.is_normal() || value == 0.0) {
        return None;
    }
    let mut text = StackText::new();
    write!(text, "{:e}", value.abs()).ok()?;
    let (mantissa, exponent) = text.as_str().split_once('e')?;
    let exponent: i32 = exponent.parse().ok()?;
    let mut significand = [0u8; 24];
    let mut count = 0usize;
    for digit in mantissa.bytes().filter(u8::is_ascii_digit) {
        *significand.get_mut(count)? = digit;
        count += 1;
    }
    while count > 1 && significand[count - 1] == b'0' {
        count -= 1;
    }
    let significand = significand.get(..count)?;
    if count == 0 || count > digits {
        return None;
    }
    let negative = value < 0.0;
    let left = exponent + 1;
    let right = (count as i32 - left).max(0) as usize;
    let fixed_width = negative as usize + left.max(1) as usize + right + (right != 0) as usize;
    let exponent_digits = if exponent.unsigned_abs() >= 100 { 3 } else { 2 };
    let scientific_width =
        negative as usize + (count > 1) as usize + (count - 1) + 3 + exponent_digits;
    let mut out = String::with_capacity(fixed_width.max(scientific_width));
    if negative {
        out.push('-');
    }
    let push_digits = |out: &mut String, digits: &[u8]| {
        out.extend(digits.iter().map(|&digit| digit as char));
    };
    if fixed_width as i64 <= scientific_width as i64 + scipen as i64 {
        if left <= 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', left.unsigned_abs() as usize));
            push_digits(&mut out, significand);
        } else if count <= left as usize {
            // Zeros stand in for digits past the shortest ones only while every integer up to
            // the value is a double, so that the value is exactly the integer they spell; GNU R
            // prints larger values' exact digits.
            if left as usize > SHORTEST_EXACT_DIGITS {
                return None;
            }
            push_digits(&mut out, significand);
            out.extend(std::iter::repeat_n('0', left as usize - count));
        } else {
            let (whole, fraction) = significand.split_at(left as usize);
            push_digits(&mut out, whole);
            out.push('.');
            push_digits(&mut out, fraction);
        }
    } else {
        let (first, rest) = significand.split_at(1);
        push_digits(&mut out, first);
        if !rest.is_empty() {
            out.push('.');
            push_digits(&mut out, rest);
        }
        write!(
            out,
            "e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
        .ok()?;
    }
    Some(out)
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
    let imaginary_text = format_real_with(imaginary_part.abs(), imaginary, "NA");
    let sign = if imaginary_part < 0.0 && imaginary_text != "0" {
        '-'
    } else {
        '+'
    };
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
/// `z_prec_r` rounds before printing through R 4.3: a part far smaller than the other one
/// prints as zero. From R 4.4 on, each part formats on its own, so nothing is rounded.
fn round_complex(value: Rcomplex, digits: usize) -> Rcomplex {
    if crate::system::r_at_least(4, 4) {
        return value;
    }
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
    // Control characters take no columns, so only printable ASCII can skip the per-character
    // width lookup.
    if text.bytes().all(|byte| (0x20..0x7F).contains(&byte)) {
        return text.len();
    }
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
    let mut padded = String::new();
    let mut number = String::new();
    // Rounded numbers repeat, so their strings go through the call's memo.
    let mut memo = values::StringMemo::default();
    let character = mode == "character";
    let integer = mode == "integer";
    let left_justify = width < 0 || flag.contains('-');
    let width = width.unsigned_abs() as usize;
    for index in 0..count {
        let element;
        let text: &str = if character {
            element = values::string_at(vector, index).unwrap_or_default();
            &element
        } else {
            number.clear();
            let value = crate::subset::numeric_element(vector, index);
            c_format_into(&mut number, value, &format, digits, &flag, integer);
            &number
        };
        let shortfall = width.saturating_sub(display_width(text));
        // GNU R writes an `fg` zero as a bare "0", whatever the width.
        if shortfall == 0 || (format == "fg" && text == "0") {
            store_string(result, index, text, &mut memo)?;
            continue;
        }
        padded.clear();
        if left_justify {
            padded.push_str(text);
            padded.extend(std::iter::repeat_n(' ', shortfall));
        } else if flag.contains('0') && !text.contains(|c: char| c.is_alphabetic() && c != 'e') {
            let (sign, digits) = match text.strip_prefix('-') {
                Some(rest) => ("-", rest),
                None => ("", text),
            };
            padded.push_str(sign);
            padded.extend(std::iter::repeat_n('0', shortfall));
            padded.push_str(digits);
        } else {
            padded.extend(std::iter::repeat_n(' ', shortfall));
            padded.push_str(text);
        }
        store_string(result, index, &padded, &mut memo)?;
    }
    Ok(result)
}

unsafe fn store_string(
    vector: Sexp,
    index: usize,
    text: &str,
    memo: &mut values::StringMemo<'_>,
) -> R<()> {
    if text.is_ascii() {
        values::set_elt(vector, index, memo.mk_char(text.as_bytes())?);
        Ok(())
    } else {
        values::set_mkchar(vector, index, text)
    }
}

/// One number in C `printf` style: `d`, `f`, `e`, `E`, `g`, `G`, `fg`.
pub fn c_format(value: f64, format: &str, digits: i32, flag: &str, integer: bool) -> String {
    let mut text = String::new();
    c_format_into(&mut text, value, format, digits, flag, integer);
    text
}

/// [`c_format`] appended to `out`, so a loop over many numbers reuses one buffer.
pub fn c_format_into(
    out: &mut String,
    value: f64,
    format: &str,
    digits: i32,
    flag: &str,
    integer: bool,
) {
    if is_na_real(value) {
        out.push_str("NA");
        return;
    }
    if value.is_nan() {
        out.push_str("NaN");
        return;
    }
    if value.is_infinite() {
        out.push_str(if value < 0.0 { "-Inf" } else { "Inf" });
        return;
    }
    if flag.contains('+') && value >= 0.0 {
        out.push('+');
    } else if flag.contains(' ') && value >= 0.0 {
        out.push(' ');
    }
    // formatC turns a negative `digits` into the default before it gets here, and negates
    // `digits` itself only to ask `fg` to keep trailing zeros, so every other format reads the
    // magnitude, as GNU R's C code does.
    let precision = digits.unsigned_abs() as usize;
    // Writing to a `String` cannot fail, so the `fmt::Result`s below carry nothing.
    match format {
        "d" => {
            write!(out, "{}", value.round() as i64).ok();
        }
        "f" => {
            if !fixed_fast(out, value, precision) {
                write!(out, "{:.*}", precision, value).ok();
            }
        }
        "e" => out.push_str(&scientific_text(value, precision)),
        "E" => out.push_str(&scientific_text(value, precision).to_uppercase()),
        "fg" => out.push_str(&fixed_significant(
            value,
            precision.max(1),
            digits < 0 || flag.contains('#'),
        )),
        "g" | "G" => {
            let text = c_general(value, precision.max(1), flag.contains('#'));
            if format == "G" {
                out.push_str(&text.to_uppercase());
            } else {
                out.push_str(&text);
            }
        }
        _ => {
            if integer {
                write!(out, "{}", value as i64).ok();
            } else {
                out.push_str(&c_general(value, precision.max(1), false));
            }
        }
    }
}

/// C's `%.*f` for a value whose scaled magnitude stays below 2^52, appended to `out`; false,
/// with `out` untouched, for any other value. The digits are the exact binary value rounded to
/// `precision` decimals, half to even, as glibc's `printf` rounds: `fma` recovers the exact
/// error of the scaled product, so the sign of the distance to the half decides the rounding
/// without the arbitrary-precision path general formatting takes.
fn fixed_fast(out: &mut String, value: f64, precision: usize) -> bool {
    const POWERS: [f64; 16] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
    ];
    let Some(&power) = POWERS.get(precision) else {
        return false;
    };
    let magnitude = value.abs();
    let scaled = magnitude * power;
    if scaled.is_nan() || scaled >= 4_503_599_627_370_496.0 {
        return false;
    }
    // `scaled + error` is the exact product, since 10^precision is exact for these powers.
    let error = magnitude.mul_add(power, -scaled);
    let whole = scaled.floor();
    // Below 2^52 the fraction and the half subtract exactly, and adding `error` rounds once,
    // which keeps the sign of the true distance to the half. A product that rounded up to a
    // whole number has a tiny negative true fraction, which still rounds to `whole`.
    let distance = (scaled - whole - 0.5) + error;
    let mut digits = whole as u64;
    if distance > 0.0 || (distance == 0.0 && digits % 2 == 1) {
        digits += 1;
    }
    let mut buffer = [0u8; 20];
    let mut cursor = buffer.len();
    let mut rest = digits;
    loop {
        cursor -= 1;
        buffer[cursor] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    let written = &buffer[cursor..];
    // C keeps the sign of any negative value, including one that rounds to zero.
    if value.is_sign_negative() {
        out.push('-');
    }
    if written.len() <= precision {
        out.push('0');
        if precision > 0 {
            out.push('.');
            out.extend(std::iter::repeat_n('0', precision - written.len()));
            out.extend(written.iter().map(|&byte| byte as char));
        }
    } else {
        let (integral, fraction) = written.split_at(written.len() - precision);
        out.extend(integral.iter().map(|&byte| byte as char));
        if precision > 0 {
            out.push('.');
            out.extend(fraction.iter().map(|&byte| byte as char));
        }
    }
    true
}

/// C's `%g`: the shorter of `%e` and `%f` at `significant` digits, without trailing zeros.
pub fn c_general(value: f64, significant: usize, keep_zeros: bool) -> String {
    if value == 0.0 && !keep_zeros {
        // C keeps the sign of a negative zero.
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let text = format!("{:.*e}", significant.saturating_sub(1), value.abs());
    let exponent: i32 = text
        .split_once('e')
        .map_or(0, |(_, exponent)| exponent.parse().unwrap_or(0));
    if exponent < -4 || exponent >= significant as i32 {
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
    }
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
    // GNU R treats `width = NULL` like `width = NA`: pad to the widest element.
    let requested = if width == nil() {
        NA_INTEGER
    } else {
        values::as_integer_scalar(width)
    };
    if requested != NA_INTEGER && requested < 0 {
        return Err(error("invalid 'width' value"));
    }
    let target = if requested == NA_INTEGER {
        encoded
            .iter()
            .flatten()
            .map(|text| display_width(text))
            .max()
            .unwrap_or(0)
    } else {
        requested as usize
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

/// C's `%g`: six significant digits, as GNU R's messages and `.Internal(inspect())` print reals.
pub fn format_g(value: f64) -> String {
    let mut buffer = [0u8; 64];
    // SAFETY: the format takes one double and the buffer is large enough for any `%g` output.
    let written = unsafe {
        libc::snprintf(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%g".as_ptr(),
            value,
        )
    };
    String::from_utf8_lossy(&buffer[..written.clamp(0, 63) as usize]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::fixed_fast;

    /// The fast path against the exact formatter: random magnitudes across the whole range it
    /// accepts, exact ties at every precision, and the neighbours of decimal halves.
    #[test]
    fn fixed_fast_matches_exact_formatting() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut values = vec![
            0.0,
            -0.0,
            0.5,
            1.5,
            2.5,
            -2.5,
            0.125,
            2.675,
            1e-300,
            4503599627370495.5,
        ];
        for _ in 0..200_000 {
            let bits = next();
            let scale = 10f64.powi((bits % 34) as i32 - 18);
            values.push(((bits >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * scale);
        }
        for numerator in -4000i64..4000 {
            for shift in [1, 2, 3, 4, 6, 10] {
                values.push(numerator as f64 / f64::from(1u32 << shift));
            }
            for power in [1e1, 1e2, 1e3, 1e5] {
                let half = (numerator as f64 + 0.5) / power;
                values.extend([
                    half,
                    f64::from_bits(half.to_bits() + 1),
                    f64::from_bits(half.to_bits() - 1),
                ]);
            }
        }
        for value in values {
            for precision in 0..16 {
                let mut fast = String::new();
                if fixed_fast(&mut fast, value, precision) {
                    assert_eq!(
                        fast,
                        format!("{value:.precision$}"),
                        "{value:e} at {precision}"
                    );
                }
            }
        }
    }
}
