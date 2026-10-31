//! The table benchmark's questions, run on the ry engine through its query IR. It prints the same
//! tab-separated lines as `tools/bench/table/datatable.R` (`question  run  seconds  rows  cols
//! checksum`), so the two outputs line up row by row. Loading is not timed, and every question
//! runs twice.
//!
//! Usage: `cargo bench -p table --bench db_benchmark -- <data-dir>`, where the directory holds the
//! CSV files `tools/bench/table/datagen.R` writes.

use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use table::{
    ColumnType, Table,
    convert::string_to_factor,
    polars::prelude::{
        ChunkAgg, Column, CsvReadOptions, DataFrame, DataType, Float64Chunked, IntoColumn,
        SerReader,
    },
    query::{
        Aggregate, BinaryOp, Expr, Join, Mult, Output, Query, Roll, Rows, Select, SortKey,
        build::{aggregate, binary, column},
        execute,
    },
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let Some(dir) = env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"))
    else {
        eprintln!("usage: db_benchmark <data-dir>");
        return ExitCode::FAILURE;
    };
    match run(&PathBuf::from(dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("db_benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &Path) -> Result<()> {
    println!(
        "# ry table engine on polars 0.55, {} threads",
        polars_threads()
    );
    groupby(dir)?;
    joins(dir)?;
    rolling_join(dir)
}

fn groupby(dir: &Path) -> Result<()> {
    let mut x = read(dir, "groupby.csv")?;
    let sum = |name: &str| aggregate(Aggregate::Sum, column(name));
    let mean = |name: &str| aggregate(Aggregate::Mean, column(name));
    let by = |names: &[&str]| {
        names
            .iter()
            .map(|name| named(name, column(name)))
            .collect::<Vec<_>>()
    };
    let questions = [
        (
            "groupby q1",
            Query {
                select: columns([("v1", sum("v1"))]),
                by: by(&["id1"]),
                ..Query::default()
            },
        ),
        (
            "groupby q2",
            Query {
                select: columns([("v1", sum("v1"))]),
                by: by(&["id1", "id2"]),
                ..Query::default()
            },
        ),
        (
            "groupby q3",
            Query {
                select: columns([("v1", sum("v1")), ("v3", mean("v3"))]),
                by: by(&["id3"]),
                ..Query::default()
            },
        ),
        (
            "groupby q4",
            Query {
                select: columns([("v1", mean("v1")), ("v2", mean("v2")), ("v3", mean("v3"))]),
                by: by(&["id4"]),
                ..Query::default()
            },
        ),
        (
            "groupby q5",
            Query {
                select: columns([("v1", sum("v1")), ("v2", sum("v2")), ("v3", sum("v3"))]),
                by: by(&["id6"]),
                ..Query::default()
            },
        ),
        (
            "groupby q6",
            Query {
                select: columns([
                    ("median_v3", aggregate(Aggregate::Median, column("v3"))),
                    ("sd_v3", aggregate(Aggregate::Sd, column("v3"))),
                ]),
                by: by(&["id4", "id5"]),
                ..Query::default()
            },
        ),
        (
            "groupby q7",
            Query {
                select: columns([(
                    "range_v1_v2",
                    binary(
                        BinaryOp::Subtract,
                        aggregate(Aggregate::Max, column("v1")),
                        aggregate(Aggregate::Min, column("v2")),
                    ),
                )]),
                by: by(&["id3"]),
                ..Query::default()
            },
        ),
        (
            "groupby q8",
            Query {
                rows: Rows::Order(vec![SortKey {
                    expr: column("v3"),
                    descending: true,
                }]),
                select: columns([("largest2_v3", Expr::Head(Box::new(column("v3")), 2))]),
                by: by(&["id6"]),
                ..Query::default()
            },
        ),
        (
            "groupby q9",
            Query {
                select: columns([(
                    "r2",
                    binary(
                        BinaryOp::Power,
                        Expr::Correlation(Box::new(column("v1")), Box::new(column("v2"))),
                        Expr::Double(2.0),
                    ),
                )]),
                by: by(&["id2", "id4"]),
                ..Query::default()
            },
        ),
        (
            "groupby q10",
            Query {
                select: columns([("v3", sum("v3")), ("count", Expr::Count)]),
                by: by(&["id1", "id2", "id3", "id4", "id5", "id6"]),
                ..Query::default()
            },
        ),
    ];
    for (question, query) in questions {
        time_question(question, || answer(execute(&mut x, query.clone())?))?;
    }

    time_question("update full", || {
        for _ in 0..100 {
            let update = binary(BinaryOp::Multiply, column("v3"), Expr::Double(1.000001));
            execute(&mut x, assign("v3", update))?;
        }
        answer(execute(
            &mut x,
            Query {
                select: columns([("v3", sum("v3"))]),
                ..Query::default()
            },
        )?)
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
        answer(execute(
            &mut x,
            Query {
                select: columns([("v1", sum("v1"))]),
                ..Query::default()
            },
        )?)
    })?;
    time_question("update add", || {
        for step in 1..=100 {
            let update = binary(BinaryOp::Multiply, column("v2"), Expr::Integer(step));
            execute(&mut x, assign(&format!("added{step}"), update))?;
        }
        answer(execute(
            &mut x,
            Query {
                select: columns([("added100", sum("added100"))]),
                ..Query::default()
            },
        )?)
    })
}

fn joins(dir: &Path) -> Result<()> {
    let mut x = read(dir, "join_x.csv")?;
    let small = read(dir, "join_small.csv")?;
    let mut medium = read(dir, "join_medium.csv")?;
    let big = read(dir, "join_big.csv")?;
    let medium_frame = medium.clone();
    let x_frame = x.clone();
    let inner = |other: &Table, on: &str| Query {
        rows: equi_join(other, on, true),
        ..Query::default()
    };
    time_question("join q1", || answer(execute(&mut x, inner(&small, "id1"))?))?;
    time_question("join q2", || {
        answer(execute(&mut x, inner(&medium_frame, "id2"))?)
    })?;
    time_question("join q3", || {
        answer(execute(
            &mut medium,
            Query {
                rows: equi_join(&x_frame, "id2", false),
                ..Query::default()
            },
        )?)
    })?;
    time_question("join q4", || {
        answer(execute(&mut x, inner(&medium_frame, "id5"))?)
    })?;
    time_question("join q5", || answer(execute(&mut x, inner(&big, "id3"))?))
}

fn rolling_join(dir: &Path) -> Result<()> {
    let mut prices = read(dir, "rolljoin_prices.csv")?;
    let trades = read(dir, "rolljoin_trades.csv")?;
    prices.set_key(&["ticker", "time"])?;
    time_question("rolljoin keyed", || {
        let rows = Rows::Join(Join {
            other: trades.clone(),
            on: vec![
                ("ticker".into(), "ticker".into()),
                ("time".into(), "time".into()),
            ],
            drop_unmatched: false,
            roll: Roll::Backward(None),
            mult: Mult::All,
        });
        answer(execute(
            &mut prices,
            Query {
                rows,
                ..Query::default()
            },
        )?)
    })
}

/// Reads a CSV file the way `fread(stringsAsFactors = TRUE)` does: strings become factors, and
/// whole numbers that fit become integers rather than doubles.
fn read(dir: &Path, name: &str) -> Result<Table> {
    let frame = CsvReadOptions::default()
        .with_has_header(true)
        .try_into_reader_with_file_path(Some(dir.join(name)))?
        .finish()?;
    let mut columns: Vec<Column> = Vec::with_capacity(frame.width());
    for column in frame.columns() {
        let series = column.as_materialized_series();
        let series = match series.dtype() {
            DataType::String => string_to_factor(series)?,
            DataType::Int64 => {
                let values = series.i64()?;
                let fits = |value: Option<i64>| {
                    value.is_none_or(|value| {
                        value > i64::from(i32::MIN) && value <= i64::from(i32::MAX)
                    })
                };
                if fits(values.min()) && fits(values.max()) {
                    series.cast(&DataType::Int32)?
                } else {
                    series.clone()
                }
            }
            _ => series.clone(),
        };
        columns.push(series.into_column());
    }
    Ok(Table::new(DataFrame::new(frame.height(), columns)?)?)
}

fn time_question(question: &str, mut run: impl FnMut() -> Result<Answer>) -> Result<()> {
    for run_number in 1..=2 {
        let start = Instant::now();
        let answer = run()?;
        let seconds = start.elapsed().as_secs_f64();
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

fn named(name: &str, expr: Expr) -> (table::polars::prelude::PlSmallStr, Expr) {
    (name.into(), expr)
}

fn columns<const N: usize>(columns: [(&str, Expr); N]) -> Select {
    Select::Columns(
        columns
            .into_iter()
            .map(|(name, expr)| named(name, expr))
            .collect(),
    )
}

fn assign(name: &str, expr: Expr) -> Query {
    Query {
        select: Select::Assign(vec![(name.into(), Some(expr))]),
        ..Query::default()
    }
}

fn equi_join(other: &Table, on: &str, drop_unmatched: bool) -> Rows {
    Rows::Join(Join {
        other: other.clone(),
        on: vec![(on.into(), on.into())],
        drop_unmatched,
        roll: Roll::None,
        mult: Mult::All,
    })
}

/// Polars sizes its thread pool from `POLARS_MAX_THREADS`, or else from the available cores.
fn polars_threads() -> usize {
    env::var("POLARS_MAX_THREADS")
        .ok()
        .and_then(|threads| threads.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
}
