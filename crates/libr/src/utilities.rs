//! Rmath's small numeric helpers and the string and file utilities of the public C API.

use std::ffi::{CStr, c_char, c_double, c_int};

unsafe extern "C" {
    fn tgamma(x: c_double) -> c_double;
    fn lgamma(x: c_double) -> c_double;
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_fmax2(x: c_double, y: c_double) -> c_double {
    if x.is_nan() || y.is_nan() { x + y } else { x.max(y) }
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_fmin2(x: c_double, y: c_double) -> c_double {
    if x.is_nan() || y.is_nan() { x + y } else { x.min(y) }
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_imax2(x: c_int, y: c_int) -> c_int {
    x.max(y)
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_imin2(x: c_int, y: c_int) -> c_int {
    x.min(y)
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_fsign(x: c_double, y: c_double) -> c_double {
    if x.is_nan() || y.is_nan() { x + y } else if y >= 0.0 { x.abs() } else { -x.abs() }
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_sign(x: c_double) -> c_double {
    if x.is_nan() { x } else if x > 0.0 { 1.0 } else if x == 0.0 { 0.0 } else { -1.0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn R_pow(x: c_double, y: c_double) -> c_double {
    if x == 1.0 || y == 0.0 {
        return 1.0;
    }
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else if y < 0.0 { f64::INFINITY } else { y };
    }
    x.powf(y)
}

#[unsafe(no_mangle)]
pub extern "C" fn R_pow_di(x: c_double, n: c_int) -> c_double {
    if x.is_nan() {
        return x;
    }
    if n == c_int::MIN {
        return x.powf(n as f64);
    }
    let mut result = 1.0;
    let mut base = if n < 0 { 1.0 / x } else { x };
    let mut exponent = n.unsigned_abs();
    while exponent > 0 {
        if exponent & 1 == 1 {
            result *= base;
        }
        exponent >>= 1;
        base *= base;
    }
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_gammafn(x: c_double) -> c_double {
    // SAFETY: a pure libm function.
    unsafe { tgamma(x) }
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_lgammafn(x: c_double) -> c_double {
    // SAFETY: a pure libm function.
    unsafe { lgamma(x) }
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_choose(n: c_double, k: c_double) -> c_double {
    let k = k.round();
    if n.is_nan() || k.is_nan() {
        return n + k;
    }
    if k < 0.0 {
        return 0.0;
    }
    if k == 0.0 {
        return 1.0;
    }
    if k < 30.0 {
        let mut result = n;
        let mut index = 2.0;
        while index <= k {
            result *= (n - index + 1.0) / index;
            index += 1.0;
        }
        return if n == n.round() { result.round() } else { result };
    }
    if n == n.round() && n >= 0.0 {
        if n < k {
            return 0.0;
        }
        return (Rf_lgammafn(n + 1.0) - Rf_lgammafn(k + 1.0) - Rf_lgammafn(n - k + 1.0)).exp().round();
    }
    Rf_gammafn(n + 1.0) / (Rf_gammafn(k + 1.0) * Rf_gammafn(n - k + 1.0))
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_fround(x: c_double, digits: c_double) -> c_double {
    let scale = 10f64.powi(digits as i32);
    (x * scale).round() / scale
}

#[unsafe(no_mangle)]
pub extern "C" fn R_ProcessEvents() {}

/// # Safety
/// `text` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_isBlankString(text: *const c_char) -> c_int {
    // SAFETY: the caller passes a C string.
    let text = unsafe { CStr::from_ptr(text) };
    text.to_bytes().iter().all(u8::is_ascii_whitespace) as c_int
}

/// # Safety
/// `text` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_strIsASCII(text: *const c_char) -> c_int {
    // SAFETY: the caller passes a C string.
    let text = unsafe { CStr::from_ptr(text) };
    text.to_bytes().is_ascii() as c_int
}

/// GNU R returns a pointer into a static buffer, which the next call overwrites.
///
/// # Safety
/// `path` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ExpandFileName(path: *const c_char) -> *const c_char {
    static mut BUFFER: [c_char; 4096] = [0; 4096];
    // SAFETY: the caller passes a C string; the buffer is only touched on the R thread.
    unsafe {
        let path = CStr::from_ptr(path).to_string_lossy();
        let expanded = interp::connections::expand_path(&path);
        let bytes = expanded.as_bytes();
        let length = bytes.len().min(4095);
        let buffer = &raw mut BUFFER;
        for (index, byte) in bytes[..length].iter().enumerate() {
            (*buffer)[index] = *byte as c_char;
        }
        (*buffer)[length] = 0;
        (*buffer).as_ptr()
    }
}

