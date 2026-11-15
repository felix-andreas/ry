//! The two halves of a process-wide interner: a [`Column`] that holds the values by dense
//! id, and an [`IdTable`] that finds the id of a value already stored.
//!
//! Every analysis thread interns the same few thousand types and identifiers millions of
//! times and reads them far more often still, so neither a lookup nor a read takes a lock
//! or writes shared memory. Only storing a new value takes the table's one lock.

use std::{
    alloc::{Layout, alloc, handle_alloc_error},
    cell::UnsafeCell,
    mem::MaybeUninit,
    num::NonZeroU32,
    ptr,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicPtr, AtomicU64, Ordering},
    },
};

/// An append-only column of at most `CHUNKS * CHUNK_LENGTH` values, indexed by a dense id.
///
/// The values sit in chunks of one fixed size that are allocated as the column grows and
/// never move or free, so a reference to an entry lives as long as the column, and a read is
/// a shift, a load of the chunk's address and an offset. The chunk addresses are a fixed
/// array, which is why the capacity is part of the type.
pub(crate) struct Column<T, const CHUNKS: usize> {
    chunks: [AtomicPtr<T>; CHUNKS],
}

const CHUNK_BITS: u32 = 12;
pub(crate) const CHUNK_LENGTH: usize = 1 << CHUNK_BITS;

/// A column's first chunk as a constant, for a column whose first entries exist before any
/// code runs.
pub(crate) struct FirstChunk<T>(UnsafeCell<[MaybeUninit<T>; CHUNK_LENGTH]>);

// SAFETY: the chunk is only reached through its `Column`, which hands out shared references
// to written entries and writes each entry once, before any reader can know its index.
unsafe impl<T: Sync> Sync for FirstChunk<T> {}

impl<T: Copy> FirstChunk<T> {
    pub(crate) const fn new<const N: usize>(values: [T; N]) -> FirstChunk<T> {
        assert!(N <= CHUNK_LENGTH, "the constant entries fit one chunk");
        let mut chunk = [const { MaybeUninit::uninit() }; CHUNK_LENGTH];
        let mut index = 0;
        while index < N {
            chunk[index] = MaybeUninit::new(values[index]);
            index += 1;
        }
        FirstChunk(UnsafeCell::new(chunk))
    }
}

impl<T, const CHUNKS: usize> Column<T, CHUNKS> {
    pub(crate) const CAPACITY: usize = CHUNKS << CHUNK_BITS;

    pub(crate) const fn new() -> Column<T, CHUNKS> {
        Column {
            chunks: [const { AtomicPtr::new(ptr::null_mut()) }; CHUNKS],
        }
    }

    /// A column that already holds the entries `first` was built with.
    pub(crate) const fn starting_with(first: &'static FirstChunk<T>) -> Column<T, CHUNKS> {
        let mut column = Column::new();
        column.chunks[0] = AtomicPtr::new(first.0.get().cast::<T>());
        column
    }

    /// The entry at `index`.
    ///
    /// # Safety
    ///
    /// The entry was written, by [`Column::write`] or as one of the constant first entries,
    /// and the write happened before this read: the reader got `index` from the thread that
    /// wrote it through a synchronized hand-off, such as an [`IdTable`] slot or a lock.
    pub(crate) unsafe fn get_unchecked(&self, index: usize) -> &T {
        // SAFETY: a written entry lies inside the column and its chunk is allocated, and the
        // hand-off the caller vouches for orders the chunk's address before this load.
        unsafe {
            let chunk = self
                .chunks
                .get_unchecked(index >> CHUNK_BITS)
                .load(Ordering::Relaxed);
            &*chunk.add(index & (CHUNK_LENGTH - 1))
        }
    }

    /// Writes the entry at `index`.
    ///
    /// # Safety
    ///
    /// The caller holds the one lock that serializes this column's writers, writes each
    /// index at most once, and hands `index` to no reader before this returns.
    pub(crate) unsafe fn write(&self, index: usize, value: T) {
        assert!(index < Self::CAPACITY, "the column is full");
        let slot = &self.chunks[index >> CHUNK_BITS];
        let mut chunk = slot.load(Ordering::Relaxed);
        if chunk.is_null() {
            let Ok(layout) = Layout::array::<T>(CHUNK_LENGTH) else {
                panic!("a column chunk's size overflows");
            };
            assert!(layout.size() > 0, "a column holds no zero-sized values");
            // SAFETY: the layout has a nonzero size.
            chunk = unsafe { alloc(layout) }.cast::<T>();
            if chunk.is_null() {
                handle_alloc_error(layout);
            }
            slot.store(chunk, Ordering::Release);
        }
        // SAFETY: the chunk holds `CHUNK_LENGTH` entries and no reader knows this one yet.
        unsafe { chunk.add(index & (CHUNK_LENGTH - 1)).write(value) };
    }
}

/// An open-addressing table from a value's hash to the nonzero id its owner gave it, and the
/// hand-off that publishes an id to the readers of the owner's [`Column`]s.
///
/// A slot is empty (0) or holds the upper half of the value's hash above its id, so a probe
/// compares values only where the hashes agree. At most half the slots are ever filled, so
/// every probe ends at an empty one. A slot changes once, from empty to filled, and a table
/// that grows is replaced, never rewritten, so a reader needs no lock.
///
/// `W` is what the owner changes only while it stores a value, such as the arena its values
/// live in. It sits behind the same lock that serializes the writers.
pub(crate) struct IdTable<W> {
    slots: AtomicPtr<Slots>,
    writer: Mutex<Writer<W>>,
}

struct Slots(Box<[AtomicU64]>);

struct Writer<W> {
    count: usize,
    state: W,
}

const INITIAL_SLOTS: usize = 1 << 12;

impl<W> IdTable<W> {
    pub(crate) const fn new(state: W) -> IdTable<W> {
        IdTable {
            slots: AtomicPtr::new(ptr::null_mut()),
            writer: Mutex::new(Writer { count: 0, state }),
        }
    }

    /// The id stored under `hash` that `matches` accepts.
    pub(crate) fn find(
        &self,
        hash: u64,
        matches: impl FnMut(NonZeroU32) -> bool,
    ) -> Option<NonZeroU32> {
        current(&self.slots)?.find(hash, matches)
    }

    /// Takes the lock a writer holds to store a value. A value may have been stored between
    /// the caller's [`IdTable::find`] and this lock, so the caller looks again through the
    /// guard before it stores.
    pub(crate) fn write(&self) -> TableWriter<'_, W> {
        TableWriter {
            slots: &self.slots,
            // A poisoned lock means a writer panicked. A writer changes its columns and the
            // table only after every step of its own that can panic, so both are coherent.
            writer: self
                .writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}

pub(crate) struct TableWriter<'table, W> {
    slots: &'table AtomicPtr<Slots>,
    writer: MutexGuard<'table, Writer<W>>,
}

impl<W> TableWriter<'_, W> {
    pub(crate) fn find(
        &self,
        hash: u64,
        matches: impl FnMut(NonZeroU32) -> bool,
    ) -> Option<NonZeroU32> {
        current(self.slots)?.find(hash, matches)
    }

    /// How many ids the table holds, which is the position the next value takes in a column
    /// its owner appends to under this lock.
    pub(crate) fn count(&self) -> usize {
        self.writer.count
    }

    pub(crate) fn state(&mut self) -> &mut W {
        &mut self.writer.state
    }

    /// Publishes `id` under `hash`. The owner has already appended the value to its columns,
    /// so a reader that finds the id can read them. `hash_of` gives the hash of an id the
    /// table already holds, which growing needs to place it again.
    pub(crate) fn insert(
        &mut self,
        hash: u64,
        id: NonZeroU32,
        hash_of: impl Fn(NonZeroU32) -> u64,
    ) {
        let mut slots = current(self.slots);
        if (self.writer.count + 1) * 2 > slots.map_or(0, |slots| slots.0.len()) {
            let length = slots.map_or(INITIAL_SLOTS, |slots| slots.0.len() * 2);
            // The table it replaces stays allocated, because a lookup on another thread may
            // still be probing it. The retired tables together are smaller than the live one.
            let grown: &'static Slots = Box::leak(Box::new(Slots(
                (0..length).map(|_| AtomicU64::new(0)).collect(),
            )));
            for slot in slots.iter().flat_map(|slots| slots.0.iter()) {
                if let Some(held) = NonZeroU32::new(slot.load(Ordering::Relaxed) as u32) {
                    grown.place(hash_of(held), held);
                }
            }
            self.slots
                .store(ptr::from_ref(grown).cast_mut(), Ordering::Release);
            slots = Some(grown);
        }
        if let Some(slots) = slots {
            slots.place(hash, id);
        }
        self.writer.count += 1;
    }
}

fn current(slots: &AtomicPtr<Slots>) -> Option<&'static Slots> {
    // SAFETY: a published table is leaked, so the pointer is null or valid forever.
    unsafe { slots.load(Ordering::Acquire).as_ref() }
}

impl Slots {
    fn find(&self, hash: u64, mut matches: impl FnMut(NonZeroU32) -> bool) -> Option<NonZeroU32> {
        let mask = self.0.len() - 1;
        let mut position = hash as usize & mask;
        loop {
            let slot = self.0[position].load(Ordering::Acquire);
            let id = NonZeroU32::new(slot as u32)?;
            if slot >> 32 == hash >> 32 && matches(id) {
                return Some(id);
            }
            position = (position + 1) & mask;
        }
    }

    fn place(&self, hash: u64, id: NonZeroU32) {
        let mask = self.0.len() - 1;
        let mut position = hash as usize & mask;
        while self.0[position].load(Ordering::Relaxed) != 0 {
            position = (position + 1) & mask;
        }
        self.0[position].store((hash >> 32) << 32 | u64::from(id.get()), Ordering::Release);
    }
}
