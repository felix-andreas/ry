//! Environment frames. A frame maps symbols to binding cells, which are heap objects of their own
//! (`SexpType::Binding`, a triple of symbol, value and spare slot). A cell has a stable address for
//! as long as anything references it, which is what lets compiled code watch a binding for
//! redefinition, and it outlives its removal from the frame if compiled code still holds it.
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

/// The cells a frame holds without a second allocation.
const INLINE_CELLS: usize = 12;

/// GNU R's default and minimum hash table size (`HASHMINSIZE`).
const HASH_MINIMUM_SIZE: u32 = 29;

/// The end of a bucket chain in `Frame::hash_chains`.
const EMPTY_CHAIN: usize = usize::MAX;

#[derive(Default)]
pub struct Frame {
    /// Inline, because most frames are a call's few arguments and locals, and a closure call
    /// should not pay for a second allocation beside the frame's own.
    cells: SmallVec<[Sexp; INLINE_CELLS]>,
    index: Option<FxHashMap<Sexp, u32>>,
    /// The initial table size of a hashed frame, or 0 for an unhashed one.
    hash_size: u32,
    /// How many leading cells are a closure's formals, which GNU R lists after the locals and
    /// in their own order.
    formals: u32,
}

impl Frame {
    pub fn with_capacity(bindings: usize) -> Frame {
        Frame {
            cells: SmallVec::with_capacity(bindings),
            ..Frame::default()
        }
    }

    pub fn cells(&self) -> &[Sexp] {
        &self.cells
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The binding cell for `symbol`, if the frame binds it.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn find(&self, symbol: Sexp) -> Option<Sexp> {
        match &self.index {
            Some(index) => index
                .get(&symbol)
                .and_then(|&position| self.cells.get(position as usize).copied()),
            None => unsafe { self.cells.iter().copied().find(|cell| cell.car() == symbol) },
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
        self.formals = self.cells.len() as u32;
    }

    /// The cells in the order GNU R lists the frame in.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn listed_cells(&self) -> Vec<Sexp> {
        if self.hash_size == 0 {
            let formals = (self.formals as usize).min(self.cells.len());
            let (formals, locals) = self.cells.split_at(formals);
            return locals.iter().rev().chain(formals).copied().collect();
        }
        let (heads, next, _) = unsafe { self.hash_chains() };
        let mut listed = Vec::with_capacity(self.cells.len());
        for &head in &heads {
            let mut position = head;
            while position != EMPTY_CHAIN {
                listed.push(self.cells[position]);
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
        if self.hash_size == 0 {
            return None;
        }
        let (heads, next, count) = unsafe { self.hash_chains() };
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

    /// Replays GNU R's chained table, measured against its output: an insert prepends to its
    /// bucket and counts one, and once the count passes 85% of the size the table grows to
    /// `size * 1.2 + 1`, rehashing the old buckets in ascending order with each entry prepended,
    /// after which the count restarts at the number of occupied buckets. Each bucket is a chain
    /// threaded through `next`, so an insert and a rehash allocate nothing per entry. Returns the
    /// bucket heads, the chain links and the count.
    unsafe fn hash_chains(&self) -> (Vec<usize>, Vec<usize>, usize) {
        let mut size = (self.hash_size as usize).max(1);
        let mut heads = vec![EMPTY_CHAIN; size];
        let mut next = vec![EMPTY_CHAIN; self.cells.len()];
        let mut count = 0;
        let hashes: Vec<usize> = self
            .cells
            .iter()
            .map(|cell| unsafe { pjw_hash(cell.car().car().bytes()) } as usize)
            .collect();
        for (position, hash) in hashes.iter().enumerate() {
            let bucket = hash % size;
            next[position] = heads[bucket];
            heads[bucket] = position;
            count += 1;
            if count as f64 > size as f64 * 0.85 {
                size = (size as f64 * 1.2) as usize + 1;
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
                count = grown.iter().filter(|head| **head != EMPTY_CHAIN).count();
                heads = grown;
            }
        }
        (heads, next, count)
    }

    /// Adds a cell. Only a closure with duplicate formals (which `formals<-` can make) adds a
    /// symbol the frame already binds, and then the first cell keeps answering lookups, as the
    /// first match in GNU R's frame list does.
    pub(crate) unsafe fn insert(&mut self, cell: Sexp) {
        let position = self.cells.len() as u32;
        self.cells.push(cell);
        match &mut self.index {
            Some(index) => {
                index.entry(unsafe { cell.car() }).or_insert(position);
            }
            None if self.cells.len() > INDEX_THRESHOLD => {
                let mut index = FxHashMap::default();
                for (position, cell) in self.cells.iter().enumerate() {
                    index
                        .entry(unsafe { cell.car() })
                        .or_insert(position as u32);
                }
                self.index = Some(index);
            }
            None => {}
        }
    }

    /// Removes the cell for `symbol` and returns it.
    pub(crate) unsafe fn remove(&mut self, symbol: Sexp) -> Option<Sexp> {
        let position = match &mut self.index {
            Some(index) => index.remove(&symbol)? as usize,
            None => unsafe { self.cells.iter().position(|cell| cell.car() == symbol)? },
        };
        // Removal keeps the insertion order, which the listing order is derived from.
        let cell = self.cells.remove(position);
        if let Some(index) = &mut self.index {
            for (offset, moved) in self.cells[position..].iter().enumerate() {
                index.insert(unsafe { moved.car() }, (position + offset) as u32);
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
