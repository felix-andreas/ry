//! The small-object space. Every object of at most 2 KiB lives in one reserved range of address
//! space, cut into 64 KiB pages, and each page holds the slots of one size class. A page's
//! metadata lives outside the page, in dense per-page bitmaps with one bit per slot: whether the
//! slot is marked, whether it is allocated, and whether its object owns memory that must be
//! released when it dies. The collector tests and sets marks, and sweeps a page, with word
//! operations on those bitmaps, so neither marking an already-marked object nor freeing a dead
//! one touches the object's own memory.
//!
//! Allocation takes a whole bitmap word of free slots at a time: the cursor of a size class
//! claims every free slot of the word in the allocated bitmap, then hands them out with one
//! trailing-zero count each. A sweep first gives back the slots a cursor claimed but never handed
//! out, so they count as free rather than as freed objects.
//!
//! Objects never move, which the C API requires, and a mark is sticky between collections, so a
//! marked object is an old one.

use {
    crate::{
        env::Frame,
        object::{Sexp, SexpType, flag_bits, gc_bits},
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
const BITMAP_WORDS: usize = PAGE_SIZE / SLOT_SIZES[0] / 64;
type Bitmap = [u64; BITMAP_WORDS];
/// Address space only: pages are committed as the heap grows, and reserving costs nothing until
/// then. It bounds the small objects one session can hold at 64 GiB. A process with a capped
/// address space (`ulimit -v`, valgrind) gets the largest power of two down to the floor.
const RESERVED_BYTES: usize = 64 << 30;
const MINIMUM_RESERVED_BYTES: usize = 1 << 30;
/// Pages committed per system call.
const COMMIT_PAGES: usize = 16;
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
    reservation: *mut libc::c_void,
    reserved_bytes: usize,
    base: NonNull<u8>,
    /// Pages backed by memory, a prefix of the reservation.
    committed_pages: usize,
    /// Pages ever given to a class, a prefix of the committed ones.
    page_count: usize,
    page_class: Vec<u8>,
    /// Marked slots per page, as of the last sweep or since.
    live_slots: Vec<u16>,
    marks: Vec<Bitmap>,
    allocated: Vec<Bitmap>,
    /// Slots whose object owns memory outside the heap: environments, whose frames spill.
    finalizable: Vec<Bitmap>,
    cursors: [Cursor; CLASS_COUNT],
    /// Pages of each class with free slots, which no cursor is on.
    available: [Vec<u32>; CLASS_COUNT],
    empty_pages: Vec<u32>,
    /// Pages a cursor entered since the last collection: the only ones that can hold young objects.
    touched: Vec<u32>,
}

#[derive(Clone, Copy)]
struct Cursor {
    /// Claimed slots of the current word not yet handed out.
    free: u64,
    /// The address of the current word's first slot.
    word_base: *mut u8,
    page: u32,
    next_word: u32,
}

const IDLE_CURSOR: Cursor = Cursor {
    free: 0,
    word_base: std::ptr::null_mut(),
    page: NO_PAGE,
    next_word: 0,
};

/// What a sweep found.
#[derive(Default)]
pub(crate) struct Swept {
    pub freed: u64,
}

impl Space {
    pub fn new() -> Space {
        let mut reserved_bytes = RESERVED_BYTES;
        let base_mapping = loop {
            // SAFETY: an anonymous private mapping with no access rights only reserves addresses.
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    reserved_bytes,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            if mapping != libc::MAP_FAILED {
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
        // `mmap` returns page-aligned memory, but pages here are 64 KiB, so the first partial
        // one is skipped.
        let aligned = (base_mapping as usize).next_multiple_of(PAGE_SIZE);
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
            marks: Vec::new(),
            allocated: Vec::new(),
            finalizable: Vec::new(),
            cursors: [IDLE_CURSOR; CLASS_COUNT],
            available: Default::default(),
            empty_pages: Vec::new(),
            touched: Vec::new(),
        }
    }

    /// A slot of `class`, or `None` when the reservation is exhausted. Its memory is whatever
    /// the slot last held.
    #[inline]
    pub fn allocate(&mut self, class: usize) -> Option<NonNull<u8>> {
        let cursor = &mut self.cursors[class];
        if cursor.free != 0 {
            let bit = cursor.free.trailing_zeros() as usize;
            cursor.free &= cursor.free - 1;
            // SAFETY: the claimed bit is a slot of the cursor's page.
            return NonNull::new(unsafe { cursor.word_base.add(bit * SLOT_SIZES[class]) });
        }
        self.refill(class)
    }

    #[cold]
    fn refill(&mut self, class: usize) -> Option<NonNull<u8>> {
        let slots = PAGE_SIZE / SLOT_SIZES[class];
        let words = slots.div_ceil(64);
        loop {
            let cursor = self.cursors[class];
            if cursor.page != NO_PAGE {
                let page = cursor.page as usize;
                let mut word = cursor.next_word as usize;
                while word < words {
                    let free = !self.allocated[page][word] & valid_slots(slots, word);
                    word += 1;
                    if free != 0 {
                        self.allocated[page][word - 1] |= free;
                        let offset = page * PAGE_SIZE + (word - 1) * 64 * SLOT_SIZES[class];
                        self.cursors[class] = Cursor {
                            free,
                            // SAFETY: the offset is inside a committed page.
                            word_base: unsafe { self.base.as_ptr().add(offset) },
                            page: cursor.page,
                            next_word: word as u32,
                        };
                        return self.allocate(class);
                    }
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
            let commit =
                COMMIT_PAGES.min(self.reserved_bytes / PAGE_SIZE - 1 - self.committed_pages);
            if commit == 0 {
                return None;
            }
            // SAFETY: the range lies inside the reservation, past every committed page.
            let failed = unsafe {
                libc::mprotect(
                    self.base
                        .as_ptr()
                        .add(self.committed_pages * PAGE_SIZE)
                        .cast(),
                    commit * PAGE_SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                ) != 0
            };
            if failed {
                return None;
            }
            self.committed_pages += commit;
        }
        let page = self.page_count;
        self.page_count += 1;
        self.page_class.push(EMPTY_PAGE);
        self.live_slots.push(0);
        self.marks.push([0; BITMAP_WORDS]);
        self.allocated.push([0; BITMAP_WORDS]);
        self.finalizable.push([0; BITMAP_WORDS]);
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

    /// Marks `object` and says whether it was unmarked.
    #[inline]
    pub unsafe fn mark(&mut self, object: Sexp) -> bool {
        match self.locate(object) {
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
        }
    }

    /// Records that `object`, an environment in this space, owns a frame to drop when it dies.
    pub fn set_finalizable(&mut self, object: Sexp) {
        if let Some((page, word, bit)) = self.locate(object) {
            self.finalizable[page][word] |= bit;
        }
    }

    /// Clears every mark, as a major collection starts.
    pub fn clear_marks(&mut self) {
        self.marks.fill([0; BITMAP_WORDS]);
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
            live += marks.count_ones();
        }
        let before = self.live_slots[page] as isize;
        self.live_slots[page] = live as u16;
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
        for cursor in &mut self.cursors {
            if cursor.page != NO_PAGE && cursor.free != 0 {
                let word = cursor.next_word as usize - 1;
                if let Some(allocated) = self
                    .allocated
                    .get_mut(cursor.page as usize)
                    .and_then(|bitmap| bitmap.get_mut(word))
                {
                    *allocated &= !cursor.free;
                }
                cursor.free = 0;
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
        self.live_slots.iter().map(|&live| live as usize).sum()
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }
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
        let failed = unsafe { libc::munmap(self.reservation, self.reserved_bytes) != 0 };
        if failed {
            panic!(
                "cannot release the R heap's address space: {}",
                std::io::Error::last_os_error()
            );
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
