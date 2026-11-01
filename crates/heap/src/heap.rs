//! ry's R object model and garbage collector.
//!
//! The collector is non-moving, because native code keeps raw pointers into objects, and
//! generational with sticky mark bits: an object that survives a collection keeps its mark and is
//! old from then on, so a minor collection marks only young objects and sweeps only the young
//! list. A write barrier on every store of a handle into an object records old objects that come
//! to point at young ones. Small objects live in size-segregated pages; large ones are allocated
//! one by one.
//!
//! The roots are the `PROTECT` stack, the precious multiset behind `R_PreserveObject`, the handle
//! stack that keeps every object native code receives alive until its foreign call returns, the
//! registered globals, every interned symbol, and any [`RootSource`] an interpreter or JIT
//! registers for its frames.

// Every accessor dereferences a raw handle, so the whole crate is an FFI-style unsafe layer and an
// `unsafe` block inside each `unsafe fn` would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has the same contract, stated once on `object::Sexp`: each handle it
// takes must point at a live object of the kind the function expects.
#![allow(clippy::missing_safety_doc)]

pub mod arena;
pub mod env;
pub mod object;
pub mod records;
mod space;
pub mod strings;

use {
    crate::{
        env::Frame,
        object::{
            NA_LOGICAL, REFCOUNT_MAX, Sexp, SexpType, Shape, Slot, SymbolObject,
            VECTOR_DATA_OFFSET, flag_bits, gc_bits, gp, object_size, slot_for,
        },
        records::Layouts,
        space::{SIZE_CLASS_BY_WORDS, SLOT_SIZES, Space, Swept},
        strings::{Encoding, StringTable},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::{alloc::Layout, cell::Cell, ffi::c_void, fmt, ptr::NonNull, rc::Rc},
};

/// The longest vector R allows, `R_XLEN_T_MAX`.
pub const MAX_VECTOR_LENGTH: isize = 4_503_599_627_370_496;
/// GNU R's default `PROTECT` stack size, `R_PPSSIZE`.
pub const DEFAULT_PROTECT_LIMIT: usize = 50_000;

pub struct Heap {
    space: Space,
    large: FxHashSet<Sexp>,
    /// Large objects allocated since the last collection, which a minor collection sweeps.
    young_large: Vec<Sexp>,
    remembered: Vec<Sexp>,
    escaped: Vec<Sexp>,
    weak_refs: Vec<Sexp>,
    /// Weak references whose keys died, kept alive with their keys until their finalizers run.
    ready_to_finalize: Vec<Sexp>,
    mark_stack: Vec<Sexp>,
    strings: StringTable,
    symbols: FxHashMap<Sexp, Sexp>,
    /// Symbols interned and objects preserved since the last collection. A minor collection marks
    /// only these: every older entry of the symbol table and the precious set is already marked,
    /// and walking those tables touched one cold header per entry on every minor collection.
    new_roots: Vec<Sexp>,
    /// The tag and layout interners of ry's records, tuples and tags.
    pub layouts: Layouts,
    pub roots: Roots,
    sources: Vec<(u64, Rc<dyn RootSource>)>,
    next_source_id: u64,
    policy: GcPolicy,
    young_bytes: usize,
    old_bytes: usize,
    stats: GcStats,
    inhibit: u32,
    stress_counter: u64,
    known: WellKnown,
    /// Bumped whenever the set of bindings outside function frames changes (a binding created or
    /// removed, an enclosure replaced), so a cached lookup that resolved through those
    /// environments knows it is still valid when the epoch is unchanged.
    binding_epoch: u64,
    /// Set while a major collection marks, which is when bytecode inline caches are emptied.
    major_marking: bool,
    /// Bumped whenever an enclosure is replaced. A symbol's lookup stamp adds it to the symbol's
    /// own epoch, so a cache keyed by symbol sees both kinds of change.
    enclosure_epoch: u64,
}

/// A formal bound by [`Heap::define_formals`]. `missing` marks a formal the call did not supply,
/// for `missing()`.
#[derive(Clone, Copy)]
pub struct Formal {
    pub symbol: Sexp,
    pub value: Sexp,
    pub missing: bool,
}

/// The objects every R session has, created with the heap.
#[derive(Clone, Copy)]
pub struct WellKnown {
    pub nil: Sexp,
    pub unbound: Sexp,
    pub missing_arg: Sexp,
    pub na_string: Sexp,
    pub blank_string: Sexp,
    pub blank_scalar_string: Sexp,
    pub empty_env: Sexp,
    pub base_env: Sexp,
    pub base_namespace: Sexp,
    pub global_env: Sexp,
    pub namespace_registry: Sexp,
    pub true_value: Sexp,
    pub false_value: Sexp,
    pub logical_na_value: Sexp,
}

pub struct Roots {
    pub protect: ProtectStack,
    precious: FxHashMap<Sexp, u32>,
    /// Objects kept alive for the rest of the current scope. A foreign call truncates it on
    /// return, which gives native code its handle scope.
    handles: Vec<Sexp>,
    globals: Vec<Sexp>,
}

/// The `PROTECT` stack. `base` is where the innermost native call's protections start, so
/// native code cannot unprotect what its caller protected.
pub struct ProtectStack {
    stack: Vec<Sexp>,
    pub limit: usize,
    base: usize,
}

/// Something that holds handles the collector cannot see otherwise, such as an interpreter's
/// frame stack or a JIT's stack maps.
pub trait RootSource {
    fn trace(&self, marker: &mut Marker<'_>);
}

pub struct Marker<'a> {
    stack: &'a mut Vec<Sexp>,
    space: &'a mut Space,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Collection {
    Minor,
    Major,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GcStress {
    Off,
    /// Collect before every allocation, every eighth collection a major one.
    EveryAllocation,
}

/// When `RY_GC_STRESS` turns collection stress on for a whole run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GcStressStart {
    /// `RY_GC_STRESS=startup`: from the heap's first allocation, base R's loading included.
    Startup,
    /// Any other non-empty value but `0`: once the session has started, so a script runs under
    /// stress without paying for base R's loading under it. The host that runs the script
    /// switches it on.
    Script,
}

/// The collection stress `RY_GC_STRESS` asks for, if any.
pub fn gc_stress_requested() -> Option<GcStressStart> {
    match std::env::var("RY_GC_STRESS").as_deref() {
        Err(_) | Ok("" | "0") => None,
        Ok("startup") => Some(GcStressStart::Startup),
        Ok(_) => Some(GcStressStart::Script),
    }
}

pub struct GcPolicy {
    /// Bytes of young allocation that trigger a minor collection while the old generation is
    /// small; past twice this size, half the old generation's bytes trigger one instead.
    pub young_limit: usize,
    /// Old-generation bytes that trigger a major collection after a minor one.
    pub old_limit: usize,
    pub min_old_limit: usize,
    pub stress: GcStress,
    /// Checks the write barrier's invariant before every minor collection (`RY_GC_VERIFY`).
    pub verify: bool,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GcStats {
    pub minor_collections: u64,
    pub major_collections: u64,
    pub objects_freed: u64,
    pub live_objects: u64,
    pub live_bytes: u64,
    pub pages: u64,
    pub large_objects: u64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HeapError {
    CannotAllocate { bytes: usize },
    NegativeLength,
    TooLong,
    InvalidVectorType { kind: SexpType, length: isize },
    ProtectOverflow,
    UnprotectUnderflow { available: usize, requested: usize },
    UnprotectPointerNotFound,
    ReprotectIndex { available: usize, index: usize },
    EmbeddedNul { text: String },
    StringTooLong,
    SymbolTooLong,
    EmptySymbol,
    EmptyEnvironment,
    LockedBinding { name: String },
    LockedEnvironment,
    LockedEnvironmentRemove,
    NotResizable,
    ResizeBeyondCapacity { capacity: isize, length: isize },
    IncompatibleRetype { from: SexpType, to: SexpType },
}

impl fmt::Display for HeapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeapError::CannotAllocate { bytes } => {
                let bytes = *bytes as f64;
                let (amount, unit) = if bytes >= 1024.0 * 1024.0 * 1024.0 {
                    (bytes / (1024.0 * 1024.0 * 1024.0), "Gb")
                } else if bytes >= 1024.0 * 1024.0 {
                    (bytes / (1024.0 * 1024.0), "Mb")
                } else {
                    (bytes / 1024.0, "Kb")
                };
                write!(
                    formatter,
                    "cannot allocate vector of size {amount:.1} {unit}"
                )
            }
            HeapError::NegativeLength => {
                write!(formatter, "negative length vectors are not allowed")
            }
            HeapError::TooLong => write!(formatter, "vector is too large"),
            HeapError::InvalidVectorType { kind, length } => write!(
                formatter,
                "invalid type/length ({}/{length}) in vector allocation",
                kind.name()
            ),
            HeapError::ProtectOverflow => write!(formatter, "protect(): protection stack overflow"),
            HeapError::UnprotectUnderflow {
                available,
                requested,
            } => write!(
                formatter,
                "unprotect(): only {available} protected items, can't unprotect {requested}"
            ),
            HeapError::UnprotectPointerNotFound => {
                write!(formatter, "unprotect_ptr: pointer not found")
            }
            HeapError::ReprotectIndex { available, index } => write!(
                formatter,
                "R_Reprotect: only {available} protected items, can't reprotect index {index}"
            ),
            HeapError::EmbeddedNul { text } => {
                write!(formatter, "embedded nul in string: '{text}'")
            }
            HeapError::StringTooLong => {
                write!(formatter, "R character strings are limited to 2^31-1 bytes")
            }
            HeapError::SymbolTooLong => {
                write!(formatter, "variable names are limited to 10000 bytes")
            }
            HeapError::EmptySymbol => write!(formatter, "attempt to use zero-length variable name"),
            HeapError::EmptyEnvironment => {
                write!(formatter, "cannot assign values in the empty environment")
            }
            HeapError::LockedBinding { name } => {
                write!(
                    formatter,
                    "cannot change value of locked binding for '{name}'"
                )
            }
            HeapError::LockedEnvironment => {
                write!(formatter, "cannot add bindings to a locked environment")
            }
            HeapError::LockedEnvironmentRemove => {
                write!(
                    formatter,
                    "cannot remove bindings from a locked environment"
                )
            }
            HeapError::NotResizable => write!(formatter, "the vector is not resizable"),
            HeapError::IncompatibleRetype { from, to } => write!(
                formatter,
                "cannot change the type of a '{}' object to '{}'",
                from.name(),
                to.name()
            ),
            HeapError::ResizeBeyondCapacity { capacity, length } => write!(
                formatter,
                "a resizable vector of capacity {capacity} cannot grow to length {length}"
            ),
        }
    }
}

impl std::error::Error for HeapError {}

pub type HeapResult<T> = Result<T, HeapError>;

const LARGE_CLASS: u8 = u8::MAX;

impl Heap {
    pub fn new() -> Heap {
        Heap::with_policy(GcPolicy::default())
    }

    pub fn with_policy(policy: GcPolicy) -> Heap {
        let mut heap = Heap {
            space: Space::new(),
            large: FxHashSet::default(),
            young_large: Vec::new(),
            remembered: Vec::new(),
            escaped: Vec::new(),
            weak_refs: Vec::new(),
            ready_to_finalize: Vec::new(),
            mark_stack: Vec::new(),
            binding_epoch: 0,
            major_marking: false,
            enclosure_epoch: 0,
            strings: StringTable::default(),
            symbols: FxHashMap::default(),
            new_roots: Vec::new(),
            layouts: Layouts::default(),
            roots: Roots {
                protect: ProtectStack {
                    stack: Vec::new(),
                    limit: DEFAULT_PROTECT_LIMIT,
                    base: 0,
                },
                precious: FxHashMap::default(),
                handles: Vec::new(),
                globals: Vec::new(),
            },
            sources: Vec::new(),
            next_source_id: 0,
            policy,
            young_bytes: 0,
            old_bytes: 0,
            stats: GcStats::default(),
            inhibit: 1,
            stress_counter: 0,
            // Overwritten below, before anything reads it.
            known: WellKnown::placeholder(),
        };
        // SAFETY: the heap is fresh, and collection stays inhibited until every well-known object
        // is a registered root.
        unsafe { heap.create_well_known() };
        heap.inhibit = 0;
        heap
    }

    pub fn known(&self) -> &WellKnown {
        &self.known
    }

    pub fn nil(&self) -> Sexp {
        self.known.nil
    }

    pub fn stats(&self) -> GcStats {
        self.stats
    }

    pub fn policy_mut(&mut self) -> &mut GcPolicy {
        &mut self.policy
    }

    /// Suspends collection until the matching [`Heap::allow_collection`], for code that builds
    /// an object graph it cannot root piecewise.
    pub fn inhibit_collection(&mut self) {
        self.inhibit += 1;
    }

    pub fn allow_collection(&mut self) {
        self.inhibit = self.inhibit.saturating_sub(1);
    }

    // Allocation.

    /// A vector of `length` elements. Atomic elements start zeroed, character elements as `""`
    /// and list elements as `NULL`, as in GNU R.
    /// A vector of plain numbers whose elements are not initialized, for a kernel that writes
    /// every element before anything reads the vector. Skipping the zeroing matters for the
    /// large results of whole-vector arithmetic.
    ///
    /// # Safety
    /// The caller must write every element before the vector is read.
    pub unsafe fn alloc_vector_uninit(
        &mut self,
        kind: SexpType,
        length: usize,
    ) -> HeapResult<Sexp> {
        if !matches!(
            kind,
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::Raw
        ) {
            return self.alloc_vector(kind, length as isize);
        }
        if length > MAX_VECTOR_LENGTH as usize {
            return Err(HeapError::TooLong);
        }
        let size = object_size(kind, length).ok_or(HeapError::TooLong)?;
        let object = self.allocate_with(kind, size, &[], VECTOR_DATA_OFFSET)?;
        object.set_length_and_capacity(length as isize, length as isize);
        Ok(object)
    }

    pub unsafe fn alloc_vector(&mut self, kind: SexpType, length: isize) -> HeapResult<Sexp> {
        if !kind.is_vector() {
            return Err(HeapError::InvalidVectorType { kind, length });
        }
        if length < 0 {
            return Err(HeapError::NegativeLength);
        }
        if length > MAX_VECTOR_LENGTH {
            return Err(HeapError::TooLong);
        }
        let size = object_size(kind, length as usize).ok_or(HeapError::TooLong)?;
        // Vectors of handles are filled below, so only the header needs zeroing first.
        let zeroed = if matches!(
            kind,
            SexpType::String | SexpType::List | SexpType::Expression
        ) {
            VECTOR_DATA_OFFSET
        } else {
            size
        };
        let object = self.allocate_with(kind, size, &[], zeroed)?;
        object.set_length_and_capacity(length, length);
        match kind {
            SexpType::String => {
                let blank = self.known.blank_string;
                object.slice_mut::<Sexp>().fill(blank);
            }
            SexpType::List | SexpType::Expression => {
                let nil = self.known.nil;
                object.slice_mut::<Sexp>().fill(nil);
            }
            _ => {}
        }
        Ok(object)
    }

    /// `R_allocResizableVector`: a vector that `resize_vector` can shrink and regrow up to
    /// `capacity` without copying.
    pub unsafe fn alloc_resizable_vector(
        &mut self,
        kind: SexpType,
        capacity: isize,
    ) -> HeapResult<Sexp> {
        let object = self.alloc_vector(kind, capacity)?;
        object.set_flag(flag_bits::RESIZABLE, true);
        Ok(object)
    }

    /// `R_resizeVector`. Growing exposes elements initialized to the vector's empty value.
    pub unsafe fn resize_vector(&mut self, vector: Sexp, length: isize) -> HeapResult<()> {
        if vector.flags() & flag_bits::RESIZABLE == 0 {
            return Err(HeapError::NotResizable);
        }
        let capacity = vector.capacity();
        if length < 0 {
            return Err(HeapError::NegativeLength);
        }
        if length > capacity {
            return Err(HeapError::ResizeBeyondCapacity { capacity, length });
        }
        let old = vector.length();
        if length > old {
            let element = vector.kind().element_size().unwrap_or(0);
            match vector.kind() {
                SexpType::String | SexpType::List | SexpType::Expression => {
                    let fill = if vector.kind() == SexpType::String {
                        self.known.blank_string
                    } else {
                        self.known.nil
                    };
                    for index in old..length {
                        vector.set_element_unbarriered(index as usize, fill);
                    }
                }
                _ => std::ptr::write_bytes(
                    vector.data().add(old as usize * element),
                    0,
                    (length - old) as usize * element,
                ),
            }
        } else if matches!(
            vector.kind(),
            SexpType::String | SexpType::List | SexpType::Expression
        ) {
            for index in length..old {
                let dropped = vector.element(index as usize);
                dropped.decrement_refcount();
                vector.set_element_unbarriered(index as usize, self.known.nil);
            }
        }
        vector.set_length(length);
        Ok(())
    }

    /// Lets `resize_vector` change the length of a vector allocated with a fixed length, within
    /// the length it was allocated with. GNU R before 4.6 let native code shrink any vector this
    /// way (`SETLENGTH`), and binaries built then still do.
    pub unsafe fn mark_resizable(&mut self, vector: Sexp) {
        vector.set_flag(flag_bits::RESIZABLE, true);
    }

    /// Changes an object's type to another of the same shape (`SET_TYPEOF`): pairlist, call and
    /// dots cells among themselves, integer and logical vectors, lists and expression vectors.
    pub unsafe fn retype(&mut self, object: Sexp, kind: SexpType) -> HeapResult<()> {
        use SexpType::*;
        let compatible = |a: SexpType, b: SexpType| {
            matches!(
                (a, b),
                (Pairlist | Language | Dots, Pairlist | Language | Dots)
                    | (Integer | Logical, Integer | Logical)
                    | (List | Expression, List | Expression)
            )
        };
        if object.kind() == kind {
            return Ok(());
        }
        if object.is_altrep() || !compatible(object.kind(), kind) {
            return Err(HeapError::IncompatibleRetype {
                from: object.kind(),
                to: kind,
            });
        }
        object.header().kind.set(kind as u8);
        Ok(())
    }

    pub unsafe fn is_resizable(&self, vector: Sexp) -> bool {
        vector.flags() & flag_bits::RESIZABLE != 0
    }

    /// A triple-shaped object: a pairlist cell, call, closure, promise or similar. The three
    /// handles stay rooted across the allocation.
    pub unsafe fn alloc_triple(
        &mut self,
        kind: SexpType,
        first: Sexp,
        second: Sexp,
        third: Sexp,
    ) -> HeapResult<Sexp> {
        self.alloc_triple_keeping(kind, first, second, third, None)
    }

    /// `alloc_triple` that also keeps `extra` alive across the allocation.
    unsafe fn alloc_triple_keeping(
        &mut self,
        kind: SexpType,
        first: Sexp,
        second: Sexp,
        third: Sexp,
        extra: Option<Sexp>,
    ) -> HeapResult<Sexp> {
        let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
        let keep = [first, second, third, extra.unwrap_or(first)];
        let object = self.allocate_with(kind, size, &keep, 0)?;
        object.set_car_unbarriered(first);
        object.set_cdr_unbarriered(second);
        object.set_tag_unbarriered(third);
        first.increment_refcount();
        second.increment_refcount();
        third.increment_refcount();
        if kind == SexpType::Symbol {
            let symbol = object.as_ptr().cast::<SymbolObject>();
            std::ptr::addr_of_mut!((*symbol).epoch).write(Cell::new(0));
            std::ptr::addr_of_mut!((*symbol).global_cell).write(Cell::new(None));
            std::ptr::addr_of_mut!((*symbol).global_stamp).write(Cell::new(u64::MAX));
        }
        Ok(object)
    }

    pub unsafe fn cons(&mut self, car: Sexp, cdr: Sexp) -> HeapResult<Sexp> {
        self.alloc_triple(SexpType::Pairlist, car, cdr, self.known.nil)
    }

    /// An ALTREP instance of `class` with the given vector type. It has the triple shape, with
    /// the data slots first and the class last, as GNU R stores them.
    pub unsafe fn alloc_altrep(
        &mut self,
        kind: SexpType,
        class: Sexp,
        data1: Sexp,
        data2: Sexp,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Pairlist, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(kind, size, &[class, data1, data2])?;
        object.set_altrep_flag();
        object.set_car_unbarriered(data1);
        object.set_cdr_unbarriered(data2);
        object.set_tag_unbarriered(class);
        data1.increment_refcount();
        data2.increment_refcount();
        Ok(object)
    }

    pub unsafe fn alloc_environment(&mut self, enclosure: Sexp) -> HeapResult<Sexp> {
        self.alloc_environment_with_capacity(enclosure, 0)
    }

    /// An environment whose frame has room for `bindings` bindings before it grows.
    pub unsafe fn alloc_environment_with_capacity(
        &mut self,
        enclosure: Sexp,
        bindings: usize,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Environment, 0).ok_or(HeapError::TooLong)?;
        // Every field is written below, so zeroing the slot first would store it twice.
        let object = self.allocate_with(SexpType::Environment, size, &[enclosure], 0)?;
        std::ptr::write(object.frame_ptr(), Frame::with_capacity(bindings));
        self.space.set_finalizable(object);
        object.set_enclosure_unbarriered(enclosure);
        Ok(object)
    }

    pub unsafe fn alloc_external_ptr(
        &mut self,
        address: *mut c_void,
        tag: Sexp,
        protected: Sexp,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::ExternalPtr, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(SexpType::ExternalPtr, size, &[tag, protected])?;
        object.set_external_address(address);
        object.set_external_tag_unbarriered(tag);
        object.set_external_protected_unbarriered(protected);
        tag.increment_refcount();
        protected.increment_refcount();
        Ok(object)
    }

    /// A weak reference. `c_finalizer` is a C function address, or zero.
    pub unsafe fn alloc_weak_ref(
        &mut self,
        key: Sexp,
        value: Sexp,
        finalizer: Sexp,
        c_finalizer: usize,
        finalize_on_exit: bool,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::WeakRef, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(SexpType::WeakRef, size, &[key, value, finalizer])?;
        let payload = object.weak_payload();
        payload.key.set(key);
        payload.value.set(value);
        payload.finalizer.set(finalizer);
        payload.c_finalizer.set(c_finalizer);
        payload.finalize_on_exit.set(finalize_on_exit);
        self.weak_refs.push(object);
        Ok(object)
    }

    pub unsafe fn alloc_primitive(&mut self, kind: SexpType, index: u32) -> HeapResult<Sexp> {
        let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(kind, size, &[])?;
        object.set_primitive_index(index);
        Ok(object)
    }

    /// A record, tuple or tag of the interned `layout`, whose slots hold `values` in layout order.
    /// Each value is stored raw or as a handle as [`slot_for`] decides, and stays rooted across
    /// the allocation.
    pub unsafe fn alloc_record(&mut self, layout: u32, values: &[Sexp]) -> HeapResult<Sexp> {
        let object = self.alloc_counted_record(layout, values)?;
        // Immutable: a modification always builds a new value.
        object.mark_not_mutable();
        Ok(object)
    }

    /// A record whose reference count tracks its holders, so a holder that owns its only
    /// reference may update it in place through [`Heap::set_record_slot`]. Dictionaries and sets
    /// use it.
    pub unsafe fn alloc_counted_record(
        &mut self,
        layout: u32,
        values: &[Sexp],
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Record, values.len()).ok_or(HeapError::TooLong)?;
        if values.len() > u32::MAX as usize {
            return Err(HeapError::TooLong);
        }
        let object = self.allocate(SexpType::Record, size, values)?;
        let slots: smallvec::SmallVec<[Slot; 4]> = values
            .iter()
            .enumerate()
            .map(|(index, &value)| slot_for(value, index))
            .collect();
        for slot in &slots {
            if let Slot::Handle(handle) = slot {
                handle.increment_refcount();
            }
        }
        object.init_record(layout, &slots);
        Ok(object)
    }

    /// Replaces slot `index` of `record`, storing `value` raw or as a handle as [`slot_for`]
    /// decides.
    pub unsafe fn set_record_slot(&mut self, record: Sexp, index: usize, value: Sexp) {
        let slot = slot_for(value, index);
        let old = record.record_handle(index);
        if let Slot::Handle(handle) = slot {
            handle.increment_refcount();
            self.write_barrier(record, handle);
        }
        if let Some(old) = old {
            old.decrement_refcount();
        }
        record.set_record_slot_unbarriered(index, slot);
    }

    /// The interned `CHARSXP` for `bytes` in `encoding`, which `Rf_mkCharLenCE` returns. ASCII
    /// text is always stored unmarked, so the same ASCII bytes give the same object in every
    /// encoding.
    pub unsafe fn mk_char(&mut self, bytes: &[u8], encoding: Encoding) -> HeapResult<Sexp> {
        if bytes.len() > i32::MAX as usize {
            return Err(HeapError::StringTooLong);
        }
        if bytes.contains(&0) {
            let text = String::from_utf8_lossy(bytes).replace('\0', "\\0");
            return Err(HeapError::EmbeddedNul { text });
        }
        let ascii = bytes.is_ascii();
        let bits = if ascii { gp::ASCII } else { encoding.gp_bits() };
        let hash = strings::hash(bytes, bits & gp::ENCODING);
        if let Some(found) = self.strings.find(hash, bytes, bits & gp::ENCODING) {
            return Ok(found);
        }
        let object = self.new_char(bytes, bits)?;
        object.set_capacity((u64::from(hash) << 32) as isize);
        object.set_gp_bits(gp::CACHED, true);
        self.strings.insert(hash, object);
        Ok(object)
    }

    unsafe fn new_char(&mut self, bytes: &[u8], bits: u16) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Char, bytes.len()).ok_or(HeapError::StringTooLong)?;
        let object = self.allocate(SexpType::Char, size, &[])?;
        object.set_length_and_capacity(bytes.len() as isize, 0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), object.data(), bytes.len());
        object.set_gp(bits);
        Ok(object)
    }

    /// The interned symbol named `name`, which `Rf_install` returns.
    pub unsafe fn install(&mut self, name: &[u8]) -> HeapResult<Sexp> {
        if name.len() > 10_000 {
            return Err(HeapError::SymbolTooLong);
        }
        if name.is_empty() {
            return Err(HeapError::EmptySymbol);
        }
        let name = self.mk_char(name, Encoding::Native)?;
        self.install_char(name)
    }

    /// The interned symbol whose print name is the `CHARSXP` `name`.
    pub unsafe fn install_char(&mut self, name: Sexp) -> HeapResult<Sexp> {
        if let Some(&symbol) = self.symbols.get(&name) {
            return Ok(symbol);
        }
        let nil = self.known.nil;
        let symbol = self.alloc_triple(SexpType::Symbol, name, nil, nil)?;
        symbol.mark_not_mutable();
        self.symbols.insert(name, symbol);
        self.new_roots.push(symbol);
        Ok(symbol)
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn string_count(&self) -> usize {
        self.strings.len()
    }

    // Stores, each through the write barrier and the reference counts.

    #[inline]
    pub unsafe fn write_barrier(&mut self, parent: Sexp, child: Sexp) {
        if parent.gc_bits() & gc_bits::REMEMBERED == 0
            && self.space.is_marked(parent)
            && !self.space.is_marked(child)
        {
            parent.set_gc_bit(gc_bits::REMEMBERED, true);
            self.remembered.push(parent);
        }
    }

    #[inline]
    unsafe fn store(&mut self, parent: Sexp, old: Sexp, new: Sexp) {
        if parent.flags() & flag_bits::UNTRACKED == 0 {
            new.increment_refcount();
            old.decrement_refcount();
        }
        self.write_barrier(parent, new);
    }

    /// Stops a cons-like object's fields from counting as references to what they hold.
    pub unsafe fn untrack_references(&mut self, object: Sexp) {
        if object.flags() & flag_bits::UNTRACKED != 0 || object.kind().shape() != Shape::Triple {
            return;
        }
        object.car().decrement_refcount();
        object.cdr().decrement_refcount();
        object.tag().decrement_refcount();
        object.set_flag(flag_bits::UNTRACKED, true);
    }

    pub unsafe fn set_car(&mut self, cell: Sexp, value: Sexp) {
        self.store(cell, cell.car(), value);
        cell.set_car_unbarriered(value);
    }

    pub unsafe fn set_cdr(&mut self, cell: Sexp, value: Sexp) {
        self.store(cell, cell.cdr(), value);
        cell.set_cdr_unbarriered(value);
    }

    pub unsafe fn set_tag(&mut self, cell: Sexp, value: Sexp) {
        self.store(cell, cell.tag(), value);
        cell.set_tag_unbarriered(value);
    }

    pub unsafe fn set_element(&mut self, vector: Sexp, index: usize, value: Sexp) {
        if vector.is_altrep()
            && let Some(elements) = crate::object::altrep_elements()
        {
            return (elements.write)(vector, index, value);
        }
        self.store(vector, vector.element(index), value);
        vector.set_element_unbarriered(index, value);
    }

    /// Giving an Array any attribute turns it back into a plain R vector, because an Array is
    /// attribute-free by definition.
    pub unsafe fn set_attrib(&mut self, object: Sexp, value: Sexp) {
        self.store(object, object.attrib(), value);
        object.set_attrib_unbarriered(value);
        if value != self.known.nil && object.is_array() {
            object.set_array(false);
        }
    }

    pub unsafe fn set_enclosure(&mut self, env: Sexp, value: Sexp) {
        self.binding_epoch += 1;
        self.enclosure_epoch += 1;
        self.write_barrier(env, value);
        env.set_enclosure_unbarriered(value);
    }

    pub unsafe fn set_external_tag(&mut self, pointer: Sexp, value: Sexp) {
        self.store(pointer, pointer.external_tag(), value);
        pointer.set_external_tag_unbarriered(value);
    }

    pub unsafe fn set_external_protected(&mut self, pointer: Sexp, value: Sexp) {
        self.store(pointer, pointer.external_protected(), value);
        pointer.set_external_protected_unbarriered(value);
    }

    /// Records that native code received a writable pointer to `vector`'s handle elements, so
    /// its stores bypass the barrier and every minor collection must scan it.
    pub unsafe fn escape(&mut self, vector: Sexp) {
        if vector.gc_bits() & gc_bits::ESCAPED == 0 {
            vector.set_gc_bit(gc_bits::ESCAPED, true);
            self.escaped.push(vector);
        }
    }

    /// Ends every escape once the outermost native call returns. Native code may store through
    /// a writable element pointer only while the call that took it runs (GNU R's collector never
    /// sees those stores at all), so each escaped vector that is old is remembered instead: the
    /// next minor collection rescans it once, catching the stores made meanwhile, and from then
    /// on it is an ordinary object whose stores go through the barrier. Without this a column a
    /// package touched once would be rescanned by every minor collection for the rest of the
    /// session.
    pub unsafe fn retire_escaped(&mut self) {
        let mut escaped = std::mem::take(&mut self.escaped);
        for vector in escaped.drain(..) {
            vector.set_gc_bit(gc_bits::ESCAPED, false);
            if vector.gc_bits() & gc_bits::REMEMBERED == 0 && self.space.is_marked(vector) {
                vector.set_gc_bit(gc_bits::REMEMBERED, true);
                self.remembered.push(vector);
            }
        }
        self.escaped = escaped;
    }

    // Environments.

    fn is_symbol_frame(&self, env: Sexp) -> bool {
        env == self.known.base_env || env == self.known.base_namespace
    }

    /// The binding cell for `symbol` in `env`'s own frame.
    pub unsafe fn frame_cell(&self, env: Sexp, symbol: Sexp) -> Option<Sexp> {
        if self.is_symbol_frame(env) {
            let cell = symbol.cdr();
            return (cell.kind() == SexpType::Binding).then_some(cell);
        }
        if env == self.known.empty_env {
            return None;
        }
        (*env.frame_ptr()).find(symbol)
    }

    /// `Rf_defineVar`: binds `symbol` in `env`'s own frame.
    pub unsafe fn define(&mut self, env: Sexp, symbol: Sexp, value: Sexp) -> HeapResult<()> {
        if let Some(cell) = self.frame_cell(env, symbol) {
            self.set_binding_value(cell, value)?;
            // A formal that was missing stops being missing once it is assigned, as in GNU R.
            cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
            return Ok(());
        }
        if env == self.known.empty_env {
            return Err(HeapError::EmptyEnvironment);
        }
        if env.has_gp(gp::FRAME_LOCKED) {
            return Err(HeapError::LockedEnvironment);
        }
        let cell =
            self.alloc_triple_keeping(SexpType::Binding, symbol, value, self.known.nil, Some(env))?;
        if !env.has_gp(gp::FUNCTION_FRAME) || symbol.has_gp(gp::WATCHED) {
            self.binding_epoch += 1;
            bump_symbol_epoch(symbol);
        }
        if self.is_symbol_frame(env) {
            self.store(symbol, symbol.cdr(), cell);
            symbol.set_cdr_unbarriered(cell);
        } else {
            self.write_barrier(env, cell);
            (*env.frame_ptr()).insert(symbol, cell);
        }
        Ok(())
    }

    /// Binds each of `bindings` in `env`, a new function frame, as a closure call binds its
    /// matched formals. The cells are allocated as one batch, with a single collection check
    /// that keeps every value alive, so the values need no other root.
    pub unsafe fn define_formals(&mut self, env: Sexp, bindings: &[Formal]) -> HeapResult<()> {
        debug_assert!(env.has_gp(gp::FUNCTION_FRAME));
        let size = object_size(SexpType::Binding, 0).ok_or(HeapError::TooLong)?;
        let request = size * bindings.len();
        if self.collection_due(request) {
            let keep: SmallVec<[Sexp; 16]> = bindings
                .iter()
                .map(|binding| binding.value)
                .chain([env])
                .collect();
            self.maybe_collect(request, &keep);
        }
        let nil = self.known.nil;
        let frame = &mut *env.frame_ptr();
        let mut first = true;
        for binding in bindings {
            let cell = self.allocate_uncollected(SexpType::Binding, size, 0)?;
            cell.set_car_unbarriered(binding.symbol);
            cell.set_cdr_unbarriered(binding.value);
            cell.set_tag_unbarriered(nil);
            binding.symbol.increment_refcount();
            binding.value.increment_refcount();
            nil.increment_refcount();
            if binding.missing {
                cell.set_gp_bits(gp::BINDING_MISSING, true);
            }
            frame.insert(binding.symbol, cell);
            // A collection above may have made `env` old, and the cells are all young, so the
            // first cell's barrier check covers every one, including when a later allocation
            // fails and leaves the cells before it in the frame.
            if first {
                self.write_barrier(env, cell);
                first = false;
            }
        }
        Ok(())
    }

    pub unsafe fn set_binding_value(&mut self, cell: Sexp, value: Sexp) -> HeapResult<()> {
        if cell.has_gp(gp::BINDING_LOCKED) {
            return Err(HeapError::LockedBinding {
                name: String::from_utf8_lossy(cell.car().car().bytes()).into_owned(),
            });
        }
        if cell.has_gp(gp::WATCHED) {
            self.binding_epoch += 1;
        }
        self.store(cell, cell.cdr(), value);
        cell.set_cdr_unbarriered(value);
        Ok(())
    }

    /// Removes `symbol` from `env`'s own frame and reports whether it was bound.
    pub unsafe fn remove_binding(&mut self, env: Sexp, symbol: Sexp) -> HeapResult<bool> {
        if env.has_gp(gp::FRAME_LOCKED) {
            return Err(HeapError::LockedEnvironmentRemove);
        }
        self.binding_epoch += 1;
        bump_symbol_epoch(symbol);
        if self.is_symbol_frame(env) {
            let cell = symbol.cdr();
            if cell.kind() != SexpType::Binding {
                return Ok(false);
            }
            self.store(symbol, cell, self.known.nil);
            symbol.set_cdr_unbarriered(self.known.nil);
            self.clear_removed_cell(cell);
            return Ok(true);
        }
        if env == self.known.empty_env {
            return Ok(false);
        }
        let Some(cell) = (*env.frame_ptr()).remove(symbol) else {
            return Ok(false);
        };
        self.clear_removed_cell(cell);
        Ok(true)
    }

    /// An inline cache can still hold a removed cell until the code next runs, and the value
    /// must not stay reachable through it: `rm(x); gc()` has to let a finalizer on `x` run.
    unsafe fn clear_removed_cell(&mut self, cell: Sexp) {
        let unbound = self.known.unbound;
        self.store(cell, cell.cdr(), unbound);
        cell.set_cdr_unbarriered(unbound);
    }

    /// A stamp for lookups of `symbol` through environments other than function frames: it
    /// changes whenever a binding of `symbol` appears or disappears in such an environment, or
    /// any enclosure is replaced, so a cell cached under an unchanged stamp is still the one a
    /// fresh walk would find.
    pub unsafe fn lookup_stamp(&self, symbol: Sexp) -> u64 {
        symbol.symbol_payload().epoch.get() + self.enclosure_epoch
    }

    /// The first binding of `symbol` on the search path, from the global environment to the
    /// base environment. GNU R keeps the same cache, because every free variable of top-level
    /// code and of functions defined there resolves along the search path, which in a session
    /// with many attached packages means many large frames to probe.
    ///
    /// The flag says whether the cell may be cached under the symbol's stamp: it may not when
    /// the walk passed a function frame, whose bindings come and go without moving the stamp.
    pub unsafe fn global_cell(&self, symbol: Sexp) -> Option<(Sexp, bool)> {
        let payload = symbol.symbol_payload();
        let stamp = self.lookup_stamp(symbol);
        if payload.global_stamp.get() == stamp {
            return payload.global_cell.get().map(|cell| (cell, true));
        }
        let mut env = self.known.global_env;
        let mut cacheable = true;
        while env != self.known.empty_env {
            cacheable &= !env.has_gp(gp::FUNCTION_FRAME);
            if let Some(cell) = self.frame_cell(env, symbol) {
                if cacheable {
                    payload.global_cell.set(Some(cell));
                    payload.global_stamp.set(stamp);
                }
                return Some((cell, cacheable));
            }
            env = env.enclosure();
        }
        None
    }

    /// See the field: an unchanged epoch means no binding outside a function frame appeared or
    /// disappeared, and no enclosure changed.
    pub fn binding_epoch(&self) -> u64 {
        self.binding_epoch
    }

    /// The symbols `env` binds, in no particular order: cheaper than [`Heap::frame_symbols`]
    /// for a caller that sorts them or only needs the set.
    pub unsafe fn bound_symbols(&self, env: Sexp) -> Vec<Sexp> {
        if self.is_symbol_frame(env) || env == self.known.empty_env {
            return self.frame_symbols(env);
        }
        (*env.frame_ptr()).symbols().collect()
    }

    /// The symbols bound in `env`'s own frame, in the order GNU R lists them.
    pub unsafe fn frame_symbols(&self, env: Sexp) -> Vec<Sexp> {
        if self.is_symbol_frame(env) {
            return self
                .symbols
                .values()
                .copied()
                .filter(|symbol| symbol.cdr().kind() == SexpType::Binding)
                .collect();
        }
        if env == self.known.empty_env {
            return Vec::new();
        }
        (*env.frame_ptr())
            .listed_cells()
            .iter()
            .map(|cell| cell.car())
            .collect()
    }

    /// Makes `env`'s frame hashed, as `new.env(hash = TRUE, size = size)` creates it, which
    /// decides the order the frame is listed in.
    pub unsafe fn set_frame_hashed(&mut self, env: Sexp, size: i32) {
        if !self.is_symbol_frame(env) && env != self.known.empty_env {
            (*env.frame_ptr()).set_hashed(size);
        }
    }

    /// Marks the bindings of a closure frame made so far as its formals.
    pub unsafe fn mark_frame_formals(&mut self, env: Sexp) {
        (*env.frame_ptr()).mark_formals();
    }

    // Roots.

    /// Keeps `object` alive until the current handle scope ends.
    pub fn root(&mut self, object: Sexp) {
        self.roots.handles.push(object);
    }

    pub fn handle_mark(&self) -> usize {
        self.roots.handles.len()
    }

    pub fn release_handles(&mut self, mark: usize) {
        self.roots.handles.truncate(mark);
    }

    /// Keeps `object` alive for the rest of the heap's life.
    pub fn add_global_root(&mut self, object: Sexp) {
        self.roots.globals.push(object);
    }

    /// `R_PreserveObject`. Preserving twice needs two releases.
    pub fn preserve(&mut self, object: Sexp) {
        *self.roots.precious.entry(object).or_insert(0) += 1;
        // An old object keeps its mark, so a minor collection would skip it anyway. The caller
        // is preserving the object, so it is live.
        if !unsafe { self.space.is_marked(object) } {
            self.new_roots.push(object);
        }
    }

    /// `R_ReleaseObject`. Releasing an object that is not preserved does nothing, as in GNU R.
    pub fn release(&mut self, object: Sexp) {
        if let Some(count) = self.roots.precious.get_mut(&object) {
            *count -= 1;
            if *count == 0 {
                self.roots.precious.remove(&object);
            }
        }
    }

    pub fn add_root_source(&mut self, source: Rc<dyn RootSource>) -> u64 {
        let id = self.next_source_id;
        self.next_source_id += 1;
        self.sources.push((id, source));
        id
    }

    pub fn remove_root_source(&mut self, id: u64) {
        self.sources.retain(|(source_id, _)| *source_id != id);
    }

    /// Weak references whose keys have died and whose finalizers should now run. The caller
    /// runs them; each stays alive until it is taken.
    pub fn take_ready_finalizers(&mut self) -> Vec<Sexp> {
        std::mem::take(&mut self.ready_to_finalize)
    }

    /// Weak references marked to finalize at exit, for `R_RunExitFinalizers`.
    pub unsafe fn take_exit_finalizers(&mut self) -> Vec<Sexp> {
        let (exit, kept): (Vec<Sexp>, Vec<Sexp>) = self
            .weak_refs
            .iter()
            .partition(|weak| weak.weak_finalize_on_exit());
        self.weak_refs = kept;
        exit
    }

    /// Clears a weak reference after its finalizer ran or was cancelled.
    pub unsafe fn clear_weak_ref(&mut self, weak: Sexp) {
        let nil = self.known.nil;
        let payload = weak.weak_payload();
        payload.key.set(nil);
        payload.value.set(nil);
        payload.finalizer.set(nil);
        payload.c_finalizer.set(0);
    }

    // Collection.

    unsafe fn allocate(&mut self, kind: SexpType, size: usize, keep: &[Sexp]) -> HeapResult<Sexp> {
        self.allocate_with(kind, size, keep, size)
    }

    /// Allocates an object and zeroes its first `zeroed` bytes. The rest is left as it was,
    /// which only a vector of plain numbers that its caller fills in may rely on.
    unsafe fn allocate_with(
        &mut self,
        kind: SexpType,
        size: usize,
        keep: &[Sexp],
        zeroed: usize,
    ) -> HeapResult<Sexp> {
        self.maybe_collect(size, keep);
        self.allocate_uncollected(kind, size, zeroed)
    }

    /// [`Heap::allocate_with`] without the collection check, for a caller that ran it once for
    /// a batch of objects.
    unsafe fn allocate_uncollected(
        &mut self,
        kind: SexpType,
        size: usize,
        zeroed: usize,
    ) -> HeapResult<Sexp> {
        let class = SIZE_CLASS_BY_WORDS.get(size.div_ceil(8)).copied();
        let (memory, size_class) = match class {
            Some(class) => {
                let class = class as usize;
                let memory = self
                    .space
                    .allocate(class)
                    .ok_or(HeapError::CannotAllocate { bytes: size })?;
                // Every slot size is a multiple of 8 bytes, and zeroing word by word keeps the
                // common small object clear of a call into `memset`.
                let words = memory.as_ptr().cast::<u64>();
                for word in 0..SLOT_SIZES[class].min(zeroed.next_multiple_of(8)) / 8 {
                    words.add(word).write(0);
                }
                (memory, class as u8)
            }
            None => {
                let layout = Layout::from_size_align(size, 16)
                    .map_err(|_| HeapError::CannotAllocate { bytes: size })?;
                let memory = if zeroed >= size {
                    NonNull::new(std::alloc::alloc_zeroed(layout))
                } else {
                    NonNull::new(std::alloc::alloc(layout)).inspect(|memory| {
                        std::ptr::write_bytes(memory.as_ptr(), 0, zeroed);
                    })
                }
                .ok_or(HeapError::CannotAllocate { bytes: size })?;
                (memory, LARGE_CLASS)
            }
        };
        let object = Sexp::from_non_null(memory.cast());
        // The nil object is created first, pointing at itself.
        let nil = if self.inhibit > 0 && kind == SexpType::Nil {
            object
        } else {
            self.known.nil
        };
        object.init_header(kind, size_class, 0, nil);
        if size_class == LARGE_CLASS {
            self.large.insert(object);
            self.young_large.push(object);
        }
        self.young_bytes += size;
        Ok(object)
    }

    unsafe fn maybe_collect(&mut self, request: usize, keep: &[Sexp]) {
        if self.inhibit > 0 {
            return;
        }
        match self.policy.stress {
            GcStress::EveryAllocation => {
                self.stress_counter += 1;
                let kind = if self.stress_counter.is_multiple_of(8) {
                    Collection::Major
                } else {
                    Collection::Minor
                };
                self.collect_keeping(kind, keep);
            }
            GcStress::Off => {
                if self.collection_due(request) {
                    self.collect_keeping(Collection::Minor, keep);
                    if self.old_bytes > self.policy.old_limit {
                        self.collect_keeping(Collection::Major, keep);
                    }
                }
            }
        }
    }

    /// Whether allocating `request` more bytes runs a collection.
    fn collection_due(&self, request: usize) -> bool {
        if self.inhibit > 0 {
            return false;
        }
        if self.policy.stress == GcStress::EveryAllocation {
            return true;
        }
        // Like GNU R's trigger, the budget grows with the heap: a fixed one would collect on
        // every large vector a big session allocates, rescanning the young ones each time.
        let budget = self.policy.young_limit.max(self.old_bytes / 2);
        self.young_bytes + request > budget
    }

    /// Runs a collection now. Every live object must be reachable from a root.
    pub unsafe fn collect(&mut self, kind: Collection) {
        self.collect_keeping(kind, &[]);
    }

    unsafe fn collect_keeping(&mut self, kind: Collection, keep: &[Sexp]) {
        if self.inhibit > 0 {
            return;
        }
        self.inhibit += 1;
        self.major_marking = kind == Collection::Major;
        if kind == Collection::Major {
            self.clear_marks();
        } else if self.policy.verify {
            self.verify_barrier();
        }
        for &object in keep {
            self.mark(object);
        }
        self.mark_roots(kind);
        if kind == Collection::Minor {
            // Old objects are already marked, so their young children are pushed directly.
            for index in 0..self.remembered.len() {
                let parent = self.remembered[index];
                self.mark_stack.push(parent);
            }
            // An escaped character vector can only have come to hold CHARSXPs, which point at
            // nothing, so marking every young CHARSXP covers whatever native code stored into it
            // without rereading its elements, each a cache miss on another object's header. A
            // young CHARSXP that is in fact garbage survives until the next major collection.
            let mut escaped_strings = false;
            for index in 0..self.escaped.len() {
                let parent = self.escaped[index];
                if parent.kind() == SexpType::String
                    && !parent.is_altrep()
                    && self.space.is_marked(parent)
                {
                    escaped_strings = true;
                    self.mark(parent.attrib());
                } else {
                    self.mark_stack.push(parent);
                }
            }
            if escaped_strings {
                // Every CHARSXP but the rooted `NA_STRING` is interned, so the young ones are
                // the strings interned since the last collection.
                for index in 0..self.strings.young().len() {
                    let string = self.strings.young()[index];
                    self.mark(string);
                }
            }
        }
        self.drain();
        self.process_weak_refs();
        self.major_marking = false;
        // Before the sweep, while a mark still means "survives".
        let space = &self.space;
        self.escaped.retain(|&object| space.is_marked(object));
        match kind {
            Collection::Minor => {
                self.strings.remove_dead_young(&self.space);
                self.sweep_young();
                self.stats.minor_collections += 1;
            }
            Collection::Major => {
                self.strings.remove_dead(&self.space);
                self.sweep_all();
                self.stats.major_collections += 1;
                self.policy.old_limit = (self.old_bytes * 2).max(self.policy.min_old_limit);
            }
        }
        for parent in self.remembered.drain(..) {
            parent.set_gc_bit(gc_bits::REMEMBERED, false);
        }
        self.young_bytes = 0;
        self.inhibit -= 1;
    }

    #[inline]
    unsafe fn mark(&mut self, object: Sexp) {
        if self.space.mark(object) {
            self.mark_stack.push(object);
        }
    }

    unsafe fn mark_roots(&mut self, kind: Collection) {
        let known = self.known;
        for object in known.all() {
            self.mark(object);
        }
        for index in 0..self.roots.globals.len() {
            self.mark(self.roots.globals[index]);
        }
        for index in 0..self.roots.protect.stack.len() {
            self.mark(self.roots.protect.stack[index]);
        }
        for index in 0..self.roots.handles.len() {
            self.mark(self.roots.handles[index]);
        }
        // Marking only touches the mark stack, so the tables can be borrowed beside it instead of
        // copied out on every collection; a package with tens of thousands of symbols made that
        // copy a large part of each minor collection.
        let mark_stack = &mut self.mark_stack;
        let space = &mut self.space;
        let mut mark = |object: Sexp| {
            if space.mark(object) {
                mark_stack.push(object);
            }
        };
        match kind {
            Collection::Major => {
                self.roots.precious.keys().for_each(|&object| mark(object));
                self.symbols.values().for_each(|&symbol| mark(symbol));
            }
            Collection::Minor => self.new_roots.iter().for_each(|&object| mark(object)),
        }
        self.new_roots.clear();
        let sources: Vec<Rc<dyn RootSource>> = self
            .sources
            .iter()
            .map(|(_, source)| source.clone())
            .collect();
        for source in sources {
            let mut marker = Marker {
                stack: &mut self.mark_stack,
                space: &mut self.space,
            };
            source.trace(&mut marker);
        }
        // A weak reference lives until its key dies; only its key and value are weak.
        for index in 0..self.weak_refs.len() {
            self.mark(self.weak_refs[index]);
        }
        for index in 0..self.ready_to_finalize.len() {
            let weak = self.ready_to_finalize[index];
            self.mark(weak);
            self.mark(weak.weak_key());
            self.mark(weak.weak_value());
            self.mark(weak.weak_finalizer());
        }
    }

    unsafe fn drain(&mut self) {
        while let Some(object) = self.mark_stack.pop() {
            self.trace(object);
        }
    }

    unsafe fn trace(&mut self, object: Sexp) {
        self.mark(object.attrib());
        if object.is_altrep() {
            self.mark(object.car());
            self.mark(object.cdr());
            self.mark(object.tag());
            return;
        }
        match object.kind().shape() {
            Shape::Vector => {
                if matches!(
                    object.kind(),
                    SexpType::String | SexpType::List | SexpType::Expression
                ) {
                    for index in 0..object.length() as usize {
                        self.mark(object.element(index));
                    }
                }
            }
            Shape::Triple => {
                self.mark(object.car());
                self.mark(object.cdr());
                let tag = object.tag();
                // A bytecode object's tag holds its inline caches: the environment each lookup
                // started from and the binding cell it found. They are only a speed-up, but
                // traced they would keep an environment alive for as long as the code lives,
                // such as an R6 object whose methods a clone shares. A major collection empties
                // them instead, and the next lookup refills them.
                if self.major_marking
                    && object.kind() == SexpType::Bytecode
                    && tag.kind() == SexpType::List
                {
                    for index in 0..tag.length() as usize {
                        tag.element(index).decrement_refcount();
                        tag.set_element_unbarriered(index, self.known.nil);
                    }
                }
                self.mark(tag);
            }
            Shape::Environment => {
                self.mark(object.enclosure());
                for cell in (*object.frame_ptr()).cells() {
                    self.mark(cell);
                }
            }
            Shape::ExternalPtr => {
                self.mark(object.external_tag());
                self.mark(object.external_protected());
            }
            Shape::Record => {
                for index in 0..object.record_slot_count() {
                    if let Some(handle) = object.record_handle(index) {
                        self.mark(handle);
                    }
                }
            }
            // Weak references are traced by `process_weak_refs`.
            Shape::WeakRef | Shape::Primitive => {}
        }
    }

    /// Ephemeron semantics: a weak reference's value and finalizer are reachable only while its
    /// key is, so the values are marked to a fixpoint before any key is declared dead.
    unsafe fn process_weak_refs(&mut self) {
        self.mark_live_weak_values();
        let mut dying = Vec::new();
        let mut index = 0;
        while index < self.weak_refs.len() {
            let weak = self.weak_refs[index];
            if self.space.is_marked(weak.weak_key()) {
                index += 1;
            } else {
                dying.push(self.weak_refs.swap_remove(index));
            }
        }
        for weak in dying {
            let has_finalizer =
                weak.weak_c_finalizer() != 0 || weak.weak_finalizer() != self.known.nil;
            if has_finalizer {
                self.mark(weak.weak_key());
                self.mark(weak.weak_value());
                self.mark(weak.weak_finalizer());
                self.ready_to_finalize.push(weak);
            } else {
                self.clear_weak_ref(weak);
            }
        }
        // Every reference still registered has a key the first fixpoint marked, so marking what
        // the dying finalizers reach cannot revive another value: it only keeps those objects.
        self.drain();
    }

    unsafe fn mark_live_weak_values(&mut self) {
        loop {
            let mut changed = false;
            for index in 0..self.weak_refs.len() {
                let weak = self.weak_refs[index];
                if self.space.is_marked(weak.weak_key()) {
                    for child in [weak.weak_value(), weak.weak_finalizer()] {
                        if self.space.mark(child) {
                            self.mark_stack.push(child);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
            self.drain();
        }
    }

    /// The write barrier's invariant: an old object that is neither remembered nor escaped
    /// points only to old objects. A violation means some store skipped the barrier, and the
    /// next minor collection would free a live child.
    unsafe fn verify_barrier(&self) {
        let check = |parent: Sexp| {
            if !self.space.is_marked(parent)
                || parent.gc_bits() & (gc_bits::REMEMBERED | gc_bits::ESCAPED) != 0
            {
                return;
            }
            let mut children = vec![parent.attrib()];
            if parent.is_altrep() {
                children.extend([parent.car(), parent.cdr(), parent.tag()]);
            } else {
                match parent.kind().shape() {
                    Shape::Vector
                        if matches!(
                            parent.kind(),
                            SexpType::String | SexpType::List | SexpType::Expression
                        ) =>
                    {
                        children.extend(
                            (0..parent.length() as usize).map(|index| parent.element(index)),
                        );
                    }
                    Shape::Triple => children.extend([parent.car(), parent.cdr(), parent.tag()]),
                    Shape::Environment => {
                        children.push(parent.enclosure());
                        let frame = parent.frame_ptr();
                        if !frame.is_null() {
                            children.extend((*frame).cells());
                        }
                    }
                    Shape::ExternalPtr => {
                        children.extend([parent.external_tag(), parent.external_protected()])
                    }
                    Shape::Record => children.extend(
                        (0..parent.record_slot_count())
                            .filter_map(|slot| parent.record_handle(slot)),
                    ),
                    _ => {}
                }
            }
            for (position, child) in children.into_iter().enumerate() {
                if !self.space.is_marked(child) {
                    panic!(
                        "write barrier violated: old {:?} (length {}) holds young {:?} at child {position}",
                        parent.kind(),
                        if parent.kind().shape() == Shape::Vector {
                            parent.length()
                        } else {
                            0
                        },
                        child.kind(),
                    );
                }
            }
        };
        // Only marked objects are old, and the marked slots are exactly the live old objects:
        // an allocated but unmarked slot may never have held one.
        for object in self.space.marked_objects() {
            check(object);
        }
        for &object in &self.large {
            check(object);
        }
    }

    unsafe fn clear_marks(&mut self) {
        self.space.clear_marks();
        for &object in &self.large {
            object.set_gc_bit(gc_bits::MARK, false);
        }
        for parent in self.remembered.drain(..) {
            parent.set_gc_bit(gc_bits::REMEMBERED, false);
        }
    }

    unsafe fn sweep_young(&mut self) {
        let mut swept = Swept::default();
        let change = self.space.sweep_young(&mut swept);
        self.old_bytes = self.old_bytes.saturating_add_signed(change);
        let young_large = std::mem::take(&mut self.young_large);
        for &object in &young_large {
            if object.gc_bits() & gc_bits::MARK != 0 {
                self.old_bytes += self.size_of(object);
            } else {
                self.free_large(object);
                swept.freed += 1;
            }
        }
        self.young_large = young_large;
        self.young_large.clear();
        self.stats.objects_freed += swept.freed;
        self.refresh_live_stats();
    }

    unsafe fn sweep_all(&mut self) {
        let mut swept = Swept::default();
        let mut old_bytes = self.space.sweep_all(&mut swept);
        let large: Vec<Sexp> = self.large.iter().copied().collect();
        for object in large {
            if object.gc_bits() & gc_bits::MARK != 0 {
                old_bytes += self.size_of(object);
            } else {
                self.free_large(object);
                swept.freed += 1;
            }
        }
        self.young_large.clear();
        self.old_bytes = old_bytes;
        self.stats.objects_freed += swept.freed;
        self.refresh_live_stats();
    }

    fn refresh_live_stats(&mut self) {
        self.stats.live_objects = (self.space.live_slots() + self.large.len()) as u64;
        self.stats.live_bytes = self.old_bytes as u64;
        self.stats.large_objects = self.large.len() as u64;
        self.stats.pages = self.space.page_count() as u64;
    }

    unsafe fn size_of(&self, object: Sexp) -> usize {
        let class = object.size_class();
        if class != LARGE_CLASS {
            return SLOT_SIZES[class as usize];
        }
        let length = match object.kind() {
            SexpType::Char => object.length() as usize,
            SexpType::Record => object.record_slot_count(),
            _ => object.capacity() as usize,
        };
        object_size(object.kind(), length).unwrap_or(0)
    }

    unsafe fn free_large(&mut self, object: Sexp) {
        let size = self.size_of(object);
        self.large.remove(&object);
        std::alloc::dealloc(
            object.as_ptr().cast(),
            Layout::from_size_align_unchecked(size, 16),
        );
    }

    unsafe fn create_well_known(&mut self) {
        let nil = self
            .allocate(
                SexpType::Nil,
                object_size(SexpType::Nil, 0).unwrap_or(40),
                &[],
            )
            .unwrap_or_else(|error| panic!("cannot create R_NilValue: {error}"));
        nil.set_car_unbarriered(nil);
        nil.set_cdr_unbarriered(nil);
        nil.set_tag_unbarriered(nil);
        nil.mark_not_mutable();
        self.known.nil = nil;
        let result = (|| -> HeapResult<()> {
            let blank_string = self.mk_char(b"", Encoding::Native)?;
            self.known.blank_string = blank_string;
            self.add_global_root(blank_string);
            // NA_STRING is a CHARSXP of its own that is never interned, so "NA" and NA differ.
            let na_string = self.new_char(b"NA", gp::ASCII)?;
            self.known.na_string = na_string;
            let unbound = self.alloc_triple(SexpType::Symbol, nil, nil, nil)?;
            self.known.unbound = unbound;
            let missing_arg = self.alloc_triple(SexpType::Symbol, blank_string, nil, nil)?;
            self.known.missing_arg = missing_arg;
            let blank_scalar = self.alloc_vector(SexpType::String, 1)?;
            blank_scalar.set_element_unbarriered(0, blank_string);
            self.known.blank_scalar_string = blank_scalar;
            let empty_env = self.alloc_environment(nil)?;
            self.known.empty_env = empty_env;
            let base_env = self.alloc_environment(empty_env)?;
            self.known.base_env = base_env;
            let global_env = self.alloc_environment(base_env)?;
            self.known.global_env = global_env;
            self.set_frame_hashed(global_env, 0);
            let base_namespace = self.alloc_environment(global_env)?;
            self.known.base_namespace = base_namespace;
            let namespace_registry = self.alloc_environment(nil)?;
            self.known.namespace_registry = namespace_registry;
            let mut logical = |value: i32| -> HeapResult<Sexp> {
                let object = self.alloc_vector_inhibited(SexpType::Logical, 1)?;
                *(object.data() as *mut i32) = value;
                Ok(object)
            };
            let (true_value, false_value, na_value) =
                (logical(1)?, logical(0)?, logical(NA_LOGICAL)?);
            self.known.true_value = true_value;
            self.known.false_value = false_value;
            self.known.logical_na_value = na_value;
            for object in self.known.all() {
                object.mark_not_mutable();
            }
            Ok(())
        })();
        if let Err(error) = result {
            panic!("cannot create the well-known R objects: {error}");
        }
    }

    unsafe fn alloc_vector_inhibited(&mut self, kind: SexpType, length: isize) -> HeapResult<Sexp> {
        let size = object_size(kind, length as usize).ok_or(HeapError::TooLong)?;
        let object = self.allocate(kind, size, &[])?;
        object.set_length_and_capacity(length, length);
        Ok(object)
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // SAFETY: the heap owns every large object, and nothing outlives it. The space drops
        // its own objects.
        unsafe {
            let large: Vec<Sexp> = self.large.iter().copied().collect();
            for object in large {
                let size = self.size_of(object);
                std::alloc::dealloc(
                    object.as_ptr().cast(),
                    Layout::from_size_align_unchecked(size, 16),
                );
            }
        }
    }
}

impl WellKnown {
    fn placeholder() -> WellKnown {
        let dangling = Sexp::from_non_null(NonNull::dangling());
        WellKnown {
            nil: dangling,
            unbound: dangling,
            missing_arg: dangling,
            na_string: dangling,
            blank_string: dangling,
            blank_scalar_string: dangling,
            empty_env: dangling,
            base_env: dangling,
            base_namespace: dangling,
            global_env: dangling,
            namespace_registry: dangling,
            true_value: dangling,
            false_value: dangling,
            logical_na_value: dangling,
        }
    }

    fn all(&self) -> [Sexp; 14] {
        [
            self.nil,
            self.unbound,
            self.missing_arg,
            self.na_string,
            self.blank_string,
            self.blank_scalar_string,
            self.empty_env,
            self.base_env,
            self.base_namespace,
            self.global_env,
            self.namespace_registry,
            self.true_value,
            self.false_value,
            self.logical_na_value,
        ]
    }
}

impl Default for GcPolicy {
    fn default() -> Self {
        GcPolicy {
            // A larger nursery collects less often but touches more fresh memory before its
            // first collection, and first touches are what a short-lived process pays for most.
            young_limit: 8 * 1024 * 1024,
            old_limit: 64 * 1024 * 1024,
            min_old_limit: 64 * 1024 * 1024,
            stress: match gc_stress_requested() {
                Some(GcStressStart::Startup) => GcStress::EveryAllocation,
                _ => GcStress::Off,
            },
            verify: std::env::var_os("RY_GC_VERIFY").is_some(),
        }
    }
}

impl ProtectStack {
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    pub fn base(&self) -> usize {
        self.base
    }

    /// Starts a native call's protection region and returns the previous base, which the call
    /// restores with [`ProtectStack::leave`].
    pub fn enter(&mut self) -> usize {
        std::mem::replace(&mut self.base, self.stack.len())
    }

    /// Ends a native call's region, dropping whatever it left protected, and reports how deep
    /// the stack was before it was cut back, so an imbalance can be reported.
    pub fn leave(&mut self, saved_base: usize) -> usize {
        let depth = self.stack.len();
        self.stack.truncate(self.base);
        self.base = saved_base;
        depth
    }

    /// Cuts the stack back to `depth`, as a jump to an enclosing context does.
    pub fn restore(&mut self, depth: usize) {
        self.stack.truncate(depth.max(self.base));
    }

    pub fn protect(&mut self, object: Sexp) -> HeapResult<usize> {
        if self.stack.len() >= self.limit {
            return Err(HeapError::ProtectOverflow);
        }
        self.stack.push(object);
        Ok(self.stack.len() - 1)
    }

    pub fn unprotect(&mut self, count: usize) -> HeapResult<()> {
        let available = self.stack.len() - self.base;
        if count > available {
            return Err(HeapError::UnprotectUnderflow {
                available,
                requested: count,
            });
        }
        self.stack.truncate(self.stack.len() - count);
        Ok(())
    }

    /// `UNPROTECT_PTR`: removes the topmost protection of `object` wherever it is.
    pub fn unprotect_object(&mut self, object: Sexp) -> HeapResult<()> {
        let position = self.stack[self.base..]
            .iter()
            .rposition(|&protected| protected == object)
            .ok_or(HeapError::UnprotectPointerNotFound)?;
        self.stack.remove(self.base + position);
        Ok(())
    }

    pub fn reprotect(&mut self, object: Sexp, index: usize) -> HeapResult<()> {
        let available = self.stack.len();
        let slot = self
            .stack
            .get_mut(index)
            .ok_or(HeapError::ReprotectIndex { available, index })?;
        *slot = object;
        Ok(())
    }
}

impl Marker<'_> {
    pub unsafe fn mark(&mut self, object: Sexp) {
        if self.space.mark(object) {
            self.stack.push(object);
        }
    }
}

impl Sexp {
    unsafe fn set_length_and_capacity(self, length: isize, capacity: isize) {
        self.set_length(length);
        self.set_capacity(capacity);
    }
}

/// Every type code a vector can have, for generators and tests.
pub const VECTOR_TYPES: [SexpType; 8] = [
    SexpType::Logical,
    SexpType::Integer,
    SexpType::Real,
    SexpType::Complex,
    SexpType::String,
    SexpType::List,
    SexpType::Expression,
    SexpType::Raw,
];

const _: () = assert!(VECTOR_DATA_OFFSET.is_multiple_of(16));
const _: () = assert!(REFCOUNT_MAX == u16::MAX);

unsafe fn bump_symbol_epoch(symbol: Sexp) {
    let epoch = &symbol.symbol_payload().epoch;
    epoch.set(epoch.get() + 1);
}
