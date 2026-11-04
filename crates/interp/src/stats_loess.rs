// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's C and Fortran sources, and
// keep their copyright notices:
//
// GNU R: src/library/stats/src/loessc.c (loess_raw, loess_dfit, loess_dfitse, loess_ifit,
// loess_ise and their workspace helpers)
//     Copyright (C) 1998-2025 The R Core Team
//
// GNU R: src/library/stats/src/loessf.f (the ehg and lowes routines)
//     The authors of this software are Cleveland, Grosse, and Shyu.
//     Copyright (c) 1989, 1992 by AT&T.
//     Permission to use, copy, modify, and distribute this software for any purpose without fee
//     is hereby granted, provided that this entire notice is included in all copies of any
//     software which is or includes a copy or modification of this software and in all copies
//     of the supporting documentation for such software. THIS SOFTWARE IS BEING PROVIDED "AS
//     IS", WITHOUT ANY EXPRESS OR IMPLIED WARRANTY. IN PARTICULAR, NEITHER THE AUTHORS NOR AT&T
//     MAKE ANY REPRESENTATION OR WARRANTY OF ANY KIND CONCERNING THE MERCHANTABILITY OF THIS
//     SOFTWARE OR ITS FITNESS FOR ANY PARTICULAR PURPOSE.
//     Copyright (C) 1998-2020 The R Core Team
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! `loess`: local polynomial regression after Cleveland, Grosse and Shyu. Each fit at a point is
//! a weighted least-squares polynomial over the `nf = floor(n * span)` nearest observations,
//! weighted by the tricube of their distance. The default `"interpolate"` surface fits only at
//! the vertices of a k-d tree over the predictors and blends those fits with cubic Hermite
//! polynomials, which is what makes loess fast; `"direct"` fits at every point.
//!
//! The routines keep GNU R's `.C` and `.Fortran` interfaces, because `simpleLoess` and
//! `predLoess` in the stats package call them directly and keep the k-d tree they return in the
//! fitted object. They also keep the order of every floating-point operation of the Fortran,
//! down to the LINPACK QR and singular value decompositions the local fits use, so that fitted
//! values, traces and the k-d tree match GNU R in the last bit.
//!
//! Cells, vertices and points are numbered from 1 as in the Fortran, with slot 0 of each array
//! unused, because the tree arrays store those numbers and the neighbourhood selection depends
//! on positions within them.

use {
    crate::{
        builtin_libraries::NativeCall,
        linpack::{ddot, dqrdc_, dqrsl_, dsvdc_},
        stats::{c_arguments, c_return, integer_values, real_values},
        unwind::R,
        values,
    },
    heap::object::Sexp,
    std::ffi::c_int,
};

/// `.C(loess_raw, y, x, weights, robust, d, n, span, degree, nonparametric, drop.square,
/// sum.drop.sqr, span * cell, surf.stat, fitted.values, parameter, a, xi, vert, vval, diagonal,
/// trL, delta1, delta2, setLf)`.
pub unsafe fn loess_raw(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let y = real_values(arguments[0])?;
    let x = real_values(arguments[1])?;
    let drop_square = integer_values(arguments[9])?;
    let mode = values::first_string(arguments[12])
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let config = Config::new(
        call,
        values::as_integer_scalar(arguments[4]),
        values::as_integer_scalar(arguments[5]),
        values::as_real_scalar(arguments[6]),
        values::as_integer_scalar(arguments[7]),
        values::as_integer_scalar(arguments[8]),
        &drop_square,
        values::as_integer_scalar(arguments[10]),
        values::as_real_scalar(arguments[11]),
        values::as_integer_scalar(arguments[23]) != 0 || mode == "interpolate/exact",
    )?;
    let (n, d) = (config.n, config.d);
    if x.len() < n * d || y.len() < n {
        return Err(call.error("invalid 'x' in 'loess'"));
    }
    let used_weights = |index: usize| -> R<Vec<f64>> {
        let weights = real_values(arguments[index])?;
        if weights.len() < n {
            return Err(call.error("invalid 'weights' in 'loess'"));
        }
        Ok(weights)
    };
    let mut workspace = Workspace::new(config);
    let tau = workspace.config.tau;
    let span = workspace.config.span;
    let mut surface = None;
    let mut diagonal = None;
    let mut trace = 0.0;
    let mut deltas = None;
    let mut tree_fitted = false;

    match mode.as_str() {
        "interpolate/none" => {
            let robust = used_weights(3)?;
            workspace.lowesb(&x, &y, &robust, false);
            surface = Some(workspace.lowese(&x, n));
            tree_fitted = true;
        }
        "direct/none" => {
            let robust = used_weights(3)?;
            surface = Some(workspace.lowesf(&x, &y, &robust, &x, n, Influence::None).0);
        }
        "interpolate/1.approx" => {
            let weights = used_weights(2)?;
            workspace.lowesb(&x, &y, &weights, true);
            surface = Some(workspace.lowese(&x, n));
            let singular = workspace.solver.singular;
            trace = forward_sum(&workspace.diagonal);
            deltas = Some(workspace.lowesa(trace, tau, singular));
            diagonal = Some(workspace.diagonal.clone());
            tree_fitted = true;
        }
        "interpolate/2.approx" => {
            let weights = used_weights(2)?;
            workspace.lowesb(&x, &y, &weights, false);
            surface = Some(workspace.lowese(&x, n));
            let singular = workspace.solver.singular;
            trace = ehg196(tau, d, span);
            deltas = Some(workspace.lowesa(trace, tau, singular));
            tree_fitted = true;
        }
        "direct/approximate" => {
            let weights = used_weights(2)?;
            let (fit, influence) =
                workspace.lowesf(&x, &y, &weights, &x, n, Influence::Diagonal);
            surface = Some(fit);
            let singular = workspace.solver.singular;
            trace = forward_sum(&influence);
            deltas = Some(workspace.lowesa(trace, tau, singular));
            diagonal = Some(influence);
        }
        "interpolate/exact" => {
            let weights = used_weights(2)?;
            workspace.lowesb(&x, &y, &weights, true);
            let mut hat = workspace.lowesl(&x, n);
            let (trace_l, delta1, delta2) = lowesc(n, &mut hat);
            trace = trace_l;
            deltas = Some((delta1, delta2));
            surface = Some(workspace.lowese(&x, n));
            diagonal = Some(workspace.diagonal.clone());
            tree_fitted = true;
        }
        "direct/exact" => {
            let weights = used_weights(2)?;
            let (fit, mut hat) = workspace.lowesf(&x, &y, &weights, &x, n, Influence::Full);
            surface = Some(fit);
            let (trace_l, delta1, delta2) = lowesc(n, &mut hat);
            trace = trace_l;
            deltas = Some((delta1, delta2));
            diagonal = Some((0..n).map(|i| hat[i * (n + 1)]).collect());
        }
        _ => {}
    }

    if let Some(surface) = surface {
        arguments[13] = values::real_vector(&surface)?;
    }
    if tree_fitted {
        write_tree(&mut arguments, &workspace)?;
    }
    if let Some(diagonal) = diagonal {
        arguments[19] = values::real_vector(&diagonal)?;
    }
    arguments[20] = values::real_vector(&[trace])?;
    if let Some((delta1, delta2)) = deltas {
        arguments[21] = values::real_vector(&[delta1])?;
        arguments[22] = values::real_vector(&[delta2])?;
    }
    emit_warnings(call, &mut workspace)?;
    c_return(call, &arguments)
}

/// `.C(loess_dfit, y, x, x.evaluate, weights, span, degree, nonparametric, drop.square,
/// sum.drop.sqr, d, n, m, fit)`: direct fits at new points.
pub unsafe fn loess_dfit(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let m = values::as_integer_scalar(arguments[11]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let x = real_values(arguments[1])?;
    let evaluate = real_values(arguments[2])?;
    let drop_square = integer_values(arguments[7])?;
    let config = Config::new(
        call,
        values::as_integer_scalar(arguments[9]),
        values::as_integer_scalar(arguments[10]),
        values::as_real_scalar(arguments[4]),
        values::as_integer_scalar(arguments[5]),
        values::as_integer_scalar(arguments[6]),
        &drop_square,
        values::as_integer_scalar(arguments[8]),
        0.0,
        false,
    )?;
    let weights = real_values(arguments[3])?;
    check_inputs(call, &config, &x, &y, &weights, &evaluate, m)?;
    let mut workspace = Workspace::new(config);
    let (fit, _) = workspace.lowesf(&x, &y, &weights, &evaluate, m, Influence::None);
    arguments[12] = values::real_vector(&fit)?;
    emit_warnings(call, &mut workspace)?;
    c_return(call, &arguments)
}

/// `.C(loess_dfitse, y, x, x.evaluate, weights, robust, family.is.gaussian, span, degree,
/// nonparametric, drop.square, sum.drop.sqr, d, n, m, fit, L)`: direct fits at new points and
/// the operator rows behind their standard errors. For a robust family the fit uses the
/// robustness weights while the operator uses the prior weights alone.
pub unsafe fn loess_dfitse(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let m = values::as_integer_scalar(arguments[13]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let x = real_values(arguments[1])?;
    let evaluate = real_values(arguments[2])?;
    let drop_square = integer_values(arguments[9])?;
    let config = Config::new(
        call,
        values::as_integer_scalar(arguments[11]),
        values::as_integer_scalar(arguments[12]),
        values::as_real_scalar(arguments[6]),
        values::as_integer_scalar(arguments[7]),
        values::as_integer_scalar(arguments[8]),
        &drop_square,
        values::as_integer_scalar(arguments[10]),
        0.0,
        false,
    )?;
    let weights = real_values(arguments[3])?;
    check_inputs(call, &config, &x, &y, &weights, &evaluate, m)?;
    let family = values::as_integer_scalar(arguments[5]);
    let mut workspace = Workspace::new(config);
    if family == 1 {
        let (fit, operator) =
            workspace.lowesf(&x, &y, &weights, &evaluate, m, Influence::Full);
        arguments[14] = values::real_vector(&fit)?;
        arguments[15] = values::real_vector(&operator)?;
    } else if family == 0 {
        let robust = real_values(arguments[4])?;
        check_inputs(call, &workspace.config, &x, &y, &robust, &evaluate, m)?;
        let (_, operator) = workspace.lowesf(&x, &y, &weights, &evaluate, m, Influence::Full);
        let (fit, _) = workspace.lowesf(&x, &y, &robust, &evaluate, m, Influence::None);
        arguments[14] = values::real_vector(&fit)?;
        arguments[15] = values::real_vector(&operator)?;
    }
    emit_warnings(call, &mut workspace)?;
    c_return(call, &arguments)
}

/// `.C(loess_ifit, parameter, a, xi, vert, vval, m, x.evaluate, fit)`: interpolates a fitted
/// k-d tree at new points.
pub unsafe fn loess_ifit(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let parameter = integer_values(arguments[0])?;
    let splits = integer_values(arguments[1])?;
    let split_values = real_values(arguments[2])?;
    let bounds = real_values(arguments[3])?;
    let vval = real_values(arguments[4])?;
    let m = values::as_integer_scalar(arguments[5]).max(0) as usize;
    let evaluate = real_values(arguments[6])?;
    let invalid = || call.error("invalid k-d tree in 'loess' fit");
    if parameter.len() < 5 {
        return Err(invalid());
    }
    let dimension = parameter[0].max(0) as usize;
    let corners = parameter[2].max(0) as usize;
    let cells = parameter[3].max(0) as usize;
    let vertices = parameter[4].max(0) as usize;
    if !(1..=4).contains(&dimension)
        || corners != 1 << dimension
        || cells == 0
        || vertices < corners
        || splits.len() < cells
        || split_values.len() < cells
        || bounds.len() < 2 * dimension
        || vval.len() < (dimension + 1) * vertices
        || evaluate.len() < m * dimension
    {
        return Err(invalid());
    }
    let mut warnings = Vec::new();
    let tree = Tree::rebuild(
        dimension,
        cells,
        vertices,
        &bounds,
        &splits[..cells],
        &split_values[..cells],
        &mut warnings,
    )
    .ok_or_else(invalid)?;
    let mut padded = vec![0.0; (dimension + 1) * (vertices + 1)];
    padded[dimension + 1..dimension + 1 + (dimension + 1) * vertices]
        .copy_from_slice(&vval[..(dimension + 1) * vertices]);
    let fit = evaluate_tree(&tree, &padded, &evaluate, m, &mut warnings);
    arguments[7] = values::real_vector(&fit)?;
    emit(call, warnings)?;
    c_return(call, &arguments)
}

/// `.C(loess_ise, y, x, x.evaluate, weights, span, degree, nonparametric, drop.square,
/// sum.drop.sqr, span * cell, d, n, m, fit, L)`: the interpolated surface's operator rows at new
/// points, for standard errors. The `fit` argument is returned as passed.
pub unsafe fn loess_ise(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let m = values::as_integer_scalar(arguments[12]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let x = real_values(arguments[1])?;
    let evaluate = real_values(arguments[2])?;
    let drop_square = integer_values(arguments[7])?;
    let config = Config::new(
        call,
        values::as_integer_scalar(arguments[10]),
        values::as_integer_scalar(arguments[11]),
        values::as_real_scalar(arguments[4]),
        values::as_integer_scalar(arguments[5]),
        values::as_integer_scalar(arguments[6]),
        &drop_square,
        values::as_integer_scalar(arguments[8]),
        values::as_real_scalar(arguments[9]),
        true,
    )?;
    let weights = real_values(arguments[3])?;
    check_inputs(call, &config, &x, &y, &weights, &evaluate, m)?;
    let mut workspace = Workspace::new(config);
    workspace.lowesb(&x, &y, &weights, false);
    let operator = workspace.lowesl(&evaluate, m);
    arguments[14] = values::real_vector(&operator)?;
    emit_warnings(call, &mut workspace)?;
    c_return(call, &arguments)
}

/// `.Fortran(lowesw, residuals, n, robust, work)`: bisquare robustness weights with six
/// median absolute residuals as the cutoff.
pub unsafe fn lowesw(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let residuals = real_values(arguments[0])?;
    let n = (values::as_integer_scalar(arguments[1]).max(0) as usize).min(residuals.len());
    if n == 0 {
        return c_return(call, &arguments);
    }
    let mut robust: Vec<f64> = residuals[..n].iter().map(|r| r.abs()).collect();
    let mut order: Vec<usize> = (1..=n).collect();
    let half = n / 2 + 1;
    ehg106(1, n, half, &robust, &mut order);
    let cutoff = if (n - half) + 1 < half {
        ehg106(1, half - 1, half - 1, &robust, &mut order);
        3.0 * (robust[order[half - 1] - 1] + robust[order[half - 2] - 1])
    } else {
        6.0 * robust[order[half - 1] - 1]
    };
    if cutoff < f64::MIN_POSITIVE {
        robust.iter_mut().for_each(|r| *r = 1.0);
    } else {
        for r in robust.iter_mut() {
            *r = if cutoff * 0.999 < *r {
                0.0
            } else if cutoff * 0.001 < *r {
                let ratio = *r / cutoff;
                let inner = 1.0 - ratio * ratio;
                inner * inner
            } else {
                1.0
            };
        }
    }
    arguments[2] = values::real_vector(&robust)?;
    c_return(call, &arguments)
}

/// `.Fortran(lowesp, n, y, fitted, prior.weights, robust, work, pseudovalues)`: pseudovalues
/// whose ordinary residuals have the spread of the robust fit's, so that a Gaussian residual
/// sum of squares can measure a robust fit.
pub unsafe fn lowesp(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let y = real_values(arguments[1])?;
    let fitted = real_values(arguments[2])?;
    let prior = real_values(arguments[3])?;
    let robust = real_values(arguments[4])?;
    let n = (values::as_integer_scalar(arguments[0]).max(0) as usize)
        .min(y.len())
        .min(fitted.len())
        .min(prior.len())
        .min(robust.len());
    if n == 0 {
        return c_return(call, &arguments);
    }
    let mut pseudo: Vec<f64> = (0..n)
        .map(|i| (y[i] - fitted[i]).abs() * prior[i].sqrt())
        .collect();
    let mut order: Vec<usize> = (1..=n).collect();
    let half = n / 2 + 1;
    ehg106(1, n, half, &pseudo, &mut order);
    let mad = if (n - half) + 1 < half {
        ehg106(1, half - 1, half - 1, &pseudo, &mut order);
        (pseudo[order[half - 2] - 1] + pseudo[order[half - 1] - 1]) / 2.0
    } else {
        pseudo[order[half - 1] - 1]
    };
    let scaled = (6.0 * mad) * (6.0 * mad) / 5.0;
    for i in 0..n {
        let residual = y[i] - fitted[i];
        pseudo[i] = 1.0 - ((residual * residual) * prior[i]) / scaled;
    }
    for i in 0..n {
        pseudo[i] *= robust[i].sqrt();
    }
    let total = if n == 0 {
        0.0
    } else {
        let mut sum = pseudo[n - 1];
        for i in (0..n - 1).rev() {
            sum = pseudo[i] + sum;
        }
        sum
    };
    let factor = n as f64 / total;
    for i in 0..n {
        pseudo[i] = fitted[i] + (factor * robust[i]) * (y[i] - fitted[i]);
    }
    arguments[6] = values::real_vector(&pseudo)?;
    c_return(call, &arguments)
}

/// How much of the operator a direct fit also returns (`ihat` in the Fortran): nothing, its
/// diagonal, or every row.
#[derive(Clone, Copy, PartialEq)]
enum Influence {
    None,
    Diagonal,
    Full,
}

/// The settings of one fit (`loess_workspace` and `lowesd`).
struct Config {
    d: usize,
    n: usize,
    /// The number of nearest observations in each neighbourhood.
    nf: usize,
    span: f64,
    degree: i32,
    /// The number of columns of the full local design for this degree (`iv(29)`).
    columns: usize,
    /// How many leading predictors enter the distance; the rest are conditionally parametric.
    nonparametric: usize,
    /// Per predictor, 2 when its square is a term of a degree-2 fit and 1 when it is dropped.
    drop_square: [i32; 8],
    nvmax: usize,
    /// `span * cell`, as the k-d tree's leaf-size fraction.
    cell: f64,
    /// The number of parameters of the local model, which scales the approximate statistics.
    tau: i64,
    keep_operator: bool,
}

impl Config {
    #[allow(clippy::too_many_arguments)]
    unsafe fn new(
        call: &NativeCall<'_>,
        d: i32,
        n: i32,
        span: f64,
        degree: i32,
        nonparametric: i32,
        drop_square: &[i32],
        sum_drop_square: i32,
        cell: f64,
        keep_operator: bool,
    ) -> R<Config> {
        if !(1..=4).contains(&d) {
            return Err(call.error("only 1-4 predictors are allowed"));
        }
        if n < 1 {
            return Err(call.error("invalid 'x' in 'loess'"));
        }
        let (d, n) = (d as usize, n as usize);
        let nvmax = n.max(200);
        let neighbours = (n as f64).min((n as f64 * span + 1e-5).floor());
        if neighbours < 1.0 || neighbours.is_nan() {
            return Err(call.error("span is too small"));
        }
        let nf = neighbours as usize;
        let degree = degree.clamp(0, 2);
        let columns = match degree {
            0 => 1,
            1 => d + 1,
            _ => (d + 2) * (d + 1) / 2,
        };
        let tau0 = if degree > 1 {
            ((d + 2) * (d + 1)) / 2
        } else {
            d + 1
        } as i64;
        let mut codes = [degree; 8];
        for (code, &given) in codes.iter_mut().zip(drop_square).take(d) {
            *code = given;
        }
        Ok(Config {
            d,
            n,
            nf,
            span,
            degree,
            columns,
            nonparametric: (nonparametric.max(1) as usize).min(d),
            drop_square: codes,
            nvmax,
            cell,
            tau: tau0 - sum_drop_square as i64,
            keep_operator,
        })
    }
}

/// Fails the call when an array is shorter than the routine will read.
unsafe fn check_inputs(
    call: &NativeCall<'_>,
    config: &Config,
    x: &[f64],
    y: &[f64],
    weights: &[f64],
    evaluate: &[f64],
    m: usize,
) -> R<()> {
    if x.len() < config.n * config.d || y.len() < config.n || evaluate.len() < m * config.d {
        return Err(call.error("invalid 'x' in 'loess'"));
    }
    if weights.len() < config.n {
        return Err(call.error("invalid 'weights' in 'loess'"));
    }
    Ok(())
}

unsafe fn emit_warnings(call: &NativeCall<'_>, workspace: &mut Workspace) -> R<()> {
    emit(call, std::mem::take(&mut workspace.solver.warnings))
}

unsafe fn emit(call: &NativeCall<'_>, warnings: Vec<String>) -> R<()> {
    for message in warnings {
        call.warning(message)?;
    }
    Ok(())
}

/// `loess_prune`: copies the fitted tree out of the workspace into the arguments R keeps.
unsafe fn write_tree(arguments: &mut [Sexp], workspace: &Workspace) -> R<()> {
    let tree = &workspace.tree;
    let (d, vc) = (tree.d, tree.vc);
    let nvmax = tree.nvmax as i32;
    let parameter = [
        d as i32,
        workspace.config.n as i32,
        vc as i32,
        tree.nc as i32,
        tree.nv as i32,
        49 + (vc as i32 + 3) * nvmax,
        49 + 2 * (d as i32 + 1) * nvmax,
    ];
    arguments[14] = values::integer_vector(&parameter)?;

    let mut splits = vec![0; values::length(arguments[15])];
    let mut split_values = vec![0.0; values::length(arguments[16])];
    for cell in 1..=tree.nc {
        if let Some(slot) = splits.get_mut(cell - 1) {
            *slot = tree.split_dimension[cell];
        }
        if let Some(slot) = split_values.get_mut(cell - 1) {
            *slot = tree.split_value[cell];
        }
    }
    arguments[15] = values::integer_vector(&splits)?;
    arguments[16] = values::real_vector(&split_values)?;

    let mut bounds = vec![0.0; values::length(arguments[17])];
    for k in 1..=d {
        if let Some(slot) = bounds.get_mut(k - 1) {
            *slot = tree.vertex(1, k);
        }
        if let Some(slot) = bounds.get_mut(k - 1 + d) {
            *slot = tree.vertex(vc, k);
        }
    }
    arguments[17] = values::real_vector(&bounds)?;

    let mut stored = vec![0.0; values::length(arguments[18])];
    let count = (d + 1) * tree.nv;
    for (slot, &value) in stored.iter_mut().zip(&workspace.vval[d + 1..]).take(count) {
        *slot = value;
    }
    arguments[18] = values::real_vector(&stored)?;
    Ok(())
}

fn forward_sum(values: &[f64]) -> f64 {
    let mut sum = 0.0;
    for &value in values {
        sum += value;
    }
    sum
}

/// The k-d tree over the predictors: its cells, their corner vertices and the vertex
/// coordinates (`v`, `a`, `xi`, `lo`, `hi` and `c` of the Fortran).
struct Tree {
    d: usize,
    vc: usize,
    nvmax: usize,
    ncmax: usize,
    nv: usize,
    nc: usize,
    /// Vertex coordinates, `v(i, k)` at `i + (nvmax + 1) * (k - 1)`.
    coordinates: Vec<f64>,
    /// The dimension each cell splits on, 0 for a leaf (`a`).
    split_dimension: Vec<i32>,
    /// The cut of each split cell (`xi`).
    split_value: Vec<f64>,
    /// The lower child of a split cell, or the first point position of a leaf (`lo`).
    lower: Vec<usize>,
    /// The upper child of a split cell, or the last point position of a leaf (`hi`).
    upper: Vec<usize>,
    /// The `vc` corner vertices of each cell (`c`).
    corners: Vec<usize>,
}

impl Tree {
    fn new(d: usize, nvmax: usize, ncmax: usize) -> Tree {
        let vc = 1 << d;
        Tree {
            d,
            vc,
            nvmax,
            ncmax,
            nv: 0,
            nc: 0,
            coordinates: vec![0.0; (nvmax + 1) * d],
            split_dimension: vec![0; ncmax + 2],
            split_value: vec![0.0; ncmax + 2],
            lower: vec![0; ncmax + 2],
            upper: vec![0; ncmax + 2],
            corners: vec![0; (ncmax + 2) * vc],
        }
    }

    fn vertex(&self, vertex: usize, k: usize) -> f64 {
        self.coordinates[vertex + (self.nvmax + 1) * (k - 1)]
    }

    fn set_vertex(&mut self, vertex: usize, k: usize, value: f64) {
        self.coordinates[vertex + (self.nvmax + 1) * (k - 1)] = value;
    }

    fn corner(&self, cell: usize, which: usize) -> usize {
        self.corners[cell * self.vc + which - 1]
    }

    fn set_corner(&mut self, cell: usize, which: usize, vertex: usize) {
        self.corners[cell * self.vc + which - 1] = vertex;
    }

    /// `ehg126`: the bounding box of the data, widened a little, and the vertices it implies.
    fn bounding_box(&mut self, x: &[f64], n: usize) {
        let (d, vc) = (self.d, self.vc);
        for k in 1..=d {
            let mut alpha = f64::MAX;
            let mut beta = -f64::MAX;
            for i in 0..n {
                let t = x[i + n * (k - 1)];
                alpha = alpha.min(t);
                beta = beta.max(t);
            }
            let margin =
                0.005 * (beta - alpha).max(1e-10 * alpha.abs().max(beta.abs()) + 1e-30);
            alpha -= margin;
            beta += margin;
            self.set_vertex(1, k, alpha);
            self.set_vertex(vc, k, beta);
        }
        self.fill_box_vertices();
    }

    /// The vertices of the box other than the first and last, each taking its coordinates from
    /// those two according to the bits of its number.
    fn fill_box_vertices(&mut self) {
        let (d, vc) = (self.d, self.vc);
        for i in 2..vc {
            let mut j = i - 1;
            for k in 1..=d {
                let source = 1 + (j % 2) * (vc - 1);
                let value = self.vertex(source, k);
                self.set_vertex(i, k, value);
                j /= 2;
            }
        }
    }

    /// `ehg125`: splits `cell` at `value` in dimension `k`, giving its children the vertices of
    /// the two halves. A vertex that already exists is reused. Returns false when the vertex
    /// table is full.
    fn split_cell(
        &mut self,
        cell: usize,
        dimension: usize,
        value: f64,
        lower: usize,
        upper: usize,
    ) -> bool {
        let d = self.d;
        let r = 1usize << (dimension - 1);
        let s = 1usize << (d - dimension);
        let existing = self.nv;
        let mut h = existing;
        for i in 1..=r {
            for j in 1..=s {
                h += 1;
                if h > self.nvmax {
                    return false;
                }
                let at = |side: usize| (i - 1) + r * side + 2 * r * (j - 1) + 1;
                let first = self.corner(cell, at(0));
                let second = self.corner(cell, at(1));
                for k in 1..=d {
                    let coordinate = self.vertex(first, k);
                    self.set_vertex(h, k, coordinate);
                }
                self.set_vertex(h, dimension, value);
                let mut found = None;
                for m in 1..=existing {
                    if (1..=d).all(|k| self.vertex(m, k) == self.vertex(h, k)) {
                        found = Some(m);
                        break;
                    }
                }
                let m = match found {
                    Some(m) => {
                        h -= 1;
                        m
                    }
                    None => h,
                };
                self.set_corner(lower, at(0), first);
                self.set_corner(lower, at(1), m);
                self.set_corner(upper, at(0), m);
                self.set_corner(upper, at(1), second);
            }
        }
        self.nv = h;
        true
    }

    /// `ehg124`: builds the tree over the points in `order`, splitting each cell with more than
    /// `leaf_size` points at the median of its widest dimension.
    fn build(
        &mut self,
        x: &[f64],
        n: usize,
        order: &mut [usize],
        leaf_size: i64,
        minimum_diameter: f64,
        nonparametric: usize,
    ) -> bool {
        let vc = self.vc;
        let mut cell = 1usize;
        let mut low = 1usize;
        let mut high = n;
        self.lower[cell] = low;
        self.upper[cell] = high;
        while cell <= self.nc {
            let mut diameter = 0.0;
            for k in 1..=nonparametric {
                let extent = self.vertex(self.corner(cell, vc), k) - self.vertex(self.corner(cell, 1), k);
                diameter += extent * extent;
            }
            let diameter = f64::sqrt(diameter);
            let mut leaf = if (high as i64 - low as i64) + 1 <= leaf_size {
                true
            } else {
                diameter <= minimum_diameter
            };
            if !leaf {
                leaf = if self.ncmax < self.nc + 2 {
                    true
                } else {
                    (self.nvmax as f64) < self.nv as f64 + vc as f64 / 2.0
                };
            }
            let mut dimension = 0;
            let mut median = 0usize;
            if !leaf {
                let widths = ehg129(low, high, nonparametric, x, n, order);
                dimension = idamax(&widths);
                let key = &x[n * (dimension - 1)..n * dimension];
                let mut position = (low + high) / 2;
                ehg106(low, high, position, key, order);
                // All ties go with the upper child, so the split moves outward from the median
                // to the nearest gap between distinct values.
                let mut offset: isize = 0;
                loop {
                    let at = position as isize + offset;
                    if at >= high as isize || at < low as isize {
                        break;
                    }
                    let (lower_end, check, upper_end) = if offset < 0 {
                        (low, at as usize, at as usize)
                    } else {
                        (at as usize + 1, at as usize + 1, high)
                    };
                    ehg106(lower_end, upper_end, check, key, order);
                    if key[order[at as usize - 1] - 1] == key[order[at as usize] - 1] {
                        offset = -offset;
                        if offset >= 0 {
                            offset += 1;
                        }
                    } else {
                        position = at as usize;
                        break;
                    }
                }
                median = position;
                let cut = key[order[median - 1] - 1];
                leaf = if self.vertex(self.corner(cell, 1), dimension) == cut {
                    true
                } else {
                    self.vertex(self.corner(cell, vc), dimension) == cut
                };
            }
            if leaf {
                self.split_dimension[cell] = 0;
            } else {
                self.split_dimension[cell] = dimension as i32;
                let cut = x[(order[median - 1] - 1) + n * (dimension - 1)];
                self.split_value[cell] = cut;
                self.nc += 1;
                self.lower[cell] = self.nc;
                let lower_child = self.nc;
                self.lower[lower_child] = low;
                self.upper[lower_child] = median;
                self.nc += 1;
                self.upper[cell] = self.nc;
                let upper_child = self.nc;
                self.lower[upper_child] = median + 1;
                self.upper[upper_child] = high;
                if !self.split_cell(cell, dimension, cut, lower_child, upper_child) {
                    return false;
                }
            }
            cell += 1;
            low = self.lower[cell];
            high = self.upper[cell];
        }
        true
    }

    /// `ehg169`: recreates the vertices, cell children and cell corners of a fitted tree from
    /// its bounding box, split dimensions and cuts. Returns `None` for a tree that does not
    /// describe a consistent subdivision.
    fn rebuild(
        d: usize,
        nc: usize,
        nv: usize,
        bounds: &[f64],
        dimensions: &[i32],
        cuts: &[f64],
        warnings: &mut Vec<String>,
    ) -> Option<Tree> {
        let mut tree = Tree::new(d, nv, nc);
        let vc = tree.vc;
        for k in 1..=d {
            tree.set_vertex(1, k, bounds[k - 1]);
            tree.set_vertex(vc, k, bounds[k - 1 + d]);
        }
        tree.fill_box_vertices();
        let mut cells = 1;
        tree.nv = vc;
        for j in 1..=vc {
            tree.set_corner(1, j, j);
        }
        for cell in 1..=nc {
            tree.split_dimension[cell] = dimensions[cell - 1];
            tree.split_value[cell] = cuts[cell - 1];
        }
        let mut cell = 1;
        while cell <= nc {
            let dimension = tree.split_dimension[cell];
            if dimension < 0 || dimension as usize > d {
                return None;
            }
            if dimension != 0 {
                if cells + 2 > nc {
                    return None;
                }
                cells += 1;
                tree.lower[cell] = cells;
                cells += 1;
                tree.upper[cell] = cells;
                let (lower, upper) = (tree.lower[cell], tree.upper[cell]);
                if !tree.split_cell(cell, dimension as usize, cuts[cell - 1], lower, upper) {
                    return None;
                }
            }
            cell += 1;
        }
        if cells != nc || tree.nv != nv {
            warnings.push(loess_warning(193));
        }
        tree.nc = nc;
        Some(tree)
    }

    /// `ehg137`: the leaf cells whose boundary or interior holds `z`.
    fn leaves_at(&self, z: &[f64], warnings: &mut Vec<String>) -> Vec<usize> {
        let mut leaves = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut cell = 1;
        while cell > 0 {
            if self.split_dimension[cell] == 0 {
                leaves.push(cell);
                cell = stack.pop().unwrap_or(0);
            } else {
                let dimension = self.split_dimension[cell] as usize;
                let cut = self.split_value[cell];
                if z[dimension - 1] == cut {
                    stack.push(self.upper[cell]);
                    if stack.len() > 20 {
                        warnings.push(loess_warning(187));
                    }
                    cell = self.lower[cell];
                } else if z[dimension - 1] <= cut {
                    cell = self.lower[cell];
                } else {
                    cell = self.upper[cell];
                }
            }
        }
        if leaves.len() > 256 {
            warnings.push(loess_warning(185));
        }
        leaves
    }
}

/// `ehg129`: the extent of the points `order[low..=high]` in each of the first `d` columns.
fn ehg129(low: usize, high: usize, d: usize, x: &[f64], n: usize, order: &[usize]) -> Vec<f64> {
    let mut sigma = vec![0.0; d];
    for (k, width) in sigma.iter_mut().enumerate() {
        let mut alpha = f64::MAX;
        let mut beta = -f64::MAX;
        for position in low..=high {
            let t = x[(order[position - 1] - 1) + n * k];
            alpha = alpha.min(t);
            beta = beta.max(t);
        }
        *width = beta - alpha;
    }
    sigma
}

/// BLAS `idamax` over `values` by magnitude, as a 1-based position.
fn idamax(values: &[f64]) -> usize {
    let mut best = 1;
    let mut largest = values[0].abs();
    for (index, value) in values.iter().enumerate().skip(1) {
        if value.abs() > largest {
            best = index + 1;
            largest = value.abs();
        }
    }
    best
}

/// `ehg106`: rearranges `order[il..=ir]` (1-based positions holding 1-based point numbers) so
/// that the point at position `k` is the one of rank `k` by `keys` (indexed by point number),
/// after Floyd and Rivest, algorithm 489.
fn ehg106(il: usize, ir: usize, k: usize, keys: &[f64], order: &mut [usize]) {
    let key = |order: &[usize], position: isize| keys[order[position as usize - 1] - 1];
    let k = k as isize;
    let mut l = il as isize;
    let mut r = ir as isize;
    while l < r {
        let t = key(order, k);
        let mut i = l;
        let mut j = r;
        order.swap(l as usize - 1, k as usize - 1);
        if t < key(order, r) {
            order.swap(l as usize - 1, r as usize - 1);
        }
        while i < j {
            order.swap(i as usize - 1, j as usize - 1);
            i += 1;
            j -= 1;
            while key(order, i) < t {
                i += 1;
            }
            while t < key(order, j) {
                j -= 1;
            }
        }
        if key(order, l) == t {
            order.swap(l as usize - 1, j as usize - 1);
        } else {
            j += 1;
            order.swap(r as usize - 1, j as usize - 1);
        }
        if j <= k {
            l = j + 1;
        }
        if k <= j {
            r = j - 1;
        }
    }
}

/// `ehg128`: evaluates the interpolant, cubic Hermite blending of the vertex values and slopes
/// in `vval` (`vval(comp, vertex)` at `comp + (d + 1) * vertex`), at `z`.
fn ehg128(tree: &Tree, z: &[f64], vval: &[f64], warnings: &mut Vec<String>) -> f64 {
    let (d, vc) = (tree.d, tree.vc);
    let width = d + 1;
    let value_at = |vertex: usize, component: usize| vval[component + width * vertex];

    let mut path = vec![1usize];
    let mut cell = 1;
    while tree.split_dimension[cell] != 0 {
        let dimension = tree.split_dimension[cell] as usize;
        cell = if z[dimension - 1] <= tree.split_value[cell] {
            tree.lower[cell]
        } else {
            tree.upper[cell]
        };
        path.push(cell);
        if path.len() >= 20 {
            warnings.push(loess_warning(181));
        }
    }
    let j = cell;

    let mut g = [0.0f64; 9 * 256];
    for i12 in 1..=vc {
        for i11 in 0..=d {
            g[i11 + 9 * (i12 - 1)] = value_at(tree.corner(j, i12), i11);
        }
    }
    let mut lg = vc;
    let ll = tree.corner(j, 1);
    let ur = tree.corner(j, vc);
    for i in (1..=d).rev() {
        let h = (z[i - 1] - tree.vertex(ll, i)) / (tree.vertex(ur, i) - tree.vertex(ll, i));
        if h < -0.001 {
            warnings.push(numbers_message("eval ", &z[..d]));
            warnings.push(numbers_message(
                "lowerlimit ",
                &(1..=d).map(|k| tree.vertex(ll, k)).collect::<Vec<_>>(),
            ));
        } else if 1.001 < h {
            warnings.push(numbers_message("eval ", &z[..d]));
            warnings.push(numbers_message(
                "upperlimit ",
                &(1..=d).map(|k| tree.vertex(ur, k)).collect::<Vec<_>>(),
            ));
        }
        if !(-0.001 <= h && h <= 1.001) {
            warnings.push(loess_warning(122));
        }
        lg /= 2;
        for ig in 1..=lg {
            let (phi0, phi1, psi0, psi1) = hermite(h);
            let low = 9 * (ig - 1);
            let high = 9 * (ig + lg - 1);
            g[low] = phi0 * g[low]
                + phi1 * g[high]
                + (psi0 * g[low + i] + psi1 * g[high + i])
                    * (tree.vertex(ur, i) - tree.vertex(ll, i));
            for ii in 1..i {
                g[low + ii] = phi0 * g[low + ii] + phi1 * g[high + ii];
            }
        }
    }
    let mut s = g[0];

    if d == 2 {
        let (gn, gpn) = blend_edge(
            tree, vval, z, &path, j, 2, tree.vertex(ur, 2), [3, 4], true, [1, 2], 1,
        );
        let (gs, gps) = blend_edge(
            tree, vval, z, &path, j, 2, tree.vertex(ll, 2), [1, 2], false, [3, 4], 1,
        );
        let (ge, gpe) = blend_edge(
            tree, vval, z, &path, j, 1, tree.vertex(ur, 1), [2, 4], true, [1, 3], 2,
        );
        let (gw, gpw) = blend_edge(
            tree, vval, z, &path, j, 1, tree.vertex(ll, 1), [1, 3], false, [2, 4], 2,
        );
        let h = (z[1] - tree.vertex(ll, 2)) / (tree.vertex(ur, 2) - tree.vertex(ll, 2));
        let (phi0, phi1, psi0, psi1) = hermite(h);
        let sns = phi0 * gs
            + phi1 * gn
            + (psi0 * gps + psi1 * gpn) * (tree.vertex(ur, 2) - tree.vertex(ll, 2));
        let h = (z[0] - tree.vertex(ll, 1)) / (tree.vertex(ur, 1) - tree.vertex(ll, 1));
        let (phi0, phi1, psi0, psi1) = hermite(h);
        let sew = phi0 * gw
            + phi1 * ge
            + (psi0 * gpw + psi1 * gpe) * (tree.vertex(ur, 1) - tree.vertex(ll, 1));
        s = (sns + sew) - s;
    }
    s
}

/// The cubic Hermite basis at `h`: the value functions at the lower and upper end, then the
/// slope functions.
fn hermite(h: f64) -> (f64, f64, f64, f64) {
    let phi0 = ((1.0 - h) * (1.0 - h)) * (1.0 + 2.0 * h);
    let phi1 = (h * h) * (3.0 - 2.0 * h);
    let psi0 = h * ((1.0 - h) * (1.0 - h));
    let psi1 = (h * h) * (h - 1.0);
    (phi0, phi1, psi0, psi1)
}

/// One side of a two-dimensional cell in `ehg128`: the Hermite interpolant along that side,
/// with the end values and slopes taken from a neighbouring cell when it is finer than this
/// one, and the interpolated slope across the side. `along` is the coordinate the side runs
/// along; `own` and `neighbour` are the corner numbers of this cell's side and of the matching
/// side of the finer neighbour.
#[allow(clippy::too_many_arguments)]
fn blend_edge(
    tree: &Tree,
    vval: &[f64],
    z: &[f64],
    path: &[usize],
    j: usize,
    split_dimension: i32,
    boundary: f64,
    own: [usize; 2],
    upper_side: bool,
    neighbour: [usize; 2],
    along: usize,
) -> (f64, f64) {
    let width = tree.d + 1;
    let components = |vertex: usize| [vval[width * vertex], vval[1 + width * vertex], vval[2 + width * vertex]];
    let ll = tree.corner(j, 1);
    let ur = tree.corner(j, tree.vc);
    let mut v0 = tree.vertex(ll, along);
    let mut v1 = tree.vertex(ur, along);
    let mut g0 = components(tree.corner(j, own[0]));
    let mut g1 = components(tree.corner(j, own[1]));
    let mut m = path.len() - 1;
    while m != 0
        && !(tree.split_dimension[path[m - 1]] == split_dimension
            && tree.split_value[path[m - 1]] == boundary)
    {
        m -= 1;
    }
    if m >= 1 {
        let mut cell = if upper_side {
            tree.upper[path[m - 1]]
        } else {
            tree.lower[path[m - 1]]
        };
        while tree.split_dimension[cell] != 0 {
            let dimension = tree.split_dimension[cell] as usize;
            cell = if z[dimension - 1] <= tree.split_value[cell] {
                tree.lower[cell]
            } else {
                tree.upper[cell]
            };
        }
        if v0 < tree.vertex(tree.corner(cell, neighbour[0]), along) {
            v0 = tree.vertex(tree.corner(cell, neighbour[0]), along);
            g0 = components(tree.corner(cell, neighbour[0]));
        }
        if tree.vertex(tree.corner(cell, neighbour[1]), along) < v1 {
            v1 = tree.vertex(tree.corner(cell, neighbour[1]), along);
            g1 = components(tree.corner(cell, neighbour[1]));
        }
    }
    let h = (z[along - 1] - v0) / (v1 - v0);
    let (phi0, phi1, psi0, psi1) = hermite(h);
    let across = 3 - along;
    let value = phi0 * g0[0] + phi1 * g1[0] + (psi0 * g0[along] + psi1 * g1[along]) * (v1 - v0);
    let slope = phi0 * g0[across] + phi1 * g1[across];
    (value, slope)
}

/// `ehg133`: the interpolant at each of `m` points, stored column-major `m` by `d`.
fn evaluate_tree(
    tree: &Tree,
    vval: &[f64],
    points: &[f64],
    m: usize,
    warnings: &mut Vec<String>,
) -> Vec<f64> {
    let d = tree.d;
    let mut point = vec![0.0; d];
    (0..m)
        .map(|i| {
            for (k, coordinate) in point.iter_mut().enumerate() {
                *coordinate = points[i + m * k];
            }
            ehg128(tree, &point, vval, warnings)
        })
        .collect()
}

/// The workspace of one fit: the settings, the k-d tree, the vertex values and the scratch of
/// the local fits (`iv` and `v` of the Fortran).
struct Workspace {
    config: Config,
    solver: Solver,
    tree: Tree,
    /// Value and slopes of the fit at each vertex, `vval(comp, vertex)` at
    /// `comp + (d + 1) * vertex`.
    vval: Vec<f64>,
    /// Scratch of the same shape, holding one vertex's contribution at a time.
    vval2: Vec<f64>,
    /// The point numbers of each vertex's neighbourhood, `lq(vertex, p)` at
    /// `(vertex - 1) + nvmax * (p - 1)`; kept only to build the operator.
    neighbourhoods: Vec<usize>,
    /// The operator rows of each vertex, `lf(comp, vertex, p)` at
    /// `comp + (d + 1) * ((vertex - 1) + nvmax * (p - 1))`.
    vertex_operator: Vec<f64>,
    /// The points in k-d tree order (`pi`).
    order: Vec<usize>,
    /// The diagonal of the operator at the data, when the fit asked for it.
    diagonal: Vec<f64>,
}

impl Workspace {
    fn new(config: Config) -> Workspace {
        let (d, n, nf, nvmax) = (config.d, config.n, config.nf, config.nvmax);
        let keep = config.keep_operator;
        Workspace {
            solver: Solver::new(&config),
            tree: Tree::new(d, nvmax, nvmax),
            vval: vec![0.0; (d + 1) * (nvmax + 1)],
            vval2: vec![0.0; (d + 1) * (nvmax + 1)],
            neighbourhoods: if keep { vec![0; nvmax * nf] } else { Vec::new() },
            vertex_operator: if keep {
                vec![0.0; (d + 1) * nvmax * nf]
            } else {
                Vec::new()
            },
            order: (1..=n).collect(),
            diagonal: Vec::new(),
            config,
        }
    }

    /// `lowesb` and `ehg131`: builds the k-d tree and fits the local polynomial at each vertex.
    /// With `influence`, also accumulates the diagonal of the operator at the data.
    fn lowesb(&mut self, x: &[f64], y: &[f64], weights: &[f64], influence: bool) {
        let n = self.config.n;
        let tree = &mut self.tree;
        tree.bounding_box(x, n);
        tree.nv = tree.vc;
        tree.nc = 1;
        for j in 1..=tree.vc {
            tree.set_corner(1, j, j);
        }
        let leaf_size = (n as f64 * self.config.cell).floor() as i64;
        self.order = (1..=n).collect();
        let built = tree.build(x, n, &mut self.order, leaf_size, 0.0, self.config.nonparametric);
        if !built {
            self.solver.warnings.push(loess_warning(180));
        }
        self.ehg139(x, y, weights, influence);
        let (nvmax, vc) = (self.tree.nvmax, self.tree.vc);
        if (nvmax as f64) < self.tree.nv as f64 + vc as f64 / 2.0 {
            self.solver.warnings.push(integer_message(
                "k-d tree limited by memory; nvmax=",
                nvmax as i64,
            ));
        } else if self.tree.ncmax < self.tree.nc + 2 {
            self.solver.warnings.push(integer_message(
                "k-d tree limited by memory. ncmax=",
                self.tree.ncmax as i64,
            ));
        }
    }

    /// `ehg139`: the local fit at each vertex of the tree, and when asked for the diagonal of
    /// the operator and the operator rows of every vertex.
    fn ehg139(&mut self, x: &[f64], y: &[f64], weights: &[f64], influence: bool) {
        let (d, n, nf) = (self.config.d, self.config.n, self.config.nf);
        let width = d + 1;
        if self.solver.columns > nf.saturating_sub(1) {
            self.solver.warnings.push(loess_warning(104));
        }
        if self.solver.columns > 15 {
            self.solver.warnings.push(loess_warning(105));
        }
        if influence {
            self.diagonal = vec![0.0; n];
            self.vval2.iter_mut().for_each(|value| *value = 0.0);
        }
        self.solver.psi = (1..=n).collect();
        let mut point = vec![0.0; d];
        for l in 1..=self.tree.nv {
            for (k, coordinate) in point.iter_mut().enumerate() {
                *coordinate = self.tree.vertex(l, k + 1);
            }
            self.solver.ehg127(
                &point,
                x,
                y,
                weights,
                &mut self.vval[width * l..width * l + width],
            );
            if influence {
                let mut inverse = vec![0usize; n];
                for i in 1..=nf {
                    inverse[self.solver.psi[i - 1] - 1] = i;
                }
                let leaves = self.tree.leaves_at(&point, &mut self.solver.warnings);
                for leaf in leaves {
                    for ii in self.tree.lower[leaf]..=self.tree.upper[leaf] {
                        let i = inverse[self.order[ii - 1] - 1];
                        if i == 0 {
                            continue;
                        }
                        if self.solver.psi[i - 1] != self.order[ii - 1] {
                            self.solver.warnings.push(loess_warning(194));
                        }
                        self.solver.unit_response(i);
                        self.solver.apply_q(true);
                        self.solver.solve_gamma();
                        let k = self.solver.columns;
                        for j in 1..=width {
                            self.vval2[(j - 1) + width * l] = if j <= k {
                                self.solver.right_row_dot_gamma(j, k)
                            } else {
                                0.0
                            };
                        }
                        let target = self.order[ii - 1];
                        for (i5, coordinate) in point.iter_mut().enumerate() {
                            *coordinate = x[(target - 1) + n * i5];
                        }
                        let term = ehg128(
                            &self.tree,
                            &point,
                            &self.vval2,
                            &mut self.solver.warnings,
                        );
                        self.diagonal[target - 1] += term;
                        for i5 in 0..=d {
                            self.vval2[i5 + width * l] = 0.0;
                        }
                        for (k, coordinate) in point.iter_mut().enumerate() {
                            *coordinate = self.tree.vertex(l, k + 1);
                        }
                    }
                }
            }
            if self.config.keep_operator {
                self.store_vertex_operator(l);
            }
        }
    }

    /// The operator rows of vertex `l`: its neighbourhood and, for each neighbour, the value
    /// and slope coefficients that map the neighbour's response to the vertex fit.
    fn store_vertex_operator(&mut self, l: usize) {
        let (d, nf, nvmax) = (self.config.d, self.config.nf, self.config.nvmax);
        let width = d + 1;
        let k = self.solver.columns;
        if k < d + 1 {
            self.solver.warnings.push(loess_warning(196));
        }
        for p in 1..=nf {
            self.neighbourhoods[(l - 1) + nvmax * (p - 1)] = self.solver.psi[p - 1];
        }
        for p in 1..=nf {
            for i5 in 0..width {
                self.vertex_operator[i5 + width * ((l - 1) + nvmax * (p - 1))] = 0.0;
            }
        }
        for j in 1..=k {
            self.solver.left_vector_response(j);
            for i in 1..=nf {
                let scaled = self.solver.eta[i - 1];
                for i5 in 0..width {
                    let slot = i5 + width * ((l - 1) + nvmax * (i - 1));
                    if i5 < k {
                        self.vertex_operator[slot] += self.solver.right(1 + i5, j) * scaled;
                    } else {
                        self.vertex_operator[slot] = 0.0;
                    }
                }
            }
        }
    }

    /// `lowese`: the interpolated surface at `m` points.
    fn lowese(&mut self, points: &[f64], m: usize) -> Vec<f64> {
        evaluate_tree(&self.tree, &self.vval, points, m, &mut self.solver.warnings)
    }

    /// `lowesf` and `ehg136`: direct fits at `m` points, with the operator's diagonal or rows.
    fn lowesf(
        &mut self,
        x: &[f64],
        y: &[f64],
        weights: &[f64],
        points: &[f64],
        m: usize,
        influence: Influence,
    ) -> (Vec<f64>, Vec<f64>) {
        let (d, n, nf) = (self.config.d, self.config.n, self.config.nf);
        if self.solver.columns > nf.saturating_sub(1) {
            self.solver.warnings.push(loess_warning(104));
        }
        if self.solver.columns > 15 {
            self.solver.warnings.push(loess_warning(105));
        }
        let mut fit = vec![0.0; m];
        let mut operator = match influence {
            Influence::None => Vec::new(),
            Influence::Diagonal => vec![0.0; m],
            Influence::Full => vec![0.0; m * n],
        };
        self.solver.psi = (1..=n).collect();
        let mut point = vec![0.0; d];
        for l in 1..=m {
            for (k, coordinate) in point.iter_mut().enumerate() {
                *coordinate = points[(l - 1) + m * k];
            }
            self.solver
                .ehg127(&point, x, y, weights, &mut fit[l - 1..l]);
            let k = self.solver.columns;
            match influence {
                Influence::None => {}
                Influence::Diagonal => {
                    if m != n {
                        self.solver.warnings.push(loess_warning(123));
                    }
                    let mut i = 1;
                    while l != self.solver.psi[i - 1] {
                        i += 1;
                        if i >= nf {
                            self.solver.warnings.push(loess_warning(123));
                            break;
                        }
                    }
                    self.solver.unit_response(i);
                    self.solver.apply_q(true);
                    self.solver.gamma_from_left_vectors();
                    operator[l - 1] = self.solver.right_row_dot_gamma(1, k);
                }
                Influence::Full => {
                    for j in 1..=k {
                        self.solver.left_vector_response(j);
                        for i in 1..=nf {
                            let target = self.solver.psi[i - 1] - 1;
                            operator[(l - 1) + m * target] += self.solver.right(1, j)
                                * self.solver.eta[i - 1];
                        }
                    }
                }
            }
        }
        (fit, operator)
    }

    /// `lowesl` and `ehg191`: the operator of the interpolated surface at `m` points, as a
    /// column-major `m` by `n` matrix.
    fn lowesl(&mut self, points: &[f64], m: usize) -> Vec<f64> {
        let (d, n, nf, nvmax) = (
            self.config.d,
            self.config.n,
            self.config.nf,
            self.config.nvmax,
        );
        let width = d + 1;
        let mut result = vec![0.0; m * n];
        let mut point = vec![0.0; d];
        for j in 1..=n {
            self.vval2.iter_mut().for_each(|value| *value = 0.0);
            for i in 1..=self.tree.nv {
                let first = self.neighbourhoods[i - 1];
                self.neighbourhoods[i - 1] = j;
                let mut p = nf;
                while self.neighbourhoods[(i - 1) + nvmax * (p - 1)] != j {
                    p -= 1;
                }
                self.neighbourhoods[i - 1] = first;
                if self.neighbourhoods[(i - 1) + nvmax * (p - 1)] == j {
                    for i1 in 0..width {
                        self.vval2[i1 + width * i] =
                            self.vertex_operator[i1 + width * ((i - 1) + nvmax * (p - 1))];
                    }
                }
            }
            for i in 1..=m {
                for (k, coordinate) in point.iter_mut().enumerate() {
                    *coordinate = points[(i - 1) + m * k];
                }
                result[(i - 1) + m * (j - 1)] = ehg128(
                    &self.tree,
                    &point,
                    &self.vval2,
                    &mut self.solver.warnings,
                );
            }
        }
        result
    }

    /// `lowesa` and `ehg141`: approximate residual degrees of freedom from the trace of the
    /// operator, by interpolating between the fits for degree 1 and degree 2.
    fn lowesa(&mut self, trace: f64, tau: i64, singular: i32) -> (f64, f64) {
        let d = self.config.d;
        let n = self.config.n;
        let warnings = &mut self.solver.warnings;
        let (dka, delta1_a, delta2_a) = ehg141(trace, n, 1, tau, d, singular, warnings);
        let (dkb, delta1_b, delta2_b) = ehg141(trace, n, 2, tau, d, singular, warnings);
        let alpha = (tau - dka) as f64 / (dkb - dka) as f64;
        (
            (1.0 - alpha) * delta1_a + alpha * delta1_b,
            (1.0 - alpha) * delta2_a + alpha * delta2_b,
        )
    }
}

/// The least-squares machinery of one local fit and the state it carries from fit to fit
/// (`ehg127` and its workspace).
struct Solver {
    n: usize,
    d: usize,
    nf: usize,
    nonparametric: usize,
    degree: i32,
    span: f64,
    drop_square: [i32; 8],
    /// The number of columns of the local design. A degree-2 fit with dropped squares has
    /// fewer than the configured count, and the first fit lowers this for every later one.
    columns: usize,
    /// The point numbers in order of distance from the current fit point (`psi`), which stays
    /// permuted from one fit to the next.
    psi: Vec<usize>,
    distances: Vec<f64>,
    /// The weighted responses, then their images under the orthogonal factor (`eta`).
    eta: Vec<f64>,
    /// The weighted design, column-major `nf` by `columns`, then its QR factorization.
    design: Vec<f64>,
    weights: Vec<f64>,
    qraux: [f64; 15],
    sigma: [f64; 15],
    /// Left singular vectors, column-major with leading dimension 15.
    left: [f64; 225],
    /// Right singular vectors, scaled back for the equilibration, leading dimension 15.
    right: [f64; 225],
    gamma: [f64; 15],
    scratch: [f64; 15],
    tolerance: f64,
    reciprocal_condition: f64,
    /// How many fits needed the pseudoinverse (`sing`).
    singular: i32,
    warnings: Vec<String>,
}

impl Solver {
    fn new(config: &Config) -> Solver {
        Solver {
            n: config.n,
            d: config.d,
            nf: config.nf,
            nonparametric: config.nonparametric,
            degree: config.degree,
            span: config.span,
            drop_square: config.drop_square,
            columns: config.columns,
            psi: (1..=config.n).collect(),
            distances: vec![0.0; config.n],
            eta: vec![0.0; config.nf],
            design: vec![0.0; config.nf * config.columns],
            weights: vec![0.0; config.nf],
            qraux: [0.0; 15],
            sigma: [0.0; 15],
            left: [0.0; 225],
            right: [0.0; 225],
            gamma: [0.0; 15],
            scratch: [0.0; 15],
            tolerance: 0.0,
            reciprocal_condition: 1.0,
            singular: 0,
            warnings: Vec::new(),
        }
    }

    /// `ehg127`: the weighted local polynomial fit at `q`. Leaves the factorization behind for
    /// the operator calculations, and stores the value and slopes of the fit in `s`.
    fn ehg127(&mut self, q: &[f64], x: &[f64], y: &[f64], weights: &[f64], s: &mut [f64]) {
        let (n, d, nf, dd) = (self.n, self.d, self.nf, self.nonparametric);
        self.distances.iter_mut().for_each(|value| *value = 0.0);
        for j in 0..dd {
            let centre = q[j];
            for i in 0..n {
                let difference = x[i + n * j] - centre;
                self.distances[i] += difference * difference;
            }
        }
        ehg106(1, n, nf, &self.distances, &mut self.psi);
        let rho = self.distances[self.psi[nf - 1] - 1] * 1.0f64.max(self.span);
        if rho <= 0.0 {
            self.warnings.push(loess_warning(120));
        }
        for i in 0..nf {
            self.weights[i] = (self.distances[self.psi[i] - 1] / rho).sqrt();
        }
        for i in 0..nf {
            let w = self.weights[i];
            let cube = w * w * w;
            let inner = 1.0 - cube;
            self.weights[i] = (weights[self.psi[i] - 1] * (inner * inner * inner)).sqrt();
        }
        let mut largest = 0;
        for i in 1..nf {
            if self.weights[i].abs() > self.weights[largest].abs() {
                largest = i;
            }
        }
        if self.weights[largest].abs() == 0.0 {
            self.warnings.push(numbers_message("at ", &q[..dd]));
            self.warnings.push(numbers_message("radius ", &[rho]));
            self.warnings.push(loess_warning(121));
        }

        let mut column = 1;
        self.design.iter_mut().for_each(|value| *value = 0.0);
        for i in 0..nf {
            self.design[i] = self.weights[i];
        }
        if self.degree >= 1 {
            for j in 1..=d {
                if self.drop_square[j - 1] >= 1 {
                    column += 1;
                    let centre = q[j - 1];
                    for i in 0..nf {
                        self.design[i + nf * (column - 1)] =
                            self.weights[i] * (x[(self.psi[i] - 1) + n * (j - 1)] - centre);
                    }
                }
            }
        }
        if self.degree >= 2 {
            for j in 1..=d {
                if self.drop_square[j - 1] >= 1 {
                    if self.drop_square[j - 1] >= 2 {
                        column += 1;
                        let centre = q[j - 1];
                        for i in 0..nf {
                            let difference = x[(self.psi[i] - 1) + n * (j - 1)] - centre;
                            self.design[i + nf * (column - 1)] =
                                self.weights[i] * (difference * difference);
                        }
                    }
                    for jj in j + 1..=d {
                        if self.drop_square[jj - 1] >= 1 {
                            column += 1;
                            let (centre, other) = (q[j - 1], q[jj - 1]);
                            for i in 0..nf {
                                self.design[i + nf * (column - 1)] = self.weights[i]
                                    * (x[(self.psi[i] - 1) + n * (j - 1)] - centre)
                                    * (x[(self.psi[i] - 1) + n * (jj - 1)] - other);
                            }
                        }
                    }
                }
            }
            self.columns = column;
        }
        let k = self.columns;
        for i in 0..nf {
            self.eta[i] = self.weights[i] * y[self.psi[i] - 1];
        }

        let mut column_norm = [0.0f64; 15];
        for j in 1..=k {
            let mut scale = 0.0;
            for i in 0..nf {
                let value = self.design[i + nf * (j - 1)];
                scale += value * value;
            }
            let scale = f64::sqrt(scale);
            if 0.0 < scale {
                for i in 0..nf {
                    self.design[i + nf * (j - 1)] /= scale;
                }
                column_norm[j - 1] = scale;
            } else {
                column_norm[j - 1] = 1.0;
            }
        }

        let (nf_c, k_c, ldu) = (nf as c_int, k as c_int, 15 as c_int);
        let mut pivots = [0 as c_int; 15];
        let mut info: c_int = 0;
        // SAFETY: `design` holds `nf * columns` doubles with leading dimension `nf`, `columns`
        // is at most 15 (checked by the configuration), and the work and auxiliary arrays hold
        // 15 entries, which is what LINPACK reads and writes for `k <= 15` columns.
        unsafe {
            dqrdc_(
                self.design.as_mut_ptr(),
                &nf_c,
                &nf_c,
                &k_c,
                self.qraux.as_mut_ptr(),
                pivots.as_mut_ptr(),
                self.scratch.as_mut_ptr(),
                &0,
            );
        }
        self.apply_q(true);
        self.left.iter_mut().for_each(|value| *value = 0.0);
        for i in 1..=k {
            for j in i..=k {
                self.left[(i - 1) + 15 * (j - 1)] = self
                    .design
                    .get((i - 1) + nf * (j - 1))
                    .copied()
                    .unwrap_or(0.0);
            }
        }
        let mut superdiagonal = [0.0f64; 15];
        // SAFETY: `left` and `right` are 15 by 15 and `k <= 15`; the decomposition overwrites
        // `left` in place with the left singular vectors, which LINPACK allows when the matrix
        // and `u` are the same array.
        unsafe {
            dsvdc_(
                self.left.as_mut_ptr(),
                &ldu,
                &k_c,
                &k_c,
                self.sigma.as_mut_ptr(),
                superdiagonal.as_mut_ptr(),
                self.left.as_mut_ptr(),
                &ldu,
                self.right.as_mut_ptr(),
                &ldu,
                self.scratch.as_mut_ptr(),
                &21,
                &mut info,
            );
        }
        if info != 0 {
            self.warnings.push(loess_warning(182));
        }
        self.tolerance = self.sigma[0] * (100.0 * f64::EPSILON);
        self.reciprocal_condition = self
            .reciprocal_condition
            .min(self.sigma[k - 1] / self.sigma[0]);
        if self.sigma[k - 1] <= self.tolerance {
            self.singular += 1;
            if self.singular == 1 {
                self.warnings
                    .push(numbers_message("pseudoinverse used at", &q[..d]));
                self.warnings
                    .push(numbers_message("neighborhood radius", &[rho.sqrt()]));
                self.warnings.push(numbers_message(
                    "reciprocal condition number ",
                    &[self.reciprocal_condition],
                ));
            } else if self.singular == 2 {
                self.warnings.push(numbers_message(
                    "There are other near singularities as well.",
                    &[rho],
                ));
            }
        }
        for j in 1..=k {
            let scale = column_norm[j - 1];
            for i in 1..=k {
                self.right[(j - 1) + 15 * (i - 1)] /= scale;
            }
        }
        for j in 1..=k {
            self.gamma[j - 1] = if self.tolerance < self.sigma[j - 1] {
                // SAFETY: both vectors hold at least `k` doubles.
                let dot = unsafe {
                    ddot(k as isize, self.left[15 * (j - 1)..].as_ptr(), self.eta.as_ptr())
                };
                dot / self.sigma[j - 1]
            } else {
                0.0
            };
        }
        for j in 0..s.len() {
            s[j] = if j < k {
                self.right_row_dot_gamma(j + 1, k)
            } else {
                0.0
            };
        }
    }

    /// The `(row, column)` entry of the scaled right singular vectors, 1-based.
    fn right(&self, row: usize, column: usize) -> f64 {
        self.right[(row - 1) + 15 * (column - 1)]
    }

    /// Row `row` of the right singular vectors dotted with the coefficient vector `gamma`.
    fn right_row_dot_gamma(&self, row: usize, k: usize) -> f64 {
        let mut sum = 0.0;
        for column in 0..k {
            sum += self.right[(row - 1) + 15 * column] * self.gamma[column];
        }
        sum
    }

    /// `eta = w[i] * e_i` for neighbour `i`: the weighted unit response behind column `i` of
    /// the operator.
    fn unit_response(&mut self, i: usize) {
        self.eta.iter_mut().for_each(|value| *value = 0.0);
        self.eta[i - 1] = self.weights[i - 1];
    }

    /// Replaces `eta` by `Q' eta` or, with `transpose` false, by `Q eta`, for the factor `Q` of
    /// the current design.
    fn apply_q(&mut self, transpose: bool) {
        let (nf, k) = (self.nf as c_int, self.columns as c_int);
        let eta = self.eta.as_mut_ptr();
        let scratch = self.scratch.as_mut_ptr();
        let mut info: c_int = 0;
        let (job, qy, qty): (c_int, *mut f64, *mut f64) = if transpose {
            (1000, scratch, eta)
        } else {
            (10000, eta, scratch)
        };
        // SAFETY: `design` holds the `nf` by `columns` factorization `dqrdc_` produced and
        // `qraux` its auxiliary column; `eta` holds `nf` doubles. LINPACK allows the vector to
        // be overwritten by its own image, and the unused outputs point at the scratch array,
        // which this job never writes.
        unsafe {
            dqrsl_(
                self.design.as_mut_ptr(),
                &nf,
                &nf,
                &k,
                self.qraux.as_mut_ptr(),
                eta,
                qy,
                qty,
                eta,
                scratch,
                scratch,
                &job,
                &mut info,
            );
        }
    }

    /// `gamma = SIGMA^+ U' eta` for the first `k` entries of the rotated response in `eta`.
    fn solve_gamma(&mut self) {
        let k = self.columns;
        for j in 1..=k {
            self.gamma[j - 1] = if self.tolerance < self.sigma[j - 1] {
                // SAFETY: both vectors hold at least `k` doubles.
                let dot = unsafe {
                    ddot(k as isize, self.left[15 * (j - 1)..].as_ptr(), self.eta.as_ptr())
                };
                dot / self.sigma[j - 1]
            } else {
                0.0
            };
        }
    }

    /// Like [`Self::solve_gamma`], accumulating `U' eta` column by column as `ehg136` does.
    fn gamma_from_left_vectors(&mut self) {
        let k = self.columns;
        self.gamma[..k].iter_mut().for_each(|value| *value = 0.0);
        for j in 1..=k {
            let weight = self.eta[j - 1];
            for i in 1..=k {
                self.gamma[i - 1] += weight * self.left[(j - 1) + 15 * (i - 1)];
            }
        }
        for j in 1..=k {
            self.gamma[j - 1] = if self.tolerance < self.sigma[j - 1] {
                self.gamma[j - 1] / self.sigma[j - 1]
            } else {
                0.0
            };
        }
    }

    /// `eta = Q U[, j] / sigma[j] * w`: the weighted neighbour coefficients of singular
    /// direction `j`, from which the operator rows are assembled.
    fn left_vector_response(&mut self, j: usize) {
        let (nf, k) = (self.nf, self.columns);
        self.eta.iter_mut().for_each(|value| *value = 0.0);
        for i in 1..=k {
            self.eta[i - 1] = self.left[(i - 1) + 15 * (j - 1)];
        }
        self.apply_q(false);
        let scale = if self.tolerance < self.sigma[j - 1] {
            1.0 / self.sigma[j - 1]
        } else {
            0.0
        };
        for i in 0..nf {
            self.eta[i] *= scale * self.weights[i];
        }
    }
}

/// `lowesc`: the trace of the operator `l` (column-major `n` by `n`) and the residual degrees
/// of freedom `tr((I - L)(I - L)')` and `tr(((I - L)(I - L)')^2)`.
fn lowesc(n: usize, l: &mut [f64]) -> (f64, f64, f64) {
    for i in 0..n {
        l[i + n * i] -= 1.0;
    }
    let mut product = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut sum = 0.0;
            for t in 0..n {
                sum += l[i + n * t] * l[j + n * t];
            }
            product[i + n * j] = sum;
        }
    }
    for i in 0..n {
        for j in i + 1..n {
            product[i + n * j] = product[j + n * i];
        }
    }
    for i in 0..n {
        l[i + n * i] += 1.0;
    }
    let mut trace = 0.0;
    let mut delta1 = 0.0;
    for i in 0..n {
        trace += l[i + n * i];
        delta1 += product[i + n * i];
    }
    let mut delta2 = 0.0;
    for i in 0..n {
        let mut sum = 0.0;
        for t in 0..n {
            sum += product[i + n * t] * product[t + n * i];
        }
        delta2 += sum;
    }
    (trace, delta1, delta2)
}

/// `ehg141`: the approximate residual degrees of freedom for local degree `degree`, from an
/// empirical fit by Cleveland and Grosse. Returns the number of local parameters too.
fn ehg141(
    trace: f64,
    n: usize,
    degree: i32,
    k: i64,
    d: usize,
    singular: i32,
    warnings: &mut Vec<String>,
) -> (i64, f64, f64) {
    const COEFFICIENTS: [f64; 48] = [
        0.2971620, 0.3802660, 0.5886043, 0.4263766, 0.3346498, 0.6271053, 0.5241198, 0.3484836,
        0.6687687, 0.6338795, 0.4076457, 0.7207693, 0.1611761, 0.3091323, 0.4401023, 0.2939609,
        0.3580278, 0.5555741, 0.3972390, 0.4171278, 0.6293196, 0.4675173, 0.4699070, 0.6674802,
        0.2848308, 0.2254512, 0.2914126, 0.5393624, 0.2517230, 0.3898970, 0.7603231, 0.2969113,
        0.4740130, 0.9664956, 0.3629838, 0.5348889, 0.2075670, 0.2822574, 0.2369957, 0.3911566,
        0.2981154, 0.3623232, 0.5508869, 0.3501989, 0.4371032, 0.7002667, 0.4291632, 0.4930370,
    ];
    let dk = match degree {
        0 => 1,
        1 => d as i64 + 1,
        _ => (((d + 2) * (d + 1)) as f64 / 2.0) as i64,
    };
    let corx = (k as f64 / n as f64).sqrt();
    let mut z = ((k as f64 / trace).sqrt() - corx) / (1.0 - corx);
    if singular == 0 && 1.0 < z {
        warnings.push(numbers_message("Chernobyl! trL<k", &[trace]));
    } else if z < 0.0 {
        warnings.push(numbers_message("Chernobyl! trL>n", &[trace]));
    }
    z = 1.0f64.min(0.0f64.max(z));
    let c4 = ehg176(z).exp();
    let coefficient = |index: usize| COEFFICIENTS[index - 1];
    let mut i = 1 + 3 * ((d.min(4) - 1) + 4 * (degree as usize - 1));
    let mut delta = [0.0; 2];
    for slot in delta.iter_mut() {
        let (c1, c2, c3) = if d <= 4 {
            (coefficient(i), coefficient(i + 1), coefficient(i + 2))
        } else {
            let extra = (d - 4) as f64;
            (
                coefficient(i) + extra * (coefficient(i) - coefficient(i - 3)),
                coefficient(i + 1) + extra * (coefficient(i + 1) - coefficient(i - 2)),
                coefficient(i + 2) + extra * (coefficient(i + 2) - coefficient(i - 1)),
            )
        };
        *slot = n as f64 - trace * (c1 * z.powf(c2) * (1.0 - z).powf(c3) * c4).exp();
        i += 24;
    }
    (dk, delta[0], delta[1])
}

/// `ehg196` and `ehg197`: the approximate trace of the operator from the span alone.
fn ehg196(tau: i64, d: usize, span: f64) -> f64 {
    let approximate = |degree: i32| {
        let dk = match degree {
            1 => d as i64 + 1,
            _ => (((d + 2) * (d + 1)) as f64 / 2.0) as i64,
        };
        let g1 = (-0.08125 * d as f64 + 0.13) * d as f64 + 1.05;
        (dk, dk as f64 * (1.0 + 0.0f64.max((g1 - span) / span)))
    };
    let (dka, trace_a) = approximate(1);
    let (dkb, trace_b) = approximate(2);
    let alpha = (tau - dka) as f64 / (dkb - dka) as f64;
    (1.0 - alpha) * trace_a + alpha * trace_b
}

/// `ehg176`: a fixed one-dimensional loess fit (its k-d tree and vertex values are data) of the
/// correction `ehg141` applies to the approximation, evaluated at `z`.
fn ehg176(z: f64) -> f64 {
    const SPLIT_DIMENSION: [i32; 17] = [1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0];
    const LOWER: [(usize, usize); 8] = [
        (1, 2),
        (2, 4),
        (3, 6),
        (4, 8),
        (5, 10),
        (6, 12),
        (7, 14),
        (15, 16),
    ];
    const SPLIT_VALUE: [(usize, f64); 8] = [
        (1, 0.3705),
        (2, 0.2017),
        (3, 0.5591),
        (4, 0.1204),
        (5, 0.2815),
        (6, 0.4536),
        (7, 0.7132),
        (15, 0.8751),
    ];
    const CORNERS: [[usize; 2]; 17] = [
        [1, 2],
        [1, 3],
        [3, 2],
        [1, 4],
        [4, 3],
        [3, 5],
        [5, 2],
        [1, 6],
        [6, 4],
        [4, 7],
        [7, 3],
        [3, 8],
        [8, 5],
        [5, 9],
        [9, 2],
        [9, 10],
        [10, 2],
    ];
    const VERTICES: [f64; 10] = [
        -5.0e-3, 1.005, 0.3705, 0.2017, 0.5591, 0.1204, 0.2815, 0.4536, 0.7132, 0.8751,
    ];
    const VALUES: [f64; 10] = [
        -9.0572e-2, -1.0856e-2, -5.3718e-2, 2.6152e-2, -5.8387e-2, 9.5807e-2, -3.1926e-2,
        -6.4170e-2, -2.0636e-2, 4.0172e-2,
    ];
    const SLOPES: [f64; 10] = [
        4.4844, -0.7736, -0.3495, -0.7286, 0.1611, -0.7978, -0.4457, 3.2813e-2, 0.3350,
        -4.1032e-2,
    ];

    let mut tree = Tree::new(1, 10, 17);
    tree.nv = 10;
    tree.nc = 17;
    for cell in 1..=17 {
        tree.split_dimension[cell] = SPLIT_DIMENSION[cell - 1];
        tree.set_corner(cell, 1, CORNERS[cell - 1][0]);
        tree.set_corner(cell, 2, CORNERS[cell - 1][1]);
    }
    for &(cell, lower) in &LOWER {
        tree.lower[cell] = lower;
        tree.upper[cell] = lower + 1;
    }
    for &(cell, cut) in &SPLIT_VALUE {
        tree.split_value[cell] = cut;
    }
    let mut vval = vec![0.0; 2 * 11];
    for vertex in 1..=10 {
        tree.set_vertex(vertex, 1, VERTICES[vertex - 1]);
        vval[2 * vertex] = VALUES[vertex - 1];
        vval[1 + 2 * vertex] = SLOPES[vertex - 1];
    }
    ehg128(&tree, &[z], &vval, &mut Vec::new())
}

/// The text of `loesswarn(code)`.
fn loess_warning(code: i32) -> String {
    match code {
        104 => "span too small.   fewer data values than degrees of freedom.".to_string(),
        105 => "k>d2MAX in ehg136.  Need to recompile with increased dimensions.".to_string(),
        120 => "zero-width neighborhood. make span bigger".to_string(),
        121 => "all data on boundary of neighborhood. make span bigger".to_string(),
        122 => "extrapolation not allowed with blending".to_string(),
        123 => "ihat=1 (diag L) in l2fit only makes sense if z=x (eval=data).".to_string(),
        180 => "nv>nvmax in cpvert.".to_string(),
        181 => "nt>20 in eval.".to_string(),
        182 => "svddc failed in l2fit.".to_string(),
        185 => "trouble descending to leaf in vleaf.".to_string(),
        187 => "insufficient stack space".to_string(),
        193 => "workspace in loread appears to be corrupted".to_string(),
        194 => "trouble in l2fit/l2tr".to_string(),
        196 => "degree must be at least 1 for vertex influence matrix".to_string(),
        other => format!("Assert failed; error code {other}\n"),
    }
}

/// `ehg184a`: the message `prefix` followed by each value as `%.5g`, then a newline.
fn numbers_message(prefix: &str, values: &[f64]) -> String {
    let mut message = prefix.to_string();
    for &value in values {
        let mut buffer = [0u8; 64];
        // SAFETY: the format takes one double and the buffer holds any `%.5g` output.
        let written = unsafe {
            libc::snprintf(
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                c" %.5g".as_ptr(),
                value,
            )
        };
        message.push_str(&String::from_utf8_lossy(&buffer[..written.clamp(0, 63) as usize]));
    }
    message.push('\n');
    message
}

/// `ehg183a`: the message `prefix` followed by the integer, then a newline.
fn integer_message(prefix: &str, value: i64) -> String {
    format!("{prefix} {value}\n")
}
