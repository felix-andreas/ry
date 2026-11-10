// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's C sources, and keep
// their copyright notices:
//
// GNU R: src/nmath/rgamma.c (rgamma)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000--2016 The R Core Team
//
// GNU R: src/nmath/rbeta.c (rbeta)
//     Copyright (C) 1995, 1996 Robert Gentleman and Ross Ihaka
//     Copyright (C) 2000--2016 The R Core Team
//     Copyright (C) 2001--2016 The R Foundation
//
// GNU R: src/nmath/rpois.c (rpois)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000-2011 The R Core Team
//
// GNU R: src/nmath/rbinom.c (rbinom)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000-2014 The R Core Team
//     Copyright (C) 2007 The R Foundation
//
// GNU R: src/nmath/rhyper.c (rhyper)
//     Copyright (C) 2000-2020 The R Core Team
//     Copyright (C) 2005-2020 The R Foundation
//     Copyright (C) 1998 Ross Ihaka
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

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
//! The algorithms are the published ones GNU R's results come from:
//! Cody (1969) for the normal distribution function, Wichura's AS 241 for its quantile, Loader's
//! (2000) saddle point expansion for the binomial and Poisson densities, continued fractions and
//! series for the incomplete gamma and beta functions, Lenth's AS 226 and AS 243 for the
//! noncentral beta and t distributions, Copenhaver and Holland (1988) for the studentized range,
//! and Ahrens and Dieter, Cheng, and Kachitvichyanukul and Schmeiser for the random generators,
//! which consume the uniform stream in exactly GNU R's order so that `set.seed` reproduces its
//! draws. The random generators are translations of GNU R's nmath versions (the files named
//! above).
//!
//! Numerical warnings that Rmath reports through `MATHLIB_WARNING` (a non-integer count, lost
//! precision) are collected while the functions run and raised by the builtin afterwards, which
//! keeps the functions free of interpreter state.

pub use crate::random::Deviates;
use {
    crate::{
        base_fast_paths::NativeEntry,
        builtin_libraries::NativeFn,
        builtins::{Arg, BuiltinDef, internal},
        coerce::coerce_vector,
        conditions::warning,
        random::{self, with_deviates},
        rmath::*,
        unwind::{R, Unwind, error},
        values::{
            alloc, alloc_uninit, as_integer_scalar, copy_attributes, get_attr, inherits,
            integer_vector, ints, ints_mut, length, list_vector, materialize, nil, reals,
            reals_mut, set_attr, symbols,
        },
    },
    heap::object::{NA_INTEGER, NA_REAL, Sexp, SexpType, is_na_real},
};

/// Generates the `.Internal` table and the stats routine table from one list, so that both
/// entry points vectorize every function identically. A row with a body defines its
/// [`Kernel`] or [`Sampler`] type from the body; a row without one names a type written by
/// hand.
macro_rules! distributions {
    (
        math { $($name:literal $arity:literal $vectors:literal $kernel:ident $(|$v:ident, $f:tt| $body:expr)?;)* }
        base { $($base_name:literal $base_arity:literal $base_vectors:literal $base_kernel:ident |$base_v:ident, $base_f:tt| $base_body:expr;)* }
        random { $($random_name:literal $random_arity:literal $shape:ident $sampler:ident $(|$s:ident, $p:ident| $random_body:expr)?;)* }
    ) => {
        $(distributions!(@kernel $kernel $vectors $(|$v, $f| $body)?);)*
        $(distributions!(@kernel $base_kernel $base_vectors |$base_v, $base_f| $base_body);)*
        $(distributions!(@sampler $sampler $(|$s, $p| $random_body)?);)*

        /// The `.Internal` forms, which GNU R 4.3 still keeps in base beside the stats routines,
        /// and the Bessel functions and `psigamma`, which only base has.
        #[rustfmt::skip]
        pub const BUILTINS: &[BuiltinDef] = &[
            $(internal($name, $arity, |call| unsafe { signalled(math::<$kernel>(call.args, Interface::Internal, Conditions::Signal)) }),)*
            $(internal($base_name, $base_arity, |call| unsafe { signalled(math::<$base_kernel>(call.args, Interface::Internal, Conditions::Signal)) }),)*
            $(internal($random_name, $random_arity, |call| unsafe {
                signalled(random_vector::<$sampler>(call.args, Interface::Internal, distributions!(@integer $shape), Conditions::Signal))
            }),)*
            internal("dtukey", 5, |_| Err(error("unimplemented real function of 4 numeric arguments"))),
        ];

        /// The stats package's `.Call` and `.External` routines, which `builtin_libraries`
        /// binds to the `C_` symbols of stats' R code.
        #[rustfmt::skip]
        pub const ROUTINES: &[(&str, NativeFn)] = &[
            $(($name, |call| unsafe { signalled(math::<$kernel>(call.args, Interface::Stats, Conditions::Signal)) }),)*
            $(($random_name, |call| unsafe {
                signalled(random_vector::<$sampler>(call.args, Interface::Stats, distributions!(@integer $shape), Conditions::Signal))
            }),)*
            ("rmultinom", |call| unsafe { rmultinom_routine(call.arg(0), call.arg(1), call.arg(2)) }),
            ("signrank_free", |_| Ok(nil())),
            ("wilcox_free", |_| {
                wilcox_free();
                Ok(nil())
            }),
        ];

        /// The stats routines a closure call can reach without a frame, with their arity: each
        /// returns `None` instead of signalling a condition, so the caller runs the closure,
        /// whose frame the condition reports (`base_fast_paths::register_routine_entries`).
        #[rustfmt::skip]
        pub const DIRECT_ROUTINES: &[(&str, usize, NativeEntry)] = &[
            $(($name, $arity, |args| unsafe {
                if !plain_numbers(args) {
                    return Ok(None);
                }
                declined_on_error(math::<$kernel>(args, Interface::Stats, Conditions::Decline))
            }),)*
            $(($random_name, $random_arity, |args| unsafe {
                if !plain_numbers(args) {
                    return Ok(None);
                }
                declined_on_error(random_vector::<$sampler>(
                    args,
                    Interface::Stats,
                    distributions!(@integer $shape),
                    Conditions::Decline,
                ))
            }),)*
        ];
    };
    (@kernel $kernel:ident $vectors:literal |$v:ident, $f:tt| $body:expr) => {
        struct $kernel;

        impl Kernel for $kernel {
            const VECTORS: usize = $vectors;

            #[inline(always)]
            fn apply($v: &[f64; 4], $f: [i32; 2]) -> f64 {
                $body
            }
        }
    };
    (@kernel $kernel:ident $vectors:literal) => {};
    (@sampler $sampler:ident |$s:ident, $p:ident| $body:expr) => {
        struct $sampler([f64; 3]);

        impl Sampler for $sampler {
            fn prepare(parameters: &[f64; 3]) -> Self {
                Self(*parameters)
            }

            #[inline(always)]
            fn draw<D: Deviates>(&mut self, $s: &mut D) -> R<f64> {
                let $p = &self.0;
                $body
            }
        }
    };
    (@sampler $sampler:ident) => {};
    (@integer integer) => { true };
    (@integer real) => { false };
}

distributions! {
    math {
        "dchisq" 3 2 Dchisq |v, f| dchisq(v[0], v[1], f[0] != 0);
        "pchisq" 4 2 Pchisq |v, f| pchisq(v[0], v[1], f[0] != 0, f[1] != 0);
        "qchisq" 4 2 Qchisq |v, f| qchisq(v[0], v[1], f[0] != 0, f[1] != 0);
        "dexp" 3 2 Dexp |v, f| dexp(v[0], v[1], f[0] != 0);
        "pexp" 4 2 Pexp |v, f| pexp(v[0], v[1], f[0] != 0, f[1] != 0);
        "qexp" 4 2 Qexp |v, f| qexp(v[0], v[1], f[0] != 0, f[1] != 0);
        "dgeom" 3 2 Dgeom |v, f| dgeom(v[0], v[1], f[0] != 0);
        "pgeom" 4 2 Pgeom |v, f| pgeom(v[0], v[1], f[0] != 0, f[1] != 0);
        "qgeom" 4 2 Qgeom |v, f| qgeom(v[0], v[1], f[0] != 0, f[1] != 0);
        "dpois" 3 2 Dpois |v, f| dpois(v[0], v[1], f[0] != 0);
        "ppois" 4 2 Ppois |v, f| ppois(v[0], v[1], f[0] != 0, f[1] != 0);
        "qpois" 4 2 Qpois |v, f| qpois(v[0], v[1], f[0] != 0, f[1] != 0);
        "dt" 3 2 Dt |v, f| dt(v[0], v[1], f[0] != 0);
        "pt" 4 2 Pt |v, f| pt(v[0], v[1], f[0] != 0, f[1] != 0);
        "qt" 4 2 Qt |v, f| qt(v[0], v[1], f[0] != 0, f[1] != 0);
        "dsignrank" 3 2 Dsignrank |v, f| dsignrank(v[0], v[1], f[0] != 0);
        "psignrank" 4 2 Psignrank |v, f| psignrank(v[0], v[1], f[0] != 0, f[1] != 0);
        "qsignrank" 4 2 Qsignrank |v, f| qsignrank(v[0], v[1], f[0] != 0, f[1] != 0);
        "dbeta" 4 3 Dbeta |v, f| dbeta(v[0], v[1], v[2], f[0] != 0);
        "pbeta" 5 3 Pbeta |v, f| pbeta(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qbeta" 5 3 Qbeta |v, f| qbeta(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dbinom" 4 3 Dbinom |v, f| dbinom(v[0], v[1], v[2], f[0] != 0);
        "pbinom" 5 3 Pbinom |v, f| pbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qbinom" 5 3 Qbinom |v, f| qbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dcauchy" 4 3 Dcauchy |v, f| dcauchy(v[0], v[1], v[2], f[0] != 0);
        "pcauchy" 5 3 Pcauchy |v, f| pcauchy(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qcauchy" 5 3 Qcauchy |v, f| qcauchy(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "df" 4 3 Df |v, f| df(v[0], v[1], v[2], f[0] != 0);
        "pf" 5 3 Pf |v, f| pf(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qf" 5 3 Qf |v, f| qf(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dgamma" 4 3 Dgamma |v, f| dgamma(v[0], v[1], v[2], f[0] != 0);
        "pgamma" 5 3 Pgamma |v, f| pgamma(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qgamma" 5 3 Qgamma |v, f| qgamma(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dlnorm" 4 3 Dlnorm |v, f| dlnorm(v[0], v[1], v[2], f[0] != 0);
        "plnorm" 5 3 Plnorm |v, f| plnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qlnorm" 5 3 Qlnorm |v, f| qlnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dlogis" 4 3 Dlogis |v, f| dlogis(v[0], v[1], v[2], f[0] != 0);
        "plogis" 5 3 Plogis |v, f| plogis(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qlogis" 5 3 Qlogis |v, f| qlogis(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dnbinom" 4 3 Dnbinom |v, f| dnbinom(v[0], v[1], v[2], f[0] != 0);
        "pnbinom" 5 3 Pnbinom |v, f| pnbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qnbinom" 5 3 Qnbinom |v, f| qnbinom(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dnbinom_mu" 4 3 DnbinomMu |v, f| dnbinom_mu(v[0], v[1], v[2], f[0] != 0);
        "pnbinom_mu" 5 3 PnbinomMu |v, f| pnbinom_mu(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qnbinom_mu" 5 3 QnbinomMu |v, f| qnbinom_mu(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dnorm" 4 3 Dnorm |v, f| dnorm(v[0], v[1], v[2], f[0] != 0);
        "pnorm" 5 3 Pnorm |v, f| pnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qnorm" 5 3 Qnorm;
        "dunif" 4 3 Dunif |v, f| dunif(v[0], v[1], v[2], f[0] != 0);
        "punif" 5 3 Punif |v, f| punif(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qunif" 5 3 Qunif |v, f| qunif(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dweibull" 4 3 Dweibull |v, f| dweibull(v[0], v[1], v[2], f[0] != 0);
        "pweibull" 5 3 Pweibull |v, f| pweibull(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qweibull" 5 3 Qweibull |v, f| qweibull(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dnchisq" 4 3 Dnchisq |v, f| dnchisq(v[0], v[1], v[2], f[0] != 0);
        "pnchisq" 5 3 Pnchisq |v, f| pnchisq(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qnchisq" 5 3 Qnchisq |v, f| qnchisq(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dnt" 4 3 Dnt |v, f| dnt(v[0], v[1], v[2], f[0] != 0);
        "pnt" 5 3 Pnt |v, f| pnt(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qnt" 5 3 Qnt |v, f| qnt(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dwilcox" 4 3 Dwilcox |v, f| dwilcox(v[0], v[1], v[2], f[0] != 0);
        "pwilcox" 5 3 Pwilcox |v, f| pwilcox(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "qwilcox" 5 3 Qwilcox |v, f| qwilcox(v[0], v[1], v[2], f[0] != 0, f[1] != 0);
        "dhyper" 5 4 Dhyper |v, f| dhyper(v[0], v[1], v[2], v[3], f[0] != 0);
        "phyper" 6 4 Phyper |v, f| phyper(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "qhyper" 6 4 Qhyper |v, f| qhyper(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "dnbeta" 5 4 Dnbeta |v, f| dnbeta(v[0], v[1], v[2], v[3], f[0] != 0);
        "pnbeta" 6 4 Pnbeta |v, f| pnbeta(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "qnbeta" 6 4 Qnbeta |v, f| qnbeta(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "dnf" 5 4 Dnf |v, f| dnf(v[0], v[1], v[2], v[3], f[0] != 0);
        "pnf" 6 4 Pnf |v, f| pnf(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "qnf" 6 4 Qnf |v, f| qnf(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "ptukey" 6 4 Ptukey |v, f| ptukey(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
        "qtukey" 6 4 Qtukey |v, f| qtukey(v[0], v[1], v[2], v[3], f[0] != 0, f[1] != 0);
    }
    base {
        "besselJ" 2 2 BesselJ |v, _| bessel_j(v[0], v[1]);
        "besselY" 2 2 BesselY |v, _| bessel_y(v[0], v[1]);
        "besselI" 3 2 BesselI |v, f| bessel_i(v[0], v[1], f[0] as f64);
        "besselK" 3 2 BesselK |v, f| bessel_k(v[0], v[1], f[0] as f64);
        "psigamma" 2 2 Psigamma |v, _| psigamma(v[0], v[1]);
    }
    random {
        "rchisq" 2 real Rchisq |s, v| rchisq(s, v[0]);
        "rexp" 2 real Rexp |s, v| rexp(s, v[0]);
        "rgeom" 2 integer Rgeom |s, v| rgeom(s, v[0]);
        "rpois" 2 integer Poisson;
        "rt" 2 real Rt |s, v| rt(s, v[0]);
        "rsignrank" 2 integer Rsignrank |s, v| rsignrank(s, v[0]);
        "rbeta" 3 real Rbeta |s, v| rbeta(s, v[0], v[1]);
        "rbinom" 3 integer Binomial;
        "rcauchy" 3 real Rcauchy |s, v| rcauchy(s, v[0], v[1]);
        "rf" 3 real Rf |s, v| rf(s, v[0], v[1]);
        "rgamma" 3 real Rgamma |s, v| rgamma(s, v[0], v[1]);
        "rlnorm" 3 real Rlnorm |s, v| rlnorm(s, v[0], v[1]);
        "rlogis" 3 real Rlogis |s, v| rlogis(s, v[0], v[1]);
        "rnbinom" 3 integer Rnbinom |s, v| rnbinom(s, v[0], v[1]);
        "rnbinom_mu" 3 real RnbinomMu |s, v| rnbinom_mu(s, v[0], v[1]);
        "rnchisq" 3 real Rnchisq |s, v| rnchisq(s, v[0], v[1]);
        "rnorm" 3 real NormalSampler;
        "runif" 3 real UniformSampler;
        "rweibull" 3 real Rweibull |s, v| rweibull(s, v[0], v[1]);
        "rwilcox" 3 integer Rwilcox |s, v| rwilcox(s, v[0], v[1]);
        "rhyper" 4 integer Rhyper |s, v| rhyper(s, v[0], v[1], v[2]);
    }
}

/// Which entry point a vectorized call came through. The stats routines and the `.Internal`
/// forms differ only in an error message's capitalization and in the type of the counts the
/// discrete generators return.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
    Internal,
    Stats,
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

/// Whether a routine signals the warnings and errors it meets, or declines with `None` before
/// any effect, so that a caller running a closure's call without its frame
/// (`base_fast_paths::native_entry`) runs the closure instead and the condition reports the
/// closure's call, as it does in GNU R.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conditions {
    Signal,
    Decline,
}

/// The value of a routine run with [`Conditions::Signal`], which never declines.
fn signalled(value: R<Option<Sexp>>) -> R<Sexp> {
    value.map(|value| value.unwrap_or_else(nil))
}

/// The value of a routine run with [`Conditions::Decline`]: an error declines too, because the
/// closure's own run raises it again against its call.
fn declined_on_error(value: R<Option<Sexp>>) -> R<Option<Sexp>> {
    match value {
        Err(Unwind::Error(_)) => Ok(None),
        value => value,
    }
}

/// Whether every argument is a logical, integer or double vector held in memory, which the
/// routines read without coercing (a string flag warns as it converts) or running an ALTREP
/// class's code, neither of which a declining routine could undo.
unsafe fn plain_numbers(args: &[Arg]) -> bool {
    args.iter().all(|arg| {
        matches!(
            arg.value.kind(),
            SexpType::Logical | SexpType::Integer | SexpType::Real
        ) && !arg.value.is_altrep()
    })
}

/// Whether the routine must decline rather than signal, discarding the warnings Rmath raised.
fn declines(conditions: Conditions, signals: bool) -> bool {
    if conditions == Conditions::Decline && (signals || has_warnings()) {
        take_warnings();
        return true;
    }
    false
}

unsafe fn raise_warnings() -> R<()> {
    for message in take_warnings() {
        warning(message)?;
    }
    Ok(())
}

// Vectorization.

/// A density, distribution or quantile function of up to four recycled operands and two
/// integer flags, as the `math` loop applies it.
trait Kernel {
    /// How many leading arguments recycle; the rest are flags.
    const VECTORS: usize;

    /// The function at one element, whose operands are neither NA nor NaN.
    fn apply(values: &[f64; 4], flags: [i32; 2]) -> f64;

    /// Writes the whole result when the first operand is as long as the result and every
    /// other operand is the scalar in `rest`, with GNU R's NA and NaN handling, and returns
    /// whether a NaN was produced. A kernel without a faster path for that case declines.
    fn fill_first(
        _first: &[f64],
        _rest: &[f64; 4],
        _flags: [i32; 2],
        _out: &mut [f64],
    ) -> Option<bool> {
        None
    }
}

/// GNU R's `math2` to `math4`: the first `K::VECTORS` arguments recycle, the rest are integer
/// flags (`log`, `lower.tail`, `log.p`, Bessel's `expo`), read as `asInteger` reads them.
unsafe fn math<K: Kernel>(
    args: &[Arg],
    interface: Interface,
    conditions: Conditions,
) -> R<Option<Sexp>> {
    let vectors = K::VECTORS;
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
        return alloc(SexpType::Real, 0).map(Some);
    }
    let count = lengths[..vectors].iter().copied().max().unwrap_or(0);
    // The loop below writes every element.
    let result = alloc_uninit(SexpType::Real, count)?;
    // The slices are taken after the last allocation, while `operands` keeps the vectors live.
    let mut values: [&[f64]; 4] = [&[]; 4];
    for index in 0..vectors {
        values[index] = reals(operands[index]);
    }
    let out = reals_mut(result);
    let first_only = lengths[0] == count && lengths[1..vectors].iter().all(|&length| length == 1);
    let fast = if first_only {
        let mut rest = [0.0f64; 4];
        for index in 1..vectors {
            rest[index] = values[index].first().copied().unwrap_or(f64::NAN);
        }
        K::fill_first(values[0], &rest, flags, out)
    } else {
        None
    };
    let nan_produced = match fast {
        Some(nan_produced) => nan_produced,
        None => math_loop::<K>(&values, flags, out),
    };
    if declines(conditions, nan_produced) {
        return Ok(None);
    }
    raise_warnings()?;
    if nan_produced {
        warning("NaNs produced")?;
    }
    if let Some(index) = (0..vectors).find(|&index| lengths[index] == count) {
        copy_attributes(originals[index], result)?;
    }
    Ok(Some(result))
}

/// The element loop of [`math`], which recycles each operand with its own wrapping position
/// rather than a division per element. Returns whether a NaN was produced.
#[inline(always)]
fn math_loop<K: Kernel>(values: &[&[f64]; 4], flags: [i32; 2], out: &mut [f64]) -> bool {
    let mut positions = [0usize; 4];
    let mut arguments = [0.0f64; 4];
    let mut nan_produced = false;
    for slot in out {
        let mut any_nan = false;
        for index in 0..K::VECTORS {
            let operand = values[index];
            let value = operand[positions[index]];
            positions[index] += 1;
            if positions[index] == operand.len() {
                positions[index] = 0;
            }
            any_nan |= value.is_nan();
            arguments[index] = value;
        }
        *slot = if any_nan {
            na_or_nan(&arguments[..K::VECTORS])
        } else {
            let value = K::apply(&arguments, flags);
            nan_produced |= value.is_nan();
            value
        };
    }
    nan_produced
}

/// The result at an element with a NaN operand: `NA` when any operand is `NA`, else NaN.
#[cold]
fn na_or_nan(operands: &[f64]) -> f64 {
    if operands.iter().any(|&value| is_na_real(value)) {
        NA_REAL
    } else {
        f64::NAN
    }
}

/// `qnorm`, whose central region runs across SIMD lanes when only the probabilities vary.
struct Qnorm;

impl Kernel for Qnorm {
    const VECTORS: usize = 3;

    #[inline(always)]
    fn apply(v: &[f64; 4], f: [i32; 2]) -> f64 {
        qnorm(v[0], v[1], v[2], f[0] != 0, f[1] != 0)
    }

    fn fill_first(
        probabilities: &[f64],
        rest: &[f64; 4],
        flags: [i32; 2],
        out: &mut [f64],
    ) -> Option<bool> {
        let [_, mu, sigma, _] = *rest;
        let lower = flags[0] != 0;
        if !mu.is_finite() || !sigma.is_finite() || sigma <= 0.0 || flags[1] != 0 {
            return None;
        }
        let mut shifted = [0.0f64; QNORM_CHUNK];
        let mut tails = [0u16; QNORM_CHUNK];
        let mut nan_produced = false;
        for (chunk, out) in probabilities
            .chunks(QNORM_CHUNK)
            .zip(out.chunks_mut(QNORM_CHUNK))
        {
            let shifted = &mut shifted[..chunk.len()];
            // `R_DT_qIv`, which `qnorm` applies before it splits off the central region.
            for (slot, &probability) in shifted.iter_mut().zip(chunk) {
                *slot = if lower {
                    probability
                } else {
                    0.5 - probability + 0.5
                };
            }
            let count = qnorm_central_fill(shifted, out, &mut tails);
            let count = qnorm_tails_fill(chunk, lower, &mut tails, count, out);
            for slot in out.iter_mut() {
                *slot = mu + sigma * *slot;
            }
            for &index in &tails[..count] {
                let index = usize::from(index);
                let probability = chunk[index];
                out[index] = if probability.is_nan() {
                    na_or_nan(&[probability])
                } else {
                    let value = qnorm(probability, mu, sigma, lower, false);
                    nan_produced |= value.is_nan();
                    value
                };
            }
        }
        Some(nan_produced)
    }
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
    if length(value) == 0 {
        return Ok(NA_INTEGER);
    }
    match value.kind() {
        // A logical stores its values as integers, with the same NA.
        SexpType::Logical | SexpType::Integer => return Ok(ints(value)[0]),
        SexpType::Real => {
            let real = reals(value)[0];
            if real.is_nan() {
                return Ok(NA_INTEGER);
            }
            // Out of range, `asInteger` warns, so that case takes the coercion below.
            if real > f64::from(i32::MIN) && real < f64::from(i32::MAX) + 1.0 {
                return Ok(real as i32);
            }
        }
        SexpType::Complex | SexpType::String => {}
        _ => return Ok(NA_INTEGER),
    }
    let integer = coerce_vector(value, SexpType::Integer)?;
    Ok(ints(integer)[0])
}

/// A random generator prepared for one parameter set, so that the work that depends only on
/// the parameters runs once per batch and not once per draw.
trait Sampler {
    fn prepare(parameters: &[f64; 3]) -> Self;

    /// One deviate, consuming the generator exactly as GNU R's function does.
    fn draw<D: Deviates>(&mut self, deviates: &mut D) -> R<f64>;

    /// Fills `slots` with deviates, exactly as as many calls of [`Sampler::draw`] would.
    #[inline(always)]
    fn fill<D: Deviates>(&mut self, deviates: &mut D, slots: &mut [f64]) -> R<()> {
        for slot in slots {
            *slot = self.draw(deviates)?;
        }
        Ok(())
    }
}

/// GNU R's `random1` to `random3`: `n` is a count, or the length of a longer vector, and the
/// parameters recycle over it. Missing parameters give `NA`s, and every NaN drawn raises "NAs
/// produced". The stats routines return the discrete generators' counts as integers when they
/// all fit.
unsafe fn random_vector<S: Sampler>(
    args: &[Arg],
    interface: Interface,
    integer: bool,
    conditions: Conditions,
) -> R<Option<Sexp>> {
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
    // Every element is written below before anything reads the vector, and a failed draw
    // returns the error, so the vector starts unzeroed.
    let result = alloc_uninit(SexpType::Real, count)?;
    let drawn = reals_mut(result);
    if count > 0 && lengths[..parameters].contains(&0) {
        if conditions == Conditions::Decline {
            return Ok(None);
        }
        drawn.fill(NA_REAL);
        warning("NAs produced")?;
        return random_result(result, integer).map(Some);
    }
    if count > 0 {
        let mut values: [&[f64]; 3] = [&[]; 3];
        for index in 0..parameters {
            values[index] = reals(operands[index]);
        }
        // Declining discards the draws, which only works while `.Random.seed` holds the state
        // the draws start from.
        let checkpoint = match conditions {
            Conditions::Signal => None,
            Conditions::Decline => match random::checkpoint() {
                Some(checkpoint) => Some(checkpoint),
                None => return Ok(None),
            },
        };
        // A checkpoint found `.Random.seed` holding the generator's state already.
        if checkpoint.is_none() {
            random::get_rng_state()?;
        }
        let outcome =
            with_deviates!(|deviates| draw_all::<S, _>(deviates, &values[..parameters], drawn));
        if let Some(checkpoint) = checkpoint {
            let produced_na = reals(result).iter().any(|value| value.is_nan());
            if declines(conditions, outcome.is_err() || produced_na) {
                random::rollback(checkpoint);
                return Ok(None);
            }
        }
        random::put_rng_state()?;
        outcome?;
        raise_warnings()?;
        if reals(result).iter().any(|value| value.is_nan()) {
            warning("NAs produced")?;
        }
    }
    random_result(result, integer).map(Some)
}

/// Fills `slots` with draws whose parameters recycle from `values`. Scalar parameters prepare
/// the sampler once for the whole batch; recycled ones prepare it again only when a parameter
/// set differs from the previous element's.
fn draw_all<S: Sampler, D: Deviates>(
    deviates: &mut D,
    values: &[&[f64]],
    slots: &mut [f64],
) -> R<()> {
    let mut arguments = [0.0f64; 3];
    for (slot, operand) in arguments.iter_mut().zip(values) {
        *slot = operand.first().copied().unwrap_or(f64::NAN);
    }
    if values.iter().all(|operand| operand.len() == 1) {
        return S::prepare(&arguments).fill(deviates, slots);
    }
    let mut sampler = S::prepare(&arguments);
    let mut prepared = arguments.map(f64::to_bits);
    let mut positions = [0usize; 3];
    for slot in slots {
        for (index, operand) in values.iter().enumerate() {
            arguments[index] = operand[positions[index]];
            positions[index] += 1;
            if positions[index] == operand.len() {
                positions[index] = 0;
            }
        }
        // Bit patterns, so that a NaN parameter compares equal to itself.
        let bits = arguments.map(f64::to_bits);
        if bits != prepared {
            sampler = S::prepare(&arguments);
            prepared = bits;
        }
        *slot = sampler.draw(deviates)?;
    }
    Ok(())
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
        SexpType::Logical | SexpType::Integer => match ints(value)[0] {
            NA_INTEGER => f64::NAN,
            count => f64::from(count),
        },
        SexpType::Real => reals(value)[0],
        _ => reals(coerce_vector(value, SexpType::Real)?)[0],
    };
    if count.is_nan() || !(0.0..=4.503599627370496e15).contains(&count) {
        return Err(error("invalid arguments"));
    }
    Ok(count as usize)
}

/// The drawn deviates, as integers when the routine counts and every count fits.
unsafe fn random_result(drawn: Sexp, integer: bool) -> R<Sexp> {
    let fits = |value: &f64| value.is_nan() || value.abs() <= f64::from(i32::MAX);
    if !integer || !reals(drawn).iter().all(fits) {
        return Ok(drawn);
    }
    let result = alloc_uninit(SexpType::Integer, length(drawn))?;
    for (slot, value) in ints_mut(result).iter_mut().zip(reals(drawn)) {
        *slot = if value.is_nan() {
            NA_INTEGER
        } else {
            *value as i32
        };
    }
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

/// `rnorm` for a batch, which draws all the standard normals in one pass when the parameters
/// make every element a draw.
struct NormalSampler {
    mu: f64,
    sigma: f64,
}

impl Sampler for NormalSampler {
    fn prepare(parameters: &[f64; 3]) -> Self {
        Self {
            mu: parameters[0],
            sigma: parameters[1],
        }
    }

    #[inline(always)]
    fn draw<D: Deviates>(&mut self, deviates: &mut D) -> R<f64> {
        rnorm(deviates, self.mu, self.sigma)
    }

    fn fill<D: Deviates>(&mut self, deviates: &mut D, slots: &mut [f64]) -> R<()> {
        let Self { mu, sigma } = *self;
        // Exactly the parameters for which `rnorm` draws a deviate.
        if !mu.is_finite() || !sigma.is_finite() || sigma <= 0.0 {
            for slot in slots {
                *slot = rnorm(deviates, mu, sigma)?;
            }
            return Ok(());
        }
        deviates.fill_normal(slots)?;
        for slot in slots {
            *slot = mu + sigma * *slot;
        }
        Ok(())
    }
}

/// `runif` for a batch, which draws all the uniforms in one pass when the generator is known
/// to stay inside (0, 1), so that `runif`'s rejection loop never repeats.
struct UniformSampler {
    a: f64,
    b: f64,
}

impl Sampler for UniformSampler {
    fn prepare(parameters: &[f64; 3]) -> Self {
        Self {
            a: parameters[0],
            b: parameters[1],
        }
    }

    #[inline(always)]
    fn draw<D: Deviates>(&mut self, deviates: &mut D) -> R<f64> {
        runif(deviates, self.a, self.b)
    }

    fn fill<D: Deviates>(&mut self, deviates: &mut D, slots: &mut [f64]) -> R<()> {
        let Self { a, b } = *self;
        // Exactly the parameters for which `runif` draws a deviate.
        if a.is_finite() && b.is_finite() && b > a && deviates.fill_uniform(slots)? {
            for slot in slots {
                *slot = a + (b - a) * *slot;
            }
            return Ok(());
        }
        for slot in slots {
            *slot = runif(deviates, a, b)?;
        }
        Ok(())
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
    Ok(location + scale * (M_PI * deviates.uniform()?).tan())
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

/// A noncentral beta deviate from two chi-squares, as GNU R's `rnbeta`.
pub fn rnbeta(deviates: &mut impl Deviates, a: f64, b: f64, ncp: f64) -> R<f64> {
    let numerator = rnchisq(deviates, 2.0 * a, ncp)?;
    Ok(numerator / (numerator + rchisq(deviates, 2.0 * b)?))
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
    let d = M_SQRT_32 - s * 12.0;
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
    let exponent_limit = f64::from(f64::MAX_EXP) * M_LN2;
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
pub fn rpois(deviates: &mut impl Deviates, mu: f64) -> R<f64> {
    Poisson::new(mu).draw(deviates)
}

/// [`rpois`] prepared for one mean, holding what GNU R keeps in static variables between calls
/// with the same mean: the table of cumulative probabilities filled so far, and the constants
/// of the normal approximation.
pub struct Poisson {
    kind: PoissonKind,
}

// The table lives inline because a sampler is a stack local built once per call (or once per
// change of mean), and boxing it would add an allocation to every small `rpois` call.
#[allow(clippy::large_enum_variant)]
enum PoissonKind {
    /// The result for every draw, without consuming the generator.
    Constant(f64),
    Table(PoissonTable),
    Normal(PoissonNormal),
}

/// Inversion for a mean below 10, against the cumulative probabilities of `0..=35`, which fill
/// in as draws need them.
struct PoissonTable {
    mu: f64,
    p0: f64,
    mode: usize,
    probability: f64,
    sum: f64,
    filled: usize,
    cumulative: [f64; 36],
}

struct PoissonNormal {
    mu: f64,
    s: f64,
    d: f64,
    big_l: f64,
    /// The constants of the quotient and hat tests, which most draws never reach.
    hat: Option<PoissonHat>,
}

struct PoissonHat {
    omega: f64,
    c0: f64,
    c1: f64,
    c2: f64,
    c3: f64,
    c: f64,
}

impl Poisson {
    pub fn new(mu: f64) -> Self {
        let kind = if !mu.is_finite() || mu < 0.0 {
            PoissonKind::Constant(f64::NAN)
        } else if mu <= 0.0 {
            PoissonKind::Constant(0.0)
        } else if mu < 10.0 {
            let p0 = (-mu).exp();
            PoissonKind::Table(PoissonTable {
                mu,
                p0,
                mode: (mu as i64).max(1) as usize,
                probability: p0,
                sum: p0,
                filled: 0,
                cumulative: [0.0; 36],
            })
        } else {
            PoissonKind::Normal(PoissonNormal {
                mu,
                s: mu.sqrt(),
                d: 6.0 * mu * mu,
                big_l: (mu - 1.1484).floor(),
                hat: None,
            })
        };
        Self { kind }
    }

    pub fn draw(&mut self, deviates: &mut impl Deviates) -> R<f64> {
        match &mut self.kind {
            PoissonKind::Constant(value) => Ok(*value),
            PoissonKind::Table(table) => table.draw(deviates),
            PoissonKind::Normal(normal) => normal.draw(deviates),
        }
    }
}

impl Sampler for Poisson {
    fn prepare(parameters: &[f64; 3]) -> Self {
        Self::new(parameters[0])
    }

    #[inline(always)]
    fn draw<D: Deviates>(&mut self, deviates: &mut D) -> R<f64> {
        Poisson::draw(self, deviates)
    }
}

impl PoissonTable {
    #[inline]
    fn draw(&mut self, deviates: &mut impl Deviates) -> R<f64> {
        loop {
            let u = deviates.uniform()?;
            if u <= self.p0 {
                return Ok(0.0);
            }
            let filled = self.filled;
            if filled > 0 {
                let first = if u > 0.458 { filled.min(self.mode) } else { 1 };
                if let Some(k) = (first..=filled).find(|&k| u <= self.cumulative[k]) {
                    return Ok(k as f64);
                }
                if filled == 35 {
                    continue;
                }
            }
            for k in filled + 1..=35 {
                self.probability *= self.mu / k as f64;
                self.sum += self.probability;
                self.cumulative[k] = self.sum;
                if u <= self.sum {
                    self.filled = k;
                    return Ok(k as f64);
                }
            }
            self.filled = 35;
        }
    }
}

impl PoissonNormal {
    #[allow(clippy::excessive_precision)]
    fn draw(&mut self, deviates: &mut impl Deviates) -> R<f64> {
        let Self {
            mu, s, d, big_l, ..
        } = *self;
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
        let hat = self.hat.get_or_insert_with(|| {
            let b1 = 1.0 / 24.0 / mu;
            let b2 = 0.3 * b1 * b1;
            let c3 = 1.0 / 7.0 * b1 * b2;
            let c2 = b2 - 15.0 * c3;
            let c1 = b1 - 6.0 * b2 + 45.0 * c3;
            let c0 = 1.0 - b1 + 3.0 * b2 - 15.0 * c3;
            PoissonHat {
                omega: M_1_SQRT_2PI / s,
                c0,
                c1,
                c2,
                c3,
                c: 0.1069 / mu,
            }
        });
        if normal >= 0.0 {
            let (px, py, fx, fy) = poisson_probabilities(mu, s, hat, count, difference);
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
                let (px, py, fx, fy) = poisson_probabilities(mu, s, hat, count, difference);
                if hat.c * u.abs() <= py * (px + e).exp() - fy * (fx + e).exp() {
                    return Ok(count);
                }
            }
        }
    }
}

/// The log Poisson probability and the discrete normal approximation at `count`.
#[allow(clippy::excessive_precision)]
fn poisson_probabilities(
    mu: f64,
    s: f64,
    hat: &PoissonHat,
    count: f64,
    difference: f64,
) -> (f64, f64, f64, f64) {
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
                * (((((((A7 * v + A6) * v + A5) * v + A4) * v + A3) * v + A2) * v + A1) * v + A0)
                - delta
        } else {
            count * v.ln_1p() - difference - delta
        };
        (px, M_1_SQRT_2PI / count.sqrt())
    };
    let x = (0.5 - difference) / s;
    let square = x * x;
    let fx = -0.5 * square;
    let fy = hat.omega * (((hat.c3 * square + hat.c2) * square + hat.c1) * square + hat.c0);
    (px, py, fx, fy)
}

/// Binomial deviates by Kachitvichyanukul and Schmeiser's (1988) algorithm BTPE: inversion when
/// the mean is below 30, and otherwise a triangle, parallelogram and two exponential tails as
/// the hat, with a squeeze and Stirling's formula in the acceptance test.
pub fn rbinom(deviates: &mut impl Deviates, size: f64, pp: f64) -> R<f64> {
    Binomial::new(size, pp).draw(deviates)
}

/// [`rbinom`] prepared for one size and probability, holding the setup GNU R keeps in static
/// variables between calls with the same parameters.
pub struct Binomial {
    kind: BinomialKind,
}

enum BinomialKind {
    /// The result for every draw, without consuming the generator.
    Constant(f64),
    /// Beyond the integer range of the algorithm, the distribution function inverted.
    Quantile {
        size: f64,
        probability: f64,
    },
    Inversion(BinomialInversion),
    Btpe(Btpe),
}

/// The common part of both algorithms: the size, the smaller of the two probabilities, and
/// whether the count is of the other outcome.
struct BinomialShape {
    n: i64,
    flip: bool,
    g: f64,
    r: f64,
}

struct BinomialInversion {
    shape: BinomialShape,
    qn: f64,
}

struct Btpe {
    shape: BinomialShape,
    nf: f64,
    p: f64,
    q: f64,
    m: i64,
    mf: f64,
    npq: f64,
    p1: f64,
    xm: f64,
    xl: f64,
    xr: f64,
    c: f64,
    xll: f64,
    xlr: f64,
    p2: f64,
    p3: f64,
    p4: f64,
}

impl Binomial {
    pub fn new(size: f64, pp: f64) -> Self {
        Self {
            kind: binomial_kind(size, pp),
        }
    }

    pub fn draw(&mut self, deviates: &mut impl Deviates) -> R<f64> {
        let (shape, draw) = match &self.kind {
            BinomialKind::Constant(value) => return Ok(*value),
            BinomialKind::Quantile { size, probability } => {
                return Ok(qbinom(
                    deviates.uniform()?,
                    *size,
                    *probability,
                    false,
                    false,
                ));
            }
            BinomialKind::Inversion(inversion) => (&inversion.shape, inversion.draw(deviates)?),
            BinomialKind::Btpe(btpe) => (&btpe.shape, btpe.draw(deviates)?),
        };
        let draw = if shape.flip { shape.n - draw } else { draw };
        Ok(draw as f64)
    }
}

impl Sampler for Binomial {
    fn prepare(parameters: &[f64; 3]) -> Self {
        Self::new(parameters[0], parameters[1])
    }

    #[inline(always)]
    fn draw<D: Deviates>(&mut self, deviates: &mut D) -> R<f64> {
        Binomial::draw(self, deviates)
    }
}

fn binomial_kind(size: f64, pp: f64) -> BinomialKind {
    if !size.is_finite() {
        return BinomialKind::Constant(f64::NAN);
    }
    let rounded = forceint(size);
    if rounded != size || !pp.is_finite() || rounded < 0.0 || !(0.0..=1.0).contains(&pp) {
        return BinomialKind::Constant(f64::NAN);
    }
    if rounded == 0.0 || pp == 0.0 {
        return BinomialKind::Constant(0.0);
    }
    if pp == 1.0 {
        return BinomialKind::Constant(rounded);
    }
    if rounded >= f64::from(i32::MAX) {
        return BinomialKind::Quantile {
            size: rounded,
            probability: pp,
        };
    }
    let n = rounded as i64;
    let nf = rounded;
    let p = pp.min(1.0 - pp);
    let q = 1.0 - p;
    let np = nf * p;
    let r = p / q;
    let shape = BinomialShape {
        n,
        flip: pp > 0.5,
        g: r * (nf + 1.0),
        r,
    };
    if np < 30.0 {
        return BinomialKind::Inversion(BinomialInversion {
            shape,
            qn: q.powf(nf),
        });
    }
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
    BinomialKind::Btpe(Btpe {
        shape,
        nf,
        p,
        q,
        m,
        mf,
        npq,
        p1,
        xm,
        xl,
        xr,
        c,
        xll,
        xlr,
        p2,
        p3,
        p4,
    })
}

impl BinomialInversion {
    #[inline]
    fn draw(&self, deviates: &mut impl Deviates) -> R<i64> {
        let BinomialShape { g, r, .. } = self.shape;
        loop {
            let mut ix = 0i64;
            let mut f = self.qn;
            let mut u = deviates.uniform()?;
            loop {
                if u < f {
                    return Ok(ix);
                }
                if ix > 110 {
                    break;
                }
                u -= f;
                ix += 1;
                f *= g / ix as f64 - r;
            }
        }
    }
}

impl Btpe {
    fn draw(&self, deviates: &mut impl Deviates) -> R<i64> {
        let Self {
            shape: BinomialShape { n, g, r, .. },
            nf,
            p,
            q,
            m,
            mf,
            npq,
            p1,
            xm,
            xl,
            xr,
            c,
            xll,
            xlr,
            p2,
            p3,
            p4,
        } = *self;
        loop {
            let u = deviates.uniform()? * p4;
            let mut v = deviates.uniform()?;
            let ix;
            if u <= p1 {
                return Ok((xm - p1 * v + u) as i64);
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
                    return Ok(ix);
                }
            } else {
                let amaxp = (kf / npq) * ((kf * (kf / 3.0 + 0.625) + 0.1666666666666) / npq + 0.5);
                let ynorm = -kf * kf / (2.0 * npq);
                let alv = v.ln();
                if alv < ynorm - amaxp {
                    return Ok(ix);
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
                        return Ok(ix);
                    }
                }
            }
        }
    }
}

/// `log(i!)`, exactly for small `i` and by Stirling's series beyond.
#[allow(clippy::excessive_precision)]
fn log_factorial(i: f64) -> f64 {
    const SMALL: [f64; 8] = [
        0.0,
        0.0,
        M_LN2,
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
    (i + 0.5) * i.ln() - i + M_LN_SQRT_2PI + (0.0833333333333333 - 0.00277777777777778 / square) / i
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
    let (red, black, drawn) = (forceint(red), forceint(black), forceint(drawn));
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
    let n = forceint(n);
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
    let (m, n) = (forceint(m), forceint(n));
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
/// binomial draws conditional on the boxes before. The remaining mass starts at the sum of the
/// probabilities, added in order as GNU R does, so probabilities that sum to one only within
/// rounding give GNU R's conditionals.
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
    // GNU R keeps the remaining probability in a `long double`. In plain `f64` ten draws of 0.1
    // leave a conditional probability a hair above 0.5, which makes `rbinom` draw the
    // complement and changes the counts, so the sum is carried as an unevaluated pair of
    // doubles (`high + low`) that is at least as precise.
    let (mut high, mut low) = (0.0, 0.0);
    for &probability in probabilities {
        (high, low) = two_sum_pair(high, low, probability);
    }
    for (index, &probability) in probabilities[..last].iter().enumerate() {
        if probability > 0.0 {
            let quotient = probability / high;
            let residual = (-quotient).mul_add(high, probability) - quotient * low;
            let conditional = quotient + residual / high;
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
        (high, low) = two_sum_pair(high, low, -probability);
    }
    counts[last] = left;
    Ok(())
}

/// `high + low + value` as a normalized pair of doubles, by Knuth's two-sum.
fn two_sum_pair(high: f64, low: f64, value: f64) -> (f64, f64) {
    let sum = high + value;
    let virtual_value = sum - high;
    let error = (high - (sum - virtual_value)) + (value - virtual_value);
    let low = low + error;
    let total = sum + low;
    (total, low - (total - sum))
}

#[cfg(test)]
#[allow(clippy::excessive_precision)]
mod tests {
    use {
        super::*,
        crate::random::SeededDeviates,
        syntax::testing::{SplitMix64, for_each_seed},
    };

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
            // The exact value: GNU R stops summing at an absolute error of 1e-9 and prints
            // 0.38769832015372552.
            (pnbeta(0.4, 2.0, 3.0, 1.5, true, false), 0.38769832028689006),
            (
                ptukey(3.5, 1.0, 4.0, 20.0, true, false),
                0.90504154945144333,
            ),
            (pweibull(2.0, 1.5, 3.0, true, false), 0.41977020402532716),
            (qcauchy(0.9, 0.0, 1.0, true, false), 3.0776835371752544),
            (plogis(2.0, 0.0, 1.0, false, true), -2.1269280110429727),
            (bessel_j(10.0, 1.5), 0.19798249275589311),
            (bessel_y(2.5, 0.0), 0.49807035961523194),
            (bessel_i(1.9, 3.7, 1.0), 0.06474585856532572),
            (bessel_k(5.0, 0.5, 2.0), 0.56049912163979287),
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

    fn uniform(picks: &mut SplitMix64) -> f64 {
        (picks.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A probability from the central region, the tails, the boundaries, outside [0, 1], or NA.
    fn probability(picks: &mut SplitMix64) -> f64 {
        // `exp(-25)` and `exp(-729)` sit where the tail switches approximations (`r = 5`, `r =
        // 27`).
        const SPECIAL: [f64; 15] = [
            1.388_794_386_496_402e-11,
            1.0e-316,
            1.0e-300,
            0.0,
            1.0,
            0.5,
            0.075,
            0.925,
            0.07499999999999999,
            0.925_000_000_000_001,
            -0.1,
            1.1,
            f64::NAN,
            NA_REAL,
            f64::INFINITY,
        ];
        match picks.below(5) {
            0 => SPECIAL[picks.below(SPECIAL.len())],
            1 => uniform(picks) * 0.08,
            2 => 1.0 - uniform(picks) * 0.08,
            3 => (-uniform(picks) * 800.0).exp(),
            _ => uniform(picks),
        }
    }

    /// The vector path of `qnorm`, with the central region across SIMD lanes, must give the
    /// scalar function's bits at every element.
    #[test]
    fn qnorm_lanes_match_the_scalar_function() {
        for_each_seed(300, |seed| {
            let mut picks = SplitMix64(0x9a07_0000 ^ seed);
            let length = picks.below(700);
            let probabilities: Vec<f64> = (0..length).map(|_| probability(&mut picks)).collect();
            let mu = (uniform(&mut picks) - 0.5) * 1e4;
            let sigma = (uniform(&mut picks) * 20.0 - 10.0).exp();
            let lower = picks.below(2) == 0;
            let flags = [i32::from(lower), 0];
            let mut out = vec![0.0; length];
            let nan_produced =
                Qnorm::fill_first(&probabilities, &[0.0, mu, sigma, 0.0], flags, &mut out)
                    .expect("finite parameters take the vector path");
            let mut expected_nan = false;
            for (&p, &got) in probabilities.iter().zip(&out) {
                let expected = if p.is_nan() {
                    na_or_nan(&[p])
                } else {
                    let value = qnorm(p, mu, sigma, lower, false);
                    expected_nan |= value.is_nan();
                    value
                };
                assert_eq!(
                    got.to_bits(),
                    expected.to_bits(),
                    "p = {p:e}, mu = {mu}, sigma = {sigma}"
                );
            }
            assert_eq!(nan_produced, expected_nan);
        });
    }

    /// The recycling loop's wrapping positions must pick the operands `element % length` picks.
    #[test]
    fn recycling_matches_modular_indexing() {
        for_each_seed(300, |seed| {
            let mut picks = SplitMix64(0x7ec7_0000 ^ seed);
            let operands: Vec<Vec<f64>> = (0..3)
                .map(|_| {
                    let length = 1 + picks.below(9);
                    (0..length)
                        .map(|_| match picks.below(8) {
                            0 => NA_REAL,
                            1 => f64::NAN,
                            2 => -1.0,
                            _ => (uniform(&mut picks) - 0.3) * 5.0,
                        })
                        .collect()
                })
                .collect();
            let count = operands.iter().map(Vec::len).max().unwrap_or(0);
            let values = [
                &operands[0][..],
                &operands[1][..],
                &operands[2][..],
                &[][..],
            ];
            let flags = [picks.below(2) as i32, 0];
            let mut out = vec![0.0; count];
            let nan_produced = math_loop::<Dnorm>(&values, flags, &mut out);
            let mut expected_nan = false;
            for (element, &got) in out.iter().enumerate() {
                let mut arguments = [0.0; 4];
                for (slot, operand) in arguments.iter_mut().zip(&operands) {
                    *slot = operand[element % operand.len()];
                }
                let expected = if arguments[..3].iter().any(|value| value.is_nan()) {
                    na_or_nan(&arguments[..3])
                } else {
                    let value = Dnorm::apply(&arguments, flags);
                    expected_nan |= value.is_nan();
                    value
                };
                assert_eq!(got.to_bits(), expected.to_bits(), "element {element}");
            }
            assert_eq!(nan_produced, expected_nan);
        });
    }

    /// A sampler prepared once and reused, for scalar or recycled parameters, must draw what a
    /// fresh call of the generator per element draws.
    #[test]
    fn prepared_samplers_match_fresh_draws() {
        for_each_seed(200, |seed| {
            let mut picks = SplitMix64(0x5a3b_0000 ^ seed);
            let length = picks.below(400);
            let means: Vec<f64> = (0..1 + picks.below(4))
                .map(|_| match picks.below(6) {
                    0 => uniform(&mut picks) * 10.0,
                    1 => 10.0 + uniform(&mut picks) * 1e4,
                    2 => [0.0, -1.0, f64::NAN, f64::INFINITY, 9.999, 10.0][picks.below(6)],
                    _ => uniform(&mut picks) * 40.0,
                })
                .collect();
            let sizes: Vec<f64> = (0..1 + picks.below(3))
                .map(|_| match picks.below(5) {
                    0 => [0.0, 1.5, -1.0, f64::NAN, 2147483647.0][picks.below(5)],
                    _ => picks.below(5000) as f64,
                })
                .collect();
            let probabilities: Vec<f64> = (0..1 + picks.below(3))
                .map(|_| match picks.below(5) {
                    0 => [0.0, 1.0, 0.5, f64::NAN, 1.5][picks.below(5)],
                    _ => uniform(&mut picks),
                })
                .collect();
            let draw_seed = seed as u32;
            let check =
                |batched: &dyn Fn(&mut SeededDeviates, &mut [f64]) -> R<()>,
                 single: &dyn Fn(&mut SeededDeviates, usize) -> R<f64>| {
                    let mut deviates = SeededDeviates::new(draw_seed);
                    let mut slots = vec![0.0; length];
                    batched(&mut deviates, &mut slots).expect("valid draws");
                    let mut deviates = SeededDeviates::new(draw_seed);
                    for (element, &got) in slots.iter().enumerate() {
                        let expected = single(&mut deviates, element).expect("valid draws");
                        assert_eq!(got.to_bits(), expected.to_bits(), "element {element}");
                    }
                };
            check(
                &|deviates, slots| draw_all::<Poisson, _>(deviates, &[&means[..]], slots),
                &|deviates, element| rpois(deviates, means[element % means.len()]),
            );
            check(
                &|deviates, slots| Poisson::new(means[0]).fill(deviates, slots),
                &|deviates, _| rpois(deviates, means[0]),
            );
            check(
                &|deviates, slots| {
                    draw_all::<Binomial, _>(deviates, &[&sizes[..], &probabilities[..]], slots)
                },
                &|deviates, element| {
                    rbinom(
                        deviates,
                        sizes[element % sizes.len()],
                        probabilities[element % probabilities.len()],
                    )
                },
            );
            check(
                &|deviates, slots| {
                    draw_all::<NormalSampler, _>(deviates, &[&means[..], &probabilities[..]], slots)
                },
                &|deviates, element| {
                    rnorm(
                        deviates,
                        means[element % means.len()],
                        probabilities[element % probabilities.len()],
                    )
                },
            );
            check(
                &|deviates, slots| {
                    NormalSampler::prepare(&[means[0], sizes[0], 0.0]).fill(deviates, slots)
                },
                &|deviates, _| rnorm(deviates, means[0], sizes[0]),
            );
            check(
                &|deviates, slots| {
                    UniformSampler::prepare(&[-means[0], sizes[0], 0.0]).fill(deviates, slots)
                },
                &|deviates, _| runif(deviates, -means[0], sizes[0]),
            );
        });
    }
}
