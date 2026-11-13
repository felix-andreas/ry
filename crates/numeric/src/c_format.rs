//! Numbers written the way C's `printf` writes them (`%d`, `%.*f`, `%.*e`, `%g`), which GNU R's
//! messages and `formatC` show and which Rust's own formatting differs from in the exponent's
//! form, the trailing zeros and the rounding of ties.

use {heap::object::is_na_real, std::fmt::Write};

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
        "d" => push_decimal(out, value.round() as i64),
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
                push_decimal(out, value as i64);
            } else {
                push_general(out, value, precision.max(1), false);
            }
        }
    }
}

/// `value` in decimal, appended to `out`, without the formatting machinery `write!` runs for
/// every number.
pub fn push_decimal(out: &mut String, value: i64) {
    let mut digits = [0u8; 20];
    let mut magnitude = value.unsigned_abs();
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if value < 0 {
        out.push('-');
    }
    out.extend(digits[start..].iter().map(|&digit| char::from(digit)));
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
pub fn fixed_fast(out: &mut String, value: f64, precision: usize) -> bool {
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
