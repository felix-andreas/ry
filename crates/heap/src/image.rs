//! Heap images: a heap's pages as they stand once a session has started, written out so that
//! another process of the same build takes them over in place instead of rebuilding the objects.
//!
//! The small-object space of the first heap in a process starts at a fixed address
//! ([`crate::space::IMAGE_BASE`]), and its large objects move to a second fixed address when an
//! image is written. Every handle in the image is therefore the address the object has in the
//! loading process too, so the loader copies or maps the pages and patches nothing in them. Two
//! things make that sound:
//!
//! - **The same starting state.** The writer records a digest of its heap's allocation state at
//!   the moment its session began to start ([`Heap::image_precondition`]), and the loader compares
//!   it with its own heap's before it takes anything. Equal digests mean that every object the
//!   loading heap holds was allocated at the address where the image holds the same object, so
//!   the image's pages replace them without leaving a dangling handle.
//! - **No process-local addresses in the pages.** The few fields that hold one are cleared in the
//!   written pages and listed in the metadata, and the loader sets them for its own process: a
//!   frame's spilled bindings, an external pointer's address, and the contents of the fixed
//!   objects that hold native addresses (the ALTREP class tables), which keep the loader's.
//!
//! The metadata also carries the heap's state outside the pages: the intern table's slots, the
//! symbol table, the precious set, the global roots, the weak references and the counters.
//!
//! A string that names a path under a directory the writer saw elsewhere can be moved under the
//! loader's directory with a [`Relocation`]. Such a string is listed with the fields that refer
//! to it, and the loader allocates the moved string and points those fields at it.

use {
    crate::{
        Heap, HeapError,
        env::{Frame, FrameImage, TableImage},
        object::{ExternalPtrObject, Header, Sexp, SexpType, Shape, flag_bits, gp, holds_handles},
        space::{IMAGE_LARGE_BASE, PAGE_SIZE, virtual_memory},
        strings,
    },
    rustc_hash::FxHashMap,
    std::{borrow::Cow, ffi::c_void, fmt},
};

const MAGIC: &[u8; 8] = b"RYHEAP\x00\x03";

/// The objects that exist before a session starts and that an image holds at the addresses they
/// have in both processes.
pub struct Fixed<'a> {
    /// Every primitive object, by primitive index.
    pub primitives: &'a [Sexp],
    /// Further objects that exist before the image loads, in an order both heaps agree on.
    pub extra: &'a [Sexp],
}

/// Path prefixes an image's strings move between: a string equal to a `from` prefix, or one that
/// continues it with `/`, loads with that prefix replaced by its `to`.
#[derive(Default)]
pub struct Relocation {
    pub prefixes: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Relocation {
    /// `text` with its prefix moved, and the encoding marks the result needs given the marks
    /// `bits` the image recorded.
    pub fn apply<'t>(&self, text: &'t [u8], bits: u16) -> (Cow<'t, [u8]>, u16) {
        for (from, to) in &self.prefixes {
            if !under(from, text) || from == to {
                continue;
            }
            let moved = [to.as_slice(), &text[from.len()..]].concat();
            // A path keeps its marks unless the new prefix is not ASCII, which makes it UTF-8.
            let bits = if to.is_ascii() {
                bits
            } else {
                (bits & !(gp::ASCII | gp::ENCODING | gp::CACHED)) | gp::UTF8
            };
            return (Cow::Owned(moved), bits);
        }
        (Cow::Borrowed(text), bits)
    }
}

/// Whether `text` is the path `prefix` or a path below it.
fn under(prefix: &[u8], text: &[u8]) -> bool {
    text.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.first() == Some(&b'/'))
}

#[derive(Debug)]
pub enum ImageError {
    /// The heap holds something an image cannot carry; the message says what.
    Unsupported(String),
    /// The image is damaged or does not belong to this build.
    Invalid(String),
    /// The image cannot apply to this heap, which it left unchanged; the message says why.
    Mismatch(String),
    Heap(HeapError),
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Unsupported(message) | ImageError::Mismatch(message) => {
                write!(formatter, "{message}")
            }
            ImageError::Invalid(message) => write!(formatter, "the image is invalid: {message}"),
            ImageError::Heap(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<HeapError> for ImageError {
    fn from(error: HeapError) -> ImageError {
        ImageError::Heap(error)
    }
}

/// What [`Heap::write_image`] writes: the metadata, the bytes of the small-object pages from
/// the space's base, and the bytes of the large-object section from [`IMAGE_LARGE_BASE`]. The
/// two runs of bytes are kept in words so that the writer can clear fields in them in place.
pub struct HeapImage {
    pub metadata: Vec<u8>,
    pages: Vec<u64>,
    large: Vec<u64>,
}

impl HeapImage {
    pub fn pages(&self) -> &[u8] {
        words_as_bytes(&self.pages)
    }

    pub fn large(&self) -> &[u8] {
        words_as_bytes(&self.large)
    }
}

fn words_as_bytes(words: &[u64]) -> &[u8] {
    // SAFETY: any initialized `u64` is eight initialized bytes.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), words.len() * 8) }
}

/// Fills a run of memory with the bytes an image stored for it: by reading them, or by mapping
/// the file over it.
pub type Fill<'a> = &'a mut dyn FnMut(&mut [u8]) -> Result<(), ImageError>;

/// A 64-bit digest of a sequence of words.
pub struct Digest(u64);

impl Digest {
    const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

    pub fn new() -> Digest {
        Digest(Digest::SEED)
    }

    pub fn word(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(26) ^ word).wrapping_mul(Digest::SEED);
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Digest {
    fn default() -> Digest {
        Digest::new()
    }
}

/// A frame whose bindings live outside its environment object, as the image lists it.
struct ListedFrame {
    env: Sexp,
    image: FrameImage,
}

impl Heap {
    /// The digest of everything that decides where this heap allocates its next objects, and of
    /// the addresses of the fixed objects. An image applies only to a heap whose digest equals
    /// the one its writer's heap had when its session began to start.
    pub unsafe fn image_precondition(&self, fixed: &Fixed<'_>) -> u64 {
        let mut digest = Digest::new();
        self.space.digest_allocation(&mut digest);
        for count in [
            self.large.len(),
            self.strings.len(),
            self.symbols.len(),
            self.roots.precious.len(),
            self.roots.globals.entries.len(),
            self.weak_refs.len(),
            self.layouts.counts().0,
            self.layouts.counts().1,
        ] {
            digest.word(count as u64);
        }
        for object in self.fixed_objects(fixed) {
            digest.word(object.as_ptr() as u64);
        }
        digest.finish()
    }

    fn fixed_objects(&self, fixed: &Fixed<'_>) -> Vec<Sexp> {
        self.known
            .all()
            .into_iter()
            .chain(fixed.primitives.iter().copied())
            .chain(fixed.extra.iter().copied())
            .collect()
    }

    /// Writes the heap as an image that a heap whose [`Heap::image_precondition`] is
    /// `precondition` can load, and leaves this heap fit only to be dropped or exited from.
    ///
    /// Everything the heap's roots, the fixed objects and `roots` reach is kept; the loader
    /// returns `roots`, in order. `replace` names an object the image stores in place of another
    /// everywhere it is referenced, such as a closure body's source in place of its compiled
    /// code. `address` encodes each external pointer's address as a nonzero token the loading
    /// process can decode, or `None` when it cannot be carried. Strings that name a path at or
    /// under one of `prefixes` are listed so that a [`Relocation`] can move them.
    pub unsafe fn write_image(
        &mut self,
        fixed: &Fixed<'_>,
        precondition: u64,
        roots: &[Sexp],
        prefixes: &[Vec<u8>],
        replace: &dyn Fn(Sexp) -> Option<Sexp>,
        address: &mut dyn FnMut(Sexp) -> Option<u64>,
    ) -> Result<HeapImage, ImageError> {
        let unsupported = |message: &str| Err(ImageError::Unsupported(message.to_owned()));
        if !self.space.at_image_base() {
            return unsupported("the heap does not start at the image address");
        }
        if self.inhibit > 0 {
            return unsupported("collection is inhibited");
        }
        let fixed_objects = self.fixed_objects(fixed);
        let mut roots = roots.to_vec();
        let keep = |roots: &[Sexp]| [fixed_objects.as_slice(), roots].concat();

        self.collect_for_image(&keep(&roots));
        self.retire_escaped();
        let forward = |object: Sexp| replace(object);
        for object in self.live_objects() {
            object.for_each_handle_field(&mut |field| {
                if let Some(replacement) = forward(*field) {
                    *field = replacement;
                }
            });
        }
        self.forward_roots(&mut roots, &forward);
        let nil = self.known.nil;
        for object in self.live_objects() {
            if object.kind().shape() == Shape::Vector
                && holds_handles(object.kind())
                && object.flags() & flag_bits::WEAK_PAIRS != 0
            {
                // A weak-pair list is a cache whose pairs may be dead, so it loads empty.
                for index in 0..object.length() as usize {
                    object.set_element_unbarriered(index, nil);
                }
            }
        }
        for symbol in self.symbols.values() {
            symbol.reset_symbol_caches();
        }
        self.collect_for_image(&keep(&roots));
        if !self.ready_to_finalize.is_empty() {
            // Finalizers that start-up left pending are not run by the loading process either.
            self.ready_to_finalize.clear();
            self.collect_for_image(&keep(&roots));
        }
        self.promote_young();

        let mut out = Writer::default();
        out.bytes.extend_from_slice(MAGIC);
        out.u64(precondition);
        let large = self.move_large_objects(&mut roots)?;
        out.u64(IMAGE_LARGE_BASE as u64);
        out.u64(large.len() as u64 * 8);
        let mut large_objects: Vec<u64> = self
            .large
            .iter()
            .map(|object| object.as_ptr() as u64)
            .collect();
        large_objects.sort_unstable();
        out.u64s(&large_objects);

        let live = self.live_objects();
        let mut frames: Vec<ListedFrame> = Vec::new();
        let mut external: Vec<(Sexp, u64)> = Vec::new();
        for &object in &live {
            if object.is_altrep() {
                continue;
            }
            match object.kind().shape() {
                Shape::Environment if (*object.frame_ptr()).owns_memory() => {
                    frames.push(ListedFrame {
                        env: object,
                        image: (*object.frame_ptr()).image(),
                    });
                }
                Shape::ExternalPtr if !object.external_address().is_null() => {
                    match address(object) {
                        Some(token) if token != 0 => external.push((object, token)),
                        _ => {
                            return unsupported(
                                "an external pointer holds an address that cannot be carried",
                            );
                        }
                    }
                }
                Shape::WeakRef if object.weak_c_finalizer() != 0 => {
                    return unsupported("a weak reference has a C finalizer");
                }
                _ => {}
            }
        }
        let relocatable = self.relocatable_strings(&live, prefixes);

        let base = self.space.base();
        let page_bytes = self.space.write_image(&mut out);
        let mut pages = vec![0u64; page_bytes.len() / 8];
        std::ptr::copy_nonoverlapping(
            page_bytes.as_ptr(),
            pages.as_mut_ptr().cast::<u8>(),
            page_bytes.len(),
        );
        let copy = pages.as_mut_ptr().cast::<u8>();
        for (start, length) in self.space.free_ranges() {
            std::ptr::write_bytes(copy.add(start), 0, length);
        }
        for frame in &frames {
            let offset = frame.env.frame_ptr() as usize - base;
            Frame::write_empty(copy.add(offset).cast(), 0);
        }
        for &(object, _) in &external {
            let offset =
                object.as_ptr() as usize - base + std::mem::offset_of!(ExternalPtrObject, address);
            copy.add(offset)
                .cast::<*mut c_void>()
                .write_unaligned(std::ptr::null_mut());
        }

        for value in [
            self.old_small_bytes as u64,
            self.old_large_bytes as u64,
            self.tenured_vector_cells,
            self.retired_bytes,
            self.binding_epoch,
            self.enclosure_epoch,
            u64::from(self.everything_bound_elsewhere),
        ] {
            out.u64(value);
        }
        let (slots, len, tombstones) = self.strings.image();
        out.u64s(slots);
        out.u64(len as u64);
        out.u64(tombstones as u64);
        let mut symbols: Vec<u64> = Vec::with_capacity(self.symbols.len() * 2);
        for (&name, &symbol) in &self.symbols {
            symbols.extend([name.as_ptr() as u64, symbol.as_ptr() as u64]);
        }
        out.u64s(&symbols);
        out.u32(self.roots.precious.len() as u32);
        let precious = &self.roots.precious;
        for (&object, &count) in precious.objects.iter().zip(&precious.counts) {
            out.u64(object.as_ptr() as u64);
            out.u32(count);
        }
        out.u64s(&addresses(&self.roots.globals.entries));
        out.u64s(&addresses(&self.weak_refs));
        out.u64s(&addresses(&self.untracked));
        out.u32(frames.len() as u32);
        for frame in &frames {
            out.u64(frame.env.as_ptr() as u64);
            out.u32(frame.image.formals);
            out.u32(frame.image.removed);
            out.u32(frame.image.slots.len() as u32);
            for slot in &frame.image.slots {
                let (symbol, cell) = slot.map_or((0, 0), |(symbol, cell)| {
                    (symbol.as_ptr() as u64, cell.as_ptr() as u64)
                });
                out.u64(symbol);
                out.u64(cell);
            }
            match &frame.image.table {
                Some(table) => {
                    out.u8(1);
                    out.u32s(&table.heads);
                    out.u32s(&table.next);
                    out.u32s(&table.hashes);
                    out.u32(table.count);
                }
                None => out.u8(0),
            }
        }
        out.u32(external.len() as u32);
        for &(object, token) in &external {
            out.u64(object.as_ptr() as u64);
            out.u64(token);
        }
        out.u32(relocatable.len() as u32);
        for (string, referrers) in &relocatable {
            out.u64(string.as_ptr() as u64);
            out.u32(referrers.len() as u32);
            for &(parent, field) in referrers {
                out.u64(parent.as_ptr() as u64);
                out.u64(field as u64);
            }
        }
        out.u64s(&addresses(&roots));
        Ok(HeapImage {
            metadata: out.bytes,
            pages,
            large,
        })
    }

    /// Runs a major collection that keeps only what an image carries: what the heap's own
    /// roots, other than the per-call ones and the host's root sources, and `keep` reach.
    unsafe fn collect_for_image(&mut self, keep: &[Sexp]) {
        let sources = std::mem::take(&mut self.sources);
        let protect = std::mem::take(&mut self.roots.protect.stack);
        let handles = std::mem::take(&mut self.roots.handles);
        let handouts = std::mem::take(&mut self.roots.handouts);
        self.collect_keeping(crate::Collection::Major, keep, 0);
        self.sources = sources;
        self.roots.protect.stack = protect;
        self.roots.handles = handles;
        self.roots.handouts = handouts;
    }

    /// Every live object, outside a collection that left marks on the dead.
    unsafe fn live_objects(&self) -> Vec<Sexp> {
        let mut live: Vec<Sexp> = self.space.marked_objects().collect();
        let mut large: Vec<Sexp> = self.large.iter().copied().collect();
        large.sort_unstable_by_key(|object| object.as_ptr() as usize);
        live.extend(large);
        live
    }

    /// Points `roots` and the heap's root tables at what `forward` returns for their entries.
    unsafe fn forward_roots(&mut self, roots: &mut [Sexp], forward: &dyn Fn(Sexp) -> Option<Sexp>) {
        let moved = |object: Sexp| forward(object).unwrap_or(object);
        for root in roots.iter_mut() {
            *root = moved(*root);
        }
        for object in &mut self.roots.precious.objects {
            *object = moved(*object);
        }
        for entry in &mut self.roots.globals.entries {
            *entry = moved(*entry);
        }
        for list in [&mut self.weak_refs, &mut self.untracked] {
            for entry in list.iter_mut() {
                *entry = moved(*entry);
            }
        }
    }

    /// Moves every large object to [`IMAGE_LARGE_BASE`], where the loading process maps them
    /// too, and returns the section's bytes.
    unsafe fn move_large_objects(&mut self, roots: &mut [Sexp]) -> Result<Vec<u64>, ImageError> {
        let mut old: Vec<Sexp> = self.large.iter().copied().collect();
        old.sort_unstable_by_key(|object| object.as_ptr() as usize);
        let mut offsets = Vec::with_capacity(old.len());
        let mut length = 0;
        for &object in &old {
            offsets.push(length);
            length += self.size_of(object).next_multiple_of(16);
        }
        let section_length = length.next_multiple_of(PAGE_SIZE);
        if section_length == 0 {
            return Ok(Vec::new());
        }
        let start = IMAGE_LARGE_BASE as *mut u8;
        if virtual_memory::reserve_at(start, section_length).is_none()
            || !virtual_memory::commit(start, section_length)
        {
            return Err(ImageError::Unsupported(
                "the large objects' image address is taken".to_owned(),
            ));
        }
        self.image_large = IMAGE_LARGE_BASE..IMAGE_LARGE_BASE + section_length;
        let mut moves: FxHashMap<Sexp, Sexp> = FxHashMap::default();
        for (&object, &offset) in old.iter().zip(&offsets) {
            let target = start.add(offset);
            std::ptr::copy_nonoverlapping(
                object.as_ptr().cast::<u8>(),
                target,
                self.size_of(object),
            );
            let moved = Sexp::from_raw(target.cast())
                .ok_or_else(|| ImageError::Unsupported("a large object is null".to_owned()))?;
            moves.insert(object, moved);
        }
        let forward = |object: Sexp| moves.get(&object).copied();
        for &object in &old {
            self.free_large(object);
        }
        self.large = moves.values().copied().collect();
        for object in self.live_objects() {
            object.for_each_handle_field(&mut |field| {
                if let Some(moved) = forward(*field) {
                    *field = moved;
                }
            });
        }
        self.strings.forward(&forward);
        self.symbols = std::mem::take(&mut self.symbols)
            .into_iter()
            .map(|(name, symbol)| {
                (
                    forward(name).unwrap_or(name),
                    forward(symbol).unwrap_or(symbol),
                )
            })
            .collect();
        self.forward_roots(roots, &forward);
        let mut section = vec![0u64; section_length / 8];
        std::ptr::copy_nonoverlapping(start, section.as_mut_ptr().cast::<u8>(), section_length);
        Ok(section)
    }

    /// The strings that name a path at or under one of `prefixes`, each with the fields that
    /// refer to it. A symbol's name is left out, because moving it would rename the symbol.
    unsafe fn relocatable_strings(
        &self,
        live: &[Sexp],
        prefixes: &[Vec<u8>],
    ) -> Vec<(Sexp, Vec<(Sexp, *mut Sexp)>)> {
        let mut strings: FxHashMap<Sexp, Vec<(Sexp, *mut Sexp)>> = FxHashMap::default();
        for &object in live {
            if object.kind() == SexpType::Char
                && !object.is_altrep()
                && !self.symbols.contains_key(&object)
                && prefixes.iter().any(|prefix| under(prefix, object.bytes()))
            {
                strings.insert(object, Vec::new());
            }
        }
        if strings.is_empty() {
            return Vec::new();
        }
        for &object in live {
            object.for_each_handle_field(&mut |field| {
                if let Some(referrers) = strings.get_mut(&*field) {
                    referrers.push((object, field));
                }
            });
        }
        let mut strings: Vec<_> = strings.into_iter().collect();
        strings.sort_unstable_by_key(|(string, _)| string.as_ptr() as usize);
        strings
    }

    /// Loads an image [`Heap::write_image`] wrote in a process of the same build, and returns the
    /// objects its roots map to. `metadata` is the image's metadata; `pages` and `large` fill the
    /// memory of the small-object pages and of the large objects with the bytes the image holds
    /// for them. `address` decodes an external pointer's token, and `relocation` moves the paths
    /// in its strings.
    ///
    /// An [`ImageError::Mismatch`] leaves the heap unchanged. Any other error leaves it unusable.
    pub unsafe fn load_image(
        &mut self,
        metadata: &[u8],
        fixed: &Fixed<'_>,
        relocation: &Relocation,
        address: &mut dyn FnMut(u64) -> Option<*mut c_void>,
        pages: Fill<'_>,
        large: Fill<'_>,
    ) -> Result<Vec<Sexp>, ImageError> {
        self.inhibit_collection();
        let result = self.load_image_inhibited(metadata, fixed, relocation, address, pages, large);
        self.allow_collection();
        result
    }

    unsafe fn load_image_inhibited(
        &mut self,
        metadata: &[u8],
        fixed: &Fixed<'_>,
        relocation: &Relocation,
        address: &mut dyn FnMut(u64) -> Option<*mut c_void>,
        pages: Fill<'_>,
        large: Fill<'_>,
    ) -> Result<Vec<Sexp>, ImageError> {
        let mismatch = |message: &str| Err(ImageError::Mismatch(message.to_owned()));
        let mut input = Reader { bytes: metadata };
        if input.take(MAGIC.len())? != MAGIC {
            return Err(invalid("it does not start with the heap image signature"));
        }
        if input.u64()? != self.image_precondition(fixed) {
            return mismatch(
                "the heap before start-up differs from the one the image was written from",
            );
        }
        if !self.space.at_image_base() {
            return mismatch("the heap does not start at the image address");
        }
        if !self.large.is_empty() || !self.image_large.is_empty() {
            return mismatch("the heap holds large objects");
        }
        let large_base = input.u64()? as usize;
        let large_length = input.u64()? as usize;
        let large_objects = input.u64s()?;
        if large_base != IMAGE_LARGE_BASE || large_length % PAGE_SIZE != 0 {
            return Err(invalid("its large objects are not at the image address"));
        }
        let section = IMAGE_LARGE_BASE as *mut u8;
        if large_length > 0 && virtual_memory::reserve_at(section, large_length).is_none() {
            return mismatch("the large objects' image address is taken");
        }
        if large_length > 0 {
            self.image_large = IMAGE_LARGE_BASE..IMAGE_LARGE_BASE + large_length;
        }

        // Objects outside the heap's tables that the loading process holds already must be among
        // the image's objects.
        let held: Vec<Sexp> = [
            &self.roots.protect.stack.entries,
            &self.roots.handles.entries,
            &self.roots.handouts.entries,
        ]
        .into_iter()
        .flatten()
        .copied()
        .collect();
        // The fixed objects that hold native addresses keep this process's contents.
        let kept: Vec<(Sexp, usize)> = self
            .fixed_objects(fixed)
            .into_iter()
            .filter(|&object| keeps_contents(object))
            .map(|object| (object, self.size_of(object)))
            .collect();
        let header = size_of::<Header>();
        let mut fill_pages = |memory: &mut [u8]| -> Result<(), ImageError> {
            let saved: Vec<Vec<u8>> = kept
                .iter()
                .map(|&(object, size)| {
                    std::slice::from_raw_parts(
                        object.as_ptr().cast::<u8>().add(header),
                        size - header,
                    )
                    .to_vec()
                })
                .collect();
            pages(memory)?;
            for (&(object, _), contents) in kept.iter().zip(&saved) {
                std::ptr::copy_nonoverlapping(
                    contents.as_ptr(),
                    object.as_ptr().cast::<u8>().add(header),
                    contents.len(),
                );
            }
            Ok(())
        };
        self.space.load_image(&mut input, &held, &mut fill_pages)?;
        if large_length > 0 {
            if !virtual_memory::commit(section, large_length) {
                return Err(ImageError::Heap(HeapError::CannotAllocate {
                    bytes: large_length,
                }));
            }
            large(std::slice::from_raw_parts_mut(section, large_length))?;
        }

        self.remembered.clear();
        self.escaped.clear();
        self.escaped_lists.clear();
        self.zero_count.clear();
        self.returned.clear();
        self.expect_dead.clear();
        self.new_roots.clear();
        self.young_large.clear();
        self.weak_pairs.clear();
        self.ready_to_finalize.clear();
        self.mark_stack.clear();
        self.zero_bytes = 0;
        self.reconcile_at = crate::RECONCILE_ENTRIES;
        self.young_bytes = 0;
        self.young_large_bytes = 0;
        self.large_allocated_since_major = 0;
        self.roots.protect.stack.scanned = 0;
        self.roots.handles.scanned = 0;
        self.roots.handouts.scanned = 0;
        self.large = large_objects
            .iter()
            .map(|&object| handle(object))
            .collect::<Result<_, _>>()?;

        self.old_small_bytes = input.u64()? as usize;
        self.old_large_bytes = input.u64()? as usize;
        self.tenured_vector_cells = input.u64()?;
        self.retired_bytes = input.u64()?;
        self.binding_epoch = self.binding_epoch.max(input.u64()?) + 1;
        self.enclosure_epoch = self.enclosure_epoch.max(input.u64()?) + 1;
        self.everything_bound_elsewhere |= input.u64()? != 0;
        let slots = input.u64s()?;
        let len = input.u64()? as usize;
        let tombstones = input.u64()? as usize;
        if !self.strings.load_image(slots, len, tombstones) {
            return Err(invalid("its string table is malformed"));
        }
        let symbols = input.u64s()?;
        self.symbols.clear();
        for pair in symbols.chunks_exact(2) {
            self.symbols.insert(handle(pair[0])?, handle(pair[1])?);
        }
        self.roots.precious.clear();
        for _ in 0..input.u32()? {
            let object = handle(input.u64()?)?;
            let count = input.u32()?;
            self.roots.precious.add(object, count);
        }
        self.roots.globals.entries = handles(&input.u64s()?)?;
        self.roots.globals.scanned = 0;
        self.weak_refs = handles(&input.u64s()?)?;
        self.untracked = handles(&input.u64s()?)?;

        let nil = self.known.nil;
        for _ in 0..input.u32()? {
            let env = handle(input.u64()?)?;
            let formals = input.u32()?;
            let removed = input.u32()?;
            let slot_count = input.u32()? as usize;
            let mut slots = Vec::with_capacity(slot_count);
            for _ in 0..slot_count {
                let symbol = input.u64()?;
                let cell = input.u64()?;
                slots.push(if symbol == 0 {
                    None
                } else {
                    Some((handle(symbol)?, handle(cell)?))
                });
            }
            let table = match input.u8()? {
                0 => None,
                _ => Some(TableImage {
                    heads: input.u32s()?,
                    next: input.u32s()?,
                    hashes: input.u32s()?,
                    count: input.u32()?,
                }),
            };
            if env.kind() != SexpType::Environment {
                return Err(invalid("a listed frame is not an environment's"));
            }
            let image = FrameImage {
                slots,
                formals,
                removed,
                table,
            };
            // The page holds an empty frame that owns nothing, so it is overwritten unread.
            std::ptr::write(env.frame_ptr(), Frame::from_image(image, nil));
        }
        for _ in 0..input.u32()? {
            let object = handle(input.u64()?)?;
            let token = input.u64()?;
            let pointer = address(token)
                .ok_or_else(|| invalid("an external pointer's address does not resolve"))?;
            object.set_external_address(pointer);
        }

        let mut moved: FxHashMap<Sexp, Sexp> = FxHashMap::default();
        for _ in 0..input.u32()? {
            let string = handle(input.u64()?)?;
            let referrers = input.u32()? as usize;
            let mut fields = Vec::with_capacity(referrers);
            for _ in 0..referrers {
                fields.push((handle(input.u64()?)?, input.u64()? as usize as *mut Sexp));
            }
            let (text, bits) = relocation.apply(string.bytes(), string.gp());
            let Cow::Owned(text) = text else {
                continue;
            };
            let replacement = if string.has_gp(gp::CACHED) {
                match self.find_interned(&text, bits) {
                    Some(found) => found,
                    None => self.intern_with_bits(&text, bits)?,
                }
            } else {
                let replacement = self.new_char(&text, 0)?;
                replacement.set_gp(bits);
                replacement
            };
            replacement.set_refcount(replacement.refcount().max(string.refcount()));
            for (parent, field) in fields {
                *field = replacement;
                if parent.is_carded() {
                    let index = (field as usize - parent.data() as usize) / size_of::<Sexp>();
                    self.element_barrier(parent, index, replacement);
                } else {
                    self.write_barrier(parent, replacement);
                }
            }
            moved.insert(string, replacement);
        }
        let mut roots = handles(&input.u64s()?)?;
        if !input.bytes.is_empty() {
            return Err(invalid("it has bytes past its end"));
        }
        if !moved.is_empty() {
            self.forward_roots(&mut roots, &|object| moved.get(&object).copied());
        }
        self.refresh_small_trigger();
        self.stats.live_vector_cells = self.tenured_vector_cells;
        self.refresh_live_stats();
        Ok(roots)
    }

    pub(crate) unsafe fn find_interned(&self, text: &[u8], bits: u16) -> Option<Sexp> {
        let scan = strings::scan(text);
        let hash = strings::hash(&scan, bits & gp::ENCODING);
        self.strings.find(hash, text, bits & gp::ENCODING)
    }

    /// Interns a string the table lacks with exactly the marks the image recorded.
    unsafe fn intern_with_bits(&mut self, text: &[u8], bits: u16) -> Result<Sexp, ImageError> {
        let scan = strings::scan(text);
        let hash = strings::hash(&scan, bits & gp::ENCODING);
        Ok(self.intern_new(text, bits & !gp::CACHED, hash)?)
    }
}

/// Whether a fixed object keeps the loading process's contents over the image's: a primitive,
/// and a vector of plain elements, which may hold native addresses (an ALTREP class's methods)
/// and which start-up does not change otherwise.
unsafe fn keeps_contents(object: Sexp) -> bool {
    match object.kind().shape() {
        Shape::Primitive => true,
        Shape::Vector => !object.is_altrep() && !holds_handles(object.kind()),
        _ => false,
    }
}

fn addresses(objects: &[Sexp]) -> Vec<u64> {
    objects
        .iter()
        .map(|object| object.as_ptr() as u64)
        .collect()
}

/// The handle at `address`, which the image's writer recorded and which the loader trusts as
/// much as the pages it maps.
unsafe fn handle(address: u64) -> Result<Sexp, ImageError> {
    unsafe { Sexp::from_raw(address as usize as *mut Header) }
        .ok_or_else(|| invalid("a handle is null"))
}

unsafe fn handles(addresses: &[u64]) -> Result<Vec<Sexp>, ImageError> {
    addresses
        .iter()
        .map(|&address| unsafe { handle(address) })
        .collect()
}

#[cold]
fn invalid(message: &str) -> ImageError {
    ImageError::Invalid(message.to_owned())
}

/// Little-endian encoding for images and the state hosts store beside them.
#[derive(Default)]
pub struct Writer {
    pub bytes: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn blob(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
    }

    pub fn u64s(&mut self, values: &[u64]) {
        self.u64(values.len() as u64);
        for &value in values {
            self.u64(value);
        }
    }

    pub fn u32s(&mut self, values: &[u32]) {
        self.u32(values.len() as u32);
        for &value in values {
            self.u32(value);
        }
    }
}

/// Reads what [`Writer`] wrote; every read fails with [`ImageError::Invalid`] past the end.
pub struct Reader<'a> {
    pub bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    #[inline(always)]
    pub fn take(&mut self, count: usize) -> Result<&'a [u8], ImageError> {
        if self.bytes.len() < count {
            return Err(invalid("it ends early"));
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    #[inline(always)]
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ImageError> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    #[inline(always)]
    pub fn u8(&mut self) -> Result<u8, ImageError> {
        Ok(self.array::<1>()?[0])
    }

    #[inline(always)]
    pub fn u16(&mut self) -> Result<u16, ImageError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    #[inline(always)]
    pub fn u32(&mut self) -> Result<u32, ImageError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    #[inline(always)]
    pub fn u64(&mut self) -> Result<u64, ImageError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    #[inline(always)]
    pub fn blob(&mut self) -> Result<&'a [u8], ImageError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| invalid("a length is too large"))?;
        self.take(length)
    }

    pub fn u32s(&mut self) -> Result<Vec<u32>, ImageError> {
        let count = self.u32()? as usize;
        if self.bytes.len() < count * 4 {
            return Err(invalid("it ends early"));
        }
        let (words, rest) = self.bytes.split_at(count * 4);
        self.bytes = rest;
        Ok(words
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect())
    }

    pub fn u64s(&mut self) -> Result<Vec<u64>, ImageError> {
        let count = usize::try_from(self.u64()?).map_err(|_| invalid("a length is too large"))?;
        let bytes = self.take(
            count
                .checked_mul(8)
                .ok_or_else(|| invalid("a length is too large"))?,
        )?;
        Ok(bytes
            .chunks_exact(8)
            .map(|word| {
                let mut array = [0u8; 8];
                array.copy_from_slice(word);
                u64::from_le_bytes(array)
            })
            .collect())
    }
}
