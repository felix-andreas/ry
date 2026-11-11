//! Element reads without a call: the direct entry of `[[` for one element of a plain vector or
//! list, which a compiled `x[[i]]` takes once its subject is known not to dispatch, and
//! `read_element` for compiled element reads. Also the in-place `x[i] <- value` a compiled
//! assignment takes for an index vector when the variable owns its value.

use {
    crate::{
        attribute_access::same_text,
        builtins::is_native_value,
        eval::reuse_permitted,
        moves,
        subset::{int_to_real, vector_element},
        unwind::R,
        values::{self, length, na_string, nil, symbols},
    },
    heap::{
        Heap,
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp},
    },
};

/// `x[i]` or `x[[i]]` (`double`) read by tier 0's handler of a call to the base primitive,
/// without the call: the element when `x` is a double, integer or logical vector (or for `[[` a
/// list) that is not an object, an array or ALTREP, and has no attributes for `[`, and `i` a
/// plain numeric scalar within bounds. A number is a fresh scalar from the young generation.
/// `None` for anything else, and when that allocation needs a collection, so the builtin runs.
#[inline(always)]
pub(crate) unsafe fn read_element(
    heap: &mut Heap,
    subject: Sexp,
    index: Sexp,
    double: bool,
) -> Option<Sexp> {
    let kind = subject.kind();
    if !matches!(
        kind,
        SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::List
    ) || (kind == SexpType::List && !double)
        || subject.is_object()
        || subject.is_array()
        || subject.is_altrep()
        || (!double && subject.attrib() != heap.known().nil)
    {
        return None;
    }
    let position = if index.kind() == SexpType::String {
        if !double {
            return None;
        }
        name_position(heap, subject, index)?
    } else {
        scalar_position(index, subject.length() as usize)?
    };
    let data = subject.data();
    match kind {
        SexpType::List => Some(*data.cast::<Sexp>().add(position)),
        SexpType::Real => {
            let bits = (*data.cast::<f64>().add(position)).to_bits();
            heap.try_alloc_scalar(kind, [bits, 0])
        }
        _ => {
            let bits = *data.cast::<i32>().add(position) as u32 as u64;
            heap.try_alloc_scalar(kind, [bits, 0])
        }
    }
}

/// `x[i]` for an attribute-free atomic vector `x` and a plain numeric scalar `i` within bounds:
/// the element as a length-one vector. `None` otherwise.
pub(crate) unsafe fn scalar_subset(object: Sexp, index: Sexp) -> R<Option<Sexp>> {
    if !matches!(index.kind(), SexpType::Integer | SexpType::Real)
        || !object.kind().is_atomic()
        || object.is_altrep()
        || index.is_altrep()
        || values::has_attributes(object)
        || values::has_attributes(index)
        || length(index) != 1
    {
        return Ok(None);
    }
    let position = match index.kind() {
        SexpType::Integer => f64::from(index.slice::<i32>()[0]),
        _ => index.slice::<f64>()[0],
    };
    // NA_integer_ and NaN fail the lower bound, and `x[2.7]` truncates to the second element.
    if position >= 1.0 && position < length(object) as f64 + 1.0 {
        return vector_element(object, position as usize - 1).map(Some);
    }
    Ok(None)
}

/// `x[[i]]` for a vector or list `x` that is not an object and a scalar `i` that selects an
/// element: a position within bounds, or for a list a name one element has exactly, and
/// `x[[i, j]]` for a matrix `x` with positions `i` and `j` within its rows and columns. Anything
/// else (out of bounds, a missing name, a recursive subscript, a name in a matrix subscript)
/// declines to `[[`'s full path, which owns those errors and results.
pub(crate) unsafe fn direct_subset2(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let (object, index, column) = match *args {
        [object, index] => (object, index, None),
        [object, row, column] => (object, row, Some(column)),
        _ => return Ok(None),
    };
    let list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    if !(list || object.kind().is_atomic())
        || object.is_object()
        || object.is_altrep()
        || object.is_array()
        || is_native_value(object)
    {
        return Ok(None);
    }
    let count = length(object);
    if let Some(column) = column {
        let dim = values::get_attr(object, values::symbols().dim);
        if dim.kind() != SexpType::Integer || length(dim) != 2 {
            return Ok(None);
        }
        let &[rows, columns] = values::ints(dim) else {
            return Ok(None);
        };
        let (Some(row), Some(column)) = (
            scalar_position(index, rows as usize),
            scalar_position(column, columns as usize),
        ) else {
            return Ok(None);
        };
        let position = row + column * rows as usize;
        if position >= count {
            return Ok(None);
        }
        if list {
            return Ok(Some(object.element(position)));
        }
        return vector_element(object, position).map(Some);
    }
    let position = match index.kind() {
        SexpType::Integer | SexpType::Real => match scalar_position(index, count) {
            Some(position) => position,
            None => return Ok(None),
        },
        SexpType::String
            if list && !index.is_altrep() && index.attrib() == nil() && length(index) == 1 =>
        {
            let wanted = index.element(0);
            if wanted == na_string() || wanted.bytes().is_empty() {
                return Ok(None);
            }
            let names = values::names(object);
            if names.kind() != SexpType::String {
                return Ok(None);
            }
            match (0..length(names)).find(|&at| {
                let name = names.element(at);
                name != na_string() && same_text(name, wanted)
            }) {
                Some(position) if position < count => position,
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    if list {
        return Ok(Some(object.element(position)));
    }
    vector_element(object, position).map(Some)
}

/// The zero-based position a plain scalar integer or real subscript selects among `count`
/// elements, or `None` when it is out of bounds, `NA`, or not such a scalar.
/// The position of the first of `subject`'s names that is the one-string `index`, when that
/// string is ASCII and not `NA`, which matches no name. Every ASCII `CHARSXP` is one interned object whatever its encoding mark, so
/// the names that match it exactly are the ones that are the same pointer; any other string, or
/// a name no element has, is left to the full subscript.
#[inline(always)]
pub(crate) unsafe fn name_position(heap: &Heap, subject: Sexp, index: Sexp) -> Option<usize> {
    let nil = heap.known().nil;
    if index.is_altrep() || index.attrib() != nil || index.length() != 1 {
        return None;
    }
    let name = *index.data().cast::<Sexp>();
    if name == na_string() || !name.has_gp(gp::ASCII) || name.length() == 0 {
        return None;
    }
    let names = values::get_attr(subject, symbols().names);
    if names.kind() != SexpType::String || names.is_altrep() {
        return None;
    }
    let names = std::slice::from_raw_parts(names.data().cast::<Sexp>(), names.length() as usize);
    names.iter().position(|candidate| *candidate == name)
}

#[inline(always)]
pub(crate) unsafe fn scalar_position(index: Sexp, count: usize) -> Option<usize> {
    if index.is_altrep() || index.attrib() != nil() || length(index) != 1 {
        return None;
    }
    match index.kind() {
        SexpType::Integer => {
            let position = values::ints(index)[0];
            (position != NA_INTEGER && position >= 1 && position as usize <= count)
                .then(|| position as usize - 1)
        }
        // NaN fails both comparisons, and `x[[2.7]]` truncates to the second element.
        SexpType::Real => {
            let position = values::reals(index)[0];
            (position >= 1.0 && position < count as f64 + 1.0).then(|| position as usize - 1)
        }
        _ => None,
    }
}

/// `variable[index] <- value` updated in place, for a variable bound in `env` to a vector that is
/// not an object and that nothing else references, an index of positions within bounds (integer
/// or real) or a logical index as long as the vector, and a value without attributes whose type
/// the vector holds without widening and whose length divides the number of positions. Returns
/// whether it assigned; anything else (an index that grows, drops or names elements, `NA`, a
/// value that widens the vector or recycles with a warning, an atomic value for a list target)
/// declines to the full `[<-`. A list target takes a list value's elements, counted as stored.
pub(crate) unsafe fn assign_subset_in_place(
    env: Sexp,
    variable: Sexp,
    index: Sexp,
    value: Sexp,
) -> R<bool> {
    let heap = crate::heap();
    let Some(cell) = heap.frame_cell(env, variable) else {
        return Ok(false);
    };
    let target = cell.cdr();
    if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED)
        || target == value
        || !(target.kind().is_atomic() || target.kind() == SexpType::List)
        || target.is_object()
        || target.is_altrep()
        || index.is_altrep()
        || index.attrib() != nil()
        || !(value.kind().is_atomic() || value.kind() == SexpType::List)
        || value.is_altrep()
        || value.attrib() != nil()
    {
        return Ok(false);
    }
    let fits = match target.kind() {
        SexpType::Real => matches!(
            value.kind(),
            SexpType::Real | SexpType::Integer | SexpType::Logical
        ),
        SexpType::Integer => matches!(value.kind(), SexpType::Integer | SexpType::Logical),
        // A list target takes the elements of a list value; an atomic value would need boxing.
        kind => value.kind() == kind,
    };
    let supply = length(value);
    if !fits || supply == 0 {
        return Ok(false);
    }
    let count = length(target);
    let positions = match index.kind() {
        SexpType::Integer => {
            let positions = values::ints(index);
            if positions.iter().any(|&position| {
                position == NA_INTEGER || position < 1 || position as usize > count
            }) {
                return Ok(false);
            }
            positions.len()
        }
        SexpType::Real => {
            // NaN fails both comparisons, and `x[2.7]` truncates to the second element.
            let positions = values::reals(index);
            if !positions
                .iter()
                .all(|&position| position >= 1.0 && position < count as f64 + 1.0)
            {
                return Ok(false);
            }
            positions.len()
        }
        SexpType::Logical => {
            let selected = values::ints(index);
            if selected.len() != count || selected.contains(&NA_LOGICAL) {
                return Ok(false);
            }
            selected.iter().filter(|&&flag| flag != 0).count()
        }
        _ => return Ok(false),
    };
    if positions == 0 || positions % supply != 0 {
        return Ok(false);
    }
    // The conditions under which `complex_assign` updates the variable's value in place.
    moves::modifying(target)?;
    if values::maybe_shared(target) || !reuse_permitted(env, target) {
        return Ok(false);
    }
    let mut written = 0;
    let mut write = |at: usize| {
        let from = written % supply;
        written += 1;
        match (target.kind(), value.kind()) {
            (SexpType::Real, SexpType::Real) => {
                values::reals_mut(target)[at] = values::reals(value)[from]
            }
            (SexpType::Real, _) => {
                values::reals_mut(target)[at] = int_to_real(values::ints(value)[from])
            }
            (SexpType::Integer | SexpType::Logical, _) => {
                values::ints_mut(target)[at] = values::ints(value)[from]
            }
            (SexpType::Complex, _) => {
                values::complexes_mut(target)[at] = values::complexes(value)[from]
            }
            (SexpType::Raw, _) => values::raws_mut(target)[at] = values::raws(value)[from],
            _ => heap.set_element(target, at, value.element(from)),
        }
    };
    match index.kind() {
        SexpType::Integer => {
            for &position in values::ints(index) {
                write(position as usize - 1);
            }
        }
        SexpType::Real => {
            for &position in values::reals(index) {
                write(position as usize - 1);
            }
        }
        _ => {
            for (at, &flag) in values::ints(index).iter().enumerate() {
                if flag != 0 {
                    write(at);
                }
            }
        }
    }
    cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
    Ok(true)
}
