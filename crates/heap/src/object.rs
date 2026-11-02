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
//! - a primitive stores its index in the primitive table;
//! - a ry record, tuple or tag stores its interned layout id, its slot count, a mask of the
//!   slots that hold raw numbers, and then its eight-byte slots.
//!
//! The accessors are `unsafe` because a handle is only valid while its object is reachable from a
//! root. Holding one across an allocation without rooting it is a bug, and the GC stress tests
//! exist to catch it.

use {
    crate::env::Frame,
    std::{
        cell::{Cell, UnsafeCell},
        ffi::c_void,
        fmt,
        ptr::NonNull,
        sync::OnceLock,
    },
};

/// Reads and writes elements of ALTREP string and list vectors. Only the C API host knows the
/// classes' methods, so it installs these; the heap and the interpreter then treat every vector
/// alike.
#[derive(Clone, Copy)]
pub struct AltrepElements {
    pub read: unsafe fn(Sexp, usize) -> Sexp,
    pub write: unsafe fn(Sexp, usize, Sexp),
}

static ALTREP_ELEMENTS: OnceLock<AltrepElements> = OnceLock::new();

/// Installs the accessors behind [`Sexp::element`] and `Heap::set_element` for ALTREP
/// instances. Later calls keep the first.
pub fn install_altrep_elements(elements: AltrepElements) {
    ALTREP_ELEMENTS.get_or_init(|| elements);
}

/// The installed ALTREP accessors, for an object that has the ALTREP flag.
pub(crate) fn altrep_elements() -> Option<&'static AltrepElements> {
    ALTREP_ELEMENTS.get()
}

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
    /// A ry record, tuple or tag. GNU R leaves the code unused; the layout says which of the
    /// three a value is (see [`crate::records`]).
    Record = 27,
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
            27 => SexpType::Record,
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
            SexpType::Object => "S4",
            SexpType::Binding => "binding",
            SexpType::Record => "record",
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
            SexpType::Record => "RECORDSXP",
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
            SexpType::Record => Shape::Record,
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
    Record,
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
    /// GNU R's `GROWABLE_MASK` on a vector, the same bit as `CACHED` on a string. ry keeps
    /// growability in the heap's resizable flag, and `LEVELS` reports it through this bit.
    pub const GROWABLE: u16 = 1 << 5;
    pub const ASCII: u16 = 1 << 6;
    /// The encoding bits that distinguish two interned strings with the same bytes.
    pub const ENCODING: u16 = BYTES | LATIN1 | UTF8;
    /// A binding cell for a formal bound to its default, which `missing()` reads.
    pub const BINDING_MISSING: u16 = 1 << 0;
    /// A binding cell for an S4 method's formal whose argument was missing from the generic's
    /// call (GNU R's `MISSING == 1`). Unlike a plain default it stays missing when the method
    /// passes it on, so `missing()` in a callee sees it.
    pub const BINDING_ABSENT: u16 = 1 << 1;
    pub const BINDING_LOCKED: u16 = 1 << 14;
    pub const ACTIVE_BINDING: u16 = 1 << 15;
    pub const FRAME_LOCKED: u16 = 1 << 14;
    /// An environment that is a closure call's frame. Bindings appear in such frames all the
    /// time, so they do not move the heap's binding epoch, and cached lookups never resolve
    /// through one.
    pub const FUNCTION_FRAME: u16 = 1 << 8;
    /// An environment a compiled closure body is running on. The body keeps values in registers
    /// that do not count as references, which code running on the environment from elsewhere
    /// must respect before it updates a value in place.
    pub const COMPILED_FRAME: u16 = 1 << 9;
    /// A binding cell, or a symbol, that a compiled function lookup depends on. Rebinding the
    /// cell, or binding the symbol in a function frame, moves the heap's binding epoch, so
    /// compiled code revalidates its lookups only when the epoch moved.
    pub const WATCHED: u16 = 1 << 10;
    /// A function definition's formals pairlist (or its body, when it has no formals) for which
    /// the interpreter registered inline-annotation checks.
    pub const TYPED_DEFINITION: u16 = 1 << 12;
    /// A promise whose value an inline parameter annotation checks when it is forced.
    pub const CHECKED_PROMISE: u16 = 1 << 12;
}

pub(crate) mod gc_bits {
    /// The mark of a large object, which lives outside the small-object space and its mark
    /// bitmaps. Marks are sticky between collections, so a marked object outside a collection
    /// belongs to the old generation.
    pub const MARK: u8 = 1 << 0;
    /// In the remembered set: an old object that may point at young objects.
    pub const REMEMBERED: u8 = 1 << 1;
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
    /// The object's fields do not count as references (GNU R's `DISABLE_REFCNT`), as for the
    /// `*tmp*` value of a complex assignment, which must not make the target look shared.
    pub const UNTRACKED: u8 = 1 << 3;
    /// A ry Array: an attribute-free, NA-free vector built by `[...]` or `as.Array`. The bit lives
    /// here, not in the general-purpose bits, so C code reading `LEVELS` never sees it.
    pub const ARRAY: u8 = 1 << 4;
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

/// A symbol is a triple (print name, base binding, internal) followed by the state of its lookup
/// caches, which no collection traces: every reader checks the stamp first, and a cell whose
/// stamp is current is still bound in an environment on the search path.
#[repr(C)]
pub(crate) struct SymbolObject {
    pub triple: TripleObject,
    pub epoch: Cell<u64>,
    pub global_cell: Cell<Option<Sexp>>,
    pub global_stamp: Cell<u64>,
}

#[repr(C)]
pub(crate) struct EnvironmentObject {
    pub header: Header,
    /// The frame lives inside the object, so a closure call's environment costs one heap
    /// allocation and no malloc while its bindings fit the frame's inline cells.
    pub frame: UnsafeCell<Frame>,
    pub enclosure: Cell<Sexp>,
}

// Every closure call allocates an environment, so growing the frame past this size class would
// cost each call a larger slot to zero.
const _: () = assert!(size_of::<EnvironmentObject>() <= 192);

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

#[repr(C)]
pub(crate) struct RecordObject {
    pub header: Header,
    pub layout: Cell<u32>,
    /// The layout's slot count, kept in the object so the collector and the sweeper size and
    /// trace it without consulting the layout table. It fills what would otherwise be padding.
    pub slot_count: Cell<u32>,
    pub unboxed: Cell<u64>,
}

/// One slot of a record, tuple or tag, as stored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slot {
    Handle(Sexp),
    Real(f64),
    Integer(i32),
    Logical(i32),
}

/// Slots past this index are always handles, because the unboxed mask has one bit per slot.
pub const UNBOXED_SLOTS: usize = 64;

/// A raw slot is eight bytes. A double is stored as its bits, and an integer or logical as its
/// 32 bits under one of these quiet-NaN prefixes, so the one-bit mask is enough to tell a raw
/// slot from a handle. A double whose upper word happens to equal a prefix is stored boxed
/// instead, which keeps every NaN payload (`NA_real_` among them) exact.
pub const INTEGER_PREFIX: u32 = 0xFFF9_A11E;
pub const LOGICAL_PREFIX: u32 = 0xFFF9_A11F;

impl Slot {
    fn encode(self) -> u64 {
        match self {
            Slot::Handle(handle) => handle.as_ptr() as u64,
            Slot::Real(value) => value.to_bits(),
            Slot::Integer(value) => (u64::from(INTEGER_PREFIX) << 32) | u64::from(value as u32),
            Slot::Logical(value) => (u64::from(LOGICAL_PREFIX) << 32) | u64::from(value as u32),
        }
    }

    fn decode_raw(bits: u64) -> Slot {
        let low = bits as u32 as i32;
        match (bits >> 32) as u32 {
            INTEGER_PREFIX => Slot::Integer(low),
            LOGICAL_PREFIX => Slot::Logical(low),
            _ => Slot::Real(f64::from_bits(bits)),
        }
    }
}

/// How `value` is stored in slot `index`: raw when it is a length-one double, integer or logical
/// without attributes, and a handle otherwise. A resizable vector stays a handle, because it is
/// kept to grow in place.
pub unsafe fn slot_for(value: Sexp, index: usize) -> Slot {
    unsafe {
        if index >= UNBOXED_SLOTS
            || value.is_altrep()
            || value.attrib().kind() != SexpType::Nil
            || !matches!(
                value.kind(),
                SexpType::Real | SexpType::Integer | SexpType::Logical
            )
            || value.length() != 1
            || value.flags() & flag_bits::RESIZABLE != 0
        {
            return Slot::Handle(value);
        }
        match value.kind() {
            SexpType::Real => {
                let number = *(value.data() as *const f64);
                let upper = (number.to_bits() >> 32) as u32;
                if upper == INTEGER_PREFIX || upper == LOGICAL_PREFIX {
                    Slot::Handle(value)
                } else {
                    Slot::Real(number)
                }
            }
            SexpType::Integer => Slot::Integer(*(value.data() as *const i32)),
            _ => Slot::Logical(*(value.data() as *const i32)),
        }
    }
}

/// Where a record's slots start.
pub const RECORD_DATA_OFFSET: usize = size_of::<RecordObject>();
/// Where a record's layout id and its unboxed-slot mask are, for compiled field reads.
pub const RECORD_LAYOUT_OFFSET: usize = std::mem::offset_of!(RecordObject, layout);
pub const RECORD_UNBOXED_OFFSET: usize = std::mem::offset_of!(RecordObject, unboxed);

/// Where a vector's elements start. Sixteen-byte aligned, so every element type is aligned.
pub const VECTOR_DATA_OFFSET: usize = size_of::<VectorObject>();

const _: () = assert!(size_of::<Header>() == 16);
const _: () = assert!(VECTOR_DATA_OFFSET == 32);
const _: () = assert!(RECORD_DATA_OFFSET == 32);

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
        Shape::Triple if kind == SexpType::Symbol => Some(size_of::<SymbolObject>()),
        Shape::Triple => Some(size_of::<TripleObject>()),
        Shape::Environment => Some(size_of::<EnvironmentObject>()),
        Shape::ExternalPtr => Some(size_of::<ExternalPtrObject>()),
        Shape::WeakRef => Some(size_of::<WeakRefObject>()),
        Shape::Primitive => Some(size_of::<PrimitiveObject>()),
        Shape::Record => length.checked_mul(8)?.checked_add(RECORD_DATA_OFFSET),
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

    /// Whether the object's fields count as references to what they hold (see
    /// `flag_bits::UNTRACKED`).
    pub unsafe fn tracks_references(self) -> bool {
        unsafe { self.header().flags.get() & flag_bits::UNTRACKED == 0 }
    }

    pub unsafe fn is_array(self) -> bool {
        unsafe { self.header().flags.get() & flag_bits::ARRAY != 0 }
    }

    pub unsafe fn set_array(self, on: bool) {
        unsafe { self.set_flag(flag_bits::ARRAY, on) }
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

    pub(crate) unsafe fn size_class(self) -> u8 {
        unsafe { self.header().size_class.get() }
    }

    /// The number of references from other objects, which drives copy-on-modify:
    /// `MAYBE_SHARED` is a count above one.
    pub unsafe fn refcount(self) -> u16 {
        unsafe { self.header().refcount.get() }
    }

    pub unsafe fn increment_refcount(self) {
        unsafe {
            let count = &self.header().refcount;
            if count.get() != REFCOUNT_MAX {
                count.set(count.get() + 1);
            }
        }
    }

    pub unsafe fn decrement_refcount(self) {
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

    /// A `CHARSXP`'s intern hash, the high half of its capacity field.
    pub unsafe fn char_hash(self) -> u32 {
        unsafe { (self.capacity() as u64 >> 32) as u32 }
    }

    /// A `CHARSXP`'s `TRUELENGTH`, the low half of its capacity field. data.table's string
    /// kernels set and read the truelength of every string they meet, so it lives in the string
    /// itself and costs a load, as in GNU R.
    pub unsafe fn char_truelength(self) -> i32 {
        unsafe { self.capacity() as u64 as u32 as i32 }
    }

    pub unsafe fn set_char_truelength(self, value: i32) {
        unsafe {
            let hash = self.capacity() as u64 & !0xFFFF_FFFF;
            self.set_capacity((hash | u64::from(value as u32)) as isize);
        }
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

    /// An element of a `STRSXP`, `VECSXP` or `EXPRSXP`. An ALTREP instance has no element
    /// storage, so its class's `Elt` method answers through the accessor the host installed.
    #[inline]
    pub unsafe fn element(self, index: usize) -> Sexp {
        unsafe {
            if self.is_altrep() {
                return self.altrep_element(index);
            }
            *(self.data() as *const Sexp).add(index)
        }
    }

    // Out of line, so that `element` is small enough to inline into the interpreter's loop.
    #[cold]
    #[inline(never)]
    unsafe fn altrep_element(self, index: usize) -> Sexp {
        unsafe {
            match ALTREP_ELEMENTS.get() {
                Some(elements) => (elements.read)(self, index),
                None => *(self.data() as *const Sexp).add(index),
            }
        }
    }

    /// An element of a vector the caller knows is not ALTREP.
    #[inline]
    pub(crate) unsafe fn stored_element(self, index: usize) -> Sexp {
        unsafe { *(self.data() as *const Sexp).add(index) }
    }

    #[inline]
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

    pub(crate) unsafe fn symbol_payload(self) -> &'static SymbolObject {
        unsafe { &*self.0.as_ptr().cast::<SymbolObject>() }
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

    // Records, tuples and tags. The caller guarantees the object has the record shape.

    pub unsafe fn record_layout(self) -> u32 {
        unsafe { self.payload::<RecordObject>().layout.get() }
    }

    pub unsafe fn record_slot_count(self) -> usize {
        unsafe { self.payload::<RecordObject>().slot_count.get() as usize }
    }

    pub(crate) unsafe fn record_unboxed(self) -> u64 {
        unsafe { self.payload::<RecordObject>().unboxed.get() }
    }

    /// Slot `index`, which must be below the slot count.
    pub unsafe fn record_slot(self, index: usize) -> Slot {
        unsafe {
            let bits = *(self.record_slots() as *const u64).add(index);
            if index < UNBOXED_SLOTS && self.record_unboxed() & (1 << index) != 0 {
                Slot::decode_raw(bits)
            } else {
                Slot::Handle(Sexp(NonNull::new_unchecked(bits as *mut Header)))
            }
        }
    }

    /// The handle in slot `index`, if the slot holds one.
    pub unsafe fn record_handle(self, index: usize) -> Option<Sexp> {
        match unsafe { self.record_slot(index) } {
            Slot::Handle(handle) => Some(handle),
            _ => None,
        }
    }

    unsafe fn record_slots(self) -> *mut u8 {
        unsafe { (self.0.as_ptr() as *mut u8).add(RECORD_DATA_OFFSET) }
    }

    pub(crate) unsafe fn set_record_slot_unbarriered(self, index: usize, slot: Slot) {
        unsafe {
            let payload = self.payload::<RecordObject>();
            if index < UNBOXED_SLOTS {
                let bit = 1u64 << index;
                let unboxed = payload.unboxed.get();
                payload.unboxed.set(if matches!(slot, Slot::Handle(_)) {
                    unboxed & !bit
                } else {
                    unboxed | bit
                });
            }
            *(self.record_slots() as *mut u64).add(index) = slot.encode();
        }
    }

    /// Fills a fresh record's header fields and slots. Nothing may read the record before.
    pub(crate) unsafe fn init_record(self, layout: u32, slots: &[Slot]) {
        unsafe {
            let payload = self.payload::<RecordObject>();
            payload.layout.set(layout);
            payload.slot_count.set(slots.len() as u32);
            let mut unboxed = 0u64;
            for (index, slot) in slots.iter().enumerate() {
                if !matches!(slot, Slot::Handle(_)) {
                    unboxed |= 1 << index;
                }
                *(self.record_slots() as *mut u64).add(index) = slot.encode();
            }
            payload.unboxed.set(unboxed);
        }
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
}
