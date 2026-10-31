//! ALTREP: vectors whose length, elements and data pointer come from a class's methods.
//!
//! An instance has the triple shape (data1, data2, class) with a vector type code and the
//! ALTREP header flag, so every accessor that reaches vector storage checks the flag and
//! dispatches here. A class is a raw vector holding its method table, preserved for the life of
//! the session. Every method is native code, so each call runs in a C context.
//!
//! The methods that decide length, elements, data pointers, regions, sortedness and missing
//! values dispatch. `Duplicate`, `Inspect` and `Extract_subset` are stored for when the
//! interpreter needs them; duplication materializes a regular vector instead.

use crate::{
    R_xlen_t, Rboolean,
    boundary::{ApiResult, enter, error, with_native_scope},
    runtime,
    vectors::sexp_type,
};
use heap::object::{Rcomplex, Sexp, SexpType};
use std::ffi::{c_char, c_int, c_void};

type LengthMethod = unsafe extern "C" fn(Sexp) -> R_xlen_t;
type DataptrMethod = unsafe extern "C" fn(Sexp, Rboolean) -> *mut c_void;
type DataptrOrNullMethod = unsafe extern "C" fn(Sexp) -> *const c_void;
type IntEltMethod = unsafe extern "C" fn(Sexp, R_xlen_t) -> c_int;
type RealEltMethod = unsafe extern "C" fn(Sexp, R_xlen_t) -> f64;
type RawEltMethod = unsafe extern "C" fn(Sexp, R_xlen_t) -> u8;
type ComplexEltMethod = unsafe extern "C" fn(Sexp, R_xlen_t) -> Rcomplex;
type SexpEltMethod = unsafe extern "C" fn(Sexp, R_xlen_t) -> Sexp;
type SetEltMethod = unsafe extern "C" fn(Sexp, R_xlen_t, Sexp);
type QueryMethod = unsafe extern "C" fn(Sexp) -> c_int;
type RegionMethod = unsafe extern "C" fn(Sexp, R_xlen_t, R_xlen_t, *mut c_void) -> R_xlen_t;

/// A class's method table, stored in the class object's raw data.
#[repr(C)]
#[derive(Default)]
struct Methods {
    kind: u32,
    length: Option<LengthMethod>,
    duplicate: usize,
    inspect: usize,
    extract_subset: usize,
    dataptr: Option<DataptrMethod>,
    dataptr_or_null: Option<DataptrOrNullMethod>,
    int_elt: Option<IntEltMethod>,
    real_elt: Option<RealEltMethod>,
    raw_elt: Option<RawEltMethod>,
    complex_elt: Option<ComplexEltMethod>,
    sexp_elt: Option<SexpEltMethod>,
    set_elt: Option<SetEltMethod>,
    get_region: Option<RegionMethod>,
    is_sorted: Option<QueryMethod>,
    no_na: Option<QueryMethod>,
}

/// `R_altrep_class_t`, which C passes by value.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AltrepClass {
    pointer: Sexp,
}

unsafe extern "C" {
    fn ry_altrep_length(method: LengthMethod, x: Sexp, result: *mut R_xlen_t) -> c_int;
    fn ry_altrep_dataptr(
        method: DataptrMethod,
        x: Sexp,
        writable: c_int,
        result: *mut *mut c_void,
    ) -> c_int;
    fn ry_altrep_dataptr_or_null(
        method: DataptrOrNullMethod,
        x: Sexp,
        result: *mut *const c_void,
    ) -> c_int;
    fn ry_altrep_int_elt(
        method: IntEltMethod,
        x: Sexp,
        index: R_xlen_t,
        result: *mut c_int,
    ) -> c_int;
    fn ry_altrep_real_elt(
        method: RealEltMethod,
        x: Sexp,
        index: R_xlen_t,
        result: *mut f64,
    ) -> c_int;
    fn ry_altrep_raw_elt(method: RawEltMethod, x: Sexp, index: R_xlen_t, result: *mut u8) -> c_int;
    fn ry_altrep_complex_elt(
        method: ComplexEltMethod,
        x: Sexp,
        index: R_xlen_t,
        result: *mut Rcomplex,
    ) -> c_int;
    fn ry_altrep_sexp_elt(
        method: SexpEltMethod,
        x: Sexp,
        index: R_xlen_t,
        result: *mut Sexp,
    ) -> c_int;
    fn ry_altrep_set_elt(method: SetEltMethod, x: Sexp, index: R_xlen_t, value: Sexp) -> c_int;
    fn ry_altrep_int_query(method: QueryMethod, x: Sexp, result: *mut c_int) -> c_int;
    fn ry_altrep_region(
        method: RegionMethod,
        x: Sexp,
        start: R_xlen_t,
        count: R_xlen_t,
        buffer: *mut c_void,
        result: *mut R_xlen_t,
    ) -> c_int;
}

unsafe fn methods<'a>(object: Sexp) -> &'a Methods {
    &*(object.tag().data() as *const Methods)
}

unsafe fn class_methods<'a>(class: AltrepClass) -> &'a mut Methods {
    &mut *(class.pointer.data() as *mut Methods)
}

pub unsafe fn length(object: Sexp) -> ApiResult<R_xlen_t> {
    let method = methods(object)
        .length
        .ok_or_else(|| error("ALTREP class has no Length method"))?;
    let mut result = 0;
    with_native_scope(|| ry_altrep_length(method, object, &mut result))?;
    Ok(result)
}

pub unsafe fn dataptr(object: Sexp, writable: bool) -> ApiResult<*mut u8> {
    let method = methods(object)
        .dataptr
        .ok_or_else(|| error("cannot access data pointer for this ALTVEC object"))?;
    let mut result = std::ptr::null_mut();
    with_native_scope(|| ry_altrep_dataptr(method, object, writable as c_int, &mut result))?;
    Ok(result.cast())
}

pub unsafe fn dataptr_or_null(object: Sexp) -> ApiResult<*const c_void> {
    let Some(method) = methods(object).dataptr_or_null else {
        return Ok(std::ptr::null());
    };
    let mut result = std::ptr::null();
    with_native_scope(|| ry_altrep_dataptr_or_null(method, object, &mut result))?;
    Ok(result)
}

pub unsafe fn int_elt(object: Sexp, index: R_xlen_t) -> ApiResult<i32> {
    match methods(object).int_elt {
        Some(method) => {
            let mut result = 0;
            with_native_scope(|| ry_altrep_int_elt(method, object, index, &mut result))?;
            Ok(result)
        }
        None => Ok(*(dataptr(object, false)? as *const i32).add(index as usize)),
    }
}

pub unsafe fn real_elt(object: Sexp, index: R_xlen_t) -> ApiResult<f64> {
    match methods(object).real_elt {
        Some(method) => {
            let mut result = 0.0;
            with_native_scope(|| ry_altrep_real_elt(method, object, index, &mut result))?;
            Ok(result)
        }
        None => Ok(*(dataptr(object, false)? as *const f64).add(index as usize)),
    }
}

pub unsafe fn sexp_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Sexp> {
    match methods(object).sexp_elt {
        Some(method) => {
            let mut result = runtime().nil();
            with_native_scope(|| ry_altrep_sexp_elt(method, object, index, &mut result))?;
            Ok(result)
        }
        None => Ok(*(dataptr(object, false)? as *const Sexp).add(index as usize)),
    }
}

pub unsafe fn set_sexp_elt(object: Sexp, index: R_xlen_t, value: Sexp) -> ApiResult<()> {
    match methods(object).set_elt {
        Some(method) => with_native_scope(|| ry_altrep_set_elt(method, object, index, value)),
        None => {
            *(dataptr(object, true)? as *mut Sexp).add(index as usize) = value;
            Ok(())
        }
    }
}

/// An element of an atomic vector of type `T`, ALTREP or not.
pub unsafe fn elt<T: Element>(object: Sexp, index: R_xlen_t) -> ApiResult<T> {
    if object.is_altrep() {
        T::altrep_elt(object, index)
    } else {
        Ok(*(object.data() as *const T).add(index as usize))
    }
}

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
        match methods(object).raw_elt {
            Some(method) => {
                let mut result = 0;
                with_native_scope(|| ry_altrep_raw_elt(method, object, index, &mut result))?;
                Ok(result)
            }
            None => Ok(*dataptr(object, false)?.add(index as usize)),
        }
    }
}

impl Element for Rcomplex {
    unsafe fn altrep_elt(object: Sexp, index: R_xlen_t) -> ApiResult<Rcomplex> {
        match methods(object).complex_elt {
            Some(method) => {
                let mut result = Rcomplex::default();
                with_native_scope(|| ry_altrep_complex_elt(method, object, index, &mut result))?;
                Ok(result)
            }
            None => Ok(*(dataptr(object, false)? as *const Rcomplex).add(index as usize)),
        }
    }
}

/// The class's region method, if it has one, for `*_GET_REGION`.
pub unsafe fn get_region<T>(
    object: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut T,
) -> ApiResult<Option<R_xlen_t>> {
    let Some(method) = methods(object).get_region else {
        return Ok(None);
    };
    let mut result = 0;
    with_native_scope(|| {
        ry_altrep_region(method, object, start, count, buffer.cast(), &mut result)
    })?;
    Ok(Some(result))
}

pub unsafe fn is_sorted(object: Sexp) -> ApiResult<c_int> {
    query(object, |methods| methods.is_sorted, i32::MIN)
}

pub unsafe fn no_na(object: Sexp) -> ApiResult<c_int> {
    query(object, |methods| methods.no_na, 0)
}

unsafe fn query(
    object: Sexp,
    pick: impl Fn(&Methods) -> Option<QueryMethod>,
    default: c_int,
) -> ApiResult<c_int> {
    if !object.is_altrep() {
        return Ok(default);
    }
    let Some(method) = pick(methods(object)) else {
        return Ok(default);
    };
    let mut result = default;
    with_native_scope(|| ry_altrep_int_query(method, object, &mut result))?;
    Ok(result)
}

/// A regular vector with an ALTREP object's elements.
pub unsafe fn materialize(object: Sexp) -> ApiResult<Sexp> {
    let kind = object.kind();
    let count = length(object)?;
    let runtime = runtime();
    let copy = runtime.heap.alloc_vector(kind, count)?;
    runtime.heap.root(copy);
    for index in 0..count {
        let position = index as usize;
        match kind {
            SexpType::Integer | SexpType::Logical => {
                *(copy.data() as *mut i32).add(position) = int_elt(object, index)?
            }
            SexpType::Real => *(copy.data() as *mut f64).add(position) = real_elt(object, index)?,
            SexpType::Raw => *copy.data().add(position) = u8::altrep_elt(object, index)?,
            SexpType::Complex => {
                *(copy.data() as *mut Rcomplex).add(position) = Rcomplex::altrep_elt(object, index)?
            }
            SexpType::String | SexpType::List => {
                let element = sexp_elt(object, index)?;
                runtime.heap.set_element(copy, position, element);
            }
            _ => return Err(error("cannot materialize this ALTREP object")),
        }
    }
    Ok(copy)
}

unsafe fn make_class(kind: SexpType) -> ApiResult<AltrepClass> {
    let runtime = runtime();
    let pointer = runtime
        .heap
        .alloc_vector(SexpType::Raw, size_of::<Methods>() as isize)?;
    std::ptr::write(
        pointer.data() as *mut Methods,
        Methods {
            kind: kind.code(),
            ..Methods::default()
        },
    );
    runtime.heap.preserve(pointer);
    Ok(AltrepClass { pointer })
}

macro_rules! class_constructor {
    ($name:ident, $kind:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            _class: *const c_char,
            _package: *const c_char,
            _dll: *mut c_void,
        ) -> AltrepClass {
            enter(|| make_class($kind))
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

impl crate::boundary::Handout for AltrepClass {}

macro_rules! method_setter {
    ($name:ident, $field:ident, $type:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(class: AltrepClass, method: Option<$type>) {
            class_methods(class).$field = method;
        }
    };
}

method_setter!(R_set_altrep_Length_method, length, LengthMethod);
method_setter!(R_set_altvec_Dataptr_method, dataptr, DataptrMethod);
method_setter!(
    R_set_altvec_Dataptr_or_null_method,
    dataptr_or_null,
    DataptrOrNullMethod
);
method_setter!(R_set_altinteger_Elt_method, int_elt, IntEltMethod);
method_setter!(R_set_altlogical_Elt_method, int_elt, IntEltMethod);
method_setter!(R_set_altreal_Elt_method, real_elt, RealEltMethod);
method_setter!(R_set_altraw_Elt_method, raw_elt, RawEltMethod);
method_setter!(R_set_altcomplex_Elt_method, complex_elt, ComplexEltMethod);
method_setter!(R_set_altstring_Elt_method, sexp_elt, SexpEltMethod);
method_setter!(R_set_altlist_Elt_method, sexp_elt, SexpEltMethod);
method_setter!(R_set_altstring_Set_elt_method, set_elt, SetEltMethod);
method_setter!(R_set_altlist_Set_elt_method, set_elt, SetEltMethod);
method_setter!(R_set_altinteger_Get_region_method, get_region, RegionMethod);
method_setter!(R_set_altreal_Get_region_method, get_region, RegionMethod);
method_setter!(R_set_altinteger_Is_sorted_method, is_sorted, QueryMethod);
method_setter!(R_set_altreal_Is_sorted_method, is_sorted, QueryMethod);
method_setter!(R_set_altstring_Is_sorted_method, is_sorted, QueryMethod);
method_setter!(R_set_altinteger_No_NA_method, no_na, QueryMethod);
method_setter!(R_set_altreal_No_NA_method, no_na, QueryMethod);
method_setter!(R_set_altstring_No_NA_method, no_na, QueryMethod);

macro_rules! stored_setter {
    ($name:ident, $field:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(class: AltrepClass, method: usize) {
            class_methods(class).$field = method;
        }
    };
}

stored_setter!(R_set_altrep_Duplicate_method, duplicate);
stored_setter!(R_set_altrep_Inspect_method, inspect);
stored_setter!(R_set_altvec_Extract_subset_method, extract_subset);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_new_altrep(class: AltrepClass, data1: Sexp, data2: Sexp) -> Sexp {
    enter(|| {
        let kind = sexp_type(class_methods(class).kind)?;
        Ok(runtime()
            .heap
            .alloc_altrep(kind, class.pointer, data1, data2)?)
    })
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
    enter(|| Ok(instance(object)?.car()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_data2(object: Sexp) -> Sexp {
    enter(|| Ok(instance(object)?.cdr()))
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
    enter(|| Ok(instance(object)?.tag()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_altrep_inherits(object: Sexp, class: AltrepClass) -> Rboolean {
    (object.is_altrep() && object.tag() == class.pointer) as Rboolean
}
