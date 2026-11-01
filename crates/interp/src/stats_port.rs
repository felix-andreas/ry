//! stats' PORT routines: `port_ivset`, `port_nlminb` behind `nlminb`, and `port_nlsb` behind
//! `nls(algorithm = "port")`.
//!
//! The R code talks to these routines through two work vectors, `iv` and `v`, which the
//! routines modify in place: `port_ivset` writes the defaults that the `control` list then
//! overrides, and the minimizers write their result back (`iv[1]` the convergence code,
//! `iv[6]` and `iv[30]` the evaluation counts, `iv[31]` the iterations, `v[10]` the final
//! objective). Only that interface follows PORT. The minimizer is ry's own: a trust-region
//! method in the scaled variables `D x`, whose steps solve `(H + mu D^2) s = -g` over the
//! variables not held at a bound and are then projected back into the box. `H` is the
//! analytic Hessian when `nlminb` has one, a BFGS approximation otherwise, and the
//! Gauss-Newton matrix `J'J` for least squares. The convergence codes keep PORT's meanings, so
//! a run converges or fails for the same reasons, but iterates and evaluation counts are not
//! PORT's.

use {
    crate::{
        builtin_libraries::NativeCall,
        coerce::coerce_vector,
        eval,
        format::c_general,
        stats::real_values,
        unwind::R,
        values::{self, length},
    },
    heap::object::{Sexp, SexpType},
};

/// `port_ivset(kind, iv, v)`: fills `iv` and `v` with the default settings. `kind` 1 is
/// least squares and 2 general minimization; both use the same defaults here.
pub unsafe fn port_ivset(call: &NativeCall<'_>) -> R<Sexp> {
    let iv = call.arg(1);
    let v = call.arg(2);
    if iv.kind() != SexpType::Integer || v.kind() != SexpType::Real {
        return Err(call.error("'iv' must be an integer vector and 'v' a double vector"));
    }
    let integers = values::ints_mut(iv);
    for (position, value) in DEFAULT_IV {
        if let Some(slot) = integers.get_mut(position - 1) {
            *slot = value;
        }
    }
    let reals = values::reals_mut(v);
    for (position, value) in default_v() {
        if let Some(slot) = reals.get_mut(position - 1) {
            *slot = value;
        }
    }
    Ok(values::nil())
}

/// `port_nlminb(objective, gradient, hessian, rho, lower, upper, d, iv, v)`. The three
/// function arguments are calls evaluated in `rho`, where `.par` holds the current point;
/// `lower` and `upper` are empty when the problem is unconstrained.
pub unsafe fn port_nlminb(call: &NativeCall<'_>) -> R<Sexp> {
    let rho = call.arg(3);
    if rho.kind() != SexpType::Environment {
        return Err(call.error("'rho' must be an environment"));
    }
    let par_symbol = values::install(".par")?;
    let par = eval::find_var_in_frame(par_symbol, rho)?;
    let start = real_values(coerce_vector(par, SexpType::Real)?)?;
    let n = start.len();
    let scale = scale_vector(call, call.arg(6), n)?;
    let (lower, upper) = bounds(call, call.arg(4), call.arg(5), n)?;
    let (iv, v) = work_vectors(call, call.arg(7), call.arg(8))?;
    if let Some(position) = out_of_range_control(values::reals(v)) {
        values::ints_mut(iv)[0] = position;
        return Ok(values::nil());
    }
    let controls = controls(values::ints(iv), values::reals(v));
    let gradient = call.arg(1);
    let hessian = call.arg(2);
    let mut model = Objective {
        call,
        rho,
        par_symbol,
        names: values::names(par),
        objective: call.arg(0),
        gradient: (gradient != values::nil()).then_some(gradient),
        hessian: (hessian != values::nil()).then_some(hessian),
        n,
    };
    let outcome = minimize(&mut model, &start, &scale, &lower, &upper, &controls)?;
    model.assign(&outcome.x)?;
    write_outcome(iv, v, &outcome);
    Ok(values::nil())
}

/// `port_nlsb(m, d, gradient, iv, v, lower, upper)`: least squares for an `nlsModel` `m`,
/// whose `setPars`, `resid` and `gradient` closures evaluate the model.
pub unsafe fn port_nlsb(call: &NativeCall<'_>) -> R<Sexp> {
    let model_list = call.arg(0);
    let closure = |name: &str| -> R<Sexp> {
        let found = values::list_get(model_list, name);
        if values::is_function(found) {
            Ok(found)
        } else {
            Err(call.error(format!("'m' has no '{name}' function")))
        }
    };
    let get_pars = closure("getPars")?;
    let mut model = LeastSquares {
        call,
        set_pars: closure("setPars")?,
        resid: closure("resid")?,
        gradient: closure("gradient")?,
        current: None,
    };
    let start = real_values(coerce_vector(
        eval::eval(values::call(get_pars, &[])?, call.env)?,
        SexpType::Real,
    )?)?;
    let n = start.len();
    let scale = scale_vector(call, call.arg(1), n)?;
    let (iv, v) = work_vectors(call, call.arg(3), call.arg(4))?;
    let (lower, upper) = bounds(call, call.arg(5), call.arg(6), n)?;
    if let Some(position) = out_of_range_control(values::reals(v)) {
        values::ints_mut(iv)[0] = position;
        return Ok(values::nil());
    }
    let controls = controls(values::ints(iv), values::reals(v));
    let outcome = minimize(&mut model, &start, &scale, &lower, &upper, &controls)?;
    model.set(&outcome.x)?;
    write_outcome(iv, v, &outcome);
    Ok(values::nil())
}

/// The 1-based position of the first tolerance in `v` outside the range PORT accepts, which
/// the minimizers report in `iv[1]` instead of running; `nlminb` names the `control` entry.
/// The ranges are PORT's: `rel.tol` between machine epsilon and 0.1, the step bounds positive
/// normal numbers, the other tolerances in [0, 1] (`abs.tol` any finite nonnegative number).
fn out_of_range_control(v: &[f64]) -> Option<i32> {
    let checks: [(usize, fn(f64) -> bool); 7] = [
        (31, |value| (0.0..f64::INFINITY).contains(&value)),
        (32, |value| (f64::EPSILON..=0.1).contains(&value)),
        (33, |value| (0.0..=1.0).contains(&value)),
        (34, |value| (0.0..=1.0).contains(&value)),
        (35, |value| {
            (f64::MIN_POSITIVE..f64::INFINITY).contains(&value)
        }),
        (36, |value| {
            (f64::MIN_POSITIVE..f64::INFINITY).contains(&value)
        }),
        (37, |value| (0.0..=1.0).contains(&value)),
    ];
    checks.into_iter().find_map(|(position, accepts)| {
        let value = *v.get(position - 1)?;
        (!accepts(value)).then_some(position as i32)
    })
}

/// The `iv` defaults, by 1-based position. Positions past 41 are PORT's storage offsets,
/// which no R code reads but which keep `iv` identical to GNU R's after `port_ivset`.
const DEFAULT_IV: [(usize, i32); 13] = [
    (1, 12),
    (17, 200),
    (18, 150),
    (20, 1),
    (24, 1),
    (25, 1),
    (42, 72),
    (44, 59),
    (45, 71),
    (49, 47),
    (50, 25),
    (51, 2),
    (58, 60),
];

/// The `v` defaults, by 1-based position: step-control constants, then the tolerances that
/// `control` exposes as `abs.tol` (31), `rel.tol` (32), `x.tol` (33), `xf.tol` (34),
/// `step.min` (35), `step.max` (36), `sing.tol` (37), `scale.init` (38) and `diff.g` (42).
fn default_v() -> [(usize, f64); 25] {
    let epsilon = f64::EPSILON;
    [
        (19, 0.1),
        (20, -0.1),
        (21, 0.1),
        (22, 0.5),
        (23, 2.0),
        (24, 0.1),
        (25, 4.0),
        (26, 0.1),
        (27, 1e-4),
        (28, 0.75),
        (29, 0.5),
        (30, 0.75),
        (31, 0.0),
        (32, 1e-10),
        (33, epsilon.sqrt()),
        (34, 100.0 * epsilon),
        (35, 1.0),
        (36, 1.0),
        (37, 1e-10),
        (38, -1.0),
        (39, 1e-6),
        (40, 1.0),
        (41, 0.6),
        (42, 1000.0 * epsilon),
        (43, 0.8),
    ]
}

struct Controls {
    evaluation_limit: i32,
    iteration_limit: i32,
    trace: i32,
    absolute_tolerance: f64,
    relative_tolerance: f64,
    x_tolerance: f64,
    false_tolerance: f64,
    initial_radius: f64,
}

unsafe fn controls(iv: &[i32], v: &[f64]) -> Controls {
    let integer = |position: usize| iv.get(position - 1).copied().unwrap_or(0);
    let real = |position: usize| v.get(position - 1).copied().unwrap_or(0.0);
    Controls {
        evaluation_limit: integer(17),
        iteration_limit: integer(18),
        trace: integer(19),
        absolute_tolerance: real(31),
        relative_tolerance: real(32),
        x_tolerance: real(33),
        false_tolerance: real(34),
        initial_radius: if real(36) > 0.0 { real(36) } else { 1.0 },
    }
}

unsafe fn work_vectors(call: &NativeCall<'_>, iv: Sexp, v: Sexp) -> R<(Sexp, Sexp)> {
    if iv.kind() != SexpType::Integer
        || v.kind() != SexpType::Real
        || length(iv) < 31
        || length(v) < 43
    {
        return Err(call.error("'iv' and 'v' must be the work vectors that 'port_ivset' filled"));
    }
    Ok((iv, v))
}

unsafe fn scale_vector(call: &NativeCall<'_>, d: Sexp, n: usize) -> R<Vec<f64>> {
    let scale = real_values(coerce_vector(d, SexpType::Real)?)?;
    if scale.len() != n
        || scale
            .iter()
            .any(|value| !(value.is_finite() && *value > 0.0))
    {
        return Err(call.error(format!(
            "'d' must be a nonzero positive vector of length {n}"
        )));
    }
    Ok(scale)
}

unsafe fn bounds(
    call: &NativeCall<'_>,
    lower: Sexp,
    upper: Sexp,
    n: usize,
) -> R<(Vec<f64>, Vec<f64>)> {
    let lower = real_values(coerce_vector(lower, SexpType::Real)?)?;
    let upper = real_values(coerce_vector(upper, SexpType::Real)?)?;
    match (lower.len(), upper.len()) {
        (0, 0) => Ok((vec![f64::NEG_INFINITY; n], vec![f64::INFINITY; n])),
        (low, high) if low == n && high == n => Ok((lower, upper)),
        _ => Err(call.error(format!(
            "'lower' and 'upper' must be numeric vectors of length {n}"
        ))),
    }
}

unsafe fn write_outcome(iv: Sexp, v: Sexp, outcome: &Outcome) {
    let integers = values::ints_mut(iv);
    integers[0] = outcome.code;
    integers[5] = outcome.evaluations;
    integers[29] = outcome.gradient_evaluations;
    integers[30] = outcome.iterations;
    let reals = values::reals_mut(v);
    reals[0] = outcome.gradient_norm;
    reals[9] = outcome.value;
}

/// What the minimizer needs from a problem: the objective, and its gradient with either an
/// exact Hessian or none (in which case the minimizer keeps a BFGS approximation).
trait Model {
    /// The objective at `x`, or `None` where it cannot be computed.
    unsafe fn value(&mut self, x: &[f64]) -> R<Option<f64>>;
    /// The gradient at `x`, where `value` was last evaluated, with the Hessian (column-major,
    /// `n * n`) when the problem supplies it.
    unsafe fn derivatives(&mut self, x: &[f64], value: f64) -> R<(Vec<f64>, Option<Vec<f64>>)>;
}

struct Outcome {
    x: Vec<f64>,
    value: f64,
    gradient_norm: f64,
    code: i32,
    iterations: i32,
    evaluations: i32,
    gradient_evaluations: i32,
}

/// PORT's convergence codes, which `port_msg` turns into messages.
mod code {
    pub const X_CONVERGENCE: i32 = 3;
    pub const RELATIVE: i32 = 4;
    pub const ABSOLUTE: i32 = 6;
    pub const FALSE_CONVERGENCE: i32 = 8;
    pub const EVALUATION_LIMIT: i32 = 9;
    pub const ITERATION_LIMIT: i32 = 10;
    pub const BAD_START: i32 = 63;
}

unsafe fn minimize(
    model: &mut dyn Model,
    start: &[f64],
    scale: &[f64],
    lower: &[f64],
    upper: &[f64],
    controls: &Controls,
) -> R<Outcome> {
    let n = start.len();
    let mut x: Vec<f64> = (0..n)
        .map(|i| start[i].clamp(lower[i], upper[i].max(lower[i])))
        .collect();
    let mut evaluations = 1;
    let mut gradient_evaluations = 0;
    let mut iterations = 0;
    let finish = |x: Vec<f64>,
                  value: f64,
                  gradient: &[f64],
                  code: i32,
                  iterations: i32,
                  evaluations: i32,
                  gradient_evaluations: i32| Outcome {
        gradient_norm: gradient
            .iter()
            .zip(scale)
            .map(|(g, d)| (g / d).powi(2))
            .sum::<f64>()
            .sqrt(),
        x,
        value,
        code,
        iterations,
        evaluations,
        gradient_evaluations,
    };
    let Some(mut value) = model.value(&x)? else {
        return Ok(finish(x, f64::NAN, &[], code::BAD_START, 0, evaluations, 0));
    };
    let (mut gradient, exact) = model.derivatives(&x, value)?;
    gradient_evaluations += 1;
    let quasi_newton = exact.is_none();
    let mut hessian = exact.unwrap_or_else(|| {
        let mut identity = vec![0.0; n * n];
        for i in 0..n {
            identity[i * n + i] = scale[i] * scale[i];
        }
        identity
    });
    let mut first_update = true;
    let mut radius = controls.initial_radius;
    if controls.trace > 0 {
        print_trace(0, value, &x);
    }
    if n == 0 {
        return Ok(finish(
            x,
            value,
            &gradient,
            code::RELATIVE,
            0,
            evaluations,
            gradient_evaluations,
        ));
    }
    loop {
        let free = free_variables(&x, &gradient, lower, upper);
        if value.abs() < controls.absolute_tolerance {
            return Ok(finish(
                x,
                value,
                &gradient,
                code::ABSOLUTE,
                iterations,
                evaluations,
                gradient_evaluations,
            ));
        }
        if iterations == 0
            && newton_reduction(&hessian, &gradient, &free, n)
                .is_some_and(|reduction| reduction <= controls.relative_tolerance * value.abs())
        {
            return Ok(finish(
                x,
                value,
                &gradient,
                code::RELATIVE,
                iterations,
                evaluations,
                gradient_evaluations,
            ));
        }
        if iterations >= controls.iteration_limit {
            return Ok(finish(
                x,
                value,
                &gradient,
                code::ITERATION_LIMIT,
                iterations,
                evaluations,
                gradient_evaluations,
            ));
        }
        iterations += 1;
        // Shrink the trust region until a step decreases the objective enough.
        let (step, newton_step, new_x, new_value) = loop {
            if evaluations >= controls.evaluation_limit {
                return Ok(finish(
                    x,
                    value,
                    &gradient,
                    code::EVALUATION_LIMIT,
                    iterations,
                    evaluations,
                    gradient_evaluations,
                ));
            }
            let (direction, newton_step) =
                trust_region_step(&hessian, &gradient, scale, &free, radius, n);
            let new_x: Vec<f64> = (0..n)
                .map(|i| (x[i] + direction[i]).clamp(lower[i], upper[i]))
                .collect();
            let step: Vec<f64> = (0..n).map(|i| new_x[i] - x[i]).collect();
            let length = scaled_norm(&step, scale);
            let predicted = -(dot(&gradient, &step) + 0.5 * quadratic_form(&hessian, &step, n));
            let new_value = if length > 0.0 && predicted > 0.0 {
                evaluations += 1;
                model.value(&new_x)?
            } else {
                None
            };
            let ratio = new_value.map_or(f64::NEG_INFINITY, |new_value| {
                (value - new_value) / predicted
            });
            if let (Some(new_value), true) = (new_value, ratio >= 1e-4) {
                if ratio > 0.75 && length >= 0.99 * radius {
                    radius *= 2.0;
                } else if ratio < 0.25 {
                    radius = 0.5 * length;
                }
                break (step, newton_step, new_x, new_value);
            }
            if relative_step(&x, &new_x, scale) <= controls.false_tolerance || length == 0.0 {
                // No step can make progress. That is convergence when the model predicts
                // nothing left to gain, and false convergence otherwise.
                let settled =
                    newton_reduction(&hessian, &gradient, &free, n).is_some_and(|reduction| {
                        reduction <= controls.relative_tolerance * value.abs()
                    });
                let code = if settled {
                    code::RELATIVE
                } else {
                    code::FALSE_CONVERGENCE
                };
                return Ok(finish(
                    x,
                    value,
                    &gradient,
                    code,
                    iterations,
                    evaluations,
                    gradient_evaluations,
                ));
            }
            radius = 0.25
                * if length > 0.0 {
                    length.min(radius)
                } else {
                    radius
                };
        };
        let reduction = value - new_value;
        let x_converged = newton_step && relative_step(&x, &new_x, scale) <= controls.x_tolerance;
        let old_gradient = std::mem::take(&mut gradient);
        let (new_gradient, exact) = model.derivatives(&new_x, new_value)?;
        gradient_evaluations += 1;
        gradient = new_gradient;
        if let Some(exact) = exact {
            hessian = exact;
        } else if quasi_newton {
            let change: Vec<f64> = (0..n).map(|i| gradient[i] - old_gradient[i]).collect();
            bfgs_update(&mut hessian, &step, &change, n, &mut first_update);
        }
        x = new_x;
        value = new_value;
        if controls.trace > 0 && iterations % controls.trace == 0 {
            print_trace(iterations, value, &x);
        }
        let relative_converged = reduction <= controls.relative_tolerance * value.abs()
            && newton_reduction(
                &hessian,
                &gradient,
                &free_variables(&x, &gradient, lower, upper),
                n,
            )
            .is_some_and(|predicted| predicted <= controls.relative_tolerance * value.abs());
        if relative_converged {
            return Ok(finish(
                x,
                value,
                &gradient,
                code::RELATIVE,
                iterations,
                evaluations,
                gradient_evaluations,
            ));
        }
        if x_converged {
            return Ok(finish(
                x,
                value,
                &gradient,
                code::X_CONVERGENCE,
                iterations,
                evaluations,
                gradient_evaluations,
            ));
        }
    }
}

/// The variables a step may move: those not at a bound that the gradient pushes against.
fn free_variables(x: &[f64], gradient: &[f64], lower: &[f64], upper: &[f64]) -> Vec<bool> {
    (0..x.len())
        .map(|i| {
            !((x[i] <= lower[i] && gradient[i] > 0.0) || (x[i] >= upper[i] && gradient[i] < 0.0))
        })
        .collect()
}

/// The decrease the quadratic model predicts for the full Newton step over the free
/// variables, `g' H^-1 g / 2`, or `None` when that part of `H` is not positive definite.
fn newton_reduction(hessian: &[f64], gradient: &[f64], free: &[bool], n: usize) -> Option<f64> {
    let indices: Vec<usize> = (0..n).filter(|&i| free[i]).collect();
    if indices.is_empty() {
        return Some(0.0);
    }
    let size = indices.len();
    let mut matrix: Vec<f64> = indices
        .iter()
        .flat_map(|&j| indices.iter().map(move |&i| hessian[j * n + i]))
        .collect();
    let rhs: Vec<f64> = indices.iter().map(|&i| gradient[i]).collect();
    if !cholesky(&mut matrix, size) {
        return None;
    }
    let half = forward_substitute(&matrix, size, &rhs);
    Some(0.5 * dot(&half, &half))
}

/// A step over the free variables that minimizes the quadratic model within `||D s|| <=
/// radius`: the Newton step when it fits, otherwise `(H + mu D^2) s = -g` with `mu` found by
/// Newton's method on `1/||D s|| = 1/radius`, after Moré and Sorensen. The flag says whether
/// the step is the Newton step.
fn trust_region_step(
    hessian: &[f64],
    gradient: &[f64],
    scale: &[f64],
    free: &[bool],
    radius: f64,
    n: usize,
) -> (Vec<f64>, bool) {
    let indices: Vec<usize> = (0..n).filter(|&i| free[i]).collect();
    let size = indices.len();
    let mut step = vec![0.0; n];
    if size == 0 {
        return (step, true);
    }
    // In the scaled variables t = D s the problem is (H~ + mu I) t = -g~ with H~ = D^-1 H D^-1.
    let scaled: Vec<f64> = indices
        .iter()
        .flat_map(|&j| {
            indices
                .iter()
                .map(move |&i| hessian[j * n + i] / (scale[i] * scale[j]))
        })
        .collect();
    let rhs: Vec<f64> = indices.iter().map(|&i| -gradient[i] / scale[i]).collect();
    let solve = |mu: f64| -> Option<(Vec<f64>, Vec<f64>)> {
        let mut matrix = scaled.clone();
        for k in 0..size {
            matrix[k * size + k] += mu;
        }
        if !cholesky(&mut matrix, size) {
            return None;
        }
        let half = forward_substitute(&matrix, size, &rhs);
        let solution = backward_substitute(&matrix, size, &half);
        Some((solution, matrix))
    };
    let largest = scaled
        .iter()
        .fold(0.0f64, |largest, value| largest.max(value.abs()));
    let gradient_norm = dot(&rhs, &rhs).sqrt();
    let mut mu = 0.0;
    // Every mu tried so far that gave too long a step (or an indefinite matrix) lies below the
    // answer, and every one that gave too short a step above it.
    let (mut below, mut above) = (0.0f64, f64::INFINITY);
    let mut newton = true;
    let mut solution = None;
    for _ in 0..60 {
        let next = match solve(mu) {
            None => {
                newton = false;
                below = below.max(mu);
                (2.0 * mu)
                    .max(1e-10 * largest.max(1.0))
                    .max(mu + gradient_norm / radius * 1e-3)
            }
            Some((candidate, factor)) => {
                let length = dot(&candidate, &candidate).sqrt();
                let q = forward_substitute(&factor, size, &candidate);
                solution = Some(candidate);
                if length <= radius * 1.1 && (mu == 0.0 || length >= radius * 0.9) {
                    break;
                }
                newton = false;
                if length > radius {
                    below = below.max(mu);
                } else {
                    above = above.min(mu);
                }
                let q_squared = dot(&q, &q).max(f64::MIN_POSITIVE);
                mu + (length * length / q_squared) * (length - radius) / radius
            }
        };
        let next = if next.is_finite() && next > below && next < above {
            next
        } else if above.is_finite() {
            0.5 * (below + above)
        } else {
            (2.0 * below).max(1e-10 * largest.max(1.0))
        };
        if (next - mu).abs() <= 1e-12 * mu.max(1.0) {
            break;
        }
        mu = next;
    }
    let solution = match solution {
        Some(solution) => solution,
        // H is so indefinite that no shift worked: fall back to steepest descent.
        None => rhs
            .iter()
            .map(|value| value * radius / gradient_norm.max(f64::MIN_POSITIVE))
            .collect(),
    };
    let length = dot(&solution, &solution).sqrt();
    let shrink = if length > radius {
        radius / length
    } else {
        1.0
    };
    for (k, &i) in indices.iter().enumerate() {
        step[i] = solution[k] * shrink / scale[i];
    }
    (step, newton && shrink == 1.0)
}

fn bfgs_update(hessian: &mut [f64], step: &[f64], change: &[f64], n: usize, first: &mut bool) {
    let curvature = dot(step, change);
    let step_norm = dot(step, step).sqrt();
    let change_norm = dot(change, change).sqrt();
    if curvature <= f64::EPSILON.sqrt() * step_norm * change_norm {
        return;
    }
    if *first {
        // Scale the initial matrix so that its size matches the observed curvature.
        let factor = dot(change, change) / curvature;
        let current = quadratic_form(hessian, step, n) / dot(step, step).max(f64::MIN_POSITIVE);
        if current > 0.0 {
            let ratio = factor / current;
            for entry in hessian.iter_mut() {
                *entry *= ratio;
            }
        }
        *first = false;
    }
    let product: Vec<f64> = (0..n)
        .map(|i| (0..n).map(|j| hessian[j * n + i] * step[j]).sum())
        .collect();
    let form = dot(step, &product);
    if form <= 0.0 {
        return;
    }
    for j in 0..n {
        for i in 0..n {
            hessian[j * n + i] +=
                change[i] * change[j] / curvature - product[i] * product[j] / form;
        }
    }
}

/// Factors a symmetric column-major matrix in place into its lower Cholesky factor `L`,
/// returning false when the matrix is not numerically positive definite.
fn cholesky(matrix: &mut [f64], size: usize) -> bool {
    for j in 0..size {
        let mut diagonal = matrix[j * size + j];
        for k in 0..j {
            diagonal -= matrix[k * size + j].powi(2);
        }
        if !(diagonal > 0.0) || !diagonal.is_finite() {
            return false;
        }
        let root = diagonal.sqrt();
        matrix[j * size + j] = root;
        for i in j + 1..size {
            let mut entry = matrix[j * size + i];
            for k in 0..j {
                entry -= matrix[k * size + i] * matrix[k * size + j];
            }
            matrix[j * size + i] = entry / root;
        }
    }
    true
}

/// Solves `L y = b` for the lower factor `cholesky` left in `factor`.
fn forward_substitute(factor: &[f64], size: usize, rhs: &[f64]) -> Vec<f64> {
    let mut solution = rhs.to_vec();
    for i in 0..size {
        for k in 0..i {
            solution[i] -= factor[k * size + i] * solution[k];
        }
        solution[i] /= factor[i * size + i];
    }
    solution
}

/// Solves `L' x = y` for the lower factor `cholesky` left in `factor`.
fn backward_substitute(factor: &[f64], size: usize, rhs: &[f64]) -> Vec<f64> {
    let mut solution = rhs.to_vec();
    for i in (0..size).rev() {
        for k in i + 1..size {
            solution[i] -= factor[i * size + k] * solution[k];
        }
        solution[i] /= factor[i * size + i];
    }
    solution
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn quadratic_form(matrix: &[f64], vector: &[f64], n: usize) -> f64 {
    (0..n)
        .map(|j| vector[j] * (0..n).map(|i| matrix[j * n + i] * vector[i]).sum::<f64>())
        .sum()
}

fn scaled_norm(step: &[f64], scale: &[f64]) -> f64 {
    step.iter()
        .zip(scale)
        .map(|(s, d)| (s * d).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// PORT's relative step size: the largest scaled change over the largest scaled magnitude.
fn relative_step(x: &[f64], new_x: &[f64], scale: &[f64]) -> f64 {
    let (mut change, mut size) = (0.0f64, 0.0f64);
    for i in 0..x.len() {
        change = change.max(scale[i] * (new_x[i] - x[i]).abs());
        size = size.max(scale[i] * (new_x[i].abs() + x[i].abs()));
    }
    if size > 0.0 { change / size } else { 0.0 }
}

/// One line of the `trace` output: the iteration, the objective and the parameters.
fn print_trace(iteration: i32, value: f64, x: &[f64]) {
    let mut line = format!("{iteration:>3}:{:>14}:", c_general(value, 8, true));
    for entry in x {
        line.push_str(&format!(" {:>8}", c_general(*entry, 6, true)));
    }
    line.push('\n');
    crate::write_console(line.as_bytes(), false);
}

/// `nlminb`'s objective: R calls evaluated in `rho` after `.par` is set to the point.
struct Objective<'a> {
    call: &'a NativeCall<'a>,
    rho: Sexp,
    par_symbol: Sexp,
    names: Sexp,
    objective: Sexp,
    gradient: Option<Sexp>,
    hessian: Option<Sexp>,
    n: usize,
}

impl Objective<'_> {
    unsafe fn assign(&self, x: &[f64]) -> R<()> {
        let par = values::real_vector(x)?;
        if self.names != values::nil() {
            values::set_attr(par, values::install("names")?, self.names)?;
        }
        eval::define(self.par_symbol, par, self.rho)
    }

    unsafe fn evaluate_numbers(
        &self,
        expression: Sexp,
        what: &str,
        size: Option<usize>,
    ) -> R<Vec<f64>> {
        let numbers = crate::scope(|| {
            let result = eval::eval(expression, self.rho)?;
            if !matches!(
                result.kind(),
                SexpType::Real | SexpType::Integer | SexpType::Logical
            ) {
                return Err(self
                    .call
                    .error(format!("{what} must return a numeric vector")));
            }
            Ok(ValueVector(real_values(coerce_vector(
                result,
                SexpType::Real,
            )?)?))
        })?
        .0;
        match size {
            Some(size) if numbers.len() != size => Err(self
                .call
                .error(format!("{what} must return a vector of length {size}"))),
            _ => Ok(numbers),
        }
    }
}

impl Model for Objective<'_> {
    unsafe fn value(&mut self, x: &[f64]) -> R<Option<f64>> {
        crate::scope(|| {
            self.assign(x)?;
            let result = eval::eval(self.objective, self.rho)?;
            if length(result) != 1 {
                return Err(self.call.error(format!(
                    "objective function in nlminb evaluates to length {} not 1",
                    length(result)
                )));
            }
            let value = values::as_real_scalar(result);
            Ok(value.is_finite().then_some(value))
        })
    }

    unsafe fn derivatives(&mut self, x: &[f64], value: f64) -> R<(Vec<f64>, Option<Vec<f64>>)> {
        let n = self.n;
        let Some(gradient_call) = self.gradient else {
            return Ok((self.difference_gradient(x, value)?, None));
        };
        self.assign(x)?;
        let gradient = self.evaluate_numbers(gradient_call, "gradient function", Some(n))?;
        let Some(hessian_call) = self.hessian else {
            return Ok((gradient, None));
        };
        let numbers = self.evaluate_numbers(hessian_call, "Hessian function", None)?;
        let mut hessian = vec![0.0; n * n];
        if numbers.len() == n * n {
            hessian.copy_from_slice(&numbers);
        } else if numbers.len() == n * (n + 1) / 2 {
            // The lower triangle, packed by rows.
            let mut position = 0;
            for i in 0..n {
                for j in 0..=i {
                    hessian[j * n + i] = numbers[position];
                    hessian[i * n + j] = numbers[position];
                    position += 1;
                }
            }
        } else {
            return Err(self.call.error(format!(
                "Hessian function must return a square numeric matrix of order {n}"
            )));
        }
        Ok((gradient, Some(hessian)))
    }
}

impl crate::unwind::Rooted for Option<f64> {}

impl Objective<'_> {
    /// Central differences with steps of `eps^(1/3) * max(|x|, 1)`, which keep the gradient
    /// accurate enough to drive the objective to its minimum to full precision, falling back to
    /// a one-sided difference where one side cannot be evaluated.
    unsafe fn difference_gradient(&mut self, x: &[f64], value: f64) -> R<Vec<f64>> {
        let mut gradient = vec![0.0; x.len()];
        let mut point = x.to_vec();
        for i in 0..x.len() {
            let step = f64::EPSILON.cbrt() * x[i].abs().max(1.0);
            point[i] = x[i] + step;
            let forward = self.value(&point)?;
            point[i] = x[i] - step;
            let backward = self.value(&point)?;
            gradient[i] = match (forward, backward) {
                (Some(forward), Some(backward)) => (forward - backward) / (2.0 * step),
                (Some(forward), None) => (forward - value) / step,
                (None, Some(backward)) => (value - backward) / step,
                (None, None) => 0.0,
            };
            point[i] = x[i];
        }
        self.assign(x)?;
        Ok(gradient)
    }
}

/// `nls`' model object for least squares: the objective is half the residual sum of squares,
/// its gradient `J' r` and its Hessian the Gauss-Newton matrix `J' J`, where `J`, the
/// Jacobian of the residuals, is the negated `m$gradient()`.
struct LeastSquares<'a> {
    call: &'a NativeCall<'a>,
    set_pars: Sexp,
    resid: Sexp,
    gradient: Sexp,
    current: Option<Vec<f64>>,
}

impl LeastSquares<'_> {
    unsafe fn set(&mut self, x: &[f64]) -> R<()> {
        if self.current.as_deref() != Some(x) {
            crate::scope(|| {
                let par = values::real_vector(x)?;
                eval::eval(values::call(self.set_pars, &[par])?, self.call.env)?;
                Ok(values::nil())
            })?;
            self.current = Some(x.to_vec());
        }
        Ok(())
    }

    unsafe fn residuals(&mut self) -> R<Vec<f64>> {
        crate::scope(|| {
            let result = eval::eval(values::call(self.resid, &[])?, self.call.env)?;
            Ok(ValueVector(real_values(coerce_vector(
                result,
                SexpType::Real,
            )?)?))
        })
        .map(|values| values.0)
    }
}

struct ValueVector(Vec<f64>);

impl crate::unwind::Rooted for ValueVector {}

impl Model for LeastSquares<'_> {
    unsafe fn value(&mut self, x: &[f64]) -> R<Option<f64>> {
        self.set(x)?;
        let residuals = self.residuals()?;
        let value = 0.5 * dot(&residuals, &residuals);
        Ok(value.is_finite().then_some(value))
    }

    unsafe fn derivatives(&mut self, x: &[f64], _value: f64) -> R<(Vec<f64>, Option<Vec<f64>>)> {
        self.set(x)?;
        let residuals = self.residuals()?;
        let rows = residuals.len();
        let n = x.len();
        let jacobian = crate::scope(|| {
            let result = eval::eval(values::call(self.gradient, &[])?, self.call.env)?;
            Ok(ValueVector(real_values(coerce_vector(
                result,
                SexpType::Real,
            )?)?))
        })?
        .0;
        if jacobian.len() != rows * n {
            return Err(self.call.error(format!(
                "the gradient of the model must be a {rows} by {n} matrix"
            )));
        }
        let mut gradient = vec![0.0; n];
        let mut hessian = vec![0.0; n * n];
        for j in 0..n {
            let column = &jacobian[j * rows..(j + 1) * rows];
            gradient[j] = -dot(column, &residuals);
            for k in 0..=j {
                let product = dot(column, &jacobian[k * rows..(k + 1) * rows]);
                hessian[j * n + k] = product;
                hessian[k * n + j] = product;
            }
        }
        Ok((gradient, Some(hessian)))
    }
}
