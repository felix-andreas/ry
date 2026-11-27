//! Subsetting and subassignment (`[`, `[[`, `$`, `@` and their replacement forms), `c` and
//! `unlist`, lengths, names, dims, and attributes.

use {
    crate::{
        attribute_access::names_factor,
        base_fast_paths::fast_paths_enabled,
        builtins::{
            Arg, BuiltinDef, Call, Generic, builtin, internal, internal_special, is_native_value,
            special,
        },
        coerce,
        distributions::SessionDeviates,
        element_access::direct_subset2,
        eval::{self, value_of},
        handles::{
            self, Alt, Args, Atomic, Chars, Complex, Cx, DotsArgument, Elements, Environment,
            Exact, Expression, Fresh, Gc, I64, Integer, Kind, Language, List, Local, Logical,
            Owned, Pairlist, Pointer, Promise, Raw, Real, Str, Symbol, Tagged, Unfilled, Vector,
            attribute_cells, attributes_unshared, clear_attributes, coerce, compact_sequence,
            dense, element_text, get_attribute, int_element, is_missing_argument, is_na, is_s4,
            item, materialize, na, names_of, null, pairlist_to_list, real_element,
            remove_attribute, safe, same_text, set_attribute, set_class, store_item, symbol,
            unshared,
        },
        random::Deviates,
        unwind::{R, Unwind, error},
        values::{self, Dims, get_attr, nil, scalar_integer, symbols, unbound},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType},
    smallvec::{SmallVec, smallvec},
    std::mem::MaybeUninit,
};

pub const BUILTINS: &[BuiltinDef] = &[
    special("[", -1, do_subset)
        .dispatch_or_eval()
        .direct(direct_subset),
    special("[[", -1, do_subset2)
        .dispatch_or_eval()
        .direct(direct_subset2),
    special("$", 2, do_dollar),
    special("@", 2, safe!(do_at)),
    special("[<-", -1, do_subassign)
        .dispatch_or_eval()
        .direct(handles::safe_direct!(assign_slab)),
    special("[[<-", -1, do_subassign2).dispatch_or_eval(),
    special("$<-", 3, do_dollar_assign),
    special("@<-", 3, do_at_assign).direct(handles::safe_direct!(at_assign_values)),
    builtin(".subset", -1, do_subset),
    builtin(".subset2", -1, do_subset2).direct(direct_subset2),
    builtin("list", -1, safe!(list_of))
        .direct(handles::safe_direct!(call direct_list))
        .direct_named(),
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
    internal("lengths", 2, safe!(do_lengths)).generic(Generic::Internal),
    builtin("dim<-", 2, safe!(do_set_dim)).generic(Generic::Internal),
    builtin("dimnames", 1, safe!(do_dimnames)).generic(Generic::Internal),
    builtin("dimnames<-", 2, safe!(do_set_dimnames)).generic(Generic::Internal),
    builtin("levels<-", 2, safe!(do_set_levels)).generic(Generic::Internal),
    builtin("attributes<-", 2, safe!(do_set_attributes)),
    internal("matrix", 7, safe!(do_matrix)),
    internal("array", 3, safe!(do_array)),
    internal("t.default", 1, safe!(do_transpose)),
    internal("max.col", 2, safe!(do_max_col)),
    internal("aperm", 3, safe!(do_aperm)),
    internal("asplit", 8, safe!(do_asplit)),
    internal("drop", 1, safe!(do_drop)),
    internal_special("cbind", -1, safe!(do_cbind)),
    internal_special("rbind", -1, safe!(do_rbind)),
    internal("rev", 1, safe!(do_rev)),
    internal("islistfactor", 2, safe!(do_is_list_factor)),
    internal("comment", 1, safe!(do_comment)),
    internal("comment<-", 2, safe!(do_set_comment)),
    internal("row", 1, safe!(do_row)),
    internal("col", 1, safe!(do_col)),
    internal("diag", 3, safe!(do_diag)),
    internal("shortRowNames", 2, safe!(do_short_row_names)),
    internal("copyDFattr", 2, safe!(do_copy_df_attr)),
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
    element_inline(context, vector, index)
}

/// [`element`] compiled into its caller: the loops of `for` and the apply functions read one
/// element per iteration through [`vector_element`], where a second call frame costs as much
/// as the read.
#[inline(always)]
fn element_inline<'s>(context: &mut Cx<'s>, vector: Local<'s>, index: usize) -> R<Local<'s>> {
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
/// The caller is on the interpreter's thread and holds no reference into the heap, and a root
/// of its own keeps `vector` alive until this returns. The result is rooted in the caller's
/// handle scope, as a raw allocation is.
pub unsafe fn vector_element(vector: Sexp, index: usize) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let vector = handles::held(context, vector);
        element_inline(context, vector, index).map(Local::raw)
    })
}

/// A length-one vector of `value`.
#[inline(always)]
fn scalar<'s, T: Atomic>(context: &mut Cx<'s>, value: T::Element) -> R<Local<'s>> {
    Ok(context.scalar::<T>(value)?.finish().value())
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
/// The caller is on the interpreter's thread and holds no reference into the heap, roots of its
/// own keep both vectors alive until this returns, and it may change `to` where it is.
pub unsafe fn copy_element(from: Sexp, from_index: usize, to: Sexp, to_index: usize) -> R<()> {
    handles::with(|context| {
        let from = handles::held(context, from);
        let mut to = Owned::assume(handles::held(context, to));
        store_element(context, from, from_index, &mut to, to_index)
    })
}

/// The elements of a standard logical or integer vector, or none for another type.
fn integers<'a, 's: 'a>(context: &'a Cx<'_>, vector: Local<'s>) -> &'a [i32] {
    match (vector.standard::<Integer>(), vector.standard::<Logical>()) {
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
    let Some(source) = from.standard::<T>() else {
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
    let Some(source) = from.standard::<T>() else {
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

/// An integer or logical element as a double, NA staying NA.
pub(crate) fn int_to_real(value: i32) -> f64 {
    if value == NA_INTEGER {
        NA_REAL
    } else {
        f64::from(value)
    }
}

/// Whether [`select`] reads `count` elements of `vector` one at a time instead of expanding it:
/// always for a compact sequence, whose elements are arithmetic and which GNU R leaves compact
/// when it is subset, and for another long ALTREP number vector of which the selection is a
/// small part, unless its class holds the data already (a wrapper does). A short one expands
/// cheaply, and its class keeps the expansion.
fn reads_sparsely(context: &mut Cx<'_>, vector: Local<'_>, count: usize) -> R<bool> {
    let Some(altrep) = vector.downcast::<Alt>() else {
        return Ok(false);
    };
    if !matches!(
        vector.r_type(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) {
        return Ok(false);
    }
    if compact_sequence(vector).is_some() {
        return Ok(true);
    }
    let extent = handles::length(context, vector);
    Ok(extent >= 1 << 16 && count <= extent / 8 && !handles::holds_elements(context, altrep)?)
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
            _ if reads_sparsely(context, vector, count)? => {
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
    let Some(vector) = vector.standard::<T>() else {
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

/// `object[index]` for a logical, integer, double, character or list vector whose selection
/// keeps none of its attributes (the caller checks that) and an integer, double or logical
/// subscript, gathered straight from the subscript without first resolving it to a position
/// list. `None` defers to the general path, which handles every other shape (negative or zero
/// subscripts, recycled logical subscripts, other element types). The subscript's attributes
/// play no part: the caller has already taken a matrix subscript of an array, and a factor
/// selects by its codes.
fn gather_plain<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    index: Local<'s>,
) -> R<Option<Local<'s>>> {
    if is_missing_argument(index) {
        return Ok(None);
    }
    // A selection `select` reads sparsely takes the general path.
    let count = handles::length(context, index);
    if reads_sparsely(context, object, count)? {
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
    // The elements read from the vector an ALTREP class keeps them in, which a wrapper shares
    // without copying, where the object's standard form would copy them to carry its attributes.
    let Some(object) = handles::elements_of::<T>(context, object)? else {
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
    if let Some(index) = index.standard::<Integer>() {
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
    if let Some(index) = index.standard::<Real>() {
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
    if let Some(index) = index.standard::<Logical>() {
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

/// [`length_dispatched`] for raw callers.
///
/// # Safety
///
/// The caller is on the interpreter's thread, holds no reference into the heap, and keeps
/// `object` and `env` alive until this returns.
pub unsafe fn dispatch_length(object: Sexp, env: Sexp) -> R<usize> {
    handles::with(|context| {
        let object = handles::held(context, object);
        let environment = handles::held(context, env)
            .downcast::<Environment>()
            .ok_or_else(|| error("internal error: `length` dispatched from no environment"))?;
        length_dispatched(context, object, environment)
    })
}

/// `length()` with S3 dispatch, for builtins that need an object's length.
pub fn length_dispatched<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<usize> {
    if !handles::is_object(object) {
        return Ok(handles::length(context, object));
    }
    let function = handles::primitive("length").ok_or_else(|| error("no length"))?;
    let function = context.root(function);
    let argument = Tagged {
        tag: null(),
        value: object,
    };
    let value = handles::apply(context, null(), function, &[argument], environment)?;
    let value = coerce(context, value, SexpType::Integer)?;
    Ok(handles::integer_scalar(context, value).max(0) as usize)
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
            let mut new_names = Vec::with_capacity(appended.len());
            for &name in &appended {
                new_names.push(context.root(name));
            }
            Ok((positions, new_names))
        }
        SexpType::Symbol => {
            let symbol = exact::<Symbol>(index)?;
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

/// `vector` as a standard vector of `T`, which the caller has checked its type to be.
#[inline(always)]
fn standard<'s, T: Vector>(vector: Local<'s>) -> R<Local<'s, T>> {
    vector
        .standard::<T>()
        .ok_or_else(|| error("internal error: a vector is not of the expected kind"))
}

/// `value` as a `T`, which the caller has checked its type to be.
#[inline(always)]
fn exact<'s, T: Exact>(value: Local<'s>) -> R<Local<'s, T>> {
    value
        .downcast::<T>()
        .ok_or_else(|| error("internal error: a value is not of the expected kind"))
}

/// The elements of a standard integer or double subscript, read as doubles with NA as NA.
#[derive(Clone, Copy)]
enum Numbers<'a> {
    Integer(&'a [i32]),
    Real(&'a [f64]),
}

impl<'a> Numbers<'a> {
    fn of<'s: 'a>(context: &'a Cx<'_>, vector: Local<'s>) -> R<Numbers<'a>> {
        if let Some(vector) = vector.standard::<Integer>() {
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

/// The zero-based position of `x[i]` or `x[[i]]` with one attribute-free numeric scalar `i`
/// inside the vector's bounds: the common case GNU R's byte code also answers before
/// resolving subscripts in general.
fn scalar_position(context: &mut Cx<'_>, object: Local<'_>, index: Local<'_>) -> Option<usize> {
    let vector = object.r_type().is_atomic()
        || matches!(object.r_type(), SexpType::List | SexpType::Expression);
    if !vector || object.downcast::<Alt>().is_some() || has_attributes(context, index) {
        return None;
    }
    let position = match (index.standard::<Integer>(), index.standard::<Real>()) {
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
    }
}

// `[`.

unsafe fn do_subset(call: &Call<'_>) -> R<Sexp> {
    handles::with(|context| {
        // Two untagged arguments are tested for a scalar read before the arguments are split,
        // because the split's loop and subscript list cost more than the read.
        if let [object, index] = call.args
            && object.tag == nil()
            && index.tag == nil()
        {
            let object = handles::held(context, object.value);
            let index = handles::held(context, index.value);
            // `[` on an Array makes an Array, which `subset` builds.
            if let Some(position) = scalar_position(context, object, index)
                && !has_attributes(context, object)
                && !handles::is_array(object)
                && object.r_type().is_atomic()
            {
                return element(context, object, position).map(Local::raw);
            }
        }
        let arguments = subset_args(context, call);
        let drop = match arguments.drop {
            Some(drop) => handles::logical_scalar(context, drop) != 0,
            None => true,
        };
        let call = handles::held(context, call.call);
        subset_value(
            context,
            arguments.object,
            &arguments.indices,
            drop,
            Some(call),
        )
        .map(Local::raw)
    })
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
    let Some(dim) = dim.standard::<Integer>() else {
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
                return Ok(
                    handles::list_to_pairlist(context, result, SexpType::Language, false)?.finish(),
                );
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
    let srcrefs = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.srcref),
    ));
    let srcrefs = dense::<List>(context, srcrefs)?;
    // Without names, a source reference or one dimension, the selection keeps no attribute, so
    // the object's other attributes (a matrix's `dim`, a factor's levels) need no further rules.
    if dims.as_ref().is_none_or(|dims| dims.len() != 1)
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
    let first_dimnames = |context: &mut Cx<'s>| -> R<Option<Local<'s>>> {
        let dimnames = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.dimnames),
        ));
        let first = dense::<List>(context, dimnames)?.map(|dimnames| dimnames.get(context, 0));
        Ok(first
            .filter(|first| first.r_type() != SexpType::Nil)
            .map(|first| context.root(first)))
    };
    if one_dimensional && drop && positions.len() <= 1 {
        if names.r_type() == SexpType::Nil
            && let Some(first) = first_dimnames(context)?
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
        if let Some(first) = first_dimnames(context)? {
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
    let all_dimnames = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    ));
    let dimnames = dense::<List>(context, all_dimnames)?;
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
        let of = Dimension {
            object,
            dimnames: all_dimnames,
            dimension,
            extent,
        };
        selections.push(dimension_positions(context, call, false, of, index)?);
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
    let position = match (index.standard::<Integer>(), index.standard::<Real>()) {
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

/// One dimension of the matrix or array a subscript is read against.
#[derive(Clone, Copy)]
struct Dimension<'s> {
    object: Local<'s>,
    /// The `dimnames` attribute of `object`.
    dimnames: Local<'s>,
    dimension: usize,
    extent: usize,
}

/// The positions the subscript `index` selects along one dimension, with the errors of GNU R's
/// `int_arraySubscript`. Unlike a vector, a dimension never stretches: a longer logical
/// subscript and a position past the extent are errors. An assignment (`assigning`) reports
/// every error against the replacement call it generated, which is built only for an error.
fn dimension_positions<'s>(
    context: &mut Cx<'s>,
    call: Option<Local<'s>>,
    assigning: bool,
    of: Dimension<'s>,
    index: Local<'s>,
) -> R<Positions> {
    let extent = of.extent;
    let reported_call = |context: &mut Cx<'s>| match call {
        Some(call) if assigning => handles::generated_call(context, call).map(Some),
        call => Ok(call),
    };
    let refused = if index.r_type() == SexpType::Logical && handles::length(context, index) > extent
    {
        Some("(subscript) logical subscript too long")
    } else if index.r_type() == SexpType::String && of.dimnames.r_type() == SexpType::Nil {
        Some("no 'dimnames' attribute for array")
    } else {
        None
    };
    if let Some(message) = refused {
        return Err(call_error(reported_call(context)?, message));
    }
    let names = match dense::<List>(context, of.dimnames)? {
        Some(dimnames) if of.dimension < dimnames.len() => {
            context.root(dimnames.get(context, of.dimension))
        }
        _ => null(),
    };
    // GNU R converts a double subscript of a matrix or array to integer before it resolves it,
    // so a value outside the integer range selects `NA` with a warning, where a vector
    // subscript of the same value is quietly out of bounds.
    const LIMIT: f64 = 2_147_483_648.0;
    let index = match dense::<Real>(context, index)? {
        Some(numbers)
            if numbers
                .as_slice(context)
                .iter()
                .any(|number| number.abs() >= LIMIT) =>
        {
            coerce(context, index, SexpType::Integer)?
        }
        _ => index,
    };
    let positions = match resolve_subscript(context, index, extent, names, false, call) {
        Ok((positions, _)) => positions,
        Err(Unwind::Error(mut failure))
            if assigning && call.is_some_and(|call| failure.call == call.raw()) =>
        {
            if let Some(reported) = reported_call(context)? {
                failure.call = reported.raw();
            }
            return Err(Unwind::Error(failure));
        }
        Err(other) => return Err(other),
    };
    let Some((offset, &position)) = positions
        .iter()
        .enumerate()
        .find(|&(_, &position)| position >= extent && position != NA_POSITION)
    else {
        return Ok(positions);
    };
    let call = reported_call(context)?;
    // GNU R reports the failing element: a name with its dimension, or a position without one.
    let named = index.r_type() == SexpType::String && offset < handles::length(context, index);
    let reported = match named {
        true => element(context, index, offset)?,
        false => scalar::<Real>(context, position as f64 + 1.0)?,
    };
    let bounds = OutOfBounds {
        object: of.object,
        dimension: named.then_some(of.dimension),
        reported: None,
    };
    Err(bounds_error(
        context,
        call,
        "subscript out of bounds",
        bounds,
        reported,
    ))
}

/// The dimensions of `object` and the positions `indices` select along each, for an assignment
/// of `value_count` elements, after every check GNU R's `MatrixAssign` and `ArrayAssign` make
/// before they look at the types: the number of subscripts, each subscript in turn, then the
/// lengths.
fn array_selections<'s>(
    context: &mut Cx<'s>,
    call: Local<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    value_count: usize,
) -> R<(Dims, SmallVec<[Positions; 4]>)> {
    let Some(dims) = handles::dims(context, object).filter(|dims| dims.len() == indices.len())
    else {
        return Err(handles::error_in(
            call,
            if indices.len() == 2 {
                "incorrect number of subscripts on matrix"
            } else {
                "incorrect number of subscripts"
            },
        ));
    };
    let dimnames =
        handles::get_attribute(context, object, handles::symbol(|symbols| symbols.dimnames));
    let dimnames = context.root(dimnames);
    let mut selections: SmallVec<[Positions; 4]> = SmallVec::new();
    for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
        let of = Dimension {
            object,
            dimnames,
            dimension,
            extent,
        };
        selections.push(dimension_positions(context, Some(call), true, of, index)?);
    }
    let total: usize = selections.iter().map(|selection| selection.len()).product();
    // A single value skips the NA rows, and a longer one cannot say which of its elements an
    // NA row would have taken. GNU R's matrix assignment checks this before comparing lengths,
    // and its array assignment after.
    let has_na = value_count > 1
        && selections
            .iter()
            .any(|selection| selection.contains(&NA_POSITION));
    let na_error = || handles::error_in(call, "NAs are not allowed in subscripted assignments");
    if has_na && dims.len() == 2 {
        return Err(na_error());
    }
    if total > 0 && value_count == 0 {
        return Err(handles::error_in(call, "replacement has length zero"));
    }
    if total > 0 && !total.is_multiple_of(value_count) {
        return Err(handles::error_in(
            call,
            "number of items to replace is not a multiple of replacement length",
        ));
    }
    if has_na {
        return Err(na_error());
    }
    Ok((dims, selections))
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
    // A dimension's names may be an ALTREP vector, such as deferred strings, so each is
    // expanded before the lookup reads it.
    let mut dimension_names: SmallVec<[Option<Local<'s, Str>>; 4]> = SmallVec::new();
    if !numeric && let Some(list) = dense::<List>(context, dimnames)? {
        for dimension in 0..list.len() {
            let names = context.root(list.get(context, dimension));
            dimension_names.push(dense::<Str>(context, names)?);
        }
    }
    let failure = {
        let numbers = match numeric {
            true => Some(Numbers::of(context, index)?),
            false => None,
        };
        let wanted = match numeric {
            true => None,
            false => Some(standard::<Str>(index)?.read(context)),
        };
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
                    let names = dimension_names.get(dimension).copied().flatten();
                    let found = names.map(|names| names.read(context)).and_then(|names| {
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

/// `[<-` for `m[i, ] <- value` or `a[i, j, ] <- value` (see [`slab`]), called without
/// building the general call's arguments; any other shape answers `None` and takes the builtin.
fn assign_slab<'a, 's>(context: &'a mut Cx<'s>, values: &[Local<'s>]) -> R<Option<Gc<'a>>> {
    let [object, indices @ .., value] = values else {
        return Ok(None);
    };
    let (object, value) = (*object, *value);
    let altrep = |value: Local<'s>| value.downcast::<handles::Alt>().is_some();
    if indices.len() < 2
        || handles::is_object(object)
        || altrep(object)
        || handles::is_array(object)
        || value.is(object)
        || handles::is_object(value)
        || altrep(value)
    {
        return Ok(None);
    }
    let Some(dims) = handles::dims(context, object) else {
        return Ok(None);
    };
    let Some(fixed) = slab(context, indices, &dims, object, value) else {
        return Ok(None);
    };
    let mut result = unshared(context, object)?;
    if !write_slab(context, &mut result, value, &dims, &fixed) {
        return Ok(None);
    }
    Ok(Some(result.finish().read(context)))
}

/// For an assignment into a numeric array of extents `dims` whose every subscript is empty or
/// one in-range position, given a value of the same type whose length divides the selection:
/// the position each dimension fixes, or `None` for a dimension taken whole.
fn slab<'s>(
    context: &mut Cx<'s>,
    indices: &[Local<'s>],
    dims: &[usize],
    result: Local<'s>,
    value: Local<'s>,
) -> Option<SmallVec<[Option<usize>; 4]>> {
    if indices.len() != dims.len()
        || value.r_type() != result.r_type()
        || !matches!(
            result.r_type(),
            SexpType::Real | SexpType::Integer | SexpType::Logical
        )
    {
        return None;
    }
    let mut fixed: SmallVec<[Option<usize>; 4]> = SmallVec::new();
    for (&index, &extent) in indices.iter().zip(dims) {
        if handles::is_missing_argument(index) {
            fixed.push(None);
        } else {
            fixed.push(Some(matrix_index(context, index, extent)?));
        }
    }
    let selected: usize = fixed
        .iter()
        .zip(dims)
        .map(|(position, &extent)| if position.is_some() { 1 } else { extent })
        .product();
    let value_count = handles::length(context, value);
    (value_count > 0 && selected > 0 && selected.is_multiple_of(value_count)).then_some(fixed)
}

/// Writes the slab [`slab`] found. False, having written nothing, unless both are standard
/// vectors of one numeric type and `value` is not `result` itself.
fn write_slab<'s>(
    context: &mut Cx<'s>,
    result: &mut Owned<'s>,
    value: Local<'s>,
    dims: &[usize],
    fixed: &[Option<usize>],
) -> bool {
    fn typed<'s, T: Atomic>(
        context: &mut Cx<'s>,
        result: &mut Owned<'s>,
        value: Local<'s>,
        dims: &[usize],
        fixed: &[Option<usize>],
    ) -> Option<bool> {
        let (result, value) = (result.narrow::<T>()?, value.standard::<T>()?);
        let mut write = result.write(context);
        let (array, reader) = write.split();
        scatter_slab(array, reader.slice(value)?, dims, fixed);
        Some(true)
    }
    typed::<Real>(context, result, value, dims, fixed)
        .or_else(|| typed::<Integer>(context, result, value, dims, fixed))
        .or_else(|| typed::<Logical>(context, result, value, dims, fixed))
        .unwrap_or(false)
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

unsafe fn do_subset2(call: &Call<'_>) -> R<Sexp> {
    handles::with(|context| {
        // The scalar read comes before the split, as in `do_subset`.
        if let [object, index] = call.args
            && object.tag == nil()
            && index.tag == nil()
        {
            let object = handles::held(context, object.value);
            let index = handles::held(context, index.value);
            if let Some(position) = scalar_position(context, object, index)
                && !handles::is_object(object)
            {
                // The position is inside a vector with storage, so a list's element is read
                // where it lies.
                if let Some(list) = object.standard::<List>() {
                    return Ok(list.get(context, position).raw());
                }
                if let Some(expressions) = object.standard::<Expression>() {
                    return Ok(expressions.get(context, position).raw());
                }
                return element(context, object, position).map(Local::raw);
            }
        }
        let arguments = subset_args(context, call);
        let object = arguments.object;
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

/// Which of GNU R's name lookups a subscript goes through.
#[derive(Clone, Copy)]
enum Lookup<'s> {
    /// `[[`, GNU R's `get1index`: an `NA` name or subscript matches nothing. `exact` is the
    /// operator's argument: `Some(false)` accepts a unique partial match, and `None` also
    /// warns about each partial match against `call`.
    Element {
        exact: Option<bool>,
        call: Option<Local<'s>>,
    },
    /// `$`: a unique partial match counts, and an `NA` name is the text "NA".
    Field,
    /// `[[<-`, GNU R's `OneIndex`: exact, and an `NA` name or subscript is the text "NA".
    Store,
}

/// The position `name` matches among `names`. Names compare as text, so a latin1-marked name
/// matches its UTF-8 spelling. Names that are an ALTREP vector, such as the deferred strings
/// `names(x) <- 1:2` stores, are expanded first.
fn name_position<'s>(
    context: &mut Cx<'s>,
    names: Local<'s>,
    name: Local<'_, Chars>,
    lookup: Lookup<'s>,
) -> R<Option<usize>> {
    let Some(names) = dense::<Str>(context, names)? else {
        return Ok(None);
    };
    let name = name.read(context);
    let na_is_text = !matches!(lookup, Lookup::Element { .. });
    if is_na(name) && !na_is_text {
        return Ok(None);
    }
    let candidates = names.read(context);
    let spells_na = |text: Gc<'_, Chars>| is_na(text) || text.bytes() == b"NA";
    let exact_match = if na_is_text && spells_na(name) {
        (0..candidates.len()).find(|&index| spells_na(candidates.get(index)))
    } else {
        (0..candidates.len()).find(|&index| same_text(candidates.get(index), name))
    };
    let (partial, warn_in) = match lookup {
        Lookup::Element { exact, call } => (exact != Some(true), exact.is_none().then_some(call)),
        Lookup::Field => (true, None),
        Lookup::Store => (false, None),
    };
    if exact_match.is_some() || !partial {
        return Ok(exact_match);
    }
    let text = handles::text(name).into_owned();
    let partial_matches: SmallVec<[usize; 2]> = (0..candidates.len())
        .filter(|&index| {
            let candidate = candidates.get(index);
            (na_is_text || !is_na(candidate)) && handles::text(candidate).starts_with(&*text)
        })
        .take(2)
        .collect();
    if let Some(call) = warn_in {
        for (rank, &position) in partial_matches.iter().enumerate() {
            let matched = handles::text(names.get(context, position)).into_owned();
            // GNU R names the call once: the warning about a second match has none.
            if rank > 0 {
                let message = format!("further partial match of '{text}' to '{matched}'");
                handles::warning_in(context, null(), message)?;
                continue;
            }
            let message = format!("partial match of '{text}' to '{matched}'");
            match call {
                Some(call) => handles::warning_in(context, call, message)?,
                None => handles::warning(context, message)?,
            }
        }
    }
    Ok(match partial_matches[..] {
        [position] => Some(position),
        _ => None,
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
        let dim = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.dim),
        ));
        dim.r_type() == SexpType::Integer && handles::length(context, dim) == indices.len()
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
        handles::reaching_environment(context)?;
        // `dense`, not `downcast`: `as.character(1.5)` is a deferred string vector.
        let name = match indices.first() {
            Some(&index) => handles::dense::<Str>(context, index)?,
            None => None,
        }
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
        let dimnames = dense::<List>(context, dimnames)?;
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
            let index = symbol_text(context, index)?;
            let position = single_index(context, index, extent, names, exact, call, bounds)?;
            offset += position * stride;
            stride *= extent;
        }
        return element(context, object, offset);
    }
    let index = symbol_text(context, indices[0])?;
    let index = materialize(context, index)?;
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
    if list && let Some(wanted) = index.standard::<Str>() {
        let wanted = context.root(wanted.get(context, 0));
        // A list has no element of that name: `[[` gives `NULL`, unlike for atomic vectors.
        if wanted.bytes(context).is_empty() {
            return Ok(null());
        }
        return match name_position(context, names, wanted, Lookup::Element { exact, call })? {
            Some(position) => element(context, object, position),
            None => Ok(null()),
        };
    }
    if list {
        // A list gives `NULL` for a missing or infinite subscript, where an atomic vector
        // reports it out of bounds; a large finite subscript is out of bounds for both.
        let absent = match (index.standard::<Real>(), integers(context, index)) {
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

/// The position a `[[` subscript that is not a symbol selects, with GNU R's errors.
fn single_index<'s>(
    context: &mut Cx<'s>,
    index: Local<'s>,
    extent: usize,
    names: Local<'s>,
    exact: Option<bool>,
    call: Option<Local<'s>>,
    bounds: OutOfBounds<'s>,
) -> R<usize> {
    let matrix = bounds.dimension.is_some();
    match one_position(
        context,
        index,
        extent,
        names,
        Lookup::Element { exact, call },
        matrix,
    )? {
        Single::At(position) => Ok(position),
        Single::Outside => Err(bounds_error(
            context,
            call,
            "subscript out of bounds",
            bounds,
            index,
        )),
        Single::Invalid(message) => Err(call_error(call, message)),
    }
}

/// A symbol subscript as the string that names the same element, and any other subscript
/// unchanged.
fn symbol_text<'s>(context: &mut Cx<'s>, index: Local<'s>) -> R<Local<'s>> {
    let Some(symbol) = index.downcast::<Symbol>() else {
        return Ok(index);
    };
    let name = context.root(symbol.read(context).name());
    let mut text = context.alloc::<Str>(1)?;
    text.set(context, 0, name);
    Ok(text.finish().value())
}

/// What one `[[` subscript selects among the elements of a vector or of one dimension.
enum Single {
    At(usize),
    /// It names no element or lies past the end.
    Outside,
    /// It cannot name a single element; the text is GNU R's message, which the caller reports
    /// against the call its operation names.
    Invalid(String),
}

/// GNU R's `get1index`: what `index`, which is not a symbol, selects among `extent` elements
/// with `names`. `matrix` says the subscript indexes one dimension.
fn one_position<'s>(
    context: &mut Cx<'s>,
    index: Local<'s>,
    extent: usize,
    names: Local<'s>,
    lookup: Lookup<'s>,
    matrix: bool,
) -> R<Single> {
    let index = materialize(context, index)?;
    let count = handles::length(context, index);
    if count != 1 {
        let amount = if count == 0 { "less" } else { "more" };
        return Ok(Single::Invalid(format!(
            "attempt to select {amount} than one element in get1index"
        )));
    }
    let inside = |position: Option<usize>| position.map_or(Single::Outside, Single::At);
    // Before 4.6.2 a matrix subscript never counts from the end, so a negative one selects
    // nothing; 4.6.2 applies the vector rule to each extent (PR#19116).
    let counts_from_end = !matrix || crate::system::r_at_least_release(4, 6, 2);
    match index.r_type() {
        SexpType::String => {
            let wanted = context.root(standard::<Str>(index)?.get(context, 0));
            // An empty name means "no name", so it never selects an element.
            if wanted.bytes(context).is_empty() {
                return Ok(Single::Outside);
            }
            Ok(inside(name_position(context, names, wanted, lookup)?))
        }
        SexpType::Integer | SexpType::Logical => {
            let value = integers(context, index)[0];
            if value == NA_INTEGER {
                return Ok(Single::Outside);
            }
            if value < 0 && extent == 2 && value >= -2 && counts_from_end {
                return Ok(Single::At(if value == -1 { 1 } else { 0 }));
            }
            if value <= 0 {
                let amount = if value < 0 && extent > 1 && counts_from_end {
                    "more"
                } else {
                    "less"
                };
                return Ok(Single::Invalid(format!(
                    "attempt to select {amount} than one element in integerOneIndex"
                )));
            }
            Ok(inside(
                (value as usize <= extent).then(|| value as usize - 1),
            ))
        }
        SexpType::Real => {
            let value = standard::<Real>(index)?.as_slice(context)[0];
            if value.is_nan() {
                return Ok(Single::Outside);
            }
            let value = value.trunc();
            if value < 0.0 {
                if extent == 2 && value >= -2.0 && counts_from_end {
                    return Ok(Single::At(if value == -1.0 { 1 } else { 0 }));
                }
                return Ok(Single::Invalid(
                    "invalid negative subscript in get1index <real>".into(),
                ));
            }
            if value == 0.0 {
                return Ok(Single::Invalid(
                    "attempt to select less than one element in get1index <real>".into(),
                ));
            }
            Ok(inside((value <= extent as f64).then(|| value as usize - 1)))
        }
        kind => Ok(Single::Invalid(format!(
            "invalid subscript type '{}'",
            kind.name()
        ))),
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
pub(crate) fn argument_name<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Chars>> {
    // A promise comes from `NextMethod` and stands for the name the method received.
    let value = handles::force(context, value)?;
    if let Some(symbol) = value.downcast::<Symbol>() {
        return Ok(context.root(symbol.read(context).name()));
    }
    if value.r_type() == SexpType::String
        && let Some(text) = dense::<Str>(context, value)?
        && !text.is_empty()
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

/// [`dollar_dispatch`] for tier 0's field instruction, in the instruction's handle scope. `name`
/// is a `CHARSXP`.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap. Its frame
/// holds every argument until the instruction ends, and it releases what this roots. `env` is
/// an environment.
pub unsafe fn dollar_value(object: Sexp, name: Sexp, call: Sexp, env: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        let name = exact::<Chars>(handles::held(context, name))?;
        let call = handles::held(context, call);
        let environment = exact::<Environment>(handles::held(context, env))?;
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
            handles::dispatch_builtin(context, generic, call, arguments, environment)?
        {
            return Ok(value);
        }
    }
    dollar_field(context, object, name, Some(call))
}

/// `object$name` without dispatch, for tier 0's field instruction, which passes the unbound
/// marker for the running call. `name` is a `CHARSXP`.
///
/// # Safety
///
/// As for [`dollar_value`].
pub unsafe fn dollar(object: Sexp, name: Sexp, call: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        let name = exact::<Chars>(handles::held(context, name))?;
        let call = (call != unbound()).then(|| handles::held(context, call));
        dollar_field(context, object, name, call).map(Local::raw)
    })
}

/// `object$name` without dispatch.
///
/// An environment's variable is read in the caller's frame: it is what tier 0's field
/// instruction reads most, and the other kinds need registers that would be saved for it too.
#[inline(always)]
fn dollar_field<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    name: Local<'s, Chars>,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    if object.r_type() == SexpType::Environment {
        return Ok(handles::frame_value(context, object, name)?.unwrap_or_else(null));
    }
    field_of_value(context, object, name, call)
}

/// [`dollar_field`] for everything but an environment.
fn field_of_value<'s>(
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
            let Some(position) = name_position(context, names, name, Lookup::Field)? else {
                return Ok(null());
            };
            let value = item(context, object, position)?;
            // A complex assignment through a partial name, `v$mi[[1]] <- 2`, assigns a new `mi`
            // element, so the `misc` element it read must not be modified in place.
            let exact = dense::<Str>(context, names)?
                .is_some_and(|names| names.get(context, position).bytes() == name.bytes(context));
            if !exact {
                handles::mark_shared(value);
            }
            Ok(value)
        }
        SexpType::Pairlist | SexpType::Language => {
            let list = pairlist_to_list(context, object)?;
            field_of_value(context, list, name, call)
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

/// `object@name`, the special: the subject is evaluated and the slot name is read from the
/// second argument's expression.
fn do_at<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let object = handles::evaluate(context, arguments.value(0), environment)?;
    let slot = slot_name(context, arguments.value(1))?;
    at(
        context,
        object,
        slot.value(),
        arguments.expression(),
        |_| Ok(environment),
    )
}

/// `object@slot` for an evaluated `object` and the symbol `slot`. Only S4 objects have slots:
/// on any other object `@` reads `.Data` (the object itself) or dispatches an S3 `@` method
/// from `environment`, which is asked for only then, and otherwise is an error.
pub(crate) fn at<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Local<'s>,
    call: Local<'s>,
    environment: impl FnOnce(&mut Cx<'s>) -> R<Local<'s, Environment>>,
) -> R<Local<'s>> {
    let slot = exact::<Symbol>(slot)?;
    // Since R 4.3 `@` is an internal generic for S3 objects, which is how S7 reads
    // properties.
    if handles::is_object(object) && !is_s4(object) {
        let name = context.root(slot.read(context).name());
        let mut text = context.alloc::<Str>(1)?;
        text.set(context, 0, name);
        let arguments = [object, text.finish().value()];
        let generic = symbol(|symbols| symbols.at);
        let environment = environment(context)?;
        if let Some(value) =
            handles::dispatch_builtin(context, generic, call, arguments, environment)?
        {
            return Ok(value);
        }
    }
    if !is_s4(object) && slot.raw() != symbol(|symbols| symbols.dot_data).raw() {
        let class = crate::dispatch::class_of(context, object)?;
        let class = handles::first_string(context, class).map(|class| class.into_owned());
        return Err(call_error(
            Some(call),
            format!(
                "no applicable method for `@` applied to an object of class \"{}\"",
                class.unwrap_or_default()
            ),
        ));
    }
    // GNU R reports a missing slot against the function that runs, not against the read.
    slot_of(context, object, slot, None)
}

/// `R_do_slot`: an S4 slot, which is an attribute, with `.Data` and `.S3Class` special. Raw
/// callers pass the unbound marker for the running call.
///
/// # Safety
///
/// As for [`subset`], with `symbol` a symbol.
pub unsafe fn get_slot(object: Sexp, symbol: Sexp, call: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let object = handles::adopt(context, object);
        let slot = exact::<Symbol>(handles::adopt(context, symbol))?;
        let call = (call != unbound()).then(|| handles::adopt(context, call));
        slot_of(context, object, slot, call).map(Local::raw)
    })
}

/// [`get_slot`] on handles.
fn slot_of<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Local<'s, Symbol>,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    if slot.raw() == symbol(|symbols| symbols.dot_data).raw()
        && let Some(function) = methods_value(context, symbol(|symbols| symbols.get_data_part))?
    {
        return handles::call_value(context, function, &[object]);
    }
    slot_value(context, object, slot, call)
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
        if null_slot {
            return Ok(null());
        }
        let value = context.root(value);
        if slot.raw() == symbol(|symbols| symbols.row_names).raw() {
            return crate::attribute_access::expand_row_names(context, value);
        }
        return Ok(value);
    }
    if slot.raw() == symbol(|symbols| symbols.dot_s3_class).raw() {
        return crate::dispatch::class_of(context, object);
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

/// The value of `name` as the methods package's namespace sees it, or `None` when the package
/// is not loaded or does not bind it.
fn methods_value<'s>(context: &mut Cx<'s>, name: Gc<'static, Symbol>) -> R<Option<Local<'s>>> {
    // SAFETY: the namespace is rooted for the session, the lookup may force a promise, which
    // `&mut Cx` permits, and its result is rooted before anything else allocates.
    unsafe {
        let methods = crate::interp().globals.methods_namespace;
        if methods == nil() {
            return Ok(None);
        }
        let value = eval::find_var(name.raw(), methods)?;
        if value == unbound() {
            return Ok(None);
        }
        Ok(Some(handles::adopt(context, value_of(value)?)))
    }
}

/// GNU R's `[[ ]] subscript out of bounds` error for `index` into `object`, naming the
/// dimension a matrix subscript belongs to.
fn subscript2_out_of_bounds<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    dimension: Option<usize>,
    index: Local<'s>,
) -> Unwind {
    let bounds = OutOfBounds {
        object,
        dimension,
        reported: None,
    };
    bounds_error(
        context,
        None,
        "[[ ]] subscript out of bounds",
        bounds,
        index,
    )
}

unsafe fn do_at_assign(call: &Call<'_>) -> R<Sexp> {
    // SAFETY: each evaluated argument is adopted before anything else allocates or runs R code,
    // the name is a symbol, which the heap keeps, and the result is rooted before the scope
    // ends.
    unsafe {
        handles::with_result(|context| {
            let env = call.env()?;
            let object = handles::adopt(context, eval::eval(call.arg(0), env)?);
            let symbol = slot_symbol(call.arg(1))?;
            let value = handles::adopt(context, eval::eval(call.arg(2), env)?);
            if handles::is_object(object) && !is_s4(object) {
                let args = [
                    Arg {
                        tag: nil(),
                        value: object.raw(),
                    },
                    Arg {
                        tag: nil(),
                        value: values::scalar_string_charsxp(symbol.car())?,
                    },
                    Arg {
                        tag: symbols().value,
                        value: value.raw(),
                    },
                ];
                if let Some(result) =
                    crate::dispatch::dispatch_builtin(symbols().at_assign, call.call, &args, env)?
                {
                    return Ok(result);
                }
            }
            let symbol = handles::permanent(exact::<Symbol>(handles::held(context, symbol))?);
            Ok(checked_slot_assign(context, object, symbol, value)?.raw())
        })
    }
}

/// `@<-` on evaluated arguments, for `object@name <- value` compiled to registers: the object,
/// the slot's name as a symbol or a string, and the value. An S3 object answers `None`, because
/// it may dispatch to a method, which the special looks up in its caller's environment.
fn at_assign_values<'a, 's>(context: &'a mut Cx<'s>, values: &[Local<'s>]) -> R<Option<Gc<'a>>> {
    let &[object, name, value] = values else {
        return Ok(None);
    };
    if handles::is_object(object) && !is_s4(object) {
        return Ok(None);
    }
    let slot = handles::permanent(slot_name(context, name)?);
    Ok(Some(
        checked_slot_assign(context, object, slot, value)?.read(context),
    ))
}

/// `object@symbol <- value` once dispatch has declined: checked, then set on `object` or a copy.
fn checked_slot_assign<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Gc<'static, Symbol>,
    value: Local<'s>,
) -> R<Local<'s>> {
    check_at_assignment(context, object, slot, value)?;
    let mut object = unshared(context, object)?;
    store_slot(context, &mut object, slot, value)
}

/// The methods package's check of `object@symbol <- value` against the class definition, which
/// uses the implicit class of an object without a class attribute, so `x@a <- 1` on a list
/// fails.
fn check_at_assignment<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Gc<'static, Symbol>,
    value: Local<'s>,
) -> R<()> {
    let Some((check, answered)) = slot_check(context)? else {
        return Ok(());
    };
    if answered && slot_accepts(context, object, slot, value)? {
        return Ok(());
    }
    let class = crate::dispatch::class_of(context, object)?;
    let value_class = crate::dispatch::class_of(context, value)?;
    let mut name_string = context.alloc::<Str>(1)?;
    name_string.set(context, 0, slot.name());
    let name_string = name_string.finish().value();
    handles::call_value(context, check, &[class, name_string, value_class])?;
    Ok(())
}

/// GNU R's `checkAt` of `check_slot_assign`: the methods package's `checkAtAssignment` as the
/// first slot assignment finds it, which every later one calls whatever the namespace binds by
/// then, and whether [`slot_accepts`] answers for it. `None` while the package is not loaded or
/// binds no such function.
fn slot_check<'s>(context: &mut Cx<'s>) -> R<Option<(Local<'s>, bool)>> {
    let recorded = crate::interp().globals.check_at_assignment;
    let check = if recorded == nil() {
        let name = symbol(|symbols| symbols.check_at_assignment);
        let Some(check) = methods_value(context, name)? else {
            return Ok(None);
        };
        if !matches!(
            check.r_type(),
            SexpType::Closure | SexpType::Builtin | SexpType::Special
        ) {
            return Ok(None);
        }
        crate::interp().globals.check_at_assignment = check.raw();
        check
    } else {
        // SAFETY: the session's globals root the function.
        unsafe { handles::adopt(context, recorded) }
    };
    let answered =
        fast_paths_enabled() && check.raw() == crate::interp().globals.answered_slot_check;
    Ok(Some((check, answered)))
}

/// Whether `checkAtAssignment(class(object), slot, class(value))` returns `TRUE` from the class
/// cache alone: the cache holds one definition of the object's class, and the class it declares
/// for the slot is `ANY`, the first class of the value, or a class the cached definition of the
/// value's class lists in its `contains`. `false` leaves the answer to the R function, which
/// searches further, looks at the declared class's subclasses and raises the errors. A value of
/// the declared class itself is accepted without an allocation.
fn slot_accepts<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Gc<'static, Symbol>,
    value: Local<'s>,
) -> R<bool> {
    let class_symbol = symbol(|symbols| symbols.class);
    let class = context.root(get_attribute(context, object, class_symbol));
    let Some(table) = methods_value(context, symbol(|symbols| symbols.class_table))? else {
        return Ok(false);
    };
    if table.r_type() != SexpType::Environment {
        return Ok(false);
    }
    let Some(definition) = cached_definition(context, table, class)? else {
        return Ok(false);
    };
    let slots = get_attribute(context, definition, symbol(|symbols| symbols.slots));
    let Some(declared) = named_item(context, slots, slot.name().bytes())
        .and_then(|declared| declared.standard::<Str>())
    else {
        return Ok(false);
    };
    if declared.len() != 1 || is_na(declared.get(0)) {
        return Ok(false);
    }
    let declared = context.root(declared.get(0));
    if declared.read(context).bytes() == b"ANY" {
        return Ok(true);
    }
    let implicit = crate::dispatch::implicit_class_name(context, value);
    let same = match implicit {
        Some(name) => name.as_bytes() == declared.read(context).bytes(),
        None => get_attribute(context, value, class_symbol)
            .standard::<Str>()
            .is_some_and(|class| class.get(0).bytes() == declared.read(context).bytes()),
    };
    if same {
        return Ok(true);
    }
    // An implicit class is one name with no package, so its definition is found by that name
    // without building the class vector.
    let value_definition = match implicit {
        Some(name) => {
            let name = handles::install_bytes(context, name.as_bytes())?;
            definition_in(context, table, name, None)?
        }
        None => {
            let value_class = context.root(get_attribute(context, value, class_symbol));
            cached_definition(context, table, value_class)?
        }
    };
    let Some(value_definition) = value_definition else {
        return Ok(false);
    };
    let contains = get_attribute(
        context,
        value_definition,
        symbol(|symbols| symbols.contains),
    );
    let extension = named_item(context, contains, declared.read(context).bytes());
    Ok(extension.is_some_and(|extension| extension.r_type() != SexpType::Nil))
}

/// `getClassDef(class)` where the class cache `table` answers it with one definition, as
/// `methods_core::cached_class` does for the native class definitions. `None` when the R code
/// would look further: the cache has no entry under the class's first name, the entry is the
/// plain list of several packages' definitions, from which the R code picks one by where it is
/// called from, or the entry belongs to another package than the class names.
fn cached_definition<'s>(
    context: &mut Cx<'s>,
    table: Local<'s>,
    class: Local<'s>,
) -> R<Option<Local<'s>>> {
    let Some(class) = class.standard::<Str>() else {
        return Ok(None);
    };
    if class.is_empty() {
        return Ok(None);
    }
    if is_na(class.read(context).get(0)) {
        return Ok(None);
    }
    let name = handles::install_element(context, class, 0)?;
    definition_in(context, table, name, Some(class))
}

/// The definition the class cache `table` holds under `name`, when it is one class
/// definition and, if `class` names a package, that package's.
fn definition_in<'s>(
    context: &mut Cx<'s>,
    table: Local<'s>,
    name: impl Pointer<Symbol>,
    class: Option<Local<'s, Str>>,
) -> R<Option<Local<'s>>> {
    let Some(definition) = handles::frame_symbol_value(context, table, name)? else {
        return Ok(None);
    };
    if !is_s4(definition) {
        return Ok(None);
    }
    let package = symbol(|symbols| symbols.package);
    if let Some(class) = class
        && let Some(wanted) = get_attribute(context, class, package).standard::<Str>()
        && let Some(defined) = get_attribute(context, definition, package).standard::<Str>()
        && !wanted.is_empty()
        && defined.len() == 1
        && wanted.get(0).bytes() != defined.get(0).bytes()
    {
        return Ok(None);
    }
    Ok(Some(definition))
}

/// `list[[name]]` for a plain list with names: the first item under `name`, or `None` when
/// `list` is another kind of value or has no such item.
fn named_item<'a>(context: &'a Cx<'_>, list: Gc<'a>, name: &[u8]) -> Option<Gc<'a>> {
    if handles::is_object(list) {
        return None;
    }
    let list = list.standard::<List>()?;
    let handles::Names::Standard(names) = handles::names(context, list) else {
        return None;
    };
    (0..names.len().min(list.len()))
        .find(|&index| !is_na(names.get(index)) && names.get(index).bytes() == name)
        .map(|index| list.get(index))
}

/// `R_do_slot_assign`.
///
/// # Safety
///
/// On the interpreter's thread, with every argument a live object that the caller keeps rooted
/// until it returns and `symbol` a symbol. The result is rooted in the caller's handle scope.
pub unsafe fn set_slot(object: Sexp, symbol: Sexp, value: Sexp) -> R<Sexp> {
    // SAFETY: the caller's roots hold the arguments, and the result is rooted before anything
    // else allocates.
    unsafe {
        handles::with_result(|context| {
            let object = handles::held(context, object);
            let slot = handles::permanent(exact::<Symbol>(handles::held(context, symbol))?);
            let value = handles::held(context, value);
            let mut object = unshared(context, object)?;
            Ok(store_slot(context, &mut object, slot, value)?.raw())
        })
    }
}

/// Sets a slot of `object` itself, even when it is shared. The methods package's `slot<-`
/// relies on that: its default `initialize` method calls it as a plain function on an object
/// it has just copied, and discards the result.
///
/// # Safety
///
/// As for [`set_slot`]; the write lands in `object` whoever else refers to it, which the
/// caller accepts.
pub unsafe fn assign_slot(object: Sexp, symbol: Sexp, value: Sexp) -> R<Sexp> {
    // SAFETY: the caller's roots hold the arguments, its contract accepts a write into a shared
    // `object`, and the result is rooted before anything else allocates.
    unsafe {
        handles::with_result(|context| {
            let object = handles::held(context, object);
            let slot = handles::permanent(exact::<Symbol>(handles::held(context, symbol))?);
            let value = handles::held(context, value);
            let mut object = Owned::assume(object);
            Ok(store_slot(context, &mut object, slot, value)?.raw())
        })
    }
}

/// Sets the slot `symbol` of the owned `object` to `value`: `.Data` through the methods
/// package's `setDataPart`, and `NULL` as the methods package's placeholder symbol.
fn store_slot<'s>(
    context: &mut Cx<'s>,
    object: &mut Owned<'s>,
    slot: Gc<'static, Symbol>,
    value: Local<'s>,
) -> R<Local<'s>> {
    if slot.is(symbol(|symbols| symbols.dot_data))
        && let Some(function) = methods_value(context, symbol(|symbols| symbols.set_data_part))?
    {
        return handles::call_value(context, function, &[object.get(), value]);
    }
    let stored = if value.r_type() == SexpType::Nil {
        let placeholder = handles::install_bytes(context, b"\x01NULL\x01")?;
        context.root(placeholder).value()
    } else {
        value
    };
    set_attribute(context, object, slot, stored)?;
    Ok(object.get())
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

/// `x[i] <- value` and `x[i, j] <- value`.
///
/// # Safety
///
/// On the interpreter's thread, with every argument a live object that the caller keeps rooted
/// until it returns. The result is rooted in the caller's handle scope.
pub unsafe fn subassign(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    // SAFETY: the caller's roots hold the arguments for the whole call, and the result is
    // rooted before anything else allocates.
    unsafe {
        handles::with_result(|context| {
            let object = handles::held(context, object);
            let indices: SmallVec<[Local<'_>; 4]> = indices
                .iter()
                .map(|&index| handles::held(context, index))
                .collect();
            let value = handles::held(context, value);
            let call = handles::held(context, call);
            Ok(assign_subset(context, object, &indices, value, call)?.raw())
        })
    }
}

/// Whether `value` is an ALTREP object.
#[inline(always)]
fn altrep(value: Local<'_>) -> bool {
    value.downcast::<handles::Alt>().is_some()
}

/// Whether `x[i] <- value` leaves the atomic vector `object` exactly as it is: the subscript
/// selects nothing and `value` needs no change of type. GNU R then returns `x` itself, so
/// `x[FALSE] <- 1` keeps an ALTREP vector, such as a wrapper, unexpanded.
fn selects_nothing<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    value: Local<'s>,
) -> R<bool> {
    let &[index] = indices else {
        return Ok(false);
    };
    let empty = match index.r_type() {
        SexpType::Nil => true,
        // A longer logical index stretches the vector even when it selects nothing.
        SexpType::Logical => {
            handles::length(context, index) <= handles::length(context, object)
                && handles::dense::<Logical>(context, index)?
                    .is_some_and(|flags| flags.as_slice(context).iter().all(|&flag| flag == 0))
        }
        SexpType::Integer | SexpType::Real | SexpType::String => {
            handles::length(context, index) == 0
        }
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
    let (object_kind, value_kind) = (object.r_type(), value.r_type());
    let fits = value_kind == object_kind
        || (atomic(value_kind)
            && atomic(object_kind)
            && coerce::higher_type(object_kind, value_kind) == object_kind);
    Ok(empty
        && fits
        && !handles::is_object(value)
        && index.attributes(context).r_type() == SexpType::Nil)
}

pub(crate) fn assign_subset<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    value: Local<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    // `s[o] <- s` writes into the vector it reads from, so the value must be a copy, as in GNU
    // R's `SubassignN`; Matrix reorders its results this way.
    let value = if value.is(object) {
        handles::shallow_copy(context, value)?.finish()
    } else {
        value
    };
    // A value an Array cannot hold, such as an NA, makes the result a plain R vector.
    let object = if handles::is_array(object) {
        match handles::array_subassign(context, object, indices, value, call, false)? {
            Some(result) => return Ok(result),
            None => handles::shallow_copy(context, object)?.finish(),
        }
    } else {
        object
    };
    if altrep(object) && selects_nothing(context, object, indices, value)? {
        return Ok(object);
    }
    let object = handles::materialize(context, object)?;
    let mut value = handles::materialize(context, value)?;
    // A pairlist value assigns its elements into a list, as a list would, but has no atomic
    // counterpart to coerce to.
    // An S4 object that is not a vector goes in as its `as.vector()`, which dispatches, so a
    // class's own method decides what its elements are.
    if value.r_type() == SexpType::Object && object.r_type() != SexpType::List {
        let function = handles::install_bytes(context, b"as.vector")?;
        let function = context.root(function).value();
        let quoted = handles::quoted(context, value)?;
        value = handles::call_value(context, function, &[quoted])?;
    } else if value.r_type() == SexpType::Object {
        handles::warning_in(
            context,
            call,
            "implicit list embedding of S4 objects is deprecated",
        )?;
    }
    // GNU R's matrix and array assignments check the subscripts and the value's length before
    // its type. For a value that is not a vector (a function, an environment, a symbol, a call,
    // `NULL`) GNU R reads a length anyway, and what it finds exceeds any selection, so only an
    // empty selection takes it.
    let lengthless = !value.r_type().is_vector() && value.r_type() != SexpType::Record;
    if lengthless && indices.len() >= 2 && object.r_type().is_vector() {
        array_selections(context, call, object, indices, usize::MAX)?;
    }
    // GNU R coerces `NULL` to the value's type before it looks at a subscript, which fails for
    // every type that is not a vector's, except for an environment that binds nothing, whose
    // length of zero makes the assignment a no-op.
    if lengthless && value.r_type() != SexpType::Nil && object.r_type() == SexpType::Nil {
        return match value.r_type() {
            SexpType::Environment if handles::length(context, value) == 0 => Ok(object),
            SexpType::Pairlist => Err(not_subsettable_error(context, Some(call), object)),
            SexpType::Language => Err(error("unimplemented type 'NULL' in 'coercePairList'\n")),
            kind => Err(error(format!(
                "invalid type/length ({}/0) in vector allocation",
                kind.name()
            ))),
        };
    }
    // GNU R assigns a call's elements, as it does a pairlist's, so `f[2] <- quote(g(h))` stores
    // the symbol `g`.
    let value = if matches!(value.r_type(), SexpType::Pairlist | SexpType::Language) {
        if !matches!(
            object.r_type(),
            SexpType::List
                | SexpType::Expression
                | SexpType::Pairlist
                | SexpType::Language
                | SexpType::Nil
        ) {
            return Err(incompatible_types(
                handles::type_name(object),
                handles::type_name(value),
                "subassignment type fix",
            ));
        }
        pairlist_to_list(context, value)?
    } else if matches!(object.r_type(), SexpType::List | SexpType::Expression)
        && value.r_type() == SexpType::Symbol
    {
        // GNU R coerces the value to a list first, and a symbol cannot be coerced. R 4.6.1
        // makes that an error; earlier releases warn, and the symbol becomes `NULL` and deletes
        // the selected elements.
        if crate::system::r_at_least_release(4, 6, 1) {
            return Err(error("'symbol' object cannot be coerced to type 'list'"));
        }
        handles::warning_in(
            context,
            call,
            "'symbol' object cannot be coerced to type 'list'",
        )?;
        null()
    } else {
        value
    };
    if matches!(object.r_type(), SexpType::Pairlist | SexpType::Language) {
        let list = pairlist_to_list(context, object)?;
        let result = assign_subset(context, list, indices, value, call)?;
        // A call stays a call with its attributes (a formula keeps its class and environment),
        // but a pairlist becomes a list, as in GNU R.
        if object.r_type() != SexpType::Language {
            return Ok(result);
        }
        let result = standard::<List>(result)?;
        let mut rebuilt = handles::list_to_pairlist(context, result, SexpType::Language, true)?;
        handles::copy_most_attributes(context, object, &mut rebuilt, true)?;
        return Ok(rebuilt.finish());
    }
    // GNU R leaves `NULL` given an empty value alone whatever the index, so
    // `x <- NULL; x[1] <- character(0)` is not the "replacement has length zero" error. An
    // empty vector given `NULL` still stretches to a longer subscript, so
    // `x <- list(); x[FALSE] <- NULL` leaves `list(NULL)`. An empty vector given an empty
    // vector is handled below, where it may still be coerced.
    if object.r_type() == SexpType::Nil
        && (value.r_type() == SexpType::Nil
            || (value.r_type().is_vector() && handles::length(context, value) == 0))
    {
        return Ok(object);
    }
    let object = if object.r_type() == SexpType::Nil {
        empty_vector(context, value.r_type())?
    } else {
        object
    };
    if !object.r_type().is_vector() {
        return Err(not_subsettable_error(context, Some(call), object));
    }
    if value.r_type() == SexpType::Nil
        && matches!(object.r_type(), SexpType::List | SexpType::Expression)
        && indices.len() <= 1
    {
        return delete_elements(context, object, indices, call);
    }
    // `NULL` has no type to mismatch: a raw vector takes it as it takes a raw value, so the
    // check of its length reports it.
    let value_kind = match (value.r_type(), object.r_type()) {
        (SexpType::Nil, SexpType::Raw) => SexpType::Raw,
        (SexpType::Nil, _) => SexpType::Logical,
        (kind, _) => kind,
    };
    // An empty vector takes an empty replacement of its own type, or an empty
    // list, unchanged whatever the index selects: an empty list must not turn
    // an atomic vector into a list.
    if handles::length(context, object) == 0
        && value.r_type() != SexpType::Nil
        && handles::length(context, value) == 0
        && (value_kind == object.r_type()
            || matches!(value_kind, SexpType::List | SexpType::Expression))
    {
        return Ok(object);
    }
    let value_count = if value.r_type() == SexpType::Nil {
        0
    } else if value.r_type().is_vector() {
        handles::length(context, value)
    } else {
        1
    };
    // GNU R's matrix and array assignments check the subscripts and the lengths before the
    // types, so a mismatch of types is reported only for an assignment that is otherwise sound.
    let target_kind = match subassign_type_fix(object.r_type(), value_kind) {
        Ok(kind) => kind,
        Err(mismatch) => {
            if indices.len() >= 2 {
                array_selections(context, call, object, indices, value_count)?;
            }
            return Err(mismatch);
        }
    };
    let mut result = if target_kind != object.r_type() {
        let converted = handles::coerce(context, object, target_kind)?;
        match handles::unreferenced(converted) {
            Some(converted) if !converted.get().is(object) => converted,
            _ => handles::shallow_copy(context, converted)?,
        }
    } else {
        unshared(context, object)?
    };
    let value = if value.r_type() != target_kind
        && value.r_type().is_vector()
        && target_kind != SexpType::List
    {
        handles::coerce(context, value, target_kind)?
    } else {
        value
    };
    // The subscripts are read against the object as it was: a vector that became a list has
    // lost its dimensions, as in GNU R, where the result of `m[1, 2] <- list(v)` is a plain list.
    if indices.len() >= 2 {
        if !value.is(result.get())
            && let Some(dims) =
                handles::dims(context, object).filter(|dims| dims.len() == indices.len())
            && let Some(fixed) = slab(context, indices, &dims, result.get(), value)
            && write_slab(context, &mut result, value, &dims, &fixed)
        {
            return Ok(result.finish());
        }
        let (dims, selections) = array_selections(context, call, object, indices, value_count)?;
        if selections.iter().all(|selection| !selection.is_empty()) {
            scatter(
                context,
                &mut result,
                ArrayOffsets::new(&selections, &dims),
                value,
            )?;
        }
        return Ok(result.finish());
    }
    let index = indices
        .first()
        .copied()
        .unwrap_or_else(handles::missing_argument);
    let count = handles::length(context, result.get());
    // A matrix with one column per dimension assigns single elements.
    if let Some(dims) = handles::dims(context, object)
        && let Some(positions) = matrix_positions(context, object, &dims, index, Some(call))?
    {
        // As with a vector subscript, a single value skips the NA rows, and a longer one
        // cannot say which of its elements an NA row would have taken.
        if value_count > 1 && positions.iter().any(Option::is_none) {
            return Err(handles::error_in(
                call,
                "NAs are not allowed in subscripted assignments",
            ));
        }
        let rows = positions.len();
        if rows > 0 && value_count == 0 {
            return Err(handles::error_in(call, "replacement has length zero"));
        }
        if rows > 0 && !rows.is_multiple_of(value_count) {
            handles::warning(
                context,
                "number of items to replace is not a multiple of replacement length",
            )?;
        }
        for (row, position) in positions.into_iter().enumerate() {
            if let Some(offset) = position {
                assign_element(
                    context,
                    &mut result,
                    offset,
                    value,
                    row % value_count.max(1),
                )?;
            }
        }
        return Ok(result.finish());
    }
    if scatter_plain(context, &mut result, index, value, value_count)? {
        return Ok(result.finish());
    }
    let names = handles::names_of(context, result.get())?;
    let (mut positions, new_names) =
        resolve_subscript(context, index, count, names, true, Some(call))?;
    // A single value skips the NA positions, and a longer one cannot say which of its elements
    // an NA position would have taken.
    if value_count > 1 && positions.contains(&NA_POSITION) {
        return Err(handles::error_in(
            call,
            "NAs are not allowed in subscripted assignments",
        ));
    }
    positions.retain(|position| *position != NA_POSITION);
    let stretch = logical_stretch(context, index);
    if positions.is_empty() && stretch <= count {
        return Ok(result.finish());
    }
    if !positions.is_empty() && value_count == 0 {
        return Err(handles::error_in(call, "replacement has length zero"));
    }
    // GNU R attributes this warning to the current call, not the `[<-` call: the assignment at
    // its own level, else the closure call, which for `NextMethod` is `NextMethod(.Generic)`.
    if !positions.is_empty() && !positions.len().is_multiple_of(value_count) {
        handles::warning(
            context,
            "number of items to replace is not a multiple of replacement length",
        )?;
    }
    let needed = positions
        .iter()
        .copied()
        .max()
        .map_or(0, |max| max + 1)
        .max(stretch);
    if needed > count {
        result = grow(context, result.get(), Some(result), needed, false)?;
        write_names(context, &mut result, count, &new_names)?;
    }
    for (position, &target) in positions.iter().enumerate() {
        assign_element(context, &mut result, target, value, position % value_count)?;
    }
    Ok(result.finish())
}

/// An empty vector of the type `x[i] <- value` gives `x <- NULL` for a value of type `kind`.
fn empty_vector<'s>(context: &mut Cx<'s>, kind: SexpType) -> R<Local<'s>> {
    Ok(match kind {
        SexpType::Logical => context.alloc::<Logical>(0)?.finish().value(),
        SexpType::Integer => context.alloc::<Integer>(0)?.finish().value(),
        SexpType::Real => context.alloc::<Real>(0)?.finish().value(),
        SexpType::Complex => context.alloc::<Complex>(0)?.finish().value(),
        SexpType::Raw => context.alloc::<Raw>(0)?.finish().value(),
        SexpType::String => context.alloc::<Str>(0)?.finish().value(),
        SexpType::Expression => context.alloc::<Expression>(0)?.finish().value(),
        _ => context.alloc::<List>(0)?.finish().value(),
    })
}

/// `x[i] <- NULL` on a list: the list without the selected elements. As with any assignment,
/// the list first stretches to a longer logical subscript or past its end, padded with NULL, so
/// `x[5] <- NULL` on two elements leaves four.
fn delete_elements<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    call: Local<'s>,
) -> R<Local<'s>> {
    let index = indices
        .first()
        .copied()
        .unwrap_or_else(handles::missing_argument);
    let stretch = logical_stretch(context, index);
    let mut object = object;
    if stretch > handles::length(context, object) {
        object = grow(context, object, None, stretch, false)?.finish();
    }
    let count = handles::length(context, object);
    let names = handles::names_of(context, object)?;
    let (positions, _) = resolve_subscript(context, index, count, names, true, Some(call))?;
    let needed = positions
        .iter()
        .filter(|&&position| position != NA_POSITION)
        .map(|&position| position + 1)
        .max()
        .unwrap_or(0)
        .max(stretch);
    let mut removed: KeptMask = smallvec![0; needed.max(count).div_ceil(64)];
    for &position in positions
        .iter()
        .filter(|&&position| position != NA_POSITION)
    {
        removed[position / 64] |= 1 << (position % 64);
    }
    if needed > count {
        object = grow(context, object, None, needed, false)?.finish();
    }
    let count = handles::length(context, object);
    let names = handles::names_of(context, object)?;
    let kept: Vec<usize> = (0..count)
        .filter(|&position| (removed[position / 64] & (1 << (position % 64))) == 0)
        .collect();
    let result = select(context, object, &kept)?;
    let mut result = match handles::unreferenced(result) {
        Some(result) => result,
        None => handles::shallow_copy(context, result)?,
    };
    // Names first, as GNU R lists them after a deletion.
    if names.r_type() != SexpType::Nil {
        let new_names = select(context, names, &kept)?;
        handles::set_attribute(
            context,
            &mut result,
            handles::symbol(|symbols| symbols.names),
            new_names,
        )?;
    }
    handles::copy_most_attributes(context, object, &mut result, false)?;
    Ok(result.finish())
}

/// `target[index] <- value` written straight from an empty subscript or an attribute-free
/// integer or double one whose positions all lie inside `target`, when `value` already has
/// `target`'s type and its length divides the number of positions. Returns false, having written
/// nothing, for every other shape, which the general path handles with GNU R's full rules.
fn scatter_plain<'s>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    index: Local<'s>,
    value: Local<'s>,
    value_count: usize,
) -> R<bool> {
    let kind = target.get().r_type();
    if index.attributes(context).r_type() != SexpType::Nil
        || value.is(target.get())
        || value.r_type() != kind
        || altrep(target.get())
        || altrep(value)
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
    let count = handles::length(context, target.get());
    if handles::is_missing_argument(index) {
        return Ok(scatter_run(context, target, 0, count, value, value_count));
    }
    if let Some((run, first, increment)) = compact_sequence(index)
        && increment == 1.0
        && first >= 1.0
        && first.fract() == 0.0
        && first as usize - 1 + run <= count
    {
        return Ok(scatter_run(
            context,
            target,
            first as usize - 1,
            run,
            value,
            value_count,
        ));
    }
    if let Some((highest, offsets)) = compact_sequence(index).and_then(compact_offsets) {
        return Ok(highest < count && scatter_offsets(context, target, offsets, value, value_count));
    }
    let index = handles::materialize(context, index)?;
    if let Some(mask) = index.standard::<Logical>() {
        return Ok(scatter_mask(context, target, mask, value));
    }
    let offsets: Positions = if let Some(positions) = index.standard::<Integer>() {
        let positions = positions.as_slice(context);
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
    } else if let Some(positions) = index.standard::<Real>() {
        let positions = positions.as_slice(context);
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
    } else {
        return Ok(false);
    };
    Ok(scatter_offsets(
        context,
        target,
        offsets.iter().copied(),
        value,
        value_count,
    ))
}

/// Writes `value`, recycled, to the elements of `target` that `mask` selects, recycled over
/// `target`. `value` has `target`'s type. False, having written nothing, for a list or character
/// target, an empty mask or one longer than `target`, and wherever the general path warns or
/// fails: an NA flag with a value longer than one, or a selection that is no multiple of it.
fn scatter_mask<'s>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    mask: Local<'s, Logical>,
    value: Local<'s>,
) -> bool {
    fn atomic<'s, T: Atomic>(
        context: &mut Cx<'s>,
        target: &mut Owned<'s>,
        mask: Local<'s, Logical>,
        value: Local<'s>,
    ) -> Option<bool> {
        let (target, source) = (target.narrow::<T>()?, value.standard::<T>()?);
        let mut write = target.write(context);
        let (elements, reader) = write.split();
        let (flags, source) = (reader.slice(mask)?, reader.slice(source)?);
        if flags.is_empty() || flags.len() > elements.len() {
            return Some(false);
        }
        if let [single] = source {
            // A single value skips the NA flags.
            for chunk in elements.chunks_mut(flags.len()) {
                for (element, &flag) in chunk.iter_mut().zip(flags) {
                    if flag != 0 && flag != NA_LOGICAL {
                        *element = *single;
                    }
                }
            }
            return Some(true);
        }
        let mut selected = 0usize;
        for chunk in elements.chunks(flags.len()) {
            for &flag in &flags[..chunk.len()] {
                if flag == NA_LOGICAL {
                    return Some(false);
                }
                selected += usize::from(flag != 0);
            }
        }
        if !selected.is_multiple_of(source.len()) {
            return Some(false);
        }
        let mut next = 0;
        for chunk in elements.chunks_mut(flags.len()) {
            for (element, &flag) in chunk.iter_mut().zip(flags) {
                if flag != 0 {
                    *element = source[next];
                    next = if next + 1 == source.len() {
                        0
                    } else {
                        next + 1
                    };
                }
            }
        }
        Some(true)
    }
    atomic::<Real>(context, target, mask, value)
        .or_else(|| atomic::<Integer>(context, target, mask, value))
        .or_else(|| atomic::<Logical>(context, target, mask, value))
        .unwrap_or(false)
}

/// Writes `value`, recycled, to the `run` elements of `target` from `start`, which lie inside it,
/// a whole copy of `value` at a time. `value` has `target`'s type and is another vector. False,
/// having written nothing, unless `run` is a multiple of `value_count`.
fn scatter_run<'s>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    start: usize,
    run: usize,
    value: Local<'s>,
    value_count: usize,
) -> bool {
    fn atomic<'s, T: Atomic>(
        context: &mut Cx<'s>,
        target: &mut Owned<'s>,
        start: usize,
        run: usize,
        value: Local<'s>,
    ) -> Option<()> {
        let (target, source) = (target.narrow::<T>()?, value.standard::<T>()?);
        let mut write = target.write(context);
        let (elements, reader) = write.split();
        fill_run(elements.get_mut(start..start + run)?, reader.slice(source)?);
        Some(())
    }
    /// The written part is copied onto the part after it, so a short value costs a barrier
    /// decision per doubling, not per copy.
    fn elements<'s, T: handles::Elements>(
        context: &mut Cx<'s>,
        target: &mut Owned<'s>,
        start: usize,
        run: usize,
        value: Local<'s>,
        value_count: usize,
    ) -> Option<()> {
        let (target, source) = (target.narrow::<T>()?, value.standard::<T>()?);
        handles::copy_elements(context, target, start, source, 0, value_count);
        let mut filled = value_count;
        while filled < run {
            let step = filled.min(run - filled);
            let written = target.get();
            handles::copy_elements(context, target, start + filled, written, start, step);
            filled += step;
        }
        Some(())
    }
    if !run.is_multiple_of(value_count) {
        return false;
    }
    if run == 0 {
        return true;
    }
    atomic::<Real>(context, target, start, run, value)
        .or_else(|| atomic::<Integer>(context, target, start, run, value))
        .or_else(|| atomic::<Logical>(context, target, start, run, value))
        .or_else(|| elements::<Str>(context, target, start, run, value, value_count))
        .or_else(|| elements::<List>(context, target, start, run, value, value_count))
        .is_some()
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
fn scatter_offsets<'s, I: ExactSizeIterator<Item = usize>>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    offsets: I,
    value: Local<'s>,
    value_count: usize,
) -> bool {
    fn elements<'s, T: handles::Elements, I: Iterator<Item = usize>>(
        context: &mut Cx<'s>,
        target: &mut Owned<'s>,
        offsets: I,
        value: Local<'s>,
        value_count: usize,
    ) -> Result<(), I> {
        let (Some(target), Some(source)) = (target.narrow::<T>(), value.standard::<T>()) else {
            return Err(offsets);
        };
        let mut write = target.write(context);
        let (mut stores, reader) = write.split_elements();
        for (position, offset) in offsets.enumerate() {
            stores.set(offset, reader.element(source, position % value_count));
        }
        Ok(())
    }
    if !offsets.len().is_multiple_of(value_count) {
        return false;
    }
    let Err(offsets) = recycle_typed::<Real, _>(context, target, offsets, value) else {
        return true;
    };
    let Err(offsets) = recycle_typed::<Integer, _>(context, target, offsets, value) else {
        return true;
    };
    let Err(offsets) = recycle_typed::<Logical, _>(context, target, offsets, value) else {
        return true;
    };
    let Err(offsets) = elements::<Str, _>(context, target, offsets, value, value_count) else {
        return true;
    };
    elements::<List, _>(context, target, offsets, value, value_count).is_ok()
}

/// A logical subscript longer than the vector stretches it on assignment to the subscript's
/// length, whatever the subscript selects, as GNU R does.
fn logical_stretch(context: &mut Cx<'_>, index: Local<'_>) -> usize {
    if index.r_type() == SexpType::Logical {
        handles::length(context, index)
    } else {
        0
    }
}

/// A vector kind whose elements [`grow`] pads, moves and appends.
pub(crate) trait Growable: Vector + Sized {
    /// Sets the elements of `vector` from `from` on to NA (`NULL` in a list).
    fn pad<'s>(context: &mut Cx<'s>, vector: &mut Owned<'s, Self>, from: usize);

    /// Stores `value`, a scalar of the vector's type or a list element, at `at`.
    fn put<'s>(
        context: &mut Cx<'s>,
        vector: &mut Owned<'s, Self>,
        at: usize,
        value: Local<'s>,
    ) -> R<()>;

    /// The first `count` elements of `from` into the new vector `to`.
    fn take<'s>(
        context: &mut Cx<'s>,
        to: &mut Fresh<'s, Self>,
        from: &mut Owned<'s, Self>,
        count: usize,
    ) -> R<()> {
        handles::copy_prefix(context, from.get(), to, count)
    }
}

macro_rules! stored_atomic {
    ($kind:ident, $na:expr, $($source:ident),+) => {
        impl Growable for $kind {
            fn pad<'s>(context: &mut Cx<'s>, vector: &mut Owned<'s, Self>, from: usize) {
                let mut write = vector.write(context);
                let (elements, _) = write.split();
                if let Some(tail) = elements.get_mut(from..) {
                    tail.fill($na);
                }
            }

            fn put<'s>(
                context: &mut Cx<'s>,
                vector: &mut Owned<'s, Self>,
                at: usize,
                value: Local<'s>,
            ) -> R<()> {
                let element = None
                    $(.or_else(|| {
                        value
                            .standard::<$source>()
                            .and_then(|source| source.as_slice(context).first().copied())
                    }))+
                    .ok_or_else(|| error("internal error: an element of another type than the vector's"))?;
                let mut write = vector.write(context);
                let (elements, _) = write.split();
                let slot = elements
                    .get_mut(at)
                    .ok_or_else(|| error("internal error: element index out of bounds"))?;
                *slot = element;
                Ok(())
            }
        }
    };
}

stored_atomic!(Logical, NA_INTEGER, Logical, Integer);
stored_atomic!(Integer, NA_INTEGER, Integer, Logical);
stored_atomic!(Real, NA_REAL, Real);
stored_atomic!(
    Complex,
    Rcomplex {
        r: NA_REAL,
        i: NA_REAL
    },
    Complex
);
stored_atomic!(Raw, 0, Raw);

impl Growable for Str {
    fn pad<'s>(context: &mut Cx<'s>, vector: &mut Owned<'s, Self>, from: usize) {
        let length = vector.get().len();
        let mut write = vector.write(context);
        let (mut stores, _) = write.split_elements();
        for index in from..length {
            stores.set(index, handles::na());
        }
    }

    fn put<'s>(
        context: &mut Cx<'s>,
        vector: &mut Owned<'s, Self>,
        at: usize,
        value: Local<'s>,
    ) -> R<()> {
        let text = value
            .standard::<Str>()
            .filter(|text| !text.is_empty())
            .ok_or_else(|| error("internal error: an element of another type than the vector's"))?;
        let mut write = vector.write(context);
        let (mut stores, reader) = write.split_elements();
        stores.set(at, reader.element(text, 0));
        Ok(())
    }
}

macro_rules! stored_list {
    ($kind:ident) => {
        impl Growable for $kind {
            /// A new list's elements and those a list grows by in place are already `NULL`.
            fn pad<'s>(_context: &mut Cx<'s>, _vector: &mut Owned<'s, Self>, _from: usize) {}

            fn put<'s>(
                context: &mut Cx<'s>,
                vector: &mut Owned<'s, Self>,
                at: usize,
                value: Local<'s>,
            ) -> R<()> {
                let mut write = vector.write(context);
                let (mut stores, _) = write.split_elements();
                stores.set(at, value);
                Ok(())
            }

            /// An owned list's elements move into the copy instead of being counted into it
            /// and back out of the original when the collector sweeps it.
            fn take<'s>(
                context: &mut Cx<'s>,
                to: &mut Fresh<'s, Self>,
                from: &mut Owned<'s, Self>,
                _count: usize,
            ) -> R<()> {
                handles::move_elements(context, to, from);
                Ok(())
            }
        }
    };
}

stored_list!(List);
stored_list!(Expression);

/// Runs `$body` with `$typed` bound to `$value` downcast to the first of the stored vector kinds
/// it is, as the type `$kind`, returning what the body returns.
macro_rules! for_stored_kind {
    ($value:expr, |$typed:ident: $kind:ident| $body:expr) => {
        for_stored_kind!(@each $value, $typed, $kind, $body,
            Logical, Integer, Real, Complex, Raw, Str, List, Expression)
    };
    (@each $value:expr, $typed:ident, $kind:ident, $body:expr, $($each:ident),+) => {
        $(
            if let Some($typed) = $value.standard::<$each>() {
                #[allow(dead_code)]
                type $kind = $each;
                return $body;
            }
        )+
    };
}

/// `vector` extended to `new_length`, padded with NA, keeping its attributes but not its dims.
/// `owned` is `vector` when the caller holds it alone.
///
/// A copy reserves room past `new_length`, by GNU R's factor of 1.05, and an owned vector with
/// room grows in place, so a loop that appends one element at a time runs in linear time.
///
/// `final_step` says that nothing after the growth can fail, so an owned list's elements may
/// move into the copy (see [`Growable::take`]). An error after a move would leave the variable
/// holding an empty list.
fn grow<'s>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    owned: Option<Owned<'s>>,
    new_length: usize,
    final_step: bool,
) -> R<Owned<'s>> {
    assert!(
        owned.as_ref().is_none_or(|owned| owned.get().is(vector)),
        "the claim is for another vector"
    );
    for_stored_kind!(vector, |typed: Kind| {
        let owned = owned.and_then(|owned| owned.standard::<Kind>().ok());
        Ok(grow_as(context, typed, owned, new_length, final_step)?
            .vector
            .value())
    });
    Err(error(format!(
        "internal error: growing a vector of type '{}'",
        handles::type_name(vector)
    )))
}

/// A grown vector and, when the growth copied its names, the copy: the vector's attribute is
/// the only reference to it, so a name for a new element is stored without a second lookup.
struct Grown<'s, T: Kind> {
    vector: Owned<'s, T>,
    names: Option<Owned<'s, Str>>,
}

fn grow_as<'s, T: Growable>(
    context: &mut Cx<'s>,
    vector: Local<'s, T>,
    owned: Option<Owned<'s, T>>,
    new_length: usize,
    final_step: bool,
) -> R<Grown<'s, T>> {
    grow_in_caller(context, vector, owned, new_length, final_step, true)
}

/// [`grow_as`] compiled into its caller, which an append is hot enough to want. `pad` is false
/// when the caller stores every new element itself.
#[inline(always)]
fn grow_in_caller<'s, T: Growable>(
    context: &mut Cx<'s>,
    vector: Local<'s, T>,
    owned: Option<Owned<'s, T>>,
    new_length: usize,
    final_step: bool,
    pad: bool,
) -> R<Grown<'s, T>> {
    let count = vector.len();
    let mut owned = match owned {
        Some(mut owned) => {
            // A full vector fails the header test, which spares it the call.
            if owned.has_room(new_length)
                && handles::resize_in_place(context, &mut owned, new_length)?
            {
                if pad {
                    T::pad(context, &mut owned, count);
                }
                return Ok(Grown {
                    vector: owned,
                    names: None,
                });
            }
            Some(owned)
        }
        None => None,
    };
    let mut result = handles::alloc_with_room::<T>(context, new_length)?;
    match owned.as_mut() {
        Some(original) if final_step => T::take(context, &mut result, original, count)?,
        _ => handles::copy_prefix(context, vector, &mut result, count)?,
    }
    let mut result = result.owned();
    if pad {
        T::pad(context, &mut result, count);
    }
    let names_symbol = handles::symbol(|symbols| symbols.names);
    let (names, only_names) = {
        let mut cells = handles::attribute_cells(context, vector);
        let (names, only_names) = match cells.next() {
            None => (None, true),
            Some((tag, names)) if tag.is(names_symbol) => (Some(names), cells.next().is_none()),
            Some(_) => {
                let names = handles::get_attribute(context, vector, names_symbol);
                ((names.r_type() != SexpType::Nil).then_some(names), false)
            }
        };
        (names.map(|names| context.root(names)), only_names)
    };
    // Names go first and the other attributes follow in order, as GNU R's `EnlargeVector`
    // lays them out; code such as the globals package checks the attribute order.
    let names = match names {
        Some(names) => {
            let mut grown = handles::alloc_with_room::<Str>(context, new_length)?;
            if let Some(names) = handles::dense::<Str>(context, names)? {
                let kept = names.len().min(count);
                handles::copy_prefix(context, names, &mut grown, kept)?;
            }
            let grown = grown.owned();
            handles::set_attribute(context, &mut result, names_symbol, grown.get().value())?;
            Some(grown)
        }
        None => None,
    };
    // The S4 bit travels with the attributes, and a vector may carry it without any.
    if !only_names || handles::is_s4(vector.value()) {
        handles::copy_most_attributes(context, vector.value(), &mut result, false)?;
    }
    Ok(Grown {
        vector: result,
        names,
    })
}

/// Writes `new_names` from position `count` into the names of the grown `vector`, which gets
/// blank names first when it has none.
fn write_names<'s>(
    context: &mut Cx<'s>,
    vector: &mut Owned<'s>,
    count: usize,
    new_names: &[Local<'s, handles::Chars>],
) -> R<()> {
    if new_names.is_empty() {
        return Ok(());
    }
    let length = handles::length(context, vector.get());
    let mut names = match handles::owned_names(context, vector) {
        Some(names) if names.get().len() == length => names,
        _ => {
            let names_symbol = handles::symbol(|symbols| symbols.names);
            let old = handles::get_attribute(context, vector.get(), names_symbol);
            let old = context.root(old);
            let mut names = handles::alloc_with_room::<Str>(context, length)?;
            if let Some(old) = handles::dense::<Str>(context, old)? {
                let kept = old.len().min(length);
                handles::copy_prefix(context, old, &mut names, kept)?;
            }
            let names = names.owned();
            handles::set_attribute(context, vector, names_symbol, names.get().value())?;
            names
        }
    };
    let mut write = names.write(context);
    let (mut stores, _) = write.split_elements();
    for (offset, &name) in new_names.iter().enumerate() {
        if count + offset < length {
            stores.set(count + offset, name);
        }
    }
    Ok(())
}

/// `vector` one element longer, for the caller to bind: `x[[n + 1]] <- value`, or `x$name <- value`
/// and `x[["name"]] <- value` for a name `x` lacks. `put` stores the new last element, and `name`
/// names it, after blank names where `vector` has none. `vector`'s only attribute is its names.
/// `owned` is the vector when the caller's variable alone holds it: it then grows where it is if
/// it has room, as [`grow`] does, and the result is otherwise a copy with room to grow.
pub(crate) fn append<'s, T: Growable>(
    context: &mut Cx<'s>,
    vector: Local<'s, T>,
    owned: Option<Owned<'s>>,
    name: Option<Local<'s, handles::Chars>>,
    put: impl FnOnce(&mut Cx<'s>, &mut Owned<'s, T>, usize),
) -> R<Local<'s>> {
    assert!(
        owned
            .as_ref()
            .is_none_or(|owned| owned.get().is(vector.value())),
        "the claim is for another vector"
    );
    let count = vector.len();
    let owned = owned.and_then(|owned| owned.standard::<T>().ok());
    // Writing a name can fail, so only an unnamed append may move an owned list's elements.
    let Grown {
        vector: mut result,
        names,
    } = grow_in_caller(context, vector, owned, count + 1, name.is_none(), false)?;
    put(context, &mut result, count);
    let mut result = result.value();
    match (name, names) {
        (Some(name), Some(mut names)) => {
            // The copied names are as long as the grown vector.
            let mut write = names.write(context);
            let (mut stores, _) = write.split_elements();
            stores.set(count, name);
        }
        (Some(name), None) => write_names(context, &mut result, count, &[name])?,
        (None, _) => {}
    }
    Ok(result.finish())
}

/// Stores `value`, a scalar of the vector's type or a list element, at `at` of `vector`.
fn store_scalar<'s>(
    context: &mut Cx<'s>,
    vector: &mut Owned<'s>,
    at: usize,
    value: Local<'s>,
) -> R<()> {
    macro_rules! each {
        ($($kind:ident),+) => {
            $(
                if let Some(typed) = vector.narrow::<$kind>() {
                    return <$kind as Growable>::put(context, typed, at, value);
                }
            )+
        };
    }
    each!(Logical, Integer, Real, Complex, Raw, Str, List, Expression);
    Err(error(format!(
        "internal error: storing into a vector of type '{}'",
        handles::type_name(vector.get())
    )))
}

/// `target[offsets] <- value`, recycling `value`, which is not empty, and skipping NA offsets.
fn scatter<'s>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    offsets: impl Iterator<Item = usize>,
    value: Local<'s>,
) -> R<()> {
    let value = handles::materialize(context, value)?;
    let count = handles::length(context, value);
    let Err(offsets) = recycle_typed::<Real, _>(context, target, offsets, value) else {
        return Ok(());
    };
    let Err(offsets) = recycle_typed::<Integer, _>(context, target, offsets, value) else {
        return Ok(());
    };
    let Err(offsets) = recycle_typed::<Logical, _>(context, target, offsets, value) else {
        return Ok(());
    };
    for (position, offset) in offsets.enumerate() {
        if offset != NA_POSITION {
            assign_element(context, target, offset, value, position % count)?;
        }
    }
    Ok(())
}

/// [`recycle_into`] when `target` and `value` are distinct standard vectors of kind `T`, and
/// otherwise the offsets back, untouched.
#[inline(always)]
fn recycle_typed<'s, T: Atomic, I: Iterator<Item = usize>>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    offsets: I,
    value: Local<'s>,
) -> Result<(), I> {
    let (Some(target), Some(source)) = (target.narrow::<T>(), value.standard::<T>()) else {
        return Err(offsets);
    };
    let mut write = target.write(context);
    let (elements, reader) = write.split();
    let Some(source) = reader.slice(source) else {
        return Err(offsets);
    };
    recycle_into(elements, offsets, source);
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

fn assign_element<'s>(
    context: &mut Cx<'s>,
    target: &mut Owned<'s>,
    position: usize,
    value: Local<'s>,
    value_index: usize,
) -> R<()> {
    if !value.r_type().is_vector() {
        if let Some(list) = target.narrow::<List>() {
            list.write(context).split_elements().0.set(position, value);
            return Ok(());
        }
        if let Some(list) = target.narrow::<Expression>() {
            list.write(context).split_elements().0.set(position, value);
            return Ok(());
        }
    }
    store_element(context, value, value_index, target, position)
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
///
/// # Safety
///
/// On the interpreter's thread, with every argument a live object that the caller keeps rooted
/// until it returns. The result is rooted in the caller's handle scope.
pub unsafe fn subassign2(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    // SAFETY: the caller's roots hold the arguments for the whole call, and the result is
    // rooted before anything else allocates.
    unsafe {
        handles::with_result(|context| {
            let object = handles::held(context, object);
            let indices: SmallVec<[Local<'_>; 4]> = indices
                .iter()
                .map(|&index| handles::held(context, index))
                .collect();
            let value = handles::held(context, value);
            let call = handles::held(context, call);
            Ok(assign_subset2(context, object, &indices, value, call)?.raw())
        })
    }
}

/// [`subassign2`] on handles.
pub(crate) fn assign_subset2<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    indices: &[Local<'s>],
    value: Local<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    // `x[[i]] <- x` stores the old `x`, which is also the assignment's value, so the new `x`
    // is a copy: modified in place, the list would contain itself.
    let object = if value.is(object) && object.r_type() != SexpType::Environment {
        handles::shallow_copy(context, object)?.finish()
    } else {
        object
    };
    if object.r_type() == SexpType::Record {
        return handles::record_subassign2(context, object, indices, value, call);
    }
    let object = if handles::is_array(object) {
        match handles::array_subassign(context, object, indices, value, call, true)? {
            Some(result) => return Ok(result),
            None => handles::shallow_copy(context, object)?.finish(),
        }
    } else {
        object
    };
    let object = handles::materialize(context, object)?;
    // A reference class object assigns into its environment and stays itself.
    if object.r_type() == SexpType::Object && is_s4(object) {
        let data = s4_data(context, object);
        if data.r_type() != SexpType::Environment {
            return Err(handles::error_in(
                call,
                "[[<- defined for objects of type \"S4\" only for subclasses of environment",
            ));
        }
        assign_subset2(context, data, indices, value, call)?;
        return Ok(object);
    }
    if let Some(environment) = object.downcast::<handles::Environment>() {
        handles::reaching_environment(context)?;
        let name = match indices.first() {
            Some(&index) if handles::length(context, index) == 1 => {
                handles::dense::<Str>(context, index)?
            }
            _ => None,
        };
        let Some(name) = name else {
            return Err(handles::error_in(
                call,
                "wrong args for environment subassignment",
            ));
        };
        let symbol = handles::install_element(context, name, 0)?;
        handles::define(context, symbol, value, environment)?;
        return Ok(object);
    }
    if indices.is_empty()
        || indices
            .iter()
            .any(|&index| handles::is_missing_argument(index))
    {
        let object_field = [("object", object)];
        let fields: &[(&str, Local<'_>)] = if object.r_type() == SexpType::Nil {
            &[]
        } else {
            &object_field
        };
        return Err(handles::classed_error(
            context,
            Some(call),
            "missing subscript",
            &["MissingSubscriptError", "error", "condition"],
            fields,
        ));
    }
    if matches!(object.r_type(), SexpType::Pairlist | SexpType::Language) {
        let list = pairlist_to_list(context, object)?;
        let result = assign_subset2(context, list, indices, value, call)?;
        let result = standard::<List>(result)?;
        let mut result = handles::list_to_pairlist(context, result, object.r_type(), true)?;
        // The call keeps its attributes, such as a formula's class and environment, and a
        // pairlist keeps its dimensions.
        handles::copy_most_attributes(context, object, &mut result, true)?;
        return Ok(result.finish());
    }
    if object.r_type() == SexpType::Nil && value.r_type() == SexpType::Nil {
        return Ok(object);
    }
    if let &[index] = indices {
        return assign_one(context, object, index, value, call, 1, false);
    }
    let Some(dims) = handles::dims(context, object).filter(|dims| dims.len() == indices.len())
    else {
        // GNU R counts the subscripts after it has checked what the object can hold.
        let kind = object.r_type();
        let is_vector = kind == SexpType::Nil
            || kind.is_atomic()
            || matches!(kind, SexpType::List | SexpType::Expression);
        if !is_vector {
            return Err(not_subsettable_error(context, Some(call), object));
        }
        if kind.is_atomic() {
            atomic_replacement_check(context, value)?;
        }
        return Err(handles::error_in(
            call,
            "[[ ]] improper number of subscripts",
        ));
    };
    // `m[[i, j]] <- value` names one cell, which must exist. GNU R checks the value's length
    // before it reads a subscript.
    let is_list = matches!(object.r_type(), SexpType::List | SexpType::Expression);
    if !is_list {
        atomic_replacement_check(context, value)?;
    }
    let dimnames = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    ));
    let dimnames = dense::<List>(context, dimnames)?;
    let mut offset = 0usize;
    let mut stride = 1usize;
    for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
        let names = match dimnames {
            Some(dimnames) if dimension < dimnames.len() => {
                context.root(dimnames.get(context, dimension))
            }
            _ => null(),
        };
        let index = symbol_text(context, index)?;
        let position = match one_position(
            context,
            index,
            extent,
            names,
            Lookup::Element {
                exact: Some(true),
                call: None,
            },
            true,
        )? {
            Single::At(position) => position,
            Single::Outside => {
                return Err(subscript2_out_of_bounds(
                    context,
                    object,
                    Some(dimension),
                    index,
                ));
            }
            Single::Invalid(message) => return Err(replacement_error(context, call, message)),
        };
        offset += position * stride;
        stride *= extent;
    }
    if is_list && value.r_type() == SexpType::Nil {
        return Err(incompatible_types(
            handles::type_name(object),
            "NULL",
            "[[ assignment",
        ));
    }
    // The cell is assigned by its position in the vector, which keeps the dimensions.
    let position = scalar::<Real>(context, offset as f64 + 1.0)?;
    assign_one(context, object, position, value, call, 1, false)
}

/// `x[[i]] <- value` with one subscript, which for a list may be a path of several (recursive
/// assignment); `level` is the depth of that path, which GNU R's error names.
/// `reached_through_shared` says that a list above `object` on the path is shared, so `object`
/// is copied before the store.
fn assign_one<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    index: Local<'s>,
    value: Local<'s>,
    call: Local<'s>,
    level: usize,
    reached_through_shared: bool,
) -> R<Local<'s>> {
    if matches!(object.r_type(), SexpType::Pairlist | SexpType::Language) {
        return assign_subset2(context, object, &[index], value, call);
    }
    // A symbol subscript names the element, so the rest of the assignment sees it as a string.
    let index = symbol_text(context, index)?;
    let kind = object.r_type();
    let is_list = matches!(kind, SexpType::Nil | SexpType::List | SexpType::Expression);
    let count = handles::length(context, index);
    let object_count = handles::length(context, object);
    let names = handles::names_attribute(context, object);
    if count > 1 {
        if !is_list {
            return Err(replacement_error(
                context,
                call,
                "attempt to select more than one element in vectorIndex",
            ));
        }
        let first = element(context, index, 0)?;
        let rest_positions: Vec<Position> = (1..count).collect();
        let rest = select(context, index, &rest_positions)?;
        let inner = match one_index(context, object_count, names, first)? {
            Slot::At(position) if position < object_count => element(context, object, position)?,
            _ => {
                return Err(replacement_error(
                    context,
                    call,
                    format!("no such index at level {level}\n"),
                ));
            }
        };
        // The element of a shared list has other owners through that list, whatever its own
        // reference count says.
        let below_shared = reached_through_shared || object.is_shared();
        let updated = assign_one(context, inner, rest, value, call, level + 1, below_shared)?;
        return assign_one(
            context,
            object,
            first,
            updated,
            call,
            level,
            reached_through_shared,
        );
    }
    let object = if reached_through_shared && kind != SexpType::Nil {
        handles::shallow_copy(context, object)?.get()
    } else {
        object
    };
    if !is_list {
        atomic_replacement_check(context, value)?;
    }
    let slot = one_index(context, object_count, names, index)?;
    if !is_list {
        let position = match slot {
            Slot::At(position) => {
                let mut position_value = context.alloc::<Real>(1)?;
                position_value.as_mut_slice()[0] = position as f64 + 1.0;
                position_value.finish().value()
            }
            Slot::Named => index,
            Slot::Absent => return Err(subscript2_out_of_bounds(context, object, None, index)),
        };
        if !value.r_type().is_atomic() {
            // A list or expression value turns the vector into one of its kind.
            let list = handles::coerce(context, object, value.r_type())?;
            return assign_one(context, list, position, value, call, level, false);
        }
        return assign_subset(context, object, &[position], value, call);
    }
    if kind == SexpType::Expression && value.r_type() != SexpType::Nil {
        if value.r_type() == SexpType::List {
            return Err(incompatible_types("expression", "list", "[[ assignment"));
        }
        subassign_type_fix(SexpType::Expression, value.r_type())?;
    }
    let position = match slot {
        Slot::At(position) => position,
        Slot::Named => object_count,
        Slot::Absent if value.r_type() == SexpType::Nil => return Ok(object),
        Slot::Absent => return Err(subscript2_out_of_bounds(context, object, None, index)),
    };
    if value.r_type() == SexpType::Nil {
        if position >= object_count {
            return Ok(object);
        }
        let mut position_value = context.alloc::<Real>(1)?;
        position_value.as_mut_slice()[0] = position as f64 + 1.0;
        let position_value = position_value.finish().value();
        return assign_subset(context, object, &[position_value], null(), call);
    }
    let list = if kind == SexpType::Nil {
        context.alloc::<List>(0)?.finish().value()
    } else {
        object
    };
    let owned = unshared(context, list)?;
    let mut result = if position >= object_count {
        let mut grown = grow(context, owned.get(), Some(owned), position + 1, true)?;
        if matches!(slot, Slot::Named) {
            let name = handles::dense::<Str>(context, index)?
                .ok_or_else(|| error("internal error: a name subscript that is not a string"))?;
            let name = name.get(context, 0);
            let name = context.root(name);
            write_names(context, &mut grown, position, &[name])?;
        }
        grown
    } else {
        owned
    };
    store_scalar(context, &mut result, position, value)?;
    Ok(result.finish())
}
/// The type `x[i] <- value` leaves `x` as, with GNU R's errors for the pairs it refuses: raw
/// mixes only with raw, an atomic vector takes no function, symbol, call or environment, and a
/// list takes no function or environment.
fn subassign_type_fix(object: SexpType, value: SexpType) -> R<SexpType> {
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

/// An error that names `call` as GNU R names the call of a replacement function, the form
/// `[[<-`(`*tmp*`, ...) for one an assignment generated.
fn replacement_error<'s>(
    context: &mut Cx<'s>,
    call: Local<'s>,
    message: impl Into<String>,
) -> Unwind {
    match handles::generated_call(context, call) {
        Ok(reported) => handles::error_in(reported, message),
        Err(failure) => failure,
    }
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

/// The slot one `[[<-` subscript selects in an object of `extent` elements with `names`, with
/// GNU R's errors: a length-2 vector takes a negative subscript as the element it leaves out.
fn one_index<'s>(
    context: &mut Cx<'s>,
    extent: usize,
    names: Local<'s>,
    index: Local<'s>,
) -> R<Slot> {
    if handles::length(context, index) == 0 {
        return Err(error("attempt to select less than one element in OneIndex"));
    }
    let wanted = match index.downcast::<Symbol>() {
        Some(symbol) => {
            let name = symbol.read(context).name();
            Some(context.root(name))
        }
        None => match dense::<Str>(context, index)? {
            Some(index) => {
                let name = index.get(context, 0);
                Some(context.root(name))
            }
            None => None,
        },
    };
    if let Some(wanted) = wanted {
        if wanted.read(context).bytes().is_empty() {
            return Ok(Slot::Named);
        }
        return Ok(
            match name_position(context, names, wanted, Lookup::Store)? {
                Some(position) => Slot::At(position),
                None => Slot::Named,
            },
        );
    }
    let integer = match dense::<Integer>(context, index)? {
        Some(index) => Some(index.as_slice(context)[0]),
        None => dense::<Logical>(context, index)?.map(|index| index.as_slice(context)[0]),
    };
    if let Some(value) = integer {
        // NA is the most negative integer, so it selects "more than one" element.
        return one_index_position(i64::from(value), extent, "integerOneIndex");
    }
    if let Some(index) = dense::<Real>(context, index)? {
        let value = index.as_slice(context)[0];
        // Past the longest vector R allows (2^52), a position selects nothing.
        if value.is_nan() || value > 4_503_599_627_370_496.0 {
            return Ok(Slot::Absent);
        }
        // Truncated toward zero, saturating at the integer range.
        return one_index_position(value.trunc() as i64, extent, "OneIndex <real>");
    }
    Err(error(format!(
        "invalid subscript type '{}'",
        handles::type_name(index)
    )))
}

fn one_index_position(value: i64, extent: usize, function: &str) -> R<Slot> {
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
fn atomic_replacement_check(context: &mut Cx<'_>, value: Local<'_>) -> R<()> {
    let kind = value.r_type();
    if !(kind == SexpType::Nil
        || kind.is_atomic()
        || matches!(kind, SexpType::List | SexpType::Expression))
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
    match handles::length(context, value) {
        0 => Err(error("replacement has length zero")),
        1 => Ok(()),
        _ => Err(error("more elements supplied than there are to replace")),
    }
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

/// `x$name <- value`, with `name` a `CHARSXP`.
///
/// # Safety
///
/// On the interpreter's thread, with every argument a live object that the caller keeps rooted
/// until it returns. The result is rooted in the caller's handle scope.
pub unsafe fn dollar_assign(object: Sexp, name: Sexp, value: Sexp, call: Sexp) -> R<Sexp> {
    // SAFETY: the caller's roots hold the arguments for the whole call, and the result is
    // rooted before anything else allocates.
    unsafe {
        handles::with_result(|context| {
            let object = handles::held(context, object);
            let name = handles::held(context, name)
                .downcast::<handles::Chars>()
                .ok_or_else(|| error("internal error: a name that is not a string"))?;
            let value = handles::held(context, value);
            let call = handles::held(context, call);
            Ok(assign_dollar(context, object, name, value, call)?.raw())
        })
    }
}

/// [`dollar_assign`] on handles.
pub(crate) fn assign_dollar<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    name: Local<'s, handles::Chars>,
    value: Local<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    // `x$name <- x` stores the old `x`, as for `[[<-`.
    let object = if value.is(object) && object.r_type() != SexpType::Environment {
        handles::shallow_copy(context, object)?.finish()
    } else {
        object
    };
    if object.r_type() == SexpType::Record {
        let bytes: SmallVec<[u8; 32]> = SmallVec::from_slice(name.bytes(context));
        return handles::record_dollar_assign(context, object, &bytes, value, call);
    }
    let object = handles::materialize(context, object)?;
    if let Some(environment) = object.downcast::<handles::Environment>() {
        handles::reaching_environment(context)?;
        let symbol = handles::install(context, name)?;
        handles::define(context, symbol, value, environment)?;
        return Ok(object);
    }
    let kind = object.r_type();
    if kind == SexpType::Object && is_s4(object) {
        let data = get_attribute(context, object, symbol(|symbols| symbols.dot_xdata));
        let data = context.root(data);
        let Some(environment) = data.downcast::<handles::Environment>() else {
            return Err(handles::error_in(call, "no '$<-' method for this S4 class"));
        };
        handles::reaching_environment(context)?;
        let symbol = handles::install(context, name)?;
        handles::define(context, symbol, value, environment)?;
        return Ok(object);
    }
    // GNU R interns the name as a symbol, which rejects an empty one and spells `NA` out.
    let name = if is_na(name.read(context)) || name.bytes(context).is_empty() {
        let symbol = handles::install(context, name)?;
        let name = symbol.read(context).name();
        context.root(name)
    } else {
        name
    };
    let list = match kind {
        SexpType::List
        | SexpType::Expression
        | SexpType::Nil
        | SexpType::Pairlist
        | SexpType::Language => object,
        _ if kind.is_atomic() => {
            handles::warning_in(context, call, "Coercing LHS to a list")?;
            handles::coerce(context, object, SexpType::List)?
        }
        _ => return Err(not_subsettable_error(context, Some(call), object)),
    };
    // GNU R's `$<-` matches an `NA` name with nothing, where its `[[<-` reads one as the text
    // "NA", so a list with such a name is assigned by position.
    if name.bytes(context) == b"NA"
        && matches!(list.r_type(), SexpType::List | SexpType::Expression)
        && let names = names_of(context, list)?
        && let Some(dense_names) = dense::<Str>(context, names)?
        && dense_names.iter(context).any(is_na)
    {
        let exact = Lookup::Element {
            exact: Some(true),
            call: None,
        };
        let found = name_position(context, names, name, exact)?;
        if found.is_none() && value.r_type() == SexpType::Nil {
            return Ok(list);
        }
        let extent = handles::length(context, list);
        let position = scalar::<Real>(context, found.unwrap_or(extent) as f64 + 1.0)?;
        let assigned = assign_subset2(context, list, &[position], value, call)?;
        if found.is_some() {
            return Ok(assigned);
        }
        let mut assigned = unshared(context, assigned)?;
        write_names(context, &mut assigned, extent, &[name])?;
        return Ok(assigned.finish());
    }
    let mut index = context.alloc::<Str>(1)?;
    index.set(context, 0, name);
    let index = index.finish().value();
    assign_subset2(context, list, &[index], value, call)
}

/// `list(...)`: the arguments as a list, named by their tags when any has one.
fn list_of<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let count = arguments.len();
    let mut result = context.alloc::<List>(count)?;
    let mut any_names = false;
    for index in 0..count {
        result.set(context, index, arguments.value(index));
        any_names |= arguments.tag(index).r_type() != SexpType::Nil;
    }
    if any_names {
        let mut names = context.alloc::<Str>(count)?;
        for index in 0..count {
            if let Some(tag) = arguments.tag(index).downcast::<Symbol>() {
                let name = tag.read(context).name();
                names.set(context, index, name);
            }
        }
        let names = names.finish().value();
        let mut result = result.owned();
        set_attribute(context, &mut result, symbol(|symbols| symbols.names), names)?;
        return Ok(result.finish().value());
    }
    Ok(result.finish().value())
}

/// `list(...)` of evaluated values, named by the tags of `call`'s arguments when any has one.
/// `None` when the call does not list `values` one for one.
fn direct_list<'a, 's>(
    context: &'a mut Cx<'s>,
    call: Local<'s>,
    values: &[Local<'s>],
) -> R<Option<Gc<'a>>> {
    let Some(call) = call.downcast::<Language>() else {
        return Ok(None);
    };
    let mut count = 0;
    let mut named = false;
    for tag in argument_tags(context, call) {
        count += 1;
        named |= tag.r_type() != SexpType::Nil;
    }
    if count != values.len() {
        return Ok(None);
    }
    let result = context.alloc_unfilled::<List>(count)?;
    let result = result.fill_elements(context, values.iter().map(|value| value.read(context)));
    if !named {
        return Ok(Some(result.finish().read(context).value()));
    }
    let names = context.alloc_unfilled::<Str>(count)?;
    let names = names.fill_elements(
        context,
        argument_tags(context, call).map(|tag| match tag.downcast::<Symbol>() {
            Some(tag) => tag.name(),
            None => handles::blank(),
        }),
    );
    let names = names.finish().value();
    let mut result = result.owned();
    set_attribute(context, &mut result, symbol(|symbols| symbols.names), names)?;
    Ok(Some(result.finish().read(context).value()))
}

/// The tag of each argument of `call`, in order: a symbol, or `NULL`.
#[inline]
fn argument_tags<'a, 's: 'a>(
    context: &'a Cx<'_>,
    call: Local<'s, Language>,
) -> impl Iterator<Item = Gc<'a>> {
    let mut cell = call.read(context).rest();
    std::iter::from_fn(move || {
        let pair = cell.downcast::<Pairlist>()?;
        cell = pair.rest();
        Some(pair.tag())
    })
}

// Lengths and names.

fn do_length<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(1)?;
    let count = handles::length(context, arguments.value(0));
    if count > i32::MAX as usize {
        return Ok(context.scalar::<Real>(count as f64)?.finish().value());
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
        .standard::<T>()
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

fn do_lengths<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let use_names = handles::logical_scalar(context, arguments.value(1)) != 0;
    let listlike =
        matches!(object.r_type(), SexpType::List | SexpType::Expression) || is_s4(object);
    if !listlike
        && !matches!(
            object.r_type(),
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
    let environment = arguments.environment(context)?;
    let count = length_dispatched(context, object, environment)?;
    let mut lengths = context.alloc::<Integer>(count)?;
    if listlike {
        // As in GNU R, a classed object's elements come from its `[[` method, because its
        // `length` method may count elements its storage does not hold (posterior's `rvar` is
        // an empty list of some length).
        let subset2 = match handles::is_object(object) {
            true => {
                let subset2 = handles::primitive("[[").ok_or_else(|| error("no [[ primitive"))?;
                Some(context.root(subset2))
            }
            false => None,
        };
        for index in 0..count {
            // Each element's length is found in a scope of its own, so that a long list does
            // not grow the handle stack.
            let length = context.scope_reading(|context, outer| {
                let (object, environment) = (outer.pass(object), outer.pass(environment));
                let element = match subset2 {
                    Some(subset2) => {
                        let position = handles::integer(context, index as i32 + 1)?;
                        let arguments = [
                            Tagged {
                                tag: null(),
                                value: object,
                            },
                            Tagged {
                                tag: null(),
                                value: position,
                            },
                        ];
                        let subset2 = outer.pass(subset2);
                        handles::apply(context, null(), subset2, &arguments, environment)?
                    }
                    None => handles::item(context, object, index)?,
                };
                // A classed element answers with its own `length` method, S4 ones included
                // (Matrix's sparse vectors).
                length_dispatched(context, element, environment)
            })?;
            lengths.as_mut_slice()[index] = length as i32;
        }
    } else {
        lengths.as_mut_slice().fill(1);
    }
    let mut result = lengths.owned().value();
    if use_names {
        let names = names_of(context, object)?;
        if names.r_type() != SexpType::Nil {
            set_attribute(context, &mut result, symbol(|symbols| symbols.names), names)?;
        }
    }
    let dim = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dim),
    ));
    if dim.r_type() != SexpType::Nil {
        set_attribute(context, &mut result, symbol(|symbols| symbols.dim), dim)?;
        let dimnames = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.dimnames),
        ));
        if dimnames.r_type() != SexpType::Nil {
            set_attribute(
                context,
                &mut result,
                symbol(|symbols| symbols.dimnames),
                dimnames,
            )?;
        }
    }
    Ok(result.finish())
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

fn do_matrix<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let data = materialize(context, arguments.value(0))?;
    if !(data.r_type().is_atomic()
        || matches!(data.r_type(), SexpType::List | SexpType::Expression))
    {
        return Err(error(format!(
            "'data' must be of a vector type, was '{}'",
            handles::type_name(data)
        )));
    }
    let by_row = match handles::logical_scalar(context, arguments.value(3)) {
        NA_LOGICAL => return Err(error("invalid 'byrow' argument")),
        by_row => by_row == 1,
    };
    let count = handles::length(context, data);
    let nrow = matrix_extent(context, arguments, 1, 5, "nrow")?;
    let ncol = matrix_extent(context, arguments, 2, 6, "ncol")?;
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
            handles::warning(
                context,
                format!(
                    "data length [{count}] is not a sub-multiple or multiple of the number of rows [{rows}]"
                ),
            )?;
        } else if unrelated(cols) {
            handles::warning(
                context,
                format!(
                    "data length [{count}] is not a sub-multiple or multiple of the number of columns [{cols}]"
                ),
            )?;
        } else if total != count {
            handles::warning(
                context,
                format!("data length differs from size of matrix: [{count} != {rows} x {cols}]"),
            )?;
        }
    } else if count > 1 && total == 0 {
        handles::warning(context, "non-empty data for zero-extent matrix")?;
    }
    let mut result = recycled(context, data, total, rows, by_row)?;
    let mut dim = context.alloc::<Integer>(2)?;
    dim.as_mut_slice()
        .copy_from_slice(&[rows as i32, cols as i32]);
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dim.finish().value(),
    )?;
    with_dimnames(context, result, arguments.value(4))
}

/// The `nrow` or `ncol` of a `matrix()` call, argument `index`, or `None` when the caller left
/// it out, which the flag at `missing_index` says.
fn matrix_extent<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
    index: usize,
    missing_index: usize,
    name: &str,
) -> R<Option<usize>> {
    if handles::logical_scalar(context, arguments.value(missing_index)) == 1 {
        return Ok(None);
    }
    let extent = arguments.value(index);
    if !matches!(
        extent.r_type(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) || handles::inherits(context, extent, "factor")
    {
        return Err(error("non-numeric matrix extent"));
    }
    if extent.r_type() == SexpType::Real && handles::length(context, extent) > 0 {
        let (_, out_of_range) = coerce::real_to_int(real_element(context, extent, 0)?);
        if out_of_range {
            handles::warning(context, "NAs introduced by coercion to integer range")?;
        }
    }
    match handles::integer_scalar(context, extent) {
        NA_INTEGER => Err(error(format!("invalid '{name}' value (too large or NA)"))),
        extent if extent < 0 => Err(error(format!("invalid '{name}' value (< 0)"))),
        extent => Ok(Some(extent as usize)),
    }
}

/// A new vector of `data`'s type with `total` elements: `data` recycled in the column order of
/// a matrix of `rows` rows, or in its row order when `by_row`, and `NA` throughout when `data`
/// is empty. `NULL` gives logical `NA`s.
fn recycled<'s>(
    context: &mut Cx<'s>,
    data: Local<'s>,
    total: usize,
    rows: usize,
    by_row: bool,
) -> R<Owned<'s>> {
    let count = handles::length(context, data);
    if count > 0 {
        macro_rules! place_as {
            ($($kind:ty),*) => {$(
                if let Some(source) = data.standard::<$kind>() {
                    let mut result = context.alloc::<$kind>(total)?;
                    place(result.as_mut_slice(), source.as_slice(context), rows, by_row);
                    return Ok(result.owned().value());
                }
            )*};
        }
        place_as!(Real, Integer, Logical, Complex, Raw);
        macro_rules! place_items_as {
            ($($kind:ty),*) => {$(
                if let Some(source) = data.standard::<$kind>() {
                    let result = context
                        .alloc_unfilled::<$kind>(total)?
                        .fill_from(context, source, |target, source| {
                            place(target, source, rows, by_row)
                        });
                    return Ok(result.owned().value());
                }
            )*};
        }
        place_items_as!(Str, List, Expression);
    }
    if data.r_type() == SexpType::Nil {
        let result = context
            .alloc_unfilled::<Logical>(total)?
            .fill(std::iter::repeat(NA_LOGICAL));
        return Ok(result.owned().value());
    }
    let cols = total.checked_div(rows).unwrap_or(0);
    // One row or one column is the same order either way.
    let by_row = by_row && rows > 1 && cols > 1;
    let positions = (0..total).map(move |target| match count {
        // A position past the end selects `NA`.
        0 => usize::MAX,
        _ if by_row => ((target % rows) * cols + target / rows) % count,
        _ => target % count,
    });
    let result = select_iter(context, data, positions, total)?;
    owned(context, result)
}

/// Fills `target`, a matrix of `rows` rows, with `source` recycled in column order, or in row
/// order when `by_row`.
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

/// `result` with `dimnames` unless they are empty. GNU R reports bad ones against the closure
/// that called the internal.
fn with_dimnames<'s>(
    context: &mut Cx<'s>,
    mut result: Owned<'s>,
    dimnames: Local<'s>,
) -> R<Local<'s>> {
    if dimnames.r_type() != SexpType::Nil && handles::length(context, dimnames) > 0 {
        assign_dimnames(context, &mut result, dimnames, None)?;
    }
    Ok(result.finish())
}

fn do_array<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let data = materialize(context, arguments.value(0))?;
    let dims = handles::integer_vector(context, arguments.value(1))?;
    let extents = dims.as_slice(context);
    let total = if crate::system::r_at_least_release(4, 6, 2) {
        dims_total(extents).map_err(|message| arguments.error(message))? as usize
    } else {
        extents
            .iter()
            .map(|&extent| extent.max(0) as usize)
            .product()
    };
    let mut result = recycled(context, data, total, 1, false)?;
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dims.value(),
    )?;
    with_dimnames(context, result, arguments.value(2))
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

fn do_transpose<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let dimnames_symbol = symbol(|symbols| symbols.dimnames);
    let (rows, cols, dimnames) = match handles::dims(context, object) {
        Some(dims) if matches!(dims.len(), 1 | 2) => {
            let dimnames = context.root(get_attribute(context, object, dimnames_symbol));
            (dims[0], dims.get(1).copied().unwrap_or(1), dimnames)
        }
        None => {
            let names = names_of(context, object)?;
            let dimnames = if names.r_type() == SexpType::Nil {
                null()
            } else {
                let mut dimnames = context.alloc::<List>(2)?;
                dimnames.set(context, 0, names);
                dimnames.finish().value()
            };
            (handles::length(context, object), 1, dimnames)
        }
        _ => return Err(arguments.error("argument is not a matrix")),
    };
    let Some(mut result) = permuted_copy(context, object, &[rows, cols], &[1, 0])? else {
        return Err(arguments.error("argument is not a matrix"));
    };
    let mut dim = context.alloc::<Integer>(2)?;
    dim.as_mut_slice()
        .copy_from_slice(&[cols as i32, rows as i32]);
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dim.finish().value(),
    )?;
    if let Some(dimnames) = dense::<List>(context, dimnames)? {
        let mut swapped = context.alloc::<List>(2)?;
        if dimnames.len() > 1 {
            swapped.set(context, 0, dimnames.get(context, 1));
        }
        if !dimnames.is_empty() {
            swapped.set(context, 1, dimnames.get(context, 0));
        }
        let mut swapped = swapped.owned().value();
        let names = names_of(context, dimnames.value())?;
        if let Some(names) = dense::<Str>(context, names)?
            && names.len() == 2
        {
            let reversed = context.alloc_unfilled::<Str>(2)?.fill_elements(
                context,
                [names.get(context, 1), names.get(context, 0)].into_iter(),
            );
            set_attribute(
                context,
                &mut swapped,
                symbol(|symbols| symbols.names),
                reversed.finish().value(),
            )?;
        }
        set_attribute(context, &mut result, dimnames_symbol, swapped.finish())?;
    }
    // GNU R keeps the other attributes after `dim` and `dimnames`, and `getParseData` relies on
    // that to read the parse data's `text` after transposing it.
    handles::copy_most_attributes(context, object, &mut result, false)?;
    Ok(result.finish())
}

/// R 4.6's `asplit(x, d.ans, d.call, dn.ans, dn.call, n.call, n.ans, drop)`, where `x` is
/// already permuted so that each piece is a contiguous run of `n.call` elements. The pieces
/// keep the remaining dimensions unless `drop`, and the list takes the split ones.
fn do_asplit<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let x = arguments.value(0);
    let (answer_dims, piece_dims) = (arguments.value(1), arguments.value(2));
    let (answer_names, piece_names) = (arguments.value(3), arguments.value(4));
    let piece_length = handles::real_scalar(context, arguments.value(5)).max(0.0) as usize;
    let pieces = handles::real_scalar(context, arguments.value(6)).max(0.0) as usize;
    let drop = handles::logical_scalar(context, arguments.value(7)) == 1;
    let vector = matches!(
        x.r_type(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
            | SexpType::Expression
    );
    if !vector || pieces.saturating_mul(piece_length) > handles::length(context, x) {
        return Err(error("invalid 'x' argument"));
    }
    if !drop && handles::length(context, piece_dims) == 0 {
        return Err(error("length-0 dimension vector is invalid"));
    }
    let named = |context: &mut Cx<'s>, names: Local<'s>| {
        names.r_type() == SexpType::List && handles::length(context, names) > 0
    };
    let piece_dims = if drop {
        null()
    } else {
        coerce(context, piece_dims, SexpType::Integer)?
    };
    let piece_names = named(context, piece_names).then_some(piece_names);
    let mut result = context.alloc::<List>(pieces)?;
    for piece in 0..pieces {
        let start = piece * piece_length;
        let mut element = select_iter(context, x, start..start + piece_length, piece_length)?;
        if !drop {
            let mut shaped = owned(context, element)?;
            set_attribute(
                context,
                &mut shaped,
                symbol(|symbols| symbols.dim),
                piece_dims,
            )?;
            if let Some(piece_names) = piece_names {
                set_attribute(
                    context,
                    &mut shaped,
                    symbol(|symbols| symbols.dimnames),
                    piece_names,
                )?;
            }
            element = shaped.finish();
        }
        result.set(context, piece, element);
    }
    let mut result = result.owned().value();
    let answer_dims = coerce(context, answer_dims, SexpType::Integer)?;
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        answer_dims,
    )?;
    if named(context, answer_names) {
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.dimnames),
            answer_names,
        )?;
    }
    Ok(result.finish())
}

/// A new vector of `object`'s elements with the dimensions `dims` permuted into the order
/// `order` (result dimension `k` is source dimension `order[k]`), without attributes, or `None`
/// for a type that has no array layout to permute (GNU R permutes only atomic vectors and lists).
fn permuted_copy<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    dims: &[usize],
    order: &[usize],
) -> R<Option<Owned<'s>>> {
    // C code can set a `dim` that disagrees with the length, which would index out of bounds.
    // Within the length, no stride or offset can overflow.
    let length = handles::length(context, object);
    let total = dims
        .iter()
        .try_fold(1usize, |product, &extent| product.checked_mul(extent))
        .filter(|&total| total <= length)
        .ok_or_else(|| error("dims do not match the length of object"))?;
    macro_rules! permuted {
        ($kind:ty) => {{
            let source = dense::<$kind>(context, object)?
                .ok_or_else(|| error("internal error: a vector has no standard form"))?;
            let result = context.alloc_unfilled::<$kind>(total)?;
            let source = &source.as_slice(context)[..total];
            // SAFETY: a permutation writes each of the `total` slots once.
            let result = unsafe { result.fill_slots(|slots| permute(source, slots, dims, order)) };
            Ok(Some(result.owned().value()))
        }};
    }
    macro_rules! permuted_items {
        ($kind:ty) => {{
            let source = dense::<$kind>(context, object)?
                .ok_or_else(|| error("internal error: a vector has no standard form"))?;
            let result = context.alloc_unfilled::<$kind>(total)?.fill_from(
                context,
                source,
                |slots, source| permute(&source[..total], slots, dims, order),
            );
            Ok(Some(result.owned().value()))
        }};
    }
    match object.r_type() {
        SexpType::Real => permuted!(Real),
        SexpType::Integer => permuted!(Integer),
        SexpType::Logical => permuted!(Logical),
        SexpType::Complex => permuted!(Complex),
        SexpType::Raw => permuted!(Raw),
        SexpType::String => permuted_items!(Str),
        SexpType::List => permuted_items!(List),
        _ => Ok(None),
    }
}

/// A tile is 32 source rows by 256 columns. One row of it reads one cache line from each column
/// (16 KiB for doubles), and the next seven rows hit the same lines in L1. Long column runs
/// amortize the loop overhead that square 32 by 32 tiles pay, which made transposes 15 to 30%
/// slower.
const ROW_TILE: usize = 32;
const COLUMN_TILE: usize = 256;

/// A slot [`permute`] writes: an element, or one not yet initialized.
trait Writable<T: Copy> {
    fn put(&mut self, value: T);
}

impl<T: Copy> Writable<T> for T {
    #[inline(always)]
    fn put(&mut self, value: T) {
        *self = value;
    }
}

impl<T: Copy> Writable<T> for MaybeUninit<T> {
    #[inline(always)]
    fn put(&mut self, value: T) {
        self.write(value);
    }
}

/// Writes `source`, a column-major array with extents `dims`, into every slot of `target` with
/// the dimensions permuted by `order`. The source's first dimension is contiguous in `source`
/// and the result's first dimension, `order[0]`, is contiguous in `target`, so those two are
/// walked as blocked tiles to keep both streams in cache; the other dimensions step an offset
/// pair. When the two are the same dimension, each step copies one contiguous run.
fn permute<T: Copy>(
    source: &[T],
    target: &mut [impl Writable<T>],
    dims: &[usize],
    order: &[usize],
) {
    let total: usize = dims.iter().product();
    assert!(source.len() >= total && target.len() == total);
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
            for (slot, &value) in target[to..to + rows]
                .iter_mut()
                .zip(&source[from..from + rows])
            {
                slot.put(value);
            }
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
                            slot.put(strided[step * column_stride]);
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

fn do_aperm<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let permutation = arguments.value(1);
    let resize = handles::logical_scalar(context, arguments.value(2)) != 0;
    let dim_symbol = symbol(|symbols| symbols.dim);
    let dimnames_symbol = symbol(|symbols| symbols.dimnames);
    // C code can set an empty `dim`, which leaves nothing to permute.
    let Some(dims) = handles::dims(context, object).filter(|dims| !dims.is_empty()) else {
        return Err(arguments.error("invalid first argument, must be an array"));
    };
    let rank = dims.len();
    let order: SmallVec<[usize; 6]> = if permutation.r_type() == SexpType::Nil {
        (0..rank).rev().collect()
    } else {
        let length = handles::length(context, permutation);
        if length != rank {
            return Err(arguments.error(format!("'perm' is of wrong length {length} (!= {rank})")));
        }
        let mut order = SmallVec::with_capacity(rank);
        if permutation.r_type() == SexpType::String {
            let dimnames = context.root(get_attribute(context, object, dimnames_symbol));
            let dimension_names = names_of(context, dimnames)?;
            let Some(dimension_names) = dense::<Str>(context, dimension_names)? else {
                return Err(arguments.error("'a' does not have named dimnames"));
            };
            let wanted = dense::<Str>(context, permutation)?
                .ok_or_else(|| error("internal error: a vector has no standard form"))?;
            for index in 0..rank {
                let name = wanted.get(context, index);
                let Some(found) = (0..rank.min(dimension_names.len()))
                    .find(|&candidate| same_text(dimension_names.get(context, candidate), name))
                else {
                    return Err(arguments.error(format!(
                        "'perm[{}]' does not match a dimension name",
                        index + 1
                    )));
                };
                order.push(found);
            }
        } else {
            // Only a number names a dimension; any other type reads as `NA`.
            let numbers = match permutation.r_type() {
                SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::Raw => {
                    let numbers = coerce(context, permutation, SexpType::Real)?;
                    dense::<Real>(context, numbers)?
                }
                _ => None,
            };
            for index in 0..rank {
                let value = numbers.map_or(NA_REAL, |numbers| numbers.as_slice(context)[index]);
                if !(value >= 1.0 && value < rank as f64 + 1.0) {
                    return Err(arguments.error("value out of range in 'perm'"));
                }
                order.push(value as usize - 1);
            }
        }
        let mut seen: SmallVec<[bool; 6]> = smallvec![false; rank];
        for &dimension in &order {
            if std::mem::replace(&mut seen[dimension], true) {
                return Err(arguments.error("invalid 'perm' argument"));
            }
        }
        order
    };
    let mut result = permuted_copy(context, object, &dims, &order)?.ok_or_else(|| {
        arguments.error(format!(
            "unimplemented type '{}' in 'aperm'\n",
            object.r_type().name()
        ))
    })?;
    if resize {
        let mut dim = context.alloc::<Integer>(rank)?;
        for (slot, &dimension) in dim.as_mut_slice().iter_mut().zip(&order) {
            *slot = dims[dimension] as i32;
        }
        let mut dim = dim.owned().value();
        let old_dim = context.root(get_attribute(context, object, dim_symbol));
        let dim_names = names_of(context, old_dim)?;
        if dim_names.r_type() != SexpType::Nil {
            let permuted = select_iter(context, dim_names, order.iter().copied(), rank)?;
            set_attribute(context, &mut dim, symbol(|symbols| symbols.names), permuted)?;
        }
        set_attribute(context, &mut result, dim_symbol, dim.finish())?;
        let dimnames = context.root(get_attribute(context, object, dimnames_symbol));
        if dimnames.r_type() != SexpType::Nil {
            let permuted = select_iter(context, dimnames, order.iter().copied(), rank)?;
            let mut permuted = owned(context, permuted)?;
            let names = names_of(context, dimnames)?;
            if names.r_type() != SexpType::Nil {
                let names = select_iter(context, names, order.iter().copied(), rank)?;
                set_attribute(
                    context,
                    &mut permuted,
                    symbol(|symbols| symbols.names),
                    names,
                )?;
            }
            set_attribute(context, &mut result, dimnames_symbol, permuted.finish())?;
        }
    } else {
        let dim = context.root(get_attribute(context, object, dim_symbol));
        set_attribute(context, &mut result, dim_symbol, dim)?;
    }
    Ok(result.finish())
}

fn do_drop<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let Some(dims) = handles::dims(context, object) else {
        return Ok(object);
    };
    let kept: SmallVec<[usize; 4]> = (0..dims.len())
        .filter(|&dimension| dims[dimension] != 1)
        .collect();
    if kept.len() == dims.len() {
        return Ok(object);
    }
    let mut result = handles::attribute_copy(context, object)?;
    let dimnames = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    ));
    let dimnames = dense::<List>(context, dimnames)?;
    remove_attribute(context, &mut result, symbol(|symbols| symbols.dim));
    remove_attribute(context, &mut result, symbol(|symbols| symbols.dimnames));
    if kept.len() > 1 {
        let mut dim = context.alloc::<Integer>(kept.len())?;
        for (slot, &dimension) in dim.as_mut_slice().iter_mut().zip(&kept) {
            *slot = dims[dimension] as i32;
        }
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.dim),
            dim.finish().value(),
        )?;
        // As in GNU R's `DropDims`, the kept dimensions keep their dimnames and the names of
        // those dimnames, unless none of the kept dimensions has any.
        if let Some(dimnames) = dimnames
            && kept
                .iter()
                .any(|&dimension| named(context, dimnames, dimension).is_some())
        {
            let mut list = context.alloc::<List>(kept.len())?;
            for (index, &dimension) in kept.iter().enumerate() {
                list.set(context, index, dimnames.get(context, dimension));
            }
            let mut list = list.owned().value();
            let dimension_names = names_of(context, dimnames.value())?;
            if let Some(dimension_names) = dense::<Str>(context, dimension_names)? {
                let kept_names = context.alloc_unfilled::<Str>(kept.len())?;
                let dimension_names = dimension_names.read(context);
                let kept_names = kept_names.fill_elements(
                    context,
                    kept.iter().map(|&dimension| dimension_names.get(dimension)),
                );
                set_attribute(
                    context,
                    &mut list,
                    symbol(|symbols| symbols.names),
                    kept_names.finish().value(),
                )?;
            }
            set_attribute(
                context,
                &mut result,
                symbol(|symbols| symbols.dimnames),
                list.finish(),
            )?;
        }
    } else if let Some(dimnames) = dimnames {
        // A plain vector takes the names of the one dimension kept, or for a single element
        // those of the only dimension that has names: with names on several dimensions, none
        // of them names a single element.
        let names = match kept[..] {
            [dimension] => named(context, dimnames, dimension),
            _ => {
                let mut named =
                    (0..dims.len()).filter_map(|dimension| named(context, dimnames, dimension));
                match (named.next(), named.next()) {
                    (Some(names), None) => Some(names),
                    _ => None,
                }
            }
        };
        if let Some(names) = names {
            let names = context.root(names);
            set_attribute(context, &mut result, symbol(|symbols| symbols.names), names)?;
        }
    }
    Ok(result.finish())
}

/// The names `dimnames` holds for `dimension`, if it has any.
fn named<'a, 's>(
    context: &'a Cx<'s>,
    dimnames: Local<'s, List>,
    dimension: usize,
) -> Option<Gc<'a>> {
    let names = dimnames.get(context, dimension);
    (names.r_type() != SexpType::Nil).then_some(names)
}

fn do_cbind<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    bind(context, arguments, false)
}

fn do_rbind<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    bind(context, arguments, true)
}

/// `cbind` and `rbind` for vectors and matrices; data frames dispatch to their R methods.
fn bind<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>, by_row: bool) -> R<Local<'s>> {
    // The arguments arrive unevaluated, because `deparse.level` names a vector argument after
    // the expression that produced it, which only the promise still knows.
    if arguments.is_empty() {
        return Err(arguments.error("argument \"deparse.level\" is missing"));
    }
    let environment = arguments.environment(context)?;
    let level = arguments.value(0);
    let level_missing = match level.downcast::<Symbol>() {
        Some(level) => handles::is_missing(context, level, environment)?,
        None => false,
    };
    let level_value = handles::evaluate(context, level, environment)?;
    let deparse_level = handles::integer_scalar(context, level_value);
    let dots = context.root(symbol(|symbols| symbols.dots));
    let mut items: Vec<DotsArgument<'s>> = Vec::with_capacity(arguments.len() - 1);
    for index in 1..arguments.len() {
        let expression = arguments.value(index);
        if expression.raw() == dots.raw() {
            items.extend(handles::dots_arguments(context, environment)?);
        } else {
            let value = handles::evaluate(context, expression, environment)?;
            items.push(DotsArgument {
                tag: arguments.tag(index),
                value,
                expression,
            });
        }
    }
    let generic = if by_row { "rbind" } else { "cbind" };
    if let Some(result) = bind_method(
        context,
        arguments,
        environment,
        generic,
        &items,
        (!level_missing).then_some(level_value),
    )? {
        return Ok(result);
    }
    // An S4 argument hands the call to the methods package's hidden `cbind` or `rbind`, which
    // dispatch on `cbind2` and `rbind2`. Their default methods bind with a deparse level of
    // -1, which marks the call as already coming from there.
    if deparse_level != -1
        && items.iter().any(|item| is_s4(item.value))
        && let Some(methods) = handles::registered_namespace(context, "methods")?
        && let Some(methods) = methods.downcast::<Environment>()
    {
        let name = handles::install_bytes(context, generic.as_bytes())?;
        if let Some(function) = handles::frame_symbol_value(context, methods.value(), name)?
            && function.r_type() == SexpType::Closure
        {
            let forwarded = forwarded_items(context, &items, Some(level_value))?;
            return handles::apply(
                context,
                arguments.expression(),
                function,
                &forwarded,
                environment,
            );
        }
    }
    // GNU R's `cbind` and `rbind` from its `bind.c`, written once for both directions: the
    // extent is the length along the bound vectors (rows for `cbind`), and each argument adds
    // lines across it (columns for `cbind`). Only a call of NULLs alone gives NULL.
    if items
        .iter()
        .all(|item| item.value.r_type() == SexpType::Nil)
    {
        return Ok(null());
    }
    // An argument that is no vector makes the result a list, as in GNU R: a call or pairlist
    // binds its elements, and anything else (a symbol, a function) is one element.
    for item in &mut items {
        match item.value.r_type() {
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
                item.value = coerce(context, item.value, SexpType::List)?;
            }
            _ => {
                let mut wrapped = context.alloc::<List>(1)?;
                wrapped.set(context, 0, item.value);
                item.value = wrapped.finish().value();
            }
        }
    }
    // Starting below raw, so that binding only raw vectors gives a raw matrix.
    let kind = match items.iter().fold(SexpType::Nil, |kind, item| {
        coerce::higher_type(kind, item.value.r_type())
    }) {
        SexpType::Nil => SexpType::Logical,
        SexpType::Expression => {
            return Err(error("cannot create a matrix from type 'expression'"));
        }
        kind => kind,
    };
    let (along, across) = if by_row { (1, 0) } else { (0, 1) };
    let mut shapes: Vec<Shape> = Vec::with_capacity(items.len());
    for item in &items {
        let length = handles::length(context, item.value);
        shapes.push(match handles::dims(context, item.value) {
            Some(dims) if dims.len() == 2 => {
                // C code can set a `dim` that disagrees with the length, which would copy past
                // the end of the matrix.
                if dims[0]
                    .checked_mul(dims[1])
                    .is_none_or(|total| total > length)
                {
                    return Err(error("dims do not match the length of object"));
                }
                Shape::Matrix {
                    rows: dims[0],
                    extent: dims[along],
                    lines: dims[across],
                }
            }
            _ => Shape::Vector { length },
        });
    }
    // A zero-length vector, NULL included, adds a line only when no argument has a nonzero
    // extent, so that `cbind(matrix(0, 0, 2), NULL)` has three columns.
    let min_length = usize::from(shapes.iter().any(|shape| match *shape {
        Shape::Matrix { extent, .. } => extent > 0,
        Shape::Vector { length } => length > 0,
    }));
    let direction = if by_row { "columns" } else { "rows" };
    let mut matrix_extent: Option<usize> = None;
    let mut vector_extent = 0usize;
    let mut total = 0usize;
    for (index, shape) in shapes.iter().enumerate() {
        match *shape {
            Shape::Matrix { extent, lines, .. } => {
                if matrix_extent.is_some_and(|known| known != extent) {
                    return Err(error(format!(
                        "number of {direction} of matrices must match (see arg {})",
                        index + 1
                    )));
                }
                matrix_extent = Some(extent);
                total += lines;
            }
            Shape::Vector { length } if length >= min_length => {
                vector_extent = vector_extent.max(length);
                total += 1;
            }
            Shape::Vector { .. } => {}
        }
    }
    let extent = matrix_extent.unwrap_or(vector_extent);
    let names_vector = |item: &DotsArgument<'_>| {
        item.tag.r_type() != SexpType::Nil
            || deparse_level == 2
            || (deparse_level == 1 && item.expression.r_type() == SexpType::Symbol)
    };
    let mut has_bound_names = false;
    let mut matrix_has_other_names = false;
    let mut longest_names = 0usize;
    let mut warned = false;
    for (index, (item, shape)) in items.iter().zip(&shapes).enumerate() {
        match *shape {
            Shape::Matrix { .. } => {
                let dimnames =
                    get_attribute(context, item.value, symbol(|symbols| symbols.dimnames));
                if let Some(dimnames) = dimnames.standard::<List>()
                    && dimnames.len() == 2
                {
                    has_bound_names |= dimnames.get(across).r_type() != SexpType::Nil;
                    // GNU R records the first matrix's extent here, so a zero extent counts
                    // as none.
                    matrix_has_other_names |=
                        dimnames.get(along).r_type() != SexpType::Nil && extent > 0;
                }
            }
            Shape::Vector { length } => {
                if !warned && length > 0 && (length > extent || !extent.is_multiple_of(length)) {
                    warned = true;
                    // The warning names the `rbind` or `cbind` closure call, not the internal.
                    let call = arguments.closure_call(context);
                    handles::warning_in(
                        context,
                        call,
                        format!(
                            "number of {direction} of result is not a multiple of vector \
                             length (arg {})",
                            index + 1
                        ),
                    )?;
                }
                has_bound_names |= length >= min_length && names_vector(item);
                let names = names_of(context, item.value)?;
                longest_names = longest_names.max(handles::length(context, names));
            }
        }
    }
    let has_other_names = matrix_has_other_names || longest_names == extent;
    let (rows, cols) = if by_row {
        (total, extent)
    } else {
        (extent, total)
    };
    let mut sources: Vec<Option<Local<'s>>> = Vec::with_capacity(items.len());
    for (item, shape) in items.iter().zip(&shapes) {
        let used = match *shape {
            Shape::Matrix { .. } => true,
            Shape::Vector { length } => length >= min_length && length > 0,
        };
        sources.push(match used {
            true => Some(coerce(context, item.value, kind)?),
            false => None,
        });
    }
    let layout = Layout {
        by_row,
        rows,
        cols,
        extent,
        min_length,
    };
    let mut result = match kind {
        SexpType::Raw => bound_numbers::<Raw>(context, &layout, &shapes, &sources)?,
        SexpType::Logical => bound_numbers::<Logical>(context, &layout, &shapes, &sources)?,
        SexpType::Integer => bound_numbers::<Integer>(context, &layout, &shapes, &sources)?,
        SexpType::Real => bound_numbers::<Real>(context, &layout, &shapes, &sources)?,
        SexpType::Complex => bound_numbers::<Complex>(context, &layout, &shapes, &sources)?,
        SexpType::String => bound_pointers::<Str>(context, &layout, &shapes, &sources)?,
        _ => bound_pointers::<List>(context, &layout, &shapes, &sources)?,
    };
    let mut bound_names = match has_bound_names {
        true => Some(context.alloc::<Str>(total)?),
        false => None,
    };
    let mut other_names = null();
    let mut offset = 0usize;
    for (item, shape) in items.iter().zip(&shapes) {
        if let Shape::Matrix { lines, .. } = *shape {
            let dimnames = context.root(get_attribute(
                context,
                item.value,
                symbol(|symbols| symbols.dimnames),
            ));
            // Only C code can set dimnames that are not one entry per dimension.
            let dimnames = dense::<List>(context, dimnames)?.filter(|dimnames| dimnames.len() == 2);
            if has_other_names
                && other_names.r_type() == SexpType::Nil
                && let Some(dimnames) = dimnames
            {
                other_names = context.root(dimnames.get(context, along));
            }
            if let Some(bound_names) = &mut bound_names {
                let names = dimnames.map(|dimnames| context.root(dimnames.get(context, across)));
                let names = match names {
                    Some(names) => {
                        dense::<Str>(context, names)?.filter(|names| names.len() >= lines)
                    }
                    None => None,
                };
                for line in 0..lines {
                    match names {
                        Some(names) => {
                            bound_names.set(context, offset + line, names.get(context, line))
                        }
                        None => bound_names.set(context, offset + line, handles::blank()),
                    }
                }
            }
            offset += lines;
            continue;
        }
        let Shape::Vector { length } = *shape else {
            continue;
        };
        if length < min_length {
            continue;
        }
        let names = names_of(context, item.value)?;
        if has_other_names
            && other_names.r_type() == SexpType::Nil
            && names.r_type() != SexpType::Nil
            && handles::length(context, names) == extent
        {
            other_names = names;
        }
        if let Some(bound_names) = &mut bound_names {
            if let Some(tag) = item.tag.downcast::<Symbol>() {
                let name = tag.read(context).name();
                bound_names.set(context, offset, name);
            } else if deparse_level == 1
                && let Some(expression) = item.expression.downcast::<Symbol>()
            {
                let name = expression.read(context).name();
                bound_names.set(context, offset, name);
            } else if deparse_level == 2 {
                let text = handles::deparse_one_line(context, item.expression);
                handles::set_text(context, bound_names, offset, &text)?;
            } else {
                bound_names.set(context, offset, handles::blank());
            }
        }
        offset += 1;
    }
    let mut dim = context.alloc::<Integer>(2)?;
    dim.as_mut_slice()
        .copy_from_slice(&[rows as i32, cols as i32]);
    set_attribute(
        context,
        &mut result,
        symbol(|symbols| symbols.dim),
        dim.finish().value(),
    )?;
    if has_bound_names || has_other_names {
        let bound_names = bound_names.map_or_else(null, |names| names.finish().value());
        let (first, second) = if by_row {
            (bound_names, other_names)
        } else {
            (other_names, bound_names)
        };
        let mut dimnames = context.alloc::<List>(2)?;
        dimnames.set(context, 0, first);
        dimnames.set(context, 1, second);
        set_attribute(
            context,
            &mut result,
            symbol(|symbols| symbols.dimnames),
            dimnames.finish().value(),
        )?;
    }
    Ok(result.finish())
}

/// The S3 method `bind` dispatches to, applied, as GNU R's `do_bind` finds it: the first
/// classed argument with a method picks it. Through R 4.3 the method gets the `...` arguments
/// alone; from R 4.4 on, a supplied `deparse.level` comes first, tagged.
fn bind_method<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
    environment: Local<'s, Environment>,
    generic: &str,
    items: &[DotsArgument<'s>],
    level: Option<Local<'s>>,
) -> R<Option<Local<'s>>> {
    for item in items {
        if !handles::is_object(item.value) {
            continue;
        }
        let classes =
            context.root_answer(|context| crate::dispatch::implicit_class(context, item.value))?;
        let Some(classes) = dense::<Str>(context, classes)? else {
            continue;
        };
        for index in 0..classes.len() {
            let mut name = format!("{generic}.").into_bytes();
            name.extend_from_slice(classes.get(context, index).bytes());
            let name = handles::install_bytes(context, &name)?;
            let base = handles::base_namespace();
            let Some(method) = handles::lookup_method(context, name, environment, base)? else {
                continue;
            };
            let level = level.filter(|_| crate::system::r_at_least(4, 4));
            let forwarded = forwarded_items(context, items, level)?;
            return handles::apply(
                context,
                arguments.expression(),
                method,
                &forwarded,
                environment,
            )
            .map(Some);
        }
    }
    Ok(None)
}

/// `items` as the arguments of a method `bind` forwards to: each a promise of its expression
/// already forced to its value, so that `data.frame` inside `cbind.data.frame` names a column
/// after its argument expression, after `deparse.level` when it is given.
fn forwarded_items<'s>(
    context: &mut Cx<'s>,
    items: &[DotsArgument<'s>],
    level: Option<Local<'s>>,
) -> R<Vec<Tagged<'s>>> {
    let mut forwarded = Vec::with_capacity(items.len() + 1);
    if let Some(level) = level {
        let tag = handles::install_bytes(context, b"deparse.level")?;
        forwarded.push(Tagged {
            tag: context.root(tag).value(),
            value: level,
        });
    }
    for item in items {
        let value = handles::forced_promise(context, item.expression, item.value)?;
        forwarded.push(Tagged {
            tag: item.tag,
            value: value.value(),
        });
    }
    Ok(forwarded)
}

/// How an argument of `cbind` or `rbind` lays out: a matrix adds `lines` lines of `extent`
/// elements and has `rows` rows; a vector adds at most one line.
#[derive(Clone, Copy)]
enum Shape {
    Matrix {
        rows: usize,
        extent: usize,
        lines: usize,
    },
    Vector {
        length: usize,
    },
}

/// The result `bind` fills: its extents, the extent of each line, whether lines are rows, and
/// the length a vector needs to add a line.
struct Layout {
    by_row: bool,
    rows: usize,
    cols: usize,
    extent: usize,
    min_length: usize,
}

/// Calls `copy(target, source, count)` for each run of elements an argument of `shape` puts
/// into the result, its first line at line `offset`, and returns the number of lines it adds.
/// A run is contiguous in both vectors: a whole matrix or a vector's recycled stretch for
/// `cbind`, one element for `rbind`.
#[inline(always)]
fn place_lines(
    layout: &Layout,
    shape: Shape,
    offset: usize,
    mut copy: impl FnMut(usize, usize, usize),
) -> usize {
    let Layout {
        by_row,
        rows,
        extent,
        min_length,
        ..
    } = *layout;
    match shape {
        Shape::Matrix {
            rows: source_rows,
            lines,
            ..
        } => {
            if by_row {
                for line in 0..lines {
                    for position in 0..extent {
                        copy(
                            offset + line + position * rows,
                            line + position * source_rows,
                            1,
                        );
                    }
                }
            } else {
                copy(offset * rows, 0, extent * lines);
            }
            lines
        }
        Shape::Vector { length } if length < min_length => 0,
        Shape::Vector { length: 0 } => 1,
        Shape::Vector { length } => {
            if by_row {
                for position in 0..extent {
                    copy(offset + position * rows, position % length, 1);
                }
            } else {
                for start in (0..extent).step_by(length) {
                    copy(offset * rows + start, 0, length.min(extent - start));
                }
            }
            1
        }
    }
}

/// The elements of a `bind` result of an atomic type, with each source already of that type.
fn bound_numbers<'s, T: Atomic>(
    context: &mut Cx<'s>,
    layout: &Layout,
    shapes: &[Shape],
    sources: &[Option<Local<'s>>],
) -> R<Owned<'s>> {
    let mut result = context.alloc::<T>(layout.rows * layout.cols)?;
    let mut offset = 0;
    for (&shape, &source) in shapes.iter().zip(sources) {
        let source = match source {
            Some(source) => dense::<T>(context, source)?,
            None => None,
        };
        let target = result.as_mut_slice();
        offset += match source {
            Some(source) => {
                let source = source.as_slice(context);
                place_lines(layout, shape, offset, |to, from, count| {
                    target[to..to + count].copy_from_slice(&source[from..from + count]);
                })
            }
            None => place_lines(layout, shape, offset, |_, _, _| {}),
        };
    }
    Ok(result.owned().value())
}

/// [`bound_numbers`] for a character vector or a list.
fn bound_pointers<'s, T: Elements>(
    context: &mut Cx<'s>,
    layout: &Layout,
    shapes: &[Shape],
    sources: &[Option<Local<'s>>],
) -> R<Owned<'s>> {
    let mut result = context.alloc::<T>(layout.rows * layout.cols)?;
    let mut offset = 0;
    for (&shape, &source) in shapes.iter().zip(sources) {
        let source = match source {
            Some(source) => dense::<T>(context, source)?,
            None => None,
        };
        offset += match source {
            Some(source) => place_lines(layout, shape, offset, |to, from, count| {
                for index in 0..count {
                    result.set(context, to + index, source.get(context, from + index));
                }
            }),
            None => place_lines(layout, shape, offset, |_, _, _| {}),
        };
    }
    Ok(result.owned().value())
}

fn do_rev<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let count = handles::length(context, object);
    let reversed = select_iter(context, object, (0..count).rev(), count)?;
    let names = names_of(context, object)?;
    if names.r_type() == SexpType::Nil {
        return Ok(reversed);
    }
    let names = select_iter(context, names, (0..count).rev(), count)?;
    let mut result = owned(context, reversed)?;
    set_attribute(context, &mut result, symbol(|symbols| symbols.names), names)?;
    Ok(result.finish())
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
fn do_max_col<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let matrix = arguments.value(0);
    let dims = context.root(get_attribute(
        context,
        matrix,
        symbol(|symbols| symbols.dim),
    ));
    let dims = handles::integer_vector(context, dims)?;
    let (rows, columns) = match *dims.as_slice(context) {
        [rows, columns] => (rows.max(0) as usize, columns.max(0) as usize),
        _ => (handles::length(context, matrix), 1),
    };
    let method = handles::integer_scalar(context, arguments.value(1));
    let matrix = coerce(context, matrix, SexpType::Real)?;
    let matrix = dense::<Real>(context, matrix)?
        .ok_or_else(|| error("internal error: a coercion to double gave another type"))?;
    let mut result = context.alloc::<Integer>(rows)?;
    let drawing = column_maxima(
        matrix.as_slice(context),
        rows,
        columns,
        method,
        result.as_mut_slice(),
    );
    // GNU R reads the generator's state at the first tie, so a call without one leaves
    // `.Random.seed` as it was, even unset.
    if !drawing.is_empty() {
        handles::with_rng_state(context, |context| {
            draw_ties(
                matrix.as_slice(context),
                rows,
                columns,
                &drawing,
                result.as_mut_slice(),
                &mut SessionDeviates,
            )
        })?;
    }
    Ok(result.finish().value())
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
    let drawing = column_maxima(matrix, rows, columns, method, maxes);
    // GNU R reads the generator's state at the first tie, so a call without one leaves
    // `.Random.seed` as it was, even unset.
    if !drawing.is_empty() {
        crate::random::get_rng_state()?;
        draw_ties(matrix, rows, columns, &drawing, maxes, &mut SessionDeviates)?;
        crate::random::put_rng_state()?;
    }
    Ok(())
}

/// [`max_col`]'s scan, column by column over blocks of rows so that it reads the matrix in
/// memory order. The maximum a row's scan follows never depends on a draw, so with `method` 1
/// the scan only counts the ties that would draw and returns the rows with any, for
/// [`draw_ties`].
fn column_maxima(
    matrix: &[f64],
    rows: usize,
    columns: usize,
    method: i32,
    maxes: &mut [i32],
) -> Vec<usize> {
    const BLOCK: usize = 256;
    let mut drawing = Vec::new();
    let Some(matrix) = matrix.get(..rows * columns).filter(|_| columns > 0) else {
        maxes.fill(NA_INTEGER);
        return drawing;
    };
    let random = method == 1;
    let last = method == 3;
    let count = rows.min(maxes.len());
    for (block, out) in maxes[..count].chunks_mut(BLOCK).enumerate() {
        let start = block * BLOCK;
        let column = |column: usize| &matrix[column * rows + start..][..out.len()];
        let mut maximum = [0.0f64; BLOCK];
        let maximum = &mut maximum[..out.len()];
        let mut chosen = [0u32; BLOCK];
        let chosen = &mut chosen[..out.len()];
        maximum.copy_from_slice(column(0));
        if random {
            // The largest finite magnitude, or NaN once the row has met a missing value.
            let mut tolerance = [0.0f64; BLOCK];
            let tolerance = &mut tolerance[..out.len()];
            for index in 0..columns {
                for (largest, &value) in tolerance.iter_mut().zip(column(index)) {
                    *largest = if value.is_nan() || largest.is_nan() {
                        f64::NAN
                    } else if value.is_finite() {
                        largest.max(value.abs())
                    } else {
                        *largest
                    };
                }
            }
            for largest in tolerance.iter_mut() {
                *largest *= 1e-5;
            }
            let mut draws = [0u32; BLOCK];
            let draws = &mut draws[..out.len()];
            for index in 1..columns {
                for row in 0..out.len() {
                    let value = column(index)[row];
                    let (current, tolerance) = (maximum[row], tolerance[row]);
                    if value > current + tolerance {
                        maximum[row] = value;
                        chosen[row] = index as u32;
                    } else if value >= current - tolerance {
                        draws[row] += 1;
                    }
                }
            }
            for row in 0..out.len() {
                out[row] = if tolerance[row].is_nan() {
                    NA_INTEGER
                } else {
                    if draws[row] > 0 {
                        drawing.push(start + row);
                    }
                    chosen[row] as i32 + 1
                };
            }
        } else {
            // A missing value makes the maximum NaN, which no later value replaces.
            for index in 1..columns {
                for (row, &value) in column(index).iter().enumerate() {
                    let current = maximum[row];
                    let better = value > current || (last && value == current);
                    chosen[row] = if better { index as u32 } else { chosen[row] };
                    maximum[row] = if better || value.is_nan() {
                        value
                    } else {
                        current
                    };
                }
            }
            for ((out, &maximum), &chosen) in out.iter_mut().zip(&*maximum).zip(&*chosen) {
                *out = if maximum.is_nan() {
                    NA_INTEGER
                } else {
                    chosen as i32 + 1
                };
            }
        }
    }
    drawing
}

/// The random-tie rows of [`column_maxima`] scanned again one at a time, in row order, which
/// makes the same draws as GNU R's row-by-row loop.
fn draw_ties(
    matrix: &[f64],
    rows: usize,
    columns: usize,
    drawing: &[usize],
    maxes: &mut [i32],
    deviates: &mut impl Deviates,
) -> R<()> {
    for &row in drawing {
        let entry = |column: usize| matrix[row + column * rows];
        let tolerance = 1e-5
            * (0..columns)
                .map(entry)
                .filter(|value| value.is_finite())
                .fold(0.0f64, |largest, value| largest.max(value.abs()));
        let (mut maximum, mut chosen, mut ties) = (entry(0), 0, 1);
        for column in 1..columns {
            let value = entry(column);
            if value > maximum + tolerance {
                maximum = value;
                chosen = column;
                ties = 1;
            } else if value >= maximum - tolerance {
                // Reservoir sampling: each tied column ends up chosen with equal probability.
                ties += 1;
                if f64::from(ties) * deviates.uniform()? < 1.0 {
                    chosen = column;
                }
            }
        }
        maxes[row] = chosen as i32 + 1;
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
    let count = match row_names.standard::<Integer>() {
        Some(rows) if rows.len() == 2 && rows.as_slice()[0] == NA_INTEGER => rows.as_slice()[1],
        _ if row_names.r_type() == SexpType::Nil => 0,
        _ => {
            let row_names = context.root(row_names);
            handles::length(context, row_names) as i32
        }
    };
    handles::integer(context, if kind == 1 { count } else { count.abs() })
}

fn do_copy_df_attr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let mut to = handles::unshared(context, arguments.value(1))?;
    clear_attributes(context, &mut to, false);
    handles::copy_attributes_to(context, arguments.value(0), &mut to)?;
    Ok(to.finish())
}
