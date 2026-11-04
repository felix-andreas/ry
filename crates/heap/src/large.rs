//! Memory for large vectors: each vector of at least [`MAPPED_BYTES`] gets an anonymous mapping
//! of its own, which goes back to the kernel when the vector dies, as GNU R's large vectors do
//! through glibc's `mmap` and `munmap`. A general-purpose allocator keeps freed memory resident
//! for reuse instead: libR's mimalloc returns it only on `gc()`, so in a loop whose temporaries
//! differ in size, the freed blocks it could not reuse stayed resident on top of the live data.
//!
//! Freed mappings, at most [`MappedPool::limit`] bytes, are kept and resized with `mremap`
//! for the next large vector, because a fresh mapping costs a page fault per page touched. A loop
//! that allocates temporaries of the same size then reuses resident pages instead of faulting in
//! new ones.

use std::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};

/// The smallest vector, in bytes with its header, that gets its own mapping. Smaller large
/// vectors come from the global allocator, which reuses their memory within its pages.
pub(crate) const MAPPED_BYTES: usize = 1 << 20;

/// Freed mappings kept for reuse: a pool of address space, not a cache of derived data, so it can
/// never go stale. Reusing a mapping skips the page faults and kernel zeroing a fresh one costs.
pub(crate) struct MappedPool {
    mappings: Vec<Mapping>,
    bytes: usize,
    /// The most bytes kept, which [`crate::Heap`] sets from its limit after each major
    /// collection, so the memory the pool holds stays a small share of the heap.
    pub limit: usize,
}

struct Mapping {
    address: NonNull<u8>,
    length: usize,
}

impl MappedPool {
    pub fn new() -> MappedPool {
        MappedPool {
            mappings: Vec::new(),
            bytes: 0,
            limit: MIN_POOL_BYTES,
        }
    }

    /// Memory for an object of `size` bytes whose first `zeroed` bytes read as zero.
    ///
    /// # Safety
    /// `size` must be at least [`MAPPED_BYTES`], and the memory must be released with
    /// [`MappedPool::release`] and the same `size`.
    pub unsafe fn allocate(&mut self, size: usize, zeroed: usize) -> Option<NonNull<u8>> {
        let length = page_rounded(size)?;
        if let Some(address) = self.reuse(length) {
            std::ptr::write_bytes(address.as_ptr(), 0, zeroed.min(size));
            return Some(address);
        }
        // A fresh anonymous mapping reads as zero.
        map(length)
    }

    /// Gives back the memory of an object of `size` bytes, keeping it for reuse while the pool
    /// has room.
    ///
    /// # Safety
    /// `address` must come from [`MappedPool::allocate`] with the same `size`, and nothing may
    /// use it afterwards.
    pub unsafe fn release(&mut self, address: NonNull<u8>, size: usize) {
        let Some(length) = page_rounded(size) else {
            return;
        };
        if self.bytes + length <= self.limit {
            self.mappings.push(Mapping { address, length });
            self.bytes += length;
        } else {
            unmap(address, length);
        }
    }

    /// Returns every kept mapping to the kernel, as an explicit `gc()` returns free memory.
    pub fn flush(&mut self) {
        for mapping in self.mappings.drain(..) {
            // SAFETY: a kept mapping is owned by the pool alone.
            unsafe { unmap(mapping.address, mapping.length) };
        }
        self.bytes = 0;
    }

    /// Drops kept mappings until the pool fits its limit.
    pub fn trim(&mut self) {
        while self.bytes > self.limit {
            let Some(mapping) = self.mappings.pop() else {
                break;
            };
            self.bytes -= mapping.length;
            // SAFETY: a kept mapping is owned by the pool alone.
            unsafe { unmap(mapping.address, mapping.length) };
        }
    }

    /// A kept mapping resized to `length`: the smallest one at least that long, which only
    /// returns pages to the kernel. When none is long enough, the longest one grows and the rest
    /// go back to the kernel, because the vector adds pages either way and memory the pool keeps
    /// idle while pages are added would raise the peak above what returning every dead vector
    /// gives. Between those moments the pool's idle mappings, at most its limit, stay resident.
    unsafe fn reuse(&mut self, length: usize) -> Option<NonNull<u8>> {
        let lengths = || self.mappings.iter().map(|mapping| mapping.length).enumerate();
        if let Some((chosen, _)) = lengths()
            .filter(|&(_, kept)| kept >= length)
            .min_by_key(|&(_, kept)| kept)
        {
            let mapping = self.mappings.swap_remove(chosen);
            self.bytes -= mapping.length;
            return resize(mapping, length);
        }
        let (chosen, _) = lengths().max_by_key(|&(_, kept)| kept)?;
        let mapping = self.mappings.swap_remove(chosen);
        self.flush();
        resize(mapping, length)
    }
}

unsafe fn resize(mapping: Mapping, length: usize) -> Option<NonNull<u8>> {
    if mapping.length == length {
        return Some(mapping.address);
    }
    let resized = remap(mapping.address, mapping.length, length);
    if resized.is_none() {
        unmap(mapping.address, mapping.length);
    }
    resized
}

impl Drop for MappedPool {
    fn drop(&mut self) {
        self.flush();
    }
}

/// A global allocator that serves every block of at least [`MAPPED_BYTES`] from an anonymous
/// mapping of its own and everything smaller from `A`, as glibc's malloc does above its `mmap`
/// threshold. The buffers a builtin fills before it copies them into a vector are as large as
/// the vector, and an allocator that keeps freed memory resident for reuse would hold them on
/// top of the vectors, which come from mappings the allocator cannot reuse. Growing a mapped
/// block moves its pages with `mremap` instead of copying them.
pub struct Mapped<A>(pub A);

unsafe impl<A: GlobalAlloc> GlobalAlloc for Mapped<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if is_mapped(layout) {
            return page_rounded(layout.size())
                .and_then(|length| map(length))
                .map_or(std::ptr::null_mut(), NonNull::as_ptr);
        }
        self.0.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if is_mapped(layout) {
            // A fresh anonymous mapping reads as zero.
            return self.alloc(layout);
        }
        self.0.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, address: *mut u8, layout: Layout) {
        match NonNull::new(address) {
            Some(address) if is_mapped(layout) => {
                if let Some(length) = page_rounded(layout.size()) {
                    unmap_block(address, length);
                }
            }
            _ => self.0.dealloc(address, layout),
        }
    }

    unsafe fn realloc(&self, address: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return std::ptr::null_mut();
        };
        match (is_mapped(layout), is_mapped(new_layout), NonNull::new(address)) {
            (false, false, _) => self.0.realloc(address, layout, new_size),
            (true, true, Some(block)) => {
                match (page_rounded(layout.size()), page_rounded(new_size)) {
                    (Some(length), Some(new_length)) if length == new_length => address,
                    (Some(length), Some(new_length)) => remap(block, length, new_length)
                        .map_or(std::ptr::null_mut(), NonNull::as_ptr),
                    _ => std::ptr::null_mut(),
                }
            }
            _ => {
                let moved = self.alloc(new_layout);
                if !moved.is_null() {
                    std::ptr::copy_nonoverlapping(address, moved, layout.size().min(new_size));
                    self.dealloc(address, layout);
                }
                moved
            }
        }
    }
}

/// Whether a block of `layout` gets a mapping. A mapping is aligned to a page, so a block that
/// needs more alignment than that comes from the inner allocator.
/// Elsewhere every block comes from the inner allocator, because the fallback below allocates
/// mappings from the global allocator itself.
fn is_mapped(layout: Layout) -> bool {
    cfg!(target_os = "linux") && layout.size() >= MAPPED_BYTES && layout.align() <= page_size()
}

/// Unmaps a block from inside the global allocator, where a panic must not unwind.
unsafe fn unmap_block(address: NonNull<u8>, length: usize) {
    if !try_unmap(address, length) {
        std::process::abort();
    }
}

/// The pool's limit for a heap whose collection limit is `heap_limit`: half of it, and at
/// least [`MIN_POOL_BYTES`]. The pool maps nothing while it holds a mapping, so what it keeps
/// never lifts the peak above the most bytes the large vectors took at once; its limit only
/// bounds the memory left idle after such a peak. A loop of 8 MB vector arithmetic frees four
/// temporaries between allocations, and a quarter of the limit kept two: the other two faulted
/// in fresh pages every iteration, which took more time than the arithmetic.
pub(crate) fn pool_limit(heap_limit: usize) -> usize {
    (heap_limit / 2).max(MIN_POOL_BYTES)
}

const MIN_POOL_BYTES: usize = 32 << 20;

fn page_rounded(size: usize) -> Option<usize> {
    size.checked_next_multiple_of(page_size())
}

#[cfg(target_os = "linux")]
fn page_size() -> usize {
    use std::sync::OnceLock;
    static PAGE_SIZE: OnceLock<usize> = OnceLock::new();
    // SAFETY: `sysconf` has no preconditions.
    *PAGE_SIZE.get_or_init(|| match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        size if size > 0 => size as usize,
        _ => 4096,
    })
}

#[cfg(target_os = "linux")]
unsafe fn map(length: usize) -> Option<NonNull<u8>> {
    let address = libc::mmap(
        std::ptr::null_mut(),
        length,
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
        -1,
        0,
    );
    if address == libc::MAP_FAILED {
        return None;
    }
    NonNull::new(address.cast())
}

#[cfg(target_os = "linux")]
unsafe fn remap(address: NonNull<u8>, length: usize, new_length: usize) -> Option<NonNull<u8>> {
    let resized = libc::mremap(
        address.as_ptr().cast(),
        length,
        new_length,
        libc::MREMAP_MAYMOVE,
    );
    if resized == libc::MAP_FAILED {
        return None;
    }
    NonNull::new(resized.cast())
}

#[cfg(target_os = "linux")]
unsafe fn try_unmap(address: NonNull<u8>, length: usize) -> bool {
    libc::munmap(address.as_ptr().cast(), length) == 0
}

#[cfg(target_os = "linux")]
unsafe fn unmap(address: NonNull<u8>, length: usize) {
    if !try_unmap(address, length) {
        panic!(
            "munmap of a large vector's {length} bytes failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

// Elsewhere the global allocator serves large vectors too. With no `mremap` to resize a kept
// block in place, the pool reuses a block only for a vector of exactly its length. `Mapped`
// passes every block through.

#[cfg(not(target_os = "linux"))]
fn page_size() -> usize {
    16
}

#[cfg(not(target_os = "linux"))]
unsafe fn map(length: usize) -> Option<NonNull<u8>> {
    NonNull::new(std::alloc::alloc_zeroed(Layout::from_size_align_unchecked(
        length, 16,
    )))
}

#[cfg(not(target_os = "linux"))]
unsafe fn remap(_address: NonNull<u8>, _length: usize, _new_length: usize) -> Option<NonNull<u8>> {
    None
}

#[cfg(not(target_os = "linux"))]
unsafe fn try_unmap(address: NonNull<u8>, length: usize) -> bool {
    unmap(address, length);
    true
}

#[cfg(not(target_os = "linux"))]
unsafe fn unmap(address: NonNull<u8>, length: usize) {
    std::alloc::dealloc(
        address.as_ptr(),
        Layout::from_size_align_unchecked(length, 16),
    );
}
