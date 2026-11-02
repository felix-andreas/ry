//! L-BFGS-B, the limited memory quasi-Newton method for bound constrained minimization behind
//! `optim(method = "L-BFGS-B")` and the C API's `lbfgsb`.
//!
//! This follows the authors' BSD-licensed reference code, L-BFGS-B 3.0 by Zhu, Byrd, Lu and
//! Nocedal (ACM TOMS Algorithm 778), routine by routine, because `optim()` results are
//! compared digit for digit against GNU R. Each routine keeps its reference name in its doc
//! comment. The vector kernels are written as the reference BLAS and LINPACK loops they stand
//! for, so every sum is accumulated in the same order.
//!
//! GNU R's results match an older variant of the method, which comparison against it pins
//! down: the subspace step backtracks into the box without first trying its projection (the
//! Morales and Nocedal remark that version 3.0 added), and a few updates add their whole
//! right-hand side as one term (`r += a + b` rather than `r = r + a + b`). Comments mark each
//! of those places.
//!
//! The reference code keeps its whole state in work arrays that persist between iterations,
//! and several paths read values a previous iteration left behind (the Cauchy coefficients
//! after an early return, the line search state after a rejected start). The state here
//! persists the same way, so those paths behave as they do there.

use {
    crate::unwind::{R, error},
    std::ffi::CStr,
};

/// The objective and its gradient as the minimizer evaluates them.
pub trait Objective {
    unsafe fn value(&mut self, x: &[f64]) -> R<f64>;
    unsafe fn gradient(&mut self, x: &[f64], gradient: &mut [f64]) -> R<()>;
}

/// Which bounds a variable has (the reference code's `nbd` 0, 1, 2 and 3).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    None,
    Lower,
    Both,
    Upper,
}

impl Bound {
    /// The bounds `optim()` gives a variable: each finite limit is a bound.
    pub fn from_limits(lower: f64, upper: f64) -> Bound {
        match (lower.is_finite(), upper.is_finite()) {
            (false, false) => Bound::None,
            (true, false) => Bound::Lower,
            (true, true) => Bound::Both,
            (false, true) => Bound::Upper,
        }
    }

    fn has_lower(self) -> bool {
        matches!(self, Bound::Lower | Bound::Both)
    }

    fn has_upper(self) -> bool {
        matches!(self, Bound::Upper | Bound::Both)
    }
}

pub struct Settings {
    /// The number of corrections kept, `lmm`.
    pub memory: i32,
    pub factr: f64,
    pub pgtol: f64,
    pub max_iterations: i32,
    pub trace: i32,
    pub report: i32,
}

pub struct Outcome {
    pub value: f64,
    pub function_count: i32,
    pub gradient_count: i32,
    /// 0 on convergence, 1 when the iteration limit was reached, 52 for an error.
    pub fail: i32,
    pub message: &'static str,
}

/// Minimizes the objective from `x` within the bounds, leaving the solution in `x`.
pub unsafe fn minimize(
    objective: &mut dyn Objective,
    x: &mut [f64],
    lower: &[f64],
    upper: &[f64],
    bounds: &[Bound],
    settings: &Settings,
) -> R<Outcome> {
    let n = x.len();
    if n == 0 {
        let value = objective.value(x)?;
        return Ok(Outcome {
            value,
            function_count: 1,
            gradient_count: 0,
            fail: 0,
            message: "NOTHING TO DO",
        });
    }
    // GNU R maps its trace levels onto the reference code's `iprint`.
    let detail = match settings.trace {
        2 => 0,
        3 => settings.report,
        4 => 99,
        5 => 100,
        6 => 101,
        _ => -1,
    };
    // GNU R's driver reports the outcome after the reference code's own summary.
    let finish = |value: f64, iterations: i32, fail: i32| {
        if settings.trace != 0 {
            print(&format!("final  value {value:.6} \n"));
            if iterations < settings.max_iterations && fail == 0 {
                print("converged\n");
            } else {
                print(&format!("stopped after {iterations} iterations\n"));
            }
        }
    };

    // errclb: arguments the algorithm cannot run with. A later problem overrides an earlier one.
    let mut invalid = None;
    if settings.memory <= 0 {
        invalid = Some("ERROR: M .LE. 0");
    }
    if settings.factr < 0.0 {
        invalid = Some("ERROR: FACTR .LT. 0");
    }
    let mut infeasible = None;
    for i in 0..n {
        if bounds[i] == Bound::Both && lower[i] > upper[i] {
            invalid = Some("ERROR: NO FEASIBLE SOLUTION");
            infeasible = Some(i);
        }
    }
    if let Some(message) = invalid {
        if detail >= 0
            && let Some(i) = infeasible
        {
            print(&format!(
                "l({}) > u({}).  No feasible solution",
                i + 1,
                i + 1
            ));
        }
        finish(0.0, 0, 52);
        return Ok(Outcome {
            value: 0.0,
            function_count: 0,
            gradient_count: 0,
            fail: 52,
            message,
        });
    }

    let m = settings.memory as usize;
    let epsilon = f64::EPSILON;
    let tolerance = settings.factr * epsilon;
    let mut memory = Memory {
        n,
        m,
        ws: vec![0.0; n * m],
        wy: vec![0.0; n * m],
        sy: vec![0.0; m * m],
        ss: vec![0.0; m * m],
        wt: vec![0.0; m * m],
        theta: 1.0,
        col: 0,
        head: 0,
        tail: 0,
        updates: 0,
        updated: false,
    };
    let mut wn = vec![0.0; 4 * m * m];
    let mut wn1 = vec![0.0; 4 * m * m];
    let mut z = vec![0.0; n];
    let mut r = vec![0.0; n];
    let mut d = vec![0.0; n];
    let mut t = vec![0.0; n];
    let mut p = vec![0.0; 2 * m];
    let mut c = vec![0.0; 2 * m];
    let mut wbp = vec![0.0; 2 * m];
    let mut v = vec![0.0; 2 * m];
    let mut index: Vec<usize> = vec![0; n];
    let mut where_: Vec<Where> = vec![Where::Free; n];
    let mut changes = Changes {
        indices: vec![0; n],
        entering: 0,
        leaving: n,
    };
    let mut search = LineSearch::default();
    let mut g = vec![0.0; n];

    let mut iteration = 0i32;
    let mut evaluations = 0i32;
    let mut segments_total = 0i32;
    let mut skipped = 0i32;
    let mut free = n;
    let mut active = 0usize;
    let mut segments = 0i32;

    // prn1lb
    if detail >= 0 {
        print(&format!(
            "N = {n}, M = {m} machine precision = {}\n",
            number(c"%g", epsilon)
        ));
        if detail >= 100 {
            print_vector("L = ", lower);
            print_vector("X0 = ", x);
            print_vector("U = ", upper);
        }
    }
    let shape = initialize_active(x, lower, upper, bounds, &mut where_, detail);

    let mut f = evaluate(objective, x, &mut g)?;
    evaluations += 1;
    let mut projected = projected_gradient_norm(x, &g, lower, upper, bounds);
    if detail >= 1 {
        print(&format!(
            "At iterate {iteration:5}  f= {}  |proj g|= {}\n",
            number(c"%12.5g", f),
            number(c"%12.5g", projected)
        ));
    }
    let mut info = 0i32;
    let task: &'static str;
    'iterations: {
        if projected <= settings.pgtol {
            task = CONVERGED_GRADIENT;
            break 'iterations;
        }
        loop {
            if detail >= 99 {
                print(&format!("Iteration {iteration:5}\n"));
            }
            let rebuild;
            if !shape.constrained && memory.col > 0 {
                z.copy_from_slice(x);
                rebuild = memory.updated;
                segments = 0;
            } else {
                let status = cauchy(
                    x,
                    lower,
                    upper,
                    bounds,
                    &g,
                    &mut changes.indices,
                    &mut where_,
                    &mut t,
                    &mut d,
                    &mut z,
                    &memory,
                    Workspace {
                        p: &mut p,
                        c: &mut c,
                        wbp: &mut wbp,
                        v: &mut v,
                    },
                    &mut segments,
                    detail,
                    projected,
                    epsilon,
                );
                if status != 0 {
                    if detail >= 1 {
                        print_error(SINGULAR_RESTART);
                    }
                    memory.reset();
                    continue;
                }
                segments_total += segments;
                rebuild = free_variables(
                    &mut free,
                    &mut index,
                    &mut changes,
                    &where_,
                    memory.updated,
                    shape.constrained,
                    detail,
                    iteration,
                );
                active = n - free;
            }
            if free != 0 && memory.col != 0 {
                if rebuild {
                    info = form_k(free, &index, &changes, &mut wn, &mut wn1, &memory);
                }
                if info != 0 {
                    if detail >= 1 {
                        print_error(INDEFINITE_K_RESTART);
                    }
                    info = 0;
                    memory.reset();
                    continue;
                }
                info = reduced_gradient(
                    x,
                    &g,
                    &memory,
                    &z,
                    &mut r,
                    &c,
                    &mut p,
                    &index,
                    free,
                    shape.constrained,
                );
                if info == 0 {
                    info = subspace_minimization(
                        free, &index, lower, upper, bounds, &mut z, &mut r, &memory, &mut p, &wn,
                    );
                }
                if info != 0 {
                    if detail >= 1 {
                        print_error(SINGULAR_RESTART);
                    }
                    info = 0;
                    memory.reset();
                    continue;
                }
            }
            for i in 0..n {
                d[i] = z[i] - x[i];
            }

            let dtd = ddot(&d, &d);
            let norm = dtd.sqrt();
            let mut step_max = BIG_STEP;
            if shape.constrained {
                if iteration == 0 {
                    step_max = 1.0;
                } else {
                    for i in 0..n {
                        let a1 = d[i];
                        if bounds[i] == Bound::None {
                            continue;
                        }
                        if a1 < 0.0 && bounds[i].has_lower() {
                            let a2 = lower[i] - x[i];
                            if a2 >= 0.0 {
                                step_max = 0.0;
                            } else if a1 * step_max < a2 {
                                step_max = a2 / a1;
                            }
                        } else if a1 > 0.0 && bounds[i].has_upper() {
                            let a2 = upper[i] - x[i];
                            if a2 <= 0.0 {
                                step_max = 0.0;
                            } else if a1 * step_max > a2 {
                                step_max = a2 / a1;
                            }
                        }
                    }
                }
            }
            let mut step = if iteration == 0 && !shape.boxed {
                fmin(1.0 / norm, step_max)
            } else {
                1.0
            };
            t.copy_from_slice(x);
            r.copy_from_slice(&g);
            let f_old = f;
            let mut trials = 0i32;
            let mut backtracks = 0;
            let mut starting = true;
            let mut gd;
            let mut gd_old = 0.0;
            let mut step_norm = 0.0;
            loop {
                gd = ddot(&g, &d);
                if trials == 0 {
                    gd_old = gd;
                    if gd >= 0.0 {
                        info = -4;
                        break;
                    }
                }
                let status = search.step(starting, f, gd, &mut step, 0.0, step_max);
                starting = false;
                step_norm = step * norm;
                if matches!(status, Search::Converged | Search::Warning) {
                    break;
                }
                trials += 1;
                evaluations += 1;
                backtracks = trials - 1;
                if backtracks >= MAX_BACKTRACKS {
                    break;
                }
                if step == 1.0 {
                    x.copy_from_slice(&z);
                } else {
                    for i in 0..n {
                        x[i] = step * d[i] + t[i];
                    }
                }
                f = evaluate(objective, x, &mut g)?;
            }
            if info != 0 || backtracks >= MAX_BACKTRACKS {
                x.copy_from_slice(&t);
                g.copy_from_slice(&r);
                f = f_old;
                if memory.col == 0 {
                    if info == 0 {
                        info = -9;
                        evaluations -= 1;
                    }
                    task = ABNORMAL;
                    iteration += 1;
                    break 'iterations;
                }
                if detail >= 1 {
                    print_error(BAD_DIRECTION_RESTART);
                }
                if info == 0 {
                    evaluations -= 1;
                }
                info = 0;
                memory.reset();
                continue;
            }
            iteration += 1;
            projected = projected_gradient_norm(x, &g, lower, upper, bounds);
            // prn2lb
            if detail >= 99 {
                print(&format!(
                    "LINE SEARCH {backtracks} times; norm of step = {}\n",
                    number(c"%g", step_norm)
                ));
                if detail > 100 {
                    print_vector("X = ", x);
                    print_vector("G = ", &g);
                }
            } else if detail > 0 && iteration % detail == 0 {
                print(&format!(
                    "At iterate {iteration:5}  f = {}  |proj g|=  {}\n",
                    number(c"%12.5g", f),
                    number(c"%12.5g", projected)
                ));
            }
            if settings.trace == 1 && iteration % settings.report == 0 {
                print(&format!("iter {iteration:4} value {f:.6}\n"));
            }
            if iteration > settings.max_iterations {
                finish(f, iteration, 1);
                return Ok(Outcome {
                    value: f,
                    function_count: evaluations,
                    gradient_count: evaluations,
                    fail: 1,
                    message: "NEW_X",
                });
            }
            if projected <= settings.pgtol {
                task = CONVERGED_GRADIENT;
                break 'iterations;
            }
            let scale = fmax(fmax(f_old.abs(), f.abs()), 1.0);
            if f_old - f <= tolerance * scale {
                task = CONVERGED_REDUCTION;
                if backtracks >= 10 {
                    info = -5;
                }
                break 'iterations;
            }
            for i in 0..n {
                r[i] = g[i] - r[i];
            }
            let rr = ddot(&r, &r);
            let (dr, threshold) = if step == 1.0 {
                (gd - gd_old, -gd_old)
            } else {
                for value in d.iter_mut() {
                    *value *= step;
                }
                ((gd - gd_old) * step, -gd_old * step)
            };
            if dr <= epsilon * threshold {
                skipped += 1;
                memory.updated = false;
                if detail >= 1 {
                    print(&format!(
                        "ys={}  -gs={}, BFGS update SKIPPED\n",
                        number(c"%10.3e", dr),
                        number(c"%10.3e", threshold)
                    ));
                }
                continue;
            }
            memory.updated = true;
            memory.updates += 1;
            update_matrices(&mut memory, &d, &r, rr, dr, step, dtd);
            if form_t(&mut memory) != 0 {
                if detail >= 1 {
                    print_error(INDEFINITE_T_RESTART);
                }
                memory.reset();
            }
        }
    }

    // prn3lb. GNU R replaces the summary of a failed line search with a one-line notice.
    if detail >= 0 && task == ABNORMAL {
        print(if info == -4 {
            DERIVATIVE_NOT_NEGATIVE
        } else {
            LINE_SEARCH_FAILED
        });
    } else if detail >= 0 {
        print(&format!(
            "\niterations {iteration}\nfunction evaluations {evaluations}\nsegments explored during Cauchy searches {segments_total}\nBFGS updates skipped {skipped}\nactive bounds at final generalized Cauchy point {active}\nnorm of the final projected gradient {}\nfinal function value {}\n\n",
            number(c"%g", projected),
            number(c"%g", f)
        ));
        if detail >= 100 {
            print_vector("X = ", x);
        }
        if detail >= 1 {
            print(&format!("F = {}\n", number(c"%g", f)));
        }
        if info == -5 {
            print(LONG_LINE_SEARCH);
        }
    }
    let reported = if task == ABNORMAL {
        iteration - 1
    } else {
        iteration
    };
    let fail = if task == ABNORMAL { 52 } else { 0 };
    finish(f, reported, fail);
    Ok(Outcome {
        value: f,
        function_count: evaluations,
        gradient_count: evaluations,
        fail,
        message: task,
    })
}

const CONVERGED_GRADIENT: &str = "CONVERGENCE: NORM OF PROJECTED GRADIENT <= PGTOL";
const CONVERGED_REDUCTION: &str = "CONVERGENCE: REL_REDUCTION_OF_F <= FACTR*EPSMCH";
const ABNORMAL: &str = "ERROR: ABNORMAL_TERMINATION_IN_LNSRCH";

const SINGULAR_RESTART: &str = "\n Singular triangular system detected;\n   refresh the lbfgs memory and restart the iteration.\n";
const INDEFINITE_K_RESTART: &str = "\n Nonpositive definiteness in Cholesky factorization in formk;\n   refresh the lbfgs memory and restart the iteration.\n";
const INDEFINITE_T_RESTART: &str = "\n Nonpositive definiteness in Cholesky factorization in formt;\n   refresh the lbfgs memory and restart the iteration.\n";
const BAD_DIRECTION_RESTART: &str = "\n Bad direction in the line search;\n   refresh the lbfgs memory and restart the iteration.\n";
const DERIVATIVE_NOT_NEGATIVE: &str = "Derivative >= 0, backtracking line search impossible.";
const LONG_LINE_SEARCH: &str =
    "Warning:  more than 10 function and gradient evaluations\n   in the last line search\n";
const LINE_SEARCH_FAILED: &str =
    "Line search cannot locate an adequate point after 20 function and gradient evaluations.";

/// The longest a line search may backtrack before the iteration gives up on its direction.
const MAX_BACKTRACKS: i32 = 20;
/// The step bound of an unconstrained line search.
const BIG_STEP: f64 = 1.0e10;

/// The limited memory: the last `col` corrections `s` (in `ws`) and `y` (in `wy`), stored
/// column-major in a ring that starts at column `head`, with `S'Y` (in `sy`), `S'S` (in
/// `ss`) and the Cholesky factor of `θS'S + LD⁻¹L'` (in `wt`).
struct Memory {
    n: usize,
    m: usize,
    ws: Vec<f64>,
    wy: Vec<f64>,
    sy: Vec<f64>,
    ss: Vec<f64>,
    wt: Vec<f64>,
    theta: f64,
    col: usize,
    head: usize,
    tail: usize,
    updates: usize,
    updated: bool,
}

impl Memory {
    fn reset(&mut self) {
        self.col = 0;
        self.head = 0;
        self.theta = 1.0;
        self.updates = 0;
        self.updated = false;
    }

    fn ws(&self, row: usize, column: usize) -> f64 {
        self.ws[row + self.n * column]
    }

    fn wy(&self, row: usize, column: usize) -> f64 {
        self.wy[row + self.n * column]
    }

    /// `sy(i, j)`, 1-based as in the reference code.
    fn sy(&self, i: usize, j: usize) -> f64 {
        self.sy[(i - 1) + self.m * (j - 1)]
    }

    fn next(&self, pointer: usize) -> usize {
        (pointer + 1) % self.m
    }
}

/// Where a variable stands relative to its bounds (the reference code's `iwhere`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    /// Free, with bounds (0).
    Free,
    /// Without bounds, so always free (-1).
    Unbounded,
    /// At its lower bound (1).
    AtLower,
    /// At its upper bound (2).
    AtUpper,
    /// Its bounds are equal (3).
    Fixed,
    /// Free with a zero gradient component (-3).
    ZeroGradient,
}

impl Where {
    fn is_free(self) -> bool {
        matches!(self, Where::Free | Where::Unbounded | Where::ZeroGradient)
    }
}

/// The variables that entered and left the free set at the last Cauchy point: entering ones
/// in `indices[..entering]`, leaving ones in `indices[leaving..]`.
struct Changes {
    indices: Vec<usize>,
    entering: usize,
    leaving: usize,
}

/// The Cauchy point's work vectors of length `2m`.
struct Workspace<'a> {
    p: &'a mut [f64],
    c: &'a mut [f64],
    wbp: &'a mut [f64],
    v: &'a mut [f64],
}

struct Shape {
    constrained: bool,
    boxed: bool,
}

unsafe fn evaluate(objective: &mut dyn Objective, x: &[f64], g: &mut [f64]) -> R<f64> {
    let value = objective.value(x)?;
    if !value.is_finite() {
        return Err(error("L-BFGS-B needs finite values of 'fn'"));
    }
    objective.gradient(x, g)?;
    Ok(value)
}

/// `active`: projects the start onto the box and classifies the variables.
fn initialize_active(
    x: &mut [f64],
    lower: &[f64],
    upper: &[f64],
    bounds: &[Bound],
    where_: &mut [Where],
    detail: i32,
) -> Shape {
    let mut at_bounds = 0;
    let mut projected = false;
    for i in 0..x.len() {
        if bounds[i] == Bound::None {
            continue;
        }
        if bounds[i].has_lower() && x[i] <= lower[i] {
            if x[i] < lower[i] {
                projected = true;
                x[i] = lower[i];
            }
            at_bounds += 1;
        } else if bounds[i].has_upper() && x[i] >= upper[i] {
            if x[i] > upper[i] {
                projected = true;
                x[i] = upper[i];
            }
            at_bounds += 1;
        }
    }
    let mut shape = Shape {
        constrained: false,
        boxed: true,
    };
    for i in 0..x.len() {
        if bounds[i] != Bound::Both {
            shape.boxed = false;
        }
        where_[i] = if bounds[i] == Bound::None {
            Where::Unbounded
        } else {
            shape.constrained = true;
            if bounds[i] == Bound::Both && upper[i] - lower[i] <= 0.0 {
                Where::Fixed
            } else {
                Where::Free
            }
        };
    }
    if detail >= 0 {
        if projected {
            print("The initial X is infeasible.  Restart with its projection.\n");
        }
        if !shape.constrained {
            print("This problem is unconstrained.\n");
        }
    }
    if detail > 0 {
        print(&format!(
            "At X0, {at_bounds} variables are exactly at the bounds\n"
        ));
    }
    shape
}

/// `projgr`: the infinity norm of the projected gradient.
fn projected_gradient_norm(
    x: &[f64],
    g: &[f64],
    lower: &[f64],
    upper: &[f64],
    bounds: &[Bound],
) -> f64 {
    let mut norm = 0.0;
    for i in 0..x.len() {
        let mut gi = g[i];
        if bounds[i] != Bound::None {
            if gi < 0.0 {
                if bounds[i].has_upper() {
                    gi = fmax(x[i] - upper[i], gi);
                }
            } else if bounds[i].has_lower() {
                gi = fmin(x[i] - lower[i], gi);
            }
        }
        norm = fmax(norm, gi.abs());
    }
    norm
}

/// `cauchy`: the generalized Cauchy point `xcp`, the first local minimizer of the quadratic
/// model along the projected steepest descent path, found by visiting the breakpoints in
/// order with a heap. Leaves `c = W'(xcp - x)` in the workspace. Returns the `bmv` status.
#[allow(clippy::too_many_arguments)]
fn cauchy(
    x: &[f64],
    lower: &[f64],
    upper: &[f64],
    bounds: &[Bound],
    g: &[f64],
    order: &mut [usize],
    where_: &mut [Where],
    t: &mut [f64],
    d: &mut [f64],
    xcp: &mut [f64],
    memory: &Memory,
    work: Workspace<'_>,
    segments: &mut i32,
    detail: i32,
    projected: f64,
    epsilon: f64,
) -> i32 {
    let Workspace { p, c, wbp, v } = work;
    let n = x.len();
    if projected <= 0.0 {
        if detail >= 0 {
            print("Subgnorm = 0.  GCP = X.\n");
        }
        xcp.copy_from_slice(x);
        return 0;
    }
    let col = memory.col;
    let col2 = 2 * col;
    let theta = memory.theta;
    let mut bounded = true;
    let mut free = n;
    let mut breaks = 0usize;
    let mut minimum_break = 0usize;
    let mut tl = 0.0;
    let mut tu = 0.0;
    let mut break_min = 0.0;
    let mut f1 = 0.0;
    if detail >= 99 {
        print("\n---------------- CAUCHY entered-------------------\n\n");
    }
    p[..col2].fill(0.0);
    for i in 0..n {
        let neggi = -g[i];
        if where_[i] != Where::Fixed && where_[i] != Where::Unbounded {
            if bounds[i].has_lower() {
                tl = x[i] - lower[i];
            }
            if bounds[i].has_upper() {
                tu = upper[i] - x[i];
            }
            let at_lower = bounds[i].has_lower() && tl <= 0.0;
            let at_upper = bounds[i].has_upper() && tu <= 0.0;
            where_[i] = Where::Free;
            if at_lower {
                if neggi <= 0.0 {
                    where_[i] = Where::AtLower;
                }
            } else if at_upper {
                if neggi >= 0.0 {
                    where_[i] = Where::AtUpper;
                }
            } else if neggi.abs() <= 0.0 {
                where_[i] = Where::ZeroGradient;
            }
        }
        let mut pointer = memory.head;
        if where_[i] != Where::Free && where_[i] != Where::Unbounded {
            d[i] = 0.0;
        } else {
            d[i] = neggi;
            f1 -= neggi * neggi;
            for j in 0..col {
                p[j] += memory.wy(i, pointer) * neggi;
                p[col + j] += memory.ws(i, pointer) * neggi;
                pointer = memory.next(pointer);
            }
            if bounds[i].has_lower() && neggi < 0.0 {
                order[breaks] = i;
                t[breaks] = tl / (-neggi);
                if breaks == 0 || t[breaks] < break_min {
                    break_min = t[breaks];
                    minimum_break = breaks;
                }
                breaks += 1;
            } else if bounds[i].has_upper() && neggi > 0.0 {
                order[breaks] = i;
                t[breaks] = tu / neggi;
                if breaks == 0 || t[breaks] < break_min {
                    break_min = t[breaks];
                    minimum_break = breaks;
                }
                breaks += 1;
            } else {
                free -= 1;
                order[free] = i;
                if neggi.abs() > 0.0 {
                    bounded = false;
                }
            }
        }
    }
    if theta != 1.0 {
        for value in &mut p[col..col2] {
            *value *= theta;
        }
    }
    xcp.copy_from_slice(x);
    if breaks == 0 && free == n {
        if detail > 100 {
            print_vector("Cauchy X =  ", xcp);
        }
        return 0;
    }
    c[..col2].fill(0.0);
    let mut f2 = -theta * f1;
    let f2_original = f2;
    if col > 0 {
        let info = bmv(memory, p, v);
        if info != 0 {
            return info;
        }
        f2 -= ddot(&v[..col2], &p[..col2]);
    }
    let mut dtm = -f1 / f2;
    let mut tsum = 0.0;
    *segments = 1;
    if detail >= 99 {
        print(&format!("There are {breaks}  breakpoints\n"));
    }
    let mut reached_all = false;
    if breaks > 0 {
        let mut left = breaks;
        let mut visit = 1;
        let mut tj = 0.0;
        loop {
            let tj0 = tj;
            let ibp;
            if visit == 1 {
                tj = break_min;
                ibp = order[minimum_break];
            } else {
                if visit == 2 && minimum_break != breaks - 1 {
                    t[minimum_break] = t[breaks - 1];
                    order[minimum_break] = order[breaks - 1];
                }
                heap_sort(&mut t[..left], &mut order[..left], visit - 2);
                tj = t[left - 1];
                ibp = order[left - 1];
            }
            let dt = tj - tj0;
            if dt != 0.0 && detail >= 100 {
                print(&format!(
                    "\nPiece {:6} f1, f2 at start point {} {}\nDistance to the next break point =  {}\nDistance to the stationary point =  {}\n",
                    *segments,
                    number(c"%11.4e", f1),
                    number(c"%11.4e", f2),
                    number(c"%11.4e", dt),
                    number(c"%11.4e", dtm)
                ));
            }
            if dtm < dt {
                break;
            }
            tsum += dt;
            left -= 1;
            visit += 1;
            let dibp = d[ibp];
            d[ibp] = 0.0;
            let zibp;
            if dibp > 0.0 {
                zibp = upper[ibp] - x[ibp];
                xcp[ibp] = upper[ibp];
                where_[ibp] = Where::AtUpper;
            } else {
                zibp = lower[ibp] - x[ibp];
                xcp[ibp] = lower[ibp];
                where_[ibp] = Where::AtLower;
            }
            if detail >= 100 {
                print(&format!("Variable  {}  is fixed.\n", ibp + 1));
            }
            if left == 0 && breaks == n {
                dtm = dt;
                reached_all = true;
                break;
            }
            *segments += 1;
            let dibp2 = dibp * dibp;
            // GNU R adds these updates to `f1` and `f2` as one term each.
            f1 += dt * f2 + dibp2 - theta * dibp * zibp;
            f2 -= theta * dibp2;
            if col > 0 {
                daxpy(dt, &p[..col2], &mut c[..col2]);
                let mut pointer = memory.head;
                for j in 0..col {
                    wbp[j] = memory.wy(ibp, pointer);
                    wbp[col + j] = theta * memory.ws(ibp, pointer);
                    pointer = memory.next(pointer);
                }
                let info = bmv(memory, wbp, v);
                if info != 0 {
                    return info;
                }
                let wmc = ddot(&c[..col2], &v[..col2]);
                let wmp = ddot(&p[..col2], &v[..col2]);
                let wmw = ddot(&wbp[..col2], &v[..col2]);
                daxpy(-dibp, &wbp[..col2], &mut p[..col2]);
                f1 += dibp * wmc;
                f2 += 2.0 * dibp * wmp - dibp2 * wmw;
            }
            f2 = fmax(epsilon * f2_original, f2);
            if left > 0 {
                dtm = -f1 / f2;
                continue;
            } else if bounded {
                f1 = 0.0;
                f2 = 0.0;
                dtm = 0.0;
            } else {
                dtm = -f1 / f2;
            }
            break;
        }
    }
    if !reached_all {
        if detail >= 99 {
            print(&format!(
                "\nGCP found in this segment\nPiece {:6} f1, f2 at start point {} {}\nDistance to the stationary point =  {}\n",
                *segments,
                number(c"%11.4e", f1),
                number(c"%11.4e", f2),
                number(c"%11.4e", dtm)
            ));
        }
        if dtm <= 0.0 {
            dtm = 0.0;
        }
        tsum += dtm;
        daxpy(tsum, d, xcp);
    }
    if col > 0 {
        daxpy(dtm, &p[..col2], &mut c[..col2]);
    }
    if detail >= 100 {
        print_vector("Cauchy X =  ", xcp);
    }
    if detail >= 99 {
        print("\n---------------- exit CAUCHY----------------------\n\n");
    }
    0
}

/// `hpsolb`: with `iheap == 0`, first arranges `t` into a heap (smallest on top); then moves
/// the smallest element to the end, keeping the rest a heap.
fn heap_sort(t: &mut [f64], order: &mut [usize], iheap: usize) {
    let n = t.len();
    // 1-based heap positions, as in the reference code.
    if iheap == 0 {
        for k in 2..=n {
            let value = t[k - 1];
            let entry = order[k - 1];
            let mut i = k;
            while i > 1 {
                let j = i / 2;
                if value < t[j - 1] {
                    t[i - 1] = t[j - 1];
                    order[i - 1] = order[j - 1];
                    i = j;
                } else {
                    break;
                }
            }
            t[i - 1] = value;
            order[i - 1] = entry;
        }
    }
    if n > 1 {
        let mut i = 1;
        let out = t[0];
        let out_entry = order[0];
        let value = t[n - 1];
        let entry = order[n - 1];
        loop {
            let mut j = i + i;
            if j <= n - 1 {
                if t[j] < t[j - 1] {
                    j += 1;
                }
                if t[j - 1] < value {
                    t[i - 1] = t[j - 1];
                    order[i - 1] = order[j - 1];
                    i = j;
                    continue;
                }
            }
            break;
        }
        t[i - 1] = value;
        order[i - 1] = entry;
        t[n - 1] = out;
        order[n - 1] = out_entry;
    }
}

/// `bmv`: the product `p = M v` of the middle matrix of the compact representation with a
/// vector of length `2 col`, through the Cholesky factor in `wt`.
fn bmv(memory: &Memory, v: &[f64], p: &mut [f64]) -> i32 {
    let col = memory.col;
    if col == 0 {
        return 0;
    }
    let m = memory.m;
    // 1-based, as in the reference code.
    p[col] = v[col];
    for i in 2..=col {
        let i2 = col + i;
        let mut sum = 0.0;
        for k in 1..i {
            sum += memory.sy(i, k) * v[k - 1] / memory.sy(k, k);
        }
        p[i2 - 1] = v[i2 - 1] + sum;
    }
    let info = triangular_solve(
        &memory.wt,
        0,
        m,
        col,
        &mut p[col..2 * col],
        Solve::Transposed,
    );
    if info != 0 {
        return info;
    }
    for i in 1..=col {
        p[i - 1] = v[i - 1] / memory.sy(i, i).sqrt();
    }
    let info = triangular_solve(&memory.wt, 0, m, col, &mut p[col..2 * col], Solve::Plain);
    if info != 0 {
        return info;
    }
    for i in 1..=col {
        p[i - 1] = -p[i - 1] / memory.sy(i, i).sqrt();
    }
    for i in 1..=col {
        let mut sum = 0.0;
        for k in i + 1..=col {
            sum += memory.sy(k, i) * p[col + k - 1] / memory.sy(i, i);
        }
        p[i - 1] += sum;
    }
    0
}

/// `freev`: records which variables entered and left the free set at the Cauchy point and
/// rebuilds `index` (free variables first, then active ones from the back). Returns whether
/// the subspace matrix must be rebuilt.
#[allow(clippy::too_many_arguments)]
fn free_variables(
    free: &mut usize,
    index: &mut [usize],
    changes: &mut Changes,
    where_: &[Where],
    updated: bool,
    constrained: bool,
    detail: i32,
    iteration: i32,
) -> bool {
    let n = index.len();
    changes.entering = 0;
    changes.leaving = n;
    if iteration > 0 && constrained {
        for i in 0..*free {
            let k = index[i];
            if !where_[k].is_free() {
                changes.leaving -= 1;
                changes.indices[changes.leaving] = k;
                if detail >= 100 {
                    print(&format!(
                        "Variable {} leaves the set of free variables\n",
                        k + 1
                    ));
                }
            }
        }
        for i in *free..n {
            let k = index[i];
            if where_[k].is_free() {
                changes.indices[changes.entering] = k;
                changes.entering += 1;
                if detail >= 100 {
                    print(&format!(
                        "Variable {} enters the set of free variables\n",
                        k + 1
                    ));
                }
            }
            // GNU R reports the running totals once per variable examined here, not once
            // after the loop.
            if detail >= 100 {
                print(&format!(
                    "{} variables leave; {} variables enter\n",
                    n - changes.leaving,
                    changes.entering
                ));
            }
        }
    }
    let rebuild = changes.leaving < n || changes.entering > 0 || updated;
    *free = 0;
    let mut active = n;
    for i in 0..n {
        if where_[i].is_free() {
            index[*free] = i;
            *free += 1;
        } else {
            active -= 1;
            index[active] = i;
        }
    }
    if detail >= 99 {
        print(&format!(
            "{}  variables are free at GCP on iteration {}\n",
            *free,
            iteration + 1
        ));
    }
    rebuild
}

/// `formk`: forms and factors the `2col × 2col` matrix of the subspace problem over the free
/// variables, updating the inner products kept in `wn1` for the variables that entered or
/// left the free set instead of recomputing them.
fn form_k(
    free: usize,
    index: &[usize],
    changes: &Changes,
    wn: &mut [f64],
    wn1: &mut [f64],
    memory: &Memory,
) -> i32 {
    let n = memory.n;
    let m = memory.m;
    let m2 = 2 * m;
    let col = memory.col;
    let theta = memory.theta;
    // 1-based row and column of a 2m × 2m matrix.
    let at = |row: usize, column: usize| (row - 1) + m2 * (column - 1);
    let wy = |row: usize, column: usize| memory.wy(row, column);
    let ws = |row: usize, column: usize| memory.ws(row, column);
    let upcl;
    if memory.updated {
        if memory.updates > m {
            // Shift the old part of wn1 up and left by one row and column.
            for jy in 1..m {
                let js = m + jy;
                for k in 0..m - jy {
                    wn1[at(jy + k, jy)] = wn1[at(jy + 1 + k, jy + 1)];
                }
                for k in 0..m - jy {
                    wn1[at(js + k, js)] = wn1[at(js + 1 + k, js + 1)];
                }
                for k in 0..m - 1 {
                    wn1[at(m + 1 + k, jy)] = wn1[at(m + 2 + k, jy + 1)];
                }
            }
        }
        // Put the new row of blocks (1,1), (2,1) and (2,2).
        let iy = col;
        let is = m + col;
        let ipntr = (memory.head + col - 1) % m;
        let mut jpntr = memory.head;
        for jy in 1..=col {
            let js = m + jy;
            let mut temp1 = 0.0;
            let mut temp2 = 0.0;
            let mut temp3 = 0.0;
            for &k1 in &index[..free] {
                temp1 += wy(k1, ipntr) * wy(k1, jpntr);
            }
            for &k1 in &index[free..n] {
                temp2 += ws(k1, ipntr) * ws(k1, jpntr);
                temp3 += ws(k1, ipntr) * wy(k1, jpntr);
            }
            wn1[at(iy, jy)] = temp1;
            wn1[at(is, js)] = temp2;
            wn1[at(is, jy)] = temp3;
            jpntr = memory.next(jpntr);
        }
        // Put the new column of block (2,1).
        let jy = col;
        let jpntr = (memory.head + col - 1) % m;
        let mut ipntr = memory.head;
        for i in 1..=col {
            let is = m + i;
            let mut temp3 = 0.0;
            for &k1 in &index[..free] {
                temp3 += ws(k1, ipntr) * wy(k1, jpntr);
            }
            ipntr = memory.next(ipntr);
            wn1[at(is, jy)] = temp3;
        }
        upcl = col - 1;
    } else {
        upcl = col;
    }
    let entering = &changes.indices[..changes.entering];
    let leaving = &changes.indices[changes.leaving..n];
    // Modify the old parts of blocks (1,1) and (2,2) for the changed free set.
    let mut ipntr = memory.head;
    for iy in 1..=upcl {
        let is = m + iy;
        let mut jpntr = memory.head;
        for jy in 1..=iy {
            let js = m + jy;
            let mut temp1 = 0.0;
            let mut temp2 = 0.0;
            let mut temp3 = 0.0;
            let mut temp4 = 0.0;
            for &k1 in entering {
                temp1 += wy(k1, ipntr) * wy(k1, jpntr);
                temp2 += ws(k1, ipntr) * ws(k1, jpntr);
            }
            for &k1 in leaving {
                temp3 += wy(k1, ipntr) * wy(k1, jpntr);
                temp4 += ws(k1, ipntr) * ws(k1, jpntr);
            }
            wn1[at(iy, jy)] = wn1[at(iy, jy)] + temp1 - temp3;
            wn1[at(is, js)] = wn1[at(is, js)] - temp2 + temp4;
            jpntr = memory.next(jpntr);
        }
        ipntr = memory.next(ipntr);
    }
    // Modify the old part of block (2,1).
    let mut ipntr = memory.head;
    for is in m + 1..=m + upcl {
        let mut jpntr = memory.head;
        for jy in 1..=upcl {
            let mut temp1 = 0.0;
            let mut temp3 = 0.0;
            for &k1 in entering {
                temp1 += ws(k1, ipntr) * wy(k1, jpntr);
            }
            for &k1 in leaving {
                temp3 += ws(k1, ipntr) * wy(k1, jpntr);
            }
            // Unlike the blocks above, this update adds the difference as one term.
            if is <= jy + m {
                wn1[at(is, jy)] += temp1 - temp3;
            } else {
                wn1[at(is, jy)] -= temp1 - temp3;
            }
            jpntr = memory.next(jpntr);
        }
        ipntr = memory.next(ipntr);
    }
    // Form the upper triangle of the indefinite matrix.
    for iy in 1..=col {
        let is = col + iy;
        let is1 = m + iy;
        for jy in 1..=iy {
            let js = col + jy;
            let js1 = m + jy;
            wn[at(jy, iy)] = wn1[at(iy, jy)] / theta;
            wn[at(js, is)] = wn1[at(is1, js1)] * theta;
        }
        for jy in 1..iy {
            wn[at(jy, is)] = -wn1[at(is1, jy)];
        }
        for jy in iy..=col {
            wn[at(jy, is)] = wn1[at(is1, jy)];
        }
        wn[at(iy, iy)] += memory.sy(iy, iy);
    }
    // Factor it as [L_a, 0; L_a^-1 ..., J'].
    if cholesky(wn, 0, m2, col) != 0 {
        return -1;
    }
    let col2 = 2 * col;
    let mut column = vec![0.0; col];
    for js in col + 1..=col2 {
        column.copy_from_slice(&wn[at(1, js)..at(1, js) + col]);
        triangular_solve(wn, 0, m2, col, &mut column, Solve::Transposed);
        wn[at(1, js)..at(1, js) + col].copy_from_slice(&column);
    }
    for is in col + 1..=col2 {
        for js in is..=col2 {
            let product = ddot(
                &wn[at(1, is)..at(1, is) + col],
                &wn[at(1, js)..at(1, js) + col],
            );
            wn[at(is, js)] += product;
        }
    }
    if cholesky(wn, at(col + 1, col + 1), m2, col) != 0 {
        return -2;
    }
    0
}

/// `formt`: forms `θS'S + LD⁻¹L'` in the upper triangle of `wt` and factors it.
fn form_t(memory: &mut Memory) -> i32 {
    let m = memory.m;
    let col = memory.col;
    let theta = memory.theta;
    // 1-based, as in the reference code.
    let at = |i: usize, j: usize| (i - 1) + m * (j - 1);
    for j in 1..=col {
        memory.wt[at(1, j)] = theta * memory.ss[at(1, j)];
    }
    for i in 2..=col {
        for j in i..=col {
            let k1 = i.min(j) - 1;
            let mut sum = 0.0;
            for k in 1..=k1 {
                sum += memory.sy(i, k) * memory.sy(j, k) / memory.sy(k, k);
            }
            memory.wt[at(i, j)] = sum + theta * memory.ss[at(i, j)];
        }
    }
    if cholesky(&mut memory.wt, 0, m, col) != 0 {
        return -3;
    }
    0
}

/// `cmprlb`: the reduced gradient of the quadratic model at the Cauchy point over the free
/// variables, `r = -Z'(B(xcp - x) + g)`.
#[allow(clippy::too_many_arguments)]
fn reduced_gradient(
    x: &[f64],
    g: &[f64],
    memory: &Memory,
    z: &[f64],
    r: &mut [f64],
    c: &[f64],
    v: &mut [f64],
    index: &[usize],
    free: usize,
    constrained: bool,
) -> i32 {
    let col = memory.col;
    let theta = memory.theta;
    if !constrained && col > 0 {
        for i in 0..x.len() {
            r[i] = -g[i];
        }
        return 0;
    }
    for i in 0..free {
        let k = index[i];
        r[i] = -theta * (z[k] - x[k]) - g[k];
    }
    if bmv(memory, c, v) != 0 {
        return -8;
    }
    let mut pointer = memory.head;
    for j in 0..col {
        let a1 = v[j];
        let a2 = theta * v[col + j];
        for i in 0..free {
            let k = index[i];
            // Added as one term, as GNU R does.
            r[i] += memory.wy(k, pointer) * a1 + memory.ws(k, pointer) * a2;
        }
        pointer = memory.next(pointer);
    }
    0
}

/// `subsm`: minimizes the quadratic model over the free variables from the Cauchy point
/// `x` along the reduced gradient `d`, then pulls the result back into the box.
#[allow(clippy::too_many_arguments)]
fn subspace_minimization(
    free: usize,
    index: &[usize],
    lower: &[f64],
    upper: &[f64],
    bounds: &[Bound],
    x: &mut [f64],
    d: &mut [f64],
    memory: &Memory,
    wv: &mut [f64],
    wn: &[f64],
) -> i32 {
    if free == 0 {
        return 0;
    }
    let col = memory.col;
    let theta = memory.theta;
    let mut pointer = memory.head;
    for i in 0..col {
        let mut temp1 = 0.0;
        let mut temp2 = 0.0;
        for j in 0..free {
            let k = index[j];
            temp1 += memory.wy(k, pointer) * d[j];
            temp2 += memory.ws(k, pointer) * d[j];
        }
        wv[i] = temp1;
        wv[col + i] = theta * temp2;
        pointer = memory.next(pointer);
    }
    let m2 = 2 * memory.m;
    let col2 = 2 * col;
    let info = triangular_solve(wn, 0, m2, col2, &mut wv[..col2], Solve::Transposed);
    if info != 0 {
        return info;
    }
    for value in &mut wv[..col] {
        *value = -*value;
    }
    let info = triangular_solve(wn, 0, m2, col2, &mut wv[..col2], Solve::Plain);
    if info != 0 {
        return info;
    }
    // GNU R adds each correction's two terms together before adding them to `d`, and divides
    // by θ rather than multiplying by its reciprocal; both choices show in the last digits.
    let mut pointer = memory.head;
    for jy in 0..col {
        let js = col + jy;
        for i in 0..free {
            let k = index[i];
            d[i] += memory.wy(k, pointer) * wv[jy] / theta + memory.ws(k, pointer) * wv[js];
        }
        pointer = memory.next(pointer);
    }
    for value in &mut d[..free] {
        *value /= theta;
    }

    // Backtrack from the subspace minimizer to the box along `d`. GNU R does not first try
    // the projection of the minimizer onto the box, which the reference code's version 3.0
    // added (Morales and Nocedal, 2011).
    let mut alpha = 1.0;
    let mut temp1 = alpha;
    let mut ibd = 0;
    for i in 0..free {
        let k = index[i];
        let dk = d[i];
        if bounds[k] != Bound::None {
            if dk < 0.0 && bounds[k].has_lower() {
                let temp2 = lower[k] - x[k];
                if temp2 >= 0.0 {
                    temp1 = 0.0;
                } else if dk * alpha < temp2 {
                    temp1 = temp2 / dk;
                }
            } else if dk > 0.0 && bounds[k].has_upper() {
                let temp2 = upper[k] - x[k];
                if temp2 <= 0.0 {
                    temp1 = 0.0;
                } else if dk * alpha > temp2 {
                    temp1 = temp2 / dk;
                }
            }
            if temp1 < alpha {
                alpha = temp1;
                ibd = i;
            }
        }
    }
    if alpha < 1.0 {
        let dk = d[ibd];
        let k = index[ibd];
        if dk > 0.0 {
            x[k] = upper[k];
            d[ibd] = 0.0;
        } else if dk < 0.0 {
            x[k] = lower[k];
            d[ibd] = 0.0;
        }
    }
    for i in 0..free {
        let k = index[i];
        x[k] += alpha * d[i];
    }
    0
}

/// `matupd`: appends the newest correction pair to the ring and updates `S'Y` and `S'S`.
fn update_matrices(
    memory: &mut Memory,
    d: &[f64],
    r: &[f64],
    rr: f64,
    dr: f64,
    step: f64,
    dtd: f64,
) {
    let n = memory.n;
    let m = memory.m;
    if memory.updates <= m {
        memory.col = memory.updates;
        memory.tail = (memory.head + memory.updates - 1) % m;
    } else {
        memory.tail = memory.next(memory.tail);
        memory.head = memory.next(memory.head);
    }
    let tail = memory.tail;
    memory.ws[n * tail..n * (tail + 1)].copy_from_slice(d);
    memory.wy[n * tail..n * (tail + 1)].copy_from_slice(r);
    memory.theta = rr / dr;
    let col = memory.col;
    // 1-based, as in the reference code.
    let at = |i: usize, j: usize| (i - 1) + m * (j - 1);
    if memory.updates > m {
        // Move the old information one row and column up and left.
        for j in 1..col {
            for k in 0..j {
                memory.ss[at(1 + k, j)] = memory.ss[at(2 + k, j + 1)];
            }
            for k in 0..col - j {
                memory.sy[at(j + k, j)] = memory.sy[at(j + 1 + k, j + 1)];
            }
        }
    }
    let mut pointer = memory.head;
    for j in 1..col {
        memory.sy[at(col, j)] = ddot(d, &memory.wy[n * pointer..n * (pointer + 1)]);
        memory.ss[at(j, col)] = ddot(&memory.ws[n * pointer..n * (pointer + 1)], d);
        pointer = memory.next(pointer);
    }
    memory.ss[at(col, col)] = if step == 1.0 { dtd } else { step * step * dtd };
    memory.sy[at(col, col)] = dr;
}

enum Search {
    Evaluate,
    Converged,
    Warning,
    Error,
}

/// `dcsrch`: Moré and Thuente's line search for a step satisfying the strong Wolfe
/// conditions, with `ftol = 1e-3`, `gtol = 0.9` and `xtol = 0.1`. Its state outlives a single
/// search: a search whose start is rejected continues from the previous search's state.
#[derive(Default)]
struct LineSearch {
    bracketed: bool,
    stage: i32,
    ginit: f64,
    gtest: f64,
    gx: f64,
    gy: f64,
    finit: f64,
    fx: f64,
    fy: f64,
    stx: f64,
    sty: f64,
    stmin: f64,
    stmax: f64,
    width: f64,
    width1: f64,
}

const FTOL: f64 = 1.0e-3;
const GTOL: f64 = 0.9;
const XTOL: f64 = 0.1;

impl LineSearch {
    fn step(
        &mut self,
        starting: bool,
        f: f64,
        g: f64,
        stp: &mut f64,
        stpmin: f64,
        stpmax: f64,
    ) -> Search {
        if starting {
            if *stp < stpmin || *stp > stpmax || g >= 0.0 || stpmax < stpmin {
                return Search::Error;
            }
            self.bracketed = false;
            self.stage = 1;
            self.finit = f;
            self.ginit = g;
            self.gtest = FTOL * self.ginit;
            self.width = stpmax - stpmin;
            self.width1 = self.width / 0.5;
            self.stx = 0.0;
            self.fx = self.finit;
            self.gx = self.ginit;
            self.sty = 0.0;
            self.fy = self.finit;
            self.gy = self.ginit;
            self.stmin = 0.0;
            self.stmax = *stp + 4.0 * *stp;
            return Search::Evaluate;
        }
        let ftest = self.finit + *stp * self.gtest;
        if self.stage == 1 && f <= ftest && g >= 0.0 {
            self.stage = 2;
        }
        let mut outcome = None;
        if self.bracketed && (*stp <= self.stmin || *stp >= self.stmax) {
            outcome = Some(Search::Warning);
        }
        if self.bracketed && self.stmax - self.stmin <= XTOL * self.stmax {
            outcome = Some(Search::Warning);
        }
        if *stp == stpmax && f <= ftest && g <= self.gtest {
            outcome = Some(Search::Warning);
        }
        if *stp == stpmin && (f > ftest || g >= self.gtest) {
            outcome = Some(Search::Warning);
        }
        if f <= ftest && g.abs() <= GTOL * (-self.ginit) {
            outcome = Some(Search::Converged);
        }
        if let Some(outcome) = outcome {
            return outcome;
        }
        if self.stage == 1 && f <= self.fx && f > ftest {
            let fm = f - *stp * self.gtest;
            let mut fxm = self.fx - self.stx * self.gtest;
            let mut fym = self.fy - self.sty * self.gtest;
            let gm = g - self.gtest;
            let mut gxm = self.gx - self.gtest;
            let mut gym = self.gy - self.gtest;
            step_interval(
                &mut self.stx,
                &mut fxm,
                &mut gxm,
                &mut self.sty,
                &mut fym,
                &mut gym,
                stp,
                fm,
                gm,
                &mut self.bracketed,
                self.stmin,
                self.stmax,
            );
            self.fx = fxm + self.stx * self.gtest;
            self.fy = fym + self.sty * self.gtest;
            self.gx = gxm + self.gtest;
            self.gy = gym + self.gtest;
        } else {
            step_interval(
                &mut self.stx,
                &mut self.fx,
                &mut self.gx,
                &mut self.sty,
                &mut self.fy,
                &mut self.gy,
                stp,
                f,
                g,
                &mut self.bracketed,
                self.stmin,
                self.stmax,
            );
        }
        if self.bracketed {
            if (self.sty - self.stx).abs() >= 0.66 * self.width1 {
                *stp = self.stx + 0.5 * (self.sty - self.stx);
            }
            self.width1 = self.width;
            self.width = (self.sty - self.stx).abs();
        }
        if self.bracketed {
            self.stmin = fmin(self.stx, self.sty);
            self.stmax = fmax(self.stx, self.sty);
        } else {
            self.stmin = *stp + 1.1 * (*stp - self.stx);
            self.stmax = *stp + 4.0 * (*stp - self.stx);
        }
        *stp = fmax(*stp, stpmin);
        *stp = fmin(*stp, stpmax);
        if self.bracketed && (*stp <= self.stmin || *stp >= self.stmax)
            || (self.bracketed && self.stmax - self.stmin <= XTOL * self.stmax)
        {
            *stp = self.stx;
        }
        Search::Evaluate
    }
}

/// `dcstep`: updates the interval of uncertainty `[stx, sty]` and computes the next trial
/// step from cubic and quadratic interpolants.
#[allow(clippy::too_many_arguments)]
fn step_interval(
    stx: &mut f64,
    fx: &mut f64,
    dx: &mut f64,
    sty: &mut f64,
    fy: &mut f64,
    dy: &mut f64,
    stp: &mut f64,
    fp: f64,
    dp: f64,
    bracketed: &mut bool,
    stpmin: f64,
    stpmax: f64,
) {
    let sgnd = dp * (*dx / dx.abs());
    let stpf;
    if fp > *fx {
        // A higher function value: the minimum is bracketed.
        let theta = 3.0 * (*fx - fp) / (*stp - *stx) + *dx + dp;
        let s = fmax(fmax(theta.abs(), dx.abs()), dp.abs());
        let ratio = theta / s;
        let mut gamma = s * (ratio * ratio - (*dx / s) * (dp / s)).sqrt();
        if *stp < *stx {
            gamma = -gamma;
        }
        let p = (gamma - *dx) + theta;
        let q = ((gamma - *dx) + gamma) + dp;
        let r = p / q;
        let stpc = *stx + r * (*stp - *stx);
        let stpq = *stx + ((*dx / ((*fx - fp) / (*stp - *stx) + *dx)) / 2.0) * (*stp - *stx);
        stpf = if (stpc - *stx).abs() < (stpq - *stx).abs() {
            stpc
        } else {
            stpc + (stpq - stpc) / 2.0
        };
        *bracketed = true;
    } else if sgnd < 0.0 {
        // Derivatives of opposite sign: the minimum is bracketed.
        let theta = 3.0 * (*fx - fp) / (*stp - *stx) + *dx + dp;
        let s = fmax(fmax(theta.abs(), dx.abs()), dp.abs());
        let ratio = theta / s;
        let mut gamma = s * (ratio * ratio - (*dx / s) * (dp / s)).sqrt();
        if *stp > *stx {
            gamma = -gamma;
        }
        let p = (gamma - dp) + theta;
        let q = ((gamma - dp) + gamma) + *dx;
        let r = p / q;
        let stpc = *stp + r * (*stx - *stp);
        let stpq = *stp + (dp / (dp - *dx)) * (*stx - *stp);
        stpf = if (stpc - *stp).abs() > (stpq - *stp).abs() {
            stpc
        } else {
            stpq
        };
        *bracketed = true;
    } else if dp.abs() < dx.abs() {
        // Same sign, decreasing magnitude.
        let theta = 3.0 * (*fx - fp) / (*stp - *stx) + *dx + dp;
        let s = fmax(fmax(theta.abs(), dx.abs()), dp.abs());
        let ratio = theta / s;
        let mut gamma = s * fmax(0.0, ratio * ratio - (*dx / s) * (dp / s)).sqrt();
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
            let chosen = if (stpc - *stp).abs() < (stpq - *stp).abs() {
                stpc
            } else {
                stpq
            };
            stpf = if *stp > *stx {
                fmin(*stp + 0.66 * (*sty - *stp), chosen)
            } else {
                fmax(*stp + 0.66 * (*sty - *stp), chosen)
            };
        } else {
            let chosen = if (stpc - *stp).abs() > (stpq - *stp).abs() {
                stpc
            } else {
                stpq
            };
            stpf = fmax(stpmin, fmin(stpmax, chosen));
        }
    } else if *bracketed {
        // Same sign, no decrease in magnitude.
        let theta = 3.0 * (fp - *fy) / (*sty - *stp) + *dy + dp;
        let s = fmax(fmax(theta.abs(), dy.abs()), dp.abs());
        let ratio = theta / s;
        let mut gamma = s * (ratio * ratio - (*dy / s) * (dp / s)).sqrt();
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
    if fp > *fx {
        *sty = *stp;
        *fy = fp;
        *dy = dp;
    } else {
        if sgnd < 0.0 {
            *sty = *stx;
            *fy = *fx;
            *dy = *dx;
        }
        *stx = *stp;
        *fx = fp;
        *dx = dp;
    }
    *stp = stpf;
}

/// Reference BLAS `ddot`, which accumulates from left to right.
fn ddot(x: &[f64], y: &[f64]) -> f64 {
    let mut sum = 0.0;
    for (a, b) in x.iter().zip(y) {
        sum += a * b;
    }
    sum
}

/// Reference BLAS `daxpy`, `y += a x`, which does nothing when `a` is zero.
fn daxpy(a: f64, x: &[f64], y: &mut [f64]) {
    if a == 0.0 {
        return;
    }
    for (target, value) in y.iter_mut().zip(x) {
        *target += a * value;
    }
}

/// LINPACK `dpofa`: the upper Cholesky factor of the `n × n` matrix stored column-major from
/// `offset` with leading dimension `lda`, in place. Returns the order of the first leading
/// minor that is not positive definite, or 0.
fn cholesky(a: &mut [f64], offset: usize, lda: usize, n: usize) -> i32 {
    let at = |row: usize, column: usize| offset + (row - 1) + lda * (column - 1);
    for j in 1..=n {
        let mut s = 0.0;
        for k in 1..j {
            let dot = ddot(
                &a[at(1, k)..at(1, k) + k - 1],
                &a[at(1, j)..at(1, j) + k - 1],
            );
            let t = (a[at(k, j)] - dot) / a[at(k, k)];
            a[at(k, j)] = t;
            s += t * t;
        }
        let s = a[at(j, j)] - s;
        if s <= 0.0 {
            return j as i32;
        }
        a[at(j, j)] = s.sqrt();
    }
    0
}

#[derive(Clone, Copy)]
enum Solve {
    /// LINPACK `dtrsl` job 01: `T x = b` with `T` upper triangular.
    Plain,
    /// LINPACK `dtrsl` job 11: `T' x = b` with `T` upper triangular.
    Transposed,
}

/// LINPACK `dtrsl` for an upper triangular `T` stored column-major from `offset` with
/// leading dimension `lda`. Returns the index of a zero diagonal element, or 0.
fn triangular_solve(
    t: &[f64],
    offset: usize,
    lda: usize,
    n: usize,
    b: &mut [f64],
    job: Solve,
) -> i32 {
    let at = |row: usize, column: usize| offset + (row - 1) + lda * (column - 1);
    for i in 1..=n {
        if t[at(i, i)] == 0.0 {
            return i as i32;
        }
    }
    if n == 0 {
        return 0;
    }
    match job {
        Solve::Transposed => {
            b[0] /= t[at(1, 1)];
            for j in 2..=n {
                let dot = ddot(&t[at(1, j)..at(1, j) + j - 1], &b[..j - 1]);
                b[j - 1] -= dot;
                b[j - 1] /= t[at(j, j)];
            }
        }
        Solve::Plain => {
            b[n - 1] /= t[at(n, n)];
            for j in (1..n).rev() {
                let factor = -b[j];
                daxpy(factor, &t[at(1, j + 1)..at(1, j + 1) + j], &mut b[..j]);
                b[j - 1] /= t[at(j, j)];
            }
        }
    }
    0
}

/// The reference code's `max` and `min`, which keep the first argument unless the second is
/// strictly larger (smaller).
fn fmax(a: f64, b: f64) -> f64 {
    if a >= b { a } else { b }
}

fn fmin(a: f64, b: f64) -> f64 {
    if a <= b { a } else { b }
}

fn print(text: &str) {
    crate::write_console(text.as_bytes(), false);
}

fn print_error(text: &str) {
    crate::write_console(text.as_bytes(), true);
}

fn print_vector(label: &str, values: &[f64]) {
    let mut text = label.to_owned();
    for &value in values {
        text.push_str(&number(c"%g", value));
        text.push(' ');
    }
    text.push('\n');
    print(&text);
}

/// One real formatted by C's `printf`, so the trace prints exactly as GNU R's does.
fn number(format: &CStr, value: f64) -> String {
    let mut buffer = [0u8; 64];
    // SAFETY: the format takes one double and the buffer is large enough for any of the
    // widths used here.
    let written = unsafe {
        libc::snprintf(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            format.as_ptr(),
            value,
        )
    };
    String::from_utf8_lossy(&buffer[..written.clamp(0, 63) as usize]).into_owned()
}
