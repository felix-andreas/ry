//! The query IR for `tbl[i, j, by]`, and its lowering to a Polars lazy plan.
//!
//! The interpreter builds a [`Query`] from the R call: `i` becomes [`Rows`], `j` becomes
//! [`Select`], and `by` or `keyby` become the grouping. Lowering is typed. Every expression is
//! typed against the table's [`Schema`] first, which checks column references and gives each
//! operation its R semantics (integer overflow to `NA`, `/` producing a double, `NA` propagating
//! through an aggregate unless `na.rm = TRUE`), and only then becomes a Polars expression. The
//! planner then sees the whole query, so every aggregation, data.table's GForce set included,
//! runs as a grouped Polars kernel rather than as R code once per group.

use crate::convert::{enum_dtype, factor_levels};
use crate::kernel::{Evaluator, Vector, scatter, selected_rows};
use crate::{ColumnType, Error, Result, Schema, Table};
use crate::{group, join};
use polars::prelude::{
    AnyValue, AsOfOptions, AsofStrategy, ChunkAgg, Column, DataFrame, DataType, ExplodeOptions,
    Expr as PolarsExpr, IntoLazy, JoinType, LazyFrame, MaintainOrderJoin, NULL, NamedFrom,
    PlSmallStr, Scalar, Series, SortMultipleOptions, by_name, col, len, lit, pearson_corr, when,
};
use std::collections::HashSet;
use std::fmt;

/// One `tbl[i, j, by]` call.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub rows: Rows,
    pub select: Select,
    /// The grouping columns, named. `by = g` is `("g", Column("g"))`, and `by = .(y = year(d))`
    /// names a computed group.
    pub by: Vec<(PlSmallStr, Expr)>,
    /// `keyby` rather than `by`: the result is sorted by the groups and keyed on them, where `by`
    /// keeps the groups in order of first appearance.
    pub keyed: bool,
}

/// The `i` part: which rows, in which order.
#[derive(Clone, Debug, Default)]
pub enum Rows {
    #[default]
    All,
    /// A logical row filter. An `NA` drops the row, as in data.table.
    Filter(Expr),
    /// `tbl[order(a, -b)]`. The sort is stable and puts `NA` last, as `order` does.
    Order(Vec<SortKey>),
    /// Zero-based row positions, already resolved from R's one-based or negative indexing.
    Positions(Vec<u32>),
    /// `x[i, on = ...]` with another table as `i`.
    Join(Join),
}

#[derive(Clone, Debug)]
pub struct SortKey {
    pub expr: Expr,
    pub descending: bool,
}

/// A join of the subject `x` with the table in `i`. The result follows data.table: one row per
/// match, in `i`'s order and then `x`'s, with `x`'s columns first and `i`'s other columns after
/// them. [`join_layout`] gives the exact columns.
#[derive(Clone, Debug)]
pub struct Join {
    /// `i`, the table whose rows are looked up in `x`.
    pub other: Table,
    /// The conditions a row of `x` must meet to match a row of `i`, all of them. Empty means `x`'s
    /// key, matched for equality in order against `i`'s key when `i` is keyed and against its
    /// leading columns otherwise, as data.table matches a join without `on`.
    pub on: Vec<On>,
    /// `nomatch = NULL`: drop the rows of `i` that match nothing, which makes the join inner.
    pub drop_unmatched: bool,
    pub roll: Roll,
    pub mult: Mult,
    /// `by = .EACHI`: `j` runs once for each row of `i` over the rows of `x` it matched, and the
    /// result starts with the join columns. A row of `i` that matched nothing sees a single row of
    /// `NA`s in `x`'s columns, with `.N` as 0. The query's own `by` must be empty.
    pub each_i: bool,
}

impl Join {
    /// An equi join on `on`, keeping every match and the rows of `i` that matched nothing.
    pub fn new(other: Table, on: Vec<On>) -> Join {
        Join {
            other,
            on,
            drop_unmatched: false,
            roll: Roll::None,
            mult: Mult::All,
            each_i: false,
        }
    }
}

/// One condition of `on`: `x op i`, comparing a column of `x` with a column of `i`.
/// `on = .(id, t >= lo)` is `id == id` and `t >= lo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct On {
    pub x: PlSmallStr,
    pub op: Comparison,
    pub i: PlSmallStr,
}

impl On {
    pub fn equal(x: impl Into<PlSmallStr>, i: impl Into<PlSmallStr>) -> On {
        On {
            x: x.into(),
            op: Comparison::Equal,
            i: i.into(),
        }
    }

    pub fn new(x: impl Into<PlSmallStr>, op: Comparison, i: impl Into<PlSmallStr>) -> On {
        On {
            x: x.into(),
            op,
            i: i.into(),
        }
    }
}

/// The operator of a join condition. An ordering compares numbers only, as data.table's non-equi
/// joins do: an `NA` or `NaN` meets an inclusive operator (`<=`, `>=`) against the same missing
/// value and nothing else, and never meets a strict one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl Comparison {
    pub fn symbol(self) -> &'static str {
        match self {
            Comparison::Equal => "==",
            Comparison::Less => "<",
            Comparison::LessEqual => "<=",
            Comparison::Greater => ">",
            Comparison::GreaterEqual => ">=",
        }
    }
}

/// data.table's `mult =` argument: which of several rows of `x` matching one row of `i` to keep.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mult {
    #[default]
    All,
    First,
    Last,
}

/// data.table's `roll =` argument. The last join column is the one that rolls.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Roll {
    #[default]
    None,
    /// `roll = TRUE` (last observation carried forward), or a positive number limiting how far.
    Backward(Option<f64>),
    /// `roll = -Inf` (next observation carried backward), or a negative number limiting how far.
    Forward(Option<f64>),
    /// `roll = "nearest"`.
    Nearest,
}

/// The `j` part.
#[derive(Clone, Debug, Default)]
pub enum Select {
    /// No `j`: the selected rows, all columns.
    #[default]
    All,
    /// `.(a = e1, b = e2)` or `list(...)`: a table with these columns, after the groups.
    Columns(Vec<(PlSmallStr, Expr)>),
    /// A bare expression such as `tbl[, x]` or `tbl[, sum(x)]` without `by`: a vector.
    Value(Expr),
    /// `:=`. `None` deletes the column, as `x := NULL` does.
    Assign(Vec<(PlSmallStr, Option<Expr>)>),
}

/// An expression inside a query. Polars' own expression type is imported as `PolarsExpr`, which
/// this lowers to.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Column(PlSmallStr),
    Integer(i32),
    Double(f64),
    Logical(bool),
    Character(String),
    /// A typed `NA`.
    Na(ColumnType),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Aggregate {
        function: Aggregate,
        arg: Box<Expr>,
        na_rm: bool,
    },
    /// `.N`, the number of rows in the group.
    Count,
    /// `head(x, n)`, which keeps the group's first `n` values rather than collapsing them.
    Head(Box<Expr>, usize),
    /// `cor(x, y)`, Pearson's correlation.
    Correlation(Box<Expr>, Box<Expr>),
    /// `fifelse(test, yes, no)` and `ifelse`.
    IfElse(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
    IsNa,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    IntegerDivide,
    Modulo,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}

/// The aggregations data.table's GForce optimizes, which all run as grouped Polars kernels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    Sum,
    Mean,
    Min,
    Max,
    First,
    Last,
    Median,
    Sd,
    Var,
    Length,
}

/// Shorthands for building expressions in tests and in the interpreter's lowering.
pub mod build {
    use super::{Aggregate, BinaryOp, Expr};

    pub fn column(name: &str) -> Expr {
        Expr::Column(name.into())
    }

    pub fn aggregate(function: Aggregate, arg: Expr) -> Expr {
        Expr::Aggregate {
            function,
            arg: Box::new(arg),
            na_rm: false,
        }
    }

    pub fn binary(op: BinaryOp, left: Expr, right: Expr) -> Expr {
        Expr::Binary(op, Box::new(left), Box::new(right))
    }
}

/// What a query produces.
#[derive(Debug)]
pub enum Output {
    Table(Table),
    Vector(Series),
    /// A `:=` query, which updated the subject in place. R returns the subject invisibly.
    Updated,
}

/// What a query gives back: its output, and the warnings it raised on the way, which the
/// interpreter signals as R warnings after the query succeeds.
#[derive(Debug)]
pub struct Executed {
    pub output: Output,
    pub warnings: Vec<Warning>,
}

/// A warning a query raises while still succeeding, as data.table warns in the same places.
#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// A grouped `sum` of integers, written as a column of `j` or of `:=` on its own, had a total
    /// beyond the integer range, so the column holds doubles.
    SumWidened { column: PlSmallStr },
    /// `:=` wrote numbers into a narrower column without replacing its type, and the conversion
    /// changed at least one of them: a double truncated or out of an integer's range, or a number
    /// other than 0 or 1 read as a logical. `value` is the first changed value as written, and
    /// `stored` what the column holds in its place.
    Converted {
        column: PlSmallStr,
        to: ColumnType,
        value: String,
        stored: String,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::SumWidened { column } => write!(
                formatter,
                "a group's sum in `{column}` is too large for an integer, so `{column}` holds doubles"
            ),
            Warning::Converted {
                column,
                to,
                value,
                stored,
            } => {
                let holds = match to {
                    ColumnType::Logical => "logicals",
                    ColumnType::Integer => "integers",
                    ColumnType::Double => "doubles",
                    ColumnType::Character => "strings",
                    ColumnType::Factor => "factor levels",
                };
                write!(
                    formatter,
                    "column `{column}` holds {holds}, so the assigned value {value} became {stored}"
                )
            }
        }
    }
}

/// Runs `query` against `table`. A `:=` query updates `table` in place, so the caller must pass
/// an unshared table, forking a shared one first, as copy-on-modify requires; every other query
/// leaves `table` unchanged.
pub fn execute(table: &mut Table, query: Query) -> Result<Executed> {
    execute_with(table, query, true)
}

/// [`execute`] with every query running as a Polars plan, bypassing the kernels. Both must give
/// the same answer, which the differential tests check through this.
#[doc(hidden)]
pub fn execute_with_plans_only(table: &mut Table, query: Query) -> Result<Executed> {
    execute_with(table, query, false)
}

fn execute_with(table: &mut Table, query: Query, kernels: bool) -> Result<Executed> {
    let sums = grouped_integer_sums(&table.schema(), table.key(), &query)?;
    let mut warnings = Vec::new();
    let output = run(table, query, kernels, &mut warnings)?;
    let schema = match &output {
        Output::Table(result) => result.schema(),
        Output::Updated => table.schema(),
        Output::Vector(_) => return Ok(Executed { output, warnings }),
    };
    warnings.extend(
        sums.into_iter()
            .filter(|column| schema.column(column) == Some(ColumnType::Double))
            .map(|column| Warning::SumWidened { column }),
    );
    Ok(Executed { output, warnings })
}

/// The columns a grouped query computes as a `sum` of integers on its own, which data.table's
/// grouped sum widens to doubles with a warning when a total overflows. A sum nested in an
/// expression, or one in `by = .EACHI`, widens silently, as in data.table. A `:=` sum written into
/// an existing column takes that column's type instead. After a join, `j` reads the joined rows,
/// whose join columns carry `i`'s types.
fn grouped_integer_sums(
    schema: &Schema,
    key: &[PlSmallStr],
    query: &Query,
) -> Result<Vec<PlSmallStr>> {
    if query.by.is_empty() {
        return Ok(Vec::new());
    }
    let rows_schema = match &query.rows {
        Rows::Join(join) => join_schema(schema, &join.other.schema(), &join_columns(join, key)?)?,
        _ => schema.clone(),
    };
    let typer = Typer {
        schema: &rows_schema,
    };
    let integer_sum = |expr: &Expr| -> Result<bool> {
        Ok(match expr {
            Expr::Aggregate {
                function: Aggregate::Sum,
                arg,
                ..
            } => typer.expr(arg)? != ColumnType::Double,
            _ => false,
        })
    };
    let mut sums = Vec::new();
    match &query.select {
        Select::All => {}
        Select::Value(expr) => {
            if integer_sum(expr)? {
                sums.push(PlSmallStr::from_static("V1"));
            }
        }
        Select::Columns(columns) => {
            for (name, expr) in columns {
                if integer_sum(expr)? {
                    sums.push(name.clone());
                }
            }
        }
        Select::Assign(assignments) => {
            for (name, expr) in assignments {
                if let Some(expr) = expr
                    && schema.column(name).is_none()
                    && integer_sum(expr)?
                {
                    sums.push(name.clone());
                }
            }
        }
    }
    Ok(sums)
}

fn run(
    table: &mut Table,
    query: Query,
    kernels: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Output> {
    let schema = table.schema();
    if let Select::Assign(assignments) = &query.select {
        return assign(
            table,
            &schema,
            &query.rows,
            &query.by,
            assignments,
            kernels,
            warnings,
        )
        .map(|()| Output::Updated);
    }
    if let Rows::Join(join) = &query.rows {
        let on = join_columns(join, table.key())?;
        let types = comparison_types(&schema, &join.other.schema(), join, &on)?;
        if join.each_i {
            return each_i(table, on, query, kernels, warnings);
        }
        if kernels && let Some(frame) = join::join(table, join, &on, &types)? {
            let mut joined = Table::from_normalized(frame);
            let query = Query {
                rows: Rows::All,
                ..query
            };
            if matches!(query.select, Select::All) && query.by.is_empty() {
                return Ok(Output::Table(joined));
            }
            return run(&mut joined, query, kernels, warnings);
        }
    }
    if matches!(query.rows, Rows::Join(_)) && nests_integer_sum(&schema, &query.select)? {
        // One plan would fix a nested sum's type before seeing its totals, so the join runs
        // first and the sum is staged over the joined rows, as it is after the join kernel.
        let joined = plan(
            table,
            Query {
                rows: query.rows.clone(),
                ..Query::default()
            },
        )?
        .collect()?;
        let query = Query {
            rows: Rows::All,
            ..query
        };
        return run(&mut Table::new(joined)?, query, kernels, warnings);
    }
    let value = matches!(query.select, Select::Value(_)) && query.by.is_empty();
    let keyed = query.keyed;
    let key: Vec<PlSmallStr> = if keyed {
        query.by.iter().map(|(name, _)| name.clone()).collect()
    } else {
        Vec::new()
    };
    // Removing rows keeps the remaining ones sorted, and so do positions that never go back, so
    // the subject's key still holds.
    let keeps_rows = matches!(query.select, Select::All)
        && match &query.rows {
            Rows::All | Rows::Filter(_) => true,
            Rows::Positions(positions) => positions.is_sorted(),
            Rows::Order(_) | Rows::Join(_) => false,
        };
    let kernel = if kernels {
        group::aggregate(table, &schema, &query)?
    } else {
        None
    };
    let frame = match kernel {
        Some(frame) => frame,
        None => match staged(table, &schema, &query)? {
            Some(frame) => frame,
            None => narrow_sums(plan(table, query)?.collect()?)?,
        },
    };
    if value {
        let column = frame
            .columns()
            .first()
            .ok_or_else(|| Error::Query("the query selected nothing".to_string()))?;
        return Ok(Output::Vector(column.as_materialized_series().clone()));
    }
    let mut result = Table::new(frame)?;
    if keyed {
        result = result.with_key(key);
    } else if keeps_rows {
        result = result.with_key(table.key().to_vec());
    }
    Ok(Output::Table(result))
}

/// `by = .EACHI`: the join runs first, with `i`'s rows numbered and `x`'s rows marked, and then
/// `j` runs grouped by the row of `i` and the join columns. An unmatched row of `i` joins to a row
/// of `NA`s without the mark, so counting the marks gives its `.N` of 0.
fn each_i(
    table: &Table,
    on: Vec<On>,
    query: Query,
    kernels: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Output> {
    let Query {
        rows: Rows::Join(join),
        select,
        by,
        ..
    } = query
    else {
        return Err(Error::Query("`by = .EACHI` needs a join".to_string()));
    };
    if !by.is_empty() {
        return Err(Error::Query(
            "`by = .EACHI` cannot be combined with other groups".to_string(),
        ));
    }
    let columns = match select {
        Select::Columns(columns) => columns,
        Select::Value(expr) => vec![(PlSmallStr::from_static("V1"), expr)],
        Select::All | Select::Assign(_) => {
            return Err(Error::Query("`by = .EACHI` needs a `j`".to_string()));
        }
    };
    let mut x = table.frame().clone();
    x.with_column(Column::new(MATCHED.into(), vec![1i32; x.height()]))?;
    let mut i = join.other.frame().clone();
    let i_rows = i32::try_from(i.height())
        .map_err(|_| Error::Query("`by = .EACHI` takes at most 2^31 rows of `i`".to_string()))?;
    i.with_column(Column::new(
        ROW_OF_I.into(),
        (0..i_rows).collect::<Vec<i32>>(),
    ))?;
    let i = Table::from_normalized(i);
    let layout = join_layout(&x.get_column_names(), &i.frame().get_column_names(), &on);
    let rows = Rows::Join(Join {
        other: i,
        on,
        each_i: false,
        ..join
    });
    let mut x = Table::from_normalized(x);
    let Output::Table(mut joined) = run(
        &mut x,
        Query {
            rows,
            ..Query::default()
        },
        kernels,
        warnings,
    )?
    else {
        return Err(Error::Query("a join gave no table".to_string()));
    };
    let mut groups: Vec<&JoinColumn> = layout
        .iter()
        .filter(|column| column.condition.is_some())
        .collect();
    groups.sort_by_key(|column| column.condition);
    let mut by = vec![(
        PlSmallStr::from_static(ROW_OF_I),
        Expr::Column(ROW_OF_I.into()),
    )];
    by.extend(
        groups
            .into_iter()
            .map(|column| (column.name.clone(), Expr::Column(column.name.clone()))),
    );
    let i_columns: Vec<&PlSmallStr> = layout
        .iter()
        .filter(|column| column.side == Side::I && column.condition.is_none())
        .map(|column| &column.name)
        .collect();
    let select = columns
        .iter()
        .map(|(name, expr)| (name.clone(), per_row_of_i(expr, &i_columns, true)))
        .collect();
    let grouped = Query {
        rows: Rows::All,
        select: Select::Columns(select),
        by,
        keyed: false,
    };
    let Output::Table(result) = run(&mut joined, grouped, kernels, warnings)? else {
        return Err(Error::Query("a grouping gave no table".to_string()));
    };
    Ok(Output::Table(Table::new(result.frame().drop(ROW_OF_I)?)?))
}

/// A hidden column of `x` holding 1 in every row, which tells a matched row from an unmatched one
/// after a join.
const MATCHED: &str = "\u{1}matched";
/// A hidden column numbering the rows of `i` for `by = .EACHI`, apart from the plans' [`I_ROW`].
const ROW_OF_I: &str = "\u{1}row_of_i";

/// `j` rewritten for `by = .EACHI`. `.N` counts the rows of `x` the row of `i` matched, as
/// `sum(matched, na.rm = TRUE)`, and a column of `i` outside an aggregate is a single value, as in
/// data.table, so it becomes the group's `first`: every row of a group carries the same row of `i`.
fn per_row_of_i(expr: &Expr, i_columns: &[&PlSmallStr], outside_aggregate: bool) -> Expr {
    let rewrite = |arg: &Expr| Box::new(per_row_of_i(arg, i_columns, outside_aggregate));
    match expr {
        Expr::Count => Expr::Aggregate {
            function: Aggregate::Sum,
            arg: Box::new(Expr::Column(MATCHED.into())),
            na_rm: true,
        },
        Expr::Column(name) if outside_aggregate && i_columns.contains(&name) => Expr::Aggregate {
            function: Aggregate::First,
            arg: Box::new(expr.clone()),
            na_rm: false,
        },
        Expr::Aggregate {
            function,
            arg,
            na_rm,
        } => Expr::Aggregate {
            function: *function,
            arg: Box::new(per_row_of_i(arg, i_columns, false)),
            na_rm: *na_rm,
        },
        Expr::Correlation(left, right) => Expr::Correlation(
            Box::new(per_row_of_i(left, i_columns, false)),
            Box::new(per_row_of_i(right, i_columns, false)),
        ),
        Expr::Unary(op, arg) => Expr::Unary(*op, rewrite(arg)),
        Expr::Head(arg, rows) => Expr::Head(rewrite(arg), *rows),
        Expr::Binary(op, left, right) => Expr::Binary(*op, rewrite(left), rewrite(right)),
        Expr::IfElse(test, yes, no) => Expr::IfElse(rewrite(test), rewrite(yes), rewrite(no)),
        Expr::Integer(_)
        | Expr::Double(_)
        | Expr::Logical(_)
        | Expr::Character(_)
        | Expr::Na(_)
        | Expr::Column(_) => expr.clone(),
    }
}

/// R's integer `sum` is an integer when the total fits and a double when it does not, as both
/// base R and data.table's grouped sum return. The lowering sums integers in 64 bits, and frames
/// never hold `Int64` otherwise, so every `Int64` column of a result is such a sum.
fn narrow_sums(frame: DataFrame) -> Result<DataFrame> {
    let height = frame.height();
    let mut columns = Vec::with_capacity(frame.width());
    for column in frame.into_columns() {
        if column.dtype() != &DataType::Int64 {
            columns.push(column);
            continue;
        }
        let values = column.as_materialized_series().i64()?;
        // `i32::MIN` is R's `NA_integer_`, so a total of exactly that is out of range too.
        let fits = |value: Option<i64>| {
            value.is_none_or(|value| value > i64::from(i32::MIN) && value <= i64::from(i32::MAX))
        };
        let target = if fits(values.min()) && fits(values.max()) {
            DataType::Int32
        } else {
            DataType::Float64
        };
        columns.push(column.cast(&target)?);
    }
    Ok(DataFrame::new(height, columns)?)
}

/// Evaluates a query whose output nests an integer `sum` inside an expression, such as
/// `sum(v) - 1L`, in two stages: the aggregates first, each integer sum narrowed to an integer or a
/// double once its totals are known, and then the expressions over them. A single plan would have
/// to fix the sum's type before seeing the data, and R's sum is a double exactly when a total
/// overflows. `None` when the query has no such sum or an output does not collapse per group.
fn staged(table: &Table, schema: &Schema, query: &Query) -> Result<Option<DataFrame>> {
    let outputs: Vec<(PlSmallStr, &Expr)> = match &query.select {
        Select::Value(expr) => vec![(PlSmallStr::from_static("V1"), expr)],
        Select::Columns(columns) => columns
            .iter()
            .map(|(name, expr)| (name.clone(), expr))
            .collect(),
        Select::All | Select::Assign(_) => return Ok(None),
    };
    if !nests_integer_sum(schema, &query.select)? || matches!(query.rows, Rows::Join(_)) {
        return Ok(None);
    }
    let mut aggregates = Vec::new();
    let mut rewritten = Vec::with_capacity(outputs.len());
    for (name, expr) in &outputs {
        match group::rewrite(expr, &mut aggregates) {
            Some(expr) => rewritten.push((name.clone(), expr)),
            None => return Ok(None),
        }
    }
    let nulls = NullCounts::of(table.frame());
    let (plan, rows_schema) = rows(
        table.frame().clone().lazy(),
        schema,
        table.key(),
        &nulls,
        &query.rows,
    )?;
    let lower = Lowering {
        schema: &rows_schema,
        nulls: &nulls,
    };
    let temporaries: Vec<(PlSmallStr, Expr)> = aggregates
        .into_iter()
        .enumerate()
        .map(|(index, expr)| (group::temporary(index), expr))
        .collect();
    let first = if query.by.is_empty() {
        let mut exprs = Vec::with_capacity(temporaries.len());
        for (name, expr) in &temporaries {
            exprs.push(lower.expr(expr)?.alias(name.clone()));
        }
        plan.select(exprs)
    } else {
        grouped(plan, &lower, &query.by, query.keyed, &temporaries)?
    };
    let first = narrow_sums(first.collect()?)?;
    let first_schema = Table::new(first.clone())?.schema();
    let first_nulls = NullCounts::of(&first);
    let lower = Lowering {
        schema: &first_schema,
        nulls: &first_nulls,
    };
    let mut exprs = Vec::with_capacity(query.by.len() + rewritten.len());
    for (name, _) in &query.by {
        exprs.push(col(name.clone()));
    }
    for (name, expr) in &rewritten {
        exprs.push(lower.expr(expr)?.alias(name.clone()));
    }
    Ok(Some(first.lazy().select(exprs).collect()?))
}

/// Whether an output of `j` nests an integer `sum` inside an expression, as `sum(v) - 1L` does.
fn nests_integer_sum(schema: &Schema, select: &Select) -> Result<bool> {
    let typer = Typer { schema };
    let outputs: Vec<&Expr> = match select {
        Select::Value(expr) => vec![expr],
        Select::Columns(columns) => columns.iter().map(|(_, expr)| expr).collect(),
        Select::All | Select::Assign(_) => return Ok(false),
    };
    for expr in outputs {
        if !matches!(expr, Expr::Aggregate { .. }) && has_integer_sum(&typer, expr)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `expr` sums an integer or logical vector anywhere.
fn has_integer_sum(typer: &Typer, expr: &Expr) -> Result<bool> {
    Ok(match expr {
        Expr::Aggregate {
            function: Aggregate::Sum,
            arg,
            ..
        } => typer.expr(arg)? != ColumnType::Double,
        Expr::Aggregate { arg, .. } | Expr::Unary(_, arg) | Expr::Head(arg, _) => {
            has_integer_sum(typer, arg)?
        }
        Expr::Binary(_, left, right) | Expr::Correlation(left, right) => {
            has_integer_sum(typer, left)? || has_integer_sum(typer, right)?
        }
        Expr::IfElse(test, yes, no) => {
            has_integer_sum(typer, test)?
                || has_integer_sum(typer, yes)?
                || has_integer_sum(typer, no)?
        }
        Expr::Integer(_)
        | Expr::Double(_)
        | Expr::Logical(_)
        | Expr::Character(_)
        | Expr::Na(_)
        | Expr::Column(_)
        | Expr::Count => false,
    })
}

/// Collects the integer `sum`s inside an expression.
fn integer_sums(typer: &Typer, expr: &Expr, sums: &mut Vec<Expr>) -> Result<()> {
    match expr {
        Expr::Aggregate {
            function: Aggregate::Sum,
            arg,
            ..
        } if typer.expr(arg)? != ColumnType::Double => sums.push(expr.clone()),
        Expr::Aggregate { arg, .. } | Expr::Unary(_, arg) | Expr::Head(arg, _) => {
            integer_sums(typer, arg, sums)?;
        }
        Expr::Binary(_, left, right) | Expr::Correlation(left, right) => {
            integer_sums(typer, left, sums)?;
            integer_sums(typer, right, sums)?;
        }
        Expr::IfElse(test, yes, no) => {
            integer_sums(typer, test, sums)?;
            integer_sums(typer, yes, sums)?;
            integer_sums(typer, no, sums)?;
        }
        Expr::Integer(_)
        | Expr::Double(_)
        | Expr::Logical(_)
        | Expr::Character(_)
        | Expr::Na(_)
        | Expr::Column(_)
        | Expr::Count => {}
    }
    Ok(())
}

/// The expression with each integer `sum(x)` taken over doubles, as `sum(x * 1.0)`, which is what
/// R's `sum` returns when the total overflows.
fn double_sums(typer: &Typer, expr: &Expr) -> Result<Expr> {
    let double = |arg: &Expr| double_sums(typer, arg).map(Box::new);
    Ok(match expr {
        Expr::Aggregate {
            function: Aggregate::Sum,
            arg,
            na_rm,
        } if typer.expr(arg)? != ColumnType::Double => Expr::Aggregate {
            function: Aggregate::Sum,
            arg: Box::new(Expr::Binary(
                BinaryOp::Multiply,
                double(arg)?,
                Box::new(Expr::Double(1.0)),
            )),
            na_rm: *na_rm,
        },
        Expr::Aggregate {
            function,
            arg,
            na_rm,
        } => Expr::Aggregate {
            function: *function,
            arg: double(arg)?,
            na_rm: *na_rm,
        },
        Expr::Unary(op, arg) => Expr::Unary(*op, double(arg)?),
        Expr::Head(arg, count) => Expr::Head(double(arg)?, *count),
        Expr::Binary(op, left, right) => Expr::Binary(*op, double(left)?, double(right)?),
        Expr::Correlation(left, right) => Expr::Correlation(double(left)?, double(right)?),
        Expr::IfElse(test, yes, no) => Expr::IfElse(double(test)?, double(yes)?, double(no)?),
        Expr::Integer(_)
        | Expr::Double(_)
        | Expr::Logical(_)
        | Expr::Character(_)
        | Expr::Na(_)
        | Expr::Column(_)
        | Expr::Count => expr.clone(),
    })
}

/// The lazy plan for a query that does not assign, so tests and `explain` can inspect what the
/// planner receives.
pub fn plan(table: &Table, query: Query) -> Result<LazyFrame> {
    if let Rows::Join(join) = &query.rows
        && join.each_i
    {
        return Err(Error::Query(
            "`by = .EACHI` runs as a join and then a grouping, so it has no single plan"
                .to_string(),
        ));
    }
    let schema = table.schema();
    let key = table.key().to_vec();
    let frame = table.frame().clone();
    let nulls = NullCounts::of(&frame);
    let (plan, schema) = rows(frame.lazy(), &schema, &key, &nulls, &query.rows)?;
    // A join changes which columns may hold nulls, so its output is treated as nullable.
    let nulls = if matches!(query.rows, Rows::Join(_)) {
        NullCounts::default()
    } else {
        nulls
    };
    let lower = Lowering {
        schema: &schema,
        nulls: &nulls,
    };
    match query.select {
        Select::All => {
            if !query.by.is_empty() {
                return Err(Error::Query("`by` needs a `j`".to_string()));
            }
            Ok(plan)
        }
        Select::Value(expr) if query.by.is_empty() => {
            Ok(plan.select([lower.expr(&expr)?.alias("V1")]))
        }
        Select::Value(expr) => grouped(
            plan,
            &lower,
            &query.by,
            query.keyed,
            &[(PlSmallStr::from_static("V1"), expr)],
        ),
        Select::Columns(columns) if query.by.is_empty() => {
            let mut exprs = Vec::with_capacity(columns.len());
            for (name, expr) in &columns {
                exprs.push(lower.expr(expr)?.alias(name.clone()));
            }
            Ok(plan.select(exprs))
        }
        Select::Columns(columns) => grouped(plan, &lower, &query.by, query.keyed, &columns),
        Select::Assign(_) => Err(Error::Query(
            "a `:=` query updates the table and has no plan of its own".to_string(),
        )),
    }
}

/// The type of each column a query produces, or of the vector it returns: the runtime
/// counterpart of the checker's result types, used to check a query before it runs. `key` is the
/// subject's key, which a join without `on` matches on.
///
/// An integer `sum` types as integer, although it yields a double when the total overflows.
pub fn result_type(schema: &Schema, key: &[PlSmallStr], query: &Query) -> Result<ResultType> {
    let typer = Typer { schema };
    let rows_schema = match &query.rows {
        Rows::Join(join) => {
            let i_schema = join.other.schema();
            let on = join_columns(join, key)?;
            comparison_types(schema, &i_schema, join, &on)?;
            join_schema(schema, &i_schema, &on)?
        }
        Rows::Filter(filter) => {
            expect_logical(typer.expr(filter)?, "a row filter")?;
            schema.clone()
        }
        Rows::Order(keys) => {
            for key in keys {
                typer.expr(&key.expr)?;
            }
            schema.clone()
        }
        Rows::All | Rows::Positions(_) => schema.clone(),
    };
    let typer = Typer {
        schema: &rows_schema,
    };
    let mut columns = Vec::new();
    let each_i = matches!(&query.rows, Rows::Join(join) if join.each_i);
    if let Rows::Join(join) = &query.rows
        && join.each_i
    {
        if !query.by.is_empty() || matches!(query.select, Select::All | Select::Assign(_)) {
            return Err(Error::Query(
                "`by = .EACHI` needs a `j` and no other groups".to_string(),
            ));
        }
        let on = join_columns(join, key)?;
        let mut groups: Vec<JoinColumn> =
            join_layout(&schema.names(), &join.other.schema().names(), &on)
                .into_iter()
                .filter(|column| column.condition.is_some())
                .collect();
        groups.sort_by_key(|column| column.condition);
        for column in groups {
            columns.push((column.name.clone(), typer.expr(&Expr::Column(column.name))?));
        }
    }
    for (name, expr) in &query.by {
        columns.push((name.clone(), typer.expr(expr)?));
    }
    match &query.select {
        Select::All => Ok(ResultType::Table(rows_schema)),
        Select::Value(expr) if query.by.is_empty() && !each_i => {
            Ok(ResultType::Vector(typer.expr(expr)?))
        }
        Select::Value(expr) => {
            columns.push((PlSmallStr::from_static("V1"), typer.expr(expr)?));
            Ok(ResultType::Table(Schema { columns }))
        }
        Select::Columns(selected) => {
            for (name, expr) in selected {
                columns.push((name.clone(), typer.expr(expr)?));
            }
            Ok(ResultType::Table(Schema { columns }))
        }
        Select::Assign(assignments) => {
            let mut columns = schema.columns.clone();
            for (name, expr) in assignments {
                match expr {
                    None => columns.retain(|(column, _)| column != name),
                    Some(expr) => {
                        let replaces = matches!(query.rows, Rows::All)
                            && query.by.is_empty()
                            && !collapses(expr);
                        let kind = typer.expr(expr)?;
                        match columns.iter_mut().find(|(column, _)| column == name) {
                            Some((_, existing)) => {
                                *existing = assigned_type(Some(*existing), kind, replaces)?;
                            }
                            None => columns.push((name.clone(), kind)),
                        }
                    }
                }
            }
            Ok(ResultType::Table(Schema { columns }))
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ResultType {
    Table(Schema),
    Vector(ColumnType),
}

/// Applies `i` to the plan and returns the schema of the rows it selects.
fn rows(
    plan: LazyFrame,
    schema: &Schema,
    key: &[PlSmallStr],
    nulls: &NullCounts,
    rows: &Rows,
) -> Result<(LazyFrame, Schema)> {
    let lower = Lowering { schema, nulls };
    Ok(match rows {
        Rows::All => (plan, schema.clone()),
        Rows::Filter(filter) => {
            expect_logical(Typer { schema }.expr(filter)?, "a row filter")?;
            (plan.filter(lower.expr(filter)?), schema.clone())
        }
        Rows::Order(keys) => {
            let typer = Typer { schema };
            let mut exprs = Vec::with_capacity(keys.len());
            let mut descending = Vec::with_capacity(keys.len());
            for key in keys {
                let mut value = lower.expr(&key.expr)?;
                // Polars sorts `NaN` above every number, so a descending sort would put it first,
                // where R's `order` puts it last with `NA`. Sorting on the value with `NaN` read as
                // null fixes that and keeps a single key on Polars' fast single-column sort.
                if typer.expr(&key.expr)? == ColumnType::Double {
                    value = value.fill_nan(lit(NULL));
                }
                exprs.push(value);
                descending.push(key.descending);
            }
            let options = SortMultipleOptions::default()
                .with_order_descending_multi(descending)
                .with_nulls_last(true)
                .with_maintain_order(true);
            (plan.sort_by_exprs(exprs, options), schema.clone())
        }
        Rows::Positions(positions) => {
            let index = Series::new(PlSmallStr::from_static("index"), positions.as_slice());
            let frame = plan.collect()?;
            let taken = frame.take(index.idx()?)?;
            (taken.lazy(), schema.clone())
        }
        Rows::Join(join) => {
            let on = join_columns(join, key)?;
            let i_schema = join.other.schema();
            let types = comparison_types(schema, &i_schema, join, &on)?;
            let i_plan = join.other.frame().clone().lazy();
            let joined = if on.iter().all(|condition| condition.op == Comparison::Equal) {
                join_plan(plan, schema, i_plan, &i_schema, join, &on, &types)?
            } else {
                non_equi_plan(plan, schema, i_plan, &i_schema, join, &on, &types)?
            };
            (joined, join_schema(schema, &i_schema, &on)?)
        }
    })
}

/// The conditions a join matches on. Without `on`, `x`'s key columns pair for equality in order
/// with `i`'s key columns, or with its leading columns when `i` has no key, and a shorter `i`
/// joins on a prefix of `x`'s key.
fn join_columns(join: &Join, key: &[PlSmallStr]) -> Result<Vec<On>> {
    if !join.on.is_empty() {
        return Ok(join.on.clone());
    }
    if key.is_empty() {
        return Err(Error::Query(
            "a join needs `on =` or a keyed table".to_string(),
        ));
    }
    let i_columns: Vec<&PlSmallStr> = if join.other.key().is_empty() {
        join.other.frame().get_column_names()
    } else {
        join.other.key().iter().collect()
    };
    Ok(key
        .iter()
        .zip(i_columns)
        .map(|(x_column, i_column)| On::equal(x_column.clone(), i_column.clone()))
        .collect())
}

/// Checks a join's conditions against both schemas, and gives for each condition the type both
/// sides compare in when their types differ. Integers meet doubles as doubles, and two factors, or
/// a factor and a string, meet as strings, since factor codes index different level sets.
fn comparison_types(
    x: &Schema,
    i: &Schema,
    join: &Join,
    on: &[On],
) -> Result<Vec<Option<DataType>>> {
    if join.roll != Roll::None && on.iter().any(|condition| condition.op != Comparison::Equal) {
        return Err(Error::Query(
            "a rolling join needs every join condition to be `==`".to_string(),
        ));
    }
    let numeric = |kind: ColumnType| {
        matches!(
            kind,
            ColumnType::Logical | ColumnType::Integer | ColumnType::Double
        )
    };
    on.iter()
        .map(|condition| {
            let x_type = x
                .column(&condition.x)
                .ok_or_else(|| Error::NoColumn(condition.x.to_string()))?;
            let i_type = i
                .column(&condition.i)
                .ok_or_else(|| Error::NoColumn(condition.i.to_string()))?;
            if condition.op != Comparison::Equal && !(numeric(x_type) && numeric(i_type)) {
                let (column, kind) = if numeric(x_type) {
                    (&condition.i, i_type)
                } else {
                    (&condition.x, x_type)
                };
                return Err(Error::Query(format!(
                    "the join condition `{} {} {}` compares numbers, but `{column}` is {}",
                    condition.x,
                    condition.op.symbol(),
                    condition.i,
                    kind.name()
                )));
            }
            Ok(match (x_type, i_type) {
                (ColumnType::Factor, ColumnType::Factor | ColumnType::Character)
                | (ColumnType::Character, ColumnType::Factor) => Some(DataType::String),
                (ColumnType::Integer, ColumnType::Double)
                | (ColumnType::Double, ColumnType::Integer) => Some(DataType::Float64),
                (x_type, i_type) if x_type == i_type => None,
                (x_type, i_type) => {
                    return Err(Error::Query(format!(
                        "cannot join {} column `{}` to {} column `{}`",
                        x_type.name(),
                        condition.x,
                        i_type.name(),
                        condition.i
                    )));
                }
            })
        })
        .collect()
}

/// Which input a column of a join's output takes its values from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    X,
    I,
}

/// One column of a join's output.
#[derive(Clone, Debug)]
pub(crate) struct JoinColumn {
    pub(crate) name: PlSmallStr,
    pub(crate) side: Side,
    /// The column of `side` the values come from.
    pub(crate) source: PlSmallStr,
    /// The index of the condition whose `i` values the column carries, for a join column.
    pub(crate) condition: Option<usize>,
}

/// data.table's layout of a join's output, the single description the plan, the kernel and the
/// result type all follow. `x`'s columns come first, and a column a condition compares carries the
/// values of `i`'s column in its first condition. A further condition on the same column of `x`
/// adds a column `name.1`, `name.2`, ... after `x`'s columns, carrying that condition's `i`
/// values. Then come `i`'s columns no condition uses, an `i.` prefix marking a name `x` has.
/// A name that still repeats takes a further numeric suffix.
pub(crate) fn join_layout(x: &[&PlSmallStr], i: &[&PlSmallStr], on: &[On]) -> Vec<JoinColumn> {
    let mut layout = Vec::with_capacity(x.len() + i.len());
    for column in x {
        let condition = on.iter().position(|condition| condition.x == **column);
        layout.push(match condition {
            Some(index) => JoinColumn {
                name: (*column).clone(),
                side: Side::I,
                source: on[index].i.clone(),
                condition,
            },
            None => JoinColumn {
                name: (*column).clone(),
                side: Side::X,
                source: (*column).clone(),
                condition: None,
            },
        });
    }
    for (index, condition) in on.iter().enumerate() {
        let earlier = on[..index]
            .iter()
            .filter(|earlier| earlier.x == condition.x)
            .count();
        if earlier > 0 {
            layout.push(JoinColumn {
                name: format!("{}.{earlier}", condition.x).into(),
                side: Side::I,
                source: condition.i.clone(),
                condition: Some(index),
            });
        }
    }
    for column in i {
        if on.iter().any(|condition| condition.i == **column) {
            continue;
        }
        let name = if x.contains(column) {
            format!("{I_PREFIX}{column}").into()
        } else {
            (*column).clone()
        };
        layout.push(JoinColumn {
            name,
            side: Side::I,
            source: (*column).clone(),
            condition: None,
        });
    }
    // A generated name can collide with a real column, as `t.1` does when `x` has a column
    // `t.1`, so a repeated name takes the first free numeric suffix, as R's `make.unique` does.
    let mut taken: HashSet<PlSmallStr> = layout.iter().map(|column| column.name.clone()).collect();
    let mut seen = HashSet::with_capacity(layout.len());
    for column in &mut layout {
        if seen.insert(column.name.clone()) {
            continue;
        }
        let name = (1..)
            .map(|suffix| PlSmallStr::from(format!("{}.{suffix}", column.name)))
            .find(|name| !taken.contains(name))
            .unwrap_or_else(|| column.name.clone());
        taken.insert(name.clone());
        seen.insert(name.clone());
        column.name = name;
    }
    layout
}

/// The prefix data.table gives a column of `i` whose name `x` already uses.
pub(crate) const I_PREFIX: &str = "i.";
/// The suffix Polars gives the clashing column of the right side, chosen so that it cannot
/// collide with a real column name.
const CLASH_SUFFIX: &str = "\u{1}x";
/// A hidden column numbering the rows of `i`.
const I_ROW: &str = "\u{1}i_row";
/// A hidden column numbering the rows of `x`.
const X_ROW: &str = "\u{1}x_row";

fn join_plan(
    x: LazyFrame,
    x_schema: &Schema,
    i: LazyFrame,
    i_schema: &Schema,
    join: &Join,
    on: &[On],
    types: &[Option<DataType>],
) -> Result<LazyFrame> {
    let mut x = x;
    let mut i = i;
    // The result's join column is cast back to `i`'s type, because it carries `i`'s values.
    let mut restore = Vec::new();
    for (condition, common) in on.iter().zip(types) {
        let Some(common) = common else {
            continue;
        };
        let i_dtype = column_dtype(&i, &condition.i)?;
        x = x.with_column(col(condition.x.clone()).cast(common.clone()));
        i = i.with_column(col(condition.i.clone()).cast(common.clone()));
        restore.push((condition.i.clone(), i_dtype));
    }
    let left_on: Vec<PolarsExpr> = on
        .iter()
        .map(|condition| col(condition.i.clone()))
        .collect();
    let right_on: Vec<PolarsExpr> = on
        .iter()
        .map(|condition| col(condition.x.clone()))
        .collect();
    let rolling = match join.roll {
        Roll::None => None,
        Roll::Backward(limit) => Some((AsofStrategy::Backward, limit)),
        Roll::Forward(limit) => Some((AsofStrategy::Forward, limit)),
        Roll::Nearest => Some((AsofStrategy::Nearest, None)),
    };
    // `mult = "first"` or `"last"` keeps one match per row of `i`, and the matches of a row are
    // adjacent in the join's output, so a numbered `i` tells where each row's matches start and end.
    let i = if join.mult == Mult::All || rolling.is_some() {
        i
    } else {
        i.with_row_index(I_ROW, None)
    };
    let joined = match rolling {
        // data.table matches an `NA` key to an `NA` key, which Polars does only when asked.
        None => i
            .join_builder()
            .with(x)
            .left_on(left_on)
            .right_on(right_on)
            .how(if join.drop_unmatched {
                JoinType::Inner
            } else {
                JoinType::Left
            })
            .join_nulls(true)
            .maintain_order(MaintainOrderJoin::LeftRight)
            .suffix(CLASH_SUFFIX)
            .finish(),
        Some((strategy, tolerance)) => {
            let rolled = on
                .last()
                .ok_or_else(|| Error::Query("a rolling join needs a join column".to_string()))?;
            let (x_roll, i_roll) = (&rolled.x, &rolled.i);
            let groups = &on[..on.len() - 1];
            let options = AsOfOptions {
                strategy,
                tolerance: tolerance.map(|limit| Scalar::from(limit.abs())),
                tolerance_str: None,
                left_by: (!groups.is_empty())
                    .then(|| groups.iter().map(|condition| condition.i.clone()).collect()),
                right_by: (!groups.is_empty())
                    .then(|| groups.iter().map(|condition| condition.x.clone()).collect()),
                allow_eq: true,
                check_sortedness: false,
            };
            // An as-of join walks both sides in order of the rolling column within each group, so
            // `i` is sorted first and restored to its own order afterwards. `x` is sorted the same
            // way, which is free when it is keyed on these columns.
            const ROW: &str = "\u{1}row";
            let mut sort_i: Vec<PolarsExpr> = groups
                .iter()
                .map(|condition| col(condition.i.clone()))
                .collect();
            sort_i.push(col(i_roll.clone()));
            let mut sort_x: Vec<PolarsExpr> = groups
                .iter()
                .map(|condition| col(condition.x.clone()))
                .collect();
            sort_x.push(col(x_roll.clone()));
            let options_sort = SortMultipleOptions::default()
                .with_maintain_order(true)
                .with_nulls_last(true);
            // Marks the rows of `x` so that an unmatched row of `i` is told apart from a match
            // whose values are `NA`.
            const MATCHED: &str = "\u{1}matched";
            let i = i
                .with_row_index(ROW, None)
                .sort_by_exprs(sort_i, options_sort.clone());
            let x = x
                .with_column(lit(true).alias(MATCHED))
                .sort_by_exprs(sort_x, options_sort);
            let mut matched = i
                .join_builder()
                .with(x)
                .left_on([col(i_roll.clone())])
                .right_on([col(x_roll.clone())])
                .how(JoinType::AsOf(Box::new(options)))
                .suffix(CLASH_SUFFIX)
                .finish()
                .sort([ROW], Default::default())
                .drop(by_name([ROW], true, false));
            if join.drop_unmatched {
                matched = matched.filter(col(MATCHED).is_not_null());
            }
            matched
        }
    };
    let joined = match rolling {
        Some(_) => joined,
        None => keep_mult(joined, join.mult),
    };
    Ok(joined.select(layout_exprs(x_schema, i_schema, on, &restore)))
}

/// The plan for a join with an ordering condition. It is the reference the join kernel is tested
/// against, since the kernel runs every such join: the rows of `i` pair with the rows of `x` that
/// meet the equality conditions (every row of `x` when there are none), the pairs that fail an
/// ordering condition drop, and the surviving rows of `x` gather in `i`'s order and then `x`'s.
fn non_equi_plan(
    x: LazyFrame,
    x_schema: &Schema,
    i: LazyFrame,
    i_schema: &Schema,
    join: &Join,
    on: &[On],
    types: &[Option<DataType>],
) -> Result<LazyFrame> {
    let x = x.with_row_index(X_ROW, None);
    let i = i.with_row_index(I_ROW, None);
    let key = |side: &str, index: usize| format!("\u{1}{side}{index}");
    let mut x_keys = vec![col(X_ROW)];
    let mut i_keys = vec![col(I_ROW)];
    let (mut left_on, mut right_on) = (Vec::new(), Vec::new());
    let mut holds = lit(true);
    for (index, (condition, common)) in on.iter().zip(types).enumerate() {
        let (x_key, i_key) = (key("x", index), key("i", index));
        let common = match condition.op {
            Comparison::Equal => common.clone(),
            _ => Some(DataType::Float64),
        };
        let (mut x_value, mut i_value) = (col(condition.x.clone()), col(condition.i.clone()));
        if let Some(common) = common {
            x_value = x_value.cast(common.clone());
            i_value = i_value.cast(common);
        }
        x_keys.push(x_value.alias(x_key.clone()));
        i_keys.push(i_value.alias(i_key.clone()));
        let (x_value, i_value) = (col(x_key.clone()), col(i_key.clone()));
        let ordered = |compare: PolarsExpr| {
            x_value
                .clone()
                .is_not_nan()
                .and(i_value.clone().is_not_nan())
                .and(compare)
        };
        let same_missing = (x_value.clone().is_null().and(i_value.clone().is_null()))
            .or(x_value.clone().is_nan().and(i_value.clone().is_nan()));
        let met = match condition.op {
            Comparison::Equal => {
                left_on.push(col(i_key));
                right_on.push(col(x_key));
                continue;
            }
            Comparison::Less => ordered(x_value.clone().lt(i_value.clone())),
            Comparison::Greater => ordered(x_value.clone().gt(i_value.clone())),
            Comparison::LessEqual => {
                ordered(x_value.clone().lt_eq(i_value.clone())).or(same_missing)
            }
            Comparison::GreaterEqual => {
                ordered(x_value.clone().gt_eq(i_value.clone())).or(same_missing)
            }
        };
        holds = holds.and(met.fill_null(lit(false)));
    }
    if left_on.is_empty() {
        // Every row of `i` pairs with every row of `x`.
        const ALL: &str = "\u{1}all";
        x_keys.push(lit(0i32).alias(ALL));
        i_keys.push(lit(0i32).alias(ALL));
        left_on.push(col(ALL));
        right_on.push(col(ALL));
    }
    let pairs = i
        .clone()
        .select(i_keys)
        .join_builder()
        .with(x.clone().select(x_keys))
        .left_on(left_on)
        .right_on(right_on)
        .how(JoinType::Inner)
        .join_nulls(true)
        .suffix(CLASH_SUFFIX)
        .finish()
        .filter(holds)
        .select([col(I_ROW), col(X_ROW)]);
    let matched = i
        .join_builder()
        .with(pairs)
        .left_on([col(I_ROW)])
        .right_on([col(I_ROW)])
        .how(if join.drop_unmatched {
            JoinType::Inner
        } else {
            JoinType::Left
        })
        .finish()
        .sort(
            [I_ROW, X_ROW],
            SortMultipleOptions::default()
                .with_nulls_last(true)
                .with_maintain_order(true),
        );
    let joined = keep_mult(matched, join.mult)
        .join_builder()
        .with(x)
        .left_on([col(X_ROW)])
        .right_on([col(X_ROW)])
        .how(JoinType::Left)
        .maintain_order(MaintainOrderJoin::Left)
        .suffix(CLASH_SUFFIX)
        .finish();
    Ok(joined.select(layout_exprs(x_schema, i_schema, on, &[])))
}

/// `mult = "first"` or `"last"`: keeps one row of each run of equal [`I_ROW`]s.
fn keep_mult(joined: LazyFrame, mult: Mult) -> LazyFrame {
    let neighbour = match mult {
        Mult::All => return joined,
        Mult::First => 1,
        Mult::Last => -1,
    };
    joined.filter(
        col(I_ROW)
            .neq(col(I_ROW).shift(lit(neighbour)))
            .fill_null(lit(true)),
    )
}

/// The final selection of a join plan in which `i` is the left side, so a column of `x` whose
/// name `i` also has carries Polars' clash suffix. `restore` casts a join column back to `i`'s
/// type after it was matched in a common one.
fn layout_exprs(
    x_schema: &Schema,
    i_schema: &Schema,
    on: &[On],
    restore: &[(PlSmallStr, DataType)],
) -> Vec<PolarsExpr> {
    join_layout(&x_schema.names(), &i_schema.names(), on)
        .into_iter()
        .map(|column| {
            let value = match column.side {
                Side::X if i_schema.column(&column.source).is_some() => {
                    col(format!("{}{CLASH_SUFFIX}", column.source))
                }
                Side::X => col(column.source.clone()),
                Side::I => match restore
                    .iter()
                    .find(|(restored, _)| *restored == column.source)
                {
                    Some((_, dtype)) => col(column.source.clone()).cast(dtype.clone()),
                    None => col(column.source.clone()),
                },
            };
            value.alias(column.name)
        })
        .collect()
}

fn column_dtype(i: &LazyFrame, column: &str) -> Result<DataType> {
    let schema = i.clone().collect_schema()?;
    Ok(schema
        .get(column)
        .ok_or_else(|| Error::NoColumn(column.to_string()))?
        .clone())
}

/// The schema of a join's output, following [`join_layout`].
fn join_schema(x: &Schema, i: &Schema, on: &[On]) -> Result<Schema> {
    let columns = join_layout(&x.names(), &i.names(), on)
        .into_iter()
        .map(|column| {
            let side = match column.side {
                Side::X => x,
                Side::I => i,
            };
            let kind = side
                .column(&column.source)
                .ok_or_else(|| Error::NoColumn(column.source.to_string()))?;
            Ok((column.name, kind))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Schema { columns })
}

fn grouped(
    plan: LazyFrame,
    lower: &Lowering,
    by: &[(PlSmallStr, Expr)],
    keyed: bool,
    columns: &[(PlSmallStr, Expr)],
) -> Result<LazyFrame> {
    let mut groups = Vec::with_capacity(by.len());
    for (name, expr) in by {
        groups.push(lower.expr(expr)?.alias(name.clone()));
    }
    let mut aggregations = Vec::with_capacity(columns.len());
    let mut expands = Vec::new();
    for (name, expr) in columns {
        if !collapses(expr) {
            expands.push(name.clone());
        }
        aggregations.push(lower.expr(expr)?.alias(name.clone()));
    }
    // data.table keeps groups in order of first appearance for `by` and sorts them for `keyby`.
    let mut result = if keyed {
        let names: Vec<PlSmallStr> = by.iter().map(|(name, _)| name.clone()).collect();
        plan.group_by(groups)
            .agg(aggregations)
            .sort(names, SortMultipleOptions::default().with_nulls_last(false))
    } else {
        plan.group_by_stable(groups).agg(aggregations)
    };
    if !expands.is_empty() {
        result = result.explode(
            by_name(expands, true, false),
            ExplodeOptions {
                empty_as_null: false,
                keep_nulls: true,
            },
        );
    }
    Ok(result)
}

/// Whether an expression yields one value per group. One that does not (`head`, or a plain
/// column) yields a list per group, which is exploded into rows as data.table recycles it.
fn collapses(expr: &Expr) -> bool {
    match expr {
        Expr::Aggregate { .. } | Expr::Count | Expr::Correlation(..) => true,
        Expr::Integer(_)
        | Expr::Double(_)
        | Expr::Logical(_)
        | Expr::Character(_)
        | Expr::Na(_) => true,
        Expr::Column(_) | Expr::Head(..) => false,
        Expr::Unary(_, arg) => collapses(arg),
        Expr::Binary(_, left, right) => collapses(left) && collapses(right),
        Expr::IfElse(test, yes, no) => collapses(test) && collapses(yes) && collapses(no),
    }
}

/// Lowers `:=`. Each new column is computed by one projection of the plan, and then replaces or
/// joins the table's column list in place, so the columns not assigned are never touched. The
/// whole update is computed before the table changes, so an update that fails leaves it intact.
fn assign(
    table: &mut Table,
    schema: &Schema,
    rows: &Rows,
    by: &[(PlSmallStr, Expr)],
    assignments: &[(PlSmallStr, Option<Expr>)],
    kernels: bool,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    let typer = Typer { schema };
    // A new column whose value nests an integer `sum`, such as `sum(v) - 1L`, is a double when a
    // total overflows, because R's `sum` then returns a double. The value is computed as written
    // and again with each such sum taken over doubles, and probes holding the sums themselves, kept
    // in 64 bits, tell which of the two to keep once the totals are known.
    let mut alternatives = Vec::new();
    for (index, (name, expr)) in assignments.iter().enumerate() {
        let Some(expr) = expr else { continue };
        let plain_sum = matches!(
            expr,
            Expr::Aggregate {
                function: Aggregate::Sum,
                ..
            }
        );
        if schema.column(name).is_none() && !plain_sum && has_integer_sum(&typer, expr)? {
            let mut probes = Vec::new();
            integer_sums(&typer, expr, &mut probes)?;
            alternatives.push((index, double_sums(&typer, expr)?, probes));
        }
    }
    if kernels
        && by.is_empty()
        && alternatives.is_empty()
        && assign_with_kernels(table, schema, rows, assignments)?
    {
        return drop_removed(table, assignments);
    }
    let frame = table.frame().clone();
    let nulls = NullCounts::of(&frame);
    let lower = Lowering {
        schema,
        nulls: &nulls,
    };
    let filter = match rows {
        Rows::All => None,
        Rows::Filter(filter) => {
            expect_logical(typer.expr(filter)?, "a row filter")?;
            Some(lower.expr(filter)?)
        }
        _ => {
            return Err(Error::Query(
                "`:=` takes a logical row filter or no `i`".to_string(),
            ));
        }
    };
    let mut groups = Vec::with_capacity(by.len());
    for (name, expr) in by {
        groups.push(lower.expr(expr)?.alias(name.clone()));
    }
    let whole = filter.is_none() && groups.is_empty();
    let mut targets = Vec::with_capacity(assignments.len());
    // For each alternative, the index of the target it may replace.
    let mut replaced = Vec::with_capacity(alternatives.len());
    // A value written into a narrower column, with the index of its target: it is computed again
    // in its own type, and comparing the two finds a value the conversion changed.
    let mut conversions = Vec::new();
    for (index, (name, expr)) in assignments.iter().enumerate() {
        if alternatives
            .iter()
            .any(|(alternative, _, _)| *alternative == index)
        {
            replaced.push(targets.len());
        }
        let existing = schema.column(name);
        let Some(expr) = expr else {
            if existing.is_none() {
                return Err(Error::Query(format!(
                    "cannot remove `{name}`: the table has no such column"
                )));
            }
            continue;
        };
        let replaces = whole && !collapses(expr);
        let natural = typer.expr(expr)?;
        let kind = assigned_type(existing, natural, replaces)?;
        if existing.is_some() && narrows(natural, kind) {
            conversions.push((targets.len(), expr, natural));
        }
        let levels = match existing {
            Some(ColumnType::Factor) if !replaces => factor_levels(frame.column(name)?.dtype()),
            _ => None,
        };
        // A new column holding an integer sum is a double when a total overflows, as data.table's
        // gsum makes it, so the sum stays 64-bit until the result is narrowed below. Written into
        // an existing column, the value is coerced to that column's type instead.
        let widens = existing.is_none()
            && matches!(
                expr,
                Expr::Aggregate {
                    function: Aggregate::Sum,
                    arg,
                    ..
                } if typer.expr(arg)? != ColumnType::Double
            );
        targets.push(Target {
            name: name.clone(),
            expr,
            kind,
            levels,
            widens,
        });
    }
    let primaries = targets.len();
    let mut hidden = 0;
    let mut hidden_name = || {
        hidden += 1;
        PlSmallStr::from(format!("\u{1}assign{hidden}"))
    };
    for (_, doubled, probes) in &alternatives {
        targets.push(Target {
            name: hidden_name(),
            expr: doubled,
            kind: typer.expr(doubled)?,
            levels: None,
            widens: false,
        });
        for probe in probes {
            targets.push(Target {
                name: hidden_name(),
                expr: probe,
                kind: ColumnType::Integer,
                levels: None,
                widens: true,
            });
        }
    }
    for (_, expr, natural) in &conversions {
        targets.push(Target {
            name: hidden_name(),
            expr,
            kind: *natural,
            levels: None,
            widens: false,
        });
    }
    let result = match &filter {
        _ if targets.is_empty() => None,
        // An aggregate, or a group, must see only the selected rows, which an expression over the
        // whole frame would not, so such an update computes on the filtered rows and scatters the
        // results back. A row-wise update keeps the cheaper `when(filter)` form below.
        Some(filter)
            if !groups.is_empty() || targets.iter().any(|target| !row_wise(target.expr)) =>
        {
            Some(assign_filtered(
                &frame,
                &lower,
                filter.clone(),
                &groups,
                &targets,
            )?)
        }
        _ => {
            let mut computed = Vec::with_capacity(targets.len());
            for target in &targets {
                let mut value = lower.expr(target.expr)?;
                if !groups.is_empty() {
                    value = value.over(groups.clone())?;
                }
                value = target.store(value);
                if let Some(filter) = &filter {
                    let otherwise = if schema.column(&target.name).is_some() {
                        target.store(col(target.name.clone()))
                    } else {
                        target.store(lit(NULL))
                    };
                    value = when(filter.clone()).then(value).otherwise(otherwise);
                }
                computed.push(value.alias(target.name.clone()));
            }
            // `with_columns` broadcasts a value that collapses to one row, where `select` would not.
            let names: Vec<PolarsExpr> = targets
                .iter()
                .map(|target| col(target.name.clone()))
                .collect();
            Some(
                frame
                    .lazy()
                    .with_columns(computed)
                    .select(names)
                    .collect()?,
            )
        }
    };
    let mut columns = Vec::with_capacity(primaries);
    if let Some(result) = result {
        let result = narrow_sums(result)?;
        let mut computed = result.into_columns();
        let mut extras = computed.split_off(primaries).into_iter();
        for ((_, _, probes), target) in alternatives.iter().zip(replaced) {
            let doubled = extras.next();
            let overflowed = extras
                .by_ref()
                .take(probes.len())
                .any(|probe| probe.dtype() == &DataType::Float64);
            if let (true, Some(doubled), Some(slot)) =
                (overflowed, doubled, computed.get_mut(target))
            {
                let name = slot.name().clone();
                *slot = doubled.with_name(name);
            }
        }
        for ((target, _, _), natural) in conversions.iter().zip(extras) {
            if let Some(stored) = computed.get(*target)
                && let Some(warning) = conversion_warning(stored, &natural, targets[*target].kind)?
            {
                warnings.push(warning);
            }
        }
        for (column, target) in computed.into_iter().zip(&targets) {
            columns.push(match &target.levels {
                Some(levels) => relevel(column, levels)?,
                None => column,
            });
        }
    }
    for column in columns {
        table.set_column(column)?;
    }
    drop_removed(table, assignments)
}

fn drop_removed(table: &mut Table, assignments: &[(PlSmallStr, Option<Expr>)]) -> Result<()> {
    for (name, expr) in assignments {
        if expr.is_none() {
            table.drop_column(name)?;
        }
    }
    Ok(())
}

/// A row-wise `:=` into logical, integer or double columns, evaluated by the kernels instead of
/// a Polars plan. A filtered update writes the selected rows into the column's buffer in place,
/// so a loop of small updates costs what the rows it touches cost, not the whole column. Gives
/// `false`, having changed nothing, when an expression or a target is outside what the kernels
/// cover, and the caller then runs the Polars plan.
fn assign_with_kernels(
    table: &mut Table,
    schema: &Schema,
    rows: &Rows,
    assignments: &[(PlSmallStr, Option<Expr>)],
) -> Result<bool> {
    let typer = Typer { schema };
    let height = table.height();
    // Every value is computed before any column changes, because each one reads the table as it
    // was before the update.
    let mut values = Vec::with_capacity(assignments.len());
    let selected = {
        let frame = table.frame();
        let selected = match rows {
            Rows::All => None,
            Rows::Filter(filter) => {
                expect_logical(typer.expr(filter)?, "a row filter")?;
                let evaluator = Evaluator {
                    frame,
                    schema,
                    rows: None,
                };
                let Some(selected) = evaluator
                    .eval(filter)
                    .and_then(|value| selected_rows(value, height))
                else {
                    return Ok(false);
                };
                Some(selected)
            }
            _ => return Ok(false),
        };
        let evaluator = Evaluator {
            frame,
            schema,
            rows: selected.as_deref(),
        };
        for (name, expr) in assignments {
            let existing = schema.column(name);
            let Some(expr) = expr else {
                if existing.is_none() {
                    return Err(Error::Query(format!(
                        "cannot remove `{name}`: the table has no such column"
                    )));
                }
                continue;
            };
            if !row_wise(expr)
                || matches!(existing, Some(ColumnType::Factor | ColumnType::Character))
            {
                return Ok(false);
            }
            let replaces = selected.is_none() && !collapses(expr);
            let natural = typer.expr(expr)?;
            let kind = assigned_type(existing, natural, replaces)?;
            // A narrowing write may warn, which the plan's update checks for.
            if existing.is_some() && narrows(natural, kind) {
                return Ok(false);
            }
            let Some(value) = evaluator.eval(expr).and_then(|value| value.cast(kind)) else {
                return Ok(false);
            };
            values.push((name, kind, existing.is_some(), value));
        }
        selected
    };
    for (name, kind, exists, value) in values {
        let column = match &selected {
            None => value
                .into_vector(height)
                .ok_or_else(|| Error::Query(format!("cannot write into column `{name}`")))?
                .into_column(name.clone()),
            Some(selected) => {
                let column = if exists {
                    table.take_column(name)?
                } else {
                    // A new column is `NA` outside the selected rows.
                    Vector::null(kind, height)
                        .ok_or_else(|| Error::Query(format!("cannot write into column `{name}`")))?
                        .into_column(name.clone())
                };
                scatter(column, selected, value)?
            }
        };
        table.put_column(column)?;
    }
    Ok(true)
}

/// One column that `:=` writes.
struct Target<'a> {
    name: PlSmallStr,
    expr: &'a Expr,
    kind: ColumnType,
    /// The levels of an existing factor column that the value is written into by label. A label
    /// the factor lacks becomes a new level, as data.table adds it.
    levels: Option<Vec<String>>,
    /// The value is an integer sum kept in 64 bits, narrowed once its totals are known.
    widens: bool,
}

impl Target<'_> {
    /// The value as the column stores it. A factor written into an existing factor goes through
    /// its labels, and a factor that becomes the whole column keeps its own levels.
    fn store(&self, value: PolarsExpr) -> PolarsExpr {
        match (&self.levels, self.kind) {
            _ if self.widens => value.cast(DataType::Int64),
            (Some(_), _) => value.cast(DataType::String),
            (None, ColumnType::Factor) => value,
            // Polars casts NaN to `true`, where R's `as.logical(NaN)` is `NA`.
            (None, ColumnType::Logical) => when(value.clone().cast(DataType::Float64).is_nan())
                .then(lit(NULL).cast(DataType::Boolean))
                .otherwise(value.cast(DataType::Boolean)),
            (None, kind) => value.cast(dtype(kind)),
        }
    }
}

/// Whether writing a `natural` value into a `kind` column can change it: a double into an integer
/// column, or a number into a logical one.
fn narrows(natural: ColumnType, kind: ColumnType) -> bool {
    matches!(
        (natural, kind),
        (ColumnType::Double, ColumnType::Integer)
            | (
                ColumnType::Integer | ColumnType::Double,
                ColumnType::Logical
            )
    )
}

/// The warning for the first value a narrowing write changed, comparing the value computed in its
/// own type, `natural`, with what the column stores.
fn conversion_warning(
    stored: &Column,
    natural: &Column,
    to: ColumnType,
) -> Result<Option<Warning>> {
    let (natural_values, stored_values) = (
        natural.cast(&DataType::Float64)?,
        stored.cast(&DataType::Float64)?,
    );
    let changed = natural_values
        .as_materialized_series()
        .f64()?
        .iter()
        .zip(stored_values.as_materialized_series().f64()?.iter())
        .position(|pair| match pair {
            (Some(value), Some(kept)) => value != kept,
            // NaN has no integer or logical form, so it becomes NA without a warning, as in R.
            (Some(value), None) => !value.is_nan(),
            (None, _) => false,
        });
    let Some(row) = changed else {
        return Ok(None);
    };
    let text = |value: AnyValue| match value {
        AnyValue::Null => "NA".to_string(),
        AnyValue::Boolean(value) => if value { "TRUE" } else { "FALSE" }.to_string(),
        AnyValue::Float64(value) => r_double(value),
        value => value.to_string(),
    };
    Ok(Some(Warning::Converted {
        column: stored.name().clone(),
        to,
        value: text(natural.get(row)?),
        stored: text(stored.get(row)?),
    }))
}

/// Formats a double as R prints it: the shorter of fixed and scientific notation, with R's
/// `1e+10` exponent form.
fn r_double(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Inf" } else { "-Inf" }.to_string();
    }
    let fixed = value.to_string();
    let scientific = format!("{value:e}");
    let scientific = match scientific.split_once('e') {
        Some((mantissa, exponent)) => match exponent.strip_prefix('-') {
            Some(digits) => format!("{mantissa}e-{digits:0>2}"),
            None => format!("{mantissa}e+{exponent:0>2}"),
        },
        None => scientific,
    };
    if scientific.len() < fixed.len() {
        scientific
    } else {
        fixed
    }
}

/// The type of a column after `:=` writes a value of type `kind` into it. A value written into an
/// existing column takes that column's type, as data.table coerces it, unless the value replaces
/// the whole column (no `i`, no `by`, one value per row), which replaces its type as well.
fn assigned_type(
    existing: Option<ColumnType>,
    kind: ColumnType,
    replaces: bool,
) -> Result<ColumnType> {
    match existing {
        None => Ok(kind),
        Some(_) if replaces => Ok(kind),
        Some(ColumnType::Factor) => match kind {
            ColumnType::Character | ColumnType::Factor => Ok(ColumnType::Factor),
            other => Err(Error::Query(format!(
                "writing {other} values into a factor column is not supported"
            ))),
        },
        Some(existing) => Ok(existing),
    }
}

/// A string column as a factor whose levels are `levels` followed by the labels they lack, in
/// order of first appearance.
fn relevel(column: Column, levels: &[String]) -> Result<Column> {
    let mut levels = levels.to_vec();
    let mut known: HashSet<String> = levels.iter().cloned().collect();
    for label in column.as_materialized_series().str()?.iter().flatten() {
        if !known.contains(label) {
            known.insert(label.to_string());
            levels.push(label.to_string());
        }
    }
    Ok(column.cast(&enum_dtype(&levels)?)?)
}

/// Whether an expression computes each row from that row alone, so that evaluating it over the
/// whole frame and keeping the selected rows gives the same values as evaluating it on them.
fn row_wise(expr: &Expr) -> bool {
    match expr {
        Expr::Aggregate { .. } | Expr::Count | Expr::Head(..) | Expr::Correlation(..) => false,
        Expr::Column(_)
        | Expr::Integer(_)
        | Expr::Double(_)
        | Expr::Logical(_)
        | Expr::Character(_)
        | Expr::Na(_) => true,
        Expr::Unary(_, arg) => row_wise(arg),
        Expr::Binary(_, left, right) => row_wise(left) && row_wise(right),
        Expr::IfElse(test, yes, no) => row_wise(test) && row_wise(yes) && row_wise(no),
    }
}

/// The assigned columns of a filtered update that must be computed on the selected rows alone,
/// in the order of `targets`.
fn assign_filtered(
    frame: &DataFrame,
    lower: &Lowering,
    filter: PolarsExpr,
    groups: &[PolarsExpr],
    targets: &[Target],
) -> Result<DataFrame> {
    const ROW: &str = "\u{1}row";
    // Marks the rows the filter selected, so that an `NA` computed for one of them still replaces
    // the old value, while unselected rows keep theirs.
    const ROW_MATCHED: &str = "\u{1}matched";
    let mut values = vec![col(ROW), lit(true).alias(ROW_MATCHED)];
    for target in targets {
        let mut value = lower.expr(target.expr)?;
        if !groups.is_empty() {
            value = value.over(groups)?;
        }
        values.push(target.store(value).alias(target.name.clone()));
    }
    let selected = frame
        .clone()
        .lazy()
        .with_row_index(ROW, None)
        .filter(filter)
        .select(values);
    let mut keep = Vec::with_capacity(targets.len());
    for target in targets {
        let name = &target.name;
        let value = if lower.schema.column(name).is_some() {
            let computed = col(format!("{name}{CLASH_SUFFIX}"));
            when(col(ROW_MATCHED).is_not_null())
                .then(computed)
                .otherwise(target.store(col(name.clone())))
        } else {
            col(name.clone())
        };
        keep.push(value.alias(name.clone()));
    }
    Ok(frame
        .clone()
        .lazy()
        .with_row_index(ROW, None)
        .join_builder()
        .with(selected)
        .left_on([col(ROW)])
        .right_on([col(ROW)])
        .how(JoinType::Left)
        .maintain_order(MaintainOrderJoin::Left)
        .suffix(CLASH_SUFFIX)
        .finish()
        .select(keep)
        .collect()?)
}

/// The null count of each input column, read once per query from the chunks' metadata. A column
/// without nulls lets an aggregate skip the guard that makes `NA` propagate.
#[derive(Default)]
struct NullCounts(Vec<(PlSmallStr, usize)>);

impl NullCounts {
    fn of(frame: &DataFrame) -> NullCounts {
        NullCounts(
            frame
                .columns()
                .iter()
                .map(|column| (column.name().clone(), column.null_count()))
                .collect(),
        )
    }

    /// Whether `expr` may produce a null. Unknown columns, such as a join's output, may.
    fn may_be_null(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Column(name) => self
                .0
                .iter()
                .find(|(column, _)| column == name)
                .is_none_or(|(_, count)| *count > 0),
            Expr::Na(_) => true,
            Expr::Integer(_)
            | Expr::Double(_)
            | Expr::Logical(_)
            | Expr::Character(_)
            | Expr::Count => false,
            Expr::Unary(UnaryOp::IsNa, _) => false,
            Expr::Unary(_, arg) | Expr::Head(arg, _) => self.may_be_null(arg),
            // Integer arithmetic overflows to NA, so it may always produce one.
            Expr::Binary(..)
            | Expr::Aggregate { .. }
            | Expr::Correlation(..)
            | Expr::IfElse(..) => true,
        }
    }
}

pub(crate) struct Typer<'a> {
    pub(crate) schema: &'a Schema,
}

impl Typer<'_> {
    /// The R type of an expression's elements, checking every column reference.
    pub(crate) fn expr(&self, expr: &Expr) -> Result<ColumnType> {
        Ok(match expr {
            Expr::Column(name) => self
                .schema
                .column(name)
                .ok_or_else(|| Error::NoColumn(name.to_string()))?,
            Expr::Integer(_) | Expr::Count => ColumnType::Integer,
            Expr::Double(_) => ColumnType::Double,
            Expr::Logical(_) => ColumnType::Logical,
            Expr::Character(_) => ColumnType::Character,
            Expr::Na(kind) => *kind,
            Expr::Unary(UnaryOp::Not, arg) => {
                expect_truthy(self.expr(arg)?, "`!`")?;
                ColumnType::Logical
            }
            Expr::Unary(UnaryOp::IsNa, arg) => {
                self.expr(arg)?;
                ColumnType::Logical
            }
            Expr::Unary(UnaryOp::Negate, arg) => arithmetic(self.expr(arg)?, ColumnType::Integer)?,
            Expr::Binary(op, left, right) => {
                let (left, right) = (self.expr(left)?, self.expr(right)?);
                match op {
                    BinaryOp::Add
                    | BinaryOp::Subtract
                    | BinaryOp::Multiply
                    | BinaryOp::IntegerDivide
                    | BinaryOp::Modulo => arithmetic(left, right)?,
                    BinaryOp::Divide | BinaryOp::Power => {
                        arithmetic(left, right)?;
                        ColumnType::Double
                    }
                    BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual => {
                        comparison(left, right)?;
                        ColumnType::Logical
                    }
                    BinaryOp::And | BinaryOp::Or => {
                        expect_truthy(left, "`&` and `|`")?;
                        expect_truthy(right, "`&` and `|`")?;
                        ColumnType::Logical
                    }
                }
            }
            Expr::Aggregate { function, arg, .. } => {
                let kind = self.expr(arg)?;
                match function {
                    Aggregate::Sum => arithmetic(kind, ColumnType::Integer)?,
                    Aggregate::Mean | Aggregate::Median | Aggregate::Sd | Aggregate::Var => {
                        arithmetic(kind, ColumnType::Integer)?;
                        ColumnType::Double
                    }
                    // R's `min` and `max` of logicals are integers.
                    Aggregate::Min | Aggregate::Max if kind == ColumnType::Logical => {
                        ColumnType::Integer
                    }
                    Aggregate::Min | Aggregate::Max | Aggregate::First | Aggregate::Last => kind,
                    Aggregate::Length => ColumnType::Integer,
                }
            }
            Expr::Head(arg, _) => self.expr(arg)?,
            Expr::Correlation(left, right) => {
                arithmetic(self.expr(left)?, self.expr(right)?)?;
                ColumnType::Double
            }
            Expr::IfElse(test, yes, no) => {
                expect_logical(self.expr(test)?, "an `ifelse` test")?;
                supertype(self.expr(yes)?, self.expr(no)?)
            }
        })
    }
}

/// The result type of R arithmetic: logical promotes to integer, and anything with a double is
/// double. Characters and factors are refused, as R refuses them.
fn arithmetic(left: ColumnType, right: ColumnType) -> Result<ColumnType> {
    match (left, right) {
        (ColumnType::Character | ColumnType::Factor, _)
        | (_, ColumnType::Character | ColumnType::Factor) => Err(Error::Query(
            "arithmetic needs numeric or logical columns".to_string(),
        )),
        (ColumnType::Double, _) | (_, ColumnType::Double) => Ok(ColumnType::Double),
        _ => Ok(ColumnType::Integer),
    }
}

/// The type both of two column types coerce to, in R's order logical < integer < double <
/// character. A factor joined with anything else becomes character.
fn supertype(left: ColumnType, right: ColumnType) -> ColumnType {
    fn rank(kind: ColumnType) -> u8 {
        match kind {
            ColumnType::Logical => 0,
            ColumnType::Integer => 1,
            ColumnType::Double => 2,
            ColumnType::Character | ColumnType::Factor => 3,
        }
    }
    if left == right {
        return left;
    }
    match rank(left).max(rank(right)) {
        0 => ColumnType::Logical,
        1 => ColumnType::Integer,
        2 => ColumnType::Double,
        _ => ColumnType::Character,
    }
}

/// The operands `!`, `&` and `|` accept: logical, or numeric with zero as `FALSE`, as in R.
fn expect_truthy(kind: ColumnType, what: &str) -> Result<()> {
    match kind {
        ColumnType::Logical | ColumnType::Integer | ColumnType::Double => Ok(()),
        other => Err(Error::Query(format!(
            "{what} needs logical or numeric operands, not {other}"
        ))),
    }
}

/// The type a comparison converts both sides to. R compares a number with a string by
/// formatting the number; ry formats logicals and integers as R does, but a double's R
/// formatting (`1e+06`, fifteen significant digits) has no Polars equivalent, so that mix is
/// refused rather than compared wrongly.
pub(crate) fn comparison(left: ColumnType, right: ColumnType) -> Result<ColumnType> {
    let textual = |kind| matches!(kind, ColumnType::Character | ColumnType::Factor);
    if (textual(left) && right == ColumnType::Double)
        || (left == ColumnType::Double && textual(right))
    {
        return Err(Error::Query(
            "comparing a double with a string is not supported".to_string(),
        ));
    }
    Ok(match supertype(left, right) {
        ColumnType::Factor => ColumnType::Character,
        other => other,
    })
}

fn expect_logical(kind: ColumnType, what: &str) -> Result<()> {
    match kind {
        ColumnType::Logical => Ok(()),
        other => Err(Error::Query(format!("{what} must be logical, not {other}"))),
    }
}

fn dtype(kind: ColumnType) -> DataType {
    match kind {
        ColumnType::Logical => DataType::Boolean,
        ColumnType::Integer => DataType::Int32,
        ColumnType::Double => DataType::Float64,
        // A factor result of `ifelse` and friends is character, as `supertype` decides, so this
        // arm only casts a factor column onto itself.
        ColumnType::Character | ColumnType::Factor => DataType::String,
    }
}

struct Lowering<'a> {
    pub(crate) schema: &'a Schema,
    nulls: &'a NullCounts,
}

impl Lowering<'_> {
    fn typer(&self) -> Typer<'_> {
        Typer {
            schema: self.schema,
        }
    }

    fn expr(&self, expr: &Expr) -> Result<PolarsExpr> {
        let typer = self.typer();
        Ok(match expr {
            Expr::Column(name) => {
                typer.expr(expr)?;
                col(name.clone())
            }
            Expr::Integer(value) => lit(*value),
            Expr::Double(value) => lit(*value),
            Expr::Logical(value) => lit(*value),
            Expr::Character(value) => lit(value.clone()),
            Expr::Na(kind) => lit(polars::prelude::Null {}).cast(dtype(*kind)),
            Expr::Count => len().cast(DataType::Int32),
            Expr::Unary(UnaryOp::Not, arg) => self.logical(arg)?.not(),
            Expr::Unary(UnaryOp::IsNa, arg) => {
                let lowered = self.expr(arg)?;
                // R's `is.na` is also true for `NaN`.
                if typer.expr(arg)? == ColumnType::Double {
                    lowered.clone().is_null().or(lowered.is_nan())
                } else {
                    lowered.is_null()
                }
            }
            Expr::Unary(UnaryOp::Negate, arg) => {
                let kind = typer.expr(expr)?;
                lit(0).cast(dtype(kind)) - self.numeric(arg, kind)?
            }
            Expr::Binary(op, left, right) => self.binary(*op, left, right, typer.expr(expr)?)?,
            Expr::Aggregate {
                function,
                arg,
                na_rm,
            } => {
                let kind = typer.expr(arg)?;
                let mut value = self.expr(arg)?;
                if kind == ColumnType::Logical
                    && matches!(function, Aggregate::Min | Aggregate::Max)
                {
                    value = value.cast(DataType::Int32);
                }
                // `na.rm = TRUE` drops `NaN` as well as `NA`. Polars' reductions skip nulls on
                // their own, but `first` and `last` do not.
                if *na_rm && kind == ColumnType::Double {
                    value = value.clone().filter(value.is_not_nan());
                } else if *na_rm && matches!(function, Aggregate::First | Aggregate::Last) {
                    value = value.drop_nulls();
                }
                let aggregated = match function {
                    // R's integer sum is exact: an integer when it fits and a double when it does
                    // not. It is summed in 64 bits here and narrowed once the result is known
                    // (see `narrow_sums`).
                    Aggregate::Sum if kind != ColumnType::Double => {
                        value.cast(DataType::Int64).sum()
                    }
                    Aggregate::Sum => value.sum(),
                    // With `na.rm = TRUE` a group with no values left has the mean `0 / 0`, which
                    // is `NaN` in R, where Polars gives null.
                    Aggregate::Mean if *na_rm => {
                        let value = value.cast(DataType::Float64);
                        value.clone().sum() / value.count().cast(DataType::Float64)
                    }
                    Aggregate::Mean => value.cast(DataType::Float64).mean(),
                    // R's `min` and `max` are `NaN` when an input is, where Polars skips it.
                    Aggregate::Min if kind == ColumnType::Double => value.nan_min(),
                    Aggregate::Max if kind == ColumnType::Double => value.nan_max(),
                    Aggregate::Min => value.min(),
                    Aggregate::Max => value.max(),
                    Aggregate::First => value.first(),
                    Aggregate::Last => value.last(),
                    Aggregate::Median => value.cast(DataType::Float64).median(),
                    Aggregate::Sd => value.cast(DataType::Float64).std(1),
                    Aggregate::Var => value.cast(DataType::Float64).var(1),
                    Aggregate::Length => len().cast(DataType::Int32),
                };
                let propagates = !na_rm
                    && !matches!(
                        function,
                        Aggregate::First | Aggregate::Last | Aggregate::Length
                    );
                if propagates && self.nulls.may_be_null(arg) {
                    when(self.expr(arg)?.null_count().gt(lit(0)))
                        .then(lit(polars::prelude::Null {}).cast(dtype(typer.expr(expr)?)))
                        .otherwise(aggregated)
                } else {
                    aggregated
                }
            }
            Expr::Head(arg, count) => self.expr(arg)?.head(Some(*count)),
            Expr::Correlation(left, right) => {
                // R's `cor` is `NA` where a standard deviation is zero or an input is `NaN`, where
                // Polars computes `NaN`.
                let (l, r) = (
                    self.numeric(left, ColumnType::Double)?,
                    self.numeric(right, ColumnType::Double)?,
                );
                let correlation = pearson_corr(l.clone(), r.clone());
                let mut undefined = correlation.clone().is_nan();
                // Polars skips `NA` pairs, where R's `cor` is `NA` as soon as one input is.
                if self.nulls.may_be_null(left) || self.nulls.may_be_null(right) {
                    undefined = undefined.or((l.null_count() + r.null_count()).gt(lit(0)));
                }
                when(undefined)
                    .then(lit(NULL).cast(DataType::Float64))
                    .otherwise(correlation)
            }
            Expr::IfElse(test, yes, no) => {
                let kind = typer.expr(expr)?;
                // Two factor branches stay factors; Polars refuses them if their levels differ,
                // as `fifelse` does.
                let branch = |value: PolarsExpr| match kind {
                    ColumnType::Factor => value,
                    kind => value.cast(dtype(kind)),
                };
                let (yes, no) = (branch(self.expr(yes)?), branch(self.expr(no)?));
                let test = self.expr(test)?;
                // An `NA` test gives `NA`, where Polars would take the `no` branch.
                when(test.clone().is_null())
                    .then(branch(lit(NULL)))
                    .when(test)
                    .then(yes)
                    .otherwise(no)
            }
        })
    }

    fn numeric(&self, expr: &Expr, kind: ColumnType) -> Result<PolarsExpr> {
        Ok(self.expr(expr)?.cast(dtype(kind)))
    }

    /// An operand of `!`, `&` or `|` as a logical: a number is `TRUE` unless it is zero, and
    /// `NaN` is `NA`, as R converts it.
    fn logical(&self, expr: &Expr) -> Result<PolarsExpr> {
        let value = self.expr(expr)?;
        Ok(match self.typer().expr(expr)? {
            ColumnType::Logical => value,
            ColumnType::Double => when(value.clone().is_nan())
                .then(lit(NULL).cast(DataType::Boolean))
                .otherwise(value.neq(lit(0.0))),
            _ => value.neq(lit(0)),
        })
    }

    /// An operand of a comparison converted to `kind`, formatting a logical as `"TRUE"` or
    /// `"FALSE"` when it meets a string, as R's `as.character` does.
    fn compared(&self, expr: &Expr, kind: ColumnType) -> Result<PolarsExpr> {
        let value = self.expr(expr)?;
        Ok(match (self.typer().expr(expr)?, kind) {
            (ColumnType::Logical, ColumnType::Character) => {
                when(value.clone()).then(lit("TRUE")).otherwise(
                    when(value.not())
                        .then(lit("FALSE"))
                        .otherwise(lit(NULL).cast(DataType::String)),
                )
            }
            _ => value.cast(dtype(kind)),
        })
    }

    fn binary(
        &self,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
        kind: ColumnType,
    ) -> Result<PolarsExpr> {
        let typer = self.typer();
        let (left_kind, right_kind) = (typer.expr(left)?, typer.expr(right)?);
        Ok(match op {
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply
                if kind == ColumnType::Integer =>
            {
                // R's integer arithmetic gives `NA` on overflow where Polars would wrap, so it is
                // computed in 64 bits and checked.
                let (l, r) = (
                    self.numeric(left, ColumnType::Integer)?
                        .cast(DataType::Int64),
                    self.numeric(right, ColumnType::Integer)?
                        .cast(DataType::Int64),
                );
                checked_int32(match op {
                    BinaryOp::Add => l + r,
                    BinaryOp::Subtract => l - r,
                    _ => l * r,
                })
            }
            BinaryOp::Add => self.numeric(left, kind)? + self.numeric(right, kind)?,
            BinaryOp::Subtract => self.numeric(left, kind)? - self.numeric(right, kind)?,
            BinaryOp::Multiply => self.numeric(left, kind)? * self.numeric(right, kind)?,
            BinaryOp::Divide => {
                self.numeric(left, ColumnType::Double)? / self.numeric(right, ColumnType::Double)?
            }
            BinaryOp::Power => {
                let (l, r) = (
                    self.numeric(left, ColumnType::Double)?,
                    self.numeric(right, ColumnType::Double)?,
                );
                // `1^y` and `x^0` are 1 in R even when the other side is `NA`.
                when(l.clone().eq(lit(1.0)).or(r.clone().eq(lit(0.0))))
                    .then(lit(1.0))
                    .otherwise(l.pow(r))
            }
            BinaryOp::IntegerDivide if kind == ColumnType::Integer => {
                let (l, r) = (self.numeric(left, kind)?, self.numeric(right, kind)?);
                // R gives `NA` for an integer division by zero.
                when(r.clone().eq(lit(0)))
                    .then(lit(polars::prelude::Null {}).cast(DataType::Int32))
                    .otherwise(l.floor_div(r))
            }
            BinaryOp::IntegerDivide => self
                .numeric(left, kind)?
                .floor_div(self.numeric(right, kind)?),
            BinaryOp::Modulo => {
                // R's `%%` takes the sign of the divisor: x - floor(x / y) * y.
                let (l, r) = (
                    self.numeric(left, ColumnType::Double)?,
                    self.numeric(right, ColumnType::Double)?,
                );
                let value = l.clone() - l.floor_div(r.clone()) * r;
                value.cast(dtype(kind))
            }
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => {
                // A factor compares by its labels, as R compares it.
                let target = comparison(left_kind, right_kind)?;
                let (l, r) = (self.compared(left, target)?, self.compared(right, target)?);
                let compared = match op {
                    BinaryOp::Equal => l.clone().eq(r.clone()),
                    BinaryOp::NotEqual => l.clone().neq(r.clone()),
                    BinaryOp::Less => l.clone().lt(r.clone()),
                    BinaryOp::LessEqual => l.clone().lt_eq(r.clone()),
                    BinaryOp::Greater => l.clone().gt(r.clone()),
                    _ => l.clone().gt_eq(r.clone()),
                };
                // Polars orders `NaN` above every number and equal to itself, where any R
                // comparison with `NaN` is `NA`.
                if target == ColumnType::Double {
                    when(l.is_nan().or(r.is_nan()))
                        .then(lit(NULL).cast(DataType::Boolean))
                        .otherwise(compared)
                } else {
                    compared
                }
            }
            BinaryOp::And => self.logical(left)?.and(self.logical(right)?),
            BinaryOp::Or => self.logical(left)?.or(self.logical(right)?),
        })
    }
}

/// An `Int64` result narrowed to `Int32`, with `NA` where it does not fit, as R's integer
/// arithmetic overflows.
fn checked_int32(value: PolarsExpr) -> PolarsExpr {
    let fits = value
        .clone()
        .gt_eq(lit(-(i32::MAX as i64)))
        .and(value.clone().lt_eq(lit(i32::MAX as i64)));
    when(fits)
        .then(value.cast(DataType::Int32))
        .otherwise(lit(polars::prelude::Null {}).cast(DataType::Int32))
}
