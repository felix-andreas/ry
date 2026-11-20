// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's Fortran and C sources, and
// keep their copyright notices:
//
// GNU R: src/library/stats/src/ppr.f (smart, subfit, fulfit, onetrm, oneone, pprdir, ppconj,
// pprder, pool, newb, setppr, setsmu, fsort, pppred, supsmu, spline, sort)
//     Modified from the SMART package by J. H. Friedman, 10/10/84, by B. D. Ripley, 1994-7;
//     the super smoother coded and copyright (c) 1984 by Jerome H. Friedman
//
// GNU R: src/library/stats/src/ksmooth.c (splineprt)
//     Copyright (C) 2003-2026 The R Core team
//     Copyright (C) 1998-2026 The R Foundation
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! Projection pursuit regression behind `ppr()`: Friedman's SMART with B. D. Ripley's spline
//! smoothers, fitting `y ~ sum(beta[l] * f[l](alpha[l]' x))` one ridge term at a time, then
//! refitting the terms together and dropping the least important until `nterms` remain.
//!
//! Every loop keeps the operation order of GNU R's `ppr.f`, because the direction search and
//! the term pruning branch on comparisons whose outcome depends on the last bits.
//!
//! The Fortran keeps its parameters in common blocks that `setppr` and `setsmu` set before a
//! fit; [`SETTINGS`] holds them between calls.

use {
    crate::{
        routines::{fortran_result, new_reals, read_reals, super_smooth},
        spline::{SearchControl, Smoother},
    },
    interp::{
        handles::{self, Cx, Local, NativeArgs},
        unwind::{R, check_user_interrupt, error},
    },
    numeric::c_format::{c_general, push_fixed},
    std::cell::Cell,
};

/// The `pprpar` and `spsmooth` common blocks: what `setppr`, and for the smoother's part
/// `setsmu`, last set.
#[derive(Clone, Copy)]
struct Settings {
    span: f64,
    alpha: f64,
    optlevel: i32,
    smoother: SmootherSettings,
}

/// The `spsmooth` common block, which `ppr()` and `supsmu()` share.
#[derive(Clone, Copy)]
pub(crate) struct SmootherSettings {
    df: f64,
    gcvpen: f64,
    /// 0 for the super smoother, 1 for a spline with `df` degrees of freedom, 2 (or any other
    /// value) for a spline chosen by GCV.
    method: i32,
    pub(crate) trace: bool,
}

std::thread_local! {
    static SETTINGS: Cell<Settings> = const {
        Cell::new(Settings {
            span: 0.0,
            alpha: 0.0,
            optlevel: 2,
            smoother: SmootherSettings { df: 4.0, gcvpen: 1.0, method: 0, trace: false },
        })
    };
}

/// `big` in `pprpar`, a single-precision literal in the Fortran.
const BIG: f64 = 1.0e20_f32 as f64;
const CONV: f64 = 0.005;
const MAX_ITERATIONS: i32 = 20;
const MITONE: i32 = 20;
const CUTMIN: f64 = 0.1;
const FDEL: f64 = 0.02;
const CJEPS: f64 = 0.001;
const MITCJ: i32 = 1;
const IFL: i32 = 6;

/// `.Fortran(C_setppr, span, bass, optlevel, ism, df, gcvpen)`: the smoother and optimization
/// level of the next `smart`. A negative `ism` is `-(method + 1)` with tracing.
pub fn setppr<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let integer =
        |context: &mut Cx<'s>, index| handles::integer_scalar(context, arguments.value(index));
    let real = |context: &mut Cx<'s>, index| handles::real_scalar(context, arguments.value(index));
    let ism = integer(context, 3);
    SETTINGS.set(Settings {
        span: real(context, 0),
        alpha: real(context, 1),
        optlevel: integer(context, 2),
        smoother: SmootherSettings {
            df: real(context, 4),
            gcvpen: real(context, 5),
            method: if ism >= 0 { ism } else { -(ism + 1) },
            trace: ism < 0,
        },
    });
    fortran_result(context, arguments, &[])
}

/// `setsmu`: `supsmu()` selects the super smoother and sets whether it traces.
pub(crate) fn set_supsmu(trace: bool) {
    let mut settings = SETTINGS.get();
    settings.smoother.method = 0;
    settings.smoother.trace = trace;
    SETTINGS.set(settings);
}

/// The smoother part of the settings, as `supsmu` reads it.
pub(crate) fn smoother_settings() -> SmootherSettings {
    SETTINGS.get().smoother
}

/// `supsmu` in `ppr.f`: the super smoother over `x` sorted increasingly, or the penalized
/// regression spline when the settings select one and `x` is not constant. Returns the
/// equivalent degrees of freedom where the smoother sets them: those of the spline, and zero
/// for a super smoother with cross-validated spans.
#[allow(clippy::too_many_arguments)]
pub(crate) fn supsmu(
    x: &[f64],
    y: &[f64],
    w: &[f64],
    iper: i32,
    span: f64,
    alpha: f64,
    settings: &SmootherSettings,
    smo: &mut [f64],
    log: &mut String,
) -> R<Option<f64>> {
    let (Some(&first), Some(&last)) = (x.first(), x.last()) else {
        return Ok(None);
    };
    if settings.method != 0 && last > first {
        return spline(x, y, w, settings, smo, log).map(Some);
    }
    Ok(super_smooth(x, y, w, iper, span, alpha, settings.trace, smo, log).then_some(0.0))
}

/// `.Fortran(C_smart, m, mu, p, q, n, w, x, y, ww, smod, nsmod, sp, nsp, dp, ndp, edf)`: fits up
/// to `m` ridge terms, then prunes them to `mu`, writing the model to `smod`.
///
/// `smod` holds `m, p, q, n`, the number of terms fitted, the response means, the response
/// scale, then the directions `a` (p by m), the response loadings `b` (q by m), the ridge
/// functions `f` and projections `t` (n by m each), and the fit criteria `asr`.
pub fn smart<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let mut count =
        |index: usize| handles::integer_scalar(context, arguments.value(index)).max(0) as usize;
    let (m, mu, p, q, n) = (count(0), count(1), count(2), count(3), count(4));
    let w = read_reals(context, arguments.value(5))?;
    let x = read_reals(context, arguments.value(6))?;
    let y = read_reals(context, arguments.value(7))?;
    let ww = read_reals(context, arguments.value(8))?;
    let mut smod = read_reals(context, arguments.value(9))?;
    let mut edf = read_reals(context, arguments.value(15))?;
    let model_length = q + 7 + m * (p + q + 2 * n) + m;
    if w.len() < n
        || x.len() < p * n
        || y.len() < q * n
        || ww.len() < q
        || smod.len() < model_length
        || edf.len() < m
        || p == 0
        || q == 0
    {
        return Err(error("invalid arguments to 'smart'"));
    }
    let settings = SETTINGS.get();
    let mut controls = Controls {
        span: settings.span,
        alpha: settings.alpha,
        lf: settings.optlevel,
        ifl: IFL,
        cutmin: CUTMIN,
        mitone: MITONE,
        smoother: settings.smoother,
    };
    smod[0] = m as f64;
    smod[1] = p as f64;
    smod[2] = q as f64;
    smod[3] = n as f64;
    let (header, rest) = smod.split_at_mut(q + 6);
    let (a, rest) = rest.split_at_mut(p * m);
    let (b, rest) = rest.split_at_mut(q * m);
    let (f, rest) = rest.split_at_mut(n * m);
    let (t, rest) = rest.split_at_mut(n * m);
    let mut terms = Terms {
        a,
        b,
        f,
        t,
        asr: &mut rest[..m + 1],
        edf: &mut edf[..m],
    };
    let problem = Problem {
        n,
        p,
        q,
        w: &w[..n],
        sw: 0.0,
        x: &x[..p * n],
        ww: &ww[..q],
    };
    let mut scratch = Scratch::new(n, p);
    let fitted = smart1(
        m,
        mu,
        problem,
        &y[..q * n],
        header,
        &mut terms,
        &mut controls,
        &mut scratch,
    );
    if !scratch.log.is_empty() {
        handles::write_output(context, scratch.log.as_bytes());
    }
    fitted?;
    let smod = new_reals(context, &smod)?;
    let edf = new_reals(context, &edf)?;
    fortran_result(context, arguments, &[(9, smod), (15, edf)])
}

/// `.Fortran(C_pppred, np, x, smod, y, sc)`: the fitted model of `smod` at each of the `np` rows
/// of `x`, written to `y` (np by q).
pub fn pppred<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let np = handles::integer_scalar(context, arguments.value(0)).max(0) as usize;
    let x = read_reals(context, arguments.value(1))?;
    let mut smod = read_reals(context, arguments.value(2))?;
    let header = |index: usize| {
        smod.get(index)
            .map_or(0, |&value| (value + 0.1).max(0.0) as usize)
    };
    let (m, p, q, n, mu) = (header(0), header(1), header(2), header(3), header(4));
    // A valid model is longer than each of its sizes, so bounding them first keeps the offsets
    // below from overflowing on a crafted `smod`.
    if [m, p, q, n].iter().any(|&size| size > smod.len()) {
        return Err(error("invalid 'smod' in 'pppred'"));
    }
    let jf = q + 6 + p * m + m * q;
    let jt = jf + n * m;
    if smod.len() < jt + n * m || mu > m || x.len() < np * p {
        return Err(error("invalid 'smod' in 'pppred'"));
    }
    let ys = smod[q + 5];
    let (front, back) = smod.split_at_mut(jt);
    fsort(mu, n, &mut front[jf..], back);
    let at = |index: usize| {
        index
            .checked_sub(1)
            .and_then(|index| smod.get(index))
            .copied()
            .unwrap_or(f64::NAN)
    };
    let mut y = vec![0.0; np * q];
    for inp in 1..=np {
        let mut ja = q + 6;
        let mut jb = ja + p * m;
        let mut jf = jb + m * q;
        let mut jt = jf + n * m;
        let output = |i: usize| (i - 1) * np + inp - 1;
        for i in 1..=q {
            y[output(i)] = 0.0;
        }
        for _ in 1..=mu {
            let mut s = 0.0;
            for j in 1..=p {
                s += at(ja + j) * x[(j - 1) * np + inp - 1];
            }
            let value = if !(s > at(jt + 1)) {
                at(jf + 1)
            } else if !(s < at(jt + n)) {
                at(jf + n)
            } else {
                let (mut low, mut high) = (0, n + 1);
                loop {
                    if low + 1 >= high {
                        let (jfl, jfh, jtl, jth) = (jf + low, jf + high, jt + low, jt + high);
                        break at(jfl) + (at(jfh) - at(jfl)) * (s - at(jtl)) / (at(jth) - at(jtl));
                    }
                    let place = (low + high) / 2;
                    let t = at(jt + place);
                    if s == t {
                        break at(jf + place);
                    }
                    if s < t {
                        high = place;
                    } else {
                        low = place;
                    }
                }
            };
            for i in 1..=q {
                y[output(i)] += at(jb + i) * value;
            }
            ja += p;
            jb += q;
            jf += n;
            jt += n;
        }
        for i in 1..=q {
            y[output(i)] = ys * y[output(i)] + at(i + 5);
        }
    }
    let smod = new_reals(context, &smod)?;
    let y = new_reals(context, &y)?;
    fortran_result(context, arguments, &[(2, smod), (3, y)])
}

/// The `pprpar` and `pprz01` controls for one fit. `fulfit` lowers `cutmin` and `mitone`, and
/// `subfit` clears `ifl`, while they run.
struct Controls {
    span: f64,
    alpha: f64,
    lf: i32,
    ifl: i32,
    cutmin: f64,
    mitone: i32,
    smoother: SmootherSettings,
}

/// The data of one fit: `x` is p by n and `w` and `ww` weight the observations and responses.
#[derive(Clone, Copy)]
struct Problem<'a> {
    n: usize,
    p: usize,
    q: usize,
    w: &'a [f64],
    sw: f64,
    x: &'a [f64],
    ww: &'a [f64],
}

/// The ridge terms inside `smod`, with their equivalent degrees of freedom.
struct Terms<'a> {
    a: &'a mut [f64],
    b: &'a mut [f64],
    f: &'a mut [f64],
    t: &'a mut [f64],
    asr: &'a mut [f64],
    edf: &'a mut [f64],
}

/// The columns of the Fortran's `sc` (n by 15), `g` (p by 3), `bt` and `dp` work arrays that
/// carry values from one call of a routine to the next.
struct Scratch {
    /// Column 1: the observations in order of their projection.
    order: Vec<usize>,
    /// Columns 2 and 3: the response and weights in that order.
    sorted_y: Vec<f64>,
    sorted_w: Vec<f64>,
    /// Columns 4 to 6: the ridge function's derivative and the direction update's inputs.
    derivative: Vec<f64>,
    residual: Vec<f64>,
    gradient: Vec<f64>,
    /// Columns 11 and 12: the sorted projections and their smooth.
    projection: Vec<f64>,
    smooth: Vec<f64>,
    /// Column 13: the responses combined by their loadings.
    response: Vec<f64>,
    /// `g(.,1)` and `g(.,2)`: the direction step and the candidate direction.
    step: Vec<f64>,
    candidate: Vec<f64>,
    /// `pprder`'s pooled copies of the projections, smooth and weights.
    pooled: [Vec<f64>; 3],
    dp: Vec<f64>,
    log: String,
}

/// The term `fulfit` refits: `g(.,3)`, `bt` and columns 14 and 15 of `sc`.
struct Trial {
    a: Vec<f64>,
    b: Vec<f64>,
    f: Vec<f64>,
    t: Vec<f64>,
}

impl Scratch {
    fn new(n: usize, p: usize) -> Scratch {
        Scratch {
            order: vec![0; n],
            sorted_y: vec![0.0; n],
            sorted_w: vec![0.0; n],
            derivative: vec![0.0; n],
            residual: vec![0.0; n],
            gradient: vec![0.0; n],
            projection: vec![0.0; n],
            smooth: vec![0.0; n],
            response: vec![0.0; n],
            step: vec![0.0; p],
            candidate: vec![0.0; p],
            pooled: std::array::from_fn(|_| vec![0.0; n]),
            dp: vec![0.0; p * (p + 1) / 2 + 6 * p],
            log: String::new(),
        }
    }
}

/// `smart1`: standardizes the responses, adds terms with `subfit`, refits them with `fulfit`,
/// then drops the least important term and refits until `mu` remain.
#[allow(clippy::too_many_arguments)]
fn smart1(
    m: usize,
    mu: usize,
    mut problem: Problem<'_>,
    y: &[f64],
    header: &mut [f64],
    terms: &mut Terms<'_>,
    controls: &mut Controls,
    scratch: &mut Scratch,
) -> R<()> {
    let Problem { n, p, q, .. } = problem;
    let (w, ww) = (problem.w, problem.ww);
    let mut sw = 0.0;
    for j in 0..n {
        sw += w[j];
    }
    problem.sw = sw;
    let mut r = y.to_vec();
    let (yb, ys) = header[5..].split_at_mut(q);
    let ys = &mut ys[0];
    for i in 0..q {
        let mut s = 0.0;
        for j in 0..n {
            s += w[j] * r[j * q + i];
        }
        yb[i] = s / sw;
    }
    for j in 0..n {
        for i in 0..q {
            r[j * q + i] -= yb[i];
        }
    }
    *ys = 0.0;
    for i in 0..q {
        let mut s = 0.0;
        for j in 0..n {
            s += w[j] * (r[j * q + i] * r[j * q + i]);
        }
        *ys += ww[i] * s / sw;
    }
    if !(*ys > 0.0) {
        return Ok(());
    }
    *ys = ys.sqrt();
    let s = 1.0 / *ys;
    for value in r.iter_mut() {
        *value *= s;
    }

    let mut trial = Trial {
        a: vec![0.0; p],
        b: vec![0.0; q],
        f: vec![0.0; n],
        t: vec![0.0; n],
    };
    let mut lm = subfit(m, problem, &mut r, terms, controls, scratch, &mut trial)?;
    if controls.lf > 0 {
        let lf = controls.lf;
        fulfit(
            lm, lf, problem, &mut r, terms, controls, scratch, &mut trial,
        )?;
        let mut order = vec![0; lm];
        let mut importance = vec![0.0; lm];
        loop {
            order.truncate(lm);
            importance.truncate(lm);
            for l in 1..=lm {
                order[l - 1] = l;
                let mut s = 0.0;
                for i in 0..q {
                    s += ww[i] * terms.b[(l - 1) * q + i].abs();
                }
                importance[l - 1] = -s;
            }
            sort(&mut importance, &mut order);
            r.copy_from_slice(y);
            for i in 0..q {
                for j in 0..n {
                    r[j * q + i] -= yb[i];
                    let mut s = 0.0;
                    for l in 0..lm {
                        s += terms.b[l * q + i] * terms.f[l * n + j];
                    }
                    r[j * q + i] = r[j * q + i] / *ys - s;
                }
            }
            if lm <= mu {
                break;
            }
            let l = order[lm - 1];
            let mut asr1 = 0.0;
            for j in 0..n {
                for i in 0..q {
                    r[j * q + i] += terms.b[(l - 1) * q + i] * terms.f[(l - 1) * n + j];
                    asr1 += w[j] * ww[i] * (r[j * q + i] * r[j * q + i]);
                }
            }
            asr1 /= sw;
            terms.asr[0] = asr1;
            if l < lm {
                terms.a.copy_within((lm - 1) * p..lm * p, (l - 1) * p);
                terms.b.copy_within((lm - 1) * q..lm * q, (l - 1) * q);
                terms.f.copy_within((lm - 1) * n..lm * n, (l - 1) * n);
                terms.t.copy_within((lm - 1) * n..lm * n, (l - 1) * n);
            }
            lm -= 1;
            fulfit(
                lm, lf, problem, &mut r, terms, controls, scratch, &mut trial,
            )?;
        }
    }
    header[4] = lm as f64;
    Ok(())
}

/// `subfit`: adds up to `m` terms one at a time to the residuals `r`, refitting the terms so
/// far after each, until the fit improves by less than `conv`. Returns the number of terms.
fn subfit(
    m: usize,
    problem: Problem<'_>,
    r: &mut [f64],
    terms: &mut Terms<'_>,
    controls: &mut Controls,
    scratch: &mut Scratch,
    trial: &mut Trial,
) -> R<usize> {
    let Problem { n, p, q, .. } = problem;
    terms.asr[0] = BIG;
    let mut lm = 0;
    for _ in 1..=m {
        check_user_interrupt()?;
        lm += 1;
        let asrold = terms.asr[0];
        newb(lm, q, problem.ww, terms.b);
        let column = lm - 1;
        onetrm(
            0,
            problem,
            r,
            &mut terms.a[column * p..lm * p],
            &mut terms.b[column * q..lm * q],
            &mut terms.f[column * n..lm * n],
            &mut terms.t[column * n..lm * n],
            &mut terms.asr[0],
            controls,
            scratch,
            &mut terms.edf[0],
        )?;
        for j in 0..n {
            for i in 0..q {
                r[j * q + i] -= terms.b[column * q + i] * terms.f[column * n + j];
            }
        }
        if lm == 1 {
            continue;
        }
        if controls.lf > 0 {
            if lm == m {
                return Ok(lm);
            }
            let saved = controls.ifl;
            controls.ifl = 0;
            fulfit(lm, 1, problem, r, terms, controls, scratch, trial)?;
            controls.ifl = saved;
        }
        if terms.asr[0] <= 0.0 || (asrold - terms.asr[0]) / asrold < CONV {
            return Ok(lm);
        }
    }
    Ok(lm)
}

/// `fulfit`: refits each of the first `lm` terms in turn against the residuals of the others,
/// keeping a refit only when it lowers the criterion, until the fit converges.
#[allow(clippy::too_many_arguments)]
fn fulfit(
    lm: usize,
    lbf: i32,
    problem: Problem<'_>,
    r: &mut [f64],
    terms: &mut Terms<'_>,
    controls: &mut Controls,
    scratch: &mut Scratch,
    trial: &mut Trial,
) -> R<()> {
    let Problem { n, p, q, .. } = problem;
    if lbf <= 0 {
        return Ok(());
    }
    let mut asri = terms.asr[0];
    let (saved_cutmin, saved_mitone) = (controls.cutmin, controls.mitone);
    if lbf < 3 {
        controls.cutmin = 1.0;
        controls.mitone = lbf - 1;
    }
    let mut iter = 0;
    loop {
        let asrold = asri;
        iter += 1;
        for lp in 0..lm {
            trial.b.copy_from_slice(&terms.b[lp * q..(lp + 1) * q]);
            trial.a.copy_from_slice(&terms.a[lp * p..(lp + 1) * p]);
            for j in 0..n {
                for i in 0..q {
                    r[j * q + i] += trial.b[i] * terms.f[lp * n + j];
                }
            }
            onetrm(
                1,
                problem,
                r,
                &mut trial.a,
                &mut trial.b,
                &mut trial.f,
                &mut trial.t,
                &mut asri,
                controls,
                scratch,
                &mut terms.edf[lp],
            )?;
            if asri < asrold {
                terms.b[lp * q..(lp + 1) * q].copy_from_slice(&trial.b);
                terms.a[lp * p..(lp + 1) * p].copy_from_slice(&trial.a);
                terms.f[lp * n..(lp + 1) * n].copy_from_slice(&trial.f);
                terms.t[lp * n..(lp + 1) * n].copy_from_slice(&trial.t);
            } else {
                asri = asrold;
            }
            for j in 0..n {
                for i in 0..q {
                    r[j * q + i] -= terms.b[lp * q + i] * terms.f[lp * n + j];
                }
            }
        }
        if !(iter <= MAX_ITERATIONS && asri > 0.0 && (asrold - asri) / asrold >= CONV) {
            break;
        }
    }
    controls.cutmin = saved_cutmin;
    controls.mitone = saved_mitone;
    if controls.ifl > 0 {
        terms.asr[lm] = asri;
        terms.asr[0] = asri;
    }
    Ok(())
}

/// `onetrm`: fits one term to the q responses `y` (q by n), alternating between the ridge
/// function of the combined response and the loadings `b`, until the criterion converges.
#[allow(clippy::too_many_arguments)]
fn onetrm(
    jfl: i32,
    problem: Problem<'_>,
    y: &[f64],
    a: &mut [f64],
    b: &mut [f64],
    f: &mut [f64],
    t: &mut [f64],
    asr: &mut f64,
    controls: &Controls,
    scratch: &mut Scratch,
    edf: &mut f64,
) -> R<()> {
    let Problem {
        n, q, w, sw, ww, ..
    } = problem;
    let mut iter = 0;
    *asr = BIG;
    let mut response = std::mem::take(&mut scratch.response);
    loop {
        iter += 1;
        let asrold = *asr;
        for j in 0..n {
            let mut s = 0.0;
            for i in 0..q {
                s += ww[i] * b[i] * y[j * q + i];
            }
            response[j] = s;
        }
        let fitted = oneone(
            jfl.max(iter - 1),
            problem,
            &response,
            a,
            f,
            t,
            asr,
            controls,
            scratch,
            edf,
        );
        if let Err(failure) = fitted {
            scratch.response = response;
            return Err(failure);
        }
        for i in 0..q {
            let mut s = 0.0;
            for j in 0..n {
                s += w[j] * y[j * q + i] * f[j];
            }
            b[i] = s / sw;
        }
        *asr = 0.0;
        for i in 0..q {
            let mut s = 0.0;
            for j in 0..n {
                let residual = y[j * q + i] - b[i] * f[j];
                s += w[j] * (residual * residual);
            }
            *asr += ww[i] * s / sw;
        }
        if !(q != 1 && iter <= MAX_ITERATIONS && *asr > 0.0 && (asrold - *asr) / asrold >= CONV) {
            break;
        }
    }
    scratch.response = response;
    Ok(())
}

/// `oneone`: fits one ridge function to the response `y`, searching for the direction `a` by
/// Gauss-Newton steps that are halved while they fail to lower the criterion. `ist <= 0`
/// starts from the least squares direction. The ridge function `f` is centered and scaled to
/// unit weighted variance; `t` holds the projections.
#[allow(clippy::too_many_arguments)]
fn oneone(
    ist: i32,
    problem: Problem<'_>,
    y: &[f64],
    a: &mut [f64],
    f: &mut [f64],
    t: &mut [f64],
    asr: &mut f64,
    controls: &Controls,
    scratch: &mut Scratch,
    edf: &mut f64,
) -> R<()> {
    let Problem { n, p, w, sw, x, .. } = problem;
    let sml = 1.0 / BIG;
    if ist <= 0 {
        if p <= 1 {
            a[0] = 1.0;
        }
        scratch.sorted_y.fill(1.0);
        pprdir(problem, y, &scratch.sorted_y, a, &mut scratch.dp);
    }
    let mut s = 0.0;
    for i in 0..p {
        scratch.step[i] = 0.0;
        s += a[i] * a[i];
    }
    s = 1.0 / s.sqrt();
    for i in 0..p {
        a[i] *= s;
    }
    let mut iter = 0;
    *asr = BIG;
    let mut cut = 1.0;
    'search: loop {
        iter += 1;
        let asrold = *asr;
        let s = loop {
            let mut s = 0.0;
            for i in 0..p {
                scratch.candidate[i] = a[i] + scratch.step[i];
                s += scratch.candidate[i] * scratch.candidate[i];
            }
            s = 1.0 / s.sqrt();
            for i in 0..p {
                scratch.candidate[i] *= s;
            }
            for j in 0..n {
                scratch.order[j] = j + 1;
                let mut s = 0.0;
                for i in 0..p {
                    s += scratch.candidate[i] * x[j * p + i];
                }
                scratch.projection[j] = s;
            }
            sort(&mut scratch.projection, &mut scratch.order);
            for j in 0..n {
                let k = scratch.order[j] - 1;
                scratch.sorted_y[j] = y[k];
                scratch.sorted_w[j] = w[k].max(sml);
            }
            if let Some(value) = supsmu(
                &scratch.projection,
                &scratch.sorted_y,
                &scratch.sorted_w,
                1,
                controls.span,
                controls.alpha,
                &controls.smoother,
                &mut scratch.smooth,
                &mut scratch.log,
            )? {
                *edf = value;
            }
            let mut s = 0.0;
            for j in 0..n {
                let residual = scratch.sorted_y[j] - scratch.smooth[j];
                s += scratch.sorted_w[j] * (residual * residual);
            }
            s /= sw;
            if s < *asr {
                break s;
            }
            cut *= 0.5;
            if cut < controls.cutmin {
                break 'search;
            }
            for i in 0..p {
                scratch.step[i] *= cut;
            }
        };
        *asr = s;
        cut = 1.0;
        a.copy_from_slice(&scratch.candidate);
        for j in 0..n {
            let k = scratch.order[j] - 1;
            t[k] = scratch.projection[j];
            f[k] = scratch.smooth[j];
        }
        if *asr <= 0.0 || (asrold - *asr) / asrold < CONV {
            break;
        }
        if iter > controls.mitone || p <= 1 {
            break;
        }
        pprder(
            &scratch.projection,
            &scratch.smooth,
            &scratch.sorted_w,
            FDEL,
            &mut scratch.derivative,
            &mut scratch.pooled,
        )?;
        for j in 0..n {
            let k = scratch.order[j] - 1;
            scratch.residual[j] = y[j] - f[j];
            scratch.gradient[k] = scratch.derivative[j];
        }
        pprdir(
            problem,
            &scratch.residual,
            &scratch.gradient,
            &mut scratch.step,
            &mut scratch.dp,
        );
    }
    let mut s = 0.0;
    let mut v = s;
    for j in 0..n {
        s += w[j] * f[j];
    }
    s /= sw;
    for j in 0..n {
        f[j] -= s;
        v += w[j] * (f[j] * f[j]);
    }
    if v > 0.0 {
        v = 1.0 / (v / sw).sqrt();
        for j in 0..n {
            f[j] *= v;
        }
    }
    Ok(())
}

/// `pprdir`: the Gauss-Newton direction `e` for the residuals `r` with derivatives `d`, from
/// the normal equations solved by [`ppconj`]. `g` is `dp`.
fn pprdir(problem: Problem<'_>, r: &[f64], d: &[f64], e: &mut [f64], g: &mut [f64]) {
    let Problem { n, p, w, sw, x, .. } = problem;
    for i in 0..p {
        let mut s = 0.0;
        for j in 0..n {
            s += w[j] * d[j] * x[j * p + i];
        }
        e[i] = s / sw;
    }
    let mut k = 0;
    let m1 = p * (p + 1) / 2;
    for j in 0..p {
        let mut s = 0.0;
        for l in 0..n {
            s += w[l] * r[l] * (d[l] * x[l * p + j] - e[j]);
        }
        g[m1 + j] = s / sw;
        for i in 0..=j {
            let mut s = 0.0;
            for l in 0..n {
                s += w[l] * (d[l] * x[l * p + i] - e[i]) * (d[l] * x[l * p + j] - e[j]);
            }
            g[k] = s / sw;
            k += 1;
        }
    }
    let (packed, rest) = g.split_at_mut(m1);
    let (c, rest) = rest.split_at_mut(p);
    let (solution, sc) = rest.split_at_mut(p);
    ppconj(p, packed, c, solution, CJEPS, MITCJ, sc);
    e[..p].copy_from_slice(solution);
}

/// `ppconj`: solves `G x = c` for the symmetric `G` packed by columns of its upper triangle, by
/// conjugate gradients restarted until `x` moves by less than `eps` or `maxit` restarts.
fn ppconj(p: usize, g: &[f64], c: &[f64], x: &mut [f64], eps: f64, maxit: i32, sc: &mut [f64]) {
    let packed = |row: usize, column: usize| g[column * (column - 1) / 2 + row - 1];
    let (residual, rest) = sc.split_at_mut(p);
    let (direction, rest) = rest.split_at_mut(p);
    let (product, previous) = rest.split_at_mut(p);
    let times = |i: usize, vector: &[f64]| {
        let mut s = packed(i, i) * vector[i - 1];
        for j in 1..i {
            s += packed(j, i) * vector[j - 1];
        }
        for j in i + 1..=p {
            s += packed(i, j) * vector[j - 1];
        }
        s
    };
    for i in 0..p {
        x[i] = 0.0;
        direction[i] = 0.0;
    }
    let mut nit = 0;
    loop {
        nit += 1;
        let mut h = 0.0;
        let mut beta = 0.0;
        for i in 1..=p {
            previous[i - 1] = x[i - 1];
            let s = times(i, x);
            residual[i - 1] = s - c[i - 1];
            h += residual[i - 1] * residual[i - 1];
        }
        if h <= 0.0 {
            return;
        }
        for _ in 1..=p {
            for i in 0..p {
                direction[i] = beta * direction[i] - residual[i];
            }
            let mut t = 0.0;
            for i in 1..=p {
                let s = times(i, direction);
                product[i - 1] = s;
                t += s * direction[i - 1];
            }
            let alpha = h / t;
            let mut s = 0.0;
            for i in 0..p {
                x[i] += alpha * direction[i];
                residual[i] += alpha * product[i];
                s += residual[i] * residual[i];
            }
            if s <= 0.0 {
                break;
            }
            beta = s / h;
            h = s;
        }
        let mut s = 0.0_f64;
        for i in 0..p {
            s = fortran_max(s, (x[i] - previous[i]).abs());
        }
        if !(s >= eps && nit < maxit) {
            return;
        }
    }
}

/// `pprder`: the derivative `d` of the smooth `s` over the sorted projections `x`, by divided
/// differences between neighbouring groups after [`pool`] merges groups closer than `fdel`
/// times the interquartile range.
fn pprder(
    x: &[f64],
    s: &[f64],
    w: &[f64],
    fdel: f64,
    d: &mut [f64],
    pooled: &mut [Vec<f64>; 3],
) -> R<()> {
    let n = x.len();
    let (Some(&first), Some(&last)) = (x.first(), x.last()) else {
        return Ok(());
    };
    if !(last > first) {
        d.fill(0.0);
        return Ok(());
    }
    let at = |i: usize| x[i.clamp(1, n) - 1];
    let mut i = n / 4;
    let mut j = 3 * i;
    let mut scale = at(j) - at(i);
    while !(scale > 0.0) {
        if j < n {
            j += 1;
        }
        if i > 1 {
            i -= 1;
        }
        scale = at(j) - at(i);
    }
    let del = fdel * scale * 2.0;
    let [px, py, pw] = pooled;
    px.copy_from_slice(x);
    py.copy_from_slice(s);
    pw.copy_from_slice(w);
    pool(px, py, pw, del);
    let slope = |to: usize, from: usize| (py[to - 1] - py[from - 1]) / (px[to - 1] - px[from - 1]);
    let (mut bl, mut el, mut bc, mut ec) = (0, 0, 0, 0);
    let mut er = 0;
    loop {
        let br = er + 1;
        er = br;
        if br > n {
            return Err(error("br is too large"));
        }
        while er < n && px[br - 1] == px[er] {
            er += 1;
        }
        if br == 1 {
            bl = br;
            el = er;
            continue;
        }
        if bc == 0 {
            bc = br;
            ec = er;
            for j in bl..=el {
                d[j - 1] = slope(bc, bl);
            }
            continue;
        }
        for j in bc..=ec {
            d[j - 1] = slope(br, bl);
        }
        if er == n {
            for j in br..=er {
                d[j - 1] = slope(br, bc);
            }
            return Ok(());
        }
        bl = bc;
        el = ec;
        bc = br;
        ec = er;
    }
}

/// `pool`: merges each group of tied `x` with its nearer neighbour while they are closer than
/// `del`, replacing both by their weighted mean.
fn pool(x: &mut [f64], y: &mut [f64], w: &mut [f64], del: f64) {
    let n = x.len();
    let at = |vector: &[f64], i: usize| vector[i - 1];
    let merge = |x: &mut [f64], y: &mut [f64], w: &mut [f64], bb: usize, eb: usize| {
        let pw = w[bb - 1] + w[eb - 1];
        let px = (x[bb - 1] * w[bb - 1] + x[eb - 1] * w[eb - 1]) / pw;
        let py = (y[bb - 1] * w[bb - 1] + y[eb - 1] * w[eb - 1]) / pw;
        for i in bb..=eb {
            x[i - 1] = px;
            y[i - 1] = py;
            w[i - 1] = pw;
        }
    };
    let mut eb = 0;
    'blocks: while eb < n {
        let mut bb = eb + 1;
        eb = bb;
        while eb < n && at(x, bb) == at(x, eb + 1) {
            eb += 1;
        }
        if eb < n && !(at(x, eb + 1) - at(x, eb) >= del) {
            let br = eb + 1;
            let mut er = br;
            while er < n && at(x, er + 1) == at(x, br) {
                er += 1;
            }
            if er < n && at(x, er + 1) - at(x, er) < at(x, eb + 1) - at(x, eb) {
                continue 'blocks;
            }
            eb = er;
            merge(x, y, w, bb, eb);
        }
        while bb > 1 && !(at(x, bb) - at(x, bb - 1) >= del) {
            let mut bl = bb - 1;
            let el = bl;
            while bl > 1 && at(x, bl - 1) == at(x, el) {
                bl -= 1;
            }
            bb = bl;
            merge(x, y, w, bb, eb);
        }
    }
}

/// `newb`: the starting loadings of term `lm` (column `lm` of `b`, q by lm), orthogonalized
/// against the last q - 1 terms' loadings, or `1, 2, ..., q` when they come out constant.
fn newb(lm: usize, q: usize, ww: &[f64], b: &mut [f64]) {
    let sml = 1.0 / BIG;
    let at = |i: usize, l: usize| (l - 1) * q + i - 1;
    if q == 1 {
        b[at(1, lm)] = 1.0;
        return;
    }
    if lm == 1 {
        for i in 1..=q {
            b[at(i, lm)] = i as f64;
        }
        return;
    }
    let lm1 = lm - 1;
    for i in 1..=q {
        b[at(i, lm)] = 0.0;
    }
    let mut t = 0.0;
    for i in 1..=q {
        let mut s = 0.0;
        for l in 1..=lm1 {
            s += b[at(i, l)].abs();
        }
        b[at(i, lm)] = s;
        t += s;
    }
    for i in 1..=q {
        b[at(i, lm)] = ww[i - 1] * (t - b[at(i, lm)]);
    }
    let l1 = if lm > q { lm - q + 1 } else { 1 };
    for l in l1..=lm1 {
        let mut s = 0.0;
        let mut t = s;
        for i in 1..=q {
            s += ww[i - 1] * b[at(i, lm)] * b[at(i, l)];
            t += ww[i - 1] * (b[at(i, l)] * b[at(i, l)]);
        }
        s /= t.sqrt();
        for i in 1..=q {
            b[at(i, lm)] -= s * b[at(i, l)];
        }
    }
    for i in 2..=q {
        if (b[at(i - 1, lm)] - b[at(i, lm)]).abs() > sml {
            return;
        }
    }
    for i in 1..=q {
        b[at(i, lm)] = i as f64;
    }
}

/// `fsort`: sorts each of the first `mu` columns of `t` (n by mu) increasingly and puts the
/// matching column of `f` in the same order.
fn fsort(mu: usize, n: usize, f: &mut [f64], t: &mut [f64]) {
    let mut order = vec![0; n];
    let mut values = vec![0.0; n];
    for l in 0..mu {
        let column = l * n..(l + 1) * n;
        for j in 0..n {
            order[j] = j + 1;
            values[j] = f[column.start + j];
        }
        sort(&mut t[column.clone()], &mut order);
        for j in 0..n {
            f[column.start + j] = values[order[j] - 1];
        }
    }
}

/// `spline` and `splineAA`: the penalized regression spline on at most 15 interior knots, with
/// the smoothing parameter matched to `df` degrees of freedom (method 1) or chosen by GCV with
/// penalty `gcvpen`. Writes the fit to `smo` and returns its equivalent degrees of freedom.
fn spline(
    x: &[f64],
    y: &[f64],
    w: &[f64],
    settings: &SmootherSettings,
    smo: &mut [f64],
    log: &mut String,
) -> R<f64> {
    let n = x.len();
    if n < 4 {
        return Err(error("too few observations for the spline smoother"));
    }
    let dx: Vec<f64> = x
        .iter()
        .map(|&value| (value - x[0]) / (x[n - 1] - x[0]))
        .collect();
    let count = n.min(15);
    let mut knots = vec![0.0; count + 4];
    knots[..4].fill(dx[0]);
    knots[count..].fill(dx[n - 1]);
    for i in 5..=count {
        // The Fortran computes the knot's position with `real`, in single precision.
        let mut p = ((n - 1) as f32 * (i - 4) as f32 / (count - 3) as f32) as f64;
        let ip = p as usize;
        p -= ip as f64;
        knots[i - 1] = (1.0 - p) * dx[ip] + p * dx[(ip + 1).min(n - 1)];
    }
    let (criterion, df) = if settings.method == 1 {
        (3, settings.df)
    } else {
        (1, 0.0)
    };
    let roots: Vec<f64> = w
        .iter()
        .map(|&weight| if weight > 0.0 { weight.sqrt() } else { weight })
        .collect();
    let mut smoother = Smoother::new(
        &dx,
        y,
        &roots,
        &knots,
        count,
        settings.gcvpen,
        df,
        0.0,
        criterion,
    );
    smoother.setup();
    let (spar, _) = smoother.minimize(&SearchControl {
        lower: 0.0,
        upper: 1.5,
        tolerance: 1e-2,
        eps: 0.000244,
        max_iterations: 500,
    });
    if smoother.info > 0 {
        log.push_str(&format!("spline(.) TROUBLE:\n[1] {}\n", smoother.info));
    }
    smo.copy_from_slice(&smoother.fitted);
    let mut edf = 0.0;
    for &leverage in &smoother.leverage {
        edf += leverage;
    }
    if settings.trace {
        let mut fixed = String::new();
        push_fixed(&mut fixed, edf, 2);
        log.push_str(&format!(
            "spline(df={:>5}, g.pen={:>11}, ismeth.={:+2}) -> (lambda, edf) = ({}, {:>5})\n",
            c_general(settings.df, 3, false),
            c_general(settings.gcvpen, 6, false),
            settings.method,
            c_general(spar, 7, false),
            fixed
        ));
    }
    Ok(edf)
}

/// `sort` in `ppr.f`: Singleton's quicksort (CACM Algorithm 347) of `v`, carrying the
/// permutation `a` along; ties end up in the order this variant leaves them, which the fit
/// depends on. The stops at the ends only matter for NaN, which the Fortran reads past.
fn sort(v: &mut [f64], a: &mut [usize]) {
    let jj = v.len().min(a.len()) as isize;
    let ii = 1isize;
    let mut iu = [0isize; 64];
    let mut il = [0isize; 64];
    let index = |i: isize| (i - 1) as usize;
    let mut m = 1usize;
    let mut i = ii;
    let mut j = jj;
    enum Label {
        L10,
        L20,
        L80,
        L90,
        L100,
    }
    let mut label = Label::L10;
    loop {
        match label {
            Label::L10 => {
                label = if i >= j { Label::L80 } else { Label::L20 };
            }
            Label::L20 => {
                let mut k = i;
                let ij = (j + i) / 2;
                let mut t = a[index(ij)];
                let mut vt = v[index(ij)];
                if !(v[index(i)] <= vt) {
                    a[index(ij)] = a[index(i)];
                    a[index(i)] = t;
                    t = a[index(ij)];
                    v[index(ij)] = v[index(i)];
                    v[index(i)] = vt;
                    vt = v[index(ij)];
                }
                let mut l = j;
                if !(v[index(j)] >= vt) {
                    a[index(ij)] = a[index(j)];
                    a[index(j)] = t;
                    t = a[index(ij)];
                    v[index(ij)] = v[index(j)];
                    v[index(j)] = vt;
                    vt = v[index(ij)];
                    if !(v[index(i)] <= vt) {
                        a[index(ij)] = a[index(i)];
                        a[index(i)] = t;
                        v[index(ij)] = v[index(i)];
                        v[index(i)] = vt;
                        vt = v[index(ij)];
                    }
                }
                loop {
                    loop {
                        l -= 1;
                        if l <= 1 || !(v[index(l)] > vt) {
                            break;
                        }
                    }
                    let tt = a[index(l)];
                    let vtt = v[index(l)];
                    loop {
                        k += 1;
                        if k >= jj || !(v[index(k)] < vt) {
                            break;
                        }
                    }
                    if k > l {
                        break;
                    }
                    a[index(l)] = a[index(k)];
                    a[index(k)] = tt;
                    v[index(l)] = v[index(k)];
                    v[index(k)] = vtt;
                }
                if l - i <= j - k {
                    il[m - 1] = k;
                    iu[m - 1] = j;
                    j = l;
                } else {
                    il[m - 1] = i;
                    iu[m - 1] = l;
                    i = k;
                }
                m += 1;
                label = Label::L90;
            }
            Label::L80 => {
                m -= 1;
                if m == 0 {
                    return;
                }
                i = il[m - 1];
                j = iu[m - 1];
                label = Label::L90;
            }
            Label::L90 => {
                if j - i > 10 {
                    label = Label::L20;
                } else if i == ii {
                    label = Label::L10;
                } else {
                    i -= 1;
                    label = Label::L100;
                }
            }
            Label::L100 => {
                i += 1;
                if i == j {
                    label = Label::L80;
                    continue;
                }
                let t = a[index(i + 1)];
                let vt = v[index(i + 1)];
                if v[index(i)] <= vt {
                    continue;
                }
                let mut k = i;
                loop {
                    a[index(k + 1)] = a[index(k)];
                    v[index(k + 1)] = v[index(k)];
                    k -= 1;
                    if k < 1 || !(vt < v[index(k)]) {
                        break;
                    }
                }
                a[index(k + 1)] = t;
                v[index(k + 1)] = vt;
            }
        }
    }
}

/// Fortran's `max` for the convergence test, which keeps the first operand unless the second
/// is larger.
fn fortran_max(first: f64, second: f64) -> f64 {
    if second > first { second } else { first }
}
