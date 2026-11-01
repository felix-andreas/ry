//! The data.table functions that real code calls beside `[`: reordering and renaming in place,
//! stacking and reshaping tables, and the vector helpers (`shift`, `fifelse`, `fcoalesce`,
//! `rleid`, `nafill`, `between`, `%chin%`, `tstrsplit`).
//!
//! Each follows data.table's semantics, which the tests pin against values read from data.table
//! under GNU R: `NA` sorts first, `NA` equals `NA` when a run or a group is formed, a stacked
//! column takes the widest type of its pieces, and a reshaped value column is coerced the same
//! way, with a warning.

use std::collections::HashMap;

use polars::prelude::{
    BooleanChunked, ChunkCompareEq, ChunkCompareIneq, ChunkFillNullValue, ChunkFull, Column,
    DataFrame, DataType, FillNullStrategy, IdxCa, IdxSize, Int32Chunked, IntoColumn, NamedFrom,
    NewChunkedArray, PlSmallStr, Series, SortMultipleOptions, SortOptions, StringChunked,
    UniqueKeepStrategy,
};
use rustc_hash::FxHashSet;

use crate::{
    ColumnType, Error, Result, Table,
    convert::{enum_dtype, factor_levels},
    query::{Aggregate, Executed, Output, Query, Select, Warning, build},
};

/// Sorts the table in place by `by`, each column ascending or descending, as `setorder` does.
/// `NA` sorts first and ties keep their order. The key is dropped, since `setorder` leaves the
/// table unkeyed.
pub fn set_order(table: &mut Table, by: &[(&str, bool)]) -> Result<()> {
    for (name, _) in by {
        require(table, name)?;
    }
    let options = SortMultipleOptions::default()
        .with_order_descending_multi(by.iter().map(|(_, descending)| *descending))
        .with_nulls_last(false)
        .with_maintain_order(true);
    let sorted = table
        .frame()
        .sort(by.iter().map(|(name, _)| *name), options)?;
    *table = Table::from_normalized(sorted);
    Ok(())
}

/// Renames columns in place, as `setnames(x, old, new)` does. A key column keeps its place in
/// the key under its new name.
pub fn set_names(table: &mut Table, renames: &[(&str, &str)]) -> Result<()> {
    for (old, _) in renames {
        require(table, old)?;
    }
    let mut frame = table.frame().clone();
    let mut key = table.key().to_vec();
    for (old, new) in renames {
        frame.rename(old, PlSmallStr::from(*new))?;
        for column in key.iter_mut().filter(|column| column.as_str() == *old) {
            *column = PlSmallStr::from(*new);
        }
    }
    let names: FxHashSet<&str> = frame
        .columns()
        .iter()
        .map(|column| column.name().as_str())
        .collect();
    if names.len() != frame.width() {
        return Err(Error::Query("the new names repeat a column name".into()));
    }
    *table = Table::from_normalized(frame).with_key(key);
    Ok(())
}

/// Moves `first` to the front in that order and keeps the rest in theirs, as `setcolorder`
/// does.
pub fn set_column_order(table: &mut Table, first: &[&str]) -> Result<()> {
    for name in first {
        require(table, name)?;
    }
    let mut order: Vec<PlSmallStr> = first.iter().map(|name| PlSmallStr::from(*name)).collect();
    for column in table.frame().columns() {
        if !first.contains(&column.name().as_str()) {
            order.push(column.name().clone());
        }
    }
    let key = table.key().to_vec();
    *table = Table::from_normalized(table.frame().select(order)?).with_key(key);
    Ok(())
}

/// The first row of each distinct combination of `by` (every column when `by` is empty), in
/// their original order, as `unique(x, by = ...)` does.
pub fn unique_rows(table: &Table, by: &[&str]) -> Result<Table> {
    let subset: Vec<String> = subset(table, by)?;
    let unique = table
        .frame()
        .unique_stable(Some(&subset), UniqueKeepStrategy::First, None)?;
    Ok(Table::from_normalized(unique))
}

/// How many distinct combinations of `by` the table holds, as `uniqueN(x, by = ...)` does.
pub fn unique_count(table: &Table, by: &[&str]) -> Result<usize> {
    let subset = subset(table, by)?;
    let frame = table.frame().select(subset)?;
    Ok(frame
        .unique_stable(None, UniqueKeepStrategy::Any, None)?
        .height())
}

/// How `rbind_list` lines up the tables' columns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bind {
    /// By position, with the first table's names, as `rbindlist` does by default.
    Position,
    /// By name, as `use.names = TRUE` does. Every table must have the same names.
    Name,
    /// By name, filling a column a table lacks with `NA`, as `fill = TRUE` does. The columns come
    /// in the order they first appear.
    Fill,
}

/// Stacks tables, as `rbindlist` does. Each column takes the widest type among its pieces
/// (logical, then integer, then double, then character), and factors combine their levels in
/// the order they appear, a character piece adding its values as levels.
pub fn rbind_list(tables: &[&Table], bind: Bind) -> Result<Table> {
    let Some(first) = tables.first() else {
        return Ok(Table::default());
    };
    let names: Vec<PlSmallStr> = match bind {
        Bind::Position => {
            for (index, table) in tables.iter().enumerate() {
                if table.width() != first.width() {
                    return Err(Error::Query(format!(
                        "item {} has {} columns, inconsistent with item 1 which has {} columns; \
                         to fill missing columns use `fill = TRUE`",
                        index + 1,
                        table.width(),
                        first.width()
                    )));
                }
            }
            first
                .frame()
                .columns()
                .iter()
                .map(|column| column.name().clone())
                .collect()
        }
        Bind::Name | Bind::Fill => {
            let mut names: Vec<PlSmallStr> = Vec::new();
            for table in tables {
                for column in table.frame().columns() {
                    if !names.contains(column.name()) {
                        names.push(column.name().clone());
                    }
                }
            }
            if bind == Bind::Name {
                for (index, table) in tables.iter().enumerate() {
                    if let Some(missing) = names
                        .iter()
                        .find(|name| table.frame().column(name).is_err())
                    {
                        return Err(Error::Query(format!(
                            "item {} has no column `{missing}`; to fill it with `NA` use \
                             `fill = TRUE`",
                            index + 1
                        )));
                    }
                }
            }
            names
        }
    };
    let pieces = |position: usize, name: &PlSmallStr| -> Vec<Option<Column>> {
        tables
            .iter()
            .map(|table| match bind {
                Bind::Position => table.frame().columns().get(position).cloned(),
                Bind::Name | Bind::Fill => table.frame().column(name).ok().cloned(),
            })
            .collect()
    };
    let height: usize = tables.iter().map(|table| table.height()).sum();
    let mut columns = Vec::with_capacity(names.len());
    for (position, name) in names.iter().enumerate() {
        let pieces = pieces(position, name);
        let dtype = stacked_type(&pieces)?;
        let mut stacked: Option<Series> = None;
        for (piece, table) in pieces.iter().zip(tables) {
            let series = match piece {
                Some(column) => cast_to(column, &dtype)?.as_materialized_series().clone(),
                None => Series::full_null(name.clone(), table.height(), &dtype),
            };
            let series = series.with_name(name.clone());
            match &mut stacked {
                None => stacked = Some(series),
                Some(stacked) => {
                    stacked.append(&series)?;
                }
            }
        }
        let column = stacked
            .unwrap_or_else(|| Series::full_null(name.clone(), 0, &dtype))
            .into_column();
        columns.push(column);
    }
    Ok(Table::from_normalized(DataFrame::new(height, columns)?))
}

/// Reshapes wide to long, as `melt(x, id.vars, measure.vars)` does: the id columns repeat once
/// per measure column, `variable` is a factor whose levels are the measure names, and `value`
/// holds the measures, coerced to their widest type with a warning when they differ.
pub fn melt(
    table: &Table,
    id: &[&str],
    measure: &[&str],
    variable: &str,
    value: &str,
) -> Result<Executed> {
    for name in id.iter().chain(measure) {
        require(table, name)?;
    }
    let measures: Vec<Option<Column>> = measure
        .iter()
        .map(|name| table.frame().column(name).ok().cloned())
        .collect();
    let dtype = stacked_type(&measures)?;
    let mut warnings = Vec::new();
    let types: FxHashSet<Option<ColumnType>> = measures
        .iter()
        .flatten()
        .map(|column| ColumnType::of(column.dtype()))
        .collect();
    if types.len() > 1
        && let Some(to) = ColumnType::of(&dtype)
    {
        warnings.push(Warning::MeltCoerced { to });
    }
    let levels: Vec<String> = measure.iter().map(|name| name.to_string()).collect();
    let variable_dtype = enum_dtype(&levels)?;
    let height = table.height();
    let mut stacked: Option<DataFrame> = None;
    for (name, column) in measure.iter().zip(measures.iter().flatten()) {
        let mut piece: Vec<Column> = Vec::with_capacity(id.len() + 2);
        for name in id {
            piece.push(table.frame().column(name)?.clone());
        }
        let label = Series::new(PlSmallStr::from(variable), [*name])
            .cast(&variable_dtype)?
            .new_from_index(0, height);
        piece.push(label.into_column());
        piece.push(cast_to(column, &dtype)?.with_name(PlSmallStr::from(value)));
        let piece = DataFrame::new(height, piece)?;
        match &mut stacked {
            None => stacked = Some(piece),
            Some(stacked) => {
                stacked.vstack_mut(&piece)?;
            }
        }
    }
    let frame = match stacked {
        Some(frame) => frame,
        None => DataFrame::empty(),
    };
    Ok(Executed {
        output: Output::Table(Table::from_normalized(frame)),
        warnings,
    })
}

/// Reshapes long to wide, as `dcast(x, rows ~ spread, value.var = value, fun.aggregate = f)`
/// does: one row per distinct combination of `rows`, sorted and keyed by them, and one column
/// per distinct value of `spread`, sorted and named by the value. A cell no input row reaches
/// holds `f` of nothing: 0 for `sum` and `length`, `NaN` for `mean`, and `NA` otherwise.
///
/// Without `f`, a cell holds its one value, unless some cell gets several, in which case dcast
/// counts them instead and warns, as data.table does.
pub fn dcast(
    table: &Table,
    rows: &[&str],
    spread: &str,
    value: &str,
    function: Option<Aggregate>,
) -> Result<Executed> {
    for name in rows.iter().chain([&spread, &value]) {
        require(table, name)?;
    }
    let mut warnings = Vec::new();
    let mut by: Vec<&str> = rows.to_vec();
    by.push(spread);
    let function = match function {
        Some(function) => function,
        None if unique_count(table, &by)? == table.height() => Aggregate::First,
        None => {
            warnings.push(Warning::CastCounted);
            Aggregate::Length
        }
    };
    let query = Query {
        select: Select::Columns(vec![(
            PlSmallStr::from(value),
            build::aggregate(function, build::column(value)),
        )]),
        by: by
            .iter()
            .map(|name| (PlSmallStr::from(*name), build::column(name)))
            .collect(),
        ..Query::default()
    };
    let mut subject = table.fork();
    let grouped = match crate::query::execute(&mut subject, query)?.output {
        Output::Table(grouped) => grouped,
        _ => return Err(Error::Query("dcast's grouping gave no table".into())),
    };
    let options = SortMultipleOptions::default()
        .with_nulls_last(false)
        .with_maintain_order(true);
    let grouped = grouped.frame().sort(by.iter().copied(), options.clone())?;
    let row_columns: Vec<Column> = rows
        .iter()
        .map(|name| grouped.column(name).cloned())
        .collect::<std::result::Result<_, _>>()?;
    let row_ids = run_ids(&row_columns, grouped.height())?;
    let row_count = row_ids.last().map_or(0, |last| *last as usize + 1);

    let spread_column = grouped.column(spread)?.as_materialized_series();
    let labels_series = spread_column.unique_stable()?.sort(
        SortOptions::default()
            .with_nulls_last(false)
            .with_maintain_order(true),
    )?;
    let label_text = |series: &Series| -> Result<Vec<String>> {
        let text = series.cast(&DataType::String)?;
        Ok(text
            .str()?
            .iter()
            .map(|label| label.unwrap_or("NA").to_string())
            .collect())
    };
    let labels = label_text(&labels_series)?;
    let positions: HashMap<&str, usize> = labels
        .iter()
        .enumerate()
        .map(|(position, label)| (label.as_str(), position))
        .collect();
    let cell_labels = label_text(spread_column)?;
    let mut cells: Vec<Vec<Option<IdxSize>>> = vec![vec![None; row_count]; labels.len()];
    for (index, (row, label)) in row_ids.iter().zip(&cell_labels).enumerate() {
        if let Some(position) = positions.get(label.as_str())
            && let Some(slot) = cells
                .get_mut(*position)
                .and_then(|column| column.get_mut(*row as usize))
        {
            *slot = Some(index as IdxSize);
        }
    }

    let mut first_rows: Vec<IdxSize> = Vec::with_capacity(row_count);
    for (index, row) in row_ids.iter().enumerate() {
        if index == 0 || row_ids.get(index - 1) != Some(row) {
            first_rows.push(index as IdxSize);
        }
    }
    let first_rows = IdxCa::from_vec(PlSmallStr::EMPTY, first_rows);
    let mut columns: Vec<Column> = Vec::with_capacity(rows.len() + labels.len());
    for column in &row_columns {
        columns.push(column.take(&first_rows)?);
    }
    let values = grouped.column(value)?.as_materialized_series();
    for (label, cells) in labels.iter().zip(cells) {
        let taken = values
            .take(&IdxCa::from_iter_options(
                PlSmallStr::EMPTY,
                cells.into_iter(),
            ))?
            .with_name(PlSmallStr::from(label.as_str()));
        columns.push(fill_empty(taken, function)?.into_column());
    }
    let key = rows.iter().map(|name| PlSmallStr::from(*name)).collect();
    let table = Table::from_normalized(DataFrame::new(row_count, columns)?).with_key(key);
    Ok(Executed {
        output: Output::Table(table),
        warnings,
    })
}

/// Shifts a column by `offset` places, as `shift(x, n)` does: a positive offset lags, so the
/// first values become `NA`, and a negative one leads.
pub fn shift(column: &Column, offset: i64) -> Column {
    column.shift(offset)
}

/// Picks `yes` where `test` is true and `no` where it is false, as `fifelse` does. A length-one
/// `yes`, `no` or `na` is recycled. An integer meets a double as a double; any other mismatch is
/// an error, because `fifelse` refuses to guess. Where `test` is `NA` the result is `na`, or
/// `NA` without it.
pub fn fifelse(test: &Column, yes: &Column, no: &Column, na: Option<&Column>) -> Result<Column> {
    let mask = test
        .bool()
        .map_err(|_| Error::Query("`test` must be a logical vector".into()))?;
    let length = test.len();
    let mut dtype = common_type("yes", yes, "no", no)?;
    if let Some(na) = na {
        dtype = common_type("yes", &yes.cast(&dtype)?, "na", na)?;
    }
    let yes = recycle("yes", &cast_to(yes, &dtype)?, length)?;
    let no = recycle("no", &cast_to(no, &dtype)?, length)?;
    let picked = yes.zip_with(&mask.fill_null_with_values(false)?, &no)?;
    if mask.null_count() == 0 {
        return Ok(picked);
    }
    let na = match na {
        Some(na) => recycle("na", &cast_to(na, &dtype)?, length)?,
        None => Column::full_null(picked.name().clone(), length, &dtype),
    };
    Ok(picked.zip_with(&mask.is_not_null(), &na)?)
}

/// The first non-`NA` value at each position across the columns, as `fcoalesce` does. Every
/// column must have the first one's type, and a length-one column is recycled.
pub fn fcoalesce(columns: &[&Column]) -> Result<Column> {
    let Some(first) = columns.first() else {
        return Err(Error::Query("fcoalesce needs at least one vector".into()));
    };
    let length = first.len();
    let mut result = (*first).clone();
    for (index, column) in columns.iter().enumerate().skip(1) {
        if column.dtype() != first.dtype() {
            return Err(Error::Query(format!(
                "item {} is type {} but the first item is type {}; coerce before coalescing",
                index + 1,
                type_name(column.dtype()),
                type_name(first.dtype())
            )));
        }
        if result.null_count() == 0 {
            break;
        }
        let next = recycle("item", column, length)?;
        let present = result.is_not_null();
        result = result.zip_with(&present, &next)?;
    }
    Ok(result)
}

/// A run id for each row, as `rleid(...)` does: it starts at 1 and grows by one wherever any of
/// the columns differs from the row before, with `NA` equal to `NA`.
pub fn rleid(columns: &[&Column]) -> Result<Column> {
    let length = columns.first().map_or(0, |column| column.len());
    let owned: Vec<Column> = columns.iter().map(|column| (*column).clone()).collect();
    let ids = run_ids(&owned, length)?;
    let ids: Vec<i32> = ids.into_iter().map(|id| id as i32 + 1).collect();
    Ok(Int32Chunked::from_vec(PlSmallStr::from("rleid"), ids).into_column())
}

/// How `nafill` replaces an `NA`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fill {
    /// With the last value before it, as `type = "locf"` does.
    Forward,
    /// With the next value after it, as `type = "nocb"` does.
    Backward,
    /// With a constant, as `fill = value` does.
    Value(f64),
}

/// Replaces `NA`s in a numeric column, as `nafill` does. A constant written into an integer
/// column is truncated to an integer.
pub fn nafill(column: &Column, fill: Fill) -> Result<Column> {
    if !matches!(column.dtype(), DataType::Int32 | DataType::Float64) {
        return Err(Error::Query(format!(
            "nafill fills numeric vectors, not {}",
            type_name(column.dtype())
        )));
    }
    Ok(match fill {
        Fill::Forward => column.fill_null(FillNullStrategy::Forward(None))?,
        Fill::Backward => column.fill_null(FillNullStrategy::Backward(None))?,
        Fill::Value(value) => {
            let constant = Series::new(column.name().clone(), [value])
                .cast(column.dtype())?
                .new_from_index(0, column.len())
                .into_column();
            column.zip_with(&column.is_not_null(), &constant)?
        }
    })
}

/// Whether each value lies between the bounds, as `between(x, lower, upper)` does, with the
/// bounds included unless `inclusive` is false. `NA` stays `NA`.
pub fn between(column: &Column, lower: f64, upper: f64, inclusive: bool) -> Result<Column> {
    let values = column.cast(&DataType::Float64)?;
    let values = values.f64()?;
    let result: BooleanChunked = if inclusive {
        &values.gt_eq(lower) & &values.lt_eq(upper)
    } else {
        &values.gt(lower) & &values.lt(upper)
    };
    Ok(result.with_name(column.name().clone()).into_column())
}

/// Whether each string is one of `set`, as `x %chin% set` does. `NA` matches an `NA` in the set.
pub fn chin(column: &Column, set: &[Option<&str>]) -> Result<Column> {
    let text = column.cast(&DataType::String)?;
    let strings = text.str()?;
    let wanted: FxHashSet<&str> = set.iter().flatten().copied().collect();
    let na_matches = set.iter().any(Option::is_none);
    let result: BooleanChunked = strings
        .iter()
        .map(|value| {
            Some(match value {
                Some(value) => wanted.contains(value),
                None => na_matches,
            })
        })
        .collect();
    Ok(result.with_name(column.name().clone()).into_column())
}

/// Splits each string on a fixed separator into one column per part, as
/// `tstrsplit(x, split, fixed = TRUE)` does. A string with fewer parts gets `NA` in the rest.
pub fn tstrsplit(column: &Column, separator: &str) -> Result<Vec<Column>> {
    if separator.is_empty() {
        return Err(Error::Query("the separator must not be empty".into()));
    }
    let text = column.cast(&DataType::String)?;
    let strings = text.str()?;
    let width = strings
        .iter()
        .map(|value| value.map_or(1, |value| value.split(separator).count()))
        .max()
        .unwrap_or(0);
    let mut parts: Vec<Vec<Option<&str>>> = vec![Vec::with_capacity(strings.len()); width];
    for value in strings.iter() {
        let mut pieces = value.map(|value| value.split(separator));
        for part in &mut parts {
            part.push(pieces.as_mut().and_then(Iterator::next));
        }
    }
    Ok(parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            StringChunked::from_iter_options(
                PlSmallStr::from(format!("V{}", index + 1)),
                part.into_iter(),
            )
            .into_column()
        })
        .collect())
}

fn require(table: &Table, name: &str) -> Result<()> {
    table
        .frame()
        .column(name)
        .map(|_| ())
        .map_err(|_| Error::NoColumn(name.to_string()))
}

fn subset(table: &Table, by: &[&str]) -> Result<Vec<String>> {
    if by.is_empty() {
        return Ok(table
            .frame()
            .columns()
            .iter()
            .map(|column| column.name().to_string())
            .collect());
    }
    for name in by {
        require(table, name)?;
    }
    Ok(by.iter().map(|name| name.to_string()).collect())
}

/// The zero-based run id of each row: it grows wherever any column differs from the row before,
/// with `NA` equal to `NA`.
fn run_ids(columns: &[Column], length: usize) -> Result<Vec<u32>> {
    let mut changed = BooleanChunked::full(PlSmallStr::EMPTY, false, length);
    for column in columns {
        if column.len() != length {
            return Err(Error::Query(format!(
                "column `{}` has {} values, but the first has {length}",
                column.name(),
                column.len()
            )));
        }
        let series = column.as_materialized_series();
        let previous = series.shift(1);
        let differs = series.not_equal_missing(&previous)?;
        changed = &changed | &differs;
    }
    let mut ids = Vec::with_capacity(length);
    let mut id = 0u32;
    for (index, differs) in changed.iter().enumerate() {
        if index > 0 && differs == Some(true) {
            id += 1;
        }
        ids.push(id);
    }
    Ok(ids)
}

/// The type a column stacked from these pieces takes: the widest of logical, integer, double and
/// character, or a factor over every piece's levels in order when the pieces are factors and
/// strings.
fn stacked_type(pieces: &[Option<Column>]) -> Result<DataType> {
    let kinds: Vec<ColumnType> = pieces
        .iter()
        .flatten()
        .filter_map(|column| ColumnType::of(column.dtype()))
        .collect();
    let has = |kind: ColumnType| kinds.contains(&kind);
    if has(ColumnType::Factor)
        && kinds
            .iter()
            .all(|kind| matches!(kind, ColumnType::Factor | ColumnType::Character))
    {
        let mut levels: Vec<String> = Vec::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for column in pieces.iter().flatten() {
            let values: Vec<String> = match factor_levels(column.dtype()) {
                Some(levels) => levels,
                None => column
                    .as_materialized_series()
                    .unique_stable()?
                    .str()?
                    .iter()
                    .flatten()
                    .map(str::to_string)
                    .collect(),
            };
            for level in values {
                if seen.insert(level.clone()) {
                    levels.push(level);
                }
            }
        }
        return enum_dtype(&levels);
    }
    Ok(if has(ColumnType::Character) || has(ColumnType::Factor) {
        DataType::String
    } else if has(ColumnType::Double) {
        DataType::Float64
    } else if has(ColumnType::Integer) {
        DataType::Int32
    } else {
        DataType::Boolean
    })
}

/// Casts a column to a stacked type. A factor going to another factor's levels passes through
/// its labels, so codes are matched by label rather than by position.
fn cast_to(column: &Column, dtype: &DataType) -> Result<Column> {
    if column.dtype() == dtype {
        return Ok(column.clone());
    }
    let column = match (column.dtype(), dtype) {
        (DataType::Enum(..), DataType::Enum(..)) => column.cast(&DataType::String)?,
        _ => column.clone(),
    };
    Ok(column.cast(dtype)?)
}

/// The type `fifelse` gives two branches: the same type, or a double when an integer meets one.
fn common_type(
    left_name: &str,
    left: &Column,
    right_name: &str,
    right: &Column,
) -> Result<DataType> {
    match (left.dtype(), right.dtype()) {
        (left, right) if left == right => Ok(left.clone()),
        (DataType::Int32, DataType::Float64) | (DataType::Float64, DataType::Int32) => {
            Ok(DataType::Float64)
        }
        (DataType::Null, other) | (other, DataType::Null) => Ok(other.clone()),
        (left, right) => Err(Error::Query(format!(
            "`{left_name}` is {} but `{right_name}` is {}; give both the same type",
            type_name(left),
            type_name(right)
        ))),
    }
}

fn recycle(name: &str, column: &Column, length: usize) -> Result<Column> {
    match column.len() {
        len if len == length => Ok(column.clone()),
        1 => Ok(column.new_from_index(0, length)),
        len => Err(Error::Query(format!(
            "`{name}` has {len} values, but the test has {length}"
        ))),
    }
}

/// Fills the cells no input row reached with the aggregate of nothing.
fn fill_empty(values: Series, function: Aggregate) -> Result<Series> {
    let mask = values.is_not_null();
    let empty = match function {
        Aggregate::Sum | Aggregate::Length => 0.0,
        Aggregate::Mean => f64::NAN,
        _ => return Ok(values),
    };
    if mask.all() {
        return Ok(values);
    }
    let constant = Series::new(values.name().clone(), [empty])
        .cast(values.dtype())?
        .new_from_index(0, values.len());
    Ok(values.zip_with(&mask, &constant)?)
}

fn type_name(dtype: &DataType) -> &'static str {
    match ColumnType::of(dtype) {
        Some(ColumnType::Logical) => "logical",
        Some(ColumnType::Integer) => "integer",
        Some(ColumnType::Double) => "double",
        Some(ColumnType::Character) => "character",
        Some(ColumnType::Factor) => "factor",
        None => "unsupported",
    }
}
