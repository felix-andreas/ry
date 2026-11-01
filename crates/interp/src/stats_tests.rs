//! The null distributions behind stats' classical tests: Shapiro and Wilk's W, Kolmogorov and
//! Smirnov's statistics, the Ansari-Bradley, Kendall and Spearman rank statistics, and the
//! exact conditional distribution of a stratified 2 x 2 table, and the random two-way tables
//! behind simulated p-values.
//!
//! Normal quantiles, normal tail areas and hypergeometric densities come from stats' own R
//! functions, so they agree with every other use of them in a session.

use {
    crate::{
        builtin_libraries::NativeCall,
        eval, random,
        stats::{integer_values, real_values},
        unwind::R,
        values,
    },
    heap::object::{NA_REAL, Sexp, SexpType},
};

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
            let factor = ((sum_squares - 2.0 * (a[1] * a[1]) - 2.0 * (a[2] * a[2]))
                / (1.0 - 2.0 * (a1 * a1) - 2.0 * (a2 * a2)))
                .sqrt();
            a[2] = a2;
            (3, factor)
        } else {
            (
                2,
                ((sum_squares - 2.0 * (a[1] * a[1])) / (1.0 - 2.0 * (a1 * a1))).sqrt(),
            )
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
        let asa = if i != j {
            sign(i as i64 - j as i64) * a[1 + i.min(j)] - sum_a
        } else {
            -sum_a
        };
        let xsx = x[i] / range - sum_x;
        ssa += asa * asa;
        ssx += xsx * xsx;
        sax += asa * xsx;
    }
    let root = (ssa * ssx).sqrt();
    let w1 = (root - sax) * (root + sax) / (ssa * ssx);
    let w = 1.0 - w1;
    let p = if n == 3 {
        let six_over_pi = 6.0 / std::f64::consts::PI;
        (six_over_pi * (w.sqrt().asin() - std::f64::consts::FRAC_PI_3)).max(0.0)
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
    values::scalar_real(kolmogorov_exact(d, n))
}

/// R 4.4's `pkolmogorov_two_exact(q, n)`: [`p_kolmogorov_2x`] over a vector of statistics.
pub unsafe fn p_kolmogorov_two_exact(call: &NativeCall<'_>) -> R<Sexp> {
    let statistics = real_values(call.arg(0))?;
    let n = values::as_integer_scalar(call.arg(1)).max(1) as usize;
    let values: Vec<f64> = statistics.iter().map(|&d| kolmogorov_exact(d, n)).collect();
    values::real_vector(&values)
}

/// `P(D_n < d)` for the one-sample Kolmogorov statistic, by Marsaglia, Tsang and Wang's matrix
/// power ("Evaluating Kolmogorov's Distribution", 2003).
fn kolmogorov_exact(d: f64, n: usize) -> f64 {
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
    matrix[(m - 1) * m] += if 2.0 * h - 1.0 > 0.0 {
        (2.0 * h - 1.0).powi(m as i32)
    } else {
        0.0
    };
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
    s * 10f64.powi(exponent)
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
    let x = real_values(call.arg(0))?;
    let tolerance = values::as_real_scalar(call.arg(1));
    let values: Vec<f64> = x
        .iter()
        .map(|&value| kolmogorov_limit(value, true, tolerance))
        .collect();
    values::real_vector(&values)
}

/// R 4.4's `pkolmogorov_two_limit(statistic, lower.tail, tol)`: [`p_ks2`], whose upper tail is
/// summed directly so that it keeps its precision far out.
pub unsafe fn p_kolmogorov_two_limit(call: &NativeCall<'_>) -> R<Sexp> {
    let x = real_values(call.arg(0))?;
    let lower = values::as_logical_scalar(call.arg(1)) != 0;
    let tolerance = values::as_real_scalar(call.arg(2));
    let values: Vec<f64> = x
        .iter()
        .map(|&value| kolmogorov_limit(value, lower, tolerance))
        .collect();
    values::real_vector(&values)
}

/// Kolmogorov's limiting distribution at `x`: the theta-function form below 1, and above it the
/// alternating series `1 - 2 sum (-1)^(k-1) exp(-2 k^2 x^2)`, whose tail alone is the upper
/// probability.
fn kolmogorov_limit(x: f64, lower: bool, tolerance: f64) -> f64 {
    if x <= 0.0 {
        return if lower { 0.0 } else { 1.0 };
    }
    if x < 1.0 {
        let k_max = (2.0 - tolerance.ln()).sqrt() as i64;
        let z = -(std::f64::consts::FRAC_PI_2 * std::f64::consts::FRAC_PI_4) / (x * x);
        let w = x.ln();
        let mut sum = 0.0;
        let mut k = 1;
        while k < k_max {
            sum += ((k * k) as f64 * z - w).exp();
            k += 2;
        }
        let p = sum / (1.0 / (2.0 * std::f64::consts::PI).sqrt());
        return if lower { p } else { 1.0 - p };
    }
    let z = -2.0 * x * x;
    let mut sign = 1.0;
    let mut k = 1i64;
    // Both sums run term by term, so the lower tail rounds exactly as R 4.3's `pKS2` did.
    let (mut below, mut tail) = (1.0_f64, 0.0_f64);
    loop {
        let term = 2.0 * sign * (z * (k * k) as f64).exp();
        below -= term;
        tail += term;
        if term.abs() <= tolerance {
            break;
        }
        sign = -sign;
        k += 1;
    }
    if lower { below } else { tail }
}

/// `psmirnov_exact(q, m, n, z, two.sided, lower.tail)`: the exact distribution of the
/// two-sample Smirnov statistic, by counting the lattice paths of the merged sample that stay
/// within the band; with ties (`z`) the band is only checked where the pooled values change.
pub unsafe fn psmirnov_exact(call: &NativeCall<'_>) -> R<Sexp> {
    let q = real_values(call.arg(0))?;
    let m = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let n = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let ties = call.arg(3);
    let checks: Option<Vec<bool>> = (ties != values::nil())
        .then(|| {
            crate::stats::integer_values(ties)
                .map(|entries| entries.iter().map(|&entry| entry != 0).collect())
        })
        .transpose()?;
    let two_sided = values::as_logical_scalar(call.arg(4)) == 1;
    let lower_tail = values::as_logical_scalar(call.arg(5)) == 1;
    let (md, nd) = (m as f64, n as f64);
    let mut result = Vec::with_capacity(q.len());
    for &statistic in &q {
        // The statistic is a multiple of 1 / lcm(m, n); compare on the integer scale with a
        // little slack so that rounding in `statistic` does not move a path across the band.
        let bound = statistic * md * nd - 1e-7;
        let check = |i: usize, j: usize| {
            checks
                .as_ref()
                .is_none_or(|flags| flags.get(i + j).copied().unwrap_or(true))
        };
        let outside = |i: usize, j: usize| -> bool {
            let difference = i as f64 * nd - j as f64 * md;
            let difference = if two_sided {
                difference.abs()
            } else {
                difference
            };
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
                u[j] = if outside(i, j) {
                    0.0
                } else {
                    w * u[j] + u[j - 1]
                };
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
        Ansari {
            counts: vec![vec![None; n + 1]; m + 1],
        }
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
        let cached = self.counts[m][n]
            .as_ref()
            .map_or(-1.0, |table| table[k as usize]);
        if cached >= 0.0 {
            return cached;
        }
        let value = if m == 0 {
            (k == 0) as i32 as f64
        } else if n == 0 {
            (k == low) as i32 as f64
        } else {
            self.count(k, m, n - 1) + self.count(k - (m + n).div_ceil(2) as i64, m - 1, n)
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
                (0..=q as i64)
                    .map(|k| count(k, n, &mut tables))
                    .sum::<f64>()
                    / factorial
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
            let sum: f64 = permutation
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    let difference = (i + 1) as f64 - p as f64;
                    difference * difference
                })
                .sum();
            if s <= sum {
                at_least += 1;
            }
            if !next_permutation(&mut permutation) {
                break;
            }
        }
        let count = if lower_tail {
            total - at_least
        } else {
            at_least
        };
        return Ok(count as f64 / total as f64);
    }
    const C: [f64; 12] = [
        0.2274, 0.2531, 0.1745, 0.0758, 0.1033, 0.3932, 0.0879, 0.0151, 0.0072, 0.0831, 0.0131,
        4.6e-4,
    ];
    let b = 1.0 / size;
    let x = (6.0 * (s - 1.0) * b / (size * size - 1.0) - 1.0) * (1.0 / b - 1.0).sqrt();
    let mut y = x * x;
    let u = x
        * b
        * (C[0]
            + b * (C[1] + C[2] * b)
            + y * (-C[3] + b * (C[4] + C[5] * b)
                - y * b * (C[6] + C[7] * b - y * (C[8] - C[9] * b + y * b * (C[10] - C[11] * y)))));
    y = u / (y / 2.0).exp();
    let tail = stats_function(
        call,
        "pnorm",
        &[&[x], &[0.0], &[1.0], &[if lower_tail { 1.0 } else { 0.0 }]],
    )?[0];
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
        let weights = stats_function(
            call,
            "dhyper",
            &[&support, &[m[stratum]], &[n[stratum]], &[t[stratum]]],
        )?;
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

/// `Fisher_sim(sr, sc, B)`: for `B` random tables with row sums `sr` and column sums `sc`, the
/// statistic `-sum(lfactorial(x))` that `fisher.test` compares against the observed table.
pub unsafe fn fisher_sim(call: &NativeCall<'_>) -> R<Sexp> {
    let margins = TableMargins::read(call, call.arg(0), call.arg(1))?;
    let replicates = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let log_factorials = log_factorials(margins.total);
    let mut statistics = Vec::with_capacity(replicates);
    random::get_rng_state()?;
    for _ in 0..replicates {
        let table = random_table(call, &margins, &log_factorials)?;
        statistics.push(
            -table
                .iter()
                .map(|&cell| log_factorials[cell as usize])
                .sum::<f64>(),
        );
    }
    random::put_rng_state()?;
    values::real_vector(&statistics)
}

/// `chisq_sim(sr, sc, B, E)`: for `B` random tables with row sums `sr` and column sums `sc`,
/// Pearson's statistic against the expected counts `E`.
pub unsafe fn chisq_sim(call: &NativeCall<'_>) -> R<Sexp> {
    let margins = TableMargins::read(call, call.arg(0), call.arg(1))?;
    let replicates = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let expected = real_values(call.arg(3))?;
    if expected.len() != margins.rows.len() * margins.columns.len() {
        return Err(call.error("'E' does not match the table's margins"));
    }
    let log_factorials = log_factorials(margins.total);
    let mut statistics = Vec::with_capacity(replicates);
    random::get_rng_state()?;
    for _ in 0..replicates {
        let table = random_table(call, &margins, &log_factorials)?;
        let statistic = table
            .iter()
            .zip(&expected)
            .map(|(&observed, &expected)| (observed as f64 - expected).powi(2) / expected)
            .sum::<f64>();
        statistics.push(statistic);
    }
    random::put_rng_state()?;
    values::real_vector(&statistics)
}

/// `r2dtable(n, r, c)`: `n` random two-way tables, as integer matrices, with row sums `r` and
/// column sums `c`.
pub unsafe fn r2dtable(call: &NativeCall<'_>) -> R<Sexp> {
    let count = values::as_integer_scalar(call.arg(0));
    if count < 0 || count == heap::object::NA_INTEGER {
        return Err(call.error("invalid argument 'n'"));
    }
    let margins = TableMargins::read(call, call.arg(1), call.arg(2))?;
    let log_factorials = log_factorials(margins.total);
    let result = values::alloc(SexpType::List, count as usize)?;
    random::get_rng_state()?;
    for index in 0..count as usize {
        let table = random_table(call, &margins, &log_factorials)?;
        let matrix = values::integer_vector(&table)?;
        let dim =
            values::integer_vector(&[margins.rows.len() as i32, margins.columns.len() as i32])?;
        values::set_attr(matrix, values::symbols().dim, dim)?;
        values::set_elt(result, index, matrix);
    }
    random::put_rng_state()?;
    Ok(result)
}

/// The fixed margins of a two-way table.
struct TableMargins {
    rows: Vec<i32>,
    columns: Vec<i32>,
    total: usize,
}

impl TableMargins {
    unsafe fn read(call: &NativeCall<'_>, rows: Sexp, columns: Sexp) -> R<Self> {
        let rows = integer_values(rows)?;
        let columns = integer_values(columns)?;
        let valid = |margin: &[i32]| margin.len() > 1 && margin.iter().all(|&count| count >= 0);
        if !valid(&rows) {
            return Err(call.error("invalid argument 'r'"));
        }
        if !valid(&columns) {
            return Err(call.error("invalid argument 'c'"));
        }
        let total: i64 = rows.iter().map(|&count| i64::from(count)).sum();
        if total != columns.iter().map(|&count| i64::from(count)).sum::<i64>() {
            return Err(call.error("arguments 'r' and 'c' must have the same sums"));
        }
        Ok(TableMargins {
            rows,
            columns,
            total: total as usize,
        })
    }
}

/// `log(k!)` for `k` in `0..=n`, summed term by term so that equal tables give equal statistics.
fn log_factorials(n: usize) -> Vec<f64> {
    let mut table = vec![0.0; n + 1];
    for k in 2..=n {
        table[k] = table[k - 1] + (k as f64).ln();
    }
    table
}

/// A random table with the given margins, drawn uniformly among all such tables by Patefield's
/// algorithm (Applied Statistics algorithm AS 159, 1981), as column-major counts.
///
/// Each cell but those in the last row and column is drawn in turn from its hypergeometric
/// distribution given the cells already fixed. The search for the drawn value starts at the
/// distribution's mode and widens one step up and one step down alternately, so a cell costs
/// one uniform deviate unless rounding leaves the accumulated probability short of it.
unsafe fn random_table(
    call: &NativeCall<'_>,
    margins: &TableMargins,
    log_factorials: &[f64],
) -> R<Vec<i32>> {
    let row_count = margins.rows.len();
    let column_count = margins.columns.len();
    let mut table = vec![0; row_count * column_count];
    let mut columns_left: Vec<i64> = margins
        .columns
        .iter()
        .map(|&count| i64::from(count))
        .collect();
    let mut rows_below = margins.total as i64;
    let factorial = |k: i64| log_factorials.get(k as usize).copied().unwrap_or(0.0);
    for row in 0..row_count - 1 {
        let mut row_left = i64::from(margins.rows[row]);
        let mut block = rows_below;
        rows_below -= row_left;
        for column in 0..column_count - 1 {
            let column_total = columns_left[column];
            let remaining = block;
            block -= column_total;
            let below = remaining - row_left;
            let corner = below - column_total;
            if remaining == 0 {
                break;
            }
            let probability = |value: i64| {
                (factorial(row_left)
                    + factorial(below)
                    + factorial(block)
                    + factorial(column_total)
                    - factorial(remaining)
                    - factorial(value)
                    - factorial(column_total - value)
                    - factorial(row_left - value)
                    - factorial(corner + value))
                .exp()
            };
            let mode =
                (row_left as f64 * column_total as f64 / remaining as f64 + 0.5).floor() as i64;
            let mut target = random::unif_rand()?;
            let value = loop {
                let at_mode = probability(mode);
                if at_mode == 0.0 {
                    return Err(call.error("the table's margins give a cell probability of zero"));
                }
                let mut accumulated = at_mode;
                if accumulated >= target {
                    break mode;
                }
                let (mut up, mut up_probability) = (mode, at_mode);
                let (mut down, mut down_probability) = (mode, at_mode);
                let mut drawn = None;
                loop {
                    let up_factor = (column_total - up) * (row_left - up);
                    if up_factor > 0 {
                        up += 1;
                        up_probability *= up_factor as f64 / (up as f64 * (corner + up) as f64);
                        accumulated += up_probability;
                        if accumulated >= target {
                            drawn = Some(up);
                            break;
                        }
                    }
                    let down_factor = down * (corner + down);
                    if down_factor > 0 {
                        down -= 1;
                        down_probability *= down_factor as f64
                            / ((column_total - down) as f64 * (row_left - down) as f64);
                        accumulated += down_probability;
                        if accumulated >= target {
                            drawn = Some(down);
                            break;
                        }
                    }
                    if up_factor <= 0 && down_factor <= 0 {
                        break;
                    }
                }
                match drawn {
                    Some(value) => break value,
                    None => target = accumulated * random::unif_rand()?,
                }
            };
            table[row + column * row_count] = value as i32;
            row_left -= value;
            columns_left[column] -= value;
        }
        table[row + (column_count - 1) * row_count] = row_left as i32;
        columns_left[column_count - 1] -= row_left;
    }
    for (column, &count) in columns_left.iter().enumerate() {
        table[row_count - 1 + column * row_count] = count as i32;
    }
    Ok(table)
}

/// R 4.6's `dpermdist1(scores)`: the distribution of the sum of a random subset of the
/// (sorted, non-negative integer) scores, each in or out with probability one half, as the
/// signed rank statistic with ties needs it. Element `s + 1` is `P(sum = s)`.
pub unsafe fn dpermdist1(call: &NativeCall<'_>) -> R<Sexp> {
    let scores = crate::stats::integer_values(call.arg(0))?;
    let total: usize = scores.iter().map(|&score| score.max(0) as usize).sum();
    let mut density = vec![0.0; total + 1];
    density[0] = 1.0;
    let mut reach = 0;
    for &score in &scores {
        let score = score.max(0) as usize;
        reach += score;
        for sum in (0..=reach).rev() {
            let with = if sum >= score {
                density[sum - score]
            } else {
                0.0
            };
            density[sum] = 0.5 * (density[sum] + with);
        }
    }
    values::real_vector(&density)
}

/// R 4.6's `dpermdist2(scores, m)`: the distribution of the sum of `m` scores drawn without
/// replacement, as the rank sum statistic with ties needs it. Element `s` is `P(sum = s)`, up
/// to the sum of the `m` largest scores. Each step mixes "item `i` is in" and "item `i` is out"
/// by their probabilities, which stays finite where counting subsets would overflow.
pub unsafe fn dpermdist2(call: &NativeCall<'_>) -> R<Sexp> {
    let mut scores = crate::stats::integer_values(call.arg(0))?;
    scores.sort_unstable();
    let m = (values::as_integer_scalar(call.arg(1)).max(0) as usize).min(scores.len());
    let top: usize = scores
        .iter()
        .rev()
        .take(m)
        .map(|&score| score.max(0) as usize)
        .sum();
    let width = top + 1;
    // `table[j * width + s]`: the probability that a uniform `j`-subset of the items so far sums
    // to `s`.
    let mut table = vec![0.0; (m + 1) * width];
    table[0] = 1.0;
    for (index, &score) in scores.iter().enumerate() {
        let items = (index + 1) as f64;
        let score = score.max(0) as usize;
        for chosen in (0..=m.min(index + 1)).rev() {
            let keep = (items - chosen as f64) / items;
            let take = chosen as f64 / items;
            for sum in (0..width).rev() {
                let without = table[chosen * width + sum];
                let with = if chosen > 0 && sum >= score {
                    table[(chosen - 1) * width + sum - score]
                } else {
                    0.0
                };
                table[chosen * width + sum] = keep * without + take * with;
            }
        }
    }
    values::real_vector(&table[m * width + 1..(m + 1) * width])
}
