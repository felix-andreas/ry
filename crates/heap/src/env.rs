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

/// The end of a bucket chain in [`HashTable`].
const EMPTY_CHAIN: u32 = u32::MAX;

#[derive(Default)]
pub struct Frame {
    /// Inline, because most frames are a call's few arguments and locals, and a closure call
    /// should not pay for a second allocation beside the frame's own.
    bindings: SmallVec<[Binding; INLINE_BINDINGS]>,
    /// Boxed, because only large frames have one and the inline map would push every
    /// environment into a larger size class.
    index: Option<Box<FxHashMap<Sexp, u32>>>,
    /// GNU R's chained table for a hashed frame, which only decides the listing order: lookups
    /// go through `index`. Boxed, for the same reason as the index.
    table: Option<Box<HashTable>>,
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

/// The [`Frame::filter`] bit for `symbol`: a multiplicative hash of its address, since a
/// symbol never moves and neighbouring symbols differ only in low bits.
#[inline(always)]
fn filter_bit(symbol: Sexp) -> u64 {
    1 << ((symbol.as_ptr() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 58)
}

impl Frame {
    /// Initializes an empty frame at `frame` with room for `bindings` bindings.
    ///
    /// The frame is written field by field where it lives: a closure call creates one, and
    /// building it on the stack and moving it copied the whole inline binding buffer (about a
    /// twentieth of a tier-0 call).
    ///
    /// # Safety
    /// `frame` must be valid for writes and must not hold a live frame.
    #[inline(always)]
    pub unsafe fn write_empty(frame: *mut Frame, bindings: usize) {
        use std::ptr::{addr_of_mut, write};
        write(addr_of_mut!((*frame).bindings), SmallVec::new());
        write(addr_of_mut!((*frame).index), None);
        write(addr_of_mut!((*frame).table), None);
        write(addr_of_mut!((*frame).formals), 0);
        write(addr_of_mut!((*frame).removed), 0);
        write(addr_of_mut!((*frame).filter), 0);
        if bindings > INLINE_BINDINGS {
            (*frame).bindings.reserve_exact(bindings);
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
    /// positive). Environments are hashed when they are created, so the bindings a frame
    /// already holds are entered as if inserted afterwards, in order.
    pub fn set_hashed(&mut self, size: i32) {
        let size = if size > 0 {
            size as u32
        } else {
            HASH_MINIMUM_SIZE
        };
        let mut table = HashTable::new(size);
        for (position, binding) in self.bindings.iter().enumerate() {
            table.push(binding.symbol);
            if binding.symbol != REMOVED {
                table.insert(position);
            }
        }
        self.table = Some(Box::new(table));
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
        let Some(table) = &self.table else {
            let formals = (self.formals as usize).min(self.bindings.len());
            let (formals, locals) = self.bindings.split_at(formals);
            return locals
                .iter()
                .rev()
                .chain(formals)
                .filter(|binding| binding.symbol != REMOVED)
                .map(|binding| binding.cell)
                .collect();
        };
        let mut listed = Vec::with_capacity(self.len());
        for &head in &table.heads {
            let mut position = head;
            while position != EMPTY_CHAIN {
                listed.push(self.bindings[position as usize].cell);
                position = table.next[position as usize];
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
        let table = self.table.as_ref()?;
        let lengths = table
            .heads
            .iter()
            .map(|&head| {
                let mut length = 0;
                let mut position = head;
                while position != EMPTY_CHAIN {
                    length += 1;
                    position = table.next[position as usize];
                }
                length
            })
            .collect();
        Some((lengths, table.count as usize))
    }

    /// Adds a cell. Only a closure with duplicate formals (which `formals<-` can make) adds a
    /// symbol the frame already binds, and then the first cell keeps answering lookups, as the
    /// first match in GNU R's frame list does.
    pub(crate) fn insert(&mut self, symbol: Sexp, cell: Sexp) {
        let position = self.bindings.len() as u32;
        self.bindings.push(Binding { symbol, cell });
        self.filter |= filter_bit(symbol);
        if let Some(table) = &mut self.table {
            table.push(symbol);
            table.insert(position as usize);
        }
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

    /// Counts a binding of `symbol` held only for the length of one operation, as GNU R's
    /// complex assignment binds `*tmp*`, in the table that orders a hashed frame.
    pub(crate) fn count_transient(&mut self, symbol: Sexp) {
        if let Some(table) = &mut self.table {
            // SAFETY: a bound symbol is interned, and its name lives as long as the symbol.
            table.count_transient(unsafe { pjw_hash(symbol.car().bytes()) });
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
        if let Some(table) = &mut self.table {
            table.remove(position);
        }
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

    /// Drops the removed slots, keeping the live bindings in insertion order.
    fn compact(&mut self) {
        let formals = self.bindings[..(self.formals as usize).min(self.bindings.len())]
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
            .count();
        if let Some(table) = &mut self.table {
            table.compact(&self.bindings);
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

/// A frame's exact contents, as a heap image stores them: every slot in order (a removed one
/// as `None`), the formal count and removal count, and the hash table's chains. The symbol
/// filter and the lookup index derive from the slots, so an image leaves them out.
pub(crate) struct FrameImage {
    pub slots: Vec<Option<(Sexp, Sexp)>>,
    pub formals: u32,
    pub removed: u32,
    pub table: Option<TableImage>,
}

pub(crate) struct TableImage {
    pub heads: Vec<u32>,
    pub next: Vec<u32>,
    pub hashes: Vec<u32>,
    pub count: u32,
}

impl Frame {
    pub(crate) fn image(&self) -> FrameImage {
        FrameImage {
            slots: self
                .bindings
                .iter()
                .map(|binding| {
                    (binding.symbol != REMOVED).then_some((binding.symbol, binding.cell))
                })
                .collect(),
            formals: self.formals,
            removed: self.removed,
            table: self.table.as_ref().map(|table| TableImage {
                heads: table.heads.clone(),
                next: table.next.clone(),
                hashes: table.hashes.clone(),
                count: table.count,
            }),
        }
    }

    /// The frame an image stored, with a removed slot's cell set to `nil`.
    pub(crate) fn from_image(image: FrameImage, nil: Sexp) -> Frame {
        let bindings: SmallVec<[Binding; INLINE_BINDINGS]> = image
            .slots
            .iter()
            .map(|slot| match *slot {
                Some((symbol, cell)) => Binding { symbol, cell },
                None => Binding {
                    symbol: REMOVED,
                    cell: nil,
                },
            })
            .collect();
        let mut filter = 0;
        for binding in bindings.iter().filter(|binding| binding.symbol != REMOVED) {
            filter |= filter_bit(binding.symbol);
        }
        let index = (bindings.len() > INDEX_THRESHOLD).then(|| {
            let mut index = FxHashMap::with_capacity_and_hasher(bindings.len(), Default::default());
            for (position, binding) in bindings.iter().enumerate() {
                if binding.symbol != REMOVED {
                    index.entry(binding.symbol).or_insert(position as u32);
                }
            }
            Box::new(index)
        });
        Frame {
            bindings,
            index,
            table: image.table.map(|table| {
                Box::new(HashTable {
                    heads: table.heads,
                    next: table.next,
                    hashes: table.hashes,
                    count: table.count,
                })
            }),
            formals: image.formals,
            removed: image.removed,
            filter,
        }
    }
}

/// GNU R's chained hash table for a hashed frame, kept in step with the frame's inserts and
/// removals because the order GNU R lists the frame in depends on that whole history: a removal
/// leaves the table at the size it grew to, and its chain counter falls only when a bucket
/// empties. Each bucket is a chain threaded through `next`, indexed like the frame's slots, so an
/// insert allocates nothing per entry.
struct HashTable {
    heads: Vec<u32>,
    next: Vec<u32>,
    /// The PJW hash of each slot's symbol name, so a resize does not hash again.
    hashes: Vec<u32>,
    /// GNU R's `HASHPRI`, which `env.profile` reports as `nchains`.
    count: u32,
}

impl HashTable {
    fn new(size: u32) -> HashTable {
        HashTable {
            heads: vec![EMPTY_CHAIN; size.max(1) as usize],
            next: Vec::new(),
            hashes: Vec::new(),
            count: 0,
        }
    }

    /// Adds the slot for a binding of `symbol`, which `insert` then links in.
    fn push(&mut self, symbol: Sexp) {
        let hash = if symbol == REMOVED {
            0
        } else {
            // SAFETY: a bound symbol is interned, and its name lives as long as the symbol.
            unsafe { pjw_hash(symbol.car().bytes()) }
        };
        self.hashes.push(hash);
        self.next.push(EMPTY_CHAIN);
    }

    /// Links slot `position` in, as GNU R's `R_HashSet` does: it prepends to the bucket and
    /// counts one whether or not the bucket was empty, and once the count passes 85% of the size
    /// the table grows to `size * 1.2 + 1`, moving the old buckets in ascending order with each
    /// entry prepended, after which the count is the number of occupied buckets.
    fn insert(&mut self, position: usize) {
        let bucket = self.hashes[position] as usize % self.heads.len();
        self.next[position] = self.heads[bucket];
        self.heads[bucket] = position as u32;
        self.count += 1;
        if self.count as f64 > self.heads.len() as f64 * 0.85 {
            self.grow();
        }
    }

    /// Counts a binding of a symbol with this hash that is inserted and removed again within one
    /// operation, as GNU R's complex assignment binds `*tmp*`. It never enters a chain, but its
    /// insert can grow the table, and its removal lowers the count only if it was alone in its
    /// bucket.
    fn count_transient(&mut self, hash: u32) {
        self.count += 1;
        if self.count as f64 > self.heads.len() as f64 * 0.85 {
            // The regrown count leaves the transient out, which is where its removal would leave it.
            self.grow();
        } else if self.heads[hash as usize % self.heads.len()] == EMPTY_CHAIN {
            self.count -= 1;
        }
    }

    /// Grows to `size * 1.2 + 1`, moving the old buckets in ascending order with each entry
    /// prepended, after which the count is the number of occupied buckets.
    fn grow(&mut self) {
        let size = (self.heads.len() as f64 * 1.2) as usize + 1;
        let mut grown = vec![EMPTY_CHAIN; size];
        for &head in &self.heads {
            let mut moved = head;
            while moved != EMPTY_CHAIN {
                let following = self.next[moved as usize];
                let bucket = self.hashes[moved as usize] as usize % size;
                self.next[moved as usize] = grown[bucket];
                grown[bucket] = moved;
                moved = following;
            }
        }
        self.count = grown.iter().filter(|head| **head != EMPTY_CHAIN).count() as u32;
        self.heads = grown;
    }

    /// Unlinks slot `position`, as GNU R's `R_HashDelete` does: the count falls only when the
    /// bucket empties, and the table never shrinks.
    fn remove(&mut self, position: usize) {
        let bucket = self.hashes[position] as usize % self.heads.len();
        let target = position as u32;
        let following = self.next[position];
        if self.heads[bucket] == target {
            self.heads[bucket] = following;
            if following == EMPTY_CHAIN {
                self.count -= 1;
            }
        } else {
            let mut previous = self.heads[bucket];
            while previous != EMPTY_CHAIN {
                if self.next[previous as usize] == target {
                    self.next[previous as usize] = following;
                    break;
                }
                previous = self.next[previous as usize];
            }
        }
        self.next[position] = EMPTY_CHAIN;
    }

    /// Renumbers the slots as the frame drops its removed ones.
    fn compact(&mut self, bindings: &[Binding]) {
        let mut renumbered = vec![EMPTY_CHAIN; bindings.len()];
        let mut live = 0;
        for (position, binding) in bindings.iter().enumerate() {
            if binding.symbol != REMOVED {
                renumbered[position] = live;
                self.hashes[live as usize] = self.hashes[position];
                self.next[live as usize] = self.next[position];
                live += 1;
            }
        }
        self.hashes.truncate(live as usize);
        self.next.truncate(live as usize);
        let renumber = |position: u32| {
            if position == EMPTY_CHAIN {
                EMPTY_CHAIN
            } else {
                renumbered[position as usize]
            }
        };
        for head in &mut self.heads {
            *head = renumber(*head);
        }
        for link in &mut self.next {
            *link = renumber(*link);
        }
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
