//! The stats package's native routines, other than the distribution functions: least squares,
//! the GLM link helpers, correlation and distance, and the numerical building blocks behind
//! `fft`, `filter`, `approx`, `spline`, `lowess` and friends. Model formulas are in
//! `stats_models`, the optimizers and integrators in `stats_optimize`.
//!
//! The algorithms follow their published descriptions (LINPACK for the QR decomposition, and
//! the papers each routine cites), in the operation order GNU R uses, so that results agree with
//! GNU R to the last printed digit.

use crate::{
    arith,
    builtin_libraries::{NativeCall, NativeFn, c_result},
    coerce::coerce_vector,
    linalg,
    long_double::LongDouble,
    stats_arima, stats_cluster, stats_deriv, stats_models, stats_nlm, stats_optimize, stats_port,
    stats_spline, stats_tests,
    unwind::{R, error},
    values::{self, length, scalar_bool, scalar_integer, symbols},
};
use heap::object::{NA_INTEGER, NA_REAL, Sexp, SexpType};

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
    ("optim", stats_optimize::optim),
    ("nlm", stats_nlm::nlm),
    ("port_ivset", stats_port::port_ivset),
    ("port_nlminb", stats_port::port_nlminb),
    ("port_nlsb", stats_port::port_nlsb),
    ("optimhess", stats_optimize::optimhess),
    ("zeroin2", stats_optimize::zeroin2),
    ("do_fmin", stats_optimize::do_fmin),
    ("call_dqags", stats_optimize::call_dqags),
    ("call_dqagi", stats_optimize::call_dqagi),
    ("doD", stats_deriv::do_d),
    ("deriv", stats_deriv::deriv),
    ("cov", cov),
    ("cor", cor),
    ("Cdist", cdist),
    ("compcases", compcases),
    ("fft", fft),
    ("mvfft", mvfft),
    ("nextn", nextn),
    ("ApproxTest", approx_test),
    ("Approx", approx),
    ("SplineCoef", spline_coef),
    ("SplineEval", spline_eval),
    ("monoFC_m", mono_fc_m),
    ("lowess", lowess),
    ("ksmooth", ksmooth),
    ("runmed", runmed),
    ("cfilter", cfilter),
    ("rfilter", rfilter),
    ("DoubleCentre", double_centre),
    ("BinDist", bin_dist),
    ("bw_den", bw_den),
    ("bw_den_binned", bw_den_binned),
    ("bw_ucv", bw_ucv),
    ("bw_bcv", bw_bcv),
    ("bw_phi4", bw_phi4),
    ("bw_phi6", bw_phi6),
    ("acf", acf),
    ("pacf1", pacf1),
    ("ar2ma", ar2ma),
    ("ARMAtoMA", arma_to_ma),
    ("intgrt_vec", intgrt_vec),
    ("pp_sum", pp_sum),
    ("TSconv", ts_conv),
    ("isoreg", isoreg),
    ("tukeyline", tukeyline),
    ("Rsm", tukey_smooth),
    ("cutree", stats_cluster::cutree),
    ("kmeans_Lloyd", stats_cluster::kmeans_lloyd),
    ("kmeans_MacQueen", stats_cluster::kmeans_macqueen),
    ("kmns", stats_cluster::kmns),
    ("hclust", stats_cluster::hclust),
    ("hcass2", stats_cluster::hcass2),
    ("numeric_deriv", stats_models::numeric_deriv),
    ("nls_iter", stats_models::nls_iter),
    ("HoltWinters", holt_winters),
    ("eureka", eureka),
    ("multi_yw", multi_yw),
    ("Burg", burg),
    ("SWilk", stats_tests::swilk),
    ("pKolmogorov2x", stats_tests::p_kolmogorov_2x),
    ("pKS2", stats_tests::p_ks2),
    ("psmirnov_exact", stats_tests::psmirnov_exact),
    ("pAnsari", stats_tests::p_ansari),
    ("qAnsari", stats_tests::q_ansari),
    ("pKendall", stats_tests::p_kendall),
    ("pRho", stats_tests::p_rho),
    ("d2x2xk", stats_tests::d2x2xk),
    ("Fisher_sim", stats_tests::fisher_sim),
    ("Fexact", stats_tests::fexact),
    ("chisq_sim", stats_tests::chisq_sim),
    ("r2dtable", stats_tests::r2dtable),
    ("ARIMA_transPars", stats_arima::transpars),
    ("ARIMA_undoPars", stats_arima::undopars),
    ("ARIMA_Invtrans", stats_arima::invtrans),
    ("ARIMA_Gradtrans", stats_arima::gradtrans),
    ("ARIMA_Like", stats_arima::arima_like),
    ("ARIMA_CSS", stats_arima::css),
    ("getQ0", stats_arima::get_q0),
    ("getQ0bis", stats_arima::get_q0_bis),
    ("KalmanLike", stats_arima::kalman_like),
    ("KalmanFore", stats_arima::kalman_fore),
    ("KalmanSmooth", stats_arima::kalman_smooth),
    ("LogLin", loglin),
    ("stl", stl),
    ("rbart", stats_spline::rbart),
    ("bvalus", stats_spline::bvalus),
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
    let integers = if x.kind() == SexpType::Integer {
        values::materialize(x)?
    } else {
        coerce_vector(x, SexpType::Integer)?
    };
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
    // LINPACK's dqrls: base's dqrdc2, then Q'y, the coefficients and the residuals of every
    // column of `y` from the first `rank` columns of the decomposition.
    let integer = |value: usize| values::scalar_integer(value as i32);
    let qraux = values::alloc(SexpType::Real, p)?;
    let pivot = values::integer_vector(&(1..=p as i32).collect::<Vec<_>>())?;
    let rank_slot = values::scalar_integer(0)?;
    let work = values::alloc(SexpType::Real, 2 * p)?;
    let tolerance_value = values::scalar_real(tolerance)?;
    linalg::dqrdc2(&[
        qr,
        integer(n)?,
        integer(n)?,
        integer(p)?,
        tolerance_value,
        rank_slot,
        qraux,
        pivot,
        work,
    ])?;
    let rank = values::ints(rank_slot)[0].max(0) as usize;
    if rank > 0 {
        let size = integer(rank)?;
        let columns = integer(ny)?;
        let solved = values::alloc(SexpType::Real, rank * ny)?;
        let info = values::scalar_integer(0)?;
        linalg::dqrcf(&[qr, integer(n)?, size, qraux, effects, columns, solved, info])?;
        let y_copy = values::duplicate(as_reals(y)?)?;
        linalg::dqrrsd(&[qr, integer(n)?, size, qraux, y_copy, columns, residuals])?;
        let b = values::reals_mut(coefficients);
        let solved = values::reals(solved);
        for column in 0..ny {
            b[column * p..column * p + rank]
                .copy_from_slice(&solved[column * rank..(column + 1) * rank]);
        }
    }
    let qraux = values::reals(qraux).to_vec();
    let pivot = values::ints(pivot).to_vec();
    let pivoted = pivot
        .iter()
        .enumerate()
        .any(|(index, &column)| column != index as i32 + 1);
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
            return Err(call.error(format!(
                "Value {} out of range (0, 1)",
                crate::format::c_general(value, 6, false)
            )));
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
            return Err(call.error(format!(
                "argument {name} must be a numeric vector of length 1 or length {n}"
            )));
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
    if [x_argument, y_argument].iter().any(|argument| {
        argument.kind() == SexpType::Integer && values::inherits(*argument, "factor")
    }) {
        return Err(call.error(
            "Calling var(x) on a factor x is defunct.\n  Use something like 'all(duplicated(x)[-1L])' to test for a constant vector.",
        ));
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
    let all_columns: Vec<&Vec<f64>> = if has_y {
        x_data.iter().chain(&y_data).collect()
    } else {
        x_data.iter().collect()
    };
    let any_missing = all_columns
        .iter()
        .any(|data| data.iter().any(|value| value.is_nan()));
    if policy == MissingPolicy::AllObservations && any_missing {
        return Err(call.error("missing observations in cov/cor"));
    }
    let mut zero_deviation = false;
    let mut result = vec![NA_REAL; x_columns * y_columns];
    if policy == MissingPolicy::Pairwise {
        for i in 0..x_columns {
            for j in 0..y_columns {
                let used: Vec<bool> = (0..n)
                    .map(|row| !x_data[i][row].is_nan() && !y_data[j][row].is_nan())
                    .collect();
                result[i + j * x_columns] = if !has_y && i == j && correlate {
                    1.0
                } else {
                    pair_statistic(
                        &x_data[i],
                        &y_data[j],
                        &used,
                        correlate,
                        kendall,
                        &mut zero_deviation,
                    )
                };
            }
        }
    } else {
        let used: Vec<bool> = if matches!(
            policy,
            MissingPolicy::CompleteObservations | MissingPolicy::NaOrComplete
        ) {
            (0..n)
                .map(|row| all_columns.iter().all(|data| !data[row].is_nan()))
                .collect()
        } else {
            vec![true; n]
        };
        if !used.iter().any(|&flag| flag) && policy == MissingPolicy::CompleteObservations {
            return Err(call.error("no complete element pairs"));
        }
        let has_missing = |data: &Vec<f64>| {
            data.iter()
                .zip(&used)
                .any(|(value, &flag)| flag && value.is_nan())
        };
        let missing_x: Vec<bool> = x_data.iter().map(has_missing).collect();
        let missing_y: Vec<bool> = y_data.iter().map(has_missing).collect();
        for i in 0..x_columns {
            for j in 0..y_columns {
                if !missing_x[i] && !missing_y[j] {
                    result[i + j * x_columns] =
                        complete_covariance(&x_data[i], &y_data[j], &used, kendall).to_f64();
                }
            }
        }
        if correlate {
            let scales = |data: &[Vec<f64>], missing: &[bool]| -> Vec<f64> {
                data.iter()
                    .zip(missing)
                    .map(|(column, &absent)| {
                        if absent {
                            NA_REAL
                        } else {
                            complete_covariance(column, column, &used, kendall)
                                .sqrt()
                                .to_f64()
                        }
                    })
                    .collect()
            };
            let x_scale = scales(&x_data, &missing_x);
            let y_scale = if has_y {
                scales(&y_data, &missing_y)
            } else {
                x_scale.clone()
            };
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
        let y_names = if has_y {
            column_names(y_argument)
        } else {
            x_names
        };
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
    if dimnames.kind() == SexpType::List && length(dimnames) == 2 {
        values::list_elt(dimnames, 1)
    } else {
        values::nil()
    }
}

/// The covariance of two columns over the used rows, or Kendall's sum of concordance signs.
fn complete_covariance(x: &[f64], y: &[f64], used: &[bool], kendall: bool) -> LongDouble {
    let count = used.iter().filter(|&&flag| flag).count();
    if count < 2 {
        return LongDouble::from(NA_REAL);
    }
    let mut sum = LongDouble::ZERO;
    if kendall {
        for k in 0..x.len() {
            if !used[k] {
                continue;
            }
            for other in 0..x.len() {
                if used[other] {
                    sum += sign(x[k] - x[other]) * sign(y[k] - y[other]);
                }
            }
        }
        return sum;
    }
    let x_mean = arith::long_double_mean(used_values(x, used));
    let y_mean = arith::long_double_mean(used_values(y, used));
    for k in 0..x.len() {
        if used[k] {
            sum = sum + (LongDouble::from(x[k]) - x_mean) * (LongDouble::from(y[k]) - y_mean);
        }
    }
    sum / (count - 1)
}

fn used_values<'a>(data: &'a [f64], used: &'a [bool]) -> impl Iterator<Item = f64> + Clone + 'a {
    data.iter()
        .zip(used)
        .filter(|(_, flag)| **flag)
        .map(|(value, _)| *value)
}

/// One entry of a pairwise-complete `cov` or `cor`.
fn pair_statistic(
    x: &[f64],
    y: &[f64],
    used: &[bool],
    correlate: bool,
    kendall: bool,
    zero_deviation: &mut bool,
) -> f64 {
    let covariance = complete_covariance(x, y, used, kendall).to_f64();
    if !correlate || covariance.is_nan() {
        return covariance;
    }
    let x_scale = complete_covariance(x, x, used, kendall).sqrt().to_f64();
    let y_scale = complete_covariance(y, y, used, kendall).sqrt().to_f64();
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

// Distances and complete cases.

/// `Cdist(x, method, attrs, p)`: the lower triangle of the distance matrix between the rows of
/// `x`, by columns. Missing coordinates are skipped and the sum scaled up for them.
unsafe fn cdist(call: &NativeCall<'_>) -> R<Sexp> {
    let x_argument = call.arg(0);
    let method = values::as_integer_scalar(call.arg(1));
    let attributes = call.arg(2);
    let power = values::as_real_scalar(call.arg(3));
    let (rows, columns) = matrix_shape(x_argument);
    let x = real_values(x_argument)?;
    if method == 6 && !(power.is_finite() && power > 0.0) {
        return Err(call.error("distance(): invalid p"));
    }
    let at = |row: usize, column: usize| x[row + column * rows];
    let distance = |first: usize, second: usize| -> f64 {
        let mut total = 0.0;
        let mut count = 0usize;
        match method {
            5 => {
                let mut differing = 0.0;
                let mut nonzero = 0usize;
                for column in 0..columns {
                    let (a, b) = (at(first, column), at(second, column));
                    if a.is_nan() || b.is_nan() {
                        continue;
                    }
                    if !a.is_finite() || !b.is_finite() {
                        continue;
                    }
                    count += 1;
                    if a != 0.0 || b != 0.0 {
                        nonzero += 1;
                        if !(a != 0.0 && b != 0.0) {
                            differing += 1.0;
                        }
                    }
                }
                return if count == 0 {
                    NA_REAL
                } else if nonzero == 0 {
                    0.0
                } else {
                    differing / nonzero as f64
                };
            }
            2 => {
                let mut largest = f64::MIN;
                for column in 0..columns {
                    let difference = (at(first, column) - at(second, column)).abs();
                    if !difference.is_nan() {
                        count += 1;
                        if difference > largest {
                            largest = difference;
                        }
                    }
                }
                return if count == 0 { NA_REAL } else { largest };
            }
            _ => {}
        }
        for column in 0..columns {
            let (a, b) = (at(first, column), at(second, column));
            if a.is_nan() || b.is_nan() {
                continue;
            }
            let term = match method {
                1 => {
                    let difference = a - b;
                    difference * difference
                }
                3 => (a - b).abs(),
                4 => {
                    let sum = a.abs() + b.abs();
                    let difference = (a - b).abs();
                    if sum > f64::MIN_POSITIVE || difference > f64::MIN_POSITIVE {
                        let ratio = difference / sum;
                        if !ratio.is_nan() {
                            ratio
                        } else if !difference.is_finite() && difference == sum {
                            1.0
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                _ => (a - b).abs().powf(power),
            };
            if term.is_nan() {
                continue;
            }
            total += term;
            count += 1;
        }
        if count == 0 {
            return NA_REAL;
        }
        if count != columns {
            total /= count as f64 / columns as f64;
        }
        match method {
            1 => total.sqrt(),
            6 => total.powf(1.0 / power),
            _ => total,
        }
    };
    let mut result = Vec::with_capacity(rows * rows.saturating_sub(1) / 2);
    for second in 0..rows {
        for first in second + 1..rows {
            result.push(distance(first, second));
        }
    }
    let answer = values::real_vector(&result)?;
    let names = values::names(attributes);
    for index in 0..length(attributes) {
        if let Some(name) = values::string_at(names, index) {
            values::set_attr(
                answer,
                values::install(&name)?,
                values::list_elt(attributes, index),
            )?;
        }
    }
    Ok(answer)
}

/// `complete.cases(...)`: which cases have no missing value in any argument.
unsafe fn compcases(call: &NativeCall<'_>) -> R<Sexp> {
    let mut columns: Vec<(Sexp, usize)> = Vec::new();
    let mut cases: Option<usize> = None;
    let mut record = |value: Sexp, rows: usize, columns: &mut Vec<(Sexp, usize)>| -> R<()> {
        match cases {
            Some(count) if count != rows => {
                return Err(call.error("not all arguments have the same length"));
            }
            _ => cases = Some(rows),
        }
        columns.push((value, rows));
        Ok(())
    };
    for arg in call.args {
        let value = arg.value;
        let items: Vec<Sexp> = match value.kind() {
            SexpType::List => {
                // A data frame without columns still has its rows, which its row names count.
                let row_names = values::get_attr(value, symbols().row_names);
                if length(value) == 0 && row_names != values::nil() {
                    let rows = stats_models::compact_row_count(row_names)
                        .unwrap_or_else(|| length(row_names));
                    record(value, rows, &mut columns)?;
                    continue;
                }
                values::list_items(value)
            }
            SexpType::Pairlist => values::pairlist_items(value)
                .into_iter()
                .map(|(_, item)| item)
                .collect(),
            SexpType::Nil => continue,
            kind if kind.is_atomic() => vec![value],
            _ => {
                return Err(call.error(format!(
                    "invalid 'type' ({}) of argument",
                    value.kind().name()
                )));
            }
        };
        for item in items {
            if !item.kind().is_atomic() {
                return Err(call.error(format!(
                    "invalid 'type' ({}) of argument",
                    item.kind().name()
                )));
            }
            let rows = matrix_shape(item).0;
            record(item, rows, &mut columns)?;
        }
    }
    let Some(count) = cases else {
        return Err(call.error("no input has determined the number of cases"));
    };
    let mut complete = vec![1; count];
    for (value, rows) in columns {
        if rows == 0 {
            continue;
        }
        let value = values::materialize(value)?;
        for index in 0..length(value) {
            let missing = match value.kind() {
                SexpType::Logical | SexpType::Integer => {
                    values::ints(value)[index] == heap::object::NA_INTEGER
                }
                SexpType::Real => values::reals(value)[index].is_nan(),
                SexpType::Complex => {
                    let entry = values::complexes(value)[index];
                    entry.r.is_nan() || entry.i.is_nan()
                }
                SexpType::String => value.element(index) == values::na_string(),
                _ => false,
            };
            if missing {
                complete[index % rows] = 0;
            }
        }
    }
    values::logical_vector(&complete)
}

// Fourier transforms.

/// `fft(z, inverse)`: the unnormalized discrete Fourier transform of a vector, or of every
/// dimension of an array.
unsafe fn fft(call: &NativeCall<'_>) -> R<Sexp> {
    let z = complex_copy(call.arg(0))?;
    let sign = fft_sign(call)?;
    let data = values::complexes_mut(z);
    match values::dims(z) {
        Some(dims) if dims.len() > 1 => {
            let mut stride = 1;
            for &extent in &dims {
                let block = stride * extent;
                if extent > 1 {
                    for start in (0..data.len()).step_by(block.max(1)) {
                        for offset in 0..stride {
                            let indices: Vec<usize> =
                                (0..extent).map(|k| start + offset + k * stride).collect();
                            let mut line: Vec<Complex> = indices
                                .iter()
                                .map(|&index| Complex::from(data[index]))
                                .collect();
                            transform(&mut line, sign);
                            for (&index, value) in indices.iter().zip(line) {
                                data[index] = value.into();
                            }
                        }
                    }
                }
                stride = block;
            }
        }
        _ => {
            let mut line: Vec<Complex> = data.iter().map(|&value| Complex::from(value)).collect();
            transform(&mut line, sign);
            for (slot, value) in data.iter_mut().zip(line) {
                *slot = value.into();
            }
        }
    }
    Ok(z)
}

/// `mvfft(z, inverse)`: the transform of each column of a matrix.
unsafe fn mvfft(call: &NativeCall<'_>) -> R<Sexp> {
    let z = complex_copy(call.arg(0))?;
    let sign = fft_sign(call)?;
    let Some(dims) = values::dims(z).filter(|dims| dims.len() == 2) else {
        return Err(call.error("vector-valued (multivariate) series required"));
    };
    let (rows, columns) = (dims[0], dims[1]);
    let data = values::complexes_mut(z);
    for column in 0..columns {
        let slice = &mut data[column * rows..(column + 1) * rows];
        let mut line: Vec<Complex> = slice.iter().map(|&value| Complex::from(value)).collect();
        transform(&mut line, sign);
        for (slot, value) in slice.iter_mut().zip(line) {
            *slot = value.into();
        }
    }
    Ok(z)
}

unsafe fn fft_sign(call: &NativeCall<'_>) -> R<f64> {
    let inverse = values::as_logical_scalar(call.arg(1));
    Ok(if inverse == heap::object::NA_LOGICAL || inverse == 0 {
        -2.0
    } else {
        2.0
    })
}

/// A fresh complex copy of `z`, keeping its attributes.
unsafe fn complex_copy(z: Sexp) -> R<Sexp> {
    let converted = coerce_vector(z, SexpType::Complex)?;
    if converted == z {
        values::duplicate(z)
    } else {
        Ok(converted)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Complex {
    re: f64,
    im: f64,
}

impl From<heap::object::Rcomplex> for Complex {
    fn from(value: heap::object::Rcomplex) -> Complex {
        Complex {
            re: value.r,
            im: value.i,
        }
    }
}

impl From<Complex> for heap::object::Rcomplex {
    fn from(value: Complex) -> heap::object::Rcomplex {
        heap::object::Rcomplex {
            r: value.re,
            i: value.im,
        }
    }
}

/// An in-place mixed-radix transform: `sign` is -2 for the forward transform (exponent
/// -2πi jk/n) and 2 for the inverse. The length is split into prime factors, and each pass
/// combines butterflies of one factor, decimating in time (the scheme of Singleton's mixed
/// radix algorithm, which GNU R uses).
fn transform(data: &mut [Complex], sign: f64) {
    let n = data.len();
    if n <= 1 {
        return;
    }
    let factors = factorize(n);
    let output = mixed_radix(data, &factors, sign / 2.0);
    data.copy_from_slice(&output);
}

fn factorize(mut n: usize) -> Vec<usize> {
    let mut factors = Vec::new();
    while n % 4 == 0 {
        factors.push(4);
        n /= 4;
    }
    if n % 2 == 0 {
        factors.push(2);
        n /= 2;
    }
    let mut divisor = 3;
    while divisor * divisor <= n {
        while n % divisor == 0 {
            factors.push(divisor);
            n /= divisor;
        }
        divisor += 2;
    }
    if n > 1 {
        factors.push(n);
    }
    factors
}

/// The recursive Cooley-Tukey step for the first factor `p` of `n = p m`: transform the `p`
/// decimated subsequences of length `m`, then combine them with twiddles.
fn mixed_radix(data: &[Complex], factors: &[usize], direction: f64) -> Vec<Complex> {
    let n = data.len();
    let Some((&radix, rest)) = factors.split_first() else {
        return data.to_vec();
    };
    if n == 1 {
        return data.to_vec();
    }
    let m = n / radix;
    let subsequences: Vec<Vec<Complex>> = (0..radix)
        .map(|offset| {
            let decimated: Vec<Complex> = (0..m).map(|k| data[offset + k * radix]).collect();
            mixed_radix(&decimated, rest, direction)
        })
        .collect();
    let angle = direction * 2.0 * std::f64::consts::PI / n as f64;
    let mut output = vec![Complex::default(); n];
    for k in 0..m {
        let twiddled: Vec<Complex> = (0..radix)
            .map(|offset| {
                let value = subsequences[offset][k];
                if offset == 0 || k == 0 {
                    return value;
                }
                let (sin, cos) = unit_root(angle, offset * k, n);
                Complex {
                    re: value.re * cos - value.im * sin,
                    im: value.re * sin + value.im * cos,
                }
            })
            .collect();
        for q in 0..radix {
            let mut sum = Complex::default();
            for (offset, value) in twiddled.iter().enumerate() {
                let (sin, cos) = unit_root(
                    direction * 2.0 * std::f64::consts::PI / radix as f64,
                    offset * q,
                    radix,
                );
                sum.re += value.re * cos - value.im * sin;
                sum.im += value.re * sin + value.im * cos;
            }
            output[k + q * m] = sum;
        }
    }
    output
}

/// `(sin, cos)` of `angle * multiple`, exact at the multiples of a quarter turn so that the
/// transforms of exactly representable data keep their exact zeros.
fn unit_root(angle: f64, multiple: usize, period: usize) -> (f64, f64) {
    let reduced = multiple % period;
    if reduced == 0 {
        return (0.0, 1.0);
    }
    if (4 * reduced) % period == 0 {
        let quarter = 4 * reduced / period;
        let direction = angle.signum();
        return match quarter {
            1 => (direction, 0.0),
            2 => (0.0, -1.0),
            _ => (-direction, 0.0),
        };
    }
    let theta = angle * reduced as f64;
    (theta.sin(), theta.cos())
}

/// `nextn(n, factors)`: for each `n`, the smallest integer at least `n` that factors entirely
/// into `factors`.
unsafe fn nextn(call: &NativeCall<'_>) -> R<Sexp> {
    let factors = integer_values(call.arg(1))?;
    if factors.is_empty() {
        return Err(call.error("no factors"));
    }
    if factors
        .iter()
        .any(|&factor| factor == NA_INTEGER || factor <= 1)
    {
        return Err(call.error("invalid factors"));
    }
    let next = |value: u64| {
        let mut candidate = value.max(1);
        loop {
            let mut rest = candidate;
            for &factor in &factors {
                while rest % factor as u64 == 0 {
                    rest /= factor as u64;
                }
            }
            if rest == 1 {
                return candidate;
            }
            candidate += 1;
        }
    };
    // A double `n` gives an integer result unless some answer exceeds the integer range, so
    // `nextn(2.14e9)` is `2^31` as a double.
    if call.arg(0).kind() == SexpType::Real {
        let result: Vec<Option<u64>> = real_values(call.arg(0))?
            .iter()
            .map(|&value| (!value.is_nan()).then(|| next(value.trunc().max(1.0) as u64)))
            .collect();
        if result
            .iter()
            .flatten()
            .all(|&value| value <= i32::MAX as u64)
        {
            let result: Vec<i32> = result
                .iter()
                .map(|value| value.map_or(NA_INTEGER, |value| value as i32))
                .collect();
            return values::integer_vector(&result);
        }
        let result: Vec<f64> = result
            .iter()
            .map(|value| value.map_or(NA_REAL, |value| value as f64))
            .collect();
        return values::real_vector(&result);
    }
    let mut overflow = false;
    let result: Vec<i32> = integer_values(call.arg(0))?
        .iter()
        .map(|&value| {
            if value == NA_INTEGER {
                return NA_INTEGER;
            }
            i32::try_from(next(value.max(1) as u64)).unwrap_or_else(|_| {
                overflow = true;
                NA_INTEGER
            })
        })
        .collect();
    if overflow {
        call.warning(&format!(
            "nextn() found no solution < {} = INT_MAX (the maximal integer); pass '0+ n' instead of 'n'",
            i32::MAX
        ))?;
    }
    values::integer_vector(&result)
}

// Interpolation.

/// `ApproxTest(x, y, method, f, na.rm)`: validates `approx`'s inputs.
unsafe fn approx_test(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let y = real_values(call.arg(1))?;
    let method = values::as_integer_scalar(call.arg(2));
    let f = values::as_real_scalar(call.arg(3));
    let remove_missing = values::as_logical_scalar(call.arg(4)) == 1;
    if method == 2 && (!f.is_finite() || !(0.0..=1.0).contains(&f)) {
        return Err(call.error("approx(): invalid f value"));
    }
    if method != 1 && method != 2 {
        return Err(call.error("approx(): invalid interpolation method"));
    }
    if remove_missing {
        if x.iter().zip(&y).any(|(a, b)| a.is_nan() || b.is_nan()) {
            return Err(call.error("approx(): attempted to interpolate NA values"));
        }
    } else if x.iter().any(|value| value.is_nan()) {
        return Err(call.error("approx(x,y, .., na.rm=FALSE): NA values in x are not allowed"));
    }
    Ok(values::nil())
}

/// `Approx(x, y, v, method, yleft, yright, f, na.rm)`: linear or constant interpolation of
/// the sorted points `(x, y)` at `v`.
unsafe fn approx(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let y = real_values(call.arg(1))?;
    let v = real_values(call.arg(2))?;
    let method = values::as_integer_scalar(call.arg(3));
    let left = values::as_real_scalar(call.arg(4));
    let right = values::as_real_scalar(call.arg(5));
    let f = values::as_real_scalar(call.arg(6));
    let n = x.len();
    let (f1, f2) = (1.0 - f, f);
    let interpolate = |point: f64| -> f64 {
        if point.is_nan() {
            return point;
        }
        if n == 0 {
            return f64::NAN;
        }
        if point < x[0] {
            return left;
        }
        if point > x[n - 1] {
            return right;
        }
        let (mut low, mut high) = (0, n - 1);
        while low + 1 < high {
            let middle = (low + high) / 2;
            if point < x[middle] {
                high = middle;
            } else {
                low = middle;
            }
        }
        if point == x[high] {
            return y[high];
        }
        if point == x[low] {
            return y[low];
        }
        if method == 1 {
            y[low] + (y[high] - y[low]) * ((point - x[low]) / (x[high] - x[low]))
        } else {
            (if f1 != 0.0 { y[low] * f1 } else { 0.0 })
                + (if f2 != 0.0 { y[high] * f2 } else { 0.0 })
        }
    };
    let result: Vec<f64> = v.iter().map(|&point| interpolate(point)).collect();
    values::real_vector(&result)
}

/// `SplineCoef(method, x, y)`: the coefficients of an interpolating cubic spline, as
/// `list(method, n, x, y, b, c, d)` with `s(u) = y[i] + b[i] h + c[i] h^2 + d[i] h^3` for
/// `h = u - x[i]`. Method 1 is periodic, 2 natural and 3 Forsythe, Malcolm and Moler's.
unsafe fn spline_coef(call: &NativeCall<'_>) -> R<Sexp> {
    let method = values::as_integer_scalar(call.arg(0));
    let x = real_values(call.arg(1))?;
    let y = real_values(call.arg(2))?;
    if x.len() != y.len() {
        return Err(call.error("inputs of different lengths"));
    }
    let (b, c, d) = match method {
        1 => periodic_spline(&x, &y),
        2 => natural_spline(&x, &y),
        3 => fmm_spline(&x, &y),
        _ => return Err(call.error("invalid spline method")),
    };
    values::named_list(&[
        ("method", values::scalar_integer(method)?),
        ("n", values::scalar_integer(x.len() as i32)?),
        ("x", values::real_vector(&x)?),
        ("y", values::real_vector(&y)?),
        ("b", values::real_vector(&b)?),
        ("c", values::real_vector(&c)?),
        ("d", values::real_vector(&d)?),
    ])
}

type SplineCoefficients = (Vec<f64>, Vec<f64>, Vec<f64>);

/// The two-point spline: a straight line.
fn linear_spline(x: &[f64], y: &[f64]) -> SplineCoefficients {
    let n = x.len();
    let mut b = vec![0.0; n];
    if n == 2 {
        let slope = (y[1] - y[0]) / (x[1] - x[0]);
        b = vec![slope, slope];
    }
    (b, vec![0.0; n], vec![0.0; n])
}

/// The cubic spline whose end conditions match the third derivatives of the cubics through the
/// first and last four points (Forsythe, Malcolm and Moler, "Computer Methods for Mathematical
/// Computations", 1977, routine SPLINE).
fn fmm_spline(x: &[f64], y: &[f64]) -> SplineCoefficients {
    let n = x.len();
    if n < 3 {
        return linear_spline(x, y);
    }
    let last = n - 1;
    // The tridiagonal system: `b` the diagonal, `d` the off-diagonal, `c` the right-hand side.
    let mut b = vec![0.0; n];
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    d[0] = x[1] - x[0];
    c[1] = (y[1] - y[0]) / d[0];
    for i in 1..last {
        d[i] = x[i + 1] - x[i];
        b[i] = 2.0 * (d[i - 1] + d[i]);
        c[i + 1] = (y[i + 1] - y[i]) / d[i];
        c[i] = c[i + 1] - c[i];
    }
    b[0] = -d[0];
    b[last] = -d[last - 1];
    c[0] = 0.0;
    c[last] = 0.0;
    if n > 3 {
        c[0] = c[2] / (x[3] - x[1]) - c[1] / (x[2] - x[0]);
        c[last] = c[last - 1] / (x[last] - x[last - 2]) - c[last - 2] / (x[last - 1] - x[last - 3]);
        c[0] = c[0] * d[0] * d[0] / (x[3] - x[0]);
        c[last] = -c[last] * d[last - 1] * d[last - 1] / (x[last] - x[last - 3]);
    }
    for i in 1..=last {
        let factor = d[i - 1] / b[i - 1];
        b[i] -= factor * d[i - 1];
        c[i] -= factor * c[i - 1];
    }
    c[last] /= b[last];
    for i in (0..last).rev() {
        c[i] = (c[i] - d[i] * c[i + 1]) / b[i];
    }
    b[last] = (y[last] - y[last - 1]) / d[last - 1] + d[last - 1] * (c[last - 1] + 2.0 * c[last]);
    for i in 0..last {
        b[i] = (y[i + 1] - y[i]) / d[i] - d[i] * (c[i + 1] + 2.0 * c[i]);
        d[i] = (c[i + 1] - c[i]) / d[i];
        c[i] *= 3.0;
    }
    c[last] *= 3.0;
    d[last] = d[last - 1];
    (b, c, d)
}

/// The natural cubic spline: zero second derivative at both ends.
fn natural_spline(x: &[f64], y: &[f64]) -> SplineCoefficients {
    let n = x.len();
    if n < 3 {
        return linear_spline(x, y);
    }
    let last = n - 1;
    let mut b = vec![0.0; n];
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    d[0] = x[1] - x[0];
    c[1] = (y[1] - y[0]) / d[0];
    for i in 1..last {
        d[i] = x[i + 1] - x[i];
        b[i] = 2.0 * (d[i - 1] + d[i]);
        c[i + 1] = (y[i + 1] - y[i]) / d[i];
        c[i] = c[i + 1] - c[i];
    }
    for i in 2..last {
        let factor = d[i - 1] / b[i - 1];
        b[i] -= factor * d[i - 1];
        c[i] -= factor * c[i - 1];
    }
    c[last - 1] /= b[last - 1];
    for i in (1..last - 1).rev() {
        c[i] = (c[i] - d[i] * c[i + 1]) / b[i];
    }
    c[0] = 0.0;
    c[last] = 0.0;
    b[0] = (y[1] - y[0]) / d[0] - d[0] * c[1];
    d[0] = c[1] / d[0];
    b[last] = (y[last] - y[last - 1]) / d[last - 1] + d[last - 1] * c[last - 1];
    for i in 1..last {
        b[i] = (y[i + 1] - y[i]) / d[i] - d[i] * (c[i + 1] + 2.0 * c[i]);
        d[i] = (c[i + 1] - c[i]) / d[i];
        c[i] *= 3.0;
    }
    c[last] = 0.0;
    d[last] = 0.0;
    (b, c, d)
}

/// The periodic cubic spline, from the cyclic tridiagonal system for the second derivatives,
/// solved by a Cholesky factorization whose last row and column carry the wrap-around.
fn periodic_spline(x: &[f64], y: &[f64]) -> SplineCoefficients {
    let n = x.len();
    if n < 2 || y[0] != y[n - 1] {
        return (vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n]);
    }
    if n == 2 {
        return (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    }
    if n == 3 {
        let (h1, h2) = (x[1] - x[0], x[2] - x[1]);
        let slope = -(y[0] - y[1]) * (x[0] - 2.0 * x[1] + x[2]) / h2 / h1;
        let c0 = -3.0 * (y[0] - y[1]) / h2 / h1;
        let d0 = -2.0 * c0 / 3.0 / h1;
        let d1 = -d0 * h1 / h2;
        return (vec![slope; 3], vec![c0, -c0, c0], vec![d0, d1, d0]);
    }
    // Unknowns are the second derivatives (over two) at points 0..m-1, with point m = point 0.
    let m = n - 1;
    let h: Vec<f64> = (0..m).map(|i| x[i + 1] - x[i]).collect();
    let mut diagonal = vec![0.0; m];
    let mut rhs = vec![0.0; m];
    for i in 0..m {
        let previous = if i == 0 { m - 1 } else { i - 1 };
        diagonal[i] = 2.0 * (h[i] + h[previous]);
        rhs[i] =
            (y[i + 1] - y[i]) / h[i] - (y[i] - y[if i == 0 { m - 1 } else { i - 1 }]) / h[previous];
    }
    // Cholesky factor: `diagonal` its diagonal, `lower` its subdiagonal, `corner` its last row.
    let mut lower = vec![0.0; m];
    let mut corner = vec![0.0; m];
    diagonal[0] = diagonal[0].sqrt();
    corner[0] = h[m - 1] / diagonal[0];
    let mut corner_squares = 0.0;
    for i in 0..m - 2 {
        lower[i] = h[i] / diagonal[i];
        if i != 0 {
            corner[i] = -corner[i - 1] * lower[i - 1] / diagonal[i];
        }
        diagonal[i + 1] = (diagonal[i + 1] - lower[i] * lower[i]).sqrt();
        corner_squares += corner[i] * corner[i];
    }
    lower[m - 2] = (h[m - 2] - corner[m - 3] * lower[m - 3]) / diagonal[m - 2];
    diagonal[m - 1] = (diagonal[m - 1] - lower[m - 2] * lower[m - 2] - corner_squares).sqrt();
    let mut sigma = rhs;
    sigma[0] /= diagonal[0];
    let mut corner_sum = 0.0;
    for i in 1..m - 1 {
        sigma[i] = (sigma[i] - lower[i - 1] * sigma[i - 1]) / diagonal[i];
        corner_sum += corner[i - 1] * sigma[i - 1];
    }
    sigma[m - 1] = (sigma[m - 1] - lower[m - 2] * sigma[m - 2] - corner_sum) / diagonal[m - 1];
    sigma[m - 1] /= diagonal[m - 1];
    sigma[m - 2] = (sigma[m - 2] - lower[m - 2] * sigma[m - 1]) / diagonal[m - 2];
    for i in (0..m - 2).rev() {
        sigma[i] = (sigma[i] - lower[i] * sigma[i + 1] - corner[i] * sigma[m - 1]) / diagonal[i];
    }
    let mut b = vec![0.0; n];
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    sigma.push(sigma[0]);
    for i in 0..m {
        b[i] = (y[i + 1] - y[i]) / h[i] - h[i] * (sigma[i + 1] + 2.0 * sigma[i]);
        d[i] = (sigma[i + 1] - sigma[i]) / h[i];
        c[i] = 3.0 * sigma[i];
    }
    b[m] = b[0];
    c[m] = c[0];
    d[m] = d[0];
    (b, c, d)
}

/// `SplineEval(xout, z)`: evaluates the spline `z` from `SplineCoef` at `xout`.
unsafe fn spline_eval(call: &NativeCall<'_>) -> R<Sexp> {
    let u = real_values(call.arg(0))?;
    let z = call.arg(1);
    let component = |name: &str| real_values(values::list_get(z, name));
    let method = values::as_integer_scalar(values::list_get(z, "method"));
    let x = component("x")?;
    let y = component("y")?;
    let b = component("b")?;
    let c = component("c")?;
    let d = component("d")?;
    let n = x.len();
    let mut result = Vec::with_capacity(u.len());
    let mut interval = 0usize;
    for &point in &u {
        if n == 0 {
            result.push(NA_REAL);
            continue;
        }
        let mut position = point;
        if method == 1 && n > 1 {
            let period = x[n - 1] - x[0];
            position = (point - x[0]) % period;
            if position < 0.0 {
                position += period;
            }
            position += x[0];
        }
        if position < x[interval] || (interval < n - 1 && x[interval + 1] < position) {
            let (mut low, mut high) = (0usize, n);
            while high > low + 1 {
                let middle = (low + high) / 2;
                if position < x[middle] {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            interval = low;
        }
        let offset = position - x[interval];
        let cubic = if method == 2 && position < x[0] {
            0.0
        } else {
            d[interval]
        };
        result.push(y[interval] + offset * (b[interval] + offset * (c[interval] + offset * cubic)));
    }
    values::real_vector(&result)
}

/// `monoFC_m(m, Sx)`: Fritsch and Carlson's adjustment of the Hermite slopes `m` so that the
/// interpolant of data with secant slopes `Sx` is monotone.
unsafe fn mono_fc_m(call: &NativeCall<'_>) -> R<Sexp> {
    let mut slopes = real_values(call.arg(0))?;
    let secants = real_values(call.arg(1))?;
    let n = slopes.len();
    if n < 2 {
        return Err(call.error("n must be at least two"));
    }
    if secants.len() != n - 1 {
        return Err(call.error("Argument Sx must be numeric vector one shorter than m[]"));
    }
    for k in 0..n - 1 {
        let secant = secants[k];
        if secant == 0.0 {
            slopes[k] = 0.0;
            slopes[k + 1] = 0.0;
        } else {
            let alpha = slopes[k] / secant;
            let beta = slopes[k + 1] / secant;
            let a2b3 = 2.0 * alpha + beta - 3.0;
            let ab23 = alpha + 2.0 * beta - 3.0;
            if a2b3 > 0.0 && ab23 > 0.0 && alpha * (a2b3 + ab23) < a2b3 * a2b3 {
                let tau = 3.0 * secant / (alpha * alpha + beta * beta).sqrt();
                slopes[k] = tau * alpha;
                slopes[k + 1] = tau * beta;
            }
        }
    }
    values::real_vector(&slopes)
}

// Smoothing.

/// `lowess(x, y, f, iter, delta)`: Cleveland's robust locally weighted regression ("Robust
/// locally weighted regression and smoothing scatterplots", JASA 1979) of `y` on the sorted
/// `x`: tricube-weighted local lines over the `f n` nearest points, refit `iter` times with
/// bisquare robustness weights, fitting exactly only at points more than `delta` apart and
/// interpolating linearly between them.
unsafe fn lowess(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let y = real_values(call.arg(1))?;
    let span = values::as_real_scalar(call.arg(2));
    let iterations = values::as_integer_scalar(call.arg(3));
    let delta = values::as_real_scalar(call.arg(4));
    let n = x.len();
    if !span.is_finite() || span <= 0.0 {
        return Err(call.error("'f' must be finite and > 0"));
    }
    if iterations == NA_INTEGER || iterations < 0 {
        return Err(call.error("'iter' must be finite and >= 0"));
    }
    if !delta.is_finite() || delta < 0.0 {
        return Err(call.error("'delta' must be finite and > 0"));
    }
    if y.len() != n {
        return Err(call.error("inputs of different lengths"));
    }
    let fitted = clowess(&x, &y, span, iterations as usize, delta);
    values::real_vector(&fitted)
}

fn clowess(x: &[f64], y: &[f64], span: f64, iterations: usize, delta: f64) -> Vec<f64> {
    let n = x.len();
    if n < 2 {
        return y.to_vec();
    }
    let neighbors = ((span * n as f64 + 1e-7) as usize).clamp(2, n);
    let mut fitted = vec![0.0; n];
    let mut robustness = vec![1.0; n];
    let mut weights = vec![0.0; n];
    let mut residuals = vec![0.0; n];
    for iteration in 0..=iterations {
        let use_robustness = iteration > 0;
        let (mut left, mut right) = (0usize, neighbors - 1);
        let mut last: Option<usize> = None;
        let mut i = 0usize;
        loop {
            while right < n - 1 {
                let to_left = x[i] - x[left];
                let to_right = x[right + 1] - x[i];
                if to_left <= to_right {
                    break;
                }
                left += 1;
                right += 1;
            }
            match local_fit(
                x,
                y,
                x[i],
                left,
                right,
                &mut weights,
                use_robustness.then_some(&robustness[..]),
            ) {
                Some(value) => fitted[i] = value,
                None => fitted[i] = y[i],
            }
            if let Some(previous) = last
                && previous + 1 < i
            {
                let span_width = x[i] - x[previous];
                for j in previous + 1..i {
                    let alpha = (x[j] - x[previous]) / span_width;
                    fitted[j] = alpha * fitted[i] + (1.0 - alpha) * fitted[previous];
                }
            }
            let mut anchor = i;
            let cut = x[anchor] + delta;
            let mut next = anchor + 1;
            while next < n {
                if x[next] > cut {
                    break;
                }
                if x[next] == x[anchor] {
                    fitted[next] = fitted[anchor];
                    anchor = next;
                }
                next += 1;
            }
            last = Some(anchor);
            i = (anchor + 1).max(next - 1);
            if anchor >= n - 1 {
                break;
            }
        }
        for k in 0..n {
            residuals[k] = y[k] - fitted[k];
        }
        let scale = residuals.iter().map(|value| value.abs()).sum::<f64>() / n as f64;
        if iteration == iterations {
            break;
        }
        let mut absolute: Vec<f64> = residuals.iter().map(|value| value.abs()).collect();
        absolute.sort_by(f64::total_cmp);
        let half = n / 2;
        let median = if n % 2 == 0 {
            (absolute[half - 1] + absolute[half]) / 2.0
        } else {
            absolute[half]
        };
        let cmad = 6.0 * median;
        if cmad < 1e-7 * scale {
            break;
        }
        let (high, low) = (0.999 * cmad, 0.001 * cmad);
        for k in 0..n {
            let r = residuals[k].abs();
            robustness[k] = if r <= low {
                1.0
            } else if r <= high {
                let ratio = r / cmad;
                (1.0 - ratio * ratio) * (1.0 - ratio * ratio)
            } else {
                0.0
            };
        }
    }
    fitted
}

/// The local weighted line through the points `left..=right` (and any further ties) at `xs`,
/// or `None` when every weight is zero.
fn local_fit(
    x: &[f64],
    y: &[f64],
    xs: f64,
    left: usize,
    right: usize,
    weights: &mut [f64],
    robustness: Option<&[f64]>,
) -> Option<f64> {
    let n = x.len();
    let range = x[n - 1] - x[0];
    let h = (xs - x[left]).max(x[right] - xs);
    let (h9, h1) = (0.999 * h, 0.001 * h);
    let cube = |value: f64| value * value * value;
    let mut total = 0.0;
    let mut j = left;
    while j < n {
        weights[j] = 0.0;
        let r = (x[j] - xs).abs();
        if r <= h9 {
            weights[j] = if r <= h1 {
                1.0
            } else {
                cube(1.0 - cube(r / h))
            };
            if let Some(robustness) = robustness {
                weights[j] *= robustness[j];
            }
            total += weights[j];
        } else if x[j] > xs {
            break;
        }
        j += 1;
    }
    let end = j;
    if total <= 0.0 {
        return None;
    }
    for weight in &mut weights[left..end] {
        *weight /= total;
    }
    if h > 0.0 {
        let mean: f64 = (left..end).map(|k| weights[k] * x[k]).sum();
        let mut slope = xs - mean;
        let spread: f64 = (left..end)
            .map(|k| weights[k] * (x[k] - mean) * (x[k] - mean))
            .sum();
        if spread.sqrt() > 0.001 * range {
            slope /= spread;
            for k in left..end {
                weights[k] *= slope * (x[k] - mean) + 1.0;
            }
        }
    }
    Some((left..end).map(|k| weights[k] * y[k]).sum())
}

/// `ksmooth(x, y, x.points, kernel, bandwidth)`: the Nadaraya-Watson kernel regression with a
/// box (1) or normal (2) kernel whose quartiles are at `±bandwidth/4`.
unsafe fn ksmooth(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let y = real_values(call.arg(1))?;
    let points = real_values(call.arg(2))?;
    let kernel = values::as_integer_scalar(call.arg(3));
    let mut bandwidth = values::as_real_scalar(call.arg(4));
    let n = x.len();
    let cutoff = match kernel {
        1 => {
            bandwidth *= 0.5;
            bandwidth
        }
        _ => {
            bandwidth *= 0.3706506;
            4.0 * bandwidth
        }
    };
    let mut first = 0usize;
    while first < n
        && points
            .first()
            .is_some_and(|&point| x[first] < point - cutoff)
    {
        first += 1;
    }
    let mut fitted = Vec::with_capacity(points.len());
    for &point in &points {
        let (mut numerator, mut denominator) = (0.0, 0.0);
        for i in first..n {
            if x[i] < point - cutoff {
                first = i;
            } else {
                if x[i] > point + cutoff {
                    break;
                }
                let distance = (x[i] - point).abs() / bandwidth;
                let weight = if kernel == 1 {
                    1.0
                } else {
                    (-0.5 * distance * distance).exp()
                };
                numerator += weight * y[i];
                denominator += weight;
            }
        }
        fitted.push(if denominator > 0.0 {
            numerator / denominator
        } else {
            NA_REAL
        });
    }
    values::named_list(&[
        ("x", values::real_vector(&points)?),
        ("y", values::real_vector(&fitted)?),
    ])
}

/// `runmed(x, k, iend, naAction, ...)`: running medians of odd span `k`. Ends keep the data
/// (`iend` 0) or take the median of the first or last `k` points (`iend` 1).
unsafe fn runmed(call: &NativeCall<'_>) -> R<Sexp> {
    let mut x = real_values(call.arg(0))?;
    let span = values::as_integer_scalar(call.arg(2)).max(1) as usize;
    let end_rule = values::as_integer_scalar(call.arg(3));
    let na_action = values::as_integer_scalar(call.arg(4));
    let n = x.len();
    let missing: Vec<usize> = (0..n).filter(|&index| x[index].is_nan()).collect();
    if !missing.is_empty() {
        match na_action {
            4 => return Err(call.error("runmed(x, .., na.action=\"na.fail\"): have NAs in x")),
            3 => {
                let kept: Vec<f64> = x.iter().copied().filter(|value| !value.is_nan()).collect();
                let smoothed = running_median(&kept, span, end_rule);
                let mut result = vec![NA_REAL; n];
                let mut source = smoothed.into_iter();
                for (slot, value) in result.iter_mut().zip(&x) {
                    if !value.is_nan() {
                        *slot = source.next().unwrap_or(NA_REAL);
                    } else {
                        *slot = *value;
                    }
                }
                return values::real_vector(&result);
            }
            _ => {
                let mut big = if na_action == 2 { -f64::MAX } else { f64::MAX };
                for &index in &missing {
                    x[index] = big;
                    big = -big;
                }
            }
        }
    }
    let mut result = running_median(&x, span, end_rule);
    for &index in &missing {
        if result[index].abs() == f64::MAX {
            result[index] = NA_REAL;
        }
    }
    for value in result.iter_mut() {
        if value.abs() == f64::MAX {
            *value = NA_REAL;
        }
    }
    values::real_vector(&result)
}

fn running_median(x: &[f64], span: usize, end_rule: i32) -> Vec<f64> {
    let n = x.len();
    let half = span / 2;
    if n == 0 || span <= 1 || n < span {
        return x.to_vec();
    }
    let median = |window: &[f64]| {
        let mut sorted = window.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[sorted.len() / 2]
    };
    let mut result = x.to_vec();
    for i in half..n - half {
        result[i] = median(&x[i - half..=i + half]);
    }
    if end_rule != 0 {
        let first = result[half];
        let last = result[n - 1 - half];
        result[..half].fill(first);
        result[n - half..].fill(last);
    }
    result
}

/// `cfilter(x, filter, sides, circular)`: the convolution filter behind `filter()`.
unsafe fn cfilter(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let filter = real_values(call.arg(1))?;
    let sides = values::as_integer_scalar(call.arg(2));
    let circular = values::as_logical_scalar(call.arg(3)) == 1;
    if sides == NA_INTEGER {
        return Err(call.error("invalid input"));
    }
    let (nx, nf) = (x.len() as isize, filter.len() as isize);
    let shift = if sides == 2 { nf / 2 } else { 0 };
    let mut result = vec![NA_REAL; x.len()];
    for i in 0..nx {
        if !circular && (i + shift - (nf - 1) < 0 || i + shift >= nx) {
            continue;
        }
        let mut sum = 0.0;
        let mut missing = false;
        for j in 0..nf {
            let mut index = i + shift - j;
            if circular {
                index = index.rem_euclid(nx);
            }
            let value = x[index as usize];
            if value.is_nan() {
                missing = true;
                break;
            }
            sum += filter[j as usize] * value;
        }
        if !missing {
            result[i as usize] = sum;
        }
    }
    values::real_vector(&result)
}

/// `rfilter(x, filter, out)`: the recursive filter behind `filter()`; `out` holds the initial
/// values followed by room for the result.
unsafe fn rfilter(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let filter = real_values(call.arg(1))?;
    let mut out = real_values(call.arg(2))?;
    let nf = filter.len();
    for i in 0..x.len() {
        let mut sum = x[i];
        for j in 0..nf {
            let value = out[nf + i - j - 1];
            if value.is_nan() {
                sum = NA_REAL;
                break;
            }
            sum += value * filter[j];
        }
        out[nf + i] = sum;
    }
    values::real_vector(&out)
}

// Small numerical helpers.

/// `DoubleCentre(A)`: subtracts the row means, then the column means, of a square matrix.
unsafe fn double_centre(call: &NativeCall<'_>) -> R<Sexp> {
    let a = values::duplicate(as_reals(call.arg(0))?)?;
    let (n, _) = matrix_shape(a);
    let data = values::reals_mut(a);
    for row in 0..n {
        let mean = (0..n).map(|column| data[row + column * n]).sum::<f64>() / n as f64;
        for column in 0..n {
            data[row + column * n] -= mean;
        }
    }
    for column in 0..n {
        let mean = (0..n).map(|row| data[row + column * n]).sum::<f64>() / n as f64;
        for row in 0..n {
            data[row + column * n] -= mean;
        }
    }
    Ok(a)
}

/// `BinDist(x, weights, lo, up, n)`: linear binning of weighted points onto `n` grid points
/// from `lo` to `up`, in a vector of twice that length as `density` needs for its FFT.
unsafe fn bin_dist(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let weights = real_values(call.arg(1))?;
    let low = values::as_real_scalar(call.arg(2));
    let high = values::as_real_scalar(call.arg(3));
    let n = values::as_integer_scalar(call.arg(4));
    if n == NA_INTEGER || n <= 0 {
        return Err(call.error("invalid 'n' argument"));
    }
    let n = n as usize;
    let mut y = vec![0.0; 2 * n];
    let last = n as isize - 2;
    let step = (high - low) / (n as f64 - 1.0);
    for (index, &value) in x.iter().enumerate() {
        if !value.is_finite() {
            continue;
        }
        let position = (value - low) / step;
        let bin = position.floor() as isize;
        let fraction = position - bin as f64;
        let weight = weights[index];
        if (0..=last).contains(&bin) {
            y[bin as usize] += weight * (1.0 - fraction);
            y[bin as usize + 1] += weight * fraction;
        } else if bin == -1 {
            y[0] += weight * fraction;
        } else if bin == last + 1 {
            y[bin as usize] += weight * (1.0 - fraction);
        }
    }
    values::real_vector(&y)
}

/// `bw_den(nb, x)`: the binned counts of the pairwise distances of `x`, for the bandwidth
/// selectors.
unsafe fn bw_den(call: &NativeCall<'_>) -> R<Sexp> {
    let bins = values::as_integer_scalar(call.arg(0)).max(1) as usize;
    let x = real_values(call.arg(1))?;
    let (low, high) = x
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), &value| {
            (low.min(value), high.max(value))
        });
    let range = (high - low) * 1.01;
    let width = range / bins as f64;
    let mut counts = vec![0.0; bins];
    for i in 1..x.len() {
        let first = (x[i] / width) as i64;
        for j in 0..i {
            let second = (x[j] / width) as i64;
            let bin = (first - second).unsigned_abs() as usize;
            if bin < bins {
                counts[bin] += 1.0;
            }
        }
    }
    values::list_vector(&[values::scalar_real(width)?, values::real_vector(&counts)?])
}

/// `bw_den_binned(counts)`: pairwise distance counts from already binned data.
unsafe fn bw_den_binned(call: &NativeCall<'_>) -> R<Sexp> {
    let x = integer_values(call.arg(0))?;
    let bins = x.len();
    let mut counts = vec![0.0; bins];
    for i in 0..bins {
        let weight = x[i] as f64;
        counts[0] += weight * (weight - 1.0);
        for j in 0..i {
            counts[i - j] += weight * x[j] as f64;
        }
    }
    counts[0] *= 0.5;
    values::real_vector(&counts)
}

/// The bandwidth selectors' criteria, from the binned distance counts `cnt` of bin width `d`.
unsafe fn bandwidth_criterion(
    call: &NativeCall<'_>,
    criterion: fn(f64) -> f64,
) -> R<(f64, f64, f64)> {
    let n = values::as_real_scalar(call.arg(0));
    let width = values::as_real_scalar(call.arg(1));
    let counts = real_values(call.arg(2))?;
    let h = values::as_real_scalar(call.arg(3));
    let mut sum = 0.0;
    for (bin, &count) in counts.iter().enumerate() {
        let mut delta = bin as f64 * width / h;
        delta *= delta;
        if delta >= 1000.0 {
            break;
        }
        sum += criterion(delta) * count;
    }
    Ok((n, h, sum))
}

const SQRT_PI: f64 = 1.772_453_850_905_516;
const SQRT_2PI: f64 = 2.506_628_274_631_000_5;

unsafe fn bw_ucv(call: &NativeCall<'_>) -> R<Sexp> {
    let (n, h, sum) = bandwidth_criterion(call, |delta| {
        (-delta / 4.0).exp() - 8f64.sqrt() * (-delta / 2.0).exp()
    })?;
    values::scalar_real((0.5 + sum / n) / (n * h * SQRT_PI))
}

unsafe fn bw_bcv(call: &NativeCall<'_>) -> R<Sexp> {
    let (n, h, sum) = bandwidth_criterion(call, |delta| {
        (-delta / 4.0).exp() * (delta * delta - 12.0 * delta + 12.0)
    })?;
    values::scalar_real((1.0 + sum / (32.0 * n)) / (2.0 * n * h * SQRT_PI))
}

unsafe fn bw_phi4(call: &NativeCall<'_>) -> R<Sexp> {
    let (n, h, sum) = bandwidth_criterion(call, |delta| {
        (-delta / 2.0).exp() * (delta * delta - 6.0 * delta + 3.0)
    })?;
    let sum = 2.0 * sum + n * 3.0;
    values::scalar_real(sum / (n * (n - 1.0) * h.powi(5) * SQRT_2PI))
}

unsafe fn bw_phi6(call: &NativeCall<'_>) -> R<Sexp> {
    let (n, h, sum) = bandwidth_criterion(call, |delta| {
        (-delta / 2.0).exp() * (delta * delta * delta - 15.0 * delta * delta + 45.0 * delta - 15.0)
    })?;
    let sum = 2.0 * sum - 15.0 * n;
    values::scalar_real(sum / (n * (n - 1.0) * h.powi(7) * SQRT_2PI))
}

// Time series.

/// `acf(x, lag.max, sCor)`: the auto- and cross-covariances (or correlations) of the columns
/// of `x`, as a `lag.max + 1` by `ns` by `ns` array; missing values are skipped.
unsafe fn acf(call: &NativeCall<'_>) -> R<Sexp> {
    let x_argument = call.arg(0);
    let (n, series) = matrix_shape(x_argument);
    let x = real_values(x_argument)?;
    let lags = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let correlate = values::as_logical_scalar(call.arg(2)) == 1;
    let size = lags + 1;
    let mut result = vec![0.0; size * series * series];
    for u in 0..series {
        for v in 0..series {
            for lag in 0..=lags {
                let mut sum = 0.0;
                let mut count = 0usize;
                for i in 0..n.saturating_sub(lag) {
                    let (a, b) = (x[i + lag + n * u], x[i + n * v]);
                    if !a.is_nan() && !b.is_nan() {
                        count += 1;
                        sum += a * b;
                    }
                }
                result[lag + size * u + size * series * v] = if count > 0 {
                    sum / (count + lag) as f64
                } else {
                    NA_REAL
                };
            }
        }
    }
    if correlate {
        if n == 1 {
            for u in 0..series {
                result[size * u + size * series * u] = 1.0;
            }
        } else {
            let scale: Vec<f64> = (0..series)
                .map(|u| result[size * u + size * series * u].sqrt())
                .collect();
            for u in 0..series {
                for v in 0..series {
                    for lag in 0..=lags {
                        let slot = &mut result[lag + size * u + size * series * v];
                        let value = *slot / (scale[u] * scale[v]);
                        *slot = value.clamp(-1.0, 1.0);
                    }
                }
            }
        }
    }
    let answer = values::real_vector(&result)?;
    let dim = values::integer_vector(&[size as i32, series as i32, series as i32])?;
    values::set_attr(answer, symbols().dim, dim)?;
    Ok(answer)
}

/// `pacf1(acf, lag.max)`: the partial autocorrelations, by the Durbin-Levinson recursion.
unsafe fn pacf1(call: &NativeCall<'_>) -> R<Sexp> {
    let correlations = real_values(call.arg(0))?;
    let lags = values::as_integer_scalar(call.arg(1)).max(1) as usize;
    let mut partial = vec![0.0; lags];
    let mut coefficients = vec![0.0; lags];
    let mut reversed = vec![0.0; lags];
    coefficients[0] = correlations[1];
    partial[0] = correlations[1];
    for order in 1..lags {
        let mut numerator = correlations[order + 1];
        let mut denominator = 1.0;
        for i in 0..order {
            numerator -= coefficients[i] * correlations[order - i];
            denominator -= coefficients[i] * correlations[i + 1];
        }
        let value = numerator / denominator;
        partial[order] = value;
        if order + 1 == lags {
            break;
        }
        coefficients[order] = value;
        for i in 0..order {
            reversed[order - i - 1] = coefficients[i];
        }
        for i in 0..order {
            coefficients[i] -= value * reversed[i];
        }
    }
    let answer = values::real_vector(&partial)?;
    let dim = values::integer_vector(&[lags as i32, 1, 1])?;
    values::set_attr(answer, symbols().dim, dim)?;
    Ok(answer)
}

/// `ar2ma(ar, npsi)`: the first `npsi` moving average weights of an autoregression.
unsafe fn ar2ma(call: &NativeCall<'_>) -> R<Sexp> {
    let ar = real_values(call.arg(0))?;
    let count = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let p = ar.len();
    let total = p + count + 1;
    let mut psi = vec![0.0; total];
    psi[..p].copy_from_slice(&ar);
    for i in 0..total - p - 1 {
        for j in 0..p {
            psi[i + j + 1] += ar[j] * psi[i];
        }
    }
    psi.truncate(count);
    values::real_vector(&psi)
}

/// `ARMAtoMA(ar, ma, lag.max)`: the moving average weights of an ARMA process.
unsafe fn arma_to_ma(call: &NativeCall<'_>) -> R<Sexp> {
    let phi = real_values(call.arg(0))?;
    let theta = real_values(call.arg(1))?;
    let lags = values::as_integer_scalar(call.arg(2));
    if lags == NA_INTEGER || lags <= 0 {
        return Err(call.error("invalid value of lag.max"));
    }
    let mut psi = vec![0.0; lags as usize];
    for i in 0..psi.len() {
        let mut value = if i < theta.len() { theta[i] } else { 0.0 };
        for j in 0..(i + 1).min(phi.len()) {
            value += phi[j] * if i >= j + 1 { psi[i - j - 1] } else { 1.0 };
        }
        psi[i] = value;
    }
    values::real_vector(&psi)
}

/// `intgrt_vec(x, xi, lag)`: the inverse of lagged differencing, from initial values `xi`.
unsafe fn intgrt_vec(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let initial = real_values(call.arg(1))?;
    let lag = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let mut y = vec![0.0; x.len() + lag];
    y[..lag.min(initial.len())].copy_from_slice(&initial[..lag.min(initial.len())]);
    for i in 0..x.len() {
        y[i + lag] = x[i] + y[i];
    }
    values::real_vector(&y)
}

/// `pp_sum(u, l)`: the Newey-West weighted sum of autocovariances behind `PP.test`.
unsafe fn pp_sum(call: &NativeCall<'_>) -> R<Sexp> {
    let u = real_values(call.arg(0))?;
    let lags = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let n = u.len();
    let mut total = 0.0;
    for i in 1..=lags {
        let mut sum = 0.0;
        for j in i..n {
            sum += u[j] * u[j - i];
        }
        sum *= 1.0 - i as f64 / (lags as f64 + 1.0);
        total += sum;
    }
    values::scalar_real(2.0 * total / n as f64)
}

/// `TSconv(a, b)`: the product of two polynomials given by their coefficients.
unsafe fn ts_conv(call: &NativeCall<'_>) -> R<Sexp> {
    let a = real_values(call.arg(0))?;
    let b = real_values(call.arg(1))?;
    if a.is_empty() || b.is_empty() {
        return values::real_vector(&[]);
    }
    let mut product = vec![0.0; a.len() + b.len() - 1];
    for (i, &left) in a.iter().enumerate() {
        for (j, &right) in b.iter().enumerate() {
            product[i + j] += left * right;
        }
    }
    values::real_vector(&product)
}

// Isotonic regression.

/// `isoreg(y)`: the isotonic (monotone increasing) least squares fit, as the slopes of the
/// greatest convex minorant of the cumulative sums.
unsafe fn isoreg(call: &NativeCall<'_>) -> R<Sexp> {
    let y_argument = as_reals(call.arg(0))?;
    let y = values::reals(y_argument).to_vec();
    let n = y.len();
    let mut cumulative = vec![0.0; n + 1];
    for i in 0..n {
        cumulative[i + 1] = cumulative[i] + y[i];
    }
    let total = cumulative[n];
    if !total.is_finite() {
        let shown = if total.is_nan() {
            "nan"
        } else if total > 0.0 {
            "inf"
        } else {
            "-inf"
        };
        return Err(error(format!(
            "non-finite sum(y) == {shown} is not allowed"
        )));
    }
    let mut fitted = vec![0.0; n];
    let mut knots: Vec<i32> = Vec::new();
    let mut known = 0usize;
    while known < n {
        let mut slope = f64::INFINITY;
        let mut knot = known;
        for i in known + 1..=n {
            let candidate = (cumulative[i] - cumulative[known]) / (i - known) as f64;
            if candidate < slope {
                slope = candidate;
                knot = i;
            }
        }
        if knot == known {
            break;
        }
        knots.push(knot as i32);
        let level = (cumulative[knot] - cumulative[known]) / (knot - known) as f64;
        fitted[known..knot].fill(level);
        known = knot;
    }
    values::named_list(&[
        ("y", y_argument),
        ("yc", values::real_vector(&cumulative)?),
        ("yf", values::real_vector(&fitted)?),
        ("iKnots", values::integer_vector(&knots)?),
    ])
}

/// `tukeyline(x, y, iter, call)`: Tukey's resistant line. The outer thirds are the points at or
/// beyond the 1/3 and 2/3 quantiles of `x`, each quantile the mean of the order statistics at
/// the floor and ceiling of `p (n - 1)`. Each iteration adds the slope through the medians of
/// the current residuals in the outer thirds, and the intercept is the residuals' median.
unsafe fn tukeyline(call: &NativeCall<'_>) -> R<Sexp> {
    let x = values::reals(as_reals(call.arg(0))?).to_vec();
    let y = values::reals(as_reals(call.arg(1))?).to_vec();
    let iterations = values::as_integer_scalar(call.arg(2)).max(1);
    let n = x.len().min(y.len());
    let mut sorted = x[..n].to_vec();
    sorted.sort_by(f64::total_cmp);
    let quantile = |probability: f64| {
        let position = probability * n.saturating_sub(1) as f64;
        let at = |index: f64| sorted.get(index as usize).copied().unwrap_or(f64::NAN);
        (at(position.floor()) + at(position.ceil())) / 2.0
    };
    let (lower, upper) = (quantile(1.0 / 3.0), quantile(2.0 / 3.0));
    let left: Vec<usize> = (0..n).filter(|&index| x[index] <= lower).collect();
    let right: Vec<usize> = (0..n).filter(|&index| x[index] >= upper).collect();
    let median_of = |values: &[f64], indices: &[usize]| {
        median(
            &mut indices
                .iter()
                .map(|&index| values[index])
                .collect::<Vec<_>>(),
        )
    };
    let x_spread = median_of(&x, &right) - median_of(&x, &left);
    let mut slope = 0.0;
    let mut residuals = y[..n].to_vec();
    for _ in 0..iterations {
        slope += (median_of(&residuals, &right) - median_of(&residuals, &left)) / x_spread;
        for index in 0..n {
            residuals[index] = y[index] - slope * x[index];
        }
    }
    let intercept = median(&mut residuals.clone());
    let fitted: Vec<f64> = x[..n]
        .iter()
        .map(|&value| intercept + slope * value)
        .collect();
    for index in 0..n {
        residuals[index] = y[index] - fitted[index];
    }
    values::named_list(&[
        ("call", call.arg(3)),
        ("coefficients", values::real_vector(&[intercept, slope])?),
        ("residuals", values::real_vector(&residuals)?),
        ("fitted.values", values::real_vector(&fitted)?),
    ])
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let count = values.len();
    match count {
        0 => f64::NAN,
        _ if count % 2 == 1 => values[count / 2],
        _ => (values[count / 2 - 1] + values[count / 2]) / 2.0,
    }
}

/// `.C(HoltWinters, x, xl, alpha, beta, gamma, start.time, seasonal, period, dotrend,
/// doseasonal, a, b, s, SSE, level, trend, seasonal)`: exponential smoothing with optional
/// trend and additive (`seasonal` 1) or multiplicative seasonality, accumulating the sum of
/// squared one-step prediction errors.
unsafe fn holt_winters(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let x = values::reals(arguments[0]).to_vec();
    let length_x = values::as_integer_scalar(arguments[1]).max(0) as usize;
    let alpha = values::as_real_scalar(arguments[2]);
    let beta = values::as_real_scalar(arguments[3]);
    let gamma = values::as_real_scalar(arguments[4]);
    let start = values::as_integer_scalar(arguments[5]).max(1) as usize;
    let additive = values::as_integer_scalar(arguments[6]) == 1;
    let period = values::as_integer_scalar(arguments[7]).max(0) as usize;
    let do_trend = values::as_integer_scalar(arguments[8]) == 1;
    let do_seasonal = values::as_integer_scalar(arguments[9]) == 1;
    let a = values::as_real_scalar(arguments[10]);
    let b = values::as_real_scalar(arguments[11]);
    let s = values::reals(arguments[12]).to_vec();
    let mut sse = 0.0;
    let level = values::reals_mut(arguments[14]);
    let trend = values::reals_mut(arguments[15]);
    let season = values::reals_mut(arguments[16]);
    level[0] = a;
    if do_trend {
        trend[0] = b;
    }
    if do_seasonal {
        season[..period].copy_from_slice(&s[..period]);
    }
    for i in start - 1..length_x {
        let i0 = i + 2 - start;
        let s0 = i0 + period - 1;
        let mut forecast = level[i0 - 1] + if do_trend { trend[i0 - 1] } else { 0.0 };
        let seasonal_term = if do_seasonal {
            season[s0 - period]
        } else if additive {
            0.0
        } else {
            1.0
        };
        if additive {
            forecast += seasonal_term;
        } else {
            forecast *= seasonal_term;
        }
        let residual = x[i] - forecast;
        sse += residual * residual;
        let deseasonalized = if additive {
            x[i] - seasonal_term
        } else {
            x[i] / seasonal_term
        };
        level[i0] = alpha * deseasonalized + (1.0 - alpha) * (level[i0 - 1] + trend[i0 - 1]);
        if do_trend {
            trend[i0] = beta * (level[i0] - level[i0 - 1]) + (1.0 - beta) * trend[i0 - 1];
        }
        if do_seasonal {
            let observed = if additive {
                x[i] - level[i0]
            } else {
                x[i] / level[i0]
            };
            season[s0] = gamma * observed + (1.0 - gamma) * seasonal_term;
        }
    }
    values::reals_mut(arguments[13])[0] = sse;
    c_return(call, &arguments)
}

/// `.Fortran(eureka, lr, r, g, f, var, a)`: Levinson's recursion for the Toeplitz system of
/// the autocovariances `r`, giving the autoregression coefficients of every order up to `lr`
/// (row `l` of `f`) and their innovations variances.
unsafe fn eureka(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let order = values::as_integer_scalar(arguments[0]).max(0) as usize;
    let r = values::reals(arguments[1]).to_vec();
    let g = values::reals(arguments[2]).to_vec();
    let mut f = vec![0.0; order * order];
    let mut variances = vec![0.0; order];
    let mut a = vec![0.0; order + 1];
    // 1-based like the recursion's statement: `f[(l, j)]` is f(l, j), `a[j]` is a(j).
    let at = |l: usize, j: usize| (l - 1) + (j - 1) * order;
    if order >= 1 {
        let mut v = r[0];
        let mut d = r[1];
        a[1] = 1.0;
        f[at(1, 1)] = g[1] / v;
        let mut q = f[at(1, 1)] * r[1];
        variances[0] = (1.0 - f[at(1, 1)] * f[at(1, 1)]) * r[0];
        for l in 2..=order {
            a[l] = -d / v;
            if l > 2 {
                let l1 = (l - 2) / 2;
                let l2 = l1 + 1;
                for j in 2..=l2 {
                    let hold = a[j];
                    let k = l - j + 1;
                    a[j] += a[l] * a[k];
                    a[k] += a[l] * hold;
                }
                if 2 * l1 != l - 2 {
                    a[l2 + 1] *= 1.0 + a[l];
                }
            }
            v += a[l] * d;
            f[at(l, l)] = (g[l] - q) / v;
            for j in 1..l {
                f[at(l, j)] = f[at(l - 1, j)] + f[at(l, l)] * a[l - j + 1];
            }
            variances[l - 1] = variances[l - 2] * (1.0 - f[at(l, l)] * f[at(l, l)]);
            if l == order {
                break;
            }
            d = 0.0;
            q = 0.0;
            for i in 1..=l {
                let k = l - i + 2;
                d += a[i] * r[k - 1];
                q += f[at(l, i)] * r[k - 1];
            }
        }
    }
    values::reals_mut(arguments[3]).copy_from_slice(&f);
    values::reals_mut(arguments[4]).copy_from_slice(&variances);
    values::reals_mut(arguments[5]).copy_from_slice(&a[1..]);
    c_return(call, &arguments)
}

/// `.Call(C_Rsm, x, kind, end_rule)`: Tukey's running median smoothers. `kind` counts from 1
/// through "3RS3R", "3RSS", "3RSR", "3R", "3" and "S". For the median kinds `end_rule` is 1 to
/// copy the end values or 2 for Tukey's end-point rule, negated in the "3RS" kinds to also split
/// flats next to the ends; for "S" it is whether to split them. The result is `list(y, iter)`,
/// or `list(y, changed)` for "S".
unsafe fn tukey_smooth(call: &NativeCall<'_>) -> R<Sexp> {
    let x = values::reals(as_reals(call.arg(0))?).to_vec();
    let kind = values::as_integer_scalar(call.arg(1));
    let end_rule = values::as_integer_scalar(call.arg(2));
    let tukey_ends = end_rule.abs() == 2;
    let split_ends = end_rule < 0;
    if kind == 6 {
        let split = split_flats(&x, end_rule != 0);
        let y = values::real_vector(&split.values)?;
        return values::named_list(&[("y", y), ("changed", scalar_bool(split.changed)?)]);
    }
    if matches!(kind, 4 | 5) && end_rule < 0 {
        return Err(error(format!(
            "invalid end-rule for running median of 3: {end_rule}"
        )));
    }
    let (y, iterations) = match kind {
        5 => {
            let y = median_of_three_pass(&x, tukey_ends);
            let changed = y != x;
            (y, usize::from(changed))
        }
        4 => repeated_median(&x, tukey_ends),
        // The split is kept only when the splitter reports a change. That report is the
        // one GNU R computes, which is not always whether a value moved.
        1 => {
            let (smooth, iterations) = repeated_median(&x, tukey_ends);
            let split = split_flats(&smooth, split_ends);
            if split.changed {
                let (resmoothed, more) = repeated_median(&split.values, tukey_ends);
                (resmoothed, iterations + 1 + more)
            } else {
                (smooth, iterations)
            }
        }
        2 => {
            let (smooth, iterations) = repeated_median(&x, tukey_ends);
            let split = split_flats(&smooth, split_ends);
            if split.changed {
                let again = split_flats(&split.values, split_ends);
                let y = if again.changed {
                    again.values
                } else {
                    split.values
                };
                (y, iterations + 1)
            } else {
                (smooth, iterations)
            }
        }
        3 => {
            let (mut y, mut iterations) = repeated_median(&x, tukey_ends);
            loop {
                iterations += 1;
                let split = split_flats(&y, split_ends);
                let (smooth, passes) = repeated_median(&split.values, tukey_ends);
                y = smooth;
                if !(split.changed || passes > 0) || iterations > 2 * x.len() {
                    break;
                }
            }
            (y, iterations)
        }
        _ => return Err(error(format!("invalid smoothing kind: {kind}"))),
    };
    values::named_list(&[
        ("y", values::real_vector(&y)?),
        ("iter", scalar_integer(iterations as i32)?),
    ])
}

fn median_of_three(a: f64, b: f64, c: f64) -> f64 {
    a.max(b).min(a.min(b).max(c))
}

/// One pass of the running median of three; the ends are copied, or moved by Tukey's end-point
/// rule, which takes the median of the end value, its smoothed neighbour, and the straight
/// line through the two smoothed values next to it.
fn median_of_three_pass(x: &[f64], tukey_ends: bool) -> Vec<f64> {
    let mut y = x.to_vec();
    let n = x.len();
    if n < 3 {
        return y;
    }
    for index in 1..n - 1 {
        y[index] = median_of_three(x[index - 1], x[index], x[index + 1]);
    }
    if tukey_ends {
        y[0] = median_of_three(x[0], y[1], 3.0 * y[1] - 2.0 * y[2]);
        y[n - 1] = median_of_three(x[n - 1], y[n - 2], 3.0 * y[n - 2] - 2.0 * y[n - 3]);
    }
    y
}

/// "3R": running medians repeated until nothing changes, with the number of passes that
/// changed something.
fn repeated_median(x: &[f64], tukey_ends: bool) -> (Vec<f64>, usize) {
    let mut y = x.to_vec();
    let mut passes = 0;
    loop {
        let next = median_of_three_pass(&y, tukey_ends);
        if next == y {
            return (y, passes);
        }
        passes += 1;
        y = next;
    }
}

struct Split {
    values: Vec<f64>,
    changed: bool,
}

/// "S": splits every two-value flat that is a peak or a valley, moving each half to the median
/// of itself, its outer neighbour, and the line through the two values beyond. A flat next to
/// an end is split only when `split_ends` is set, and then its end half copies the end value.
/// `changed` follows GNU R: each interior flat sets it to whether the right half's line misses
/// the flat value and something has changed so far, and a flat at an end sets it.
fn split_flats(x: &[f64], split_ends: bool) -> Split {
    let n = x.len();
    let mut values = x.to_vec();
    let mut changed = false;
    for i in 1..n.saturating_sub(2) {
        let j = i + 1;
        if x[i] != x[j] || (x[i] - x[i - 1]) * (x[j + 1] - x[j]) >= 0.0 {
            continue;
        }
        let left_inside = i >= 2;
        let right_inside = j + 2 < n;
        if !(left_inside && right_inside) && (!split_ends || !(left_inside || right_inside)) {
            continue;
        }
        values[i] = if left_inside {
            median_of_three(x[i], x[i - 1], 3.0 * x[i - 1] - 2.0 * x[i - 2])
        } else {
            x[i - 1]
        };
        let right_line = if right_inside {
            3.0 * x[j + 1] - 2.0 * x[j + 2]
        } else {
            f64::NAN
        };
        values[j] = if right_inside {
            median_of_three(x[j], x[j + 1], right_line)
        } else {
            x[j + 1]
        };
        changed = if left_inside && right_inside {
            right_line != x[j] && (changed || values[i] != x[i] || values[j] != x[j])
        } else {
            true
        };
    }
    Split { values, changed }
}

/// `.C(multi_yw, acf, n.obs, order.max, nser, coefs, pacf, var, aic, order, use.aic)`: the
/// multivariate Yule-Walker fit by Whittle's recursion, which runs a forward and a backward
/// autoregression side by side. Every matrix is an `nser` by `nser` block stored by rows:
/// `acf` holds the lag covariances, `pacf` the forward partial autoregression of each order,
/// `var` the forward innovations variance of each order, and `coefs` the coefficients of the
/// chosen order with the opposite sign (lag 0 is the identity). The chosen order minimises
/// the AIC when `use.aic` is set, and is `order.max` otherwise.
unsafe fn multi_yw(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let observations = values::as_integer_scalar(arguments[1]) as f64;
    let order_max = values::as_integer_scalar(arguments[2]).max(0) as usize;
    let size = values::as_integer_scalar(arguments[3]).max(0) as usize;
    let use_aic = values::as_integer_scalar(arguments[9]) != 0;
    let block = size * size;
    let acf = values::reals(arguments[0]).to_vec();
    if acf.len() < (order_max + 1) * block {
        return Err(error("invalid autocovariances"));
    }
    let lag = |h: usize| &acf[h * block..(h + 1) * block];
    let identity: Vec<f64> = (0..block)
        .map(|index| {
            if index / size == index % size {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    let mut forward: Vec<Vec<f64>> = Vec::new();
    let mut backward: Vec<Vec<f64>> = Vec::new();
    let mut forward_variance = lag(0).to_vec();
    let mut backward_variance = lag(0).to_vec();
    let mut pacf = identity.clone();
    let mut variances = forward_variance.clone();
    let mut aic = vec![observations * matrix_log_determinant(&forward_variance, size)?];
    let mut coefficients_by_order: Vec<Vec<Vec<f64>>> = vec![Vec::new()];
    for order in 1..=order_max {
        let mut innovation = lag(order).to_vec();
        for (k, coefficient) in forward.iter().enumerate() {
            let product = matrix_product(coefficient, lag(order - k - 1), size);
            for (target, value) in innovation.iter_mut().zip(product) {
                *target -= value;
            }
        }
        let innovation_t = matrix_transpose(&innovation, size);
        let forward_last = matrix_product(
            &innovation,
            &matrix_inverse(&backward_variance, size)?,
            size,
        );
        let backward_last = matrix_product(
            &innovation_t,
            &matrix_inverse(&forward_variance, size)?,
            size,
        );
        let mut next_forward = Vec::with_capacity(order);
        let mut next_backward = Vec::with_capacity(order);
        for k in 0..order - 1 {
            let mut f = forward[k].clone();
            for (target, value) in f.iter_mut().zip(matrix_product(
                &forward_last,
                &backward[order - k - 2],
                size,
            )) {
                *target -= value;
            }
            let mut b = backward[k].clone();
            for (target, value) in b.iter_mut().zip(matrix_product(
                &backward_last,
                &forward[order - k - 2],
                size,
            )) {
                *target -= value;
            }
            next_forward.push(f);
            next_backward.push(b);
        }
        for (target, value) in
            forward_variance
                .iter_mut()
                .zip(matrix_product(&forward_last, &innovation_t, size))
        {
            *target -= value;
        }
        for (target, value) in
            backward_variance
                .iter_mut()
                .zip(matrix_product(&backward_last, &innovation, size))
        {
            *target -= value;
        }
        pacf.extend_from_slice(&forward_last);
        next_forward.push(forward_last);
        next_backward.push(backward_last);
        forward = next_forward;
        backward = next_backward;
        variances.extend_from_slice(&forward_variance);
        aic.push(
            observations * matrix_log_determinant(&forward_variance, size)?
                + 2.0 * (order * block) as f64,
        );
        coefficients_by_order.push(forward.clone());
    }
    let chosen = if use_aic {
        let mut best = 0;
        for (order, &value) in aic.iter().enumerate() {
            if value < aic[best] {
                best = order;
            }
        }
        best
    } else {
        order_max
    };
    let mut coefficients = identity;
    for coefficient in &coefficients_by_order[chosen] {
        coefficients.extend(coefficient.iter().map(|value| -value));
    }
    coefficients.resize((order_max + 1) * block, 0.0);
    values::reals_mut(arguments[4]).copy_from_slice(&coefficients);
    values::reals_mut(arguments[5]).copy_from_slice(&pacf);
    values::reals_mut(arguments[6]).copy_from_slice(&variances);
    values::reals_mut(arguments[7]).copy_from_slice(&aic);
    if let Some(slot) = values::ints_mut(arguments[8]).first_mut() {
        *slot = chosen as i32;
    }
    c_return(call, &arguments)
}

fn matrix_product(left: &[f64], right: &[f64], size: usize) -> Vec<f64> {
    let mut result = vec![0.0; size * size];
    for row in 0..size {
        for column in 0..size {
            result[row * size + column] = (0..size)
                .map(|k| left[row * size + k] * right[k * size + column])
                .sum();
        }
    }
    result
}

fn matrix_transpose(matrix: &[f64], size: usize) -> Vec<f64> {
    (0..size * size)
        .map(|index| matrix[(index % size) * size + index / size])
        .collect()
}

/// Gauss-Jordan elimination with partial pivoting.
fn matrix_inverse(matrix: &[f64], size: usize) -> R<Vec<f64>> {
    let mut work = matrix.to_vec();
    let mut inverse: Vec<f64> = (0..size * size)
        .map(|index| {
            if index / size == index % size {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    for column in 0..size {
        let pivot = (column..size)
            .max_by(|&a, &b| {
                work[a * size + column]
                    .abs()
                    .total_cmp(&work[b * size + column].abs())
            })
            .unwrap_or(column);
        if work[pivot * size + column] == 0.0 {
            return Err(error("singular matrix in 'multi_yw'"));
        }
        for k in 0..size {
            work.swap(column * size + k, pivot * size + k);
            inverse.swap(column * size + k, pivot * size + k);
        }
        let scale = work[column * size + column];
        for k in 0..size {
            work[column * size + k] /= scale;
            inverse[column * size + k] /= scale;
        }
        for row in 0..size {
            if row != column {
                let factor = work[row * size + column];
                for k in 0..size {
                    work[row * size + k] -= factor * work[column * size + k];
                    inverse[row * size + k] -= factor * inverse[column * size + k];
                }
            }
        }
    }
    Ok(inverse)
}

/// The log of the determinant, from an LU factorization with partial pivoting.
fn matrix_log_determinant(matrix: &[f64], size: usize) -> R<f64> {
    let mut work = matrix.to_vec();
    let mut determinant = 1.0;
    for column in 0..size {
        let pivot = (column..size)
            .max_by(|&a, &b| {
                work[a * size + column]
                    .abs()
                    .total_cmp(&work[b * size + column].abs())
            })
            .unwrap_or(column);
        if pivot != column {
            for k in 0..size {
                work.swap(column * size + k, pivot * size + k);
            }
            determinant = -determinant;
        }
        let diagonal = work[column * size + column];
        determinant *= diagonal;
        if diagonal == 0.0 {
            break;
        }
        for row in column + 1..size {
            let factor = work[row * size + column] / diagonal;
            for k in column..size {
                work[row * size + k] -= factor * work[column * size + k];
            }
        }
    }
    Ok(determinant.ln())
}

/// `Burg(x, order.max)`: Burg's estimates of the autoregressions of every order up to
/// `order.max`, as `list(coefficients, innovations variance by the recursion, and by the
/// mean squared forward and backward errors)`.
unsafe fn burg(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let order = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let n = x.len();
    let mut coefficients = vec![0.0; order * order];
    let mut variance1 = vec![0.0; order + 1];
    let mut variance2 = vec![0.0; order + 1];
    let mut forward: Vec<f64> = x.iter().rev().copied().collect();
    let mut backward = forward.clone();
    let sum: f64 = x.iter().map(|value| value * value).sum();
    variance1[0] = sum / n as f64;
    variance2[0] = sum / n as f64;
    for p in 1..=order {
        let (mut numerator, mut denominator) = (0.0, 0.0);
        for t in p..n {
            numerator += backward[t] * forward[t - 1];
            denominator += backward[t] * backward[t] + forward[t - 1] * forward[t - 1];
        }
        let reflection = 2.0 * numerator / denominator;
        coefficients[order * (p - 1) + (p - 1)] = reflection;
        for j in 1..p {
            coefficients[p - 1 + order * (j - 1)] = coefficients[p - 2 + order * (j - 1)]
                - reflection * coefficients[p - 2 + order * (p - j - 1)];
        }
        let previous = forward.clone();
        for t in p..n {
            forward[t] = previous[t - 1] - reflection * backward[t];
            backward[t] -= reflection * previous[t - 1];
        }
        variance1[p] = variance1[p - 1] * (1.0 - reflection * reflection);
        let total: f64 = (p..n)
            .map(|t| backward[t] * backward[t] + forward[t] * forward[t])
            .sum();
        variance2[p] = total / (2.0 * (n - p) as f64);
    }
    values::list_vector(&[
        values::real_vector(&coefficients)?,
        values::real_vector(&variance1)?,
        values::real_vector(&variance2)?,
    ])
}

/// `LogLin(dim, config, table, start, nmar, eps, iter)`: iterative proportional fitting of a
/// log-linear model (Haberman's Applied Statistics algorithm AS 51): each cycle scales the
/// fit to every margin in `config` in turn, until the largest difference between a fitted
/// and an observed margin entry falls below `eps`.
unsafe fn loglin(call: &NativeCall<'_>) -> R<Sexp> {
    let dims: Vec<usize> = integer_values(call.arg(0))?
        .iter()
        .map(|&value| value.max(0) as usize)
        .collect();
    let configuration = integer_values(call.arg(1))?;
    let table = real_values(call.arg(2))?;
    let mut fit = real_values(call.arg(3))?;
    let tolerance = values::as_real_scalar(call.arg(5));
    let max_iterations = values::as_integer_scalar(call.arg(6)).max(0) as usize;
    let variables = dims.len();
    if variables == 0 || configuration.len() % variables != 0 {
        return Err(call.error("incorrect specification of 'table' or 'start'"));
    }
    let margins: Vec<Vec<usize>> = configuration
        .chunks(variables)
        .map(|column| {
            column
                .iter()
                .take_while(|&&variable| variable > 0)
                .map(|&variable| variable as usize - 1)
                .collect()
        })
        .collect();
    let cells = table.len();
    // The marginal cell of each table cell for each margin.
    let marginal_index: Vec<(Vec<usize>, usize)> = margins
        .iter()
        .map(|margin| {
            let size: usize = margin.iter().map(|&variable| dims[variable]).product();
            let indices = (0..cells)
                .map(|cell| {
                    let mut rest = cell;
                    let mut coordinates = vec![0usize; variables];
                    for (variable, coordinate) in coordinates.iter_mut().enumerate() {
                        *coordinate = rest % dims[variable];
                        rest /= dims[variable];
                    }
                    let mut index = 0;
                    let mut stride = 1;
                    for &variable in margin {
                        index += coordinates[variable] * stride;
                        stride *= dims[variable];
                    }
                    index
                })
                .collect();
            (indices, size)
        })
        .collect();
    let observed: Vec<Vec<f64>> = marginal_index
        .iter()
        .map(|(indices, size)| {
            let mut sums = vec![0.0; *size];
            for (cell, &index) in indices.iter().enumerate() {
                sums[index] += table[cell];
            }
            sums
        })
        .collect();
    let mut deviations = vec![0.0; max_iterations];
    let mut last = 0usize;
    let mut converged = false;
    for iteration in 0..max_iterations {
        let mut largest = 0.0_f64;
        for (margin, (indices, size)) in marginal_index.iter().enumerate() {
            let mut fitted = vec![0.0; *size];
            for (cell, &index) in indices.iter().enumerate() {
                fitted[index] += fit[cell];
            }
            for (index, value) in fitted.iter().enumerate() {
                largest = largest.max((value - observed[margin][index]).abs());
            }
            for (cell, &index) in indices.iter().enumerate() {
                fit[cell] = if fitted[index] > 0.0 {
                    fit[cell] * observed[margin][index] / fitted[index]
                } else {
                    0.0
                };
            }
        }
        deviations[iteration] = largest;
        last = iteration + 1;
        if largest < tolerance {
            converged = true;
            break;
        }
    }
    if !converged && max_iterations > 0 {
        call.warning("algorithm did not converge")?;
    }
    values::named_list(&[
        ("fit", values::real_vector(&fit)?),
        ("dev", values::real_vector(&deviations)?),
        ("nlast", values::scalar_integer(last as i32)?),
    ])
}

/// `.Fortran(stl, y, n, np, ns, nt, nl, isdeg, itdeg, ildeg, nsjump, ntjump, nljump, ni, no,
/// rw, season, trend, work)`: Cleveland, Cleveland, McRae and Terpenning's seasonal-trend
/// decomposition by loess ("STL", Journal of Official Statistics, 1990): inner loops smooth
/// the cycle subseries and the deseasonalized series alternately, outer loops reweight
/// against outliers.
unsafe fn stl(call: &NativeCall<'_>) -> R<Sexp> {
    let arguments = c_arguments(call)?;
    let y = values::reals(arguments[0]).to_vec();
    let n = values::as_integer_scalar(arguments[1]).max(0) as usize;
    let odd = |value: i32| -> usize {
        let value = value.max(3) as usize;
        if value % 2 == 0 { value + 1 } else { value }
    };
    let period = (values::as_integer_scalar(arguments[2]).max(2)) as usize;
    let seasonal_span = odd(values::as_integer_scalar(arguments[3]));
    let trend_span = odd(values::as_integer_scalar(arguments[4]));
    let lowpass_span = odd(values::as_integer_scalar(arguments[5]));
    let degree = |index: usize| values::as_integer_scalar(arguments[index]);
    let (seasonal_degree, trend_degree, lowpass_degree) = (degree(6), degree(7), degree(8));
    let jump = |index: usize| values::as_integer_scalar(arguments[index]).max(1) as usize;
    let (seasonal_jump, trend_jump, lowpass_jump) = (jump(9), jump(10), jump(11));
    let inner = values::as_integer_scalar(arguments[12]).max(0) as usize;
    let outer = values::as_integer_scalar(arguments[13]).max(0);
    let mut weights = vec![0.0; n];
    let mut season = vec![0.0; n];
    let mut trend = vec![0.0; n];
    let mut robust = false;
    let mut pass = 0;
    loop {
        for _ in 0..inner {
            let detrended: Vec<f64> = (0..n).map(|i| y[i] - trend[i]).collect();
            let cycles = cycle_subseries_smooth(
                &detrended,
                period,
                seasonal_span,
                seasonal_degree,
                seasonal_jump,
                robust,
                &weights,
            );
            let first = moving_average(&cycles, period);
            let second = moving_average(&first, period);
            let lowpass_input = moving_average(&second, 3);
            let lowpass = loess_jumps(
                &lowpass_input,
                lowpass_span,
                lowpass_degree,
                lowpass_jump,
                false,
                &[],
            );
            for i in 0..n {
                season[i] = cycles[period + i] - lowpass[i];
            }
            let deseasonalized: Vec<f64> = (0..n).map(|i| y[i] - season[i]).collect();
            trend = loess_jumps(
                &deseasonalized,
                trend_span,
                trend_degree,
                trend_jump,
                robust,
                &weights,
            );
        }
        pass += 1;
        if pass > outer {
            break;
        }
        let fitted: Vec<f64> = (0..n).map(|i| trend[i] + season[i]).collect();
        weights = robustness_weights(&y, &fitted);
        robust = true;
    }
    if outer <= 0 {
        weights.fill(1.0);
    }
    values::reals_mut(arguments[14]).copy_from_slice(&weights);
    values::reals_mut(arguments[15]).copy_from_slice(&season);
    values::reals_mut(arguments[16]).copy_from_slice(&trend);
    c_return(call, &arguments)
}

/// Smooths each cycle subseries by loess and extends it by one value at each end, giving a
/// series of length `n + 2 period` laid out by cycle position.
fn cycle_subseries_smooth(
    y: &[f64],
    period: usize,
    span: usize,
    degree: i32,
    jump: usize,
    robust: bool,
    weights: &[f64],
) -> Vec<f64> {
    let n = y.len();
    let mut result = vec![0.0; n + 2 * period];
    for j in 0..period {
        let count = if j < n { (n - j - 1) / period + 1 } else { 0 };
        let series: Vec<f64> = (0..count).map(|i| y[i * period + j]).collect();
        let series_weights: Vec<f64> = if robust {
            (0..count).map(|i| weights[i * period + j]).collect()
        } else {
            Vec::new()
        };
        let mut smoothed = vec![0.0; count + 2];
        let inner = loess_jumps(&series, span, degree, jump, robust, &series_weights);
        smoothed[1..count + 1].copy_from_slice(&inner);
        let right = span.min(count);
        smoothed[0] = loess_estimate(
            &series,
            span,
            degree,
            0.0,
            1,
            right,
            robust,
            &series_weights,
        )
        .unwrap_or(smoothed[1]);
        let left = if count >= span { count - span + 1 } else { 1 };
        smoothed[count + 1] = loess_estimate(
            &series,
            span,
            degree,
            (count + 1) as f64,
            left,
            count,
            robust,
            &series_weights,
        )
        .unwrap_or(smoothed[count]);
        for (m, &value) in smoothed.iter().enumerate() {
            let index = m * period + j;
            if index < result.len() {
                result[index] = value;
            }
        }
    }
    result
}

fn moving_average(x: &[f64], length: usize) -> Vec<f64> {
    let count = x.len() + 1 - length;
    let mut result = Vec::with_capacity(count);
    let mut sum: f64 = x[..length].iter().sum();
    result.push(sum / length as f64);
    for j in 1..count {
        sum = sum - x[j - 1] + x[j + length - 1];
        result.push(sum / length as f64);
    }
    result
}

/// Loess at every point (1-based positions), fitting exactly every `jump` points and
/// interpolating linearly between them.
fn loess_jumps(
    y: &[f64],
    span: usize,
    degree: i32,
    jump: usize,
    robust: bool,
    weights: &[f64],
) -> Vec<f64> {
    let n = y.len();
    if n < 2 {
        return y.to_vec();
    }
    let mut fitted = vec![0.0; n];
    let step = jump.min(n - 1);
    let (mut left, mut right) = (1usize, n);
    let fit_at = |i: usize, left: usize, right: usize, fitted: &mut [f64]| {
        fitted[i - 1] = loess_estimate(y, span, degree, i as f64, left, right, robust, weights)
            .unwrap_or(y[i - 1]);
    };
    if span >= n {
        let mut i = 1;
        while i <= n {
            fit_at(i, 1, n, &mut fitted);
            i += step;
        }
    } else if step == 1 {
        let half = (span + 1) / 2;
        right = span;
        for i in 1..=n {
            if i > half && right != n {
                left += 1;
                right += 1;
            }
            fit_at(i, left, right, &mut fitted);
        }
    } else {
        let half = (span + 1) / 2;
        let mut i = 1;
        while i <= n {
            if i < half {
                left = 1;
                right = span;
            } else if i >= n - half + 1 {
                left = n - span + 1;
                right = n;
            } else {
                left = i - half + 1;
                right = span + i - half;
            }
            fit_at(i, left, right, &mut fitted);
            i += step;
        }
    }
    if step != 1 {
        let mut i = 1;
        while i + step <= n {
            let delta = (fitted[i + step - 1] - fitted[i - 1]) / step as f64;
            for j in i + 1..i + step {
                fitted[j - 1] = fitted[i - 1] + delta * (j - i) as f64;
            }
            i += step;
        }
        let k = ((n - 1) / step) * step + 1;
        if k != n {
            fit_at(n, left, right, &mut fitted);
            if k != n - 1 {
                let delta = (fitted[n - 1] - fitted[k - 1]) / (n - k) as f64;
                for j in k + 1..n {
                    fitted[j - 1] = fitted[k - 1] + delta * (j - k) as f64;
                }
            }
        }
    }
    fitted
}

/// The tricube-weighted local constant (`degree` 0) or line at `xs` from points `left..=right`
/// (1-based), or `None` when all weights vanish.
#[allow(clippy::too_many_arguments)]
fn loess_estimate(
    y: &[f64],
    span: usize,
    degree: i32,
    xs: f64,
    left: usize,
    right: usize,
    robust: bool,
    robustness: &[f64],
) -> Option<f64> {
    let n = y.len();
    let range = n as f64 - 1.0;
    let mut h = (xs - left as f64).max(right as f64 - xs);
    if span > n {
        h += ((span - n) / 2) as f64;
    }
    let (h9, h1) = (0.999 * h, 0.001 * h);
    let mut weights = vec![0.0; right + 1];
    let mut total = 0.0;
    for j in left..=right {
        let r = (j as f64 - xs).abs();
        if r <= h9 {
            weights[j] = if r <= h1 {
                1.0
            } else {
                (1.0 - (r / h).powi(3)).powi(3)
            };
            if robust {
                weights[j] *= robustness[j - 1];
            }
            total += weights[j];
        }
    }
    if total <= 0.0 {
        return None;
    }
    for weight in &mut weights[left..=right] {
        *weight /= total;
    }
    if h > 0.0 && degree > 0 {
        let mean: f64 = (left..=right).map(|j| weights[j] * j as f64).sum();
        let mut slope = xs - mean;
        let spread: f64 = (left..=right)
            .map(|j| weights[j] * (j as f64 - mean).powi(2))
            .sum();
        if spread.sqrt() > 0.001 * range {
            slope /= spread;
            for j in left..=right {
                weights[j] *= slope * (j as f64 - mean) + 1.0;
            }
        }
    }
    Some((left..=right).map(|j| weights[j] * y[j - 1]).sum())
}

/// Bisquare weights of the residuals, scaled by six times their median absolute value.
fn robustness_weights(y: &[f64], fitted: &[f64]) -> Vec<f64> {
    let n = y.len();
    let residuals: Vec<f64> = (0..n).map(|i| (y[i] - fitted[i]).abs()).collect();
    let mut sorted = residuals.clone();
    sorted.sort_by(f64::total_cmp);
    let middle1 = n / 2 + 1;
    let middle2 = n - middle1 + 1;
    let cmad = 3.0 * (sorted[middle1 - 1] + sorted[middle2 - 1]);
    let (high, low) = (0.999 * cmad, 0.001 * cmad);
    residuals
        .iter()
        .map(|&r| {
            if r <= low {
                1.0
            } else if r <= high {
                (1.0 - (r / cmad).powi(2)).powi(2)
            } else {
                0.0
            }
        })
        .collect()
}
