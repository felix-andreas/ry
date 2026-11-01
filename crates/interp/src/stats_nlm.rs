//! stats' `nlm`: unconstrained minimization by a quasi-Newton method with a line search.
//!
//! The algorithm is the modular one of Dennis and Schnabel, "Numerical Methods for
//! Unconstrained Optimization and Nonlinear Equations" (1983, appendix A): forward or central
//! difference gradients (A5.6.3, A5.6.4), the backtracking line search (A6.3.1), the stopping
//! tests (A7.2.1), the factored BFGS update (A9.4.2) with its QR update (A3.4.1), and, when the
//! objective supplies its Hessian, Newton steps through the perturbed Cholesky factorization
//! (A5.5.1, A5.5.2). GNU R's `nlm` uses the same algorithm, so the iterates, and with them the
//! printed iteration counts and residual gradients, agree with it.

use crate::{
    builtin_libraries::NativeCall,
    coerce::coerce_vector,
    eval,
    print::{PrintOptions, print_vector},
    stats::{real_matrix, real_values},
    unwind::R,
    values::{self, length},
};
use heap::object::{NA_INTEGER, Sexp, SexpType};

/// `nlm(f, p, hessian, typsize, fscale, msg, ndigit, gradtol, stepmax, steptol, iterlim)`,
/// called through `.External2`. `msg` holds the print level as bits: 8 silences all output,
/// 16 prints every iteration, and 2 and 4 skip the checks of an analytic gradient and Hessian.
pub unsafe fn nlm(call: &NativeCall<'_>) -> R<Sexp> {
    let function = call.arg(0);
    if !values::is_function(function) {
        return Err(call.error("attempt to minimize non-function"));
    }
    let start = real_values(coerce_vector(call.arg(1), SexpType::Real)?)?;
    let n = start.len();
    if n == 0 {
        return Err(call.error("non-positive number of parameters in nlm"));
    }
    if start.iter().any(|value| value.is_nan()) {
        return Err(call.error("missing value in parameter"));
    }
    let want_hessian = values::as_logical_scalar(call.arg(2)) == 1;
    let mut typical = real_values(coerce_vector(call.arg(3), SexpType::Real)?)?;
    if typical.len() != n {
        return Err(call.error("parameter vector and 'typsize' have different lengths"));
    }
    for size in &mut typical {
        *size = if *size == 0.0 { 1.0 } else { size.abs() };
    }
    let scale: Vec<f64> = typical.iter().map(|size| 1.0 / size).collect();
    let fscale = match values::as_real_scalar(call.arg(4)) {
        0.0 => 1.0,
        value => value.abs(),
    };
    let message = values::as_integer_scalar(call.arg(5));
    let digits = values::as_integer_scalar(call.arg(6));
    let gradient_tolerance = values::as_real_scalar(call.arg(7));
    let max_step = values::as_real_scalar(call.arg(8));
    let step_tolerance = values::as_real_scalar(call.arg(9));
    let iteration_limit = values::as_integer_scalar(call.arg(10));
    if gradient_tolerance.is_nan() || gradient_tolerance < 0.0 {
        return Err(call.error("invalid gradient tolerance in nlm"));
    }
    if iteration_limit == NA_INTEGER || iteration_limit <= 0 {
        return Err(call.error("invalid iteration limit in nlm"));
    }
    let max_step = if max_step > 0.0 {
        max_step
    } else {
        let size = start
            .iter()
            .zip(&scale)
            .map(|(x, s)| (x * s).powi(2))
            .sum::<f64>()
            .sqrt();
        (1000.0 * size).max(1000.0)
    };
    let mut objective = Objective {
        call,
        function,
        n,
        last: None,
    };
    let first = objective.evaluate(&start)?;
    let settings = Settings {
        scale,
        typical,
        fscale,
        gradient_tolerance,
        step_tolerance,
        max_step,
        iteration_limit,
        noise: 10f64.powi(-digits).max(f64::EPSILON),
        analytic_gradient: first.gradient.is_some(),
        analytic_hessian: first.gradient.is_some() && first.hessian.is_some(),
        silent: message & 8 != 0,
        trace: message & 16 != 0,
        check_gradient: message & 2 == 0,
    };
    let outcome = minimize(&mut objective, &start, &settings)?;
    let mut entries = vec![
        ("minimum", values::scalar_real(outcome.value)?),
        ("estimate", values::real_vector(&outcome.estimate)?),
        ("gradient", values::real_vector(&outcome.gradient)?),
    ];
    if want_hessian {
        let hessian = difference_hessian(
            &mut objective,
            &outcome.estimate,
            outcome.value,
            &settings.typical,
            digits,
        )?;
        entries.push(("hessian", real_matrix(&hessian, n, n)?));
    }
    entries.push(("code", values::scalar_integer(outcome.code)?));
    entries.push(("iterations", values::scalar_integer(outcome.iterations)?));
    values::named_list(&entries)
}

struct Settings {
    /// The reciprocal typical size of each parameter, `Dx` in Dennis and Schnabel.
    scale: Vec<f64>,
    typical: Vec<f64>,
    fscale: f64,
    gradient_tolerance: f64,
    step_tolerance: f64,
    max_step: f64,
    iteration_limit: i32,
    /// The relative noise in the objective's values, `eta`.
    noise: f64,
    analytic_gradient: bool,
    analytic_hessian: bool,
    silent: bool,
    trace: bool,
    check_gradient: bool,
}

struct Outcome {
    value: f64,
    estimate: Vec<f64>,
    gradient: Vec<f64>,
    code: i32,
    iterations: i32,
}

/// How the gradient is computed. A failed line search with forward differences switches to
/// central differences for the rest of the run.
#[derive(Clone, Copy, PartialEq)]
enum GradientKind {
    Analytic,
    Forward,
    Central,
}

unsafe fn minimize(
    objective: &mut Objective<'_>,
    start: &[f64],
    settings: &Settings,
) -> R<Outcome> {
    let n = start.len();
    let scale = &settings.scale;
    let mut kind = if settings.analytic_gradient {
        GradientKind::Analytic
    } else {
        GradientKind::Forward
    };
    let mut x = start.to_vec();
    let mut f = objective.value(&x)?;
    let mut g = gradient(objective, &x, f, kind, settings)?;
    if settings.check_gradient && kind == GradientKind::Analytic {
        let estimate = forward_gradient(objective, &x, f, settings)?;
        let tolerance = settings.noise.sqrt().max(1e-2);
        let wrong = (0..n).any(|i| {
            let size = f.abs().max(settings.fscale) / x[i].abs().max(settings.typical[i]);
            (g[i] - estimate[i]).abs() > g[i].abs().max(size) * tolerance
        });
        if wrong {
            return Err(objective
                .call
                .error("probable coding error in analytic gradient"));
        }
    }
    let mut iterations = 0;
    let mut consecutive_max_steps = 0;
    let mut code = stopping_code(
        &x,
        f,
        &g,
        &x,
        iterations,
        &mut consecutive_max_steps,
        None,
        false,
        settings,
    );
    if code != 0 {
        if !settings.silent {
            print_iterate(iterations, None, &x, f, &g)?;
            print_stop_message(code);
        }
        return Ok(Outcome {
            value: f,
            estimate: x,
            gradient: g,
            code,
            iterations,
        });
    }
    // The secant method keeps the Cholesky factor `L` of its Hessian approximation in the
    // lower triangle; Newton's method keeps the Hessian there and factors it each iteration.
    let mut factor = vec![0.0; n * n];
    if settings.analytic_hessian {
        objective.hessian_into(&x, &mut factor)?;
    } else {
        for i in 0..n {
            factor[i * n + i] = scale[i];
        }
    }
    let mut first_update = true;
    if !settings.silent {
        print_iterate(iterations, Some(&vec![0.0; n]), &x, f, &g)?;
    }
    loop {
        iterations += 1;
        if settings.analytic_hessian {
            perturbed_cholesky(&mut factor, n, scale);
        }
        let (mut x_new, mut f_new, mut took_max_step, mut failed);
        loop {
            let direction = newton_direction(&factor, n, &g);
            let search = line_search(objective, &x, f, &g, direction, settings)?;
            (x_new, f_new, took_max_step, failed) =
                (search.x, search.f, search.took_max_step, search.failed);
            if failed && kind == GradientKind::Forward {
                kind = GradientKind::Central;
                g = central_gradient(objective, &x, settings)?;
                continue;
            }
            break;
        }
        let g_new = gradient(objective, &x_new, f_new, kind, settings)?;
        code = stopping_code(
            &x_new,
            f_new,
            &g_new,
            &x,
            iterations,
            &mut consecutive_max_steps,
            Some(failed),
            took_max_step,
            settings,
        );
        if code != 0 {
            if code == 3 {
                x_new = x;
                f_new = f;
                return finish(settings, iterations, code, x_new, f_new, g);
            }
            return finish(settings, iterations, code, x_new, f_new, g_new);
        }
        if settings.analytic_hessian {
            objective.hessian_into(&x_new, &mut factor)?;
        } else {
            let relative_tolerance = if kind == GradientKind::Forward {
                settings.noise.sqrt()
            } else {
                settings.noise
            };
            secant_update(
                &mut factor,
                n,
                &x,
                &x_new,
                &g,
                &g_new,
                scale,
                relative_tolerance,
                &mut first_update,
            );
        }
        if settings.trace && !settings.silent {
            let step: Vec<f64> = x_new.iter().zip(&x).map(|(new, old)| new - old).collect();
            print_iterate(iterations, Some(&step), &x_new, f_new, &g_new)?;
        }
        x = x_new;
        f = f_new;
        g = g_new;
    }
}

unsafe fn finish(
    settings: &Settings,
    iterations: i32,
    code: i32,
    x: Vec<f64>,
    f: f64,
    g: Vec<f64>,
) -> R<Outcome> {
    if !settings.silent {
        print_iterate(iterations, None, &x, f, &g)?;
        print_stop_message(code);
    }
    Ok(Outcome {
        value: f,
        estimate: x,
        gradient: g,
        code,
        iterations,
    })
}

unsafe fn gradient(
    objective: &mut Objective<'_>,
    x: &[f64],
    f: f64,
    kind: GradientKind,
    settings: &Settings,
) -> R<Vec<f64>> {
    match kind {
        GradientKind::Analytic => objective.gradient(x),
        GradientKind::Forward => forward_gradient(objective, x, f, settings),
        GradientKind::Central => central_gradient(objective, x, settings),
    }
}

/// Forward differences with steps of `sqrt(eta)` relative to each parameter's size (A5.6.3).
unsafe fn forward_gradient(
    objective: &mut Objective<'_>,
    x: &[f64],
    f: f64,
    settings: &Settings,
) -> R<Vec<f64>> {
    let mut point = x.to_vec();
    let mut result = Vec::with_capacity(x.len());
    for j in 0..x.len() {
        let step = settings.noise.sqrt() * x[j].abs().max(1.0 / settings.scale[j]);
        point[j] = x[j] + step;
        let shifted = objective.value(&point)?;
        point[j] = x[j];
        result.push((shifted - f) / step);
    }
    Ok(result)
}

/// Central differences with steps of `eta^(1/3)` relative to each parameter's size (A5.6.4).
unsafe fn central_gradient(
    objective: &mut Objective<'_>,
    x: &[f64],
    settings: &Settings,
) -> R<Vec<f64>> {
    let mut point = x.to_vec();
    let mut result = Vec::with_capacity(x.len());
    for i in 0..x.len() {
        let step = settings.noise.cbrt() * x[i].abs().max(1.0 / settings.scale[i]);
        point[i] = x[i] + step;
        let plus = objective.value(&point)?;
        point[i] = x[i] - step;
        let minus = objective.value(&point)?;
        point[i] = x[i];
        result.push((plus - minus) / (step * 2.0));
    }
    Ok(result)
}

/// The termination tests of A7.2.1: 1 for a small relative gradient, 2 for a small relative
/// step, 3 for a failed line search, 4 for the iteration limit and 5 for five maximal steps
/// in a row; 0 continues.
#[allow(clippy::too_many_arguments)]
fn stopping_code(
    x_new: &[f64],
    f_new: f64,
    g_new: &[f64],
    x: &[f64],
    iterations: i32,
    consecutive_max_steps: &mut i32,
    line_search_failed: Option<bool>,
    took_max_step: bool,
    settings: &Settings,
) -> i32 {
    if line_search_failed == Some(true) {
        return 3;
    }
    let size = |i: usize| x_new[i].abs().max(1.0 / settings.scale[i]);
    let denominator = f_new.abs().max(settings.fscale);
    let relative_gradient = (0..x_new.len())
        .map(|i| g_new[i].abs() * size(i) / denominator)
        .fold(0.0, f64::max);
    if relative_gradient <= settings.gradient_tolerance {
        return 1;
    }
    if iterations == 0 {
        return 0;
    }
    let relative_step = (0..x_new.len())
        .map(|i| (x_new[i] - x[i]).abs() / size(i))
        .fold(0.0, f64::max);
    if relative_step <= settings.step_tolerance {
        return 2;
    }
    if iterations >= settings.iteration_limit {
        return 4;
    }
    if !took_max_step {
        *consecutive_max_steps = 0;
        return 0;
    }
    *consecutive_max_steps += 1;
    if *consecutive_max_steps >= 5 { 5 } else { 0 }
}

/// The step `-(L L')^-1 g` for the factor `L` in the lower triangle.
fn newton_direction(factor: &[f64], n: usize, g: &[f64]) -> Vec<f64> {
    let mut y = vec![0.0; n];
    for i in 0..n {
        let sum: f64 = (0..i).map(|k| factor[i * n + k] * y[k]).sum();
        y[i] = (-g[i] - sum) / factor[i * n + i];
    }
    let mut p = vec![0.0; n];
    for i in (0..n).rev() {
        let sum: f64 = (i + 1..n).map(|k| factor[k * n + i] * p[k]).sum();
        p[i] = (y[i] - sum) / factor[i * n + i];
    }
    p
}

struct Search {
    x: Vec<f64>,
    f: f64,
    took_max_step: bool,
    failed: bool,
}

/// The backtracking line search of A6.3.1: accept the first point along `direction` with
/// sufficient decrease, backtracking by a quadratic and then cubic model of the objective,
/// and fail once the step shrinks below the step tolerance.
unsafe fn line_search(
    objective: &mut Objective<'_>,
    x: &[f64],
    f: f64,
    g: &[f64],
    mut direction: Vec<f64>,
    settings: &Settings,
) -> R<Search> {
    let n = x.len();
    let scale = &settings.scale;
    let mut length = (0..n)
        .map(|i| (scale[i] * direction[i]).powi(2))
        .sum::<f64>()
        .sqrt();
    if length > settings.max_step {
        let shrink = settings.max_step / length;
        for value in &mut direction {
            *value *= shrink;
        }
        length = settings.max_step;
    }
    let slope: f64 = (0..n).map(|i| g[i] * direction[i]).sum();
    let relative_length = (0..n)
        .map(|i| direction[i].abs() / x[i].abs().max(1.0 / scale[i]))
        .fold(0.0, f64::max);
    let min_lambda = settings.step_tolerance / relative_length;
    let mut lambda = 1.0;
    let mut first_backtrack = true;
    let (mut previous_lambda, mut previous_f) = (0.0, 0.0);
    loop {
        let point: Vec<f64> = (0..n).map(|i| x[i] + lambda * direction[i]).collect();
        let value = objective.value(&point)?;
        if value <= f + slope * 1e-4 * lambda {
            let took_max_step = lambda == 1.0 && length > settings.max_step * 0.99;
            return Ok(Search {
                x: point,
                f: value,
                took_max_step,
                failed: false,
            });
        }
        if lambda < min_lambda {
            return Ok(Search {
                x: point,
                f: value,
                took_max_step: false,
                failed: true,
            });
        }
        let next = if first_backtrack {
            first_backtrack = false;
            -lambda * slope / ((value - f - slope) * 2.0)
        } else {
            let t1 = value - f - lambda * slope;
            let t2 = previous_f - f - previous_lambda * slope;
            let t3 = 1.0 / (lambda - previous_lambda);
            let a3 = t3 * (t1 / (lambda * lambda) - t2 / (previous_lambda * previous_lambda));
            let b = t3
                * (t2 * lambda / (previous_lambda * previous_lambda)
                    - t1 * previous_lambda / (lambda * lambda));
            let discriminant = b * b - a3 * 3.0 * slope;
            let root = discriminant.sqrt();
            // One positive critical point means it is the minimum; two mean the first is.
            let candidate = if discriminant > b * b {
                (-b + if a3 < 0.0 { -root } else { root }) / (a3 * 3.0)
            } else {
                (-b + if a3 < 0.0 { root } else { -root }) / (a3 * 3.0)
            };
            candidate.min(lambda * 0.5)
        };
        previous_lambda = lambda;
        previous_f = value;
        lambda = if next < lambda * 0.1 {
            lambda * 0.1
        } else {
            next
        };
    }
}

/// The factored BFGS update of A9.4.2 on the Cholesky factor in the lower triangle. The first
/// update rescales the initial factor; an update is skipped when `y's` is too small or when
/// the current model already predicts the gradient change within the noise.
#[allow(clippy::too_many_arguments)]
fn secant_update(
    factor: &mut [f64],
    n: usize,
    x: &[f64],
    x_new: &[f64],
    g: &[f64],
    g_new: &[f64],
    scale: &[f64],
    relative_tolerance: f64,
    first_update: &mut bool,
) {
    let s: Vec<f64> = (0..n).map(|i| x_new[i] - x[i]).collect();
    let y: Vec<f64> = (0..n).map(|i| g_new[i] - g[i]).collect();
    let den1: f64 = (0..n).map(|i| s[i] * y[i]).sum();
    let s_norm = (0..n)
        .map(|i| (scale[i] * s[i]).powi(2))
        .sum::<f64>()
        .sqrt();
    let y_norm = y.iter().map(|value| value * value).sum::<f64>().sqrt();
    if den1 < f64::EPSILON.sqrt() * s_norm * y_norm {
        return;
    }
    // u = L' s
    let mut u: Vec<f64> = (0..n)
        .map(|i| (i..n).map(|k| factor[k * n + i] * s[k]).sum())
        .collect();
    let mut den2: f64 = u.iter().map(|value| value * value).sum();
    let mut alpha = (den1 / den2).sqrt();
    if *first_update {
        for value in &mut u {
            *value *= alpha;
        }
        for i in 0..n {
            for j in 0..=i {
                factor[i * n + j] *= alpha;
            }
        }
        *first_update = false;
        den2 = den1;
        alpha = 1.0;
    }
    // w = L u
    let mut w: Vec<f64> = (0..n)
        .map(|i| (0..=i).map(|k| factor[i * n + k] * u[k]).sum())
        .collect();
    let skip =
        (0..n).all(|i| (y[i] - w[i]).abs() < relative_tolerance * g[i].abs().max(g_new[i].abs()));
    if skip {
        return;
    }
    for i in 0..n {
        w[i] = y[i] - alpha * w[i];
    }
    let alpha = 1.0 / (den1 * den2).sqrt();
    for value in &mut w {
        *value *= alpha;
    }
    // The upper triangle holds R = L' for the QR update.
    let mut upper = vec![0.0; n * n];
    for i in 0..n {
        for j in i..n {
            upper[i * n + j] = factor[j * n + i];
        }
    }
    qr_update(&mut upper, n, &mut u, &w);
    for i in 0..n {
        for j in 0..=i {
            factor[i * n + j] = upper[j * n + i];
        }
    }
}

/// Replaces the upper triangular `R` by the triangular factor of `R + u w'` (A3.4.1): Jacobi
/// rotations reduce `u` to a multiple of the first unit vector, the rank-one term is added to
/// the first row, and rotations restore the triangle from the resulting Hessenberg matrix.
fn qr_update(upper: &mut [f64], n: usize, u: &mut [f64], w: &[f64]) {
    let mut k = n - 1;
    while k > 0 && u[k] == 0.0 {
        k -= 1;
    }
    for i in (0..k).rev() {
        if u[i] == 0.0 {
            swap_rows(upper, n, i);
            u[i] = u[i + 1];
        } else {
            rotate_rows(upper, n, i, u[i], -u[i + 1]);
            u[i] = u[i].hypot(u[i + 1]);
        }
    }
    for j in 0..n {
        upper[j] += u[0] * w[j];
    }
    for i in 0..k {
        if upper[i * n + i] == 0.0 {
            swap_rows(upper, n, i);
        } else {
            let (a, b) = (upper[i * n + i], -upper[(i + 1) * n + i]);
            rotate_rows(upper, n, i, a, b);
        }
    }
}

fn swap_rows(matrix: &mut [f64], n: usize, i: usize) {
    for j in i..n {
        matrix.swap(i * n + j, (i + 1) * n + j);
    }
}

fn rotate_rows(matrix: &mut [f64], n: usize, i: usize, a: f64, b: f64) {
    let length = a.hypot(b);
    let (cosine, sine) = (a / length, b / length);
    for j in i..n {
        let (top, bottom) = (matrix[i * n + j], matrix[(i + 1) * n + j]);
        matrix[i * n + j] = cosine * top - sine * bottom;
        matrix[(i + 1) * n + j] = sine * top + cosine * bottom;
    }
}

/// Replaces the Hessian in the lower triangle by the Cholesky factor of `H + mu I`, scaled,
/// with `mu` the smallest shift that makes it safely positive definite (A5.5.1).
fn perturbed_cholesky(matrix: &mut [f64], n: usize, scale: &[f64]) {
    for i in 0..n {
        for j in 0..=i {
            matrix[i * n + j] /= scale[i] * scale[j];
        }
    }
    let tolerance = f64::EPSILON.sqrt();
    let diagonal = |matrix: &[f64]| (0..n).map(|i| matrix[i * n + i]).collect::<Vec<f64>>();
    let entries = diagonal(matrix);
    let mut largest = entries.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let smallest = entries.iter().copied().fold(f64::INFINITY, f64::min);
    let positive_largest = largest.max(0.0);
    if smallest <= positive_largest * tolerance {
        let mut shift = tolerance * (positive_largest - smallest) - smallest;
        if shift == 0.0 {
            let off_diagonal = (0..n)
                .flat_map(|i| (0..i).map(move |j| (i, j)))
                .map(|(i, j)| matrix[i * n + j].abs())
                .fold(0.0, f64::max);
            shift = if off_diagonal == 0.0 {
                1.0
            } else {
                off_diagonal * (1.0 + tolerance)
            };
        }
        for i in 0..n {
            matrix[i * n + i] += shift;
        }
        largest += shift;
    }
    // The upper triangle keeps the shifted Hessian in case a second factorization is needed.
    let saved_diagonal = diagonal(matrix);
    for i in 0..n {
        for j in 0..i {
            matrix[j * n + i] = matrix[i * n + j];
        }
    }
    let added = cholesky_with_additions(matrix, n, largest, tolerance);
    if added > 0.0 {
        for i in 0..n {
            matrix[i * n + i] = saved_diagonal[i];
            for j in 0..i {
                matrix[i * n + j] = matrix[j * n + i];
            }
        }
        let mut lowest = 0.0f64;
        let mut highest = matrix[0];
        for i in 0..n {
            let row: f64 = (0..n)
                .filter(|&j| j != i)
                .map(|j| matrix[i.min(j) * n + i.max(j)].abs())
                .sum();
            lowest = lowest.min(matrix[i * n + i] - row);
            highest = highest.max(matrix[i * n + i] + row);
        }
        let spread = tolerance * (highest - lowest) - lowest;
        let shift = spread.min(added);
        for i in 0..n {
            matrix[i * n + i] += shift;
        }
        cholesky_with_additions(matrix, n, 0.0, tolerance);
    }
    for i in 0..n {
        for j in 0..=i {
            matrix[i * n + j] *= scale[i];
        }
    }
}

/// Cholesky factorization into the lower triangle that raises a diagonal element too small
/// for a stable factor, returning the largest amount added (A5.5.2).
fn cholesky_with_additions(
    matrix: &mut [f64],
    n: usize,
    largest_diagonal: f64,
    tolerance: f64,
) -> f64 {
    let mut added = 0.0f64;
    let minimum = (largest_diagonal * tolerance).sqrt();
    let minimum_squared = minimum * minimum;
    for j in 0..n {
        let sum: f64 = (0..j).map(|k| matrix[j * n + k].powi(2)).sum();
        let remainder = matrix[j * n + j] - sum;
        if remainder >= minimum_squared {
            matrix[j * n + j] = remainder.sqrt();
        } else {
            let mut off_diagonal = (j + 1..n)
                .map(|i| matrix[i * n + j].abs())
                .fold(0.0, f64::max);
            if off_diagonal <= minimum_squared {
                off_diagonal = minimum_squared;
            }
            matrix[j * n + j] = off_diagonal.sqrt();
            added = added.max(off_diagonal - remainder);
        }
        for i in j + 1..n {
            let sum: f64 = (0..j).map(|k| matrix[i * n + k] * matrix[j * n + k]).sum();
            matrix[i * n + j] = (matrix[i * n + j] - sum) / matrix[j * n + j];
        }
    }
    added
}

/// The Hessian `nlm(hessian = TRUE)` reports: second differences of the objective with steps
/// of `10^(-ndigit/3)` relative to each parameter's typical size (A5.6.2).
unsafe fn difference_hessian(
    objective: &mut Objective<'_>,
    x: &[f64],
    value: f64,
    typical: &[f64],
    digits: i32,
) -> R<Vec<f64>> {
    let n = x.len();
    let eta = 10f64.powf(-(digits as f64) / 3.0);
    let mut point = x.to_vec();
    let mut steps = vec![0.0; n];
    let mut single = vec![0.0; n];
    for i in 0..n {
        let step = eta * x[i].max(typical[i]);
        point[i] = x[i] + step;
        steps[i] = point[i] - x[i];
        single[i] = objective.value(&point)?;
        point[i] = x[i];
    }
    let mut hessian = vec![0.0; n * n];
    for i in 0..n {
        point[i] = x[i] + 2.0 * steps[i];
        let double = objective.value(&point)?;
        hessian[i * n + i] = ((value - single[i]) + (double - single[i])) / (steps[i] * steps[i]);
        point[i] = x[i] + steps[i];
        for j in i + 1..n {
            point[j] = x[j] + steps[j];
            let both = objective.value(&point)?;
            let entry = ((value - single[i]) + (both - single[j])) / (steps[i] * steps[j]);
            hessian[i * n + j] = entry;
            hessian[j * n + i] = entry;
            point[j] = x[j];
        }
        point[i] = x[i];
    }
    Ok(hessian)
}

/// One evaluation of the objective: its value and the `gradient` and `hessian` attributes it
/// may carry.
impl crate::unwind::Rooted for Evaluation {}

struct Evaluation {
    point: Vec<f64>,
    value: f64,
    gradient: Option<Vec<f64>>,
    hessian: Option<Vec<f64>>,
}

/// The R objective, remembering its last evaluation so that a gradient or Hessian read from
/// the attributes does not call the function again at the same point.
struct Objective<'a> {
    call: &'a NativeCall<'a>,
    function: Sexp,
    n: usize,
    last: Option<Evaluation>,
}

impl Objective<'_> {
    unsafe fn evaluate(&mut self, x: &[f64]) -> R<&Evaluation> {
        if !self.last.as_ref().is_some_and(|last| last.point == x) {
            self.last = Some(self.compute(x)?);
        }
        match &self.last {
            Some(last) => Ok(last),
            None => Err(self.call.error("nlm lost its cached evaluation")),
        }
    }

    unsafe fn compute(&mut self, x: &[f64]) -> R<Evaluation> {
        crate::scope(|| {
            let argument = values::real_vector(x)?;
            let expression = values::call(self.function, &[argument])?;
            let result = eval::eval(expression, self.call.env)?;
            let value = match result.kind() {
                SexpType::Integer if length(result) == 1 => {
                    let entry = values::ints(result)[0];
                    if entry == NA_INTEGER {
                        self.call.warning("NA replaced by maximum positive value")?;
                        f64::MAX
                    } else {
                        entry as f64
                    }
                }
                SexpType::Real if length(result) == 1 => {
                    let entry = values::reals(result)[0];
                    if entry.is_finite() {
                        entry
                    } else {
                        self.call
                            .warning("NA/Inf replaced by maximum positive value")?;
                        f64::MAX
                    }
                }
                _ => return Err(self.call.error("invalid function value in 'nlm' optimizer")),
            };
            let attribute = |name: &str, size: usize| -> R<Option<Vec<f64>>> {
                let found = values::get_attr(result, values::install(name)?);
                if found == values::nil() {
                    return Ok(None);
                }
                let numbers = real_values(coerce_vector(found, SexpType::Real)?)?;
                if numbers.len() != size {
                    return Err(self
                        .call
                        .error(format!("the '{name}' attribute has the wrong length")));
                }
                Ok(Some(numbers))
            };
            Ok(Evaluation {
                point: x.to_vec(),
                value,
                gradient: attribute("gradient", self.n)?,
                hessian: attribute("hessian", self.n * self.n)?,
            })
        })
    }

    unsafe fn value(&mut self, x: &[f64]) -> R<f64> {
        Ok(self.evaluate(x)?.value)
    }

    unsafe fn gradient(&mut self, x: &[f64]) -> R<Vec<f64>> {
        let evaluation = self.evaluate(x)?;
        Ok(evaluation
            .gradient
            .clone()
            .unwrap_or_else(|| vec![0.0; x.len()]))
    }

    /// Writes the lower triangle of the analytic Hessian, which R stores by columns.
    unsafe fn hessian_into(&mut self, x: &[f64], matrix: &mut [f64]) -> R<()> {
        let n = self.n;
        let evaluation = self.evaluate(x)?;
        if let Some(hessian) = &evaluation.hessian {
            for i in 0..n {
                for j in 0..=i {
                    matrix[i * n + j] = hessian[j * n + i];
                }
            }
        }
        Ok(())
    }
}

unsafe fn print_iterate(
    iteration: i32,
    step: Option<&[f64]>,
    x: &[f64],
    f: f64,
    g: &[f64],
) -> R<()> {
    let options = PrintOptions::defaults(values::global_env());
    let print_numbers =
        |numbers: &[f64]| -> R<()> { print_vector(values::real_vector(numbers)?, &options) };
    print(&format!("iteration = {iteration}\n"));
    if let Some(step) = step {
        print("Step:\n");
        print_numbers(step)?;
    }
    print("Parameter:\n");
    print_numbers(x)?;
    print("Function Value\n");
    print_numbers(&[f])?;
    print("Gradient:\n");
    print_numbers(g)?;
    print("\n");
    Ok(())
}

fn print_stop_message(code: i32) {
    let text = match code {
        1 => "Relative gradient close to zero.\nCurrent iterate is probably solution.\n",
        2 => "Successive iterates within tolerance.\nCurrent iterate is probably solution.\n",
        3 => {
            "Last global step failed to locate a point lower than x.\nEither x is an approximate local minimum of the function,\nthe function is too non-linear for this algorithm,\nor steptol is too large.\n"
        }
        4 => "Iteration limit exceeded.  Algorithm failed.\n",
        5 => {
            "Maximum step size exceeded 5 consecutive times.\nEither the function is unbounded below,\nbecomes asymptotic to a finite value\nfrom above in some direction,\nor stepmx is too small.\n"
        }
        _ => return,
    };
    print(text);
    print("\n");
}

fn print(text: &str) {
    crate::write_console(text.as_bytes(), false);
}
