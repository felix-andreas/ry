//! R's mathematical library through the C API `Rmath.h` declares. The functions themselves
//! are `interp::rmath`, shared with the interpreter's builtins; this module converts C's `int`
//! flags, draws random variates from the session's generator, and exports the symbols.

use {
    crate::boundary::{self, enter, error},
    heap::object::NA_INTEGER,
    interp::{
        distributions::{self, SessionDeviates},
        rmath::*,
    },
    std::ffi::c_int,
};

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
                enter(|| distributions::$target(&mut SessionDeviates, $($argument),*))
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
    if n == NA_INTEGER || n < 0 {
        result[0] = NA_INTEGER;
        return;
    }
    let mut total = 0.0;
    for (slot, &probability) in result.iter_mut().zip(probabilities) {
        if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
            *slot = NA_INTEGER;
            return;
        }
        total += probability;
        *slot = 0;
    }
    if n == 0 || (count == 1 && total == 0.0) {
        return;
    }
    enter(|| {
        if (total - 1.0).abs() > 1e-7 {
            return Err(error(format!(
                "rbinom: probability sum should be 1, but is {}",
                format_g(total)
            )));
        }
        distributions::rmultinom(&mut SessionDeviates, n, probabilities, result)
    })
}

/// GNU R frees the tables `dwilcox` and friends cache; ry's tables are rebuilt per call.
#[unsafe(no_mangle)]
pub extern "C" fn wilcox_free() {}

#[unsafe(no_mangle)]
pub extern "C" fn signrank_free() {}

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
