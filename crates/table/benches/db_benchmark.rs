//! The table benchmark's questions, run on the ry engine through its query IR. It prints the same
//! tab-separated lines as `tools/bench/internal/table/datatable.R` (`question  run  seconds  rows  cols
//! checksum`), so the two outputs line up row by row. Loading is not timed, and every question
//! runs twice.
//!
//! Usage: `cargo bench -p table --bench db_benchmark -- <data-dir> [--groupby-only | --joins-only]
//! [--polars-streaming]`, where the directory holds the CSV files
//! `tools/bench/internal/table/datagen.R` writes. `--groupby-only` runs only the ten group-by
//! questions, and `--joins-only` only the five join questions.
//!
//! `--polars-streaming` answers the group-by and join questions with plain Polars plans on
//! Polars' streaming engine instead of ry's kernels, over the same tables in memory, so the two
//! can be compared question by question.

mod allocator;

use {
    std::{
        env,
        path::{Path, PathBuf},
        process::ExitCode,
        time::Instant,
    },
    table::{
        ColumnType, Table, factor,
        io::{ReadOptions, read_csv_with},
        lazy::{How, Plan, Source, Step, Strategy, collect, output_name},
        polars::prelude::{
            DataFrame, DataType, Engine, Float64Chunked, IntoColumn, IntoLazy, JoinArgs, JoinType,
            LazyFrame, PlSmallStr, SortMultipleOptions, col, len, lit, pearson_corr,
        },
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
        eprintln!(
            "usage: db_benchmark <data-dir> [--groupby-only | --joins-only] [--polars-streaming]"
        );
        return ExitCode::FAILURE;
    };
    let polars_streaming = env::args()
        .skip(1)
        .any(|argument| argument == "--polars-streaming");
    let only = env::args()
        .skip(1)
        .find_map(|argument| match argument.as_str() {
            "--groupby-only" => Some(Only::Groupby),
            "--joins-only" => Some(Only::Joins),
            _ => None,
        });
    let dir = PathBuf::from(dir);
    let result = if polars_streaming {
        streaming(&dir, only)
    } else {
        run(&dir, only)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("db_benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The part of the benchmark a run is limited to.
#[derive(Clone, Copy, PartialEq)]
enum Only {
    Groupby,
    Joins,
}

fn run(dir: &Path, only: Option<Only>) -> Result<()> {
    println!(
        "# ry table engine on polars 0.55, {} threads",
        polars_threads()
    );
    if only != Some(Only::Joins) {
        groupby(dir, only == Some(Only::Groupby))?;
    }
    if only == Some(Only::Groupby) {
        return Ok(());
    }
    joins(dir)?;
    if only == Some(Only::Joins) {
        return Ok(());
    }
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

/// The group-by and join questions as plain Polars plans on Polars' streaming engine.
///
/// The tables are read by ry's reader and stay in memory, as in the other runs. A factor column
/// is handed to Polars as its codes, so Polars groups and joins on small integers and never sees
/// a label: the comparison is between ry's kernels and the streaming engine's operators, not
/// between ry's factors and Polars' strings. For the join on the factor `id5`, the right table's
/// column is recoded to the left table's levels before the clock starts, and a label the left
/// table lacks becomes a null, which matches nothing in an inner join.
fn streaming(dir: &Path, only: Option<Only>) -> Result<()> {
    println!(
        "# polars 0.55 streaming engine, {} threads",
        polars_threads()
    );
    if only != Some(Only::Joins) {
        let x = codes(read(dir, "groupby.csv", true)?)?.lazy();
        let by = |names: &[&str]| x.clone().group_by(names.iter().map(|&name| col(name)).collect::<Vec<_>>());
        let questions = [
            ("groupby q1", by(&["id1"]).agg([col("v1").sum()])),
            ("groupby q2", by(&["id1", "id2"]).agg([col("v1").sum()])),
            (
                "groupby q3",
                by(&["id3"]).agg([col("v1").sum(), col("v3").mean()]),
            ),
            (
                "groupby q4",
                by(&["id4"]).agg([col("v1").mean(), col("v2").mean(), col("v3").mean()]),
            ),
            (
                "groupby q5",
                by(&["id6"]).agg([col("v1").sum(), col("v2").sum(), col("v3").sum()]),
            ),
            (
                "groupby q6",
                by(&["id4", "id5"]).agg([
                    col("v3").median().alias("median_v3"),
                    col("v3").std(1).alias("sd_v3"),
                ]),
            ),
            (
                "groupby q7",
                by(&["id3"]).agg([(col("v1").max() - col("v2").min()).alias("range_v1_v2")]),
            ),
            (
                "groupby q8",
                x.clone()
                    .sort(
                        ["v3"],
                        SortMultipleOptions::default().with_order_descending(true),
                    )
                    .group_by([col("id6")])
                    .agg([col("v3").head(Some(2)).alias("largest2_v3")]),
            ),
            (
                "groupby q9",
                by(&["id2", "id4"])
                    .agg([pearson_corr(col("v1"), col("v2")).pow(lit(2)).alias("r2")]),
            ),
            (
                "groupby q10",
                by(&["id1", "id2", "id3", "id4", "id5", "id6"])
                    .agg([col("v3").sum(), len().alias("count")]),
            ),
        ];
        for (question, plan) in questions {
            time_streaming(question, &plan)?;
        }
    }
    if only == Some(Only::Groupby) {
        return Ok(());
    }

    let x = read(dir, "join_x.csv", true)?;
    let small = codes(read(dir, "join_small.csv", true)?)?.lazy();
    let mut medium = read(dir, "join_medium.csv", true)?.into_frame();
    let levels = factor::levels(x.frame().column("id5")?.dtype())
        .ok_or("`id5` of `join_x.csv` is read as a factor")?;
    let recoded = factor::recode(medium.column("id5")?, levels)?;
    medium.with_column(recoded.into_column())?;
    let medium = codes(Table::new(medium)?)?.lazy();
    let big = codes(read(dir, "join_big.csv", true)?)?.lazy();
    let x = codes(x)?.lazy();
    let joined = |other: &LazyFrame, key: &str, how: JoinType| {
        x.clone()
            .join(other.clone(), [col(key)], [col(key)], JoinArgs::new(how))
    };
    let questions = [
        ("join q1", joined(&small, "id1", JoinType::Inner)),
        ("join q2", joined(&medium, "id2", JoinType::Inner)),
        ("join q3", joined(&medium, "id2", JoinType::Left)),
        ("join q4", joined(&medium, "id5", JoinType::Inner)),
        ("join q5", joined(&big, "id3", JoinType::Inner)),
    ];
    for (question, plan) in questions {
        time_streaming(question, &plan)?;
    }
    Ok(())
}

/// A table's frame with every factor column replaced by its codes.
fn codes(table: Table) -> Result<DataFrame> {
    let mut frame = table.into_frame();
    let factors: Vec<_> = frame
        .columns()
        .iter()
        .filter(|column| factor::is_factor(column.dtype()))
        .map(|column| {
            column
                .as_materialized_series()
                .to_storage()
                .clone()
                .into_column()
        })
        .collect();
    for column in factors {
        frame.with_column(column)?;
    }
    Ok(frame)
}

/// Times one streaming plan twice and prints the lines [`time_question`] prints. The checksum
/// sums the integer and double columns; code columns are not values and stay out of it.
fn time_streaming(question: &str, plan: &LazyFrame) -> Result<()> {
    for run_number in 1..=2 {
        let start = Instant::now();
        let frame = plan
            .clone()
            .collect_with_engine(Engine::Streaming)?
            .unwrap_single();
        let seconds = start.elapsed().as_secs_f64();
        let mut checksum = 0.0;
        for column in frame.columns() {
            let series = column.as_materialized_series();
            match series.dtype() {
                DataType::Int32 => {
                    checksum += series
                        .i32()?
                        .downcast_iter()
                        .flat_map(|array| array.iter().flatten())
                        .map(|&value| f64::from(value))
                        .sum::<f64>();
                }
                DataType::Float64 => checksum += sum_without_na(series.f64()?),
                _ => {}
            }
        }
        println!(
            "{question}\t{run_number}\t{seconds:.4}\t{}\t{}\t{checksum:.6e}",
            frame.height(),
            frame.width()
        );
    }
    Ok(())
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
                // Each value widens as it is added. Casting a whole integer column of 1e8 rows
                // first would hold 0.8 GB beside the answer.
                let series = frame.column(name)?.as_materialized_series();
                match kind {
                    ColumnType::Integer => {
                        checksum += series
                            .i32()?
                            .downcast_iter()
                            .flat_map(|array| array.iter().flatten())
                            .map(|&value| f64::from(value))
                            .sum::<f64>();
                    }
                    ColumnType::Double => checksum += sum_without_na(series.f64()?),
                    _ => {}
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
