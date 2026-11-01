//! R's mathematical library through the C API `Rmath.h` declares. The functions themselves
//! are `interp::rmath`, shared with the interpreter's builtins; this module converts C's `int`
//! flags, draws random variates from the session's generator, and exports the symbols.

use crate::{
    arith::Rng,
    boundary::{self, ApiResult, enter, error},
    runtime,
};
use interp::rmath::*;
use std::ffi::c_int;

/// Hands the warnings the last Rmath call collected to R's warning machinery.
fn raise_warnings() {
    for message in take_warnings() {
        boundary::warn(message);
    }
}

// The C API. Every function is a thin wrapper that converts C's `int` flags; the work happens
// in the Rust functions below, which the interpreter can call directly.

macro_rules! export {
    ($($name:ident($($argument:ident: $kind:tt),*) => $target:ident;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($argument: export!(@type $kind)),*) -> f64 {
                let value = $target($(export!(@value $kind $argument)),*);
                raise_warnings();
                value
            }
        )*
    };
    (@type f64) => { f64 };
    (@type flag) => { c_int };
    (@value f64 $argument:ident) => { $argument };
    (@value flag $argument:ident) => { $argument != 0 };
}

/// Random variates draw from the session's generator, as `unif_rand` does, so the caller
/// brackets them with `GetRNGstate` and `PutRNGstate` exactly as under GNU R.
macro_rules! export_random {
    ($($name:ident($($argument:ident),*) => $target:ident;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name($($argument: f64),*) -> f64 {
                $target(&mut runtime().rng, $($argument),*)
            }
        )*
    };
}

export! {
    Rf_dnorm4(x: f64, mu: f64, sigma: f64, log: flag) => dnorm;
    Rf_pnorm5(x: f64, mu: f64, sigma: f64, lower: flag, log: flag) => pnorm;
    Rf_qnorm5(p: f64, mu: f64, sigma: f64, lower: flag, log: flag) => qnorm;
    Rf_dunif(x: f64, a: f64, b: f64, log: flag) => dunif;
    Rf_punif(x: f64, a: f64, b: f64, lower: flag, log: flag) => punif;
    Rf_qunif(p: f64, a: f64, b: f64, lower: flag, log: flag) => qunif;
    Rf_dgamma(x: f64, shape: f64, scale: f64, log: flag) => dgamma;
    Rf_pgamma(x: f64, shape: f64, scale: f64, lower: flag, log: flag) => pgamma;
    Rf_qgamma(p: f64, shape: f64, scale: f64, lower: flag, log: flag) => qgamma;
    Rf_dbeta(x: f64, a: f64, b: f64, log: flag) => dbeta;
    Rf_pbeta(x: f64, a: f64, b: f64, lower: flag, log: flag) => pbeta;
    Rf_qbeta(p: f64, a: f64, b: f64, lower: flag, log: flag) => qbeta;
    Rf_dlnorm(x: f64, meanlog: f64, sdlog: f64, log: flag) => dlnorm;
    Rf_plnorm(x: f64, meanlog: f64, sdlog: f64, lower: flag, log: flag) => plnorm;
    Rf_qlnorm(p: f64, meanlog: f64, sdlog: f64, lower: flag, log: flag) => qlnorm;
    Rf_dchisq(x: f64, df: f64, log: flag) => dchisq;
    Rf_pchisq(x: f64, df: f64, lower: flag, log: flag) => pchisq;
    Rf_qchisq(p: f64, df: f64, lower: flag, log: flag) => qchisq;
    Rf_dnchisq(x: f64, df: f64, ncp: f64, log: flag) => dnchisq;
    Rf_pnchisq(x: f64, df: f64, ncp: f64, lower: flag, log: flag) => pnchisq;
    Rf_qnchisq(p: f64, df: f64, ncp: f64, lower: flag, log: flag) => qnchisq;
    Rf_df(x: f64, m: f64, n: f64, log: flag) => df;
    Rf_pf(x: f64, m: f64, n: f64, lower: flag, log: flag) => pf;
    Rf_qf(p: f64, m: f64, n: f64, lower: flag, log: flag) => qf;
    Rf_dt(x: f64, n: f64, log: flag) => dt;
    Rf_pt(x: f64, n: f64, lower: flag, log: flag) => pt;
    Rf_qt(p: f64, n: f64, lower: flag, log: flag) => qt;
    Rf_dbinom(x: f64, n: f64, p: f64, log: flag) => dbinom;
    Rf_dbinom_raw(x: f64, n: f64, p: f64, q: f64, log: flag) => dbinom_raw;
    Rf_pbinom(x: f64, n: f64, p: f64, lower: flag, log: flag) => pbinom;
    Rf_qbinom(p: f64, n: f64, pr: f64, lower: flag, log: flag) => qbinom;
    Rf_dcauchy(x: f64, location: f64, scale: f64, log: flag) => dcauchy;
    Rf_pcauchy(x: f64, location: f64, scale: f64, lower: flag, log: flag) => pcauchy;
    Rf_qcauchy(p: f64, location: f64, scale: f64, lower: flag, log: flag) => qcauchy;
    Rf_dexp(x: f64, scale: f64, log: flag) => dexp;
    Rf_pexp(x: f64, scale: f64, lower: flag, log: flag) => pexp;
    Rf_qexp(p: f64, scale: f64, lower: flag, log: flag) => qexp;
    Rf_dgeom(x: f64, p: f64, log: flag) => dgeom;
    Rf_pgeom(x: f64, p: f64, lower: flag, log: flag) => pgeom;
    Rf_qgeom(p: f64, prob: f64, lower: flag, log: flag) => qgeom;
    Rf_dhyper(x: f64, r: f64, b: f64, n: f64, log: flag) => dhyper;
    Rf_phyper(x: f64, r: f64, b: f64, n: f64, lower: flag, log: flag) => phyper;
    Rf_qhyper(p: f64, r: f64, b: f64, n: f64, lower: flag, log: flag) => qhyper;
    Rf_dnbinom(x: f64, size: f64, prob: f64, log: flag) => dnbinom;
    Rf_pnbinom(x: f64, size: f64, prob: f64, lower: flag, log: flag) => pnbinom;
    Rf_qnbinom(p: f64, size: f64, prob: f64, lower: flag, log: flag) => qnbinom;
    Rf_dnbinom_mu(x: f64, size: f64, mu: f64, log: flag) => dnbinom_mu;
    Rf_pnbinom_mu(x: f64, size: f64, mu: f64, lower: flag, log: flag) => pnbinom_mu;
    Rf_qnbinom_mu(p: f64, size: f64, mu: f64, lower: flag, log: flag) => qnbinom_mu;
    Rf_dpois(x: f64, lambda: f64, log: flag) => dpois;
    Rf_dpois_raw(x: f64, lambda: f64, log: flag) => dpois_raw;
    Rf_ppois(x: f64, lambda: f64, lower: flag, log: flag) => ppois;
    Rf_qpois(p: f64, lambda: f64, lower: flag, log: flag) => qpois;
    Rf_dweibull(x: f64, shape: f64, scale: f64, log: flag) => dweibull;
    Rf_pweibull(x: f64, shape: f64, scale: f64, lower: flag, log: flag) => pweibull;
    Rf_qweibull(p: f64, shape: f64, scale: f64, lower: flag, log: flag) => qweibull;
    Rf_dlogis(x: f64, location: f64, scale: f64, log: flag) => dlogis;
    Rf_plogis(x: f64, location: f64, scale: f64, lower: flag, log: flag) => plogis;
    Rf_qlogis(p: f64, location: f64, scale: f64, lower: flag, log: flag) => qlogis;
    Rf_dnbeta(x: f64, a: f64, b: f64, ncp: f64, log: flag) => dnbeta;
    Rf_pnbeta(x: f64, a: f64, b: f64, ncp: f64, lower: flag, log: flag) => pnbeta;
    Rf_qnbeta(p: f64, a: f64, b: f64, ncp: f64, lower: flag, log: flag) => qnbeta;
    Rf_dnf(x: f64, m: f64, n: f64, ncp: f64, log: flag) => dnf;
    Rf_pnf(x: f64, m: f64, n: f64, ncp: f64, lower: flag, log: flag) => pnf;
    Rf_qnf(p: f64, m: f64, n: f64, ncp: f64, lower: flag, log: flag) => qnf;
    Rf_dnt(x: f64, df: f64, ncp: f64, log: flag) => dnt;
    Rf_pnt(x: f64, df: f64, ncp: f64, lower: flag, log: flag) => pnt;
    Rf_qnt(p: f64, df: f64, ncp: f64, lower: flag, log: flag) => qnt;
    Rf_ptukey(q: f64, ranges: f64, means: f64, df: f64, lower: flag, log: flag) => ptukey;
    Rf_qtukey(p: f64, ranges: f64, means: f64, df: f64, lower: flag, log: flag) => qtukey;
    Rf_dwilcox(x: f64, m: f64, n: f64, log: flag) => dwilcox;
    Rf_pwilcox(q: f64, m: f64, n: f64, lower: flag, log: flag) => pwilcox;
    Rf_qwilcox(p: f64, m: f64, n: f64, lower: flag, log: flag) => qwilcox;
    Rf_dsignrank(x: f64, n: f64, log: flag) => dsignrank;
    Rf_psignrank(x: f64, n: f64, lower: flag, log: flag) => psignrank;
    Rf_qsignrank(p: f64, n: f64, lower: flag, log: flag) => qsignrank;
    Rf_gammafn(x: f64) => gammafn;
    Rf_lgammafn(x: f64) => lgammafn;
    Rf_lgamma1p(a: f64) => lgamma1p;
    Rf_digamma(x: f64) => digamma;
    Rf_trigamma(x: f64) => trigamma;
    Rf_tetragamma(x: f64) => tetragamma;
    Rf_pentagamma(x: f64) => pentagamma;
    Rf_psigamma(x: f64, deriv: f64) => psigamma;
    Rf_beta(a: f64, b: f64) => beta;
    Rf_lbeta(a: f64, b: f64) => lbeta;
    Rf_choose(n: f64, k: f64) => choose;
    Rf_lchoose(n: f64, k: f64) => lchoose;
    Rf_bessel_i(x: f64, alpha: f64, expo: f64) => bessel_i;
    Rf_bessel_j(x: f64, alpha: f64) => bessel_j;
    Rf_bessel_k(x: f64, alpha: f64, expo: f64) => bessel_k;
    Rf_bessel_y(x: f64, alpha: f64) => bessel_y;
    Rf_pythag(a: f64, b: f64) => pythag;
    Rf_fmax2(x: f64, y: f64) => fmax2;
    Rf_fmin2(x: f64, y: f64) => fmin2;
    Rf_sign(x: f64) => sign;
    Rf_fprec(x: f64, digits: f64) => fprec;
    Rf_fround(x: f64, digits: f64) => fround;
    Rf_fsign(x: f64, y: f64) => fsign;
    Rf_ftrunc(x: f64) => ftrunc;
    Rf_log1pmx(x: f64) => log1pmx;
    Rf_log1pexp(x: f64) => log1pexp;
    Rf_log1mexp(x: f64) => log1mexp;
    Rf_logspace_add(logx: f64, logy: f64) => logspace_add;
    Rf_logspace_sub(logx: f64, logy: f64) => logspace_sub;
    Rf_pow1p(x: f64, y: f64) => pow1p;
    R_pow(x: f64, y: f64) => r_pow;
    cospi(x: f64) => cospi_;
    sinpi(x: f64) => sinpi_;
    tanpi(x: f64) => tanpi_;
    Rtanpi(x: f64) => tanpi_;
    Rlog1p(x: f64) => ln_1p;
}

export_random! {
    Rf_rnorm(mu, sigma) => rnorm;
    Rf_runif(a, b) => runif;
    Rf_rgamma(shape, scale) => rgamma;
    Rf_rbeta(a, b) => rbeta;
    Rf_rnbeta(a, b, ncp) => rnbeta;
    Rf_rlnorm(meanlog, sdlog) => rlnorm;
    Rf_rchisq(df) => rchisq;
    Rf_rnchisq(df, ncp) => rnchisq;
    Rf_rf(m, n) => rf;
    Rf_rt(df) => rt;
    Rf_rbinom(n, p) => rbinom;
    Rf_rcauchy(location, scale) => rcauchy;
    Rf_rexp(scale) => rexp;
    Rf_rgeom(p) => rgeom;
    Rf_rhyper(r, b, n) => rhyper;
    Rf_rnbinom(size, prob) => rnbinom;
    Rf_rnbinom_mu(size, mu) => rnbinom_mu;
    Rf_rpois(mu) => rpois;
    Rf_rweibull(shape, scale) => rweibull;
    Rf_rlogis(location, scale) => rlogis;
    Rf_rwilcox(m, n) => rwilcox;
    Rf_rsignrank(n) => rsignrank;
}

#[unsafe(no_mangle)]
pub extern "C" fn R_pow_di(x: f64, n: c_int) -> f64 {
    r_pow_di(x, n)
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
pub unsafe extern "C" fn Rf_lgammafn_sign(x: f64, sign: *mut c_int) -> f64 {
    let (value, value_sign) = lgammafn_sign(x);
    if !sign.is_null() {
        *sign = value_sign;
    }
    value
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_logspace_sum(values: *const f64, count: c_int) -> f64 {
    if values.is_null() || count <= 0 {
        return f64::NEG_INFINITY;
    }
    logspace_sum(std::slice::from_raw_parts(values, count as usize))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_pnorm_both(
    x: f64,
    cumulative: *mut f64,
    complementary: *mut f64,
    tail: c_int,
    log: c_int,
) {
    let (lower, upper) = pnorm_both(x, tail, log != 0);
    if !cumulative.is_null() {
        *cumulative = lower;
    }
    if !complementary.is_null() {
        *complementary = upper;
    }
}

/// Amos's interface to the polygamma functions: `ans[j]` receives
/// `(-1)^(k+1) / k! * psi^(k)(x)` for `k = n + j`, `j < m`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_dpsifn(
    x: f64,
    n: c_int,
    kode: c_int,
    m: c_int,
    ans: *mut f64,
    nz: *mut c_int,
    ierr: *mut c_int,
) {
    let mut values = vec![0.0; m.max(0) as usize];
    let status = dpsifn(x, n, kode, &mut values);
    if !ans.is_null() {
        std::ptr::copy_nonoverlapping(values.as_ptr(), ans, values.len());
    }
    if !nz.is_null() {
        *nz = 0;
    }
    if !ierr.is_null() {
        *ierr = status;
    }
}

/// The `_ex` variants take a work array so that callers can avoid an allocation per call;
/// ry's implementation needs no workspace, so the array goes unused.
#[unsafe(no_mangle)]
pub extern "C" fn Rf_bessel_i_ex(x: f64, alpha: f64, expo: f64, _work: *mut f64) -> f64 {
    bessel_i(x, alpha, expo)
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_bessel_j_ex(x: f64, alpha: f64, _work: *mut f64) -> f64 {
    bessel_j(x, alpha)
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_bessel_k_ex(x: f64, alpha: f64, expo: f64, _work: *mut f64) -> f64 {
    bessel_k(x, alpha, expo)
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_bessel_y_ex(x: f64, alpha: f64, _work: *mut f64) -> f64 {
    bessel_y(x, alpha)
}

/// Draws one multinomial vector of `n` trials over `count` categories into `result`. An
/// invalid probability marks the result `NA`, and probabilities that do not sum to one are an
/// error, both as in GNU R.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_rmultinom(
    n: c_int,
    probabilities: *mut f64,
    count: c_int,
    result: *mut c_int,
) {
    if count < 1 || probabilities.is_null() || result.is_null() {
        return;
    }
    let probabilities = std::slice::from_raw_parts(probabilities, count as usize);
    let result = std::slice::from_raw_parts_mut(result, count as usize);
    enter(|| rmultinom(&mut runtime().rng, n, probabilities, result))
}

/// GNU R frees the tables `dwilcox` and friends cache; ry's tables are rebuilt per call.
#[unsafe(no_mangle)]
pub extern "C" fn wilcox_free() {}

#[unsafe(no_mangle)]
pub extern "C" fn signrank_free() {}


// Random variates. Each generator consumes uniforms in the order of the published algorithm
// GNU R implements, so a seed gives GNU R's stream.

pub fn rnorm(rng: &mut Rng, mu: f64, sigma: f64) -> f64 {
    if mu.is_nan() || !finite(sigma) || sigma < 0.0 {
        return f64::NAN;
    }
    if sigma == 0.0 || !finite(mu) {
        mu
    } else {
        mu + sigma * rng.norm_rand()
    }
}

pub fn runif(rng: &mut Rng, a: f64, b: f64) -> f64 {
    if !finite(a) || !finite(b) || b < a {
        return f64::NAN;
    }
    if a == b {
        return a;
    }
    let mut u;
    loop {
        u = rng.unif_rand();
        if u > 0.0 && u < 1.0 {
            break;
        }
    }
    a + (b - a) * u
}

pub fn rlnorm(rng: &mut Rng, meanlog: f64, sdlog: f64) -> f64 {
    if meanlog.is_nan() || !finite(sdlog) || sdlog < 0.0 {
        return f64::NAN;
    }
    rnorm(rng, meanlog, sdlog).exp()
}

pub fn rexp(rng: &mut Rng, scale: f64) -> f64 {
    if !finite(scale) || scale <= 0.0 {
        if scale == 0.0 {
            return 0.0;
        }
        return f64::NAN;
    }
    scale * rng.exp_rand()
}

/// Gamma variates: Ahrens and Dieter's GS (1974) for `a < 1` and GD (1982) otherwise.
pub fn rgamma(rng: &mut Rng, a: f64, scale: f64) -> f64 {
    const SQRT32: f64 = 5.656854;
    const EXP_M1: f64 = 0.36787944117144233;
    const Q1: f64 = 0.04166669;
    const Q2: f64 = 0.02083148;
    const Q3: f64 = 0.00801191;
    const Q4: f64 = 0.00144121;
    const Q5: f64 = -7.388e-5;
    const Q6: f64 = 2.4511e-4;
    const Q7: f64 = 2.424e-4;
    const A1: f64 = 0.3333333;
    const A2: f64 = -0.250003;
    const A3: f64 = 0.2000062;
    const A4: f64 = -0.1662921;
    const A5: f64 = 0.1423657;
    const A6: f64 = -0.1367177;
    const A7: f64 = 0.1233795;
    if a.is_nan() || scale.is_nan() {
        return f64::NAN;
    }
    if a <= 0.0 || scale <= 0.0 {
        if scale == 0.0 || a == 0.0 {
            return 0.0;
        }
        return f64::NAN;
    }
    if !finite(a) || !finite(scale) {
        return f64::INFINITY;
    }
    if a < 1.0 {
        let e = 1.0 + EXP_M1 * a;
        loop {
            let p = e * rng.unif_rand();
            if p >= 1.0 {
                let x = -((e - p) / a).ln();
                if rng.exp_rand() >= (1.0 - a) * x.ln() {
                    return scale * x;
                }
            } else {
                let x = (p.ln() / a).exp();
                if rng.exp_rand() >= x {
                    return scale * x;
                }
            }
        }
    }
    let s2 = a - 0.5;
    let s = s2.sqrt();
    let d = SQRT32 - s * 12.0;
    let mut t = rng.norm_rand();
    let x = s + 0.5 * t;
    let ret_val = x * x;
    if t >= 0.0 {
        return scale * ret_val;
    }
    let mut u = rng.unif_rand();
    if d * u <= t * t * t {
        return scale * ret_val;
    }
    let r = 1.0 / a;
    let q0 = ((((((Q7 * r + Q6) * r + Q5) * r + Q4) * r + Q3) * r + Q2) * r + Q1) * r;
    let (b, si, c);
    if a <= 3.686 {
        b = 0.463 + s + 0.178 * s2;
        si = 1.235;
        c = 0.195 / s - 0.079 + 0.16 * s;
    } else if a <= 13.022 {
        b = 1.654 + 0.0076 * s2;
        si = 1.68 / s + 0.275;
        c = 0.062 / s + 0.024;
    } else {
        b = 1.77;
        si = 0.75;
        c = 0.1515 / s;
    }
    let quotient = |t: f64| {
        let v = t / (s + s);
        if v.abs() <= 0.25 {
            q0 + 0.5
                * t
                * t
                * ((((((A7 * v + A6) * v + A5) * v + A4) * v + A3) * v + A2) * v + A1)
                * v
        } else {
            q0 - s * t + 0.25 * t * t + (s2 + s2) * (1.0 + v).ln()
        }
    };
    if x > 0.0 && (1.0 - u).ln() <= quotient(t) {
        return scale * ret_val;
    }
    loop {
        let e = rng.exp_rand();
        u = rng.unif_rand();
        u = u + u - 1.0;
        t = if u < 0.0 { b - si * e } else { b + si * e };
        if t >= -0.71874483771719 {
            let q = quotient(t);
            if q > 0.0 {
                let w = q.exp_m1();
                if c * u.abs() <= w * (e - 0.5 * t * t).exp() {
                    break;
                }
            }
        }
    }
    let x = s + 0.5 * t;
    scale * x * x
}

pub fn rchisq(rng: &mut Rng, df: f64) -> f64 {
    if !finite(df) || df < 0.0 {
        return f64::NAN;
    }
    rgamma(rng, df / 2.0, 2.0)
}

pub fn rnchisq(rng: &mut Rng, df: f64, lambda: f64) -> f64 {
    if !finite(df) || !finite(lambda) || df < 0.0 || lambda < 0.0 {
        return f64::NAN;
    }
    if lambda == 0.0 {
        return if df == 0.0 {
            0.0
        } else {
            rgamma(rng, df / 2.0, 2.0)
        };
    }
    let mut r = rpois(rng, lambda / 2.0);
    if r > 0.0 {
        r = rchisq(rng, 2.0 * r);
    }
    if df > 0.0 {
        r += rgamma(rng, df / 2.0, 2.0);
    }
    r
}

/// A noncentral beta variate as `X / (X + Y)`, with `X` a noncentral chi-squared on `2 a`
/// degrees of freedom and `Y` a central one on `2 b`. GNU R declares this function but leaves
/// it undefined, so `rbeta(n, a, b, ncp)` there is R code drawing the same construction.
pub fn rnbeta(rng: &mut Rng, a: f64, b: f64, ncp: f64) -> f64 {
    let x = rnchisq(rng, 2.0 * a, ncp);
    x / (x + rchisq(rng, 2.0 * b))
}

/// Beta variates by Cheng's algorithms BB (both parameters above one) and BC (1978).
pub fn rbeta(rng: &mut Rng, aa: f64, bb: f64) -> f64 {
    const EXPMAX: f64 = f64::MAX_EXP as f64 * M_LN2;
    if aa.is_nan() || bb.is_nan() || aa < 0.0 || bb < 0.0 {
        return f64::NAN;
    }
    if !finite(aa) && !finite(bb) {
        return 0.5;
    }
    if aa == 0.0 && bb == 0.0 {
        return if rng.unif_rand() < 0.5 { 0.0 } else { 1.0 };
    }
    if !finite(aa) || bb == 0.0 {
        return 1.0;
    }
    if !finite(bb) || aa == 0.0 {
        return 0.0;
    }
    let a = fmin2(aa, bb);
    let b = fmax2(aa, bb);
    let alpha = a + b;
    let v_w = |u1: f64, beta: f64, scale: f64| {
        let v = beta * (u1 / (1.0 - u1)).ln();
        let w = if v <= EXPMAX {
            let w = scale * v.exp();
            if w.is_infinite() { DBL_MAX } else { w }
        } else {
            DBL_MAX
        };
        (v, w)
    };
    if a <= 1.0 {
        let beta = 1.0 / a;
        let delta = 1.0 + b - a;
        let k1 = delta * (0.0138889 + 0.0416667 * a) / (b * beta - 0.777778);
        let k2 = 0.25 + (0.5 + 0.25 / delta) * a;
        let w;
        loop {
            let u1 = rng.unif_rand();
            let u2 = rng.unif_rand();
            let z;
            if u1 < 0.5 {
                let y = u1 * u2;
                z = u1 * y;
                if 0.25 * u2 + z - y >= k1 {
                    continue;
                }
            } else {
                z = u1 * u1 * u2;
                if z <= 0.25 {
                    w = v_w(u1, beta, b).1;
                    break;
                }
                if z >= k2 {
                    continue;
                }
            }
            let (v, candidate) = v_w(u1, beta, b);
            if alpha * ((alpha / (a + candidate)).ln() + v) - 1.3862944 >= z.ln() {
                w = candidate;
                break;
            }
        }
        if aa == a { a / (a + w) } else { w / (a + w) }
    } else {
        let beta = ((alpha - 2.0) / (2.0 * a * b - alpha)).sqrt();
        let gamma = a + 1.0 / beta;
        let mut w;
        loop {
            let u1 = rng.unif_rand();
            let u2 = rng.unif_rand();
            let (v, candidate) = v_w(u1, beta, a);
            w = candidate;
            let z = u1 * u1 * u2;
            let r = gamma * v - 1.3862944;
            let s = a + r - w;
            if s + 2.609438 >= 5.0 * z {
                break;
            }
            let t = z.ln();
            if s > t {
                break;
            }
            if r + alpha * (alpha / (b + w)).ln() >= t {
                break;
            }
        }
        if aa != a { b / (b + w) } else { w / (b + w) }
    }
}

pub fn rf(rng: &mut Rng, m: f64, n: f64) -> f64 {
    if m.is_nan() || n.is_nan() || m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    let numerator = if finite(m) { rchisq(rng, m) / m } else { 1.0 };
    let denominator = if finite(n) { rchisq(rng, n) / n } else { 1.0 };
    numerator / denominator
}

pub fn rt(rng: &mut Rng, df: f64) -> f64 {
    if df.is_nan() || df <= 0.0 {
        return f64::NAN;
    }
    if !finite(df) {
        return rng.norm_rand();
    }
    let numerator = rng.norm_rand();
    numerator / (rchisq(rng, df) / df).sqrt()
}

pub fn rcauchy(rng: &mut Rng, location: f64, scale: f64) -> f64 {
    if location.is_nan() || !finite(scale) || scale < 0.0 {
        return f64::NAN;
    }
    if scale == 0.0 || !finite(location) {
        location
    } else {
        location + scale * (M_PI * rng.unif_rand()).tan()
    }
}

pub fn rweibull(rng: &mut Rng, shape: f64, scale: f64) -> f64 {
    if !finite(shape) || !finite(scale) || shape <= 0.0 || scale <= 0.0 {
        if scale == 0.0 {
            return 0.0;
        }
        return f64::NAN;
    }
    scale * (-(rng.unif_rand()).ln()).powf(1.0 / shape)
}

pub fn rlogis(rng: &mut Rng, location: f64, scale: f64) -> f64 {
    if location.is_nan() || !finite(scale) {
        return f64::NAN;
    }
    if scale == 0.0 || !finite(location) {
        return location;
    }
    let u = rng.unif_rand();
    location + scale * (u / (1.0 - u)).ln()
}

/// Poisson variates: Ahrens and Dieter's PD (1982) for `mu >= 10`, table lookup inversion
/// below.
pub fn rpois(rng: &mut Rng, mu: f64) -> f64 {
    const A0: f64 = -0.5;
    const A1: f64 = 0.3333333;
    const A2: f64 = -0.2500068;
    const A3: f64 = 0.2000118;
    const A4: f64 = -0.1661269;
    const A5: f64 = 0.1421878;
    const A6: f64 = -0.1384794;
    const A7: f64 = 0.1250060;
    const ONE_7: f64 = 0.1428571428571428571;
    const ONE_12: f64 = 0.0833333333333333333;
    const ONE_24: f64 = 0.0416666666666666667;
    const FACT: [f64; 10] = [
        1.0, 1.0, 2.0, 6.0, 24.0, 120.0, 720.0, 5040.0, 40320.0, 362880.0,
    ];
    if !finite(mu) || mu < 0.0 {
        return f64::NAN;
    }
    if mu <= 0.0 {
        return 0.0;
    }
    if mu < 10.0 {
        // Inversion against the cumulative probabilities, extended as far as needed.
        let m = (mu as i32).max(1) as usize;
        let p0 = (-mu).exp();
        let mut pp = [0.0; 36];
        let mut l = 0;
        let mut p = p0;
        let mut q = p0;
        loop {
            let u = rng.unif_rand();
            if u <= p0 {
                return 0.0;
            }
            if l > 0 {
                let j = if u > 0.458 { l.min(m) } else { 1 };
                for (k, cumulative) in pp.iter().enumerate().take(l + 1).skip(j) {
                    if u <= *cumulative {
                        return k as f64;
                    }
                }
                if l == 35 {
                    continue;
                }
            }
            l += 1;
            for (k, cumulative) in pp.iter_mut().enumerate().skip(l) {
                p *= mu / k as f64;
                q += p;
                *cumulative = q;
                if u <= q {
                    return k as f64;
                }
            }
            l = 35;
        }
    }
    let s = mu.sqrt();
    let d = 6.0 * mu * mu;
    let big_l = (mu - 1.1484).floor();
    let mut pois = -1.0;
    let g = mu + s * rng.norm_rand();
    let mut u = 0.0;
    let mut fk = 0.0;
    let mut difmuk = 0.0;
    if g >= 0.0 {
        pois = g.floor();
        if pois >= big_l {
            return pois;
        }
        fk = pois;
        difmuk = mu - fk;
        u = rng.unif_rand();
        if d * u >= difmuk * difmuk * difmuk {
            return pois;
        }
    }
    let omega = M_1_SQRT_2PI / s;
    let b1 = ONE_24 / mu;
    let b2 = 0.3 * b1 * b1;
    let c3 = ONE_7 * b1 * b2;
    let c2 = b2 - 15.0 * c3;
    let c1 = b1 - 6.0 * b2 + 45.0 * c3;
    let c0 = 1.0 - b1 + 3.0 * b2 - 15.0 * c3;
    let c = 0.1069 / mu;
    // Ahrens and Dieter's procedure F: the Poisson and the discrete normal densities at pois.
    let procedure_f = |pois: f64, fk: f64, difmuk: f64| {
        let (px, py);
        if pois < 10.0 {
            px = -mu;
            py = mu.powf(pois) / FACT[pois as usize];
        } else {
            let mut del = ONE_12 / fk;
            del *= 1.0 - 4.8 * del * del;
            let v = difmuk / fk;
            if v.abs() <= 0.25 {
                px = fk
                    * v
                    * v
                    * (((((((A7 * v + A6) * v + A5) * v + A4) * v + A3) * v + A2) * v + A1) * v
                        + A0)
                    - del;
            } else {
                px = fk * (1.0 + v).ln() - difmuk - del;
            }
            py = M_1_SQRT_2PI / fk.sqrt();
        }
        let x = (0.5 - difmuk) / s;
        let xx = x * x;
        let fx = -0.5 * xx;
        let fy = omega * (((c3 * xx + c2) * xx + c1) * xx + c0);
        (px, py, fx, fy)
    };
    if g >= 0.0 {
        let (px, py, fx, fy) = procedure_f(pois, fk, difmuk);
        if fy - u * fy <= py * (px - fx).exp() {
            return pois;
        }
    }
    loop {
        let e = rng.exp_rand();
        u = 2.0 * rng.unif_rand() - 1.0;
        let t = 1.8 + fsign(e, u);
        if t > -0.6744 {
            pois = (mu + s * t).floor();
            fk = pois;
            difmuk = mu - fk;
            let (px, py, fx, fy) = procedure_f(pois, fk, difmuk);
            if c * u.abs() <= py * (px + e).exp() - fy * (fx + e).exp() {
                return pois;
            }
        }
    }
}

/// Binomial variates by Kachitvichyanukul and Schmeiser's BTPE (1988), inversion when the
/// mean is below 30.
pub fn rbinom(rng: &mut Rng, nin: f64, pp: f64) -> f64 {
    if !finite(nin) {
        return f64::NAN;
    }
    let r = forceint(nin);
    if r != nin || !finite(pp) || r < 0.0 || !(0.0..=1.0).contains(&pp) {
        return f64::NAN;
    }
    if r == 0.0 || pp == 0.0 {
        return 0.0;
    }
    if pp == 1.0 {
        return r;
    }
    if r >= i32::MAX as f64 {
        return qbinom(rng.unif_rand(), r, pp, false, false);
    }
    let n = r as i32;
    let p = fmin2(pp, 1.0 - pp);
    let q = 1.0 - p;
    let np = n as f64 * p;
    let r = p / q;
    let g = r * (n as f64 + 1.0);
    let ix: i32;
    if np < 30.0 {
        let qn = r_pow_di(q, n);
        'outer: loop {
            let mut ix_ = 0;
            let mut f = qn;
            let mut u = rng.unif_rand();
            loop {
                if u < f {
                    ix = ix_;
                    break 'outer;
                }
                if ix_ > 110 {
                    break;
                }
                u -= f;
                ix_ += 1;
                f *= g / ix_ as f64 - r;
            }
        }
    } else {
        let fm = np + p;
        let m = fm as i32;
        let npq = np * q;
        let p1 = ((2.195 * npq.sqrt() - 4.6 * q) as i32) as f64 + 0.5;
        let xm = m as f64 + 0.5;
        let xl = xm - p1;
        let xr = xm + p1;
        let c = 0.134 + 20.5 / (15.3 + m as f64);
        let mut al = (fm - xl) / (fm - xl * p);
        let xll = al * (1.0 + 0.5 * al);
        al = (xr - fm) / (xr * q);
        let xlr = al * (1.0 + 0.5 * al);
        let p2 = p1 * (1.0 + c + c);
        let p3 = p2 + c / xll;
        let p4 = p3 + c / xlr;
        loop {
            let u = rng.unif_rand() * p4;
            let mut v = rng.unif_rand();
            let candidate: i32;
            if u <= p1 {
                ix = (xm - p1 * v + u) as i32;
                break;
            }
            if u <= p2 {
                let x = xl + (u - p1) / c;
                v = v * c + 1.0 - (xm - x).abs() / p1;
                if v > 1.0 || v <= 0.0 {
                    continue;
                }
                candidate = x as i32;
            } else if u > p3 {
                candidate = (xr - v.ln() / xlr) as i32;
                if candidate > n {
                    continue;
                }
                v *= (u - p3) * xlr;
            } else {
                candidate = (xl + v.ln() / xll) as i32;
                if candidate < 0 {
                    continue;
                }
                v *= (u - p2) * xll;
            }
            let k = (candidate - m).abs();
            if k <= 20 || k as f64 >= npq / 2.0 - 1.0 {
                // Explicit evaluation of the density ratio.
                let mut f = 1.0;
                if m < candidate {
                    for i in m + 1..=candidate {
                        f *= g / i as f64 - r;
                    }
                } else if m > candidate {
                    for i in candidate + 1..=m {
                        f /= g / i as f64 - r;
                    }
                }
                if v <= f {
                    ix = candidate;
                    break;
                }
            } else {
                // Squeeze with bounds on log f(x), then Stirling's formula.
                let k = k as f64;
                let amaxp = (k / npq) * ((k * (k / 3.0 + 0.625) + 0.1666666666666) / npq + 0.5);
                let ynorm = -k * k / (2.0 * npq);
                let alv = v.ln();
                if alv < ynorm - amaxp {
                    ix = candidate;
                    break;
                }
                if alv <= ynorm + amaxp {
                    let x1 = candidate as f64 + 1.0;
                    let f1 = fm + 1.0;
                    let z = n as f64 + 1.0 - fm;
                    let w = n as f64 - candidate as f64 + 1.0;
                    let z2 = z * z;
                    let x2 = x1 * x1;
                    let f2 = f1 * f1;
                    let w2 = w * w;
                    let correction = |a: f64, a2: f64| {
                        (13860.0 - (462.0 - (132.0 - (99.0 - 140.0 / a2) / a2) / a2) / a2)
                            / a
                            / 166320.0
                    };
                    if alv
                        <= xm * (f1 / x1).ln()
                            + (n as f64 - m as f64 + 0.5) * (z / w).ln()
                            + (candidate - m) as f64 * (w * p / (x1 * q)).ln()
                            + correction(f1, f2)
                            + correction(z, z2)
                            + correction(x1, x2)
                            + correction(w, w2)
                    {
                        ix = candidate;
                        break;
                    }
                }
            }
        }
    }
    let ix = if pp > 0.5 { n - ix } else { ix };
    ix as f64
}

pub fn rgeom(rng: &mut Rng, p: f64) -> f64 {
    if !finite(p) || p <= 0.0 || p > 1.0 {
        return f64::NAN;
    }
    let mean = rng.exp_rand() * ((1.0 - p) / p);
    rpois(rng, mean)
}

pub fn rnbinom(rng: &mut Rng, size: f64, prob: f64) -> f64 {
    if !finite(prob) || size.is_nan() || size <= 0.0 || prob <= 0.0 || prob > 1.0 {
        return f64::NAN;
    }
    let size = if finite(size) { size } else { DBL_MAX / 2.0 };
    if prob == 1.0 {
        return 0.0;
    }
    let mean = rgamma(rng, size, (1.0 - prob) / prob);
    rpois(rng, mean)
}

pub fn rnbinom_mu(rng: &mut Rng, size: f64, mu: f64) -> f64 {
    if !finite(mu) || size.is_nan() || size <= 0.0 || mu < 0.0 {
        return f64::NAN;
    }
    let size = if finite(size) { size } else { DBL_MAX / 2.0 };
    if mu == 0.0 {
        return 0.0;
    }
    let mean = rgamma(rng, size, mu / size);
    rpois(rng, mean)
}

/// `log(i!)`, exactly tabulated to 7 and by Stirling's series beyond.
fn afc(i: i32) -> f64 {
    const AL: [f64; 8] = [
        0.0,
        0.0,
        0.69314718055994530941723212145817,
        1.79175946922805500081247735838070,
        3.17805383034794561964694160129705,
        4.78749174278204599424770093452324,
        6.57925121201010099506017829290394,
        8.52516136106541430016553103634712,
    ];
    if i < 0 {
        boundary::warn(format!("rhyper.c: afc(i), i={i} < 0 -- SHOULD NOT HAPPEN!"));
        return -1.0;
    }
    if i <= 7 {
        return AL[i as usize];
    }
    let di = i as f64;
    let i2 = di * di;
    (di + 0.5) * di.ln() - di + M_LN_SQRT_2PI + (0.0833333333333333 - 0.00277777777777778 / i2) / di
}

/// Hypergeometric variates by Kachitvichyanukul and Schmeiser's HIN (inversion) and H2PE
/// (1985): the number of red balls among `kk` drawn from `nn1` red and `nn2` blue.
pub fn rhyper(rng: &mut Rng, nn1in: f64, nn2in: f64, kkin: f64) -> f64 {
    const DELTAL: f64 = 0.0078;
    const DELTAU: f64 = 0.0034;
    const SCALE: f64 = 1e25;
    const CON: f64 = 57.5646273248511421;
    if !finite(nn1in) || !finite(nn2in) || !finite(kkin) {
        return f64::NAN;
    }
    let nn1in = forceint(nn1in);
    let nn2in = forceint(nn2in);
    let kkin = forceint(kkin);
    if nn1in < 0.0 || nn2in < 0.0 || kkin < 0.0 || kkin > nn1in + nn2in {
        return f64::NAN;
    }
    if nn1in >= i32::MAX as f64 || nn2in >= i32::MAX as f64 || kkin >= i32::MAX as f64 {
        if kkin == 1.0 {
            return rbinom(rng, kkin, nn1in / (nn1in + nn2in));
        }
        return qhyper(rng.unif_rand(), nn1in, nn2in, kkin, false, false);
    }
    let nn1 = nn1in as i32;
    let nn2 = nn2in as i32;
    let kk = kkin as i32;
    let total = nn1 as f64 + nn2 as f64;
    let (n1, n2) = if nn1 <= nn2 { (nn1, nn2) } else { (nn2, nn1) };
    let k = if kk as f64 + kk as f64 >= total {
        (total - kk as f64) as i32
    } else {
        kk
    };
    let m = ((k as f64 + 1.0) * (n1 as f64 + 1.0) / (total + 2.0)) as i32;
    let minjx = 0.max(k - n2);
    let maxjx = n1.min(k);
    let ix: i32;
    if minjx == maxjx {
        ix = maxjx;
    } else if m - minjx < 10 {
        let lw = if k < n2 {
            afc(n2) + afc(n1 + n2 - k) - afc(n2 - k) - afc(n1 + n2)
        } else {
            afc(n1) + afc(k) - afc(k - n2) - afc(n1 + n2)
        };
        let w = (lw + CON).exp();
        'restart: loop {
            let mut candidate = minjx;
            let mut u = rng.unif_rand() * SCALE;
            let mut p = w;
            while u > p {
                u -= p;
                p *= (n1 as f64 - candidate as f64) * (k - candidate) as f64;
                candidate += 1;
                p = p / candidate as f64 / (n2 - k + candidate) as f64;
                if candidate > maxjx {
                    continue 'restart;
                }
            }
            ix = candidate;
            break;
        }
    } else {
        let s =
            ((total - k as f64) * k as f64 * n1 as f64 * n2 as f64 / (total - 1.0) / total / total)
                .sqrt();
        let d = ((1.5 * s) as i32) as f64 + 0.5;
        let xl = m as f64 - d + 0.5;
        let xr = m as f64 + d + 0.5;
        let a = afc(m) + afc(n1 - m) + afc(k - m) + afc(n2 - k + m);
        let kl = (a
            - afc(xl as i32)
            - afc((n1 as f64 - xl) as i32)
            - afc((k as f64 - xl) as i32)
            - afc((n2 as f64 - k as f64 + xl) as i32))
        .exp();
        let kr = (a
            - afc((xr - 1.0) as i32)
            - afc((n1 as f64 - xr + 1.0) as i32)
            - afc((k as f64 - xr + 1.0) as i32)
            - afc((n2 as f64 - k as f64 + xr - 1.0) as i32))
        .exp();
        let lamdl =
            -(xl * (n2 as f64 - k as f64 + xl) / (n1 as f64 - xl + 1.0) / (k as f64 - xl + 1.0))
                .ln();
        let lamdr =
            -((n1 as f64 - xr + 1.0) * (k as f64 - xr + 1.0) / xr / (n2 as f64 - k as f64 + xr))
                .ln();
        let p1 = d + d;
        let p2 = p1 + kl / lamdl;
        let p3 = p2 + kr / lamdr;
        let mut n_uv = 0;
        loop {
            let u = rng.unif_rand() * p3;
            let mut v = rng.unif_rand();
            n_uv += 1;
            if n_uv >= 10000 {
                boundary::warn(format!(
                    "rhyper(*, n1={nn1}, n2={nn2}, k={kk}): branch III: giving up after {n_uv} rejections"
                ));
                return f64::NAN;
            }
            let candidate: i32;
            if u < p1 {
                candidate = (xl + u) as i32;
            } else if u <= p2 {
                candidate = (xl + v.ln() / lamdl) as i32;
                if candidate < minjx {
                    continue;
                }
                v *= (u - p1) * lamdl;
            } else {
                candidate = (xr - v.ln() / lamdr) as i32;
                if candidate > maxjx {
                    continue;
                }
                v *= (u - p2) * lamdr;
            }
            let accept;
            if m < 100 || candidate <= 50 {
                let mut f = 1.0;
                if m < candidate {
                    for i in m + 1..=candidate {
                        f = f * (n1 - i + 1) as f64 * (k - i + 1) as f64
                            / (n2 - k + i) as f64
                            / i as f64;
                    }
                } else if m > candidate {
                    for i in candidate + 1..=m {
                        f = f * i as f64 * (n2 - k + i) as f64
                            / (n1 - i + 1) as f64
                            / (k - i + 1) as f64;
                    }
                }
                accept = v <= f;
            } else {
                let y = candidate as f64;
                let y1 = y + 1.0;
                let ym = y - m as f64;
                let yn = n1 as f64 - y + 1.0;
                let yk = k as f64 - y + 1.0;
                let nk = n2 as f64 - k as f64 + y1;
                let r = -ym / y1;
                let s = ym / yn;
                let t = ym / yk;
                let e = -ym / nk;
                let g = yn * yk / (y1 * nk) - 1.0;
                let dg = if g < 0.0 { 1.0 + g } else { 1.0 };
                let gu = g * (1.0 + g * (-0.5 + g / 3.0));
                let gl = gu - 0.25 * (g * g * g * g) / dg;
                let xm = m as f64 + 0.5;
                let xn = n1 as f64 - m as f64 + 0.5;
                let xk = k as f64 - m as f64 + 0.5;
                let nm = n2 as f64 - k as f64 + xm;
                let ub = y * gu - m as f64 * gl
                    + DELTAU
                    + xm * r * (1.0 + r * (-0.5 + r / 3.0))
                    + xn * s * (1.0 + s * (-0.5 + s / 3.0))
                    + xk * t * (1.0 + t * (-0.5 + t / 3.0))
                    + nm * e * (1.0 + e * (-0.5 + e / 3.0));
                let alv = v.ln();
                if alv > ub {
                    accept = false;
                } else {
                    let mut dr = xm * (r * r * r * r);
                    if r < 0.0 {
                        dr /= 1.0 + r;
                    }
                    let mut ds = xn * (s * s * s * s);
                    if s < 0.0 {
                        ds /= 1.0 + s;
                    }
                    let mut dt = xk * (t * t * t * t);
                    if t < 0.0 {
                        dt /= 1.0 + t;
                    }
                    let mut de = nm * (e * e * e * e);
                    if e < 0.0 {
                        de /= 1.0 + e;
                    }
                    if alv < ub - 0.25 * (dr + ds + dt + de) + (y + m as f64) * (gl - gu) - DELTAL {
                        accept = true;
                    } else {
                        accept = alv
                            <= a - afc(candidate)
                                - afc(n1 - candidate)
                                - afc(k - candidate)
                                - afc(n2 - k + candidate);
                    }
                }
            }
            if accept {
                ix = candidate;
                break;
            }
        }
    }
    let ix = if kk as f64 + kk as f64 >= total {
        if nn1 > nn2 { kk - nn2 + ix } else { nn1 - ix }
    } else if nn1 > nn2 {
        kk - ix
    } else {
        ix
    };
    ix as f64
}

pub fn rsignrank(rng: &mut Rng, n: f64) -> f64 {
    if n.is_nan() {
        return n;
    }
    let n = forceint(n);
    if n < 0.0 {
        return f64::NAN;
    }
    if n == 0.0 {
        return 0.0;
    }
    let mut r = 0.0;
    for i in 1..=n as i32 {
        r += i as f64 * (rng.unif_rand() + 0.5).floor();
    }
    r
}

pub fn rwilcox(rng: &mut Rng, m: f64, n: f64) -> f64 {
    if m.is_nan() || n.is_nan() {
        return m + n;
    }
    let m = forceint(m);
    let n = forceint(n);
    if m < 0.0 || n < 0.0 {
        return f64::NAN;
    }
    if m == 0.0 || n == 0.0 {
        return 0.0;
    }
    // The draw shuffles an `m + n` array, which GNU R also sizes with a C `int`.
    if !(m + n <= f64::from(i32::MAX)) {
        return f64::NAN;
    }
    let mut k = (m + n) as usize;
    let mut x: Vec<i32> = (0..k as i32).collect();
    let mut r = 0.0;
    for _ in 0..n as usize {
        let j = rng.unif_index(k as f64) as usize;
        r += x[j] as f64;
        k -= 1;
        x[j] = x[k];
    }
    r - n * (n - 1.0) / 2.0
}

/// One multinomial draw: successive binomials on the remaining probability mass.
pub fn rmultinom(
    rng: &mut Rng,
    n: c_int,
    probabilities: &[f64],
    result: &mut [c_int],
) -> ApiResult<()> {
    let count = probabilities.len().min(result.len());
    if count == 0 {
        return Ok(());
    }
    if n == heap::object::NA_INTEGER || n < 0 {
        result[0] = heap::object::NA_INTEGER;
        return Ok(());
    }
    let mut total = 0.0;
    for (k, &probability) in probabilities.iter().take(count).enumerate() {
        if !finite(probability) || !(0.0..=1.0).contains(&probability) {
            result[k] = heap::object::NA_INTEGER;
            return Ok(());
        }
        total += probability;
        result[k] = 0;
    }
    if (total - 1.0).abs() > 1e-7 {
        return Err(error(format!(
            "rbinom: probability sum should be 1, but is {}",
            format_g(total)
        )));
    }
    if n == 0 || (count == 1 && total == 0.0) {
        return Ok(());
    }
    let mut left = n;
    for k in 0..count - 1 {
        if probabilities[k] != 0.0 {
            let p = probabilities[k] / total;
            result[k] = if p < 1.0 {
                rbinom(rng, left as f64, p) as c_int
            } else {
                left
            };
            left -= result[k];
        } else {
            result[k] = 0;
        }
        if left <= 0 {
            return Ok(());
        }
        total -= probabilities[k];
    }
    result[count - 1] = left;
    Ok(())
}

/// C's `%g` for the few messages that print a number that way.
fn format_g(value: f64) -> String {
    if value == 0.0 || (1e-4..1e6).contains(&value.abs()) {
        let text = format!("{value:.6}");
        let text = text.trim_end_matches('0').trim_end_matches('.');
        let digits = text.chars().filter(char::is_ascii_digit).count();
        if digits <= 6 {
            return text.to_owned();
        }
        let precision = 6usize.saturating_sub(value.abs().log10().floor() as usize + 1);
        let text = format!("{value:.precision$}");
        return text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    format!("{value:e}")
}

