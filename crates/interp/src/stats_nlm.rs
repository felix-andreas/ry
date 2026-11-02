//! UNCMIN, the unconstrained minimizer behind stats' `nlm` and the C API's `optif9` and
//! `fdhess`, which adapt this one implementation to C callbacks.
//!
//! The algorithm is the modular one of Dennis and Schnabel, "Numerical Methods for
//! Unconstrained Optimization and Nonlinear Equations" (1983, appendix A): a quasi-Newton
//! method whose global strategy is a backtracking line search (method 1, A6.3.1), a double
//! dogleg trust region (method 2, A6.4.3 to A6.4.5) or a More-Hebdon trust region (method 3,
//! A6.4.1 and A6.4.2); forward or central difference gradients (A5.6.3, A5.6.4); an analytic,
//! finite difference (A5.6.1, A5.6.2) or secant (A9.4.1, A9.4.2) Hessian; the perturbed
//! Cholesky factorization (A5.5.1, A5.5.2); and the stopping tests (A7.2.1). GNU R's `nlm`
//! uses the same algorithm, so the iterates, and with them the printed iteration counts and
//! residual gradients, agree with it.

// The algorithm indexes several arrays in lockstep exactly as published, which keeps it
// checkable line by line against the book.
#![allow(clippy::needless_range_loop)]

use {
    crate::{
        builtin_libraries::NativeCall,
        coerce::coerce_vector,
        eval,
        print::{PrintOptions, print_vector},
        stats::{real_matrix, real_values},
        unwind::R,
        values::{self, length},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType},
};

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
        return Err(call.error("invalid parameter length"));
    }
    if start.iter().any(|value| !value.is_finite()) {
        return Err(call.error("missing value in parameter"));
    }
    let want_hessian = values::as_logical_scalar(call.arg(2)) == 1;
    let mut typical = real_values(coerce_vector(call.arg(3), SexpType::Real)?)?;
    if typical.len() != n {
        return Err(call.error("parameter vector and 'typsize' have different lengths"));
    }
    let message = values::as_integer_scalar(call.arg(5));
    let digits = values::as_integer_scalar(call.arg(6));
    let gradient_tolerance = values::as_real_scalar(call.arg(7));
    let iteration_limit = values::as_integer_scalar(call.arg(10));
    if gradient_tolerance.is_nan() || gradient_tolerance < 0.0 {
        return Err(call.error("invalid gradient tolerance in nlm"));
    }
    if iteration_limit == NA_INTEGER || iteration_limit <= 0 {
        return Err(call.error("invalid iteration limit in nlm"));
    }
    let mut objective = Objective {
        call,
        function,
        n,
        analytic_gradient: false,
        analytic_hessian: false,
        recent: Vec::with_capacity(RECENT_EVALUATIONS),
        oldest: 0,
    };
    let (analytic_gradient, analytic_hessian) = objective.probe(&start)?;
    objective.analytic_gradient = analytic_gradient;
    objective.analytic_hessian = analytic_hessian;
    // Skipping the check of a derivative the objective does not supply is no request at all.
    let mut message = message;
    if !analytic_gradient {
        message &= !2;
    }
    if !analytic_hessian {
        message &= !4;
    }
    let settings = Settings {
        method: 1,
        expensive: !analytic_hessian,
        analytic_gradient,
        analytic_hessian,
        message,
        digits,
        iteration_limit,
        fscale: values::as_real_scalar(call.arg(4)),
        trust_radius: 1.0,
        gradient_tolerance,
        max_step: values::as_real_scalar(call.arg(8)),
        step_tolerance: values::as_real_scalar(call.arg(9)),
    };
    let mut workspace = vec![0.0; n * n];
    let mut matrix = Matrix {
        values: &mut workspace,
        rows: n,
    };
    let minimum = match minimize(&mut objective, &mut matrix, &start, &mut typical, &settings)? {
        Ok(minimum) => minimum,
        Err(code) => {
            return Err(call.error(match code {
                -5 => "minimization function has no good digits in nlm".to_owned(),
                -21 => "probable coding error in analytic gradient".to_owned(),
                -22 => "probable coding error in analytic Hessian".to_owned(),
                code => format!("invalid nlm settings (UNCMIN error code {code})"),
            }));
        }
    };
    if message & 8 == 0 {
        print_stop_message(minimum.code);
    }
    let mut entries = vec![
        ("minimum", values::scalar_real(minimum.value)?),
        ("estimate", values::real_vector(&minimum.estimate)?),
        ("gradient", values::real_vector(&minimum.gradient)?),
    ];
    if want_hessian {
        let mut hessian = vec![0.0; n * n];
        let mut steps = vec![0.0; n];
        let mut shifted = vec![0.0; n];
        finite_difference_hessian(
            &mut |x| objective.value(x),
            &minimum.estimate,
            minimum.value,
            &typical,
            digits,
            &mut Matrix {
                values: &mut hessian,
                rows: n,
            },
            &mut steps,
            &mut shifted,
        )?;
        for j in 0..n {
            for i in j + 1..n {
                hessian[i + j * n] = hessian[j + i * n];
            }
        }
        entries.push(("hessian", real_matrix(&hessian, n, n)?));
    }
    entries.push(("code", values::scalar_integer(minimum.code)?));
    entries.push(("iterations", values::scalar_integer(minimum.iterations)?));
    values::named_list(&entries)
}

/// The function UNCMIN minimizes, with its optional analytic derivatives, and where it reports
/// the iterates.
pub trait Problem {
    unsafe fn value(&mut self, x: &[f64]) -> R<f64>;
    unsafe fn gradient(&mut self, x: &[f64], gradient: &mut [f64]) -> R<()>;
    /// Writes the analytic Hessian at `x` into the lower triangle and diagonal of `hessian`;
    /// the strict upper triangle holds data UNCMIN still needs.
    unsafe fn hessian(&mut self, x: &[f64], hessian: &mut Matrix<'_>) -> R<()>;
    /// Prints one iterate; `step` is the step that led to it, absent in the final report.
    unsafe fn report(
        &mut self,
        iteration: i32,
        step: Option<&[f64]>,
        x: &[f64],
        f: f64,
        g: &[f64],
    ) -> R<()>;
}

/// A column-major matrix with `rows` rows, of which UNCMIN uses the leading `n` by `n` block.
/// The C API hands UNCMIN the caller's work array, so its leading dimension may exceed `n`.
pub struct Matrix<'a> {
    pub values: &'a mut [f64],
    pub rows: usize,
}

impl Matrix<'_> {
    fn get(&self, row: usize, column: usize) -> f64 {
        self.values[row + column * self.rows]
    }

    fn set(&mut self, row: usize, column: usize, value: f64) {
        self.values[row + column * self.rows] = value;
    }
}

/// `optif9`'s settings. Out-of-range values are replaced by defaults as UNCMIN's `optchk`
/// does: a method outside 1 to 3 becomes 1, a nonpositive `max_step` becomes
/// `1000 max(|D x|, 1)`, a zero `fscale` 1, a negative `digits` the precision of a double, and
/// a nonpositive `trust_radius` the length of the first Cauchy step.
pub struct Settings {
    /// 1 line search, 2 double dogleg, 3 More-Hebdon.
    pub method: i32,
    /// Whether the objective is expensive to evaluate, which replaces finite difference
    /// Hessians by secant updates.
    pub expensive: bool,
    pub analytic_gradient: bool,
    pub analytic_hessian: bool,
    /// Bits: 1 allows a single parameter, 2 skips the check of the analytic gradient, 4 that
    /// of the analytic Hessian, 8 prints nothing and 16 prints every iterate.
    pub message: i32,
    /// The number of good decimal digits in the objective's values.
    pub digits: i32,
    pub iteration_limit: i32,
    pub fscale: f64,
    pub trust_radius: f64,
    pub gradient_tolerance: f64,
    pub max_step: f64,
    pub step_tolerance: f64,
}

/// Where UNCMIN stopped. `code` says why: 1 small relative gradient, 2 small relative step,
/// 3 no lower point found, 4 iteration limit, 5 five consecutive maximal steps.
pub struct Minimum {
    pub estimate: Vec<f64>,
    pub value: f64,
    pub gradient: Vec<f64>,
    pub code: i32,
    pub iterations: i32,
}

/// Minimizes the problem from `start`. `typical` holds each parameter's typical size and is
/// normalized in place to positive values; `a` is the work matrix, which ends up holding the
/// last Hessian or its factor. `Err` carries `optif9`'s negative `msg` code for settings
/// UNCMIN rejects (-1 to -7) or an analytic derivative that disagrees with finite differences
/// (-21 for the gradient, -22 for the Hessian).
pub unsafe fn minimize(
    problem: &mut dyn Problem,
    a: &mut Matrix<'_>,
    start: &[f64],
    typical: &mut [f64],
    settings: &Settings,
) -> R<Result<Minimum, i32>> {
    let n = start.len();
    let epsm = f64::EPSILON;
    let message = settings.message;
    let method = if (1..=3).contains(&settings.method) {
        settings.method
    } else {
        1
    };
    if message & 2 != 0 && !settings.analytic_gradient {
        return Ok(Err(-6));
    }
    if message & 4 != 0 && !settings.analytic_hessian {
        return Ok(Err(-7));
    }
    if n == 0 {
        return Ok(Err(-1));
    }
    if n == 1 && message & 1 == 0 {
        return Ok(Err(-2));
    }
    let mut sx = vec![0.0; n];
    for (scale, size) in sx.iter_mut().zip(typical.iter_mut()) {
        if *size == 0.0 {
            *size = 1.0;
        } else if *size < 0.0 {
            *size = -*size;
        }
        *scale = 1.0 / *size;
    }
    let mut stepmx = settings.max_step;
    if stepmx <= 0.0 {
        let length: f64 = start
            .iter()
            .zip(&sx)
            .map(|(value, scale)| value * value * scale * scale)
            .sum();
        stepmx = 1000.0 * length.sqrt().max(1.0);
    }
    let fscale = if settings.fscale == 0.0 {
        1.0
    } else {
        settings.fscale.abs()
    };
    if settings.gradient_tolerance < 0.0 {
        return Ok(Err(-3));
    }
    if settings.iteration_limit <= 0 {
        return Ok(Err(-4));
    }
    let digits = match settings.digits {
        0 => return Ok(Err(-5)),
        digits if digits < 0 => (-epsm.log10()) as i32,
        digits => digits,
    };
    let mut dlt = if settings.trust_radius <= 0.0 {
        -1.0
    } else {
        settings.trust_radius.min(stepmx)
    };
    let expensive = settings.expensive;
    let rnf = 10f64.powi(-digits).max(epsm);
    let analtl = 1e-2f64.max(rnf.sqrt());
    let quiet = message & 8 != 0;
    let limits = Limits {
        gradtl: settings.gradient_tolerance,
        steptl: settings.step_tolerance,
        fscale,
        itnlim: settings.iteration_limit,
    };
    let mut kind = if settings.analytic_gradient {
        GradientKind::Analytic
    } else {
        GradientKind::Forward
    };

    let mut x = start.to_vec();
    let mut p = vec![0.0; n];
    let mut g = vec![0.0; n];
    let mut udiag = vec![0.0; n];
    let mut work = vec![0.0; n];
    let mut xpls = vec![0.0; n];
    let mut gpls = vec![0.0; n];
    let mut fpls = 0.0;
    let mut iterations = 0;
    let mut f = problem.value(&x)?;
    if kind == GradientKind::Analytic {
        problem.gradient(&x, &mut g)?;
        if message & 2 == 0
            && !gradient_agrees(problem, &x, f, &g, typical, &sx, fscale, rnf, analtl)?
        {
            return Ok(Err(-21));
        }
    } else {
        forward_gradient(problem, &x, f, &mut g, &sx, rnf)?;
    }
    let mut consecutive_maximal = 0;
    let mut stopped_at_new_iterate = false;
    let mut code = stopping_reason(
        &x,
        f,
        &g,
        &work,
        iterations,
        &mut consecutive_maximal,
        &sx,
        &limits,
        -1,
        false,
    );
    if code == 0 {
        if expensive {
            for j in 0..n {
                a.set(j, j, if method == 3 { sx[j] * sx[j] } else { sx[j] });
                for i in j + 1..n {
                    a.set(i, j, 0.0);
                }
            }
        } else if !settings.analytic_hessian {
            if kind == GradientKind::Analytic {
                hessian_from_gradient(problem, a, &x, &g, &sx, rnf)?;
            } else {
                second_order_hessian(problem, a, &x, f, &sx, rnf)?;
            }
        } else if message & 4 == 0 {
            if !hessian_agrees(
                problem, a, &x, f, &g, typical, &sx, rnf, analtl, kind, &mut udiag,
            )? {
                return Ok(Err(-22));
            }
        } else {
            problem.hessian(&x, a)?;
        }
        if !quiet {
            problem.report(iterations, Some(&p), &x, f, &g)?;
        }

        let mut hook = HookState::default();
        let mut saved_hook = HookState::default();
        let mut saved_dlt = 0.0;
        let mut no_update = false;
        loop {
            iterations += 1;
            let mut factor = !(expensive && method != 3);
            let maximal_step;
            let mut iretcd;
            loop {
                if factor {
                    perturbed_cholesky(a, n, epsm, &sx, &mut udiag);
                }
                for (target, &value) in work.iter_mut().zip(&g) {
                    *target = -value;
                }
                cholesky_solve(a, &mut p, &work);
                if kind == GradientKind::Forward && method != 1 {
                    saved_dlt = dlt;
                    if method != 2 {
                        saved_hook = hook;
                    }
                }
                let step = Step {
                    x: &x,
                    f,
                    g: &g,
                    sx: &sx,
                    stepmx,
                    steptl: settings.step_tolerance,
                };
                let (outcome, maximal) = match method {
                    1 => line_search(problem, &step, &mut p, &mut xpls, &mut fpls)?,
                    2 => dogleg_driver(problem, a, &step, &p, &mut xpls, &mut fpls, &mut dlt)?,
                    _ => hook_driver(
                        problem, a, &step, &udiag, &p, &mut xpls, &mut fpls, &mut dlt, &mut hook,
                        epsm, iterations,
                    )?,
                };
                iretcd = outcome;
                if iretcd == 1 && kind == GradientKind::Forward {
                    // No acceptable step with forward differences: retry with central ones.
                    kind = GradientKind::Central;
                    central_gradient(problem, &x, &sx, rnf, &mut g)?;
                    if method != 1 {
                        dlt = saved_dlt;
                    }
                    if method == 3 {
                        hook = saved_hook;
                    }
                    factor = method == 3;
                    continue;
                }
                maximal_step = maximal;
                break;
            }
            for i in 0..n {
                p[i] = xpls[i] - x[i];
            }
            match kind {
                GradientKind::Central => central_gradient(problem, &xpls, &sx, rnf, &mut gpls)?,
                GradientKind::Forward => {
                    forward_gradient(problem, &xpls, fpls, &mut gpls, &sx, rnf)?
                }
                GradientKind::Analytic => problem.gradient(&xpls, &mut gpls)?,
            }
            code = stopping_reason(
                &xpls,
                fpls,
                &gpls,
                &x,
                iterations,
                &mut consecutive_maximal,
                &sx,
                &limits,
                iretcd,
                maximal_step,
            );
            if code != 0 {
                break;
            }
            if expensive {
                if iterations == 1 {
                    no_update = true;
                }
                let secant = Secant {
                    x: &x,
                    g: &g,
                    xpls: &xpls,
                    gpls: &gpls,
                    epsm,
                    rnf,
                    kind,
                };
                if method == 3 {
                    secant_unfactored(a, &secant, &mut udiag, &mut no_update);
                } else {
                    secant_factored(a, &secant, &mut no_update);
                }
            } else if settings.analytic_hessian {
                problem.hessian(&xpls, a)?;
            } else if kind != GradientKind::Forward {
                // After the switch to central differences GNU R differences the gradient
                // callback here, as if the gradient were analytic, and so must a C caller that
                // supplies one.
                hessian_from_gradient(problem, a, &xpls, &gpls, &sx, rnf)?;
            } else {
                second_order_hessian(problem, a, &xpls, fpls, &sx, rnf)?;
            }
            if message & 16 != 0 {
                problem.report(iterations, Some(&p), &xpls, fpls, &gpls)?;
            }
            f = fpls;
            x.copy_from_slice(&xpls);
            g.copy_from_slice(&gpls);
        }
        stopped_at_new_iterate = code != 3;
    }
    if !stopped_at_new_iterate {
        // Stopped at the previous iterate, or before the first step.
        fpls = f;
        xpls = x;
        gpls = g;
    }
    if !quiet {
        problem.report(iterations, None, &xpls, fpls, &gpls)?;
    }
    Ok(Ok(Minimum {
        estimate: xpls,
        value: fpls,
        gradient: gpls,
        code,
        iterations,
    }))
}

/// A finite difference Hessian of `value` at `x`, where it is `fval`, into the upper triangle
/// and diagonal of `hessian`, with steps `10^(-digits/3)` relative to `max(x, typical)` (A5.6.2
/// from function values alone). `steps` receives the steps actually taken and `shifted` the
/// values one step along each axis. This is the C API's `fdhess` and `nlm(hessian = TRUE)`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn finite_difference_hessian(
    value: &mut dyn FnMut(&[f64]) -> R<f64>,
    x: &[f64],
    fval: f64,
    typical: &[f64],
    digits: i32,
    hessian: &mut Matrix<'_>,
    steps: &mut [f64],
    shifted: &mut [f64],
) -> R<()> {
    let n = x.len();
    let eta = 10f64.powf(-digits as f64 / 3.0);
    let mut point = x.to_vec();
    for i in 0..n {
        let mut step = eta * x[i].max(typical[i]);
        if typical[i] < 0.0 {
            step = -step;
        }
        point[i] = x[i] + step;
        steps[i] = point[i] - x[i];
        shifted[i] = value(&point)?;
        point[i] = x[i];
    }
    for i in 0..n {
        point[i] = x[i] + steps[i] * 2.0;
        let double = value(&point)?;
        hessian.set(
            i,
            i,
            ((fval - shifted[i]) + (double - shifted[i])) / (steps[i] * steps[i]),
        );
        point[i] = x[i] + steps[i];
        for j in i + 1..n {
            point[j] = x[j] + steps[j];
            let both = value(&point)?;
            hessian.set(
                i,
                j,
                ((fval - shifted[i]) + (both - shifted[j])) / (steps[i] * steps[j]),
            );
            point[j] = x[j];
        }
        point[i] = x[i];
    }
    Ok(())
}

/// How the gradient is computed. A failed step with forward differences switches to central
/// differences for the rest of the run.
#[derive(Clone, Copy, PartialEq)]
enum GradientKind {
    Analytic,
    Forward,
    Central,
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

/// The stopping tolerances.
struct Limits {
    gradtl: f64,
    steptl: f64,
    fscale: f64,
    itnlim: i32,
}

/// The termination test of A7.2.1 (`optstp`) for the step from `x` to `xpls`, or 0 to go on.
#[allow(clippy::too_many_arguments)]
fn stopping_reason(
    xpls: &[f64],
    fpls: f64,
    gpls: &[f64],
    x: &[f64],
    iteration: i32,
    consecutive_maximal: &mut i32,
    sx: &[f64],
    limits: &Limits,
    iretcd: i32,
    maximal_step: bool,
) -> i32 {
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
    if !maximal_step {
        *consecutive_maximal = 0;
        return 0;
    }
    *consecutive_maximal += 1;
    if *consecutive_maximal < 5 { 0 } else { 5 }
}

/// Forward difference gradient with steps of `sqrt(eta)` relative to each parameter's size
/// (A5.6.3, `fstofd` with one row).
unsafe fn forward_gradient(
    problem: &mut dyn Problem,
    x: &[f64],
    f: f64,
    gradient: &mut [f64],
    sx: &[f64],
    rnoise: f64,
) -> R<()> {
    let mut point = x.to_vec();
    for j in 0..x.len() {
        let step = rnoise.sqrt() * x[j].abs().max(1.0 / sx[j]);
        point[j] = x[j] + step;
        let shifted = problem.value(&point)?;
        point[j] = x[j];
        gradient[j] = (shifted - f) / step;
    }
    Ok(())
}

/// Forward difference Hessian from the analytic gradient (`fstofd` for a Hessian), averaged
/// into the lower triangle.
unsafe fn hessian_from_gradient(
    problem: &mut dyn Problem,
    a: &mut Matrix<'_>,
    x: &[f64],
    gradient: &[f64],
    sx: &[f64],
    rnoise: f64,
) -> R<()> {
    let n = x.len();
    let mut point = x.to_vec();
    let mut shifted = vec![0.0; n];
    for j in 0..n {
        let step = rnoise.sqrt() * x[j].abs().max(1.0 / sx[j]);
        point[j] = x[j] + step;
        problem.gradient(&point, &mut shifted)?;
        point[j] = x[j];
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

/// Central difference gradient with steps of `eta^(1/3)` relative to each parameter's size
/// (A5.6.4, `fstocd`).
unsafe fn central_gradient(
    problem: &mut dyn Problem,
    x: &[f64],
    sx: &[f64],
    rnoise: f64,
    gradient: &mut [f64],
) -> R<()> {
    let mut point = x.to_vec();
    for i in 0..x.len() {
        let step = rnoise.powf(1.0 / 3.0) * x[i].abs().max(1.0 / sx[i]);
        point[i] = x[i] + step;
        let plus = problem.value(&point)?;
        point[i] = x[i] - step;
        let minus = problem.value(&point)?;
        point[i] = x[i];
        gradient[i] = (plus - minus) / (step * 2.0);
    }
    Ok(())
}

/// Second order finite difference Hessian from function values alone (`sndofd`), into the
/// lower triangle.
unsafe fn second_order_hessian(
    problem: &mut dyn Problem,
    a: &mut Matrix<'_>,
    x: &[f64],
    f: f64,
    sx: &[f64],
    rnoise: f64,
) -> R<()> {
    let n = x.len();
    let mut point = x.to_vec();
    let mut step = vec![0.0; n];
    let mut neighbour = vec![0.0; n];
    for i in 0..n {
        step[i] = rnoise.powf(1.0 / 3.0) * x[i].abs().max(1.0 / sx[i]);
        point[i] = x[i] + step[i];
        neighbour[i] = problem.value(&point)?;
        point[i] = x[i];
    }
    for i in 0..n {
        point[i] = x[i] + step[i] * 2.0;
        let double = problem.value(&point)?;
        a.set(
            i,
            i,
            ((f - neighbour[i]) + (double - neighbour[i])) / (step[i] * step[i]),
        );
        point[i] = x[i] + step[i];
        for j in 0..i {
            point[j] = x[j] + step[j];
            let both = problem.value(&point)?;
            a.set(
                i,
                j,
                ((f - neighbour[i]) + (both - neighbour[j])) / (step[i] * step[j]),
            );
            point[j] = x[j];
        }
        point[i] = x[i];
    }
    Ok(())
}

/// `grdchk`: whether the analytic gradient matches a forward difference one.
#[allow(clippy::too_many_arguments)]
unsafe fn gradient_agrees(
    problem: &mut dyn Problem,
    x: &[f64],
    f: f64,
    gradient: &[f64],
    typical: &[f64],
    sx: &[f64],
    fscale: f64,
    rnoise: f64,
    tolerance: f64,
) -> R<bool> {
    let mut numeric = vec![0.0; x.len()];
    forward_gradient(problem, x, f, &mut numeric, sx, rnoise)?;
    for i in 0..x.len() {
        let scale = f.abs().max(fscale) / x[i].abs().max(typical[i]);
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
    problem: &mut dyn Problem,
    a: &mut Matrix<'_>,
    x: &[f64],
    f: f64,
    gradient: &[f64],
    typical: &[f64],
    sx: &[f64],
    rnoise: f64,
    tolerance: f64,
    kind: GradientKind,
    udiag: &mut [f64],
) -> R<bool> {
    let n = x.len();
    if kind == GradientKind::Analytic {
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
    problem.hessian(x, a)?;
    for j in 0..n {
        let scale = gradient[j].abs().max(1.0) / x[j].abs().max(typical[j]);
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

/// `chlhsn` (A5.5.1): the Cholesky factor `L` of the Hessian in `a`'s lower triangle,
/// perturbed by a multiple of the identity when the Hessian is not safely positive definite.
/// On return the lower triangle holds `L`, the strict upper triangle the Hessian, and `udiag`
/// its diagonal.
fn perturbed_cholesky(a: &mut Matrix<'_>, n: usize, epsm: f64, sx: &[f64], udiag: &mut [f64]) {
    for j in 0..n {
        for i in j..n {
            a.set(i, j, a.get(i, j) / (sx[i] * sx[j]));
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
            a.set(i, i, a.get(i, i) + shift);
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
            a.set(i, i, a.get(i, i) + shift);
            udiag[i] = a.get(i, i);
        }
        cholesky(a, n, 0.0, tolerance);
    }
    for j in 0..n {
        for i in j..n {
            a.set(i, j, a.get(i, j) * sx[i]);
        }
        for i in 0..j {
            a.set(i, j, a.get(i, j) * sx[i] * sx[j]);
        }
        udiag[j] *= sx[j] * sx[j];
    }
}

/// `choldc` (A5.5.2): Cholesky decomposition of the matrix in `a`'s upper triangle and
/// diagonal into `L` in its lower triangle, raising diagonal elements that would fall below
/// `sqrt(diagonal_max * tolerance)`. Returns the largest amount added.
fn cholesky(a: &mut Matrix<'_>, n: usize, diagonal_max: f64, tolerance: f64) -> f64 {
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
fn cholesky_solve(a: &Matrix<'_>, x: &mut [f64], b: &[f64]) {
    x.copy_from_slice(b);
    solve_lower(a, x);
    solve_lower_transposed(a, x);
}

/// Solves `L y = b` in place for `L` in `a`'s lower triangle, column by column as LINPACK's
/// `dtrsl` does, so the sums round as there. A zero on the diagonal leaves `b` unchanged.
fn solve_lower(a: &Matrix<'_>, b: &mut [f64]) {
    let n = b.len();
    if n == 0 || (0..n).any(|i| a.get(i, i) == 0.0) {
        return;
    }
    b[0] /= a.get(0, 0);
    for j in 1..n {
        let factor = -b[j - 1];
        if factor != 0.0 {
            for i in j..n {
                b[i] += factor * a.get(i, j - 1);
            }
        }
        b[j] /= a.get(j, j);
    }
}

/// Solves `L' x = b` in place for `L` in `a`'s lower triangle, as `dtrsl` does.
fn solve_lower_transposed(a: &Matrix<'_>, b: &mut [f64]) {
    let n = b.len();
    if n == 0 || (0..n).any(|i| a.get(i, i) == 0.0) {
        return;
    }
    b[n - 1] /= a.get(n - 1, n - 1);
    for j in (0..n - 1).rev() {
        let mut sum = 0.0;
        for k in j + 1..n {
            sum += a.get(k, j) * b[k];
        }
        b[j] = (b[j] - sum) / a.get(j, j);
    }
}

fn dot(x: &[f64], y: &[f64]) -> f64 {
    x.iter().zip(y).fold(0.0, |sum, (x, y)| sum + x * y)
}

/// The Euclidean norm, accumulated with scaling as the reference BLAS `dnrm2` does so that it
/// neither overflows nor underflows.
fn norm(x: &[f64]) -> f64 {
    let mut scale = 0.0f64;
    let mut sum = 1.0f64;
    for &value in x {
        if value != 0.0 {
            let magnitude = value.abs();
            if scale < magnitude {
                sum = 1.0 + sum * (scale / magnitude) * (scale / magnitude);
                scale = magnitude;
            } else {
                sum += (magnitude / scale) * (magnitude / scale);
            }
        }
    }
    if scale == 0.0 {
        0.0
    } else {
        scale * sum.sqrt()
    }
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

/// `lnsrch` (A6.3.1): backtracking along the Newton direction `p` until the sufficient
/// decrease condition holds, by quadratic and then cubic interpolation. Returns `iretcd` (0
/// found, 1 none sufficiently distinct from `x`) and whether a maximal step was taken.
unsafe fn line_search(
    problem: &mut dyn Problem,
    step: &Step<'_>,
    p: &mut [f64],
    xpls: &mut [f64],
    fpls: &mut f64,
) -> R<(i32, bool)> {
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
    let slope = dot(g, p);
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
        // nlm replaces a non-finite value by the largest double. Either one only shrinks the
        // step, because the interpolating models would overflow on it.
        if !(*fpls < f64::MAX) {
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
            // Three times the cubic coefficient, grouped as 3 * t3 * (...) because that order
            // reproduces GNU R's backtracking steps to the last bit.
            let a3 = 3.0 * t3 * (t1 / (lambda * lambda) - t2 / (previous_lambda * previous_lambda));
            let b = t3
                * (t2 * lambda / (previous_lambda * previous_lambda)
                    - t1 * previous_lambda / (lambda * lambda));
            let discriminant = b * b - a3 * slope;
            let root = discriminant.sqrt();
            // One positive critical point means it is the minimum; two mean the first is.
            trial = if discriminant > b * b {
                (-b + if a3 < 0.0 { -root } else { root }) / a3
            } else {
                (-b + if a3 < 0.0 { root } else { -root }) / a3
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
    iretcd: i32,
    xplsp: Vec<f64>,
    fplsp: f64,
}

/// `tregup` (A6.4.5): evaluates the trust region step `sc` and decides whether to accept it,
/// shrink the region (`iretcd` 2), or try a doubled region (3), keeping the last acceptable
/// point in `xplsp`. Returns whether a maximal step was taken.
#[allow(clippy::too_many_arguments)]
unsafe fn trust_region_update(
    problem: &mut dyn Problem,
    a: &Matrix<'_>,
    step: &Step<'_>,
    sc: &[f64],
    newton_taken: bool,
    dlt: &mut f64,
    attempt: &mut TrustAttempt,
    xpls: &mut [f64],
    fpls: &mut f64,
    method: i32,
    udiag: &[f64],
) -> R<bool> {
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
    let slope = dot(g, sc);
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
    if let Ok(mut debug) = std::fs::OpenOptions::new().append(true).open("/tmp/claude-0/-home-user-ry/05f36392-06b4-560e-9355-f87e5eff3d37/scratchpad/op/tregup.log") {
        use std::io::Write;
        let mut upper = 0.0;
        let mut full = 0.0;
        for i in 0..n {
            let mut term = 0.0;
            for j in i..n {
                term += a.get(i, j) * sc[j];
            }
            upper += term * term;
            for j in 0..n {
                let h = if i == j { udiag.get(i).copied().unwrap_or(f64::NAN) } else if i < j { a.get(i, j) } else { a.get(j, i) };
                full += sc[i] * h * sc[j];
            }
        }
        writeln!(debug, "TREGUP m{method} iretcd={} change={change:e} slope={slope:e} pred={predicted:e} upper={:e} full={:e} newton={newton_taken} dlt={:e} stepmx={stepmx:e}", attempt.iretcd, slope + upper / 2.0, if udiag.is_empty() { f64::NAN } else { slope + full / 2.0 }, *dlt).ok();
    }
    if attempt.iretcd != 2
        && ((predicted - change).abs() <= change.abs() * 0.1 || change <= slope)
        && newton_taken
        && *dlt <= stepmx * 0.99
    {
        attempt.iretcd = 3;
        attempt.xplsp.copy_from_slice(xpls);
        attempt.fplsp = *fpls;
        *dlt = (*dlt * 2.0).min(stepmx);
        return Ok(false);
    }
    attempt.iretcd = 0;
    let maximal_step = *dlt > stepmx * 0.99;
    if change >= predicted * 0.1 {
        *dlt *= 0.5;
    } else if change <= predicted * 0.75 {
        *dlt = (*dlt * 2.0).min(stepmx);
    }
    Ok(maximal_step)
}

/// The Cauchy step's ingredients: `|D^-1 g|^2` and `|L' D^-2 g|^2`.
fn cauchy_terms(a: &Matrix<'_>, g: &[f64], sx: &[f64]) -> (f64, f64) {
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

/// `dogdrv` (A6.4.3): double dogleg steps until the trust region update accepts one.
unsafe fn dogleg_driver(
    problem: &mut dyn Problem,
    a: &Matrix<'_>,
    step: &Step<'_>,
    p: &[f64],
    xpls: &mut [f64],
    fpls: &mut f64,
    dlt: &mut f64,
) -> R<(i32, bool)> {
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
            // dogstp (A6.4.4).
            if first {
                first = false;
                let (alpha, beta) = cauchy_terms(a, g, sx);
                for i in 0..n {
                    steepest[i] = -(alpha / beta) * sx[i] * g[i] / (sx[i] * sx[i]);
                }
                cauchy_length = alpha * alpha.sqrt() / beta;
                eta = 0.8 * alpha * alpha / (-beta * dot(g, p)) + 0.2;
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
                let dot1 = dot(&v, &steepest);
                let dot2 = dot(&v, &v);
                let lambda = (-dot1
                    + (dot1 * dot1 - dot2 * (cauchy_length * cauchy_length - *dlt * *dlt)).sqrt())
                    / dot2;
                for i in 0..n {
                    sc[i] = (steepest[i] + lambda * v[i]) / sx[i];
                }
            }
        }
        let maximal_step = trust_region_update(
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
            return Ok((attempt.iretcd, maximal_step));
        }
    }
}

/// `hookdrv` (A6.4.1): More-Hebdon steps until the trust region update accepts one.
#[allow(clippy::too_many_arguments)]
unsafe fn hook_driver(
    problem: &mut dyn Problem,
    a: &mut Matrix<'_>,
    step: &Step<'_>,
    udiag: &[f64],
    p: &[f64],
    xpls: &mut [f64],
    fpls: &mut f64,
    dlt: &mut f64,
    hook: &mut HookState,
    epsm: f64,
    iteration: i32,
) -> R<(i32, bool)> {
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
        let maximal_step = trust_region_update(
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
            return Ok((attempt.iretcd, maximal_step));
        }
    }
}

/// `hookst` (A6.4.2): the step `sc` solving `(H + mu D^2) sc = -g` with `mu` chosen by More and
/// Hebdon's iteration so the scaled step length is within [0.75, 1.5] of the radius. Returns
/// whether the Newton step itself was taken.
#[allow(clippy::too_many_arguments)]
fn hook_step(
    a: &mut Matrix<'_>,
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
    if *first {
        for i in 0..n {
            work[i] = sx[i] * sx[i] * p[i];
        }
        solve_lower(a, work);
        let length = norm(work);
        hook.phip0 = -(length * length) / newton_length;
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
        solve_lower(a, work);
        let length = norm(work);
        let phip = -(length * length) / step_length;
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
    kind: GradientKind,
}

impl Secant<'_> {
    /// The step `s`, the gradient change `y`, `y's`, and whether the update is safe to make.
    fn differences(&self) -> (Vec<f64>, Vec<f64>, f64, bool) {
        let n = self.x.len();
        let s: Vec<f64> = (0..n).map(|i| self.xpls[i] - self.x[i]).collect();
        let y: Vec<f64> = (0..n).map(|i| self.gpls[i] - self.g[i]).collect();
        let den1 = dot(&s, &y);
        let safe = den1 >= self.epsm.sqrt() * norm(&s) * norm(&y);
        (s, y, den1, safe)
    }
}

/// `secfac` (A9.4.2): the BFGS update of the Cholesky factor `L` in `a`'s lower triangle, done
/// as a rank-one QR update of `L'`.
fn secant_factored(a: &mut Matrix<'_>, secant: &Secant<'_>, no_update: &mut bool) {
    let n = secant.x.len();
    let (s, y, den1, safe) = secant.differences();
    if !safe {
        return;
    }
    // u = L' s
    let mut u: Vec<f64> = (0..n)
        .map(|i| (i..n).map(|j| a.get(j, i) * s[j]).sum())
        .collect();
    let den2 = dot(&u, &u);
    let mut alpha = (den1 / den2).sqrt();
    if *no_update {
        for j in 0..n {
            u[j] *= alpha;
            for i in j..n {
                a.set(i, j, a.get(i, j) * alpha);
            }
        }
        *no_update = false;
        alpha = 1.0;
    }
    // w = L u = L L' s
    let mut w: Vec<f64> = (0..n)
        .map(|i| (0..=i).map(|j| a.get(i, j) * u[j]).sum())
        .collect();
    let tolerance = if secant.kind == GradientKind::Forward {
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

/// `qrupdt` (A3.4.1): replaces the upper triangular `R` in `a` by the `R*` of
/// `Q* R* = R + u v'`, with Jacobi rotations.
fn qr_update(a: &mut Matrix<'_>, n: usize, u: &mut [f64], v: &[f64]) {
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
        a.set(0, j, a.get(0, j) + u[0] * v[j]);
    }
    for i in 0..k {
        if a.get(i, i) == 0.0 {
            swap_rows(a, n, i);
        } else {
            let (first, second) = (a.get(i, i), -a.get(i + 1, i));
            rotate_rows(a, n, i, first, second);
        }
    }
}

/// Swaps rows `i` and `i + 1` of an upper Hessenberg matrix from column `i` on.
fn swap_rows(a: &mut Matrix<'_>, n: usize, i: usize) {
    for j in i..n {
        let held = a.get(i, j);
        a.set(i, j, a.get(i + 1, j));
        a.set(i + 1, j, held);
    }
}

/// Pre-multiplies rows `i` and `i + 1` by the Jacobi rotation of `(first, second)`.
fn rotate_rows(a: &mut Matrix<'_>, n: usize, i: usize, first: f64, second: f64) {
    let denominator = first.hypot(second);
    let c = first / denominator;
    let s = second / denominator;
    for j in i..n {
        let (y, z) = (a.get(i, j), a.get(i + 1, j));
        a.set(i, j, c * y - s * z);
        a.set(i + 1, j, s * y + c * z);
    }
}

/// `secunf` (A9.4.1): the BFGS update of the unfactored Hessian kept in the upper triangle and
/// `udiag`, written into the lower triangle and diagonal.
fn secant_unfactored(
    a: &mut Matrix<'_>,
    secant: &Secant<'_>,
    udiag: &mut [f64],
    no_update: &mut bool,
) {
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
    let mut den2 = dot(&s, &t);
    if *no_update {
        let gamma = den1 / den2;
        den2 *= gamma;
        for j in 0..n {
            t[j] *= gamma;
            for i in j..n {
                a.set(i, j, a.get(i, j) * gamma);
            }
        }
        *no_update = false;
    }
    let skip = (0..n).all(|i| {
        let mut tolerance = secant.rnf * secant.g[i].abs().max(secant.gpls[i].abs());
        if secant.kind == GradientKind::Forward {
            tolerance /= secant.rnf.sqrt();
        }
        (y[i] - t[i]).abs() < tolerance
    });
    if skip {
        return;
    }
    for j in 0..n {
        for i in j..n {
            a.set(i, j, a.get(i, j) + y[i] * y[j] / den1 - t[i] * t[j] / den2);
        }
    }
}

/// One evaluation of the objective: its value and the `gradient` and `hessian` attributes it
/// may carry.
struct Evaluation {
    point: Vec<f64>,
    value: f64,
    gradient: Option<Vec<f64>>,
    hessian: Option<Vec<f64>>,
}

impl crate::unwind::Rooted for Evaluation {}

/// How many evaluations the objective remembers. GNU R remembers five, which is visible in how
/// often a function with side effects is called: the Hessian check revisits the start and the
/// gradient check's points, and calls none of them again for up to four parameters.
const RECENT_EVALUATIONS: usize = 5;

/// The R objective, remembering its recent evaluations so that a gradient or Hessian read from
/// the attributes, or a point visited again, does not call the function again.
struct Objective<'a> {
    call: &'a NativeCall<'a>,
    function: Sexp,
    n: usize,
    /// Which attributes the probe at the start accepted; the others are never read.
    analytic_gradient: bool,
    analytic_hessian: bool,
    /// A ring of the latest evaluations, where `oldest` is the one the next new point replaces.
    recent: Vec<Evaluation>,
    oldest: usize,
}

impl Objective<'_> {
    /// Calls the objective at the start only to learn which derivatives it supplies, as GNU R
    /// does: the value is not looked at and not cached, so minimizing calls the start again.
    /// A derivative of the wrong length or mode is ignored with a warning, and a Hessian
    /// counts only beside a gradient.
    unsafe fn probe(&mut self, x: &[f64]) -> R<(bool, bool)> {
        crate::scope(|| {
            let argument = values::real_vector(x)?;
            let expression = values::call(self.function, &[argument])?;
            let result = eval::eval(expression, self.call.env)?;
            let supplied = |name: &str, size: usize| -> R<bool> {
                let found = values::get_attr(result, values::install(name)?);
                if found == values::nil() {
                    return Ok(false);
                }
                let numeric = matches!(found.kind(), SexpType::Integer | SexpType::Real);
                if numeric && length(found) == size {
                    return Ok(true);
                }
                self.call.warning(format!(
                    "{name} supplied is of the wrong length or mode, so ignored"
                ))?;
                Ok(false)
            };
            let gradient = supplied("gradient", self.n)?;
            let hessian = gradient && supplied("hessian", self.n * self.n)?;
            Ok((gradient, hessian))
        })
    }

    unsafe fn evaluate(&mut self, x: &[f64]) -> R<&Evaluation> {
        let index = match self.recent.iter().position(|recent| recent.point == x) {
            Some(index) => index,
            None => {
                let evaluation = self.compute(x)?;
                if self.recent.len() < RECENT_EVALUATIONS {
                    self.recent.push(evaluation);
                    self.recent.len() - 1
                } else {
                    let index = self.oldest;
                    self.recent[index] = evaluation;
                    self.oldest = (index + 1) % RECENT_EVALUATIONS;
                    index
                }
            }
        };
        match self.recent.get(index) {
            Some(evaluation) => Ok(evaluation),
            None => Err(self.call.error("nlm lost its cached evaluation")),
        }
    }

    unsafe fn compute(&mut self, x: &[f64]) -> R<Evaluation> {
        // A diverging iteration (an infinite value turns a difference gradient infinite) asks
        // for a non-finite point, which GNU R rejects rather than passing to the objective.
        if x.iter().any(|entry| !entry.is_finite()) {
            return Err(self.call.error("non-finite value supplied by 'nlm'"));
        }
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
            let attribute = |name: &str, size: usize, wanted: bool| -> R<Option<Vec<f64>>> {
                if !wanted {
                    return Ok(None);
                }
                let found = values::get_attr(result, values::install(name)?);
                if found == values::nil() {
                    return Ok(None);
                }
                // GNU R reads the first `size` entries of a longer attribute, and only the
                // probe at the start insists on the exact length.
                let mut numbers = real_values(coerce_vector(found, SexpType::Real)?)?;
                numbers.truncate(size);
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
                gradient: attribute("gradient", self.n, self.analytic_gradient)?,
                hessian: attribute("hessian", self.n * self.n, self.analytic_hessian)?,
            })
        })
    }

    unsafe fn value(&mut self, x: &[f64]) -> R<f64> {
        Ok(self.evaluate(x)?.value)
    }
}

impl Problem for Objective<'_> {
    unsafe fn value(&mut self, x: &[f64]) -> R<f64> {
        Objective::value(self, x)
    }

    unsafe fn gradient(&mut self, x: &[f64], gradient: &mut [f64]) -> R<()> {
        match &self.evaluate(x)?.gradient {
            Some(values) => gradient.copy_from_slice(values),
            None => gradient.fill(0.0),
        }
        Ok(())
    }

    /// The `hessian` attribute is a full matrix by columns; UNCMIN reads its lower triangle.
    unsafe fn hessian(&mut self, x: &[f64], matrix: &mut Matrix<'_>) -> R<()> {
        let n = self.n;
        let hessian = self.evaluate(x)?.hessian.as_deref();
        for j in 0..n {
            for i in j..n {
                matrix.set(i, j, hessian.map_or(0.0, |hessian| hessian[i + j * n]));
            }
        }
        Ok(())
    }

    unsafe fn report(
        &mut self,
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
