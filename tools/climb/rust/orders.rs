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

struct Order {
    id: i64,
    quantity: i64,
    unit: f64,
    loyal: bool,
    region: Region,
}

#[allow(dead_code)]
struct Invoice {
    id: i64,
    net: f64,
    tax: f64,
    gross: f64,
}

fn tax_rate(region: Region) -> f64 {
    match region {
        Region::North => 0.2,
        Region::South => 0.1,
        Region::East => 0.15,
        Region::West => 0.05,
    }
}

fn price_line(order: &Order) -> Invoice {
    let mut net = order.quantity as f64 * order.unit;
    if order.loyal {
        net *= 0.95;
    }
    if order.quantity > 5 {
        net -= 1.0;
    }
    let tax = net * tax_rate(order.region);
    Invoice { id: order.id, net, tax, gross: net + tax }
}

fn region_of(i: i64) -> Region {
    match i % 4 {
        0 => Region::North,
        1 => Region::South,
        2 => Region::East,
        _ => Region::West,
    }
}

fn price_orders(count: i64) -> f64 {
    (1..=count)
        .map(|i| {
            let order = Order {
                id: i,
                quantity: i % 7 + 1,
                unit: 10.0 + (i % 13) as f64 * 0.5,
                loyal: i % 3 == 0,
                region: region_of(i),
            };
            price_line(&order).gross
        })
        .sum()
}

fn main() {
    measure("orders", || price_orders(black_box(200_000)));
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
