//! The table engine, driven directly from Rust. Every expected value that encodes data.table
//! semantics (join layout, rolling matches, group order, `NA` handling) was read from data.table
//! 1.17 under GNU R on the same inputs.

use syntax::testing::{SplitMix64, fuzz_iterations};
use table::{
    ColumnType, Schema, Table,
    convert::{NA_INTEGER, NA_REAL, RVector, is_na_real, to_r, to_series},
    ffi,
    polars::prelude::{IntoColumn, PlSmallStr},
    query::{
        Aggregate, BinaryOp, Comparison, Executed, Expr, Join, Mult, On, Output, Query, ResultType,
        Roll, Rows, Select, SortKey, UnaryOp,
        build::{aggregate, binary, column},
        execute, execute_with_plans_only, plan, result_type,
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
    table
        .column_r(name)
        .unwrap_or_else(|error| panic!("no column {name}: {error}"))
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
    execute(table, query)
        .unwrap_or_else(|error| panic!("query failed: {error}"))
        .output
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
    let table = frame(vec![("x", RVector::Double(values))]);
    let slice = table
        .column_slice_f64("x")
        .unwrap_or_else(|| panic!("no zero-copy view"));
    assert_eq!(
        slice.as_ptr(),
        address,
        "the R buffer became the Arrow buffer"
    );

    // A column with nulls has no zero-copy view, because R needs NA_real_ written into the data.
    let mut with_na = frame(vec![("x", doubles(&[1.0, NA_REAL]))]);
    assert!(with_na.column_slice_f64("x").is_none());
    assert!(is_na_real(match get(&mut with_na, "x") {
        RVector::Double(values) => values[1],
        _ => panic!("not double"),
    }));
}

#[test]
fn copy_on_modify_forks_only_the_column_list() {
    let mut original = frame(vec![
        ("x", doubles(&[1.0, 2.0])),
        ("y", doubles(&[3.0, 4.0])),
    ]);
    let before = original.column_slice_f64("x").map(<[f64]>::as_ptr);
    let mut fork = original.fork();
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
    let table = frame(vec![
        ("x", RVector::Double(values)),
        ("name", strings(&["a", "NA", "c"])),
    ]);
    let address = table.column_slice_f64("x").map(<[f64]>::as_ptr);

    let exported = ffi::export_column(&table, "x").unwrap_or_else(|error| panic!("{error}"));
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

    let exported = ffi::export_table(&table).unwrap_or_else(|error| panic!("{error}"));
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
    let explained = plan(&table, query.clone())
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
        result_type(&table.schema(), table.key(), &query).map_err(|error| error.to_string()),
        Err("the frame has no column `pric`".to_string())
    );
    assert!(execute(&mut table, query).is_err());

    let query = Query {
        rows: Rows::Filter(column("w")),
        ..Query::default()
    };
    assert_eq!(
        result_type(&table.schema(), table.key(), &query).map_err(|error| error.to_string()),
        Err("invalid query: a row filter must be logical, not double[]".to_string())
    );

    let query = Query {
        select: Select::Columns(vec![named("m", aggregate(Aggregate::Mean, column("v")))]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    assert_eq!(
        result_type(&table.schema(), table.key(), &query).ok(),
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
        other: i.clone(),
        on: on.iter().map(|(x, i)| On::equal(*x, *i)).collect(),
        drop_unmatched,
        roll,
        mult: Mult::All,
        each_i: false,
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

fn query(rows: Rows, select: Select) -> Query {
    Query {
        rows,
        select,
        ..Query::default()
    }
}

#[test]
fn a_filtered_assignment_aggregates_over_the_selected_rows_only() {
    let mut x = frame(vec![
        ("g", strings(&["a", "b", "a"])),
        ("v", ints(&[1, 2, 3])),
    ]);
    let selected = Rows::Filter(binary(
        BinaryOp::Equal,
        column("g"),
        Expr::Character("a".to_string()),
    ));
    run(
        &mut x,
        query(
            selected.clone(),
            Select::Assign(vec![
                set("s", aggregate(Aggregate::Sum, column("v"))),
                set("n", Expr::Count),
            ]),
        ),
    );
    assert_eq!(read(&mut x, "s"), ints(&[4, NA_INTEGER, 4]));
    assert_eq!(read(&mut x, "n"), ints(&[2, NA_INTEGER, 2]));
}

#[test]
fn a_keyed_join_without_on_matches_the_leading_columns_of_i() {
    let mut x = frame(vec![
        ("id", ints(&[1, 2, 3])),
        ("v", doubles(&[10.0, 20.0, 30.0])),
    ]);
    x.set_key(&["id"]).unwrap_or_else(|error| panic!("{error}"));
    let mut i = frame(vec![("code", ints(&[3, 1])), ("id", ints(&[1, 2]))]);
    let rows = join(&mut i, &[], false, Roll::None);
    let mut result = run_table(&mut x, query(rows, Select::All));
    assert_eq!(names(&result), ["id", "v", "i.id"]);
    assert_eq!(read(&mut result, "id"), ints(&[3, 1]));
    assert_eq!(read(&mut result, "v"), doubles(&[30.0, 10.0]));
    assert_eq!(read(&mut result, "i.id"), ints(&[1, 2]));
}

#[test]
fn na_keys_join_each_other() {
    let mut x = frame(vec![
        ("k", ints(&[1, NA_INTEGER, 3])),
        ("v", doubles(&[1.0, 2.0, 3.0])),
    ]);
    let mut i = frame(vec![("k", ints(&[NA_INTEGER, 3]))]);
    let rows = join(&mut i, &[("k", "k")], false, Roll::None);
    let mut result = run_table(&mut x, query(rows, Select::All));
    assert_eq!(read(&mut result, "k"), ints(&[NA_INTEGER, 3]));
    assert_eq!(read(&mut result, "v"), doubles(&[2.0, 3.0]));
}

#[test]
fn a_rolling_inner_join_keeps_matches_whose_values_are_na() {
    let mut x = frame(vec![("t", ints(&[1, 5])), ("v", doubles(&[NA_REAL, 2.0]))]);
    let mut i = frame(vec![("t", ints(&[0, 2, 6]))]);
    let rows = join(&mut i, &[("t", "t")], true, Roll::Backward(None));
    let mut result = run_table(&mut x, query(rows, Select::All));
    assert_eq!(read(&mut result, "t"), ints(&[2, 6]));
    assert_eq!(read(&mut result, "v"), doubles(&[NA_REAL, 2.0]));
}

#[test]
fn a_join_column_takes_the_type_of_i() {
    let mut x = frame(vec![("id", ints(&[1, 2])), ("v", doubles(&[10.0, 20.0]))]);
    let mut i = frame(vec![
        ("id", doubles(&[1.5, 2.0])),
        ("v", doubles(&[1.0, 2.0])),
    ]);
    let rows = join(&mut i, &[("id", "id")], false, Roll::None);
    let select = Select::Columns(vec![named("id", column("id")), named("iv", column("i.v"))]);
    let joined = query(rows, select);
    assert_eq!(
        result_type(&x.schema(), x.key(), &joined).ok(),
        Some(ResultType::Table(Schema {
            columns: vec![
                ("id".into(), ColumnType::Double),
                ("iv".into(), ColumnType::Double),
            ]
        }))
    );
    let mut result = run_table(&mut x, joined);
    assert_eq!(read(&mut result, "id"), doubles(&[1.5, 2.0]));
    assert_eq!(read(&mut result, "iv"), doubles(&[1.0, 2.0]));
}

#[test]
fn reordering_positions_drop_the_key() {
    let mut x = frame(vec![("id", ints(&[1, 2, 3]))]);
    x.set_key(&["id"]).unwrap_or_else(|error| panic!("{error}"));
    let kept = run_table(&mut x, query(Rows::Positions(vec![0, 2]), Select::All));
    assert_eq!(kept.key(), ["id"]);
    let reordered = run_table(&mut x, query(Rows::Positions(vec![2, 0]), Select::All));
    assert!(reordered.key().is_empty());
}

#[test]
fn logical_operators_treat_numbers_as_r_does() {
    let mut x = frame(vec![("a", ints(&[0, 2])), ("b", ints(&[2, 0]))]);
    let select = Select::Columns(vec![
        named("n", Expr::Unary(UnaryOp::Not, Box::new(column("a")))),
        named("both", binary(BinaryOp::And, column("a"), column("b"))),
        named("either", binary(BinaryOp::Or, column("a"), column("b"))),
    ]);
    let mut result = run_table(&mut x, query(Rows::All, select));
    assert_eq!(read(&mut result, "n"), RVector::Logical(vec![1, 0]));
    assert_eq!(read(&mut result, "both"), RVector::Logical(vec![0, 0]));
    assert_eq!(read(&mut result, "either"), RVector::Logical(vec![1, 1]));
}

#[test]
fn nan_compares_sorts_and_aggregates_as_in_r() {
    let mut x = frame(vec![("d", doubles(&[1.0, f64::NAN, -1.0, NA_REAL]))]);
    let positive = Rows::Filter(binary(BinaryOp::Greater, column("d"), Expr::Double(0.0)));
    let mut kept = run_table(&mut x, query(positive, Select::All));
    assert_eq!(read(&mut kept, "d"), doubles(&[1.0]));

    let descending = Rows::Order(vec![SortKey {
        expr: column("d"),
        descending: true,
    }]);
    let mut sorted = run_table(&mut x, query(descending, Select::All));
    assert_eq!(
        read(&mut sorted, "d"),
        doubles(&[1.0, -1.0, f64::NAN, NA_REAL])
    );

    // `order` keeps `NA` and `NaN` in their original order at the end, in either direction.
    let mut na_first = frame(vec![("d", doubles(&[NA_REAL, f64::NAN, 1.0, -1.0]))]);
    for descending in [false, true] {
        let order = Rows::Order(vec![SortKey {
            expr: column("d"),
            descending,
        }]);
        let mut sorted = run_table(&mut na_first, query(order, Select::All));
        let numbers = if descending { [1.0, -1.0] } else { [-1.0, 1.0] };
        assert_eq!(
            read(&mut sorted, "d"),
            doubles(&[numbers[0], numbers[1], NA_REAL, f64::NAN])
        );
    }

    let mut without_na = frame(vec![("d", doubles(&[1.0, f64::NAN, -1.0]))]);
    let select = Select::Columns(vec![
        named("mx", aggregate(Aggregate::Max, column("d"))),
        named("mn", aggregate(Aggregate::Min, column("d"))),
    ]);
    let mut extremes = run_table(&mut without_na, query(Rows::All, select));
    assert!(
        matches!(get(&mut extremes, "mx"), RVector::Double(values) if values[0].is_nan() && !is_na_real(values[0]))
    );
    assert!(
        matches!(get(&mut extremes, "mn"), RVector::Double(values) if values[0].is_nan() && !is_na_real(values[0]))
    );
}

#[test]
fn a_logical_compares_with_a_string_by_its_r_spelling() {
    let mut x = frame(vec![
        ("l", RVector::Logical(vec![1, 0])),
        ("s", strings(&["TRUE", "TRUE"])),
    ]);
    let select = Select::Columns(vec![named(
        "eq",
        binary(BinaryOp::Equal, column("l"), column("s")),
    )]);
    let mut result = run_table(&mut x, query(Rows::All, select));
    assert_eq!(read(&mut result, "eq"), RVector::Logical(vec![1, 0]));
}

#[test]
fn an_integer_sum_that_overflows_becomes_double() {
    let mut x = frame(vec![
        ("g", ints(&[1, 1, 2])),
        ("v", ints(&[2_000_000_000, 2_000_000_000, 1])),
    ]);
    let by_group = Query {
        select: Select::Columns(vec![named("s", aggregate(Aggregate::Sum, column("v")))]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let mut result = run_table(&mut x, by_group);
    assert_eq!(read(&mut result, "s"), doubles(&[4e9, 1.0]));

    let small = Rows::Filter(binary(BinaryOp::Equal, column("g"), Expr::Integer(2)));
    let select = Select::Columns(vec![named("s", aggregate(Aggregate::Sum, column("v")))]);
    let mut result = run_table(&mut x, query(small, select));
    assert_eq!(
        read(&mut result, "s"),
        ints(&[1]),
        "a sum that fits stays integer"
    );

    let nested = Select::Columns(vec![named(
        "s",
        binary(
            BinaryOp::Subtract,
            aggregate(Aggregate::Sum, column("v")),
            Expr::Integer(1),
        ),
    )]);
    let mut result = run_table(&mut x, query(Rows::All, nested));
    assert_eq!(
        read(&mut result, "s"),
        doubles(&[4e9 + 1.0 - 1.0]),
        "a nested sum that overflows is a double too"
    );

    let mut updated = x.fork();
    run(
        &mut updated,
        Query {
            select: Select::Assign(vec![set("s", aggregate(Aggregate::Sum, column("v")))]),
            by: vec![named("g", column("g"))],
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut updated, "s"),
        doubles(&[4e9, 4e9, 1.0]),
        "a new column holding a grouped sum that overflows is a double"
    );
}

/// R's integer `sum` returns `NA` as soon as it meets one without `na.rm`, so a group holding an `NA`
/// never overflows, and only the groups that really overflow make the result a double.
#[test]
fn an_na_group_sum_neither_overflows_nor_loses_its_na() {
    let plus_one = || Query {
        select: Select::Columns(vec![named(
            "s",
            binary(
                BinaryOp::Add,
                aggregate(Aggregate::Sum, column("v")),
                Expr::Integer(1),
            ),
        )]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let cases = [
        (
            vec![1, 1, 2, 3],
            vec![2_000_000_000, 2_000_000_000, 1, NA_INTEGER],
            doubles(&[4e9 + 1.0, 2.0, NA_REAL]),
        ),
        (
            vec![1, 1, 2],
            vec![2_000_000_000, NA_INTEGER, 1],
            ints(&[NA_INTEGER, 2]),
        ),
    ];
    for (g, v, expected) in cases {
        let x = frame(vec![("g", ints(&g)), ("v", ints(&v))]);
        let mut kernel = run_table(&mut x.fork(), plus_one());
        assert_eq!(read(&mut kernel, "s"), expected.clone(), "kernel");
        let mut planned = match execute_with_plans_only(&mut x.fork(), plus_one())
            .map(|executed| executed.output)
        {
            Ok(Output::Table(result)) => result,
            other => panic!("expected a table, got {other:?}"),
        };
        assert_eq!(read(&mut planned, "s"), expected, "plan");
    }
}

fn factor(codes: &[i32], levels: &[&str]) -> RVector {
    RVector::Factor {
        codes: codes.to_vec(),
        levels: levels.iter().map(|level| level.to_string()).collect(),
    }
}

#[test]
fn a_keyed_join_without_on_matches_the_key_of_a_keyed_i() {
    let mut x = frame(vec![
        ("a", ints(&[1, 2, 3])),
        ("v", doubles(&[10.0, 20.0, 30.0])),
    ]);
    x.set_key(&["a"]).unwrap_or_else(|error| panic!("{error}"));
    let mut i = frame(vec![("z", ints(&[3, 1])), ("a", ints(&[1, 2]))]);
    i.set_key(&["a"]).unwrap_or_else(|error| panic!("{error}"));
    let rows = join(&mut i, &[], false, Roll::None);
    let mut result = run_table(&mut x, query(rows, Select::All));
    assert_eq!(names(&result), ["a", "v", "z"]);
    assert_eq!(read(&mut result, "a"), ints(&[1, 2]));
    assert_eq!(read(&mut result, "v"), doubles(&[10.0, 20.0]));
    assert_eq!(read(&mut result, "z"), ints(&[3, 1]));
}

#[test]
fn na_rm_drops_nan_and_an_emptied_mean_is_nan() {
    let mut x = frame(vec![
        ("x", doubles(&[1.0, f64::NAN, 3.0, NA_REAL])),
        ("g", ints(&[1, 1, 2, 2])),
    ]);
    let removed = |function| Expr::Aggregate {
        function,
        arg: Box::new(column("x")),
        na_rm: true,
    };
    let grouped = Query {
        select: Select::Columns(vec![
            named("s", removed(Aggregate::Sum)),
            named("m", removed(Aggregate::Mean)),
            named("mx", removed(Aggregate::Max)),
            named("md", removed(Aggregate::Median)),
        ]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let mut result = run_table(&mut x, grouped);
    for name in ["s", "m", "mx", "md"] {
        assert_eq!(read(&mut result, name), doubles(&[1.0, 3.0]), "{name}");
    }

    let mut empty = frame(vec![("x", doubles(&[NA_REAL, f64::NAN]))]);
    let select = Select::Columns(vec![named("m", removed(Aggregate::Mean))]);
    let mut result = run_table(&mut empty, query(Rows::All, select));
    assert!(
        matches!(get(&mut result, "m"), RVector::Double(values) if values[0].is_nan() && !is_na_real(values[0])),
        "data.table gives NaN, not NA"
    );
}

#[test]
fn an_na_ifelse_test_gives_na() {
    let mut x = frame(vec![("t", RVector::Logical(vec![1, NA_INTEGER]))]);
    let choice = Expr::IfElse(
        Box::new(column("t")),
        Box::new(Expr::Double(1.0)),
        Box::new(Expr::Double(2.0)),
    );
    let mut result = run_table(
        &mut x,
        query(Rows::All, Select::Columns(vec![named("r", choice)])),
    );
    assert_eq!(read(&mut result, "r"), doubles(&[1.0, NA_REAL]));
}

#[test]
fn max_and_min_of_logicals_are_integers() {
    let mut x = frame(vec![
        ("b", RVector::Logical(vec![1, 0, NA_INTEGER])),
        ("g", ints(&[1, 1, 2])),
    ]);
    let grouped = Query {
        select: Select::Columns(vec![
            named("mx", aggregate(Aggregate::Max, column("b"))),
            named("mn", aggregate(Aggregate::Min, column("b"))),
        ]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    assert_eq!(
        result_type(&x.schema(), x.key(), &grouped).ok(),
        Some(ResultType::Table(Schema::new([
            ("g", ColumnType::Integer),
            ("mx", ColumnType::Integer),
            ("mn", ColumnType::Integer),
        ])))
    );
    let mut result = run_table(&mut x, grouped);
    assert_eq!(read(&mut result, "mx"), ints(&[1, NA_INTEGER]));
    assert_eq!(read(&mut result, "mn"), ints(&[0, NA_INTEGER]));
}

#[test]
fn assigning_into_an_existing_column_keeps_its_type_unless_the_value_replaces_it() {
    let mut x = frame(vec![
        ("i", ints(&[1, 2])),
        ("d", doubles(&[1.5, 2.5])),
        ("f", factor(&[1, 2], &["a", "b"])),
        ("h", factor(&[2, 1], &["q", "r"])),
        ("g", ints(&[1, 2])),
    ]);
    let second = || Rows::Filter(binary(BinaryOp::Greater, column("g"), Expr::Integer(1)));
    run(
        &mut x,
        query(second(), Select::Assign(vec![set("i", Expr::Double(1.5))])),
    );
    assert_eq!(
        read(&mut x, "i"),
        ints(&[1, 1]),
        "a subset truncates into the integer column"
    );
    run(
        &mut x,
        query(
            second(),
            Select::Assign(vec![set("f", Expr::Character("z".into()))]),
        ),
    );
    assert_eq!(
        read(&mut x, "f"),
        factor(&[1, 3], &["a", "b", "z"]),
        "a new label adds a level"
    );
    run(
        &mut x,
        query(Rows::All, Select::Assign(vec![set("d", Expr::Integer(1))])),
    );
    assert_eq!(
        read(&mut x, "d"),
        doubles(&[1.0, 1.0]),
        "one value coerces to the column"
    );
    run(
        &mut x,
        query(Rows::All, Select::Assign(vec![set("g", column("d"))])),
    );
    assert_eq!(
        read(&mut x, "g"),
        doubles(&[1.0, 1.0]),
        "a whole column replaces the type"
    );
    run(
        &mut x,
        query(Rows::All, Select::Assign(vec![set("k", column("h"))])),
    );
    assert_eq!(
        read(&mut x, "k"),
        factor(&[2, 1], &["q", "r"]),
        "a factor stays a factor"
    );
}

#[test]
fn a_failing_update_leaves_the_table_unchanged() {
    let mut x = frame(vec![("a", ints(&[1, 2]))]);
    let update = Select::Assign(vec![
        set("a", Expr::Integer(9)),
        (PlSmallStr::from("missing"), None),
    ]);
    assert!(execute(&mut x, query(Rows::All, update)).is_err());
    assert_eq!(read(&mut x, "a"), ints(&[1, 2]));
}

/// A random table for the differential fuzz: grouping columns of every coded type (an integer
/// that is sometimes wide enough to force the hashed layout, a factor and a logical), and value
/// columns with `NA`, `NaN` and integers large enough to overflow a sum.
fn random_table(rng: &mut SplitMix64, rows: usize) -> Table {
    let key_range = if rng.chance(1, 3) {
        3_000_000
    } else {
        1 + rng.below(20)
    };
    let big = rng.chance(1, 5);
    let maybe_na =
        |rng: &mut SplitMix64, value: i32| if rng.chance(1, 12) { NA_INTEGER } else { value };
    let g1: Vec<i32> = (0..rows)
        .map(|_| {
            let value = rng.below(key_range) as i32 - 2;
            maybe_na(rng, value)
        })
        .collect();
    let g2: Vec<i32> = (0..rows)
        .map(|_| {
            let value = 1 + rng.below(4) as i32;
            maybe_na(rng, value)
        })
        .collect();
    let g3: Vec<i32> = (0..rows)
        .map(|_| {
            let value = rng.below(2) as i32;
            maybe_na(rng, value)
        })
        .collect();
    let v: Vec<i32> = (0..rows)
        .map(|_| {
            let value = if big {
                2_000_000_000 - rng.below(1000) as i32
            } else {
                rng.below(200) as i32 - 100
            };
            maybe_na(rng, value)
        })
        .collect();
    let w: Vec<f64> = (0..rows)
        .map(|_| match rng.below(14) {
            0 => NA_REAL,
            1 => f64::NAN,
            _ => rng.below(4000) as f64 / 16.0 - 50.0,
        })
        .collect();
    let z: Vec<i32> = (0..rows)
        .map(|_| {
            let value = rng.below(2) as i32;
            maybe_na(rng, value)
        })
        .collect();
    frame(vec![
        ("g1", ints(&g1)),
        ("g2", factor(&g2, &["d", "a", "c", "b"])),
        ("g3", RVector::Logical(g3)),
        ("v", ints(&v)),
        ("w", doubles(&w)),
        ("z", RVector::Logical(z)),
    ])
}

/// A table to join [`random_table`] to: its keys overlap that table's, and its factor has the
/// levels in another order plus one that table lacks.
fn random_lookup(rng: &mut SplitMix64, rows: usize) -> Table {
    let maybe_na =
        |rng: &mut SplitMix64, value: i32| if rng.chance(1, 10) { NA_INTEGER } else { value };
    let g1: Vec<i32> = (0..rows)
        .map(|_| {
            let value = rng.below(30) as i32 - 4;
            maybe_na(rng, value)
        })
        .collect();
    let g2: Vec<i32> = (0..rows)
        .map(|_| {
            let value = 1 + rng.below(5) as i32;
            maybe_na(rng, value)
        })
        .collect();
    let g3: Vec<i32> = (0..rows)
        .map(|_| {
            let value = rng.below(2) as i32;
            maybe_na(rng, value)
        })
        .collect();
    let q: Vec<f64> = (0..rows).map(|_| rng.below(1000) as f64 / 8.0).collect();
    let v: Vec<i32> = (0..rows).map(|_| rng.below(100) as i32).collect();
    frame(vec![
        ("g1", ints(&g1)),
        ("g2", factor(&g2, &["b", "e", "a", "d", "c"])),
        ("g3", RVector::Logical(g3)),
        ("q", doubles(&q)),
        ("v", ints(&v)),
    ])
}

/// Prices keyed on `(g, t)`, with `t` strictly increasing within each `g`, and trades to roll
/// onto them.
/// Prices keyed on `(g, t)` and trades to roll onto them. The time is an integer or a double, and
/// sometimes repeats within a group, which the rolling kernel leaves to the plan.
fn random_series(rng: &mut SplitMix64, rows: usize) -> (Table, Table) {
    let groups = 1 + rng.below(6) as i32;
    let repeats = rng.chance(1, 4);
    let doubled = rng.chance(1, 3);
    let column_of = |values: Vec<i32>| {
        if doubled {
            doubles(
                &values
                    .iter()
                    .map(|value| f64::from(*value) / 2.0)
                    .collect::<Vec<_>>(),
            )
        } else {
            ints(&values)
        }
    };
    let mut g = Vec::with_capacity(rows);
    let mut t = Vec::with_capacity(rows);
    let mut price = Vec::with_capacity(rows);
    for row in 0..rows {
        g.push(1 + rng.below(groups as usize) as i32);
        let step = if repeats { row / 2 } else { row };
        t.push(step as i32 * 3 + rng.below(3) as i32);
        price.push(rng.below(500) as f64 / 4.0);
    }
    let mut prices = frame(vec![
        ("g", ints(&g)),
        ("t", column_of(t)),
        ("price", doubles(&price)),
    ]);
    prices
        .set_key(&["g", "t"])
        .unwrap_or_else(|error| panic!("cannot key the prices: {error}"));
    let trades_rows = 1 + rng.below(rows.min(5_000));
    let span = rows as u64 * 3 + 20;
    let trade_g: Vec<i32> = (0..trades_rows)
        .map(|_| rng.below(groups as usize + 1) as i32)
        .collect();
    let trade_t: Vec<i32> = (0..trades_rows)
        .map(|_| rng.below(span as usize) as i32 - 10)
        .collect();
    let qty: Vec<i32> = (0..trades_rows).map(|_| rng.below(100) as i32).collect();
    let trades = frame(vec![
        ("g", ints(&trade_g)),
        ("t", column_of(trade_t)),
        ("qty", ints(&qty)),
    ]);
    (prices, trades)
}

fn random_aggregate(rng: &mut SplitMix64) -> Expr {
    if rng.chance(1, 8) {
        return Expr::Count;
    }
    let functions = [
        Aggregate::Sum,
        Aggregate::Mean,
        Aggregate::Min,
        Aggregate::Max,
        Aggregate::First,
        Aggregate::Last,
        Aggregate::Length,
        Aggregate::Median,
        Aggregate::Sd,
        Aggregate::Var,
    ];
    let arg = |rng: &mut SplitMix64| match rng.below(4) {
        0 => column("v"),
        1 => column("w"),
        2 => column("z"),
        _ => binary(BinaryOp::Multiply, column("v"), Expr::Integer(2)),
    };
    if rng.chance(1, 10) {
        let x = arg(rng);
        return Expr::Correlation(Box::new(x), Box::new(arg(rng)));
    }
    Expr::Aggregate {
        function: functions[rng.below(functions.len())],
        arg: Box::new(arg(rng)),
        na_rm: rng.chance(1, 2),
    }
}

fn random_row_wise(rng: &mut SplitMix64, depth: usize) -> Expr {
    if depth == 0 || rng.chance(1, 3) {
        return match rng.below(7) {
            0 => column("v"),
            1 => column("w"),
            2 => column("z"),
            3 => column("g1"),
            4 => Expr::Integer(rng.below(7) as i32 - 3),
            5 => Expr::Double(rng.below(9) as f64 / 2.0 - 2.0),
            _ => Expr::Na(ColumnType::Integer),
        };
    }
    let ops = [
        BinaryOp::Add,
        BinaryOp::Subtract,
        BinaryOp::Multiply,
        BinaryOp::Divide,
        BinaryOp::Power,
        BinaryOp::IntegerDivide,
        BinaryOp::Modulo,
        BinaryOp::Less,
        BinaryOp::Equal,
        BinaryOp::GreaterEqual,
        BinaryOp::And,
        BinaryOp::Or,
    ];
    match rng.below(8) {
        0 => Expr::Unary(UnaryOp::Not, Box::new(random_row_wise(rng, depth - 1))),
        1 => Expr::Unary(UnaryOp::IsNa, Box::new(random_row_wise(rng, depth - 1))),
        2 => Expr::Unary(UnaryOp::Negate, Box::new(random_row_wise(rng, depth - 1))),
        3 => Expr::IfElse(
            Box::new(binary(
                BinaryOp::Greater,
                random_row_wise(rng, depth - 1),
                Expr::Integer(0),
            )),
            Box::new(random_row_wise(rng, depth - 1)),
            Box::new(random_row_wise(rng, depth - 1)),
        ),
        _ => binary(
            ops[rng.below(ops.len())],
            random_row_wise(rng, depth - 1),
            random_row_wise(rng, depth - 1),
        ),
    }
}

/// Two results agree when they have the same columns and values, doubles up to rounding, since
/// the kernels may add a group's values in a different order.
fn assert_same_output(
    kernel: Result<Executed, table::Error>,
    plan: Result<Executed, table::Error>,
    what: &str,
) {
    let (kernel, plan) = match (kernel, plan) {
        (Ok(kernel), Ok(plan)) => {
            assert_eq!(kernel.warnings, plan.warnings, "{what}: warnings");
            (kernel.output, plan.output)
        }
        (Err(_), Err(_)) => return,
        (kernel, plan) => panic!("{what}: one path failed: kernel {kernel:?}, plan {plan:?}"),
    };
    match (kernel, plan) {
        (Output::Table(mut kernel), Output::Table(mut plan)) => {
            assert_eq!(names(&kernel), names(&plan), "{what}: column names");
            assert_eq!(kernel.height(), plan.height(), "{what}: rows");
            for name in names(&plan) {
                assert_same_vector(
                    get(&mut kernel, &name),
                    get(&mut plan, &name),
                    &format!("{what}, column {name}"),
                );
            }
        }
        (Output::Updated, Output::Updated) => {}
        (kernel, plan) => panic!("{what}: different outputs: {kernel:?} and {plan:?}"),
    }
}

fn assert_same_vector(kernel: RVector, plan: RVector, what: &str) {
    match (&kernel, &plan) {
        (RVector::Double(left), RVector::Double(right)) => {
            assert_eq!(left.len(), right.len(), "{what}: length");
            for (row, (left, right)) in left.iter().zip(right).enumerate() {
                let same = if is_na_real(*left) || is_na_real(*right) {
                    is_na_real(*left) && is_na_real(*right)
                } else if left.is_nan() || right.is_nan() {
                    left.is_nan() && right.is_nan()
                } else {
                    // The two sides add in different orders, and `var`, `sd` and `cor` use
                    // different stable algorithms (Welford against two passes), which part ways
                    // in the last digits when the spread is small next to the mean.
                    left == right || (left - right).abs() <= 1e-6 * left.abs().max(right.abs())
                };
                assert!(same, "{what}, row {row}: kernel {left} and plan {right}");
            }
        }
        _ => assert_eq!(kernel, plan, "{what}"),
    }
}

/// The kernels and the Polars plans give the same answer on random grouped queries and
/// updates, at sizes that exercise the serial, parallel, dense and hashed paths.
#[test]
fn fuzz_kernels_agree_with_the_polars_plans() {
    let mut rng = SplitMix64(0x6e4e1);
    for iteration in 0..fuzz_iterations(60) {
        let rows = if iteration % 10 == 9 {
            70_000 + rng.below(70_000)
        } else {
            1 + rng.below(400)
        };
        let table = random_table(&mut rng, rows);
        let keys = ["g1", "g2", "g3"];
        let mut by = Vec::new();
        for key in keys {
            if rng.chance(1, 2) {
                by.push(named(key, column(key)));
            }
        }
        if by.is_empty() {
            by.push(named("g1", column("g1")));
        }
        if rng.chance(1, 2) {
            by.reverse();
        }
        let keyed = rng.chance(1, 3);
        let mut columns = Vec::new();
        for index in 0..1 + rng.below(3) {
            let first = random_aggregate(&mut rng);
            let expr = match rng.below(4) {
                0 => binary(BinaryOp::Subtract, first, random_aggregate(&mut rng)),
                1 => binary(BinaryOp::Add, first, Expr::Integer(1)),
                _ => first,
            };
            columns.push(named(&format!("a{index}"), expr));
        }
        let grouped = Query {
            select: Select::Columns(columns),
            by: by.clone(),
            keyed,
            ..Query::default()
        };
        let what = format!("iteration {iteration}: {grouped:?}");
        assert_same_output(
            execute(&mut table.fork(), grouped.clone()),
            execute_with_plans_only(&mut table.fork(), grouped),
            &what,
        );

        let top = Query {
            rows: Rows::Order(vec![SortKey {
                expr: column("v"),
                descending: rng.chance(1, 2),
            }]),
            select: Select::Columns(vec![named(
                "top",
                Expr::Head(Box::new(column("w")), 1 + rng.below(3)),
            )]),
            by,
            keyed,
        };
        let what = format!("iteration {iteration}: {top:?}");
        assert_same_output(
            execute(&mut table.fork(), top.clone()),
            execute_with_plans_only(&mut table.fork(), top),
            &what,
        );

        let target = ["v", "w", "z", "new"][rng.below(4)];
        let rows_selected = if rng.chance(1, 3) {
            Rows::All
        } else {
            Rows::Filter(match rng.below(3) {
                0 => binary(BinaryOp::Equal, column("g2"), Expr::Character("a".into())),
                1 => binary(
                    BinaryOp::Less,
                    column("v"),
                    Expr::Integer(rng.below(50) as i32),
                ),
                _ => random_row_wise(&mut rng, 2),
            })
        };
        let update = Query {
            rows: rows_selected,
            select: Select::Assign(vec![set(target, random_row_wise(&mut rng, 3))]),
            ..Query::default()
        };
        // A big table joined on a key with few values multiplies its rows, so the lookup stays
        // small when the table is big.
        let lookup_rows = if iteration % 10 == 9 {
            1 + rng.below(8)
        } else {
            1 + rng.below(60)
        };
        let other = random_lookup(&mut rng, lookup_rows);
        let mut on = Vec::new();
        for key in ["g1", "g2", "g3"] {
            if rng.chance(1, 2) {
                on.push(On::equal(key, key));
            }
        }
        if on.is_empty() {
            on.push(On::equal("g2", "g2"));
        }
        let (subject, other) = if rng.chance(1, 4) {
            (other, table.fork())
        } else {
            (table.fork(), other)
        };
        let joined = Query {
            rows: Rows::Join(Join {
                other,
                on,
                drop_unmatched: rng.chance(1, 2),
                roll: Roll::None,
                mult: [Mult::All, Mult::First, Mult::Last][rng.below(3)],
                each_i: false,
            }),
            ..Query::default()
        };
        let what = format!("iteration {iteration}: {joined:?}");
        assert_same_output(
            execute(&mut subject.fork(), joined.clone()),
            execute_with_plans_only(&mut subject.fork(), joined),
            &what,
        );

        let (prices, trades) = random_series(&mut rng, rows);
        let rolled = Query {
            rows: Rows::Join(Join {
                other: trades,
                on: vec![On::equal("g", "g"), On::equal("t", "t")],
                drop_unmatched: rng.chance(1, 3),
                roll: match rng.below(4) {
                    0 => Roll::Forward(None),
                    1 => Roll::Backward(Some(rng.below(20) as f64)),
                    2 => Roll::Forward(Some(-(rng.below(20) as f64))),
                    _ => Roll::Backward(None),
                },
                mult: Mult::All,
                each_i: false,
            }),
            ..Query::default()
        };
        let what = format!("iteration {iteration}: {rolled:?}");
        assert_same_output(
            execute(&mut prices.fork(), rolled.clone()),
            execute_with_plans_only(&mut prices.fork(), rolled),
            &what,
        );

        let (mut kernel, mut plan) = (table.fork(), table.fork());
        let what = format!("iteration {iteration}: {update:?}");
        assert_same_output(
            execute(&mut kernel, update.clone()),
            execute_with_plans_only(&mut plan, update),
            &what,
        );
        assert_eq!(
            names(&kernel),
            names(&plan),
            "{what}: columns after the update"
        );
        for name in names(&plan) {
            assert_same_vector(
                get(&mut kernel, &name),
                get(&mut plan, &name),
                &format!("{what}, column {name}"),
            );
        }
    }
}

#[test]
fn mult_keeps_the_first_or_last_match_as_data_table_does() {
    let x = frame(vec![
        ("k", ints(&[1, 1, 2, 1])),
        ("v", ints(&[10, 20, 30, 40])),
    ]);
    let i = frame(vec![
        ("k", ints(&[2, 1, 3])),
        ("w", doubles(&[5.0, 6.0, 7.0])),
    ]);
    let cases = [
        (Mult::First, false, vec![2, 1, 3], vec![30, 10, NA_INTEGER]),
        (Mult::Last, false, vec![2, 1, 3], vec![30, 40, NA_INTEGER]),
        (Mult::Last, true, vec![2, 1], vec![30, 40]),
    ];
    for (mult, drop_unmatched, keys, values) in cases {
        let query = || Query {
            rows: Rows::Join(Join {
                other: i.clone(),
                on: vec![On::equal("k", "k")],
                drop_unmatched,
                roll: Roll::None,
                mult,
                each_i: false,
            }),
            ..Query::default()
        };
        let kernel = run_table(&mut x.fork(), query());
        let planned =
            match execute_with_plans_only(&mut x.fork(), query()).map(|executed| executed.output) {
                Ok(Output::Table(result)) => result,
                other => panic!("expected a table, got {other:?}"),
            };
        for mut result in [kernel, planned] {
            assert_eq!(names(&result), ["k", "v", "w"]);
            assert_eq!(read(&mut result, "k"), ints(&keys), "{mult:?}");
            assert_eq!(read(&mut result, "v"), ints(&values), "{mult:?}");
        }
    }
}

/// The fuzz builds small tables, so this one is large enough for the grouped median, sd and `cor`
/// kernels to split their rows across threads; the Polars plan is the reference.
#[test]
fn large_grouped_medians_and_moments_match_the_plan() {
    let mut rng = SplitMix64(7);
    let rows = 300_000;
    let groups: Vec<i32> = (0..rows).map(|_| rng.below(500) as i32).collect();
    let x: Vec<f64> = (0..rows)
        .map(|_| {
            if rng.chance(1, 1000) {
                NA_REAL
            } else {
                rng.below(10_000) as f64 / 7.0
            }
        })
        .collect();
    let y: Vec<f64> = (0..rows).map(|_| rng.below(1_000) as f64).collect();
    let z: Vec<i32> = y
        .iter()
        .map(|y| *y as i32 / 3 + rng.below(100) as i32)
        .collect();
    let table = frame(vec![
        ("g", ints(&groups)),
        ("x", doubles(&x)),
        ("y", doubles(&y)),
        ("z", ints(&z)),
    ]);
    let removed = |function| Expr::Aggregate {
        function,
        arg: Box::new(column("x")),
        na_rm: true,
    };
    let query = || Query {
        select: Select::Columns(vec![
            named("md", removed(Aggregate::Median)),
            named("md_na", aggregate(Aggregate::Median, column("x"))),
            named("sd", removed(Aggregate::Sd)),
            named("var", aggregate(Aggregate::Var, column("y"))),
            named(
                "cor",
                Expr::Correlation(Box::new(column("y")), Box::new(column("z"))),
            ),
        ]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    let mut kernel = run_table(&mut table.fork(), query());
    let mut planned =
        match execute_with_plans_only(&mut table.fork(), query()).map(|executed| executed.output) {
            Ok(Output::Table(result)) => result,
            other => panic!("expected a table, got {other:?}"),
        };
    assert_eq!(read(&mut kernel, "g"), get(&mut planned, "g"));
    for name in ["md", "md_na"] {
        assert_eq!(read(&mut kernel, name), get(&mut planned, name), "{name}");
    }
    for name in ["sd", "var", "cor"] {
        let (RVector::Double(left), RVector::Double(right)) =
            (get(&mut kernel, name), get(&mut planned, name))
        else {
            panic!("{name} is not a double column");
        };
        assert_eq!(left.len(), right.len(), "{name}");
        for (left, right) in left.iter().zip(&right) {
            assert_eq!(
                is_na_real(*left),
                is_na_real(*right),
                "{name}: {left} against {right}"
            );
            assert!(
                is_na_real(*left) || (left - right).abs() <= 1e-6 * right.abs().max(1.0),
                "{name}: {left} against {right}"
            );
        }
    }
}

/// data.table 1.18 under R 4.3 gives a double column for `s := sum(v) - 1L` when a total
/// overflows, with or without `by`, because R's `sum` then returns a double.
#[test]
fn a_new_column_nesting_an_overflowing_sum_is_a_double() {
    let x = frame(vec![
        ("g", ints(&[1, 1, 2])),
        ("v", ints(&[2_000_000_000, 2_000_000_000, 1])),
    ]);
    let nested = || {
        binary(
            BinaryOp::Subtract,
            aggregate(Aggregate::Sum, column("v")),
            Expr::Integer(1),
        )
    };
    let update = |rows: Rows, by: &[&str]| {
        let mut updated = x.fork();
        run(
            &mut updated,
            Query {
                rows,
                select: Select::Assign(vec![set("s", nested())]),
                by: by.iter().map(|name| named(name, column(name))).collect(),
                ..Query::default()
            },
        );
        get(&mut updated, "s")
    };
    assert_eq!(
        Read(update(Rows::All, &["g"])),
        doubles(&[3_999_999_999.0, 3_999_999_999.0, 0.0])
    );
    assert_eq!(Read(update(Rows::All, &[])), doubles(&[4e9, 4e9, 4e9]));
    let small = || Rows::Filter(binary(BinaryOp::Equal, column("g"), Expr::Integer(2)));
    assert_eq!(
        Read(update(small(), &[])),
        ints(&[NA_INTEGER, NA_INTEGER, 0]),
        "a sum that fits stays integer"
    );
    let large = Rows::Filter(binary(BinaryOp::Equal, column("g"), Expr::Integer(1)));
    assert_eq!(
        Read(update(large, &["g"])),
        doubles(&[3_999_999_999.0, 3_999_999_999.0, NA_REAL])
    );
}

/// A comparison over an overflowing sum keeps its own type: R's `sum(v) > 0L` is a logical `TRUE`
/// when the sum is a double, and the column must not become a double.
#[test]
fn a_new_column_comparing_an_overflowing_sum_stays_logical() {
    let x = frame(vec![
        ("g", ints(&[1, 1, 2])),
        ("v", ints(&[2_000_000_000, 2_000_000_000, 1])),
    ]);
    for by in [vec![], vec![named("g", column("g"))]] {
        let mut updated = x.fork();
        run(
            &mut updated,
            Query {
                select: Select::Assign(vec![set(
                    "big",
                    binary(
                        BinaryOp::Greater,
                        aggregate(Aggregate::Sum, column("v")),
                        Expr::Integer(0),
                    ),
                )]),
                by,
                ..Query::default()
            },
        );
        assert_eq!(read(&mut updated, "big"), RVector::Logical(vec![1, 1, 1]));
    }
}

/// A join the kernels decline, here on a double key, still stages a nested sum over the joined
/// rows, so `sum(v) + 1L` is a double when a total overflows, as after the join kernel.
#[test]
fn a_nested_sum_after_a_planned_join_widens_on_overflow() {
    let x = frame(vec![
        ("k", doubles(&[1.0, 1.0, 2.0])),
        ("v", ints(&[2_000_000_000, 2_000_000_000, 1])),
    ]);
    let i = frame(vec![("k", doubles(&[1.0, 2.0]))]);
    let query = || Query {
        rows: Rows::Join(Join {
            other: i.clone(),
            on: vec![On::equal("k", "k")],
            drop_unmatched: false,
            roll: Roll::None,
            mult: Mult::All,
            each_i: false,
        }),
        select: Select::Columns(vec![named(
            "s",
            binary(
                BinaryOp::Add,
                aggregate(Aggregate::Sum, column("v")),
                Expr::Integer(1),
            ),
        )]),
        by: vec![named("k", column("k"))],
        ..Query::default()
    };
    let kernel = run_table(&mut x.fork(), query());
    let planned =
        match execute_with_plans_only(&mut x.fork(), query()).map(|executed| executed.output) {
            Ok(Output::Table(result)) => result,
            other => panic!("expected a table, got {other:?}"),
        };
    for mut result in [kernel, planned] {
        assert_eq!(read(&mut result, "s"), doubles(&[4_000_000_001.0, 2.0]));
    }
}

fn non_equi_x() -> Table {
    frame(vec![
        ("id", ints(&[1, 1, 2, NA_INTEGER, 1])),
        ("t", doubles(&[5.0, NA_REAL, 3.0, 4.0, f64::NAN])),
        ("v", ints(&[1, 2, 3, 4, 5])),
    ])
}

fn non_equi_i() -> Table {
    frame(vec![
        ("id", ints(&[1, NA_INTEGER, 2, 1])),
        ("lo", doubles(&[1.0, 1.0, NA_REAL, f64::NAN])),
        ("hi", doubles(&[10.0, 10.0, 10.0, 10.0])),
    ])
}

fn on(conditions: &[(&str, Comparison, &str)]) -> Vec<On> {
    conditions
        .iter()
        .map(|(x, op, i)| On::new(*x, *op, *i))
        .collect()
}

fn non_equi(i: &Table, conditions: &[(&str, Comparison, &str)], mult: Mult) -> Join {
    Join {
        mult,
        ..Join::new(i.clone(), on(conditions))
    }
}

/// Runs a query through the kernels and through the plans, checks that both agree, and gives the
/// kernels' table.
fn run_both(table: &mut Table, query: Query) -> Table {
    let what = format!("{query:?}");
    assert_same_output(
        execute(&mut table.fork(), query.clone()),
        execute_with_plans_only(&mut table.fork(), query.clone()),
        &what,
    );
    run_table(table, query)
}

#[test]
fn a_non_equi_join_follows_data_tables_layout_and_missing_values() {
    use Comparison::{GreaterEqual, Less};
    let (mut x, i) = (non_equi_x(), non_equi_i());
    let join = non_equi(
        &i,
        &[
            ("id", Comparison::Equal, "id"),
            ("t", GreaterEqual, "lo"),
            ("t", Less, "hi"),
        ],
        Mult::All,
    );
    let mut result = run_both(
        &mut x,
        Query {
            rows: Rows::Join(join),
            ..Query::default()
        },
    );
    assert_eq!(names(&result), ["id", "t", "v", "t.1"]);
    assert_eq!(read(&mut result, "id"), ints(&[1, NA_INTEGER, 2, 1]));
    assert_eq!(
        read(&mut result, "t"),
        doubles(&[1.0, 1.0, NA_REAL, f64::NAN])
    );
    assert_eq!(
        read(&mut result, "v"),
        ints(&[1, 4, NA_INTEGER, NA_INTEGER])
    );
    assert_eq!(read(&mut result, "t.1"), doubles(&[10.0; 4]));

    let mut result = run_both(
        &mut x,
        Query {
            rows: Rows::Join(non_equi(&i, &[("t", Comparison::Greater, "lo")], Mult::All)),
            ..Query::default()
        },
    );
    assert_eq!(names(&result), ["id", "t", "v", "i.id", "hi"]);
    assert_eq!(
        read(&mut result, "v"),
        ints(&[1, 3, 4, 1, 3, 4, NA_INTEGER, NA_INTEGER])
    );
    assert_eq!(
        read(&mut result, "i.id"),
        ints(&[1, 1, 1, NA_INTEGER, NA_INTEGER, NA_INTEGER, 2, 1])
    );
    assert_eq!(
        read(&mut result, "id"),
        ints(&[1, 2, NA_INTEGER, 1, 2, NA_INTEGER, NA_INTEGER, NA_INTEGER])
    );

    let mut result = run_both(
        &mut x,
        Query {
            rows: Rows::Join(non_equi(
                &i,
                &[("id", Comparison::Equal, "id"), ("t", GreaterEqual, "lo")],
                Mult::Last,
            )),
            ..Query::default()
        },
    );
    assert_eq!(read(&mut result, "v"), ints(&[1, 4, NA_INTEGER, 5]));
}

#[test]
fn an_ordering_condition_matches_a_missing_value_only_inclusively() {
    let mut x = frame(vec![
        (
            "t",
            doubles(&[NA_REAL, f64::NAN, 1.0, f64::NEG_INFINITY, f64::INFINITY]),
        ),
        ("v", ints(&[1, 2, 3, 4, 5])),
    ]);
    let i = frame(vec![
        (
            "lo",
            doubles(&[NA_REAL, f64::NAN, 1.0, f64::NEG_INFINITY, f64::INFINITY]),
        ),
        ("k", ints(&[1, 2, 3, 4, 5])),
    ]);
    let n = NA_INTEGER;
    let cases = [
        (
            Comparison::GreaterEqual,
            vec![1, 2, 3, 3, 4, 4, 4, 5],
            vec![1, 2, 3, 5, 3, 4, 5, 5],
        ),
        (
            Comparison::Greater,
            vec![1, 2, 3, 4, 4, 5],
            vec![n, n, 5, 3, 5, n],
        ),
        (
            Comparison::LessEqual,
            vec![1, 2, 3, 3, 4, 5, 5, 5],
            vec![1, 2, 3, 4, 4, 3, 4, 5],
        ),
        (
            Comparison::Less,
            vec![1, 2, 3, 4, 5, 5],
            vec![n, n, 4, n, 3, 4],
        ),
    ];
    for (op, keys, values) in cases {
        let mut result = run_both(
            &mut x,
            Query {
                rows: Rows::Join(non_equi(&i, &[("t", op, "lo")], Mult::All)),
                ..Query::default()
            },
        );
        assert_eq!(read(&mut result, "k"), ints(&keys), "{op:?}");
        assert_eq!(read(&mut result, "v"), ints(&values), "{op:?}");
    }
}

#[test]
fn a_non_equi_join_compares_numbers_only() {
    let mut x = frame(vec![("s", strings(&["b", "a"])), ("t", ints(&[1, 2]))]);
    let i = frame(vec![("lo", strings(&["a"])), ("n", ints(&[0]))]);
    let error = execute(
        &mut x,
        Query {
            rows: Rows::Join(non_equi(&i, &[("s", Comparison::Greater, "lo")], Mult::All)),
            ..Query::default()
        },
    )
    .err()
    .map(|error| error.to_string());
    assert_eq!(
        error.as_deref(),
        Some("invalid query: the join condition `s > lo` compares numbers, but `s` is character[]")
    );
    let rolled = Join {
        roll: Roll::Backward(None),
        ..non_equi(&i, &[("t", Comparison::Greater, "n")], Mult::All)
    };
    let error = execute(
        &mut x,
        Query {
            rows: Rows::Join(rolled),
            ..Query::default()
        },
    )
    .err()
    .map(|error| error.to_string());
    assert_eq!(
        error.as_deref(),
        Some("invalid query: a rolling join needs every join condition to be `==`")
    );
}

fn each_i(join: Join) -> Rows {
    Rows::Join(Join {
        each_i: true,
        ..join
    })
}

#[test]
fn each_i_runs_j_once_per_row_of_i() {
    use Comparison::{Equal, GreaterEqual, LessEqual};
    let (mut x, i) = (non_equi_x(), non_equi_i());
    let count_and_sum = || {
        Select::Columns(vec![
            named("n", Expr::Count),
            named("s", aggregate(Aggregate::Sum, column("v"))),
        ])
    };
    let mut result = run_both(
        &mut x,
        Query {
            rows: each_i(non_equi(
                &i,
                &[("id", Equal, "id"), ("t", GreaterEqual, "lo")],
                Mult::All,
            )),
            select: count_and_sum(),
            ..Query::default()
        },
    );
    assert_eq!(names(&result), ["id", "t", "n", "s"]);
    assert_eq!(read(&mut result, "id"), ints(&[1, NA_INTEGER, 2, 1]));
    assert_eq!(
        read(&mut result, "t"),
        doubles(&[1.0, 1.0, NA_REAL, f64::NAN])
    );
    assert_eq!(read(&mut result, "n"), ints(&[1, 1, 0, 1]));
    assert_eq!(read(&mut result, "s"), ints(&[1, 4, NA_INTEGER, 5]));

    let mut select = match count_and_sum() {
        Select::Columns(columns) => columns,
        _ => Vec::new(),
    };
    select.push(named("i.id", column("i.id")));
    let mut result = run_both(
        &mut x,
        Query {
            rows: each_i(non_equi(&i, &[("t", GreaterEqual, "lo")], Mult::All)),
            select: Select::Columns(select),
            ..Query::default()
        },
    );
    assert_eq!(names(&result), ["t", "n", "s", "i.id"]);
    assert_eq!(read(&mut result, "n"), ints(&[3, 3, 1, 1]));
    assert_eq!(read(&mut result, "s"), ints(&[8, 8, 2, 5]));
    assert_eq!(read(&mut result, "i.id"), ints(&[1, NA_INTEGER, 2, 1]));

    let mut result = run_both(
        &mut x,
        Query {
            rows: each_i(non_equi(
                &i,
                &[("t", LessEqual, "hi"), ("t", GreaterEqual, "lo")],
                Mult::All,
            )),
            select: Select::Columns(vec![named("n", Expr::Count)]),
            ..Query::default()
        },
    );
    // data.table names both join columns `t`; a frame's names are unique, so the second is `t.1`,
    // as in the join's own layout.
    assert_eq!(names(&result), ["t", "t.1", "n"]);
    assert_eq!(read(&mut result, "t"), doubles(&[10.0; 4]));
    assert_eq!(
        read(&mut result, "t.1"),
        doubles(&[1.0, 1.0, NA_REAL, f64::NAN])
    );
    assert_eq!(read(&mut result, "n"), ints(&[3, 3, 0, 0]));
}

#[test]
fn each_i_on_an_equi_join_keeps_duplicate_rows_of_i_apart() {
    let (mut x, i) = (non_equi_x(), non_equi_i());
    let equal = || non_equi(&i, &[("id", Comparison::Equal, "id")], Mult::All);
    let mut result = run_both(
        &mut x,
        Query {
            rows: each_i(equal()),
            select: Select::Value(aggregate(Aggregate::Sum, column("v"))),
            ..Query::default()
        },
    );
    assert_eq!(names(&result), ["id", "V1"]);
    assert_eq!(read(&mut result, "id"), ints(&[1, NA_INTEGER, 2, 1]));
    assert_eq!(read(&mut result, "V1"), ints(&[8, 4, 3, 8]));

    let mut result = run_both(
        &mut x,
        Query {
            rows: each_i(equal()),
            select: Select::Columns(vec![named("v", column("v"))]),
            ..Query::default()
        },
    );
    assert_eq!(
        read(&mut result, "id"),
        ints(&[1, 1, 1, NA_INTEGER, 2, 1, 1, 1])
    );
    assert_eq!(read(&mut result, "v"), ints(&[1, 2, 5, 4, 3, 1, 2, 5]));

    let dropped = Join {
        drop_unmatched: true,
        ..non_equi(&i, &[("t", Comparison::Greater, "lo")], Mult::All)
    };
    let mut result = run_both(
        &mut x,
        Query {
            rows: each_i(dropped),
            select: Select::Columns(vec![named("n", Expr::Count)]),
            ..Query::default()
        },
    );
    assert_eq!(read(&mut result, "n"), ints(&[3, 3]));
}

#[test]
fn each_i_widens_an_overflowing_sum() {
    let mut x = frame(vec![
        ("t", ints(&[1, 5, 3])),
        ("v", ints(&[i32::MAX, 1, 5])),
    ]);
    let i = frame(vec![("lo", ints(&[0]))]);
    let query = Query {
        rows: each_i(non_equi(
            &i,
            &[("t", Comparison::GreaterEqual, "lo")],
            Mult::All,
        )),
        select: Select::Columns(vec![named("s", aggregate(Aggregate::Sum, column("v")))]),
        ..Query::default()
    };
    assert_eq!(
        result_type(&x.schema(), &[], &query).ok(),
        Some(ResultType::Table(Schema::new([
            ("t", ColumnType::Integer),
            ("s", ColumnType::Integer),
        ])))
    );
    let mut result = run_both(&mut x, query);
    assert_eq!(read(&mut result, "t"), ints(&[0]));
    assert_eq!(read(&mut result, "s"), doubles(&[2_147_483_653.0]));
}

#[test]
fn non_equi_joins_match_the_plan_on_random_tables() {
    let mut rng = SplitMix64(0x006e_6f6e_6571_7569);
    let value = |rng: &mut SplitMix64| match rng.below(12) {
        0 => NA_REAL,
        1 => f64::NAN,
        _ => rng.below(9) as f64 - 2.0,
    };
    let int_value = |rng: &mut SplitMix64| {
        if rng.chance(1, 10) {
            NA_INTEGER
        } else {
            rng.below(5) as i32
        }
    };
    for iteration in 0..fuzz_iterations(200) {
        let (x_rows, i_rows) = (rng.below(40), 1 + rng.below(12));
        let x = frame(vec![
            (
                "g",
                ints(&(0..x_rows).map(|_| int_value(&mut rng)).collect::<Vec<_>>()),
            ),
            (
                "a",
                doubles(&(0..x_rows).map(|_| value(&mut rng)).collect::<Vec<_>>()),
            ),
            (
                "b",
                ints(&(0..x_rows).map(|_| int_value(&mut rng)).collect::<Vec<_>>()),
            ),
            ("v", ints(&(0..x_rows as i32).collect::<Vec<_>>())),
        ]);
        let i = frame(vec![
            (
                "g",
                ints(&(0..i_rows).map(|_| int_value(&mut rng)).collect::<Vec<_>>()),
            ),
            (
                "lo",
                doubles(&(0..i_rows).map(|_| value(&mut rng)).collect::<Vec<_>>()),
            ),
            (
                "hi",
                ints(&(0..i_rows).map(|_| int_value(&mut rng)).collect::<Vec<_>>()),
            ),
            ("k", ints(&(0..i_rows as i32).collect::<Vec<_>>())),
        ]);
        let ops = [
            Comparison::Less,
            Comparison::LessEqual,
            Comparison::Greater,
            Comparison::GreaterEqual,
        ];
        let mut conditions = Vec::new();
        if rng.chance(1, 2) {
            conditions.push(On::equal("g", "g"));
        }
        let x_columns = ["a", "b"];
        let i_columns = ["lo", "hi"];
        for _ in 0..1 + rng.below(3) {
            conditions.push(On::new(
                x_columns[rng.below(2)],
                ops[rng.below(4)],
                i_columns[rng.below(2)],
            ));
        }
        let join = Join {
            drop_unmatched: rng.chance(1, 3),
            mult: [Mult::All, Mult::First, Mult::Last][rng.below(3)],
            ..Join::new(i, conditions)
        };
        let joined = Query {
            rows: Rows::Join(join.clone()),
            ..Query::default()
        };
        let what = format!("iteration {iteration}: {joined:?}");
        assert_same_output(
            execute(&mut x.fork(), joined.clone()),
            execute_with_plans_only(&mut x.fork(), joined),
            &what,
        );
        let grouped = Query {
            rows: each_i(join),
            select: Select::Columns(vec![
                named("n", Expr::Count),
                named("s", aggregate(Aggregate::Sum, column("v"))),
                named("k", column("k")),
            ]),
            ..Query::default()
        };
        let what = format!("iteration {iteration}: {grouped:?}");
        assert_same_output(
            execute(&mut x.fork(), grouped.clone()),
            execute_with_plans_only(&mut x.fork(), grouped),
            &what,
        );
    }
}

/// The warnings `query` raises, after checking that the kernels and the plans raise the same ones.
fn warnings(table: &Table, query: Query) -> Vec<String> {
    let what = format!("{query:?}");
    assert_same_output(
        execute(&mut table.fork(), query.clone()),
        execute_with_plans_only(&mut table.fork(), query.clone()),
        &what,
    );
    execute(&mut table.fork(), query)
        .unwrap_or_else(|error| panic!("query failed: {error}"))
        .warnings
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn a_grouped_sum_that_overflows_warns_once_per_column() {
    let x = frame(vec![
        ("g", ints(&[1, 1, 2])),
        ("v", ints(&[i32::MAX, 1, 5])),
        ("w", ints(&[i32::MAX, 2, 1])),
    ]);
    let sums = || {
        vec![
            named("v", aggregate(Aggregate::Sum, column("v"))),
            named("w", aggregate(Aggregate::Sum, column("w"))),
        ]
    };
    let expected = [
        "a group's sum in `v` is too large for an integer, so `v` holds doubles",
        "a group's sum in `w` is too large for an integer, so `w` holds doubles",
    ];
    for keyby in [false, true] {
        let query = Query {
            select: Select::Columns(sums()),
            by: vec![named("g", column("g"))],
            keyed: keyby,
            ..Query::default()
        };
        assert_eq!(warnings(&x, query), expected, "keyby {keyby}");
    }
    let assigned = Query {
        select: Select::Assign(vec![set("s", aggregate(Aggregate::Sum, column("v")))]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    assert_eq!(
        warnings(&x, assigned),
        ["a group's sum in `s` is too large for an integer, so `s` holds doubles"]
    );
}

#[test]
fn a_sum_warns_only_as_a_plain_grouped_column() {
    let x = frame(vec![
        ("g", ints(&[1, 1, 2])),
        ("v", ints(&[i32::MAX, 1, 5])),
    ]);
    let ungrouped = Query {
        select: Select::Columns(vec![named("s", aggregate(Aggregate::Sum, column("v")))]),
        ..Query::default()
    };
    assert_eq!(warnings(&x, ungrouped), Vec::<String>::new());
    let nested = Query {
        select: Select::Columns(vec![named(
            "s",
            binary(
                BinaryOp::Subtract,
                aggregate(Aggregate::Sum, column("v")),
                Expr::Integer(1),
            ),
        )]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    assert_eq!(warnings(&x, nested), Vec::<String>::new());
    let fits = Query {
        select: Select::Columns(vec![named("s", aggregate(Aggregate::Sum, column("g")))]),
        by: vec![named("g", column("g"))],
        ..Query::default()
    };
    assert_eq!(warnings(&x, fits), Vec::<String>::new());
    let i = frame(vec![("lo", ints(&[0]))]);
    let each = Query {
        rows: each_i(non_equi(
            &i,
            &[("g", Comparison::GreaterEqual, "lo")],
            Mult::All,
        )),
        select: Select::Columns(vec![named("s", aggregate(Aggregate::Sum, column("v")))]),
        ..Query::default()
    };
    assert_eq!(warnings(&x, each), Vec::<String>::new());
}

#[test]
fn assigning_a_narrower_value_warns_about_the_conversion() {
    let x = frame(vec![
        ("w", ints(&[1, 2, 3])),
        ("b", RVector::Logical(vec![1, 0, 1])),
    ]);
    let assign = |name: &str, value: Expr| Query {
        rows: Rows::Filter(binary(BinaryOp::Greater, column("w"), Expr::Integer(1))),
        select: Select::Assign(vec![set(name, value)]),
        ..Query::default()
    };
    assert_eq!(
        warnings(&x, assign("w", Expr::Double(3.5))),
        ["column `w` holds integers, so the assigned value 3.5 became 3"]
    );
    let mut updated = x.fork();
    run(&mut updated, assign("w", Expr::Double(1e10)));
    assert_eq!(read(&mut updated, "w"), ints(&[1, NA_INTEGER, NA_INTEGER]));
    assert_eq!(
        warnings(&x, assign("w", Expr::Double(1e10))),
        ["column `w` holds integers, so the assigned value 1e+10 became NA"]
    );
    assert_eq!(
        warnings(&x, assign("w", Expr::Double(4.0))),
        Vec::<String>::new()
    );
    assert_eq!(
        warnings(&x, assign("b", Expr::Integer(5))),
        ["column `b` holds logicals, so the assigned value 5 became TRUE"]
    );
}
