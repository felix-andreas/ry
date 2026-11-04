use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

#[derive(Clone, Copy)]
enum Region {
    North,
    South,
    East,
    West,
}

const REGIONS: [Region; 4] = [Region::North, Region::South, Region::East, Region::West];

impl Region {
    fn name(self) -> &'static str {
        match self {
            Region::North => "north",
            Region::South => "south",
            Region::East => "east",
            Region::West => "west",
        }
    }
}

struct Sales {
    region: Vec<Region>,
    units: Vec<i32>,
    price: Vec<f64>,
}

// The region column is categorical (an enum per row), the way a Rust program would model a
// column with four known values, so grouping indexes an array instead of hashing strings.
fn pipeline(sales: &Sales) -> f64 {
    let mut totals = [0.0f64; 4];
    for ((&region, &units), &price) in sales.region.iter().zip(&sales.units).zip(&sales.price) {
        if units > 3 {
            totals[region as usize] += units as f64 * price;
        }
    }
    let mut groups: Vec<(&str, f64)> = REGIONS.iter().map(|&region| (region.name(), totals[region as usize])).collect();
    groups.sort_unstable_by_key(|&(name, _)| name);
    groups.iter().zip(1..).map(|(&(_, total), rank)| total * rank as f64).sum()
}

fn main() {
    let rows = 1..=1_000_000i32;
    let sales = Sales {
        region: rows.clone().map(|i| REGIONS[(i % 4) as usize]).collect(),
        units: rows.clone().map(|i| i % 17).collect(),
        price: rows.map(|i| (1 + i % 100) as f64).collect(),
    };
    measure("sales", || pipeline(black_box(&sales)));
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
