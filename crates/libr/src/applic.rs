//! The numerical routines of `R_ext/Applic.h`: the optimizers behind `optim()` (`vmmin`,
//! `nmmin`, `cgmin`, `lbfgsb`, `samin`) and `nlm()` (`optif9`, `fdhess`), QUADPACK's adaptive
//! quadrature behind `integrate()` (`Rdqags`, `Rdqagi`), interval search (`findInterval`),
//! `R_max_col` and `R_pretty`.
//!
//! Each follows the published algorithm GNU R implements (Nash's "Compact Numerical Methods"
//! for the optim family, Dennis and Schnabel's UNCMIN, QUADPACK, Byrd, Lu, Nocedal and Zhu's
//! L-BFGS-B with the Moré-Thuente line search) step for step, because callers observe more than
//! the optimum: the evaluation counts, the convergence codes and the exact iterates.
//!
//! The objective functions, gradients and integrands are native code, so they run through C
//! trampolines in `native.c`. An error inside one comes back as `Err` and leaves through
//! `enter` like any other API error, after every buffer here has been dropped.

// The algorithms index several arrays in lockstep exactly as published, which keeps them
// checkable line by line against the papers; the quadrature constants are quoted at their
// published precision.
#![allow(clippy::needless_range_loop, clippy::excessive_precision)]

use crate::{
    arith::{read_seed, write_seed},
    boundary::{ApiResult, enter, error, take_pending, warn, write_console},
    linpack::{Matrix, ddot, dnrm2, triangular_solve},
    runtime,
    utils::format_real_vector,
};
use heap::object::NA_INTEGER;
use std::ffi::{c_char, c_int, c_void};

pub type OptimFn = unsafe extern "C" fn(c_int, *mut f64, *mut c_void) -> f64;
pub type OptimGr = unsafe extern "C" fn(c_int, *mut f64, *mut f64, *mut c_void);
pub type IntegrFn = unsafe extern "C" fn(*mut f64, c_int, *mut c_void);
pub type FcnP = unsafe extern "C" fn(c_int, *mut f64, *mut f64, *mut c_void);
pub type D2FcnP = unsafe extern "C" fn(c_int, c_int, *mut f64, *mut f64, *mut c_void);

unsafe extern "C" {
    fn ry_call_optimfn(
        function: OptimFn,
        n: c_int,
        parameters: *mut f64,
        ex: *mut c_void,
        result: *mut f64,
    ) -> c_int;
    fn ry_call_optimgr(
        function: OptimGr,
        n: c_int,
        parameters: *mut f64,
        gradient: *mut f64,
        ex: *mut c_void,
    ) -> c_int;
    fn ry_call_integrand(function: IntegrFn, x: *mut f64, n: c_int, ex: *mut c_void) -> c_int;
    fn ry_call_fcn(
        function: FcnP,
        n: c_int,
        x: *mut f64,
        value: *mut f64,
        state: *mut c_void,
    ) -> c_int;
    fn ry_call_d2fcn(
        function: D2FcnP,
        nr: c_int,
        n: c_int,
        x: *mut f64,
        h: *mut f64,
        state: *mut c_void,
    ) -> c_int;
    fn ry_norm_rand_available() -> c_int;
    fn ry_norm_rand() -> f64;
}

/// Runs one native callback. Handles the callback's API calls hand out are released when it
/// returns, so a long optimization does not accumulate them; R_alloc memory is left alone,
/// because GNU R keeps it until the enclosing `.Call` returns and callbacks may rely on that.
unsafe fn callback(call: impl FnOnce() -> c_int) -> ApiResult<()> {
    let mark = runtime().heap.handle_mark();
    let status = call();
    runtime().heap.release_handles(mark);
    if status != 0 {
        Err(take_pending())
    } else {
        Ok(())
    }
}

/// An objective, its optional gradient and the opaque pointer both receive.
#[derive(Clone, Copy)]
struct Objective {
    function: OptimFn,
    gradient: Option<OptimGr>,
    ex: *mut c_void,
}

impl Objective {
    fn new(
        function: Option<OptimFn>,
        gradient: Option<OptimGr>,
        ex: *mut c_void,
    ) -> ApiResult<Objective> {
        let function = function.ok_or_else(|| error("the objective function is NULL"))?;
        Ok(Objective {
            function,
            gradient,
            ex,
        })
    }

    unsafe fn value(&self, parameters: &mut [f64]) -> ApiResult<f64> {
        let mut result = 0.0;
        callback(|| {
            ry_call_optimfn(
                self.function,
                parameters.len() as c_int,
                parameters.as_mut_ptr(),
                self.ex,
                &mut result,
            )
        })?;
        Ok(result)
    }

    unsafe fn gradient(&self, parameters: &mut [f64], gradient: &mut [f64]) -> ApiResult<()> {
        let function = self
            .gradient
            .ok_or_else(|| error("the gradient function is NULL"))?;
        callback(|| {
            ry_call_optimgr(
                function,
                parameters.len() as c_int,
                parameters.as_mut_ptr(),
                gradient.as_mut_ptr(),
                self.ex,
            )
        })
    }
}

unsafe fn vector<'a>(pointer: *mut f64, length: c_int) -> &'a mut [f64] {
    if pointer.is_null() || length <= 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(pointer, length as usize)
    }
}

fn print(text: &str) {
    write_console(text.as_bytes(), false);
}

/// C's `%g`: 6 significant digits, scientific when the exponent is below -4 or above 5, with
/// trailing zeros removed.
fn format_g(value: f64) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "nan".into()
        } else if value > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    if value == 0.0 {
        return "0".into();
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let trim = |text: &str| -> String {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            text.to_owned()
        }
    };
    if !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exponent.abs())
    } else {
        let decimals = (5 - exponent).max(0) as usize;
        trim(&format!("{value:.decimals$}"))
    }
}

// Nash's optimizers, as optim() runs them.

/// What Nelder-Mead and simulated annealing substitute for a non-finite function value.
const BIG: f64 = 1.0e+35;

/// Variable metric minimization (BFGS), Nash's Algorithm 21, over the parameters whose `mask`
/// entry is nonzero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmmin(
    n0: c_int,
    b: *mut f64,
    fmin: *mut f64,
    fminfn: Option<OptimFn>,
    fmingr: Option<OptimGr>,
    maxit: c_int,
    trace: c_int,
    mask: *mut c_int,
    abstol: f64,
    reltol: f64,
    report: c_int,
    ex: *mut c_void,
    fncount: *mut c_int,
    grcount: *mut c_int,
    fail: *mut c_int,
) {
    enter(|| {
        let objective = Objective::new(fminfn, fmingr, ex)?;
        let parameters = vector(b, n0);
        if maxit <= 0 {
            *fail = 0;
            *fmin = objective.value(parameters)?;
            *fncount = 0;
            *grcount = 0;
            return Ok(());
        }
        if report <= 0 {
            return Err(error("REPORT must be > 0 (method = \"BFGS\")"));
        }
        let free: Vec<usize> = (0..parameters.len())
            .filter(|&index| mask.is_null() || *mask.add(index) != 0)
            .collect();
        let count = free.len();
        let mut value = objective.value(parameters)?;
        if !value.is_finite() {
            return Err(error("initial value in 'vmmin' is not finite"));
        }
        if trace != 0 {
            print(&format!("initial  value {value:.6} \n"));
        }
        let mut best = value;
        let mut evaluations: c_int = 1;
        let mut gradients: c_int = 1;
        let mut gradient = vec![0.0; parameters.len()];
        objective.gradient(parameters, &mut gradient)?;
        let mut iteration: c_int = 1;
        let mut last_reset = gradients;
        // The approximate inverse Hessian, symmetric, so only its lower triangle is kept.
        let mut metric = vec![0.0; count * count];
        let mut origin = vec![0.0; count];
        let mut gradient_change = vec![0.0; count];
        let mut direction = vec![0.0; count];
        loop {
            if last_reset == gradients {
                for row in 0..count {
                    for column in 0..=row {
                        metric[row * count + column] = if row == column { 1.0 } else { 0.0 };
                    }
                }
            }
            for (position, &index) in free.iter().enumerate() {
                origin[position] = parameters[index];
                gradient_change[position] = gradient[index];
            }
            let mut slope = 0.0;
            for row in 0..count {
                let mut sum = 0.0;
                for column in 0..=row {
                    sum -= metric[row * count + column] * gradient[free[column]];
                }
                for column in row + 1..count {
                    sum -= metric[column * count + row] * gradient[free[column]];
                }
                direction[row] = sum;
                slope += sum * gradient[free[row]];
            }
            let stalled;
            if slope < 0.0 {
                let mut step = 1.0;
                let mut moved = acceptable_point(
                    &objective,
                    parameters,
                    &free,
                    &origin,
                    &direction,
                    Descent { best, slope },
                    &mut step,
                    &mut value,
                    &mut evaluations,
                    false,
                )?;
                let improving =
                    value > abstol && (value - best).abs() > reltol * (best.abs() + reltol);
                if !improving {
                    moved = false;
                    best = value;
                }
                if moved {
                    best = value;
                    objective.gradient(parameters, &mut gradient)?;
                    gradients += 1;
                    iteration += 1;
                    let mut curvature = 0.0;
                    for position in 0..count {
                        direction[position] *= step;
                        gradient_change[position] =
                            gradient[free[position]] - gradient_change[position];
                        curvature += direction[position] * gradient_change[position];
                    }
                    if curvature > 0.0 {
                        // `origin` is free again, so it holds the metric times the gradient change.
                        let mut weight = 0.0;
                        for row in 0..count {
                            let mut sum = 0.0;
                            for column in 0..=row {
                                sum += metric[row * count + column] * gradient_change[column];
                            }
                            for column in row + 1..count {
                                sum += metric[column * count + row] * gradient_change[column];
                            }
                            origin[row] = sum;
                            weight += sum * gradient_change[row];
                        }
                        weight = 1.0 + weight / curvature;
                        for row in 0..count {
                            for column in 0..=row {
                                metric[row * count + column] +=
                                    (weight * direction[row] * direction[column]
                                        - origin[row] * direction[column]
                                        - direction[row] * origin[column])
                                        / curvature;
                            }
                        }
                    } else {
                        last_reset = gradients;
                    }
                    stalled = false;
                } else if last_reset < gradients {
                    stalled = false;
                    last_reset = gradients;
                } else {
                    stalled = true;
                }
            } else if last_reset == gradients {
                stalled = true;
            } else {
                stalled = false;
                last_reset = gradients;
            }
            if trace != 0 && iteration % report == 0 {
                print(&format!("iter{iteration:>4} value {value:.6}\n"));
            }
            if iteration >= maxit {
                break;
            }
            // The metric also restarts from the identity after 2n updates, as steepest
            // descent does in a conjugate gradients method, so that stale curvature from
            // far away cannot steer the search for long.
            if gradients - last_reset > 2 * count as c_int {
                last_reset = gradients;
            }
            if stalled && last_reset == gradients {
                break;
            }
        }
        if trace != 0 {
            print(&format!("final  value {best:.6} \n"));
            if iteration < maxit {
                print("converged\n");
            } else {
                print(&format!("stopped after {iteration} iterations\n"));
            }
        }
        *fail = c_int::from(iteration >= maxit);
        *fmin = best;
        *fncount = evaluations;
        *grcount = gradients;
        Ok(())
    })
}

/// The descent a step is judged against: the best value so far and the directional
/// derivative there.
#[derive(Clone, Copy)]
struct Descent {
    best: f64,
    slope: f64,
}

/// Nash's acceptable point search: shortens the step along `direction` from `origin` by a fifth
/// until the value falls by at least a ten-thousandth of the decrease the slope predicts.
/// Returns false when the step became too short to move any free parameter, measured against
/// 10 rather than against each parameter, so tiny parameters do not keep the search alive.
/// `value` keeps the last value evaluated, which callers read even when no new point was
/// tried.
#[allow(clippy::too_many_arguments)]
unsafe fn acceptable_point(
    objective: &Objective,
    parameters: &mut [f64],
    free: &[usize],
    origin: &[f64],
    direction: &[f64],
    descent: Descent,
    step: &mut f64,
    value: &mut f64,
    evaluations: &mut c_int,
    mark_rejections: bool,
) -> ApiResult<bool> {
    const SHRINK: f64 = 0.2;
    const SUFFICIENT_DECREASE: f64 = 0.0001;
    const MOVEMENT_REFERENCE: f64 = 10.0;
    loop {
        let mut unchanged = 0;
        for (position, &index) in free.iter().enumerate() {
            parameters[index] = origin[position] + *step * direction[position];
            if MOVEMENT_REFERENCE + origin[position] == MOVEMENT_REFERENCE + parameters[index] {
                unchanged += 1;
            }
        }
        if unchanged == free.len() {
            return Ok(false);
        }
        *value = objective.value(parameters)?;
        *evaluations += 1;
        if value.is_finite() && *value <= descent.best + descent.slope * *step * SUFFICIENT_DECREASE
        {
            return Ok(true);
        }
        *step *= SHRINK;
        if mark_rejections {
            print("*");
        }
    }
}

/// Nelder-Mead simplex minimization, Nash's Algorithm 19, with reflection `alpha`,
/// contraction `bet` and expansion `gamm`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmmin(
    n: c_int,
    bvec: *mut f64,
    x_out: *mut f64,
    fmin: *mut f64,
    fminfn: Option<OptimFn>,
    fail: *mut c_int,
    abstol: f64,
    intol: f64,
    ex: *mut c_void,
    alpha: f64,
    bet: f64,
    gamm: f64,
    trace: c_int,
    fncount: *mut c_int,
    maxit: c_int,
) {
    enter(|| {
        let objective = Objective::new(fminfn, None, ex)?;
        let size = n.max(0) as usize;
        let mut trial = read_vector(bvec, size);
        if maxit <= 0 {
            *fmin = objective.value(&mut trial)?;
            *fncount = 0;
            *fail = 0;
            return Ok(());
        }
        let finite_or_big = |value: f64| if value.is_finite() { value } else { BIG };
        if trace != 0 {
            print("  Nelder-Mead direct search function minimizer\n");
        }
        let start = objective.value(&mut trial)?;
        if !start.is_finite() {
            return Err(error("function cannot be evaluated at initial parameters"));
        }
        if trace != 0 {
            print(&format!(
                "function value for initial parameters = {start:.6}\n"
            ));
        }
        let mut evaluations: c_int = 1;
        let tolerance = intol * (start.abs() + intol);
        if trace != 0 {
            print(&format!(
                "  Scaled convergence tolerance is {}\n",
                format_g(tolerance)
            ));
        }
        // Vertex `v` occupies `simplex[v * size..(v + 1) * size]`; vertex 0 is the start.
        let vertices = size + 1;
        let mut simplex: Vec<f64> = (0..vertices).flat_map(|_| trial.iter().copied()).collect();
        let mut values = vec![0.0; vertices];
        values[0] = start;
        let mut step = 0.0;
        for &parameter in &trial {
            if 0.1 * parameter.abs() > step {
                step = 0.1 * parameter.abs();
            }
        }
        if step == 0.0 {
            step = 0.1;
        }
        if trace != 0 {
            print(&format!("Stepsize computed as {step:.6}\n"));
        }
        // Each vertex moves one coordinate away from the start. A coordinate so large that the
        // step vanishes in it gets ten times the step until it moves.
        let mut extent = 0.0;
        for vertex in 1..vertices {
            let coordinate = vertex - 1;
            let mut offset = step;
            while simplex[vertex * size + coordinate] == trial[coordinate] {
                simplex[vertex * size + coordinate] = trial[coordinate] + offset;
                offset *= 10.0;
            }
            extent += offset;
        }
        let mut previous_extent = extent;
        let mut stale_values = true;
        let mut lowest = 0;
        let mut action = "BUILD";
        let mut code = 0;
        let mut centroid = vec![0.0; size];
        loop {
            if stale_values {
                for vertex in 0..vertices {
                    if vertex != lowest {
                        trial.copy_from_slice(&simplex[vertex * size..(vertex + 1) * size]);
                        values[vertex] = finite_or_big(objective.value(&mut trial)?);
                        evaluations += 1;
                    }
                }
                stale_values = false;
            }
            let mut low_value = values[lowest];
            let mut high_value = low_value;
            let mut highest = lowest;
            for vertex in 0..vertices {
                if vertex != lowest {
                    let value = values[vertex];
                    if value < low_value {
                        lowest = vertex;
                        low_value = value;
                    }
                    if value > high_value {
                        highest = vertex;
                        high_value = value;
                    }
                }
            }
            if high_value <= low_value + tolerance || low_value <= abstol {
                break;
            }
            if trace != 0 {
                print(&format!(
                    "{action:<15}{evaluations:>5} {high_value:.6} {low_value:.6}\n"
                ));
            }
            let high = highest * size;
            for coordinate in 0..size {
                let mut sum = -simplex[high + coordinate];
                for vertex in 0..vertices {
                    sum += simplex[vertex * size + coordinate];
                }
                centroid[coordinate] = sum / size as f64;
            }
            for coordinate in 0..size {
                trial[coordinate] =
                    (1.0 + alpha) * centroid[coordinate] - alpha * simplex[high + coordinate];
            }
            let reflected = finite_or_big(objective.value(&mut trial)?);
            evaluations += 1;
            action = "REFLECTION";
            if reflected < low_value {
                // `centroid` takes the reflected point while `trial` moves on to the expansion.
                for coordinate in 0..size {
                    let expanded = gamm * trial[coordinate] + (1.0 - gamm) * centroid[coordinate];
                    centroid[coordinate] = trial[coordinate];
                    trial[coordinate] = expanded;
                }
                let expanded = finite_or_big(objective.value(&mut trial)?);
                evaluations += 1;
                if expanded < reflected {
                    simplex[high..high + size].copy_from_slice(&trial);
                    values[highest] = expanded;
                    action = "EXTENSION";
                } else {
                    simplex[high..high + size].copy_from_slice(&centroid);
                    values[highest] = reflected;
                }
            } else {
                action = "HI-REDUCTION";
                if reflected < high_value {
                    simplex[high..high + size].copy_from_slice(&trial);
                    values[highest] = reflected;
                    action = "LO-REDUCTION";
                }
                for coordinate in 0..size {
                    trial[coordinate] =
                        (1.0 - bet) * simplex[high + coordinate] + bet * centroid[coordinate];
                }
                let contracted = finite_or_big(objective.value(&mut trial)?);
                evaluations += 1;
                if contracted < values[highest] {
                    simplex[high..high + size].copy_from_slice(&trial);
                    values[highest] = contracted;
                } else if reflected >= high_value {
                    action = "SHRINK";
                    stale_values = true;
                    extent = 0.0;
                    let low = lowest * size;
                    for vertex in 0..vertices {
                        if vertex != lowest {
                            for coordinate in 0..size {
                                let anchor = simplex[low + coordinate];
                                let moved =
                                    bet * (simplex[vertex * size + coordinate] - anchor) + anchor;
                                simplex[vertex * size + coordinate] = moved;
                                extent += (moved - anchor).abs();
                            }
                        }
                    }
                    if extent < previous_extent {
                        previous_extent = extent;
                    } else {
                        if trace != 0 {
                            print("Polytope size measure not decreased in shrink\n");
                        }
                        code = 10;
                        break;
                    }
                }
            }
            if evaluations > maxit {
                break;
            }
        }
        if trace != 0 {
            print("Exiting from Nelder Mead minimizer\n");
            print(&format!("    {evaluations} function evaluations used\n"));
        }
        if code == 0 && evaluations > maxit {
            code = 1;
        }
        // `bvec` and `x_out` may be the same array, so both go through their raw pointers, the
        // answer last.
        write_vector(bvec, &trial);
        write_vector(x_out, &simplex[lowest * size..(lowest + 1) * size]);
        *fmin = values[lowest];
        *fncount = evaluations;
        *fail = code;
        Ok(())
    })
}

/// A copy of the caller's `length` doubles at `pointer`, taken so that no Rust reference into
/// a caller array outlives the copy: callers may pass the same array for input and output.
unsafe fn read_vector(pointer: *const f64, length: usize) -> Vec<f64> {
    if pointer.is_null() {
        return vec![0.0; length];
    }
    (0..length).map(|index| *pointer.add(index)).collect()
}

unsafe fn write_vector(pointer: *mut f64, values: &[f64]) {
    if pointer.is_null() {
        return;
    }
    for (index, &value) in values.iter().enumerate() {
        pointer.add(index).write(value);
    }
}

/// Conjugate gradients minimization, Nash's Algorithm 22: `type_` 1 is Fletcher-Reeves, 2
/// Polak-Ribiere and 3 Beale-Sorenson. GNU R accepts `abstol` here but never stops on it, so
/// neither does this.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cgmin(
    n: c_int,
    bvec: *mut f64,
    x_out: *mut f64,
    fmin: *mut f64,
    fminfn: Option<OptimFn>,
    fmingr: Option<OptimGr>,
    fail: *mut c_int,
    _abstol: f64,
    intol: f64,
    ex: *mut c_void,
    type_: c_int,
    trace: c_int,
    fncount: *mut c_int,
    grcount: *mut c_int,
    maxit: c_int,
) {
    enter(|| {
        let objective = Objective::new(fminfn, fmingr, ex)?;
        let size = n.max(0) as usize;
        let mut point = read_vector(bvec, size);
        if maxit <= 0 {
            *fmin = objective.value(&mut point)?;
            *fncount = 0;
            *grcount = 0;
            *fail = 0;
            return Ok(());
        }
        let method = match type_ {
            1 => "Fletcher Reeves",
            2 => "Polak Ribiere",
            3 => "Beale Sorenson",
            _ => return Err(error("unknown type in \"CG\" method of 'optim'")),
        };
        if trace != 0 {
            print("  Conjugate gradients function minimizer\n");
            print(&format!("Method: {method}\n"));
        }
        let tolerance = intol * size as f64 * intol.sqrt();
        if trace != 0 {
            print(&format!(
                "tolerance used in gradient test={}\n",
                format_g(tolerance)
            ));
        }
        let mut value = objective.value(&mut point)?;
        if !value.is_finite() {
            return Err(error("Function cannot be evaluated at initial parameters"));
        }
        let mut best = value;
        let mut evaluations: c_int = 1;
        let mut gradients: c_int = 0;
        let free: Vec<usize> = (0..size).collect();
        let mut origin = vec![0.0; size];
        let mut gradient = vec![0.0; size];
        let mut previous_gradient = vec![0.0; size];
        let mut direction = vec![0.0; size];
        let mut step = 1.0;
        loop {
            // Each restart begins with steepest descent.
            direction.fill(0.0);
            previous_gradient.fill(0.0);
            let mut cycle = 0;
            let mut initial_step = 1.0;
            let mut stalled = false;
            let mut gradient_measure;
            loop {
                cycle += 1;
                if trace != 0 {
                    print(&format!("{gradients} {evaluations} {best:.6}\n"));
                    let mut line = String::from("parameters ");
                    for parameter in &point {
                        line.push_str(&format!("{parameter:>10.5} "));
                    }
                    line.push('\n');
                    print(&line);
                }
                gradients += 1;
                if gradients > maxit {
                    write_vector(bvec, &point);
                    write_vector(x_out, &origin);
                    *fmin = best;
                    *fncount = evaluations;
                    *grcount = gradients;
                    *fail = 1;
                    return Ok(());
                }
                objective.gradient(&mut point, &mut gradient)?;
                gradient_measure = 0.0;
                let mut previous_measure = 0.0;
                for index in 0..size {
                    origin[index] = point[index];
                    let current = gradient[index];
                    let previous = previous_gradient[index];
                    match type_ {
                        1 => {
                            gradient_measure += current * current;
                            previous_measure += previous * previous;
                        }
                        2 => {
                            gradient_measure += current * (current - previous);
                            previous_measure += previous * previous;
                        }
                        _ => {
                            gradient_measure += current * (current - previous);
                            previous_measure += direction[index] * (current - previous);
                        }
                    }
                    previous_gradient[index] = current;
                }
                if gradient_measure > tolerance {
                    let conjugacy = if previous_measure > 0.0 {
                        gradient_measure / previous_measure
                    } else {
                        1.0
                    };
                    let mut slope = 0.0;
                    for index in 0..size {
                        direction[index] = direction[index] * conjugacy - gradient[index];
                        slope += direction[index] * gradient[index];
                    }
                    step = initial_step;
                    let moved = acceptable_point(
                        &objective,
                        &mut point,
                        &free,
                        &origin,
                        &direction,
                        Descent { best, slope },
                        &mut step,
                        &mut value,
                        &mut evaluations,
                        trace != 0,
                    )?;
                    stalled = !moved;
                    if moved {
                        best = value;
                        // GNU R fits Nash's parabola after `best` already holds the accepted
                        // value, which reduces the interpolation to trying half the accepted
                        // step. Matching that keeps counts and traces identical to GNU R.
                        let curvature = 2.0 * (value - best - slope * step);
                        if curvature > 0.0 {
                            let interpolated = -(slope * step * step / curvature);
                            for index in 0..size {
                                point[index] = origin[index] + interpolated * direction[index];
                            }
                            value = objective.value(&mut point)?;
                            evaluations += 1;
                            if value < best {
                                best = value;
                                if trace != 0 {
                                    print(" i< ");
                                }
                            } else {
                                if trace != 0 {
                                    print(" i> ");
                                }
                                for index in 0..size {
                                    point[index] = origin[index] + step * direction[index];
                                }
                            }
                        }
                    }
                }
                initial_step = (1.7 * step).min(1.0);
                if stalled || gradient_measure <= tolerance || cycle == size {
                    break;
                }
            }
            if cycle == 1 && (stalled || gradient_measure <= tolerance) {
                break;
            }
        }
        if trace != 0 {
            print("Exiting from conjugate gradients minimizer\n");
            print(&format!("    {evaluations} function evaluations used\n"));
            print(&format!("    {gradients} gradient evaluations used\n"));
        }
        write_vector(bvec, &point);
        write_vector(x_out, &point);
        *fmin = best;
        *fncount = evaluations;
        *grcount = gradients;
        *fail = 0;
        Ok(())
    })
}

/// Simulated annealing: Gaussian candidate steps scaled by the temperature, Metropolis
/// acceptance, and the logarithmic cooling schedule `ti / log(k + e - 1)`, each temperature
/// held for `tmax` evaluations. GNU R's version also reads a custom candidate generator out of
/// `ex` when `optim()` set one up; `ex` is opaque here, so the Gaussian step is always used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn samin(
    n: c_int,
    pb: *mut f64,
    yb: *mut f64,
    fminfn: Option<OptimFn>,
    maxit: c_int,
    tmax: c_int,
    ti: f64,
    trace: c_int,
    ex: *mut c_void,
) {
    enter(|| {
        let objective = Objective::new(fminfn, None, ex)?;
        if trace < 0 {
            return Err(error("trace, REPORT must be >= 0 (method = \"SANN\")"));
        }
        let best_point = vector(pb, n);
        if best_point.is_empty() {
            *yb = objective.value(best_point)?;
            return Ok(());
        }
        if ry_norm_rand_available() == 0 {
            return Err(error(
                "simulated annealing needs norm_rand, which this build of libR lacks",
            ));
        }
        read_seed()?;
        let finite_or_big = |value: f64| if value.is_finite() { value } else { BIG };
        let mut best = finite_or_big(objective.value(best_point)?);
        let mut current_point = best_point.to_vec();
        let mut current = best;
        let mut candidate = vec![0.0; current_point.len()];
        if trace != 0 {
            print("sann objective function values\n");
            print(&format!("initial       value {best:.6}\n"));
        }
        let scale = 1.0 / ti;
        let mut evaluation: c_int = 1;
        let mut temperature_index: c_int = 1;
        while evaluation < maxit {
            let temperature = ti / (f64::from(evaluation) + COOLING_OFFSET).ln();
            let mut held: c_int = 1;
            while held <= tmax && evaluation < maxit {
                let spread = scale * temperature;
                for (next, &position) in candidate.iter_mut().zip(&current_point) {
                    *next = position + spread * ry_norm_rand();
                }
                let value = finite_or_big(objective.value(&mut candidate)?);
                let rise = value - current;
                if rise <= 0.0 || runtime().rng.unif_rand() < (-rise / temperature).exp() {
                    current_point.copy_from_slice(&candidate);
                    current = value;
                    if current <= best {
                        best_point.copy_from_slice(&current_point);
                        best = current;
                    }
                }
                evaluation += 1;
                held += 1;
            }
            if trace != 0 && temperature_index % trace == 0 {
                print(&format!("iter {:>8} value {best:.6}\n", evaluation - 1));
            }
            temperature_index += 1;
        }
        if trace != 0 {
            print(&format!("final         value {best:.6}\n"));
            print(&format!(
                "sann stopped after {} iterations\n",
                evaluation - 1
            ));
        }
        *yb = best;
        write_seed()
    })
}

/// Belisle's schedule divides the starting temperature by `log(k + e - 1)` at the `k`th
/// evaluation that opens a temperature. GNU R writes `e - 1` to eight digits, which makes every
/// step about 1e-8 longer than the exact constant would; the shortened value reproduces GNU R's
/// stream under a given seed.
const COOLING_OFFSET: f64 = 1.7182818;

// QUADPACK's adaptive quadrature, with R's vectorized integrand: each rule hands all its
// abscissae to the integrand in one call.

/// An integrand and its opaque pointer.
#[derive(Clone, Copy)]
struct Integrand {
    function: IntegrFn,
    ex: *mut c_void,
}

impl Integrand {
    unsafe fn evaluate(&self, points: &mut [f64]) -> ApiResult<()> {
        callback(|| {
            ry_call_integrand(
                self.function,
                points.as_mut_ptr(),
                points.len() as c_int,
                self.ex,
            )
        })
    }
}

/// One application of a quadrature rule: the estimate, its error, the integral of `|f|`, and
/// the integral of `|f - mean|`.
#[derive(Clone, Copy, Default)]
struct RuleResult {
    result: f64,
    error: f64,
    absolute: f64,
    deviation: f64,
}

/// The 21-point Kronrod abscissae on [0, 1]; the odd-numbered ones (1-based even) are also the
/// 10-point Gauss abscissae.
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
    0.0,
];
const KRONROD21_WEIGHTS: [f64; 11] = [
    0.011694638867371874278064396062192,
    0.032558162307964727478818972459390,
    0.054755896574351996031381300244580,
    0.075039674810919952767043140916190,
    0.093125454583697605535065465083366,
    0.109387158802297641899210590325805,
    0.123491976262065851077208745815573,
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

/// The 15-point Kronrod abscissae on [0, 1] for the transformed infinite range.
const KRONROD15_NODES: [f64; 8] = [
    0.991455371120812639206854697526329,
    0.949107912342758524526189684047851,
    0.864864423359769072789712788640926,
    0.741531185599394439863864773280788,
    0.586087235467691130294144845693013,
    0.405845151377397166906606412076961,
    0.207784955007898467600689403773245,
    0.0,
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

/// The error estimate QUADPACK derives from the Gauss-Kronrod difference.
fn rule_error(difference: f64, absolute: f64, deviation: f64) -> f64 {
    let epsilon = f64::EPSILON;
    let mut error = difference;
    if deviation != 0.0 && error != 0.0 {
        error = deviation * (error * 200.0 / deviation).powf(1.5).min(1.0);
    }
    if absolute > f64::MIN_POSITIVE / (epsilon * 50.0) {
        error = error.max(epsilon * 50.0 * absolute);
    }
    error
}

/// QUADPACK's `dqk21` on [a, b].
unsafe fn kronrod21(integrand: Integrand, a: f64, b: f64) -> ApiResult<RuleResult> {
    let center = 0.5 * (a + b);
    let half = 0.5 * (b - a);
    let mut points = [0.0f64; 21];
    points[0] = center;
    for j in 1..=5 {
        let abscissa = half * KRONROD21_NODES[2 * j - 1];
        points[2 * j - 1] = center - abscissa;
        points[2 * j] = center + abscissa;
    }
    for j in 1..=5 {
        let abscissa = half * KRONROD21_NODES[2 * j - 2];
        points[2 * j + 9] = center - abscissa;
        points[2 * j + 10] = center + abscissa;
    }
    integrand.evaluate(&mut points)?;
    let fc = points[0];
    let mut gauss = 0.0;
    let mut kronrod = KRONROD21_WEIGHTS[10] * fc;
    let mut absolute = kronrod.abs();
    let mut left = [0.0f64; 10];
    let mut right = [0.0f64; 10];
    for j in 1..=5 {
        let node = 2 * j - 1;
        let (value1, value2) = (points[2 * j - 1], points[2 * j]);
        left[node] = value1;
        right[node] = value2;
        let sum = value1 + value2;
        gauss += GAUSS10_WEIGHTS[j - 1] * sum;
        kronrod += KRONROD21_WEIGHTS[node] * sum;
        absolute += KRONROD21_WEIGHTS[node] * (value1.abs() + value2.abs());
    }
    for j in 1..=5 {
        let node = 2 * j - 2;
        let (value1, value2) = (points[2 * j + 9], points[2 * j + 10]);
        left[node] = value1;
        right[node] = value2;
        let sum = value1 + value2;
        kronrod += KRONROD21_WEIGHTS[node] * sum;
        absolute += KRONROD21_WEIGHTS[node] * (value1.abs() + value2.abs());
    }
    let mean = kronrod * 0.5;
    let mut deviation = KRONROD21_WEIGHTS[10] * (fc - mean).abs();
    for j in 0..10 {
        deviation += KRONROD21_WEIGHTS[j] * ((left[j] - mean).abs() + (right[j] - mean).abs());
    }
    let length = half.abs();
    let absolute = absolute * length;
    let deviation = deviation * length;
    Ok(RuleResult {
        result: kronrod * half,
        error: rule_error(((kronrod - gauss) * half).abs(), absolute, deviation),
        absolute,
        deviation,
    })
}

/// QUADPACK's `dqk15i`: the 15-point rule on the transformed range `t in [a, b]` within
/// (0, 1], where `x = bound + direction (1 - t) / t`. For a doubly infinite range the
/// integrand is evaluated at `x` and `-x` in two calls and the values summed.
unsafe fn kronrod15_infinite(
    integrand: Integrand,
    bound: f64,
    infinity: c_int,
    a: f64,
    b: f64,
) -> ApiResult<RuleResult> {
    let direction = infinity.min(1) as f64;
    let center = 0.5 * (a + b);
    let half = 0.5 * (b - a);
    let transform = |t: f64| bound + direction * (1.0 - t) / t;
    let mut points = [0.0f64; 15];
    let mut mirrored = [0.0f64; 15];
    points[0] = transform(center);
    for j in 1..=7 {
        let abscissa = half * KRONROD15_NODES[j - 1];
        points[2 * j - 1] = transform(center - abscissa);
        points[2 * j] = transform(center + abscissa);
    }
    if infinity == 2 {
        for (mirror, &point) in mirrored.iter_mut().zip(&points) {
            *mirror = -point;
        }
    }
    integrand.evaluate(&mut points)?;
    if infinity == 2 {
        integrand.evaluate(&mut mirrored)?;
    }
    let value_at = |index: usize| {
        if infinity == 2 {
            points[index] + mirrored[index]
        } else {
            points[index]
        }
    };
    let fc = (value_at(0) / center) / center;
    let mut gauss = GAUSS7_WEIGHTS[7] * fc;
    let mut kronrod = KRONROD15_WEIGHTS[7] * fc;
    let mut absolute = kronrod.abs();
    let mut left = [0.0f64; 7];
    let mut right = [0.0f64; 7];
    for j in 1..=7 {
        let abscissa = half * KRONROD15_NODES[j - 1];
        let (t1, t2) = (center - abscissa, center + abscissa);
        let value1 = (value_at(2 * j - 1) / t1) / t1;
        let value2 = (value_at(2 * j) / t2) / t2;
        left[j - 1] = value1;
        right[j - 1] = value2;
        let sum = value1 + value2;
        gauss += GAUSS7_WEIGHTS[j - 1] * sum;
        kronrod += KRONROD15_WEIGHTS[j - 1] * sum;
        absolute += KRONROD15_WEIGHTS[j - 1] * (value1.abs() + value2.abs());
    }
    let mean = kronrod * 0.5;
    let mut deviation = KRONROD15_WEIGHTS[7] * (fc - mean).abs();
    for j in 0..7 {
        deviation += KRONROD15_WEIGHTS[j] * ((left[j] - mean).abs() + (right[j] - mean).abs());
    }
    let absolute = absolute * half;
    let deviation = deviation * half;
    Ok(RuleResult {
        result: kronrod * half,
        error: rule_error(((kronrod - gauss) * half).abs(), absolute, deviation),
        absolute,
        deviation,
    })
}

/// The table of Wynn's epsilon algorithm QUADPACK extrapolates with (`dqelg`).
struct Extrapolation {
    table: [f64; 52],
    /// The number of elements in `table`.
    count: usize,
    last_three: [f64; 3],
    calls: usize,
}

impl Extrapolation {
    fn new(first: f64) -> Extrapolation {
        let mut table = [0.0; 52];
        table[0] = first;
        Extrapolation {
            table,
            count: 2,
            last_three: [0.0; 3],
            calls: 0,
        }
    }

    /// Appends `value` and returns the extrapolated limit with its error estimate.
    fn push(&mut self, value: f64) -> (f64, f64) {
        self.count += 1;
        self.table[self.count - 1] = value;
        self.extrapolate()
    }

    fn extrapolate(&mut self) -> (f64, f64) {
        let epsilon = f64::EPSILON;
        let overflow = f64::MAX;
        let table = &mut self.table;
        let at = |index: usize| index - 1;
        self.calls += 1;
        let mut n = self.count;
        let mut error = overflow;
        let mut result = table[at(n)];
        'compute: {
            if n < 3 {
                break 'compute;
            }
            const LIMIT: usize = 50;
            table[at(n + 2)] = table[at(n)];
            let new_elements = (n - 1) / 2;
            table[at(n)] = overflow;
            let original = n;
            let mut k1 = n;
            for i in 1..=new_elements {
                let k2 = k1 - 1;
                let k3 = k1 - 2;
                let res = table[at(k1 + 2)];
                let e0 = table[at(k3)];
                let e1 = table[at(k2)];
                let e2 = res;
                let e1_magnitude = e1.abs();
                let delta2 = e2 - e1;
                let error2 = delta2.abs();
                let tolerance2 = e2.abs().max(e1_magnitude) * epsilon;
                let delta3 = e1 - e0;
                let error3 = delta3.abs();
                let tolerance3 = e1_magnitude.max(e0.abs()) * epsilon;
                if error2 <= tolerance2 && error3 <= tolerance3 {
                    // e0, e1 and e2 agree to machine accuracy: convergence.
                    result = res;
                    error = error2 + error3;
                    error = error.max(5.0 * epsilon * result.abs());
                    self.count = n;
                    return (result, error);
                }
                let e3 = table[at(k1)];
                table[at(k1)] = e1;
                let delta1 = e1 - e3;
                let error1 = delta1.abs();
                let tolerance1 = e1_magnitude.max(e3.abs()) * epsilon;
                let mut irregular =
                    error1 <= tolerance1 || error2 <= tolerance2 || error3 <= tolerance3;
                let mut ss = 0.0;
                if !irregular {
                    ss = 1.0 / delta1 + 1.0 / delta2 - 1.0 / delta3;
                    irregular = (ss * e1).abs() <= 1e-4;
                }
                if irregular {
                    n = i + i - 1;
                    break;
                }
                let res = e1 + 1.0 / ss;
                table[at(k1)] = res;
                k1 -= 2;
                let candidate_error = error2 + (res - e2).abs() + error3;
                if candidate_error <= error {
                    error = candidate_error;
                    result = res;
                }
            }
            if n == LIMIT {
                n = 2 * (LIMIT / 2) - 1;
            }
            let mut ib = if original.is_multiple_of(2) { 2 } else { 1 };
            for _ in 1..=new_elements + 1 {
                let ib2 = ib + 2;
                table[at(ib)] = table[at(ib2)];
                ib = ib2;
            }
            if original != n {
                for (i, source) in (1..=n).zip(original - n + 1..) {
                    table[at(i)] = table[at(source)];
                }
            }
            if self.calls < 4 {
                self.last_three[self.calls - 1] = result;
                error = overflow;
            } else {
                error = (result - self.last_three[2]).abs()
                    + (result - self.last_three[1]).abs()
                    + (result - self.last_three[0]).abs();
                self.last_three = [self.last_three[1], self.last_three[2], result];
            }
        }
        self.count = n;
        (result, error.max(5.0 * epsilon * result.abs()))
    }
}

/// The subintervals of an adaptive integration, kept in the caller's `work` and `iwork` as
/// QUADPACK lays them out: left ends, right ends, estimates, errors, and the 1-based order of
/// the errors from largest down.
struct Subintervals<'a> {
    left: &'a mut [f64],
    right: &'a mut [f64],
    estimate: &'a mut [f64],
    error: &'a mut [f64],
    order: &'a mut [c_int],
}

impl Subintervals<'_> {
    /// QUADPACK's `dqpsrt`: keeps `order` descending by error after interval `maxerr` was
    /// bisected into `maxerr` and `last`. Returns the new `maxerr`, its error, and `nrmax`.
    fn sort(
        &mut self,
        limit: usize,
        last: usize,
        maxerr: usize,
        nrmax: &mut usize,
    ) -> (usize, f64) {
        let order = &mut *self.order;
        let error = &*self.error;
        let at = |index: usize| index - 1;
        let element = |order: &[c_int], index: usize| order[at(index)] as usize;
        'insert: {
            if last <= 2 {
                order[0] = 1;
                order[1] = 2;
                break 'insert;
            }
            let largest = error[at(maxerr)];
            if *nrmax != 1 {
                for _ in 1..*nrmax {
                    let successor = element(order, *nrmax - 1);
                    if largest <= error[at(successor)] {
                        break;
                    }
                    order[at(*nrmax)] = successor as c_int;
                    *nrmax -= 1;
                }
            }
            let upper = if last > limit / 2 + 2 {
                limit + 3 - last
            } else {
                last
            };
            let smallest = error[at(last)];
            let bound = upper - 1;
            let start = *nrmax + 1;
            let mut position = start;
            while position <= bound {
                let successor = element(order, position);
                if largest >= error[at(successor)] {
                    break;
                }
                order[at(position - 1)] = successor as c_int;
                position += 1;
            }
            if position > bound {
                order[at(bound)] = maxerr as c_int;
                order[at(upper)] = last as c_int;
                break 'insert;
            }
            order[at(position - 1)] = maxerr as c_int;
            let mut k = bound;
            for _ in position..=bound {
                let successor = element(order, k);
                if smallest < error[at(successor)] {
                    order[at(k + 1)] = last as c_int;
                    break 'insert;
                }
                order[at(k + 1)] = successor as c_int;
                k -= 1;
            }
            order[at(position)] = last as c_int;
        }
        let maxerr = element(order, *nrmax);
        (maxerr, error[at(maxerr)])
    }
}

/// What an adaptive integration reports.
struct Integration {
    result: f64,
    error: f64,
    status: c_int,
    last: usize,
}

/// QUADPACK's `dqagse` and `dqagie`, which differ only in the rule and the starting interval:
/// bisect the interval with the largest error until the sum of errors meets the tolerance,
/// extrapolating the sequence of sums with the epsilon algorithm when bisection alone stalls.
unsafe fn adaptive(
    rule: &mut dyn FnMut(f64, f64) -> ApiResult<RuleResult>,
    a: f64,
    b: f64,
    absolute_tolerance: f64,
    relative_tolerance: f64,
    limit: usize,
    intervals: &mut Subintervals,
) -> ApiResult<Integration> {
    let epsilon = f64::EPSILON;
    let underflow = f64::MIN_POSITIVE;
    let overflow = f64::MAX;
    let at = |index: usize| index - 1;
    intervals.left[0] = a;
    intervals.right[0] = b;
    intervals.estimate[0] = 0.0;
    intervals.error[0] = 0.0;
    let mut outcome = Integration {
        result: 0.0,
        error: 0.0,
        status: 0,
        last: 0,
    };
    if absolute_tolerance <= 0.0 && relative_tolerance < (50.0 * epsilon).max(0.5e-28) {
        outcome.status = 6;
        return Ok(outcome);
    }
    let first = rule(a, b)?;
    let mut result = first.result;
    let mut abserr = first.error;
    let defabs = first.absolute;
    let resabs = first.deviation;
    let dres = result.abs();
    let mut errbnd = absolute_tolerance.max(relative_tolerance * dres);
    let mut last = 1;
    intervals.estimate[0] = result;
    intervals.error[0] = abserr;
    intervals.order[0] = 1;
    let mut ier = 0;
    if abserr <= 100.0 * epsilon * defabs && abserr > errbnd {
        ier = 2;
    }
    if limit == 1 {
        ier = 1;
    }
    if ier != 0 || (abserr <= errbnd && abserr != resabs) || abserr == 0.0 {
        return Ok(Integration {
            result,
            error: abserr,
            status: ier,
            last,
        });
    }
    let mut extrapolation = Extrapolation::new(result);
    let mut errmax = abserr;
    let mut maxerr = 1;
    let mut area = result;
    let mut errsum = abserr;
    abserr = overflow;
    let mut nrmax = 1;
    let mut ktmin = 0;
    let mut extrapolating = false;
    let mut no_extrapolation = false;
    let mut ierro = 0;
    let (mut iroff1, mut iroff2, mut iroff3) = (0, 0, 0);
    let ksgn = if dres >= (1.0 - 50.0 * epsilon) * defabs {
        1
    } else {
        -1
    };
    let mut small = 0.0;
    let mut erlarg = 0.0;
    let mut ertest = 0.0;
    let mut correc = 0.0;
    // Whether the loop ended by summing the subintervals rather than by extrapolation.
    let mut summed = false;
    last = 2;
    while last <= limit {
        let a1 = intervals.left[at(maxerr)];
        let b1 = 0.5 * (intervals.left[at(maxerr)] + intervals.right[at(maxerr)]);
        let a2 = b1;
        let b2 = intervals.right[at(maxerr)];
        let erlast = errmax;
        let first_half = rule(a1, b1)?;
        let second_half = rule(a2, b2)?;
        let (area1, error1, defab1) = (first_half.result, first_half.error, first_half.deviation);
        let (area2, error2, defab2) =
            (second_half.result, second_half.error, second_half.deviation);
        let area12 = area1 + area2;
        let erro12 = error1 + error2;
        errsum = errsum + erro12 - errmax;
        area = area + area12 - intervals.estimate[at(maxerr)];
        if defab1 != error1 && defab2 != error2 {
            if (intervals.estimate[at(maxerr)] - area12).abs() <= 1e-5 * area12.abs()
                && erro12 >= 0.99 * errmax
            {
                if extrapolating {
                    iroff2 += 1;
                } else {
                    iroff1 += 1;
                }
            }
            if last > 10 && erro12 > errmax {
                iroff3 += 1;
            }
        }
        intervals.estimate[at(maxerr)] = area1;
        intervals.estimate[at(last)] = area2;
        errbnd = absolute_tolerance.max(relative_tolerance * area.abs());
        if iroff1 + iroff2 >= 10 || iroff3 >= 20 {
            ier = 2;
        }
        if iroff2 >= 5 {
            ierro = 3;
        }
        if last == limit {
            ier = 1;
        }
        if a1.abs().max(b2.abs()) <= (1.0 + 100.0 * epsilon) * (a2.abs() + 1000.0 * underflow) {
            ier = 4;
        }
        if error2 > error1 {
            intervals.left[at(maxerr)] = a2;
            intervals.left[at(last)] = a1;
            intervals.right[at(last)] = b1;
            intervals.estimate[at(maxerr)] = area2;
            intervals.estimate[at(last)] = area1;
            intervals.error[at(maxerr)] = error2;
            intervals.error[at(last)] = error1;
        } else {
            intervals.left[at(last)] = a2;
            intervals.right[at(maxerr)] = b1;
            intervals.right[at(last)] = b2;
            intervals.error[at(maxerr)] = error1;
            intervals.error[at(last)] = error2;
        }
        (maxerr, errmax) = intervals.sort(limit, last, maxerr, &mut nrmax);
        if errsum <= errbnd {
            summed = true;
            break;
        }
        if ier != 0 {
            break;
        }
        if last == 2 {
            small = (b - a).abs() * 0.375;
            erlarg = errsum;
            ertest = errbnd;
            extrapolation.table[1] = area;
            last += 1;
            continue;
        }
        if no_extrapolation {
            last += 1;
            continue;
        }
        erlarg -= erlast;
        if (b1 - a1).abs() > small {
            erlarg += erro12;
        }
        if !extrapolating {
            if (intervals.right[at(maxerr)] - intervals.left[at(maxerr)]).abs() > small {
                last += 1;
                continue;
            }
            extrapolating = true;
            nrmax = 2;
        }
        let mut keep_bisecting = false;
        if ierro != 3 && erlarg > ertest {
            // Bisect the large intervals before extrapolating again.
            let upper = if last > 2 + limit / 2 {
                limit + 3 - last
            } else {
                last
            };
            let first = nrmax;
            for _ in first..=upper {
                maxerr = intervals.order[at(nrmax)] as usize;
                errmax = intervals.error[at(maxerr)];
                if (intervals.right[at(maxerr)] - intervals.left[at(maxerr)]).abs() > small {
                    keep_bisecting = true;
                    break;
                }
                nrmax += 1;
            }
        }
        if keep_bisecting {
            last += 1;
            continue;
        }
        let (reseps, abseps) = extrapolation.push(area);
        ktmin += 1;
        if ktmin > 5 && abserr < 1e-3 * errsum {
            ier = 5;
        }
        if abseps < abserr {
            ktmin = 0;
            abserr = abseps;
            result = reseps;
            correc = erlarg;
            ertest = absolute_tolerance.max(relative_tolerance * reseps.abs());
            if abserr <= ertest {
                break;
            }
        }
        if extrapolation.count == 1 {
            no_extrapolation = true;
        }
        if ier == 5 {
            break;
        }
        maxerr = intervals.order[0] as usize;
        errmax = intervals.error[at(maxerr)];
        nrmax = 1;
        extrapolating = false;
        small *= 0.5;
        erlarg = errsum;
        last += 1;
    }
    let last = last.min(limit);
    // Choose between the extrapolated result and the plain sum of the subintervals.
    'finish: {
        if !summed {
            if abserr == overflow {
                summed = true;
                break 'finish;
            }
            if ier + ierro != 0 {
                if ierro == 3 {
                    abserr += correc;
                }
                if ier == 0 {
                    ier = 3;
                }
                if result != 0.0 && area != 0.0 {
                    if abserr / result.abs() > errsum / area.abs() {
                        summed = true;
                        break 'finish;
                    }
                } else if abserr > errsum {
                    summed = true;
                    break 'finish;
                } else if area == 0.0 {
                    break 'finish;
                }
            }
            if !(ksgn == -1 && result.abs().max(area.abs()) <= defabs * 0.01)
                && (0.01 > result / area || result / area > 100.0 || errsum > area.abs())
            {
                ier = 6;
            }
        }
    }
    if summed {
        result = intervals.estimate[..last].iter().sum();
        abserr = errsum;
    }
    if ier > 2 {
        ier -= 1;
    }
    Ok(Integration {
        result,
        error: abserr,
        status: ier,
        last,
    })
}

/// Splits the caller's work arrays into the subinterval table, or `None` when they are too
/// small for `limit` intervals.
unsafe fn subintervals<'a>(
    limit: *const c_int,
    lenw: *const c_int,
    iwork: *mut c_int,
    work: *mut f64,
) -> Option<(usize, Subintervals<'a>)> {
    let limit = *limit;
    if limit < 1 || i64::from(*lenw) < i64::from(limit) * 4 {
        return None;
    }
    let limit = limit as usize;
    let work = std::slice::from_raw_parts_mut(work, 4 * limit);
    let (left, rest) = work.split_at_mut(limit);
    let (right, rest) = rest.split_at_mut(limit);
    let (estimate, error) = rest.split_at_mut(limit);
    let order = std::slice::from_raw_parts_mut(iwork, limit);
    Some((
        limit,
        Subintervals {
            left,
            right,
            estimate,
            error,
            order,
        },
    ))
}

/// Integrates `f` over the finite interval `[a, b]` (QUADPACK `dqags`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rdqags(
    f: Option<IntegrFn>,
    ex: *mut c_void,
    a: *mut f64,
    b: *mut f64,
    epsabs: *mut f64,
    epsrel: *mut f64,
    result: *mut f64,
    abserr: *mut f64,
    neval: *mut c_int,
    ier: *mut c_int,
    limit: *mut c_int,
    lenw: *mut c_int,
    last: *mut c_int,
    iwork: *mut c_int,
    work: *mut f64,
) {
    enter(|| {
        *ier = 6;
        *neval = 0;
        *last = 0;
        *result = 0.0;
        *abserr = 0.0;
        let Some((limit, mut intervals)) = subintervals(limit, lenw, iwork, work) else {
            return Ok(());
        };
        let function = f.ok_or_else(|| error("the integrand is NULL"))?;
        let integrand = Integrand { function, ex };
        let mut rule = |low: f64, high: f64| kronrod21(integrand, low, high);
        let outcome = adaptive(&mut rule, *a, *b, *epsabs, *epsrel, limit, &mut intervals)?;
        *result = outcome.result;
        *abserr = outcome.error;
        *ier = outcome.status;
        *last = outcome.last as c_int;
        *neval = if outcome.last == 0 {
            0
        } else {
            42 * outcome.last as c_int - 21
        };
        Ok(())
    })
}

/// Integrates `f` over `(bound, Inf)` (`inf = 1`), `(-Inf, bound)` (`inf = -1`) or the whole
/// line (`inf = 2`) by mapping the range onto (0, 1] (QUADPACK `dqagi`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rdqagi(
    f: Option<IntegrFn>,
    ex: *mut c_void,
    bound: *mut f64,
    inf: *mut c_int,
    epsabs: *mut f64,
    epsrel: *mut f64,
    result: *mut f64,
    abserr: *mut f64,
    neval: *mut c_int,
    ier: *mut c_int,
    limit: *mut c_int,
    lenw: *mut c_int,
    last: *mut c_int,
    iwork: *mut c_int,
    work: *mut f64,
) {
    enter(|| {
        *ier = 6;
        *neval = 0;
        *last = 0;
        *result = 0.0;
        *abserr = 0.0;
        let Some((limit, mut intervals)) = subintervals(limit, lenw, iwork, work) else {
            return Ok(());
        };
        let function = f.ok_or_else(|| error("the integrand is NULL"))?;
        let integrand = Integrand { function, ex };
        let infinity = *inf;
        let start = if infinity == 2 { 0.0 } else { *bound };
        let mut rule =
            |low: f64, high: f64| kronrod15_infinite(integrand, start, infinity, low, high);
        let outcome = adaptive(&mut rule, 0.0, 1.0, *epsabs, *epsrel, limit, &mut intervals)?;
        *result = outcome.result;
        *abserr = outcome.error;
        *ier = outcome.status;
        *last = outcome.last as c_int;
        let mut evaluations = if outcome.last == 0 {
            0
        } else {
            30 * outcome.last as c_int - 15
        };
        if infinity == 2 {
            evaluations *= 2;
        }
        *neval = evaluations;
        Ok(())
    })
}

// Interval search.

/// The index `i` of the interval of the sorted `xt[0..n]` holding `x`: `xt[i-1] <= x <
/// xt[i]` in 1-based terms, or with the inequalities flipped when `left_open`. `mflag` is -1
/// left of the knots, 1 right of them and 0 inside. The hint `ilo` only speeds GNU R's search,
/// so a binary search gives the same answer without it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn findInterval2(
    xt: *mut f64,
    n: c_int,
    x: f64,
    rightmost_closed: c_int,
    all_inside: c_int,
    left_open: c_int,
    _ilo: c_int,
    mflag: *mut c_int,
) -> c_int {
    let knots = vector(xt, n);
    let (index, flag) = find_interval(
        knots,
        x,
        rightmost_closed != 0,
        all_inside != 0,
        left_open != 0,
    );
    if !mflag.is_null() {
        *mflag = flag;
    }
    index
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn findInterval(
    xt: *mut f64,
    n: c_int,
    x: f64,
    rightmost_closed: c_int,
    all_inside: c_int,
    ilo: c_int,
    mflag: *mut c_int,
) -> c_int {
    findInterval2(xt, n, x, rightmost_closed, all_inside, 0, ilo, mflag)
}

/// `findInterval` for Fortran callers, every argument by reference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn interv_(
    xt: *mut f64,
    n: *const c_int,
    x: *const f64,
    rightmost_closed: *const c_int,
    all_inside: *const c_int,
    ilo: *const c_int,
    mflag: *mut c_int,
) -> c_int {
    findInterval(xt, *n, *x, *rightmost_closed, *all_inside, *ilo, mflag)
}

pub fn find_interval(
    knots: &[f64],
    x: f64,
    rightmost_closed: bool,
    all_inside: bool,
    left_open: bool,
) -> (c_int, c_int) {
    let n = knots.len();
    if n == 0 {
        return (0, 0);
    }
    let (first, last) = (knots[0], knots[n - 1]);
    // The number of knots at or below x (right-continuous) or strictly below it (left-open).
    let count = if left_open {
        knots.partition_point(|&knot| knot < x)
    } else {
        knots.partition_point(|&knot| knot <= x)
    } as c_int;
    let n = n as c_int;
    if count == 0 {
        let inside = all_inside || (left_open && rightmost_closed && x == first);
        return (inside as c_int, -1);
    }
    if count == n {
        let inside = all_inside || (!left_open && rightmost_closed && x == last);
        return (if inside { n - 1 } else { n }, 1);
    }
    (count, 0)
}

/// The column of each row's maximum, 1-based, or `NA` for a row with a missing value. Ties
/// within a relative 1e-5 of the row's largest magnitude break at random (`ties_meth` 1),
/// to the first (2) or to the last (3).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_max_col(
    matrix: *mut f64,
    nr: *mut c_int,
    nc: *mut c_int,
    maxes: *mut c_int,
    ties_meth: *mut c_int,
) {
    enter(|| {
        let rows = (*nr).max(0) as usize;
        let columns = (*nc).max(0) as usize;
        let method = *ties_meth;
        let random = method == 1;
        if random {
            read_seed()?;
        }
        let entry = |row: usize, column: usize| *matrix.add(row + column * rows);
        for row in 0..rows {
            let mut missing = false;
            let mut largest = 0.0f64;
            for column in 0..columns {
                let value = entry(row, column);
                if value.is_nan() {
                    missing = true;
                    break;
                }
                if value.is_finite() {
                    largest = largest.max(value.abs());
                }
            }
            if missing || columns == 0 {
                *maxes.add(row) = NA_INTEGER;
                continue;
            }
            let mut chosen = 0;
            let mut maximum = entry(row, 0);
            if random {
                // Entries are treated as probabilities: values this close to the maximum tie.
                let tolerance = 1e-5 * largest;
                let mut ties = 1;
                for column in 1..columns {
                    let value = entry(row, column);
                    if value > maximum + tolerance {
                        maximum = value;
                        chosen = column;
                        ties = 1;
                    } else if value >= maximum - tolerance {
                        // Reservoir sampling: each of the tied columns ends up chosen with
                        // equal probability.
                        ties += 1;
                        if f64::from(ties) * runtime().rng.unif_rand() < 1.0 {
                            chosen = column;
                        }
                    }
                }
            } else {
                let last = method == 3;
                for column in 1..columns {
                    let value = entry(row, column);
                    if value > maximum || (last && value == maximum) {
                        maximum = value;
                        chosen = column;
                    }
                }
            }
            *maxes.add(row) = chosen as c_int + 1;
        }
        if random {
            write_seed()?;
        }
        Ok(())
    })
}

/// The tick spacing for `pretty()`: a "nice" unit (1, 2 or 5 times a power of ten) dividing
/// `[lo, up]` into about `ndiv` cells, at least `min_n`. On return `ndiv` is the number of
/// cells and `lo`, `up` are the covering bounds (`return_bounds`) or the bounds in units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_pretty(
    lo: *mut f64,
    up: *mut f64,
    ndiv: *mut c_int,
    min_n: c_int,
    shrink_sml: f64,
    high_u_fact: *const f64,
    eps_correction: c_int,
    return_bounds: c_int,
) -> f64 {
    // Unit counts within this fraction of an integer count as that integer.
    const SNAP: f64 = 1e-10;
    // The largest cell kept is the largest double over this factor, so that rounding the
    // cell up to the next nice unit cannot overflow.
    const HEADROOM: f64 = 1.25;
    let high_bias = *high_u_fact;
    let five_bias = *high_u_fact.add(1);
    let smallest_factor = *high_u_fact.add(2);
    let divisions = *ndiv;
    let range = *up - *lo;
    // A range that ends at zero is padded away from zero only, as GNU R does.
    let starts_at_zero = *lo == 0.0 && *up != 0.0;
    let ends_at_zero = *up == 0.0 && *lo != 0.0;
    let mut cell;
    let small;
    if range == 0.0 && *up == 0.0 {
        cell = 1.0;
        small = true;
    } else {
        cell = lo.read().abs().max(up.read().abs());
        // A range this narrow next to the magnitudes is rounding noise, not data.
        let bias = if five_bias >= 1.5 * high_bias + 0.5 {
            1.0 / (1.0 + high_bias)
        } else {
            1.5 / (1.0 + five_bias)
        };
        let noise = (1.0 + bias) * f64::from(divisions.max(1)) * f64::EPSILON;
        small = range < cell * noise * 3.0;
    }
    if small {
        if cell > 10.0 {
            cell = 9.0 + cell / 10.0;
        }
        cell *= shrink_sml;
        if min_n > 1 {
            cell /= f64::from(min_n);
        }
    } else {
        cell = range;
        if range.is_infinite() {
            if divisions > 1 {
                // The range overflows, its parts do not.
                cell = *up / f64::from(divisions) - *lo / f64::from(divisions);
            } else {
                warn(format!(
                    "R_pretty(): infinite range; *ndiv={divisions}, should have ndiv >= 2"
                ));
            }
        } else if divisions > 1 {
            cell /= f64::from(divisions);
        }
    }
    let mut smallest_cell = smallest_factor * f64::MIN_POSITIVE;
    if smallest_cell == 0.0 {
        smallest_cell = f64::MIN_POSITIVE;
    }
    if cell < smallest_cell {
        warn(format!(
            "R_pretty(): very small range 'cell'={}, corrected to {}",
            format_g(cell),
            format_g(smallest_cell)
        ));
        cell = smallest_cell;
    } else if cell > f64::MAX / HEADROOM {
        warn(format!(
            "R_pretty(): very large range 'cell'={}, corrected to {}",
            format_g(cell),
            format_g(f64::MAX / HEADROOM)
        ));
        cell = f64::MAX / HEADROOM;
    }
    let base = 10f64.powf(cell.log10().floor());
    let mut unit = base;
    let two = 2.0 * base;
    if two - cell < high_bias * (cell - unit) {
        unit = two;
        let five = 5.0 * base;
        if five - cell < five_bias * (cell - unit) {
            unit = five;
            let ten = 10.0 * base;
            if ten - cell < high_bias * (cell - unit) {
                unit = ten;
            }
        }
    }
    let mut start = (*lo / unit + SNAP).floor();
    let mut end = (*up / unit - SNAP).ceil();
    if eps_correction != 0 && (eps_correction > 1 || !small) {
        *lo = if *lo != 0.0 {
            *lo * (1.0 - f64::EPSILON)
        } else {
            -f64::MIN_POSITIVE
        };
        *up = if *up != 0.0 {
            *up * (1.0 + f64::EPSILON)
        } else {
            f64::MIN_POSITIVE
        };
    }
    while start * unit > *lo + SNAP * unit {
        start -= 1.0;
    }
    while end * unit < *up - SNAP * unit {
        end += 1.0;
    }
    // Near the ends of the double range a covering count can overflow; step back inside.
    while !(start * unit).is_finite() {
        start += 1.0;
    }
    while !(end * unit).is_finite() {
        end -= 1.0;
    }
    let mut cells = (0.5 + end - start) as c_int;
    if cells < min_n {
        let missing = min_n - cells;
        if starts_at_zero {
            end += f64::from(missing);
        } else if ends_at_zero {
            start -= f64::from(missing);
        } else if start >= 0.0 {
            end += f64::from(missing / 2);
            start -= f64::from(missing / 2 + missing % 2);
        } else {
            start -= f64::from(missing / 2);
            end += f64::from(missing / 2 + missing % 2);
        }
        cells = min_n;
    }
    *ndiv = cells;
    if return_bounds != 0 {
        if start * unit < *lo {
            *lo = start * unit;
        }
        if end * unit > *up {
            *up = end * unit;
        }
    } else {
        *lo = start;
        *up = end;
    }
    unit
}

// Dennis and Schnabel's UNCMIN ("Numerical Methods for Unconstrained Optimization and
// Nonlinear Equations", 1983, Appendix A), the engine of nlm(): a quasi-Newton method with a
// line search (method 1), a double dogleg trust region (2) or a More-Hebdon trust region (3),
// and an analytic, finite difference or secant Hessian.

/// The problem `optif9` minimizes: the objective, its optional analytic derivatives, and the
/// state pointer all three receive.
#[derive(Clone, Copy)]
struct Problem {
    function: FcnP,
    gradient: Option<FcnP>,
    hessian: Option<D2FcnP>,
    state: *mut c_void,
}

impl Problem {
    unsafe fn value(&self, x: &mut [f64]) -> ApiResult<f64> {
        let mut value = 0.0;
        callback(|| {
            ry_call_fcn(
                self.function,
                x.len() as c_int,
                x.as_mut_ptr(),
                &mut value,
                self.state,
            )
        })?;
        Ok(value)
    }

    unsafe fn gradient(&self, x: &mut [f64], gradient: &mut [f64]) -> ApiResult<()> {
        let function = self
            .gradient
            .ok_or_else(|| error("the analytic gradient function is NULL"))?;
        callback(|| {
            ry_call_fcn(
                function,
                x.len() as c_int,
                x.as_mut_ptr(),
                gradient.as_mut_ptr(),
                self.state,
            )
        })
    }

    unsafe fn hessian(&self, a: Dense, x: &mut [f64]) -> ApiResult<()> {
        let function = self
            .hessian
            .ok_or_else(|| error("the analytic Hessian function is NULL"))?;
        callback(|| {
            ry_call_d2fcn(
                function,
                a.rows as c_int,
                x.len() as c_int,
                x.as_mut_ptr(),
                a.base,
                self.state,
            )
        })
    }
}

/// A column-major matrix with `rows` rows, addressed 0-based as UNCMIN's C translation is.
#[derive(Clone, Copy)]
struct Dense {
    base: *mut f64,
    rows: usize,
}

impl Dense {
    unsafe fn get(self, row: usize, column: usize) -> f64 {
        *self.base.add(row + column * self.rows)
    }

    unsafe fn set(self, row: usize, column: usize, value: f64) {
        *self.base.add(row + column * self.rows) = value;
    }

    unsafe fn update(self, row: usize, column: usize, change: impl FnOnce(f64) -> f64) {
        let cell = self.base.add(row + column * self.rows);
        *cell = change(*cell);
    }

    fn linpack(self) -> Matrix {
        Matrix::new(self.base, self.rows as c_int)
    }
}

/// The trust region state the More-Hebdon step carries between iterations.
#[derive(Clone, Copy, Default)]
struct HookState {
    amu: f64,
    dltp: f64,
    phi: f64,
    phip0: f64,
    phip: f64,
}

/// `optif9`'s scalar arguments, normalized by `optchk`.
#[derive(Clone, Copy)]
struct Settings {
    fscale: f64,
    method: c_int,
    iexp: c_int,
    ndigit: c_int,
    itnlim: c_int,
    iagflg: c_int,
    iahflg: c_int,
    dlt: f64,
    gradtl: f64,
    stepmx: f64,
    steptl: f64,
}

/// The unconstrained minimizer behind `nlm()`. `msg` is a bit set on input (1: allow `n == 1`;
/// 2: skip the analytic gradient check; 4: skip the analytic Hessian check; 8: print nothing;
/// 16: print every iteration) and a negative error code on output. `itrmcd` says why it
/// stopped: 1 small gradient, 2 small step, 3 no lower point found, 4 iteration limit, 5 five
/// consecutive maximal steps.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn optif9(
    nr: c_int,
    n: c_int,
    x: *mut f64,
    fcn: Option<FcnP>,
    d1fcn: Option<FcnP>,
    d2fcn: Option<D2FcnP>,
    state: *mut c_void,
    typsiz: *mut f64,
    fscale: f64,
    method: c_int,
    iexp: c_int,
    msg: *mut c_int,
    ndigit: c_int,
    itnlim: c_int,
    iagflg: c_int,
    iahflg: c_int,
    dlt: f64,
    gradtl: f64,
    stepmx: f64,
    steptl: f64,
    xpls: *mut f64,
    fpls: *mut f64,
    gpls: *mut f64,
    itrmcd: *mut c_int,
    a: *mut f64,
    _wrk: *mut f64,
    itncnt: *mut c_int,
) {
    enter(|| {
        let function = fcn.ok_or_else(|| error("the objective function is NULL"))?;
        let problem = Problem {
            function,
            gradient: d1fcn,
            hessian: d2fcn,
            state,
        };
        let settings = Settings {
            fscale,
            method,
            iexp,
            ndigit,
            itnlim,
            iagflg,
            iahflg,
            dlt,
            gradtl,
            stepmx,
            steptl,
        };
        let size = n.max(0);
        // C callers may pass the same array for `x` and `xpls`, so the start point is copied
        // before `xpls` becomes a `&mut` slice.
        let mut start = vector(x, size).to_vec();
        let outputs = Outputs {
            xpls: vector(xpls, size),
            fpls: &mut *fpls,
            gpls: vector(gpls, size),
            itrmcd: &mut *itrmcd,
            itncnt: &mut *itncnt,
            msg: &mut *msg,
        };
        let a = Dense {
            base: a,
            rows: nr.max(0) as usize,
        };
        uncmin(
            problem,
            a,
            n,
            &mut start,
            vector(typsiz, size),
            settings,
            outputs,
        )
    })
}

/// Where `optif9` reports.
struct Outputs<'a> {
    xpls: &'a mut [f64],
    fpls: &'a mut f64,
    gpls: &'a mut [f64],
    itrmcd: &'a mut c_int,
    itncnt: &'a mut c_int,
    msg: &'a mut c_int,
}

unsafe fn uncmin(
    problem: Problem,
    a: Dense,
    n_declared: c_int,
    x: &mut [f64],
    typsiz: &mut [f64],
    mut settings: Settings,
    out: Outputs,
) -> ApiResult<()> {
    let Outputs {
        xpls,
        fpls,
        gpls,
        itrmcd,
        itncnt,
        msg,
    } = out;
    let n = x.len();
    let epsm = f64::EPSILON;
    let mut p = vec![0.0; n];
    let mut g = vec![0.0; n];
    let mut udiag = vec![0.0; n];
    let mut sx = vec![0.0; n];
    let mut work = vec![0.0; n];
    *itncnt = 0;
    let mut iretcd = -1;

    // optchk: replace out-of-range settings with defaults, reject impossible ones.
    if !(1..=3).contains(&settings.method) {
        settings.method = 1;
    }
    if settings.iagflg != 1 {
        settings.iagflg = 0;
    }
    if settings.iahflg != 1 {
        settings.iahflg = 0;
    }
    if settings.iexp != 0 {
        settings.iexp = 1;
    }
    // Asking to skip the check of a derivative that was not supplied is an error.
    if (*msg / 2) % 2 == 1 && settings.iagflg == 0 {
        *msg = -6;
        return Ok(());
    }
    if (*msg / 4) % 2 == 1 && settings.iahflg == 0 {
        *msg = -7;
        return Ok(());
    }
    if n_declared <= 0 {
        *msg = -1;
        return Ok(());
    }
    if n == 1 && *msg % 2 == 0 {
        *msg = -2;
        return Ok(());
    }
    for (scale, size) in sx.iter_mut().zip(typsiz.iter_mut()) {
        if *size == 0.0 {
            *size = 1.0;
        } else if *size < 0.0 {
            *size = -*size;
        }
        *scale = 1.0 / *size;
    }
    if settings.stepmx <= 0.0 {
        let length: f64 = x
            .iter()
            .zip(&sx)
            .map(|(value, scale)| value * value * scale * scale)
            .sum();
        settings.stepmx = 1000.0 * length.sqrt().max(1.0);
    }
    if settings.fscale == 0.0 {
        settings.fscale = 1.0;
    } else if settings.fscale < 0.0 {
        settings.fscale = -settings.fscale;
    }
    if settings.gradtl < 0.0 {
        *msg = -3;
        return Ok(());
    }
    if settings.itnlim <= 0 {
        *msg = -4;
        return Ok(());
    }
    if settings.ndigit == 0 {
        *msg = -5;
        return Ok(());
    } else if settings.ndigit < 0 {
        settings.ndigit = (-epsm.log10()) as c_int;
    }
    if settings.dlt <= 0.0 {
        settings.dlt = -1.0;
    } else if settings.dlt > settings.stepmx {
        settings.dlt = settings.stepmx;
    }
    let Settings {
        fscale,
        method,
        iexp,
        ndigit,
        itnlim,
        mut iagflg,
        iahflg,
        mut dlt,
        gradtl,
        stepmx,
        steptl,
    } = settings;
    let expensive = iexp != 0;
    let rnf = 10f64.powi(-ndigit).max(epsm);
    let analtl = 1e-2f64.max(rnf.sqrt());
    let quiet = (*msg / 8) % 2 != 0;
    let limits = Limits {
        gradtl,
        steptl,
        fscale,
        itnlim,
    };

    let mut f = problem.value(x)?;
    if iagflg == 0 {
        forward_gradient(&problem, x, f, &mut g, &sx, rnf)?;
    } else {
        problem.gradient(x, &mut g)?;
        if (*msg / 2) % 2 == 0
            && !gradient_agrees(&problem, x, f, &g, typsiz, &sx, fscale, rnf, analtl)?
        {
            *msg = -21;
            return Ok(());
        }
    }
    let mut consecutive_maximal = 0;
    let mut stopped_at_new_iterate = false;
    *itrmcd = stopping_reason(
        x,
        f,
        &g,
        &work,
        *itncnt,
        &mut consecutive_maximal,
        &sx,
        limits,
        iretcd,
        false,
    );
    if *itrmcd == 0 {
        if expensive {
            for j in 0..n {
                a.set(j, j, if method == 3 { sx[j] * sx[j] } else { sx[j] });
                for i in j + 1..n {
                    a.set(i, j, 0.0);
                }
            }
        } else if iahflg == 0 {
            if iagflg != 0 {
                hessian_from_gradient(&problem, a, x, &g, &sx, rnf)?;
            } else {
                second_order_hessian(&problem, a, x, f, &sx, rnf)?;
            }
        } else if (*msg / 4) % 2 == 0 {
            if !hessian_agrees(
                &problem, a, x, f, &g, typsiz, &sx, rnf, analtl, iagflg, &mut udiag,
            )? {
                *msg = -22;
                return Ok(());
            }
        } else {
            problem.hessian(a, x)?;
        }
        if !quiet {
            print_iterate(x, f, &g, &p, *itncnt, true);
        }

        let mut hook = HookState::default();
        let mut saved_hook = HookState::default();
        let mut saved_dlt = 0.0;
        let mut no_update = false;
        loop {
            *itncnt += 1;
            let mut factor = !(expensive && method != 3);
            let mxtake;
            loop {
                if factor {
                    perturbed_cholesky(a, n, epsm, &sx, &mut udiag);
                }
                for (target, &value) in work.iter_mut().zip(&g) {
                    *target = -value;
                }
                cholesky_solve(a, &mut p, &work);
                if iagflg == 0 && method != 1 {
                    saved_dlt = dlt;
                    if method != 2 {
                        saved_hook = hook;
                    }
                }
                let step = Step {
                    x,
                    f,
                    g: &g,
                    sx: &sx,
                    stepmx,
                    steptl,
                };
                let (code, maximal) = match method {
                    1 => line_search(&problem, &step, &mut p, xpls, fpls)?,
                    2 => dogleg_driver(&problem, a, &step, &p, xpls, fpls, &mut dlt)?,
                    _ => hook_driver(
                        &problem, a, &step, &udiag, &p, xpls, fpls, &mut dlt, &mut hook, epsm,
                        *itncnt,
                    )?,
                };
                iretcd = code;
                if iretcd == 1 && iagflg == 0 {
                    // No acceptable step with forward differences: retry with central ones.
                    iagflg = -1;
                    central_gradient(&problem, x, &sx, rnf, &mut g)?;
                    if method != 1 {
                        dlt = saved_dlt;
                    }
                    if method == 3 {
                        hook = saved_hook;
                    }
                    factor = method == 3;
                    continue;
                }
                mxtake = maximal;
                break;
            }
            for i in 0..n {
                p[i] = xpls[i] - x[i];
            }
            if iagflg == -1 {
                central_gradient(&problem, xpls, &sx, rnf, gpls)?;
            } else if iagflg == 0 {
                forward_gradient(&problem, xpls, *fpls, gpls, &sx, rnf)?;
            } else {
                problem.gradient(xpls, gpls)?;
            }
            *itrmcd = stopping_reason(
                xpls,
                *fpls,
                gpls,
                x,
                *itncnt,
                &mut consecutive_maximal,
                &sx,
                limits,
                iretcd,
                mxtake,
            );
            if *itrmcd != 0 {
                break;
            }
            if expensive {
                if *itncnt == 1 {
                    no_update = true;
                }
                let secant = Secant {
                    x,
                    g: &g,
                    xpls,
                    gpls,
                    epsm,
                    rnf,
                    iagflg,
                };
                if method == 3 {
                    secant_unfactored(a, &secant, &mut udiag, &mut no_update);
                } else {
                    secant_factored(a, &secant, &mut no_update);
                }
            } else if iahflg == 1 {
                problem.hessian(a, xpls)?;
            } else if iagflg == 1 {
                hessian_from_gradient(&problem, a, xpls, gpls, &sx, rnf)?;
            } else {
                second_order_hessian(&problem, a, xpls, *fpls, &sx, rnf)?;
            }
            if (*msg / 16) % 2 == 1 {
                print_iterate(xpls, *fpls, gpls, &p, *itncnt, true);
            }
            f = *fpls;
            x.copy_from_slice(xpls);
            g.copy_from_slice(gpls);
        }
        stopped_at_new_iterate = *itrmcd != 3;
    }
    if !stopped_at_new_iterate {
        // Stopped at the previous iterate, or before the first step.
        *fpls = f;
        xpls.copy_from_slice(x);
        gpls.copy_from_slice(&g);
    }
    if !quiet {
        print_iterate(xpls, *fpls, gpls, &p, *itncnt, false);
    }
    *msg = 0;
    Ok(())
}

fn print_iterate(x: &[f64], f: f64, g: &[f64], p: &[f64], iteration: c_int, with_step: bool) {
    let mut text = format!("iteration = {iteration}\n");
    if with_step {
        text.push_str("Step:\n");
        text.push_str(&format_real_vector(p));
    }
    text.push_str("Parameter:\n");
    text.push_str(&format_real_vector(x));
    text.push_str("Function Value\n");
    text.push_str(&format_real_vector(&[f]));
    text.push_str("Gradient:\n");
    text.push_str(&format_real_vector(g));
    text.push('\n');
    print(&text);
}

/// The stopping tolerances.
#[derive(Clone, Copy)]
struct Limits {
    gradtl: f64,
    steptl: f64,
    fscale: f64,
    itnlim: c_int,
}

/// UNCMIN's `optstp`: the termination code for the step from `x` to `xpls`, or 0 to go on.
#[allow(clippy::too_many_arguments)]
fn stopping_reason(
    xpls: &[f64],
    fpls: f64,
    gpls: &[f64],
    x: &[f64],
    iteration: c_int,
    consecutive_maximal: &mut c_int,
    sx: &[f64],
    limits: Limits,
    iretcd: c_int,
    mxtake: bool,
) -> c_int {
    if iretcd == 1 {
        return 3;
    }
    let scale = fpls.abs().max(limits.fscale);
    let mut relative_gradient = 0.0f64;
    for i in 0..xpls.len() {
        relative_gradient =
            relative_gradient.max(gpls[i].abs() * xpls[i].abs().max(1.0 / sx[i]) / scale);
    }
    if relative_gradient <= limits.gradtl {
        return 1;
    }
    if iteration == 0 {
        return 0;
    }
    let mut relative_step = 0.0f64;
    for i in 0..xpls.len() {
        relative_step = relative_step.max((xpls[i] - x[i]).abs() / xpls[i].abs().max(1.0 / sx[i]));
    }
    if relative_step <= limits.steptl {
        return 2;
    }
    if iteration >= limits.itnlim {
        return 4;
    }
    if !mxtake {
        *consecutive_maximal = 0;
        return 0;
    }
    *consecutive_maximal += 1;
    if *consecutive_maximal < 5 { 0 } else { 5 }
}

/// Forward difference gradient of the objective (`fstofd` with one row).
unsafe fn forward_gradient(
    problem: &Problem,
    x: &mut [f64],
    f: f64,
    gradient: &mut [f64],
    sx: &[f64],
    rnoise: f64,
) -> ApiResult<()> {
    for j in 0..x.len() {
        let step = rnoise.sqrt() * x[j].abs().max(1.0 / sx[j]);
        let saved = x[j];
        x[j] = saved + step;
        let shifted = problem.value(x)?;
        x[j] = saved;
        gradient[j] = (shifted - f) / step;
    }
    Ok(())
}

/// Forward difference Hessian from the analytic gradient (`fstofd` for a Hessian), averaged
/// into the lower triangle.
unsafe fn hessian_from_gradient(
    problem: &Problem,
    a: Dense,
    x: &mut [f64],
    gradient: &[f64],
    sx: &[f64],
    rnoise: f64,
) -> ApiResult<()> {
    let n = x.len();
    let mut shifted = vec![0.0; n];
    for j in 0..n {
        let step = rnoise.sqrt() * x[j].abs().max(1.0 / sx[j]);
        let saved = x[j];
        x[j] = saved + step;
        problem.gradient(x, &mut shifted)?;
        x[j] = saved;
        for i in 0..n {
            a.set(i, j, (shifted[i] - gradient[i]) / step);
        }
    }
    for j in 0..n.saturating_sub(1) {
        for i in j + 1..n {
            a.set(i, j, (a.get(i, j) + a.get(j, i)) / 2.0);
        }
    }
    Ok(())
}

/// Central difference gradient (`fstocd`).
unsafe fn central_gradient(
    problem: &Problem,
    x: &mut [f64],
    sx: &[f64],
    rnoise: f64,
    gradient: &mut [f64],
) -> ApiResult<()> {
    for i in 0..x.len() {
        let saved = x[i];
        let step = rnoise.powf(1.0 / 3.0) * saved.abs().max(1.0 / sx[i]);
        x[i] = saved + step;
        let plus = problem.value(x)?;
        x[i] = saved - step;
        let minus = problem.value(x)?;
        x[i] = saved;
        gradient[i] = (plus - minus) / (step * 2.0);
    }
    Ok(())
}

/// Second order finite difference Hessian from function values alone (`sndofd`), into the
/// lower triangle.
unsafe fn second_order_hessian(
    problem: &Problem,
    a: Dense,
    x: &mut [f64],
    f: f64,
    sx: &[f64],
    rnoise: f64,
) -> ApiResult<()> {
    let n = x.len();
    let mut step = vec![0.0; n];
    let mut neighbour = vec![0.0; n];
    for i in 0..n {
        let saved = x[i];
        step[i] = rnoise.powf(1.0 / 3.0) * saved.abs().max(1.0 / sx[i]);
        x[i] = saved + step[i];
        neighbour[i] = problem.value(x)?;
        x[i] = saved;
    }
    for i in 0..n {
        let saved = x[i];
        x[i] = saved + step[i] * 2.0;
        let double = problem.value(x)?;
        a.set(
            i,
            i,
            ((f - neighbour[i]) + (double - neighbour[i])) / (step[i] * step[i]),
        );
        x[i] = saved + step[i];
        for j in 0..i {
            let other = x[j];
            x[j] = other + step[j];
            let both = problem.value(x)?;
            a.set(
                i,
                j,
                ((f - neighbour[i]) + (both - neighbour[j])) / (step[i] * step[j]),
            );
            x[j] = other;
        }
        x[i] = saved;
    }
    Ok(())
}

/// `grdchk`: whether the analytic gradient matches a forward difference one.
#[allow(clippy::too_many_arguments)]
unsafe fn gradient_agrees(
    problem: &Problem,
    x: &mut [f64],
    f: f64,
    gradient: &[f64],
    typsiz: &[f64],
    sx: &[f64],
    fscale: f64,
    rnoise: f64,
    tolerance: f64,
) -> ApiResult<bool> {
    let mut numeric = vec![0.0; x.len()];
    forward_gradient(problem, x, f, &mut numeric, sx, rnoise)?;
    for i in 0..x.len() {
        let scale = f.abs().max(fscale) / x[i].abs().max(typsiz[i]);
        if (gradient[i] - numeric[i]).abs() > gradient[i].abs().max(scale) * tolerance {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `heschk`: whether the analytic Hessian matches a finite difference one. Leaves the analytic
/// Hessian in `a`'s lower triangle.
#[allow(clippy::too_many_arguments)]
unsafe fn hessian_agrees(
    problem: &Problem,
    a: Dense,
    x: &mut [f64],
    f: f64,
    gradient: &[f64],
    typsiz: &[f64],
    sx: &[f64],
    rnoise: f64,
    tolerance: f64,
    iagflg: c_int,
    udiag: &mut [f64],
) -> ApiResult<bool> {
    let n = x.len();
    if iagflg != 0 {
        hessian_from_gradient(problem, a, x, gradient, sx, rnoise)?;
    } else {
        second_order_hessian(problem, a, x, f, sx, rnoise)?;
    }
    for j in 0..n {
        udiag[j] = a.get(j, j);
        for i in j + 1..n {
            a.set(j, i, a.get(i, j));
        }
    }
    problem.hessian(a, x)?;
    for j in 0..n {
        let scale = gradient[j].abs().max(1.0) / x[j].abs().max(typsiz[j]);
        if (a.get(j, j) - udiag[j]).abs() > udiag[j].abs().max(scale) * tolerance {
            return Ok(false);
        }
        for i in j + 1..n {
            if (a.get(i, j) - a.get(j, i)).abs() > a.get(i, j).abs().max(scale) * tolerance {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `chlhsn`: the Cholesky factor `L` of the Hessian in `a`'s lower triangle, perturbed by a
/// multiple of the identity when the Hessian is not safely positive definite. On return the
/// lower triangle holds `L`, the strict upper triangle the Hessian, and `udiag` its diagonal.
unsafe fn perturbed_cholesky(a: Dense, n: usize, epsm: f64, sx: &[f64], udiag: &mut [f64]) {
    for j in 0..n {
        for i in j..n {
            a.update(i, j, |value| value / (sx[i] * sx[j]));
        }
    }
    let tolerance = epsm.sqrt();
    let mut diagonal_max = a.get(0, 0);
    let mut diagonal_min = a.get(0, 0);
    for i in 1..n {
        diagonal_min = diagonal_min.min(a.get(i, i));
        diagonal_max = diagonal_max.max(a.get(i, i));
    }
    let positive_max = diagonal_max.max(0.0);
    if diagonal_min <= positive_max * tolerance {
        let mut shift = tolerance * (positive_max - diagonal_min) - diagonal_min;
        if shift == 0.0 {
            let mut off_max = 0.0f64;
            for i in 1..n {
                for j in 0..i {
                    off_max = off_max.max(a.get(i, j).abs());
                }
            }
            shift = if off_max == 0.0 {
                1.0
            } else {
                off_max * (1.0 + tolerance)
            };
        }
        for i in 0..n {
            a.update(i, i, |value| value + shift);
        }
        diagonal_max += shift;
    }
    for j in 0..n {
        udiag[j] = a.get(j, j);
        for i in 0..j {
            a.set(i, j, a.get(j, i));
        }
    }
    let added = cholesky(a, n, diagonal_max, tolerance);
    if added > 0.0 {
        // Restore the Hessian and shift it until it is safely diagonally dominant.
        for j in 0..n {
            a.set(j, j, udiag[j]);
            for i in 0..j {
                a.set(j, i, a.get(i, j));
            }
        }
        let mut eigen_min = 0.0f64;
        let mut eigen_max = a.get(0, 0);
        for i in 0..n {
            let mut off_row = 0.0;
            for j in 0..i {
                off_row += a.get(i, j).abs();
            }
            for j in i + 1..n {
                off_row += a.get(j, i).abs();
            }
            eigen_min = eigen_min.min(a.get(i, i) - off_row);
            eigen_max = eigen_max.max(a.get(i, i) + off_row);
        }
        let sdd = tolerance * (eigen_max - eigen_min) - eigen_min;
        let shift = sdd.min(added);
        for i in 0..n {
            a.update(i, i, |value| value + shift);
            udiag[i] = a.get(i, i);
        }
        cholesky(a, n, 0.0, tolerance);
    }
    for j in 0..n {
        for i in j..n {
            a.update(i, j, |value| value * sx[i]);
        }
        for i in 0..j {
            a.update(i, j, |value| value * sx[i] * sx[j]);
        }
        udiag[j] *= sx[j] * sx[j];
    }
}

/// `choldc`: Cholesky decomposition of the matrix in `a`'s upper triangle and diagonal into
/// `L` in its lower triangle, raising diagonal elements that would fall below
/// `sqrt(diagonal_max * tolerance)`. Returns the largest amount added.
unsafe fn cholesky(a: Dense, n: usize, diagonal_max: f64, tolerance: f64) -> f64 {
    let mut added = 0.0f64;
    let minimum = (diagonal_max * tolerance).sqrt();
    let minimum_squared = minimum * minimum;
    for i in 0..n {
        for j in 0..i {
            let mut sum = 0.0;
            for k in 0..j {
                sum += a.get(i, k) * a.get(j, k);
            }
            a.set(i, j, (a.get(j, i) - sum) / a.get(j, j));
        }
        let mut sum = 0.0;
        for k in 0..i {
            sum += a.get(i, k) * a.get(i, k);
        }
        let remainder = a.get(i, i) - sum;
        if remainder >= minimum_squared {
            a.set(i, i, remainder.sqrt());
        } else {
            let mut off_max = 0.0f64;
            for j in 0..i {
                off_max = off_max.max(a.get(i, j).abs());
            }
            if off_max <= minimum_squared {
                off_max = minimum_squared;
            }
            a.set(i, i, off_max.sqrt());
            added = added.max(off_max - remainder);
        }
    }
    added
}

/// Solves `L L' x = b` with `L` in `a`'s lower triangle (`lltslv`).
unsafe fn cholesky_solve(a: Dense, x: &mut [f64], b: &[f64]) {
    x.copy_from_slice(b);
    let n = x.len() as isize;
    let mut info = 0;
    triangular_solve(a.linpack(), n, x.as_mut_ptr(), 0, &mut info);
    triangular_solve(a.linpack(), n, x.as_mut_ptr(), 10, &mut info);
}

/// The current iterate and the step limits every global strategy reads.
struct Step<'a> {
    x: &'a [f64],
    f: f64,
    g: &'a [f64],
    sx: &'a [f64],
    stepmx: f64,
    steptl: f64,
}

fn scaled_length(sx: &[f64], p: &[f64]) -> f64 {
    sx.iter()
        .zip(p)
        .map(|(scale, value)| scale * scale * value * value)
        .sum::<f64>()
        .sqrt()
}

/// `lnsrch`: backtracking along the Newton direction `p` until the sufficient decrease
/// condition holds, by quadratic and then cubic interpolation. Returns `iretcd` (0 found, 1
/// none sufficiently distinct from `x`) and whether a maximal step was taken.
unsafe fn line_search(
    problem: &Problem,
    step: &Step,
    p: &mut [f64],
    xpls: &mut [f64],
    fpls: &mut f64,
) -> ApiResult<(c_int, bool)> {
    let Step {
        x,
        f,
        g,
        sx,
        stepmx,
        steptl,
    } = *step;
    let n = x.len();
    let mut length = scaled_length(sx, p);
    if length > stepmx {
        let scale = stepmx / length;
        for value in p.iter_mut() {
            *value *= scale;
        }
        length = stepmx;
    }
    let slope = ddot(n as isize, g.as_ptr(), p.as_ptr());
    let mut relative_length = 0.0f64;
    for i in 0..n {
        relative_length = relative_length.max(p[i].abs() / x[i].abs().max(1.0 / sx[i]));
    }
    let minimum_lambda = steptl / relative_length;
    let mut lambda = 1.0f64;
    let mut first_backtrack = true;
    let mut previous_lambda = 0.0f64;
    let mut previous_f = 0.0f64;
    loop {
        for i in 0..n {
            xpls[i] = x[i] + lambda * p[i];
        }
        *fpls = problem.value(xpls)?;
        if *fpls <= f + slope * 1e-4 * lambda {
            return Ok((0, lambda == 1.0 && length > stepmx * 0.99));
        }
        if lambda < minimum_lambda {
            return Ok((1, false));
        }
        if !(*fpls).is_finite() {
            lambda *= 0.1;
            first_backtrack = true;
            continue;
        }
        let mut trial;
        if first_backtrack {
            trial = -lambda * slope / ((*fpls - f - slope) * 2.0);
            first_backtrack = false;
        } else {
            let t1 = *fpls - f - lambda * slope;
            let t2 = previous_f - f - previous_lambda * slope;
            let t3 = 1.0 / (lambda - previous_lambda);
            let a3 = t3 * (t1 / (lambda * lambda) - t2 / (previous_lambda * previous_lambda));
            let b = t3
                * (t2 * lambda / (previous_lambda * previous_lambda)
                    - t1 * previous_lambda / (lambda * lambda));
            let discriminant = b * b - a3 * 3.0 * slope;
            let root = discriminant.sqrt();
            trial = if discriminant > b * b {
                (-b + if a3 < 0.0 { -root } else { root }) / (a3 * 3.0)
            } else {
                (-b + if a3 < 0.0 { root } else { -root }) / (a3 * 3.0)
            };
            if trial > lambda * 0.5 {
                trial = lambda * 0.5;
            }
        }
        previous_lambda = lambda;
        previous_f = *fpls;
        lambda = if trial < lambda * 0.1 {
            lambda * 0.1
        } else {
            trial
        };
    }
}

/// The state `tregup` keeps across the attempts of one global step.
struct TrustAttempt {
    iretcd: c_int,
    xplsp: Vec<f64>,
    fplsp: f64,
}

/// `tregup`: evaluates the trust region step `sc` and decides whether to accept it, shrink
/// the region (`iretcd` 2), or try a doubled region (3), keeping the last acceptable point in
/// `xplsp`. Returns whether a maximal step was taken.
#[allow(clippy::too_many_arguments)]
unsafe fn trust_region_update(
    problem: &Problem,
    a: Dense,
    step: &Step,
    sc: &[f64],
    newton_taken: bool,
    dlt: &mut f64,
    attempt: &mut TrustAttempt,
    xpls: &mut [f64],
    fpls: &mut f64,
    method: c_int,
    udiag: &[f64],
) -> ApiResult<bool> {
    let Step {
        x,
        f,
        g,
        sx,
        stepmx,
        steptl,
    } = *step;
    let n = x.len();
    for i in 0..n {
        xpls[i] = x[i] + sc[i];
    }
    *fpls = problem.value(xpls)?;
    let change = *fpls - f;
    let slope = ddot(n as isize, g.as_ptr(), sc.as_ptr());
    if attempt.iretcd == 4 {
        attempt.fplsp = 0.0;
    }
    if attempt.iretcd == 3 && (*fpls >= attempt.fplsp || change > slope * 1e-4) {
        attempt.iretcd = 0;
        xpls.copy_from_slice(&attempt.xplsp);
        *fpls = attempt.fplsp;
        *dlt *= 0.5;
        return Ok(false);
    }
    if change > slope * 1e-4 {
        let mut relative_length = 0.0f64;
        for i in 0..n {
            relative_length = relative_length.max(sc[i].abs() / xpls[i].abs().max(1.0 / sx[i]));
        }
        if relative_length < steptl {
            attempt.iretcd = 1;
        } else {
            attempt.iretcd = 2;
            let candidate = -slope * *dlt / ((change - slope) * 2.0);
            *dlt = if candidate < *dlt * 0.1 {
                *dlt * 0.1
            } else {
                candidate
            };
        }
        return Ok(false);
    }
    // The decrease is sufficient: compare it with the quadratic model's prediction.
    let mut predicted = 0.0;
    if method == 2 {
        for i in 0..n {
            let mut term = 0.0;
            for j in i..n {
                term += a.get(j, i) * sc[j];
            }
            predicted += term * term;
        }
    } else {
        for i in 0..n {
            predicted += udiag[i] * sc[i] * sc[i];
            let mut term = 0.0;
            for j in i + 1..n {
                term += a.get(i, j) * sc[i] * sc[j];
            }
            predicted += term * 2.0;
        }
    }
    let predicted = slope + predicted / 2.0;
    if attempt.iretcd != 2
        && ((predicted - change).abs() <= change.abs() * 0.1 || change <= slope)
        && !newton_taken
        && *dlt <= stepmx * 0.99
    {
        attempt.iretcd = 3;
        attempt.xplsp.copy_from_slice(xpls);
        attempt.fplsp = *fpls;
        *dlt = (*dlt * 2.0).min(stepmx);
        return Ok(false);
    }
    attempt.iretcd = 0;
    let mxtake = *dlt > stepmx * 0.99;
    if change >= predicted * 0.1 {
        *dlt *= 0.5;
    } else if change <= predicted * 0.75 {
        *dlt = (*dlt * 2.0).min(stepmx);
    }
    Ok(mxtake)
}

/// The Cauchy step's ingredients: `|D^-1 g|^2` and `|L' D^-2 g|^2`.
unsafe fn cauchy_terms(a: Dense, g: &[f64], sx: &[f64]) -> (f64, f64) {
    let n = g.len();
    let alpha: f64 = (0..n).map(|i| g[i] * g[i] / (sx[i] * sx[i])).sum();
    let mut beta = 0.0;
    for i in 0..n {
        let mut term = 0.0;
        for j in i..n {
            term += a.get(j, i) * g[j] / (sx[j] * sx[j]);
        }
        beta += term * term;
    }
    (alpha, beta)
}

/// `dogdrv`: double dogleg steps until the trust region update accepts one.
unsafe fn dogleg_driver(
    problem: &Problem,
    a: Dense,
    step: &Step,
    p: &[f64],
    xpls: &mut [f64],
    fpls: &mut f64,
    dlt: &mut f64,
) -> ApiResult<(c_int, bool)> {
    let (g, sx) = (step.g, step.sx);
    let n = g.len();
    let mut first = true;
    let newton_length = scaled_length(sx, p);
    let mut steepest = vec![0.0; n];
    let mut v = vec![0.0; n];
    let mut sc = vec![0.0; n];
    let mut attempt = TrustAttempt {
        iretcd: 4,
        xplsp: vec![0.0; n],
        fplsp: 0.0,
    };
    let mut cauchy_length = 0.0;
    let mut eta = 0.0;
    loop {
        let newton_taken = newton_length <= *dlt;
        if newton_taken {
            sc.copy_from_slice(p);
            *dlt = newton_length;
        } else {
            if first {
                first = false;
                let (alpha, beta) = cauchy_terms(a, g, sx);
                for i in 0..n {
                    steepest[i] = -(alpha / beta) * sx[i] * g[i] / (sx[i] * sx[i]);
                }
                cauchy_length = alpha * alpha.sqrt() / beta;
                eta =
                    0.8 * alpha * alpha / (-beta * ddot(n as isize, g.as_ptr(), p.as_ptr())) + 0.2;
                for i in 0..n {
                    v[i] = eta * sx[i] * p[i] - steepest[i];
                }
                if *dlt == -1.0 {
                    *dlt = cauchy_length.min(step.stepmx);
                }
            }
            if eta * newton_length <= *dlt {
                for i in 0..n {
                    sc[i] = *dlt / newton_length * p[i];
                }
            } else if cauchy_length >= *dlt {
                for i in 0..n {
                    sc[i] = *dlt / cauchy_length * steepest[i] / sx[i];
                }
            } else {
                let dot1 = ddot(n as isize, v.as_ptr(), steepest.as_ptr());
                let dot2 = ddot(n as isize, v.as_ptr(), v.as_ptr());
                let lambda = (-dot1
                    + (dot1 * dot1 - dot2 * (cauchy_length * cauchy_length - *dlt * *dlt)).sqrt())
                    / dot2;
                for i in 0..n {
                    sc[i] = (steepest[i] + lambda * v[i]) / sx[i];
                }
            }
        }
        let mxtake = trust_region_update(
            problem,
            a,
            step,
            &sc,
            newton_taken,
            dlt,
            &mut attempt,
            xpls,
            fpls,
            2,
            &[],
        )?;
        if attempt.iretcd <= 1 {
            return Ok((attempt.iretcd, mxtake));
        }
    }
}

/// `hookdrv`: More-Hebdon steps until the trust region update accepts one.
#[allow(clippy::too_many_arguments)]
unsafe fn hook_driver(
    problem: &Problem,
    a: Dense,
    step: &Step,
    udiag: &[f64],
    p: &[f64],
    xpls: &mut [f64],
    fpls: &mut f64,
    dlt: &mut f64,
    hook: &mut HookState,
    epsm: f64,
    iteration: c_int,
) -> ApiResult<(c_int, bool)> {
    let (g, sx) = (step.g, step.sx);
    let n = g.len();
    let mut first = true;
    let newton_length = scaled_length(sx, p);
    if iteration == 1 {
        hook.amu = 0.0;
        if *dlt == -1.0 {
            let (alpha, beta) = cauchy_terms(a, g, sx);
            *dlt = (alpha * alpha.sqrt() / beta).min(step.stepmx);
        }
    }
    let mut sc = vec![0.0; n];
    let mut work = vec![0.0; n];
    let mut attempt = TrustAttempt {
        iretcd: 4,
        xplsp: vec![0.0; n],
        fplsp: 0.0,
    };
    loop {
        let newton_taken = hook_step(
            a,
            g,
            udiag,
            p,
            sx,
            newton_length,
            dlt,
            hook,
            &mut first,
            &mut sc,
            &mut work,
            epsm,
        );
        hook.dltp = *dlt;
        let mxtake = trust_region_update(
            problem,
            a,
            step,
            &sc,
            newton_taken,
            dlt,
            &mut attempt,
            xpls,
            fpls,
            3,
            udiag,
        )?;
        if attempt.iretcd <= 1 {
            return Ok((attempt.iretcd, mxtake));
        }
    }
}

/// `hookst`: the step `sc` solving `(H + mu D^2) sc = -g` with `mu` chosen by More and
/// Hebdon's iteration so the scaled step length is within [0.75, 1.5] of the radius. Returns
/// whether the Newton step itself was taken.
#[allow(clippy::too_many_arguments)]
unsafe fn hook_step(
    a: Dense,
    g: &[f64],
    udiag: &[f64],
    p: &[f64],
    sx: &[f64],
    newton_length: f64,
    dlt: &mut f64,
    hook: &mut HookState,
    first: &mut bool,
    sc: &mut [f64],
    work: &mut [f64],
    epsm: f64,
) -> bool {
    const HIGH: f64 = 1.5;
    const LOW: f64 = 0.75;
    let n = g.len();
    if newton_length <= HIGH * *dlt {
        sc.copy_from_slice(p);
        *dlt = dlt.min(newton_length);
        hook.amu = 0.0;
        return true;
    }
    if hook.amu > 0.0 {
        hook.amu -= (hook.phi + hook.dltp) * (hook.dltp - *dlt + hook.phi) / (*dlt * hook.phip);
    }
    hook.phi = newton_length - *dlt;
    let mut info = 0;
    if *first {
        for i in 0..n {
            work[i] = sx[i] * sx[i] * p[i];
        }
        triangular_solve(a.linpack(), n as isize, work.as_mut_ptr(), 0, &mut info);
        let norm = dnrm2(n as isize, work.as_ptr());
        hook.phip0 = -(norm * norm) / newton_length;
        *first = false;
    }
    let mut amu_low = -hook.phi / hook.phip0;
    let mut amu_up = (0..n)
        .map(|i| g[i] * g[i] / (sx[i] * sx[i]))
        .sum::<f64>()
        .sqrt()
        / *dlt;
    loop {
        if hook.amu < amu_low || hook.amu > amu_up {
            hook.amu = (amu_low * amu_up).sqrt().max(amu_up * 0.001);
        }
        // Factor H + mu D^2 into the lower triangle; H stays in the upper triangle and udiag.
        for j in 0..n {
            a.set(j, j, udiag[j] + hook.amu * sx[j] * sx[j]);
            for i in 0..j {
                a.set(j, i, a.get(i, j));
            }
        }
        cholesky(a, n, 0.0, epsm.sqrt());
        for i in 0..n {
            work[i] = -g[i];
        }
        cholesky_solve(a, sc, work);
        let step_length = scaled_length(sx, sc);
        hook.phi = step_length - *dlt;
        for i in 0..n {
            work[i] = sx[i] * sx[i] * sc[i];
        }
        triangular_solve(a.linpack(), n as isize, work.as_mut_ptr(), 0, &mut info);
        let norm = dnrm2(n as isize, work.as_ptr());
        let phip = -(norm * norm) / step_length;
        hook.phip = phip;
        // Dennis and Schnabel stop when the bracket on mu is empty; GNU R's UNCMIN tests the
        // opposite, so it accepts the first step whenever the bracket is not empty, and its
        // callers see those steps.
        if (LOW * *dlt <= step_length && step_length <= HIGH * *dlt) || amu_up - amu_low > 0.0 {
            return false;
        }
        amu_low = amu_low.max(hook.amu - hook.phi / phip);
        if hook.phi < 0.0 {
            amu_up = amu_up.min(hook.amu);
        }
        hook.amu -= step_length * hook.phi / (*dlt * phip);
    }
}

/// The two iterates a secant update compares.
struct Secant<'a> {
    x: &'a [f64],
    g: &'a [f64],
    xpls: &'a [f64],
    gpls: &'a [f64],
    epsm: f64,
    rnf: f64,
    iagflg: c_int,
}

impl Secant<'_> {
    /// The step `s`, the gradient change `y`, and whether the update is safe to make.
    fn differences(&self) -> (Vec<f64>, Vec<f64>, f64, bool) {
        let n = self.x.len();
        let s: Vec<f64> = (0..n).map(|i| self.xpls[i] - self.x[i]).collect();
        let y: Vec<f64> = (0..n).map(|i| self.gpls[i] - self.g[i]).collect();
        // SAFETY: both vectors have n elements.
        let (den1, s_norm, y_norm) = unsafe {
            (
                ddot(n as isize, s.as_ptr(), y.as_ptr()),
                dnrm2(n as isize, s.as_ptr()),
                dnrm2(n as isize, y.as_ptr()),
            )
        };
        let safe = den1 >= self.epsm.sqrt() * s_norm * y_norm;
        (s, y, den1, safe)
    }
}

/// `secfac`: the BFGS update of the Cholesky factor `L` in `a`'s lower triangle, done as a
/// rank-one QR update of `L'`.
unsafe fn secant_factored(a: Dense, secant: &Secant, no_update: &mut bool) {
    let n = secant.x.len();
    let (s, y, den1, safe) = secant.differences();
    if !safe {
        return;
    }
    // u = L' s
    let mut u: Vec<f64> = (0..n)
        .map(|i| (i..n).map(|j| a.get(j, i) * s[j]).sum())
        .collect();
    let den2 = ddot(n as isize, u.as_ptr(), u.as_ptr());
    let mut alpha = (den1 / den2).sqrt();
    if *no_update {
        for j in 0..n {
            u[j] *= alpha;
            for i in j..n {
                a.update(i, j, |value| value * alpha);
            }
        }
        *no_update = false;
        alpha = 1.0;
    }
    // w = L u = L L' s
    let mut w: Vec<f64> = (0..n)
        .map(|i| (0..=i).map(|j| a.get(i, j) * u[j]).sum())
        .collect();
    let tolerance = if secant.iagflg == 0 {
        secant.rnf.sqrt()
    } else {
        secant.rnf
    };
    let skip = (0..n)
        .all(|i| (y[i] - w[i]).abs() < tolerance * secant.g[i].abs().max(secant.gpls[i].abs()));
    if skip {
        return;
    }
    for i in 0..n {
        w[i] = y[i] - alpha * w[i];
    }
    alpha /= den1;
    for value in u.iter_mut() {
        *value *= alpha;
    }
    for i in 1..n {
        for j in 0..i {
            a.set(j, i, a.get(i, j));
            a.set(i, j, 0.0);
        }
    }
    qr_update(a, n, &mut u, &w);
    for i in 1..n {
        for j in 0..i {
            a.set(i, j, a.get(j, i));
        }
    }
}

/// `qrupdt`: replaces the upper triangular `R` in `a` by the `R*` of `Q* R* = R + u v'`, with
/// Jacobi rotations.
unsafe fn qr_update(a: Dense, n: usize, u: &mut [f64], v: &[f64]) {
    let mut k = n - 1;
    while k > 0 && u[k] == 0.0 {
        k -= 1;
    }
    for i in (0..k).rev() {
        if u[i] == 0.0 {
            swap_rows(a, n, i);
            u[i] = u[i + 1];
        } else {
            rotate_rows(a, n, i, u[i], -u[i + 1]);
            u[i] = u[i].hypot(u[i + 1]);
        }
    }
    for j in 0..n {
        a.update(0, j, |value| value + u[0] * v[j]);
    }
    for i in 0..k {
        if a.get(i, i) == 0.0 {
            swap_rows(a, n, i);
        } else {
            rotate_rows(a, n, i, a.get(i, i), -a.get(i + 1, i));
        }
    }
}

/// Swaps rows `i` and `i + 1` of an upper Hessenberg matrix from column `i` on.
unsafe fn swap_rows(a: Dense, n: usize, i: usize) {
    for j in i..n {
        let held = a.get(i, j);
        a.set(i, j, a.get(i + 1, j));
        a.set(i + 1, j, held);
    }
}

/// Pre-multiplies rows `i` and `i + 1` by the Jacobi rotation of `(first, second)`.
unsafe fn rotate_rows(a: Dense, n: usize, i: usize, first: f64, second: f64) {
    let denominator = first.hypot(second);
    let c = first / denominator;
    let s = second / denominator;
    for j in i..n {
        let (y, z) = (a.get(i, j), a.get(i + 1, j));
        a.set(i, j, c * y - s * z);
        a.set(i + 1, j, s * y + c * z);
    }
}

/// `secunf`: the BFGS update of the unfactored Hessian kept in the upper triangle and `udiag`,
/// written into the lower triangle and diagonal.
unsafe fn secant_unfactored(a: Dense, secant: &Secant, udiag: &mut [f64], no_update: &mut bool) {
    let n = secant.x.len();
    for j in 0..n {
        a.set(j, j, udiag[j]);
        for i in j + 1..n {
            a.set(i, j, a.get(j, i));
        }
    }
    let (s, y, den1, safe) = secant.differences();
    if !safe {
        return;
    }
    // t = H s, with H symmetric in the lower triangle.
    let mut t: Vec<f64> = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| if j <= i { a.get(i, j) } else { a.get(j, i) } * s[j])
                .sum()
        })
        .collect();
    let mut den2 = ddot(n as isize, s.as_ptr(), t.as_ptr());
    if *no_update {
        let gamma = den1 / den2;
        den2 *= gamma;
        for j in 0..n {
            t[j] *= gamma;
            for i in j..n {
                a.update(i, j, |value| value * gamma);
            }
        }
        *no_update = false;
    }
    let skip = (0..n).all(|i| {
        let mut tolerance = secant.rnf * secant.g[i].abs().max(secant.gpls[i].abs());
        if secant.iagflg == 0 {
            tolerance /= secant.rnf.sqrt();
        }
        (y[i] - t[i]).abs() < tolerance
    });
    if skip {
        return;
    }
    for j in 0..n {
        for i in j..n {
            a.update(i, j, |value| {
                value + y[i] * y[j] / den1 - t[i] * t[j] / den2
            });
        }
    }
}

/// A finite difference Hessian of `fun` at `x`, into the upper triangle of `h` (leading
/// dimension `nfd`), with steps `10^(-ndigit/3)` relative to `max(x, typx)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdhess(
    n: c_int,
    x: *mut f64,
    fval: f64,
    fun: Option<FcnP>,
    state: *mut c_void,
    h: *mut f64,
    nfd: c_int,
    step: *mut f64,
    f: *mut f64,
    ndigit: c_int,
    typx: *mut f64,
) {
    enter(|| {
        let function = fun.ok_or_else(|| error("the objective function is NULL"))?;
        let problem = Problem {
            function,
            gradient: None,
            hessian: None,
            state,
        };
        let x = vector(x, n);
        let step = vector(step, n);
        let values = vector(f, n);
        let typx = vector(typx, n);
        let h = Dense {
            base: h,
            rows: nfd.max(0) as usize,
        };
        let eta = 10f64.powf(-ndigit as f64 / 3.0);
        for i in 0..x.len() {
            step[i] = eta * x[i].max(typx[i]);
            if typx[i] < 0.0 {
                step[i] = -step[i];
            }
            let saved = x[i];
            x[i] += step[i];
            step[i] = x[i] - saved;
            values[i] = problem.value(x)?;
            x[i] = saved;
        }
        for i in 0..x.len() {
            let saved = x[i];
            x[i] += step[i] * 2.0;
            let double = problem.value(x)?;
            h.set(
                i,
                i,
                ((fval - values[i]) + (double - values[i])) / (step[i] * step[i]),
            );
            x[i] = saved + step[i];
            for j in i + 1..x.len() {
                let other = x[j];
                x[j] += step[j];
                let both = problem.value(x)?;
                h.set(
                    i,
                    j,
                    ((fval - values[i]) + (both - values[j])) / (step[i] * step[j]),
                );
                x[j] = other;
            }
            x[i] = saved;
        }
        Ok(())
    })
}

// L-BFGS-B (Byrd, Lu, Nocedal and Zhu, "A limited memory algorithm for bound constrained
// optimization", SIAM J. Sci. Comput. 16, 1995), in the formulation of its version 2.3, which
// optim() uses: a generalized Cauchy point along the projected steepest descent path, direct
// primal subspace minimization over the free variables, truncation at the bounds, and the
// More-Thuente line search.

/// Limited-memory BFGS minimization within the bounds `l` and `u`. `msg` receives the
/// termination message (at most 59 characters); `fail` is 0 on convergence, 1 when `maxit`
/// iterations were reached, 51 for a warning and 52 for an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lbfgsb(
    n: c_int,
    m: c_int,
    x: *mut f64,
    l: *mut f64,
    u: *mut f64,
    nbd: *mut c_int,
    fmin: *mut f64,
    fminfn: Option<OptimFn>,
    fmingr: Option<OptimGr>,
    fail: *mut c_int,
    ex: *mut c_void,
    factr: f64,
    pgtol: f64,
    fncount: *mut c_int,
    grcount: *mut c_int,
    maxit: c_int,
    msg: *mut c_char,
    trace: c_int,
    report: c_int,
) {
    enter(|| {
        let objective = Objective::new(fminfn, fmingr, ex)?;
        if n == 0 {
            *fncount = 1;
            *grcount = 0;
            *fmin = objective.value(&mut [])?;
            set_message(msg, "NOTHING TO DO");
            *fail = 0;
            return Ok(());
        }
        if report <= 0 {
            return Err(error("REPORT must be > 0 (method = \"L-BFGS-B\")"));
        }
        let detail = match trace {
            2 => 0,
            3 => report,
            4 => 99,
            5 => 100,
            6 => 101,
            _ => -1,
        };
        let size = n.max(0);
        let bounds = Bounds {
            lower: vector(l, size),
            upper: vector(u, size),
            kind: if nbd.is_null() || size == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(nbd, size as usize)
            },
        };
        let x = vector(x, size);
        let settings = LbfgsbSettings {
            memory: m,
            factr,
            pgtol,
            maxit,
            trace,
            report,
            detail,
        };
        let outcome = minimize_bounded(&objective, x, &bounds, settings)?;
        *fail = outcome.fail;
        *fmin = outcome.value;
        *fncount = outcome.evaluations;
        *grcount = outcome.evaluations;
        if trace != 0 {
            if outcome.iterations < maxit && outcome.fail == 0 {
                print(&format!("final  value {:.6} \nconverged\n", outcome.value));
            } else {
                print(&format!(
                    "stopped after {} iterations\n",
                    outcome.iterations
                ));
            }
        }
        set_message(msg, &outcome.message);
        Ok(())
    })
}

unsafe fn set_message(msg: *mut c_char, text: &str) {
    if msg.is_null() {
        return;
    }
    let bytes = &text.as_bytes()[..text.len().min(59)];
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), msg as *mut u8, bytes.len());
    *msg.add(bytes.len()) = 0;
}

struct Bounds<'a> {
    lower: &'a [f64],
    upper: &'a [f64],
    /// 0 unbounded, 1 lower only, 2 both, 3 upper only.
    kind: &'a [c_int],
}

impl Bounds<'_> {
    fn has_lower(&self, i: usize) -> bool {
        matches!(self.kind[i], 1 | 2)
    }

    fn has_upper(&self, i: usize) -> bool {
        matches!(self.kind[i], 2 | 3)
    }

    /// The infinity norm of the gradient projected onto the feasible box.
    fn projected_gradient_norm(&self, x: &[f64], g: &[f64]) -> f64 {
        let mut norm = 0.0f64;
        for i in 0..x.len() {
            let mut component = g[i];
            if self.kind[i] != 0 {
                if component < 0.0 {
                    if self.has_upper(i) {
                        component = component.max(x[i] - self.upper[i]);
                    }
                } else if self.has_lower(i) {
                    component = component.min(x[i] - self.lower[i]);
                }
            }
            norm = norm.max(component.abs());
        }
        norm
    }
}

#[derive(Clone, Copy)]
struct LbfgsbSettings {
    memory: c_int,
    factr: f64,
    pgtol: f64,
    maxit: c_int,
    trace: c_int,
    report: c_int,
    detail: c_int,
}

struct LbfgsbOutcome {
    value: f64,
    fail: c_int,
    evaluations: c_int,
    iterations: c_int,
    message: String,
}

/// The limited memory: the last `m` steps `s` and gradient changes `y`, oldest first, with
/// `theta` and the middle matrix `M` of the compact representation `B = theta I - W M W'`,
/// `W = [Y, theta S]`.
struct Memory {
    capacity: usize,
    s: Vec<Vec<f64>>,
    y: Vec<Vec<f64>>,
    theta: f64,
    middle: Vec<Vec<f64>>,
}

impl Memory {
    fn new(capacity: usize) -> Memory {
        Memory {
            capacity,
            s: Vec::new(),
            y: Vec::new(),
            theta: 1.0,
            middle: Vec::new(),
        }
    }

    fn columns(&self) -> usize {
        self.s.len()
    }

    fn reset(&mut self) {
        self.s.clear();
        self.y.clear();
        self.theta = 1.0;
        self.middle.clear();
    }

    fn push(&mut self, s: Vec<f64>, y: Vec<f64>, theta: f64) {
        if self.s.len() == self.capacity {
            self.s.remove(0);
            self.y.remove(0);
        }
        self.s.push(s);
        self.y.push(y);
        self.theta = theta;
        let k = self.s.len();
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
        let mut matrix = vec![vec![0.0; 2 * k]; 2 * k];
        for i in 0..k {
            matrix[i][i] = -dot(&self.s[i], &self.y[i]);
            for j in 0..k {
                if i > j {
                    // L, the strictly lower part of S'Y, and its transpose.
                    let value = dot(&self.s[i], &self.y[j]);
                    matrix[k + i][j] = value;
                    matrix[j][k + i] = value;
                }
                matrix[k + i][k + j] = theta * dot(&self.s[i], &self.s[j]);
            }
        }
        self.middle = invert(matrix).unwrap_or_default();
        if self.middle.is_empty() {
            self.reset();
        }
    }

    /// Row `i` of `W`.
    fn row(&self, i: usize) -> Vec<f64> {
        let k = self.columns();
        let mut row = vec![0.0; 2 * k];
        for j in 0..k {
            row[j] = self.y[j][i];
            row[k + j] = self.theta * self.s[j][i];
        }
        row
    }

    fn times_middle(&self, v: &[f64]) -> Vec<f64> {
        self.middle
            .iter()
            .map(|row| row.iter().zip(v).map(|(a, b)| a * b).sum())
            .collect()
    }
}

/// Gauss-Jordan inversion with partial pivoting; `None` when the matrix is singular.
fn invert(mut matrix: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let size = matrix.len();
    let mut inverse: Vec<Vec<f64>> = (0..size)
        .map(|i| (0..size).map(|j| (i == j) as u8 as f64).collect())
        .collect();
    for column in 0..size {
        let pivot = (column..size).max_by(|&a, &b| {
            matrix[a][column]
                .abs()
                .partial_cmp(&matrix[b][column].abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        if matrix[pivot][column] == 0.0 || !matrix[pivot][column].is_finite() {
            return None;
        }
        matrix.swap(column, pivot);
        inverse.swap(column, pivot);
        let scale = matrix[column][column];
        for j in 0..size {
            matrix[column][j] /= scale;
            inverse[column][j] /= scale;
        }
        for row in 0..size {
            if row != column {
                let factor = matrix[row][column];
                if factor != 0.0 {
                    for j in 0..size {
                        matrix[row][j] -= factor * matrix[column][j];
                        inverse[row][j] -= factor * inverse[column][j];
                    }
                }
            }
        }
    }
    Some(inverse)
}

unsafe fn minimize_bounded(
    objective: &Objective,
    x: &mut [f64],
    bounds: &Bounds,
    settings: LbfgsbSettings,
) -> ApiResult<LbfgsbOutcome> {
    let n = x.len();
    let epsilon = f64::EPSILON;
    let mut outcome = LbfgsbOutcome {
        value: f64::NAN,
        fail: 52,
        evaluations: 0,
        iterations: 0,
        message: String::new(),
    };
    // errclb: arguments the algorithm cannot run with.
    let invalid = if settings.memory <= 0 {
        Some("ERROR: M .LE. 0")
    } else if settings.factr < 0.0 {
        Some("ERROR: FACTR .LT. 0")
    } else if bounds.kind.iter().any(|kind| !(0..=3).contains(kind)) {
        Some("ERROR: INVALID NBD")
    } else if (0..n).any(|i| bounds.kind[i] == 2 && bounds.lower[i] > bounds.upper[i]) {
        Some("ERROR: NO FEASIBLE SOLUTION")
    } else {
        None
    };
    if let Some(message) = invalid {
        outcome.message = message.into();
        return Ok(outcome);
    }
    let constrained = bounds.kind.iter().any(|&kind| kind != 0);
    let boxed = bounds.kind.iter().all(|&kind| kind == 2);
    for i in 0..n {
        if bounds.has_lower(i) {
            x[i] = x[i].max(bounds.lower[i]);
        }
        if bounds.has_upper(i) {
            x[i] = x[i].min(bounds.upper[i]);
        }
    }
    if settings.detail >= 0 {
        print(&format!(
            "N = {n}, M = {} machine precision = {}\n",
            settings.memory,
            format_g(epsilon)
        ));
    }
    let tolerance = settings.factr * epsilon;
    let mut memory = Memory::new(settings.memory as usize);
    let mut g = vec![0.0; n];
    let evaluate = |x: &mut [f64], g: &mut [f64]| -> ApiResult<f64> {
        let value = objective.value(x)?;
        if !value.is_finite() {
            return Err(error("L-BFGS-B needs finite values of 'fn'"));
        }
        objective.gradient(x, g)?;
        Ok(value)
    };
    let mut f = evaluate(x, &mut g)?;
    let mut evaluations: c_int = 1;
    let mut iterations: c_int = 0;
    let finish =
        |outcome: &mut LbfgsbOutcome, f: f64, evaluations, iterations, fail, message: &str| {
            outcome.value = f;
            outcome.evaluations = evaluations;
            outcome.iterations = iterations;
            outcome.fail = fail;
            outcome.message = message.into();
        };
    let mut projected = bounds.projected_gradient_norm(x, &g);
    if projected <= settings.pgtol {
        finish(
            &mut outcome,
            f,
            evaluations,
            iterations,
            0,
            "CONVERGENCE: NORM OF PROJECTED GRADIENT <= PGTOL",
        );
        return Ok(outcome);
    }
    loop {
        // The generalized Cauchy point z and the coefficients c of its W'(z - x) term.
        let (mut z, c) = if !constrained && memory.columns() > 0 {
            (x.to_vec(), vec![0.0; 2 * memory.columns()])
        } else {
            cauchy_point(x, &g, bounds, &memory)
        };
        let free: Vec<usize> = (0..n)
            .filter(|&i| {
                !(bounds.has_lower(i) && z[i] <= bounds.lower[i])
                    && !(bounds.has_upper(i) && z[i] >= bounds.upper[i])
            })
            .collect();
        if !free.is_empty() && memory.columns() > 0 {
            subspace_minimization(x, &g, bounds, &memory, &free, &c, &mut z);
        }
        let d: Vec<f64> = (0..n).map(|i| z[i] - x[i]).collect();

        // The line search along d.
        let d_norm = dnrm2(n as isize, d.as_ptr());
        let mut step_max = 1e10;
        if constrained {
            if iterations == 0 {
                step_max = 1.0;
            } else {
                for i in 0..n {
                    let direction = d[i];
                    if bounds.kind[i] == 0 {
                        continue;
                    }
                    if direction < 0.0 && bounds.has_lower(i) {
                        let room = bounds.lower[i] - x[i];
                        if room >= 0.0 {
                            step_max = 0.0;
                        } else if direction * step_max < room {
                            step_max = room / direction;
                        }
                    } else if direction > 0.0 && bounds.has_upper(i) {
                        let room = bounds.upper[i] - x[i];
                        if room <= 0.0 {
                            step_max = 0.0;
                        } else if direction * step_max > room {
                            step_max = room / direction;
                        }
                    }
                }
            }
        }
        let mut step = if iterations == 0 && !boxed {
            (1.0 / d_norm).min(step_max)
        } else {
            1.0
        };
        let start = x.to_vec();
        let start_gradient = g.clone();
        let f_old = f;
        let mut trials = 0;
        let mut search = MoreThuente::new(1e-3, 0.9, 0.1, 0.0, step_max);
        let mut gd_old = 0.0;
        let mut gd;
        let mut failed = false;
        loop {
            gd = ddot(n as isize, g.as_ptr(), d.as_ptr());
            if trials == 0 {
                gd_old = gd;
                if gd >= 0.0 {
                    failed = true;
                    break;
                }
            }
            let status = search.step(f, gd, &mut step);
            if status != SearchStatus::Evaluate {
                break;
            }
            trials += 1;
            evaluations += 1;
            if trials > 20 {
                evaluations -= 1;
                failed = true;
                break;
            }
            if step == 1.0 {
                x.copy_from_slice(&z);
            } else {
                for i in 0..n {
                    x[i] = step * d[i] + start[i];
                }
            }
            f = evaluate(x, &mut g)?;
        }
        if failed {
            x.copy_from_slice(&start);
            g.copy_from_slice(&start_gradient);
            f = f_old;
            if memory.columns() == 0 {
                finish(
                    &mut outcome,
                    f,
                    evaluations,
                    iterations,
                    52,
                    "ERROR: ABNORMAL_TERMINATION_IN_LNSRCH",
                );
                return Ok(outcome);
            }
            memory.reset();
            continue;
        }
        iterations += 1;
        projected = bounds.projected_gradient_norm(x, &g);
        if settings.trace == 1 && iterations % settings.report == 0 {
            print(&format!("iter {iterations:4} value {f:.6}\n"));
        }
        if settings.detail > 0 && iterations % settings.detail == 0 {
            print(&format!(
                "At iterate {iterations:5}  f = {}  |proj g|=  {}\n",
                format_g(f),
                format_g(projected)
            ));
        }
        if iterations > settings.maxit {
            finish(&mut outcome, f, evaluations, iterations, 1, "NEW_X");
            return Ok(outcome);
        }
        if projected <= settings.pgtol {
            finish(
                &mut outcome,
                f,
                evaluations,
                iterations,
                0,
                "CONVERGENCE: NORM OF PROJECTED GRADIENT <= PGTOL",
            );
            return Ok(outcome);
        }
        let scale = f_old.abs().max(f.abs()).max(1.0);
        if f_old - f <= tolerance * scale {
            finish(
                &mut outcome,
                f,
                evaluations,
                iterations,
                0,
                "CONVERGENCE: REL_REDUCTION_OF_F <= FACTR*EPSMCH",
            );
            return Ok(outcome);
        }
        // Update the memory unless the curvature condition fails.
        let s: Vec<f64> = (0..n).map(|i| x[i] - start[i]).collect();
        let y: Vec<f64> = (0..n).map(|i| g[i] - start_gradient[i]).collect();
        let rr = ddot(n as isize, y.as_ptr(), y.as_ptr());
        let (dr, threshold) = if step == 1.0 {
            (gd - gd_old, -gd_old)
        } else {
            ((gd - gd_old) * step, -gd_old * step)
        };
        if dr > epsilon * threshold {
            memory.push(s, y, rr / dr);
        }
    }
}

/// The generalized Cauchy point: the first local minimizer of the quadratic model along the
/// projected steepest descent path `x(t) = P(x - t g)`, found by walking its breakpoints.
/// Returns the point and `c = W'(z - x)`.
fn cauchy_point(x: &[f64], g: &[f64], bounds: &Bounds, memory: &Memory) -> (Vec<f64>, Vec<f64>) {
    let n = x.len();
    let k2 = 2 * memory.columns();
    let theta = memory.theta;
    let mut z = x.to_vec();
    let mut d = vec![0.0; n];
    let mut breakpoints: Vec<(f64, usize)> = Vec::new();
    for i in 0..n {
        let fixed = bounds.kind[i] == 2 && bounds.lower[i] == bounds.upper[i];
        let breakpoint = if g[i] < 0.0 && bounds.has_upper(i) {
            (x[i] - bounds.upper[i]) / g[i]
        } else if g[i] > 0.0 && bounds.has_lower(i) {
            (x[i] - bounds.lower[i]) / g[i]
        } else {
            f64::INFINITY
        };
        if fixed || breakpoint <= 0.0 {
            continue;
        }
        d[i] = -g[i];
        if breakpoint.is_finite() {
            breakpoints.push((breakpoint, i));
        }
    }
    breakpoints.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut c = vec![0.0; k2];
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
    let mut p = vec![0.0; k2];
    if k2 > 0 {
        for i in 0..n {
            if d[i] != 0.0 {
                let row = memory.row(i);
                for j in 0..k2 {
                    p[j] += row[j] * d[i];
                }
            }
        }
    }
    let mut fp = -dot(&d, &d);
    if fp == 0.0 {
        return (z, c);
    }
    let initial_curvature = -theta * fp;
    let mut fpp = initial_curvature
        - if k2 > 0 {
            dot(&p, &memory.times_middle(&p))
        } else {
            0.0
        };
    let curvature = |fpp: f64| fpp.max(f64::EPSILON * initial_curvature);
    let mut dt_min = -fp / curvature(fpp);
    let mut t_old = 0.0;
    for &(t, b) in &breakpoints {
        let dt = t - t_old;
        if dt_min < dt {
            break;
        }
        z[b] = if d[b] > 0.0 {
            bounds.upper[b]
        } else {
            bounds.lower[b]
        };
        let zb = z[b] - x[b];
        let gb = g[b];
        for j in 0..k2 {
            c[j] += dt * p[j];
        }
        if k2 > 0 {
            let row = memory.row(b);
            let mrow = memory.times_middle(&row);
            fp += dt * fpp + gb * gb + theta * gb * zb - gb * dot(&mrow, &c);
            fpp -= theta * gb * gb + 2.0 * gb * dot(&mrow, &p) + gb * gb * dot(&mrow, &row);
            for j in 0..k2 {
                p[j] += gb * row[j];
            }
        } else {
            fp += dt * fpp + gb * gb + theta * gb * zb;
            fpp -= theta * gb * gb;
        }
        d[b] = 0.0;
        dt_min = -fp / curvature(fpp);
        t_old = t;
    }
    let dt_min = dt_min.max(0.0);
    t_old += dt_min;
    for i in 0..n {
        if d[i] != 0.0 {
            z[i] = x[i] + t_old * d[i];
        }
    }
    for j in 0..k2 {
        c[j] += dt_min * p[j];
    }
    (z, c)
}

/// Minimizes the quadratic model over the free variables from the Cauchy point by the direct
/// primal method, then backtracks onto the box.
fn subspace_minimization(
    x: &[f64],
    g: &[f64],
    bounds: &Bounds,
    memory: &Memory,
    free: &[usize],
    c: &[f64],
    z: &mut [f64],
) {
    let k2 = 2 * memory.columns();
    let theta = memory.theta;
    let mc = memory.times_middle(c);
    let rows: Vec<Vec<f64>> = free.iter().map(|&i| memory.row(i)).collect();
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
    let reduced: Vec<f64> = free
        .iter()
        .zip(&rows)
        .map(|(&i, row)| -(g[i] + theta * (z[i] - x[i]) - dot(row, &mc)))
        .collect();
    let mut v = vec![0.0; k2];
    let mut gram = vec![vec![0.0; k2]; k2];
    for (row, &r) in rows.iter().zip(&reduced) {
        for a in 0..k2 {
            v[a] += row[a] * r;
            for b in 0..k2 {
                gram[a][b] += row[a] * row[b];
            }
        }
    }
    let v = memory.times_middle(&v);
    // N = I - (1/theta) M W'ZZ'W
    let mut system = vec![vec![0.0; k2]; k2];
    for a in 0..k2 {
        for b in 0..k2 {
            let product: f64 = (0..k2).map(|j| memory.middle[a][j] * gram[j][b]).sum();
            system[a][b] = (a == b) as u8 as f64 - product / theta;
        }
    }
    let Some(system_inverse) = invert(system) else {
        return;
    };
    let solved: Vec<f64> = system_inverse.iter().map(|row| dot(row, &v)).collect();
    let mut direction: Vec<f64> = reduced
        .iter()
        .zip(&rows)
        .map(|(&r, row)| r / theta + dot(row, &solved) / (theta * theta))
        .collect();
    let mut alpha = 1.0f64;
    let mut blocking = None;
    for (position, &k) in free.iter().enumerate() {
        let dk = direction[position];
        if bounds.kind[k] == 0 {
            continue;
        }
        let mut limit = alpha;
        if dk < 0.0 && bounds.has_lower(k) {
            let room = bounds.lower[k] - z[k];
            if room >= 0.0 {
                limit = 0.0;
            } else if dk * alpha < room {
                limit = room / dk;
            }
        } else if dk > 0.0 && bounds.has_upper(k) {
            let room = bounds.upper[k] - z[k];
            if room <= 0.0 {
                limit = 0.0;
            } else if dk * alpha > room {
                limit = room / dk;
            }
        }
        if limit < alpha {
            alpha = limit;
            blocking = Some(position);
        }
    }
    if alpha < 1.0
        && let Some(position) = blocking
    {
        let k = free[position];
        if direction[position] > 0.0 {
            z[k] = bounds.upper[k];
            direction[position] = 0.0;
        } else if direction[position] < 0.0 {
            z[k] = bounds.lower[k];
            direction[position] = 0.0;
        }
    }
    for (position, &k) in free.iter().enumerate() {
        z[k] += alpha * direction[position];
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SearchStatus {
    Evaluate,
    Converged,
    Warning,
}

/// The More-Thuente line search (MINPACK-2 `dcsrch`/`dcstep`) as a state machine: each call
/// takes the function value and directional derivative at the current step and either asks
/// for another evaluation at the updated step or stops.
struct MoreThuente {
    ftol: f64,
    gtol: f64,
    xtol: f64,
    step_min: f64,
    step_max: f64,
    started: bool,
    bracketed: bool,
    stage: u8,
    f_initial: f64,
    g_initial: f64,
    g_test: f64,
    width: f64,
    width_previous: f64,
    stx: f64,
    fx: f64,
    gx: f64,
    sty: f64,
    fy: f64,
    gy: f64,
    interval_min: f64,
    interval_max: f64,
}

impl MoreThuente {
    fn new(ftol: f64, gtol: f64, xtol: f64, step_min: f64, step_max: f64) -> MoreThuente {
        MoreThuente {
            ftol,
            gtol,
            xtol,
            step_min,
            step_max,
            started: false,
            bracketed: false,
            stage: 1,
            f_initial: 0.0,
            g_initial: 0.0,
            g_test: 0.0,
            width: 0.0,
            width_previous: 0.0,
            stx: 0.0,
            fx: 0.0,
            gx: 0.0,
            sty: 0.0,
            fy: 0.0,
            gy: 0.0,
            interval_min: 0.0,
            interval_max: 0.0,
        }
    }

    fn step(&mut self, f: f64, g: f64, step: &mut f64) -> SearchStatus {
        const EXTRAPOLATE_LOW: f64 = 1.1;
        const EXTRAPOLATE_HIGH: f64 = 4.0;
        if !self.started {
            if *step < self.step_min || *step > self.step_max || g >= 0.0 {
                return SearchStatus::Warning;
            }
            self.started = true;
            self.f_initial = f;
            self.g_initial = g;
            self.g_test = self.ftol * g;
            self.width = self.step_max - self.step_min;
            self.width_previous = self.width / 0.5;
            self.fx = f;
            self.gx = g;
            self.fy = f;
            self.gy = g;
            self.interval_max = *step + EXTRAPOLATE_HIGH * *step;
            return SearchStatus::Evaluate;
        }
        let f_test = self.f_initial + *step * self.g_test;
        if self.stage == 1 && f <= f_test && g >= 0.0 {
            self.stage = 2;
        }
        let mut status = None;
        if self.bracketed && (*step <= self.interval_min || *step >= self.interval_max) {
            status = Some(SearchStatus::Warning);
        }
        if self.bracketed && self.interval_max - self.interval_min <= self.xtol * self.interval_max
        {
            status = Some(SearchStatus::Warning);
        }
        if *step == self.step_max && f <= f_test && g <= self.g_test {
            status = Some(SearchStatus::Warning);
        }
        if *step == self.step_min && (f > f_test || g >= self.g_test) {
            status = Some(SearchStatus::Warning);
        }
        if f <= f_test && g.abs() <= self.gtol * (-self.g_initial) {
            status = Some(SearchStatus::Converged);
        }
        if let Some(status) = status {
            return status;
        }
        if self.stage == 1 && f <= self.fx && f > f_test {
            // Use the modified function until the sufficient decrease condition is reached.
            let mut fxm = self.fx - self.stx * self.g_test;
            let mut fym = self.fy - self.sty * self.g_test;
            let mut gxm = self.gx - self.g_test;
            let mut gym = self.gy - self.g_test;
            let fm = f - *step * self.g_test;
            let gm = g - self.g_test;
            self.take_step(&mut fxm, &mut gxm, &mut fym, &mut gym, step, fm, gm);
            self.fx = fxm + self.stx * self.g_test;
            self.fy = fym + self.sty * self.g_test;
            self.gx = gxm + self.g_test;
            self.gy = gym + self.g_test;
        } else {
            let (mut fx, mut gx, mut fy, mut gy) = (self.fx, self.gx, self.fy, self.gy);
            self.take_step(&mut fx, &mut gx, &mut fy, &mut gy, step, f, g);
            (self.fx, self.gx, self.fy, self.gy) = (fx, gx, fy, gy);
        }
        if self.bracketed {
            if (self.sty - self.stx).abs() >= 0.66 * self.width_previous {
                *step = self.stx + 0.5 * (self.sty - self.stx);
            }
            self.width_previous = self.width;
            self.width = (self.sty - self.stx).abs();
            self.interval_min = self.stx.min(self.sty);
            self.interval_max = self.stx.max(self.sty);
        } else {
            self.interval_min = *step + EXTRAPOLATE_LOW * (*step - self.stx);
            self.interval_max = *step + EXTRAPOLATE_HIGH * (*step - self.stx);
        }
        *step = step.max(self.step_min).min(self.step_max);
        if (self.bracketed && (*step <= self.interval_min || *step >= self.interval_max))
            || (self.bracketed
                && self.interval_max - self.interval_min <= self.xtol * self.interval_max)
        {
            *step = self.stx;
        }
        SearchStatus::Evaluate
    }

    /// `dcstep`: a safeguarded cubic or quadratic step that updates the interval of
    /// uncertainty `[stx, sty]`.
    #[allow(clippy::too_many_arguments)]
    fn take_step(
        &mut self,
        fx: &mut f64,
        dx: &mut f64,
        fy: &mut f64,
        dy: &mut f64,
        stp: &mut f64,
        fp: f64,
        dp: f64,
    ) {
        let (stx, sty) = (self.stx, self.sty);
        let (low, high) = (self.interval_min, self.interval_max);
        let sign = dp * (*dx / dx.abs());
        let stpf;
        if fp > *fx {
            let theta = 3.0 * (*fx - fp) / (*stp - stx) + *dx + dp;
            let s = theta.abs().max(dx.abs()).max(dp.abs());
            let mut gamma = s * ((theta / s).powi(2) - (*dx / s) * (dp / s)).sqrt();
            if *stp < stx {
                gamma = -gamma;
            }
            let p = (gamma - *dx) + theta;
            let q = ((gamma - *dx) + gamma) + dp;
            let r = p / q;
            let stpc = stx + r * (*stp - stx);
            let stpq = stx + ((*dx / ((*fx - fp) / (*stp - stx) + *dx)) / 2.0) * (*stp - stx);
            stpf = if (stpc - stx).abs() < (stpq - stx).abs() {
                stpc
            } else {
                stpc + (stpq - stpc) / 2.0
            };
            self.bracketed = true;
        } else if sign < 0.0 {
            let theta = 3.0 * (*fx - fp) / (*stp - stx) + *dx + dp;
            let s = theta.abs().max(dx.abs()).max(dp.abs());
            let mut gamma = s * ((theta / s).powi(2) - (*dx / s) * (dp / s)).sqrt();
            if *stp > stx {
                gamma = -gamma;
            }
            let p = (gamma - dp) + theta;
            let q = ((gamma - dp) + gamma) + *dx;
            let r = p / q;
            let stpc = *stp + r * (stx - *stp);
            let stpq = *stp + (dp / (dp - *dx)) * (stx - *stp);
            stpf = if (stpc - *stp).abs() > (stpq - *stp).abs() {
                stpc
            } else {
                stpq
            };
            self.bracketed = true;
        } else if dp.abs() < dx.abs() {
            let theta = 3.0 * (*fx - fp) / (*stp - stx) + *dx + dp;
            let s = theta.abs().max(dx.abs()).max(dp.abs());
            let mut gamma = s * ((theta / s).powi(2) - (*dx / s) * (dp / s)).max(0.0).sqrt();
            if *stp > stx {
                gamma = -gamma;
            }
            let p = (gamma - dp) + theta;
            let q = (gamma + (*dx - dp)) + gamma;
            let r = p / q;
            let stpc = if r < 0.0 && gamma != 0.0 {
                *stp + r * (stx - *stp)
            } else if *stp > stx {
                high
            } else {
                low
            };
            let stpq = *stp + (dp / (dp - *dx)) * (stx - *stp);
            if self.bracketed {
                let mut candidate = if (stpc - *stp).abs() < (stpq - *stp).abs() {
                    stpc
                } else {
                    stpq
                };
                candidate = if *stp > stx {
                    candidate.min(*stp + 0.66 * (sty - *stp))
                } else {
                    candidate.max(*stp + 0.66 * (sty - *stp))
                };
                stpf = candidate;
            } else {
                let candidate = if (stpc - *stp).abs() > (stpq - *stp).abs() {
                    stpc
                } else {
                    stpq
                };
                stpf = candidate.min(high).max(low);
            }
        } else if self.bracketed {
            let theta = 3.0 * (fp - *fy) / (sty - *stp) + *dy + dp;
            let s = theta.abs().max(dy.abs()).max(dp.abs());
            let mut gamma = s * ((theta / s).powi(2) - (*dy / s) * (dp / s)).sqrt();
            if *stp > sty {
                gamma = -gamma;
            }
            let p = (gamma - dp) + theta;
            let q = ((gamma - dp) + gamma) + *dy;
            let r = p / q;
            stpf = *stp + r * (sty - *stp);
        } else if *stp > stx {
            stpf = high;
        } else {
            stpf = low;
        }
        if fp > *fx {
            self.sty = *stp;
            *fy = fp;
            *dy = dp;
        } else {
            if sign < 0.0 {
                self.sty = stx;
                *fy = *fx;
                *dy = *dx;
            }
            self.stx = *stp;
            *fx = fp;
            *dx = dp;
        }
        *stp = stpf;
    }
}
