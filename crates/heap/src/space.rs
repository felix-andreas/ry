//! The small-object space. Every object of at most 2 KiB lives in one reserved range of address
//! space, cut into 64 KiB pages, and each page holds the slots of one size class. A page's
//! metadata lives outside the page, in dense per-page bitmaps with one bit per slot: whether the
//! slot is marked, whether it is allocated, and whether its object owns memory that must be
//! released when it dies. The collector tests and sets marks, and sweeps a page, with word
//! operations on those bitmaps, so neither marking an already-marked object nor freeing a dead
//! one touches the object's own memory. Marking an object for the first time also sets its
//! header's `OLD` bit, which the write barrier reads; the collector scans that object next, so
//! the header is about to be in cache anyway. An object ages one generation per collection that
//! marks it, as in GNU R: an intermediate or major collection sets the `TENURED` bit of a survivor
//! that was already old, and a per-page tenured bitmap records the same slots so that an
//! intermediate collection can unmark the younger old generation with word operations.
//!
//! Allocation bumps a pointer. The cursor of a size class claims a run of consecutive free slots
//! in the allocated bitmap, which on a fresh or emptied page is the whole page, and then hands
//! out one slot per allocation by adding the slot size to its pointer. A sweep first gives back
//! the slots a cursor claimed but never handed out, so they count as free rather than as freed
//! objects.
//!
//! Objects never move, which the C API requires, and a mark is sticky between collections, so a
//! marked object is an old one.

use {
    crate::{
        env::Frame,
        image::{ImageError, Reader, Writer},
        object::{Sexp, SexpType, Shape, flag_bits, gc_bits},
    },
    std::{collections::VecDeque, ptr::NonNull},
};

/// Slot sizes of the small-object pages. Forty bytes is a triple: a pairlist cell, a symbol, a
/// closure. Anything above the last class is a large object, allocated on its own.
pub(crate) const SLOT_SIZES: [usize; CLASS_COUNT] = [
    32, 40, 48, 64, 80, 96, 128, 160, 192, 256, 320, 384, 512, 768, 1024, 1536, 2048,
];
const CLASS_COUNT: usize = 17;
/// The size class for each object size in 8-byte words, so allocation finds its class with one
/// load instead of a search through `SLOT_SIZES`.
pub(crate) const SIZE_CLASS_BY_WORDS: [u8; 2048 / 8 + 1] = {
    let mut table = [0u8; 2048 / 8 + 1];
    let mut units = 0;
    let mut class = 0;
    while units < table.len() {
        while SLOT_SIZES[class] < units * 8 {
            class += 1;
        }
        table[units] = class as u8;
        units += 1;
    }
    table
};
pub(crate) const PAGE_SIZE: usize = 64 * 1024;
const PAGE_SHIFT: u32 = 16;
/// One bit per slot of the smallest class.
const BITMAP_WORDS: usize = PAGE_SIZE / SLOT_SIZES[0] / 64;
type Bitmap = [u64; BITMAP_WORDS];
/// Address space only: pages are committed as the heap grows, and reserving costs nothing until
/// then. It bounds the small objects one session can hold at 64 GiB. A process with a capped
/// address space (`ulimit -v`, valgrind) gets the largest power of two down to the floor.
const RESERVED_BYTES: usize = 64 << 30;
/// Where the space starts when the address is free, so that a start-up image written by one
/// process holds the addresses its objects have in another, and loads without relocating a single
/// pointer (`crate::image`). It is below 2^48, which the intern table's packed addresses need,
/// and far from where Linux, macOS and Windows place mappings and executables.
pub(crate) const IMAGE_BASE: usize = 0x2000_0000_0000;
/// Where a start-up image's large objects live: past the largest reservation of the space.
pub(crate) const IMAGE_LARGE_BASE: usize = IMAGE_BASE + RESERVED_BYTES + HUGE_PAGE_SIZE;
const MINIMUM_RESERVED_BYTES: usize = 1 << 30;
/// Pages committed per system call.
const COMMIT_PAGES: usize = HUGE_PAGE_SIZE / PAGE_SIZE;
const HUGE_PAGE_SIZE: usize = 2 << 20;
const NO_PAGE: u32 = u32::MAX;
/// The class of a page that holds no objects and can be given to any class.
const EMPTY_PAGE: u8 = u8::MAX;
/// `offset * SLOT_INDEX_MULTIPLIERS[class] >> 32` is the index of the slot at byte `offset` of a
/// page, exact for every slot start because offsets stay below 2^16.
const SLOT_INDEX_MULTIPLIERS: [u64; CLASS_COUNT] = {
    let mut multipliers = [0u64; CLASS_COUNT];
    let mut class = 0;
    while class < CLASS_COUNT {
        multipliers[class] = (1u64 << 32).div_ceil(SLOT_SIZES[class] as u64);
        class += 1;
    }
    multipliers
};
/// The byte a poisoned slot is filled with.
const POISON: u8 = 0xDB;
/// The kind byte of a poisoned slot. In a debug build it is no `SexpType`, so [`Sexp::kind`]
/// panics on a freed object; a release build skips that check and reads `FREESXP`.
pub(crate) const POISONED_KIND: u8 = if cfg!(debug_assertions) {
    POISON
} else {
    SexpType::Free as u8
};
/// The bytes of freed small slots quarantine holds back from reuse before it gives the oldest
/// back to the allocator.
const QUARANTINE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct Space {
    /// The start of the reservation, which `base` rounds up to a page boundary.
    reservation: *mut u8,
    reserved_bytes: usize,
    base: NonNull<u8>,
    /// Pages backed by memory, a prefix of the reservation.
    committed_pages: usize,
    /// Pages ever given to a class, a prefix of the committed ones.
    page_count: usize,
    page_class: Vec<u8>,
    /// Marked slots per page, as of the last sweep or since.
    live_slots: Vec<u16>,
    /// The sum of `live_slots`, kept beside it so the statistics after every collection do not
    /// walk every page.
    total_live_slots: usize,
    marks: Vec<Bitmap>,
    /// The oldest generation, whose objects carry `TENURED` in their headers. Only intermediate
    /// and major collections free a marked slot, and they drop the dead slots from these bits,
    /// so they are always a subset of the marks.
    tenured: Vec<Bitmap>,
    allocated: Vec<Bitmap>,
    /// Slots whose object owns memory outside the heap: environments, whose frames spill.
    finalizable: Vec<Bitmap>,
    /// Slots whose object is a string in the intern table. A minor collection finds the dead
    /// young strings, which it must take out of the table, from these bits on the pages it
    /// sweeps, instead of from a list of every string interned since the last collection, which
    /// would cost 8 bytes per new string.
    interned: Vec<Bitmap>,
    cursors: [Cursor; CLASS_COUNT],
    /// The cursors allocation is not using: the pretenuring ones while young objects are
    /// allocated, and the young ones while pretenuring.
    parked_cursors: [Cursor; CLASS_COUNT],
    /// Whether `cursors` are the pretenuring ones ([`Space::set_pretenuring`]).
    pretenuring: bool,
    /// Pages a pretenuring cursor entered since the last flush. They hold old objects and the
    /// pretenured ones, never a young object, so their allocated and unmarked slots are exactly
    /// the pretenured objects not yet flushed.
    pretenured_pages: Vec<u32>,
    /// Pages of each class with free slots, which no cursor is on.
    available: [Vec<u32>; CLASS_COUNT],
    empty_pages: Vec<u32>,
    /// Freed slots held back from reuse, while collection stress or verification is on.
    quarantine: Option<Quarantine>,
    /// Pages a cursor entered since the last collection: the only ones that can hold young objects.
    touched: Vec<u32>,
    /// The vector data, in 8-byte cells, of the objects marked since the heap last reset it,
    /// counted as marking reaches each object because its header is in cache then.
    pub marked_vector_cells: u64,
    /// The part of `marked_vector_cells` whose objects are tenured, counted the same way.
    pub tenured_vector_cells: u64,
    /// The header bits a newly marked object that was already old gets: `OLD` in a minor
    /// collection, and `OLD` and `TENURED` in an intermediate or major one. A young object
    /// becomes `OLD` only.
    pub promote_bits: u8,
    /// The slots allocated when a session that will be imaged began to start
    /// ([`Space::pin_allocated`]). A loading process holds objects at those addresses, so
    /// compacting the image leaves them where they are.
    pinned: Vec<Bitmap>,
}

#[derive(Clone, Copy)]
struct Cursor {
    /// The next slot to hand out.
    next: *mut u8,
    /// The end of the run of claimed free slots `next` walks through.
    end: *mut u8,
    page: u32,
    /// The slot of `page` where the search for the next run starts.
    next_slot: u32,
}

const IDLE_CURSOR: Cursor = Cursor {
    next: std::ptr::null_mut(),
    end: std::ptr::null_mut(),
    page: NO_PAGE,
    next_slot: 0,
};

/// Freed slots that are neither allocated nor free: they hold poison, and the allocator skips
/// them until they leave the quarantine, oldest first, once it holds more than
/// [`QUARANTINE_BYTES`]. A read through a stale reference then hits poison instead of an object
/// that has since taken the slot, which is what lets collection stress catch a use after free.
struct Quarantine {
    /// The quarantined slots of each page.
    slots: Vec<Bitmap>,
    /// The quarantined slots in the order they were freed, a bitmap word at a time.
    queue: VecDeque<QuarantinedWord>,
    bytes: usize,
}

struct QuarantinedWord {
    page: u32,
    word: u32,
    bits: u64,
}

/// What a sweep found.
#[derive(Default)]
pub(crate) struct Swept {
    pub freed: u64,
    /// Bytes of the dead objects outside the oldest generation, which an intermediate
    /// collection would have freed too.
    pub untenured_bytes: usize,
}

impl Space {
    pub fn new() -> Space {
        let mut reserved_bytes = RESERVED_BYTES;
        // The first space of a process takes the image address; any other, and a process whose
        // address space has no room there, reserves wherever the system puts it and cannot load
        // an image.
        let fixed = virtual_memory::reserve_at(IMAGE_BASE as *mut u8, RESERVED_BYTES);
        let base_mapping = loop {
            if let Some(mapping) = fixed {
                break mapping;
            }
            if let Some(mapping) = virtual_memory::reserve(reserved_bytes) {
                break mapping;
            }
            if reserved_bytes == MINIMUM_RESERVED_BYTES {
                panic!(
                    "cannot reserve 1 GiB of address space for the R heap: {}",
                    std::io::Error::last_os_error()
                );
            }
            reserved_bytes /= 2;
        };
        // A reservation is aligned to the system page (4 KiB on Unix). The heap starts at the
        // next 2 MiB boundary, so each commit covers whole transparent huge pages.
        let aligned = (base_mapping as usize).next_multiple_of(HUGE_PAGE_SIZE);
        let base = NonNull::new(aligned as *mut u8)
            .unwrap_or_else(|| panic!("mmap returned a null reservation"));
        Space {
            reservation: base_mapping,
            reserved_bytes,
            base,
            committed_pages: 0,
            page_count: 0,
            page_class: Vec::new(),
            live_slots: Vec::new(),
            total_live_slots: 0,
            marks: Vec::new(),
            tenured: Vec::new(),
            allocated: Vec::new(),
            finalizable: Vec::new(),
            interned: Vec::new(),
            cursors: [IDLE_CURSOR; CLASS_COUNT],
            parked_cursors: [IDLE_CURSOR; CLASS_COUNT],
            pretenuring: false,
            pretenured_pages: Vec::new(),
            available: Default::default(),
            empty_pages: Vec::new(),
            quarantine: None,
            touched: Vec::new(),
            marked_vector_cells: 0,
            tenured_vector_cells: 0,
            promote_bits: gc_bits::OLD,
            pinned: Vec::new(),
        }
    }

    /// A slot of `class`, or `None` when the reservation is exhausted. Its memory is whatever
    /// the slot last held.
    #[inline(always)]
    pub fn allocate(&mut self, class: usize) -> Option<NonNull<u8>> {
        let cursor = &mut self.cursors[class];
        let slot = cursor.next;
        if slot != cursor.end {
            // SAFETY: `slot` is a claimed slot of the cursor's run, so the next one is at most
            // the run's end.
            cursor.next = unsafe { slot.add(SLOT_SIZES[class]) };
            // SAFETY: a run lies inside a committed page, never at address zero.
            return Some(unsafe { NonNull::new_unchecked(slot) });
        }
        self.refill(class)
    }

    /// [`Space::allocate`] without the refill: the next slot of the cursor's current run, or
    /// `None` when the run is used up. It makes no call, so an interpreter fast path that
    /// allocates through it needs no stack frame and leaves the refill to its slow path.
    #[inline(always)]
    pub fn allocate_from_run(&mut self, class: usize) -> Option<NonNull<u8>> {
        let cursor = &mut self.cursors[class];
        let slot = cursor.next;
        if slot == cursor.end {
            return None;
        }
        // SAFETY: as in `allocate`.
        cursor.next = unsafe { slot.add(SLOT_SIZES[class]) };
        // SAFETY: as in `allocate`.
        Some(unsafe { NonNull::new_unchecked(slot) })
    }

    /// Claims the cursor's next run of free slots, moving to another page when its page has
    /// none left, and hands out the run's first slot.
    #[cold]
    #[inline(never)]
    fn refill(&mut self, class: usize) -> Option<NonNull<u8>> {
        // One test per refill keeps the quarantine out of the claim loop, so the allocator
        // without quarantine runs the same code as if quarantine did not exist.
        if self.quarantine.is_some() {
            return self.refill_quarantined(class);
        }
        self.refill_from::<false>(class)
    }

    #[cold]
    #[inline(never)]
    fn refill_quarantined(&mut self, class: usize) -> Option<NonNull<u8>> {
        self.refill_from::<true>(class)
    }

    /// [`Space::refill`], skipping quarantined slots when `QUARANTINE` is set.
    #[inline(always)]
    fn refill_from<const QUARANTINE: bool>(&mut self, class: usize) -> Option<NonNull<u8>> {
        let slot_size = SLOT_SIZES[class];
        let slots = PAGE_SIZE / slot_size;
        loop {
            let cursor = self.cursors[class];
            if cursor.page != NO_PAGE {
                let page = cursor.page as usize;
                let from = cursor.next_slot as usize;
                let held = if QUARANTINE {
                    self.quarantine
                        .as_ref()
                        .and_then(|quarantine| quarantine.slots.get(page))
                } else {
                    None
                };
                if let Some((start, end)) = claim_run(&mut self.allocated[page], held, slots, from)
                {
                    // SAFETY: both offsets are inside a committed page.
                    let page_base = unsafe { self.base.as_ptr().add(page * PAGE_SIZE) };
                    let next = unsafe { page_base.add(start * slot_size) };
                    self.cursors[class] = Cursor {
                        // SAFETY: the run holds at least one slot.
                        next: unsafe { next.add(slot_size) },
                        end: unsafe { page_base.add(end * slot_size) },
                        page: cursor.page,
                        next_slot: end as u32,
                    };
                    return NonNull::new(next);
                }
            }
            let page = match self.available[class].pop() {
                Some(page) => page,
                None => {
                    let page = match self.empty_pages.pop() {
                        Some(page) => page,
                        None => self.new_page()?,
                    };
                    self.page_class[page as usize] = class as u8;
                    page
                }
            };
            if self.pretenuring {
                self.pretenured_pages.push(page);
            } else {
                self.touched.push(page);
            }
            self.cursors[class] = Cursor {
                page,
                ..IDLE_CURSOR
            };
        }
    }

    fn new_page(&mut self) -> Option<u32> {
        if self.page_count == self.committed_pages {
            let commit = COMMIT_PAGES
                .min((self.reserved_bytes - HUGE_PAGE_SIZE) / PAGE_SIZE - self.committed_pages);
            if commit == 0 {
                return None;
            }
            // SAFETY: the range lies inside the reservation, past every committed page.
            let committed = unsafe {
                virtual_memory::commit(
                    self.base.as_ptr().add(self.committed_pages * PAGE_SIZE),
                    commit * PAGE_SIZE,
                )
            };
            if !committed {
                return None;
            }
            self.committed_pages += commit;
        }
        let page = self.page_count;
        self.page_count += 1;
        self.page_class.push(EMPTY_PAGE);
        self.live_slots.push(0);
        self.marks.push([0; BITMAP_WORDS]);
        self.tenured.push([0; BITMAP_WORDS]);
        self.allocated.push([0; BITMAP_WORDS]);
        self.finalizable.push([0; BITMAP_WORDS]);
        self.interned.push([0; BITMAP_WORDS]);
        if let Some(quarantine) = &mut self.quarantine {
            quarantine.slots.push([0; BITMAP_WORDS]);
        }
        Some(page as u32)
    }

    /// The page, word and bit of `object`'s slot, or `None` for a large object.
    #[inline]
    fn locate(&self, object: Sexp) -> Option<(usize, usize, u64)> {
        let offset = (object.as_ptr() as usize).wrapping_sub(self.base.as_ptr() as usize);
        let page = offset >> PAGE_SHIFT;
        let class = *self.page_class.get(page)? as usize;
        let in_page = (offset & (PAGE_SIZE - 1)) as u64;
        let slot = (in_page * SLOT_INDEX_MULTIPLIERS.get(class)?) >> 32;
        Some((page, (slot >> 6) as usize, 1 << (slot & 63)))
    }

    #[inline]
    pub unsafe fn is_marked(&self, object: Sexp) -> bool {
        match self.locate(object) {
            Some((page, word, bit)) => self.marks[page][word] & bit != 0,
            None => unsafe { object.gc_bits() & gc_bits::MARK != 0 },
        }
    }

    /// Marks `object` and says whether it was unmarked. A newly marked object ages: it gets
    /// [`Space::promote_bits`] if it was already old, and `OLD` if it was young.
    ///
    /// A minor collection finds most children old and marked already, so that test is inlined
    /// into every tracing loop, and the work for a newly marked object is out of line.
    #[inline(always)]
    pub unsafe fn mark(&mut self, object: Sexp) -> bool {
        let Some((page, word, bit)) = self.locate(object) else {
            return unsafe { self.mark_large(object) };
        };
        // `locate` found `page` in `page_class`, which every bitmap matches in length, and a
        // slot of the smallest class at the end of a page still has its bit in the last word.
        let marks = unsafe { self.marks.get_unchecked_mut(page).get_unchecked_mut(word) };
        if *marks & bit != 0 {
            return false;
        }
        *marks |= bit;
        unsafe { self.age(object, Some((page, word, bit))) };
        true
    }

    #[inline(never)]
    unsafe fn mark_large(&mut self, object: Sexp) -> bool {
        if unsafe { object.gc_bits() } & gc_bits::MARK != 0 {
            return false;
        }
        unsafe {
            object.set_gc_bit(gc_bits::MARK, true);
            self.age(object, None);
        }
        true
    }

    #[inline(never)]
    unsafe fn age(&mut self, object: Sexp, location: Option<(usize, usize, u64)>) {
        let bits = unsafe { object.gc_bits() };
        let promote = if bits & gc_bits::OLD == 0 {
            gc_bits::OLD
        } else {
            self.promote_bits
        };
        // A major collection marks every live object again, so the test keeps it from storing
        // into the header of each one that is already promoted.
        if bits & promote != promote {
            unsafe { object.set_gc_bit(promote, true) };
        }
        let cells = unsafe { vector_cells(object) };
        self.marked_vector_cells += cells;
        if promote & gc_bits::TENURED != 0 {
            self.tenured_vector_cells += cells;
            if let Some((page, word, bit)) = location {
                self.tenured[page][word] |= bit;
            }
        }
    }

    /// [`Space::mark`] for a child of a tenured object in a deeper collection, which must also
    /// learn whether the child is tenured once marked. Reading that bit beside the mark saves
    /// locating the slot a second time.
    #[inline(always)]
    pub unsafe fn mark_beside_tenured(&mut self, object: Sexp) -> (bool, bool) {
        let Some((page, word, bit)) = self.locate(object) else {
            let newly = unsafe { self.mark_large(object) };
            return (newly, unsafe { object.gc_bits() } & gc_bits::TENURED != 0);
        };
        // The bounds hold for the reason `Space::mark` gives.
        let marks = unsafe { self.marks.get_unchecked_mut(page).get_unchecked_mut(word) };
        let newly = *marks & bit == 0;
        if newly {
            *marks |= bit;
            unsafe { self.age(object, Some((page, word, bit))) };
        }
        let tenured = unsafe { *self.tenured.get_unchecked(page).get_unchecked(word) } & bit != 0;
        (newly, tenured)
    }

    /// Records that `object`, an environment in this space, has a frame that owns memory, which
    /// the sweep drops when the environment dies.
    pub fn set_finalizable(&mut self, object: Sexp) {
        if let Some((page, word, bit)) = self.locate(object) {
            self.finalizable[page][word] |= bit;
        }
    }

    /// Records that `object` is in the intern table, and says whether it lives in this space; a
    /// string too long for any slot is a large object, which the caller tracks itself.
    pub fn set_interned(&mut self, object: Sexp) -> bool {
        match self.locate(object) {
            Some((page, word, bit)) => {
                self.interned[page][word] |= bit;
                true
            }
            None => false,
        }
    }

    /// The interned strings allocated since the last collection that it has not marked. Only
    /// pages a cursor entered since then can hold them.
    pub fn unmarked_young_interned(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.touched.iter().flat_map(move |&page| {
            let page = page as usize;
            let slot_size = SLOT_SIZES[self.page_class[page] as usize];
            (0..BITMAP_WORDS).flat_map(move |word| {
                let bits = self.interned[page][word] & !self.marks[page][word];
                set_bits(bits).map(move |bit| {
                    let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                    // SAFETY: an interned slot holds a string of a committed page.
                    unsafe { Sexp::from_non_null(self.base.add(offset).cast()) }
                })
            })
        })
    }

    /// Marks every interned string allocated since the last collection.
    pub unsafe fn mark_young_interned(&mut self) {
        for index in 0..self.touched.len() {
            let page = self.touched[index] as usize;
            let slot_size = SLOT_SIZES[self.page_class[page] as usize];
            for word in 0..BITMAP_WORDS {
                let unmarked = self.interned[page][word] & !self.marks[page][word];
                for bit in set_bits(unmarked) {
                    let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                    // SAFETY: an interned slot holds a string of a committed page.
                    unsafe { self.mark(Sexp::from_non_null(self.base.add(offset).cast())) };
                }
            }
        }
    }

    /// Moves every small object into the oldest generation without tracing, and returns their
    /// vector cells. A tenured object may point only at tenured objects, which holds trivially
    /// once every object is tenured.
    pub unsafe fn tenure_all(&mut self) -> u64 {
        // The unused tail of a cursor's run counts as allocated until it is released.
        self.release_cursors();
        let mut cells = 0;
        for page in 0..self.allocated.len() {
            let Some(&slot_size) = SLOT_SIZES.get(self.page_class[page] as usize) else {
                continue;
            };
            for word in 0..BITMAP_WORDS {
                let live = self.allocated[page][word];
                for bit in set_bits(live) {
                    let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                    // SAFETY: an allocated slot of a committed page holds an object.
                    unsafe {
                        let object = Sexp::from_non_null(self.base.add(offset).cast());
                        if object.gc_bits() & (gc_bits::OLD | gc_bits::TENURED)
                            != gc_bits::OLD | gc_bits::TENURED
                        {
                            object.set_gc_bit(gc_bits::OLD | gc_bits::TENURED, true);
                        }
                        cells += vector_cells(object);
                    }
                }
                self.marks[page][word] |= live;
                self.tenured[page][word] |= live;
            }
        }
        cells
    }

    /// Clears every mark, as a major collection starts.
    pub fn clear_marks(&mut self) {
        self.marks.fill([0; BITMAP_WORDS]);
    }

    /// Unmarks the objects outside the oldest generation, as an intermediate collection starts.
    pub fn unmark_untenured(&mut self) {
        for (marks, tenured) in self.marks.iter_mut().zip(&self.tenured) {
            for (mark, tenured) in marks.iter_mut().zip(tenured) {
                *mark &= tenured;
            }
        }
    }

    /// Drops the dead slots from the tenured bits, after an intermediate or major collection
    /// swept.
    pub fn forget_dead_tenured(&mut self) {
        for (tenured, marks) in self.tenured.iter_mut().zip(&self.marks) {
            for (tenured, mark) in tenured.iter_mut().zip(marks) {
                *tenured &= mark;
            }
        }
    }

    /// Bytes of the old objects outside the oldest generation, which outside a collection are
    /// the marked slots that are not tenured: what an intermediate collection can free.
    pub fn younger_old_bytes(&self) -> usize {
        let mut bytes = 0;
        for page in 0..self.page_count {
            let Some(&slot_size) = SLOT_SIZES.get(self.page_class[page] as usize) else {
                continue;
            };
            let slots: u32 = self.marks[page]
                .iter()
                .zip(&self.tenured[page])
                .map(|(marks, tenured)| (marks & !tenured).count_ones())
                .sum();
            bytes += slots as usize * slot_size;
        }
        bytes
    }

    /// Whether `object`, a small object, is in the tenured bitmap; `None` for a large object.
    pub fn is_tenured_slot(&self, object: Sexp) -> Option<bool> {
        self.locate(object)
            .map(|(page, word, bit)| self.tenured[page][word] & bit != 0)
    }

    /// The objects marked so far, which outside a collection are the old ones.
    pub fn marked_objects(&self) -> impl Iterator<Item = Sexp> + '_ {
        (0..self.page_count).flat_map(move |page| {
            // An empty page has no marks, so its slot size never matters.
            let slot_size = SLOT_SIZES
                .get(self.page_class[page] as usize)
                .copied()
                .unwrap_or(0);
            self.marks[page]
                .iter()
                .enumerate()
                .flat_map(move |(word, &bits)| {
                    set_bits(bits).map(move |bit| {
                        let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                        // SAFETY: a marked slot holds an object of a committed page.
                        unsafe { Sexp::from_non_null(self.base.add(offset).cast()) }
                    })
                })
        })
    }

    /// Frees the unmarked objects of the pages allocated into since the last collection: the
    /// young ones, after a minor collection marked every survivor.
    pub unsafe fn sweep_young(&mut self, swept: &mut Swept) -> isize {
        self.release_cursors();
        self.release_quarantine(QUARANTINE_BYTES);
        let touched = std::mem::take(&mut self.touched);
        let mut live_bytes = 0;
        for &page in &touched {
            live_bytes += unsafe { self.sweep_page(page as usize, swept) };
        }
        for &page in &touched {
            self.file_page(page);
        }
        self.touched = touched;
        self.touched.clear();
        self.reset_cursors();
        live_bytes
    }

    /// Frees every unmarked object, after a major collection marked everything reachable.
    /// Returns the bytes the survivors take.
    pub unsafe fn sweep_all(&mut self, swept: &mut Swept) -> usize {
        self.release_cursors();
        for available in &mut self.available {
            available.clear();
        }
        self.empty_pages.clear();
        self.touched.clear();
        self.release_quarantine(QUARANTINE_BYTES);
        let mut live_bytes = 0;
        for page in 0..self.page_count {
            if self.page_class[page] == EMPTY_PAGE {
                self.empty_pages.push(page as u32);
                continue;
            }
            unsafe { self.sweep_page(page, swept) };
            live_bytes +=
                self.live_slots[page] as usize * SLOT_SIZES[self.page_class[page] as usize];
            self.file_page(page as u32);
        }
        self.reset_cursors();
        live_bytes
    }

    /// Sweeps one page and returns the change in the bytes its live objects take.
    unsafe fn sweep_page(&mut self, page: usize, swept: &mut Swept) -> isize {
        // Poisoning makes a use after free fail loudly, but costs a store per byte of every
        // dead slot, so only debug builds and quarantine pay for it. The two instances keep
        // the release sweep without quarantine free of the poisoning code, whose writes
        // through `self` would make the word loop reload the page's bitmaps.
        if cfg!(debug_assertions) || self.quarantine.is_some() {
            unsafe { self.sweep_page_words::<true>(page, swept) }
        } else {
            unsafe { self.sweep_page_words::<false>(page, swept) }
        }
    }

    #[inline(always)]
    unsafe fn sweep_page_words<const POISON: bool>(
        &mut self,
        page: usize,
        swept: &mut Swept,
    ) -> isize {
        let class = self.page_class[page] as usize;
        let slot_size = SLOT_SIZES[class];
        let page_base = unsafe { self.base.as_ptr().add(page * PAGE_SIZE) };
        let mut live = 0;
        for word in 0..BITMAP_WORDS {
            let marks = self.marks[page][word];
            let dead = self.allocated[page][word] & !marks;
            if dead != 0 {
                let finalizable = self.finalizable[page][word] & dead;
                for bit in set_bits(finalizable) {
                    let object = unsafe {
                        Sexp::from_non_null(
                            NonNull::new_unchecked(page_base.add((word * 64 + bit) * slot_size))
                                .cast(),
                        )
                    };
                    unsafe { drop_frame(object) };
                }
                self.finalizable[page][word] &= !dead;
                if POISON {
                    for bit in set_bits(dead) {
                        unsafe { poison(page_base.add((word * 64 + bit) * slot_size), slot_size) };
                    }
                    if let Some(quarantine) = &mut self.quarantine {
                        quarantine.slots[page][word] |= dead;
                        quarantine.queue.push_back(QuarantinedWord {
                            page: page as u32,
                            word: word as u32,
                            bits: dead,
                        });
                        quarantine.bytes += dead.count_ones() as usize * slot_size;
                    }
                }
                swept.freed += u64::from(dead.count_ones());
                swept.untenured_bytes +=
                    (dead & !self.tenured[page][word]).count_ones() as usize * slot_size;
            }
            self.allocated[page][word] = marks;
            self.interned[page][word] &= marks;
            live += marks.count_ones();
        }
        let before = self.live_slots[page] as isize;
        self.live_slots[page] = live as u16;
        self.total_live_slots = (self.total_live_slots as isize + live as isize - before) as usize;
        (live as isize - before) * slot_size as isize
    }

    /// Puts a swept page where the allocator finds it: back with its class when it has free
    /// slots, among the empty pages when it has no objects.
    fn file_page(&mut self, page: u32) {
        let index = page as usize;
        let class = self.page_class[index] as usize;
        if self.cursors[class].page == page {
            return;
        }
        let live = self.live_slots[index] as usize;
        if live == 0 && !self.holds_quarantined(index) {
            self.page_class[index] = EMPTY_PAGE;
            self.empty_pages.push(page);
        } else if live < PAGE_SIZE / SLOT_SIZES[class] {
            self.available[class].push(page);
        }
    }

    /// Starts or stops holding freed slots back from reuse. Stopping gives every quarantined
    /// slot back to the allocator.
    pub fn set_quarantine(&mut self, on: bool) {
        if on && self.quarantine.is_none() {
            self.quarantine = Some(Quarantine {
                slots: vec![[0; BITMAP_WORDS]; self.page_count],
                queue: VecDeque::new(),
                bytes: 0,
            });
        } else if !on {
            self.quarantine = None;
        }
    }

    pub fn quarantining(&self) -> bool {
        self.quarantine.is_some()
    }

    fn holds_quarantined(&self, page: usize) -> bool {
        self.quarantine
            .as_ref()
            .and_then(|quarantine| quarantine.slots.get(page))
            .is_some_and(|slots| slots.iter().any(|&word| word != 0))
    }

    /// Gives the oldest quarantined slots back to the allocator until at most `limit` bytes
    /// stay. A sweep calls this before it frees anything, so the quarantine holds the last
    /// sweep's dead slots in full; a page that empties keeps its class until a sweep files it.
    fn release_quarantine(&mut self, limit: usize) {
        let Some(quarantine) = &mut self.quarantine else {
            return;
        };
        while quarantine.bytes > limit {
            let Some(entry) = quarantine.queue.pop_front() else {
                break;
            };
            let page = entry.page as usize;
            if let Some(word) = quarantine
                .slots
                .get_mut(page)
                .and_then(|slots| slots.get_mut(entry.word as usize))
            {
                *word &= !entry.bits;
            }
            let slot_size = SLOT_SIZES[self.page_class[page] as usize];
            quarantine.bytes -= entry.bits.count_ones() as usize * slot_size;
        }
    }

    /// Gives back the slots each cursor claimed but never handed out, so the sweep neither
    /// counts them as freed objects nor reads them.
    fn release_cursors(&mut self) {
        release_runs(&mut self.cursors, &mut self.allocated, self.base);
    }

    /// Restarts each cursor at the beginning of its page, whose free slots the sweep changed,
    /// and counts its page as touched again.
    fn reset_cursors(&mut self) {
        for cursor in &mut self.cursors {
            if cursor.page != NO_PAGE {
                self.touched.push(cursor.page);
                *cursor = Cursor {
                    page: cursor.page,
                    ..IDLE_CURSOR
                };
            }
        }
    }

    /// Adds the objects the coming sweep frees to `by_kind`, by type code: those of the pages
    /// allocated into since the last collection, or with `all`, of every page. Reading each
    /// dead object's header costs a cache miss per object, so only the profile summary asks.
    pub fn count_dead_by_kind(&self, all: bool, by_kind: &mut [u64; 32]) {
        let mut count_page = |page: usize| {
            let slot_size = SLOT_SIZES[self.page_class[page] as usize];
            let page_base = self.base.as_ptr().wrapping_add(page * PAGE_SIZE);
            for word in 0..BITMAP_WORDS {
                for bit in set_bits(self.allocated[page][word] & !self.marks[page][word]) {
                    // SAFETY: an allocated slot of a committed page starts with its kind byte.
                    let kind = unsafe { *page_base.add((word * 64 + bit) * slot_size) };
                    if let Some(count) = by_kind.get_mut(usize::from(kind & 31)) {
                        *count += 1;
                    }
                }
            }
        };
        if all {
            (0..self.page_count)
                .filter(|&page| self.page_class[page] != EMPTY_PAGE)
                .for_each(&mut count_page);
        } else {
            self.touched
                .iter()
                .for_each(|&page| count_page(page as usize));
        }
    }

    pub fn live_slots(&self) -> usize {
        self.total_live_slots
    }

    /// The address of the first page.
    pub fn base(&self) -> usize {
        self.base.as_ptr() as usize
    }

    /// Switches allocation between young objects and pretenured ones, which are old from the
    /// next flush on ([`Space::flush_pretenured`]). Each has its own cursors, so the inline
    /// allocation path is the same for both, and a pretenured object never shares a page with
    /// a young one.
    pub fn set_pretenuring(&mut self, on: bool) {
        if self.pretenuring != on {
            std::mem::swap(&mut self.cursors, &mut self.parked_cursors);
            self.pretenuring = on;
        }
    }

    pub fn has_pretenured(&self) -> bool {
        !self.pretenured_pages.is_empty()
    }

    /// Ends the pretenuring cursors' runs and gives each pretenured object not yet flushed the
    /// `OLD` header bit. Returns their vector cells. The objects stay unmarked until
    /// [`Space::flush_pretenured`], so [`Space::for_each_pretenured`] still visits them.
    pub unsafe fn age_pretenured(&mut self) -> u64 {
        debug_assert!(!self.pretenuring);
        release_runs(&mut self.parked_cursors, &mut self.allocated, self.base);
        self.parked_cursors = [IDLE_CURSOR; CLASS_COUNT];
        let mut cells = 0;
        self.for_each_pretenured(|object| {
            // SAFETY: an allocated slot of a committed page holds an object.
            unsafe {
                object.set_gc_bit(gc_bits::OLD, true);
                cells += vector_cells(object);
            }
        });
        cells
    }

    /// Calls `visit` on each pretenured object not yet flushed.
    pub fn for_each_pretenured(&self, mut visit: impl FnMut(Sexp)) {
        for &page in &self.pretenured_pages {
            let page = page as usize;
            let slot_size = SLOT_SIZES[self.page_class[page] as usize];
            let page_base = page * PAGE_SIZE;
            for word in 0..BITMAP_WORDS {
                let mut bits = self.allocated[page][word] & !self.marks[page][word];
                while bits != 0 {
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let offset = page_base + (word * 64 + bit) * slot_size;
                    // SAFETY: an allocated slot of a committed page holds an object.
                    visit(unsafe { Sexp::from_non_null(self.base.add(offset).cast()) });
                }
            }
        }
    }

    /// Marks the pretenured objects, which [`Space::age_pretenured`] made old, and puts their
    /// pages back where the allocator finds them. Returns the bytes they take.
    pub fn flush_pretenured(&mut self) -> usize {
        let pages = std::mem::take(&mut self.pretenured_pages);
        let mut bytes = 0;
        for &page in &pages {
            let index = page as usize;
            let mut live = 0;
            for word in 0..BITMAP_WORDS {
                self.marks[index][word] |= self.allocated[index][word];
                live += self.marks[index][word].count_ones();
            }
            let added = live as usize - self.live_slots[index] as usize;
            self.live_slots[index] = live as u16;
            self.total_live_slots += added;
            bytes += added * SLOT_SIZES[self.page_class[index] as usize];
        }
        for &page in &pages {
            self.file_page(page);
        }
        self.pretenured_pages = pages;
        self.pretenured_pages.clear();
        bytes
    }

    /// Whether the space starts at [`IMAGE_BASE`], where an image's pages belong.
    pub fn at_image_base(&self) -> bool {
        self.base.as_ptr() as usize == IMAGE_BASE
    }

    /// Feeds the state that decides where the next objects go into `digest`: which slots are
    /// taken, what each page holds and where each cursor stands.
    pub fn digest_allocation(&self, digest: &mut crate::image::Digest) {
        digest.word(self.page_count as u64);
        for page in 0..self.page_count {
            digest.word(u64::from(self.page_class[page]));
            for &word in &self.allocated[page] {
                digest.word(word);
            }
        }
        for cursor in &self.cursors {
            digest.word(cursor.next as u64);
            digest.word(cursor.end as u64);
            digest.word(u64::from(cursor.page));
            digest.word(u64::from(cursor.next_slot));
        }
    }

    /// Writes the per-page metadata of an image to `out` and returns the bytes of the pages it
    /// describes, starting at the space's base. Every cursor is released first, so the slots a
    /// cursor claimed count as free and the loader's cursors start idle.
    pub fn write_image(&mut self, out: &mut Writer) -> &[u8] {
        self.release_cursors();
        self.cursors = [IDLE_CURSOR; CLASS_COUNT];
        out.u32(self.page_count as u32);
        out.bytes
            .extend_from_slice(&self.page_class[..self.page_count]);
        for &live in &self.live_slots[..self.page_count] {
            out.u16(live);
        }
        for bitmaps in [
            &self.marks,
            &self.tenured,
            &self.allocated,
            &self.finalizable,
            &self.interned,
        ] {
            for bitmap in &bitmaps[..self.page_count] {
                for &word in bitmap {
                    out.u64(word);
                }
            }
        }
        // SAFETY: the first `page_count` pages are committed.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr(), self.page_count * PAGE_SIZE) }
    }

    /// The free slots of the image's pages, as `(start, length)` byte ranges from the base, so a
    /// writer can clear what they last held.
    pub fn free_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        for page in 0..self.page_count {
            let Some(&slot_size) = SLOT_SIZES.get(self.page_class[page] as usize) else {
                ranges.push((page * PAGE_SIZE, PAGE_SIZE));
                continue;
            };
            let slots = PAGE_SIZE / slot_size;
            for slot in 0..slots {
                if self.allocated[page][slot / 64] & (1 << (slot % 64)) == 0 {
                    ranges.push((page * PAGE_SIZE + slot * slot_size, slot_size));
                }
            }
            // The bytes past a page's last slot.
            let tail = slots * slot_size;
            if tail < PAGE_SIZE {
                ranges.push((page * PAGE_SIZE + tail, PAGE_SIZE - tail));
            }
        }
        ranges
    }

    /// Records the slots allocated now as ones [`Space::compact_for_image`] must not move.
    pub fn pin_allocated(&mut self) {
        // A cursor's claimed run counts as allocated, but nothing holds its slots yet.
        let mut pinned = self.allocated[..self.page_count].to_vec();
        for cursors in [self.cursors, self.parked_cursors] {
            release_runs(&mut { cursors }, &mut pinned, self.base);
        }
        self.pinned = pinned;
    }

    /// Whether `object` is a small object in a slot [`Space::pin_allocated`] recorded.
    pub fn is_pinned(&self, object: Sexp) -> bool {
        self.locate(object).is_some_and(|(page, word, bit)| {
            self.pinned
                .get(page)
                .is_some_and(|bitmap| bitmap[word] & bit != 0)
        })
    }

    /// Whether `object` is a small object in the intern table.
    pub fn is_interned(&self, object: Sexp) -> bool {
        self.locate(object)
            .is_some_and(|(page, word, bit)| self.interned[page][word] & bit != 0)
    }

    /// Whether `object` lives in this space rather than among the large objects.
    pub fn holds(&self, object: Sexp) -> bool {
        self.locate(object).is_some()
    }

    /// Moves each object of `order`, which are small, marked and unpinned, into a slot of its
    /// class laid out in that order, and returns each move as `(from, to)`. The first objects of
    /// a class fill the free slots of the pages that keep an object, in page order; the rest go
    /// to the emptied and then to new pages, a page taken whenever a class needs one, so an
    /// object's page follows the order of the objects that first needed it. Every collection
    /// bit moves with its object; the caller points every reference at the new addresses.
    pub unsafe fn compact_for_image(&mut self, order: &[Sexp]) -> Vec<(Sexp, Sexp)> {
        self.release_cursors();
        release_runs(&mut self.parked_cursors, &mut self.allocated, self.base);
        self.cursors = [IDLE_CURSOR; CLASS_COUNT];
        self.parked_cursors = [IDLE_CURSOR; CLASS_COUNT];
        self.pretenuring = false;
        self.pretenured_pages.clear();
        self.touched.clear();
        struct Moved {
            from: Sexp,
            class: usize,
            start: usize,
            tenured: bool,
            interned: bool,
            finalizable: bool,
        }
        let mut saved: Vec<u8> = Vec::new();
        let mut moving: Vec<Moved> = Vec::with_capacity(order.len());
        for &object in order {
            let Some((page, word, bit)) = self.locate(object) else {
                continue;
            };
            if self.marks[page][word] & bit == 0 || self.is_pinned(object) {
                continue;
            }
            let class = self.page_class[page] as usize;
            let start = saved.len();
            saved.extend_from_slice(std::slice::from_raw_parts(
                object.as_ptr().cast::<u8>(),
                SLOT_SIZES[class],
            ));
            moving.push(Moved {
                from: object,
                class,
                start,
                tenured: self.tenured[page][word] & bit != 0,
                interned: self.interned[page][word] & bit != 0,
                finalizable: self.finalizable[page][word] & bit != 0,
            });
            for bitmap in [
                &mut self.marks,
                &mut self.tenured,
                &mut self.allocated,
                &mut self.interned,
                &mut self.finalizable,
            ] {
                bitmap[page][word] &= !bit;
            }
            self.live_slots[page] -= 1;
        }
        // The pages that keep an object keep their class; every other page is free for any.
        let mut holes: [Vec<u32>; CLASS_COUNT] = Default::default();
        let mut free_pages: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        for page in 0..self.page_count {
            if self.live_slots[page] == 0 {
                self.page_class[page] = EMPTY_PAGE;
                free_pages.push_back(page as u32);
            } else {
                holes[self.page_class[page] as usize].push(page as u32);
            }
        }
        for pages in &mut holes {
            pages.reverse();
        }
        let mut current: [Option<(u32, usize)>; CLASS_COUNT] = [None; CLASS_COUNT];
        let mut moves = Vec::with_capacity(moving.len());
        for moved in &moving {
            let class = moved.class;
            let slots = PAGE_SIZE / SLOT_SIZES[class];
            let (page, slot) = loop {
                if let Some((page, from)) = current[class] {
                    let bitmap = &self.allocated[page as usize];
                    if let Some(slot) =
                        (from..slots).find(|&slot| bitmap[slot / 64] & (1 << (slot % 64)) == 0)
                    {
                        break (page, slot);
                    }
                }
                let page = match holes[class].pop() {
                    Some(page) => page,
                    None => {
                        let page = match free_pages.pop_front() {
                            Some(page) => page,
                            None => match self.new_page() {
                                Some(page) => page,
                                None => panic!("no page is left to compact the image into"),
                            },
                        };
                        self.page_class[page as usize] = class as u8;
                        page
                    }
                };
                current[class] = Some((page, 0));
            };
            current[class] = Some((page, slot + 1));
            let (index, word, bit) = (page as usize, slot / 64, 1u64 << (slot % 64));
            let to = self
                .base
                .as_ptr()
                .add(index * PAGE_SIZE + slot * SLOT_SIZES[class]);
            std::ptr::copy_nonoverlapping(saved.as_ptr().add(moved.start), to, SLOT_SIZES[class]);
            self.allocated[index][word] |= bit;
            self.marks[index][word] |= bit;
            if moved.tenured {
                self.tenured[index][word] |= bit;
            }
            if moved.interned {
                self.interned[index][word] |= bit;
            }
            if moved.finalizable {
                self.finalizable[index][word] |= bit;
            }
            self.live_slots[index] += 1;
            moves.push((
                moved.from,
                Sexp::from_non_null(NonNull::new_unchecked(to).cast()),
            ));
        }
        // Trailing pages left empty are dropped from the image, down to the pages a loading
        // process already holds.
        let mut page_count = self.page_count;
        while page_count > self.pinned.len() && self.page_class[page_count - 1] == EMPTY_PAGE {
            page_count -= 1;
        }
        self.page_count = page_count;
        self.page_class.truncate(page_count);
        self.live_slots.truncate(page_count);
        for bitmaps in [
            &mut self.marks,
            &mut self.tenured,
            &mut self.allocated,
            &mut self.finalizable,
            &mut self.interned,
        ] {
            bitmaps.truncate(page_count);
        }
        self.total_live_slots = self.live_slots.iter().map(|&live| live as usize).sum();
        self.empty_pages.clear();
        for available in &mut self.available {
            available.clear();
        }
        for page in 0..page_count {
            if self.page_class[page] == EMPTY_PAGE {
                self.empty_pages.push(page as u32);
            } else {
                self.file_page(page as u32);
            }
        }
        moves
    }

    /// Takes over the pages and metadata [`Space::write_image`] wrote, in place of everything the
    /// space held. `fill` receives the memory of the image's pages, at the space's base, and
    /// fills it with the pages' bytes. The caller has checked that the space allocated exactly
    /// what the writer's had when its image's objects began, so the objects allocated since live
    /// where the image's copies of them go.
    ///
    /// `held` are objects the process holds outside the heap's tables, which must be among the
    /// image's objects; when one is not, the space is left unchanged and the error is
    /// [`ImageError::Mismatch`].
    pub unsafe fn load_image(
        &mut self,
        input: &mut Reader<'_>,
        held: &[Sexp],
        fill: &mut dyn FnMut(&mut [u8]) -> Result<(), ImageError>,
    ) -> Result<(), ImageError> {
        let page_count = input.u32()? as usize;
        let invalid = |message: &str| ImageError::Invalid(message.to_owned());
        if page_count < self.page_count
            || page_count * PAGE_SIZE > self.reserved_bytes - HUGE_PAGE_SIZE
        {
            return Err(invalid("its pages do not fit the heap"));
        }
        let page_class = input.take(page_count)?.to_vec();
        if page_class
            .iter()
            .any(|&class| class != EMPTY_PAGE && class as usize >= CLASS_COUNT)
        {
            return Err(invalid("a page has an unknown size class"));
        }
        let mut live_slots = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            live_slots.push(input.u16()?);
        }
        let mut bitmaps: [Vec<Bitmap>; 5] = Default::default();
        for bitmap in &mut bitmaps {
            let bytes = input.take(page_count * size_of::<Bitmap>())?;
            bitmap.reserve_exact(page_count);
            for page in bytes.as_chunks::<{ size_of::<Bitmap>() }>().0 {
                let mut words = [0u64; BITMAP_WORDS];
                for (word, bytes) in words.iter_mut().zip(page.as_chunks::<8>().0) {
                    *word = u64::from_le_bytes(*bytes);
                }
                bitmap.push(words);
            }
        }
        for &object in held {
            let offset = (object.as_ptr() as usize).wrapping_sub(self.base.as_ptr() as usize);
            let page = offset / PAGE_SIZE;
            let allocated = page < page_count
                && SLOT_SIZES
                    .get(page_class[page] as usize)
                    .is_some_and(|&slot_size| {
                        let slot = offset % PAGE_SIZE / slot_size;
                        bitmaps[2][page][slot / 64] & (1 << (slot % 64)) != 0
                    });
            if !allocated {
                return Err(ImageError::Mismatch(
                    "the process holds an object the image does not".to_owned(),
                ));
            }
        }
        let needed = page_count.next_multiple_of(COMMIT_PAGES);
        if needed > self.committed_pages {
            // SAFETY: the range lies inside the reservation, past every committed page.
            let committed = unsafe {
                virtual_memory::commit(
                    self.base.as_ptr().add(self.committed_pages * PAGE_SIZE),
                    (needed - self.committed_pages) * PAGE_SIZE,
                )
            };
            if !committed {
                return Err(ImageError::Heap(crate::HeapError::CannotAllocate {
                    bytes: needed * PAGE_SIZE,
                }));
            }
            self.committed_pages = needed;
        }
        // The image's pages replace every frame these pages hold.
        self.drop_frames();
        // SAFETY: the first `page_count` pages are committed, and the objects in them are
        // replaced by the image's copies as a whole.
        fill(unsafe {
            std::slice::from_raw_parts_mut(self.base.as_ptr(), page_count * PAGE_SIZE)
        })?;
        let [marks, tenured, allocated, finalizable, interned] = bitmaps;
        self.page_count = page_count;
        self.page_class = page_class;
        self.total_live_slots = live_slots.iter().map(|&live| live as usize).sum();
        self.live_slots = live_slots;
        self.marks = marks;
        self.tenured = tenured;
        self.allocated = allocated;
        self.finalizable = finalizable;
        self.interned = interned;
        self.cursors = [IDLE_CURSOR; CLASS_COUNT];
        if self.quarantine.is_some() {
            self.set_quarantine(false);
            self.set_quarantine(true);
        }
        self.touched.clear();
        self.empty_pages.clear();
        for available in &mut self.available {
            available.clear();
        }
        for page in 0..page_count {
            if self.page_class[page] == EMPTY_PAGE {
                self.empty_pages.push(page as u32);
            } else {
                self.file_page(page as u32);
            }
        }
        self.marked_vector_cells = 0;
        self.tenured_vector_cells = 0;
        self.promote_bits = gc_bits::OLD;
        Ok(())
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// A dense number for every slot of the space as it is laid out now and for each of
    /// `large`, which are the heap's large objects.
    pub fn slot_index(&self, large: impl Iterator<Item = Sexp>) -> SlotIndex {
        let mut first_slot = Vec::with_capacity(self.page_count);
        let mut slots = 0;
        for &class in &self.page_class[..self.page_count] {
            first_slot.push(slots as u32);
            if let Some(size) = SLOT_SIZES.get(class as usize) {
                slots += PAGE_SIZE / size;
            }
        }
        let mut large: Vec<usize> = large.map(|object| object.as_ptr() as usize).collect();
        large.sort_unstable();
        SlotIndex {
            base: self.base.as_ptr() as usize,
            page_class: self.page_class[..self.page_count].to_vec(),
            first_slot,
            slots,
            large,
        }
    }
}

/// Numbers every slot of the space and every large object densely, as they lay when the index
/// was taken, so that a number keeps naming the object that was there after objects move. The
/// image writer keys its sets and its forwarding table on these numbers instead of hashing
/// addresses: a slot's number is its page's first number plus its index in the page, and a
/// large object's comes after every slot's, in address order.
pub(crate) struct SlotIndex {
    base: usize,
    page_class: Vec<u8>,
    first_slot: Vec<u32>,
    slots: usize,
    large: Vec<usize>,
}

impl SlotIndex {
    pub fn len(&self) -> usize {
        self.slots + self.large.len()
    }

    /// The number of the slot or large object at `object`'s address, or `None` when neither
    /// was there.
    #[inline]
    pub fn of(&self, object: Sexp) -> Option<usize> {
        let address = object.as_ptr() as usize;
        let offset = address.wrapping_sub(self.base);
        let page = offset >> PAGE_SHIFT;
        if let Some(&class) = self.page_class.get(page) {
            let multiplier = *SLOT_INDEX_MULTIPLIERS.get(class as usize)?;
            let slot = ((offset & (PAGE_SIZE - 1)) as u64 * multiplier) >> 32;
            return Some(self.first_slot[page] as usize + slot as usize);
        }
        let position = self.large.binary_search(&address).ok()?;
        Some(self.slots + position)
    }

    /// An empty set of the objects this index numbers.
    pub fn set(&self) -> SlotSet<'_> {
        SlotSet {
            index: self,
            words: vec![0; self.len().div_ceil(64)],
        }
    }
}

/// A set of the objects a [`SlotIndex`] numbers, one bit each.
pub(crate) struct SlotSet<'a> {
    index: &'a SlotIndex,
    words: Vec<u64>,
}

impl SlotSet<'_> {
    /// Adds `object` and says whether it was absent. An object the index does not number is
    /// never added.
    pub fn insert(&mut self, object: Sexp) -> bool {
        let Some(number) = self.index.of(object) else {
            return false;
        };
        let (word, bit) = (number / 64, 1u64 << (number % 64));
        let absent = self.words[word] & bit == 0;
        self.words[word] |= bit;
        absent
    }

    pub fn contains(&self, object: Sexp) -> bool {
        self.index
            .of(object)
            .is_some_and(|number| self.words[number / 64] & (1 << (number % 64)) != 0)
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        self.drop_frames();
        // SAFETY: the reservation is unmapped exactly once, after its last object is dropped.
        let released = unsafe { virtual_memory::release(self.reservation, self.reserved_bytes) };
        if !released {
            panic!(
                "cannot release the R heap's address space: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

impl Space {
    /// Drops the frames of every live environment, which own memory outside the space.
    fn drop_frames(&mut self) {
        for page in 0..self.page_count {
            if self.page_class[page] == EMPTY_PAGE {
                continue;
            }
            let slot_size = SLOT_SIZES[self.page_class[page] as usize];
            for word in 0..BITMAP_WORDS {
                // A finalizable slot that was swept had its bit cleared, so every set bit is a
                // live environment.
                for bit in set_bits(self.finalizable[page][word]) {
                    // SAFETY: the slot holds an environment whose frame is still owned.
                    unsafe {
                        let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                        drop_frame(Sexp::from_non_null(self.base.add(offset).cast()));
                    }
                }
            }
            self.finalizable[page] = [0; BITMAP_WORDS];
        }
    }
}

unsafe fn drop_frame(object: Sexp) {
    unsafe {
        if object.kind() == SexpType::Environment && object.flags() & flag_bits::ALTREP == 0 {
            std::ptr::drop_in_place::<Frame>(object.frame_ptr());
        }
    }
}

/// Overwrites a freed object of `bytes` bytes with [`POISON`] under a [`POISONED_KIND`] kind
/// byte. Its GC bits stay clear, so the
/// collector never takes a stale reference to it for a marked object.
pub(crate) unsafe fn poison(object: *mut u8, bytes: usize) {
    unsafe {
        std::ptr::write_bytes(object, POISON, bytes);
        *object = POISONED_KIND;
        *object.add(1) = 0;
    }
}

/// Finds the first run of free slots at or after `from` among a page's first `slots`, marks it
/// allocated, and returns its slot range. A slot in `held` is not free. A run stops at the first
/// slot that is not free, so on a page with no objects it is the whole page.
#[inline(always)]
fn claim_run(
    allocated: &mut Bitmap,
    held: Option<&Bitmap>,
    slots: usize,
    from: usize,
) -> Option<(usize, usize)> {
    let words = slots.div_ceil(64);
    let mut word = from / 64;
    let mut start = None;
    let mut free = 0;
    while word < words {
        free = !(allocated[word] | held.map_or(0, |held| held[word])) & valid_slots(slots, word);
        if word == from / 64 {
            free &= u64::MAX << (from % 64);
        }
        if free != 0 {
            start = Some(word * 64 + free.trailing_zeros() as usize);
            break;
        }
        word += 1;
    }
    let start = start?;
    let mut end = start;
    loop {
        let bit = end % 64;
        let run = (free >> bit).trailing_ones() as usize;
        allocated[end / 64] |= run_mask(bit, run);
        end += run;
        if bit + run < 64 || end / 64 >= words {
            break;
        }
        free = !(allocated[end / 64] | held.map_or(0, |held| held[end / 64]))
            & valid_slots(slots, end / 64);
        if free & 1 == 0 {
            break;
        }
    }
    Some((start, end))
}

/// Gives back the slots each of `cursors` claimed but never handed out.
fn release_runs(cursors: &mut [Cursor; CLASS_COUNT], allocated: &mut [Bitmap], base: NonNull<u8>) {
    for (class, cursor) in cursors.iter_mut().enumerate() {
        if cursor.page != NO_PAGE && cursor.next != cursor.end {
            let page = cursor.page as usize;
            let page_base = base.as_ptr() as usize + page * PAGE_SIZE;
            let slot_size = SLOT_SIZES[class];
            let from = (cursor.next as usize - page_base) / slot_size;
            let to = (cursor.end as usize - page_base) / slot_size;
            if let Some(allocated) = allocated.get_mut(page) {
                release_run(allocated, from, to);
            }
            cursor.end = cursor.next;
        }
    }
}

/// Clears the allocated bits of slots `from..to`.
fn release_run(allocated: &mut Bitmap, from: usize, to: usize) {
    let mut slot = from;
    while slot < to {
        let bit = slot % 64;
        let run = (64 - bit).min(to - slot);
        if let Some(word) = allocated.get_mut(slot / 64) {
            *word &= !run_mask(bit, run);
        }
        slot += run;
    }
}

/// `run` set bits starting at `bit`, with `bit + run <= 64`.
fn run_mask(bit: usize, run: usize) -> u64 {
    if run == 64 {
        u64::MAX
    } else {
        ((1u64 << run) - 1) << bit
    }
}

/// The 8-byte cells `object`'s vector data takes, as GNU R's `Vcells` count it, or zero for an
/// object that is not a vector. An ALTREP vector's data lives in the objects it points at.
pub(crate) unsafe fn vector_cells(object: Sexp) -> u64 {
    let kind = unsafe { object.kind() };
    if kind.shape() != Shape::Vector || unsafe { object.is_altrep() } {
        return 0;
    }
    let Some(element) = kind.element_size() else {
        return 0;
    };
    let elements = unsafe { object.length() } as usize + usize::from(kind == SexpType::Char);
    (elements * element).div_ceil(8) as u64
}

/// The slots of word `word` that exist in a page of `slots` slots.
fn valid_slots(slots: usize, word: usize) -> u64 {
    match slots.saturating_sub(word * 64) {
        0 => 0,
        remaining if remaining >= 64 => u64::MAX,
        remaining => (1u64 << remaining) - 1,
    }
}

/// The indices of the set bits of `bits`, lowest first.
fn set_bits(mut bits: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        if bits == 0 {
            return None;
        }
        let bit = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        Some(bit)
    })
}

/// Address-space reservation, the one place the heap talks to the operating system. Memory is
/// reserved inaccessible up front and committed a run of pages at a time, so the heap's pages
/// stay contiguous without claiming physical memory before they are used.
#[cfg(unix)]
pub(crate) mod virtual_memory {
    /// Reserves `bytes` of inaccessible, page-aligned address space.
    pub fn reserve(bytes: usize) -> Option<*mut u8> {
        // SAFETY: an anonymous private mapping with no access rights only reserves addresses.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the range is the mapping just made. The advice is a hint, so a kernel without
        // transparent huge pages refusing it changes nothing.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::madvise(mapping, bytes, libc::MADV_HUGEPAGE);
        }
        Some(mapping.cast())
    }

    /// Reserves `bytes` of inaccessible address space at exactly `start`, or `None` when
    /// something is mapped there or the system places it elsewhere.
    pub fn reserve_at(start: *mut u8, bytes: usize) -> Option<*mut u8> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = libc::MAP_PRIVATE
            | libc::MAP_ANONYMOUS
            | libc::MAP_NORESERVE
            | libc::MAP_FIXED_NOREPLACE;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        // SAFETY: as in `reserve`; without `MAP_FIXED` the address is a hint, and a mapping
        // placed elsewhere is returned to the system below.
        let mapping = unsafe { libc::mmap(start.cast(), bytes, libc::PROT_NONE, flags, -1, 0) };
        if mapping == libc::MAP_FAILED {
            return None;
        }
        if mapping.cast() != start {
            // SAFETY: the mapping was just made and nothing uses it.
            unsafe { libc::munmap(mapping, bytes) };
            return None;
        }
        // SAFETY: as in `reserve`.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::madvise(mapping, bytes, libc::MADV_HUGEPAGE);
        }
        Some(mapping.cast())
    }

    /// Makes `bytes` at `start`, inside a reservation, readable and writable.
    pub unsafe fn commit(start: *mut u8, bytes: usize) -> bool {
        unsafe { libc::mprotect(start.cast(), bytes, libc::PROT_READ | libc::PROT_WRITE) == 0 }
    }

    /// Returns a whole reservation to the system.
    pub unsafe fn release(start: *mut u8, bytes: usize) -> bool {
        unsafe { libc::munmap(start.cast(), bytes) == 0 }
    }
}

#[cfg(windows)]
pub(crate) mod virtual_memory {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc,
        VirtualFree,
    };

    /// Reserves `bytes` of inaccessible address space, aligned to the 64 KiB allocation
    /// granularity.
    pub fn reserve(bytes: usize) -> Option<*mut u8> {
        // SAFETY: reserving without committing maps nothing.
        let start = unsafe { VirtualAlloc(std::ptr::null(), bytes, MEM_RESERVE, PAGE_NOACCESS) };
        (!start.is_null()).then_some(start.cast())
    }

    /// Reserves `bytes` of inaccessible address space at exactly `start`, or `None` when the
    /// range is taken.
    pub fn reserve_at(start: *mut u8, bytes: usize) -> Option<*mut u8> {
        // SAFETY: reserving without committing maps nothing.
        let mapping = unsafe { VirtualAlloc(start.cast(), bytes, MEM_RESERVE, PAGE_NOACCESS) };
        (mapping.cast() == start).then_some(start)
    }

    /// Commits `bytes` at `start`, inside a reservation, as readable and writable memory.
    pub unsafe fn commit(start: *mut u8, bytes: usize) -> bool {
        unsafe { !VirtualAlloc(start.cast(), bytes, MEM_COMMIT, PAGE_READWRITE).is_null() }
    }

    /// Returns a whole reservation to the system; `MEM_RELEASE` takes the size 0.
    pub unsafe fn release(start: *mut u8, _bytes: usize) -> bool {
        unsafe { VirtualFree(start.cast(), 0, MEM_RELEASE) != 0 }
    }
}
