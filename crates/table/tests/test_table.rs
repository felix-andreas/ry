//! The table engine, driven directly from Rust. Every expected value that encodes data.table
//! semantics (join layout, rolling matches, group order, `NA` handling) was read from data.table
//! 1.17 under GNU R on the same inputs.

use syntax::testing::{SplitMix64, fuzz_iterations};
use table::{
    ColumnType, Residency, Schema, Table,
    convert::{NA_INTEGER, NA_REAL, RVector, is_na_real, to_r, to_series},
    ffi,
    polars::prelude::{IntoColumn, PlSmallStr},
    query::{
        Aggregate, BinaryOp, Expr, Join, Output, Query, ResultType, Roll, Rows, Select, SortKey,
        UnaryOp,
        build::{aggregate, binary, column},
        execute, plan, result_type,
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

fn strings(values: &[&str]) -> RVector {
    RVector::Character(
        values
            .iter()
            .map(|value| (*value != "NA").then(|| value.to_string()))
            .collect(),
    )
}

fn get(table: &mut Table, name: &str) -> RVector {
    (*table
        .read_r(name)
        .unwrap_or_else(|error| panic!("no column {name}: {error}")))
    .clone()
}

/// A column read back from a table, compared bit for bit so that `NA_real_` equals itself and
/// differs from `NaN`, which `f64` equality cannot express.
#[derive(Debug)]
struct Read(RVector);

impl PartialEq<RVector> for Read {
    fn eq(&self, other: &RVector) -> bool {
        match (&self.0, other) {
            (RVector::Double(left), RVector::Double(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| left.to_bits() == right.to_bits())
            }
            (left, right) => left == right,
        }
    }
}

fn read(table: &mut Table, name: &str) -> Read {
    Read(get(table, name))
}

fn run(table: &mut Table, query: Query) -> Output {
    execute(table, query).unwrap_or_else(|error| panic!("query failed: {error}"))
}

fn run_table(table: &mut Table, query: Query) -> Table {
    match run(table, query) {
        Output::Table(result) => result,
        other => panic!("expected a table, got {other:?}"),
    }
}

fn names(table: &Table) -> Vec<String> {
    table
        .schema()
        .columns
        .iter()
        .map(|(name, _)| name.to_string())
        .collect()
}

fn set(name: &str, expr: Expr) -> (PlSmallStr, Option<Expr>) {
    (PlSmallStr::from(name), Some(expr))
}

fn named(name: &str, expr: Expr) -> (PlSmallStr, Expr) {
    (PlSmallStr::from(name), expr)
}

#[test]
fn na_and_nan_stay_distinct_across_the_boundary() {
    let values = vec![1.5, NA_REAL, f64::NAN, -0.0];
    let series = to_series("x".into(), RVector::Double(values.clone()))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(series.null_count(), 1, "only NA_real_ becomes a null");
    let RVector::Double(back) = to_r(&series).unwrap_or_else(|error| panic!("{error}")) else {
        panic!("not double")
    };
    assert_eq!(back[0], 1.5);
    assert!(is_na_real(back[1]), "the null comes back as NA_real_");
    assert!(
        back[2].is_nan() && !is_na_real(back[2]),
        "NaN stays NaN, not NA"
    );
    assert_eq!(back[3].to_bits(), (-0.0f64).to_bits());

    let series =
        to_series("i".into(), ints(&[1, NA_INTEGER, 3])).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(series.null_count(), 1);
    assert_eq!(
        to_r(&series).unwrap_or_else(|error| panic!("{error}")),
        ints(&[1, NA_INTEGER, 3])
    );

    let logical = RVector::Logical(vec![1, 0, NA_INTEGER]);
    let series = to_series("l".into(), logical.clone()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        to_r(&series).unwrap_or_else(|error| panic!("{error}")),
        logical
    );

    let factor = RVector::Factor {
        codes: vec![2, 1, NA_INTEGER, 2],
        levels: vec!["lo".into(), "hi".into()],
    };
    let series = to_series("f".into(), factor.clone()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        to_r(&series).unwrap_or_else(|error| panic!("{error}")),
        factor,
        "level order survives"
    );
}

#[test]
fn integer_and_double_data_move_into_arrow_without_a_copy() {
    let values: Vec<f64> = (0..1000).map(f64::from).collect();
    let address = values.as_ptr();
    let mut table = frame(vec![("x", RVector::Double(values))]);
    let slice = table
        .column_slice_f64("x")
        .unwrap_or_else(|| panic!("no zero-copy view"));
    assert_eq!(
        slice.as_ptr(),
        address,
        "the R buffer became the Arrow buffer"
    );
    assert_eq!(
        table
            .residency("x")
            .unwrap_or_else(|error| panic!("{error}")),
        Residency::Arrow
    );

    // A column with nulls has no zero-copy view, because R needs NA_real_ written into the data.
    let mut with_na = frame(vec![("x", doubles(&[1.0, NA_REAL]))]);
    assert!(with_na.column_slice_f64("x").is_none());
    assert!(is_na_real(match get(&mut with_na, "x") {
        RVector::Double(values) => values[1],
        _ => panic!("not double"),
    }));
    drop(table.frame());
}

#[test]
fn residency_tracks_reads_and_writes_per_column() {
    let mut table = frame(vec![("a", ints(&[1, 2, 3])), ("b", ints(&[4, 5, 6]))]);
    assert_eq!(table.residency("a").ok(), Some(Residency::Arrow));
    table.read_r("a").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(table.residency("a").ok(), Some(Residency::Shared));
    assert_eq!(
        table.residency("b").ok(),
        Some(Residency::Arrow),
        "residency is per column"
    );

    match table.write_r("b").unwrap_or_else(|error| panic!("{error}")) {
        RVector::Integer(values) => values[0] = NA_INTEGER,
        _ => panic!("not integer"),
    }
    assert_eq!(table.residency("b").ok(), Some(Residency::R));
    let total = run(
        &mut table,
        Query {
            select: Select::Value(Expr::Aggregate {
                function: Aggregate::Sum,
                arg: Box::new(column("b")),
                na_rm: true,
            }),
            ..Query::default()
        },
    );
    let Output::Vector(total) = total else {
        panic!("not a vector")
    };
    assert_eq!(
        to_r(&total).ok(),
        Some(ints(&[11])),
        "the query saw the write made through the R pointer"
    );

    // Replacing the column drops its R copy, which is stale now.
    table
        .set_column(
            to_series("a".into(), ints(&[7, 8, 9]))
                .unwrap_or_else(|error| panic!("{error}"))
                .into_column(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(table.residency("a").ok(), Some(Residency::Arrow));
}

#[test]
fn copy_on_modify_forks_only_the_column_list() {
    let mut original = frame(vec![
        ("x", doubles(&[1.0, 2.0])),
        ("y", doubles(&[3.0, 4.0])),
    ]);
    let before = original.column_slice_f64("x").map(<[f64]>::as_ptr);
    let mut fork = original.fork().unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        fork.column_slice_f64("x").map(<[f64]>::as_ptr),
        before,
        "the fork shares the buffers"
    );

    let update = Query {
        select: Select::Assign(vec![set(
            "x",
            binary(BinaryOp::Multiply, column("x"), Expr::Double(2.0)),
        )]),
        ..Query::default()
    };
    run(&mut fork, update);
    assert_eq!(read(&mut fork, "x"), doubles(&[2.0, 4.0]));
    assert_eq!(
        read(&mut original, "x"),
        doubles(&[1.0, 2.0]),
        "the shared original is unchanged"
    );
    assert_eq!(
        original.column_slice_f64("y").map(<[f64]>::as_ptr),
        fork.column_slice_f64("y").map(<[f64]>::as_ptr),
        "the untouched column is still shared"
    );
}

#[test]
fn an_in_place_update_leaves_other_columns_alone() {
    let mut table = frame(vec![
        ("x", doubles(&[1.0, 2.0, 3.0])),
        ("y", doubles(&[3.0, 4.0, 5.0])),
    ]);
    let y = table.column_slice_f64("y").map(<[f64]>::as_ptr);
    run(
        &mut table,
        Query {
            select: Select::Assign(vec![set(
                "z",
                binary(BinaryOp::Add, column("x"), column("y")),
            )]),
            ..Query::default()
        },
    );
    assert_eq!(names(&table), ["x", "y", "z"]);
    assert_eq!(read(&mut table, "z"), doubles(&[4.0, 6.0, 8.0]));
    assert_eq!(table.column_slice_f64("y").map(<[f64]>::as_ptr), y);
}

#[test]
fn the_schema_prints_and_parses_in_the_checkers_notation() {
    let table = frame(vec![
        ("id", ints(&[1])),
        ("price", doubles(&[1.0])),
        ("ticker", strings(&["a"])),
    ]);
    let schema = table.schema();
    assert_eq!(
        schema.to_string(),
        "frame{id: integer[], price: double[], ticker: character[]}"
    );
    assert_eq!(
        Schema::parse(&schema.to_string()).ok(),
        Some(schema.clone())
    );
    assert!(schema.check(&table).is_ok());
    let wrong = Schema::parse("frame{id: integer[], price: integer[], ticker: character[]}")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        wrong.check(&table).map_err(|error| error.to_string()),
        Err("column `price` is double[], but the schema declares integer[]".to_string())
    );
}

#[test]
fn polars_dtypes_without_an_r_type_are_normalized() {
    use table::polars::prelude::{DataFrame, NamedFrom, Series};
    let input = DataFrame::new_infer_height(vec![
        Series::new("small".into(), &[1i16, 2]).into_column(),
        Series::new("wide".into(), &[1i64, 2]).into_column(),
        Series::new("single".into(), &[1.5f32, 2.5]).into_column(),
    ])
    .unwrap_or_else(|error| panic!("{error}"));
    let table = Table::new(input).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        table.schema().to_string(),
        "frame{small: integer[], wide: double[], single: double[]}"
    );
}

#[test]
fn the_arrow_c_interface_round_trips_without_copying() {
    let values: Vec<f64> = vec![1.0, 2.0, 3.0];
    let mut table = frame(vec![
        ("x", RVector::Double(values)),
        ("name", strings(&["a", "NA", "c"])),
    ]);
    let address = table.column_slice_f64("x").map(<[f64]>::as_ptr);

    let exported = ffi::export_column(&mut table, "x").unwrap_or_else(|error| panic!("{error}"));
    let series = unsafe { ffi::import_column(exported.array, &exported.schema) }
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        series
            .f64()
            .ok()
            .and_then(|chunked| chunked.cont_slice().ok())
            .map(<[f64]>::as_ptr),
        address,
        "the imported column points at the same buffer"
    );

    let exported = ffi::export_table(&mut table).unwrap_or_else(|error| panic!("{error}"));
    let mut back = unsafe { ffi::import_table(exported.array, &exported.schema) }
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(back.schema(), table.schema());
    assert_eq!(read(&mut back, "name"), strings(&["a", "NA", "c"]));
    assert_eq!(back.column_slice_f64("x").map(<[f64]>::as_ptr), address);
}

fn sales() -> Table {
    frame(vec![
        ("g", strings(&["b", "a", "b", "c", "a"])),
        ("v", ints(&[1, NA_INTEGER, 3, 4, 5])),
        ("w", doubles(&[1.0, 2.0, 3.0, 4.0, 5.0])),
    ])
}

#[test]
fn by_keeps_first_appearance_and_na_propagates_unless_removed() {
    let mut table = sales();
    let query = Query {
        select: Select::Columns(vec![
            named("s", aggregate(Aggregate::Sum, column("v"))),
            named("n", Expr::Count),
        ]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let mut result = run_table(&mut table, query);
    assert_eq!(names(&result), ["g", "s", "n"]);
    assert_eq!(read(&mut result, "g"), strings(&["b", "a", "c"]));
    assert_eq!(read(&mut result, "s"), ints(&[4, NA_INTEGER, 4]));
    assert_eq!(read(&mut result, "n"), ints(&[2, 2, 1]));
    assert_eq!(
        result.schema().to_string(),
        "frame{g: character[], s: integer[], n: integer[]}"
    );
}

#[test]
fn keyby_sorts_and_keys_the_result() {
    let mut table = sales();
    let query = Query {
        select: Select::Columns(vec![named(
            "s",
            Expr::Aggregate {
                function: Aggregate::Sum,
                arg: Box::new(column("v")),
                na_rm: true,
            },
        )]),
        by: vec![named("g", column("g"))],
        keyed: true,
        ..Query::default()
    };
    let mut result = run_table(&mut table, query);
    assert_eq!(read(&mut result, "g"), strings(&["a", "b", "c"]));
    assert_eq!(read(&mut result, "s"), ints(&[5, 4, 4]));
    assert_eq!(result.key(), [PlSmallStr::from("g")]);
}

#[test]
fn every_gforce_aggregate_lowers_to_one_grouped_plan() {
    let mut table = frame(vec![
        ("g", ints(&[1, 1, 1, 2, 2])),
        ("x", doubles(&[1.0, 2.0, 6.0, 3.0, 5.0])),
    ]);
    let functions = [
        ("sum", Aggregate::Sum),
        ("mean", Aggregate::Mean),
        ("min", Aggregate::Min),
        ("max", Aggregate::Max),
        ("first", Aggregate::First),
        ("last", Aggregate::Last),
        ("median", Aggregate::Median),
        ("sd", Aggregate::Sd),
        ("var", Aggregate::Var),
        ("length", Aggregate::Length),
    ];
    let mut columns: Vec<_> = functions
        .iter()
        .map(|(name, function)| named(name, aggregate(*function, column("x"))))
        .collect();
    columns.push(named("N", Expr::Count));
    let query = Query {
        select: Select::Columns(columns),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let explained = plan(&mut table, query.clone())
        .and_then(|plan| Ok(plan.explain(true)?))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        explained.matches("AGGREGATE").count(),
        1,
        "one grouped aggregation:\n{explained}"
    );
    let mut result = run_table(&mut table, query);
    let expect = |result: &mut Table, name: &str, values: &[f64]| match get(result, name) {
        RVector::Double(found) => {
            for (found, expected) in found.iter().zip(values) {
                assert!(
                    (found - expected).abs() < 1e-12,
                    "{name}: {found} != {expected}"
                );
            }
        }
        other => panic!("{name} is {other:?}"),
    };
    expect(&mut result, "sum", &[9.0, 8.0]);
    expect(&mut result, "mean", &[3.0, 4.0]);
    expect(&mut result, "min", &[1.0, 3.0]);
    expect(&mut result, "max", &[6.0, 5.0]);
    expect(&mut result, "first", &[1.0, 3.0]);
    expect(&mut result, "last", &[6.0, 5.0]);
    expect(&mut result, "median", &[2.0, 4.0]);
    expect(&mut result, "sd", &[(7.0f64).sqrt(), (2.0f64).sqrt()]);
    expect(&mut result, "var", &[7.0, 2.0]);
    assert_eq!(read(&mut result, "length"), ints(&[3, 2]));
    assert_eq!(read(&mut result, "N"), ints(&[3, 2]));
}

#[test]
fn head_recycles_into_rows_per_group() {
    let mut table = sales();
    let query = Query {
        select: Select::Columns(vec![named("top", Expr::Head(Box::new(column("v")), 2))]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let mut result = run_table(&mut table, query);
    assert_eq!(read(&mut result, "g"), strings(&["b", "b", "a", "a", "c"]));
    assert_eq!(read(&mut result, "top"), ints(&[1, 3, NA_INTEGER, 5, 4]));
}

#[test]
fn filters_order_and_positions_select_rows() {
    let mut table = sales();
    let filter = binary(BinaryOp::Greater, column("v"), Expr::Integer(2));
    let mut result = run_table(
        &mut table,
        Query {
            rows: Rows::Filter(filter),
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut result, "v"),
        ints(&[3, 4, 5]),
        "an NA in the filter drops the row"
    );

    let order = Rows::Order(vec![
        SortKey {
            expr: column("g"),
            descending: false,
        },
        SortKey {
            expr: column("w"),
            descending: true,
        },
    ]);
    let mut result = run_table(
        &mut table,
        Query {
            rows: order,
            ..Query::default()
        },
    );
    assert_eq!(read(&mut result, "w"), doubles(&[5.0, 2.0, 3.0, 1.0, 4.0]));

    let order = Rows::Order(vec![SortKey {
        expr: column("v"),
        descending: false,
    }]);
    let mut result = run_table(
        &mut table,
        Query {
            rows: order,
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut result, "v"),
        ints(&[1, 3, 4, 5, NA_INTEGER]),
        "order puts NA last"
    );

    let mut result = run_table(
        &mut table,
        Query {
            rows: Rows::Positions(vec![4, 0]),
            ..Query::default()
        },
    );
    assert_eq!(read(&mut result, "g"), strings(&["a", "b"]));
}

#[test]
fn a_bare_j_returns_a_vector() {
    let mut table = sales();
    let Output::Vector(series) = run(
        &mut table,
        Query {
            select: Select::Value(column("w")),
            ..Query::default()
        },
    ) else {
        panic!("not a vector")
    };
    assert_eq!(
        to_r(&series).ok(),
        Some(doubles(&[1.0, 2.0, 3.0, 4.0, 5.0]))
    );
}

#[test]
fn integer_arithmetic_follows_r() {
    let mut table = frame(vec![
        ("x", ints(&[i32::MAX, 7, -7])),
        ("y", ints(&[1, 2, 2])),
    ]);
    let select = |op| Query {
        select: Select::Value(binary(op, column("x"), column("y"))),
        ..Query::default()
    };
    let value = |table: &mut Table, query| match run(table, query) {
        Output::Vector(series) => to_r(&series).unwrap_or_else(|error| panic!("{error}")),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        value(&mut table, select(BinaryOp::Add)),
        ints(&[NA_INTEGER, 9, -5]),
        "overflow is NA"
    );
    assert_eq!(
        value(&mut table, select(BinaryOp::Divide)),
        doubles(&[i32::MAX as f64, 3.5, -3.5]),
        "`/` is double"
    );
    assert_eq!(
        value(&mut table, select(BinaryOp::IntegerDivide)),
        ints(&[i32::MAX, 3, -4]),
        "`%/%` floors"
    );
    assert_eq!(
        value(&mut table, select(BinaryOp::Modulo)),
        ints(&[0, 1, 1]),
        "`%%` takes the divisor's sign"
    );
    let negated = Query {
        select: Select::Value(Expr::Unary(UnaryOp::Negate, Box::new(column("y")))),
        ..Query::default()
    };
    assert_eq!(value(&mut table, negated), ints(&[-1, -2, -2]));
}

#[test]
fn column_references_are_checked_before_the_query_runs() {
    let mut table = sales();
    let query = Query {
        select: Select::Value(aggregate(Aggregate::Sum, column("pric"))),
        ..Query::default()
    };
    assert_eq!(
        result_type(&table.schema(), &query).map_err(|error| error.to_string()),
        Err("the frame has no column `pric`".to_string())
    );
    assert!(execute(&mut table, query).is_err());

    let query = Query {
        rows: Rows::Filter(column("w")),
        ..Query::default()
    };
    assert_eq!(
        result_type(&table.schema(), &query).map_err(|error| error.to_string()),
        Err("invalid query: a row filter must be logical, not double[]".to_string())
    );

    let query = Query {
        select: Select::Columns(vec![named("m", aggregate(Aggregate::Mean, column("v")))]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    assert_eq!(
        result_type(&table.schema(), &query).ok(),
        Some(ResultType::Table(Schema::new([
            ("g", ColumnType::Character),
            ("m", ColumnType::Double)
        ])))
    );
}

fn join_x() -> Table {
    frame(vec![
        ("id", ints(&[1, 2, 2, 3])),
        ("v", doubles(&[10.0, 20.0, 21.0, 30.0])),
        ("w", strings(&["a", "b", "c", "d"])),
    ])
}

fn join_i() -> Table {
    frame(vec![
        ("id", ints(&[2, 4, 1])),
        ("v", doubles(&[100.0, 400.0, 111.0])),
    ])
}

fn join(i: &mut Table, on: &[(&str, &str)], drop_unmatched: bool, roll: Roll) -> Rows {
    Rows::Join(Join {
        other: i.frame().unwrap_or_else(|error| panic!("{error}")).clone(),
        on: on
            .iter()
            .map(|(x, i)| (PlSmallStr::from(*x), PlSmallStr::from(*i)))
            .collect(),
        drop_unmatched,
        roll,
    })
}

#[test]
fn a_join_follows_the_rows_of_i_with_data_tables_column_layout() {
    let (mut x, mut i) = (join_x(), join_i());
    let rows = join(&mut i, &[("id", "id")], false, Roll::None);
    let mut result = run_table(
        &mut x,
        Query {
            rows,
            ..Query::default()
        },
    );
    assert_eq!(names(&result), ["id", "v", "w", "i.v"]);
    assert_eq!(read(&mut result, "id"), ints(&[2, 2, 4, 1]));
    assert_eq!(
        read(&mut result, "v"),
        doubles(&[20.0, 21.0, NA_REAL, 10.0])
    );
    assert_eq!(read(&mut result, "w"), strings(&["b", "c", "NA", "a"]));
    assert_eq!(
        read(&mut result, "i.v"),
        doubles(&[100.0, 100.0, 400.0, 111.0])
    );

    let rows = join(&mut i, &[("id", "id")], true, Roll::None);
    let mut result = run_table(
        &mut x,
        Query {
            rows,
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut result, "id"),
        ints(&[2, 2, 1]),
        "nomatch = NULL drops unmatched rows of i"
    );
}

#[test]
fn a_keyed_table_joins_on_its_key() {
    let (mut x, mut i) = (join_x(), join_i());
    x.set_key(&["id"]).unwrap_or_else(|error| panic!("{error}"));
    let rows = join(&mut i, &[], true, Roll::None);
    let mut result = run_table(
        &mut x,
        Query {
            rows,
            ..Query::default()
        },
    );
    assert_eq!(read(&mut result, "id"), ints(&[2, 2, 1]));
}

fn prices() -> Table {
    let mut table = frame(vec![
        ("g", ints(&[1, 1, 1, 2])),
        ("t", ints(&[1, 5, 9, 3])),
        ("price", doubles(&[1.0, 5.0, 9.0, 3.0])),
    ]);
    table
        .set_key(&["g", "t"])
        .unwrap_or_else(|error| panic!("{error}"));
    table
}

fn trades() -> Table {
    frame(vec![
        ("g", ints(&[1, 2, 1, 1])),
        ("t", ints(&[6, 2, 0, 9])),
        ("q", ints(&[1, 2, 3, 4])),
    ])
}

#[test]
fn rolling_joins_match_data_table() {
    let cases = [
        (Roll::Backward(None), [5.0, NA_REAL, NA_REAL, 9.0]),
        (Roll::Forward(None), [9.0, 3.0, 1.0, 9.0]),
        (Roll::Nearest, [5.0, 3.0, 1.0, 9.0]),
        (Roll::Backward(Some(2.0)), [5.0, NA_REAL, NA_REAL, 9.0]),
    ];
    for (roll, expected) in cases {
        let (mut x, mut i) = (prices(), trades());
        let rows = join(&mut i, &[("g", "g"), ("t", "t")], false, roll);
        let mut result = run_table(
            &mut x,
            Query {
                rows,
                ..Query::default()
            },
        );
        assert_eq!(names(&result), ["g", "t", "price", "q"]);
        assert_eq!(
            read(&mut result, "t"),
            ints(&[6, 2, 0, 9]),
            "the rolled column carries i's values ({roll:?})"
        );
        assert_eq!(
            read(&mut result, "q"),
            ints(&[1, 2, 3, 4]),
            "rows follow i ({roll:?})"
        );
        let RVector::Double(found) = get(&mut result, "price") else {
            panic!("not double")
        };
        for (found, expected) in found.iter().zip(expected) {
            assert!(
                found.to_bits() == expected.to_bits() || found == &expected,
                "{roll:?}: {found:?} != {expected:?}"
            );
        }
    }
}

#[test]
fn a_tight_roll_window_misses() {
    let (mut x, mut i) = (prices(), trades());
    let rows = join(
        &mut i,
        &[("g", "g"), ("t", "t")],
        false,
        Roll::Backward(Some(0.5)),
    );
    let mut result = run_table(
        &mut x,
        Query {
            rows,
            ..Query::default()
        },
    );
    let RVector::Double(found) = get(&mut result, "price") else {
        panic!("not double")
    };
    assert!(is_na_real(found[0]), "6 is more than 0.5 after 5");
    assert_eq!(found[3], 9.0, "an exact match is within any window");
}

#[test]
fn assignment_by_subset_fills_na_and_by_group_uses_windows() {
    let mut table = sales();
    let filter = binary(BinaryOp::Greater, column("v"), Expr::Integer(2));
    let assign = vec![set(
        "z",
        binary(BinaryOp::Multiply, column("v"), Expr::Integer(2)),
    )];
    run(
        &mut table,
        Query {
            rows: Rows::Filter(filter),
            select: Select::Assign(assign),
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut table, "z"),
        ints(&[NA_INTEGER, NA_INTEGER, 6, 8, 10])
    );

    let total = vec![set("total", aggregate(Aggregate::Sum, column("w")))];
    run(
        &mut table,
        Query {
            select: Select::Assign(total),
            by: vec![named("g", column("g"))],
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut table, "total"),
        doubles(&[4.0, 7.0, 4.0, 4.0, 7.0])
    );

    run(
        &mut table,
        Query {
            select: Select::Assign(vec![(PlSmallStr::from("total"), None)]),
            ..Query::default()
        },
    );
    assert_eq!(names(&table), ["g", "v", "w", "z"], "`:= NULL` deletes");
}

#[test]
fn a_filtered_grouped_assignment_groups_only_the_selected_rows() {
    let mut table = sales();
    let filter = binary(BinaryOp::Greater, column("w"), Expr::Double(1.5));
    let assign = vec![set("n", Expr::Count)];
    run(
        &mut table,
        Query {
            rows: Rows::Filter(filter),
            select: Select::Assign(assign),
            by: vec![named("g", column("g"))],
            ..Query::default()
        },
    );
    // Rows 2 to 5 pass: groups a (rows 2, 5), b (row 3) and c (row 4); row 1 gets NA.
    assert_eq!(read(&mut table, "n"), ints(&[NA_INTEGER, 2, 1, 1, 2]));

    // Overwriting an existing column: a selected row whose group computes NA takes the NA, and the
    // unselected row 1 keeps its value.
    let mut table = sales();
    let filter = binary(BinaryOp::Greater, column("w"), Expr::Double(1.5));
    let assign = vec![set("v", aggregate(Aggregate::Sum, column("v")))];
    run(
        &mut table,
        Query {
            rows: Rows::Filter(filter),
            select: Select::Assign(assign),
            by: vec![named("g", column("g"))],
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut table, "v"),
        ints(&[1, NA_INTEGER, 3, 4, NA_INTEGER])
    );
}

/// Grouped sums and means over random data with NAs, checked against a direct computation.
#[test]
fn fuzz_grouped_aggregates_against_a_reference() {
    let mut rng = SplitMix64(0x7ab1e);
    for _ in 0..fuzz_iterations(40) {
        let rows = 1 + rng.below(300);
        let groups = 1 + rng.below(12);
        let keys: Vec<i32> = (0..rows).map(|_| rng.below(groups) as i32).collect();
        let values: Vec<f64> = (0..rows)
            .map(|_| {
                if rng.chance(1, 10) {
                    NA_REAL
                } else {
                    rng.below(1000) as f64 / 8.0
                }
            })
            .collect();
        let na_rm = rng.chance(1, 2);
        let mut table = frame(vec![
            ("k", ints(&keys)),
            ("x", RVector::Double(values.clone())),
        ]);
        let query = Query {
            select: Select::Columns(vec![
                named(
                    "s",
                    Expr::Aggregate {
                        function: Aggregate::Sum,
                        arg: Box::new(column("x")),
                        na_rm,
                    },
                ),
                named(
                    "m",
                    Expr::Aggregate {
                        function: Aggregate::Mean,
                        arg: Box::new(column("x")),
                        na_rm,
                    },
                ),
            ]),
            by: vec![named("k", column("k"))],
            ..Query::default()
        };
        let mut result = run_table(&mut table, query);
        let RVector::Integer(found_keys) = get(&mut result, "k") else {
            panic!("keys")
        };
        let RVector::Double(sums) = get(&mut result, "s") else {
            panic!("sums")
        };
        let RVector::Double(means) = get(&mut result, "m") else {
            panic!("means")
        };
        let mut order = Vec::new();
        for key in &keys {
            if !order.contains(key) {
                order.push(*key);
            }
        }
        assert_eq!(found_keys, order, "groups in order of first appearance");
        for (index, key) in order.iter().enumerate() {
            let members: Vec<f64> = keys
                .iter()
                .zip(&values)
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| *v)
                .collect();
            let has_na = members.iter().any(|value| is_na_real(*value));
            let present: Vec<f64> = members
                .iter()
                .copied()
                .filter(|value| !is_na_real(*value))
                .collect();
            if has_na && !na_rm {
                assert!(
                    is_na_real(sums[index]) && is_na_real(means[index]),
                    "NA propagates"
                );
                continue;
            }
            let sum: f64 = present.iter().sum();
            assert!(
                (sums[index] - sum).abs() < 1e-9,
                "sum {} != {sum}",
                sums[index]
            );
            if present.is_empty() {
                assert!(means[index].is_nan(), "the mean of nothing is NaN, as in R");
            } else {
                assert!((means[index] - sum / present.len() as f64).abs() < 1e-9);
            }
        }
    }
}

/// Random R vectors survive the trip into Arrow and back unchanged, NA and NaN included.
#[test]
fn fuzz_boundary_round_trip() {
    let mut rng = SplitMix64(0xa11);
    for _ in 0..fuzz_iterations(200) {
        let length = rng.below(64);
        let vector = match rng.below(5) {
            0 => RVector::Integer(
                (0..length)
                    .map(|_| {
                        if rng.chance(1, 6) {
                            NA_INTEGER
                        } else {
                            rng.next_u64() as i32 | 1
                        }
                    })
                    .collect(),
            ),
            1 => RVector::Double(
                (0..length)
                    .map(|_| match rng.below(8) {
                        0 => NA_REAL,
                        1 => f64::NAN,
                        2 => f64::INFINITY,
                        _ => f64::from_bits(rng.next_u64() & !(0x7FF << 52)),
                    })
                    .collect(),
            ),
            2 => RVector::Logical(
                (0..length)
                    .map(|_| [0, 1, NA_INTEGER][rng.below(3)])
                    .collect(),
            ),
            3 => RVector::Character(
                (0..length)
                    .map(|_| (!rng.chance(1, 5)).then(|| format!("s{}", rng.below(9))))
                    .collect(),
            ),
            _ => {
                let levels: Vec<String> = (0..1 + rng.below(5))
                    .map(|level| format!("l{level}"))
                    .collect();
                let codes = (0..length)
                    .map(|_| {
                        if rng.chance(1, 6) {
                            NA_INTEGER
                        } else {
                            1 + rng.below(levels.len()) as i32
                        }
                    })
                    .collect();
                RVector::Factor { codes, levels }
            }
        };
        let series =
            to_series("v".into(), vector.clone()).unwrap_or_else(|error| panic!("{error}"));
        let back = to_r(&series).unwrap_or_else(|error| panic!("{error}"));
        match (&vector, &back) {
            (RVector::Double(left), RVector::Double(right)) => {
                assert_eq!(left.len(), right.len());
                for (left, right) in left.iter().zip(right) {
                    assert!(
                        left.to_bits() == right.to_bits()
                            || (left.is_nan()
                                && right.is_nan()
                                && is_na_real(*left) == is_na_real(*right))
                    );
                }
            }
            _ => assert_eq!(vector, back),
        }
    }
}

#[test]
fn factors_with_different_levels_join_by_label() {
    let factor = |codes: &[i32], levels: &[&str]| RVector::Factor {
        codes: codes.to_vec(),
        levels: levels.iter().map(|level| level.to_string()).collect(),
    };
    let mut x = frame(vec![
        ("k", factor(&[1, 2, 3], &["a", "b", "c"])),
        ("v", ints(&[1, 2, 3])),
    ]);
    let mut i = frame(vec![
        ("k", factor(&[2, 1, 1], &["b", "d"])),
        ("w", doubles(&[1.0, 2.0, 3.0])),
    ]);
    let rows = join(&mut i, &[("k", "k")], false, Roll::None);
    let mut result = run_table(
        &mut x,
        Query {
            rows,
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut result, "k"),
        factor(&[2, 1, 1], &["b", "d"]),
        "the join column keeps i's levels"
    );
    assert_eq!(read(&mut result, "v"), ints(&[NA_INTEGER, 2, 2]));
}
