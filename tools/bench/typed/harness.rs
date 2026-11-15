//! The Rust twin of `harness.typed.R`. Every program in this directory reports through `measure`,
//! which takes the same samples as the typed harness and prints the same line, so `bench.py` reads
//! both engines alike. A program passes its inputs through `black_box`, which keeps the compiler
//! from computing the result at compile time.
use std::hint::black_box;
use std::time::Instant;

/// Runs the program once cold, then `RY_BENCH_REPEAT` more times warm (default 5), and prints
///   <name> <checksum> <median warm seconds> <best warm seconds> <cold seconds>
/// The checksum is the result itself, which every warm run must reproduce.
pub fn measure(name: &str, mut run: impl FnMut() -> f64) {
    let repeats = match std::env::var("RY_BENCH_REPEAT") {
        Ok(text) => match text.parse::<usize>() {
            Ok(repeats) if repeats > 0 => repeats,
            _ => fail(&format!("RY_BENCH_REPEAT must be a positive whole number, not `{text}`")),
        },
        Err(_) => 5,
    };
    let started = Instant::now();
    let value = black_box(run());
    let cold = started.elapsed().as_secs_f64();
    let mut times = Vec::with_capacity(repeats);
    for _ in 0..repeats {
        let started = Instant::now();
        let again = black_box(run());
        times.push(started.elapsed().as_secs_f64());
        if again != value {
            fail("the program is not deterministic");
        }
    }
    times.sort_by(f64::total_cmp);
    let middle = repeats / 2;
    let median = if repeats % 2 == 1 { times[middle] } else { (times[middle - 1] + times[middle]) / 2.0 };
    println!("{name} {value} {median:.6} {:.6} {cold:.6}", times[0]);
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}
