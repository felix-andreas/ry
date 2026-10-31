//! The non-numeric half of `R_ext/Utils.h` (file names, temporary names, string predicates)
//! and the helpers Fortran code calls to print and to signal (`intpr`, `dblepr`, `rexitc`,
//! ...). Printing reproduces `print()`'s layout for a plain vector: common width, `[i]` line
//! labels and wrapping at 80 columns, at R's default 7 significant digits.

use crate::{
    Global, Rboolean,
    boundary::{ApiResult, enter, error, write_console},
    strings::c_bytes,
};
use heap::object::{NA_INTEGER, is_na_real};
use std::ffi::{CStr, c_char, c_int, c_uint};

const PRINT_WIDTH: usize = 80;
const PRINT_DIGITS: usize = 7;
const PATH_BUFFER: usize = 4096;

static EXPANDED: Global<[u8; PATH_BUFFER]> = Global::new([0; PATH_BUFFER]);

/// Expands a leading `~` (the home directory) or `~user`. The result lives in a static buffer
/// that the next call overwrites, as in GNU R; a name that cannot be expanded comes back as is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ExpandFileName(name: *const c_char) -> *const c_char {
    if name.is_null() {
        return name;
    }
    let bytes = c_bytes(name);
    let Some(expanded) = expand_tilde(bytes) else {
        return name;
    };
    if expanded.len() >= PATH_BUFFER {
        return name;
    }
    let buffer = &mut *EXPANDED.as_ptr();
    buffer[..expanded.len()].copy_from_slice(&expanded);
    buffer[expanded.len()] = 0;
    buffer.as_ptr() as *const c_char
}

fn expand_tilde(name: &[u8]) -> Option<Vec<u8>> {
    let rest = name.strip_prefix(b"~")?;
    let user_end = rest.iter().position(|&byte| byte == b'/').unwrap_or(rest.len());
    let (user, tail) = rest.split_at(user_end);
    let home = if user.is_empty() {
        match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => home.into_encoded_bytes(),
            _ => user_home(None)?,
        }
    } else {
        user_home(Some(user))?
    };
    let mut expanded = home;
    expanded.extend_from_slice(tail);
    Some(expanded)
}

/// The home directory from the password database, of `user` or of the current user.
fn user_home(user: Option<&[u8]>) -> Option<Vec<u8>> {
    // SAFETY: the password database entry is copied out before anything else can overwrite it.
    unsafe {
        let entry = match user {
            Some(user) => {
                let user = std::ffi::CString::new(user).ok()?;
                libc::getpwnam(user.as_ptr())
            }
            None => libc::getpwuid(libc::getuid()),
        };
        if entry.is_null() || (*entry).pw_dir.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*entry).pw_dir).to_bytes().to_vec())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_tmpnam(prefix: *const c_char, directory: *const c_char) -> *mut c_char {
    R_tmpnam2(prefix, directory, std::ptr::null())
}

/// `directory/prefix<pid><random>extension` for a name no file has yet, in `malloc`ed memory
/// that `R_free_tmpnam` releases.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_tmpnam2(
    prefix: *const c_char,
    directory: *const c_char,
    extension: *const c_char,
) -> *mut c_char {
    enter(|| temporary_name(prefix, directory, extension))
}

unsafe fn temporary_name(
    prefix: *const c_char,
    directory: *const c_char,
    extension: *const c_char,
) -> ApiResult<*mut c_char> {
    let text = |pointer: *const c_char| {
        if pointer.is_null() {
            String::new()
        } else {
            String::from_utf8_lossy(c_bytes(pointer)).into_owned()
        }
    };
    let (prefix, directory, extension) = (text(prefix), text(directory), text(extension));
    let pid = std::process::id();
    for _ in 0..100 {
        let candidate = format!(
            "{directory}/{prefix}{pid:x}{:x}{extension}",
            libc::rand() as c_uint
        );
        if std::fs::symlink_metadata(&candidate).is_err() {
            let length = candidate.len();
            let copy = libc::malloc(length + 1) as *mut u8;
            if copy.is_null() {
                return Err(error("allocation failed in R_tmpnam2"));
            }
            std::ptr::copy_nonoverlapping(candidate.as_ptr(), copy, length);
            *copy.add(length) = 0;
            return Ok(copy as *mut c_char);
        }
    }
    Err(error("cannot find unused tempfile name"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_free_tmpnam(name: *mut c_char) {
    if !name.is_null() {
        libc::free(name.cast());
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_StringTrue(text: *const c_char) -> Rboolean {
    (!text.is_null() && matches!(c_bytes(text), b"T" | b"True" | b"TRUE" | b"true")) as Rboolean
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_StringFalse(text: *const c_char) -> Rboolean {
    (!text.is_null() && matches!(c_bytes(text), b"F" | b"False" | b"FALSE" | b"false")) as Rboolean
}

/// True when the string is empty or all white space, Unicode white space included.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_isBlankString(text: *const c_char) -> Rboolean {
    if text.is_null() {
        return 1;
    }
    let bytes = c_bytes(text);
    let blank = match std::str::from_utf8(bytes) {
        Ok(text) => text.chars().all(char::is_whitespace),
        Err(_) => bytes.iter().all(u8::is_ascii_whitespace),
    };
    blank as Rboolean
}

// Fortran's printing and signalling helpers. Fortran appends hidden string lengths to the
// argument list; these signatures ignore them, as GNU R's do, and take the label's length from
// `nchar` (negative meaning "up to the terminating NUL").

/// The label's bytes, or `None` after warning about a length GNU R also rejects.
unsafe fn label<'a>(text: *const c_char, length: *const c_int) -> Option<&'a [u8]> {
    let length = if length.is_null() { -1 } else { *length };
    let length = if length < 0 {
        if text.is_null() { 0 } else { c_bytes(text).len() }
    } else {
        length as usize
    };
    if length > 255 {
        crate::boundary::warn("invalid character length in Fortran printing");
        return None;
    }
    if length == 0 || text.is_null() {
        return Some(&[]);
    }
    Some(std::slice::from_raw_parts(text as *const u8, length))
}

unsafe fn print_label(text: *const c_char, length: *const c_int) {
    if let Some(label) = label(text, length).filter(|label| !label.is_empty()) {
        let mut line = label.to_vec();
        line.push(b'\n');
        write_console(&line, false);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn labelpr_(text: *const c_char, length: *const c_int) -> c_int {
    print_label(text, length);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn intpr_(
    text: *const c_char,
    length: *const c_int,
    data: *const c_int,
    count: *const c_int,
) -> c_int {
    print_label(text, length);
    if *count > 0 {
        let values = std::slice::from_raw_parts(data, *count as usize);
        write_console(format_integer_vector(values).as_bytes(), false);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dblepr_(
    text: *const c_char,
    length: *const c_int,
    data: *const f64,
    count: *const c_int,
) -> c_int {
    print_label(text, length);
    if *count > 0 {
        let values = std::slice::from_raw_parts(data, *count as usize);
        write_console(format_real_vector(values).as_bytes(), false);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realpr_(
    text: *const c_char,
    length: *const c_int,
    data: *const f32,
    count: *const c_int,
) -> c_int {
    print_label(text, length);
    if *count > 0 {
        let values: Vec<f64> = std::slice::from_raw_parts(data, *count as usize)
            .iter()
            .map(|&value| value as f64)
            .collect();
        write_console(format_real_vector(&values).as_bytes(), false);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn intpr1_(text: *const c_char, length: *const c_int, value: *const c_int) -> c_int {
    let one = 1;
    intpr_(text, length, value, &one)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dblepr1_(text: *const c_char, length: *const c_int, value: *const f64) -> c_int {
    let one = 1;
    dblepr_(text, length, value, &one)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realpr1_(text: *const c_char, length: *const c_int, value: *const f32) -> c_int {
    let one = 1;
    realpr_(text, length, value, &one)
}

/// Fortran's interrupt check.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rchkusr_() {
    crate::boundary::R_CheckUserInterrupt();
}

/// `print()`'s layout for an integer vector, ending in a newline.
pub fn format_integer_vector(values: &[c_int]) -> String {
    let cells: Vec<String> = values
        .iter()
        .map(|&value| {
            if value == NA_INTEGER {
                "NA".to_owned()
            } else {
                value.to_string()
            }
        })
        .collect();
    lay_out(&cells)
}

/// `print()`'s layout for a double vector at 7 significant digits, ending in a newline.
pub fn format_real_vector(values: &[f64]) -> String {
    lay_out(&format_reals(values))
}

/// Right-aligns the cells to a common width after a one-space gap and wraps them into lines
/// that start with a right-aligned `[i]` label, as R prints a vector.
fn lay_out(cells: &[String]) -> String {
    let count = cells.len();
    let label_width = ((count as f64 + 0.5).log10() + 1.0) as usize + 2;
    let cell_width = cells.iter().map(String::len).max().unwrap_or(0) + 1;
    let mut text = String::new();
    let mut width = label_width;
    text.push_str(&format!("{:>label_width$}", "[1]"));
    for (position, cell) in cells.iter().enumerate() {
        if position > 0 && width + cell_width > PRINT_WIDTH {
            text.push('\n');
            text.push_str(&format!("{:>label_width$}", format!("[{}]", position + 1)));
            width = label_width;
        }
        text.push_str(&format!("{cell:>cell_width$}"));
        width += cell_width;
    }
    text.push('\n');
    text
}

/// Formats doubles the way `print()` does: the fewest significant digits (at most 7) that
/// show every value, in a common fixed notation unless scientific is narrower.
pub fn format_reals(values: &[f64]) -> Vec<String> {
    let finite: Vec<(i32, usize, bool)> = values
        .iter()
        .filter(|value| value.is_finite())
        .map(|&value| {
            let (exponent, significant) = significant_digits(value);
            (exponent, significant, value < 0.0)
        })
        .collect();
    if finite.is_empty() {
        return values.iter().map(|&value| special(value)).collect();
    }
    let negative = finite.iter().any(|&(_, _, negative)| negative) as usize;
    let left = finite
        .iter()
        .map(|&(exponent, _, _)| (exponent + 1).max(1) as usize)
        .max()
        .unwrap_or(1);
    let right = finite
        .iter()
        .map(|&(exponent, significant, _)| (significant as i32 - exponent - 1).max(0) as usize)
        .max()
        .unwrap_or(0);
    let fixed_width = negative + left + if right > 0 { right + 1 } else { 0 };
    let mantissa_digits = finite
        .iter()
        .map(|&(_, significant, _)| significant)
        .max()
        .unwrap_or(1);
    let wide_exponent = finite.iter().any(|&(exponent, _, _)| exponent.abs() >= 100);
    let decimals = mantissa_digits - 1;
    let scientific_width =
        negative + (decimals > 0) as usize + decimals + 4 + wide_exponent as usize;
    values
        .iter()
        .map(|&value| {
            if !value.is_finite() {
                special(value)
            } else if fixed_width <= scientific_width {
                format!("{value:.right$}")
            } else {
                scientific(value, decimals)
            }
        })
        .collect()
}

fn special(value: f64) -> String {
    if is_na_real(value) {
        "NA".into()
    } else if value.is_nan() {
        "NaN".into()
    } else if value > 0.0 {
        "Inf".into()
    } else {
        "-Inf".into()
    }
}

/// The decimal exponent of `value` and the significant digits it needs, at most 7, after
/// rounding to 7.
fn significant_digits(value: f64) -> (i32, usize) {
    if value == 0.0 {
        return (0, 1);
    }
    let text = format!("{:.*e}", PRINT_DIGITS - 1, value.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent = exponent.parse().unwrap_or(0);
    let digits = mantissa.replace('.', "");
    let significant = digits.trim_end_matches('0').len().max(1);
    (exponent, significant)
}

/// C's `%.*e`: a signed exponent of at least two digits.
fn scientific(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exponent.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_vectors_print_like_r() {
        assert_eq!(format_real_vector(&[1.0, 2.5, 3.0]), "[1] 1.0 2.5 3.0\n");
        assert_eq!(format_real_vector(&[1e10, 1.0]), "[1] 1e+10 1e+00\n");
        assert_eq!(format_real_vector(&[std::f64::consts::PI]), "[1] 3.141593\n");
        assert_eq!(format_real_vector(&[0.1, f64::NAN, -2.0]), "[1]  0.1  NaN -2.0\n");
        assert_eq!(format_integer_vector(&[1, NA_INTEGER, 300]), "[1]   1  NA 300\n");
    }

    #[test]
    fn long_vectors_wrap_with_index_labels() {
        let values: Vec<c_int> = (1..=30).collect();
        let text = format_integer_vector(&values);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0].len(), 4 + 25 * 3);
        assert!(lines[1].starts_with("[26]"), "{text}");
    }
}
