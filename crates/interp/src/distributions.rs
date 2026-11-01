//! Probability distributions: the density, distribution, quantile and random generation
//! functions behind `.Internal(dnorm(...))` and its relatives, the Bessel functions and
//! `psigamma`.
//!
//! Each distribution is a set of plain functions with GNU R's Rmath conventions (`lower_tail`,
//! `log_p`, NaN for invalid parameters), so the C API can reuse them for `dnorm4`, `pnorm5` and
//! the other Rmath entry points. The builtins vectorize them the way GNU R's `math2`, `math3` and
//! `math4` do: arguments recycle to the longest, the result takes every attribute of the first
//! argument of that length, an `NA` argument gives `NA`, and a NaN computed from non-NaN arguments
//! raises "NaNs produced".
//!
//! The algorithms are the published ones GNU R's results come from, implemented from the papers:
//! Cody (1969) for the normal distribution function, Wichura's AS 241 for its quantile, Loader's
//! (2000) saddle point expansion for the binomial and Poisson densities, continued fractions and
//! series for the incomplete gamma and beta functions, Lenth's AS 226 and AS 243 for the
//! noncentral beta and t distributions, Copenhaver and Holland (1988) for the studentized range,
//! and Ahrens and Dieter, Cheng, and Kachitvichyanukul and Schmeiser for the random generators,
//! which consume the uniform stream in exactly GNU R's order so that `set.seed` reproduces its
//! draws.
//!
//! Numerical warnings that Rmath reports through `MATHLIB_WARNING` (a non-integer count, lost
//! precision) are collected while the functions run and raised by the builtin afterwards, which
//! keeps the functions free of interpreter state.

use crate::{
    builtin_libraries::NativeFn,
    builtins::{Arg, BuiltinDef, internal},
    coerce::coerce_vector,
    conditions::warning,
    random,
    unwind::{R, error},
    values::{
        alloc, as_integer_scalar, copy_attributes, get_attr, inherits, integer_vector, ints,
        ints_mut, length, list_vector, materialize, nil, reals, reals_mut, set_attr, symbols,
    },
};
use heap::object::{NA_INTEGER, NA_REAL, Sexp, SexpType, is_na_real};
use std::{
    cell::RefCell,
    f64::consts::{LN_2, PI, SQRT_2, TAU},
};

unsafe extern "C" {
    fn lgamma(x: f64) -> f64;
    fn tgamma(x: f64) -> f64;
}

/// Generates the `.Internal` table and the stats routine table from one list, so that both
/// entry points vectorize every function identically.
macro_rules! distributions {
    ($($name:literal $arity:literal $glue:ident $shape:tt $function:expr;)*) => {
        /// The `.Internal` forms, which GNU R 4.3 still keeps in base beside the stats routines,
        /// and the Bessel functions and `psigamma`, which only base has.
        #[rustfmt::skip]
        pub const BUILTINS: &[BuiltinDef] = &[
            $(internal($name, $arity, |call| unsafe {
                distributions!(@call $glue $shape $function, call.args, Interface::Internal)
            }),)*
            internal("besselJ", 2, |call| unsafe { math(call.args, Interface::Internal, 2, |v, _| bessel_j(v[0], v[1])) }),
            internal("besselY", 2, |call| unsafe { math(call.args, Interface::Internal, 2, |v, _| bessel_y(v[0], v[1])) }),
            internal("besselI", 3, |call| unsafe { math(call.args, Interface::Internal, 2, |v, f| bessel_i(v[0], v[1], f[0] == 2)) }),
            internal("besselK", 3, |call| unsafe { math(call.args, Interface::Internal, 2, |v, f| bessel_k(v[0], v[1], f[0] == 2)) }),
            internal("psigamma", 2, |call| unsafe { math(call.args, Interface::Internal, 2, |v, _| psigamma(v[0], v[1])) }),
            internal("dtukey", 5, |_| Err(error("unimplemented real function of 4 numeric arguments"))),
        ];

        /// The stats package's `.Call` and `.External` routines, which `builtin_libraries`
        /// binds to the `C_` symbols of stats' R code.
        #[rustfmt::skip]
        pub const ROUTINES: &[(&str, NativeFn)] = &[
            $(($name, |call| unsafe {
                distributions!(@call $glue $shape $function, call.args, Interface::Stats)
            }),)*
            ("rmultinom", |call| unsafe { rmultinom_routine(call.arg(0), call.arg(1), call.arg(2)) }),
            ("signrank_free", |_| Ok(nil())),
            ("wilcox_free", |_| Ok(nil())),
        ];
    };
    (@call math $vectors:tt $function:expr, $args:expr, $interface:expr) => {
        math($args, $interface, $vectors, $function)
    };
    (@call random integer $function:expr, $args:expr, $interface:expr) => {
        random_vector($args, $interface, true, $function)
    };
    (@call random real $function:expr, $args:expr, $interface:expr) => {
        random_vector($args, $interface, false, $function)
    };
}

distributions! {
    "dchisq" 3 math 2 |v, f| dchisq(v[0], v[1], f[0] != 0);
    "pchisq" 4 math 2 |v, f| pchisq(v[0], v[1], f[0] != 0, f[1] != 0);
    "qchisq" 4 math 2 |v, f| qchisq(v[0], v[1], f[0] != 0, f[1] != 0);
    "dexp" 3 math 2 |v, f| dexp(v[0], v[1], f[0] != 0);
    "pexp" 4 math 2 |v, f| pexp(v[0], v[1], f[0] != 0, f[1] != 0);
    "qexp" 4 math 2 |v, f| qexp(v[0], v[1], f[0] != 0, f[1] != 0);
    "dgeom" 3 math 2 |v, f| dgeom(v[0], v[1], f[0] != 0);
    "pgeom" 4 math 2 |v, f| pgeom(v[0], v[1], f[0] != 0, f[1] != 0);
    "qgeom" 4 math 2 |v, f| qgeom(v[0], v[1], f[0] != 0, f[1] != 0);
    "dpois" 3 math 2 |v, f| dpois(v[0], v[1], f[0] != 0);
    "ppois" 4 math 2 |v, f| ppois(v[0], v[1], f[0] != 0, f[1] != 0);
    "qpois" 4 math 2 |v, f| qpois(v[0], v[1], f[0] != 0, f[1] != 0);
    "dt" 3 math 2 |v, f| dt(v[0], v[1], f[0] != 0);
    "pt" 4 math 2 |v, f| pt(v[0], v[1], f[0] != 0, f[1] != 0);
    "qt" 4 math 2 |v, f| qt(v[0], v[1], f[0] != 0, f[1] != 0);
    "dsignrank" 3 math 2 |v, f| dsignrank(v[0], v[1], f[0] != 0);
    "psignrank" 4 math 2 |v, f| psignrank(v[0], v[1], f[0] != 0, f[1] != 0);
    "qsignrank" 4 math 2 |v, f| qsignrank(v[0], v[1], f[0] != 0, f[1] != 0);
    "dbeta" 4 math 3 |v, f| dbeta(v[0], v[1], v[2], f[0] != 0);
    "pbeta" 5 math 3 |v, f| pbeta(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qbeta" 5 math 3 |v, f| qbeta(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dbinom" 4 math 3 |v, f| dbinom(v[0], v[1], v[2], f[0] != 0);
    "pbinom" 5 math 3 |v, f| pbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qbinom" 5 math 3 |v, f| qbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dcauchy" 4 math 3 |v, f| dcauchy(v[0], v[1], v[2], f[0] != 0);
    "pcauchy" 5 math 3 |v, f| pcauchy(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qcauchy" 5 math 3 |v, f| qcauchy(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "df" 4 math 3 |v, f| df(v[0], v[1], v[2], f[0] != 0);
    "pf" 5 math 3 |v, f| pf(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qf" 5 math 3 |v, f| qf(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dgamma" 4 math 3 |v, f| dgamma(v[0], v[1], v[2], f[0] != 0);
    "pgamma" 5 math 3 |v, f| pgamma(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qgamma" 5 math 3 |v, f| qgamma(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dlnorm" 4 math 3 |v, f| dlnorm(v[0], v[1], v[2], f[0] != 0);
    "plnorm" 5 math 3 |v, f| plnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qlnorm" 5 math 3 |v, f| qlnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dlogis" 4 math 3 |v, f| dlogis(v[0], v[1], v[2], f[0] != 0);
    "plogis" 5 math 3 |v, f| plogis(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qlogis" 5 math 3 |v, f| qlogis(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dnbinom" 4 math 3 |v, f| dnbinom(v[0], v[1], v[2], f[0] != 0);
    "pnbinom" 5 math 3 |v, f| pnbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qnbinom" 5 math 3 |v, f| qnbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dnbinom_mu" 4 math 3 |v, f| dnbinom_mu(v[0], v[1], v[2], f[0] != 0);
    "pnbinom_mu" 5 math 3 |v, f| pnbinom_mu(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qnbinom_mu" 5 math 3 |v, f| qnbinom_mu(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dnorm" 4 math 3 |v, f| dnorm(v[0], v[1], v[2], f[0] != 0);
    "pnorm" 5 math 3 |v, f| pnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qnorm" 5 math 3 |v, f| qnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dunif" 4 math 3 |v, f| dunif(v[0], v[1], v[2], f[0] != 0);
    "punif" 5 math 3 |v, f| punif(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qunif" 5 math 3 |v, f| qunif(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dweibull" 4 math 3 |v, f| dweibull(v[0], v[1], v[2], f[0] != 0);
    "pweibull" 5 math 3 |v, f| pweibull(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qweibull" 5 math 3 |v, f| qweibull(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dnchisq" 4 math 3 |v, f| dnchisq(v[0], v[1], v[2], f[0] != 0);
    "pnchisq" 5 math 3 |v, f| pnchisq(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qnchisq" 5 math 3 |v, f| qnchisq(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dnt" 4 math 3 |v, f| dnt(v[0], v[1], v[2], f[0] != 0);
    "pnt" 5 math 3 |v, f| pnt(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qnt" 5 math 3 |v, f| qnt(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dwilcox" 4 math 3 |v, f| dwilcox(v[0], v[1], v[2], f[0] != 0);
    "pwilcox" 5 math 3 |v, f| pwilcox(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "qwilcox" 5 math 3 |v, f| qwilcox(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
    "dhyper" 5 math 4 |v, f| dhyper(v[0], v[1], v[2], v[3], f[0] != 0);
    "phyper" 6 math 4 |v, f| phyper(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "qhyper" 6 math 4 |v, f| qhyper(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "dnbeta" 5 math 4 |v, f| dnbeta(v[0], v[1], v[2], v[3], f[0] != 0);
    "pnbeta" 6 math 4 |v, f| pnbeta(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "qnbeta" 6 math 4 |v, f| qnbeta(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "dnf" 5 math 4 |v, f| dnf(v[0], v[1], v[2], v[3], f[0] != 0);
    "pnf" 6 math 4 |v, f| pnf(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "qnf" 6 math 4 |v, f| qnf(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "ptukey" 6 math 4 |v, f| ptukey(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "qtukey" 6 math 4 |v, f| qtukey(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    "rchisq" 2 random real |s, v| rchisq(s, v[0]);
    "rexp" 2 random real |s, v| rexp(s, v[0]);
    "rgeom" 2 random integer |s, v| rgeom(s, v[0]);
    "rpois" 2 random integer |s, v| rpois(s, v[0]);
    "rt" 2 random real |s, v| rt(s, v[0]);
    "rsignrank" 2 random integer |s, v| rsignrank(s, v[0]);
    "rbeta" 3 random real |s, v| rbeta(s, v[0], v[1]);
    "rbinom" 3 random integer |s, v| rbinom(s, v[0], v[1]);
    "rcauchy" 3 random real |s, v| rcauchy(s, v[0], v[1]);
    "rf" 3 random real |s, v| rf(s, v[0], v[1]);
    "rgamma" 3 random real |s, v| rgamma(s, v[0], v[1]);
    "rlnorm" 3 random real |s, v| rlnorm(s, v[0], v[1]);
    "rlogis" 3 random real |s, v| rlogis(s, v[0], v[1]);
    "rnbinom" 3 random integer |s, v| rnbinom(s, v[0], v[1]);
    "rnbinom_mu" 3 random real |s, v| rnbinom_mu(s, v[0], v[1]);
    "rnchisq" 3 random real |s, v| rnchisq(s, v[0], v[1]);
    "rnorm" 3 random real |s, v| rnorm(s, v[0], v[1]);
    "runif" 3 random real |s, v| runif(s, v[0], v[1]);
    "rweibull" 3 random real |s, v| rweibull(s, v[0], v[1]);
    "rwilcox" 3 random integer |s, v| rwilcox(s, v[0], v[1]);
    "rhyper" 4 random integer |s, v| rhyper(s, v[0], v[1], v[2]);
}

/// Which entry point a vectorized call came through. The stats routines and the `.Internal`
/// forms differ only in an error message's capitalization and in the type of the counts the
/// discrete generators return.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
    Internal,
    Stats,
}

/// A source of the uniform, normal and exponential deviates and the uniform indices that the
/// random generators are built from.
pub trait Deviates {
    fn uniform(&mut self) -> R<f64>;
    fn normal(&mut self) -> R<f64>;
    fn exponential(&mut self) -> R<f64>;
    /// A uniform integer in `0..n`, as a double.
    fn index(&mut self, n: f64) -> R<f64>;
}

/// The session's generator, as `unif_rand`, `norm_rand`, `exp_rand` and `R_unif_index` reach it.
/// The caller brackets the draws with `GetRNGstate` and `PutRNGstate`.
pub struct SessionDeviates;

impl Deviates for SessionDeviates {
    fn uniform(&mut self) -> R<f64> {
        // SAFETY: the interpreter is installed whenever R code or the C API draws numbers.
        unsafe { random::unif_rand() }
    }

    fn normal(&mut self) -> R<f64> {
        // SAFETY: as above.
        unsafe { random::norm_rand() }
    }

    fn exponential(&mut self) -> R<f64> {
        // SAFETY: as above.
        unsafe { random::exp_rand() }
    }

    fn index(&mut self, n: f64) -> R<f64> {
        // SAFETY: as above.
        unsafe { random::unif_index(n) }
    }
}

/// The warnings the functions raised since the last call, in order. The builtins raise them as
/// R warnings; the C API layer drains them the same way after each Rmath call.
pub fn take_warnings() -> Vec<String> {
    WARNINGS.with(|warnings| std::mem::take(&mut *warnings.borrow_mut()))
}

thread_local! {
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn warn(message: String) {
    WARNINGS.with(|warnings| warnings.borrow_mut().push(message));
}

unsafe fn raise_warnings() -> R<()> {
    for message in take_warnings() {
        warning(message)?;
    }
    Ok(())
}

// Vectorization.

/// GNU R's `math2` to `math4`: the first `vectors` arguments recycle, the rest are integer flags
/// (`log`, `lower.tail`, `log.p`, Bessel's `expo`), read as `asInteger` reads them.
unsafe fn math(
    args: &[Arg],
    interface: Interface,
    vectors: usize,
    function: fn(&[f64; 4], [i32; 2]) -> f64,
) -> R<Sexp> {
    let mut originals = [nil(); 4];
    let mut operands = [nil(); 4];
    let mut lengths = [0usize; 4];
    for index in 0..vectors {
        let original = materialize(argument(args, index))?;
        if !is_numeric(original) {
            return Err(error(match interface {
                Interface::Internal => "non-numeric argument to mathematical function",
                Interface::Stats => "Non-numeric argument to mathematical function",
            }));
        }
        originals[index] = original;
        operands[index] = coerce_vector(original, SexpType::Real)?;
        lengths[index] = length(original);
    }
    let mut flags = [0i32; 2];
    for (slot, index) in (vectors..args.len()).enumerate().take(2) {
        flags[slot] = flag(argument(args, index))?;
    }
    if lengths[..vectors].contains(&0) {
        return alloc(SexpType::Real, 0);
    }
    let count = lengths[..vectors].iter().copied().max().unwrap_or(0);
    let result = alloc(SexpType::Real, count)?;
    let mut nan_produced = false;
    let mut arguments = [0.0f64; 4];
    for (element, slot) in reals_mut(result).iter_mut().enumerate() {
        let mut any_na = false;
        let mut any_nan = false;
        for index in 0..vectors {
            let value = reals(operands[index])[element % lengths[index]];
            any_na |= is_na_real(value);
            any_nan |= value.is_nan();
            arguments[index] = value;
        }
        *slot = if any_na {
            NA_REAL
        } else if any_nan {
            f64::NAN
        } else {
            let value = function(&arguments, flags);
            nan_produced |= value.is_nan();
            value
        };
    }
    raise_warnings()?;
    if nan_produced {
        warning("NaNs produced")?;
    }
    if let Some(index) = (0..vectors).find(|&index| lengths[index] == count) {
        copy_attributes(originals[index], result)?;
    }
    Ok(result)
}

fn argument(args: &[Arg], index: usize) -> Sexp {
    args.get(index).map_or_else(nil, |arg| arg.value)
}

/// `isNumeric`: logical, integer or double, but not a factor.
unsafe fn is_numeric(value: Sexp) -> bool {
    matches!(
        value.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && !inherits(value, "factor")
}

/// A flag as `asInteger` reads it.
unsafe fn flag(value: Sexp) -> R<i32> {
    let value = materialize(value)?;
    if length(value) == 0
        || !matches!(
            value.kind(),
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::String
        )
    {
        return Ok(NA_INTEGER);
    }
    let integer = coerce_vector(value, SexpType::Integer)?;
    Ok(ints(integer)[0])
}

/// GNU R's `random1` to `random3`: `n` is a count, or the length of a longer vector, and the
/// parameters recycle over it. Missing parameters give `NA`s, and every NaN drawn raises "NAs
/// produced". The stats routines return the discrete generators' counts as integers when they
/// all fit.
unsafe fn random_vector(
    args: &[Arg],
    interface: Interface,
    integer: bool,
    function: fn(&mut SessionDeviates, &[f64; 3]) -> R<f64>,
) -> R<Sexp> {
    let count = random_count(argument(args, 0))?;
    let parameters = args.len().saturating_sub(1).min(3);
    let mut operands = [nil(); 3];
    let mut lengths = [0usize; 3];
    for index in 0..parameters {
        let original = materialize(argument(args, index + 1))?;
        if !is_numeric(original) {
            return Err(error("invalid arguments"));
        }
        operands[index] = coerce_vector(original, SexpType::Real)?;
        lengths[index] = length(original);
    }
    let integer = integer && interface == Interface::Stats;
    let mut drawn = vec![NA_REAL; count];
    if count > 0 && lengths[..parameters].contains(&0) {
        warning("NAs produced")?;
        return random_result(&drawn, integer);
    }
    if count > 0 {
        random::get_rng_state()?;
        let mut deviates = SessionDeviates;
        let mut arguments = [0.0f64; 3];
        let mut outcome = Ok(());
        let scalar = lengths[..parameters].iter().all(|&length| length == 1);
        for index in 0..parameters {
            arguments[index] = reals(operands[index])[0];
        }
        for (element, slot) in drawn.iter_mut().enumerate() {
            if !scalar {
                for index in 0..parameters {
                    arguments[index] = reals(operands[index])[element % lengths[index]];
                }
            }
            match function(&mut deviates, &arguments) {
                Ok(value) => *slot = value,
                Err(failure) => {
                    outcome = Err(failure);
                    break;
                }
            }
        }
        random::put_rng_state()?;
        outcome?;
        raise_warnings()?;
        if drawn.iter().any(|value| value.is_nan()) {
            warning("NAs produced")?;
        }
    }
    random_result(&drawn, integer)
}

/// The number of draws `n` asks for: the value of a scalar, or the length of a longer vector.
unsafe fn random_count(value: Sexp) -> R<usize> {
    let value = materialize(value)?;
    if !matches!(
        value.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
            | SexpType::Expression
    ) {
        return Err(error("invalid arguments"));
    }
    if length(value) != 1 {
        return Ok(length(value));
    }
    let count = match value.kind() {
        SexpType::List | SexpType::Expression => f64::NAN,
        _ => reals(coerce_vector(value, SexpType::Real)?)[0],
    };
    if count.is_nan() || !(0.0..=4.503599627370496e15).contains(&count) {
        return Err(error("invalid arguments"));
    }
    Ok(count as usize)
}

unsafe fn random_result(drawn: &[f64], integer: bool) -> R<Sexp> {
    let fits = |value: &f64| value.is_nan() || value.abs() <= f64::from(i32::MAX);
    if integer && drawn.iter().all(fits) {
        let result = alloc(SexpType::Integer, drawn.len())?;
        for (slot, value) in ints_mut(result).iter_mut().zip(drawn) {
            *slot = if value.is_nan() {
                NA_INTEGER
            } else {
                *value as i32
            };
        }
        return Ok(result);
    }
    let result = alloc(SexpType::Real, drawn.len())?;
    reals_mut(result).copy_from_slice(drawn);
    Ok(result)
}

/// `rmultinom(n, size, prob)`: `n` draws of the counts of `size` objects put into
/// `length(prob)` boxes, as the columns of an integer matrix.
unsafe fn rmultinom_routine(n: Sexp, size: Sexp, prob: Sexp) -> R<Sexp> {
    let draws = as_integer_scalar(n);
    if draws == NA_INTEGER || draws < 0 {
        return Err(error("invalid first argument 'n'"));
    }
    let size = as_integer_scalar(size);
    if size == NA_INTEGER || size < 0 {
        return Err(error("invalid second argument 'size'"));
    }
    let probabilities_vector = coerce_vector(materialize(prob)?, SexpType::Real)?;
    let mut probabilities = reals(probabilities_vector).to_vec();
    let boxes = probabilities.len();
    let mut total = 0.0;
    let mut positive = 0;
    for &probability in &probabilities {
        if !probability.is_finite() {
            return Err(error("NA in probability vector"));
        }
        if probability < 0.0 {
            return Err(error("negative probability"));
        }
        if probability > 0.0 {
            positive += 1;
            total += probability;
        }
    }
    if positive == 0 {
        return Err(error("no positive probabilities"));
    }
    for probability in &mut probabilities {
        *probability /= total;
    }
    let result = alloc(SexpType::Integer, boxes * draws as usize)?;
    random::get_rng_state()?;
    let mut deviates = SessionDeviates;
    let mut outcome = Ok(());
    for column in ints_mut(result)
        .chunks_mut(boxes.max(1))
        .take(draws as usize)
    {
        if let Err(failure) = rmultinom(&mut deviates, size, &probabilities, column) {
            outcome = Err(failure);
            break;
        }
    }
    random::put_rng_state()?;
    outcome?;
    let dims = integer_vector(&[boxes as i32, draws])?;
    set_attr(result, symbols().dim, dims)?;
    let names = get_attr(prob, symbols().names);
    if names != nil() {
        let dimnames = list_vector(&[names, nil()])?;
        set_attr(result, symbols().dimnames, dimnames)?;
    }
    Ok(result)
}

// Rmath's conventions for the scale (`log_p`) and the tail (`lower_tail`) of a probability.

/// Probability zero on the requested scale.
fn d_zero(log_p: bool) -> f64 {
    if log_p { f64::NEG_INFINITY } else { 0.0 }
}

fn d_one(log_p: bool) -> f64 {
    if log_p { 0.0 } else { 1.0 }
}

/// The probability of the requested tail when the lower tail has probability zero.
fn dt_zero(lower_tail: bool, log_p: bool) -> f64 {
    if lower_tail {
        d_zero(log_p)
    } else {
        d_one(log_p)
    }
}

fn dt_one(lower_tail: bool, log_p: bool) -> f64 {
    if lower_tail {
        d_one(log_p)
    } else {
        d_zero(log_p)
    }
}

/// `p` on the requested scale.
fn d_val(p: f64, log_p: bool) -> f64 {
    if log_p { p.ln() } else { p }
}

/// `exp(x)` on the requested scale: `x` itself when on the log scale.
fn d_exp(x: f64, log_p: bool) -> f64 {
    if log_p { x } else { x.exp() }
}

/// `1 - p` on the requested scale.
fn d_clog(p: f64, log_p: bool) -> f64 {
    if log_p { (-p).ln_1p() } else { 0.5 - p + 0.5 }
}

/// The requested tail's probability, given the lower tail's `p` on the natural scale.
fn dt_val(p: f64, lower_tail: bool, log_p: bool) -> f64 {
    if lower_tail {
        d_val(p, log_p)
    } else {
        d_clog(p, log_p)
    }
}

/// The lower tail's probability on the natural scale, from `p` as given.
fn dt_qiv(p: f64, lower_tail: bool, log_p: bool) -> f64 {
    match (log_p, lower_tail) {
        (true, true) => p.exp(),
        (true, false) => -p.exp_m1(),
        (false, true) => p,
        (false, false) => 0.5 - p + 0.5,
    }
}

/// The upper tail's probability on the natural scale, from `p` as given.
fn dt_civ(p: f64, lower_tail: bool, log_p: bool) -> f64 {
    dt_qiv(p, !lower_tail, log_p)
}

/// `log(p)` from `p` as given on the requested scale.
fn d_log(p: f64, log_p: bool) -> f64 {
    if log_p { p } else { p.ln() }
}

/// `log(1 - p)` from `p` as given on the requested scale.
fn d_lexp(p: f64, log_p: bool) -> f64 {
    if log_p { log1mexp(p) } else { (-p).ln_1p() }
}

/// The log of the upper tail's probability, from `p` as given.
fn dt_clog(p: f64, lower_tail: bool, log_p: bool) -> f64 {
    if lower_tail {
        d_lexp(p, log_p)
    } else {
        d_log(p, log_p)
    }
}

/// The log of the lower tail's probability, from `p` as given.
fn dt_log(p: f64, lower_tail: bool, log_p: bool) -> f64 {
    if lower_tail {
        d_log(p, log_p)
    } else {
        d_lexp(p, log_p)
    }
}

/// `log(1 - exp(x))` for `x <= 0`, accurate on both sides of `-log(2)` (Mächler 2012).
pub fn log1mexp(x: f64) -> f64 {
    if x > -LN_2 {
        (-x.exp_m1()).ln()
    } else {
        (-x.exp()).ln_1p()
    }
}

/// `log(1 + exp(x))` without overflow.
pub fn log1pexp(x: f64) -> f64 {
    if x <= 18.0 {
        x.exp().ln_1p()
    } else if x > 33.3 {
        x
    } else {
        x + (-x).exp()
    }
}

/// Whether `p` lies outside the probabilities of its scale.
fn p_invalid(p: f64, log_p: bool) -> bool {
    if log_p {
        p > 0.0
    } else {
        !(0.0..=1.0).contains(&p)
    }
}

/// A quantile function's value at the edges of the probability range: `left` for the lower
/// tail's zero and `right` for its one, NaN outside, `None` inside.
fn quantile_boundaries(
    p: f64,
    left: f64,
    right: f64,
    lower_tail: bool,
    log_p: bool,
) -> Option<f64> {
    if p_invalid(p, log_p) {
        return Some(f64::NAN);
    }
    let (zero, one) = if log_p {
        (f64::NEG_INFINITY, 0.0)
    } else {
        (0.0, 1.0)
    };
    if p == zero {
        return Some(if lower_tail { left } else { right });
    }
    if p == one {
        return Some(if lower_tail { right } else { left });
    }
    None
}

/// The rounding Rmath calls `R_forceint`.
fn force_int(x: f64) -> f64 {
    x.round_ties_even()
}

fn is_non_integer(x: f64) -> bool {
    (x - force_int(x)).abs() > 1e-7 * x.abs().max(1.0)
}

/// Whether `x` is negative or not an integer, which makes it an invalid count.
fn negative_or_non_integer(x: f64) -> bool {
    x < 0.0 || is_non_integer(x)
}

/// Warns about a non-integer `x`, which has density zero.
fn warn_non_integer(x: f64) -> bool {
    if is_non_integer(x) {
        warn(format!("non-integer x = {x:.6}"));
        true
    } else {
        false
    }
}

// Constants.

const LN_SQRT_2PI: f64 = 0.918_938_533_204_672_741_780_329_736_406;
const ONE_OVER_SQRT_2PI: f64 = 0.398_942_280_401_432_677_939_946_059_934;
const SQRT_32: f64 = 5.656_854_249_492_380_195_206_754_896_838;
const LN_2PI: f64 = 1.837_877_066_409_345_483_560_659_472_811;
/// The log of the smallest positive double, below which `exp` underflows to zero.
const MIN_LOG: f64 = -745.2;
const EULER_GAMMA: f64 = 0.577_215_664_901_532_860_606_512_090_082;

// Special functions shared by the distributions.

/// `log|Γ(x)|`, evaluated as GNU R evaluates it, since results such as the studentized range's
/// normalizing constant inherit its last bit: `log|Γ(x)|` up to 10, and above it Stirling's
/// formula `(x - 1/2) log x - x + log √(2π)` plus the correction series, summed in that order.
pub fn lgammafn(x: f64) -> f64 {
    if x > 10.0 && x.is_finite() {
        let correction = if x < 1e17 {
            let inverse_square = 1.0 / (x * x);
            (1.0 / 12.0
                - inverse_square
                    * (1.0 / 360.0
                        - inverse_square
                            * (1.0 / 1260.0
                                - inverse_square
                                    * (1.0 / 1680.0 - inverse_square * (1.0 / 1188.0)))))
                / x
        } else {
            1.0 / (12.0 * x)
        };
        return LN_SQRT_2PI + (x - 0.5) * x.ln() - x + correction;
    }
    if (x.abs() <= 10.0 && x != x.floor()) || (x > 0.0 && x <= 10.0) {
        return gammafn(x).abs().ln();
    }
    // SAFETY: `lgamma` is the C library's pure function.
    unsafe { lgamma(x) }
}

/// `Γ(x)`.
pub fn gammafn(x: f64) -> f64 {
    // SAFETY: `tgamma` is the C library's pure function.
    unsafe { tgamma(x) }
}

/// `log Γ(1 + a)`, accurate for small `a` where `lgamma(1 + a)` loses digits to the rounding of
/// `1 + a`. The power series of `log Γ(1 + a)` has coefficients `(-1)^k ζ(k) / k`; splitting off
/// `log(1 + a)` (whose coefficients are `(-1)^(k+1) / k`) leaves `(ζ(k) - 1) / k`, which decay
/// like `2^-k`, so a few dozen terms reach full precision for `|a| < 0.5`.
pub fn lgamma1p(a: f64) -> f64 {
    if a.abs() >= 0.5 {
        return lgammafn(a + 1.0);
    }
    // (ζ(k) - 1) / k for k = 2, 3, ...
    #[allow(clippy::excessive_precision)]
    const COEFFICIENTS: [f64; 40] = [
        0.3224670334241132182362075833230126e-0,
        0.6735230105319809513324605383715000e-1,
        0.2058080842778454787900092413529198e-1,
        0.7385551028673985266273097291406834e-2,
        0.2890510330741523285752988298486755e-2,
        0.1192753911703260977113935692828109e-2,
        0.5096695247430424223356548135815582e-3,
        0.2231547584535793797614188036013401e-3,
        0.9945751278180853371459589003190170e-4,
        0.4492623673813314170020750240635786e-4,
        0.2050721277567069155316650397830591e-4,
        0.9439488275268395903987425104415055e-5,
        0.4374866789907487804181793223952411e-5,
        0.2039215753801366236781900709670839e-5,
        0.9551412130407419832857179772951265e-6,
        0.4492469198764566043294290331193655e-6,
        0.2120718480555466586923135901077628e-6,
        0.1004322482396809960872083050053344e-6,
        0.4769810169363980565760193417246730e-7,
        0.2271109460894316491031998116062124e-7,
        0.1083865921489695409107491757968159e-7,
        0.5183475041970046655121248647057669e-8,
        0.2483674543802478317185008663991718e-8,
        0.1192140140586091207442548202774640e-8,
        0.5731367241678862013330194857961011e-9,
        0.2758522714757910209632162708709524e-9,
        0.1329045294158347226005536113584990e-9,
        0.6410184944456802061713488262046030e-10,
        0.3094706217464298580580690658117320e-10,
        0.1495440282591264497006436738380117e-10,
        0.7232412544733516286002148262012840e-11,
        0.3500423017883426591440130223440760e-11,
        0.1695272285651209209225637010416046e-11,
        0.8215454269484131087617283216823880e-12,
        0.3983601022993082962591051946520480e-12,
        0.1932646406555019648946094093212548e-12,
        0.9381111339103463227045853018618100e-13,
        0.4555710567802456591458393023020310e-13,
        0.2213342373045163218733508023862760e-13,
        0.1075801047107924097564633911567400e-13,
    ];
    // log Γ(1 + a) = -γa + Σ_{k≥2} (-a)^k ζ(k)/k, and Σ_{k≥2} (-a)^k / k = a - log(1 + a), so
    // log Γ(1 + a) = -γa - (log(1 + a) - a) + Σ_{k≥2} (-a)^k (ζ(k) - 1)/k.
    let mut sum = 0.0;
    let mut power = a * a;
    for coefficient in COEFFICIENTS {
        let term = coefficient * power;
        sum += term;
        if term.abs() < f64::EPSILON * 1e-3 * sum.abs().max(f64::MIN_POSITIVE) {
            break;
        }
        power *= -a;
    }
    -EULER_GAMMA * a - log1pmx(a) + sum
}

/// `log(1 + x) - x`, accurate for small `x`.
pub fn log1pmx(x: f64) -> f64 {
    if !(-0.79149064..=1.0).contains(&x) {
        return x.ln_1p() - x;
    }
    // log(1 + x) - x = -x²/2 + x³/3 - ... ; with r = x / (2 + x) the series
    // 2 r (1 + r²/3 + r⁴/5 + ...) - x converges much faster.
    let r = x / (2.0 + x);
    let y = r * r;
    if x.abs() < 1e-2 {
        return r * ((((2.0 / 9.0 * y + 2.0 / 7.0) * y + 2.0 / 5.0) * y + 2.0 / 3.0) * y - x);
    }
    // 2 r Σ_{k≥0} y^k / (2k + 1) - x = r (2 y Σ_{k≥0} y^k / (2k + 3) - x) since 2r - x = -r x.
    let mut sum = 0.0;
    let mut power = 1.0;
    let mut k = 0.0;
    loop {
        let term = power / (2.0 * k + 3.0);
        sum += term;
        if term < 1e-17 * sum {
            break;
        }
        power *= y;
        k += 1.0;
    }
    r * (2.0 * y * sum - x)
}

/// The error of Stirling's formula: `log(n!) - log(sqrt(2πn) (n/e)^n)`.
pub fn stirlerr(n: f64) -> f64 {
    // stirlerr(k / 2) for k = 0, ..., 30, computed in high precision.
    #[allow(clippy::excessive_precision)]
    const HALVES: [f64; 31] = [
        0.0,
        0.15342640972002734529,
        0.08106146679532725822,
        0.054814121051917653896,
        0.041340695955409294094,
        0.033162873519936287485,
        0.027677925684998339149,
        0.023746163656297495971,
        0.020790672103765093112,
        0.018488450532673185231,
        0.016644691189821192163,
        0.015134973221917378874,
        0.013876128823070747999,
        0.012810465242920226924,
        0.011896709945891770095,
        0.011104559758206917327,
        0.010411265261972096497,
        0.0097994161261588032984,
        0.0092554621827127329177,
        0.008768700134139385463,
        0.0083305634333628712565,
        0.0079341145643140205472,
        0.007573675487951840795,
        0.0072445543013203831795,
        0.0069428401072095298657,
        0.0066652470327076824424,
        0.0064089941880042070684,
        0.0061717122630394576475,
        0.0059513701127588477356,
        0.005746216513010115682,
        0.005554733551962801371,
    ];
    // The asymptotic series 1/(12n) - 1/(360n³) + 1/(1260n⁵) - 1/(1680n⁷) + 1/(1188n⁹).
    const S0: f64 = 1.0 / 12.0;
    const S1: f64 = 1.0 / 360.0;
    const S2: f64 = 1.0 / 1260.0;
    const S3: f64 = 1.0 / 1680.0;
    const S4: f64 = 1.0 / 1188.0;
    if n <= 15.0 {
        let doubled = n + n;
        if doubled == doubled.trunc() {
            return HALVES[doubled as usize];
        }
        return lgammafn(n + 1.0) - (n + 0.5) * n.ln() + n - LN_SQRT_2PI;
    }
    let square = n * n;
    if n > 500.0 {
        return (S0 - S1 / square) / n;
    }
    if n > 80.0 {
        return (S0 - (S1 - S2 / square) / square) / n;
    }
    if n > 35.0 {
        return (S0 - (S1 - (S2 - S3 / square) / square) / square) / n;
    }
    (S0 - (S1 - (S2 - (S3 - S4 / square) / square) / square) / square) / n
}

/// The deviance term `x log(x / np) + np - x` of Loader's saddle point expansion, computed by
/// its series when `x` is close to `np`.
pub fn bd0(x: f64, np: f64) -> f64 {
    if !x.is_finite() || !np.is_finite() || np == 0.0 {
        return f64::NAN;
    }
    if (x - np).abs() < 0.1 * (x + np) {
        let mut v = (x - np) / (x + np);
        let mut sum = (x - np) * v;
        if sum.abs() < f64::MIN_POSITIVE {
            return sum;
        }
        let mut term = 2.0 * x * v;
        v *= v;
        let mut j = 1.0;
        loop {
            term *= v;
            let next = sum + term / (j + j + 1.0);
            if next == sum {
                return next;
            }
            sum = next;
            j += 1.0;
            if j > 1000.0 {
                return sum;
            }
        }
    }
    x * (x / np).ln() + np - x
}

/// The binomial density for real `x` and `n`, given `p` and `q = 1 - p` separately so a caller
/// can keep the precision of whichever is small (Loader 2000).
pub fn dbinom_raw(x: f64, n: f64, p: f64, q: f64, log_p: bool) -> f64 {
    if p == 0.0 {
        return if x == 0.0 {
            d_one(log_p)
        } else {
            d_zero(log_p)
        };
    }
    if q == 0.0 {
        return if x == n { d_one(log_p) } else { d_zero(log_p) };
    }
    if x == 0.0 {
        if n == 0.0 {
            return d_one(log_p);
        }
        let log_density = if p < 0.1 {
            -bd0(n, n * q) - n * p
        } else {
            n * q.ln()
        };
        return d_exp(log_density, log_p);
    }
    if x == n {
        let log_density = if q < 0.1 {
            -bd0(n, n * p) - n * q
        } else {
            n * p.ln()
        };
        return d_exp(log_density, log_p);
    }
    if x < 0.0 || x > n {
        return d_zero(log_p);
    }
    let log_core = stirlerr(n) - stirlerr(x) - stirlerr(n - x) - bd0(x, n * p) - bd0(n - x, n * q);
    let log_factor = LN_2PI + x.ln() + (-x / n).ln_1p();
    d_exp(log_core - 0.5 * log_factor, log_p)
}

/// The Poisson density for real `x` (Loader 2000).
pub fn dpois_raw(x: f64, lambda: f64, log_p: bool) -> f64 {
    if lambda == 0.0 {
        return if x == 0.0 {
            d_one(log_p)
        } else {
            d_zero(log_p)
        };
    }
    if !lambda.is_finite() || x < 0.0 {
        return d_zero(log_p);
    }
    if x <= lambda * f64::MIN_POSITIVE {
        return d_exp(-lambda, log_p);
    }
    if lambda < x * f64::MIN_POSITIVE {
        if !x.is_finite() {
            return d_zero(log_p);
        }
        return d_exp(-lambda + x * lambda.ln() - lgammafn(x + 1.0), log_p);
    }
    let exponent = -stirlerr(x) - bd0(x, lambda);
    let factor = TAU * x;
    if log_p {
        -0.5 * factor.ln() + exponent
    } else {
        exponent.exp() / factor.sqrt()
    }
}

/// `log B(a, b)`, with Stirling's corrections for large arguments so that the large terms
/// cancel analytically.
pub fn lbeta(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    let p = a.min(b);
    let q = a.max(b);
    if p < 0.0 {
        return f64::NAN;
    }
    if p == 0.0 {
        return f64::INFINITY;
    }
    if !q.is_finite() {
        return f64::NEG_INFINITY;
    }
    if p >= 10.0 {
        let correction = stirlerr(p) + stirlerr(q) - stirlerr(p + q);
        return q.ln() * -0.5
            + LN_SQRT_2PI
            + correction
            + (p - 0.5) * (p / (p + q)).ln()
            + q * (-p / (p + q)).ln_1p();
    }
    if q >= 10.0 {
        let correction = stirlerr(q) - stirlerr(p + q);
        return lgammafn(p) + correction + p - p * (p + q).ln()
            + (q - 0.5) * (-p / (p + q)).ln_1p();
    }
    if p + q < 171.61447887182298 {
        return (gammafn(p) * (gammafn(q) / gammafn(p + q))).ln();
    }
    lgammafn(p) + lgammafn(q) - lgammafn(p + q)
}

/// `B(a, b)`.
pub fn beta(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    if a < 0.0 || b < 0.0 {
        return f64::NAN;
    }
    if a == 0.0 || b == 0.0 {
        return f64::INFINITY;
    }
    if !a.is_finite() || !b.is_finite() {
        return 0.0;
    }
    if a + b < 171.61447887182298 {
        return (1.0 / gammafn(a + b)) * (gammafn(a) * gammafn(b));
    }
    lbeta(a, b).exp()
}

/// `tan(πx)`, exact where it is 0, ±1 or infinite.
fn tanpi(x: f64) -> f64 {
    if !x.is_finite() {
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
        (PI * x).tan()
    }
}

fn cospi(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let x = (x.abs()) % 2.0;
    if x % 1.0 == 0.5 {
        0.0
    } else if x == 1.0 {
        -1.0
    } else if x == 0.0 {
        1.0
    } else {
        (PI * x).cos()
    }
}

fn sinpi(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let mut x = x % 2.0;
    if x <= -1.0 {
        x += 2.0;
    } else if x > 1.0 {
        x -= 2.0;
    }
    if x == 0.0 || x == 1.0 {
        0.0
    } else if x == 0.5 {
        1.0
    } else if x == -0.5 {
        -1.0
    } else {
        (PI * x).sin()
    }
}

/// `c[0] r^(n-1) + ... + c[n-1]` by Horner's rule.
fn horner(r: f64, coefficients: &[f64]) -> f64 {
    let mut value = coefficients[0];
    for &coefficient in &coefficients[1..] {
        value = value * r + coefficient;
    }
    value
}

// Root finding for the continuous quantile functions.

/// The root of `function`, which increases in `t` and returns its value and slope, by Newton's
/// method kept inside the bracket `[low, high]` (which may be unbounded) by bisection. The
/// bracket shrinks to every point evaluated, so the iteration converges even where the function
/// is not convex.
fn increasing_root(
    mut t: f64,
    mut low: f64,
    mut high: f64,
    function: impl Fn(f64) -> (f64, f64),
) -> f64 {
    for _ in 0..400 {
        let (value, slope) = function(t);
        if value == 0.0 {
            return t;
        }
        if value.is_nan() {
            break;
        }
        if value < 0.0 {
            low = t;
        } else {
            high = t;
        }
        let newton = t - value / slope;
        let next = if newton.is_finite() && newton > low && newton < high {
            newton
        } else if low.is_finite() && high.is_finite() {
            low + (high - low) / 2.0
        } else if low.is_finite() {
            t + (t - low).abs().max(1.0) * 2.0
        } else {
            t - (high - t).abs().max(1.0) * 2.0
        };
        if (next - t).abs() <= 2.0 * f64::EPSILON * t.abs() || next == t {
            return next;
        }
        if low.is_finite()
            && high.is_finite()
            && high - low <= 2.0 * f64::EPSILON * low.abs().max(high.abs())
        {
            return next;
        }
        t = next;
    }
    t
}

// The normal distribution.

pub fn dnorm(x: f64, mu: f64, sigma: f64, log_p: bool) -> f64 {
    if x.is_nan() || mu.is_nan() || sigma.is_nan() {
        return x + mu + sigma;
    }
    if sigma < 0.0 {
        return f64::NAN;
    }
    if !sigma.is_finite() {
        return d_zero(log_p);
    }
    if !x.is_finite() && mu == x {
        return f64::NAN;
    }
    if sigma == 0.0 {
        return if x == mu {
            f64::INFINITY
        } else {
            d_zero(log_p)
        };
    }
    let z = (x - mu) / sigma;
    if !z.is_finite() {
        return d_zero(log_p);
    }
    let z = z.abs();
    if z >= 2.0 * f64::MAX.sqrt() {
        return d_zero(log_p);
    }
    if log_p {
        return -(LN_SQRT_2PI + 0.5 * z * z + sigma.ln());
    }
    if z < 5.0 {
        return ONE_OVER_SQRT_2PI * (-0.5 * z * z).exp() / sigma;
    }
    // Beyond this the density underflows, even as a denormal.
    if z > (-2.0 * LN_2 * f64::from(f64::MIN_EXP + 1 - f64::MANTISSA_DIGITS as i32)).sqrt() {
        return 0.0;
    }
    // exp(-z²/2) loses relative precision as z grows because z² rounds; splitting z into a part
    // with 16 fractional bits, whose square is exact, and a small rest keeps it.
    let high = ((z * 65536.0).round_ties_even()) / 65536.0;
    let rest = z - high;
    ONE_OVER_SQRT_2PI / sigma * ((-0.5 * high * high).exp() * ((-0.5 * rest - high) * rest).exp())
}

pub fn pnorm(x: f64, mu: f64, sigma: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || mu.is_nan() || sigma.is_nan() {
        return x + mu + sigma;
    }
    if !x.is_finite() && mu == x {
        return f64::NAN;
    }
    if sigma <= 0.0 {
        if sigma < 0.0 {
            return f64::NAN;
        }
        return if x < mu {
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    let z = (x - mu) / sigma;
    if !z.is_finite() {
        return if x < mu {
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    let (lower, upper) = pnorm_both(z, if lower_tail { 0 } else { 1 }, log_p);
    if lower_tail { lower } else { upper }
}

/// Both tails of the standard normal distribution by Cody's (1969) rational Chebyshev
/// approximations. `tails` is 0 when only the lower tail is needed, 1 for the upper and 2 for
/// both; it decides where the far tails may be cut to 0 and 1.
// The coefficients are quoted with every published digit.
#[allow(clippy::excessive_precision)]
pub fn pnorm_both(x: f64, tails: i32, log_p: bool) -> (f64, f64) {
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
    let lower_needed = tails != 1;
    let upper_needed = tails != 0;
    let y = x.abs();
    let mut cumulative;
    let mut complement;
    // Multiplies `temp` by exp(-y²/2) with y split so that the square is exact, and returns the
    // smaller tail and its complement.
    let tail_pair = |temp: f64| -> (f64, f64) {
        let rounded = (y * 16.0).trunc() / 16.0;
        let delta = (y - rounded) * (y + rounded);
        if log_p {
            let small = (-rounded * rounded * 0.5) + (-delta * 0.5) + temp.ln();
            let large = (-(-rounded * rounded * 0.5).exp() * (-delta * 0.5).exp() * temp).ln_1p();
            (small, large)
        } else {
            let small = (-rounded * rounded * 0.5).exp() * (-delta * 0.5).exp() * temp;
            (small, 1.0 - small)
        }
    };
    if y <= 0.67448975 {
        let (mut numerator, mut denominator, square);
        if y > f64::EPSILON * 0.5 {
            square = x * x;
            numerator = A[4] * square;
            denominator = square;
            for index in 0..3 {
                numerator = (numerator + A[index]) * square;
                denominator = (denominator + B[index]) * square;
            }
        } else {
            numerator = 0.0;
            denominator = 0.0;
        }
        let temp = x * (numerator + A[3]) / (denominator + B[3]);
        cumulative = 0.5 + temp;
        complement = 0.5 - temp;
        if log_p {
            cumulative = cumulative.ln();
            complement = complement.ln();
        }
        return (cumulative, complement);
    }
    if y <= SQRT_32 {
        let mut numerator = C[8] * y;
        let mut denominator = y;
        for index in 0..7 {
            numerator = (numerator + C[index]) * y;
            denominator = (denominator + D[index]) * y;
        }
        let temp = (numerator + C[7]) / (denominator + D[7]);
        let (small, large) = tail_pair(temp);
        return if x > 0.0 {
            (large, small)
        } else {
            (small, large)
        };
    }
    if (log_p && y < 1e170)
        || (lower_needed && -37.5193 < x && x < 8.2924)
        || (upper_needed && -8.2924 < x && x < 37.5193)
    {
        let square = 1.0 / (x * x);
        let mut numerator = P[5] * square;
        let mut denominator = square;
        for index in 0..4 {
            numerator = (numerator + P[index]) * square;
            denominator = (denominator + Q[index]) * square;
        }
        let mut temp = square * (numerator + P[4]) / (denominator + Q[4]);
        temp = (ONE_OVER_SQRT_2PI - temp) / y;
        let (small, large) = tail_pair(temp);
        return if x > 0.0 {
            (large, small)
        } else {
            (small, large)
        };
    }
    if x > 0.0 {
        (d_one(log_p), d_zero(log_p))
    } else {
        (d_zero(log_p), d_one(log_p))
    }
}

/// The normal quantile by Wichura's (1988) AS 241, with a Newton refinement on the log scale
/// for the extreme tails, where the rational approximation is no longer accurate.
// The coefficients are quoted with every published digit.
#[allow(clippy::excessive_precision)]
pub fn qnorm(p: f64, mu: f64, sigma: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || mu.is_nan() || sigma.is_nan() {
        return p + mu + sigma;
    }
    if let Some(edge) = quantile_boundaries(p, f64::NEG_INFINITY, f64::INFINITY, lower_tail, log_p)
    {
        return edge;
    }
    if sigma < 0.0 {
        return f64::NAN;
    }
    if sigma == 0.0 {
        return mu;
    }
    let lower = dt_qiv(p, lower_tail, log_p);
    let q = lower - 0.5;
    if q.abs() <= 0.425 {
        let r = 0.180625 - q * q;
        let value = q * horner(
            r,
            &[
                2509.0809287301226727,
                33430.575583588128105,
                67265.770927008700853,
                45921.953931549871457,
                13731.693765509461125,
                1971.5909503065514427,
                133.14166789178437745,
                3.387132872796366608,
            ],
        ) / horner(
            r,
            &[
                5226.495278852854561,
                28729.085735721942674,
                39307.89580009271061,
                21213.794301586595867,
                5394.1960214247511077,
                687.1870074920579083,
                42.313330701600911252,
                1.0,
            ],
        );
        return mu + sigma * value;
    }
    // The log of the smaller tail's probability.
    let log_tail = if log_p && ((lower_tail && q <= 0.0) || (!lower_tail && q > 0.0)) {
        p
    } else {
        (if q > 0.0 {
            dt_civ(p, lower_tail, log_p)
        } else {
            lower
        })
        .ln()
    };
    let mut r = (-log_tail).sqrt();
    let mut value;
    if r <= 5.0 {
        r -= 1.6;
        value = horner(
            r,
            &[
                7.7454501427834140764e-4,
                0.0227238449892691845833,
                0.24178072517745061177,
                1.27045825245236838258,
                3.64784832476320460504,
                5.7694972214606914055,
                4.6303378461565452959,
                1.42343711074968357734,
            ],
        ) / horner(
            r,
            &[
                1.05075007164441684324e-9,
                5.475938084995344946e-4,
                0.0151986665636164571966,
                0.14810397642748007459,
                0.68976733498510000455,
                1.6763848301838038494,
                2.05319162663775882187,
                1.0,
            ],
        );
    } else if r >= 816.0 {
        // log Φ(-x) ~ -x²/2 for x beyond about 1154, where x√2 is the root to every digit.
        value = r * SQRT_2;
    } else {
        r -= 5.0;
        value = horner(
            r,
            &[
                2.01033439929228813265e-7,
                2.71155556874348757815e-5,
                0.0012426609473880784386,
                0.026532189526576123093,
                0.29656057182850489123,
                1.7848265399172913358,
                5.4637849111641143699,
                6.6579046435011037772,
            ],
        ) / horner(
            r,
            &[
                2.04426310338993978564e-15,
                1.4215117583164458887e-7,
                1.8463183175100546818e-5,
                7.868691311456132591e-4,
                0.0148753612908506148525,
                0.13692988092273580531,
                0.59983220655588793769,
                1.0,
            ],
        );
        if log_p && (-log_tail).sqrt() > 27.0 {
            // Beyond p = exp(-729) AS 241 is accurate to only about five digits; Newton's method
            // on log Φ(-x) = log_tail recovers the rest (the slope is -φ(x)/Φ(-x)).
            for _ in 0..4 {
                let log_value = pnorm(-value, 0.0, 1.0, true, true);
                let log_density = dnorm(value, 0.0, 1.0, true);
                let step = (log_value - log_tail) / (log_density - log_value).exp();
                value += step;
                if step.abs() <= 1e-15 * value {
                    break;
                }
            }
        }
    }
    if q < 0.0 {
        value = -value;
    }
    mu + sigma * value
}

// The uniform, exponential, Cauchy, logistic, log-normal and Weibull distributions, which have
// closed forms.

pub fn dunif(x: f64, a: f64, b: f64, log_p: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if b <= a {
        return f64::NAN;
    }
    if a <= x && x <= b {
        if log_p { -(b - a).ln() } else { 1.0 / (b - a) }
    } else {
        d_zero(log_p)
    }
}

pub fn punif(x: f64, a: f64, b: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if b < a || !a.is_finite() || !b.is_finite() {
        return f64::NAN;
    }
    if x >= b {
        return dt_one(lower_tail, log_p);
    }
    if x <= a {
        return dt_zero(lower_tail, log_p);
    }
    if lower_tail {
        d_val((x - a) / (b - a), log_p)
    } else {
        d_val((b - x) / (b - a), log_p)
    }
}

pub fn qunif(p: f64, a: f64, b: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || a.is_nan() || b.is_nan() {
        return p + a + b;
    }
    if p_invalid(p, log_p) || !a.is_finite() || !b.is_finite() || b < a {
        return f64::NAN;
    }
    if b == a {
        return a;
    }
    a + dt_qiv(p, lower_tail, log_p) * (b - a)
}

pub fn dexp(x: f64, scale: f64, log_p: bool) -> f64 {
    if x.is_nan() || scale.is_nan() {
        return x + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d_zero(log_p);
    }
    if log_p {
        (-x / scale) - scale.ln()
    } else {
        (-x / scale).exp() / scale
    }
}

pub fn pexp(x: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || scale.is_nan() {
        return x + scale;
    }
    if scale < 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    let exponent = -(x / scale);
    if lower_tail {
        if log_p {
            log1mexp(exponent)
        } else {
            -exponent.exp_m1()
        }
    } else {
        d_exp(exponent, log_p)
    }
}

pub fn qexp(p: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || scale.is_nan() {
        return p + scale;
    }
    if scale < 0.0 || p_invalid(p, log_p) {
        return f64::NAN;
    }
    if p == dt_zero(lower_tail, log_p) {
        return 0.0;
    }
    -scale * dt_clog(p, lower_tail, log_p)
}

pub fn dcauchy(x: f64, location: f64, scale: f64, log_p: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let y = (x - location) / scale;
    let value = PI * scale * (1.0 + y * y);
    if log_p { -value.ln() } else { 1.0 / value }
}

pub fn pcauchy(x: f64, location: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let mut z = (x - location) / scale;
    if z.is_nan() {
        return f64::NAN;
    }
    if !z.is_finite() {
        return if z < 0.0 {
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    if !lower_tail {
        z = -z;
    }
    // For |z| > 1, atan(1/z) keeps the precision of the small tail that 0.5 + atan(z)/π loses.
    if z.abs() > 1.0 {
        let y = (1.0 / z).atan() / PI;
        if z > 0.0 {
            d_clog(y, log_p)
        } else {
            d_val(-y, log_p)
        }
    } else {
        d_val(0.5 + z.atan() / PI, log_p)
    }
}

pub fn qcauchy(p: f64, location: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || location.is_nan() || scale.is_nan() {
        return p + location + scale;
    }
    if p_invalid(p, log_p) {
        return f64::NAN;
    }
    if scale <= 0.0 || !scale.is_finite() {
        return if scale == 0.0 { location } else { f64::NAN };
    }
    let mut lower = lower_tail;
    let mut p = p;
    let signed_infinity = |lower: bool| if lower { scale } else { -scale } * f64::INFINITY;
    if log_p {
        if p > -1.0 {
            if p == 0.0 {
                return location + signed_infinity(lower);
            }
            lower = !lower;
            p = -p.exp_m1();
        } else {
            p = p.exp();
        }
    } else if p > 0.5 {
        if p == 1.0 {
            return location + signed_infinity(lower);
        }
        p = 1.0 - p;
        lower = !lower;
    }
    if p == 0.5 {
        return location;
    }
    if p == 0.0 {
        return location - signed_infinity(lower);
    }
    location + if lower { -scale } else { scale } / tanpi(p)
}

pub fn dlogis(x: f64, location: f64, scale: f64, log_p: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let z = ((x - location) / scale).abs();
    let e = (-z).exp();
    let f = 1.0 + e;
    if log_p {
        -(z + (scale * f * f).ln())
    } else {
        e / (scale * f * f)
    }
}

pub fn plogis(x: f64, location: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || location.is_nan() || scale.is_nan() {
        return x + location + scale;
    }
    if scale <= 0.0 {
        return f64::NAN;
    }
    let z = (x - location) / scale;
    if z.is_nan() {
        return f64::NAN;
    }
    if !z.is_finite() {
        return if z > 0.0 {
            dt_one(lower_tail, log_p)
        } else {
            dt_zero(lower_tail, log_p)
        };
    }
    let exponent = if lower_tail { -z } else { z };
    if log_p {
        -log1pexp(exponent)
    } else {
        1.0 / (1.0 + exponent.exp())
    }
}

pub fn qlogis(p: f64, location: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || location.is_nan() || scale.is_nan() {
        return p + location + scale;
    }
    if let Some(edge) = quantile_boundaries(p, f64::NEG_INFINITY, f64::INFINITY, lower_tail, log_p)
    {
        return edge;
    }
    if scale < 0.0 {
        return f64::NAN;
    }
    if scale == 0.0 {
        return location;
    }
    let logit = if log_p {
        if lower_tail {
            p - log1mexp(p)
        } else {
            log1mexp(p) - p
        }
    } else if lower_tail {
        (p / (1.0 - p)).ln()
    } else {
        ((1.0 - p) / p).ln()
    };
    location + scale * logit
}

pub fn dlnorm(x: f64, meanlog: f64, sdlog: f64, log_p: bool) -> f64 {
    if x.is_nan() || meanlog.is_nan() || sdlog.is_nan() {
        return x + meanlog + sdlog;
    }
    if sdlog < 0.0 {
        return f64::NAN;
    }
    if sdlog == 0.0 {
        return if x.ln() == meanlog {
            f64::INFINITY
        } else {
            d_zero(log_p)
        };
    }
    if x <= 0.0 {
        return d_zero(log_p);
    }
    let y = (x.ln() - meanlog) / sdlog;
    if log_p {
        -(LN_SQRT_2PI + 0.5 * y * y + (x * sdlog).ln())
    } else {
        ONE_OVER_SQRT_2PI * (-0.5 * y * y).exp() / (x * sdlog)
    }
}

pub fn plnorm(x: f64, meanlog: f64, sdlog: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || meanlog.is_nan() || sdlog.is_nan() {
        return x + meanlog + sdlog;
    }
    if sdlog < 0.0 {
        return f64::NAN;
    }
    if x > 0.0 {
        return pnorm(x.ln(), meanlog, sdlog, lower_tail, log_p);
    }
    dt_zero(lower_tail, log_p)
}

pub fn qlnorm(p: f64, meanlog: f64, sdlog: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || meanlog.is_nan() || sdlog.is_nan() {
        return p + meanlog + sdlog;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    qnorm(p, meanlog, sdlog, lower_tail, log_p).exp()
}

pub fn dweibull(x: f64, shape: f64, scale: f64, log_p: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape <= 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 || !x.is_finite() {
        return d_zero(log_p);
    }
    if x == 0.0 && shape < 1.0 {
        return f64::INFINITY;
    }
    let power = (x / scale).powf(shape - 1.0);
    let exponent = power * (x / scale);
    if log_p {
        -exponent + (shape * power / scale).ln()
    } else {
        shape * power * (-exponent).exp() / scale
    }
}

pub fn pweibull(x: f64, shape: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape <= 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    let exponent = -(x / scale).powf(shape);
    if lower_tail {
        if log_p {
            log1mexp(exponent)
        } else {
            -exponent.exp_m1()
        }
    } else {
        d_exp(exponent, log_p)
    }
}

pub fn qweibull(p: f64, shape: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || shape.is_nan() || scale.is_nan() {
        return p + shape + scale;
    }
    if shape <= 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    scale * (-dt_clog(p, lower_tail, log_p)).powf(1.0 / shape)
}

// The gamma, chi-squared and Poisson distributions.

pub fn dgamma(x: f64, shape: f64, scale: f64, log_p: bool) -> f64 {
    if x.is_nan() || shape.is_nan() || scale.is_nan() {
        return x + shape + scale;
    }
    if shape < 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d_zero(log_p);
    }
    if shape == 0.0 {
        return if x == 0.0 {
            f64::INFINITY
        } else {
            d_zero(log_p)
        };
    }
    if x == 0.0 {
        if shape < 1.0 {
            return f64::INFINITY;
        }
        if shape > 1.0 {
            return d_zero(log_p);
        }
        return if log_p { -scale.ln() } else { 1.0 / scale };
    }
    // x^(a-1) e^-x / Γ(a) is the Poisson density of a - 1 at x, written through a's own
    // density when a < 1 so that the count stays nonnegative.
    if shape < 1.0 {
        let density = dpois_raw(shape, x / scale, log_p);
        return if log_p {
            density
                + if (shape / x).is_finite() {
                    (shape / x).ln()
                } else {
                    shape.ln() - x.ln()
                }
        } else {
            density * shape / x
        };
    }
    let density = dpois_raw(shape - 1.0, x / scale, log_p);
    if log_p {
        density - scale.ln()
    } else {
        density / scale
    }
}

pub fn pgamma(x: f64, shape: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
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
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    pgamma_raw(x, shape, lower_tail, log_p)
}

/// The regularized incomplete gamma function `P(a, x)` or its complement `Q(a, x)`.
///
/// Below `x = 1` the alternating series of `γ(a, x)` gives both tails without cancellation (the
/// upper one as `1 - x^a / Γ(1 + a)` corrected by the series' small remainder). Above it, the
/// positive series of `P` converges for `x <= a` and Legendre's continued fraction of `Q`, by
/// Lentz's method, for `x > a`; the other tail is the complement of the one computed, which is
/// then at least about one half and loses no relative precision.
pub fn pgamma_raw(x: f64, a: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= f64::INFINITY {
        return dt_one(lower_tail, log_p);
    }
    if x < 1.0 {
        let log_leading = a * x.ln() - lgamma1p(a);
        let mut term = 1.0;
        let mut remainder = 0.0;
        let mut n = 1.0;
        loop {
            term *= -x / n;
            let contribution = term * a / (a + n);
            remainder += contribution;
            if contribution.abs() <= f64::EPSILON * 0.5 * remainder.abs() || n > 500.0 {
                break;
            }
            n += 1.0;
        }
        let leading = log_leading.exp();
        let lower = leading * (1.0 + remainder);
        if lower_tail {
            return if log_p {
                log_leading + remainder.ln_1p()
            } else {
                lower
            };
        }
        let upper = -log_leading.exp_m1() - leading * remainder;
        return if !log_p {
            upper
        } else if lower < 0.5 {
            (-lower).ln_1p()
        } else {
            upper.ln()
        };
    }
    if x <= a {
        let log_density = dpois_raw(a, x, true);
        let mut sum = 1.0;
        let mut term = 1.0;
        let mut n = 1.0;
        while n < 1e7 {
            term *= x / (a + n);
            sum += term;
            if term <= f64::EPSILON * 0.5 * sum {
                break;
            }
            n += 1.0;
        }
        let log_lower = log_density + sum.ln();
        return if lower_tail {
            d_exp(log_lower, log_p)
        } else if log_p {
            log1mexp(log_lower)
        } else {
            -log_lower.exp_m1()
        };
    }
    let log_upper = a.ln() + dpois_raw(a, x, true) - upper_gamma_fraction(x, a).ln();
    if lower_tail {
        if log_p {
            log1mexp(log_upper)
        } else {
            -log_upper.exp_m1()
        }
    } else {
        d_exp(log_upper, log_p)
    }
}

/// Legendre's continued fraction `x + 1 - a - 1(1 - a)/(x + 3 - a - 2(2 - a)/(x + 5 - a - ...))`,
/// whose reciprocal times `x^a e^-x / Γ(a)` is `Q(a, x)`.
fn upper_gamma_fraction(x: f64, a: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let mut b = x + 1.0 - a;
    let mut fraction = if b == 0.0 { TINY } else { b };
    let mut c = fraction;
    let mut d = 0.0;
    let mut n = 1.0;
    while n < 1e7 {
        let numerator = -n * (n - a);
        b += 2.0;
        d = b + numerator * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = c * d;
        fraction *= delta;
        if (delta - 1.0).abs() <= f64::EPSILON {
            break;
        }
        n += 1.0;
    }
    fraction
}

pub fn qgamma(p: f64, shape: f64, scale: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || shape.is_nan() || scale.is_nan() {
        return p + shape + scale;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    if shape < 0.0 || scale <= 0.0 {
        return f64::NAN;
    }
    if shape == 0.0 {
        return 0.0;
    }
    if shape < 1e-10 {
        warn(format!(
            "value of shape ({}) is extremely small: results may be unreliable",
            format_g(shape)
        ));
    }
    scale * gamma_quantile(p, shape, lower_tail, log_p)
}

/// The quantile of the standard gamma distribution. The root is found on the smaller tail's
/// log probability, so the far tails keep their precision: in `log x` for the lower tail, where
/// `log P` is close to linear in `log x`, and in `x` for the upper, where `log Q` is close to
/// linear in `x`.
fn gamma_quantile(p: f64, a: f64, lower_tail: bool, log_p: bool) -> f64 {
    let lower = dt_qiv(p, lower_tail, log_p);
    if lower <= 0.5 {
        let target = dt_log(p, lower_tail, log_p);
        let log_lower = |x: f64| pgamma_raw(x, a, true, true);
        let small = (target + lgamma1p(a)) / a;
        // Near zero P(a, x) ~ x^a / Γ(1 + a), so a root this small underflows.
        if small < MIN_LOG {
            return 0.0;
        }
        let z = qnorm(target, 0.0, 1.0, true, true);
        let start = wilson_hilferty(a, z)
            .filter(|_| a >= 1.0)
            .map_or(small, f64::ln);
        let ceiling = (a + 1.0).ln();
        let log_root = increasing_root(start.min(ceiling), f64::NEG_INFINITY, ceiling, |u| {
            let x = u.exp();
            let value = log_lower(x);
            (value - target, (u + dgamma(x, a, 1.0, true) - value).exp())
        });
        if log_root.exp() < f64::MIN_POSITIVE {
            return log_root.exp();
        }
        return increasing_root(log_root.exp(), 0.0, a + 1.0, |x| {
            let value = log_lower(x);
            (value - target, (dgamma(x, a, 1.0, true) - value).exp())
        });
    }
    let target = dt_clog(p, lower_tail, log_p);
    if a < 1.0 {
        // The median may be far below one, so the root is found in log x first.
        let lower_probability = -target.exp_m1();
        let small = (lower_probability.ln() + lgamma1p(a)) / a;
        if small < MIN_LOG {
            return 0.0;
        }
        let log_root = increasing_root(small.min(0.0), f64::NEG_INFINITY, f64::INFINITY, |u| {
            let x = u.exp();
            let value = pgamma_raw(x, a, false, true);
            (target - value, (u + dgamma(x, a, 1.0, true) - value).exp())
        });
        if log_root.exp() < f64::MIN_POSITIVE {
            return log_root.exp();
        }
        return increasing_root(log_root.exp(), 0.0, f64::INFINITY, |x| {
            let value = pgamma_raw(x, a, false, true);
            (target - value, (dgamma(x, a, 1.0, true) - value).exp())
        });
    }
    let floor = a - 1.0;
    let z = qnorm(target, 0.0, 1.0, false, true);
    let tail = -target + (a - 1.0) * (-target).max(1.0).ln() - lgammafn(a);
    let start = wilson_hilferty(a, z).unwrap_or(tail).max(floor);
    increasing_root(start, floor, f64::INFINITY, |x| {
        let value = pgamma_raw(x, a, false, true);
        (target - value, (dgamma(x, a, 1.0, true) - value).exp())
    })
}

/// Wilson and Hilferty's normal approximation to the gamma quantile at normal quantile `z`.
fn wilson_hilferty(a: f64, z: f64) -> Option<f64> {
    let c = 1.0 / (9.0 * a);
    let root = 1.0 - c + z * c.sqrt();
    (root > 0.0 && z.is_finite()).then_some(a * root * root * root)
}

/// C's `%g` formatting, for warning messages.
fn format_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return "0".into();
    }
    let exponent = x.abs().log10().floor() as i32;
    if !(-4..6).contains(&exponent) {
        let mantissa = format!("{x:.5e}");
        let (digits, power) = mantissa.split_once('e').unwrap_or((&mantissa, "0"));
        let power: i32 = power.parse().unwrap_or(0);
        return format!(
            "{}e{}{:02}",
            trim_zeros(digits),
            if power < 0 { '-' } else { '+' },
            power.abs()
        );
    }
    let decimals = (5 - exponent).max(0) as usize;
    trim_zeros(&format!("{x:.decimals$}"))
}

fn trim_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text.to_owned()
    }
}

pub fn dchisq(x: f64, df: f64, log_p: bool) -> f64 {
    dgamma(x, df / 2.0, 2.0, log_p)
}

pub fn pchisq(x: f64, df: f64, lower_tail: bool, log_p: bool) -> f64 {
    pgamma(x, df / 2.0, 2.0, lower_tail, log_p)
}

pub fn qchisq(p: f64, df: f64, lower_tail: bool, log_p: bool) -> f64 {
    qgamma(p, 0.5 * df, 2.0, lower_tail, log_p)
}

pub fn dpois(x: f64, lambda: f64, log_p: bool) -> f64 {
    if x.is_nan() || lambda.is_nan() {
        return x + lambda;
    }
    if lambda < 0.0 {
        return f64::NAN;
    }
    if warn_non_integer(x) {
        return d_zero(log_p);
    }
    if x < 0.0 || !x.is_finite() {
        return d_zero(log_p);
    }
    dpois_raw(force_int(x), lambda, log_p)
}

pub fn ppois(x: f64, lambda: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || lambda.is_nan() {
        return x + lambda;
    }
    if lambda < 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if lambda == 0.0 || !x.is_finite() {
        return dt_one(lower_tail, log_p);
    }
    let x = (x + 1e-7).floor();
    pgamma(lambda, x + 1.0, 1.0, !lower_tail, log_p)
}

pub fn qpois(p: f64, lambda: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || lambda.is_nan() {
        return p + lambda;
    }
    if !lambda.is_finite() || lambda < 0.0 || p_invalid(p, log_p) {
        return f64::NAN;
    }
    if lambda == 0.0 {
        return 0.0;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    let sigma = lambda.sqrt();
    let start = cornish_fisher(p, lambda, sigma, 1.0 / sigma, lower_tail, log_p);
    discrete_quantile(p, start, f64::INFINITY, lower_tail, log_p, |y| {
        ppois(y, lambda, lower_tail, log_p)
    })
}

/// The Cornish-Fisher approximation to a discrete quantile, from the mean, the standard
/// deviation and the skewness.
fn cornish_fisher(
    p: f64,
    mu: f64,
    sigma: f64,
    skewness: f64,
    lower_tail: bool,
    log_p: bool,
) -> f64 {
    let z = qnorm(p, 0.0, 1.0, lower_tail, log_p);
    force_int(mu + sigma * (z + skewness * (z * z - 1.0) / 6.0)).max(0.0)
}

/// The smallest count `y` in `0..=max` whose probability on the requested tail and scale
/// (`cdf`) reaches `p`: `F(y) >= p` for the lower tail, `P(X > y) <= p` for the upper. The
/// comparison allows a few ulps, so that a `p` computed as the probability of a count maps back
/// to that count despite rounding. The search gallops from `start` and then bisects.
fn discrete_quantile(
    p: f64,
    start: f64,
    max: f64,
    lower_tail: bool,
    log_p: bool,
    cdf: impl Fn(f64) -> f64,
) -> f64 {
    let target = match (log_p, lower_tail) {
        (false, true) => p * (1.0 - 8.0 * f64::EPSILON),
        (false, false) => p * (1.0 + 8.0 * f64::EPSILON),
        (true, true) => p * (1.0 + 2.0 * f64::EPSILON),
        (true, false) => p * (1.0 - 2.0 * f64::EPSILON),
    };
    let reached = |y: f64| {
        let value = cdf(y);
        if lower_tail {
            value >= target
        } else {
            value <= target
        }
    };
    let start = if start.is_finite() {
        start.clamp(0.0, max)
    } else {
        0.0
    };
    let (mut low, mut high);
    if reached(start) {
        high = start;
        low = -1.0;
        let mut step = 1.0;
        while high > 0.0 {
            let candidate = (high - step).max(0.0);
            if reached(candidate) {
                high = candidate;
                step *= 2.0;
            } else {
                low = candidate;
                break;
            }
        }
    } else {
        low = start;
        high = max;
        let mut step = 1.0;
        loop {
            let candidate = low + step;
            if candidate >= max {
                break;
            }
            if reached(candidate) {
                high = candidate;
                break;
            }
            low = candidate;
            step *= 2.0;
        }
    }
    while high - low > 1.0 {
        let middle = ((low + high) / 2.0).floor();
        if reached(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    high
}

// The beta distribution and the distributions computed from it: binomial, negative binomial,
// geometric, t and F.

pub fn dbeta(x: f64, a: f64, b: f64, log_p: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if a < 0.0 || b < 0.0 {
        return f64::NAN;
    }
    if !(0.0..=1.0).contains(&x) {
        return d_zero(log_p);
    }
    if a == 0.0 || b == 0.0 || !a.is_finite() || !b.is_finite() {
        // The limits are point masses: at 0 and 1, at 0, at 1, or at 1/2.
        let at = if a == 0.0 && b == 0.0 {
            x == 0.0 || x == 1.0
        } else if a == 0.0 || a / b == f64::INFINITY {
            x == 0.0
        } else if b == 0.0 || b / a == f64::INFINITY {
            x == 1.0
        } else {
            x == 0.5
        };
        return if at { f64::INFINITY } else { d_zero(log_p) };
    }
    if x == 0.0 {
        if a > 1.0 {
            return d_zero(log_p);
        }
        if a < 1.0 {
            return f64::INFINITY;
        }
        return d_val(b, log_p);
    }
    if x == 1.0 {
        if b > 1.0 {
            return d_zero(log_p);
        }
        if b < 1.0 {
            return f64::INFINITY;
        }
        return d_val(a, log_p);
    }
    let log_density = if a <= 2.0 || b <= 2.0 {
        (a - 1.0) * x.ln() + (b - 1.0) * (-x).ln_1p() - lbeta(a, b)
    } else {
        // The binomial form keeps the large terms of the logs from cancelling.
        (a + b - 1.0).ln() + dbinom_raw(a - 1.0, a + b - 2.0, x, 1.0 - x, true)
    };
    d_exp(log_density, log_p)
}

pub fn pbeta(x: f64, a: f64, b: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() {
        return x + a + b;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= 1.0 {
        return dt_one(lower_tail, log_p);
    }
    pbeta_raw(x, a, b, lower_tail, log_p)
}

/// `pbeta` for `0 < x < 1`, including the limits of zero and infinite shapes.
pub fn pbeta_raw(x: f64, a: f64, b: f64, lower_tail: bool, log_p: bool) -> f64 {
    if a < 0.0 || b < 0.0 {
        return f64::NAN;
    }
    if a == 0.0 || b == 0.0 || !a.is_finite() || !b.is_finite() {
        if a == 0.0 && b == 0.0 {
            return if log_p { -LN_2 } else { 0.5 };
        }
        if a == 0.0 || a / b == f64::INFINITY {
            return dt_one(lower_tail, log_p);
        }
        if b == 0.0 || b / a == f64::INFINITY {
            return dt_zero(lower_tail, log_p);
        }
        return if x < 0.5 {
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    incomplete_beta(x, 0.5 - x + 0.5, a, b, lower_tail, log_p)
}

/// The regularized incomplete beta function `I_x(a, b)` or its complement, with `y = 1 - x`
/// passed separately so a caller can keep the precision of whichever is small.
///
/// The continued fraction of `I_x(a, b)` converges quickly for `x < (a + 1)/(a + b + 2)`;
/// beyond that, `I_x(a, b) = 1 - I_y(b, a)` turns the problem around. The fraction's value is
/// close to `1 - c` with `c = (a + b) x / (a + 1)`, so it cancels when `c` is close to one, which
/// happens on the turned-around side when one shape is small and `x` is close to an end; the
/// other side is used then, where the fraction still converges quickly. The prefactor
/// `x^a y^b / (a B(a, b))` is computed in saddle point form, which keeps its precision when the
/// shapes are large.
pub fn incomplete_beta(x: f64, y: f64, a: f64, b: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if y <= 0.0 {
        return dt_one(lower_tail, log_p);
    }
    let mut swap = x > (a + 1.0) / (a + b + 2.0);
    if a.min(b) <= 1.0 {
        let direct = ((a + b) * x / (a + 1.0) - 1.0).abs();
        let turned = ((a + b) * y / (b + 1.0) - 1.0).abs();
        let (chosen, other) = if swap {
            (turned, direct)
        } else {
            (direct, turned)
        };
        if chosen < 0.05 && other > chosen && other < 3.0 {
            swap = !swap;
        }
    }
    if swap {
        return beta_fraction_tail(y, x, b, a, !lower_tail, log_p);
    }
    beta_fraction_tail(x, y, a, b, lower_tail, log_p)
}

/// `I_x(a, b)` or its complement by the continued fraction, without choosing the side.
fn beta_fraction_tail(x: f64, y: f64, a: f64, b: f64, lower_tail: bool, log_p: bool) -> f64 {
    let log_value = log_beta_kernel(x, y, a, b) - beta_fraction(x, a, b).ln();
    if lower_tail {
        d_exp(log_value, log_p)
    } else if log_p {
        log1mexp(log_value)
    } else {
        -log_value.exp_m1()
    }
}

/// `log(x^a y^b / (a B(a, b)))` for `x + y = 1`, in Loader's saddle point form. With
/// `n = a + b`, Stirling's formula for the gamma functions leaves
/// `sqrt(b / (2π n a))`, their Stirling errors, and the deviances `bd0(a, nx)` and `bd0(b, ny)`.
/// The pieces are grouped so that nothing large cancels however small either shape is: for
/// `a < 1`, `-log(a)/2 - stirlerr(a)` is evaluated as `a log a - a + log √(2π) - log Γ(1 + a)`,
/// and `log(b/n)` as `-log1p(a/b)` when `a` is the smaller shape.
fn log_beta_kernel(x: f64, y: f64, a: f64, b: f64) -> f64 {
    let n = a + b;
    let first = if a < 1.0 {
        a * a.ln() - a + LN_SQRT_2PI - lgamma1p(a)
    } else {
        -0.5 * a.ln() - stirlerr(a)
    };
    let ratio = if a < b {
        -0.5 * (a / b).ln_1p()
    } else {
        0.5 * (b / n).ln()
    };
    first + ratio - stirlerr(b) + stirlerr(n) - LN_SQRT_2PI - bd0(a, n * x) - bd0(b, n * y)
}

/// The continued fraction `1 + d1/(1 + d2/(1 + ...))` of the incomplete beta function, by
/// Lentz's method.
fn beta_fraction(x: f64, a: f64, b: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let mut fraction = 1.0;
    let mut c = 1.0;
    let mut d = 0.0;
    let mut step = |numerator: f64, fraction: &mut f64| -> f64 {
        d = 1.0 + numerator * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = c * d;
        *fraction *= delta;
        delta
    };
    let mut m = 0.0;
    while m < 1e7 {
        let odd = -(a + m) * (a + b + m) * x / ((a + 2.0 * m) * (a + 2.0 * m + 1.0));
        let first = step(odd, &mut fraction);
        m += 1.0;
        let even = m * (b - m) * x / ((a + 2.0 * m - 1.0) * (a + 2.0 * m));
        let second = step(even, &mut fraction);
        if (first - 1.0).abs() <= f64::EPSILON && (second - 1.0).abs() <= f64::EPSILON {
            break;
        }
    }
    fraction
}

pub fn qbeta(alpha: f64, p: f64, q: f64, lower_tail: bool, log_p: bool) -> f64 {
    if alpha.is_nan() || p.is_nan() || q.is_nan() {
        return alpha + p + q;
    }
    if p < 0.0 || q < 0.0 {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(alpha, 0.0, 1.0, lower_tail, log_p) {
        return edge;
    }
    if p == 0.0 || q == 0.0 || !p.is_finite() || !q.is_finite() {
        if p == 0.0 && q == 0.0 {
            let half = if log_p { -LN_2 } else { 0.5 };
            return if alpha < half {
                0.0
            } else if alpha > half {
                1.0
            } else {
                0.5
            };
        }
        if p == 0.0 || p / q == f64::INFINITY {
            return 0.0;
        }
        if q == 0.0 || q / p == f64::INFINITY {
            return 1.0;
        }
        return 0.5;
    }
    let lower = dt_qiv(alpha, lower_tail, log_p);
    if lower <= 0.5 {
        let root = beta_lower_quantile(dt_log(alpha, lower_tail, log_p), p, q);
        // On the log scale GNU R reports an underflowing quantile as half the smallest normal
        // double rather than as zero or a subnormal.
        return if log_p && root < f64::MIN_POSITIVE {
            f64::MIN_POSITIVE / 2.0
        } else {
            root
        };
    }
    let target = dt_clog(alpha, lower_tail, log_p);
    let start = 0.5 - beta_lower_quantile(target, q, p) + 0.5;
    if start >= 1.0 {
        return start;
    }
    increasing_root(start, 0.0, 1.0, |x| {
        let value = incomplete_beta(x, 0.5 - x + 0.5, p, q, false, true);
        (target - value, (dbeta(x, p, q, true) - value).exp())
    })
}

/// The `x` whose lower tail `I_x(a, b)` has log probability `target`, at most `log(1/2)`.
fn beta_lower_quantile(target: f64, a: f64, b: f64) -> f64 {
    let log_lower = |x: f64, y: f64| incomplete_beta(x, y, a, b, true, true);
    // Near zero, I_x(a, b) ~ x^a / (a B(a, b)).
    let small = (target + a.ln() + lbeta(a, b)) / a;
    if small < MIN_LOG {
        return 0.0;
    }
    let mean = (a / (a + b)).ln();
    let log_root = increasing_root(small.min(mean), f64::NEG_INFINITY, 0.0, |u| {
        let x = u.exp();
        let value = log_lower(x, -u.exp_m1());
        (value - target, (u + dbeta(x, a, b, true) - value).exp())
    });
    let start = log_root.exp();
    if start < f64::MIN_POSITIVE {
        return start;
    }
    increasing_root(start, 0.0, 1.0, |x| {
        let value = log_lower(x, 0.5 - x + 0.5);
        (value - target, (dbeta(x, a, b, true) - value).exp())
    })
}

pub fn dbinom(x: f64, n: f64, p: f64, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() || p.is_nan() {
        return x + n + p;
    }
    if !(0.0..=1.0).contains(&p) || negative_or_non_integer(n) {
        return f64::NAN;
    }
    if warn_non_integer(x) {
        return d_zero(log_p);
    }
    if x < 0.0 || !x.is_finite() {
        return d_zero(log_p);
    }
    dbinom_raw(force_int(x), force_int(n), p, 1.0 - p, log_p)
}

pub fn pbinom(x: f64, n: f64, p: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() || p.is_nan() {
        return x + n + p;
    }
    if !n.is_finite() || !p.is_finite() {
        return f64::NAN;
    }
    if is_non_integer(n) {
        warn(format!("non-integer n = {n:.6}"));
        return f64::NAN;
    }
    let n = force_int(n);
    if n < 0.0 || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if x < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    let x = (x + 1e-7).floor();
    if n <= x {
        return dt_one(lower_tail, log_p);
    }
    pbeta(p, x + 1.0, n - x, !lower_tail, log_p)
}

pub fn qbinom(p: f64, n: f64, pr: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || n.is_nan() || pr.is_nan() {
        return p + n + pr;
    }
    if !n.is_finite() || !pr.is_finite() {
        return f64::NAN;
    }
    if n != n.floor() || n < 0.0 || !(0.0..=1.0).contains(&pr) {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, n, lower_tail, log_p) {
        return edge;
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
    let start = cornish_fisher(p, mu, sigma, (q - pr) / sigma, lower_tail, log_p).min(n);
    discrete_quantile(p, start, n, lower_tail, log_p, |y| {
        pbinom(y, n, pr, lower_tail, log_p)
    })
}

pub fn dnbinom(x: f64, size: f64, prob: f64, log_p: bool) -> f64 {
    if x.is_nan() || size.is_nan() || prob.is_nan() {
        return x + size + prob;
    }
    if prob <= 0.0 || prob > 1.0 || size < 0.0 {
        return f64::NAN;
    }
    if warn_non_integer(x) {
        return d_zero(log_p);
    }
    if x < 0.0 || !x.is_finite() {
        return d_zero(log_p);
    }
    if x == 0.0 && size == 0.0 {
        return d_one(log_p);
    }
    let x = force_int(x);
    let size = if size.is_finite() { size } else { f64::MAX };
    let density = dbinom_raw(size, x + size, prob, 1.0 - prob, log_p);
    let ratio = size / (size + x);
    if log_p {
        ratio.ln() + density
    } else {
        ratio * density
    }
}

pub fn dnbinom_mu(x: f64, size: f64, mu: f64, log_p: bool) -> f64 {
    if x.is_nan() || size.is_nan() || mu.is_nan() {
        return x + size + mu;
    }
    if mu < 0.0 || size < 0.0 {
        return f64::NAN;
    }
    if warn_non_integer(x) {
        return d_zero(log_p);
    }
    if x < 0.0 || !x.is_finite() {
        return d_zero(log_p);
    }
    if x == 0.0 && size == 0.0 {
        return d_one(log_p);
    }
    let x = force_int(x);
    if !size.is_finite() {
        return dpois_raw(x, mu, log_p);
    }
    if x == 0.0 {
        let log_ratio = if size < mu {
            (size / (size + mu)).ln()
        } else {
            (-mu / (size + mu)).ln_1p()
        };
        return d_exp(size * log_ratio, log_p);
    }
    if x < 1e-10 * size {
        // For a size much larger than x, the density is Poisson's up to a factor 1 + O(x²/size).
        let log_mean = if size < mu {
            (size / (1.0 + size / mu)).ln()
        } else {
            (mu / (1.0 + mu / size)).ln()
        };
        return d_exp(
            x * log_mean - mu - lgammafn(x + 1.0) + (x * (x - 1.0) / (2.0 * size)).ln_1p(),
            log_p,
        );
    }
    let ratio = size / (size + x);
    let density = dbinom_raw(size, x + size, size / (size + mu), mu / (size + mu), log_p);
    if log_p {
        ratio.ln() + density
    } else {
        ratio * density
    }
}

pub fn pnbinom(x: f64, size: f64, prob: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || size.is_nan() || prob.is_nan() {
        return x + size + prob;
    }
    if !size.is_finite() || !prob.is_finite() || size < 0.0 || prob <= 0.0 || prob > 1.0 {
        return f64::NAN;
    }
    if size == 0.0 {
        return if x >= 0.0 {
            dt_one(lower_tail, log_p)
        } else {
            dt_zero(lower_tail, log_p)
        };
    }
    if x < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if !x.is_finite() {
        return dt_one(lower_tail, log_p);
    }
    let x = (x + 1e-7).floor();
    pbeta(prob, size, x + 1.0, lower_tail, log_p)
}

pub fn pnbinom_mu(x: f64, size: f64, mu: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || size.is_nan() || mu.is_nan() {
        return x + size + mu;
    }
    if !mu.is_finite() || size < 0.0 || mu < 0.0 {
        return f64::NAN;
    }
    if size == 0.0 {
        return if x >= 0.0 {
            dt_one(lower_tail, log_p)
        } else {
            dt_zero(lower_tail, log_p)
        };
    }
    if x < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if !x.is_finite() {
        return dt_one(lower_tail, log_p);
    }
    if !size.is_finite() {
        return ppois(x, mu, lower_tail, log_p);
    }
    let x = (x + 1e-7).floor();
    incomplete_beta(
        size / (size + mu),
        mu / (size + mu),
        size,
        x + 1.0,
        lower_tail,
        log_p,
    )
}

pub fn qnbinom(p: f64, size: f64, prob: f64, lower_tail: bool, log_p: bool) -> f64 {
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
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    let inverse = 1.0 / prob;
    let odds = (1.0 - prob) * inverse;
    let sigma = (size * odds * inverse).sqrt();
    let start = cornish_fisher(
        p,
        size * odds,
        sigma,
        (inverse + odds) / sigma,
        lower_tail,
        log_p,
    );
    discrete_quantile(p, start, f64::INFINITY, lower_tail, log_p, |y| {
        pnbinom(y, size, prob, lower_tail, log_p)
    })
}

pub fn qnbinom_mu(p: f64, size: f64, mu: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || size.is_nan() || mu.is_nan() {
        return p + size + mu;
    }
    if size == f64::INFINITY {
        return qpois(p, mu, lower_tail, log_p);
    }
    if mu < 0.0 || size < 0.0 {
        return f64::NAN;
    }
    if mu == 0.0 || size == 0.0 {
        return 0.0;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    let prob = size / (size + mu);
    let inverse = 1.0 / prob;
    let odds = (1.0 - prob) * inverse;
    let sigma = (size * odds * inverse).sqrt();
    let start = cornish_fisher(p, mu, sigma, (inverse + odds) / sigma, lower_tail, log_p);
    discrete_quantile(p, start, f64::INFINITY, lower_tail, log_p, |y| {
        pnbinom_mu(y, size, mu, lower_tail, log_p)
    })
}

pub fn dgeom(x: f64, p: f64, log_p: bool) -> f64 {
    if x.is_nan() || p.is_nan() {
        return x + p;
    }
    if p <= 0.0 || p > 1.0 {
        return f64::NAN;
    }
    if warn_non_integer(x) {
        return d_zero(log_p);
    }
    if x < 0.0 || !x.is_finite() || p == 0.0 {
        return d_zero(log_p);
    }
    let x = force_int(x);
    let density = dbinom_raw(0.0, x, p, 1.0 - p, log_p);
    if log_p { p.ln() + density } else { p * density }
}

pub fn pgeom(x: f64, p: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || p.is_nan() {
        return x + p;
    }
    if p <= 0.0 || p > 1.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if !x.is_finite() {
        return dt_one(lower_tail, log_p);
    }
    let x = (x + 1e-7).floor();
    if p == 1.0 {
        return dt_one(lower_tail, log_p);
    }
    // The upper tail is (1 - p)^(x + 1).
    let log_upper = (-p).ln_1p() * (x + 1.0);
    if log_p {
        if lower_tail {
            log1mexp(log_upper)
        } else {
            log_upper
        }
    } else if lower_tail {
        -log_upper.exp_m1()
    } else {
        log_upper.exp()
    }
}

pub fn qgeom(p: f64, prob: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || prob.is_nan() {
        return p + prob;
    }
    if prob <= 0.0 || prob > 1.0 || p_invalid(p, log_p) {
        return f64::NAN;
    }
    if prob == 1.0 {
        return 0.0;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    (dt_clog(p, lower_tail, log_p) / (-prob).ln_1p() - 1.0 - 1e-12)
        .ceil()
        .max(0.0)
}

pub fn dt(x: f64, n: f64, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if n <= 0.0 {
        return f64::NAN;
    }
    if !x.is_finite() {
        return d_zero(log_p);
    }
    if !n.is_finite() {
        return dnorm(x, 0.0, 1.0, log_p);
    }
    // Loader's form: the ratio of gamma functions as a deviance and two Stirling errors, and
    // (1 + x²/n)^(-(n+1)/2) as a deviance, so that nothing large cancels.
    let ratio = -bd0(n / 2.0, (n + 1.0) / 2.0) + stirlerr((n + 1.0) / 2.0) - stirlerr(n / 2.0);
    let square_ratio = x * x / n;
    let large = square_ratio > 1.0 / f64::EPSILON;
    let (log_root, exponent);
    if large {
        log_root = x.abs().ln() - n.ln() / 2.0;
        exponent = n * log_root;
    } else if square_ratio > 0.2 {
        log_root = square_ratio.ln_1p() / 2.0;
        exponent = n * log_root;
    } else {
        log_root = square_ratio.ln_1p() / 2.0;
        exponent = -bd0(n / 2.0, (n + x * x) / 2.0) + x * x / 2.0;
    }
    if log_p {
        return ratio - exponent - (LN_SQRT_2PI + log_root);
    }
    let inverse_root = if large {
        n.sqrt() / x.abs()
    } else {
        (-log_root).exp()
    };
    (ratio - exponent).exp() * ONE_OVER_SQRT_2PI * inverse_root
}

pub fn pt(x: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if n <= 0.0 {
        return f64::NAN;
    }
    if !x.is_finite() {
        return if x < 0.0 {
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    if !n.is_finite() {
        return pnorm(x, 0.0, 1.0, lower_tail, log_p);
    }
    let one_plus = 1.0 + (x / n) * x;
    // Twice the probability of the tail beyond |x|.
    let value = if one_plus > 1e100 {
        // Where 1/(1 + x²/n) underflows: I_z(a, 1/2) ~ z^a / (a B(a, 1/2)).
        let log_value =
            -0.5 * n * (2.0 * x.abs().ln() - n.ln()) - lbeta(0.5 * n, 0.5) - (0.5 * n).ln();
        if log_p { log_value } else { log_value.exp() }
    } else if n > x * x {
        pbeta(x * x / (n + x * x), 0.5, n / 2.0, false, log_p)
    } else {
        pbeta(1.0 / one_plus, n / 2.0, 0.5, true, log_p)
    };
    let lower = if x <= 0.0 { !lower_tail } else { lower_tail };
    if log_p {
        if lower {
            (-0.5 * value.exp()).ln_1p()
        } else {
            value - LN_2
        }
    } else {
        let half = value / 2.0;
        if lower { 0.5 - half + 0.5 } else { half }
    }
}

pub fn qt(p: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || n.is_nan() {
        return p + n;
    }
    if let Some(edge) = quantile_boundaries(p, f64::NEG_INFINITY, f64::INFINITY, lower_tail, log_p)
    {
        return edge;
    }
    if n <= 0.0 {
        return f64::NAN;
    }
    if n > 1e20 {
        return qnorm(p, 0.0, 1.0, lower_tail, log_p);
    }
    let lower = dt_qiv(p, lower_tail, log_p);
    if lower == 0.5 {
        return 0.0;
    }
    let negative = lower < 0.5;
    // The log of the tail beyond the quantile, which is the smaller one.
    let target = if negative {
        dt_log(p, lower_tail, log_p)
    } else {
        dt_clog(p, lower_tail, log_p)
    };
    let magnitude = if n == 1.0 {
        1.0 / tanpi(target.exp())
    } else if n == 2.0 {
        // The tail beyond t is (1 - t/√(2 + t²))/2, which inverts in closed form.
        let twice = 2.0 * target.exp();
        if twice < 1e-100 {
            (0.5 * (LN_2 - (LN_2 + target) - (2.0 - twice).ln())).exp()
        } else {
            (2.0 / (twice * (2.0 - twice)) - 2.0).sqrt()
        }
    } else if pt(f64::MAX, n, false, true) > target {
        // The quantile is beyond the largest double.
        f64::INFINITY
    } else {
        let z = qnorm(target, 0.0, 1.0, false, true);
        let start = (z + (z * z * z + z) / (4.0 * n)).max(1e-300).ln();
        let log_root = increasing_root(start, f64::NEG_INFINITY, f64::INFINITY, |u| {
            let t = u.exp();
            let value = pt(t, n, false, true);
            (target - value, (u + dt(t, n, true) - value).exp())
        });
        increasing_root(log_root.exp(), 0.0, f64::INFINITY, |t| {
            let value = pt(t, n, false, true);
            (target - value, (dt(t, n, true) - value).exp())
        })
    };
    if negative { -magnitude } else { magnitude }
}

pub fn df(x: f64, m: f64, n: f64, log_p: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d_zero(log_p);
    }
    if x == 0.0 {
        return if m > 2.0 {
            d_zero(log_p)
        } else if m == 2.0 {
            d_one(log_p)
        } else {
            f64::INFINITY
        };
    }
    if !m.is_finite() && !n.is_finite() {
        return if x == 1.0 {
            f64::INFINITY
        } else {
            d_zero(log_p)
        };
    }
    if !n.is_finite() {
        return dgamma(x, m / 2.0, 2.0 / m, log_p);
    }
    if m > 1e14 {
        let density = dgamma(1.0 / x, n / 2.0, 2.0 / n, log_p);
        return if log_p {
            density - 2.0 * x.ln()
        } else {
            density / (x * x)
        };
    }
    let f = 1.0 / (n + x * m);
    let q = n * f;
    let p = x * m * f;
    let (factor, density) = if m >= 2.0 {
        (
            m * q / 2.0,
            dbinom_raw((m - 2.0) / 2.0, (m + n - 2.0) / 2.0, p, q, log_p),
        )
    } else {
        (
            m * m * q / (2.0 * p * (m + n)),
            dbinom_raw(m / 2.0, (m + n) / 2.0, p, q, log_p),
        )
    };
    if log_p {
        factor.ln() + density
    } else {
        factor * density
    }
}

pub fn pf(x: f64, m: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= f64::INFINITY {
        return dt_one(lower_tail, log_p);
    }
    if m == f64::INFINITY {
        if n == f64::INFINITY {
            if x < 1.0 {
                return dt_zero(lower_tail, log_p);
            }
            if x == 1.0 {
                return if log_p { -LN_2 } else { 0.5 };
            }
            return dt_one(lower_tail, log_p);
        }
        return pchisq(n / x, n, !lower_tail, log_p);
    }
    if n == f64::INFINITY {
        return pchisq(x * m, m, lower_tail, log_p);
    }
    if m * x > n {
        pbeta(n / (n + m * x), n / 2.0, m / 2.0, !lower_tail, log_p)
    } else {
        pbeta(m * x / (n + m * x), m / 2.0, n / 2.0, lower_tail, log_p)
    }
}

pub fn qf(p: f64, m: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || m.is_nan() || n.is_nan() {
        return p + m + n;
    }
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    if m <= n && n > 4e5 {
        if !m.is_finite() {
            return 1.0;
        }
        return qchisq(p, m, lower_tail, log_p) / m;
    }
    if m > 4e5 {
        return n / qchisq(p, n, !lower_tail, log_p);
    }
    (1.0 / qbeta(p, n / 2.0, m / 2.0, !lower_tail, log_p) - 1.0) * (n / m)
}

// The hypergeometric distribution.

pub fn dhyper(x: f64, r: f64, b: f64, n: f64, log_p: bool) -> f64 {
    if x.is_nan() || r.is_nan() || b.is_nan() || n.is_nan() {
        return x + r + b + n;
    }
    if negative_or_non_integer(r)
        || negative_or_non_integer(b)
        || negative_or_non_integer(n)
        || n > r + b
    {
        return f64::NAN;
    }
    if x < 0.0 {
        return d_zero(log_p);
    }
    if warn_non_integer(x) {
        return d_zero(log_p);
    }
    let (x, r, b, n) = (force_int(x), force_int(r), force_int(b), force_int(n));
    if n < x || r < x || n - x > b {
        return d_zero(log_p);
    }
    if n == 0.0 {
        return if x == 0.0 {
            d_one(log_p)
        } else {
            d_zero(log_p)
        };
    }
    // The ratio of three binomial densities at a common p cancels the powers of p, leaving
    // C(r, x) C(b, n - x) / C(r + b, n), with every factor in saddle point form.
    let p = n / (r + b);
    let q = (r + b - n) / (r + b);
    let first = dbinom_raw(x, r, p, q, log_p);
    let second = dbinom_raw(n - x, b, p, q, log_p);
    let third = dbinom_raw(n, r + b, p, q, log_p);
    if log_p {
        first + second - third
    } else {
        first * second / third
    }
}

pub fn phyper(x: f64, red: f64, black: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || red.is_nan() || black.is_nan() || n.is_nan() {
        return x + red + black + n;
    }
    let mut x = (x + 1e-7).floor();
    let (mut red, mut black, n) = (force_int(red), force_int(black), force_int(n));
    if red < 0.0 || black < 0.0 || !(red + black).is_finite() || n < 0.0 || n > red + black {
        return f64::NAN;
    }
    let mut lower_tail = lower_tail;
    // Summing the shorter tail is faster and more accurate.
    if x * (red + black) > n * red {
        std::mem::swap(&mut red, &mut black);
        x = n - x - 1.0;
        lower_tail = !lower_tail;
    }
    if x < 0.0 || x < n - black {
        return dt_zero(lower_tail, log_p);
    }
    if x >= red || x >= n {
        return dt_one(lower_tail, log_p);
    }
    let density = dhyper(x, red, black, n, log_p);
    if (!log_p && density == 0.0) || (log_p && density == f64::NEG_INFINITY) {
        return dt_zero(lower_tail, log_p);
    }
    let ratio = hyper_tail_ratio(x, red, black, n, log_p);
    if log_p {
        let log_value = density + ratio;
        if lower_tail {
            log_value
        } else {
            log1mexp(log_value)
        }
    } else {
        let value = density * ratio;
        if lower_tail { value } else { 0.5 - value + 0.5 }
    }
}

/// `P(X <= x) / P(X = x)` for the hypergeometric distribution, by summing the ratios of
/// successive densities downwards from `x` (Smith's algorithm).
fn hyper_tail_ratio(x: f64, red: f64, black: f64, n: f64, log_p: bool) -> f64 {
    let mut sum = 0.0;
    let mut term = 1.0;
    let mut k = x;
    while k > 0.0 && term >= f64::EPSILON * sum {
        term *= k * (black - n + k) / (n + 1.0 - k) / (red + 1.0 - k);
        sum += term;
        k -= 1.0;
    }
    if log_p { sum.ln_1p() } else { 1.0 + sum }
}

pub fn qhyper(p: f64, red: f64, black: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || red.is_nan() || black.is_nan() || n.is_nan() {
        return p + red + black + n;
    }
    if !p.is_finite() || !red.is_finite() || !black.is_finite() || !n.is_finite() {
        return f64::NAN;
    }
    if p_invalid(p, log_p) {
        return f64::NAN;
    }
    let (red, black, n) = (force_int(red), force_int(black), force_int(n));
    let total = red + black;
    if red < 0.0 || black < 0.0 || n < 0.0 || n > total {
        return f64::NAN;
    }
    let start = (n - black).max(0.0);
    let end = n.min(red);
    if let Some(edge) = quantile_boundaries(p, start, end, lower_tail, log_p) {
        return edge;
    }
    let target = dt_qiv(p, lower_tail, log_p) * (1.0 - 1000.0 * f64::EPSILON);
    let mut x = start;
    let mut log_term =
        log_choose_fast(red, x) + log_choose_fast(black, n - x) - log_choose_fast(total, n);
    let mut sum = log_term.exp();
    while sum < target && x < end {
        // P(X = x + 1) / P(X = x) = (red - x)(n - x) / ((x + 1)(black - n + x + 1)).
        log_term += ((red - x) / (x + 1.0)).ln() + ((n - x) / (black - n + x + 1.0)).ln();
        sum += log_term.exp();
        x += 1.0;
    }
    x
}

/// `log C(n, k)` for nonnegative integers, through `lbeta`.
fn log_choose_fast(n: f64, k: f64) -> f64 {
    -(n + 1.0).ln() - lbeta(n - k + 1.0, k + 1.0)
}

/// `C(n, k)` for real `n` and integer `k`, exact while the product is.
pub fn choose(n: f64, k: f64) -> f64 {
    let k = force_int(k);
    if n.is_nan() || k.is_nan() {
        return n + k;
    }
    if k < 30.0 {
        if n - k < k && n >= 0.0 && n == force_int(n) {
            return choose(n, force_int(n - k));
        }
        if k < 0.0 {
            return 0.0;
        }
        if k == 0.0 {
            return 1.0;
        }
        let mut result = n;
        let mut j = 2.0;
        while j <= k {
            result *= (n - j + 1.0) / j;
            j += 1.0;
        }
        return if n == force_int(n) {
            force_int(result)
        } else {
            result
        };
    }
    if n < 0.0 {
        let result = choose(-n + k - 1.0, k);
        return if k % 2.0 == 1.0 { -result } else { result };
    }
    if n == force_int(n) {
        let n = force_int(n);
        if n < k {
            return 0.0;
        }
        if n - k < 30.0 {
            return choose(n, n - k);
        }
        return force_int(log_choose_fast(n, k).exp());
    }
    if n < k - 1.0 {
        // Γ(n - k + 1) changes sign with every integer below zero.
        let sign = if (n - k + 1.0).floor() % 2.0 == 0.0 {
            1.0
        } else {
            -1.0
        };
        let value = lgammafn(n + 1.0) - lgammafn(k + 1.0) - lgammafn(n - k + 1.0);
        return sign * value.exp();
    }
    log_choose_fast(n, k).exp()
}

// The Wilcoxon signed rank and rank sum distributions, by counting.

/// The number of subsets of `1..=n` with each sum `0..=n(n+1)/2`.
fn signed_rank_counts(n: usize) -> Vec<f64> {
    let total = n * (n + 1) / 2;
    let mut counts = vec![0.0; total + 1];
    counts[0] = 1.0;
    let mut reach = 0;
    for value in 1..=n {
        reach += value;
        for sum in (value..=reach).rev() {
            counts[sum] += counts[sum - value];
        }
    }
    counts
}

pub fn dsignrank(x: f64, n: f64, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    let n = force_int(n);
    if n <= 0.0 {
        return f64::NAN;
    }
    if (x - force_int(x)).abs() > 1e-7 {
        return d_zero(log_p);
    }
    let x = force_int(x);
    if x < 0.0 || x > n * (n + 1.0) / 2.0 {
        return d_zero(log_p);
    }
    let counts = signed_rank_counts(n as usize);
    d_exp(counts[x as usize].ln() - n * LN_2, log_p)
}

pub fn psignrank(x: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if !n.is_finite() {
        return f64::NAN;
    }
    let n = force_int(n);
    if n <= 0.0 {
        return f64::NAN;
    }
    let x = force_int(x + 1e-7);
    let total = n * (n + 1.0) / 2.0;
    if x < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= total {
        return dt_one(lower_tail, log_p);
    }
    let counts = signed_rank_counts(n as usize);
    let scale = (-n * LN_2).exp();
    // The distribution is symmetric, so the shorter tail is summed.
    let (limit, lower_tail) = if x <= total / 2.0 {
        (x, lower_tail)
    } else {
        (total - x - 1.0, !lower_tail)
    };
    let probability: f64 = counts[..=limit as usize]
        .iter()
        .map(|count| count * scale)
        .sum();
    dt_val(probability, lower_tail, log_p)
}

pub fn qsignrank(x: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || n.is_nan() {
        return x + n;
    }
    if !x.is_finite() || !n.is_finite() || p_invalid(x, log_p) {
        return f64::NAN;
    }
    let n = force_int(n);
    if n <= 0.0 {
        return f64::NAN;
    }
    let total = n * (n + 1.0) / 2.0;
    if let Some(edge) = quantile_boundaries(x, 0.0, total, lower_tail, log_p) {
        return edge;
    }
    let probability = dt_qiv(x, lower_tail, log_p);
    let counts = signed_rank_counts(n as usize);
    let scale = (-n * LN_2).exp();
    discrete_count_quantile(probability, &counts, scale, total)
}

/// The smallest count whose cumulative probability reaches `probability`, summing the shorter
/// tail of a symmetric distribution given by `counts * scale`.
fn discrete_count_quantile(probability: f64, counts: &[f64], scale: f64, total: f64) -> f64 {
    let mut cumulative = 0.0;
    let mut q = 0usize;
    if probability <= 0.5 {
        let target = probability - 10.0 * f64::EPSILON;
        loop {
            cumulative += counts.get(q).copied().unwrap_or(0.0) * scale;
            if cumulative >= target || q as f64 >= total {
                return q as f64;
            }
            q += 1;
        }
    }
    let target = 1.0 - probability + 10.0 * f64::EPSILON;
    loop {
        cumulative += counts.get(q).copied().unwrap_or(0.0) * scale;
        if cumulative > target || q as f64 >= total {
            return total - q as f64;
        }
        q += 1;
    }
}

/// The number of ways the rank sum statistic of samples of sizes `m` and `n` takes each value
/// `0..=mn`: the coefficients of the Gaussian binomial `[m + n, m]_q`, built as the product of
/// `(1 - q^(n + i)) / (1 - q^i)` for `i = 1..=m`.
fn rank_sum_counts(m: usize, n: usize) -> Vec<f64> {
    let total = m * n;
    let mut counts = vec![0.0; total + 1];
    counts[0] = 1.0;
    let mut degree = 0;
    for i in 1..=m {
        let shift = n + i;
        degree += n;
        for k in (shift..=degree.min(total)).rev() {
            counts[k] -= counts[k - shift];
        }
        for k in i..=degree.min(total) {
            counts[k] += counts[k - i];
        }
    }
    counts
}

pub fn dwilcox(x: f64, m: f64, n: f64, log_p: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    let (m, n) = (force_int(m), force_int(n));
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    if (x - force_int(x)).abs() > 1e-7 {
        return d_zero(log_p);
    }
    let x = force_int(x);
    if x < 0.0 || x > m * n {
        return d_zero(log_p);
    }
    let counts = rank_sum_counts(m as usize, n as usize);
    let count = counts[x as usize];
    if log_p {
        count.ln() - log_choose(m + n, n)
    } else {
        count / choose(m + n, n)
    }
}

fn log_choose(n: f64, k: f64) -> f64 {
    log_choose_fast(n, force_int(k))
}

pub fn pwilcox(q: f64, m: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if q.is_nan() || m.is_nan() || n.is_nan() {
        return q + m + n;
    }
    if !m.is_finite() || !n.is_finite() {
        return f64::NAN;
    }
    let (m, n) = (force_int(m), force_int(n));
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    let q = (q + 1e-7).floor();
    if q < 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if q >= m * n {
        return dt_one(lower_tail, log_p);
    }
    let counts = rank_sum_counts(m as usize, n as usize);
    let combinations = choose(m + n, n);
    let (limit, lower_tail) = if q <= m * n / 2.0 {
        (q as usize + 1, lower_tail)
    } else {
        ((m * n - q) as usize, !lower_tail)
    };
    let probability: f64 = counts[..limit]
        .iter()
        .map(|count| count / combinations)
        .sum();
    dt_val(probability, lower_tail, log_p)
}

pub fn qwilcox(x: f64, m: f64, n: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || m.is_nan() || n.is_nan() {
        return x + m + n;
    }
    if !x.is_finite() || !m.is_finite() || !n.is_finite() || p_invalid(x, log_p) {
        return f64::NAN;
    }
    let (m, n) = (force_int(m), force_int(n));
    if m <= 0.0 || n <= 0.0 {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(x, 0.0, m * n, lower_tail, log_p) {
        return edge;
    }
    let probability = dt_qiv(x, lower_tail, log_p);
    let counts = rank_sum_counts(m as usize, n as usize);
    discrete_count_quantile(probability, &counts, 1.0 / choose(m + n, n), m * n)
}

// The noncentral chi-squared, beta, F and t distributions. The first three are Poisson
// mixtures of their central counterparts; the sums run outwards from the largest term, which
// the terms' unimodality in the mixture index guarantees.

/// The sum over `i >= 0` of `exp(log_term(i))`, starting from `start`, on the log scale. The
/// terms must be unimodal in `i`.
fn log_mixture_sum(start: f64, log_term: impl Fn(f64) -> f64) -> f64 {
    let mut peak = start.max(0.0).floor();
    let mut peak_value = log_term(peak);
    let ascending = log_term(peak + 1.0) > peak_value;
    let direction = if ascending { 1.0 } else { -1.0 };
    loop {
        let next = peak + direction;
        if next < 0.0 {
            break;
        }
        let value = log_term(next);
        if value.is_nan() || value <= peak_value {
            break;
        }
        peak = next;
        peak_value = value;
    }
    if peak_value == f64::NEG_INFINITY || peak_value.is_nan() {
        return peak_value;
    }
    let mut sum = 1.0;
    for direction in [1.0, -1.0] {
        let mut i = peak + direction;
        while i >= 0.0 {
            let relative = (log_term(i) - peak_value).exp();
            sum += relative;
            if relative <= 1e-17 * sum || (i - peak).abs() > 1e7 {
                break;
            }
            i += direction;
        }
    }
    peak_value + sum.ln()
}

pub fn dnchisq(x: f64, df: f64, ncp: f64, log_p: bool) -> f64 {
    if x.is_nan() || df.is_nan() || ncp.is_nan() {
        return x + df + ncp;
    }
    if !df.is_finite() || !ncp.is_finite() || ncp < 0.0 || df < 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d_zero(log_p);
    }
    if x == 0.0 && df < 2.0 {
        return f64::INFINITY;
    }
    if ncp == 0.0 {
        return if df > 0.0 {
            dchisq(x, df, log_p)
        } else {
            d_zero(log_p)
        };
    }
    if x == f64::INFINITY {
        return d_zero(log_p);
    }
    let half = ncp / 2.0;
    let log_density = log_mixture_sum(half, |i| {
        dpois_raw(i, half, true) + dchisq(x, df + 2.0 * i, true)
    });
    d_exp(log_density, log_p)
}

pub fn pnchisq(x: f64, df: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || df.is_nan() || ncp.is_nan() {
        return x + df + ncp;
    }
    if !df.is_finite() || !ncp.is_finite() || df < 0.0 || ncp < 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= f64::INFINITY {
        return dt_one(lower_tail, log_p);
    }
    if ncp == 0.0 {
        return pchisq(x, df, lower_tail, log_p);
    }
    let half = ncp / 2.0;
    // The tail that is smaller is summed, and the other is its complement.
    let lower_small = x < df + ncp;
    let log_tail = log_mixture_sum(half, |i| {
        let weight = dpois_raw(i, half, true);
        if df + 2.0 * i == 0.0 {
            // A chi-squared with no degrees of freedom is a point mass at zero.
            return if lower_small {
                weight
            } else {
                f64::NEG_INFINITY
            };
        }
        weight + pchisq(x, df + 2.0 * i, lower_small, true)
    });
    let log_tail = log_tail.min(0.0);
    if lower_small == lower_tail {
        d_exp(log_tail, log_p)
    } else if log_p {
        log1mexp(log_tail)
    } else {
        -log_tail.exp_m1()
    }
}

pub fn qnchisq(p: f64, df: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || df.is_nan() || ncp.is_nan() {
        return p + df + ncp;
    }
    if !df.is_finite() || df < 0.0 || ncp < 0.0 {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    if ncp == 0.0 {
        return qchisq(p, df, lower_tail, log_p);
    }
    // Pearson's approximation by a scaled central chi-squared starts the iteration.
    let b = ncp * ncp / (df + 3.0 * ncp);
    let c = (df + 3.0 * ncp) / (df + 2.0 * ncp);
    let f = (df + 2.0 * ncp) / (c * c);
    let start = (b + c * qchisq(p, f, lower_tail, log_p)).max(f64::MIN_POSITIVE);
    continuous_quantile(
        p,
        lower_tail,
        log_p,
        start.ln(),
        f64::NEG_INFINITY,
        f64::INFINITY,
        |x, lower| (pnchisq(x, df, ncp, lower, true), dnchisq(x, df, ncp, true)),
    )
}

/// The quantile of a continuous distribution on `(0, ∞)` or a part of it, from its log
/// distribution function on either tail and its log density (`function(x, lower)`), starting
/// from `log_start` with `log x` bracketed by `[log_low, log_high]`. The smaller tail is
/// matched on the log scale, first in `log x` and then in `x`.
fn continuous_quantile(
    p: f64,
    lower_tail: bool,
    log_p: bool,
    log_start: f64,
    log_low: f64,
    log_high: f64,
    function: impl Fn(f64, bool) -> (f64, f64),
) -> f64 {
    let lower_small = dt_qiv(p, lower_tail, log_p) <= 0.5;
    let target = if lower_small {
        dt_log(p, lower_tail, log_p)
    } else {
        dt_clog(p, lower_tail, log_p)
    };
    let sign = if lower_small { 1.0 } else { -1.0 };
    let evaluate = |x: f64| {
        let (log_tail, log_density) = function(x, lower_small);
        (sign * (log_tail - target), (log_density - log_tail).exp())
    };
    let log_root = increasing_root(log_start.clamp(log_low, log_high), log_low, log_high, |u| {
        let (value, slope) = evaluate(u.exp());
        (value, slope * u.exp())
    });
    increasing_root(log_root.exp(), log_low.exp(), log_high.exp(), evaluate)
}

pub fn dnbeta(x: f64, a: f64, b: f64, ncp: f64, log_p: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() || ncp.is_nan() {
        return x + a + b + ncp;
    }
    if ncp < 0.0 || a <= 0.0 || b <= 0.0 || !a.is_finite() || !b.is_finite() || !ncp.is_finite() {
        return f64::NAN;
    }
    if !(0.0..=1.0).contains(&x) {
        return d_zero(log_p);
    }
    if ncp == 0.0 {
        return dbeta(x, a, b, log_p);
    }
    let half = ncp / 2.0;
    let log_density = log_mixture_sum(half, |i| {
        dpois_raw(i, half, true) + dbeta(x, a + i, b, true)
    });
    d_exp(log_density, log_p)
}

pub fn pnbeta(x: f64, a: f64, b: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || a.is_nan() || b.is_nan() || ncp.is_nan() {
        return x + a + b + ncp;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= 1.0 {
        return dt_one(lower_tail, log_p);
    }
    noncentral_beta(x, 0.5 - x + 0.5, a, b, ncp, lower_tail, log_p)
}

/// The noncentral beta distribution function with `y = 1 - x` given separately: Lenth's (1987)
/// AS 226 with Frick's (1990) R84 start a few standard deviations below the Poisson mean, which
/// sums the Poisson mixture of incomplete beta functions upwards by their recurrence until the
/// remaining Poisson mass bounds the error by 1e-9.
fn noncentral_beta(x: f64, y: f64, a: f64, b: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if ncp < 0.0 || a <= 0.0 || b <= 0.0 || !ncp.is_finite() || !a.is_finite() || !b.is_finite() {
        return f64::NAN;
    }
    let lower = noncentral_beta_lower(x, y, a, b, ncp);
    if lower_tail {
        return if log_p { lower.ln() } else { lower };
    }
    if lower > 1.0 - 1e-10 {
        warn("full precision may not have been achieved in 'pnbeta'".into());
    }
    let lower = lower.min(1.0);
    if log_p { (-lower).ln_1p() } else { 1.0 - lower }
}

fn noncentral_beta_lower(x: f64, y: f64, a: f64, b: f64, ncp: f64) -> f64 {
    const ERROR_LIMIT: f64 = 1e-9;
    const ITERATIONS: f64 = 10000.0;
    if x < 0.0 || y > 1.0 || (x == 0.0 && y == 1.0) {
        return 0.0;
    }
    if x > 1.0 || y < 0.0 || (x == 1.0 && y == 0.0) {
        return 1.0;
    }
    let c = ncp / 2.0;
    let start = (c - 7.0 * c.sqrt()).max(0.0).floor();
    let a0 = a + start;
    let log_beta = lgammafn(a0) + lgammafn(b) - lgammafn(a0 + b);
    let mut beta_value = incomplete_beta(x, y, a0, b, true, false);
    let mut step =
        (a0 * x.ln() + b * if x < 0.5 { (-x).ln_1p() } else { y.ln() } - log_beta - a0.ln()).exp();
    let mut weight = if a0 > a {
        (-c + start * c.ln() - lgammafn(start + 1.0)).exp()
    } else {
        (-c).exp()
    };
    let mut remaining = 1.0 - weight;
    let mut sum = weight * beta_value;
    let mut j = start;
    loop {
        j += 1.0;
        beta_value -= step;
        step *= x * (a + b + j - 1.0) / (a + j);
        weight *= c / j;
        remaining -= weight;
        sum += beta_value * weight;
        let bound = (beta_value - step) * remaining;
        if bound <= ERROR_LIMIT || j >= ITERATIONS + start {
            if bound > ERROR_LIMIT {
                warn("full precision may not have been achieved in 'pnbeta'".into());
            }
            if j >= ITERATIONS + start {
                warn("convergence failed in 'pnbeta'".into());
            }
            return sum;
        }
    }
}

pub fn qnbeta(p: f64, a: f64, b: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || a.is_nan() || b.is_nan() || ncp.is_nan() {
        return p + a + b + ncp;
    }
    if ncp < 0.0 || a <= 0.0 || b <= 0.0 || !ncp.is_finite() {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, 1.0, lower_tail, log_p) {
        return edge;
    }
    let half = ncp / 2.0;
    let start = ((a + half) / (a + b + half)).ln();
    continuous_quantile(
        p,
        lower_tail,
        log_p,
        start,
        f64::NEG_INFINITY,
        0.0,
        |x, lower| {
            (
                pnbeta(x, a, b, ncp, lower, true),
                dnbeta(x, a, b, ncp, true),
            )
        },
    )
}

pub fn dnf(x: f64, df1: f64, df2: f64, ncp: f64, log_p: bool) -> f64 {
    if x.is_nan() || df1.is_nan() || df2.is_nan() || ncp.is_nan() {
        return x + df1 + df2 + ncp;
    }
    if df1 <= 0.0 || df2 <= 0.0 || ncp < 0.0 {
        return f64::NAN;
    }
    if x < 0.0 {
        return d_zero(log_p);
    }
    if !ncp.is_finite() {
        return f64::NAN;
    }
    if !df1.is_finite() && !df2.is_finite() {
        return if x == 1.0 {
            f64::INFINITY
        } else {
            d_zero(log_p)
        };
    }
    if !df2.is_finite() {
        let density = dnchisq(x * df1, df1, ncp, log_p);
        return if log_p {
            df1.ln() + density
        } else {
            df1 * density
        };
    }
    if df1 > 1e14 && ncp < 1e7 {
        // A huge df1 makes the numerator a constant 1 + ncp/df1.
        let f = 1.0 + ncp / df1;
        let density = dgamma(1.0 / x / f, df2 / 2.0, 2.0 / df2, log_p);
        return if log_p {
            density - 2.0 * x.ln() - f.ln()
        } else {
            density / (x * x) / f
        };
    }
    let y = (df1 / df2) * x;
    let density = dnbeta(y / (1.0 + y), df1 / 2.0, df2 / 2.0, ncp, log_p);
    if log_p {
        density + df1.ln() - df2.ln() - 2.0 * y.ln_1p()
    } else {
        density * (df1 / df2) / (1.0 + y) / (1.0 + y)
    }
}

pub fn pnf(x: f64, df1: f64, df2: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if x.is_nan() || df1.is_nan() || df2.is_nan() || ncp.is_nan() {
        return x + df1 + df2 + ncp;
    }
    if df1 <= 0.0
        || df2 <= 0.0
        || ncp < 0.0
        || !ncp.is_finite()
        || (!df1.is_finite() && !df2.is_finite())
    {
        return f64::NAN;
    }
    if x <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if x >= f64::INFINITY {
        return dt_one(lower_tail, log_p);
    }
    if df2 > 1e8 {
        return pnchisq(x * df1, df1, ncp, lower_tail, log_p);
    }
    let y = (df1 / df2) * x;
    noncentral_beta(
        y / (1.0 + y),
        1.0 / (1.0 + y),
        df1 / 2.0,
        df2 / 2.0,
        ncp,
        lower_tail,
        log_p,
    )
}

pub fn qnf(p: f64, df1: f64, df2: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || df1.is_nan() || df2.is_nan() || ncp.is_nan() {
        return p + df1 + df2 + ncp;
    }
    if df1 <= 0.0
        || df2 <= 0.0
        || ncp < 0.0
        || !ncp.is_finite()
        || (!df1.is_finite() && !df2.is_finite())
    {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    if df2 > 1e8 {
        return qnchisq(p, df1, ncp, lower_tail, log_p) / df1;
    }
    let y = qnbeta(p, df1 / 2.0, df2 / 2.0, ncp, lower_tail, log_p);
    y / (1.0 - y) * (df2 / df1)
}

pub fn dnt(x: f64, df: f64, ncp: f64, log_p: bool) -> f64 {
    if x.is_nan() || df.is_nan() || ncp.is_nan() {
        return x + df + ncp;
    }
    if df <= 0.0 {
        return f64::NAN;
    }
    if ncp == 0.0 {
        return dt(x, df, log_p);
    }
    if !x.is_finite() {
        return d_zero(log_p);
    }
    if !df.is_finite() || df > 1e8 {
        return dnorm(x, ncp, 1.0, log_p);
    }
    // The density in terms of the distribution functions (Johnson, Kotz and Balakrishnan),
    // with its limit at zero.
    let log_density = if x.abs() > (df * f64::EPSILON).sqrt() {
        df.ln() - x.abs().ln()
            + (pnt(x * ((df + 2.0) / df).sqrt(), df + 2.0, ncp, true, false)
                - pnt(x, df, ncp, true, false))
            .abs()
            .ln()
    } else {
        lgammafn((df + 1.0) / 2.0)
            - lgammafn(df / 2.0)
            - (0.5 * PI.ln() + 0.5 * (df.ln() + ncp * ncp))
    };
    d_exp(log_density, log_p)
}

/// The noncentral t distribution function by Lenth's (1989) AS 243: for `t >= 0`, `Φ(-δ)` plus
/// a series of incomplete beta functions weighted by Poisson probabilities of `δ²/2`, summed
/// upwards with the functions' recurrences until the remaining Poisson mass bounds the error by
/// 1e-12. A negative `t` reflects to `-t` with `-δ`.
pub fn pnt(t: f64, df: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if t.is_nan() || df.is_nan() || ncp.is_nan() {
        return t + df + ncp;
    }
    if df <= 0.0 {
        return f64::NAN;
    }
    if ncp == 0.0 {
        return pt(t, df, lower_tail, log_p);
    }
    if !t.is_finite() {
        return if t < 0.0 {
            dt_zero(lower_tail, log_p)
        } else {
            dt_one(lower_tail, log_p)
        };
    }
    let (negative, magnitude, delta) = if t >= 0.0 {
        (false, t, ncp)
    } else {
        // The lower tail beyond a large negative t is out of reach of the series.
        if ncp > 40.0 && (!log_p || !lower_tail) {
            return dt_zero(lower_tail, log_p);
        }
        (true, -t, -ncp)
    };
    if df > 4e5 || delta * delta > 2.0 * LN_2 * -f64::from(f64::MIN_EXP) {
        // Abramowitz and Stegun 26.7.10.
        let s = 1.0 / (4.0 * df);
        return pnorm(
            magnitude * (1.0 - s),
            delta,
            (1.0 + magnitude * magnitude * 2.0 * s).sqrt(),
            lower_tail != negative,
            log_p,
        );
    }
    let square = magnitude * magnitude;
    let x = square / (square + df);
    let mut total = 0.0;
    if x > 0.0 {
        const ERROR_LIMIT: f64 = 1e-12;
        const SQRT_2_OVER_PI: f64 = 0.797_884_560_802_865_355_879_892_119_869;
        const LN_SQRT_PI: f64 = 0.572_364_942_924_700_087_071_713_675_677;
        let lambda = delta * delta;
        let mut p = 0.5 * (-0.5 * lambda).exp();
        if p == 0.0 {
            warn("underflow occurred in 'pnt'".into());
            warn("value out of range in 'pnt'".into());
            return dt_zero(lower_tail, log_p);
        }
        let mut q = SQRT_2_OVER_PI * p * delta;
        // The Poisson mass not yet summed, which bounds the truncation error.
        let mut remaining = 0.5 - p;
        if remaining < 1e-7 {
            remaining = -0.5 * (-0.5 * lambda).exp_m1();
        }
        let mut a = 0.5;
        let b = 0.5 * df;
        let complement_power = (df / (square + df)).powf(b);
        let log_beta = LN_SQRT_PI + lgammafn(b) - lgammafn(0.5 + b);
        // I_x(j + 1/2, b) and I_x(j + 1, b), stepped down by their recurrences.
        let mut odd = pbeta(x, a, b, true, false);
        let mut odd_step = 2.0 * complement_power * (a * x.ln() - log_beta).exp();
        let bx = b * x;
        let mut even = if bx < f64::EPSILON {
            bx
        } else {
            1.0 - complement_power
        };
        let mut even_step = bx * complement_power;
        total = p * odd + q * even;
        let mut converged = false;
        for iteration in 1..=1000 {
            let it = f64::from(iteration);
            a += 1.0;
            odd -= odd_step;
            even -= even_step;
            odd_step *= x * (a + b - 1.0) / a;
            even_step *= x * (a + b - 0.5) / (a + 0.5);
            p *= lambda / (2.0 * it);
            q *= lambda / (2.0 * it + 1.0);
            total += p * odd + q * even;
            remaining -= p;
            if remaining < -1e-10 {
                warn("full precision may not have been achieved in 'pnt'".into());
                converged = true;
                break;
            }
            if remaining <= 0.0 && iteration > 1 {
                converged = true;
                break;
            }
            let bound = 2.0 * remaining * (odd - odd_step);
            if bound.abs() < ERROR_LIMIT {
                converged = true;
                break;
            }
        }
        if !converged {
            warn("convergence failed in 'pnt'".into());
        }
    }
    let mut value = total + pnorm(-delta, 0.0, 1.0, true, false);
    let lower = lower_tail != negative;
    if value > 1.0 - 1e-10 && lower {
        warn("full precision may not have been achieved in 'pnt{final}'".into());
    }
    value = value.min(1.0);
    dt_val(value, lower, log_p)
}

pub fn qnt(p: f64, df: f64, ncp: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || df.is_nan() || ncp.is_nan() {
        return p + df + ncp;
    }
    if df <= 0.0 {
        return f64::NAN;
    }
    if ncp == 0.0 && df >= 1.0 {
        return qt(p, df, lower_tail, log_p);
    }
    if let Some(edge) = quantile_boundaries(p, f64::NEG_INFINITY, f64::INFINITY, lower_tail, log_p)
    {
        return edge;
    }
    if !df.is_finite() {
        return qnorm(p, ncp, 1.0, lower_tail, log_p);
    }
    let probability = dt_qiv(p, lower_tail, log_p);
    if probability > 1.0 - f64::EPSILON {
        return f64::INFINITY;
    }
    // Bracket the root by doubling, then bisect: pnt is accurate on the natural scale only,
    // so Newton's method would gain nothing.
    let mut high = ncp.max(1.0);
    while high < f64::MAX && pnt(high, df, ncp, true, false) < probability {
        high *= 2.0;
    }
    let mut low = (-ncp).min(-1.0);
    while low > -f64::MAX && pnt(low, df, ncp, true, false) > probability {
        low *= 2.0;
    }
    while high - low > low.abs().max(high.abs()) * 1e-15 {
        let middle = 0.5 * (low + high);
        if middle == low || middle == high {
            break;
        }
        if pnt(middle, df, ncp, true, false) > probability {
            high = middle;
        } else {
            low = middle;
        }
    }
    0.5 * (low + high)
}

// The studentized range distribution.

/// The positive nodes and the weights of 12-point Gauss-Legendre quadrature.
#[allow(clippy::excessive_precision)]
const LEGENDRE_12: [(f64, f64); 6] = [
    (
        0.981560634246719250690549090149,
        0.047175336386511827194615961485,
    ),
    (
        0.904117256370474856678465866119,
        0.106939325995318430960254718194,
    ),
    (
        0.769902674194304687036893833213,
        0.160078328543346226334652529543,
    ),
    (
        0.587317954286617447296702418941,
        0.203167426723065921749064455810,
    ),
    (
        0.367831498998180193752691536644,
        0.233492536538354808760849898925,
    ),
    (
        0.125233408511468915472441369464,
        0.249147045813402785000562436043,
    ),
];

/// The positive nodes and the weights of 16-point Gauss-Legendre quadrature.
#[allow(clippy::excessive_precision)]
const LEGENDRE_16: [(f64, f64); 8] = [
    (
        0.989400934991649932596154173450,
        0.027152459411754094851780572456,
    ),
    (
        0.944575023073232576077988415535,
        0.062253523938647892862843836994,
    ),
    (
        0.865631202387831743880467897712,
        0.095158511682492784809925107602,
    ),
    (
        0.755404408355003033895101194847,
        0.124628971255533872052476282192,
    ),
    (
        0.617876244402643748446671764049,
        0.149595988816576732081501730547,
    ),
    (
        0.458016777657227386342419442984,
        0.169156519395002538189312079030,
    ),
    (
        0.281603550779258913230460501460,
        0.182603415044923588866763667969,
    ),
    (
        0.095012509837637440185319335425,
        0.189450610455068496285396723208,
    ),
];

/// The distribution function of the range of `cc` standard normals, raised to the power `rr`,
/// at `w` (Copenhaver and Holland 1988): the probability that all lie within `w` of the
/// smallest, integrated over the smallest by Gauss-Legendre quadrature on a few intervals.
fn tukey_range_probability(w: f64, rr: f64, cc: f64) -> f64 {
    const UPPER: f64 = 8.0;
    const LOG_FLOOR: f64 = -30.0;
    const EXPONENT_LIMIT: f64 = 60.0;
    let half = w * 0.5;
    if half >= UPPER {
        return 1.0;
    }
    // The case where every observation lies in [-w/2, w/2].
    let centered = 2.0 * pnorm(half, 0.0, 1.0, true, false) - 1.0;
    let mut probability = if centered >= 1.0 {
        1.0
    } else {
        centered.powf(cc)
    };
    let intervals = if w > 3.0 { 2.0 } else { 3.0 };
    let width = (UPPER - half) / intervals;
    let mut lower_bound = half;
    let mut integral = 0.0;
    for _ in 0..intervals as usize {
        let upper_bound = lower_bound + width;
        let center = 0.5 * (upper_bound + lower_bound);
        let radius = 0.5 * (upper_bound - lower_bound);
        let mut interval_sum = 0.0;
        let nodes = LEGENDRE_12
            .iter()
            .map(|&(node, weight)| (-node, weight))
            .chain(LEGENDRE_12.iter().rev().copied());
        for (node, weight) in nodes {
            let point = center + radius * node;
            let exponent = point * point;
            if exponent > EXPONENT_LIMIT {
                break;
            }
            let inside = pnorm(point, 0.0, 1.0, true, false) - pnorm(point, w, 1.0, true, false);
            if inside >= (LOG_FLOOR / (cc - 1.0)).exp() {
                interval_sum += weight * (-0.5 * exponent).exp() * inside.powf(cc - 1.0);
            }
        }
        integral += interval_sum * (2.0 * radius) * cc * ONE_OVER_SQRT_2PI;
        lower_bound = upper_bound;
    }
    probability += integral;
    if probability <= (LOG_FLOOR / rr).exp() {
        return 0.0;
    }
    probability = probability.powf(rr);
    probability.min(1.0)
}

/// The studentized range distribution of `cc` means in `rr` ranges with `df` degrees of
/// freedom: the range probability integrated against the density of the scale, `chi(df)/√df`,
/// by 16-point Gauss-Legendre quadrature on successive intervals until they contribute
/// nothing.
pub fn ptukey(q: f64, rr: f64, cc: f64, df: f64, lower_tail: bool, log_p: bool) -> f64 {
    if q.is_nan() || rr.is_nan() || cc.is_nan() || df.is_nan() {
        return f64::NAN;
    }
    if q <= 0.0 {
        return dt_zero(lower_tail, log_p);
    }
    if df < 2.0 || rr < 1.0 || cc < 2.0 {
        return f64::NAN;
    }
    if !q.is_finite() {
        return dt_one(lower_tail, log_p);
    }
    if df > 25000.0 {
        return dt_val(tukey_range_probability(q, rr, cc), lower_tail, log_p);
    }
    let half_df = df * 0.5;
    let quarter_df = df * 0.25;
    let length: f64 = if df <= 100.0 {
        1.0
    } else if df <= 800.0 {
        0.5
    } else if df <= 5000.0 {
        0.25
    } else {
        0.125
    };
    let log_constant = half_df * df.ln() - df * LN_2 - lgammafn(half_df) + length.ln();
    let mut total = 0.0;
    let mut converged = false;
    for interval in 1..=50 {
        let center = (2.0 * f64::from(interval) - 1.0) * length;
        let mut interval_sum = 0.0;
        // The nodes below the center and then those above, each from the outermost in.
        let nodes = LEGENDRE_16
            .iter()
            .map(|&(node, weight)| (-node, weight))
            .chain(LEGENDRE_16.iter().copied());
        for (node, weight) in nodes {
            let point = center + node * length;
            let log_weight = log_constant + (half_df - 1.0) * point.ln() - point * quarter_df;
            if log_weight >= -30.0 {
                let scaled = q * (point * 0.5).sqrt();
                interval_sum += tukey_range_probability(scaled, rr, cc) * weight * log_weight.exp();
            }
        }
        if f64::from(interval) * length >= 1.0 && interval_sum <= 1e-14 {
            converged = true;
            break;
        }
        total += interval_sum;
    }
    if !converged {
        warn("full precision may not have been achieved in 'ptukey'".into());
    }
    dt_val(total.min(1.0), lower_tail, log_p)
}

/// The studentized range quantile by the secant method from the approximation of Lund and
/// Lund's AS 190, to an absolute accuracy of 0.0001.
pub fn qtukey(p: f64, rr: f64, cc: f64, df: f64, lower_tail: bool, log_p: bool) -> f64 {
    if p.is_nan() || rr.is_nan() || cc.is_nan() || df.is_nan() {
        return p + rr + cc + df;
    }
    if df < 2.0 || rr < 1.0 || cc < 2.0 {
        return f64::NAN;
    }
    if let Some(edge) = quantile_boundaries(p, 0.0, f64::INFINITY, lower_tail, log_p) {
        return edge;
    }
    let p = dt_qiv(p, lower_tail, log_p);
    let mut x0 = tukey_quantile_start(p, cc, df);
    let mut value0 = ptukey(x0, rr, cc, df, true, false) - p;
    let mut x1 = if value0 > 0.0 {
        (x0 - 1.0).max(0.0)
    } else {
        x0 + 1.0
    };
    let mut value1 = ptukey(x1, rr, cc, df, true, false) - p;
    let mut answer = 0.0;
    for _ in 1..50 {
        answer = x1 - (value1 * (x1 - x0)) / (value1 - value0);
        value0 = value1;
        x0 = x1;
        if answer < 0.0 {
            answer = 0.0;
        }
        value1 = ptukey(answer, rr, cc, df, true, false) - p;
        x1 = answer;
        if (x1 - x0).abs() < 0.0001 {
            return answer;
        }
    }
    warn("convergence failed in 'qtukey'".into());
    answer
}

/// AS 190's initial approximation to the studentized range quantile.
#[allow(clippy::excessive_precision)]
fn tukey_quantile_start(p: f64, cc: f64, df: f64) -> f64 {
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
    // AS 190 rounds √2 to five digits, and the start must match its iterations.
    #[allow(clippy::approx_constant)]
    const C5: f64 = 1.4142;
    const DF_MAX: f64 = 120.0;
    let tail = 0.5 - 0.5 * p;
    let y = (1.0 / (tail * tail)).ln().sqrt();
    let mut t = y
        + ((((y * P4 + P3) * y + P2) * y + P1) * y + P0)
            / ((((y * Q4 + Q3) * y + Q2) * y + Q1) * y + Q0);
    if df < DF_MAX {
        t += (t * t * t + t) / df / 4.0;
    }
    let mut q = C1 - C2 * t;
    if df < DF_MAX {
        q += -C3 / df + C4 * t / df;
    }
    t * (q * (cc - 1.0).ln() + C5)
}

// Bessel functions of real order. J and Y come from Steed's method with Temme's series for
// small arguments, and I and K from the modified analogues (Temme 1975, 1976): a continued
// fraction gives the ratio of consecutive orders at ν, recurrence carries it down to an order
// μ in [-1/2, 1/2], where a series (x < 2) or a second continued fraction (x >= 2) gives the
// second kind's values, and the Wronskian normalizes the first kind's.

/// The largest order the J and Y algorithms accept.
const BESSEL_ORDER_LIMIT: f64 = 1e7;

pub fn bessel_j(x: f64, nu: f64) -> f64 {
    if x.is_nan() || nu.is_nan() {
        return x + nu;
    }
    if x < 0.0 {
        warn("value out of range in 'bessel_j'".into());
        return f64::NAN;
    }
    let whole = nu.floor();
    if nu < 0.0 {
        // J_{-ν} = cos(νπ) J_ν - sin(νπ) Y_ν, where each term vanishes exactly when its
        // trigonometric factor does.
        let cosine = if nu - whole == 0.5 {
            0.0
        } else {
            bessel_j(x, -nu) * cospi(nu)
        };
        let sine = if nu == whole {
            0.0
        } else {
            bessel_y(x, -nu) * sinpi(nu)
        };
        return cosine + sine;
    }
    if nu > BESSEL_ORDER_LIMIT {
        warn(format!(
            "besselJ(x, nu): nu={} too large for bessel_j() algorithm",
            format_g(nu)
        ));
        return f64::NAN;
    }
    if x > 1e5 {
        warn("value out of range in 'J_bessel'".into());
        return 0.0;
    }
    if x == 0.0 {
        return if nu == 0.0 { 1.0 } else { 0.0 };
    }
    let j = if let Some((j, _)) = hankel_jy(x, nu) {
        j
    } else if x <= 1.0 || x * x < nu + 1.0 {
        bessel_series(x, nu, -1.0)
    } else {
        bessel_jy(x, nu).0
    };
    if j == 0.0 {
        warn(format!(
            "bessel_j({},nu={}): precision lost in result",
            format_g(x),
            format_g(nu)
        ));
    }
    j
}

pub fn bessel_y(x: f64, nu: f64) -> f64 {
    if x.is_nan() || nu.is_nan() {
        return x + nu;
    }
    if x < 0.0 {
        warn("value out of range in 'bessel_y'".into());
        return f64::NAN;
    }
    let whole = nu.floor();
    if nu < 0.0 {
        // Y_{-ν} = sin(νπ) J_ν + cos(νπ) Y_ν.
        let cosine = if nu - whole == 0.5 {
            0.0
        } else {
            bessel_y(x, -nu) * cospi(nu)
        };
        let sine = if nu == whole {
            0.0
        } else {
            bessel_j(x, -nu) * sinpi(nu)
        };
        return cosine - sine;
    }
    if nu > BESSEL_ORDER_LIMIT {
        warn(format!(
            "besselY(x, nu): nu={} too large for bessel_y() algorithm",
            format_g(nu)
        ));
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x > 1e8 {
        return 0.0;
    }
    let y = match hankel_jy(x, nu) {
        Some((_, y)) => y,
        None => bessel_jy(x, nu).1,
    };
    if y == f64::NEG_INFINITY {
        warn(format!(
            "bessel_y({},nu={}): precision lost in result",
            format_g(x),
            format_g(nu)
        ));
    }
    y
}

pub fn bessel_i(x: f64, nu: f64, expon_scaled: bool) -> f64 {
    if x.is_nan() || nu.is_nan() {
        return x + nu;
    }
    if x < 0.0 {
        warn("value out of range in 'bessel_i'".into());
        return f64::NAN;
    }
    if nu < 0.0 {
        // I_{-ν} = I_ν + (2/π) sin(νπ) K_ν.
        let reflected = bessel_i(x, -nu, expon_scaled);
        if nu == nu.floor() {
            return reflected;
        }
        let k = bessel_k(x, -nu, expon_scaled);
        let factor = if expon_scaled {
            2.0 * (-2.0 * x).exp()
        } else {
            2.0
        };
        return reflected + k * factor / PI * sinpi(-nu);
    }
    if x == 0.0 {
        return if nu == 0.0 { 1.0 } else { 0.0 };
    }
    if expon_scaled && x > 1e5 {
        return 0.0;
    }
    if !expon_scaled && x > 709.0 {
        return f64::INFINITY;
    }
    let i = if let Some((i, _)) = hankel_ik(x, nu) {
        i
    } else if x <= 2.0 {
        bessel_series(x, nu, 1.0) * (-x).exp()
    } else {
        bessel_ik(x, nu).0
    };
    let value = if expon_scaled { i } else { i * x.exp() };
    if value == 0.0 {
        warn(format!(
            "bessel_i({},nu={}): precision lost in result",
            format_g(x),
            format_g(nu)
        ));
    }
    value
}

pub fn bessel_k(x: f64, nu: f64, expon_scaled: bool) -> f64 {
    if x.is_nan() || nu.is_nan() {
        return x + nu;
    }
    if x < 0.0 {
        warn("value out of range in 'bessel_k'".into());
        return f64::NAN;
    }
    let nu = nu.abs();
    if x == 0.0 {
        return f64::INFINITY;
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    let k = match hankel_ik(x, nu) {
        Some((_, k)) => k,
        None => bessel_ik(x, nu).1,
    };
    if expon_scaled { k } else { k * (-x).exp() }
}

/// The power series `(x/2)^ν Σ (sign x²/4)^k / (k! Γ(ν + k + 1))`: `J_ν` for `sign = -1`, `I_ν`
/// for `sign = 1`.
fn bessel_series(x: f64, nu: f64, sign: f64) -> f64 {
    let half = 0.5 * x;
    let leading = if nu + 1.0 < 170.0 {
        half.powf(nu) / gammafn(nu + 1.0)
    } else {
        (nu * half.ln() - lgammafn(nu + 1.0)).exp()
    };
    let quarter = sign * half * half;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    while k < 500.0 {
        term *= quarter / (k * (nu + k));
        sum += term;
        if term.abs() <= f64::EPSILON * 0.5 * sum.abs() {
            break;
        }
        k += 1.0;
    }
    leading * sum
}

/// Hankel's asymptotic expansions of `J_ν(x)` and `Y_ν(x)` for large `x`, or `None` where they
/// would not reach full precision. The phase `x - (ν/2 + 1/4)π` is never formed, since its
/// rounding would cost the precision of `x`; its sine and cosine come from those of `x`.
fn hankel_jy(x: f64, nu: f64) -> Option<(f64, f64)> {
    if x < 30.0 || x < nu * nu {
        return None;
    }
    let (p, q) = hankel_series(x, nu)?;
    let shift = nu / 2.0 + 0.25;
    let (cos_shift, sin_shift) = (cospi(shift), sinpi(shift));
    let (sin_x, cos_x) = x.sin_cos();
    let cos_phase = cos_x * cos_shift + sin_x * sin_shift;
    let sin_phase = sin_x * cos_shift - cos_x * sin_shift;
    let scale = (2.0 / (PI * x)).sqrt();
    Some((
        scale * (p * cos_phase - q * sin_phase),
        scale * (p * sin_phase + q * cos_phase),
    ))
}

/// The asymptotic expansions of `e^-x I_ν(x)` and `e^x K_ν(x)` for large `x`.
fn hankel_ik(x: f64, nu: f64) -> Option<(f64, f64)> {
    if x < 30.0 || x < nu * nu {
        return None;
    }
    let (alternating, positive) = modified_hankel_series(x, nu)?;
    Some((
        alternating / (TAU * x).sqrt(),
        positive * (PI / (2.0 * x)).sqrt(),
    ))
}

/// Hankel's `P` and `Q`: the even and odd terms of `Π (4ν² - (2j - 1)²) / (k! (8x)^k)`, with
/// alternating signs within each.
fn hankel_series(x: f64, nu: f64) -> Option<(f64, f64)> {
    let mu = 4.0 * nu * nu;
    let mut term = 1.0;
    let mut p = 1.0;
    let mut q = 0.0;
    let mut previous = f64::INFINITY;
    for k in 1..200 {
        let kf = f64::from(k);
        term *= (mu - (2.0 * kf - 1.0) * (2.0 * kf - 1.0)) / (kf * 8.0 * x);
        if term.abs() > previous {
            return None;
        }
        previous = term.abs();
        // Even k carry (-1)^(k/2), odd k carry (-1)^((k-1)/2).
        let signed = if (k / 2) % 2 == 0 { term } else { -term };
        if k % 2 == 0 {
            p += signed
        } else {
            q += signed
        }
        if term.abs() <= f64::EPSILON * 0.25 * p.abs().max(q.abs()) {
            return Some((p, q));
        }
    }
    None
}

/// `Σ (-1)^k a_k` and `Σ a_k` for the modified functions' expansions, with `a_k` as in
/// [`hankel_series`].
fn modified_hankel_series(x: f64, nu: f64) -> Option<(f64, f64)> {
    let mu = 4.0 * nu * nu;
    let mut term = 1.0;
    let mut alternating = 1.0;
    let mut positive = 1.0;
    let mut previous = f64::INFINITY;
    for k in 1..200 {
        let kf = f64::from(k);
        term *= (mu - (2.0 * kf - 1.0) * (2.0 * kf - 1.0)) / (kf * 8.0 * x);
        if term.abs() > previous {
            return None;
        }
        previous = term.abs();
        positive += term;
        alternating += if k % 2 == 0 { term } else { -term };
        if term.abs() <= f64::EPSILON * 0.25 * alternating.abs().min(positive.abs()) {
            return Some((alternating, positive));
        }
    }
    None
}

/// `1/Γ(1 + μ)` and `1/Γ(1 - μ)` with Temme's `γ1 = (1/Γ(1 - μ) - 1/Γ(1 + μ)) / (2μ)` and
/// `γ2 = (1/Γ(1 - μ) + 1/Γ(1 + μ)) / 2`, from the Taylor series of `1/Γ(1 + z)` so that `γ1`
/// keeps its precision as μ goes to zero.
#[allow(clippy::excessive_precision)]
fn temme_gammas(mu: f64) -> (f64, f64, f64, f64) {
    const RECIPROCAL_GAMMA: [f64; 27] = [
        1.0,
        0.5772156649015328606065,
        -0.655878071520253881077,
        -0.042002635034095235529,
        0.1665386113822914895017,
        -0.04219773455554433674821,
        -0.009621971527876973562115,
        0.007218943246663099542395,
        -0.001165167591859065112114,
        -0.0002152416741149509728157,
        0.0001280502823881161861532,
        -0.00002013485478078823865569,
        -0.000001250493482142670657345,
        0.000001133027231981695882374,
        -2.05633841697760710345e-7,
        6.116095104481415817862e-9,
        5.002007644469222930056e-9,
        -1.181274570487020144588e-9,
        1.043426711691100510492e-10,
        7.78226343990507125405e-12,
        -3.696805618642205708188e-12,
        5.100370287454475979015e-13,
        -2.058326053566506783222e-14,
        -5.34812253942301798237e-15,
        1.226778628238260790159e-15,
        -1.181259301697458769514e-16,
        1.18669225475160033258e-18,
    ];
    let mut odd = 0.0;
    let mut even = 0.0;
    for (k, coefficient) in RECIPROCAL_GAMMA.iter().enumerate().rev() {
        if k % 2 == 1 {
            odd = odd * mu * mu + coefficient;
        } else {
            even = even * mu * mu + coefficient;
        }
    }
    // 1/Γ(1 + μ) = even + μ·odd, 1/Γ(1 - μ) = even - μ·odd.
    let gamma1 = -odd;
    let gamma2 = even;
    (gamma1, gamma2, even + mu * odd, even - mu * odd)
}

/// `J_ν(x)` and `Y_ν(x)` for `ν >= 0`, `x > 0`.
fn bessel_jy(x: f64, nu: f64) -> (f64, f64) {
    const EPSILON: f64 = 1e-16;
    const FLOOR: f64 = 1e-300;
    const MAX_ITERATIONS: usize = 10_000_000;
    let steps = if x < 2.0 {
        (nu + 0.5).floor()
    } else {
        (nu - x + 1.5).floor().max(0.0)
    };
    let mu = nu - steps;
    let inverse = 1.0 / x;
    let twice_inverse = 2.0 * inverse;
    let wronskian = twice_inverse / PI;
    // CF1: J'_ν / J_ν, with the sign of J_ν tracked through the denominators.
    let mut sign = 1.0;
    let mut h = (nu * inverse).max(FLOOR);
    let mut b = twice_inverse * nu;
    let mut d = 0.0;
    let mut c = h;
    for _ in 0..MAX_ITERATIONS {
        b += twice_inverse;
        d = b - d;
        if d.abs() < FLOOR {
            d = FLOOR;
        }
        c = b - 1.0 / c;
        if c.abs() < FLOOR {
            c = FLOOR;
        }
        d = 1.0 / d;
        let delta = c * d;
        h *= delta;
        if d < 0.0 {
            sign = -sign;
        }
        if (delta - 1.0).abs() < EPSILON {
            break;
        }
    }
    // Downward recurrence from ν to μ on an arbitrarily scaled J, rescaled before it overflows.
    let mut j_lower = sign * FLOOR;
    let mut j_prime_lower = h * j_lower;
    let mut j_at_nu = j_lower;
    let mut factor = nu * inverse;
    let mut l = steps;
    while l >= 1.0 {
        let next = factor * j_lower + j_prime_lower;
        factor -= inverse;
        j_prime_lower = factor * next - j_lower;
        j_lower = next;
        if j_lower.abs() > 1e250 {
            j_lower *= 1e-250;
            j_prime_lower *= 1e-250;
            j_at_nu *= 1e-250;
        }
        l -= 1.0;
    }
    if j_lower == 0.0 {
        j_lower = EPSILON;
    }
    let f = j_prime_lower / j_lower;
    let (j_mu, y_mu, y_mu_plus_1);
    if x < 2.0 {
        let half = 0.5 * x;
        let pi_mu = PI * mu;
        let fact = if pi_mu.abs() < EPSILON {
            1.0
        } else {
            pi_mu / pi_mu.sin()
        };
        let d = -half.ln();
        let e = mu * d;
        let fact2 = if e.abs() < EPSILON { 1.0 } else { e.sinh() / e };
        let (gamma1, gamma2, gamma_plus, gamma_minus) = temme_gammas(mu);
        let mut ff = 2.0 / PI * fact * (gamma1 * e.cosh() + gamma2 * fact2 * d);
        let e = e.exp();
        let mut p = e / (gamma_plus * PI);
        let mut q = 1.0 / (e * PI * gamma_minus);
        let half_pi_mu = 0.5 * pi_mu;
        let fact3 = if half_pi_mu.abs() < EPSILON {
            1.0
        } else {
            half_pi_mu.sin() / half_pi_mu
        };
        let r = PI * half_pi_mu * fact3 * fact3;
        let mut c = 1.0;
        let d = -half * half;
        let mut sum = ff + r * q;
        let mut sum1 = p;
        let mut i = 1.0;
        loop {
            ff = (i * ff + p + q) / (i * i - mu * mu);
            c *= d / i;
            p /= i - mu;
            q /= i + mu;
            let delta = c * (ff + r * q);
            sum += delta;
            let delta1 = c * p - i * delta;
            sum1 += delta1;
            if delta.abs() < (1.0 + sum.abs()) * EPSILON || i > 1e4 {
                break;
            }
            i += 1.0;
        }
        y_mu = -sum;
        y_mu_plus_1 = -sum1 * twice_inverse;
        let y_mu_prime = mu * inverse * y_mu - y_mu_plus_1;
        j_mu = wronskian / (y_mu_prime - f * y_mu);
    } else {
        // CF2: p + iq = (J'_μ + iY'_μ) / (J_μ + iY_μ), by Steed's method in complex arithmetic.
        let mut a = 0.25 - mu * mu;
        let mut p = -0.5 * inverse;
        let mut q = 1.0;
        let br = 2.0 * x;
        let mut bi = 2.0;
        let mut fact = a * inverse / (p * p + q * q);
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
        let mut i = 2.0;
        while i < 1e7 {
            a += 2.0 * (i - 1.0);
            bi += 2.0;
            dr = a * dr + br;
            di = a * di + bi;
            if dr.abs() + di.abs() < FLOOR {
                dr = FLOOR;
            }
            fact = a / (cr * cr + ci * ci);
            cr = br + cr * fact;
            ci = bi - ci * fact;
            if cr.abs() + ci.abs() < FLOOR {
                cr = FLOOR;
            }
            den = dr * dr + di * di;
            dr /= den;
            di = -di / den;
            dlr = cr * dr - ci * di;
            dli = cr * di + ci * dr;
            temp = p * dlr - q * dli;
            q = p * dli + q * dlr;
            p = temp;
            if (dlr - 1.0).abs() + dli.abs() < EPSILON {
                break;
            }
            i += 1.0;
        }
        let gamma = (p - f) / q;
        let magnitude = (wronskian / ((p - f) * gamma + q)).sqrt();
        j_mu = magnitude.copysign(j_lower);
        y_mu = j_mu * gamma;
        let y_mu_prime = y_mu * (p + q / gamma);
        y_mu_plus_1 = mu * inverse * y_mu - y_mu_prime;
    }
    let j = j_at_nu * (j_mu / j_lower);
    // Upward recurrence for Y, which is stable in that direction; once it overflows, Y_ν is
    // beyond the double range.
    let mut y_current = y_mu;
    let mut y_next = y_mu_plus_1;
    let mut i = 1.0;
    while i <= steps {
        let next = (mu + i) * twice_inverse * y_next - y_current;
        y_current = y_next;
        y_next = next;
        if !y_current.is_finite() || y_current.is_nan() {
            return (j, f64::NEG_INFINITY);
        }
        i += 1.0;
    }
    (j, y_current)
}

/// `e^-x I_ν(x)` and `e^x K_ν(x)` for `ν >= 0`, `x > 0` (the scaled forms, which neither
/// overflow nor underflow for large x).
fn bessel_ik(x: f64, nu: f64) -> (f64, f64) {
    const EPSILON: f64 = 1e-16;
    const FLOOR: f64 = 1e-300;
    let steps = (nu + 0.5).floor();
    let mu = nu - steps;
    let inverse = 1.0 / x;
    let twice_inverse = 2.0 * inverse;
    // CF1: I'_ν / I_ν.
    let mut h = (nu * inverse).max(FLOOR);
    let mut b = twice_inverse * nu;
    let mut d = 0.0;
    let mut c = h;
    for _ in 0..10_000_000 {
        b += twice_inverse;
        d = 1.0 / (b + d);
        c = b + 1.0 / c;
        let delta = c * d;
        h *= delta;
        if (delta - 1.0).abs() < EPSILON {
            break;
        }
    }
    let mut i_lower = FLOOR;
    let mut i_prime_lower = h * i_lower;
    let mut i_at_nu = i_lower;
    let mut factor = nu * inverse;
    let mut l = steps;
    while l >= 1.0 {
        let next = factor * i_lower + i_prime_lower;
        factor -= inverse;
        i_prime_lower = factor * next + i_lower;
        i_lower = next;
        if i_lower.abs() > 1e250 {
            i_lower *= 1e-250;
            i_prime_lower *= 1e-250;
            i_at_nu *= 1e-250;
        }
        l -= 1.0;
    }
    let f = i_prime_lower / i_lower;
    let (k_mu, k_mu_plus_1);
    if x < 2.0 {
        let half = 0.5 * x;
        let pi_mu = PI * mu;
        let fact = if pi_mu.abs() < EPSILON {
            1.0
        } else {
            pi_mu / pi_mu.sin()
        };
        let d = -half.ln();
        let e = mu * d;
        let fact2 = if e.abs() < EPSILON { 1.0 } else { e.sinh() / e };
        let (gamma1, gamma2, gamma_plus, gamma_minus) = temme_gammas(mu);
        let mut ff = fact * (gamma1 * e.cosh() + gamma2 * fact2 * d);
        let mut sum = ff;
        let e = e.exp();
        let mut p = 0.5 * e / gamma_plus;
        let mut q = 0.5 / (e * gamma_minus);
        let mut c = 1.0;
        let d = half * half;
        let mut sum1 = p;
        let mut i = 1.0;
        loop {
            ff = (i * ff + p + q) / (i * i - mu * mu);
            c *= d / i;
            p /= i - mu;
            q /= i + mu;
            let delta = c * ff;
            sum += delta;
            let delta1 = c * (p - i * ff);
            sum1 += delta1;
            if delta.abs() < sum.abs() * EPSILON || i > 1e4 {
                break;
            }
            i += 1.0;
        }
        let scale = x.exp();
        k_mu = sum * scale;
        k_mu_plus_1 = sum1 * twice_inverse * scale;
    } else {
        // Steed's method for Temme's continued fraction of K_μ, which gives e^x K_μ directly.
        let mut b = 2.0 * (1.0 + x);
        let mut d = 1.0 / b;
        let mut h = d;
        let mut delh = d;
        let mut q1 = 0.0;
        let mut q2 = 1.0;
        let a1 = 0.25 - mu * mu;
        let mut q = a1;
        let mut c = a1;
        let mut a = -a1;
        let mut s = 1.0 + q * delh;
        let mut i = 2.0;
        while i < 1e7 {
            a -= 2.0 * (i - 1.0);
            c = -a * c / i;
            let q_new = (q1 - b * q2) / a;
            q1 = q2;
            q2 = q_new;
            q += c * q_new;
            b += 2.0;
            d = 1.0 / (b + a * d);
            delh *= b * d - 1.0;
            h += delh;
            let dels = q * delh;
            s += dels;
            if (dels / s).abs() < EPSILON {
                break;
            }
            i += 1.0;
        }
        h *= a1;
        k_mu = (PI / (2.0 * x)).sqrt() / s;
        k_mu_plus_1 = k_mu * (mu + x + 0.5 - h) * inverse;
    }
    let k_mu_prime = mu * inverse * k_mu - k_mu_plus_1;
    // The Wronskian I K' - I' K = -1/x with the scaled K gives the scaled I.
    let i_mu = inverse / (f * k_mu - k_mu_prime);
    let i = i_mu * i_at_nu / i_lower;
    let mut k_current = k_mu;
    let mut k_next = k_mu_plus_1;
    let mut index = 1.0;
    while index <= steps {
        let next = (mu + index) * twice_inverse * k_next + k_current;
        k_current = k_next;
        k_next = next;
        index += 1.0;
    }
    (i, k_current)
}

// The polygamma functions.

/// `psigamma(x, deriv)`: the `deriv`-th derivative of the digamma function, for `deriv` up to
/// 100. Positive arguments are shifted up by the recurrence `ψ⁽ⁿ⁾(x + 1) = ψ⁽ⁿ⁾(x) +
/// (-1)ⁿ n! / x^(n+1)` until the asymptotic expansion in Bernoulli numbers is accurate;
/// negative ones reflect through `ψ⁽ⁿ⁾(1 - x)` and the n-th derivative of `π cot(πx)`.
pub fn psigamma(x: f64, deriv: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let deriv = force_int(deriv);
    if deriv.is_nan() || deriv < 0.0 {
        return f64::NAN;
    }
    if deriv > 100.0 {
        warn(format!("deriv = {} > 100 (= n_max)\n", deriv as i64));
        return f64::NAN;
    }
    let n = deriv as usize;
    if x == f64::INFINITY {
        return if n == 0 { x } else { 0.0 };
    }
    if x == x.floor() && x <= 0.0 {
        // A pole: ψ⁽ⁿ⁾ ~ (-1)^(n+1) n! / (x - x0)^(n+1), whose sides agree only for odd n.
        return if n % 2 == 1 { f64::INFINITY } else { f64::NAN };
    }
    if x < 0.0 {
        // GNU R's algorithm reflects only up to the fifth derivative.
        if n > 5 {
            return f64::NAN;
        }
        let reflected = polygamma_positive(1.0 - x, n);
        let sign = if n.is_multiple_of(2) { 1.0 } else { -1.0 };
        return sign * reflected - PI.powi(n as i32 + 1) * cot_derivative(PI * x, n);
    }
    let value = polygamma_positive(x, n);
    // An overflowing result is reported as NaN, as GNU R's does.
    if value.is_finite() { value } else { f64::NAN }
}

/// `ψ⁽ⁿ⁾(x)` for `x > 0`.
#[allow(clippy::excessive_precision)]
fn polygamma_positive(x: f64, n: usize) -> f64 {
    const BERNOULLI: [f64; 20] = [
        0.1666666666666666666667,
        -0.03333333333333333333333,
        0.02380952380952380952381,
        -0.03333333333333333333333,
        0.07575757575757575757576,
        -0.2531135531135531135531,
        1.166666666666666666667,
        -7.092156862745098039216,
        54.97117794486215538847,
        -529.1242424242424242424,
        6192.123188405797101449,
        -86580.25311355311355311,
        1425517.166666666666667,
        -27298231.06781609195402,
        601580873.9006423683843,
        -15116315767.09215686275,
        429614643061.1666666667,
        -13711655205088.33277216,
        488332318973593.1666667,
        -19296579341940068.14863,
    ];
    let nf = n as f64;
    let threshold = 20.0 + nf;
    let mut shift_sum = 0.0;
    let mut x = x;
    // Σ 1/(x + k)^(n+1) over the shifts, for n >= 1; Σ 1/(x + k) for n = 0.
    while x < threshold {
        shift_sum += x.powi(-(n as i32) - 1);
        x += 1.0;
    }
    let factorial = gammafn(nf + 1.0);
    if n == 0 {
        let inverse_square = 1.0 / (x * x);
        let mut series = 0.0;
        let mut power = inverse_square;
        for (k, bernoulli) in BERNOULLI.iter().enumerate() {
            let term = bernoulli / (2.0 * (k as f64 + 1.0)) * power;
            series += term;
            if term.abs() < 1e-17 * x.ln().abs().max(1.0) {
                break;
            }
            power *= inverse_square;
        }
        return x.ln() - 0.5 / x - series - shift_sum;
    }
    // (-1)^(n+1) [ (n-1)!/x^n + n!/(2 x^(n+1)) + Σ B_2k (2k+n-1)!/((2k)! x^(2k+n)) ], divided
    // by n! to keep the terms in range.
    let mut series = 1.0 / (nf * x.powi(n as i32)) + 0.5 / x.powi(n as i32 + 1);
    let mut ratio = 1.0 / x.powi(n as i32);
    for (k, bernoulli) in BERNOULLI.iter().enumerate() {
        let two_k = 2.0 * (k as f64 + 1.0);
        // (2k + n - 1)! / ((2k)! n!) x^-(2k+n), built incrementally.
        ratio /= x * x;
        let coefficient = (lgammafn(two_k + nf) - lgammafn(two_k + 1.0) - lgammafn(nf + 1.0)).exp();
        let term = bernoulli * coefficient * ratio;
        series += term;
        if term.abs() < 1e-17 * series.abs() {
            break;
        }
    }
    let sign = if n % 2 == 1 { 1.0 } else { -1.0 };
    sign * factorial * (series + shift_sum)
}

/// The n-th derivative of `cot` at `theta`: `P_n(cot θ)` with `P_0(c) = c` and
/// `P_{n+1}(c) = -(1 + c²) P_n'(c)`.
fn cot_derivative(theta: f64, n: usize) -> f64 {
    let mut coefficients = vec![0.0, 1.0];
    for _ in 0..n {
        let mut next = vec![0.0; coefficients.len() + 1];
        for (power, &coefficient) in coefficients.iter().enumerate().skip(1) {
            let derivative = coefficient * power as f64;
            next[power - 1] -= derivative;
            next[power + 1] -= derivative;
        }
        coefficients = next;
    }
    let c = 1.0 / theta.tan();
    coefficients
        .iter()
        .rev()
        .fold(0.0, |value, coefficient| value * c + coefficient)
}

// Random generation. Each generator consumes the uniform, normal and exponential deviates in
// the order of the published algorithm GNU R implements, so that a seed reproduces its stream.

pub fn rnorm(deviates: &mut impl Deviates, mu: f64, sigma: f64) -> R<f64> {
    if mu.is_nan() || !sigma.is_finite() || sigma < 0.0 {
        return Ok(f64::NAN);
    }
    if sigma == 0.0 || !mu.is_finite() {
        return Ok(mu);
    }
    Ok(mu + sigma * deviates.normal()?)
}

pub fn runif(deviates: &mut impl Deviates, a: f64, b: f64) -> R<f64> {
    if !a.is_finite() || !b.is_finite() || b < a {
        return Ok(f64::NAN);
    }
    if a == b {
        return Ok(a);
    }
    loop {
        let u = deviates.uniform()?;
        if u > 0.0 && u < 1.0 {
            return Ok(a + (b - a) * u);
        }
    }
}

pub fn rexp(deviates: &mut impl Deviates, scale: f64) -> R<f64> {
    if !scale.is_finite() || scale <= 0.0 {
        return Ok(if scale == 0.0 { 0.0 } else { f64::NAN });
    }
    Ok(scale * deviates.exponential()?)
}

pub fn rcauchy(deviates: &mut impl Deviates, location: f64, scale: f64) -> R<f64> {
    if location.is_nan() || !scale.is_finite() || scale < 0.0 {
        return Ok(f64::NAN);
    }
    if scale == 0.0 || !location.is_finite() {
        return Ok(location);
    }
    Ok(location + scale * (PI * deviates.uniform()?).tan())
}

pub fn rlogis(deviates: &mut impl Deviates, location: f64, scale: f64) -> R<f64> {
    if location.is_nan() || !scale.is_finite() {
        return Ok(f64::NAN);
    }
    if scale == 0.0 || !location.is_finite() {
        return Ok(location);
    }
    let u = deviates.uniform()?;
    Ok(location + scale * (u / (1.0 - u)).ln())
}

pub fn rlnorm(deviates: &mut impl Deviates, meanlog: f64, sdlog: f64) -> R<f64> {
    if meanlog.is_nan() || !sdlog.is_finite() || sdlog < 0.0 {
        return Ok(f64::NAN);
    }
    Ok(rnorm(deviates, meanlog, sdlog)?.exp())
}

pub fn rweibull(deviates: &mut impl Deviates, shape: f64, scale: f64) -> R<f64> {
    if !shape.is_finite() || !scale.is_finite() || shape <= 0.0 || scale <= 0.0 {
        return Ok(if scale == 0.0 { 0.0 } else { f64::NAN });
    }
    Ok(scale * (-deviates.uniform()?.ln()).powf(1.0 / shape))
}

pub fn rchisq(deviates: &mut impl Deviates, df: f64) -> R<f64> {
    if !df.is_finite() || df < 0.0 {
        return Ok(f64::NAN);
    }
    rgamma(deviates, df / 2.0, 2.0)
}

pub fn rnchisq(deviates: &mut impl Deviates, df: f64, lambda: f64) -> R<f64> {
    if !df.is_finite() || !lambda.is_finite() || df < 0.0 || lambda < 0.0 {
        return Ok(f64::NAN);
    }
    if lambda == 0.0 {
        return if df == 0.0 {
            Ok(0.0)
        } else {
            rgamma(deviates, df / 2.0, 2.0)
        };
    }
    // A Poisson mixture of central chi-squares with 2K extra degrees of freedom.
    let mut value = rpois(deviates, lambda / 2.0)?;
    if value > 0.0 {
        value = rchisq(deviates, 2.0 * value)?;
    }
    if df > 0.0 {
        value += rgamma(deviates, df / 2.0, 2.0)?;
    }
    Ok(value)
}

pub fn rt(deviates: &mut impl Deviates, df: f64) -> R<f64> {
    if df.is_nan() || df <= 0.0 {
        return Ok(f64::NAN);
    }
    if !df.is_finite() {
        return deviates.normal();
    }
    let numerator = deviates.normal()?;
    Ok(numerator / (rchisq(deviates, df)? / df).sqrt())
}

pub fn rf(deviates: &mut impl Deviates, m: f64, n: f64) -> R<f64> {
    if m.is_nan() || n.is_nan() || m <= 0.0 || n <= 0.0 {
        return Ok(f64::NAN);
    }
    let numerator = if m.is_finite() {
        rchisq(deviates, m)? / m
    } else {
        1.0
    };
    let denominator = if n.is_finite() {
        rchisq(deviates, n)? / n
    } else {
        1.0
    };
    Ok(numerator / denominator)
}

pub fn rgeom(deviates: &mut impl Deviates, p: f64) -> R<f64> {
    if !p.is_finite() || p <= 0.0 || p > 1.0 {
        return Ok(f64::NAN);
    }
    // A Poisson count with an exponentially distributed mean.
    let mean = deviates.exponential()? * ((1.0 - p) / p);
    rpois(deviates, mean)
}

pub fn rnbinom(deviates: &mut impl Deviates, size: f64, prob: f64) -> R<f64> {
    if !prob.is_finite() || size.is_nan() || size <= 0.0 || prob <= 0.0 || prob > 1.0 {
        return Ok(f64::NAN);
    }
    let size = if size.is_finite() {
        size
    } else {
        f64::MAX / 2.0
    };
    if prob == 1.0 {
        return Ok(0.0);
    }
    let mean = rgamma(deviates, size, (1.0 - prob) / prob)?;
    rpois(deviates, mean)
}

pub fn rnbinom_mu(deviates: &mut impl Deviates, size: f64, mu: f64) -> R<f64> {
    if !mu.is_finite() || size.is_nan() || size <= 0.0 || mu < 0.0 {
        return Ok(f64::NAN);
    }
    let size = if size.is_finite() {
        size
    } else {
        f64::MAX / 2.0
    };
    if mu == 0.0 {
        return Ok(0.0);
    }
    let mean = rgamma(deviates, size, mu / size)?;
    rpois(deviates, mean)
}

/// Gamma deviates by Ahrens and Dieter's algorithms GS (1974) for shapes below one and GD
/// (1982) for the others.
#[allow(clippy::excessive_precision)]
pub fn rgamma(deviates: &mut impl Deviates, a: f64, scale: f64) -> R<f64> {
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
        return Ok(f64::NAN);
    }
    if a <= 0.0 || scale <= 0.0 {
        return Ok(if scale == 0.0 || a == 0.0 {
            0.0
        } else {
            f64::NAN
        });
    }
    if !a.is_finite() || !scale.is_finite() {
        return Ok(f64::INFINITY);
    }
    if a < 1.0 {
        // GS: rejection from a mixture of a power density and an exponential tail.
        let e = 1.0 + 0.36787944117144233 * a;
        loop {
            let p = e * deviates.uniform()?;
            if p >= 1.0 {
                let x = -((e - p) / a).ln();
                if deviates.exponential()? >= (1.0 - a) * x.ln() {
                    return Ok(scale * x);
                }
            } else {
                let x = (p.ln() / a).exp();
                if deviates.exponential()? >= x {
                    return Ok(scale * x);
                }
            }
        }
    }
    // GD: a normal deviate transformed by x = (s + t/2)², accepted outright when t >= 0, by a
    // squeeze, or by the quotient test; otherwise a double exponential hat is sampled.
    let s2 = a - 0.5;
    let s = s2.sqrt();
    let d = SQRT_32 - s * 12.0;
    let mut t = deviates.normal()?;
    let x = s + 0.5 * t;
    let value = x * x;
    if t >= 0.0 {
        return Ok(scale * value);
    }
    let u = deviates.uniform()?;
    if d * u <= t * t * t {
        return Ok(scale * value);
    }
    let r = 1.0 / a;
    let q0 = ((((((Q7 * r + Q6) * r + Q5) * r + Q4) * r + Q3) * r + Q2) * r + Q1) * r;
    let (b, si, c) = if a <= 3.686 {
        (0.463 + s + 0.178 * s2, 1.235, 0.195 / s - 0.079 + 0.16 * s)
    } else if a <= 13.022 {
        (1.654 + 0.0076 * s2, 1.68 / s + 0.275, 0.062 / s + 0.024)
    } else {
        (1.77, 0.75, 0.1515 / s)
    };
    let log_quotient = |t: f64| {
        let v = t / (s + s);
        if v.abs() <= 0.25 {
            q0 + 0.5
                * t
                * t
                * ((((((A7 * v + A6) * v + A5) * v + A4) * v + A3) * v + A2) * v + A1)
                * v
        } else {
            q0 - s * t + 0.25 * t * t + (s2 + s2) * v.ln_1p()
        }
    };
    if x > 0.0 && (1.0 - u).ln() <= log_quotient(t) {
        return Ok(scale * value);
    }
    loop {
        let e = deviates.exponential()?;
        let mut u = deviates.uniform()?;
        u = u + u - 1.0;
        t = if u < 0.0 { b - si * e } else { b + si * e };
        if t >= -0.71874483771719 {
            let q = log_quotient(t);
            if q > 0.0 {
                let w = q.exp_m1();
                if c * u.abs() <= w * (e - 0.5 * t * t).exp() {
                    break;
                }
            }
        }
    }
    let x = s + 0.5 * t;
    Ok(scale * x * x)
}

/// Beta deviates by Cheng's (1978) algorithms BB (both shapes above one) and BC.
pub fn rbeta(deviates: &mut impl Deviates, aa: f64, bb: f64) -> R<f64> {
    const LOG_4: f64 = 1.3862944;
    if aa.is_nan() || bb.is_nan() || aa < 0.0 || bb < 0.0 {
        return Ok(f64::NAN);
    }
    if !aa.is_finite() && !bb.is_finite() {
        return Ok(0.5);
    }
    if aa == 0.0 && bb == 0.0 {
        return Ok(if deviates.uniform()? < 0.5 { 0.0 } else { 1.0 });
    }
    if !aa.is_finite() || bb == 0.0 {
        return Ok(1.0);
    }
    if !bb.is_finite() || aa == 0.0 {
        return Ok(0.0);
    }
    let exponent_limit = f64::from(f64::MAX_EXP) * LN_2;
    let a = aa.min(bb);
    let b = aa.max(bb);
    let alpha = a + b;
    // v = β log(u/(1 - u)) and w = shape · e^v, saturated rather than overflowing.
    let v_and_w = |u: f64, beta: f64, shape: f64| {
        let v = beta * (u / (1.0 - u)).ln();
        let w = if v <= exponent_limit {
            let w = shape * v.exp();
            if w.is_finite() { w } else { f64::MAX }
        } else {
            f64::MAX
        };
        (v, w)
    };
    if a <= 1.0 {
        let beta = 1.0 / a;
        let delta = 1.0 + b - a;
        let k1 = delta * (0.0138889 + 0.0416667 * a) / (b * beta - 0.777778);
        let k2 = 0.25 + (0.5 + 0.25 / delta) * a;
        let w = loop {
            let u1 = deviates.uniform()?;
            let u2 = deviates.uniform()?;
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
                    break v_and_w(u1, beta, b).1;
                }
                if z >= k2 {
                    continue;
                }
            }
            let (v, w) = v_and_w(u1, beta, b);
            if alpha * ((alpha / (a + w)).ln() + v) - LOG_4 >= z.ln() {
                break w;
            }
        };
        return Ok(if aa == a { a / (a + w) } else { w / (a + w) });
    }
    let beta = ((alpha - 2.0) / (2.0 * a * b - alpha)).sqrt();
    let gamma = a + 1.0 / beta;
    let w = loop {
        let u1 = deviates.uniform()?;
        let u2 = deviates.uniform()?;
        let (v, w) = v_and_w(u1, beta, a);
        let z = u1 * u1 * u2;
        let r = gamma * v - LOG_4;
        let s = a + r - w;
        if s + 2.609438 >= 5.0 * z {
            break w;
        }
        let t = z.ln();
        if s > t {
            break w;
        }
        if r + alpha * (alpha / (b + w)).ln() >= t {
            break w;
        }
    };
    Ok(if aa != a { b / (b + w) } else { w / (b + w) })
}

/// Poisson deviates by Ahrens and Dieter's (1982) algorithm PD: table lookup inversion for
/// means below 10, and otherwise a normal deviate accepted outright, by a squeeze, or by
/// quotient and hat tests against Hermite approximations to the discrete normal probabilities.
#[allow(clippy::excessive_precision)]
pub fn rpois(deviates: &mut impl Deviates, mu: f64) -> R<f64> {
    const A0: f64 = -0.5;
    const A1: f64 = 0.3333333;
    const A2: f64 = -0.2500068;
    const A3: f64 = 0.2000118;
    const A4: f64 = -0.1661269;
    const A5: f64 = 0.1421878;
    const A6: f64 = -0.1384794;
    const A7: f64 = 0.1250060;
    const FACTORIALS: [f64; 10] = [
        1.0, 1.0, 2.0, 6.0, 24.0, 120.0, 720.0, 5040.0, 40320.0, 362880.0,
    ];
    if !mu.is_finite() || mu < 0.0 {
        return Ok(f64::NAN);
    }
    if mu <= 0.0 {
        return Ok(0.0);
    }
    if mu < 10.0 {
        // Inversion against the cumulative probabilities of 0..=35, drawn again past them.
        let p0 = (-mu).exp();
        let mode = (mu as i64).max(1) as usize;
        let mut cumulative = [0.0f64; 36];
        let mut filled = 0usize;
        let mut probability = p0;
        let mut sum = p0;
        loop {
            let u = deviates.uniform()?;
            if u <= p0 {
                return Ok(0.0);
            }
            if filled > 0 {
                let first = if u > 0.458 { filled.min(mode) } else { 1 };
                if let Some(k) = (first..=filled).find(|&k| u <= cumulative[k]) {
                    return Ok(k as f64);
                }
                if filled == 35 {
                    continue;
                }
            }
            for (k, slot) in cumulative.iter_mut().enumerate().skip(filled + 1) {
                probability *= mu / k as f64;
                sum += probability;
                *slot = sum;
                if u <= sum {
                    return Ok(k as f64);
                }
            }
            filled = 35;
        }
    }
    let s = mu.sqrt();
    let d = 6.0 * mu * mu;
    let big_l = (mu - 1.1484).floor();
    let mut count = 0.0;
    let mut difference = 0.0;
    let mut u = 0.0;
    let normal = mu + s * deviates.normal()?;
    if normal >= 0.0 {
        count = normal.floor();
        if count >= big_l {
            return Ok(count);
        }
        difference = mu - count;
        u = deviates.uniform()?;
        if d * u >= difference * difference * difference {
            return Ok(count);
        }
    }
    let omega = ONE_OVER_SQRT_2PI / s;
    let b1 = 1.0 / 24.0 / mu;
    let b2 = 0.3 * b1 * b1;
    let c3 = 1.0 / 7.0 * b1 * b2;
    let c2 = b2 - 15.0 * c3;
    let c1 = b1 - 6.0 * b2 + 45.0 * c3;
    let c0 = 1.0 - b1 + 3.0 * b2 - 15.0 * c3;
    let c = 0.1069 / mu;
    // The log Poisson probability and the discrete normal approximation at `count`.
    let probabilities = |count: f64, difference: f64| -> (f64, f64, f64, f64) {
        let (px, py) = if count < 10.0 {
            (-mu, mu.powf(count) / FACTORIALS[count as usize])
        } else {
            let mut delta = 1.0 / 12.0 / count;
            delta *= 1.0 - 4.8 * delta * delta;
            let v = difference / count;
            let px = if v.abs() <= 0.25 {
                count
                    * v
                    * v
                    * (((((((A7 * v + A6) * v + A5) * v + A4) * v + A3) * v + A2) * v + A1) * v
                        + A0)
                    - delta
            } else {
                count * v.ln_1p() - difference - delta
            };
            (px, ONE_OVER_SQRT_2PI / count.sqrt())
        };
        let x = (0.5 - difference) / s;
        let square = x * x;
        let fx = -0.5 * square;
        let fy = omega * (((c3 * square + c2) * square + c1) * square + c0);
        (px, py, fx, fy)
    };
    if normal >= 0.0 {
        let (px, py, fx, fy) = probabilities(count, difference);
        if fy - u * fy <= py * (px - fx).exp() {
            return Ok(count);
        }
    }
    loop {
        let e = deviates.exponential()?;
        let u = 2.0 * deviates.uniform()? - 1.0;
        let t = 1.8 + if u >= 0.0 { e } else { -e };
        if t > -0.6744 {
            let count = (mu + s * t).floor();
            let difference = mu - count;
            let (px, py, fx, fy) = probabilities(count, difference);
            if c * u.abs() <= py * (px + e).exp() - fy * (fx + e).exp() {
                return Ok(count);
            }
        }
    }
}

/// Binomial deviates by Kachitvichyanukul and Schmeiser's (1988) algorithm BTPE: inversion when
/// the mean is below 30, and otherwise a triangle, parallelogram and two exponential tails as
/// the hat, with a squeeze and Stirling's formula in the acceptance test.
pub fn rbinom(deviates: &mut impl Deviates, size: f64, pp: f64) -> R<f64> {
    if !size.is_finite() {
        return Ok(f64::NAN);
    }
    let rounded = force_int(size);
    if rounded != size || !pp.is_finite() || rounded < 0.0 || !(0.0..=1.0).contains(&pp) {
        return Ok(f64::NAN);
    }
    if rounded == 0.0 || pp == 0.0 {
        return Ok(0.0);
    }
    if pp == 1.0 {
        return Ok(rounded);
    }
    if rounded >= f64::from(i32::MAX) {
        // Beyond the integer range of the algorithm, invert the distribution function.
        return Ok(qbinom(deviates.uniform()?, rounded, pp, false, false));
    }
    let n = rounded as i64;
    let nf = rounded;
    let p = pp.min(1.0 - pp);
    let q = 1.0 - p;
    let np = nf * p;
    let r = p / q;
    let g = r * (nf + 1.0);
    let draw = if np < 30.0 {
        let qn = q.powf(nf);
        'outer: loop {
            let mut ix = 0i64;
            let mut f = qn;
            let mut u = deviates.uniform()?;
            loop {
                if u < f {
                    break 'outer ix;
                }
                if ix > 110 {
                    break;
                }
                u -= f;
                ix += 1;
                f *= g / ix as f64 - r;
            }
        }
    } else {
        let mode_real = np + p;
        let m = mode_real as i64;
        let mf = m as f64;
        let npq = np * q;
        let p1 = (2.195 * npq.sqrt() - 4.6 * q).trunc() + 0.5;
        let xm = mf + 0.5;
        let xl = xm - p1;
        let xr = xm + p1;
        let c = 0.134 + 20.5 / (15.3 + mf);
        let mut al = (mode_real - xl) / (mode_real - xl * p);
        let xll = al * (1.0 + 0.5 * al);
        al = (xr - mode_real) / (xr * q);
        let xlr = al * (1.0 + 0.5 * al);
        let p2 = p1 * (1.0 + c + c);
        let p3 = p2 + c / xll;
        let p4 = p3 + c / xlr;
        loop {
            let u = deviates.uniform()? * p4;
            let mut v = deviates.uniform()?;
            let ix;
            if u <= p1 {
                break (xm - p1 * v + u) as i64;
            }
            if u <= p2 {
                let x = xl + (u - p1) / c;
                v = v * c + 1.0 - (xm - x).abs() / p1;
                if v > 1.0 || v <= 0.0 {
                    continue;
                }
                ix = x as i64;
            } else if u > p3 {
                ix = (xr - v.ln() / xlr) as i64;
                if ix > n {
                    continue;
                }
                v *= (u - p3) * xlr;
            } else {
                ix = (xl + v.ln() / xll) as i64;
                if ix < 0 {
                    continue;
                }
                v *= (u - p2) * xll;
            }
            let k = (ix - m).abs();
            let kf = k as f64;
            if k <= 20 || kf >= npq / 2.0 - 1.0 {
                // The ratio f(ix)/f(m) by the recurrence of the probabilities.
                let mut f = 1.0;
                if m < ix {
                    for i in m + 1..=ix {
                        f *= g / i as f64 - r;
                    }
                } else if m > ix {
                    for i in ix + 1..=m {
                        f /= g / i as f64 - r;
                    }
                }
                if v <= f {
                    break ix;
                }
            } else {
                let amaxp = (kf / npq) * ((kf * (kf / 3.0 + 0.625) + 0.1666666666666) / npq + 0.5);
                let ynorm = -kf * kf / (2.0 * npq);
                let alv = v.ln();
                if alv < ynorm - amaxp {
                    break ix;
                }
                if alv <= ynorm + amaxp {
                    let x1 = ix as f64 + 1.0;
                    let f1 = mf + 1.0;
                    let z = nf + 1.0 - mf;
                    let w = nf - ix as f64 + 1.0;
                    let correction = |y: f64| {
                        let square = y * y;
                        (13860.0
                            - (462.0 - (132.0 - (99.0 - 140.0 / square) / square) / square)
                                / square)
                            / y
                            / 166320.0
                    };
                    if alv
                        <= xm * (f1 / x1).ln()
                            + (nf - mf + 0.5) * (z / w).ln()
                            + (ix - m) as f64 * (w * p / (x1 * q)).ln()
                            + correction(f1)
                            + correction(z)
                            + correction(x1)
                            + correction(w)
                    {
                        break ix;
                    }
                }
            }
        }
    };
    let draw = if pp > 0.5 { n - draw } else { draw };
    Ok(draw as f64)
}

/// `log(i!)`, exactly for small `i` and by Stirling's series beyond.
#[allow(clippy::excessive_precision)]
fn log_factorial(i: f64) -> f64 {
    const SMALL: [f64; 8] = [
        0.0,
        0.0,
        LN_2,
        1.79175946922805500081247735838070,
        3.17805383034794561964694160129705,
        4.78749174278204599424770093452324,
        6.57925121201010099506017829290394,
        8.52516136106541430016553103634712,
    ];
    if i < 0.0 {
        return 0.0;
    }
    if i <= 7.0 {
        return SMALL[i as usize];
    }
    let square = i * i;
    (i + 0.5) * i.ln() - i + LN_SQRT_2PI + (0.0833333333333333 - 0.00277777777777778 / square) / i
}

/// Hypergeometric deviates by Kachitvichyanukul and Schmeiser's (1985) algorithms: inversion
/// (HIN) when the mode is within 10 of the smallest value, and otherwise H2PE, a rectangle and
/// two exponential tails as the hat with squeezes in the acceptance test.
pub fn rhyper(deviates: &mut impl Deviates, red: f64, black: f64, drawn: f64) -> R<f64> {
    const DELTA_LOWER: f64 = 0.0078;
    const DELTA_UPPER: f64 = 0.0034;
    if !red.is_finite() || !black.is_finite() || !drawn.is_finite() {
        return Ok(f64::NAN);
    }
    let (red, black, drawn) = (force_int(red), force_int(black), force_int(drawn));
    if red < 0.0 || black < 0.0 || drawn < 0.0 || drawn > red + black {
        return Ok(f64::NAN);
    }
    let limit = f64::from(i32::MAX);
    if red >= limit || black >= limit || drawn >= limit {
        if drawn == 1.0 {
            return rbinom(deviates, drawn, red / (red + black));
        }
        return Ok(qhyper(deviates.uniform()?, red, black, drawn, false, false));
    }
    let total = red + black;
    let (n1, n2) = if red <= black {
        (red, black)
    } else {
        (black, red)
    };
    let k = if drawn + drawn >= total {
        total - drawn
    } else {
        drawn
    };
    let m = ((k + 1.0) * (n1 + 1.0) / (total + 2.0)).trunc();
    let min_x = (k - n2).max(0.0);
    let max_x = n1.min(k);
    let ix = if min_x == max_x {
        max_x
    } else if m - min_x < 10.0 {
        // The probability of min_x, scaled by 1e25 against underflow.
        const SCALE: f64 = 1e25;
        const LOG_SCALE: f64 = 57.564_627_324_851_14;
        let log_w = if k < n2 {
            log_factorial(n2) + log_factorial(n1 + n2 - k)
                - log_factorial(n2 - k)
                - log_factorial(n1 + n2)
        } else {
            log_factorial(n1) + log_factorial(k) - log_factorial(k - n2) - log_factorial(n1 + n2)
        };
        let w = (log_w + LOG_SCALE).exp();
        'draw: loop {
            let mut ix = min_x;
            let mut u = deviates.uniform()? * SCALE;
            let mut p = w;
            while u > p {
                u -= p;
                p *= (n1 - ix) * (k - ix);
                ix += 1.0;
                p = p / ix / (n2 - k + ix);
                if ix > max_x {
                    continue 'draw;
                }
            }
            break ix;
        }
    } else {
        let s = ((total - k) * k * n1 * n2 / (total - 1.0) / total / total).sqrt();
        let d = (1.5 * s).trunc() + 0.5;
        let xl = m - d + 0.5;
        let xr = m + d + 0.5;
        let a = log_factorial(m)
            + log_factorial(n1 - m)
            + log_factorial(k - m)
            + log_factorial(n2 - k + m);
        let kl = (a
            - log_factorial(xl.trunc())
            - log_factorial((n1 - xl).trunc())
            - log_factorial((k - xl).trunc())
            - log_factorial((n2 - k + xl).trunc()))
        .exp();
        let kr = (a
            - log_factorial((xr - 1.0).trunc())
            - log_factorial((n1 - xr + 1.0).trunc())
            - log_factorial((k - xr + 1.0).trunc())
            - log_factorial((n2 - k + xr - 1.0).trunc()))
        .exp();
        let lambda_left = -(xl * (n2 - k + xl) / (n1 - xl + 1.0) / (k - xl + 1.0)).ln();
        let lambda_right = -((n1 - xr + 1.0) * (k - xr + 1.0) / xr / (n2 - k + xr)).ln();
        let p1 = d + d;
        let p2 = p1 + kl / lambda_left;
        let p3 = p2 + kr / lambda_right;
        let mut attempts = 0;
        loop {
            let u = deviates.uniform()? * p3;
            let mut v = deviates.uniform()?;
            attempts += 1;
            if attempts >= 10000 {
                return Ok(f64::NAN);
            }
            let ix;
            if u < p1 {
                ix = (xl + u).trunc();
            } else if u <= p2 {
                ix = (xl + v.ln() / lambda_left).trunc();
                if ix < min_x {
                    continue;
                }
                v *= (u - p1) * lambda_left;
            } else {
                ix = (xr - v.ln() / lambda_right).trunc();
                if ix > max_x {
                    continue;
                }
                v *= (u - p2) * lambda_right;
            }
            let accept = if m < 100.0 || ix <= 50.0 {
                // The ratio f(ix)/f(m) by the recurrence of the probabilities.
                let mut f = 1.0;
                if m < ix {
                    let mut i = m + 1.0;
                    while i <= ix {
                        f = f * (n1 - i + 1.0) * (k - i + 1.0) / (n2 - k + i) / i;
                        i += 1.0;
                    }
                } else if m > ix {
                    let mut i = ix + 1.0;
                    while i <= m {
                        f = f * i * (n2 - k + i) / (n1 - i + 1.0) / (k - i + 1.0);
                        i += 1.0;
                    }
                }
                v <= f
            } else {
                let y = ix;
                let y1 = y + 1.0;
                let ym = y - m;
                let yn = n1 - y + 1.0;
                let yk = k - y + 1.0;
                let nk = n2 - k + y1;
                let r = -ym / y1;
                let s = ym / yn;
                let t = ym / yk;
                let e = -ym / nk;
                let g = yn * yk / (y1 * nk) - 1.0;
                let dg = if g < 0.0 { 1.0 + g } else { 1.0 };
                let gu = g * (1.0 + g * (-0.5 + g / 3.0));
                let gl = gu - 0.25 * (g * g * g * g) / dg;
                let xm = m + 0.5;
                let xn = n1 - m + 0.5;
                let xk = k - m + 0.5;
                let nm = n2 - k + xm;
                let series = |z: f64| z * (1.0 + z * (-0.5 + z / 3.0));
                let upper = y * gu - m * gl
                    + DELTA_UPPER
                    + xm * series(r)
                    + xn * series(s)
                    + xk * series(t)
                    + nm * series(e);
                let alv = v.ln();
                if alv > upper {
                    false
                } else {
                    let fourth = |weight: f64, z: f64| {
                        let value = weight * (z * z * z * z);
                        if z < 0.0 { value / (1.0 + z) } else { value }
                    };
                    let lower = upper
                        - 0.25 * (fourth(xm, r) + fourth(xn, s) + fourth(xk, t) + fourth(nm, e))
                        + (y + m) * (gl - gu)
                        - DELTA_LOWER;
                    alv < lower
                        || alv
                            <= a - log_factorial(ix)
                                - log_factorial(n1 - ix)
                                - log_factorial(k - ix)
                                - log_factorial(n2 - k + ix)
                }
            };
            if accept {
                break ix;
            }
        }
    };
    // Undo the reductions to the smaller color and the smaller sample.
    let value = if drawn + drawn >= total {
        if red > black {
            drawn - black + ix
        } else {
            red - ix
        }
    } else if red > black {
        drawn - ix
    } else {
        ix
    };
    Ok(value)
}

pub fn rsignrank(deviates: &mut impl Deviates, n: f64) -> R<f64> {
    if n.is_nan() {
        return Ok(n);
    }
    let n = force_int(n);
    if n < 0.0 {
        return Ok(f64::NAN);
    }
    // Each rank joins the positive ranks with probability 1/2.
    let mut sum = 0.0;
    let mut rank = 1.0;
    while rank <= n {
        sum += rank * (deviates.uniform()? + 0.5).floor();
        rank += 1.0;
    }
    Ok(sum)
}

pub fn rwilcox(deviates: &mut impl Deviates, m: f64, n: f64) -> R<f64> {
    if m.is_nan() || n.is_nan() {
        return Ok(m + n);
    }
    let (m, n) = (force_int(m), force_int(n));
    if m < 0.0 || n < 0.0 {
        return Ok(f64::NAN);
    }
    if m == 0.0 || n == 0.0 {
        return Ok(0.0);
    }
    // The ranks of a sample of n drawn without replacement from the m + n pooled ranks.
    let mut remaining = (m + n) as usize;
    let mut pool: Vec<f64> = (0..remaining).map(|rank| rank as f64).collect();
    let mut sum = 0.0;
    for _ in 0..n as usize {
        let index = deviates.index(remaining as f64)? as usize;
        sum += pool[index];
        remaining -= 1;
        pool[index] = pool[remaining];
    }
    Ok(sum - n * (n - 1.0) / 2.0)
}

/// One multinomial draw of `size` objects into `probabilities.len()` boxes, as successive
/// binomial draws conditional on the boxes before.
pub fn rmultinom(
    deviates: &mut impl Deviates,
    size: i32,
    probabilities: &[f64],
    counts: &mut [i32],
) -> R<()> {
    counts.fill(0);
    let Some(last) = probabilities.len().checked_sub(1) else {
        return Ok(());
    };
    let mut left = size;
    let mut remaining_probability = 1.0;
    for (index, &probability) in probabilities[..last].iter().enumerate() {
        if probability > 0.0 {
            let conditional = probability / remaining_probability;
            counts[index] = if conditional < 1.0 {
                rbinom(deviates, f64::from(left), conditional)? as i32
            } else {
                left
            };
            left -= counts[index];
        }
        if left <= 0 {
            return Ok(());
        }
        remaining_probability -= probability;
    }
    counts[last] = left;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::excessive_precision)]
mod tests {
    use super::*;
    use crate::random::SeededDeviates;

    // Expected values are GNU R 4.3's, printed with `sprintf("%.17g")` from `.Internal(...)`.

    #[test]
    fn values_match_gnu_r() {
        let cases: [(f64, f64); 39] = [
            (dnorm(1.5, 0.0, 1.0, false), 0.12951759566589174),
            (dnorm(40.0, 0.0, 1.0, true), -800.91893853320471),
            (pnorm(-1.96, 0.0, 1.0, true, false), 0.024997895148220428),
            (pnorm(-40.0, 0.0, 1.0, true, true), -804.6084420137538),
            (qnorm(0.975, 0.0, 1.0, true, false), 1.9599639845400536),
            (qnorm(-1e5, 0.0, 1.0, true, true), -447.19789367852508),
            (qnorm(1e-300, 0.0, 1.0, true, false), -37.047096299361201),
            (pgamma(3.0, 2.5, 1.0, true, false), 0.69378108158672158),
            (pgamma(0.3, 0.01, 1.0, false, false), 0.009072396421765095),
            (qgamma(0.01, 0.5, 2.0, true, false), 0.00015708785790970206),
            (pbeta(0.3, 2.0, 5.0, true, false), 0.57982500000000015),
            (qbeta(0.3, 2.0, 5.0, true, false), 0.18180347131894911),
            (pt(2.1, 7.0, true, false), 0.9630644018935387),
            (qt(0.975, 10.0, true, false), 2.2281388519862739),
            (pf(2.5, 3.0, 12.0, false, false), 0.10915471239500631),
            (qf(0.95, 3.0, 12.0, true, false), 3.4902948194976036),
            (dbinom(3.0, 10.0, 0.3, false), 0.26682793199999993),
            (pbinom(3.0, 10.0, 0.3, true, false), 0.64961071840000018),
            (qbinom(0.5, 100.0, 0.3, true, false), 30.0),
            (dpois(5.0, 3.2, false), 0.11397938346351823),
            (ppois(5.0, 3.2, true, false), 0.89459189453082255),
            (qpois(0.9, 3.2, true, false), 6.0),
            (dnbinom(4.0, 3.0, 0.4, false), 0.124416),
            (pnbinom_mu(4.0, 3.0, 2.5, true, false), 0.84108134526042799),
            (
                phyper(3.0, 10.0, 7.0, 8.0, true, false),
                0.11703002879473474,
            ),
            (qhyper(0.5, 10.0, 7.0, 8.0, true, false), 5.0),
            (psignrank(10.0, 8.0, true, false), 0.15625000000000003),
            (qwilcox(0.3, 4.0, 6.0, true, false), 9.0),
            (pnt(1.5, 6.0, 0.8, true, false), 0.72121769867754626),
            (pnbeta(0.4, 2.0, 3.0, 1.5, true, false), 0.38769832015372552),
            (
                ptukey(3.5, 1.0, 4.0, 20.0, true, false),
                0.90504154945144333,
            ),
            (pweibull(2.0, 1.5, 3.0, true, false), 0.41977020402532716),
            (qcauchy(0.9, 0.0, 1.0, true, false), 3.0776835371752544),
            (plogis(2.0, 0.0, 1.0, false, true), -2.1269280110429727),
            (bessel_j(10.0, 1.5), 0.19798249275589311),
            (bessel_y(2.5, 0.0), 0.49807035961523194),
            (bessel_i(1.9, 3.7, false), 0.06474585856532572),
            (bessel_k(5.0, 0.5, true), 0.56049912163979287),
            (psigamma(3.0, 1.0), 0.39493406684822635),
        ];
        for (index, (computed, expected)) in cases.iter().enumerate() {
            let tolerance = 2e-15 * expected.abs();
            assert!(
                (computed - expected).abs() <= tolerance,
                "case {index}: computed {computed:e}, GNU R {expected:e}"
            );
        }
    }

    /// Six draws after `set.seed(42)`.
    fn draws(mut draw: impl FnMut(&mut SeededDeviates) -> R<f64>) -> [f64; 6] {
        let mut deviates = SeededDeviates::new(42);
        let mut values = [0.0; 6];
        for value in &mut values {
            *value = draw(&mut deviates).unwrap_or(f64::NAN);
        }
        values
    }

    #[test]
    fn streams_match_gnu_r() {
        let cases = [
            (
                "rnorm(6, 0, 1)",
                draws(|d| rnorm(d, 0.0, 1.0)),
                [
                    1.3709584471466685,
                    -0.56469817139608869,
                    0.3631284113373392,
                    0.63286260496104041,
                    0.40426832314099903,
                    -0.10612451609148403,
                ],
            ),
            (
                "rexp(6, 1)",
                draws(|d| rexp(d, 1.0)),
                [
                    0.19833681178388843,
                    0.66089525213465095,
                    0.28349103778600693,
                    0.038191898260265589,
                    0.47317662928253412,
                    1.4636271390223745,
                ],
            ),
            (
                "rgamma(6, 0.5, 1)",
                draws(|d| rgamma(d, 0.5, 1.0)),
                [
                    0.76051683481073429,
                    0.29369752276972361,
                    1.8663435236133064,
                    1.4817392879311493,
                    0.21342310315690366,
                    0.21113732015893,
                ],
            ),
            (
                "rgamma(6, 2.5, 1)",
                draws(|d| rgamma(d, 2.5, 1.0)),
                [
                    4.4087097953554784,
                    1.2811171935588865,
                    2.0682924802028513,
                    0.74289547108561538,
                    1.8527328732720545,
                    1.1782280677846075,
                ],
            ),
            (
                "rgamma(6, 30, 2)",
                draws(|d| rgamma(d, 30.0, 2.0)),
                [
                    74.832184205812368,
                    53.025249732731176,
                    59.521307851046316,
                    47.611048707336067,
                    57.852823883421195,
                    52.073846018491196,
                ],
            ),
            (
                "rbeta(6, 0.5, 3)",
                draws(|d| rbeta(d, 0.5, 3.0)),
                [
                    0.049375912760696576,
                    0.020869392201724776,
                    0.043455154844259096,
                    0.18955791671532488,
                    0.00081352712776081644,
                    0.18399255024043168,
                ],
            ),
            (
                "rbeta(6, 2, 3)",
                draws(|d| rbeta(d, 2.0, 3.0)),
                [
                    0.26816459253316166,
                    0.49404130999934553,
                    0.56653368574781304,
                    0.50500157980891991,
                    0.37369729134432084,
                    0.79190119136819337,
                ],
            ),
            (
                "rbinom(6, 10, 0.3)",
                draws(|d| rbinom(d, 10.0, 0.3)),
                [5.0, 5.0, 2.0, 4.0, 3.0, 3.0],
            ),
            (
                "rbinom(6, 1000, 0.4)",
                draws(|d| rbinom(d, 1000.0, 0.4)),
                [387.0, 413.0, 381.0, 408.0, 398.0, 430.0],
            ),
            (
                "rpois(6, 4)",
                draws(|d| rpois(d, 4.0)),
                [7.0, 7.0, 3.0, 6.0, 5.0, 4.0],
            ),
            (
                "rpois(6, 50)",
                draws(|d| rpois(d, 50.0)),
                [59.0, 46.0, 50.0, 42.0, 49.0, 60.0],
            ),
            (
                "rgeom(6, 0.2)",
                draws(|d| rgeom(d, 0.2)),
                [2.0, 1.0, 0.0, 2.0, 3.0, 3.0],
            ),
            (
                "rhyper(6, 5, 5, 3)",
                draws(|d| rhyper(d, 5.0, 5.0, 3.0)),
                [2.0, 3.0, 1.0, 2.0, 2.0, 2.0],
            ),
            (
                "rhyper(6, 1000, 1500, 800)",
                draws(|d| rhyper(d, 1000.0, 1500.0, 800.0)),
                [302.0, 314.0, 328.0, 332.0, 329.0, 321.0],
            ),
            (
                "rnbinom(6, 3, 0.5)",
                draws(|d| rnbinom(d, 3.0, 0.5)),
                [4.0, 4.0, 4.0, 4.0, 5.0, 3.0],
            ),
            (
                "rt(6, 5)",
                draws(|d| rt(d, 5.0)),
                [
                    1.9151370856546839,
                    0.08784220871601052,
                    -0.077326986327275235,
                    -0.061759309802029538,
                    -0.047980922981410913,
                    5.0230454971384102,
                ],
            ),
            (
                "rcauchy(6, 0, 1)",
                draws(|d| rcauchy(d, 0.0, 1.0)),
                [
                    -0.27422408712683582,
                    -0.20029939108706107,
                    1.2574027505194214,
                    -0.58950182681702079,
                    -2.0952077123310016,
                    -16.648972316285512,
                ],
            ),
            (
                "rsignrank(6, 10)",
                draws(|d| rsignrank(d, 10.0)),
                [44.0, 28.0, 32.0, 32.0, 44.0, 27.0],
            ),
            (
                "rwilcox(6, 4, 5)",
                draws(|d| rwilcox(d, 4.0, 5.0)),
                [9.0, 9.0, 11.0, 4.0, 15.0, 11.0],
            ),
            (
                "rnchisq(6, 3, 2)",
                draws(|d| rnchisq(d, 3.0, 2.0)),
                [
                    12.289642359516762,
                    4.9855135022633217,
                    2.8900046541136231,
                    7.4106758924475198,
                    16.410071340079739,
                    6.8346783809913472,
                ],
            ),
        ];
        for (name, computed, expected) in cases {
            assert_eq!(computed, expected, "{name}");
        }
    }

    #[test]
    fn invalid_parameters_give_nan() {
        assert!(dnorm(1.0, 0.0, -1.0, false).is_nan());
        assert!(pbeta(0.5, -1.0, 2.0, true, false).is_nan());
        assert!(qgamma(1.5, 2.0, 1.0, true, false).is_nan());
        assert_eq!(qgamma(0.0, 2.0, 1.0, true, false), 0.0);
        assert_eq!(qnorm(0.0, 0.0, 1.0, false, true), f64::NEG_INFINITY);
    }

    #[test]
    fn non_integer_counts_warn() {
        take_warnings();
        assert_eq!(dbinom(1.5, 3.0, 0.5, false), 0.0);
        assert_eq!(take_warnings(), ["non-integer x = 1.500000"]);
    }
}
