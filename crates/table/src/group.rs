//! Grouped aggregation kernels for `tbl[, j, by]`.
//!
//! Polars' `group_by_stable` hashes every key column and then sorts the groups back into order
//! of first appearance, which is what made multi-key and high-cardinality groupings slower than
//! data.table. These kernels code the grouping columns into one integer per row instead: each
//! column is a digit of a mixed-radix number, with `NA` as digit zero, a factor's level `l` as
//! `l + 1` and an integer `v` as `v - min + 1`. Comparing codes compares the keys in `keyby`
//! order (`NA` first), so a sorted result only sorts the groups.
//!
//! When the codes span at most [`DENSE_CODES`] values, they index the group states directly, and
//! each thread accumulates a contiguous run of rows that is merged in row order afterwards. Wider
//! codes are hash partitioned: every row goes to one of up to 256 partitions by a hash of its
//! code, keeping row order within a partition, and each partition then numbers its groups with
//! an open-addressing table and accumulates them on its own thread. Either way a group's first
//! row is known, which gives `by` its order of first appearance and the grouping columns their
//! values.
//!
//! Only GForce-style aggregates are covered (`sum`, `mean`, `min`, `max`, `first`, `last`,
//! `length` and `.N`, with arithmetic on top of them), plus the `x[order(v), head(v, k), by]`
//! top-k shape. Anything else returns `None`, and the query runs as a Polars plan.

use polars::prelude::{Column, DataFrame, PlSmallStr};
use polars_arrow::bitmap::Bitmap;
use polars_buffer::Buffer;
use polars_core::runtime::THREAD_POOL;
use rayon::prelude::*;

use crate::kernel::{
    CodeValues, Codes, Evaluator, PARALLEL_ROWS, Shared, Value, Vector, fill, gather_column,
};
use crate::query::{Aggregate, Expr, Query, Rows, Select, SortKey};
use crate::{ColumnType, Result, Schema, Table};

/// The widest code range whose group states are indexed by code directly. Each thread holds one
/// state per possible code, so this bounds that memory.
const DENSE_CODES: u64 = 1 << 20;
/// Marks "no row" in a row slot.
const NO_ROW: u32 = u32::MAX;
/// The longest `head(x, k)` the top-k kernel keeps per group.
const MAX_TOP: usize = 8;

/// Runs a grouped query with the kernels, or gives `None` when its shape is outside what they
/// cover. The frame is the query's result, before [`narrow_sums`](crate::query) (which leaves it
/// unchanged, because the kernels already narrow integer sums).
pub(crate) fn aggregate(
    table: &Table,
    schema: &Schema,
    query: &Query,
) -> Result<Option<DataFrame>> {
    let frame = table.frame();
    let height = frame.height();
    if query.by.is_empty() || height == 0 || u32::try_from(height).is_err() {
        return Ok(None);
    }
    let columns: Vec<(PlSmallStr, &Expr)> = match &query.select {
        Select::Columns(columns) => columns
            .iter()
            .map(|(name, expr)| (name.clone(), expr))
            .collect(),
        Select::Value(expr) => vec![(PlSmallStr::from_static("V1"), expr)],
        Select::All | Select::Assign(_) => return Ok(None),
    };
    let mut names: Vec<&str> = query.by.iter().map(|(name, _)| name.as_str()).collect();
    names.extend(columns.iter().map(|(name, _)| name.as_str()));
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != names.len() {
        return Ok(None);
    }
    let order = match &query.rows {
        Rows::All => None,
        Rows::Order(keys) => match top_k_shape(keys, &columns) {
            Some(shape) => Some(shape),
            None => return Ok(None),
        },
        Rows::Filter(_) | Rows::Positions(_) | Rows::Join(_) => return Ok(None),
    };
    let Some(keys) = group_keys(frame, schema, &query.by)? else {
        return Ok(None);
    };
    let evaluator = Evaluator {
        frame,
        schema,
        rows: None,
    };
    if let Some((sort, top)) = order {
        return top_k(
            frame,
            &evaluator,
            &keys,
            &query.by,
            query.keyed,
            sort,
            top,
            &columns,
        );
    }
    // Every aggregate, deduplicated, and each output column as arithmetic over them.
    let mut aggregates: Vec<Expr> = Vec::new();
    let mut outputs = Vec::with_capacity(columns.len());
    for (name, expr) in &columns {
        let Some(rewritten) = rewrite(expr, &mut aggregates) else {
            return Ok(None);
        };
        outputs.push((name.clone(), rewritten));
    }
    let mut aggregations = Vec::with_capacity(aggregates.len());
    for expr in &aggregates {
        let Some(aggregation) = Aggregation::of(expr, &evaluator, height) else {
            return Ok(None);
        };
        aggregations.push(aggregation);
    }
    let codes = row_codes(&keys, height);
    let grouping = group(&codes, keys.radix, &aggregations, None);
    let order = output_order(&grouping, query.keyed, height);
    let first_rows: Vec<u32> = order
        .iter()
        .map(|slot| grouping.first_rows[*slot as usize])
        .collect();

    let groups = order.len();
    let mut result = key_columns(frame, &query.by, &first_rows)?;
    let mut temporaries = Vec::with_capacity(aggregations.len());
    let mut temporary_schema = Vec::with_capacity(aggregations.len());
    for (index, (aggregation, state)) in aggregations.iter().zip(&grouping.states).enumerate() {
        let name = temporary(index);
        let vector = finish(aggregation, state, &order);
        temporary_schema.push((name.clone(), vector.kind()));
        temporaries.push(vector.into_column(name));
    }
    let temporaries = DataFrame::new(groups, temporaries)?;
    let temporary_schema = Schema {
        columns: temporary_schema,
    };
    let evaluator = Evaluator {
        frame: &temporaries,
        schema: &temporary_schema,
        rows: None,
    };
    for (name, expr) in outputs {
        let Some(vector) = evaluator
            .eval(&expr)
            .and_then(|value| value.into_vector(groups))
        else {
            return Ok(None);
        };
        result.push(vector.into_column(name));
    }
    Ok(Some(DataFrame::new(groups, result)?))
}

/// The grouping columns, each a digit of the row code.
struct Keys {
    digits: Vec<Digit>,
    /// The number of possible codes: the product of the digits' radixes.
    radix: u64,
}

struct Digit {
    source: DigitSource,
    radix: u64,
}

enum DigitSource {
    Factor(Codes),
    Integer {
        values: Buffer<i32>,
        validity: Option<Bitmap>,
        min: i32,
    },
    Logical {
        values: Bitmap,
        validity: Option<Bitmap>,
    },
}

/// The grouping columns as digits, or `None` when a group is computed, is a double or a string,
/// or the codes would not fit in 64 bits.
fn group_keys(
    frame: &DataFrame,
    schema: &Schema,
    by: &[(PlSmallStr, Expr)],
) -> Result<Option<Keys>> {
    let mut digits = Vec::with_capacity(by.len());
    let mut radix: u64 = 1;
    for (_, expr) in by {
        let Expr::Column(name) = expr else {
            return Ok(None);
        };
        let Ok(column) = frame.column(name) else {
            return Ok(None);
        };
        let digit = match schema.column(name) {
            Some(ColumnType::Factor) => {
                let Some(levels) = crate::convert::factor_levels(column.dtype()) else {
                    return Ok(None);
                };
                let Some(codes) = Codes::of(column) else {
                    return Ok(None);
                };
                Digit {
                    source: DigitSource::Factor(codes),
                    radix: levels.len() as u64 + 1,
                }
            }
            Some(ColumnType::Integer) => {
                let Some(Vector::Integer { values, validity }) = Vector::of(column) else {
                    return Ok(None);
                };
                let chunked = column.as_materialized_series().i32()?;
                let (min, max) = match (
                    polars::prelude::ChunkAgg::min(chunked),
                    polars::prelude::ChunkAgg::max(chunked),
                ) {
                    (Some(min), Some(max)) => (min, max),
                    _ => (0, 0),
                };
                Digit {
                    source: DigitSource::Integer {
                        values,
                        validity,
                        min,
                    },
                    radix: (i64::from(max) - i64::from(min) + 2) as u64,
                }
            }
            Some(ColumnType::Logical) => {
                let Some(Vector::Logical { values, validity }) = Vector::of(column) else {
                    return Ok(None);
                };
                Digit {
                    source: DigitSource::Logical { values, validity },
                    radix: 3,
                }
            }
            _ => return Ok(None),
        };
        let Some(product) = radix.checked_mul(digit.radix) else {
            return Ok(None);
        };
        radix = product;
        digits.push(digit);
    }
    // `u64::MAX` marks an empty hash slot, so no code may reach it.
    if radix == u64::MAX {
        return Ok(None);
    }
    Ok(Some(Keys { digits, radix }))
}

/// Each row's code, the first grouping column being the most significant digit.
fn row_codes(keys: &Keys, height: usize) -> Vec<u64> {
    fill(height, |start, out| {
        for slot in out.iter_mut() {
            slot.write(0);
        }
        // SAFETY: every element was initialized above.
        let out: &mut [u64] = unsafe { &mut *(out as *mut [_] as *mut [u64]) };
        for digit in &keys.digits {
            let radix = digit.radix;
            match &digit.source {
                DigitSource::Factor(codes) => match &codes.values {
                    CodeValues::U8(values) => {
                        append_digits(out, start, radix, &codes.validity, |row| {
                            u64::from(values[row]) + 1
                        })
                    }
                    CodeValues::U16(values) => {
                        append_digits(out, start, radix, &codes.validity, |row| {
                            u64::from(values[row]) + 1
                        })
                    }
                    CodeValues::U32(values) => {
                        append_digits(out, start, radix, &codes.validity, |row| {
                            u64::from(values[row]) + 1
                        })
                    }
                },
                DigitSource::Integer {
                    values,
                    validity,
                    min,
                } => {
                    let base = i64::from(*min) - 1;
                    append_digits(out, start, radix, validity, |row| {
                        (i64::from(values[row]) - base) as u64
                    });
                }
                DigitSource::Logical { values, validity } => {
                    append_digits(out, start, radix, validity, |row| {
                        u64::from(values.get_bit(row)) + 1
                    });
                }
            }
        }
    })
}

/// Appends one digit to the codes of the rows from `start` on. A missing value is digit zero,
/// and its value is never read, since the value behind an `NA` is unspecified.
fn append_digits(
    codes: &mut [u64],
    start: usize,
    radix: u64,
    validity: &Option<Bitmap>,
    digit: impl Fn(usize) -> u64,
) {
    match validity {
        None => {
            for (offset, code) in codes.iter_mut().enumerate() {
                *code = *code * radix + digit(start + offset);
            }
        }
        Some(validity) => {
            for (offset, code) in codes.iter_mut().enumerate() {
                let row = start + offset;
                let value = if validity.get_bit(row) { digit(row) } else { 0 };
                *code = *code * radix + value;
            }
        }
    }
}

/// The groups of a query, numbered by slot, with every aggregation's state.
struct Grouping {
    /// Each slot's first row. In the dense layout a slot is a code, and an unused one has
    /// [`NO_ROW`].
    first_rows: Vec<u32>,
    /// Each slot's code, in the hashed layout. Empty in the dense layout, where the slot is the
    /// code.
    codes: Vec<u64>,
    states: Vec<State>,
}

/// Numbers the groups and accumulates every aggregation. `top` is the top-k ranking, when the
/// query keeps the best rows of each group instead of aggregating.
fn group(
    codes: &[u64],
    radix: u64,
    aggregations: &[Aggregation],
    top: Option<&Ranking>,
) -> Grouping {
    if radix <= DENSE_CODES {
        dense(codes, radix as usize, aggregations, top)
    } else {
        hashed(codes, aggregations, top)
    }
}

fn dense(
    codes: &[u64],
    radix: usize,
    aggregations: &[Aggregation],
    top: Option<&Ranking>,
) -> Grouping {
    let height = codes.len();
    let run = |start: usize, end: usize| {
        let mut first_rows = vec![NO_ROW; radix];
        for (offset, code) in codes[start..end].iter().enumerate() {
            let slot = &mut first_rows[*code as usize];
            if *slot == NO_ROW {
                *slot = (start + offset) as u32;
            }
        }
        let mut states: Vec<State> = aggregations
            .iter()
            .map(|aggregation| State::new(aggregation, radix, top))
            .collect();
        for (aggregation, state) in aggregations.iter().zip(&mut states) {
            update(
                state,
                aggregation,
                top,
                end - start,
                |index| start + index,
                |index| codes[start + index] as usize,
            );
        }
        (first_rows, states)
    };
    let threads = THREAD_POOL.current_num_threads().max(1);
    if height < PARALLEL_ROWS || threads == 1 {
        let (first_rows, states) = run(0, height);
        return Grouping {
            first_rows,
            codes: Vec::new(),
            states,
        };
    }
    let step = height.div_ceil(threads);
    let parts: Vec<_> = THREAD_POOL.install(|| {
        (0..threads)
            .into_par_iter()
            .map(|thread| {
                run(
                    (thread * step).min(height),
                    ((thread + 1) * step).min(height),
                )
            })
            .collect()
    });
    let mut parts = parts.into_iter();
    let Some((mut first_rows, mut states)) = parts.next() else {
        return Grouping {
            first_rows: Vec::new(),
            codes: Vec::new(),
            states: Vec::new(),
        };
    };
    // Later runs cover later rows, so merging them in order keeps "first" and "last" right.
    for (later_rows, later_states) in parts {
        for (row, later) in first_rows.iter_mut().zip(later_rows) {
            if *row == NO_ROW {
                *row = later;
            }
        }
        for (state, later) in states.iter_mut().zip(later_states) {
            state.merge(later, top);
        }
    }
    Grouping {
        first_rows,
        codes: Vec::new(),
        states,
    }
}

fn hash(code: u64) -> u64 {
    // splitmix64's finalizer: cheap, and every input bit affects every output bit.
    let mut value = code.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn hashed(codes: &[u64], aggregations: &[Aggregation], top: Option<&Ranking>) -> Grouping {
    let height = codes.len();
    let threads = THREAD_POOL.current_num_threads().max(1);
    let bits = if height < PARALLEL_ROWS {
        0
    } else {
        (threads * 16).next_power_of_two().trailing_zeros().min(8)
    };
    let partitions = 1usize << bits;
    let partition_of = |code: u64| {
        if bits == 0 {
            0
        } else {
            (hash(code) >> (64 - bits)) as usize
        }
    };
    let chunks = if height < PARALLEL_ROWS {
        1
    } else {
        threads * 4
    };
    let step = height.div_ceil(chunks);
    let range = |chunk: usize| (chunk * step).min(height)..((chunk + 1) * step).min(height);

    // Count each chunk's rows per partition, then lay partitions out one after another, each
    // holding its chunks in order, so a partition lists its rows in ascending order.
    let counts: Vec<Vec<u32>> = THREAD_POOL.install(|| {
        (0..chunks)
            .into_par_iter()
            .map(|chunk| {
                let mut counts = vec![0u32; partitions];
                for code in &codes[range(chunk)] {
                    counts[partition_of(*code)] += 1;
                }
                counts
            })
            .collect()
    });
    let mut offsets = vec![vec![0usize; partitions]; chunks];
    let mut bounds = Vec::with_capacity(partitions + 1);
    let mut total = 0usize;
    for partition in 0..partitions {
        bounds.push(total);
        for chunk in 0..chunks {
            offsets[chunk][partition] = total;
            total += counts[chunk][partition] as usize;
        }
    }
    bounds.push(total);
    let mut rows: Vec<u32> = Vec::with_capacity(height);
    let mut row_codes: Vec<u64> = Vec::with_capacity(height);
    let rows_out = Shared(rows.as_mut_ptr());
    let codes_out = Shared(row_codes.as_mut_ptr());
    THREAD_POOL.install(|| {
        offsets
            .into_par_iter()
            .enumerate()
            .for_each(|(chunk, mut cursor)| {
                for row in range(chunk) {
                    let code = codes[row];
                    let partition = partition_of(code);
                    let position = cursor[partition];
                    cursor[partition] += 1;
                    // SAFETY: the offsets give every (chunk, partition) pair its own range of
                    // positions, sized by the counts, so every position below `height` is written
                    // exactly once.
                    unsafe {
                        rows_out.write(position, row as u32);
                        codes_out.write(position, code);
                    }
                }
            });
    });
    // SAFETY: the scatter above wrote every position in `0..height`.
    unsafe {
        rows.set_len(height);
        row_codes.set_len(height);
    }

    let parts: Vec<(Vec<u32>, Vec<u64>, Vec<State>)> = THREAD_POOL.install(|| {
        (0..partitions)
            .into_par_iter()
            .map(|partition| {
                let span = bounds[partition]..bounds[partition + 1];
                let rows = &rows[span.clone()];
                let codes = &row_codes[span];
                let (slots, first_rows, slot_codes) = number_groups(codes, rows);
                let mut states: Vec<State> = aggregations
                    .iter()
                    .map(|aggregation| State::new(aggregation, first_rows.len(), top))
                    .collect();
                for (aggregation, state) in aggregations.iter().zip(&mut states) {
                    update(
                        state,
                        aggregation,
                        top,
                        rows.len(),
                        |index| rows[index] as usize,
                        |index| slots[index] as usize,
                    );
                }
                (first_rows, slot_codes, states)
            })
            .collect()
    });
    let groups: usize = parts
        .iter()
        .map(|(first_rows, _, _)| first_rows.len())
        .sum();
    let mut first_rows = Vec::with_capacity(groups);
    let mut slot_codes = Vec::with_capacity(groups);
    let mut states: Vec<State> = aggregations
        .iter()
        .map(|aggregation| State::new(aggregation, 0, top))
        .collect();
    for (part_rows, part_codes, part_states) in parts {
        first_rows.extend(part_rows);
        slot_codes.extend(part_codes);
        for (state, part) in states.iter_mut().zip(part_states) {
            state.append(part);
        }
    }
    Grouping {
        first_rows,
        codes: slot_codes,
        states,
    }
}

/// Numbers the distinct codes of one partition in order of first appearance: each row's slot,
/// and each slot's first row and code.
fn number_groups(codes: &[u64], rows: &[u32]) -> (Vec<u32>, Vec<u32>, Vec<u64>) {
    const EMPTY: u64 = u64::MAX;
    let capacity = (codes.len() * 2).next_power_of_two().max(16);
    let mask = capacity - 1;
    let mut table: Vec<(u64, u32)> = vec![(EMPTY, 0); capacity];
    let mut slots = Vec::with_capacity(codes.len());
    let mut first_rows = Vec::new();
    let mut slot_codes = Vec::new();
    for (code, row) in codes.iter().zip(rows) {
        let mut position = hash(*code) as usize & mask;
        let slot = loop {
            let entry = &mut table[position];
            if entry.0 == *code {
                break entry.1;
            }
            if entry.0 == EMPTY {
                let slot = first_rows.len() as u32;
                *entry = (*code, slot);
                first_rows.push(*row);
                slot_codes.push(*code);
                break slot;
            }
            position = (position + 1) & mask;
        };
        slots.push(slot);
    }
    (slots, first_rows, slot_codes)
}

/// The slots in output order: by code for `keyby`, by first row for `by`.
fn output_order(grouping: &Grouping, keyed: bool, height: usize) -> Vec<u32> {
    if grouping.codes.is_empty() {
        // Dense: the slot is the code, so ascending slots are sorted keys.
        let mut present: Vec<u32> = (0..grouping.first_rows.len() as u32)
            .filter(|slot| grouping.first_rows[*slot as usize] != NO_ROW)
            .collect();
        if !keyed {
            sort_by_key(&mut present, |slot| grouping.first_rows[*slot as usize]);
        }
        return present;
    }
    let groups = grouping.first_rows.len();
    if keyed {
        let mut slots: Vec<u32> = (0..groups as u32).collect();
        sort_by_key(&mut slots, |slot| grouping.codes[*slot as usize]);
        return slots;
    }
    // Order by first row without sorting: mark each group at its first row, then read the marks
    // in row order.
    let mut marks = vec![NO_ROW; height];
    let marks_out = Shared(marks.as_mut_ptr());
    let first_rows = &grouping.first_rows;
    let write = |slots: std::ops::Range<usize>| {
        for slot in slots {
            // SAFETY: groups have distinct first rows, each below `height`.
            unsafe { marks_out.write(first_rows[slot] as usize, slot as u32) };
        }
    };
    if groups < PARALLEL_ROWS {
        write(0..groups);
    } else {
        THREAD_POOL.install(|| {
            (0..groups.div_ceil(PARALLEL_ROWS))
                .into_par_iter()
                .for_each(|chunk| {
                    write(chunk * PARALLEL_ROWS..((chunk + 1) * PARALLEL_ROWS).min(groups));
                });
        });
    }
    let parts: Vec<Vec<u32>> = THREAD_POOL.install(|| {
        marks
            .par_chunks(PARALLEL_ROWS)
            .map(|chunk| {
                chunk
                    .iter()
                    .copied()
                    .filter(|slot| *slot != NO_ROW)
                    .collect()
            })
            .collect()
    });
    let mut order = Vec::with_capacity(groups);
    for part in parts {
        order.extend(part);
    }
    order
}

fn sort_by_key<K: Ord + Send>(slots: &mut [u32], key: impl Fn(&u32) -> K + Sync + Send) {
    if slots.len() < PARALLEL_ROWS {
        slots.sort_unstable_by_key(key);
    } else {
        THREAD_POOL.install(|| slots.par_sort_unstable_by_key(key));
    }
}

/// The grouping columns' values at the groups' first rows.
fn key_columns(
    frame: &DataFrame,
    by: &[(PlSmallStr, Expr)],
    first_rows: &[u32],
) -> Result<Vec<Column>> {
    let mut columns = Vec::with_capacity(by.len());
    for (name, expr) in by {
        let Expr::Column(source) = expr else {
            continue;
        };
        columns.push(gather_column(frame.column(source)?, first_rows, None, name.clone())?);
    }
    Ok(columns)
}

pub(crate) fn temporary(index: usize) -> PlSmallStr {
    PlSmallStr::from(format!("\u{1}{index}"))
}

/// The expression with each aggregate replaced by a reference to its temporary column, or `None`
/// when part of it does not collapse to one value per group.
pub(crate) fn rewrite(expr: &Expr, aggregates: &mut Vec<Expr>) -> Option<Expr> {
    Some(match expr {
        Expr::Aggregate { .. } | Expr::Count => {
            let index = match aggregates.iter().position(|known| known == expr) {
                Some(index) => index,
                None => {
                    aggregates.push(expr.clone());
                    aggregates.len() - 1
                }
            };
            Expr::Column(temporary(index))
        }
        Expr::Integer(_) | Expr::Double(_) | Expr::Logical(_) | Expr::Na(_) => expr.clone(),
        Expr::Character(_) | Expr::Column(_) | Expr::Head(..) | Expr::Correlation(..) => {
            return None;
        }
        Expr::Unary(op, arg) => Expr::Unary(*op, Box::new(rewrite(arg, aggregates)?)),
        Expr::Binary(op, left, right) => Expr::Binary(
            *op,
            Box::new(rewrite(left, aggregates)?),
            Box::new(rewrite(right, aggregates)?),
        ),
        Expr::IfElse(test, yes, no) => Expr::IfElse(
            Box::new(rewrite(test, aggregates)?),
            Box::new(rewrite(yes, aggregates)?),
            Box::new(rewrite(no, aggregates)?),
        ),
    })
}

/// One aggregate to compute per group.
struct Aggregation {
    function: Function,
    input: Input,
    na_rm: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Function {
    Sum,
    Mean,
    Min,
    Max,
    First,
    Last,
    Count,
    /// The best rows of each group under a [`Ranking`].
    Top,
}

enum Input {
    None,
    Integer {
        values: Buffer<i32>,
        validity: Option<Bitmap>,
    },
    Double {
        values: Buffer<f64>,
        validity: Option<Bitmap>,
    },
    /// A column whose values are only gathered (`first`, `last` and `head`), never combined.
    Gathered(Vector),
}

impl Aggregation {
    fn of(expr: &Expr, evaluator: &Evaluator, height: usize) -> Option<Aggregation> {
        let (function, arg, na_rm) = match expr {
            Expr::Count => {
                return Some(Aggregation {
                    function: Function::Count,
                    input: Input::None,
                    na_rm: false,
                });
            }
            Expr::Aggregate {
                function,
                arg,
                na_rm,
            } => (*function, arg, *na_rm),
            _ => return None,
        };
        let function = match function {
            Aggregate::Sum => Function::Sum,
            Aggregate::Mean => Function::Mean,
            Aggregate::Min => Function::Min,
            Aggregate::Max => Function::Max,
            Aggregate::First => Function::First,
            Aggregate::Last => Function::Last,
            Aggregate::Length => Function::Count,
            Aggregate::Median | Aggregate::Sd | Aggregate::Var => return None,
        };
        if function == Function::Count {
            // `length(x)` still needs `x` to be a valid expression.
            evaluator.eval(arg)?;
            return Some(Aggregation {
                function,
                input: Input::None,
                na_rm: false,
            });
        }
        let vector = evaluator.eval(arg)?.into_vector(height)?;
        let input = match (function, vector) {
            (Function::First | Function::Last, vector) => Input::Gathered(vector),
            (_, Vector::Double { values, validity }) => Input::Double { values, validity },
            (_, Vector::Integer { values, validity }) => Input::Integer { values, validity },
            (_, logical @ Vector::Logical { .. }) => {
                match Value::Vector(logical)
                    .cast(ColumnType::Integer)?
                    .into_vector(height)?
                {
                    Vector::Integer { values, validity } => Input::Integer { values, validity },
                    _ => return None,
                }
            }
        };
        Some(Aggregation {
            function,
            input,
            na_rm,
        })
    }
}

/// One aggregation's per-slot accumulators. `missing` records that a group met an `NA`, which
/// makes the result `NA` unless `na.rm = TRUE`.
enum State {
    IntegerSum {
        sums: Vec<i64>,
        missing: Vec<bool>,
    },
    DoubleSum {
        sums: Vec<f64>,
        missing: Vec<bool>,
    },
    Mean {
        sums: Vec<f64>,
        counts: Vec<u32>,
        missing: Vec<bool>,
    },
    IntegerExtreme {
        max: bool,
        values: Vec<i32>,
        seen: Vec<bool>,
        missing: Vec<bool>,
    },
    DoubleExtreme {
        max: bool,
        values: Vec<f64>,
        seen: Vec<bool>,
        missing: Vec<bool>,
    },
    /// The first or last row of each group.
    Row {
        last: bool,
        rows: Vec<u32>,
    },
    Count(Vec<u32>),
    /// Each group's best rows under the ranking, best first, [`NO_ROW`]-padded to `k` per group.
    Top {
        k: usize,
        rows: Vec<u32>,
    },
}

impl State {
    fn new(aggregation: &Aggregation, slots: usize, top: Option<&Ranking>) -> State {
        let floats = matches!(aggregation.input, Input::Double { .. });
        match aggregation.function {
            Function::Sum if floats => State::DoubleSum {
                sums: vec![0.0; slots],
                missing: vec![false; slots],
            },
            Function::Sum => State::IntegerSum {
                sums: vec![0; slots],
                missing: vec![false; slots],
            },
            Function::Mean => State::Mean {
                sums: vec![0.0; slots],
                counts: vec![0; slots],
                missing: vec![false; slots],
            },
            Function::Min | Function::Max if floats => State::DoubleExtreme {
                max: aggregation.function == Function::Max,
                values: vec![0.0; slots],
                seen: vec![false; slots],
                missing: vec![false; slots],
            },
            Function::Min | Function::Max => State::IntegerExtreme {
                max: aggregation.function == Function::Max,
                values: vec![0; slots],
                seen: vec![false; slots],
                missing: vec![false; slots],
            },
            Function::First | Function::Last => State::Row {
                last: aggregation.function == Function::Last,
                rows: vec![NO_ROW; slots],
            },
            Function::Count => State::Count(vec![0; slots]),
            Function::Top => {
                let k = top.map_or(1, |ranking| ranking.k);
                State::Top {
                    k,
                    rows: vec![NO_ROW; slots * k],
                }
            }
        }
    }

    /// Folds in the state of a later run of rows over the same slots.
    fn merge(&mut self, later: State, top: Option<&Ranking>) {
        match (self, later) {
            (
                State::IntegerSum { sums, missing },
                State::IntegerSum {
                    sums: later_sums,
                    missing: later_missing,
                },
            ) => {
                for (sum, later) in sums.iter_mut().zip(later_sums) {
                    *sum += later;
                }
                or_into(missing, later_missing);
            }
            (
                State::DoubleSum { sums, missing },
                State::DoubleSum {
                    sums: later_sums,
                    missing: later_missing,
                },
            ) => {
                for (sum, later) in sums.iter_mut().zip(later_sums) {
                    *sum += later;
                }
                or_into(missing, later_missing);
            }
            (
                State::Mean {
                    sums,
                    counts,
                    missing,
                },
                State::Mean {
                    sums: later_sums,
                    counts: later_counts,
                    missing: later_missing,
                },
            ) => {
                for (sum, later) in sums.iter_mut().zip(later_sums) {
                    *sum += later;
                }
                for (count, later) in counts.iter_mut().zip(later_counts) {
                    *count += later;
                }
                or_into(missing, later_missing);
            }
            (
                State::IntegerExtreme {
                    max,
                    values,
                    seen,
                    missing,
                },
                State::IntegerExtreme {
                    values: later_values,
                    seen: later_seen,
                    missing: later_missing,
                    ..
                },
            ) => {
                let max = *max;
                merge_extreme(values, seen, later_values, later_seen, |a, b| {
                    if max { a.max(b) } else { a.min(b) }
                });
                or_into(missing, later_missing);
            }
            (
                State::DoubleExtreme {
                    max,
                    values,
                    seen,
                    missing,
                },
                State::DoubleExtreme {
                    values: later_values,
                    seen: later_seen,
                    missing: later_missing,
                    ..
                },
            ) => {
                let max = *max;
                merge_extreme(values, seen, later_values, later_seen, |a, b| {
                    extreme(max, a, b)
                });
                or_into(missing, later_missing);
            }
            (
                State::Row { last, rows },
                State::Row {
                    rows: later_rows, ..
                },
            ) => {
                let last = *last;
                for (row, later) in rows.iter_mut().zip(later_rows) {
                    if later != NO_ROW && (last || *row == NO_ROW) {
                        *row = later;
                    }
                }
            }
            (State::Count(counts), State::Count(later_counts)) => {
                for (count, later) in counts.iter_mut().zip(later_counts) {
                    *count += later;
                }
            }
            (
                State::Top { k, rows },
                State::Top {
                    rows: later_rows, ..
                },
            ) => {
                if let Some(ranking) = top {
                    let k = *k;
                    let mut merged = Vec::with_capacity(k);
                    for (group, later) in rows.chunks_mut(k).zip(later_rows.chunks(k)) {
                        merged.clear();
                        merged.extend(
                            group
                                .iter()
                                .chain(later)
                                .copied()
                                .filter(|row| *row != NO_ROW),
                        );
                        merged.sort_by(|a, b| ranking.compare(*a, *b));
                        for (index, slot) in group.iter_mut().enumerate() {
                            *slot = merged.get(index).copied().unwrap_or(NO_ROW);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Appends the slots of another partition.
    fn append(&mut self, other: State) {
        match (self, other) {
            (
                State::IntegerSum { sums, missing },
                State::IntegerSum {
                    sums: other_sums,
                    missing: other_missing,
                },
            ) => {
                sums.extend(other_sums);
                missing.extend(other_missing);
            }
            (
                State::DoubleSum { sums, missing },
                State::DoubleSum {
                    sums: other_sums,
                    missing: other_missing,
                },
            ) => {
                sums.extend(other_sums);
                missing.extend(other_missing);
            }
            (
                State::Mean {
                    sums,
                    counts,
                    missing,
                },
                State::Mean {
                    sums: other_sums,
                    counts: other_counts,
                    missing: other_missing,
                },
            ) => {
                sums.extend(other_sums);
                counts.extend(other_counts);
                missing.extend(other_missing);
            }
            (
                State::IntegerExtreme {
                    values,
                    seen,
                    missing,
                    ..
                },
                State::IntegerExtreme {
                    values: other_values,
                    seen: other_seen,
                    missing: other_missing,
                    ..
                },
            ) => {
                values.extend(other_values);
                seen.extend(other_seen);
                missing.extend(other_missing);
            }
            (
                State::DoubleExtreme {
                    values,
                    seen,
                    missing,
                    ..
                },
                State::DoubleExtreme {
                    values: other_values,
                    seen: other_seen,
                    missing: other_missing,
                    ..
                },
            ) => {
                values.extend(other_values);
                seen.extend(other_seen);
                missing.extend(other_missing);
            }
            (
                State::Row { rows, .. },
                State::Row {
                    rows: other_rows, ..
                },
            ) => {
                rows.extend(other_rows);
            }
            (State::Count(counts), State::Count(other_counts)) => counts.extend(other_counts),
            (
                State::Top { rows, .. },
                State::Top {
                    rows: other_rows, ..
                },
            ) => {
                rows.extend(other_rows);
            }
            _ => {}
        }
    }
}

fn or_into(missing: &mut [bool], later: Vec<bool>) {
    for (missing, later) in missing.iter_mut().zip(later) {
        *missing |= later;
    }
}

fn merge_extreme<T: Copy>(
    values: &mut [T],
    seen: &mut [bool],
    later_values: Vec<T>,
    later_seen: Vec<bool>,
    better: impl Fn(T, T) -> T,
) {
    for (((value, seen), later), later_seen) in values
        .iter_mut()
        .zip(seen.iter_mut())
        .zip(later_values)
        .zip(later_seen)
    {
        if later_seen {
            *value = if *seen { better(*value, later) } else { later };
            *seen = true;
        }
    }
}

/// R's `max` or `min` of two doubles, which is `NaN` when either is.
fn extreme(max: bool, a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if max {
        a.max(b)
    } else {
        a.min(b)
    }
}

/// Accumulates `len` rows into `state`: the `index`th has row `row_of(index)` and belongs to slot
/// `slot_of(index)`. Within one call the rows ascend, which "first" and "last" rely on.
fn update(
    state: &mut State,
    aggregation: &Aggregation,
    top: Option<&Ranking>,
    len: usize,
    row_of: impl Fn(usize) -> usize,
    slot_of: impl Fn(usize) -> usize,
) {
    let na_rm = aggregation.na_rm;
    match (state, &aggregation.input) {
        (State::IntegerSum { sums, missing }, Input::Integer { values, validity }) => {
            match validity {
                None => {
                    for index in 0..len {
                        sums[slot_of(index)] += i64::from(values[row_of(index)]);
                    }
                }
                Some(validity) => {
                    for index in 0..len {
                        let (row, slot) = (row_of(index), slot_of(index));
                        if validity.get_bit(row) {
                            sums[slot] += i64::from(values[row]);
                        } else if !na_rm {
                            missing[slot] = true;
                        }
                    }
                }
            }
        }
        (State::DoubleSum { sums, missing }, Input::Double { values, validity }) => {
            if validity.is_none() && !na_rm {
                for index in 0..len {
                    sums[slot_of(index)] += values[row_of(index)];
                }
            } else {
                for index in 0..len {
                    let (row, slot) = (row_of(index), slot_of(index));
                    let valid = validity
                        .as_ref()
                        .is_none_or(|validity| validity.get_bit(row));
                    let value = values[row];
                    if !valid {
                        if !na_rm {
                            missing[slot] = true;
                        }
                    } else if !(na_rm && value.is_nan()) {
                        sums[slot] += value;
                    }
                }
            }
        }
        (
            State::Mean {
                sums,
                counts,
                missing,
            },
            input,
        ) => {
            for index in 0..len {
                let (row, slot) = (row_of(index), slot_of(index));
                let value = match input {
                    Input::Integer { values, validity } => validity
                        .as_ref()
                        .is_none_or(|validity| validity.get_bit(row))
                        .then(|| f64::from(values[row])),
                    Input::Double { values, validity } => validity
                        .as_ref()
                        .is_none_or(|validity| validity.get_bit(row))
                        .then(|| values[row]),
                    Input::None | Input::Gathered(_) => return,
                };
                match value {
                    Some(value) if !(na_rm && value.is_nan()) => {
                        sums[slot] += value;
                        counts[slot] += 1;
                    }
                    Some(_) => {}
                    None if !na_rm => missing[slot] = true,
                    None => {}
                }
            }
        }
        (
            State::IntegerExtreme {
                max,
                values: kept,
                seen,
                missing,
            },
            Input::Integer { values, validity },
        ) => {
            let max = *max;
            for index in 0..len {
                let (row, slot) = (row_of(index), slot_of(index));
                if validity
                    .as_ref()
                    .is_none_or(|validity| validity.get_bit(row))
                {
                    let value = values[row];
                    kept[slot] = if !seen[slot] {
                        value
                    } else if max {
                        kept[slot].max(value)
                    } else {
                        kept[slot].min(value)
                    };
                    seen[slot] = true;
                } else if !na_rm {
                    missing[slot] = true;
                }
            }
        }
        (
            State::DoubleExtreme {
                max,
                values: kept,
                seen,
                missing,
            },
            Input::Double { values, validity },
        ) => {
            let max = *max;
            for index in 0..len {
                let (row, slot) = (row_of(index), slot_of(index));
                if validity
                    .as_ref()
                    .is_none_or(|validity| validity.get_bit(row))
                {
                    let value = values[row];
                    if na_rm && value.is_nan() {
                        continue;
                    }
                    kept[slot] = if seen[slot] {
                        extreme(max, kept[slot], value)
                    } else {
                        value
                    };
                    seen[slot] = true;
                } else if !na_rm {
                    missing[slot] = true;
                }
            }
        }
        (State::Row { last, rows }, input) => {
            let last = *last;
            // With `na.rm = TRUE`, `first` and `last` skip `NA`, and `NaN` too, as `is.na` does.
            let skipped = |row: usize| match input {
                Input::Gathered(vector) if na_rm => {
                    vector
                        .validity()
                        .is_some_and(|validity| !validity.get_bit(row))
                        || matches!(vector, Vector::Double { values, .. } if values[row].is_nan())
                }
                _ => false,
            };
            for index in 0..len {
                let row = row_of(index);
                let slot = &mut rows[slot_of(index)];
                if (last || *slot == NO_ROW) && !skipped(row) {
                    *slot = row as u32;
                }
            }
        }
        (State::Count(counts), _) => {
            for index in 0..len {
                counts[slot_of(index)] += 1;
            }
        }
        (State::Top { k, rows }, _) => {
            let Some(ranking) = top else { return };
            let k = *k;
            for index in 0..len {
                let row = row_of(index) as u32;
                let group = &mut rows[slot_of(index) * k..][..k];
                // Insertion into a short sorted list: shift worse rows down one place.
                let mut position = k;
                while position > 0
                    && (group[position - 1] == NO_ROW
                        || ranking.compare(row, group[position - 1]).is_lt())
                {
                    position -= 1;
                }
                if position < k {
                    group.copy_within(position..k - 1, position + 1);
                    group[position] = row;
                }
            }
        }
        _ => {}
    }
}

/// One aggregation's result per group, for the slots in `order`.
fn finish(aggregation: &Aggregation, state: &State, order: &[u32]) -> Vector {
    let groups = order.len();
    let validity = |missing: &dyn Fn(usize) -> bool| {
        let bits: Bitmap = order.iter().map(|slot| !missing(*slot as usize)).collect();
        (bits.unset_bits() > 0).then_some(bits)
    };
    match state {
        State::IntegerSum { sums, missing } => {
            let validity = validity(&|slot| missing[slot]);
            // R's integer sum is an integer when every total fits and a double otherwise. An `NA`
            // total stops R's sum before it can overflow, so it fits whatever was added.
            let fits = order.iter().all(|slot| {
                let (sum, missing) = (sums[*slot as usize], missing[*slot as usize]);
                missing || (sum > i64::from(i32::MIN) && sum <= i64::from(i32::MAX))
            });
            if fits {
                Vector::Integer {
                    values: Buffer::from(gather_slots(order, |slot| sums[slot] as i32)),
                    validity,
                }
            } else {
                Vector::Double {
                    values: Buffer::from(gather_slots(order, |slot| sums[slot] as f64)),
                    validity,
                }
            }
        }
        State::DoubleSum { sums, missing } => Vector::Double {
            values: Buffer::from(gather_slots(order, |slot| sums[slot])),
            validity: validity(&|slot| missing[slot]),
        },
        State::Mean {
            sums,
            counts,
            missing,
        } => Vector::Double {
            values: Buffer::from(gather_slots(order, |slot| {
                sums[slot] / f64::from(counts[slot])
            })),
            validity: validity(&|slot| missing[slot]),
        },
        State::IntegerExtreme {
            values,
            seen,
            missing,
            ..
        } => Vector::Integer {
            values: Buffer::from(gather_slots(order, |slot| values[slot])),
            validity: validity(&|slot| missing[slot] || !seen[slot]),
        },
        State::DoubleExtreme {
            values,
            seen,
            missing,
            ..
        } => Vector::Double {
            values: Buffer::from(gather_slots(order, |slot| values[slot])),
            validity: validity(&|slot| missing[slot] || !seen[slot]),
        },
        State::Row { rows, .. } => {
            let rows = gather_slots(order, |slot| rows[slot]);
            match &aggregation.input {
                // A group whose values `na.rm` skipped entirely has no row and gives `NA`.
                Input::Gathered(vector) if rows.contains(&NO_ROW) => {
                    let present_rows: Bitmap = rows.iter().map(|row| *row != NO_ROW).collect();
                    let present: Vec<u32> = rows
                        .iter()
                        .map(|row| if *row == NO_ROW { 0 } else { *row })
                        .collect();
                    with_validity(vector.gather(&present), &present_rows)
                }
                Input::Gathered(vector) => vector.gather(&rows),
                _ => Vector::Integer {
                    values: Buffer::from(vec![0; groups]),
                    validity: Some(Bitmap::new_zeroed(groups)),
                },
            }
        }
        State::Count(counts) => Vector::Integer {
            values: Buffer::from(gather_slots(order, |slot| counts[slot] as i32)),
            validity: None,
        },
        State::Top { .. } => Vector::Integer {
            values: Buffer::from(vec![0; groups]),
            validity: Some(Bitmap::new_zeroed(groups)),
        },
    }
}

fn gather_slots<T: Send>(order: &[u32], value: impl Fn(usize) -> T + Sync) -> Vec<T> {
    fill(order.len(), |start, out| {
        let end = start + out.len();
        for (slot, index) in out.iter_mut().zip(&order[start..end]) {
            slot.write(value(*index as usize));
        }
    })
}

/// The order `x[order(v)]` puts rows in: by one numeric column, ascending or descending, `NA`
/// last, ties in row order.
struct Ranking {
    values: Vector,
    descending: bool,
    k: usize,
}

impl Ranking {
    fn compare(&self, left: u32, right: u32) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let (left_row, right_row) = (left as usize, right as usize);
        let valid = |row: usize| {
            self.values
                .validity()
                .is_none_or(|validity| validity.get_bit(row))
        };
        let by_value = match (valid(left_row), valid(right_row)) {
            (false, false) => Ordering::Equal,
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (true, true) => {
                let ordering = match &self.values {
                    Vector::Integer { values, .. } => values[left_row].cmp(&values[right_row]),
                    Vector::Double { values, .. } => values[left_row]
                        .partial_cmp(&values[right_row])
                        .unwrap_or(Ordering::Equal),
                    Vector::Logical { values, .. } => {
                        values.get_bit(left_row).cmp(&values.get_bit(right_row))
                    }
                };
                if self.descending {
                    ordering.reverse()
                } else {
                    ordering
                }
            }
        };
        by_value.then(left.cmp(&right))
    }
}

/// The `x[order(v), .(name = head(w, k)), by]` shape: one sort key and one `head` of a column.
fn top_k_shape<'a>(
    keys: &'a [SortKey],
    columns: &[(PlSmallStr, &Expr)],
) -> Option<(&'a SortKey, usize)> {
    let [sort] = keys else { return None };
    let [(_, Expr::Head(arg, k))] = columns else {
        return None;
    };
    if !matches!(sort.expr, Expr::Column(_))
        || !matches!(**arg, Expr::Column(_))
        || *k == 0
        || *k > MAX_TOP
    {
        return None;
    }
    Some((sort, *k))
}

/// Runs the top-k shape: each group's `k` best rows under the sort, groups in order of their
/// best row (which is where each first appears in the sorted table).
#[allow(clippy::too_many_arguments)]
fn top_k(
    frame: &DataFrame,
    evaluator: &Evaluator,
    keys: &Keys,
    by: &[(PlSmallStr, Expr)],
    keyed: bool,
    sort: &SortKey,
    k: usize,
    columns: &[(PlSmallStr, &Expr)],
) -> Result<Option<DataFrame>> {
    let height = frame.height();
    let [(name, Expr::Head(arg, _))] = columns else {
        return Ok(None);
    };
    let (Some(Value::Vector(values)), Some(Value::Vector(kept))) =
        (evaluator.eval(&sort.expr), evaluator.eval(arg))
    else {
        return Ok(None);
    };
    // `NaN` sorts differently in R's `order` and in Polars, so the plan decides.
    if let Vector::Double {
        values: numbers, ..
    } = &values
        && numbers.iter().any(|value| value.is_nan())
    {
        return Ok(None);
    }
    let ranking = Ranking {
        values,
        descending: sort.descending,
        k,
    };
    let aggregation = Aggregation {
        function: Function::Top,
        input: Input::None,
        na_rm: false,
    };
    let codes = row_codes(keys, height);
    let grouping = group(
        &codes,
        keys.radix,
        std::slice::from_ref(&aggregation),
        Some(&ranking),
    );
    let Some(State::Top { rows, .. }) = grouping.states.first() else {
        return Ok(None);
    };
    let mut order: Vec<u32> = if grouping.codes.is_empty() {
        (0..grouping.first_rows.len() as u32)
            .filter(|slot| grouping.first_rows[*slot as usize] != NO_ROW)
            .collect()
    } else {
        (0..grouping.first_rows.len() as u32).collect()
    };
    if keyed {
        if !grouping.codes.is_empty() {
            sort_by_key(&mut order, |slot| grouping.codes[*slot as usize]);
        }
    } else {
        let best = |slot: u32| rows[slot as usize * k];
        if order.len() < PARALLEL_ROWS {
            order.sort_unstable_by(|a, b| ranking.compare(best(*a), best(*b)));
        } else {
            THREAD_POOL
                .install(|| order.par_sort_unstable_by(|a, b| ranking.compare(best(*a), best(*b))));
        }
    }
    let mut kept_rows = Vec::with_capacity(order.len() * k);
    for slot in &order {
        kept_rows.extend(
            rows[*slot as usize * k..][..k]
                .iter()
                .copied()
                .filter(|row| *row != NO_ROW),
        );
    }
    // Every kept row belongs to its group, so the grouping columns read at the kept rows repeat
    // each group's key once per value.
    let mut result = key_columns(frame, by, &kept_rows)?;
    result.push(kept.gather(&kept_rows).into_column(name.clone()));
    Ok(Some(DataFrame::new(kept_rows.len(), result)?))
}

/// The vector with the rows `keep` clears set to `NA`.
fn with_validity(vector: Vector, keep: &Bitmap) -> Vector {
    let combine = |validity: Option<Bitmap>| {
        Some(match validity {
            Some(validity) => &validity & keep,
            None => keep.clone(),
        })
    };
    match vector {
        Vector::Logical { values, validity } => Vector::Logical {
            values,
            validity: combine(validity),
        },
        Vector::Integer { values, validity } => Vector::Integer {
            values,
            validity: combine(validity),
        },
        Vector::Double { values, validity } => Vector::Double {
            values,
            validity: combine(validity),
        },
    }
}
