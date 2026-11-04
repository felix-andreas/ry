use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

fn risk_report(returns: &[f64], lambda: f64) -> f64 {
    let mut wealth = 1.0;
    let mut peak = 1.0;
    let mut worst = 0.0;
    let first = returns.first().copied().unwrap_or(0.0);
    let mut variance = first * first;
    for &r in returns {
        wealth *= 1.0 + r;
        if wealth > peak {
            peak = wealth;
        }
        let drawdown = (peak - wealth) / peak;
        if drawdown > worst {
            worst = drawdown;
        }
        variance = lambda * variance + (1.0 - lambda) * r * r;
    }
    worst + (variance * 252.0).sqrt()
}

fn main() {
    let returns: Vec<f64> = (1..=2_000_000).map(|i| (i as f64 * 0.37).sin() * 0.01 + 0.0002).collect();
    measure("drawdown", || risk_report(black_box(&returns), black_box(0.94)));
}

fn measure<T: PartialEq + Display>(name: &str, mut run: impl FnMut() -> T) {
    let started = Instant::now();
    let value = black_box(run());
    let cold = started.elapsed().as_secs_f64();
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let started = Instant::now();
        let again = black_box(run());
        best = best.min(started.elapsed().as_secs_f64());
        if again != value {
            eprintln!("the benchmark is not deterministic");
            std::process::exit(1);
        }
    }
    println!("{name} {value} {best:.4} {cold:.4}");
}
