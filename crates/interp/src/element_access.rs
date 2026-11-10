//! The direct entry of `[[` for one element of a plain vector or list, which a compiled
//! `x[[i]]` takes once its subject is known not to dispatch, and the in-place `x[i] <- value` a
//! compiled assignment takes for an index vector when the variable owns its value.

use {
    crate::{
        attribute_access::same_text,
        builtins::is_native_value,
        eval::reuse_permitted,
        moves,
        subset::{int_to_real, vector_element},
        unwind::R,
        values::{self, length, na_string, nil},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp},
};

/// `x[[i]]` for a vector or list `x` that is not an object and a scalar `i` that selects an
/// element: a position within bounds, or for a list a name one element has exactly, and
/// `x[[i, j]]` for a matrix `x` with positions `i` and `j` within its rows and columns. Anything
/// else (out of bounds, a missing name, a recursive subscript, a name in a matrix subscript)
/// declines to `[[`'s full path, which owns those errors and results.
pub(crate) unsafe fn direct_subset2(args: &[Sexp]) -> R<Option<Sexp>> {
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
unsafe fn scalar_position(index: Sexp, count: usize) -> Option<usize> {
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
/// value that widens the vector or recycles with a warning, a list) declines to the full `[<-`.
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
        || !target.kind().is_atomic()
        || target.is_object()
        || target.is_altrep()
        || index.is_altrep()
        || index.attrib() != nil()
        || !value.kind().is_atomic()
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
