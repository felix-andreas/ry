//! File reading and writing and the data.table functions beyond `[`, run on the ry engine. It
//! prints the same tab-separated lines as `tools/bench/table/ops.R` (`question  run  seconds
//! rows  cols  checksum`), so the two outputs line up row by row. Every question runs twice, and
//! an input a question modifies in place is forked outside the clock.
//!
//! Usage: `cargo bench -p table --bench ops_benchmark -- <data-dir>`, where the directory holds
//! `groupby.csv` from `tools/bench/table/datagen.R`.

use {
    std::{
        env,
        path::{Path, PathBuf},
        process::ExitCode,
        time::Instant,
    },
    table::{
        ColumnType, Table, io, ops,
        polars::prelude::{Column, DataType, Float64Chunked, IntoColumn},
        query::{Aggregate, Output},
    },
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let Some(dir) = env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"))
    else {
        eprintln!("usage: ops_benchmark <data-dir>");
        return ExitCode::FAILURE;
    };
    match run(&PathBuf::from(dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ops_benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What a question gives back, reduced the way `ops.R` reduces it.
enum Answer {
    Table(Table),
    Vector(Column),
    Columns(Vec<Column>),
    Count(usize),
}

fn run(dir: &Path) -> Result<()> {
    println!(
        "# ry table engine on polars 0.55, {} threads",
        rayon_threads()
    );
    let path = dir.join("groupby.csv");
    let out = dir.join("ops_written.csv");
    let mut x = Table::default();
    time_question("fread", || {
        x = io::read_csv(&path)?;
        Ok(Answer::Table(x.clone()))
    })?;
    time_question("fwrite", || {
        io::write_csv(&x, &out)?;
        Ok(Answer::Table(x.clone()))
    })?;
    in_place("setkey", &x, |y| Ok(y.set_key(&["id1", "id4"])?))?;
    in_place("setorder", &x, |y| {
        Ok(ops::set_order(y, &[("id3", false), ("v3", true)])?)
    })?;
    in_place("setnames", &x, |y| {
        Ok(ops::set_names(y, &[("id1", "first"), ("v1", "value")])?)
    })?;
    in_place("setcolorder", &x, |y| {
        Ok(ops::set_column_order(y, &["v3", "v2"])?)
    })?;
    time_question("unique", || {
        Ok(Answer::Table(ops::unique_rows(&x, &["id1", "id2"])?))
    })?;
    time_question("uniqueN", || {
        Ok(Answer::Count(ops::unique_count(
            &x,
            &["id1", "id2", "id4"],
        )?))
    })?;
    time_question("rbindlist", || {
        Ok(Answer::Table(ops::rbind_list(
            &[&x, &x],
            ops::Bind::Position,
        )?))
    })?;
    time_question("melt", || {
        let executed = ops::melt(
            &x,
            &["id1", "id2", "id3"],
            &["v1", "v2", "v3"],
            "variable",
            "value",
        )?;
        table_answer(executed.output)
    })?;
    time_question("dcast", || {
        let executed = ops::dcast(&x, &["id1"], "id4", "v1", Some(Aggregate::Sum))?;
        table_answer(executed.output)
    })?;
    let column = |name: &str| -> Result<Column> { Ok(x.frame().column(name)?.clone()) };
    let v1 = column("v1")?;
    let v3 = column("v3")?;
    time_question("shift", || Ok(Answer::Vector(ops::shift(&v1, 1))))?;
    let test = ops::between(&v1, 3.0, f64::INFINITY, true)?;
    let negated = (v3.as_materialized_series() * -1.0).into_column();
    time_question("fifelse", || {
        Ok(Answer::Vector(ops::fifelse(&test, &v3, &negated, None)?))
    })?;
    let missing = Column::full_null("na".into(), 1, &DataType::Float64);
    let gappy = ops::fifelse(&test, &v3, &missing, None)?;
    let fallback = column("v2")?.cast(&DataType::Float64)?;
    time_question("fcoalesce", || {
        Ok(Answer::Vector(ops::fcoalesce(&[&gappy, &fallback])?))
    })?;
    time_question("nafill", || {
        Ok(Answer::Vector(ops::nafill(&gappy, ops::Fill::Forward)?))
    })?;
    let id4 = column("id4")?;
    time_question("rleid", || Ok(Answer::Vector(ops::rleid(&[&id4])?)))?;
    time_question("between", || {
        Ok(Answer::Vector(ops::between(&v3, 25.0, 75.0, true)?))
    })?;
    let id1 = column("id1")?;
    time_question("chin", || {
        Ok(Answer::Vector(ops::chin(
            &id1,
            &[Some("id001"), Some("id002"), Some("id003")],
        )?))
    })?;
    let id3 = column("id3")?;
    time_question("tstrsplit", || {
        Ok(Answer::Columns(ops::tstrsplit(&id3, "d")?))
    })?;
    std::fs::remove_file(&out)?;
    Ok(())
}

fn table_answer(output: Output) -> Result<Answer> {
    match output {
        Output::Table(table) => Ok(Answer::Table(table)),
        _ => Err("the question gave no table".into()),
    }
}

/// Times an in-place function on a fresh fork of `x`, as `ops.R` times it on a `copy(x)`.
fn in_place(
    question: &str,
    x: &Table,
    mut run: impl FnMut(&mut Table) -> Result<()>,
) -> Result<()> {
    for run_number in 1..=2 {
        let mut y = x.fork();
        let start = Instant::now();
        run(&mut y)?;
        let seconds = start.elapsed().as_secs_f64();
        report(question, run_number, seconds, Answer::Table(y))?;
    }
    Ok(())
}

fn time_question(question: &str, mut run: impl FnMut() -> Result<Answer>) -> Result<()> {
    for run_number in 1..=2 {
        let start = Instant::now();
        let answer = run()?;
        let seconds = start.elapsed().as_secs_f64();
        report(question, run_number, seconds, answer)?;
    }
    Ok(())
}

fn report(question: &str, run: usize, seconds: f64, answer: Answer) -> Result<()> {
    let (rows, cols, checksum) = match answer {
        Answer::Table(table) => {
            let frame = table.frame();
            let mut checksum = 0.0;
            for (name, kind) in &table.schema().columns {
                if matches!(kind, ColumnType::Integer | ColumnType::Double) {
                    checksum += numeric_sum(frame.column(name)?)?;
                }
            }
            (frame.height(), frame.width(), checksum)
        }
        Answer::Vector(column) => {
            let checksum = if column.dtype() == &DataType::String {
                0.0
            } else {
                numeric_sum(&column)?
            };
            (column.len(), 1, checksum)
        }
        Answer::Columns(columns) => (
            columns.first().map_or(0, Column::len),
            columns.len(),
            columns.len() as f64,
        ),
        Answer::Count(count) => (1, 1, count as f64),
    };
    println!("{question}\t{run}\t{seconds:.4}\t{rows}\t{cols}\t{checksum:.6e}");
    Ok(())
}

/// `sum(as.numeric(x), na.rm = TRUE)`, which drops `NaN` as well as `NA`.
fn numeric_sum(column: &Column) -> Result<f64> {
    let values = column.cast(&DataType::Float64)?;
    let values: &Float64Chunked = values.f64()?;
    Ok(values
        .downcast_iter()
        .flat_map(|array| array.iter().flatten().copied())
        .filter(|value| !value.is_nan())
        .sum())
}

fn rayon_threads() -> usize {
    rayon::current_num_threads()
}
