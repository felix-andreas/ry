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
//! `setup_starma` returns an external pointer that owns the model state, which the other
//! routines read and update. `free_starma` releases it and clears the pointer's address, so a
//! later use is the error `bad Starma struct`; a pointer the collector reclaims without
//! `free_starma` releases the state through its finalizer.

use {
    crate::routines::{integer_values, real_data, real_matrix},
    heap::object::{NA_REAL, Sexp, SexpType},
    interp::{
        builtin_libraries::NativeCall,
        heap,
        unwind::R,
        values::{self, install, length, nil, scalar_real},
    },
    std::{cell::Cell, ffi::c_void},
};

unsafe extern "C" {
    // GNU R calls the C library's `atanh`, whose rounding `f64::atanh` does not reproduce.
    fn atanh(x: f64) -> f64;
}

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
    static TAG: Cell<Option<Sexp>> = const { Cell::new(None) };
}

/// The symbol that tags a Starma pointer. Symbols are never collected, so caching it is safe.
unsafe fn starma_tag() -> R<Sexp> {
    if let Some(tag) = TAG.with(Cell::get) {
        return Ok(tag);
    }
    let tag = install("STARMA_TAG")?;
    TAG.with(|cell| cell.set(Some(tag)));
    Ok(tag)
}

/// The state a Starma pointer owns.
///
/// # Safety
/// The reference must not outlive the routine call that holds `handle` as an argument.
unsafe fn starma_of<'a>(call: &NativeCall<'_>, handle: Sexp) -> R<&'a mut Starma> {
    let tagged = handle.kind() == SexpType::ExternalPtr
        && TAG
            .with(Cell::get)
            .is_some_and(|tag| handle.external_tag() == tag);
    if !tagged || handle.external_address().is_null() {
        return Err(call.error("bad Starma struct"));
    }
    Ok(&mut *handle.external_address().cast::<Starma>())
}

unsafe extern "C" fn finalize(object: Sexp) {
    let address = object.external_address();
    if !address.is_null() {
        object.set_external_address(std::ptr::null_mut());
        drop(Box::from_raw(address.cast::<Starma>()));
    }
}

/// `setup_starma(arma, x, n, xreg, m, delta, trans, ncond)`.
pub unsafe fn setup_starma(call: &NativeCall<'_>) -> R<Sexp> {
    let na = integer_values(call.arg(0))?;
    let x = real_data(call.arg(1))?;
    let n = values::as_integer_scalar(call.arg(2));
    let xreg = real_data(call.arg(3))?;
    let m = values::as_integer_scalar(call.arg(4));
    let delta = values::as_real_scalar(call.arg(5));
    let trans = values::as_integer_scalar(call.arg(6)) != 0;
    let ncond = values::as_integer_scalar(call.arg(7));
    if na.len() < 5 || na[..5].iter().any(|&order| order < 0) {
        return Err(call.error("invalid ARMA orders"));
    }
    let (mp, mq, msp, msq, ns) = (
        na[0] as usize,
        na[1] as usize,
        na[2] as usize,
        na[3] as usize,
        na[4] as usize,
    );
    if n < 0 || x.len() < n as usize {
        return Err(call.error("invalid series length"));
    }
    let n = n as usize;
    if m < 0 || xreg.len() < n * m as usize {
        return Err(call.error("invalid regressors"));
    }
    let m = m as usize;
    let ip = ns * msp + mp;
    let iq = ns * msq + mq;
    let ir = ip.max(iq + 1);
    let np = (ir * (ir + 1)) / 2;
    let nrbar = 1.max(np * np.saturating_sub(1) / 2);
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
        phi: vec![0.0; ir],
        theta: vec![0.0; ir],
        a: vec![0.0; ir],
        covariance: vec![0.0; np],
        innovation: vec![0.0; np],
        thetab: vec![0.0; np],
        xnext: vec![0.0; np],
        xrow: vec![0.0; np],
        rbar: vec![0.0; nrbar],
        w: x[..n].to_vec(),
        wkeep: x[..n].to_vec(),
        resid: vec![0.0; n],
        reg,
    };
    let address = Box::into_raw(Box::new(starma)).cast::<c_void>();
    let tag = match starma_tag() {
        Ok(tag) => tag,
        Err(failure) => {
            drop(Box::from_raw(address.cast::<Starma>()));
            return Err(failure);
        }
    };
    let heap = heap();
    let object = match heap.alloc_external_ptr(address, tag, nil()) {
        Ok(object) => object,
        Err(failure) => {
            drop(Box::from_raw(address.cast::<Starma>()));
            return Err(failure.into());
        }
    };
    heap.root(object);
    heap.alloc_weak_ref(object, nil(), nil(), finalize as *const () as usize, false)?;
    Ok(object)
}

/// `free_starma(G)`.
pub unsafe fn free_starma(call: &NativeCall<'_>) -> R<Sexp> {
    let handle = call.arg(0);
    starma_of(call, handle)?;
    finalize(handle);
    Ok(nil())
}

/// `Starma_method(G, method)`: 1 selects the conditional sum of squares, 0 the exact
/// likelihood.
pub unsafe fn starma_method(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    g.method = values::as_integer_scalar(call.arg(1));
    Ok(nil())
}

/// `Dotrans(G, x)`: the parameters with the ARMA parts mapped to stationarity.
pub unsafe fn dotrans_routine(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    let raw = parameters(call, call.arg(1), g.parameter_count())?;
    let mut new = raw.to_vec();
    dotrans(call, g, &raw, &mut new, true)?;
    values::real_vector(&new)
}

/// `set_trans(G, trans)`.
pub unsafe fn set_trans(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    g.trans = values::as_integer_scalar(call.arg(1)) != 0;
    Ok(nil())
}

/// `arma0fa(G, params)`: half the log of the innovations variance plus half the mean log
/// prediction variance (the exact likelihood up to a constant), or half the log of the
/// conditional sum of squares per term.
pub unsafe fn arma0fa(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    let inparams = parameters(call, call.arg(1), g.parameter_count())?;
    let mut params = std::mem::take(&mut g.params);
    let transformed = dotrans(call, g, &inparams, &mut params, g.trans);
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
    if g.m > 0 {
        for i in 0..g.n {
            let mut tmp = g.wkeep[i];
            for j in 0..g.m {
                tmp -= g.reg[i + g.n * j] * g.params[streg + j];
            }
            g.w[i] = tmp;
        }
    }

    let ans = if g.method == 1 {
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
            for j in 0..(i - ncond).min(p) {
                tmp -= phi[j] * w[i - j - 1];
            }
            for j in 0..(i - ncond).min(q) {
                tmp -= theta[j] * resid[i - j - 1];
            }
            resid[i] = tmp;
            if !tmp.is_nan() {
                nu += 1;
                ssq += tmp * tmp;
            }
        }
        g.s2 = ssq / nu as f64;
        0.5 * g.s2.ln()
    } else {
        let ifault = starma(g);
        if ifault != 0 {
            return Err(call.error(format!("starma error code {ifault}")));
        }
        let mut sumlog = 0.0;
        let mut ssq = 0.0;
        let mut it = 0;
        karma(g, &mut sumlog, &mut ssq, 1, &mut it);
        g.s2 = ssq / g.nused as f64;
        0.5 * ((ssq / g.nused as f64).ln() + sumlog / g.nused as f64)
    };
    scalar_real(ans)
}

/// `get_s2(G)`: the innovations variance of the last evaluation.
pub unsafe fn get_s2(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    scalar_real(g.s2)
}

/// `get_resid(G)`: the standardized residuals of the last evaluation.
pub unsafe fn get_resid(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    values::real_vector(&g.resid[..g.n])
}

/// `arma0_kfore(G, d, sd, n.ahead)`: forecasts and their mean square errors for the model
/// differenced `d` times and seasonally `sd` times.
pub unsafe fn arma0_kfore(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    let dd = values::as_integer_scalar(call.arg(1));
    let sd = values::as_integer_scalar(call.arg(2));
    let il = values::as_integer_scalar(call.arg(3));
    if il < 0 {
        return Err(call.error("negative length vectors are not allowed"));
    }
    let ns = g.ns;
    let d = i64::from(dd) + ns as i64 * i64::from(sd);
    if d < 0 || d > i64::from(i32::MAX) {
        let ifault = if il < 1 { 11 } else { 8 };
        return Err(call.error(format!("forkal error code {ifault}")));
    }
    let d = d as usize;

    let mut del = vec![0.0; d + 1];
    let mut del2 = vec![0.0; d + 1];
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
    let mut x = vec![0.0; il];
    let mut var = vec![0.0; il];
    forkal(call, g, d, il, &del[1..], &mut x, &mut var)?;
    values::list_vector(&[values::real_vector(&x)?, values::real_vector(&var)?])
}

/// `Invtrans(G, x)`: the inverse of `Dotrans`.
pub unsafe fn invtrans(call: &NativeCall<'_>) -> R<Sexp> {
    let g = starma_of(call, call.arg(0))?;
    let raw = parameters(call, call.arg(1), g.parameter_count())?;
    let mut new = raw.to_vec();
    let mut v = 0;
    for count in [g.mp, g.mq, g.msp, g.msq] {
        invpartrans(call, &raw[v..v + count], &mut new[v..v + count])?;
        v += count;
    }
    values::real_vector(&new)
}

/// `Gradtrans(G, x)`: the Jacobian of `Dotrans`, by forward differences with step 1e-3.
pub unsafe fn gradtrans(call: &NativeCall<'_>) -> R<Sexp> {
    const EPS: f64 = 1e-3;
    let g = starma_of(call, call.arg(0))?;
    let n = g.parameter_count();
    let raw = parameters(call, call.arg(1), n)?;
    if raw.len() != n {
        return Err(call.error("wrong number of parameters"));
    }
    let mut jacobian = vec![0.0; n * n];
    for i in 0..n {
        jacobian[i + i * n] = 1.0;
    }
    // GNU R writes the MA block's derivatives into the first columns rather than the block's
    // own (column `j`, not `j + v`), and `arima0`'s standard errors depend on that.
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
            partrans(call, &w1, &mut w2)?;
            for i in 0..count {
                w1[i] += EPS;
                partrans(call, &w1, &mut w3)?;
                for j in 0..count {
                    jacobian[i + v + (j + column) * n] = (w3[j] - w2[j]) / EPS;
                }
                w1[i] -= EPS;
            }
        }
    }
    real_matrix(&jacobian, n, n)
}

/// The doubles of a parameter vector, which must hold at least `count` of them.
unsafe fn parameters(
    call: &NativeCall<'_>,
    x: Sexp,
    count: usize,
) -> R<std::borrow::Cow<'static, [f64]>> {
    if length(x) < count {
        return Err(call.error("too few parameters"));
    }
    real_data(x)
}

fn dotrans(call: &NativeCall<'_>, g: &Starma, raw: &[f64], new: &mut [f64], trans: bool) -> R<()> {
    let n = g.parameter_count();
    new[..n].copy_from_slice(&raw[..n]);
    if trans {
        let mut v = 0;
        for count in [g.mp, g.mq, g.msp, g.msq] {
            partrans(call, &raw[v..v + count], &mut new[v..v + count])?;
            v += count;
        }
    }
    Ok(())
}

/// Maps unconstrained values to the coefficients of a stationary autoregression: `tanh` gives
/// partial autocorrelations in (-1, 1), and the Durbin-Levinson recursion turns them into
/// coefficients.
fn partrans(call: &NativeCall<'_>, raw: &[f64], new: &mut [f64]) -> R<()> {
    let p = raw.len();
    if p > 100 {
        return Err(call.error("can only transform 100 pars in arima0"));
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

fn invpartrans(call: &NativeCall<'_>, phi: &[f64], new: &mut [f64]) -> R<()> {
    let p = phi.len();
    if p > 100 {
        return Err(call.error("can only transform 100 pars in arima0"));
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
        // SAFETY: `atanh` is a pure C library function.
        *value = unsafe { atanh(*value) };
    }
    Ok(())
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
    if nrbar != np * (np - 1) / 2 {
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
            for j in 0..p.min(ii) {
                et -= phi[j] * w[ii - j - 1];
            }
            for j in 0..ii.min(q) {
                et -= theta[j] * resid[ii - j - 1];
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
    call: &NativeCall<'_>,
    g: &mut Starma,
    d: usize,
    il: usize,
    delta: &[f64],
    y: &mut [f64],
    amse: &mut [f64],
) -> R<()> {
    let (p, q, r, n, np) = (g.p, g.q, g.r, g.n, g.np);
    let rd = r + d;
    let rz = rd * (rd + 1) / 2;
    let mut store = vec![0.0; rd];
    g.a = vec![0.0; rd];
    g.covariance = vec![0.0; rz];

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
        return Err(call.error(format!("forkal error code {ifault}")));
    }

    if r == 1 {
        g.a[0] = 0.0;
        g.innovation[0] = 1.0;
        g.covariance[0] = 1.0 / (1.0 - g.phi[0] * g.phi[0]);
    } else {
        let ifault = starma(g);
        if ifault != 0 {
            return Err(call.error(format!("forkal error code {ifault}")));
        }
    }

    if d > 0 && n < d + 1 {
        return Err(call.error("too few observations to forecast"));
    }
    let nt = n - d;
    if d > 0 {
        for j in 0..d {
            store[j] = g.w[n - j - 2];
            if store[j].is_nan() {
                return Err(call.error(format!("missing value in last {d} observations")));
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
