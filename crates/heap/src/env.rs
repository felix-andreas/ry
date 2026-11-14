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

use {
    crate::object::{Sexp, gp},
    rustc_hash::FxHashSet,
    std::{
        alloc::{Layout, alloc, alloc_zeroed, dealloc, handle_alloc_error},
        mem::MaybeUninit,
        ops::{Deref, DerefMut},
        ptr::NonNull,
    },
};

/// Frames this small are scanned; larger ones keep a hash index beside the cells. A scan is
/// guarded by the frame's symbol filter and usually skipped by a lookup site's slot hint, so
/// it stays cheaper than building the index, which a call of a base closure with many formals
/// and locals (`seq.default` binds about 20) would otherwise pay on every call.
const INDEX_THRESHOLD: usize = 32;

/// The bindings a frame holds without a second allocation: as many as keep the environment object,
/// with its symbol filter, in the 192-byte size class.
const INLINE_BINDINGS: usize = 8;

/// A binding cell and its symbol, which is the cell's own first field.
#[derive(Clone, Copy)]
#[repr(C)]
struct Binding {
    symbol: Sexp,
    cell: Sexp,
}

/// The symbol of a removed binding's slot. Removal leaves the slot in place, so it costs O(1)
/// instead of shifting every later binding and index entry, and `rm()` over a large frame stays
/// linear; once half the slots are removed the frame compacts. No real symbol lives at the
/// dangling address, so a lookup never matches it.
const REMOVED: Sexp = Sexp::dangling();

/// The [`Frame::formals`] bit set once some symbol is bound twice, which only duplicate formals
/// do. Hinted lookups are off in such a frame, because a hint at the later duplicate would answer
/// with the wrong cell.
const SHADOWED: u32 = 1 << 31;

/// Marks [`Frame::removed`] as holding the position of a slot frame instead of a count
/// ([`Frame::slot_owner`]).
const SLOT_OWNER: u32 = 1 << 31;

/// GNU R's default and minimum hash table size (`HASHMINSIZE`).
const HASH_MINIMUM_SIZE: u32 = 29;

/// The end of a bucket chain in [`HashTable`].
const EMPTY_CHAIN: u32 = u32::MAX;

#[derive(Default)]
pub struct Frame {
    /// Inline, because most frames are a call's few arguments and locals, and a closure call
    /// should not pay for a second allocation beside the frame's own.
    bindings: Bindings,
    /// One pointer wide, because only large frames have one and anything wider would push
    /// every environment into a larger size class.
    index: Option<SlotIndex>,
    /// GNU R's chained table for a hashed frame, which only decides the listing order: lookups
    /// go through `index`. Boxed, for the same reason as the index.
    table: Option<Box<HashTable>>,
    /// How many leading slots are a closure's formals, which GNU R lists after the locals and
    /// in their own order.
    /// The top bit is [`SHADOWED`], kept here because a field of its own would push the
    /// environment object out of its size class.
    formals: u32,
    /// How many slots hold [`REMOVED`]. The environment of a live slot frame keeps the
    /// position of that frame on the interpreter's frame stack here instead, under
    /// [`SLOT_OWNER`], so a lookup through the environment reaches the frame without searching
    /// the stack. Such an environment removes nothing: it takes all of its frame's variables
    /// and gives the position up before a binding is removed. The position shares the field
    /// because a field of its own would push the environment object out of its size class.
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

/// Every binding a frame gains passes here, which is what makes [`gp::BOUND_ELSEWHERE`] sound:
/// the base frames keep their bindings on the symbols and never insert. A detached frame (see
/// [`gp::DETACHED`]) marks nothing, because no lookup from an attached environment passes it, and
/// it marks its whole frame once it is attached ([`Frame::mark_bound_elsewhere`]). That keeps
/// `.ArgsEnv`, `.GenericArgsEnv` and other tables under the empty environment from marking every
/// primitive. The test before the store keeps a symbol's header clean when the bit is set.
#[inline(always)]
fn mark_bound_elsewhere(symbol: Sexp) {
    // SAFETY: a frame holds only interned symbols, which live as long as the heap.
    unsafe {
        if !symbol.has_gp(gp::BOUND_ELSEWHERE) {
            symbol.set_gp_bits(gp::BOUND_ELSEWHERE, true);
        }
    }
}

/// Where machine code finds a frame's fields: tier 1 reads frame slots inline
/// (`crates/jit/src/codegen.rs`), so these offsets are part of its contract.
pub mod layout {
    use super::{Binding, Bindings, Frame};
    /// The `u64` symbol filter ([`super::filter_bit`]).
    pub const FILTER: usize = std::mem::offset_of!(Frame, filter);
    /// The `u32` formal count, whose top bit ([`SHADOWED`]) turns hints off.
    pub const FORMALS: usize = std::mem::offset_of!(Frame, formals);
    pub const SHADOWED: u32 = super::SHADOWED;
    /// The `u32` slot count.
    pub const LENGTH: usize =
        std::mem::offset_of!(Frame, bindings) + std::mem::offset_of!(Bindings, length);
    /// The `u32` capacity: [`INLINE`] while the slots are inline at [`STORAGE`], otherwise
    /// [`STORAGE`] holds a pointer to them.
    pub const CAPACITY: usize =
        std::mem::offset_of!(Frame, bindings) + std::mem::offset_of!(Bindings, capacity);
    pub const STORAGE: usize =
        std::mem::offset_of!(Frame, bindings) + std::mem::offset_of!(Bindings, storage);
    pub const INLINE: u32 = super::INLINE_BINDINGS as u32;
    /// A slot is a symbol and its cell, each a pointer.
    pub const SLOT: usize = size_of::<Binding>();
    pub const SLOT_CELL: usize = std::mem::offset_of!(Binding, cell);

    /// The [`FILTER`] bit of `symbol`.
    pub fn filter_bit(symbol: crate::object::Sexp) -> u64 {
        super::filter_bit(symbol)
    }
}

/// A frame's slots: inline up to [`INLINE_BINDINGS`], on the heap past that. Its own type
/// rather than a `SmallVec`, because machine code reads it at fixed offsets ([`layout`]).
struct Bindings {
    length: u32,
    capacity: u32,
    storage: Storage,
}

union Storage {
    inline: [MaybeUninit<Binding>; INLINE_BINDINGS],
    heap: Spilled,
}

/// A spilled frame's slots, and the bookkeeping that only a large frame needs, kept in the inline
/// buffer it no longer uses so that it costs the environment object nothing.
#[derive(Clone, Copy)]
#[repr(C)]
struct Spilled {
    /// First, because machine code loads it from [`layout::STORAGE`].
    slots: *mut Binding,
    /// How many leading slots held only old cells when a minor collection last scanned this
    /// frame as a remembered parent. Slots are appended, so a later young cell lands at or past
    /// it, and a minor collection rescans only the tail. Compaction moves cells down and resets
    /// it to 0.
    scanned: u32,
}

impl Bindings {
    const fn new() -> Bindings {
        Bindings {
            length: 0,
            capacity: INLINE_BINDINGS as u32,
            storage: Storage {
                inline: [MaybeUninit::uninit(); INLINE_BINDINGS],
            },
        }
    }

    fn spilled(&self) -> bool {
        self.capacity as usize > INLINE_BINDINGS
    }

    fn data(&self) -> *const Binding {
        // SAFETY: the capacity says which union field is live.
        unsafe {
            if self.spilled() {
                self.storage.heap.slots
            } else {
                self.storage.inline.as_ptr().cast()
            }
        }
    }

    fn data_mut(&mut self) -> *mut Binding {
        // SAFETY: as in `data`.
        unsafe {
            if self.spilled() {
                self.storage.heap.slots
            } else {
                self.storage.inline.as_mut_ptr().cast()
            }
        }
    }

    fn push(&mut self, binding: Binding) {
        if self.length == self.capacity {
            self.grow(self.capacity as usize * 2);
        }
        // SAFETY: the slot at `length` is within the capacity.
        unsafe { self.data_mut().add(self.length as usize).write(binding) };
        self.length += 1;
    }

    /// Makes room for `additional` more slots, exactly.
    fn reserve_exact(&mut self, additional: usize) {
        let needed = self.length as usize + additional;
        if needed > self.capacity as usize {
            self.grow(needed);
        }
    }

    fn grow(&mut self, capacity: usize) {
        let capacity = u32::try_from(capacity).expect("a frame has fewer than 2^32 bindings");
        let layout = Layout::array::<Binding>(capacity as usize).expect("a frame fits in memory");
        // SAFETY: the layout is non-zero, since a grown frame holds more than the inline count.
        unsafe {
            let data = alloc(layout).cast::<Binding>();
            if data.is_null() {
                handle_alloc_error(layout);
            }
            std::ptr::copy_nonoverlapping(self.data(), data, self.length as usize);
            let scanned = self.scanned() as u32;
            self.release();
            self.storage.heap = Spilled {
                slots: data,
                scanned,
            };
        }
        self.capacity = capacity;
    }

    /// Frees the heap slots, if any.
    fn release(&mut self) {
        if self.spilled() {
            let layout = Layout::array::<Binding>(self.capacity as usize)
                .expect("a frame's layout was valid when it grew");
            // SAFETY: the slots were allocated with this layout in `grow`.
            unsafe { dealloc(self.storage.heap.slots.cast(), layout) };
        }
    }

    /// See [`Spilled::scanned`]. An inline frame is small enough to rescan whole.
    fn scanned(&self) -> usize {
        // SAFETY: the capacity says which union field is live.
        unsafe {
            if self.spilled() {
                self.storage.heap.scanned as usize
            } else {
                0
            }
        }
    }

    fn set_scanned(&mut self, scanned: u32) {
        if self.spilled() {
            self.storage.heap.scanned = scanned;
        }
    }

    fn retain(&mut self, keep: impl Fn(&Binding) -> bool) {
        let data = self.data_mut();
        let mut kept = 0;
        for position in 0..self.length as usize {
            // SAFETY: both positions are below the length, and `kept <= position`.
            unsafe {
                let binding = *data.add(position);
                if keep(&binding) {
                    *data.add(kept) = binding;
                    kept += 1;
                }
            }
        }
        self.length = kept as u32;
        self.set_scanned(0);
    }
}

impl Default for Bindings {
    fn default() -> Bindings {
        Bindings::new()
    }
}

impl Drop for Bindings {
    fn drop(&mut self) {
        self.release();
    }
}

impl Deref for Bindings {
    type Target = [Binding];

    fn deref(&self) -> &[Binding] {
        // SAFETY: the first `length` slots are initialized.
        unsafe { std::slice::from_raw_parts(self.data(), self.length as usize) }
    }
}

impl DerefMut for Bindings {
    fn deref_mut(&mut self) -> &mut [Binding] {
        // SAFETY: as in `deref`.
        unsafe { std::slice::from_raw_parts_mut(self.data_mut(), self.length as usize) }
    }
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
        write(addr_of_mut!((*frame).bindings), Bindings::new());
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

    /// Each live binding's symbol and cell, in slot order.
    pub(crate) fn symbols_and_cells(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.live()
            .flat_map(|binding| [binding.symbol, binding.cell])
    }

    /// Points each binding at the cell `forward` returns for its cell, for a writer that moves
    /// cells. The symbols stay, so the index, the table and the filter stay valid.
    pub(crate) fn forward_cells(&mut self, forward: &dyn Fn(Sexp) -> Option<Sexp>) {
        for binding in self.bindings.iter_mut() {
            if binding.symbol != REMOVED
                && let Some(moved) = forward(binding.cell)
            {
                binding.cell = moved;
            }
        }
    }

    /// The cells a minor collection must mark in this frame as a remembered parent: those past
    /// the slots it found old last time (see [`Spilled::scanned`]).
    pub(crate) fn unscanned_cells(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.bindings[self.bindings.scanned()..]
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
            .map(|binding| binding.cell)
    }

    /// The cells [`Frame::unscanned_cells`] leaves out.
    pub(crate) fn scanned_cells(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.bindings[..self.bindings.scanned()]
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
            .map(|binding| binding.cell)
    }

    /// Records that every cell now in the frame is marked and survives the collection under way.
    pub(crate) fn mark_scanned(&mut self) {
        let length = self.bindings.length;
        self.bindings.set_scanned(length);
    }

    pub fn symbols(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.live().map(|binding| binding.symbol)
    }

    pub fn len(&self) -> usize {
        self.bindings.len() - self.removed() as usize
    }

    fn removed(&self) -> u32 {
        if self.removed & SLOT_OWNER == 0 {
            self.removed
        } else {
            0
        }
    }

    /// The position on the interpreter's frame stack of the live slot frame this environment
    /// stands for, or `None` for any other environment.
    #[inline(always)]
    pub fn slot_owner(&self) -> Option<usize> {
        (self.removed & SLOT_OWNER != 0).then_some((self.removed & !SLOT_OWNER) as usize)
    }

    /// Records the slot frame this environment stands for, or with `None` that it stands for
    /// none any more. The frame must hold no removed slot.
    pub fn set_slot_owner(&mut self, owner: Option<usize>) {
        debug_assert_eq!(self.removed(), 0);
        self.removed = match owner {
            Some(owner) => {
                assert!(owner < SLOT_OWNER as usize, "the frame stack is too deep");
                owner as u32 | SLOT_OWNER
            }
            None => 0,
        };
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
        let position = self.find_position(symbol)?;
        self.bindings.get(position).map(|binding| binding.cell)
    }

    /// [`Frame::find`] that tries position `*hint` first and leaves the binding's position in
    /// it. Calls of one function bind their formals and locals in the same order, so a hint a
    /// compiled lookup site kept from an earlier call answers with one comparison in the
    /// frames of later calls. A removed slot holds no real symbol, and a frame that binds a
    /// symbol twice ignores hints, so a stale hint never matches the wrong binding.
    #[inline(always)]
    pub fn find_hinted(&self, symbol: Sexp, hint: &mut u32) -> Option<Sexp> {
        // Most probes miss, so the filter comes before the hint.
        if self.filter & filter_bit(symbol) == 0 {
            return None;
        }
        if let Some(binding) = self.bindings.get(*hint as usize)
            && binding.symbol == symbol
            && self.formals & SHADOWED == 0
        {
            return Some(binding.cell);
        }
        let position = self.find_position(symbol)?;
        *hint = position as u32;
        self.bindings.get(position).map(|binding| binding.cell)
    }

    #[inline]
    fn find_position(&self, symbol: Sexp) -> Option<usize> {
        if self.filter & filter_bit(symbol) == 0 {
            return None;
        }
        match &self.index {
            Some(index) => index.find(symbol, &self.bindings),
            None => self
                .bindings
                .iter()
                .position(|binding| binding.symbol == symbol),
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
        self.formals = self.formals & SHADOWED | self.bindings.len() as u32;
    }

    /// Whether the frame may bind `symbol`: `false` only when it certainly does not.
    #[inline(always)]
    pub(crate) fn may_bind(&self, symbol: Sexp) -> bool {
        self.filter & filter_bit(symbol) != 0
    }

    /// Makes [`Frame::may_bind`] answer `true` for `symbol` before the frame binds it. A slot
    /// frame's environment expects the names its frame keeps in slots, so a lookup of any other
    /// name passes it without asking the interpreter.
    pub(crate) fn expect(&mut self, symbol: Sexp) {
        self.filter |= filter_bit(symbol);
    }

    /// Puts the bindings in the order a closure call binds them: ascending `rank`, with the
    /// first `formals` ranks being the closure's formals, and the symbols `rank` leaves out
    /// after them in the order they were bound. A hashed frame lists by its table and keeps
    /// its order.
    pub(crate) fn order_by(&mut self, rank: impl Fn(Sexp) -> Option<u32>, formals: u32) {
        if self.table.is_some() {
            return;
        }
        if self.removed() > 0 {
            self.compact();
        }
        let position = |binding: &Binding| rank(binding.symbol).unwrap_or(u32::MAX);
        let mut leading = 0;
        let mut last = 0;
        let mut ordered = true;
        for binding in self.bindings.iter() {
            let next = position(binding);
            ordered &= next >= last;
            last = next;
            leading += u32::from(next < formals);
        }
        self.formals = self.formals & SHADOWED | leading;
        // A frame that bound its variables in one step already lists them this way.
        if ordered {
            return;
        }
        self.bindings.sort_by_cached_key(position);
        self.bindings.set_scanned(0);
        if self.index.is_some() {
            self.index = Some(SlotIndex::of(&self.bindings));
        }
    }

    /// The cells in the order GNU R lists the frame in.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn listed_cells(&self) -> Vec<Sexp> {
        let Some(table) = &self.table else {
            let formals = ((self.formals & !SHADOWED) as usize).min(self.bindings.len());
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
    pub(crate) fn insert(&mut self, symbol: Sexp, cell: Sexp, attached: bool) {
        if attached {
            mark_bound_elsewhere(symbol);
        }
        if self.filter & filter_bit(symbol) != 0 && self.find_position(symbol).is_some() {
            self.formals |= SHADOWED;
        }
        let position = self.bindings.len() as u32;
        self.bindings.push(Binding { symbol, cell });
        self.filter |= filter_bit(symbol);
        if let Some(table) = &mut self.table {
            table.push(symbol);
            table.insert(position as usize);
        }
        match &mut self.index {
            Some(index) => index.insert(symbol, position, &self.bindings),
            None if self.bindings.len() > INDEX_THRESHOLD => {
                self.index = Some(SlotIndex::of(&self.bindings));
            }
            None => {}
        }
    }

    /// [`Frame::insert`] for a fresh function frame's formals, which [`Frame::write_empty`] made
    /// with room for them: the frame has no index or table yet, and a frame that would grow past
    /// [`INDEX_THRESHOLD`] bindings takes the general path, which builds the index. Only
    /// duplicate formals bind a symbol twice, and they are rare, so the duplicate scan runs only
    /// when a symbol's filter bit is already set.
    pub(crate) fn insert_formals(
        &mut self,
        cells: &[Sexp],
        symbols: impl Iterator<Item = Sexp>,
        attached: bool,
    ) {
        if self.index.is_some()
            || self.table.is_some()
            || self.bindings.len() + cells.len() > INDEX_THRESHOLD
        {
            for (symbol, &cell) in symbols.zip(cells) {
                self.insert(symbol, cell, attached);
            }
            return;
        }
        for (symbol, &cell) in symbols.zip(cells) {
            if attached {
                mark_bound_elsewhere(symbol);
            }
            let bit = filter_bit(symbol);
            if self.filter & bit != 0
                && self.bindings.iter().any(|binding| binding.symbol == symbol)
            {
                self.formals |= SHADOWED;
            }
            self.filter |= bit;
            self.bindings.push(Binding { symbol, cell });
        }
    }

    /// Marks every symbol the frame binds, when its environment becomes attached.
    pub(crate) fn mark_bound_elsewhere(&self) {
        for binding in self
            .bindings
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
        {
            mark_bound_elsewhere(binding.symbol);
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
            Some(index) => index.remove(symbol, &self.bindings)?,
            None => self
                .bindings
                .iter()
                .position(|binding| binding.symbol == symbol)?,
        };
        assert!(
            self.removed & SLOT_OWNER == 0,
            "a live slot frame's environment removed a binding"
        );
        let binding = self.bindings.get_mut(position)?;
        let cell = binding.cell;
        binding.symbol = REMOVED;
        self.removed += 1;
        if let Some(table) = &mut self.table {
            table.remove(position);
        }
        // Only formals repeat a symbol, and a duplicate behind the removed one now answers for it.
        if let Some(index) = &mut self.index
            && let Some(duplicate) = (position + 1..(self.formals & !SHADOWED) as usize)
                .find(|&later| self.bindings[later].symbol == symbol)
        {
            index.insert(symbol, duplicate as u32, &self.bindings);
        }
        if self.removed as usize * 2 > self.bindings.len() {
            self.compact();
        }
        Some(cell)
    }

    /// Drops the removed slots, keeping the live bindings in insertion order.
    fn compact(&mut self) {
        let formals = self.bindings
            [..((self.formals & !SHADOWED) as usize).min(self.bindings.len())]
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
            .count();
        if let Some(table) = &mut self.table {
            table.compact(&self.bindings);
        }
        self.bindings.retain(|binding| binding.symbol != REMOVED);
        self.formals = self.formals & SHADOWED | formals as u32;
        self.removed = 0;
        if self.index.is_some() {
            self.index = Some(SlotIndex::of(&self.bindings));
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
    /// Whether the frame owns memory outside its environment object: spilled slots, a lookup
    /// index or a hash table. Only such a frame is marked finalizable, so the sweep drops it and
    /// never reads a dead call frame that fits its inline slots. An image cannot carry those
    /// addresses either, so it stores such a frame's contents and the loader builds it again.
    pub(crate) fn owns_memory(&self) -> bool {
        self.bindings.spilled() || self.index.is_some() || self.table.is_some()
    }

    pub(crate) fn image(&self) -> FrameImage {
        FrameImage {
            slots: self
                .bindings
                .iter()
                .map(|binding| {
                    (binding.symbol != REMOVED).then_some((binding.symbol, binding.cell))
                })
                .collect(),
            formals: self.formals & !SHADOWED,
            removed: self.removed(),
            table: self.table.as_ref().map(|table| TableImage {
                heads: table.heads.clone(),
                next: table.next.clone(),
                hashes: table.hashes.clone(),
                count: table.count,
            }),
        }
    }

    /// The frame an image stored, with a removed slot's cell set to `nil`.
    pub(crate) fn from_image(image: FrameImage, nil: Sexp, attached: bool) -> Frame {
        let mut bindings = Bindings::new();
        bindings.reserve_exact(image.slots.len());
        for slot in &image.slots {
            bindings.push(match *slot {
                Some((symbol, cell)) => Binding { symbol, cell },
                None => Binding {
                    symbol: REMOVED,
                    cell: nil,
                },
            });
        }
        let mut filter = 0;
        let mut seen = FxHashSet::default();
        let mut shadowed = false;
        for binding in bindings.iter().filter(|binding| binding.symbol != REMOVED) {
            if attached {
                mark_bound_elsewhere(binding.symbol);
            }
            filter |= filter_bit(binding.symbol);
            shadowed |= !seen.insert(binding.symbol);
        }
        let index = (bindings.len() > INDEX_THRESHOLD).then(|| SlotIndex::of(&bindings));
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
            formals: if shadowed {
                image.formals | SHADOWED
            } else {
                image.formals
            },
            removed: image.removed,
            filter,
        }
    }
}

/// A large frame's lookup index: an open-addressing table with linear probing over the frame's
/// slot positions. Each entry packs the upper half of the symbol's hash above its position plus
/// one, so zero marks an empty entry, a probe that meets another symbol mostly fails on the
/// packed hash without loading that binding, and growing or deleting rehashes from the entries
/// alone. The table is at most half full, so a probe for a symbol the frame does not bind,
/// which is what most lookups through a namespace ask, ends after about one entry.
///
/// One allocation holds the capacity and the count ahead of the entries, so the frame keeps a
/// single thin pointer to it.
struct SlotIndex {
    header: NonNull<u64>,
}

/// The words ahead of a [`SlotIndex`]'s entries: its capacity, then its count.
const INDEX_HEADER: usize = 2;
const INDEX_MINIMUM_CAPACITY: usize = 64;

/// The hash both the index's entry and its home bucket derive from: a multiplicative hash of the
/// symbol's address, whose upper bits are well mixed.
#[inline(always)]
fn index_hash(symbol: Sexp) -> u32 {
    ((symbol.as_ptr() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32) as u32
}

impl SlotIndex {
    /// The index of every live binding in `bindings`, the first one winning for a symbol bound
    /// twice.
    fn of(bindings: &[Binding]) -> SlotIndex {
        let live = bindings
            .iter()
            .filter(|binding| binding.symbol != REMOVED)
            .count();
        let mut index = SlotIndex::with_capacity((live * 2).next_power_of_two());
        for (position, binding) in bindings.iter().enumerate() {
            if binding.symbol != REMOVED {
                index.insert(binding.symbol, position as u32, bindings);
            }
        }
        index
    }

    fn with_capacity(capacity: usize) -> SlotIndex {
        let capacity = capacity.max(INDEX_MINIMUM_CAPACITY);
        let layout = SlotIndex::layout(capacity);
        // SAFETY: the layout is non-zero; zeroed entries are empty.
        unsafe {
            let header = alloc_zeroed(layout).cast::<u64>();
            let Some(header) = NonNull::new(header) else {
                handle_alloc_error(layout);
            };
            header.as_ptr().write(capacity as u64);
            SlotIndex { header }
        }
    }

    fn layout(capacity: usize) -> Layout {
        Layout::array::<u64>(INDEX_HEADER + capacity).expect("a frame index fits in memory")
    }

    fn capacity(&self) -> usize {
        // SAFETY: the header words are always initialized.
        unsafe { *self.header.as_ptr() as usize }
    }

    fn count(&self) -> usize {
        // SAFETY: as in `capacity`.
        unsafe { *self.header.as_ptr().add(1) as usize }
    }

    fn set_count(&mut self, count: usize) {
        // SAFETY: as in `capacity`.
        unsafe { *self.header.as_ptr().add(1) = count as u64 }
    }

    fn entries(&self) -> &[u64] {
        // SAFETY: `capacity` entries follow the header, all initialized.
        unsafe {
            std::slice::from_raw_parts(self.header.as_ptr().add(INDEX_HEADER), self.capacity())
        }
    }

    fn entries_mut(&mut self) -> &mut [u64] {
        // SAFETY: as in `entries`.
        unsafe {
            std::slice::from_raw_parts_mut(self.header.as_ptr().add(INDEX_HEADER), self.capacity())
        }
    }

    /// The bucket a hash starts probing at in a table of `capacity` entries, a power of two.
    #[inline(always)]
    fn home(hash: u32, capacity: usize) -> usize {
        ((hash as u64 * capacity as u64) >> 32) as usize
    }

    #[inline]
    fn find(&self, symbol: Sexp, bindings: &[Binding]) -> Option<usize> {
        let hash = index_hash(symbol);
        let entries = self.entries();
        let mask = entries.len() - 1;
        let mut bucket = SlotIndex::home(hash, entries.len());
        loop {
            let entry = *entries.get(bucket)?;
            if entry == 0 {
                return None;
            }
            if (entry >> 32) as u32 == hash {
                let position = (entry as u32 - 1) as usize;
                if bindings.get(position)?.symbol == symbol {
                    return Some(position);
                }
            }
            bucket = (bucket + 1) & mask;
        }
    }

    /// Indexes `symbol` at `position`, unless the index already holds it.
    fn insert(&mut self, symbol: Sexp, position: u32, bindings: &[Binding]) {
        if self.find(symbol, bindings).is_some() {
            return;
        }
        if (self.count() + 1) * 2 > self.capacity() {
            self.grow();
        }
        let hash = index_hash(symbol);
        let entry = (hash as u64) << 32 | (position as u64 + 1);
        let count = self.count();
        self.place(entry);
        self.set_count(count + 1);
    }

    /// Writes `entry` into the first empty bucket from its home.
    fn place(&mut self, entry: u64) {
        let entries = self.entries_mut();
        let mask = entries.len() - 1;
        let mut bucket = SlotIndex::home((entry >> 32) as u32, entries.len());
        while entries[bucket] != 0 {
            bucket = (bucket + 1) & mask;
        }
        entries[bucket] = entry;
    }

    fn grow(&mut self) {
        let mut grown = SlotIndex::with_capacity(self.capacity() * 2);
        for &entry in self.entries() {
            if entry != 0 {
                grown.place(entry);
            }
        }
        grown.set_count(self.count());
        *self = grown;
    }

    /// Removes `symbol` and returns its position, shifting later entries of the same probe run
    /// back so that no lookup needs a tombstone to keep probing past the hole.
    fn remove(&mut self, symbol: Sexp, bindings: &[Binding]) -> Option<usize> {
        let hash = index_hash(symbol);
        let count = self.count();
        let entries = self.entries_mut();
        let capacity = entries.len();
        let mask = capacity - 1;
        let mut bucket = SlotIndex::home(hash, capacity);
        let position = loop {
            let entry = entries[bucket];
            if entry == 0 {
                return None;
            }
            if (entry >> 32) as u32 == hash {
                let position = (entry as u32 - 1) as usize;
                if bindings.get(position)?.symbol == symbol {
                    break position;
                }
            }
            bucket = (bucket + 1) & mask;
        };
        let mut hole = bucket;
        let mut next = bucket;
        loop {
            next = (next + 1) & mask;
            let entry = entries[next];
            if entry == 0 {
                break;
            }
            let home = SlotIndex::home((entry >> 32) as u32, capacity);
            // The entry may fill the hole when the hole lies between its home and its bucket.
            if (next.wrapping_sub(home) & mask) >= (next.wrapping_sub(hole) & mask) {
                entries[hole] = entry;
                hole = next;
            }
        }
        entries[hole] = 0;
        self.set_count(count - 1);
        Some(position)
    }
}

impl Drop for SlotIndex {
    fn drop(&mut self) {
        // SAFETY: the allocation was made in `with_capacity` with this layout.
        unsafe {
            dealloc(
                self.header.as_ptr().cast(),
                SlotIndex::layout(self.capacity()),
            )
        };
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
