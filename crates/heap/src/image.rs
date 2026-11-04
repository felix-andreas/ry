//! Heap images: the objects reachable from a heap's roots, written out with every reference as a
//! dense index, and loaded back into another heap of the same build.
//!
//! An image is not mapped in place. The loader allocates every object afresh and then patches
//! its references, which costs a few milliseconds for a started R session and keeps the heap free
//! of a second kind of object that would need copy-on-write.
//!
//! Some objects already exist in the heap an image loads into, and an image refers to them
//! instead of copying them:
//!
//! - the *fixed* objects: the well-known objects, the primitives by index, and any others the
//!   caller names in a fixed order (such as registered ALTREP classes);
//! - interned strings and symbols, which load by their text, so a symbol keeps its identity;
//! - the binding cells of the base environment, which hang off their symbols.
//!
//! The loader overwrites the contents of the mutable ones among these (an environment's frame, a
//! symbol's base binding), so the loaded heap holds what the image's heap held.
//!
//! A string that names a path under a directory the writer saw elsewhere can be moved under the
//! loader's directory with a [`Relocation`], so an image built in one place loads in another.
//!
//! The image has two passes' worth of records. The allocation table describes every object that
//! is not fixed, in index order, with what allocating it needs: a vector's length and its plain
//! elements, a string's text, a symbol's name. The contents then give every object's header and
//! references, which the loader writes once every object exists.

use {
    crate::{
        Heap, HeapError,
        env::{Frame, FrameImage, TableImage},
        object::{Sexp, SexpType, Shape, flag_bits, gc_bits, gp, object_size},
        strings,
    },
    rustc_hash::{FxHashMap, FxHashSet},
    std::{borrow::Cow, ffi::c_void, fmt},
};

const MAGIC: &[u8; 8] = b"RYIMAGE\x01";

/// The objects an image refers to by position instead of storing.
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
            let Some(rest) = text.strip_prefix(from.as_slice()) else {
                continue;
            };
            if !(rest.is_empty() || rest.first() == Some(&b'/')) || from == to {
                continue;
            }
            let moved = [to.as_slice(), rest].concat();
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

#[derive(Debug)]
pub enum ImageError {
    /// The heap holds something an image cannot carry; the message says what.
    Unsupported(String),
    /// The image does not match the heap it is loaded into, or is damaged.
    Invalid(String),
    /// The image was written against other fixed objects; the heap is unchanged.
    FixedObjects,
    Heap(HeapError),
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Unsupported(message) => write!(formatter, "{message}"),
            ImageError::Invalid(message) => write!(formatter, "the image is invalid: {message}"),
            ImageError::FixedObjects => {
                write!(formatter, "its fixed objects differ from this heap's")
            }
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

/// How an object that is not fixed comes to exist in the loading heap.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    /// An interned string, found or interned by its text.
    String,
    /// A symbol, installed by its name, which is a string earlier in the table.
    Symbol,
    /// A symbol's binding in the base environment, reused if the loading heap has one.
    BaseCell,
    /// Anything else, allocated afresh.
    Fresh,
}

const ORIGIN_STRING: u8 = 0;
const ORIGIN_SYMBOL: u8 = 1;
const ORIGIN_BASE_CELL: u8 = 2;
const ORIGIN_FRESH: u8 = 3;
const ORIGIN_ALTREP: u8 = 4;

const NO_INDEX: u32 = u32::MAX;

impl Heap {
    /// Writes the objects reachable from the heap's roots and from `roots` as an image. The
    /// loader returns `roots` mapped to its own objects, in order. `address` encodes each
    /// external pointer's address as a nonzero token the loading process can decode, or `None`
    /// when it cannot be carried. `replace` names an object the image stores in place of another
    /// everywhere it is referenced, such as a closure body's source in place of its compiled code.
    pub unsafe fn write_image(
        &self,
        fixed: &Fixed<'_>,
        roots: &[Sexp],
        replace: &dyn Fn(Sexp) -> Option<Sexp>,
        address: &mut dyn FnMut(Sexp) -> Option<u64>,
    ) -> Result<Vec<u8>, ImageError> {
        let resolve = |object: Sexp| replace(object).unwrap_or(object);
        let mut index: FxHashMap<Sexp, u32> = FxHashMap::default();
        let mut objects: Vec<Sexp> = Vec::new();
        let fixed_objects: Vec<Sexp> = self
            .known
            .all()
            .into_iter()
            .chain(fixed.primitives.iter().copied())
            .chain(fixed.extra.iter().copied())
            .collect();
        for &object in &fixed_objects {
            if index.insert(object, objects.len() as u32).is_some() {
                return Err(ImageError::Unsupported(format!(
                    "a fixed {} is listed twice",
                    object.kind().name()
                )));
            }
            objects.push(object);
        }
        let fixed_count = objects.len();

        let mut stack: Vec<Sexp> = fixed_objects.clone();
        let mut discovered: Vec<Sexp> = Vec::new();
        let mut seen: FxHashSet<Sexp> = fixed_objects.iter().copied().collect();
        let mut visit = |object: Sexp, stack: &mut Vec<Sexp>, discovered: &mut Vec<Sexp>| {
            if seen.insert(object) {
                discovered.push(object);
                stack.push(object);
            }
        };
        let mut start: Vec<Sexp> = self.symbols.values().copied().collect();
        start.extend(self.roots.precious.keys().copied());
        start.extend(self.roots.globals.entries.iter().copied());
        start.extend(roots.iter().copied());
        for object in start {
            visit(resolve(object), &mut stack, &mut discovered);
        }
        while let Some(object) = stack.pop() {
            for child in children(object, self.known.nil) {
                visit(resolve(child), &mut stack, &mut discovered);
            }
        }

        let mut classified: Vec<(Origin, Sexp)> = Vec::with_capacity(discovered.len());
        for &object in &discovered {
            classified.push((self.origin(object)?, object));
        }
        // Strings come first so that a symbol's name loads before the symbol, and symbols before
        // their base bindings.
        classified.sort_by_key(|&(origin, _)| origin);
        for &(_, object) in &classified {
            index.insert(object, objects.len() as u32);
            objects.push(object);
        }

        let reference = |object: Sexp| -> Result<u32, ImageError> {
            let object = resolve(object);
            index.get(&object).copied().ok_or_else(|| {
                ImageError::Unsupported(format!(
                    "a {} is referenced but was not reached",
                    // SAFETY: the object is live, being referenced from a reached one.
                    unsafe { object.kind().name() }
                ))
            })
        };

        let mut out = Writer::default();
        out.bytes.extend_from_slice(MAGIC);
        out.u32(fixed_count as u32);
        out.u32(objects.len() as u32);

        for &(origin, object) in &classified {
            match origin {
                Origin::String => {
                    out.u8(ORIGIN_STRING);
                    out.u16(object.gp());
                    out.blob(object.bytes());
                }
                Origin::Symbol => {
                    out.u8(ORIGIN_SYMBOL);
                    out.u32(reference(object.car())?);
                }
                Origin::BaseCell => {
                    out.u8(ORIGIN_BASE_CELL);
                    out.u32(reference(object.car())?);
                }
                Origin::Fresh if object.is_altrep() => {
                    out.u8(ORIGIN_ALTREP);
                    out.u8(object.kind() as u8);
                }
                Origin::Fresh => {
                    out.u8(ORIGIN_FRESH);
                    let kind = object.kind();
                    out.u8(kind as u8);
                    match kind.shape() {
                        Shape::Vector => {
                            out.u64(object.length() as u64);
                            if kind == SexpType::Char {
                                // An uninterned string: its text and marks are all it has.
                                out.u16(object.gp());
                                out.blob(object.bytes());
                            } else {
                                out.u64(object.capacity() as u64);
                                if !holds_handles(kind) {
                                    let size = kind.element_size().unwrap_or(0);
                                    out.blob(std::slice::from_raw_parts(
                                        object.data(),
                                        object.length() as usize * size,
                                    ));
                                }
                            }
                        }
                        Shape::Record => {
                            out.u32(object.record_layout());
                            out.u32(object.record_slot_count() as u32);
                        }
                        Shape::Primitive => {
                            return Err(ImageError::Unsupported(format!(
                                "the primitive with index {} is not the registered one",
                                object.primitive_index()
                            )));
                        }
                        Shape::Triple
                        | Shape::Environment
                        | Shape::ExternalPtr
                        | Shape::WeakRef => {}
                    }
                }
            }
        }

        for (position, &object) in objects.iter().enumerate() {
            out.u16(object.refcount());
            if !restores_contents(object, position < fixed_count, &self.known) {
                out.u8(0);
                continue;
            }
            out.u8(1);
            out.u16(object.gp());
            out.u8(object.flags());
            out.u32(reference(object.attrib())?);
            if object.is_altrep() {
                for field in [object.car(), object.cdr(), object.tag()] {
                    out.u32(reference(field)?);
                }
                continue;
            }
            match object.kind().shape() {
                Shape::Vector => {
                    if holds_handles(object.kind()) {
                        let weak = object.flags() & flag_bits::WEAK_PAIRS != 0;
                        for element in 0..object.length() as usize {
                            // A weak-pair list is a cache whose pairs may be dead, so it loads
                            // empty.
                            let element = if weak {
                                self.known.nil
                            } else {
                                object.element(element)
                            };
                            out.u32(reference(element)?);
                        }
                    }
                }
                Shape::Triple => {
                    for field in [object.car(), object.cdr(), object.tag()] {
                        out.u32(reference(field)?);
                    }
                }
                Shape::Environment => {
                    out.u32(reference(object.enclosure())?);
                    let frame = (*object.frame_ptr()).image();
                    out.u32(frame.slots.len() as u32);
                    for slot in &frame.slots {
                        match *slot {
                            Some((symbol, cell)) => {
                                out.u32(reference(symbol)?);
                                out.u32(reference(cell)?);
                            }
                            None => {
                                out.u32(NO_INDEX);
                                out.u32(NO_INDEX);
                            }
                        }
                    }
                    out.u32(frame.formals);
                    out.u32(frame.removed);
                    match &frame.table {
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
                Shape::ExternalPtr => {
                    let token = if object.external_address().is_null() {
                        0
                    } else {
                        match address(object) {
                            Some(token) if token != 0 => token,
                            _ => {
                                return Err(ImageError::Unsupported(
                                    "an external pointer holds an address that cannot be carried"
                                        .to_owned(),
                                ));
                            }
                        }
                    };
                    out.u64(token);
                    out.u32(reference(object.external_tag())?);
                    out.u32(reference(object.external_protected())?);
                }
                Shape::WeakRef => {
                    if object.weak_c_finalizer() != 0 {
                        return Err(ImageError::Unsupported(
                            "a weak reference has a C finalizer".to_owned(),
                        ));
                    }
                    out.u32(reference(object.weak_key())?);
                    out.u32(reference(object.weak_value())?);
                    out.u32(reference(object.weak_finalizer())?);
                    out.u8(u8::from(object.weak_finalize_on_exit()));
                }
                Shape::Record => {
                    out.u64(object.record_unboxed());
                    for slot in 0..object.record_slot_count() {
                        let (bits, raw) = object.record_slot_bits(slot);
                        if raw {
                            out.u64(bits);
                        } else {
                            let handle = Sexp::from_raw(bits as *mut crate::object::Header)
                                .ok_or_else(|| {
                                    ImageError::Unsupported("a record slot is null".to_owned())
                                })?;
                            out.u64(u64::from(reference(handle)?));
                        }
                    }
                }
                Shape::Primitive => {}
            }
        }

        let mut precious: Vec<(u32, u32)> = Vec::new();
        for (&object, &count) in &self.roots.precious {
            let position = reference(object)?;
            if position as usize >= fixed_count {
                precious.push((position, count));
            }
        }
        precious.sort_unstable();
        out.u32(precious.len() as u32);
        for (position, count) in precious {
            out.u32(position);
            out.u32(count);
        }
        let mut globals: Vec<u32> = Vec::new();
        for &object in &self.roots.globals.entries {
            let position = reference(object)?;
            if position as usize >= fixed_count {
                globals.push(position);
            }
        }
        out.u32s(&globals);
        let roots: Vec<u32> = roots
            .iter()
            .map(|&root| reference(root))
            .collect::<Result<_, _>>()?;
        out.u32s(&roots);
        Ok(out.bytes)
    }

    /// Loads an image [`Heap::write_image`] wrote in a process of the same build, and returns the
    /// objects its roots map to. `address` decodes an external pointer's token, and `relocation`
    /// moves the paths in its strings. The heap must
    /// hold no state the image replaces other than what a fresh start leaves: the image's
    /// environments, symbols and bindings overwrite the loading heap's.
    pub unsafe fn load_image(
        &mut self,
        image: &[u8],
        fixed: &Fixed<'_>,
        relocation: &Relocation,
        address: &mut dyn FnMut(u64) -> Option<*mut c_void>,
    ) -> Result<Vec<Sexp>, ImageError> {
        self.inhibit_collection();
        let result = self.load_image_inhibited(image, fixed, relocation, address);
        self.allow_collection();
        if result.is_ok() {
            self.promote_young();
        }
        result
    }

    unsafe fn load_image_inhibited(
        &mut self,
        image: &[u8],
        fixed: &Fixed<'_>,
        relocation: &Relocation,
        address: &mut dyn FnMut(u64) -> Option<*mut c_void>,
    ) -> Result<Vec<Sexp>, ImageError> {
        let mut input = Reader { bytes: image };
        if input.take(MAGIC.len())? != MAGIC {
            return Err(invalid("it does not start with the image signature"));
        }
        let fixed_count = input.u32()? as usize;
        let object_count = input.u32()? as usize;
        let mut objects: Vec<Sexp> = Vec::with_capacity(object_count);
        objects.extend(self.known.all());
        objects.extend(fixed.primitives.iter().copied());
        objects.extend(fixed.extra.iter().copied());
        if objects.len() != fixed_count || object_count < fixed_count {
            return Err(ImageError::FixedObjects);
        }
        // Objects that existed before the load keep their reference counts at least as high as
        // the image's, and an old one that comes to point at a loaded object is remembered.
        let mut preexisting = vec![true; fixed_count];
        let nil = self.known.nil;

        while objects.len() < object_count {
            let origin = input.u8()?;
            let (object, existed) = match origin {
                ORIGIN_STRING => {
                    let bits = input.u16()?;
                    let (text, bits) = relocation.apply(input.blob()?, bits);
                    let existed = self.find_interned(&text, bits);
                    match existed {
                        Some(found) => (found, true),
                        None => (self.intern_with_bits(&text, bits)?, false),
                    }
                }
                ORIGIN_SYMBOL => {
                    let name = lookup(&objects, input.u32()?)?;
                    let existed = self.symbols.contains_key(&name);
                    (self.install_char(name)?, existed)
                }
                ORIGIN_BASE_CELL => {
                    let symbol = lookup(&objects, input.u32()?)?;
                    let cell = symbol.cdr();
                    if cell.kind() == SexpType::Binding {
                        (cell, true)
                    } else {
                        let size = object_size(SexpType::Binding, 0).unwrap_or(40);
                        let cell = self.allocate_uncollected(SexpType::Binding, size, size)?;
                        cell.set_car_unbarriered(symbol);
                        cell.set_cdr_unbarriered(nil);
                        cell.set_tag_unbarriered(nil);
                        (cell, false)
                    }
                }
                ORIGIN_ALTREP => {
                    let kind = kind_of(input.u8()?)?;
                    let size = object_size(SexpType::Pairlist, 0).unwrap_or(40);
                    (self.allocate_uncollected(kind, size, size)?, false)
                }
                ORIGIN_FRESH => (self.allocate_from_image(&mut input, relocation)?, false),
                _ => return Err(invalid("an object has an unknown origin")),
            };
            objects.push(object);
            preexisting.push(existed);
        }

        for position in 0..object_count {
            let object = objects[position];
            let refcount = input.u16()?;
            let existed = preexisting[position];
            if existed {
                object.set_refcount(object.refcount().max(refcount));
            } else {
                object.set_refcount(refcount);
            }
            if input.u8()? == 0 {
                continue;
            }
            object.set_gp(input.u16()?);
            object.set_flags(input.u8()?);
            object.set_attrib_unbarriered(lookup(&objects, input.u32()?)?);
            if existed && object.gc_bits() & (gc_bits::OLD | gc_bits::REMEMBERED) == gc_bits::OLD {
                object.set_gc_bit(gc_bits::REMEMBERED, true);
                self.remembered.push(object);
            }
            if object.is_altrep() {
                object.set_car_unbarriered(lookup(&objects, input.u32()?)?);
                object.set_cdr_unbarriered(lookup(&objects, input.u32()?)?);
                object.set_tag_unbarriered(lookup(&objects, input.u32()?)?);
                continue;
            }
            match object.kind().shape() {
                Shape::Vector => {
                    if holds_handles(object.kind()) {
                        for element in 0..object.length() as usize {
                            object
                                .set_element_unbarriered(element, lookup(&objects, input.u32()?)?);
                        }
                    }
                }
                Shape::Triple => {
                    let car = lookup(&objects, input.u32()?)?;
                    let cdr = lookup(&objects, input.u32()?)?;
                    let tag = lookup(&objects, input.u32()?)?;
                    // A symbol's name is what found it.
                    if object.kind() != SexpType::Symbol {
                        object.set_car_unbarriered(car);
                    } else {
                        object.reset_symbol_caches();
                    }
                    object.set_cdr_unbarriered(cdr);
                    object.set_tag_unbarriered(tag);
                }
                Shape::Environment => {
                    object.set_enclosure_unbarriered(lookup(&objects, input.u32()?)?);
                    let slot_count = input.u32()? as usize;
                    let mut slots = Vec::with_capacity(slot_count);
                    for _ in 0..slot_count {
                        let symbol = input.u32()?;
                        let cell = input.u32()?;
                        slots.push(if symbol == NO_INDEX {
                            None
                        } else {
                            Some((lookup(&objects, symbol)?, lookup(&objects, cell)?))
                        });
                    }
                    let formals = input.u32()?;
                    let removed = input.u32()?;
                    let table = match input.u8()? {
                        0 => None,
                        _ => Some(TableImage {
                            heads: input.u32s()?,
                            next: input.u32s()?,
                            hashes: input.u32s()?,
                            count: input.u32()?,
                        }),
                    };
                    let frame = Frame::from_image(
                        FrameImage {
                            slots,
                            formals,
                            removed,
                            table,
                        },
                        nil,
                    );
                    *object.frame_ptr() = frame;
                }
                Shape::ExternalPtr => {
                    let token = input.u64()?;
                    let pointer = if token == 0 {
                        std::ptr::null_mut()
                    } else {
                        address(token).ok_or_else(|| {
                            invalid("an external pointer's address does not resolve")
                        })?
                    };
                    object.set_external_address(pointer);
                    object.set_external_tag_unbarriered(lookup(&objects, input.u32()?)?);
                    object.set_external_protected_unbarriered(lookup(&objects, input.u32()?)?);
                }
                Shape::WeakRef => {
                    let payload = object.weak_payload();
                    payload.key.set(lookup(&objects, input.u32()?)?);
                    payload.value.set(lookup(&objects, input.u32()?)?);
                    payload.finalizer.set(lookup(&objects, input.u32()?)?);
                    payload.c_finalizer.set(0);
                    payload.finalize_on_exit.set(input.u8()? != 0);
                }
                Shape::Record => {
                    let unboxed = input.u64()?;
                    for slot in 0..object.record_slot_count() {
                        let bits = input.u64()?;
                        let raw = slot < crate::object::UNBOXED_SLOTS && unboxed & (1 << slot) != 0;
                        let bits = if raw {
                            bits
                        } else {
                            lookup(&objects, bits as u32)?.as_ptr() as u64
                        };
                        object.set_record_slot_bits(slot, bits, raw);
                    }
                }
                Shape::Primitive => {}
            }
        }

        for _ in 0..input.u32()? {
            let object = lookup(&objects, input.u32()?)?;
            let count = input.u32()?;
            *self.roots.precious.entry(object).or_insert(0) += count;
            self.new_roots.push(object);
        }
        for position in input.u32s()? {
            let object = lookup(&objects, position)?;
            self.add_global_root(object);
        }
        let roots = input
            .u32s()?
            .into_iter()
            .map(|position| lookup(&objects, position))
            .collect::<Result<Vec<_>, _>>()?;
        if !input.bytes.is_empty() {
            return Err(invalid("it has bytes past its end"));
        }
        for symbol in self.symbols.values() {
            symbol.reset_symbol_caches();
        }
        self.binding_epoch += 1;
        self.enclosure_epoch += 1;
        Ok(roots)
    }

    /// How an object reached by the writer comes to exist in the loading heap.
    unsafe fn origin(&self, object: Sexp) -> Result<Origin, ImageError> {
        if object.is_altrep() {
            return Ok(Origin::Fresh);
        }
        Ok(match object.kind() {
            SexpType::Char if object.has_gp(gp::CACHED) => Origin::String,
            SexpType::Symbol => {
                if self.symbols.get(&object.car()) != Some(&object) {
                    return Err(ImageError::Unsupported(
                        "an uninterned symbol is reachable".to_owned(),
                    ));
                }
                Origin::Symbol
            }
            SexpType::Binding
                if object.car().kind() == SexpType::Symbol && object.car().cdr() == object =>
            {
                Origin::BaseCell
            }
            _ => Origin::Fresh,
        })
    }

    /// Allocates an object of the allocation table's [`ORIGIN_FRESH`] kind, with its plain
    /// elements.
    unsafe fn allocate_from_image(
        &mut self,
        input: &mut Reader<'_>,
        relocation: &Relocation,
    ) -> Result<Sexp, ImageError> {
        let kind = kind_of(input.u8()?)?;
        let nil = self.known.nil;
        Ok(match kind.shape() {
            Shape::Vector => {
                let length = input.u64()? as usize;
                if kind == SexpType::Char {
                    let bits = input.u16()?;
                    let text = input.blob()?;
                    if text.len() != length {
                        return Err(invalid("a string's length differs from its text"));
                    }
                    let (text, bits) = relocation.apply(text, bits);
                    let object = self.new_char(&text, 0)?;
                    object.set_gp(bits);
                    return Ok(object);
                }
                let capacity = input.u64()? as usize;
                if capacity < length {
                    return Err(invalid("a vector is longer than its capacity"));
                }
                let size = object_size(kind, capacity).ok_or(HeapError::TooLong)?;
                let object =
                    self.allocate_uncollected(kind, size, crate::object::VECTOR_DATA_OFFSET)?;
                object.set_length_and_capacity(length as isize, capacity as isize);
                if holds_handles(kind) {
                    // Every element is stored by the contents pass; until then they must be
                    // valid handles.
                    std::slice::from_raw_parts_mut(object.data() as *mut Sexp, capacity).fill(nil);
                } else {
                    let element = kind.element_size().unwrap_or(0);
                    let data = input.blob()?;
                    if data.len() != length * element {
                        return Err(invalid("a vector's data has the wrong size"));
                    }
                    std::ptr::copy_nonoverlapping(data.as_ptr(), object.data(), data.len());
                    std::ptr::write_bytes(
                        object.data().add(data.len()),
                        0,
                        (capacity - length) * element,
                    );
                }
                object
            }
            Shape::Triple => {
                let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
                let object = self.allocate_uncollected(kind, size, size)?;
                object.set_car_unbarriered(nil);
                object.set_cdr_unbarriered(nil);
                object.set_tag_unbarriered(nil);
                object
            }
            Shape::Environment => self.alloc_environment_uncollected(nil, 0)?,
            Shape::ExternalPtr => {
                let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
                let object = self.allocate_uncollected(kind, size, size)?;
                object.set_external_tag_unbarriered(nil);
                object.set_external_protected_unbarriered(nil);
                object
            }
            Shape::WeakRef => {
                let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
                let object = self.allocate_uncollected(kind, size, size)?;
                let payload = object.weak_payload();
                payload.key.set(nil);
                payload.value.set(nil);
                payload.finalizer.set(nil);
                self.weak_refs.push(object);
                object
            }
            Shape::Record => {
                let layout = input.u32()?;
                let slot_count = input.u32()?;
                if self.layouts.get(layout).is_none() {
                    return Err(invalid("a record's layout is not registered"));
                }
                let size = object_size(kind, slot_count as usize).ok_or(HeapError::TooLong)?;
                let object = self.allocate_uncollected(kind, size, size)?;
                object.init_record_header(layout, slot_count);
                for slot in 0..slot_count as usize {
                    object.set_record_slot_bits(slot, nil.as_ptr() as u64, false);
                }
                object
            }
            Shape::Primitive => return Err(invalid("a primitive is not fixed")),
        })
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

/// The objects a collection traces from `object`, plus a weak reference's fields, which an
/// image keeps.
unsafe fn children(object: Sexp, nil: Sexp) -> smallvec::SmallVec<[Sexp; 4]> {
    let mut children = smallvec::SmallVec::new();
    children.push(object.attrib());
    if object.is_altrep() {
        children.extend([object.car(), object.cdr(), object.tag()]);
        return children;
    }
    match object.kind().shape() {
        Shape::Vector => {
            if holds_handles(object.kind()) && object.flags() & flag_bits::WEAK_PAIRS == 0 {
                children.extend((0..object.length() as usize).map(|index| object.element(index)));
            }
        }
        Shape::Triple => children.extend([object.car(), object.cdr(), object.tag()]),
        Shape::Environment => {
            children.push(object.enclosure());
            let frame = &*object.frame_ptr();
            children.extend(frame.symbols());
            children.extend(frame.cells());
        }
        Shape::ExternalPtr => {
            children.extend([object.external_tag(), object.external_protected()]);
        }
        Shape::WeakRef => {
            children.extend([
                object.weak_key(),
                object.weak_value(),
                object.weak_finalizer(),
            ]);
        }
        Shape::Record => {
            children.extend(
                (0..object.record_slot_count()).filter_map(|index| object.record_handle(index)),
            );
        }
        Shape::Primitive => {}
    }
    children.retain(|child| *child != nil);
    children
}

fn holds_handles(kind: SexpType) -> bool {
    matches!(
        kind,
        SexpType::String | SexpType::List | SexpType::Expression
    )
}

/// Whether the loader writes an object's header and references. Fixed objects that cannot
/// change keep the loading heap's contents.
unsafe fn restores_contents(object: Sexp, fixed: bool, known: &crate::WellKnown) -> bool {
    if !fixed {
        return object.kind() != SexpType::Char;
    }
    if object == known.unbound || object == known.missing_arg || object == known.empty_env {
        return false;
    }
    matches!(
        object.kind().shape(),
        Shape::Environment | Shape::ExternalPtr | Shape::WeakRef | Shape::Record
    ) || (object.kind().shape() == Shape::Triple && object.kind() != SexpType::Nil)
}

fn kind_of(code: u8) -> Result<SexpType, ImageError> {
    // A binding cell's code is internal, so the decoder native code reaches leaves it out.
    if code == SexpType::Binding as u8 {
        return Ok(SexpType::Binding);
    }
    SexpType::from_code(u32::from(code)).ok_or_else(|| invalid("an object has an unknown type"))
}

fn lookup(objects: &[Sexp], position: u32) -> Result<Sexp, ImageError> {
    objects
        .get(position as usize)
        .copied()
        .ok_or_else(|| invalid("a reference points past the objects loaded so far"))
}

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
    pub fn take(&mut self, count: usize) -> Result<&'a [u8], ImageError> {
        if self.bytes.len() < count {
            return Err(invalid("it ends early"));
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ImageError> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    pub fn u8(&mut self) -> Result<u8, ImageError> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, ImageError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32, ImageError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, ImageError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

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
        (0..count).map(|_| self.u32()).collect()
    }
}
