//! Subsetting and subassignment (`[`, `[[`, `$`, `@` and their replacement forms), `c` and
//! `unlist`, lengths, names, dims, and attributes.

use crate::{
    builtins::{
        Arg, BuiltinDef, Call, Generic, builtin, internal, internal_special, present, special,
    },
    coerce::{self, coerce_vector},
    conditions,
    eval::{self, value_of},
    unwind::{R, Unwind, error},
    values::{
        self, alloc, char_str, get_attr, length, missing_arg, na_string, nil, scalar_integer,
        string_vector, symbol_name, symbols, unbound,
    },
};
use heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp};

pub const BUILTINS: &[BuiltinDef] = &[
    special("[", -1, do_subset).dispatch_or_eval(),
    special("[[", -1, do_subset2).dispatch_or_eval(),
    special("$", 2, do_dollar),
    special("@", 2, do_at),
    special("[<-", -1, do_subassign).dispatch_or_eval(),
    special("[[<-", -1, do_subassign2).dispatch_or_eval(),
    special("$<-", 3, do_dollar_assign),
    special("@<-", 3, do_at_assign),
    builtin(".subset", -1, do_subset).keep_missing(),
    builtin(".subset2", -1, do_subset2).keep_missing(),
    builtin("c", -1, do_c).generic(Generic::Internal),
    internal("unlist", 3, do_unlist).generic(Generic::Internal),
    builtin("list", -1, do_list),
    builtin("length", 1, do_length).generic(Generic::Internal),
    builtin("length<-", 2, do_set_length).generic(Generic::Internal),
    internal("lengths", 2, do_lengths),
    builtin("names", 1, do_names).generic(Generic::Internal),
    builtin("names<-", 2, do_set_names).generic(Generic::Internal),
    builtin("dim", 1, do_dim).generic(Generic::Internal),
    builtin("dim<-", 2, do_set_dim).generic(Generic::Internal),
    builtin("dimnames", 1, do_dimnames).generic(Generic::Internal),
    builtin("dimnames<-", 2, do_set_dimnames).generic(Generic::Internal),
    builtin("levels<-", 2, do_set_levels).generic(Generic::Internal),
    builtin("attr", -1, do_attr),
    builtin("attr<-", 3, do_set_attr),
    builtin("attributes", 1, do_attributes),
    builtin("attributes<-", 2, do_set_attributes),
    internal("matrix", 7, do_matrix),
    internal("array", 3, do_array),
    internal("t.default", 1, do_transpose),
    internal("max.col", 2, do_max_col),
    internal("aperm", 3, do_aperm),
    internal("drop", 1, do_drop),
    internal_special("cbind", -1, |call| unsafe { bind(call, false) }),
    internal_special("rbind", -1, |call| unsafe { bind(call, true) }),
    internal("rev", 1, do_rev),
    internal("islistfactor", 2, do_is_list_factor),
    internal("comment", 1, |call| unsafe {
        Ok(get_attr(call.arg(0), symbols().comment))
    }),
    internal("comment<-", 2, |call| unsafe {
        {
            let object = modifiable(call.arg(0))?;
            values::set_attr(object, symbols().comment, call.arg(1))?;
            Ok(object)
        }
    }),
    internal("row", 1, |call| unsafe { row_col(call, true) }),
    internal("col", 1, |call| unsafe { row_col(call, false) }),
    internal("diag", 3, do_diag),
    internal("shortRowNames", 2, do_short_row_names),
    internal("copyDFattr", 2, do_copy_df_attr),
];

/// A copy of `object` that may be modified in place.
pub unsafe fn modifiable(object: Sexp) -> R<Sexp> {
    if values::maybe_shared(object) {
        values::shallow_duplicate(object)
    } else {
        Ok(object)
    }
}

/// Element `index` of any vector as a length-one vector (or, for lists, the element).
pub unsafe fn vector_element(vector: Sexp, index: usize) -> R<Sexp> {
    let vector = values::materialize(vector)?;
    match vector.kind() {
        SexpType::List | SexpType::Expression => Ok(vector.element(index)),
        SexpType::Logical => values::fresh_logical(values::ints(vector)[index]),
        SexpType::Integer => values::scalar_integer(values::ints(vector)[index]),
        SexpType::Real => values::scalar_real(values::reals(vector)[index]),
        SexpType::Complex => values::scalar_complex(values::complexes(vector)[index]),
        SexpType::String => values::scalar_string_charsxp(vector.element(index)),
        SexpType::Raw => values::scalar_raw(values::raws(vector)[index]),
        _ => Err(error("object is not subsettable")),
    }
}

/// Like `vector_element`, and for a pairlist the element at that position.
pub unsafe fn list_or_vector_element(vector: Sexp, index: usize) -> R<Sexp> {
    match vector.kind() {
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            let mut cell = vector;
            for _ in 0..index {
                cell = cell.cdr();
            }
            Ok(cell.car())
        }
        _ => vector_element(vector, index),
    }
}

/// A numeric element as a double, for index and dimension vectors.
pub unsafe fn numeric_element(vector: Sexp, index: usize) -> f64 {
    crate::arith::real_at(vector, index)
}

/// Copies element `from_index` of `from` into `to[to_index]`, converting to `to`'s type.
pub unsafe fn copy_element(from: Sexp, from_index: usize, to: Sexp, to_index: usize) -> R<()> {
    let from = values::materialize(from)?;
    if from.kind() == to.kind() {
        match to.kind() {
            SexpType::Logical | SexpType::Integer => {
                values::ints_mut(to)[to_index] = values::ints(from)[from_index]
            }
            SexpType::Real => values::reals_mut(to)[to_index] = values::reals(from)[from_index],
            SexpType::Complex => {
                values::complexes_mut(to)[to_index] = values::complexes(from)[from_index]
            }
            SexpType::Raw => values::raws_mut(to)[to_index] = values::raws(from)[from_index],
            SexpType::String | SexpType::List | SexpType::Expression => {
                values::set_elt(to, to_index, from.element(from_index))
            }
            _ => {}
        }
        return Ok(());
    }
    match to.kind() {
        SexpType::List | SexpType::Expression => {
            let element = vector_element(from, from_index)?;
            values::set_elt(to, to_index, element);
        }
        SexpType::String => {
            let charsxp = coerce::element_as_charsxp(from, from_index)?;
            values::set_elt(to, to_index, charsxp);
        }
        _ => {
            let single = vector_element(from, from_index)?;
            let converted = coerce_vector(single, to.kind())?;
            copy_element(converted, 0, to, to_index)?;
        }
    }
    Ok(())
}

/// Sets `to[to_index]` to the type's NA (or `NULL` for lists).
unsafe fn set_na(to: Sexp, to_index: usize) {
    match to.kind() {
        SexpType::Logical | SexpType::Integer => values::ints_mut(to)[to_index] = NA_INTEGER,
        SexpType::Real => values::reals_mut(to)[to_index] = NA_REAL,
        SexpType::Complex => {
            values::complexes_mut(to)[to_index] = Rcomplex {
                r: NA_REAL,
                i: NA_REAL,
            }
        }
        SexpType::String => values::set_elt(to, to_index, na_string()),
        SexpType::List | SexpType::Expression => values::set_elt(to, to_index, nil()),
        SexpType::Raw => values::raws_mut(to)[to_index] = 0,
        _ => {}
    }
}

/// A vector of the elements at `indices` (`usize::MAX` gives NA).
pub unsafe fn select_elements(vector: Sexp, indices: &[usize]) -> R<Sexp> {
    let vector = values::materialize(vector)?;
    let kind = if vector == nil() {
        SexpType::Nil
    } else {
        vector.kind()
    };
    if kind == SexpType::Nil {
        return Ok(nil());
    }
    if matches!(kind, SexpType::Pairlist | SexpType::Language) {
        let list = values::pairlist_to_list(vector)?;
        return select_elements(list, indices);
    }
    let count = length(vector);
    let result = alloc(kind, indices.len())?;
    for (position, &index) in indices.iter().enumerate() {
        if index < count {
            copy_element(vector, index, result, position)?;
        } else {
            set_na(result, position);
        }
    }
    Ok(result)
}

/// A `length.out`-style argument as a count.
pub unsafe fn length_argument(value: Sexp) -> R<usize> {
    if length(value) != 1 {
        return Err(error("invalid 'length' argument"));
    }
    let count = values::as_real_scalar(value);
    if count.is_nan() || count < 0.0 {
        return Err(error("vector: cannot make a vector of negative length"));
    }
    Ok(count as usize)
}

/// `length()` with S3 dispatch, for builtins that need an object's length.
pub unsafe fn dispatch_length(object: Sexp, env: Sexp) -> R<usize> {
    if !object.is_object() {
        return Ok(length(object));
    }
    let function = crate::builtins::primitive("length").ok_or_else(|| error("no length"))?;
    let value = eval::apply(nil(), function, &eval::value_args(&[object]), env)?;
    Ok(values::as_integer_scalar(value).max(0) as usize)
}

// Index resolution.

/// One resolved subscript: a zero-based position (possibly past the end), or NA.
type Position = Option<usize>;

/// Resolves a subscript against an extent. `names` are the names to match character
/// subscripts against; unmatched names give positions past the end, with the names to give
/// them when assigning.
unsafe fn resolve(
    index: Sexp,
    extent: usize,
    names: Sexp,
    assigning: bool,
    call: Sexp,
) -> R<(Vec<Position>, Vec<Option<Sexp>>)> {
    if index == missing_arg() {
        return Ok(((0..extent).map(Some).collect(), Vec::new()));
    }
    let index = values::materialize(index)?;
    let index = if values::inherits(index, "factor") {
        coerce_vector(index, SexpType::Integer)?
    } else {
        index
    };
    match index.kind() {
        SexpType::Nil => Ok((Vec::new(), Vec::new())),
        SexpType::Logical => {
            let flags = values::ints(index);
            let count = flags.len();
            if count == 0 {
                return Ok((Vec::new(), Vec::new()));
            }
            let total = extent.max(count);
            let mut positions = Vec::new();
            for position in 0..total {
                match flags[position % count] {
                    1 => positions.push(Some(position)),
                    NA_LOGICAL => positions.push(None),
                    _ => {}
                }
            }
            Ok((positions, Vec::new()))
        }
        SexpType::Integer | SexpType::Real => {
            let count = length(index);
            let mut any_negative = false;
            let mut any_positive = false;
            let mut any_na = false;
            for position in 0..count {
                let value = numeric_element(index, position);
                if value.is_nan() {
                    any_na = true;
                } else if value < 0.0 && value.trunc() != 0.0 {
                    any_negative = true;
                } else if value >= 1.0 {
                    any_positive = true;
                }
            }
            if any_negative {
                if any_positive || any_na {
                    return Err(crate::unwind::error_in(
                        call,
                        "can't mix positive and negative subscripts",
                    ));
                }
                let mut keep = vec![true; extent];
                for position in 0..count {
                    let value = (-numeric_element(index, position)).trunc() as usize;
                    if value >= 1 && value <= extent {
                        keep[value - 1] = false;
                    }
                }
                let positions = keep
                    .iter()
                    .enumerate()
                    .filter(|(_, kept)| **kept)
                    .map(|(position, _)| Some(position))
                    .collect();
                return Ok((positions, Vec::new()));
            }
            let mut positions = Vec::with_capacity(count);
            for position in 0..count {
                let value = numeric_element(index, position);
                if value.is_nan() {
                    positions.push(None);
                } else if value >= 1.0 {
                    let position = value.trunc() as usize - 1;
                    if position >= extent && !assigning && false {
                        positions.push(None);
                    } else {
                        positions.push(Some(position));
                    }
                }
            }
            Ok((positions, Vec::new()))
        }
        SexpType::String => {
            let count = length(index);
            let mut positions = Vec::with_capacity(count);
            let mut new_names: Vec<Option<Sexp>> = Vec::new();
            let mut appended: Vec<Sexp> = Vec::new();
            for position in 0..count {
                let wanted = index.element(position);
                let found = if wanted == na_string() || names == nil() {
                    None
                } else {
                    (0..length(names)).find(|&candidate| {
                        let name = names.element(candidate);
                        name == wanted && !values::is_blank(name)
                    })
                };
                match found {
                    Some(found) => positions.push(Some(found)),
                    None => {
                        if assigning {
                            let existing = appended
                                .iter()
                                .position(|&name| name == wanted && wanted != na_string());
                            let slot = match existing {
                                Some(existing) => extent + existing,
                                None => {
                                    appended.push(wanted);
                                    extent + appended.len() - 1
                                }
                            };
                            positions.push(Some(slot));
                        } else {
                            positions.push(Some(usize::MAX));
                        }
                    }
                }
            }
            new_names.extend(appended.into_iter().map(Some));
            Ok((positions, new_names))
        }
        SexpType::Symbol => {
            let name = values::scalar_string_charsxp(index.car())?;
            resolve(name, extent, names, assigning, call)
        }
        SexpType::Complex => Err(crate::unwind::error_in(
            call,
            "invalid subscript type 'complex'",
        )),
        SexpType::List => Err(crate::unwind::error_in(
            call,
            "invalid subscript type 'list'",
        )),
        kind => Err(crate::unwind::error_in(
            call,
            format!("invalid subscript type '{}'", kind.name()),
        )),
    }
}

/// Splits a builtin's arguments into the object, the subscripts, and `drop`/`exact`.
unsafe fn subset_args(call: &Call<'_>) -> (Sexp, Vec<Sexp>, Option<Sexp>, Option<Sexp>) {
    let mut object = nil();
    let mut indices = Vec::new();
    let mut drop = None;
    let mut exact = None;
    let mut first = true;
    for arg in call.args {
        if arg.tag == symbols().drop {
            drop = Some(arg.value);
            continue;
        }
        if arg.tag == symbols().exact {
            exact = Some(arg.value);
            continue;
        }
        if first {
            object = arg.value;
            first = false;
        } else {
            indices.push(arg.value);
        }
    }
    (object, indices, drop, exact)
}

// `[`.

unsafe fn do_subset(call: &Call<'_>) -> R<Sexp> {
    let (object, indices, drop, _) = subset_args(call);
    let drop = drop.is_none_or(|drop| values::as_logical_scalar(drop) != 0);
    subset(object, &indices, drop, call.call)
}

pub unsafe fn subset(object: Sexp, indices: &[Sexp], drop: bool, call: Sexp) -> R<Sexp> {
    let object = values::materialize(object)?;
    if object.kind() == SexpType::Object && object.has_gp(gp::S4_OBJECT) {
        let data = s4_data_part(object);
        if data == nil() {
            return Err(crate::unwind::error_in(
                call,
                "object of type 'S4' is not subsettable",
            ));
        }
        return subset(data, indices, drop, call);
    }
    match object.kind() {
        SexpType::Nil => return Ok(nil()),
        SexpType::Environment => {
            return Err(crate::unwind::error_in(
                call,
                "object of type 'environment' is not subsettable",
            ));
        }
        SexpType::Closure | SexpType::Builtin | SexpType::Special => {
            return Err(crate::unwind::error_in(
                call,
                "object of type 'closure' is not subsettable",
            ));
        }
        SexpType::Symbol => {
            return Err(crate::unwind::error_in(
                call,
                "object of type 'symbol' is not subsettable",
            ));
        }
        SexpType::Pairlist | SexpType::Language => {
            let list = values::pairlist_to_list(object)?;
            let result = subset(list, indices, drop, call)?;
            // Subsetting a pairlist gives a list, as in GNU R; a call stays a call.
            if object.kind() == SexpType::Language {
                return values::list_to_pairlist(result, SexpType::Language);
            }
            return Ok(result);
        }
        _ => {}
    }
    let dims = values::dims(object);
    if indices.len() >= 2 {
        let Some(dims) = dims else {
            return Err(crate::unwind::error_in(
                call,
                "incorrect number of dimensions",
            ));
        };
        if dims.len() != indices.len() {
            return Err(crate::unwind::error_in(
                call,
                "incorrect number of dimensions",
            ));
        }
        return array_subset(object, indices, &dims, drop, call);
    }
    let index = indices.first().copied().unwrap_or(missing_arg());
    // A matrix with one column per dimension selects single elements.
    if let Some(dims) = &dims
        && let Some(positions) = matrix_index_positions(object, dims, index, call)?
    {
        let positions: Vec<usize> = positions
            .into_iter()
            .map(|position| position.unwrap_or(usize::MAX))
            .collect();
        return select_elements(object, &positions);
    }
    let count = length(object);
    let names = values::names(object);
    let (positions, _) = resolve(index, count, names, false, call)?;
    let indices: Vec<usize> = positions
        .iter()
        .map(|position| position.unwrap_or(usize::MAX))
        .collect();
    let result = select_elements(object, &indices)?;
    if names != nil() {
        let new_names = alloc(SexpType::String, indices.len())?;
        for (position, &index) in indices.iter().enumerate() {
            let name = if index < count {
                names.element(index)
            } else if object.kind() == SexpType::List || object.kind() == SexpType::Expression {
                if index == usize::MAX && positions[position].is_none() {
                    na_string()
                } else {
                    na_string()
                }
            } else {
                na_string()
            };
            values::set_elt(new_names, position, name);
        }
        // An out-of-range name on a list subset becomes `NA`; character subscripts keep `NA`.
        values::set_attr(result, symbols().names, new_names)?;
    }
    // A one-dimensional array stays one when indexed.
    if let Some(dims) = dims
        && dims.len() == 1
        && index != missing_arg()
    {
        let dim = values::integer_vector(&[indices.len() as i32])?;
        values::set_attr(result, symbols().dim, dim)?;
        let dimnames = get_attr(object, symbols().dimnames);
        if dimnames != nil() && dimnames.element(0) != nil() {
            let selected = select_elements(dimnames.element(0), &indices)?;
            let new_dimnames = values::list_vector(&[selected])?;
            values::set_attr(result, symbols().dimnames, new_dimnames)?;
            values::remove_attr(result, symbols().names);
        }
    }
    if index == missing_arg() {
        // `x[]` keeps every attribute.
        values::copy_attributes(object, result)?;
    } else if object.kind() == SexpType::Expression || values::inherits(object, "factor") {
        let levels = get_attr(object, symbols().levels);
        if levels != nil() {
            values::set_attr(result, symbols().levels, levels)?;
        }
    }
    Ok(result)
}

unsafe fn array_subset(
    object: Sexp,
    indices: &[Sexp],
    dims: &[usize],
    drop: bool,
    call: Sexp,
) -> R<Sexp> {
    let dimnames = get_attr(object, symbols().dimnames);
    let mut selections: Vec<Vec<usize>> = Vec::with_capacity(dims.len());
    for (dimension, (&index, &extent)) in indices.iter().zip(dims).enumerate() {
        let names = if dimnames.kind() == SexpType::List {
            dimnames.element(dimension)
        } else {
            nil()
        };
        let (positions, _) = resolve(index, extent, names, false, call)?;
        let mut selection = Vec::with_capacity(positions.len());
        for position in positions {
            match position {
                Some(position) if position < extent => selection.push(position),
                Some(_) => return Err(crate::unwind::error_in(call, "subscript out of bounds")),
                None => selection.push(usize::MAX),
            }
        }
        selections.push(selection);
    }
    let total: usize = selections.iter().map(Vec::len).product();
    let mut flat = Vec::with_capacity(total);
    let mut counters = vec![0usize; dims.len()];
    for _ in 0..total {
        let mut offset = 0usize;
        let mut stride = 1usize;
        let mut na = false;
        for (dimension, &counter) in counters.iter().enumerate() {
            let position = selections[dimension][counter];
            if position == usize::MAX {
                na = true;
            }
            offset = offset.wrapping_add(position.wrapping_mul(stride));
            stride *= dims[dimension];
        }
        flat.push(if na { usize::MAX } else { offset });
        for (dimension, counter) in counters.iter_mut().enumerate() {
            *counter += 1;
            if *counter < selections[dimension].len() {
                break;
            }
            *counter = 0;
        }
    }
    let result = select_elements(object, &flat)?;
    let new_dims: Vec<usize> = selections.iter().map(Vec::len).collect();
    let new_dimnames: Vec<Sexp> = if dimnames.kind() == SexpType::List {
        selections
            .iter()
            .enumerate()
            .map(|(dimension, selection)| {
                let names = dimnames.element(dimension);
                if names == nil() {
                    Ok(nil())
                } else {
                    select_elements(names, selection)
                }
            })
            .collect::<R<_>>()?
    } else {
        vec![nil(); dims.len()]
    };
    let dimnames_names = values::names(dimnames);
    let kept: Vec<usize> = if drop {
        (0..new_dims.len())
            .filter(|&dimension| new_dims[dimension] != 1)
            .collect()
    } else {
        (0..new_dims.len()).collect()
    };
    if kept.len() <= 1 && drop {
        let names = dropped_names(&kept, &new_dimnames);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
        }
        return Ok(result);
    }
    let dim_values: Vec<i32> = kept
        .iter()
        .map(|&dimension| new_dims[dimension] as i32)
        .collect();
    let dim = values::integer_vector(&dim_values)?;
    values::set_attr(result, symbols().dim, dim)?;
    if kept
        .iter()
        .any(|&dimension| new_dimnames[dimension] != nil())
        || dimnames_names != nil()
    {
        let list: Vec<Sexp> = kept
            .iter()
            .map(|&dimension| new_dimnames[dimension])
            .collect();
        let list = values::list_vector(&list)?;
        if dimnames_names != nil() {
            let kept_names = select_elements(dimnames_names, &kept)?;
            values::set_attr(list, symbols().names, kept_names)?;
        }
        values::set_attr(result, symbols().dimnames, list)?;
    }
    Ok(result)
}

// `[[`.

unsafe fn do_subset2(call: &Call<'_>) -> R<Sexp> {
    let (object, indices, _, exact) = subset_args(call);
    let exact = match exact {
        Some(exact) => match values::as_logical_scalar(exact) {
            NA_LOGICAL => None,
            value => Some(value != 0),
        },
        None => Some(true),
    };
    subset2(object, &indices, exact, call.call)
}

/// GNU R's `R_getS4DataSlot`: the data part of an S4 object whose class extends a basic type,
/// held in `.Data` or, for an environment such as a reference class object, in `.xData`.
/// `NULL` when the class extends none.
unsafe fn s4_data_part(object: Sexp) -> Sexp {
    let data = get_attr(object, symbols().dot_data);
    if data != nil() {
        return data;
    }
    get_attr(object, symbols().dot_xdata)
}

/// The position `name` matches among `names`: exactly, or with `exact == Some(false)` (or
/// `None`, which also warns) by a unique partial match.
unsafe fn match_name(names: Sexp, name: &[u8], exact: Option<bool>) -> R<Option<usize>> {
    if names == nil() {
        return Ok(None);
    }
    let count = length(names);
    for index in 0..count {
        if names.element(index).bytes() == name && names.element(index) != na_string() {
            return Ok(Some(index));
        }
    }
    if exact == Some(true) {
        return Ok(None);
    }
    let mut found = None;
    for index in 0..count {
        if names.element(index).bytes().starts_with(name) && names.element(index) != na_string() {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(index);
        }
    }
    if found.is_some() && exact.is_none() {
        conditions::warning(format!(
            "partial match of '{}' to '{}'",
            String::from_utf8_lossy(name),
            char_str(names.element(found.unwrap_or(0)))
        ))?;
    }
    Ok(found)
}

pub unsafe fn subset2(object: Sexp, indices: &[Sexp], exact: Option<bool>, call: Sexp) -> R<Sexp> {
    if object.kind() == SexpType::Record {
        return crate::records::subset2(object, indices, call);
    }
    let object = values::materialize(object)?;
    if object.kind() == SexpType::Object && object.has_gp(gp::S4_OBJECT) {
        let data = s4_data_part(object);
        if data == nil() {
            return Err(crate::unwind::error_in(
                call,
                "this S4 class is not subsettable",
            ));
        }
        return subset2(data, indices, exact, call);
    }
    let subscript_count_fits =
        indices.len() <= 1 || length(values::get_attr(object, symbols().dim)) == indices.len();
    if object.kind() != SexpType::Environment
        && subscript_count_fits
        && (indices.is_empty() || indices.contains(&missing_arg()))
    {
        // GNU R attaches the object only when it is NULL.
        let object_field = [("object", nil())];
        let fields: &[(&str, Sexp)] = if object == nil() { &object_field } else { &[] };
        return Err(missing_subscript(call, fields));
    }
    if object == nil() {
        return Ok(nil());
    }
    if object.kind() == SexpType::Environment {
        let index = indices.first().copied().unwrap_or(nil());
        if index.kind() != SexpType::String || length(index) != 1 {
            return Err(crate::unwind::error_in(
                call,
                "wrong arguments for subsetting an environment",
            ));
        }
        let symbol = values::install_bytes(index.element(0).bytes())?;
        let value = eval::find_var_in_frame(symbol, object)?;
        if value == unbound() {
            return Ok(nil());
        }
        return value_of(value);
    }
    if matches!(
        object.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        let list = values::pairlist_to_list(object)?;
        return subset2(list, indices, exact, call);
    }
    if !object.kind().is_vector() {
        return Err(crate::unwind::error_in(
            call,
            format!(
                "object of type '{}' is not subsettable",
                object.kind().name()
            ),
        ));
    }
    if indices.len() >= 2 {
        let Some(dims) = values::dims(object) else {
            return Err(crate::unwind::error_in(
                call,
                "incorrect number of subscripts",
            ));
        };
        if dims.len() != indices.len() {
            return Err(crate::unwind::error_in(
                call,
                "incorrect number of subscripts",
            ));
        }
        let dimnames = get_attr(object, symbols().dimnames);
        let mut offset = 0usize;
        let mut stride = 1usize;
        for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
            let names = if dimnames.kind() == SexpType::List {
                dimnames.element(dimension)
            } else {
                nil()
            };
            let position = single_position(index, extent, names, exact, call)?;
            offset += position * stride;
            stride *= extent;
        }
        return element_or_value(object, offset);
    }
    let index = values::materialize(indices[0])?;
    let count = length(index);
    if count > 1 && matches!(object.kind(), SexpType::List | SexpType::Expression) {
        // Recursive indexing: each subscript goes one level deeper.
        let mut current = object;
        for position in 0..count {
            let single = vector_element(index, position)?;
            if !matches!(current.kind(), SexpType::List | SexpType::Expression)
                && position + 1 < count
            {
                return Err(crate::unwind::error_in(call, "subscript out of bounds"));
            }
            current = subset2(current, &[single], exact, call)?;
        }
        return Ok(current);
    }
    if count > 1 {
        return Err(crate::unwind::error_in(
            call,
            "attempt to select more than one element in vectorIndex",
        ));
    }
    if count == 0 {
        return Err(crate::unwind::error_in(
            call,
            "attempt to select less than one element in get1index",
        ));
    }
    let names = values::names(object);
    if index.kind() == SexpType::String
        && matches!(object.kind(), SexpType::List | SexpType::Expression)
        && (index.element(0).bytes().is_empty()
            || match_name(names, index.element(0).bytes(), exact)?.is_none())
    {
        // A list has no element of that name: `[[` gives `NULL`, unlike for atomic vectors.
        return Ok(nil());
    }
    if matches!(object.kind(), SexpType::List | SexpType::Expression) && length(index) == 1 {
        // A list gives `NULL` for a missing or infinite subscript, where an atomic vector
        // reports it out of bounds; a large finite subscript is out of bounds for both.
        let absent = match index.kind() {
            SexpType::Integer | SexpType::Logical => values::ints(index)[0] == NA_INTEGER,
            SexpType::Real => {
                let value = values::reals(index)[0];
                value.is_nan() || value == f64::INFINITY
            }
            _ => false,
        };
        if absent {
            return Ok(nil());
        }
    }
    let position = single_position(index, length(object), names, exact, call)?;
    element_or_value(object, position)
}

unsafe fn element_or_value(object: Sexp, position: usize) -> R<Sexp> {
    match object.kind() {
        SexpType::List | SexpType::Expression => Ok(object.element(position)),
        _ => vector_element(object, position),
    }
}

/// The position a `[[` subscript selects, with GNU R's errors.
unsafe fn single_position(
    index: Sexp,
    extent: usize,
    names: Sexp,
    exact: Option<bool>,
    call: Sexp,
) -> R<usize> {
    if length(index) != 1 {
        return Err(crate::unwind::error_in(
            call,
            if length(index) == 0 {
                "attempt to select less than one element in get1index"
            } else {
                "attempt to select more than one element in get1index"
            },
        ));
    }
    match index.kind() {
        SexpType::String => {
            let wanted = index.element(0);
            // An empty name means "no name", so it never selects an element.
            let found = if wanted.bytes().is_empty() {
                None
            } else {
                match_name(names, wanted.bytes(), exact)?
            };
            match found {
                Some(position) => Ok(position),
                None => Err(crate::unwind::error_in(call, "subscript out of bounds")),
            }
        }
        SexpType::Integer | SexpType::Logical => {
            let value = values::ints(index)[0];
            if value == NA_INTEGER {
                return Err(crate::unwind::error_in(call, "subscript out of bounds"));
            }
            if value < 0 && extent == 2 && value >= -2 {
                return Ok(if value == -1 { 1 } else { 0 });
            }
            if value <= 0 {
                let amount = if value < 0 && extent > 1 {
                    "more"
                } else {
                    "less"
                };
                return Err(crate::unwind::error_in(
                    call,
                    format!("attempt to select {amount} than one element in integerOneIndex"),
                ));
            }
            if value as usize > extent {
                return Err(crate::unwind::error_in(call, "subscript out of bounds"));
            }
            Ok(value as usize - 1)
        }
        SexpType::Real => {
            let value = values::reals(index)[0];
            if value.is_nan() {
                return Err(crate::unwind::error_in(call, "subscript out of bounds"));
            }
            let value = value.trunc();
            if value < 0.0 {
                if extent == 2 && value >= -2.0 {
                    return Ok(if value == -1.0 { 1 } else { 0 });
                }
                return Err(crate::unwind::error_in(
                    call,
                    "invalid negative subscript in get1index <real>",
                ));
            }
            if value == 0.0 {
                return Err(crate::unwind::error_in(
                    call,
                    "attempt to select less than one element in get1index <real>",
                ));
            }
            if value > extent as f64 {
                return Err(crate::unwind::error_in(call, "subscript out of bounds"));
            }
            Ok(value as usize - 1)
        }
        SexpType::Symbol => {
            let name = values::scalar_string_charsxp(index.car())?;
            single_position(name, extent, names, exact, call)
        }
        kind => Err(crate::unwind::error_in(
            call,
            format!("invalid subscript type '{}'", kind.name()),
        )),
    }
}

// `$` and `@`.

/// The name argument of `$` and `@`: a symbol or string, never evaluated.
unsafe fn name_argument(value: Sexp) -> R<Sexp> {
    // A promise comes from `NextMethod` and stands for the name the method received.
    let value = eval::value_of(value)?;
    match value.kind() {
        SexpType::Symbol => Ok(value.car()),
        SexpType::String if length(value) >= 1 => Ok(value.element(0)),
        SexpType::Language if value.car() == symbols().quote => name_argument(value.cdr().car()),
        _ => Err(error(
            "invalid subscript type '".to_string() + value.kind().name() + "'",
        )),
    }
}

/// `x$name`: `$` is special, so the name is never evaluated; an object dispatches with the
/// name as a string.
unsafe fn do_dollar(call: &Call<'_>) -> R<Sexp> {
    if call.len() != 2 {
        return Err(call.error(format!(
            "{} arguments passed to '$' which requires 2",
            call.len()
        )));
    }
    let object = eval::eval(call.arg(0), call.env)?;
    let name = name_argument(call.arg(1))?;
    if object.is_object() {
        let name_string = values::scalar_string_charsxp(name)?;
        let args = [
            Arg {
                tag: nil(),
                value: object,
            },
            Arg {
                tag: nil(),
                value: name_string,
            },
        ];
        if let Some(value) = crate::dispatch::dispatch_builtin("$", call.call, &args, call.env)? {
            return Ok(value);
        }
    }
    dollar(object, name, call.call)
}

pub unsafe fn dollar(object: Sexp, name: Sexp, call: Sexp) -> R<Sexp> {
    if object.kind() == SexpType::Record {
        return crate::records::dollar(object, name.bytes(), call);
    }
    let object = values::materialize(object)?;
    match object.kind() {
        SexpType::List | SexpType::Expression => {
            let names = values::names(object);
            match match_name(names, name.bytes(), Some(false))? {
                Some(position) => {
                    let value = object.element(position);
                    // A complex assignment through a partial name, `v$mi[[1]] <- 2`, assigns
                    // a new `mi` element, so the `misc` element it read must not be modified
                    // in place.
                    if names.element(position).bytes() != name.bytes() {
                        value.mark_not_mutable();
                    }
                    Ok(value)
                }
                None => Ok(nil()),
            }
        }
        SexpType::Pairlist | SexpType::Language => {
            let list = values::pairlist_to_list(object)?;
            dollar(list, name, call)
        }
        SexpType::Environment => {
            let symbol = crate::heap().install_char(name)?;
            let value = eval::find_var_in_frame(symbol, object)?;
            if value == unbound() {
                return Ok(nil());
            }
            value_of(value)
        }
        SexpType::Nil => Ok(nil()),
        SexpType::Object if object.has_gp(gp::S4_OBJECT) => {
            let data = get_attr(object, symbols().dot_xdata);
            if data.kind() == SexpType::Environment {
                return dollar(data, name, call);
            }
            Err(crate::unwind::error_in(
                call,
                "$ operator not defined for this S4 class",
            ))
        }
        _ => Err(crate::unwind::error_in(
            call,
            "$ operator is invalid for atomic vectors",
        )),
    }
}

unsafe fn slot_symbol(value: Sexp) -> R<Sexp> {
    let value = if value.kind() == SexpType::Promise {
        value.cdr()
    } else {
        value
    };
    match value.kind() {
        SexpType::Symbol => Ok(value),
        SexpType::String if length(value) == 1 => values::install_bytes(value.element(0).bytes()),
        _ => Err(error("invalid type or length for slot name")),
    }
}

unsafe fn do_at(call: &Call<'_>) -> R<Sexp> {
    let object = eval::eval(call.arg(0), call.env)?;
    let symbol = slot_symbol(call.arg(1))?;
    get_slot(object, symbol, call.call)
}

/// `R_do_slot`: an S4 slot, which is an attribute, with `.Data` and `.S3Class` special.
/// The elements a matrix subscript selects from an array with `dims`: one row per element and one
/// column per dimension, holding indices or (for a character matrix) dimnames. `None` when
/// `index` is not such a matrix; a row with an `NA` selects `NA`.
unsafe fn matrix_index_positions(
    object: Sexp,
    dims: &[usize],
    index: Sexp,
    call: Sexp,
) -> R<Option<Vec<Option<usize>>>> {
    let Some(index_dims) = values::dims(index) else {
        return Ok(None);
    };
    let numeric = matches!(index.kind(), SexpType::Integer | SexpType::Real);
    if dims.len() < 2
        || index_dims.len() != 2
        || index_dims[1] != dims.len()
        || !(numeric || index.kind() == SexpType::String)
    {
        return Ok(None);
    }
    let dimnames = get_attr(object, symbols().dimnames);
    let rows = index_dims[0];
    let mut positions = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut offset = 0usize;
        let mut stride = 1usize;
        let mut na = false;
        for (dimension, &extent) in dims.iter().enumerate() {
            let element = row + dimension * rows;
            let position = if numeric {
                let value = numeric_element(index, element);
                if value.is_nan() {
                    na = true;
                    break;
                }
                if value < 1.0 || value as usize > extent {
                    return Err(crate::unwind::error_in(call, "subscript out of bounds"));
                }
                value as usize - 1
            } else {
                let Some(name) = values::string_at(index, element) else {
                    na = true;
                    break;
                };
                let names = if dimnames == nil() {
                    nil()
                } else {
                    dimnames.element(dimension)
                };
                let found = (0..length(names)).find(|&candidate| {
                    names.element(candidate) != na_string()
                        && values::string_at(names, candidate).as_deref() == Some(&*name)
                });
                found.ok_or_else(|| crate::unwind::error_in(call, "subscript out of bounds"))?
            };
            offset += position * stride;
            stride *= extent;
        }
        positions.push((!na).then_some(offset));
    }
    Ok(Some(positions))
}

pub unsafe fn get_slot(object: Sexp, symbol: Sexp, call: Sexp) -> R<Sexp> {
    if symbol == symbols().dot_data {
        let methods = interp_methods_function("getDataPart")?;
        if let Some(function) = methods {
            let call = values::call(function, &[object])?;
            return eval::eval(call, values::base_env());
        }
    }
    let value = get_attr(object, symbol);
    if value == nil() {
        if symbol == symbols().dot_s3_class {
            return crate::dispatch::class_of(object);
        }
        if symbol_name(symbol) == "names" && object.kind() != SexpType::Object {
            return Ok(values::names(object));
        }
        if values::has_attributes(object) && heap_has_attr(object, symbol) {
            return Ok(nil());
        }
        let class = values::class_attr(object);
        let Some(class_name) = values::first_string(class) else {
            return Err(crate::unwind::error_in(
                call,
                format!(
                    "cannot get a slot (\"{}\") from an object of type \"{}\"",
                    symbol_name(symbol),
                    object.kind().name()
                ),
            ));
        };
        return Err(crate::unwind::error_in(
            call,
            format!(
                "no slot of name \"{}\" for this object of class \"{class_name}\"",
                symbol_name(symbol)
            ),
        ));
    }
    // A slot holding `NULL` is stored as a special symbol.
    if value.kind() == SexpType::Symbol && value.car().bytes() == b"\x01NULL\x01" {
        return Ok(nil());
    }
    Ok(value)
}

unsafe fn heap_has_attr(object: Sexp, symbol: Sexp) -> bool {
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == symbol {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

unsafe fn interp_methods_function(name: &str) -> R<Option<Sexp>> {
    let methods = crate::interp().globals.methods_namespace;
    if methods == nil() {
        return Ok(None);
    }
    let value = eval::find_var(values::install(name)?, methods)?;
    if value == unbound() {
        return Ok(None);
    }
    Ok(Some(value_of(value)?))
}

unsafe fn do_at_assign(call: &Call<'_>) -> R<Sexp> {
    let object = eval::eval(call.arg(0), call.env)?;
    let symbol = slot_symbol(call.arg(1))?;
    let value = eval::eval(call.arg(2), call.env)?;

    // The methods package checks the assignment against the class definition.
    if let Some(check) = interp_methods_function("checkAtAssignment")? {
        let class = values::class_attr(object);
        if class != nil() {
            let name = values::scalar_string_charsxp(symbol.car())?;
            let value_class = crate::dispatch::class_of(value)?;
            let check_call = values::call(check, &[class, name, value_class])?;
            eval::eval(check_call, values::base_env())?;
        }
    }
    set_slot(object, symbol, value)
}

/// `R_do_slot_assign`.
pub unsafe fn set_slot(object: Sexp, symbol: Sexp, value: Sexp) -> R<Sexp> {
    assign_slot(modifiable(object)?, symbol, value)
}

/// Sets a slot of `object` itself, even when it is shared. The methods package's `slot<-`
/// relies on that: its default `initialize` method calls it as a plain function on an object
/// it has just copied, and discards the result.
pub unsafe fn assign_slot(object: Sexp, symbol: Sexp, value: Sexp) -> R<Sexp> {
    if symbol == symbols().dot_data {
        if let Some(function) = interp_methods_function("setDataPart")? {
            let call = values::call(function, &[object, value])?;
            return eval::eval(call, values::base_env());
        }
    }
    let stored = if value == nil() {
        values::install_bytes(b"\x01NULL\x01")?
    } else {
        value
    };
    values::set_attr(object, symbol, stored)?;
    Ok(object)
}

// Subassignment.

unsafe fn do_subassign(call: &Call<'_>) -> R<Sexp> {
    let Some((value_arg, rest)) = call.args.split_last() else {
        return Err(call.error("SubAssignArgs: invalid number of arguments"));
    };
    let mut object = nil();
    let mut indices = Vec::new();
    for (position, arg) in rest.iter().enumerate() {
        if position == 0 {
            object = arg.value;
        } else if arg.tag != symbols().exact && arg.tag != symbols().drop {
            indices.push(arg.value);
        }
    }
    subassign(object, &indices, value_arg.value, call.call)
}

/// `x[i] <- value` and `x[i, j] <- value`.
pub unsafe fn subassign(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    let object = values::materialize(object)?;
    let value = values::materialize(value)?;
    if object.kind() == SexpType::Pairlist || object.kind() == SexpType::Language {
        let list = values::pairlist_to_list(object)?;
        let result = subassign(list, indices, value, call)?;
        // A call stays a call, but a pairlist becomes a list, as in GNU R.
        return if object.kind() == SexpType::Language {
            values::list_to_pairlist(result, SexpType::Language)
        } else {
            Ok(result)
        };
    }
    if object == nil() && (value == nil() || (value.kind().is_vector() && length(value) == 0)) {
        return Ok(nil());
    }
    let object = if object == nil() {
        alloc(
            if value.kind().is_vector() {
                value.kind()
            } else {
                SexpType::List
            },
            0,
        )?
    } else {
        object
    };
    if !object.kind().is_vector() {
        return Err(crate::unwind::error_in(
            call,
            format!(
                "object of type '{}' is not subsettable",
                object.kind().name()
            ),
        ));
    }
    // Deleting list elements.
    if value == nil()
        && matches!(object.kind(), SexpType::List | SexpType::Expression)
        && indices.len() <= 1
    {
        let count = length(object);
        let names = values::names(object);
        let index = indices.first().copied().unwrap_or(missing_arg());
        let (positions, _) = resolve(index, count, names, true, call)?;
        let remove: std::collections::HashSet<usize> = positions.into_iter().flatten().collect();
        let kept: Vec<usize> = (0..count)
            .filter(|position| !remove.contains(position))
            .collect();
        let result = select_elements(object, &kept)?;
        values::copy_most_attributes(object, result, false)?;
        if names != nil() {
            let new_names = select_elements(names, &kept)?;
            values::set_attr(result, symbols().names, new_names)?;
        }
        return Ok(result);
    }
    let value_kind = if value == nil() {
        SexpType::Logical
    } else {
        value.kind()
    };
    // An empty vector takes an empty replacement of its own type, or an empty
    // list, unchanged whatever the index selects: an empty list must not turn
    // an atomic vector into a list.
    if length(object) == 0
        && value != nil()
        && length(value) == 0
        && (value_kind == object.kind()
            || matches!(value_kind, SexpType::List | SexpType::Expression))
    {
        return Ok(object);
    }
    let target_kind = if !value_kind.is_vector() {
        SexpType::List
    } else {
        coerce::higher_type(object.kind(), value_kind)
    };
    let mut result = if target_kind != object.kind() {
        let converted = coerce_vector(object, target_kind)?;
        if converted == object {
            values::shallow_duplicate(object)?
        } else {
            converted
        }
    } else {
        modifiable(object)?
    };
    let value =
        if value.kind() != target_kind && value.kind().is_vector() && target_kind != SexpType::List
        {
            coerce_vector(value, target_kind)?
        } else {
            value
        };
    let value_count = if value == nil() {
        0
    } else if value.kind().is_vector() {
        length(value)
    } else {
        1
    };
    if indices.len() >= 2 {
        let Some(dims) = values::dims(result) else {
            return Err(crate::unwind::error_in(
                call,
                "incorrect number of subscripts on matrix",
            ));
        };
        if dims.len() != indices.len() {
            return Err(crate::unwind::error_in(
                call,
                "incorrect number of subscripts",
            ));
        }
        let dimnames = get_attr(result, symbols().dimnames);
        let mut selections = Vec::new();
        for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
            let names = if dimnames.kind() == SexpType::List {
                dimnames.element(dimension)
            } else {
                nil()
            };
            let (positions, _) = resolve(index, extent, names, false, call)?;
            let mut selection = Vec::new();
            for position in positions {
                match position {
                    Some(position) if position < extent => selection.push(position),
                    _ => return Err(crate::unwind::error_in(call, "subscript out of bounds")),
                }
            }
            selections.push(selection);
        }
        let total: usize = selections.iter().map(Vec::len).product();
        if total == 0 {
            return Ok(result);
        }
        if value_count == 0 {
            return Err(crate::unwind::error_in(call, "replacement has length zero"));
        }
        if total % value_count != 0 {
            return Err(crate::unwind::error_in(
                call,
                "number of items to replace is not a multiple of replacement length",
            ));
        }
        let mut counters = vec![0usize; dims.len()];
        for position in 0..total {
            let mut offset = 0usize;
            let mut stride = 1usize;
            for (dimension, &counter) in counters.iter().enumerate() {
                offset += selections[dimension][counter] * stride;
                stride *= dims[dimension];
            }
            assign_element(result, offset, value, position % value_count)?;
            for (dimension, counter) in counters.iter_mut().enumerate() {
                *counter += 1;
                if *counter < selections[dimension].len() {
                    break;
                }
                *counter = 0;
            }
        }
        return Ok(result);
    }
    let index = indices.first().copied().unwrap_or(missing_arg());
    let count = length(result);
    let names = values::names(result);
    // A matrix with one column per dimension assigns single elements.
    if let Some(dims) = values::dims(result)
        && let Some(positions) = matrix_index_positions(result, &dims, index, call)?
    {
        for (row, position) in positions.into_iter().enumerate() {
            let Some(offset) = position else {
                return Err(crate::unwind::error_in(
                    call,
                    "NAs are not allowed in subscripted assignments",
                ));
            };
            assign_element(result, offset, value, row % value_count.max(1))?;
        }
        return Ok(result);
    }
    let (positions, new_names) = resolve(index, count, names, true, call)?;
    let positions: Vec<usize> = positions.into_iter().flatten().collect();
    if positions.is_empty() {
        return Ok(result);
    }
    if value_count == 0 {
        return Err(crate::unwind::error_in(call, "replacement has length zero"));
    }
    if positions.len() % value_count != 0 {
        conditions::warning_call(
            call,
            "number of items to replace is not a multiple of replacement length",
        )?;
    }
    let needed = positions.iter().copied().max().map_or(0, |max| max + 1);
    if needed > count {
        result = grow(result, needed)?;
        if names != nil() || !new_names.is_empty() {
            let grown_names = alloc(SexpType::String, needed)?;
            for position in 0..needed {
                let name = if names != nil() && position < count {
                    names.element(position)
                } else if position >= count && position - count < new_names.len() {
                    new_names[position - count].unwrap_or(values::blank_string())
                } else {
                    values::blank_string()
                };
                values::set_elt(grown_names, position, name);
            }
            values::set_attr(result, symbols().names, grown_names)?;
        }
    }
    for (position, &target) in positions.iter().enumerate() {
        assign_element(result, target, value, position % value_count)?;
    }
    Ok(result)
}

/// `vector` extended to `length`, padded with NA, keeping its attributes but not its dims.
unsafe fn grow(vector: Sexp, new_length: usize) -> R<Sexp> {
    let count = length(vector);
    let result = alloc(vector.kind(), new_length)?;
    for position in 0..new_length {
        if position < count {
            copy_element(vector, position, result, position)?;
        } else {
            set_na(result, position);
        }
    }
    let mut cell = vector.attrib();
    while cell.kind() == SexpType::Pairlist {
        let tag = cell.tag();
        if tag != symbols().dim && tag != symbols().dimnames && tag != symbols().names {
            values::set_attr(result, tag, cell.car())?;
        }
        cell = cell.cdr();
    }
    let names = values::names(vector);
    if names != nil() {
        let grown = alloc(SexpType::String, new_length)?;
        for position in 0..count {
            values::set_elt(grown, position, names.element(position));
        }
        values::set_attr(result, symbols().names, grown)?;
    }
    Ok(result)
}

unsafe fn assign_element(target: Sexp, position: usize, value: Sexp, value_index: usize) -> R<()> {
    if matches!(target.kind(), SexpType::List | SexpType::Expression) && !value.kind().is_vector() {
        values::set_elt(target, position, value);
        return Ok(());
    }
    copy_element(value, value_index, target, position)
}

unsafe fn do_subassign2(call: &Call<'_>) -> R<Sexp> {
    let Some((value_arg, rest)) = call.args.split_last() else {
        return Err(call.error("SubAssignArgs: invalid number of arguments"));
    };
    let mut object = nil();
    let mut indices = Vec::new();
    for (position, arg) in rest.iter().enumerate() {
        if position == 0 {
            object = arg.value;
        } else if arg.tag != symbols().exact {
            indices.push(arg.value);
        }
    }
    subassign2(object, &indices, value_arg.value, call.call)
}

/// `x[[i]] <- value`.
pub unsafe fn subassign2(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    if object.kind() == SexpType::Record {
        return crate::records::subassign2(object, indices, value, call);
    }
    let object = values::materialize(object)?;
    // A reference class object assigns into its environment and stays itself.
    if object.kind() == SexpType::Object && object.has_gp(gp::S4_OBJECT) {
        let data = s4_data_part(object);
        if data.kind() != SexpType::Environment {
            return Err(crate::unwind::error_in(
                call,
                "[[<- defined for objects of type \"S4\" only for subclasses of environment",
            ));
        }
        subassign2(data, indices, value, call)?;
        return Ok(object);
    }
    if object.kind() == SexpType::Environment {
        let index = indices.first().copied().unwrap_or(nil());
        if index.kind() != SexpType::String || length(index) != 1 {
            return Err(crate::unwind::error_in(
                call,
                "wrong args for environment subassignment",
            ));
        }
        let symbol = values::install_bytes(index.element(0).bytes())?;
        eval::define(symbol, value, object)?;
        return Ok(object);
    }
    if indices.is_empty() || indices.contains(&missing_arg()) {
        let object_field = [("object", object)];
        let fields: &[(&str, Sexp)] = if object == nil() { &[] } else { &object_field };
        return Err(missing_subscript(call, fields));
    }
    if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
        let list = values::pairlist_to_list(object)?;
        let result = subassign2(list, indices, value, call)?;
        let result = values::list_to_pairlist(result, object.kind())?;
        // The call keeps its attributes, such as a formula's class and environment, and a
        // pairlist keeps its dimensions.
        values::copy_most_attributes(object, result, true)?;
        return Ok(result);
    }
    if indices.len() == 1
        && length(indices[0]) > 1
        && matches!(object.kind(), SexpType::List | SexpType::Expression)
    {
        // Recursive assignment.
        let index = indices[0];
        let first = vector_element(index, 0)?;
        let rest_indices: Vec<usize> = (1..length(index)).collect();
        let rest = select_elements(index, &rest_indices)?;
        let inner = subset2(object, &[first], Some(true), call)?;
        let updated = subassign2(inner, &[rest], value, call)?;
        return subassign2(object, &[first], updated, call);
    }
    let is_list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    if object == nil() && value == nil() {
        return Ok(nil());
    }
    if is_list || object == nil() || !value.kind().is_atomic() || length(value) != 1 {
        // Whole-element assignment into a list.
        // GNU R grows `NULL` into a list for `[[<-` and `$<-` whatever the value is.
        let list = if object == nil() {
            alloc(SexpType::List, 0)?
        } else if is_list {
            modifiable(object)?
        } else {
            if value.kind().is_atomic() && length(value) > 1 {
                return Err(crate::unwind::error_in(
                    call,
                    "more elements supplied than there are to replace",
                ));
            }
            if value.kind().is_atomic() && length(value) == 0 {
                return Err(crate::unwind::error_in(call, "replacement has length zero"));
            }
            coerce_vector(object, SexpType::List)?
        };
        if indices.len() >= 2 {
            let wrapped = values::list_vector(&[value])?;
            return subassign(list, indices, wrapped, call);
        }
        let index = indices.first().copied().unwrap_or(missing_arg());
        if value == nil() {
            return subassign(list, &[index], nil(), call);
        }
        let count = length(list);
        let names = values::names(list);
        let position = if index.kind() == SexpType::String {
            match match_name(names, index.element(0).bytes(), Some(true))? {
                Some(position) => position,
                None => count,
            }
        } else {
            let numeric = numeric_element(index, 0);
            if numeric.is_nan() || numeric < 1.0 {
                return Err(crate::unwind::error_in(
                    call,
                    "[[ ]] subscript out of bounds",
                ));
            }
            numeric as usize - 1
        };
        let mut result = list;
        if position >= count {
            result = grow(list, position + 1)?;
            if index.kind() == SexpType::String {
                let names_now = values::names(result);
                let new_names = if names_now == nil() {
                    alloc(SexpType::String, position + 1)?
                } else {
                    names_now
                };
                values::set_elt(new_names, position, index.element(0));
                values::set_attr(result, symbols().names, new_names)?;
            }
        }
        values::set_elt(result, position, value);
        return Ok(result);
    }
    subassign(object, indices, value, call)
}

unsafe fn missing_subscript(call: Sexp, fields: &[(&str, Sexp)]) -> Unwind {
    crate::conditions::classed_error(
        call,
        "missing subscript",
        &["MissingSubscriptError", "error", "condition"],
        fields,
    )
}

unsafe fn do_dollar_assign(call: &Call<'_>) -> R<Sexp> {
    let object = eval::eval(call.arg(0), call.env)?;
    let name = name_argument(call.arg(1))?;
    let value = eval::eval(call.arg(2), call.env)?;
    if object.is_object() {
        let name_string = values::scalar_string_charsxp(name)?;
        let args = [
            Arg {
                tag: nil(),
                value: object,
            },
            Arg {
                tag: nil(),
                value: name_string,
            },
            Arg {
                tag: symbols().value,
                value,
            },
        ];
        if let Some(result) = crate::dispatch::dispatch_builtin("$<-", call.call, &args, call.env)?
        {
            return Ok(result);
        }
    }
    dollar_assign(object, name, value, call.call)
}

pub unsafe fn dollar_assign(object: Sexp, name: Sexp, value: Sexp, call: Sexp) -> R<Sexp> {
    if object.kind() == SexpType::Record {
        return crate::records::dollar_assign(object, name.bytes(), value, call);
    }
    let object = values::materialize(object)?;
    match object.kind() {
        SexpType::Environment => {
            let symbol = crate::heap().install_char(name)?;
            eval::define(symbol, value, object)?;
            Ok(object)
        }
        SexpType::Object if object.has_gp(gp::S4_OBJECT) => {
            let data = get_attr(object, symbols().dot_xdata);
            if data.kind() == SexpType::Environment {
                let symbol = crate::heap().install_char(name)?;
                eval::define(symbol, value, data)?;
                return Ok(object);
            }
            Err(crate::unwind::error_in(
                call,
                "no '$<-' method for this S4 class",
            ))
        }
        SexpType::List
        | SexpType::Expression
        | SexpType::Nil
        | SexpType::Pairlist
        | SexpType::Language => {
            let index = values::scalar_string_charsxp(name)?;
            subassign2(object, &[index], value, call)
        }
        _ if object.kind().is_atomic() => {
            conditions::warning_call(call, "Coercing LHS to a list")?;
            let list = coerce_vector(object, SexpType::List)?;
            let index = values::scalar_string_charsxp(name)?;
            subassign2(list, &[index], value, call)
        }
        _ => Err(crate::unwind::error_in(
            call,
            "invalid type for $ assignment",
        )),
    }
}

// c() and unlist().

/// The pieces `c()` and `unlist()` flatten, with their names.
struct Flattener {
    kind: SexpType,
    values: Vec<(Sexp, usize)>,
    /// `None` is an `NA` name, which survives only when no outer tag is prefixed to it.
    names: Vec<Option<String>>,
    any_names: bool,
    recursive: bool,
    use_names: bool,
}

impl Flattener {
    unsafe fn add(&mut self, value: Sexp, tag: &str) -> R<()> {
        let value = values::materialize(value)?;
        // A pairlist combines like the list of its elements.
        let value = if value.kind() == SexpType::Pairlist {
            values::pairlist_to_list(value)?
        } else {
            value
        };
        let count = length(value);
        let element_names = values::names(value);
        if !tag.is_empty() || element_names != nil() {
            self.any_names = true;
        }
        let is_list = matches!(
            value.kind(),
            SexpType::List | SexpType::Expression | SexpType::Language
        );
        if is_list && self.recursive && value.kind() != SexpType::Language {
            for index in 0..count {
                let name = self.child_name(tag, element_names, index, count);
                self.add(value.element(index), name.as_deref().unwrap_or("NA"))?;
            }
            return Ok(());
        }
        if value == nil() {
            return Ok(());
        }
        if !value.kind().is_vector() {
            self.values.push((value, usize::MAX));
            self.names.push(Some(tag.to_owned()));
            return Ok(());
        }
        for index in 0..count {
            let name = self.child_name(tag, element_names, index, count);
            self.values.push((value, index));
            self.names.push(name);
        }
        Ok(())
    }

    unsafe fn child_name(
        &self,
        tag: &str,
        names: Sexp,
        index: usize,
        count: usize,
    ) -> Option<String> {
        if !self.use_names {
            return Some(String::new());
        }
        let inner = if names != nil() && index < length(names) {
            values::string_at(names, index).map(|text| text.into_owned())
        } else {
            Some(String::new())
        };
        if tag.is_empty() {
            return inner;
        }
        let inner = inner.unwrap_or_else(|| "NA".to_owned());
        Some(if !inner.is_empty() {
            format!("{tag}.{inner}")
        } else if count == 1 {
            tag.to_owned()
        } else {
            format!("{tag}{}", index + 1)
        })
    }

    unsafe fn kind_of(&mut self, value: Sexp) -> R<()> {
        let value = values::materialize(value)?;
        let kind = match value.kind() {
            SexpType::Nil => return Ok(()),
            SexpType::List | SexpType::Pairlist if self.recursive => {
                let list = if value.kind() == SexpType::Pairlist {
                    values::pairlist_to_list(value)?
                } else {
                    value
                };
                for index in 0..length(list) {
                    self.kind_of(list.element(index))?;
                }
                return Ok(());
            }
            SexpType::Expression => SexpType::Expression,
            kind if kind.is_atomic() => kind,
            _ => SexpType::List,
        };
        self.kind = coerce::higher_type(self.kind, kind);
        Ok(())
    }

    unsafe fn build(self) -> R<Sexp> {
        let result = alloc(self.kind, self.values.len())?;
        for (position, &(value, index)) in self.values.iter().enumerate() {
            if index == usize::MAX {
                values::set_elt(result, position, value);
            } else {
                copy_element(value, index, result, position)?;
            }
        }
        if self.any_names && self.use_names && !self.values.is_empty() {
            let names = values::string_vector_na(&self.names)?;
            values::set_attr(result, symbols().names, names)?;
        }
        Ok(result)
    }
}

/// `c()` of already evaluated values, without names.
pub unsafe fn combine(items: &[Sexp]) -> R<Sexp> {
    let args: Vec<Arg> = items
        .iter()
        .map(|&value| Arg { tag: nil(), value })
        .collect();
    combine_args(&args, false, true)
}

unsafe fn combine_args(args: &[Arg], recursive: bool, use_names: bool) -> R<Sexp> {
    let mut flattener = Flattener {
        kind: SexpType::Nil,
        values: Vec::new(),
        names: Vec::new(),
        any_names: false,
        recursive,
        use_names,
    };
    for arg in args {
        flattener.kind_of(arg.value)?;
    }
    if flattener.kind == SexpType::Nil {
        return Ok(nil());
    }
    if matches!(flattener.kind, SexpType::List | SexpType::Expression) {
        flattener.recursive = recursive;
    }
    for arg in args {
        let tag = if arg.tag == nil() {
            String::new()
        } else {
            symbol_name(arg.tag).into_owned()
        };
        if matches!(flattener.kind, SexpType::List | SexpType::Expression)
            && !recursive
            && !matches!(
                arg.value.kind(),
                SexpType::List | SexpType::Expression | SexpType::Pairlist
            )
            && arg.value.kind().is_atomic()
        {
            // Atomic vectors are split into elements of the list.
            flattener.add(arg.value, &tag)?;
            continue;
        }
        flattener.add(arg.value, &tag)?;
    }
    flattener.build()
}

unsafe fn do_c(call: &Call<'_>) -> R<Sexp> {
    let mut recursive = false;
    let mut use_names = true;
    let mut args = Vec::new();
    for arg in call.args {
        if arg.tag != nil() && symbol_name(arg.tag) == "recursive" {
            recursive = values::as_logical_scalar(arg.value) == 1;
        } else if arg.tag == symbols().use_names {
            use_names = values::as_logical_scalar(arg.value) != 0;
        } else {
            args.push(*arg);
        }
    }
    combine_args(&args, recursive, use_names)
}

unsafe fn do_unlist(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let recursive = values::as_logical_scalar(call.arg(1)) != 0;
    let use_names = values::as_logical_scalar(call.arg(2)) != 0;
    if !matches!(
        object.kind(),
        SexpType::List | SexpType::Pairlist | SexpType::Expression
    ) {
        return Ok(object);
    }
    let list = if object.kind() == SexpType::Pairlist {
        values::pairlist_to_list(object)?
    } else {
        object
    };
    let names = values::names(list);
    let args: Vec<Arg> = (0..length(list))
        .map(|index| {
            let tag = if names != nil() && !values::is_blank(names.element(index)) {
                crate::heap()
                    .install_char(names.element(index))
                    .unwrap_or(nil())
            } else {
                nil()
            };
            Arg {
                tag,
                value: list.element(index),
            }
        })
        .collect();
    if !recursive {
        return combine_args(&args, false, use_names);
    }
    combine_args(&args, true, use_names)
}

unsafe fn do_list(call: &Call<'_>) -> R<Sexp> {
    let result = alloc(SexpType::List, call.len())?;
    let mut any_names = false;
    for (index, arg) in call.args.iter().enumerate() {
        values::set_elt(result, index, arg.value);
        if arg.tag != nil() {
            any_names = true;
        }
    }
    if any_names {
        let names = alloc(SexpType::String, call.len())?;
        for (index, arg) in call.args.iter().enumerate() {
            if arg.tag.kind() == SexpType::Symbol {
                values::set_elt(names, index, arg.tag.car());
            }
        }
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

// Lengths and names.

unsafe fn do_length(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(1)?;
    let count = length(call.arg(0));
    if count > i32::MAX as usize {
        return values::scalar_real(count as f64);
    }
    scalar_integer(count as i32)
}

unsafe fn do_set_length(call: &Call<'_>) -> R<Sexp> {
    let object = values::materialize(call.arg(0))?;
    let new_length = length_argument(call.arg(1))?;
    if object == nil() && new_length == 0 {
        return Ok(nil());
    }
    if object.kind() == SexpType::Pairlist {
        // A pairlist keeps its leading cells with their tags and grows with untagged NULLs.
        let mut items = Vec::with_capacity(new_length);
        let mut cell = object;
        while items.len() < new_length {
            if cell.kind() == SexpType::Pairlist {
                items.push((cell.tag(), cell.car()));
                cell = cell.cdr();
            } else {
                items.push((nil(), nil()));
            }
        }
        return values::pairlist(&items, SexpType::Pairlist);
    }
    if !object.kind().is_vector() && object != nil() {
        return Err(call.error("invalid argument"));
    }
    let object = if object == nil() {
        alloc(SexpType::Logical, 0)?
    } else {
        object
    };
    let count = length(object);
    let result = alloc(object.kind(), new_length)?;
    for position in 0..new_length {
        if position < count {
            copy_element(object, position, result, position)?;
        } else {
            set_na(result, position);
        }
    }
    let names = values::names(object);
    if names != nil() {
        let new_names = alloc(SexpType::String, new_length)?;
        for position in 0..new_length.min(count) {
            values::set_elt(new_names, position, names.element(position));
        }
        values::set_attr(result, symbols().names, new_names)?;
    }
    Ok(result)
}

unsafe fn do_lengths(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let use_names = values::as_logical_scalar(call.arg(1)) != 0;
    let count = length(object);
    let result = alloc(SexpType::Integer, count)?;
    for index in 0..count {
        let element = match object.kind() {
            SexpType::List | SexpType::Expression => length(object.element(index)),
            _ => 1,
        };
        values::ints_mut(result)[index] = element as i32;
    }
    if use_names {
        let names = values::names(object);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
        }
    }
    let dim = get_attr(object, symbols().dim);
    if dim != nil() {
        values::set_attr(result, symbols().dim, dim)?;
        let dimnames = get_attr(object, symbols().dimnames);
        if dimnames != nil() {
            values::set_attr(result, symbols().dimnames, dimnames)?;
        }
    }
    Ok(result)
}

unsafe fn do_names(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() == SexpType::Environment {
        // GNU R: `names(env)` is `ls(env, all.names = TRUE, sorted = FALSE)`.
        return crate::envir::env_names(object, true, false);
    }
    Ok(values::names(object))
}

unsafe fn do_set_names(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let names = call.arg(1);
    if object == nil() {
        if names == nil() {
            return Ok(nil());
        }
        return Err(call.error("attempt to set an attribute on NULL"));
    }
    let object = modifiable(object)?;
    set_names(object, names)?;
    Ok(object)
}

/// `names<-`: names are converted to character and padded with `NA` to the object's length.
pub unsafe fn set_names(object: Sexp, names: Sexp) -> R<()> {
    if names == nil() {
        if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
            let mut cell = object;
            while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
                crate::heap().set_tag(cell, nil());
                cell = cell.cdr();
            }
        }
        values::remove_attr(object, symbols().names);
        return Ok(());
    }
    let names = if values::inherits(names, "factor") {
        coerce::factor_as_character(names)?
    } else {
        coerce_vector(names, SexpType::String)?
    };
    let count = length(object);
    if length(names) > count {
        return Err(error(format!(
            "'names' attribute [{}] must be the same length as the vector [{count}]",
            length(names)
        )));
    }
    if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
        let mut cell = object;
        let mut index = 0;
        while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
            let tag = if index < length(names)
                && names.element(index) != na_string()
                && !names.element(index).bytes().is_empty()
            {
                crate::heap().install_char(names.element(index))?
            } else {
                nil()
            };
            crate::heap().set_tag(cell, tag);
            cell = cell.cdr();
            index += 1;
        }
        return Ok(());
    }
    let padded = if length(names) < count {
        let padded = alloc(SexpType::String, count)?;
        for index in 0..count {
            values::set_elt(
                padded,
                index,
                if index < length(names) {
                    names.element(index)
                } else {
                    na_string()
                },
            );
        }
        padded
    } else if names.attrib() != nil() {
        let copy = values::shallow_duplicate(names)?;
        values::clear_attributes(copy);
        copy
    } else {
        names
    };
    // A one-dimensional array keeps its names as dimnames.
    if let Some(dims) = values::dims(object)
        && dims.len() == 1
    {
        let dimnames = values::list_vector(&[padded])?;
        values::set_attr(object, symbols().dimnames, dimnames)?;
        return Ok(());
    }
    values::set_attr(object, symbols().names, padded)
}

unsafe fn do_dim(call: &Call<'_>) -> R<Sexp> {
    Ok(get_attr(call.arg(0), symbols().dim))
}

unsafe fn do_set_dim(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let dims = call.arg(1);
    let object = modifiable(object)?;
    if dims == nil() {
        values::remove_attr(object, symbols().dim);
        values::remove_attr(object, symbols().dimnames);
        return Ok(object);
    }
    if !object.kind().is_vector() && object.kind() != SexpType::Pairlist {
        return Err(call.error("invalid first argument, must be vector (list or atomic)"));
    }
    let dims = coerce_vector(dims, SexpType::Integer)?;
    let product: i64 = values::ints(dims)
        .iter()
        .map(|&value| value as i64)
        .product();
    if values::ints(dims)
        .iter()
        .any(|&value| value == NA_INTEGER || value < 0)
    {
        return Err(call.error("the dims contain missing or negative values"));
    }
    if product != length(object) as i64 {
        return Err(call.error(format!(
            "dims [product {product}] do not match the length of object [{}]",
            length(object)
        )));
    }
    values::remove_attr(object, symbols().names);
    values::remove_attr(object, symbols().dimnames);
    let dims = if dims.attrib() != nil() {
        let copy = values::duplicate(dims)?;
        values::clear_attributes(copy);
        copy
    } else {
        dims
    };
    values::set_attr(object, symbols().dim, dims)?;
    Ok(object)
}

unsafe fn do_dimnames(call: &Call<'_>) -> R<Sexp> {
    Ok(get_attr(call.arg(0), symbols().dimnames))
}

unsafe fn do_set_dimnames(call: &Call<'_>) -> R<Sexp> {
    let object = modifiable(call.arg(0))?;
    set_dimnames(object, call.arg(1), call.call)?;
    Ok(object)
}

/// `dimnames<-` with GNU R's checks: each component is `NULL` or as long as its extent. A
/// list shorter than the dimensions is padded with `NULL`, an empty list removes the
/// dimnames, and a list of only `NULL` components is kept as it is.
pub unsafe fn set_dimnames(object: Sexp, dimnames: Sexp, call: Sexp) -> R<()> {
    if dimnames == nil() {
        values::remove_attr(object, symbols().dimnames);
        return Ok(());
    }
    let Some(dims) = values::dims(object) else {
        return Err(crate::unwind::error_in(
            call,
            "'dimnames' applied to non-array",
        ));
    };
    if dimnames.kind() != SexpType::List {
        return Err(crate::unwind::error_in(call, "'dimnames' must be a list"));
    }
    if length(dimnames) == 0 {
        values::remove_attr(object, symbols().dimnames);
        return Ok(());
    }
    if length(dimnames) > dims.len() {
        return Err(crate::unwind::error_in(
            call,
            format!(
                "length of 'dimnames' [{}] must match that of 'dims' [{}]",
                length(dimnames),
                dims.len()
            ),
        ));
    }
    let result = alloc(SexpType::List, dims.len())?;
    for (dimension, &extent) in dims.iter().enumerate().take(length(dimnames)) {
        let component = dimnames.element(dimension);
        // A zero-length component means no names for that dimension, whatever its extent.
        if component == nil() || length(component) == 0 {
            continue;
        }
        let component = if values::inherits(component, "factor") {
            coerce::factor_as_character(component)?
        } else if component.kind() != SexpType::String {
            let converted = coerce_vector(component, SexpType::String)?;
            values::clear_attributes(converted);
            converted
        } else {
            component
        };
        if length(component) != extent {
            return Err(crate::unwind::error_in(
                call,
                format!(
                    "length of 'dimnames' [{}] not equal to array extent",
                    dimension + 1
                ),
            ));
        }
        values::set_elt(result, dimension, component);
    }
    let names = values::names(dimnames);
    if names != nil() {
        let padded = alloc(SexpType::String, dims.len())?;
        for index in 0..dims.len() {
            let name = if index < length(names) {
                names.element(index)
            } else {
                values::mkchar("")?
            };
            values::set_elt(padded, index, name);
        }
        values::set_attr(result, symbols().names, padded)?;
    }
    values::set_attr(object, symbols().dimnames, result)
}

unsafe fn do_set_levels(call: &Call<'_>) -> R<Sexp> {
    let object = modifiable(call.arg(0))?;
    values::set_attr(object, symbols().levels, call.arg(1))?;
    Ok(object)
}

// Attributes.

unsafe fn do_attr(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "which", "exact"])?;
    let object = present(matched[0]).unwrap_or(nil());
    let which = present(matched[1])
        .ok_or_else(|| call.error("exactly one attribute 'which' must be given"))?;
    let exact = present(matched[2]).is_some_and(|exact| values::as_logical_scalar(exact) == 1);
    if which.kind() != SexpType::String || length(which) != 1 {
        return Err(call.error("exactly one attribute 'which' must be given"));
    }
    let name = which.element(0);
    if name.bytes() == b"names" {
        return Ok(values::names(object));
    }
    let symbol = crate::heap().install_char(name)?;
    let value = get_attr(object, symbol);
    if value != nil() || exact {
        if symbol == symbols().row_names {
            return expand_row_names(value);
        }
        return Ok(value);
    }
    // Partial matching on attribute names.
    let mut found = nil();
    let mut cell = object.attrib();
    let mut matches = 0;
    while cell.kind() == SexpType::Pairlist {
        if cell.tag().car().bytes().starts_with(name.bytes()) {
            found = cell.car();
            matches += 1;
        }
        cell = cell.cdr();
    }
    Ok(if matches == 1 { found } else { nil() })
}

/// Compact row names (`c(NA, -n)`) read back as `1:n`.
unsafe fn expand_row_names(value: Sexp) -> R<Sexp> {
    if value.kind() == SexpType::Integer
        && length(value) == 2
        && values::ints(value)[0] == NA_INTEGER
    {
        let count = values::ints(value)[1].unsigned_abs() as usize;
        let result = alloc(SexpType::Integer, count)?;
        for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
            *slot = index as i32 + 1;
        }
        return Ok(result);
    }
    Ok(value)
}

unsafe fn do_set_attr(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "which", "value"])?;
    let object = present(matched[0]).unwrap_or(nil());
    let which = present(matched[1]).unwrap_or(nil());
    let value = present(matched[2]).unwrap_or(nil());
    if which.kind() != SexpType::String || length(which) != 1 {
        return Err(call.error("'name' must be non-null character string"));
    }
    if object == nil() {
        if value == nil() {
            return Ok(nil());
        }
        return Err(call.error("attempt to set an attribute on NULL"));
    }
    let object = modifiable(object)?;
    let symbol = crate::heap().install_char(which.element(0))?;
    set_attribute_checked(object, symbol, value, call.call)?;
    Ok(object)
}

/// Sets an attribute with the checks GNU R's `setAttrib` makes for names, dims, dimnames and
/// class.
pub unsafe fn set_attribute_checked(object: Sexp, symbol: Sexp, value: Sexp, call: Sexp) -> R<()> {
    let symbols = symbols();
    if symbol == symbols.names {
        return set_names(object, value);
    }
    if symbol == symbols.dim {
        let args = [
            Arg {
                tag: nil(),
                value: object,
            },
            Arg { tag: nil(), value },
        ];
        let function = crate::builtins::primitive("dim<-").ok_or_else(|| error("no dim<-"))?;
        let context = Call {
            call,
            op: function,
            args: &args,
            env: values::global_env(),
        };
        let result = do_set_dim(&context)?;
        if result != object {
            crate::heap().set_attrib(object, result.attrib());
        }
        return Ok(());
    }
    if symbol == symbols.dimnames {
        return set_dimnames(object, value, call);
    }
    if symbol == symbols.class {
        return crate::dispatch::set_class_attribute(object, value);
    }
    if symbol == symbols.row_names && value.kind() == SexpType::Integer {
        // Row names 1:n are stored compactly, as GNU R does.
        let data = values::ints(value);
        let compact = !data.is_empty()
            && data
                .iter()
                .enumerate()
                .all(|(index, &row)| row == index as i32 + 1);
        if compact {
            let stored = values::integer_vector(&[NA_INTEGER, -(data.len() as i32)])?;
            return values::set_attr(object, symbol, stored);
        }
    }
    values::set_attr(object, symbol, value)
}

unsafe fn do_attributes(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let mut items: Vec<(String, Sexp)> = Vec::new();
    if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
        let names = values::pairlist_names(object)?;
        if names != nil() {
            items.push(("names".into(), names));
        }
    }
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == symbols().names && object.kind() != SexpType::Environment {
            items.push(("names".into(), get_attr(object, symbols().names)));
        } else if cell.tag() != symbols().names {
            let value = if cell.tag() == symbols().row_names {
                expand_row_names(cell.car())?
            } else {
                cell.car()
            };
            items.push((symbol_name(cell.tag()).into_owned(), value));
        }
        cell = cell.cdr();
    }
    if items.is_empty() {
        return Ok(nil());
    }
    let pairs: Vec<(&str, Sexp)> = items
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    values::named_list(&pairs)
}

unsafe fn do_set_attributes(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let attributes = call.arg(1);
    if object == nil() {
        if attributes == nil() {
            return Ok(nil());
        }
        return Err(call.error("attempt to set an attribute on NULL"));
    }
    let object = modifiable(object)?;
    let was_s4 = object.has_gp(gp::S4_OBJECT);
    values::clear_attributes(object);
    if attributes == nil() {
        return Ok(object);
    }
    // GNU R drops the S4 bit only when no attributes remain, so `structure()` on an S4
    // object keeps it an S4 object.
    if was_s4 && length(attributes) > 0 {
        object.set_gp_bits(gp::S4_OBJECT, true);
    }
    if attributes.kind() != SexpType::List {
        return Err(call.error("attributes must be a list or NULL"));
    }
    let names = values::names(attributes);
    if length(attributes) > 0 && names == nil() {
        return Err(call.error("attributes must be named"));
    }
    // `dim` goes first, so `dimnames` can be checked against it.
    for pass in 0..2 {
        for index in 0..length(attributes) {
            let name = names.element(index);
            let is_dim = name.bytes() == b"dim";
            if (pass == 0) != is_dim {
                continue;
            }
            let symbol = crate::heap().install_char(name)?;
            set_attribute_checked(object, symbol, attributes.element(index), call.call)?;
        }
    }
    Ok(object)
}

// Matrices and arrays.

unsafe fn do_matrix(call: &Call<'_>) -> R<Sexp> {
    let data = values::materialize(call.arg(0))?;
    let nrow_arg = call.arg(1);
    let ncol_arg = call.arg(2);
    let by_row = values::as_logical_scalar(call.arg(3)) == 1;
    let dimnames = call.arg(4);
    let missing_nrow = values::as_logical_scalar(call.arg(5)) == 1;
    let missing_ncol = values::as_logical_scalar(call.arg(6)) == 1;
    let count = length(data);
    let (rows, cols) = match (missing_nrow, missing_ncol) {
        (true, true) => (count, 1),
        (false, true) => {
            let rows = values::as_integer_scalar(nrow_arg).max(0) as usize;
            (rows, if rows == 0 { 0 } else { count.div_ceil(rows) })
        }
        (true, false) => {
            let cols = values::as_integer_scalar(ncol_arg).max(0) as usize;
            (if cols == 0 { 0 } else { count.div_ceil(cols) }, cols)
        }
        (false, false) => (
            values::as_integer_scalar(nrow_arg).max(0) as usize,
            values::as_integer_scalar(ncol_arg).max(0) as usize,
        ),
    };
    let total = rows * cols;
    if count > 0
        && total > 0
        && (total % count != 0 && count % rows.max(1) != 0 || (count > total && count % total != 0))
    {
        if count > total {
            conditions::warning_call(
                call.call,
                format!("data length differs from size of matrix: [{count} != {rows} x {cols}]"),
            )?;
        } else if (rows > 0 && count % rows != 0) || (cols > 0 && count % cols != 0) {
            conditions::warning_call(
                call.call,
                format!(
                    "data length [{count}] is not a sub-multiple or multiple of the number of rows [{rows}]"
                ),
            )?;
        }
    }
    let kind = if data == nil() {
        SexpType::Logical
    } else {
        data.kind()
    };
    let result = alloc(kind, total)?;
    if count == 0 {
        for position in 0..total {
            set_na(result, position);
        }
    } else {
        for row in 0..rows {
            for col in 0..cols {
                let target = row + col * rows;
                let source = if by_row { row * cols + col } else { target };
                copy_element(data, source % count, result, target)?;
            }
        }
    }
    let dim = values::integer_vector(&[rows as i32, cols as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    if dimnames != nil() && length(dimnames) > 0 {
        set_dimnames(result, dimnames, call.call)?;
    }
    Ok(result)
}

unsafe fn do_array(call: &Call<'_>) -> R<Sexp> {
    let data = values::materialize(call.arg(0))?;
    let dims = coerce_vector(call.arg(1), SexpType::Integer)?;
    let dimnames = call.arg(2);
    let total: usize = values::ints(dims)
        .iter()
        .map(|&value| value.max(0) as usize)
        .product();
    let count = length(data);
    let kind = if data == nil() {
        SexpType::Logical
    } else {
        data.kind()
    };
    let result = alloc(kind, total)?;
    for position in 0..total {
        if count == 0 {
            set_na(result, position);
        } else {
            copy_element(data, position % count, result, position)?;
        }
    }
    let dims = values::duplicate(dims)?;
    values::clear_attributes(dims);
    values::set_attr(result, symbols().dim, dims)?;
    if dimnames != nil() && length(dimnames) > 0 {
        set_dimnames(result, dimnames, call.call)?;
    }
    Ok(result)
}

unsafe fn do_transpose(call: &Call<'_>) -> R<Sexp> {
    let object = values::materialize(call.arg(0))?;
    let (rows, cols, dimnames) = match values::dims(object) {
        Some(dims) if dims.len() == 2 => (dims[0], dims[1], get_attr(object, symbols().dimnames)),
        Some(dims) if dims.len() == 1 => (dims[0], 1, get_attr(object, symbols().dimnames)),
        None => {
            let names = values::names(object);
            let dimnames = if names != nil() {
                values::list_vector(&[names, nil()])?
            } else {
                nil()
            };
            (length(object), 1, dimnames)
        }
        _ => return Err(call.error("argument is not a matrix")),
    };
    let result = alloc(object.kind(), rows * cols)?;
    for row in 0..rows {
        for col in 0..cols {
            copy_element(object, row + col * rows, result, col + row * cols)?;
        }
    }
    values::copy_most_attributes(object, result, false)?;
    let dim = values::integer_vector(&[cols as i32, rows as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    if dimnames != nil() {
        let first = if length(dimnames) > 1 {
            dimnames.element(1)
        } else {
            nil()
        };
        let second = dimnames.element(0);
        let swapped = values::list_vector(&[first, second])?;
        let names = values::names(dimnames);
        if names != nil() && length(names) == 2 {
            let new_names = alloc(SexpType::String, 2)?;
            values::set_elt(new_names, 0, names.element(1));
            values::set_elt(new_names, 1, names.element(0));
            values::set_attr(swapped, symbols().names, new_names)?;
        }
        values::set_attr(result, symbols().dimnames, swapped)?;
    }
    Ok(result)
}

/// The column of each row's maximum. Ties are broken at random, first or last
/// (`ties.method` 1, 2 or 3). At random, values within a relative tolerance of the row's
/// largest magnitude count as tied, and each tied column replaces the choice so far with
/// probability one over the number of ties seen, so every tied column is equally likely.
unsafe fn do_max_col(call: &Call<'_>) -> R<Sexp> {
    let matrix = crate::coerce::coerce_vector(call.arg(0), SexpType::Real)?;
    let (rows, cols) = match values::dims(call.arg(0)) {
        Some(dims) if dims.len() == 2 => (dims[0], dims[1]),
        _ => return Err(call.error("'m' must be a matrix")),
    };
    let ties = values::as_integer_scalar(call.arg(1));
    let values: &[f64] = matrix.slice();
    let at = |row: usize, col: usize| values.get(row + col * rows).copied().unwrap_or(f64::NAN);
    let random = ties == 1;
    if random {
        crate::random::get_rng_state()?;
    }
    let result = alloc(SexpType::Integer, rows)?;
    for row in 0..rows {
        if cols == 0 || (0..cols).any(|col| at(row, col).is_nan()) {
            result.slice_mut::<i32>()[row] = NA_INTEGER;
            continue;
        }
        let mut best = 0;
        let mut best_value = at(row, 0);
        if random {
            let largest = (0..cols).map(|col| at(row, col).abs()).fold(0.0, f64::max);
            let tolerance = 1e-5 * largest;
            let mut tied = 1.0;
            for col in 1..cols {
                let value = at(row, col);
                if value > best_value + tolerance {
                    tied = 1.0;
                    best_value = value;
                    best = col;
                } else if value >= best_value - tolerance {
                    tied += 1.0;
                    if tied * crate::random::unif_rand()? < 1.0 {
                        best = col;
                    }
                }
            }
        } else {
            for col in 1..cols {
                let value = at(row, col);
                if value > best_value || (ties == 3 && value == best_value) {
                    best_value = value;
                    best = col;
                }
            }
        }
        result.slice_mut::<i32>()[row] = best as i32 + 1;
    }
    if random {
        crate::random::put_rng_state()?;
    }
    Ok(result)
}

unsafe fn do_aperm(call: &Call<'_>) -> R<Sexp> {
    let object = values::materialize(call.arg(0))?;
    let permutation = call.arg(1);
    let resize = values::as_logical_scalar(call.arg(2)) != 0;
    let Some(dims) = values::dims(object) else {
        return Err(call.error("invalid first argument, must be an array"));
    };
    let rank = dims.len();
    let order: Vec<usize> = if permutation == nil() {
        (0..rank).rev().collect()
    } else if permutation.kind() == SexpType::String {
        let dimnames_names = values::names(get_attr(object, symbols().dimnames));
        (0..rank)
            .map(|index| {
                let wanted = permutation.element(index);
                (0..rank)
                    .find(|&candidate| {
                        dimnames_names != nil() && dimnames_names.element(candidate) == wanted
                    })
                    .unwrap_or(index)
            })
            .collect()
    } else {
        (0..rank)
            .map(|index| numeric_element(permutation, index) as usize - 1)
            .collect()
    };
    let new_dims: Vec<usize> = order.iter().map(|&dimension| dims[dimension]).collect();
    let total: usize = dims.iter().product();
    let result = alloc(object.kind(), total)?;
    let mut counters = vec![0usize; rank];
    for position in 0..total {
        // `counters` indexes the result; map back to the source.
        let mut source = 0usize;
        let mut stride = 1usize;
        let mut source_counters = vec![0usize; rank];
        for (new_dimension, &old_dimension) in order.iter().enumerate() {
            source_counters[old_dimension] = counters[new_dimension];
        }
        for (dimension, &counter) in source_counters.iter().enumerate() {
            source += counter * stride;
            stride *= dims[dimension];
        }
        copy_element(object, source, result, position)?;
        for (dimension, counter) in counters.iter_mut().enumerate() {
            *counter += 1;
            if *counter < new_dims[dimension] {
                break;
            }
            *counter = 0;
        }
    }
    if resize {
        let dim_values: Vec<i32> = new_dims.iter().map(|&extent| extent as i32).collect();
        let dim = values::integer_vector(&dim_values)?;
        values::set_attr(result, symbols().dim, dim)?;
        let dimnames = get_attr(object, symbols().dimnames);
        if dimnames != nil() {
            let permuted: Vec<Sexp> = order
                .iter()
                .map(|&dimension| dimnames.element(dimension))
                .collect();
            let list = values::list_vector(&permuted)?;
            let names = values::names(dimnames);
            if names != nil() {
                let new_names = select_elements(names, &order)?;
                values::set_attr(list, symbols().names, new_names)?;
            }
            values::set_attr(result, symbols().dimnames, list)?;
        }
    } else {
        let dim = get_attr(object, symbols().dim);
        values::set_attr(result, symbols().dim, dim)?;
    }
    Ok(result)
}

unsafe fn do_drop(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let Some(dims) = values::dims(object) else {
        return Ok(object);
    };
    let kept: Vec<usize> = (0..dims.len())
        .filter(|&dimension| dims[dimension] != 1)
        .collect();
    if kept.len() == dims.len() {
        return Ok(object);
    }
    let result = values::shallow_duplicate(object)?;
    let dimnames = get_attr(object, symbols().dimnames);
    values::remove_attr(result, symbols().dim);
    values::remove_attr(result, symbols().dimnames);
    if kept.len() > 1 {
        let dim_values: Vec<i32> = kept
            .iter()
            .map(|&dimension| dims[dimension] as i32)
            .collect();
        let dim = values::integer_vector(&dim_values)?;
        values::set_attr(result, symbols().dim, dim)?;
        if dimnames != nil() {
            let list: Vec<Sexp> = kept
                .iter()
                .map(|&dimension| dimnames.element(dimension))
                .collect();
            let list = values::list_vector(&list)?;
            values::set_attr(result, symbols().dimnames, list)?;
        }
    } else if dimnames != nil() {
        let all_dimnames: Vec<Sexp> = (0..dims.len())
            .map(|dimension| dimnames.element(dimension))
            .collect();
        let names = dropped_names(&kept, &all_dimnames);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
        }
    }
    Ok(result)
}

/// The names of an array dropped to a plain vector: those of the one dimension `kept`, or for a
/// single element those of the only dimension that has names. With names on several
/// dimensions, none of them names a single element.
fn dropped_names(kept: &[usize], dimnames: &[Sexp]) -> Sexp {
    let named: Vec<Sexp> = match kept {
        [dimension] => dimnames.get(*dimension).copied().into_iter().collect(),
        _ => dimnames
            .iter()
            .copied()
            .filter(|names| *names != nil())
            .collect(),
    };
    match named.as_slice() {
        [names] => *names,
        _ => nil(),
    }
}

/// `cbind` and `rbind` for vectors and matrices; data frames dispatch to their R methods.
unsafe fn bind(call: &Call<'_>, by_row: bool) -> R<Sexp> {
    // The arguments arrive unevaluated, because `deparse.level` names a vector argument after
    // the expression that produced it, which only the promise still knows.
    let Some((level, rest)) = call.args.split_first() else {
        return Err(call.error("argument \"deparse.level\" is missing"));
    };
    let level_value = eval::eval(level.value, call.env)?;
    let deparse_level = values::as_integer_scalar(level_value);
    let mut items: Vec<Arg> = Vec::with_capacity(rest.len());
    let mut expressions: Vec<Sexp> = Vec::with_capacity(rest.len());
    for arg in rest {
        if arg.value == symbols().dots {
            let dots = eval::find_var(symbols().dots, call.env)?;
            let mut cell = dots;
            while matches!(cell.kind(), SexpType::Dots | SexpType::Pairlist) {
                let item = cell.car();
                let mut expression = item;
                while expression.kind() == SexpType::Promise {
                    expression = expression.cdr();
                }
                items.push(Arg {
                    tag: cell.tag(),
                    value: value_of(item)?,
                });
                expressions.push(expression);
                cell = cell.cdr();
            }
        } else {
            items.push(Arg {
                tag: arg.tag,
                value: eval::eval(arg.value, call.env)?,
            });
            expressions.push(arg.value);
        }
    }
    // S3 dispatch, as GNU R's `do_bind` does it: the first classed argument with a method
    // picks it, and the method gets the `...` arguments alone, never `deparse.level`.
    let generic = if by_row { "rbind" } else { "cbind" };
    for arg in &items {
        if !arg.value.is_object() {
            continue;
        }
        let classes = crate::dispatch::implicit_class(arg.value)?;
        for index in 0..length(classes) {
            let symbol =
                values::install(&format!("{generic}.{}", char_str(classes.element(index))))?;
            if let Some(method) =
                crate::dispatch::lookup_method(symbol, call.env, values::base_namespace())?
            {
                // The method sees promises, so `data.frame` inside `cbind.data.frame` names a
                // column after its argument expression.
                let mut promises = Vec::with_capacity(items.len());
                for (item, &expression) in items.iter().zip(&expressions) {
                    promises.push(Arg {
                        tag: item.tag,
                        value: eval::forced_promise(expression, item.value)?,
                    });
                }
                return eval::apply(call.call, method, &promises, call.env);
            }
        }
    }
    let (items, expressions): (Vec<Arg>, Vec<Sexp>) = items
        .into_iter()
        .zip(expressions)
        .filter(|(arg, _)| arg.value != nil())
        .unzip();
    if items.is_empty() {
        return Ok(nil());
    }
    let mut kind = SexpType::Logical;
    for arg in &items {
        kind = coerce::higher_type(kind, arg.value.kind());
    }
    // The extent along the bound dimension.
    let mut extent = 0usize;
    for arg in &items {
        if let Some(dims) = values::dims(arg.value).filter(|dims| dims.len() == 2) {
            extent = if by_row { dims[1] } else { dims[0] };
            break;
        }
    }
    if extent == 0 {
        extent = items.iter().map(|arg| length(arg.value)).max().unwrap_or(0);
    }
    let mut pieces: Vec<(Sexp, usize, bool)> = Vec::new();
    let mut total = 0usize;
    for arg in &items {
        match values::dims(arg.value).filter(|dims| dims.len() == 2) {
            Some(dims) => {
                let count = if by_row { dims[0] } else { dims[1] };
                pieces.push((arg.value, count, true));
                total += count;
            }
            None => {
                pieces.push((arg.value, 1, false));
                total += 1;
            }
        }
    }
    let (rows, cols) = if by_row {
        (total, extent)
    } else {
        (extent, total)
    };
    let result = alloc(kind, rows * cols)?;
    let mut offset = 0usize;
    let mut bound_names: Vec<String> = Vec::new();
    let mut other_names = nil();
    for (position, &(value, count, is_matrix)) in pieces.iter().enumerate() {
        let source_length = length(value);
        for line in 0..count {
            for along in 0..extent {
                let target = if by_row {
                    (offset + line) + along * rows
                } else {
                    along + (offset + line) * rows
                };
                if source_length == 0 {
                    set_na(result, target);
                    continue;
                }
                let source = if is_matrix {
                    let dims = values::dims(value).unwrap_or_default();
                    if by_row {
                        line + along * dims[0]
                    } else {
                        along + line * dims[0]
                    }
                } else {
                    along % source_length
                };
                copy_element(value, source, result, target)?;
            }
            let arg = &items[position];
            if is_matrix {
                let dimnames = get_attr(value, symbols().dimnames);
                let names = if dimnames != nil() {
                    dimnames.element(if by_row { 0 } else { 1 })
                } else {
                    nil()
                };
                bound_names.push(if names != nil() {
                    values::string_at(names, line)
                        .map(|text| text.into_owned())
                        .unwrap_or_default()
                } else {
                    String::new()
                });
                if other_names == nil() && dimnames != nil() {
                    other_names = dimnames.element(if by_row { 1 } else { 0 });
                }
            } else {
                let expression = expressions[position];
                let name = if arg.tag != nil() {
                    symbol_name(arg.tag).into_owned()
                } else if deparse_level >= 1 && expression.kind() == SexpType::Symbol {
                    symbol_name(expression).into_owned()
                } else if deparse_level >= 2 && expression != nil() {
                    crate::deparse::deparse_one_line(expression)
                } else {
                    String::new()
                };
                bound_names.push(name);
                if other_names == nil() && source_length == extent {
                    let names = values::names(value);
                    if names != nil() {
                        other_names = names;
                    }
                }
            }
        }
        offset += count;
    }
    let dim = values::integer_vector(&[rows as i32, cols as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    let bound = if bound_names.iter().any(|name| !name.is_empty()) {
        string_vector(&bound_names)?
    } else {
        nil()
    };
    if bound != nil() || other_names != nil() {
        let dimnames = if by_row {
            values::list_vector(&[bound, other_names])?
        } else {
            values::list_vector(&[other_names, bound])?
        };
        values::set_attr(result, symbols().dimnames, dimnames)?;
    }
    Ok(result)
}

/// The unevaluated expressions behind the `...` arguments of the `cbind` or `rbind` closure
/// whose environment is `env`, which `deparse.level` names untagged vectors after. A position
/// with no expression to read (the internal called some other way) holds `NULL`.
unsafe fn argument_expressions(env: Sexp, count: usize) -> R<Vec<Sexp>> {
    let mut expressions = vec![nil(); count];
    let dots = eval::find_var(symbols().dots, env)?;
    if dots.kind() != SexpType::Dots || values::pairlist_length(dots) != count {
        return Ok(expressions);
    }
    let mut cell = dots;
    for expression in expressions.iter_mut() {
        let mut value = cell.car();
        while value.kind() == SexpType::Promise {
            value = value.cdr();
        }
        *expression = value;
        cell = cell.cdr();
    }
    Ok(expressions)
}

unsafe fn do_rev(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let count = length(object);
    let indices: Vec<usize> = (0..count).rev().collect();
    let result = select_elements(object, &indices)?;
    let names = values::names(object);
    if names != nil() {
        let reversed = select_elements(names, &indices)?;
        values::set_attr(result, symbols().names, reversed)?;
    }
    Ok(result)
}

unsafe fn do_is_list_factor(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let recursive = values::as_logical_scalar(call.arg(1)) == 1;
    unsafe fn check(object: Sexp, recursive: bool) -> Option<bool> {
        if object.kind() != SexpType::List {
            return Some(values::inherits(object, "factor"));
        }
        let mut any = false;
        for index in 0..length(object) {
            let element = object.element(index);
            if element.kind() == SexpType::List {
                if !recursive {
                    return Some(false);
                }
                match check(element, recursive) {
                    Some(true) => any = true,
                    Some(false) => return Some(false),
                    None => {}
                }
            } else if values::inherits(element, "factor") {
                any = true;
            } else {
                return Some(false);
            }
        }
        if any { Some(true) } else { None }
    }
    if object.kind() != SexpType::List || length(object) == 0 {
        return values::scalar_bool(false);
    }
    values::scalar_bool(check(object, recursive) == Some(true))
}

unsafe fn row_col(call: &Call<'_>, rows: bool) -> R<Sexp> {
    let dims = coerce_vector(call.arg(0), SexpType::Integer)?;
    if length(dims) != 2 {
        return Err(call.error("a matrix-like object is required as argument to 'row'/'col'"));
    }
    let row_count = values::ints(dims)[0].max(0) as usize;
    let col_count = values::ints(dims)[1].max(0) as usize;
    let result = alloc(SexpType::Integer, row_count * col_count)?;
    for row in 0..row_count {
        for col in 0..col_count {
            values::ints_mut(result)[row + col * row_count] =
                if rows { row as i32 + 1 } else { col as i32 + 1 };
        }
    }
    let dim = values::integer_vector(&[row_count as i32, col_count as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    Ok(result)
}

unsafe fn do_diag(call: &Call<'_>) -> R<Sexp> {
    let data = values::materialize(call.arg(0))?;
    let rows = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let cols = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let kind = if matches!(data.kind(), SexpType::Complex) {
        SexpType::Complex
    } else if data.kind() == SexpType::Integer {
        SexpType::Integer
    } else {
        SexpType::Real
    };
    let data = coerce_vector(data, kind)?;
    let result = alloc(kind, rows * cols)?;
    let count = length(data);
    for index in 0..rows.min(cols) {
        if count > 0 {
            copy_element(data, index % count, result, index + index * rows)?;
        }
    }
    let dim = values::integer_vector(&[rows as i32, cols as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    Ok(result)
}

unsafe fn do_short_row_names(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let kind = values::as_integer_scalar(call.arg(1));
    let row_names = get_attr(object, symbols().row_names);
    if kind == 0 {
        return Ok(row_names);
    }
    let count = if row_names.kind() == SexpType::Integer
        && length(row_names) == 2
        && values::ints(row_names)[0] == NA_INTEGER
    {
        values::ints(row_names)[1]
    } else if row_names == nil() {
        0
    } else {
        length(row_names) as i32
    };
    scalar_integer(if kind == 1 { count } else { count.abs() })
}

unsafe fn do_copy_df_attr(call: &Call<'_>) -> R<Sexp> {
    let from = call.arg(0);
    let to = modifiable(call.arg(1))?;
    crate::heap().set_attrib(to, nil());
    values::copy_attributes(from, to)?;
    Ok(to)
}
