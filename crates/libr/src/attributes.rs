//! Attributes, classes and the header bits native code reads.

use crate::{
    Rboolean,
    boundary::{ApiResult, enter, error},
    globals::{R_ClassSymbol, R_DimNamesSymbol, R_DimSymbol, R_NamesSymbol, R_RowNamesSymbol},
    runtime,
    strings::c_bytes,
    vectors::{coerce_vector, generic_length, integers, set_sexp_elt, sexp_elt, xlength},
};
use heap::object::{NA_INTEGER, Sexp, SexpType, gp};
use std::ffi::{c_char, c_int};

/// Resolves an attribute name given as a symbol or as a character vector.
unsafe fn attribute_symbol(name: Sexp) -> ApiResult<Sexp> {
    match name.kind() {
        SexpType::Symbol => Ok(name),
        SexpType::String if xlength(name)? >= 1 => {
            let name = sexp_elt(name, 0)?;
            Ok(runtime().heap.install_char(name)?)
        }
        _ => Err(error("invalid type for attribute name")),
    }
}

/// `Rf_getAttrib`. Pairlist names come from the tags, and compact row names expand to
/// `1:n`, as in GNU R.
pub unsafe fn get_attrib(object: Sexp, name: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    if object.kind() == SexpType::Char {
        return Err(error("cannot have attributes on a CHARSXP"));
    }
    let name = attribute_symbol(name)?;
    if name == R_NamesSymbol.get()
        && matches!(
            object.kind(),
            SexpType::Pairlist | SexpType::Language | SexpType::Dots
        )
    {
        return pairlist_names(object);
    }
    let value = raw_attribute(object, name);
    if name == R_RowNamesSymbol.get()
        && value.kind() == SexpType::Integer
        && !value.is_altrep()
        && value.length() == 2
    {
        let values = value.slice::<i32>();
        if values[0] == NA_INTEGER {
            let count = values[1].unsigned_abs() as isize;
            let expanded = runtime.heap.alloc_vector(SexpType::Integer, count)?;
            for (index, slot) in expanded.slice_mut::<i32>().iter_mut().enumerate() {
                *slot = index as i32 + 1;
            }
            return Ok(expanded);
        }
    }
    Ok(value)
}

/// The stored value of an attribute, or `R_NilValue`.
pub unsafe fn raw_attribute(object: Sexp, name: Sexp) -> Sexp {
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == name {
            return cell.car();
        }
        cell = cell.cdr();
    }
    runtime().nil()
}

unsafe fn pairlist_names(list: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let mut tags = Vec::new();
    let mut cell = list;
    while matches!(
        cell.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        tags.push(cell.tag());
        cell = cell.cdr();
    }
    if tags.iter().all(|tag| *tag == runtime.nil()) {
        return Ok(runtime.nil());
    }
    let names = runtime
        .heap
        .alloc_vector(SexpType::String, tags.len() as isize)?;
    runtime.heap.root(names);
    for (index, tag) in tags.iter().enumerate() {
        if tag.kind() == SexpType::Symbol {
            runtime.heap.set_element(names, index, tag.car());
        }
    }
    Ok(names)
}

/// `Rf_setAttrib`, with the checks and conversions R applies to `names`, `dim`, `dimnames`,
/// `class` and `row.names`. Setting `NULL` removes the attribute.
pub unsafe fn set_attrib(object: Sexp, name: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let nil = runtime.nil();
    if object == nil {
        return Err(error("attempt to set an attribute on NULL"));
    }
    if object.kind() == SexpType::Char {
        return Err(error("cannot set attribute on a CHARSXP"));
    }
    let name = attribute_symbol(name)?;
    runtime.heap.root(name);
    runtime.heap.root(value);
    if value == nil {
        remove_attribute(object, name);
        if name == R_ClassSymbol.get() {
            object.set_object(false);
        }
        return Ok(value);
    }
    if name == R_NamesSymbol.get() {
        return names_gets(object, value);
    }
    if name == R_DimSymbol.get() {
        return dim_gets(object, value);
    }
    if name == R_DimNamesSymbol.get() {
        return dimnames_gets(object, value);
    }
    if name == R_ClassSymbol.get() {
        return class_gets(object, value);
    }
    if name == R_RowNamesSymbol.get() {
        return row_names_gets(object, value);
    }
    install_attribute(object, name, value)?;
    Ok(value)
}

/// Replaces or appends an attribute without any checks.
pub unsafe fn install_attribute(object: Sexp, name: Sexp, value: Sexp) -> ApiResult<()> {
    crate::vectors::refuse_native(object)?;
    let runtime = runtime();
    runtime.heap.root(object);
    let mut cell = object.attrib();
    let mut last = None;
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == name {
            runtime.heap.set_car(cell, value);
            return Ok(());
        }
        last = Some(cell);
        cell = cell.cdr();
    }
    let nil = runtime.nil();
    let new_cell = runtime
        .heap
        .alloc_triple(SexpType::Pairlist, value, nil, name)?;
    match last {
        Some(last) => runtime.heap.set_cdr(last, new_cell),
        None => runtime.heap.set_attrib(object, new_cell),
    }
    Ok(())
}

pub unsafe fn remove_attribute(object: Sexp, name: Sexp) {
    let runtime = runtime();
    if name == R_NamesSymbol.get()
        && matches!(
            object.kind(),
            SexpType::Pairlist | SexpType::Language | SexpType::Dots
        )
    {
        let mut cell = object;
        while matches!(
            cell.kind(),
            SexpType::Pairlist | SexpType::Language | SexpType::Dots
        ) {
            runtime.heap.set_tag(cell, runtime.nil());
            cell = cell.cdr();
        }
        return;
    }
    let mut previous: Option<Sexp> = None;
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == name {
            match previous {
                Some(previous) => runtime.heap.set_cdr(previous, cell.cdr()),
                None => runtime.heap.set_attrib(object, cell.cdr()),
            }
            return;
        }
        previous = Some(cell);
        cell = cell.cdr();
    }
}

pub unsafe fn names_gets(object: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let nil = runtime.nil();
    if value == nil {
        remove_attribute(object, R_NamesSymbol.get());
        return Ok(value);
    }
    let names = if value.kind() == SexpType::String && !value.is_object() {
        value
    } else {
        coerce_vector(value, SexpType::String)?
    };
    runtime.heap.root(names);
    let target_length = generic_length(object)?;
    let names_length = xlength(names)?;
    if names_length > target_length {
        return Err(error(format!(
            "'names' attribute [{names_length}] must be the same length as the vector [{target_length}]"
        )));
    }
    if matches!(
        object.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        let mut cell = object;
        let mut index = 0;
        while matches!(
            cell.kind(),
            SexpType::Pairlist | SexpType::Language | SexpType::Dots
        ) {
            let name = if (index as isize) < names_length {
                sexp_elt(names, index)?
            } else {
                nil
            };
            let tag = if name != nil && !name.bytes().is_empty() {
                runtime.heap.install_char(name)?
            } else {
                nil
            };
            runtime.heap.set_tag(cell, tag);
            cell = cell.cdr();
            index += 1;
        }
        return Ok(value);
    }
    let names = if names_length < target_length {
        let padded = runtime.heap.alloc_vector(SexpType::String, target_length)?;
        runtime.heap.root(padded);
        let na = runtime.heap.known().na_string;
        for index in 0..target_length as usize {
            let element = if (index as isize) < names_length {
                sexp_elt(names, index)?
            } else {
                na
            };
            runtime.heap.set_element(padded, index, element);
        }
        padded
    } else {
        names
    };
    install_attribute(object, R_NamesSymbol.get(), names)?;
    Ok(names)
}

pub unsafe fn dim_gets(object: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    if !object.kind().is_vector() && !object.is_altrep() {
        return Err(error(
            "invalid first argument, must be vector (list or atomic)",
        ));
    }
    let length = xlength(object)?;
    let dims = match value.kind() {
        SexpType::Integer | SexpType::Real | SexpType::Logical => integers(value)?,
        _ => return Err(error("invalid second argument, must be vector or NULL")),
    };
    if dims.is_empty() {
        return Err(error("length-0 dimension vector is invalid"));
    }
    if dims.contains(&NA_INTEGER) {
        return Err(error("the dims contain missing or negative values"));
    }
    if dims.iter().any(|&extent| extent < 0) {
        return Err(error("the dims contain negative values"));
    }
    let product = dims.iter().map(|&extent| extent as i64).product::<i64>();
    if product != length as i64 {
        return Err(error(format!(
            "dims [product {product}] do not match the length of object [{length}]"
        )));
    }
    remove_attribute(object, R_NamesSymbol.get());
    remove_attribute(object, R_DimNamesSymbol.get());
    let dim = runtime
        .heap
        .alloc_vector(SexpType::Integer, dims.len() as isize)?;
    runtime.heap.root(dim);
    dim.slice_mut::<i32>().copy_from_slice(&dims);
    install_attribute(object, R_DimSymbol.get(), dim)?;
    dim.mark_not_mutable();
    Ok(dim)
}

pub unsafe fn dimnames_gets(object: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let dim = raw_attribute(object, R_DimSymbol.get());
    if dim.kind() != SexpType::Integer {
        return Err(error("'dimnames' applied to non-array"));
    }
    let list = match value.kind() {
        SexpType::List => value,
        SexpType::Pairlist => crate::lists::pairlist_to_vector(value)?,
        _ => return Err(error("'dimnames' must be a list")),
    };
    runtime.heap.root(list);
    let extents = integers(dim)?;
    let list_length = xlength(list)?;
    if list_length as usize != extents.len() {
        return Err(error(format!(
            "length of 'dimnames' [{list_length}] must match that of 'dims' [{}]",
            extents.len()
        )));
    }
    for (index, &extent) in extents.iter().enumerate() {
        let names = sexp_elt(list, index)?;
        if names == runtime.nil() {
            continue;
        }
        let names = if names.kind() == SexpType::String {
            names
        } else {
            coerce_vector(names, SexpType::String)?
        };
        if xlength(names)? != extent as isize {
            return Err(error(format!(
                "length of 'dimnames' [{}] not equal to array extent",
                index + 1
            )));
        }
        set_sexp_elt(list, index, names)?;
    }
    install_attribute(object, R_DimNamesSymbol.get(), list)?;
    Ok(list)
}

pub unsafe fn class_gets(object: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    if value == runtime.nil() || (value.kind() == SexpType::String && xlength(value)? == 0) {
        remove_attribute(object, R_ClassSymbol.get());
        object.set_object(false);
        return Ok(value);
    }
    if value.kind() != SexpType::String {
        return Err(error("attempt to set invalid 'class' attribute"));
    }
    let mut factor = false;
    for index in 0..xlength(value)? as usize {
        factor |= sexp_elt(value, index)?.bytes() == b"factor";
    }
    if factor && object.kind() != SexpType::Integer {
        return Err(error("adding class \"factor\" to an invalid object"));
    }
    install_attribute(object, R_ClassSymbol.get(), value)?;
    object.set_object(true);
    Ok(value)
}

/// `row.names` that are `1:n` are stored in GNU R's compact form, `c(NA, -n)`.
unsafe fn row_names_gets(object: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    if value.kind() == SexpType::Integer {
        let values = integers(value)?;
        let sequential = values
            .iter()
            .enumerate()
            .all(|(index, &value)| value == index as i32 + 1);
        if sequential && !values.is_empty() {
            let compact = runtime.heap.alloc_vector(SexpType::Integer, 2)?;
            runtime.heap.root(compact);
            compact
                .slice_mut::<i32>()
                .copy_from_slice(&[NA_INTEGER, -(values.len() as i32)]);
            install_attribute(object, R_RowNamesSymbol.get(), compact)?;
            return Ok(value);
        }
    }
    install_attribute(object, R_RowNamesSymbol.get(), value)?;
    Ok(value)
}

pub unsafe fn inherits(object: Sexp, class: &str) -> ApiResult<bool> {
    if !object.is_object() {
        return Ok(false);
    }
    let classes = raw_attribute(object, R_ClassSymbol.get());
    if classes.kind() != SexpType::String {
        return Ok(false);
    }
    for index in 0..xlength(classes)? as usize {
        if sexp_elt(classes, index)?.bytes() == class.as_bytes() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_getAttrib(object: Sexp, name: Sexp) -> Sexp {
    enter(|| get_attrib(object, name))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_setAttrib(object: Sexp, name: Sexp, value: Sexp) -> Sexp {
    enter(|| set_attrib(object, name, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_classgets(object: Sexp, value: Sexp) -> Sexp {
    enter(|| class_gets(object, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_namesgets(object: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        names_gets(object, value)?;
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_dimgets(object: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        dim_gets(object, value)?;
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_dimnamesgets(object: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        dimnames_gets(object, value)?;
        Ok(object)
    })
}

/// `Rf_GetRowNames`: the first element of a `dimnames` list.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetRowNames(dimnames: Sexp) -> Sexp {
    enter(|| {
        if dimnames == runtime().nil() {
            return Ok(dimnames);
        }
        crate::vectors::sexp_elt(dimnames, 0)
    })
}

/// `Rf_copyMostAttrib`: every attribute except `names`, `dim` and `dimnames`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_copyMostAttrib(from: Sexp, to: Sexp) {
    enter(|| {
        let runtime = runtime();
        if to == runtime.nil() {
            return Err(error("attempt to set an attribute on NULL"));
        }
        let mut cell = from.attrib();
        while cell.kind() == SexpType::Pairlist {
            let tag = cell.tag();
            if tag != R_NamesSymbol.get()
                && tag != R_DimSymbol.get()
                && tag != R_DimNamesSymbol.get()
            {
                install_attribute(to, tag, cell.car())?;
            }
            cell = cell.cdr();
        }
        to.set_object(from.is_object());
        to.set_gp_bits(gp::S4_OBJECT, from.has_gp(gp::S4_OBJECT));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_inherits(object: Sexp, class: *const c_char) -> Rboolean {
    enter(|| {
        let class = String::from_utf8_lossy(c_bytes(class)).into_owned();
        Ok(inherits(object, &class)? as Rboolean)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ATTRIB(object: Sexp) -> Sexp {
    object.attrib()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_ATTRIB(object: Sexp, value: Sexp) {
    enter(|| {
        crate::vectors::refuse_native(object)?;
        if !matches!(value.kind(), SexpType::Nil | SexpType::Pairlist) {
            return Err(error(format!(
                "value of 'SET_ATTRIB' must be a pairlist or NULL, not a '{}'",
                value.kind().name()
            )));
        }
        runtime().heap.set_attrib(object, value);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ANY_ATTRIB(object: Sexp) -> c_int {
    (object.attrib() != runtime().nil()) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn CLEAR_ATTRIB(object: Sexp) {
    let runtime = runtime();
    runtime.heap.set_attrib(object, runtime.nil());
    object.set_object(false);
    object.set_gp_bits(gp::S4_OBJECT, false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn OBJECT(object: Sexp) -> c_int {
    object.is_object() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_OBJECT(object: Sexp, value: c_int) {
    object.set_object(value != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_S4_OBJECT(object: Sexp) -> c_int {
    object.has_gp(gp::S4_OBJECT) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_S4_OBJECT(object: Sexp) {
    object.set_gp_bits(gp::S4_OBJECT, true);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn UNSET_S4_OBJECT(object: Sexp) {
    object.set_gp_bits(gp::S4_OBJECT, false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn LEVELS(object: Sexp) -> c_int {
    object.gp() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SETLEVELS(object: Sexp, value: c_int) -> c_int {
    object.set_gp(value as u16);
    value
}

/// Since R 4.0, `NAMED` reports the reference count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn NAMED(object: Sexp) -> c_int {
    object.refcount() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn REFCNT(object: Sexp) -> c_int {
    object.refcount() as c_int
}

/// A no-op since R 4.0, where reference counts replaced `NAMED`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_NAMED(_object: Sexp, _value: c_int) {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn MARK_NOT_MUTABLE(object: Sexp) {
    object.mark_not_mutable();
}

/// The attribute names in storage order, as symbols. Pairlists have their tags as names.
unsafe fn attribute_names(object: Sexp) -> Vec<Sexp> {
    let mut names = Vec::new();
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        names.push(cell.tag());
        cell = cell.cdr();
    }
    names
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getAttribCount(object: Sexp) -> crate::R_xlen_t {
    enter(|| Ok(attribute_names(object).len() as crate::R_xlen_t))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getAttribNames(object: Sexp) -> Sexp {
    enter(|| {
        let names = attribute_names(object);
        let runtime = runtime();
        let result = runtime
            .heap
            .alloc_vector(SexpType::String, names.len() as isize)?;
        for (index, name) in names.into_iter().enumerate() {
            runtime.heap.set_element(result, index, name.car());
        }
        Ok(result)
    })
}

/// `R_getAttributes(x)`: the attributes as a named list, as `attributes(x)` returns them, with
/// compact row names expanded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getAttributes(object: Sexp) -> Sexp {
    enter(|| {
        let names = attribute_names(object);
        let runtime = runtime();
        if names.is_empty() {
            return Ok(runtime.nil());
        }
        let result = runtime
            .heap
            .alloc_vector(SexpType::List, names.len() as isize)?;
        runtime.heap.root(result);
        let labels = runtime
            .heap
            .alloc_vector(SexpType::String, names.len() as isize)?;
        runtime.heap.root(labels);
        for (index, &name) in names.iter().enumerate() {
            let value = get_attrib(object, name)?;
            runtime.heap.set_element(result, index, value);
            runtime.heap.set_element(labels, index, name.car());
        }
        install_attribute(result, R_NamesSymbol.get(), labels)?;
        Ok(result)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_hasAttrib(object: Sexp, name: Sexp) -> bool {
    enter(|| {
        let symbol = attribute_symbol(name)?;
        Ok(raw_attribute(object, symbol).kind() != SexpType::Nil)
    })
}

/// The row count R reports for `x`: a data frame's row names, a matrix's first dimension, or a
/// vector's length.
unsafe fn row_count(object: Sexp) -> ApiResult<crate::R_xlen_t> {
    if inherits(object, "data.frame")? {
        let row_names = get_attrib(object, R_RowNamesSymbol.get())?;
        return xlength(row_names);
    }
    let dim = raw_attribute(object, R_DimSymbol.get());
    if dim.kind() == SexpType::Integer && dim.length() >= 1 {
        return Ok(integers(dim)?[0] as crate::R_xlen_t);
    }
    generic_length(object)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_nrow(object: Sexp) -> crate::R_xlen_t {
    enter(|| row_count(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ncol(object: Sexp) -> crate::R_xlen_t {
    enter(|| {
        if inherits(object, "data.frame")? {
            return generic_length(object);
        }
        let dim = raw_attribute(object, R_DimSymbol.get());
        if dim.kind() == SexpType::Integer && dim.length() >= 2 {
            return Ok(integers(dim)?[1] as crate::R_xlen_t);
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_nlevels(factor: Sexp) -> c_int {
    enter(|| {
        let levels = get_attrib(factor, crate::globals::R_LevelsSymbol.get())?;
        Ok(if levels.kind() == SexpType::Nil {
            0
        } else {
            xlength(levels)? as c_int
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetColNames(dimnames: Sexp) -> Sexp {
    enter(|| dimnames_element(dimnames, 1))
}

unsafe fn dimnames_element(dimnames: Sexp, index: usize) -> ApiResult<Sexp> {
    if dimnames.kind() == SexpType::List && dimnames.length() as usize > index {
        Ok(dimnames.element(index))
    } else {
        Ok(runtime().nil())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetArrayDimnames(object: Sexp) -> Sexp {
    enter(|| get_attrib(object, R_DimNamesSymbol.get()))
}

/// `Rf_GetMatrixDimnames(x, rl, cl, rn, cn)`: the row and column names of a matrix, and the
/// names of the dimnames themselves as C strings (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetMatrixDimnames(
    object: Sexp,
    row_labels: *mut Sexp,
    column_labels: *mut Sexp,
    row_title: *mut *const c_char,
    column_title: *mut *const c_char,
) {
    enter(|| {
        let dimnames = get_attrib(object, R_DimNamesSymbol.get())?;
        let nil = runtime().nil();
        *row_labels = nil;
        *column_labels = nil;
        *row_title = std::ptr::null();
        *column_title = std::ptr::null();
        if dimnames.kind() != SexpType::List || dimnames.length() < 2 {
            return Ok(());
        }
        *row_labels = dimnames.element(0);
        *column_labels = dimnames.element(1);
        let names = raw_attribute(dimnames, R_NamesSymbol.get());
        if names.kind() == SexpType::String && names.length() >= 2 {
            *row_title = crate::strings::arena_c_string(names.element(0).bytes())?;
            *column_title = crate::strings::arena_c_string(names.element(1).bytes())?;
        }
        Ok(())
    })
}
