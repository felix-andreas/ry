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

/// The bindings a frame holds without a second allocation: as many as keep the environment object
/// in the 192-byte size class.
const INLINE_BINDINGS: usize = 9;

/// A binding cell and its symbol, which is the cell's own first field.
#[derive(Clone, Copy)]
struct Binding {
    symbol: Sexp,
    cell: Sexp,
}

/// GNU R's default and minimum hash table size (`HASHMINSIZE`).
const HASH_MINIMUM_SIZE: u32 = 29;

#[derive(Default)]
pub struct Frame {
    /// Inline, because most frames are a call's few arguments and locals, and a closure call
    /// should not pay for a second allocation beside the frame's own.
    bindings: SmallVec<[Binding; INLINE_BINDINGS]>,
    /// Boxed, because only large frames have one and the inline map would push every
    /// environment into a larger size class.
    index: Option<Box<FxHashMap<Sexp, u32>>>,
    /// The initial table size of a hashed frame, or 0 for an unhashed one.
    hash_size: u32,
    /// How many leading cells are a closure's formals, which GNU R lists after the locals and
    /// in their own order.
    formals: u32,
}

impl Frame {
    pub fn with_capacity(bindings: usize) -> Frame {
        Frame {
            bindings: SmallVec::with_capacity(bindings),
            ..Frame::default()
        }
    }

    pub fn cells(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.bindings.iter().map(|binding| binding.cell)
    }

    pub fn symbols(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.bindings.iter().map(|binding| binding.symbol)
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The binding cell for `symbol`, if the frame binds it.
    #[inline]
    pub fn find(&self, symbol: Sexp) -> Option<Sexp> {
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
        self.hash_size = if size > 0 {
            size as u32
        } else {
            HASH_MINIMUM_SIZE
        };
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
        if self.hash_size == 0 {
            let formals = (self.formals as usize).min(self.bindings.len());
            let (formals, locals) = self.bindings.split_at(formals);
            return locals
                .iter()
                .rev()
                .chain(formals)
                .map(|binding| binding.cell)
                .collect();
        }
        // Replays GNU R's chained table, measured against its output: an insert prepends to its
        // bucket and counts one, and once the count passes 85% of the size the table grows to
        // `size * 1.2 + 1`, rehashing the old buckets in ascending order with each entry
        // prepended, after which the count restarts at the number of occupied buckets.
        // Each bucket is a chain threaded through `next`, so an insert and a rehash allocate
        // nothing per entry.
        const EMPTY: usize = usize::MAX;
        let mut size = self.hash_size as usize;
        let mut heads = vec![EMPTY; size];
        let mut next = vec![EMPTY; self.bindings.len()];
        let mut count = 0;
        let hashes: Vec<usize> = self
            .bindings
            .iter()
            .map(|binding| unsafe { pjw_hash(binding.symbol.car().bytes()) } as usize)
            .collect();
        for (position, hash) in hashes.iter().enumerate() {
            let bucket = hash % size;
            next[position] = heads[bucket];
            heads[bucket] = position;
            count += 1;
            if count as f64 > size as f64 * 0.85 {
                size = (size as f64 * 1.2) as usize + 1;
                let mut grown = vec![EMPTY; size];
                for &head in &heads {
                    let mut moved = head;
                    while moved != EMPTY {
                        let following = next[moved];
                        let bucket = hashes[moved] % size;
                        next[moved] = grown[bucket];
                        grown[bucket] = moved;
                        moved = following;
                    }
                }
                count = grown.iter().filter(|head| **head != EMPTY).count();
                heads = grown;
            }
        }
        let mut listed = Vec::with_capacity(self.bindings.len());
        for &head in &heads {
            let mut position = head;
            while position != EMPTY {
                listed.push(self.bindings[position].cell);
                position = next[position];
            }
        }
        listed
    }

    /// Adds a cell. Only a closure with duplicate formals (which `formals<-` can make) adds a
    /// symbol the frame already binds, and then the first cell keeps answering lookups, as the
    /// first match in GNU R's frame list does.
    pub(crate) fn insert(&mut self, symbol: Sexp, cell: Sexp) {
        let position = self.bindings.len() as u32;
        self.bindings.push(Binding { symbol, cell });
        match &mut self.index {
            Some(index) => {
                index.entry(symbol).or_insert(position);
            }
            None if self.bindings.len() > INDEX_THRESHOLD => {
                let mut index = FxHashMap::default();
                for (position, binding) in self.bindings.iter().enumerate() {
                    index.entry(binding.symbol).or_insert(position as u32);
                }
                self.index = Some(Box::new(index));
            }
            None => {}
        }
    }

    /// Removes the cell for `symbol` and returns it.
    pub(crate) fn remove(&mut self, symbol: Sexp) -> Option<Sexp> {
        let position = match &mut self.index {
            Some(index) => index.remove(&symbol)? as usize,
            None => self
                .bindings
                .iter()
                .position(|binding| binding.symbol == symbol)?,
        };
        // Removal keeps the insertion order, which the listing order is derived from.
        let cell = self.bindings.remove(position).cell;
        if let Some(index) = &mut self.index {
            for slot in index.values_mut() {
                if *slot as usize > position {
                    *slot -= 1;
                }
            }
            // A duplicate formal behind the removed binding now answers for its symbol.
            if let Some(offset) = self.bindings[position..]
                .iter()
                .position(|binding| binding.symbol == symbol)
            {
                index.insert(symbol, (position + offset) as u32);
            }
        }
        if position < self.formals as usize {
            self.formals -= 1;
        }
        Some(cell)
    }
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
