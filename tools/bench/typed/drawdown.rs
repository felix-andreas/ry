// Maximum drawdown and EWMA volatility in one pass over two million returns.
mod harness;

use harness::measure;
use std::hint::black_box;

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
