use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

fn closures(count: i64) -> f64 {
    let add = |a: f64, b: f64| a + b;
    let scale = black_box(3.0);
    let times_scale = |a: f64| a * scale;
    let mut sum = 0.0;
    for i in 1..=count {
        sum = add(sum, times_scale(i as f64));
    }
    let squares: Vec<f64> = (1..=count).map(|x| x as f64 * (x as f64 + 0.5)).collect();
    let halves: Vec<f64> = (1..=count).map(|x| x as f64 / 2.0).collect();
    sum + squares.iter().sum::<f64>() + halves.iter().sum::<f64>()
}

fn main() {
    measure("closures", || closures(black_box(300_000)));
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
