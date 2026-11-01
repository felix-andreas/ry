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

use polars::prelude::{Column, DataFrame, DataType, PlSmallStr};
use polars_arrow::bitmap::Bitmap;
use polars_core::runtime::THREAD_POOL;
use rayon::prelude::*;

use crate::convert::factor_levels;
use crate::kernel::{
    CodeValues, Codes, PARALLEL_ROWS, Shared, Vector, fill, gather_column, pack_map,
};
use crate::query::{I_PREFIX, Join, Mult, Roll};
use crate::{Result, Table};

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
    on: &[(PlSmallStr, PlSmallStr)],
) -> Result<Option<DataFrame>> {
    let (x_frame, i_frame) = (x.frame(), join.other.frame());
    let (x_height, i_height) = (x_frame.height(), i_frame.height());
    if on.is_empty() || u32::try_from(x_height).is_err() || u32::try_from(i_height).is_err() {
        return Ok(None);
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

fn key_codes(
    x: &DataFrame,
    i: &DataFrame,
    on: &[(PlSmallStr, PlSmallStr)],
) -> Result<Option<Keys>> {
    let mut keys: Option<Keys> = None;
    for (x_column, i_column) in on {
        // A missing column is left to the plan, which reports it.
        let (Ok(x_column), Ok(i_column)) = (x.column(x_column), i.column(i_column)) else {
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
    on: &[(PlSmallStr, PlSmallStr)],
    direction: Direction,
    limit: Option<f64>,
) -> Result<Option<Matches>> {
    let Some(((x_roll, i_roll), groups)) = on.split_last() else {
        return Ok(None);
    };
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

/// The joined frame in data.table's layout: `x`'s columns, with each join column carrying `i`'s
/// values, and then `i`'s other columns, prefixed with `i.` where `x` has the same name.
fn assemble(
    x: &DataFrame,
    i: &DataFrame,
    on: &[(PlSmallStr, PlSmallStr)],
    matches: &Matches,
) -> Result<DataFrame> {
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
    let mut sources: Vec<(&Column, bool, PlSmallStr)> = Vec::with_capacity(x.width() + i.width());
    for column in x.columns() {
        match on.iter().find(|(x_column, _)| x_column == column.name()) {
            Some((_, i_column)) => sources.push((i.column(i_column)?, true, column.name().clone())),
            None => sources.push((column, false, column.name().clone())),
        }
    }
    for column in i.columns() {
        if on.iter().any(|(_, i_column)| i_column == column.name()) {
            continue;
        }
        let name = if x.column(column.name()).is_ok() {
            PlSmallStr::from(format!("{I_PREFIX}{}", column.name()))
        } else {
            column.name().clone()
        };
        sources.push((column, true, name));
    }
    // Each gather is parallel over rows, so the columns go one at a time.
    let columns = sources
        .into_iter()
        .map(|(column, from_i, name)| {
            if from_i {
                gather_column(column, &matches.i_rows, None, name)
            } else {
                gather_column(column, &x_rows, keep.as_ref(), name)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(DataFrame::new(height, columns)?)
}
