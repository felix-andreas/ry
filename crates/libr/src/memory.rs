//! Allocation, protection and collection: `Rf_allocVector` and its relatives, `PROTECT`, the
//! precious list, `R_alloc` and the `R_Calloc` family.

use crate::{
    R_len_t, R_xlen_t, Rboolean, SEXPTYPE,
    boundary::{ApiResult, enter, error, run_pending_finalizers},
    runtime,
    strings::c_bytes,
    vectors::{set_names_from, sexp_type},
};
use heap::{
    Collection, HeapError,
    arena::Arena,
    object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp},
    strings::Encoding,
};
use std::ffi::{c_char, c_int, c_long, c_void};

/// `Rf_allocVector` as Rust code calls it. Pairlist and call types allocate pairlists, as in
/// GNU R.
pub unsafe fn alloc_vector(kind: SexpType, length: R_xlen_t) -> ApiResult<Sexp> {
    let runtime = runtime();
    match kind {
        SexpType::Nil => Ok(runtime.nil()),
        SexpType::Pairlist => alloc_list(length, SexpType::Pairlist),
        SexpType::Language => alloc_list(length, SexpType::Language),
        _ if kind.is_vector() && kind != SexpType::Char => {
            let object = runtime.heap.alloc_vector(kind, length)?;
            runtime.heap.root(object);
            Ok(object)
        }
        _ => Err(HeapError::InvalidVectorType { kind, length }.into()),
    }
}

/// A pairlist of `length` cells holding `NULL`; with `Language`, the first cell is a call.
pub unsafe fn alloc_list(length: R_xlen_t, head: SexpType) -> ApiResult<Sexp> {
    if length < 0 {
        return Err(HeapError::NegativeLength.into());
    }
    let runtime = runtime();
    let nil = runtime.nil();
    let mut list = nil;
    for index in (0..length).rev() {
        let kind = if index == 0 { head } else { SexpType::Pairlist };
        list = runtime.heap.alloc_triple(kind, nil, list, nil)?;
        runtime.heap.root(list);
    }
    Ok(list)
}

/// A vector of length one holding `value`.
pub unsafe fn scalar<T: Copy>(kind: SexpType, value: T) -> ApiResult<Sexp> {
    let object = alloc_vector(kind, 1)?;
    *(object.data() as *mut T) = value;
    Ok(object)
}

pub unsafe fn scalar_string(charsxp: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    runtime.heap.root(charsxp);
    let object = runtime.heap.alloc_vector(SexpType::String, 1)?;
    runtime.heap.root(object);
    runtime.heap.set_element(object, 0, charsxp);
    Ok(object)
}

/// `Rf_duplicate` and `Rf_shallow_duplicate`.
pub unsafe fn duplicate(object: Sexp, deep: bool) -> ApiResult<Sexp> {
    if object.is_altrep() {
        let copy = crate::altrep::materialize(object)?;
        copy_attributes(copy, object, deep)?;
        return Ok(copy);
    }
    let copy = match object.kind() {
        SexpType::Nil
        | SexpType::Symbol
        | SexpType::Environment
        | SexpType::Special
        | SexpType::Builtin
        | SexpType::ExternalPtr
        | SexpType::WeakRef
        | SexpType::Bytecode
        | SexpType::Char
        | SexpType::Promise
        | SexpType::Binding
        | SexpType::Any
        | SexpType::Free => return Ok(object),
        SexpType::Closure | SexpType::Object => {
            let copy = runtime().heap.alloc_triple(
                object.kind(),
                object.car(),
                object.cdr(),
                object.tag(),
            )?;
            runtime().heap.root(copy);
            copy
        }
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            let mut cells = Vec::new();
            let mut cell = object;
            while matches!(
                cell.kind(),
                SexpType::Pairlist | SexpType::Language | SexpType::Dots
            ) {
                cells.push(cell);
                cell = cell.cdr();
            }
            let mut copy = cell;
            for &original in cells.iter().rev() {
                let car = if deep {
                    duplicate(original.car(), true)?
                } else {
                    original.car()
                };
                runtime().heap.root(car);
                copy = runtime()
                    .heap
                    .alloc_triple(original.kind(), car, copy, original.tag())?;
                runtime().heap.root(copy);
                copy_attributes(copy, original, deep)?;
                copy.set_gp(original.gp());
            }
            return Ok(copy);
        }
        kind => {
            let length = object.length();
            let copy = runtime().heap.alloc_vector(kind, length)?;
            runtime().heap.root(copy);
            match kind {
                SexpType::String => {
                    for index in 0..length as usize {
                        runtime()
                            .heap
                            .set_element(copy, index, object.element(index));
                    }
                }
                SexpType::List | SexpType::Expression => {
                    for index in 0..length as usize {
                        let element = object.element(index);
                        let element = if deep {
                            duplicate(element, true)?
                        } else {
                            element
                        };
                        runtime().heap.set_element(copy, index, element);
                    }
                }
                _ => {
                    let bytes = length as usize * kind.element_size().unwrap_or(0);
                    std::ptr::copy_nonoverlapping(object.data(), copy.data(), bytes);
                }
            }
            copy
        }
    };
    copy_attributes(copy, object, deep)?;
    Ok(copy)
}

/// `DUPLICATE_ATTRIB` and its shallow form: the attributes, the object bit and the S4 bit.
pub unsafe fn copy_attributes(to: Sexp, from: Sexp, deep: bool) -> ApiResult<()> {
    let runtime = runtime();
    let attributes = from.attrib();
    let attributes = if attributes == runtime.nil() {
        attributes
    } else {
        duplicate(attributes, deep)?
    };
    runtime.heap.set_attrib(to, attributes);
    to.set_object(from.is_object());
    to.set_gp_bits(gp::S4_OBJECT, from.has_gp(gp::S4_OBJECT));
    Ok(())
}

/// `Rf_xlengthgets`: a copy of `object` with `length` elements, padded with `NA` (or `NULL` in
/// a list) and keeping only the names.
pub unsafe fn length_gets(object: Sexp, length: R_xlen_t) -> ApiResult<Sexp> {
    let kind = object.kind();
    if kind == SexpType::Nil {
        if length > 0 {
            crate::boundary::warn("length of NULL cannot be changed");
        }
        return Ok(object);
    }
    if !(kind.is_vector() || matches!(kind, SexpType::Nil | SexpType::Pairlist))
        || kind == SexpType::Char
    {
        return Err(error("cannot set length of non-(vector or list)"));
    }
    if length < 0 {
        return Err(error("invalid value"));
    }
    if kind == SexpType::Pairlist {
        let vector = crate::lists::pairlist_to_vector(object)?;
        return length_gets(vector, length);
    }
    let old_length = crate::vectors::xlength(object)?;
    let copy = runtime().heap.alloc_vector(kind, length)?;
    runtime().heap.root(copy);
    let kept = old_length.min(length) as usize;
    for index in 0..length as usize {
        let present = index < kept;
        match kind {
            SexpType::Logical | SexpType::Integer => {
                let value = if present {
                    crate::vectors::int_elt(object, index)?
                } else {
                    NA_INTEGER
                };
                *(copy.data() as *mut i32).add(index) = value;
            }
            SexpType::Real => {
                let value = if present {
                    crate::vectors::real_elt(object, index)?
                } else {
                    NA_REAL
                };
                *(copy.data() as *mut f64).add(index) = value;
            }
            SexpType::Complex => {
                let value = if present {
                    *(crate::vectors::data_pointer(object, false)? as *const Rcomplex).add(index)
                } else {
                    Rcomplex {
                        r: NA_REAL,
                        i: NA_REAL,
                    }
                };
                *(copy.data() as *mut Rcomplex).add(index) = value;
            }
            SexpType::Raw => {
                let value = if present {
                    *(crate::vectors::data_pointer(object, false)? as *const u8).add(index)
                } else {
                    0
                };
                *copy.data().add(index) = value;
            }
            SexpType::String => {
                let value = if present {
                    crate::vectors::sexp_elt(object, index)?
                } else {
                    runtime().heap.known().na_string
                };
                runtime().heap.set_element(copy, index, value);
            }
            SexpType::List | SexpType::Expression if present => {
                let value = crate::vectors::sexp_elt(object, index)?;
                runtime().heap.set_element(copy, index, value);
            }
            _ => {}
        }
    }
    let names = crate::attributes::get_attrib(object, crate::globals::R_NamesSymbol.get())?;
    if names != runtime().nil() {
        let blank = runtime().heap.known().blank_string;
        let new_names = runtime().heap.alloc_vector(SexpType::String, length)?;
        runtime().heap.root(new_names);
        for index in 0..length as usize {
            let name = if index < kept {
                names.element(index)
            } else {
                blank
            };
            runtime().heap.set_element(new_names, index, name);
        }
        set_names_from(copy, new_names)?;
    }
    Ok(copy)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocVector(kind: SEXPTYPE, length: R_xlen_t) -> Sexp {
    enter(|| alloc_vector(sexp_type(kind)?, length))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocVector3(
    kind: SEXPTYPE,
    length: R_xlen_t,
    allocator: *mut c_void,
) -> Sexp {
    enter(|| {
        if !allocator.is_null() {
            return Err(error(
                "allocVector3 with a custom allocator is not supported by ry yet",
            ));
        }
        alloc_vector(sexp_type(kind)?, length)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocMatrix(kind: SEXPTYPE, rows: c_int, columns: c_int) -> Sexp {
    enter(|| {
        if rows < 0 || columns < 0 {
            return Err(error("negative extents to matrix"));
        }
        alloc_array(sexp_type(kind)?, &[rows, columns])
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_alloc3DArray(
    kind: SEXPTYPE,
    first: c_int,
    second: c_int,
    third: c_int,
) -> Sexp {
    enter(|| {
        if first < 0 || second < 0 || third < 0 {
            return Err(error("negative extents to 3D array"));
        }
        alloc_array(sexp_type(kind)?, &[first, second, third])
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocArray(kind: SEXPTYPE, dims: Sexp) -> Sexp {
    enter(|| {
        let extents = crate::vectors::integers(dims)?;
        alloc_array(sexp_type(kind)?, &extents)
    })
}

unsafe fn alloc_array(kind: SexpType, extents: &[i32]) -> ApiResult<Sexp> {
    let length = extents
        .iter()
        .try_fold(1isize, |total, &extent| total.checked_mul(extent as isize));
    let length = length.ok_or_else(|| error("'alloc3DArray': too many elements specified"))?;
    let array = alloc_vector(kind, length)?;
    let runtime = runtime();
    let dim = runtime
        .heap
        .alloc_vector(SexpType::Integer, extents.len() as isize)?;
    runtime.heap.root(dim);
    dim.slice_mut::<i32>().copy_from_slice(extents);
    crate::attributes::set_attrib(array, crate::globals::R_DimSymbol.get(), dim)?;
    Ok(array)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocList(length: c_int) -> Sexp {
    enter(|| alloc_list(length as R_xlen_t, SexpType::Pairlist))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocLang(length: c_int) -> Sexp {
    enter(|| alloc_list(length as R_xlen_t, SexpType::Language))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocSExp(kind: SEXPTYPE) -> Sexp {
    enter(|| {
        let kind = sexp_type(kind)?;
        let runtime = runtime();
        let nil = runtime.nil();
        let object = match kind.shape() {
            heap::object::Shape::Environment => runtime.heap.alloc_environment(nil)?,
            heap::object::Shape::Triple => runtime.heap.alloc_triple(kind, nil, nil, nil)?,
            _ => {
                return Err(error(format!(
                    "allocSExp cannot allocate a {}",
                    kind.name()
                )));
            }
        };
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_allocS4Object() -> Sexp {
    enter(|| {
        let runtime = runtime();
        let nil = runtime.nil();
        let object = runtime.heap.alloc_triple(SexpType::Object, nil, nil, nil)?;
        object.set_gp_bits(gp::S4_OBJECT, true);
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ScalarLogical(value: c_int) -> Sexp {
    let value = if value == NA_LOGICAL {
        NA_LOGICAL
    } else {
        (value != 0) as i32
    };
    enter(|| scalar(SexpType::Logical, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ScalarInteger(value: c_int) -> Sexp {
    enter(|| scalar(SexpType::Integer, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ScalarReal(value: f64) -> Sexp {
    enter(|| scalar(SexpType::Real, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ScalarComplex(value: Rcomplex) -> Sexp {
    enter(|| scalar(SexpType::Complex, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ScalarRaw(value: u8) -> Sexp {
    enter(|| scalar(SexpType::Raw, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ScalarString(charsxp: Sexp) -> Sexp {
    enter(|| {
        crate::strings::check_charsxp(charsxp, "ScalarString")?;
        scalar_string(charsxp)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkString(text: *const c_char) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let charsxp = runtime.heap.mk_char(c_bytes(text), Encoding::Native)?;
        runtime.heap.root(charsxp);
        scalar_string(charsxp)
    })
}

/// `mkNamed(type, names)`: a vector whose names come from a `""`-terminated array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkNamed(kind: SEXPTYPE, names: *const *const c_char) -> Sexp {
    enter(|| {
        let mut count = 0;
        while !(*names.add(count)).is_null() && *(*names.add(count)) != 0 {
            count += 1;
        }
        let vector = alloc_vector(sexp_type(kind)?, count as R_xlen_t)?;
        let runtime = runtime();
        let name_vector = runtime
            .heap
            .alloc_vector(SexpType::String, count as isize)?;
        runtime.heap.root(name_vector);
        for index in 0..count {
            let name = runtime
                .heap
                .mk_char(c_bytes(*names.add(index)), Encoding::Native)?;
            runtime.heap.set_element(name_vector, index, name);
        }
        set_names_from(vector, name_vector)?;
        Ok(vector)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_duplicate(object: Sexp) -> Sexp {
    enter(|| duplicate(object, true))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_shallow_duplicate(object: Sexp) -> Sexp {
    enter(|| duplicate(object, false))
}

/// GNU R defers the copy until a write; ry copies now when the object is shared, which callers
/// cannot tell apart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_lazy_duplicate(object: Sexp) -> Sexp {
    enter(|| {
        if object.refcount() > 1 {
            duplicate(object, false)
        } else {
            Ok(object)
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_lengthgets(object: Sexp, length: R_len_t) -> Sexp {
    enter(|| length_gets(object, length as R_xlen_t))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_xlengthgets(object: Sexp, length: R_xlen_t) -> Sexp {
    enter(|| length_gets(object, length))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn DUPLICATE_ATTRIB(to: Sexp, from: Sexp) {
    enter(|| copy_attributes(to, from, true))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SHALLOW_DUPLICATE_ATTRIB(to: Sexp, from: Sexp) {
    enter(|| copy_attributes(to, from, false))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_allocResizableVector(kind: SEXPTYPE, capacity: R_xlen_t) -> Sexp {
    enter(|| {
        let kind = sexp_type(kind)?;
        if !kind.is_vector() || kind == SexpType::Char {
            return Err(HeapError::InvalidVectorType {
                kind,
                length: capacity,
            }
            .into());
        }
        Ok(runtime().heap.alloc_resizable_vector(kind, capacity)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_resizeVector(vector: Sexp, length: R_xlen_t) {
    enter(|| Ok(runtime().heap.resize_vector(vector, length)?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_isResizable(vector: Sexp) -> Rboolean {
    enter(|| {
        Ok(
            (vector.kind().is_vector()
                && !vector.is_altrep()
                && runtime().heap.is_resizable(vector)) as Rboolean,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_maxLength(vector: Sexp) -> R_xlen_t {
    enter(|| {
        if runtime().heap.is_resizable(vector) {
            Ok(vector.capacity())
        } else {
            crate::vectors::xlength(vector)
        }
    })
}

// Protection.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_protect(object: Sexp) -> Sexp {
    enter(|| {
        runtime().heap.roots.protect.protect(object)?;
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_unprotect(count: c_int) {
    enter(|| {
        if count < 0 {
            return Err(error("unprotect(): negative count"));
        }
        Ok(runtime().heap.roots.protect.unprotect(count as usize)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_unprotect_ptr(object: Sexp) {
    enter(|| Ok(runtime().heap.roots.protect.unprotect_object(object)?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ProtectWithIndex(object: Sexp, index: *mut c_int) {
    enter(|| {
        let position = runtime().heap.roots.protect.protect(object)?;
        *index = position as c_int;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_Reprotect(object: Sexp, index: c_int) {
    enter(|| {
        Ok(runtime()
            .heap
            .roots
            .protect
            .reprotect(object, index.max(0) as usize)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_PreserveObject(object: Sexp) {
    enter(|| {
        runtime().heap.preserve(object);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ReleaseObject(object: Sexp) {
    enter(|| {
        runtime().heap.release(object);
        Ok(())
    })
}

/// A precious multiset is a pairlist cell whose car is a growable list and whose tag counts its
/// members. Native code keeps it protected, so its members stay alive through it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_NewPreciousMSet(initial_size: c_int) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let nil = runtime.nil();
        let count = scalar(SexpType::Integer, 0i32)?;
        let size = scalar(SexpType::Integer, initial_size.max(0))?;
        let set = runtime
            .heap
            .alloc_triple(SexpType::Pairlist, nil, size, count)?;
        Ok(set)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_PreserveInMSet(object: Sexp, set: Sexp) {
    enter(|| {
        let runtime = runtime();
        if object == runtime.nil() || object.kind() == SexpType::Symbol {
            return Ok(());
        }
        runtime.heap.root(object);
        let count = *(set.tag().data() as *const i32) as usize;
        let mut store = set.car();
        let capacity = if store == runtime.nil() {
            0
        } else {
            store.length() as usize
        };
        if count >= capacity {
            let initial = *(set.cdr().data() as *const i32) as usize;
            let grown = (capacity * 2).max(initial).max(4);
            let new_store = runtime.heap.alloc_vector(SexpType::List, grown as isize)?;
            for index in 0..count {
                runtime
                    .heap
                    .set_element(new_store, index, store.element(index));
            }
            runtime.heap.set_car(set, new_store);
            store = new_store;
        }
        runtime.heap.set_element(store, count, object);
        *(set.tag().data() as *mut i32) = (count + 1) as i32;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ReleaseFromMSet(object: Sexp, set: Sexp) {
    enter(|| {
        let runtime = runtime();
        let store = set.car();
        if store == runtime.nil() {
            return Ok(());
        }
        let count = *(set.tag().data() as *const i32) as usize;
        if let Some(position) = (0..count)
            .rev()
            .find(|&index| store.element(index) == object)
        {
            for index in position..count - 1 {
                runtime
                    .heap
                    .set_element(store, index, store.element(index + 1));
            }
            runtime.heap.set_element(store, count - 1, runtime.nil());
            *(set.tag().data() as *mut i32) = (count - 1) as i32;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ReleaseMSet(set: Sexp, keep_size: c_int) {
    enter(|| {
        let runtime = runtime();
        let store = set.car();
        if store != runtime.nil() {
            if store.length() > keep_size as isize {
                runtime.heap.set_car(set, runtime.nil());
            } else {
                for index in 0..store.length() as usize {
                    runtime.heap.set_element(store, index, runtime.nil());
                }
            }
        }
        *(set.tag().data() as *mut i32) = 0;
        Ok(())
    })
}

// Collection.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_gc() {
    runtime().heap.collect(Collection::Major);
    run_pending_finalizers();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_gc_running() -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RunPendingFinalizers() {
    run_pending_finalizers();
}

// Transient and C-heap memory.

unsafe fn arena_alloc(bytes: usize) -> ApiResult<*mut c_char> {
    if bytes == 0 {
        return Ok(std::ptr::null_mut());
    }
    let block = runtime().arena.allocate(bytes).map_err(|exhausted| {
        error(format!(
            "cannot allocate memory block of size {:.1} Gb",
            exhausted.bytes as f64 / 1024.0 / 1024.0 / 1024.0
        ))
    })?;
    Ok(block.as_ptr().cast())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_alloc(count: usize, size: c_int) -> *mut c_char {
    enter(|| {
        let bytes = count
            .checked_mul(size.max(0) as usize)
            .ok_or_else(|| error("cannot allocate memory block of size Inf Gb"))?;
        arena_alloc(bytes)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_allocLD(count: usize) -> *mut c_void {
    enter(|| {
        let bytes = count
            .checked_mul(16)
            .ok_or_else(|| error("cannot allocate memory block of size Inf Gb"))?;
        Ok(arena_alloc(bytes)?.cast())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn S_alloc(count: c_long, size: c_int) -> *mut c_char {
    enter(|| {
        let bytes = (count.max(0) as usize)
            .checked_mul(size.max(0) as usize)
            .ok_or_else(|| error("cannot allocate memory block of size Inf Gb"))?;
        let block = arena_alloc(bytes)?;
        if !block.is_null() {
            std::ptr::write_bytes(block, 0, bytes);
        }
        Ok(block)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn S_realloc(
    pointer: *mut c_char,
    new_count: c_long,
    old_count: c_long,
    size: c_int,
) -> *mut c_char {
    enter(|| {
        if new_count <= old_count {
            return Ok(pointer);
        }
        let bytes = (new_count as usize)
            .checked_mul(size.max(0) as usize)
            .ok_or_else(|| error("cannot allocate memory block of size Inf Gb"))?;
        let block = arena_alloc(bytes)?;
        std::ptr::write_bytes(block, 0, bytes);
        if !pointer.is_null() {
            std::ptr::copy_nonoverlapping(
                pointer,
                block,
                old_count.max(0) as usize * size.max(0) as usize,
            );
        }
        Ok(block)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmaxget() -> *mut c_void {
    Arena::encode(runtime().arena.watermark()) as *mut c_void
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmaxset(watermark: *const c_void) {
    runtime().arena.reset(Arena::decode(watermark as usize));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_chk_calloc(count: usize, size: usize) -> *mut c_void {
    enter(|| {
        let block = libc::calloc(count.max(1), size.max(1));
        if block.is_null() {
            return Err(error(format!(
                "'R_Calloc' could not allocate memory ({count} of {size} bytes)"
            )));
        }
        Ok(block)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_chk_realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
    enter(|| {
        let block = if pointer.is_null() {
            libc::malloc(size)
        } else {
            libc::realloc(pointer, size)
        };
        if block.is_null() {
            return Err(error(format!(
                "'R_Realloc' could not re-allocate memory ({size} bytes)"
            )));
        }
        Ok(block)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_chk_free(pointer: *mut c_void) {
    if !pointer.is_null() {
        libc::free(pointer);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_malloc_gc(size: usize) -> *mut c_void {
    libc::malloc(size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_calloc_gc(count: usize, size: usize) -> *mut c_void {
    libc::calloc(count, size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_realloc_gc(pointer: *mut c_void, size: usize) -> *mut c_void {
    libc::realloc(pointer, size)
}
