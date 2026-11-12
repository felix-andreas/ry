//! Formatting numbers and strings the way R prints them. Every vector printed together shares a
//! format: one width, and for doubles one choice between fixed and scientific notation and one
//! number of decimals, decided by GNU R's rule (fixed unless it is wider than scientific by more
//! than `scipen`).

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        long_double::LongDouble,
        strings::{insert_fill, with_text_buffer},
        unwind::{R, error},
        values::{self, alloc, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Rcomplex, Sexp, SexpType, gp, is_na_real},
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
            return 0;
        }
        // A `scipen` that is not a number (`options(scipen = "a")`) reads as NA, which GNU R's
        // width comparison then treats like 0.
        match values::as_integer_scalar(scipen) {
            NA_INTEGER => 0,
            value => value,
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

/// How one finite value looks at `digits` significant digits, as GNU R's `scientific` in
/// `format.c` decides it: `|value| = alpha * 10^kpower` with `1 <= alpha < 10`, and `alpha`
/// needing `significant` digits once its trailing zeros go.
struct Significance {
    negative: bool,
    kpower: i32,
    significant: i32,
    /// Rounding to `digits` carries into a new leading digit that fixed notation, which keeps
    /// more of the value, does not show: `9996` at 3 digits is `1.00e+04` but prints as `9996`.
    rounding_widens: bool,
}

/// GNU R scales the value by a power of ten in x87 extended precision and rounds the result to
/// an integer, so its digit counts are not those of the correctly rounded decimal: at 15
/// digits, `4.0270798167452e-09` has a 16th digit only in its exact expansion. Matching that
/// byte for byte needs the same arithmetic, which `LongDouble` models, and the same powers of
/// ten: GNU R's table up to `1e27`, and glibc's `powl(10, k)` beyond, which is not always
/// correctly rounded and so comes from [`GLIBC_POWERS_OF_TEN`].
fn scientific(value: f64, digits: usize) -> Significance {
    if value == 0.0 {
        return Significance {
            negative: false,
            kpower: 0,
            significant: 1,
            rounding_widens: false,
        };
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    let digits = digits.clamp(1, 22) as i32;
    if digits > f64::DIGITS as i32 {
        return significance_via_text(magnitude, negative, digits);
    }
    let mut kp = magnitude.log10().floor() as i32 - digits + 1;
    let mut scaled = LongDouble::from(magnitude);
    if kp.unsigned_abs() <= TABLE_POWERS {
        if kp > 0 {
            scaled = scaled / table_power_of_ten(kp);
        } else if kp < 0 {
            scaled = scaled * table_power_of_ten(-kp);
        }
    } else {
        scaled = scaled / glibc_power_of_ten(kp);
    }
    if scaled < table_power_of_ten(digits - 1) {
        scaled = scaled * LongDouble::from(10.0);
        kp -= 1;
    }
    // At most `10^digits`, so the conversion is exact.
    let mut alpha = scaled.nearbyint().to_f64() as u64;
    let mut significant = digits;
    while significant > 0 && alpha % 10 == 0 {
        alpha /= 10;
        significant -= 1;
    }
    if significant == 0 {
        significant = 1;
        kp += 1;
    }
    let kpower = kp + digits - 1;
    // The decimal place fixed notation cuts off, and half a unit there.
    let cut = (digits - kpower).clamp(0, TABLE_POWERS as i32);
    let fuzz = 0.5 / DOUBLE_POWERS_OF_TEN[cut as usize];
    let rounding_widens = kpower > 0
        && kpower <= TABLE_POWERS as i32
        && LongDouble::from(magnitude) < table_power_of_ten(kpower) - LongDouble::from(fuzz);
    Significance {
        negative,
        kpower,
        significant,
        rounding_widens,
    }
}

/// GNU R's `format_via_sprintf`, for more digits than a double carries: the correctly rounded
/// `%e` digits decide.
fn significance_via_text(magnitude: f64, negative: bool, digits: i32) -> Significance {
    let mut text = StackText::new();
    let (significant, kpower) = write!(text, "{:.*e}", digits as usize - 1, magnitude)
        .ok()
        .and_then(|()| mantissa_digits(text.as_str()))
        .unwrap_or((digits as usize, 0));
    Significance {
        negative,
        kpower,
        significant: significant as i32,
        rounding_widens: false,
    }
}

/// The largest power GNU R's `tbl` holds. Its entries are written as double literals, so
/// `1e23` to `1e27`, which a double cannot hold exactly, are the nearest doubles widened to
/// long double, not the exact powers.
const TABLE_POWERS: u32 = 27;

const DOUBLE_POWERS_OF_TEN: [f64; TABLE_POWERS as usize + 1] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27,
];

fn table_power_of_ten(power: i32) -> LongDouble {
    LongDouble::from(
        DOUBLE_POWERS_OF_TEN
            .get(power as usize)
            .copied()
            .unwrap_or(0.0),
    )
}

fn glibc_power_of_ten(power: i32) -> LongDouble {
    let index = (power - GLIBC_LOWEST_POWER) as usize;
    match GLIBC_POWERS_OF_TEN.get(index) {
        Some(&(significand, exponent)) => LongDouble::from_parts(significand, exponent),
        None => LongDouble::from(10f64.powi(power)),
    }
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
    widen_to_nsmall(&mut format, settings.nsmall);
    format.width = format.width.max(candidates.special_width);
    format
}

/// Gives a fixed format at least `nsmall` decimals. `nsmall` widens the format only after the
/// choice between fixed and scientific notation, so it never tips a value into scientific.
fn widen_to_nsmall(format: &mut RealFormat, nsmall: usize) {
    if format.exponent == 0 && format.width > 0 && nsmall > format.decimals {
        format.width += nsmall - format.decimals + (format.decimals == 0) as usize;
        format.decimals = nsmall;
    }
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
    let mut max_significant = 0;
    let mut max_left = i32::MIN;
    let mut min_left = i32::MAX;
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
        let significance = scientific(value, settings.digits);
        let left = significance.kpower + 1 - significance.rounding_widens as i32;
        let sleft = significance.negative as i32 + left.max(1);
        negative |= significance.negative;
        right_max = right_max.max(significance.significant - left);
        max_sleft = max_sleft.max(sleft);
        max_significant = max_significant.max(significance.significant);
        max_left = max_left.max(left);
        min_left = min_left.min(left);
    }
    if max_significant == 0 {
        return RealCandidates {
            notations: None,
            special_width,
        };
    }
    let exponent_digits = if max_left > 100 || min_left <= -99 {
        3
    } else {
        2
    };
    let mantissa_decimals = max_significant as usize - 1;
    let scientific_width = negative as usize
        + (mantissa_decimals > 0) as usize
        + mantissa_decimals
        + 3
        + exponent_digits;
    let right = right_max.max(0) as usize;
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
    let mut text = String::new();
    // Writing into a `String` cannot fail, and the text stays empty if it ever did.
    if push_real_with(&mut text, value, format, na).is_err() {
        text.clear();
    }
    text
}

/// Appends `value` to `out` as [`format_real_with`] formats it.
pub fn push_real_with(
    out: &mut String,
    value: f64,
    format: RealFormat,
    na: &str,
) -> std::fmt::Result {
    if is_na_real(value) {
        out.push_str(na);
        return Ok(());
    }
    if value.is_nan() {
        out.push_str("NaN");
        return Ok(());
    }
    if value.is_infinite() {
        out.push_str(if value < 0.0 { "-Inf" } else { "Inf" });
        return Ok(());
    }
    // GNU R's `encodeReal0` writes a negative zero as a positive one.
    let value = if value == 0.0 { 0.0 } else { value };
    if format.exponent == 0 {
        let begin = out.len();
        if !fixed_fast(out, value, format.decimals) {
            write!(out, "{:.*}", format.decimals, value)?;
        }
        // `-0` prints as `0`.
        let text = &out[begin..];
        if text.starts_with('-') && text[1..].bytes().all(|byte| byte == b'0' || byte == b'.') {
            out.remove(begin);
        }
        return Ok(());
    }
    // Adding zero turns `-0` into `0`, which is how it prints. Rust writes the exponent bare
    // (`1e-5`), and C pads it to two digits with an explicit sign.
    let begin = out.len();
    write!(out, "{:.*e}", format.decimals, value + 0.0)?;
    let marker = out[begin..].rfind('e').ok_or(std::fmt::Error)? + begin;
    let exponent: i32 = out[marker + 1..].parse().map_err(|_| std::fmt::Error)?;
    out.truncate(marker);
    write!(
        out,
        "e{}{:02}",
        if exponent < 0 { '-' } else { '+' },
        exponent.unsigned_abs()
    )
}

/// `value` in scientific notation with `decimals` mantissa decimals and at least two exponent
/// digits. A vector whose exponents need three digits pads its shorter elements to the common
/// width with spaces, not with exponent zeros, as GNU R does.
pub fn scientific_text(value: f64, decimals: usize) -> String {
    let mut text = String::new();
    push_scientific(&mut text, value, decimals);
    text
}

/// [`scientific_text`] appended to `out`: C's `%.*e`. Rust writes the exponent bare (`1e-5`),
/// so it is rewritten in place to C's explicit sign and at least two digits.
pub fn push_scientific(out: &mut String, value: f64, decimals: usize) {
    let begin = out.len();
    // Writing to a `String` cannot fail.
    write!(out, "{:.*e}", decimals, value).ok();
    rewrite_exponent(out, begin);
}

/// Rewrites the bare exponent Rust wrote after the last `e` from `begin` in C's form.
fn rewrite_exponent(out: &mut String, begin: usize) {
    let Some(marker) = out[begin..].rfind('e').map(|offset| begin + offset) else {
        return;
    };
    let (negative, digits) = match out.as_bytes().get(marker + 1) {
        Some(b'-') => (true, marker + 2),
        _ => (false, marker + 1),
    };
    let exponent = out[digits..]
        .bytes()
        .fold(0u32, |sum, digit| sum * 10 + u32::from(digit - b'0'));
    out.truncate(marker + 1);
    out.push(if negative { '-' } else { '+' });
    if exponent < 10 {
        out.push('0');
    }
    write!(out, "{exponent}").ok();
}

/// One double formatted on its own at `digits` significant digits, as `as.character` and
/// `paste` show it (15 digits for those).
pub fn format_real_alone(value: f64, digits: usize) -> String {
    format_real_alone_scipen(value, digits, 0)
}

/// As `format_real_alone`, with a scientific-notation penalty (`options(scipen)`).
pub fn format_real_alone_scipen(value: f64, digits: usize, scipen: i32) -> String {
    let mut text = String::new();
    push_real_alone(&mut text, value, digits, scipen);
    text
}

/// Appends `value` to `out` as [`format_real_alone_scipen`] formats it.
pub fn push_real_alone(out: &mut String, value: f64, digits: usize, scipen: i32) {
    let begin = out.len();
    if value.is_finite() && push_shortest_alone(out, value, digits, scipen).is_some() {
        return;
    }
    out.truncate(begin);
    let settings = Settings {
        digits,
        scipen,
        na_width: 2,
        nsmall: 0,
    };
    let format = format_real_common(&[value], settings);
    // Writing into a `String` cannot fail, and the text stays empty if it ever did.
    if push_real_with(out, value, format, "NA").is_err() {
        out.truncate(begin);
    }
}

/// Whether `options(OutDec)` is the default `"."`, so formatted numbers need no localizing.
pub fn decimal_is_point() -> bool {
    OUTPUT_DECIMAL.with(|decimal| decimal.borrow().as_str() == ".")
}

/// `push_real_alone` for a finite value whose shortest round-trip digits fit in
/// `digits` (see `SHORTEST_EXACT_DIGITS`), built straight from those digits. Those digits are exactly what `scientific` counts
/// and what either notation prints, so one shortest conversion replaces the significance pass
/// and the second, fixed-precision conversion. `None` when the digits do not fit, with `out`
/// left partly written for the caller to cut back.
fn push_shortest_alone(out: &mut String, value: f64, digits: usize, scipen: i32) -> Option<()> {
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
    out.reserve(fixed_width.max(scientific_width));
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
            push_digits(out, significand);
        } else if count <= left as usize {
            // Zeros stand in for digits past the shortest ones only while every integer up to
            // the value is a double, so that the value is exactly the integer they spell; GNU R
            // prints larger values' exact digits.
            if left as usize > SHORTEST_EXACT_DIGITS {
                return None;
            }
            push_digits(out, significand);
            out.extend(std::iter::repeat_n('0', left as usize - count));
        } else {
            let (whole, fraction) = significand.split_at(left as usize);
            push_digits(out, whole);
            out.push('.');
            push_digits(out, fraction);
        }
    } else {
        let (first, rest) = significand.split_at(1);
        push_digits(out, first);
        if !rest.is_empty() {
            out.push('.');
            push_digits(out, rest);
        }
        write!(
            out,
            "e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
        .ok()?;
    }
    Some(())
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

pub(crate) fn output_decimal() -> String {
    OUTPUT_DECIMAL.with(|decimal| decimal.borrow().clone())
}

pub fn set_output_decimal(mark: &str) {
    OUTPUT_DECIMAL.with(|decimal| {
        let mut decimal = decimal.borrow_mut();
        if decimal.as_str() != mark {
            decimal.clear();
            decimal.push_str(mark);
        }
    });
}

/// GNU R 4.5.0 and later warn each time a double is written with a decimal mark that is not one
/// column wide, so a complex number warns once per part and an integer never warns. `count` is
/// the number of doubles about to be written.
pub(crate) unsafe fn warn_decimal_width(count: usize) -> R<()> {
    let width = OUTPUT_DECIMAL.with(|decimal| {
        let decimal = decimal.borrow();
        (decimal.as_str() != ".").then(|| display_width(&decimal))
    });
    let Some(width) = width.filter(|&width| width != 1) else {
        return Ok(());
    };
    if !crate::system::r_at_least(4, 5) {
        return Ok(());
    }
    let message = format!(
        "the decimal mark is {} than one character wide; this will become an error",
        if width > 1 { "more" } else { "less" }
    );
    for _ in 0..count {
        crate::conditions::warning(message.as_str())?;
    }
    Ok(())
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
    // GNU R rounds only to decide which parts print as zero; a nonzero part prints from its
    // unrounded value.
    let rounded = round_complex(value, digits);
    let real_part = if rounded.r == 0.0 { 0.0 } else { value.r };
    let imaginary_part = if rounded.i == 0.0 { 0.0 } else { value.i };
    let real_text = format_real_with(real_part, real, "NA");
    let imaginary_text = format_real_with(imaginary_part.abs(), imaginary, "NA");
    // A negative part rounded to zero keeps its sign unless it prints as a bare `0`.
    let sign = if value.i < 0.0 && imaginary_text != "0" {
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
    let mut real = format_real_common(&reals, settings);
    let imaginary = format_real_common(&imaginaries, settings);
    // From R 4.4 on, each part chooses its notation on its own, and only the space an `NA`
    // needs is shared.
    if crate::system::r_at_least(4, 4) {
        let has_na = rounded.len() < values.len();
        if has_na && real.width + imaginary.width + 2 < settings.na_width {
            real.width = settings.na_width - imaginary.width - 2;
        }
        return (real, imaginary);
    }
    // Through R 4.3, the notation is chosen for both parts together: when either part alone would print in
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
    widen_to_nsmall(&mut real, settings.nsmall);
    widen_to_nsmall(&mut imaginary, settings.nsmall);
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
            return crate::rmath::fround(part * scale, (decimals - 4) as f64) / scale;
        }
        crate::rmath::fround(part, decimals as f64)
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

/// A "bytes" string as GNU R spells it: printable ASCII as itself and every other byte as
/// `\xNN`.
pub fn bytes_spelling(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            0x20..0x80 => text.push(byte as char),
            _ => text.push_str(&format!("\\x{byte:02x}")),
        }
    }
    text
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

/// Pads `out` with spaces on the left to `width` display columns, as [`pad`] does.
fn pad_start(out: &mut String, width: usize) {
    let current = display_width(out);
    if current < width {
        insert_fill(out, 0, ' ', width - current);
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
    let vector = values::read_contents(vector)?;
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
            warn_decimal_width(data.len())?;
            // The common width can exceed every element's own: a negative mantissa and a
            // three-digit exponent each widen it, even when they occur in different elements.
            // GNU R measures the common width with a one-column decimal mark, so a wider mark
            // lengthens a number without widening the rest.
            let items: Vec<String> = data
                .iter()
                .map(|&value| {
                    pad(
                        &format_real_with(value, format, na_string),
                        format.width,
                        true,
                    )
                })
                .collect();
            justify(items, true)
                .into_iter()
                .map(localize_decimal)
                .collect()
        }
        SexpType::Complex => {
            let data = values::complexes(vector);
            let (real, imaginary) = format_complex_common(data, settings);
            let written = data
                .iter()
                .filter(|value| !is_na_real(value.r) && !is_na_real(value.i))
                .count();
            warn_decimal_width(2 * written)?;
            let items: Vec<String> = data
                .iter()
                .map(|&value| {
                    if is_na_real(value.r) || is_na_real(value.i) {
                        na_string.to_owned()
                    } else {
                        format_complex_with(value, real, imaginary, settings.digits)
                    }
                })
                .collect();
            justify(items, true)
                .into_iter()
                .map(localize_decimal)
                .collect()
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
                    None => na_string.to_owned(),
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
pub(crate) unsafe fn do_format(call: &Call<'_>) -> R<Sexp> {
    let vector = values::materialize(call.arg(0))?;
    // GNU R's `do_format` checks in this order and reports against the closure that called
    // `.Internal(format())`, as `error()` does.
    let trim = values::as_logical_scalar(call.arg(1));
    if trim == NA_LOGICAL {
        return Err(error("invalid 'trim' argument"));
    }
    let trim = trim == 1;
    let digits = call.arg(2);
    let mut settings = if digits == nil() {
        Settings::current()
    } else {
        let digits = values::as_integer_scalar(digits);
        if digits == NA_INTEGER || !(1..=22).contains(&digits) {
            return Err(error(format!(
                "invalid value {digits} for 'digits' argument"
            )));
        }
        Settings::with_digits(digits as usize)
    };
    let nsmall = values::as_integer_scalar(call.arg(3));
    if nsmall == NA_INTEGER || !(0..=20).contains(&nsmall) {
        return Err(error("invalid 'nsmall' argument"));
    }
    settings.nsmall = nsmall as usize;
    let width = call.arg(4);
    let min_width = if width == nil() {
        0
    } else {
        let width = values::as_integer_scalar(width);
        if width == NA_INTEGER {
            return Err(error("invalid 'width' argument"));
        }
        width.max(0) as usize
    };
    let justify_mode = values::as_integer_scalar(call.arg(5));
    if justify_mode == NA_INTEGER || !(0..=3).contains(&justify_mode) {
        return Err(error("invalid 'justify' argument"));
    }
    let na_encode = values::as_logical_scalar(call.arg(6));
    if na_encode == NA_LOGICAL {
        return Err(error("invalid 'na.encode' argument"));
    }
    let na_encode = na_encode != 0;
    let scientific = call.arg(7);
    if length(scientific) != 1 {
        return Err(error("invalid 'scientific' argument"));
    }
    match scientific.kind() {
        // `NA`, the default of `format.default`, keeps `options(scipen)`. `FALSE` is GNU R's
        // penalty of 310, which is not absolute: `.Machine$double.xmax` prints all 309 digits,
        // but a value needing more than 310 extra columns of fixed notation stays scientific.
        SexpType::Logical => match values::as_logical_scalar(scientific) {
            1 => settings.scipen = -99,
            0 => settings.scipen = 310,
            _ => {}
        },
        SexpType::Integer | SexpType::Real if !values::inherits(scientific, "factor") => {
            let penalty = values::as_integer_scalar(scientific);
            if penalty != NA_INTEGER {
                settings.scipen = penalty;
            }
        }
        _ => return Err(error("invalid 'scientific' argument")),
    }
    let decimal_mark = call.arg(8);
    if decimal_mark.kind() != SexpType::String || length(decimal_mark) != 1 {
        return Err(error("invalid 'decimal.mark' argument"));
    }
    let items: Vec<Option<String>> = match vector.kind() {
        SexpType::String => {
            let strings = values::strings(vector);
            // A "bytes" string becomes plain text holding its `\xNN` spelling, and every string
            // is measured as `print` would escape it, so a backslash takes two columns.
            let texts: Vec<String> = strings
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let charsxp = vector.element(index);
                    match item {
                        Some(_) if charsxp.has_gp(gp::BYTES) => bytes_spelling(charsxp.bytes()),
                        Some(text) => text.clone(),
                        None => "NA".into(),
                    }
                })
                .collect();
            let widths: Vec<usize> = texts
                .iter()
                .map(|text| display_width(&encode_string(text, None)))
                .collect();
            let common = widths.iter().copied().max().unwrap_or(0).max(min_width);
            strings
                .iter()
                .zip(texts)
                .zip(widths)
                .map(|((original, text), width)| {
                    if original.is_none() && !na_encode {
                        return None;
                    }
                    let total = common.saturating_sub(width);
                    Some(match justify_mode {
                        0 => text + &" ".repeat(total),
                        1 => " ".repeat(total) + &text,
                        2 => {
                            let left = total / 2;
                            format!("{}{}{}", " ".repeat(left), text, " ".repeat(total - left))
                        }
                        _ => text,
                    })
                })
                .collect()
        }
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex => {
            // The mark usually is the current `OutDec` already, which then needs no swap.
            let mark = values::first_string(call.arg(8));
            let saved = mark
                .as_deref()
                .filter(|&mark| OUTPUT_DECIMAL.with(|decimal| decimal.borrow().as_str() != mark))
                .map(|mark| {
                    let saved = output_decimal();
                    set_output_decimal(mark);
                    saved
                });
            let formatted = (|| -> R<Result<Sexp, Vec<String>>> {
                // SAFETY: `vector` is rooted as the call's argument.
                unsafe {
                    Ok(match format_numbers(vector, settings, trim, min_width)? {
                        Some(result) => Ok(result),
                        None => Err(format_vector(vector, settings, false, "NA")?),
                    })
                }
            })();
            if let Some(saved) = &saved {
                set_output_decimal(saved);
            }
            let mut items = match formatted? {
                Ok(result) => {
                    copy_format_attributes(vector, result)?;
                    return Ok(result);
                }
                Err(items) => items,
            };
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
        SexpType::ExternalPtr => vec![Some(crate::print::pointer_text(vector))],
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
    copy_format_attributes(vector, result)?;
    Ok(result)
}

/// `format` of a logical, integer or double vector, written into the result element by element.
/// Formatting every element to a Rust string first and interning them afterwards would hold a
/// million heap strings at once for a million elements, which more than doubles the call's peak
/// memory. Returns `None` when the vector needs the general path: a complex vector, or double
/// elements whose texts differ in width, so that justifying them changes them.
unsafe fn format_numbers(
    vector: Sexp,
    settings: Settings,
    trim: bool,
    min_width: usize,
) -> R<Option<Sexp>> {
    const NA: &str = "NA";
    let vector = values::materialize(vector)?;
    let count = length(vector);
    let real_format = match vector.kind() {
        SexpType::Real => Some(format_real_common(
            values::reals(vector),
            Settings {
                na_width: display_width(NA),
                ..settings
            },
        )),
        SexpType::Logical | SexpType::Integer => None,
        _ => return Ok(None),
    };
    // Logical and integer texts are justified to the widest one, which their values give
    // without formatting them.
    let common = match vector.kind() {
        SexpType::Logical => values::ints(vector)
            .iter()
            .map(|&value| match value {
                NA_INTEGER => NA.len(),
                value => format_logical(value).len(),
            })
            .max(),
        SexpType::Integer => values::ints(vector)
            .iter()
            .map(|&value| match value {
                NA_INTEGER => NA.len(),
                value => {
                    usize::from(value < 0)
                        + value
                            .unsigned_abs()
                            .checked_ilog10()
                            .map_or(1, |digits| digits as usize + 1)
                }
            })
            .max(),
        _ => None,
    }
    .unwrap_or(0);
    let localized = !decimal_is_point();
    let result = alloc(SexpType::String, count)?;
    let mut width = None;
    for index in 0..count {
        let fits = with_text_buffer(|out| {
            match (vector.kind(), real_format) {
                (SexpType::Real, Some(format)) => {
                    push_real_with(out, values::reals(vector)[index], format, NA)
                        .map_err(|_| error("formatting a number failed"))?;
                    pad_start(out, format.width);
                    if localized {
                        *out = localize_decimal(std::mem::take(out));
                    }
                }
                (SexpType::Logical, _) => {
                    out.push_str(match values::ints(vector)[index] {
                        NA_INTEGER => NA,
                        value => format_logical(value),
                    });
                    pad_start(out, common);
                }
                _ => {
                    match values::ints(vector)[index] {
                        NA_INTEGER => out.push_str(NA),
                        value => write!(out, "{value}")
                            .map_err(|_| error("formatting a number failed"))?,
                    }
                    pad_start(out, common);
                }
            }
            if real_format.is_some()
                && *width.get_or_insert(display_width(out)) != display_width(out)
            {
                return Ok(false);
            }
            if trim {
                let spaces = out.len() - out.trim_start().len();
                out.drain(..spaces);
            }
            pad_start(out, min_width);
            values::set_mkchar(result, index, out)?;
            Ok(true)
        })?;
        if !fits {
            return Ok(None);
        }
    }
    // Warned only once the result is final: the general path warns for itself.
    if real_format.is_some() {
        warn_decimal_width(count)?;
    }
    Ok(Some(result))
}

unsafe fn copy_format_attributes(vector: Sexp, result: Sexp) -> R<()> {
    for symbol in [symbols().names, symbols().dim, symbols().dimnames] {
        let value = values::get_attr(vector, symbol);
        if value != nil() {
            values::set_attr(result, symbol, value)?;
        }
    }
    Ok(())
}

/// `formatC(x, mode, width, digits, format, flag, i.strlen)`: C-style formatting per element.
pub(crate) unsafe fn do_format_c(call: &Call<'_>) -> R<Sexp> {
    let vector = call.arg(0);
    let mode = values::require_str(call.arg(1), "mode")?;
    let width = values::as_integer_scalar(call.arg(2));
    let digits = values::as_integer_scalar(call.arg(3));
    let format = values::require_str(call.arg(4), "format")?;
    let flag = values::require_str(call.arg(5), "flag")?;
    if !matches!(
        vector.kind(),
        SexpType::Integer | SexpType::Real | SexpType::String
    ) {
        return Err(call.error("unsupported type "));
    }
    format_c_vector(vector, &mode, width, digits, &format, &flag)
}

/// The body of `formatC()`'s internal on an integer, double or character `vector`, for callers
/// that hold the arguments as Rust values. `mode` is `"character"`, `"integer"` or another mode
/// that formats doubles.
pub(crate) unsafe fn format_c_vector(
    vector: Sexp,
    mode: &str,
    width: i32,
    digits: i32,
    format: &str,
    flag: &str,
) -> R<Sexp> {
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
            // GNU R writes an `fg` zero as a bare "0", whatever the width and flag.
            if format == "fg" && value == 0.0 {
                number.push('0');
            } else {
                c_format_into(&mut number, value, format, digits, flag, integer);
            }
            &number
        };
        let shortfall = width.saturating_sub(display_width(text));
        if shortfall == 0 || (format == "fg" && text == "0") {
            store_string(result, index, text, &mut memo)?;
            continue;
        }
        padded.clear();
        if left_justify {
            padded.push_str(text);
            padded.extend(std::iter::repeat_n(' ', shortfall));
        } else if flag.contains('0') && !text.contains(|c: char| c.is_alphabetic() && c != 'e') {
            let sign_length = usize::from(text.starts_with(['-', '+', ' ']));
            let (sign, digits) = text.split_at(sign_length);
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
        "f" => push_fixed(out, value, precision),
        "e" | "E" => {
            let begin = out.len();
            push_scientific(out, value, precision);
            if format == "E" {
                out[begin..].make_ascii_uppercase();
            }
        }
        "fg" => out.push_str(&fixed_significant(
            value,
            precision.max(1),
            digits < 0 || flag.contains('#'),
        )),
        "g" | "G" => {
            let begin = out.len();
            push_general(out, value, precision.max(1), flag.contains('#'));
            if format == "G" {
                out[begin..].make_ascii_uppercase();
            }
        }
        _ => {
            if integer {
                write!(out, "{}", value as i64).ok();
            } else {
                push_general(out, value, precision.max(1), false);
            }
        }
    }
}

/// C's `%.*f`, appended to `out`.
pub fn push_fixed(out: &mut String, value: f64, precision: usize) {
    if !fixed_fast(out, value, precision) {
        // Writing to a `String` cannot fail.
        write!(out, "{:.*}", precision, value).ok();
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
    // The text is built backwards in a stack buffer and pushed once: the fraction digits (zeros
    // where the number has fewer), the point, the integral digits, then the sign. The scaled
    // value is below 2^52, so it has at most 16 digits, and the precision is at most 15.
    let mut buffer = [0u8; 40];
    let mut cursor = buffer.len();
    let mut rest = digits;
    for _ in 0..precision {
        cursor -= 1;
        buffer[cursor] = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
    if precision > 0 {
        cursor -= 1;
        buffer[cursor] = b'.';
    }
    loop {
        cursor -= 1;
        buffer[cursor] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    // C keeps the sign of any negative value, including one that rounds to zero.
    if value.is_sign_negative() {
        cursor -= 1;
        buffer[cursor] = b'-';
    }
    // SAFETY: the buffer holds only ASCII digits, the point and the sign.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buffer[cursor..]) });
    true
}

/// C's `%g`: the shorter of `%e` and `%f` at `significant` digits, without trailing zeros.
///
/// `keep_zeros` is C's `#` flag: trailing zeros stay, and the decimal point stays even with no
/// digits after it.
pub fn c_general(value: f64, significant: usize, keep_zeros: bool) -> String {
    let mut text = String::new();
    push_general(&mut text, value, significant, keep_zeros);
    text
}

/// [`c_general`] appended to `out`.
pub fn push_general(out: &mut String, value: f64, significant: usize, keep_zeros: bool) {
    if value == 0.0 && !keep_zeros {
        // C keeps the sign of a negative zero.
        out.push_str(if value.is_sign_negative() { "-0" } else { "0" });
        return;
    }
    if value.is_sign_negative() {
        out.push('-');
    }
    let magnitude = value.abs();
    if !magnitude.is_finite() {
        out.push_str(if magnitude.is_nan() { "nan" } else { "inf" });
        return;
    }
    let begin = out.len();
    // The exponent of the value rounded to `significant` digits picks the notation, so the
    // scientific form is written first and kept when it wins.
    write!(out, "{:.*e}", significant.saturating_sub(1), magnitude).ok();
    let marker = out[begin..]
        .rfind('e')
        .map_or(out.len(), |offset| begin + offset);
    let exponent: i32 = out[marker + 1..].parse().unwrap_or(0);
    if exponent < -4 || exponent >= significant as i32 {
        if keep_zeros {
            // glibc drops the mantissa zeros when rounding carries a value just below
            // 10^significant up to it, so 99.95 at two digits prints as `1.e+02`, not `1.0e+02`.
            let carried = exponent == significant as i32
                && format!("1e{significant}")
                    .parse::<f64>()
                    .is_ok_and(|power| magnitude < power);
            if carried {
                out.replace_range(begin..marker, "1.");
            } else if significant <= 1 {
                out.insert(marker, '.');
            }
            rewrite_exponent(out, begin);
        } else {
            let kept = begin + trim_zeros(&out[begin..marker]).len();
            out.replace_range(kept..marker, "");
            rewrite_exponent(out, begin);
        }
    } else {
        out.truncate(begin);
        let decimals = (significant as i32 - 1 - exponent).max(0) as usize;
        push_fixed(out, magnitude, decimals);
        if keep_zeros {
            if decimals == 0 {
                out.push('.');
            }
        } else {
            let kept = begin + trim_zeros(&out[begin..]).len();
            out.truncate(kept);
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

/// The lowest power in [`GLIBC_POWERS_OF_TEN`]. The scaling powers `scientific` needs run from
/// `-338` (the smallest subnormal at 15 digits) to `308` (the largest double at 1 digit).
const GLIBC_LOWEST_POWER: i32 = -340;

/// glibc's x86-64 `powl(10, k)` for `k` from `-340` to `310`, as a significand with its top bit
/// set and a power of two. 65 of these differ from the correctly rounded power by one unit in
/// the last place, and GNU R's digit counts follow glibc's.
#[rustfmt::skip]
const GLIBC_POWERS_OF_TEN: [(u64, i32); 651] = [
    (0xbaaee17fa23ebf76, -1193),
    (0xe95a99df8ace6f54, -1190),
    (0x91d8a02bb6c10594, -1186),
    (0xb64ec836a47146fa, -1183),
    (0xe3e27a444d8d98b8, -1180),
    (0x8e6d8c6ab0787f73, -1176),
    (0xb208ef855c969f50, -1173),
    (0xde8b2b66b3bc4724, -1170),
    (0x8b16fb203055ac76, -1166),
    (0xaddcb9e83c6b1794, -1163),
    (0xd953e8624b85dd79, -1160),
    (0x87d4713d6f33aa6c, -1156),
    (0xa9c98d8ccb009506, -1153),
    (0xd43bf0effdc0ba48, -1150),
    (0x84a57695fe98746d, -1146),
    (0xa5ced43b7e3e9188, -1143),
    (0xcf42894a5dce35ea, -1140),
    (0x818995ce7aa0e1b2, -1136),
    (0xa1ebfb4219491a1f, -1133),
    (0xca66fa129f9b60a7, -1130),
    (0xfd00b897478238d1, -1127),
    (0x9e20735e8cb16382, -1123),
    (0xc5a890362fddbc63, -1120),
    (0xf712b443bbd52b7c, -1117),
    (0x9a6bb0aa55653b2d, -1113),
    (0xc1069cd4eabe89f9, -1110),
    (0xf148440a256e2c77, -1107),
    (0x96cd2a865764dbca, -1103),
    (0xbc807527ed3e12bd, -1100),
    (0xeba09271e88d976c, -1097),
    (0x93445b8731587ea4, -1093),
    (0xb8157268fdae9e4c, -1090),
    (0xe61acf033d1a45df, -1087),
    (0x8fd0c16206306bac, -1083),
    (0xb3c4f1ba87bc8696, -1080),
    (0xe0b62e2929aba83c, -1077),
    (0x8c71dcd9ba0b4926, -1073),
    (0xaf8e5410288e1b6f, -1070),
    (0xdb71e91432b1a24b, -1067),
    (0x892731ac9faf056f, -1063),
    (0xab70fe17c79ac6ca, -1060),
    (0xd64d3d9db981787d, -1057),
    (0x85f0468293f0eb4e, -1053),
    (0xa76c582338ed2622, -1050),
    (0xd1476e2c07286faa, -1047),
    (0x82cca4db847945ca, -1043),
    (0xa37fce126597973d, -1040),
    (0xcc5fc196fefd7d0c, -1037),
    (0xff77b1fcbebcdc4f, -1034),
    (0x9faacf3df73609b2, -1030),
    (0xc795830d75038c1e, -1027),
    (0xf97ae3d0d2446f25, -1024),
    (0x9becce62836ac577, -1020),
    (0xc2e801fb244576d5, -1017),
    (0xf3a20279ed56d48a, -1014),
    (0x9845418c345644d6, -1010),
    (0xbe5691ef416bd60c, -1007),
    (0xedec366b11c6cb8f, -1004),
    (0x94b3a202eb1c3f3a, -1000),
    (0xb9e08a83a5e34f08, -997),
    (0xe858ad248f5c22ca, -994),
    (0x91376c36d99995be, -990),
    (0xb58547448ffffb2e, -987),
    (0xe2e69915b3fff9f9, -984),
    (0x8dd01fad907ffc3c, -980),
    (0xb1442798f49ffb4a, -977),
    (0xdd95317f31c7fa1d, -974),
    (0x8a7d3eef7f1cfc52, -970),
    (0xad1c8eab5ee43b67, -967),
    (0xd863b256369d4a40, -964),
    (0x873e4f75e2224e68, -960),
    (0xa90de3535aaae202, -957),
    (0xd3515c2831559a83, -954),
    (0x8412d9991ed58092, -950),
    (0xa5178fff668ae0b6, -947),
    (0xce5d73ff402d98e4, -944),
    (0x80fa687f881c7f8e, -940),
    (0xa139029f6a239f72, -937),
    (0xc987434744ac874f, -934),
    (0xfbe9141915d7a922, -931),
    (0x9d71ac8fada6c9b5, -927),
    (0xc4ce17b399107c23, -924),
    (0xf6019da07f549b2c, -921),
    (0x99c102844f94e0fb, -917),
    (0xc0314325637a193a, -914),
    (0xf03d93eebc589f88, -911),
    (0x96267c7535b763b5, -907),
    (0xbbb01b9283253ca2, -904),
    (0xea9c227723ee8bcb, -901),
    (0x92a1958a7675175f, -897),
    (0xb749faed14125d36, -894),
    (0xe51c79a85916f484, -891),
    (0x8f31cc0937ae58d3, -887),
    (0xb2fe3f0b8599ef08, -884),
    (0xdfbdcece67006aca, -881),
    (0x8bd6a141006042be, -877),
    (0xaecc49914078536d, -874),
    (0xda7f5bf590966849, -871),
    (0x888f99797a5e012d, -867),
    (0xaab37fd7d8f58179, -864),
    (0xd5605fcdcf32e1d7, -861),
    (0x855c3be0a17fcd26, -857),
    (0xa6b34ad8c9dfc070, -854),
    (0xd0601d8efc57b08c, -851),
    (0x823c12795db6ce57, -847),
    (0xa2cb1717b52481ee, -844),
    (0xcb7ddcdda26da269, -841),
    (0xfe5d54150b090b03, -838),
    (0x9efa548d26e5a6e2, -834),
    (0xc6b8e9b0709f109a, -831),
    (0xf867241c8cc6d4c1, -828),
    (0x9b407691d7fc44f8, -824),
    (0xc21094364dfb5637, -821),
    (0xf294b943e17a2bc4, -818),
    (0x979cf3ca6cec5b5b, -814),
    (0xbd8430bd08277232, -811),
    (0xece53cec4a314ebe, -808),
    (0x940f4613ae5ed136, -804),
    (0xb913179899f68584, -801),
    (0xe757dd7ec07426e5, -798),
    (0x9096ea6f3848984f, -794),
    (0xb4bca50b065abe63, -791),
    (0xe1ebce4dc7f16dfc, -788),
    (0x8d3360f09cf6e4bd, -784),
    (0xb080392cc4349ded, -781),
    (0xdca04777f541c568, -778),
    (0x89e42caaf9491b61, -774),
    (0xac5d37d5b79b6239, -771),
    (0xd77485cb25823ac8, -768),
    (0x86a8d39ef77164bd, -764),
    (0xa8530886b54dbdec, -761),
    (0xd267caa862a12d67, -758),
    (0x8380dea93da4bc60, -754),
    (0xa46116538d0deb78, -751),
    (0xcd795be870516656, -748),
    (0x806bd9714632dff6, -744),
    (0xa086cfcd97bf97f4, -741),
    (0xc8a883c0fdaf7df0, -738),
    (0xfad2a4b13d1b5d6c, -735),
    (0x9cc3a6eec6311a64, -731),
    (0xc3f490aa77bd60fd, -728),
    (0xf4f1b4d515acb93c, -725),
    (0x991711052d8bf3c5, -721),
    (0xbf5cd54678eef0b7, -718),
    (0xef340a98172aace4, -715),
    (0x9580869f0e7aac0f, -711),
    (0xbae0a846d2195712, -708),
    (0xe998d258869facd7, -705),
    (0x91ff83775423cc06, -701),
    (0xb67f6455292cbf08, -698),
    (0xe41f3d6a7377eeca, -695),
    (0x8e938662882af53e, -691),
    (0xb23867fb2a35b28e, -688),
    (0xdec681f9f4c31f32, -685),
    (0x8b3c113c38f9f37f, -681),
    (0xae0b158b4738705e, -678),
    (0xd98ddaee19068c76, -675),
    (0x87f8a8d4cfa417ca, -671),
    (0xa9f6d30a038d1dbc, -668),
    (0xd47487cc8470652c, -665),
    (0x84c8d4dfd2c63f3b, -661),
    (0xa5fb0a17c777cf0a, -658),
    (0xcf79cc9db955c2cc, -655),
    (0x81ac1fe293d599c0, -651),
    (0xa21727db38cb0030, -648),
    (0xca9cf1d206fdc03c, -645),
    (0xfd442e4688bd304b, -642),
    (0x9e4a9cec15763e2e, -638),
    (0xc5dd44271ad3cdba, -635),
    (0xf7549530e188c129, -632),
    (0x9a94dd3e8cf578ba, -628),
    (0xc13a148e3032d6e8, -625),
    (0xf18899b1bc3f8ca2, -622),
    (0x96f5600f15a7b7e5, -618),
    (0xbcb2b812db11a5de, -615),
    (0xebdf661791d60f56, -612),
    (0x936b9fcebb25c996, -608),
    (0xb84687c269ef3bfb, -605),
    (0xe65829b3046b0afa, -602),
    (0x8ff71a0fe2c2e6dc, -598),
    (0xb3f4e093db73a093, -595),
    (0xe0f218b8d25088b8, -592),
    (0x8c974f7383725573, -588),
    (0xafbd2350644eead0, -585),
    (0xdbac6c247d62a584, -582),
    (0x894bc396ce5da772, -578),
    (0xab9eb47c81f5114f, -575),
    (0xd686619ba27255a3, -572),
    (0x8613fd0145877586, -568),
    (0xa798fc4196e952e7, -565),
    (0xd17f3b51fca3a7a1, -562),
    (0x82ef85133de648c5, -558),
    (0xa3ab66580d5fdaf6, -555),
    (0xcc963fee10b7d1b3, -552),
    (0xffbbcfe994e5c620, -549),
    (0x9fd561f1fd0f9bd4, -545),
    (0xc7caba6e7c5382c9, -542),
    (0xf9bd690a1b68637b, -539),
    (0x9c1661a651213e2d, -535),
    (0xc31bfa0fe5698db8, -532),
    (0xf3e2f893dec3f126, -529),
    (0x986ddb5c6b3a76b8, -525),
    (0xbe89523386091466, -522),
    (0xee2ba6c0678b5980, -519),
    (0x94db483840b717f0, -515),
    (0xba121a4650e4ddec, -512),
    (0xe896a0d7e51e1566, -509),
    (0x915e2486ef32cd60, -505),
    (0xb5b5ada8aaff80b8, -502),
    (0xe3231912d5bf60e6, -499),
    (0x8df5efabc5979c90, -495),
    (0xb1736b96b6fd83b4, -492),
    (0xddd0467c64bce4a1, -489),
    (0x8aa22c0dbef60ee4, -485),
    (0xad4ab7112eb3929e, -482),
    (0xd89d64d57a607745, -479),
    (0x87625f056c7c4a8b, -475),
    (0xa93af6c6c79b5d2e, -472),
    (0xd389b47879823479, -469),
    (0x843610cb4bf160cc, -465),
    (0xa54394fe1eedb8ff, -462),
    (0xce947a3da6a9273e, -459),
    (0x811ccc668829b887, -455),
    (0xa163ff802a3426a9, -452),
    (0xc9bcff6034c13053, -449),
    (0xfc2c3f3841f17c68, -446),
    (0x9d9ba7832936edc1, -442),
    (0xc5029163f384a931, -439),
    (0xf64335bcf065d37d, -436),
    (0x99ea0196163fa42e, -432),
    (0xc06481fb9bcf8d3a, -429),
    (0xf07da27a82c37088, -426),
    (0x964e858c91ba2655, -422),
    (0xbbe226efb628afea, -419),
    (0xeadab0aba3b2dbe5, -416),
    (0x92c8ae6b464fc96f, -412),
    (0xb77ada0617e3bbcb, -409),
    (0xe55990879ddcaabe, -406),
    (0x8f57fa54c2a9eab7, -402),
    (0xb32df8e9f3546564, -399),
    (0xdff9772470297ebe, -396),
    (0x8bfbea76c619ef36, -392),
    (0xaefae51477a06b04, -389),
    (0xdab99e59958885c5, -386),
    (0x88b402f7fd75539b, -382),
    (0xaae103b5fcd2a882, -379),
    (0xd59944a37c0752a2, -376),
    (0x857fcae62d8493a5, -372),
    (0xa6dfbd9fb8e5b88f, -369),
    (0xd097ad07a71f26b2, -366),
    (0x825ecc24c8737830, -362),
    (0xa2f67f2dfa90563b, -359),
    (0xcbb41ef979346bca, -356),
    (0xfea126b7d78186bd, -353),
    (0x9f24b832e6b0f436, -349),
    (0xc6ede63fa05d3144, -346),
    (0xf8a95fcf88747d94, -343),
    (0x9b69dbe1b548ce7d, -339),
    (0xc24452da229b021c, -336),
    (0xf2d56790ab41c2a3, -333),
    (0x97c560ba6b0919a6, -329),
    (0xbdb6b8e905cb6010, -326),
    (0xed246723473e3813, -323),
    (0x9436c0760c86e30c, -319),
    (0xb94470938fa89bcf, -316),
    (0xe7958cb87392c2c3, -313),
    (0x90bd77f3483bb9ba, -309),
    (0xb4ecd5f01a4aa828, -306),
    (0xe2280b6c20dd5232, -303),
    (0x8d590723948a535f, -299),
    (0xb0af48ec79ace837, -296),
    (0xdcdb1b2798182245, -293),
    (0x8a08f0f8bf0f156b, -289),
    (0xac8b2d36eed2dac6, -286),
    (0xd7adf884aa879177, -283),
    (0x86ccbb52ea94baeb, -279),
    (0xa87fea27a539e9a5, -276),
    (0xd29fe4b18e88640e, -273),
    (0x83a3eeeef9153e89, -269),
    (0xa48ceaaab75a8e2c, -266),
    (0xcdb02555653131b6, -263),
    (0x808e17555f3ebf12, -259),
    (0xa0b19d2ab70e6ed6, -256),
    (0xc8de047564d20a8c, -253),
    (0xfb158592be068d2f, -250),
    (0x9ced737bb6c4183d, -246),
    (0xc428d05aa4751e4d, -243),
    (0xf53304714d9265e0, -240),
    (0x993fe2c6d07b7fac, -236),
    (0xbf8fdb78849a5f97, -233),
    (0xef73d256a5c0f77d, -230),
    (0x95a8637627989aae, -226),
    (0xbb127c53b17ec159, -223),
    (0xe9d71b689dde71b0, -220),
    (0x9226712162ab070e, -216),
    (0xb6b00d69bb55c8d1, -213),
    (0xe45c10c42a2b3b06, -210),
    (0x8eb98a7a9a5b04e3, -206),
    (0xb267ed1940f1c61c, -203),
    (0xdf01e85f912e37a3, -200),
    (0x8b61313bbabce2c6, -196),
    (0xae397d8aa96c1b78, -193),
    (0xd9c7dced53c72256, -190),
    (0x881cea14545c7576, -186),
    (0xaa242499697392d3, -183),
    (0xd4ad2dbfc3d07788, -180),
    (0x84ec3c97da624ab5, -176),
    (0xa6274bbdd0fadd62, -173),
    (0xcfb11ead453994ba, -170),
    (0x81ceb32c4b43fcf5, -166),
    (0xa2425ff75e14fc32, -163),
    (0xcad2f7f5359a3b3e, -160),
    (0xfd87b5f28300ca0e, -157),
    (0x9e74d1b791e07e48, -153),
    (0xc612062576589ddb, -150),
    (0xf79687aed3eec551, -147),
    (0x9abe14cd44753b53, -143),
    (0xc16d9a0095928a27, -140),
    (0xf1c90080baf72cb1, -137),
    (0x971da05074da7bef, -133),
    (0xbce5086492111aea, -130),
    (0xec1e4a7db69561a5, -127),
    (0x9392ee8e921d5d07, -123),
    (0xb877aa3236a4b449, -120),
    (0xe69594bec44de15b, -117),
    (0x901d7cf73ab0acd9, -113),
    (0xb424dc35095cd80f, -110),
    (0xe12e13424bb40e13, -107),
    (0x8cbccc096f5088cc, -103),
    (0xafebff0bcb24aaff, -100),
    (0xdbe6fecebdedd5bf, -97),
    (0x89705f4136b4a597, -93),
    (0xabcc77118461cefd, -90),
    (0xd6bf94d5e57a42bc, -87),
    (0x8637bd05af6c69b6, -83),
    (0xa7c5ac471b478423, -80),
    (0xd1b71758e219652c, -77),
    (0x83126e978d4fdf3c, -73),
    (0xa3d70a3d70a3d70b, -70),
    (0xcccccccccccccccd, -67),
    (0x8000000000000000, -63),
    (0xa000000000000000, -60),
    (0xc800000000000000, -57),
    (0xfa00000000000000, -54),
    (0x9c40000000000000, -50),
    (0xc350000000000000, -47),
    (0xf424000000000000, -44),
    (0x9896800000000000, -40),
    (0xbebc200000000000, -37),
    (0xee6b280000000000, -34),
    (0x9502f90000000000, -30),
    (0xba43b74000000000, -27),
    (0xe8d4a51000000000, -24),
    (0x9184e72a00000000, -20),
    (0xb5e620f480000000, -17),
    (0xe35fa931a0000000, -14),
    (0x8e1bc9bf04000000, -10),
    (0xb1a2bc2ec5000000, -7),
    (0xde0b6b3a76400000, -4),
    (0x8ac7230489e80000, 0),
    (0xad78ebc5ac620000, 3),
    (0xd8d726b7177a8000, 6),
    (0x878678326eac9000, 10),
    (0xa968163f0a57b400, 13),
    (0xd3c21bcecceda100, 16),
    (0x84595161401484a0, 20),
    (0xa56fa5b99019a5c8, 23),
    (0xcecb8f27f4200f3a, 26),
    (0x813f3978f8940984, 30),
    (0xa18f07d736b90be5, 33),
    (0xc9f2c9cd04674edf, 36),
    (0xfc6f7c4045812296, 39),
    (0x9dc5ada82b70b59e, 43),
    (0xc5371912364ce305, 46),
    (0xf684df56c3e01bc7, 49),
    (0x9a130b963a6c115c, 53),
    (0xc097ce7bc90715b3, 56),
    (0xf0bdc21abb48db20, 59),
    (0x96769950b50d88f4, 63),
    (0xbc143fa4e250eb31, 66),
    (0xeb194f8e1ae525fd, 69),
    (0x92efd1b8d0cf37be, 73),
    (0xb7abc627050305ae, 76),
    (0xe596b7b0c643c71a, 79),
    (0x8f7e32ce7bea5c70, 83),
    (0xb35dbf821ae4f38c, 86),
    (0xe0352f62a19e306f, 89),
    (0x8c213d9da502de45, 93),
    (0xaf298d050e4395d7, 96),
    (0xdaf3f04651d47b4c, 99),
    (0x88d8762bf324cd10, 103),
    (0xab0e93b6efee0054, 106),
    (0xd5d238a4abe98068, 109),
    (0x85a36366eb71f041, 113),
    (0xa70c3c40a64e6c52, 116),
    (0xd0cf4b50cfe20766, 119),
    (0x82818f1281ed44a0, 123),
    (0xa321f2d7226895c8, 126),
    (0xcbea6f8ceb02bb3a, 129),
    (0xfee50b7025c36a08, 132),
    (0x9f4f2726179a2245, 136),
    (0xc722f0ef9d80aad6, 139),
    (0xf8ebad2b84e0d58c, 142),
    (0x9b934c3b330c8577, 146),
    (0xc2781f49ffcfa6d5, 149),
    (0xf316271c7fc3908b, 152),
    (0x97edd871cfda3a57, 156),
    (0xbde94e8e43d0c8ec, 159),
    (0xed63a231d4c4fb27, 162),
    (0x945e455f24fb1cf9, 166),
    (0xb975d6b6ee39e436, 169),
    (0xe7d34c64a9c85d44, 172),
    (0x90e40fbeea1d3a4b, 176),
    (0xb51d13aea4a488de, 179),
    (0xe264589a4dcdab15, 182),
    (0x8d7eb76070a08aed, 186),
    (0xb0de65388cc8ada8, 189),
    (0xdd15fe86affad912, 192),
    (0x8a2dbf142dfcc7ab, 196),
    (0xacb92ed9397bf996, 199),
    (0xd7e77a8f87daf7fc, 202),
    (0x86f0ac99b4e8dafd, 206),
    (0xa8acd7c0222311bd, 209),
    (0xd2d80db02aabd62c, 212),
    (0x83c7088e1aab65db, 216),
    (0xa4b8cab1a1563f52, 219),
    (0xcde6fd5e09abcf27, 222),
    (0x80b05e5ac60b6178, 226),
    (0xa0dc75f1778e39d6, 229),
    (0xc913936dd571c84c, 232),
    (0xfb5878494ace3a5f, 235),
    (0x9d174b2dcec0e47b, 239),
    (0xc45d1df942711d9a, 242),
    (0xf5746577930d6501, 245),
    (0x9968bf6abbe85f20, 249),
    (0xbfc2ef456ae276e8, 252),
    (0xefb3ab16c59b14a3, 255),
    (0x95d04aee3b80ece6, 259),
    (0xbb445da9ca61281f, 262),
    (0xea1575143cf97227, 265),
    (0x924d692ca61be758, 269),
    (0xb6e0c377cfa2e12e, 272),
    (0xe498f455c38b997a, 275),
    (0x8edf98b59a373fec, 279),
    (0xb2977ee300c50fe8, 282),
    (0xdf3d5e9bc0f653e1, 285),
    (0x8b865b215899f46d, 289),
    (0xae67f1e9aec07188, 292),
    (0xda01ee641a708dea, 295),
    (0x884134fe908658b2, 299),
    (0xaa51823e34a7eedf, 302),
    (0xd4e5e2cdc1d1ea96, 305),
    (0x850fadc09923329e, 309),
    (0xa6539930bf6bff46, 312),
    (0xcfe87f7cef46ff17, 315),
    (0x81f14fae158c5f6e, 319),
    (0xa26da3999aef774a, 322),
    (0xcb090c8001ab551c, 325),
    (0xfdcb4fa002162a63, 328),
    (0x9e9f11c4014dda7e, 332),
    (0xc646d63501a1511e, 335),
    (0xf7d88bc24209a565, 338),
    (0x9ae757596946075f, 342),
    (0xc1a12d2fc3978937, 345),
    (0xf209787bb47d6b85, 348),
    (0x9745eb4d50ce6333, 352),
    (0xbd176620a501fc00, 355),
    (0xec5d3fa8ce427b00, 358),
    (0x93ba47c980e98ce0, 362),
    (0xb8a8d9bbe123f018, 365),
    (0xe6d3102ad96cec1e, 368),
    (0x9043ea1ac7e41392, 372),
    (0xb454e4a179dd1877, 375),
    (0xe16a1dc9d8545e95, 378),
    (0x8ce2529e2734bb1d, 382),
    (0xb01ae745b101e9e4, 385),
    (0xdc21a1171d42645e, 388),
    (0x899504ae72497eba, 392),
    (0xabfa45da0edbde69, 395),
    (0xd6f8d7509292d603, 398),
    (0x865b86925b9bc5c2, 402),
    (0xa7f26836f282b732, 405),
    (0xd1ef0244af2364ff, 408),
    (0x8335616aed761f20, 412),
    (0xa402b9c5a8d3a6e7, 415),
    (0xcd036837130890a1, 418),
    (0x802221226be55a65, 422),
    (0xa02aa96b06deb0fe, 425),
    (0xc83553c5c8965d3d, 428),
    (0xfa42a8b73abbf48d, 431),
    (0x9c69a97284b578d8, 435),
    (0xc38413cf25e2d70e, 438),
    (0xf46518c2ef5b8cd2, 441),
    (0x98bf2f79d5993803, 445),
    (0xbeeefb584aff8604, 448),
    (0xeeaaba2e5dbf6785, 451),
    (0x952ab45cfa97a0b3, 455),
    (0xba756174393d88e0, 458),
    (0xe912b9d1478ceb18, 461),
    (0x91abb422ccb812ef, 465),
    (0xb616a12b7fe617aa, 468),
    (0xe39c49765fdf9d95, 471),
    (0x8e41ade9fbebc27d, 475),
    (0xb1d219647ae6b31c, 478),
    (0xde469fbd99a05fe4, 481),
    (0x8aec23d680043bee, 485),
    (0xada72ccc20054aea, 488),
    (0xd910f7ff28069da4, 491),
    (0x87aa9aff79042287, 495),
    (0xa99541bf57452b28, 498),
    (0xd3fa922f2d1675f2, 501),
    (0x847c9b5d7c2e09b7, 505),
    (0xa59bc234db398c25, 508),
    (0xcf02b2c21207ef2f, 511),
    (0x8161afb94b44f57d, 515),
    (0xa1ba1ba79e1632dc, 518),
    (0xca28a291859bbf94, 521),
    (0xfcb2cb35e702af78, 524),
    (0x9defbf01b061adab, 528),
    (0xc56baec21c7a1916, 531),
    (0xf6c69a72a3989f5c, 534),
    (0x9a3c2087a63f6399, 538),
    (0xc0cb28a98fcf3c80, 541),
    (0xf0fdf2d3f3c30b9f, 544),
    (0x969eb7c47859e744, 548),
    (0xbc4665b596706114, 551),
    (0xeb57ff22fc0c795a, 554),
    (0x9316ff75dd87cbd8, 558),
    (0xb7dcbf5354e9bece, 561),
    (0xe5d3ef282a242e82, 564),
    (0x8fa475791a569d11, 568),
    (0xb38d92d760ec4455, 571),
    (0xe070f78d3927556a, 574),
    (0x8c469ab843b89563, 578),
    (0xaf58416654a6babb, 581),
    (0xdb2e51bfe9d0696a, 584),
    (0x88fcf317f22241e2, 588),
    (0xab3c2fddeeaad25b, 591),
    (0xd60b3bd56a5586f2, 594),
    (0x85c7056562757457, 598),
    (0xa738c6bebb12d16c, 601),
    (0xd106f86e69d785c8, 604),
    (0x82a45b450226b39d, 608),
    (0xa34d721642b06084, 611),
    (0xcc20ce9bd35c78a5, 614),
    (0xff290242c83396ce, 617),
    (0x9f79a169bd203e41, 621),
    (0xc75809c42c684dd1, 624),
    (0xf92e0c3537826146, 627),
    (0x9bbcc7a142b17ccc, 631),
    (0xc2abf989935ddbfe, 634),
    (0xf356f7ebf83552fe, 637),
    (0x98165af37b2153df, 641),
    (0xbe1bf1b059e9a8d6, 644),
    (0xeda2ee1c7064130c, 647),
    (0x9485d4d1c63e8be8, 651),
    (0xb9a74a0637ce2ee2, 654),
    (0xe8111c87c5c1ba9a, 657),
    (0x910ab1d4db9914a0, 661),
    (0xb54d5e4a127f59c8, 664),
    (0xe2a0b5dc971f303a, 667),
    (0x8da471a9de737e24, 671),
    (0xb10d8e1456105dae, 674),
    (0xdd50f1996b947519, 677),
    (0x8a5296ffe33cc930, 681),
    (0xace73cbfdc0bfb7b, 684),
    (0xd8210befd30efa5a, 687),
    (0x8714a775e3e95c78, 691),
    (0xa8d9d1535ce3b396, 694),
    (0xd31045a8341ca07c, 697),
    (0x83ea2b892091e44e, 701),
    (0xa4e4b66b68b65d61, 704),
    (0xce1de40642e3f4b9, 707),
    (0x80d2ae83e9ce78f4, 711),
    (0xa1075a24e4421731, 714),
    (0xc94930ae1d529cfd, 717),
    (0xfb9b7cd9a4a7443c, 720),
    (0x9d412e0806e88aa6, 724),
    (0xc491798a08a2ad4f, 727),
    (0xf5b5d7ec8acb58a3, 730),
    (0x9991a6f3d6bf1766, 734),
    (0xbff610b0cc6edd3f, 737),
    (0xeff394dcff8a948f, 740),
    (0x95f83d0a1fb69cd9, 744),
    (0xbb764c4ca7a44410, 747),
    (0xea53df5fd18d5514, 750),
    (0x92746b9be2f8552c, 754),
    (0xb7118682dbb66a78, 757),
    (0xe4d5e82392a40515, 760),
    (0x8f05b1163ba6832d, 764),
    (0xb2c71d5bca9023f8, 767),
    (0xdf78e4b2bd342cf6, 770),
    (0x8bab8eefb6409c1a, 774),
    (0xae9672aba3d0c321, 777),
    (0xda3c0f568cc4f3e9, 780),
    (0x8865899617fb1872, 784),
    (0xaa7eebfb9df9de8e, 787),
    (0xd51ea6fa85785632, 790),
    (0x8533285c936b35df, 794),
    (0xa67ff273b8460356, 797),
    (0xd01fef10a657842c, 800),
    (0x8213f56a67f6b29c, 804),
    (0xa298f2c501f45f42, 807),
    (0xcb3f2f7642717713, 810),
    (0xfe0efb53d30dd4d8, 813),
    (0x9ec95d1463e8a507, 817),
    (0xc67bb4597ce2ce49, 820),
    (0xf81aa16fdc1b81db, 823),
    (0x9b10a4e5e9913129, 827),
    (0xc1d4ce1f63f57d73, 830),
    (0xf24a01a73cf2dcd0, 833),
    (0x976e41088617ca02, 837),
    (0xbd49d14aa79dbc82, 840),
    (0xec9c459d51852ba3, 843),
    (0x93e1ab8252f33b46, 847),
    (0xb8da1662e7b00a18, 850),
    (0xe7109bfba19c0c9d, 853),
    (0x906a617d450187e2, 857),
    (0xb484f9dc9641e9db, 860),
    (0xe1a63853bbd26451, 863),
    (0x8d07e33455637eb3, 867),
    (0xb049dc016abc5e60, 870),
    (0xdc5c5301c56b75f8, 873),
    (0x89b9b3e11b6329bb, 877),
    (0xac2820d9623bf429, 880),
    (0xd732290fbacaf134, 883),
    (0x867f59a9d4bed6c0, 887),
    (0xa81f301449ee8c70, 890),
    (0xd226fc195c6a2f8c, 893),
    (0x83585d8fd9c25db8, 897),
    (0xa42e74f3d032f526, 900),
    (0xcd3a1230c43fb26f, 903),
    (0x80444b5e7aa7cf85, 907),
    (0xa0555e361951c367, 910),
    (0xc86ab5c39fa63440, 913),
    (0xfa856334878fc151, 916),
    (0x9c935e00d4b9d8d2, 920),
    (0xc3b8358109e84f07, 923),
    (0xf4a642e14c6262c9, 926),
    (0x98e7e9cccfbd7dbe, 930),
    (0xbf21e44003acdd2d, 933),
    (0xeeea5d5004981478, 936),
    (0x95527a5202df0ccb, 940),
    (0xbaa718e68396cffe, 943),
    (0xe950df20247c83fd, 946),
    (0x91d28b7416cdd27e, 950),
    (0xb6472e511c81471e, 953),
    (0xe3d8f9e563a198e5, 956),
    (0x8e679c2f5e44ff8f, 960),
    (0xb201833b35d63f73, 963),
    (0xde81e40a034bcf50, 966),
];

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
