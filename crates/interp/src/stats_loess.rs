//! `loess`: local polynomial regression after Cleveland, Grosse and Shyu. Each fit at a point is
//! a weighted least-squares polynomial over the `floor(n * span)` nearest observations, weighted
//! by the tricube of their distance. The default `"interpolate"` surface fits only at the
//! vertices of a k-d tree over the predictors and blends those fits with cubic Hermite
//! polynomials, which is what makes loess fast; `"direct"` fits at every point.
//!
//! The routines keep GNU R's `.C` and `.Fortran` interfaces, because `simpleLoess` and
//! `predLoess` in the stats package call them directly and keep the k-d tree they return in the
//! fitted object.

use crate::{
    builtin_libraries::NativeCall,
    stats::{c_arguments, c_return, integer_values, real_values},
    unwind::{R, error},
    values,
};
use heap::object::Sexp;
use rustc_hash::FxHashMap;

/// `.C(loess_raw, y, x, weights, robust, d, n, span, degree, nonparametric, drop.square,
/// sum.drop.sqr, span * cell, surf.stat, fitted.values, parameter, a, xi, vert, vval, diagonal,
/// trL, delta1, delta2, setLf)`.
pub unsafe fn loess_raw(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let d = values::as_integer_scalar(arguments[4]).max(1) as usize;
    let n = values::as_integer_scalar(arguments[5]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let weights = combined_weights(&real_values(arguments[2])?, &real_values(arguments[3])?, n);
    let model = Model::read(&arguments, 1, 6, 7, 8, 9, d, n, weights)?;
    let fraction = values::as_real_scalar(arguments[11]);
    let surface_statistics = values::first_string(arguments[12])
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let (surface, statistics) = surface_statistics
        .split_once('/')
        .unwrap_or(("interpolate", "none"));
    let capacity = values::length(arguments[15]);

    let rows = match surface {
        "direct" => {
            let mut rows = vec![0.0; n * n];
            for i in 0..n {
                let point: Vec<f64> = (0..d).map(|k| model.x[i + n * k]).collect();
                let fit = local_fit(&model, &point)?;
                for (position, &j) in fit.points.iter().enumerate() {
                    rows[i + n * j] = fit.rows[position * (d + 1)];
                }
            }
            rows
        }
        _ => {
            let tree = KdTree::build(
                &model.x,
                n,
                d,
                (n as f64 * fraction).floor() as usize,
                capacity,
            );
            let operators = vertex_operators(&model, &tree)?;
            let mut rows = vec![0.0; n * n];
            for i in 0..n {
                let point: Vec<f64> = (0..d).map(|k| model.x[i + n * k]).collect();
                for (vertex, component, coefficient) in tree.interpolation_terms(&point) {
                    let operator = &operators[(vertex * (d + 1) + component) * n..][..n];
                    for j in 0..n {
                        rows[i + n * j] += coefficient * operator[j];
                    }
                }
            }
            let vval: Vec<f64> = (0..tree.vertex_count() * (d + 1))
                .map(|row| dot(&operators[row * n..][..n], &y))
                .collect();
            write_tree(&mut arguments, &tree, n, capacity, &vval)?;
            rows
        }
    };
    let fitted: Vec<f64> = (0..n)
        .map(|i| (0..n).map(|j| rows[i + n * j] * y[j]).sum())
        .collect();
    arguments[13] = values::real_vector(&fitted)?;

    let diagonal: Vec<f64> = (0..n).map(|i| rows[i + n * i]).collect();
    let (trace, delta1, delta2) = match statistics {
        "none" => (0.0, 0.0, 0.0),
        "2.approx" => {
            let trace = approximate_trace(model.degree, d, model.span);
            let (delta1, delta2) = approximate_deltas(n, trace);
            (trace, delta1, delta2)
        }
        "exact" => {
            let (delta1, delta2) = exact_deltas(&rows, n);
            (diagonal.iter().sum(), delta1, delta2)
        }
        _ => {
            let trace: f64 = diagonal.iter().sum();
            let (delta1, delta2) = if n <= EXACT_STATISTICS_LIMIT {
                exact_deltas(&rows, n)
            } else {
                approximate_deltas(n, trace)
            };
            (trace, delta1, delta2)
        }
    };
    if statistics != "none" && statistics != "2.approx" {
        arguments[19] = values::real_vector(&diagonal)?;
    }
    arguments[20] = values::real_vector(&[trace])?;
    arguments[21] = values::real_vector(&[delta1])?;
    arguments[22] = values::real_vector(&[delta2])?;
    c_return(call, &arguments)
}

/// `.C(loess_dfit, y, x, x.evaluate, weights, span, degree, nonparametric, drop.square,
/// sum.drop.sqr, d, n, m, fit)`: direct fits at new points.
pub unsafe fn loess_dfit(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let d = values::as_integer_scalar(arguments[9]).max(1) as usize;
    let n = values::as_integer_scalar(arguments[10]).max(0) as usize;
    let m = values::as_integer_scalar(arguments[11]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let weights = combined_weights(&real_values(arguments[3])?, &[1.0], n);
    let model = Model::read(&arguments, 1, 4, 5, 6, 7, d, n, weights)?;
    let evaluate = real_values(arguments[2])?;
    let mut fit = vec![0.0; m];
    for (i, value) in fit.iter_mut().enumerate() {
        let point = row_of(&evaluate, m, d, i);
        let local = local_fit(&model, &point)?;
        *value = local
            .points
            .iter()
            .enumerate()
            .map(|(position, &j)| local.rows[position * (d + 1)] * y[j])
            .sum();
    }
    arguments[12] = values::real_vector(&fit)?;
    c_return(call, &arguments)
}

/// `.C(loess_dfitse, y, x, x.evaluate, weights, robust, family.is.gaussian, span, degree,
/// nonparametric, drop.square, sum.drop.sqr, d, n, m, fit, L)`: direct fits at new points and
/// the operator rows behind their standard errors. For a robust family the fit uses the
/// robustness weights while the operator uses the prior weights alone.
pub unsafe fn loess_dfitse(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let d = values::as_integer_scalar(arguments[11]).max(1) as usize;
    let n = values::as_integer_scalar(arguments[12]).max(0) as usize;
    let m = values::as_integer_scalar(arguments[13]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let gaussian = values::as_integer_scalar(arguments[5]) != 0;
    let fit_model = Model::read(
        &arguments,
        1,
        6,
        7,
        8,
        9,
        d,
        n,
        combined_weights(&real_values(arguments[3])?, &[1.0], n),
    )?;
    let operator_model = if gaussian {
        None
    } else {
        Some(Model::read(
            &arguments,
            1,
            6,
            7,
            8,
            9,
            d,
            n,
            combined_weights(&real_values(arguments[4])?, &[1.0], n),
        )?)
    };
    let evaluate = real_values(arguments[2])?;
    let mut fit = vec![0.0; m];
    let mut operator = vec![0.0; m * n];
    for i in 0..m {
        let point = row_of(&evaluate, m, d, i);
        let local = local_fit(&fit_model, &point)?;
        fit[i] = local
            .points
            .iter()
            .enumerate()
            .map(|(position, &j)| local.rows[position * (d + 1)] * y[j])
            .sum();
        let rows = match &operator_model {
            Some(model) => local_fit(model, &point)?,
            None => local,
        };
        for (position, &j) in rows.points.iter().enumerate() {
            operator[i + m * j] = rows.rows[position * (d + 1)];
        }
    }
    arguments[14] = values::real_vector(&fit)?;
    arguments[15] = values::real_vector(&operator)?;
    c_return(call, &arguments)
}

/// `.C(loess_ifit, parameter, a, xi, vert, vval, m, x.evaluate, fit)`: interpolates a fitted
/// k-d tree at new points.
pub unsafe fn loess_ifit(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let parameter = integer_values(arguments[0])?;
    let d = parameter.first().copied().unwrap_or(1).max(1) as usize;
    let cell_count = parameter.get(3).copied().unwrap_or(0).max(0) as usize;
    let splits = integer_values(arguments[1])?;
    let values_at = real_values(arguments[2])?;
    let bounds = real_values(arguments[3])?;
    let vval = real_values(arguments[4])?;
    let m = values::as_integer_scalar(arguments[5]).max(0) as usize;
    let evaluate = real_values(arguments[6])?;
    if splits.len() < cell_count || values_at.len() < cell_count || bounds.len() < 2 * d {
        return Err(error("invalid k-d tree in 'loess' fit"));
    }
    let tree = KdTree::rebuild(d, &bounds, &splits[..cell_count], &values_at[..cell_count]);
    if vval.len() < tree.vertex_count() * (d + 1) {
        return Err(error("invalid k-d tree in 'loess' fit"));
    }
    let fit: Vec<f64> = (0..m)
        .map(|i| {
            let point = row_of(&evaluate, m, d, i);
            tree.interpolation_terms(&point)
                .into_iter()
                .map(|(vertex, component, coefficient)| {
                    coefficient * vval[vertex * (d + 1) + component]
                })
                .sum()
        })
        .collect();
    arguments[7] = values::real_vector(&fit)?;
    c_return(call, &arguments)
}

/// `.C(loess_ise, y, x, x.evaluate, weights, span, degree, nonparametric, drop.square,
/// sum.drop.sqr, span * cell, d, n, m, fit, L)`: the interpolated surface's operator rows at new
/// points, for standard errors.
pub unsafe fn loess_ise(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let d = values::as_integer_scalar(arguments[10]).max(1) as usize;
    let n = values::as_integer_scalar(arguments[11]).max(0) as usize;
    let m = values::as_integer_scalar(arguments[12]).max(0) as usize;
    let y = real_values(arguments[0])?;
    let model = Model::read(
        &arguments,
        1,
        4,
        5,
        6,
        7,
        d,
        n,
        combined_weights(&real_values(arguments[3])?, &[1.0], n),
    )?;
    let fraction = values::as_real_scalar(arguments[9]);
    let evaluate = real_values(arguments[2])?;
    let tree = KdTree::build(
        &model.x,
        n,
        d,
        (n as f64 * fraction).floor() as usize,
        n.max(200),
    );
    let operators = vertex_operators(&model, &tree)?;
    let mut fit = vec![0.0; m];
    let mut operator = vec![0.0; m * n];
    for i in 0..m {
        let point = row_of(&evaluate, m, d, i);
        for (vertex, component, coefficient) in tree.interpolation_terms(&point) {
            let row = &operators[(vertex * (d + 1) + component) * n..][..n];
            for j in 0..n {
                operator[i + m * j] += coefficient * row[j];
            }
        }
        fit[i] = (0..n).map(|j| operator[i + m * j] * y[j]).sum();
    }
    arguments[13] = values::real_vector(&fit)?;
    arguments[14] = values::real_vector(&operator)?;
    c_return(call, &arguments)
}

/// `.Fortran(lowesw, residuals, n, robust, work)`: bisquare robustness weights with six
/// median absolute residuals as the cutoff.
pub unsafe fn lowesw(call: &NativeCall<'_>) -> R<Sexp> {
    let mut arguments = c_arguments(call)?;
    let residuals = real_values(arguments[0])?;
    let n = (values::as_integer_scalar(arguments[1]).max(0) as usize).min(residuals.len());
    let absolute: Vec<f64> = residuals[..n].iter().map(|r| r.abs()).collect();
    let cutoff = 6.0 * median(&absolute);
    let robust: Vec<f64> = absolute
        .iter()
        .map(|&r| {
            if cutoff < f64::MIN_POSITIVE || r <= 0.001 * cutoff {
                1.0
            } else if r > 0.999 * cutoff {
                0.0
            } else {
                let ratio = r / cutoff;
                (1.0 - ratio * ratio).powi(2)
            }
        })
        .collect();
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
    let scaled: Vec<f64> = (0..n)
        .map(|i| (y[i] - fitted[i]).abs() * prior[i].sqrt())
        .collect();
    let spread = 6.0 * median(&scaled);
    let scale = spread * spread / 5.0;
    let mut sum = 0.0;
    for i in 0..n {
        let residual = y[i] - fitted[i];
        sum += (1.0 - residual * residual * prior[i] / scale) * robust[i].sqrt();
    }
    let factor = n as f64 / sum;
    let pseudovalues: Vec<f64> = (0..n)
        .map(|i| fitted[i] + factor * robust[i] * (y[i] - fitted[i]))
        .collect();
    arguments[6] = values::real_vector(&pseudovalues)?;
    c_return(call, &arguments)
}

/// Fits of up to this many observations compute the residual degrees of freedom from the full
/// operator matrix, whose cost grows with the cube of the number of observations.
const EXACT_STATISTICS_LIMIT: usize = 1000;

/// The data and settings every local fit shares.
struct Model {
    /// The predictors, column-major `n` by `d`, conditionally parametric ones last.
    x: Vec<f64>,
    n: usize,
    d: usize,
    weights: Vec<f64>,
    span: f64,
    degree: usize,
    /// How many leading predictors enter the distance; the rest are conditionally parametric.
    nonparametric: usize,
    /// Per predictor, whether its square is a term of a degree-2 fit.
    squares: Vec<bool>,
}

impl Model {
    #[allow(clippy::too_many_arguments)]
    unsafe fn read(
        arguments: &[Sexp],
        x: usize,
        span: usize,
        degree: usize,
        nonparametric: usize,
        drop_square: usize,
        d: usize,
        n: usize,
        weights: Vec<f64>,
    ) -> R<Model> {
        let x = real_values(arguments[x])?;
        if x.len() < n * d {
            return Err(error("invalid 'x' in 'loess'"));
        }
        let drop_square = integer_values(arguments[drop_square])?;
        Ok(Model {
            x,
            n,
            d,
            weights,
            span: values::as_real_scalar(arguments[span]),
            degree: values::as_integer_scalar(arguments[degree]).clamp(0, 2) as usize,
            nonparametric: (values::as_integer_scalar(arguments[nonparametric]).max(1) as usize)
                .min(d),
            squares: (0..d)
                .map(|k| drop_square.get(k).is_none_or(|&code| code != 1))
                .collect(),
        })
    }
}

/// A local fit's coefficients for the value and the `d` slopes at its point, as linear
/// functionals of the responses of the observations in `points`.
struct LocalFit {
    points: Vec<usize>,
    /// `points.len()` blocks of `d + 1` coefficients: the value first, then each slope.
    rows: Vec<f64>,
}

/// The weighted least-squares polynomial at `point` over its nearest observations.
fn local_fit(model: &Model, point: &[f64]) -> R<LocalFit> {
    let (n, d) = (model.n, model.d);
    let distances: Vec<f64> = (0..n)
        .map(|i| {
            (0..model.nonparametric)
                .map(|k| {
                    let difference = model.x[i + n * k] - point[k];
                    difference * difference
                })
                .sum::<f64>()
                .sqrt()
        })
        .collect();
    let neighbours = ((n as f64 * model.span).floor() as usize).min(n);
    if neighbours == 0 {
        return Err(error("span is too small"));
    }
    let radius = if model.span <= 1.0 {
        let mut sorted = distances.clone();
        let (_, nearest, _) = sorted.select_nth_unstable_by(neighbours - 1, f64::total_cmp);
        *nearest
    } else {
        distances.iter().copied().fold(0.0, f64::max) * model.span.sqrt()
    };
    let mut points = Vec::new();
    let mut root_weights = Vec::new();
    for (i, &distance) in distances.iter().enumerate() {
        let tricube = if radius > 0.0 {
            let ratio = distance / radius;
            if ratio < 1.0 {
                (1.0 - ratio * ratio * ratio).powi(3)
            } else {
                0.0
            }
        } else if distance == 0.0 {
            1.0
        } else {
            0.0
        };
        let weight = tricube * model.weights[i];
        if weight > 0.0 {
            points.push(i);
            root_weights.push(weight.sqrt());
        }
    }

    let mut columns: Vec<Vec<f64>> = vec![root_weights.clone()];
    if model.degree >= 1 {
        for k in 0..d {
            columns.push(
                points
                    .iter()
                    .zip(&root_weights)
                    .map(|(&i, w)| w * (model.x[i + n * k] - point[k]))
                    .collect(),
            );
        }
    }
    if model.degree == 2 {
        for k in 0..d {
            if model.squares[k] {
                columns.push(
                    columns[k + 1]
                        .iter()
                        .zip(&root_weights)
                        .map(|(c, w)| c * c / w)
                        .collect(),
                );
            }
        }
        for j in 0..d {
            for k in j + 1..d {
                columns.push(
                    columns[j + 1]
                        .iter()
                        .zip(&columns[k + 1])
                        .zip(&root_weights)
                        .map(|((a, b), w)| a * b / w)
                        .collect(),
                );
            }
        }
    }
    let targets = if model.degree >= 1 { d + 1 } else { 1 };
    let functionals = least_squares_functionals(&mut columns, targets);
    let mut rows = vec![0.0; points.len() * (d + 1)];
    for (target, functional) in functionals.iter().enumerate() {
        for (position, value) in functional.iter().enumerate() {
            rows[position * (d + 1) + target] = value * root_weights[position];
        }
    }
    Ok(LocalFit { points, rows })
}

/// For each of the first `targets` columns of the least-squares problem with design `columns`,
/// the row vector that maps the (already weighted) responses to that coefficient. Uses a
/// Householder QR decomposition of the equilibrated columns with column pivoting; a coefficient
/// whose column is numerically dependent on the others is zero.
fn least_squares_functionals(columns: &mut [Vec<f64>], targets: usize) -> Vec<Vec<f64>> {
    let p = columns.len();
    let m = columns.first().map_or(0, Vec::len);
    let scales: Vec<f64> = columns
        .iter()
        .map(|column| column.iter().map(|v| v * v).sum::<f64>().sqrt())
        .collect();
    for (column, &scale) in columns.iter_mut().zip(&scales) {
        if scale > 0.0 {
            column.iter_mut().for_each(|v| *v /= scale);
        }
    }
    let mut order: Vec<usize> = (0..p).collect();
    let mut reflectors: Vec<(Vec<f64>, f64)> = Vec::new();
    let mut diagonal = Vec::new();
    let steps = p.min(m);
    for step in 0..steps {
        let best = (step..p)
            .max_by(|&a, &b| {
                let norm = |c: usize| columns[order[c]][step..].iter().map(|v| v * v).sum::<f64>();
                norm(a).total_cmp(&norm(b)).then(b.cmp(&a))
            })
            .unwrap_or(step);
        order.swap(step, best);
        let column = &columns[order[step]];
        let norm = column[step..].iter().map(|v| v * v).sum::<f64>().sqrt();
        let leading = column[step];
        let alpha = if leading >= 0.0 { -norm } else { norm };
        let mut vector: Vec<f64> = column[step..].to_vec();
        vector[0] -= alpha;
        let length_squared: f64 = vector.iter().map(|v| v * v).sum();
        diagonal.push(alpha);
        if length_squared > 0.0 {
            for &index in &order[step..] {
                let target = &mut columns[index];
                let projection: f64 = vector.iter().zip(&target[step..]).map(|(a, b)| a * b).sum();
                let factor = 2.0 * projection / length_squared;
                for (entry, v) in target[step..].iter_mut().zip(&vector) {
                    *entry -= factor * v;
                }
            }
        }
        reflectors.push((vector, length_squared));
    }
    let largest = diagonal.first().map_or(0.0, |v: &f64| v.abs());
    let rank = diagonal
        .iter()
        .take_while(|v| v.abs() > largest * 1e-10 && v.abs() > 0.0)
        .count();

    (0..targets.min(p))
        .map(|target| {
            let mut functional = vec![0.0; m];
            let Some(position) = order.iter().position(|&index| index == target) else {
                return functional;
            };
            if position >= rank || scales[target] == 0.0 {
                return functional;
            }
            // Solve R^T u = e_position over the leading rank-by-rank block, then map u back
            // through the reflectors: the coefficient is u . (Q^T y).
            let mut u = vec![0.0; rank];
            for row in 0..rank {
                let mut sum = if row == position { 1.0 } else { 0.0 };
                for (col, value) in u.iter().enumerate().take(row) {
                    sum -= columns[order[row]][col] * value;
                }
                let pivot = if row < diagonal.len() {
                    diagonal[row]
                } else {
                    1.0
                };
                u[row] = sum / pivot;
            }
            functional[..rank].copy_from_slice(&u);
            for (step, (vector, length_squared)) in reflectors.iter().enumerate().take(rank).rev() {
                if *length_squared > 0.0 {
                    let projection: f64 = vector
                        .iter()
                        .zip(&functional[step..])
                        .map(|(a, b)| a * b)
                        .sum();
                    let factor = 2.0 * projection / length_squared;
                    for (entry, v) in functional[step..].iter_mut().zip(vector) {
                        *entry -= factor * v;
                    }
                }
            }
            functional.iter_mut().for_each(|v| *v /= scales[target]);
            functional
        })
        .collect()
}

/// For every vertex of `tree` and every component (value, then each slope), the dense operator
/// row over the `n` responses, laid out vertex-major.
fn vertex_operators(model: &Model, tree: &KdTree) -> R<Vec<f64>> {
    let (n, d) = (model.n, model.d);
    let mut operators = vec![0.0; tree.vertex_count() * (d + 1) * n];
    for vertex in 0..tree.vertex_count() {
        let fit = local_fit(model, tree.vertex(vertex))?;
        for (position, &j) in fit.points.iter().enumerate() {
            for component in 0..=d {
                operators[(vertex * (d + 1) + component) * n + j] =
                    fit.rows[position * (d + 1) + component];
            }
        }
    }
    Ok(operators)
}

/// The k-d tree whose vertices carry the local fits. Cells are numbered in the order they are
/// created, both children of a cell together, so the split dimensions and values alone (the `a`
/// and `xi` vectors kept in the fit) determine every cell and vertex again.
struct KdTree {
    d: usize,
    /// Vertex coordinates, `d` per vertex.
    vertices: Vec<f64>,
    vertex_index: FxHashMap<Vec<u64>, usize>,
    cells: Vec<Cell>,
}

struct Cell {
    /// The `2^d` corner vertices; bit `k` of the corner number selects the upper bound in
    /// dimension `k`.
    corners: Vec<usize>,
    split: Option<Split>,
}

#[derive(Clone, Copy)]
struct Split {
    dimension: usize,
    value: f64,
    lower: usize,
}

impl KdTree {
    /// Splits the bounding box (widened by half a percent of each range) at the median of the
    /// widest dimension until a cell holds at most `leaf_size` points.
    fn build(x: &[f64], n: usize, d: usize, leaf_size: usize, capacity: usize) -> KdTree {
        let mut bounds = vec![0.0; 2 * d];
        for k in 0..d {
            let column = &x[n * k..n * (k + 1)];
            let low = column.iter().copied().fold(f64::INFINITY, f64::min);
            let high = column.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let margin = 0.005 * (high - low).max(1e-10 * low.abs().max(high.abs()) + 1e-30);
            bounds[k] = low - margin;
            bounds[d + k] = high + margin;
        }
        let mut tree = KdTree::root(d, &bounds);
        let mut order: Vec<usize> = (0..n).collect();
        let mut ranges = vec![(0, n)];
        let mut cell = 0;
        while cell < tree.cells.len() {
            let (start, end) = ranges[cell];
            cell += 1;
            if end - start <= leaf_size
                || tree.cells.len() + 2 > capacity
                || tree.vertex_count() + (1 << (d - 1)) > capacity
            {
                continue;
            }
            let members = &mut order[start..end];
            let dimension = (0..d)
                .map(|k| {
                    let (low, high) = members
                        .iter()
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), &i| {
                            (low.min(x[i + n * k]), high.max(x[i + n * k]))
                        });
                    high - low
                })
                .enumerate()
                .fold((0, f64::NEG_INFINITY), |best, (k, width)| {
                    if width > best.1 { (k, width) } else { best }
                })
                .0;
            members.sort_by(|&a, &b| x[a + n * dimension].total_cmp(&x[b + n * dimension]));
            let value_at = |position: usize| x[members[position] + n * dimension];
            // The median, moved outward to the nearest gap between distinct values so that
            // tied points never straddle the split.
            let count = members.len();
            let middle = (count + 1) / 2 - 1;
            let mut offset: isize = 0;
            let mut chosen = middle;
            loop {
                let position = middle as isize + offset;
                if position < 0 || position as usize + 1 >= count {
                    break;
                }
                let position = position as usize;
                if value_at(position) != value_at(position + 1) {
                    chosen = position;
                    break;
                }
                offset = -offset;
                if offset >= 0 {
                    offset += 1;
                }
            }
            let value = value_at(chosen);
            let corners = &tree.cells[cell - 1].corners;
            let low_bound = tree.vertex(corners[0])[dimension];
            let high_bound = tree.vertex(corners[corners.len() - 1])[dimension];
            if value == low_bound || value == high_bound {
                continue;
            }
            tree.split(cell - 1, dimension, value);
            ranges.push((start, start + chosen + 1));
            ranges.push((start + chosen + 1, end));
        }
        tree
    }

    /// The tree a fit kept, from its bounding box and its cells' split dimensions (1-based, 0
    /// for a leaf) and values.
    fn rebuild(d: usize, bounds: &[f64], dimensions: &[i32], split_values: &[f64]) -> KdTree {
        let mut tree = KdTree::root(d, bounds);
        let mut cell = 0;
        while cell < tree.cells.len() && cell < dimensions.len() {
            let dimension = dimensions[cell];
            if dimension >= 1 && (dimension as usize) <= d {
                tree.split(cell, dimension as usize - 1, split_values[cell]);
            }
            cell += 1;
        }
        tree
    }

    fn root(d: usize, bounds: &[f64]) -> KdTree {
        let mut tree = KdTree {
            d,
            vertices: Vec::new(),
            vertex_index: FxHashMap::default(),
            cells: Vec::new(),
        };
        let corners = (0..1usize << d)
            .map(|corner| {
                let coordinates: Vec<f64> = (0..d)
                    .map(|k| {
                        if corner & (1 << k) != 0 {
                            bounds[d + k]
                        } else {
                            bounds[k]
                        }
                    })
                    .collect();
                tree.vertex_at(&coordinates)
            })
            .collect();
        tree.cells.push(Cell {
            corners,
            split: None,
        });
        tree
    }

    fn split(&mut self, cell: usize, dimension: usize, value: f64) {
        let corners = self.cells[cell].corners.clone();
        let mut lower = corners.clone();
        let mut upper = corners.clone();
        for (corner, &vertex) in corners.iter().enumerate() {
            if corner & (1 << dimension) != 0 {
                continue;
            }
            let mut coordinates = self.vertex(vertex).to_vec();
            coordinates[dimension] = value;
            let middle = self.vertex_at(&coordinates);
            lower[corner | (1 << dimension)] = middle;
            upper[corner] = middle;
        }
        let first = self.cells.len();
        self.cells.push(Cell {
            corners: lower,
            split: None,
        });
        self.cells.push(Cell {
            corners: upper,
            split: None,
        });
        self.cells[cell].split = Some(Split {
            dimension,
            value,
            lower: first,
        });
    }

    fn vertex_at(&mut self, coordinates: &[f64]) -> usize {
        let key: Vec<u64> = coordinates.iter().map(|v| v.to_bits()).collect();
        if let Some(&index) = self.vertex_index.get(&key) {
            return index;
        }
        let index = self.vertex_count();
        self.vertices.extend_from_slice(coordinates);
        self.vertex_index.insert(key, index);
        index
    }

    fn vertex_count(&self) -> usize {
        self.vertices.len() / self.d
    }

    fn vertex(&self, index: usize) -> &[f64] {
        &self.vertices[index * self.d..(index + 1) * self.d]
    }

    /// The surface at `point` as a combination of vertex values (component 0) and slopes
    /// (component `k + 1`). One predictor interpolates by cubic Hermite over the leaf cell. Two
    /// predictors blend the four cell edges as a Coons patch: each edge is a Hermite curve
    /// through every vertex on it (a neighbour's split leaves extra vertices there), the slope
    /// across an edge blends between its two vertices, and the corner tensor product is
    /// subtracted. More predictors use the corner tensor product alone.
    fn interpolation_terms(&self, point: &[f64]) -> Vec<(usize, usize, f64)> {
        let d = self.d;
        let mut cell = 0;
        while let Some(split) = self.cells[cell].split {
            cell = if point[split.dimension] <= split.value {
                split.lower
            } else {
                split.lower + 1
            };
        }
        let corners = &self.cells[cell].corners;
        let low = self.vertex(corners[0]).to_vec();
        let high = self.vertex(corners[corners.len() - 1]).to_vec();
        let widths: Vec<f64> = (0..d).map(|k| high[k] - low[k]).collect();
        let positions: Vec<f64> = (0..d)
            .map(|k| fraction(point[k], low[k], widths[k]))
            .collect();
        let mut terms = Vec::with_capacity(corners.len() * (d + 1) * 3);
        let tensor_sign = if d == 2 { -1.0 } else { 1.0 };
        for (corner, &vertex) in corners.iter().enumerate() {
            let upper = |k: usize| corner & (1 << k) != 0;
            let factors: Vec<f64> = (0..d)
                .map(|k| value_basis(positions[k], upper(k)))
                .collect();
            terms.push((vertex, 0, tensor_sign * factors.iter().product::<f64>()));
            for k in 0..d {
                let others: f64 = (0..d).filter(|&j| j != k).map(|j| factors[j]).product();
                let slope = others * slope_basis(positions[k], upper(k)) * widths[k];
                terms.push((vertex, k + 1, tensor_sign * slope));
            }
        }
        if d == 2 {
            for across in 0..2 {
                let along = 1 - across;
                for upper in [false, true] {
                    let bound = if upper { high[across] } else { low[across] };
                    let value_weight = value_basis(positions[across], upper);
                    let slope_weight = slope_basis(positions[across], upper) * widths[across];
                    let (first, second) = self.edge_vertices(
                        along,
                        across,
                        bound,
                        low[along],
                        high[along],
                        point[along],
                    );
                    let first_at = self.vertex(first)[along];
                    let width = self.vertex(second)[along] - first_at;
                    let t = fraction(point[along], first_at, width);
                    for (vertex, end) in [(first, false), (second, true)] {
                        terms.push((vertex, 0, value_weight * value_basis(t, end)));
                        terms.push((
                            vertex,
                            along + 1,
                            value_weight * slope_basis(t, end) * width,
                        ));
                        terms.push((vertex, across + 1, slope_weight * value_basis(t, end)));
                    }
                }
            }
        }
        terms
    }

    /// The two vertices on the cell edge at `bound` in dimension `across`, spanning
    /// `low..=high` in dimension `along`, that bracket `coordinate`.
    fn edge_vertices(
        &self,
        along: usize,
        across: usize,
        bound: f64,
        low: f64,
        high: f64,
        coordinate: f64,
    ) -> (usize, usize) {
        let mut below: Option<(f64, usize)> = None;
        let mut above: Option<(f64, usize)> = None;
        let mut below_strict: Option<(f64, usize)> = None;
        for vertex in 0..self.vertex_count() {
            let coordinates = self.vertex(vertex);
            let at = coordinates[along];
            if coordinates[across] != bound || at < low || at > high {
                continue;
            }
            if at <= coordinate && below.is_none_or(|(best, _)| at > best) {
                below = Some((at, vertex));
            }
            if at < coordinate && below_strict.is_none_or(|(best, _)| at > best) {
                below_strict = Some((at, vertex));
            }
            if at > coordinate && above.is_none_or(|(best, _)| at < best) {
                above = Some((at, vertex));
            }
        }
        match (below, above, below_strict) {
            (Some((_, first)), Some((_, second)), _) => (first, second),
            (Some((_, second)), None, Some((_, first))) => (first, second),
            (Some((_, only)), None, None) | (None, Some((_, only)), _) => (only, only),
            (None, None, _) => (0, 0),
        }
    }
}

/// Where `value` lies between `low` and `low + width`, as a fraction.
fn fraction(value: f64, low: f64, width: f64) -> f64 {
    if width > 0.0 {
        (value - low) / width
    } else {
        0.0
    }
}

/// The cubic Hermite weight of the value at the lower (or `upper`) end at position `t`.
fn value_basis(t: f64, upper: bool) -> f64 {
    if upper {
        t * t * (3.0 - 2.0 * t)
    } else {
        (1.0 - t) * (1.0 - t) * (1.0 + 2.0 * t)
    }
}

/// The cubic Hermite weight of the slope at the lower (or `upper`) end, per unit width.
fn slope_basis(t: f64, upper: bool) -> f64 {
    if upper {
        t * t * (t - 1.0)
    } else {
        t * (1.0 - t) * (1.0 - t)
    }
}

/// Stores the tree in the `parameter`, `a`, `xi`, `vert` and `vval` arguments of `loess_raw`.
unsafe fn write_tree(
    arguments: &mut [Sexp],
    tree: &KdTree,
    n: usize,
    capacity: usize,
    vval: &[f64],
) -> R<()> {
    let d = tree.d;
    let corners = 1usize << d;
    let limit = capacity.max(1) as i32;
    let parameter = [
        d as i32,
        n as i32,
        corners as i32,
        tree.cells.len() as i32,
        tree.vertex_count() as i32,
        49 + (corners as i32 + 3) * limit,
        49 + 2 * (d as i32 + 1) * limit,
    ];
    arguments[14] = values::integer_vector(&parameter)?;
    let mut dimensions = vec![0; capacity];
    let mut split_values = vec![0.0; capacity];
    for (index, cell) in tree.cells.iter().enumerate().take(capacity) {
        if let Some(split) = cell.split {
            dimensions[index] = split.dimension as i32 + 1;
            split_values[index] = split.value;
        }
    }
    arguments[15] = values::integer_vector(&dimensions)?;
    arguments[16] = values::real_vector(&split_values)?;
    let low = tree.vertex(0);
    let high = tree.vertex(corners - 1);
    let bounds: Vec<f64> = low.iter().chain(high).copied().collect();
    arguments[17] = values::real_vector(&bounds)?;
    let mut stored = vec![0.0; (d + 1) * capacity];
    let count = vval.len().min(stored.len());
    stored[..count].copy_from_slice(&vval[..count]);
    arguments[18] = values::real_vector(&stored)?;
    Ok(())
}

/// The residual degrees of freedom `tr((I - L)^T (I - L))` and `tr(((I - L)^T (I - L))^2)`
/// from the operator `rows` (column-major `n` by `n`).
fn exact_deltas(rows: &[f64], n: usize) -> (f64, f64) {
    let residual = |i: usize, j: usize| (if i == j { 1.0 } else { 0.0 }) - rows[i + n * j];
    let mut delta1 = 0.0;
    for j in 0..n {
        for i in 0..n {
            delta1 += residual(i, j).powi(2);
        }
    }
    let mut columns = vec![0.0; n * n];
    for j in 0..n {
        for i in 0..n {
            columns[i + n * j] = residual(i, j);
        }
    }
    let mut delta2 = 0.0;
    for i in 0..n {
        let left = &columns[n * i..n * (i + 1)];
        for j in i..n {
            let product = dot(left, &columns[n * j..n * (j + 1)]);
            delta2 += if i == j {
                product * product
            } else {
                2.0 * product * product
            };
        }
    }
    (delta1, delta2)
}

/// A rough approximation of the residual degrees of freedom for fits too large for the full
/// operator: each falls short of `n` by a little more than the trace of the operator.
fn approximate_deltas(n: usize, trace: f64) -> (f64, f64) {
    (n as f64 - 1.15 * trace, n as f64 - 1.2 * trace)
}

/// The trace of the operator approximated from the number of local parameters and the span.
fn approximate_trace(degree: usize, d: usize, span: f64) -> f64 {
    let d = d as f64;
    let parameters = match degree {
        0 => 1.0,
        1 => d + 1.0,
        _ => (d + 2.0) * (d + 1.0) / 2.0,
    };
    let knee = (-0.08125 * d + 0.13) * d + 1.05;
    parameters * (1.0 + ((knee - span) / span).max(0.0))
}

fn combined_weights(weights: &[f64], robust: &[f64], n: usize) -> Vec<f64> {
    let at = |values: &[f64], i: usize| {
        if values.is_empty() {
            1.0
        } else {
            values[i % values.len()]
        }
    };
    (0..n).map(|i| at(weights, i) * at(robust, i)).collect()
}

fn row_of(matrix: &[f64], rows: usize, columns: usize, row: usize) -> Vec<f64> {
    (0..columns)
        .map(|k| matrix.get(row + rows * k).copied().unwrap_or(f64::NAN))
        .collect()
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

/// The median, averaging the two middle values of an even count.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let half = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[half - 1] + sorted[half]) / 2.0
    } else {
        sorted[half]
    }
}
