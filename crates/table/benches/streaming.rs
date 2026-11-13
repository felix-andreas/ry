//! The group-by and join questions of `db_benchmark` as plain Polars plans on Polars' streaming
//! engine, which the library itself does not use. The bench has it only when it is built with
//! `--features polars-streaming`, so that every other build of the crate, tests included, links
//! the Polars the library ships with.

use {
    super::{Only, Result, polars_threads, read, sum_without_na},
    std::{path::Path, time::Instant},
    table::{
        Table, factor,
        polars::prelude::{
            DataFrame, DataType, Engine, IntoColumn, IntoLazy, JoinArgs, JoinType, LazyFrame,
            SortMultipleOptions, col, len, lit, pearson_corr,
        },
    },
};

/// The group-by and join questions as plain Polars plans on Polars' streaming engine.
///
/// The tables are read by ry's reader and stay in memory, as in the other runs. A factor column
/// is handed to Polars as its codes, so Polars groups and joins on small integers and never sees
/// a label: the comparison is between ry's kernels and the streaming engine's operators, not
/// between ry's factors and Polars' strings. For the join on the factor `id5`, the right table's
/// column is recoded to the left table's levels before the clock starts, and a label the left
/// table lacks becomes a null, which matches nothing in an inner join.
pub fn run(dir: &Path, only: Option<Only>) -> Result<()> {
    println!(
        "# polars 0.55 streaming engine, {} threads",
        polars_threads()
    );
    if only != Some(Only::Joins) {
        let x = codes(read(dir, "groupby.csv", true)?)?.lazy();
        let by = |names: &[&str]| {
            x.clone()
                .group_by(names.iter().map(|&name| col(name)).collect::<Vec<_>>())
        };
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
                    // The count is an integer column in ry's answer, and the checksum sums it.
                    .agg([col("v3").sum(), len().cast(DataType::Int32).alias("count")]),
            ),
        ];
        for (question, plan) in questions {
            time(question, &plan)?;
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
        time(question, &plan)?;
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

/// Times one plan twice and prints the lines the kernel run prints for a question. The checksum
/// sums the integer and double columns, as the kernel run does. A factor's codes are unsigned, so
/// they stay out of it, as the factor does there.
fn time(question: &str, plan: &LazyFrame) -> Result<()> {
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
