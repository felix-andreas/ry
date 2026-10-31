//! L-BFGS-B, the limited memory quasi-Newton method for bound constrained minimization behind
//! `optim(method = "L-BFGS-B")`, after Byrd, Lu, Nocedal and Zhu, "A limited memory algorithm
//! for bound constrained optimization" (SIAM J. Scientific Computing, 1995).
//!
//! Each iteration finds the generalized Cauchy point along the projected steepest descent
//! path of the quadratic model, minimizes the model over the variables still free there
//! (backtracking into the box), and searches along the resulting direction with Moré and
//! Thuente's line search. The model's Hessian is the compact limited memory BFGS matrix
//! `B = θI - W M W'` with `W = [Y θS]`; the small matrices are formed densely.

use crate::{stats_optimize::Objective, unwind::{R, error}};

pub struct Settings {
    pub memory: usize,
    pub factor: f64,
    pub projected_tolerance: f64,
    pub max_iterations: i32,
    pub trace: i32,
    pub report: i32,
}

pub struct Outcome {
    pub solution: Vec<f64>,
    pub value: f64,
    pub function_count: usize,
    pub gradient_count: usize,
    pub fail: i32,
    pub message: String,
}

/// Which bounds a variable has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bound {
    None,
    Lower,
    Both,
    Upper,
}

impl Bound {
    fn has_lower(self) -> bool {
        matches!(self, Bound::Lower | Bound::Both)
    }

    fn has_upper(self) -> bool {
        matches!(self, Bound::Upper | Bound::Both)
    }
}

pub(crate) unsafe fn minimize(objective: &mut dyn Objective, start: &[f64], lower: &[f64], upper: &[f64], settings: &Settings) -> R<Outcome> {
    let n = start.len();
    let bounds: Vec<Bound> = (0..n)
        .map(|i| match (lower[i].is_finite(), upper[i].is_finite()) {
            (false, false) => Bound::None,
            (true, false) => Bound::Lower,
            (true, true) => Bound::Both,
            (false, true) => Bound::Upper,
        })
        .collect();
    let mut state = State {
        n,
        lower: lower.to_vec(),
        upper: upper.to_vec(),
        bounds,
        memory: settings.memory,
        s: Vec::new(),
        y: Vec::new(),
        theta: 1.0,
    };
    let trace = settings.trace > 0;
    if (0..n).any(|i| state.bounds[i] == Bound::Both && lower[i] > upper[i]) {
        return Ok(Outcome {
            solution: start.to_vec(),
            value: f64::NAN,
            function_count: 0,
            gradient_count: 0,
            fail: 52,
            message: "ERROR: NO FEASIBLE SOLUTION".to_owned(),
        });
    }
    let constrained = state.bounds.iter().any(|&bound| bound != Bound::None);
    let boxed = state.bounds.iter().all(|&bound| bound == Bound::Both);
    let mut x: Vec<f64> = (0..n).map(|i| state.project(i, start[i])).collect();
    let mut evaluations = 0usize;
    let mut f = objective.value(&x)?;
    if !f.is_finite() {
        return Err(error("L-BFGS-B needs finite values of 'fn'"));
    }
    let mut g = objective.gradient(&x)?;
    evaluations += 1;
    let tolerance = settings.factor * f64::EPSILON;
    let mut iteration = 0i32;
    let finish = |x: Vec<f64>, f: f64, evaluations: usize, fail: i32, message: &str, iteration: i32| {
        if trace {
            crate::write_console(format!("final  value {f:.6} \n").as_bytes(), false);
            if iteration < settings.max_iterations && fail == 0 {
                crate::write_console(b"converged\n", false);
            } else {
                crate::write_console(format!("stopped after {iteration} iterations\n").as_bytes(), false);
            }
        }
        Ok(Outcome { solution: x, value: f, function_count: evaluations, gradient_count: evaluations, fail, message: message.to_owned() })
    };
    if state.projected_gradient_norm(&x, &g) <= settings.projected_tolerance {
        return finish(x, f, evaluations, 0, "CONVERGENCE: NORM OF PROJECTED GRADIENT <= PGTOL", iteration);
    }
    loop {
        // The point the model is minimized at: the Cauchy point, refined over the free
        // variables, or for an unconstrained problem with memory the subspace step from x.
        let target = if !constrained && !state.s.is_empty() {
            let all: Vec<usize> = (0..n).collect();
            state.subspace_minimum(&x, &g, &x, &vec![0.0; 2 * state.s.len()], &all)
        } else {
            match state.cauchy_point(&x, &g) {
                Some((point, c, free)) => {
                    if free.is_empty() || state.s.is_empty() {
                        Some(point)
                    } else {
                        state.subspace_minimum(&x, &g, &point, &c, &free)
                    }
                }
                None => None,
            }
        };
        let Some(target) = target else {
            state.reset();
            continue;
        };
        let direction: Vec<f64> = (0..n).map(|i| target[i] - x[i]).collect();
        // The line search.
        let squared_norm: f64 = direction.iter().map(|value| value * value).sum();
        let norm = squared_norm.sqrt();
        let mut max_step = 1e10;
        if constrained {
            if iteration == 0 {
                max_step = 1.0;
            } else {
                for i in 0..n {
                    let step = direction[i];
                    let bound = state.bounds[i];
                    if step < 0.0 && bound.has_lower() {
                        let room = lower[i] - x[i];
                        if room >= 0.0 {
                            max_step = 0.0;
                        } else if step * max_step < room {
                            max_step = room / step;
                        }
                    } else if step > 0.0 && bound.has_upper() {
                        let room = upper[i] - x[i];
                        if room <= 0.0 {
                            max_step = 0.0;
                        } else if step * max_step > room {
                            max_step = room / step;
                        }
                    }
                }
            }
        }
        let mut step = if iteration == 0 && !boxed { (1.0 / norm).min(max_step) } else { 1.0 };
        let (x_old, g_old, f_old) = (x.clone(), g.clone(), f);
        let initial_slope: f64 = g.iter().zip(&direction).map(|(a, b)| a * b).sum();
        let mut search = LineSearch::new();
        let mut trials = 0usize;
        let mut slope = initial_slope;
        let mut failed = initial_slope >= 0.0;
        if !failed {
            loop {
                match search.step(&mut step, f, slope, 1e-3, 0.9, 0.1, 0.0, max_step, trials == 0) {
                    Search::Converged => break,
                    Search::Error => {
                        failed = true;
                        break;
                    }
                    Search::Evaluate => {}
                }
                if trials >= 20 {
                    failed = true;
                    break;
                }
                trials += 1;
                x = if step == 1.0 { target.clone() } else { (0..n).map(|i| step * direction[i] + x_old[i]).collect() };
                evaluations += 1;
                f = objective.value(&x)?;
                if !f.is_finite() {
                    return Err(error("L-BFGS-B needs finite values of 'fn'"));
                }
                g = objective.gradient(&x)?;
                slope = g.iter().zip(&direction).map(|(a, b)| a * b).sum();
            }
        }
        if failed {
            x = x_old;
            g = g_old;
            f = f_old;
            if state.s.is_empty() {
                return finish(x, f, evaluations, 52, "ABNORMAL_TERMINATION_IN_LNSRCH", iteration + 1);
            }
            state.reset();
            continue;
        }
        iteration += 1;
        if trace && iteration % settings.report == 0 {
            crate::write_console(format!("iter {iteration:4} value {f:.6}\n").as_bytes(), false);
        }
        if iteration > settings.max_iterations {
            return finish(x, f, evaluations, 1, "NEW_X", iteration);
        }
        if state.projected_gradient_norm(&x, &g) <= settings.projected_tolerance {
            return finish(x, f, evaluations, 0, "CONVERGENCE: NORM OF PROJECTED GRADIENT <= PGTOL", iteration);
        }
        let scale = f_old.abs().max(f.abs()).max(1.0);
        if f_old - f <= tolerance * scale {
            return finish(x, f, evaluations, 0, "CONVERGENCE: REL_REDUCTION_OF_F <= FACTR*EPSMCH", iteration);
        }
        let y: Vec<f64> = (0..n).map(|i| g[i] - g_old[i]).collect();
        let s: Vec<f64> = (0..n).map(|i| step * direction[i]).collect();
        let yy: f64 = y.iter().map(|value| value * value).sum();
        let (sy, threshold) = if step == 1.0 {
            (slope - initial_slope, -initial_slope)
        } else {
            ((slope - initial_slope) * step, -initial_slope * step)
        };
        if sy <= f64::EPSILON * threshold {
            continue;
        }
        if state.s.len() == state.memory {
            state.s.remove(0);
            state.y.remove(0);
        }
        state.s.push(s);
        state.y.push(y);
        state.theta = yy / sy;
        if state.middle().is_none() {
            state.reset();
        }
    }
}

struct State {
    n: usize,
    lower: Vec<f64>,
    upper: Vec<f64>,
    bounds: Vec<Bound>,
    memory: usize,
    /// The stored corrections, oldest first.
    s: Vec<Vec<f64>>,
    y: Vec<Vec<f64>>,
    theta: f64,
}

impl State {
    fn project(&self, i: usize, value: f64) -> f64 {
        let mut value = value;
        if self.bounds[i].has_lower() {
            value = value.max(self.lower[i]);
        }
        if self.bounds[i].has_upper() {
            value = value.min(self.upper[i]);
        }
        value
    }

    fn reset(&mut self) {
        self.s.clear();
        self.y.clear();
        self.theta = 1.0;
    }

    /// The infinity norm of the projected gradient.
    fn projected_gradient_norm(&self, x: &[f64], g: &[f64]) -> f64 {
        let mut norm = 0.0_f64;
        for i in 0..self.n {
            let mut component = g[i];
            if component < 0.0 {
                if self.bounds[i].has_upper() {
                    component = (x[i] - self.upper[i]).max(component);
                }
            } else if self.bounds[i].has_lower() {
                component = (x[i] - self.lower[i]).min(component);
            }
            norm = norm.max(component.abs());
        }
        norm
    }

    /// Row `i` of `W = [Y θS]`.
    fn w_row(&self, i: usize) -> Vec<f64> {
        let k = self.s.len();
        let mut row = vec![0.0; 2 * k];
        for j in 0..k {
            row[j] = self.y[j][i];
            row[k + j] = self.theta * self.s[j][i];
        }
        row
    }

    /// The middle matrix `M = [[-D, L'], [L, θ S'S]]^-1`, row-major, or `None` when singular.
    fn middle(&self) -> Option<Vec<f64>> {
        let k = self.s.len();
        let size = 2 * k;
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
        let mut matrix = vec![0.0; size * size];
        for i in 0..k {
            for j in 0..k {
                let sy = dot(&self.s[i], &self.y[j]);
                if i == j {
                    matrix[i * size + j] = -sy;
                }
                if i > j {
                    // L, the strictly lower part of S'Y, and its transpose.
                    matrix[(k + i) * size + j] = sy;
                    matrix[j * size + k + i] = sy;
                }
                matrix[(k + i) * size + k + j] = self.theta * dot(&self.s[i], &self.s[j]);
            }
        }
        // T = θ S'S + L D^-1 L' must be positive definite for the update to be usable.
        let mut t = vec![0.0; k * k];
        for i in 0..k {
            for j in 0..k {
                let mut value = matrix[(k + i) * size + k + j];
                for m in 0..k.min(i).min(j) {
                    let d = dot(&self.s[m], &self.y[m]);
                    value += dot(&self.s[i], &self.y[m]) * dot(&self.s[j], &self.y[m]) / d;
                }
                t[i * k + j] = value;
            }
        }
        cholesky(&mut t, k)?;
        invert(&matrix, size)
    }

    /// The generalized Cauchy point: the first local minimizer of the quadratic model along
    /// the projected steepest descent path `P(x - t g)`. Returns it, `c = W'(xcp - x)` and the
    /// variables free there.
    fn cauchy_point(&self, x: &[f64], g: &[f64]) -> Option<(Vec<f64>, Vec<f64>, Vec<usize>)> {
        let n = self.n;
        let k = self.s.len();
        let middle = if k > 0 { self.middle()? } else { Vec::new() };
        let size = 2 * k;
        let multiply = |v: &[f64]| -> Vec<f64> { (0..size).map(|i| (0..size).map(|j| middle[i * size + j] * v[j]).sum()).collect() };
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
        let mut breakpoints: Vec<(f64, usize)> = Vec::new();
        let mut direction = vec![0.0; n];
        let mut point = x.to_vec();
        for i in 0..n {
            let bound = self.bounds[i];
            let t = if g[i] < 0.0 && bound.has_upper() {
                (x[i] - self.upper[i]) / g[i]
            } else if g[i] > 0.0 && bound.has_lower() {
                (x[i] - self.lower[i]) / g[i]
            } else {
                f64::INFINITY
            };
            if t > 0.0 {
                direction[i] = -g[i];
            }
            if t > 0.0 && t.is_finite() {
                breakpoints.push((t, i));
            }
        }
        let mut p = vec![0.0; size];
        for i in 0..n {
            if direction[i] != 0.0 {
                let row = self.w_row(i);
                for j in 0..size {
                    p[j] += row[j] * direction[i];
                }
            }
        }
        let mut c = vec![0.0; size];
        let mut slope = -dot(&direction, &direction);
        if slope == 0.0 {
            let free: Vec<usize> = (0..n).filter(|&i| self.is_free(i, &point)).collect();
            return Some((point, c, free));
        }
        let mp = multiply(&p);
        let mut curvature = -self.theta * slope - dot(&p, &mp);
        let mut minimum_step = -slope / curvature;
        let mut previous = 0.0;
        breakpoints.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut index = 0;
        while index < breakpoints.len() {
            let (t, b) = breakpoints[index];
            let interval = t - previous;
            if minimum_step < interval {
                break;
            }
            point[b] = if direction[b] > 0.0 { self.upper[b] } else { self.lower[b] };
            let z = point[b] - x[b];
            for j in 0..size {
                c[j] += interval * p[j];
            }
            let w = self.w_row(b);
            let mc = multiply(&c);
            let mp = multiply(&p);
            let mw = multiply(&w);
            let gb = g[b];
            slope += interval * curvature + gb * gb + self.theta * gb * z - gb * dot(&w, &mc);
            curvature -= self.theta * gb * gb + 2.0 * gb * dot(&w, &mp) + gb * gb * dot(&w, &mw);
            for j in 0..size {
                p[j] += gb * w[j];
            }
            direction[b] = 0.0;
            minimum_step = -slope / curvature;
            previous = t;
            index += 1;
            if index == breakpoints.len() {
                previous = t;
            }
        }
        let minimum_step = minimum_step.max(0.0);
        let total = previous + minimum_step;
        for i in 0..n {
            if direction[i] != 0.0 {
                point[i] = x[i] + total * direction[i];
            }
        }
        for j in 0..size {
            c[j] += minimum_step * p[j];
        }
        let free: Vec<usize> = (0..n).filter(|&i| self.is_free(i, &point)).collect();
        Some((point, c, free))
    }

    fn is_free(&self, i: usize, point: &[f64]) -> bool {
        let bound = self.bounds[i];
        !((bound.has_lower() && point[i] <= self.lower[i]) || (bound.has_upper() && point[i] >= self.upper[i]))
    }

    /// The direct primal subspace minimization: the minimizer of the quadratic model over the
    /// `free` variables from `point`, then a step back towards `point` to stay within the box.
    fn subspace_minimum(&self, x: &[f64], g: &[f64], point: &[f64], c: &[f64], free: &[usize]) -> Option<Vec<f64>> {
        let k = self.s.len();
        let size = 2 * k;
        let middle = self.middle()?;
        let multiply = |v: &[f64]| -> Vec<f64> { (0..size).map(|i| (0..size).map(|j| middle[i * size + j] * v[j]).sum()).collect() };
        let mc = multiply(c);
        let theta = self.theta;
        // The reduced gradient of the model at `point`: Z'(g + θ(xcp - x) - W M c).
        let reduced: Vec<f64> = free
            .iter()
            .map(|&i| {
                let w = self.w_row(i);
                -(g[i] + theta * (point[i] - x[i]) - w.iter().zip(&mc).map(|(a, b)| a * b).sum::<f64>())
            })
            .collect();
        // (θI - Z'W M W'Z)^-1 r = r/θ + (1/θ²) Z'W (I - (1/θ) M W'Z Z'W)^-1 M W'Z r.
        let mut wz_r = vec![0.0; size];
        let mut wtw = vec![0.0; size * size];
        for (position, &i) in free.iter().enumerate() {
            let w = self.w_row(i);
            for a in 0..size {
                wz_r[a] += w[a] * reduced[position];
                for b in 0..size {
                    wtw[a * size + b] += w[a] * w[b];
                }
            }
        }
        let mut inner = vec![0.0; size * size];
        for a in 0..size {
            for b in 0..size {
                let product: f64 = (0..size).map(|m| middle[a * size + m] * wtw[m * size + b]).sum();
                inner[a * size + b] = if a == b { 1.0 } else { 0.0 } - product / theta;
            }
        }
        let mut rhs = multiply(&wz_r);
        let correction = solve_dense(&mut inner, &mut rhs, size)?;
        let mut step: Vec<f64> = free
            .iter()
            .enumerate()
            .map(|(position, &i)| {
                let w = self.w_row(i);
                reduced[position] / theta + w.iter().zip(&correction).map(|(a, b)| a * b).sum::<f64>() / (theta * theta)
            })
            .collect();
        let mut result = point.to_vec();
        let mut alpha = 1.0_f64;
        let mut limiting = None;
        for (position, &i) in free.iter().enumerate() {
            let d = step[position];
            let bound = self.bounds[i];
            let mut candidate = alpha;
            if d < 0.0 && bound.has_lower() {
                let room = self.lower[i] - point[i];
                if room >= 0.0 {
                    candidate = 0.0;
                } else if d * alpha < room {
                    candidate = room / d;
                }
            } else if d > 0.0 && bound.has_upper() {
                let room = self.upper[i] - point[i];
                if room <= 0.0 {
                    candidate = 0.0;
                } else if d * alpha > room {
                    candidate = room / d;
                }
            }
            if candidate < alpha {
                alpha = candidate;
                limiting = Some(position);
            }
        }
        if alpha < 1.0 {
            if let Some(position) = limiting {
                let i = free[position];
                result[i] = if step[position] > 0.0 { self.upper[i] } else { self.lower[i] };
                step[position] = 0.0;
            }
        }
        for (position, &i) in free.iter().enumerate() {
            result[i] += alpha * step[position];
        }
        Some(result)
    }
}

fn cholesky(matrix: &mut [f64], n: usize) -> Option<()> {
    for j in 0..n {
        let mut diagonal = matrix[j * n + j];
        for k in 0..j {
            diagonal -= matrix[j * n + k] * matrix[j * n + k];
        }
        if diagonal <= 0.0 {
            return None;
        }
        let diagonal = diagonal.sqrt();
        matrix[j * n + j] = diagonal;
        for i in j + 1..n {
            let mut value = matrix[i * n + j];
            for k in 0..j {
                value -= matrix[i * n + k] * matrix[j * n + k];
            }
            matrix[i * n + j] = value / diagonal;
        }
    }
    Some(())
}

fn solve_dense(matrix: &mut [f64], rhs: &mut [f64], n: usize) -> Option<Vec<f64>> {
    for column in 0..n {
        let pivot = (column..n).max_by(|&a, &b| matrix[a * n + column].abs().total_cmp(&matrix[b * n + column].abs()))?;
        if matrix[pivot * n + column] == 0.0 {
            return None;
        }
        if pivot != column {
            for k in 0..n {
                matrix.swap(pivot * n + k, column * n + k);
            }
            rhs.swap(pivot, column);
        }
        for row in column + 1..n {
            let factor = matrix[row * n + column] / matrix[column * n + column];
            for k in column..n {
                matrix[row * n + k] -= factor * matrix[column * n + k];
            }
            rhs[row] -= factor * rhs[column];
        }
    }
    let mut solution = vec![0.0; n];
    for row in (0..n).rev() {
        let mut value = rhs[row];
        for k in row + 1..n {
            value -= matrix[row * n + k] * solution[k];
        }
        solution[row] = value / matrix[row * n + row];
    }
    Some(solution)
}

fn invert(matrix: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut inverse = vec![0.0; n * n];
    for column in 0..n {
        let mut copy = matrix.to_vec();
        let mut unit = vec![0.0; n];
        unit[column] = 1.0;
        let solution = solve_dense(&mut copy, &mut unit, n)?;
        for row in 0..n {
            inverse[row * n + column] = solution[row];
        }
    }
    Some(inverse)
}

enum Search {
    Evaluate,
    Converged,
    Error,
}

/// Moré and Thuente's line search ("Line search algorithms with guaranteed sufficient
/// decrease", ACM TOMS 1994, MINPACK-2's `dcsrch`), driven one function value at a time.
struct LineSearch {
    bracketed: bool,
    stage: u8,
    f_initial: f64,
    g_initial: f64,
    g_test: f64,
    width: f64,
    width1: f64,
    stx: f64,
    fx: f64,
    gx: f64,
    sty: f64,
    fy: f64,
    gy: f64,
    stmin: f64,
    stmax: f64,
}

impl LineSearch {
    fn new() -> LineSearch {
        LineSearch {
            bracketed: false,
            stage: 1,
            f_initial: 0.0,
            g_initial: 0.0,
            g_test: 0.0,
            width: 0.0,
            width1: 0.0,
            stx: 0.0,
            fx: 0.0,
            gx: 0.0,
            sty: 0.0,
            fy: 0.0,
            gy: 0.0,
            stmin: 0.0,
            stmax: 0.0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn step(&mut self, stp: &mut f64, f: f64, g: f64, ftol: f64, gtol: f64, xtol: f64, stpmin: f64, stpmax: f64, start: bool) -> Search {
        if start {
            if *stp < stpmin || *stp > stpmax || g >= 0.0 {
                return Search::Error;
            }
            self.bracketed = false;
            self.stage = 1;
            self.f_initial = f;
            self.g_initial = g;
            self.g_test = ftol * g;
            self.width = stpmax - stpmin;
            self.width1 = self.width / 0.5;
            self.stx = 0.0;
            self.fx = f;
            self.gx = g;
            self.sty = 0.0;
            self.fy = f;
            self.gy = g;
            self.stmin = 0.0;
            self.stmax = *stp + 4.0 * *stp;
            return Search::Evaluate;
        }
        let f_test = self.f_initial + *stp * self.g_test;
        if self.stage == 1 && f <= f_test && g >= 0.0 {
            self.stage = 2;
        }
        let mut warning = false;
        if self.bracketed && (*stp <= self.stmin || *stp >= self.stmax) {
            warning = true;
        }
        if self.bracketed && self.stmax - self.stmin <= xtol * self.stmax {
            warning = true;
        }
        if *stp == stpmax && f <= f_test && g <= self.g_test {
            warning = true;
        }
        if *stp == stpmin && (f > f_test || g >= self.g_test) {
            warning = true;
        }
        if f <= f_test && g.abs() <= gtol * (-self.g_initial) {
            return Search::Converged;
        }
        if warning {
            return Search::Converged;
        }
        if self.stage == 1 && f <= self.fx && f > f_test {
            let mut fm = f - *stp * self.g_test;
            let mut fxm = self.fx - self.stx * self.g_test;
            let mut fym = self.fy - self.sty * self.g_test;
            let gm = g - self.g_test;
            let mut gxm = self.gx - self.g_test;
            let mut gym = self.gy - self.g_test;
            dcstep(&mut self.stx, &mut fxm, &mut gxm, &mut self.sty, &mut fym, &mut gym, stp, &mut fm, gm, &mut self.bracketed, self.stmin, self.stmax);
            self.fx = fxm + self.stx * self.g_test;
            self.fy = fym + self.sty * self.g_test;
            self.gx = gxm + self.g_test;
            self.gy = gym + self.g_test;
        } else {
            let mut fp = f;
            dcstep(&mut self.stx, &mut self.fx, &mut self.gx, &mut self.sty, &mut self.fy, &mut self.gy, stp, &mut fp, g, &mut self.bracketed, self.stmin, self.stmax);
        }
        if self.bracketed {
            if (self.sty - self.stx).abs() >= 0.66 * self.width1 {
                *stp = self.stx + 0.5 * (self.sty - self.stx);
            }
            self.width1 = self.width;
            self.width = (self.sty - self.stx).abs();
        }
        if self.bracketed {
            self.stmin = self.stx.min(self.sty);
            self.stmax = self.stx.max(self.sty);
        } else {
            self.stmin = *stp + 1.1 * (*stp - self.stx);
            self.stmax = *stp + 4.0 * (*stp - self.stx);
        }
        *stp = stp.max(stpmin).min(stpmax);
        if (self.bracketed && (*stp <= self.stmin || *stp >= self.stmax)) || (self.bracketed && self.stmax - self.stmin <= xtol * self.stmax) {
            *stp = self.stx;
        }
        Search::Evaluate
    }
}

/// MINPACK-2's `dcstep`: a safeguarded cubic or quadratic step for the line search, updating
/// the interval `[stx, sty]` that contains a step satisfying the conditions.
#[allow(clippy::too_many_arguments)]
fn dcstep(stx: &mut f64, fx: &mut f64, dx: &mut f64, sty: &mut f64, fy: &mut f64, dy: &mut f64, stp: &mut f64, fp: &mut f64, dp: f64, bracketed: &mut bool, stpmin: f64, stpmax: f64) {
    let sign = dp * (*dx / dx.abs());
    let stpf;
    if *fp > *fx {
        let theta = 3.0 * (*fx - *fp) / (*stp - *stx) + *dx + dp;
        let s = theta.abs().max(dx.abs()).max(dp.abs());
        let mut gamma = s * ((theta / s).powi(2) - (*dx / s) * (dp / s)).sqrt();
        if *stp < *stx {
            gamma = -gamma;
        }
        let p = (gamma - *dx) + theta;
        let q = ((gamma - *dx) + gamma) + dp;
        let r = p / q;
        let stpc = *stx + r * (*stp - *stx);
        let stpq = *stx + ((*dx / ((*fx - *fp) / (*stp - *stx) + *dx)) / 2.0) * (*stp - *stx);
        stpf = if (stpc - *stx).abs() < (stpq - *stx).abs() { stpc } else { stpc + (stpq - stpc) / 2.0 };
        *bracketed = true;
    } else if sign < 0.0 {
        let theta = 3.0 * (*fx - *fp) / (*stp - *stx) + *dx + dp;
        let s = theta.abs().max(dx.abs()).max(dp.abs());
        let mut gamma = s * ((theta / s).powi(2) - (*dx / s) * (dp / s)).sqrt();
        if *stp > *stx {
            gamma = -gamma;
        }
        let p = (gamma - dp) + theta;
        let q = ((gamma - dp) + gamma) + *dx;
        let r = p / q;
        let stpc = *stp + r * (*stx - *stp);
        let stpq = *stp + (dp / (dp - *dx)) * (*stx - *stp);
        stpf = if (stpc - *stp).abs() > (stpq - *stp).abs() { stpc } else { stpq };
        *bracketed = true;
    } else if dp.abs() < dx.abs() {
        let theta = 3.0 * (*fx - *fp) / (*stp - *stx) + *dx + dp;
        let s = theta.abs().max(dx.abs()).max(dp.abs());
        let mut gamma = s * (0.0f64).max((theta / s).powi(2) - (*dx / s) * (dp / s)).sqrt();
        if *stp > *stx {
            gamma = -gamma;
        }
        let p = (gamma - dp) + theta;
        let q = (gamma + (*dx - dp)) + gamma;
        let r = p / q;
        let stpc = if r < 0.0 && gamma != 0.0 {
            *stp + r * (*stx - *stp)
        } else if *stp > *stx {
            stpmax
        } else {
            stpmin
        };
        let stpq = *stp + (dp / (dp - *dx)) * (*stx - *stp);
        if *bracketed {
            let mut chosen = if (stpc - *stp).abs() < (stpq - *stp).abs() { stpc } else { stpq };
            if *stp > *stx {
                chosen = chosen.min(*stp + 0.66 * (*sty - *stp));
            } else {
                chosen = chosen.max(*stp + 0.66 * (*sty - *stp));
            }
            stpf = chosen;
        } else {
            let chosen = if (stpc - *stp).abs() > (stpq - *stp).abs() { stpc } else { stpq };
            stpf = chosen.min(stpmax).max(stpmin);
        }
    } else if *bracketed {
        let theta = 3.0 * (*fp - *fy) / (*sty - *stp) + *dy + dp;
        let s = theta.abs().max(dy.abs()).max(dp.abs());
        let mut gamma = s * ((theta / s).powi(2) - (*dy / s) * (dp / s)).sqrt();
        if *stp > *sty {
            gamma = -gamma;
        }
        let p = (gamma - dp) + theta;
        let q = ((gamma - dp) + gamma) + *dy;
        let r = p / q;
        stpf = *stp + r * (*sty - *stp);
    } else if *stp > *stx {
        stpf = stpmax;
    } else {
        stpf = stpmin;
    }
    if *fp > *fx {
        *sty = *stp;
        *fy = *fp;
        *dy = dp;
    } else {
        if sign < 0.0 {
            *sty = *stx;
            *fy = *fx;
            *dy = *dx;
        }
        *stx = *stp;
        *fx = *fp;
        *dx = dp;
    }
    *stp = stpf;
}
