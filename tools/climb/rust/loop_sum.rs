use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

fn loop_sum(count: i64) -> f64 {
    (1..=count).map(|i| i as f64 * 0.5).sum()
}

fn main() {
    measure("loop_sum", || loop_sum(black_box(10_000_000)));
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
