//! Arithmetic helpers, sorting and the random number generator.
//!
//! The generator is R's default, the Mersenne Twister with R's seeding and its fix-up into the
//! open interval (0, 1), and its state lives where R keeps it, in `.Random.seed` in the global
//! environment, so `GetRNGstate` and `PutRNGstate` exchange it with R code exactly as in GNU R.

use crate::{
    boundary::{ApiResult, enter, error},
    environments::frame_value,
    globals::R_SeedsSymbol,
    runtime,
};
use heap::object::{NA_INTEGER, SexpType, is_na_real};
use std::ffi::c_int;

// The generator itself is the interpreter's (`interp::random`), so R code and native code share
// one state through `.Random.seed`.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GetRNGstate() {
    enter(|| interp::random::get_rng_state())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PutRNGstate() {
    enter(|| interp::random::put_rng_state())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn unif_rand() -> f64 {
    enter(|| interp::random::unif_rand())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn norm_rand() -> f64 {
    enter(|| interp::random::norm_rand())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn exp_rand() -> f64 {
    enter(|| interp::random::exp_rand())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_unif_index(n: f64) -> f64 {
    enter(|| interp::random::unif_index(n))
}

#[unsafe(no_mangle)]
pub extern "C" fn R_IsNA(value: f64) -> c_int {
    is_na_real(value) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn R_IsNaN(value: f64) -> c_int {
    (value.is_nan() && !is_na_real(value)) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn R_finite(value: f64) -> c_int {
    value.is_finite() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn R_isnancpp(value: f64) -> c_int {
    value.is_nan() as c_int
}

/// R's sort order for doubles: ascending, with `NA` and `NaN` last.
fn real_order(left: f64, right: f64) -> std::cmp::Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// R's sort order for integers: ascending, with `NA` last.
fn int_order(left: i32, right: i32) -> std::cmp::Ordering {
    match (left == NA_INTEGER, right == NA_INTEGER) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left.cmp(&right),
    }
}

unsafe fn slice<'a, T>(values: *mut T, count: c_int) -> &'a mut [T] {
    if values.is_null() || count <= 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(values, count as usize)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_isort(values: *mut c_int, count: c_int) {
    slice(values, count).sort_by(|left, right| int_order(*left, *right));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_rsort(values: *mut f64, count: c_int) {
    slice(values, count).sort_by(|left, right| real_order(*left, *right));
}

/// Sorts `values` ascending and permutes `index` the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rsort_with_index(values: *mut f64, index: *mut c_int, count: c_int) {
    let values = slice(values, count);
    let index = slice(index, count);
    let mut pairs: Vec<(f64, c_int)> = values.iter().copied().zip(index.iter().copied()).collect();
    pairs.sort_by(|left, right| real_order(left.0, right.0));
    for (position, (value, original)) in pairs.into_iter().enumerate() {
        values[position] = value;
        index[position] = original;
    }
}

/// Sorts `values` descending and permutes `index` the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn revsort(values: *mut f64, index: *mut c_int, count: c_int) {
    let values = slice(values, count);
    let index = slice(index, count);
    let mut pairs: Vec<(f64, c_int)> = values.iter().copied().zip(index.iter().copied()).collect();
    pairs.sort_by(|left, right| real_order(right.0, left.0));
    for (position, (value, original)) in pairs.into_iter().enumerate() {
        values[position] = value;
        index[position] = original;
    }
}

/// Partial sort: element `k` ends up where a full sort would put it, smaller ones before it and
/// larger ones after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_iPsort(values: *mut c_int, count: c_int, k: c_int) {
    let values = slice(values, count);
    if (0..values.len() as c_int).contains(&k) {
        values.select_nth_unstable_by(k as usize, |left, right| int_order(*left, *right));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_rPsort(values: *mut f64, count: c_int, k: c_int) {
    let values = slice(values, count);
    if (0..values.len() as c_int).contains(&k) {
        values.select_nth_unstable_by(k as usize, |left, right| real_order(*left, *right));
    }
}

/// The C stack is the host's, and Rust's own guard pages catch an overflow, so there is nothing
/// to check yet.
#[unsafe(no_mangle)]
pub extern "C" fn R_CheckStack() {}

#[unsafe(no_mangle)]
pub extern "C" fn R_CheckStack2(_extra: usize) {}

/// Seeds the generator as `set.seed(seed)` does and stores `.Random.seed`. Not GNU R API; the
/// test harness compares sequences against GNU R with it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_set_seed(seed: c_int) {
    enter(|| interp::random::set_seed(seed))
}
