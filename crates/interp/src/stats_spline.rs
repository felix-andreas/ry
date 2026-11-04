// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's C and Fortran sources, and
// keep their copyright notices:
//
// GNU R: src/library/stats/src/sbart.c (sbart)
//     Derived from the GAMFIT sources, which derive from code by Finbarr O'Sullivan; the file
//     carries no copyright line of its own
//
// GNU R: src/library/stats/src/qsbart.f (rbart)
//     Copyright (C) 1998-2016 The R Core Team
//
// GNU R: src/library/stats/src/sgram.f, stxwx.f, sslvrg.f, sinerp.f (sgram, stxwx, sslvrg, sinerp)
//     Output from Public domain Ratfor, version 1.0; no copyright line
//
// GNU R: src/library/stats/src/bsplvd.f, bvalue.f (bsplvd, bsplvb, bvalue)
//     Based on C. de Boor's "A Practical Guide to Splines" and the public domain GAMFIT
//     package; no copyright line
//
// GNU R: src/appl/interv.c (findInterval)
//     Copyright (C) 2002--2025 The R Core Team
//
// GNU R: src/appl/dpbfa.f, dpbsl.f (dpbfa, dpbsl)
//     LINPACK, by Cleve Moler; public domain
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! Penalized regression splines behind `smooth.spline()` (`rbart`) and their evaluation in
//! `predict()` (`bvalus`).
//!
//! The fit is a cubic B-spline `f = sum(coef[j] B[j])` on knots scaled to [0, 1] that
//! minimizes `sum(w (y - f(x))^2) + lambda * integral(f''^2)`. The routines keep the order of
//! every floating-point operation of GNU R's Fortran, because the smoothing parameter is found
//! by a search whose path depends on the last bits of the criterion. The coefficients, fitted
//! values and leverages returned are those of the last evaluation of the search, not of the
//! parameter it settles on, exactly as GNU R returns them.
//!
//! Index arithmetic is written in the Fortran's 1-based form (`at`, `col`), because the band
//! storage and the interval search depend on it.

use {
    crate::{
        builtin_libraries::NativeCall,
        stats::{c_arguments, c_return},
        unwind::{R, error},
        values,
    },
    heap::object::Sexp,
};

/// The bandwidth of the normal equations: three super-diagonals beside the main one.
const BAND: usize = 3;
/// Rows of the band storage, `BAND + 1`.
const LD4: usize = 4;
const BIG_CRITERION: f64 = 1e100;
/// The squared inverse of the golden ratio, `(3 - sqrt(5)) / 2`.
const GOLDEN: f64 = 0.381966011250105151795413165634;

/// `.Fortran(C_rbart, penalty, dofoff, x, y, w, ssw, n, knot, nk, coef, ty, lev, crit, iparms,
/// spar, parms, scratch, ld4, ldnk, ier)`: fits the smoothing spline, choosing `spar` by the
/// criterion in `iparms` unless it is given.
pub unsafe fn rbart(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let real = |index: usize| {
        values::reals(arguments[index])
            .first()
            .copied()
            .unwrap_or(0.0)
    };
    let n = values::as_integer_scalar(arguments[6]).max(0) as usize;
    let count = values::as_integer_scalar(arguments[8]).max(0) as usize;
    let first_n = |index: usize| {
        values::reals(arguments[index])
            .get(..n)
            .ok_or_else(|| error("'n' is larger than the data given to 'rbart'"))
    };
    let (x, y, raw_weights) = (first_n(2)?, first_n(3)?, first_n(4)?);
    let knots = values::reals(arguments[7]).to_vec();
    let int_parameters = values::ints(arguments[13]).to_vec();
    let parameters = values::reals(arguments[15]).to_vec();
    if knots.len() < count + 4 || count < 4 || parameters.len() < 5 || int_parameters.len() < 4 {
        return Err(error("invalid knots for the smoothing spline"));
    }
    let criterion_code = int_parameters[0];
    let spar_given = int_parameters[1] == 1;
    let max_iterations = int_parameters[2];
    let spar_is_lambda = int_parameters[3] == 1;

    // The Fortran squares the weights again, so it works with their roots.
    let weights: Vec<f64> = raw_weights
        .iter()
        .map(|&weight| if weight > 0.0 { weight.sqrt() } else { weight })
        .collect();
    let mut smoother = Smoother::new(
        x,
        y,
        &weights,
        &knots,
        count,
        real(0),
        real(1),
        real(5),
        criterion_code,
    );
    smoother.crit = values::reals(arguments[12]).first().copied().unwrap_or(0.0);
    smoother.spar_is_lambda = spar_is_lambda;
    smoother.setup();

    let mut spar = real(14);
    let mut iterations = max_iterations;
    if spar_given {
        smoother.evaluate(spar);
    } else {
        let search = SearchControl {
            lower: parameters[0],
            upper: parameters[1],
            tolerance: parameters[2],
            eps: parameters[3],
            max_iterations,
        };
        let (minimum, used) = smoother.minimize(&search);
        spar = minimum;
        iterations = used;
    }

    values::reals_mut(arguments[9])[..count].copy_from_slice(&smoother.coef);
    values::reals_mut(arguments[10])[..n].copy_from_slice(&smoother.fitted);
    if smoother.criterion_code >= 1 {
        let leverages = values::reals_mut(arguments[11]);
        let length = leverages.len().min(n);
        leverages[..length].copy_from_slice(&smoother.leverage[..length]);
    }
    values::reals_mut(arguments[12])[0] = smoother.crit;
    if !spar_given && let Some(slot) = values::ints_mut(arguments[13]).get_mut(2) {
        *slot = iterations;
    }
    values::reals_mut(arguments[14])[0] = spar;
    let parameters = values::reals_mut(arguments[15]);
    parameters[0] = smoother.lambda;
    parameters[4] = smoother.ratio;
    let scratch = values::reals_mut(arguments[16]);
    if scratch.len() >= 13 * count {
        scratch[..count].copy_from_slice(&smoother.xwy);
        for offset in 0..4 {
            let start = (1 + offset) * count;
            scratch[start..start + count].copy_from_slice(&smoother.gram[offset]);
            let start = (5 + offset) * count;
            scratch[start..start + count].copy_from_slice(&smoother.penalty_matrix[offset]);
        }
        scratch[9 * count..13 * count].copy_from_slice(&smoother.abd);
    }
    values::ints_mut(arguments[19])[0] = smoother.info;
    c_return(call, &arguments)
}

/// `.Fortran(C_bvalus, n, knot, coef, nk, x, s, order)`: the spline, or its derivative of the
/// given order, at each `x` in [0, 1].
pub unsafe fn bvalus(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let n = values::as_integer_scalar(arguments[0]).max(0) as usize;
    let count = values::as_integer_scalar(arguments[3]).max(0) as usize;
    let knots = values::reals(arguments[1]).to_vec();
    let coefficients = values::reals(arguments[2]).to_vec();
    let order = values::as_integer_scalar(arguments[6]).clamp(0, 4) as usize;
    if knots.len() < count + 4 || coefficients.len() < count || count < 4 {
        return Err(error("invalid knots for the smoothing spline"));
    }
    let x = values::reals(arguments[4]).to_vec();
    let values_out = values::reals_mut(arguments[5]);
    for (index, &point) in x.iter().enumerate().take(n) {
        values_out[index] = bvalue(&knots, &coefficients, count, 4, point, order);
    }
    c_return(call, &arguments)
}

struct SearchControl {
    lower: f64,
    upper: f64,
    tolerance: f64,
    eps: f64,
    max_iterations: i32,
}

/// The state `sbart` and `sslvrg` share, kept between evaluations because a failed evaluation
/// leaves the previous criterion in place and the search reads it.
struct Smoother<'a> {
    x: &'a [f64],
    y: &'a [f64],
    /// The square roots of the observation weights.
    weights: &'a [f64],
    knots: &'a [f64],
    count: usize,
    penalty: f64,
    dof_offset: f64,
    within_ss: f64,
    criterion_code: i32,
    spar_is_lambda: bool,
    ratio: f64,
    lambda: f64,
    xwy: Vec<f64>,
    /// The four diagonals of X'WX.
    gram: [Vec<f64>; 4],
    /// The four diagonals of the roughness matrix SIGMA.
    penalty_matrix: [Vec<f64>; 4],
    /// `[X'WX + lambda SIGMA]`, and then its Cholesky factor, in `LD4` by `count` band storage.
    abd: Vec<f64>,
    /// The band of the inverse's inner products, in the same storage.
    inner_products: Vec<f64>,
    coef: Vec<f64>,
    fitted: Vec<f64>,
    leverage: Vec<f64>,
    crit: f64,
    info: i32,
}

impl<'a> Smoother<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        x: &'a [f64],
        y: &'a [f64],
        weights: &'a [f64],
        knots: &'a [f64],
        count: usize,
        penalty: f64,
        dof_offset: f64,
        within_ss: f64,
        criterion_code: i32,
    ) -> Smoother<'a> {
        Smoother {
            x,
            y,
            weights,
            knots,
            count,
            penalty,
            dof_offset,
            within_ss,
            criterion_code,
            spar_is_lambda: false,
            ratio: 1.0,
            lambda: 0.0,
            xwy: vec![0.0; count],
            gram: std::array::from_fn(|_| vec![0.0; count]),
            penalty_matrix: std::array::from_fn(|_| vec![0.0; count]),
            abd: vec![0.0; LD4 * count],
            inner_products: vec![0.0; LD4 * count],
            coef: vec![0.0; count],
            fitted: vec![0.0; x.len()],
            leverage: vec![0.0; x.len()],
            crit: 0.0,
            info: 0,
        }
    }

    /// `sgram` and `stxwx`, and the trace ratio that scales lambda.
    fn setup(&mut self) {
        self.penalty_matrix = roughness_matrix(self.knots, self.count);
        let (xwy, gram) = normal_equations(self.x, self.y, self.weights, self.knots, self.count);
        self.xwy = xwy;
        self.gram = gram;
        if !self.spar_is_lambda {
            let (mut gram_trace, mut penalty_trace) = (0.0, 0.0);
            for index in 2..self.count - 3 {
                gram_trace += self.gram[0][index];
                penalty_trace += self.penalty_matrix[0][index];
            }
            self.ratio = gram_trace / penalty_trace;
        }
    }

    /// The `SSPLINE_COMP` macro: lambda from `spar`, then the fit and criterion for it.
    fn evaluate(&mut self, spar: f64) {
        self.lambda = if self.spar_is_lambda {
            spar
        } else {
            self.ratio * r_pow(16.0, spar * 6.0 - 2.0)
        };
        self.fit(self.lambda);
    }

    /// Brent's minimizer in the form `sbart` inlines, over `spar`. Returns the abscissa and the
    /// number of iterations.
    fn minimize(&mut self, search: &SearchControl) -> (f64, i32) {
        let (mut a, mut b) = (search.lower, search.upper);
        let mut iterations = 0;
        let mut v = a + GOLDEN * (b - a);
        let mut w = v;
        let mut x = v;
        let mut e = 0.0_f64;
        let mut d = 0.0_f64;
        self.evaluate(x);
        let mut fx = self.crit;
        let mut fv = fx;
        let mut fw = fx;
        while self.info == 0 {
            let xm = (a + b) * 0.5;
            let tol1 = search.eps * x.abs() + search.tolerance / 3.0;
            let tol2 = tol1 * 2.0;
            iterations += 1;
            if (x - xm).abs() <= tol2 - (b - a) * 0.5 || iterations > search.max_iterations {
                break;
            }

            let mut golden = e.abs() <= tol1
                || fx >= BIG_CRITERION
                || fv >= BIG_CRITERION
                || fw >= BIG_CRITERION;
            if !golden {
                let mut r = (x - w) * (fx - fv);
                let mut q = (x - v) * (fx - fw);
                let mut p = (x - v) * q - (x - w) * r;
                q = (q - r) * 2.0;
                if q > 0.0 {
                    p = -p;
                }
                q = q.abs();
                r = e;
                e = d;
                if p.abs() >= (0.5 * q * r).abs()
                    || q == 0.0
                    || p <= q * (a - x)
                    || p >= q * (b - x)
                {
                    golden = true;
                } else {
                    d = p / q;
                    let u = x + d;
                    if u - a < tol2 || b - u < tol2 {
                        d = fsign(tol1, xm - x);
                    }
                }
            }
            if golden {
                e = if x >= xm { a - x } else { b - x };
                d = GOLDEN * e;
            }

            let u = x + if d.abs() >= tol1 { d } else { fsign(tol1, d) };
            self.evaluate(u);
            let mut fu = self.crit;
            if !fu.is_finite() {
                fu = 2.0 * BIG_CRITERION;
            }

            if fu <= fx {
                if u >= x {
                    a = x;
                } else {
                    b = x;
                }
                v = w;
                fv = fw;
                w = x;
                fw = fx;
                x = u;
                fx = fu;
            } else {
                if u < x {
                    a = u;
                } else {
                    b = u;
                }
                if fu <= fw || w == x {
                    v = w;
                    fv = fw;
                    w = u;
                    fw = fu;
                } else if fu <= fv || v == x || v == w {
                    v = u;
                    fv = fu;
                }
            }
        }
        self.crit = fx;
        (x, iterations)
    }

    /// `sslvrg`: the spline for one lambda with its leverages and the selected criterion.
    fn fit(&mut self, lambda: f64) {
        let count = self.count;
        for index in 0..count {
            self.coef[index] = self.xwy[index];
            self.abd[at(4, index + 1)] =
                self.gram[0][index] + lambda * self.penalty_matrix[0][index];
        }
        for index in 0..count - 1 {
            self.abd[at(3, index + 2)] =
                self.gram[1][index] + lambda * self.penalty_matrix[1][index];
        }
        for index in 0..count - 2 {
            self.abd[at(2, index + 3)] =
                self.gram[2][index] + lambda * self.penalty_matrix[2][index];
        }
        for index in 0..count - 3 {
            self.abd[at(1, index + 4)] =
                self.gram[3][index] + lambda * self.penalty_matrix[3][index];
        }
        self.info = dpbfa(&mut self.abd, count);
        if self.info != 0 {
            return;
        }
        dpbsl(&self.abd, count, &mut self.coef);

        for (index, &point) in self.x.iter().enumerate() {
            self.fitted[index] = bvalue(self.knots, &self.coef, count, 4, point, 0);
        }
        if self.criterion_code < 1 {
            return;
        }

        sinerp(&self.abd, count, &mut self.inner_products);
        let eps = 1e-11;
        let knots = self.knots;
        for (index, &point) in self.x.iter().enumerate() {
            let mut x = point;
            let (mut left, flag) = find_interval(&knots[..count + 1], x);
            if flag == -1 {
                left = 4;
                x = knots[3] + eps;
            } else if flag == 1 {
                left = count;
                x = knots[count] - eps;
            }
            let j = left - 3;
            let basis = bsplvd(knots, 4, x, left, 1);
            let (b0, b1, b2, b3) = (basis[0], basis[1], basis[2], basis[3]);
            let p = |row: usize, column: usize| self.inner_products[at(row, column)];
            self.leverage[index] = (p(4, j) * (b0 * b0)
                + 2.0 * p(3, j) * b0 * b1
                + 2.0 * p(2, j) * b0 * b2
                + 2.0 * p(1, j) * b0 * b3
                + p(4, j + 1) * (b1 * b1)
                + 2.0 * p(3, j + 1) * b1 * b2
                + 2.0 * p(2, j + 1) * b1 * b3
                + p(4, j + 2) * (b2 * b2)
                + 2.0 * p(3, j + 2) * b2 * b3
                + p(4, j + 3) * (b3 * b3))
                * (self.weights[index] * self.weights[index]);
        }

        let n = self.x.len();
        let mut df = 0.0;
        match self.criterion_code {
            1 => {
                let mut rss = self.within_ss;
                let mut sum_weights = 0.0;
                for index in 0..n {
                    let scaled = (self.y[index] - self.fitted[index]) * self.weights[index];
                    rss += scaled * scaled;
                    df += self.leverage[index];
                    sum_weights += self.weights[index] * self.weights[index];
                }
                let shrink = 1.0 - (self.dof_offset + self.penalty * df) / sum_weights;
                self.crit = (rss / sum_weights) / (shrink * shrink);
            }
            2 => {
                self.crit = 0.0;
                for index in 0..n {
                    let ratio = ((self.y[index] - self.fitted[index]) * self.weights[index])
                        / (1.0 - self.leverage[index]);
                    self.crit += ratio * ratio;
                }
                self.crit /= n as f64;
            }
            code => {
                for index in 0..n {
                    df += self.leverage[index];
                }
                self.crit = if code == 3 {
                    let gap = self.dof_offset - df;
                    3.0 + gap * gap
                } else {
                    df - self.dof_offset
                };
            }
        }
    }
}

/// The position of Fortran element `(row, column)` of an array with `LD4` rows.
fn at(row: usize, column: usize) -> usize {
    (column - 1) * LD4 + (row - 1)
}

/// `R_pow`, as the search uses it for lambda.
fn r_pow(base: f64, exponent: f64) -> f64 {
    if base == 1.0 || exponent == 0.0 {
        1.0
    } else if exponent == 2.0 {
        base * base
    } else {
        base.powf(exponent)
    }
}

/// Fortran's `sign(a, b)`: the magnitude of `a` with the sign of `b`.
fn fsign(magnitude: f64, sign: f64) -> f64 {
    if sign >= 0.0 {
        magnitude.abs()
    } else {
        -magnitude.abs()
    }
}

/// `findInterval` with its default flags on the first `xt.len()` knots: the 1-based interval
/// with `xt[i] <= x < xt[i+1]` and flag 0, or 0 and -1 below the first knot, or `n` and 1 at or
/// above the last.
fn find_interval(xt: &[f64], x: f64) -> (usize, i32) {
    let at_or_below = xt.partition_point(|&knot| knot <= x);
    if at_or_below == 0 {
        (0, -1)
    } else if at_or_below >= xt.len() {
        (xt.len(), 1)
    } else {
        (at_or_below, 0)
    }
}

/// `stxwx`: X'W²y and the four diagonals of X'W²X.
fn normal_equations(
    x: &[f64],
    z: &[f64],
    w: &[f64],
    knots: &[f64],
    count: usize,
) -> (Vec<f64>, [Vec<f64>; 4]) {
    let mut y = vec![0.0; count];
    let mut hs: [Vec<f64>; 4] = std::array::from_fn(|_| vec![0.0; count]);
    let eps = 0.1e-9;
    for index in 0..x.len() {
        let (mut left, flag) = find_interval(&knots[..count + 1], x[index]);
        if flag == 1 {
            if x[index] <= knots[left - 1] + eps {
                left -= 1;
            } else {
                return (y, hs);
            }
        } else if flag == -1 {
            return (y, hs);
        }
        let v = bsplvd(knots, 4, x[index], left, 1);
        let weight = w[index] * w[index];
        let start = left - 4;
        for a in 0..4 {
            let j = start + a;
            y[j] += weight * z[index] * v[a];
            hs[0][j] += weight * (v[a] * v[a]);
            for b in a + 1..4 {
                hs[b - a][j] += weight * v[a] * v[b];
            }
        }
    }
    (y, hs)
}

/// `sgram`: the upper four diagonals of the integrals of products of second derivatives of the
/// B-splines, each derivative approximated as linear over a knot interval.
fn roughness_matrix(knots: &[f64], count: usize) -> [Vec<f64>; 4] {
    let mut sigma: [Vec<f64>; 4] = std::array::from_fn(|_| vec![0.0; count]);
    for i in 1..=count {
        let (left, _) = find_interval(&knots[..count + 1], knots[i - 1]);
        let at_left = bsplvd(knots, 4, knots[i - 1], left, 3);
        let first: [f64; 4] = std::array::from_fn(|ii| at_left[8 + ii]);
        let at_right = bsplvd(knots, 4, knots[i], left, 3);
        let slope: [f64; 4] = std::array::from_fn(|ii| at_right[8 + ii] - first[ii]);
        let width = knots[i] - knots[i - 1];
        let size = left.min(4);
        let base = left - size;
        for ii in 0..size {
            for jj in ii..size {
                let term = width
                    * (first[ii] * first[jj]
                        + (slope[ii] * first[jj] + slope[jj] * first[ii]) * 0.5
                        + slope[ii] * slope[jj] * 0.3330);
                sigma[jj - ii][base + ii] += term;
            }
        }
    }
    sigma
}

/// `dpbfa`: the Cholesky factor of a symmetric positive definite band matrix with `BAND`
/// super-diagonals, in place. Returns 0, or the 1-based column where it failed.
fn dpbfa(abd: &mut [f64], count: usize) -> i32 {
    for j in 1..=count {
        let mut s = 0.0;
        let mut ik = BAND + 1;
        let mut jk = j.saturating_sub(BAND).max(1);
        let mu = (BAND + 2).saturating_sub(j).max(1);
        for k in mu..=BAND {
            let mut dot = 0.0;
            for offset in 0..k - mu {
                dot += abd[at(ik + offset, jk)] * abd[at(mu + offset, j)];
            }
            let mut t = abd[at(k, j)] - dot;
            t /= abd[at(BAND + 1, jk)];
            abd[at(k, j)] = t;
            s += t * t;
            ik -= 1;
            jk += 1;
        }
        s = abd[at(BAND + 1, j)] - s;
        if s <= 0.0 {
            return j as i32;
        }
        abd[at(BAND + 1, j)] = s.sqrt();
    }
    0
}

/// `dpbsl`: solves with the factor `dpbfa` left, overwriting `b` with the solution.
fn dpbsl(abd: &[f64], count: usize, b: &mut [f64]) {
    for k in 1..=count {
        let lm = (k - 1).min(BAND);
        let la = BAND + 1 - lm;
        let lb = k - lm;
        let mut dot = 0.0;
        for offset in 0..lm {
            dot += abd[at(la + offset, k)] * b[lb - 1 + offset];
        }
        b[k - 1] = (b[k - 1] - dot) / abd[at(BAND + 1, k)];
    }
    for k in (1..=count).rev() {
        let lm = (k - 1).min(BAND);
        let la = BAND + 1 - lm;
        let lb = k - lm;
        b[k - 1] /= abd[at(BAND + 1, k)];
        let t = -b[k - 1];
        for offset in 0..lm {
            b[lb - 1 + offset] += t * abd[at(la + offset, k)];
        }
    }
}

/// `sinerp` with `flag = 0`: the inner products between columns of the inverse of the factor
/// within the band.
fn sinerp(abd: &[f64], count: usize, p1ip: &mut [f64]) {
    let (mut c1, mut c2, mut c3) = (0.0, 0.0, 0.0);
    let mut wjm3 = [0.0; 3];
    let mut wjm2 = [0.0; 2];
    let mut wjm1 = [0.0; 1];
    for i in 1..=count {
        let j = count - i + 1;
        let c0 = 1.0 / abd[at(4, j)];
        if j + 3 <= count {
            c1 = abd[at(1, j + 3)] * c0;
            c2 = abd[at(2, j + 2)] * c0;
            c3 = abd[at(3, j + 1)] * c0;
        } else if j + 2 == count {
            c1 = 0.0;
            c2 = abd[at(2, j + 2)] * c0;
            c3 = abd[at(3, j + 1)] * c0;
        } else if j + 1 == count {
            c1 = 0.0;
            c2 = 0.0;
            c3 = abd[at(3, j + 1)] * c0;
        } else if j == count {
            c1 = 0.0;
            c2 = 0.0;
            c3 = 0.0;
        }
        p1ip[at(1, j)] = 0.0 - (c1 * wjm3[0] + c2 * wjm3[1] + c3 * wjm3[2]);
        p1ip[at(2, j)] = 0.0 - (c1 * wjm3[1] + c2 * wjm2[0] + c3 * wjm2[1]);
        p1ip[at(3, j)] = 0.0 - (c1 * wjm3[2] + c2 * wjm2[1] + c3 * wjm1[0]);
        p1ip[at(4, j)] = c0 * c0
            + c1 * c1 * wjm3[0]
            + 2.0 * c1 * c2 * wjm3[1]
            + 2.0 * c1 * c3 * wjm3[2]
            + c2 * c2 * wjm2[0]
            + 2.0 * c2 * c3 * wjm2[1]
            + c3 * c3 * wjm1[0];
        wjm3[0] = wjm2[0];
        wjm3[1] = wjm2[1];
        wjm3[2] = p1ip[at(2, j)];
        wjm2[0] = wjm1[0];
        wjm2[1] = p1ip[at(3, j)];
        wjm1[0] = p1ip[at(4, j)];
    }
}

/// `bsplvb`'s saved state: the running order and the knot differences.
struct BasisState {
    order: usize,
    delta_left: [f64; 20],
    delta_right: [f64; 20],
}

/// `bsplvb` (1-based `t`, `left`): the values of the `jhigh` B-splines of order `jhigh` that
/// do not vanish at `x`; with `restart` false, continues from the order the state holds.
fn bsplvb(
    t: &[f64],
    jhigh: usize,
    restart: bool,
    x: f64,
    left: usize,
    biatx: &mut [f64],
    state: &mut BasisState,
) {
    if restart {
        state.order = 1;
        biatx[0] = 1.0;
        if state.order >= jhigh {
            return;
        }
    }
    loop {
        let j = state.order;
        let next = j + 1;
        state.delta_right[j - 1] = t[left + j - 1] - x;
        state.delta_left[j - 1] = x - t[left - j];
        let mut saved = 0.0;
        for i in 1..=j {
            let term = biatx[i - 1] / (state.delta_right[i - 1] + state.delta_left[next - i - 1]);
            biatx[i - 1] = saved + state.delta_right[i - 1] * term;
            saved = state.delta_left[next - i - 1] * term;
        }
        biatx[next - 1] = saved;
        state.order = next;
        if state.order >= jhigh {
            break;
        }
    }
}

/// `bsplvd` (1-based `left`): the values and derivatives, up to order `nderiv - 1`, of the `k`
/// B-splines that do not vanish at `x`, as `k` by `nderiv` column-major.
fn bsplvd(t: &[f64], k: usize, x: f64, left: usize, nderiv: usize) -> Vec<f64> {
    let mut dbiatx = vec![0.0; k * nderiv];
    let mut state = BasisState {
        order: 1,
        delta_left: [0.0; 20],
        delta_right: [0.0; 20],
    };
    let mhigh = nderiv.min(k).max(1);
    let kp1 = k + 1;
    bsplvb(t, kp1 - mhigh, true, x, left, &mut dbiatx, &mut state);
    if mhigh == 1 {
        return dbiatx;
    }
    let mut ideriv = mhigh;
    for _ in 2..=mhigh {
        let mut jp1mid = 1;
        for j in ideriv..=k {
            dbiatx[(ideriv - 1) * k + j - 1] = dbiatx[jp1mid - 1];
            jp1mid += 1;
        }
        ideriv -= 1;
        bsplvb(t, kp1 - ideriv, false, x, left, &mut dbiatx, &mut state);
    }
    let mut a = vec![0.0; k * k];
    let mut jlow = 1;
    for i in 1..=k {
        for j in jlow..=k {
            a[(i - 1) * k + j - 1] = 0.0;
        }
        jlow = i;
        a[(i - 1) * k + i - 1] = 1.0;
    }
    for m in 2..=mhigh {
        let kp1mm = kp1 - m;
        let factor_numerator = kp1mm as f64;
        let mut il = left;
        let mut i = k;
        for _ in 1..=kp1mm {
            let factor = factor_numerator / (t[il + kp1mm - 1] - t[il - 1]);
            for j in 1..=i {
                a[(j - 1) * k + i - 1] = (a[(j - 1) * k + i - 1] - a[(j - 1) * k + i - 2]) * factor;
            }
            il -= 1;
            i -= 1;
        }
        for i in 1..=k {
            let mut sum = 0.0;
            let jlow = i.max(m);
            for j in jlow..=k {
                sum = a[(i - 1) * k + j - 1] * dbiatx[(m - 1) * k + j - 1] + sum;
            }
            dbiatx[(m - 1) * k + i - 1] = sum;
        }
    }
    dbiatx
}

/// `bvalue`: the `jderiv`-th derivative at `x` of the order `k` spline with `n` coefficients,
/// continuous from the right; 0 outside the knots.
fn bvalue(t: &[f64], bcoef: &[f64], n: usize, k: usize, x: f64, jderiv: usize) -> f64 {
    if jderiv >= k {
        return 0.0;
    }
    let i = if x != t[n] || t[n] != t[n + k - 1] {
        let (interval, flag) = find_interval(&t[..n + k], x);
        if flag != 0 {
            return 0.0;
        }
        interval
    } else {
        n
    };
    let km1 = k - 1;
    if km1 == 0 {
        return bcoef[i - 1];
    }
    let mut aj = [0.0; 20];
    let mut dm = [0.0; 20];
    let mut dp = [0.0; 20];
    let mut jcmin = 1;
    let imk = i as isize - k as isize;
    if imk >= 0 {
        for j in 1..=km1 {
            dm[j - 1] = x - t[i - j];
        }
    } else {
        jcmin = (1 - imk) as usize;
        for j in 1..=i {
            dm[j - 1] = x - t[i - j];
        }
        for j in i..=km1 {
            aj[k - j - 1] = 0.0;
            dm[j - 1] = dm[i - 1];
        }
    }
    let mut jcmax = k;
    let nmi = n as isize - i as isize;
    if nmi >= 0 {
        for j in 1..=km1 {
            dp[j - 1] = t[i + j - 1] - x;
        }
    } else {
        jcmax = (k as isize + nmi) as usize;
        for j in 1..=jcmax {
            dp[j - 1] = t[i + j - 1] - x;
        }
        for j in jcmax..=km1 {
            aj[j] = 0.0;
            dp[j - 1] = dp[jcmax - 1];
        }
    }
    for jc in jcmin..=jcmax {
        aj[jc - 1] = bcoef[(imk + jc as isize - 1) as usize];
    }
    for j in 1..=jderiv {
        let kmj = k - j;
        let fkmj = kmj as f64;
        let mut ilo = kmj;
        for jj in 1..=kmj {
            aj[jj - 1] = ((aj[jj] - aj[jj - 1]) / (dm[ilo - 1] + dp[jj - 1])) * fkmj;
            ilo -= 1;
        }
    }
    if jderiv != km1 {
        for j in jderiv + 1..=km1 {
            let kmj = k - j;
            let mut ilo = kmj;
            for jj in 1..=kmj {
                aj[jj - 1] =
                    (aj[jj] * dm[ilo - 1] + aj[jj - 1] * dp[jj - 1]) / (dm[ilo - 1] + dp[jj - 1]);
                ilo -= 1;
            }
        }
    }
    aj[0]
}
