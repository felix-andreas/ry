// Formats one report line per account, then joins and measures the report.
mod harness;

use harness::measure;
use std::hint::black_box;

fn report(count: i64) -> f64 {
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
    (text.len() + long + early) as f64
}

fn main() {
    measure("report", || report(black_box(20_000)));
}
