//! Polars-style queries: a [`Plan`] is a source and a chain of verbs (`filter`, `select`,
//! `with_columns`, `group_by` and `agg`, `sort`, `head`, `join`), the shape R code builds with
//! `DataFrame.filter(df, col("x") > 1)` and its siblings.
//!
//! An eager verb is a one-step plan run at once. A lazy plan runs only when it is collected, and
//! first goes through [`optimize`], which rewrites the chain without changing its result:
//!
//! - adjacent filters merge into one conjunction;
//! - a filter moves below a sort, below a row-wise `with_columns` it does not read, below a
//!   selection of plain columns, and into the left side of a join it only reads from;
//! - a `head` right after the source is read from the file as its first rows;
//! - a file source reads only the columns some later step mentions.
//!
//! [`collect`] then fuses each run of steps that one [`Query`] can express (rows, then a
//! selection or a grouping) and executes it through [`query::execute`], so every verb runs on the
//! same kernels as `tbl[i, j, by]`.

use {
    crate::{
        Error, Result, Schema, Table,
        io::{self, ReadOptions},
        query::{
            self, Aggregate, BinaryOp, Expr, Join, On, Output, Query, ResultType, Rows, Select,
            SortKey, UnaryOp, Warning, collapses, join_layout,
        },
    },
    polars::prelude::{
        IntoLazy, JoinArgs, JoinCoalesce, JoinType, ParquetReader, PlSmallStr, SerReader, col,
    },
    rustc_hash::FxHashSet,
    std::{
        fmt,
        path::{Path, PathBuf},
    },
};

/// A query: where the rows come from and the verbs applied to them, in order.
#[derive(Clone, Debug)]
pub struct Plan {
    pub source: Source,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug)]
pub enum Source {
    Table(Table),
    Csv { path: PathBuf, options: ReadOptions },
    Parquet { path: PathBuf, options: ReadOptions },
}

/// One verb. Every named expression's name is its output column.
#[derive(Clone, Debug)]
pub enum Step {
    /// Keeps the rows where the predicate is `TRUE`; `NA` drops the row.
    Filter(Expr),
    /// Replaces the columns with these.
    Select(Vec<(PlSmallStr, Expr)>),
    /// Replaces the columns these name, in place, and appends the others.
    WithColumns(Vec<(PlSmallStr, Expr)>),
    /// One row per distinct combination of the keys, in order of first appearance, with the keys
    /// first and then one value per aggregate.
    Aggregate {
        keys: Vec<(PlSmallStr, Expr)>,
        aggregates: Vec<(PlSmallStr, Expr)>,
    },
    /// A stable sort that puts `NA` last.
    Sort(Vec<SortKey>),
    Head(usize),
    Tail(usize),
    Join {
        other: Box<Plan>,
        left_on: Vec<PlSmallStr>,
        right_on: Vec<PlSmallStr>,
        how: How,
        suffix: PlSmallStr,
    },
}

/// Which rows a join keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// The left rows that match, once per match.
    Inner,
    /// Every left row, once per match, with `NA` in the right columns when nothing matches.
    Left,
    /// Every row of both sides, the keys merged into one column.
    Full,
    /// The left rows that match something, once each, with only the left columns.
    Semi,
    /// The left rows that match nothing, with only the left columns.
    Anti,
}

impl How {
    pub fn parse(text: &str) -> Option<How> {
        Some(match text {
            "inner" => How::Inner,
            "left" => How::Left,
            "full" => How::Full,
            "semi" => How::Semi,
            "anti" => How::Anti,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            How::Inner => "inner",
            How::Left => "left",
            How::Full => "full",
            How::Semi => "semi",
            How::Anti => "anti",
        }
    }
}

/// A plan's result, and the warnings the engine raised while computing it.
pub struct Collected {
    pub table: Table,
    pub warnings: Vec<Warning>,
}

impl Plan {
    pub fn new(source: Source) -> Plan {
        Plan {
            source,
            steps: Vec::new(),
        }
    }

    pub fn scan_csv(path: impl Into<PathBuf>) -> Plan {
        Plan::new(Source::Csv {
            path: path.into(),
            options: ReadOptions::default(),
        })
    }

    pub fn scan_parquet(path: impl Into<PathBuf>) -> Plan {
        Plan::new(Source::Parquet {
            path: path.into(),
            options: ReadOptions::default(),
        })
    }

    pub fn with(mut self, step: Step) -> Plan {
        self.steps.push(step);
        self
    }

    /// The columns the plan produces, worked out without running it. A file source is sniffed
    /// (CSV) or has its footer read (Parquet).
    pub fn schema(&self) -> Result<Schema> {
        let mut schema = source_schema(&self.source)?;
        for step in &self.steps {
            schema = step_schema(&schema, step)?;
        }
        Ok(schema)
    }

    /// The schema after the longest prefix of steps that type. When a step names a missing
    /// column, this is the schema that step read, which is what an error message should list.
    pub fn valid_schema(&self) -> Option<Schema> {
        let mut schema = source_schema(&self.source).ok()?;
        for step in &self.steps {
            match step_schema(&schema, step) {
                Ok(next) => schema = next,
                Err(_) => break,
            }
        }
        Some(schema)
    }
}

/// The name Polars gives an expression's output column when it has no alias: the first column it
/// reads, `len` for a row count, and `literal` for a constant.
pub fn output_name(expr: &Expr) -> PlSmallStr {
    fn first_column(expr: &Expr) -> Option<&PlSmallStr> {
        match expr {
            Expr::Column(name) => Some(name),
            Expr::Unary(_, arg) | Expr::Head(arg, _) => first_column(arg),
            Expr::Aggregate { arg, .. } => first_column(arg),
            Expr::Binary(_, left, right) | Expr::Correlation(left, right) => {
                first_column(left).or_else(|| first_column(right))
            }
            Expr::IfElse(test, yes, no) => first_column(yes)
                .or_else(|| first_column(no))
                .or_else(|| first_column(test)),
            _ => None,
        }
    }
    match (expr, first_column(expr)) {
        (Expr::Count, _) => PlSmallStr::from_static("len"),
        (_, Some(name)) => name.clone(),
        (_, None) => PlSmallStr::from_static("literal"),
    }
}

/// Whether `expr` gives one value for a whole group: every column it reads sits inside an
/// aggregation. `GroupBy.agg` accepts only such expressions.
pub fn is_aggregation(expr: &Expr) -> bool {
    collapses(expr)
}

/// Runs the plan, optimized.
pub fn collect(plan: Plan) -> Result<Collected> {
    plan.schema()?;
    run(optimize(plan)?)
}

/// [`collect`] without the optimizer, which must give the same table; the property tests compare
/// the two.
#[doc(hidden)]
pub fn collect_unoptimized(plan: Plan) -> Result<Collected> {
    plan.schema()?;
    run(plan)
}

/// The optimized plan as text, one verb per line, the source last.
pub fn explain(plan: Plan) -> Result<String> {
    let plan = optimize(plan)?;
    let mut text = String::new();
    write_plan(&mut text, &plan, 0);
    Ok(text)
}

// Optimization.

/// Rewrites the plan into one that computes the same table with less work.
pub fn optimize(plan: Plan) -> Result<Plan> {
    let Plan { mut source, steps } = plan;
    let mut steps = steps
        .into_iter()
        .map(|step| match step {
            Step::Join {
                other,
                left_on,
                right_on,
                how,
                suffix,
            } => optimize(*other).map(|other| Step::Join {
                other: Box::new(other),
                left_on,
                right_on,
                how,
                suffix,
            }),
            step => Ok(step),
        })
        .collect::<Result<Vec<_>>>()?;
    let source_schema = source_schema(&source)?;
    push_down_predicates(&source_schema, &mut steps)?;
    if let (
        Source::Csv { options, .. } | Source::Parquet { options, .. },
        Some(Step::Head(count)),
    ) = (&mut source, steps.first())
    {
        options.rows = Some(options.rows.map_or(*count, |rows| rows.min(*count)));
        steps.remove(0);
    }
    if let Source::Csv { options, .. } | Source::Parquet { options, .. } = &mut source {
        let needed = needed_columns(&source_schema, &steps);
        if needed.len() < source_schema.columns.len() {
            options.columns = Some(needed);
        }
    }
    Ok(Plan { source, steps })
}

/// Moves each filter as early as it can go. A filter that aggregates (`col("x") > mean(col("x"))`)
/// depends on which rows it sees, so it moves only below a sort, and merges with no filter
/// before it.
fn push_down_predicates(source_schema: &Schema, steps: &mut Vec<Step>) -> Result<()> {
    let mut index = 0;
    while index < steps.len() {
        let Step::Filter(predicate) = &steps[index] else {
            index += 1;
            continue;
        };
        let predicate = predicate.clone();
        let Some(before) = index.checked_sub(1) else {
            index += 1;
            continue;
        };
        let row_wise = !contains_aggregate(&predicate);
        let moved = match &steps[before] {
            Step::Filter(earlier) if row_wise => {
                let merged = Expr::Binary(
                    BinaryOp::And,
                    Box::new(earlier.clone()),
                    Box::new(predicate),
                );
                steps[before] = Step::Filter(merged);
                steps.remove(index);
                index = before;
                continue;
            }
            Step::Sort(_) => Some(predicate),
            Step::WithColumns(columns)
                if row_wise
                    && columns.iter().all(|(_, expr)| !contains_aggregate(expr))
                    && !columns.iter().any(|(name, _)| reads(&predicate, name)) =>
            {
                Some(predicate)
            }
            Step::Select(columns)
                if row_wise
                    && columns
                        .iter()
                        .all(|(_, expr)| matches!(expr, Expr::Column(_))) =>
            {
                Some(rename_columns(&predicate, columns))
            }
            // Every left column keeps its name through the join, and a right column whose name
            // the left side has is renamed by the suffix, so a predicate that reads only names of
            // the left side reads only left columns, whose values each output row copies from
            // its left row. A full join also adds rows no left row produced, so it keeps its
            // filters.
            Step::Join {
                how: How::Inner | How::Left | How::Semi | How::Anti,
                ..
            } if row_wise => {
                let left = schema_before(source_schema, &steps[..before])?;
                let mut columns = FxHashSet::default();
                columns_read(&predicate, &mut columns);
                columns
                    .iter()
                    .all(|name| left.column(name).is_some())
                    .then_some(predicate)
            }
            _ => None,
        };
        match moved {
            Some(predicate) => {
                steps.remove(index);
                steps.insert(before, Step::Filter(predicate));
                index = before;
            }
            None => index += 1,
        }
    }
    Ok(())
}

fn schema_before(source_schema: &Schema, steps: &[Step]) -> Result<Schema> {
    let mut schema = source_schema.clone();
    for step in steps {
        schema = step_schema(&schema, step)?;
    }
    Ok(schema)
}

/// The predicate over a selection's input: each selected name read as the column it selects.
fn rename_columns(expr: &Expr, columns: &[(PlSmallStr, Expr)]) -> Expr {
    map_columns(expr, &|name| {
        columns
            .iter()
            .find(|(output, _)| output == name)
            .map_or_else(|| Expr::Column(name.clone()), |(_, source)| source.clone())
    })
}

fn map_columns(expr: &Expr, rename: &dyn Fn(&PlSmallStr) -> Expr) -> Expr {
    let go = |expr: &Expr| Box::new(map_columns(expr, rename));
    match expr {
        Expr::Column(name) => rename(name),
        Expr::Unary(op, arg) => Expr::Unary(*op, go(arg)),
        Expr::Binary(op, left, right) => Expr::Binary(*op, go(left), go(right)),
        Expr::Aggregate {
            function,
            arg,
            na_rm,
        } => Expr::Aggregate {
            function: *function,
            arg: go(arg),
            na_rm: *na_rm,
        },
        Expr::Head(arg, count) => Expr::Head(go(arg), *count),
        Expr::Correlation(left, right) => Expr::Correlation(go(left), go(right)),
        Expr::IfElse(test, yes, no) => Expr::IfElse(go(test), go(yes), go(no)),
        other => other.clone(),
    }
}

fn contains_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Aggregate { .. } | Expr::Count | Expr::Head(..) | Expr::Correlation(..) => true,
        Expr::Unary(_, arg) => contains_aggregate(arg),
        Expr::Binary(_, left, right) => contains_aggregate(left) || contains_aggregate(right),
        Expr::IfElse(test, yes, no) => {
            contains_aggregate(test) || contains_aggregate(yes) || contains_aggregate(no)
        }
        _ => false,
    }
}

fn reads(expr: &Expr, column: &str) -> bool {
    let mut columns = FxHashSet::default();
    columns_read(expr, &mut columns);
    columns.contains(column)
}

fn columns_read(expr: &Expr, columns: &mut FxHashSet<PlSmallStr>) {
    match expr {
        Expr::Column(name) => {
            columns.insert(name.clone());
        }
        Expr::Unary(_, arg) | Expr::Head(arg, _) | Expr::Aggregate { arg, .. } => {
            columns_read(arg, columns);
        }
        Expr::Binary(_, left, right) | Expr::Correlation(left, right) => {
            columns_read(left, columns);
            columns_read(right, columns);
        }
        Expr::IfElse(test, yes, no) => {
            columns_read(test, columns);
            columns_read(yes, columns);
            columns_read(no, columns);
        }
        _ => {}
    }
}

/// The source columns some step reads or passes through to the result, in the source's order.
/// Walking the steps backwards, a step that replaces the columns (`select`, `agg`) resets the
/// set to what it reads; every other step adds what it reads.
fn needed_columns(source_schema: &Schema, steps: &[Step]) -> Vec<PlSmallStr> {
    let all: FxHashSet<PlSmallStr> = source_schema.names().into_iter().cloned().collect();
    let mut needed: Option<FxHashSet<PlSmallStr>> = None;
    for step in steps.iter().rev() {
        let mut read = FxHashSet::default();
        match step {
            Step::Select(columns) => {
                for (_, expr) in columns {
                    columns_read(expr, &mut read);
                }
                needed = Some(read);
                continue;
            }
            Step::Aggregate { keys, aggregates } => {
                for (_, expr) in keys.iter().chain(aggregates) {
                    columns_read(expr, &mut read);
                }
                needed = Some(read);
                continue;
            }
            Step::WithColumns(columns) => {
                for (_, expr) in columns {
                    columns_read(expr, &mut read);
                }
                if let Some(needed) = &mut needed {
                    for (name, _) in columns {
                        needed.remove(name);
                    }
                }
            }
            Step::Filter(expr) => columns_read(expr, &mut read),
            Step::Sort(keys) => {
                for key in keys {
                    columns_read(&key.expr, &mut read);
                }
            }
            Step::Join { left_on, .. } => {
                // The left side's columns all reach the result, except through a semi or anti
                // join, which a later step's reads already cover.
                read.extend(left_on.iter().cloned());
                if needed.is_some() {
                    needed = None;
                }
            }
            Step::Head(_) | Step::Tail(_) => {}
        }
        if let Some(needed) = &mut needed {
            needed.extend(read);
        }
    }
    let needed = needed.unwrap_or(all);
    source_schema
        .names()
        .into_iter()
        .filter(|name| needed.contains(*name))
        .cloned()
        .collect()
}

// Typing.

fn source_schema(source: &Source) -> Result<Schema> {
    match source {
        Source::Table(table) => Ok(table.schema()),
        Source::Csv { path, .. } => io::csv_schema(path),
        Source::Parquet { path, .. } => {
            let file = std::fs::File::open(path)?;
            let schema = ParquetReader::new(file).schema()?;
            let mut columns = Vec::with_capacity(schema.len());
            for (name, field) in schema.iter() {
                let dtype = polars::prelude::DataType::from_arrow_field(field);
                let kind = crate::ColumnType::of(&dtype).ok_or_else(|| Error::UnsupportedType {
                    name: name.to_string(),
                    found: dtype.to_string(),
                })?;
                columns.push((name.clone(), kind));
            }
            Ok(Schema { columns })
        }
    }
}

/// The columns a step produces from `schema`, checking every column it names and every
/// expression's types as [`query::result_type`] does.
pub fn step_schema(schema: &Schema, step: &Step) -> Result<Schema> {
    let table_type = |query: &Query| match query::result_type(schema, &[], query)? {
        ResultType::Table(schema) => Ok(schema),
        ResultType::Vector(_) => Err(Error::Query("the step produced a vector".to_string())),
    };
    match step {
        Step::Filter(predicate) => table_type(&Query {
            rows: Rows::Filter(predicate.clone()),
            ..Query::default()
        }),
        Step::Sort(keys) => table_type(&Query {
            rows: Rows::Order(keys.clone()),
            ..Query::default()
        }),
        Step::Head(_) | Step::Tail(_) => Ok(schema.clone()),
        Step::Select(columns) => {
            check_unique(columns.iter().map(|(name, _)| name))?;
            table_type(&Query {
                select: Select::Columns(columns.clone()),
                ..Query::default()
            })
        }
        Step::WithColumns(columns) => {
            check_unique(columns.iter().map(|(name, _)| name))?;
            table_type(&Query {
                select: Select::Columns(with_columns(schema, columns)),
                ..Query::default()
            })
        }
        Step::Aggregate { keys, aggregates } => {
            check_unique(keys.iter().chain(aggregates).map(|(name, _)| name))?;
            if let Some((name, _)) = aggregates.iter().find(|(_, expr)| !is_aggregation(expr)) {
                return Err(Error::Query(format!(
                    "`{name}` gives one value per row, but `agg` needs one value per group"
                )));
            }
            table_type(&Query {
                select: Select::Columns(aggregates.clone()),
                by: keys.clone(),
                ..Query::default()
            })
        }
        Step::Join {
            other,
            left_on,
            right_on,
            how,
            suffix,
        } => {
            let right = other.schema()?;
            check_join_keys(schema, &right, left_on, right_on)?;
            Ok(match how {
                How::Semi | How::Anti => schema.clone(),
                How::Inner | How::Left | How::Full => {
                    let mut columns = schema.columns.clone();
                    for (name, kind) in &right.columns {
                        if right_on.contains(name) {
                            continue;
                        }
                        let name = if schema.column(name).is_some() {
                            PlSmallStr::from(format!("{name}{suffix}"))
                        } else {
                            name.clone()
                        };
                        columns.push((name, *kind));
                    }
                    Schema { columns }
                }
            })
        }
    }
}

fn check_unique<'a>(names: impl Iterator<Item = &'a PlSmallStr>) -> Result<()> {
    let mut seen = FxHashSet::default();
    for name in names {
        if !seen.insert(name) {
            return Err(Error::Query(format!(
                "the column name `{name}` is repeated; give one of them another name with `Expr.alias`"
            )));
        }
    }
    Ok(())
}

fn check_join_keys(
    left: &Schema,
    right: &Schema,
    left_on: &[PlSmallStr],
    right_on: &[PlSmallStr],
) -> Result<()> {
    if left_on.is_empty() || left_on.len() != right_on.len() {
        return Err(Error::Query(
            "a join needs the same number of key columns on both sides".to_string(),
        ));
    }
    for (left_key, right_key) in left_on.iter().zip(right_on) {
        let left_type = left
            .column(left_key)
            .ok_or_else(|| Error::NoColumn(left_key.to_string()))?;
        let right_type = right
            .column(right_key)
            .ok_or_else(|| Error::NoColumn(right_key.to_string()))?;
        let numeric = |kind| matches!(kind, crate::ColumnType::Integer | crate::ColumnType::Double);
        if left_type != right_type && !(numeric(left_type) && numeric(right_type)) {
            return Err(Error::Query(format!(
                "cannot join `{left_key}` ({left_type}) with `{right_key}` ({right_type})"
            )));
        }
    }
    Ok(())
}

/// `with_columns` as a selection: every input column, each replaced by the expression that names
/// it, then the new columns in order.
fn with_columns(schema: &Schema, columns: &[(PlSmallStr, Expr)]) -> Vec<(PlSmallStr, Expr)> {
    let mut selected: Vec<(PlSmallStr, Expr)> = schema
        .names()
        .into_iter()
        .map(|name| {
            let expr = columns
                .iter()
                .find(|(output, _)| output == name)
                .map_or_else(|| Expr::Column(name.clone()), |(_, expr)| expr.clone());
            (name.clone(), expr)
        })
        .collect();
    for (name, expr) in columns {
        if schema.column(name).is_none() {
            selected.push((name.clone(), expr.clone()));
        }
    }
    selected
}

// Execution.

fn run(plan: Plan) -> Result<Collected> {
    let mut warnings = Vec::new();
    let mut table = read(plan.source)?;
    let mut steps = plan.steps.into_iter().peekable();
    while let Some(step) = steps.next() {
        let rows = match step {
            Step::Filter(predicate) => Rows::Filter(predicate),
            Step::Sort(keys) => Rows::Order(keys),
            Step::Head(count) => Rows::Positions((0..count.min(table.height()) as u32).collect()),
            Step::Tail(count) => {
                let height = table.height();
                Rows::Positions((height.saturating_sub(count) as u32..height as u32).collect())
            }
            Step::Join {
                other,
                left_on,
                right_on,
                how,
                suffix,
            } => {
                let right = run(*other)?;
                warnings.extend(right.warnings);
                table = join(
                    table,
                    right.table,
                    &left_on,
                    &right_on,
                    how,
                    &suffix,
                    &mut warnings,
                )?;
                continue;
            }
            step => {
                let query = selection(&table.schema(), step, Rows::All)?;
                table = execute(&mut table, query, &mut warnings)?;
                continue;
            }
        };
        // A selection or a grouping right after the rows runs in the same query, so the rows
        // are never gathered for columns the selection drops.
        let fusable = matches!(
            steps.peek(),
            Some(Step::Select(_) | Step::WithColumns(_) | Step::Aggregate { .. })
        );
        let query = match steps.next_if(|_| fusable) {
            Some(next) => selection(&table.schema(), next, rows)?,
            None => Query {
                rows,
                ..Query::default()
            },
        };
        table = execute(&mut table, query, &mut warnings)?;
    }
    Ok(Collected { table, warnings })
}

fn read(source: Source) -> Result<Table> {
    match source {
        Source::Table(table) => Ok(table),
        Source::Csv { path, options } => io::read_csv_with(&path, &options),
        Source::Parquet { path, options } => read_parquet(&path, &options),
    }
}

/// Reads a Parquet file, or the columns and leading rows `options` asks for.
pub fn read_parquet(path: &Path, options: &ReadOptions) -> Result<Table> {
    let file = std::fs::File::open(path)?;
    let mut reader = ParquetReader::new(file).with_slice(options.rows.map(|rows| (0, rows)));
    if let Some(columns) = &options.columns {
        reader = reader.with_columns(Some(columns.iter().map(|name| name.to_string()).collect()));
    }
    Table::new(reader.finish()?)
}

/// The query for a selection, a `with_columns` or an aggregation over `rows`.
fn selection(schema: &Schema, step: Step, rows: Rows) -> Result<Query> {
    let (select, by) = match step {
        Step::Select(columns) => (columns, Vec::new()),
        Step::WithColumns(columns) => (with_columns(schema, &columns), Vec::new()),
        Step::Aggregate { keys, aggregates } => (aggregates, keys),
        _ => {
            return Err(Error::Query(
                "only a selection or an aggregation runs as a selection".to_string(),
            ));
        }
    };
    Ok(Query {
        rows,
        select: Select::Columns(select),
        by,
        keyed: false,
    })
}

fn execute(table: &mut Table, query: Query, warnings: &mut Vec<Warning>) -> Result<Table> {
    let executed = query::execute(table, query)?;
    warnings.extend(executed.warnings);
    match executed.output {
        Output::Table(table) => Ok(table),
        _ => Err(Error::Query("the step did not produce a table".to_string())),
    }
}

/// Joins `left` with `right`. An inner or left equi join runs as data.table's `right[left, on]`
/// on the engine's join kernel, then has its columns renamed and reordered into Polars' layout:
/// the left columns, then the right's other columns, a name the left side has taking `suffix`.
/// A full, semi or anti join runs as a Polars join.
fn join(
    left: Table,
    right: Table,
    left_on: &[PlSmallStr],
    right_on: &[PlSmallStr],
    how: How,
    suffix: &str,
    warnings: &mut Vec<Warning>,
) -> Result<Table> {
    check_join_keys(&left.schema(), &right.schema(), left_on, right_on)?;
    match how {
        How::Inner | How::Left => {
            let on: Vec<On> = right_on
                .iter()
                .zip(left_on)
                .map(|(x, i)| On::equal(x.clone(), i.clone()))
                .collect();
            let left_schema = left.schema();
            let right_schema = right.schema();
            let layout = join_layout(&right_schema.names(), &left_schema.names(), &on);
            let mut select = Vec::with_capacity(layout.len());
            for (name, _) in &left_schema.columns {
                let joined = layout
                    .iter()
                    .find(|column| match column.condition {
                        Some(index) => &left_on[index] == name,
                        None => column.side == query::Side::I && &column.source == name,
                    })
                    .ok_or_else(|| Error::NoColumn(name.to_string()))?;
                select.push((name.clone(), Expr::Column(joined.name.clone())));
            }
            for column in layout.iter().filter(|column| column.side == query::Side::X) {
                let name = if left_schema.column(&column.source).is_some() {
                    PlSmallStr::from(format!("{}{suffix}", column.source))
                } else {
                    column.source.clone()
                };
                select.push((name, Expr::Column(column.name.clone())));
            }
            let mut join = Join::new(left, on);
            join.drop_unmatched = how == How::Inner;
            let mut right = right;
            let mut joined = execute(
                &mut right,
                Query {
                    rows: Rows::Join(join),
                    ..Query::default()
                },
                warnings,
            )?;
            execute(
                &mut joined,
                Query {
                    select: Select::Columns(select),
                    ..Query::default()
                },
                warnings,
            )
        }
        How::Full | How::Semi | How::Anti => {
            let join_type = match how {
                How::Full => JoinType::Full,
                How::Semi => JoinType::Semi,
                _ => JoinType::Anti,
            };
            let args = JoinArgs::new(join_type)
                .with_coalesce(JoinCoalesce::CoalesceColumns)
                .with_suffix(Some(PlSmallStr::from(suffix)));
            let left_keys: Vec<_> = left_on.iter().map(|name| col(name.clone())).collect();
            let right_keys: Vec<_> = right_on.iter().map(|name| col(name.clone())).collect();
            let frame = left
                .into_frame()
                .lazy()
                .join(right.into_frame().lazy(), left_keys, right_keys, args)
                .collect()?;
            Table::new(frame)
        }
    }
}

// Display.

fn write_plan(text: &mut String, plan: &Plan, depth: usize) {
    let mut depth = depth;
    for step in plan.steps.iter().rev() {
        indent(text, depth);
        match step {
            Step::Filter(predicate) => text.push_str(&format!("FILTER {predicate}\n")),
            Step::Select(columns) => {
                text.push_str(&format!("SELECT [{}]\n", named_list(columns)));
            }
            Step::WithColumns(columns) => {
                text.push_str(&format!("WITH_COLUMNS [{}]\n", named_list(columns)));
            }
            Step::Aggregate { keys, aggregates } => text.push_str(&format!(
                "AGGREGATE [{}] BY [{}]\n",
                named_list(aggregates),
                named_list(keys)
            )),
            Step::Sort(keys) => {
                let keys: Vec<String> = keys
                    .iter()
                    .map(|key| {
                        if key.descending {
                            format!("{} DESC", key.expr)
                        } else {
                            key.expr.to_string()
                        }
                    })
                    .collect();
                text.push_str(&format!("SORT BY [{}]\n", keys.join(", ")));
            }
            Step::Head(count) => text.push_str(&format!("HEAD {count}\n")),
            Step::Tail(count) => text.push_str(&format!("TAIL {count}\n")),
            Step::Join {
                other,
                left_on,
                right_on,
                how,
                ..
            } => {
                let keys: Vec<String> = left_on
                    .iter()
                    .zip(right_on)
                    .map(|(left, right)| {
                        if left == right {
                            format!("\"{left}\"")
                        } else {
                            format!("\"{left}\" = \"{right}\"")
                        }
                    })
                    .collect();
                text.push_str(&format!(
                    "{} JOIN ON [{}]\n",
                    how.name().to_uppercase(),
                    keys.join(", ")
                ));
                indent(text, depth + 1);
                text.push_str("RIGHT:\n");
                write_plan(text, other, depth + 2);
                indent(text, depth + 1);
                text.push_str("LEFT:\n");
                depth += 1;
            }
        }
        depth += 1;
    }
    indent(text, depth);
    let (kind, path, options) = match &plan.source {
        Source::Table(table) => {
            text.push_str(&format!(
                "DATAFRAME {} rows, {} columns\n",
                table.height(),
                table.width()
            ));
            return;
        }
        Source::Csv { path, options } => ("CSV", path, options),
        Source::Parquet { path, options } => ("PARQUET", path, options),
    };
    text.push_str(&format!("SCAN {kind} \"{}\"", path.display()));
    if let Some(columns) = &options.columns {
        let columns: Vec<String> = columns.iter().map(|name| format!("\"{name}\"")).collect();
        text.push_str(&format!(" COLUMNS [{}]", columns.join(", ")));
    }
    if let Some(rows) = options.rows {
        text.push_str(&format!(" ROWS {rows}"));
    }
    text.push('\n');
}

fn indent(text: &mut String, depth: usize) {
    for _ in 0..depth {
        text.push_str("  ");
    }
}

fn named_list(columns: &[(PlSmallStr, Expr)]) -> String {
    columns
        .iter()
        .map(|(name, expr)| {
            if *name == output_name(expr) {
                expr.to_string()
            } else {
                aliased(expr, name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// An expression renamed with `.alias`, in Polars' notation.
pub fn aliased(expr: &Expr, name: &str) -> String {
    format!("{}.alias(\"{name}\")", Receiver(expr))
}

/// Expressions print in Polars' notation, as `col("x").sum() > 1`.
impl fmt::Display for Expr {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Expr::Column(name) => write!(formatter, "col(\"{name}\")"),
            Expr::Integer(value) => write!(formatter, "{value}L"),
            Expr::Double(value) => write!(formatter, "{value:?}"),
            Expr::Logical(true) => formatter.write_str("TRUE"),
            Expr::Logical(false) => formatter.write_str("FALSE"),
            Expr::Character(value) => write!(formatter, "{value:?}"),
            Expr::Na(_) => formatter.write_str("NA"),
            Expr::Unary(UnaryOp::Negate, arg) => write!(formatter, "-{arg}"),
            Expr::Unary(UnaryOp::Not, arg) => write!(formatter, "!{arg}"),
            Expr::Unary(UnaryOp::IsNa, arg) => write!(formatter, "{}.is_null()", Receiver(arg)),
            Expr::Binary(op, left, right) => {
                let symbol = match op {
                    BinaryOp::Add => "+",
                    BinaryOp::Subtract => "-",
                    BinaryOp::Multiply => "*",
                    BinaryOp::Divide => "/",
                    BinaryOp::Power => "^",
                    BinaryOp::IntegerDivide => "%/%",
                    BinaryOp::Modulo => "%%",
                    BinaryOp::Equal => "==",
                    BinaryOp::NotEqual => "!=",
                    BinaryOp::Less => "<",
                    BinaryOp::LessEqual => "<=",
                    BinaryOp::Greater => ">",
                    BinaryOp::GreaterEqual => ">=",
                    BinaryOp::And => "&",
                    BinaryOp::Or => "|",
                };
                let operand = |formatter: &mut fmt::Formatter, expr: &Expr| match expr {
                    Expr::Binary(..) => write!(formatter, "({expr})"),
                    _ => write!(formatter, "{expr}"),
                };
                operand(formatter, left)?;
                write!(formatter, " {symbol} ")?;
                operand(formatter, right)
            }
            Expr::Aggregate { function, arg, .. } => {
                let name = match function {
                    Aggregate::Sum => "sum",
                    Aggregate::Mean => "mean",
                    Aggregate::Min => "min",
                    Aggregate::Max => "max",
                    Aggregate::First => "first",
                    Aggregate::Last => "last",
                    Aggregate::Median => "median",
                    Aggregate::Sd => "std",
                    Aggregate::Var => "var",
                    Aggregate::Length => "count",
                };
                write!(formatter, "{}.{name}()", Receiver(arg))
            }
            Expr::Count => formatter.write_str("len()"),
            Expr::Head(arg, count) => write!(formatter, "{}.head({count})", Receiver(arg)),
            Expr::Correlation(left, right) => write!(formatter, "cor({left}, {right})"),
            Expr::IfElse(test, yes, no) => {
                write!(formatter, "when({test}).then({yes}).otherwise({no})")
            }
        }
    }
}

/// An expression a method call follows, parenthesized when an operator would otherwise bind the
/// call to its last operand.
struct Receiver<'a>(&'a Expr);

impl fmt::Display for Receiver<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            Expr::Binary(..) | Expr::Unary(UnaryOp::Negate | UnaryOp::Not, _) => {
                write!(formatter, "({})", self.0)
            }
            expr => write!(formatter, "{expr}"),
        }
    }
}
