//! The table benchmark's questions, run on the ry engine through its query IR. It prints the same
//! tab-separated lines as `tools/bench/table/datatable.R` (`question  run  seconds  rows  cols
//! checksum`), so the two outputs line up row by row. Loading is not timed, and every question
//! runs twice.
//!
//! Usage: `cargo bench -p table --bench db_benchmark -- <data-dir> [--groupby-only]`, where the
//! directory holds the CSV files `tools/bench/table/datagen.R` writes. `--groupby-only` runs only
//! the ten group-by questions.

mod allocator;

use {
    std::{
        env,
        path::{Path, PathBuf},
        process::ExitCode,
        time::Instant,
    },
    table::{
        ColumnType, Table,
        io::{ReadOptions, read_csv_with},
        lazy::{How, Plan, Source, Step, Strategy, collect, output_name},
        polars::prelude::{DataType, Float64Chunked, PlSmallStr},
        query::{
            Aggregate, BinaryOp, Expr, Output, Query, Rows, Select, SortKey,
            build::{aggregate, binary, column},
            execute,
        },
    },
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let Some(dir) = env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"))
    else {
        eprintln!("usage: db_benchmark <data-dir> [--groupby-only]");
        return ExitCode::FAILURE;
    };
    let groupby_only = env::args()
        .skip(1)
        .any(|argument| argument == "--groupby-only");
    match run(&PathBuf::from(dir), groupby_only) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("db_benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &Path, groupby_only: bool) -> Result<()> {
    println!(
        "# ry table engine on polars 0.55, {} threads",
        polars_threads()
    );
    groupby(dir, groupby_only)?;
    if groupby_only {
        return Ok(());
    }
    joins(dir)?;
    rolling_join(dir)
}

fn groupby(dir: &Path, groupby_only: bool) -> Result<()> {
    let mut x = read(dir, "groupby.csv", true)?;
    let sum = |name: &str| unnamed(aggregate(Aggregate::Sum, column(name)));
    let mean = |name: &str| unnamed(aggregate(Aggregate::Mean, column(name)));
    let questions = [
        ("groupby q1", grouped(&x, &["id1"], vec![sum("v1")])),
        ("groupby q2", grouped(&x, &["id1", "id2"], vec![sum("v1")])),
        (
            "groupby q3",
            grouped(&x, &["id3"], vec![sum("v1"), mean("v3")]),
        ),
        (
            "groupby q4",
            grouped(&x, &["id4"], vec![mean("v1"), mean("v2"), mean("v3")]),
        ),
        (
            "groupby q5",
            grouped(&x, &["id6"], vec![sum("v1"), sum("v2"), sum("v3")]),
        ),
        (
            "groupby q6",
            grouped(
                &x,
                &["id4", "id5"],
                vec![
                    named("median_v3", aggregate(Aggregate::Median, column("v3"))),
                    named("sd_v3", aggregate(Aggregate::Sd, column("v3"))),
                ],
            ),
        ),
        (
            "groupby q7",
            grouped(
                &x,
                &["id3"],
                vec![named(
                    "range_v1_v2",
                    binary(
                        BinaryOp::Subtract,
                        aggregate(Aggregate::Max, column("v1")),
                        aggregate(Aggregate::Min, column("v2")),
                    ),
                )],
            ),
        ),
        (
            "groupby q8",
            Plan::new(Source::Table(x.clone()))
                .with(Step::Sort(vec![SortKey {
                    expr: column("v3"),
                    descending: true,
                }]))
                .with(Step::Aggregate {
                    keys: keys(&["id6"]),
                    aggregates: vec![named("largest2_v3", Expr::Head(Box::new(column("v3")), 2))],
                }),
        ),
        (
            "groupby q9",
            grouped(
                &x,
                &["id2", "id4"],
                vec![named(
                    "r2",
                    binary(
                        BinaryOp::Power,
                        Expr::Correlation(Box::new(column("v1")), Box::new(column("v2"))),
                        Expr::Double(2.0),
                    ),
                )],
            ),
        ),
        (
            "groupby q10",
            grouped(
                &x,
                &["id1", "id2", "id3", "id4", "id5", "id6"],
                vec![sum("v3"), named("count", Expr::Count)],
            ),
        ),
    ];
    for (question, plan) in questions {
        time_question(question, || collected(plan.clone()))?;
    }
    if groupby_only {
        return Ok(());
    }

    time_question("update full", || {
        for _ in 0..100 {
            let update = binary(BinaryOp::Multiply, column("v3"), Expr::Double(1.000001));
            execute(&mut x, assign("v3", update))?;
        }
        total(&mut x, "v3")
    })?;
    time_question("update subset", || {
        for step in 1..=100 {
            let rows = Rows::Filter(binary(BinaryOp::Equal, column("id4"), Expr::Integer(step)));
            let update = binary(BinaryOp::Add, column("v1"), Expr::Integer(1));
            execute(
                &mut x,
                Query {
                    rows,
                    ..assign("v1", update)
                },
            )?;
        }
        total(&mut x, "v1")
    })?;
    time_question("update add", || {
        for step in 1..=100 {
            let update = binary(BinaryOp::Multiply, column("v2"), Expr::Integer(step));
            execute(&mut x, assign(&format!("added{step}"), update))?;
        }
        total(&mut x, "added100")
    })
}

fn joins(dir: &Path) -> Result<()> {
    let x = read(dir, "join_x.csv", true)?;
    let small = read(dir, "join_small.csv", true)?;
    let medium = read(dir, "join_medium.csv", true)?;
    let big = read(dir, "join_big.csv", true)?;
    let questions = [
        ("join q1", joined(&x, &small, &["id1"], How::Inner)),
        ("join q2", joined(&x, &medium, &["id2"], How::Inner)),
        ("join q3", joined(&x, &medium, &["id2"], How::Left)),
        ("join q4", joined(&x, &medium, &["id5"], How::Inner)),
        ("join q5", joined(&x, &big, &["id3"], How::Inner)),
    ];
    for (question, plan) in questions {
        time_question(question, || collected(plan.clone()))?;
    }
    Ok(())
}

fn rolling_join(dir: &Path) -> Result<()> {
    let prices = read(dir, "rolljoin_prices.csv", false)?;
    let trades = read(dir, "rolljoin_trades.csv", false)?;
    let plan = joined(
        &trades,
        &prices,
        &["ticker", "time"],
        How::Asof(Strategy::Backward),
    );
    time_question("rolljoin keyed", || collected(plan.clone()))
}

/// Reads a CSV file as `DataFrame.read_csv(strings_as_factors = factors)` does in `ry.R`.
fn read(dir: &Path, name: &str, factors: bool) -> Result<Table> {
    let options = ReadOptions {
        factors,
        ..ReadOptions::default()
    };
    Ok(read_csv_with(&dir.join(name), &options)?)
}

/// Times one question. The checksum is computed after the clock stops, as `datatable.R` does.
fn time_question(question: &str, mut run: impl FnMut() -> Result<Output>) -> Result<()> {
    for run_number in 1..=2 {
        let start = Instant::now();
        let output = run()?;
        let seconds = start.elapsed().as_secs_f64();
        let answer = answer(output)?;
        println!(
            "{question}\t{run_number}\t{seconds:.4}\t{}\t{}\t{:.6e}",
            answer.rows, answer.cols, answer.checksum
        );
    }
    Ok(())
}

/// What `datatable.R` reports about an answer: its shape and the sum of its numeric columns.
struct Answer {
    rows: usize,
    cols: usize,
    checksum: f64,
}

fn answer(output: Output) -> Result<Answer> {
    match output {
        Output::Table(table) => {
            let schema = table.schema();
            let frame = table.frame();
            let mut checksum = 0.0;
            for (name, kind) in &schema.columns {
                if matches!(kind, ColumnType::Integer | ColumnType::Double) {
                    let values = frame
                        .column(name)?
                        .as_materialized_series()
                        .cast(&DataType::Float64)?;
                    checksum += sum_without_na(values.f64()?);
                }
            }
            Ok(Answer {
                rows: frame.height(),
                cols: frame.width(),
                checksum,
            })
        }
        Output::Vector(series) => {
            let values = series.cast(&DataType::Float64)?;
            Ok(Answer {
                rows: series.len(),
                cols: 1,
                checksum: sum_without_na(values.f64()?),
            })
        }
        Output::Updated => {
            Err("a question must produce an answer, not only update the table".into())
        }
    }
}

/// `sum(x, na.rm = TRUE)`, which drops `NaN` as well as `NA`.
fn sum_without_na(values: &Float64Chunked) -> f64 {
    values
        .downcast_iter()
        .flat_map(|array| array.iter().flatten().copied())
        .filter(|value| !value.is_nan())
        .sum()
}

/// The plan `DataFrame.group_by(x, keys) |> GroupBy.agg(aggregates)` builds.
fn grouped(x: &Table, names: &[&str], aggregates: Vec<(PlSmallStr, Expr)>) -> Plan {
    Plan::new(Source::Table(x.clone())).with(Step::Aggregate {
        keys: keys(names),
        aggregates,
    })
}

/// The plan `DataFrame.join(x, other, on = names, how = how)` builds.
fn joined(x: &Table, other: &Table, names: &[&str], how: How) -> Plan {
    Plan::new(Source::Table(x.clone())).with(Step::Join {
        other: Box::new(Plan::new(Source::Table(other.clone()))),
        left_on: names.iter().map(|&name| name.into()).collect(),
        right_on: names.iter().map(|&name| name.into()).collect(),
        how,
        suffix: "_right".into(),
    })
}

fn collected(plan: Plan) -> Result<Output> {
    Ok(Output::Table(collect(plan)?.table))
}

fn keys(names: &[&str]) -> Vec<(PlSmallStr, Expr)> {
    names
        .iter()
        .map(|&name| named(name, column(name)))
        .collect()
}

/// An aggregate named after its column, as Polars names `col("v1") |> Expr.sum()`.
fn unnamed(expr: Expr) -> (PlSmallStr, Expr) {
    (output_name(&expr), expr)
}

fn named(name: &str, expr: Expr) -> (PlSmallStr, Expr) {
    (name.into(), expr)
}

/// `x[, .(name = sum(name))]`, the answer of a `:=` loop.
fn total(x: &mut Table, name: &str) -> Result<Output> {
    let query = Query {
        select: Select::Columns(vec![named(name, aggregate(Aggregate::Sum, column(name)))]),
        ..Query::default()
    };
    Ok(execute(x, query)?.output)
}

fn assign(name: &str, expr: Expr) -> Query {
    Query {
        select: Select::Assign(vec![(name.into(), Some(expr))]),
        ..Query::default()
    }
}

/// Polars sizes its thread pool from `POLARS_MAX_THREADS`, or else from the available cores.
fn polars_threads() -> usize {
    env::var("POLARS_MAX_THREADS")
        .ok()
        .and_then(|threads| threads.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
}
