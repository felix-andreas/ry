//! Properties of the Rmath functions on random and extreme arguments, complementing the fixed
//! grid that `test_rmath` compares against GNU R: quantiles invert distribution functions,
//! distribution functions are monotone with complementary tails, densities are nonnegative
//! and agree with their logs, NA stays NA, random variates stay in their support, and nothing
//! panics on NaN, infinite, huge or tiny arguments. `FUZZ_ITERS` scales the run.

use {
    R::{arith::Rng, rmath::*},
    interp::rmath::*,
    syntax::testing::{SplitMix64, fuzz_iterations},
};

const NA: f64 = f64::from_bits(0x7ff0_0000_0000_07a2);

fn is_na(value: f64) -> bool {
    value.is_nan() && (value.to_bits() & 0xffff_ffff) == 1954
}

fn uniform(rng: &mut SplitMix64) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// A positive parameter spread over many orders of magnitude.
fn positive(rng: &mut SplitMix64, low: f64, high: f64) -> f64 {
    (low.ln() + uniform(rng) * (high.ln() - low.ln())).exp()
}

/// An argument from the awkward corners as often as from the ordinary range.
fn wild(rng: &mut SplitMix64) -> f64 {
    const SPECIAL: [f64; 14] = [
        f64::NAN,
        NA,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -0.0,
        1.0,
        -1.0,
        f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        -f64::MAX,
        1e300,
        0.5,
    ];
    match rng.below(4) {
        0 => SPECIAL[rng.below(SPECIAL.len())],
        1 => (uniform(rng) - 0.5) * 20.0,
        2 => positive(rng, 1e-300, 1e300) * if rng.below(2) == 0 { 1.0 } else { -1.0 },
        _ => rng.below(200) as f64 - 50.0,
    }
}

type Pq = (
    &'static str,
    fn(f64, f64, f64, bool, bool) -> f64,
    fn(f64, f64, f64, bool, bool) -> f64,
    fn(&mut SplitMix64) -> (f64, f64),
);

/// Continuous two-parameter families, with a generator of valid parameters. One-parameter
/// families ignore the second.
fn continuous() -> Vec<Pq> {
    vec![
        ("norm", pnorm, qnorm, |rng| {
            ((uniform(rng) - 0.5) * 100.0, positive(rng, 1e-3, 1e3))
        }),
        ("unif", punif, qunif, |rng| {
            let a = (uniform(rng) - 0.5) * 10.0;
            (a, a + positive(rng, 1e-3, 1e3))
        }),
        ("gamma", pgamma, qgamma, |rng| {
            (positive(rng, 1e-2, 1e4), positive(rng, 1e-2, 1e2))
        }),
        ("beta", pbeta, qbeta, |rng| {
            (positive(rng, 1e-2, 1e4), positive(rng, 1e-2, 1e4))
        }),
        ("lnorm", plnorm, qlnorm, |rng| {
            ((uniform(rng) - 0.5) * 10.0, positive(rng, 1e-2, 10.0))
        }),
        ("cauchy", pcauchy, qcauchy, |rng| {
            ((uniform(rng) - 0.5) * 10.0, positive(rng, 1e-2, 1e2))
        }),
        ("weibull", pweibull, qweibull, |rng| {
            (positive(rng, 0.1, 20.0), positive(rng, 1e-2, 1e2))
        }),
        ("logis", plogis, qlogis, |rng| {
            ((uniform(rng) - 0.5) * 10.0, positive(rng, 1e-2, 1e2))
        }),
        (
            "t",
            |x, n, _, lower, log| pt(x, n, lower, log),
            |p, n, _, lower, log| qt(p, n, lower, log),
            |rng| (positive(rng, 1.0, 1e6), 0.0),
        ),
        (
            "chisq",
            |x, n, _, lower, log| pchisq(x, n, lower, log),
            |p, n, _, lower, log| qchisq(p, n, lower, log),
            |rng| (positive(rng, 0.05, 1e4), 0.0),
        ),
        (
            "exp",
            |x, s, _, lower, log| pexp(x, s, lower, log),
            |p, s, _, lower, log| qexp(p, s, lower, log),
            |rng| (positive(rng, 1e-3, 1e3), 0.0),
        ),
        ("f", pf, qf, |rng| {
            (positive(rng, 0.5, 1e3), positive(rng, 0.5, 1e3))
        }),
    ]
}

#[test]
fn quantiles_invert_distribution_functions() {
    let mut rng = SplitMix64(0x5eed_0001);
    let mut failures = Vec::new();
    for _ in 0..fuzz_iterations(300) {
        for (name, p, q, parameters) in continuous() {
            let (a, b) = parameters(&mut rng);
            let lower = rng.below(2) == 0;
            let log = rng.below(2) == 0;
            // qf is (1/qbeta - 1) df2/df1, as in GNU R, which cancels wherever qbeta is near
            // one, so it inverts pf only to about 1e-8 there; only its upper tail is checked.
            if name == "f" && lower {
                continue;
            }
            let probability = positive(&mut rng, 1e-12, 1.0 - 1e-12).min(1.0 - 1e-12);
            let target = if log { probability.ln() } else { probability };
            let quantile = q(target, a, b, lower, log);
            if !quantile.is_finite() {
                continue;
            }
            let back = p(quantile, a, b, lower, log);
            // The quantile is exact only up to its own rounding, so allow the change in p
            // that one part in 1e12 of the quantile makes.
            let wiggle = (p(quantile * (1.0 + 1e-12) + 1e-300, a, b, lower, log)
                - p(quantile * (1.0 - 1e-12) - 1e-300, a, b, lower, log))
            .abs();
            let error = (back - target).abs();
            if error > 1e-9 * target.abs() + wiggle + 1e-300 {
                failures.push(format!(
                    "{name}(a={a}, b={b}, lower={lower}, log={log}): p(q({target})) = {back}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn distribution_functions_are_monotone_with_complementary_tails() {
    let mut rng = SplitMix64(0x5eed_0002);
    for _ in 0..fuzz_iterations(200) {
        for (name, p, q, parameters) in continuous() {
            let (a, b) = parameters(&mut rng);
            let low = q(1e-6, a, b, true, false);
            let high = q(1.0 - 1e-6, a, b, true, false);
            if !low.is_finite() || !high.is_finite() {
                continue;
            }
            let mut previous = -1.0;
            for step in 0..=20 {
                let x = low + (high - low) * step as f64 / 20.0;
                let lower = p(x, a, b, true, false);
                let upper = p(x, a, b, false, false);
                assert!(
                    lower >= previous - 1e-15,
                    "{name}(a={a}, b={b}): p({x}) = {lower} < {previous}"
                );
                assert!(
                    (lower + upper - 1.0).abs() < 1e-12,
                    "{name}(a={a}, b={b}): tails at {x} sum to {}",
                    lower + upper
                );
                let log_lower = p(x, a, b, true, true);
                assert!(
                    (log_lower.exp() - lower).abs() <= 1e-12 * lower.max(1e-300),
                    "{name}(a={a}, b={b}): log p({x}) = {log_lower}, p = {lower}"
                );
                previous = lower;
            }
        }
    }
}

#[test]
fn densities_are_nonnegative_and_match_their_logs() {
    let mut rng = SplitMix64(0x5eed_0003);
    type Density = (&'static str, fn(f64, f64, f64, bool) -> f64);
    let densities: [Density; 10] = [
        ("norm", dnorm),
        ("gamma", dgamma),
        ("beta", dbeta),
        ("lnorm", dlnorm),
        ("cauchy", dcauchy),
        ("weibull", dweibull),
        ("logis", dlogis),
        ("f", df),
        ("nbinom", dnbinom_mu),
        ("binom", |x, n, p, log| {
            dbinom(x, n.round(), p.fract().abs(), log)
        }),
    ];
    for _ in 0..fuzz_iterations(2000) {
        for (name, density) in densities {
            let x = if rng.below(2) == 0 {
                rng.below(60) as f64
            } else {
                positive(&mut rng, 1e-5, 1e3)
            };
            let a = positive(&mut rng, 0.05, 200.0);
            let b = positive(&mut rng, 0.05, 200.0);
            let plain = density(x, a, b, false);
            let logged = density(x, a, b, true);
            if plain.is_nan() {
                continue;
            }
            assert!(plain >= 0.0, "{name}({x}, {a}, {b}) = {plain}");
            if plain > 1e-290 && plain.is_finite() {
                assert!(
                    (logged.exp() - plain).abs() <= 1e-11 * plain,
                    "{name}({x}, {a}, {b}): exp(log d) = {}, d = {plain}",
                    logged.exp()
                );
            }
        }
    }
}

#[test]
fn discrete_quantiles_are_the_smallest_with_enough_probability() {
    let mut rng = SplitMix64(0x5eed_0004);
    for _ in 0..fuzz_iterations(500) {
        let p = positive(&mut rng, 1e-8, 1.0 - 1e-8).min(1.0 - 1e-8);
        let n = (rng.below(5000) + 1) as f64;
        let probability = uniform(&mut rng).clamp(0.001, 0.999);
        let lambda = positive(&mut rng, 1e-3, 1e5);
        let size = positive(&mut rng, 0.1, 1e3);
        let cases: [(&str, f64, f64, f64); 3] = [
            (
                "binom",
                qbinom(p, n, probability, true, false),
                pbinom(
                    qbinom(p, n, probability, true, false),
                    n,
                    probability,
                    true,
                    false,
                ),
                pbinom(
                    qbinom(p, n, probability, true, false) - 1.0,
                    n,
                    probability,
                    true,
                    false,
                ),
            ),
            (
                "pois",
                qpois(p, lambda, true, false),
                ppois(qpois(p, lambda, true, false), lambda, true, false),
                ppois(qpois(p, lambda, true, false) - 1.0, lambda, true, false),
            ),
            (
                "nbinom",
                qnbinom(p, size, probability, true, false),
                pnbinom(
                    qnbinom(p, size, probability, true, false),
                    size,
                    probability,
                    true,
                    false,
                ),
                pnbinom(
                    qnbinom(p, size, probability, true, false) - 1.0,
                    size,
                    probability,
                    true,
                    false,
                ),
            ),
        ];
        for (name, quantile, at, below) in cases {
            assert!(
                at >= p * (1.0 - 64.0 * f64::EPSILON) && (quantile == 0.0 || below < p),
                "{name}: q({p}) = {quantile} with P = {at} and {below} below it"
            );
        }
    }
}

#[test]
fn nothing_panics_and_na_stays_na() {
    let mut rng = SplitMix64(0x5eed_0005);
    type Three = fn(f64, f64, f64) -> f64;
    let functions: [(&str, Three); 32] = [
        ("dnorm", |x, a, b| dnorm(x, a, b, false)),
        ("pnorm", |x, a, b| pnorm(x, a, b, true, true)),
        ("qnorm", |x, a, b| qnorm(x, a, b, false, true)),
        ("pgamma", |x, a, b| pgamma(x, a, b, false, false)),
        ("qgamma", |x, a, b| qgamma(x, a, b, true, false)),
        ("dgamma", |x, a, b| dgamma(x, a, b, true)),
        ("pbeta", |x, a, b| pbeta(x, a, b, true, true)),
        ("qbeta", |x, a, b| qbeta(x, a, b, false, false)),
        ("dbeta", |x, a, b| dbeta(x, a, b, false)),
        ("pt", |x, a, _| pt(x, a, false, true)),
        ("qt", |x, a, _| qt(x, a, true, false)),
        ("pf", |x, a, b| pf(x, a, b, true, false)),
        ("qf", |x, a, b| qf(x, a, b, true, true)),
        ("pbinom", |x, a, b| pbinom(x, a, b, true, false)),
        ("qbinom", |x, a, b| qbinom(x, a, b, true, false)),
        ("dbinom", |x, a, b| dbinom(x, a, b, true)),
        ("ppois", |x, a, _| ppois(x, a, false, false)),
        ("qpois", |x, a, _| qpois(x, a, true, true)),
        ("phyper", |x, a, b| phyper(x, a, b, 7.0, true, false)),
        ("qnbinom", |x, a, b| qnbinom(x, a, b, true, false)),
        ("pnchisq", |x, a, b| {
            pnchisq(x, a, b.abs().min(500.0), true, false)
        }),
        ("pnt", |x, a, b| {
            pnt(x, a, b.clamp(-30.0, 30.0), true, false)
        }),
        ("ptukey", |x, a, b| {
            ptukey(x, 1.0, a.abs().min(30.0), b, true, false)
        }),
        ("pwilcox", |x, a, b| {
            pwilcox(x, a.abs().min(30.0), b.abs().min(30.0), true, false)
        }),
        ("psignrank", |x, a, _| {
            psignrank(x, a.abs().min(200.0), true, false)
        }),
        ("gammafn", |x, _, _| gammafn(x)),
        ("lgammafn", |x, _, _| lgammafn(x)),
        ("psigamma", |x, a, _| psigamma(x, a.abs().min(10.0).floor())),
        ("bessel_i", |x, a, b| {
            bessel_i(x, a.clamp(-200.0, 200.0), if b > 0.0 { 2.0 } else { 1.0 })
        }),
        ("bessel_j", |x, a, _| {
            bessel_j(x.min(1e6), a.clamp(-200.0, 200.0))
        }),
        ("bessel_k", |x, a, b| {
            bessel_k(x, a.clamp(-200.0, 200.0), if b > 0.0 { 2.0 } else { 1.0 })
        }),
        ("fround", |x, a, _| fround(x, a)),
    ];
    for _ in 0..fuzz_iterations(800) {
        for (name, function) in functions {
            let (x, a, b) = (wild(&mut rng), wild(&mut rng), wild(&mut rng));
            let result = function(x, a, b);
            // Invalid parameters give NaN before an NA argument is looked at, ptukey reports
            // any NaN argument as NaN, and pnt treats a NaN quantile as +Inf, all as in GNU R.
            let valid = |value: f64| value.is_finite() && value > 0.0;
            if is_na(x)
                && valid(a)
                && valid(b)
                && name.starts_with(['d', 'p', 'q'])
                && !matches!(name, "ptukey" | "pnt")
            {
                assert!(is_na(result), "{name}(NA, {a}, {b}) = {result}, not NA");
            }
        }
    }
}

#[test]
fn random_variates_stay_in_their_support() {
    let mut seeds = SplitMix64(0x5eed_0006);
    let mut rng = Rng::default();
    rng.seed(20251031);
    for _ in 0..fuzz_iterations(2000) {
        let a = positive(&mut seeds, 1e-3, 1e4);
        let b = positive(&mut seeds, 1e-3, 1e4);
        let p = uniform(&mut seeds).clamp(1e-6, 1.0);
        let n = seeds.below(100000) as f64;
        let checks = [
            ("rgamma", rgamma(&mut rng, a, b), 0.0, f64::INFINITY),
            ("rbeta", rbeta(&mut rng, a, b), 0.0, 1.0),
            ("rchisq", rchisq(&mut rng, a), 0.0, f64::INFINITY),
            ("rbinom", rbinom(&mut rng, n, p), 0.0, n),
            ("rpois", rpois(&mut rng, a), 0.0, f64::INFINITY),
            ("rgeom", rgeom(&mut rng, p), 0.0, f64::INFINITY),
            ("rnbinom", rnbinom(&mut rng, a, p), 0.0, f64::INFINITY),
            (
                "rhyper",
                rhyper(&mut rng, n, b.floor(), (n / 2.0).floor()),
                0.0,
                n,
            ),
            ("rexp", rexp(&mut rng, a), 0.0, f64::INFINITY),
            ("runif", runif(&mut rng, -a, b), -a, b),
            (
                "rweibull",
                rweibull(&mut rng, a.min(50.0), b),
                0.0,
                f64::INFINITY,
            ),
            (
                "rsignrank",
                rsignrank(&mut rng, (n / 1000.0).floor()),
                0.0,
                f64::INFINITY,
            ),
        ];
        for (name, value, low, high) in checks {
            assert!(
                value >= low && value <= high,
                "{name} drew {value} outside [{low}, {high}] (a={a}, b={b}, p={p}, n={n})"
            );
        }
        let normal = rng.norm_rand();
        assert!(normal.is_finite(), "norm_rand drew {normal}");
        let exponential = rng.exp_rand();
        assert!(exponential >= 0.0 && exponential.is_finite());
    }
}
