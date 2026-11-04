use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() {
    measure("fib", || fib(black_box(30)));
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
