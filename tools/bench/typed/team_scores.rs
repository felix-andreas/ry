// Parses 60,000 "name,age,score,team" rows, rejects the invalid ones, and totals the valid
// scores per team in a hash map. A parsed person borrows its name and team from its row.
mod harness;

use harness::measure;
use std::collections::HashMap;
use std::hint::black_box;

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
    measure("team_scores", || team_scores(black_box(&rows)));
}
