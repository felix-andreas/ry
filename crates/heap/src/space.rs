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
        image::{ImageError, Pages, Reader, Writer},
        object::{Sexp, SexpType, Shape, flag_bits, gc_bits},
    },
    std::ptr::NonNull,
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
pub(crate) const BITMAP_WORDS: usize = PAGE_SIZE / SLOT_SIZES[0] / 64;
pub(crate) type Bitmap = [u64; BITMAP_WORDS];
/// Address space only: pages are committed as the heap grows, and reserving costs nothing until
/// then. It bounds the small objects one session can hold at 64 GiB. A process with a capped
/// address space (`ulimit -v`, valgrind) gets the largest power of two down to the floor.
const RESERVED_BYTES: usize = 64 << 30;
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
const POISON: u8 = 0xDB;

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
    /// Pages of each class with free slots, which no cursor is on.
    available: [Vec<u32>; CLASS_COUNT],
    empty_pages: Vec<u32>,
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

/// What a sweep found.
#[derive(Default)]
pub(crate) struct Swept {
    pub freed: u64,
}

impl Space {
    /// A space whose pages start at `base` when that address range is free, which is what lets
    /// a start-up image map its pages in place (see `crate::image`), and anywhere otherwise.
    /// `base` must be a multiple of 2 MiB.
    pub fn new(base: Option<usize>) -> Space {
        let placed = base.and_then(|base| {
            virtual_memory::reserve_at(base, RESERVED_BYTES).map(|mapping| (mapping, RESERVED_BYTES))
        });
        let (base_mapping, reserved_bytes) = match placed {
            Some(placed) => placed,
            None => {
                let mut reserved_bytes = RESERVED_BYTES;
                loop {
                    if let Some(mapping) = virtual_memory::reserve(reserved_bytes) {
                        break (mapping, reserved_bytes);
                    }
                    if reserved_bytes == MINIMUM_RESERVED_BYTES {
                        panic!(
                            "cannot reserve 1 GiB of address space for the R heap: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                    reserved_bytes /= 2;
                }
            }
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
            available: Default::default(),
            empty_pages: Vec::new(),
            touched: Vec::new(),
            marked_vector_cells: 0,
            tenured_vector_cells: 0,
            promote_bits: gc_bits::OLD,
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

    /// Claims the cursor's next run of free slots, moving to another page when its page has
    /// none left, and hands out the run's first slot.
    #[cold]
    #[inline(never)]
    fn refill(&mut self, class: usize) -> Option<NonNull<u8>> {
        let slot_size = SLOT_SIZES[class];
        let slots = PAGE_SIZE / slot_size;
        loop {
            let cursor = self.cursors[class];
            if cursor.page != NO_PAGE {
                let page = cursor.page as usize;
                if let Some((start, end)) =
                    claim_run(&mut self.allocated[page], slots, cursor.next_slot as usize)
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
            self.touched.push(page);
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
        Some(page as u32)
    }

    /// The page, word and bit of `object`'s slot, or `None` for a large object.
    #[inline]
    pub(crate) fn locate(&self, object: Sexp) -> Option<(usize, usize, u64)> {
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
    #[inline]
    pub unsafe fn mark(&mut self, object: Sexp) -> bool {
        let location = self.locate(object);
        let unmarked = match location {
            Some((page, word, bit)) => {
                let marks = &mut self.marks[page][word];
                let unmarked = *marks & bit == 0;
                *marks |= bit;
                unmarked
            }
            None => unsafe {
                let unmarked = object.gc_bits() & gc_bits::MARK == 0;
                object.set_gc_bit(gc_bits::MARK, true);
                unmarked
            },
        };
        if unmarked {
            let bits = unsafe { object.gc_bits() };
            let promote = if bits & gc_bits::OLD == 0 {
                gc_bits::OLD
            } else {
                self.promote_bits
            };
            // A major collection marks every live object again, so the test keeps it from
            // storing into the header of each one that is already promoted.
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
        unmarked
    }

    /// Whether `object`, which this collection has marked, is tenured.
    #[inline]
    pub unsafe fn is_tenured(&self, object: Sexp) -> bool {
        match self.locate(object) {
            Some((page, word, bit)) => self.tenured[page][word] & bit != 0,
            None => unsafe { object.gc_bits() & gc_bits::TENURED != 0 },
        }
    }

    /// Records that `object`, an environment in this space, owns a frame to drop when it dies.
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
        let pages: Vec<u32> = (0..self.page_count as u32).collect();
        unsafe { self.tenure_pages(&pages) }
    }

    /// Moves the small objects allocated since the last collection into the oldest generation,
    /// as [`Space::tenure_all`] does for all of them.
    pub unsafe fn tenure_touched(&mut self) -> u64 {
        let touched = std::mem::take(&mut self.touched);
        let cells = unsafe { self.tenure_pages(&touched) };
        self.touched = touched;
        cells
    }

    unsafe fn tenure_pages(&mut self, pages: &[u32]) -> u64 {
        // The unused tail of a cursor's run counts as allocated until it is released.
        self.release_cursors();
        let mut cells = 0;
        for &page in pages {
            let page = page as usize;
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
                // Poisoning makes a use after free fail loudly, but costs a store per byte of
                // every dead slot, so only debug builds pay for it. The kind byte stays
                // meaningful: a freed slot reads as `FREESXP`.
                if cfg!(debug_assertions) {
                    for bit in set_bits(dead) {
                        let slot = unsafe { page_base.add((word * 64 + bit) * slot_size) };
                        unsafe {
                            std::ptr::write_bytes(slot, POISON, slot_size);
                            *slot = SexpType::Free as u8;
                            *slot.add(1) = 0;
                        }
                    }
                }
                swept.freed += u64::from(dead.count_ones());
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
        if live == 0 {
            self.page_class[index] = EMPTY_PAGE;
            self.empty_pages.push(page);
        } else if live < PAGE_SIZE / SLOT_SIZES[class] {
            self.available[class].push(page);
        }
    }

    /// Gives back the slots each cursor claimed but never handed out, so the sweep neither
    /// counts them as freed objects nor reads them.
    fn release_cursors(&mut self) {
        for (class, cursor) in self.cursors.iter_mut().enumerate() {
            if cursor.page != NO_PAGE && cursor.next != cursor.end {
                let page = cursor.page as usize;
                let page_base = self.base.as_ptr() as usize + page * PAGE_SIZE;
                let slot_size = SLOT_SIZES[class];
                let from = (cursor.next as usize - page_base) / slot_size;
                let to = (cursor.end as usize - page_base) / slot_size;
                if let Some(allocated) = self.allocated.get_mut(page) {
                    release_run(allocated, from, to);
                }
                cursor.end = cursor.next;
            }
        }
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

    pub fn live_slots(&self) -> usize {
        self.total_live_slots
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }
}

/// The page section of a heap image.
impl Space {
    /// The address of the first page.
    pub fn base_address(&self) -> usize {
        self.base.as_ptr() as usize
    }

    /// The memory of every page ever given to a class, which an image stores as it is.
    pub fn page_bytes(&self) -> &[u8] {
        // SAFETY: the first `page_count` pages are committed and readable.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr(), self.page_count * PAGE_SIZE) }
    }

    /// A bitmap per page with no bit set, for the writer's reachability pass.
    pub fn empty_bitmaps(&self) -> Vec<Bitmap> {
        vec![[0; BITMAP_WORDS]; self.page_count]
    }

    /// The objects whose bits are set in `bitmaps`, which has one bitmap per page.
    pub fn objects_in<'a>(&'a self, bitmaps: &'a [Bitmap]) -> impl Iterator<Item = Sexp> + 'a {
        bitmaps.iter().enumerate().flat_map(move |(page, bitmap)| {
            let slot_size = SLOT_SIZES
                .get(self.page_class[page] as usize)
                .copied()
                .unwrap_or(0);
            bitmap.iter().enumerate().flat_map(move |(word, &bits)| {
                set_bits(bits).map(move |bit| {
                    let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                    // SAFETY: a set bit names a slot of a committed page.
                    unsafe { Sexp::from_non_null(self.base.add(offset).cast()) }
                })
            })
        })
    }

    /// Feeds the layout of the space into `mix`: which pages exist, their classes, which slots
    /// are allocated and where each cursor stands. Two spaces built by the same steps agree.
    pub fn mix_layout(&self, mix: &mut dyn FnMut(u64)) {
        mix(self.base_address() as u64);
        mix(self.page_count as u64);
        for page in 0..self.page_count {
            mix(u64::from(self.page_class[page]));
            for &word in &self.allocated[page] {
                mix(word);
            }
        }
        for cursor in &self.cursors {
            mix(cursor.next as u64);
            mix(cursor.end as u64);
            mix(u64::from(cursor.page));
            mix(u64::from(cursor.next_slot));
        }
    }

    /// Writes the metadata of the pages, keeping only the slots set in `reachable`, and
    /// returns the bytes those objects take. Every object kept must be tenured.
    pub fn write_image(&self, reachable: &[Bitmap], out: &mut Writer) -> usize {
        out.u32(self.page_count as u32);
        let mut live_slots = Vec::with_capacity(self.page_count);
        for page in 0..self.page_count {
            let live: u32 = reachable[page].iter().map(|word| word.count_ones()).sum();
            live_slots.push(live as u16);
            out.u8(if live == 0 {
                EMPTY_PAGE
            } else {
                self.page_class[page]
            });
        }
        let mut bytes = 0;
        for (page, &live) in live_slots.iter().enumerate() {
            out.u16(live);
            if let Some(&slot_size) = SLOT_SIZES.get(self.page_class[page] as usize) {
                bytes += live as usize * slot_size;
            }
        }
        for page in 0..self.page_count {
            for &word in &reachable[page] {
                out.u64(word);
            }
        }
        for bits in [&self.finalizable, &self.interned] {
            for page in 0..self.page_count {
                for word in 0..BITMAP_WORDS {
                    out.u64(bits[page][word] & reachable[page][word]);
                }
            }
        }
        bytes
    }

    /// Replaces every page with the pages of an image, whose metadata [`Space::write_image`]
    /// wrote, and returns the bytes their objects take. Every object of the image is old and
    /// tenured. The frames the space's own environments owned are dropped first, since the
    /// image's pages hold other frames at their addresses.
    pub unsafe fn adopt_image(
        &mut self,
        input: &mut Reader<'_>,
        pages: Pages<'_>,
    ) -> Result<usize, ImageError> {
        let page_count = input.u32()? as usize;
        let invalid = |message: &str| ImageError::Invalid(message.to_owned());
        let limit_pages = (self.reserved_bytes - HUGE_PAGE_SIZE) / PAGE_SIZE;
        if page_count > limit_pages {
            return Err(invalid("it has more pages than the heap can hold"));
        }
        let classes = input.take(page_count)?;
        if classes
            .iter()
            .any(|&class| class != EMPTY_PAGE && class as usize >= CLASS_COUNT)
        {
            return Err(invalid("a page has an unknown size class"));
        }
        let live_bytes = input.take(page_count * 2)?;
        let bitmap_bytes = page_count * BITMAP_WORDS * 8;
        let allocated = read_bitmaps(input.take(bitmap_bytes)?);
        let finalizable = read_bitmaps(input.take(bitmap_bytes)?);
        let interned = read_bitmaps(input.take(bitmap_bytes)?);

        for page in 0..self.page_count {
            let Some(&slot_size) = SLOT_SIZES.get(self.page_class[page] as usize) else {
                continue;
            };
            for word in 0..BITMAP_WORDS {
                for bit in set_bits(self.finalizable[page][word]) {
                    // SAFETY: the slot holds a live environment whose frame is still owned.
                    unsafe {
                        let offset = page * PAGE_SIZE + (word * 64 + bit) * slot_size;
                        drop_frame(Sexp::from_non_null(self.base.add(offset).cast()));
                    }
                }
            }
        }

        let length = page_count * PAGE_SIZE;
        let committed = page_count.next_multiple_of(COMMIT_PAGES).min(limit_pages);
        match pages {
            #[cfg(unix)]
            Pages::File { descriptor, offset } => {
                // SAFETY: the range lies inside the reservation, and nothing in it is referenced
                // any more: the image replaces every object the pages held.
                if length > 0
                    && unsafe {
                        !virtual_memory::map_file(self.base.as_ptr(), length, descriptor, offset)
                    }
                {
                    return Err(ImageError::Invalid(format!(
                        "its pages cannot be mapped: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                let from = self.committed_pages.max(page_count);
                if from < committed {
                    self.commit_range(from, committed)?;
                }
            }
            Pages::Bytes(bytes) => {
                if bytes.len() != length {
                    return Err(invalid("its pages have the wrong size"));
                }
                if self.committed_pages < committed {
                    self.commit_range(self.committed_pages, committed)?;
                }
                // SAFETY: the first `committed` pages are committed, and the image's pages fit.
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.as_ptr(), length);
                }
            }
        }
        self.committed_pages = self.committed_pages.max(committed);

        self.page_count = page_count;
        self.page_class = classes.to_vec();
        self.live_slots = live_bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        self.total_live_slots = self.live_slots.iter().map(|&live| live as usize).sum();
        self.marks = allocated.clone();
        self.tenured = allocated.clone();
        self.allocated = allocated;
        self.finalizable = finalizable;
        self.interned = interned;
        self.cursors = [IDLE_CURSOR; CLASS_COUNT];
        for available in &mut self.available {
            available.clear();
        }
        self.empty_pages.clear();
        self.touched.clear();
        let mut bytes = 0;
        // Pages are taken from the ends of these lists, so the lowest pages fill first.
        for page in (0..page_count).rev() {
            let class = self.page_class[page];
            let live = self.live_slots[page] as usize;
            if class == EMPTY_PAGE || live == 0 {
                self.page_class[page] = EMPTY_PAGE;
                self.empty_pages.push(page as u32);
                continue;
            }
            let slot_size = SLOT_SIZES[class as usize];
            bytes += live * slot_size;
            if live < PAGE_SIZE / slot_size {
                self.available[class as usize].push(page as u32);
            }
        }
        Ok(bytes)
    }

    fn commit_range(&mut self, from: usize, to: usize) -> Result<(), ImageError> {
        // SAFETY: the range lies inside the reservation, past every page in use.
        let committed = unsafe {
            virtual_memory::commit(
                self.base.as_ptr().add(from * PAGE_SIZE),
                (to - from) * PAGE_SIZE,
            )
        };
        if committed {
            Ok(())
        } else {
            Err(ImageError::Invalid(format!(
                "the heap cannot grow to hold it: {}",
                std::io::Error::last_os_error()
            )))
        }
    }
}

fn read_bitmaps(bytes: &[u8]) -> Vec<Bitmap> {
    bytes
        .chunks_exact(BITMAP_WORDS * 8)
        .map(|page| {
            let mut bitmap = [0u64; BITMAP_WORDS];
            for (word, bytes) in bitmap.iter_mut().zip(page.chunks_exact(8)) {
                let mut array = [0u8; 8];
                array.copy_from_slice(bytes);
                *word = u64::from_le_bytes(array);
            }
            bitmap
        })
        .collect()
}

impl Drop for Space {
    fn drop(&mut self) {
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
        }
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

pub(crate) unsafe fn drop_frame(object: Sexp) {
    unsafe {
        if object.kind() == SexpType::Environment && object.flags() & flag_bits::ALTREP == 0 {
            std::ptr::drop_in_place::<Frame>(object.frame_ptr());
        }
    }
}

/// Finds the first run of free slots at or after `from` among a page's first `slots`, marks it
/// allocated, and returns its slot range. A run stops at the first allocated slot, so on a page
/// with no objects it is the whole page.
fn claim_run(allocated: &mut Bitmap, slots: usize, from: usize) -> Option<(usize, usize)> {
    let words = slots.div_ceil(64);
    let mut word = from / 64;
    let mut start = None;
    let mut free = 0;
    while word < words {
        free = !allocated[word] & valid_slots(slots, word);
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
        free = !allocated[end / 64] & valid_slots(slots, end / 64);
        if free & 1 == 0 {
            break;
        }
    }
    Some((start, end))
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
mod virtual_memory {
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

    /// Reserves `bytes` as [`reserve`] does, but only at `address`.
    pub fn reserve_at(address: usize, bytes: usize) -> Option<*mut u8> {
        // Without `MAP_FIXED_NOREPLACE` the address is a hint, which a kernel may not take.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = libc::MAP_PRIVATE
            | libc::MAP_ANONYMOUS
            | libc::MAP_NORESERVE
            | libc::MAP_FIXED_NOREPLACE;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        // SAFETY: as in `reserve`; the flags never replace an existing mapping.
        let mapping = unsafe {
            libc::mmap(
                address as *mut libc::c_void,
                bytes,
                libc::PROT_NONE,
                flags,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return None;
        }
        if mapping as usize != address {
            // SAFETY: the mapping was just made and nothing uses it.
            if unsafe { libc::munmap(mapping, bytes) } != 0 {
                panic!(
                    "cannot release a misplaced heap reservation: {}",
                    std::io::Error::last_os_error()
                );
            }
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

    /// Maps `bytes` of the file `descriptor` from `offset` at `start`, inside a reservation, as
    /// private memory: the file's pages are shared until a write copies one.
    pub unsafe fn map_file(
        start: *mut u8,
        bytes: usize,
        descriptor: std::os::fd::RawFd,
        offset: u64,
    ) -> bool {
        let Ok(offset) = libc::off_t::try_from(offset) else {
            return false;
        };
        let mapping = unsafe {
            libc::mmap(
                start.cast(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                descriptor,
                offset,
            )
        };
        mapping == start.cast()
    }

    /// Returns a whole reservation to the system.
    pub unsafe fn release(start: *mut u8, bytes: usize) -> bool {
        unsafe { libc::munmap(start.cast(), bytes) == 0 }
    }
}

#[cfg(windows)]
mod virtual_memory {
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

    /// Reserves `bytes` as [`reserve`] does, but only at `address`.
    pub fn reserve_at(address: usize, bytes: usize) -> Option<*mut u8> {
        // SAFETY: reserving without committing maps nothing, and fails where anything is mapped.
        let start =
            unsafe { VirtualAlloc(address as *const _, bytes, MEM_RESERVE, PAGE_NOACCESS) };
        (start as usize == address).then_some(start.cast())
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
