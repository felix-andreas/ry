// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's C sources, and keep
// their copyright notices:
//
// GNU R: src/nmath/toms708.c (bratio and its helpers, grat_r)
//     Based on the C translation of ACM TOMS 708 (Didonato and Morris, 1992), with
//     changes by The R Core Team; the file carries no copyright line of its own
//
// GNU R: src/nmath/pnorm.c (pnorm_both)
//     Copyright (C) 2000-2020 The R Core Team
//     Copyright (C) 2003 The R Foundation
//     Copyright (C) 1998 Ross Ihaka
//
// GNU R: src/nmath/qnorm.c (qnorm)
//     Copyright (C) 2000--2023 The R Core Team
//     Copyright (C) 1998 Ross Ihaka
//     based on AS 241 (C) 1988 Royal Statistical Society
//
// GNU R: src/nmath/ptukey.c (wprob, ptukey)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000--2007 The R Core Team
//
// GNU R: src/nmath/qtukey.c (qinv, qtukey)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000--2005 The R Core Team
//     based in part on AS70 (C) 1974 Royal Statistical Society
//
// GNU R: src/nmath/pnt.c (pnt)
//     Copyright (C) 1998-2015 The R Core Team
//     based on AS243 (C) 1989 Royal Statistical Society
//
// GNU R: src/nmath/pt.c (pt)
//     Copyright (C) 1995, 1996 Robert Gentleman and Ross Ihaka
//     Copyright (C) 2000-2007 The R Core Team
//
// GNU R: src/nmath/qt.c (qt)
//     Copyright (C) 2000-2022 The R Core Team
//     Copyright (C) 2003-2022 The R Foundation
//     Copyright (C) 1998 Ross Ihaka
//
// GNU R: src/nmath/qbeta.c (qbeta_raw)
//     Copyright (C) 1998--2022 The R Core Team
//     Copyright (C) 1995, 1996 Robert Gentleman and Ross Ihaka
//     based on code (C) 1979 and later Royal Statistical Society
//
// GNU R: src/nmath/chebyshev.c (chebyshev_eval)
//     Copyright (C) 1998 Ross Ihaka
//
// GNU R: src/nmath/gamma.c (gammafn)
//     Copyright (C) 2000-2018 The R Core Team
//     Copyright (C) 2002-2018 The R Foundation
//     Copyright (C) 1998 Ross Ihaka
//
// GNU R: src/nmath/lgamma.c (lgammafn_sign)
//     Copyright (C) 2000-2020 The R Core Team
//     Copyright (C) 1998 Ross Ihaka
//
// GNU R: src/nmath/pgamma.c (pgamma_raw and its helpers, logcf, log1pmx, lgamma1p, logspace_sub)
//     Copyright (C) 2006-2019 The R Core Team
//     Copyright (C) 2005-6 Morten Welinder <terra@gnome.org>
//     Copyright (C) 2005-10 The R Foundation
//
// GNU R: src/nmath/bd0.c (ebd0 and its scale table)
//     Copyright (C) 2000-2025 The R Core Team
//     Copyright (C) 2014 Morten Welinder
//     Copyright (C) 2000 Catherine Loader
//
// GNU R: src/nmath/qgamma.c (qgamma, qchisq_appr)
//     Copyright (C) 2000--2015 The R Core Team
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2004--2015 The R Foundation
//
// GNU R: src/nmath/qDiscrete_search.h (q_DISCRETE_BODY, do_search), qpois.c, qbinom.c, qnbinom.c
//     Copyright (C) 2000-2024 The R Core Team
//     Copyright (C) 2005-2021 The R Foundation
//
// GNU R: src/nmath/stirlerr.c (stirlerr)
//     Copyright (C) 2000-2025 The R Core Team
//     Copyright (C) 2000 Catherine Loader
//
// GNU R: src/nmath/dpois.c (dpois_raw)
//     Copyright (C) 2000-2021 The R Core Team
//     Copyright (C) 2000 Catherine Loader
//
// GNU R: src/nmath/cospi.c (cospi_, sinpi_, tanpi_)
//     Copyright (C) 2013-2022 The R Core Team
//
// GNU R: src/nmath/stirlerr.c (stirlerr)
//     Copyright (C) 2000-2021 The R Core Team
//     partly based on Catherine Loader's work,
//     (C) 1999-2000 Lucent Technologies, Bell Laboratories
//
// GNU R: src/nmath/bd0.c (bd0)
//     Copyright (C) 2000-2022 The R Core Team
//
// GNU R: src/nmath/dbinom.c (dbinom_raw)
//     Copyright (C) 2000-2020 The R Core Team
//     Copyright (C) 2008 The R Foundation
//     partly based on Catherine Loader's work,
//     (C) 1999-2000 Lucent Technologies, Bell Laboratories
//
// GNU R: src/nmath/dpois.c (dpois_raw)
//     Copyright (C) 2000-2021 The R Core Team
//     partly based on Catherine Loader's work,
//     (C) 1999-2000 Lucent Technologies, Bell Laboratories
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! R's mathematical library, the API `Rmath.h` declares: densities, distribution functions,
//! quantiles and random variates for R's distributions, and the special functions they rest on.
//!
//! Results must agree with GNU R's to the last few bits, because R code compares them exactly
//! and packages' own tests record them. So the core routines are translations of GNU R's own
//! nmath code (the files named above), including R Core's extensions to the published
//! algorithms: Cody's rational approximations for the normal distribution, Wichura's AS 241 for
//! its quantile with R's asymptotic tail, Didonato and Morris's TOMS 708 for the incomplete
//! beta ratio with R's `log_p` additions, and Copenhaver and Holland's AS 190 for the
//! studentized range. Other functions follow the published method GNU R's nmath library uses
//! (Loader's saddle point for binomial and Poisson densities) in GNU R's order of operations.
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

use {
    crate::format::format_g,
    std::{cell::RefCell, ffi::c_int, rc::Rc},
};

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

/// `signif(x, digits)`, GNU R's `fprec`: `x` rounded to `digits` significant decimal digits by
/// scaling it with a power of ten, rounding half to even, and scaling back.
pub fn fprec(x: f64, digits: f64) -> f64 {
    const MAX_DIGITS: i32 = 22;
    const MAX10E: i32 = 308;
    if x.is_nan() || digits.is_nan() {
        return x + digits;
    }
    if !finite(x) {
        return x;
    }
    let digits = if finite(digits) {
        digits
    } else if digits > 0.0 {
        return x;
    } else {
        1.0
    };
    if x == 0.0 {
        return x;
    }
    // C's `round`, which rounds half away from zero.
    let mut dig = c_int_cast(digits.round());
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
        // Numbers below 10^(dig - 1) * 1e-308, whose scale factor alone would overflow.
        if e10 > MAX10E {
            p10 = r_pow_di(10.0, e10 - MAX10E);
            e10 = MAX10E;
        }
        // A positive power of ten is exactly representable up to 1e22, so the scaling
        // multiplies or divides by one rather than by its inexact reciprocal.
        if e10 > 0 {
            let pow10 = r_pow_di(10.0, e10);
            sgn * (forceint((x * pow10) * p10) / pow10) / p10
        } else {
            let pow10 = r_pow_di(10.0, -e10);
            sgn * (forceint(x / pow10) * pow10)
        }
    } else {
        // Up to R 4.5.0 the headroom below the largest double counted only its 308 whole
        // decades, so `signif(1.09e308, 2)` truncated to 1e308.
        let largest = if l10 > 0.0 && !crate::system::r_at_least_release(4, 5, 1) {
            MAX10E as f64
        } else {
            f64::MAX.log10()
        };
        let do_round = largest - l10 >= r_pow_di(10.0, -dig);
        let e2 = dig + (if e10 > 0 { 1 } else { -1 }) * MAX_DIGITS;
        let p10 = r_pow_di(10.0, e2);
        let big_p10 = r_pow_di(10.0, e10 - e2);
        let mut x = x * p10;
        x *= big_p10;
        if do_round {
            x += 0.5;
        }
        x = x.floor() / p10;
        sgn * x / big_p10
    }
}

/// `round(x, digits)`, GNU R's `fround`: of the two neighbours of `x` with `digits` decimals,
/// each computed as a double by scaling with a power of ten, the one closer to `x`, and on a
/// tie the one with an even last digit. So `round(0.15, 1)` is 0.1, because the double nearest
/// 0.15 lies below it, and `round(99.21 + 1e-14, 2)` is the double nearest 99.21 even though
/// scaling `x` by 100 already gives a whole number.
pub fn fround(x: f64, digits: f64) -> f64 {
    const MAX10E: i32 = 308;
    // Large `digits` matter for tiny numbers, down to the subnormals' 10^-323.
    const MAX_DIGITS: f64 = (MAX10E + f64::DIGITS as i32) as f64;
    if x.is_nan() || digits.is_nan() {
        return x + digits;
    }
    if !finite(x) {
        return x;
    }
    if digits > MAX_DIGITS || x == 0.0 {
        return x;
    } else if digits < -MAX10E as f64 {
        return 0.0;
    } else if digits == 0.0 {
        return forceint(x);
    }
    let dig = (digits + 0.5).floor() as i32;
    let (sgn, x) = if x < 0.0 { (-1.0, -x) } else { (1.0, x) };
    // About log10(x), and enough to tell that `x` has no digits left to round away.
    let l10x = std::f64::consts::LOG10_2 * (0.5 + logb(x));
    if l10x + dig as f64 > f64::DIGITS as f64 {
        return sgn * x;
    }
    let (i10, down, up);
    if dig <= MAX10E {
        // Neither 10^dig nor `x` scaled by it overflows here.
        let pow10 = r_pow_di(10.0, dig);
        let x10 = x * pow10;
        i10 = x10.floor();
        down = i10 / pow10;
        up = x10.ceil() / pow10;
    } else {
        // `x` is far below 1, about 10^-305 or less, and 10^dig would overflow.
        let p10 = r_pow_di(10.0, dig - MAX10E);
        let pow10 = r_pow_di(10.0, MAX10E);
        let x10 = (x * pow10) * p10;
        i10 = x10.floor();
        down = i10 / pow10 / p10;
        up = x10.ceil() / pow10 / p10;
    }
    let distance_up = up - x;
    let distance_down = x - down;
    sgn * if distance_up < distance_down || (i10 % 2.0 == 1.0 && distance_up == distance_down) {
        up
    } else {
        down
    }
}

/// C's conversion of a double to `int`, which truncates and is undefined out of range. On
/// x86-64 the instruction gives `INT_MIN` there, so GNU R's `signif(x, 1e10)` keeps one digit,
/// while ARM saturates as Rust's `as` does.
pub fn c_int_cast(x: f64) -> c_int {
    if cfg!(target_arch = "x86_64") && !(x > -2147483649.0 && x < 2147483648.0) {
        c_int::MIN
    } else {
        x as c_int
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
    if let Some(product) = crate::arith::small_power_product(x, y) {
        return product;
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

/// `sum_{k >= 0} x^k / (i + k d)`, by the continued fraction of GNU R's `logcf`, with a relative
/// tolerance `eps`. It is the series behind `log1pmx` and `lgamma1p`.
fn logcf(x: f64, i: f64, d: f64, eps: f64) -> f64 {
    let scale_factor = scale_factor();
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
        if b2.abs() > scale_factor {
            a1 /= scale_factor;
            b1 /= scale_factor;
            a2 /= scale_factor;
            b2 /= scale_factor;
        } else if b2.abs() < 1.0 / scale_factor {
            a1 *= scale_factor;
            b1 *= scale_factor;
            a2 *= scale_factor;
            b2 *= scale_factor;
        }
    }
    a2 / b2
}

/// `(2^32)^8 = 2^256`, the rescaling factor of the continued fractions.
fn scale_factor() -> f64 {
    f64::from_bits((1023 + 256) << 52)
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
    // zeta(42) - 1, the remainder the series scales.
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

/// `stirlerr(n) = ln(n!) - ln(sqrt(2 pi n) (n / e)^n)` at the half integers up to 15, rounded
/// from 50-digit evaluations of that definition (`stirlerr(0)` is 0 by convention).
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

unsafe extern "C" {
    /// The C library's `lgamma`, which GNU R's `stirlerr` calls on `(1, 5.25]`.
    fn lgamma(x: f64) -> f64;
}

/// `stirlerr(n) = ln(n!) - ln(sqrt(2 pi n) (n / e)^n)`, with GNU R's pieces: the table at half
/// integers up to 15, a direct form below 5.25, and Stirling's series with as many terms as each
/// range needs. R before 4.4 used one cutoff set and the direct form up to 15.
#[allow(clippy::excessive_precision)]
fn stirlerr(n: f64) -> f64 {
    if !crate::system::r_at_least_release(4, 4, 0) {
        return stirlerr_before_4_4(n);
    }
    // The series coefficients B_2k / (2k (2k - 1)), as GNU R spells them.
    const S: [f64; 17] = [
        0.083333333333333333333,
        0.00277777777777777777778,
        0.00079365079365079365079365,
        0.000595238095238095238095238,
        0.0008417508417508417508417508,
        0.0019175269175269175269175262,
        0.0064102564102564102564102561,
        0.029550653594771241830065352,
        0.17964437236883057316493850,
        1.3924322169059011164274315,
        13.402864044168391994478957,
        156.84828462600201730636509,
        2193.1033333333333333333333,
        36108.771253724989357173269,
        691472.26885131306710839498,
        15238221.539407416192283370,
        382900751.39141414141414141,
    ];
    // Horner form with the last coefficient index `last`: (S0-(S1-(...-S_last/nn)/nn)/nn)/n.
    let series = |last: usize, nn: f64| {
        let mut tail = S[last];
        for index in (0..last).rev() {
            tail = S[index] - tail / nn;
        }
        tail / n
    };
    if n <= 23.5 {
        let doubled = n + n;
        if n <= 15.0
            && doubled >= 0.0
            && doubled == doubled.trunc()
            && let Some(value) = STIRLERR_HALVES.get(doubled as usize)
        {
            return *value;
        }
        if n <= 5.25 {
            return if n >= 1.0 {
                let log_n = n.ln();
                // SAFETY: `lgamma` is a pure C library function of a double.
                let log_gamma = unsafe { lgamma(n) };
                log_gamma + n * (1.0 - log_n) + (log_n - M_LN_2PI) / 2.0
            } else {
                lgamma1p(n) - (n + 0.5) * n.ln() + n - M_LN_SQRT_2PI
            };
        }
        let nn = n * n;
        let last = if n > 12.8 {
            6
        } else if n > 12.3 {
            7
        } else if n > 8.9 {
            8
        } else if n > 7.3 {
            10
        } else if n > 6.6 {
            12
        } else if n > 6.1 {
            14
        } else {
            16
        };
        return series(last, nn);
    }
    let nn = n * n;
    let last = if n > 15.7e6 {
        0
    } else if n > 6180.0 {
        1
    } else if n > 205.0 {
        2
    } else if n > 86.0 {
        3
    } else if n > 27.0 {
        4
    } else {
        5
    };
    series(last, nn)
}

/// The error of Stirling's approximation to `ln(n!)`, `ln Gamma(n + 1) - (n + 1/2) ln n + n -
/// ln sqrt(2 pi)` (Loader 2000). Up to 15 it is read from the table at half integers or computed
/// from that definition; above 15 it is Stirling's series `sum_k B_2k / (2k (2k - 1) n^(2k - 1))`
/// with as many terms as Loader's cutoffs (35, 80 and 500) need for full precision.
fn stirlerr_before_4_4(n: f64) -> f64 {
    // |B_2k| / (2k (2k - 1)) for k = 1..5; the series alternates in sign.
    const STIRLING_SERIES: [f64; 5] = [
        1.0 / 12.0,
        1.0 / 360.0,
        1.0 / 1260.0,
        1.0 / 1680.0,
        1.0 / 1188.0,
    ];
    if n <= 15.0 {
        let doubled = 2.0 * n;
        if doubled == doubled.trunc()
            && doubled >= 0.0
            && let Some(value) = STIRLERR_HALVES.get(doubled as usize)
        {
            return *value;
        }
        return lgammafn(n + 1.0) - (n + 0.5) * n.ln() + n - M_LN_SQRT_2PI;
    }
    let terms = if n > 500.0 {
        2
    } else if n > 80.0 {
        3
    } else if n > 35.0 {
        4
    } else {
        5
    };
    let square = n * n;
    let mut sum = 0.0;
    for coefficient in STIRLING_SERIES.iter().take(terms).rev() {
        sum = coefficient - sum / square;
    }
    sum / n
}

/// The deviance term `x log(x/np) + np - x`, by a series when `x` is near `np` (Loader 2000).
fn bd0(x: f64, np: f64) -> f64 {
    if !finite(x) || !finite(np) || np == 0.0 {
        return f64::NAN;
    }
    // R 4.5.1 halves the series' terms and doubles the sum at the end, and splits the log of
    // `x/np`, so that extreme arguments neither overflow nor underflow.
    if crate::system::r_at_least_release(4, 5, 1) {
        if (x - np).abs() < 0.1 * (x + np) {
            let difference = x - np;
            let mut v = difference / (x + np);
            if difference != 0.0 && v == 0.0 {
                let (x_quarter, np_quarter) = (x / 4.0, np / 4.0);
                v = (x_quarter - np_quarter) / (x_quarter + np_quarter);
            }
            let mut s = difference / 2.0 * v;
            if (s * 2.0).abs() < DBL_MIN {
                return s * 2.0;
            }
            let mut ej = x * v;
            v *= v;
            for j in 1..1000 {
                ej *= v;
                let previous = s;
                s += ej / ((j << 1) + 1) as f64;
                if s == previous {
                    return s * 2.0;
                }
            }
        }
        let log_ratio = if finite(x / np) {
            (x / np).ln()
        } else {
            x.ln() - np.ln()
        };
        return if x > np {
            x * (log_ratio - 1.0) + np
        } else {
            x * log_ratio + np - x
        };
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

/// `x ln(x/m) + m - x` as an unevaluated sum `(high, low)` (Welinder's `ebd0`). A plain double
/// holds the deviance only to an absolute precision of an ulp of its largest term, which
/// `exp(-bd0)` turns into a relative error of the Poisson density; the two parts keep it.
fn ebd0(x: f64, m: f64) -> (f64, f64) {
    const SCALE_BITS: i32 = 10;
    const TABLE_STEPS: i32 = 128;
    let mut high = 0.0;
    let mut low = 0.0;
    if x == m {
        return (0.0, 0.0);
    }
    if x == 0.0 {
        return (m, 0.0);
    }
    if m == 0.0 {
        return (f64::INFINITY, 0.0);
    }
    if m / x == f64::INFINITY {
        return (m, 0.0);
    }
    let (r, e) = frexp(m / x);
    if M_LN2 * f64::from(-e) > 1.0 + f64::MAX / x {
        return (f64::INFINITY, 0.0);
    }
    let i = ((r - 0.5) * f64::from(2 * TABLE_STEPS) + 0.5).floor() as usize;
    let f = (f64::from(1u32 << SCALE_BITS) / (0.5 + i as f64 / f64::from(2 * TABLE_STEPS)) + 0.5)
        .floor();
    let fg = ldexp(f, -(e + SCALE_BITS));
    if fg == f64::INFINITY {
        return (fg, 0.0);
    }
    // Splits each term into its nearest integer and the remainder, so that nice arguments add
    // without rounding.
    fn add(high: &mut f64, low: &mut f64, term: f64) {
        let rounded = (term + 0.5).floor();
        *high += rounded;
        *low += term - rounded;
    }
    add(&mut high, &mut low, -x * log1pmx((m * fg - x) / x));
    if fg == 1.0 {
        return (high, low);
    }
    for j in 0..4 {
        add(&mut high, &mut low, x * BD0_SCALE[i][j]);
        add(&mut high, &mut low, -x * BD0_SCALE[0][j] * f64::from(e));
        if !finite(high) {
            return (f64::INFINITY, 0.0);
        }
    }
    add(&mut high, &mut low, m);
    add(&mut high, &mut low, -m * fg);
    (high, low)
}

/// `frexp`: `x = fraction * 2^exponent` with the fraction in `[0.5, 1)`, for finite nonzero `x`.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    // Subnormals are lifted into the normal range first.
    let (scaled, lift) = if x.abs() < f64::MIN_POSITIVE {
        (x * f64::from_bits((1023 + 54) << 52), 54)
    } else {
        (x, 0)
    };
    let bits = scaled.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1022;
    let fraction = f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52));
    (fraction, exponent - lift)
}

/// `ldexp`: `x * 2^n` with a single rounding, also into the subnormal range.
fn ldexp(mut x: f64, mut n: i32) -> f64 {
    let two_1023 = f64::from_bits(0x7fe0_0000_0000_0000);
    let two_m969 = f64::from_bits(0x0010_0000_0000_0000) * f64::from_bits((1023 + 53) << 52);
    if n > 1023 {
        x *= two_1023;
        n -= 1023;
        if n > 1023 {
            x *= two_1023;
            n -= 1023;
            n = n.min(1023);
        }
    } else if n < -1022 {
        x *= two_m969;
        n += 1022 - 53;
        if n < -1022 {
            x *= two_m969;
            n += 1022 - 53;
            n = n.max(-1022);
        }
    }
    x * f64::from_bits(((1023 + n) as u64) << 52)
}

const BD0_SCALE: [[f64; 4]; 129] = [
    [
        0.6931471824645996,
        -1.9046542121259336e-09,
        -8.78318373858934e-17,
        3.061840738529369e-24,
    ],
    [
        0.6853040456771851,
        -4.2578264469739224e-08,
        -1.1723105396588968e-15,
        6.20339263720161e-23,
    ],
    [
        0.6773988008499146,
        2.274189014883632e-08,
        1.4411920431605914e-15,
        7.046384546650164e-23,
    ],
    [
        0.6699305772781372,
        -4.8293856025338755e-08,
        -8.664795531738382e-16,
        7.049557660755325e-24,
    ],
    [
        0.6624060869216919,
        -4.791602492559832e-08,
        -2.161508226230938e-15,
        7.092968360825275e-23,
    ],
    [
        0.6548244953155518,
        6.237715233226027e-09,
        1.1806699607549086e-16,
        6.660323533535123e-25,
    ],
    [
        0.6471850872039795,
        -4.220866856030625e-08,
        -1.3817589176253094e-15,
        -1.1159932395766606e-23,
    ],
    [
        0.6400018930435181,
        -4.979170142860312e-08,
        4.887013763907625e-16,
        1.6847465694533583e-23,
    ],
    [
        0.6327667236328125,
        -5.406177194799966e-08,
        -2.7224545250006775e-15,
        -2.9070780223955397e-23,
    ],
    [
        0.6249561309814453,
        3.285935434860221e-08,
        1.201672936775467e-16,
        -8.668233185091897e-24,
    ],
    [
        0.6181373596191406,
        -6.406189467789147e-11,
        3.550580962335878e-18,
        -3.623679406748965e-25,
    ],
    [
        0.6107416152954102,
        4.229853800552519e-08,
        1.548143293520366e-15,
        -5.446353118719766e-23,
    ],
    [
        0.6032907962799072,
        5.515817491641428e-08,
        2.1193636784238266e-15,
        1.247109826410663e-22,
    ],
    [
        0.5963221788406372,
        3.28135385530004e-09,
        -1.4033469816541837e-16,
        5.544061982643074e-24,
    ],
    [
        0.5893045663833618,
        4.307998580088679e-08,
        -3.2446651600595306e-15,
        -2.569782398567476e-23,
    ],
    [
        0.5822374820709229,
        -3.983067387025585e-08,
        2.9387905513776885e-15,
        1.6124430498687833e-22,
    ],
    [
        0.5751199722290039,
        2.242384056216906e-09,
        -2.204516403537114e-17,
        4.078543753751354e-25,
    ],
    [
        0.5685046911239624,
        4.422870603093543e-08,
        2.7879957109977287e-16,
        -9.741708794029408e-24,
    ],
    [
        0.5618454217910767,
        2.147161382026752e-08,
        1.3374919170106156e-15,
        -2.326922260336506e-23,
    ],
    [
        0.554580807685852,
        4.57783499996367e-09,
        2.633145912141026e-16,
        2.070995903387998e-23,
    ],
    [
        0.5478278398513794,
        -7.667999568639061e-09,
        6.199095327775322e-16,
        6.341979011056023e-24,
    ],
    [
        0.5410289764404297,
        -3.7302321231891256e-08,
        -3.3801781496424835e-15,
        1.4469198371142414e-22,
    ],
    [
        0.5347557067871094,
        4.382891916066001e-08,
        -8.601820749692393e-16,
        -8.775950563978016e-24,
    ],
    [
        0.5278670787811279,
        1.0839714903454478e-08,
        -4.480281248130319e-16,
        3.8840996084516777e-23,
    ],
    [
        0.5215104818344116,
        4.2031594205127476e-08,
        1.211009992711288e-15,
        -2.3206744819671555e-23,
    ],
    [
        0.5145297050476074,
        -1.2322940889930578e-08,
        2.820084495413274e-16,
        1.880026562185923e-23,
    ],
    [
        0.5080875158309937,
        -1.2297126872340414e-08,
        -4.295835588700959e-16,
        -1.8036626177499945e-23,
    ],
    [
        0.5016034841537476,
        5.9145378372704727e-08,
        1.2728561033550608e-15,
        -5.824094430019416e-23,
    ],
    [
        0.4950772523880005,
        1.4409851090135817e-08,
        -6.381910184083422e-17,
        -3.500509376717118e-25,
    ],
    [
        0.48910707235336304,
        2.745798610703787e-08,
        -1.4470418644525664e-15,
        4.211866896949939e-23,
    ],
    [
        0.4824984669685364,
        1.762245460668055e-08,
        4.128675224747099e-16,
        2.6082691866392177e-23,
    ],
    [
        0.4764525294303894,
        -1.2470243504481004e-08,
        -9.162193924794196e-17,
        3.765782542240376e-24,
    ],
    [
        0.469759464263916,
        -5.450353945946063e-09,
        -4.304846960468561e-16,
        2.074710343707083e-25,
    ],
    [
        0.46363574266433716,
        -1.7013046527125653e-09,
        7.601622738878589e-18,
        -2.0415879231895994e-25,
    ],
    [
        0.4574742913246155,
        6.943684516258486e-10,
        -2.5461310777374286e-17,
        5.5412533419976475e-25,
    ],
    [
        0.4512746334075928,
        1.0731865174307131e-08,
        6.374000219845047e-16,
        3.901547318408994e-23,
    ],
    [
        0.44503629207611084,
        2.8650656958006948e-08,
        -9.155352545558342e-16,
        -4.7365878254772817e-23,
    ],
    [
        0.43938833475112915,
        2.6186132373595683e-08,
        1.3619601577579505e-15,
        -5.2672794844240613e-23,
    ],
    [
        0.4330751895904541,
        1.9065733880552216e-08,
        1.014319201479946e-15,
        1.0145670737413337e-22,
    ],
    [
        0.42735910415649414,
        -1.141359362577532e-08,
        1.3242044582274781e-16,
        -1.0240055893684678e-23,
    ],
    [
        0.42096930742263794,
        -1.2778508917676845e-08,
        6.143525731270704e-16,
        1.419242178938019e-23,
    ],
    [
        0.4151833653450012,
        -7.767916088141646e-09,
        5.955443124124071e-16,
        2.732668133771502e-23,
    ],
    [
        0.4093637466430664,
        1.8807551072086426e-09,
        1.9153331349462894e-16,
        -5.6208063158075e-24,
    ],
    [
        0.40351009368896484,
        -2.0416603518924603e-08,
        -2.93405013148838e-16,
        1.894346911970581e-24,
    ],
    [
        0.39762192964553833,
        1.0016001361634608e-09,
        2.2863242352463633e-17,
        9.458133611770764e-25,
    ],
    [
        0.3916988968849182,
        1.545909711353488e-08,
        1.0962823446210085e-15,
        3.108302239821126e-23,
    ],
    [
        0.3864043951034546,
        -2.076412375373593e-09,
        1.5073464810114547e-16,
        7.412449454375094e-24,
    ],
    [
        0.38041436672210693,
        -8.244395388601333e-09,
        1.4866224964678982e-16,
        -3.927292740683968e-24,
    ],
    [
        0.3743882179260254,
        9.158529934438775e-09,
        5.656919067358501e-16,
        3.4213474905617904e-23,
    ],
    [
        0.36900103092193604,
        -2.2253590969967263e-08,
        6.231405399548338e-16,
        -2.1564751355837555e-23,
    ],
    [
        0.36358463764190674,
        -2.6778728567933285e-08,
        -9.943908455716573e-16,
        -4.704929732945495e-24,
    ],
    [
        0.35745590925216675,
        -2.033036139437172e-08,
        -1.5794492077344114e-15,
        6.318678032068057e-23,
    ],
    [
        0.3519763946533203,
        2.850385882879891e-08,
        -9.566434575519799e-16,
        -6.409595040255684e-24,
    ],
    [
        0.3464667797088623,
        -1.236265312343221e-08,
        -6.003368248279719e-16,
        -2.860901497105794e-24,
    ],
    [
        0.3409265875816345,
        -6.110413286464222e-10,
        1.7467136243918857e-17,
        1.9962587429804357e-25,
    ],
    [
        0.3353555202484131,
        2.167272583619706e-08,
        -1.0918773497788125e-15,
        -3.047574780704126e-23,
    ],
    [
        0.33045530319213867,
        -1.608884048209802e-08,
        -3.833435008838916e-16,
        -7.683741875124221e-24,
    ],
    [
        0.3248254060745239,
        2.801670362373443e-08,
        -2.0725720961098414e-16,
        1.3160777896524739e-23,
    ],
    [
        0.3191636800765991,
        2.6222629401218e-08,
        -1.3995222573204161e-15,
        8.599883909680833e-23,
    ],
    [
        0.31418323516845703,
        2.6826626253750874e-08,
        -9.792556373536439e-16,
        2.2954960929108544e-23,
    ],
    [
        0.3084607720375061,
        1.368350943664609e-08,
        5.591995050742643e-16,
        -1.1938701403427125e-23,
    ],
    [
        0.3034266233444214,
        -8.629042369534545e-09,
        -5.222554219259392e-16,
        3.228770766379237e-23,
    ],
    [
        0.2983669638633728,
        8.688424202318856e-09,
        2.764116793423446e-16,
        -1.0171858868428321e-23,
    ],
    [
        0.29255300760269165,
        -4.91631446664087e-09,
        2.562284723960507e-16,
        -2.6341575505102177e-23,
    ],
    [
        0.28743791580200195,
        -1.3782395669181824e-08,
        7.29039351877645e-16,
        -4.431977943282236e-24,
    ],
    [
        0.28229647874832153,
        2.3770866164340987e-08,
        6.449228392264254e-16,
        3.417536976845211e-23,
    ],
    [
        0.27712851762771606,
        1.4733029018998423e-08,
        6.793364114448283e-16,
        3.593898034032222e-24,
    ],
    [
        0.27193373441696167,
        -1.8933320689029642e-08,
        7.77939410955583e-16,
        2.591972424742619e-23,
    ],
    [
        0.2667117714881897,
        1.4300387263244119e-11,
        -7.458876800955772e-19,
        4.247418782257993e-26,
    ],
    [
        0.2622140049934387,
        7.802219315067305e-09,
        5.022431038767539e-16,
        -2.4063174816868367e-23,
    ],
    [
        0.25694090127944946,
        2.9618050234603288e-08,
        7.279528203645545e-16,
        2.638556081145549e-23,
    ],
    [
        0.2516399025917053,
        -6.44787778725231e-09,
        4.122028133693521e-16,
        7.427559296153698e-24,
    ],
    [
        0.2470736801624298,
        -1.9981829524340355e-09,
        -1.0909757890855787e-16,
        6.236552089808213e-24,
    ],
    [
        0.24171993136405945,
        5.523085988556886e-09,
        -2.686547633696265e-16,
        -2.764495989170092e-24,
    ],
    [
        0.23710808157920837,
        1.0085374313462125e-08,
        -4.775626813761317e-16,
        2.330620598032363e-23,
    ],
    [
        0.231700599193573,
        -1.3946383603524737e-08,
        1.0970921279709183e-17,
        3.229909378627549e-25,
    ],
    [
        0.22704219818115234,
        -6.451284839670279e-09,
        -4.2529947798112047e-16,
        -1.0260254677162862e-23,
    ],
    [
        0.2223619818687439,
        1.4110645096820917e-08,
        6.025568981827254e-16,
        1.9385180739531644e-23,
    ],
    [
        0.21765980124473572,
        -8.286782815503102e-09,
        5.232363919579781e-16,
        5.078443538654066e-23,
    ],
    [
        0.21214580535888672,
        -8.254218641923217e-09,
        3.255553133321774e-16,
        1.571430013634162e-23,
    ],
    [
        0.20739519596099854,
        -1.6149279691290985e-09,
        2.1131592679643073e-17,
        1.4275617427027514e-24,
    ],
    [
        0.20262190699577332,
        8.597639933327628e-09,
        -3.3804619056798137e-16,
        2.5623235609364303e-24,
    ],
    [
        0.19782572984695435,
        1.348296585490516e-08,
        -3.2024568730231384e-16,
        -2.5712252251631252e-23,
    ],
    [
        0.1930064558982849,
        9.956859781112826e-10,
        9.001674563844606e-17,
        -3.754797654135173e-24,
    ],
    [
        0.1889725625514984,
        4.2415360113068346e-09,
        3.8086815297465933e-16,
        -2.1147402916208568e-23,
    ],
    [
        0.18411031365394592,
        6.931054841174955e-09,
        -3.4784858522920016e-16,
        2.4665943434547742e-23,
    ],
    [
        0.17922431230545044,
        5.0739235035734964e-09,
        3.2221329189922637e-16,
        -1.0379009008973928e-23,
    ],
    [
        0.17431432008743286,
        3.794385250444066e-09,
        3.190066898587176e-16,
        2.0292714723890484e-23,
    ],
    [
        0.17020416259765625,
        3.4223344158590407e-09,
        -1.8846416901959178e-16,
        1.1415315069779235e-23,
    ],
    [
        0.16524958610534668,
        -1.3210039284672348e-08,
        -2.3213954359040806e-16,
        3.043054213286757e-24,
    ],
    [
        0.1602703034877777,
        6.007922159767531e-09,
        -7.521047737154288e-17,
        -1.2649106048711768e-25,
    ],
    [
        0.15610191226005554,
        -1.2301535790015805e-08,
        3.0175617567361414e-16,
        -8.633806506327147e-24,
    ],
    [
        0.15191605687141418,
        -1.4845571882915465e-08,
        -3.265830289949929e-16,
        -1.5268151962784823e-23,
    ],
    [
        0.14686977863311768,
        -4.6748995785605985e-09,
        -4.294291341758996e-16,
        1.328295982896899e-23,
    ],
    [
        0.142645001411438,
        9.186472027522541e-09,
        -8.049373155998929e-16,
        1.437998766909278e-23,
    ],
    [
        0.1384023129940033,
        9.86511672351753e-09,
        -8.837306496940929e-16,
        7.295324909194215e-24,
    ],
    [
        0.1332872211933136,
        9.990350768873668e-10,
        3.316946610734358e-17,
        2.7351440526086287e-24,
    ],
    [
        0.129004567861557,
        -7.46120853989396e-09,
        -6.212113165383437e-16,
        1.855187264989731e-24,
    ],
    [
        0.12470348179340363,
        -3.2924463155836747e-09,
        -7.404120132741752e-17,
        1.3246955625609024e-24,
    ],
    [
        0.12038381397724152,
        3.3791991427278845e-09,
        1.6214981996371606e-16,
        -6.00070673940474e-24,
    ],
    [
        0.11604541540145874,
        3.5638392237302696e-10,
        -7.354219635108878e-18,
        7.794312440645008e-26,
    ],
    [
        0.11168810725212097,
        3.136765958089427e-09,
        -8.994406293238225e-19,
        -7.920937558182207e-26,
    ],
    [
        0.10731174051761627,
        -4.728527791542092e-09,
        -4.2976339455270984e-16,
        5.351143287258107e-24,
    ],
    [
        0.10291612148284912,
        2.8332007850906393e-09,
        4.925742757234052e-17,
        2.794436810397303e-24,
    ],
    [
        0.09850110113620758,
        4.970725164810119e-09,
        4.130512756847049e-16,
        6.313449560595865e-25,
    ],
    [
        0.09406651556491852,
        -6.525850970717784e-09,
        -1.3492816627243298e-16,
        -9.079650179574527e-24,
    ],
    [
        0.08961215615272522,
        2.5369617517867482e-09,
        1.6110664594961319e-16,
        -5.189504504486964e-24,
    ],
    [
        0.08603434264659882,
        -5.304795713811927e-09,
        5.127575488441481e-17,
        1.463615545692133e-24,
    ],
    [
        0.08154398202896118,
        2.0112156384755053e-09,
        8.065769331577608e-17,
        -3.0150319017810437e-24,
    ],
    [
        0.07703337073326111,
        5.7495661565099e-09,
        -2.503851097302985e-16,
        -1.846143093040508e-23,
    ],
    [
        0.07250232994556427,
        1.177662634077592e-09,
        -3.525476857925506e-17,
        1.3164077898906505e-24,
    ],
    [
        0.0688626617193222,
        -7.043545302565235e-09,
        2.49712406751501e-16,
        1.0686882487619963e-23,
    ],
    [
        0.06429435312747955,
        -2.422082090447475e-09,
        -2.0555896149129847e-16,
        8.602907613530545e-24,
    ],
    [
        0.0606246218085289,
        7.905943261166115e-12,
        -8.270443345471096e-19,
        -2.3533820836754142e-26,
    ],
    [
        0.056018441915512085,
        -5.139745296034448e-10,
        -3.811651571366802e-17,
        1.9072195442219776e-24,
    ],
    [
        0.052318163216114044,
        -3.5574325707443677e-09,
        9.191155834145393e-17,
        -5.321463973420977e-24,
    ],
    [
        0.04767347127199173,
        -1.8026349302147082e-09,
        1.0329634289704177e-16,
        -2.2283569301283993e-24,
    ],
    [
        0.04394212365150452,
        -1.795005699634089e-09,
        -5.3817402974104447e-17,
        -1.3996196977941442e-24,
    ],
    [
        0.040196798741817474,
        3.8451930528538014e-10,
        -2.4485452721520977e-17,
        -7.386769024377949e-26,
    ],
    [
        0.0354953333735466,
        -3.5901653872016936e-10,
        -2.073207767866976e-17,
        -2.412097216555168e-26,
    ],
    [
        0.0317181795835495,
        6.87235046648027e-10,
        -6.430478093749473e-18,
        1.3508692031871337e-25,
    ],
    [
        0.02792670577764511,
        7.568773385813188e-10,
        -4.22031585165944e-17,
        2.534760563692782e-24,
    ],
    [
        0.02412080392241478,
        -1.1255707477175747e-09,
        4.89700584100947e-17,
        1.4172214525647275e-24,
    ],
    [
        0.01934296265244484,
        1.9068610579431322e-10,
        -1.0635946218849709e-17,
        -5.300489542457734e-25,
    ],
    [
        0.015504186972975731,
        -4.3701048335620385e-10,
        6.61106154763715e-18,
        2.5398086818405174e-25,
    ],
    [
        0.01165061630308628,
        9.168890091615367e-10,
        -1.5848697818454755e-17,
        -1.350491609984469e-24,
    ],
    [
        0.007782140746712685,
        -3.046577434773212e-10,
        7.793435996762285e-18,
        4.660100148208369e-25,
    ],
    [
        0.0038986406289041042,
        -2.1324678134426733e-10,
        1.2541658163801307e-19,
        8.745035431740123e-27,
    ],
    [0.0, 0.0, 0.0, 0.0],
];

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
    // From R 4.5 on, the tail formula runs on into the subnormal range, out to
    // `qnorm(log(2^-1074), log.p = TRUE)`, before a probability is rounded to 0 or 1.
    let denormal_edge = if crate::system::r_at_least(4, 5) {
        38.4674
    } else {
        37.5193
    };
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
        || (lower && -denormal_edge < x && x < 8.2924)
        || (upper && -8.2924 < x && x < denormal_edge)
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
        value = qnorm_central(q);
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

/// The quantile of the standard normal at `p` in (0, 1), as `qnorm(p, 0, 1)` computes it, for
/// a caller that turns uniforms into normal deviates in bulk.
#[inline]
pub fn qnorm_standard(p: f64) -> f64 {
    let q = p - 0.5;
    if q.abs() <= 0.425 {
        qnorm_central(q)
    } else {
        qnorm(p, 0.0, 1.0, true, false)
    }
}

/// Wichura's AS 241 rational approximation for `|p - 0.5| <= 0.425`, where `q = p - 0.5`.
#[inline]
fn qnorm_central(q: f64) -> f64 {
    let r = 0.180625 - q * q;
    q * (((((((r * 2509.0809287301226727 + 33430.575583588128105) * r + 67265.770927008700853)
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
        / (((((((r * 5226.495278852854561 + 28729.085735721942674) * r + 39307.89580009271061)
            * r
            + 21213.794301586595867)
            * r
            + 5394.1960214247511077)
            * r
            + 687.1870074920579083)
            * r
            + 42.313330701600911252)
            * r
            + 1.0)
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
    // 2^1023 / pi: from here 2 pi x overflows.
    const X_LARGE: f64 = 2.861_117_485_757_028_2e+307;
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
    let (high, deviance_low) = ebd0(x, lambda);
    let low = deviance_low + stirlerr(x);
    let large = x >= X_LARGE;
    let root = if large {
        M_SQRT_2PI * x.sqrt()
    } else {
        M_2PI * x
    };
    if log {
        -low - high - if large { root.ln() } else { 0.5 * root.ln() }
    } else {
        (-low).exp() * (-high).exp() / if large { root } else { root.sqrt() }
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
    pgamma_raw(x, shape, lower, log)
}

/// `P(alph, x)` when `lower_tail`, else `Q(alph, x)`, for `alph > 0` (Welinder's `pgamma_raw`).
/// It picks among the series of Abramowitz and Stegun 6.5.29 for small `x`, a scaled series or
/// continued fraction for `x` far from `alph`, and Temme-style Poisson asymptotics near it.
fn pgamma_raw(x: f64, alph: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x <= 0.0 {
        return dt0(lower_tail, log_p);
    }
    if x >= f64::INFINITY {
        return dt1(lower_tail, log_p);
    }
    let res = if x < 1.0 {
        pgamma_smallx(x, alph, lower_tail, log_p)
    } else if x <= alph - 1.0 && x < 0.8 * (alph + 50.0) {
        let sum = pd_upper_series(x, alph, log_p);
        let d = dpois_wrap(alph, x, log_p);
        if !lower_tail {
            if log_p {
                log1_exp(d + sum)
            } else {
                1.0 - d * sum
            }
        } else if log_p {
            sum + d
        } else {
            sum * d
        }
    } else if alph - 1.0 < x && alph < 0.8 * (x + 50.0) {
        let d = dpois_wrap(alph, x, log_p);
        let sum = if alph < 1.0 {
            if x * DBL_EPSILON > 1.0 - alph {
                d1(log_p)
            } else {
                let f = pd_lower_cf(alph, x - (alph - 1.0)) * x / alph;
                if log_p { f.ln() } else { f }
            }
        } else {
            let sum = pd_lower_series(x, alph - 1.0);
            if log_p { sum.ln_1p() } else { 1.0 + sum }
        };
        if !lower_tail {
            if log_p { sum + d } else { sum * d }
        } else if log_p {
            log1_exp(d + sum)
        } else {
            1.0 - d * sum
        }
    } else {
        ppois_asymp(alph - 1.0, x, !lower_tail, log_p)
    };
    // Near the smallest normal number the result loses accuracy to underflow, so it is redone on
    // the log scale.
    if !log_p && res < DBL_MIN / DBL_EPSILON {
        pgamma_raw(x, alph, lower_tail, true).exp()
    } else {
        res
    }
}

/// `dpois(x_plus_1 - 1, lambda)`, also for `x_plus_1` below one.
fn dpois_wrap(x_plus_1: f64, lambda: f64, log: bool) -> f64 {
    // If |x| > |k| * M_CUTOFF then log(exp(-x) * k^x) is about -x.
    const M_CUTOFF: f64 = M_LN2 * 1024.0 / DBL_EPSILON;
    if !finite(lambda) {
        return d0(log);
    }
    if x_plus_1 > 1.0 {
        return dpois_raw(x_plus_1 - 1.0, lambda, log);
    }
    if lambda > (x_plus_1 - 1.0).abs() * M_CUTOFF {
        d_exp(-lambda - lgammafn(x_plus_1), log)
    } else {
        let d = dpois_raw(x_plus_1, lambda, log);
        if log {
            d + (x_plus_1 / lambda).ln()
        } else {
            d * (x_plus_1 / lambda)
        }
    }
}

/// The lower or upper tail for `x < 1` by Abramowitz and Stegun 6.5.29.
fn pgamma_smallx(x: f64, alph: f64, lower_tail: bool, log_p: bool) -> f64 {
    let mut sum = 0.0;
    let mut c = alph;
    let mut n = 0.0;
    loop {
        n += 1.0;
        c *= -x / n;
        let term = c / (alph + n);
        sum += term;
        if term.abs() <= DBL_EPSILON * sum.abs() {
            break;
        }
    }
    if lower_tail {
        let f1 = if log_p { sum.ln_1p() } else { 1.0 + sum };
        let f2 = if alph > 1.0 {
            let f2 = dpois_raw(alph, x, log_p);
            if log_p { f2 + x } else { f2 * x.exp() }
        } else if log_p {
            alph * x.ln() - lgamma1p(alph)
        } else {
            x.powf(alph) / lgamma1p(alph).exp()
        };
        if log_p { f1 + f2 } else { f1 * f2 }
    } else {
        let lf2 = alph * x.ln() - lgamma1p(alph);
        if log_p {
            log1_exp(sum.ln_1p() + lf2)
        } else {
            let f1m1 = sum;
            let f2m1 = lf2.exp_m1();
            -(f1m1 + f2m1 + f1m1 * f2m1)
        }
    }
}

/// `sum_{n >= 1} x^n / (y (y + 1) ... (y + n - 1))`, about `x / y` for large `y`.
fn pd_upper_series(x: f64, mut y: f64, log_p: bool) -> f64 {
    let mut term = x / y;
    let mut sum = term;
    loop {
        y += 1.0;
        term *= x / y;
        sum += term;
        if term <= sum * DBL_EPSILON {
            break;
        }
    }
    if log_p { sum.ln() } else { sum }
}

/// The continued fraction for the scaled upper-tail `F_gamma`, about `(y / d) (1 + (1 - y) / d)`.
fn pd_lower_cf(y: f64, d: f64) -> f64 {
    const MAX_IT: u32 = 200_000;
    let scale_factor = scale_factor();
    if y == 0.0 {
        return 0.0;
    }
    let mut f0 = y / d;
    if (y - 1.0).abs() < d.abs() * DBL_EPSILON {
        return f0;
    }
    if f0 > 1.0 {
        f0 = 1.0;
    }
    let mut c2 = y;
    let mut c4 = d;
    let mut a1 = 0.0;
    let mut b1 = 1.0;
    let mut a2 = y;
    let mut b2 = d;
    while b2 > scale_factor {
        a1 /= scale_factor;
        b1 /= scale_factor;
        a2 /= scale_factor;
        b2 /= scale_factor;
    }
    let mut i = 0.0;
    let mut old = -1.0;
    let mut f = 0.0;
    while i < f64::from(MAX_IT) {
        i += 1.0;
        c2 -= 1.0;
        let mut c3 = i * c2;
        c4 += 2.0;
        a1 = c4 * a2 + c3 * a1;
        b1 = c4 * b2 + c3 * b1;
        i += 1.0;
        c2 -= 1.0;
        c3 = i * c2;
        c4 += 2.0;
        a2 = c4 * a1 + c3 * a2;
        b2 = c4 * b1 + c3 * b2;
        if b2 > scale_factor {
            a1 /= scale_factor;
            b1 /= scale_factor;
            a2 /= scale_factor;
            b2 /= scale_factor;
        }
        if b2 != 0.0 {
            f = a2 / b2;
            if (f - old).abs() <= DBL_EPSILON * fmax2(f0, f.abs()) {
                return f;
            }
            old = f;
        }
    }
    f
}

/// `sum_{n >= 0} y (y - 1) ... (y - n) / lambda^(n + 1)`, about `y / lambda`.
fn pd_lower_series(lambda: f64, mut y: f64) -> f64 {
    let mut term = 1.0;
    let mut sum = 0.0;
    while y >= 1.0 && term > sum * DBL_EPSILON {
        term *= y / lambda;
        sum += term;
        y -= 1.0;
    }
    if y != y.floor() {
        // The series does not converge for y below -lambda, so one more term comes from the
        // continued fraction.
        let f = pd_lower_cf(y, lambda + 1.0 - y);
        sum += term * f;
    }
    sum
}

/// `dnorm(x) / pnorm(x, lower_tail)` with more accuracy than the quotient, given `lp =
/// pnorm(x, lower_tail, log = TRUE)` (Abramowitz and Stegun 26.2.12).
fn dpnorm(mut x: f64, mut lower_tail: bool, lp: f64) -> f64 {
    if x < 0.0 {
        x = -x;
        lower_tail = !lower_tail;
    }
    if x > 10.0 && !lower_tail {
        let mut term = 1.0 / x;
        let mut sum = term;
        let x2 = x * x;
        let mut i = 1.0;
        loop {
            term *= -i / x2;
            sum += term;
            i += 2.0;
            if term.abs() <= DBL_EPSILON * sum {
                break;
            }
        }
        1.0 / sum
    } else {
        dnorm(x, 0.0, 1.0, false) / lp.exp()
    }
}

/// The probability that a Poisson variate is at most `x`, by an asymptotic expansion in the
/// normal distribution (Smith's, at members.aol.com/iandjmsmith/PoissonApprox.htm).
fn ppois_asymp(x: f64, lambda: f64, lower_tail: bool, log_p: bool) -> f64 {
    const COEFS_A: [f64; 8] = [
        -1e99,
        2.0 / 3.0,
        -4.0 / 135.0,
        8.0 / 2835.0,
        16.0 / 8505.0,
        -8992.0 / 12629925.0,
        -334144.0 / 492567075.0,
        698752.0 / 1477701225.0,
    ];
    const COEFS_B: [f64; 8] = [
        -1e99,
        1.0 / 12.0,
        1.0 / 288.0,
        -139.0 / 51840.0,
        -571.0 / 2488320.0,
        163879.0 / 209018880.0,
        5246819.0 / 75246796800.0,
        -534703531.0 / 902961561600.0,
    ];
    let dfm = lambda - x;
    let pt = -log1pmx(dfm / x);
    let mut s2pt = (2.0 * x * pt).sqrt();
    if dfm < 0.0 {
        s2pt = -s2pt;
    }
    let mut res12 = 0.0;
    let mut res1_term = x.sqrt();
    let mut res1_ig = res1_term;
    let mut res2_term = s2pt;
    let mut res2_ig = res2_term;
    for i in 1..8 {
        res12 += res1_ig * COEFS_A[i];
        res12 += res2_ig * COEFS_B[i];
        res1_term *= pt / i as f64;
        res2_term *= 2.0 * pt / (2 * i + 1) as f64;
        res1_ig = res1_ig / x + res1_term;
        res2_ig = res2_ig / x + res2_term;
    }
    let mut elfb = x;
    let mut elfb_term = 1.0;
    for coefficient in &COEFS_B[1..8] {
        elfb += elfb_term * coefficient;
        elfb_term /= x;
    }
    if !lower_tail {
        elfb = -elfb;
    }
    let f = res12 / elfb;
    let np = pnorm(s2pt, 0.0, 1.0, !lower_tail, log_p);
    if log_p {
        let n_d_over_p = dpnorm(s2pt, !lower_tail, np);
        np + (f * n_d_over_p).ln_1p()
    } else {
        let nd = dnorm(s2pt, 0.0, 1.0, log_p);
        np + f * nd
    }
}

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

/// The gamma quantile: AS 91's chi-squared approximation, then up to 1000 steps of a seven-term
/// Taylor series on `pgamma`, then a Newton iteration on the log scale for the last digits (GNU
/// R's `qgamma`).
pub fn qgamma(p: f64, alpha: f64, scale: f64, lower: bool, log: bool) -> f64 {
    const EPS1: f64 = 1e-2;
    const EPS2: f64 = 5e-7;
    const EPS_N: f64 = 1e-15;
    const MAX_IT: u32 = 1000;
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
    let mut max_it_newton = 1;
    if alpha < 1e-10 {
        max_it_newton = 7;
    }
    let mut p_lower = dt_qiv(p, lower, log);
    let g = lgammafn(alpha);
    let mut ch = qchisq_appr(p, 2.0 * alpha, g, lower, log, EPS1);
    'phases: {
        if !finite(ch) {
            max_it_newton = 0;
            break 'phases;
        }
        if ch < EPS2 {
            max_it_newton = 20;
            break 'phases;
        }
        if p_lower > P_MAX || p_lower < P_MIN {
            max_it_newton = 20;
            break 'phases;
        }
        let c = alpha - 1.0;
        let s6 = (120.0 + c * (346.0 + 127.0 * c)) * I5040;
        let ch0 = ch;
        for _ in 1..=MAX_IT {
            let q = ch;
            let p1 = 0.5 * ch;
            let p2 = p_lower - pgamma_raw(p1, alpha, true, false);
            if !finite(p2) || ch <= 0.0 {
                ch = ch0;
                max_it_newton = 27;
                break 'phases;
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
                break 'phases;
            }
            if (q - ch).abs() > 0.1 * ch {
                ch = if ch < q { 0.9 * q } else { 1.1 * q };
            }
        }
    }
    let mut x = 0.5 * scale * ch;
    if max_it_newton > 0 {
        let mut p = p;
        let mut log_p = log;
        if !log_p {
            p = p.ln();
            log_p = true;
        }
        if x == 0.0 {
            const ONE_PLUS: f64 = 1.0 + 1e-7;
            const ONE_MINUS: f64 = 1.0 - 1e-7;
            x = DBL_MIN;
            p_lower = pgamma(x, alpha, scale, lower, log_p);
            if (lower && p_lower > p * ONE_PLUS) || (!lower && p_lower < p * ONE_MINUS) {
                return 0.0;
            }
        } else {
            p_lower = pgamma(x, alpha, scale, lower, log_p);
        }
        if p_lower == f64::NEG_INFINITY {
            return 0.0;
        }
        for i in 1..=max_it_newton {
            let p1 = p_lower - p;
            if p1.abs() < (EPS_N * p).abs() {
                break;
            }
            let density = dgamma(x, alpha, scale, log_p);
            if density == d0(log_p) {
                break;
            }
            let mut t = if log_p {
                p1 * (p_lower - density).exp()
            } else {
                p1 / density
            };
            t = if lower { x - t } else { x + t };
            p_lower = pgamma(t, alpha, scale, lower, log_p);
            if (p_lower - p).abs() > p1.abs() || (i > 1 && (p_lower - p).abs() == p1.abs()) {
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

/// `R_Log1_Exp` as TOMS 708 defines it, with its own `rexpm1` instead of the C library's `expm1`.
fn log1_exp_toms(x: f64) -> f64 {
    if x > -M_LN2 {
        (-rexpm1(x)).ln()
    } else {
        (-x.exp()).ln_1p()
    }
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
            w1 = if log { log1_exp_toms(w) } else { 0.5 - w + 0.5 };
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
                w1 = if log { log1_exp_toms(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::W1Bpser => {
                w1 = bpser(b0, a0, y0, eps, log);
                w = if log {
                    log1_exp_toms(w1)
                } else {
                    0.5 - w1 + 0.5
                };
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
                w1 = if log { log1_exp_toms(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::Bfrac => {
                w = bfrac(a0, b0, x0, y0, lambda, eps * 15.0, log);
                w1 = if log { log1_exp_toms(w) } else { 0.5 - w + 0.5 };
                done = Done::Both;
            }
            Path::Basym => {
                w = basym(a0, b0, lambda, eps * 100.0, log);
                w1 = if log { log1_exp_toms(w) } else { 0.5 - w + 0.5 };
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
                    w1 = log1_exp_toms(w);
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
                w = log1_exp_toms(w1);
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
    // R 4.5.1 caps the iterations at 1000 instead of 10000, orders the product `n x (b - n)`
    // differently, and rescales the terms when `n x (b - n)` overflows for a huge `b`.
    let current = crate::system::r_at_least_release(4, 5, 1);
    let maximum = if current { 1000.0 } else { 10000.0 };
    loop {
        n += 1.0;
        let mut rescale = false;
        let w = if current {
            let w = n * x * (b - n);
            if finite(w) {
                w
            } else {
                rescale = true;
                n * x * ldexp(b - n, -20)
            }
        } else {
            n * (b - n) * x
        };
        let mut t = n / a;
        let mut e = a / s;
        let alpha = p * (p + c0) * e * e * (w * x);
        e = (t + 1.0) / (c1 + t + t);
        let beta = if current {
            let tail = n + e * (c + n * yp1);
            w / s + if rescale { ldexp(tail, -20) } else { tail }
        } else {
            n + w / s + e * (c + n * yp1)
        };
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
        if n >= maximum {
            break;
        }
    }
    if n >= maximum && (r - r0).abs() > eps * r {
        warning(format!(
            " bfrac(a={a}, b={b}, x={x}, y={y}, lambda={lambda}) did *not* converge (in {maximum} steps)"
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

/// The beta quantile (AS 109 with Remark AS R83, and R's refinements): a starting value from
/// Hastings's and Carter's normal approximations, then safeguarded Newton steps on `x` or on
/// `log x`.
pub fn qbeta(alpha: f64, p: f64, q: f64, lower: bool, log: bool) -> f64 {
    if p.is_nan() || q.is_nan() || alpha.is_nan() {
        return p + q + alpha;
    }
    if p < 0.0 || q < 0.0 {
        return f64::NAN;
    }
    qbeta_raw(alpha, p, q, lower, log, USE_LOG_X_CUTOFF, N_NEWTON_FREE)[0]
}

const USE_LOG_X_CUTOFF: f64 = -5.0;
const N_NEWTON_FREE: i32 = 4;

/// Returns `[qbeta, 1 - qbeta]`, the second useful when the quantile is close to one. With
/// `log_q_cut` infinite the results are on the log scale.
#[allow(clippy::too_many_lines, unused_assignments)]
fn qbeta_raw(
    alpha: f64,
    p: f64,
    q: f64,
    lower_tail: bool,
    log_p: bool,
    log_q_cut: f64,
    n_newton: i32,
) -> [f64; 2] {
    // Assumes subnormal numbers, so there is no underflow at DBL_MIN.
    const DBL_VERY_MIN: f64 = DBL_MIN / 4.0;
    const DBL_LOG_V_MIN: f64 = M_LN2 * (-1021.0 - 2.0);
    const FPU: f64 = 3e-308;
    const ACU_MIN: f64 = 1e-300;
    const P_LO: f64 = FPU;
    const P_HI: f64 = 1.0 - 2.22e-16;
    const CONST1: f64 = 2.30753;
    const CONST2: f64 = 0.27061;
    const CONST3: f64 = 0.99229;
    const CONST4: f64 = 0.04481;
    // log(DBL_EPSILON)
    const LOG_EPS_C: f64 = M_LN2 * (1.0 - 53.0);
    let dbl_1_minus_eps = f64::from_bits(0x3fef_ffff_ffff_ffff);

    let give_log_q = log_q_cut == f64::INFINITY;
    let mut use_log_x = give_log_q;
    let mut warned = false;
    let mut add_n_step = true;
    let mut swap_tail;
    let (mut a, mut la, mut pp, mut qq) = (0.0, 0.0, 0.0, 0.0);
    let (mut g, mut h, mut r, mut s, mut t, mut w);
    (r, t, w) = (0.0, 0.0, 0.0);
    let mut y = -1.0;
    let mut u = 0.0;
    let mut xinbta = 0.0;
    let mut tx = 0.0;
    let mut u_n = 1.0;

    let q_zero = || {
        if give_log_q {
            [f64::NEG_INFINITY, 0.0]
        } else {
            [0.0, 1.0]
        }
    };
    let q_one = || {
        if give_log_q {
            [0.0, f64::NEG_INFINITY]
        } else {
            [1.0, 0.0]
        }
    };
    let q_half = || {
        if give_log_q {
            [-M_LN2, -M_LN2]
        } else {
            [0.5, 0.5]
        }
    };

    if alpha == dt0(lower_tail, log_p) {
        return q_zero();
    }
    if alpha == dt1(lower_tail, log_p) {
        return q_one();
    }
    // Checked before the transformation, which may lose all accuracy.
    if (log_p && alpha > 0.0) || (!log_p && !(0.0..=1.0).contains(&alpha)) {
        return [f64::NAN, f64::NAN];
    }
    // p == 0, q == 0, p = Inf and q = Inf are one- or two-point masses.
    if p == 0.0 || q == 0.0 || !finite(p) || !finite(q) {
        if p == 0.0 && q == 0.0 {
            let half = if log_p { -M_LN2 } else { 0.5 };
            if alpha < half {
                return q_zero();
            }
            if alpha > half {
                return q_one();
            }
            return q_half();
        } else if p == 0.0 || p / q == 0.0 {
            return q_zero();
        } else if q == 0.0 || q / p == 0.0 {
            return q_one();
        }
        return q_half();
    }

    let p_lower = dt_qiv(alpha, lower_tail, log_p);
    let logbeta = lbeta(p, q);
    swap_tail = p_lower > 0.5;
    let mut n_maybe_swaps = 0;
    let (mut acu, mut rp);

    enum Exit {
        Converged,
        NotConverged,
        Return,
    }
    let exit = 'search: loop {
        if swap_tail {
            a = dt_civ(alpha, lower_tail, log_p);
            la = dt_clog(alpha, lower_tail, log_p);
            pp = q;
            qq = p;
        } else {
            a = p_lower;
            la = dt_log(alpha, lower_tail, log_p);
            pp = p;
            qq = q;
        }
        n_maybe_swaps += 1;
        acu = ACU_MIN.max(10f64.powf(-13.0 - 2.5 / (pp * pp) - 0.5 / (a * a)));
        let u0 = (la + pp.ln() + logbeta) / pp;
        rp = pp * (1.0 - qq) / (pp + 1.0);
        t = 0.2;
        let u0_maybe = M_LN2 * -1021.0 < u0 && u0 < -0.01;
        u_n = 1.0;
        let mut jump_to_newton = false;
        if u0_maybe
            && u0
                < (t * LOG_EPS_C
                    - (pp * (1.0 - qq) * (2.0 - qq) / (2.0 * (pp + 2.0)))
                        .abs()
                        .ln())
                    / 2.0
        {
            // One cheap correction step, instead of a Newton step.
            rp *= u0.exp();
            if rp > -1.0 {
                u = u0 - rp.ln_1p() / pp;
            } else {
                u = u0;
            }
            xinbta = u.exp();
            tx = xinbta;
            use_log_x = true;
            jump_to_newton = true;
        }
        if !jump_to_newton {
            // y_alpha of AS 64: Hastings's approximation of qnorm(1 - a).
            r = (-2.0 * la).sqrt();
            y = r - (CONST1 + CONST2 * r) / (1.0 + (CONST3 + CONST4 * r) * r);
            if pp > 1.0 && qq > 1.0 {
                // Carter (1947), AS 109 remark 5.
                r = (y * y - 3.0) / 6.0;
                s = 1.0 / (pp + pp - 1.0);
                t = 1.0 / (qq + qq - 1.0);
                h = 2.0 / (s + t);
                w = y * (h + r).sqrt() / h - (t - s) * (r + 5.0 / 6.0 - 2.0 / (3.0 * h));
                if w > 300.0 {
                    t = w + w + qq.ln() - pp.ln();
                    u = if t <= 18.0 {
                        -t.exp().ln_1p()
                    } else {
                        -t - (-t).exp()
                    };
                    xinbta = u.exp();
                } else {
                    xinbta = pp / (pp + qq * (w + w).exp());
                    u = -(qq / pp * (w + w).exp()).ln_1p();
                }
            } else {
                // AS 64, Scheffe-Tukey and Wilson-Hilferty.
                r = qq + qq;
                t = 1.0 / (3.0 * qq.sqrt());
                t = r * r_pow_di(1.0 + t * (-t + y), 3);
                s = 4.0 * pp + r - 2.0;
                if t == 0.0 || (t < 0.0 && s >= t) {
                    let l1ma = if swap_tail {
                        dt_log(alpha, lower_tail, log_p)
                    } else {
                        dt_clog(alpha, lower_tail, log_p)
                    };
                    let xx = (l1ma + qq.ln() + logbeta) / qq;
                    if xx <= 0.0 {
                        xinbta = -xx.exp_m1();
                        u = log1_exp(xx);
                    } else {
                        let r_ = rp * u0.exp();
                        if r_ > -1.0 {
                            u = u0 - r_.ln_1p() / pp;
                        } else {
                            u = u0;
                        }
                        xinbta = u.exp();
                    }
                } else {
                    t = s / t;
                    if t <= 1.0 {
                        u = u0;
                        xinbta = u.exp();
                    } else {
                        xinbta = 1.0 - 2.0 / (t + 1.0);
                        u = (-2.0 / (t + 1.0)).ln_1p();
                    }
                }
            }
            // If the initial u is completely wrong this decision is wrong too.
            if (swap_tail && u >= -log_q_cut.exp())
                || (!swap_tail && u >= -(4.0 * log_q_cut).exp() && pp / qq < 1000.0)
            {
                swap_tail = !swap_tail;
                if swap_tail {
                    a = dt_civ(alpha, lower_tail, log_p);
                    la = dt_clog(alpha, lower_tail, log_p);
                    pp = q;
                    qq = p;
                } else {
                    a = p_lower;
                    la = dt_log(alpha, lower_tail, log_p);
                    pp = p;
                    qq = q;
                }
                u = log1_exp(u);
                xinbta = u.exp();
            }
            if !use_log_x {
                use_log_x = u < log_q_cut;
            }
            let bad_u = !finite(u);
            let bad_init = bad_u || xinbta > P_HI;
            tx = xinbta;
            if bad_u || u < log_q_cut {
                // Try the left border quickly, at the smallest positive number.
                w = pbeta_raw(DBL_VERY_MIN, pp, qq, true, log_p);
                if w > (if log_p { la } else { a }) {
                    if log_p || (w - a).abs() < (0.0 - a).abs() {
                        tx = DBL_VERY_MIN;
                        u_n = DBL_LOG_V_MIN;
                    } else {
                        tx = 0.0;
                        u_n = f64::NEG_INFINITY;
                    }
                    use_log_x = log_p;
                    add_n_step = false;
                    break 'search Exit::Return;
                } else if u < DBL_LOG_V_MIN {
                    u = DBL_LOG_V_MIN;
                    xinbta = DBL_VERY_MIN;
                }
            }
            // The approximation is sometimes negative, and zero is not acceptable either.
            if bad_init && !(use_log_x && tx > 0.0) {
                if u == f64::NEG_INFINITY {
                    u = M_LN2 * -1021.0;
                    xinbta = DBL_MIN;
                } else {
                    xinbta = if xinbta > 1.1 {
                        0.5
                    } else if xinbta < P_LO {
                        u.exp()
                    } else {
                        P_HI
                    };
                    if bad_u {
                        u = xinbta.ln();
                    }
                }
            }
        }

        // Newton-Raphson with pbeta_raw.
        r = 1.0 - pp;
        t = 1.0 - qq;
        let mut wprev = 0.0;
        let mut prev: f64 = 1.0;
        let mut adj: f64 = 1.0;
        if use_log_x {
            for i_pb in 0..1000 {
                y = pbeta_raw(xinbta, pp, qq, true, true);
                w = if y == f64::NEG_INFINITY {
                    0.0
                } else {
                    (y - la) * (y - u + logbeta + r * u + t * log1_exp(u)).exp()
                };
                if !finite(w) {
                    if n_maybe_swaps <= 1 {
                        continue 'search;
                    }
                    return [f64::NAN, f64::NAN];
                }
                if i_pb >= n_newton && w * wprev <= 0.0 {
                    prev = adj.abs().max(FPU);
                }
                g = 1.0;
                for _ in 0..1000 {
                    adj = g * w;
                    if adj.abs() < prev {
                        u_n = u - adj;
                        if u_n <= 0.0 {
                            if prev <= acu || w.abs() <= acu {
                                break 'search Exit::Converged;
                            }
                            break;
                        }
                    }
                    g /= 3.0;
                }
                let d = adj.abs().min((u_n - u).abs());
                if d <= 4e-16 * (u_n + u).abs() {
                    break 'search Exit::Converged;
                }
                u = u_n;
                xinbta = u.exp();
                wprev = w;
            }
        } else {
            for i_pb in 0..1000 {
                y = pbeta_raw(xinbta, pp, qq, true, log_p);
                w = if log_p {
                    (y - la) * (y + logbeta + r * xinbta.ln() + t * (-xinbta).ln_1p()).exp()
                } else {
                    (y - a) * (logbeta + r * xinbta.ln() + t * (-xinbta).ln_1p()).exp()
                };
                if !finite(w) {
                    if n_maybe_swaps <= 2 {
                        if !log_p && n_maybe_swaps == 2 {
                            use_log_x = true;
                        }
                        if !log_p || n_maybe_swaps <= 1 {
                            continue 'search;
                        }
                    }
                    return [f64::NAN, f64::NAN];
                }
                if i_pb >= n_newton && w * wprev <= 0.0 {
                    prev = adj.abs().max(FPU);
                }
                g = 1.0;
                for _ in 0..1000 {
                    adj = g * w;
                    // Full Newton steps at the beginning, then the safeguard.
                    if i_pb < n_newton || adj.abs() < prev {
                        tx = xinbta - adj;
                        if (0.0..=1.0).contains(&tx) {
                            if prev <= acu || w.abs() <= acu {
                                break 'search Exit::Converged;
                            }
                            if tx != 0.0 && tx != 1.0 {
                                break;
                            }
                        }
                    }
                    g /= 3.0;
                }
                if (tx - xinbta).abs() <= 4e-16 * (tx + xinbta) {
                    break 'search Exit::Converged;
                }
                xinbta = tx;
                if tx == 0.0 {
                    break;
                }
                wprev = w;
            }
        }
        break 'search Exit::NotConverged;
    };

    if let Exit::NotConverged = exit {
        warned = true;
        precision_warning("qbeta");
    }
    if !matches!(exit, Exit::Return) {
        let log_ = log_p || use_log_x;
        if (log_ && y == f64::NEG_INFINITY) || (!log_ && y == 0.0) {
            // Stuck at the left: the smallest positive number may be better.
            w = pbeta_raw(DBL_VERY_MIN, pp, qq, true, log_);
            if log_ || (w - a).abs() <= (y - a).abs() {
                tx = DBL_VERY_MIN;
                u_n = DBL_LOG_V_MIN;
            }
            add_n_step = false;
        } else if !warned
            && (if log_ {
                (y - la).abs() > 3.0
            } else {
                (y - a).abs() > 1e-4
            })
            && !(log_
                && y == f64::NEG_INFINITY
                && pbeta_raw(dbl_1_minus_eps, pp, qq, true, true) > la + 2.0)
        {
            warning(format!(
                "qbeta(a, *) =: x0 with |pbeta(x0,*{}) - alpha| = {} is not accurate",
                if log_ { ", log_" } else { "" },
                crate::format::c_format((y - if log_ { la } else { a }).abs(), "g", 5, "", false)
            ));
        }
    }

    if give_log_q {
        let r = log1_exp(u_n);
        return if swap_tail { [r, u_n] } else { [u_n, r] };
    }
    if use_log_x {
        if add_n_step {
            // One last Newton step on the original scale.
            if u_n != 1.0 {
                xinbta = u_n.exp();
            }
            y = pbeta_raw(xinbta, pp, qq, true, log_p);
            w = if log_p {
                (y - la) * (y + logbeta + r * xinbta.ln() + t * (-xinbta).ln_1p()).exp()
            } else {
                (y - a) * (logbeta + r * xinbta.ln() + t * (-xinbta).ln_1p()).exp()
            };
            tx = if finite(w) { xinbta - w } else { xinbta };
        } else {
            return if swap_tail {
                [-u_n.exp_m1(), u_n.exp()]
            } else {
                [u_n.exp(), -u_n.exp_m1()]
            };
        }
    }
    if swap_tail {
        [1.0 - tx, tx]
    } else {
        [tx, 1.0 - tx]
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
    // From R 4.4 on, the end points are powers of the smaller of p and q, the more accurate.
    let powers = crate::system::r_at_least(4, 4);
    if x == 0.0 {
        if n == 0.0 {
            return d1(log);
        }
        if powers {
            return match (p > q, log) {
                (true, true) => n * q.ln(),
                (true, false) => q.powf(n),
                (false, true) => n * (-p).ln_1p(),
                (false, false) => pow1p(-p, n),
            };
        }
        let lc = if p < 0.1 {
            -bd0(n, n * q) - n * p
        } else {
            n * q.ln()
        };
        return d_exp(lc, log);
    }
    if x == n {
        if powers {
            return match (p > q, log) {
                (true, true) => n * (-q).ln_1p(),
                (true, false) => pow1p(-q, n),
                (false, true) => n * p.ln(),
                (false, false) => p.powf(n),
            };
        }
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
    let mut n = n;
    if !finite(n) && crate::system::r_at_least_release(4, 5, 1) {
        if finite(x) {
            return d0(log);
        }
        n = f64::MAX;
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

/// The quantile of a distribution on the integers `0..=maximum`, GNU R's `q_DISCRETE_BODY`.
///
/// The search starts from the Cornish-Fisher expansion `mu + sigma (z + gamma (z^2 - 1) / 6)`
/// about the normal quantile `z`, where `gamma` is the skewness (Cornish and Fisher (1937);
/// Johnson, Kemp and Kotz (2005), section 1.8). `p` is first nudged by a few units in the last
/// place so that a probability that was rounded on the way in still finds its own integer, and a
/// start past 4096 walks in shrinking strides.
#[allow(clippy::too_many_arguments)]
fn discrete_quantile(
    mut p: f64,
    mu: f64,
    sigma: f64,
    gamma: f64,
    lower: bool,
    log: bool,
    maximum: Option<f64>,
    distribution: &dyn Fn(f64, bool, bool) -> f64,
) -> f64 {
    const FUZZ_PLAIN: f64 = 8.0;
    const FUZZ_LOG: f64 = 2.0;
    const LARGE: f64 = 4096.0;
    const INCREMENT_FRACTION: f64 = 1.0 / 64.0;
    const SHRINK: f64 = 8.0;
    const RELATIVE_TOLERANCE: f64 = 1e-15;
    const EXTRA_FACTOR: f64 = 4.0;
    let normal = qnorm(p, 0.0, 1.0, lower, log);
    let mut y = forceint(mu + sigma * (normal + gamma * (normal * normal - 1.0) / 6.0));
    match maximum {
        Some(limit) if y > limit => y = limit,
        _ if y < 0.0 => y = 0.0,
        _ => {}
    }
    let mut z = distribution(y, lower, log);
    if log {
        let e = FUZZ_LOG * DBL_EPSILON;
        if lower && p > -f64::MAX {
            p *= 1.0 + e;
        } else {
            p *= 1.0 - e;
        }
    } else {
        let e = FUZZ_PLAIN * DBL_EPSILON;
        if lower {
            p *= 1.0 - e;
        } else if 1.0 - p > EXTRA_FACTOR * e {
            p *= 1.0 + e;
        }
    }
    let search = |mut y: f64, z: &mut f64, increment: f64| -> f64 {
        let left = if lower { *z >= p } else { *z < p };
        if left {
            loop {
                let mut next = -1.0;
                if y > 0.0 {
                    next = distribution(y - increment, lower, log);
                } else if y < 0.0 {
                    y = 0.0;
                }
                if y == 0.0 || next.is_nan() || (if lower { next < p } else { next >= p }) {
                    return y;
                }
                let lowered = y - increment;
                y = if 0.0 < lowered { lowered } else { 0.0 };
                *z = next;
            }
        }
        loop {
            let previous = y;
            let mut next = -1.0;
            y += increment;
            match maximum {
                Some(limit) if y < limit => next = distribution(y, lower, log),
                Some(limit) if y > limit => y = limit,
                Some(_) => {}
                None => next = distribution(y, lower, log),
            }
            if maximum == Some(y) || next.is_nan() || (if lower { next >= p } else { next < p }) {
                if increment <= 1.0 {
                    *z = next;
                    return y;
                }
                return previous;
            }
            *z = next;
        }
    };
    if y < LARGE {
        return search(y, &mut z, 1.0);
    }
    let mut increment = (y * INCREMENT_FRACTION).floor();
    loop {
        let previous = increment;
        y = search(y, &mut z, increment);
        let shrunk = (increment / SHRINK).floor();
        increment = if 1.0 < shrunk { shrunk } else { 1.0 };
        if !(previous > 1.0 && increment > y * RELATIVE_TOLERANCE) {
            return y;
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
    discrete_quantile(
        p,
        mu,
        sigma,
        gamma,
        lower,
        log,
        Some(n),
        &|y, lower, log| pbinom(y, n, pr, lower, log),
    )
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
    if crate::system::r_at_least_release(4, 4, 0) {
        let x = forceint(x);
        if x == 0.0 {
            // The limit as size goes to zero is a point mass at zero.
            if size == 0.0 {
                return d1(log);
            }
            return if log {
                size * prob.ln()
            } else {
                prob.powf(size)
            };
        }
        let size = if finite(size) { size } else { DBL_MAX };
        if x < 1e-10 * size {
            // Two terms of Abramowitz and Stegun 6.1.47 in place of dbinom_raw.
            let xx2s = x_choose_two_over(x, size);
            return d_exp(
                size * prob.ln() + x * (size.ln() + (-prob).ln_1p()) - lgamma1p(x) + xx2s.ln_1p(),
                log,
            );
        }
        // log(size / (size + x)) is much less accurate than log1p(-x / (size + x)) for x < size.
        let p = if log {
            if x < size {
                (-x / (size + x)).ln_1p()
            } else {
                (size / (size + x)).ln()
            }
        } else {
            size / (size + x)
        };
        let ans = dbinom_raw(size, x + size, prob, 1.0 - prob, log);
        return if log { p + ans } else { p * ans };
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

/// `x (x - 1) / (2 size)` without overflow for huge `x`.
fn x_choose_two_over(x: f64, size: f64) -> f64 {
    if x < f64::MAX.sqrt() {
        ldexp(x * (x - 1.0), -1) / size
    } else {
        x * (ldexp(x, -1) / size)
    }
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
        if crate::system::r_at_least_release(4, 4, 0) {
            return d_exp(
                x * p - mu - lgamma1p(x) + x_choose_two_over(x, size).ln_1p(),
                log,
            );
        }
        return d_exp(
            x * p - mu - lgammafn(x + 1.0) + (x * (x - 1.0) / (2.0 * size)).ln_1p(),
            log,
        );
    }
    let ans = dbinom_raw(size, x + size, size / (size + mu), mu / (size + mu), log);
    if crate::system::r_at_least_release(4, 4, 0) {
        let p = if log {
            if x < size {
                (-x / (size + x)).ln_1p()
            } else {
                (size / (size + x)).ln()
            }
        } else {
            size / (size + x)
        };
        return if log { p + ans } else { p * ans };
    }
    let p = size / (size + x);
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
    discrete_quantile(p, mu, sigma, gamma, lower, log, None, &|y, lower, log| {
        pnbinom(y, size, prob, lower, log)
    })
}

pub fn qnbinom_mu(p: f64, size: f64, mu: f64, lower: bool, log: bool) -> f64 {
    if size == f64::INFINITY {
        return qpois(p, mu, lower, log);
    }
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
    let q = 1.0 + mu / size;
    let pp = mu / size;
    let sigma = (size * pp * q).sqrt();
    let gamma = (q + pp) / sigma;
    discrete_quantile(p, mu, sigma, gamma, lower, log, None, &|y, lower, log| {
        pnbinom_mu(y, size, mu, lower, log)
    })
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

/// The geometric quantile, the smallest count `k` with `P(X <= k) >= p`. Since
/// `P(X > k) = (1 - prob)^(k + 1)`, it is the ceiling of `ln P(X > k) / ln(1 - prob) - 1`. The
/// quotient is lowered by 1e-12 first, so that one which is an integer in exact arithmetic but
/// rounded up does not step past it. The fuzz is absolute, which is what GNU R's answers show;
/// beyond a quotient of about 1e4 it is smaller than the rounding it is meant to absorb.
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
    let quotient = dt_clog(p, lower, log) / (-prob).ln_1p();
    fmax2(0.0, (quotient - 1.0 - 1e-12).ceil())
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
    discrete_quantile(p, mu, sigma, gamma, lower, log, None, &|y, lower, log| {
        ppois(y, lambda, lower, log)
    })
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

/// The noncentral chi-squared density, the Poisson mixture `sum_i w_i(ncp / 2) f(x; df + 2i)`
/// of central chi-squared densities (Johnson, Kotz and Balakrishnan (1995), chapter 29).
/// Neighbouring terms have the ratio `(x ncp / 2) / ((i + 1)(df + 2i))`, which falls as `i`
/// grows, so the terms rise to a single largest one at the ceiling of the root of
/// `2i^2 + (df + 2) i + df - x ncp / 2 = 0` and fall geometrically on both sides of it. The sum
/// starts there and runs outward relative to that term, whose size enters once at the end, on
/// the log scale when it underflows. Where the peak lies beyond 1e10, the density is Patnaik's
/// (1949) scaled central chi-squared with the same mean and variance.
pub fn dnchisq(x: f64, df: f64, ncp: f64, log: bool) -> f64 {
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
    let half_ncp = 0.5 * ncp;
    let product = x * half_ncp;
    let root = (-(2.0 + df) + ((2.0 - df) * (2.0 - df) + 4.0 * ncp * x).sqrt()) / 4.0;
    // Past this many terms on each side of the peak the outward sum costs too much, and the
    // two-moment approximation is already within about `1 / sqrt(root)` relative.
    if !(root <= 1e10) {
        let mean = df + ncp;
        let ratio = (mean + ncp) / mean;
        let density = dchisq(x / ratio, mean / ratio, log);
        return if log {
            density - ratio.ln()
        } else {
            density / ratio
        };
    }
    let peak = root.ceil().max(0.0);
    let peak_df = df + 2.0 * peak;
    // A remaining tail below this share of the sum cannot change it.
    let tolerance = f64::EPSILON / 4.0;
    let mut sum = 1.0;
    let (mut term, mut index, mut degrees) = (1.0, peak, peak_df);
    loop {
        index += 1.0;
        let ratio = product / (index * degrees);
        degrees += 2.0;
        term *= ratio;
        sum += term;
        if ratio < 1.0 && term * ratio <= (1.0 - ratio) * tolerance * sum {
            break;
        }
    }
    (term, index, degrees) = (1.0, peak, peak_df);
    while index > 0.0 {
        let ratio = index * (degrees - 2.0) / product;
        index -= 1.0;
        degrees -= 2.0;
        term *= ratio;
        sum += term;
        if ratio < 1.0 && term * ratio <= (1.0 - ratio) * tolerance * sum {
            break;
        }
    }
    if !log {
        let peak_term = dpois_raw(peak, half_ncp, false) * dchisq(x, peak_df, false);
        if peak_term >= DBL_MIN {
            return peak_term * sum;
        }
    }
    let log_density = dpois_raw(peak, half_ncp, true) + dchisq(x, peak_df, true) + sum.ln();
    d_exp(log_density, log)
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
    static WILCOX_CACHE: RefCell<Option<WilcoxTable>> = const { RefCell::new(None) };
}

/// A Mann-Whitney count table and the sample sizes `(m, n)` it was built for.
type WilcoxTable = ((usize, usize), Rc<[f64]>);

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
    let cells = if crate::system::r_at_least(4, 6) {
        m * n / 2.0
    } else {
        small * small * (m + n)
    };
    if cells > MAX_COUNT_CELLS {
        warning(format!(
            "wilcox: samples of sizes {m} and {n} are too large"
        ));
        return None;
    }
    Some((m as usize, n as usize))
}

/// Where the Mann-Whitney counts come from: the whole table for R before 4.6, or GNU R
/// 4.6's lazily filled half table, whose state outlives a call.
enum WilcoxCounts {
    Table(Rc<[f64]>),
    Lazy(usize, usize),
}

fn wilcox_source(m: usize, n: usize) -> WilcoxCounts {
    if crate::system::r_at_least(4, 6) {
        WilcoxCounts::Lazy(m, n)
    } else {
        WilcoxCounts::Table(wilcox_counts(m, n))
    }
}

fn wilcox_count(source: &WilcoxCounts, k: usize) -> f64 {
    match source {
        WilcoxCounts::Table(counts) => counts.get(k).copied().unwrap_or(0.0),
        WilcoxCounts::Lazy(m, n) => cwilcox(k, *m, *n),
    }
}

/// GNU R 4.6's `w`, `sigma`, `allocated_m`, `allocated_n` and `max_k` globals. The state
/// is reused, without refilling, by any later call whose sizes fit inside the allocated
/// ones, so a C caller that does not call `wilcox_free` between sizes reads counts left
/// over from the earlier sizes. Results only match GNU R if that reuse is reproduced.
struct WilcoxState {
    w: Vec<f64>,
    sigma: Vec<i64>,
    allocated: (usize, usize),
    filled: usize,
}

thread_local! {
    static WILCOX_STATE: RefCell<WilcoxState> = const {
        RefCell::new(WilcoxState {
            w: Vec::new(),
            sigma: Vec::new(),
            allocated: (0, 0),
            filled: 0,
        })
    };
}

/// Releases the lazily filled Mann-Whitney counts, as the GNU R wrappers do on exit.
pub fn wilcox_free() {
    WILCOX_STATE.with_borrow_mut(|state| {
        state.w = Vec::new();
        state.sigma = Vec::new();
        state.allocated = (0, 0);
        state.filled = 0;
    });
}

/// The number of choices with Mann-Whitney statistic `k`, by Löffler's recursion
/// `w[k] = sum(w[i] sigma[k - i]) / k`, where `sigma` sums the divisors of `k` up to `m`
/// less those from `n + 1` to `m + n`. `filled` counts the entries of `w` computed so far.
fn cwilcox(k: usize, m: usize, n: usize) -> f64 {
    let total = m * n;
    if k > total {
        return 0.0;
    }
    let k = if k > total / 2 { total - k } else { k };
    let (small, large) = (m.min(n), m.max(n));
    if small == 0 || k == 0 {
        return if k == 0 { 1.0 } else { 0.0 };
    }
    WILCOX_STATE.with_borrow_mut(|state| {
        if state.w.is_empty() || state.allocated != (small, large) {
            if !state.w.is_empty() && (small > state.allocated.0 || large > state.allocated.1) {
                state.w = Vec::new();
                state.sigma = Vec::new();
            }
            if state.w.is_empty() {
                state.w = vec![0.0; small * large / 2 + 1];
                state.sigma = vec![0; small * large / 2 + 1];
                state.allocated = (small, large);
                state.filled = 0;
            }
        }
        let (small, large) = (small as i64, large as i64);
        for index in state.filled..=k {
            let index_signed = index as i64;
            let mut sum = 0;
            for divisor in 1..=small.min(index_signed) {
                if index_signed % divisor == 0 {
                    sum += divisor;
                }
            }
            for divisor in large + 1..=(small + large).min(index_signed) {
                if index_signed % divisor == 0 {
                    sum -= divisor;
                }
            }
            state.sigma[index] = sum;
        }
        for index in state.filled..=k {
            state.w[index] = if index == 0 {
                1.0
            } else {
                let mut sum = 0.0;
                for i in 0..index {
                    sum += state.w[i] * state.sigma[index - i] as f64;
                }
                sum / index as f64
            };
        }
        state.filled = state.filled.max(k + 1);
        state.w[k]
    })
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
    let count = wilcox_count(&wilcox_source(mm, nn), x as usize);
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
    let counts = wilcox_source(mm, nn);
    let c = choose(m + n, n);
    let mut p = 0.0;
    let mut lower = lower;
    if q <= m * n / 2.0 {
        for i in 0..=q as usize {
            p += wilcox_count(&counts, i) / c;
        }
    } else {
        let q = m * n - q;
        for i in 0..q as usize {
            p += wilcox_count(&counts, i) / c;
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
    let counts = wilcox_source(mm, nn);
    let total = mm * nn;
    let c = choose(m + n, n);
    let mut p = 0.0;
    let mut q = 0usize;
    if x <= 0.5 {
        x -= 10.0 * DBL_EPSILON;
        while q < total {
            p += wilcox_count(&counts, q) / c;
            if p >= x {
                break;
            }
            q += 1;
        }
        q as f64
    } else {
        x = 1.0 - x + 10.0 * DBL_EPSILON;
        while q < total {
            p += wilcox_count(&counts, q) / c;
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
    // R 4.6's wilcox.test asks for the upper tail at `q - 1/4`, which only floors correctly.
    let x = if crate::system::r_at_least(4, 6) {
        (x + 1e-7).floor()
    } else {
        forceint(x + 1e-7)
    };
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

/// `exp(-x) I_nu(x)` and `k_factor * exp(x) K_nu(x)` for `x > 0` and `nu >= 0`, by the
/// asymptotic expansion where `x` is large against `nu^2` and by Temme's method elsewhere.
/// Passing `k_factor = exp(-x)` gives `K_nu(x)` itself without forming the scaled value, which
/// overflows for a high order before the unscaled one does.
fn bessel_ik_scaled(x: f64, nu: f64, k_factor: f64) -> (f64, f64) {
    if x >= 50.0 && x >= 2.0 * nu * nu {
        let (i, k) = bessel_ik_asymptotic(x, nu);
        return (i, k * k_factor);
    }
    bessel_ik_temme(x, nu, k_factor)
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

fn bessel_ik_temme(x: f64, nu: f64, k_factor: f64) -> (f64, f64) {
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
    // The upward recurrence is linear, so scaling its two starting values scales every order.
    let mut rkmu = rkmu * k_factor;
    let mut rk1 = rk1 * k_factor;
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
/// R 4.6.2 lets `J`, `I` and `K` go to this order (`M_bessel_j_max_alpha` and
/// `M_bessel_ik_max_alpha`), and warns beyond it.
const BESSEL_NU_MAX_WIDE: f64 = 1e9;

fn bessel_limits_widened() -> bool {
    crate::system::r_at_least_release(4, 6, 2)
}
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
    if alpha > BESSEL_NU_MAX_WIDE && bessel_limits_widened() {
        warning(format!(
            "besselI(x, nu): nu={} > max_alpha_i (= {}): too large for bessel_i() algorithm",
            format_g(alpha),
            format_g(BESSEL_NU_MAX_WIDE)
        ));
        return f64::NAN;
    }
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
    let (i, _) = bessel_ik_scaled(x, alpha, 1.0);
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
    if alpha > BESSEL_NU_MAX_WIDE && bessel_limits_widened() {
        warning(format!(
            "besselK(x, nu): |nu|={} > max_alpha_k (= {}): too large for bessel_k() algorithm",
            format_g(alpha),
            format_g(BESSEL_NU_MAX_WIDE)
        ));
        return f64::NAN;
    }
    let scaled = expo as i32 == 2;
    if x == 0.0 {
        return f64::INFINITY;
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    let k_factor = if scaled { 1.0 } else { (-x).exp() };
    bessel_ik_scaled(x, alpha, k_factor).1
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
    if bessel_limits_widened() {
        if alpha > BESSEL_NU_MAX_WIDE {
            warning(format!(
                "besselJ(x, nu): nu={} > max_alpha_j (= {}): too large for bessel_j() algorithm",
                format_g(alpha),
                format_g(BESSEL_NU_MAX_WIDE)
            ));
            return f64::NAN;
        }
    } else if alpha > BESSEL_NU_MAX {
        warning(format!(
            "besselJ(x, nu): nu={} too large for bessel_j() algorithm",
            format_g(alpha)
        ));
        return f64::NAN;
    }
    if x == 0.0 {
        return if alpha == 0.0 { 1.0 } else { 0.0 };
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    let value = bessel_jy_any(x, alpha).0;
    if value == 0.0 && x > 0.0 {
        warning(format!(
            "bessel_j({},nu={}): precision lost in result",
            format_g(x),
            format_g(alpha)
        ));
    }
    value
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
        if -alpha > BESSEL_NU_MAX && bessel_limits_widened() {
            warning(format!(
                "besselY(x, -nu): nu = {} > {} =: max_alpha_y, too large for bessel_y() algorithm",
                format_g(-alpha),
                format_g(BESSEL_NU_MAX)
            ));
            return f64::NAN;
        }
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
        let message = match bessel_limits_widened() {
            true => format!(
                "besselY(x, nu): nu={} > {} =: max_alpha_y, too large for bessel_y() algorithm",
                format_g(alpha),
                format_g(BESSEL_NU_MAX)
            ),
            false => format!(
                "besselY(x, nu): nu={} too large for bessel_y() algorithm",
                format_g(alpha)
            ),
        };
        warning(message);
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    let value = bessel_jy_any(x, alpha).1;
    if value.is_infinite() {
        warning(format!(
            "bessel_y({},nu={}): precision lost in result",
            format_g(x),
            format_g(alpha)
        ));
    }
    value
}
