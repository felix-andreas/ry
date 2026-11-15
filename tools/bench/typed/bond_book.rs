// Prices a book of 200,000 bonds, each a short sum over its discounted cash flows.
mod harness;

use harness::measure;
use std::hint::black_box;

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
