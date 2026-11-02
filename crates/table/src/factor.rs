//! Factors: a dense column of level codes plus one shared table of level labels.
//!
//! A factor column is a Polars extension column whose storage holds zero-based codes in the
//! narrowest unsigned type that fits every level (`u8`, `u16` or `u32`) and whose type carries
//! the level table, [`Levels`], behind an `Arc`. Every row-level operation Polars runs on the
//! column (filter, take, slice, sort, group, join, concatenation) runs on the codes alone, and the
//! level table travels with the dtype without being copied.
//!
//! Polars' own `Enum` keeps a hash map, an ordered list and an interned string array per level,
//! about 135 bytes for a short label. A [`Levels`] table holds one 16-byte string view per level,
//! with labels longer than 12 bytes in a shared byte buffer, and builds its label-to-code index
//! only when something looks a label up.

use {
    crate::{
        Error, Result,
        kernel::{CodeValues, Codes, gather_values},
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
    std::{
        any::Any,
        borrow::Cow,
        hash::BuildHasher,
        sync::{Arc, OnceLock},
    },
};

/// A factor's levels in order. Code `c` names `labels[c]`, and the labels are distinct.
pub struct Levels {
    labels: Utf8ViewArray,
    index: OnceLock<HashTable<u32>>,
}

impl Levels {
    /// The levels whose labels are `labels`, which must be distinct and hold no nulls.
    pub fn new(labels: Utf8ViewArray) -> Levels {
        Levels {
            labels,
            index: OnceLock::new(),
        }
    }

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
        let mut array = MutableBinaryViewArray::<str>::with_capacity(kept.len());
        for label in kept {
            array.push_value(label);
        }
        Levels::new(array.freeze())
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.len() == 0
    }

    pub fn get(&self, code: u32) -> Option<&str> {
        ((code as usize) < self.labels.len()).then(|| self.labels.value(code as usize))
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.labels.values_iter()
    }

    /// The code of `label`. The first lookup builds the label index, which every later lookup
    /// shares.
    pub fn position(&self, label: &str) -> Option<u32> {
        let state = FixedState::default();
        let index = self.index.get_or_init(|| {
            let mut index = HashTable::with_capacity(self.labels.len());
            for (code, label) in self.labels.values_iter().enumerate() {
                index.insert_unique(state.hash_one(label), code as u32, |code| {
                    state.hash_one(self.labels.value(*code as usize))
                });
            }
            index
        });
        index
            .find(state.hash_one(label), |code| {
                self.labels.value(*code as usize) == label
            })
            .copied()
    }

    pub fn labels(&self) -> &Utf8ViewArray {
        &self.labels
    }

    fn same(&self, other: &Levels) -> bool {
        std::ptr::eq(self, other) || (self.len() == other.len() && self.iter().eq(other.iter()))
    }
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

    /// Hashes the level count and the outer labels, which equal level tables share, without
    /// reading every label of a large table.
    fn dyn_hash(&self) -> u64 {
        let state = FixedState::default();
        let levels = &self.0;
        let first = levels.get(0).unwrap_or("");
        let last = levels
            .len()
            .checked_sub(1)
            .and_then(|code| levels.get(code as u32));
        state.hash_one((levels.len(), first, last.unwrap_or("")))
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

/// A column as text: a factor gives each row's label, sharing the level table's string bytes,
/// and any other column is cast to strings.
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
    let labels = levels.labels();
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
pub fn recode(column: &Column, levels: &Arc<Levels>) -> Result<Series> {
    if let Some(own) = self::levels(column.dtype())
        && own.same(levels)
    {
        return Ok(column.as_materialized_series().clone());
    }
    let name = column.name().clone();
    let (codes, validity): (Vec<u32>, Vec<bool>) = match self::levels(column.dtype()) {
        Some(own) => {
            let translation: Vec<Option<u32>> =
                own.iter().map(|label| levels.position(label)).collect();
            let codes = column.as_materialized_series().to_physical_repr();
            let codes = codes.cast(&DataType::UInt32)?;
            codes
                .u32()?
                .iter()
                .map(|code| {
                    match code.and_then(|code| translation.get(code as usize).copied().flatten()) {
                        Some(code) => (code, true),
                        None => (0, false),
                    }
                })
                .unzip()
        }
        None => {
            let text = column.cast(&DataType::String)?;
            text.str()?
                .iter()
                .map(
                    |label| match label.and_then(|label| levels.position(label)) {
                        Some(code) => (code, true),
                        None => (0, false),
                    },
                )
                .unzip()
        }
    };
    let validity = validity
        .iter()
        .any(|valid| !valid)
        .then(|| Bitmap::from_iter(validity));
    Ok(from_codes(name, levels.clone(), codes, validity))
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
/// warm. A partition copies only labels longer than a view holds inline into its own byte arena,
/// which later becomes one of the level table's buffers, so a label's bytes are copied once.
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

/// A raw pointer to the codes a piece writes, shared by the partitions because each writes only
/// the rows whose strings hash into it.
#[derive(Clone, Copy)]
struct Codes32(*mut u32);

// SAFETY: the partitions write disjoint rows, and the pointer outlives every write.
unsafe impl Send for Codes32 {}
unsafe impl Sync for Codes32 {}

impl Codes32 {
    /// # Safety
    /// `row` must be within the piece, and no other thread may write it.
    unsafe fn write(self, row: usize, code: u32) {
        unsafe { *self.0.add(row) = code };
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
        let codes = Codes32(self.codes[base..].as_mut_ptr());
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
    /// A key is as wide as a view, so the sorted keys become the level table's views in place,
    /// and the most the method holds beyond the codes is the partitions' views, the keys and one
    /// rank per level.
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
        let mut start = 0;
        while start < keys.len() {
            let prefix = prefix_of(&keys[start]);
            let end = start
                + keys[start..]
                    .iter()
                    .position(|key| prefix_of(key) != prefix)
                    .unwrap_or(keys.len() - start);
            if end - start > 1 {
                keys[start..end].sort_unstable_by(|a, b| label_of(a).cmp(label_of(b)));
            }
            start = end;
        }
        let span = partitions
            .iter()
            .map(|owned| owned.views.len())
            .max()
            .unwrap_or(0)
            << PARTITION_BITS;
        let mut rank = vec![0u32; span];
        for (position, key) in keys.iter().enumerate() {
            rank[key.offset as usize] = position as u32;
        }
        keys.par_iter_mut().for_each(|key| {
            let code = key.offset;
            *key = partitions[(code as usize) & (PARTITIONS - 1)].views
                [(code >> PARTITION_BITS) as usize];
        });
        let buffers: Buffer<Buffer<u8>> = partitions
            .into_iter()
            .map(|owned| Buffer::from(owned.arena))
            .collect::<Vec<_>>()
            .into();
        let valid = validity.as_ref();
        codes.par_iter_mut().enumerate().for_each(|(row, code)| {
            *code = match valid.is_none_or(|bits| bits.get(row)) {
                true => rank.get(*code as usize).copied().unwrap_or(0),
                false => 0,
            }
        });
        drop(rank);
        // SAFETY: every view is inline or points into its partition's arena, which became the
        // buffer at the partition's index, and every label is valid UTF-8 because it was read
        // from a string array.
        let labels = unsafe {
            Utf8ViewArray::new_unchecked_unknown_md(
                ArrowDataType::Utf8View,
                keys.into(),
                buffers,
                None,
                None,
            )
        };
        from_codes(
            name,
            Arc::new(Levels::new(labels)),
            codes,
            validity.and_then(|bits| bits.into()),
        )
    }
}
