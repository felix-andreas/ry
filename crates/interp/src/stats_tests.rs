//! The null distributions behind stats' classical tests: Shapiro and Wilk's W, Kolmogorov and
//! Smirnov's statistics, the Ansari-Bradley, Kendall and Spearman rank statistics, and the
//! exact conditional distribution of a stratified 2 x 2 table.
//!
//! Normal quantiles, normal tail areas and hypergeometric densities come from stats' own R
//! functions, so they agree with every other use of them in a session.

use crate::{
    builtin_libraries::NativeCall,
    eval,
    stats::real_values,
    unwind::R,
    values,
};
use heap::object::{NA_REAL, Sexp};

/// Calls the stats function `name` (as the calling R code sees it) on numeric arguments and
/// returns its value as doubles.
unsafe fn stats_function(call: &NativeCall<'_>, name: &str, arguments: &[&[f64]]) -> R<Vec<f64>> {
    let mut items = Vec::with_capacity(arguments.len());
    for argument in arguments {
        items.push(values::real_vector(argument)?);
    }
    let expression = values::call(values::install(name)?, &items)?;
    real_values(eval::eval(expression, call.env)?)
}

/// `SWilk(x)`: Shapiro and Wilk's W for the sorted sample `x` and its p-value, by Royston's
/// approximations (Applied Statistics algorithm AS R94, 1995), as `c(W, p)`.
pub unsafe fn swilk(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let n = x.len();
    let half = n / 2;
    let size = n as f64;
    let mut a = vec![0.0; half + 1];
    let polynomial = |coefficients: &[f64], value: f64| -> f64 {
        let mut result = coefficients[0];
        if coefficients.len() > 1 {
            let mut p = value * coefficients[coefficients.len() - 1];
            for &coefficient in coefficients[1..coefficients.len() - 1].iter().rev() {
                p = (p + coefficient) * value;
            }
            result += p;
        }
        result
    };
    const C1: [f64; 6] = [0.0, 0.221157, -0.147981, -2.07119, 4.434685, -2.706056];
    const C2: [f64; 6] = [0.0, 0.042981, -0.293762, -1.752461, 5.682633, -3.582633];
    const C3: [f64; 4] = [0.544, -0.39978, 0.025054, -6.714e-4];
    const C4: [f64; 4] = [1.3822, -0.77857, 0.062767, -0.0020322];
    const C5: [f64; 4] = [-1.5861, -0.31082, -0.083751, 0.0038915];
    const C6: [f64; 3] = [-0.4803, -0.082676, 0.0030302];
    const G: [f64; 2] = [-2.273, 0.459];
    const SMALL: f64 = 1e-19;
    if n == 3 {
        a[1] = 0.5f64.sqrt();
    } else {
        let offset = size + 0.25;
        let probabilities: Vec<f64> = (1..=half).map(|i| (i as f64 - 0.375) / offset).collect();
        let quantiles = stats_function(call, "qnorm", &[&probabilities])?;
        let mut sum_squares = 0.0;
        for i in 1..=half {
            a[i] = quantiles[i - 1];
            sum_squares += a[i] * a[i];
        }
        sum_squares *= 2.0;
        let root = sum_squares.sqrt();
        let reciprocal = 1.0 / size.sqrt();
        let a1 = polynomial(&C1, reciprocal) - a[1] / root;
        let (first, factor) = if n > 5 {
            let a2 = -a[2] / root + polynomial(&C2, reciprocal);
            let factor = ((sum_squares - 2.0 * (a[1] * a[1]) - 2.0 * (a[2] * a[2])) / (1.0 - 2.0 * (a1 * a1) - 2.0 * (a2 * a2))).sqrt();
            a[2] = a2;
            (3, factor)
        } else {
            (2, ((sum_squares - 2.0 * (a[1] * a[1])) / (1.0 - 2.0 * (a1 * a1))).sqrt())
        };
        a[1] = a1;
        for coefficient in a.iter_mut().take(half + 1).skip(first) {
            *coefficient /= -factor;
        }
    }
    let range = x[n - 1] - x[0];
    if range < SMALL {
        return Err(call.error("ifault=6. This should not happen"));
    }
    if n > 5000 {
        return Err(call.error("ifault=2. This should not happen"));
    }
    let mut sum_x = x[0] / range;
    let mut sum_a = -a[1];
    let sign = |value: i64| (value > 0) as i64 as f64 - (value < 0) as i64 as f64;
    let mut i = 1usize;
    let mut j = n - 1;
    while i < n {
        sum_x += x[i] / range;
        i += 1;
        if i != j {
            sum_a += sign(i as i64 - j as i64) * a[i.min(j)];
        }
        j -= 1;
    }
    sum_a /= size;
    sum_x /= size;
    let (mut ssa, mut ssx, mut sax) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let j = n - 1 - i;
        let asa = if i != j { sign(i as i64 - j as i64) * a[1 + i.min(j)] - sum_a } else { -sum_a };
        let xsx = x[i] / range - sum_x;
        ssa += asa * asa;
        ssx += xsx * xsx;
        sax += asa * xsx;
    }
    let root = (ssa * ssx).sqrt();
    let w1 = (root - sax) * (root + sax) / (ssa * ssx);
    let w = 1.0 - w1;
    let p = if n == 3 {
        let six_over_pi = 1.909_859_317_102_74;
        let third_pi = 1.047_197_551_196_6;
        (six_over_pi * (w.sqrt().asin() - third_pi)).max(0.0)
    } else {
        let mut y = w1.ln();
        let log_n = size.ln();
        let (mean, deviation) = if n <= 11 {
            let gamma = polynomial(&G, size);
            if y >= gamma {
                return values::real_vector(&[w, 1e-99]);
            }
            y = -(gamma - y).ln();
            (polynomial(&C3, size), polynomial(&C4, size).exp())
        } else {
            (polynomial(&C5, log_n), polynomial(&C6, log_n).exp())
        };
        stats_function(call, "pnorm", &[&[y], &[mean], &[deviation], &[0.0]])?[0]
    };
    values::real_vector(&[w, p])
}

/// `pKolmogorov2x(statistic, n)`: the exact distribution function of the one-sample
/// two-sided Kolmogorov statistic, by Marsaglia, Tsang and Wang's matrix power ("Evaluating
/// Kolmogorov's distribution", Journal of Statistical Software, 2003).
pub unsafe fn p_kolmogorov_2x(call: &NativeCall<'_>) -> R<Sexp> {
    let d = values::as_real_scalar(call.arg(0));
    let n = values::as_integer_scalar(call.arg(1)).max(1) as usize;
    let k = (n as f64 * d) as usize + 1;
    let m = 2 * k - 1;
    let h = k as f64 - n as f64 * d;
    let mut matrix = vec![0.0; m * m];
    for i in 0..m {
        for j in 0..m {
            matrix[i * m + j] = if i + 1 < j { 0.0 } else { 1.0 };
        }
    }
    for i in 0..m {
        matrix[i * m] -= h.powi(i as i32 + 1);
        matrix[(m - 1) * m + i] -= h.powi((m - i) as i32);
    }
    matrix[(m - 1) * m] += if 2.0 * h - 1.0 > 0.0 { (2.0 * h - 1.0).powi(m as i32) } else { 0.0 };
    for i in 0..m {
        for j in 0..m {
            if i + 1 > j {
                for g in 1..=(i + 1 - j) {
                    matrix[i * m + j] /= g as f64;
                }
            }
        }
    }
    let (power, mut exponent) = matrix_power(&matrix, 0, m, n);
    let mut s = power[(k - 1) * m + k - 1];
    for i in 1..=n {
        s = s * i as f64 / n as f64;
        if s < 1e-140 {
            s *= 1e140;
            exponent -= 140;
        }
    }
    s *= 10f64.powi(exponent);
    values::scalar_real(s)
}

fn matrix_multiply(a: &[f64], b: &[f64], m: usize) -> Vec<f64> {
    let mut c = vec![0.0; m * m];
    for i in 0..m {
        for j in 0..m {
            let mut sum = 0.0;
            for k in 0..m {
                sum += a[i * m + k] * b[k * m + j];
            }
            c[i * m + j] = sum;
        }
    }
    c
}

/// `A^n`, with a decimal exponent kept aside to avoid overflow.
fn matrix_power(a: &[f64], a_exponent: i32, m: usize, n: usize) -> (Vec<f64>, i32) {
    if n == 1 {
        return (a.to_vec(), a_exponent);
    }
    let (half, half_exponent) = matrix_power(a, a_exponent, m, n / 2);
    let square = matrix_multiply(&half, &half, m);
    let square_exponent = 2 * half_exponent;
    let (mut result, mut exponent) = if n % 2 == 0 {
        (square, square_exponent)
    } else {
        (matrix_multiply(a, &square, m), a_exponent + square_exponent)
    };
    if result[(m / 2) * m + m / 2] > 1e140 {
        for value in result.iter_mut() {
            *value *= 1e-140;
        }
        exponent += 140;
    }
    (result, exponent)
}

/// `pKS2(statistic, tol)`: Kolmogorov's limiting distribution of `sqrt(n) D`, from the
/// alternating series (Durbin, "Distribution Theory for Tests Based on the Sample
/// Distribution Function", 1973), with its theta-function form for small arguments.
pub unsafe fn p_ks2(call: &NativeCall<'_>) -> R<Sexp> {
    let mut x = real_values(call.arg(0))?;
    let tolerance = values::as_real_scalar(call.arg(1));
    let k_max = (2.0 - tolerance.ln()).sqrt() as i64;
    for value in x.iter_mut() {
        if *value < 1.0 {
            let z = -(std::f64::consts::FRAC_PI_2 * std::f64::consts::FRAC_PI_4) / (*value * *value);
            let w = value.ln();
            let mut sum = 0.0;
            let mut k = 1;
            while k < k_max {
                sum += ((k * k) as f64 * z - w).exp();
                k += 2;
            }
            *value = sum / (1.0 / (2.0 * std::f64::consts::PI).sqrt());
        } else {
            let z = -2.0 * *value * *value;
            let mut sign = -1.0;
            let mut k = 1i64;
            let mut old = 0.0_f64;
            let mut new = 1.0;
            while (old - new).abs() > tolerance {
                old = new;
                new += 2.0 * sign * (z * (k * k) as f64).exp();
                sign = -sign;
                k += 1;
            }
            *value = new;
        }
    }
    values::real_vector(&x)
}

/// `psmirnov_exact(q, m, n, z, two.sided, lower.tail)`: the exact distribution of the
/// two-sample Smirnov statistic, by counting the lattice paths of the merged sample that stay
/// within the band; with ties (`z`) the band is only checked where the pooled values change.
pub unsafe fn psmirnov_exact(call: &NativeCall<'_>) -> R<Sexp> {
    let q = real_values(call.arg(0))?;
    let m = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let n = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let ties = call.arg(3);
    let checks: Option<Vec<bool>> = (ties != values::nil()).then(|| {
        crate::stats::integer_values(ties).map(|entries| entries.iter().map(|&entry| entry != 0).collect())
    }).transpose()?;
    let two_sided = values::as_logical_scalar(call.arg(4)) == 1;
    let lower_tail = values::as_logical_scalar(call.arg(5)) == 1;
    let (md, nd) = (m as f64, n as f64);
    let mut result = Vec::with_capacity(q.len());
    for &statistic in &q {
        // The statistic is a multiple of 1 / lcm(m, n); compare on the integer scale with a
        // little slack so that rounding in `statistic` does not move a path across the band.
        let bound = statistic * md * nd - 1e-7;
        let check = |i: usize, j: usize| checks.as_ref().is_none_or(|flags| flags.get(i + j).copied().unwrap_or(true));
        let outside = |i: usize, j: usize| -> bool {
            let difference = i as f64 * nd - j as f64 * md;
            let difference = if two_sided { difference.abs() } else { difference };
            check(i, j) && difference >= bound
        };
        // `u[j]` is the probability that a random path reaches (i, j) inside the band.
        let mut u = vec![0.0; n + 1];
        u[0] = 1.0;
        for j in 1..=n {
            u[j] = if outside(0, j) { 0.0 } else { u[j - 1] };
        }
        for i in 1..=m {
            let w = i as f64 / (i + n) as f64;
            u[0] = if outside(i, 0) { 0.0 } else { w * u[0] };
            for j in 1..=n {
                u[j] = if outside(i, j) { 0.0 } else { w * u[j] + u[j - 1] };
            }
        }
        let below = u[n];
        result.push(if lower_tail { below } else { 1.0 - below });
    }
    values::real_vector(&result)
}

/// The number of arrangements of `m` and `n` with Ansari-Bradley statistic `k`, memoized.
struct Ansari {
    counts: Vec<Vec<Option<Vec<f64>>>>,
}

impl Ansari {
    fn new(m: usize, n: usize) -> Ansari {
        Ansari { counts: vec![vec![None; n + 1]; m + 1] }
    }

    fn count(&mut self, k: i64, m: usize, n: usize) -> f64 {
        let low = ((m + 1) * (m + 1) / 4) as i64;
        let high = low + (m * n / 2) as i64;
        if k < low || k > high {
            return 0.0;
        }
        if self.counts[m][n].is_none() {
            self.counts[m][n] = Some(vec![-1.0; high as usize + 1]);
        }
        let cached = self.counts[m][n].as_ref().map_or(-1.0, |table| table[k as usize]);
        if cached >= 0.0 {
            return cached;
        }
        let value = if m == 0 {
            (k == 0) as i32 as f64
        } else if n == 0 {
            (k == low) as i32 as f64
        } else {
            self.count(k, m, n - 1) + self.count(k - ((m + n + 1) / 2) as i64, m - 1, n)
        };
        if let Some(table) = self.counts[m][n].as_mut() {
            table[k as usize] = value;
        }
        value
    }
}

fn choose(n: usize, k: usize) -> f64 {
    let k = k.min(n - k);
    let mut result = 1.0;
    for i in 0..k {
        result = result * (n - i) as f64 / (i + 1) as f64;
    }
    result.round()
}

/// `pAnsari(q, m, n)`: the exact distribution function of the Ansari-Bradley statistic.
pub unsafe fn p_ansari(call: &NativeCall<'_>) -> R<Sexp> {
    let q = real_values(call.arg(0))?;
    let m = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let n = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let mut table = Ansari::new(m, n);
    let low = ((m + 1) * (m + 1) / 4) as i64;
    let high = low + (m * n / 2) as i64;
    let total = choose(m + n, m);
    let result: Vec<f64> = q
        .iter()
        .map(|&value| {
            let x = (value + 1e-7).floor();
            if x < low as f64 {
                0.0
            } else if x > high as f64 {
                1.0
            } else {
                (low..=x as i64).map(|k| table.count(k, m, n)).sum::<f64>() / total
            }
        })
        .collect();
    values::real_vector(&result)
}

/// `qAnsari(p, m, n)`: the quantiles of the Ansari-Bradley statistic.
pub unsafe fn q_ansari(call: &NativeCall<'_>) -> R<Sexp> {
    let p = real_values(call.arg(0))?;
    let m = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let n = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let mut table = Ansari::new(m, n);
    let low = ((m + 1) * (m + 1) / 4) as i64;
    let high = low + (m * n / 2) as i64;
    let total = choose(m + n, m);
    let mut result = Vec::with_capacity(p.len());
    for &probability in &p {
        if !(0.0..=1.0).contains(&probability) {
            return Err(call.error("probabilities outside [0,1] in qansari()"));
        }
        if probability == 0.0 {
            result.push(low as f64);
        } else if probability == 1.0 {
            result.push(high as f64);
        } else {
            let mut cumulative = 0.0;
            let mut q = 0i64;
            loop {
                cumulative += table.count(q, m, n) / total;
                if cumulative >= probability {
                    break;
                }
                q += 1;
            }
            result.push(q as f64);
        }
    }
    values::real_vector(&result)
}

/// `pKendall(q, n)`: the exact distribution function of Kendall's statistic (the number of
/// concordant pairs) for `n` observations without ties.
pub unsafe fn p_kendall(call: &NativeCall<'_>) -> R<Sexp> {
    let q = real_values(call.arg(0))?;
    let n = values::as_integer_scalar(call.arg(1)).max(1) as usize;
    let mut tables: Vec<Option<Vec<f64>>> = vec![None; n + 1];
    fn count(k: i64, n: usize, tables: &mut [Option<Vec<f64>>]) -> f64 {
        let high = (n * (n - 1) / 2) as i64;
        if k < 0 || k > high {
            return 0.0;
        }
        if tables[n].is_none() {
            tables[n] = Some(vec![-1.0; high as usize + 1]);
        }
        let cached = tables[n].as_ref().map_or(-1.0, |table| table[k as usize]);
        if cached >= 0.0 {
            return cached;
        }
        let value = if n == 1 {
            (k == 0) as i32 as f64
        } else {
            (0..n as i64).map(|i| count(k - i, n - 1, tables)).sum()
        };
        if let Some(table) = tables[n].as_mut() {
            table[k as usize] = value;
        }
        value
    }
    let factorial: f64 = (1..=n).map(|i| i as f64).product();
    let high = (n * (n - 1) / 2) as f64;
    let result: Vec<f64> = q
        .iter()
        .map(|&value| {
            let q = (value + 1e-7).floor();
            if q < 0.0 {
                0.0
            } else if q > high {
                1.0
            } else {
                (0..=q as i64).map(|k| count(k, n, &mut tables)).sum::<f64>() / factorial
            }
        })
        .collect();
    values::real_vector(&result)
}

/// `pRho(q, n, lower.tail)`: the distribution of Spearman's S, the sum of squared rank
/// differences, exactly by enumerating permutations for small `n` and otherwise by Best and
/// Roberts' Edgeworth series (Applied Statistics algorithm AS 89, 1975).
pub unsafe fn p_rho(call: &NativeCall<'_>) -> R<Sexp> {
    let q = real_values(call.arg(0))?;
    let n = values::as_integer_scalar(call.arg(1));
    let lower_tail = values::as_logical_scalar(call.arg(2)) == 1;
    let mut result = Vec::with_capacity(q.len());
    for &statistic in &q {
        result.push(prho(call, n, statistic, lower_tail)?);
    }
    values::real_vector(&result)
}

/// `P(S >= s)` (or `P(S < s)` for the lower tail).
unsafe fn prho(call: &NativeCall<'_>, n: i32, s: f64, lower_tail: bool) -> R<f64> {
    if n <= 1 {
        return Ok(if lower_tail { 0.0 } else { 1.0 });
    }
    if s <= 0.0 {
        return Ok(if lower_tail { 0.0 } else { 1.0 });
    }
    let size = n as f64;
    let maximum = size * (size * size - 1.0) / 3.0;
    if s > maximum {
        return Ok(if lower_tail { 1.0 } else { 0.0 });
    }
    if n <= 9 {
        let n = n as usize;
        let mut permutation: Vec<usize> = (1..=n).collect();
        let mut total = 0u64;
        let mut at_least = 0u64;
        loop {
            total += 1;
            let sum: f64 = permutation.iter().enumerate().map(|(i, &p)| {
                let difference = (i + 1) as f64 - p as f64;
                difference * difference
            }).sum();
            if s <= sum {
                at_least += 1;
            }
            if !next_permutation(&mut permutation) {
                break;
            }
        }
        let count = if lower_tail { total - at_least } else { at_least };
        return Ok(count as f64 / total as f64);
    }
    const C: [f64; 12] = [0.2274, 0.2531, 0.1745, 0.0758, 0.1033, 0.3932, 0.0879, 0.0151, 0.0072, 0.0831, 0.0131, 4.6e-4];
    let b = 1.0 / size;
    let x = (6.0 * (s - 1.0) * b / (size * size - 1.0) - 1.0) * (1.0 / b - 1.0).sqrt();
    let mut y = x * x;
    let u = x
        * b
        * (C[0] + b * (C[1] + C[2] * b)
            + y * (-C[3] + b * (C[4] + C[5] * b) - y * b * (C[6] + C[7] * b - y * (C[8] - C[9] * b + y * b * (C[10] - C[11] * y)))));
    y = u / (y / 2.0).exp();
    let tail = stats_function(call, "pnorm", &[&[x], &[0.0], &[1.0], &[if lower_tail { 1.0 } else { 0.0 }]])?[0];
    let p = if lower_tail { tail - y } else { y + tail };
    Ok(p.clamp(0.0, 1.0))
}

fn next_permutation(values: &mut [usize]) -> bool {
    let n = values.len();
    if n < 2 {
        return false;
    }
    let mut i = n - 1;
    while i > 0 && values[i - 1] >= values[i] {
        i -= 1;
    }
    if i == 0 {
        return false;
    }
    let mut j = n - 1;
    while values[j] <= values[i - 1] {
        j -= 1;
    }
    values.swap(i - 1, j);
    values[i..].reverse();
    true
}

/// `d2x2xk(K, m, n, t, srn)`: the exact distribution of the total of the first cells of `K`
/// independent 2 x 2 tables with the given margins, by convolving their hypergeometric
/// densities.
pub unsafe fn d2x2xk(call: &NativeCall<'_>) -> R<Sexp> {
    let k = values::as_integer_scalar(call.arg(0)).max(0) as usize;
    let m = real_values(call.arg(1))?;
    let n = real_values(call.arg(2))?;
    let t = real_values(call.arg(3))?;
    let size = values::as_integer_scalar(call.arg(4)).max(0) as usize;
    let mut density = vec![1.0];
    for stratum in 0..k {
        let low = (t[stratum] - n[stratum]).max(0.0) as i64;
        let high = m[stratum].min(t[stratum]) as i64;
        let support: Vec<f64> = (low..=high).map(|value| value as f64).collect();
        let weights = stats_function(call, "dhyper", &[&support, &[m[stratum]], &[n[stratum]], &[t[stratum]]])?;
        let mut next = vec![0.0; density.len() + support.len() - 1];
        for (j, &weight) in weights.iter().enumerate() {
            for (w, &value) in density.iter().enumerate() {
                next[w + j] += value * weight;
            }
        }
        density = next;
    }
    let total: f64 = density.iter().sum();
    let mut result: Vec<f64> = density.iter().map(|value| value / total).collect();
    result.resize(size, NA_REAL);
    values::real_vector(&result)
}
