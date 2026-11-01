//! GNU R's number formatting and vector printing for native code (`R_ext/PrtUtil.h`): the
//! `format*` functions compute the common field of a vector as `print()` lays it out, the
//! `Encode*` functions render one element in such a field, and the `print*Vector` functions
//! print a vector with its `[1]` index labels. The field computation is the interpreter's own
//! (`interp::format`), so native code and `print()` agree.
//!
//! A `format*` function reports a double's field as width `w`, decimals `d` and `e`, which is 0
//! for fixed notation and otherwise one less than the number of exponent digits, as GNU R does.

use {
    crate::{R_xlen_t, boundary::write_console, vectors},
    heap::object::{NA_INTEGER, Rcomplex, Sexp, SexpType, is_na_real},
    interp::format::{
        RealFormat, Settings, format_complex_common, format_integer, format_logical,
        format_real_common, format_real_with,
    },
    std::ffi::{CStr, c_char, c_int},
};

/// The field width `print()` gives the logicals.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_formatLogical(x: *const c_int, n: R_xlen_t, width: *mut c_int) {
    let values = elements(x, n);
    *width = values
        .iter()
        .map(|&value| format_logical(value).len())
        .max()
        .unwrap_or(1) as c_int;
}

/// The field width `print()` gives the integers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_formatInteger(x: *const c_int, n: R_xlen_t, width: *mut c_int) {
    let values = elements(x, n);
    *width = values
        .iter()
        .map(|&value| format_integer(value).len())
        .max()
        .unwrap_or(1) as c_int;
}

/// The common field of the doubles: width, decimals and exponent digits, with at least
/// `nsmall` decimals in fixed notation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_formatReal(
    x: *const f64,
    n: R_xlen_t,
    width: *mut c_int,
    decimals: *mut c_int,
    exponent: *mut c_int,
    nsmall: c_int,
) {
    let format = format_real_common(elements(x, n), settings(nsmall));
    (*width, *decimals, *exponent) = report(format);
}

/// The common fields of the complex numbers' real and imaginary parts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_formatComplex(
    x: *const Rcomplex,
    n: R_xlen_t,
    real_width: *mut c_int,
    real_decimals: *mut c_int,
    real_exponent: *mut c_int,
    imaginary_width: *mut c_int,
    imaginary_decimals: *mut c_int,
    imaginary_exponent: *mut c_int,
    nsmall: c_int,
) {
    let values = elements(x, n);
    let (real, imaginary) = format_complex_common(values, settings(nsmall));
    (*real_width, *real_decimals, *real_exponent) = report(real);
    (*imaginary_width, *imaginary_decimals, *imaginary_exponent) = report(imaginary);
    // A vector of only NAs still needs room for "NA" across both fields and the sign and "i".
    if values
        .iter()
        .any(|value| is_na_real(value.r) || is_na_real(value.i))
    {
        let total = *real_width + *imaginary_width + 2;
        if total < 2 {
            *real_width += 2 - total;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn formatLogicalS(x: Sexp, n: R_xlen_t, width: *mut c_int) {
    Rf_formatLogical(vectors::LOGICAL(x), n, width);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn formatIntegerS(x: Sexp, n: R_xlen_t, width: *mut c_int) {
    Rf_formatInteger(vectors::INTEGER(x), n, width);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn formatRealS(
    x: Sexp,
    n: R_xlen_t,
    width: *mut c_int,
    decimals: *mut c_int,
    exponent: *mut c_int,
    nsmall: c_int,
) {
    Rf_formatReal(vectors::REAL(x), n, width, decimals, exponent, nsmall);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn formatComplexS(
    x: Sexp,
    n: R_xlen_t,
    real_width: *mut c_int,
    real_decimals: *mut c_int,
    real_exponent: *mut c_int,
    imaginary_width: *mut c_int,
    imaginary_decimals: *mut c_int,
    imaginary_exponent: *mut c_int,
    nsmall: c_int,
) {
    Rf_formatComplex(
        vectors::COMPLEX(x),
        n,
        real_width,
        real_decimals,
        real_exponent,
        imaginary_width,
        imaginary_decimals,
        imaginary_exponent,
        nsmall,
    );
}

/// One logical right-aligned in a field of `width`. The text lives until the next call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_EncodeLogical(x: c_int, width: c_int) -> *const c_char {
    hand_out(&LOGICAL_TEXT, pad(format_logical(x), width))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_EncodeInteger(x: c_int, width: c_int) -> *const c_char {
    hand_out(&INTEGER_TEXT, pad(&format_integer(x), width))
}

/// One double in the field `formatReal` computed, with `dec` as the decimal mark.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_EncodeReal0(
    x: f64,
    width: c_int,
    decimals: c_int,
    exponent: c_int,
    dec: *const c_char,
) -> *const c_char {
    let mark = if dec.is_null() {
        "."
    } else {
        CStr::from_ptr(dec).to_str().unwrap_or(".")
    };
    hand_out(&REAL_TEXT, encode_real(x, width, decimals, exponent, mark))
}

/// As `Rf_EncodeReal0`, with a one-character decimal mark.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_EncodeReal(
    x: f64,
    width: c_int,
    decimals: c_int,
    exponent: c_int,
    dec: c_char,
) -> *const c_char {
    let mark = char::from(dec as u8).to_string();
    hand_out(&REAL_TEXT, encode_real(x, width, decimals, exponent, &mark))
}

/// One complex number in the two fields `formatComplex` computed, as `re+imi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_EncodeComplex(
    x: Rcomplex,
    real_width: c_int,
    real_decimals: c_int,
    real_exponent: c_int,
    imaginary_width: c_int,
    imaginary_decimals: c_int,
    imaginary_exponent: c_int,
    dec: *const c_char,
) -> *const c_char {
    let mark = if dec.is_null() {
        "."
    } else {
        CStr::from_ptr(dec).to_str().unwrap_or(".")
    };
    let text = if is_na_real(x.r) || is_na_real(x.i) {
        pad("NA", real_width + imaginary_width + 2)
    } else {
        let real = encode_real(x.r, real_width, real_decimals, real_exponent, mark);
        let imaginary = encode_real(
            x.i.abs(),
            imaginary_width,
            imaginary_decimals,
            imaginary_exponent,
            mark,
        );
        let shows_zero = imaginary
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'0' | b'.' | b','));
        let sign = if x.i < 0.0 && !shows_zero { '-' } else { '+' };
        format!("{real}{sign}{imaginary}i")
    };
    hand_out(&COMPLEX_TEXT, text)
}

/// The width of the label `[n]` without its brackets.
#[unsafe(no_mangle)]
pub extern "C" fn Rf_IndexWidth(n: R_xlen_t) -> c_int {
    ((n as f64 + 0.5).log10() + 1.0) as c_int
}

/// Prints the label `[i]` right-aligned in a field of `width`.
#[unsafe(no_mangle)]
pub extern "C" fn Rf_VectorIndex(i: R_xlen_t, width: c_int) {
    let label = format!("[{i}]");
    write_console(pad(&label, width).as_bytes(), false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_printIntegerVector(x: *const c_int, n: R_xlen_t, indx: c_int) {
    let mut width = 0;
    Rf_formatInteger(x, n, &mut width);
    let texts = elements(x, n)
        .iter()
        .map(|&value| pad(&format_integer(value), width));
    print_fields(texts, n, width, indx != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_printRealVector(x: *const f64, n: R_xlen_t, indx: c_int) {
    let (mut width, mut decimals, mut exponent) = (0, 0, 0);
    Rf_formatReal(x, n, &mut width, &mut decimals, &mut exponent, 0);
    let texts = elements(x, n)
        .iter()
        .map(|&value| encode_real(value, width, decimals, exponent, "."));
    print_fields(texts, n, width, indx != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_printComplexVector(x: *const Rcomplex, n: R_xlen_t, indx: c_int) {
    let mut fields = [0; 6];
    let [
        real_width,
        real_decimals,
        real_exponent,
        imaginary_width,
        imaginary_decimals,
        imaginary_exponent,
    ] = &mut fields;
    Rf_formatComplex(
        x,
        n,
        real_width,
        real_decimals,
        real_exponent,
        imaginary_width,
        imaginary_decimals,
        imaginary_exponent,
        0,
    );
    let width = fields[0] + fields[3] + 2;
    let texts = elements(x, n).iter().map(|&value| {
        let text = Rf_EncodeComplex(
            value,
            fields[0],
            fields[1],
            fields[2],
            fields[3],
            fields[4],
            fields[5],
            c".".as_ptr(),
        );
        pad(&CStr::from_ptr(text).to_string_lossy(), width)
    });
    print_fields(texts, n, width, indx != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn printIntegerVectorS(x: Sexp, n: R_xlen_t, indx: c_int) {
    Rf_printIntegerVector(vectors::INTEGER(x), n, indx);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn printRealVectorS(x: Sexp, n: R_xlen_t, indx: c_int) {
    Rf_printRealVector(vectors::REAL(x), n, indx);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn printComplexVectorS(x: Sexp, n: R_xlen_t, indx: c_int) {
    Rf_printComplexVector(vectors::COMPLEX(x), n, indx);
}

static LOGICAL_TEXT: crate::Global<Vec<u8>> = crate::Global::new(Vec::new());
static INTEGER_TEXT: crate::Global<Vec<u8>> = crate::Global::new(Vec::new());
static REAL_TEXT: crate::Global<Vec<u8>> = crate::Global::new(Vec::new());
static COMPLEX_TEXT: crate::Global<Vec<u8>> = crate::Global::new(Vec::new());

/// Stores `text` NUL-terminated in `buffer` and returns it, as GNU R's encoders return a
/// static buffer the next call overwrites.
unsafe fn hand_out(buffer: &crate::Global<Vec<u8>>, text: String) -> *const c_char {
    let buffer = &mut *buffer.as_ptr();
    buffer.clear();
    buffer.extend_from_slice(text.as_bytes());
    buffer.push(0);
    buffer.as_ptr().cast()
}

unsafe fn elements<'a, T>(x: *const T, n: R_xlen_t) -> &'a [T] {
    if x.is_null() || n <= 0 {
        return &[];
    }
    std::slice::from_raw_parts(x, n as usize)
}

/// The print settings of the session (`digits` and `scipen`), or GNU R's defaults before the
/// interpreter runs.
unsafe fn settings(nsmall: c_int) -> Settings {
    let base = if interp::is_installed() {
        Settings::current()
    } else {
        Settings {
            digits: 7,
            scipen: 0,
            na_width: 2,
            nsmall: 0,
        }
    };
    Settings {
        nsmall: nsmall.max(0) as usize,
        ..base
    }
}

fn report(format: RealFormat) -> (c_int, c_int, c_int) {
    (
        format.width as c_int,
        format.decimals as c_int,
        format.exponent.saturating_sub(1) as c_int,
    )
}

fn encode_real(x: f64, width: c_int, decimals: c_int, exponent: c_int, mark: &str) -> String {
    let format = RealFormat {
        width: width.max(0) as usize,
        decimals: decimals.max(0) as usize,
        exponent: if exponent > 0 {
            exponent as usize + 1
        } else {
            0
        },
    };
    let mut text = format_real_with(x, format, "NA");
    if mark != "." && !is_na_real(x) && x.is_finite() {
        text = text.replacen('.', mark, 1);
    }
    pad(&text, width)
}

fn pad(text: &str, width: c_int) -> String {
    format!("{text:>width$}", width = width.max(0) as usize)
}

/// Prints right-aligned fields with a gap of one space, breaking lines at `options(width)`
/// and labelling each line with the index of its first element when `labels` is set.
unsafe fn print_fields(
    texts: impl Iterator<Item = String>,
    n: R_xlen_t,
    width: c_int,
    labels: bool,
) {
    let line_width = line_width();
    let label_width = if labels { Rf_IndexWidth(n) + 2 } else { 0 };
    let field = width + 1;
    let mut output = String::new();
    if labels {
        output.push_str(&pad("[1]", label_width));
    }
    let mut used = label_width;
    for (index, text) in texts.enumerate() {
        if index > 0 && used + field > line_width {
            output.push('\n');
            if labels {
                output.push_str(&pad(&format!("[{}]", index + 1), label_width));
            }
            used = label_width;
        }
        output.push(' ');
        output.push_str(&text);
        used += field;
    }
    output.push('\n');
    write_console(output.as_bytes(), false);
}

unsafe fn line_width() -> c_int {
    if !interp::is_installed() {
        return 80;
    }
    let width = interp::envir::get_option("width");
    if width.kind() == SexpType::Nil {
        return 80;
    }
    match interp::values::as_integer_scalar(width) {
        NA_INTEGER => 80,
        width => width,
    }
}
