// Recursion: about 2.7 million calls of a small function.
mod harness;

use harness::measure;
use std::hint::black_box;

fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() {
    measure("fib", || fib(black_box(30)) as f64);
}
