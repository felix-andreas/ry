//! Factors: a dense column of level codes plus one shared table of level labels.
//!
//! A factor column is a Polars extension column whose storage holds zero-based codes in the
//! narrowest unsigned type that fits every level (`u8`, `u16` or `u32`) and whose type carries
//! the level table, [`Levels`], behind an `Arc`. Every row-level operation Polars runs on the
//! column (filter, take, slice, sort, group, join, concatenation) runs on the codes alone, and the
//! level table travels with the dtype without being copied.
//!
//! Polars' own `Enum` keeps a hash map, an ordered list and an interned string array per level,
//! about 135 bytes for a short label. A [`Levels`] table keeps its labels in byte order and
//! writes each as the bytes it does not share with the label before it, and it finds a label by
//! binary search, so it holds no index beside the labels.

use {
    crate::{
        Error, Result,
        kernel::{CodeValues, Codes, Shared, gather_values},
    },
    foldhash::fast::FixedState,
    hashbrown::HashTable,
    polars::prelude::{
        Column, DataType, Expr, Field, IntoColumn, IntoSeries, PlSmallStr, PolarsError, Series,
        StringChunked, UInt8Chunked, UInt16Chunked, UInt32Chunked,
    },
    polars_arrow::{
        array::{Array, MutableBinaryViewArray, PrimitiveArray, Utf8ViewArray, View},
        bitmap::{Bitmap, MutableBitmap},
        datatypes::ArrowDataType,
    },
    polars_buffer::Buffer,
    polars_core::datatypes::extension::{ExtensionTypeImpl, ExtensionTypeInstance},
    rayon::prelude::*,
    std::{any::Any, borrow::Cow, cmp::Ordering, hash::BuildHasher, sync::Arc},
};

/// A factor's levels: distinct labels, each named by one code.
///
/// The labels are stored in byte order and front-coded. A label's entry gives the number of
/// leading bytes it shares with the label before it and then the bytes that follow them, so a
/// column of identifiers costs a few bytes a level where a string view costs 16. Every
/// [`BLOCK`]-th label shares nothing and its entry's position is recorded, so a lookup searches
/// those whole labels and then reads at most one block.
#[derive(Default)]
pub struct Levels {
    /// The entries in byte order of their labels. An entry is a header byte, the lengths the
    /// header could not hold, and the label's bytes after the shared ones. The header's high four
    /// bits are the shared length and its low four bits the length of the rest; a length of 15 or
    /// more is written as 15, with the excess following the header as a varint.
    bytes: Vec<u8>,
    /// Where each block's first entry starts in `bytes`.
    blocks: Vec<u64>,
    len: usize,
    /// The levels' order when it is not byte order, which a factor built from labels in a chosen
    /// order has. Without it, a label's code is its rank in byte order.
    order: Option<Order>,
}

/// The labels in a block: the most a lookup reads after its binary search, and the interval at
/// which a label is written whole.
const BLOCK: usize = 16;

/// A level order that differs from byte order, as both directions of one permutation, because a
/// row's label is found from its code and a label's code from its rank.
#[derive(PartialEq)]
struct Order {
    rank_of: Vec<u32>,
    code_of: Vec<u32>,
}

impl Levels {
    /// The levels named by `labels` in order, keeping the first of any repeated label.
    pub fn from_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> Levels {
        let mut seen: HashTable<u32> = HashTable::new();
        let mut kept: Vec<&str> = Vec::new();
        let state = FixedState::default();
        for label in labels {
            let hash = state.hash_one(label);
            if seen
                .find(hash, |index| kept[*index as usize] == label)
                .is_none()
            {
                seen.insert_unique(hash, kept.len() as u32, |index| {
                    state.hash_one(kept[*index as usize])
                });
                kept.push(label);
            }
        }
        let mut code_of: Vec<u32> = (0..kept.len() as u32).collect();
        code_of.sort_unstable_by_key(|code| kept[*code as usize].as_bytes());
        let mut levels = Levels::default();
        let mut previous: &[u8] = &[];
        for code in &code_of {
            let label = kept[*code as usize].as_bytes();
            levels.push(previous, label);
            previous = label;
        }
        let sorted = code_of
            .iter()
            .enumerate()
            .all(|(rank, code)| rank == *code as usize);
        if !sorted {
            let mut rank_of = vec![0u32; code_of.len()];
            for (rank, code) in code_of.iter().enumerate() {
                rank_of[*code as usize] = rank as u32;
            }
            levels.order = Some(Order { rank_of, code_of });
        }
        levels
    }

    /// The table of several tables' labels, where every label of a piece sorts after the labels
    /// of the pieces before it and every piece but the last holds whole blocks. A column's
    /// labels are encoded in such pieces in parallel.
    fn joined(pieces: Vec<Levels>) -> Levels {
        let mut levels = Levels::default();
        levels
            .bytes
            .reserve_exact(pieces.iter().map(|piece| piece.bytes.len()).sum());
        levels
            .blocks
            .reserve_exact(pieces.iter().map(|piece| piece.blocks.len()).sum());
        // Each piece is freed as soon as it is copied, so the pieces and the table together
        // never hold much more than the table's own size.
        for piece in pieces {
            let base = levels.bytes.len() as u64;
            levels
                .blocks
                .extend(piece.blocks.iter().map(|start| start + base));
            levels.bytes.extend_from_slice(&piece.bytes);
            levels.len += piece.len;
        }
        levels
    }

    /// Appends `label`, which must sort after `previous`, the label appended before it.
    fn push(&mut self, previous: &[u8], label: &[u8]) {
        let shared = if self.len.is_multiple_of(BLOCK) {
            self.blocks.push(self.bytes.len() as u64);
            0
        } else {
            previous
                .iter()
                .zip(label)
                .take_while(|(before, byte)| before == byte)
                .count()
        };
        let rest = label.len() - shared;
        self.bytes
            .push(((shared.min(15) as u8) << 4) | rest.min(15) as u8);
        for length in [shared, rest] {
            if let Some(mut excess) = length.checked_sub(15) {
                while excess >= 0x80 {
                    self.bytes.push(excess as u8 | 0x80);
                    excess >>= 7;
                }
                self.bytes.push(excess as u8);
            }
        }
        self.bytes.extend_from_slice(&label[shared..]);
        self.len += 1;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The label of `code`, read from the start of its block.
    pub fn get(&self, code: u32) -> Option<String> {
        let mut cursor = Cursor::new(self);
        cursor.seek(self.rank(code)?);
        cursor.current().map(string)
    }

    /// The code of `label`: a binary search among the blocks' first labels, then a read of the
    /// one block that can hold it.
    pub fn position(&self, label: &str) -> Option<u32> {
        self.find(label.as_bytes())
    }

    /// Every label, in the order of the codes.
    pub fn to_strings(&self) -> Vec<String> {
        let mut strings = vec![String::new(); self.len];
        let mut cursor = Cursor::new(self);
        cursor.seek(0);
        while let Some(label) = cursor.current() {
            strings[self.code(cursor.rank) as usize] = string(label);
            cursor.advance();
        }
        strings
    }

    /// For each code of this table, the code `to` gives the same label, or `absent` when `to`
    /// lacks the label. Both tables are sorted, so one pass over the two matches every label; a
    /// table much smaller than `to` looks its labels up one by one, which reads less of `to`.
    pub fn translation(&self, to: &Levels, absent: u32) -> Vec<u32> {
        let mut translation = vec![absent; self.len];
        let mut own = Cursor::new(self);
        own.seek(0);
        if self.len.saturating_mul(2 * BLOCK) < to.len {
            while let Some(label) = own.current() {
                if let Some(code) = to.find(label) {
                    translation[self.code(own.rank) as usize] = code;
                }
                own.advance();
            }
            return translation;
        }
        let mut other = Cursor::new(to);
        other.seek(0);
        while let (Some(label), Some(candidate)) = (own.current(), other.current()) {
            match label.cmp(candidate) {
                Ordering::Less => own.advance(),
                Ordering::Greater => other.advance(),
                Ordering::Equal => {
                    translation[self.code(own.rank) as usize] = to.code(other.rank);
                    own.advance();
                    other.advance();
                }
            }
        }
        translation
    }

    fn find(&self, label: &[u8]) -> Option<u32> {
        let block = self
            .blocks
            .partition_point(|start| self.first(*start) <= label)
            .checked_sub(1)?;
        let mut cursor = Cursor::new(self);
        cursor.seek(block * BLOCK);
        for _ in 0..BLOCK {
            match cursor.current()?.cmp(label) {
                Ordering::Less => cursor.advance(),
                Ordering::Equal => return Some(self.code(cursor.rank)),
                Ordering::Greater => return None,
            }
        }
        None
    }

    /// The first label of the block whose entry starts at `start`. It shares nothing with the
    /// label before it, so its entry holds it whole.
    fn first(&self, start: u64) -> &[u8] {
        let start = start as usize;
        let (length, at) = read_length(&self.bytes, start + 1, self.bytes[start] & 15);
        &self.bytes[at..at + length]
    }

    fn rank(&self, code: u32) -> Option<usize> {
        match &self.order {
            Some(order) => order.rank_of.get(code as usize).map(|rank| *rank as usize),
            None => ((code as usize) < self.len).then_some(code as usize),
        }
    }

    fn code(&self, rank: usize) -> u32 {
        match &self.order {
            Some(order) => order.code_of[rank],
            None => rank as u32,
        }
    }

    /// Every label as a string array, in the order of the codes. The labels are decoded in
    /// parallel, a run of whole blocks to a task, and each task's labels too long for a view to
    /// hold inline share one buffer.
    fn labels(&self) -> Result<Utf8ViewArray> {
        const TASK: usize = BLOCK << 12;
        let mut views = vec![View::default(); self.len];
        let buffers = views
            .par_chunks_mut(TASK)
            .enumerate()
            .map(|(task, views)| {
                let mut long: Vec<u8> = Vec::new();
                let mut cursor = Cursor::new(self);
                cursor.seek(task * TASK);
                for view in views {
                    let Some(label) = cursor.current() else {
                        break;
                    };
                    *view = if label.len() <= View::MAX_INLINE_SIZE as usize {
                        View::new_inline(label)
                    } else {
                        let offset = u32::try_from(long.len())
                            .ok()
                            .filter(|offset| {
                                u32::try_from(label.len())
                                    .is_ok_and(|len| offset.checked_add(len).is_some())
                            })
                            .ok_or_else(|| Error::Query("a factor's labels are too long".into()))?;
                        long.extend_from_slice(label);
                        View::new_from_bytes(label, task as u32, offset)
                    };
                    cursor.advance();
                }
                Ok(Buffer::from(long))
            })
            .collect::<Result<Vec<Buffer<u8>>>>()?;
        let views: Buffer<View> = match &self.order {
            Some(order) => gather_values(&views, &order.rank_of),
            None => views.into(),
        };
        // SAFETY: every view is inline or points into the buffer of the task that wrote it, and
        // every label is valid UTF-8 because the table was built from strings.
        Ok(unsafe {
            Utf8ViewArray::new_unchecked_unknown_md(
                ArrowDataType::Utf8View,
                views,
                buffers.into(),
                None,
                None,
            )
        })
    }

    /// Whether both tables give every code the same label. Equal label sets encode to equal
    /// bytes, so the labels are compared without being decoded.
    fn same(&self, other: &Levels) -> bool {
        std::ptr::eq(self, other)
            || (self.len == other.len && self.order == other.order && self.bytes == other.bytes)
    }
}

/// A position among a level table's labels in byte order, holding the label it is on. Reading
/// the next label rewrites the end of the held one, which is how front-coded labels decode.
struct Cursor<'a> {
    levels: &'a Levels,
    /// The rank of the held label, or the level count when the cursor is on none.
    rank: usize,
    /// Where the entry after the held label starts.
    next: usize,
    label: Vec<u8>,
}

impl<'a> Cursor<'a> {
    /// A cursor on no label, until [`Cursor::seek`] moves it to one.
    fn new(levels: &'a Levels) -> Cursor<'a> {
        Cursor {
            levels,
            rank: levels.len,
            next: 0,
            label: Vec::new(),
        }
    }

    fn current(&self) -> Option<&[u8]> {
        (self.rank < self.levels.len).then_some(self.label.as_slice())
    }

    fn advance(&mut self) {
        self.rank += 1;
        if self.rank < self.levels.len {
            self.next = read_entry(&self.levels.bytes, self.next, &mut self.label);
        }
    }

    /// Moves to the label of `rank`. A label further on in the cursor's own block is reached by
    /// reading on, and any other from the first label of its block.
    fn seek(&mut self, rank: usize) {
        let levels = self.levels;
        if rank >= levels.len {
            self.rank = levels.len;
            return;
        }
        let ahead = self.rank <= rank && self.rank / BLOCK == rank / BLOCK;
        if !ahead {
            let block = rank / BLOCK;
            self.rank = block * BLOCK;
            self.next = read_entry(
                &levels.bytes,
                levels.blocks[block] as usize,
                &mut self.label,
            );
        }
        while self.rank < rank {
            self.advance();
        }
    }
}

/// Reads the entry at `at` into `label`, which holds the label of the entry before it, and
/// returns where the next entry starts.
fn read_entry(bytes: &[u8], at: usize, label: &mut Vec<u8>) -> usize {
    let header = bytes[at];
    let (shared, at) = read_length(bytes, at + 1, header >> 4);
    let (rest, at) = read_length(bytes, at, header & 15);
    label.truncate(shared);
    label.extend_from_slice(&bytes[at..at + rest]);
    at + rest
}

/// A length whose half of an entry's header is `nibble`, with its excess at `at` when the header
/// could not hold it. Returns the length and where the bytes after it start.
fn read_length(bytes: &[u8], mut at: usize, nibble: u8) -> (usize, usize) {
    let mut length = nibble as usize;
    if nibble == 15 {
        let mut shift = 0;
        loop {
            let byte = bytes[at];
            at += 1;
            length += ((byte & 0x7f) as usize) << shift;
            shift += 7;
            if byte < 0x80 {
                break;
            }
        }
    }
    (length, at)
}

/// A label as a string. Every label was built from a string, so the conversion loses nothing.
fn string(label: &[u8]) -> String {
    String::from_utf8_lossy(label).into_owned()
}

/// The extension type of a factor column.
struct FactorType(Arc<Levels>);

impl ExtensionTypeImpl for FactorType {
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed("ry.factor")
    }

    fn serialize_metadata(&self) -> Option<Cow<'_, str>> {
        None
    }

    fn dyn_clone(&self) -> Box<dyn ExtensionTypeImpl> {
        Box::new(FactorType(self.0.clone()))
    }

    fn dyn_eq(&self, other: &dyn ExtensionTypeImpl) -> bool {
        (other as &dyn Any)
            .downcast_ref::<FactorType>()
            .is_some_and(|other| self.0.same(&other.0))
    }

    /// Hashes the level count and the first entries, which equal level tables share, without
    /// reading every label of a large table.
    fn dyn_hash(&self) -> u64 {
        let levels = &self.0;
        let head = &levels.bytes[..levels.bytes.len().min(64)];
        FixedState::default().hash_one((levels.len, levels.bytes.len(), head))
    }

    fn dyn_display(&self) -> Cow<'_, str> {
        Cow::Borrowed("factor")
    }

    fn dyn_debug(&self) -> Cow<'_, str> {
        Cow::Owned(format!("Factor({} levels)", self.0.len()))
    }
}

/// A factor column's levels, read from its dtype.
pub fn levels(dtype: &DataType) -> Option<&Arc<Levels>> {
    match dtype {
        DataType::Extension(typ, _) => (&*typ.0 as &dyn Any)
            .downcast_ref::<FactorType>()
            .map(|factor| &factor.0),
        _ => None,
    }
}

pub fn is_factor(dtype: &DataType) -> bool {
    levels(dtype).is_some()
}

/// The dtype of a factor over `levels`, stored in the narrowest code type that fits them.
pub fn dtype(levels: Arc<Levels>) -> DataType {
    let storage = storage(levels.len());
    DataType::Extension(
        ExtensionTypeInstance(Box::new(FactorType(levels))),
        Box::new(storage),
    )
}

fn storage(levels: usize) -> DataType {
    if levels <= 1 << 8 {
        DataType::UInt8
    } else if levels <= 1 << 16 {
        DataType::UInt16
    } else {
        DataType::UInt32
    }
}

/// A factor column over `levels` whose rows have the zero-based `codes`, each below the level
/// count. A row `validity` marks missing may hold any code.
pub fn from_codes(
    name: PlSmallStr,
    levels: Arc<Levels>,
    codes: Vec<u32>,
    validity: Option<Bitmap>,
) -> Series {
    let dtype = dtype(levels);
    let DataType::Extension(typ, storage) = dtype else {
        unreachable!("a factor's dtype is an extension type");
    };
    let codes = match *storage {
        DataType::UInt8 => UInt8Chunked::with_chunk(
            name,
            PrimitiveArray::new(
                ArrowDataType::UInt8,
                codes
                    .par_iter()
                    .map(|code| *code as u8)
                    .collect::<Vec<_>>()
                    .into(),
                validity,
            ),
        )
        .into_series(),
        DataType::UInt16 => UInt16Chunked::with_chunk(
            name,
            PrimitiveArray::new(
                ArrowDataType::UInt16,
                codes
                    .par_iter()
                    .map(|code| *code as u16)
                    .collect::<Vec<_>>()
                    .into(),
                validity,
            ),
        )
        .into_series(),
        _ => UInt32Chunked::with_chunk(
            name,
            PrimitiveArray::new(ArrowDataType::UInt32, codes.into(), validity),
        )
        .into_series(),
    };
    codes.into_extension(typ)
}

/// A factor column whose codes were gathered from a factor of `dtype`, so they already fit it.
pub(crate) fn from_gathered(name: PlSmallStr, dtype: &DataType, codes: Codes) -> Series {
    let DataType::Extension(typ, _) = dtype else {
        unreachable!("only a factor's codes are gathered as a factor");
    };
    let storage = match codes.values {
        CodeValues::U8(values) => UInt8Chunked::with_chunk(
            name,
            PrimitiveArray::new(ArrowDataType::UInt8, values, codes.validity),
        )
        .into_series(),
        CodeValues::U16(values) => UInt16Chunked::with_chunk(
            name,
            PrimitiveArray::new(ArrowDataType::UInt16, values, codes.validity),
        )
        .into_series(),
        CodeValues::U32(values) => UInt32Chunked::with_chunk(
            name,
            PrimitiveArray::new(ArrowDataType::UInt32, values, codes.validity),
        )
        .into_series(),
    };
    storage.into_extension(typ.clone())
}

/// A column as text: a factor gives each row's label, and any other column is cast to strings.
///
/// A factor's level table is decoded into strings once and the rows share them. A column much
/// shorter than its level table, such as a few rows filtered out of a large one, reads each
/// row's label from the label's block, because decoding the whole table would cost more than
/// the rows do.
pub fn text(column: &Column) -> Result<Column> {
    let Some(levels) = levels(column.dtype()) else {
        return Ok(column.cast(&DataType::String)?);
    };
    let Some(codes) = Codes::of(column) else {
        return Err(Error::Query(format!(
            "the factor `{}` has unreadable codes",
            column.name()
        )));
    };
    let rows = column.len();
    if rows.saturating_mul(BLOCK) < levels.len() {
        const TASK: usize = 1 << 14;
        let valid = |row: usize| {
            codes
                .validity
                .as_ref()
                .is_none_or(|validity| validity.get_bit(row))
        };
        let code = |row: usize| match &codes.values {
            CodeValues::U8(values) => u32::from(values[row]),
            CodeValues::U16(values) => u32::from(values[row]),
            CodeValues::U32(values) => values[row],
        };
        let pieces: Vec<Utf8ViewArray> = (0..rows.div_ceil(TASK))
            .into_par_iter()
            .map(|task| {
                let start = task * TASK;
                let end = (start + TASK).min(rows);
                let mut strings = MutableBinaryViewArray::<str>::with_capacity(end - start);
                let mut cursor = Cursor::new(levels);
                for row in start..end {
                    let rank = valid(row).then(|| levels.rank(code(row))).flatten();
                    cursor.seek(rank.unwrap_or(levels.len()));
                    match cursor.current() {
                        Some(label) => strings.push_value(String::from_utf8_lossy(label)),
                        None => strings.push_null(),
                    }
                }
                strings.freeze()
            })
            .collect();
        return Ok(StringChunked::from_chunk_iter(column.name().clone(), pieces).into_column());
    }
    let labels = levels.labels()?;
    let views = labels.views();
    let views: Buffer<View> = match &codes.values {
        CodeValues::U8(values) => gather_views(views, values),
        CodeValues::U16(values) => gather_views(views, values),
        CodeValues::U32(values) => gather_views(views, values),
    };
    // SAFETY: every gathered view is a view of `labels`, so it stays valid over its buffers.
    let gathered = unsafe {
        Utf8ViewArray::new_unchecked_unknown_md(
            labels.dtype().clone(),
            views,
            labels.data_buffers().clone(),
            codes.validity,
            Some(labels.total_buffer_len()),
        )
    };
    Ok(StringChunked::with_chunk(column.name().clone(), gathered).into_column())
}

fn gather_views<T: Copy + Into<u32> + Send + Sync>(views: &[View], codes: &[T]) -> Buffer<View> {
    let Some(last) = views.len().checked_sub(1) else {
        return vec![View::default(); codes.len()].into();
    };
    // A missing row's code may be any value, so it is clamped to a level; its validity bit hides
    // the label.
    let rows: Vec<u32> = codes
        .par_iter()
        .map(|code| (*code).into().min(last as u32))
        .collect();
    gather_values(views, &rows)
}

/// `expr` as text inside a lazy plan: [`text`] applied to whatever column it evaluates to.
pub fn text_expr(expr: Expr) -> Expr {
    expr.map(
        |column| text(&column).map_err(polars_error),
        |_, field| Ok(Field::new(field.name().clone(), DataType::String)),
    )
}

/// A column converted to `dtype`. A factor converts through its labels, and a column becomes a
/// factor by looking its labels up among the target's levels, because Polars cannot cast an
/// extension column.
pub fn cast(column: &Column, dtype: &DataType) -> Result<Column> {
    if column.dtype() == dtype {
        return Ok(column.clone());
    }
    if let Some(levels) = levels(dtype) {
        return Ok(recode(column, levels)?.into_column());
    }
    match is_factor(column.dtype()) {
        true => Ok(text(column)?.cast(dtype)?),
        false => Ok(column.cast(dtype)?),
    }
}

/// [`cast`] inside a lazy plan.
pub fn cast_expr(expr: Expr, dtype: DataType) -> Expr {
    let output = dtype.clone();
    expr.map(
        move |column| cast(&column, &dtype).map_err(polars_error),
        move |_, field| Ok(Field::new(field.name().clone(), output.clone())),
    )
}

fn polars_error(error: Error) -> PolarsError {
    match error {
        Error::Polars(error) => error,
        error => PolarsError::ComputeError(error.to_string().into()),
    }
}

/// A column as a factor over `levels`: each row's label is looked up among them, and a label
/// they lack becomes `NA`. A factor already over `levels` is returned as it is.
///
/// The lookup runs once per distinct label, not once per row: a factor's level table is matched
/// against `levels`, and any other column becomes a factor of its own distinct strings first.
pub fn recode(column: &Column, levels: &Arc<Levels>) -> Result<Series> {
    let series = match self::levels(column.dtype()) {
        Some(own) if own.same(levels) => return Ok(column.as_materialized_series().clone()),
        Some(_) => column.as_materialized_series().clone(),
        None => from_strings(column.as_materialized_series())?,
    };
    let Some(own) = self::levels(series.dtype()) else {
        unreachable!("the series is a factor");
    };
    const ABSENT: u32 = u32::MAX;
    let translation = own.translation(levels, ABSENT);
    let own_codes = series.to_physical_repr().cast(&DataType::UInt32)?;
    let mut validity = MutableBitmap::with_capacity(series.len());
    let codes: Vec<u32> = own_codes
        .u32()?
        .iter()
        .map(|code| {
            let code = code
                .and_then(|code| translation.get(code as usize).copied())
                .filter(|code| *code != ABSENT);
            validity.push(code.is_some());
            code.unwrap_or(0)
        })
        .collect();
    Ok(from_codes(
        series.name().clone(),
        levels.clone(),
        codes,
        validity.into(),
    ))
}

/// A string column as a factor with its distinct values as levels, sorted in byte order, which
/// is what `factor(x)` gives in R's C locale.
pub fn from_strings(series: &Series) -> Result<Series> {
    let strings = series.cast(&DataType::String)?;
    let mut interner = Interner::new(strings.len());
    for array in strings.str()?.downcast_iter() {
        interner.intern(array)?;
    }
    Ok(interner.finish(series.name().clone()))
}

const PARTITION_BITS: u32 = 8;
const PARTITIONS: usize = 1 << PARTITION_BITS;

/// Numbers a column's strings as they arrive in pieces, such as the slices of a file being read,
/// and turns them into a factor at the end.
///
/// A column can hold tens of millions of distinct strings, and then every random access into one
/// big hash table misses the cache. So each piece's rows are hashed in parallel and split by hash
/// into partitions, and each partition numbers its own strings in a table small enough to stay
/// warm. A partition holds a 16-byte view per string and copies only the strings a view cannot
/// hold inline into its own byte arena.
pub(crate) struct Interner {
    partitions: Vec<Partition>,
    /// Per row, the partition in the low bits and the string's index within the partition above
    /// them, until [`Interner::finish`] renumbers them into level order.
    codes: Vec<u32>,
    validity: Option<MutableBitmap>,
    /// The rows the whole column is expected to have, which sizes the partitions once the first
    /// piece shows how many distinct strings a row brings.
    expected_rows: usize,
}

#[derive(Default)]
struct Partition {
    table: HashTable<u32>,
    views: Vec<View>,
    arena: Vec<u8>,
}

impl Partition {
    fn label(&self, index: u32) -> &[u8] {
        label(&self.views, &self.arena, index)
    }

    fn intern(&mut self, partition: u32, value: &[u8], hash: u64) -> Result<u32> {
        let state = FixedState::default();
        let Partition {
            table,
            views,
            arena,
        } = self;
        let found = table.find(hash, |index| {
            let view = &views[*index as usize];
            view.length as usize == value.len()
                && match view.get_inlined_slice() {
                    Some(bytes) => bytes == value,
                    None => {
                        let start = view.offset as usize;
                        &arena[start..start + value.len()] == value
                    }
                }
        });
        if let Some(index) = found {
            return Ok(*index);
        }
        let index = u32::try_from(views.len())
            .ok()
            .filter(|index| *index < u32::MAX >> PARTITION_BITS)
            .ok_or_else(|| Error::Query("a factor has too many levels".into()))?;
        let view = if value.len() <= View::MAX_INLINE_SIZE as usize {
            View::new_inline(value)
        } else {
            let offset = u32::try_from(arena.len())
                .ok()
                .filter(|offset| {
                    u32::try_from(value.len()).is_ok_and(|len| offset.checked_add(len).is_some())
                })
                .ok_or_else(|| Error::Query("a factor's labels are too long".into()))?;
            arena.extend_from_slice(value);
            View::new_from_bytes(value, partition, offset)
        };
        views.push(view);
        table.insert_unique(hash, index, |index| {
            state.hash_one(label(views, arena, *index))
        });
        Ok(index)
    }

    /// Makes room for `levels` labels in all, so that a column of mostly distinct strings neither
    /// rehashes its table nor copies its views as they grow. A reservation the column does not
    /// fill still costs the table's control bytes, one per slot, which are written when the
    /// table is allocated; the views beyond the filled ones are never paged in.
    fn reserve(&mut self, levels: usize) {
        let state = FixedState::default();
        let Partition {
            table,
            views,
            arena,
        } = self;
        let more = levels.saturating_sub(views.len());
        views.reserve(more);
        table.reserve(more, |index| state.hash_one(label(views, arena, *index)));
    }
}

fn label<'a>(views: &'a [View], arena: &'a [u8], index: u32) -> &'a [u8] {
    let view = &views[index as usize];
    match view.get_inlined_slice() {
        Some(bytes) => bytes,
        None => {
            let start = view.offset as usize;
            &arena[start..start + view.length as usize]
        }
    }
}

impl Interner {
    pub(crate) fn new(rows: usize) -> Interner {
        Interner {
            partitions: (0..PARTITIONS).map(|_| Partition::default()).collect(),
            codes: Vec::with_capacity(rows),
            validity: None,
            expected_rows: rows,
        }
    }

    pub(crate) fn intern(&mut self, array: &Utf8ViewArray) -> Result<()> {
        let rows = array.len();
        let base = self.codes.len();
        self.codes.resize(base + rows, 0);
        match (self.validity.as_mut(), array.validity()) {
            (Some(bits), Some(valid)) => bits.extend_from_bitmap(valid),
            (Some(bits), None) => bits.extend_constant(rows, true),
            (None, Some(valid)) if valid.unset_bits() > 0 => {
                let mut bits = MutableBitmap::with_capacity(self.codes.capacity());
                bits.extend_constant(base, true);
                bits.extend_from_bitmap(valid);
                self.validity = Some(bits);
            }
            (None, _) => {}
        }
        let state = FixedState::default();
        let valid = |row: usize| array.validity().is_none_or(|bits| bits.get_bit(row));
        let hashes: Vec<u64> = (0..rows)
            .into_par_iter()
            .map(|row| match valid(row) {
                true => state.hash_one(array.value(row).as_bytes()),
                false => 0,
            })
            .collect();
        // hashbrown tags an entry with the hash's top seven bits and finds its bucket with the
        // low bits, so the partition comes from bits that neither uses.
        let partition_of = |hash: u64| ((hash >> 40) as usize) & (PARTITIONS - 1);
        let mut starts = [0usize; PARTITIONS + 1];
        for (row, hash) in hashes.iter().enumerate() {
            if valid(row) {
                starts[partition_of(*hash) + 1] += 1;
            }
        }
        for partition in 0..PARTITIONS {
            starts[partition + 1] += starts[partition];
        }
        let mut order = vec![0u32; starts[PARTITIONS]];
        let mut next = starts;
        for (row, hash) in hashes.iter().enumerate() {
            if valid(row) {
                let slot = &mut next[partition_of(*hash)];
                order[*slot] = row as u32;
                *slot += 1;
            }
        }
        let codes = Shared(self.codes[base..].as_mut_ptr());
        let expected_rows = self.expected_rows;
        self.partitions
            .par_iter_mut()
            .enumerate()
            .try_for_each(|(partition, owned)| {
                for row in &order[starts[partition]..starts[partition + 1]] {
                    let row = *row as usize;
                    let index =
                        owned.intern(partition as u32, array.value(row).as_bytes(), hashes[row])?;
                    // SAFETY: `row` is within the piece, and no other partition owns it.
                    unsafe { codes.write(row, (index << PARTITION_BITS) | partition as u32) };
                }
                // A column whose first piece is mostly distinct strings is sized for the whole
                // file now, scaled from the piece, because growing it later copies and rehashes
                // every label several times and holds the old and new tables at once.
                if base == 0 && rows > 0 && owned.views.len() * 4 > rows / PARTITIONS {
                    owned.reserve(owned.views.len() * expected_rows.div_ceil(rows));
                }
                Ok(())
            })
    }

    /// The factor of every row interned so far, with its levels sorted in byte order. The strings
    /// sort by a key of their first 12 bytes and their interned code, and only runs of strings
    /// that share those 12 bytes are sorted again by their whole bytes.
    ///
    /// The sorted keys give each interned code its rank, which renumbers the rows, and then the
    /// labels are written into the level table in key order. The most the method holds beyond
    /// the codes is the partitions' views, the keys and one rank per level; the level table is
    /// written after the ranks are freed, and the views and keys are freed once it is.
    pub(crate) fn finish(self, name: PlSmallStr) -> Series {
        let Interner {
            mut partitions,
            mut codes,
            validity,
            ..
        } = self;
        for partition in &mut partitions {
            partition.table = HashTable::new();
        }
        let partitions = partitions;
        let total = partitions.iter().map(|owned| owned.views.len()).sum();
        let mut keys = vec![View::default(); total];
        let mut rest = keys.as_mut_slice();
        let mut chunks = Vec::with_capacity(PARTITIONS);
        for owned in &partitions {
            let (chunk, tail) = rest.split_at_mut(owned.views.len());
            chunks.push(chunk);
            rest = tail;
        }
        // The key's first three words are the label's first 12 bytes read big-endian, so keys
        // order as their labels' prefixes do, and the last word is the interned code.
        chunks
            .into_par_iter()
            .zip(partitions.par_iter())
            .enumerate()
            .for_each(|(partition, (chunk, owned))| {
                for (index, slot) in chunk.iter_mut().enumerate() {
                    let label = owned.label(index as u32);
                    let mut prefix = [0u8; 12];
                    let take = label.len().min(12);
                    prefix[..take].copy_from_slice(&label[..take]);
                    let word = |at: usize| {
                        u32::from_be_bytes([
                            prefix[at],
                            prefix[at + 1],
                            prefix[at + 2],
                            prefix[at + 3],
                        ])
                    };
                    *slot = View {
                        length: word(0),
                        prefix: word(4),
                        buffer_idx: word(8),
                        offset: ((index as u32) << PARTITION_BITS) | partition as u32,
                    };
                }
            });
        let order = |key: &View| (key.length, key.prefix, key.buffer_idx, key.offset);
        keys.par_sort_unstable_by_key(order);
        let label_of = |key: &View| {
            let owned = &partitions[(key.offset as usize) & (PARTITIONS - 1)];
            owned.label(key.offset >> PARTITION_BITS)
        };
        let prefix_of = |key: &View| (key.length, key.prefix, key.buffer_idx);
        keys.par_chunk_by_mut(|before, key| prefix_of(before) == prefix_of(key))
            .for_each(|run| {
                if run.len() > 1 {
                    run.par_sort_unstable_by(|a, b| label_of(a).cmp(label_of(b)));
                }
            });
        let span = partitions
            .iter()
            .map(|owned| owned.views.len())
            .max()
            .unwrap_or(0)
            << PARTITION_BITS;
        let mut rank = vec![0u32; span];
        let ranks = Shared(rank.as_mut_ptr());
        keys.par_iter().enumerate().for_each(|(position, key)| {
            // SAFETY: an interned code is below `span`, and no two keys hold the same one.
            unsafe { ranks.write(key.offset as usize, position as u32) };
        });
        let valid = validity.as_ref();
        codes.par_iter_mut().enumerate().for_each(|(row, code)| {
            *code = match valid.is_none_or(|bits| bits.get(row)) {
                true => rank.get(*code as usize).copied().unwrap_or(0),
                false => 0,
            }
        });
        drop(rank);
        const TASK: usize = BLOCK << 16;
        let pieces: Vec<Levels> = keys
            .par_chunks(TASK)
            .map(|keys| {
                let mut piece = Levels::default();
                let mut previous: &[u8] = &[];
                for key in keys {
                    let label = label_of(key);
                    piece.push(previous, label);
                    previous = label;
                }
                piece
            })
            .collect();
        drop(keys);
        drop(partitions);
        from_codes(
            name,
            Arc::new(Levels::joined(pieces)),
            codes,
            validity.and_then(|bits| bits.into()),
        )
    }
}
