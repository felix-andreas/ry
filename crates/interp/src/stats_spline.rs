//! Penalized regression splines behind `smooth.spline()` (`rbart`) and their evaluation in
//! `predict()` (`bvalus`).
//!
//! The fit is a cubic B-spline `f = sum(coef[j] B[j])` on knots scaled to [0, 1] that
//! minimizes `sum(w (y - f(x))^2) + lambda * integral(f''^2)`. Its normal equations are banded
//! with three super-diagonals, so every matrix here is stored as four diagonals.

use {
    crate::{
        builtin_libraries::NativeCall,
        stats::{c_arguments, c_return},
        stats_optimize::brent_minimize,
        unwind::{R, error},
        values,
    },
    heap::object::Sexp,
};

/// A symmetric matrix with three super-diagonals: `bands[offset][j]` is entry `(j, j + offset)`.
type Banded = [Vec<f64>; 4];

/// `.Fortran(C_rbart, penalty, dofoff, x, y, w, ssw, n, knot, nk, coef, ty, lev, crit, iparms,
/// spar, parms, scratch, ld4, ldnk, ier)`: fits the smoothing spline, choosing `spar` by the
/// criterion in `iparms` unless it is given.
pub unsafe fn rbart(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let real = |index: usize| {
        values::reals(arguments[index])
            .first()
            .copied()
            .unwrap_or(0.0)
    };
    let (penalty, dof_offset, within_ss) = (real(0), real(1), real(5));
    let n = values::as_integer_scalar(arguments[6]).max(0) as usize;
    let count = values::as_integer_scalar(arguments[8]).max(0) as usize;
    let first_n = |index: usize| {
        values::reals(arguments[index])
            .get(..n)
            .ok_or_else(|| error("'n' is larger than the data given to 'rbart'"))
    };
    let (x, y, weights) = (first_n(2)?, first_n(3)?, first_n(4)?);
    let knots = values::reals(arguments[7]).to_vec();
    let int_parameters = values::ints(arguments[13]).to_vec();
    let criterion = Criterion::from_code(int_parameters.first().copied().unwrap_or(1));
    let given = int_parameters.get(1).copied().unwrap_or(0) == 1;
    let max_iterations = int_parameters.get(2).copied().unwrap_or(500);
    let spar_is_lambda = int_parameters.get(3).copied().unwrap_or(0) == 1;
    let parameters = values::reals(arguments[15]).to_vec();
    let parameter = |index: usize| parameters.get(index).copied().unwrap_or(0.0);
    if knots.len() < count + 4 || count < 4 {
        return Err(error("invalid knots for the smoothing spline"));
    }

    let data = Data {
        x,
        y,
        weights,
        knots: &knots,
        count,
        penalty,
        dof_offset,
        within_ss,
        criterion,
    };
    let (gram, xwy) = normal_equations(&data);
    let roughness = roughness_penalty(&knots, count);
    // Scaling lambda by the ratio of the traces of the two matrices over the interior
    // coefficients makes `spar` roughly independent of the data's scale and spacing.
    let interior = 2..count.saturating_sub(3);
    let ratio =
        gram[0][interior.clone()].iter().sum::<f64>() / roughness[0][interior].iter().sum::<f64>();
    let lambda_of = |spar: f64| {
        if spar_is_lambda {
            spar
        } else {
            ratio * 256f64.powf(3.0 * spar - 1.0)
        }
    };

    let mut spar = real(14);
    let mut iterations = 0;
    if !given {
        let mut objective = |spar: f64| -> R<f64> {
            let fit = fit(&data, &gram, &xwy, &roughness, lambda_of(spar));
            Ok(if fit.factor_error == 0 && fit.criterion.is_finite() {
                fit.criterion
            } else {
                f64::MAX
            })
        };
        let (minimum, used) = brent_minimize(
            parameter(0),
            parameter(1),
            parameter(2),
            parameter(3),
            Some(max_iterations),
            &mut objective,
        )?;
        spar = minimum;
        // GNU R counts the first evaluation as an iteration too.
        iterations = used + 1;
    }
    let lambda = lambda_of(spar);
    let result = fit(&data, &gram, &xwy, &roughness, lambda);

    values::reals_mut(arguments[9])[..count].copy_from_slice(&result.coefficients);
    values::reals_mut(arguments[10])[..n].copy_from_slice(&result.fitted);
    let leverages = values::reals_mut(arguments[11]);
    if criterion != Criterion::None && leverages.len() >= n {
        leverages[..n].copy_from_slice(&result.leverages);
    }
    values::reals_mut(arguments[12])[0] = result.criterion;
    if !given && let Some(slot) = values::ints_mut(arguments[13]).get_mut(2) {
        *slot = iterations;
    }
    values::reals_mut(arguments[14])[0] = spar;
    let parameters = values::reals_mut(arguments[15]);
    if let Some(slot) = parameters.get_mut(0) {
        *slot = lambda;
    }
    if let Some(slot) = parameters.get_mut(4) {
        *slot = ratio;
    }
    let scratch = values::reals_mut(arguments[16]);
    if scratch.len() >= 13 * count {
        scratch[..count].copy_from_slice(&xwy);
        for offset in 0..4 {
            let start = (1 + offset) * count;
            scratch[start..start + count].copy_from_slice(&gram[offset]);
            let start = (5 + offset) * count;
            scratch[start..start + count].copy_from_slice(&roughness[offset]);
        }
        // The factor in LINPACK's band layout: column j holds entries (j - 3..=j, j).
        for column in 0..count {
            for offset in 0..4 {
                let value = if column >= offset {
                    result.factor[offset][column - offset]
                } else {
                    0.0
                };
                scratch[9 * count + 4 * column + 3 - offset] = value;
            }
        }
    }
    values::ints_mut(arguments[19])[0] = result.factor_error;
    c_return(call, &arguments)
}

/// `.Fortran(C_bvalus, n, knot, coef, nk, x, s, order)`: the spline, or its derivative of the
/// given order, at each `x` in [0, 1].
pub unsafe fn bvalus(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let n = values::as_integer_scalar(arguments[0]).max(0) as usize;
    let count = values::as_integer_scalar(arguments[3]).max(0) as usize;
    let knots = values::reals(arguments[1]).to_vec();
    let coefficients = values::reals(arguments[2]).to_vec();
    let order = values::as_integer_scalar(arguments[6]).clamp(0, 4) as usize;
    if knots.len() < count + 4 || coefficients.len() < count || count < 4 {
        return Err(error("invalid knots for the smoothing spline"));
    }
    let x = values::reals(arguments[4]).to_vec();
    let values_out = values::reals_mut(arguments[5]);
    for (index, &point) in x.iter().enumerate().take(n) {
        let left = interval(&knots, count, point);
        let local: [f64; 4] = std::array::from_fn(|j| coefficients[left - 3 + j]);
        values_out[index] = evaluate(&knots, left, point, local, order);
    }
    c_return(call, &arguments)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Criterion {
    None,
    GeneralizedCrossValidation,
    CrossValidation,
    DegreesOfFreedom,
}

impl Criterion {
    fn from_code(code: i32) -> Criterion {
        match code {
            0 => Criterion::None,
            2 => Criterion::CrossValidation,
            3 => Criterion::DegreesOfFreedom,
            _ => Criterion::GeneralizedCrossValidation,
        }
    }
}

struct Data<'a> {
    x: &'a [f64],
    y: &'a [f64],
    weights: &'a [f64],
    knots: &'a [f64],
    count: usize,
    penalty: f64,
    dof_offset: f64,
    /// The weighted sum of squares within groups of tied `x`, which no spline can fit.
    within_ss: f64,
    criterion: Criterion,
}

struct Fit {
    coefficients: Vec<f64>,
    fitted: Vec<f64>,
    leverages: Vec<f64>,
    criterion: f64,
    /// The Cholesky factor of the penalized normal equations, upper triangular.
    factor: Banded,
    /// LINPACK's `info`: the 1-based column where the factorization failed, or 0.
    factor_error: i32,
}

/// The fit for one `lambda`, with the leverages and the criterion it is chosen by.
fn fit(data: &Data<'_>, gram: &Banded, xwy: &[f64], roughness: &Banded, lambda: f64) -> Fit {
    let count = data.count;
    let n = data.x.len();
    let system: Banded = std::array::from_fn(|offset| {
        (0..count)
            .map(|j| gram[offset][j] + lambda * roughness[offset][j])
            .collect()
    });
    let (factor, factor_error) = cholesky(&system);
    if factor_error != 0 {
        return Fit {
            coefficients: vec![0.0; count],
            fitted: vec![0.0; n],
            leverages: vec![0.0; n],
            criterion: f64::NAN,
            factor,
            factor_error,
        };
    }
    let coefficients = solve(&factor, xwy);
    let inverse = (data.criterion != Criterion::None).then(|| band_of_inverse(&factor));
    let mut fitted = vec![0.0; n];
    let mut leverages = vec![0.0; n];
    for i in 0..n {
        let left = interval(data.knots, count, data.x[i]);
        let basis = basis_values(data.knots, left, data.x[i]);
        fitted[i] = (0..4).map(|j| coefficients[left - 3 + j] * basis[j]).sum();
        if let Some(inverse) = &inverse {
            let mut quadratic = 0.0;
            for j in 0..4 {
                for k in 0..4 {
                    let (low, high) = (left - 3 + j.min(k), left - 3 + j.max(k));
                    quadratic += basis[j] * basis[k] * inverse[high - low][low];
                }
            }
            leverages[i] = data.weights[i] * quadratic;
        }
    }
    let criterion = match data.criterion {
        Criterion::None => 0.0,
        Criterion::GeneralizedCrossValidation => {
            let residual_ss = data.within_ss
                + (0..n)
                    .map(|i| data.weights[i] * (data.y[i] - fitted[i]).powi(2))
                    .sum::<f64>();
            let total_weight: f64 = data.weights.iter().sum();
            let df: f64 = leverages.iter().sum();
            (residual_ss / total_weight)
                / (1.0 - (data.dof_offset + data.penalty * df) / total_weight).powi(2)
        }
        Criterion::CrossValidation => {
            (0..n)
                .map(|i| data.weights[i] * ((data.y[i] - fitted[i]) / (1.0 - leverages[i])).powi(2))
                .sum::<f64>()
                / n as f64
        }
        Criterion::DegreesOfFreedom => {
            let df: f64 = leverages.iter().sum();
            3.0 + (data.dof_offset - df).powi(2)
        }
    };
    Fit {
        coefficients,
        fitted,
        leverages,
        criterion,
        factor,
        factor_error: 0,
    }
}

/// `X'WX` in band form and `X'Wy`, where row `i` of `X` is the B-spline basis at `x[i]`.
fn normal_equations(data: &Data<'_>) -> (Banded, Vec<f64>) {
    let count = data.count;
    let mut gram: Banded = std::array::from_fn(|_| vec![0.0; count]);
    let mut xwy = vec![0.0; count];
    for i in 0..data.x.len() {
        let left = interval(data.knots, count, data.x[i]);
        let basis = basis_values(data.knots, left, data.x[i]);
        let weight = data.weights[i];
        for j in 0..4 {
            xwy[left - 3 + j] += weight * data.y[i] * basis[j];
            for k in j..4 {
                gram[k - j][left - 3 + j] += weight * basis[j] * basis[k];
            }
        }
    }
    (gram, xwy)
}

/// The roughness penalty `integral(B[j]'' B[k]'')` in band form. On each knot interval the
/// second derivatives are linear, so the integral of a product is closed-form; its `s^2` term
/// uses 0.333 where the exact value is 1/3, as GNU R does, so that `spar` maps to the same
/// `lambda` and selects the same fit.
fn roughness_penalty(knots: &[f64], count: usize) -> Banded {
    let mut penalty: Banded = std::array::from_fn(|_| vec![0.0; count]);
    for left in 3..count {
        let width = knots[left + 1] - knots[left];
        if width <= 0.0 {
            continue;
        }
        let start: [f64; 4] =
            std::array::from_fn(|j| evaluate(knots, left, knots[left], unit(j), 2));
        let end: [f64; 4] =
            std::array::from_fn(|j| evaluate(knots, left, knots[left + 1], unit(j), 2));
        for j in 0..4 {
            let slope_j = end[j] - start[j];
            for k in j..4 {
                let slope_k = end[k] - start[k];
                penalty[k - j][left - 3 + j] += width
                    * (start[j] * start[k]
                        + (slope_j * start[k] + slope_k * start[j]) * 0.5
                        + slope_j * slope_k * 0.333);
            }
        }
    }
    penalty
}

fn unit(index: usize) -> [f64; 4] {
    std::array::from_fn(|j| if j == index { 1.0 } else { 0.0 })
}

/// The index `left` of the knot interval `[knots[left], knots[left + 1])` holding `x`, among
/// the intervals of the spline's range; the right end belongs to the last nonempty interval.
fn interval(knots: &[f64], count: usize, x: f64) -> usize {
    let (first, last) = (3, count - 1);
    if x >= knots[count] {
        let mut left = last;
        while left > first && knots[left] >= knots[left + 1] {
            left -= 1;
        }
        return left;
    }
    let (mut low, mut high) = (first, last);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if knots[middle] <= x {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

/// The four cubic B-splines that are nonzero on interval `left`, at `x`.
fn basis_values(knots: &[f64], left: usize, x: f64) -> [f64; 4] {
    let values = lower_order_values(knots, left, x, 4);
    [values[0], values[1], values[2], values[3]]
}

/// The `derivative` of `sum(local[j] B[left - 3 + j])` at `x`, evaluated as the polynomial
/// piece of interval `left`. Each derivative turns the coefficients into differences on
/// B-splines one order lower.
fn evaluate(knots: &[f64], left: usize, x: f64, mut local: [f64; 4], derivative: usize) -> f64 {
    if derivative >= 4 {
        return 0.0;
    }
    let mut order = 4;
    for start in 1..=derivative {
        for j in (start..4).rev() {
            let index = left - 3 + j;
            let span = knots[index + order - 1] - knots[index];
            local[j] = if span > 0.0 {
                (order - 1) as f64 * (local[j] - local[j - 1]) / span
            } else {
                0.0
            };
        }
        order -= 1;
    }
    let values = lower_order_values(knots, left, x, order);
    (0..order).map(|j| local[derivative + j] * values[j]).sum()
}

/// The B-splines of `order` that are nonzero on interval `left`, at `x`, by the Cox-de Boor
/// recursion: entry `j` is `B[left - order + 1 + j]`.
fn lower_order_values(knots: &[f64], left: usize, x: f64, order: usize) -> Vec<f64> {
    let mut values = vec![1.0];
    for current in 1..order {
        let mut next = vec![0.0; current + 1];
        for (j, &value) in values.iter().enumerate() {
            let index = left + j + 1 - current;
            let span = knots[index + current] - knots[index];
            if span <= 0.0 {
                continue;
            }
            let right_share = (knots[index + current] - x) / span;
            next[j] += value * right_share;
            next[j + 1] += value * (1.0 - right_share);
        }
        values = next;
    }
    values
}

/// The upper Cholesky factor `R` with `A = R'R` of a banded positive definite matrix, and
/// the 1-based column where it stopped being positive definite (0 if it did not).
fn cholesky(matrix: &Banded) -> (Banded, i32) {
    let count = matrix[0].len();
    let mut factor: Banded = std::array::from_fn(|_| vec![0.0; count]);
    for column in 0..count {
        let first = column.saturating_sub(3);
        let mut squares = 0.0;
        for row in first..column {
            let mut value = matrix[column - row][row];
            for k in first.max(row.saturating_sub(3))..row {
                value -= factor[row - k][k] * factor[column - k][k];
            }
            value /= factor[0][row];
            factor[column - row][row] = value;
            squares += value * value;
        }
        let diagonal = matrix[0][column] - squares;
        if diagonal <= 0.0 {
            return (factor, column as i32 + 1);
        }
        factor[0][column] = diagonal.sqrt();
    }
    (factor, 0)
}

/// Solves `R'R c = b` by forward and back substitution.
fn solve(factor: &Banded, rhs: &[f64]) -> Vec<f64> {
    let count = rhs.len();
    let mut z = rhs.to_vec();
    for column in 0..count {
        for row in column.saturating_sub(3)..column {
            z[column] -= factor[column - row][row] * z[row];
        }
        z[column] /= factor[0][column];
    }
    for row in (0..count).rev() {
        for column in row + 1..count.min(row + 4) {
            z[row] -= factor[column - row][row] * z[column];
        }
        z[row] /= factor[0][row];
    }
    z
}

/// The band of `(R'R)^-1`, computed from the last row up without forming the full inverse:
/// `S R' = R^-1` gives each entry of row `i` from the entries below and to the right of it,
/// which lie inside the band.
fn band_of_inverse(factor: &Banded) -> Banded {
    let count = factor[0].len();
    let mut inverse: Banded = std::array::from_fn(|_| vec![0.0; count]);
    let entry = |inverse: &Banded, a: usize, b: usize| {
        let (low, high) = (a.min(b), a.max(b));
        if high - low > 3 {
            0.0
        } else {
            inverse[high - low][low]
        }
    };
    for i in (0..count).rev() {
        let pivot = factor[0][i];
        let neighbours = (i + 1)..count.min(i + 4);
        for j in neighbours.clone().rev() {
            let sum: f64 = neighbours
                .clone()
                .map(|k| factor[k - i][i] * entry(&inverse, k, j))
                .sum();
            inverse[j - i][i] = -sum / pivot;
        }
        let sum: f64 = neighbours
            .clone()
            .map(|k| factor[k - i][i] * inverse[k - i][i])
            .sum();
        inverse[0][i] = 1.0 / (pivot * pivot) - sum / pivot;
    }
    inverse
}
