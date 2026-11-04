use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

fn report(count: i64) -> usize {
    let lines: Vec<String> = (1..=count)
        .map(|i| {
            let cents = i * 125;
            let side = if i % 2 == 0 { "long" } else { "short" };
            format!("ACC{i:06};{}.{:02};{side}", cents / 100, cents % 100)
        })
        .collect();
    let text = lines.join("\n");
    let long = lines.iter().filter(|line| line.to_ascii_uppercase().len() > 25).count();
    let early = lines.iter().filter(|line| line.starts_with("ACC00")).count();
    text.len() + long + early
}

fn main() {
    measure("report", || report(black_box(20_000)));
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
