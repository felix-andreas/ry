//! The `R_alloc` arena: transient memory that native code borrows for the duration of a foreign
//! call. R frees it when the `.Call`, `.C` or `.External` call that allocated it returns, and
//! `vmaxget`/`vmaxset` let code release it earlier, so it is a stack of nested watermarks.
//!
//! The arena bumps through fixed-size chunks. A request too large for a chunk gets a chunk of its
//! own, so a watermark is just a chunk count and an offset into the last chunk.

use std::{alloc::Layout, ptr::NonNull};

/// Every block is aligned like this, which covers every C scalar type, as R's own blocks do.
pub const ALIGN: usize = 16;
const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Default)]
pub struct Arena {
    chunks: Vec<Chunk>,
    /// The bytes used in the last chunk.
    offset: usize,
    /// One released standard chunk, kept so a loop that allocates and resets does not hit the
    /// system allocator every time.
    spare: Option<Chunk>,
}

/// A position in the arena, as `vmaxget` returns it. Opaque to native code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Watermark {
    chunks: usize,
    offset: usize,
}

struct Chunk {
    memory: NonNull<u8>,
    size: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ArenaExhausted {
    pub bytes: usize,
}

impl Arena {
    pub fn watermark(&self) -> Watermark {
        Watermark {
            chunks: self.chunks.len(),
            offset: self.offset,
        }
    }

    /// Encodes a watermark as the pointer `vmaxget` returns. Offsets stay below 2^32 because a
    /// chunk larger than the standard size is always full.
    pub fn encode(watermark: Watermark) -> usize {
        (watermark.chunks << 32) | watermark.offset
    }

    pub fn decode(encoded: usize) -> Watermark {
        Watermark {
            chunks: encoded >> 32,
            offset: encoded & 0xFFFF_FFFF,
        }
    }

    /// Releases everything allocated after `watermark`. A watermark from the future, which only
    /// buggy code can produce, is ignored, as GNU R ignores a `vmaxset` above the stack top.
    pub fn reset(&mut self, watermark: Watermark) {
        if watermark.chunks > self.chunks.len()
            || (watermark.chunks == self.chunks.len() && watermark.offset > self.offset)
        {
            return;
        }
        while self.chunks.len() > watermark.chunks {
            if let Some(chunk) = self.chunks.pop() {
                self.release(chunk);
            }
        }
        self.offset = if watermark.chunks == 0 {
            0
        } else {
            watermark.offset
        };
    }

    /// `R_alloc`: `bytes` of uninitialized memory, aligned to [`ALIGN`].
    pub fn allocate(&mut self, bytes: usize) -> Result<NonNull<u8>, ArenaExhausted> {
        let rounded = bytes
            .checked_add(ALIGN - 1)
            .ok_or(ArenaExhausted { bytes })?
            & !(ALIGN - 1);
        let rounded = rounded.max(ALIGN);
        if rounded > CHUNK_SIZE / 4 {
            let chunk = Chunk::new(rounded).ok_or(ArenaExhausted { bytes })?;
            let memory = chunk.memory;
            self.chunks.push(chunk);
            self.offset = rounded;
            return Ok(memory);
        }
        let fits = self
            .chunks
            .last()
            .is_some_and(|chunk| chunk.size == CHUNK_SIZE && self.offset + rounded <= chunk.size);
        if !fits {
            let chunk = match self.spare.take() {
                Some(chunk) => chunk,
                None => Chunk::new(CHUNK_SIZE).ok_or(ArenaExhausted { bytes })?,
            };
            self.chunks.push(chunk);
            self.offset = 0;
        }
        let Some(chunk) = self.chunks.last() else {
            return Err(ArenaExhausted { bytes });
        };
        // SAFETY: the offset plus the request fits in the chunk, checked or fresh above.
        let memory = unsafe { NonNull::new_unchecked(chunk.memory.as_ptr().add(self.offset)) };
        self.offset += rounded;
        Ok(memory)
    }

    /// The bytes currently held, for statistics and tests.
    pub fn allocated(&self) -> usize {
        match self.chunks.split_last() {
            Some((_, rest)) => rest.iter().map(|chunk| chunk.size).sum::<usize>() + self.offset,
            None => 0,
        }
    }

    fn release(&mut self, chunk: Chunk) {
        if chunk.size == CHUNK_SIZE && self.spare.is_none() {
            self.spare = Some(chunk);
        }
    }
}

impl Chunk {
    fn new(size: usize) -> Option<Chunk> {
        let layout = Layout::from_size_align(size, ALIGN).ok()?;
        // SAFETY: the layout has a nonzero size.
        let memory = NonNull::new(unsafe { std::alloc::alloc(layout) })?;
        Some(Chunk { memory, size })
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: allocated in `Chunk::new` with this layout.
        unsafe {
            std::alloc::dealloc(
                self.memory.as_ptr(),
                Layout::from_size_align_unchecked(self.size, ALIGN),
            )
        }
    }
}
