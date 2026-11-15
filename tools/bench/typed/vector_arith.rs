// Whole-slice arithmetic on a million doubles, `x * 2 + x / 3 - 1` summed, twenty times.
mod harness;

use harness::measure;
use std::hint::black_box;

fn vector_arith(x: &[f64], times: usize) -> f64 {
    let mut total = 0.0;
    for _ in 0..times {
        total += x.iter().map(|item| item * 2.0 + item / 3.0 - 1.0).sum::<f64>();
    }
    total
}

fn main() {
    let x = sequence(0.0, 1.0, 1_000_000);
    measure("vector_arith", || vector_arith(black_box(&x), black_box(20)));
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
