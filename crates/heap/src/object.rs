//! The object model. Every R value is a heap object behind a [`Sexp`] handle, which is the raw,
//! non-moving pointer that native code receives as a C `SEXP`.
//!
//! Every object starts with a [`Header`]. What follows depends on the object's shape:
//!
//! - a vector (`CHARSXP`, the atomic vectors, `STRSXP`, `VECSXP`, `EXPRSXP`) stores its length,
//!   its capacity and then its elements inline, so `INTEGER(x)` is an interior pointer;
//! - a triple (pairlists, calls, closures, promises, symbols, binding cells, ALTREP instances)
//!   stores three handles, which are `CAR`, `CDR` and `TAG` for a pairlist;
//! - an environment stores its frame and its enclosure;
//! - an external pointer stores its address, tag and protected value;
//! - a weak reference stores its key, value and finalizer;
//! - a primitive stores its index in the primitive table.
//!
//! The accessors are `unsafe` because a handle is only valid while its object is reachable from a
//! root. Holding one across an allocation without rooting it is a bug, and the GC stress tests
//! exist to catch it.

use crate::env::Frame;
use std::{cell::Cell, ffi::c_void, fmt, ptr::NonNull};

/// R's type codes, numbered exactly as GNU R numbers them because native code switches on them.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SexpType {
    Nil = 0,
    Symbol = 1,
    Pairlist = 2,
    Closure = 3,
    Environment = 4,
    Promise = 5,
    Language = 6,
    Special = 7,
    Builtin = 8,
    Char = 9,
    Logical = 10,
    Integer = 13,
    Real = 14,
    Complex = 15,
    String = 16,
    Dots = 17,
    Any = 18,
    List = 19,
    Expression = 20,
    Bytecode = 21,
    ExternalPtr = 22,
    WeakRef = 23,
    Raw = 24,
    Object = 25,
    /// An environment binding cell. It is internal to ry and never reaches native code, so it
    /// takes a code GNU R leaves unused.
    Binding = 26,
    /// A free heap slot. Seeing it through a handle means the handle outlived its object.
    Free = 31,
}

impl SexpType {
    pub fn from_code(code: u32) -> Option<SexpType> {
        Some(match code {
            0 => SexpType::Nil,
            1 => SexpType::Symbol,
            2 => SexpType::Pairlist,
            3 => SexpType::Closure,
            4 => SexpType::Environment,
            5 => SexpType::Promise,
            6 => SexpType::Language,
            7 => SexpType::Special,
            8 => SexpType::Builtin,
            9 => SexpType::Char,
            10 => SexpType::Logical,
            13 => SexpType::Integer,
            14 => SexpType::Real,
            15 => SexpType::Complex,
            16 => SexpType::String,
            17 => SexpType::Dots,
            18 => SexpType::Any,
            19 => SexpType::List,
            20 => SexpType::Expression,
            21 => SexpType::Bytecode,
            22 => SexpType::ExternalPtr,
            23 => SexpType::WeakRef,
            24 => SexpType::Raw,
            25 => SexpType::Object,
            _ => return None,
        })
    }

    pub fn code(self) -> u32 {
        self as u32
    }

    /// The name `typeof()` reports, which is also what `Rf_type2char` returns.
    pub fn name(self) -> &'static str {
        match self {
            SexpType::Nil => "NULL",
            SexpType::Symbol => "symbol",
            SexpType::Pairlist => "pairlist",
            SexpType::Closure => "closure",
            SexpType::Environment => "environment",
            SexpType::Promise => "promise",
            SexpType::Language => "language",
            SexpType::Special => "special",
            SexpType::Builtin => "builtin",
            SexpType::Char => "char",
            SexpType::Logical => "logical",
            SexpType::Integer => "integer",
            SexpType::Real => "double",
            SexpType::Complex => "complex",
            SexpType::String => "character",
            SexpType::Dots => "...",
            SexpType::Any => "any",
            SexpType::List => "list",
            SexpType::Expression => "expression",
            SexpType::Bytecode => "bytecode",
            SexpType::ExternalPtr => "externalptr",
            SexpType::WeakRef => "weakref",
            SexpType::Raw => "raw",
            SexpType::Object => "object",
            SexpType::Binding => "binding",
            SexpType::Free => "FREESXP",
        }
    }

    /// The name of the `SEXPTYPE` constant, which some R error messages print.
    pub fn constant_name(self) -> &'static str {
        match self {
            SexpType::Nil => "NILSXP",
            SexpType::Symbol => "SYMSXP",
            SexpType::Pairlist => "LISTSXP",
            SexpType::Closure => "CLOSXP",
            SexpType::Environment => "ENVSXP",
            SexpType::Promise => "PROMSXP",
            SexpType::Language => "LANGSXP",
            SexpType::Special => "SPECIALSXP",
            SexpType::Builtin => "BUILTINSXP",
            SexpType::Char => "CHARSXP",
            SexpType::Logical => "LGLSXP",
            SexpType::Integer => "INTSXP",
            SexpType::Real => "REALSXP",
            SexpType::Complex => "CPLXSXP",
            SexpType::String => "STRSXP",
            SexpType::Dots => "DOTSXP",
            SexpType::Any => "ANYSXP",
            SexpType::List => "VECSXP",
            SexpType::Expression => "EXPRSXP",
            SexpType::Bytecode => "BCODESXP",
            SexpType::ExternalPtr => "EXTPTRSXP",
            SexpType::WeakRef => "WEAKREFSXP",
            SexpType::Raw => "RAWSXP",
            SexpType::Object => "OBJSXP",
            SexpType::Binding => "BINDINGSXP",
            SexpType::Free => "FREESXP",
        }
    }

    /// The types whose elements are stored inline after the header.
    pub fn is_vector(self) -> bool {
        self.element_size().is_some()
    }

    pub fn is_atomic(self) -> bool {
        matches!(
            self,
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::String
                | SexpType::Raw
        )
    }

    /// Bytes per element, for the vector types only.
    pub fn element_size(self) -> Option<usize> {
        Some(match self {
            SexpType::Char | SexpType::Raw => 1,
            SexpType::Logical | SexpType::Integer => 4,
            SexpType::Real => 8,
            SexpType::Complex => 16,
            SexpType::String | SexpType::List | SexpType::Expression => size_of::<Sexp>(),
            _ => return None,
        })
    }

    /// The shape of the payload after the header. An ALTREP instance reports a vector type but
    /// has the triple shape, so callers that walk objects check `is_altrep` first.
    pub fn shape(self) -> Shape {
        if self.is_vector() {
            return Shape::Vector;
        }
        match self {
            SexpType::Environment => Shape::Environment,
            SexpType::ExternalPtr => Shape::ExternalPtr,
            SexpType::WeakRef => Shape::WeakRef,
            SexpType::Builtin | SexpType::Special => Shape::Primitive,
            _ => Shape::Triple,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Vector,
    Triple,
    Environment,
    ExternalPtr,
    WeakRef,
    Primitive,
}

/// The header every object starts with. Sixteen bytes on a 64-bit target.
#[repr(C)]
pub struct Header {
    pub(crate) kind: Cell<u8>,
    gc: Cell<u8>,
    flags: Cell<u8>,
    size_class: Cell<u8>,
    gp: Cell<u16>,
    refcount: Cell<u16>,
    attrib: Cell<Sexp>,
}

/// A handle to a heap object: the pointer native code sees as a `SEXP`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sexp(NonNull<Header>);

impl fmt::Debug for Sexp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Sexp({:p})", self.0)
    }
}

/// R's complex number layout, which is part of the C API.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rcomplex {
    pub r: f64,
    pub i: f64,
}

pub const NA_INTEGER: i32 = i32::MIN;
pub const NA_LOGICAL: i32 = i32::MIN;
/// `NA_real_` is a NaN whose low word is 1954, the year Ross Ihaka was born.
pub const NA_REAL_BITS: u64 = 0x7FF0_0000_0000_07A2;
pub const NA_REAL: f64 = f64::from_bits(NA_REAL_BITS);

/// `R_IsNA`: the NaN that R reads as a missing value, as opposed to a computed NaN.
pub fn is_na_real(value: f64) -> bool {
    value.is_nan() && (value.to_bits() & 0xFFFF_FFFF) == 1954
}

/// The general-purpose bits, which GNU R calls `LEVELS`. Their positions are not API, but
/// packages test them through `LEVELS()` (data.table's `IS_UTF8` is `LEVELS(x) & 8`), so they
/// keep GNU R's positions.
pub mod gp {
    pub const BYTES: u16 = 1 << 1;
    pub const LATIN1: u16 = 1 << 2;
    pub const UTF8: u16 = 1 << 3;
    pub const S4_OBJECT: u16 = 1 << 4;
    pub const CACHED: u16 = 1 << 5;
    pub const ASCII: u16 = 1 << 6;
    /// The encoding bits that distinguish two interned strings with the same bytes.
    pub const ENCODING: u16 = BYTES | LATIN1 | UTF8;
    pub const BINDING_LOCKED: u16 = 1 << 14;
    pub const ACTIVE_BINDING: u16 = 1 << 15;
    pub const FRAME_LOCKED: u16 = 1 << 14;
}

pub(crate) mod gc_bits {
    /// Reached by the current collection. Marks are sticky between collections, so a marked
    /// object outside a collection belongs to the old generation.
    pub const MARK: u8 = 1 << 0;
    /// In the remembered set: an old object that may point at young objects.
    pub const REMEMBERED: u8 = 1 << 1;
    /// Allocated on its own instead of in a size-class page.
    pub const LARGE: u8 = 1 << 2;
    /// A writable pointer to its elements was handed out (`STRING_PTR`, `DATAPTR` on a list),
    /// so stores into it bypass the write barrier and it is scanned at every minor collection.
    pub const ESCAPED: u8 = 1 << 3;
}

pub(crate) mod flag_bits {
    pub const OBJECT: u8 = 1 << 0;
    pub const ALTREP: u8 = 1 << 1;
    /// The object was preallocated by the heap (`R_NilValue`, `NA_STRING`, the environments).
    /// Allocated by `R_allocResizableVector`, so its length may change within its capacity.
    pub const RESIZABLE: u8 = 1 << 2;
}

/// The reference count saturates here, and a saturated count is sticky, as in GNU R.
pub const REFCOUNT_MAX: u16 = u16::MAX;

#[repr(C)]
pub(crate) struct VectorObject {
    pub header: Header,
    pub length: Cell<isize>,
    pub capacity: Cell<isize>,
}

#[repr(C)]
pub(crate) struct TripleObject {
    pub header: Header,
    pub first: Cell<Sexp>,
    pub second: Cell<Sexp>,
    pub third: Cell<Sexp>,
}

#[repr(C)]
pub(crate) struct EnvironmentObject {
    pub header: Header,
    pub frame: Cell<*mut Frame>,
    pub enclosure: Cell<Sexp>,
    pub spare: Cell<usize>,
}

#[repr(C)]
pub(crate) struct ExternalPtrObject {
    pub header: Header,
    pub address: Cell<*mut c_void>,
    pub tag: Cell<Sexp>,
    pub protected: Cell<Sexp>,
}

#[repr(C)]
pub(crate) struct WeakRefObject {
    pub header: Header,
    pub key: Cell<Sexp>,
    pub value: Cell<Sexp>,
    pub finalizer: Cell<Sexp>,
    /// A C finalizer, stored as an address so the heap stays free of C function types.
    pub c_finalizer: Cell<usize>,
    pub finalize_on_exit: Cell<bool>,
}

#[repr(C)]
pub(crate) struct PrimitiveObject {
    pub header: Header,
    pub index: Cell<u32>,
}

/// Where a vector's elements start. Sixteen-byte aligned, so every element type is aligned.
pub const VECTOR_DATA_OFFSET: usize = size_of::<VectorObject>();

const _: () = assert!(size_of::<Header>() == 16);
const _: () = assert!(VECTOR_DATA_OFFSET == 32);

/// The bytes an object of `kind` with `length` elements occupies, header included. `CHARSXP`
/// keeps a terminating NUL after its bytes, as `R_CHAR` promises.
pub fn object_size(kind: SexpType, length: usize) -> Option<usize> {
    match kind.shape() {
        Shape::Vector => {
            let element = kind.element_size()?;
            let elements = if kind == SexpType::Char {
                length.checked_add(1)?
            } else {
                length
            };
            let data = elements.checked_mul(element)?;
            VECTOR_DATA_OFFSET
                .checked_add(data)?
                .checked_add(15)
                .map(|size| size & !15)
        }
        Shape::Triple => Some(size_of::<TripleObject>()),
        Shape::Environment => Some(size_of::<EnvironmentObject>()),
        Shape::ExternalPtr => Some(size_of::<ExternalPtrObject>()),
        Shape::WeakRef => Some(size_of::<WeakRefObject>()),
        Shape::Primitive => Some(size_of::<PrimitiveObject>()),
    }
}

impl Sexp {
    /// # Safety
    /// `pointer` must be null or point at a live heap object.
    pub unsafe fn from_raw(pointer: *mut Header) -> Option<Sexp> {
        NonNull::new(pointer).map(Sexp)
    }

    /// A placeholder for a handle slot that is filled before first use, such as an exported
    /// data symbol before the runtime starts. It must never be dereferenced.
    pub const fn dangling() -> Sexp {
        Sexp(NonNull::dangling())
    }

    pub fn as_ptr(self) -> *mut Header {
        self.0.as_ptr()
    }

    pub(crate) fn from_non_null(pointer: NonNull<Header>) -> Sexp {
        Sexp(pointer)
    }

    pub(crate) unsafe fn header<'a>(self) -> &'a Header {
        unsafe { self.0.as_ref() }
    }

    unsafe fn payload<'a, T>(self) -> &'a T {
        unsafe { &*(self.0.as_ptr() as *const T) }
    }

    pub unsafe fn kind(self) -> SexpType {
        // The kind byte only ever holds a `SexpType` discriminant.
        unsafe { std::mem::transmute::<u8, SexpType>(self.header().kind.get()) }
    }

    pub unsafe fn attrib(self) -> Sexp {
        unsafe { self.header().attrib.get() }
    }

    /// Writes the attribute pairlist without the write barrier or reference counts; the heap's
    /// `set_attrib` is the barriered form.
    pub(crate) unsafe fn set_attrib_unbarriered(self, value: Sexp) {
        unsafe { self.header().attrib.set(value) }
    }

    pub unsafe fn gp(self) -> u16 {
        unsafe { self.header().gp.get() }
    }

    pub unsafe fn set_gp(self, value: u16) {
        unsafe { self.header().gp.set(value) }
    }

    pub unsafe fn has_gp(self, bits: u16) -> bool {
        unsafe { self.gp() & bits != 0 }
    }

    pub unsafe fn set_gp_bits(self, bits: u16, on: bool) {
        unsafe {
            let gp = self.gp();
            self.set_gp(if on { gp | bits } else { gp & !bits });
        }
    }

    pub unsafe fn is_object(self) -> bool {
        unsafe { self.header().flags.get() & flag_bits::OBJECT != 0 }
    }

    pub unsafe fn set_object(self, on: bool) {
        unsafe { self.set_flag(flag_bits::OBJECT, on) }
    }

    pub unsafe fn is_altrep(self) -> bool {
        unsafe { self.header().flags.get() & flag_bits::ALTREP != 0 }
    }

    pub(crate) unsafe fn set_flag(self, bit: u8, on: bool) {
        unsafe {
            let flags = &self.header().flags;
            flags.set(if on {
                flags.get() | bit
            } else {
                flags.get() & !bit
            });
        }
    }

    pub(crate) unsafe fn flags(self) -> u8 {
        unsafe { self.header().flags.get() }
    }

    pub(crate) unsafe fn gc_bits(self) -> u8 {
        unsafe { self.header().gc.get() }
    }

    pub(crate) unsafe fn set_gc_bit(self, bit: u8, on: bool) {
        unsafe {
            let gc = &self.header().gc;
            gc.set(if on { gc.get() | bit } else { gc.get() & !bit });
        }
    }

    pub(crate) unsafe fn is_marked(self) -> bool {
        unsafe { self.gc_bits() & gc_bits::MARK != 0 }
    }

    pub(crate) unsafe fn size_class(self) -> u8 {
        unsafe { self.header().size_class.get() }
    }

    /// The number of references from other objects, which drives copy-on-modify:
    /// `MAYBE_SHARED` is a count above one.
    pub unsafe fn refcount(self) -> u16 {
        unsafe { self.header().refcount.get() }
    }

    pub(crate) unsafe fn increment_refcount(self) {
        unsafe {
            let count = &self.header().refcount;
            if count.get() != REFCOUNT_MAX {
                count.set(count.get() + 1);
            }
        }
    }

    pub(crate) unsafe fn decrement_refcount(self) {
        unsafe {
            let count = &self.header().refcount;
            if count.get() != REFCOUNT_MAX && count.get() > 0 {
                count.set(count.get() - 1);
            }
        }
    }

    /// `MARK_NOT_MUTABLE`: saturate the count, so the object is shared from now on.
    pub unsafe fn mark_not_mutable(self) {
        unsafe { self.header().refcount.set(REFCOUNT_MAX) }
    }

    // Vectors. The caller guarantees the object has the vector shape and is not ALTREP.

    pub unsafe fn length(self) -> isize {
        unsafe { self.payload::<VectorObject>().length.get() }
    }

    /// The elements the allocation has room for, which exceeds the length only for a
    /// resizable vector (`R_allocResizableVector`). For a `CHARSXP` the field holds its hash.
    pub unsafe fn capacity(self) -> isize {
        unsafe { self.payload::<VectorObject>().capacity.get() }
    }

    pub(crate) unsafe fn set_capacity(self, capacity: isize) {
        unsafe { self.payload::<VectorObject>().capacity.set(capacity) }
    }

    /// Shrinks or regrows a vector within its capacity. Elements past the old length must have
    /// been initialized by the caller.
    pub unsafe fn set_length(self, length: isize) {
        unsafe { self.payload::<VectorObject>().length.set(length) }
    }

    /// The first element, as the interior pointer `DATAPTR` returns.
    pub unsafe fn data(self) -> *mut u8 {
        unsafe { (self.0.as_ptr() as *mut u8).add(VECTOR_DATA_OFFSET) }
    }

    pub unsafe fn slice<'a, T>(self) -> &'a [T] {
        unsafe { std::slice::from_raw_parts(self.data() as *const T, self.length() as usize) }
    }

    #[allow(clippy::mut_from_ref)]
    pub unsafe fn slice_mut<'a, T>(self) -> &'a mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.data() as *mut T, self.length() as usize) }
    }

    /// A `CHARSXP`'s or `RAWSXP`'s bytes, without the terminating NUL.
    pub unsafe fn bytes<'a>(self) -> &'a [u8] {
        unsafe { self.slice::<u8>() }
    }

    /// An element of a `STRSXP`, `VECSXP` or `EXPRSXP`.
    pub unsafe fn element(self, index: usize) -> Sexp {
        unsafe { *(self.data() as *const Sexp).add(index) }
    }

    pub(crate) unsafe fn set_element_unbarriered(self, index: usize, value: Sexp) {
        unsafe { *(self.data() as *mut Sexp).add(index) = value }
    }

    // Triples: pairlists, calls, closures, promises, symbols, binding cells, ALTREP instances.

    pub unsafe fn car(self) -> Sexp {
        unsafe { self.payload::<TripleObject>().first.get() }
    }

    pub unsafe fn cdr(self) -> Sexp {
        unsafe { self.payload::<TripleObject>().second.get() }
    }

    pub unsafe fn tag(self) -> Sexp {
        unsafe { self.payload::<TripleObject>().third.get() }
    }

    pub(crate) unsafe fn set_car_unbarriered(self, value: Sexp) {
        unsafe { self.payload::<TripleObject>().first.set(value) }
    }

    pub(crate) unsafe fn set_cdr_unbarriered(self, value: Sexp) {
        unsafe { self.payload::<TripleObject>().second.set(value) }
    }

    pub(crate) unsafe fn set_tag_unbarriered(self, value: Sexp) {
        unsafe { self.payload::<TripleObject>().third.set(value) }
    }

    // Environments.

    pub(crate) unsafe fn frame_ptr(self) -> *mut Frame {
        unsafe { self.payload::<EnvironmentObject>().frame.get() }
    }

    pub(crate) unsafe fn set_frame_ptr(self, frame: *mut Frame) {
        unsafe { self.payload::<EnvironmentObject>().frame.set(frame) }
    }

    pub unsafe fn enclosure(self) -> Sexp {
        unsafe { self.payload::<EnvironmentObject>().enclosure.get() }
    }

    pub(crate) unsafe fn set_enclosure_unbarriered(self, value: Sexp) {
        unsafe { self.payload::<EnvironmentObject>().enclosure.set(value) }
    }

    // External pointers.

    pub unsafe fn external_address(self) -> *mut c_void {
        unsafe { self.payload::<ExternalPtrObject>().address.get() }
    }

    pub unsafe fn set_external_address(self, address: *mut c_void) {
        unsafe { self.payload::<ExternalPtrObject>().address.set(address) }
    }

    pub unsafe fn external_tag(self) -> Sexp {
        unsafe { self.payload::<ExternalPtrObject>().tag.get() }
    }

    pub unsafe fn external_protected(self) -> Sexp {
        unsafe { self.payload::<ExternalPtrObject>().protected.get() }
    }

    pub(crate) unsafe fn set_external_tag_unbarriered(self, value: Sexp) {
        unsafe { self.payload::<ExternalPtrObject>().tag.set(value) }
    }

    pub(crate) unsafe fn set_external_protected_unbarriered(self, value: Sexp) {
        unsafe { self.payload::<ExternalPtrObject>().protected.set(value) }
    }

    // Weak references.

    pub unsafe fn weak_key(self) -> Sexp {
        unsafe { self.payload::<WeakRefObject>().key.get() }
    }

    pub unsafe fn weak_value(self) -> Sexp {
        unsafe { self.payload::<WeakRefObject>().value.get() }
    }

    pub unsafe fn weak_finalizer(self) -> Sexp {
        unsafe { self.payload::<WeakRefObject>().finalizer.get() }
    }

    pub unsafe fn weak_c_finalizer(self) -> usize {
        unsafe { self.payload::<WeakRefObject>().c_finalizer.get() }
    }

    pub unsafe fn weak_finalize_on_exit(self) -> bool {
        unsafe { self.payload::<WeakRefObject>().finalize_on_exit.get() }
    }

    pub(crate) unsafe fn weak_payload<'a>(self) -> &'a WeakRefObject {
        unsafe { self.payload::<WeakRefObject>() }
    }

    // Primitives.

    pub unsafe fn primitive_index(self) -> u32 {
        unsafe { self.payload::<PrimitiveObject>().index.get() }
    }

    pub(crate) unsafe fn set_primitive_index(self, index: u32) {
        unsafe { self.payload::<PrimitiveObject>().index.set(index) }
    }

    /// Initializes a fresh slot's header. The caller fills the payload before anything reads it.
    pub(crate) unsafe fn init_header(self, kind: SexpType, size_class: u8, gc: u8, nil: Sexp) {
        unsafe {
            let header = self.0.as_ptr();
            std::ptr::write(
                header,
                Header {
                    kind: Cell::new(kind as u8),
                    gc: Cell::new(gc),
                    flags: Cell::new(0),
                    size_class: Cell::new(size_class),
                    gp: Cell::new(0),
                    refcount: Cell::new(0),
                    attrib: Cell::new(nil),
                },
            );
        }
    }

    pub(crate) unsafe fn set_altrep_flag(self) {
        unsafe { self.set_flag(flag_bits::ALTREP, true) }
    }

    /// The raw kind byte, read without forming a reference to the header, so it is valid on a
    /// free slot too.
    pub(crate) unsafe fn kind_byte(self) -> u8 {
        unsafe { *(self.0.as_ptr() as *const u8) }
    }
}
