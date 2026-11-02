//! Environment frames. A frame maps symbols to binding cells, which are heap objects of their own
//! (`SexpType::Binding`, a triple of symbol, value and spare slot). A cell has a stable address for
//! as long as anything references it, which is what lets compiled code watch a binding for
//! redefinition, and it outlives its removal from the frame if compiled code still holds it.
//!
//! The frame keeps each cell's symbol beside it, so a lookup scans one contiguous array inside the
//! environment object instead of loading every cell, which would cost a cache miss per binding.
//!
//! The cells are kept in insertion order, and [`Frame::listed_cells`] derives from it the order
//! GNU R lists a frame in (`ls(sorted = FALSE)`, `as.list`, `eapply`), which packages such as R6
//! depend on. An unhashed frame is a cons list there, so it lists the newest binding first, after
//! which come a closure's formals in their own order. A hashed one lists its buckets in index
//! order, each newest first, with the buckets fixed by GNU R's PJW string hash and its growth rule.
//!
//! The base environment and the base namespace have no `Frame`: their cells hang off the symbols
//! themselves, so a lookup of a base function is one load from the interned symbol.

use {crate::object::Sexp, rustc_hash::FxHashMap, smallvec::SmallVec};

/// Frames this small are scanned; larger ones keep a hash index beside the cells. Scanning a
/// dozen cells is cheaper than building the index, which a call to a function with many
/// formals (`identical` has nine) would otherwise pay on every call.
const INDEX_THRESHOLD: usize = 16;

/// The bindings a frame holds without a second allocation: as many as keep the environment object,
/// with its symbol filter, in the 192-byte size class.
const INLINE_BINDINGS: usize = 8;

/// A binding cell and its symbol, which is the cell's own first field.
#[derive(Clone, Copy)]
struct Binding {
    symbol: Sexp,
    cell: Sexp,
}

/// The symbol of a removed binding's slot. Removal leaves the slot in place, so it costs O(1)
/// instead of shifting every later binding and index entry, and `rm()` over a large frame stays
/// linear; once half the slots are removed the frame compacts. No real symbol lives at the
/// dangling address, so a lookup never matches it.
const REMOVED: Sexp = Sexp::dangling();

/// GNU R's default and minimum hash table size (`HASHMINSIZE`).
const HASH_MINIMUM_SIZE: u32 = 29;

/// The end of a bucket chain in `Frame::hash_chains`.
const EMPTY_CHAIN: usize = usize::MAX;

#[derive(Default)]
pub struct Frame {
    /// Inline, because most frames are a call's few arguments and locals, and a closure call
    /// should not pay for a second allocation beside the frame's own.
    bindings: SmallVec<[Binding; INLINE_BINDINGS]>,
    /// Boxed, because only large frames have one and the inline map would push every
    /// environment into a larger size class.
    index: Option<Box<FxHashMap<Sexp, u32>>>,
    /// GNU R's table bookkeeping, for a hashed frame only. Boxed, so an unhashed frame pays one
    /// word for it.
    hash: Option<Box<HashTable>>,
    /// How many leading slots are a closure's formals, which GNU R lists after the locals and
    /// in their own order.
    formals: u32,
    /// How many slots hold [`REMOVED`].
    removed: u32,
    /// One bit per symbol ever bound here, at [`filter_bit`]. Most lookups probe frames that do
    /// not bind the symbol (a call to `length` from a function body asks the function's frame
    /// first), and a clear bit answers those without scanning. Removal leaves the bit set.
    filter: u64,
}

/// What a hashed frame needs to list itself as GNU R's chained table would: the table's growth
/// history and its chain counter. GNU R lists a hashed frame bucket by bucket, so the order
/// depends on every size the table has had, and the size grows when the counter passes 85% of
/// it. Measured against GNU R, an insert counts one even into an occupied bucket, a removal
/// uncounts one only when it empties its bucket, and a growth restarts the counter at the number
/// of occupied buckets.
struct HashTable {
    initial_size: u32,
    size: u32,
    /// GNU R's chain counter, which `env.profile` reports as `nchains`.
    count: u32,
    /// How many live bindings each bucket of the current table holds, which tells a removal
    /// whether it empties its bucket.
    occupancy: Vec<u32>,
    /// For each growth, how many leading slots of [`Frame::bindings`] were inserted before it.
    grows: Vec<u32>,
}

/// The [`Frame::filter`] bit for `symbol`: a multiplicative hash of its address, since a
/// symbol never moves and neighbouring symbols differ only in low bits.
#[inline(always)]
fn filter_bit(symbol: Sexp) -> u64 {
    1 << ((symbol.as_ptr() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 58)
}

impl Frame {
    pub fn with_capacity(bindings: usize) -> Frame {
        Frame {
            bindings: SmallVec::with_capacity(bindings),
            ..Frame::default()
        }
    }

    pub fn cells(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.live().map(|binding| binding.cell)
    }

    pub fn symbols(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.live().map(|binding| binding.symbol)
    }

    pub fn len(&self) -> usize {
        self.bindings.len() - self.removed as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn live(&self) -> impl DoubleEndedIterator<Item = &Binding> + '_ {
        self.bindings
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
    }

    /// The binding cell for `symbol`, if the frame binds it.
    #[inline]
    pub fn find(&self, symbol: Sexp) -> Option<Sexp> {
        if self.filter & filter_bit(symbol) == 0 {
            return None;
        }
        match &self.index {
            Some(index) => self
                .bindings
                .get(*index.get(&symbol)? as usize)
                .map(|binding| binding.cell),
            None => self
                .bindings
                .iter()
                .find(|binding| binding.symbol == symbol)
                .map(|binding| binding.cell),
        }
    }

    /// Makes the frame hashed with GNU R's table size for `size` (29 when `size` is not
    /// positive).
    pub fn set_hashed(&mut self, size: i32) {
        let size = if size > 0 {
            size as u32
        } else {
            HASH_MINIMUM_SIZE
        };
        let mut table = HashTable {
            initial_size: size,
            size,
            count: 0,
            occupancy: vec![0; size as usize],
            grows: Vec::new(),
        };
        for slot in 0..self.bindings.len() {
            if self.bindings[slot].symbol != REMOVED {
                hash_insert(&mut table, &self.bindings, slot);
            }
        }
        self.hash = Some(Box::new(table));
    }

    /// Marks every cell bound so far as a closure formal.
    pub fn mark_formals(&mut self) {
        self.formals = self.bindings.len() as u32;
    }

    /// The cells in the order GNU R lists the frame in.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn listed_cells(&self) -> Vec<Sexp> {
        if self.hash.is_none() {
            let formals = (self.formals as usize).min(self.bindings.len());
            let (formals, locals) = self.bindings.split_at(formals);
            return locals
                .iter()
                .rev()
                .chain(formals)
                .filter(|binding| binding.symbol != REMOVED)
                .map(|binding| binding.cell)
                .collect();
        }
        let (heads, next) = unsafe { self.hash_chains() };
        let cells: Vec<Sexp> = self.cells().collect();
        let mut listed = Vec::with_capacity(cells.len());
        for &head in &heads {
            let mut position = head;
            while position != EMPTY_CHAIN {
                listed.push(cells[position]);
                position = next[position];
            }
        }
        listed
    }

    /// The binding count of each bucket of GNU R's hash table for a hashed frame, together with
    /// the table's chain counter, or `None` for an unhashed frame. `env.profile` reports the
    /// counter as `nchains`, so it can exceed the number of non-empty buckets.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn hash_profile(&self) -> Option<(Vec<usize>, usize)> {
        let count = self.hash.as_ref()?.count as usize;
        let (heads, next) = unsafe { self.hash_chains() };
        let lengths = heads
            .iter()
            .map(|&head| {
                let mut length = 0;
                let mut position = head;
                while position != EMPTY_CHAIN {
                    length += 1;
                    position = next[position];
                }
                length
            })
            .collect();
        Some((lengths, count))
    }

    /// Replays GNU R's chained table over the live bindings: an insert prepends to its bucket,
    /// and a growth to `size * 1.2 + 1` rehashes the old buckets in ascending order with each
    /// entry prepended. Removed bindings only ever left their chains, so leaving them out keeps
    /// every other entry's order. Each bucket is a chain threaded through `next`, so an insert and
    /// a rehash allocate nothing per entry. Returns the bucket heads and the chain links, with
    /// positions counting live bindings only, or no buckets for an unhashed frame.
    unsafe fn hash_chains(&self) -> (Vec<usize>, Vec<usize>) {
        let Some(table) = self.hash.as_deref() else {
            return (Vec::new(), Vec::new());
        };
        let mut size = table.initial_size as usize;
        let mut heads = vec![EMPTY_CHAIN; size];
        let mut next = vec![EMPTY_CHAIN; self.len()];
        let hashes: Vec<usize> = self
            .live()
            .map(|binding| symbol_hash(binding.symbol) as usize)
            .collect();
        let mut grows = table.grows.iter().copied().peekable();
        let mut position = 0;
        // A growth marker counts the slots inserted before it, and compaction can leave one at 0
        // when every binding inserted before the growth has since been removed.
        for slot in 0..=self.bindings.len() {
            while grows.next_if_eq(&(slot as u32)).is_some() {
                size = grown_size(size as u32) as usize;
                let mut grown = vec![EMPTY_CHAIN; size];
                for &head in &heads {
                    let mut moved = head;
                    while moved != EMPTY_CHAIN {
                        let following = next[moved];
                        let bucket = hashes[moved] % size;
                        next[moved] = grown[bucket];
                        grown[bucket] = moved;
                        moved = following;
                    }
                }
                heads = grown;
            }
            if let Some(binding) = self.bindings.get(slot)
                && binding.symbol != REMOVED
            {
                let bucket = hashes[position] % size;
                next[position] = heads[bucket];
                heads[bucket] = position;
                position += 1;
            }
        }
        (heads, next)
    }

    /// Adds a cell. Only a closure with duplicate formals (which `formals<-` can make) adds a
    /// symbol the frame already binds, and then the first cell keeps answering lookups, as the
    /// first match in GNU R's frame list does.
    pub(crate) fn insert(&mut self, symbol: Sexp, cell: Sexp) {
        let position = self.bindings.len() as u32;
        self.bindings.push(Binding { symbol, cell });
        if let Some(table) = &mut self.hash {
            hash_insert(table, &self.bindings, position as usize);
        }
        self.filter |= filter_bit(symbol);
        match &mut self.index {
            Some(index) => {
                index.entry(symbol).or_insert(position);
            }
            None if self.bindings.len() > INDEX_THRESHOLD => {
                let mut index = FxHashMap::default();
                for (position, binding) in self.bindings.iter().enumerate() {
                    if binding.symbol != REMOVED {
                        index.entry(binding.symbol).or_insert(position as u32);
                    }
                }
                self.index = Some(Box::new(index));
            }
            None => {}
        }
    }

    /// Counts an insert and removal of `symbol` in a hashed frame's table without keeping a
    /// binding, for a binding that exists only for the length of one operation. GNU R would
    /// rebind an existing binding instead, which counts nothing.
    pub(crate) fn count_transient(&mut self, symbol: Sexp) {
        if self.hash.is_none() || self.find(symbol).is_some() {
            return;
        }
        let slot = self.bindings.len();
        self.bindings.push(Binding {
            symbol,
            cell: REMOVED,
        });
        if let Some(table) = &mut self.hash {
            hash_insert(table, &self.bindings, slot);
        }
        self.bindings[slot].symbol = REMOVED;
        self.removed += 1;
        self.uncount_removal(symbol);
        if self.removed as usize * 2 > self.bindings.len() {
            self.compact();
        }
    }

    /// Removes the cell for `symbol` and returns it, in O(1) amortized time.
    pub(crate) fn remove(&mut self, symbol: Sexp) -> Option<Sexp> {
        if self.filter & filter_bit(symbol) == 0 {
            return None;
        }
        let position = match &mut self.index {
            Some(index) => index.remove(&symbol)? as usize,
            None => self
                .bindings
                .iter()
                .position(|binding| binding.symbol == symbol)?,
        };
        let binding = self.bindings.get_mut(position)?;
        let cell = binding.cell;
        binding.symbol = REMOVED;
        self.removed += 1;
        self.uncount_removal(symbol);
        // Only formals repeat a symbol, and a duplicate behind the removed one now answers for it.
        if let Some(index) = &mut self.index
            && let Some(duplicate) = (position + 1..self.formals as usize)
                .find(|&later| self.bindings[later].symbol == symbol)
        {
            index.insert(symbol, duplicate as u32);
        }
        if self.removed as usize * 2 > self.bindings.len() {
            self.compact();
        }
        Some(cell)
    }

    /// Takes a removed binding of `symbol` out of a hashed frame's table, where GNU R uncounts it
    /// only when it empties its bucket.
    fn uncount_removal(&mut self, symbol: Sexp) {
        let Some(table) = &mut self.hash else {
            return;
        };
        let bucket = symbol_hash(symbol) as usize % table.size as usize;
        if let Some(occupants) = table.occupancy.get_mut(bucket) {
            *occupants = occupants.saturating_sub(1);
            if *occupants == 0 {
                table.count = table.count.saturating_sub(1);
            }
        }
    }

    /// Drops the removed slots, keeping the live bindings in insertion order.
    fn compact(&mut self) {
        let formals = self.bindings[..(self.formals as usize).min(self.bindings.len())]
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
            .count();
        if let Some(table) = &mut self.hash {
            for grow in &mut table.grows {
                *grow = self.bindings[..*grow as usize]
                    .iter()
                    .filter(|binding| binding.symbol != REMOVED)
                    .count() as u32;
            }
        }
        self.bindings.retain(|binding| binding.symbol != REMOVED);
        self.formals = formals as u32;
        self.removed = 0;
        if let Some(index) = &mut self.index {
            index.clear();
            for (position, binding) in self.bindings.iter().enumerate() {
                index.entry(binding.symbol).or_insert(position as u32);
            }
        }
    }
}

/// Counts the insert of the live binding at `slot` in GNU R's table, growing it as GNU R would.
fn hash_insert(table: &mut HashTable, bindings: &[Binding], slot: usize) {
    let Some(binding) = bindings.get(slot) else {
        return;
    };
    let bucket = symbol_hash(binding.symbol) as usize % table.size as usize;
    if let Some(occupants) = table.occupancy.get_mut(bucket) {
        *occupants += 1;
    }
    table.count += 1;
    if table.count as f64 <= table.size as f64 * 0.85 {
        return;
    }
    table.size = grown_size(table.size);
    table.occupancy = vec![0; table.size as usize];
    for binding in bindings[..=slot]
        .iter()
        .filter(|binding| binding.symbol != REMOVED)
    {
        if let Some(occupants) = table
            .occupancy
            .get_mut(symbol_hash(binding.symbol) as usize % table.size as usize)
        {
            *occupants += 1;
        }
    }
    table.count = table.occupancy.iter().filter(|&&occupants| occupants > 0).count() as u32;
    table.grows.push(slot as u32 + 1);
}

fn grown_size(size: u32) -> u32 {
    (size as f64 * 1.2) as u32 + 1
}

/// The PJW hash of a bound symbol's name.
fn symbol_hash(symbol: Sexp) -> u32 {
    // SAFETY: a frame binds only interned symbols, which are never collected, and a symbol's
    // first field is its name.
    unsafe { pjw_hash(symbol.car().bytes()) }
}

/// GNU R's `R_Newhashpjw`, which adds each byte as a signed `char`.
fn pjw_hash(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &byte in bytes {
        hash = (hash << 4).wrapping_add(byte as i8 as i32 as u32);
        let high = hash & 0xf000_0000;
        if high != 0 {
            hash ^= high >> 24;
            hash ^= high;
        }
    }
    hash
}
