// A table pipeline over a million sales rows: filter, add a revenue column, total it per region.
// The table is a struct of column vectors whose region column holds an enum per row.
mod harness;

use harness::measure;
use std::hint::black_box;

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
