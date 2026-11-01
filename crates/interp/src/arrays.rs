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

/// An operator applied to at least one Array: `operator` is its spelling, `operands` its one or
/// two arguments. Returns whether the strict rules govern it, checking them first: two Arrays
/// must have one item type and one length, and the operator must suit the items.
///
/// An operand that is neither an Array nor a single item of the Array's item type (an R
/// vector, an `NA`, a number of the other numeric type) makes it `false`: R's operator then
/// runs and the result is the plain R vector R would build, as an Array replacement does with a
/// value that is not an item. That keeps base R code working when it combines an Array with a
/// vector it derived, as `str()` does with `ob - signif(ob, digits)`. Typed code never gets
/// there, because the checker rejects such an operand.
pub unsafe fn check_operands(
    call: Sexp,
    operator: &str,
    class: OperatorClass,
    operands: &[Sexp],
) -> R<bool> {
    let arrays: Vec<Sexp> = operands
        .iter()
        .copied()
        .filter(|value| value.is_array())
        .collect();
    let Some(&first) = arrays.first() else {
        return Ok(false);
    };
    let kind = items(first);
    if let [left, right] = arrays[..] {
        if length(left) != length(right) {
            return Err(error_in(
                call,
                format!(
                    "`{operator}` needs Arrays of the same length, but they have {} and {} items; an Array is never recycled",
                    length(left),
                    length(right)
                ),
            ));
        }
        if items(right) != kind {
            return Err(error_in(
                call,
                format!(
                    "`{operator}` needs operands with one item type, but one is {} and the other is {}; convert one explicitly",
                    kind.name(),
                    items(right).name()
                ),
            ));
        }
    }
    let plain_items_fit = operands
        .iter()
        .filter(|value| !value.is_array())
        .all(|&value| kind != Items::RyValues && item_kind(value).is_ok_and(|found| found == kind));
    if !plain_items_fit {
        return Ok(false);
    }
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
    Ok(true)
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

/// `array[i]`: the items that positions `i` select, as an Array. `array[]` is the Array itself.
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

/// `array[i] <- value`, where `value` is a single item or an Array with one item per position,
/// of the Array's item type. `None` means `value` is not that (an NA, an R vector, another item
/// type), and the caller then assigns into the plain R vector instead. That keeps R code working
/// when it receives an Array, as base `ifelse()` does when it assigns doubles into its logical
/// `test`. Typed code never gets there, because the checker rejects such a value; the rules it
/// cannot check, a position in bounds and no recycling, stay errors.
pub unsafe fn subassign(array: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Option<Sexp>> {
    let kind = items(array);
    if !fits(kind, value, false) {
        return Ok(None);
    }
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
    if value.is_array() && length(value) != positions.len() {
        return Err(error_in(
            call,
            format!(
                "`[<-` assigns {} positions but the replacement Array has {} items; an Array is never recycled",
                positions.len(),
                length(value)
            ),
        ));
    }
    let target = writable(array)?;
    for (offset, &position) in positions.iter().enumerate() {
        if value.is_array() {
            copy_item(value, offset, target, position);
        } else {
            store_single(kind, value, target, position);
        }
    }
    Ok(Some(target))
}

/// `array[[i]] <- value`, under the rules of [`subassign`]: `None` when `value` is not one item
/// of the Array's item type, and an error when `i` is not one position in bounds.
pub unsafe fn subassign2(
    array: Sexp,
    indices: &[Sexp],
    value: Sexp,
    call: Sexp,
) -> R<Option<Sexp>> {
    let kind = items(array);
    if !fits(kind, value, true) {
        return Ok(None);
    }
    let position = single_position(array, indices, call, "[[<-")?;
    let target = writable(array)?;
    store_single(kind, value, target, position);
    Ok(Some(target))
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

/// The Array itself when nothing else references it, or else a copy flagged as an Array again
/// (a copy drops the bit, see `values::duplicate`).
unsafe fn writable(array: Sexp) -> R<Sexp> {
    if values::maybe_shared(array) {
        Ok(finish(values::duplicate(array)?))
    } else {
        Ok(array)
    }
}

/// Whether `value` can be stored in an Array of `kind` items: a single item of that kind, or
/// (for `[<-`, which assigns several positions) an Array of them. An Array of ry values holds an
/// Array as one item, so `[[<-` stores it whole.
unsafe fn fits(kind: Items, value: Sexp, single: bool) -> bool {
    if value.is_array() && !(single && kind == Items::RyValues) {
        return !single && items(value) == kind;
    }
    item_kind(value).is_ok_and(|found| found == kind)
}

/// The one position `[[` and `[[<-` take: a positive whole number in bounds.
unsafe fn single_position(array: Sexp, indices: &[Sexp], call: Sexp, operator: &str) -> R<usize> {
    let [index] = indices else {
        return Err(error_in(
            call,
            format!("`{operator}` on an Array takes one position"),
        ));
    };
    let single = match index.kind() {
        SexpType::Integer if length(*index) == 1 => Some(f64::from(values::ints(*index)[0])),
        SexpType::Real if length(*index) == 1 => Some(values::reals(*index)[0]),
        _ => None,
    };
    match single {
        Some(number) if number >= 1.0 => Ok(positions(array, *index, call)?[0]),
        _ => Err(error_in(
            call,
            format!(
                "`{operator}` on an Array takes one whole-number position, not {}",
                describe(*index)
            ),
        )),
    }
}

/// The positions an index selects, as in R but never producing an NA: whole numbers from 1 to
/// the length, or negative ones that leave positions out (zeros select nothing), or a logical
/// mask with one entry per item and no NA. Anything else is an error, because R would answer it
/// with NA or with names an Array does not have.
unsafe fn positions(array: Sexp, index: Sexp, call: Sexp) -> R<Vec<usize>> {
    let count = length(array);
    let index = values::materialize(index)?;
    match index.kind() {
        SexpType::Logical => {
            let mask = values::ints(index);
            if mask.len() != count {
                return Err(error_in(
                    call,
                    format!(
                        "a logical mask needs one entry per item, but the Array has {count} items and the mask {}",
                        mask.len()
                    ),
                ));
            }
            if let Some(position) = mask.iter().position(|&flag| flag == NA_LOGICAL) {
                return Err(error_in(
                    call,
                    format!("the logical mask is NA at {}", position + 1),
                ));
            }
            Ok(mask
                .iter()
                .enumerate()
                .filter(|(_, flag)| **flag != 0)
                .map(|(position, _)| position)
                .collect())
        }
        SexpType::Integer | SexpType::Real => {
            let numbers: Vec<f64> = if index.kind() == SexpType::Integer {
                values::ints(index)
                    .iter()
                    .map(|&number| {
                        if number == NA_INTEGER {
                            f64::NAN
                        } else {
                            f64::from(number)
                        }
                    })
                    .collect()
            } else {
                values::reals(index).to_vec()
            };
            let out_of_bounds = |number: f64| {
                let shown = if number.is_nan() {
                    "NA".to_owned()
                } else {
                    format!("{number}")
                };
                error_in(
                    call,
                    format!("position {shown} is out of bounds for an Array of {count} items"),
                )
            };
            // R truncates a fractional position toward zero.
            let mut whole = Vec::with_capacity(numbers.len());
            for number in numbers {
                if number.is_nan() || number > count as f64 {
                    return Err(out_of_bounds(number));
                }
                whole.push(number.trunc());
            }
            if whole.iter().any(|&number| number < 0.0) {
                if whole.iter().any(|&number| number > 0.0) {
                    return Err(error_in(
                        call,
                        "an index cannot mix positive and negative positions",
                    ));
                }
                let mut keep = vec![true; count];
                for number in whole.into_iter().filter(|&number| number < 0.0) {
                    if let Some(slot) = keep.get_mut((-number) as usize - 1) {
                        *slot = false;
                    }
                }
                return Ok(keep
                    .iter()
                    .enumerate()
                    .filter(|(_, kept)| **kept)
                    .map(|(position, _)| position)
                    .collect());
            }
            Ok(whole
                .into_iter()
                .filter(|&number| number >= 1.0)
                .map(|number| number as usize - 1)
                .collect())
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
