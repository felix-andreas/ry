// A pipeline over a million sales rows held as one vector per column: keep the rows with more
// than three units, price them, and total the revenue per region in a hash map keyed by the
// region's name.
mod harness;

use harness::measure;
use std::collections::HashMap;
use std::hint::black_box;

fn pipeline(region: &[String], units: &[i64], price: &[f64]) -> f64 {
    let mut totals: HashMap<&str, f64> = HashMap::new();
    for ((region, &units), &price) in region.iter().zip(units).zip(price) {
        if units > 3 {
            *totals.entry(region).or_insert(0.0) += units as f64 * price;
        }
    }
    let total = |name: &str| totals.get(name).copied().unwrap_or(0.0);
    total("east") + 2.0 * total("north") + 3.0 * total("south") + 4.0 * total("west")
}

fn main() {
    let names = ["north", "south", "east", "west"];
    let rows = 1..=1_000_000i64;
    let region: Vec<String> = rows.clone().map(|i| names[(i % 4) as usize].to_string()).collect();
    let units: Vec<i64> = rows.clone().map(|i| i % 17).collect();
    let price: Vec<f64> = rows.map(|i| (1 + i % 100) as f64).collect();
    measure("sales", || pipeline(black_box(&region), black_box(&units), black_box(&price)));
}
