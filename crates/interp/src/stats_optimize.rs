//! stats' optimizers, root finder and quadrature: `optimize` (Brent's minimizer), `uniroot`
//! (Brent's root finder), `optim` (Nelder and Mead's simplex, BFGS, conjugate gradients,
//! L-BFGS-B and simulated annealing) with its Hessian, and `integrate` (QUADPACK's adaptive
//! Gauss-Kronrod quadrature with extrapolation).
//!
//! The algorithms follow their published descriptions: Brent, "Algorithms for Minimization
//! without Derivatives" (1973); Nash, "Compact Numerical Methods for Computers" (1990,
//! algorithms 19, 21 and 22); Byrd, Lu, Nocedal and Zhu, "A limited memory algorithm for bound
//! constrained optimization" (1995); Piessens, de Doncker-Kapenga, Überhuber and Kahaner,
//! "QUADPACK" (1983). Every objective is an R function called back from here, each call in its
//! own handle scope so that long runs do not accumulate handles.

use crate::{
    builtin_libraries::NativeCall,
    coerce::coerce_vector,
    eval,
    stats::{real_matrix, real_values},
    unwind::R,
    values::{self, length, nil, symbols},
};
use heap::object::{NA_INTEGER, Sexp, SexpType};

/// `do_fmin(f, lower, upper, tol)`: the minimum of `f` on `[lower, upper]` by Brent's method.
pub unsafe fn do_fmin(call: &NativeCall<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let lower = values::as_real_scalar(call.arg(1));
    let upper = values::as_real_scalar(call.arg(2));
    let tolerance = values::as_real_scalar(call.arg(3));
    if !values::is_function(function) {
        return Err(call.error("attempt to minimize non-function"));
    }
    if !lower.is_finite() {
        return Err(call.error("invalid 'xmin' value"));
    }
    if !upper.is_finite() {
        return Err(call.error("invalid 'xmax' value"));
    }
    if lower >= upper {
        return Err(call.error("'xmin' not less than 'xmax'"));
    }
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(call.error("invalid 'tol' value"));
    }
    let mut objective = |x: f64| -> R<f64> {
        let value = scalar_objective(call, function, x, "optimize")?;
        Ok(clamp_infinite(call, value)?)
    };
    let minimum = brent_fmin(lower, upper, tolerance, &mut objective)?;
    values::scalar_real(minimum)
}

/// `zeroin2(f, lower, upper, f.lower, f.upper, tol, maxiter)`: a root of `f` in the bracket,
/// by Brent's method, as `c(root, iterations, estimated precision)`.
pub unsafe fn zeroin2(call: &NativeCall<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let lower = values::as_real_scalar(call.arg(1));
    let upper = values::as_real_scalar(call.arg(2));
    let f_lower = values::as_real_scalar(call.arg(3));
    let f_upper = values::as_real_scalar(call.arg(4));
    let tolerance = values::as_real_scalar(call.arg(5));
    let max_iterations = values::as_integer_scalar(call.arg(6));
    if !values::is_function(function) {
        return Err(call.error("attempt to minimize non-function"));
    }
    if !lower.is_finite() {
        return Err(call.error("invalid 'xmin' value"));
    }
    if !upper.is_finite() {
        return Err(call.error("invalid 'xmax' value"));
    }
    if lower >= upper {
        return Err(call.error("'xmin' not less than 'xmax'"));
    }
    if f_lower.is_nan() {
        return Err(call.error("NA value for 'f.lower' is not allowed"));
    }
    if f_upper.is_nan() {
        return Err(call.error("NA value for 'f.upper' is not allowed"));
    }
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(call.error("invalid 'tol' value"));
    }
    if max_iterations == NA_INTEGER || max_iterations <= 0 {
        return Err(call.error("'maxiter' must be positive"));
    }
    let mut objective = |x: f64| -> R<f64> {
        let value = scalar_objective(call, function, x, "zeroin")?;
        if value.is_finite() {
            return Ok(value);
        }
        if value == f64::NEG_INFINITY {
            call.warning("-Inf replaced by maximally negative value")?;
            Ok(-f64::MAX)
        } else if value == f64::INFINITY {
            call.warning("Inf replaced by maximum positive value")?;
            Ok(f64::MAX)
        } else {
            call.warning("NA replaced by maximum positive value")?;
            Ok(f64::MAX)
        }
    };
    let (root, iterations, precision) = brent_zeroin(
        lower,
        upper,
        f_lower,
        f_upper,
        tolerance,
        max_iterations,
        &mut objective,
    )?;
    values::real_vector(&[root, iterations as f64, precision])
}

/// Calls the R function `function` at the scalar `x` and returns its numeric value.
unsafe fn scalar_objective(call: &NativeCall<'_>, function: Sexp, x: f64, what: &str) -> R<f64> {
    crate::scope(|| {
        let argument = values::scalar_real(x)?;
        let expression = values::call(function, &[argument])?;
        let value = eval::eval(expression, call.env)?;
        match value.kind() {
            SexpType::Integer | SexpType::Logical if length(value) == 1 => {
                let entry = values::ints(value)[0];
                Ok(if entry == NA_INTEGER {
                    f64::NAN
                } else {
                    entry as f64
                })
            }
            SexpType::Real if length(value) == 1 => Ok(values::reals(value)[0]),
            _ => Err(call.error(format!("invalid function value in '{what}'"))),
        }
    })
}

/// `optimize`'s treatment of a non-finite value: the largest finite value of the same sign.
unsafe fn clamp_infinite(call: &NativeCall<'_>, value: f64) -> R<f64> {
    if value.is_finite() {
        return Ok(value);
    }
    if value == f64::NEG_INFINITY {
        call.warning("-Inf replaced by maximally negative value")?;
        Ok(-f64::MAX)
    } else {
        call.warning("NA/Inf replaced by maximum positive value")?;
        Ok(f64::MAX)
    }
}

/// Brent's minimizer: golden section search accelerated by successive parabolic
/// interpolation, never evaluating closer than the tolerance to a previous point.
fn brent_fmin(lower: f64, upper: f64, tolerance: f64, f: &mut dyn FnMut(f64) -> R<f64>) -> R<f64> {
    let (minimum, _) = brent_minimize(lower, upper, tolerance, f64::EPSILON.sqrt(), None, f)?;
    Ok(minimum)
}

/// [`brent_fmin`] with a relative tolerance `epsilon` and an optional limit on the number of
/// evaluations after the first. Returns the minimum and the number of those evaluations.
pub fn brent_minimize(
    lower: f64,
    upper: f64,
    tolerance: f64,
    epsilon: f64,
    max_iterations: Option<i32>,
    f: &mut dyn FnMut(f64) -> R<f64>,
) -> R<(f64, i32)> {
    let golden = (3.0 - 5f64.sqrt()) * 0.5;
    let mut iterations = 0;
    let (mut a, mut b) = (lower, upper);
    let mut x = a + golden * (b - a);
    let (mut w, mut v) = (x, x);
    let mut fx = f(x)?;
    let (mut fw, mut fv) = (fx, fx);
    let (mut step, mut previous_step) = (0.0_f64, 0.0_f64);
    let third = tolerance / 3.0;
    loop {
        let middle = (a + b) * 0.5;
        let tol1 = epsilon * x.abs() + third;
        let tol2 = tol1 * 2.0;
        if (x - middle).abs() <= tol2 - (b - a) * 0.5
            || max_iterations.is_some_and(|limit| iterations >= limit)
        {
            return Ok((x, iterations));
        }
        iterations += 1;
        let (mut p, mut q, mut r) = (0.0, 0.0, 0.0);
        if previous_step.abs() > tol1 {
            r = (x - w) * (fx - fv);
            q = (x - v) * (fx - fw);
            p = (x - v) * q - (x - w) * r;
            q = (q - r) * 2.0;
            if q > 0.0 {
                p = -p;
            } else {
                q = -q;
            }
            r = previous_step;
            previous_step = step;
        }
        if p.abs() >= (q * 0.5 * r).abs() || p <= q * (a - x) || p >= q * (b - x) {
            previous_step = if x < middle { b - x } else { a - x };
            step = golden * previous_step;
        } else {
            step = p / q;
            let u = x + step;
            if u - a < tol2 || b - u < tol2 {
                step = if x >= middle { -tol1 } else { tol1 };
            }
        }
        let u = if step.abs() >= tol1 {
            x + step
        } else if step > 0.0 {
            x + tol1
        } else {
            x - tol1
        };
        let fu = f(u)?;
        if fu <= fx {
            if u < x {
                b = x;
            } else {
                a = x;
            }
            v = w;
            w = x;
            x = u;
            fv = fw;
            fw = fx;
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
}

/// Brent's root finder: bisection safeguarding inverse quadratic (or secant) interpolation.
/// Returns the root, the iterations used and the final bracket width; a failure to converge
/// returns -1 for both.
fn brent_zeroin(
    lower: f64,
    upper: f64,
    f_lower: f64,
    f_upper: f64,
    tolerance: f64,
    max_iterations: i32,
    f: &mut dyn FnMut(f64) -> R<f64>,
) -> R<(f64, i32, f64)> {
    let (mut a, mut b) = (lower, upper);
    let (mut fa, mut fb) = (f_lower, f_upper);
    let (mut c, mut fc) = (a, fa);
    if fa == 0.0 {
        return Ok((a, 0, 0.0));
    }
    if fb == 0.0 {
        return Ok((b, 0, 0.0));
    }
    let mut remaining = max_iterations + 1;
    while remaining > 0 {
        remaining -= 1;
        let previous_step = b - a;
        if fc.abs() < fb.abs() {
            a = b;
            b = c;
            c = a;
            fa = fb;
            fb = fc;
            fc = fa;
        }
        let tolerance_now = 2.0 * f64::EPSILON * b.abs() + tolerance / 2.0;
        let mut step = (c - b) / 2.0;
        if step.abs() <= tolerance_now || fb == 0.0 {
            return Ok((b, max_iterations - remaining, (c - b).abs()));
        }
        if previous_step.abs() >= tolerance_now && fa.abs() > fb.abs() {
            let to_c = c - b;
            let (mut p, mut q);
            if a == c {
                let t1 = fb / fa;
                p = to_c * t1;
                q = 1.0 - t1;
            } else {
                let qa = fa / fc;
                let t1 = fb / fc;
                let t2 = fb / fa;
                p = t2 * (to_c * qa * (qa - t1) - (b - a) * (t1 - 1.0));
                q = (qa - 1.0) * (t1 - 1.0) * (t2 - 1.0);
            }
            if p > 0.0 {
                q = -q;
            } else {
                p = -p;
            }
            if p < 0.75 * to_c * q - (tolerance_now * q).abs() / 2.0
                && p < (previous_step * q / 2.0).abs()
            {
                step = p / q;
            }
        }
        if step.abs() < tolerance_now {
            step = if step > 0.0 {
                tolerance_now
            } else {
                -tolerance_now
            };
        }
        a = b;
        fa = fb;
        b += step;
        fb = f(b)?;
        if (fb > 0.0 && fc > 0.0) || (fb < 0.0 && fc < 0.0) {
            c = a;
            fc = fa;
        }
    }
    Ok((b, -1, -1.0))
}

// optim.

/// What every `optim` method shares: the R objective and gradient, and the scaling of
/// parameters and values the control list asks for.
struct Problem<'a> {
    call: &'a NativeCall<'a>,
    function: Sexp,
    gradient: Sexp,
    names: Sexp,
    fn_scale: f64,
    par_scale: Vec<f64>,
    steps: Vec<f64>,
    /// Bounds in scaled units, when the method uses them for finite differences.
    bounds: Option<(Vec<f64>, Vec<f64>)>,
    function_count: usize,
    gradient_count: usize,
}

impl Problem<'_> {
    /// The scaled objective at the scaled parameters `p`.
    unsafe fn value(&mut self, p: &[f64]) -> R<f64> {
        self.function_count += 1;
        let parameters: Vec<f64> = p
            .iter()
            .zip(&self.par_scale)
            .map(|(value, scale)| value * scale)
            .collect();
        let raw = self.evaluate_function(&parameters)?;
        Ok(raw / self.fn_scale)
    }

    unsafe fn evaluate_function(&self, parameters: &[f64]) -> R<f64> {
        let call = self.call;
        let (function, names) = (self.function, self.names);
        crate::scope(|| {
            let argument = values::real_vector(parameters)?;
            if names != nil() {
                values::set_attr(argument, symbols().names, names)?;
            }
            let expression = values::call(function, &[argument])?;
            let value = eval::eval(expression, call.env)?;
            if length(value) != 1 {
                return Err(call.error(format!(
                    "objective function in optim evaluates to length {} not 1",
                    length(value)
                )));
            }
            let value = coerce_vector(value, SexpType::Real)?;
            Ok(values::reals(value)[0])
        })
    }

    /// The gradient of the scaled objective at the scaled parameters `p`: the R gradient
    /// function's, or central differences of the objective (one-sided at a bound).
    unsafe fn gradient(&mut self, p: &[f64]) -> R<Vec<f64>> {
        let n = p.len();
        if self.gradient != nil() {
            self.gradient_count += 1;
            let parameters: Vec<f64> = p
                .iter()
                .zip(&self.par_scale)
                .map(|(value, scale)| value * scale)
                .collect();
            let call = self.call;
            let (gradient, names) = (self.gradient, self.names);
            let raw = crate::scope(|| {
                let argument = values::real_vector(&parameters)?;
                if names != nil() {
                    values::set_attr(argument, symbols().names, names)?;
                }
                let expression = values::call(gradient, &[argument])?;
                let value = eval::eval(expression, call.env)?;
                if length(value) != n {
                    return Err(call.error(format!(
                        "gradient in optim evaluated to length {} not {n}",
                        length(value)
                    )));
                }
                let value = coerce_vector(value, SexpType::Real)?;
                Ok(ValueList(values::reals(value).to_vec()))
            })?
            .0;
            return Ok(raw
                .iter()
                .zip(&self.par_scale)
                .map(|(value, scale)| value * scale / self.fn_scale)
                .collect());
        }
        let mut parameters: Vec<f64> = p
            .iter()
            .zip(&self.par_scale)
            .map(|(value, scale)| value * scale)
            .collect();
        let mut result = vec![0.0; n];
        for i in 0..n {
            let step = self.steps[i];
            let (mut forward, mut backward) = (p[i] + step, p[i] - step);
            let (mut forward_step, mut backward_step) = (step, step);
            if let Some((lower, upper)) = &self.bounds {
                if forward > upper[i] {
                    forward = upper[i];
                    forward_step = forward - p[i];
                }
                if backward < lower[i] {
                    backward = lower[i];
                    backward_step = p[i] - backward;
                }
            }
            parameters[i] = forward * self.par_scale[i];
            let high = self.evaluate_function(&parameters)? / self.fn_scale;
            if !high.is_finite() {
                return Err(self
                    .call
                    .error(format!("non-finite finite-difference value [{}]", i + 1)));
            }
            parameters[i] = backward * self.par_scale[i];
            let low = self.evaluate_function(&parameters)? / self.fn_scale;
            if !low.is_finite() {
                return Err(self
                    .call
                    .error(format!("non-finite finite-difference value [{}]", i + 1)));
            }
            result[i] = if self.bounds.is_some() {
                (high - low) / (forward_step + backward_step)
            } else {
                (high - low) / (2.0 * step)
            };
            if !result[i].is_finite() {
                return Err(self
                    .call
                    .error(format!("non-finite finite-difference value [{}]", i + 1)));
            }
            parameters[i] = p[i] * self.par_scale[i];
        }
        Ok(result)
    }
}

/// A plain vector of numbers, which holds no handles.
struct ValueList(Vec<f64>);

impl crate::unwind::Rooted for ValueList {}

/// The control list's entries `optim` reads.
struct Control {
    trace: i32,
    max_iterations: i32,
    abs_tol: f64,
    rel_tol: f64,
    report: i32,
}

unsafe fn control_real(control: Sexp, name: &str) -> f64 {
    values::as_real_scalar(values::list_get(control, name))
}

unsafe fn control_integer(control: Sexp, name: &str) -> i32 {
    values::as_integer_scalar(values::list_get(control, name))
}

unsafe fn problem<'a>(
    call: &'a NativeCall<'a>,
    par: Sexp,
    function: Sexp,
    gradient: Sexp,
    control: Sexp,
) -> R<Problem<'a>> {
    if !values::is_function(function) {
        return Err(call.error("'fn' is not a function"));
    }
    if gradient != nil() && !values::is_function(gradient) {
        return Err(call.error("'gr' is not a function"));
    }
    let n = length(par);
    let par_scale = real_values(values::list_get(control, "parscale"))?;
    if par_scale.len() != n {
        return Err(call.error("'parscale' is of the wrong length"));
    }
    let steps = real_values(values::list_get(control, "ndeps"))?;
    if gradient == nil() && steps.len() != n {
        return Err(call.error(format!("'ndeps' is of length {} not {n}", steps.len())));
    }
    Ok(Problem {
        call,
        function,
        gradient,
        names: values::names(par),
        fn_scale: control_real(control, "fnscale"),
        par_scale,
        steps,
        bounds: None,
        function_count: 0,
        gradient_count: 0,
    })
}

/// `optim(par, fn, gr, method, control, lower, upper)`.
pub unsafe fn optim(call: &NativeCall<'_>) -> R<Sexp> {
    let par = coerce_vector(call.arg(0), SexpType::Real)?;
    let method = values::first_string(call.arg(3))
        .map(|name| name.into_owned())
        .unwrap_or_default();
    let control_list = call.arg(4);
    let lower_argument = call.arg(5);
    let upper_argument = call.arg(6);
    let mut problem = problem(call, par, call.arg(1), call.arg(2), control_list)?;
    let control = Control {
        trace: control_integer(control_list, "trace"),
        max_iterations: control_integer(control_list, "maxit"),
        abs_tol: control_real(control_list, "abstol"),
        rel_tol: control_real(control_list, "reltol"),
        report: control_integer(control_list, "REPORT"),
    };
    let start: Vec<f64> = values::reals(par)
        .iter()
        .zip(&problem.par_scale)
        .map(|(value, scale)| value / scale)
        .collect();
    let (solution, value, function_count, gradient_count, fail, message) = match method.as_str() {
        "Nelder-Mead" => {
            let alpha = control_real(control_list, "alpha");
            let beta = control_real(control_list, "beta");
            let gamma = control_real(control_list, "gamma");
            let (solution, value, count, fail) =
                nelder_mead(&mut problem, &start, &control, alpha, beta, gamma)?;
            (solution, value, count as i32, NA_INTEGER, fail, nil())
        }
        "BFGS" => {
            let (solution, value, counts, fail) = variable_metric(&mut problem, &start, &control)?;
            (
                solution,
                value,
                counts.0 as i32,
                counts.1 as i32,
                fail,
                nil(),
            )
        }
        "CG" => {
            let kind = control_integer(control_list, "type");
            let (solution, value, counts, fail) =
                conjugate_gradients(&mut problem, &start, &control, kind)?;
            (
                solution,
                value,
                counts.0 as i32,
                counts.1 as i32,
                fail,
                nil(),
            )
        }
        "L-BFGS-B" => {
            let lower: Vec<f64> = real_values(lower_argument)?
                .iter()
                .zip(&problem.par_scale)
                .map(|(bound, scale)| bound / scale)
                .collect();
            let upper: Vec<f64> = real_values(upper_argument)?
                .iter()
                .zip(&problem.par_scale)
                .map(|(bound, scale)| bound / scale)
                .collect();
            let memory = control_integer(control_list, "lmm");
            let factor = control_real(control_list, "factr");
            let projected_tolerance = control_real(control_list, "pgtol");
            problem.bounds = Some((lower.clone(), upper.clone()));
            let settings = crate::stats_lbfgsb::Settings {
                memory: memory.max(1) as usize,
                factor,
                projected_tolerance,
                max_iterations: control.max_iterations,
                trace: control.trace,
                report: control.report.max(1),
            };
            let outcome = crate::stats_lbfgsb::minimize(
                &mut problem_adapter(&mut problem),
                &start,
                &lower,
                &upper,
                &settings,
            )?;
            let message = values::scalar_string(&outcome.message)?;
            (
                outcome.solution,
                outcome.value,
                outcome.function_count as i32,
                outcome.gradient_count as i32,
                outcome.fail,
                message,
            )
        }
        "SANN" => {
            let max_temperature_steps = control_integer(control_list, "tmax");
            let temperature = control_real(control_list, "temp");
            if max_temperature_steps < 1 {
                return Err(call.error("'tmax' is not a positive integer"));
            }
            let (solution, value) = simulated_annealing(
                &mut problem,
                &start,
                &control,
                max_temperature_steps,
                temperature,
            )?;
            let count = if start.is_empty() {
                1
            } else {
                control.max_iterations
            };
            (solution, value, count, NA_INTEGER, 0, nil())
        }
        _ => return Err(call.error("unknown 'method'")),
    };
    let parameters: Vec<f64> = solution
        .iter()
        .zip(&problem.par_scale)
        .map(|(value, scale)| value * scale)
        .collect();
    let parameters = values::real_vector(&parameters)?;
    if problem.names != nil() {
        values::set_attr(parameters, symbols().names, problem.names)?;
    }
    let counts = values::integer_vector(&[function_count, gradient_count])?;
    values::set_attr(
        counts,
        symbols().names,
        values::string_vector(&["function", "gradient"])?,
    )?;
    values::named_list(&[
        ("par", parameters),
        ("value", values::scalar_real(value * problem.fn_scale)?),
        ("counts", counts),
        ("convergence", values::scalar_integer(fail)?),
        ("message", message),
    ])
}

/// The objective and gradient as the L-BFGS-B driver sees them.
pub(crate) trait Objective {
    unsafe fn value(&mut self, x: &[f64]) -> R<f64>;
    unsafe fn gradient(&mut self, x: &[f64]) -> R<Vec<f64>>;
}

struct ProblemObjective<'p, 'a> {
    problem: &'p mut Problem<'a>,
}

impl Objective for ProblemObjective<'_, '_> {
    unsafe fn value(&mut self, x: &[f64]) -> R<f64> {
        self.problem.value(x)
    }

    unsafe fn gradient(&mut self, x: &[f64]) -> R<Vec<f64>> {
        self.problem.gradient(x)
    }
}

fn problem_adapter<'p, 'a>(problem: &'p mut Problem<'a>) -> ProblemObjective<'p, 'a> {
    ProblemObjective { problem }
}

/// `optimhess(par, fn, gr, control)`: the Hessian at `par`, by central differences of the
/// gradient, symmetrized.
pub unsafe fn optimhess(call: &NativeCall<'_>) -> R<Sexp> {
    let par = coerce_vector(call.arg(0), SexpType::Real)?;
    let control = call.arg(3);
    let mut problem = problem(call, par, call.arg(1), call.arg(2), control)?;
    let n = length(par);
    let mut point: Vec<f64> = values::reals(par)
        .iter()
        .zip(&problem.par_scale)
        .map(|(value, scale)| value / scale)
        .collect();
    let mut hessian = vec![0.0; n * n];
    for i in 0..n {
        let step = problem.steps[i] / problem.par_scale[i];
        point[i] += step;
        let forward = problem.gradient(&point)?;
        point[i] -= 2.0 * step;
        let backward = problem.gradient(&point)?;
        for j in 0..n {
            hessian[i * n + j] = problem.fn_scale * (forward[j] - backward[j])
                / (2.0 * step * problem.par_scale[i] * problem.par_scale[j]);
        }
        point[i] += step;
    }
    for i in 0..n {
        for j in 0..i {
            let mean = 0.5 * (hessian[i * n + j] + hessian[j * n + i]);
            hessian[i * n + j] = mean;
            hessian[j * n + i] = mean;
        }
    }
    let matrix = real_matrix(&hessian, n, n)?;
    if problem.names != nil() {
        let dimnames = values::list_vector(&[problem.names, problem.names])?;
        values::set_attr(matrix, symbols().dimnames, dimnames)?;
    }
    Ok(matrix)
}

fn print(text: &str) {
    crate::write_console(text.as_bytes(), false);
}

/// The largest value standing in for a non-finite objective in the direct search methods.
const BIG: f64 = 1.0e35;

/// Nelder and Mead's simplex search as Nash gives it: reflection, extension, contraction
/// towards the centroid, and shrinking towards the best vertex, until the spread of the
/// function values over the simplex falls below the tolerance.
unsafe fn nelder_mead(
    problem: &mut Problem<'_>,
    start: &[f64],
    control: &Control,
    alpha: f64,
    beta: f64,
    gamma: f64,
) -> R<(Vec<f64>, f64, usize, i32)> {
    let n = start.len();
    let trace = control.trace > 0;
    if control.max_iterations <= 0 {
        let value = problem.value(start)?;
        return Ok((start.to_vec(), value, 0, 0));
    }
    if trace {
        print("  Nelder-Mead direct search function minimizer\n");
    }
    let value = problem.value(start)?;
    if !value.is_finite() {
        return Err(problem
            .call
            .error("function cannot be evaluated at initial parameters"));
    }
    if trace {
        print(&format!(
            "function value for initial parameters = {value:.6}\n"
        ));
    }
    let mut count = 1usize;
    let tolerance = control.rel_tol * (value.abs() + control.rel_tol);
    if trace {
        print(&format!(
            "  Scaled convergence tolerance is {}\n",
            crate::format::c_general(tolerance, 6, false)
        ));
    }
    let vertices = n + 1;
    // Column `j` is vertex `j` (its coordinates, then its function value); the extra column is
    // the centroid.
    let mut simplex = vec![vec![0.0; n + 1]; vertices + 1];
    simplex[0][..n].copy_from_slice(start);
    simplex[0][n] = value;
    let mut best = 0usize;
    let mut step = start
        .iter()
        .fold(0.0_f64, |largest, value| largest.max(0.1 * value.abs()));
    if step == 0.0 {
        step = 0.1;
    }
    if trace {
        print(&format!("Stepsize computed as {step:.6}\n"));
    }
    let mut size = 0.0;
    for j in 1..vertices {
        simplex[j][..n].copy_from_slice(start);
        let mut trial = step;
        while simplex[j][j - 1] == start[j - 1] {
            simplex[j][j - 1] = start[j - 1] + trial;
            trial *= 10.0;
        }
        size += trial;
    }
    let mut old_size = size;
    let mut evaluate_vertices = true;
    let mut fail = 0;
    let mut action = "BUILD          ";
    let mut point = vec![0.0; n];
    loop {
        if evaluate_vertices {
            for j in 0..vertices {
                if j != best {
                    point.copy_from_slice(&simplex[j][..n]);
                    let mut f = problem.value(&point)?;
                    if !f.is_finite() {
                        f = BIG;
                    }
                    count += 1;
                    simplex[j][n] = f;
                }
            }
            evaluate_vertices = false;
        }
        let mut lowest = simplex[best][n];
        let mut highest = lowest;
        let mut worst = best;
        for j in 0..vertices {
            if j != best {
                let f = simplex[j][n];
                if f < lowest {
                    best = j;
                    lowest = f;
                }
                if f > highest {
                    worst = j;
                    highest = f;
                }
            }
        }
        if highest <= lowest + tolerance || lowest <= control.abs_tol {
            break;
        }
        if trace {
            print(&format!("{action}{count:5} {highest:.6} {lowest:.6}\n"));
        }
        let centroid = vertices;
        for i in 0..n {
            let mut total = -simplex[worst][i];
            for vertex in simplex.iter().take(vertices) {
                total += vertex[i];
            }
            simplex[centroid][i] = total / n as f64;
        }
        for i in 0..n {
            point[i] = (1.0 + alpha) * simplex[centroid][i] - alpha * simplex[worst][i];
        }
        let mut f = problem.value(&point)?;
        if !f.is_finite() {
            f = BIG;
        }
        count += 1;
        action = "REFLECTION     ";
        let reflected = f;
        if reflected < lowest {
            simplex[centroid][n] = f;
            for i in 0..n {
                let extended = gamma * point[i] + (1.0 - gamma) * simplex[centroid][i];
                simplex[centroid][i] = point[i];
                point[i] = extended;
            }
            let mut f = problem.value(&point)?;
            if !f.is_finite() {
                f = BIG;
            }
            count += 1;
            if f < reflected {
                simplex[worst][..n].copy_from_slice(&point);
                simplex[worst][n] = f;
                action = "EXTENSION      ";
            } else {
                let reflection: Vec<f64> = simplex[centroid][..n].to_vec();
                simplex[worst][..n].copy_from_slice(&reflection);
                simplex[worst][n] = reflected;
            }
        } else {
            action = "HI-REDUCTION   ";
            if reflected < highest {
                simplex[worst][..n].copy_from_slice(&point);
                simplex[worst][n] = reflected;
                action = "LO-REDUCTION   ";
            }
            for i in 0..n {
                point[i] = (1.0 - beta) * simplex[worst][i] + beta * simplex[centroid][i];
            }
            let mut f = problem.value(&point)?;
            if !f.is_finite() {
                f = BIG;
            }
            count += 1;
            if f < simplex[worst][n] {
                simplex[worst][..n].copy_from_slice(&point);
                simplex[worst][n] = f;
            } else if reflected >= highest {
                action = "SHRINK         ";
                evaluate_vertices = true;
                size = 0.0;
                for j in 0..vertices {
                    if j != best {
                        for i in 0..n {
                            simplex[j][i] =
                                beta * (simplex[j][i] - simplex[best][i]) + simplex[best][i];
                            size += (simplex[j][i] - simplex[best][i]).abs();
                        }
                    }
                }
                if size < old_size {
                    old_size = size;
                } else {
                    if trace {
                        print("Polytope size measure not decreased in shrink\n");
                    }
                    fail = 10;
                    break;
                }
            }
        }
        if count > control.max_iterations as usize {
            break;
        }
    }
    if trace {
        print(&format!(
            "Exiting from Nelder Mead minimizer\n    {count} function evaluations used\n"
        ));
    }
    if count > control.max_iterations as usize {
        fail = 1;
    }
    Ok((simplex[best][..n].to_vec(), simplex[best][n], count, fail))
}

const STEP_REDUCTION: f64 = 0.2;
const ACCEPTANCE: f64 = 0.0001;
const RELATIVE_TEST: f64 = 10.0;

/// Nash's variable metric method: BFGS updates of an inverse Hessian approximation, with a
/// backtracking line search on an Armijo condition and a reset to steepest descent whenever
/// the update fails.
unsafe fn variable_metric(
    problem: &mut Problem<'_>,
    start: &[f64],
    control: &Control,
) -> R<(Vec<f64>, f64, (usize, usize), i32)> {
    let n = start.len();
    let trace = control.trace > 0;
    let report = control.report.max(1) as usize;
    let mut b = start.to_vec();
    if control.max_iterations <= 0 {
        let value = problem.value(&b)?;
        return Ok((b, value, (0, 0), 0));
    }
    if report == 0 {
        return Err(problem.call.error("REPORT must be > 0 (method = \"BFGS\")"));
    }
    let mut f = problem.value(&b)?;
    if !f.is_finite() {
        return Err(problem.call.error("initial value in 'vmmin' is not finite"));
    }
    if trace {
        print(&format!("initial  value {f:.6} \n"));
    }
    let mut minimum = f;
    let mut function_count = 1usize;
    let mut gradient_count = 1usize;
    let mut g = problem.gradient(&b)?;
    let mut iteration = 1usize;
    let mut last_reset = gradient_count;
    let mut inverse = vec![vec![0.0; n]; n];
    let mut previous = vec![0.0; n];
    let mut previous_gradient = vec![0.0; n];
    let mut direction = vec![0.0; n];
    let mut count;
    loop {
        if last_reset == gradient_count {
            for (i, row) in inverse.iter_mut().enumerate() {
                row.fill(0.0);
                row[i] = 1.0;
            }
        }
        previous.copy_from_slice(&b);
        previous_gradient.copy_from_slice(&g);
        let mut slope = 0.0;
        for i in 0..n {
            let mut s = 0.0;
            for j in 0..=i {
                s -= inverse[i][j] * g[j];
            }
            for j in i + 1..n {
                s -= inverse[j][i] * g[j];
            }
            direction[i] = s;
            slope += s * g[i];
        }
        if slope < 0.0 {
            let mut step = 1.0;
            let mut accepted = false;
            loop {
                count = 0;
                for i in 0..n {
                    b[i] = previous[i] + step * direction[i];
                    if RELATIVE_TEST + previous[i] == RELATIVE_TEST + b[i] {
                        count += 1;
                    }
                }
                if count < n {
                    f = problem.value(&b)?;
                    function_count += 1;
                    accepted = f.is_finite() && f <= minimum + slope * step * ACCEPTANCE;
                    if !accepted {
                        step *= STEP_REDUCTION;
                    }
                }
                if count == n || accepted {
                    break;
                }
            }
            let enough = f > control.abs_tol
                && (f - minimum).abs() > control.rel_tol * (minimum.abs() + control.rel_tol);
            if !enough {
                count = n;
                minimum = f;
            }
            if count < n {
                minimum = f;
                g = problem.gradient(&b)?;
                gradient_count += 1;
                iteration += 1;
                let mut d1 = 0.0;
                for i in 0..n {
                    direction[i] *= step;
                    previous_gradient[i] = g[i] - previous_gradient[i];
                    d1 += direction[i] * previous_gradient[i];
                }
                if d1 > 0.0 {
                    let mut d2 = 0.0;
                    let mut product = vec![0.0; n];
                    for i in 0..n {
                        let mut s = 0.0;
                        for j in 0..=i {
                            s += inverse[i][j] * previous_gradient[j];
                        }
                        for j in i + 1..n {
                            s += inverse[j][i] * previous_gradient[j];
                        }
                        product[i] = s;
                        d2 += s * previous_gradient[i];
                    }
                    d2 = 1.0 + d2 / d1;
                    for i in 0..n {
                        for j in 0..=i {
                            inverse[i][j] += (d2 * direction[i] * direction[j]
                                - product[i] * direction[j]
                                - direction[i] * product[j])
                                / d1;
                        }
                    }
                } else {
                    last_reset = gradient_count;
                }
            } else if last_reset < gradient_count {
                count = 0;
                last_reset = gradient_count;
            }
        } else {
            count = 0;
            if last_reset == gradient_count {
                count = n;
            } else {
                last_reset = gradient_count;
            }
        }
        if trace && iteration % report == 0 {
            print(&format!("iter{iteration:4} value {f:.6}\n"));
        }
        if iteration >= control.max_iterations as usize {
            break;
        }
        if gradient_count - last_reset > 2 * n {
            last_reset = gradient_count;
        }
        if count == n && last_reset == gradient_count {
            break;
        }
    }
    let converged = iteration < control.max_iterations as usize;
    if trace {
        print(&format!("final  value {minimum:.6} \n"));
        if converged {
            print("converged\n");
        } else {
            print(&format!("stopped after {iteration} iterations\n"));
        }
    }
    Ok((
        b,
        minimum,
        (function_count, gradient_count),
        if converged { 0 } else { 1 },
    ))
}

/// Nash's conjugate gradients method, with Fletcher-Reeves (1), Polak-Ribiere (2) or
/// Beale-Sorenson (3) updates, a backtracking line search refined by one quadratic
/// interpolation step, and a restart every `n` cycles.
unsafe fn conjugate_gradients(
    problem: &mut Problem<'_>,
    start: &[f64],
    control: &Control,
    kind: i32,
) -> R<(Vec<f64>, f64, (usize, usize), i32)> {
    let n = start.len();
    let trace = control.trace > 0;
    let mut b = start.to_vec();
    if control.max_iterations <= 0 {
        let value = problem.value(&b)?;
        return Ok((b, value, (0, 0), 0));
    }
    if !(1..=3).contains(&kind) {
        return Err(problem
            .call
            .error("unknown 'type' in \"CG\" method of 'optim'"));
    }
    if trace {
        print(&format!(
            "  Conjugate gradients function minimizer\nMethod: {}\n",
            ["Fletcher Reeves", "Polak Ribiere", "Beale Sorenson"][kind as usize - 1]
        ));
    }
    let intolerance = control.rel_tol;
    let tolerance = intolerance * n as f64 * intolerance.sqrt();
    let mut f = problem.value(&b)?;
    if !f.is_finite() {
        return Err(problem
            .call
            .error("Function cannot be evaluated at initial parameters"));
    }
    if trace {
        print(&format!(
            "tolerance used in gradient test={}\n",
            crate::format::c_general(tolerance, 6, false)
        ));
    }
    let mut minimum = f;
    let mut function_count = 1usize;
    let mut gradient_count = 0usize;
    let progress = |gradient_count: usize, function_count: usize, minimum: f64, b: &[f64]| {
        let mut line = format!("{gradient_count} {function_count} {minimum:.6}\nparameters ");
        for value in b {
            line.push_str(&format!("{value:10.5} "));
        }
        line.push('\n');
        print(&line);
    };
    let mut previous_gradient = vec![0.0; n];
    let mut direction = vec![0.0; n];
    let mut x = vec![0.0; n];
    let mut count;
    let mut cycle;
    let mut g1;
    loop {
        direction.fill(0.0);
        previous_gradient.fill(0.0);
        cycle = 0;
        let mut old_step = 1.0;
        count = 0;
        loop {
            if trace {
                progress(gradient_count, function_count, minimum, &b);
            }
            cycle += 1;
            count += 1;
            gradient_count += 1;
            if gradient_count > control.max_iterations as usize {
                // The point of the last gradient, not the last trial point.
                return Ok((x, minimum, (function_count, gradient_count), 1));
            }
            let g = problem.gradient(&b)?;
            g1 = 0.0;
            let mut g2 = 0.0;
            for i in 0..n {
                x[i] = b[i];
                match kind {
                    1 => {
                        g1 += g[i] * g[i];
                        g2 += previous_gradient[i] * previous_gradient[i];
                    }
                    2 => {
                        g1 += g[i] * (g[i] - previous_gradient[i]);
                        g2 += previous_gradient[i] * previous_gradient[i];
                    }
                    _ => {
                        g1 += g[i] * (g[i] - previous_gradient[i]);
                        g2 += direction[i] * (g[i] - previous_gradient[i]);
                    }
                }
                previous_gradient[i] = g[i];
            }
            if g1 > tolerance {
                let ratio = if g2 > 0.0 { g1 / g2 } else { 1.0 };
                let mut slope = 0.0;
                for i in 0..n {
                    direction[i] = direction[i] * ratio - g[i];
                    slope += direction[i] * g[i];
                }
                let mut step = old_step;
                let mut accepted = false;
                loop {
                    count = 0;
                    for i in 0..n {
                        b[i] = x[i] + step * direction[i];
                        if RELATIVE_TEST + x[i] == RELATIVE_TEST + b[i] {
                            count += 1;
                        }
                    }
                    if count < n {
                        f = problem.value(&b)?;
                        function_count += 1;
                        accepted = f.is_finite() && f <= minimum + slope * step * ACCEPTANCE;
                        if !accepted {
                            step *= STEP_REDUCTION;
                            if trace {
                                print("*");
                            }
                        } else {
                            // GNU R records the accepted value here, so the quadratic
                            // interpolation below always proposes half the accepted step.
                            minimum = f;
                        }
                    }
                    if count == n || accepted {
                        break;
                    }
                }
                if count < n {
                    let mut new_step = 2.0 * (f - minimum - slope * step);
                    if new_step > 0.0 {
                        new_step = -(slope * step * step / new_step);
                        for i in 0..n {
                            b[i] = x[i] + new_step * direction[i];
                        }
                        minimum = f;
                        f = problem.value(&b)?;
                        function_count += 1;
                        if f < minimum {
                            minimum = f;
                            if trace {
                                print(" i< ");
                            }
                        } else {
                            if trace {
                                print(" i> ");
                            }
                            for i in 0..n {
                                b[i] = x[i] + step * direction[i];
                            }
                        }
                    }
                }
                old_step = (1.7 * step).min(1.0);
            }
            if !(count != n && g1 > tolerance && cycle != n) {
                break;
            }
        }
        if !(cycle != 1 || (count != n && g1 > tolerance && minimum > control.abs_tol)) {
            break;
        }
    }
    if trace {
        print(&format!(
            "Exiting from conjugate gradients minimizer\n    {function_count} function evaluations used\n    {gradient_count} gradient evaluations used\n"
        ));
    }
    Ok((b, minimum, (function_count, gradient_count), 0))
}

/// Simulated annealing as `optim`'s "SANN": Gaussian candidate moves scaled by the
/// temperature, which falls logarithmically and stays at each level for `tmax` steps,
/// accepting uphill moves by the Metropolis criterion. Returns the best point found.
unsafe fn simulated_annealing(
    problem: &mut Problem<'_>,
    start: &[f64],
    control: &Control,
    steps_per_temperature: i32,
    start_temperature: f64,
) -> R<(Vec<f64>, f64)> {
    let n = start.len();
    let trace = control.trace > 0;
    // e - 1, so that the first temperature is the starting one.
    let e1 = 1.7182818;
    let big = BIG;
    if control.max_iterations <= 0 || n == 0 {
        let value = problem.value(start)?;
        return Ok((start.to_vec(), value));
    }
    let mut point = start.to_vec();
    let mut value = problem.value(&point)?;
    if !value.is_finite() {
        value = big;
    }
    let mut best = point.clone();
    let mut best_value = value;
    let report = control.report.max(1);
    if trace {
        print(&format!(
            "sann objective function values\ninitial       value {value:.6}\n"
        ));
    }
    let scale = 1.0 / start_temperature;
    let mut iteration = 1;
    let mut temperature_level = 1;
    crate::random::get_rng_state()?;
    while iteration < control.max_iterations {
        let temperature = start_temperature / (iteration as f64 + e1).ln();
        let mut k = 1;
        while k <= steps_per_temperature && iteration < control.max_iterations {
            let candidate: Vec<f64> = if problem.gradient != nil() {
                candidate_from_r(problem, &point)?
            } else {
                let mut proposal = point.clone();
                for value in proposal.iter_mut() {
                    *value += scale * temperature * crate::random::norm_rand()?;
                }
                proposal
            };
            let mut candidate_value = problem.value(&candidate)?;
            if !candidate_value.is_finite() {
                candidate_value = big;
            }
            let change = candidate_value - value;
            if change <= 0.0 || crate::random::unif_rand()? < (-change / temperature).exp() {
                point = candidate;
                value = candidate_value;
                if value <= best_value {
                    best = point.clone();
                    best_value = value;
                }
            }
            iteration += 1;
            k += 1;
        }
        if trace && temperature_level % report == 0 {
            print(&format!("iter {:8} value {best_value:.6}\n", iteration - 1));
        }
        temperature_level += 1;
    }
    crate::random::put_rng_state()?;
    if trace {
        print(&format!(
            "final         value {best_value:.6}\nsann stopped after {} iterations\n",
            iteration - 1
        ));
    }
    Ok((best, best_value))
}

/// A SANN candidate from the user's `gr`, which for this method generates the next point.
unsafe fn candidate_from_r(problem: &Problem<'_>, point: &[f64]) -> R<Vec<f64>> {
    let parameters: Vec<f64> = point
        .iter()
        .zip(&problem.par_scale)
        .map(|(value, scale)| value * scale)
        .collect();
    let call = problem.call;
    let (gradient, names, n) = (problem.gradient, problem.names, point.len());
    let raw = crate::scope(|| {
        let argument = values::real_vector(&parameters)?;
        if names != nil() {
            values::set_attr(argument, symbols().names, names)?;
        }
        let expression = values::call(gradient, &[argument])?;
        let value = eval::eval(expression, call.env)?;
        if length(value) != n {
            return Err(call.error(format!(
                "candidate point in 'optim' evaluated to length {} not {n}",
                length(value)
            )));
        }
        let value = coerce_vector(value, SexpType::Real)?;
        Ok(ValueList(values::reals(value).to_vec()))
    })?;
    Ok(raw
        .0
        .iter()
        .zip(&problem.par_scale)
        .map(|(value, scale)| value / scale)
        .collect())
}

// integrate.

/// `call_dqags(f, rho, lower, upper, abs.tol, rel.tol, limit)`: QUADPACK's QAGS on a finite
/// interval.
pub unsafe fn call_dqags(call: &NativeCall<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let env = call.arg(1);
    let lower = values::as_real_scalar(call.arg(2));
    let upper = values::as_real_scalar(call.arg(3));
    let abs_tol = values::as_real_scalar(call.arg(4));
    let rel_tol = values::as_real_scalar(call.arg(5));
    let limit = values::as_integer_scalar(call.arg(6));
    let mut integrand = Integrand {
        call,
        function,
        env,
    };
    let outcome = qags(
        &mut integrand,
        lower,
        upper,
        abs_tol,
        rel_tol,
        limit.max(1) as usize,
    )?;
    integration_result(outcome)
}

/// `call_dqagi(f, rho, bound, inf, abs.tol, rel.tol, limit)`: QUADPACK's QAGI on a half-
/// infinite (`inf` 1 or -1) or infinite (`inf` 2) range.
pub unsafe fn call_dqagi(call: &NativeCall<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let env = call.arg(1);
    let bound = values::as_real_scalar(call.arg(2));
    let infinite = values::as_integer_scalar(call.arg(3));
    let abs_tol = values::as_real_scalar(call.arg(4));
    let rel_tol = values::as_real_scalar(call.arg(5));
    let limit = values::as_integer_scalar(call.arg(6));
    let mut integrand = Integrand {
        call,
        function,
        env,
    };
    let outcome = qagi(
        &mut integrand,
        bound,
        infinite,
        abs_tol,
        rel_tol,
        limit.max(1) as usize,
    )?;
    integration_result(outcome)
}

unsafe fn integration_result(outcome: Quadrature) -> R<Sexp> {
    values::named_list(&[
        ("value", values::scalar_real(outcome.result)?),
        ("abs.error", values::scalar_real(outcome.error)?),
        ("subdivisions", values::scalar_integer(outcome.last as i32)?),
        ("ierr", values::scalar_integer(outcome.status)?),
    ])
}

/// The R integrand, evaluated on a whole vector of abscissae at once.
struct Integrand<'a> {
    call: &'a NativeCall<'a>,
    function: Sexp,
    env: Sexp,
}

impl Integrand<'_> {
    unsafe fn evaluate(&mut self, x: &mut [f64]) -> R<()> {
        let call = self.call;
        let (function, env) = (self.function, self.env);
        let n = x.len();
        let values = crate::scope(|| {
            let argument = values::real_vector(x)?;
            let expression = values::call(function, &[argument])?;
            let result = eval::eval(expression, env)?;
            if length(result) != n {
                return Err(call.error("evaluation of function gave a result of wrong length"));
            }
            let result = match result.kind() {
                SexpType::Integer => coerce_vector(result, SexpType::Real)?,
                SexpType::Real => result,
                _ => return Err(call.error("evaluation of function gave a result of wrong type")),
            };
            Ok(ValueList(values::reals(result).to_vec()))
        })?;
        for (slot, value) in x.iter_mut().zip(values.0) {
            if !value.is_finite() {
                return Err(call.error("non-finite function value"));
            }
            *slot = value;
        }
        Ok(())
    }
}

struct Quadrature {
    result: f64,
    error: f64,
    last: usize,
    status: i32,
}

/// The 21-point Kronrod abscissae on [-1, 1] (the even ones are the 10-point Gauss
/// abscissae) and the Kronrod and Gauss weights.
const KRONROD21_NODES: [f64; 11] = [
    0.995657163025808080735527280689003,
    0.973906528517171720077964012084452,
    0.930157491355708226001207180059508,
    0.865063366688984510732096688423493,
    0.780817726586416897063717578345042,
    0.679409568299024406234327365114874,
    0.562757134668604683339000099272694,
    0.433395394129247190799265943165784,
    0.294392862701460198131126603103866,
    0.148874338981631210884826001129720,
    0.000000000000000000000000000000000,
];
const KRONROD21_WEIGHTS: [f64; 11] = [
    0.011694638867371874278064396062192,
    0.032558162307964727478818972459390,
    0.054755896574351996031381300244580,
    0.075039674810919952767043140916190,
    0.093125454583697605535065465083366,
    0.109387158802297641899210590325805,
    0.123491976262065851077958109831074,
    0.134709217311473325928054001771707,
    0.142775938577060080797094273138717,
    0.147739104901338491374841515972068,
    0.149445554002916905664936468389821,
];
const GAUSS10_WEIGHTS: [f64; 5] = [
    0.066671344308688137593568809893332,
    0.149451349150580593145776339657697,
    0.219086362515982043995534934228163,
    0.269266719309996355091226921569469,
    0.295524224714752870173892994651338,
];

const KRONROD15_NODES: [f64; 8] = [
    0.991455371120812639206854697526329,
    0.949107912342758524526189684047851,
    0.864864423359769072789712788640926,
    0.741531185599394439863864773280788,
    0.586087235467691130294144845693013,
    0.405845151377397166906606412076961,
    0.207784955007898467600689403773245,
    0.000000000000000000000000000000000,
];
const KRONROD15_WEIGHTS: [f64; 8] = [
    0.022935322010529224963732008058970,
    0.063092092629978553290700663189204,
    0.104790010322250183839876322541518,
    0.140653259715525918745189590510238,
    0.169004726639267902826583426598550,
    0.190350578064785409913256402421014,
    0.204432940075298892414161999234649,
    0.209482141084727828012999174891714,
];
const GAUSS7_WEIGHTS: [f64; 8] = [
    0.0,
    0.129484966168869693270611432679082,
    0.0,
    0.279705391489276667901467771423780,
    0.0,
    0.381830050505118944950369775488975,
    0.0,
    0.417959183673469387755102040816327,
];

/// One rule application: the integral estimate, its error estimate, the integral of `|f|`
/// and the integral of `|f - mean|`.
struct Rule {
    result: f64,
    error: f64,
    absolute: f64,
    deviation: f64,
}

/// QUADPACK's error estimate from the difference of the Kronrod and Gauss results.
fn rule_error(difference: f64, absolute: f64, deviation: f64) -> f64 {
    let mut error = difference.abs();
    if deviation != 0.0 && error != 0.0 {
        error = deviation * (200.0 * error / deviation).powf(1.5).min(1.0);
    }
    if absolute > f64::MIN_POSITIVE / (50.0 * f64::EPSILON) {
        error = error.max(50.0 * f64::EPSILON * absolute);
    }
    error
}

/// The 21-point Gauss-Kronrod rule on `[a, b]`.
unsafe fn kronrod21(f: &mut Integrand<'_>, a: f64, b: f64) -> R<Rule> {
    let center = (a + b) * 0.5;
    let half = (b - a) * 0.5;
    // The abscissae in the order the integrand sees them: the center, the Gauss points in
    // pairs, then the remaining Kronrod points in pairs.
    let mut x = [0.0; 21];
    x[0] = center;
    for j in 1..=5 {
        let offset = half * KRONROD21_NODES[2 * j - 1];
        x[2 * j - 1] = center - offset;
        x[2 * j] = center + offset;
    }
    for j in 1..=5 {
        let offset = half * KRONROD21_NODES[2 * j - 2];
        x[2 * j + 9] = center - offset;
        x[2 * j + 10] = center + offset;
    }
    f.evaluate(&mut x)?;
    let center_value = x[0];
    let mut gauss = 0.0;
    let mut kronrod = KRONROD21_WEIGHTS[10] * center_value;
    let mut absolute = kronrod.abs();
    let mut left = [0.0; 10];
    let mut right = [0.0; 10];
    for j in 1..=5 {
        let node = 2 * j - 1;
        let (low, high) = (x[2 * j - 1], x[2 * j]);
        left[node] = low;
        right[node] = high;
        gauss += GAUSS10_WEIGHTS[j - 1] * (low + high);
        kronrod += KRONROD21_WEIGHTS[node] * (low + high);
        absolute += KRONROD21_WEIGHTS[node] * (low.abs() + high.abs());
    }
    for j in 1..=5 {
        let node = 2 * j - 2;
        let (low, high) = (x[2 * j + 9], x[2 * j + 10]);
        left[node] = low;
        right[node] = high;
        kronrod += KRONROD21_WEIGHTS[node] * (low + high);
        absolute += KRONROD21_WEIGHTS[node] * (low.abs() + high.abs());
    }
    let mean = kronrod * 0.5;
    let mut deviation = KRONROD21_WEIGHTS[10] * (center_value - mean).abs();
    for j in 0..10 {
        deviation += KRONROD21_WEIGHTS[j] * ((left[j] - mean).abs() + (right[j] - mean).abs());
    }
    let scale = half.abs();
    let (absolute, deviation) = (absolute * scale, deviation * scale);
    Ok(Rule {
        result: kronrod * half,
        error: rule_error((kronrod - gauss) * half, absolute, deviation),
        absolute,
        deviation,
    })
}

/// The 15-point Gauss-Kronrod rule on `[a, b]` within (0, 1], for the integral over the
/// infinite range mapped there by `x = bound + sign (1 - t) / t`.
unsafe fn kronrod15_infinite(
    f: &mut Integrand<'_>,
    bound: f64,
    infinite: i32,
    a: f64,
    b: f64,
) -> R<Rule> {
    let sign = infinite.min(1) as f64;
    let center = (a + b) * 0.5;
    let half = (b - a) * 0.5;
    let map = |t: f64| bound + sign * (1.0 - t) / t;
    let mut x = [0.0; 15];
    x[0] = map(center);
    for j in 1..=7 {
        let offset = half * KRONROD15_NODES[j - 1];
        x[2 * j - 1] = map(center - offset);
        x[2 * j] = map(center + offset);
    }
    let mut mirrored: Option<[f64; 15]> = (infinite == 2).then(|| x.map(|value| -value));
    f.evaluate(&mut x)?;
    if let Some(mirror) = mirrored.as_mut() {
        f.evaluate(mirror)?;
    }
    let total = |index: usize| x[index] + mirrored.map_or(0.0, |mirror| mirror[index]);
    let center_value = total(0) / center / center;
    let mut gauss = GAUSS7_WEIGHTS[7] * center_value;
    let mut kronrod = KRONROD15_WEIGHTS[7] * center_value;
    let mut absolute = kronrod.abs();
    let mut left = [0.0; 7];
    let mut right = [0.0; 7];
    for j in 1..=7 {
        let offset = half * KRONROD15_NODES[j - 1];
        let (t1, t2) = (center - offset, center + offset);
        let low = total(2 * j - 1) / t1 / t1;
        let high = total(2 * j) / t2 / t2;
        left[j - 1] = low;
        right[j - 1] = high;
        gauss += GAUSS7_WEIGHTS[j - 1] * (low + high);
        kronrod += KRONROD15_WEIGHTS[j - 1] * (low + high);
        absolute += KRONROD15_WEIGHTS[j - 1] * (low.abs() + high.abs());
    }
    let mean = kronrod * 0.5;
    let mut deviation = KRONROD15_WEIGHTS[7] * (center_value - mean).abs();
    for j in 0..7 {
        deviation += KRONROD15_WEIGHTS[j] * ((left[j] - mean).abs() + (right[j] - mean).abs());
    }
    let (absolute, deviation) = (absolute * half, deviation * half);
    Ok(Rule {
        result: kronrod * half,
        error: rule_error((kronrod - gauss) * half, absolute, deviation),
        absolute,
        deviation,
    })
}

unsafe fn qags(
    f: &mut Integrand<'_>,
    a: f64,
    b: f64,
    abs_tol: f64,
    rel_tol: f64,
    limit: usize,
) -> R<Quadrature> {
    adaptive(
        limit,
        abs_tol,
        rel_tol,
        a,
        b,
        (b - a).abs() * 0.375,
        &mut |lower, upper| kronrod21(f, lower, upper),
    )
}

unsafe fn qagi(
    f: &mut Integrand<'_>,
    bound: f64,
    infinite: i32,
    abs_tol: f64,
    rel_tol: f64,
    limit: usize,
) -> R<Quadrature> {
    let bound = if infinite == 2 { 0.0 } else { bound };
    adaptive(
        limit,
        abs_tol,
        rel_tol,
        0.0,
        1.0,
        0.375,
        &mut |lower, upper| kronrod15_infinite(f, bound, infinite, lower, upper),
    )
}

/// QUADPACK's QAGSE/QAGIE driver: bisects the interval with the largest error estimate,
/// keeping the error estimates sorted, and accelerates convergence by Wynn's epsilon
/// algorithm over the sequence of estimates once the smallest intervals dominate the error.
unsafe fn adaptive(
    limit: usize,
    abs_tol: f64,
    rel_tol: f64,
    a: f64,
    b: f64,
    initial_small: f64,
    rule: &mut dyn FnMut(f64, f64) -> R<Rule>,
) -> R<Quadrature> {
    let epsilon = f64::EPSILON;
    let underflow = f64::MIN_POSITIVE;
    let overflow = f64::MAX;
    if abs_tol <= 0.0 && rel_tol < (50.0 * epsilon).max(0.5e-28) {
        return Ok(Quadrature {
            result: 0.0,
            error: 0.0,
            last: 0,
            status: 6,
        });
    }
    // Interval lists, 1-based as QUADPACK keeps them so that `order` can hold indices.
    let mut lower = vec![0.0; limit + 1];
    let mut upper = vec![0.0; limit + 1];
    let mut results = vec![0.0; limit + 1];
    let mut errors = vec![0.0; limit + 1];
    let mut order = vec![0usize; limit + 1];
    lower[1] = a;
    upper[1] = b;
    let first = rule(a, b)?;
    let mut result = first.result;
    let mut abserr = first.error;
    let defabs = first.absolute;
    let magnitude = result.abs();
    let mut bound = abs_tol.max(rel_tol * magnitude);
    let mut last = 1usize;
    results[1] = result;
    errors[1] = abserr;
    order[1] = 1;
    let mut status = 0;
    if abserr <= 100.0 * epsilon * defabs && abserr > bound {
        status = 2;
    }
    if limit == 1 {
        status = 1;
    }
    if status != 0 || (abserr <= bound && abserr != first.deviation) || abserr == 0.0 {
        return Ok(Quadrature {
            result,
            error: abserr,
            last,
            status,
        });
    }
    let mut table = [0.0; 55];
    table[1] = result;
    let mut previous_results = [0.0; 4];
    let mut error_max = abserr;
    let mut max_error = 1usize;
    let mut area = result;
    let mut error_sum = abserr;
    abserr = overflow;
    let mut nr_max = 1usize;
    let mut results_count = 0usize;
    let mut table_size = 2usize;
    let mut k_min = 0;
    let mut extrapolating = false;
    let mut no_extrapolation = false;
    let (mut roundoff1, mut roundoff2, mut roundoff3) = (0, 0, 0);
    let mut roundoff_status = 0;
    let sign = if magnitude >= (1.0 - 50.0 * epsilon) * defabs {
        1
    } else {
        -1
    };
    let mut small = 0.0;
    let mut error_large = 0.0;
    let mut error_test = 0.0;
    let mut correction = 0.0;
    let mut summed = false;
    last = 2;
    while last <= limit {
        let a1 = lower[max_error];
        let b1 = 0.5 * (lower[max_error] + upper[max_error]);
        let a2 = b1;
        let b2 = upper[max_error];
        let error_last = error_max;
        let left = rule(a1, b1)?;
        let right = rule(a2, b2)?;
        let area12 = left.result + right.result;
        let error12 = left.error + right.error;
        error_sum = error_sum + error12 - error_max;
        area = area + area12 - results[max_error];
        if left.deviation != left.error && right.deviation != right.error {
            if (results[max_error] - area12).abs() <= 1e-5 * area12.abs()
                && error12 >= 0.99 * error_max
            {
                if extrapolating {
                    roundoff2 += 1;
                } else {
                    roundoff1 += 1;
                }
            }
            if last > 10 && error12 > error_max {
                roundoff3 += 1;
            }
        }
        results[max_error] = left.result;
        results[last] = right.result;
        bound = abs_tol.max(rel_tol * area.abs());
        if roundoff1 + roundoff2 >= 10 || roundoff3 >= 20 {
            status = 2;
        }
        if roundoff2 >= 5 {
            roundoff_status = 3;
        }
        if last == limit {
            status = 1;
        }
        if a1.abs().max(b2.abs()) <= (1.0 + 100.0 * epsilon) * (a2.abs() + 1000.0 * underflow) {
            status = 4;
        }
        if right.error > left.error {
            lower[max_error] = a2;
            lower[last] = a1;
            upper[last] = b1;
            results[max_error] = right.result;
            results[last] = left.result;
            errors[max_error] = right.error;
            errors[last] = left.error;
        } else {
            lower[last] = a2;
            upper[max_error] = b1;
            upper[last] = b2;
            errors[max_error] = left.error;
            errors[last] = right.error;
        }
        sort_errors(
            limit,
            last,
            &mut max_error,
            &mut error_max,
            &errors,
            &mut order,
            &mut nr_max,
        );
        if error_sum <= bound {
            summed = true;
            break;
        }
        if status != 0 {
            break;
        }
        if last == 2 {
            small = initial_small;
            error_large = error_sum;
            error_test = bound;
            table[2] = area;
            last += 1;
            continue;
        }
        if no_extrapolation {
            last += 1;
            continue;
        }
        error_large -= error_last;
        if (b1 - a1).abs() > small {
            error_large += error12;
        }
        if !extrapolating {
            if (upper[max_error] - lower[max_error]).abs() > small {
                last += 1;
                continue;
            }
            extrapolating = true;
            nr_max = 2;
        }
        let mut bisect_large = false;
        if roundoff_status != 3 && error_large > error_test {
            let upper_bound = if last > 2 + limit / 2 {
                limit + 3 - last
            } else {
                last
            };
            for _ in nr_max..=upper_bound {
                max_error = order[nr_max];
                error_max = errors[max_error];
                if (upper[max_error] - lower[max_error]).abs() > small {
                    bisect_large = true;
                    break;
                }
                nr_max += 1;
            }
        }
        if bisect_large {
            last += 1;
            continue;
        }
        table_size += 1;
        table[table_size] = area;
        let (extrapolated, extrapolated_error) = epsilon_algorithm(
            &mut table_size,
            &mut table,
            &mut previous_results,
            &mut results_count,
        );
        k_min += 1;
        if k_min > 5 && abserr < 1e-3 * error_sum {
            status = 5;
        }
        if extrapolated_error < abserr {
            k_min = 0;
            abserr = extrapolated_error;
            result = extrapolated;
            correction = error_large;
            error_test = abs_tol.max(rel_tol * extrapolated.abs());
            if abserr <= error_test {
                break;
            }
        }
        if table_size == 1 {
            no_extrapolation = true;
        }
        if status == 5 {
            break;
        }
        max_error = order[1];
        error_max = errors[max_error];
        nr_max = 1;
        extrapolating = false;
        small *= 0.5;
        error_large = error_sum;
        last += 1;
    }
    let last = last.min(limit);
    let sum_results = |last: usize| results[1..=last].iter().sum::<f64>();
    if !summed {
        // QUADPACK's labels 100 to 130: choose between the extrapolated and summed results.
        let mut use_sum = false;
        let mut divergence_test = true;
        if abserr == overflow {
            use_sum = true;
        } else if status + roundoff_status != 0 {
            if roundoff_status == 3 {
                abserr += correction;
            }
            if status == 0 {
                status = 3;
            }
            if result != 0.0 && area != 0.0 {
                if abserr / result.abs() > error_sum / area.abs() {
                    use_sum = true;
                }
            } else if abserr > error_sum {
                use_sum = true;
            } else if area == 0.0 {
                divergence_test = false;
            }
        }
        if use_sum {
            result = sum_results(last);
            abserr = error_sum;
        } else if divergence_test && !(sign == -1 && result.abs().max(area.abs()) <= defabs * 0.01)
        {
            if 0.01 > result / area || result / area > 100.0 || error_sum > area.abs() {
                status = 6;
            }
        }
    } else {
        result = sum_results(last);
        abserr = error_sum;
    }
    if status > 2 {
        status -= 1;
    }
    Ok(Quadrature {
        result,
        error: abserr,
        last,
        status,
    })
}

/// QUADPACK's QPSRT: keeps `order[1..]` listing the intervals by decreasing error estimate
/// (only as deep as the remaining subdivisions can use), after interval `max_error` was
/// split into itself and `last`; then selects the `nr_max`-th largest for bisection.
fn sort_errors(
    limit: usize,
    last: usize,
    max_error: &mut usize,
    error_max: &mut f64,
    errors: &[f64],
    order: &mut [usize],
    nr_max: &mut usize,
) {
    if last <= 2 {
        order[1] = 1;
        order[2] = 2;
    } else {
        let largest = errors[*max_error];
        if *nr_max != 1 {
            for _ in 1..*nr_max {
                let successor = order[*nr_max - 1];
                if largest <= errors[successor] {
                    break;
                }
                order[*nr_max] = successor;
                *nr_max -= 1;
            }
        }
        let depth = if last > limit / 2 + 2 {
            limit + 3 - last
        } else {
            last
        };
        let smallest = errors[last];
        let bottom = depth - 1;
        let begin = *nr_max + 1;
        let mut inserted_at = None;
        for i in begin..=bottom {
            let successor = order[i];
            if largest >= errors[successor] {
                inserted_at = Some(i);
                break;
            }
            order[i - 1] = successor;
        }
        match inserted_at {
            None => {
                order[bottom] = *max_error;
                order[depth] = last;
            }
            Some(i) => {
                order[i - 1] = *max_error;
                let mut k = bottom;
                let mut placed = false;
                for _ in i..=bottom {
                    let successor = order[k];
                    if smallest < errors[successor] {
                        order[k + 1] = last;
                        placed = true;
                        break;
                    }
                    order[k + 1] = successor;
                    k -= 1;
                }
                if !placed {
                    order[i] = last;
                }
            }
        }
    }
    *max_error = order[*nr_max];
    *error_max = errors[*max_error];
}

/// QUADPACK's QELG: Wynn's epsilon algorithm on `table[1..=size]`, which it extends and then
/// shortens. Returns the extrapolated limit and an error estimate from the last three
/// results.
fn epsilon_algorithm(
    size: &mut usize,
    table: &mut [f64; 55],
    previous: &mut [f64; 4],
    count: &mut usize,
) -> (f64, f64) {
    let epsilon = f64::EPSILON;
    let overflow = f64::MAX;
    let limit_size = 50;
    *count += 1;
    let mut error = overflow;
    let mut result = table[*size];
    if *size < 3 {
        return (result, error.max(5.0 * epsilon * result.abs()));
    }
    let n = *size;
    table[n + 2] = table[n];
    let new_elements = (n - 1) / 2;
    table[n] = overflow;
    let original = n;
    let mut k1 = n;
    let mut length_now = n;
    for i in 1..=new_elements {
        let k2 = k1 - 1;
        let k3 = k1 - 2;
        let res = table[k1 + 2];
        let e0 = table[k3];
        let e1 = table[k2];
        let e2 = res;
        let e1_abs = e1.abs();
        let delta2 = e2 - e1;
        let err2 = delta2.abs();
        let tol2 = e2.abs().max(e1_abs) * epsilon;
        let delta3 = e1 - e0;
        let err3 = delta3.abs();
        let tol3 = e1_abs.max(e0.abs()) * epsilon;
        if err2 <= tol2 && err3 <= tol3 {
            result = res;
            error = err2 + err3;
            return (result, error.max(5.0 * epsilon * result.abs()));
        }
        let e3 = table[k1];
        table[k1] = e1;
        let delta1 = e1 - e3;
        let err1 = delta1.abs();
        let tol1 = e1_abs.max(e3.abs()) * epsilon;
        if err1 <= tol1 || err2 <= tol2 || err3 <= tol3 {
            length_now = i + i - 1;
            break;
        }
        let ss = 1.0 / delta1 + 1.0 / delta2 - 1.0 / delta3;
        let epsinf = (ss * e1).abs();
        if epsinf <= 1e-4 {
            length_now = i + i - 1;
            break;
        }
        let res = e1 + 1.0 / ss;
        table[k1] = res;
        k1 -= 2;
        let candidate_error = err2 + (res - e2).abs() + err3;
        if candidate_error <= error {
            error = candidate_error;
            result = res;
        }
    }
    if length_now == limit_size {
        length_now = 2 * (limit_size / 2) - 1;
    }
    let mut ib = if original % 2 == 0 { 2 } else { 1 };
    for _ in 1..=new_elements + 1 {
        table[ib] = table[ib + 2];
        ib += 2;
    }
    if original != length_now {
        let mut index = original - length_now + 1;
        for i in 1..=length_now {
            table[i] = table[index];
            index += 1;
        }
    }
    *size = length_now;
    if *count < 4 {
        previous[*count] = result;
        error = overflow;
    } else {
        error = (result - previous[3]).abs()
            + (result - previous[2]).abs()
            + (result - previous[1]).abs();
        previous[1] = previous[2];
        previous[2] = previous[3];
        previous[3] = result;
    }
    (result, error.max(5.0 * epsilon * result.abs()))
}
