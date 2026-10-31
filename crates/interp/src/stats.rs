//! The stats package's native routines, other than the distribution functions: least squares,
//! the GLM link helpers, correlation and distance, and the numerical building blocks behind
//! `fft`, `filter`, `approx`, `spline`, `lowess` and friends. Model formulas are in
//! `stats_models`, the optimizers and integrators in `stats_optimize`.
//!
//! The algorithms follow their published descriptions (LINPACK for the QR decomposition, and
//! the papers each routine cites), in the operation order GNU R uses, so that results agree with
//! GNU R to the last printed digit.

use crate::{
    builtin_libraries::{NativeCall, NativeFn, c_result},
    coerce::coerce_vector,
    stats_models,
    unwind::R,
    values::{self, length, symbols},
};
use heap::object::{NA_REAL, Sexp, SexpType};

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("termsform", stats_models::termsform),
    ("updateform", stats_models::updateform),
    ("modelframe", stats_models::modelframe),
    ("modelmatrix", stats_models::modelmatrix),
    ("Cdqrls", cdqrls),
    ("influence", stats_models::influence),
    ("logit_link", logit_link),
    ("logit_linkinv", logit_linkinv),
    ("logit_mu_eta", logit_mu_eta),
    ("binomial_dev_resids", binomial_dev_resids),
    ("cov", cov),
    ("cor", cor),
];

/// The arguments of a `.C` or `.Fortran` call, coerced as the routine's declared types, ready
/// to be modified and returned with [`c_result`].
pub(crate) unsafe fn c_arguments(call: &NativeCall<'_>) -> R<Vec<Sexp>> {
    let mut arguments = Vec::with_capacity(call.args.len());
    for arg in call.args {
        arguments.push(values::duplicate(arg.value)?);
    }
    Ok(arguments)
}

pub(crate) unsafe fn c_return(call: &NativeCall<'_>, arguments: &[Sexp]) -> R<Sexp> {
    c_result(call, arguments)
}

/// `x` as a double vector (a fresh one if it had to be converted).
pub(crate) unsafe fn as_reals(x: Sexp) -> R<Sexp> {
    if x.kind() == SexpType::Real {
        values::materialize(x)
    } else {
        coerce_vector(x, SexpType::Real)
    }
}

/// The elements of `x` as doubles.
pub(crate) unsafe fn real_values(x: Sexp) -> R<Vec<f64>> {
    let reals = as_reals(x)?;
    Ok(values::reals(reals).to_vec())
}

/// The elements of `x` as integers.
pub(crate) unsafe fn integer_values(x: Sexp) -> R<Vec<i32>> {
    let integers = if x.kind() == SexpType::Integer { values::materialize(x)? } else { coerce_vector(x, SexpType::Integer)? };
    Ok(values::ints(integers).to_vec())
}

/// A double matrix with `rows` rows, column-major.
pub(crate) unsafe fn real_matrix(data: &[f64], rows: usize, columns: usize) -> R<Sexp> {
    let matrix = values::real_vector(data)?;
    let dim = values::integer_vector(&[rows as i32, columns as i32])?;
    values::set_attr(matrix, symbols().dim, dim)?;
    Ok(matrix)
}

/// The number of rows and columns of `x`: its `dim`, or a single column.
pub(crate) unsafe fn matrix_shape(x: Sexp) -> (usize, usize) {
    match values::dims(x) {
        Some(dims) if dims.len() == 2 => (dims[0], dims[1]),
        _ => (length(x), 1),
    }
}

// Least squares.

/// `Cdqrls(x, y, tol, check)`: the least squares fit behind `lm.fit` and `glm.fit`, through a
/// Householder QR decomposition with limited column pivoting.
unsafe fn cdqrls(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let y = call.arg(1);
    let tolerance = values::as_real_scalar(call.arg(2));
    let check = values::as_logical_scalar(call.arg(3)) == 1;
    let (n, p) = matrix_shape(x);
    let y_dims = values::dims(y);
    let (y_rows, ny) = match &y_dims {
        Some(dims) if dims.len() == 2 => (dims[0], dims[1]),
        _ => (length(y), 1),
    };
    if check && y_rows != n {
        return Err(call.error(format!(
            "dimensions of 'x' ({n},{p}) and 'y' ({y_rows}) do not match"
        )));
    }
    let qr = as_reals(x)?;
    let qr = if qr == x { values::duplicate(x)? } else { qr };
    let effects = values::duplicate(as_reals(y)?)?;
    let residuals = values::duplicate(effects)?;
    for (value, name) in [(qr, "x"), (effects, "y")] {
        if values::reals(value).iter().any(|entry| !entry.is_finite()) {
            return Err(call.error(format!("NA/NaN/Inf in '{name}'")));
        }
    }
    let coefficients = match &y_dims {
        Some(dims) if dims.len() == 2 => {
            let matrix = values::alloc(SexpType::Real, p * ny)?;
            let dim = values::integer_vector(&[p as i32, ny as i32])?;
            values::set_attr(matrix, symbols().dim, dim)?;
            matrix
        }
        _ => values::alloc(SexpType::Real, p)?,
    };
    values::reals_mut(coefficients).fill(0.0);
    let mut qraux = vec![0.0; p];
    let mut pivot: Vec<i32> = (1..=p as i32).collect();
    let rank = {
        let matrix = values::reals_mut(qr);
        let rank = dqrdc2(matrix, n, n, p, tolerance, &mut qraux, &mut pivot);
        let y_data = values::reals(effects).to_vec();
        let qty = values::reals_mut(effects);
        let rsd = values::reals_mut(residuals);
        let b = values::reals_mut(coefficients);
        if rank > 0 {
            for column in 0..ny {
                let range = column * n..(column + 1) * n;
                let mut coefficient = vec![0.0; rank];
                dqrsl(
                    matrix,
                    n,
                    n,
                    rank,
                    &qraux,
                    &y_data[range.clone()],
                    QrOutputs { qy: None, qty: Some(&mut qty[range.clone()]), b: Some(&mut coefficient), rsd: Some(&mut rsd[range.clone()]), xb: None },
                );
                b[column * p..column * p + rank].copy_from_slice(&coefficient);
            }
        } else {
            rsd.copy_from_slice(&y_data);
        }
        rank
    };
    let pivoted = pivot.iter().enumerate().any(|(index, &column)| column != index as i32 + 1);
    values::named_list(&[
        ("qr", qr),
        ("coefficients", coefficients),
        ("residuals", residuals),
        ("effects", effects),
        ("rank", values::scalar_integer(rank as i32)?),
        ("pivot", values::integer_vector(&pivot)?),
        ("qraux", values::real_vector(&qraux)?),
        ("tol", values::scalar_real(tolerance)?),
        ("pivoted", values::scalar_bool(pivoted)?),
    ])
}

/// The reference BLAS `dnrm2` (Anderson's algorithm, used since LAPACK 3.10): the Euclidean
/// norm, with entries too small or too large to square safely accumulated separately and
/// scaled. Ordinary entries are squared and summed directly, which is what fixes the last bits
/// of GNU R's QR results.
pub(crate) fn dnrm2(values: &[f64]) -> f64 {
    let small_threshold = 2f64.powi(-511);
    let big_threshold = 2f64.powi(486);
    let small_scale = 2f64.powi(537);
    let big_scale = 2f64.powi(-538);
    let (mut small, mut medium, mut big) = (0.0_f64, 0.0_f64, 0.0_f64);
    let mut not_big = true;
    for &value in values {
        let absolute = value.abs();
        if absolute > big_threshold {
            big += (absolute * big_scale) * (absolute * big_scale);
            not_big = false;
        } else if absolute < small_threshold {
            if not_big {
                small += (absolute * small_scale) * (absolute * small_scale);
            }
        } else {
            medium += absolute * absolute;
        }
    }
    let (scale, sum) = if big > 0.0 {
        if medium > 0.0 || medium.is_nan() {
            big += (medium * big_scale) * big_scale;
        }
        (1.0 / big_scale, big)
    } else if small > 0.0 {
        if medium > 0.0 || medium.is_nan() {
            let medium = medium.sqrt();
            let small = small.sqrt() / small_scale;
            let (low, high) = if small > medium { (medium, small) } else { (small, medium) };
            (1.0, high * high * (1.0 + (low / high) * (low / high)))
        } else {
            (1.0 / small_scale, small)
        }
    } else {
        (1.0, medium)
    };
    scale * sum.sqrt()
}

fn ddot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

/// The Householder QR decomposition of the `n` by `p` matrix `x` (leading dimension `ldx`), in
/// LINPACK's compact form, with GNU R's limited pivoting: a column whose norm has fallen below
/// `tolerance` times its original norm is moved to the end, so the first `rank` columns are the
/// linearly independent ones. Returns the rank.
pub(crate) fn dqrdc2(
    x: &mut [f64],
    ldx: usize,
    n: usize,
    p: usize,
    tolerance: f64,
    qraux: &mut [f64],
    pivot: &mut [i32],
) -> usize {
    let at = |row: usize, column: usize| row + column * ldx;
    let mut norms = vec![0.0; p];
    let mut original = vec![0.0; p];
    for column in 0..p {
        let norm = dnrm2(&x[at(0, column)..at(n, column)]);
        qraux[column] = norm;
        norms[column] = norm;
        original[column] = if norm == 0.0 { 1.0 } else { norm };
    }
    let steps = n.min(p);
    // `limit` is one past the last column still considered independent.
    let mut limit = p + 1;
    for l in 0..steps {
        while l + 1 < limit && qraux[l] < original[l] * tolerance {
            for row in 0..n {
                let moved = x[at(row, l)];
                for column in l + 1..p {
                    x[at(row, column - 1)] = x[at(row, column)];
                }
                x[at(row, p - 1)] = moved;
            }
            pivot[l..p].rotate_left(1);
            qraux[l..p].rotate_left(1);
            norms[l..p].rotate_left(1);
            original[l..p].rotate_left(1);
            limit -= 1;
        }
        if l + 1 == n {
            break;
        }
        let mut norm = dnrm2(&x[at(l, l)..at(n, l)]);
        if norm == 0.0 {
            continue;
        }
        if x[at(l, l)] != 0.0 {
            norm = norm.abs().copysign(x[at(l, l)]);
        }
        for row in l..n {
            x[at(row, l)] *= 1.0 / norm;
        }
        x[at(l, l)] += 1.0;
        for column in l + 1..p {
            let t = -ddot(&x[at(l, l)..at(n, l)], &x[at(l, column)..at(n, column)]) / x[at(l, l)];
            for row in l..n {
                x[at(row, column)] += t * x[at(row, l)];
            }
            if qraux[column] != 0.0 {
                let ratio = x[at(l, column)].abs() / qraux[column];
                let remaining = (1.0 - ratio * ratio).max(0.0);
                if remaining.abs() < 1e-6 {
                    qraux[column] = dnrm2(&x[at(l + 1, column)..at(n, column)]);
                    norms[column] = qraux[column];
                } else {
                    qraux[column] *= remaining.sqrt();
                }
            }
        }
        qraux[l] = x[at(l, l)];
        x[at(l, l)] = -norm;
    }
    (limit - 1).min(n)
}

/// The outputs `dqrsl` can compute; each is filled when present.
pub(crate) struct QrOutputs<'a> {
    pub qy: Option<&'a mut [f64]>,
    pub qty: Option<&'a mut [f64]>,
    pub b: Option<&'a mut [f64]>,
    pub rsd: Option<&'a mut [f64]>,
    pub xb: Option<&'a mut [f64]>,
}

/// LINPACK's `dqrsl`: applies the QR decomposition from [`dqrdc2`] (its first `k` columns) to
/// `y`, computing Qy, Q'y, the coefficients, the residuals and the fitted values. Returns the
/// index (1-based) of a zero diagonal element of R that stopped the back substitution, or 0.
pub(crate) fn dqrsl(x: &mut [f64], ldx: usize, n: usize, k: usize, qraux: &[f64], y: &[f64], outputs: QrOutputs<'_>) -> usize {
    let at = |row: usize, column: usize| row + column * ldx;
    let QrOutputs { qy, qty, b, rsd, xb } = outputs;
    let mut info = 0;
    let steps = k.min(n.saturating_sub(1));
    let reflect = |x: &mut [f64], j: usize, target: &mut [f64]| {
        if qraux[j] != 0.0 {
            let diagonal = x[at(j, j)];
            x[at(j, j)] = qraux[j];
            let t = -ddot(&x[at(j, j)..at(n, j)], &target[j..n]) / x[at(j, j)];
            for row in j..n {
                target[row] += t * x[at(row, j)];
            }
            x[at(j, j)] = diagonal;
        }
    };
    if steps == 0 {
        if let Some(qy) = qy {
            qy[0] = y[0];
        }
        if let Some(qty) = qty {
            qty[0] = y[0];
        }
        if let Some(xb) = xb {
            xb[0] = y[0];
        }
        if let Some(b) = b {
            if x[0] == 0.0 {
                info = 1;
            } else {
                b[0] = y[0] / x[0];
            }
        }
        if let Some(rsd) = rsd {
            rsd[0] = 0.0;
        }
        return info;
    }
    if let Some(qy) = qy {
        qy[..n].copy_from_slice(&y[..n]);
        for j in (0..steps).rev() {
            reflect(x, j, qy);
        }
    }
    let mut transformed = y[..n].to_vec();
    for j in 0..steps {
        reflect(x, j, &mut transformed);
    }
    if let Some(qty) = qty {
        qty[..n].copy_from_slice(&transformed);
    }
    let mut rsd = rsd;
    let mut xb = xb;
    if let Some(rsd) = rsd.as_deref_mut() {
        rsd[..k].fill(0.0);
        rsd[k..n].copy_from_slice(&transformed[k..n]);
    }
    if let Some(xb) = xb.as_deref_mut() {
        xb[..k].copy_from_slice(&transformed[..k]);
        xb[k..n].fill(0.0);
    }
    if let Some(b) = b {
        b[..k].copy_from_slice(&transformed[..k]);
        for j in (0..k).rev() {
            if x[at(j, j)] == 0.0 {
                info = j + 1;
                break;
            }
            b[j] /= x[at(j, j)];
            if j != 0 {
                let t = -b[j];
                for row in 0..j {
                    b[row] += t * x[at(row, j)];
                }
            }
        }
    }
    if rsd.is_some() || xb.is_some() {
        for j in (0..steps).rev() {
            if let Some(rsd) = rsd.as_deref_mut() {
                reflect(x, j, rsd);
            }
            if let Some(xb) = xb.as_deref_mut() {
                reflect(x, j, xb);
            }
        }
    }
    info
}

// The binomial family's link helpers.

const THRESHOLD: f64 = 30.0;
const MINUS_THRESHOLD: f64 = -30.0;

unsafe fn numeric_argument(call: &NativeCall<'_>, x: Sexp, name: &str) -> R<()> {
    if x.kind() != SexpType::Real || length(x) == 0 {
        return Err(call.error(format!("Argument {name} must be a nonempty numeric vector")));
    }
    Ok(())
}

/// A fresh copy of the double vector `x`, with its attributes, whose elements are `map` of the
/// originals.
unsafe fn map_reals(x: Sexp, map: impl Fn(f64) -> R<f64>) -> R<Sexp> {
    let result = values::shallow_duplicate(x)?;
    let input = values::reals(x).to_vec();
    let output = values::reals_mut(result);
    for (slot, value) in output.iter_mut().zip(input) {
        *slot = map(value)?;
    }
    Ok(result)
}

unsafe fn logit_link(call: &NativeCall<'_>) -> R<Sexp> {
    let mu = call.arg(0);
    numeric_argument(call, mu, "mu")?;
    map_reals(mu, |value| {
        if !(0.0..=1.0).contains(&value) {
            return Err(call.error(format!("Value {} out of range (0, 1)", crate::format::c_general(value, 6, false))));
        }
        Ok((value / (1.0 - value)).ln())
    })
}

unsafe fn logit_linkinv(call: &NativeCall<'_>) -> R<Sexp> {
    let eta = call.arg(0);
    numeric_argument(call, eta, "eta")?;
    map_reals(eta, |value| {
        let odds = if value < MINUS_THRESHOLD {
            f64::EPSILON
        } else if value > THRESHOLD {
            1.0 / f64::EPSILON
        } else {
            value.exp()
        };
        Ok(odds / (1.0 + odds))
    })
}

unsafe fn logit_mu_eta(call: &NativeCall<'_>) -> R<Sexp> {
    let eta = call.arg(0);
    numeric_argument(call, eta, "eta")?;
    map_reals(eta, |value| {
        let one_plus = 1.0 + value.exp();
        Ok(if !(MINUS_THRESHOLD..=THRESHOLD).contains(&value) {
            f64::EPSILON
        } else {
            value.exp() / (one_plus * one_plus)
        })
    })
}

unsafe fn binomial_dev_resids(call: &NativeCall<'_>) -> R<Sexp> {
    let y = as_reals(call.arg(0))?;
    let mu = as_reals(call.arg(1))?;
    let weights = as_reals(call.arg(2))?;
    let n = length(y);
    for (value, name) in [(mu, "mu"), (weights, "wt")] {
        let size = length(value);
        if size != n && size != 1 {
            return Err(call.error(format!("argument {name} must be a numeric vector of length 1 or length {n}")));
        }
    }
    let mu = values::reals(mu).to_vec();
    let weights = values::reals(weights).to_vec();
    let y_log_y = |y: f64, mu: f64| if y != 0.0 { y * (y / mu).ln() } else { 0.0 };
    let result = values::shallow_duplicate(y)?;
    let input = values::reals(y).to_vec();
    let output = values::reals_mut(result);
    for (index, &value) in input.iter().enumerate() {
        let mean = mu[if mu.len() > 1 { index } else { 0 }];
        let weight = weights[if weights.len() > 1 { index } else { 0 }];
        output[index] = 2.0 * weight * (y_log_y(value, mean) + y_log_y(1.0 - value, 1.0 - mean));
    }
    Ok(result)
}

// Correlation and covariance.

/// How `cov` and `cor` treat missing values: `use`'s choices, in the order of its codes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MissingPolicy {
    AllObservations,
    CompleteObservations,
    Pairwise,
    Everything,
    NaOrComplete,
}

unsafe fn cov(call: &NativeCall<'_>) -> R<Sexp> {
    correlation(call, false)
}

unsafe fn cor(call: &NativeCall<'_>) -> R<Sexp> {
    correlation(call, true)
}

/// `cov(x, y, na.method, kendall)` and `cor(...)`: Pearson (or Kendall) covariances or
/// correlations between the columns of `x`, or of `x` and `y`. Means are refined by a second
/// pass and sums are accumulated in extended precision, as GNU R does.
unsafe fn correlation(call: &NativeCall<'_>, correlate: bool) -> R<Sexp> {
    let x_argument = call.arg(0);
    let y_argument = call.arg(1);
    let policy = match values::as_integer_scalar(call.arg(2)) {
        1 => MissingPolicy::AllObservations,
        2 => MissingPolicy::CompleteObservations,
        3 => MissingPolicy::Pairwise,
        4 => MissingPolicy::Everything,
        5 => MissingPolicy::NaOrComplete,
        _ => return Err(call.error("invalid 'use' (computational method)")),
    };
    let kendall = values::as_logical_scalar(call.arg(3)) == 1;
    if x_argument == values::nil() {
        return Err(call.error("'x' is NULL"));
    }
    let x = real_values(x_argument)?;
    let (n, x_columns) = matrix_shape(x_argument);
    if length(x_argument) == 0 {
        return Err(call.error("'x' is empty"));
    }
    let has_y = y_argument != values::nil();
    let (y, y_columns) = if has_y {
        let (y_rows, y_columns) = matrix_shape(y_argument);
        if y_rows != n {
            return Err(call.error("incompatible dimensions"));
        }
        (real_values(y_argument)?, y_columns)
    } else {
        (x.clone(), x_columns)
    };
    let column = |data: &[f64], index: usize| data[index * n..(index + 1) * n].to_vec();
    let x_data: Vec<Vec<f64>> = (0..x_columns).map(|index| column(&x, index)).collect();
    let y_data: Vec<Vec<f64>> = (0..y_columns).map(|index| column(&y, index)).collect();
    let all_columns: Vec<&Vec<f64>> = if has_y { x_data.iter().chain(&y_data).collect() } else { x_data.iter().collect() };
    let any_missing = all_columns.iter().any(|data| data.iter().any(|value| value.is_nan()));
    if policy == MissingPolicy::AllObservations && any_missing {
        return Err(call.error("missing observations in cov/cor"));
    }
    let mut zero_deviation = false;
    let mut result = vec![NA_REAL; x_columns * y_columns];
    if policy == MissingPolicy::Pairwise {
        for i in 0..x_columns {
            for j in 0..y_columns {
                let used: Vec<bool> = (0..n).map(|row| !x_data[i][row].is_nan() && !y_data[j][row].is_nan()).collect();
                result[i + j * x_columns] = if !has_y && i == j && correlate {
                    1.0
                } else {
                    pair_statistic(&x_data[i], &y_data[j], &used, correlate, kendall, &mut zero_deviation)
                };
            }
        }
    } else {
        let used: Vec<bool> = if matches!(policy, MissingPolicy::CompleteObservations | MissingPolicy::NaOrComplete) {
            (0..n).map(|row| all_columns.iter().all(|data| !data[row].is_nan())).collect()
        } else {
            vec![true; n]
        };
        if !used.iter().any(|&flag| flag) && policy == MissingPolicy::CompleteObservations {
            return Err(call.error("no complete element pairs"));
        }
        let has_missing = |data: &Vec<f64>| data.iter().zip(&used).any(|(value, &flag)| flag && value.is_nan());
        let missing_x: Vec<bool> = x_data.iter().map(has_missing).collect();
        let missing_y: Vec<bool> = y_data.iter().map(has_missing).collect();
        for i in 0..x_columns {
            for j in 0..y_columns {
                if !missing_x[i] && !missing_y[j] {
                    result[i + j * x_columns] = complete_covariance(&x_data[i], &y_data[j], &used, kendall);
                }
            }
        }
        if correlate {
            let scales = |data: &[Vec<f64>], missing: &[bool]| -> Vec<f64> {
                data.iter()
                    .zip(missing)
                    .map(|(column, &absent)| if absent { NA_REAL } else { complete_covariance(column, column, &used, kendall).sqrt() })
                    .collect()
            };
            let x_scale = scales(&x_data, &missing_x);
            let y_scale = if has_y { scales(&y_data, &missing_y) } else { x_scale.clone() };
            for i in 0..x_columns {
                for j in 0..y_columns {
                    let slot = &mut result[i + j * x_columns];
                    if !has_y && i == j {
                        *slot = 1.0;
                    } else if slot.is_nan() {
                    } else if x_scale[i] == 0.0 || y_scale[j] == 0.0 {
                        zero_deviation = true;
                        *slot = NA_REAL;
                    } else {
                        *slot = (*slot / (x_scale[i] * y_scale[j])).clamp(-1.0, 1.0);
                    }
                }
            }
        }
    }
    let is_matrix = |value: Sexp| values::dims(value).is_some_and(|dims| dims.len() == 2);
    let answer = if is_matrix(x_argument) || (has_y && is_matrix(y_argument)) {
        let matrix = real_matrix(&result, x_columns, y_columns)?;
        let x_names = column_names(x_argument);
        let y_names = if has_y { column_names(y_argument) } else { x_names };
        if x_names != values::nil() || y_names != values::nil() {
            let dimnames = values::list_vector(&[x_names, y_names])?;
            values::set_attr(matrix, symbols().dimnames, dimnames)?;
        }
        matrix
    } else {
        values::real_vector(&result)?
    };
    if zero_deviation {
        call.warning("the standard deviation is zero")?;
    }
    Ok(answer)
}

unsafe fn column_names(x: Sexp) -> Sexp {
    let dimnames = values::get_attr(x, symbols().dimnames);
    if dimnames.kind() == SexpType::List && length(dimnames) == 2 { values::list_elt(dimnames, 1) } else { values::nil() }
}

/// A sum accumulated with an error term, standing in for the 80-bit `long double` GNU R sums
/// in, so that rounding agrees in the last digit.
#[derive(Clone, Copy, Default)]
pub(crate) struct Accumulator {
    high: f64,
    low: f64,
}

impl Accumulator {
    pub(crate) fn add(&mut self, value: f64) {
        let sum = self.high + value;
        let error = if self.high.abs() >= value.abs() { (self.high - sum) + value } else { (value - sum) + self.high };
        self.high = sum;
        self.low += error;
    }

    pub(crate) fn value(self) -> f64 {
        self.high + self.low
    }

    pub(crate) fn divided(self, divisor: f64) -> f64 {
        let quotient = self.high / divisor;
        let remainder = (-quotient).mul_add(divisor, self.high);
        quotient + (remainder + self.low) / divisor
    }
}

/// The mean of the used entries, refined by a second pass over the deviations.
fn refined_mean(data: &[f64], used: &[bool]) -> f64 {
    let count = used.iter().filter(|&&flag| flag).count() as f64;
    let mut sum = Accumulator::default();
    for (value, _) in data.iter().zip(used).filter(|(_, flag)| **flag) {
        sum.add(*value);
    }
    let mean = sum.divided(count);
    if !mean.is_finite() {
        return mean;
    }
    let mut correction = Accumulator::default();
    for (value, _) in data.iter().zip(used).filter(|(_, flag)| **flag) {
        correction.add(value - mean);
    }
    mean + correction.divided(count)
}

/// The covariance of two columns over the used rows, or Kendall's sum of concordance signs.
fn complete_covariance(x: &[f64], y: &[f64], used: &[bool], kendall: bool) -> f64 {
    let count = used.iter().filter(|&&flag| flag).count();
    if count < 2 {
        return NA_REAL;
    }
    let mut sum = Accumulator::default();
    if kendall {
        for k in 0..x.len() {
            if !used[k] {
                continue;
            }
            for other in 0..x.len() {
                if used[other] {
                    sum.add(sign(x[k] - x[other]) * sign(y[k] - y[other]));
                }
            }
        }
        return sum.value();
    }
    let x_mean = refined_mean(x, used);
    let y_mean = refined_mean(y, used);
    for k in 0..x.len() {
        if used[k] {
            sum.add((x[k] - x_mean) * (y[k] - y_mean));
        }
    }
    sum.divided((count - 1) as f64)
}

/// One entry of a pairwise-complete `cov` or `cor`.
fn pair_statistic(x: &[f64], y: &[f64], used: &[bool], correlate: bool, kendall: bool, zero_deviation: &mut bool) -> f64 {
    let covariance = complete_covariance(x, y, used, kendall);
    if !correlate || covariance.is_nan() {
        return covariance;
    }
    let x_scale = complete_covariance(x, x, used, kendall).sqrt();
    let y_scale = complete_covariance(y, y, used, kendall).sqrt();
    if x_scale == 0.0 || y_scale == 0.0 {
        *zero_deviation = true;
        return NA_REAL;
    }
    (covariance / (x_scale * y_scale)).clamp(-1.0, 1.0)
}

pub(crate) fn sign(value: f64) -> f64 {
    if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        0.0
    }
}
