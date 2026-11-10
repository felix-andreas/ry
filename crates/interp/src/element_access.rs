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
/// element: a position within bounds, or for a list a name one element has exactly. Anything
/// else (out of bounds, a missing name, a recursive or matrix subscript) declines to `[[`'s
/// full path, which owns those errors and results.
pub(crate) unsafe fn direct_subset2(args: &[Sexp]) -> R<Option<Sexp>> {
    let &[object, index] = args else {
        return Ok(None);
    };
    let list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    if !(list || object.kind().is_atomic())
        || object.is_object()
        || object.is_altrep()
        || object.is_array()
        || is_native_value(object)
        || index.is_altrep()
        || index.attrib() != nil()
        || length(index) != 1
    {
        return Ok(None);
    }
    let count = length(object);
    let position = match index.kind() {
        SexpType::Integer => {
            let position = values::ints(index)[0];
            if position == NA_INTEGER || position < 1 || position as usize > count {
                return Ok(None);
            }
            position as usize - 1
        }
        SexpType::Real => {
            // NaN fails both comparisons, and `x[[2.7]]` truncates to the second element.
            let position = values::reals(index)[0];
            if !(position >= 1.0 && position < count as f64 + 1.0) {
                return Ok(None);
            }
            position as usize - 1
        }
        SexpType::String if list => {
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
