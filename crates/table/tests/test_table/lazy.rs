//! The Polars-style verbs and the lazy planner, driven from Rust. Expected layouts (join column
//! order, suffixes, group order) follow Polars, which the `DataFrame` API mirrors.

use {
    syntax::testing::{SplitMix64, for_each_seed},
    table::{
        ColumnType, Table,
        convert::{RVector, is_na_real, to_r},
        lazy::{self, How, Plan, Source, Step, Strategy},
        polars::prelude::PlSmallStr,
        query::{
            Aggregate, BinaryOp, Expr, Function, SortKey,
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

/// A missing `by` key is a group of its own, so it rolls to the right rows whose key is missing,
/// as data.table's rolling join does, while an empty string stays apart from it.
#[test]
fn an_asof_join_matches_a_missing_by_key_with_a_missing_key() {
    let trades = frame(vec![
        (
            "ticker",
            RVector::Character(vec![Some("a".to_string()), None, Some(String::new())]),
        ),
        ("time", ints(&[5, 3, 4])),
    ]);
    let quotes = frame(vec![
        (
            "ticker",
            RVector::Character(vec![Some("a".to_string()), None]),
        ),
        ("time", ints(&[2, 1])),
        ("bid", doubles(&[1.5, 7.0])),
    ]);
    let joined = collect(Plan::new(Source::Table(trades)).with(Step::Join {
        other: Box::new(Plan::new(Source::Table(quotes))),
        left_on: vec!["ticker".into(), "time".into()],
        right_on: vec!["ticker".into(), "time".into()],
        how: How::Asof(Strategy::Backward),
        suffix: "_right".into(),
    }));
    assert_eq!(names(&joined), ["ticker", "time", "bid"]);
    assert_eq!(na_doubles(&joined, "bid"), [Some(1.5), Some(7.0), None]);
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
    for_each_seed(200, |seed| {
        let mut rng = SplitMix64(seed);
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
    });
}

#[test]
fn a_window_gives_each_row_its_groups_value() {
    let plan = Plan::new(Source::Table(sales())).with(Step::WithColumns(vec![
        named(
            "region_units",
            Expr::Window(Box::new(sum("units")), vec![PlSmallStr::from("region")]),
        ),
        named(
            "price_rank",
            Expr::Rank {
                arg: Box::new(column("price")),
                descending: true,
            },
        ),
    ]));
    let result = collect(plan);
    assert_eq!(
        column_values(&result, "region_units"),
        ints(&[5, 6, 5, 7, 6])
    );
    assert_eq!(column_values(&result, "price_rank"), ints(&[4, 3, 2, 5, 1]));
}

/// Ties keep row order and `NA` ranks last, as `frank(x, ties.method = "first", na.last = TRUE)`.
#[test]
fn rank_breaks_ties_by_row_and_puts_na_last() {
    let table = frame(vec![(
        "x",
        doubles(&[2.0, table::convert::NA_REAL, 1.0, 2.0]),
    )]);
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![
        named(
            "up",
            Expr::Rank {
                arg: Box::new(column("x")),
                descending: false,
            },
        ),
        named(
            "down",
            Expr::Rank {
                arg: Box::new(column("x")),
                descending: true,
            },
        ),
    ]));
    let result = collect(plan);
    assert_eq!(column_values(&result, "up"), ints(&[2, 4, 1, 3]));
    assert_eq!(column_values(&result, "down"), ints(&[1, 4, 3, 2]));
}

#[test]
fn dates_shift_by_days_and_subtract_to_days() {
    let table = frame(vec![(
        "day",
        RVector::Date(vec![19753.0, 19768.0, table::convert::NA_REAL]),
    )]);
    let plan = Plan::new(Source::Table(table))
        .with(Step::WithColumns(vec![
            named(
                "later",
                binary(BinaryOp::Add, column("day"), Expr::Double(30.0)),
            ),
            named(
                "since",
                binary(BinaryOp::Subtract, column("day"), Expr::Date(19700)),
            ),
        ]))
        .with(Step::Filter(binary(
            BinaryOp::GreaterEqual,
            column("day"),
            Expr::Date(19760),
        )));
    let result = collect(plan);
    assert_eq!(column_values(&result, "day"), RVector::Date(vec![19768.0]));
    assert_eq!(
        column_values(&result, "later"),
        RVector::Date(vec![19798.0])
    );
    assert_eq!(column_values(&result, "since"), ints(&[68]));
}

/// A date takes only `+` and `-` of days and comparisons with another date. Without the second
/// check, a date against a number would meet at character and compare as text.
#[test]
fn a_date_rejects_arithmetic_and_comparisons_it_has_no_meaning_for() {
    let cases = [
        (
            Step::Select(vec![named(
                "twice",
                binary(BinaryOp::Multiply, column("day"), Expr::Integer(2)),
            )]),
            "a date only takes `+` and `-`",
        ),
        (
            Step::Filter(binary(
                BinaryOp::Greater,
                column("day"),
                Expr::Integer(19000),
            )),
            "a Date compares only with a Date, not with integer[]",
        ),
    ];
    for (step, expected) in cases {
        let table = frame(vec![("day", RVector::Date(vec![19753.0]))]);
        let plan = Plan::new(Source::Table(table)).with(step);
        let error = lazy::collect(plan).err().map(|error| error.to_string());
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains(expected)),
            "{expected}: {error:?}"
        );
    }
}

/// A filter that reads which rows come first must not move below the sort that orders them.
#[test]
fn a_filter_on_rank_stays_above_the_sort() {
    let plan = Plan::new(Source::Table(sales()))
        .with(Step::Sort(vec![SortKey {
            expr: column("units"),
            descending: false,
        }]))
        .with(Step::Filter(binary(
            BinaryOp::LessEqual,
            Expr::Rank {
                arg: Box::new(column("price")),
                descending: false,
            },
            Expr::Integer(2),
        )));
    let result = collect(plan);
    assert_eq!(column_values(&result, "units"), ints(&[3, 7]));
}

#[test]
fn a_cross_join_pairs_every_row_left_major() {
    let visits = frame(vec![("visit", ints(&[1, 2])), ("units", ints(&[0, 0]))]);
    let plan = Plan::new(Source::Table(sales())).with(Step::Join {
        other: Box::new(Plan::new(Source::Table(visits))),
        left_on: Vec::new(),
        right_on: Vec::new(),
        how: How::Cross,
        suffix: PlSmallStr::from("_right"),
    });
    let result = collect(plan);
    assert_eq!(
        names(&result),
        ["region", "units", "price", "visit", "units_right"]
    );
    assert_eq!(
        column_values(&result, "visit"),
        ints(&[1, 2, 1, 2, 1, 2, 1, 2, 1, 2])
    );
    assert_eq!(
        column_values(&result, "units"),
        ints(&[3, 3, 5, 5, 2, 2, 7, 7, 1, 1])
    );
}

/// Every kind of join matches a missing key with a missing key, as R's `merge()` does.
#[test]
fn every_kind_of_join_matches_a_missing_key_with_a_missing_key() {
    let left = frame(vec![
        ("id", ints(&[1, i32::MIN, 2])),
        ("name", strings(&["a", "missing", "b"])),
    ]);
    let right = frame(vec![
        ("id", ints(&[1, i32::MIN])),
        ("score", ints(&[10, 20])),
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
    assert_eq!(column_values(&join(How::Inner), "score"), ints(&[10, 20]));
    assert_eq!(
        column_values(&join(How::Left), "score"),
        ints(&[10, 20, i32::MIN])
    );
    assert_eq!(
        column_values(&join(How::Semi), "name"),
        strings(&["a", "missing"])
    );
    assert_eq!(column_values(&join(How::Anti), "name"), strings(&["b"]));
    assert_eq!(join(How::Full).height(), 3);
}

/// Two date-time keys match by instant whatever their zones, and an integer key matches a double
/// key by value; the joined key keeps the left side's zone.
#[test]
fn a_full_join_matches_keys_across_zones_and_number_types() {
    let instant = 1_704_110_400.0;
    let utc = frame(vec![
        (
            "t",
            RVector::Datetime {
                seconds: vec![instant],
                zone: Some("UTC".to_string()),
            },
        ),
        ("id", ints(&[1])),
    ]);
    let new_york = frame(vec![
        (
            "t",
            RVector::Datetime {
                seconds: vec![instant],
                zone: Some("America/New_York".to_string()),
            },
        ),
        ("id", doubles(&[1.0])),
    ]);
    let join = |on: &str| {
        collect(Plan::new(Source::Table(utc.clone())).with(Step::Join {
            other: Box::new(Plan::new(Source::Table(new_york.clone()))),
            left_on: vec![on.into()],
            right_on: vec![on.into()],
            how: How::Full,
            suffix: "_right".into(),
        }))
    };
    let by_time = join("t");
    assert_eq!(by_time.height(), 1);
    assert_eq!(
        column_values(&by_time, "t"),
        RVector::Datetime {
            seconds: vec![instant],
            zone: Some("UTC".to_string()),
        }
    );
    assert_eq!(join("id").height(), 1);
}

/// `count` counts the values that are not missing, `NaN` included among the missing, while
/// `len` counts every row; `na_rm = FALSE` makes a sum over a missing value missing.
#[test]
fn count_skips_missing_values_and_a_sum_can_keep_them() {
    let table = frame(vec![
        ("key", strings(&["a", "a", "b", "b"])),
        (
            "value",
            doubles(&[1.0, table::convert::NA_REAL, f64::NAN, 2.0]),
        ),
    ]);
    let plan = Plan::new(Source::Table(table)).with(Step::Aggregate {
        keys: vec![named("key", column("key"))],
        aggregates: vec![
            named("count", aggregate(Aggregate::Count, column("value"))),
            named("len", aggregate(Aggregate::Length, column("value"))),
            named("kept", sum("value")),
            named(
                "skipped",
                Expr::Aggregate {
                    function: Aggregate::Sum,
                    arg: Box::new(column("value")),
                    na_rm: true,
                },
            ),
        ],
    });
    let result = collect(plan);
    assert_eq!(column_values(&result, "count"), ints(&[1, 1]));
    assert_eq!(column_values(&result, "len"), ints(&[2, 2]));
    assert_eq!(na_doubles(&result, "kept")[0], None);
    assert_eq!(na_doubles(&result, "skipped"), [Some(1.0), Some(2.0)]);
}

/// A date-time keeps its zone, shifts by seconds, subtracts to seconds and compares with
/// another date-time.
#[test]
fn date_times_shift_by_seconds_and_keep_their_zone() {
    let table = frame(vec![(
        "at",
        RVector::Datetime {
            seconds: vec![1_700_000_000.0, 1_700_003_600.5, table::convert::NA_REAL],
            zone: Some("Europe/Berlin".to_string()),
        },
    )]);
    let plan = Plan::new(Source::Table(table))
        .with(Step::WithColumns(vec![
            named(
                "later",
                binary(BinaryOp::Add, column("at"), Expr::Double(60.0)),
            ),
            named(
                "since",
                binary(
                    BinaryOp::Subtract,
                    column("at"),
                    Expr::Datetime {
                        micros: 1_699_999_000_000_000,
                        zone: None,
                    },
                ),
            ),
        ]))
        .with(Step::Filter(binary(
            BinaryOp::Greater,
            column("at"),
            Expr::Datetime {
                micros: 1_700_000_000_000_000,
                zone: None,
            },
        )));
    let result = collect(plan);
    assert_eq!(
        column_values(&result, "at"),
        RVector::Datetime {
            seconds: vec![1_700_003_600.5],
            zone: Some("Europe/Berlin".to_string()),
        }
    );
    assert_eq!(
        column_values(&result, "later"),
        RVector::Datetime {
            seconds: vec![1_700_003_660.5],
            zone: Some("Europe/Berlin".to_string()),
        }
    );
    assert_eq!(column_values(&result, "since"), doubles(&[4_600.5]));
}

fn call(function: Function, args: Vec<Expr>) -> Expr {
    Expr::Call(function, args)
}

/// A date column with `NA` as `None`, because `NA` is a NaN and never equals itself.
fn na_dates(table: &Table, name: &str) -> Vec<Option<f64>> {
    match column_values(table, name) {
        RVector::Date(values) => values
            .into_iter()
            .map(|value| (!is_na_real(value)).then_some(value))
            .collect(),
        other => panic!("`{name}` is not a date column: {other:?}"),
    }
}

fn na_strings(values: &[Option<&str>]) -> RVector {
    RVector::Character(
        values
            .iter()
            .map(|value| value.map(str::to_owned))
            .collect(),
    )
}

#[test]
fn text_functions_keep_missing_values_missing() {
    let table = frame(vec![(
        "city",
        na_strings(&[Some("Oslo"), Some("Bergen"), None]),
    )]);
    let text = || Expr::Character("o".to_owned());
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![
        named(
            "has",
            call(
                Function::Contains { literal: false },
                vec![column("city"), Expr::Character("^B".to_owned())],
            ),
        ),
        named(
            "swapped",
            call(
                Function::Replace {
                    all: true,
                    literal: true,
                },
                vec![column("city"), text(), Expr::Character("0".to_owned())],
            ),
        ),
        named(
            "tail",
            call(
                Function::Slice {
                    start: -2,
                    length: None,
                },
                vec![column("city")],
            ),
        ),
        named(
            "joined",
            call(
                Function::ConcatStr {
                    separator: "-".into(),
                },
                vec![column("city"), Expr::Integer(1)],
            ),
        ),
        named(
            "either",
            call(
                Function::Coalesce,
                vec![column("city"), Expr::Character("none".to_owned())],
            ),
        ),
    ]));
    let result = collect(plan);
    assert_eq!(
        column_values(&result, "has"),
        RVector::Logical(vec![0, 1, table::convert::NA_INTEGER])
    );
    assert_eq!(
        column_values(&result, "swapped"),
        na_strings(&[Some("Osl0"), Some("Bergen"), None])
    );
    assert_eq!(
        column_values(&result, "tail"),
        na_strings(&[Some("lo"), Some("en"), None])
    );
    assert_eq!(
        column_values(&result, "joined"),
        na_strings(&[Some("Oslo-1"), Some("Bergen-1"), None])
    );
    assert_eq!(
        column_values(&result, "either"),
        strings(&["Oslo", "Bergen", "none"])
    );
}

/// A cast to text prints a double with R's 15 significant digits, and a cast from text reads
/// it as R's `as.*` functions do.
#[test]
fn casts_convert_as_r_does() {
    let table = frame(vec![
        ("x", doubles(&[0.1 + 0.2, 1e-20, 100000.0, 123456.0])),
        ("text", strings(&["1.9", "T", "yes", "2024-02-29"])),
    ]);
    let cast = |name: &str, to| call(Function::Cast(to), vec![column(name)]);
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![
        named("shown", cast("x", ColumnType::Character)),
        named("whole", cast("text", ColumnType::Integer)),
        named("flag", cast("text", ColumnType::Logical)),
        named("day", cast("text", ColumnType::Date)),
    ]));
    let result = collect(plan);
    let na = table::convert::NA_INTEGER;
    assert_eq!(
        column_values(&result, "shown"),
        strings(&["0.3", "1e-20", "1e+05", "123456"])
    );
    assert_eq!(column_values(&result, "whole"), ints(&[1, na, na, na]));
    assert_eq!(
        column_values(&result, "flag"),
        RVector::Logical(vec![na, 1, na, na])
    );
    assert_eq!(na_dates(&result, "day"), [None, None, None, Some(19782.0)]);
}

/// `shift`, `cum_sum` and the row index read the rows in order, so a filter above them must not
/// move below them.
#[test]
fn running_values_number_the_rows_after_a_filter() {
    let plan = Plan::new(Source::Table(sales()))
        .with(Step::Filter(binary(
            BinaryOp::Greater,
            column("units"),
            Expr::Integer(1),
        )))
        .with(Step::WithColumns(vec![
            named("row", call(Function::RowIndex(1), vec![])),
            named("previous", call(Function::Shift(1), vec![column("units")])),
            named("running", call(Function::CumSum, vec![column("units")])),
        ]))
        .with(Step::Filter(binary(
            BinaryOp::Less,
            column("row"),
            Expr::Integer(4),
        )));
    let result = collect(plan);
    let na = table::convert::NA_INTEGER;
    assert_eq!(column_values(&result, "row"), ints(&[1, 2, 3]));
    assert_eq!(column_values(&result, "units"), ints(&[3, 5, 2]));
    assert_eq!(column_values(&result, "previous"), ints(&[na, 3, 5]));
    assert_eq!(column_values(&result, "running"), ints(&[3, 8, 10]));
}

#[test]
fn n_unique_counts_missing_values_once() {
    let table = frame(vec![
        ("group", strings(&["a", "a", "a", "b"])),
        (
            "x",
            doubles(&[1.0, table::convert::NA_REAL, table::convert::NA_REAL, 2.0]),
        ),
    ]);
    let plan = Plan::new(Source::Table(table)).with(Step::Aggregate {
        keys: vec![named("group", column("group"))],
        aggregates: vec![named(
            "distinct",
            aggregate(Aggregate::NUnique, column("x")),
        )],
    });
    let result = collect(plan);
    assert_eq!(column_values(&result, "distinct"), ints(&[2, 1]));
}

#[test]
fn dates_split_shift_by_calendar_months_and_format() {
    let table = frame(vec![(
        "day",
        RVector::Date(vec![19753.0, table::convert::NA_REAL]),
    )]);
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![
        named("month", call(Function::Month, vec![column("day")])),
        named(
            "later",
            call(Function::OffsetBy { by: "1mo".into() }, vec![column("day")]),
        ),
        named(
            "shown",
            call(
                Function::Format {
                    format: "%d.%m.%Y".into(),
                },
                vec![column("day")],
            ),
        ),
    ]));
    let result = collect(plan);
    let na = table::convert::NA_INTEGER;
    assert_eq!(column_values(&result, "month"), ints(&[1, na]));
    assert_eq!(na_dates(&result, "later"), [Some(19782.0), None]);
    assert_eq!(
        column_values(&result, "shown"),
        na_strings(&[Some("31.01.2024"), None])
    );
}

fn failure(plan: Plan) -> String {
    match lazy::collect(plan) {
        Ok(_) => panic!("the plan should fail"),
        Err(error) => error.to_string(),
    }
}

/// R's integer `cumsum` gives `NA` from the first sum outside the integer range on.
#[test]
fn integer_cum_sum_is_missing_from_the_first_overflow_on() {
    let table = frame(vec![("x", ints(&[i32::MAX - 1, 1, 1, -5, 3]))]);
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![named(
        "running",
        call(Function::CumSum, vec![column("x")]),
    )]));
    let na = table::convert::NA_INTEGER;
    assert_eq!(
        column_values(&collect(plan), "running"),
        ints(&[i32::MAX - 1, i32::MAX, na, na, na])
    );
}

#[test]
fn factors_cast_to_their_codes_and_fill_by_their_labels() {
    let na = table::convert::NA_INTEGER;
    let levels = |labels: &[&str]| labels.iter().map(|label| label.to_string()).collect();
    let table = frame(vec![
        (
            "f",
            RVector::Factor {
                codes: vec![2, 1, na],
                levels: levels(&["10", "20"]),
            },
        ),
        (
            "g",
            RVector::Factor {
                codes: vec![1, 1, 2],
                levels: levels(&["a", "b"]),
            },
        ),
    ]);
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![
        named(
            "codes",
            call(Function::Cast(ColumnType::Integer), vec![column("f")]),
        ),
        named(
            "numbers",
            call(Function::Cast(ColumnType::Double), vec![column("f")]),
        ),
        named(
            "filled",
            call(Function::FillNull, vec![column("f"), column("g")]),
        ),
    ]));
    let result = collect(plan);
    assert_eq!(column_values(&result, "codes"), ints(&[2, 1, na]));
    assert_eq!(na_doubles(&result, "numbers"), [Some(2.0), Some(1.0), None]);
    let RVector::Factor { codes, levels } = column_values(&result, "filled") else {
        panic!("`fill_null` of two factors is not a factor");
    };
    let labels: Vec<&str> = codes
        .iter()
        .map(|&code| levels[usize::try_from(code - 1).unwrap_or(usize::MAX)].as_str())
        .collect();
    assert_eq!(labels, ["20", "10", "b"]);
}

#[test]
fn text_casts_to_numbers_as_as_numeric_reads_them() {
    let table = frame(vec![(
        "text",
        strings(&[" 1.5", "0x10", "1e3 ", "abc", "", "2.7"]),
    )]);
    let plan = Plan::new(Source::Table(table)).with(Step::Select(vec![
        named(
            "number",
            call(Function::Cast(ColumnType::Double), vec![column("text")]),
        ),
        named(
            "whole",
            call(Function::Cast(ColumnType::Integer), vec![column("text")]),
        ),
    ]));
    let result = collect(plan);
    let na = table::convert::NA_INTEGER;
    assert_eq!(
        na_doubles(&result, "number"),
        [Some(1.5), Some(16.0), Some(1000.0), None, None, Some(2.7)]
    );
    assert_eq!(
        column_values(&result, "whole"),
        ints(&[1, 16, 1000, na, na, 2])
    );
}

#[test]
fn bad_patterns_durations_and_fills_are_refused_by_name() {
    let table = frame(vec![
        ("city", strings(&["a"])),
        ("day", RVector::Date(vec![19753.0])),
        ("units", ints(&[1])),
    ]);
    let select = |expr: Expr| {
        Plan::new(Source::Table(table.clone())).with(Step::Select(vec![named("out", expr)]))
    };
    let pattern = failure(select(call(
        Function::Contains { literal: false },
        vec![column("city"), Expr::Character("(".into())],
    )));
    assert!(
        pattern.contains("`str_contains` cannot read the regular expression \"(\""),
        "{pattern}"
    );
    let offset = failure(select(call(
        Function::OffsetBy { by: "12h".into() },
        vec![column("day")],
    )));
    assert!(
        offset.contains("`dt_offset_by` moves a Date by whole days"),
        "{offset}"
    );
    let fill = failure(select(call(
        Function::Coalesce,
        vec![column("day"), column("units")],
    )));
    assert!(
        fill.contains("`coalesce` fills Date values only from values of the same type"),
        "{fill}"
    );
}
