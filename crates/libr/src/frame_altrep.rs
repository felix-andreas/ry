//! Frame columns as R vectors, through ALTREP.
//!
//! An exposed column is an ALTREP vector whose methods read the column's Arrow buffer, so R code
//! and compiled packages see an ordinary integer, double, logical or character vector without the
//! column being copied. `data1` is an external pointer that owns the Arrow array, and `data2` is
//! `NULL` until something needs R's own layout, when it holds the materialized vector:
//!
//! - `DATAPTR_OR_NULL` and a read-only `DATAPTR` return the Arrow buffer itself for an integer or
//!   double column without NAs, because the bytes are identical in both layouts.
//! - A column with NAs, a logical column (Arrow packs booleans into bits) and a writable
//!   `DATAPTR` materialize `data2` once, and every later access reads it. A write therefore
//!   changes the R vector, never the frame, which is R's value semantics: the vector a package
//!   writes into is its own.
//! - A character column materializes its `CHARSXP`s on the first element access, since R's string
//!   cache has to intern every string anyway.
//! - A factor is exposed as an ordinary integer vector with its levels, because R's one-based
//!   `int` codes differ from the enum's unsigned zero-based codes, so a copy is unavoidable.
//!
//! [`import_column`] is the other direction. An exposed column whose `data2` is still `NULL` comes
//! back as the very same Arrow array, so `frame(a = f$a)` shares the buffer. Any other vector is
//! copied into a new Arrow buffer, with its NAs rewritten into validity.
//!
//! The functions here allocate on the R heap and must run on the thread that owns the runtime.
//! Their results are rooted in the current handle scope, like any value an API function returns.

use table::{
    ColumnType, Table,
    convert::{RVector, to_r, to_series},
};
use heap::{
    object::{Header, NA_INTEGER, NA_REAL, Sexp, SexpType},
    strings::Encoding,
};
use crate::{
    R_xlen_t, Rboolean,
    altrep::{
        AltrepClass, R_altrep_inherits, R_make_altinteger_class, R_make_altlogical_class,
        R_make_altreal_class, R_make_altstring_class, R_new_altrep, R_set_altinteger_Elt_method,
        R_set_altinteger_Get_region_method, R_set_altinteger_No_NA_method,
        R_set_altlogical_Elt_method, R_set_altreal_Elt_method, R_set_altreal_Get_region_method,
        R_set_altreal_No_NA_method, R_set_altrep_Length_method, R_set_altstring_Elt_method,
        R_set_altstring_No_NA_method, R_set_altvec_Dataptr_method,
        R_set_altvec_Dataptr_or_null_method,
    },
    attributes::{class_gets, get_attrib, set_attrib},
    boundary::{ApiResult, Unwind, enter, error},
    globals::{R_ClassSymbol, R_LevelsSymbol, R_NamesSymbol, R_RowNamesSymbol},
    memory::alloc_vector,
    runtime,
};
use polars::prelude::{
    BooleanChunked, Float64Chunked, Int32Chunked, IntoSeries, PlSmallStr, Series, StringChunked,
};
use polars_arrow::array::{Array, BooleanArray, PrimitiveArray, Utf8ViewArray};
use std::{
    ffi::{c_int, c_void},
    sync::OnceLock,
};

/// Exposes one column as an R vector.
///
/// # Safety
///
/// The R runtime must be running on this thread.
pub unsafe fn expose_column(table: &Table, name: &str) -> ApiResult<Sexp> {
    unsafe {
        let frame = table.frame();
        let column = frame
            .column(name)
            .map_err(|_| error(format!("the frame has no column `{name}`")))?;
        let series = column.as_materialized_series().rechunk();
        let payload = match ColumnType::of(series.dtype()) {
            Some(ColumnType::Integer) => {
                Payload::Integer(first_chunk(series.i32().map_err(polars_error)?)?)
            }
            Some(ColumnType::Double) => {
                Payload::Double(first_chunk(series.f64().map_err(polars_error)?)?)
            }
            Some(ColumnType::Logical) => {
                Payload::Logical(first_chunk(series.bool().map_err(polars_error)?)?)
            }
            Some(ColumnType::Character) => {
                Payload::Character(first_chunk(series.str().map_err(polars_error)?)?)
            }
            Some(ColumnType::Factor) => return factor_vector(&series),
            None => return Err(error(format!("column `{name}` has no R type"))),
        };
        new_altrep(payload)
    }
}

/// Exposes the whole frame as the R object `frame(...)` returns: a list of exposed columns with
/// names, compact row names and the class `c("frame", "data.frame")`.
///
/// # Safety
///
/// The R runtime must be running on this thread.
pub unsafe fn expose_table(table: &Table) -> ApiResult<Sexp> {
    unsafe {
        let schema = table.schema();
        let height = table.height();
        let list = alloc_vector(SexpType::List, schema.columns.len() as R_xlen_t)?;
        let names = alloc_vector(SexpType::String, schema.columns.len() as R_xlen_t)?;
        for (index, (name, _)) in schema.columns.iter().enumerate() {
            let column = expose_column(table, name)?;
            runtime().heap.set_element(list, index, column);
            let name = runtime().heap.mk_char(name.as_bytes(), Encoding::Utf8)?;
            runtime().heap.set_element(names, index, name);
        }
        set_attrib(list, R_NamesSymbol.get(), names)?;
        // Compact row names, `c(NA_integer_, -n)`, which is how R stores `1:n` row names.
        let row_names = alloc_vector(SexpType::Integer, 2)?;
        row_names
            .slice_mut::<i32>()
            .copy_from_slice(&[NA_INTEGER, -(height as i32)]);
        set_attrib(list, R_RowNamesSymbol.get(), row_names)?;
        class_gets(list, strings(&["frame", "data.frame"])?)?;
        Ok(list)
    }
}

/// Reads an R vector as a column. An exposed column that R has not materialized returns its own
/// Arrow array, so the round trip shares the buffer.
///
/// # Safety
///
/// The R runtime must be running on this thread, and `vector` must be a live R object.
pub unsafe fn import_column(name: PlSmallStr, vector: Sexp) -> ApiResult<Series> {
    unsafe {
        if let Some(payload) = exposed(vector)
            && vector.cdr() == runtime().nil()
        {
            return Ok(payload.series(name));
        }
        let length = vector_length(vector)?;
        let r_vector = match vector.kind() {
            SexpType::Integer => {
                let codes = read_slice::<i32>(vector, length)?.to_vec();
                let levels = get_attrib(vector, R_LevelsSymbol.get())?;
                if levels.kind() == SexpType::String {
                    let levels = read_strings(levels)?
                        .into_iter()
                        .map(Option::unwrap_or_default)
                        .collect();
                    RVector::Factor { codes, levels }
                } else {
                    RVector::Integer(codes)
                }
            }
            SexpType::Logical => RVector::Logical(read_slice::<i32>(vector, length)?.to_vec()),
            SexpType::Real => RVector::Double(read_slice::<f64>(vector, length)?.to_vec()),
            SexpType::String => RVector::Character(read_strings(vector)?),
            other => {
                return Err(error(format!(
                    "a frame column cannot be a {} vector",
                    other.name()
                )));
            }
        };
        to_series(name, r_vector).map_err(table_error)
    }
}

/// What an exposed column's external pointer owns: the column's Arrow array, a cheap handle on
/// its shared buffers.
enum Payload {
    Integer(PrimitiveArray<i32>),
    Double(PrimitiveArray<f64>),
    Logical(BooleanArray),
    Character(Utf8ViewArray),
}

impl Payload {
    fn len(&self) -> usize {
        match self {
            Payload::Integer(array) => array.len(),
            Payload::Double(array) => array.len(),
            Payload::Logical(array) => array.len(),
            Payload::Character(array) => array.len(),
        }
    }

    fn null_count(&self) -> usize {
        match self {
            Payload::Integer(array) => array.null_count(),
            Payload::Double(array) => array.null_count(),
            Payload::Logical(array) => array.null_count(),
            Payload::Character(array) => array.null_count(),
        }
    }

    fn kind(&self) -> SexpType {
        match self {
            Payload::Integer(_) => SexpType::Integer,
            Payload::Double(_) => SexpType::Real,
            Payload::Logical(_) => SexpType::Logical,
            Payload::Character(_) => SexpType::String,
        }
    }

    /// The buffer R can read directly, when the layouts agree.
    fn shared_pointer(&self) -> Option<*const c_void> {
        match self {
            Payload::Integer(array) if array.null_count() == 0 => {
                Some(array.values().as_ptr().cast())
            }
            Payload::Double(array) if array.null_count() == 0 => {
                Some(array.values().as_ptr().cast())
            }
            _ => None,
        }
    }

    fn int(&self, index: usize) -> i32 {
        match self {
            Payload::Integer(array) => {
                if array.is_valid(index) {
                    array.value(index)
                } else {
                    NA_INTEGER
                }
            }
            Payload::Logical(array) => {
                if array.is_valid(index) {
                    i32::from(array.value(index))
                } else {
                    NA_INTEGER
                }
            }
            _ => NA_INTEGER,
        }
    }

    fn real(&self, index: usize) -> f64 {
        match self {
            Payload::Double(array) => {
                if array.is_valid(index) {
                    array.value(index)
                } else {
                    NA_REAL
                }
            }
            _ => NA_REAL,
        }
    }

    fn series(&self, name: PlSmallStr) -> Series {
        match self {
            Payload::Integer(array) => Int32Chunked::with_chunk(name, array.clone()).into_series(),
            Payload::Double(array) => Float64Chunked::with_chunk(name, array.clone()).into_series(),
            Payload::Logical(array) => {
                BooleanChunked::with_chunk(name, array.clone()).into_series()
            }
            Payload::Character(array) => {
                StringChunked::with_chunk(name, array.clone()).into_series()
            }
        }
    }
}

/// The four ALTREP classes, registered once per session.
#[derive(Clone, Copy)]
struct Classes {
    integer: AltrepClass,
    real: AltrepClass,
    logical: AltrepClass,
    string: AltrepClass,
}

// SAFETY: the classes are preserved R objects, created once and never changed, and the R runtime
// that reads them runs on one thread at a time.
unsafe impl Send for Classes {}
unsafe impl Sync for Classes {}

static CLASSES: OnceLock<Classes> = OnceLock::new();

fn classes() -> Classes {
    *CLASSES.get_or_init(|| unsafe { register_classes() })
}

unsafe fn register_classes() -> Classes {
    unsafe {
        let integer = R_make_altinteger_class(
            c"ry_frame_integer".as_ptr(),
            c"ry".as_ptr(),
            std::ptr::null_mut(),
        );
        let real = R_make_altreal_class(
            c"ry_frame_double".as_ptr(),
            c"ry".as_ptr(),
            std::ptr::null_mut(),
        );
        let logical = R_make_altlogical_class(
            c"ry_frame_logical".as_ptr(),
            c"ry".as_ptr(),
            std::ptr::null_mut(),
        );
        let string = R_make_altstring_class(
            c"ry_frame_character".as_ptr(),
            c"ry".as_ptr(),
            std::ptr::null_mut(),
        );
        for class in [integer, real, logical, string] {
            R_set_altrep_Length_method(class, Some(length_method));
            R_set_altvec_Dataptr_method(class, Some(dataptr_method));
            R_set_altvec_Dataptr_or_null_method(class, Some(dataptr_or_null_method));
        }
        R_set_altinteger_Elt_method(integer, Some(int_elt_method));
        R_set_altlogical_Elt_method(logical, Some(int_elt_method));
        R_set_altreal_Elt_method(real, Some(real_elt_method));
        R_set_altstring_Elt_method(string, Some(string_elt_method));
        R_set_altinteger_Get_region_method(integer, Some(int_region_method));
        R_set_altreal_Get_region_method(real, Some(real_region_method));
        R_set_altinteger_No_NA_method(integer, Some(no_na_method));
        R_set_altreal_No_NA_method(real, Some(no_na_method));
        R_set_altstring_No_NA_method(string, Some(no_na_method));
        Classes {
            integer,
            real,
            logical,
            string,
        }
    }
}

unsafe fn new_altrep(payload: Payload) -> ApiResult<Sexp> {
    unsafe {
        let classes = classes();
        let class = match payload.kind() {
            SexpType::Integer => classes.integer,
            SexpType::Real => classes.real,
            SexpType::Logical => classes.logical,
            _ => classes.string,
        };
        let runtime = runtime();
        let nil = runtime.nil();
        let address = Box::into_raw(Box::new(payload));
        let pointer = match runtime.heap.alloc_external_ptr(address.cast(), nil, nil) {
            Ok(pointer) => pointer,
            Err(failure) => {
                drop(Box::from_raw(address));
                return Err(failure.into());
            }
        };
        runtime.heap.root(pointer);
        let finalizer = release_payload as unsafe extern "C" fn(Sexp) as usize;
        runtime
            .heap
            .alloc_weak_ref(pointer, nil, nil, finalizer, true)?;
        // `R_new_altrep` roots its result only inside a native call, and this may run outside one.
        let object = R_new_altrep(class, pointer, nil);
        crate::runtime().heap.root(object);
        Ok(object)
    }
}

/// Frees the payload when its external pointer is collected, which drops this side's reference
/// to the Arrow buffers.
unsafe extern "C" fn release_payload(pointer: Sexp) {
    unsafe {
        let address = pointer.external_address().cast::<Payload>();
        if !address.is_null() {
            pointer.set_external_address(std::ptr::null_mut());
            drop(Box::from_raw(address));
        }
    }
}

/// The payload behind `vector`, when it is one of this module's ALTREP columns.
unsafe fn exposed<'a>(vector: Sexp) -> Option<&'a Payload> {
    unsafe {
        let classes = classes();
        let ours = [
            classes.integer,
            classes.real,
            classes.logical,
            classes.string,
        ]
        .into_iter()
        .any(|class| R_altrep_inherits(vector, class) != 0);
        if !ours {
            return None;
        }
        payload(vector)
    }
}

unsafe fn payload<'a>(vector: Sexp) -> Option<&'a Payload> {
    unsafe { vector.car().external_address().cast::<Payload>().as_ref() }
}

/// `data2`, R's layout of the column, materialized on first need.
unsafe fn materialized(vector: Sexp) -> ApiResult<Sexp> {
    unsafe {
        let runtime = runtime();
        if vector.cdr() != runtime.nil() {
            return Ok(vector.cdr());
        }
        let payload = payload(vector).ok_or_else(|| error("a frame column outlived its data"))?;
        let length = payload.len();
        let copy = alloc_vector(payload.kind(), length as R_xlen_t)?;
        // Attached before it is filled, so the strings allocated below cannot collect it.
        runtime.heap.set_cdr(vector, copy);
        match payload {
            Payload::Integer(_) | Payload::Logical(_) => {
                for (index, slot) in copy.slice_mut::<i32>().iter_mut().enumerate() {
                    *slot = payload.int(index);
                }
            }
            Payload::Double(_) => {
                for (index, slot) in copy.slice_mut::<f64>().iter_mut().enumerate() {
                    *slot = payload.real(index);
                }
            }
            Payload::Character(array) => {
                let na = runtime.heap.known().na_string;
                for (index, value) in array.iter().enumerate() {
                    let element = match value {
                        Some(text) => match runtime.heap.mk_char(text.as_bytes(), Encoding::Utf8) {
                            Ok(element) => element,
                            Err(failure) => {
                                let nil = runtime.nil();
                                runtime.heap.set_cdr(vector, nil);
                                return Err(failure.into());
                            }
                        },
                        None => na,
                    };
                    runtime.heap.set_element(copy, index, element);
                }
            }
        }
        Ok(copy)
    }
}

unsafe extern "C" fn length_method(vector: Sexp) -> R_xlen_t {
    unsafe { payload(vector).map_or(0, |payload| payload.len() as R_xlen_t) }
}

unsafe extern "C" fn dataptr_or_null_method(vector: Sexp) -> *const c_void {
    unsafe {
        if vector.cdr() != runtime().nil() {
            return vector.cdr().data().cast_const().cast();
        }
        payload(vector)
            .and_then(Payload::shared_pointer)
            .unwrap_or(std::ptr::null())
    }
}

unsafe extern "C" fn dataptr_method(vector: Sexp, writable: Rboolean) -> *mut c_void {
    unsafe {
        enter(|| {
            if writable == 0
                && vector.cdr() == runtime().nil()
                && let Some(pointer) = payload(vector).and_then(Payload::shared_pointer)
            {
                // R promises not to write through a read-only pointer.
                return Ok(pointer.cast_mut());
            }
            Ok(materialized(vector)?.data().cast::<c_void>())
        })
    }
}

unsafe extern "C" fn int_elt_method(vector: Sexp, index: R_xlen_t) -> c_int {
    unsafe {
        if vector.cdr() != runtime().nil() {
            return vector
                .cdr()
                .slice::<i32>()
                .get(index as usize)
                .copied()
                .unwrap_or(NA_INTEGER);
        }
        payload(vector).map_or(NA_INTEGER, |payload| payload.int(index as usize))
    }
}

unsafe extern "C" fn real_elt_method(vector: Sexp, index: R_xlen_t) -> f64 {
    unsafe {
        if vector.cdr() != runtime().nil() {
            return vector
                .cdr()
                .slice::<f64>()
                .get(index as usize)
                .copied()
                .unwrap_or(NA_REAL);
        }
        payload(vector).map_or(NA_REAL, |payload| payload.real(index as usize))
    }
}

/// R declares the element method as returning a nullable `SEXP`; this one never returns null.
unsafe extern "C" fn string_elt_method(vector: Sexp, index: R_xlen_t) -> *mut Header {
    let element = unsafe {
        enter(|| {
            let strings = materialized(vector)?;
            if index < 0 || index >= strings.length() {
                return Err(error("string index out of range"));
            }
            Ok(strings.element(index as usize))
        })
    };
    element.as_ptr()
}

unsafe extern "C" fn int_region_method(
    vector: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut c_void,
) -> R_xlen_t {
    unsafe {
        let length = length_method(vector);
        let count = count.min(length - start).max(0);
        let buffer = std::slice::from_raw_parts_mut(buffer.cast::<i32>(), count as usize);
        for (offset, slot) in buffer.iter_mut().enumerate() {
            *slot = int_elt_method(vector, start + offset as R_xlen_t);
        }
        count
    }
}

unsafe extern "C" fn real_region_method(
    vector: Sexp,
    start: R_xlen_t,
    count: R_xlen_t,
    buffer: *mut c_void,
) -> R_xlen_t {
    unsafe {
        let length = length_method(vector);
        let count = count.min(length - start).max(0);
        let buffer = std::slice::from_raw_parts_mut(buffer.cast::<f64>(), count as usize);
        for (offset, slot) in buffer.iter_mut().enumerate() {
            *slot = real_elt_method(vector, start + offset as R_xlen_t);
        }
        count
    }
}

/// 1 when the column has no NAs, 0 when it may have some. A materialized column may have been
/// written since, so it answers 0.
unsafe extern "C" fn no_na_method(vector: Sexp) -> c_int {
    unsafe {
        if vector.cdr() != runtime().nil() {
            return 0;
        }
        payload(vector).is_some_and(|payload| payload.null_count() == 0) as c_int
    }
}

unsafe fn factor_vector(series: &Series) -> ApiResult<Sexp> {
    unsafe {
        let RVector::Factor { codes, levels } = to_r(series).map_err(table_error)? else {
            return Err(error("a factor column must convert to a factor"));
        };
        let vector = alloc_vector(SexpType::Integer, codes.len() as R_xlen_t)?;
        vector.slice_mut::<i32>().copy_from_slice(&codes);
        let levels: Vec<&str> = levels.iter().map(String::as_str).collect();
        set_attrib(vector, R_LevelsSymbol.get(), strings(&levels)?)?;
        set_attrib(vector, R_ClassSymbol.get(), strings(&["factor"])?)?;
        Ok(vector)
    }
}

unsafe fn strings(values: &[&str]) -> ApiResult<Sexp> {
    unsafe {
        let vector = alloc_vector(SexpType::String, values.len() as R_xlen_t)?;
        for (index, value) in values.iter().enumerate() {
            let element = runtime().heap.mk_char(value.as_bytes(), Encoding::Utf8)?;
            runtime().heap.set_element(vector, index, element);
        }
        Ok(vector)
    }
}

/// The vector's data in R's layout. An ALTREP vector from elsewhere is asked for its data pointer,
/// which materializes it if it must.
unsafe fn read_slice<'a, T>(vector: Sexp, length: usize) -> ApiResult<&'a [T]> {
    unsafe {
        if !vector.is_altrep() {
            return Ok(vector.slice::<T>());
        }
        let pointer = crate::altrep::dataptr(vector, false)?;
        Ok(std::slice::from_raw_parts(pointer.cast::<T>(), length))
    }
}

/// The length of any vector. An ALTREP vector's header holds no length; its class answers.
unsafe fn vector_length(vector: Sexp) -> ApiResult<usize> {
    unsafe {
        let length = if vector.is_altrep() {
            crate::altrep::length(vector)?
        } else {
            vector.length()
        };
        Ok(length as usize)
    }
}

unsafe fn read_strings(vector: Sexp) -> ApiResult<Vec<Option<String>>> {
    unsafe {
        let na = runtime().heap.known().na_string;
        let length = vector_length(vector)?;
        let mut values = Vec::with_capacity(length);
        for index in 0..length as R_xlen_t {
            let element = if vector.is_altrep() {
                crate::altrep::sexp_elt(vector, index)?
            } else {
                vector.element(index as usize)
            };
            values.push(
                (element != na).then(|| String::from_utf8_lossy(element.bytes()).into_owned()),
            );
        }
        Ok(values)
    }
}

fn first_chunk<T: Array + Clone>(
    chunked: &polars::prelude::ChunkedArray<impl polars::prelude::PolarsDataType<Array = T>>,
) -> ApiResult<T> {
    chunked
        .downcast_iter()
        .next()
        .cloned()
        .map_or_else(|| Err(error("an empty column has no chunk")), Ok)
}

fn table_error(failure: table::Error) -> Unwind {
    error(failure.to_string())
}

fn polars_error(failure: polars::prelude::PolarsError) -> Unwind {
    error(failure.to_string())
}
