// Counts the items above one half in a slice of a million doubles, five times.
mod harness;

use harness::measure;
use std::hint::black_box;

fn array_filter(x: &[f64], times: usize) -> f64 {
    let mut total = 0.0;
    for _ in 0..times {
        total += x.iter().filter(|&&item| item > 0.5).count() as f64;
    }
    total
}

fn main() {
    let x = sequence(0.0, 1.0, 1_000_000);
    measure("array_filter", || array_filter(black_box(&x), black_box(5)));
}

// R's seq(from, to, length.out = n): interior points are from + k * by, and the last is exactly to.
fn sequence(from: f64, to: f64, length: usize) -> Vec<f64> {
    let by = (to - from) / (length - 1) as f64;
    let mut values: Vec<f64> = (0..length).map(|k| from + k as f64 * by).collect();
    if let Some(last) = values.last_mut() {
        *last = to;
    }
    values
}
