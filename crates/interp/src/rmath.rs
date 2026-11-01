//! R's mathematical library, the API `Rmath.h` declares: densities, distribution functions,
//! quantiles and random variates for R's distributions, and the special functions they rest on.
//!
//! Results must agree with GNU R's to the last few bits, because R code compares them exactly
//! and packages' own tests record them. That pins the algorithms, so every function here follows
//! the published method GNU R's nmath library uses (Cody's rational approximations for the normal
//! distribution, Wichura's AS 241 for its quantile, Didonato and Morris's TOMS 708 for the
//! incomplete beta ratio, Loader's saddle point for binomial and Poisson densities, the Ahrens,
//! Dieter, Kachitvichyanukul, Schmeiser and Cheng generators for random variates) and the same
//! order of uniform draws, written independently from the papers. Where a published series
//! needs constants, they are the mathematically defined values (Chebyshev coefficients, zeta
//! values, Gauss-Legendre nodes), so they match any faithful implementation of the method.
//!
//! The conventions are R's: `lower` selects P[X <= x] over P[X > x], `log` gives and takes
//! probabilities on the log scale, invalid parameters give `NaN` without a warning (R reports
//! those from its vectorized wrappers, not from C), and an `NA` argument comes back as that
//! same `NA` because the arithmetic that propagates it keeps the payload.

// The coefficients are the published ones, written with the digits their sources give (some
// round to a `std::f64::consts` value, some carry more digits than a double holds) so that
// they read against the papers.
#![allow(clippy::excessive_precision, clippy::approx_constant)]
// Loops written `if !(a > b) { break }` must end when a comparison meets NaN, as the C
// `do ... while (a > b)` they follow does; `a <= b` would loop forever.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::linalg::format_g;
use std::{cell::RefCell, ffi::c_int, rc::Rc};

pub const DBL_EPSILON: f64 = f64::EPSILON;
pub const DBL_MIN: f64 = f64::MIN_POSITIVE;
pub const DBL_MAX: f64 = f64::MAX;
/// `DBL_MIN_EXP` in C's convention, where the smallest normal number is `2^(DBL_MIN_EXP - 1)`.
pub const DBL_MIN_EXP: i32 = -1021;
pub const DBL_MANT_DIG: i32 = 53;

pub const M_PI: f64 = std::f64::consts::PI;
pub const M_2PI: f64 = 2.0 * std::f64::consts::PI;
pub const M_LN2: f64 = std::f64::consts::LN_2;
pub const M_1_PI: f64 = std::f64::consts::FRAC_1_PI;
pub const M_PI_2: f64 = std::f64::consts::FRAC_PI_2;
pub const M_SQRT2: f64 = std::f64::consts::SQRT_2;
pub const M_SQRT_PI: f64 = 1.772_453_850_905_516;
pub const M_SQRT_2PI: f64 = 2.506_628_274_631_000_7;
pub const M_SQRT_32: f64 = 5.656_854_249_492_381;
pub const M_1_SQRT_2PI: f64 = 0.398_942_280_401_432_7;
pub const M_SQRT_2DPI: f64 = 0.797_884_560_802_865_4;
pub const M_LN_2PI: f64 = 1.837_877_066_409_345_5;
pub const M_LN_SQRT_PI: f64 = 0.572_364_942_924_700_1;
pub const M_LN_SQRT_2PI: f64 = 0.918_938_533_204_672_8;
pub const M_LN_SQRT_PID2: f64 = 0.225_791_352_644_727_43;

// Shared conventions: the `R_D_*` and `R_DT_*` vocabulary of densities and probabilities that
// may be on the log scale and in either tail.

fn d0(log: bool) -> f64 {
    if log { f64::NEG_INFINITY } else { 0.0 }
}

fn d1(log: bool) -> f64 {
    if log { 0.0 } else { 1.0 }
}

fn dt0(lower: bool, log: bool) -> f64 {
    if lower { d0(log) } else { d1(log) }
}

fn dt1(lower: bool, log: bool) -> f64 {
    if lower { d1(log) } else { d0(log) }
}

/// `R_D_val`: a probability on the requested scale.
fn d_val(x: f64, log: bool) -> f64 {
    if log { x.ln() } else { x }
}

/// `R_D_exp`: a log probability on the requested scale.
fn d_exp(x: f64, log: bool) -> f64 {
    if log { x } else { x.exp() }
}

/// `R_D_Clog`: one minus a probability, on the requested scale.
fn d_clog(p: f64, log: bool) -> f64 {
    if log { (-p).ln_1p() } else { 0.5 - p + 0.5 }
}

/// `R_DT_val`: a lower-tail probability, converted to the requested tail and scale.
fn dt_val(x: f64, lower: bool, log: bool) -> f64 {
    if lower { d_val(x, log) } else { d_clog(x, log) }
}

/// `R_Log1_Exp`: `log(1 - exp(x))` for `x <= 0`, accurate on both sides of `-log(2)`.
fn log1_exp(x: f64) -> f64 {
    if x > -M_LN2 {
        (-x.exp_m1()).ln()
    } else {
        (-x.exp()).ln_1p()
    }
}

/// `R_D_LExp`: `log(1 - p)` for `p` on the requested scale.
fn d_lexp(p: f64, log: bool) -> f64 {
    if log { log1_exp(p) } else { (-p).ln_1p() }
}

/// `R_D_log`: the natural log of a probability on the requested scale.
fn d_log(p: f64, log: bool) -> f64 {
    if log { p } else { p.ln() }
}

/// `R_D_Lval`: the lower-tail probability of a non-log probability.
fn d_lval(p: f64, lower: bool) -> f64 {
    if lower { p } else { 0.5 - p + 0.5 }
}

/// `R_D_Cval`: the upper-tail probability of a non-log probability.
fn d_cval(p: f64, lower: bool) -> f64 {
    if lower { 0.5 - p + 0.5 } else { p }
}

/// `R_DT_qIv`: the lower-tail, non-log probability of a quantile function's argument.
fn dt_qiv(p: f64, lower: bool, log: bool) -> f64 {
    if log {
        if lower { p.exp() } else { -p.exp_m1() }
    } else {
        d_lval(p, lower)
    }
}

/// `R_DT_CIv`: the upper-tail, non-log probability of a quantile function's argument.
fn dt_civ(p: f64, lower: bool, log: bool) -> f64 {
    if log {
        if lower { -p.exp_m1() } else { p.exp() }
    } else {
        d_cval(p, lower)
    }
}

/// `R_D_qIv`: a probability off the log scale.
fn d_qiv(p: f64, log: bool) -> f64 {
    if log { p.exp() } else { p }
}

/// `R_DT_log`: the log of the lower-tail probability.
fn dt_log(p: f64, lower: bool, log: bool) -> f64 {
    if lower { d_log(p, log) } else { d_lexp(p, log) }
}

/// `R_DT_Clog`: the log of the upper-tail probability.
fn dt_clog(p: f64, lower: bool, log: bool) -> f64 {
    if lower { d_lexp(p, log) } else { d_log(p, log) }
}

/// `R_DT_Log`: the log of the lower-tail probability, given a log probability.
fn dt_log_of_log(p: f64, lower: bool) -> f64 {
    if lower { p } else { log1_exp(p) }
}

/// `R_Q_P01_check`: whether `p` is not a probability on its scale.
fn q_p01_invalid(p: f64, log: bool) -> bool {
    (log && p > 0.0) || (!log && !(0.0..=1.0).contains(&p))
}

/// `R_Q_P01_boundaries`: the quantile at the ends of the probability scale, or `None` when `p`
/// lies strictly inside it.
fn q_p01_boundaries(p: f64, lower: bool, log: bool, left: f64, right: f64) -> Option<f64> {
    if log {
        if p > 0.0 {
            return Some(f64::NAN);
        }
        if p == 0.0 {
            return Some(if lower { right } else { left });
        }
        if p == f64::NEG_INFINITY {
            return Some(if lower { left } else { right });
        }
    } else {
        if !(0.0..=1.0).contains(&p) {
            return Some(f64::NAN);
        }
        if p == 0.0 {
            return Some(if lower { left } else { right });
        }
        if p == 1.0 {
            return Some(if lower { right } else { left });
        }
    }
    None
}

/// `R_forceint`: round half to even, as C's `nearbyint` does in the default rounding mode.
pub fn forceint(x: f64) -> f64 {
    x.round_ties_even()
}

/// `R_nonint`: whether `x` is not within rounding of an integer.
fn nonint(x: f64) -> bool {
    (x - forceint(x)).abs() > 1e-9 * fmax2(1.0, x.abs())
}

/// Whether `choose` and `lchoose` treat `n` as an integer, which allows more slack than
/// `nonint`, so `choose(4 - 1e-7, 4)` is 1.
fn near_integer(x: f64) -> bool {
    (x - forceint(x)).abs() <= 1e-7 * fmax2(1.0, x.abs())
}

/// `R_D_negInonint`.
fn negative_or_nonint(x: f64) -> bool {
    x < 0.0 || nonint(x)
}

/// `R_D_nonint_check`: GNU R warns about a non-integer argument of a discrete density and
/// returns a zero density.
fn nonint_warning(x: f64) -> bool {
    if nonint(x) {
        warning(format!("non-integer x = {x:.6}"));
        true
    } else {
        false
    }
}

/// `MATHLIB_WARNING`. The functions stay free of interpreter state, so a warning is collected
/// here and raised by the caller afterwards: the builtins as R warnings, the C API through its
/// own warning path.
fn warning(message: String) {
    WARNINGS.with(|warnings| warnings.borrow_mut().push(message));
}

/// The warnings the functions raised since the last call, in order.
pub fn take_warnings() -> Vec<String> {
    WARNINGS.with(|warnings| std::mem::take(&mut *warnings.borrow_mut()))
}

thread_local! {
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// `ML_WARNING` for the conditions GNU R reports; a domain error stays silent at this level.
fn precision_warning(function: &str) {
    warning(format!(
        "full precision may not have been achieved in '{function}'"
    ));
}

fn range_warning(function: &str) {
    warning(format!("value out of range in '{function}'"));
}

fn convergence_warning(function: &str) {
    warning(format!("convergence failed in '{function}'"));
}

fn underflow_warning(function: &str) {
    warning(format!("underflow occurred in '{function}'"));
}

pub fn finite(x: f64) -> bool {
    x.is_finite()
}

// Small utilities.

pub fn fmax2(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x < y { y } else { x }
}

pub fn fmin2(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x < y { x } else { y }
}

pub fn sign(x: f64) -> f64 {
    if x.is_nan() {
        x
    } else if x > 0.0 {
        1.0
    } else if x == 0.0 {
        0.0
    } else {
        -1.0
    }
}

/// `|x|` with the sign of `y`, where a zero `y` counts as positive.
pub fn fsign(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if y >= 0.0 { x.abs() } else { -x.abs() }
}

pub fn ftrunc(x: f64) -> f64 {
    x.trunc()
}

pub fn ln_1p(x: f64) -> f64 {
    x.ln_1p()
}

/// `sqrt(a^2 + b^2)` without overflow.
pub fn pythag(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    if a.is_infinite() || b.is_infinite() {
        return f64::INFINITY;
    }
    a.hypot(b)
}

/// `signif(x, digits)`: rounds to `digits` significant decimal digits.
pub fn fprec(x: f64, digits: f64) -> f64 {
    const MAX_DIGITS: i32 = 22;
    const MAX10E: i32 = 308;
    if x.is_nan() || digits.is_nan() {
        return x + digits;
    }
    if !finite(x) {
        return x;
    }
    let digits = if !finite(digits) {
        if digits > 0.0 {
            return x;
        }
        1.0
    } else {
        digits
    };
    if x == 0.0 {
        return x;
    }
    let mut dig = digits.round() as i32;
    if dig > MAX_DIGITS {
        return x;
    } else if dig < 1 {
        dig = 1;
    }
    let (sgn, x) = if x < 0.0 { (-1.0, -x) } else { (1.0, x) };
    let l10 = x.log10();
    let mut e10 = dig - 1 - l10.floor() as i32;
    if l10.abs() < (MAX10E - 2) as f64 {
        let mut p10 = 1.0;
        if e10 > MAX10E {
            p10 = r_pow_di(10.0, e10 - MAX10E);
            e10 = MAX10E;
        }
        if e10 > 0 {
            let pow10 = r_pow_di(10.0, e10);
            sgn * ((x * pow10 * p10).round_ties_even() / pow10) / p10
        } else {
            let pow10 = r_pow_di(10.0, -e10);
            sgn * ((x / pow10).round_ties_even() * pow10)
        }
    } else if l10 < 0.0 {
        // Below 1e-306 the scale factor 10^e10 overflows; the decimal formatter rounds the
        // exact binary value to `dig` digits instead, which is the answer GNU R gives.
        let text = format!("{:.*e}", (dig - 1) as usize, x);
        sgn * text.parse::<f64>().unwrap_or(x)
    } else {
        let do_round = MAX10E as f64 - l10 >= r_pow_di(10.0, -dig);
        let e2 = dig + if e10 > 0 { 1 } else { 6 };
        let p10 = r_pow_di(10.0, e2);
        let big_p10 = r_pow_di(10.0, e10 - e2);
        let mut x = x * p10 * big_p10;
        if do_round {
            x += 0.5;
        }
        x = x.floor() / p10;
        sgn * x / big_p10
    }
}

/// `round(x, digits)`: of the two decimal neighbours of `x` with `digits` decimals, the one
/// closer to `x` as a double, the even one on a tie, so `round(0.15, 1)` is 0.1 because the
/// double nearest 0.15 lies below it.
pub fn fround(x: f64, digits: f64) -> f64 {
    const MAX_DIGITS: f64 = 308.0;
    if x.is_nan() || digits.is_nan() {
        return x + digits;
    }
    if !finite(x) || digits > MAX_DIGITS + 15.0 || x == 0.0 {
        return x;
    }
    if digits < -MAX_DIGITS {
        return 0.0;
    }
    if digits == 0.0 {
        return forceint(x);
    }
    let dig = (digits + 0.5).floor() as i32;
    let (sgn, x) = if x < 0.0 { (-1.0, -x) } else { (1.0, x) };
    let l10x = std::f64::consts::LOG10_2 * (0.5 + logb(x));
    if l10x + dig as f64 > f64::DIGITS as f64 {
        return sgn * x;
    }
    let (x10, i10, down, up);
    if dig > 0 {
        let pow10 = r_pow_di(10.0, dig);
        x10 = pow10 * x;
        i10 = x10.floor();
        down = i10 / pow10;
        up = x10.ceil() / pow10;
    } else {
        let pow10 = r_pow_di(10.0, -dig);
        x10 = x / pow10;
        i10 = x10.floor();
        down = i10 * pow10;
        up = x10.ceil() * pow10;
    }
    let du = up - x;
    let dd = x - down;
    sgn * if du < dd || (du == dd && i10 % 2.0 == 1.0) {
        up
    } else {
        down
    }
}

/// C's `logb`: the unbiased binary exponent.
fn logb(x: f64) -> f64 {
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if !finite(x) {
        return x.abs();
    }
    let bits = x.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    if exponent == 0 {
        let mantissa = bits & ((1 << 52) - 1);
        (63 - mantissa.leading_zeros() as i64 - 1074) as f64
    } else {
        (exponent - 1023) as f64
    }
}

/// `x^y` with R's conventions for the cases C's `pow` leaves to the platform.
pub fn r_pow(x: f64, y: f64) -> f64 {
    if y == 2.0 {
        return x * x;
    }
    if x == 1.0 || y == 0.0 {
        return 1.0;
    }
    if x == 0.0 {
        return if y > 0.0 {
            0.0
        } else if y < 0.0 {
            f64::INFINITY
        } else {
            y
        };
    }
    if finite(x) && finite(y) {
        return x.powf(y);
    }
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if !finite(x) {
        if x > 0.0 {
            return if y < 0.0 { 0.0 } else { f64::INFINITY };
        } else if finite(y) && y == y.floor() {
            return if y < 0.0 {
                0.0
            } else if y % 2.0 != 0.0 {
                x
            } else {
                -x
            };
        }
    }
    if !finite(y) && x >= 0.0 {
        return if y > 0.0 {
            if x >= 1.0 { f64::INFINITY } else { 0.0 }
        } else if x < 1.0 {
            f64::INFINITY
        } else {
            0.0
        };
    }
    f64::NAN
}

/// `x^n` by repeated squaring, the order of multiplications GNU R uses.
pub fn r_pow_di(x: f64, n: c_int) -> f64 {
    if x.is_nan() {
        return x;
    }
    if n == heap::object::NA_INTEGER {
        return heap::object::NA_REAL;
    }
    let mut result = 1.0;
    if n != 0 {
        if !finite(x) {
            return r_pow(x, n as f64);
        }
        let negative = n < 0;
        let mut n = n.unsigned_abs();
        let mut x = x;
        loop {
            if n & 1 != 0 {
                result *= x;
            }
            n >>= 1;
            if n != 0 {
                x *= x;
            } else {
                break;
            }
        }
        if negative {
            result = 1.0 / result;
        }
    }
    result
}

/// `(1 + x)^y`, accurate for small `x`.
pub fn pow1p(x: f64, y: f64) -> f64 {
    if y.is_nan() {
        return if x == 0.0 { 1.0 } else { y };
    }
    if (0.0..=4.0).contains(&y) && y == y.trunc() {
        match y as i32 {
            0 => return 1.0,
            1 => return x + 1.0,
            2 => return x * (x + 2.0) + 1.0,
            3 => return x * (x * (x + 3.0) + 3.0) + 1.0,
            _ => return x * (x * (x * (x + 4.0) + 6.0) + 4.0) + 1.0,
        }
    }
    let xp1 = x + 1.0;
    let back = xp1 - 1.0;
    if back == x || x.abs() > 0.5 || x.is_nan() {
        xp1.powf(y)
    } else {
        (y * x.ln_1p()).exp()
    }
}

/// `cos(pi * x)`, exact at multiples of one half.
pub fn cospi_(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if !finite(x) {
        return f64::NAN;
    }
    let x = x.abs() % 2.0;
    if x % 1.0 == 0.5 {
        return 0.0;
    }
    if x == 1.0 {
        return -1.0;
    }
    if x == 0.0 {
        return 1.0;
    }
    (M_PI * x).cos()
}

/// `sin(pi * x)`, exact at multiples of one half.
pub fn sinpi_(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if !finite(x) {
        return f64::NAN;
    }
    let mut x = x % 2.0;
    if x <= -1.0 {
        x += 2.0;
    } else if x > 1.0 {
        x -= 2.0;
    }
    if x == 0.0 || x == 1.0 {
        return 0.0;
    }
    if x == 0.5 {
        return 1.0;
    }
    if x == -0.5 {
        return -1.0;
    }
    (M_PI * x).sin()
}

/// `tan(pi * x)`, exact at multiples of one quarter.
pub fn tanpi_(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if !finite(x) {
        return f64::NAN;
    }
    let mut x = x % 1.0;
    if x <= -0.5 {
        x += 1.0;
    } else if x > 0.5 {
        x -= 1.0;
    }
    if x == 0.0 {
        0.0
    } else if x == 0.5 {
        f64::NAN
    } else if x == 0.25 {
        1.0
    } else if x == -0.25 {
        -1.0
    } else {
        (M_PI * x).tan()
    }
}

/// `log(1 + exp(x))` without overflow.
pub fn log1pexp(x: f64) -> f64 {
    if x <= 18.0 {
        return x.exp().ln_1p();
    }
    if x > 33.3 {
        return x;
    }
    x + (-x).exp()
}

/// `log(1 - exp(-x))` for `x >= 0`.
pub fn log1mexp(x: f64) -> f64 {
    if x < 0.0 {
        return f64::NAN;
    }
    if x <= M_LN2 {
        (-(-x).exp_m1()).ln()
    } else {
        (-(-x).exp()).ln_1p()
    }
}

/// `log(exp(logx) + exp(logy))`.
pub fn logspace_add(logx: f64, logy: f64) -> f64 {
    fmax2(logx, logy) + (-(logx - logy).abs()).exp().ln_1p()
}

/// `log(exp(logx) - exp(logy))`.
pub fn logspace_sub(logx: f64, logy: f64) -> f64 {
    logx + log1_exp(logy - logx)
}

/// `log(sum(exp(logx)))`, scaled by the largest term.
pub fn logspace_sum(logx: &[f64]) -> f64 {
    match logx {
        [] => f64::NEG_INFINITY,
        [single] => *single,
        [first, second] => logspace_add(*first, *second),
        _ => {
            let maximum =
                logx.iter().copied().fold(
                    logx[0],
                    |maximum, value| {
                        if maximum < value { value } else { maximum }
                    },
                );
            let sum: f64 = logx.iter().map(|value| (value - maximum).exp()).sum();
            maximum + sum.ln()
        }
    }
}

/// The continued fraction for `sum_k x^k / (i + k d)`, the tail of `log1pmx` and `lgamma1p`.
fn logcf(x: f64, i: f64, d: f64, eps: f64) -> f64 {
    const SCALE: f64 = 1.157_920_892_373_162e77;
    let mut c1 = 2.0 * d;
    let mut c2 = i + d;
    let mut c4 = c2 + d;
    let mut a1 = c2;
    let mut b1 = i * (c2 - i * x);
    let mut b2 = d * d * x;
    let mut a2 = c4 * c2 - b2;
    b2 = c4 * b1 - i * b2;
    while (a2 * b1 - a1 * b2).abs() > (eps * b1 * b2).abs() {
        let mut c3 = c2 * c2 * x;
        c2 += d;
        c4 += d;
        a1 = c4 * a2 - c3 * a1;
        b1 = c4 * b2 - c3 * b1;
        c3 = c1 * c1 * x;
        c1 += d;
        c4 += d;
        a2 = c4 * a1 - c3 * a2;
        b2 = c4 * b1 - c3 * b2;
        if b2.abs() > SCALE {
            a1 /= SCALE;
            b1 /= SCALE;
            a2 /= SCALE;
            b2 /= SCALE;
        } else if b2.abs() < 1.0 / SCALE {
            a1 *= SCALE;
            b1 *= SCALE;
            a2 *= SCALE;
            b2 *= SCALE;
        }
    }
    a2 / b2
}

/// `log(1 + x) - x`, accurate near zero.
pub fn log1pmx(x: f64) -> f64 {
    const MIN_LOG1_VALUE: f64 = -0.79149064;
    if !(MIN_LOG1_VALUE..=1.0).contains(&x) {
        return x.ln_1p() - x;
    }
    let r = x / (2.0 + x);
    let y = r * r;
    if x.abs() < 1e-2 {
        r * ((((2.0 / 9.0 * y + 2.0 / 7.0) * y + 2.0 / 5.0) * y + 2.0 / 3.0) * y - x)
    } else {
        r * (2.0 * y * logcf(y, 3.0, 2.0, 1e-14) - x)
    }
}

/// `(zeta(i + 2) - 1) / (i + 2)` for `i < 40`: the series of `log(gamma(1 + a))` about zero
/// (Abramowitz and Stegun 6.1.33).
const LGAMMA1P_COEFFICIENTS: [f64; 40] = [
    0.3224670334241132,
    0.0673523010531981,
    0.020580808427784546,
    0.007385551028673986,
    0.0028905103307415234,
    0.001192753911703261,
    0.0005096695247430425,
    0.00022315475845357939,
    9.945751278180853e-05,
    4.492623673813314e-05,
    2.050721277567069e-05,
    9.439488275268397e-06,
    4.374866789907488e-06,
    2.039215753801366e-06,
    9.55141213040742e-07,
    4.492469198764566e-07,
    2.1207184805554665e-07,
    1.0043224823968099e-07,
    4.7698101693639804e-08,
    2.2711094608943164e-08,
    1.0838659214896955e-08,
    5.183475041970047e-09,
    2.4836745438024785e-09,
    1.1921401405860912e-09,
    5.731367241678862e-10,
    2.7595228851242334e-10,
    1.330476437424449e-10,
    6.4229645638381e-11,
    3.1044247747322276e-11,
    1.5021384080754142e-11,
    7.275974480239079e-12,
    3.527742476575915e-12,
    1.711991790559618e-12,
    8.315385841420285e-13,
    4.04220052528944e-13,
    1.9664756310966165e-13,
    9.573630387838556e-14,
    4.6640760264283744e-14,
    2.2737369600659724e-14,
    1.1091399470834522e-14,
];

/// `log(gamma(1 + a))`, accurate for small `a`.
pub fn lgamma1p(a: f64) -> f64 {
    const EULER: f64 = 0.577_215_664_901_532_9;
    // zeta(42) - 1, the remainder the continued fraction scales.
    const TAIL: f64 = 2.2737368458246524e-13;
    if a.abs() >= 0.5 {
        return lgammafn(a + 1.0);
    }
    let count = LGAMMA1P_COEFFICIENTS.len();
    let mut lgam = TAIL * logcf(-a / 2.0, (count + 2) as f64, 1.0, 1e-14);
    for coefficient in LGAMMA1P_COEFFICIENTS.iter().rev() {
        lgam = coefficient - a * lgam;
    }
    (a * lgam - EULER) * a - log1pmx(a)
}

// The gamma function and its relatives.

/// Evaluates `c[0]/2 + sum c[k] T_k(x)` by Clenshaw's recurrence.
fn chebyshev_eval(x: f64, coefficients: &[f64]) -> f64 {
    if !(-1.1..=1.1).contains(&x) {
        return f64::NAN;
    }
    let twox = x * 2.0;
    let (mut b0, mut b1, mut b2) = (0.0, 0.0, 0.0);
    for coefficient in coefficients.iter().rev() {
        b2 = b1;
        b1 = b0;
        b0 = twox * b1 - b2 + coefficient;
    }
    (b0 - b2) * 0.5
}

/// The Chebyshev series of `gamma(1 + y) - 0.9375` on `0 <= y <= 1`, in `t = 2y - 1`.
const GAMMA_SERIES: [f64; 22] = [
    0.00857119559098933,
    0.004415381324841007,
    0.05685043681599363,
    -0.00421983539641856,
    0.0013268081812124603,
    -0.00018930245297988805,
    3.606925327441245e-05,
    -6.056761904460864e-06,
    1.0558295463022833e-06,
    -1.811967365542384e-07,
    3.117724964715322e-08,
    -5.354219639019687e-09,
    9.193275519859589e-10,
    -1.5779412802883398e-10,
    2.7079806229349544e-11,
    -4.64681865382573e-12,
    7.97335019200742e-13,
    -1.368078209830916e-13,
    2.3473194865638007e-14,
    -4.027432614949067e-15,
    6.910051747372101e-16,
    -1.185584500221993e-16,
];

/// The Chebyshev series of `x * lgammacor(x)` for `x >= 10`, in `t = 2 (10/x)^2 - 1`.
const LGAMMACOR_SERIES: [f64; 5] = [
    0.16663894804518634,
    -1.384948176067564e-05,
    9.81082564692473e-09,
    -1.809129475572494e-11,
    6.221098041892606e-14,
];

/// The remainder of Stirling's approximation of `log(gamma(x))` for `x >= 10`.
fn lgammacor(x: f64) -> f64 {
    const XBIG: f64 = 94906265.62425156;
    const XMAX: f64 = 3.745194030963158e306;
    if x < 10.0 {
        return f64::NAN;
    }
    if x >= XMAX {
        underflow_warning("lgammacor");
    } else if x < XBIG {
        let t = 10.0 / x;
        return chebyshev_eval(t * t * 2.0 - 1.0, &LGAMMACOR_SERIES) / x;
    }
    1.0 / (x * 12.0)
}

pub fn gammafn(x: f64) -> f64 {
    const XMIN: f64 = -170.5674972726612;
    const XMAX: f64 = 171.61447887182298;
    const XSML: f64 = 2.2474362225598545e-308;
    const DXREL: f64 = 1.490116119384765696e-8;
    if x.is_nan() {
        return x;
    }
    if x == 0.0 || (x < 0.0 && x == x.round()) {
        return f64::NAN;
    }
    let mut y = x.abs();
    if y <= 10.0 {
        let mut n = x as i32;
        if x < 0.0 {
            n -= 1;
        }
        y = x - n as f64;
        n -= 1;
        let mut value = chebyshev_eval(y * 2.0 - 1.0, &GAMMA_SERIES) + 0.9375;
        if n == 0 {
            return value;
        }
        if n < 0 {
            if x < -0.5 && (x - (x - 0.5).trunc() / x).abs() < DXREL {
                precision_warning("gammafn");
            }
            if y < XSML {
                range_warning("gammafn");
                return if x > 0.0 {
                    f64::INFINITY
                } else {
                    f64::NEG_INFINITY
                };
            }
            for i in 0..-n {
                value /= x + i as f64;
            }
            value
        } else {
            for i in 1..=n {
                value *= y + i as f64;
            }
            value
        }
    } else {
        if x > XMAX {
            return f64::INFINITY;
        }
        if x < XMIN {
            return 0.0;
        }
        let value = if y <= 50.0 && y == y.trunc() {
            let mut value = 1.0;
            for i in 2..y as i32 {
                value *= i as f64;
            }
            value
        } else {
            ((y - 0.5) * y.ln() - y + M_LN_SQRT_2PI + stirlerr(y)).exp()
        };
        if x > 0.0 {
            return value;
        }
        if ((x - (x - 0.5).trunc()) / x).abs() < DXREL {
            precision_warning("gammafn");
        }
        let sinpiy = sinpi_(y);
        if sinpiy == 0.0 {
            range_warning("gammafn");
            return f64::INFINITY;
        }
        -M_PI / (y * sinpiy * value)
    }
}

pub fn lgammafn(x: f64) -> f64 {
    lgammafn_sign(x).0
}

/// `log|gamma(x)|` and the sign of `gamma(x)`.
pub fn lgammafn_sign(x: f64) -> (f64, c_int) {
    const XMAX: f64 = 2.5327372760800758e305;
    const DXREL: f64 = 1.490116119384765625e-8;
    let mut sign = 1;
    if x.is_nan() {
        return (x, sign);
    }
    if x < 0.0 && (-x).floor() % 2.0 == 0.0 {
        sign = -1;
    }
    if x <= 0.0 && x == x.trunc() {
        return (f64::INFINITY, sign);
    }
    let y = x.abs();
    if y < 1e-306 {
        return (-y.ln(), sign);
    }
    if y <= 10.0 {
        return (gammafn(x).abs().ln(), sign);
    }
    if y > XMAX {
        return (f64::INFINITY, sign);
    }
    if x > 0.0 {
        let value = if x > 1e17 {
            x * (x.ln() - 1.0)
        } else if x > 4934720.0 {
            M_LN_SQRT_2PI + (x - 0.5) * x.ln() - x
        } else {
            M_LN_SQRT_2PI + (x - 0.5) * x.ln() - x + lgammacor(x)
        };
        return (value, sign);
    }
    let sinpiy = sinpi_(y).abs();
    if sinpiy == 0.0 {
        return (f64::NAN, sign);
    }
    let answer = M_LN_SQRT_PID2 + (x - 0.5) * y.ln() - x - sinpiy.ln() - lgammacor(y);
    if ((x - (x - 0.5).trunc()) * answer / x).abs() < DXREL {
        precision_warning("lgamma");
    }
    (answer, sign)
}

/// `stirlerr(n) = log(n!) - log(sqrt(2 pi n) (n/e)^n)` at the half integers up to 15.
const STIRLERR_HALVES: [f64; 31] = [
    0.0,
    0.15342640972002736,
    0.08106146679532726,
    0.05481412105191765,
    0.0413406959554093,
    0.03316287351993629,
    0.02767792568499834,
    0.023746163656297496,
    0.020790672103765093,
    0.018488450532673187,
    0.016644691189821193,
    0.015134973221917378,
    0.013876128823070748,
    0.012810465242920227,
    0.01189670994589177,
    0.011104559758206917,
    0.010411265261972096,
    0.009799416126158804,
    0.009255462182712733,
    0.008768700134139386,
    0.00833056343336287,
    0.00793411456431402,
    0.007573675487951841,
    0.007244554301320383,
    0.00694284010720953,
    0.006665247032707682,
    0.006408994188004207,
    0.006171712263039458,
    0.0059513701127588475,
    0.0057462165130101155,
    0.005554733551962801,
];

/// The error of Stirling's formula for `log(n!)` (Loader 2000).
fn stirlerr(n: f64) -> f64 {
    const S0: f64 = 1.0 / 12.0;
    const S1: f64 = 1.0 / 360.0;
    const S2: f64 = 1.0 / 1260.0;
    const S3: f64 = 1.0 / 1680.0;
    const S4: f64 = 1.0 / 1188.0;
    if n <= 15.0 {
        let nn = n + n;
        if nn == nn.trunc() {
            return STIRLERR_HALVES[nn as usize];
        }
        return lgammafn(n + 1.0) - (n + 0.5) * n.ln() + n - M_LN_SQRT_2PI;
    }
    let nn = n * n;
    if n > 500.0 {
        return (S0 - S1 / nn) / n;
    }
    if n > 80.0 {
        return (S0 - (S1 - S2 / nn) / nn) / n;
    }
    if n > 35.0 {
        return (S0 - (S1 - (S2 - S3 / nn) / nn) / nn) / n;
    }
    (S0 - (S1 - (S2 - (S3 - S4 / nn) / nn) / nn) / nn) / n
}

/// The deviance term `x log(x/np) + np - x`, by a series when `x` is near `np` (Loader 2000).
fn bd0(x: f64, np: f64) -> f64 {
    if !finite(x) || !finite(np) || np == 0.0 {
        return f64::NAN;
    }
    if (x - np).abs() < 0.1 * (x + np) {
        let mut v = (x - np) / (x + np);
        let mut s = (x - np) * v;
        if s.abs() < DBL_MIN {
            return s;
        }
        let mut ej = 2.0 * x * v;
        v *= v;
        for j in 1..1000 {
            ej *= v;
            let s1 = s + ej / ((j << 1) + 1) as f64;
            if s1 == s {
                return s1;
            }
            s = s1;
        }
    }
    x * (x / np).ln() + np - x
}

/// `bd0` as an unevaluated sum `high + low`. A plain double holds the deviance only to an
/// absolute precision of an ulp of itself or, away from `x == np`, of `x`, which `exp(-bd0)`
/// turns into a relative error of the density; the Poisson density needs it to the last digit.
fn bd0_extended(x: f64, np: f64) -> (f64, f64) {
    let ratio = x / np;
    let plain = bd0(x, np);
    // Past about 1e300 the product below overflows, and a deviance anywhere near that size
    // leaves no density for the extra digits to refine.
    if !(ratio.is_finite() && ratio > 0.0) || !(plain < 1e300) {
        return (plain, 0.0);
    }
    let ratio_low = (-ratio).mul_add(np, x) / np;
    let log_ratio = ln_double_double((ratio, ratio_low));
    let product = double_double_mul_f64(log_ratio, x);
    let difference = two_sum(np, -x);
    double_double_add(product, difference)
}

/// The natural logarithm of a double-double, by `2 atanh((m - 1)/(m + 1))` on the mantissa
/// reduced to [1/sqrt(2), sqrt(2)], where the series loses nothing to cancellation.
fn ln_double_double(value: (f64, f64)) -> (f64, f64) {
    const LN_2: (f64, f64) = (std::f64::consts::LN_2, 2.319_046_813_846_299_6e-17);
    let mut exponent = value.0.log2().floor();
    let mut scale = 2f64.powf(-exponent);
    if value.0 * scale > std::f64::consts::SQRT_2 {
        exponent += 1.0;
        scale *= 0.5;
    }
    let mantissa = (value.0 * scale, value.1 * scale);
    let numerator = two_sum(mantissa.0 - 1.0, mantissa.1);
    let denominator = double_double_add(two_sum(mantissa.0, 1.0), (mantissa.1, 0.0));
    let s = double_double_div(numerator, denominator);
    let s_squared = double_double_mul(s, s);
    let mut power = s;
    let mut sum = s;
    for k in 1..200 {
        power = double_double_mul(power, s_squared);
        let term = double_double_div(power, ((2 * k + 1) as f64, 0.0));
        sum = double_double_add(sum, term);
        if term.0.abs() <= 1e-34 * sum.0.abs() {
            break;
        }
    }
    let twice = (2.0 * sum.0, 2.0 * sum.1);
    double_double_add(twice, double_double_mul_f64(LN_2, exponent))
}

fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let sum = a + b;
    let b_virtual = sum - a;
    let error = (a - (sum - b_virtual)) + (b - b_virtual);
    (sum, error)
}

fn double_double_add(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (sum, error) = two_sum(a.0, b.0);
    let error = error + a.1 + b.1;
    two_sum(sum, error)
}

fn double_double_mul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let product = a.0 * b.0;
    let error = a.0.mul_add(b.0, -product) + (a.0 * b.1 + a.1 * b.0);
    two_sum(product, error)
}

fn double_double_mul_f64(a: (f64, f64), b: f64) -> (f64, f64) {
    double_double_mul(a, (b, 0.0))
}

fn double_double_div(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let quotient = a.0 / b.0;
    let remainder = double_double_add(a, double_double_mul((-quotient, 0.0), b));
    let correction = remainder.0 / b.0;
    two_sum(quotient, correction)
}

pub fn lbeta(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    let p = a.min(b);
    let q = a.max(b);
    if p < 0.0 {
        return f64::NAN;
    } else if p == 0.0 {
        return f64::INFINITY;
    } else if !finite(q) {
        return f64::NEG_INFINITY;
    }
    if p >= 10.0 {
        let corr = lgammacor(p) + lgammacor(q) - lgammacor(p + q);
        q.ln() * -0.5
            + M_LN_SQRT_2PI
            + corr
            + (p - 0.5) * (p / (p + q)).ln()
            + q * (-p / (p + q)).ln_1p()
    } else if q >= 10.0 {
        let corr = lgammacor(q) - lgammacor(p + q);
        lgammafn(p) + corr + p - p * (p + q).ln() + (q - 0.5) * (-p / (p + q)).ln_1p()
    } else if p < 1e-306 {
        lgammafn(p) + (lgammafn(q) - lgammafn(p + q))
    } else {
        (gammafn(p) * (gammafn(q) / gammafn(p + q))).ln()
    }
}

pub fn beta(a: f64, b: f64) -> f64 {
    const XMAX: f64 = 171.61447887182298;
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    if a < 0.0 || b < 0.0 {
        return f64::NAN;
    } else if a == 0.0 || b == 0.0 {
        return f64::INFINITY;
    } else if !finite(a) || !finite(b) {
        return 0.0;
    }
    if a + b < XMAX {
        // Multiplying left to right keeps a tiny argument's huge gamma from overflowing.
        (1.0 / gammafn(a + b)) * gammafn(a) * gammafn(b)
    } else {
        lbeta(a, b).exp()
    }
}

fn lfastchoose(n: f64, k: f64) -> f64 {
    -(n + 1.0).ln() - lbeta(n - k + 1.0, k + 1.0)
}

/// `lfastchoose` for `n < k - 1`, where the gamma function changes sign.
fn lfastchoose2(n: f64, k: f64) -> (f64, c_int) {
    let (r, sign) = lgammafn_sign(n - k + 1.0);
    (lgammafn(n + 1.0) - lgammafn(k + 1.0) - r, sign)
}

fn round_k_warning(k0: f64, k: f64) {
    if (k - k0).abs() > 1e-7 {
        warning(format!("'k' ({k0:.2}) must be integer, rounded to {k:.0}"));
    }
}

pub fn lchoose(n: f64, k: f64) -> f64 {
    let k0 = k;
    let k = forceint(k);
    if n.is_nan() || k.is_nan() {
        return n + k;
    }
    round_k_warning(k0, k);
    if k < 2.0 {
        if k < 0.0 {
            return f64::NEG_INFINITY;
        }
        if k == 0.0 {
            return 0.0;
        }
        return n.abs().ln();
    }
    if n < 0.0 {
        return lchoose(-n + k - 1.0, k);
    } else if near_integer(n) {
        let n = forceint(n);
        if n < k {
            return f64::NEG_INFINITY;
        }
        if n - k < 2.0 {
            return lchoose(n, n - k);
        }
        return lfastchoose(n, k);
    }
    if n < k - 1.0 {
        return lfastchoose2(n, k).0;
    }
    lfastchoose(n, k)
}

pub fn choose(n: f64, k: f64) -> f64 {
    const K_SMALL_MAX: f64 = 30.0;
    let k0 = k;
    let mut k = forceint(k);
    if n.is_nan() || k.is_nan() {
        return n + k;
    }
    round_k_warning(k0, k);
    if k < K_SMALL_MAX {
        if n - k < k && n >= 0.0 && near_integer(n) {
            k = forceint(n - k);
        }
        if k < 0.0 {
            return 0.0;
        }
        if k == 0.0 {
            return 1.0;
        }
        let mut r = n;
        let mut j = 2.0;
        while j <= k {
            r *= (n - j + 1.0) / j;
            j += 1.0;
        }
        return if near_integer(n) { forceint(r) } else { r };
    }
    if n < 0.0 {
        let r = choose(-n + k - 1.0, k);
        return if k != 2.0 * (k / 2.0).floor() { -r } else { r };
    } else if near_integer(n) {
        let n = forceint(n);
        if n < k {
            return 0.0;
        }
        if n - k < K_SMALL_MAX {
            return choose(n, n - k);
        }
        return forceint(lfastchoose(n, k).exp());
    }
    if n < k - 1.0 {
        let (r, sign) = lfastchoose2(n, k);
        return sign as f64 * r.exp();
    }
    lfastchoose(n, k).exp()
}

// The polygamma functions.

/// The Bernoulli numbers `B_2, B_4, ..., B_30`.
const BERNOULLI: [f64; 15] = [
    1.0 / 6.0,
    -1.0 / 30.0,
    1.0 / 42.0,
    -1.0 / 30.0,
    5.0 / 66.0,
    -691.0 / 2730.0,
    7.0 / 6.0,
    -3617.0 / 510.0,
    43867.0 / 798.0,
    -174611.0 / 330.0,
    854513.0 / 138.0,
    -236364091.0 / 2730.0,
    8553103.0 / 6.0,
    -23749461029.0 / 870.0,
    8615841276005.0 / 14322.0,
];

/// `psi(x)` for `x > 0`: the recurrence `psi(x) = psi(x + 1) - 1/x` up to where the asymptotic
/// series in the Bernoulli numbers is exact to double precision.
fn digamma_positive(x: f64) -> f64 {
    let mut shift = 0.0;
    let mut z = x;
    while z < 10.0 {
        shift += 1.0 / z;
        z += 1.0;
    }
    let inverse_square = 1.0 / (z * z);
    let mut power = inverse_square;
    let mut series = 0.0;
    for (index, bernoulli) in BERNOULLI.iter().take(8).enumerate() {
        let order = 2.0 * (index + 1) as f64;
        series += bernoulli / (order * 1.0) * power;
        power *= inverse_square;
    }
    z.ln() - 0.5 / z - series - shift
}

/// The Hurwitz zeta function `sum_{i >= 0} (x + i)^(-s)` for integer `s >= 2` and `x > 0`, by
/// Euler-Maclaurin summation after shifting `x` far enough that the correction terms shrink.
fn hurwitz_zeta(s: f64, x: f64) -> f64 {
    let threshold = 16.0 + 0.5 * s;
    let mut sum = 0.0;
    let mut z = x;
    while z < threshold {
        sum += z.powf(-s);
        z += 1.0;
    }
    let mut tail = z.powf(1.0 - s) / (s - 1.0) + 0.5 * z.powf(-s);
    // term_j = B_2j / (2j)! * s (s+1) ... (s+2j-2) * z^(-s-2j+1)
    let mut factor = s / z.powf(s + 1.0);
    let mut factorial = 2.0;
    for (index, bernoulli) in BERNOULLI.iter().enumerate() {
        let j = (index + 1) as f64;
        let term = bernoulli / factorial * factor;
        tail += term;
        if term.abs() <= DBL_EPSILON * 0.25 * tail.abs() {
            break;
        }
        factor *= (s + 2.0 * j - 1.0) * (s + 2.0 * j) / (z * z);
        factorial *= (2.0 * j + 1.0) * (2.0 * j + 2.0);
    }
    sum + tail
}

/// Fills `ans[j]` with `(-1)^(k+1) / k! * psi^(k)(x)` for `k = n + j` and returns Amos's error
/// code: 1 for invalid arguments, 4 where the reflection for negative `x` does not apply.
pub fn dpsifn(x: f64, n: c_int, kode: c_int, ans: &mut [f64]) -> c_int {
    if n < 0 || !(1..=2).contains(&kode) || ans.is_empty() {
        return 1;
    }
    if x.is_nan() {
        ans.fill(x);
        return 0;
    }
    if x == f64::INFINITY {
        for (j, slot) in ans.iter_mut().enumerate() {
            *slot = if j as c_int + n == 0 {
                f64::NEG_INFINITY
            } else {
                0.0
            };
        }
        return 0;
    }
    if x <= 0.0 && x == x.round() {
        for (j, slot) in ans.iter_mut().enumerate() {
            *slot = if (j as c_int + n) % 2 == 1 {
                f64::INFINITY
            } else {
                f64::NAN
            };
        }
        return 0;
    }
    if x < 0.0 {
        // psi^(k)(x) = (-1)^k psi^(k)(1 - x) - pi^(k+1) cot^(k)(pi x), the k-th derivative of
        // the reflection formula psi(1 - x) - psi(x) = pi cot(pi x).
        // GNU R's reflection stops at the fifth derivative.
        if n as usize + ans.len() - 1 > 5 {
            return 4;
        }
        let status = dpsifn(1.0 - x, n, 1, ans);
        if status != 0 {
            return status;
        }
        let cot = cot_derivatives(x * M_PI, (n as usize) + ans.len() - 1);
        let mut factorial = 1.0;
        let mut pi_power = M_PI;
        for k in 1..=n {
            factorial *= k as f64;
            pi_power *= M_PI;
        }
        for (j, slot) in ans.iter_mut().enumerate() {
            let k = n as usize + j;
            if j > 0 {
                factorial *= k as f64;
                pi_power *= M_PI;
            }
            // ans holds (-1)^(k+1)/k! psi^(k)(1 - x); convert, reflect and convert back.
            let sign = if k.is_multiple_of(2) { -1.0 } else { 1.0 };
            let psi_reflected = sign * factorial * *slot;
            let parity = if k.is_multiple_of(2) { 1.0 } else { -1.0 };
            let psi = parity * psi_reflected - pi_power * cot[k];
            *slot = sign * psi / factorial;
        }
        return 0;
    }
    for (j, slot) in ans.iter_mut().enumerate() {
        let k = n + j as c_int;
        *slot = if k == 0 {
            let value = -digamma_positive(x);
            if kode == 2 { value + x.ln() } else { value }
        } else {
            hurwitz_zeta((k + 1) as f64, x)
        };
        if !finite(*slot) {
            // Amos's overflow error, which GNU R reports as NaN.
            return 2;
        }
    }
    0
}

/// The derivatives `d^k/dt^k cot(t)` for `k <= order` at `t`, from the polynomials in
/// `c = cot(t)` that the rule `d/dt c = -(1 + c^2)` generates.
fn cot_derivatives(t: f64, order: usize) -> Vec<f64> {
    let c = t.cos() / t.sin();
    let mut polynomial = vec![0.0, 1.0];
    let mut values = Vec::with_capacity(order + 1);
    for k in 0..=order {
        values.push(
            polynomial
                .iter()
                .rev()
                .fold(0.0, |sum, coefficient| sum * c + coefficient),
        );
        if k == order {
            break;
        }
        // P'(c) * -(1 + c^2)
        let mut next = vec![0.0; polynomial.len() + 1];
        for (power, coefficient) in polynomial.iter().enumerate().skip(1) {
            let derivative = coefficient * power as f64;
            next[power - 1] -= derivative;
            next[power + 1] -= derivative;
        }
        polynomial = next;
    }
    values
}

/// The `n`-th derivative of the digamma function.
pub fn psigamma(x: f64, deriv: f64) -> f64 {
    const N_MAX: f64 = 100.0;
    if x.is_nan() {
        return x;
    }
    let deriv = forceint(deriv);
    let n = deriv as c_int;
    if deriv > N_MAX {
        warning(format!("deriv = {n} > {N_MAX} (= n_max)"));
        return f64::NAN;
    }
    let mut answer = [0.0];
    if dpsifn(x, n, 1, &mut answer) != 0 {
        return f64::NAN;
    }
    let mut answer = -answer[0];
    for k in 1..=n {
        answer *= -(k as f64);
    }
    answer
}

fn polygamma(x: f64, n: c_int, scale: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let mut answer = [0.0];
    if dpsifn(x, n, 1, &mut answer) != 0 {
        return f64::NAN;
    }
    scale * answer[0]
}

pub fn digamma(x: f64) -> f64 {
    polygamma(x, 0, -1.0)
}

pub fn trigamma(x: f64) -> f64 {
    polygamma(x, 1, 1.0)
}

pub fn tetragamma(x: f64) -> f64 {
    polygamma(x, 2, -2.0)
}

pub fn pentagamma(x: f64) -> f64 {
    polygamma(x, 3, 6.0)
}

// The normal distribution.

pub fn dnorm(x: f64, mu: f64, sigma: f64, log: bool) -> f64 {
    if x.is_nan() || mu.is_nan() || sigma.is_nan() {
        return x + mu + sigma;
    }
    if sigma < 0.0 {
        return f64::NAN;
    }
    if !finite(sigma) {
        return d0(log);
    }
    if !finite(x) && mu == x {
        return f64::NAN;
    }
    if sigma == 0.0 {
        return if x == mu { f64::INFINITY } else { d0(log) };
    }
    let x = (x - mu) / sigma;
    if !finite(x) {
        return d0(log);
    }
    let x = x.abs();
    if x >= 2.0 * DBL_MAX.sqrt() {
        return d0(log);
    }
    if log {
        return -(M_LN_SQRT_2PI + 0.5 * x * x + sigma.ln());
    }
    if x < 5.0 {
        return M_1_SQRT_2PI * (-0.5 * x * x).exp() / sigma;
    }
    // Beyond 5 the square loses bits that exp() amplifies, so split x into a part with few
    // significant bits, whose square is exact, and the rest.
    if x > (-2.0 * M_LN2 * (DBL_MIN_EXP + 1 - DBL_MANT_DIG) as f64).sqrt() {
        return 0.0;
    }
    let x1 = forceint(x * 65536.0) / 65536.0;
    let x2 = x - x1;
    M_1_SQRT_2PI / sigma * ((-0.5 * x1 * x1).exp() * ((-0.5 * x2 - x1) * x2).exp())
}

pub fn pnorm(x: f64, mu: f64, sigma: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || mu.is_nan() || sigma.is_nan() {
        return x + mu + sigma;
    }
    if !finite(x) && mu == x {
        return f64::NAN;
    }
    if sigma <= 0.0 {
        if sigma < 0.0 {
            return f64::NAN;
        }
        return if x < mu {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    let p = (x - mu) / sigma;
    if !finite(p) {
        return if x < mu {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    let (cumulative, complementary) = pnorm_both(p, if lower { 0 } else { 1 }, log);
    if lower { cumulative } else { complementary }
}

/// Both tails of the standard normal distribution by Cody's rational Chebyshev approximations
/// (Cody 1969, Math. Comp. 23). `tail` 0 computes the lower tail, 1 the upper and 2 both.
pub fn pnorm_both(x: f64, tail: c_int, log: bool) -> (f64, f64) {
    const A: [f64; 5] = [
        2.2352520354606839287,
        161.02823106855587881,
        1067.6894854603709582,
        18154.981253343561249,
        0.065682337918207449113,
    ];
    const B: [f64; 4] = [
        47.20258190468824187,
        976.09855173777669322,
        10260.932208618978205,
        45507.789335026729956,
    ];
    const C: [f64; 9] = [
        0.39894151208813466764,
        8.8831497943883759412,
        93.506656132177855979,
        597.27027639480026226,
        2494.5375852903726711,
        6848.1904505362823326,
        11602.651437647350124,
        9842.7148383839780218,
        1.0765576773720192317e-8,
    ];
    const D: [f64; 8] = [
        22.266688044328115691,
        235.38790178262499861,
        1519.377599407554805,
        6485.558298266760755,
        18615.571640885098091,
        34900.952721145977266,
        38912.003286093271411,
        19685.429676859990727,
    ];
    const P: [f64; 6] = [
        0.21589853405795699,
        0.1274011611602473639,
        0.022235277870649807,
        0.001421619193227893466,
        2.9112874951168792e-5,
        0.02307344176494017303,
    ];
    const Q: [f64; 5] = [
        1.28426009614491121,
        0.468238212480865118,
        0.0659881378689285515,
        0.00378239633202758244,
        7.29751555083966205e-5,
    ];
    if x.is_nan() {
        return (x, x);
    }
    let eps = DBL_EPSILON * 0.5;
    let lower = tail != 1;
    let upper = tail != 0;
    let mut cumulative = f64::NAN;
    let mut complementary = f64::NAN;
    let y = x.abs();

    // exp(-at^2/2) * temp, with at^2 split so that the large part of the square is exact.
    let tails_from = |temp: f64, at: f64, cumulative: &mut f64, complementary: &mut f64| {
        let xsq = (at * 16.0).trunc() / 16.0;
        let del = (at - xsq) * (at + xsq);
        if log {
            // Halving before squaring keeps x near sqrt(DBL_MAX) finite.
            *cumulative = (-0.5 * xsq * xsq) - del * 0.5 + temp.ln();
            if (lower && x > 0.0) || (upper && x <= 0.0) {
                *complementary = (-(-xsq * xsq * 0.5).exp() * (-del * 0.5).exp() * temp).ln_1p();
            }
        } else {
            *cumulative = (-xsq * xsq * 0.5).exp() * (-del * 0.5).exp() * temp;
            *complementary = 1.0 - *cumulative;
        }
    };
    let swap_tail = |cumulative: &mut f64, complementary: &mut f64| {
        if x > 0.0 {
            let temp = *cumulative;
            if lower {
                *cumulative = *complementary;
            }
            *complementary = temp;
        }
    };

    if y <= 0.67448975 {
        let (mut xnum, mut xden) = (0.0, 0.0);
        if y > eps {
            let xsq = x * x;
            xnum = A[4] * xsq;
            xden = xsq;
            for i in 0..3 {
                xnum = (xnum + A[i]) * xsq;
                xden = (xden + B[i]) * xsq;
            }
        }
        let temp = x * (xnum + A[3]) / (xden + B[3]);
        if lower {
            cumulative = 0.5 + temp;
        }
        if upper {
            complementary = 0.5 - temp;
        }
        if log {
            if lower {
                cumulative = cumulative.ln();
            }
            if upper {
                complementary = complementary.ln();
            }
        }
    } else if y <= M_SQRT_32 {
        let mut xnum = C[8] * y;
        let mut xden = y;
        for i in 0..7 {
            xnum = (xnum + C[i]) * y;
            xden = (xden + D[i]) * y;
        }
        let temp = (xnum + C[7]) / (xden + D[7]);
        tails_from(temp, y, &mut cumulative, &mut complementary);
        swap_tail(&mut cumulative, &mut complementary);
    } else if (log && y < 1e170)
        || (lower && -37.5193 < x && x < 8.2924)
        || (upper && -8.2924 < x && x < 37.5193)
    {
        let xsq = 1.0 / (x * x);
        let mut xnum = P[5] * xsq;
        let mut xden = xsq;
        for i in 0..4 {
            xnum = (xnum + P[i]) * xsq;
            xden = (xden + Q[i]) * xsq;
        }
        let mut temp = xsq * (xnum + P[4]) / (xden + Q[4]);
        temp = (M_1_SQRT_2PI - temp) / y;
        tails_from(temp, x, &mut cumulative, &mut complementary);
        swap_tail(&mut cumulative, &mut complementary);
    } else if x > 0.0 {
        cumulative = d1(log);
        complementary = d0(log);
    } else {
        cumulative = d0(log);
        complementary = d1(log);
    }
    (cumulative, complementary)
}

/// The normal quantile by Wichura's AS 241 (PPND16), with asymptotic expansions of the
/// Mills ratio where the log probability is so small that AS 241's tail formula loses digits.
pub fn qnorm(p: f64, mu: f64, sigma: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || mu.is_nan() || sigma.is_nan() {
        return p + mu + sigma;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, f64::NEG_INFINITY, f64::INFINITY) {
        return boundary;
    }
    if sigma < 0.0 {
        return f64::NAN;
    }
    if sigma == 0.0 {
        return mu;
    }
    let p_ = dt_qiv(p, lower, log);
    let q = p_ - 0.5;
    let mut value;
    if q.abs() <= 0.425 {
        let r = 0.180625 - q * q;
        value = q
            * (((((((r * 2509.0809287301226727 + 33430.575583588128105) * r
                + 67265.770927008700853)
                * r
                + 45921.953931549871457)
                * r
                + 13731.693765509461125)
                * r
                + 1971.5909503065514427)
                * r
                + 133.14166789178437745)
                * r
                + 3.387132872796366608)
            / (((((((r * 5226.495278852854561 + 28729.085735721942674) * r
                + 39307.89580009271061)
                * r
                + 21213.794301586595867)
                * r
                + 5394.1960214247511077)
                * r
                + 687.1870074920579083)
                * r
                + 42.313330701600911252)
                * r
                + 1.0);
    } else {
        let lp = if log && ((lower && q <= 0.0) || (!lower && q > 0.0)) {
            p
        } else {
            (if q > 0.0 { dt_civ(p, lower, log) } else { p_ }).ln()
        };
        let mut r = (-lp).sqrt();
        if r <= 5.0 {
            r -= 1.6;
            value = (((((((r * 7.7454501427834140764e-4 + 0.0227238449892691845833) * r
                + 0.24178072517745061177)
                * r
                + 1.27045825245236838258)
                * r
                + 3.64784832476320460504)
                * r
                + 5.7694972214606914055)
                * r
                + 4.6303378461565452959)
                * r
                + 1.42343711074968357734)
                / (((((((r * 1.05075007164441684324e-9 + 5.475938084995344946e-4) * r
                    + 0.0151986665636164571966)
                    * r
                    + 0.14810397642748007459)
                    * r
                    + 0.68976733498510000455)
                    * r
                    + 1.6763848301838038494)
                    * r
                    + 2.05319162663775882187)
                    * r
                    + 1.0);
        } else if r <= 27.0 {
            r -= 5.0;
            value = (((((((r * 2.01033439929228813265e-7 + 2.71155556874348757815e-5) * r
                + 0.0012426609473880784386)
                * r
                + 0.026532189526576123093)
                * r
                + 0.29656057182850489123)
                * r
                + 1.7848265399172913358)
                * r
                + 5.4637849111641143699)
                * r
                + 6.6579046435011037772)
                / (((((((r * 2.04426310338993978564e-15 + 1.4215117583164458887e-7) * r
                    + 1.8463183175100546818e-5)
                    * r
                    + 7.868691311456132591e-4)
                    * r
                    + 0.0148753612908506148525)
                    * r
                    + 0.13692988092273580531)
                    * r
                    + 0.59983220655588793769)
                    * r
                    + 1.0);
        } else if r >= 6.4e8 {
            value = r * M_SQRT2;
        } else {
            // Iterates x^2 = 2s - log(2 pi x^2) + 2 log(1 - 1/x^2 + 3/x^4 ...), the Mills
            // ratio expansion of the tail with s = -log(p), to more terms the closer p is to 0.
            let s2 = -2.0 * lp;
            let mut x2 = s2 - (M_2PI * s2).ln();
            if r < 36000.0 {
                x2 = s2 - (M_2PI * x2).ln() - 2.0 / (2.0 + x2);
                if r < 840.0 {
                    x2 = s2 - (M_2PI * x2).ln()
                        + 2.0 * (-(1.0 - 1.0 / (4.0 + x2)) / (2.0 + x2)).ln_1p();
                    if r < 109.0 {
                        x2 = s2 - (M_2PI * x2).ln()
                            + 2.0
                                * (-(1.0 - (1.0 - 5.0 / (6.0 + x2)) / (4.0 + x2)) / (2.0 + x2))
                                    .ln_1p();
                        if r < 55.0 {
                            x2 = s2 - (M_2PI * x2).ln()
                                + 2.0
                                    * (-(1.0
                                        - (1.0 - (5.0 - 9.0 / (8.0 + x2)) / (6.0 + x2))
                                            / (4.0 + x2))
                                        / (2.0 + x2))
                                        .ln_1p();
                        }
                    }
                }
            }
            value = x2.sqrt();
        }
        if q < 0.0 {
            value = -value;
        }
    }
    mu + sigma * value
}

// The uniform distribution.

pub fn dunif(x: f64, a: f64, b: f64, log: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if b <= a {
        return f64::NAN;
    }
    if a <= x && x <= b {
        if log { -(b - a).ln() } else { 1.0 / (b - a) }
    } else {
        d0(log)
    }
}

pub fn punif(x: f64, a: f64, b: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if b < a || !finite(a) || !finite(b) {
        return f64::NAN;
    }
    if x >= b {
        return dt1(lower, log);
    }
    if x <= a {
        return dt0(lower, log);
    }
    if lower {
        d_val((x - a) / (b - a), log)
    } else {
        d_val((b - x) / (b - a), log)
    }
}

pub fn qunif(p: f64, a: f64, b: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || a.is_nan() || b.is_nan() {
        return p + a + b;
    }
    if q_p01_invalid(p, log) || !finite(a) || !finite(b) || b < a {
        return f64::NAN;
    }
    if b == a {
        return a;
    }
    a + dt_qiv(p, lower, log) * (b - a)
}

// The Poisson density, the gamma distribution and its special cases.

pub fn dpois_raw(x: f64, lambda: f64, log: bool) -> f64 {
    if lambda == 0.0 {
        return if x == 0.0 { d1(log) } else { d0(log) };
    }
    if !finite(lambda) || x < 0.0 {
        return d0(log);
    }
    if x <= lambda * DBL_MIN {
        return d_exp(-lambda, log);
    }
    if lambda < x * DBL_MIN {
        if !finite(x) {
            return d0(log);
        }
        return d_exp(-lambda + x * lambda.ln() - lgammafn(x + 1.0), log);
    }
    let (high, low) = bd0_extended(x, lambda);
    let low = low + stirlerr(x);
    // 2 pi x overflows for x near DBL_MAX, where sqrt(2 pi) sqrt(x) does not.
    let root = M_SQRT_2PI * x.sqrt();
    if log {
        -low - high - root.ln()
    } else if low.abs() > 1.0 {
        // The low part stays below an ulp of the high part, which here is beyond the range
        // where the density is representable.
        (-(high + low)).exp() / root
    } else {
        (-low).exp() * (-high).exp() / root
    }
}

pub fn dpois(x: f64, lambda: f64, log: bool) -> f64 {
    if x.is_nan() || lambda.is_nan() {
        return x + lambda;
    }
    if lambda < 0.0 {
        return f64::NAN;
    }
    if nonint_warning(x) || x < 0.0 || !finite(x) {
        return d0(log);
    }
    dpois_raw(forceint(x), lambda, log)
}

pub fn dgamma(x: f64, shape: f64, scale: f64, log: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape < 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d0(log);
    }
    if shape == 0.0 {
        return if x == 0.0 { f64::INFINITY } else { d0(log) };
    }
    if x == 0.0 {
        if shape < 1.0 {
            return f64::INFINITY;
        }
        if shape > 1.0 {
            return d0(log);
        }
        return if log { -scale.ln() } else { 1.0 / scale };
    }
    if shape < 1.0 {
        let pr = dpois_raw(shape, x / scale, log);
        return if log {
            pr + if finite(shape / x) {
                (shape / x).ln()
            } else {
                shape.ln() - x.ln()
            }
        } else {
            pr * shape / x
        };
    }
    let pr = dpois_raw(shape - 1.0, x / scale, log);
    if log { pr - scale.ln() } else { pr / scale }
}

/// The gamma distribution function: the regularized incomplete gamma ratio `P(shape, x /
/// scale)` or its complement `Q`.
pub fn pgamma(x: f64, shape: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape < 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    let x = x / scale;
    if x.is_nan() {
        return x;
    }
    if shape == 0.0 {
        return if x <= 0.0 {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    // An infinite shape moves all the mass to infinity.
    if shape == f64::INFINITY {
        return if x == f64::INFINITY {
            dt1(lower, log)
        } else {
            dt0(lower, log)
        };
    }
    regularized_gamma(x, shape, lower, log)
}

/// `P(shape, x)` when `lower`, else `Q(shape, x) = 1 - P(shape, x)`, for `shape > 0`.
///
/// Each method computes one tail directly, and the other comes from its complement only
/// where the direct tail is at most about 0.9, so neither tail loses relative accuracy. The
/// methods are those of DiDonato and Morris (1986) and Gautschi (1979): the power series of
/// the lower tail below the mode, Legendre's continued fraction for the upper tail above it,
/// a series for `Q` itself when the shape is below one and `x` is small (where `Q` can be
/// tiny while `P` is near one), and Temme's (1979) uniform asymptotic expansion near the
/// mode of a large shape, where the series and the fraction would need `O(sqrt(shape))`
/// terms.
fn regularized_gamma(x: f64, shape: f64, lower: bool, log: bool) -> f64 {
    if x <= 0.0 {
        return dt0(lower, log);
    }
    if x == f64::INFINITY {
        return dt1(lower, log);
    }
    let ratio = x / shape;
    let (value, is_upper) = if shape < 1.0 && x < 1.5 {
        if gamma_lower_series(x, shape, false) < 0.5 {
            (gamma_lower_series(x, shape, log), false)
        } else {
            (gamma_upper_small_shape(x, shape, log), true)
        }
    } else if shape >= 20.0 && (0.3..=2.4).contains(&ratio) {
        gamma_uniform_expansion(x, shape, log)
    } else if x < shape + 1.0 {
        (gamma_lower_series(x, shape, log), false)
    } else {
        (gamma_upper_fraction(x, shape, log), true)
    };
    if is_upper != lower {
        value
    } else if log {
        log1_exp(value)
    } else {
        0.5 - value + 0.5
    }
}

/// `P(shape, x) = x^shape e^-x / Gamma(shape + 1) * sum_n x^n / ((shape + 1) ... (shape +
/// n))`, the series of Abramowitz and Stegun 6.5.29, for `x` below about `shape + 1`.
fn gamma_lower_series(x: f64, shape: f64, log: bool) -> f64 {
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut index = 1.0;
    while index < 1e6 {
        term *= x / (shape + index);
        sum += term;
        if term <= sum * DBL_EPSILON * 0.5 {
            break;
        }
        index += 1.0;
    }
    if log {
        gamma_density_factor(x, shape, true) + sum.ln()
    } else {
        gamma_density_factor(x, shape, false) * sum
    }
}

/// `Q(shape, x)` from Legendre's continued fraction (Abramowitz and Stegun 6.5.31),
///
/// `Gamma(shape, x) = e^-x x^shape / (x + 1 - shape - 1 (1 - shape) / (x + 3 - shape - 2 (2
/// - shape) / (x + 5 - shape - ...)))`,
///
/// evaluated by Lentz's method (Thompson and Barnett 1986) for `x` above about `shape + 1`.
fn gamma_upper_fraction(x: f64, shape: f64, log: bool) -> f64 {
    const TINY: f64 = 1e-300;
    let nonzero = |value: f64| if value.abs() < TINY { TINY } else { value };
    let mut denominator = nonzero(x + 1.0 - shape);
    let mut numerator_ratio = denominator;
    let mut inverse_ratio = 0.0;
    let mut index = 1.0;
    while index < 1e6 {
        let partial_numerator = -index * (index - shape);
        let partial_denominator = x + 2.0 * index + 1.0 - shape;
        inverse_ratio = 1.0 / nonzero(partial_denominator + partial_numerator * inverse_ratio);
        numerator_ratio = nonzero(partial_denominator + partial_numerator / numerator_ratio);
        let step = numerator_ratio * inverse_ratio;
        denominator *= step;
        if (step - 1.0).abs() <= DBL_EPSILON {
            break;
        }
        index += 1.0;
    }
    if log {
        shape.ln() + gamma_density_factor(x, shape, true) - denominator.ln()
    } else {
        shape * gamma_density_factor(x, shape, false) / denominator
    }
}

/// `Q(shape, x)` for `shape < 1` and small `x`, from the expansion of the lower incomplete
/// gamma function `gamma(shape, x) = x^shape sum_n (-x)^n / (n! (shape + n))` with its
/// first term split off (Gautschi 1979, section 4.1):
///
/// `Q = 1 - x^shape / Gamma(shape + 1) - x^shape / Gamma(shape + 1) * shape * sum_{n >= 1}
/// (-x)^n / (n! (shape + n))`.
///
/// The first difference is `-expm1(shape ln x - ln Gamma(shape + 1))`, which stays accurate
/// as `shape` goes to zero, where `Q` is about `shape E1(x)`.
fn gamma_upper_small_shape(x: f64, shape: f64, log: bool) -> f64 {
    let log_power = shape * x.ln() - lgamma1p(shape);
    let mut factor = 1.0;
    let mut sum = 0.0;
    let mut index = 1.0;
    while index < 1000.0 {
        factor *= -x / index;
        let term = factor / (shape + index);
        sum += term;
        if term.abs() <= sum.abs() * DBL_EPSILON * 0.5 {
            break;
        }
        index += 1.0;
    }
    let upper = -log_power.exp_m1() - log_power.exp() * shape * sum;
    if log { upper.ln() } else { upper }
}

/// The smaller tail of the gamma distribution near the mode of a large shape, by Temme's
/// (1979) uniform asymptotic expansion (DLMF 8.12.3, 8.12.7-8.12.11):
///
/// `Q(a, x) = erfc(eta sqrt(a / 2)) / 2 + e^(-a eta^2 / 2) / sqrt(2 pi a) sum_k c_k(eta) a^-k`
///
/// with `eta^2 / 2 = lambda - 1 - ln lambda`, `lambda = x / a`, and `eta` of the sign of
/// `lambda - 1`. Factoring out the normal density turns the error function into Mills'
/// ratio, so a tail far below the double range keeps its logarithm. Returns the tail on the
/// requested scale and whether it is the upper one.
fn gamma_uniform_expansion(x: f64, shape: f64, log: bool) -> (f64, bool) {
    let exponent = gamma_deviance(x, shape);
    let is_upper = x > shape;
    let magnitude = (2.0 * exponent / shape).sqrt();
    let eta = if is_upper { magnitude } else { -magnitude };
    let mut correction = 0.0;
    for row in TEMME_COEFFICIENTS.iter().rev() {
        let coefficient = row.iter().rev().fold(0.0, |sum, &value| sum * eta + value);
        correction = correction / shape + coefficient;
    }
    let root_shape = shape.sqrt();
    let mills = mills_ratio(magnitude * root_shape) * root_shape;
    let bracket = if is_upper {
        mills + correction
    } else {
        mills - correction
    };
    let value = if log {
        -exponent - 0.5 * (M_2PI * shape).ln() + bracket.ln()
    } else {
        (-exponent).exp() * bracket / (M_2PI * shape).sqrt()
    };
    (value, is_upper)
}

/// `x^shape e^-x / Gamma(shape + 1)` on the requested scale, as `exp(-stirlerr(shape) -
/// deviance) / sqrt(2 pi shape)` (Loader 2000), which keeps the large exponents of the two
/// factors from cancelling.
fn gamma_density_factor(x: f64, shape: f64, log: bool) -> f64 {
    let exponent = -stirlerr(shape) - gamma_deviance(x, shape);
    if log {
        exponent - 0.5 * (M_2PI * shape).ln()
    } else {
        exponent.exp() / (M_2PI * shape).sqrt()
    }
}

/// The deviance `shape ln(shape / x) + x - shape = shape (lambda - 1 - ln lambda)`, with
/// `lambda = x / shape`. Near `lambda = 1` the direct form cancels almost completely; there
/// `-shape log1pmx(lambda - 1)` is accurate to a few rounding errors relative. Far below,
/// `lambda - 1` would round away `lambda` itself, but the logarithm dominates the direct form.
fn gamma_deviance(x: f64, shape: f64) -> f64 {
    let relative = (x - shape) / shape;
    // A tiny shape against a huge `x` overflows `lambda - 1`; that far from 1 the direct form
    // has nothing to cancel.
    if x >= 0.25 * shape && relative.is_finite() {
        -shape * log1pmx(relative)
    } else {
        shape * (shape.ln() - x.ln()) + x - shape
    }
}

/// Mills' ratio `(1 - Phi(t)) / phi(t)` for `t >= 0`. Below 30 the normal tail does not
/// underflow and the quotient is exact to rounding; above, Laplace's continued fraction
/// `1 / (t + 1 / (t + 2 / (t + 3 / (t + ...))))` converges in a few terms.
fn mills_ratio(t: f64) -> f64 {
    if t < 30.0 {
        return pnorm(t, 0.0, 1.0, false, false) / dnorm(t, 0.0, 1.0, false);
    }
    let mut tail = t;
    for index in (1..=40).rev() {
        tail = t + index as f64 / tail;
    }
    1.0 / tail
}

/// The Taylor coefficients in `eta` of Temme's `c_k(eta)`, one row per `k`, derived from
/// `c_0 = 1 / (lambda - 1) - 1 / eta` and `c_k = c_{k-1}'(eta) / eta + (-1)^k g_k /
/// (lambda - 1)` (DLMF 8.12.10-8.12.11), with `g_k` the coefficients of Stirling's series
/// for the gamma function, by exact rational series reversion of `eta^2 / 2 = lambda - 1 -
/// ln lambda`. Rows stop where their terms fall below 1e-19 for `|eta| <= 1.05` and shapes
/// of at least 20.
const TEMME_COEFFICIENTS: [&[f64]; 11] = [
    &[
        -0.3333333333333333,
        0.08333333333333333,
        -0.014814814814814815,
        0.0011574074074074073,
        0.0003527336860670194,
        -0.0001787551440329218,
        3.919263178522438e-05,
        -2.185448510679992e-06,
        -1.85406221071516e-06,
        8.296711340953087e-07,
        -1.7665952736826078e-07,
        6.707853543401498e-09,
        1.0261809784240309e-08,
        -4.382036018453353e-09,
        9.14769958223679e-10,
        -2.5514193994946248e-11,
        -5.830772132550426e-11,
        2.4361948020667415e-11,
        -5.0276692801141755e-12,
        1.1004392031956135e-13,
        3.371763262400985e-13,
        -1.392388722418162e-13,
        2.8534893807047445e-14,
        -5.139111834242572e-16,
        -1.9752288294349442e-15,
        8.099521156704561e-16,
        -1.6522531216398162e-16,
        2.5305430097478883e-18,
        1.1686939738559576e-17,
        -4.770037049820485e-18,
    ],
    &[
        -0.001851851851851852,
        -0.003472222222222222,
        0.0026455026455026454,
        -0.0009902263374485596,
        0.00020576131687242798,
        -4.018775720164609e-07,
        -1.8098550334489977e-05,
        7.64916091608111e-06,
        -1.6120900894563446e-06,
        4.647127802807434e-09,
        1.378633446915721e-07,
        -5.752545603517705e-08,
        1.1951628599778148e-08,
        -1.7543241719747647e-11,
        -1.0091543710600413e-09,
        4.162792991842583e-10,
        -8.56390702649298e-11,
        6.067215101604758e-14,
        7.1624989648114856e-12,
        -2.933186643771437e-12,
        5.996696365683689e-13,
        -2.1671786527323313e-16,
        -4.978339972369262e-14,
        2.0291628823713425e-14,
        -4.13125571381061e-15,
        8.286516239883097e-19,
        3.4100308869333327e-16,
        -1.3854195302893971e-16,
        2.812346653228875e-17,
    ],
    &[
        0.004133597883597883,
        -0.0026813271604938273,
        0.0007716049382716049,
        2.0093878600823047e-06,
        -0.0001073665322636516,
        5.2923448829120125e-05,
        -1.2760635188618728e-05,
        3.423578734096138e-08,
        1.3721957309062934e-06,
        -6.298992138380055e-07,
        1.4280614206064242e-07,
        -2.0477098421990866e-10,
        -1.409252991086752e-08,
        6.228974084922022e-09,
        -1.3670488396617114e-09,
        9.428356159014678e-13,
        1.2872252400089318e-10,
        -5.5645956134363323e-11,
        1.197593554636698e-11,
        -4.1689782251838634e-15,
        -1.0940640427884595e-12,
        4.662239946390136e-13,
        -9.905105763906907e-14,
        1.8931876768373515e-17,
        8.859221872591127e-15,
        -3.737820398046405e-15,
        7.868833639035156e-16,
        -9.000027395741211e-20,
        -6.928881229347671e-17,
        2.9020384270164786e-17,
    ],
    &[
        0.0006494341563786008,
        0.00022947209362139917,
        -0.0004691894943952557,
        0.00026772063206283885,
        -7.561801671883977e-05,
        -2.396505113867297e-07,
        1.1082654115347302e-05,
        -5.6749528269915965e-06,
        1.4230900732435883e-06,
        -2.7861080291528143e-11,
        -1.6958404091930278e-07,
        8.099464905388083e-08,
        -1.9111168485973655e-08,
        2.3928620439808118e-12,
        2.0620131815488797e-09,
        -9.460496661855133e-10,
        2.1541049775774907e-10,
        -1.388823336813903e-14,
        -2.1894761681963938e-11,
        9.790998951171684e-12,
        -2.178219188018096e-12,
        6.208819573407901e-17,
        2.126978363279737e-13,
        -9.344688791517433e-14,
        2.045367122678285e-14,
        -2.58260790403495e-19,
        -1.9405297673344544e-15,
        8.415979290484816e-16,
    ],
    &[
        -0.0008618882909167117,
        0.0007840392217200666,
        -0.0002990724803031902,
        -1.4638452578843418e-06,
        6.641498215465122e-05,
        -3.968365047179435e-05,
        1.1375726970678419e-05,
        2.507497226237533e-10,
        -1.6954149536558305e-06,
        8.907507532205309e-07,
        -2.292934834000805e-07,
        2.956794137544049e-11,
        2.8865829742708783e-08,
        -1.4189739437803219e-08,
        3.4463580499464896e-09,
        -2.3024517174528067e-13,
        -3.9409233028046403e-10,
        1.86023389685045e-10,
        -4.356323005056618e-11,
        1.278600101629623e-15,
        4.67927502665792e-12,
        -2.149246470613483e-12,
        4.908815614809652e-13,
        -6.33859148489156e-18,
        -5.045332069080094e-14,
        2.2722958222901286e-14,
        -5.096082608472402e-15,
    ],
    &[
        -0.00033679855336635813,
        -6.972813758365857e-05,
        0.0002772753244959392,
        -0.00019932570516188847,
        6.797780477937208e-05,
        1.419062920643967e-07,
        -1.3594048189768693e-05,
        8.018470256334202e-06,
        -2.291481176508095e-06,
        -3.252473551298454e-10,
        3.4652846491085265e-07,
        -1.8447187191171344e-07,
        4.8240967037894184e-08,
        -1.7989466721743514e-14,
        -6.306194500013523e-09,
        3.162417628774568e-09,
        -7.840924253697429e-10,
        5.192679165254041e-15,
        9.358944242306784e-11,
        -4.513426216163278e-11,
        1.0799129993116828e-11,
        -3.661886712685252e-17,
        -1.210902069055155e-12,
        5.680743584990564e-13,
        -1.3249659916340829e-13,
    ],
    &[
        0.0005313079364639922,
        -0.0005921664373536939,
        0.0002708782096718045,
        7.902353232660328e-07,
        -8.153969367561969e-05,
        5.61168275310625e-05,
        -1.8329116582843375e-05,
        -3.0796134506033047e-09,
        3.465155368803609e-06,
        -2.0291327396058603e-06,
        5.788792863149004e-07,
        2.338630673826657e-13,
        -8.828600746330484e-08,
        4.7435958880408125e-08,
        -1.2545415020710383e-08,
        8.649648858010293e-14,
        1.6846058979264062e-09,
        -8.575492823577594e-10,
        2.1598224929232125e-10,
        -7.613230520476153e-16,
        -2.6639822008536144e-11,
        1.3065700536611057e-11,
        -3.1799163902367977e-12,
    ],
    &[
        0.00034436760689237765,
        5.171790908260592e-05,
        -0.00033493161081142234,
        0.0002812695154763237,
        -0.00010976582244684731,
        -1.2741009095484485e-07,
        2.7744451511563645e-05,
        -1.8263488805711332e-05,
        5.7876949497350525e-06,
        4.93875893393627e-10,
        -1.0595367014026043e-06,
        6.166714376110408e-07,
        -1.7562973359060463e-07,
        -1.297447328701544e-12,
        2.695423606288966e-08,
        -1.4578352908731272e-08,
        3.887645959386175e-09,
        -3.881002251019412e-17,
        -5.327994173877286e-10,
        2.7437977643314844e-10,
        -6.995796092070568e-11,
    ],
    &[
        -0.0006526239185953094,
        0.0008394987206720873,
        -0.000438297098541721,
        -6.969091458420552e-07,
        0.00016644846642067547,
        -0.00012783517679769218,
        4.629953263691304e-05,
        4.557909867922708e-09,
        -1.0595271125805195e-05,
        6.783342904865167e-06,
        -2.1075476666258803e-06,
        -1.7213731432817144e-11,
        3.773587741611098e-07,
        -2.1867506700122867e-07,
        6.220228804018927e-08,
        6.597703826733e-16,
        -9.590386497425686e-09,
        5.213214492280807e-09,
        -1.3991589583935709e-09,
    ],
    &[
        -0.0005967612901927463,
        -7.204895416020011e-05,
        0.0006782308837667328,
        -0.0006401475260262758,
        0.00027750107634328704,
        1.819700838046515e-07,
        -8.479507117068503e-05,
        6.105192082501531e-05,
        -2.1073920183404862e-05,
        -8.858589014125599e-10,
        4.5284535953805374e-06,
        -2.8427815022504407e-06,
        8.708234177864641e-07,
        3.6886101871706966e-12,
        -1.534469519070206e-07,
        8.862466778790695e-08,
        -2.5184812301826817e-08,
    ],
    &[
        0.0013324454494800656,
        -0.0019144384985654776,
        0.0011089369134596636,
        9.9324041226423e-07,
        -0.0005087450129309319,
        0.00042735056665392886,
        -0.00016858853767910798,
        -8.1301893922785e-09,
        4.5284402370562144e-05,
        -3.127053674781734e-05,
        1.044986828530338e-05,
        4.8435226265680926e-11,
        -2.148256587345626e-06,
        1.329369701097492e-06,
    ],
];

/// The starting value of `qgamma`: AS 91's chi-squared approximations.
pub fn qchisq_appr(p: f64, nu: f64, g: f64, lower: bool, log: bool, tol: f64) -> f64 {
    const C7: f64 = 4.67;
    const C8: f64 = 6.66;
    const C9: f64 = 6.73;
    const C10: f64 = 13.32;
    if p.is_nan() || nu.is_nan() {
        return p + nu;
    }
    if q_p01_invalid(p, log) || nu <= 0.0 {
        return f64::NAN;
    }
    let alpha = 0.5 * nu;
    let c = alpha - 1.0;
    let p1 = dt_log(p, lower, log);
    if nu < -1.24 * p1 {
        let lgam1pa = if alpha < 0.5 {
            lgamma1p(alpha)
        } else {
            alpha.ln() + g
        };
        ((lgam1pa + p1) / alpha + M_LN2).exp()
    } else if nu > 0.32 {
        let x = qnorm(p, 0.0, 1.0, lower, log);
        let p1 = 2.0 / (9.0 * nu);
        let mut ch = nu * (x * p1.sqrt() + 1.0 - p1).powf(3.0);
        if ch > 2.2 * nu + 6.0 {
            ch = -2.0 * (dt_clog(p, lower, log) - c * (0.5 * ch).ln() + g);
        }
        ch
    } else {
        let mut ch = 0.4;
        let a = dt_clog(p, lower, log) + g + c * M_LN2;
        loop {
            let q = ch;
            let p1 = 1.0 / (1.0 + ch * (C7 + ch));
            let p2 = ch * (C9 + ch * (C8 + ch));
            let t = -0.5 + (C7 + 2.0 * ch) * p1 - (C9 + ch * (C10 + 3.0 * ch)) / p2;
            ch -= (1.0 - (a + 0.5 * ch).exp() * p2 * p1) / t;
            if !((q - ch).abs() > tol * ch.abs()) {
                break;
            }
        }
        ch
    }
}

/// The gamma quantile: AS 91 (Best and Roberts) with its seven-term Taylor iteration, then
/// Newton steps on the log scale to full precision.
pub fn qgamma(p: f64, alpha: f64, scale: f64, lower: bool, log: bool) -> f64 {
    const EPS1: f64 = 1e-2;
    const EPS2: f64 = 5e-7;
    const EPS_N: f64 = 1e-15;
    const MAXIT: i32 = 1000;
    const P_MIN: f64 = 1e-100;
    const P_MAX: f64 = 1.0 - 1e-14;
    const I420: f64 = 1.0 / 420.0;
    const I2520: f64 = 1.0 / 2520.0;
    const I5040: f64 = 1.0 / 5040.0;
    if p.is_nan() || alpha.is_nan() || scale.is_nan() {
        return p + alpha + scale;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    if alpha < 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if alpha == 0.0 {
        return 0.0;
    }
    let mut p_ = dt_qiv(p, lower, log);
    // Within 1e-8 of one, the lower probability that the AS 91 stage solves for keeps fewer
    // than eight significant digits of its complement, so Newton's method on the requested
    // scale has to carry the quantile the rest of the way.
    let mut max_it_newton = if alpha < 1e-10 {
        7
    } else if p_ > 1.0 - 1e-8 {
        20
    } else {
        1
    };
    let g = lgammafn(alpha);
    let mut ch = qchisq_appr(p, 2.0 * alpha, g, lower, log, EPS1);
    'iterate: {
        if !finite(ch) {
            max_it_newton = 0;
            break 'iterate;
        }
        if ch < EPS2 || !(P_MIN..=P_MAX).contains(&p_) {
            max_it_newton = 20;
            break 'iterate;
        }
        let c = alpha - 1.0;
        let s6 = (120.0 + c * (346.0 + 127.0 * c)) * I5040;
        let ch0 = ch;
        for _ in 1..=MAXIT {
            let q = ch;
            let p1 = 0.5 * ch;
            let p2 = p_ - regularized_gamma(p1, alpha, true, false);
            if !finite(p2) || ch <= 0.0 {
                ch = ch0;
                max_it_newton = 27;
                break 'iterate;
            }
            let t = p2 * (alpha * M_LN2 + g + p1 - c * ch.ln()).exp();
            let b = t / ch;
            let a = 0.5 * t - b * c;
            let s1 =
                (210.0 + a * (140.0 + a * (105.0 + a * (84.0 + a * (70.0 + 60.0 * a))))) * I420;
            let s2 = (420.0 + a * (735.0 + a * (966.0 + a * (1141.0 + 1278.0 * a)))) * I2520;
            let s3 = (210.0 + a * (462.0 + a * (707.0 + 932.0 * a))) * I2520;
            let s4 =
                (252.0 + a * (672.0 + 1182.0 * a) + c * (294.0 + a * (889.0 + 1740.0 * a))) * I5040;
            let s5 = (84.0 + 2264.0 * a + c * (1175.0 + 606.0 * a)) * I2520;
            ch += t
                * (1.0 + 0.5 * t * s1
                    - b * c * (s1 - b * (s2 - b * (s3 - b * (s4 - b * (s5 - b * s6))))));
            if (q - ch).abs() < EPS2 * ch {
                break 'iterate;
            }
            if (q - ch).abs() > 0.1 * ch {
                ch = if ch < q { 0.9 * q } else { 1.1 * q };
            }
        }
    }
    let mut x = 0.5 * scale * ch;
    if max_it_newton > 0 {
        let (p, log) = if log { (p, log) } else { (p.ln(), true) };
        if x == 0.0 {
            x = DBL_MIN;
            p_ = pgamma(x, alpha, scale, lower, log);
            if (lower && p_ > p * (1.0 + 1e-7)) || (!lower && p_ < p * (1.0 - 1e-7)) {
                return 0.0;
            }
        } else {
            p_ = pgamma(x, alpha, scale, lower, log);
        }
        if p_ == f64::NEG_INFINITY {
            return 0.0;
        }
        for i in 1..=max_it_newton {
            let p1 = p_ - p;
            if p1.abs() < (EPS_N * p).abs() {
                break;
            }
            let g = dgamma(x, alpha, scale, log);
            if g == d0(log) {
                break;
            }
            let t = if log { p1 * (p_ - g).exp() } else { p1 / g };
            let t = if lower { x - t } else { x + t };
            p_ = pgamma(t, alpha, scale, lower, log);
            if (p_ - p).abs() > p1.abs() || (i > 1 && (p_ - p).abs() == p1.abs()) {
                break;
            }
            x = t;
        }
    }
    x
}

pub fn dchisq(x: f64, df: f64, log: bool) -> f64 {
    dgamma(x, df / 2.0, 2.0, log)
}

pub fn pchisq(x: f64, df: f64, lower: bool, log: bool) -> f64 {
    pgamma(x, df / 2.0, 2.0, lower, log)
}

pub fn qchisq(p: f64, df: f64, lower: bool, log: bool) -> f64 {
    qgamma(p, 0.5 * df, 2.0, lower, log)
}

pub fn dexp(x: f64, scale: f64, log: bool) -> f64 {
    if x.is_nan() || scale.is_nan() {
        return x + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d0(log);
    }
    if log {
        (-x / scale) - scale.ln()
    } else {
        (-x / scale).exp() / scale
    }
}

pub fn pexp(x: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || scale.is_nan() {
        return x + scale;
    }
    if scale < 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt0(lower, log);
    }
    let x = -(x / scale);
    if lower {
        if log { log1_exp(x) } else { -x.exp_m1() }
    } else {
        d_exp(x, log)
    }
}

pub fn qexp(p: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || scale.is_nan() {
        return p + scale;
    }
    if scale < 0.0 || q_p01_invalid(p, log) {
        return f64::NAN;
    }
    if p == dt0(lower, log) {
        return 0.0;
    }
    -scale * dt_clog(p, lower, log)
}

pub fn dweibull(x: f64, shape: f64, scale: f64, log: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape <= 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 || !finite(x) {
        return d0(log);
    }
    if x == 0.0 && shape < 1.0 {
        return f64::INFINITY;
    }
    let tmp1 = (x / scale).powf(shape - 1.0);
    let tmp2 = tmp1 * (x / scale);
    if log {
        -tmp2 + (shape * tmp1 / scale).ln()
    } else {
        shape * tmp1 * (-tmp2).exp() / scale
    }
}

pub fn pweibull(x: f64, shape: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape <= 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt0(lower, log);
    }
    let x = -(x / scale).powf(shape);
    if lower {
        if log { log1_exp(x) } else { -x.exp_m1() }
    } else {
        d_exp(x, log)
    }
}

pub fn qweibull(p: f64, shape: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || shape.is_nan() || scale.is_nan() {
        return p + shape + scale;
    }
    if shape <= 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    scale * (-dt_clog(p, lower, log)).powf(1.0 / shape)
}

pub fn dlnorm(x: f64, meanlog: f64, sdlog: f64, log: bool) -> f64 {
    if x.is_nan() || meanlog.is_nan() || sdlog.is_nan() {
        return x + meanlog + sdlog;
    }
    if sdlog < 0.0 {
        return f64::NAN;
    }
    if !finite(x) && x.ln() == meanlog {
        return f64::NAN;
    }
    if sdlog == 0.0 {
        return if x.ln() == meanlog {
            f64::INFINITY
        } else {
            d0(log)
        };
    }
    if x <= 0.0 {
        return d0(log);
    }
    let y = (x.ln() - meanlog) / sdlog;
    if log {
        -(M_LN_SQRT_2PI + 0.5 * y * y + (x * sdlog).ln())
    } else {
        M_1_SQRT_2PI * (-0.5 * y * y).exp() / (x * sdlog)
    }
}

pub fn plnorm(x: f64, meanlog: f64, sdlog: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || meanlog.is_nan() || sdlog.is_nan() {
        return x + meanlog + sdlog;
    }
    if sdlog < 0.0 {
        return f64::NAN;
    }
    if x > 0.0 {
        return pnorm(x.ln(), meanlog, sdlog, lower, log);
    }
    dt0(lower, log)
}

pub fn qlnorm(p: f64, meanlog: f64, sdlog: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || meanlog.is_nan() || sdlog.is_nan() {
        return p + meanlog + sdlog;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    qnorm(p, meanlog, sdlog, lower, log).exp()
}

pub fn dcauchy(x: f64, location: f64, scale: f64, log: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let y = (x - location) / scale;
    if log {
        -(M_PI * scale * (1.0 + y * y)).ln()
    } else {
        1.0 / (M_PI * scale * (1.0 + y * y))
    }
}

pub fn pcauchy(x: f64, location: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let mut x = (x - location) / scale;
    if x.is_nan() {
        return f64::NAN;
    }
    if !finite(x) {
        return if x < 0.0 {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    if !lower {
        x = -x;
    }
    // atan(1/x) for large |x| avoids the cancellation in 1/2 + atan(x)/pi.
    if x.abs() > 1.0 {
        let y = (1.0 / x).atan() / M_PI;
        if x > 0.0 {
            d_clog(y, log)
        } else {
            d_val(-y, log)
        }
    } else {
        d_val(0.5 + x.atan() / M_PI, log)
    }
}

pub fn qcauchy(p: f64, location: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || location.is_nan() || scale.is_nan() {
        return p + location + scale;
    }
    if q_p01_invalid(p, log) {
        return f64::NAN;
    }
    if scale <= 0.0 || !finite(scale) {
        if scale == 0.0 {
            return location;
        }
        return f64::NAN;
    }
    let infinity = |lower: bool| location + if lower { scale } else { -scale } * f64::INFINITY;
    let mut lower = lower;
    let mut p = p;
    if log {
        if p > -1.0 {
            if p == 0.0 {
                return infinity(lower);
            }
            lower = !lower;
            p = -p.exp_m1();
        } else {
            p = p.exp();
        }
    } else if p > 0.5 {
        if p == 1.0 {
            return infinity(lower);
        }
        p = 1.0 - p;
        lower = !lower;
    }
    if p == 0.5 {
        return location;
    }
    if p == 0.0 {
        return location + if lower { scale } else { -scale } * f64::NEG_INFINITY;
    }
    location + if lower { -scale } else { scale } / tanpi_(p)
}

pub fn dlogis(x: f64, location: f64, scale: f64, log: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let x = ((x - location) / scale).abs();
    let e = (-x).exp();
    let f = 1.0 + e;
    if log {
        -(x + (scale * f * f).ln())
    } else {
        e / (scale * f * f)
    }
}

pub fn plogis(x: f64, location: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let x = (x - location) / scale;
    if x.is_nan() {
        return f64::NAN;
    }
    if !finite(x) {
        return if x > 0.0 {
            dt1(lower, log)
        } else {
            dt0(lower, log)
        };
    }
    let exponent = if lower { -x } else { x };
    if log {
        -log1pexp(exponent)
    } else {
        1.0 / (1.0 + exponent.exp())
    }
}

pub fn qlogis(p: f64, location: f64, scale: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || location.is_nan() || scale.is_nan() {
        return p + location + scale;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, f64::NEG_INFINITY, f64::INFINITY) {
        return boundary;
    }
    if scale < 0.0 {
        return f64::NAN;
    }
    if scale == 0.0 {
        return location;
    }
    let p = if log {
        if lower {
            p - log1_exp(p)
        } else {
            log1_exp(p) - p
        }
    } else if lower {
        (p / (1.0 - p)).ln()
    } else {
        ((1.0 - p) / p).ln()
    };
    location + scale * p
}

// The beta distribution. The incomplete beta ratio is Didonato and Morris's ACM TOMS 708
// (`bratio`), extended to return log probabilities without underflow.

pub fn dbeta(x: f64, a: f64, b: f64, log: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if a < 0.0 || b < 0.0 {
        return f64::NAN;
    }
    if !(0.0..=1.0).contains(&x) {
        return d0(log);
    }
    if a == 0.0 || b == 0.0 || !finite(a) || !finite(b) {
        let mass_at = if a == 0.0 && b == 0.0 {
            return if x == 0.0 || x == 1.0 {
                f64::INFINITY
            } else {
                d0(log)
            };
        } else if a == 0.0 || b / a == f64::INFINITY {
            0.0
        } else if b == 0.0 || a / b == f64::INFINITY {
            1.0
        } else {
            0.5
        };
        return if x == mass_at { f64::INFINITY } else { d0(log) };
    }
    if x == 0.0 {
        if a > 1.0 {
            return d0(log);
        }
        if a < 1.0 {
            return f64::INFINITY;
        }
        return d_val(b, log);
    }
    if x == 1.0 {
        if b > 1.0 {
            return d0(log);
        }
        if b < 1.0 {
            return f64::INFINITY;
        }
        return d_val(a, log);
    }
    let lval = if a <= 2.0 || b <= 2.0 {
        (a - 1.0) * x.ln() + (b - 1.0) * (-x).ln_1p() - lbeta(a, b)
    } else {
        (a + b - 1.0).ln() + dbinom_raw(a - 1.0, a + b - 2.0, x, 1.0 - x, true)
    };
    d_exp(lval, log)
}

/// `pbeta` without the argument checks. The limits of the parameters are point masses, and
/// GNU R answers them before looking at `x`.
pub fn pbeta_raw(x: f64, a: f64, b: f64, lower: bool, log: bool) -> f64 {
    if x >= 1.0 {
        return dt1(lower, log);
    }
    if a == 0.0 || b == 0.0 || !finite(a) || !finite(b) {
        if a == 0.0 && b == 0.0 {
            return if log { -M_LN2 } else { 0.5 };
        }
        if a == 0.0 || b / a == f64::INFINITY {
            return dt1(lower, log);
        }
        if b == 0.0 || a / b == f64::INFINITY {
            return dt0(lower, log);
        }
        return if x < 0.5 {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    if x <= 0.0 {
        return dt0(lower, log);
    }
    let x1 = 0.5 - x + 0.5;
    let (w, wc, _) = bratio(a, b, x, x1, log);
    if lower { w } else { wc }
}

pub fn pbeta(x: f64, a: f64, b: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if a < 0.0 || b < 0.0 {
        return f64::NAN;
    }
    pbeta_raw(x, a, b, lower, log)
}

/// The incomplete beta ratio `I_x(a, b)` and its complement `1 - I_x(a, b)`, with `y = 1 - x`
/// passed separately for accuracy near one, and TOMS 708's error code.
fn bratio(a: f64, b: f64, x: f64, y: f64, log: bool) -> (f64, f64, i32) {
    let mut eps = 2.0 * (0.5 * DBL_EPSILON);
    let mut w = d0(log);
    let mut w1 = d0(log);
    if x.is_nan() || y.is_nan() || a.is_nan() || b.is_nan() {
        return (w, w1, 9);
    }
    if a < 0.0 || b < 0.0 {
        return (w, w1, 1);
    }
    if a == 0.0 && b == 0.0 {
        return (w, w1, 2);
    }
    if !(0.0..=1.0).contains(&x) {
        return (w, w1, 3);
    }
    if !(0.0..=1.0).contains(&y) {
        return (w, w1, 4);
    }
    let z = x + y - 0.5 - 0.5;
    if z.abs() > eps * 3.0 {
        return (w, w1, 5);
    }
    if x == 0.0 {
        if a == 0.0 {
            return (w, w1, 6);
        }
        return (d0(log), d1(log), 0);
    }
    if y == 0.0 {
        if b == 0.0 {
            return (w, w1, 7);
        }
        return (d1(log), d0(log), 0);
    }
    if a == 0.0 {
        return (d1(log), d0(log), 0);
    }
    if b == 0.0 {
        return (d0(log), d1(log), 0);
    }

    eps = eps.max(1e-15);
    let a_lt_b = a < b;
    if (if a_lt_b { b } else { a }) < eps * 0.001 {
        // Both parameters are so small that the ratio no longer depends on x.
        return if log {
            if a_lt_b {
                ((-a / (a + b)).ln_1p(), (a / (a + b)).ln(), 0)
            } else {
                ((b / (a + b)).ln(), (-b / (a + b)).ln_1p(), 0)
            }
        } else {
            (b / (a + b), a / (a + b), 0)
        };
    }

    let mut ierr = 0;
    let (mut a0, mut b0, x0, y0);
    let do_swap;
    // How w and w1 were computed, which decides how the other one is derived.
    enum Done {
        Both,
        FromW,
        FromW1,
        FromW1Log,
    }
    let done;

    if a.min(b) <= 1.0 {
        do_swap = x > 0.5;
        if do_swap {
            (a0, x0, b0, y0) = (b, y, a, x);
        } else {
            (a0, x0, b0, y0) = (a, x, b, y);
        }
        enum Path {
            WBpser,
            W1Bpser,
            Bgrat { w1: f64, did_bup: bool, n: i32 },
        }
        let path;
        if b0 < eps.min(eps * a0) {
            w = fpser(a0, b0, x0, eps, log);
            w1 = if log { log1_exp(w) } else { 0.5 - w + 0.5 };
            return finish(w, w1, do_swap, ierr);
        }
        if a0 < eps.min(eps * b0) && b0 * x0 <= 1.0 {
            w1 = apser(a0, b0, x0, eps);
            return end_from_w1(w1, log, do_swap, ierr);
        }
        if a0.max(b0) > 1.0 {
            if b0 <= 1.0 {
                path = Path::WBpser;
            } else if x0 >= 0.29 {
                path = Path::W1Bpser;
            } else if x0 < 0.1 && (x0 * b0).powf(a0) <= 0.7 {
                path = Path::WBpser;
            } else if b0 > 15.0 {
                path = Path::Bgrat {
                    w1: 0.0,
                    did_bup: false,
                    n: 0,
                };
            } else {
                let n = 20;
                path = Path::Bgrat {
                    w1: bup(b0, a0, y0, x0, n, eps, false),
                    did_bup: true,
                    n,
                };
            }
        } else if a0 >= 0.2f64.min(b0) || x0.powf(a0) <= 0.9 {
            path = Path::WBpser;
        } else if x0 >= 0.3 {
            path = Path::W1Bpser;
        } else {
            let n = 20;
            path = Path::Bgrat {
                w1: bup(b0, a0, y0, x0, n, eps, false),
                did_bup: true,
                n,
            };
        }
        match path {
            Path::WBpser => {
                w = bpser(a0, b0, x0, eps, log);
                w1 = if log { log1_exp(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::W1Bpser => {
                w1 = bpser(b0, a0, y0, eps, log);
                w = if log { log1_exp(w1) } else { 0.5 - w1 + 0.5 };
                done = Done::Both;
            }
            Path::Bgrat {
                w1: start,
                did_bup,
                n,
            } => {
                w1 = start;
                if did_bup {
                    b0 += n as f64;
                }
                let mut ierr1 = 0;
                bgrat(b0, a0, y0, x0, &mut w1, 15.0 * eps, &mut ierr1, false);
                if w1 == 0.0 || (0.0 < w1 && w1 < DBL_MIN) {
                    // Most likely underflow: redo the whole sum on the log scale.
                    w1 = if did_bup {
                        bup(b0 - n as f64, a0, y0, x0, n, eps, true)
                    } else {
                        f64::NEG_INFINITY
                    };
                    bgrat(b0, a0, y0, x0, &mut w1, 15.0 * eps, &mut ierr1, true);
                    if ierr1 != 0 {
                        ierr = 10 + ierr1;
                    }
                    done = Done::FromW1Log;
                } else {
                    if ierr1 != 0 {
                        ierr = 10 + ierr1;
                    }
                    if w1 < 0.0 {
                        warning(format!("bratio(a={a}, b={b}, x={x}): bgrat() -> w1 = {w1}"));
                    }
                    done = Done::FromW1;
                }
            }
        }
    } else {
        let mut lambda = if finite(a + b) {
            if a > b {
                (a + b) * y - b
            } else {
                a - (a + b) * x
            }
        } else {
            a * y - b * x
        };
        do_swap = lambda < 0.0;
        if do_swap {
            lambda = -lambda;
            (a0, x0, b0, y0) = (b, y, a, x);
        } else {
            (a0, x0, b0, y0) = (a, x, b, y);
        }
        enum Path {
            WBpser,
            Bfrac,
            Bup,
            Basym,
        }
        let path = if b0 < 40.0 {
            if b0 * x0 <= 0.7 || (log && lambda > 650.0) {
                Path::WBpser
            } else {
                Path::Bup
            }
        } else if a0 > b0 {
            if b0 <= 100.0 || lambda > b0 * 0.03 {
                Path::Bfrac
            } else {
                Path::Basym
            }
        } else if a0 <= 100.0 || lambda > a0 * 0.03 {
            Path::Bfrac
        } else {
            Path::Basym
        };
        match path {
            Path::WBpser => {
                w = bpser(a0, b0, x0, eps, log);
                w1 = if log { log1_exp(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::Bfrac => {
                w = bfrac(a0, b0, x0, y0, lambda, eps * 15.0, log);
                w1 = if log { log1_exp(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::Basym => {
                w = basym(a0, b0, lambda, eps * 100.0, log);
                w1 = if log { log1_exp(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::Bup => {
                let mut n = b0 as i32;
                b0 -= n as f64;
                if b0 == 0.0 {
                    n -= 1;
                    b0 = 1.0;
                }
                w = bup(b0, a0, y0, x0, n, eps, false);
                if w < DBL_MIN && log {
                    b0 += n as f64;
                    w = bpser(a0, b0, x0, eps, log);
                    w1 = log1_exp(w);
                    done = Done::Both;
                } else if x0 <= 0.7 {
                    w += bpser(a0, b0, x0, eps, false);
                    done = Done::FromW;
                } else {
                    if a0 <= 15.0 {
                        let n = 20;
                        w += bup(a0, b0, x0, y0, n, eps, false);
                        a0 += n as f64;
                    }
                    let mut ierr1 = 0;
                    bgrat(a0, b0, x0, y0, &mut w, 15.0 * eps, &mut ierr1, false);
                    if ierr1 != 0 {
                        ierr = 10 + ierr1;
                    }
                    done = Done::FromW;
                }
            }
        }
    }

    match done {
        Done::Both => {}
        Done::FromW => {
            if log {
                w1 = (-w).ln_1p();
                w = w.ln();
            } else {
                w1 = 0.5 - w + 0.5;
            }
        }
        Done::FromW1 => return end_from_w1(w1, log, do_swap, ierr),
        Done::FromW1Log => {
            if log {
                w = log1_exp(w1);
            } else {
                w = -w1.exp_m1();
                w1 = w1.exp();
            }
        }
    }
    finish(w, w1, do_swap, ierr)
}

fn end_from_w1(w1: f64, log: bool, do_swap: bool, ierr: i32) -> (f64, f64, i32) {
    let (w, w1) = if log {
        ((-w1).ln_1p(), w1.ln())
    } else {
        (0.5 - w1 + 0.5, w1)
    };
    finish(w, w1, do_swap, ierr)
}

fn finish(w: f64, w1: f64, do_swap: bool, ierr: i32) -> (f64, f64, i32) {
    if do_swap {
        (w1, w, ierr)
    } else {
        (w, w1, ierr)
    }
}

/// `I_x(a, b)` for `b < min(eps, eps * a)` and `x <= 1/2`.
fn fpser(a: f64, b: f64, x: f64, eps: f64, log: bool) -> f64 {
    let mut answer;
    if log {
        answer = a * x.ln();
    } else if a > eps * 0.001 {
        let t = a * x.ln();
        if t < exparg(1) {
            return 0.0;
        }
        answer = t.exp();
    } else {
        answer = 1.0;
    }
    if log {
        answer += b.ln() - a.ln();
    } else {
        answer *= b / a;
    }
    let tol = eps / a;
    let mut an = a + 1.0;
    let mut t = x;
    let mut s = t / an;
    loop {
        an += 1.0;
        t *= x;
        let c = t / an;
        s += c;
        if !(c.abs() > tol) {
            break;
        }
    }
    if log {
        answer += (a * s).ln_1p();
    } else {
        answer *= a * s + 1.0;
    }
    answer
}

/// `I_{1-x}(b, a)` for `a <= min(eps, eps * b)`, `b x <= 1` and `x <= 1/2`.
fn apser(a: f64, b: f64, x: f64, eps: f64) -> f64 {
    const G: f64 = 0.577215664901533;
    let bx = b * x;
    let mut t = x - bx;
    let c = if b * eps <= 0.02 {
        x.ln() + psi(b) + G + t
    } else {
        bx.ln() + G + t
    };
    let tol = eps * 5.0 * c.abs();
    let mut j = 1.0;
    let mut s = 0.0;
    loop {
        j += 1.0;
        t *= x - bx / j;
        let aj = t / j;
        s += aj;
        if !(aj.abs() > tol) {
            break;
        }
    }
    -a * (c + s)
}

/// The power series of `I_x(a, b)` for `b <= 1` or `b x <= 0.7`.
fn bpser(a: f64, b: f64, x: f64, eps: f64, log: bool) -> f64 {
    if x == 0.0 {
        return d0(log);
    }
    let mut answer;
    let a0 = a.min(b);
    if a0 >= 1.0 {
        let z = a * x.ln() - betaln(a, b);
        answer = if log { z - a.ln() } else { z.exp() / a };
    } else {
        let mut b0 = a.max(b);
        if b0 < 8.0 {
            if b0 <= 1.0 {
                if log {
                    answer = a * x.ln();
                } else {
                    answer = x.powf(a);
                    if answer == 0.0 {
                        return answer;
                    }
                }
                let apb = a + b;
                let z = if apb > 1.0 {
                    (gam1(apb - 1.0) + 1.0) / apb
                } else {
                    gam1(apb) + 1.0
                };
                let c = (gam1(a) + 1.0) * (gam1(b) + 1.0) / z;
                if log {
                    answer += (c * (b / apb)).ln();
                } else {
                    answer *= c * (b / apb);
                }
            } else {
                let mut u = gamln1(a0);
                let m = (b0 - 1.0) as i32;
                if m >= 1 {
                    let mut c = 1.0;
                    for _ in 1..=m {
                        b0 -= 1.0;
                        c *= b0 / (a0 + b0);
                    }
                    u += c.ln();
                }
                let z = a * x.ln() - u;
                b0 -= 1.0;
                let apb = a0 + b0;
                let t = if apb > 1.0 {
                    (gam1(apb - 1.0) + 1.0) / apb
                } else {
                    gam1(apb) + 1.0
                };
                answer = if log {
                    z + (a0 / a).ln() + gam1(b0).ln_1p() - t.ln()
                } else {
                    z.exp() * (a0 / a) * (gam1(b0) + 1.0) / t
                };
            }
        } else {
            let u = gamln1(a0) + algdiv(a0, b0);
            let z = a * x.ln() - u;
            answer = if log {
                z + (a0 / a).ln()
            } else {
                a0 / a * z.exp()
            };
        }
    }
    if answer == d0(log) || (!log && a <= eps * 0.1) {
        return answer;
    }
    let tol = eps / a;
    let mut n = 0.0;
    let mut sum = 0.0;
    let mut c = 1.0;
    let mut w;
    loop {
        n += 1.0;
        c *= (0.5 - b / n + 0.5) * x;
        w = c / (a + n);
        sum += w;
        if !(n < 1e7 && w.abs() > tol) {
            break;
        }
    }
    if w.abs() > tol
        && ((log && !(a * sum > -1.0 && (a * sum).ln_1p().abs() < eps * answer.abs()))
            || (!log && (a * sum + 1.0).abs() != 1.0))
    {
        warning(format!(
            " bpser(a={a}, b={b}, x={x},...) did not converge (n=1e7, |w|/tol={} > 1; A={answer})",
            w.abs() / tol
        ));
    }
    if log {
        if a * sum > -1.0 {
            answer += (a * sum).ln_1p();
        } else {
            if answer > f64::NEG_INFINITY {
                warning(format!(
                    "pbeta(*, log.p=TRUE) -> bpser(a={a}, b={b}, x={x},...) underflow to -Inf"
                ));
            }
            answer = f64::NEG_INFINITY;
        }
    } else if a * sum > -1.0 {
        answer *= a * sum + 1.0;
    } else {
        answer = 0.0;
    }
    answer
}

/// `I_x(a, b) - I_x(a + n, b)` for a positive integer `n`.
#[allow(clippy::too_many_arguments)]
fn bup(a: f64, b: f64, x: f64, y: f64, n: i32, eps: f64, log: bool) -> f64 {
    let apb = a + b;
    let ap1 = a + 1.0;
    let (mu, mut d);
    if n > 1 && a >= 1.0 && apb >= ap1 * 1.1 {
        let mut m = exparg(1).abs() as i32;
        let k = exparg(0) as i32;
        if m > k {
            m = k;
        }
        mu = m;
        d = (-(mu as f64)).exp();
    } else {
        mu = 0;
        d = 1.0;
    }
    let mut answer = if log {
        brcmp1(mu, a, b, x, y, true) - a.ln()
    } else {
        brcmp1(mu, a, b, x, y, false) / a
    };
    if n == 1 || (log && answer == f64::NEG_INFINITY) || (!log && answer == 0.0) {
        return answer;
    }
    let nm1 = n - 1;
    let mut w = d;
    let mut k = 0;
    if b > 1.0 {
        if y > 1e-4 {
            let r = (b - 1.0) * x / y - a;
            if r >= 1.0 {
                k = if r < nm1 as f64 { r as i32 } else { nm1 };
            }
        } else {
            k = nm1;
        }
        for i in 0..k {
            let l = i as f64;
            d *= (apb + l) / (ap1 + l) * x;
            w += d;
        }
    }
    for i in k..nm1 {
        let l = i as f64;
        d *= (apb + l) / (ap1 + l) * x;
        w += d;
        if d <= eps * w {
            break;
        }
    }
    if log {
        answer += w.ln();
    } else {
        answer *= w;
    }
    answer
}

/// The continued fraction of `I_x(a, b)` for `a, b > 1`, with `lambda = (a + b) y - b`.
fn bfrac(a: f64, b: f64, x: f64, y: f64, lambda: f64, eps: f64, log: bool) -> f64 {
    if !finite(lambda) {
        return f64::NAN;
    }
    let brc = brcomp(a, b, x, y, log);
    if brc.is_nan() {
        return f64::NAN;
    }
    if !log && brc == 0.0 {
        return 0.0;
    }
    let c = lambda + 1.0;
    let c0 = b / a;
    let c1 = 1.0 / a + 1.0;
    let yp1 = y + 1.0;
    let mut n = 0.0;
    let mut p = 1.0;
    let mut s = a + 1.0;
    let mut an = 0.0;
    let mut bn = 1.0;
    let mut anp1 = 1.0;
    let mut bnp1 = c / c1;
    let mut r = c1 / c;
    let mut r0;
    loop {
        n += 1.0;
        let mut t = n / a;
        let w = n * (b - n) * x;
        let mut e = a / s;
        let alpha = p * (p + c0) * e * e * (w * x);
        e = (t + 1.0) / (c1 + t + t);
        let beta = n + w / s + e * (c + n * yp1);
        p = t + 1.0;
        s += 2.0;
        t = alpha * an + beta * anp1;
        an = anp1;
        anp1 = t;
        t = alpha * bn + beta * bnp1;
        bn = bnp1;
        bnp1 = t;
        r0 = r;
        r = anp1 / bnp1;
        if (r - r0).abs() <= eps * r {
            break;
        }
        an /= bnp1;
        bn /= bnp1;
        anp1 = r;
        bnp1 = 1.0;
        if n >= 10000.0 {
            break;
        }
    }
    if n >= 10000.0 && (r - r0).abs() > eps * r {
        warning(format!(
            " bfrac(a={a}, b={b}, x={x}, y={y}, lambda={lambda}) did *not* converge (in 10000 steps)"
        ));
    }
    if log { brc + r.ln() } else { brc * r }
}

/// `x^a y^b / Beta(a, b)`.
fn brcomp(a: f64, b: f64, x: f64, y: f64, log: bool) -> f64 {
    const CONST: f64 = 0.398942280401433;
    if x == 0.0 || y == 0.0 {
        return d0(log);
    }
    let a0 = a.min(b);
    if a0 < 8.0 {
        let (lnx, lny) = if x <= 0.375 {
            (x.ln(), alnrel(-x))
        } else if y > 0.375 {
            (x.ln(), y.ln())
        } else {
            (alnrel(-y), y.ln())
        };
        let mut z = a * lnx + b * lny;
        if a0 >= 1.0 {
            z -= betaln(a, b);
            return d_exp(z, log);
        }
        let mut b0 = a.max(b);
        if b0 >= 8.0 {
            let u = gamln1(a0) + algdiv(a0, b0);
            return if log {
                a0.ln() + (z - u)
            } else {
                a0 * (z - u).exp()
            };
        }
        if b0 <= 1.0 {
            let e_z = d_exp(z, log);
            if !log && e_z == 0.0 {
                return 0.0;
            }
            let apb = a + b;
            let z = if apb > 1.0 {
                (gam1(apb - 1.0) + 1.0) / apb
            } else {
                gam1(apb) + 1.0
            };
            let c = (gam1(a) + 1.0) * (gam1(b) + 1.0) / z;
            return if log {
                e_z + (a0 * c).ln() - (a0 / b0).ln_1p()
            } else {
                e_z * (a0 * c) / (a0 / b0 + 1.0)
            };
        }
        let mut u = gamln1(a0);
        let n = (b0 - 1.0) as i32;
        if n >= 1 {
            let mut c = 1.0;
            for _ in 1..=n {
                b0 -= 1.0;
                c *= b0 / (a0 + b0);
            }
            u += c.ln();
        }
        z -= u;
        b0 -= 1.0;
        let apb = a0 + b0;
        let t = if apb > 1.0 {
            (gam1(apb - 1.0) + 1.0) / apb
        } else {
            gam1(apb) + 1.0
        };
        if log {
            a0.ln() + z + gam1(b0).ln_1p() - t.ln()
        } else {
            a0 * z.exp() * (gam1(b0) + 1.0) / t
        }
    } else {
        let (x0, y0, lambda);
        if a <= b {
            let h = a / b;
            x0 = h / (h + 1.0);
            y0 = 1.0 / (h + 1.0);
            lambda = a - (a + b) * x;
        } else {
            let h = b / a;
            x0 = 1.0 / (h + 1.0);
            y0 = h / (h + 1.0);
            lambda = (a + b) * y - b;
        }
        let mut e = -lambda / a;
        let u = if e.abs() > 0.6 {
            e - (x / x0).ln()
        } else {
            rlog1(e)
        };
        e = lambda / b;
        let v = if e.abs() <= 0.6 {
            rlog1(e)
        } else {
            e - (y / y0).ln()
        };
        let z = if log {
            -(a * u + b * v)
        } else {
            (-(a * u + b * v)).exp()
        };
        if log {
            -M_LN_SQRT_2PI + 0.5 * (b * x0).ln() + z - bcorr(a, b)
        } else {
            CONST * (b * x0).sqrt() * z * (-bcorr(a, b)).exp()
        }
    }
}

/// `exp(mu) x^a y^b / Beta(a, b)`.
fn brcmp1(mu: i32, a: f64, b: f64, x: f64, y: f64, log: bool) -> f64 {
    const CONST: f64 = 0.398942280401433;
    let a0 = a.min(b);
    if a0 < 8.0 {
        let (lnx, lny) = if x <= 0.375 {
            (x.ln(), alnrel(-x))
        } else if y > 0.375 {
            (x.ln(), y.ln())
        } else {
            (alnrel(-y), y.ln())
        };
        let mut z = a * lnx + b * lny;
        if a0 >= 1.0 {
            z -= betaln(a, b);
            return esum(mu, z, log);
        }
        let mut b0 = a.max(b);
        if b0 >= 8.0 {
            let u = gamln1(a0) + algdiv(a0, b0);
            return if log {
                a0.ln() + esum(mu, z - u, true)
            } else {
                a0 * esum(mu, z - u, false)
            };
        } else if b0 <= 1.0 {
            let answer = esum(mu, z, log);
            if answer == if log { f64::NEG_INFINITY } else { 0.0 } {
                return answer;
            }
            let apb = a + b;
            let z = if apb > 1.0 {
                (gam1(apb - 1.0) + 1.0) / apb
            } else {
                gam1(apb) + 1.0
            };
            let c = if log {
                gam1(a).ln_1p() + gam1(b).ln_1p() - z.ln()
            } else {
                (gam1(a) + 1.0) * (gam1(b) + 1.0) / z
            };
            return if log {
                answer + a0.ln() + c - (a0 / b0).ln_1p()
            } else {
                answer * (a0 * c) / (a0 / b0 + 1.0)
            };
        }
        let mut u = gamln1(a0);
        let n = (b0 - 1.0) as i32;
        if n >= 1 {
            let mut c = 1.0;
            for _ in 1..=n {
                b0 -= 1.0;
                c *= b0 / (a0 + b0);
            }
            u += c.ln();
        }
        z -= u;
        b0 -= 1.0;
        let apb = a0 + b0;
        let t = if apb > 1.0 {
            (gam1(apb - 1.0) + 1.0) / apb
        } else {
            gam1(apb) + 1.0
        };
        if log {
            a0.ln() + esum(mu, z, true) + gam1(b0).ln_1p() - t.ln()
        } else {
            a0 * esum(mu, z, false) * (gam1(b0) + 1.0) / t
        }
    } else {
        let (x0, y0, lambda);
        if a > b {
            let h = b / a;
            x0 = 1.0 / (h + 1.0);
            y0 = h / (h + 1.0);
            lambda = (a + b) * y - b;
        } else {
            let h = a / b;
            x0 = h / (h + 1.0);
            y0 = 1.0 / (h + 1.0);
            lambda = a - (a + b) * x;
        }
        let lx0 = -(b / a).ln_1p();
        let mut e = -lambda / a;
        let u = if e.abs() > 0.6 {
            e - (x / x0).ln()
        } else {
            rlog1(e)
        };
        e = lambda / b;
        let v = if e.abs() > 0.6 {
            e - (y / y0).ln()
        } else {
            rlog1(e)
        };
        let z = esum(mu, -(a * u + b * v), log);
        if log {
            CONST.ln() + (b.ln() + lx0) / 2.0 + z - bcorr(a, b)
        } else {
            CONST * (b * x0).sqrt() * z * (-bcorr(a, b)).exp()
        }
    }
}

/// The asymptotic expansion of `I_x(a, b)` for large `a` and `b <= 1`, added to `w` (on the
/// log scale when `log_w`).
#[allow(clippy::too_many_arguments)]
fn bgrat(a: f64, b: f64, x: f64, y: f64, w: &mut f64, eps: f64, ierr: &mut i32, log_w: bool) {
    const N_TERMS: usize = 30;
    let mut c = [0.0; N_TERMS];
    let mut d = [0.0; N_TERMS];
    let bm1 = b - 0.5 - 0.5;
    let nu = a + bm1 * 0.5;
    let lnx = if y > 0.375 { x.ln() } else { alnrel(-y) };
    let z = -nu * lnx;
    if b * z == 0.0 {
        warning(format!(
            "bgrat(a={a}, b={b}, x={x}, y={y}): z={z}, b*z == 0 underflow, hence inaccurate pbeta()"
        ));
        *ierr = 1;
        return;
    }
    let log_r = b.ln() + gam1(b).ln_1p() + b * z.ln() + nu * lnx;
    let log_u = log_r - (algdiv(b, a) + b * nu.ln());
    let u = log_u.exp();
    if log_u == f64::NEG_INFINITY {
        *ierr = 2;
        return;
    }
    let u_0 = u == 0.0;
    let l = if log_w {
        if *w == f64::NEG_INFINITY {
            0.0
        } else {
            (*w - log_u).exp()
        }
    } else if *w == 0.0 {
        0.0
    } else {
        (w.ln() - log_u).exp()
    };
    let q_r = grat_r(b, z, log_r, eps);
    let v = 0.25 / (nu * nu);
    let t2 = lnx * 0.25 * lnx;
    let mut j = q_r;
    let mut sum = j;
    let mut t = 1.0;
    let mut cn = 1.0;
    let mut n2 = 0.0;
    for n in 1..=N_TERMS {
        let bp2n = b + n2;
        j = (bp2n * (bp2n + 1.0) * j + (z + bp2n + 1.0) * t) * v;
        n2 += 2.0;
        t *= t2;
        cn /= n2 * (n2 + 1.0);
        let nm1 = n - 1;
        c[nm1] = cn;
        let mut s = 0.0;
        if n > 1 {
            let mut coef = b - n as f64;
            for i in 1..=nm1 {
                s += coef * c[i - 1] * d[nm1 - i];
                coef += b;
            }
        }
        d[nm1] = bm1 * cn + s / n as f64;
        let dj = d[nm1] * j;
        sum += dj;
        if sum <= 0.0 {
            *ierr = 3;
            return;
        }
        if dj.abs() <= eps * (sum + l) {
            *ierr = 0;
            break;
        } else if n == N_TERMS {
            *ierr = 4;
            warning(format!(
                "bgrat(a={}, b={}, x={}) *no* convergence: NOTIFY R-core!\n dj={}, rel.err={}",
                format_g(a),
                format_g(b),
                format_g(x),
                format_g(dj),
                format_g(dj.abs() / (sum + l))
            ));
        }
    }
    if log_w {
        *w = logspace_add(*w, log_u + sum.ln());
    } else {
        *w += if u_0 {
            (log_u + sum.ln()).exp()
        } else {
            u * sum
        };
    }
}

/// `Q(a, x) / r` with `r = exp(-x) x^a / gamma(a)` and `a <= 1`.
fn grat_r(a: f64, x: f64, log_r: f64, eps: f64) -> f64 {
    if a * x == 0.0 {
        if x <= a {
            return (-log_r).exp();
        }
        return 0.0;
    }
    if a == 0.5 {
        if x < 0.25 {
            let p = erf_toms(x.sqrt());
            return (0.5 - p + 0.5) * (-log_r).exp();
        }
        let sx = x.sqrt();
        return erfc1(1, sx) / sx * M_SQRT_PI;
    }
    if x < 1.1 {
        let mut an = 3.0;
        let mut c = x;
        let mut sum = x / (a + 3.0);
        let tol = eps * 0.1 / (a + 1.0);
        loop {
            an += 1.0;
            c *= -(x / an);
            let t = c / (a + an);
            sum += t;
            if !(t.abs() > tol) {
                break;
            }
        }
        let j = a * x * ((sum / 6.0 - 0.5 / (a + 2.0)) * x + 1.0 / (a + 1.0));
        let z = a * x.ln();
        let h = gam1(a);
        let g = h + 1.0;
        if (x >= 0.25 && a < x / 2.59) || z > -0.13394 {
            let l = rexpm1(z);
            let q = ((l + 0.5 + 0.5) * j - l) * g - h;
            if q <= 0.0 { 0.0 } else { q * (-log_r).exp() }
        } else {
            let p = z.exp() * g * (0.5 - j + 0.5);
            (0.5 - p + 0.5) * (-log_r).exp()
        }
    } else {
        let mut a2n_1 = 1.0;
        let mut a2n = 1.0;
        let mut b2n_1 = x;
        let mut b2n = x + (1.0 - a);
        let mut c = 1.0;
        loop {
            a2n_1 = x * a2n + c * a2n_1;
            b2n_1 = x * b2n + c * b2n_1;
            let am0 = a2n_1 / b2n_1;
            c += 1.0;
            let c_a = c - a;
            a2n = a2n_1 + c_a * a2n;
            b2n = b2n_1 + c_a * b2n;
            let an0 = a2n / b2n;
            if !((an0 - am0).abs() >= eps * an0) {
                return an0;
            }
        }
    }
}

/// The asymptotic expansion of `I_x(a, b)` for large `a` and `b`, with
/// `lambda = (a + b) y - b >= 0`.
fn basym(a: f64, b: f64, lambda: f64, eps: f64, log: bool) -> f64 {
    const NUM_IT: usize = 20;
    const E0: f64 = 1.12837916709551;
    const E1: f64 = 0.353553390593274;
    const LN_E0: f64 = 0.120782237635245;
    let mut a0 = [0.0; NUM_IT + 1];
    let mut b0 = [0.0; NUM_IT + 1];
    let mut c = [0.0; NUM_IT + 1];
    let mut d = [0.0; NUM_IT + 1];
    let f = a * rlog1(-lambda / a) + b * rlog1(lambda / b);
    let t = if log {
        -f
    } else {
        let t = (-f).exp();
        if t == 0.0 {
            return 0.0;
        }
        t
    };
    let z0 = f.sqrt();
    let z = z0 / E1 * 0.5;
    let z2 = f + f;
    let (h, r0, r1, w0);
    if a < b {
        h = a / b;
        r0 = 1.0 / (h + 1.0);
        r1 = (b - a) / b;
        w0 = 1.0 / (a * (h + 1.0)).sqrt();
    } else {
        h = b / a;
        r0 = 1.0 / (h + 1.0);
        r1 = (b - a) / a;
        w0 = 1.0 / (b * (h + 1.0)).sqrt();
    }
    a0[0] = r1 * 0.66666666666666663;
    c[0] = a0[0] * -0.5;
    d[0] = -c[0];
    let mut j0 = 0.5 / E0 * erfc1(1, z0);
    let mut j1 = E1;
    let mut sum = j0 + d[0] * w0 * j1;
    let mut s = 1.0;
    let h2 = h * h;
    let mut hn = 1.0;
    let mut w = w0;
    let mut znm1 = z;
    let mut zn = z2;
    let mut n = 2;
    while n <= NUM_IT {
        hn *= h2;
        a0[n - 1] = r0 * 2.0 * (h * hn + 1.0) / (n as f64 + 2.0);
        let np1 = n + 1;
        s += hn;
        a0[np1 - 1] = r1 * 2.0 * s / (n as f64 + 3.0);
        for i in n..=np1 {
            let r = (i as f64 + 1.0) * -0.5;
            b0[0] = r * a0[0];
            for m in 2..=i {
                let mut bsum = 0.0;
                for j in 1..m {
                    let mmj = m - j;
                    bsum += (j as f64 * r - mmj as f64) * a0[j - 1] * b0[mmj - 1];
                }
                b0[m - 1] = r * a0[m - 1] + bsum / m as f64;
            }
            c[i - 1] = b0[i - 1] / (i as f64 + 1.0);
            let mut dsum = 0.0;
            for j in 1..i {
                dsum += d[i - j - 1] * c[j - 1];
            }
            d[i - 1] = -(dsum + c[i - 1]);
        }
        j0 = E1 * znm1 + (n as f64 - 1.0) * j0;
        j1 = E1 * zn + n as f64 * j1;
        znm1 *= z2;
        zn *= z2;
        w *= w0;
        let t0 = d[n - 1] * w * j0;
        w *= w0;
        let t1 = d[np1 - 1] * w * j1;
        sum += t0 + t1;
        if t0.abs() + t1.abs() <= eps * sum {
            break;
        }
        n += 2;
    }
    if log {
        LN_E0 + t - bcorr(a, b) + sum.ln()
    } else {
        let u = (-bcorr(a, b)).exp();
        E0 * t * u * sum
    }
}

/// TOMS 708's bounds on `exp`: the largest `w` with finite `exp(w)` for `l == 0`, otherwise
/// the most negative `w` with nonzero `exp(w)`, each with a little room to spare.
fn exparg(l: i32) -> f64 {
    const LNB: f64 = 0.69314718055995;
    let m = if l == 0 {
        f64::MAX_EXP
    } else {
        f64::MIN_EXP - 1
    };
    m as f64 * LNB * 0.99999
}

/// `exp(mu + x)`.
fn esum(mu: i32, x: f64, log: bool) -> f64 {
    if log {
        return x + mu as f64;
    }
    let w;
    if x > 0.0 {
        if mu > 0 {
            return (mu as f64).exp() * x.exp();
        }
        w = mu as f64 + x;
        if w < 0.0 {
            return (mu as f64).exp() * x.exp();
        }
    } else {
        if mu < 0 {
            return (mu as f64).exp() * x.exp();
        }
        w = mu as f64 + x;
        if w > 0.0 {
            return (mu as f64).exp() * x.exp();
        }
    }
    w.exp()
}

/// `exp(x) - 1`.
fn rexpm1(x: f64) -> f64 {
    const P1: f64 = 9.14041914819518e-10;
    const P2: f64 = 0.0238082361044469;
    const Q1: f64 = -0.499999999085958;
    const Q2: f64 = 0.107141568980644;
    const Q3: f64 = -0.0119041179760821;
    const Q4: f64 = 5.95130811860248e-4;
    if x.abs() <= 0.15 {
        x * (((P2 * x + P1) * x + 1.0) / ((((Q4 * x + Q3) * x + Q2) * x + Q1) * x + 1.0))
    } else {
        let w = x.exp();
        if x > 0.0 {
            w * (0.5 - 1.0 / w + 0.5)
        } else {
            w - 0.5 - 0.5
        }
    }
}

/// `log(1 + a)`.
fn alnrel(a: f64) -> f64 {
    const P1: f64 = -1.29418923021993;
    const P2: f64 = 0.405303492862024;
    const P3: f64 = -0.0178874546012214;
    const Q1: f64 = -1.62752256355323;
    const Q2: f64 = 0.747811014037616;
    const Q3: f64 = -0.0845104217945565;
    if a.abs() > 0.375 {
        return (1.0 + a).ln();
    }
    let t = a / (a + 2.0);
    let t2 = t * t;
    let w = (((P3 * t2 + P2) * t2 + P1) * t2 + 1.0) / (((Q3 * t2 + Q2) * t2 + Q1) * t2 + 1.0);
    t * 2.0 * w
}

/// `x - log(1 + x)`.
fn rlog1(x: f64) -> f64 {
    const A: f64 = 0.0566749439387324;
    const B: f64 = 0.0456512608815524;
    const P0: f64 = 0.333333333333333;
    const P1: f64 = -0.224696413112536;
    const P2: f64 = 0.00620886815375787;
    const Q1: f64 = -1.27408923933623;
    const Q2: f64 = 0.354508718369557;
    if !(-0.39..=0.57).contains(&x) {
        let w = x + 0.5 + 0.5;
        return x - w.ln();
    }
    let (h, w1) = if x < -0.18 {
        let h = (x + 0.3) / 0.7;
        (h, A - h * 0.3)
    } else if x > 0.18 {
        let h = x * 0.75 - 0.25;
        (h, B + h / 3.0)
    } else {
        (x, 0.0)
    };
    let r = h / (h + 2.0);
    let t = r * r;
    let w = ((P2 * t + P1) * t + P0) / ((Q2 * t + Q1) * t + 1.0);
    t * 2.0 * (1.0 / (1.0 - r) - r * w) + w1
}

const ERF_A: [f64; 5] = [
    7.7105849500132e-5,
    -0.00133733772997339,
    0.0323076579225834,
    0.0479137145607681,
    0.128379167095513,
];
const ERF_B: [f64; 3] = [0.00301048631703895, 0.0538971687740286, 0.375795757275549];
const ERF_P: [f64; 8] = [
    -1.36864857382717e-7,
    0.564195517478974,
    7.21175825088309,
    43.1622272220567,
    152.98928504694,
    339.320816734344,
    451.918953711873,
    300.459261020162,
];
const ERF_Q: [f64; 8] = [
    1.0,
    12.7827273196294,
    77.0001529352295,
    277.585444743988,
    638.980264465631,
    931.35409485061,
    790.950925327898,
    300.459260956983,
];
const ERF_R: [f64; 5] = [
    2.10144126479064,
    26.2370141675169,
    21.3688200555087,
    4.6580782871847,
    0.282094791773523,
];
const ERF_S: [f64; 4] = [
    94.153775055546,
    187.11481179959,
    99.0191814623914,
    18.0124575948747,
];
const ERF_C: f64 = 0.564189583547756;

fn erf_middle(ax: f64) -> f64 {
    let p = &ERF_P;
    let q = &ERF_Q;
    let top = ((((((p[0] * ax + p[1]) * ax + p[2]) * ax + p[3]) * ax + p[4]) * ax + p[5]) * ax
        + p[6])
        * ax
        + p[7];
    let bot = ((((((q[0] * ax + q[1]) * ax + q[2]) * ax + q[3]) * ax + q[4]) * ax + q[5]) * ax
        + q[6])
        * ax
        + q[7];
    top / bot
}

/// The error function, TOMS 708's rational approximations.
fn erf_toms(x: f64) -> f64 {
    let ax = x.abs();
    if ax <= 0.5 {
        let t = x * x;
        let top = (((ERF_A[0] * t + ERF_A[1]) * t + ERF_A[2]) * t + ERF_A[3]) * t + ERF_A[4] + 1.0;
        let bot = ((ERF_B[0] * t + ERF_B[1]) * t + ERF_B[2]) * t + 1.0;
        return x * (top / bot);
    }
    if ax <= 4.0 {
        let r = 0.5 - (-x * x).exp() * erf_middle(ax) + 0.5;
        return if x < 0.0 { -r } else { r };
    }
    if ax >= 5.8 {
        return if x > 0.0 { 1.0 } else { -1.0 };
    }
    let x2 = x * x;
    let mut t = 1.0 / x2;
    let top = (((ERF_R[0] * t + ERF_R[1]) * t + ERF_R[2]) * t + ERF_R[3]) * t + ERF_R[4];
    let bot = (((ERF_S[0] * t + ERF_S[1]) * t + ERF_S[2]) * t + ERF_S[3]) * t + 1.0;
    t = (ERF_C - top / (x2 * bot)) / ax;
    let r = 0.5 - (-x2).exp() * t + 0.5;
    if x < 0.0 { -r } else { r }
}

/// `erfc(x)` for `ind == 0`, otherwise `exp(x^2) erfc(x)`.
fn erfc1(ind: i32, x: f64) -> f64 {
    let ax = x.abs();
    if ax <= 0.5 {
        let t = x * x;
        let top = (((ERF_A[0] * t + ERF_A[1]) * t + ERF_A[2]) * t + ERF_A[3]) * t + ERF_A[4] + 1.0;
        let bot = ((ERF_B[0] * t + ERF_B[1]) * t + ERF_B[2]) * t + 1.0;
        let mut value = 0.5 - x * (top / bot) + 0.5;
        if ind != 0 {
            value *= t.exp();
        }
        return value;
    }
    let mut value;
    if ax <= 4.0 {
        value = erf_middle(ax);
    } else {
        if x <= -5.6 {
            return if ind != 0 { (x * x).exp() * 2.0 } else { 2.0 };
        }
        if ind == 0 && (x > 100.0 || x * x > -exparg(1)) {
            return 0.0;
        }
        let t = 1.0 / (x * x);
        let top = (((ERF_R[0] * t + ERF_R[1]) * t + ERF_R[2]) * t + ERF_R[3]) * t + ERF_R[4];
        let bot = (((ERF_S[0] * t + ERF_S[1]) * t + ERF_S[2]) * t + ERF_S[3]) * t + 1.0;
        value = (ERF_C - t * top / bot) / ax;
    }
    if ind != 0 {
        if x < 0.0 {
            value = (x * x).exp() * 2.0 - value;
        }
    } else {
        let w = x * x;
        let t = w;
        let e = w - t;
        value *= (0.5 - e + 0.5) * (-t).exp();
        if x < 0.0 {
            value = 2.0 - value;
        }
    }
    value
}

/// `1/gamma(a + 1) - 1` for `-0.5 <= a <= 1.5`.
fn gam1(a: f64) -> f64 {
    let d = a - 0.5;
    let t = if d > 0.0 { d - 0.5 } else { a };
    if t < 0.0 {
        const R: [f64; 9] = [
            -0.422784335098468,
            -0.771330383816272,
            -0.244757765222226,
            0.118378989872749,
            9.30357293360349e-4,
            -0.0118290993445146,
            0.00223047661158249,
            2.66505979058923e-4,
            -1.32674909766242e-4,
        ];
        const S1: f64 = 0.273076135303957;
        const S2: f64 = 0.0559398236957378;
        let top = (((((((R[8] * t + R[7]) * t + R[6]) * t + R[5]) * t + R[4]) * t + R[3]) * t
            + R[2])
            * t
            + R[1])
            * t
            + R[0];
        let bot = (S2 * t + S1) * t + 1.0;
        let w = top / bot;
        if d > 0.0 {
            t * w / a
        } else {
            a * (w + 0.5 + 0.5)
        }
    } else if t == 0.0 {
        0.0
    } else {
        const P: [f64; 7] = [
            0.577215664901533,
            -0.409078193005776,
            -0.230975380857675,
            0.0597275330452234,
            0.0076696818164949,
            -0.00514889771323592,
            5.89597428611429e-4,
        ];
        const Q: [f64; 5] = [
            1.0,
            0.427569613095214,
            0.158451672430138,
            0.0261132021441447,
            0.00423244297896961,
        ];
        let top = (((((P[6] * t + P[5]) * t + P[4]) * t + P[3]) * t + P[2]) * t + P[1]) * t + P[0];
        let bot = (((Q[4] * t + Q[3]) * t + Q[2]) * t + Q[1]) * t + 1.0;
        let w = top / bot;
        if d > 0.0 {
            t / a * (w - 0.5 - 0.5)
        } else {
            a * w
        }
    }
}

/// `log(gamma(1 + a))` for `-0.2 <= a <= 1.25`.
fn gamln1(a: f64) -> f64 {
    if a < 0.6 {
        const P: [f64; 7] = [
            0.577215664901533,
            0.844203922187225,
            -0.168860593646662,
            -0.780427615533591,
            -0.402055799310489,
            -0.0673562214325671,
            -0.00271935708322958,
        ];
        const Q: [f64; 6] = [
            2.88743195473681,
            3.12755088914843,
            1.56875193295039,
            0.361951990101499,
            0.0325038868253937,
            6.67465618796164e-4,
        ];
        let w = ((((((P[6] * a + P[5]) * a + P[4]) * a + P[3]) * a + P[2]) * a + P[1]) * a + P[0])
            / ((((((Q[5] * a + Q[4]) * a + Q[3]) * a + Q[2]) * a + Q[1]) * a + Q[0]) * a + 1.0);
        -a * w
    } else {
        const R: [f64; 6] = [
            0.422784335098467,
            0.848044614534529,
            0.565221050691933,
            0.156513060486551,
            0.017050248402265,
            4.97958207639485e-4,
        ];
        const S: [f64; 5] = [
            1.24313399877507,
            0.548042109832463,
            0.10155218743983,
            0.00713309612391,
            1.16165475989616e-4,
        ];
        let x = a - 0.5 - 0.5;
        let w = (((((R[5] * x + R[4]) * x + R[3]) * x + R[2]) * x + R[1]) * x + R[0])
            / (((((S[4] * x + S[3]) * x + S[2]) * x + S[1]) * x + S[0]) * x + 1.0);
        x * w
    }
}

/// The digamma function, Cody, Strecok and Thacher's rational approximations as TOMS 708
/// uses them; zero signals an argument it cannot handle.
fn psi(x: f64) -> f64 {
    const PIOV4: f64 = 0.785398163397448;
    const DX0: f64 = 1.461632144968362341262659542325721325;
    const P1: [f64; 7] = [
        0.0089538502298197,
        4.77762828042627,
        142.441585084029,
        1186.45200713425,
        3633.51846806499,
        4138.10161269013,
        1305.60269827897,
    ];
    const Q1: [f64; 6] = [
        44.8452573429826,
        520.752771467162,
        2210.0079924783,
        3641.27349079381,
        1908.310765963,
        6.91091682714533e-6,
    ];
    const P2: [f64; 4] = [
        -2.12940445131011,
        -7.01677227766759,
        -4.48616543918019,
        -0.648157123766197,
    ];
    const Q2: [f64; 4] = [
        32.2703493791143,
        89.2920700481861,
        54.6117738103215,
        7.77788548522962,
    ];
    let xmax1 = (i32::MAX as f64).min(1.0 / DBL_EPSILON);
    let xsmall = 1e-9;
    let mut x = x;
    let mut aug = 0.0;
    if x < 0.5 {
        if x.abs() <= xsmall {
            if x == 0.0 {
                return 0.0;
            }
            aug = -1.0 / x;
        } else {
            let mut w = -x;
            let mut sgn = PIOV4;
            if w <= 0.0 {
                w = -w;
                sgn = -sgn;
            }
            if w >= xmax1 {
                return 0.0;
            }
            let mut nq = w as i32;
            w -= nq as f64;
            nq = (w * 4.0) as i32;
            w = (w - nq as f64 * 0.25) * 4.0;
            let mut n = nq / 2;
            if n + n != nq {
                w = 1.0 - w;
            }
            let z = PIOV4 * w;
            let mut m = n / 2;
            if m + m != n {
                sgn = -sgn;
            }
            n = (nq + 1) / 2;
            m = n / 2;
            m += m;
            if m == n {
                if z == 0.0 {
                    return 0.0;
                }
                aug = sgn * (z.cos() / z.sin() * 4.0);
            } else {
                aug = sgn * (z.sin() / z.cos() * 4.0);
            }
        }
        x = 1.0 - x;
    }
    if x <= 3.0 {
        let mut den = x;
        let mut upper = P1[0] * x;
        for i in 1..=5 {
            den = (den + Q1[i - 1]) * x;
            upper = (upper + P1[i]) * x;
        }
        den = (upper + P1[6]) / (den + Q1[5]);
        let xmx0 = x - DX0;
        return den * xmx0 + aug;
    }
    if x < xmax1 {
        let w = 1.0 / (x * x);
        let mut den = w;
        let mut upper = P2[0] * w;
        for i in 1..=3 {
            den = (den + Q2[i - 1]) * w;
            upper = (upper + P2[i]) * w;
        }
        aug += upper / (den + Q2[3]) - 0.5 / x;
    }
    aug + x.ln()
}

/// `log(Beta(a0, b0))`.
fn betaln(a0: f64, b0: f64) -> f64 {
    const E: f64 = 0.918938533204673;
    let mut a = a0.min(b0);
    let mut b = a0.max(b0);
    if a >= 8.0 {
        let w = bcorr(a, b);
        let h = a / b;
        let c = h / (h + 1.0);
        let u = -(a - 0.5) * c.ln();
        let v = b * alnrel(h);
        return if u > v {
            b.ln() * -0.5 + E + w - v - u
        } else {
            b.ln() * -0.5 + E + w - u - v
        };
    }
    if a < 1.0 {
        return if b < 8.0 {
            gamln(a) + (gamln(b) - gamln(a + b))
        } else {
            gamln(a) + algdiv(a, b)
        };
    }
    let w;
    if a < 2.0 {
        if b <= 2.0 {
            return gamln(a) + gamln(b) - gsumln(a, b);
        }
        if b >= 8.0 {
            return gamln(a) + algdiv(a, b);
        }
        w = 0.0;
    } else if b <= 1e3 {
        let n = (a - 1.0) as i32;
        let mut product = 1.0;
        for _ in 1..=n {
            a -= 1.0;
            let h = a / b;
            product *= h / (h + 1.0);
        }
        w = product.ln();
        if b >= 8.0 {
            return w + gamln(a) + algdiv(a, b);
        }
    } else {
        let n = (a - 1.0) as i32;
        let mut product = 1.0;
        for _ in 1..=n {
            a -= 1.0;
            product *= a / (a / b + 1.0);
        }
        return product.ln() - n as f64 * b.ln() + (gamln(a) + algdiv(a, b));
    }
    let n = (b - 1.0) as i32;
    let mut z = 1.0;
    for _ in 1..=n {
        b -= 1.0;
        z *= b / (a + b);
    }
    w + z.ln() + (gamln(a) + (gamln(b) - gsumln(a, b)))
}

/// `log(gamma(a + b))` for `1 <= a, b <= 2`.
fn gsumln(a: f64, b: f64) -> f64 {
    let x = a + b - 2.0;
    if x <= 0.25 {
        gamln1(x + 1.0)
    } else if x <= 1.25 {
        gamln1(x) + alnrel(x)
    } else {
        gamln1(x - 1.0) + (x * (x + 1.0)).ln()
    }
}

const DEL_C: [f64; 6] = [
    0.0833333333333333,
    -0.00277777777760991,
    7.9365066682539e-4,
    -5.9520293135187e-4,
    8.37308034031215e-4,
    -0.00165322962780713,
];

/// `del(a0) + del(b0) - del(a0 + b0)` where `del` is Stirling's remainder, for `a0, b0 >= 8`.
fn bcorr(a0: f64, b0: f64) -> f64 {
    let c = &DEL_C;
    let a = a0.min(b0);
    let b = a0.max(b0);
    let h = a / b;
    let cc = h / (h + 1.0);
    let x = 1.0 / (h + 1.0);
    let x2 = x * x;
    let s3 = x + x2 + 1.0;
    let s5 = x + x2 * s3 + 1.0;
    let s7 = x + x2 * s5 + 1.0;
    let s9 = x + x2 * s7 + 1.0;
    let s11 = x + x2 * s9 + 1.0;
    let mut t = 1.0 / b;
    t *= t;
    let mut w = ((((c[5] * s11 * t + c[4] * s9) * t + c[3] * s7) * t + c[2] * s5) * t + c[1] * s3)
        * t
        + c[0];
    w *= cc / b;
    t = 1.0 / a;
    t *= t;
    (((((c[5] * t + c[4]) * t + c[3]) * t + c[2]) * t + c[1]) * t + c[0]) / a + w
}

/// `log(gamma(b) / gamma(a + b))` for `b >= 8`.
fn algdiv(a: f64, b: f64) -> f64 {
    let co = &DEL_C;
    let (h, c, x, d);
    if a > b {
        h = b / a;
        c = 1.0 / (h + 1.0);
        x = h / (h + 1.0);
        d = a + (b - 0.5);
    } else {
        h = a / b;
        c = h / (h + 1.0);
        x = 1.0 / (h + 1.0);
        d = b + (a - 0.5);
    }
    let x2 = x * x;
    let s3 = x + x2 + 1.0;
    let s5 = x + x2 * s3 + 1.0;
    let s7 = x + x2 * s5 + 1.0;
    let s9 = x + x2 * s7 + 1.0;
    let s11 = x + x2 * s9 + 1.0;
    let t = 1.0 / (b * b);
    let mut w =
        ((((co[5] * s11 * t + co[4] * s9) * t + co[3] * s7) * t + co[2] * s5) * t + co[1] * s3) * t
            + co[0];
    w *= c / b;
    let u = d * alnrel(a / b);
    let v = a * (b.ln() - 1.0);
    if u > v { w - v - u } else { w - u - v }
}

/// `log(gamma(a))` for positive `a`.
fn gamln(a: f64) -> f64 {
    const D: f64 = 0.418938533204673;
    let c = &DEL_C;
    if a <= 0.8 {
        gamln1(a) - a.ln()
    } else if a <= 2.25 {
        gamln1(a - 0.5 - 0.5)
    } else if a < 10.0 {
        let n = (a - 1.25) as i32;
        let mut t = a;
        let mut w = 1.0;
        for _ in 1..=n {
            t -= 1.0;
            w *= t;
        }
        gamln1(t - 1.0) + w.ln()
    } else {
        let t = 1.0 / (a * a);
        let w = (((((c[5] * t + c[4]) * t + c[3]) * t + c[2]) * t + c[1]) * t + c[0]) / a;
        D + w + (a - 0.5) * (a.ln() - 1.0)
    }
}

/// The beta quantile. AS 109's starting value, then Newton steps on `log x` against the
/// smaller tail's log probability, safeguarded by bisection, so the answer is the root of
/// `pbeta` to the last bits wherever `pbeta` itself is accurate.
pub fn qbeta(alpha: f64, p: f64, q: f64, lower: bool, log: bool) -> f64 {
    // The search probes pbeta far from the quantile, where its series may fail to converge
    // without that touching the answer, so those warnings are not the caller's.
    let raised = WARNINGS.with(|warnings| warnings.borrow().len());
    let quantile = qbeta_search(alpha, p, q, lower, log);
    WARNINGS.with(|warnings| warnings.borrow_mut().truncate(raised));
    quantile
}

fn qbeta_search(alpha: f64, p: f64, q: f64, lower: bool, log: bool) -> f64 {
    if alpha.is_nan() || p.is_nan() || q.is_nan() {
        return alpha + p + q;
    }
    if p < 0.0 || q < 0.0 {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(alpha, lower, log, 0.0, 1.0) {
        return boundary;
    }
    if p == 0.0 || q == 0.0 || !finite(p) || !finite(q) {
        if p == 0.0 && q == 0.0 {
            return if alpha < d_val(0.5, log) {
                if lower { 0.0 } else { 1.0 }
            } else if alpha > d_val(0.5, log) {
                if lower { 1.0 } else { 0.0 }
            } else {
                0.5
            };
        }
        // A vanishing first shape (relative to the second) puts all the mass at 0, and a
        // vanishing second shape puts it at 1.
        if p == 0.0 || q / p == f64::INFINITY {
            return 0.0;
        }
        if q == 0.0 || p / q == f64::INFINITY {
            return 1.0;
        }
        return 0.5;
    }
    // The log probability of each tail, and which end of [0, 1] the quantile lies nearer:
    // solving for the distance to that end keeps its relative precision.
    let log_lower = if log {
        if lower { alpha } else { log1_exp(alpha) }
    } else if lower {
        alpha.ln()
    } else {
        (-alpha).ln_1p()
    };
    let log_upper = if log {
        if lower { log1_exp(alpha) } else { alpha }
    } else if lower {
        (-alpha).ln_1p()
    } else {
        alpha.ln()
    };
    // Solve for t, the distance from the quantile to the nearer end of [0, 1], so that a
    // quantile near either end keeps its relative precision. When the quantile lies above
    // 1/2, t = 1 - x follows Beta(q, p). The equation is on the log scale of whichever tail
    // probability is smaller, the one the arguments represent accurately.
    let log_half = pbeta(0.5, p, q, true, true);
    if log_lower == log_half {
        return 0.5;
    }
    let near_zero = log_lower < log_half;
    let (a, b) = if near_zero { (p, q) } else { (q, p) };
    let use_lower = log_lower <= log_upper;
    let (target, t_lower) = match (near_zero, use_lower) {
        (true, true) => (log_lower, true),
        (true, false) => (log_upper, false),
        (false, true) => (log_lower, false),
        (false, false) => (log_upper, true),
    };
    let t_lower_log = if near_zero { log_lower } else { log_upper };
    // h(t) increases with t: the lower tail's log probability, or minus the upper tail's.
    let direction = if t_lower { 1.0 } else { -1.0 };
    // Newton's method on log(t), stepping as t * exp(du) so tiny quantiles keep their relative
    // precision, inside a bracket that falls back to bisection when a step leaves it.
    let mut t = qbeta_start(t_lower_log, a, b);
    if !(t > 0.0 && t < 0.5) {
        t = 0.25;
    }
    let lbeta_ab = lbeta(a, b);
    if !lbeta_ab.is_finite() || !(a + b).is_finite() {
        // The parameters are beyond what the density can be evaluated at, as in GNU R.
        return f64::NAN;
    }
    let mut lo = 0.0_f64;
    let mut hi = 0.5_f64;
    for _ in 0..1000 {
        let value = pbeta(t, a, b, t_lower, true);
        if value.is_nan() {
            // pbeta's asymptotic expansion fails for an extreme second shape far from the
            // quantile; the quantile lies nearer 0, where it converges.
            hi = t;
            t = if lo == 0.0 {
                hi * 1e-3
            } else {
                (lo * hi).sqrt()
            };
            continue;
        }
        let difference = direction * (value - target);
        if difference == 0.0 {
            break;
        }
        if difference > 0.0 {
            hi = t;
        } else {
            lo = t;
        }
        // d/du log P(e^u) = t f(t) / P(t), and the same with Q for the upper tail.
        let log_density = (a - 1.0) * t.ln() + (b - 1.0) * (-t).ln_1p() - lbeta_ab;
        let step = -difference / (t.ln() + log_density - value).exp();
        let mut next = t * step.exp();
        if !(next > lo && next < hi) {
            next = if lo == 0.0 {
                hi * 1e-3
            } else if hi / lo > 4.0 {
                (lo * hi).sqrt()
            } else {
                0.5 * (lo + hi)
            };
        } else if step.abs() < 1e-15 {
            t = next;
            break;
        }
        if next == t {
            break;
        }
        t = next;
    }
    let quantile = if near_zero { t } else { 0.5 - t + 0.5 };
    // Newton's method converges in t, and turning a t near 1/2 into 1 - t rounds, so the
    // quantile can land a few doubles from the one whose probability is nearest the target.
    // A short walk over neighbouring doubles settles it. pbeta rounds to the same value over
    // runs of adjacent doubles, so the walk looks a few doubles past a flat stretch.
    const LOOKAHEAD: usize = 8;
    let miss = |x: f64| (pbeta(x, p, q, lower, log) - alpha).abs();
    let mut best = quantile;
    let mut best_miss = miss(best);
    // The log-scale solve resolves a probability only to |log p| ulps, which near 1e-60 is
    // about a hundred ulps of p itself, so a few Newton steps on the caller's own scale
    // recover the digits a small probability represents exactly.
    for _ in 0..8 {
        if best_miss == 0.0 || !(best > 0.0 && best < 1.0) {
            break;
        }
        let value = pbeta(best, p, q, lower, log);
        let density = dbeta(best, p, q, false);
        let slope = if lower { density } else { -density };
        let slope = if log { slope / value.exp() } else { slope };
        if !(slope.is_finite() && slope != 0.0) {
            break;
        }
        let next = best - (value - alpha) / slope;
        if !(next > 0.0 && next < 1.0) {
            break;
        }
        let next_miss = miss(next);
        if next_miss >= best_miss {
            break;
        }
        best = next;
        best_miss = next_miss;
    }
    // A quantile of exactly 0 or 1 is a tail probability below the double range, which no
    // interior double improves on.
    let directions: &[fn(f64) -> f64] = if quantile > 0.0 && quantile < 1.0 {
        &[f64::next_up, f64::next_down]
    } else {
        &[]
    };
    for step in directions {
        'walk: loop {
            let mut candidate = best;
            for _ in 0..LOOKAHEAD {
                candidate = step(candidate);
                if !(candidate > 0.0 && candidate < 1.0) {
                    break 'walk;
                }
                let candidate_miss = miss(candidate);
                if candidate_miss < best_miss {
                    best = candidate;
                    best_miss = candidate_miss;
                    continue 'walk;
                }
            }
            break;
        }
    }
    // GNU R never returns a subnormal beta quantile: it gives 0 or 2^-1024, whichever has
    // the probability nearer the target, and its own tests rely on that.
    if best < f64::MIN_POSITIVE {
        let floor = f64::MIN_POSITIVE / 4.0;
        best = if miss(floor) < miss(0.0) { floor } else { 0.0 };
    }
    best
}

/// AS 109's approximation of the lower-tail beta quantile at log probability `log_p`.
fn qbeta_start(log_p: f64, a: f64, b: f64) -> f64 {
    let p_ = log_p.exp();
    let logbeta = lbeta(a, b);
    // Hastings's approximation of the normal quantile of the upper tail 1 - p.
    let r = (-2.0 * log_p).sqrt();
    let y = r - (2.30753 + 0.27061 * r) / (1.0 + (0.99229 + 0.04481 * r) * r);
    if a > 1.0 && b > 1.0 {
        let r = (y * y - 3.0) / 6.0;
        let s = 1.0 / (a + a - 1.0);
        let t = 1.0 / (b + b - 1.0);
        let h = 2.0 / (s + t);
        let w = y * (h + r).sqrt() / h - (t - s) * (r + 5.0 / 6.0 - 2.0 / (3.0 * h));
        a / (a + b * (w + w).exp())
    } else {
        let r = b + b;
        let mut t = 1.0 / (9.0 * b);
        t = r * (1.0 - t + y * t.sqrt()).powi(3);
        if t <= 0.0 {
            1.0 - ((-p_).ln_1p() + b.ln() + logbeta).exp() / b
        } else {
            t = (4.0 * a + r - 2.0) / t;
            if t <= 1.0 {
                ((log_p + a.ln() + logbeta) / a).exp()
            } else {
                1.0 - 2.0 / (t + 1.0)
            }
        }
    }
}

// The binomial, negative binomial, geometric and hypergeometric distributions.

/// The binomial density by Loader's saddle point, with `q = 1 - p` passed separately.
pub fn dbinom_raw(x: f64, n: f64, p: f64, q: f64, log: bool) -> f64 {
    if p == 0.0 {
        return if x == 0.0 { d1(log) } else { d0(log) };
    }
    if q == 0.0 {
        return if x == n { d1(log) } else { d0(log) };
    }
    if x == 0.0 {
        if n == 0.0 {
            return d1(log);
        }
        let lc = if p < 0.1 {
            -bd0(n, n * q) - n * p
        } else {
            n * q.ln()
        };
        return d_exp(lc, log);
    }
    if x == n {
        let lc = if q < 0.1 {
            -bd0(n, n * p) - n * q
        } else {
            n * p.ln()
        };
        return d_exp(lc, log);
    }
    if x < 0.0 || x > n {
        return d0(log);
    }
    let lc = stirlerr(n) - stirlerr(x) - stirlerr(n - x) - bd0(x, n * p) - bd0(n - x, n * q);
    let lf = M_LN_2PI + x.ln() + (-x / n).ln_1p();
    d_exp(lc - 0.5 * lf, log)
}

pub fn dbinom(x: f64, n: f64, p: f64, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() || p.is_nan() {
        return x + n + p;
    }
    if !(0.0..=1.0).contains(&p) || negative_or_nonint(n) {
        return f64::NAN;
    }
    if nonint_warning(x) || x < 0.0 || !finite(x) {
        return d0(log);
    }
    dbinom_raw(forceint(x), forceint(n), p, 1.0 - p, log)
}

pub fn pbinom(x: f64, n: f64, p: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() || p.is_nan() {
        return x + n + p;
    }
    if !finite(n) || !finite(p) {
        return f64::NAN;
    }
    if nonint(n) {
        warning(format!("non-integer n = {n:.6}"));
        return f64::NAN;
    }
    let n = forceint(n);
    if n < 0.0 || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if x < 0.0 {
        return dt0(lower, log);
    }
    let x = (x + 1e-7).floor();
    if n <= x {
        return dt1(lower, log);
    }
    pbeta(p, x + 1.0, n - x, !lower, log)
}

/// The quantile of a distribution on the integers `0..=maximum`: the smallest `y` whose lower
/// tail `distribution(y, true, log)` reaches `p`, or for the upper tail the smallest `y` whose
/// upper tail `distribution(y, false, log)` has fallen to `p`.
///
/// The search starts from the Cornish-Fisher expansion `mu + sigma (z + gamma (z^2 - 1) / 6)`
/// about the normal quantile `z`, where `gamma` is the skewness (Cornish and Fisher (1937);
/// Johnson, Kemp and Kotz (2005), section 1.8). From there it doubles its step until it
/// passes the quantile, then halves the bracket. A probability given as the distribution
/// function at an integer may have been rounded on the way in, so `p` is relaxed by a few
/// units in the last place: that integer, not its successor, is the answer.
#[allow(clippy::too_many_arguments)]
fn discrete_quantile(
    p: f64,
    mu: f64,
    sigma: f64,
    gamma: f64,
    lower: bool,
    log: bool,
    maximum: f64,
    distribution: &dyn Fn(f64, bool, bool) -> f64,
) -> f64 {
    // The search accepts a value within 64 epsilon of the target probability, relative to
    // the nearer end of [0, 1]: near 1, adjacent CDF values differ by a few ulps of 1, and a
    // slack relative to p itself would return the quantile below. On the log scale the
    // slack is 16 epsilon of the log probability.
    let slack = if log {
        16.0 * DBL_EPSILON * p.abs()
    } else {
        64.0 * DBL_EPSILON * p.abs().min(1.0 - p)
    };
    let reached = |y: f64| {
        let value = distribution(y, lower, log);
        value.is_nan()
            || if lower {
                value >= p - slack
            } else {
                value < p + slack
            }
    };
    let normal = qnorm(p, 0.0, 1.0, lower, log);
    let start = (mu + sigma * (normal + gamma * (normal * normal - 1.0) / 6.0)).round();
    let start = if start.is_nan() { mu.round() } else { start };
    let start = start.clamp(0.0, maximum);

    // Gallop away from the start until the quantile lies in (below, above].
    let (mut below, mut above);
    let mut step = 1.0;
    if reached(start) {
        above = start;
        loop {
            if above == 0.0 {
                return 0.0;
            }
            let candidate = (above - step).max(0.0);
            if reached(candidate) {
                above = candidate;
                step *= 2.0;
            } else {
                below = candidate;
                break;
            }
        }
    } else {
        below = start;
        loop {
            if below >= maximum {
                return maximum;
            }
            let candidate = (below + step).min(maximum);
            if reached(candidate) {
                above = candidate;
                break;
            }
            below = candidate;
            step *= 2.0;
        }
    }
    loop {
        let middle = (below + (above - below) / 2.0).floor();
        if middle <= below || middle >= above {
            return above;
        }
        if reached(middle) {
            above = middle;
        } else {
            below = middle;
        }
    }
}

pub fn qbinom(p: f64, n: f64, pr: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || n.is_nan() || pr.is_nan() {
        return p + n + pr;
    }
    if !finite(n) || !finite(pr) || (!finite(p) && !log) {
        return f64::NAN;
    }
    if n != (n + 0.5).floor() || !(0.0..=1.0).contains(&pr) || n < 0.0 {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, n) {
        return boundary;
    }
    if pr == 0.0 || n == 0.0 {
        return 0.0;
    }
    let q = 1.0 - pr;
    if q == 0.0 {
        return n;
    }
    let mu = n * pr;
    let sigma = (n * pr * q).sqrt();
    let gamma = (q - pr) / sigma;
    discrete_quantile(p, mu, sigma, gamma, lower, log, n, &|y, lower, log| {
        pbinom(y, n, pr, lower, log)
    })
}

pub fn dnbinom(x: f64, size: f64, prob: f64, log: bool) -> f64 {
    if x.is_nan() || size.is_nan() || prob.is_nan() {
        return x + size + prob;
    }
    if prob <= 0.0 || prob > 1.0 || size < 0.0 {
        return f64::NAN;
    }
    if nonint_warning(x) || x < 0.0 || !finite(x) {
        return d0(log);
    }
    if x == 0.0 && size == 0.0 {
        return d1(log);
    }
    let x = forceint(x);
    let size = if finite(size) { size } else { DBL_MAX };
    if x != 0.0 && x < 1e-10 * size {
        // log(choose(size + x - 1, x)) is about x log(size) - lgamma(x + 1) here, which
        // dbinom_raw would lose to cancellation (Abramowitz and Stegun 6.1.47).
        return d_exp(
            size * prob.ln() + x * (size.ln() + (-prob).ln_1p()) - lgamma1p(x)
                + (x * (x - 1.0) / (2.0 * size)).ln_1p(),
            log,
        );
    }
    let ans = dbinom_raw(size, x + size, prob, 1.0 - prob, log);
    let p = size / (size + x);
    if log { p.ln() + ans } else { p * ans }
}

pub fn dnbinom_mu(x: f64, size: f64, mu: f64, log: bool) -> f64 {
    if x.is_nan() || size.is_nan() || mu.is_nan() {
        return x + size + mu;
    }
    if mu < 0.0 || size < 0.0 {
        return f64::NAN;
    }
    if nonint_warning(x) || x < 0.0 || !finite(x) {
        return d0(log);
    }
    if x == 0.0 && size == 0.0 {
        return d1(log);
    }
    let x = forceint(x);
    if !finite(size) {
        return dpois_raw(x, mu, log);
    }
    if x == 0.0 {
        return d_exp(
            size * if size < mu {
                (size / (size + mu)).ln()
            } else {
                (-mu / (size + mu)).ln_1p()
            },
            log,
        );
    }
    if x < 1e-10 * size {
        let p = if size < mu {
            (size / (1.0 + size / mu)).ln()
        } else {
            (mu / (1.0 + mu / size)).ln()
        };
        return d_exp(
            x * p - mu - lgammafn(x + 1.0) + (x * (x - 1.0) / (2.0 * size)).ln_1p(),
            log,
        );
    }
    let p = size / (size + x);
    let ans = dbinom_raw(size, x + size, size / (size + mu), mu / (size + mu), log);
    if log { p.ln() + ans } else { p * ans }
}

pub fn pnbinom(x: f64, size: f64, prob: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || size.is_nan() || prob.is_nan() {
        return x + size + prob;
    }
    if !finite(size) || !finite(prob) || size < 0.0 || prob <= 0.0 || prob > 1.0 {
        return f64::NAN;
    }
    if size == 0.0 {
        return if x >= 0.0 {
            dt1(lower, log)
        } else {
            dt0(lower, log)
        };
    }
    if x < 0.0 {
        return dt0(lower, log);
    }
    if !finite(x) {
        return dt1(lower, log);
    }
    let x = (x + 1e-7).floor();
    pbeta(prob, size, x + 1.0, lower, log)
}

pub fn pnbinom_mu(x: f64, size: f64, mu: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || size.is_nan() || mu.is_nan() {
        return x + size + mu;
    }
    if !finite(mu) || size < 0.0 || mu < 0.0 {
        return f64::NAN;
    }
    if size == 0.0 {
        return if x >= 0.0 {
            dt1(lower, log)
        } else {
            dt0(lower, log)
        };
    }
    if x < 0.0 {
        return dt0(lower, log);
    }
    if !finite(x) {
        return dt1(lower, log);
    }
    if !finite(size) {
        return ppois(x, mu, lower, log);
    }
    let x = (x + 1e-7).floor();
    let (w, wc, ierr) = bratio(size, x + 1.0, size / (size + mu), mu / (size + mu), log);
    if ierr != 0 {
        warning(format!("pnbinom_mu() -> bratio() gave error code {ierr}"));
    }
    if lower { w } else { wc }
}

pub fn qnbinom(p: f64, size: f64, prob: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || size.is_nan() || prob.is_nan() {
        return p + size + prob;
    }
    if prob == 0.0 && size == 0.0 {
        return 0.0;
    }
    if prob <= 0.0 || prob > 1.0 || size < 0.0 {
        return f64::NAN;
    }
    if prob == 1.0 || size == 0.0 {
        return 0.0;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    let q = 1.0 / prob;
    let pp = (1.0 - prob) * q;
    let mu = size * pp;
    let sigma = (size * pp * q).sqrt();
    let gamma = (q + pp) / sigma;
    discrete_quantile(
        p,
        mu,
        sigma,
        gamma,
        lower,
        log,
        f64::INFINITY,
        &|y, lower, log| pnbinom(y, size, prob, lower, log),
    )
}

pub fn qnbinom_mu(p: f64, size: f64, mu: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || size.is_nan() || mu.is_nan() {
        return p + size + mu;
    }
    if mu == 0.0 || size == 0.0 {
        return 0.0;
    }
    if mu < 0.0 || size < 0.0 {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    if !finite(size) {
        return qpois(p, mu, lower, log);
    }
    let q = 1.0 + mu / size;
    let pp = mu / size;
    let sigma = (size * pp * q).sqrt();
    let gamma = (q + pp) / sigma;
    discrete_quantile(
        p,
        mu,
        sigma,
        gamma,
        lower,
        log,
        f64::INFINITY,
        &|y, lower, log| pnbinom_mu(y, size, mu, lower, log),
    )
}

pub fn dgeom(x: f64, p: f64, log: bool) -> f64 {
    if x.is_nan() || p.is_nan() {
        return x + p;
    }
    if p <= 0.0 || p > 1.0 {
        return f64::NAN;
    }
    if nonint_warning(x) || x < 0.0 || !finite(x) || p == 0.0 {
        return d0(log);
    }
    let x = forceint(x);
    let prob = dbinom_raw(0.0, x, p, 1.0 - p, log);
    if log { p.ln() + prob } else { p * prob }
}

pub fn pgeom(x: f64, p: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || p.is_nan() {
        return x + p;
    }
    if p <= 0.0 || p > 1.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return dt0(lower, log);
    }
    if !finite(x) {
        return dt1(lower, log);
    }
    let x = (x + 1e-7).floor();
    if p == 1.0 {
        let x: f64 = if lower { 1.0 } else { 0.0 };
        return if log { x.ln() } else { x };
    }
    let x = (-p).ln_1p() * (x + 1.0);
    if log {
        dt_clog(x, lower, log)
    } else if lower {
        -x.exp_m1()
    } else {
        x.exp()
    }
}

pub fn qgeom(p: f64, prob: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || prob.is_nan() {
        return p + prob;
    }
    if prob <= 0.0 || prob > 1.0 || q_p01_invalid(p, log) {
        return f64::NAN;
    }
    if prob == 1.0 {
        return 0.0;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    fmax2(
        0.0,
        (dt_clog(p, lower, log) / (-prob).ln_1p() - 1.0 - 1e-12).ceil(),
    )
}

pub fn dhyper(x: f64, r: f64, b: f64, n: f64, log: bool) -> f64 {
    if x.is_nan() || r.is_nan() || b.is_nan() || n.is_nan() {
        return x + r + b + n;
    }
    if negative_or_nonint(r) || negative_or_nonint(b) || negative_or_nonint(n) || n > r + b {
        return f64::NAN;
    }
    if x < 0.0 || nonint_warning(x) {
        return d0(log);
    }
    let (x, r, b, n) = (forceint(x), forceint(r), forceint(b), forceint(n));
    if n < x || r < x || n - x > b {
        return d0(log);
    }
    if n == 0.0 {
        return if x == 0.0 { d1(log) } else { d0(log) };
    }
    let p = n / (r + b);
    let q = (r + b - n) / (r + b);
    let p1 = dbinom_raw(x, r, p, q, log);
    let p2 = dbinom_raw(n - x, b, p, q, log);
    let p3 = dbinom_raw(n, r + b, p, q, log);
    if log { p1 + p2 - p3 } else { p1 * p2 / p3 }
}

/// `phyper(x) / dhyper(x)` in the lower tail, for `x (NR + NB) <= n NR`.
fn pdhyper(x: f64, nr: f64, nb: f64, n: f64, log: bool) -> f64 {
    let mut x = x;
    let mut sum = 0.0;
    let mut term = 1.0;
    while x > 0.0 && term >= DBL_EPSILON * sum {
        term *= x * (nb - n + x) / (n + 1.0 - x) / (nr + 1.0 - x);
        sum += term;
        x -= 1.0;
    }
    if log { sum.ln_1p() } else { 1.0 + sum }
}

pub fn phyper(x: f64, nr: f64, nb: f64, n: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || nr.is_nan() || nb.is_nan() || n.is_nan() {
        return x + nr + nb + n;
    }
    let mut x = (x + 1e-7).floor();
    let mut nr = forceint(nr);
    let mut nb = forceint(nb);
    let n = forceint(n);
    let mut lower = lower;
    if nr < 0.0 || nb < 0.0 || !finite(nr + nb) || n < 0.0 || n > nr + nb {
        return f64::NAN;
    }
    if x * (nr + nb) > n * nr {
        std::mem::swap(&mut nr, &mut nb);
        x = n - x - 1.0;
        lower = !lower;
    }
    if x < 0.0 || x < n - nb {
        return dt0(lower, log);
    }
    if x >= nr || x >= n {
        return dt1(lower, log);
    }
    let d = dhyper(x, nr, nb, n, log);
    if (!log && d == 0.0) || (log && d == f64::NEG_INFINITY) {
        return dt0(lower, log);
    }
    let pd = pdhyper(x, nr, nb, n, log);
    if log {
        dt_log_of_log(d + pd, lower)
    } else {
        d_lval(d * pd, lower)
    }
}

pub fn qhyper(p: f64, nr: f64, nb: f64, n: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || nr.is_nan() || nb.is_nan() || n.is_nan() {
        return p + nr + nb + n;
    }
    if !finite(p) || !finite(nr) || !finite(nb) || !finite(n) {
        return f64::NAN;
    }
    let mut nr = forceint(nr);
    let mut nb = forceint(nb);
    let total = nr + nb;
    let n = forceint(n);
    if nr < 0.0 || nb < 0.0 || n < 0.0 || n > total {
        return f64::NAN;
    }
    let xstart = fmax2(0.0, n - nb);
    let xend = fmin2(n, nr);
    if let Some(boundary) = q_p01_boundaries(p, lower, log, xstart, xend) {
        return boundary;
    }
    let mut xr = xstart;
    let mut xb = n - xr;
    let small_n = total < 1000.0;
    let mut term = lfastchoose(nr, xr) + lfastchoose(nb, xb) - lfastchoose(total, n);
    if small_n {
        term = term.exp();
    }
    nr -= xr;
    nb -= xb;
    let mut p = p;
    if !lower || log {
        p = dt_qiv(p, lower, log);
    }
    p *= 1.0 - 1000.0 * DBL_EPSILON;
    let mut sum = if small_n { term } else { term.exp() };
    while sum < p && xr < xend {
        xr += 1.0;
        nb += 1.0;
        if small_n {
            term *= (nr / xr) * (xb / nb);
        } else {
            term += (nr / xr).ln() + (xb / nb).ln();
        }
        sum += if small_n { term } else { term.exp() };
        xb -= 1.0;
        nr -= 1.0;
    }
    xr
}

pub fn ppois(x: f64, lambda: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || lambda.is_nan() {
        return x + lambda;
    }
    if lambda < 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return dt0(lower, log);
    }
    if lambda == 0.0 || !finite(x) {
        return dt1(lower, log);
    }
    let x = (x + 1e-7).floor();
    pgamma(lambda, x + 1.0, 1.0, !lower, log)
}

pub fn qpois(p: f64, lambda: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || lambda.is_nan() {
        return p + lambda;
    }
    if !finite(lambda) || lambda < 0.0 {
        return f64::NAN;
    }
    if lambda == 0.0 {
        let outside = if log {
            p > 0.0
        } else {
            !(0.0..=1.0).contains(&p)
        };
        return if outside { f64::NAN } else { 0.0 };
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    let mu = lambda;
    let sigma = lambda.sqrt();
    let gamma = 1.0 / sigma;
    discrete_quantile(
        p,
        mu,
        sigma,
        gamma,
        lower,
        log,
        f64::INFINITY,
        &|y, lower, log| ppois(y, lambda, lower, log),
    )
}

// Student's t and Fisher's F.

pub fn dt(x: f64, n: f64, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if n <= 0.0 {
        return f64::NAN;
    }
    if !finite(x) {
        return d0(log);
    }
    if !finite(n) {
        return dnorm(x, 0.0, 1.0, log);
    }
    let t = -bd0(n / 2.0, (n + 1.0) / 2.0) + stirlerr((n + 1.0) / 2.0) - stirlerr(n / 2.0);
    let x2n = x * x / n;
    let large = x2n > 1.0 / DBL_EPSILON;
    let mut ax = 0.0;
    let (l_x2n, u);
    if large {
        ax = x.abs();
        l_x2n = ax.ln() - n.ln() / 2.0;
        u = n * l_x2n;
    } else if x2n > 0.2 {
        l_x2n = (1.0 + x2n).ln() / 2.0;
        u = n * l_x2n;
    } else {
        l_x2n = x2n.ln_1p() / 2.0;
        u = -bd0(n / 2.0, (n + x * x) / 2.0) + x * x / 2.0;
    }
    if log {
        return t - u - (M_LN_SQRT_2PI + l_x2n);
    }
    let i_sqrt = if large { n.sqrt() / ax } else { (-l_x2n).exp() };
    (t - u).exp() * M_1_SQRT_2PI * i_sqrt
}

pub fn pt(x: f64, n: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if n <= 0.0 {
        return f64::NAN;
    }
    if !finite(x) {
        return if x < 0.0 {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    if !finite(n) {
        return pnorm(x, 0.0, 1.0, lower, log);
    }
    let nx = 1.0 + (x / n) * x;
    let val = if nx > 1e100 {
        let lval = -0.5 * n * (2.0 * x.abs().ln() - n.ln()) - lbeta(0.5 * n, 0.5) - (0.5 * n).ln();
        if log { lval } else { lval.exp() }
    } else if n > x * x {
        pbeta(x * x / (n + x * x), 0.5, n / 2.0, false, log)
    } else {
        pbeta(1.0 / nx, n / 2.0, 0.5, true, log)
    };
    let lower = if x <= 0.0 { !lower } else { lower };
    if log {
        if lower {
            (-0.5 * val.exp()).ln_1p()
        } else {
            val - M_LN2
        }
    } else {
        d_cval(val / 2.0, lower)
    }
}

/// Student's t quantile by Hill's algorithm 396 with a two-term Taylor refinement (Hill 1981),
/// bisection below one degree of freedom.
pub fn qt(p: f64, ndf: f64, lower: bool, log: bool) -> f64 {
    const EPS: f64 = 1e-12;
    if p.is_nan() || ndf.is_nan() {
        return p + ndf;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, f64::NEG_INFINITY, f64::INFINITY) {
        return boundary;
    }
    if ndf <= 0.0 {
        return f64::NAN;
    }
    if ndf < 1.0 {
        const ACCU: f64 = 1e-13;
        const EPS_BRACKET: f64 = 1e-11;
        let p = dt_qiv(p, lower, log);
        if p > 1.0 - DBL_EPSILON {
            return f64::INFINITY;
        }
        let mut pp = fmin2(1.0 - DBL_EPSILON, p * (1.0 + EPS_BRACKET));
        let mut ux = 1.0;
        while ux < DBL_MAX && pt(ux, ndf, true, false) < pp {
            ux *= 2.0;
        }
        pp = p * (1.0 - EPS_BRACKET);
        let mut lx = -1.0;
        while lx > -DBL_MAX && pt(lx, ndf, true, false) > pp {
            lx *= 2.0;
        }
        let mut iteration = 0;
        loop {
            let nx = 0.5 * (lx + ux);
            if pt(nx, ndf, true, false) > p {
                ux = nx;
            } else {
                lx = nx;
            }
            iteration += 1;
            if !((ux - lx) / nx.abs() > ACCU && iteration < 1000) {
                break;
            }
        }
        if iteration >= 1000 {
            precision_warning("qt");
        }
        return 0.5 * (lx + ux);
    }
    if ndf > 1e20 {
        return qnorm(p, 0.0, 1.0, lower, log);
    }
    let mut big_p = d_qiv(p, log);
    let neg = (!lower || big_p < 0.5) && (lower || big_p > 0.5);
    let is_neg_lower = lower == neg;
    if neg {
        big_p = 2.0
            * if log {
                if lower { big_p } else { -p.exp_m1() }
            } else {
                d_lval(p, lower)
            };
    } else {
        big_p = 2.0
            * if log {
                if lower { -p.exp_m1() } else { big_p }
            } else {
                d_cval(p, lower)
            };
    }
    let mut q;
    if (ndf - 2.0).abs() < EPS {
        if big_p > DBL_MIN {
            if 3.0 * big_p < DBL_EPSILON {
                q = 1.0 / big_p.sqrt();
            } else if big_p > 0.9 {
                q = (1.0 - big_p) * (2.0 / (big_p * (2.0 - big_p))).sqrt();
            } else {
                q = (2.0 / (big_p * (2.0 - big_p)) - 2.0).sqrt();
            }
        } else if log {
            q = if is_neg_lower {
                (-p / 2.0).exp() / M_SQRT2
            } else {
                1.0 / (-p.exp_m1()).sqrt()
            };
        } else {
            q = f64::INFINITY;
        }
    } else if ndf < 1.0 + EPS {
        if big_p == 1.0 {
            q = 0.0;
        } else if big_p > 0.0 {
            q = 1.0 / tanpi_(big_p / 2.0);
        } else if log {
            q = if is_neg_lower {
                M_1_PI * (-p).exp()
            } else {
                -1.0 / (M_PI * p.exp_m1())
            };
        } else {
            q = f64::INFINITY;
        }
    } else {
        let mut x = 0.0;
        let mut y;
        let mut log_p2 = 0.0;
        let a = 1.0 / (ndf - 0.5);
        let b = 48.0 / (a * a);
        let mut c = ((20700.0 * a / b - 98.0) * a - 16.0) * a + 96.36;
        let d = ((94.5 / (b + c) - 3.0) / b + 1.0) * (a * M_PI_2).sqrt() * ndf;
        let p_ok1 = big_p > DBL_MIN || !log;
        let mut p_ok = p_ok1;
        y = 0.0;
        if p_ok1 {
            y = (d * big_p).powf(2.0 / ndf);
            p_ok = y >= DBL_EPSILON;
        }
        if !p_ok {
            log_p2 = if is_neg_lower {
                d_log(p, log)
            } else {
                d_lexp(p, log)
            };
            x = (d.ln() + M_LN2 + log_p2) / ndf;
            y = (2.0 * x).exp();
        }
        if (ndf < 2.1 && big_p > 0.5) || y > 0.05 + a {
            x = if p_ok {
                qnorm(0.5 * big_p, 0.0, 1.0, true, false)
            } else {
                qnorm(log_p2, 0.0, 1.0, lower, true)
            };
            y = x * x;
            if ndf < 5.0 {
                c += 0.3 * (ndf - 4.5) * (x + 0.6);
            }
            c += (((0.05 * d * x - 5.0) * x - 7.0) * x - 2.0) * x + b;
            y = (((((0.4 * y + 6.3) * y + 36.0) * y + 94.5) / c - y - 3.0) / b + 1.0) * x;
            y = (a * y * y).exp_m1();
            q = (ndf * y).sqrt();
        } else if !p_ok && x < -M_LN2 * DBL_MANT_DIG as f64 {
            q = ndf.sqrt() * (-x).exp();
        } else {
            y = ((1.0 / (((ndf + 6.0) / (ndf * y) - 0.089 * d - 0.822) * (ndf + 2.0) * 3.0)
                + 0.5 / (ndf + 4.0))
                * y
                - 1.0)
                * (ndf + 1.0)
                / (ndf + 2.0)
                + 1.0 / y;
            q = (ndf * y).sqrt();
        }
        if p_ok1 {
            let mut iteration = 0;
            loop {
                if iteration >= 10 {
                    break;
                }
                iteration += 1;
                let y = dt(q, ndf, false);
                if y <= 0.0 {
                    break;
                }
                let x = (pt(q, ndf, false, false) - big_p / 2.0) / y;
                if !finite(x) || x.abs() <= 1e-14 * q.abs() {
                    break;
                }
                // q (q^2 + ndf)^-1 written as (q + ndf / q)^-1, because q^2 overflows when the
                // tail probability is far below DBL_MIN's square root.
                q += x * (1.0 + x * (ndf + 1.0) / (2.0 * (q + ndf / q)));
            }
        }
    }
    if neg {
        q = -q;
    }
    q
}

pub fn df(x: f64, m: f64, n: f64, log: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d0(log);
    }
    if x == 0.0 {
        return if m > 2.0 {
            d0(log)
        } else if m == 2.0 {
            d1(log)
        } else {
            f64::INFINITY
        };
    }
    if !finite(m) && !finite(n) {
        return if x == 1.0 { f64::INFINITY } else { d0(log) };
    }
    if !finite(n) {
        return dgamma(x, m / 2.0, 2.0 / m, log);
    }
    if m > 1e14 {
        let dens = dgamma(1.0 / x, n / 2.0, 2.0 / n, log);
        return if log {
            dens - 2.0 * x.ln()
        } else {
            dens / (x * x)
        };
    }
    let mut f = 1.0 / (n + x * m);
    let q = n * f;
    let p = x * m * f;
    let dens;
    if m >= 2.0 {
        f = m * q / 2.0;
        dens = dbinom_raw((m - 2.0) / 2.0, (m + n - 2.0) / 2.0, p, q, log);
    } else {
        f = m * m * q / (2.0 * p * (m + n));
        dens = dbinom_raw(m / 2.0, (m + n) / 2.0, p, q, log);
    }
    if log { f.ln() + dens } else { f * dens }
}

pub fn pf(x: f64, df1: f64, df2: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || df1.is_nan() || df2.is_nan() {
        return x + df2 + df1;
    }
    if df1 <= 0.0 || df2 <= 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt0(lower, log);
    }
    if x >= f64::INFINITY {
        return dt1(lower, log);
    }
    if df2 == f64::INFINITY {
        if df1 == f64::INFINITY {
            if x < 1.0 {
                return dt0(lower, log);
            }
            if x == 1.0 {
                return if log { -M_LN2 } else { 0.5 };
            }
            return dt1(lower, log);
        }
        return pchisq(x * df1, df1, lower, log);
    }
    if df1 == f64::INFINITY {
        return pchisq(df2 / x, df2, !lower, log);
    }
    let value = if df1 * x > df2 {
        pbeta(df2 / (df2 + df1 * x), df2 / 2.0, df1 / 2.0, !lower, log)
    } else {
        pbeta(df1 * x / (df2 + df1 * x), df1 / 2.0, df2 / 2.0, lower, log)
    };
    if value.is_nan() { f64::NAN } else { value }
}

pub fn qf(p: f64, df1: f64, df2: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || df1.is_nan() || df2.is_nan() {
        return p + df1 + df2;
    }
    if df1 <= 0.0 || df2 <= 0.0 {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    if df1 <= df2 && df2 > 4e5 {
        if !finite(df1) {
            return 1.0;
        }
        return qchisq(p, df1, lower, log) / df1;
    }
    if df1 > 4e5 {
        return df2 / qchisq(p, df2, !lower, log);
    }
    let value = (1.0 / qbeta(p, df2 / 2.0, df1 / 2.0, !lower, log) - 1.0) * (df2 / df1);
    if value.is_nan() { f64::NAN } else { value }
}

// Non-central distributions.

pub fn dnchisq(x: f64, df: f64, ncp: f64, log: bool) -> f64 {
    const EPS: f64 = 5e-15;
    if x.is_nan() || df.is_nan() || ncp.is_nan() {
        return x + df + ncp;
    }
    if !finite(df) || !finite(ncp) || ncp < 0.0 || df < 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d0(log);
    }
    if x == 0.0 && df < 2.0 {
        return f64::INFINITY;
    }
    if ncp == 0.0 {
        return if df > 0.0 {
            dchisq(x, df, log)
        } else {
            d0(log)
        };
    }
    if x == f64::INFINITY {
        return d0(log);
    }
    let ncp2 = 0.5 * ncp;
    // Sum the Poisson mixture outwards from its largest term.
    let mut imax = ((-(2.0 + df) + ((2.0 - df) * (2.0 - df) + 4.0 * ncp * x).sqrt()) / 4.0).ceil();
    if imax < 0.0 {
        imax = 0.0;
    }
    let (dfmid, mid);
    if finite(imax) {
        dfmid = df + 2.0 * imax;
        mid = dpois_raw(imax, ncp2, false) * dchisq(x, dfmid, false);
    } else {
        dfmid = 0.0;
        mid = 0.0;
    }
    if mid == 0.0 {
        if log || ncp > 1000.0 {
            let nl = df + ncp;
            let ic = nl / (nl + ncp);
            return dchisq(x * ic, nl * ic, log);
        }
        return d0(log);
    }
    let mut sum = mid;
    let mut term = mid;
    let mut df_ = dfmid;
    let mut i = imax;
    let x2 = x * ncp2;
    loop {
        i += 1.0;
        let q = x2 / i / df_;
        df_ += 2.0;
        term *= q;
        sum += term;
        if !(q >= 1.0 || term * q > (1.0 - q) * EPS || term > 1e-10 * sum) {
            break;
        }
    }
    term = mid;
    df_ = dfmid;
    i = imax;
    while i != 0.0 {
        df_ -= 2.0;
        let q = i * df_ / x2;
        i -= 1.0;
        term *= q;
        sum += term;
        if q < 1.0 && term * q <= (1.0 - q) * EPS {
            break;
        }
    }
    d_val(sum, log)
}

/// The noncentral chi-squared distribution function, the Poisson mixture
/// `sum_k w_k(ncp / 2) P(df / 2 + k, x / 2)` of regularized lower incomplete gamma functions
/// (Johnson, Kotz and Balakrishnan (1995), chapter 29). Neighbouring components differ by
/// `P(s, y) - P(s + 1, y) = y^s e^-y / Gamma(s + 1)`, the Poisson density at a real `s`, so
/// the mixture is summed as `poisson_mixture_tails` describes.
pub fn pnchisq(x: f64, df: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || df.is_nan() || ncp.is_nan() {
        return x + df + ncp;
    }
    if !finite(df) || !finite(ncp) || df < 0.0 || ncp < 0.0 {
        return f64::NAN;
    }
    let rate = ncp / 2.0;
    if x < 0.0 || (x == 0.0 && df > 0.0) {
        return dt0(lower, log);
    }
    if x == 0.0 {
        // With no degrees of freedom the first component is an atom at zero.
        return match (lower, log) {
            (true, _) => d_exp(-rate, log),
            (false, true) => log1_exp(-rate),
            (false, false) => -(-rate).exp_m1(),
        };
    }
    if x == f64::INFINITY {
        return dt1(lower, log);
    }
    let mode = rate.floor();
    if mode > i32::MAX as f64 {
        precision_warning("pnchisq");
        return f64::NAN;
    }
    let half = x / 2.0;
    let shape = df / 2.0 + mode;
    let log_half = half.ln();
    let (lower_sum, upper_sum) = poisson_mixture_tails(
        rate,
        mode,
        (
            pgamma(half, shape, 1.0, true, false),
            pgamma(half, shape, 1.0, false, false),
        ),
        dpois_raw(shape, half, true),
        |index| log_half - (df / 2.0 + index + 1.0).ln(),
    );
    // A lower tail below the double range underflows in the linear sum; on the log scale it
    // is summed term by term, each component's log probability coming straight from pgamma.
    if lower && log && lower_sum < 1e-280 {
        let mut total = f64::NEG_INFINITY;
        let mut index = 0.0;
        while index < 1e6 {
            let term =
                dpois_raw(index, rate, true) + pgamma(half, df / 2.0 + index, 1.0, true, true);
            total = logspace_add(total, term);
            // Past the Poisson mode the terms only shrink, so one far below the total ends it.
            if index > rate && term < total - 40.0 {
                break;
            }
            index += 1.0;
        }
        return total;
    }
    mixture_tail(lower_sum, upper_sum, lower, log)
}

/// Both tails of a Poisson mixture `sum_k w_k(rate) F_k` whose component distribution
/// functions fall by `F_k - F_(k+1) = g_k`, given the tails of `F_mode` at the Poisson mode,
/// `ln g_mode`, and `ln(g_(k+1) / g_k)` as `log_ratio(k)`. This is the summation of Lenth
/// (1987) AS 226, with the remainders bounded by the Poisson tails as in Frick (1990) AS R84.
///
/// The sum runs outward from the mode, where the weights are largest. Each tail is
/// accumulated in both directions, which means that in one direction its recurrence
/// subtracts; there its rounding error stays below that of the starting value at the mode,
/// whose term alone is a fixed share of the sum. Both tails are therefore summed directly,
/// rather than one being the complement of the other, and a small tail keeps its relative
/// accuracy. The weights beyond index `k` sum to at most `w_(k+1) / (1 - rate / (k + 2))`, and
/// those below it to at most `w_(k-1) / (1 - (k - 1) / rate)`; a component's tail lies in
/// [0, 1] and only grows away from the mode on the side being bounded.
fn poisson_mixture_tails(
    rate: f64,
    mode: f64,
    (start_lower, start_upper): (f64, f64),
    start_log_step: f64,
    log_ratio: impl Fn(f64) -> f64,
) -> (f64, f64) {
    let tolerance = DBL_EPSILON / 4.0;
    let start_weight = dpois_raw(mode, rate, false);
    let mut lower_sum = start_weight * start_lower;
    let mut upper_sum = start_weight * start_upper;

    let (mut index, mut weight, mut log_step) = (mode, start_weight, start_log_step);
    let (mut lower_part, mut upper_part) = (start_lower, start_upper);
    while index > 0.0 {
        log_step -= log_ratio(index - 1.0);
        let step = log_step.exp();
        lower_part = (lower_part + step).min(1.0);
        upper_part = (upper_part - step).max(0.0);
        weight *= index / rate;
        index -= 1.0;
        lower_sum += weight * lower_part;
        upper_sum += weight * upper_part;
        let remainder = if index == 0.0 {
            0.0
        } else {
            weight * (index / rate) / (1.0 - (index - 1.0) / rate)
        };
        if weight == 0.0
            || (remainder <= tolerance * lower_sum
                && upper_part * remainder <= tolerance * upper_sum)
        {
            break;
        }
    }

    let (mut index, mut weight, mut log_step) = (mode, start_weight, start_log_step);
    let (mut lower_part, mut upper_part) = (start_lower, start_upper);
    loop {
        let step = log_step.exp();
        lower_part = (lower_part - step).max(0.0);
        upper_part = (upper_part + step).min(1.0);
        log_step += log_ratio(index);
        weight *= rate / (index + 1.0);
        index += 1.0;
        lower_sum += weight * lower_part;
        upper_sum += weight * upper_part;
        let remainder = weight * (rate / (index + 1.0)) / (1.0 - rate / (index + 2.0));
        if weight == 0.0
            || (lower_part * remainder <= tolerance * lower_sum
                && remainder <= tolerance * upper_sum)
        {
            break;
        }
    }
    (lower_sum, upper_sum)
}

/// The requested tail and scale from both tails of a mixture. A log probability near zero
/// comes from the other tail, which keeps the digits the direct one has lost.
fn mixture_tail(lower_sum: f64, upper_sum: f64, lower: bool, log: bool) -> f64 {
    // The Poisson weights that were summed fall short of 1 by rounding and by the truncated
    // remainder, so the tails are scaled by their total. Without that, a mixture whose every
    // component is exactly 1 would come out a few units in the last place below 1.
    let total = lower_sum + upper_sum;
    let (lower_sum, upper_sum) = if total > 0.0 {
        (lower_sum / total, upper_sum / total)
    } else {
        (lower_sum, upper_sum)
    };
    let (direct, other) = if lower {
        (lower_sum, upper_sum)
    } else {
        (upper_sum, lower_sum)
    };
    if !log {
        direct.min(1.0)
    } else if direct > 0.5 {
        (-other).ln_1p()
    } else {
        direct.ln()
    }
}

/// The quantile of the noncentral chi-squared distribution, by bisection on the distribution
/// function. The search starts from Patnaik's (1949) approximation, which matches the first
/// two moments with a scaled central chi-squared, `c * chi2(nu)` with
/// `c = (df + 2 ncp) / (df + ncp)` and `nu = (df + ncp)^2 / (df + 2 ncp)`, and widens a
/// bracket around it by doubling or halving.
pub fn qnchisq(p: f64, df: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || df.is_nan() || ncp.is_nan() {
        return p + df + ncp;
    }
    if !finite(df) || !finite(ncp) || df < 0.0 || ncp < 0.0 || (df == 0.0 && ncp == 0.0) {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    let below_root = |x: f64| {
        let value = pnchisq(x, df, ncp, lower, log);
        if lower { value < p } else { value > p }
    };
    let scale = (df + 2.0 * ncp) / (df + ncp);
    let shape = (df + ncp) * (df + ncp) / (df + 2.0 * ncp);
    let mut start = scale * qchisq(p, shape, lower, log);
    if !(start > 0.0 && start.is_finite()) {
        start = (df + ncp).max(1.0);
    }
    let (mut low, mut high);
    if below_root(start) {
        low = start;
        high = 2.0 * start;
        while below_root(high) {
            low = high;
            high *= 2.0;
            if high == f64::INFINITY {
                return f64::INFINITY;
            }
        }
    } else {
        high = start;
        low = 0.5 * start;
        // With df = 0 the distribution has an atom at zero, so the bracket may close on it.
        while !below_root(low) {
            high = low;
            low *= 0.5;
            if low < DBL_MIN {
                return 0.0;
            }
        }
    }
    // GNU R stops at a relative accuracy of about 1e-13, and its own tests compare round trips
    // with all.equal, which leaves exact matches out of the mean: a quantile that is exact more
    // often than GNU's shifts that mean, so the search stops at the same accuracy.
    bisect(low, high, 1e-13, below_root)
}

/// The density of the noncentral beta distribution, the Poisson mixture
/// `sum_k w_k(ncp / 2) dbeta(x; a + k, b)` (Johnson, Kotz and Balakrishnan (1995), chapter
/// 30). Consecutive terms have the ratio `r(k) = (ncp / 2) x (a + b + k) / ((k + 1)(a + k))`,
/// which decreases in `k`, so the largest term sits where `r(k) = 1`, a quadratic in `k`. The
/// sum runs outward from there in both directions, and because the ratios keep shrinking
/// away from the peak, the terms not yet added are bounded by a geometric series.
pub fn dnbeta(x: f64, a: f64, b: f64, ncp: f64, log: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() || ncp.is_nan() {
        return x + a + b + ncp;
    }
    if a <= 0.0 || b <= 0.0 || ncp < 0.0 || !finite(a) || !finite(b) || !finite(ncp) {
        return f64::NAN;
    }
    if !(0.0..=1.0).contains(&x) {
        return d0(log);
    }
    if ncp == 0.0 {
        return dbeta(x, a, b, log);
    }
    let rate = ncp / 2.0;
    if x == 0.0 {
        // Only the first component can put density at zero.
        return d_exp(dbeta(0.0, a, b, true) - rate, log);
    }
    if x == 1.0 {
        // Every component has density b^0 (a + k) at one when b = 1, so the mixture is the
        // Poisson mean shifted by a.
        return if b < 1.0 {
            f64::INFINITY
        } else if b > 1.0 {
            d0(log)
        } else {
            d_val(a + rate, log)
        };
    }
    let linear = a + 1.0 - rate * x;
    let constant = a - rate * x * (a + b);
    let root = (-linear + (linear * linear - 4.0 * constant).sqrt()) / 2.0;
    let peak = if root > 0.0 { root.floor() } else { 0.0 };
    if peak > i32::MAX as f64 {
        precision_warning("dnbeta");
        return f64::NAN;
    }
    let peak_log = dbeta(x, a + peak, b, true) + dpois_raw(peak, rate, true);
    if !peak_log.is_finite() {
        return if peak_log > 0.0 {
            f64::INFINITY
        } else {
            d0(log)
        };
    }
    let ratio = |k: f64| rate * x * (a + b + k) / ((k + 1.0) * (a + k));
    let tolerance = DBL_EPSILON / 4.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut index = peak;
    loop {
        let step = ratio(index);
        term *= step;
        index += 1.0;
        sum += term;
        if term == 0.0 || (step < 1.0 && term * step / (1.0 - step) <= tolerance * sum) {
            break;
        }
    }
    term = 1.0;
    index = peak;
    while index > 0.0 {
        let step = 1.0 / ratio(index - 1.0);
        term *= step;
        index -= 1.0;
        sum += term;
        if term == 0.0 || (step < 1.0 && term * step / (1.0 - step) <= tolerance * sum) {
            break;
        }
    }
    d_exp(peak_log + sum.ln(), log)
}

pub fn pnbeta(x: f64, a: f64, b: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() || ncp.is_nan() {
        return x + a + b + ncp;
    }
    if a <= 0.0 || b <= 0.0 || ncp < 0.0 || !finite(a) || !finite(b) || !finite(ncp) {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt0(lower, log);
    }
    if x >= 1.0 {
        return dt1(lower, log);
    }
    pnbeta_both(x, 0.5 - x + 0.5, a, b, ncp, lower, log)
}

/// The noncentral beta distribution function at `x`, with `complement = 1 - x` passed
/// separately so that a caller near one keeps its precision. It is the Poisson mixture
/// `sum_k w_k(ncp / 2) I_x(a + k, b)` of incomplete beta ratios, whose neighbours differ by
/// `I_x(s, b) - I_x(s + 1, b) = x^s (1 - x)^b / (s B(s, b))`, summed as
/// `poisson_mixture_tails` describes.
fn pnbeta_both(x: f64, complement: f64, a: f64, b: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    let rate = ncp / 2.0;
    let mode = rate.floor();
    if mode > i32::MAX as f64 {
        precision_warning("pnbeta");
        return f64::NAN;
    }
    let shape = a + mode;
    let (start_lower, start_upper, error) = bratio(shape, b, x, complement, false);
    if error != 0 {
        precision_warning("pnbeta");
    }
    // Near one, x itself rounds to 1 and only the complement keeps the distance from it.
    let log_x = if x > 0.5 {
        (-complement).ln_1p()
    } else {
        x.ln()
    };
    let start_log_step = shape * log_x + b * complement.ln() - lbeta(shape, b) - shape.ln();
    let (lower_sum, upper_sum) = poisson_mixture_tails(
        rate,
        mode,
        (start_lower, start_upper),
        start_log_step,
        |index| {
            let shape = a + index;
            log_x + (shape + b).ln() - (shape + 1.0).ln()
        },
    );
    mixture_tail(lower_sum, upper_sum, lower, log)
}

/// The quantile of the noncentral beta distribution, by bisection on its distribution
/// function over (0, 1).
pub fn qnbeta(p: f64, a: f64, b: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || a.is_nan() || b.is_nan() || ncp.is_nan() {
        return p + a + b + ncp;
    }
    if a <= 0.0 || b <= 0.0 || ncp < 0.0 || !finite(a) || !finite(b) || !finite(ncp) {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, 1.0) {
        return boundary;
    }
    if (ncp / 2.0).floor() > i32::MAX as f64 {
        precision_warning("qnbeta");
        return f64::NAN;
    }
    bisect(0.0, 1.0, 0.0, |x| {
        let value = pnbeta_both(x, 0.5 - x + 0.5, a, b, ncp, lower, log);
        if lower { value < p } else { value > p }
    })
}

/// The point where `below_root` turns from true to false within `(low, high)`, to the
/// resolution of a double. While the bracket spans more than a factor of four, its geometric
/// mean is tried, so a root many orders of magnitude below `high` is found in a number of
/// steps that grows with the number of exponent bits rather than mantissa bits. A positive
/// `tolerance` stops the search once the bracket is narrower than that fraction of its midpoint.
fn bisect(mut low: f64, mut high: f64, tolerance: f64, below_root: impl Fn(f64) -> bool) -> f64 {
    loop {
        if high - low < tolerance * 0.5 * (low + high) {
            return 0.5 * (low + high);
        }
        let middle = if low == 0.0 {
            if high > 0.5 { 0.5 * high } else { high * high }
        } else if high > 4.0 * low {
            (low.sqrt()) * (high.sqrt())
        } else {
            low + 0.5 * (high - low)
        };
        if middle <= low || middle >= high {
            return 0.5 * (low + high);
        }
        if below_root(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
}

pub fn dnf(x: f64, df1: f64, df2: f64, ncp: f64, log: bool) -> f64 {
    if x.is_nan() || df1.is_nan() || df2.is_nan() || ncp.is_nan() {
        return x + df2 + df1 + ncp;
    }
    if df1 <= 0.0 || df2 <= 0.0 || ncp < 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d0(log);
    }
    if !finite(ncp) {
        return f64::NAN;
    }
    if !finite(df1) && !finite(df2) {
        return if x == 1.0 { f64::INFINITY } else { d0(log) };
    }
    if !finite(df2) {
        return df1 * dnchisq(x * df1, df1, ncp, log);
    }
    if df1 > 1e14 && ncp < 1e7 {
        let f = 1.0 + ncp / df1;
        let z = dgamma(1.0 / x / f, df2 / 2.0, 2.0 / df2, log);
        return if log {
            z - 2.0 * x.ln() - f.ln()
        } else {
            z / (x * x) / f
        };
    }
    let y = (df1 / df2) * x;
    let z = dnbeta(y / (1.0 + y), df1 / 2.0, df2 / 2.0, ncp, log);
    if log {
        z + df1.ln() - df2.ln() - 2.0 * y.ln_1p()
    } else {
        z * (df1 / df2) / (1.0 + y) / (1.0 + y)
    }
}

pub fn pnf(x: f64, df1: f64, df2: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || df1.is_nan() || df2.is_nan() || ncp.is_nan() {
        return x + df2 + df1 + ncp;
    }
    if df1 <= 0.0 || df2 <= 0.0 || ncp < 0.0 || !finite(ncp) || (!finite(df1) && !finite(df2)) {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt0(lower, log);
    }
    if x >= f64::INFINITY {
        return dt1(lower, log);
    }
    if df2 > 1e8 {
        return pnchisq(x * df1, df1, ncp, lower, log);
    }
    let y = (df1 / df2) * x;
    pnbeta_both(
        y / (1.0 + y),
        1.0 / (1.0 + y),
        df1 / 2.0,
        df2 / 2.0,
        ncp,
        lower,
        log,
    )
}

pub fn qnf(p: f64, df1: f64, df2: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || df1.is_nan() || df2.is_nan() || ncp.is_nan() {
        return p + df1 + df2 + ncp;
    }
    if df1 <= 0.0 || df2 <= 0.0 || ncp < 0.0 || !finite(ncp) || (!finite(df1) && !finite(df2)) {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    if df2 > 1e8 {
        return qnchisq(p, df1, ncp, lower, log) / df1;
    }
    let y = qnbeta(p, df1 / 2.0, df2 / 2.0, ncp, lower, log);
    y / (1.0 - y) * (df2 / df1)
}

pub fn dnt(x: f64, df: f64, ncp: f64, log: bool) -> f64 {
    if x.is_nan() || df.is_nan() {
        return x + df;
    }
    if df <= 0.0 {
        return f64::NAN;
    }
    if ncp == 0.0 {
        return dt(x, df, log);
    }
    if !finite(x) {
        return d0(log);
    }
    if !finite(df) || df > 1e8 {
        return dnorm(x, ncp, 1.0, log);
    }
    let u = if x.abs() > (df * DBL_EPSILON).sqrt() {
        df.ln() - x.abs().ln()
            + (pnt(x * ((df + 2.0) / df).sqrt(), df + 2.0, ncp, true, false)
                - pnt(x, df, ncp, true, false))
            .abs()
            .ln()
    } else {
        lgammafn((df + 1.0) / 2.0)
            - lgammafn(df / 2.0)
            - (M_LN_SQRT_PI + 0.5 * (df.ln() + ncp * ncp))
    };
    if log { u } else { u.exp() }
}

/// The non-central t distribution function by Lenth's AS 243.
pub fn pnt(t: f64, df: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    const ITRMAX: i32 = 1000;
    const ERRMAX: f64 = 1e-12;
    if df <= 0.0 {
        return f64::NAN;
    }
    if ncp == 0.0 {
        return pt(t, df, lower, log);
    }
    if !finite(t) {
        return if t < 0.0 {
            dt0(lower, log)
        } else {
            dt1(lower, log)
        };
    }
    let (negdel, tt, del);
    if t >= 0.0 {
        negdel = false;
        tt = t;
        del = ncp;
    } else {
        if ncp > 40.0 && (!log || !lower) {
            return dt0(lower, log);
        }
        negdel = true;
        tt = -t;
        del = -ncp;
    }
    if df > 4e5 || del * del > 2.0 * M_LN2 * -(DBL_MIN_EXP as f64) {
        // Abramowitz and Stegun 26.7.10.
        let s = 1.0 / (4.0 * df);
        return pnorm(
            tt * (1.0 - s),
            del,
            (1.0 + tt * tt * 2.0 * s).sqrt(),
            lower != negdel,
            log,
        );
    }
    let mut x = t * t;
    let mut rxb = df / (x + df);
    x /= x + df;
    let mut tnc;
    'series: {
        if x > 0.0 {
            let lambda = del * del;
            let mut p = 0.5 * (-0.5 * lambda).exp();
            if p == 0.0 {
                underflow_warning("pnt");
                range_warning("pnt");
                return dt0(lower, log);
            }
            let mut q = M_SQRT_2DPI * p * del;
            let mut s = 0.5 - p;
            if s < 1e-7 {
                s = -0.5 * (-0.5 * lambda).exp_m1();
            }
            let mut a = 0.5;
            let b = 0.5 * df;
            rxb = rxb.powf(b);
            let albeta = M_LN_SQRT_PI + lgammafn(b) - lgammafn(0.5 + b);
            let mut xodd = pbeta(x, a, b, true, false);
            let mut godd = 2.0 * rxb * (a * x.ln() - albeta).exp();
            tnc = b * x;
            let mut xeven = if tnc < DBL_EPSILON { tnc } else { 1.0 - rxb };
            let mut geven = tnc * rxb;
            tnc = p * xodd + q * xeven;
            for it in 1..=ITRMAX {
                a += 1.0;
                xodd -= godd;
                xeven -= geven;
                godd *= x * (a + b - 1.0) / a;
                geven *= x * (a + b - 0.5) / (a + 0.5);
                p *= lambda / (2 * it) as f64;
                q *= lambda / (2 * it + 1) as f64;
                tnc += p * xodd + q * xeven;
                s -= p;
                if s < -1e-10 {
                    precision_warning("pnt");
                    break 'series;
                }
                if s <= 0.0 && it > 1 {
                    break 'series;
                }
                let errbd = 2.0 * s * (xodd - godd);
                if errbd.abs() < ERRMAX {
                    break 'series;
                }
            }
            convergence_warning("pnt");
        } else {
            tnc = 0.0;
        }
    }
    tnc += pnorm(-del, 0.0, 1.0, true, false);
    let lower = lower != negdel;
    if tnc > 1.0 - 1e-10 && lower {
        precision_warning("pnt{final}");
    }
    dt_val(fmin2(tnc, 1.0), lower, log)
}

pub fn qnt(p: f64, df: f64, ncp: f64, lower: bool, log: bool) -> f64 {
    const ACCU: f64 = 1e-13;
    const EPS: f64 = 1e-11;
    if p.is_nan() || df.is_nan() || ncp.is_nan() {
        return p + df + ncp;
    }
    if df <= 0.0 {
        return f64::NAN;
    }
    if ncp == 0.0 && df >= 1.0 {
        return qt(p, df, lower, log);
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, f64::NEG_INFINITY, f64::INFINITY) {
        return boundary;
    }
    if !finite(df) {
        return qnorm(p, ncp, 1.0, lower, log);
    }
    let p = dt_qiv(p, lower, log);
    if p > 1.0 - DBL_EPSILON {
        return f64::INFINITY;
    }
    let mut pp = fmin2(1.0 - DBL_EPSILON, p * (1.0 + EPS));
    let mut ux = fmax2(1.0, ncp);
    while ux < DBL_MAX && pnt(ux, df, ncp, true, false) < pp {
        ux *= 2.0;
    }
    pp = p * (1.0 - EPS);
    let mut lx = fmin2(-1.0, -ncp);
    while lx > -DBL_MAX && pnt(lx, df, ncp, true, false) > pp {
        lx *= 2.0;
    }
    loop {
        let nx = 0.5 * (lx + ux);
        if pnt(nx, df, ncp, true, false) > p {
            ux = nx;
        } else {
            lx = nx;
        }
        if !((ux - lx) > fmax2(lx.abs(), ux.abs()) * ACCU) {
            break;
        }
    }
    0.5 * (lx + ux)
}

// The studentized range (Copenhaver and Holland 1988).

/// The probability that the range of `cc` standard normal samples is below `w`, raised to
/// the power `rr`, by 12-point Gauss-Legendre quadrature.
fn wprob(w: f64, rr: f64, cc: f64) -> f64 {
    const NLEG: usize = 12;
    const IHALF: usize = 6;
    const C1: f64 = -30.0;
    const C2: f64 = -50.0;
    const C3: f64 = 60.0;
    const BB: f64 = 8.0;
    const WLAR: f64 = 3.0;
    const WINCR1: f64 = 2.0;
    const WINCR2: f64 = 3.0;
    const XLEG: [f64; IHALF] = [
        0.9815606342467192,
        0.9041172563704749,
        0.7699026741943047,
        0.5873179542866175,
        0.3678314989981802,
        0.1252334085114689,
    ];
    const ALEG: [f64; IHALF] = [
        0.04717533638651183,
        0.10693932599531843,
        0.16007832854334622,
        0.20316742672306592,
        0.2334925365383548,
        0.24914704581340277,
    ];
    let qsqz = w * 0.5;
    if qsqz >= BB {
        return 1.0;
    }
    let mut pr_w = 2.0 * pnorm(qsqz, 0.0, 1.0, true, false) - 1.0;
    if pr_w >= (C2 / cc).exp() {
        pr_w = pr_w.powf(cc);
    } else {
        pr_w = 0.0;
    }
    let wincr = if w > WLAR { WINCR1 } else { WINCR2 };
    let mut blb = qsqz;
    let binc = (BB - qsqz) / wincr;
    let mut bub = blb + binc;
    let mut einsum = 0.0;
    let cc1 = cc - 1.0;
    let mut wi = 1.0;
    while wi <= wincr {
        let mut elsum = 0.0;
        let a = 0.5 * (bub + blb);
        let b = 0.5 * (bub - blb);
        for jj in 1..=NLEG {
            let (j, xx) = if IHALF < jj {
                let j = NLEG - jj + 1;
                (j, XLEG[j - 1])
            } else {
                (jj, -XLEG[jj - 1])
            };
            let c = b * xx;
            let ac = a + c;
            let qexpo = ac * ac;
            if qexpo > C3 {
                break;
            }
            let pplus = 2.0 * pnorm(ac, 0.0, 1.0, true, false);
            let pminus = 2.0 * pnorm(ac, w, 1.0, true, false);
            let mut rinsum = pplus * 0.5 - pminus * 0.5;
            if rinsum >= (C1 / cc1).exp() {
                rinsum = (ALEG[j - 1] * (-(0.5 * qexpo)).exp()) * rinsum.powf(cc1);
                elsum += rinsum;
            }
        }
        elsum *= ((2.0 * b) * cc) / M_SQRT_2PI;
        einsum += elsum;
        blb = bub;
        bub += binc;
        wi += 1.0;
    }
    pr_w += einsum;
    if pr_w <= (C1 / rr).exp() {
        return 0.0;
    }
    pr_w = pr_w.powf(rr);
    if pr_w >= 1.0 { 1.0 } else { pr_w }
}

pub fn ptukey(q: f64, rr: f64, cc: f64, df: f64, lower: bool, log: bool) -> f64 {
    const NLEGQ: usize = 16;
    const IHALFQ: usize = 8;
    const EPS1: f64 = -30.0;
    const EPS2: f64 = 1.0e-14;
    const DHAF: f64 = 100.0;
    const DQUAR: f64 = 800.0;
    const DEIGH: f64 = 5000.0;
    const DLARG: f64 = 25000.0;
    const XLEGQ: [f64; IHALFQ] = [
        0.9894009349916499,
        0.9445750230732326,
        0.8656312023878318,
        0.755404408355003,
        0.6178762444026438,
        0.45801677765722737,
        0.2816035507792589,
        0.09501250983763744,
    ];
    const ALEGQ: [f64; IHALFQ] = [
        0.027152459411754096,
        0.062253523938647894,
        0.09515851168249279,
        0.12462897125553388,
        0.14959598881657674,
        0.16915651939500254,
        0.18260341504492358,
        0.1894506104550685,
    ];
    if q.is_nan() || rr.is_nan() || cc.is_nan() || df.is_nan() {
        return f64::NAN;
    }
    if q <= 0.0 {
        return dt0(lower, log);
    }
    if df < 2.0 || rr < 1.0 || cc < 2.0 {
        return f64::NAN;
    }
    if !finite(q) {
        return dt1(lower, log);
    }
    if df > DLARG {
        return dt_val(wprob(q, rr, cc), lower, log);
    }
    let f2 = df * 0.5;
    let mut f2lf = ((f2 * df.ln()) - (df * M_LN2)) - lgammafn(f2);
    let f21 = f2 - 1.0;
    let ff4 = df * 0.25;
    let ulen: f64 = if df <= DHAF {
        1.0
    } else if df <= DQUAR {
        0.5
    } else if df <= DEIGH {
        0.25
    } else {
        0.125
    };
    f2lf += ulen.ln();
    let mut answer = 0.0;
    let mut otsum = 0.0;
    for i in 1..=50 {
        otsum = 0.0;
        let twa1 = (2 * i - 1) as f64 * ulen;
        for jj in 1..=NLEGQ {
            let (j, t1) = if IHALFQ < jj {
                let j = jj - IHALFQ - 1;
                (
                    j,
                    (f2lf + (f21 * (twa1 + (XLEGQ[j] * ulen)).ln()))
                        - (((XLEGQ[j] * ulen) + twa1) * ff4),
                )
            } else {
                let j = jj - 1;
                (
                    j,
                    (f2lf + (f21 * (twa1 - (XLEGQ[j] * ulen)).ln()))
                        + (((XLEGQ[j] * ulen) - twa1) * ff4),
                )
            };
            if t1 >= EPS1 {
                let qsqz = if IHALFQ < jj {
                    q * (((XLEGQ[j] * ulen) + twa1) * 0.5).sqrt()
                } else {
                    q * (((-(XLEGQ[j] * ulen)) + twa1) * 0.5).sqrt()
                };
                let wprb = wprob(qsqz, rr, cc);
                otsum += (wprb * ALEGQ[j]) * t1.exp();
            }
        }
        if i as f64 * ulen >= 1.0 && otsum <= EPS2 {
            break;
        }
        answer += otsum;
    }
    if otsum > EPS2 {
        precision_warning("ptukey");
    }
    if answer > 1.0 {
        answer = 1.0;
    }
    dt_val(answer, lower, log)
}

/// Odeh and Evans's starting value for `qtukey`.
fn qinv(p: f64, c: f64, v: f64) -> f64 {
    const P0: f64 = 0.322232421088;
    const Q0: f64 = 0.993484626060e-01;
    const P1: f64 = -1.0;
    const Q1: f64 = 0.588581570495;
    const P2: f64 = -0.342242088547;
    const Q2: f64 = 0.531103462366;
    const P3: f64 = -0.204231210125;
    const Q3: f64 = 0.103537752850;
    const P4: f64 = -0.453642210148e-04;
    const Q4: f64 = 0.38560700634e-02;
    const C1: f64 = 0.8832;
    const C2: f64 = 0.2368;
    const C3: f64 = 1.214;
    const C4: f64 = 1.208;
    const C5: f64 = 1.4142;
    const VMAX: f64 = 120.0;
    let ps = 0.5 - 0.5 * p;
    let yi = (1.0 / (ps * ps)).ln().sqrt();
    let mut t = yi
        + ((((yi * P4 + P3) * yi + P2) * yi + P1) * yi + P0)
            / ((((yi * Q4 + Q3) * yi + Q2) * yi + Q1) * yi + Q0);
    if v < VMAX {
        t += (t * t * t + t) / v / 4.0;
    }
    let mut q = C1 - C2 * t;
    if v < VMAX {
        q += -C3 / v + C4 * t / v;
    }
    t * (q * (c - 1.0).ln() + C5)
}

pub fn qtukey(p: f64, rr: f64, cc: f64, df: f64, lower: bool, log: bool) -> f64 {
    const EPS: f64 = 0.0001;
    const MAXITER: i32 = 50;
    if p.is_nan() || rr.is_nan() || cc.is_nan() || df.is_nan() {
        return p + rr + cc + df;
    }
    if df < 2.0 || rr < 1.0 || cc < 2.0 {
        return f64::NAN;
    }
    if let Some(boundary) = q_p01_boundaries(p, lower, log, 0.0, f64::INFINITY) {
        return boundary;
    }
    let p = dt_qiv(p, lower, log);
    let mut x0 = qinv(p, cc, df);
    let mut valx0 = ptukey(x0, rr, cc, df, true, false) - p;
    let mut x1 = if valx0 > 0.0 {
        fmax2(0.0, x0 - 1.0)
    } else {
        x0 + 1.0
    };
    let mut valx1 = ptukey(x1, rr, cc, df, true, false) - p;
    let mut answer = 0.0;
    for _ in 1..MAXITER {
        answer = x1 - ((valx1 * (x1 - x0)) / (valx1 - valx0));
        valx0 = valx1;
        x0 = x1;
        if answer < 0.0 {
            answer = 0.0;
        }
        valx1 = ptukey(answer, rr, cc, df, true, false) - p;
        x1 = answer;
        if (x1 - x0).abs() < EPS {
            return answer;
        }
    }
    convergence_warning("qtukey");
    answer
}

// The Wilcoxon rank sum and signed rank statistics, by counting.

/// The largest counting table the Wilcoxon distributions build, in cells. GNU R fails its
/// allocation somewhere above this; an unchecked size would abort the process instead.
const MAX_COUNT_CELLS: f64 = (1u64 << 27) as f64;

thread_local! {
    /// The last Mann-Whitney table, because vectorised calls repeat the same `m` and `n` and
    /// a table costs `O(m n (m + n))` to build.
    static WILCOX_CACHE: RefCell<Option<((usize, usize), Rc<[f64]>)>> = const { RefCell::new(None) };
}

/// The number of ways the Mann-Whitney statistic of samples of sizes `m` and `n` takes each
/// value from 0 to `m n`: the coefficients of the Gaussian binomial `[m + n choose m]_q`,
/// built by Pascal's rule `[N, k] = [N-1, k-1] + q^k [N-1, k]`, which only adds counts.
fn wilcox_counts(m: usize, n: usize) -> Rc<[f64]> {
    let key = (m.min(n), m.max(n));
    if let Some(counts) = WILCOX_CACHE.with_borrow(|cache| {
        cache
            .as_ref()
            .filter(|(cached, _)| *cached == key)
            .map(|(_, counts)| Rc::clone(counts))
    }) {
        return counts;
    }
    let small = key.0;
    let mut polynomials: Vec<Vec<f64>> = vec![Vec::new(); small + 1];
    polynomials[0] = vec![1.0];
    for total in 1..=m + n {
        for k in (1..=small.min(total)).rev() {
            let mut next = vec![0.0; k * (total - k) + 1];
            for (power, count) in polynomials[k - 1].iter().enumerate() {
                next[power] += count;
            }
            for (power, count) in polynomials[k].iter().enumerate() {
                next[power + k] += count;
            }
            polynomials[k] = next;
        }
    }
    let counts: Rc<[f64]> = polynomials.swap_remove(small).into();
    WILCOX_CACHE.with_borrow_mut(|cache| *cache = Some((key, Rc::clone(&counts))));
    counts
}

/// The sample sizes as table dimensions, or `None` (a `NaN` result) when they are not
/// positive, not finite, or need a table larger than `MAX_COUNT_CELLS`.
fn wilcox_valid(m: f64, n: f64) -> Option<(usize, usize)> {
    let m = forceint(m);
    let n = forceint(n);
    if !(m > 0.0 && n > 0.0 && m.is_finite() && n.is_finite()) {
        return None;
    }
    let small = m.min(n);
    if small * small * (m + n) > MAX_COUNT_CELLS {
        warning(format!(
            "wilcox: samples of sizes {m} and {n} are too large"
        ));
        return None;
    }
    Some((m as usize, n as usize))
}

pub fn dwilcox(x: f64, m: f64, n: f64, log: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    let Some((mm, nn)) = wilcox_valid(m, n) else {
        return f64::NAN;
    };
    let (m, n) = (forceint(m), forceint(n));
    if (x - forceint(x)).abs() > 1e-7 {
        return d0(log);
    }
    let x = forceint(x);
    if x < 0.0 || x > m * n {
        return d0(log);
    }
    let count = wilcox_counts(mm, nn)[x as usize];
    if log {
        count.ln() - lchoose(m + n, n)
    } else {
        count / choose(m + n, n)
    }
}

pub fn pwilcox(q: f64, m: f64, n: f64, lower: bool, log: bool) -> f64 {
    if q.is_nan() || m.is_nan() || n.is_nan() {
        return q + m + n;
    }
    if !finite(m) || !finite(n) {
        return f64::NAN;
    }
    let Some((mm, nn)) = wilcox_valid(m, n) else {
        return f64::NAN;
    };
    let (m, n) = (forceint(m), forceint(n));
    let q = (q + 1e-7).floor();
    if q < 0.0 {
        return dt0(lower, log);
    }
    if q >= m * n {
        return dt1(lower, log);
    }
    let counts = wilcox_counts(mm, nn);
    let c = choose(m + n, n);
    let mut p = 0.0;
    let mut lower = lower;
    if q <= m * n / 2.0 {
        for count in &counts[..=q as usize] {
            p += count / c;
        }
    } else {
        let q = m * n - q;
        for count in &counts[..q as usize] {
            p += count / c;
        }
        lower = !lower;
    }
    dt_val(p, lower, log)
}

pub fn qwilcox(x: f64, m: f64, n: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    if !finite(x) || !finite(m) || !finite(n) || q_p01_invalid(x, log) {
        return f64::NAN;
    }
    let Some((mm, nn)) = wilcox_valid(m, n) else {
        return f64::NAN;
    };
    let (m, n) = (forceint(m), forceint(n));
    if x == dt0(lower, log) {
        return 0.0;
    }
    if x == dt1(lower, log) {
        return m * n;
    }
    let mut x = if log || !lower {
        dt_qiv(x, lower, log)
    } else {
        x
    };
    let counts = wilcox_counts(mm, nn);
    let c = choose(m + n, n);
    let mut p = 0.0;
    let mut q = 0usize;
    if x <= 0.5 {
        x -= 10.0 * DBL_EPSILON;
        while q + 1 < counts.len() {
            p += counts[q] / c;
            if p >= x {
                break;
            }
            q += 1;
        }
        q as f64
    } else {
        x = 1.0 - x + 10.0 * DBL_EPSILON;
        while q + 1 < counts.len() {
            p += counts[q] / c;
            if p > x {
                break;
            }
            q += 1;
        }
        m * n - q as f64
    }
}

/// Whether `signrank_counts(n)` fits in `MAX_COUNT_CELLS`; a larger `n` gives `NaN`.
fn signrank_valid(n: f64) -> bool {
    if n.is_finite() && n * (n + 1.0) / 2.0 <= MAX_COUNT_CELLS {
        return true;
    }
    warning(format!("signrank: n = {n} is too large"));
    false
}

/// The number of subsets of `1..=n` with each sum from 0 to `n (n + 1) / 2`.
fn signrank_counts(n: usize) -> Vec<f64> {
    debug_assert!(signrank_valid(n as f64));
    let total = n * (n + 1) / 2;
    let mut counts = vec![0.0; total + 1];
    counts[0] = 1.0;
    let mut reach = 0;
    for j in 1..=n {
        reach += j;
        for i in (j..=reach).rev() {
            counts[i] += counts[i - j];
        }
    }
    counts
}

pub fn dsignrank(x: f64, n: f64, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    let n = forceint(n);
    if n <= 0.0 || !signrank_valid(n) {
        return f64::NAN;
    }
    if (x - forceint(x)).abs() > 1e-7 {
        return d0(log);
    }
    let x = forceint(x);
    if x < 0.0 || x > n * (n + 1.0) / 2.0 {
        return d0(log);
    }
    let count = signrank_counts(n as usize)[x as usize];
    d_exp(count.ln() - n * M_LN2, log)
}

pub fn psignrank(x: f64, n: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if !finite(n) {
        return f64::NAN;
    }
    let n = forceint(n);
    if n <= 0.0 || !signrank_valid(n) {
        return f64::NAN;
    }
    let x = forceint(x + 1e-7);
    if x < 0.0 {
        return dt0(lower, log);
    }
    if x >= n * (n + 1.0) / 2.0 {
        return dt1(lower, log);
    }
    let counts = signrank_counts(n as usize);
    let f = (-n * M_LN2).exp();
    let mut p = 0.0;
    let mut lower = lower;
    if x <= n * (n + 1.0) / 4.0 {
        for count in &counts[..=x as usize] {
            p += count * f;
        }
    } else {
        let x = n * (n + 1.0) / 2.0 - x;
        for count in &counts[..x as usize] {
            p += count * f;
        }
        lower = !lower;
    }
    dt_val(p, lower, log)
}

pub fn qsignrank(x: f64, n: f64, lower: bool, log: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if !finite(x) || !finite(n) || q_p01_invalid(x, log) {
        return f64::NAN;
    }
    let n = forceint(n);
    if n <= 0.0 || !signrank_valid(n) {
        return f64::NAN;
    }
    if x == dt0(lower, log) {
        return 0.0;
    }
    if x == dt1(lower, log) {
        return n * (n + 1.0) / 2.0;
    }
    let mut x = if log || !lower {
        dt_qiv(x, lower, log)
    } else {
        x
    };
    let counts = signrank_counts(n as usize);
    let f = (-n * M_LN2).exp();
    let mut p = 0.0;
    let mut q = 0usize;
    if x <= 0.5 {
        x -= 10.0 * DBL_EPSILON;
        while q + 1 < counts.len() {
            p += counts[q] * f;
            if p >= x {
                break;
            }
            q += 1;
        }
        q as f64
    } else {
        x = 1.0 - x + 10.0 * DBL_EPSILON;
        while q + 1 < counts.len() {
            p += counts[q] * f;
            if p > x {
                break;
            }
            q += 1;
        }
        n * (n + 1.0) / 2.0 - q as f64
    }
}

// Bessel functions. I and K come together from Temme's series (x < 2) or Steed's continued
// fraction (x >= 2) for K at an order in [-1/2, 1/2], the continued fraction for I'/I, and
// the Wronskian; J and Y likewise (Temme 1975, Barnett and Thompson 1986). The scaled forms
// exp(-x) I and exp(x) K come out directly, so large arguments do not overflow on the way.

/// The Taylor coefficients of `1 / gamma(1 + x)` at zero.
const RECIPROCAL_GAMMA: [f64; 27] = [
    1.0,
    0.5772156649015329,
    -0.6558780715202539,
    -0.04200263503409524,
    0.16653861138229148,
    -0.04219773455554433,
    -0.009621971527876973,
    0.0072189432466631,
    -0.0011651675918590652,
    -0.00021524167411495098,
    0.0001280502823881162,
    -2.013485478078824e-05,
    -1.2504934821426706e-06,
    1.133027231981696e-06,
    -2.056338416977607e-07,
    6.116095104481416e-09,
    5.002007644469223e-09,
    -1.18127457048702e-09,
    1.0434267116911005e-10,
    7.782263439905071e-12,
    -3.696805618642206e-12,
    5.100370287454476e-13,
    -2.0583260535665066e-14,
    -5.348122539423018e-15,
    1.2267786282382608e-15,
    -1.1812593016974588e-16,
    1.1866922547516004e-18,
];

/// Temme's auxiliary functions for `|mu| <= 1/2`: `(1/gamma(1-mu) - 1/gamma(1+mu)) / (2 mu)`,
/// `(1/gamma(1-mu) + 1/gamma(1+mu)) / 2`, `1/gamma(1+mu)` and `1/gamma(1-mu)`.
fn temme_gammas(mu: f64) -> (f64, f64, f64, f64) {
    let mut odd = 0.0;
    let mut even = 0.0;
    let mut power = 1.0;
    for (k, coefficient) in RECIPROCAL_GAMMA.iter().enumerate() {
        if k % 2 == 0 {
            even += coefficient * power;
        } else {
            odd += coefficient * power;
        }
        if k % 2 == 1 {
            power *= mu * mu;
        }
    }
    // 1/gamma(1+mu) = even + mu * odd, 1/gamma(1-mu) = even - mu * odd.
    let plus = even + mu * odd;
    let minus = even - mu * odd;
    (-odd, even, plus, minus)
}

const BESSEL_EPS: f64 = 1e-16;
const BESSEL_FPMIN: f64 = 1e-300;
const BESSEL_MAXIT: usize = 10_000_000;
/// Downward recurrences rescale past this magnitude so that a high order does not overflow.
const BESSEL_BIG: f64 = 1e250;

/// `exp(-x) I_nu(x)` and `exp(x) K_nu(x)` for `x > 0` and `nu >= 0`, by the asymptotic
/// expansion where `x` is large against `nu^2` and by Temme's method elsewhere.
fn bessel_ik_scaled(x: f64, nu: f64) -> (f64, f64) {
    if x >= 50.0 && x >= 2.0 * nu * nu {
        return bessel_ik_asymptotic(x, nu);
    }
    bessel_ik_temme(x, nu)
}

/// `exp(-x) I_nu(x) ~ (2 pi x)^(-1/2) sum (-1)^k a_k / x^k` and
/// `exp(x) K_nu(x) ~ (pi / (2 x))^(1/2) sum a_k / x^k`, with
/// `a_k = prod_{j <= k} (4 nu^2 - (2j - 1)^2) / (k! 8^k)`.
fn bessel_ik_asymptotic(x: f64, nu: f64) -> (f64, f64) {
    let mu = 4.0 * nu * nu;
    let z8 = 8.0 * x;
    let mut term: f64 = 1.0;
    let mut i_sum = 1.0;
    let mut k_sum = 1.0;
    let mut k = 1.0;
    loop {
        let next = term * (mu - (2.0 * k - 1.0) * (2.0 * k - 1.0)) / (k * z8);
        if next.abs() > term.abs() || next == 0.0 {
            break;
        }
        term = next;
        k_sum += term;
        i_sum += if (k as i64) % 2 == 1 { -term } else { term };
        if term.abs() < DBL_EPSILON * 1e-3 {
            break;
        }
        k += 1.0;
    }
    (
        i_sum / (2.0 * M_PI * x).sqrt(),
        k_sum * (M_PI / (2.0 * x)).sqrt(),
    )
}

fn bessel_ik_temme(x: f64, nu: f64) -> (f64, f64) {
    let nl = (nu + 0.5) as usize;
    let xmu = nu - nl as f64;
    let xmu2 = xmu * xmu;
    let xi = 1.0 / x;
    let xi2 = 2.0 * xi;
    let mut h = nu * xi;
    if h < BESSEL_FPMIN {
        h = BESSEL_FPMIN;
    }
    let mut b = xi2 * nu;
    let mut d = 0.0;
    let mut c = h;
    for _ in 0..BESSEL_MAXIT {
        b += xi2;
        d = 1.0 / (b + d);
        c = b + 1.0 / c;
        let del = c * d;
        h *= del;
        // At tiny x the terms overflow; callers then use only the parts that do not need
        // the ratio.
        if !((del - 1.0).abs() >= BESSEL_EPS) {
            break;
        }
    }
    let mut ril = BESSEL_FPMIN;
    let mut ripl = h * ril;
    let mut ril1 = ril;
    let mut fact = nu * xi;
    for _ in 0..nl {
        let ritemp = fact * ril + ripl;
        fact -= xi;
        ripl = fact * ritemp + ril;
        ril = ritemp;
        if ril.abs() > BESSEL_BIG {
            ril /= BESSEL_BIG;
            ripl /= BESSEL_BIG;
            ril1 /= BESSEL_BIG;
        }
    }
    let f = ripl / ril;
    let (rkmu, rk1);
    if x < 2.0 {
        let x2 = 0.5 * x;
        let pimu = M_PI * xmu;
        let fact = if pimu.abs() < BESSEL_EPS {
            1.0
        } else {
            pimu / pimu.sin()
        };
        let d = -x2.ln();
        let e = xmu * d;
        let fact2 = if e.abs() < BESSEL_EPS {
            1.0
        } else {
            e.sinh() / e
        };
        let (gam1, gam2, gampl, gammi) = temme_gammas(xmu);
        let mut ff = fact * (gam1 * e.cosh() + gam2 * fact2 * d);
        let mut sum = ff;
        let e = e.exp();
        let mut p = 0.5 * e / gampl;
        let mut q = 0.5 / (e * gammi);
        let mut c = 1.0;
        let d = x2 * x2;
        let mut sum1 = p;
        let mut i = 1.0;
        loop {
            ff = (i * ff + p + q) / (i * i - xmu2);
            c *= d / i;
            p /= i - xmu;
            q /= i + xmu;
            let del = c * ff;
            sum += del;
            sum1 += c * (p - i * ff);
            if del.abs() < sum.abs() * BESSEL_EPS || i > 1000.0 {
                break;
            }
            i += 1.0;
        }
        let scale = x.exp();
        rkmu = sum * scale;
        rk1 = sum1 * xi2 * scale;
    } else {
        let mut b = 2.0 * (1.0 + x);
        let mut d = 1.0 / b;
        let mut h = d;
        let mut delh = d;
        let mut q1 = 0.0;
        let mut q2 = 1.0;
        let a1 = 0.25 - xmu2;
        let mut q = a1;
        let mut c = a1;
        let mut a = -a1;
        let mut s = 1.0 + q * delh;
        for i in 2..BESSEL_MAXIT {
            a -= 2.0 * (i - 1) as f64;
            c = -a * c / i as f64;
            let qnew = (q1 - b * q2) / a;
            q1 = q2;
            q2 = qnew;
            q += c * qnew;
            b += 2.0;
            d = 1.0 / (b + a * d);
            delh *= b * d - 1.0;
            h += delh;
            let dels = q * delh;
            s += dels;
            if (dels / s).abs() < BESSEL_EPS {
                break;
            }
        }
        h *= a1;
        rkmu = (M_PI / (2.0 * x)).sqrt() / s;
        rk1 = rkmu * (xmu + x + 0.5 - h) * xi;
    }
    let rkmup = xmu * xi * rkmu - rk1;
    let rimu = xi / (f * rkmu - rkmup);
    let rinu = (rimu * ril1) / ril;
    let mut rkmu = rkmu;
    let mut rk1 = rk1;
    for i in 1..=nl {
        let rktemp = (xmu + i as f64) * xi2 * rk1 + rkmu;
        rkmu = rk1;
        rk1 = rktemp;
    }
    (rinu, rkmu)
}

/// `J_nu(x)` and `Y_nu(x)` for `x > 0` and `nu >= 0`.
fn bessel_jy(x: f64, nu: f64) -> (f64, f64) {
    let nl = if x < 2.0 {
        (nu + 0.5) as usize
    } else {
        (nu - x + 1.5).max(0.0) as usize
    };
    let xmu = nu - nl as f64;
    let xmu2 = xmu * xmu;
    let xi = 1.0 / x;
    let xi2 = 2.0 * xi;
    let w = xi2 / M_PI;
    let mut isign = 1.0;
    let mut h = nu * xi;
    if h < BESSEL_FPMIN {
        h = BESSEL_FPMIN;
    }
    let mut b = xi2 * nu;
    let mut d = 0.0;
    let mut c = h;
    for _ in 0..BESSEL_MAXIT {
        b += xi2;
        d = b - d;
        if d.abs() < BESSEL_FPMIN {
            d = BESSEL_FPMIN;
        }
        c = b - 1.0 / c;
        if c.abs() < BESSEL_FPMIN {
            c = BESSEL_FPMIN;
        }
        d = 1.0 / d;
        let del = c * d;
        h *= del;
        if d < 0.0 {
            isign = -isign;
        }
        if !((del - 1.0).abs() >= BESSEL_EPS) {
            break;
        }
    }
    let mut rjl = isign * BESSEL_FPMIN;
    let mut rjpl = h * rjl;
    let mut rjl1 = rjl;
    let mut fact = nu * xi;
    for _ in 0..nl {
        let rjtemp = fact * rjl + rjpl;
        fact -= xi;
        rjpl = fact * rjtemp - rjl;
        rjl = rjtemp;
        if rjl.abs() > BESSEL_BIG {
            rjl /= BESSEL_BIG;
            rjpl /= BESSEL_BIG;
            rjl1 /= BESSEL_BIG;
        }
    }
    if rjl == 0.0 {
        rjl = BESSEL_EPS;
    }
    let f = rjpl / rjl;
    let (rjmu, mut rymu, mut ry1);
    if x < 2.0 {
        let x2 = 0.5 * x;
        let pimu = M_PI * xmu;
        let fact = if pimu.abs() < BESSEL_EPS {
            1.0
        } else {
            pimu / pimu.sin()
        };
        let d = -x2.ln();
        let e = xmu * d;
        let fact2 = if e.abs() < BESSEL_EPS {
            1.0
        } else {
            e.sinh() / e
        };
        let (gam1, gam2, gampl, gammi) = temme_gammas(xmu);
        let mut ff = 2.0 / M_PI * fact * (gam1 * e.cosh() + gam2 * fact2 * d);
        let e = e.exp();
        let mut p = e / (gampl * M_PI);
        let mut q = 1.0 / (e * M_PI * gammi);
        let pimu2 = 0.5 * pimu;
        let fact3 = if pimu2.abs() < BESSEL_EPS {
            1.0
        } else {
            pimu2.sin() / pimu2
        };
        let r = M_PI * pimu2 * fact3 * fact3;
        let mut c = 1.0;
        let d = -x2 * x2;
        let mut sum = ff + r * q;
        let mut sum1 = p;
        let mut i = 1.0;
        loop {
            ff = (i * ff + p + q) / (i * i - xmu2);
            c *= d / i;
            p /= i - xmu;
            q /= i + xmu;
            let del = c * (ff + r * q);
            sum += del;
            let del1 = c * p - i * del;
            sum1 += del1;
            if del.abs() < (1.0 + sum.abs()) * BESSEL_EPS || i > 1000.0 {
                break;
            }
            i += 1.0;
        }
        rymu = -sum;
        ry1 = -sum1 * xi2;
        let rymup = xmu * xi * rymu - ry1;
        rjmu = w / (rymup - f * rymu);
    } else {
        let mut a = 0.25 - xmu2;
        let mut p = -0.5 * xi;
        let mut q = 1.0;
        let br = 2.0 * x;
        let mut bi = 2.0;
        let mut fact = a * xi / (p * p + q * q);
        let mut cr = br + q * fact;
        let mut ci = bi + p * fact;
        let mut den = br * br + bi * bi;
        let mut dr = br / den;
        let mut di = -bi / den;
        let mut dlr = cr * dr - ci * di;
        let mut dli = cr * di + ci * dr;
        let mut temp = p * dlr - q * dli;
        q = p * dli + q * dlr;
        p = temp;
        for i in 2..BESSEL_MAXIT {
            a += 2.0 * (i - 1) as f64;
            bi += 2.0;
            dr = a * dr + br;
            di = a * di + bi;
            if dr.abs() + di.abs() < BESSEL_FPMIN {
                dr = BESSEL_FPMIN;
            }
            fact = a / (cr * cr + ci * ci);
            cr = br + cr * fact;
            ci = bi - ci * fact;
            if cr.abs() + ci.abs() < BESSEL_FPMIN {
                cr = BESSEL_FPMIN;
            }
            den = dr * dr + di * di;
            dr /= den;
            di /= -den;
            dlr = cr * dr - ci * di;
            dli = cr * di + ci * dr;
            temp = p * dlr - q * dli;
            q = p * dli + q * dlr;
            p = temp;
            if (dlr - 1.0).abs() + dli.abs() < BESSEL_EPS {
                break;
            }
        }
        let gam = (p - f) / q;
        let magnitude = (w / ((p - f) * gam + q)).sqrt();
        rjmu = if rjl < 0.0 { -magnitude } else { magnitude };
        rymu = rjmu * gam;
        let rymup = rymu * (p + q / gam);
        ry1 = xmu * xi * rymu - rymup;
    }
    let fact = rjmu / rjl;
    let rj = rjl1 * fact;
    for i in 1..=nl {
        let rytemp = (xmu + i as f64) * xi2 * ry1 - rymu;
        rymu = ry1;
        ry1 = rytemp;
        if !rymu.is_finite() {
            break;
        }
    }
    (rj, rymu)
}

/// GNU R stops at orders this large, where its recurrences would need too much memory.
const BESSEL_NU_MAX: f64 = 1e7;
/// Above this argument, unscaled `I` is reported as infinite, as GNU R does.
const BESSEL_EXPARG: f64 = 709.0;

pub fn bessel_i(x: f64, alpha: f64, expo: f64) -> f64 {
    if x.is_nan() || alpha.is_nan() {
        return x + alpha;
    }
    if x < 0.0 {
        range_warning("bessel_i");
        return f64::NAN;
    }
    let scaled = expo as i32 == 2;
    if alpha < 0.0 {
        let na = alpha.floor();
        // Abramowitz and Stegun 9.6.2 and 9.6.6.
        return bessel_i(x, -alpha, expo)
            + if alpha == na {
                0.0
            } else {
                bessel_k(x, -alpha, expo) * if scaled { 2.0 * (-2.0 * x).exp() } else { 2.0 } / M_PI
                    * sinpi_(-alpha)
            };
    }
    if x == 0.0 {
        return if alpha == 0.0 { 1.0 } else { 0.0 };
    }
    if !scaled && x > BESSEL_EXPARG {
        return f64::INFINITY;
    }
    if x == f64::INFINITY {
        return if scaled { 0.0 } else { f64::INFINITY };
    }
    if x <= 15.0 || x * x <= 50.0 * (alpha + 1.0) {
        let i = bessel_i_series(x, alpha);
        return if scaled { i * (-x).exp() } else { i };
    }
    let (i, _) = bessel_ik_scaled(x, alpha);
    if scaled { i } else { i * x.exp() }
}

/// `I_nu(x)` by its power series, whose terms are all positive. The Wronskian route loses
/// digits to cancellation at small `x`.
fn bessel_i_series(x: f64, nu: f64) -> f64 {
    let half = 0.5 * x;
    let mut prefactor = 0.0;
    if nu + 1.0 < 170.0 {
        prefactor = half.powf(nu) / gammafn(nu + 1.0);
    }
    if !(prefactor.is_finite() && prefactor >= DBL_MIN) {
        prefactor = (nu * half.ln() - lgammafn(nu + 1.0)).exp();
    }
    let quarter_square = half * half;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    loop {
        term *= quarter_square / (k * (nu + k));
        sum += term;
        if !(term >= DBL_EPSILON * 0.25 * sum) {
            break;
        }
        k += 1.0;
    }
    prefactor * sum
}

pub fn bessel_k(x: f64, alpha: f64, expo: f64) -> f64 {
    if x.is_nan() || alpha.is_nan() {
        return x + alpha;
    }
    if x < 0.0 {
        range_warning("bessel_k");
        return f64::NAN;
    }
    let alpha = alpha.abs();
    let scaled = expo as i32 == 2;
    if x == 0.0 {
        return f64::INFINITY;
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    let (_, k) = bessel_ik_scaled(x, alpha);
    if scaled { k } else { k * (-x).exp() }
}

pub fn bessel_j(x: f64, alpha: f64) -> f64 {
    if x.is_nan() || alpha.is_nan() {
        return x + alpha;
    }
    if x < 0.0 {
        range_warning("bessel_j");
        return f64::NAN;
    }
    let na = alpha.floor();
    if alpha < 0.0 {
        // Abramowitz and Stegun 9.1.2.
        return (if alpha - na == 0.5 {
            0.0
        } else {
            bessel_j(x, -alpha) * cospi_(alpha)
        }) + if alpha == na {
            0.0
        } else {
            bessel_y(x, -alpha) * sinpi_(alpha)
        };
    }
    if alpha > BESSEL_NU_MAX {
        warning(format!(
            "besselJ(x, nu): nu={alpha} too large for bessel_j() algorithm"
        ));
        return f64::NAN;
    }
    if x == 0.0 {
        return if alpha == 0.0 { 1.0 } else { 0.0 };
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    bessel_jy_any(x, alpha).0
}

/// `J_nu(x)` and `Y_nu(x)`. Below `x = 20` the continued fractions of `bessel_jy` serve,
/// with the power series for `J` where it has no cancellation. From 20 on, Hankel's
/// expansion gives the two orders nearest zero exactly, and the three-term recurrence carries
/// them up: upwards for `Y` always, and for `J` while the order stays below `x`, where the
/// recurrence neither grows nor damps errors. Above `x`, `J` recurs downwards from Lentz's
/// continued fraction for `J_nu / J_(nu-1)` and is normalized where the two ranges meet.
fn bessel_jy_any(x: f64, nu: f64) -> (f64, f64) {
    if x < 20.0 {
        let (j, y) = bessel_jy(x, nu);
        if x * x < 2.0 * (nu + 1.0) {
            return (bessel_j_series(x, nu), y);
        }
        return (j, y);
    }
    let n = nu.floor() as usize;
    let mu = nu - n as f64;
    let (j0, y0) = bessel_jy_hankel(x, mu);
    let (j1, y1) = bessel_jy_hankel(x, mu + 1.0);
    let upward = |mut previous: f64, mut current: f64, steps: usize| {
        if steps == 0 {
            return (previous, current);
        }
        for k in 1..steps {
            let next = 2.0 * (mu + k as f64) / x * current - previous;
            previous = current;
            current = next;
            if !current.is_finite() {
                break;
            }
        }
        (previous, current)
    };
    let y = if n == 0 { y0 } else { upward(y0, y1, n).1 };
    if nu <= x {
        let j = if n == 0 { j0 } else { upward(j0, j1, n).1 };
        return (j, y);
    }
    // The meeting point m: the largest order mu + m not above x.
    let m = (x - mu).floor() as usize;
    let (jm, jm1) = if m == 0 {
        (j0, j1)
    } else {
        upward(j0, j1, m + 1)
    };
    // J_nu / J_(nu-1) = 1 / (2 nu/x - 1 / (2 (nu+1)/x - ...)) by modified Lentz.
    let mut f = 2.0 * nu / x;
    if f == 0.0 {
        f = BESSEL_FPMIN;
    }
    let mut c = f;
    let mut d = 0.0;
    let mut k = nu + 1.0;
    for _ in 0..BESSEL_MAXIT {
        let b = 2.0 * k / x;
        d = b - d;
        if d.abs() < BESSEL_FPMIN {
            d = BESSEL_FPMIN;
        }
        c = b - 1.0 / c;
        if c.abs() < BESSEL_FPMIN {
            c = BESSEL_FPMIN;
        }
        d = 1.0 / d;
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < BESSEL_EPS {
            break;
        }
        k += 1.0;
    }
    let ratio = 1.0 / f;
    // Unnormalized J from order nu down to mu + m, rescaled so it cannot overflow; `top` is
    // the (rescaled) value at order nu.
    let mut top = 1.0;
    let mut upper = 1.0;
    let mut lower = 1.0 / ratio;
    let mut order = nu - 1.0;
    while order > mu + m as f64 + 0.5 {
        let next = 2.0 * order / x * lower - upper;
        upper = lower;
        lower = next;
        order -= 1.0;
        if lower.abs() > BESSEL_BIG {
            lower /= BESSEL_BIG;
            upper /= BESSEL_BIG;
            top /= BESSEL_BIG;
        }
    }
    // The least-squares fit of the computed pair to the true one, scaled so no square
    // overflows.
    let scale = lower.abs().max(upper.abs());
    let (lower, upper) = (lower / scale, upper / scale);
    let fact = (jm * lower + jm1 * upper) / (lower * lower + upper * upper);
    (top / scale * fact, y)
}

/// `J_nu(x)` by its power series, for `x^2 / 4` small against `nu + 1`, where the terms
/// shrink from the first.
fn bessel_j_series(x: f64, nu: f64) -> f64 {
    let half = 0.5 * x;
    let mut prefactor = 0.0;
    if nu + 1.0 < 170.0 {
        prefactor = half.powf(nu) / gammafn(nu + 1.0);
    }
    if !(prefactor.is_finite() && prefactor >= DBL_MIN) {
        prefactor = (nu * half.ln() - lgammafn(nu + 1.0)).exp();
    }
    let quarter_square = half * half;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    loop {
        term *= -quarter_square / (k * (nu + k));
        sum += term;
        if !(term.abs() >= DBL_EPSILON * 0.25 * sum.abs()) {
            break;
        }
        k += 1.0;
    }
    prefactor * sum
}

/// Hankel's asymptotic expansion of `J_nu(x)` and `Y_nu(x)` for `x` large against `nu^2`.
fn bessel_jy_hankel(x: f64, nu: f64) -> (f64, f64) {
    let mu = 4.0 * nu * nu;
    let z8 = 8.0 * x;
    let mut p = 1.0;
    let mut q = 0.0;
    let mut term: f64 = 1.0;
    let mut k = 1.0;
    loop {
        let next = term * (mu - (2.0 * k - 1.0) * (2.0 * k - 1.0)) / (k * z8);
        if next.abs() > term.abs() || next == 0.0 {
            break;
        }
        term = next;
        // t_k contributes to Q for odd k and to P for even k, with alternating signs.
        match (k as i64) % 4 {
            1 => q += term,
            2 => p -= term,
            3 => q -= term,
            _ => p += term,
        }
        if term.abs() < DBL_EPSILON * 1e-3 {
            break;
        }
        k += 1.0;
    }
    // chi = x - (nu/2 + 1/4) pi, expanded so that only x needs argument reduction.
    let phase = nu / 2.0 + 0.25;
    let (cos_phase, sin_phase) = (cospi_(phase), sinpi_(phase));
    let (sin_x, cos_x) = x.sin_cos();
    let cos_chi = cos_x * cos_phase + sin_x * sin_phase;
    let sin_chi = sin_x * cos_phase - cos_x * sin_phase;
    let amplitude = (2.0 / (M_PI * x)).sqrt();
    (
        amplitude * (p * cos_chi - q * sin_chi),
        amplitude * (p * sin_chi + q * cos_chi),
    )
}

pub fn bessel_y(x: f64, alpha: f64) -> f64 {
    if x.is_nan() || alpha.is_nan() {
        return x + alpha;
    }
    if x < 0.0 {
        range_warning("bessel_y");
        return f64::NAN;
    }
    let na = alpha.floor();
    if alpha < 0.0 {
        // Abramowitz and Stegun 9.1.2.
        return (if alpha - na == 0.5 {
            0.0
        } else {
            bessel_y(x, -alpha) * cospi_(alpha)
        }) - if alpha == na {
            0.0
        } else {
            bessel_j(x, -alpha) * sinpi_(alpha)
        };
    }
    if alpha > BESSEL_NU_MAX {
        warning(format!(
            "besselY(x, nu): nu={alpha} too large for bessel_y() algorithm"
        ));
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    bessel_jy_any(x, alpha).1
}
