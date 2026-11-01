//! ALTREP: vectors whose length, elements and data pointer come from a class's methods.
//!
//! An instance has the triple shape (data1, data2, class) with a vector type code and the
//! ALTREP header flag, so every accessor that reaches vector storage checks the flag and
//! dispatches here. A class is a raw vector whose bytes hold its method table and whose
//! attribute is the pairlist (class symbol, package symbol, type code), the layout packages
//! read through `ATTRIB(ALTREP_CLASS(x))`. Classes are preserved for the life of the session
//! and registered by (class symbol, package symbol), which is how unserialization finds them.
//!
//! A method slot is empty (GNU R's default behavior applies), holds native code, or holds one
//! of ry's own built-in implementations. Native methods run in a C context through the
//! per-signature trampolines in `native.c`; built-in methods are plain Rust and report errors
//! as `Err`, so the hot built-in classes (`compact_intseq` behind `1:n`) cost no `setjmp`.
//!
//! The built-in classes are GNU R's, under GNU R's class and package names, so inspection and
//! serialization agree: `compact_intseq` and `compact_realseq`, `deferred_string`, and the
//! `wrap_*` classes that attach sortedness and missing-value metadata to a vector.

use {
    crate::{
        Global, R_xlen_t, Rboolean,
        boundary::{ApiResult, Handout, Reachable, enter, error, with_native_scope, write_console},
        memory, runtime,
        vectors::{self, sexp_type},
    },
    heap::object::{Header, NA_INTEGER, Rcomplex, Sexp, SexpType},
    std::{
        convert::Infallible,
        ffi::{CStr, c_char, c_int, c_void},
    },
};

pub const UNKNOWN_SORTEDNESS: c_int = i32::MIN;
pub const SORTED_DECR: c_int = -1;
pub const SORTED_INCR: c_int = 1;

/// `R_altrep_class_t`, which C passes by value.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AltrepClass {
    pointer: Sexp,
}

impl Handout for AltrepClass {}

impl AltrepClass {
    /// The class object, the handle `ALTREP_CLASS` returns.
    pub fn handle(self) -> Sexp {
        self.pointer
    }
}

/// The callback an `Inspect` method uses to print a sub-object. It must not unwind.
pub type InspectSubtree = unsafe extern "C" fn(Sexp, c_int, c_int, c_int);

/// A `SEXP` that native code may return as C `NULL`.
type NullableSexp = *mut Header;

type NativeLength = unsafe extern "C" fn(Sexp) -> R_xlen_t;
type NativeDataptr = unsafe extern "C" fn(Sexp, Rboolean) -> *mut c_void;
type NativeDataptrOrNull = unsafe extern "C" fn(Sexp) -> *const c_void;
type NativeIntElt = unsafe extern "C" fn(Sexp, R_xlen_t) -> c_int;
type NativeRealElt = unsafe extern "C" fn(Sexp, R_xlen_t) -> f64;
type NativeRawElt = unsafe extern "C" fn(Sexp, R_xlen_t) -> u8;
type NativeComplexElt = unsafe extern "C" fn(Sexp, R_xlen_t) -> Rcomplex;
type NativeSexpElt = unsafe extern "C" fn(Sexp, R_xlen_t) -> NullableSexp;
type NativeSetElt = unsafe extern "C" fn(Sexp, R_xlen_t, Sexp);
type NativeRegion = unsafe extern "C" fn(Sexp, R_xlen_t, R_xlen_t, *mut c_void) -> R_xlen_t;
type NativeQuery = unsafe extern "C" fn(Sexp) -> c_int;
/// `Duplicate`, `DuplicateEX`, `Sum`, `Min` and `Max` share this shape.
type NativeWithFlag = unsafe extern "C" fn(Sexp, Rboolean) -> NullableSexp;
type NativeCoerce = unsafe extern "C" fn(Sexp, c_int) -> NullableSexp;
type NativeState = unsafe extern "C" fn(Sexp) -> NullableSexp;
type NativeUnserialize = unsafe extern "C" fn(Sexp, Sexp) -> NullableSexp;
type NativeUnserializeEx = unsafe extern "C" fn(Sexp, Sexp, Sexp, c_int, c_int) -> NullableSexp;
type NativeExtractSubset = unsafe extern "C" fn(Sexp, Sexp, Sexp) -> NullableSexp;
type NativeInspect = unsafe extern "C" fn(Sexp, c_int, c_int, c_int, InspectSubtree) -> Rboolean;

type BuiltinLength = unsafe fn(Sexp) -> ApiResult<R_xlen_t>;
type BuiltinDataptr = unsafe fn(Sexp, bool) -> ApiResult<*mut u8>;
type BuiltinDataptrOrNull = unsafe fn(Sexp) -> ApiResult<*const c_void>;
type BuiltinElt<T> = unsafe fn(Sexp, R_xlen_t) -> ApiResult<T>;
type BuiltinSetElt = unsafe fn(Sexp, R_xlen_t, Sexp) -> ApiResult<()>;
type BuiltinRegion = unsafe fn(Sexp, R_xlen_t, R_xlen_t, *mut c_void) -> ApiResult<R_xlen_t>;
type BuiltinQuery = unsafe fn(Sexp) -> ApiResult<c_int>;
type BuiltinWithFlag = unsafe fn(Sexp, bool) -> ApiResult<Option<Sexp>>;
type BuiltinCoerce = unsafe fn(Sexp, SexpType) -> ApiResult<Option<Sexp>>;
type BuiltinState = unsafe fn(Sexp) -> ApiResult<Option<Sexp>>;
type BuiltinUnserialize = unsafe fn(Sexp, Sexp) -> ApiResult<Sexp>;
type BuiltinInspect = unsafe fn(Sexp, c_int, c_int, c_int, InspectSubtree) -> ApiResult<bool>;

#[derive(Clone, Copy)]
enum Method<N, B> {
    Default,
    Native(N),
    Builtin(B),
}

/// A class's method table, stored in the class object's raw bytes. It holds no handles, so the
/// collector has nothing to trace in it.
#[derive(Clone, Copy)]
struct Methods {
    kind: SexpType,
    unserialize_ex: Method<NativeUnserializeEx, Infallible>,
    unserialize: Method<NativeUnserialize, BuiltinUnserialize>,
    serialized_state: Method<NativeState, BuiltinState>,
    duplicate_ex: Method<NativeWithFlag, Infallible>,
    duplicate: Method<NativeWithFlag, BuiltinWithFlag>,
    coerce: Method<NativeCoerce, BuiltinCoerce>,
    inspect: Method<NativeInspect, BuiltinInspect>,
    length: Method<NativeLength, BuiltinLength>,
    dataptr: Method<NativeDataptr, BuiltinDataptr>,
    dataptr_or_null: Method<NativeDataptrOrNull, BuiltinDataptrOrNull>,
    extract_subset: Method<NativeExtractSubset, Infallible>,
    /// The `Elt` method of an integer or logical class.
    int_elt: Method<NativeIntElt, BuiltinElt<i32>>,
    real_elt: Method<NativeRealElt, BuiltinElt<f64>>,
    raw_elt: Method<NativeRawElt, BuiltinElt<u8>>,
    complex_elt: Method<NativeComplexElt, BuiltinElt<Rcomplex>>,
    /// The `Elt` method of a string or list class.
    sexp_elt: Method<NativeSexpElt, BuiltinElt<Sexp>>,
    set_elt: Method<NativeSetElt, BuiltinSetElt>,
    get_region: Method<NativeRegion, BuiltinRegion>,
    is_sorted: Method<NativeQuery, BuiltinQuery>,
    no_na: Method<NativeQuery, BuiltinQuery>,
    sum: Method<NativeWithFlag, BuiltinWithFlag>,
    min: Method<NativeWithFlag, BuiltinWithFlag>,
    max: Method<NativeWithFlag, BuiltinWithFlag>,
}

impl Methods {
    fn new(kind: SexpType) -> Methods {
        Methods {
            kind,
            unserialize_ex: Method::Default,
            unserialize: Method::Default,
            serialized_state: Method::Default,
            duplicate_ex: Method::Default,
            duplicate: Method::Default,
            coerce: Method::Default,
            inspect: Method::Default,
            length: Method::Default,
            dataptr: Method::Default,
            dataptr_or_null: Method::Default,
            extract_subset: Method::Default,
            int_elt: Method::Default,
            real_elt: Method::Default,
            raw_elt: Method::Default,
            complex_elt: Method::Default,
            sexp_elt: Method::Default,
            set_elt: Method::Default,
            get_region: Method::Default,
            is_sorted: Method::Default,
            no_na: Method::Default,
            sum: Method::Default,
            min: Method::Default,
            max: Method::Default,
        }
    }
}

unsafe extern "C" {
    fn ry_altrep_length(method: NativeLength, x: Sexp, result: *mut R_xlen_t) -> c_int;
    fn ry_altrep_dataptr(
        method: NativeDataptr,
        x: Sexp,
        writable: c_int,
        result: *mut *mut c_void,
    ) -> c_int;
    fn ry_altrep_dataptr_or_null(
        method: NativeDataptrOrNull,
        x: Sexp,
        result: *mut *const c_void,
    ) -> c_int;
    fn ry_altrep_int_elt(
        method: NativeIntElt,
        x: Sexp,
        index: R_xlen_t,
        result: *mut c_int,
    ) -> c_int;
    fn ry_altrep_real_elt(
        method: NativeRealElt,
        x: Sexp,
        index: R_xlen_t,
        result: *mut f64,
    ) -> c_int;
    fn ry_altrep_raw_elt(method: NativeRawElt, x: Sexp, index: R_xlen_t, result: *mut u8) -> c_int;
    fn ry_altrep_complex_elt(
        method: NativeComplexElt,
        x: Sexp,
        index: R_xlen_t,
        result: *mut Rcomplex,
    ) -> c_int;
    fn ry_altrep_sexp_elt(
        method: NativeSexpElt,
        x: Sexp,
        index: R_xlen_t,
        result: *mut NullableSexp,
    ) -> c_int;
    fn ry_altrep_set_elt(method: NativeSetElt, x: Sexp, index: R_xlen_t, value: Sexp) -> c_int;
    fn ry_altrep_int_query(method: NativeQuery, x: Sexp, result: *mut c_int) -> c_int;
    fn ry_altrep_region(
        method: NativeRegion,
        x: Sexp,
        start: R_xlen_t,
        count: R_xlen_t,
        buffer: *mut c_void,
        result: *mut R_xlen_t,
    ) -> c_int;
    fn ry_altrep_with_flag(
        method: NativeWithFlag,
        x: Sexp,
        flag: c_int,
        result: *mut NullableSexp,
    ) -> c_int;
    fn ry_altrep_coerce(
        method: NativeCoerce,
        x: Sexp,
        kind: c_int,
        result: *mut NullableSexp,
    ) -> c_int;
    fn ry_altrep_state(method: NativeState, x: Sexp, result: *mut NullableSexp) -> c_int;
    fn ry_altrep_unserialize(
        method: NativeUnserialize,
        class: Sexp,
        state: Sexp,
        result: *mut NullableSexp,
    ) -> c_int;
    fn ry_altrep_unserialize_ex(
        method: NativeUnserializeEx,
        class: Sexp,
        state: Sexp,
        attributes: Sexp,
        object: c_int,
        levels: c_int,
        result: *mut NullableSexp,
    ) -> c_int;
    fn ry_altrep_extract_subset(
        method: NativeExtractSubset,
        x: Sexp,
        indices: Sexp,
        call: Sexp,
        result: *mut NullableSexp,
    ) -> c_int;
    fn ry_altrep_inspect(
        method: NativeInspect,
        x: Sexp,
        pre: c_int,
        deep: c_int,
        pvec: c_int,
        subtree: InspectSubtree,
        result: *mut c_int,
    ) -> c_int;
}

/// Runs a native method through its trampoline and returns what it wrote.
unsafe fn native<T>(initial: T, call: impl FnOnce(*mut T) -> c_int) -> ApiResult<T> {
    let mut result = initial;
    with_native_scope(|| call(&mut result))?;
    Ok(result)
}

unsafe fn methods<'a>(object: Sexp) -> &'a Methods {
    &*(object.tag().data() as *const Methods)
}

unsafe fn class_methods<'a>(class: AltrepClass) -> &'a mut Methods {
    &mut *(class.pointer.data() as *mut Methods)
}

// Dispatch. Every function takes an ALTREP instance unless it says otherwise.

pub unsafe fn length(object: Sexp) -> ApiResult<R_xlen_t> {
    match methods(object).length {
        Method::Native(method) => native(0, |result| ry_altrep_length(method, object, result)),
        Method::Builtin(method) => method(object),
        Method::Default => Err(class_error(object, "no ALTREP Length method defined")),
    }
}

/// `DATAPTR` and its read-only form. GNU R calls the class's `Dataptr` with `writable` set
/// for `INTEGER(x)` and the other writable accessors.
pub unsafe fn dataptr(object: Sexp, writable: bool) -> ApiResult<*mut u8> {
    let pointer = match methods(object).dataptr {
        Method::Native(method) => native(std::ptr::null_mut(), |result| {
            ry_altrep_dataptr(method, object, writable as c_int, result)
        })?
        .cast::<u8>(),
        Method::Builtin(method) => method(object, writable)?,
        Method::Default => {
            return Err(class_error(
                object,
                "cannot access data pointer for this ALTVEC object",
            ));
        }
    };
    if writable
        && matches!(
            object.kind(),
            SexpType::String | SexpType::List | SexpType::Expression
        )
    {
        escape_storage(object, pointer);
    }
    Ok(pointer)
}

/// A writable element pointer into a handle vector lets native code store without the write
/// barrier. Classes conventionally keep their materialized storage in data1 or data2, so the
/// vector the pointer points into is found there and rescanned from then on.
unsafe fn escape_storage(object: Sexp, pointer: *mut u8) {
    for slot in [object.car(), object.cdr()] {
        if slot.kind().is_vector() && !slot.is_altrep() && slot.data() == pointer {
            runtime().heap.escape(slot);
        }
    }
}

pub unsafe fn dataptr_or_null(object: Sexp) -> ApiResult<*const c_void> {
    match methods(object).dataptr_or_null {
        Method::Native(method) => native(std::ptr::null(), |result| {
            ry_altrep_dataptr_or_null(method, object, result)
        }),
        Method::Builtin(method) => method(object),
        Method::Default => Ok(std::ptr::null()),
    }
}

pub unsafe fn int_elt(object: Sexp, index: R_xlen_t) -> ApiResult<i32> {
    match methods(object).int_elt {
        Method::Native(method) => {
            native(0, |result| ry_altrep_int_elt(method, object, index, result))
        }
        Method::Builtin(method) => method(object, index),
        Method::Default => Ok(*(dataptr(object, true)? as *const i32).add(index as usize)),
    }
}

pub unsafe fn real_elt(object: Sexp, index: R_xlen_t) -> ApiResult<f64> {
    match methods(object).real_elt {
        Method::Native(method) => native(0.0, |result| {
            ry_altrep_real_elt(method, object, index, result)
        }),
        Method::Builtin(method) => method(object, index),
        Method::Default => Ok(*(dataptr(object, true)? as *const f64).add(index as usize)),
    }
}

pub unsafe fn raw_elt(object: Sexp, index: R_xlen_t) -> ApiResult<u8> {
    match methods(object).raw_elt {
        Method::Native(method) => {
            native(0, |result| ry_altrep_raw_elt(method, object, index, result))
        }
        Method::Builtin(method) => method(object, index),
        Method::Default => Ok(*dataptr(object, true)?.add(index as usize)),
    }
}

pub unsafe fn complex_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Rcomplex> {
    match methods(object).complex_elt {
        Method::Native(method) => native(Rcomplex::default(), |result| {
            ry_altrep_complex_elt(method, object, index, result)
        }),
        Method::Builtin(method) => method(object, index),
        Method::Default => Ok(*(dataptr(object, true)? as *const Rcomplex).add(index as usize)),
    }
}

/// An element of a string or list class. Neither has a default: GNU R requires the method.
pub unsafe fn sexp_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Sexp> {
    match methods(object).sexp_elt {
        Method::Native(method) => {
            let element = native(std::ptr::null_mut(), |result| {
                ry_altrep_sexp_elt(method, object, index, result)
            })?;
            Sexp::from_raw(element).ok_or_else(|| error("an ALTREP Elt method returned NULL"))
        }
        Method::Builtin(method) => method(object, index),
        Method::Default => Err(class_error(
            object,
            if object.kind() == SexpType::String {
                "No Elt method found for ALTSTRING class"
            } else {
                "ALTLIST classes must provide an Elt method"
            },
        )),
    }
}

pub unsafe fn set_sexp_elt(object: Sexp, index: R_xlen_t, value: Sexp) -> ApiResult<()> {
    match methods(object).set_elt {
        Method::Native(method) => {
            with_native_scope(|| ry_altrep_set_elt(method, object, index, value))
        }
        Method::Builtin(method) => method(object, index, value),
        Method::Default => Err(class_error(
            object,
            if object.kind() == SexpType::String {
                "No Set_elt found for ALTSTRING class"
            } else {
                "ALTLIST classes must provide a Set_elt method"
            },
        )),
    }
}

/// An error about a missing method, naming the class as GNU R does.
unsafe fn class_error(object: Sexp, message: &str) -> crate::boundary::Unwind {
    let info = object.tag().attrib();
    error(format!(
        "{message} [class: {}, pkg: {}]",
        symbol_text(info.car()),
        symbol_text(info.cdr().car())
    ))
}

/// An element of an atomic vector of type `T`, ALTREP or not.
pub unsafe fn elt<T: Element>(object: Sexp, index: R_xlen_t) -> ApiResult<T> {
    if object.is_altrep() {
        T::altrep_elt(object, index)
    } else {
        Ok(*(object.data() as *const T).add(index as usize))
    }
}

/// The element types of the atomic vectors, with their ALTREP `Elt` dispatch.
pub trait Element: Copy {
    unsafe fn altrep_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Self>;
}

impl Element for i32 {
    unsafe fn altrep_elt(object: Sexp, index: R_xlen_t) -> ApiResult<i32> {
        int_elt(object, index)
    }
}

impl Element for f64 {
    unsafe fn altrep_elt(object: Sexp, index: R_xlen_t) -> ApiResult<f64> {
        real_elt(object, index)
    }
}

impl Element for u8 {
    unsafe fn altrep_elt(object: Sexp, index: R_xlen_t) -> ApiResult<u8> {
        raw_elt(object, index)
    }
}

impl Element for Rcomplex {
    unsafe fn altrep_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Rcomplex> {
        complex_elt(object, index)
    }
}

/// `INTEGER_GET_REGION` and its relatives, for any vector: copies up to `count` elements
/// starting at `start` into `buffer` and returns how many it copied. As in GNU R, a vector
/// whose data pointer is available is copied directly, and only otherwise does the class's
/// `Get_region` run, which by default reads element by element.
pub unsafe fn get_region<T: Element>(
    object: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut T,
) -> ApiResult<R_xlen_t> {
    let size = vectors::xlength(object)?;
    // A start outside the vector copies nothing rather than reading before or past its data.
    if start < 0 || start > size || count < 0 {
        return Ok(0);
    }
    let copied = count.min(size - start);
    let source = if object.is_altrep() {
        dataptr_or_null(object)? as *const T
    } else {
        object.data() as *const T
    };
    if !source.is_null() {
        std::ptr::copy_nonoverlapping(source.add(start as usize), buffer, copied as usize);
        return Ok(copied);
    }
    match methods(object).get_region {
        Method::Native(method) => native(0, |result| {
            ry_altrep_region(method, object, start, count, buffer.cast(), result)
        }),
        Method::Builtin(method) => method(object, start, count, buffer.cast()),
        Method::Default => {
            for index in 0..copied {
                *buffer.add(index as usize) = T::altrep_elt(object, start + index)?;
            }
            Ok(copied)
        }
    }
}

/// `*_IS_SORTED`: `UNKNOWN_SORTEDNESS` for an ordinary vector or a class that does not say.
pub unsafe fn is_sorted(object: Sexp) -> ApiResult<c_int> {
    if !object.is_altrep() {
        return Ok(UNKNOWN_SORTEDNESS);
    }
    query(object, methods(object).is_sorted, UNKNOWN_SORTEDNESS)
}

/// `*_NO_NA`: 1 only when the class knows the vector has no missing values.
pub unsafe fn no_na(object: Sexp) -> ApiResult<c_int> {
    if !object.is_altrep() {
        return Ok(0);
    }
    query(object, methods(object).no_na, 0)
}

unsafe fn query(
    object: Sexp,
    method: Method<NativeQuery, BuiltinQuery>,
    default: c_int,
) -> ApiResult<c_int> {
    match method {
        Method::Native(method) => native(default, |result| {
            ry_altrep_int_query(method, object, result)
        }),
        Method::Builtin(method) => method(object),
        Method::Default => Ok(default),
    }
}

/// `ALTINTEGER_SUM` and friends for the interpreter's `sum()`: `None` when the vector is not
/// ALTREP or its class cannot compute the answer cheaply, and the caller computes it.
pub unsafe fn sum(object: Sexp, remove_na: bool) -> ApiResult<Option<Sexp>> {
    summary(object, |methods| methods.sum, remove_na)
}

pub unsafe fn min(object: Sexp, remove_na: bool) -> ApiResult<Option<Sexp>> {
    summary(object, |methods| methods.min, remove_na)
}

pub unsafe fn max(object: Sexp, remove_na: bool) -> ApiResult<Option<Sexp>> {
    summary(object, |methods| methods.max, remove_na)
}

unsafe fn summary(
    object: Sexp,
    pick: impl Fn(&Methods) -> Method<NativeWithFlag, BuiltinWithFlag>,
    remove_na: bool,
) -> ApiResult<Option<Sexp>> {
    if !object.is_altrep() {
        return Ok(None);
    }
    with_flag(object, pick(methods(object)), remove_na)
}

unsafe fn with_flag(
    object: Sexp,
    method: Method<NativeWithFlag, BuiltinWithFlag>,
    flag: bool,
) -> ApiResult<Option<Sexp>> {
    match method {
        Method::Native(method) => {
            let result = native(std::ptr::null_mut(), |result| {
                ry_altrep_with_flag(method, object, flag as c_int, result)
            })?;
            Ok(Sexp::from_raw(result))
        }
        Method::Builtin(method) => method(object, flag),
        Method::Default => Ok(None),
    }
}

/// `DuplicateEX`: the class's own copy, or `None` for the default materializing copy. A copy
/// made by `Duplicate` receives the attributes here, as GNU R's default `DuplicateEX` gives it.
pub unsafe fn duplicate(object: Sexp, deep: bool) -> ApiResult<Option<Sexp>> {
    if let Method::Native(method) = methods(object).duplicate_ex {
        let result = native(std::ptr::null_mut(), |result| {
            ry_altrep_with_flag(method, object, deep as c_int, result)
        })?;
        return Ok(Sexp::from_raw(result));
    }
    let Some(copy) = with_flag(object, methods(object).duplicate, deep)? else {
        return Ok(None);
    };
    if copy != object {
        runtime().heap.root(copy);
        memory::copy_attributes(copy, object, deep)?;
    }
    Ok(Some(copy))
}

/// The class's `Coerce`, which `Rf_coerceVector` tries first; `None` means the default.
pub unsafe fn coerce(object: Sexp, target: SexpType) -> ApiResult<Option<Sexp>> {
    match methods(object).coerce {
        Method::Native(method) => {
            let result = native(std::ptr::null_mut(), |result| {
                ry_altrep_coerce(method, object, target.code() as c_int, result)
            })?;
            Ok(Sexp::from_raw(result))
        }
        Method::Builtin(method) => method(object, target),
        Method::Default => Ok(None),
    }
}

/// The class's `Extract_subset` for `[`: `None` means the interpreter subsets as usual.
pub unsafe fn extract_subset(object: Sexp, indices: Sexp, call: Sexp) -> ApiResult<Option<Sexp>> {
    match methods(object).extract_subset {
        Method::Native(method) => {
            let result = native(std::ptr::null_mut(), |result| {
                ry_altrep_extract_subset(method, object, indices, call, result)
            })?;
            Ok(Sexp::from_raw(result))
        }
        Method::Builtin(never) => match never {},
        Method::Default => Ok(None),
    }
}

/// The class's part of `.Internal(inspect(x))`: `false` when the class has nothing to say and
/// the caller prints the object as an ordinary vector.
pub unsafe fn inspect(
    object: Sexp,
    pre: c_int,
    deep: c_int,
    pvec: c_int,
    subtree: InspectSubtree,
) -> ApiResult<bool> {
    match methods(object).inspect {
        Method::Native(method) => {
            let shown = native(0, |result| {
                ry_altrep_inspect(method, object, pre, deep, pvec, subtree, result)
            })?;
            Ok(shown != 0)
        }
        Method::Builtin(method) => method(object, pre, deep, pvec, subtree),
        Method::Default => Ok(false),
    }
}

// Serialization.

/// `ALTREP_SERIALIZED_CLASS`: the pairlist (class symbol, package symbol, type code) the
/// serializer writes before the state, or `None` for an ordinary vector.
pub unsafe fn serialized_class(object: Sexp) -> Option<Sexp> {
    object.is_altrep().then(|| object.tag().attrib())
}

/// The class's `Serialized_state`: `None` means the object is serialized as an ordinary
/// vector.
pub unsafe fn serialized_state(object: Sexp) -> ApiResult<Option<Sexp>> {
    match methods(object).serialized_state {
        Method::Native(method) => {
            let result = native(std::ptr::null_mut(), |result| {
                ry_altrep_state(method, object, result)
            })?;
            Ok(Sexp::from_raw(result))
        }
        Method::Builtin(method) => method(object),
        Method::Default => Ok(None),
    }
}

/// Rebuilds an ALTREP object from what the serializer read: the class pairlist, the state,
/// the attributes and the header's object bit and levels. `None` means no class with that
/// name and package is registered, so the caller can load the package's namespace and retry,
/// or fall back as GNU R does.
pub unsafe fn unserialize(
    info: Sexp,
    state: Sexp,
    attributes: Sexp,
    object_bit: c_int,
    levels: c_int,
) -> ApiResult<Option<Sexp>> {
    for handle in [info, state, attributes] {
        runtime().heap.root(handle);
    }
    let class_symbol = info.car();
    let package_symbol = info.cdr().car();
    let Some(class) = find_class(class_symbol, package_symbol)? else {
        return Ok(None);
    };
    let serialized_kind = info.cdr().cdr().car();
    let wanted = vectors::int_elt(serialized_kind, 0)?;
    let registered = class_methods(class).kind;
    if wanted != registered.code() as i32 {
        return Err(error(format!(
            "serialized class '{}' from package '{}' has type {}; registered class has type {}",
            symbol_text(class_symbol),
            symbol_text(package_symbol),
            sexp_type(wanted as u32).map_or("unknown", SexpType::name),
            registered.name()
        )));
    }
    let methods = *class_methods(class);
    if let Method::Native(method) = methods.unserialize_ex {
        let result = native(std::ptr::null_mut(), |result| {
            ry_altrep_unserialize_ex(
                method,
                class.pointer,
                state,
                attributes,
                object_bit,
                levels,
                result,
            )
        })?;
        return Sexp::from_raw(result)
            .map(Some)
            .ok_or_else(|| error("an ALTREP UnserializeEX method returned NULL"));
    }
    let object = match methods.unserialize {
        Method::Native(method) => {
            let result = native(std::ptr::null_mut(), |result| {
                ry_altrep_unserialize(method, class.pointer, state, result)
            })?;
            Sexp::from_raw(result)
                .ok_or_else(|| error("an ALTREP Unserialize method returned NULL"))?
        }
        Method::Builtin(method) => method(class.pointer, state)?,
        Method::Default => return Err(error("cannot unserialize this ALTREP object yet")),
    };
    runtime().heap.root(object);
    runtime().heap.set_attrib(object, attributes);
    object.set_gp(levels as u16);
    object.set_object(object_bit != 0);
    Ok(Some(object))
}

unsafe fn symbol_text(symbol: Sexp) -> String {
    if symbol.kind() == SexpType::Symbol {
        String::from_utf8_lossy(symbol.car().bytes()).into_owned()
    } else {
        String::new()
    }
}

// Classes and the registry.

static REGISTRY: Global<Vec<Sexp>> = Global::new(Vec::new());
static BUILTINS: Global<Option<Builtins>> = Global::new(None);

fn registry<'a>() -> &'a mut Vec<Sexp> {
    // SAFETY: the API is single-threaded, see `Global`.
    unsafe { &mut *REGISTRY.as_ptr() }
}

/// The registered class with this class name and package, both symbols. ry's built-in classes
/// are registered under package `base`.
pub unsafe fn find_class(
    class_symbol: Sexp,
    package_symbol: Sexp,
) -> ApiResult<Option<AltrepClass>> {
    builtins()?;
    Ok(lookup(class_symbol, package_symbol).map(|pointer| AltrepClass { pointer }))
}

unsafe fn lookup(class_symbol: Sexp, package_symbol: Sexp) -> Option<Sexp> {
    registry().iter().copied().find(|class| {
        let info = class.attrib();
        info.car() == class_symbol && info.cdr().car() == package_symbol
    })
}

/// Registers a class, or reinitializes the registered class with the same name, package and
/// type, as GNU R does when a package is loaded again: its instances then use the new methods.
unsafe fn make_class(name: &[u8], package: &[u8], kind: SexpType) -> ApiResult<AltrepClass> {
    let mark = runtime().heap.handle_mark();
    let result = register(name, package, kind);
    runtime().heap.release_handles(mark);
    result
}

unsafe fn register(name: &[u8], package: &[u8], kind: SexpType) -> ApiResult<AltrepClass> {
    let heap = &mut runtime().heap;
    let class_symbol = heap.install(name)?;
    let package_symbol = heap.install(package)?;
    let existing = lookup(class_symbol, package_symbol);
    if let Some(pointer) = existing {
        let class = AltrepClass { pointer };
        if class_methods(class).kind == kind {
            *class_methods(class) = Methods::new(kind);
            return Ok(class);
        }
    }
    let pointer = heap.alloc_vector(SexpType::Raw, size_of::<Methods>() as isize)?;
    heap.root(pointer);
    std::ptr::write(pointer.data() as *mut Methods, Methods::new(kind));
    let code = heap.alloc_vector(SexpType::Integer, 1)?;
    *(code.data() as *mut i32) = kind.code() as i32;
    let nil = heap.nil();
    let info = heap.cons(code, nil)?;
    heap.root(info);
    let info = heap.cons(package_symbol, info)?;
    heap.root(info);
    let info = heap.cons(class_symbol, info)?;
    heap.set_attrib(pointer, info);
    heap.preserve(pointer);
    match registry()
        .iter_mut()
        .find(|class| Some(**class) == existing)
    {
        Some(slot) => *slot = pointer,
        None => registry().push(pointer),
    }
    Ok(AltrepClass { pointer })
}

macro_rules! class_constructor {
    ($name:ident, $kind:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            class: *const c_char,
            package: *const c_char,
            _dll: *mut c_void,
        ) -> AltrepClass {
            enter(|| {
                builtins()?;
                make_class(
                    CStr::from_ptr(class).to_bytes(),
                    CStr::from_ptr(package).to_bytes(),
                    $kind,
                )
            })
        }
    };
}

class_constructor!(R_make_altinteger_class, SexpType::Integer);
class_constructor!(R_make_altreal_class, SexpType::Real);
class_constructor!(R_make_altlogical_class, SexpType::Logical);
class_constructor!(R_make_altraw_class, SexpType::Raw);
class_constructor!(R_make_altcomplex_class, SexpType::Complex);
class_constructor!(R_make_altstring_class, SexpType::String);
class_constructor!(R_make_altlist_class, SexpType::List);

macro_rules! method_setter {
    ($name:ident, $field:ident, $type:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(class: AltrepClass, method: Option<$type>) {
            class_methods(class).$field = method.map_or(Method::Default, Method::Native);
        }
    };
}

method_setter!(
    R_set_altrep_UnserializeEX_method,
    unserialize_ex,
    NativeUnserializeEx
);
method_setter!(
    R_set_altrep_Unserialize_method,
    unserialize,
    NativeUnserialize
);
method_setter!(
    R_set_altrep_Serialized_state_method,
    serialized_state,
    NativeState
);
method_setter!(
    R_set_altrep_DuplicateEX_method,
    duplicate_ex,
    NativeWithFlag
);
method_setter!(R_set_altrep_Duplicate_method, duplicate, NativeWithFlag);
method_setter!(R_set_altrep_Coerce_method, coerce, NativeCoerce);
method_setter!(R_set_altrep_Inspect_method, inspect, NativeInspect);
method_setter!(R_set_altrep_Length_method, length, NativeLength);
method_setter!(R_set_altvec_Dataptr_method, dataptr, NativeDataptr);
method_setter!(
    R_set_altvec_Dataptr_or_null_method,
    dataptr_or_null,
    NativeDataptrOrNull
);
method_setter!(
    R_set_altvec_Extract_subset_method,
    extract_subset,
    NativeExtractSubset
);
method_setter!(R_set_altinteger_Elt_method, int_elt, NativeIntElt);
method_setter!(R_set_altinteger_Get_region_method, get_region, NativeRegion);
method_setter!(R_set_altinteger_Is_sorted_method, is_sorted, NativeQuery);
method_setter!(R_set_altinteger_No_NA_method, no_na, NativeQuery);
method_setter!(R_set_altinteger_Sum_method, sum, NativeWithFlag);
method_setter!(R_set_altinteger_Min_method, min, NativeWithFlag);
method_setter!(R_set_altinteger_Max_method, max, NativeWithFlag);
method_setter!(R_set_altreal_Elt_method, real_elt, NativeRealElt);
method_setter!(R_set_altreal_Get_region_method, get_region, NativeRegion);
method_setter!(R_set_altreal_Is_sorted_method, is_sorted, NativeQuery);
method_setter!(R_set_altreal_No_NA_method, no_na, NativeQuery);
method_setter!(R_set_altreal_Sum_method, sum, NativeWithFlag);
method_setter!(R_set_altreal_Min_method, min, NativeWithFlag);
method_setter!(R_set_altreal_Max_method, max, NativeWithFlag);
method_setter!(R_set_altlogical_Elt_method, int_elt, NativeIntElt);
method_setter!(R_set_altlogical_Get_region_method, get_region, NativeRegion);
method_setter!(R_set_altlogical_Is_sorted_method, is_sorted, NativeQuery);
method_setter!(R_set_altlogical_No_NA_method, no_na, NativeQuery);
method_setter!(R_set_altlogical_Sum_method, sum, NativeWithFlag);
method_setter!(R_set_altraw_Elt_method, raw_elt, NativeRawElt);
method_setter!(R_set_altraw_Get_region_method, get_region, NativeRegion);
method_setter!(R_set_altcomplex_Elt_method, complex_elt, NativeComplexElt);
method_setter!(R_set_altcomplex_Get_region_method, get_region, NativeRegion);
method_setter!(R_set_altstring_Elt_method, sexp_elt, NativeSexpElt);
method_setter!(R_set_altstring_Set_elt_method, set_elt, NativeSetElt);
method_setter!(R_set_altstring_Is_sorted_method, is_sorted, NativeQuery);
method_setter!(R_set_altstring_No_NA_method, no_na, NativeQuery);
method_setter!(R_set_altlist_Elt_method, sexp_elt, NativeSexpElt);
method_setter!(R_set_altlist_Set_elt_method, set_elt, NativeSetElt);

/// An instance of `class`. The class's type decides the instance's type.
pub unsafe fn new_altrep(class: AltrepClass, data1: Sexp, data2: Sexp) -> ApiResult<Sexp> {
    let kind = class_methods(class).kind;
    let object = runtime()
        .heap
        .alloc_altrep(kind, class.pointer, data1, data2)?;
    // The interpreter then copies an instance (into ordinary storage) before it modifies one,
    // instead of storing through a `Set_elt` method that most classes do not provide.
    object.mark_not_mutable();
    runtime().heap.root(object);
    Ok(object)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_new_altrep(class: AltrepClass, data1: Sexp, data2: Sexp) -> Sexp {
    enter(|| new_altrep(class, data1, data2))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTREP(object: Sexp) -> c_int {
    object.is_altrep() as c_int
}

unsafe fn instance(object: Sexp) -> ApiResult<Sexp> {
    if !object.is_altrep() {
        return Err(error("not an ALTREP object"));
    }
    Ok(object)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_data1(object: Sexp) -> Sexp {
    enter(|| Ok(Reachable(instance(object)?.car()))).0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_data2(object: Sexp) -> Sexp {
    enter(|| Ok(Reachable(instance(object)?.cdr()))).0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_set_altrep_data1(object: Sexp, value: Sexp) {
    enter(|| {
        runtime().heap.set_car(instance(object)?, value);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_set_altrep_data2(object: Sexp, value: Sexp) {
    enter(|| {
        runtime().heap.set_cdr(instance(object)?, value);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTREP_CLASS(object: Sexp) -> Sexp {
    enter(|| Ok(Reachable(instance(object)?.tag()))).0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_inherits(object: Sexp, class: AltrepClass) -> Rboolean {
    (object.is_altrep() && object.tag() == class.pointer) as Rboolean
}

/// The class symbol of an ALTREP object, or `R_NilValue` for an ordinary one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_class_name(object: Sexp) -> Sexp {
    match serialized_class(object) {
        Some(info) => info.car(),
        None => runtime().nil(),
    }
}

/// The package symbol of an ALTREP object's class, or `R_NilValue` for an ordinary object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_class_package(object: Sexp) -> Sexp {
    match serialized_class(object) {
        Some(info) => info.cdr().car(),
        None => runtime().nil(),
    }
}

/// A wrapper around `x` with unknown metadata when its type has a wrapper class, and `x`
/// itself otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_tryWrap(object: Sexp) -> Sexp {
    enter(|| wrap(object, UNKNOWN_SORTEDNESS, 0))
}

/// `n1:n2` as a compact sequence, for an embedding host. GNU R keeps the same function
/// internal, so it carries ry's prefix.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_compact_intrange(first: R_xlen_t, last: R_xlen_t) -> Sexp {
    enter(|| compact_intrange(first, last))
}

// GNU R's libR exports its dispatch functions too, and binaries built against older headers,
// whose inline accessors called them, import them.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTREP_LENGTH(object: Sexp) -> R_xlen_t {
    enter(|| length(object))
}

/// ALTREP objects have no spare capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTREP_TRUELENGTH(_object: Sexp) -> R_xlen_t {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTVEC_DATAPTR(object: Sexp) -> *mut c_void {
    enter(|| Ok(dataptr(object, true)?.cast::<c_void>()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTVEC_DATAPTR_RO(object: Sexp) -> *const c_void {
    enter(|| Ok(dataptr(object, false)? as *const c_void))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTVEC_DATAPTR_OR_NULL(object: Sexp) -> *const c_void {
    enter(|| dataptr_or_null(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTSTRING_ELT(object: Sexp, index: R_xlen_t) -> Sexp {
    enter(|| sexp_elt(object, index))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ALTLIST_ELT(object: Sexp, index: R_xlen_t) -> Sexp {
    enter(|| sexp_elt(object, index))
}

macro_rules! set_elt_api {
    ($name:ident, $type:ty) => {
        /// Writes through the class's writable data pointer, as GNU R does.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp, index: R_xlen_t, value: $type) {
            enter(|| {
                *(dataptr(object, true)? as *mut $type).add(index as usize) = value;
                Ok(())
            })
        }
    };
}

set_elt_api!(ALTINTEGER_SET_ELT, c_int);
set_elt_api!(ALTLOGICAL_SET_ELT, c_int);
set_elt_api!(ALTREAL_SET_ELT, f64);
set_elt_api!(ALTRAW_SET_ELT, u8);
set_elt_api!(ALTCOMPLEX_SET_ELT, Rcomplex);

macro_rules! summary_api {
    ($name:ident, $function:ident) => {
        /// The class's answer, or C `NULL` when it has none.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp, remove_na: Rboolean) -> Option<Sexp> {
            enter(|| $function(object, remove_na != 0))
        }
    };
}

summary_api!(ALTINTEGER_SUM, sum);
summary_api!(ALTINTEGER_MIN, min);
summary_api!(ALTINTEGER_MAX, max);
summary_api!(ALTREAL_SUM, sum);
summary_api!(ALTREAL_MIN, min);
summary_api!(ALTREAL_MAX, max);
summary_api!(ALTLOGICAL_SUM, sum);

// The built-in classes.

#[derive(Clone, Copy)]
struct Builtins {
    compact_intseq: AltrepClass,
    compact_realseq: AltrepClass,
    deferred_string: AltrepClass,
    wrap_integer: AltrepClass,
    wrap_logical: AltrepClass,
    wrap_real: AltrepClass,
    wrap_complex: AltrepClass,
    wrap_raw: AltrepClass,
    wrap_string: AltrepClass,
}

unsafe fn builtins() -> ApiResult<Builtins> {
    if let Some(builtins) = BUILTINS.get() {
        return Ok(builtins);
    }
    let compact_intseq = make_class(b"compact_intseq", b"base", SexpType::Integer)?;
    {
        let methods = class_methods(compact_intseq);
        methods.length = Method::Builtin(sequence_length);
        methods.int_elt = Method::Builtin(intseq_elt);
        methods.dataptr = Method::Builtin(intseq_dataptr);
        methods.dataptr_or_null = Method::Builtin(sequence_dataptr_or_null);
        methods.get_region = Method::Builtin(intseq_region);
        methods.is_sorted = Method::Builtin(sequence_is_sorted);
        methods.no_na = Method::Builtin(sequence_no_na);
        methods.sum = Method::Builtin(intseq_sum);
        methods.coerce = Method::Builtin(intseq_coerce);
        methods.inspect = Method::Builtin(intseq_inspect);
        methods.serialized_state = Method::Builtin(sequence_state);
        methods.unserialize = Method::Builtin(intseq_unserialize);
    }
    let compact_realseq = make_class(b"compact_realseq", b"base", SexpType::Real)?;
    {
        let methods = class_methods(compact_realseq);
        methods.length = Method::Builtin(sequence_length);
        methods.real_elt = Method::Builtin(realseq_elt);
        methods.dataptr = Method::Builtin(realseq_dataptr);
        methods.dataptr_or_null = Method::Builtin(sequence_dataptr_or_null);
        methods.get_region = Method::Builtin(realseq_region);
        methods.is_sorted = Method::Builtin(sequence_is_sorted);
        methods.no_na = Method::Builtin(sequence_no_na);
        methods.sum = Method::Builtin(realseq_sum);
        methods.inspect = Method::Builtin(realseq_inspect);
        methods.serialized_state = Method::Builtin(sequence_state);
        methods.unserialize = Method::Builtin(realseq_unserialize);
    }
    let deferred_string = make_class(b"deferred_string", b"base", SexpType::String)?;
    {
        let methods = class_methods(deferred_string);
        methods.length = Method::Builtin(deferred_length);
        methods.sexp_elt = Method::Builtin(deferred_elt);
        methods.set_elt = Method::Builtin(deferred_set_elt);
        methods.dataptr = Method::Builtin(deferred_dataptr);
        methods.dataptr_or_null = Method::Builtin(deferred_dataptr_or_null);
        methods.no_na = Method::Builtin(deferred_no_na);
        methods.inspect = Method::Builtin(deferred_inspect);
        methods.serialized_state = Method::Builtin(deferred_state);
        methods.unserialize = Method::Builtin(deferred_unserialize);
    }
    let wrapper = |name: &[u8], kind: SexpType| -> ApiResult<AltrepClass> {
        let class = make_class(name, b"base", kind)?;
        let methods = class_methods(class);
        methods.length = Method::Builtin(wrapper_length);
        methods.dataptr = Method::Builtin(wrapper_dataptr);
        methods.dataptr_or_null = Method::Builtin(wrapper_dataptr_or_null);
        methods.is_sorted = Method::Builtin(wrapper_is_sorted);
        methods.no_na = Method::Builtin(wrapper_no_na);
        methods.duplicate = Method::Builtin(wrapper_duplicate);
        methods.inspect = Method::Builtin(wrapper_inspect);
        methods.serialized_state = Method::Builtin(wrapper_state);
        methods.unserialize = Method::Builtin(wrapper_unserialize);
        match kind {
            SexpType::Integer | SexpType::Logical => {
                methods.int_elt = Method::Builtin(wrapper_elt::<i32>);
                methods.get_region = Method::Builtin(wrapper_region::<i32>);
            }
            SexpType::Real => {
                methods.real_elt = Method::Builtin(wrapper_elt::<f64>);
                methods.get_region = Method::Builtin(wrapper_region::<f64>);
            }
            SexpType::Complex => {
                methods.complex_elt = Method::Builtin(wrapper_elt::<Rcomplex>);
                methods.get_region = Method::Builtin(wrapper_region::<Rcomplex>);
            }
            SexpType::Raw => {
                methods.raw_elt = Method::Builtin(wrapper_elt::<u8>);
                methods.get_region = Method::Builtin(wrapper_region::<u8>);
            }
            _ => {
                methods.sexp_elt = Method::Builtin(wrapper_sexp_elt);
                methods.set_elt = Method::Builtin(wrapper_set_elt);
            }
        }
        Ok(class)
    };
    let builtins = Builtins {
        compact_intseq,
        compact_realseq,
        deferred_string,
        wrap_integer: wrapper(b"wrap_integer", SexpType::Integer)?,
        wrap_logical: wrapper(b"wrap_logical", SexpType::Logical)?,
        wrap_real: wrapper(b"wrap_real", SexpType::Real)?,
        wrap_complex: wrapper(b"wrap_complex", SexpType::Complex)?,
        wrap_raw: wrapper(b"wrap_raw", SexpType::Raw)?,
        wrap_string: wrapper(b"wrap_string", SexpType::String)?,
    };
    BUILTINS.set(Some(builtins));
    Ok(builtins)
}

// Compact sequences. data1 is the double vector (length, first, increment), so a long
// sequence fits; data2 is `NULL` until the sequence is expanded into an ordinary vector, which
// happens when native code asks for its data pointer.

/// `n1:n2`: a compact integer sequence, or a compact double sequence when an end lies outside
/// the integer range.
pub unsafe fn compact_intrange(first: R_xlen_t, last: R_xlen_t) -> ApiResult<Sexp> {
    let length = (last - first).abs() + 1;
    let increment = if first <= last { 1 } else { -1 };
    let fits = |value: R_xlen_t| value > i32::MIN as R_xlen_t && value <= i32::MAX as R_xlen_t;
    if fits(first) && fits(last) {
        compact_intseq(length, first as i32, increment)
    } else {
        compact_realseq(length, first as f64, increment as f64)
    }
}

/// The integers `first`, `first + increment`, ... of length `length`. Only increments of 1
/// and -1 are supported, as in GNU R, and a sequence of length one is an ordinary scalar.
pub unsafe fn compact_intseq(length: R_xlen_t, first: i32, increment: i32) -> ApiResult<Sexp> {
    if length == 1 {
        return memory::scalar(SexpType::Integer, first);
    }
    if increment != 1 && increment != -1 {
        return Err(error(format!(
            "compact sequences with increment {increment} not supported yet"
        )));
    }
    let last = first as i64 + increment as i64 * (length as i64 - 1);
    if length < 0 || last <= i32::MIN as i64 || last > i32::MAX as i64 {
        return Err(error("compact integer sequence out of the integer range"));
    }
    new_sequence(
        builtins()?.compact_intseq,
        length,
        first as f64,
        increment as f64,
    )
}

/// The doubles `first`, `first + increment`, ... of length `length`, with an increment of 1
/// or -1. A sequence of length one is an ordinary scalar.
pub unsafe fn compact_realseq(length: R_xlen_t, first: f64, increment: f64) -> ApiResult<Sexp> {
    if length == 1 {
        return memory::scalar(SexpType::Real, first);
    }
    if increment != 1.0 && increment != -1.0 {
        return Err(error(format!(
            "compact sequences with increment {increment} not supported yet"
        )));
    }
    if length < 0 {
        return Err(error("negative length for a compact sequence"));
    }
    new_sequence(builtins()?.compact_realseq, length, first, increment)
}

unsafe fn new_sequence(
    class: AltrepClass,
    length: R_xlen_t,
    first: f64,
    increment: f64,
) -> ApiResult<Sexp> {
    let info = runtime().heap.alloc_vector(SexpType::Real, 3)?;
    runtime().heap.root(info);
    info.slice_mut::<f64>()
        .copy_from_slice(&[length as f64, first, increment]);
    let nil = runtime().nil();
    new_altrep(class, info, nil)
}

unsafe fn sequence_info(object: Sexp) -> (R_xlen_t, f64, f64) {
    let info = object.car().slice::<f64>();
    (info[0] as R_xlen_t, info[1], info[2])
}

unsafe fn expanded(object: Sexp) -> Option<Sexp> {
    let data = object.cdr();
    (data.kind() != SexpType::Nil).then_some(data)
}

unsafe fn sequence_length(object: Sexp) -> ApiResult<R_xlen_t> {
    Ok(sequence_info(object).0)
}

unsafe fn intseq_elt(object: Sexp, index: R_xlen_t) -> ApiResult<i32> {
    if let Some(data) = expanded(object) {
        return Ok(*(data.data() as *const i32).add(index as usize));
    }
    let (_, first, increment) = sequence_info(object);
    Ok((first + increment * index as f64) as i32)
}

unsafe fn realseq_elt(object: Sexp, index: R_xlen_t) -> ApiResult<f64> {
    if let Some(data) = expanded(object) {
        return Ok(*(data.data() as *const f64).add(index as usize));
    }
    let (_, first, increment) = sequence_info(object);
    Ok(first + increment * index as f64)
}

unsafe fn expand_sequence(object: Sexp, kind: SexpType) -> ApiResult<Sexp> {
    if let Some(data) = expanded(object) {
        return Ok(data);
    }
    let (length, first, increment) = sequence_info(object);
    runtime().heap.root(object);
    let data = runtime().heap.alloc_vector(kind, length)?;
    if kind == SexpType::Integer {
        for (index, slot) in data.slice_mut::<i32>().iter_mut().enumerate() {
            *slot = (first + increment * index as f64) as i32;
        }
    } else {
        for (index, slot) in data.slice_mut::<f64>().iter_mut().enumerate() {
            *slot = first + increment * index as f64;
        }
    }
    runtime().heap.set_cdr(object, data);
    Ok(data)
}

unsafe fn intseq_dataptr(object: Sexp, _writable: bool) -> ApiResult<*mut u8> {
    Ok(expand_sequence(object, SexpType::Integer)?.data())
}

unsafe fn realseq_dataptr(object: Sexp, _writable: bool) -> ApiResult<*mut u8> {
    Ok(expand_sequence(object, SexpType::Real)?.data())
}

unsafe fn sequence_dataptr_or_null(object: Sexp) -> ApiResult<*const c_void> {
    Ok(expanded(object).map_or(std::ptr::null(), |data| data.data() as *const c_void))
}

unsafe fn intseq_region(
    object: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut c_void,
) -> ApiResult<R_xlen_t> {
    sequence_region::<i32>(object, start, count, buffer, |value| value as i32)
}

unsafe fn realseq_region(
    object: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut c_void,
) -> ApiResult<R_xlen_t> {
    sequence_region::<f64>(object, start, count, buffer, |value| value)
}

unsafe fn sequence_region<T>(
    object: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut c_void,
    convert: impl Fn(f64) -> T,
) -> ApiResult<R_xlen_t> {
    let (length, first, increment) = sequence_info(object);
    if start < 0 || start > length || count < 0 {
        return Ok(0);
    }
    let copied = count.min(length - start);
    let buffer = buffer as *mut T;
    for index in 0..copied {
        *buffer.add(index as usize) = convert(first + increment * (start + index) as f64);
    }
    Ok(copied)
}

/// Expansion hands out a writable pointer, so an expanded sequence no longer vouches for its
/// order or its missing values.
unsafe fn sequence_is_sorted(object: Sexp) -> ApiResult<c_int> {
    if expanded(object).is_some() {
        return Ok(UNKNOWN_SORTEDNESS);
    }
    Ok(if sequence_info(object).2 < 0.0 {
        SORTED_DECR
    } else {
        SORTED_INCR
    })
}

unsafe fn sequence_no_na(object: Sexp) -> ApiResult<c_int> {
    Ok(expanded(object).is_none() as c_int)
}

unsafe fn sequence_total(object: Sexp) -> f64 {
    let (length, first, increment) = sequence_info(object);
    let length = length as f64;
    length / 2.0 * (first * 2.0 + increment * (length - 1.0))
}

/// The arithmetic-series sum: an integer when it fits, as GNU R's `sum(1:n)` is.
unsafe fn intseq_sum(object: Sexp, _remove_na: bool) -> ApiResult<Option<Sexp>> {
    if expanded(object).is_some() {
        return Ok(None);
    }
    let total = sequence_total(object);
    if total > NA_INTEGER as f64 && total <= i32::MAX as f64 {
        Ok(Some(memory::scalar(SexpType::Integer, total as i32)?))
    } else {
        Ok(Some(memory::scalar(SexpType::Real, total)?))
    }
}

unsafe fn realseq_sum(object: Sexp, _remove_na: bool) -> ApiResult<Option<Sexp>> {
    if expanded(object).is_some() {
        return Ok(None);
    }
    Ok(Some(memory::scalar(
        SexpType::Real,
        sequence_total(object),
    )?))
}

/// `as.double(1:n)` stays compact.
unsafe fn intseq_coerce(object: Sexp, target: SexpType) -> ApiResult<Option<Sexp>> {
    if target != SexpType::Real || expanded(object).is_some() || object.attrib() != runtime().nil()
    {
        return Ok(None);
    }
    let (length, first, increment) = sequence_info(object);
    Ok(Some(compact_realseq(length, first, increment)?))
}

unsafe fn intseq_inspect(
    object: Sexp,
    _pre: c_int,
    _deep: c_int,
    _pvec: c_int,
    _subtree: InspectSubtree,
) -> ApiResult<bool> {
    let (length, first, increment) = sequence_info(object);
    let last = first + increment * (length - 1) as f64;
    print_sequence(object, format!("{} : {}", first as i64, last as i64));
    Ok(true)
}

unsafe fn realseq_inspect(
    object: Sexp,
    _pre: c_int,
    _deep: c_int,
    _pvec: c_int,
    _subtree: InspectSubtree,
) -> ApiResult<bool> {
    let (length, first, increment) = sequence_info(object);
    let last = first + increment * (length - 1) as f64;
    print_sequence(
        object,
        format!(
            "{} : {}",
            vectors::format_real(first),
            vectors::format_real(last)
        ),
    );
    Ok(true)
}

unsafe fn print_sequence(object: Sexp, range: String) {
    let state = if expanded(object).is_some() {
        "expanded"
    } else {
        "compact"
    };
    write_console(format!(" {range} ({state})\n").as_bytes(), false);
}

/// An expanded sequence may have been written to, so it serializes as an ordinary vector.
unsafe fn sequence_state(object: Sexp) -> ApiResult<Option<Sexp>> {
    Ok(expanded(object).is_none().then(|| object.car()))
}

unsafe fn intseq_unserialize(_class: Sexp, state: Sexp) -> ApiResult<Sexp> {
    let (length, first, increment) = serialized_sequence(state)?;
    compact_intseq(length, first as i32, increment as i32)
}

unsafe fn realseq_unserialize(_class: Sexp, state: Sexp) -> ApiResult<Sexp> {
    let (length, first, increment) = serialized_sequence(state)?;
    compact_realseq(length, first, increment)
}

/// A sequence's state is the double vector (length, first, increment); old R versions wrote
/// an integer vector.
unsafe fn serialized_sequence(state: Sexp) -> ApiResult<(R_xlen_t, f64, f64)> {
    if !matches!(state.kind(), SexpType::Real | SexpType::Integer) || vectors::xlength(state)? != 3
    {
        return Err(error("invalid compact sequence state"));
    }
    let value = |index| -> ApiResult<f64> {
        Ok(if state.kind() == SexpType::Real {
            vectors::real_elt(state, index)?
        } else {
            vectors::int_elt(state, index)? as f64
        })
    };
    Ok((value(0)? as R_xlen_t, value(1)?, value(2)?))
}

// Deferred string conversion. data1 is the pairlist (source, scipen) until every element is
// converted, then `NULL`; data2 is `NULL` until the first element is converted, then a
// character vector whose unconverted elements hold `NULL`.

/// `as.character(x)` for an integer or double vector without attributes, converting each
/// element when it is first read. The source is marked immutable, because the result reads it
/// later.
pub unsafe fn deferred_string(source: Sexp) -> ApiResult<Sexp> {
    if !matches!(source.kind(), SexpType::Integer | SexpType::Real)
        || source.attrib() != runtime().nil()
    {
        return Err(error(
            "a deferred string conversion needs an integer or double vector without attributes",
        ));
    }
    runtime().heap.root(source);
    let class = builtins()?.deferred_string;
    let heap = &mut runtime().heap;
    source.mark_not_mutable();
    let scipen = heap.alloc_vector(SexpType::Integer, 1)?;
    heap.root(scipen);
    let state = heap.cons(source, scipen)?;
    heap.root(state);
    let nil = heap.nil();
    new_altrep(class, state, nil)
}

unsafe fn deferred_source(object: Sexp) -> Option<Sexp> {
    let state = object.car();
    (state.kind() != SexpType::Nil).then(|| state.car())
}

unsafe fn deferred_length(object: Sexp) -> ApiResult<R_xlen_t> {
    match deferred_source(object) {
        Some(source) => vectors::xlength(source),
        None => Ok(object.cdr().length()),
    }
}

unsafe fn deferred_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Sexp> {
    if index < 0 || index >= deferred_length(object)? {
        return Err(error(
            "attempt to access an element of a deferred string out of bounds",
        ));
    }
    let nil = runtime().nil();
    let converted = object.cdr();
    if converted != nil {
        let element = converted.element(index as usize);
        if element != nil {
            return Ok(element);
        }
    }
    let Some(source) = deferred_source(object) else {
        return Err(error("a deferred string lost its source"));
    };
    runtime().heap.root(object);
    let element = vectors::element_as_charsxp(source, index as usize)?;
    runtime().heap.root(element);
    let converted = deferred_storage(object)?;
    runtime()
        .heap
        .set_element(converted, index as usize, element);
    Ok(element)
}

unsafe fn deferred_storage(object: Sexp) -> ApiResult<Sexp> {
    let heap = &mut runtime().heap;
    let nil = heap.nil();
    if object.cdr() != nil {
        return Ok(object.cdr());
    }
    let length = deferred_length(object)?;
    let heap = &mut runtime().heap;
    let converted = heap.alloc_vector(SexpType::String, length)?;
    converted.slice_mut::<Sexp>().fill(nil);
    heap.set_cdr(object, converted);
    Ok(converted)
}

/// Converts every element and drops the source.
unsafe fn deferred_expand(object: Sexp) -> ApiResult<Sexp> {
    if deferred_source(object).is_some() {
        runtime().heap.root(object);
        let length = deferred_length(object)?;
        deferred_storage(object)?;
        for index in 0..length {
            deferred_elt(object, index)?;
        }
        let nil = runtime().nil();
        runtime().heap.set_car(object, nil);
    }
    Ok(object.cdr())
}

unsafe fn deferred_dataptr(object: Sexp, writable: bool) -> ApiResult<*mut u8> {
    let converted = deferred_expand(object)?;
    if writable {
        runtime().heap.escape(converted);
    }
    Ok(converted.data())
}

unsafe fn deferred_dataptr_or_null(object: Sexp) -> ApiResult<*const c_void> {
    Ok(if deferred_source(object).is_none() {
        object.cdr().data() as *const c_void
    } else {
        std::ptr::null()
    })
}

unsafe fn deferred_set_elt(object: Sexp, index: R_xlen_t, value: Sexp) -> ApiResult<()> {
    let converted = deferred_expand(object)?;
    runtime().heap.set_element(converted, index as usize, value);
    Ok(())
}

unsafe fn deferred_no_na(object: Sexp) -> ApiResult<c_int> {
    match deferred_source(object) {
        Some(source) => no_na(source),
        None => Ok(0),
    }
}

unsafe fn deferred_inspect(
    object: Sexp,
    pre: c_int,
    deep: c_int,
    pvec: c_int,
    subtree: InspectSubtree,
) -> ApiResult<bool> {
    match deferred_source(object) {
        Some(source) => {
            write_console(b"  <deferred string conversion>\n", false);
            subtree(source, pre, deep, pvec);
        }
        None => {
            write_console(b"  <expanded string conversion>\n", false);
            subtree(object.cdr(), pre, deep, pvec);
        }
    }
    Ok(true)
}

unsafe fn deferred_state(object: Sexp) -> ApiResult<Option<Sexp>> {
    Ok(deferred_source(object).map(|_| object.car()))
}

unsafe fn deferred_unserialize(class: Sexp, state: Sexp) -> ApiResult<Sexp> {
    if state.kind() != SexpType::Pairlist {
        return Err(error("invalid deferred string state"));
    }
    state.car().mark_not_mutable();
    let nil = runtime().nil();
    new_altrep(AltrepClass { pointer: class }, state, nil)
}

// Wrappers. data1 is the wrapped vector and data2 the integer vector (sortedness, no_na),
// which vouches for the wrapped vector until a writable data pointer is handed out.

/// `x` under a wrapper that records `sorted` and `no_na`, or `x` itself when its type has no
/// wrapper class. The wrapper carries a shallow copy of `x`'s attributes.
pub unsafe fn wrap(object: Sexp, sorted: c_int, no_na: c_int) -> ApiResult<Sexp> {
    runtime().heap.root(object);
    let builtins = builtins()?;
    let class = match object.kind() {
        SexpType::Integer => builtins.wrap_integer,
        SexpType::Logical => builtins.wrap_logical,
        SexpType::Real => builtins.wrap_real,
        SexpType::Complex => builtins.wrap_complex,
        SexpType::Raw => builtins.wrap_raw,
        SexpType::String => builtins.wrap_string,
        _ => return Ok(object),
    };
    let metadata = runtime().heap.alloc_vector(SexpType::Integer, 2)?;
    runtime().heap.root(metadata);
    metadata
        .slice_mut::<i32>()
        .copy_from_slice(&[sorted, no_na]);
    let wrapper = new_altrep(class, object, metadata)?;
    memory::copy_attributes(wrapper, object, false)?;
    Ok(wrapper)
}

unsafe fn wrapper_metadata(object: Sexp) -> (c_int, c_int) {
    let metadata = object.cdr().slice::<i32>();
    (metadata[0], metadata[1])
}

unsafe fn wrapper_length(object: Sexp) -> ApiResult<R_xlen_t> {
    vectors::xlength(object.car())
}

unsafe fn wrapper_elt<T: Element>(object: Sexp, index: R_xlen_t) -> ApiResult<T> {
    elt::<T>(object.car(), index)
}

unsafe fn wrapper_sexp_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Sexp> {
    vectors::sexp_elt(object.car(), index as usize)
}

unsafe fn wrapper_region<T: Element>(
    object: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut c_void,
) -> ApiResult<R_xlen_t> {
    get_region::<T>(object.car(), start, count, buffer.cast())
}

/// The wrapped vector, ready to be written: unshared by a shallow copy, with the metadata
/// forgotten because the writer may break it.
unsafe fn wrapper_writable(object: Sexp) -> ApiResult<Sexp> {
    let wrapped = object.car();
    if wrapped.refcount() > 1 {
        runtime().heap.root(object);
        let copy = memory::duplicate(wrapped, false)?;
        runtime().heap.set_car(object, copy);
    }
    object
        .cdr()
        .slice_mut::<i32>()
        .copy_from_slice(&[UNKNOWN_SORTEDNESS, 0]);
    Ok(object.car())
}

unsafe fn wrapper_dataptr(object: Sexp, writable: bool) -> ApiResult<*mut u8> {
    let wrapped = if writable {
        wrapper_writable(object)?
    } else {
        object.car()
    };
    vectors::data_pointer(wrapped, writable)
}

unsafe fn wrapper_dataptr_or_null(object: Sexp) -> ApiResult<*const c_void> {
    let wrapped = object.car();
    if wrapped.is_altrep() {
        dataptr_or_null(wrapped)
    } else {
        Ok(wrapped.data() as *const c_void)
    }
}

unsafe fn wrapper_set_elt(object: Sexp, index: R_xlen_t, value: Sexp) -> ApiResult<()> {
    let wrapped = wrapper_writable(object)?;
    vectors::set_sexp_elt(wrapped, index as usize, value)
}

unsafe fn wrapper_is_sorted(object: Sexp) -> ApiResult<c_int> {
    match wrapper_metadata(object).0 {
        UNKNOWN_SORTEDNESS => is_sorted(object.car()),
        sorted => Ok(sorted),
    }
}

unsafe fn wrapper_no_na(object: Sexp) -> ApiResult<c_int> {
    match wrapper_metadata(object).1 {
        0 => no_na(object.car()),
        known => Ok(known),
    }
}

unsafe fn wrapper_duplicate(object: Sexp, deep: bool) -> ApiResult<Option<Sexp>> {
    runtime().heap.root(object);
    let copy = memory::duplicate(object.car(), deep)?;
    runtime().heap.root(copy);
    let (sorted, no_na) = wrapper_metadata(object);
    let metadata = runtime().heap.alloc_vector(SexpType::Integer, 2)?;
    runtime().heap.root(metadata);
    metadata
        .slice_mut::<i32>()
        .copy_from_slice(&[sorted, no_na]);
    let class = AltrepClass {
        pointer: object.tag(),
    };
    Ok(Some(new_altrep(class, copy, metadata)?))
}

unsafe fn wrapper_inspect(
    object: Sexp,
    pre: c_int,
    deep: c_int,
    pvec: c_int,
    subtree: InspectSubtree,
) -> ApiResult<bool> {
    let (sorted, no_na) = wrapper_metadata(object);
    write_console(
        format!(" wrapper [srt={sorted},no_na={no_na}]\n").as_bytes(),
        false,
    );
    subtree(object.car(), pre, deep, pvec);
    Ok(true)
}

unsafe fn wrapper_state(object: Sexp) -> ApiResult<Option<Sexp>> {
    Ok(Some(runtime().heap.cons(object.car(), object.cdr())?))
}

unsafe fn wrapper_unserialize(class: Sexp, state: Sexp) -> ApiResult<Sexp> {
    if state.kind() != SexpType::Pairlist {
        return Err(error("invalid wrapper state"));
    }
    new_altrep(AltrepClass { pointer: class }, state.car(), state.cdr())
}

/// A regular vector with an ALTREP object's elements and no attributes.
pub unsafe fn materialize(object: Sexp) -> ApiResult<Sexp> {
    runtime().heap.root(object);
    let kind = object.kind();
    let count = length(object)?;
    let copy = runtime().heap.alloc_vector(kind, count)?;
    runtime().heap.root(copy);
    match kind {
        SexpType::Integer | SexpType::Logical => {
            get_region::<i32>(object, 0, count, copy.data().cast())?;
        }
        SexpType::Real => {
            get_region::<f64>(object, 0, count, copy.data().cast())?;
        }
        SexpType::Raw => {
            get_region::<u8>(object, 0, count, copy.data())?;
        }
        SexpType::Complex => {
            get_region::<Rcomplex>(object, 0, count, copy.data().cast())?;
        }
        SexpType::String | SexpType::List | SexpType::Expression => {
            for index in 0..count {
                let element = sexp_elt(object, index)?;
                runtime().heap.set_element(copy, index as usize, element);
            }
        }
        _ => return Err(error("cannot materialize this ALTREP object")),
    }
    Ok(copy)
}
