//! Join kernels for `x[i, on = ...]`.
//!
//! Polars' hash join hashes both sides and then restores the order data.table promises (each row
//! of `i` in turn, with its matches in `x`'s order), which made joins slower than data.table's
//! binary search on sorted keys. These kernels code the join columns into one dense integer per
//! row instead (an integer `v` as `v - min`, a factor by its label, `NA` as a code of its own, so
//! that `NA` matches `NA` as in data.table) and match on the codes:
//!
//! - When `i` is small, the codes of `i` index its rows, every thread scans a run of `x` and
//!   counts the matches of each `i` row, and a second pass writes them in place. That keeps the
//!   big side's scan sequential and parallel.
//! - Otherwise the rows of `x` are bucketed by code, and every row of `i` copies its bucket.
//!
//! A rolling join first checks that `x` is sorted within each group of the leading join columns,
//! with each rolling value at most once, and then binary searches each row of `i` in its group.
//!
//! Join columns of other types (doubles, strings, a mix), codes spanning more than
//! [`MAX_CODES`] values, rolling to the nearest value, and rolling over `NA`s give `None`, and
//! the join runs as a Polars plan.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use polars::prelude::{Column, DataFrame, DataType, GroupsType, PlSmallStr};
use polars_arrow::bitmap::Bitmap;
use polars_core::runtime::THREAD_POOL;
use rayon::prelude::*;

use crate::convert::factor_levels;
use crate::kernel::{
    CodeValues, Codes, PARALLEL_ROWS, Shared, Vector, fill, gather_column, pack_map,
};
use crate::query::{Comparison, Join, Mult, On, Roll, Side, join_layout};
use crate::{Error, Result, Table};

/// The widest code range the kernels index directly: one bucket per code on the built side.
const MAX_CODES: u64 = 1 << 27;
/// The most rows of `i` the kernels index while scanning `x`. Each thread keeps a count per row of
/// `i`, so this bounds that memory.
const MAX_INDEXED_I: usize = 1 << 16;
/// Marks a row of `i` that matched nothing.
const NO_ROW: u32 = u32::MAX;

/// Joins `x` and `join.other` on `on`, the `(x column, i column)` pairs, into data.table's layout,
/// or gives `None` when the join is outside what the kernels cover.
pub(crate) fn join(
    x: &Table,
    join: &Join,
    on: &[On],
    types: &[Option<DataType>],
) -> Result<Option<DataFrame>> {
    let (x_frame, i_frame) = (x.frame(), join.other.frame());
    let (x_height, i_height) = (x_frame.height(), i_frame.height());
    if on.is_empty() || u32::try_from(x_height).is_err() || u32::try_from(i_height).is_err() {
        return Ok(None);
    }
    if on.iter().any(|condition| condition.op != Comparison::Equal) {
        let matches = non_equi(x_frame, i_frame, on, types, join.drop_unmatched, join.mult)?;
        return assemble(x_frame, i_frame, on, &matches).map(Some);
    }
    let matches = match join.roll {
        Roll::None => {
            let Some(keys) = key_codes(x_frame, i_frame, on)? else {
                return Ok(None);
            };
            // Buckets cost memory per possible code, so sparse keys over a wide range are left
            // to the plan's hash join.
            if u64::from(keys.radix) > 8 * (x_height + i_height) as u64 + (1 << 12) {
                return Ok(None);
            }
            if i_height <= MAX_INDEXED_I && i_height <= x_height {
                scan_x(&keys, join.drop_unmatched)
            } else {
                scan_i(&keys, join.drop_unmatched)
            }
        }
        Roll::Backward(limit) => {
            let Some(matches) = rolling(x_frame, i_frame, on, Direction::Backward, limit)? else {
                return Ok(None);
            };
            matches
        }
        Roll::Forward(limit) => {
            let Some(matches) = rolling(x_frame, i_frame, on, Direction::Forward, limit)? else {
                return Ok(None);
            };
            matches
        }
        Roll::Nearest => return Ok(None),
    };
    let matches = if join.drop_unmatched && join.roll != Roll::None {
        matches.drop_unmatched()
    } else {
        matches
    };
    let matches = match join.mult {
        Mult::All => matches,
        mult => matches.keep_one(mult == Mult::Last),
    };
    assemble(x_frame, i_frame, on, &matches).map(Some)
}

/// The output rows of a join: the row of `i` and the row of `x` ([`NO_ROW`] when unmatched) each
/// output row takes its values from.
struct Matches {
    i_rows: Vec<u32>,
    x_rows: Vec<u32>,
}

impl Matches {
    /// `mult = "first"` or `"last"`: one row per row of `i`. A row's matches are adjacent and in
    /// `x`'s order.
    fn keep_one(self, last: bool) -> Matches {
        let height = self.i_rows.len();
        let kept = |position: usize| {
            let neighbour = if last {
                position.checked_add(1).filter(|next| *next < height)
            } else {
                position.checked_sub(1)
            };
            neighbour.is_none_or(|neighbour| self.i_rows[neighbour] != self.i_rows[position])
        };
        let (i_rows, x_rows) = (0..height)
            .filter(|position| kept(*position))
            .map(|position| (self.i_rows[position], self.x_rows[position]))
            .unzip();
        Matches { i_rows, x_rows }
    }

    fn drop_unmatched(self) -> Matches {
        let (i_rows, x_rows) = self
            .i_rows
            .into_iter()
            .zip(self.x_rows)
            .filter(|(_, x_row)| *x_row != NO_ROW)
            .unzip();
        Matches { i_rows, x_rows }
    }
}

/// Both sides' join columns coded into one integer per row, in `0..radix`. Equal keys get equal
/// codes, `NA` included.
struct Keys {
    x: Vec<u32>,
    i: Vec<u32>,
    radix: u32,
}

fn key_codes(x: &DataFrame, i: &DataFrame, on: &[On]) -> Result<Option<Keys>> {
    let mut keys: Option<Keys> = None;
    for condition in on {
        // A missing column is left to the plan, which reports it.
        let (Ok(x_column), Ok(i_column)) = (x.column(&condition.x), i.column(&condition.i)) else {
            return Ok(None);
        };
        let Some(digit) = column_codes(x_column, i_column) else {
            return Ok(None);
        };
        keys = Some(match keys {
            None => digit,
            Some(keys) => {
                let radix = u64::from(keys.radix) * u64::from(digit.radix);
                if radix > MAX_CODES {
                    return Ok(None);
                }
                let combine = |high: &[u32], low: &[u32]| {
                    fill(high.len(), |start, out| {
                        let end = start + out.len();
                        for ((slot, high), low) in
                            out.iter_mut().zip(&high[start..end]).zip(&low[start..end])
                        {
                            slot.write(high * digit.radix + low);
                        }
                    })
                };
                Keys {
                    x: combine(&keys.x, &digit.x),
                    i: combine(&keys.i, &digit.i),
                    radix: radix as u32,
                }
            }
        });
    }
    Ok(keys)
}

/// One pair of join columns coded as [`Keys`], or `None` for a pair the kernels do not code.
fn column_codes(x: &Column, i: &Column) -> Option<Keys> {
    match (x.dtype(), i.dtype()) {
        (DataType::Int32, DataType::Int32) => integer_codes(x, i),
        (DataType::Boolean, DataType::Boolean) => logical_codes(x, i),
        (DataType::Enum(..), DataType::Enum(..)) => factor_codes(x, i),
        _ => None,
    }
}

fn integer_codes(x: &Column, i: &Column) -> Option<Keys> {
    let (
        Vector::Integer {
            values: x_values,
            validity: x_validity,
        },
        Vector::Integer {
            values: i_values,
            validity: i_validity,
        },
    ) = (Vector::of(x)?, Vector::of(i)?)
    else {
        return None;
    };
    let range = |values: &[i32], validity: &Option<Bitmap>| {
        let valid = |row: usize| {
            validity
                .as_ref()
                .is_none_or(|validity| validity.get_bit(row))
        };
        let mut low = i32::MAX;
        let mut high = i32::MIN;
        for (row, value) in values.iter().enumerate() {
            if valid(row) {
                low = low.min(*value);
                high = high.max(*value);
            }
        }
        (low, high)
    };
    let (x_low, x_high) = range(&x_values, &x_validity);
    let (i_low, i_high) = range(&i_values, &i_validity);
    let (low, high) = (x_low.min(i_low), x_high.max(i_high));
    // With no valid value on either side, every key is `NA`.
    let span = if low > high {
        0
    } else {
        u64::try_from(i64::from(high) - i64::from(low) + 1).ok()?
    };
    if span + 1 > MAX_CODES {
        return None;
    }
    let na = span as u32;
    let codes = |values: &[i32], validity: &Option<Bitmap>| {
        fill(values.len(), |start, out| {
            let end = start + out.len();
            for (offset, (slot, value)) in out.iter_mut().zip(&values[start..end]).enumerate() {
                let valid = validity
                    .as_ref()
                    .is_none_or(|validity| validity.get_bit(start + offset));
                slot.write(if valid {
                    (i64::from(*value) - i64::from(low)) as u32
                } else {
                    na
                });
            }
        })
    };
    Some(Keys {
        x: codes(&x_values, &x_validity),
        i: codes(&i_values, &i_validity),
        radix: na + 1,
    })
}

fn logical_codes(x: &Column, i: &Column) -> Option<Keys> {
    let codes = |vector: Vector| match vector {
        Vector::Logical { values, validity } => Some(fill(values.len(), |start, out| {
            for (offset, slot) in out.iter_mut().enumerate() {
                let row = start + offset;
                let valid = validity
                    .as_ref()
                    .is_none_or(|validity| validity.get_bit(row));
                slot.write(if valid {
                    u32::from(values.get_bit(row))
                } else {
                    2
                });
            }
        })),
        _ => None,
    };
    Some(Keys {
        x: codes(Vector::of(x)?)?,
        i: codes(Vector::of(i)?)?,
        radix: 3,
    })
}

/// Factors match by label, so `i`'s levels are translated into `x`'s. A level of `i` that `x`
/// lacks gets a code no row of `x` has.
fn factor_codes(x: &Column, i: &Column) -> Option<Keys> {
    let x_levels = factor_levels(x.dtype())?;
    let i_levels = factor_levels(i.dtype())?;
    let positions: HashMap<&str, u32> = x_levels
        .iter()
        .enumerate()
        .map(|(position, level)| (level.as_str(), position as u32))
        .collect();
    let levels = u32::try_from(x_levels.len()).ok()?;
    let (na, absent) = (levels, levels + 1);
    let translation: Vec<u32> = i_levels
        .iter()
        .map(|level| positions.get(level.as_str()).copied().unwrap_or(absent))
        .collect();
    let identity: Vec<u32> = (0..levels).collect();
    let codes = |codes: Codes, translation: &[u32]| {
        let validity = codes.validity;
        let translate = |row: usize, code: u32| {
            if validity
                .as_ref()
                .is_none_or(|validity| validity.get_bit(row))
            {
                translation.get(code as usize).copied().unwrap_or(absent)
            } else {
                na
            }
        };
        macro_rules! translate {
            ($values:expr) => {
                fill($values.len(), |start, out| {
                    let end = start + out.len();
                    for (offset, (slot, code)) in
                        out.iter_mut().zip(&$values[start..end]).enumerate()
                    {
                        slot.write(translate(start + offset, u32::from(*code)));
                    }
                })
            };
        }
        match &codes.values {
            CodeValues::U8(values) => translate!(values),
            CodeValues::U16(values) => translate!(values),
            CodeValues::U32(values) => translate!(values),
        }
    };
    Some(Keys {
        x: codes(Codes::of(x)?, &identity),
        i: codes(Codes::of(i)?, &translation),
        radix: levels + 2,
    })
}

/// Rows grouped by code, each group in row order: the rows with code `c` are
/// `rows[starts[c]..starts[c + 1]]`.
struct Buckets {
    starts: Vec<u32>,
    rows: Vec<u32>,
}

impl Buckets {
    fn of(codes: &[u32], radix: u32) -> Buckets {
        let mut starts = vec![0u32; radix as usize + 1];
        for code in codes {
            starts[*code as usize + 1] += 1;
        }
        for index in 1..starts.len() {
            starts[index] += starts[index - 1];
        }
        let mut next = starts.clone();
        let mut rows = vec![0u32; codes.len()];
        for (row, code) in codes.iter().enumerate() {
            let slot = &mut next[*code as usize];
            rows[*slot as usize] = row as u32;
            *slot += 1;
        }
        Buckets { starts, rows }
    }

    fn get(&self, code: u32) -> &[u32] {
        let code = code as usize;
        &self.rows[self.starts[code] as usize..self.starts[code + 1] as usize]
    }
}

/// The join for a small `i`: buckets `i`'s rows by code and scans `x` in parallel runs, counting
/// each `i` row's matches per run first so that the second pass writes every match straight into
/// its output position.
fn scan_x(keys: &Keys, drop_unmatched: bool) -> Matches {
    let (x_height, i_height) = (keys.x.len(), keys.i.len());
    let buckets = Buckets::of(&keys.i, keys.radix);
    let threads = THREAD_POOL.current_num_threads().max(1);
    let runs = if x_height < PARALLEL_ROWS {
        1
    } else {
        threads * 4
    };
    let run_length = x_height.div_ceil(runs).max(1);
    let run_range =
        |run: usize| (run * run_length).min(x_height)..((run + 1) * run_length).min(x_height);
    let counts: Vec<Vec<u32>> = THREAD_POOL.install(|| {
        (0..runs)
            .into_par_iter()
            .map(|run| {
                let mut counts = vec![0u32; i_height];
                for code in &keys.x[run_range(run)] {
                    for i_row in buckets.get(*code) {
                        counts[*i_row as usize] += 1;
                    }
                }
                counts
            })
            .collect()
    });
    // Output positions: `i` row major, run minor, so each row's matches stay in `x`'s order.
    let mut offsets = vec![0usize; i_height * runs];
    let mut i_starts = Vec::with_capacity(i_height + 1);
    let mut total = 0usize;
    for i_row in 0..i_height {
        i_starts.push(total);
        let mut matched = 0usize;
        for (run, counts) in counts.iter().enumerate() {
            offsets[i_row * runs + run] = total + matched;
            matched += counts[i_row] as usize;
        }
        total += if matched == 0 && !drop_unmatched {
            1
        } else {
            matched
        };
    }
    i_starts.push(total);
    let mut x_rows = vec![NO_ROW; total];
    let out = Shared(x_rows.as_mut_ptr());
    let offsets = &offsets;
    THREAD_POOL.install(|| {
        (0..runs).into_par_iter().for_each(|run| {
            let range = run_range(run);
            let mut next: Vec<usize> = (0..i_height)
                .map(|i_row| offsets[i_row * runs + run])
                .collect();
            for (offset, code) in keys.x[range.clone()].iter().enumerate() {
                for i_row in buckets.get(*code) {
                    let slot = &mut next[*i_row as usize];
                    // SAFETY: each run writes only the positions its counts reserved, which no
                    // other run's reservation overlaps, and they lie inside `x_rows`.
                    unsafe { out.write(*slot, (range.start + offset) as u32) };
                    *slot += 1;
                }
            }
        });
    });
    let i_rows = fill(total, |start, out| {
        let mut i_row = i_starts.partition_point(|position| *position <= start) - 1;
        for (offset, slot) in out.iter_mut().enumerate() {
            while i_starts[i_row + 1] <= start + offset {
                i_row += 1;
            }
            slot.write(i_row as u32);
        }
    });
    Matches { i_rows, x_rows }
}

/// The join for a large `i`: buckets `x`'s rows by code, and every row of `i` copies its bucket.
fn scan_i(keys: &Keys, drop_unmatched: bool) -> Matches {
    let i_height = keys.i.len();
    let buckets = Buckets::of(&keys.x, keys.radix);
    let width = |code: u32| {
        let matched = buckets.get(code).len();
        if matched == 0 && !drop_unmatched {
            1
        } else {
            matched
        }
    };
    let mut starts = Vec::with_capacity(i_height + 1);
    let mut total = 0usize;
    for code in &keys.i {
        starts.push(total);
        total += width(*code);
    }
    starts.push(total);
    let mut i_rows = vec![0u32; total];
    let mut x_rows = vec![NO_ROW; total];
    let (i_out, x_out) = (Shared(i_rows.as_mut_ptr()), Shared(x_rows.as_mut_ptr()));
    let starts = &starts;
    THREAD_POOL.install(|| {
        (0..i_height)
            .into_par_iter()
            .with_min_len(PARALLEL_ROWS)
            .for_each(|i_row| {
                let start = starts[i_row];
                let matched = buckets.get(keys.i[i_row]);
                for offset in 0..starts[i_row + 1] - start {
                    // SAFETY: row `i_row` owns the positions `starts[i_row]..starts[i_row + 1]`.
                    unsafe {
                        i_out.write(start + offset, i_row as u32);
                        if let Some(x_row) = matched.get(offset) {
                            x_out.write(start + offset, *x_row);
                        }
                    }
                }
            });
    });
    Matches { i_rows, x_rows }
}

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    /// The last row of `x` at or before the value of `i`.
    Backward,
    /// The first row of `x` at or after the value of `i`.
    Forward,
}

/// A rolling join: one output row per row of `i`, matched to the row of `x` with the same leading
/// join columns whose rolling value is the closest one in `direction`, within `limit`.
fn rolling(
    x: &DataFrame,
    i: &DataFrame,
    on: &[On],
    direction: Direction,
    limit: Option<f64>,
) -> Result<Option<Matches>> {
    let Some((rolled, groups)) = on.split_last() else {
        return Ok(None);
    };
    let (x_roll, i_roll) = (&rolled.x, &rolled.i);
    let (x_height, i_height) = (x.height(), i.height());
    let keys = if groups.is_empty() {
        Keys {
            x: vec![0; x_height],
            i: vec![0; i_height],
            radix: 1,
        }
    } else {
        match key_codes(x, i, groups)? {
            Some(keys) => keys,
            None => return Ok(None),
        }
    };
    let (Ok(x_column), Ok(i_column)) = (x.column(x_roll), i.column(i_roll)) else {
        return Ok(None);
    };
    if x_column.dtype() != i_column.dtype()
        || x_column.null_count() > 0
        || i_column.null_count() > 0
    {
        return Ok(None);
    }
    let x_rows = match (Vector::of(x_column), Vector::of(i_column)) {
        (
            Some(Vector::Integer {
                values: x_values, ..
            }),
            Some(Vector::Integer {
                values: i_values, ..
            }),
        ) => roll(&keys, &x_values, &i_values, direction, limit),
        (
            Some(Vector::Double {
                values: x_values, ..
            }),
            Some(Vector::Double {
                values: i_values, ..
            }),
        ) if !x_values
            .iter()
            .chain(i_values.iter())
            .any(|value| value.is_nan()) =>
        {
            roll(&keys, &x_values, &i_values, direction, limit)
        }
        _ => None,
    };
    let Some(x_rows) = x_rows else {
        return Ok(None);
    };
    let i_rows = fill(i_height, |start, out| {
        for (offset, slot) in out.iter_mut().enumerate() {
            slot.write((start + offset) as u32);
        }
    });
    Ok(Some(Matches { i_rows, x_rows }))
}

/// Matches each row of `i` to a row of `x` by binary search within its group, or gives `None` when
/// `x` is not one run of rows per group with strictly increasing rolling values, which a table
/// keyed on the join columns is.
fn roll<T>(
    keys: &Keys,
    x_values: &[T],
    i_values: &[T],
    direction: Direction,
    limit: Option<f64>,
) -> Option<Vec<u32>>
where
    T: Copy + PartialOrd + Into<f64> + Send + Sync,
{
    let x_height = x_values.len();
    let chunk = PARALLEL_ROWS.max(x_height.div_ceil(THREAD_POOL.current_num_threads().max(1) * 4));
    // Each chunk checks the rows it holds against their predecessors, so chunk borders are checked
    // too, and lists where a new group starts.
    let starts: Option<Vec<Vec<u32>>> = THREAD_POOL.install(|| {
        (0..x_height.div_ceil(chunk))
            .into_par_iter()
            .map(|index| {
                let mut starts = Vec::new();
                for row in (index * chunk).max(1)..((index + 1) * chunk).min(x_height) {
                    if keys.x[row] != keys.x[row - 1] {
                        starts.push(row as u32);
                    } else if x_values[row] <= x_values[row - 1] {
                        return None;
                    }
                }
                Some(starts)
            })
            .collect()
    });
    let mut runs: Vec<Option<Range<u32>>> = vec![None; keys.radix as usize];
    let mut start = 0u32;
    let ends = starts?
        .into_iter()
        .flatten()
        .chain(std::iter::once(x_height as u32));
    for end in ends {
        if end == 0 {
            continue;
        }
        let slot = runs.get_mut(*keys.x.get(start as usize)? as usize)?;
        if slot.is_some() {
            return None;
        }
        *slot = Some(start..end);
        start = end;
    }
    let within = |distance: f64| limit.is_none_or(|limit| distance <= limit.abs());
    Some(fill(i_values.len(), |start, out| {
        for (offset, slot) in out.iter_mut().enumerate() {
            let i_row = start + offset;
            let value = i_values[i_row];
            let matched = runs[keys.i[i_row] as usize].clone().and_then(|run| {
                let values = &x_values[run.start as usize..run.end as usize];
                let after = values.partition_point(|x_value| *x_value <= value);
                match direction {
                    Direction::Backward => after
                        .checked_sub(1)
                        .filter(|position| within(value.into() - values[*position].into())),
                    Direction::Forward => {
                        let at = if after > 0 && values[after - 1] == value {
                            after - 1
                        } else {
                            after
                        };
                        (at < values.len() && within(values[at].into() - value.into()))
                            .then_some(at)
                    }
                }
                .map(|position| run.start + position as u32)
            });
            slot.write(matched.unwrap_or(NO_ROW));
        }
    }))
}

/// The joined frame in data.table's layout, as [`join_layout`] gives it.
fn assemble(x: &DataFrame, i: &DataFrame, on: &[On], matches: &Matches) -> Result<DataFrame> {
    let height = matches.i_rows.len();
    let unmatched = pack_map(height, &matches.x_rows, |row| row != NO_ROW);
    let (x_rows, keep) = if unmatched.unset_bits() == 0 {
        (Cow::Borrowed(&matches.x_rows), None)
    } else {
        let rows = fill(height, |start, out| {
            let end = start + out.len();
            for (slot, row) in out.iter_mut().zip(&matches.x_rows[start..end]) {
                slot.write(if *row == NO_ROW { 0 } else { *row });
            }
        });
        (Cow::Owned(rows), Some(unmatched))
    };
    let layout = join_layout(&x.get_column_names(), &i.get_column_names(), on);
    // Each gather is parallel over rows, so the columns go one at a time.
    let columns = layout
        .into_iter()
        .map(|column| match column.side {
            Side::I => gather_column(
                i.column(&column.source)?,
                &matches.i_rows,
                None,
                column.name,
            ),
            Side::X => gather_column(
                x.column(&column.source)?,
                &x_rows,
                keep.as_ref(),
                column.name,
            ),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(DataFrame::new(height, columns)?)
}

/// A join with an ordering condition, run for every such join. The equality conditions split `x`
/// into groups, and the rows of each group sort by the column of `x` the first ordering condition
/// compares. Every condition on that column bounds a range of the sorted group, found by binary
/// search, so a row of `i` visits only the rows of `x` inside all of those ranges and checks the
/// remaining conditions on them. Its matches then sort back into `x`'s order.
fn non_equi(
    x: &DataFrame,
    i: &DataFrame,
    on: &[On],
    types: &[Option<DataType>],
    drop_unmatched: bool,
    mult: Mult,
) -> Result<Matches> {
    let (x_height, i_height) = (x.height(), i.height());
    let equal: Vec<(On, Option<DataType>)> = on
        .iter()
        .zip(types)
        .filter(|(condition, _)| condition.op == Comparison::Equal)
        .map(|(condition, common)| (condition.clone(), common.clone()))
        .collect();
    let groups = equal_groups(x, i, &equal)?;
    let ordering: Vec<&On> = on
        .iter()
        .filter(|condition| condition.op != Comparison::Equal)
        .collect();
    let ranged = &ordering
        .first()
        .ok_or_else(|| Error::Query("a non-equi join needs an ordering condition".to_string()))?
        .x;
    let x_keys = order_keys(x.column(ranged)?)?;
    let mut bounds = Vec::new();
    let mut checks = Vec::new();
    for condition in ordering {
        let i_keys = order_keys(i.column(&condition.i)?)?;
        if condition.x == *ranged {
            bounds.push((condition.op, i_keys));
        } else {
            checks.push((condition.op, order_keys(x.column(&condition.x)?)?, i_keys));
        }
    }
    // The rows of `x` as `(order key, row)`, grouped and sorted by key within each group.
    let mut starts = vec![0usize; groups.count + 1];
    for group in &groups.x {
        starts[*group as usize + 1] += 1;
    }
    for group in 0..groups.count {
        starts[group + 1] += starts[group];
    }
    let mut sorted = vec![(0u64, 0u32); x_height];
    let mut next = starts.clone();
    for (row, group) in groups.x.iter().enumerate() {
        let slot = &mut next[*group as usize];
        sorted[*slot] = (x_keys[row], row as u32);
        *slot += 1;
    }
    let mut slices = Vec::with_capacity(groups.count);
    let mut rest = sorted.as_mut_slice();
    for group in 0..groups.count {
        let (slice, tail) = rest.split_at_mut(starts[group + 1] - starts[group]);
        slices.push(slice);
        rest = tail;
    }
    THREAD_POOL.install(|| {
        slices.into_par_iter().for_each(|slice| {
            if slice.len() > PARALLEL_ROWS {
                slice.par_sort_unstable();
            } else {
                slice.sort_unstable();
            }
        });
    });
    let matches_of = |row: usize, found: &mut Vec<u32>| {
        found.clear();
        let group = groups.i[row] as usize;
        let Some(slice) = starts
            .get(group)
            .zip(starts.get(group + 1))
            .map(|(start, end)| &sorted[*start..*end])
        else {
            return;
        };
        let (mut start, mut end) = (0, slice.len());
        for (op, i_keys) in &bounds {
            let (low, high) = key_range(slice, *op, i_keys[row]);
            start = start.max(low);
            end = end.min(high);
        }
        if start >= end {
            return;
        }
        found.extend(
            slice[start..end]
                .iter()
                .map(|(_, x_row)| *x_row)
                .filter(|x_row| {
                    checks.iter().all(|(op, x_keys, i_keys)| {
                        meets(*op, x_keys[*x_row as usize], i_keys[row])
                    })
                }),
        );
        match mult {
            Mult::All => found.sort_unstable(),
            Mult::First | Mult::Last => {
                let kept = if mult == Mult::First {
                    found.iter().min()
                } else {
                    found.iter().max()
                };
                let kept: Vec<u32> = kept.copied().into_iter().collect();
                *found = kept;
            }
        }
    };
    const CHUNK: usize = 1 << 12;
    let parts: Vec<(Vec<u32>, Vec<u32>)> = THREAD_POOL.install(|| {
        (0..i_height.div_ceil(CHUNK))
            .into_par_iter()
            .map(|chunk| {
                let (mut i_rows, mut x_rows) = (Vec::new(), Vec::new());
                let mut found = Vec::new();
                for row in chunk * CHUNK..((chunk + 1) * CHUNK).min(i_height) {
                    matches_of(row, &mut found);
                    if found.is_empty() {
                        if !drop_unmatched {
                            i_rows.push(row as u32);
                            x_rows.push(NO_ROW);
                        }
                        continue;
                    }
                    i_rows.extend(std::iter::repeat_n(row as u32, found.len()));
                    x_rows.extend_from_slice(&found);
                }
                (i_rows, x_rows)
            })
            .collect()
    });
    let height = parts.iter().map(|(i_rows, _)| i_rows.len()).sum();
    let mut matches = Matches {
        i_rows: Vec::with_capacity(height),
        x_rows: Vec::with_capacity(height),
    };
    for (i_rows, x_rows) in parts {
        matches.i_rows.extend(i_rows);
        matches.x_rows.extend(x_rows);
    }
    Ok(matches)
}

/// The rows of `x` and of `i` numbered by their values in the equality conditions, equal values
/// sharing a number in `0..count`, `NA` matching `NA`.
struct Groups {
    x: Vec<u32>,
    i: Vec<u32>,
    count: usize,
}

fn equal_groups(x: &DataFrame, i: &DataFrame, equal: &[(On, Option<DataType>)]) -> Result<Groups> {
    let (x_height, i_height) = (x.height(), i.height());
    if equal.is_empty() {
        return Ok(Groups {
            x: vec![0; x_height],
            i: vec![0; i_height],
            count: 1,
        });
    }
    let conditions: Vec<On> = equal
        .iter()
        .map(|(condition, _)| condition.clone())
        .collect();
    if let Some(keys) = key_codes(x, i, &conditions)?
        && u64::from(keys.radix) <= 8 * (x_height + i_height) as u64 + (1 << 12)
    {
        return Ok(Groups {
            x: keys.x,
            i: keys.i,
            count: keys.radix as usize,
        });
    }
    // Other types, or codes spread too wide to index, are numbered by Polars' grouping over both
    // sides stacked, with each pair of columns in its common type.
    let mut columns = Vec::with_capacity(equal.len());
    for (index, (condition, common)) in equal.iter().enumerate() {
        let (mut x_column, mut i_column) = (
            x.column(&condition.x)?.as_materialized_series().clone(),
            i.column(&condition.i)?.as_materialized_series().clone(),
        );
        if let Some(common) = common {
            x_column = x_column.cast(common)?;
            i_column = i_column.cast(common)?;
        }
        let mut stacked = x_column.with_name(format!("{index}").into());
        stacked.append(&i_column)?;
        columns.push(Column::from(stacked));
    }
    let names: Vec<PlSmallStr> = columns.iter().map(|column| column.name().clone()).collect();
    let stacked = DataFrame::new(x_height + i_height, columns)?;
    let grouping = stacked.group_by(names)?;
    let mut numbers = vec![0u32; x_height + i_height];
    let mut count = 0;
    match &**grouping.get_groups() {
        GroupsType::Idx(groups) => {
            for rows in groups.all() {
                for row in rows.iter() {
                    numbers[*row as usize] = count;
                }
                count += 1;
            }
        }
        GroupsType::Slice { groups, .. } => {
            for [first, len] in groups.iter() {
                numbers[*first as usize..(*first + *len) as usize].fill(count);
                count += 1;
            }
        }
    }
    let i_numbers = numbers.split_off(x_height);
    Ok(Groups {
        x: numbers,
        i: i_numbers,
        count: count as usize,
    })
}

/// The order key of an `NA`, below every other key.
const NA_KEY: u64 = 0;
/// The order key of a `NaN`, above `NA` and below every number.
const NAN_KEY: u64 = 1;

/// Each value of a numeric column as a `u64` whose order is the values' order: [`NA_KEY`], then
/// [`NAN_KEY`], then the numbers. A number's key flips its sign bit when it is positive and all of
/// its bits when it is negative, which orders the bit patterns as the numbers, and keeps every
/// number's key above [`NAN_KEY`].
fn order_keys(column: &Column) -> Result<Vec<u64>> {
    let doubles = column.cast(&DataType::Float64)?;
    let doubles = doubles.as_materialized_series().rechunk();
    let array = doubles
        .f64()?
        .downcast_iter()
        .next()
        .cloned()
        .ok_or_else(|| Error::Query("an empty join column has no values".to_string()))?;
    let (values, validity) = (array.values().as_slice(), array.validity());
    Ok(fill(values.len(), |start, out| {
        for (offset, slot) in out.iter_mut().enumerate() {
            let row = start + offset;
            let valid = validity.is_none_or(|validity| validity.get_bit(row));
            let value = values[row];
            slot.write(if !valid {
                NA_KEY
            } else if value.is_nan() {
                NAN_KEY
            } else {
                // Adding zero turns `-0.0` into `0.0`, so the two compare equal.
                let bits = (value + 0.0).to_bits();
                if bits >> 63 == 1 {
                    !bits
                } else {
                    bits | (1 << 63)
                }
            });
        }
    }))
}

/// The positions of a group's sorted `(key, row)` pairs whose keys meet `op` against `value`.
fn key_range(slice: &[(u64, u32)], op: Comparison, value: u64) -> (usize, usize) {
    let below = |bound: u64| slice.partition_point(|(key, _)| *key < bound);
    let through = |bound: u64| slice.partition_point(|(key, _)| *key <= bound);
    if value <= NAN_KEY {
        return if inclusive(op) {
            (below(value), through(value))
        } else {
            (0, 0)
        };
    }
    let numbers = NAN_KEY + 1;
    match op {
        Comparison::Equal => (below(value), through(value)),
        Comparison::GreaterEqual => (below(value), slice.len()),
        Comparison::Greater => (through(value), slice.len()),
        Comparison::LessEqual => (below(numbers), through(value)),
        Comparison::Less => (below(numbers), below(value)),
    }
}

/// Whether the order keys `x` and `i` meet `x op i`. A missing value meets only the same missing
/// value, and only through an operator that includes equality.
fn meets(op: Comparison, x: u64, i: u64) -> bool {
    if x <= NAN_KEY || i <= NAN_KEY {
        return inclusive(op) && x == i;
    }
    match op {
        Comparison::Equal => x == i,
        Comparison::Less => x < i,
        Comparison::LessEqual => x <= i,
        Comparison::Greater => x > i,
        Comparison::GreaterEqual => x >= i,
    }
}

fn inclusive(op: Comparison) -> bool {
    matches!(
        op,
        Comparison::Equal | Comparison::LessEqual | Comparison::GreaterEqual
    )
}
