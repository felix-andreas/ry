use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

struct Trial {
    time: Vec<i64>,
    event: Vec<bool>,
    treated: Vec<bool>,
}

fn log_rank(data: &Trial) -> f64 {
    let mut times: Vec<i64> = data
        .time
        .iter()
        .zip(&data.event)
        .filter(|&(_, &event)| event)
        .map(|(&time, _)| time)
        .collect();
    times.sort_unstable();
    times.dedup();
    let mut observed = 0.0;
    let mut expected = 0.0;
    let mut variance = 0.0;
    for &t in &times {
        let (mut at_risk, mut treated_at_risk, mut deaths, mut treated_deaths) = (0u32, 0u32, 0u32, 0u32);
        for ((&time, &event), &treated) in data.time.iter().zip(&data.event).zip(&data.treated) {
            let risk = time >= t;
            let died = time == t && event;
            at_risk += risk as u32;
            treated_at_risk += (risk && treated) as u32;
            deaths += died as u32;
            treated_deaths += (died && treated) as u32;
        }
        let n = at_risk as f64;
        let n1 = treated_at_risk as f64;
        let d = deaths as f64;
        observed += treated_deaths as f64;
        expected += d * n1 / n;
        if n > 1.0 {
            variance += d * (n1 / n) * (1.0 - n1 / n) * (n - d) / (n - 1.0);
        }
    }
    (observed - expected) * (observed - expected) / variance
}

fn main() {
    let subjects = 1..=4000i64;
    let data = Trial {
        time: subjects.clone().map(|i| (i * 7919) % 1000 + 1).collect(),
        event: subjects.clone().map(|i| i % 3 != 0).collect(),
        treated: subjects.map(|i| i % 2 == 0).collect(),
    };
    measure("log_rank", || log_rank(black_box(&data)));
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
