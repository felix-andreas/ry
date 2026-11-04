use std::collections::HashMap;
use std::fmt::Display;
use std::hint::black_box;
use std::time::Instant;

#[allow(dead_code)]
struct Person<'a> {
    name: &'a str,
    age: i64,
    score: f64,
    team: &'a str,
}

#[allow(dead_code)]
enum RowError {
    BadAge,
    BadScore,
    Minor,
}

fn parse_row(row: &str) -> Result<Person<'_>, RowError> {
    let mut parts = row.split(',');
    let mut next = || parts.next().unwrap_or("");
    let (name, age, score, team) = (next(), next(), next(), next());
    let age: i64 = age.parse().map_err(|_| RowError::BadAge)?;
    if age < 18 {
        return Err(RowError::Minor);
    }
    let score: f64 = score.parse().map_err(|_| RowError::BadScore)?;
    Ok(Person { name, age, score, team })
}

fn team_scores(rows: &[String]) -> f64 {
    let parsed: Vec<Result<Person, RowError>> = rows.iter().map(|row| parse_row(row)).collect();
    let mut totals: HashMap<&str, f64> = HashMap::new();
    let mut people = 0;
    for person in parsed.iter().flatten() {
        *totals.entry(person.team).or_insert(0.0) += person.score;
        people += 1;
    }
    let errors = (parsed.len() - people) as f64;
    let score = |team: &str| totals.get(team).copied().unwrap_or(0.0);
    score("red") + 2.0 * score("blue") + 3.0 * score("green") + errors
}

fn main() {
    let teams = ["red", "blue", "green"];
    let rows: Vec<String> = (1..=60_000i64)
        .map(|i| {
            let age = if i % 17 == 0 { "x".to_string() } else { (i % 90).to_string() };
            format!("user{i},{age},{},{}", (i % 50) as f64 / 2.0, teams[(i % 3) as usize])
        })
        .collect();
    measure("validate", || team_scores(black_box(&rows)));
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
