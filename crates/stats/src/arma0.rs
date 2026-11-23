// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's C sources, and keep their
// copyright notices:
//
// GNU R: src/library/stats/src/starma.c (starma, karma, forkal; based on Applied Statistics
// algorithms AS 154 and AS 182, (C) Royal Statistical Society 1980, 1982)
//     Copyright (C) 1999-2022 The R Core Team
// GNU R: src/library/stats/src/pacf.c (setup_starma, arma0fa, arma0_kfore and the parameter
// transformations)
//     Copyright (C) 1999-2022 The R Core Team
// GNU R: src/library/stats/src/ts.h (the Starma structure)
//     Copyright (C) 2001-2017 The R Core Team
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! The native half of `arima0` and `predict.arima0`: the exact ARMA likelihood by the Kalman
//! filter of Gardner, Harvey and Phillips (AS 154), the conditional sum of squares, and finite
//! sample forecasts of an ARIMA process (AS 182).
//!
//! `setup_starma` keeps the model state in a slot of [`MODELS`] and returns the slot's index as
//! an integer, which the other routines take back and read and update the state through.
//! `free_starma` empties the slot, so a later use is the error `bad Starma struct`. GNU R hands
//! out an external pointer instead; its R code only passes the handle back and frees it on exit.

use {
    crate::routines::{new_real_matrix, new_reals, read_integers, read_reals},
    heap::object::NA_REAL,
    interp::{
        handles::{self, Cx, Integer, List, Local, NativeArgs, Real},
        unwind::{R, Unwind},
    },
    std::cell::RefCell,
};

/// The model state. `p`, `q` and `r` are the orders of the expanded AR and MA polynomials and
/// the state dimension `max(p, q + 1)`; `mp`, `mq`, `msp`, `msq` and `ns` are the nonseasonal
/// and seasonal orders and the period; `m` is the number of regressors.
struct Starma {
    p: usize,
    q: usize,
    r: usize,
    np: usize,
    nrbar: usize,
    n: usize,
    ncond: usize,
    m: usize,
    trans: bool,
    method: i32,
    nused: usize,
    mp: usize,
    mq: usize,
    msp: usize,
    msq: usize,
    ns: usize,
    delta: f64,
    s2: f64,
    params: Vec<f64>,
    phi: Vec<f64>,
    theta: Vec<f64>,
    a: Vec<f64>,
    /// `P`: the state covariance, upper triangle packed by rows.
    covariance: Vec<f64>,
    /// `V`: the covariance of the state innovations, packed as `covariance`.
    innovation: Vec<f64>,
    thetab: Vec<f64>,
    xnext: Vec<f64>,
    xrow: Vec<f64>,
    rbar: Vec<f64>,
    w: Vec<f64>,
    wkeep: Vec<f64>,
    resid: Vec<f64>,
    reg: Vec<f64>,
}

impl Starma {
    fn parameter_count(&self) -> usize {
        self.mp + self.mq + self.msp + self.msq + self.m
    }
}

thread_local! {
    /// The live models, indexed by handle. `free_starma` empties a slot, and `setup_starma`
    /// fills the first empty one.
    static MODELS: RefCell<Vec<Option<Starma>>> = const { RefCell::new(Vec::new()) };
}

/// `setup_starma(arma, x, n, xreg, m, delta, trans, ncond)`.
pub fn setup_starma<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let na = read_integers(context, arguments.value(0))?;
    let x = read_reals(context, arguments.value(1))?;
    let n = handles::integer_scalar(context, arguments.value(2));
    let xreg = read_reals(context, arguments.value(3))?;
    let m = handles::integer_scalar(context, arguments.value(4));
    let delta = handles::real_scalar(context, arguments.value(5));
    let trans = handles::integer_scalar(context, arguments.value(6)) != 0;
    let ncond = handles::integer_scalar(context, arguments.value(7));
    if na.len() < 5 || na[..5].iter().any(|&order| order < 0) {
        return Err(arguments.error("invalid ARMA orders"));
    }
    let (mp, mq, msp, msq, ns) = (
        na[0] as usize,
        na[1] as usize,
        na[2] as usize,
        na[3] as usize,
        na[4] as usize,
    );
    if n < 0 || x.len() < n as usize {
        return Err(arguments.error("invalid series length"));
    }
    let n = n as usize;
    if m < 0 || xreg.len() < n * m as usize {
        return Err(arguments.error("invalid regressors"));
    }
    let m = m as usize;
    let ip = ns * msp + mp;
    let iq = ns * msq + mq;
    let ir = ip.max(iq + 1);
    let np = ir
        .checked_mul(ir + 1)
        .map(|twice| twice / 2)
        .ok_or_else(|| allocation_failure(arguments, usize::MAX))?;
    // starma.c sizes `rbar` in C `int`, so past that range its consistency check fails with
    // fault 7 instead of allocating the true size.
    let nrbar = match np.checked_mul(np.saturating_sub(1)) {
        Some(twice) if twice / 2 <= i32::MAX as usize => (twice / 2).max(1),
        _ => 1,
    };
    let mut reg = vec![0.0; 1 + n * m];
    reg[..n * m].copy_from_slice(&xreg[..n * m]);
    let starma = Starma {
        p: ip,
        q: iq,
        r: ir,
        np,
        nrbar,
        n,
        ncond: ncond.max(0) as usize,
        m,
        trans,
        method: 0,
        nused: 0,
        mp,
        mq,
        msp,
        msq,
        ns,
        delta,
        s2: 0.0,
        params: vec![0.0; mp + mq + msp + msq + m],
        phi: zeroed(arguments, ir)?,
        theta: zeroed(arguments, ir)?,
        a: zeroed(arguments, ir)?,
        covariance: zeroed(arguments, np)?,
        innovation: zeroed(arguments, np)?,
        thetab: zeroed(arguments, np)?,
        xnext: zeroed(arguments, np)?,
        xrow: zeroed(arguments, np)?,
        rbar: zeroed(arguments, nrbar)?,
        w: x[..n].to_vec(),
        wkeep: x[..n].to_vec(),
        resid: vec![0.0; n],
        reg,
    };
    let index = MODELS.with_borrow_mut(|models| match models.iter().position(Option::is_none) {
        Some(index) => {
            models[index] = Some(starma);
            index
        }
        None => {
            models.push(Some(starma));
            models.len() - 1
        }
    });
    let handle =
        i32::try_from(index).map_err(|_| arguments.error("too many ARMA models are live"))?;
    handles::integer(context, handle)
}

/// `free_starma(G)`.
pub fn free_starma<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let freed = MODELS.with_borrow_mut(|models| models.get_mut(index?)?.take());
    freed.ok_or_else(|| bad_model(arguments))?;
    Ok(handles::null())
}

/// `Starma_method(G, method)`: 1 selects the conditional sum of squares, 0 the exact
/// likelihood.
pub fn starma_method<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let method = handles::integer_scalar(context, arguments.value(1));
    with_model(arguments, index, |g| {
        g.method = method;
        Ok(())
    })?;
    Ok(handles::null())
}

/// `Dotrans(G, x)`: the parameters with the ARMA parts mapped to stationarity.
pub fn dotrans_routine<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let raw = parameters(context, arguments, index)?;
    let new = with_model(arguments, index, |g| {
        let mut new = raw.clone();
        dotrans(arguments, g, &raw, &mut new, true)?;
        Ok(new)
    })?;
    new_reals(context, &new)
}

/// `set_trans(G, trans)`.
pub fn set_trans<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let trans = handles::integer_scalar(context, arguments.value(1)) != 0;
    with_model(arguments, index, |g| {
        g.trans = trans;
        Ok(())
    })?;
    Ok(handles::null())
}

/// `arma0fa(G, params)`: half the log of the innovations variance plus half the mean log
/// prediction variance (the exact likelihood up to a constant), or half the log of the
/// conditional sum of squares per term.
pub fn arma0fa<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let inparams = parameters(context, arguments, index)?;
    let ans = with_model(arguments, index, |g| likelihood(arguments, g, &inparams))?;
    Ok(context.scalar::<Real>(ans)?.finish().value())
}

/// `get_s2(G)`: the innovations variance of the last evaluation.
pub fn get_s2<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let s2 = with_model(arguments, index, |g| Ok(g.s2))?;
    Ok(context.scalar::<Real>(s2)?.finish().value())
}

/// `get_resid(G)`: the standardized residuals of the last evaluation.
pub fn get_resid<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let resid = with_model(arguments, index, |g| Ok(g.resid[..g.n].to_vec()))?;
    new_reals(context, &resid)
}

/// `arma0_kfore(G, d, sd, n.ahead)`: forecasts and their mean square errors for the model
/// differenced `d` times and seasonally `sd` times.
pub fn arma0_kfore<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let dd = handles::integer_scalar(context, arguments.value(1));
    let sd = handles::integer_scalar(context, arguments.value(2));
    let il = handles::integer_scalar(context, arguments.value(3));
    let (x, var) = with_model(arguments, index, |g| forecast(arguments, g, dd, sd, il))?;
    let mut result = context.alloc::<List>(2)?;
    let x = new_reals(context, &x)?;
    result.set(context, 0, x);
    let var = new_reals(context, &var)?;
    result.set(context, 1, var);
    Ok(result.finish().value())
}

/// `Invtrans(G, x)`: the inverse of `Dotrans`.
pub fn invtrans<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let index = model_index(context, arguments);
    let raw = parameters(context, arguments, index)?;
    let new = with_model(arguments, index, |g| {
        let mut new = raw.clone();
        let mut v = 0;
        for count in [g.mp, g.mq, g.msp, g.msq] {
            invpartrans(arguments, &raw[v..v + count], &mut new[v..v + count])?;
            v += count;
        }
        Ok(new)
    })?;
    new_reals(context, &new)
}

/// `Gradtrans(G, x)`: the Jacobian of `Dotrans`, by forward differences with step 1e-3.
pub fn gradtrans<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    const EPS: f64 = 1e-3;
    let index = model_index(context, arguments);
    let raw = parameters(context, arguments, index)?;
    let (jacobian, n) = with_model(arguments, index, |g| {
        let n = g.parameter_count();
        if raw.len() != n {
            return Err(arguments.error("wrong number of parameters"));
        }
        let mut jacobian = vec![0.0; n * n];
        for i in 0..n {
            jacobian[i + i * n] = 1.0;
        }
        // GNU R writes the MA block's derivatives into the first columns rather than the
        // block's own (column `j`, not `j + v`), and `arima0`'s standard errors depend on that.
        let blocks = [
            (0, g.mp, 0),
            (g.mp, g.mq, 0),
            (g.mp + g.mq, g.msp, g.mp + g.mq),
            (g.mp + g.mq + g.msp, g.msq, g.mp + g.mq + g.msp),
        ];
        for (v, count, column) in blocks {
            if count > 0 {
                let mut w1 = raw[v..v + count].to_vec();
                let mut w2 = vec![0.0; count];
                let mut w3 = vec![0.0; count];
                partrans(arguments, &w1, &mut w2)?;
                for i in 0..count {
                    w1[i] += EPS;
                    partrans(arguments, &w1, &mut w3)?;
                    for j in 0..count {
                        jacobian[i + v + (j + column) * n] = (w3[j] - w2[j]) / EPS;
                    }
                    w1[i] -= EPS;
                }
            }
        }
        Ok((jacobian, n))
    })?;
    new_real_matrix(context, &jacobian, n, n)
}

/// The slot the handle argument names, `None` for a value that names no slot.
fn model_index(context: &mut Cx<'_>, arguments: &NativeArgs<'_, '_>) -> Option<usize> {
    let handle = arguments.value(0).standard::<Integer>()?;
    match handle.as_slice(context) {
        &[index] => usize::try_from(index).ok(),
        _ => None,
    }
}

/// Runs `body` on the live model in slot `index`. `body` must not reach R, which could run an
/// `arima0` of its own and so borrow the models again.
fn with_model<T>(
    arguments: &NativeArgs<'_, '_>,
    index: Option<usize>,
    body: impl FnOnce(&mut Starma) -> R<T>,
) -> R<T> {
    MODELS.with_borrow_mut(|models| {
        match index
            .and_then(|index| models.get_mut(index))
            .and_then(Option::as_mut)
        {
            Some(g) => body(g),
            None => Err(bad_model(arguments)),
        }
    })
}

/// A zeroed vector whose length comes from the model orders or the forecast horizon, which a
/// caller can make too large to allocate: that is an R error, as `R_Calloc`'s failure is.
fn zeroed(arguments: &NativeArgs<'_, '_>, len: usize) -> R<Vec<f64>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| allocation_failure(arguments, len))?;
    values.resize(len, 0.0);
    Ok(values)
}

fn allocation_failure(arguments: &NativeArgs<'_, '_>, len: usize) -> Unwind {
    arguments.error(format!(
        "'R_Calloc' could not allocate memory ({len} of 8 bytes)"
    ))
}

fn bad_model(arguments: &NativeArgs<'_, '_>) -> Unwind {
    arguments.error("bad Starma struct")
}

/// The doubles of the parameter vector argument, which must hold at least as many as the
/// model in slot `index` has parameters.
fn parameters<'s>(
    context: &mut Cx<'s>,
    arguments: &NativeArgs<'_, 's>,
    index: Option<usize>,
) -> R<Vec<f64>> {
    let count = with_model(arguments, index, |g| Ok(g.parameter_count()))?;
    let x = arguments.value(1);
    if handles::length(context, x) < count {
        return Err(arguments.error("too few parameters"));
    }
    read_reals(context, x)
}

/// The body of `arma0fa` once the parameters are read.
fn likelihood(arguments: &NativeArgs<'_, '_>, g: &mut Starma, inparams: &[f64]) -> R<f64> {
    let mut params = std::mem::take(&mut g.params);
    let transformed = dotrans(arguments, g, inparams, &mut params, g.trans);
    g.params = params;
    transformed?;

    let (mp, mq, msp, msq, ns) = (g.mp, g.mq, g.msp, g.msq, g.ns);
    if ns > 0 {
        for i in 0..mp {
            g.phi[i] = g.params[i];
        }
        for i in 0..mq {
            g.theta[i] = g.params[i + mp];
        }
        for i in mp..g.p {
            g.phi[i] = 0.0;
        }
        for i in mq..g.q {
            g.theta[i] = 0.0;
        }
        for j in 0..msp {
            g.phi[(j + 1) * ns - 1] += g.params[j + mp + mq];
            for i in 0..mp {
                g.phi[(j + 1) * ns + i] -= g.params[i] * g.params[j + mp + mq];
            }
        }
        for j in 0..msq {
            g.theta[(j + 1) * ns - 1] += g.params[j + mp + mq + msp];
            for i in 0..mq {
                g.theta[(j + 1) * ns + i] += g.params[i + mp] * g.params[j + mp + mq + msp];
            }
        }
    } else {
        for i in 0..mp {
            g.phi[i] = g.params[i];
        }
        for i in 0..mq {
            g.theta[i] = g.params[i + mp];
        }
    }

    let streg = mp + mq + msp + msq;
    let n = g.n;
    if g.m > 0 && n > 0 {
        // Column by column, which subtracts each element's terms in the same order as row by row.
        let w = &mut g.w[..n];
        w.copy_from_slice(&g.wkeep[..n]);
        for (column, &coefficient) in g.reg.chunks_exact(n).zip(&g.params[streg..streg + g.m]) {
            for (value, &regressor) in w.iter_mut().zip(column) {
                *value -= regressor * coefficient;
            }
        }
    }

    if g.method == 1 {
        let p = mp + ns * msp;
        let q = mq + ns * msq;
        let ncond = g.ncond.min(g.n);
        let mut nu = 0;
        let mut ssq = 0.0;
        // Slices rather than the vectors, so that the loop keeps their bounds in registers.
        let (phi, theta, w, resid) = (&g.phi[..p], &g.theta[..q], &g.w[..], &mut g.resid[..]);
        for i in 0..ncond {
            resid[i] = 0.0;
        }
        for i in ncond..g.n {
            let mut tmp = w[i];
            let lags = i - ncond;
            for (coefficient, value) in phi[..lags.min(p)].iter().zip(w[..i].iter().rev()) {
                tmp -= coefficient * value;
            }
            for (coefficient, value) in theta[..lags.min(q)].iter().zip(resid[..i].iter().rev()) {
                tmp -= coefficient * value;
            }
            resid[i] = tmp;
            if !tmp.is_nan() {
                nu += 1;
                ssq += tmp * tmp;
            }
        }
        g.s2 = ssq / nu as f64;
        Ok(0.5 * g.s2.ln())
    } else {
        let ifault = starma(g);
        if ifault != 0 {
            return Err(arguments.error(format!("starma error code {ifault}")));
        }
        let mut sumlog = 0.0;
        let mut ssq = 0.0;
        let mut it = 0;
        karma(g, &mut sumlog, &mut ssq, 1, &mut it);
        g.s2 = ssq / g.nused as f64;
        Ok(0.5 * ((ssq / g.nused as f64).ln() + sumlog / g.nused as f64))
    }
}

/// The body of `arma0_kfore` once its arguments are read: the forecasts and their variances.
fn forecast(
    arguments: &NativeArgs<'_, '_>,
    g: &mut Starma,
    dd: i32,
    sd: i32,
    il: i32,
) -> R<(Vec<f64>, Vec<f64>)> {
    if il < 0 {
        return Err(arguments.error("negative length vectors are not allowed"));
    }
    let ns = g.ns;
    let d = i64::from(dd) + ns as i64 * i64::from(sd);
    if d < 0 || d > i64::from(i32::MAX) {
        let ifault = if il < 1 { 11 } else { 8 };
        return Err(arguments.error(format!("forkal error code {ifault}")));
    }
    let d = d as usize;

    let mut del = zeroed(arguments, d + 1)?;
    let mut del2 = zeroed(arguments, d + 1)?;
    del[0] = 1.0;
    for _ in 0..dd.max(0) {
        del2.copy_from_slice(&del);
        for i in 0..d {
            del[i + 1] -= del2[i];
        }
    }
    for _ in 0..sd.max(0) {
        del2.copy_from_slice(&del);
        for i in 0..(d + 1).saturating_sub(ns) {
            del[i + ns] -= del2[i];
        }
    }
    for i in 1..=d {
        del[i] *= -1.0;
    }

    let il = il as usize;
    let mut x = zeroed(arguments, il)?;
    let mut var = zeroed(arguments, il)?;
    forkal(arguments, g, d, il, &del[1..], &mut x, &mut var)?;
    Ok((x, var))
}

fn dotrans(
    arguments: &NativeArgs<'_, '_>,
    g: &Starma,
    raw: &[f64],
    new: &mut [f64],
    trans: bool,
) -> R<()> {
    let n = g.parameter_count();
    new[..n].copy_from_slice(&raw[..n]);
    if trans {
        let mut v = 0;
        for count in [g.mp, g.mq, g.msp, g.msq] {
            partrans(arguments, &raw[v..v + count], &mut new[v..v + count])?;
            v += count;
        }
    }
    Ok(())
}

/// Maps unconstrained values to the coefficients of a stationary autoregression: `tanh` gives
/// partial autocorrelations in (-1, 1), and the Durbin-Levinson recursion turns them into
/// coefficients.
fn partrans(arguments: &NativeArgs<'_, '_>, raw: &[f64], new: &mut [f64]) -> R<()> {
    let p = raw.len();
    if p > 100 {
        return Err(arguments.error("can only transform 100 pars in arima0"));
    }
    if p == 0 {
        return Ok(());
    }
    let mut work = [0.0; 100];
    for j in 0..p {
        new[j] = raw[j].tanh();
        work[j] = new[j];
    }
    for j in 1..p {
        let a = new[j];
        for k in 0..j {
            work[k] -= a * new[j - k - 1];
        }
        new[..j].copy_from_slice(&work[..j]);
    }
    Ok(())
}

fn invpartrans(arguments: &NativeArgs<'_, '_>, phi: &[f64], new: &mut [f64]) -> R<()> {
    let p = phi.len();
    if p > 100 {
        return Err(arguments.error("can only transform 100 pars in arima0"));
    }
    if p == 0 {
        return Ok(());
    }
    let mut work = [0.0; 100];
    for j in 0..p {
        new[j] = phi[j];
        work[j] = new[j];
    }
    for j in (1..p).rev() {
        let a = new[j];
        for k in 0..j {
            work[k] = (new[k] + a * new[j - k - 1]) / (1.0 - a * a);
        }
        new[..j].copy_from_slice(&work[..j]);
    }
    for value in new.iter_mut() {
        *value = atanh(*value);
    }
    Ok(())
}

/// The C library's `atanh`, which GNU R calls and whose rounding `f64::atanh` does not
/// reproduce: glibc's algorithm, over the platform's `log1p`.
fn atanh(x: f64) -> f64 {
    let magnitude = x.abs();
    let half = if magnitude < 0.5 {
        if magnitude < 2f64.powi(-28) {
            return x;
        }
        let twice = magnitude + magnitude;
        0.5 * (twice + twice * magnitude / (1.0 - magnitude)).ln_1p()
    } else if magnitude < 1.0 {
        0.5 * ((magnitude + magnitude) / (1.0 - magnitude)).ln_1p()
    } else if magnitude > 1.0 {
        // glibc's spelling of the processor's default NaN, whose sign differs between
        // processors.
        #[expect(clippy::eq_op)]
        return (x - x) / (x - x);
    } else {
        return x / 0.0;
    };
    half.copysign(x)
}

/// AS 154: sets `V` and the initial state `a` and covariance `P` of the ARMA process from
/// `phi` and `theta`. Returns the fault code, 0 on success.
fn starma(g: &mut Starma) -> i32 {
    let (p, q, r, np, nrbar) = (g.p, g.q, g.r, g.np, g.nrbar);
    let Starma {
        phi,
        theta,
        a,
        covariance: pm,
        innovation: v,
        thetab,
        xnext,
        xrow,
        rbar,
        ..
    } = g;

    if !(q > 0 || p > 1) {
        v[0] = 1.0;
        a[0] = 0.0;
        pm[0] = 1.0 / (1.0 - phi[0] * phi[0]);
        return 0;
    }

    let mut ifault = 0;
    if p == 0 && q == 0 {
        ifault = 4;
    }
    let k = (q + 1).max(p);
    if r != k {
        ifault = 5;
    }
    if np != r * (r + 1) / 2 {
        ifault = 6;
    }
    if nrbar != np.saturating_mul(np - 1) / 2 {
        ifault = 7;
    }
    if r == 1 {
        ifault = 8;
    }
    if ifault != 0 {
        return ifault;
    }

    for i in 1..r {
        a[i] = 0.0;
        if i >= p {
            phi[i] = 0.0;
        }
        v[i] = 0.0;
        if i < q + 1 {
            v[i] = theta[i - 1];
        }
    }
    a[0] = 0.0;
    if p == 0 {
        phi[0] = 0.0;
    }
    v[0] = 1.0;
    let mut ind = r;
    for j in 1..r {
        let vj = v[j];
        for i in j..r {
            v[ind] = v[i] * vj;
            ind += 1;
        }
    }

    if p > 0 {
        // The equations s * vec(P(0)) = vec(V) are solved for vec(P(0)), generating s row by
        // row in xnext. The order of the elements of P is changed so as to bring more leading
        // zeros into the rows of s.
        for i in 0..nrbar {
            rbar[i] = 0.0;
        }
        for i in 0..np {
            pm[i] = 0.0;
            thetab[i] = 0.0;
            xnext[i] = 0.0;
        }
        ind = 0;
        // One past C's `ind1`, which starts at -1 and is pre-incremented.
        let mut ind1 = 0;
        let npr = np - r;
        let npr1 = npr + 1;
        let mut indj = npr;
        let mut ind2 = npr - 1;
        for j in 0..r {
            let phij = phi[j];
            xnext[indj] = 0.0;
            indj += 1;
            let mut indi = npr1 + j;
            for i in j..r {
                let ynext = v[ind];
                ind += 1;
                let phii = phi[i];
                if j != r - 1 {
                    xnext[indj] = -phii;
                    if i != r - 1 {
                        xnext[indi] -= phij;
                        xnext[ind1] = -1.0;
                        ind1 += 1;
                    }
                }
                xnext[npr] = -phii * phij;
                ind2 += 1;
                if ind2 >= np {
                    ind2 = 0;
                }
                xnext[ind2] += 1.0;
                inclu2(np, xnext, xrow, ynext, pm, rbar, thetab);
                xnext[ind2] = 0.0;
                if i != r - 1 {
                    xnext[indi] = 0.0;
                    indi += 1;
                    xnext[ind1 - 1] = 0.0;
                }
            }
        }

        let mut ithisr = nrbar;
        for i in 0..np {
            let im = np - 1 - i;
            let mut bi = thetab[im];
            let mut jm = np - 1;
            for _ in 0..i {
                ithisr -= 1;
                bi -= rbar[ithisr] * pm[jm];
                jm -= 1;
            }
            pm[im] = bi;
        }

        ind = npr;
        for i in 0..r {
            xnext[i] = pm[ind];
            ind += 1;
        }
        for i in 0..npr {
            pm[np - 1 - i] = pm[npr - 1 - i];
        }
        for i in 0..r {
            pm[i] = xnext[i];
        }
    } else {
        // P(0) by back substitution for a moving average process.
        let mut indn = np;
        ind = np;
        for i in 0..r {
            for j in 0..=i {
                ind -= 1;
                pm[ind] = v[ind];
                if j != 0 {
                    indn -= 1;
                    pm[ind] += pm[indn];
                }
            }
        }
    }
    0
}

/// Updates `d`, `rbar` and `thetab` by the inclusion of `xnext` and `ynext` (AS 154's
/// Givens update).
fn inclu2(
    np: usize,
    xnext: &[f64],
    xrow: &mut [f64],
    mut ynext: f64,
    d: &mut [f64],
    rbar: &mut [f64],
    thetab: &mut [f64],
) {
    xrow[..np].copy_from_slice(&xnext[..np]);
    let mut ithisr = 0;
    for i in 0..np {
        if xrow[i] != 0.0 {
            let xi = xrow[i];
            let di = d[i];
            let dpi = di + xi * xi;
            d[i] = dpi;
            let cbar = di / dpi;
            let sbar = xi / dpi;
            for k in (i + 1)..np {
                let xk = xrow[k];
                let rbthis = rbar[ithisr];
                xrow[k] = xk - xi * rbthis;
                rbar[ithisr] = cbar * rbthis + sbar * xk;
                ithisr += 1;
            }
            let xk = ynext;
            ynext = xk - xi * thetab[i];
            thetab[i] = cbar * thetab[i] + sbar * xk;
            if di == 0.0 {
                return;
            }
        } else {
            ithisr = ithisr + np - i - 1;
        }
    }
}

/// AS 154's filter: updates `a`, `P`, `sumlog` and `ssq` by the inclusion of `w[0..n]`, and
/// sets the standardized residuals. Once the prediction variance falls below `1 + delta` it
/// switches to the quick recursions, which treat the filter as converged.
fn karma(g: &mut Starma, sumlog: &mut f64, ssq: &mut f64, iupd: i32, nit: &mut usize) {
    let (p, q, r, n, delta) = (g.p, g.q, g.r, g.n, g.delta);
    let Starma {
        phi,
        theta,
        a,
        covariance: pm,
        innovation: v,
        w,
        resid,
        xnext: work,
        ..
    } = &mut *g;
    let mut nu = 0;

    let quick_from = if *nit == 0 {
        let mut switch = None;
        for i in 0..n {
            if iupd != 1 || i > 0 {
                // Here dt = ft - 1.
                let mut dt = if r > 1 { pm[r] } else { 0.0 };
                if dt < delta {
                    switch = Some(i);
                    break;
                }
                let a1 = a[0];
                for j in 0..r - 1 {
                    a[j] = a[j + 1];
                }
                a[r - 1] = 0.0;
                for j in 0..p {
                    a[j] += phi[j] * a1;
                }
                if pm[0] == 0.0 {
                    // The last observation was available.
                    let mut ind = 0;
                    let mut indn = r;
                    for j in 0..r {
                        for l in j..r {
                            pm[ind] = v[ind];
                            if l < r - 1 {
                                pm[ind] += pm[indn];
                                indn += 1;
                            }
                            ind += 1;
                        }
                    }
                } else {
                    for j in 0..r {
                        work[j] = pm[j];
                    }
                    let mut ind = 0;
                    let mut indn = r;
                    dt = pm[0];
                    for j in 0..r {
                        let phij = phi[j];
                        let phijdt = phij * dt;
                        for l in j..r {
                            pm[ind] = v[ind] + phi[l] * phijdt;
                            if j < r - 1 {
                                pm[ind] += work[j + 1] * phi[l];
                            }
                            if l < r - 1 {
                                pm[ind] += work[l + 1] * phij + pm[indn];
                                indn += 1;
                            }
                            ind += 1;
                        }
                    }
                }
            }

            let ft = pm[0];
            if !w[i].is_nan() {
                let ut = w[i] - a[0];
                if r > 1 {
                    let mut ind = r;
                    for j in 1..r {
                        let gain = pm[j] / ft;
                        a[j] += gain * ut;
                        for l in j..r {
                            pm[ind] -= gain * pm[l];
                            ind += 1;
                        }
                    }
                }
                a[0] = w[i];
                resid[i] = ut / ft.sqrt();
                *ssq += ut * ut / ft;
                *sumlog += ft.ln();
                nu += 1;
                for l in 0..r {
                    pm[l] = 0.0;
                }
            } else {
                resid[i] = NA_REAL;
            }
        }
        *nit = n;
        switch
    } else {
        // The quick recursions never see missing values.
        Some(0)
    };

    if let Some(i) = quick_from {
        *nit = i;
        let (phi, theta, w, resid) = (&phi[..p], &theta[..q], &w[..], &mut resid[..]);
        let mut sum = *ssq;
        for ii in i..n {
            let mut et = w[ii];
            for (coefficient, value) in phi.iter().zip(w[..ii].iter().rev()) {
                et -= coefficient * value;
            }
            for (coefficient, value) in theta.iter().zip(resid[..ii].iter().rev()) {
                et -= coefficient * value;
            }
            resid[ii] = et;
            sum += et * et;
            nu += 1;
        }
        *ssq = sum;
    }
    g.nused = nu;
}

/// AS 182: finite sample predictions `y[0..il]` and their mean square errors `amse` for the
/// ARIMA process whose differencing polynomial has coefficients `delta[0..d]`.
fn forkal(
    arguments: &NativeArgs<'_, '_>,
    g: &mut Starma,
    d: usize,
    il: usize,
    delta: &[f64],
    y: &mut [f64],
    amse: &mut [f64],
) -> R<()> {
    let (p, q, r, n, np) = (g.p, g.q, g.r, g.n, g.np);
    let rd = r + d;
    let rz = rd
        .checked_mul(rd + 1)
        .map(|twice| twice / 2)
        .ok_or_else(|| allocation_failure(arguments, usize::MAX))?;
    let mut store = zeroed(arguments, rd)?;
    g.a = zeroed(arguments, rd)?;
    g.covariance = zeroed(arguments, rz)?;

    let mut ifault = 0;
    if p * p + q * q == 0 {
        ifault = 4;
    }
    if r != p.max(q + 1) {
        ifault = 5;
    }
    if np != r * (r + 1) / 2 {
        ifault = 6;
    }
    if il < 1 {
        ifault = 11;
    }
    if ifault != 0 {
        return Err(arguments.error(format!("forkal error code {ifault}")));
    }

    if r == 1 {
        g.a[0] = 0.0;
        g.innovation[0] = 1.0;
        g.covariance[0] = 1.0 / (1.0 - g.phi[0] * g.phi[0]);
    } else {
        let ifault = starma(g);
        if ifault != 0 {
            return Err(arguments.error(format!("forkal error code {ifault}")));
        }
    }

    if d > 0 && n < d + 1 {
        return Err(arguments.error("too few observations to forecast"));
    }
    let nt = n - d;
    if d > 0 {
        for j in 0..d {
            store[j] = g.w[n - j - 2];
            if store[j].is_nan() {
                return Err(arguments.error(format!("missing value in last {d} observations")));
            }
        }
        for i in 0..nt {
            let mut aa = 0.0;
            for k in 0..d {
                aa -= delta[k] * g.w[d + i - k - 1];
            }
            g.w[i] = g.w[i + d] + aa;
        }
    }

    {
        let mut sumlog = 0.0;
        let mut ssq = 0.0;
        let mut nit = 0;
        g.n = nt;
        karma(g, &mut sumlog, &mut ssq, 1, &mut nit);
    }

    let mut nu = 0;
    let mut sigma2 = 0.0;
    for j in 0..nt {
        let tmp = g.resid[j];
        if !tmp.is_nan() {
            nu += 1;
            sigma2 += tmp * tmp;
        }
    }
    sigma2 /= nu as f64;

    let Starma {
        phi,
        a,
        covariance: pm,
        innovation: v,
        xrow,
        ..
    } = g;

    // Reset the initial a and P when differencing occurs.
    if d > 0 {
        for i in 0..np {
            xrow[i] = pm[i];
        }
        for i in 0..rz {
            pm[i] = 0.0;
        }
        let mut ind = 0;
        for j in 0..r {
            let mut k = j * (rd + 1) - j * (j + 1) / 2;
            for _ in j..r {
                pm[k] = xrow[ind];
                k += 1;
                ind += 1;
            }
        }
        for j in 0..d {
            a[r + j] = store[j];
        }
    }

    let i45 = 2 * rd + 1;
    let jkl = r * (2 * d + r + 1) / 2;

    for l in 0..il {
        // Predict a.
        let mut a1 = a[0];
        for i in 0..r - 1 {
            a[i] = a[i + 1];
        }
        a[r - 1] = 0.0;
        for j in 0..p {
            a[j] += phi[j] * a1;
        }
        if d > 0 {
            for j in 0..d {
                a1 += delta[j] * a[r + j];
            }
            for i in ((r + 1)..rd).rev() {
                a[i] = a[i - 1];
            }
            a[r] = a1;
        }

        // Predict P.
        if d > 0 {
            for i in 0..d {
                store[i] = 0.0;
                for j in 0..d {
                    let ll = i.max(j);
                    let k = i.min(j);
                    let jj = jkl + (ll - k) + k * (2 * d + 2 - k - 1) / 2;
                    store[i] += delta[j] * pm[jj];
                }
            }
            if d > 1 {
                for j in 0..d - 1 {
                    let jj = d - j - 1;
                    let mut lk = (jj - 1) * (2 * d + 2 - jj) / 2 + jkl;
                    let mut lk1 = jj * (2 * d + 1 - jj) / 2 + jkl;
                    for _ in 0..=j {
                        pm[lk1] = pm[lk];
                        lk1 += 1;
                        lk += 1;
                    }
                }
                for j in 0..d - 1 {
                    pm[jkl + j + 1] = store[j] + pm[r + j];
                }
            }
            pm[jkl] = pm[0];
            for i in 0..d {
                pm[jkl] += delta[i] * (store[i] + 2.0 * pm[r + i]);
            }
            for i in 0..d {
                store[i] = pm[r + i];
            }
            for j in 0..r {
                let kk1 = (j + 1) * (2 * rd - j - 2) / 2 + r;
                let k1 = j * (2 * rd - j - 1) / 2 + r;
                for i in 0..d {
                    let kk = kk1 + i;
                    let k = k1 + i;
                    pm[k] = phi[j] * store[i];
                    if j < r - 1 {
                        pm[k] += pm[kk];
                    }
                }
            }

            for j in 0..r {
                store[j] = 0.0;
                let mut kkk = (j + 1) * (i45 - j - 1) / 2 - d;
                for i in 0..d {
                    store[j] += delta[i] * pm[kkk];
                    kkk += 1;
                }
            }
            for j in 0..r {
                let mut k = (j + 1) * (rd + 1) - (j + 1) * (j + 2) / 2;
                for _ in 0..d - 1 {
                    k -= 1;
                    pm[k] = pm[k - 1];
                }
            }
            for j in 0..r {
                let k = j * (2 * rd - j - 1) / 2 + r;
                pm[k] = store[j] + phi[j] * pm[0];
                if j < r - 1 {
                    pm[k] += pm[j + 1];
                }
            }
        }
        for i in 0..r {
            store[i] = pm[i];
        }

        let mut ind = 0;
        let dt = pm[0];
        for j in 0..r {
            let phij = phi[j];
            let phijdt = phij * dt;
            // C's `ind2` and `ind1` are pre-incremented, so both start one past C's values.
            let mut ind2 = j * (2 * rd - j + 1) / 2;
            let mut ind1 = (j + 1) * (i45 - j - 1) / 2;
            for i in j..r {
                let phii = phi[i];
                pm[ind2] = v[ind] + phii * phijdt;
                ind += 1;
                if j < r - 1 {
                    pm[ind2] += store[j + 1] * phii;
                }
                if i < r - 1 {
                    pm[ind2] += store[i + 1] * phij + pm[ind1];
                    ind1 += 1;
                }
                ind2 += 1;
            }
        }

        // Predict y.
        y[l] = a[0];
        for j in 0..d {
            y[l] += a[r + j] * delta[j];
        }

        // The mean square error of y.
        let mut ams = pm[0];
        if d > 0 {
            for j in 0..d {
                let k = r * (i45 - r) / 2 + j * (2 * d + 1 - j) / 2;
                let tmp = delta[j];
                ams += 2.0 * tmp * pm[r + j] + pm[k] * tmp * tmp;
            }
            for j in 0..d - 1 {
                let mut k = r * (i45 - r) / 2 + 1 + j * (2 * d + 1 - j) / 2;
                for i in (j + 1)..d {
                    ams += 2.0 * delta[i] * delta[j] * pm[k];
                    k += 1;
                }
            }
        }
        amse[l] = ams * sigma2;
    }
    Ok(())
}
