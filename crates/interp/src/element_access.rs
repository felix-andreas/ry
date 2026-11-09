//! The direct entry of `[[` for one element of a plain vector or list, which a compiled
//! `x[[i]]` takes once its subject is known not to dispatch.

use {
    crate::{
        attribute_access::same_text,
        builtins::is_native_value,
        subset::vector_element,
        unwind::R,
        values::{self, length, na_string, nil},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType},
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
