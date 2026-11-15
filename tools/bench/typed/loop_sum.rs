// A scalar double accumulator over a counted range of ten million steps.
mod harness;

use harness::measure;
use std::hint::black_box;

fn loop_sum(count: i64) -> f64 {
    (1..=count).map(|i| i as f64 * 0.5).sum()
}

fn main() {
    measure("loop_sum", || loop_sum(black_box(10_000_000)));
}
