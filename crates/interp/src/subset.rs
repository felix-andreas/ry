//! Subsetting and subassignment (`[`, `[[`, `$`, `@` and their replacement forms), `c` and
//! `unlist`, lengths, names, dims, and attributes.

use {
    crate::{
        attribute_access::names_factor,
        builtins::{
            Arg, BuiltinDef, Call, Generic, builtin, internal, internal_special, is_native_value,
            special,
        },
        coerce::{self, coerce_vector},
        conditions,
        element_access::direct_subset2,
        eval::{self, value_of},
        handles::{
            self, Alt, Args, Atomic, Chars, Complex, Cx, Elements, Environment, Expression, Fresh,
            Gc, I64, Integer, Kind, Language, List, Local, Logical, Owned, Pairlist, Pointer,
            Promise, Raw, Real, Str, Symbol, Unfilled, Vector, attribute_cells,
            attributes_unshared, clear_attributes, coerce, compact_sequence, dense, element_text,
            get_attribute, int_element, is_missing_argument, is_na, is_s4, item, materialize, na,
            names_of, null, pairlist_to_list, real_element, remove_attribute, safe, same_text,
            set_attribute, set_class, store_item, symbol, unshared,
        },
        unwind::{R, Unwind, error, error_in, errorcall},
        values::{
            self, alloc, char_str, get_attr, length, missing_arg, na_string, nil, scalar_integer,
            symbol_name, symbols, unbound,
        },
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp},
    smallvec::{SmallVec, smallvec},
};

pub const BUILTINS: &[BuiltinDef] = &[
    special("[", -1, do_subset)
        .dispatch_or_eval()
        .direct(direct_subset),
    special("[[", -1, do_subset2)
        .dispatch_or_eval()
        .direct(direct_subset2),
    special("$", 2, do_dollar),
    special("@", 2, do_at),
    special("[<-", -1, do_subassign)
        .dispatch_or_eval()
        .direct(direct_subassign),
    special("[[<-", -1, do_subassign2).dispatch_or_eval(),
    special("$<-", 3, do_dollar_assign),
    special("@<-", 3, do_at_assign).direct(direct_at_assign),
    builtin(".subset", -1, do_subset),
    builtin(".subset2", -1, do_subset2).direct(direct_subset2),
    builtin("list", -1, do_list),
    builtin("length", 1, safe!(do_length))
        .generic(Generic::Internal)
        .unary(|value| unsafe {
            let plain = match value.kind() {
                SexpType::Nil => true,
                SexpType::Char => false,
                kind => {
                    kind.is_vector()
                        && !is_native_value(value)
                        && !value.is_object()
                        && !value.is_altrep()
                }
            };
            if !plain || value.length() > i32::MAX as isize {
                return Ok(None);
            }
            let count = if value == nil() {
                0
            } else {
                value.length() as i32
            };
            scalar_integer(count).map(Some)
        }),
    builtin("length<-", 2, safe!(do_set_length)).generic(Generic::Internal),
    internal("lengths", 2, do_lengths).generic(Generic::Internal),
    builtin("dim<-", 2, safe!(do_set_dim)).generic(Generic::Internal),
    builtin("dimnames", 1, safe!(do_dimnames)).generic(Generic::Internal),
    builtin("dimnames<-", 2, safe!(do_set_dimnames)).generic(Generic::Internal),
    builtin("levels<-", 2, safe!(do_set_levels)).generic(Generic::Internal),
    builtin("attributes<-", 2, safe!(do_set_attributes)),
    internal("matrix", 7, do_matrix),
    internal("array", 3, do_array),
    internal("t.default", 1, do_transpose),
    internal("max.col", 2, do_max_col),
    internal("aperm", 3, do_aperm),
    internal("asplit", 8, do_asplit),
    internal("drop", 1, do_drop),
    internal_special("cbind", -1, |call| unsafe { bind(call, false) }),
    internal_special("rbind", -1, |call| unsafe { bind(call, true) }),
    internal("rev", 1, do_rev),
    internal("islistfactor", 2, safe!(do_is_list_factor)),
    internal("comment", 1, safe!(do_comment)),
    internal("comment<-", 2, safe!(do_set_comment)),
    internal("row", 1, safe!(do_row)),
    internal("col", 1, safe!(do_col)),
    internal("diag", 3, safe!(do_diag)),
    internal("shortRowNames", 2, safe!(do_short_row_names)),
    internal("copyDFattr", 2, do_copy_df_attr),
];

/// A copy of `object` that may be modified in place.
pub unsafe fn modifiable(object: Sexp) -> R<Sexp> {
    // Ending a move hands the caller this same value back, which makes it shared.
    crate::moves::modifying(object)?;
    if values::maybe_shared(object) {
        values::shallow_duplicate(object)
    } else {
        Ok(object)
    }
}

/// [`modifiable`] for a builtin that changes only attributes, as GNU R's `names<-`,
/// `dimnames<-`, `attributes<-` and `class<-` are: a shared long vector is copied under a
/// wrapper, so its data is not copied at all, and an ALTREP instance keeps its class.
pub unsafe fn modifiable_attributes(object: Sexp) -> R<Sexp> {
    crate::moves::modifying(object)?;
    if values::maybe_shared(object) {
        values::replacement_copy(object)
    } else {
        Ok(object)
    }
}

fn do_comment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    Ok(context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.comment),
    )))
}

fn do_set_comment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let mut object = unshared(context, arguments.value(0))?;
    let comment = match handles::length(context, arguments.value(1)) {
        0 => null(),
        _ => arguments.value(1),
    };
    assign_attribute(
        context,
        &mut object,
        symbol(|symbols| symbols.comment),
        comment,
        None,
    )?;
    Ok(object.finish())
}

/// Element `index` of any vector as a length-one vector (or, for lists, the element).
pub(crate) fn element<'s>(context: &mut Cx<'s>, vector: Local<'s>, index: usize) -> R<Local<'s>> {
    // String and list elements read through the ALTREP class's `Elt` method on their own, and
    // logical, integer and double ones through `int_elt` and `real_elt`.
    match vector.r_type() {
        SexpType::List | SexpType::Expression => item(context, vector, index),
        SexpType::String => {
            let mut result = context.alloc::<Str>(1)?;
            store_item(context, vector, index, &mut result, 0)?;
            Ok(result.finish().value())
        }
        SexpType::Logical => {
            let value = int_element(context, vector, index)?;
            scalar::<Logical>(context, value)
        }
        SexpType::Integer => {
            let value = int_element(context, vector, index)?;
            scalar::<Integer>(context, value)
        }
        SexpType::Real => {
            let value = real_element(context, vector, index)?;
            scalar::<Real>(context, value)
        }
        SexpType::Complex => dense_element::<Complex>(context, vector, index),
        SexpType::Raw => dense_element::<Raw>(context, vector, index),
        SexpType::I64 => dense_element::<I64>(context, vector, index),
        _ => Err(error("object is not subsettable")),
    }
}

/// [`element`] for raw callers.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap, `vector` is
/// live. The result is rooted in the caller's handle scope, as a raw allocation is.
pub unsafe fn vector_element(vector: Sexp, index: usize) -> R<Sexp> {
    handles::with_result(|context| {
        let vector = handles::adopt(context, vector);
        element(context, vector, index).map(Local::raw)
    })
}

/// A length-one vector of `value`.
#[inline(always)]
fn scalar<'s, T: Atomic>(context: &mut Cx<'s>, value: T::Element) -> R<Local<'s>> {
    let result = context.alloc_unfilled::<T>(1)?;
    Ok(result.fill(std::iter::once(value)).finish().value())
}

/// Element `index` of a vector of `T`, expanded first when it is ALTREP, as a length-one
/// vector.
fn dense_element<'s, T: Atomic>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    index: usize,
) -> R<Local<'s>> {
    let Some(vector) = dense::<T>(context, vector)? else {
        return Err(error("object is not subsettable"));
    };
    let value = vector.as_slice(context)[index];
    scalar::<T>(context, value)
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

/// `to[to_index] <- from[from_index]`, converted to `to`'s type.
pub(crate) fn store_element<'s>(
    context: &mut Cx<'s>,
    from: Local<'s>,
    from_index: usize,
    to: &mut Owned<'s>,
    to_index: usize,
) -> R<()> {
    let from = materialize(context, from)?;
    let kind = to.get().r_type();
    if from.r_type() == kind {
        return match kind {
            SexpType::Logical => copy_number::<Logical>(context, from, from_index, to, to_index),
            SexpType::Integer => copy_number::<Integer>(context, from, from_index, to, to_index),
            SexpType::Real => copy_number::<Real>(context, from, from_index, to, to_index),
            SexpType::Complex => copy_number::<Complex>(context, from, from_index, to, to_index),
            SexpType::Raw => copy_number::<Raw>(context, from, from_index, to, to_index),
            SexpType::I64 => copy_number::<I64>(context, from, from_index, to, to_index),
            SexpType::String => copy_pointer::<Str>(context, from, from_index, to, to_index),
            SexpType::List => copy_pointer::<List>(context, from, from_index, to, to_index),
            SexpType::Expression => {
                copy_pointer::<Expression>(context, from, from_index, to, to_index)
            }
            _ => Ok(()),
        };
    }
    match kind {
        SexpType::List => {
            let value = element(context, from, from_index)?;
            put_pointer::<List>(context, to, to_index, value)
        }
        SexpType::Expression => {
            let value = element(context, from, from_index)?;
            put_pointer::<Expression>(context, to, to_index, value)
        }
        SexpType::String => {
            let chars = element_text(context, from, from_index)?;
            put_pointer::<Str>(context, to, to_index, chars)
        }
        // The widening conversions `c()` and growing assignments make, which never warn, write
        // the converted element directly.
        SexpType::Real if matches!(from.r_type(), SexpType::Integer | SexpType::Logical) => {
            let value = int_to_real(integers(context, from)[from_index]);
            put_number::<Real>(context, to, to_index, value)
        }
        SexpType::Integer if from.r_type() == SexpType::Logical => {
            let value = integers(context, from)[from_index];
            put_number::<Integer>(context, to, to_index, value)
        }
        _ => {
            let single = element(context, from, from_index)?;
            let converted = coerce(context, single, kind)?;
            store_element(context, converted, 0, to, to_index)
        }
    }
}

/// [`store_element`] for raw callers.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap, both are
/// live, and the caller may change `to` where it is.
pub unsafe fn copy_element(from: Sexp, from_index: usize, to: Sexp, to_index: usize) -> R<()> {
    handles::with(|context| {
        let from = handles::adopt(context, from);
        let mut to = Owned::assume(handles::adopt(context, to));
        store_element(context, from, from_index, &mut to, to_index)
    })
}

/// The elements of a standard logical or integer vector, or none for another type.
fn integers<'a, 's: 'a>(context: &'a Cx<'_>, vector: Local<'s>) -> &'a [i32] {
    match (vector.downcast::<Integer>(), vector.downcast::<Logical>()) {
        (Some(vector), _) => vector.as_slice(context),
        (_, Some(vector)) => vector.as_slice(context),
        _ => &[],
    }
}

/// `to` as a standard vector of `T`, which the caller has checked its type to be.
fn target<'a, 's, T: Kind>(to: &'a mut Owned<'s>) -> R<&'a mut Owned<'s, T>> {
    to.narrow::<T>()
        .ok_or_else(|| error("internal error: an assignment target is not a standard vector"))
}

/// `to[to_index] <- from[from_index]` for two vectors of plain elements of one type, which may
/// be the same vector.
fn copy_number<'s, T: Atomic>(
    context: &mut Cx<'s>,
    from: Local<'s>,
    from_index: usize,
    to: &mut Owned<'s>,
    to_index: usize,
) -> R<()> {
    let Some(source) = from.downcast::<T>() else {
        return Err(error(
            "internal error: a copy from a vector of another type",
        ));
    };
    let mut write = target::<T>(to)?.write(context);
    let (slots, reader) = write.split();
    slots[to_index] = match reader.slice(source) {
        Some(source) => source[from_index],
        None => slots[from_index],
    };
    Ok(())
}

/// `to[to_index] <- from[from_index]` for two character vectors or lists of one type.
fn copy_pointer<'s, T: Elements>(
    context: &mut Cx<'s>,
    from: Local<'s>,
    from_index: usize,
    to: &mut Owned<'s>,
    to_index: usize,
) -> R<()> {
    let Some(source) = from.downcast::<T>() else {
        return Err(error(
            "internal error: a copy from a vector of another type",
        ));
    };
    let mut write = target::<T>(to)?.write(context);
    let (mut stores, reader) = write.split_elements();
    stores.set(to_index, reader.element(source, from_index));
    Ok(())
}

fn put_number<'s, T: Atomic>(
    context: &mut Cx<'s>,
    to: &mut Owned<'s>,
    to_index: usize,
    value: T::Element,
) -> R<()> {
    let mut write = target::<T>(to)?.write(context);
    write.split().0[to_index] = value;
    Ok(())
}

fn put_pointer<'s, T: Elements>(
    context: &mut Cx<'s>,
    to: &mut Owned<'s>,
    to_index: usize,
    value: impl Pointer<T::Element>,
) -> R<()> {
    let mut write = target::<T>(to)?.write(context);
    write.split_elements().0.set(to_index, value);
    Ok(())
}

/// Copies the first `count` elements of `from` into a fresh `to` of the same type, which is not
/// ALTREP, in bulk.
pub(crate) unsafe fn copy_prefix(from: Sexp, to: Sexp, count: usize) -> R<()> {
    match from.kind() {
        SexpType::String | SexpType::List | SexpType::Expression => {
            crate::heap().copy_elements(to, 0, from, 0, count);
            Ok(())
        }
        _ => copy_mapped(from, to, count, |index| index, |index| index),
    }
}

/// `to[target(i)] <- from[source(i)]` for `i` in `0..count`, converting to `to`'s type, with a
/// typed loop for the same type and for the widening to double. `from` is not ALTREP.
unsafe fn copy_mapped(
    from: Sexp,
    to: Sexp,
    count: usize,
    source: impl Fn(usize) -> usize,
    target: impl Fn(usize) -> usize,
) -> R<()> {
    match (from.kind(), to.kind()) {
        (SexpType::Logical | SexpType::Integer, SexpType::Logical | SexpType::Integer)
            if from.kind() == to.kind() =>
        {
            let (from, to) = (values::ints(from), values::ints_mut(to));
            for index in 0..count {
                to[target(index)] = from[source(index)];
            }
        }
        (SexpType::Real, SexpType::Real) => {
            let (from, to) = (values::reals(from), values::reals_mut(to));
            for index in 0..count {
                to[target(index)] = from[source(index)];
            }
        }
        (SexpType::Logical | SexpType::Integer, SexpType::Real) => {
            let (from, to) = (values::ints(from), values::reals_mut(to));
            for index in 0..count {
                to[target(index)] = int_to_real(from[source(index)]);
            }
        }
        (SexpType::Complex, SexpType::Complex) => {
            let (from, to) = (values::complexes(from), values::complexes_mut(to));
            for index in 0..count {
                to[target(index)] = from[source(index)];
            }
        }
        _ => {
            for index in 0..count {
                copy_element(from, source(index), to, target(index))?;
            }
        }
    }
    Ok(())
}

/// An integer or logical element as a double, NA staying NA.
pub(crate) fn int_to_real(value: i32) -> f64 {
    if value == NA_INTEGER {
        NA_REAL
    } else {
        f64::from(value)
    }
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

/// Whether [`select`] reads `count` elements of `vector` one at a time instead of expanding it:
/// always for a compact sequence, whose elements are arithmetic and which GNU R leaves compact
/// when it is subset, and for another long ALTREP number vector of which the selection is a
/// small part. A short one expands cheaply, and its class keeps the expansion.
fn reads_sparsely(context: &mut Cx<'_>, vector: Local<'_>, count: usize) -> bool {
    if vector.downcast::<Alt>().is_none()
        || !matches!(
            vector.r_type(),
            SexpType::Logical | SexpType::Integer | SexpType::Real
        )
    {
        return false;
    }
    if compact_sequence(vector).is_some() {
        return true;
    }
    let extent = handles::length(context, vector);
    extent >= 1 << 16 && count <= extent / 8
}

/// The elements of `vector` at `positions` (`NA_POSITION` gives NA, or `NULL` in a list).
pub(crate) fn select<'s>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    positions: &[Position],
) -> R<Local<'s>> {
    select_iter(context, vector, positions.iter().copied(), positions.len())
}

/// [`select`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
pub unsafe fn select_elements(vector: Sexp, indices: &[usize]) -> R<Sexp> {
    select_positions(vector, indices.iter().copied(), indices.len())
}

/// [`select_iter`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
pub unsafe fn select_positions(
    vector: Sexp,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Sexp> {
    handles::with_result(|context| {
        let vector = handles::adopt(context, vector);
        select_iter(context, vector, positions, count).map(Local::raw)
    })
}

/// [`select`] for the `count` positions an iterator yields. Taking an iterator lets a caller
/// whose positions follow a rule, such as `rep`, gather without first writing eight bytes per
/// result element into a position list. The iterator must not allocate.
fn select_iter<'s>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Local<'s>> {
    let positions = positions.take(count);
    if vector.downcast::<Alt>().is_some() {
        // An ALTREP string or list reads only the selected elements through its class's `Elt`
        // method, so `as.character(x)[1:5]` converts five numbers, not all of `x`.
        match vector.r_type() {
            SexpType::String => {
                return read_items::<Str>(context, vector, positions, count, Some(na()));
            }
            SexpType::List => return read_items::<List>(context, vector, positions, count, None),
            _ if reads_sparsely(context, vector, count) => {
                return read_numbers(context, vector, positions, count);
            }
            _ => {}
        }
    }
    let vector = materialize(context, vector)?;
    match vector.r_type() {
        SexpType::Nil => Ok(null()),
        SexpType::Pairlist | SexpType::Language => {
            let list = pairlist_to_list(context, vector)?;
            gathered::<List>(context, list, positions, count)
        }
        SexpType::Logical => gathered::<Logical>(context, vector, positions, count),
        SexpType::Integer => gathered::<Integer>(context, vector, positions, count),
        SexpType::Real => gathered::<Real>(context, vector, positions, count),
        SexpType::Complex => gathered::<Complex>(context, vector, positions, count),
        SexpType::Raw => gathered::<Raw>(context, vector, positions, count),
        SexpType::I64 => gathered::<I64>(context, vector, positions, count),
        SexpType::String => gathered::<Str>(context, vector, positions, count),
        SexpType::List => gathered::<List>(context, vector, positions, count),
        SexpType::Expression => gathered::<Expression>(context, vector, positions, count),
        _ => Err(error("object is not subsettable")),
    }
}

/// `vector[positions]` for a standard vector of `T`.
#[inline(always)]
fn gathered<'s, T: Gather>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Local<'s>> {
    let Some(vector) = vector.downcast::<T>() else {
        return Err(error(
            "internal error: a selection from a vector of another type",
        ));
    };
    let result = context.alloc_unfilled::<T>(count)?;
    Ok(T::fill(result, context, vector, positions).finish().value())
}

/// A vector type [`select`] and [`gather_plain`] gather: how a new vector of it is filled from
/// a source of the same type, with the type's NA (`NULL` in a list) past the source's end.
trait Gather: Vector + Sized {
    fn fill<'s>(
        result: Unfilled<'s, Self>,
        context: &Cx<'s>,
        source: Local<'s, Self>,
        positions: impl Iterator<Item = usize>,
    ) -> Fresh<'s, Self>;
}

macro_rules! gather_numbers {
    ($($kind:ty => $missing:expr;)*) => {$(
        impl Gather for $kind {
            #[inline(always)]
            fn fill<'s>(
                result: Unfilled<'s, Self>,
                context: &Cx<'s>,
                source: Local<'s, Self>,
                positions: impl Iterator<Item = usize>,
            ) -> Fresh<'s, Self> {
                let source = source.as_slice(context);
                result.fill(
                    positions.map(|position| source.get(position).copied().unwrap_or($missing)),
                )
            }
        }
    )*};
}

gather_numbers! {
    Logical => NA_LOGICAL;
    Integer => NA_INTEGER;
    Real => NA_REAL;
    Complex => Rcomplex { r: NA_REAL, i: NA_REAL };
    Raw => 0;
    I64 => 0;
}

macro_rules! gather_pointers {
    ($($kind:ty => |$context:ident| $missing:expr;)*) => {$(
        impl Gather for $kind {
            // The elements go straight into the result's slots and are counted as references
            // afterwards, in one pass each.
            #[inline(always)]
            fn fill<'s>(
                result: Unfilled<'s, Self>,
                $context: &Cx<'s>,
                source: Local<'s, Self>,
                positions: impl Iterator<Item = usize>,
            ) -> Fresh<'s, Self> {
                let source = source.read($context);
                let extent = source.len();
                let missing = $missing;
                result.fill_elements(
                    $context,
                    positions.map(|position| match position < extent {
                        true => source.get(position),
                        false => missing,
                    }),
                )
            }
        }
    )*};
}

gather_pointers! {
    Str => |context| na();
    List => |context| null().read(context);
    Expression => |context| null().read(context);
}

/// The selected elements of an ALTREP character vector or list, each read through its class's
/// `Elt` method, with `missing` (or the fill, `NULL`) past the end.
fn read_items<'s, T: Elements>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    positions: impl Iterator<Item = usize>,
    count: usize,
    missing: Option<Gc<'static, T::Element>>,
) -> R<Local<'s>> {
    let extent = handles::length(context, vector);
    let mut result = context.alloc::<T>(count)?;
    for (slot, index) in positions.enumerate() {
        if index < extent {
            store_item(context, vector, index, &mut result, slot)?;
        } else if let Some(missing) = missing {
            result.set(context, slot, missing);
        }
    }
    Ok(result.finish().value())
}

/// A few elements of a long ALTREP number vector, such as `seq_len(2^31 - 1)[c(1, 10)]`, read
/// one at a time, because expanding a compact sequence can take gigabytes.
fn read_numbers<'s>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Local<'s>> {
    let extent = handles::length(context, vector);
    if let Some((_, first, increment)) = compact_sequence(vector) {
        let number = |index: usize| first + increment * index as f64;
        return Ok(if vector.r_type() == SexpType::Real {
            let result = context.alloc_unfilled::<Real>(count)?;
            let numbers = positions.map(|index| match index < extent {
                true => number(index),
                false => NA_REAL,
            });
            result.fill(numbers).finish().value()
        } else {
            let result = context.alloc_unfilled::<Integer>(count)?;
            let numbers = positions.map(|index| match index < extent {
                true => number(index) as i32,
                false => NA_INTEGER,
            });
            result.fill(numbers).finish().value()
        });
    }
    match vector.r_type() {
        SexpType::Real => {
            let mut result = context.alloc::<Real>(count)?;
            for (slot, index) in result.as_mut_slice().iter_mut().zip(positions) {
                *slot = match index < extent {
                    true => real_element(context, vector, index)?,
                    false => NA_REAL,
                };
            }
            Ok(result.finish().value())
        }
        SexpType::Logical => read_integers::<Logical>(context, vector, positions, count, extent),
        _ => read_integers::<Integer>(context, vector, positions, count, extent),
    }
}

fn read_integers<'s, T: Atomic<Element = i32>>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    positions: impl Iterator<Item = usize>,
    count: usize,
    extent: usize,
) -> R<Local<'s>> {
    let mut result = context.alloc::<T>(count)?;
    for (slot, index) in result.as_mut_slice().iter_mut().zip(positions) {
        *slot = match index < extent {
            true => int_element(context, vector, index)?,
            false => NA_INTEGER,
        };
    }
    Ok(result.finish().value())
}

/// `object[index]` for a plain logical, integer, double, character or list vector and an
/// attribute-free integer, double or logical subscript, gathered straight from the subscript
/// without first resolving it to a position list. `None` defers to the general path, which
/// handles every other shape (negative or zero subscripts, recycled logical subscripts, other
/// element types).
fn gather_plain<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    index: Local<'s>,
) -> R<Option<Local<'s>>> {
    if is_missing_argument(index)
        || has_attributes(context, index)
        || has_attributes(context, object)
    {
        return Ok(None);
    }
    // A selection `select` reads sparsely takes the general path.
    let count = handles::length(context, index);
    if reads_sparsely(context, object, count) {
        return Ok(None);
    }
    match object.r_type() {
        SexpType::Logical => gather_subscript::<Logical>(context, object, index),
        SexpType::Integer => gather_subscript::<Integer>(context, object, index),
        SexpType::Real => gather_subscript::<Real>(context, object, index),
        SexpType::String => gather_subscript::<Str>(context, object, index),
        SexpType::List => gather_subscript::<List>(context, object, index),
        _ => Ok(None),
    }
}

/// [`gather_plain`] for an object of `T`.
fn gather_subscript<'s, T: Gather>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    index: Local<'s>,
) -> R<Option<Local<'s>>> {
    let Some(object) = dense::<T>(context, object)? else {
        return Ok(None);
    };
    if let Some((_, offsets)) = compact_sequence(index).and_then(compact_offsets) {
        let result = context.alloc_unfilled::<T>(offsets.len())?;
        return Ok(Some(
            T::fill(result, context, object, offsets).finish().value(),
        ));
    }
    let extent = object.len();
    let index = materialize(context, index)?;
    if let Some(index) = index.downcast::<Integer>() {
        let positions = index.as_slice(context);
        if positions
            .iter()
            .all(|&position| position <= 0 && position != NA_INTEGER)
            && let Some(keep) = excluded(
                extent,
                positions.iter().map(|&position| -f64::from(position)),
            )
        {
            return gather_kept::<T>(context, object, &keep).map(Some);
        }
        // Every subscript must be NA or positive; anything else needs the general rules.
        if positions
            .iter()
            .any(|&position| position <= 0 && position != NA_INTEGER)
        {
            return Ok(None);
        }
        let result = context.alloc_unfilled::<T>(index.len())?;
        let positions = index.as_slice(context).iter();
        let positions = positions.map(|&position| (position as u32 as usize).wrapping_sub(1));
        return Ok(Some(
            T::fill(result, context, object, positions).finish().value(),
        ));
    }
    if let Some(index) = index.downcast::<Real>() {
        let positions = index.as_slice(context);
        if positions.iter().all(|&position| position < 1.0)
            && let Some(keep) = excluded(extent, positions.iter().map(|&position| -position))
        {
            return gather_kept::<T>(context, object, &keep).map(Some);
        }
        if positions.iter().any(|&position| position < 1.0) {
            return Ok(None);
        }
        let result = context.alloc_unfilled::<T>(index.len())?;
        let positions = index.as_slice(context).iter().map(|&position| {
            if !position.is_finite() {
                NA_POSITION
            } else {
                position.trunc() as usize - 1
            }
        });
        return Ok(Some(
            T::fill(result, context, object, positions).finish().value(),
        ));
    }
    if let Some(index) = index.downcast::<Logical>() {
        let flags = index.as_slice(context);
        if flags.len() != extent {
            return Ok(None);
        }
        let selected = flags.iter().filter(|&&flag| flag != 0).count();
        let result = context.alloc_unfilled::<T>(selected)?;
        let positions =
            index
                .as_slice(context)
                .iter()
                .enumerate()
                .filter_map(|(position, &flag)| match flag {
                    0 => None,
                    NA_LOGICAL => Some(NA_POSITION),
                    _ => Some(position),
                });
        return Ok(Some(
            T::fill(result, context, object, positions).finish().value(),
        ));
    }
    Ok(None)
}

/// Whether `object` has any attribute.
#[inline(always)]
fn has_attributes(context: &Cx<'_>, object: Local<'_>) -> bool {
    object.attributes(context).r_type() != SexpType::Nil
}

/// The zero-based offsets a compact sequence subscript selects, computed from its `c(n, n1,
/// inc)` info instead of expanding it, as GNU R reads a compact subscript through `*_ELT`, and
/// the highest of them. `None` unless every element is a whole number of at least one, the only
/// case that needs no further rules; offsets past the end are the caller's to check.
fn compact_offsets(
    (count, first, increment): (usize, f64, f64),
) -> Option<(usize, impl ExactSizeIterator<Item = usize>)> {
    let last = first + increment * count.saturating_sub(1) as f64;
    if first.min(last) < 1.0 || first.fract() != 0.0 || increment.abs() != 1.0 {
        return None;
    }
    let start = first as usize - 1;
    let step = increment as isize;
    let highest = first.max(last) as usize - 1;
    Some((
        highest,
        (0..count).map(move |slot| start.wrapping_add_signed(step * slot as isize)),
    ))
}

/// [`compact_offsets`] of a raw subscript.
unsafe fn sequence_offsets(index: Sexp) -> Option<(usize, impl ExactSizeIterator<Item = usize>)> {
    values::compact_sequence(index).and_then(compact_offsets)
}

/// One bit per element, inline for vectors of up to 256 elements.
type KeptMask = SmallVec<[u64; 4]>;

/// The elements negative subscripts keep, as a bitset with one bit per element (set when kept):
/// `excluded` is each subscript negated. `None` unless every subscript is negative or zero and at
/// least one is negative, the only case the mask covers; NA, a positive subscript or all zeros
/// take the general rules.
fn excluded(extent: usize, excluded: impl Iterator<Item = f64>) -> Option<KeptMask> {
    let mut keep: KeptMask = smallvec![u64::MAX; extent.div_ceil(64)];
    if let Some(last) = keep.last_mut()
        && extent % 64 != 0
    {
        *last = (1 << (extent % 64)) - 1;
    }
    let mut any_negative = false;
    for value in excluded {
        // A subscript between -1 and 1 truncates to zero, which selects nothing. An infinite
        // one counts as NA, which the general rules handle.
        if !value.is_finite() || value <= -1.0 {
            return None;
        }
        let position = value.trunc();
        if position >= 1.0 {
            any_negative = true;
            if position <= extent as f64 {
                let index = position as usize - 1;
                if let Some(word) = keep.get_mut(index / 64) {
                    *word &= !(1 << (index % 64));
                }
            }
        }
    }
    any_negative.then_some(keep)
}

/// The elements of `object` whose bit in `keep` is set, streamed without a position list.
fn gather_kept<'s, T: Gather>(
    context: &mut Cx<'s>,
    object: Local<'s, T>,
    keep: &[u64],
) -> R<Local<'s>> {
    let kept = keep.iter().map(|word| word.count_ones() as usize).sum();
    let result = context.alloc_unfilled::<T>(kept)?;
    Ok(T::fill(result, context, object, kept_positions(keep))
        .finish()
        .value())
}

/// The zero-based positions whose bit in `keep` is set, in increasing order.
fn kept_positions(keep: &[u64]) -> impl Iterator<Item = usize> + '_ {
    keep.iter().enumerate().flat_map(|(index, &word)| {
        let mut remaining = word;
        std::iter::from_fn(move || {
            (remaining != 0).then(|| {
                let bit = remaining.trailing_zeros() as usize;
                remaining &= remaining - 1;
                index * 64 + bit
            })
        })
    })
}

/// `length()` with S3 dispatch, for builtins that need an object's length.
pub unsafe fn dispatch_length(object: Sexp, env: Sexp) -> R<usize> {
    if !object.is_object() {
        return Ok(length(object));
    }
    let function = crate::builtins::primitive("length").ok_or_else(|| error("no length"))?;
    let value = eval::apply(nil(), function, &eval::value_args(&[object]), env)?;
    let value = crate::coerce::coerce_vector(value, SexpType::Integer)?;
    Ok(values::as_integer_scalar(value).max(0) as usize)
}

// Index resolution.

/// One resolved subscript: a zero-based position, possibly past the end. It is a plain `usize`
/// with two reserved values rather than an `Option`, because subsetting a long vector resolves
/// one position per element and the halved width and the lack of a conversion pass show up in
/// vector code.
type Position = usize;

/// Resolved subscripts, which are usually few.
type Positions = SmallVec<[Position; 8]>;
/// An `NA` subscript.
const NA_POSITION: Position = usize::MAX;
/// A character subscript that matches no name, outside assignment.
const UNMATCHED: Position = usize::MAX - 1;

/// Resolves a subscript against an extent. `names` are the names to match character subscripts
/// against; unmatched names give positions past the end, and, when assigning, the second part
/// names those positions in order.
pub(crate) fn resolve_subscript<'s>(
    context: &mut Cx<'s>,
    index: Local<'s>,
    extent: usize,
    names: Local<'s>,
    assigning: bool,
    call: Option<Local<'s>>,
) -> R<(Positions, Vec<Local<'s, Chars>>)> {
    if is_missing_argument(index) {
        return Ok(((0..extent).collect(), Vec::new()));
    }
    let index = materialize(context, index)?;
    let index = if handles::inherits(context, index, "factor") {
        coerce(context, index, SexpType::Integer)?
    } else {
        index
    };
    match index.r_type() {
        SexpType::Nil => Ok((Positions::new(), Vec::new())),
        SexpType::Logical => {
            let flags = standard::<Logical>(index)?.as_slice(context);
            let count = flags.len();
            if count == 0 {
                return Ok((Positions::new(), Vec::new()));
            }
            let total = extent.max(count);
            let mut positions = Positions::new();
            for position in 0..total {
                match flags[position % count] {
                    1 => positions.push(position),
                    NA_LOGICAL => positions.push(NA_POSITION),
                    _ => {}
                }
            }
            Ok((positions, Vec::new()))
        }
        SexpType::Integer | SexpType::Real => {
            let numbers = Numbers::of(context, index)?;
            let count = numbers.len();
            let mut any_negative = false;
            let mut any_positive = false;
            let mut any_na = false;
            for position in 0..count {
                let value = numbers.at(position);
                // An infinite subscript selects nothing real, so it counts as NA, as in GNU R.
                if !value.is_finite() {
                    any_na = true;
                } else if value < 0.0 && value.trunc() != 0.0 {
                    any_negative = true;
                } else if value >= 1.0 {
                    any_positive = true;
                }
            }
            if any_negative {
                let mixed = || call_error(call, "only 0's may be mixed with negative subscripts");
                if any_positive || any_na {
                    return Err(mixed());
                }
                let negated = (0..count).map(|position| -numbers.at(position));
                let Some(keep) = excluded(extent, negated) else {
                    return Err(mixed());
                };
                let kept = keep.iter().map(|word| word.count_ones() as usize).sum();
                let mut positions = Positions::with_capacity(kept);
                positions.extend(kept_positions(&keep));
                return Ok((positions, Vec::new()));
            }
            let mut positions = Positions::with_capacity(count);
            for position in 0..count {
                let value = numbers.at(position);
                if !value.is_finite() {
                    positions.push(NA_POSITION);
                } else if value >= 1.0 {
                    positions.push(value.trunc() as usize - 1);
                }
            }
            Ok((positions, Vec::new()))
        }
        SexpType::String => {
            let names = dense::<Str>(context, names)?;
            let wanted = standard::<Str>(index)?.read(context);
            let names = names.map(|names| names.read(context));
            let mut positions = Positions::with_capacity(wanted.len());
            let mut appended: Vec<Gc<'_, Chars>> = Vec::new();
            for position in 0..wanted.len() {
                let wanted = wanted.get(position);
                let found = match names {
                    Some(names) if !is_na(wanted) => (0..names.len()).find(|&candidate| {
                        let name = names.get(candidate);
                        same_text(name, wanted) && !name.bytes().is_empty()
                    }),
                    _ => None,
                };
                match found {
                    Some(found) => positions.push(found),
                    None if assigning => {
                        let existing = appended
                            .iter()
                            .position(|&name| !is_na(wanted) && same_text(name, wanted));
                        let slot = match existing {
                            Some(existing) => extent + existing,
                            None => {
                                appended.push(wanted);
                                extent + appended.len() - 1
                            }
                        };
                        positions.push(slot);
                    }
                    None => positions.push(UNMATCHED),
                }
            }
            let new_names = appended
                .into_iter()
                .map(|name| context.root(name))
                .collect();
            Ok((positions, new_names))
        }
        SexpType::Symbol => {
            let symbol = standard::<Symbol>(index)?;
            let name = context.root(symbol.read(context).name());
            let mut text = context.alloc::<Str>(1)?;
            text.set(context, 0, name);
            resolve_subscript(
                context,
                text.finish().value(),
                extent,
                names,
                assigning,
                call,
            )
        }
        SexpType::Complex => Err(call_error(call, "invalid subscript type 'complex'")),
        SexpType::List => Err(call_error(call, "invalid subscript type 'list'")),
        kind => Err(call_error(
            call,
            format!("invalid subscript type '{}'", kind.name()),
        )),
    }
}

/// [`resolve_subscript`] for raw callers, which pass the unbound marker for the running call
/// and `NULL` for none.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap, the three
/// objects are live, and the caller keeps `index` rooted while it reads the names.
unsafe fn resolve(
    index: Sexp,
    extent: usize,
    names: Sexp,
    assigning: bool,
    call: Sexp,
) -> R<(Positions, Vec<Option<Sexp>>)> {
    let (positions, new_names) = handles::with(|context| {
        let index = handles::adopt(context, index);
        let names = handles::adopt(context, names);
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        let (positions, new_names) =
            resolve_subscript(context, index, extent, names, assigning, call)?;
        let new_names: Vec<Option<Sexp>> =
            new_names.into_iter().map(|name| Some(name.raw())).collect();
        Ok((positions, new_names))
    })?;
    // Raw callers keep the new names in their scope, as they would a raw allocation's result.
    for name in new_names.iter().flatten() {
        crate::heap().root(*name);
    }
    Ok((positions, new_names))
}

/// `vector` as a standard vector of `T`, which the caller has checked its type to be.
#[inline(always)]
fn standard<'s, T: Kind>(vector: Local<'s>) -> R<Local<'s, T>> {
    vector
        .downcast::<T>()
        .ok_or_else(|| error("internal error: a vector is not of the expected kind"))
}

/// The elements of a standard integer or double subscript, read as doubles with NA as NA.
#[derive(Clone, Copy)]
enum Numbers<'a> {
    Integer(&'a [i32]),
    Real(&'a [f64]),
}

impl<'a> Numbers<'a> {
    fn of<'s: 'a>(context: &'a Cx<'_>, vector: Local<'s>) -> R<Numbers<'a>> {
        if let Some(vector) = vector.downcast::<Integer>() {
            return Ok(Numbers::Integer(vector.as_slice(context)));
        }
        Ok(Numbers::Real(standard::<Real>(vector)?.as_slice(context)))
    }

    fn len(self) -> usize {
        match self {
            Numbers::Integer(numbers) => numbers.len(),
            Numbers::Real(numbers) => numbers.len(),
        }
    }

    #[inline(always)]
    fn at(self, index: usize) -> f64 {
        match self {
            Numbers::Integer(numbers) => int_to_real(numbers[index]),
            Numbers::Real(numbers) => numbers[index],
        }
    }
}

/// GNU R converts a double matrix or array subscript to integer before it resolves it, so a
/// value outside the integer range selects `NA` with a warning, where a vector subscript of the
/// same value is quietly out of bounds.
pub(crate) fn dimension_index<'s>(context: &mut Cx<'s>, index: Local<'s>) -> R<Local<'s>> {
    const LIMIT: f64 = 2_147_483_648.0;
    let Some(numbers) = dense::<Real>(context, index)? else {
        return Ok(index);
    };
    if numbers
        .as_slice(context)
        .iter()
        .any(|number| number.abs() >= LIMIT)
    {
        return coerce(context, index, SexpType::Integer);
    }
    Ok(index)
}

/// [`dimension_index`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
unsafe fn dimension_subscript(index: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let index = handles::adopt(context, index);
        dimension_index(context, index).map(Local::raw)
    })
}

/// The zero-based position of `x[i]` or `x[[i]]` with one untagged, attribute-free numeric
/// scalar `i` inside the vector's bounds: the common case GNU R's byte code also answers before
/// resolving subscripts in general.
fn scalar_position(context: &mut Cx<'_>, arguments: &SubsetArgs<'_>) -> Option<usize> {
    let ([index], true) = (arguments.indices.as_slice(), arguments.plain) else {
        return None;
    };
    let object = arguments.object;
    let vector = object.r_type().is_atomic()
        || matches!(object.r_type(), SexpType::List | SexpType::Expression);
    if !vector || object.downcast::<Alt>().is_some() || has_attributes(context, *index) {
        return None;
    }
    let position = match (index.downcast::<Integer>(), index.downcast::<Real>()) {
        (Some(index), _) if index.len() == 1 => f64::from(index.as_slice(context)[0]),
        (_, Some(index)) if index.len() == 1 => index.as_slice(context)[0],
        _ => return None,
    };
    // NaN fails both comparisons, and `x[2.7]` truncates to the second element. An Array
    // rejects a position past its last item before truncating, which its own path reports.
    let count = handles::length(context, object) as f64;
    let within = if handles::is_array(object) {
        position <= count
    } else {
        position < count + 1.0
    };
    (position >= 1.0 && within).then(|| position as usize - 1)
}

/// The arguments of `[` and `[[`: the object, the subscripts, and `drop` and `exact`.
struct SubsetArgs<'s> {
    object: Local<'s>,
    indices: SmallVec<[Local<'s>; 2]>,
    drop: Option<Local<'s>>,
    exact: Option<Local<'s>>,
    /// Whether the call has exactly two arguments, neither tagged.
    plain: bool,
}

/// Splits a builtin's arguments into the object, the subscripts, and `drop`/`exact`.
///
/// # Safety
///
/// The caller keeps the call's arguments rooted until the context's scope ends.
unsafe fn subset_args<'s>(context: &Cx<'s>, call: &Call<'_>) -> SubsetArgs<'s> {
    let mut object = None;
    let mut indices = SmallVec::new();
    let mut drop = None;
    let mut exact = None;
    for arg in call.args {
        let value = handles::held(context, arg.value);
        if arg.tag == symbols().drop {
            drop = Some(value);
        } else if arg.tag == symbols().exact {
            exact = Some(value);
        } else if object.is_none() {
            object = Some(value);
        } else {
            indices.push(value);
        }
    }
    SubsetArgs {
        object: object.unwrap_or_else(null),
        indices,
        drop,
        exact,
        plain: matches!(call.args, [object, index] if object.tag == nil() && index.tag == nil()),
    }
}

// `[`.

unsafe fn do_subset(call: &Call<'_>) -> R<Sexp> {
    handles::with(|context| {
        let arguments = subset_args(context, call);
        let call = handles::held(context, call.call);
        subset_call(context, &arguments, call).map(Local::raw)
    })
}

fn subset_call<'s>(
    context: &mut Cx<'s>,
    arguments: &SubsetArgs<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    let object = arguments.object;
    // `[` on an Array makes an Array, which `subset` builds.
    if let Some(position) = scalar_position(context, arguments)
        && !has_attributes(context, object)
        && !handles::is_array(object)
        && object.r_type().is_atomic()
    {
        return element(context, object, position);
    }
    let drop = match arguments.drop {
        Some(drop) => handles::logical_scalar(context, drop) != 0,
        None => true,
    };
    subset_value(context, object, &arguments.indices, drop, Some(call))
}

/// `x[i, ...]` with positional subscripts on a subject that is not an object: the same
/// selection as [`do_subset`] without building the argument list, and a numeric scalar
/// position in an attribute-free atomic vector read straight out.
pub(crate) unsafe fn direct_subset(call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    handles::in_caller_scope(|context| {
        let call = handles::held(context, call);
        let args = Local::adopt_slice(args);
        subset_direct(context, call, args).map(|result| result.map(Local::raw))
    })
}

fn subset_direct<'s>(
    context: &mut Cx<'s>,
    call: Local<'s>,
    args: &[Local<'s>],
) -> R<Option<Local<'s>>> {
    let [object, indices @ ..] = args else {
        return Ok(None);
    };
    let object = *object;
    if handles::is_object(object)
        || is_native(object)
        || indices.iter().any(|&index| is_native(index))
    {
        return Ok(None);
    }
    if let [index] = indices
        && let Some(element) = handles::scalar_subset(context, object, *index)?
    {
        return Ok(Some(element));
    }
    if indices.len() >= 2
        && let Some(element) = scalar_cell(context, object, indices)?
    {
        return Ok(Some(element));
    }
    subset_value(context, object, indices, true, Some(call)).map(Some)
}

/// Whether `value` is a ry record or `I64`, which `[` leaves to its own methods.
fn is_native(value: Local<'_>) -> bool {
    matches!(value.r_type(), SexpType::Record | SexpType::I64)
}

/// `a[i, j, k]` with one plain in-range position per dimension of an array without dimnames:
/// the element as a length-one vector, found without building index vectors. `None` otherwise.
fn scalar_cell<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
) -> R<Option<Local<'s>>> {
    if !matches!(
        object.r_type(),
        SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::String
    ) {
        return Ok(None);
    }
    let dimnames = get_attribute(context, object, symbol(|symbols| symbols.dimnames));
    let dim = get_attribute(context, object, symbol(|symbols| symbols.dim));
    let Some(dim) = dim.downcast::<Integer>() else {
        return Ok(None);
    };
    if dim.len() != indices.len() || dimnames.r_type() != SexpType::Nil {
        return Ok(None);
    }
    let (mut offset, mut stride) = (0usize, 1usize);
    for (&index, &extent) in indices.iter().zip(dim.as_slice()) {
        let extent = extent.max(0) as usize;
        let Some(position) = matrix_index(context, index, extent) else {
            return Ok(None);
        };
        offset += position * stride;
        stride *= extent;
    }
    if offset >= handles::length(context, object) {
        return Ok(None);
    }
    element(context, object, offset).map(Some)
}

/// `x[...]` without dispatch, for raw callers, which pass the unbound marker for the running
/// call and `NULL` for none.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap, the object,
/// the subscripts and the call are live. The result is rooted in the caller's handle scope, as
/// a raw allocation is.
pub unsafe fn subset(object: Sexp, indices: &[Sexp], drop: bool, call: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let object = handles::adopt(context, object);
        let indices: SmallVec<[Local<'_>; 2]> = indices
            .iter()
            .map(|&index| handles::adopt(context, index))
            .collect();
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        subset_value(context, object, &indices, drop, call).map(Local::raw)
    })
}

/// `x[...]` without dispatch.
fn subset_value<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    drop: bool,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    if handles::is_array(object) {
        return handles::array_subset(context, object, indices, call);
    }
    // String and list elements read through the class's `Elt` method in `select`; the matrix
    // and array paths read the data directly, so a vector with dimensions materializes.
    let dims = handles::dims(context, object);
    let object = if matches!(
        object.r_type(),
        SexpType::String | SexpType::List | SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && dims.is_none()
    {
        object
    } else {
        materialize(context, object)?
    };
    if object.r_type() == SexpType::Object && is_s4(object) {
        let data = s4_data(context, object);
        if data.r_type() == SexpType::Nil {
            return Err(call_error(call, "object of type 'S4' is not subsettable"));
        }
        return subset_value(context, data, indices, drop, call);
    }
    match object.r_type() {
        SexpType::Nil => return Ok(null()),
        SexpType::Environment
        | SexpType::Closure
        | SexpType::Builtin
        | SexpType::Special
        | SexpType::Symbol
        | SexpType::Dots => return Err(not_subsettable_error(context, call, object)),
        SexpType::Pairlist | SexpType::Language => {
            let list = pairlist_to_list(context, object)?;
            let result = subset_value(context, list, indices, drop, call)?;
            // Subsetting a pairlist gives a list, as in GNU R; a call stays a call.
            if object.r_type() == SexpType::Language {
                let result = standard::<List>(result)?;
                return handles::list_to_pairlist(context, result, SexpType::Language);
            }
            return Ok(result);
        }
        _ => {}
    }
    if indices.len() >= 2 {
        let Some(dims) = dims else {
            return Err(call_error(call, "incorrect number of dimensions"));
        };
        if dims.len() != indices.len() {
            return Err(call_error(call, "incorrect number of dimensions"));
        }
        return matrix_subset(context, object, indices, &dims, drop, call);
    }
    let index = indices
        .first()
        .copied()
        .unwrap_or_else(handles::missing_argument);
    let missing = is_missing_argument(index);
    // A matrix with one column per dimension selects single elements.
    if let Some(dims) = &dims
        && let Some(positions) = matrix_positions(context, object, dims, index, call)?
    {
        let positions: Vec<usize> = positions
            .into_iter()
            .map(|position| position.unwrap_or(NA_POSITION))
            .collect();
        return select(context, object, &positions);
    }
    let count = handles::length(context, object);
    let names = names_of(context, object)?;
    // A parsed expression's per-element source references are subset along with it, as in GNU
    // R, so `attr(exprs[i], "srcref")` still locates element `i`.
    let srcrefs = get_attribute(context, object, symbol(|symbols| symbols.srcref));
    let srcrefs = srcrefs
        .downcast::<List>()
        .map(|srcrefs| context.root(srcrefs));
    if dims.is_none()
        && names.r_type() == SexpType::Nil
        && srcrefs.is_none()
        && let Some(result) = gather_plain(context, object, index)?
    {
        return Ok(result);
    }
    let (positions, _) = resolve_subscript(context, index, count, names, false, call)?;
    let selected = select(context, object, &positions)?;
    let mut result = owned(context, selected)?;
    if let Some(names) = dense::<Str>(context, names)? {
        let new_names = context.alloc_unfilled::<Str>(positions.len())?;
        let names = names.read(context);
        let new_names = new_names.fill_elements(
            context,
            positions.iter().map(|&position| match position < count {
                true => names.get(position),
                false => na(),
            }),
        );
        let new_names = new_names.finish().value();
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.names),
            new_names,
        )?;
    }
    if let Some(srcrefs) = srcrefs
        && !missing
    {
        let extent = srcrefs.len();
        let positions: Vec<usize> = positions
            .iter()
            .map(|&position| {
                if position < extent {
                    position
                } else {
                    NA_POSITION
                }
            })
            .collect();
        let selected = select(context, srcrefs.value(), &positions)?;
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.srcref),
            selected,
        )?;
    }
    // A one-dimensional array stays one when indexed, unless `drop` leaves at most one element,
    // which GNU R returns as a vector named by the dimnames.
    let one_dimensional = dims.as_ref().is_some_and(|dims| dims.len() == 1) && !missing;
    let first_dimnames = |context: &Cx<'s>| {
        let dimnames = get_attribute(context, object, symbol(|symbols| symbols.dimnames));
        let first = dimnames.downcast::<List>().map(|dimnames| dimnames.get(0));
        first
            .filter(|first| first.r_type() != SexpType::Nil)
            .map(|first| context.root(first))
    };
    if one_dimensional && drop && positions.len() <= 1 {
        if names.r_type() == SexpType::Nil
            && let Some(first) = first_dimnames(context)
        {
            let selected = select(context, first, &positions)?;
            set_attribute(
                context,
                &mut result,
                symbol(|symbols| symbols.names),
                selected,
            )?;
        }
    } else if one_dimensional {
        let dim = scalar::<Integer>(context, positions.len() as i32)?;
        let mut dim = owned(context, dim)?;
        let old_dim = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.dim),
        ));
        let dim_names = names_of(context, old_dim)?;
        if dim_names.r_type() != SexpType::Nil {
            set_attribute(
                context,
                &mut dim,
                symbol(|symbols| symbols.names),
                dim_names,
            )?;
        }
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.dim),
            dim.finish(),
        )?;
        if let Some(first) = first_dimnames(context) {
            let selected = select(context, first, &positions)?;
            let mut new_dimnames = context.alloc::<List>(1)?;
            new_dimnames.set(context, 0, selected);
            let mut new_dimnames = new_dimnames.owned().value();
            let dimnames = context.root(get_attribute(
                context,
                object,
                symbol(|symbols| symbols.dimnames),
            ));
            let dimnames_names = names_of(context, dimnames)?;
            if dimnames_names.r_type() != SexpType::Nil {
                set_attribute(
                    context,
                    &mut new_dimnames,
                    symbol(|symbols| symbols.names),
                    dimnames_names,
                )?;
            }
            let dimnames_symbol = symbol(|symbols| symbols.dimnames);
            set_attribute(context, &mut result, dimnames_symbol, new_dimnames.finish())?;
            remove_attribute(context, &mut result, symbol(|symbols| symbols.names));
        }
    }
    if missing {
        // `x[]` keeps every attribute but the class and `tsp`, which the default method drops
        // as it does for any subscript; a class with a `[` method restores its own. The copy
        // replaces the names set above, so the attributes keep the object's order.
        clear_attributes(context, &mut result, false);
        handles::copy_attributes_to(context, object, &mut result)?;
        remove_attribute(context, &mut result, symbol(|symbols| symbols.class));
        remove_attribute(context, &mut result, symbol(|symbols| symbols.tsp));
    } else if object.r_type() == SexpType::Expression
        || handles::inherits(context, object, "factor")
    {
        let levels = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.levels),
        ));
        if levels.r_type() != SexpType::Nil {
            set_attribute(
                context,
                &mut result,
                symbol(|symbols| symbols.levels),
                levels,
            )?;
        }
    }
    Ok(result.finish())
}

/// A value the caller made, to change where it is: itself when nothing refers to it, as for a
/// new vector, and otherwise a copy.
fn owned<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Owned<'s>> {
    match handles::unreferenced(value) {
        Some(owned) => Ok(owned),
        None => unshared(context, value),
    }
}

/// GNU R's `R_getS4DataSlot`: the data part of an S4 object whose class extends a basic type,
/// held in `.Data` or, for an environment such as a reference class object, in `.xData`.
/// `NULL` when the class extends none.
fn s4_data<'s>(context: &Cx<'s>, object: Local<'s>) -> Local<'s> {
    let data = get_attribute(context, object, symbol(|symbols| symbols.dot_data));
    if data.r_type() != SexpType::Nil {
        return context.root(data);
    }
    context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dot_xdata),
    ))
}

/// The error for subsetting an object of a type that has no elements.
fn not_subsettable_error(
    context: &mut Cx<'_>,
    call: Option<Local<'_>>,
    object: Local<'_>,
) -> Unwind {
    let message = format!(
        "object of type '{}' is not subsettable",
        object.r_type().name()
    );
    handles::classed_error(
        context,
        call,
        &message,
        &["notSubsettableError", "error", "condition"],
        &[("object", object)],
    )
}

/// `m[i, j, ...]` for a matrix or array that is not a ry Array.
fn matrix_subset<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    dims: &[usize],
    drop: bool,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let dimnames = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    ));
    let dimnames = dimnames.downcast::<List>();
    let component = |context: &Cx<'s>, dimension: usize| match dimnames {
        Some(dimnames) if dimension < dimnames.len() => {
            context.root(dimnames.get(context, dimension))
        }
        _ => null(),
    };
    // `m[i, ]` on a plain numeric matrix is a strided copy of one row, the shape of a
    // simulation that keeps one row per step. A single column drops to a scalar whose name
    // depends on both dimnames, which the general path works out.
    if drop
        && let ([row, column], &[rows, columns]) = (indices, dims)
        && is_missing_argument(*column)
        && columns != 1
        && let Some(row) = matrix_index(context, *row, rows)
        && let Some(result) = matrix_row(context, object, row, rows, columns)?
    {
        let names = component(context, 1);
        return named_slice(context, result, names);
    }
    // `m[, j]` is one contiguous run, the shape `apply` takes once per row or column.
    if drop
        && let ([row, column], &[rows, columns]) = (indices, dims)
        && is_missing_argument(*row)
        && rows != 1
        && let Some(column) = matrix_index(context, *column, columns)
        // C code can set a `dim` that disagrees with the length, which the general path reports.
        && (column + 1) * rows <= handles::length(context, object)
        && let Some(result) = matrix_column(context, object, column * rows, rows)?
    {
        let names = component(context, 0);
        return named_slice(context, result, names);
    }
    let mut selections: SmallVec<[Positions; 4]> = SmallVec::with_capacity(dims.len());
    for (dimension, (&index, &extent)) in indices.iter().zip(dims).enumerate() {
        let names = component(context, dimension);
        // Unlike a vector, a dimension never stretches to a longer logical subscript.
        if index.r_type() == SexpType::Logical && handles::length(context, index) > extent {
            return Err(call_error(call, "(subscript) logical subscript too long"));
        }
        let index = dimension_index(context, index)?;
        let (positions, _) = resolve_subscript(context, index, extent, names, false, call)?;
        if let Some((offset, &position)) = positions
            .iter()
            .enumerate()
            .find(|&(_, &position)| position >= extent && position != NA_POSITION)
        {
            let failure = DimensionBounds {
                object,
                dimension,
                index,
                offset,
                position,
            };
            return Err(dimension_out_of_bounds(context, call, failure));
        }
        selections.push(positions);
    }
    let offsets = ArrayOffsets::new(&selections, dims);
    let count = offsets.remaining;
    let selected = select_iter(context, object, offsets, count)?;
    let mut result = owned(context, selected)?;
    let new_dims: SmallVec<[usize; 4]> =
        selections.iter().map(|selection| selection.len()).collect();
    let mut new_dimnames: SmallVec<[Local<'s>; 4]> = SmallVec::with_capacity(dims.len());
    for (dimension, selection) in selections.iter().enumerate() {
        let names = component(context, dimension);
        // An empty selection has no names: GNU R stores `NULL` for an empty component.
        new_dimnames.push(
            match names.r_type() == SexpType::Nil || selection.is_empty() {
                true => null(),
                false => select(context, names, selection)?,
            },
        );
    }
    let dimnames_names = match dimnames {
        Some(dimnames) => names_of(context, dimnames.value())?,
        None => null(),
    };
    let kept: SmallVec<[usize; 4]> = (0..new_dims.len())
        .filter(|&dimension| !drop || new_dims[dimension] != 1)
        .collect();
    let is_null = |value: &Local<'_>| value.r_type() == SexpType::Nil;
    if kept.len() <= 1 && drop {
        let names = match kept.as_slice() {
            &[dimension] => new_dimnames[dimension],
            _ => {
                let mut named = new_dimnames.iter().filter(|names| !is_null(names));
                match (named.next(), named.next()) {
                    (Some(&names), None) => names,
                    _ => null(),
                }
            }
        };
        if !is_null(&names) {
            set_attribute(context, &mut result, symbol(|symbols| symbols.names), names)?;
        }
        return Ok(result.finish());
    }
    let dim = context.alloc_unfilled::<Integer>(kept.len())?;
    let dim = dim.fill(kept.iter().map(|&dimension| new_dims[dimension] as i32));
    let mut dim = dim.owned().value();
    let old_dim = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dim),
    ));
    let dim_names = names_of(context, old_dim)?;
    if !is_null(&dim_names) {
        let kept_names = select(context, dim_names, &kept)?;
        set_attribute(
            context,
            &mut dim,
            symbol(|symbols| symbols.names),
            kept_names,
        )?;
    }
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dim.finish(),
    )?;
    // GNU R keeps dimnames whose components are all NULL unless a dropped dimension forces it
    // to rebuild them.
    let undropped = dimnames.is_some() && kept.len() == new_dims.len();
    if undropped
        || kept
            .iter()
            .any(|&dimension| !is_null(&new_dimnames[dimension]))
        || !is_null(&dimnames_names)
    {
        let mut list = context.alloc::<List>(kept.len())?;
        for (slot, &dimension) in kept.iter().enumerate() {
            list.set(context, slot, new_dimnames[dimension]);
        }
        let mut list = list.owned().value();
        if !is_null(&dimnames_names) {
            let kept_names = select(context, dimnames_names, &kept)?;
            set_attribute(
                context,
                &mut list,
                symbol(|symbols| symbols.names),
                kept_names,
            )?;
        }
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.dimnames),
            list.finish(),
        )?;
    }
    Ok(result.finish())
}

/// Row `row` of a logical, integer or double matrix with `rows` rows and `columns` columns,
/// one element per column, or `None` for another type.
fn matrix_row<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    row: usize,
    rows: usize,
    columns: usize,
) -> R<Option<Local<'s>>> {
    fn strided<'s, T: Atomic>(
        context: &mut Cx<'s>,
        matrix: Local<'s, T>,
        row: usize,
        rows: usize,
        columns: usize,
    ) -> R<Option<Local<'s>>> {
        let result = context.alloc_unfilled::<T>(columns)?;
        let elements = matrix.as_slice(context).iter().skip(row).step_by(rows);
        Ok(Some(result.fill(elements.copied()).finish().value()))
    }
    match object.r_type() {
        SexpType::Real => strided(context, standard::<Real>(object)?, row, rows, columns),
        SexpType::Integer => strided(context, standard::<Integer>(object)?, row, rows, columns),
        SexpType::Logical => strided(context, standard::<Logical>(object)?, row, rows, columns),
        _ => Ok(None),
    }
}

/// The `length` elements of a logical, integer or double vector from `start` on, or `None` for
/// another type.
fn matrix_column<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    start: usize,
    length: usize,
) -> R<Option<Local<'s>>> {
    fn run<'s, T: Atomic>(
        context: &mut Cx<'s>,
        vector: Local<'s, T>,
        start: usize,
        length: usize,
    ) -> R<Option<Local<'s>>> {
        let result = context.alloc_unfilled::<T>(length)?;
        let elements = &vector.as_slice(context)[start..start + length];
        Ok(Some(result.fill(elements.iter().copied()).finish().value()))
    }
    match object.r_type() {
        SexpType::Real => run(context, standard::<Real>(object)?, start, length),
        SexpType::Integer => run(context, standard::<Integer>(object)?, start, length),
        SexpType::Logical => run(context, standard::<Logical>(object)?, start, length),
        _ => Ok(None),
    }
}

/// `slice` with `names` as its names when they are not `NULL`. The names vector may carry
/// names of its own, which the result's names drop.
fn named_slice<'s>(context: &mut Cx<'s>, slice: Local<'s>, names: Local<'s>) -> R<Local<'s>> {
    if names.r_type() == SexpType::Nil {
        return Ok(slice);
    }
    let mut slice = owned(context, slice)?;
    let mut names = handles::attribute_copy(context, names)?;
    clear_attributes(context, &mut names, false);
    set_attribute(
        context,
        &mut slice,
        symbol(|symbols| symbols.names),
        names.finish(),
    )?;
    Ok(slice.finish())
}

/// The zero-based position a scalar subscript selects along a matrix dimension of `extent`,
/// when it is a plain positive in-range number; anything else takes the general path, which
/// also reports errors.
#[inline(always)]
fn matrix_index(context: &Cx<'_>, index: Local<'_>, extent: usize) -> Option<usize> {
    if has_attributes(context, index) {
        return None;
    }
    let position = match (index.downcast::<Integer>(), index.downcast::<Real>()) {
        (Some(index), _) => match index.as_slice(context) {
            &[position] if position != NA_INTEGER => f64::from(position),
            _ => return None,
        },
        (_, Some(index)) => match index.as_slice(context) {
            &[position] => position,
            _ => return None,
        },
        _ => return None,
    };
    // NaN fails both comparisons, and `m[2.7, ]` truncates to the second row.
    (position >= 1.0 && position < extent as f64 + 1.0).then(|| position as usize - 1)
}

/// [`matrix_index`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
unsafe fn matrix_position(index: Sexp, extent: usize) -> Option<usize> {
    let position = handles::with(|context| {
        let index = handles::held(context, index);
        Ok(matrix_index(context, index, extent))
    });
    position.ok().flatten()
}

/// The context GNU R's `subscriptOutOfBoundsError` carries: the object being indexed, the
/// dimension a matrix subscript belongs to, and the subscript to report when it is not the one
/// that failed, such as the whole vector of a recursive `[[`.
#[derive(Clone, Copy)]
struct OutOfBounds<'s> {
    object: Local<'s>,
    dimension: Option<usize>,
    reported: Option<Local<'s>>,
}

/// An out-of-bounds subscript, signalled as GNU R's `subscriptOutOfBoundsError` with the
/// object, the 1-based dimension (`NA` for none) and the subscript, so handlers can tell it apart
/// from other errors.
fn bounds_error<'s>(
    context: &mut Cx<'s>,
    call: Option<Local<'s>>,
    message: &str,
    bounds: OutOfBounds<'s>,
    index: Local<'s>,
) -> Unwind {
    let dimension = bounds
        .dimension
        .map_or(NA_INTEGER, |dimension| dimension as i32 + 1);
    let subscript = match scalar::<Integer>(context, dimension) {
        Ok(subscript) => subscript,
        Err(failure) => return failure,
    };
    handles::classed_error(
        context,
        call,
        message,
        &["subscriptOutOfBoundsError", "error", "condition"],
        &[
            ("object", bounds.object),
            ("subscript", subscript),
            ("index", bounds.reported.unwrap_or(index)),
        ],
    )
}

/// An out-of-bounds array subscript in `[` or `[<-`: element `offset` of the subscript `index`
/// of dimension `dimension` selected `position`.
struct DimensionBounds<'s> {
    object: Local<'s>,
    dimension: usize,
    index: Local<'s>,
    offset: usize,
    position: usize,
}

/// The error for an out-of-bounds array subscript. GNU R reports the failing element: a name
/// with its dimension, or a position without one.
fn dimension_out_of_bounds<'s>(
    context: &mut Cx<'s>,
    call: Option<Local<'s>>,
    failure: DimensionBounds<'s>,
) -> Unwind {
    let index = failure.index;
    let named =
        index.r_type() == SexpType::String && failure.offset < handles::length(context, index);
    let reported = match named {
        true => element(context, index, failure.offset),
        false => scalar::<Real>(context, failure.position as f64 + 1.0),
    };
    let reported = match reported {
        Ok(reported) => reported,
        Err(failure) => return failure,
    };
    let bounds = OutOfBounds {
        object: failure.object,
        dimension: named.then_some(failure.dimension),
        reported: None,
    };
    bounds_error(context, call, "subscript out of bounds", bounds, reported)
}

/// The elements a matrix subscript selects from an array with `dims`: one row per element and one
/// column per dimension, holding indices or (for a character matrix) dimnames. `None` when
/// `index` is not such a matrix. A row with an `NA` selects `NA`, and a numeric row with a 0
/// selects nothing.
pub(crate) fn matrix_positions<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    dims: &[usize],
    index: Local<'s>,
    call: Option<Local<'s>>,
) -> R<Option<Vec<Option<usize>>>> {
    let Some(index_dims) = handles::dims(context, index) else {
        return Ok(None);
    };
    let numeric = matches!(index.r_type(), SexpType::Integer | SexpType::Real);
    if dims.len() < 2
        || index_dims.len() != 2
        || index_dims[1] != dims.len()
        || !(numeric || index.r_type() == SexpType::String)
    {
        return Ok(None);
    }
    let dimnames = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    ));
    if !numeric && dimnames.r_type() == SexpType::Nil {
        return Err(call_error(call, "no 'dimnames' attribute for array"));
    }
    let index = materialize(context, index)?;
    let rows = index_dims[0];
    let mut positions = Vec::with_capacity(rows);
    let failure = {
        let numbers = match numeric {
            true => Some(Numbers::of(context, index)?),
            false => None,
        };
        let wanted = match numeric {
            true => None,
            false => Some(standard::<Str>(index)?.read(context)),
        };
        let dimnames = dimnames
            .downcast::<List>()
            .map(|dimnames| dimnames.read(context));
        let mut failure = None;
        'rows: for row in 0..rows {
            let mut offset = 0usize;
            let mut stride = 1usize;
            for (dimension, &extent) in dims.iter().enumerate() {
                let cell = row + dimension * rows;
                let position = if let Some(numbers) = numbers {
                    let value = numbers.at(cell).trunc();
                    if value.is_nan() {
                        positions.push(None);
                        continue 'rows;
                    }
                    if value < 0.0 {
                        failure = Some(MatrixFailure::Negative);
                        break 'rows;
                    }
                    // GNU R drops a row that names position 0 in the first dimension it reaches,
                    // the way a 0 in a vector subscript selects nothing.
                    if value == 0.0 {
                        continue 'rows;
                    }
                    if value as usize > extent {
                        failure = Some(MatrixFailure::Beyond { dimension, value });
                        break 'rows;
                    }
                    value as usize - 1
                } else {
                    let name = wanted.map(|wanted| wanted.get(cell));
                    let Some(name) = name.filter(|&name| !is_na(name)) else {
                        positions.push(None);
                        continue 'rows;
                    };
                    let names = dimnames
                        .filter(|dimnames| dimension < dimnames.len())
                        .and_then(|dimnames| dimnames.get(dimension).downcast::<Str>());
                    let found = names.and_then(|names| {
                        (0..names.len()).find(|&candidate| {
                            let candidate = names.get(candidate);
                            !is_na(candidate) && same_text(candidate, name)
                        })
                    });
                    match found {
                        Some(found) => found,
                        None => {
                            failure = Some(MatrixFailure::Unnamed { dimension, cell });
                            break 'rows;
                        }
                    }
                };
                offset += position * stride;
                stride *= extent;
            }
            positions.push(Some(offset));
        }
        failure
    };
    let bounds = |dimension| OutOfBounds {
        object,
        dimension: Some(dimension),
        reported: None,
    };
    match failure {
        None => Ok(Some(positions)),
        Some(MatrixFailure::Negative) => Err(call_error(
            call,
            "negative values are not allowed in a matrix subscript",
        )),
        Some(MatrixFailure::Beyond { dimension, value }) => {
            let reported = scalar::<Real>(context, value)?;
            let message = "subscript out of bounds";
            Err(bounds_error(
                context,
                call,
                message,
                bounds(dimension),
                reported,
            ))
        }
        Some(MatrixFailure::Unnamed { dimension, cell }) => {
            let reported = element(context, index, cell)?;
            let message = "subscript out of bounds";
            Err(bounds_error(
                context,
                call,
                message,
                bounds(dimension),
                reported,
            ))
        }
    }
}

/// Why a matrix subscript selects nothing valid, found while its rows are read and reported
/// once the reading ends.
enum MatrixFailure {
    Negative,
    Beyond { dimension: usize, value: f64 },
    Unnamed { dimension: usize, cell: usize },
}

/// [`matrix_positions`] for raw callers, which pass the unbound marker for the running call.
///
/// # Safety
///
/// As for [`vector_element`].
unsafe fn matrix_index_positions(
    object: Sexp,
    dims: &[usize],
    index: Sexp,
    call: Sexp,
) -> R<Option<Vec<Option<usize>>>> {
    handles::with(|context| {
        let object = handles::adopt(context, object);
        let index = handles::adopt(context, index);
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        matrix_positions(context, object, dims, index, call)
    })
}

/// `[<-` for `m[i, ] <- value` or `a[i, j, ] <- value` (see [`slab`]), called without
/// building the general call's arguments; any other shape answers `None` and takes the builtin.
pub(crate) unsafe fn direct_subassign(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let [object, indices @ .., value] = args else {
        return Ok(None);
    };
    if indices.len() < 2
        || object.is_object()
        || object.is_altrep()
        || object.is_array()
        || *value == *object
        || value.is_object()
        || value.is_altrep()
    {
        return Ok(None);
    }
    let Some(dims) = values::dims(*object) else {
        return Ok(None);
    };
    let Some(fixed) = slab(indices, &dims, *object, *value) else {
        return Ok(None);
    };
    let result = modifiable(*object)?;
    write_slab(result, *value, &dims, &fixed);
    Ok(Some(result))
}

/// For an assignment into a numeric array of extents `dims` whose every subscript is empty or
/// one in-range position, given a value of the same type whose length divides the selection:
/// the position each dimension fixes, or `None` for a dimension taken whole.
unsafe fn slab(
    indices: &[Sexp],
    dims: &[usize],
    result: Sexp,
    value: Sexp,
) -> Option<SmallVec<[Option<usize>; 4]>> {
    if indices.len() != dims.len()
        || value.kind() != result.kind()
        || !matches!(
            result.kind(),
            SexpType::Real | SexpType::Integer | SexpType::Logical
        )
    {
        return None;
    }
    let fixed = indices
        .iter()
        .zip(dims)
        .map(|(&index, &extent)| {
            if index == missing_arg() {
                Some(None)
            } else {
                matrix_position(index, extent).map(Some)
            }
        })
        .collect::<Option<SmallVec<[Option<usize>; 4]>>>()?;
    let selected: usize = fixed
        .iter()
        .zip(dims)
        .map(|(position, &extent)| if position.is_some() { 1 } else { extent })
        .product();
    let value_count = length(value);
    (value_count > 0 && selected > 0 && selected.is_multiple_of(value_count)).then_some(fixed)
}

/// Writes the slab [`slab`] found.
unsafe fn write_slab(result: Sexp, value: Sexp, dims: &[usize], fixed: &[Option<usize>]) {
    match result.kind() {
        SexpType::Real => {
            scatter_slab(values::reals_mut(result), values::reals(value), dims, fixed)
        }
        _ => scatter_slab(values::ints_mut(result), values::ints(value), dims, fixed),
    }
}

/// Writes `value`, recycled, over the elements of a column-major array with extents `dims`
/// that `fixed` selects: one position along each dimension that has one, and every position
/// along the others, the first dimension varying fastest.
fn scatter_slab<T: Copy>(array: &mut [T], value: &[T], dims: &[usize], fixed: &[Option<usize>]) {
    let mut offset = 0;
    let mut stride = 1;
    let mut free: SmallVec<[(usize, usize); 4]> = SmallVec::new();
    for (&extent, position) in dims.iter().zip(fixed) {
        match position {
            Some(position) => offset += position * stride,
            None => free.push((stride, extent)),
        }
        stride *= extent;
    }
    if value.is_empty() {
        return;
    }
    // A free first dimension makes the slab a sequence of contiguous runs (a whole column for
    // `m[, j] <- v`), each copied as a block from the recycled value.
    let (run, outer) = match free.first() {
        Some(&(1, extent)) => (extent, &free[1..]),
        _ => (1, &free[..]),
    };
    let mut counters: SmallVec<[usize; 4]> = smallvec![0; outer.len()];
    let mut source = 0;
    loop {
        let mut filled = 0;
        while filled < run {
            let chunk = (run - filled).min(value.len() - source);
            if let Some(target) = array.get_mut(offset + filled..offset + filled + chunk)
                && let Some(from) = value.get(source..source + chunk)
            {
                target.copy_from_slice(from);
            }
            filled += chunk;
            source += chunk;
            if source == value.len() {
                source = 0;
            }
        }
        let mut dimension = 0;
        loop {
            let Some(&(stride, extent)) = outer.get(dimension) else {
                return;
            };
            counters[dimension] += 1;
            offset += stride;
            if counters[dimension] < extent {
                break;
            }
            offset -= stride * extent;
            counters[dimension] = 0;
            dimension += 1;
        }
    }
}

/// The column-major offsets of every combination of `selections`, the first dimension varying
/// fastest, with `NA_POSITION` wherever a selected position is `NA`. They are produced one at a
/// time, so `x[, , k]` on a large array gathers without first writing a list of offsets as long
/// as the result.
///
/// The innermost run walks the first selection longer than one position, and the single
/// positions before it fold into the base, so `x[5, , k]` strides through a row without taking
/// an odometer step per element.
struct ArrayOffsets<'a> {
    selections: &'a [Positions],
    strides: SmallVec<[usize; 4]>,
    /// The position within each selection; only the dimensions after `lead` move.
    counters: SmallVec<[usize; 4]>,
    /// The offset the non-`NA` positions outside the innermost run contribute.
    base: usize,
    /// How many dimensions outside the innermost run sit on an `NA` position.
    missing: usize,
    /// The dimension of the innermost run.
    lead: usize,
    /// The selection of the innermost run and its stride.
    run: &'a [Position],
    run_stride: usize,
    /// The position within the innermost run.
    inner: usize,
    remaining: usize,
}

impl<'a> ArrayOffsets<'a> {
    fn new(selections: &'a [Positions], dims: &[usize]) -> ArrayOffsets<'a> {
        let mut strides = SmallVec::with_capacity(dims.len());
        let mut stride = 1usize;
        for &extent in dims {
            strides.push(stride);
            stride = stride.saturating_mul(extent);
        }
        let lead = selections
            .iter()
            .position(|selection| selection.len() != 1)
            .unwrap_or(selections.len().saturating_sub(1));
        let mut offsets = ArrayOffsets {
            selections,
            run_stride: strides.get(lead).copied().unwrap_or(0),
            strides,
            counters: smallvec![0; selections.len()],
            base: 0,
            missing: 0,
            lead,
            run: selections
                .get(lead)
                .map_or(&[], |selection| selection.as_slice()),
            inner: 0,
            remaining: selections.iter().map(|selection| selection.len()).product(),
        };
        if offsets.remaining > 0 {
            for dimension in (0..selections.len()).filter(|&dimension| dimension != lead) {
                offsets.enter(dimension, 0);
            }
        }
        offsets
    }

    /// Adds the contribution of position `counter` in the selection of `dimension` to the base.
    fn enter(&mut self, dimension: usize, counter: usize) {
        match self.contribution(dimension, counter) {
            Some(part) => self.base += part,
            None => self.missing += 1,
        }
    }

    fn leave(&mut self, dimension: usize, counter: usize) {
        match self.contribution(dimension, counter) {
            Some(part) => self.base -= part,
            None => self.missing -= 1,
        }
    }

    fn contribution(&self, dimension: usize, counter: usize) -> Option<usize> {
        let position = *self.selections.get(dimension)?.get(counter)?;
        let stride = *self.strides.get(dimension)?;
        (position != NA_POSITION).then(|| position * stride)
    }

    /// Starts the next run: an odometer step over the dimensions after the innermost run, where
    /// only the dimensions whose counter changes update the base, so advancing is amortized
    /// constant time.
    #[inline(never)]
    fn advance(&mut self) -> Option<()> {
        self.inner = 0;
        for dimension in self.lead + 1..self.selections.len() {
            let counter = *self.counters.get(dimension)?;
            self.leave(dimension, counter);
            let next = match counter + 1 < self.selections.get(dimension)?.len() {
                true => counter + 1,
                false => 0,
            };
            *self.counters.get_mut(dimension)? = next;
            self.enter(dimension, next);
            if next != 0 {
                break;
            }
        }
        Some(())
    }
}

impl Iterator for ArrayOffsets<'_> {
    type Item = usize;

    // Inlined so the gather loop keeps the iterator's state in registers; the odometer step
    // stays out of line because it runs once per run, not once per element.
    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.remaining == 0 {
            return None;
        }
        if self.inner == self.run.len() {
            self.advance()?;
        }
        let position = *self.run.get(self.inner)?;
        self.inner += 1;
        self.remaining -= 1;
        Some(match self.missing > 0 || position == NA_POSITION {
            true => NA_POSITION,
            false => self.base + position * self.run_stride,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

// `[[`.

/// [`dimension_out_of_bounds`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
unsafe fn array_out_of_bounds(
    call: Sexp,
    object: Sexp,
    dimension: usize,
    index: Sexp,
    offset: usize,
    position: usize,
) -> Unwind {
    let failure = handles::with(|context| {
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        let failure = DimensionBounds {
            object: handles::adopt(context, object),
            dimension,
            index: handles::adopt(context, index),
            offset,
            position,
        };
        Ok(dimension_out_of_bounds(context, call, failure))
    });
    match failure {
        Ok(failure) => {
            failure.root();
            failure
        }
        Err(failure) => failure,
    }
}

unsafe fn do_subset2(call: &Call<'_>) -> R<Sexp> {
    handles::with(|context| {
        let arguments = subset_args(context, call);
        let object = arguments.object;
        if let Some(position) = scalar_position(context, &arguments)
            && !handles::is_object(object)
        {
            match object.r_type() {
                SexpType::List | SexpType::Expression => {
                    return item(context, object, position).map(Local::raw);
                }
                kind if kind.is_atomic() => {
                    return element(context, object, position).map(Local::raw);
                }
                _ => {}
            }
        }
        if arguments.indices.is_empty() {
            return Err(call.error("no index specified"));
        }
        let exact = match arguments.exact {
            Some(exact) => match handles::logical_scalar(context, exact) {
                NA_LOGICAL => None,
                value => Some(value != 0),
            },
            None => Some(true),
        };
        let call = handles::held(context, call.call);
        subset2_value(
            context,
            object,
            &arguments.indices,
            exact,
            Some(call),
            None,
            None,
        )
        .map(Local::raw)
    })
}

/// GNU R's `R_getS4DataSlot`: the data part of an S4 object whose class extends a basic type,
/// held in `.Data` or, for an environment such as a reference class object, in `.xData`.
/// `NULL` when the class extends none.
pub(crate) unsafe fn s4_data_part(object: Sexp) -> Sexp {
    let data = get_attr(object, symbols().dot_data);
    if data != nil() {
        return data;
    }
    get_attr(object, symbols().dot_xdata)
}

/// The position `name` matches among `names`: exactly, or with `exact == Some(false)` (or
/// `None`, which also warns) by a unique partial match. Names compare as text, so a
/// latin1-marked name matches its UTF-8 spelling, and an `NA` name matches nothing. GNU R's
/// `get1index` agrees for `[[`, but its `$` matches an `NA` name as the text "NA" (`backlog.md`).
fn name_position(
    context: &mut Cx<'_>,
    names: Local<'_>,
    name: Local<'_, Chars>,
    exact: Option<bool>,
) -> R<Option<usize>> {
    let Some(names) = names.downcast::<Str>() else {
        return Ok(None);
    };
    let name = name.read(context);
    if is_na(name) {
        return Ok(None);
    }
    let candidates = names.read(context);
    if let Some(position) =
        (0..candidates.len()).find(|&index| same_text(candidates.get(index), name))
    {
        return Ok(Some(position));
    }
    if exact == Some(true) {
        return Ok(None);
    }
    let text = handles::text(name).into_owned();
    let mut found = None;
    for index in 0..candidates.len() {
        let candidate = candidates.get(index);
        if !is_na(candidate) && handles::text(candidate).starts_with(&*text) {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(index);
        }
    }
    if let Some(position) = found
        && exact.is_none()
    {
        let matched = handles::text(candidates.get(position)).into_owned();
        handles::warning(context, format!("partial match of '{text}' to '{matched}'"))?;
    }
    Ok(found)
}

/// [`name_position`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`], with `name` a `CHARSXP`.
unsafe fn match_name(names: Sexp, name: Sexp, exact: Option<bool>) -> R<Option<usize>> {
    handles::with(|context| {
        let names = handles::adopt(context, names);
        let name = handles::adopt(context, name);
        let name = standard::<Chars>(name)?;
        name_position(context, names, name, exact)
    })
}

/// `x[[...]]` without dispatch, for raw callers, which pass the unbound marker for the running
/// call.
///
/// # Safety
///
/// As for [`subset`].
pub unsafe fn subset2(object: Sexp, indices: &[Sexp], exact: Option<bool>, call: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let object = handles::adopt(context, object);
        let indices: SmallVec<[Local<'_>; 2]> = indices
            .iter()
            .map(|&index| handles::adopt(context, index))
            .collect();
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        subset2_value(context, object, &indices, exact, call, None, None).map(Local::raw)
    })
}

/// `[[` on one level. An out-of-bounds error reports `shown` as the object when set, as for a
/// pairlist read through a list copy, and `recursive` as the subscript when set, which is the
/// whole subscript of the recursive `[[` this level belongs to.
fn subset2_value<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    exact: Option<bool>,
    call: Option<Local<'s>>,
    shown: Option<Local<'s>>,
    recursive: Option<Local<'s>>,
) -> R<Local<'s>> {
    if object.r_type() == SexpType::Record {
        return handles::record_subset2(context, object, indices, call);
    }
    if handles::is_array(object) {
        return handles::array_subset2(context, object, indices, call);
    }
    if object.r_type() == SexpType::I64 {
        return Err(call_error(call, "an I64 is a number and cannot be indexed"));
    }
    // String, list and number elements read through the class's `Elt` method, so `x[[i]]` asks
    // for element `i` alone and leaves a compact sequence compact, as GNU R does; other vectors
    // are read as slices.
    let object = if matches!(
        object.r_type(),
        SexpType::String | SexpType::List | SexpType::Logical | SexpType::Integer | SexpType::Real
    ) {
        object
    } else {
        materialize(context, object)?
    };
    if object.r_type() == SexpType::Object && is_s4(object) {
        let data = s4_data(context, object);
        if data.r_type() == SexpType::Nil {
            return Err(call_error(call, "this S4 class is not subsettable"));
        }
        return subset2_value(context, data, indices, exact, call, None, None);
    }
    let is_nil = object.r_type() == SexpType::Nil;
    let subscript_count_fits = indices.len() <= 1 || {
        let dim = get_attribute(context, object, symbol(|symbols| symbols.dim));
        dim.downcast::<Integer>().map_or(0, |dim| dim.len()) == indices.len()
    };
    if object.r_type() != SexpType::Environment
        && subscript_count_fits
        && (indices.is_empty() || indices.iter().any(|&index| is_missing_argument(index)))
    {
        // GNU R attaches the object only when it is NULL.
        let object_field = [("object", null())];
        let fields: &[(&str, Local<'_>)] = if is_nil { &object_field } else { &[] };
        return Err(handles::classed_error(
            context,
            call,
            "missing subscript",
            &["MissingSubscriptError", "error", "condition"],
            fields,
        ));
    }
    if is_nil {
        return Ok(null());
    }
    if object.r_type() == SexpType::Environment {
        let name = indices
            .first()
            .and_then(|index| index.downcast::<Str>())
            .filter(|index| index.len() == 1);
        let Some(name) = name else {
            return Err(call_error(
                call,
                "wrong arguments for subsetting an environment",
            ));
        };
        let name = context.root(name.get(context, 0));
        return Ok(handles::frame_value(context, object, name)?.unwrap_or_else(null));
    }
    if matches!(object.r_type(), SexpType::Pairlist | SexpType::Language) {
        let list = pairlist_to_list(context, object)?;
        return subset2_value(context, list, indices, exact, call, Some(object), recursive);
    }
    if !object.r_type().is_vector() {
        return Err(not_subsettable_error(context, call, object));
    }
    if indices.len() >= 2 {
        let dims = handles::dims(context, object);
        let Some(dims) = dims.filter(|dims| dims.len() == indices.len()) else {
            return Err(call_error(call, "incorrect number of subscripts"));
        };
        let dimnames = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.dimnames),
        ));
        let dimnames = dimnames.downcast::<List>();
        let mut offset = 0usize;
        let mut stride = 1usize;
        for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
            let names = match dimnames {
                Some(dimnames) => context.root(dimnames.get(context, dimension)),
                None => null(),
            };
            let bounds = OutOfBounds {
                object: shown.unwrap_or(object),
                dimension: Some(dimension),
                reported: None,
            };
            let position = single_index(context, index, extent, names, exact, call, bounds)?;
            offset += position * stride;
            stride *= extent;
        }
        return element(context, object, offset);
    }
    let index = materialize(context, indices[0])?;
    let count = handles::length(context, index);
    let list = matches!(object.r_type(), SexpType::List | SexpType::Expression);
    if count > 1 && list {
        // Recursive indexing: each subscript goes one level deeper, through calls and
        // pairlists as well as lists, as GNU R's `isPairList` lets it.
        let mut current = object;
        for position in 0..count {
            let single = element(context, index, position)?;
            if !matches!(
                current.r_type(),
                SexpType::List | SexpType::Expression | SexpType::Pairlist | SexpType::Language
            ) && position + 1 < count
            {
                return Err(call_error(
                    call,
                    format!("recursive indexing failed at level {}\n", position + 1),
                ));
            }
            current = subset2_value(context, current, &[single], exact, call, None, Some(index))?;
        }
        return Ok(current);
    }
    if count > 1 {
        return Err(call_error(
            call,
            "attempt to select more than one element in vectorIndex",
        ));
    }
    if count == 0 {
        return Err(call_error(
            call,
            "attempt to select less than one element in get1index",
        ));
    }
    let names = names_of(context, object)?;
    if list && let Some(wanted) = index.downcast::<Str>() {
        let wanted = context.root(wanted.get(context, 0));
        // A list has no element of that name: `[[` gives `NULL`, unlike for atomic vectors.
        if wanted.bytes(context).is_empty()
            || name_position(context, names, wanted, exact)?.is_none()
        {
            return Ok(null());
        }
    }
    if list {
        // A list gives `NULL` for a missing or infinite subscript, where an atomic vector
        // reports it out of bounds; a large finite subscript is out of bounds for both.
        let absent = match (index.downcast::<Real>(), integers(context, index)) {
            (Some(index), _) => {
                let value = index.as_slice(context)[0];
                value.is_nan() || value == f64::INFINITY
            }
            (None, &[value]) => value == NA_INTEGER,
            _ => false,
        };
        if absent {
            return Ok(null());
        }
    }
    let bounds = OutOfBounds {
        object: shown.unwrap_or(object),
        dimension: None,
        reported: recursive,
    };
    let extent = handles::length(context, object);
    let position = single_index(context, index, extent, names, exact, call, bounds)?;
    element(context, object, position)
}

/// The position a `[[` subscript selects, with GNU R's errors.
fn single_index<'s>(
    context: &mut Cx<'s>,
    index: Local<'s>,
    extent: usize,
    names: Local<'s>,
    exact: Option<bool>,
    call: Option<Local<'s>>,
    bounds: OutOfBounds<'s>,
) -> R<usize> {
    let index = materialize(context, index)?;
    let count = handles::length(context, index);
    if count != 1 && index.r_type() != SexpType::Symbol {
        return Err(call_error(
            call,
            if count == 0 {
                "attempt to select less than one element in get1index"
            } else {
                "attempt to select more than one element in get1index"
            },
        ));
    }
    let out_of_bounds = |context: &mut Cx<'s>| {
        bounds_error(context, call, "subscript out of bounds", bounds, index)
    };
    // Before 4.6.2 a matrix subscript never counts from the end, so a negative one selects
    // nothing; 4.6.2 applies the vector rule to each extent (PR#19116).
    let matrix = bounds.dimension.is_some() && !crate::system::r_at_least_release(4, 6, 2);
    match index.r_type() {
        SexpType::String => {
            let wanted = context.root(standard::<Str>(index)?.get(context, 0));
            // An empty name means "no name", so it never selects an element.
            let found = match wanted.bytes(context).is_empty() {
                true => None,
                false => name_position(context, names, wanted, exact)?,
            };
            found.ok_or_else(|| out_of_bounds(context))
        }
        SexpType::Integer | SexpType::Logical => {
            let value = integers(context, index)[0];
            if value == NA_INTEGER {
                return Err(out_of_bounds(context));
            }
            if value < 0 && extent == 2 && value >= -2 && !matrix {
                return Ok(if value == -1 { 1 } else { 0 });
            }
            if value <= 0 {
                let amount = if value < 0 && extent > 1 && !matrix {
                    "more"
                } else {
                    "less"
                };
                return Err(call_error(
                    call,
                    format!("attempt to select {amount} than one element in integerOneIndex"),
                ));
            }
            if value as usize > extent {
                return Err(out_of_bounds(context));
            }
            Ok(value as usize - 1)
        }
        SexpType::Real => {
            let value = standard::<Real>(index)?.as_slice(context)[0];
            if value.is_nan() {
                return Err(out_of_bounds(context));
            }
            let value = value.trunc();
            if value < 0.0 {
                if extent == 2 && value >= -2.0 && !matrix {
                    return Ok(if value == -1.0 { 1 } else { 0 });
                }
                return Err(call_error(
                    call,
                    "invalid negative subscript in get1index <real>",
                ));
            }
            if value == 0.0 {
                return Err(call_error(
                    call,
                    "attempt to select less than one element in get1index <real>",
                ));
            }
            if value > extent as f64 {
                return Err(out_of_bounds(context));
            }
            Ok(value as usize - 1)
        }
        SexpType::Symbol => {
            let symbol = standard::<Symbol>(index)?;
            let name = context.root(symbol.read(context).name());
            let mut text = context.alloc::<Str>(1)?;
            text.set(context, 0, name);
            let text = text.finish().value();
            single_index(context, text, extent, names, exact, call, bounds)
        }
        kind => Err(call_error(
            call,
            format!("invalid subscript type '{}'", kind.name()),
        )),
    }
}

/// The context GNU R's `subscriptOutOfBoundsError` carries: the object being indexed, the
/// dimension a matrix subscript belongs to, and the subscript to report when it is not the one
/// that failed, such as the whole vector of a recursive `[[`.
#[derive(Clone, Copy)]
struct Bounds {
    object: Sexp,
    dimension: Option<usize>,
    reported: Option<Sexp>,
}

impl Bounds {
    fn of(object: Sexp) -> Bounds {
        Bounds {
            object,
            dimension: None,
            reported: None,
        }
    }
}

/// An out-of-bounds subscript, signalled as GNU R's `subscriptOutOfBoundsError` with the
/// object, the 1-based dimension (`NA` for none) and the subscript, so handlers can tell it apart
/// from other errors.
unsafe fn out_of_bounds(call: Sexp, message: &str, bounds: Bounds, index: Sexp) -> Unwind {
    let failure = handles::with(|context| {
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        let bounds = OutOfBounds {
            object: handles::adopt(context, bounds.object),
            dimension: bounds.dimension,
            reported: bounds
                .reported
                .map(|reported| handles::adopt(context, reported)),
        };
        let index = handles::adopt(context, index);
        Ok(bounds_error(context, call, message, bounds, index))
    });
    match failure {
        Ok(failure) => {
            failure.root();
            failure
        }
        Err(failure) => failure,
    }
}

// `$` and `@`.

/// The name argument of `$` and `@` as a `CHARSXP`, for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
pub(crate) unsafe fn name_argument(value: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let value = handles::adopt(context, value);
        argument_name(context, value).map(|name| name.raw())
    })
}

/// The name argument of `$` and `@`: a symbol or string, never evaluated.
fn argument_name<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Chars>> {
    // A promise comes from `NextMethod` and stands for the name the method received.
    let value = handles::force(context, value)?;
    if let Some(symbol) = value.downcast::<Symbol>() {
        return Ok(context.root(symbol.read(context).name()));
    }
    if value.r_type() == SexpType::String
        && let Some(text) = dense::<Str>(context, value)?
        && text.len() >= 1
    {
        return Ok(context.root(text.get(context, 0)));
    }
    if let Some(call) = value.downcast::<Language>() {
        let call = call.read(context);
        if call.head().raw() == symbol(|symbols| symbols.quote).raw() {
            let quoted = match call.rest().downcast::<Pairlist>() {
                Some(arguments) => context.root(arguments.head()),
                None => null(),
            };
            return argument_name(context, quoted);
        }
    }
    Err(error(format!(
        "invalid subscript type '{}'",
        value.r_type().name()
    )))
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
    handles::with(|context| {
        let environment = handles::call_environment(context, call)?;
        let subject = handles::held(context, call.arg(0));
        let object = handles::evaluate(context, subject, environment)?;
        let name = handles::held(context, call.arg(1));
        let name = argument_name(context, name)?;
        let call = handles::held(context, call.call);
        dollar_dispatch(context, object, name, call, environment).map(Local::raw)
    })
}

/// [`dollar_dispatch`] for raw callers. `name` is a `CHARSXP`.
///
/// # Safety
///
/// As for [`subset`], with `env` an environment.
pub unsafe fn dollar_value(object: Sexp, name: Sexp, call: Sexp, env: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let object = handles::adopt(context, object);
        let name = standard::<Chars>(handles::adopt(context, name))?;
        let call = handles::adopt(context, call);
        let environment = standard::<Environment>(handles::adopt(context, env))?;
        dollar_dispatch(context, object, name, call, environment).map(Local::raw)
    })
}

/// `object$name` once the subject is evaluated: an object dispatches with the name as a string.
fn dollar_dispatch<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    name: Local<'s, Chars>,
    call: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    if handles::is_object(object) {
        let mut text = context.alloc::<Str>(1)?;
        text.set(context, 0, name);
        let arguments = [object, text.finish().value()];
        let generic = symbol(|symbols| symbols.dollar);
        if let Some(value) =
            handles::dispatch_builtin(context, generic, call, &arguments, environment)?
        {
            return Ok(value);
        }
    }
    dollar_field(context, object, name, Some(call))
}

/// `object$name` without dispatch, for raw callers, which pass the unbound marker for the
/// running call. `name` is a `CHARSXP`.
///
/// # Safety
///
/// As for [`subset`].
pub unsafe fn dollar(object: Sexp, name: Sexp, call: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let object = handles::adopt(context, object);
        let name = standard::<Chars>(handles::adopt(context, name))?;
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        dollar_field(context, object, name, call).map(Local::raw)
    })
}

/// `object$name` without dispatch.
fn dollar_field<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    name: Local<'s, Chars>,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    if object.r_type() == SexpType::Record {
        return handles::record_dollar(context, object, name, call);
    }
    if object.r_type() == SexpType::I64 {
        let field = String::from_utf8_lossy(name.bytes(context)).into_owned();
        let message = if handles::is_array(object) {
            format!("a List<I64> has no field `{field}`")
        } else {
            format!("an I64 is a number and has no field `{field}`")
        };
        return Err(call_error(call, message));
    }
    let object = materialize(context, object)?;
    match object.r_type() {
        SexpType::List | SexpType::Expression => {
            let names = names_of(context, object)?;
            let Some(position) = name_position(context, names, name, Some(false))? else {
                return Ok(null());
            };
            let value = item(context, object, position)?;
            // A complex assignment through a partial name, `v$mi[[1]] <- 2`, assigns a new `mi`
            // element, so the `misc` element it read must not be modified in place.
            let exact = names
                .downcast::<Str>()
                .is_some_and(|names| names.get(context, position).bytes() == name.bytes(context));
            if !exact {
                handles::mark_shared(value);
            }
            Ok(value)
        }
        SexpType::Pairlist | SexpType::Language => {
            let list = pairlist_to_list(context, object)?;
            dollar_field(context, list, name, call)
        }
        SexpType::Environment => {
            Ok(handles::frame_value(context, object, name)?.unwrap_or_else(null))
        }
        SexpType::Nil => Ok(null()),
        SexpType::Object if is_s4(object) => {
            let data = get_attribute(context, object, symbol(|symbols| symbols.dot_xdata));
            let data = context.root(data);
            if data.r_type() == SexpType::Environment {
                return dollar_field(context, data, name, call);
            }
            Err(call_error(call, "$ operator not defined for this S4 class"))
        }
        kind if kind.is_atomic() => {
            Err(call_error(call, "$ operator is invalid for atomic vectors"))
        }
        _ => Err(not_subsettable_error(context, call, object)),
    }
}

/// [`slot_name`] for raw callers.
///
/// # Safety
///
/// As for [`vector_element`].
unsafe fn slot_symbol(value: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let value = handles::adopt(context, value);
        slot_name(context, value).map(|name| name.raw())
    })
}

/// The slot name argument of `@` and `@<-`: a symbol or a single string, read from the
/// expression of a promise.
fn slot_name<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Symbol>> {
    let value = match value.downcast::<Promise>() {
        Some(promise) => context.root(promise.read(context).code()),
        None => value,
    };
    if let Some(symbol) = value.downcast::<Symbol>() {
        return Ok(symbol);
    }
    if value.r_type() == SexpType::String
        && let Some(text) = dense::<Str>(context, value)?
        && text.len() == 1
    {
        return handles::install_element(context, text, 0);
    }
    Err(error("invalid type or length for slot name"))
}

/// `object@name`. Only S4 objects have slots: on any other object `@` reads `.Data` (the
/// object itself) or dispatches an S3 `@` method, and otherwise is an error.
unsafe fn do_at(call: &Call<'_>) -> R<Sexp> {
    handles::with(|context| {
        let environment = handles::call_environment(context, call)?;
        let subject = handles::held(context, call.arg(0));
        let object = handles::evaluate(context, subject, environment)?;
        let name = handles::held(context, call.arg(1));
        let slot = slot_name(context, name)?;
        let call_value = handles::held(context, call.call);
        // Since R 4.3 `@` is an internal generic for S3 objects, which is how S7 reads
        // properties.
        if handles::is_object(object) && !is_s4(object) {
            let name = context.root(slot.read(context).name());
            let mut text = context.alloc::<Str>(1)?;
            text.set(context, 0, name);
            let arguments = [object, text.finish().value()];
            let generic = symbol(|symbols| symbols.at);
            if let Some(value) =
                handles::dispatch_builtin(context, generic, call_value, &arguments, environment)?
            {
                return Ok(value.raw());
            }
        }
        if !is_s4(object) && slot.raw() != symbol(|symbols| symbols.dot_data).raw() {
            let class = handles::class_of(context, object)?;
            let class = handles::first_string(context, class).map(|class| class.into_owned());
            return Err(call.error(format!(
                "no applicable method for `@` applied to an object of class \"{}\"",
                class.unwrap_or_default()
            )));
        }
        get_slot(object.raw(), slot.raw(), call.call)
    })
}

/// `R_do_slot`: an S4 slot, which is an attribute, with `.Data` and `.S3Class` special. Raw
/// callers pass the unbound marker for the running call.
///
/// # Safety
///
/// As for [`subset`], with `symbol` a symbol.
pub unsafe fn get_slot(object: Sexp, symbol: Sexp, call: Sexp) -> R<Sexp> {
    if symbol == symbols().dot_data
        && let Some(function) = interp_methods_function("getDataPart")?
    {
        let call = values::call(function, &[object])?;
        return eval::eval(call, values::base_env());
    }
    handles::with_result(|context| {
        let object = handles::adopt(context, object);
        let slot = standard::<Symbol>(handles::adopt(context, symbol))?;
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        slot_value(context, object, slot, call).map(Local::raw)
    })
}

/// The slot `slot` of `object` once `.Data` is ruled out.
fn slot_value<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Local<'s, Symbol>,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let value = get_attribute(context, object, slot.read(context));
    if value.r_type() != SexpType::Nil {
        // A slot holding `NULL` is stored as a special symbol.
        let null_slot = value
            .downcast::<Symbol>()
            .is_some_and(|value| value.name().bytes() == b"\x01NULL\x01");
        return Ok(match null_slot {
            true => null(),
            false => context.root(value),
        });
    }
    if slot.raw() == symbol(|symbols| symbols.dot_s3_class).raw() {
        return handles::class_of(context, object);
    }
    let name = handles::text(slot.read(context).name()).into_owned();
    if name == "names" && object.r_type() != SexpType::Object {
        return names_of(context, object);
    }
    if handles::attribute_cells(context, object).any(|(tag, _)| tag.raw() == slot.raw()) {
        return Ok(null());
    }
    let class = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.class),
    ));
    let message = match handles::first_string(context, class) {
        Some(class) => format!("no slot of name \"{name}\" for this object of class \"{class}\""),
        None => format!(
            "cannot get a slot (\"{name}\") from an object of type \"{}\"",
            object.r_type().name()
        ),
    };
    Err(call_error(call, message))
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
    let object = eval::eval(call.arg(0), call.env()?)?;
    let symbol = slot_symbol(call.arg(1))?;
    let value = eval::eval(call.arg(2), call.env()?)?;
    if object.is_object() && !object.has_gp(gp::S4_OBJECT) {
        let args = [
            Arg {
                tag: nil(),
                value: object,
            },
            Arg {
                tag: nil(),
                value: values::scalar_string_charsxp(symbol.car())?,
            },
            Arg {
                tag: symbols().value,
                value,
            },
        ];
        if let Some(result) =
            crate::dispatch::dispatch_builtin(symbols().at_assign, call.call, &args, call.env()?)?
        {
            return Ok(result);
        }
    }

    check_at_assignment(object, symbol, value)?;
    set_slot(object, symbol, value)
}

/// `@<-` on evaluated arguments, for `object@name <- value` compiled to registers: the object,
/// the slot's name as a symbol or a string, and the value. An S3 object answers `None`, because
/// it may dispatch to a method, which the special looks up in its caller's environment.
pub(crate) unsafe fn direct_at_assign(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let &[object, name, value] = args else {
        return Ok(None);
    };
    if object.is_object() && !object.has_gp(gp::S4_OBJECT) {
        return Ok(None);
    }
    let symbol = slot_symbol(name)?;
    check_at_assignment(object, symbol, value)?;
    set_slot(object, symbol, value).map(Some)
}

/// The methods package's check of `object@symbol <- value` against the class definition, which
/// uses the implicit class of an object without a class attribute, so `x@a <- 1` on a list
/// fails.
unsafe fn check_at_assignment(object: Sexp, symbol: Sexp, value: Sexp) -> R<()> {
    let Some(check) = interp_methods_function("checkAtAssignment")? else {
        return Ok(());
    };
    let class = crate::dispatch::class_of(object)?;
    crate::heap().root(class);
    let name = values::scalar_string_charsxp(symbol.car())?;
    crate::heap().root(name);
    let value_class = crate::dispatch::class_of(value)?;
    crate::heap().root(value_class);
    let check_call = values::call(check, &[class, name, value_class])?;
    eval::eval(check_call, values::base_env())?;
    Ok(())
}

/// `R_do_slot_assign`.
pub unsafe fn set_slot(object: Sexp, symbol: Sexp, value: Sexp) -> R<Sexp> {
    assign_slot(modifiable(object)?, symbol, value)
}

/// Sets a slot of `object` itself, even when it is shared. The methods package's `slot<-`
/// relies on that: its default `initialize` method calls it as a plain function on an object
/// it has just copied, and discards the result.
pub unsafe fn assign_slot(object: Sexp, symbol: Sexp, value: Sexp) -> R<Sexp> {
    if symbol == symbols().dot_data
        && let Some(function) = interp_methods_function("setDataPart")?
    {
        let call = values::call(function, &[object, value])?;
        return eval::eval(call, values::base_env());
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
    // Every argument between the object and the value is a subscript, whatever its name: GNU R
    // gives `drop` and `exact` no meaning in an assignment.
    let Some((object, indices)) = rest.split_first() else {
        return subassign(nil(), &[], value_arg.value, call.call);
    };
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|arg| arg.value).collect();
    subassign(object.value, &indices, value_arg.value, call.call)
}

/// Whether `x[i] <- value` leaves the atomic vector `object` exactly as it is: the subscript
/// selects nothing and `value` needs no change of type. GNU R then returns `x` itself, so
/// `x[FALSE] <- 1` keeps an ALTREP vector, such as a wrapper, unexpanded.
unsafe fn selects_nothing(object: Sexp, indices: &[Sexp], value: Sexp) -> bool {
    let &[index] = indices else {
        return false;
    };
    let empty = match index.kind() {
        SexpType::Nil => true,
        // A longer logical index stretches the vector even when it selects nothing.
        SexpType::Logical => {
            length(index) <= length(object) && values::ints(index).iter().all(|&flag| flag == 0)
        }
        SexpType::Integer | SexpType::Real | SexpType::String => length(index) == 0,
        _ => false,
    };
    let atomic = |kind| {
        matches!(
            kind,
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::String
        )
    };
    let fits = value.kind() == object.kind()
        || (atomic(value.kind())
            && atomic(object.kind())
            && coerce::higher_type(object.kind(), value.kind()) == object.kind());
    empty && fits && !value.is_object() && index.attrib() == nil()
}

/// `x[i] <- value` and `x[i, j] <- value`.
pub unsafe fn subassign(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    // `s[o] <- s` writes into the vector it reads from, so the value must be a copy, as in GNU
    // R's `SubassignN`; Matrix reorders its results this way.
    let value = if value == object {
        let copy = values::shallow_duplicate(value)?;
        crate::heap().root(copy);
        copy
    } else {
        value
    };
    // A value an Array cannot hold, such as an NA, makes the result a plain R vector.
    let object = if object.is_array() {
        match crate::arrays::subassign(object, indices, value, call)? {
            Some(result) => return Ok(result),
            None => values::plain(object)?,
        }
    } else {
        object
    };
    if object.is_altrep() && selects_nothing(object, indices, value) {
        return Ok(object);
    }
    let object = values::materialize(object)?;
    let mut value = values::materialize(value)?;
    // A pairlist value assigns its elements into a list, as a list would, but has no atomic
    // counterpart to coerce to.
    // An S4 object that is not a vector goes in as its `as.vector()`, which dispatches, so a
    // class's own method decides what its elements are.
    if value.kind() == SexpType::Object && object.kind() != SexpType::List {
        let coerce = values::call(values::install("as.vector")?, &[eval::quote_value(value)?])?;
        value = eval::eval(coerce, values::base_env())?;
    } else if value.kind() == SexpType::Object {
        conditions::warning_call(call, "implicit list embedding of S4 objects is deprecated")?;
    }
    // GNU R assigns a call's elements, as it does a pairlist's, so `f[2] <- quote(g(h))` stores
    // the symbol `g`.
    let value = if matches!(value.kind(), SexpType::Pairlist | SexpType::Language) {
        if !matches!(
            object.kind(),
            SexpType::List
                | SexpType::Expression
                | SexpType::Pairlist
                | SexpType::Language
                | SexpType::Nil
        ) {
            return Err(incompatible_types(
                crate::coerce::type_name(object),
                crate::coerce::type_name(value),
                "subassignment type fix",
            ));
        }
        values::pairlist_to_list(value)?
    } else if matches!(object.kind(), SexpType::List | SexpType::Expression) {
        // GNU R coerces the value to a list first: a call becomes the list of its parts, and a
        // symbol cannot be coerced. R 4.6.1 makes that an error; earlier releases warn, and the
        // symbol becomes `NULL` and deletes the selected elements.
        match value.kind() {
            SexpType::Language => values::pairlist_to_list(value)?,
            SexpType::Symbol if crate::system::r_at_least_release(4, 6, 1) => {
                return Err(error("'symbol' object cannot be coerced to type 'list'"));
            }
            SexpType::Symbol => {
                conditions::warning_call(call, "'symbol' object cannot be coerced to type 'list'")?;
                nil()
            }
            _ => value,
        }
    } else {
        value
    };
    if object.kind() == SexpType::Pairlist || object.kind() == SexpType::Language {
        let list = values::pairlist_to_list(object)?;
        let result = subassign(list, indices, value, call)?;
        // A call stays a call with its attributes (a formula keeps its class and environment),
        // but a pairlist becomes a list, as in GNU R.
        return if object.kind() == SexpType::Language {
            let rebuilt = values::list_to_pairlist(result, SexpType::Language)?;
            values::copy_attributes(object, rebuilt)?;
            Ok(rebuilt)
        } else {
            Ok(result)
        };
    }
    // GNU R leaves `NULL` given an empty value alone whatever the index, so
    // `x <- NULL; x[1] <- character(0)` is not the "replacement has length zero" error. An
    // empty vector given `NULL` still stretches to a longer subscript, so
    // `x <- list(); x[FALSE] <- NULL` leaves `list(NULL)`. An empty vector given an empty
    // vector is handled below, where it may still be coerced.
    if object == nil() && (value == nil() || (value.kind().is_vector() && length(value) == 0)) {
        return Ok(object);
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
        return Err(not_subsettable(call, object));
    }
    // Deleting list elements.
    if value == nil()
        && matches!(object.kind(), SexpType::List | SexpType::Expression)
        && indices.len() <= 1
    {
        let index = indices.first().copied().unwrap_or(missing_arg());
        let object = if logical_stretch(index) > length(object) {
            grow(object, logical_stretch(index), false)?
        } else {
            object
        };
        let count = length(object);
        let names = values::names(object);
        let (positions, _) = resolve(index, count, names, true, call)?;
        let remove: std::collections::HashSet<usize> = positions
            .into_iter()
            .filter(|&position| position != NA_POSITION)
            .collect();
        // As with any assignment, the list first stretches to a longer logical subscript or past
        // its end, padded with NULL, so `x[5] <- NULL` on two elements leaves four.
        let stretch = if index.kind() == SexpType::Logical {
            length(index)
        } else {
            0
        };
        let needed = remove
            .iter()
            .map(|&position| position + 1)
            .max()
            .unwrap_or(0)
            .max(stretch);
        let object = if needed > count {
            grow(object, needed, false)?
        } else {
            object
        };
        let count = length(object);
        let names = values::names(object);
        let kept: Vec<usize> = (0..count)
            .filter(|position| !remove.contains(position))
            .collect();
        let result = select_elements(object, &kept)?;
        // Names first, as GNU R lists them after a deletion.
        if names != nil() {
            let new_names = select_elements(names, &kept)?;
            values::set_attr(result, symbols().names, new_names)?;
        }
        values::copy_most_attributes(object, result, false)?;
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
    let target_kind = subassign_type_fix(object.kind(), value_kind)?;
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
    // The subscripts are read against the object as it was: a vector that became a list has
    // lost its dimensions, as in GNU R, where the result of `m[1, 2] <- list(v)` is a plain list.
    if indices.len() >= 2 {
        let Some(dims) = values::dims(object).filter(|dims| dims.len() == indices.len()) else {
            return Err(error_in(
                call,
                if indices.len() == 2 {
                    "incorrect number of subscripts on matrix"
                } else {
                    "incorrect number of subscripts"
                },
            ));
        };
        if value != result
            && let Some(fixed) = slab(indices, &dims, result, value)
        {
            write_slab(result, value, &dims, &fixed);
            return Ok(result);
        }
        let dimnames = get_attr(object, symbols().dimnames);
        let mut selections: SmallVec<[Positions; 4]> = SmallVec::new();
        for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
            let names = if dimnames.kind() == SexpType::List {
                dimnames.element(dimension)
            } else {
                nil()
            };
            let index = dimension_subscript(index)?;
            let (positions, _) = resolve(index, extent, names, false, call)?;
            if let Some((offset, &position)) = positions
                .iter()
                .enumerate()
                .find(|&(_, &position)| position >= extent && position != NA_POSITION)
            {
                let call = crate::unwind::generated_call(call)?;
                return Err(array_out_of_bounds(
                    call, object, dimension, index, offset, position,
                ));
            }
            selections.push(positions);
        }
        let total: usize = selections.iter().map(|selection| selection.len()).product();
        // A single value skips the NA rows, and a longer one cannot say which of its elements
        // an NA row would have taken. GNU R's matrix assignment checks this before comparing
        // lengths, and its array assignment after.
        let has_na = value_count > 1
            && selections
                .iter()
                .any(|selection| selection.contains(&NA_POSITION));
        let na_error = || error_in(call, "NAs are not allowed in subscripted assignments");
        if has_na && dims.len() == 2 {
            return Err(na_error());
        }
        if total > 0 && value_count == 0 {
            return Err(error_in(call, "replacement has length zero"));
        }
        if total > 0 && !total.is_multiple_of(value_count) {
            return Err(error_in(
                call,
                "number of items to replace is not a multiple of replacement length",
            ));
        }
        if has_na {
            return Err(na_error());
        }
        if total == 0 {
            return Ok(result);
        }
        scatter(result, ArrayOffsets::new(&selections, &dims), value)?;
        return Ok(result);
    }
    let index = indices.first().copied().unwrap_or(missing_arg());
    let count = length(result);
    let names = values::names(result);
    // A matrix with one column per dimension assigns single elements.
    if let Some(dims) = values::dims(object)
        && let Some(positions) = matrix_index_positions(object, &dims, index, call)?
    {
        // As with a vector subscript, a single value skips the NA rows, and a longer one
        // cannot say which of its elements an NA row would have taken.
        if value_count > 1 && positions.iter().any(Option::is_none) {
            return Err(error_in(
                call,
                "NAs are not allowed in subscripted assignments",
            ));
        }
        let rows = positions.len();
        if rows > 0 && value_count == 0 {
            return Err(error_in(call, "replacement has length zero"));
        }
        if rows > 0 && !rows.is_multiple_of(value_count) {
            conditions::warning(
                "number of items to replace is not a multiple of replacement length",
            )?;
        }
        for (row, position) in positions.into_iter().enumerate() {
            if let Some(offset) = position {
                assign_element(result, offset, value, row % value_count.max(1))?;
            }
        }
        return Ok(result);
    }
    if scatter_plain(result, index, value, value_count)? {
        return Ok(result);
    }
    let (positions, new_names) = resolve(index, count, names, true, call)?;
    let positions: Vec<usize> = positions
        .into_iter()
        .filter(|&position| position != NA_POSITION)
        .collect();
    let stretch = logical_stretch(index);
    if positions.is_empty() && stretch <= count {
        return Ok(result);
    }
    if !positions.is_empty() && value_count == 0 {
        return Err(error_in(call, "replacement has length zero"));
    }
    // GNU R attributes this warning to the current call, not the `[<-` call: the assignment at
    // its own level, else the closure call, which for `NextMethod` is `NextMethod(.Generic)`.
    if !positions.is_empty() && !positions.len().is_multiple_of(value_count) {
        conditions::warning("number of items to replace is not a multiple of replacement length")?;
    }
    let needed = positions
        .iter()
        .copied()
        .max()
        .map_or(0, |max| max + 1)
        .max(stretch);
    if needed > count {
        result = grow(result, needed, false)?;
        // `grow` keeps the names, padded with blanks, in a vector with room to grow, so only
        // the appended names are written.
        if !new_names.is_empty() {
            let mut grown_names = values::names(result);
            if grown_names == nil() {
                grown_names = alloc_with_room(SexpType::String, needed)?;
                values::set_attr(result, symbols().names, grown_names)?;
            }
            for (offset, name) in new_names.iter().enumerate() {
                if let Some(name) = *name
                    && count + offset < needed
                {
                    values::set_elt(grown_names, count + offset, name);
                }
            }
        }
    }
    for (position, &target) in positions.iter().enumerate() {
        assign_element(result, target, value, position % value_count)?;
    }
    Ok(result)
}

/// `target[index] <- value` written straight from an empty subscript or an attribute-free
/// integer or double one whose positions all lie inside `target`, when `value` already has
/// `target`'s type and its length divides the number of positions. Returns false, having written
/// nothing, for every other shape, which the general path handles with GNU R's full rules.
unsafe fn scatter_plain(target: Sexp, index: Sexp, value: Sexp, value_count: usize) -> R<bool> {
    let kind = target.kind();
    if values::has_attributes(index)
        || value == target
        || value.kind() != kind
        || target.is_altrep()
        || value.is_altrep()
        || value_count == 0
        || !matches!(
            kind,
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::String
                | SexpType::List
        )
    {
        return Ok(false);
    }
    let count = length(target);
    if index == missing_arg() {
        return Ok(scatter_run(target, 0, count, value, value_count));
    }
    if let Some((run, first, increment)) = values::compact_sequence(index)
        && increment == 1.0
        && first >= 1.0
        && first.fract() == 0.0
        && first as usize - 1 + run <= count
    {
        return Ok(scatter_run(
            target,
            first as usize - 1,
            run,
            value,
            value_count,
        ));
    }
    if let Some((highest, offsets)) = sequence_offsets(index) {
        return Ok(highest < count && scatter_offsets(target, offsets, value, value_count));
    }
    let index = values::materialize(index)?;
    let offsets: Vec<usize> = match index.kind() {
        SexpType::Integer => {
            let positions = values::ints(index);
            if positions
                .iter()
                .any(|&position| position <= 0 || position as usize > count)
            {
                return Ok(false);
            }
            positions
                .iter()
                .map(|&position| position as usize - 1)
                .collect()
        }
        SexpType::Real => {
            let positions = values::reals(index);
            // NaN fails both comparisons, so an NA subscript also declines.
            if !positions
                .iter()
                .all(|&position| position >= 1.0 && position < count as f64 + 1.0)
            {
                return Ok(false);
            }
            positions
                .iter()
                .map(|&position| position as usize - 1)
                .collect()
        }
        _ => return Ok(false),
    };
    Ok(scatter_offsets(
        target,
        offsets.iter().copied(),
        value,
        value_count,
    ))
}

/// Writes `value`, recycled, to the `run` elements of `target` from `start`, which lie inside it,
/// a whole copy of `value` at a time. `value` has `target`'s type and is another vector. False,
/// having written nothing, unless `run` is a multiple of `value_count`.
unsafe fn scatter_run(
    target: Sexp,
    start: usize,
    run: usize,
    value: Sexp,
    value_count: usize,
) -> bool {
    if !run.is_multiple_of(value_count) {
        return false;
    }
    if run == 0 {
        return true;
    }
    match target.kind() {
        SexpType::Real => fill_run(
            &mut values::reals_mut(target)[start..start + run],
            values::reals(value),
        ),
        SexpType::Logical | SexpType::Integer => fill_run(
            &mut values::ints_mut(target)[start..start + run],
            values::ints(value),
        ),
        _ => {
            // The written part is copied onto the part after it, so a short value costs a
            // barrier decision per doubling, not per copy.
            let heap = crate::heap();
            heap.copy_elements(target, start, value, 0, value_count);
            let mut filled = value_count;
            while filled < run {
                let step = filled.min(run - filled);
                heap.copy_elements(target, start + filled, target, start, step);
                filled += step;
            }
        }
    }
    true
}

/// Fills `target`, whose length is a multiple of `source`'s, with copies of `source`.
fn fill_run<T: Copy>(target: &mut [T], source: &[T]) {
    if let [single] = source {
        target.fill(*single);
        return;
    }
    let Some(first) = target.get_mut(..source.len()) else {
        return;
    };
    first.copy_from_slice(source);
    let mut filled = source.len();
    while filled < target.len() {
        let step = filled.min(target.len() - filled);
        target.copy_within(..step, filled);
        filled += step;
    }
}

/// Writes `value`, recycled, to `offsets` of `target`, which share its type and lie inside it.
/// False, having written nothing, unless the number of offsets is a multiple of `value_count`.
unsafe fn scatter_offsets(
    target: Sexp,
    offsets: impl ExactSizeIterator<Item = usize>,
    value: Sexp,
    value_count: usize,
) -> bool {
    if !offsets.len().is_multiple_of(value_count) {
        return false;
    }
    match target.kind() {
        SexpType::Real => recycle_into(values::reals_mut(target), offsets, values::reals(value)),
        SexpType::Logical | SexpType::Integer => {
            recycle_into(values::ints_mut(target), offsets, values::ints(value))
        }
        _ => {
            let source = value.slice::<Sexp>();
            let heap = crate::heap();
            for (position, offset) in offsets.enumerate() {
                heap.set_element(target, offset, source[position % value_count]);
            }
        }
    }
    true
}

/// A logical subscript longer than the vector stretches it on assignment to the subscript's
/// length, whatever the subscript selects, as GNU R does.
unsafe fn logical_stretch(index: Sexp) -> usize {
    if index.kind() == SexpType::Logical {
        length(index)
    } else {
        0
    }
}

/// `vector` extended to `new_length`, padded with NA, keeping its attributes but not its dims.
///
/// A copy reserves room past `new_length`, by GNU R's factor of 1.05, and an unshared vector with
/// room grows in place, so a loop that appends one element at a time runs in linear time.
///
/// `final_step` says that nothing after the growth can fail, so an unshared list's elements may
/// move into the copy instead of being counted into it and back out of the original when the
/// collector sweeps it. An error after a move would leave the variable holding an empty list.
unsafe fn grow(vector: Sexp, new_length: usize, final_step: bool) -> R<Sexp> {
    let count = length(vector);
    if grows_in_place(vector, new_length) {
        let names = values::names(vector);
        if names == nil() || grows_in_place(names, new_length) {
            crate::heap().resize_vector(vector, new_length as isize)?;
            for position in count..new_length {
                set_na(vector, position);
            }
            if names != nil() {
                crate::heap().resize_vector(names, new_length as isize)?;
            }
            return Ok(vector);
        }
    }
    let result = alloc_with_room(vector.kind(), new_length)?;
    if final_step
        && matches!(vector.kind(), SexpType::List | SexpType::Expression)
        && !vector.is_altrep()
        && !values::maybe_shared(vector)
    {
        crate::heap().move_elements(result, vector);
    } else {
        copy_prefix(values::materialize(vector)?, result, count)?;
    }
    for position in count..new_length {
        set_na(result, position);
    }
    // Names go first and the other attributes follow in order, as GNU R's `EnlargeVector`
    // lays them out; code such as the globals package checks the attribute order.
    let names = values::names(vector);
    if names != nil() {
        let grown = alloc_with_room(SexpType::String, new_length)?;
        let names = values::materialize(names)?;
        crate::heap().copy_elements(grown, 0, names, 0, count);
        values::set_attr(result, symbols().names, grown)?;
    }
    let mut cell = vector.attrib();
    while cell.kind() == SexpType::Pairlist {
        let tag = cell.tag();
        if tag != symbols().dim && tag != symbols().dimnames && tag != symbols().names {
            values::set_attr(result, tag, cell.car())?;
        }
        cell = cell.cdr();
    }
    Ok(result)
}

/// `vector` with `value` appended as a new last element named `name`, a CHARSXP: `x$name <- value`
/// or `x[["name"]] <- value` for a name `x` lacks. `vector` is an unshared list or atomic vector
/// whose only attribute is `names`, and `value` a list element or a scalar of `vector`'s type. An
/// unnamed `vector` gets blank names before the new one. The vector grows in place when it has
/// room, as [`grow`] does, and otherwise the result is a copy with room to grow.
pub(crate) unsafe fn append_named(vector: Sexp, name: Sexp, value: Sexp, reuse: bool) -> R<Sexp> {
    let heap = crate::heap();
    let count = length(vector);
    let names = values::names(vector);
    let result = if reuse
        && grows_in_place(vector, count + 1)
        && (names == nil() || grows_in_place(names, count + 1))
    {
        heap.resize_vector(vector, count as isize + 1)?;
        vector
    } else {
        let result = alloc_with_room(vector.kind(), count + 1)?;
        copy_prefix(vector, result, count)?;
        result
    };
    let names = if names == nil() || result != vector {
        let grown = alloc_with_room(SexpType::String, count + 1)?;
        if names != nil() {
            heap.copy_elements(grown, 0, names, 0, count);
        }
        values::set_attr(result, symbols().names, grown)?;
        grown
    } else {
        heap.resize_vector(names, count as isize + 1)?;
        names
    };
    heap.set_element(names, count, name);
    match result.kind() {
        SexpType::Real => values::reals_mut(result)[count] = value.slice::<f64>()[0],
        SexpType::Integer | SexpType::Logical => {
            values::ints_mut(result)[count] = value.slice::<i32>()[0]
        }
        SexpType::String => heap.set_element(result, count, value.element(0)),
        _ => heap.set_element(result, count, value),
    }
    Ok(result)
}

/// Whether `vector` can take `new_length` elements without a copy: it is a resizable vector
/// with that much room that nothing else references, and it has no dims for growth to drop.
unsafe fn grows_in_place(vector: Sexp, new_length: usize) -> bool {
    crate::heap().is_resizable(vector)
        && !vector.is_altrep()
        && new_length as isize <= vector.capacity()
        && !values::maybe_shared(vector)
        && values::get_attr(vector, symbols().dim) == nil()
        && values::get_attr(vector, symbols().dimnames) == nil()
}

/// A copy of `vector`, which has no attributes, one element longer with the new element unset
/// and room to grow, which the caller binds. The caller grows a vector in place where it has
/// room. Nothing after this may fail, because an unshared list's elements move into the copy
/// (see [`grow`]).
#[cold]
pub(crate) unsafe fn append_copy(vector: Sexp, reuse: bool) -> R<Sexp> {
    let count = length(vector);
    let heap = crate::heap();
    let result = alloc_with_room(vector.kind(), count + 1)?;
    if reuse
        && matches!(vector.kind(), SexpType::List | SexpType::Expression)
        && !vector.is_altrep()
        && !values::maybe_shared(vector)
    {
        heap.move_elements(result, vector);
    } else {
        copy_prefix(values::materialize(vector)?, result, count)?;
    }
    Ok(result)
}

/// A resizable vector of `length` elements with room to grow by GNU R's factor of 1.05.
unsafe fn alloc_with_room(kind: SexpType, length: usize) -> R<Sexp> {
    let capacity = length + length / 20;
    let heap = crate::heap();
    let vector = heap.alloc_resizable_vector(kind, capacity as isize)?;
    heap.root(vector);
    heap.resize_vector(vector, length as isize)?;
    Ok(vector)
}

/// `target[offsets] <- value`, recycling `value`, which is not empty, and skipping NA offsets.
unsafe fn scatter(target: Sexp, offsets: impl Iterator<Item = usize>, value: Sexp) -> R<()> {
    let value = values::materialize(value)?;
    let count = length(value);
    if value != target && value.kind() == target.kind() {
        match target.kind() {
            SexpType::Real => {
                recycle_into(values::reals_mut(target), offsets, values::reals(value));
                return Ok(());
            }
            SexpType::Integer | SexpType::Logical => {
                recycle_into(values::ints_mut(target), offsets, values::ints(value));
                return Ok(());
            }
            _ => {}
        }
    }
    for (position, offset) in offsets.enumerate() {
        if offset != NA_POSITION {
            assign_element(target, offset, value, position % count)?;
        }
    }
    Ok(())
}

/// Writes `source`, recycled, to `offsets` of `target`, skipping the NA offsets.
fn recycle_into<T: Copy>(target: &mut [T], offsets: impl Iterator<Item = usize>, source: &[T]) {
    for (offset, &item) in offsets.zip(source.iter().cycle()) {
        if let Some(slot) = target.get_mut(offset) {
            *slot = item;
        }
    }
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
    let Some((object, indices)) = rest.split_first() else {
        return subassign2(nil(), &[], value_arg.value, call.call);
    };
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|arg| arg.value).collect();
    subassign2(object.value, &indices, value_arg.value, call.call)
}

/// `x[[i]] <- value`.
pub unsafe fn subassign2(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    // `x[[i]] <- x` stores the old `x`, which is also the assignment's value, so the new `x`
    // is a copy: modified in place, the list would contain itself.
    let object = if value == object && object.kind() != SexpType::Environment {
        values::shallow_duplicate(object)?
    } else {
        object
    };
    if object.kind() == SexpType::Record {
        return crate::records::subassign2(object, indices, value, call);
    }
    let object = if object.is_array() {
        match crate::arrays::subassign2(object, indices, value, call)? {
            Some(result) => return Ok(result),
            None => values::plain(object)?,
        }
    } else {
        object
    };
    let object = values::materialize(object)?;
    // A reference class object assigns into its environment and stays itself.
    if object.kind() == SexpType::Object && object.has_gp(gp::S4_OBJECT) {
        let data = s4_data_part(object);
        if data.kind() != SexpType::Environment {
            return Err(error_in(
                call,
                "[[<- defined for objects of type \"S4\" only for subclasses of environment",
            ));
        }
        subassign2(data, indices, value, call)?;
        return Ok(object);
    }
    if object.kind() == SexpType::Environment {
        crate::moves::reaching_environment()?;
        let index = indices.first().copied().unwrap_or(nil());
        if index.kind() != SexpType::String || length(index) != 1 {
            return Err(error_in(call, "wrong args for environment subassignment"));
        }
        let symbol = values::install_charsxp(index.element(0))?;
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
    if object == nil() && value == nil() {
        return Ok(nil());
    }
    if let [index] = indices {
        return subassign2_one(object, *index, value, call, 1);
    }
    let Some(dims) = values::dims(object).filter(|dims| dims.len() == indices.len()) else {
        // GNU R counts the subscripts after it has checked what the object can hold.
        let is_vector = object == nil()
            || object.kind().is_atomic()
            || matches!(object.kind(), SexpType::List | SexpType::Expression);
        if !is_vector {
            return Err(not_subsettable(call, object));
        }
        if object.kind().is_atomic() {
            atomic_replacement_check(value)?;
        }
        return Err(error_in(call, "[[ ]] improper number of subscripts"));
    };
    // `m[[i, j]] <- value` names one cell, which must exist.
    let dimnames = get_attr(object, symbols().dimnames);
    for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
        let names = if dimnames.kind() == SexpType::List {
            dimnames.element(dimension)
        } else {
            nil()
        };
        // A subscript GNU R rejects for another reason is left to the assignment below.
        let inside = match one_index(extent, names, index) {
            Ok(Slot::At(position)) => position < extent,
            Ok(_) => false,
            Err(_) => true,
        };
        if !inside {
            let bounds = Bounds {
                dimension: Some(dimension),
                ..Bounds::of(object)
            };
            return Err(out_of_bounds(
                unbound(),
                "[[ ]] subscript out of bounds",
                bounds,
                index,
            ));
        }
    }
    let is_list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    if is_list || object == nil() || !value.kind().is_atomic() || length(value) != 1 {
        let list = if object == nil() {
            alloc(SexpType::List, 0)?
        } else if is_list {
            modifiable(object)?
        } else {
            // The assignment below reads the dims and then makes the object a list, which a
            // coercion here would leave without dims.
            atomic_replacement_check(value)?;
            object
        };
        let wrapped = values::list_vector(&[value])?;
        return subassign(list, indices, wrapped, call);
    }
    subassign(object, indices, value, call)
}

/// The type `x[i] <- value` leaves `x` as, with GNU R's errors for the pairs it refuses: raw
/// mixes only with raw, an atomic vector takes no function, symbol, call or environment, and a
/// list takes no function or environment.
unsafe fn subassign_type_fix(object: SexpType, value: SexpType) -> R<SexpType> {
    let atomic = |kind: SexpType| {
        matches!(
            kind,
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::String
        )
    };
    let refused = || incompatible_types(object.name(), value.name(), "subassignment type fix");
    match object {
        SexpType::List => match value {
            SexpType::Closure | SexpType::Builtin | SexpType::Special => Err(error(format!(
                "cannot coerce type '{}' to vector of type 'list'",
                value.name()
            ))),
            SexpType::Environment => Err(error("environments cannot be coerced to other types")),
            _ => Ok(SexpType::List),
        },
        SexpType::Expression => match value {
            SexpType::Raw
            | SexpType::Closure
            | SexpType::Builtin
            | SexpType::Special
            | SexpType::Environment => Err(refused()),
            _ => Ok(SexpType::Expression),
        },
        SexpType::Raw => match value {
            SexpType::Raw => Ok(SexpType::Raw),
            SexpType::List | SexpType::Expression => Ok(value),
            _ => Err(refused()),
        },
        _ if atomic(object) => match value {
            SexpType::List | SexpType::Expression => Ok(value),
            _ if atomic(value) => Ok(coerce::higher_type(object, value)),
            _ => Err(refused()),
        },
        _ => Ok(if value.is_vector() {
            coerce::higher_type(object, value)
        } else {
            SexpType::List
        }),
    }
}

/// GNU R's refusal to assign a `value` type into an `object` type. R 4.4 names the object's
/// type first; earlier releases name the value's first.
fn incompatible_types(object: &str, value: &str, context: &str) -> Unwind {
    let (from, to) = match crate::system::r_at_least(4, 4) {
        true => (object, value),
        false => (value, object),
    };
    error(format!(
        "incompatible types (from {from} to {to}) in {context}"
    ))
}

/// `x[[i]] <- value` with one subscript, which for a list may be a path of several (recursive
/// assignment); `level` is the depth of that path, which GNU R's error names.
unsafe fn subassign2_one(
    object: Sexp,
    index: Sexp,
    value: Sexp,
    call: Sexp,
    level: usize,
) -> R<Sexp> {
    if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
        return subassign2(object, &[index], value, call);
    }
    // A symbol subscript names the element, so the rest of the assignment sees it as a string.
    let index = if index.kind() == SexpType::Symbol {
        let name = values::scalar_string_charsxp(index.car())?;
        crate::heap().root(name);
        name
    } else {
        index
    };
    let is_list = object == nil() || matches!(object.kind(), SexpType::List | SexpType::Expression);
    let count = length(index);
    if count > 1 {
        if !is_list {
            return Err(errorcall(
                call,
                "attempt to select more than one element in vectorIndex",
            ));
        }
        let first = vector_element(index, 0)?;
        let rest_positions: Vec<usize> = (1..count).collect();
        let rest = select_elements(index, &rest_positions)?;
        let inner = match one_index(length(object), values::names(object), first)? {
            Slot::At(position) if position < length(object) => object.element(position),
            _ => {
                return Err(errorcall(call, format!("no such index at level {level}\n")));
            }
        };
        let updated = subassign2_one(inner, rest, value, call, level + 1)?;
        return subassign2_one(object, first, updated, call, level);
    }
    if !is_list {
        atomic_replacement_check(value)?;
    }
    let slot = one_index(length(object), values::names(object), index)?;
    if !is_list {
        let position = match slot {
            Slot::At(position) => values::scalar_real(position as f64 + 1.0)?,
            Slot::Named => index,
            Slot::Absent => {
                return Err(out_of_bounds(
                    unbound(),
                    "[[ ]] subscript out of bounds",
                    Bounds::of(object),
                    index,
                ));
            }
        };
        if !value.kind().is_atomic() {
            // A list or expression value turns the vector into one of its kind.
            let list = coerce_vector(object, value.kind())?;
            return subassign2_one(list, position, value, call, level);
        }
        return subassign(object, &[position], value, call);
    }
    if object.kind() == SexpType::Expression && value != nil() {
        if value.kind() == SexpType::List {
            return Err(incompatible_types("expression", "list", "[[ assignment"));
        }
        subassign_type_fix(SexpType::Expression, value.kind())?;
    }
    let list = if object == nil() {
        alloc(SexpType::List, 0)?
    } else {
        modifiable(object)?
    };
    let count = length(list);
    let position = match slot {
        Slot::At(position) => position,
        Slot::Named => count,
        Slot::Absent if value == nil() => return Ok(list),
        Slot::Absent => {
            return Err(out_of_bounds(
                unbound(),
                "[[ ]] subscript out of bounds",
                Bounds::of(object),
                index,
            ));
        }
    };
    if value == nil() {
        if position >= count {
            return Ok(list);
        }
        return subassign(
            list,
            &[values::scalar_real(position as f64 + 1.0)?],
            nil(),
            call,
        );
    }
    let mut result = list;
    if position >= count {
        result = grow(list, position + 1, true)?;
        if matches!(slot, Slot::Named) {
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
    Ok(result)
}

/// Where a single `[[<-` subscript points.
#[derive(Clone, Copy)]
enum Slot {
    /// An existing element, or one past the end that the assignment appends.
    At(usize),
    /// A name no element has, which the assignment appends under that name.
    Named,
    /// A missing or infinite real subscript, which selects nothing.
    Absent,
}

/// The slot one `[[<-` subscript selects in `object`, with GNU R's errors: a length-2 vector
/// takes a negative subscript as the element it leaves out.
unsafe fn one_index(extent: usize, names: Sexp, index: Sexp) -> R<Slot> {
    if length(index) == 0 {
        return Err(error("attempt to select less than one element in OneIndex"));
    }
    match index.kind() {
        SexpType::String => {
            // NA selects the first NA name, as GNU R compares the string pointers.
            if index.element(0) == values::na_string() {
                let position = (names != nil())
                    .then(|| {
                        (0..length(names)).find(|&at| names.element(at) == values::na_string())
                    })
                    .flatten();
                return Ok(position.map_or(Slot::Named, Slot::At));
            }
            let wanted = index.element(0);
            if wanted.bytes().is_empty() {
                return Ok(Slot::Named);
            }
            Ok(match match_name(names, wanted, Some(true))? {
                Some(position) => Slot::At(position),
                None => Slot::Named,
            })
        }
        SexpType::Integer | SexpType::Logical => {
            let value = values::ints(index)[0];
            // NA is the most negative integer, so it selects "more than one" element.
            one_index_position(i64::from(value), extent, "integerOneIndex")
        }
        SexpType::Real => {
            let value = values::reals(index)[0];
            // Past the longest vector R allows (2^52), a position selects nothing.
            if value.is_nan() || value > 4_503_599_627_370_496.0 {
                return Ok(Slot::Absent);
            }
            // Truncated toward zero, saturating at the integer range.
            one_index_position(value.trunc() as i64, extent, "OneIndex <real>")
        }
        SexpType::Symbol => one_index(extent, names, values::scalar_string_charsxp(index.car())?),
        kind => Err(error(format!("invalid subscript type '{}'", kind.name()))),
    }
}

unsafe fn one_index_position(value: i64, extent: usize, function: &str) -> R<Slot> {
    if value > 0 {
        return Ok(Slot::At(value as usize - 1));
    }
    if value == 0 || extent < 2 {
        return Err(error(format!(
            "attempt to select less than one element in {function}"
        )));
    }
    if extent == 2 && value > -3 {
        return Ok(Slot::At((2 + value) as usize));
    }
    Err(error(format!(
        "attempt to select more than one element in {function}"
    )))
}

/// GNU R's checks on a value assigned into one element of an atomic vector.
unsafe fn atomic_replacement_check(value: Sexp) -> R<()> {
    if !(value == nil()
        || value.kind().is_atomic()
        || matches!(value.kind(), SexpType::List | SexpType::Expression))
    {
        // GNU R reads the length of a value that has none and reports this, naming the line of
        // `subassign.c` that reads it, which moves between releases.
        let line = match crate::system::r_at_least_release(4, 6, 2) {
            true => 1861,
            false if crate::system::r_at_least(4, 6) => 1856,
            false if crate::system::r_at_least(4, 5) => 1854,
            false if crate::system::r_at_least(4, 4) => 1851,
            false => 1841,
        };
        return Err(error(format!(
            "long vectors not supported yet: subassign.c:{line}"
        )));
    }
    match length(value) {
        0 => Err(error("replacement has length zero")),
        1 => Ok(()),
        _ => Err(error("more elements supplied than there are to replace")),
    }
}

/// GNU R's `notSubsettableError`, which carries the object.
unsafe fn not_subsettable(call: Sexp, object: Sexp) -> Unwind {
    crate::conditions::classed_error(
        call,
        &format!(
            "object of type '{}' is not subsettable",
            object.kind().name()
        ),
        &["notSubsettableError", "error", "condition"],
        &[("object", object)],
    )
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
    let object = eval::eval(call.arg(0), call.env()?)?;
    // GNU R reports a name that is none against the replacement call itself.
    let name = name_argument(call.arg(1)).map_err(|unwind| match unwind {
        Unwind::Error(failure) if failure.call == unbound() => {
            crate::unwind::errorcall(call.call, failure.message)
        }
        unwind => unwind,
    })?;
    let value = eval::eval(call.arg(2), call.env()?)?;
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
        if let Some(result) = crate::dispatch::dispatch_builtin(
            symbols().dollar_assign,
            call.call,
            &args,
            call.env()?,
        )? {
            return Ok(result);
        }
    }
    let object = if object.refcount() >= 1 && !eval::is_assignment(call.call) {
        values::shallow_duplicate(object)?
    } else {
        object
    };
    dollar_assign(object, name, value, call.call)
}

pub unsafe fn dollar_assign(object: Sexp, name: Sexp, value: Sexp, call: Sexp) -> R<Sexp> {
    // `x$name <- x` stores the old `x`, as for `[[<-`.
    let object = if value == object && object.kind() != SexpType::Environment {
        values::shallow_duplicate(object)?
    } else {
        object
    };
    if object.kind() == SexpType::Record {
        return crate::records::dollar_assign(object, name.bytes(), value, call);
    }
    let object = values::materialize(object)?;
    match object.kind() {
        SexpType::Environment => {
            crate::moves::reaching_environment()?;
            let symbol = crate::heap().install_char(name)?;
            eval::define(symbol, value, object)?;
            Ok(object)
        }
        SexpType::Object if object.has_gp(gp::S4_OBJECT) => {
            let data = get_attr(object, symbols().dot_xdata);
            if data.kind() == SexpType::Environment {
                crate::moves::reaching_environment()?;
                let symbol = crate::heap().install_char(name)?;
                eval::define(symbol, value, data)?;
                return Ok(object);
            }
            Err(error_in(call, "no '$<-' method for this S4 class"))
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
        _ => Err(not_subsettable(call, object)),
    }
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

fn do_length<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(1)?;
    let count = handles::length(context, arguments.value(0));
    if count > i32::MAX as usize {
        let mut result = context.alloc::<Real>(1)?;
        result.as_mut_slice().fill(count as f64);
        return Ok(result.finish().value());
    }
    handles::integer(context, count as i32)
}

fn do_set_length<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = handles::materialize(context, arguments.value(0))?;
    let value = arguments.value(1);
    if handles::length(context, value) != 1 {
        return Err(error("wrong length for 'value' argument"));
    }
    let new_length = handles::real_scalar(context, value);
    if new_length.is_nan() || new_length < 0.0 {
        return Err(error("invalid value"));
    }
    let new_length = new_length as usize;
    match object.r_type() {
        SexpType::Nil => {
            if new_length > 0 {
                arguments.warning(context, "length of NULL cannot be changed")?;
            }
            return Ok(null());
        }
        // A pairlist keeps its leading cells with their tags and grows with untagged NULLs.
        SexpType::Pairlist => return handles::pairlist_prefix(context, object, new_length),
        kind if !kind.is_vector() => return Err(arguments.error("invalid argument")),
        _ => {}
    }
    // GNU returns the object itself when the length is unchanged, so every attribute survives;
    // the methods package relies on this to compare signatures with `identical`.
    if handles::length(context, object) == new_length {
        return Ok(object);
    }
    let names = names_of(context, object)?;
    let names = match names.r_type() {
        SexpType::Nil => None,
        _ => Some(handles::character_vector(context, names)?),
    };
    let complex_na = Rcomplex {
        r: NA_REAL,
        i: NA_REAL,
    };
    match object.r_type() {
        SexpType::Logical => {
            resized::<Logical>(context, object, names, new_length, |_, result, from| {
                pad(result.as_mut_slice(), from, NA_LOGICAL)
            })
        }
        SexpType::Integer => {
            resized::<Integer>(context, object, names, new_length, |_, result, from| {
                pad(result.as_mut_slice(), from, NA_INTEGER)
            })
        }
        SexpType::Real => resized::<Real>(context, object, names, new_length, |_, result, from| {
            pad(result.as_mut_slice(), from, NA_REAL)
        }),
        SexpType::Complex => {
            resized::<Complex>(context, object, names, new_length, |_, result, from| {
                pad(result.as_mut_slice(), from, complex_na)
            })
        }
        SexpType::Raw => resized::<Raw>(context, object, names, new_length, |_, result, from| {
            pad(result.as_mut_slice(), from, 0)
        }),
        SexpType::String => resized::<Str>(
            context,
            object,
            names,
            new_length,
            |context, result, from| {
                for index in from..result.len() {
                    result.set(context, index, handles::na());
                }
            },
        ),
        SexpType::List => resized::<List>(
            context,
            object,
            names,
            new_length,
            |context, result, from| {
                for index in from..result.len() {
                    result.set(context, index, null());
                }
            },
        ),
        SexpType::Expression => resized::<Expression>(
            context,
            object,
            names,
            new_length,
            |context, result, from| {
                for index in from..result.len() {
                    result.set(context, index, null());
                }
            },
        ),
        _ => Err(arguments.error("invalid argument")),
    }
}

/// A new vector of `length` elements of `T`: the leading ones copied from `object`, the rest
/// filled by `fill` from the position it is given, and `names` cut or padded with blanks to
/// the same length.
fn resized<'s, T: Vector>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    names: Option<Local<'s, Str>>,
    length: usize,
    fill: impl FnOnce(&Cx<'s>, &mut Fresh<'s, T>, usize),
) -> R<Local<'s>> {
    let object = object
        .downcast::<T>()
        .ok_or_else(|| error("internal error: an expanded vector has no standard form"))?;
    let kept = length.min(object.len());
    let mut result = context.alloc::<T>(length)?;
    handles::copy_prefix(context, object, &mut result, kept)?;
    fill(context, &mut result, object.len());
    let mut result = result.owned();
    if let Some(names) = names {
        let mut new_names = context.alloc::<Str>(length)?;
        handles::copy_prefix(context, names, &mut new_names, kept.min(names.len()))?;
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.names),
            new_names.finish().value(),
        )?;
    }
    Ok(result.finish().value())
}

fn pad<T: Copy>(elements: &mut [T], from: usize, value: T) {
    for slot in elements.iter_mut().skip(from) {
        *slot = value;
    }
}

unsafe fn do_lengths(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let use_names = values::as_logical_scalar(call.arg(1)) != 0;
    let listlike = matches!(object.kind(), SexpType::List | SexpType::Expression)
        || crate::coerce::is_s4(object);
    if !listlike
        && !matches!(
            object.kind(),
            SexpType::Nil
                | SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::String
                | SexpType::Raw
        )
    {
        return Err(error("'x' must be a list or atomic vector"));
    }
    let count = dispatch_length(object, call.env()?)?;
    let result = alloc(SexpType::Integer, count)?;
    crate::heap().root(result);
    // As in GNU R, a classed object's elements come from its `[[` method, because its `length`
    // method may count elements its storage does not hold (posterior's `rvar` is an empty list
    // of some length).
    let subset2 = if listlike && object.is_object() {
        Some(crate::builtins::primitive("[[").ok_or_else(|| error("no [[ primitive"))?)
    } else {
        None
    };
    for index in 0..count {
        let element = match subset2 {
            Some(subset2) => crate::scope(|| {
                let position = values::scalar_integer(index as i32 + 1)?;
                let element = eval::apply(
                    nil(),
                    subset2,
                    &eval::value_args(&[object, position]),
                    call.env()?,
                )?;
                dispatch_length(element, call.env()?)
            })?,
            // A classed element answers with its own `length` method, S4 ones included
            // (Matrix's sparse vectors).
            None if listlike => dispatch_length(object.element(index), call.env()?)?,
            None => 1,
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

/// GNU R's `namesgets`, behind `attr<-`, `attributes<-` and the C API: names are converted to
/// character without dispatch, keeping the value's own attributes as `coerceVector` does, and
/// padded with `NA` to the object's length.
pub fn assign_names<'s>(context: &mut Cx<'s>, object: &mut Owned<'s>, names: Local<'s>) -> R<()> {
    let target = object.get();
    let tagged = matches!(target.r_type(), SexpType::Pairlist | SexpType::Language);
    if names.r_type() == SexpType::Nil {
        if tagged {
            handles::set_tags(context, object, None)?;
        }
        remove_attribute(context, object, symbol(|symbols| symbols.names));
        return Ok(());
    }
    let names = if handles::inherits(context, names, "factor") {
        handles::factor_as_character(context, names)?
    } else {
        handles::coerce(context, names, SexpType::String)?
    };
    let is_vector_or_list = matches!(
        target.r_type(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
            | SexpType::Expression
            | SexpType::Pairlist
            | SexpType::Language
    );
    if !is_vector_or_list && !is_s4(target) {
        return Err(error("names() applied to a non-vector"));
    }
    let count = handles::length(context, target);
    let names_length = handles::length(context, names);
    if names_length > count {
        return Err(error(format!(
            "'names' attribute [{names_length}] must be the same length as the vector [{count}]"
        )));
    }
    if tagged {
        let names = handles::character_vector(context, names)?;
        return handles::set_tags(context, object, Some(names));
    }
    let names = if names_length < count {
        let names = handles::character_vector(context, names)?;
        let mut padded = context.alloc::<Str>(count)?;
        for index in 0..count {
            let name = if index < names_length {
                names.get(context, index)
            } else {
                handles::na()
            };
            padded.set(context, index, name);
        }
        padded.finish().value()
    } else if names.is(target) {
        // `names(x) <- x` must not make the names attribute the vector itself, which would be a
        // cycle that `serialize` recurses into forever (GNU R's `R_FixupRHS`).
        let names = handles::character_vector(context, names)?;
        handles::shallow_duplicate(context, names)?.finish().value()
    } else {
        names
    };
    // A one-dimensional array keeps its names as dimnames.
    if let Some(dims) = handles::dims(context, target)
        && dims.len() == 1
    {
        let mut dimnames = context.alloc::<List>(1)?;
        dimnames.set(context, 0, names);
        return set_attribute(
            context,
            object,
            symbol(|symbols| symbols.dimnames),
            dimnames.finish().value(),
        );
    }
    set_attribute(context, object, symbol(|symbols| symbols.names), names)
}

/// `dim<-` drops the names whatever the new dims are, where `attr(x, "dim") <-` keeps them.
fn do_set_dim<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let dims = arguments.value(1);
    // A pairlist's tags are its names but no attribute, so `dim(x) <- NULL` keeps them.
    if dims.r_type() == SexpType::Nil
        && !attribute_cells(context, object).any(|(tag, _)| {
            tag.is(symbol(|symbols| symbols.dim)) || tag.is(symbol(|symbols| symbols.names))
        })
    {
        return Ok(object);
    }
    let mut object = unshared(context, object)?;
    assign_dim(context, &mut object, dims, Some(arguments.expression()))?;
    remove_attribute(context, &mut object, symbol(|symbols| symbols.names));
    Ok(object.finish())
}

/// GNU R's `setAttrib(x, R_DimSymbol, dims)`: checks the dims against the length and drops the
/// dimnames.
fn assign_dim<'s>(
    context: &mut Cx<'s>,
    object: &mut Owned<'s>,
    dims: Local<'s>,
    call: Option<Local<'s>>,
) -> R<()> {
    if dims.r_type() == SexpType::Nil {
        remove_attribute(context, object, symbol(|symbols| symbols.dim));
        remove_attribute(context, object, symbol(|symbols| symbols.dimnames));
        return Ok(());
    }
    let target = object.get();
    if target.r_type() == SexpType::Nil {
        values::check_attributable(SexpType::Nil)?;
    }
    if !target.r_type().is_vector() && target.r_type() != SexpType::Pairlist {
        return Err(call_error(
            call,
            "invalid first argument, must be vector (list or atomic)",
        ));
    }
    let dims = handles::coerce(context, dims, SexpType::Integer)?;
    let extents = handles::integer_vector(context, dims)?;
    let object_length = handles::length(context, target) as i64;
    let extents = extents.as_slice(context);
    if crate::system::r_at_least_release(4, 6, 2) {
        let product = dims_total(extents).map_err(|message| call_error(call, message))?;
        if product != object_length {
            return Err(call_error(
                call,
                format!(
                    "dims [product {product}] do not match the length of object [{object_length}]"
                ),
            ));
        }
    } else {
        if extents.is_empty() {
            return Err(call_error(call, "length-0 dimension vector is invalid"));
        }
        // The first offending extent names the problem.
        if let Some(&bad) = extents
            .iter()
            .find(|&&value| value == NA_INTEGER || value < 0)
        {
            return Err(call_error(
                call,
                if bad == NA_INTEGER {
                    "the dims contain missing values"
                } else {
                    "the dims contain negative values"
                },
            ));
        }
        let product = extents.iter().fold(1i64, |product, &extent| {
            product.saturating_mul(extent as i64)
        });
        if product != object_length {
            // GNU R before 4.6.2 prints the extents only when both fit in an `int`.
            if product > i32::MAX as i64 || object_length > i32::MAX as i64 {
                return Err(call_error(call, "dims do not match the length of object"));
            }
            return Err(call_error(
                call,
                format!(
                    "dims [product {product}] do not match the length of object [{object_length}]"
                ),
            ));
        }
    }
    remove_attribute(context, object, symbol(|symbols| symbols.dimnames));
    set_attribute(context, object, symbol(|symbols| symbols.dim), dims)
}

/// An error that names `call`, or the running call for `None`.
fn call_error(call: Option<Local<'_>>, message: impl Into<String>) -> Unwind {
    match call {
        Some(call) => handles::error_in(call, message),
        None => error(message),
    }
}

fn do_dimnames<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    Ok(context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    )))
}

fn do_set_dimnames<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let mut object = attributes_unshared(context, arguments.value(0))?;
    assign_dimnames(
        context,
        &mut object,
        arguments.value(1),
        Some(arguments.expression()),
    )?;
    Ok(object.finish())
}

/// `dimnames<-` for raw callers, which report an error against `call` (`NULL` for none, the
/// unbound marker for the running call).
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap, all three
/// are live, and the caller may change `object` where it is.
pub unsafe fn set_dimnames(object: Sexp, dimnames: Sexp, call: Sexp) -> R<()> {
    handles::with(|context| {
        let mut object = Owned::assume(handles::adopt(context, object));
        let dimnames = handles::adopt(context, dimnames);
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        assign_dimnames(context, &mut object, dimnames, call)
    })
}

/// `dimnames<-` with GNU R's checks: each component is `NULL` or as long as its extent. A
/// list shorter than the dimensions is padded with `NULL`, an empty list removes the
/// dimnames, and a list of only `NULL` components is kept as it is.
fn assign_dimnames<'s>(
    context: &mut Cx<'s>,
    object: &mut Owned<'s>,
    dimnames: Local<'s>,
    call: Option<Local<'s>>,
) -> R<()> {
    let name = symbol(|symbols| symbols.dimnames);
    if dimnames.r_type() == SexpType::Nil {
        remove_attribute(context, object, name);
        return Ok(());
    }
    let target = object.get();
    if target.r_type() == SexpType::Nil {
        values::check_attributable(SexpType::Nil)?;
    }
    let Some(dims) = handles::dims(context, target) else {
        return Err(call_error(call, "'dimnames' applied to non-array"));
    };
    let Some(list) = dense::<List>(context, dimnames)? else {
        return Err(call_error(call, "'dimnames' must be a list"));
    };
    if list.is_empty() {
        remove_attribute(context, object, name);
        return Ok(());
    }
    if list.len() > dims.len() {
        return Err(call_error(
            call,
            format!(
                "length of 'dimnames' [{}] must match that of 'dims' [{}]",
                list.len(),
                dims.len()
            ),
        ));
    }
    // An unreferenced list of the right length is installed itself, as GNU R's `dimnamesgets`
    // does, so C code that sets the attribute and then fills the list sees its writes kept.
    let reused = (list.len() == dims.len())
        .then(|| handles::unreferenced(list))
        .flatten();
    let reuse = reused.is_some();
    let mut result = match reused {
        Some(list) => list,
        None => context.alloc::<List>(dims.len())?.owned(),
    };
    for (dimension, &extent) in dims.iter().enumerate().take(list.len()) {
        let component = context.root(list.get(context, dimension));
        // A zero-length component means no names for that dimension, whatever its extent.
        let component =
            if component.r_type() == SexpType::Nil || handles::length(context, component) == 0 {
                null()
            } else {
                let component = if handles::inherits(context, component, "factor") {
                    handles::factor_as_character(context, component)?
                } else if component.r_type() != SexpType::String {
                    handles::coerce_bare(context, component, SexpType::String)?
                } else {
                    component
                };
                if handles::length(context, component) != extent {
                    return Err(call_error(
                        call,
                        format!(
                            "length of 'dimnames' [{}] not equal to array extent",
                            dimension + 1
                        ),
                    ));
                }
                component
            };
        result
            .write(context)
            .split_elements()
            .0
            .set(dimension, component);
    }
    let names = names_of(context, list.value())?;
    if names.r_type() != SexpType::Nil && !reuse {
        let names = handles::character_vector(context, names)?;
        let mut padded = context.alloc::<Str>(dims.len())?;
        for index in 0..dims.len() {
            let name = if index < names.len() {
                names.get(context, index)
            } else {
                handles::blank()
            };
            padded.set(context, index, name);
        }
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.names),
            padded.finish().value(),
        )?;
    }
    set_attribute(context, object, name, result.finish().value())
}

fn do_set_levels<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let mut object = unshared(context, arguments.value(0))?;
    let levels = arguments.value(1);
    if levels.r_type().is_vector()
        && let Some(index) = handles::first_duplicate(context, levels)?
    {
        return Err(arguments.error(format!("factor level [{}] is duplicated", index + 1)));
    }
    set_attribute(
        context,
        &mut object,
        symbol(|symbols| symbols.levels),
        levels,
    )?;
    Ok(object.finish())
}

// Attributes.

/// Sets an attribute with the checks GNU R's `setAttrib` makes for names, dims, dimnames and
/// class. `call` is the call an error names, or `None` for the running one.
pub fn assign_attribute<'s>(
    context: &mut Cx<'s>,
    object: &mut Owned<'s>,
    name: Gc<'_, Symbol>,
    value: Local<'s>,
    call: Option<Local<'s>>,
) -> R<()> {
    if value.r_type() != SexpType::Nil {
        values::check_attributable(object.get().r_type())?;
    }
    if name.is(symbol(|symbols| symbols.comment))
        && !matches!(value.r_type(), SexpType::Nil | SexpType::String)
    {
        return Err(call_error(
            call,
            "attempt to set invalid 'comment' attribute",
        ));
    }
    if name.is(symbol(|symbols| symbols.names)) {
        return assign_names(context, object, value);
    }
    if name.is(symbol(|symbols| symbols.dim)) {
        return assign_dim(context, object, value, call);
    }
    if name.is(symbol(|symbols| symbols.dimnames)) {
        return assign_dimnames(context, object, value, call);
    }
    if name.is(symbol(|symbols| symbols.class)) {
        // GNU R's `classgets`: printing or dispatching on a class that is not character would
        // read its elements as strings.
        if !matches!(value.r_type(), SexpType::Nil | SexpType::String) {
            return Err(call_error(call, "attempt to set invalid 'class' attribute"));
        }
        if object.get().r_type() != SexpType::Integer && names_factor(context, value)? {
            return Err(call_error(
                call,
                "adding class \"factor\" to an invalid object",
            ));
        }
        return set_class(context, object, value);
    }
    if name.is(symbol(|symbols| symbols.row_names)) {
        return assign_row_names(context, object, value);
    }
    set_attribute(context, object, name, value)
}

/// [`assign_attribute`] for raw callers, with `call` as [`set_dimnames`] takes it.
///
/// # Safety
///
/// As for [`set_dimnames`], with `symbol` a symbol.
pub unsafe fn set_attribute_checked(object: Sexp, symbol: Sexp, value: Sexp, call: Sexp) -> R<()> {
    handles::with(|context| {
        let mut object = Owned::assume(handles::adopt(context, object));
        let value = handles::adopt(context, value);
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        assign_attribute(context, &mut object, Gc::adopt(symbol), value, call)
    })
}

/// GNU R's `row_names_gets`: the double `c(NA, n)` that `dput()` writes is stored as the
/// integer `c(NA, n)`, and integer row names `1:n` of three rows or more as `c(NA, n)` with a
/// positive `n`, which `.row_names_info()` then reports as not automatic. Other row names,
/// `1:2` and `1L` among them, are stored as given; `.set_row_names(n)` passes `c(NA, -n)` itself.
pub fn assign_row_names<'s>(
    context: &mut Cx<'s>,
    object: &mut Owned<'s>,
    value: Local<'s>,
) -> R<()> {
    let name = symbol(|symbols| symbols.row_names);
    let compact_double = value.r_type() == SexpType::Real
        && handles::length(context, value) == 2
        && handles::real_scalar(context, value).is_nan();
    let value = if compact_double {
        handles::coerce(context, value, SexpType::Integer)?
    } else {
        value
    };
    match value.r_type() {
        SexpType::Integer => {
            let rows = handles::integer_vector(context, value)?;
            let data = rows.as_slice(context);
            let compact = data.len() > 2
                && data
                    .iter()
                    .enumerate()
                    .all(|(index, &row)| row == index as i32 + 1);
            if compact {
                let count = data.len() as i32;
                let mut stored = context.alloc::<Integer>(2)?;
                stored.as_mut_slice().copy_from_slice(&[NA_INTEGER, count]);
                return set_attribute(context, object, name, stored.finish().value());
            }
        }
        SexpType::Nil | SexpType::String => {}
        kind => {
            return Err(error(format!(
                "row names must be 'character' or 'integer', not '{}'",
                kind.name()
            )));
        }
    }
    set_attribute(context, object, name, value)
}

/// [`assign_row_names`] for raw callers.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap, both are
/// live, and the caller may change `object` where it is.
pub unsafe fn set_row_names(object: Sexp, value: Sexp) -> R<()> {
    handles::with(|context| {
        let mut object = Owned::assume(handles::adopt(context, object));
        let value = handles::adopt(context, value);
        assign_row_names(context, &mut object, value)
    })
}

pub(crate) fn do_set_attributes<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
) -> R<Local<'s>> {
    let attributes = arguments.value(1);
    // Unlike `attr<-`, giving NULL attributes turns it into an empty list, as in GNU R.
    let mut object = match arguments.value(0) {
        object if object.r_type() != SexpType::Nil => attributes_unshared(context, object)?,
        _ if attributes.r_type() == SexpType::Nil => return Ok(null()),
        _ => context.alloc::<List>(0)?.owned().value(),
    };
    let was_s4 = is_s4(object.get());
    if attributes.r_type() == SexpType::Nil {
        clear_attributes(context, &mut object, false);
        return Ok(object.finish());
    }
    // The value is checked before anything is cleared, so a bad one leaves the object as it was.
    let Some(list) = dense::<List>(context, attributes)? else {
        return Err(arguments.error("attributes must be a list or NULL"));
    };
    if list.is_empty() {
        clear_attributes(context, &mut object, false);
        return Ok(object.finish());
    }
    let names = names_of(context, list.value())?;
    if names.r_type() == SexpType::Nil {
        return Err(arguments.error("attributes must be named"));
    }
    let names = handles::character_vector(context, names)?;
    // GNU R checks the names from the second on; an empty first name fails later, when it is
    // installed as a symbol.
    if names.get(context, 0).bytes().is_empty() {
        return Err(arguments.error("attempt to use zero-length variable name"));
    }
    for index in 1..list.len() {
        if names.get(context, index).bytes().is_empty() {
            return Err(arguments.error(format!(
                "all attributes must have names [{} does not]",
                index + 1
            )));
        }
    }
    // GNU R drops the S4 bit only when no attributes remain, so `structure()` on an S4 object
    // keeps it an S4 object.
    clear_attributes(context, &mut object, was_s4);
    // `dim` goes first, so `dimnames` can be checked against it.
    for pass in 0..2 {
        for index in 0..list.len() {
            let name = names.get(context, index);
            if !is_na(name) && name.bytes().is_empty() {
                return Err(arguments.error(format!(
                    "all attributes must have names [{} does not]",
                    index + 1
                )));
            }
            let is_dim = name.bytes() == b"dim";
            if (pass == 0) != is_dim {
                continue;
            }
            let name = context.root(name);
            let name = handles::install_interned(context, name)?;
            let value = context.root(list.get(context, index));
            assign_attribute(
                context,
                &mut object,
                name,
                value,
                Some(arguments.expression()),
            )?;
        }
    }
    Ok(object.finish())
}

// Matrices and arrays.

unsafe fn do_matrix(call: &Call<'_>) -> R<Sexp> {
    let data = values::materialize(call.arg(0))?;
    if !(data.kind().is_atomic() || matches!(data.kind(), SexpType::List | SexpType::Expression)) {
        return Err(error(format!(
            "'data' must be of a vector type, was '{}'",
            crate::coerce::type_name(data)
        )));
    }
    let by_row = values::as_logical_scalar(call.arg(3));
    if by_row == NA_LOGICAL {
        return Err(error("invalid 'byrow' argument"));
    }
    let by_row = by_row == 1;
    let dimnames = call.arg(4);
    let count = length(data);
    let nrow = matrix_extent(call, 1, 5, "nrow")?;
    let ncol = matrix_extent(call, 2, 6, "ncol")?;
    // GNU R compares the length with `extent * INT_MAX` before it looks for a zero extent, so
    // non-empty data with one zero extent given is "too long".
    let (rows, cols) = match (nrow, ncol) {
        (None, None) => (count, 1),
        (Some(0), None) | (None, Some(0)) if count > 0 => {
            return Err(error("data is too long"));
        }
        (Some(rows), None) => (rows, if rows == 0 { 0 } else { count.div_ceil(rows) }),
        (None, Some(cols)) => (if cols == 0 { 0 } else { count.div_ceil(cols) }, cols),
        (Some(rows), Some(cols)) => (rows, cols),
    };
    let total = rows * cols;
    // Whether `count` neither divides `extent` nor is divided by it.
    let unrelated = |extent: usize| {
        (count > extent && !count.is_multiple_of(extent))
            || (count < extent && !extent.is_multiple_of(count))
    };
    if count > 1 && !total.is_multiple_of(count) {
        if unrelated(rows) {
            conditions::warning(format!(
                "data length [{count}] is not a sub-multiple or multiple of the number of rows [{rows}]"
            ))?;
        } else if unrelated(cols) {
            conditions::warning(format!(
                "data length [{count}] is not a sub-multiple or multiple of the number of columns [{cols}]"
            ))?;
        } else if total != count {
            conditions::warning(format!(
                "data length differs from size of matrix: [{count} != {rows} x {cols}]"
            ))?;
        }
    } else if count > 1 && total == 0 {
        conditions::warning("non-empty data for zero-extent matrix")?;
    }
    let result = alloc(data.kind(), total)?;
    if count == 0 {
        for position in 0..total {
            set_na(result, position);
        }
    } else if !place_elements(result, data, rows, by_row) {
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
    // GNU R reports a bad `dimnames` against the closure that called the internal.
    if dimnames != nil() && length(dimnames) > 0 {
        set_dimnames(result, dimnames, crate::heap().known().unbound)?;
    }
    Ok(result)
}

/// The `nrow` or `ncol` of a `matrix()` call, argument `index`, or `None` when the caller left
/// it out, which the flag at `missing_index` says.
unsafe fn matrix_extent(
    call: &Call<'_>,
    index: usize,
    missing_index: usize,
    name: &str,
) -> R<Option<usize>> {
    if values::as_logical_scalar(call.arg(missing_index)) == 1 {
        return Ok(None);
    }
    let extent = call.arg(index);
    if !values::is_numeric_type(extent.kind()) || values::inherits(extent, "factor") {
        return Err(error("non-numeric matrix extent"));
    }
    if extent.kind() == SexpType::Real && length(extent) > 0 {
        let (_, out_of_range) = crate::coerce::real_to_int(values::reals(extent)[0]);
        if out_of_range {
            conditions::warning("NAs introduced by coercion to integer range")?;
        }
    }
    match values::as_integer_scalar(extent) {
        NA_INTEGER => Err(error(format!("invalid '{name}' value (too large or NA)"))),
        extent if extent < 0 => Err(error(format!("invalid '{name}' value (< 0)"))),
        extent => Ok(Some(extent as usize)),
    }
}

/// Fills `result`, a matrix of `rows` rows and of `data`'s type, with `data` recycled in column
/// order, or in row order when `by_row`. Returns whether it did, which it does for every vector
/// type when `data` is not empty.
unsafe fn place_elements(result: Sexp, data: Sexp, rows: usize, by_row: bool) -> bool {
    fn place<T: Copy>(target: &mut [T], source: &[T], rows: usize, by_row: bool) {
        let cols = target.len().checked_div(rows).unwrap_or(0);
        // One row or one column is the same order either way.
        if !by_row || rows <= 1 || cols <= 1 {
            if let [single] = source {
                target.fill(*single);
            } else {
                for (slot, &item) in target.iter_mut().zip(source.iter().cycle()) {
                    *slot = item;
                }
            }
            return;
        }
        let count = source.len();
        for (col, column) in target.chunks_exact_mut(rows).enumerate() {
            if count == rows * cols {
                for (row, slot) in column.iter_mut().enumerate() {
                    *slot = source[row * cols + col];
                }
            } else {
                for (row, slot) in column.iter_mut().enumerate() {
                    *slot = source[(row * cols + col) % count];
                }
            }
        }
    }
    if data.kind() != result.kind() || length(data) == 0 || data.is_altrep() {
        return false;
    }
    match data.kind() {
        SexpType::Real => place(values::reals_mut(result), values::reals(data), rows, by_row),
        SexpType::Integer | SexpType::Logical => {
            place(values::ints_mut(result), values::ints(data), rows, by_row)
        }
        SexpType::Complex => place(
            values::complexes_mut(result),
            values::complexes(data),
            rows,
            by_row,
        ),
        SexpType::Raw => place(values::raws_mut(result), values::raws(data), rows, by_row),
        SexpType::String | SexpType::List | SexpType::Expression => {
            place(
                result.slice_mut::<Sexp>(),
                data.slice::<Sexp>(),
                rows,
                by_row,
            );
            crate::heap().count_written_elements(result);
        }
        _ => return false,
    }
    true
}

unsafe fn do_array(call: &Call<'_>) -> R<Sexp> {
    let data = values::materialize(call.arg(0))?;
    let dims = coerce_vector(call.arg(1), SexpType::Integer)?;
    let dimnames = call.arg(2);
    let total: usize = if crate::system::r_at_least_release(4, 6, 2) {
        dims_total(values::ints(dims)).map_err(|message| call.error(message))? as usize
    } else {
        values::ints(dims)
            .iter()
            .map(|&value| value.max(0) as usize)
            .product()
    };
    let count = length(data);
    let kind = if data == nil() {
        SexpType::Logical
    } else {
        data.kind()
    };
    let result = alloc(kind, total)?;
    if count == 0 || !place_elements(result, data, 1, false) {
        for position in 0..total {
            if count == 0 {
                set_na(result, position);
            } else {
                copy_element(data, position % count, result, position)?;
            }
        }
    }
    values::set_attr(result, symbols().dim, dims)?;
    // GNU R reports a bad `dimnames` against the closure that called the internal.
    if dimnames != nil() && length(dimnames) > 0 {
        set_dimnames(result, dimnames, crate::heap().known().unbound)?;
    }
    Ok(result)
}

/// The number of elements an integer `dim` vector describes, checked in the order GNU R's
/// `dim2total` (4.6.2 and later) checks it: an empty vector, then each extent for `NA` and
/// sign, then a product beyond the longest vector. The product is a double, as there, so a
/// zero extent still lets the later extents be checked.
fn dims_total(extents: &[i32]) -> Result<i64, &'static str> {
    const LONGEST_VECTOR: f64 = 4_503_599_627_370_496.0;
    if extents.is_empty() {
        return Err("'dim' cannot be of length 0");
    }
    let mut product = 1.0f64;
    for &extent in extents {
        if extent == NA_INTEGER {
            return Err("the dims contain missing values");
        }
        if extent < 0 {
            return Err("the dims contain negative values");
        }
        product = if extent == 0 {
            0.0
        } else {
            product * f64::from(extent)
        };
    }
    if product > LONGEST_VECTOR {
        return Err("too many elements specified");
    }
    Ok(product as i64)
}

unsafe fn do_transpose(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
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
    let Some(result) = permuted_copy(object, &[rows, cols], &[1, 0])? else {
        return Err(call.error("argument is not a matrix"));
    };
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
    // GNU R keeps the other attributes after `dim` and `dimnames`, and `getParseData` relies on
    // that to read the parse data's `text` after transposing it.
    values::copy_most_attributes(object, result, false)?;
    Ok(result)
}

/// R 4.6's `asplit(x, d.ans, d.call, dn.ans, dn.call, n.call, n.ans, drop)`, where `x` is
/// already permuted so that each piece is a contiguous run of `n.call` elements. The pieces
/// keep the remaining dimensions unless `drop`, and the list takes the split ones.
unsafe fn do_asplit(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let (answer_dims, piece_dims) = (call.arg(1), call.arg(2));
    let (answer_names, piece_names) = (call.arg(3), call.arg(4));
    let piece_length = values::as_real_scalar(call.arg(5)).max(0.0) as usize;
    let pieces = values::as_real_scalar(call.arg(6)).max(0.0) as usize;
    let drop = values::as_logical_scalar(call.arg(7)) == 1;
    let vector = matches!(
        x.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
            | SexpType::Expression
    );
    if !vector || pieces.saturating_mul(piece_length) > length(x) {
        return Err(error("invalid 'x' argument"));
    }
    if !drop && length(piece_dims) == 0 {
        return Err(error("length-0 dimension vector is invalid"));
    }
    let named = |names: Sexp| names.kind() == SexpType::List && length(names) > 0;
    let result = alloc(SexpType::List, pieces)?;
    for piece in 0..pieces {
        let element = alloc(x.kind(), piece_length)?;
        for offset in 0..piece_length {
            copy_element(x, piece * piece_length + offset, element, offset)?;
        }
        values::set_elt(result, piece, element);
        if !drop {
            values::set_attr(
                element,
                symbols().dim,
                coerce_vector(piece_dims, SexpType::Integer)?,
            )?;
            if named(piece_names) {
                values::set_attr(element, symbols().dimnames, piece_names)?;
            }
        }
    }
    values::set_attr(
        result,
        symbols().dim,
        coerce_vector(answer_dims, SexpType::Integer)?,
    )?;
    if named(answer_names) {
        values::set_attr(result, symbols().dimnames, answer_names)?;
    }
    Ok(result)
}

/// A fresh vector of `object`'s elements with the dimensions `dims` permuted into the order
/// `order` (result dimension `k` is source dimension `order[k]`), without attributes, or `None`
/// for a type that has no array layout to permute (GNU R permutes only atomic vectors and lists).
unsafe fn permuted_copy(object: Sexp, dims: &[usize], order: &[usize]) -> R<Option<Sexp>> {
    // C code can set a `dim` that disagrees with the length, which would index out of bounds.
    // Within the length, no stride or offset can overflow.
    let total = dims
        .iter()
        .try_fold(1usize, |product, &extent| product.checked_mul(extent))
        .filter(|&total| total <= length(object))
        .ok_or_else(|| error("dims do not match the length of object"))?;
    match object.kind() {
        SexpType::Real => {
            let result = values::alloc_uninit(SexpType::Real, total)?;
            let source = &values::reals(object)[..total];
            permute(source, values::reals_mut(result), dims, order);
            Ok(Some(result))
        }
        SexpType::Integer | SexpType::Logical => {
            let result = values::alloc_uninit(object.kind(), total)?;
            let source = &values::ints(object)[..total];
            permute(source, values::ints_mut(result), dims, order);
            Ok(Some(result))
        }
        SexpType::Complex => {
            let result = alloc(SexpType::Complex, total)?;
            let source = &values::complexes(object)[..total];
            permute(source, values::complexes_mut(result), dims, order);
            Ok(Some(result))
        }
        SexpType::Raw => {
            let result = alloc(SexpType::Raw, total)?;
            let source = &values::raws(object)[..total];
            permute(source, values::raws_mut(result), dims, order);
            Ok(Some(result))
        }
        SexpType::String | SexpType::List => {
            let source = values::materialize(object)?;
            let result = alloc(object.kind(), total)?;
            permute(
                &source.slice::<Sexp>()[..total],
                result.slice_mut::<Sexp>(),
                dims,
                order,
            );
            crate::heap().count_written_elements(result);
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}

/// A tile is 32 source rows by 256 columns. One row of it reads one cache line from each column
/// (16 KiB for doubles), and the next seven rows hit the same lines in L1. Long column runs
/// amortize the loop overhead that square 32 by 32 tiles pay, which made transposes 15 to 30%
/// slower.
const ROW_TILE: usize = 32;
const COLUMN_TILE: usize = 256;

/// Writes `source`, a column-major array with extents `dims`, into `target` with the dimensions
/// permuted by `order`. The source's first dimension is contiguous in `source` and the result's
/// first dimension, `order[0]`, is contiguous in `target`, so those two are walked as blocked
/// tiles to keep both streams in cache; the other dimensions step an offset pair. When the two
/// are the same dimension, each step copies one contiguous run.
fn permute<T: Copy>(source: &[T], target: &mut [T], dims: &[usize], order: &[usize]) {
    let total: usize = dims.iter().product();
    assert!(source.len() >= total && target.len() >= total);
    if total == 0 {
        return;
    }
    let mut target_stride: SmallVec<[usize; 6]> = smallvec![0; dims.len()];
    let mut stride = 1usize;
    for &dimension in order {
        target_stride[dimension] = stride;
        stride *= dims[dimension];
    }
    let fast = order[0];
    let rows = dims[0];
    let row_stride = target_stride[0];
    let columns = if fast == 0 { 1 } else { dims[fast] };
    let mut column_stride = 0;
    let mut outer: SmallVec<[OuterDimension; 4]> = SmallVec::new();
    stride = 1;
    for (dimension, (&extent, &target_step)) in dims.iter().zip(&target_stride).enumerate() {
        if dimension == fast {
            column_stride = stride;
        } else if dimension != 0 {
            outer.push(OuterDimension {
                extent,
                source_step: stride,
                target_step,
                counter: 0,
            });
        }
        stride *= extent;
    }
    let (mut from, mut to) = (0usize, 0usize);
    for _ in 0..total / (rows * columns) {
        if fast == 0 {
            target[to..to + rows].copy_from_slice(&source[from..from + rows]);
        } else {
            // Plain loops rather than `step_by`, whose setup costs more than copying a small
            // array.
            let mut row_block = 0;
            while row_block < rows {
                let row_end = rows.min(row_block + ROW_TILE);
                let mut column_block = 0;
                while column_block < columns {
                    let width = COLUMN_TILE.min(columns - column_block);
                    for row in row_block..row_end {
                        let strided = &source[from + row + column_block * column_stride..];
                        let destination =
                            &mut target[to + row * row_stride + column_block..][..width];
                        for (step, slot) in destination.iter_mut().enumerate() {
                            *slot = strided[step * column_stride];
                        }
                    }
                    column_block += COLUMN_TILE;
                }
                row_block += ROW_TILE;
            }
        }
        for dimension in &mut outer {
            dimension.counter += 1;
            from += dimension.source_step;
            to += dimension.target_step;
            if dimension.counter < dimension.extent {
                break;
            }
            from -= dimension.source_step * dimension.extent;
            to -= dimension.target_step * dimension.extent;
            dimension.counter = 0;
        }
    }
}

/// A dimension of [`permute`] other than the two a tile spans, stepped as one digit of an
/// odometer: each step moves the source and target offsets by its strides.
struct OuterDimension {
    extent: usize,
    source_step: usize,
    target_step: usize,
    counter: usize,
}

unsafe fn do_aperm(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let permutation = call.arg(1);
    let resize = values::as_logical_scalar(call.arg(2)) != 0;
    // C code can set an empty `dim`, which leaves nothing to permute.
    let Some(dims) = values::dims(object).filter(|dims| !dims.is_empty()) else {
        return Err(call.error("invalid first argument, must be an array"));
    };
    let rank = dims.len();
    let order: Vec<usize> = if permutation == nil() {
        (0..rank).rev().collect()
    } else {
        let length = values::length(permutation);
        if length != rank {
            return Err(call.error(format!("'perm' is of wrong length {length} (!= {rank})")));
        }
        let mut order = Vec::with_capacity(rank);
        if permutation.kind() == SexpType::String {
            let dimnames_names = values::names(get_attr(object, symbols().dimnames));
            if dimnames_names == nil() {
                return Err(call.error("'a' does not have named dimnames"));
            }
            for index in 0..rank {
                let wanted = permutation.element(index);
                let Some(found) = (0..rank).find(|&candidate| {
                    values::same_string(dimnames_names.element(candidate), wanted)
                }) else {
                    return Err(call.error(format!(
                        "'perm[{}]' does not match a dimension name",
                        index + 1
                    )));
                };
                order.push(found);
            }
        } else {
            for index in 0..rank {
                let value = numeric_element(permutation, index);
                if !(value >= 1.0 && value < rank as f64 + 1.0) {
                    return Err(call.error("value out of range in 'perm'"));
                }
                order.push(value as usize - 1);
            }
        }
        let mut seen = vec![false; rank];
        for &dimension in &order {
            if std::mem::replace(&mut seen[dimension], true) {
                return Err(call.error("invalid 'perm' argument"));
            }
        }
        order
    };
    let new_dims: Vec<usize> = order.iter().map(|&dimension| dims[dimension]).collect();
    let result = permuted_copy(object, &dims, &order)?.ok_or_else(|| {
        call.error(format!(
            "unimplemented type '{}' in 'aperm'\n",
            object.kind().name()
        ))
    })?;
    if resize {
        let dim_values: Vec<i32> = new_dims.iter().map(|&extent| extent as i32).collect();
        let dim = values::integer_vector(&dim_values)?;
        let dim_names = values::names(get_attr(object, symbols().dim));
        if dim_names != nil() {
            values::set_attr(dim, symbols().names, select_elements(dim_names, &order)?)?;
        }
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
    let result = values::shallow_duplicate_attr(object)?;
    let dimnames = get_attr(object, symbols().dimnames);
    values::remove_attr(result, symbols().dim);
    values::remove_attr(result, symbols().dimnames);
    if kept.len() > 1 {
        let dim_values: SmallVec<[i32; 4]> = kept
            .iter()
            .map(|&dimension| dims[dimension] as i32)
            .collect();
        let dim = values::integer_vector(&dim_values)?;
        values::set_attr(result, symbols().dim, dim)?;
        // As in GNU R's `DropDims`, the kept dimensions keep their dimnames and the names of
        // those dimnames, unless none of the kept dimensions has any.
        let list: SmallVec<[Sexp; 4]> = if dimnames == nil() {
            SmallVec::new()
        } else {
            kept.iter()
                .map(|&dimension| dimnames.element(dimension))
                .collect()
        };
        if list.iter().any(|names| *names != nil()) {
            let list = values::list_vector(&list)?;
            let dimension_names = get_attr(dimnames, symbols().names);
            if dimension_names != nil() {
                let kept_names = alloc(SexpType::String, kept.len())?;
                for (index, &dimension) in kept.iter().enumerate() {
                    crate::heap().set_element(
                        kept_names,
                        index,
                        dimension_names.element(dimension),
                    );
                }
                values::set_attr(list, symbols().names, kept_names)?;
            }
            values::set_attr(result, symbols().dimnames, list)?;
        }
    } else if dimnames != nil() {
        let all_dimnames: SmallVec<[Sexp; 4]> = (0..dims.len())
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
    if let [dimension] = kept {
        return dimnames.get(*dimension).copied().unwrap_or(nil());
    }
    let mut named = dimnames.iter().copied().filter(|names| *names != nil());
    match (named.next(), named.next()) {
        (Some(names), None) => names,
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
    let level_missing =
        level.value.kind() == SexpType::Symbol && eval::is_missing(level.value, call.env()?)?;
    let level_value = eval::eval(level.value, call.env()?)?;
    let deparse_level = values::as_integer_scalar(level_value);
    let mut items: Vec<Arg> = Vec::with_capacity(rest.len());
    let mut expressions: Vec<Sexp> = Vec::with_capacity(rest.len());
    for arg in rest {
        if arg.value == symbols().dots {
            let dots = eval::find_var(symbols().dots, call.env()?)?;
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
                value: eval::eval(arg.value, call.env()?)?,
            });
            expressions.push(arg.value);
        }
    }
    // S3 dispatch, as GNU R's `do_bind` does it: the first classed argument with a method
    // picks it. Through R 4.3 the method gets the `...` arguments alone; from R 4.4 on, a
    // supplied `deparse.level` comes first, tagged, and only a missing one is dropped.
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
                crate::dispatch::lookup_method(symbol, call.env()?, values::base_namespace())?
            {
                // The method sees promises, so `data.frame` inside `cbind.data.frame` names a
                // column after its argument expression.
                let mut arguments = Vec::with_capacity(items.len() + 1);
                if !level_missing && crate::system::r_at_least(4, 4) {
                    arguments.push(Arg {
                        tag: values::install("deparse.level")?,
                        value: level_value,
                    });
                }
                for (item, &expression) in items.iter().zip(&expressions) {
                    arguments.push(Arg {
                        tag: item.tag,
                        value: eval::forced_promise(expression, item.value)?,
                    });
                }
                return eval::apply(call.call, method, &arguments, call.env()?);
            }
        }
    }
    // An S4 argument hands the call to the methods package's hidden `cbind` or `rbind`, which
    // dispatch on `cbind2` and `rbind2`. Their default methods bind with a deparse level of
    // -1, which marks the call as already coming from there.
    if deparse_level != -1 && items.iter().any(|arg| coerce::is_s4(arg.value)) {
        let registry = crate::heap().known().namespace_registry;
        let methods = value_of(eval::find_var_in_frame(
            values::install("methods")?,
            registry,
        )?)?;
        if methods.kind() == SexpType::Environment {
            let function = value_of(eval::find_var_in_frame(values::install(generic)?, methods)?)?;
            if function.kind() == SexpType::Closure {
                let mut arguments = Vec::with_capacity(items.len() + 1);
                arguments.push(Arg {
                    tag: values::install("deparse.level")?,
                    value: level_value,
                });
                for (item, &expression) in items.iter().zip(&expressions) {
                    arguments.push(Arg {
                        tag: item.tag,
                        value: eval::forced_promise(expression, item.value)?,
                    });
                }
                return eval::apply(call.call, function, &arguments, call.env()?);
            }
        }
    }
    // GNU R's `cbind` and `rbind` from its `bind.c`, written once for both directions: the
    // extent is the length along the bound vectors (rows for `cbind`), and each argument adds
    // lines across it (columns for `cbind`). Only a call of NULLs alone gives NULL.
    if items.iter().all(|arg| arg.value == nil()) {
        return Ok(nil());
    }
    // An argument that is no vector makes the result a list, as in GNU R: a call or pairlist
    // binds its elements, and anything else (a symbol, a function) is one element.
    for arg in &mut items {
        match arg.value.kind() {
            SexpType::Nil
            | SexpType::Raw
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::List
            | SexpType::Expression => {}
            SexpType::Language | SexpType::Pairlist => {
                arg.value = coerce::coerce_vector(arg.value, SexpType::List)?;
                crate::heap().root(arg.value);
            }
            _ => {
                let wrapped = alloc(SexpType::List, 1)?;
                values::set_elt(wrapped, 0, arg.value);
                crate::heap().root(wrapped);
                arg.value = wrapped;
            }
        }
    }
    // Starting below raw, so that binding only raw vectors gives a raw matrix.
    let mut kind = SexpType::Nil;
    for arg in &items {
        kind = coerce::higher_type(kind, arg.value.kind());
    }
    if kind == SexpType::Nil {
        kind = SexpType::Logical;
    }
    if kind == SexpType::Expression {
        return Err(crate::unwind::error(
            "cannot create a matrix from type 'expression'",
        ));
    }
    let (along, across) = if by_row { (1, 0) } else { (0, 1) };
    let matrix_dims = |value: Sexp| values::dims(value).filter(|dims| dims.len() == 2);
    // A zero-length vector, NULL included, adds a line only when no argument has a nonzero
    // extent, so that `cbind(matrix(0, 0, 2), NULL)` has three columns.
    let min_length = usize::from(items.iter().any(|arg| match matrix_dims(arg.value) {
        Some(dims) => dims[along] > 0,
        None => length(arg.value) > 0,
    }));
    let mut matrix_extent: Option<usize> = None;
    let mut vector_extent = 0usize;
    let mut total = 0usize;
    for (index, arg) in items.iter().enumerate() {
        match matrix_dims(arg.value) {
            Some(dims) => {
                if matrix_extent.is_some_and(|extent| extent != dims[along]) {
                    let direction = if by_row { "columns" } else { "rows" };
                    // Reported against the `rbind` or `cbind` closure call, as GNU R does.
                    return Err(crate::unwind::error(format!(
                        "number of {direction} of matrices must match (see arg {})",
                        index + 1
                    )));
                }
                matrix_extent = Some(dims[along]);
                total += dims[across];
            }
            None if length(arg.value) >= min_length => {
                vector_extent = vector_extent.max(length(arg.value));
                total += 1;
            }
            None => {}
        }
    }
    let extent = matrix_extent.unwrap_or(vector_extent);
    // The warning names the `rbind` or `cbind` closure call, not the internal.
    let frames = &crate::interp().frames;
    let closure_call = call
        .frame_index()
        .map_or(call.call, |position| frames[position].call);
    let names_vector = |index: usize| {
        let expression = expressions[index];
        items[index].tag != nil()
            || deparse_level == 2
            || (deparse_level == 1 && expression.kind() == SexpType::Symbol)
    };
    let mut has_bound_names = false;
    let mut matrix_has_other_names = false;
    let mut longest_names = 0usize;
    let mut warned = false;
    for (index, arg) in items.iter().enumerate() {
        if matrix_dims(arg.value).is_some() {
            let dimnames = get_attr(arg.value, symbols().dimnames);
            if length(dimnames) == 2 {
                has_bound_names |= dimnames.element(across) != nil();
                // GNU R records the first matrix's extent here, so a zero extent counts as none.
                matrix_has_other_names |= dimnames.element(along) != nil() && extent > 0;
            }
            continue;
        }
        let count = length(arg.value);
        if !warned && count > 0 && (count > extent || !extent.is_multiple_of(count)) {
            warned = true;
            let direction = if by_row { "columns" } else { "rows" };
            crate::conditions::warning_call(
                closure_call,
                format!(
                    "number of {direction} of result is not a multiple of vector length (arg {})",
                    index + 1
                ),
            )?;
        }
        has_bound_names |= count >= min_length && names_vector(index);
        longest_names = longest_names.max(length(values::names(arg.value)));
    }
    let has_other_names = matrix_has_other_names || longest_names == extent;
    let (rows, cols) = if by_row {
        (total, extent)
    } else {
        (extent, total)
    };
    let result = alloc(kind, rows * cols)?;
    let bound_names = if has_bound_names {
        alloc(SexpType::String, total)?
    } else {
        nil()
    };
    let mut other_names = nil();
    let mut offset = 0usize;
    for (index, arg) in items.iter().enumerate() {
        let value = arg.value;
        if let Some(dims) = matrix_dims(value) {
            let count = dims[across];
            let source = values::materialize(value)?;
            let source_rows = dims[0];
            for line in 0..count {
                let line_offset = offset + line;
                if by_row {
                    copy_mapped(
                        source,
                        result,
                        extent,
                        |position| line + position * source_rows,
                        |position| line_offset + position * rows,
                    )?;
                } else {
                    copy_mapped(
                        source,
                        result,
                        extent,
                        |position| position + line * source_rows,
                        |position| position + line_offset * rows,
                    )?;
                }
            }
            let dimnames = get_attr(value, symbols().dimnames);
            let names = if dimnames != nil() {
                dimnames.element(across)
            } else {
                nil()
            };
            if has_other_names && other_names == nil() && dimnames != nil() {
                other_names = dimnames.element(along);
            }
            if has_bound_names {
                for line in 0..count {
                    let name = if names != nil() {
                        names.element(line)
                    } else {
                        values::blank_string()
                    };
                    crate::heap().set_element(bound_names, offset + line, name);
                }
            }
            offset += count;
            continue;
        }
        let source_length = length(value);
        if source_length < min_length {
            continue;
        }
        if source_length > 0 {
            let source = values::materialize(value)?;
            if by_row {
                copy_mapped(
                    source,
                    result,
                    extent,
                    |position| position % source_length,
                    |position| offset + position * rows,
                )?;
            } else {
                copy_mapped(
                    source,
                    result,
                    extent,
                    |position| position % source_length,
                    |position| position + offset * rows,
                )?;
            }
        }
        let names = values::names(value);
        if has_other_names && other_names == nil() && names != nil() && length(names) == extent {
            other_names = names;
        }
        if has_bound_names {
            let expression = expressions[index];
            let name = if arg.tag != nil() {
                values::mkchar(&symbol_name(arg.tag))?
            } else if deparse_level == 1 && expression.kind() == SexpType::Symbol {
                values::mkchar(&symbol_name(expression))?
            } else if deparse_level == 2 {
                values::mkchar(&crate::deparse::deparse_one_line(expression))?
            } else {
                values::blank_string()
            };
            crate::heap().set_element(bound_names, offset, name);
        }
        offset += 1;
    }
    let dim = values::integer_vector(&[rows as i32, cols as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    if has_bound_names || has_other_names {
        let dimnames = if by_row {
            values::list_vector(&[bound_names, other_names])?
        } else {
            values::list_vector(&[other_names, bound_names])?
        };
        values::set_attr(result, symbols().dimnames, dimnames)?;
    }
    Ok(result)
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

pub(crate) fn do_is_list_factor<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
) -> R<Local<'s>> {
    let object = arguments.value(0);
    let recursive = match handles::logical_scalar(context, arguments.value(1)) {
        // R 4.5 refuses an NA, which earlier releases took as true.
        NA_LOGICAL if crate::system::r_at_least(4, 5) => {
            return Err(arguments.error("NA in coercion to boolean"));
        }
        recursive => recursive != 0,
    };
    let found = match dense::<List>(context, object)? {
        Some(list) if !list.is_empty() => list_factor(context, list, recursive)? == Some(true),
        _ => false,
    };
    handles::logical_constant(context, i32::from(found))
}

/// Whether every element of `list` is a factor, or with `recursive` a list of them: `None`
/// for a list with no factor at any depth.
fn list_factor<'s>(
    context: &mut Cx<'s>,
    list: Local<'s, List>,
    recursive: bool,
) -> R<Option<bool>> {
    let mut any = false;
    for index in 0..list.len() {
        let element = list.get(context, index);
        if element.r_type() == SexpType::List {
            if !recursive {
                return Ok(Some(false));
            }
            let element = context.root(element);
            let element = dense::<List>(context, element)?
                .ok_or_else(|| error("internal error: a list has no standard form"))?;
            match list_factor(context, element, recursive)? {
                Some(true) => any = true,
                Some(false) => return Ok(Some(false)),
                None => {}
            }
        } else if handles::inherits(context, element, "factor") {
            any = true;
        } else {
            return Ok(Some(false));
        }
    }
    Ok(any.then_some(true))
}

fn do_row<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    row_col(context, arguments, true)
}

fn do_col<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    row_col(context, arguments, false)
}

fn row_col<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>, rows: bool) -> R<Local<'s>> {
    let dims = handles::integer_vector(context, arguments.value(0))?;
    let &[row_count, col_count] = dims.as_slice(context) else {
        return Err(error(format!(
            "a matrix-like object is required as argument to '{}'",
            if rows { "row" } else { "col" }
        )));
    };
    let (row_count, col_count) = (row_count.max(0) as usize, col_count.max(0) as usize);
    let mut result = context.alloc::<Integer>(row_count * col_count)?;
    let elements = result.as_mut_slice();
    for row in 0..row_count {
        for col in 0..col_count {
            elements[row + col * row_count] = if rows { row as i32 + 1 } else { col as i32 + 1 };
        }
    }
    let mut dim = context.alloc::<Integer>(2)?;
    dim.as_mut_slice()
        .copy_from_slice(&[row_count as i32, col_count as i32]);
    let mut result = result.owned();
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dim.finish().value(),
    )?;
    Ok(result.finish().value())
}

/// `.Internal(max.col(m, ties.method))`, with the method as GNU R's code: 1 random, 2 first,
/// 3 last.
unsafe fn do_max_col(call: &Call<'_>) -> R<Sexp> {
    let matrix = call.arg(0);
    let dims = get_attr(matrix, symbols().dim);
    let dims = coerce_vector(dims, SexpType::Integer)?;
    let (rows, columns) = match values::ints(dims) {
        &[rows, columns] => (rows.max(0) as usize, columns.max(0) as usize),
        _ => (length(matrix), 1),
    };
    let method = values::as_integer_scalar(call.arg(1));
    let matrix = coerce_vector(matrix, SexpType::Real)?;
    let result = alloc(SexpType::Integer, rows)?;
    max_col(
        values::reals(matrix),
        rows,
        columns,
        method,
        values::ints_mut(result),
    )?;
    Ok(result)
}

/// `R_max_col`: the column of each row's maximum of the column-major `matrix`, or `NA` for a
/// row with a missing value. With `method` 1, entries within a relative 1e-5 of the row's
/// largest magnitude tie and one of them is drawn at random; 2 takes the first maximum and 3
/// the last.
pub unsafe fn max_col(
    matrix: &[f64],
    rows: usize,
    columns: usize,
    method: i32,
    maxes: &mut [i32],
) -> R<()> {
    let random = method == 1;
    if random {
        crate::random::get_rng_state()?;
    }
    let entry = |row: usize, column: usize| matrix.get(row + column * rows).copied();
    for (row, slot) in maxes.iter_mut().enumerate().take(rows) {
        let mut missing = false;
        let mut largest = 0.0f64;
        for column in 0..columns {
            let value = entry(row, column).unwrap_or(f64::NAN);
            if value.is_nan() {
                missing = true;
                break;
            }
            if value.is_finite() {
                largest = largest.max(value.abs());
            }
        }
        let Some(mut maximum) = entry(row, 0).filter(|_| !missing && columns > 0) else {
            *slot = NA_INTEGER;
            continue;
        };
        let mut chosen = 0;
        if random {
            // Entries are treated as probabilities: values this close to the maximum tie.
            let tolerance = 1e-5 * largest;
            let mut ties = 1;
            for column in 1..columns {
                let value = entry(row, column).unwrap_or(f64::NAN);
                if value > maximum + tolerance {
                    maximum = value;
                    chosen = column;
                    ties = 1;
                } else if value >= maximum - tolerance {
                    // Reservoir sampling: each tied column ends up chosen with equal
                    // probability.
                    ties += 1;
                    if f64::from(ties) * crate::random::unif_rand()? < 1.0 {
                        chosen = column;
                    }
                }
            }
        } else {
            let last = method == 3;
            for column in 1..columns {
                let value = entry(row, column).unwrap_or(f64::NAN);
                if value > maximum || (last && value == maximum) {
                    maximum = value;
                    chosen = column;
                }
            }
        }
        *slot = chosen as i32 + 1;
    }
    if random {
        crate::random::put_rng_state()?;
    }
    Ok(())
}

fn do_diag<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let data = handles::materialize(context, arguments.value(0))?;
    let rows = handles::integer_scalar(context, arguments.value(1)).max(0) as usize;
    let cols = handles::integer_scalar(context, arguments.value(2)).max(0) as usize;
    if rows.min(cols) > 0 && handles::length(context, data) == 0 {
        return Err(error("'x' must have positive length"));
    }
    let mut result = match data.r_type() {
        SexpType::Complex => diagonal::<Complex>(context, data, rows, cols)?,
        SexpType::Integer => diagonal::<Integer>(context, data, rows, cols)?,
        SexpType::Logical => diagonal::<Logical>(context, data, rows, cols)?,
        SexpType::Raw => diagonal::<Raw>(context, data, rows, cols)?,
        _ => diagonal::<Real>(context, data, rows, cols)?,
    };
    let mut dim = context.alloc::<Integer>(2)?;
    dim.as_mut_slice()
        .copy_from_slice(&[rows as i32, cols as i32]);
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dim.finish().value(),
    )?;
    Ok(result.finish())
}

/// A `rows` by `cols` matrix of `T`, zero but for its diagonal, which repeats `data`.
fn diagonal<'s, T: Atomic>(
    context: &mut Cx<'s>,
    data: Local<'s>,
    rows: usize,
    cols: usize,
) -> R<Owned<'s>> {
    let data = handles::coerce(context, data, T::TYPE)?;
    let data = dense::<T>(context, data)?
        .ok_or_else(|| error("internal error: a coerced vector has no standard form"))?;
    let mut result = context.alloc::<T>(rows * cols)?;
    let target = result.as_mut_slice();
    let source = data.as_slice(context);
    if !source.is_empty() {
        for index in 0..rows.min(cols) {
            target[index + index * rows] = source[index % source.len()];
        }
    }
    Ok(result.owned().value())
}

fn do_short_row_names<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let kind = handles::integer_scalar(context, arguments.value(1));
    let row_names = get_attribute(context, object, symbol(|symbols| symbols.row_names));
    if kind == 0 {
        return Ok(context.root(row_names));
    }
    let count = match row_names.downcast::<Integer>() {
        Some(rows) if rows.len() == 2 && rows.as_slice()[0] == NA_INTEGER => rows.as_slice()[1],
        _ if row_names.r_type() == SexpType::Nil => 0,
        _ => {
            let row_names = context.root(row_names);
            handles::length(context, row_names) as i32
        }
    };
    handles::integer(context, if kind == 1 { count } else { count.abs() })
}

unsafe fn do_copy_df_attr(call: &Call<'_>) -> R<Sexp> {
    let from = call.arg(0);
    let to = modifiable(call.arg(1))?;
    crate::heap().set_attrib(to, nil());
    values::copy_attributes(from, to)?;
    Ok(to)
}
