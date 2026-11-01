//! Environment frames. A frame maps symbols to binding cells, which are heap objects of their own
//! (`SexpType::Binding`, a triple of symbol, value and spare slot). A cell has a stable address for
//! as long as anything references it, which is what lets compiled code watch a binding for
//! redefinition, and it outlives its removal from the frame if compiled code still holds it.
//!
//! The cells are kept in insertion order, and [`Frame::listed_cells`] derives from it the order
//! GNU R lists a frame in (`ls(sorted = FALSE)`, `as.list`, `eapply`), which packages such as R6
//! depend on. An unhashed frame is a cons list there, so it lists the newest binding first, after
//! which come a closure's formals in their own order. A hashed one lists its buckets in index
//! order, each newest first, with the buckets fixed by GNU R's PJW string hash.
//!
//! The base environment and the base namespace have no `Frame`: their cells hang off the symbols
//! themselves, so a lookup of a base function is one load from the interned symbol.

use crate::object::Sexp;
use rustc_hash::FxHashMap;

/// Frames this small are scanned; larger ones keep a hash index beside the cells.
const INDEX_THRESHOLD: usize = 8;

/// GNU R's default and minimum hash table size (`HASHMINSIZE`).
const HASH_MINIMUM_SIZE: u32 = 29;

#[derive(Default)]
pub struct Frame {
    cells: Vec<Sexp>,
    index: Option<FxHashMap<Sexp, u32>>,
    /// The initial table size of a hashed frame, or 0 for an unhashed one.
    hash_size: u32,
    /// How many leading cells are a closure's formals, which GNU R lists after the locals and
    /// in their own order.
    formals: u32,
}

impl Frame {
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
        let (buckets, _) = unsafe { self.hash_buckets() };
        buckets
            .iter()
            .flatten()
            .map(|&position| self.cells[position])
            .collect()
    }

    /// GNU R's hash table for a hashed frame, as positions into the cells, together with the
    /// table's chain counter, or `None` for an unhashed frame.
    ///
    /// The table is replayed from the insertion order. GNU R prepends a new binding to its
    /// bucket and counts it as a new chain whether or not the bucket was empty. Once that counter
    /// exceeds 85% of the size, the table grows to `1 + 1.2 * size` buckets, each chain's cells
    /// move, head first, to the front of their new bucket, and the counter becomes the number of
    /// non-empty buckets. `env.profile` reports the counter as `nchains`, so it can exceed the
    /// number of non-empty buckets.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn hash_table(&self) -> Option<(Vec<Vec<usize>>, usize)> {
        (self.hash_size != 0).then(|| unsafe { self.hash_buckets() })
    }

    unsafe fn hash_buckets(&self) -> (Vec<Vec<usize>>, usize) {
        let hashes: Vec<usize> = self
            .cells
            .iter()
            .map(|cell| unsafe { pjw_hash(cell.car().car().bytes()) } as usize)
            .collect();
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); self.hash_size.max(1) as usize];
        let mut chains = 0usize;
        for (position, &hash) in hashes.iter().enumerate() {
            let size = buckets.len();
            let Some(bucket) = buckets.get_mut(hash % size) else {
                continue;
            };
            bucket.insert(0, position);
            chains += 1;
            if chains as f64 > size as f64 * 0.85 {
                let grown = 1 + (size as f64 * 1.2) as usize;
                let mut rehashed: Vec<Vec<usize>> = vec![Vec::new(); grown];
                for &moved in buckets.iter().flatten() {
                    if let Some(target) = hashes
                        .get(moved)
                        .and_then(|hash| rehashed.get_mut(hash % grown))
                    {
                        target.insert(0, moved);
                    }
                }
                chains = rehashed.iter().filter(|bucket| !bucket.is_empty()).count();
                buckets = rehashed;
            }
        }
        (buckets, chains)
    }

    /// Adds a cell whose symbol the frame does not bind yet.
    pub(crate) unsafe fn insert(&mut self, cell: Sexp) {
        let position = self.cells.len() as u32;
        self.cells.push(cell);
        match &mut self.index {
            Some(index) => {
                index.insert(unsafe { cell.car() }, position);
            }
            None if self.cells.len() > INDEX_THRESHOLD => {
                let index = self
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(position, cell)| (unsafe { cell.car() }, position as u32))
                    .collect();
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
