//! ARIMA models and the Kalman filter behind `arima`, `StructTS` and `KalmanLike`.
//!
//! A model is the state space form `makeARIMA` builds: state `a` with covariance `P`,
//! transition `T`, state noise covariance `V`, observation `y = Z'a` plus noise of variance
//! `h`. The filter is the textbook one (Harvey, "Forecasting, Structural Time Series Models and
//! the Kalman Filter", 1989; Durbin and Koopman, "Time Series Analysis by State Space Methods",
//! 2001), and the initial covariance of the ARMA part solves the stationary Lyapunov equation
//! `P = T P T' + R R'` directly.

use crate::{
    builtin_libraries::NativeCall,
    stats::{real_matrix, real_values},
    unwind::R,
    values::{self, symbols},
};
use heap::object::{NA_REAL, Sexp, SexpType};

/// The ARMA orders `arima` passes as `arma`: AR, MA, seasonal AR, seasonal MA, period,
/// differences and seasonal differences.
struct Orders {
    p: usize,
    q: usize,
    seasonal_p: usize,
    seasonal_q: usize,
    period: usize,
    differences: usize,
    seasonal_differences: usize,
}

unsafe fn orders(arma: Sexp) -> R<Orders> {
    let values = crate::stats::integer_values(arma)?;
    let at = |index: usize| values.get(index).copied().unwrap_or(0).max(0) as usize;
    Ok(Orders {
        p: at(0),
        q: at(1),
        seasonal_p: at(2),
        seasonal_q: at(3),
        period: at(4),
        differences: at(5),
        seasonal_differences: at(6),
    })
}

/// Maps unconstrained values to the coefficients of a stationary autoregression: `tanh`
/// gives partial autocorrelations in (-1, 1), and the Durbin-Levinson recursion turns them
/// into coefficients (Jones, "Maximum likelihood fitting of ARMA models", 1980).
fn partrans(raw: &[f64]) -> Vec<f64> {
    let p = raw.len();
    let mut coefficients: Vec<f64> = raw.iter().map(|value| value.tanh()).collect();
    let mut work = coefficients.clone();
    for j in 1..p {
        let a = coefficients[j];
        for k in 0..j {
            work[k] -= a * coefficients[j - k - 1];
        }
        coefficients[..j].copy_from_slice(&work[..j]);
    }
    coefficients
}

/// The inverse of [`partrans`]: the recursion run backwards, then `atanh`.
fn invpartrans(phi: &[f64]) -> Vec<f64> {
    let p = phi.len();
    let mut coefficients = phi.to_vec();
    let mut work = coefficients.clone();
    for j in (1..p).rev() {
        let a = coefficients[j];
        for k in 0..j {
            work[k] = (coefficients[k] + a * coefficients[j - k - 1]) / (1.0 - a * a);
        }
        coefficients[..j].copy_from_slice(&work[..j]);
    }
    coefficients.iter().map(|value| value.atanh()).collect()
}

/// `ARIMA_transPars(par, arma, trans)`: the expanded AR and MA polynomials (seasonal factors
/// multiplied out), after mapping the AR parts to stationarity when `trans`.
pub unsafe fn transpars(call: &NativeCall<'_>) -> R<Sexp> {
    let input = real_values(call.arg(0))?;
    let orders = orders(call.arg(1))?;
    let transform = values::as_logical_scalar(call.arg(2)) == 1;
    let (phi, theta) = expand(&input, &orders, transform);
    values::list_vector(&[values::real_vector(&phi)?, values::real_vector(&theta)?])
}

fn expand(input: &[f64], orders: &Orders, transform: bool) -> (Vec<f64>, Vec<f64>) {
    let Orders { p: mp, q: mq, seasonal_p: msp, seasonal_q: msq, period: ns, .. } = *orders;
    let mut parameters = input.to_vec();
    if transform {
        if mp > 0 {
            parameters[..mp].copy_from_slice(&partrans(&input[..mp]));
        }
        let start = mp + mq;
        if msp > 0 {
            parameters[start..start + msp].copy_from_slice(&partrans(&input[start..start + msp]));
        }
    }
    let p = mp + ns * msp;
    let q = mq + ns * msq;
    let mut phi = vec![0.0; p];
    let mut theta = vec![0.0; q];
    phi[..mp].copy_from_slice(&parameters[..mp]);
    theta[..mq].copy_from_slice(&parameters[mp..mp + mq]);
    if ns > 0 {
        for j in 0..msp {
            let seasonal = parameters[j + mp + mq];
            phi[(j + 1) * ns - 1] += seasonal;
            for i in 0..mp {
                phi[(j + 1) * ns + i] -= parameters[i] * seasonal;
            }
        }
        for j in 0..msq {
            let seasonal = parameters[j + mp + mq + msp];
            theta[(j + 1) * ns - 1] += seasonal;
            for i in 0..mq {
                theta[(j + 1) * ns + i] += parameters[i + mp] * seasonal;
            }
        }
    }
    (phi, theta)
}

/// `ARIMA_undoPars(par, arma)`: the parameters with the AR parts mapped to stationarity.
pub unsafe fn undopars(call: &NativeCall<'_>) -> R<Sexp> {
    let input = real_values(call.arg(0))?;
    let orders = orders(call.arg(1))?;
    let mut result = input.clone();
    if orders.p > 0 {
        result[..orders.p].copy_from_slice(&partrans(&input[..orders.p]));
    }
    let start = orders.p + orders.q;
    if orders.seasonal_p > 0 {
        result[start..start + orders.seasonal_p].copy_from_slice(&partrans(&input[start..start + orders.seasonal_p]));
    }
    values::real_vector(&result)
}

/// `ARIMA_Invtrans(par, arma)`: the inverse of `ARIMA_undoPars`.
pub unsafe fn invtrans(call: &NativeCall<'_>) -> R<Sexp> {
    let input = real_values(call.arg(0))?;
    let orders = orders(call.arg(1))?;
    let mut result = input.clone();
    if orders.p > 0 {
        result[..orders.p].copy_from_slice(&invpartrans(&input[..orders.p]));
    }
    let start = orders.p + orders.q;
    if orders.seasonal_p > 0 {
        result[start..start + orders.seasonal_p].copy_from_slice(&invpartrans(&input[start..start + orders.seasonal_p]));
    }
    values::real_vector(&result)
}

/// `ARIMA_Gradtrans(par, arma)`: the Jacobian of `ARIMA_undoPars`, by forward differences
/// with step 1e-3 (identity outside the AR parts).
pub unsafe fn gradtrans(call: &NativeCall<'_>) -> R<Sexp> {
    let raw = real_values(call.arg(0))?;
    let orders = orders(call.arg(1))?;
    let n = raw.len();
    let epsilon = 1e-3;
    let mut jacobian = vec![0.0; n * n];
    for i in 0..n {
        jacobian[i + i * n] = 1.0;
    }
    for (start, count) in [(0, orders.p), (orders.p + orders.q, orders.seasonal_p)] {
        if count == 0 {
            continue;
        }
        let mut point = raw[start..start + count].to_vec();
        let base = partrans(&point);
        for i in 0..count {
            point[i] += epsilon;
            let moved = partrans(&point);
            for j in 0..count {
                jacobian[i + start + (j + start) * n] = (moved[j] - base[j]) / epsilon;
            }
            point[i] -= epsilon;
        }
    }
    real_matrix(&jacobian, n, n)
}

/// `ARIMA_CSS(y, arma, phi, theta, ncond, residuals)`: the conditional sum of squares of an
/// ARIMA model, given the first `ncond` observations, divided by the number of terms.
pub unsafe fn css(call: &NativeCall<'_>) -> R<Sexp> {
    let y = real_values(call.arg(0))?;
    let orders = orders(call.arg(1))?;
    let phi = real_values(call.arg(2))?;
    let theta = real_values(call.arg(3))?;
    let condition = values::as_integer_scalar(call.arg(4)).max(0) as usize;
    let give_residuals = values::as_logical_scalar(call.arg(5)) == 1;
    let n = y.len();
    let mut w = y.clone();
    for _ in 0..orders.differences {
        for l in (1..n).rev() {
            w[l] -= w[l - 1];
        }
    }
    let period = orders.period;
    for _ in 0..orders.seasonal_differences {
        for l in (period..n).rev() {
            w[l] -= w[l - period];
        }
    }
    let mut residuals = vec![0.0; n];
    let mut sum = 0.0;
    let mut count = 0usize;
    for l in condition..n {
        let mut value = w[l];
        for (j, coefficient) in phi.iter().enumerate() {
            value -= coefficient * w[l - j - 1];
        }
        for j in 0..(l - condition).min(theta.len()) {
            value -= theta[j] * residuals[l - j - 1];
        }
        residuals[l] = value;
        if !value.is_nan() {
            count += 1;
            sum += value * value;
        }
    }
    let criterion = values::scalar_real(sum / count as f64)?;
    if give_residuals {
        values::list_vector(&[criterion, values::real_vector(&residuals)?])
    } else {
        Ok(criterion)
    }
}

/// `getQ0(phi, theta)`: the stationary covariance of the ARMA state, the solution of
/// `P = T P T' + R R'` for the companion transition `T` and `R = (1, theta)`.
pub unsafe fn get_q0(call: &NativeCall<'_>) -> R<Sexp> {
    let phi = real_values(call.arg(0))?;
    let theta = real_values(call.arg(1))?;
    let r = phi.len().max(theta.len() + 1);
    let covariance = stationary_covariance(&phi, &theta, r).ok_or_else(|| call.error("the stationary covariance is singular"))?;
    real_matrix(&covariance, r, r)
}

/// `getQ0bis(phi, theta, tol)`: the same covariance, which this implementation computes
/// exactly either way.
pub unsafe fn get_q0_bis(call: &NativeCall<'_>) -> R<Sexp> {
    get_q0(call)
}

/// Solves the Lyapunov equation for the symmetric `r` by `r` covariance, with its upper
/// triangle as unknowns.
fn stationary_covariance(phi: &[f64], theta: &[f64], r: usize) -> Option<Vec<f64>> {
    let transition = |i: usize, j: usize| -> f64 {
        let mut value = 0.0;
        if j == 0 && i < phi.len() {
            value += phi[i];
        }
        if j == i + 1 {
            value += 1.0;
        }
        value
    };
    let noise: Vec<f64> = (0..r).map(|i| if i == 0 { 1.0 } else { theta.get(i - 1).copied().unwrap_or(0.0) }).collect();
    let index = |i: usize, j: usize| -> usize {
        let (low, high) = if i <= j { (i, j) } else { (j, i) };
        low * r - low * (low + 1) / 2 + high
    };
    let unknowns = r * (r + 1) / 2;
    let mut system = vec![0.0; unknowns * unknowns];
    let mut rhs = vec![0.0; unknowns];
    for i in 0..r {
        for j in i..r {
            let row = index(i, j);
            system[row * unknowns + row] += 1.0;
            for k in 0..r {
                let tik = transition(i, k);
                if tik == 0.0 {
                    continue;
                }
                for l in 0..r {
                    let tjl = transition(j, l);
                    if tjl != 0.0 {
                        system[row * unknowns + index(k, l)] -= tik * tjl;
                    }
                }
            }
            rhs[row] = noise[i] * noise[j];
        }
    }
    let solution = solve(&mut system, &mut rhs, unknowns)?;
    let mut covariance = vec![0.0; r * r];
    for i in 0..r {
        for j in 0..r {
            covariance[i + j * r] = solution[index(i, j)];
        }
    }
    Some(covariance)
}

/// Gaussian elimination with partial pivoting of the row-major `n` by `n` system.
fn solve(matrix: &mut [f64], rhs: &mut [f64], n: usize) -> Option<Vec<f64>> {
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
            if factor == 0.0 {
                continue;
            }
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

/// A state space model read from the list `makeARIMA` (or `StructTS`) builds.
struct Model {
    z: Vec<f64>,
    a: Vec<f64>,
    p: Vec<f64>,
    t: Vec<f64>,
    v: Vec<f64>,
    h: f64,
    pn: Vec<f64>,
    size: usize,
}

unsafe fn component(call: &NativeCall<'_>, model: Sexp, name: &str) -> R<Sexp> {
    let value = values::list_get(model, name);
    if value.kind() != SexpType::Real {
        return Err(call.error("invalid argument type"));
    }
    Ok(value)
}

impl Model {
    unsafe fn read(call: &NativeCall<'_>, model: Sexp) -> R<Model> {
        let z = values::reals(component(call, model, "Z")?).to_vec();
        let size = z.len();
        let read = |name: &str| -> R<Vec<f64>> { Ok(values::reals(component(call, model, name)?).to_vec()) };
        let model = Model {
            a: read("a")?,
            p: read("P")?,
            t: read("T")?,
            v: read("V")?,
            h: values::as_real_scalar(component(call, model, "h")?),
            pn: read("Pn")?,
            z,
            size,
        };
        let square = size * size;
        if model.a.len() != size || model.p.len() != square || model.t.len() != square || model.v.len() != square || model.pn.len() != square {
            return Err(call.error("invalid argument type"));
        }
        Ok(model)
    }

    /// The prediction step: `a = T a`, and `Pn = T P T' + V` unless `keep_prior`.
    fn predict(&mut self, keep_prior: bool) {
        let n = self.size;
        let mut next = vec![0.0; n];
        for i in 0..n {
            next[i] = (0..n).map(|k| self.t[i + n * k] * self.a[k]).sum();
        }
        self.a = next;
        if !keep_prior {
            let mut product = vec![0.0; n * n];
            for i in 0..n {
                for j in 0..n {
                    product[i + n * j] = (0..n).map(|k| self.t[i + n * k] * self.p[k + n * j]).sum();
                }
            }
            for i in 0..n {
                for j in 0..n {
                    let value: f64 = (0..n).map(|k| product[i + n * k] * self.t[j + n * k]).sum();
                    self.pn[i + n * j] = self.v[i + n * j] + value;
                }
            }
        }
    }

    /// The update step for observation `y`, from the predicted `a` and `Pn`: returns the
    /// innovation and its variance.
    fn update(&mut self, y: f64) -> (f64, f64) {
        let n = self.size;
        let innovation = y - (0..n).map(|i| self.z[i] * self.a[i]).sum::<f64>();
        let gain_vector: Vec<f64> = (0..n).map(|i| (0..n).map(|k| self.pn[i + n * k] * self.z[k]).sum()).collect();
        let variance = self.h + (0..n).map(|i| self.z[i] * gain_vector[i]).sum::<f64>();
        for i in 0..n {
            self.a[i] += gain_vector[i] * innovation / variance;
        }
        for i in 0..n {
            for j in 0..n {
                self.p[i + n * j] = self.pn[i + n * j] - gain_vector[i] * gain_vector[j] / variance;
            }
        }
        (innovation, variance)
    }

    /// Stores the state back into the R list's vectors, in place, as GNU R does.
    unsafe fn write_back(&self, call: &NativeCall<'_>, model: Sexp) -> R<()> {
        values::reals_mut(component(call, model, "a")?).copy_from_slice(&self.a);
        values::reals_mut(component(call, model, "P")?).copy_from_slice(&self.p);
        values::reals_mut(component(call, model, "Pn")?).copy_from_slice(&self.pn);
        Ok(())
    }
}

/// `ARIMA_Like(y, mod, up, residuals)`: the Kalman filter of an ARIMA model, returning
/// `c(sum of squared standardized innovations, sum of log variances, count)` (and the
/// standardized residuals). Observations whose innovation variance is at least 1e4 (those
/// dominated by the diffuse prior of the differenced states) are left out of the sums. The
/// filtered state is left in `mod`.
pub unsafe fn arima_like(call: &NativeCall<'_>) -> R<Sexp> {
    let y = real_values(call.arg(0))?;
    let model_list = call.arg(1);
    let use_prior_until = values::as_integer_scalar(call.arg(2));
    let give_residuals = values::as_logical_scalar(call.arg(3)) == 1;
    let mut model = Model::read(call, model_list)?;
    model.h = 0.0;
    // `arima` updates `phi` and `theta` without rebuilding `T` and `V`, so the filter works
    // from the coefficients, exploiting the companion structure. With differencing the
    // diffuse prior makes `P - M M' / gain` cancel badly, so the sums are accumulated term by
    // term in the order of the structure, which is what reproduces GNU R's digits.
    let phi = values::reals(component(call, model_list, "phi")?).to_vec();
    let theta = values::reals(component(call, model_list, "theta")?).to_vec();
    let delta = values::reals(component(call, model_list, "Delta")?).to_vec();
    let rd = model.size;
    let d = delta.len();
    let r = rd - d;
    let (p, q) = (phi.len(), theta.len());
    let (mut squares, mut log_sum, mut count) = (0.0, 0.0, 0usize);
    let mut residuals = vec![NA_REAL; if give_residuals { y.len() } else { 0 }];
    let mut anew = vec![0.0; rd];
    let mut mm = vec![0.0; rd * rd];
    let mut m = vec![0.0; rd];
    let (a, big_p, pnew) = (&mut model.a, &mut model.p, &mut model.pn);
    for (l, &observation) in y.iter().enumerate() {
        for i in 0..r {
            let mut value = if i + 1 < r { a[i + 1] } else { 0.0 };
            if i < p {
                value += phi[i] * a[0];
            }
            anew[i] = value;
        }
        if d > 0 {
            for i in r + 1..rd {
                anew[i] = a[i - 1];
            }
            let mut value = a[0];
            for i in 0..d {
                value += delta[i] * a[r + i];
            }
            anew[r] = value;
        }
        if l as i64 > use_prior_until as i64 {
            if d == 0 {
                for i in 0..r {
                    let vi = if i == 0 { 1.0 } else if i - 1 < q { theta[i - 1] } else { 0.0 };
                    for j in 0..r {
                        let mut value = if j == 0 { vi } else if j - 1 < q { vi * theta[j - 1] } else { 0.0 };
                        if i < p && j < p {
                            value += phi[i] * phi[j] * big_p[0];
                        }
                        if i + 1 < r && j + 1 < r {
                            value += big_p[i + 1 + r * (j + 1)];
                        }
                        if i < p && j + 1 < r {
                            value += phi[i] * big_p[j + 1];
                        }
                        if j < p && i + 1 < r {
                            value += phi[j] * big_p[i + 1];
                        }
                        pnew[i + r * j] = value;
                    }
                }
            } else {
                for i in 0..r {
                    for j in 0..rd {
                        let mut value = 0.0;
                        if i < p {
                            value += phi[i] * big_p[rd * j];
                        }
                        if i + 1 < r {
                            value += big_p[i + 1 + rd * j];
                        }
                        mm[i + rd * j] = value;
                    }
                }
                for j in 0..rd {
                    let mut value = big_p[rd * j];
                    for k in 0..d {
                        value += delta[k] * big_p[r + k + rd * j];
                    }
                    mm[r + rd * j] = value;
                }
                for i in 1..d {
                    for j in 0..rd {
                        mm[r + i + rd * j] = big_p[r + i - 1 + rd * j];
                    }
                }
                for i in 0..r {
                    for j in 0..rd {
                        let mut value = 0.0;
                        if i < p {
                            value += phi[i] * mm[j];
                        }
                        if i + 1 < r {
                            value += mm[rd * (i + 1) + j];
                        }
                        pnew[j + rd * i] = value;
                    }
                }
                for j in 0..rd {
                    let mut value = mm[j];
                    for k in 0..d {
                        value += delta[k] * mm[rd * (r + k) + j];
                    }
                    pnew[j + rd * r] = value;
                }
                for i in 1..d {
                    for j in 0..rd {
                        pnew[j + rd * (r + i)] = mm[rd * (r + i - 1) + j];
                    }
                }
                for i in 0..=q {
                    let vi = if i == 0 { 1.0 } else { theta[i - 1] };
                    for j in 0..=q {
                        pnew[i + rd * j] += vi * if j == 0 { 1.0 } else { theta[j - 1] };
                    }
                }
            }
        }
        if observation.is_nan() {
            a.copy_from_slice(&anew);
            big_p.copy_from_slice(pnew);
            continue;
        }
        let mut innovation = observation - anew[0];
        for i in 0..d {
            innovation -= delta[i] * anew[r + i];
        }
        let mut variance = pnew[0];
        for i in 0..d {
            variance += delta[i] * pnew[r + i];
        }
        for i in 0..rd {
            let mut value = pnew[i];
            for k in 0..d {
                value += pnew[i + rd * (r + k)] * delta[k];
            }
            m[i] = value;
        }
        for i in 0..d {
            variance += delta[i] * m[r + i];
        }
        if variance < 1e4 {
            count += 1;
            squares += innovation * innovation / variance;
            log_sum += variance.ln();
        }
        if give_residuals {
            residuals[l] = innovation / variance.sqrt();
        }
        for i in 0..rd {
            a[i] = anew[i] + m[i] * innovation / variance;
        }
        for i in 0..rd {
            for j in 0..rd {
                big_p[i + rd * j] = pnew[i + rd * j] - m[i] * m[j] / variance;
            }
        }
    }
    model.write_back(call, model_list)?;
    let statistics = values::real_vector(&[squares, log_sum, count as f64])?;
    if give_residuals {
        values::list_vector(&[statistics, values::real_vector(&residuals)?])
    } else {
        Ok(statistics)
    }
}

/// `KalmanLike(y, mod, nit, op, update)`: the filter of a general state space model, as
/// `c(s2, mean log variance)`, or with `op` a list of those values, the standardized
/// residuals and the filtered states. With `update` the filtered model is attached.
pub unsafe fn kalman_like(call: &NativeCall<'_>) -> R<Sexp> {
    let y = real_values(call.arg(0))?;
    let model_list = values::duplicate(call.arg(1))?;
    let use_prior_until = values::as_integer_scalar(call.arg(2));
    let full = values::as_logical_scalar(call.arg(3)) == 1;
    let update = values::as_logical_scalar(call.arg(4)) == 1;
    let mut model = Model::read(call, model_list)?;
    let n = y.len();
    let size = model.size;
    let (mut squares, mut log_sum, mut count) = (0.0, 0.0, 0usize);
    let mut residuals = vec![NA_REAL; n];
    let mut states = vec![0.0; n * size];
    for (l, &observation) in y.iter().enumerate() {
        model.predict(l as i64 <= use_prior_until as i64);
        if observation.is_nan() {
            model.p.copy_from_slice(&model.pn);
        } else {
            let (innovation, variance) = model.update(observation);
            count += 1;
            squares += innovation * innovation / variance;
            log_sum += variance.ln();
            residuals[l] = innovation / variance.sqrt();
        }
        for i in 0..size {
            states[l + n * i] = model.a[i];
        }
    }
    let values_vector = values::real_vector(&[squares / count as f64, log_sum / count as f64])?;
    let result = if full {
        values::named_list(&[
            ("values", values_vector),
            ("resid", values::real_vector(&residuals)?),
            ("states", real_matrix(&states, n, size)?),
        ])?
    } else {
        values_vector
    };
    if update {
        model.write_back(call, model_list)?;
        values::set_attr(result, values::install("mod")?, model_list)?;
    }
    Ok(result)
}

/// `KalmanFore(n.ahead, mod, update)`: forecasts and their variances.
pub unsafe fn kalman_fore(call: &NativeCall<'_>) -> R<Sexp> {
    let steps = values::as_integer_scalar(call.arg(0)).max(0) as usize;
    let model_list = values::duplicate(call.arg(1))?;
    let update = values::as_logical_scalar(call.arg(2)) == 1;
    let mut model = Model::read(call, model_list)?;
    let size = model.size;
    let mut forecasts = Vec::with_capacity(steps);
    let mut variances = Vec::with_capacity(steps);
    for _ in 0..steps {
        model.predict(false);
        model.p.copy_from_slice(&model.pn);
        forecasts.push((0..size).map(|i| model.z[i] * model.a[i]).sum::<f64>());
        let mut variance = model.h;
        for i in 0..size {
            for j in 0..size {
                variance += model.z[i] * model.z[j] * model.p[i + size * j];
            }
        }
        variances.push(variance);
    }
    let result = values::named_list(&[("pred", values::real_vector(&forecasts)?), ("var", values::real_vector(&variances)?)])?;
    if update {
        model.write_back(call, model_list)?;
        values::set_attr(result, values::install("mod")?, model_list)?;
    }
    Ok(result)
}

/// `KalmanSmooth(y, mod, nit)`: the fixed-interval smoothed states and their variances, by
/// the filter and de Jong's backward recursion; a missing observation contributes nothing.
pub unsafe fn kalman_smooth(call: &NativeCall<'_>) -> R<Sexp> {
    let y = real_values(call.arg(0))?;
    let model_list = values::duplicate(call.arg(1))?;
    let use_prior_until = values::as_integer_scalar(call.arg(2));
    let mut model = Model::read(call, model_list)?;
    let n = y.len();
    let size = model.size;
    let square = size * size;
    let mut predicted_states = vec![0.0; n * size];
    let mut predicted_variances = vec![0.0; n * square];
    let mut innovations = vec![NA_REAL; n];
    let mut variances = vec![NA_REAL; n];
    for (l, &observation) in y.iter().enumerate() {
        model.predict(l as i64 <= use_prior_until as i64);
        predicted_states[l * size..(l + 1) * size].copy_from_slice(&model.a);
        predicted_variances[l * square..(l + 1) * square].copy_from_slice(&model.pn);
        if observation.is_nan() {
            model.p.copy_from_slice(&model.pn);
        } else {
            let (innovation, variance) = model.update(observation);
            innovations[l] = innovation;
            variances[l] = variance;
        }
    }
    let t = &model.t;
    let z = &model.z;
    let mut r = vec![0.0; size];
    let mut big_n = vec![0.0; square];
    let mut smooth = vec![0.0; n * size];
    let mut smooth_variance = vec![0.0; n * square];
    for l in (0..n).rev() {
        let a = &predicted_states[l * size..(l + 1) * size];
        let p = &predicted_variances[l * square..(l + 1) * square];
        // L = T - T K Z' with K = P Z / F, applied as r_{t-1} = Z v / F + L' r_t.
        let (next_r, next_n) = if innovations[l].is_nan() {
            let mut next_r = vec![0.0; size];
            for i in 0..size {
                next_r[i] = (0..size).map(|k| t[k + size * i] * r[k]).sum();
            }
            let mut next_n = vec![0.0; square];
            for i in 0..size {
                for j in 0..size {
                    let mut value = 0.0;
                    for k in 0..size {
                        for m in 0..size {
                            value += t[k + size * i] * big_n[k + size * m] * t[m + size * j];
                        }
                    }
                    next_n[i + size * j] = value;
                }
            }
            (next_r, next_n)
        } else {
            let variance = variances[l];
            let gain: Vec<f64> = (0..size).map(|i| (0..size).map(|k| p[i + size * k] * z[k]).sum::<f64>() / variance).collect();
            let mut l_matrix = vec![0.0; square];
            for i in 0..size {
                for j in 0..size {
                    let tk: f64 = (0..size).map(|k| t[i + size * k] * gain[k]).sum();
                    l_matrix[i + size * j] = t[i + size * j] - tk * z[j];
                }
            }
            let mut next_r = vec![0.0; size];
            for i in 0..size {
                next_r[i] = z[i] * innovations[l] / variance + (0..size).map(|k| l_matrix[k + size * i] * r[k]).sum::<f64>();
            }
            let mut next_n = vec![0.0; square];
            for i in 0..size {
                for j in 0..size {
                    let mut value = z[i] * z[j] / variance;
                    for k in 0..size {
                        for m in 0..size {
                            value += l_matrix[k + size * i] * big_n[k + size * m] * l_matrix[m + size * j];
                        }
                    }
                    next_n[i + size * j] = value;
                }
            }
            (next_r, next_n)
        };
        r = next_r;
        big_n = next_n;
        for i in 0..size {
            smooth[l + n * i] = a[i] + (0..size).map(|k| p[i + size * k] * r[k]).sum::<f64>();
        }
        for i in 0..size {
            for j in 0..size {
                let mut value = p[i + size * j];
                for k in 0..size {
                    for m in 0..size {
                        value -= p[i + size * k] * big_n[k + size * m] * p[m + size * j];
                    }
                }
                smooth_variance[l + n * (i + size * j)] = value;
            }
        }
    }
    let variance = values::real_vector(&smooth_variance)?;
    let dim = values::integer_vector(&[n as i32, square as i32])?;
    values::set_attr(variance, symbols().dim, dim)?;
    values::named_list(&[("smooth", real_matrix(&smooth, n, size)?), ("var", variance)])
}
