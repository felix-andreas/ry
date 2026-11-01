//! The Polars-style verbs and the lazy planner, driven from Rust. Expected layouts (join column
//! order, suffixes, group order) follow Polars, which the `DataFrame` API mirrors.

use {
    syntax::testing::{SplitMix64, fuzz_iterations},
    table::{
        Table,
        convert::{RVector, is_na_real, to_r},
        lazy::{self, How, Plan, Source, Step, Strategy},
        polars::prelude::PlSmallStr,
        query::{
            Aggregate, BinaryOp, Expr, SortKey,
            build::{aggregate, binary, column},
        },
    },
};

fn frame(columns: Vec<(&str, RVector)>) -> Table {
    Table::from_r(
        columns
            .into_iter()
            .map(|(name, vector)| (PlSmallStr::from(name), vector))
            .collect(),
    )
    .unwrap_or_else(|error| panic!("cannot build the frame: {error}"))
}

fn ints(values: &[i32]) -> RVector {
    RVector::Integer(values.to_vec())
}

fn doubles(values: &[f64]) -> RVector {
    RVector::Double(values.to_vec())
}

/// A double column with `NA` as `None`, because `NA` is a NaN and never equals itself.
fn na_doubles(table: &Table, name: &str) -> Vec<Option<f64>> {
    match column_values(table, name) {
        RVector::Double(values) => values
            .into_iter()
            .map(|value| (!is_na_real(value)).then_some(value))
            .collect(),
        other => panic!("`{name}` is not a double column: {other:?}"),
    }
}

fn strings(values: &[&str]) -> RVector {
    RVector::Character(values.iter().map(|value| Some(value.to_string())).collect())
}

fn named(name: &str, expr: Expr) -> (PlSmallStr, Expr) {
    (PlSmallStr::from(name), expr)
}

fn collect(plan: Plan) -> Table {
    lazy::collect(plan)
        .unwrap_or_else(|error| panic!("the plan failed: {error}"))
        .table
}

fn column_values(table: &Table, name: &str) -> RVector {
    let column = table
        .frame()
        .column(name)
        .unwrap_or_else(|error| panic!("no column `{name}`: {error}"));
    to_r(column.as_materialized_series()).unwrap_or_else(|error| panic!("{error}"))
}

fn names(table: &Table) -> Vec<String> {
    table
        .frame()
        .get_column_names()
        .iter()
        .map(|name| name.to_string())
        .collect()
}

fn sales() -> Table {
    frame(vec![
        ("region", strings(&["eu", "us", "eu", "asia", "us"])),
        ("units", ints(&[3, 5, 2, 7, 1])),
        ("price", doubles(&[1.5, 2.0, 3.0, 0.5, 4.0])),
    ])
}

fn sum(name: &str) -> Expr {
    aggregate(Aggregate::Sum, column(name))
}

#[test]
fn verbs_compose_eagerly_and_lazily() {
    let plan = Plan::new(Source::Table(sales()))
        .with(Step::Filter(binary(
            BinaryOp::Greater,
            column("units"),
            Expr::Integer(1),
        )))
        .with(Step::WithColumns(vec![named(
            "revenue",
            binary(BinaryOp::Multiply, column("units"), column("price")),
        )]))
        .with(Step::Aggregate {
            keys: vec![named("region", column("region"))],
            aggregates: vec![named("revenue", sum("revenue")), named("n", Expr::Count)],
        })
        .with(Step::Sort(vec![SortKey {
            expr: column("revenue"),
            descending: true,
        }]));
    let result = collect(plan);
    assert_eq!(names(&result), ["region", "revenue", "n"]);
    assert_eq!(
        column_values(&result, "region"),
        strings(&["eu", "us", "asia"])
    );
    assert_eq!(
        column_values(&result, "revenue"),
        doubles(&[10.5, 10.0, 3.5])
    );
    assert_eq!(column_values(&result, "n"), ints(&[2, 1, 1]));
}

#[test]
fn agg_rejects_a_per_row_expression() {
    let plan = Plan::new(Source::Table(sales())).with(Step::Aggregate {
        keys: vec![named("region", column("region"))],
        aggregates: vec![named("units", column("units"))],
    });
    let error = lazy::collect(plan).err().map(|error| error.to_string());
    assert_eq!(
        error.as_deref(),
        Some("invalid query: `units` gives one value per row, but `agg` needs one value per group")
    );
}

#[test]
fn joins_follow_polars_layout() {
    let left = frame(vec![
        ("id", ints(&[1, 2, 3])),
        ("name", strings(&["a", "b", "c"])),
        ("score", doubles(&[1.0, 2.0, 3.0])),
    ]);
    let right = frame(vec![
        ("score", doubles(&[10.0, 30.0, 31.0])),
        ("id", ints(&[1, 3, 3])),
    ]);
    let join = |how| {
        collect(Plan::new(Source::Table(left.clone())).with(Step::Join {
            other: Box::new(Plan::new(Source::Table(right.clone()))),
            left_on: vec!["id".into()],
            right_on: vec!["id".into()],
            how,
            suffix: "_right".into(),
        }))
    };
    let inner = join(How::Inner);
    assert_eq!(names(&inner), ["id", "name", "score", "score_right"]);
    assert_eq!(column_values(&inner, "id"), ints(&[1, 3, 3]));
    assert_eq!(
        column_values(&inner, "score_right"),
        doubles(&[10.0, 30.0, 31.0])
    );
    let left_join = join(How::Left);
    assert_eq!(column_values(&left_join, "id"), ints(&[1, 2, 3, 3]));
    assert_eq!(names(&join(How::Semi)), ["id", "name", "score"]);
    assert_eq!(column_values(&join(How::Anti), "id"), ints(&[2]));
    assert_eq!(join(How::Full).height(), 4);
}

#[test]
fn an_asof_join_takes_the_nearest_row_within_each_by_group() {
    let trades = frame(vec![
        ("ticker", strings(&["a", "b", "a", "c"])),
        ("time", ints(&[5, 3, 1, 9])),
    ]);
    let quotes = frame(vec![
        ("ticker", strings(&["a", "a", "b", "b"])),
        ("time", ints(&[2, 4, 1, 6])),
        ("bid", doubles(&[1.5, 1.6, 7.0, 7.5])),
    ]);
    let join = |strategy| {
        collect(Plan::new(Source::Table(trades.clone())).with(Step::Join {
            other: Box::new(Plan::new(Source::Table(quotes.clone()))),
            left_on: vec!["ticker".into(), "time".into()],
            right_on: vec!["ticker".into(), "time".into()],
            how: How::Asof(strategy),
            suffix: "_right".into(),
        }))
    };
    let backward = join(Strategy::Backward);
    assert_eq!(names(&backward), ["ticker", "time", "bid"]);
    assert_eq!(column_values(&backward, "time"), ints(&[5, 3, 1, 9]));
    assert_eq!(
        na_doubles(&backward, "bid"),
        [Some(1.6), Some(7.0), None, None]
    );
    assert_eq!(
        na_doubles(&join(Strategy::Forward), "bid"),
        [None, Some(7.5), Some(1.5), None]
    );
    assert_eq!(
        na_doubles(&join(Strategy::Nearest), "bid"),
        [Some(1.6), Some(7.0), Some(1.5), None]
    );
}

#[test]
fn head_in_an_aggregation_keeps_each_groups_best_rows_after_a_sort() {
    let table = frame(vec![
        ("g", strings(&["p", "q", "p", "p", "q"])),
        ("v", doubles(&[3.0, 9.0, 7.0, 1.0, 2.0])),
    ]);
    let result = collect(
        Plan::new(Source::Table(table))
            .with(Step::Sort(vec![SortKey {
                expr: column("v"),
                descending: true,
            }]))
            .with(Step::Aggregate {
                keys: vec![("g".into(), column("g"))],
                aggregates: vec![("v".into(), Expr::Head(Box::new(column("v")), 2))],
            }),
    );
    assert_eq!(column_values(&result, "g"), strings(&["q", "q", "p", "p"]));
    assert_eq!(column_values(&result, "v"), doubles(&[9.0, 2.0, 7.0, 3.0]));
}

#[test]
fn the_optimizer_pushes_filters_and_projections() {
    let directory = std::env::temp_dir().join(format!("ry-lazy-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
    let path = directory.join("sales.csv");
    std::fs::write(
        &path,
        "region,units,price,note\neu,3,1.5,x\nus,5,2,y\neu,2,3,z\n",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let plan = Plan::scan_csv(&path)
        .with(Step::WithColumns(vec![named(
            "double_price",
            binary(BinaryOp::Multiply, column("price"), Expr::Double(2.0)),
        )]))
        .with(Step::Filter(binary(
            BinaryOp::Greater,
            column("units"),
            Expr::Integer(2),
        )))
        .with(Step::Select(vec![
            named("region", column("region")),
            named("double_price", column("double_price")),
        ]));
    let explained = lazy::explain(plan.clone()).unwrap_or_else(|error| panic!("{error}"));
    let expected = format!(
        "SELECT [col(\"region\"), col(\"double_price\")]\n  WITH_COLUMNS [(col(\"price\") * 2.0).alias(\"double_price\")]\n    FILTER col(\"units\") > 2L\n      SCAN CSV \"{}\" COLUMNS [\"region\", \"units\", \"price\"]\n",
        path.display()
    );
    assert_eq!(explained, expected);
    let result = collect(plan);
    assert_eq!(column_values(&result, "double_price"), doubles(&[3.0, 4.0]));

    let head = Plan::scan_csv(&path).with(Step::Head(2));
    let explained = lazy::explain(head.clone()).unwrap_or_else(|error| panic!("{error}"));
    assert!(explained.ends_with("ROWS 2\n"), "{explained}");
    assert_eq!(collect(head).height(), 2);
    std::fs::remove_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
}

/// Random chains of verbs give the same table with and without the optimizer.
#[test]
fn optimizing_never_changes_the_result() {
    let mut rng = SplitMix64(0x5eed_1a2f);
    for _ in 0..fuzz_iterations(200) {
        let height = rng.below(12);
        let groups = ["a", "b", "c"];
        let table = frame(vec![
            (
                "g",
                RVector::Character(
                    (0..height)
                        .map(|_| Some(groups[rng.below(3)].to_string()))
                        .collect(),
                ),
            ),
            (
                "x",
                RVector::Integer((0..height).map(|_| rng.below(10) as i32).collect()),
            ),
            (
                "y",
                RVector::Double((0..height).map(|_| rng.below(100) as f64 / 4.0).collect()),
            ),
        ]);
        let other = frame(vec![
            ("g", strings(&["a", "c"])),
            ("w", doubles(&[1.0, 2.0])),
        ]);
        let mut plan = Plan::new(Source::Table(table));
        let mut has_w = false;
        for _ in 0..rng.below(6) {
            let step = match rng.below(7) {
                0 => Step::Filter(binary(
                    BinaryOp::Greater,
                    column("x"),
                    Expr::Integer(rng.below(10) as i32),
                )),
                1 => Step::Sort(vec![SortKey {
                    expr: column(if rng.chance(1, 2) { "x" } else { "y" }),
                    descending: rng.chance(1, 2),
                }]),
                2 => Step::WithColumns(vec![named(
                    "y",
                    binary(BinaryOp::Add, column("y"), Expr::Double(1.0)),
                )]),
                3 => Step::Head(rng.below(8)),
                4 => Step::Tail(rng.below(8)),
                5 if !has_w => {
                    has_w = true;
                    Step::Join {
                        other: Box::new(Plan::new(Source::Table(other.clone()))),
                        left_on: vec!["g".into()],
                        right_on: vec!["g".into()],
                        how: [How::Inner, How::Left, How::Semi, How::Anti][rng.below(4)],
                        suffix: "_right".into(),
                    }
                }
                _ => Step::Filter(binary(
                    BinaryOp::LessEqual,
                    column("y"),
                    aggregate(Aggregate::Mean, column("y")),
                )),
            };
            plan = plan.with(step);
        }
        let optimized =
            lazy::collect(plan.clone()).map(|collected| collected.table.frame().clone());
        let plain = lazy::collect_unoptimized(plan.clone())
            .map(|collected| collected.table.frame().clone());
        match (optimized, plain) {
            (Ok(optimized), Ok(plain)) => assert!(
                optimized.equals_missing(&plain),
                "{plan:?}\noptimized:\n{optimized}\nplain:\n{plain}"
            ),
            (optimized, plain) => assert_eq!(
                optimized.is_ok(),
                plain.is_ok(),
                "{plan:?}: {:?} vs {:?}",
                optimized.err(),
                plain.err()
            ),
        }
    }
}
