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

use {
    crate::{
        boundary::{ApiResult, enter, error, take_pending, warn, write_console},
        runtime,
        utils::format_real_vector,
    },
    interp::{
        arith::find_interval,
        format::format_g,
        random,
        stats_lbfgsb::{self as lbfgsb, Bound, minimize},
        stats_nlm::{self as nlm, finite_difference_hessian},
        stats_optimize::{Integrand, Subintervals, qagi, qags},
    },
    std::ffi::{c_char, c_int, c_void},
};

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
        random::get_rng_state()?;
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
                    *next = position + spread * random::norm_rand()?;
                }
                let value = finite_or_big(objective.value(&mut candidate)?);
                let rise = value - current;
                if rise <= 0.0 || random::unif_rand()? < (-rise / temperature).exp() {
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
        random::put_rng_state()
    })
}

/// Belisle's schedule divides the starting temperature by `log(k + e - 1)` at the `k`th
/// evaluation that opens a temperature. GNU R writes `e - 1` to eight digits, which makes every
/// step about 1e-8 longer than the exact constant would; the shortened value reproduces GNU R's
/// stream under a given seed.
const COOLING_OFFSET: f64 = 1.7182818;

// QUADPACK lives in `interp::stats_optimize`; `Rdqags` and `Rdqagi` adapt it to C integrands
// and the caller's work arrays.

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
        let Some(mut intervals) = subintervals(limit, lenw, iwork, work) else {
            return Ok(());
        };
        let function = f.ok_or_else(|| error("the integrand is NULL"))?;
        let outcome = qags(
            &mut CallbackIntegrand { function, ex },
            *a,
            *b,
            *epsabs,
            *epsrel,
            &mut intervals,
        )?;
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
        let Some(mut intervals) = subintervals(limit, lenw, iwork, work) else {
            return Ok(());
        };
        let function = f.ok_or_else(|| error("the integrand is NULL"))?;
        let infinity = *inf;
        let outcome = qagi(
            &mut CallbackIntegrand { function, ex },
            *bound,
            infinity,
            *epsabs,
            *epsrel,
            &mut intervals,
        )?;
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

/// Splits the caller's work arrays into the subinterval table, or `None` when they are too
/// small for `limit` intervals.
unsafe fn subintervals<'a>(
    limit: *const c_int,
    lenw: *const c_int,
    iwork: *mut c_int,
    work: *mut f64,
) -> Option<Subintervals<'a>> {
    let limit = *limit;
    if limit < 1 || i64::from(*lenw) < i64::from(limit) * 4 {
        return None;
    }
    let limit = limit as usize;
    let work = std::slice::from_raw_parts_mut(work, 4 * limit);
    let (left, rest) = work.split_at_mut(limit);
    let (right, rest) = rest.split_at_mut(limit);
    let (estimate, error) = rest.split_at_mut(limit);
    Some(Subintervals {
        left,
        right,
        estimate,
        error,
        order: std::slice::from_raw_parts_mut(iwork, limit),
    })
}

/// A C integrand and its opaque pointer.
struct CallbackIntegrand {
    function: IntegrFn,
    ex: *mut c_void,
}

impl Integrand for CallbackIntegrand {
    unsafe fn evaluate(&mut self, points: &mut [f64]) -> ApiResult<()> {
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

/// `interp::subset::max_col`, which `max.col()` shares.
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
        if rows == 0 {
            return Ok(());
        }
        let matrix: &[f64] = if columns == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(matrix, rows * columns)
        };
        let maxes = std::slice::from_raw_parts_mut(maxes, rows);
        interp::subset::max_col(matrix, rows, columns, *ties_meth, maxes)
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

// UNCMIN, the engine of nlm(), lives in `interp::stats_nlm`; `optif9` and `fdhess` adapt it to
// C callbacks.

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
        let mut problem = CallbackProblem {
            function,
            gradient: d1fcn,
            hessian: d2fcn,
            state,
        };
        let size = n.max(0);
        let rows = nr.max(0) as usize;
        if rows < size as usize {
            return Err(error(
                "optif9: the leading dimension 'nr' is smaller than 'n'",
            ));
        }
        // C callers may pass the same array for `x` and `xpls`, so the start point is copied
        // before `xpls` becomes a `&mut` slice.
        let start = vector(x, size).to_vec();
        let cells = rows
            .checked_mul(size as usize)
            .and_then(|cells| c_int::try_from(cells).ok())
            .ok_or_else(|| error("optif9: the Hessian 'a' is too large"))?;
        let mut matrix = nlm::Matrix {
            values: vector(a, cells),
            rows,
        };
        let settings = nlm::Settings {
            method,
            expensive: iexp != 0,
            analytic_gradient: iagflg == 1,
            analytic_hessian: iahflg == 1,
            message: *msg,
            digits: ndigit,
            iteration_limit: itnlim,
            fscale,
            trust_radius: dlt,
            gradient_tolerance: gradtl,
            max_step: stepmx,
            step_tolerance: steptl,
        };
        *itncnt = 0;
        match nlm::minimize(
            &mut problem,
            &mut matrix,
            &start,
            vector(typsiz, size),
            &settings,
        )? {
            Ok(minimum) => {
                vector(xpls, size).copy_from_slice(&minimum.estimate);
                vector(gpls, size).copy_from_slice(&minimum.gradient);
                *fpls = minimum.value;
                *itrmcd = minimum.code;
                *itncnt = minimum.iterations;
                *msg = 0;
            }
            Err(code) => *msg = code,
        }
        Ok(())
    })
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
        let mut problem = CallbackProblem {
            function,
            gradient: None,
            hessian: None,
            state,
        };
        let size = n.max(0);
        let rows = nfd.max(0) as usize;
        if rows < size as usize {
            return Err(error(
                "fdhess: the leading dimension 'nfd' is smaller than 'n'",
            ));
        }
        let cells = rows
            .checked_mul(size as usize)
            .and_then(|cells| c_int::try_from(cells).ok())
            .ok_or_else(|| error("fdhess: the Hessian 'h' is too large"))?;
        finite_difference_hessian(
            &mut |point| nlm::Problem::value(&mut problem, point),
            vector(x, size),
            fval,
            vector(typx, size),
            ndigit,
            &mut nlm::Matrix {
                values: vector(h, cells),
                rows,
            },
            vector(step, size),
            vector(f, size),
        )
    })
}

/// The C callbacks of `optif9` and `fdhess` as UNCMIN evaluates them. The callbacks take
/// mutable parameter pointers, so each evaluation hands them a copy.
struct CallbackProblem {
    function: FcnP,
    gradient: Option<FcnP>,
    hessian: Option<D2FcnP>,
    state: *mut c_void,
}

impl nlm::Problem for CallbackProblem {
    unsafe fn value(&mut self, x: &[f64]) -> ApiResult<f64> {
        let mut point = x.to_vec();
        let mut value = 0.0;
        callback(|| {
            ry_call_fcn(
                self.function,
                point.len() as c_int,
                point.as_mut_ptr(),
                &mut value,
                self.state,
            )
        })?;
        Ok(value)
    }

    unsafe fn gradient(&mut self, x: &[f64], gradient: &mut [f64]) -> ApiResult<()> {
        let function = self
            .gradient
            .ok_or_else(|| error("the analytic gradient function is NULL"))?;
        let mut point = x.to_vec();
        callback(|| {
            ry_call_fcn(
                function,
                point.len() as c_int,
                point.as_mut_ptr(),
                gradient.as_mut_ptr(),
                self.state,
            )
        })
    }

    unsafe fn hessian(&mut self, x: &[f64], hessian: &mut nlm::Matrix<'_>) -> ApiResult<()> {
        let function = self
            .hessian
            .ok_or_else(|| error("the analytic Hessian function is NULL"))?;
        let mut point = x.to_vec();
        callback(|| {
            ry_call_d2fcn(
                function,
                hessian.rows as c_int,
                point.len() as c_int,
                point.as_mut_ptr(),
                hessian.values.as_mut_ptr(),
                self.state,
            )
        })
    }

    unsafe fn report(
        &mut self,
        iteration: i32,
        step: Option<&[f64]>,
        x: &[f64],
        f: f64,
        g: &[f64],
    ) -> ApiResult<()> {
        let mut text = format!("iteration = {iteration}\n");
        if let Some(step) = step {
            text.push_str("Step:\n");
            text.push_str(&format_real_vector(step));
        }
        text.push_str("Parameter:\n");
        text.push_str(&format_real_vector(x));
        text.push_str("Function Value\n");
        text.push_str(&format_real_vector(&[f]));
        text.push_str("Gradient:\n");
        text.push_str(&format_real_vector(g));
        text.push('\n');
        print(&text);
        Ok(())
    }
}

/// Limited-memory BFGS minimization within the bounds `l` and `u`, with `nbd` saying which
/// bounds each variable has. `msg` receives the termination message (at most 59 characters);
/// `fail` is 0 on convergence, 1 when `maxit` iterations were reached and 52 for an error.
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
        if n > 0 && report <= 0 {
            return Err(error("REPORT must be > 0 (method = \"L-BFGS-B\")"));
        }
        let size = n.max(0);
        let lower = vector(l, size);
        let upper = vector(u, size);
        let kinds: &[c_int] = if nbd.is_null() || size == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(nbd, size as usize)
        };
        let expected = size as usize;
        let point = vector(x, size);
        if [point.len(), lower.len(), upper.len(), kinds.len()] != [expected; 4] {
            return Err(error(
                "lbfgsb: 'x', 'l', 'u' and 'nbd' must hold 'n' values",
            ));
        }
        let mut bounds = Vec::with_capacity(kinds.len());
        for &kind in kinds {
            bounds.push(match kind {
                0 => Bound::None,
                1 => Bound::Lower,
                2 => Bound::Both,
                3 => Bound::Upper,
                _ => {
                    // The reference code reports an invalid kind unless a later variable has
                    // infeasible bounds, whose message then wins.
                    let infeasible = kinds
                        .iter()
                        .zip(lower.iter().zip(upper.iter()))
                        .any(|(&kind, (lower, upper))| kind == 2 && lower > upper);
                    let message = if infeasible {
                        "ERROR: NO FEASIBLE SOLUTION"
                    } else {
                        "ERROR: INVALID NBD"
                    };
                    *fail = 52;
                    *fncount = 0;
                    *grcount = 0;
                    set_message(msg, message);
                    return Ok(());
                }
            });
        }
        let settings = lbfgsb::Settings {
            memory: m,
            factr,
            pgtol,
            max_iterations: maxit,
            trace,
            report,
        };
        let mut adapter = CallbackObjective { objective };
        let outcome = minimize(&mut adapter, point, lower, upper, &bounds, &settings)?;
        *fail = outcome.fail;
        *fmin = outcome.value;
        *fncount = outcome.function_count;
        *grcount = outcome.gradient_count;
        set_message(msg, outcome.message);
        Ok(())
    })
}

/// The C callbacks as the shared L-BFGS-B core evaluates them. The callbacks take mutable
/// parameter pointers, so each evaluation hands them a copy.
struct CallbackObjective {
    objective: Objective,
}

impl lbfgsb::Objective for CallbackObjective {
    unsafe fn value(&mut self, x: &[f64]) -> ApiResult<f64> {
        self.objective.value(&mut x.to_vec())
    }

    unsafe fn gradient(&mut self, x: &[f64], gradient: &mut [f64]) -> ApiResult<()> {
        self.objective.gradient(&mut x.to_vec(), gradient)
    }
}

unsafe fn set_message(msg: *mut c_char, text: &str) {
    if msg.is_null() {
        return;
    }
    let bytes = &text.as_bytes()[..text.len().min(59)];
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), msg as *mut u8, bytes.len());
    *msg.add(bytes.len()) = 0;
}
