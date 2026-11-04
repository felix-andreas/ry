use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

fn bond_price(coupon: f64, rate: f64, years: i64) -> f64 {
    (1..=years)
        .map(|year| {
            let flow = if year == years { coupon + 100.0 } else { coupon };
            flow * (-rate * year as f64).exp()
        })
        .sum()
}

fn price_book(count: i64) -> f64 {
    let total: f64 = (1..=count)
        .map(|i| bond_price((2 + i % 5) as f64, 0.01 + (i % 7) as f64 * 0.003, 5 + i % 20))
        .sum();
    (total * 1_000_000.0).round() / 1_000_000.0
}

fn main() {
    measure("bond_book", || price_book(black_box(200_000)));
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
