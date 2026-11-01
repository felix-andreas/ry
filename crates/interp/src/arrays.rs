//! ry's `Array<T>` at runtime: the `[1, 2, 3]` constructor, the conversions to and from R
//! vectors, and the rules that make an Array strict where an R vector is lenient.
//!
//! An Array is an ordinary R vector with the heap's Array bit set (`Sexp::is_array`), so it
//! keeps R's unboxed storage, `.Call` code reads it as the vector it is, and converting it to an
//! R vector only clears the bit. The bit promises three invariants that R vectors lack: the
//! vector has no attributes, holds no NA, and is homogeneous. An Array of logical, integer,
//! double or character items is that atomic vector; an Array of records, tuples, tags,
//! dictionaries, sets or Arrays is a list. Setting any attribute clears the bit (see
//! `Heap::set_attrib`), so no code path can produce an Array with names or a class.

use {
    crate::{
        builtins::{BuiltinDef, Call, builtin},
        coerce,
        unwind::{R, error_in},
        values::{self, alloc, length, missing_arg, nil, symbol_name},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType},
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".ry_array", -1, do_array),
    builtin("as.Array", 1, do_as_array),
    builtin("is.Array", 1, do_is_array),
];

/// What an Array holds, which is also what an operator or an assignment must match.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Items {
    Logical,
    Integer,
    Double,
    Character,
    /// Records, tuples, tags, dictionaries, sets or Arrays, stored in a list.
    RyValues,
}

impl Items {
    fn name(self) -> &'static str {
        match self {
            Items::Logical => "logical",
            Items::Integer => "integer",
            Items::Double => "double",
            Items::Character => "character",
            Items::RyValues => "ry value",
        }
    }

    fn storage(self) -> SexpType {
        match self {
            Items::Logical => SexpType::Logical,
            Items::Integer => SexpType::Integer,
            Items::Double => SexpType::Real,
            Items::Character => SexpType::String,
            Items::RyValues => SexpType::List,
        }
    }
}

/// The item kind of an Array.
pub unsafe fn items(array: Sexp) -> Items {
    match array.kind() {
        SexpType::Logical => Items::Logical,
        SexpType::Integer => Items::Integer,
        SexpType::Real => Items::Double,
        SexpType::String => Items::Character,
        _ => Items::RyValues,
    }
}

/// Whether `value` is a ry value an Array may hold as an item.
pub unsafe fn is_ry_value(value: Sexp) -> bool {
    value.kind() == SexpType::Record || value.is_array()
}

/// Marks `vector`, which the caller just allocated and nobody else references, as an Array.
pub unsafe fn finish(vector: Sexp) -> Sexp {
    vector.set_array(true);
    vector
}

/// The first missing value in an atomic vector, by position.
pub unsafe fn first_missing(vector: Sexp) -> Option<usize> {
    let count = length(vector);
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => {
            let missing = if vector.kind() == SexpType::Logical {
                NA_LOGICAL
            } else {
                NA_INTEGER
            };
            values::ints(vector)
                .iter()
                .position(|&item| item == missing)
        }
        SexpType::Real => values::reals(vector).iter().position(|item| item.is_nan()),
        SexpType::String => {
            (0..count).find(|&index| values::string_elt(vector, index) == values::na_string())
        }
        _ => None,
    }
}

/// The item kind a single value contributes to an Array literal, or why it cannot be an item.
unsafe fn item_kind(value: Sexp) -> Result<Items, String> {
    if is_ry_value(value) {
        return Ok(Items::RyValues);
    }
    let kind = match value.kind() {
        SexpType::Logical => Items::Logical,
        SexpType::Integer => Items::Integer,
        SexpType::Real => Items::Double,
        SexpType::String => Items::Character,
        _ => {
            return Err(format!(
                "an Array item must be a single logical, integer, double or character value or a ry value, not {}",
                describe(value)
            ));
        }
    };
    if value.is_object() || length(value) != 1 {
        return Err(format!(
            "an Array item must be a single value, not {}; convert a vector with `as.Array()`",
            describe(value)
        ));
    }
    if first_missing(value).is_some() {
        return Err("an Array cannot hold NA; mark an item that may be missing with a tag instead, as in `[:Some(1), :None]`".to_owned());
    }
    Ok(kind)
}

/// `value` for an error message, such as "a double vector of length 3".
pub unsafe fn describe(value: Sexp) -> String {
    if value.is_array() {
        return format!("an Array of {} items", length(value));
    }
    if value == nil() {
        return "NULL".to_owned();
    }
    let name = coerce::type_name(value);
    if value.kind().is_atomic() {
        format!("a {name} vector of length {}", length(value))
    } else {
        format!("a {name}")
    }
}

unsafe fn do_array(call: &Call<'_>) -> R<Sexp> {
    let mut items = Vec::with_capacity(call.args.len());
    for arg in call.args {
        if arg.value == missing_arg() {
            return Err(call.error("an Array item is empty"));
        }
        if arg.tag.kind() == SexpType::Symbol && !symbol_name(arg.tag).is_empty() {
            return Err(call.error("an Array's items have no names; use a record for named fields"));
        }
        items.push(arg.value);
    }
    from_items(&items)?.map_err(|message| call.error(message))
}

/// An Array of `items`, each a single non-NA value or a ry value, all of one kind; or why they
/// cannot form one. An empty Array is a logical vector of length zero.
pub unsafe fn from_items(items: &[Sexp]) -> R<Result<Sexp, String>> {
    let mut kind = None;
    for (position, &value) in items.iter().enumerate() {
        let item = match item_kind(value) {
            Ok(item) => item,
            Err(message) => return Ok(Err(message)),
        };
        match kind {
            None => kind = Some(item),
            Some(first) if first != item => {
                return Ok(Err(format!(
                    "Array items must all have one type, but item 1 is {} and item {} is {}",
                    first.name(),
                    position + 1,
                    item.name()
                )));
            }
            Some(_) => {}
        }
    }
    let kind = kind.unwrap_or(Items::Logical);
    let result = alloc(kind.storage(), items.len())?;
    for (index, &value) in items.iter().enumerate() {
        store_single(kind, value, result, index);
    }
    Ok(Ok(finish(result)))
}

unsafe fn do_as_array(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    if value.is_array() {
        return Ok(value);
    }
    let value = values::materialize(value)?;
    if value.is_object() {
        return Err(call.error(format!(
            "`as.Array()` takes a plain vector, but this one has class {}; convert it with `as.vector()` first",
            class_text(value)
        )));
    }
    match value.kind() {
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::String => {
            if let Some(position) = first_missing(value) {
                return Err(call.error(format!(
                    "an Array cannot hold NA, but element {} is NA",
                    position + 1
                )));
            }
        }
        SexpType::List => {
            for index in 0..length(value) {
                if !is_ry_value(values::list_elt(value, index)) {
                    return Err(call.error(format!(
                        "`as.Array()` turns a list into an Array only when every element is a ry value, but element {} is {}; use `unlist()` for a list of single values",
                        index + 1,
                        describe(values::list_elt(value, index))
                    )));
                }
            }
        }
        _ => {
            return Err(call.error(format!(
                "`as.Array()` takes a logical, integer, double or character vector or a list of ry values, not {}",
                describe(value)
            )));
        }
    }
    let copy = values::shallow_duplicate(value)?;
    values::clear_attributes(copy);
    Ok(finish(copy))
}

unsafe fn class_text(value: Sexp) -> String {
    values::strings(values::class_attr(value))
        .into_iter()
        .flatten()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

unsafe fn do_is_array(call: &Call<'_>) -> R<Sexp> {
    values::scalar_bool(call.arg(0).is_array())
}

/// An operator applied to at least one Array: `expression` is the operator's spelling,
/// `operands` its one or two arguments. Checks the strict rules before R's own operator runs:
/// the item types match exactly, a second operand is an Array of the same length or a single
/// plain value, and the operator suits the items.
pub unsafe fn check_operands(
    call: Sexp,
    operator: &str,
    class: OperatorClass,
    operands: &[Sexp],
) -> R<()> {
    let length_of = |value: Sexp| length(value);
    let array_length = operands
        .iter()
        .find(|value| value.is_array())
        .map(|&value| length_of(value))
        .unwrap_or(0);
    let mut kind: Option<Items> = None;
    for &operand in operands {
        let operand_kind = if operand.is_array() {
            if length_of(operand) != array_length {
                return Err(error_in(
                    call,
                    format!(
                        "`{operator}` needs Arrays of the same length, but they have {} and {} items; an Array is never recycled",
                        length_of(operands[0]),
                        length_of(operands[1])
                    ),
                ));
            }
            items(operand)
        } else {
            match item_kind(operand) {
                Ok(Items::RyValues) | Err(_) => {
                    return Err(error_in(
                        call,
                        format!(
                            "`{operator}` combines an Array only with another Array or a single value, not {}",
                            describe(operand)
                        ),
                    ));
                }
                Ok(item) => item,
            }
        };
        match kind {
            None => kind = Some(operand_kind),
            Some(first) if first != operand_kind => {
                return Err(error_in(
                    call,
                    format!(
                        "`{operator}` needs operands with one item type, but one is {} and the other is {}; convert one explicitly",
                        first.name(),
                        operand_kind.name()
                    ),
                ));
            }
            Some(_) => {}
        }
    }
    let kind = kind.unwrap_or(Items::Logical);
    let allowed = match class {
        OperatorClass::Arithmetic => matches!(kind, Items::Integer | Items::Double),
        OperatorClass::Comparison => kind != Items::RyValues,
        OperatorClass::Logic => kind == Items::Logical,
    };
    if !allowed {
        return Err(error_in(
            call,
            format!(
                "`{operator}` is not defined for an Array of {} items",
                kind.name()
            ),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub enum OperatorClass {
    Arithmetic,
    Comparison,
    Logic,
}

/// The result of an operator on Arrays, which R computed as a plain vector, as an Array. A
/// missing value (integer overflow, `0 / 0`) is an error, because an Array cannot hold one.
pub unsafe fn finish_result(call: Sexp, operator: &str, result: Sexp) -> R<Sexp> {
    if let Some(position) = first_missing(result) {
        return Err(error_in(
            call,
            format!(
                "`{operator}` produced a missing value at item {} (an integer overflow or an undefined result such as 0 / 0), which an Array cannot hold",
                position + 1
            ),
        ));
    }
    // Small results may be shared constants; never flag one of those in place.
    let result =
        if values::maybe_shared(result) || length(result) <= 1 || values::has_attributes(result) {
            let copy = values::shallow_duplicate(result)?;
            values::clear_attributes(copy);
            copy
        } else {
            result
        };
    Ok(finish(result))
}

/// `array[i]`: the items at positions `i` (whole numbers, or a logical mask as long as the
/// Array), as an Array. `array[]` is the Array itself.
pub unsafe fn subset(array: Sexp, indices: &[Sexp], call: Sexp) -> R<Sexp> {
    let index = match indices {
        [] => return Ok(array),
        [index] if *index == missing_arg() => return Ok(array),
        [index] => *index,
        _ => {
            return Err(error_in(
                call,
                "an Array has one dimension, so `[` takes one index",
            ));
        }
    };
    let positions = positions(array, index, call)?;
    let kind = items(array);
    let result = alloc(kind.storage(), positions.len())?;
    for (target, &source) in positions.iter().enumerate() {
        copy_item(array, source, result, target);
    }
    Ok(finish(result))
}

/// `array[[i]]`: the item at position `i`, which must exist.
pub unsafe fn subset2(array: Sexp, indices: &[Sexp], call: Sexp) -> R<Sexp> {
    let position = single_position(array, indices, call, "[[")?;
    item(array, position)
}

/// `array[i] <- value`: `value` is a single item or an Array with one item per position, of the
/// Array's item type and without NA.
pub unsafe fn subassign(array: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    let positions = match indices {
        [] => (0..length(array)).collect(),
        [index] if *index == missing_arg() => (0..length(array)).collect(),
        [index] => positions(array, *index, call)?,
        _ => {
            return Err(error_in(
                call,
                "an Array has one dimension, so `[<-` takes one index",
            ));
        }
    };
    let kind = items(array);
    let replacement = replacement(kind, value, call, "[<-")?;
    let count = if replacement.is_array() {
        length(replacement)
    } else {
        1
    };
    if replacement.is_array() && count != positions.len() {
        return Err(error_in(
            call,
            format!(
                "`[<-` assigns {} positions but the replacement Array has {count} items; an Array is never recycled",
                positions.len()
            ),
        ));
    }
    let target = writable(array)?;
    for (offset, &position) in positions.iter().enumerate() {
        if replacement.is_array() {
            copy_item(replacement, offset, target, position);
        } else {
            store_single(kind, replacement, target, position);
        }
    }
    Ok(target)
}

/// `array[[i]] <- value`: `value` is a single item of the Array's item type.
pub unsafe fn subassign2(array: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    let position = single_position(array, indices, call, "[[<-")?;
    let kind = items(array);
    let replacement = replacement(kind, value, call, "[[<-")?;
    if replacement.is_array() && kind != Items::RyValues {
        return Err(error_in(
            call,
            "`[[<-` stores one item; use `[<-` to assign several",
        ));
    }
    let target = writable(array)?;
    store_single(kind, replacement, target, position);
    Ok(target)
}

/// The item at `position` as an R value: a fresh scalar, or the ry value the list holds.
pub unsafe fn item(array: Sexp, position: usize) -> R<Sexp> {
    match array.kind() {
        SexpType::Logical => values::scalar_logical(values::ints(array)[position]),
        SexpType::Integer => values::scalar_integer(values::ints(array)[position]),
        SexpType::Real => values::scalar_real(values::reals(array)[position]),
        SexpType::String => values::scalar_string_charsxp(values::string_elt(array, position)),
        _ => Ok(values::list_elt(array, position)),
    }
}

unsafe fn copy_item(from: Sexp, source: usize, to: Sexp, target: usize) {
    match from.kind() {
        SexpType::Logical | SexpType::Integer => {
            values::ints_mut(to)[target] = values::ints(from)[source]
        }
        SexpType::Real => values::reals_mut(to)[target] = values::reals(from)[source],
        SexpType::String => values::set_elt(to, target, values::string_elt(from, source)),
        _ => values::set_elt(to, target, values::list_elt(from, source)),
    }
}

unsafe fn store_single(kind: Items, value: Sexp, to: Sexp, target: usize) {
    match kind {
        Items::Logical | Items::Integer => values::ints_mut(to)[target] = values::ints(value)[0],
        Items::Double => values::reals_mut(to)[target] = values::reals(value)[0],
        Items::Character => values::set_elt(to, target, values::string_elt(value, 0)),
        Items::RyValues => values::set_elt(to, target, value),
    }
}

/// The Array itself when nothing else references it, or else a copy, which keeps the Array bit.
unsafe fn writable(array: Sexp) -> R<Sexp> {
    if values::maybe_shared(array) {
        values::duplicate(array)
    } else {
        Ok(array)
    }
}

/// Checks a replacement against the Array's item type: a single item, or (for `[<-` and for an
/// Array of Arrays) an Array of the same item type.
unsafe fn replacement(kind: Items, value: Sexp, call: Sexp, operator: &str) -> R<Sexp> {
    let found = if value.is_array() && !(kind == Items::RyValues && operator == "[[<-") {
        items(value)
    } else {
        item_kind(value)
            .map_err(|message| error_in(call, format!("`{operator}` on an Array: {message}")))?
    };
    if found != kind {
        return Err(error_in(
            call,
            format!(
                "`{operator}` cannot store a {} in an Array of {} items; convert it explicitly",
                found.name(),
                kind.name()
            ),
        ));
    }
    Ok(value)
}

unsafe fn single_position(array: Sexp, indices: &[Sexp], call: Sexp, operator: &str) -> R<usize> {
    let [index] = indices else {
        return Err(error_in(
            call,
            format!("`{operator}` on an Array takes one position"),
        ));
    };
    if !matches!(index.kind(), SexpType::Integer | SexpType::Real) || length(*index) != 1 {
        return Err(error_in(
            call,
            format!(
                "`{operator}` on an Array takes one whole-number position, not {}",
                describe(*index)
            ),
        ));
    }
    let positions = positions(array, *index, call)?;
    Ok(positions[0])
}

/// The zero-based positions an index selects. Positions are whole numbers from 1 to the length,
/// and a logical mask has exactly one entry per item; anything else is an error, because an
/// Array has no names, no NA to return for a missing position, and no recycling.
unsafe fn positions(array: Sexp, index: Sexp, call: Sexp) -> R<Vec<usize>> {
    let count = length(array);
    let index = values::materialize(index)?;
    match index.kind() {
        SexpType::Logical => {
            if length(index) != count {
                return Err(error_in(
                    call,
                    format!(
                        "a logical mask needs one entry per item, but the Array has {count} items and the mask {}",
                        length(index)
                    ),
                ));
            }
            let mut selected = Vec::new();
            for (position, &flag) in values::ints(index).iter().enumerate() {
                if flag == NA_LOGICAL {
                    return Err(error_in(
                        call,
                        format!("the logical mask is NA at {}", position + 1),
                    ));
                }
                if flag != 0 {
                    selected.push(position);
                }
            }
            Ok(selected)
        }
        SexpType::Integer | SexpType::Real => {
            let numbers: Vec<f64> = if index.kind() == SexpType::Integer {
                values::ints(index)
                    .iter()
                    .map(|&number| {
                        if number == NA_INTEGER {
                            f64::NAN
                        } else {
                            number as f64
                        }
                    })
                    .collect()
            } else {
                values::reals(index).to_vec()
            };
            numbers
                .into_iter()
                .map(|number| {
                    if number.is_nan()
                        || number.fract() != 0.0
                        || number < 1.0
                        || number > count as f64
                    {
                        let shown = if number.is_nan() {
                            "NA".to_owned()
                        } else {
                            format!("{number}")
                        };
                        Err(error_in(
                            call,
                            format!(
                                "position {shown} is out of bounds for an Array of {count} items"
                            ),
                        ))
                    } else {
                        Ok(number as usize - 1)
                    }
                })
                .collect()
        }
        _ => Err(error_in(
            call,
            format!(
                "an Array is indexed by positions or a logical mask, not {}",
                describe(index)
            ),
        )),
    }
}
