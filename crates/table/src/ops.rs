//! The data.table functions that real code calls beside `[`: reordering and renaming in place,
//! stacking and reshaping tables, and the vector helpers (`shift`, `fifelse`, `fcoalesce`,
//! `rleid`, `nafill`, `between`, `%chin%`, `tstrsplit`).
//!
//! Each follows data.table's semantics, which the tests pin against values read from data.table
//! under GNU R: `NA` sorts first, `NA` equals `NA` when a run or a group is formed, a stacked
//! column takes the widest type of its pieces, and a reshaped value column is coerced the same
//! way, with a warning.

use {
    crate::{
        ColumnType, Error, Result, Table,
        convert::{enum_dtype, factor_levels},
        io::write_double,
        kernel::{
            CHUNK_ROWS, CodeValues, Codes, PARALLEL_ROWS, Shared, Vector, fill, gather_column,
            pack_map, pack_rows,
        },
        query::{Aggregate, Executed, Output, Query, Select, Warning, build, execute},
    },
    polars::prelude::{
        BooleanChunked, ChunkCompareEq, ChunkCompareIneq, ChunkFillNullValue, ChunkFull, Column,
        DataFrame, DataType, FillNullStrategy, IdxCa, IdxSize, Int32Chunked, IntoColumn, NamedFrom,
        NewChunkedArray, PlSmallStr, Series, SortMultipleOptions, SortOptions, StringChunked,
        UniqueKeepStrategy,
    },
    polars_arrow::{
        array::Array,
        bitmap::{Bitmap, MutableBitmap},
    },
    polars_buffer::Buffer,
    polars_core::runtime::THREAD_POOL,
    rayon::prelude::*,
    rustc_hash::FxHashSet,
    std::{
        collections::HashMap,
        mem::swap,
        sync::atomic::{AtomicU64, Ordering},
    },
};

/// The row number's share of a packed sort key: a table has fewer than 2^32 rows.
const ROW_BITS: u32 = 32;
/// Up to this many key bits, `unique` and `uniqueN` record each key's first row in a flat table
/// of 2^bits entries instead of sorting.
const DENSE_BITS: u32 = 24;
/// The widest dense table each parallel task keeps a copy of.
const PARALLEL_DENSE_BITS: u32 = 20;
/// A radix sort over at most this many key bits sorts them in one pass.
const ONE_PASS_BITS: u32 = 16;
/// The digit width of each pass of a radix sort over more bits.
const RADIX_BITS: u32 = 11;
/// LSD passes beyond this cost more than one MSD pass followed by in-cache bucket sorts.
const MAX_LSD_PASSES: u32 = 3;
/// The digit width of the MSD pass, which leaves buckets of a few hundred keys at ten million rows.
const MSD_BITS: u32 = 16;
/// A string ranking run looks each short string up in a direct-mapped cache of 2^CACHE_BITS
/// entries before its hash map, which pays off while the column has few distinct strings.
const CACHE_BITS: u32 = 12;
/// Past this many distinct strings in a run the cache mostly misses, so the run stops using it.
const CACHED_DISTINCT: usize = 1 << (CACHE_BITS - 2);
/// The longest string a view holds inline.
const INLINE_BYTES: u32 = 12;
/// Up to this many strings, `%chin%` compares each value against every string in the set.
const SHORT_SET: usize = 8;

/// Sorts the table in place by `by`, each column ascending or descending, as `setorder` does.
/// `NA` sorts first and ties keep their order. The key is dropped, since `setorder` leaves the
/// table unkeyed.
pub fn set_order(table: &mut Table, by: &[(&str, bool)]) -> Result<()> {
    for (name, _) in by {
        require(table, name)?;
    }
    *table = Table::from_normalized(sort_frame(table.frame(), by)?);
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
    let frame = table.frame();
    if let Some(digits) = key_digits(frame, &subset)?
        && let Some(rows) = first_rows(&digits, frame.height())
    {
        return Ok(Table::from_normalized(gather_frame(frame, &rows)?));
    }
    let unique = frame.unique_stable(Some(&subset), UniqueKeepStrategy::First, None)?;
    Ok(Table::from_normalized(unique))
}

/// How many distinct combinations of `by` the table holds, as `uniqueN(x, by = ...)` does.
pub fn unique_count(table: &Table, by: &[&str]) -> Result<usize> {
    let subset = subset(table, by)?;
    if let Some(digits) = key_digits(table.frame(), &subset)? {
        return Ok(distinct_count(&digits, table.height()));
    }
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
    let grouped = match execute(&mut subject, query)?.output {
        Output::Table(grouped) => grouped,
        _ => return Err(Error::Query("dcast's grouping gave no table".into())),
    };
    let ascending: Vec<(&str, bool)> = by.iter().map(|name| (*name, false)).collect();
    let grouped = sort_frame(grouped.frame(), &ascending)?;
    let row_columns: Vec<Column> = rows
        .iter()
        .map(|name| grouped.column(name).cloned())
        .collect::<std::result::Result<_, _>>()?;
    let row_ids = run_ids(&row_columns, grouped.height(), 0)?;
    let row_count = row_ids.last().map_or(0, |last| *last as usize + 1);

    let spread_column = grouped.column(spread)?.as_materialized_series();
    let labels_series = spread_column.unique_stable()?.sort(
        SortOptions::default()
            .with_nulls_last(false)
            .with_maintain_order(true),
    )?;
    // A label is the value as R's `as.character` spells it, which for a double is the shortest
    // round-tripping form; an `NA` stays `None` so that it never meets a literal "NA" string.
    let label_text = |series: &Series| -> Result<Vec<Option<String>>> {
        if let Ok(doubles) = series.f64() {
            return Ok(doubles
                .iter()
                .map(|label| {
                    label.map(|value| {
                        let mut buffer = Vec::new();
                        write_double(&mut buffer, value);
                        String::from_utf8_lossy(&buffer).into_owned()
                    })
                })
                .collect());
        }
        let text = series.cast(&DataType::String)?;
        Ok(text
            .str()?
            .iter()
            .map(|label| label.map(str::to_string))
            .collect())
    };
    let labels = label_text(&labels_series)?;
    let positions: HashMap<Option<&str>, usize> = labels
        .iter()
        .enumerate()
        .map(|(position, label)| (label.as_deref(), position))
        .collect();
    let cell_labels = label_text(spread_column)?;
    let mut cells: Vec<Vec<Option<IdxSize>>> = vec![vec![None; row_count]; labels.len()];
    for (index, (row, label)) in row_ids.iter().zip(&cell_labels).enumerate() {
        if let Some(position) = positions.get(&label.as_deref())
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
            .with_name(PlSmallStr::from(label.as_deref().unwrap_or("NA")));
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
    // Every item is checked, even those a full result never reads, as data.table does.
    for (index, column) in columns.iter().enumerate().skip(1) {
        if column.dtype() != first.dtype() {
            return Err(Error::Query(format!(
                "item {} is type {} but the first item is type {}; coerce before coalescing",
                index + 1,
                type_name(column.dtype()),
                type_name(first.dtype())
            )));
        }
    }
    let mut result = (*first).clone();
    for column in columns.iter().skip(1) {
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
    let ids = run_ids(&owned, length, 1)?;
    let ids: Vec<i32> = ids.into_iter().map(|id| id as i32).collect();
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
    let name = column.name().clone();
    // data.table's default `nan = NA` fills `NaN` like `NA`, so a `NaN` becomes a missing value
    // before the fill runs.
    let vector = match Vector::of(column) {
        Some(Vector::Double { values, validity }) if values.iter().any(|value| value.is_nan()) => {
            let numbers = pack_map(values.len(), &values, |value: f64| !value.is_nan());
            let validity = match validity {
                Some(validity) => &validity & &numbers,
                None => numbers,
            };
            Some(Vector::Double {
                values,
                validity: Some(validity),
            })
        }
        vector => vector,
    };
    match (vector, fill) {
        (
            Some(Vector::Integer {
                values,
                validity: Some(validity),
            }),
            Fill::Forward | Fill::Backward,
        ) => {
            let (values, validity) = carry(&values, &validity, fill == Fill::Backward);
            return Ok(Vector::Integer {
                values: Buffer::from(values),
                validity,
            }
            .into_column(name));
        }
        (
            Some(Vector::Double {
                values,
                validity: Some(validity),
            }),
            Fill::Forward | Fill::Backward,
        ) => {
            let (values, validity) = carry(&values, &validity, fill == Fill::Backward);
            return Ok(Vector::Double {
                values: Buffer::from(values),
                validity,
            }
            .into_column(name));
        }
        (
            Some(Vector::Double {
                values,
                validity: Some(validity),
            }),
            Fill::Value(value),
        ) => {
            let values = fill_rows(values.len(), |row| {
                if validity.get_bit(row) {
                    values[row]
                } else {
                    value
                }
            });
            return Ok(Vector::Double {
                values: Buffer::from(values),
                validity: None,
            }
            .into_column(name));
        }
        (Some(Vector::Integer { .. } | Vector::Double { .. }), _) if column.null_count() == 0 => {
            return Ok(column.clone());
        }
        _ => {}
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
    let test = |value: f64| {
        if inclusive {
            value >= lower && value <= upper
        } else {
            value > lower && value < upper
        }
    };
    let name = column.name().clone();
    match Vector::of(column) {
        Some(Vector::Double { values, validity }) => {
            let values = pack_map(values.len(), &values, test);
            return Ok(Vector::Logical { values, validity }.into_column(name));
        }
        Some(Vector::Integer { values, validity }) => {
            let values = pack_map(values.len(), &values, |value| test(f64::from(value)));
            return Ok(Vector::Logical { values, validity }.into_column(name));
        }
        _ => {}
    }
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
    let strings = text.str()?.rechunk();
    let wanted: FxHashSet<&str> = set.iter().flatten().copied().collect();
    let listed: Vec<&str> = wanted.iter().copied().collect();
    let na_matches = set.iter().any(Option::is_none);
    let Some(array) = strings.downcast_iter().next() else {
        return Ok(BooleanChunked::full(column.name().clone(), false, 0).into_column());
    };
    // A short set is faster to scan than to hash into, and `%chin%` sets are usually short.
    let values = pack_rows(array.len(), |row| {
        if !array.is_valid(row) {
            return na_matches;
        }
        let value = array.value(row);
        if listed.len() <= SHORT_SET {
            listed.contains(&value)
        } else {
            wanted.contains(value)
        }
    });
    Ok(Vector::Logical {
        values,
        validity: None,
    }
    .into_column(column.name().clone()))
}

/// Splits each string on a fixed separator into one column per part, as
/// `tstrsplit(x, split, fixed = TRUE)` does. A string with fewer parts gets `NA` in the rest.
pub fn tstrsplit(column: &Column, separator: &str) -> Result<Vec<Column>> {
    if separator.is_empty() {
        return Err(Error::Query("the separator must not be empty".into()));
    }
    let text = column.cast(&DataType::String)?;
    let strings = text.str()?;
    // `strsplit` drops one empty piece after a trailing separator, so `"a,b,"` has two pieces
    // and `""` has none.
    fn split<'a>(value: &'a str, separator: &str) -> Vec<&'a str> {
        let mut pieces: Vec<&str> = value.split(separator).collect();
        if pieces.last().is_some_and(|last| last.is_empty()) {
            pieces.pop();
        }
        pieces
    }
    let pieces: Vec<Option<Vec<&str>>> = strings
        .iter()
        .map(|value| value.map(|value| split(value, separator)))
        .collect();
    let width = pieces
        .iter()
        .map(|pieces| pieces.as_ref().map_or(1, Vec::len))
        .max()
        .unwrap_or(0);
    let mut parts: Vec<Vec<Option<&str>>> = vec![Vec::with_capacity(strings.len()); width];
    for pieces in &pieces {
        for (index, part) in parts.iter_mut().enumerate() {
            part.push(
                pieces
                    .as_ref()
                    .and_then(|pieces| pieces.get(index).copied()),
            );
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

/// The run id of each row, counting from `first`: it grows wherever any column differs from the
/// row before, with `NA` equal to `NA`.
fn run_ids(columns: &[Column], length: usize, first: u32) -> Result<Vec<u32>> {
    let mut changed = vec![false; length];
    for column in columns {
        if column.len() != length {
            return Err(Error::Query(format!(
                "column `{}` has {} values, but the first has {length}",
                column.name(),
                column.len()
            )));
        }
        mark_changes(column, &mut changed)?;
    }
    let counts: Vec<u32> = THREAD_POOL.install(|| {
        changed
            .par_chunks(CHUNK_ROWS)
            .map(|chunk| chunk.iter().filter(|changed| **changed).count() as u32)
            .collect()
    });
    let mut bases = Vec::with_capacity(counts.len());
    let mut total = first;
    for count in counts {
        bases.push(total);
        total += count;
    }
    let mut ids = vec![0u32; length];
    THREAD_POOL.install(|| {
        ids.par_chunks_mut(CHUNK_ROWS)
            .zip(changed.par_chunks(CHUNK_ROWS))
            .zip(bases.par_iter())
            .for_each(|((out, changed), base)| {
                let mut id = *base;
                for (slot, changed) in out.iter_mut().zip(changed) {
                    id += u32::from(*changed);
                    *slot = id;
                }
            });
    });
    Ok(ids)
}

/// Sets `changed[row]` where the column's value at `row` differs from the one before it.
fn mark_changes(column: &Column, changed: &mut [bool]) -> Result<()> {
    fn mark(changed: &mut [bool], differs: impl Fn(usize) -> bool + Sync) {
        THREAD_POOL.install(|| {
            changed
                .par_chunks_mut(CHUNK_ROWS)
                .enumerate()
                .for_each(|(index, chunk)| {
                    let start = index * CHUNK_ROWS;
                    for (offset, slot) in chunk.iter_mut().enumerate() {
                        let row = start + offset;
                        if row > 0 && differs(row) {
                            *slot = true;
                        }
                    }
                });
        });
    }
    fn valid(validity: Option<&Bitmap>, row: usize) -> bool {
        validity.is_none_or(|validity| validity.get_bit(row))
    }
    // Two `NA`s are equal, an `NA` and a value differ, and two values compare with `same`.
    fn differs(validity: Option<&Bitmap>, row: usize, same: impl Fn() -> bool) -> bool {
        match (valid(validity, row), valid(validity, row - 1)) {
            (true, true) => !same(),
            (false, false) => false,
            _ => true,
        }
    }
    match (Vector::of(column), column.dtype()) {
        (Some(Vector::Integer { values, validity }), _) => mark(changed, |row| {
            differs(validity.as_ref(), row, || values[row] == values[row - 1])
        }),
        (Some(Vector::Double { values, validity }), _) => mark(changed, |row| {
            differs(validity.as_ref(), row, || {
                let (value, previous) = (values[row], values[row - 1]);
                value == previous || value.to_bits() == previous.to_bits()
            })
        }),
        (Some(Vector::Logical { values, validity }), _) => mark(changed, |row| {
            differs(validity.as_ref(), row, || {
                values.get_bit(row) == values.get_bit(row - 1)
            })
        }),
        (None, DataType::String) => {
            let strings = column.as_materialized_series().str()?.rechunk();
            if let Some(array) = strings.downcast_iter().next() {
                mark(changed, |row| {
                    differs(array.validity(), row, || {
                        array.value(row) == array.value(row - 1)
                    })
                });
            }
        }
        (None, DataType::Enum(..)) if let Some(codes) = Codes::of(column) => {
            mark(changed, |row| {
                differs(codes.validity.as_ref(), row, || {
                    code_at(&codes.values, row) == code_at(&codes.values, row - 1)
                })
            });
        }
        _ => {
            let series = column.as_materialized_series();
            let differs = series.not_equal_missing(&series.shift(1))?;
            for (row, (slot, differs)) in changed.iter_mut().zip(differs.iter()).enumerate() {
                if row > 0 && differs == Some(true) {
                    *slot = true;
                }
            }
        }
    }
    Ok(())
}

fn code_at(values: &CodeValues, row: usize) -> u32 {
    match values {
        CodeValues::U8(values) => u32::from(values[row]),
        CodeValues::U16(values) => u32::from(values[row]),
        CodeValues::U32(values) => values[row],
    }
}

/// `values` with each `NA` replaced by the last value before it, or the next value after it when
/// `backward`, as `nafill`'s `locf` and `nocb` do. Only the `NA`s with no value on that side stay.
///
/// The chunks fill in parallel, each from its own values, and then the `NA`s at a chunk's start
/// (its end, when `backward`) take the nearest value of the chunks before (after) it.
fn carry<T: Copy + Send + Sync>(
    values: &[T],
    validity: &Bitmap,
    backward: bool,
) -> (Vec<T>, Option<Bitmap>) {
    let length = values.len();
    let mut out = fill_rows(length, |row| values[row]);
    // Per chunk: how many `NA`s its own values left unfilled, and the value it carries out.
    let ends: Vec<(usize, Option<T>)> = THREAD_POOL.install(|| {
        out.par_chunks_mut(CHUNK_ROWS)
            .enumerate()
            .map(|(index, chunk)| {
                let start = index * CHUNK_ROWS;
                let mut last: Option<T> = None;
                let mut unfilled = 0usize;
                let mut visit = |offset: usize, chunk: &mut [T]| {
                    if validity.get_bit(start + offset) {
                        last = Some(chunk[offset]);
                    } else if let Some(value) = last {
                        chunk[offset] = value;
                    } else {
                        unfilled += 1;
                    }
                };
                if backward {
                    for offset in (0..chunk.len()).rev() {
                        visit(offset, chunk);
                    }
                } else {
                    for offset in 0..chunk.len() {
                        visit(offset, chunk);
                    }
                }
                (unfilled, last)
            })
            .collect()
    });
    let mut incoming: Vec<Option<T>> = vec![None; ends.len()];
    let mut carried: Option<T> = None;
    let order: Vec<usize> = if backward {
        (0..ends.len()).rev().collect()
    } else {
        (0..ends.len()).collect()
    };
    let mut unfilled = 0usize;
    for index in order {
        incoming[index] = carried;
        let (left, last) = ends[index];
        if carried.is_none() {
            unfilled += left;
        }
        carried = last.or(carried);
    }
    THREAD_POOL.install(|| {
        out.par_chunks_mut(CHUNK_ROWS)
            .zip(ends.par_iter().zip(incoming.par_iter()))
            .for_each(|(chunk, ((left, _), incoming))| {
                if let Some(value) = incoming {
                    let size = chunk.len();
                    let range = if backward {
                        size - left..size
                    } else {
                        0..*left
                    };
                    chunk[range].fill(*value);
                }
            });
    });
    if unfilled == 0 {
        return (out, None);
    }
    let mut bits = MutableBitmap::with_capacity(length);
    if backward {
        bits.extend_constant(length - unfilled, true);
        bits.extend_constant(unfilled, false);
    } else {
        bits.extend_constant(unfilled, false);
        bits.extend_constant(length - unfilled, true);
    }
    (out, Some(bits.into()))
}

/// The frame sorted by `by`, each column ascending or descending, with `NA` first and ties in row
/// order, as data.table's `setorder` and `setkey` sort.
pub(crate) fn sort_frame(frame: &DataFrame, by: &[(&str, bool)]) -> Result<DataFrame> {
    let mut digits = Vec::with_capacity(by.len());
    for (name, descending) in by {
        match order_digits(frame.column(name)?, *descending)? {
            Some(digit) => digits.push(digit),
            None => {
                digits.clear();
                break;
            }
        }
    }
    if !digits.is_empty()
        && let Some(rows) = sorted_rows(&digits, frame.height())
    {
        return gather_frame(frame, &rows);
    }
    let options = SortMultipleOptions::default()
        .with_order_descending_multi(by.iter().map(|(_, descending)| *descending))
        .with_nulls_last(false)
        .with_maintain_order(true);
    Ok(frame.sort(by.iter().map(|(name, _)| *name), options)?)
}

/// One sort column coded as an integer per row whose order is the sort's: `NA` is 0 and comes
/// first, and the other codes fit in `bits` bits.
///
/// Coding the columns this way is what makes sorting fast: the codes of every sort column and
/// the row number pack into one `u64` or `u128`, and one parallel sort of those integers orders
/// the rows. A string column codes as its rank among the column's distinct strings, which
/// replaces string comparisons in the sort by one hash lookup per row.
struct Digit {
    codes: Vec<u64>,
    bits: u32,
}

/// The sort columns coded as [`Digit`]s, or `None` when a column has a type the coding does not
/// cover.
fn key_digits(frame: &DataFrame, names: &[String]) -> Result<Option<Vec<Digit>>> {
    let mut digits = Vec::with_capacity(names.len());
    for name in names {
        match order_digits(frame.column(name)?, false)? {
            Some(digit) => digits.push(digit),
            None => return Ok(None),
        }
    }
    Ok(Some(digits))
}

fn order_digits(column: &Column, descending: bool) -> Result<Option<Digit>> {
    if u32::try_from(column.len()).is_err() {
        return Ok(None);
    }
    let digit = |codes: Vec<u64>, top: u64| Digit {
        codes,
        bits: u64::BITS - top.leading_zeros(),
    };
    Ok(Some(match Vector::of(column) {
        Some(Vector::Integer { values, validity }) => {
            let (low, high) = valid_range(&values, validity.as_ref(), i64::from);
            let codes = fill_rows(values.len(), |row| {
                if !valid(validity.as_ref(), row) {
                    0
                } else if descending {
                    (high - i64::from(values[row])) as u64 + 1
                } else {
                    (i64::from(values[row]) - low) as u64 + 1
                }
            });
            digit(codes, (high - low).max(0) as u64 + 1)
        }
        Some(Vector::Double { values, validity }) => {
            // `NA` is 0 and `NaN` is 1, both before every number, as `setorder` places them.
            let number = |row: usize| valid(validity.as_ref(), row) && !values[row].is_nan();
            let (low, high) = THREAD_POOL.install(|| {
                (0..values.len())
                    .into_par_iter()
                    .with_min_len(CHUNK_ROWS)
                    .filter(|row| number(*row))
                    .map(|row| ordered_bits(values[row]))
                    .fold(
                        || (u64::MAX, 0u64),
                        |(low, high), bits| (low.min(bits), high.max(bits)),
                    )
                    .reduce(
                        || (u64::MAX, 0u64),
                        |left, right| (left.0.min(right.0), left.1.max(right.1)),
                    )
            });
            let Some(top) = high.saturating_sub(low).checked_add(2) else {
                return Ok(None);
            };
            let codes = fill_rows(values.len(), |row| {
                if !valid(validity.as_ref(), row) {
                    0
                } else if values[row].is_nan() {
                    1
                } else if descending {
                    high - ordered_bits(values[row]) + 2
                } else {
                    ordered_bits(values[row]) - low + 2
                }
            });
            digit(codes, top)
        }
        Some(Vector::Logical { values, validity }) => {
            let codes = fill_rows(values.len(), |row| {
                if !valid(validity.as_ref(), row) {
                    0
                } else {
                    1 + u64::from(values.get_bit(row) != descending)
                }
            });
            digit(codes, 2)
        }
        None if matches!(column.dtype(), DataType::Enum(..)) => {
            let (Some(codes), Some(levels)) = (Codes::of(column), factor_levels(column.dtype()))
            else {
                return Ok(None);
            };
            let levels = levels.len() as u64;
            let values = fill_rows(column.len(), |row| {
                if !valid(codes.validity.as_ref(), row) {
                    0
                } else if descending {
                    levels - u64::from(code_at(&codes.values, row))
                } else {
                    u64::from(code_at(&codes.values, row)) + 1
                }
            });
            digit(values, levels)
        }
        None if matches!(column.dtype(), DataType::String) => {
            let (mut codes, distinct) = string_ranks(column)?;
            if descending {
                THREAD_POOL.install(|| {
                    codes
                        .par_iter_mut()
                        .filter(|code| **code != 0)
                        .for_each(|code| *code = distinct + 1 - *code);
                });
            }
            digit(codes, distinct)
        }
        None => return Ok(None),
    }))
}

/// Each string's rank among the column's distinct strings in byte order, counting from 1, with
/// `NA` as 0, and how many distinct strings there are.
///
/// Each parallel run numbers the strings it meets in a hash map of its own, so the runs share
/// nothing while they scan. The distinct strings of all runs are then sorted once, and each run
/// translates its own numbers into ranks.
fn string_ranks(column: &Column) -> Result<(Vec<u64>, u64)> {
    let strings = column.as_materialized_series().str()?.rechunk();
    let Some(array) = strings.downcast_iter().next() else {
        return Ok((Vec::new(), 0));
    };
    let length = array.len();
    // Every run may meet every distinct string, so the sort and the translation below grow with
    // the number of runs: one per thread keeps them small.
    let runs = if length < PARALLEL_ROWS {
        1
    } else {
        THREAD_POOL.current_num_threads().max(1)
    };
    let run_length = length.div_ceil(runs).max(1);
    let numbered: Vec<(Vec<&str>, Vec<u32>)> = THREAD_POOL.install(|| {
        (0..length)
            .step_by(run_length)
            .collect::<Vec<_>>()
            .into_par_iter()
            .map(|start| {
                let mut inline: foldhash::HashMap<u128, u32> = foldhash::HashMap::default();
                let mut cache = vec![(0u128, u32::MAX); 1 << CACHE_BITS];
                let mut long: foldhash::HashMap<&str, u32> = foldhash::HashMap::default();
                let mut distinct: Vec<&str> = Vec::new();
                let views = array.views();
                let local = (start..(start + run_length).min(length))
                    .map(|row| {
                        if !array.is_valid(row) {
                            return u32::MAX;
                        }
                        let view = views[row];
                        // A string of up to 12 bytes lives inside its 16-byte view, so the view
                        // (with the bytes past the string masked off) identifies it without
                        // reading or hashing the string itself.
                        if view.length <= INLINE_BYTES {
                            let bits = view.as_u128() & inline_mask(view.length);
                            let cached = distinct.len() <= CACHED_DISTINCT;
                            let slot = &mut cache[cache_slot(bits)];
                            if cached && slot.0 == bits && slot.1 != u32::MAX {
                                return slot.1;
                            }
                            let number = match inline.get(&bits) {
                                Some(number) => *number,
                                None => {
                                    let number = distinct.len() as u32;
                                    distinct.push(array.value(row));
                                    inline.insert(bits, number);
                                    number
                                }
                            };
                            *slot = (bits, number);
                            number
                        } else {
                            let value = array.value(row);
                            *long.entry(value).or_insert_with(|| {
                                distinct.push(value);
                                (distinct.len() - 1) as u32
                            })
                        }
                    })
                    .collect();
                (distinct, local)
            })
            .collect()
    });
    // Ordering by the first bytes as an integer first makes almost every comparison an integer
    // one, and the integer order agrees with byte order because the zero padding of a short
    // string sorts before every byte a longer string can continue with.
    let mut sorted: Vec<(u128, &str)> = numbered
        .iter()
        .flat_map(|(distinct, _)| distinct.iter().map(|value| (prefix_key(value), *value)))
        .collect();
    THREAD_POOL.install(|| sorted.par_sort_unstable());
    sorted.dedup_by(|right, left| right.1 == left.1);
    let mut ranks = vec![0u64; length];
    THREAD_POOL.install(|| {
        ranks
            .par_chunks_mut(run_length)
            .zip(numbered.par_iter())
            .for_each(|(out, (distinct, local))| {
                let translation: Vec<u64> = distinct
                    .iter()
                    .map(|value| {
                        sorted.partition_point(|other| *other < (prefix_key(value), *value)) as u64
                            + 1
                    })
                    .collect();
                for (slot, number) in out.iter_mut().zip(local) {
                    *slot = translation.get(*number as usize).copied().unwrap_or(0);
                }
            });
    });
    Ok((ranks, sorted.len() as u64))
}

/// The string's first 16 bytes as a big-endian integer, zero-padded, so that integer order is
/// byte order up to ties between strings that share those bytes.
fn prefix_key(value: &str) -> u128 {
    let mut bytes = [0u8; 16];
    let length = value.len().min(16);
    bytes[..length].copy_from_slice(&value.as_bytes()[..length]);
    u128::from_be_bytes(bytes)
}

/// The cache entry for an inline view's bits: a multiplicative hash of both halves.
fn cache_slot(bits: u128) -> usize {
    let mixed = (bits as u64 ^ (bits >> 64) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed >> (u64::BITS - CACHE_BITS)) as usize
}

/// The mask that keeps a view's length and the first `length` bytes of its inline string.
fn inline_mask(length: u32) -> u128 {
    if length >= INLINE_BYTES {
        u128::MAX
    } else {
        (1u128 << (32 + 8 * length)) - 1
    }
}

/// The rows in the order the digits sort them, ties in row order, or `None` when the digits and
/// the row number need more than 128 bits.
fn sorted_rows(digits: &[Digit], height: usize) -> Option<Vec<u32>> {
    let bits: u32 = digits.iter().map(|digit| digit.bits).sum();
    if bits + ROW_BITS <= u64::BITS {
        Some(sort_packed::<u64>(digits, height))
    } else if bits + ROW_BITS <= u128::BITS {
        Some(sort_packed::<u128>(digits, height))
    } else {
        None
    }
}

/// The first row of each distinct combination of the digits, in row order, or `None` when the
/// digits need more than 96 bits.
fn first_rows(digits: &[Digit], height: usize) -> Option<Vec<u32>> {
    let bits: u32 = digits.iter().map(|digit| digit.bits).sum();
    if dense(bits, height) {
        let mut rows: Vec<u32> = dense_first_rows(digits, height, bits)
            .into_iter()
            .filter(|row| *row != u32::MAX)
            .collect();
        rows.sort_unstable();
        return Some(rows);
    }
    let sorted = sorted_rows(digits, height)?;
    let same = |left: u32, right: u32| {
        digits
            .iter()
            .all(|digit| digit.codes[left as usize] == digit.codes[right as usize])
    };
    let mut rows: Vec<u32> = sorted
        .iter()
        .enumerate()
        .filter(|(index, row)| *index == 0 || !same(sorted[index - 1], **row))
        .map(|(_, row)| *row)
        .collect();
    THREAD_POOL.install(|| rows.par_sort_unstable());
    Some(rows)
}

/// Whether a key of `bits` bits takes the dense path: it fits `DENSE_BITS`, and its table is not
/// far larger than the rows, which a short table with a sparse key would make it.
fn dense(bits: u32, height: usize) -> bool {
    bits <= DENSE_BITS && 1usize << bits <= height.max(1 << 10).saturating_mul(4)
}

/// For each packed combination of the digits, which fit in `bits` bits, the first row that holds
/// it, or `u32::MAX` when none does.
///
/// Each parallel task records first rows in a table of its own, and the tables merge by taking
/// the smallest row. Past `PARALLEL_DENSE_BITS` the per-task tables would cost more memory than
/// the parallel scan saves, so one task scans every row.
fn dense_first_rows(digits: &[Digit], height: usize, bits: u32) -> Vec<u32> {
    let keys: Vec<u64> = packed_keys(digits, height, false);
    let first_rows_of = |start: usize, slice: &[u64]| {
        let mut first = vec![u32::MAX; 1 << bits];
        for (offset, key) in slice.iter().enumerate() {
            let slot = &mut first[*key as usize];
            if *slot == u32::MAX {
                *slot = (start + offset) as u32;
            }
        }
        first
    };
    if bits > PARALLEL_DENSE_BITS || height < PARALLEL_ROWS {
        return first_rows_of(0, &keys);
    }
    let task_length = height.div_ceil(THREAD_POOL.current_num_threads().max(1));
    THREAD_POOL.install(|| {
        keys.par_chunks(task_length)
            .enumerate()
            .map(|(task, slice)| first_rows_of(task * task_length, slice))
            .reduce_with(|mut left, right| {
                for (slot, row) in left.iter_mut().zip(right) {
                    *slot = (*slot).min(row);
                }
                left
            })
            .unwrap_or_default()
    })
}

/// How many distinct combinations of the digits the rows hold.
fn distinct_count(digits: &[Digit], height: usize) -> usize {
    let bits: u32 = digits.iter().map(|digit| digit.bits).sum();
    if dense(bits, height) {
        // One shared bit per combination. A task reads a word before it sets a bit, so the words
        // of common combinations stay shared in every core's cache instead of bouncing.
        let words: Vec<AtomicU64> = (0..(1usize << bits).div_ceil(64))
            .map(|_| AtomicU64::new(0))
            .collect();
        THREAD_POOL.install(|| {
            (0..height)
                .into_par_iter()
                .with_min_len(CHUNK_ROWS)
                .for_each(|row| {
                    let key = digits
                        .iter()
                        .fold(0u64, |key, digit| (key << digit.bits) | digit.codes[row]);
                    let word = &words[(key >> 6) as usize];
                    let bit = 1u64 << (key & 63);
                    if word.load(Ordering::Relaxed) & bit == 0 {
                        word.fetch_or(bit, Ordering::Relaxed);
                    }
                })
        });
        return words
            .iter()
            .map(|word| word.load(Ordering::Relaxed).count_ones() as usize)
            .sum();
    }
    fn count_sorted<T: Packed>(digits: &[Digit], height: usize) -> usize {
        let mut keys: Vec<T> = packed_keys(digits, height, false);
        THREAD_POOL.install(|| keys.par_sort_unstable());
        keys.windows(2).filter(|pair| pair[0] != pair[1]).count() + usize::from(height > 0)
    }
    if bits <= u64::BITS {
        count_sorted::<u64>(digits, height)
    } else if bits <= u128::BITS {
        count_sorted::<u128>(digits, height)
    } else {
        let mut keys: Vec<Vec<u64>> = (0..height)
            .map(|row| digits.iter().map(|digit| digit.codes[row]).collect())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys.len()
    }
}

/// An unsigned integer wide enough to hold the packed digits of a row.
trait Packed: Copy + Ord + Send + Sync {
    const ZERO: Self;
    fn push(self, code: u64, bits: u32) -> Self;
    fn low_row(self) -> u32;
    /// The bits of the key from `shift` on, masked by `mask`.
    fn digit(self, shift: u32, mask: u64) -> usize;
}

impl Packed for u64 {
    const ZERO: Self = 0;
    fn push(self, code: u64, bits: u32) -> Self {
        self.checked_shl(bits).unwrap_or(0) | code
    }
    fn low_row(self) -> u32 {
        self as u32
    }
    fn digit(self, shift: u32, mask: u64) -> usize {
        ((self >> shift) & mask) as usize
    }
}

impl Packed for u128 {
    const ZERO: Self = 0;
    fn push(self, code: u64, bits: u32) -> Self {
        self.checked_shl(bits).unwrap_or(0) | u128::from(code)
    }
    fn low_row(self) -> u32 {
        self as u32
    }
    fn digit(self, shift: u32, mask: u64) -> usize {
        ((self >> shift) as u64 & mask) as usize
    }
}

/// Each row's digits packed into one integer, the first digit highest, followed by the row
/// number when `with_row`, so that sorting the integers sorts the rows stably.
fn packed_keys<T: Packed>(digits: &[Digit], height: usize, with_row: bool) -> Vec<T> {
    fill_rows(height, |row| {
        let mut key = T::ZERO;
        for digit in digits {
            key = key.push(digit.codes[row], digit.bits);
        }
        if with_row {
            key = key.push(row as u64, ROW_BITS);
        }
        key
    })
}

fn sort_packed<T: Packed>(digits: &[Digit], height: usize) -> Vec<u32> {
    let keys: Vec<T> = packed_keys(digits, height, true);
    let bits: u32 = digits.iter().map(|digit| digit.bits).sum();
    let keys = radix_sort(keys, ROW_BITS, ROW_BITS + bits);
    fill_rows(height, |index| keys[index].low_row())
}

/// Sorts the keys by their bits from `low` up to `high`, keeping keys that tie on those bits in
/// their order. The bits below `low` must be the row number, which the keys hold in ascending
/// order, and no bit at or above `high` may be set.
///
/// Narrow keys take least-significant-digit radix passes, each a stable `counting_pass`. The row
/// number sits below `low`, so the sort never spends a pass on it, and a pass whose digit is the
/// same for every key is skipped.
fn radix_sort<T: Packed>(mut keys: Vec<T>, low: u32, high: u32) -> Vec<T> {
    let total = high - low;
    if total == 0 || keys.len() < 2 {
        return keys;
    }
    let passes = if total <= ONE_PASS_BITS {
        1
    } else {
        total.div_ceil(RADIX_BITS)
    };
    let mut spare = vec![T::ZERO; keys.len()];
    if passes > MAX_LSD_PASSES {
        // Wide keys would cost a pass over every key per digit. One pass on the top bits instead
        // splits the keys into buckets small enough to sort in cache, and each bucket sorts
        // independently. The row number below `low` makes every key distinct, so an unstable
        // sort of a bucket still keeps tied rows in row order.
        let Some(ends) = counting_pass(&keys, &mut spare, high - MSD_BITS, MSD_BITS) else {
            THREAD_POOL.install(|| keys.par_sort_unstable());
            return keys;
        };
        let mut buckets = Vec::with_capacity(ends.len());
        let mut rest = spare.as_mut_slice();
        let mut start = 0;
        for end in ends {
            let (bucket, tail) = rest.split_at_mut(end - start);
            buckets.push(bucket);
            rest = tail;
            start = end;
        }
        THREAD_POOL.install(|| {
            buckets.into_par_iter().for_each(|bucket| {
                if bucket.len() < PARALLEL_ROWS {
                    bucket.sort_unstable();
                } else {
                    bucket.par_sort_unstable();
                }
            })
        });
        return spare;
    }
    let width = total.div_ceil(passes);
    for pass in 0..passes {
        let shift = low + pass * width;
        let digit_bits = width.min(high - shift);
        if counting_pass(&keys, &mut spare, shift, digit_bits).is_some() {
            swap(&mut keys, &mut spare);
        }
    }
    keys
}

/// One parallel stable counting sort of `keys` into `out` by the `bits` bits from `shift` on,
/// returning where each digit's run ends. When every key has the same digit it writes nothing
/// and returns `None`, since the pass would not change the order.
///
/// Every task counts the digits of its slice, the counts give each task a disjoint run of output
/// positions per digit, and the tasks then scatter their keys into those runs.
fn counting_pass<T: Packed>(
    keys: &[T],
    out: &mut [T],
    shift: u32,
    bits: u32,
) -> Option<Vec<usize>> {
    debug_assert_eq!(out.len(), keys.len());
    let length = keys.len();
    let buckets = 1usize << bits;
    // Each task keeps a count and a start per digit, so a wide digit gets fewer, longer tasks:
    // with 16 bits a task's tables are a megabyte, and a task shorter than its table spends
    // more time on the table than on its keys.
    let tasks = if length < PARALLEL_ROWS {
        1
    } else {
        let per_thread = if bits > 11 { 1 } else { 4 };
        (THREAD_POOL.current_num_threads().max(1) * per_thread).min((length / buckets).max(1))
    };
    let task_length = length.div_ceil(tasks).max(1);
    let mask = (buckets - 1) as u64;
    let counts: Vec<Vec<u32>> = THREAD_POOL.install(|| {
        keys.par_chunks(task_length)
            .map(|slice| {
                let mut counts = vec![0u32; buckets];
                for key in slice {
                    counts[key.digit(shift, mask)] += 1;
                }
                counts
            })
            .collect()
    });
    let mut starts = vec![0usize; counts.len() * buckets];
    let mut ends = Vec::with_capacity(buckets);
    let mut position = 0usize;
    for bucket in 0..buckets {
        for (task, counts) in counts.iter().enumerate() {
            starts[task * buckets + bucket] = position;
            position += counts[bucket] as usize;
        }
        ends.push(position);
    }
    let mut previous = 0;
    let uniform = ends.iter().any(|end| {
        let full = end - previous == length;
        previous = *end;
        full
    });
    if uniform {
        return None;
    }
    let target = Shared(out.as_mut_ptr());
    THREAD_POOL.install(|| {
        keys.par_chunks(task_length)
            .zip(starts.par_chunks_mut(buckets))
            .for_each(|(slice, next)| {
                for key in slice {
                    let slot = &mut next[key.digit(shift, mask)];
                    // SAFETY: the counts reserve each task a run of positions per digit that no
                    // other task's run overlaps, all of them inside `out`, which is as long as
                    // `keys`, and the task writes each position once.
                    unsafe { target.write(*slot, *key) };
                    *slot += 1;
                }
            });
    });
    Some(ends)
}

fn gather_frame(frame: &DataFrame, rows: &[u32]) -> Result<DataFrame> {
    let columns: Vec<Column> = THREAD_POOL.install(|| {
        frame
            .columns()
            .par_iter()
            .map(|column| gather_column(column, rows, None, column.name().clone()))
            .collect::<Result<_>>()
    })?;
    Ok(DataFrame::new(rows.len(), columns)?)
}

fn valid(validity: Option<&Bitmap>, row: usize) -> bool {
    validity.is_none_or(|validity| validity.get_bit(row))
}

fn valid_range<T: Copy + Sync>(
    values: &[T],
    validity: Option<&Bitmap>,
    widen: impl Fn(T) -> i64 + Sync,
) -> (i64, i64) {
    let (low, high) = THREAD_POOL.install(|| {
        (0..values.len())
            .into_par_iter()
            .with_min_len(CHUNK_ROWS)
            .filter(|row| valid(validity, *row))
            .map(|row| widen(values[row]))
            .fold(
                || (i64::MAX, i64::MIN),
                |(low, high), value| (low.min(value), high.max(value)),
            )
            .reduce(
                || (i64::MAX, i64::MIN),
                |left, right| (left.0.min(right.0), left.1.max(right.1)),
            )
    });
    if low > high { (0, 0) } else { (low, high) }
}

/// A double's bits rearranged so that unsigned order is numeric order, with `-0.0` equal to
/// `0.0`.
fn ordered_bits(value: f64) -> u64 {
    let bits = (value + 0.0).to_bits();
    if bits >> 63 == 1 {
        !bits
    } else {
        bits | (1 << 63)
    }
}

fn fill_rows<R: Send>(length: usize, value: impl Fn(usize) -> R + Sync) -> Vec<R> {
    fill(length, |start, out| {
        for (offset, slot) in out.iter_mut().enumerate() {
            slot.write(value(start + offset));
        }
    })
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
