// Small closures called in a loop, then two maps over a range, each collected and summed.
mod harness;

use harness::measure;
use std::hint::black_box;

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
