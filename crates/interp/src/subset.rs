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
            self, Args, Atomic, Complex, Cx, Expression, Fresh, Gc, Integer, List, Local, Logical,
            Owned, Raw, Real, Str, Symbol, Vector, attribute_cells, attributes_unshared,
            clear_attributes, dense, get_attribute, is_na, is_s4, names_of, null, remove_attribute,
            safe, set_attribute, set_class, symbol, unshared,
        },
        unwind::{R, Unwind, error, error_in, errorcall},
        values::{
            self, Dims, alloc, char_str, get_attr, length, missing_arg, na_string, nil,
            scalar_integer, symbol_name, symbols, unbound,
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
pub unsafe fn vector_element(vector: Sexp, index: usize) -> R<Sexp> {
    // String and list elements read through the ALTREP class's `Elt` method on their own, and
    // logical, integer and double ones through `int_elt` and `real_elt`.
    let vector = if matches!(
        vector.kind(),
        SexpType::String | SexpType::List | SexpType::Logical | SexpType::Integer | SexpType::Real
    ) {
        vector
    } else {
        values::materialize(vector)?
    };
    match vector.kind() {
        SexpType::List | SexpType::Expression => Ok(vector.element(index)),
        SexpType::Logical => values::fresh_logical(values::int_elt(vector, index)?),
        SexpType::Integer => values::scalar_integer(values::int_elt(vector, index)?),
        SexpType::Real => values::scalar_real(values::real_elt(vector, index)?),
        SexpType::Complex => values::scalar_complex(values::complexes(vector)[index]),
        SexpType::String => values::scalar_string_charsxp(vector.element(index)),
        SexpType::Raw => values::scalar_raw(values::raws(vector)[index]),
        SexpType::I64 => crate::numbers::alloc(vector.slice::<i64>()[index]),
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
        // The widening conversions `c()` and growing assignments make, which never warn, write
        // the converted element directly.
        SexpType::Real if matches!(from.kind(), SexpType::Integer | SexpType::Logical) => {
            values::reals_mut(to)[to_index] = int_to_real(values::ints(from)[from_index]);
        }
        SexpType::Integer if from.kind() == SexpType::Logical => {
            values::ints_mut(to)[to_index] = values::ints(from)[from_index];
        }
        _ => {
            let single = vector_element(from, from_index)?;
            let converted = coerce_vector(single, to.kind())?;
            copy_element(converted, 0, to, to_index)?;
        }
    }
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

/// Fills `to[start..start + count]` with `from` recycled, a run of `from` at a time.
unsafe fn copy_recycled(from: Sexp, to: Sexp, start: usize, count: usize) -> R<()> {
    let length = length(from);
    if length == 0 || count == 0 {
        return Ok(());
    }
    match (from.kind(), to.kind()) {
        (SexpType::Logical | SexpType::Integer, SexpType::Logical | SexpType::Integer)
            if from.kind() == to.kind() =>
        {
            let from = values::ints(from);
            for chunk in values::ints_mut(to)[start..start + count].chunks_mut(length) {
                chunk.copy_from_slice(&from[..chunk.len()]);
            }
        }
        (SexpType::Real, SexpType::Real) => {
            let from = values::reals(from);
            for chunk in values::reals_mut(to)[start..start + count].chunks_mut(length) {
                chunk.copy_from_slice(&from[..chunk.len()]);
            }
        }
        (SexpType::Logical | SexpType::Integer, SexpType::Real) => {
            let from = values::ints(from);
            for chunk in values::reals_mut(to)[start..start + count].chunks_mut(length) {
                for (slot, &value) in chunk.iter_mut().zip(from) {
                    *slot = int_to_real(value);
                }
            }
        }
        _ => copy_mapped(
            from,
            to,
            count,
            |position| position % length,
            |position| start + position,
        )?,
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

/// Whether [`select_positions`] reads `count` elements of `vector` one at a time instead of
/// expanding it: always for a compact sequence, whose elements are arithmetic and which GNU R
/// leaves compact when it is subset, and for another long ALTREP number vector of which the
/// selection is a small part, unless its class holds the data already (a wrapper does). A short
/// one expands cheaply, and its class keeps the expansion.
unsafe fn reads_sparsely(vector: Sexp, count: usize) -> R<bool> {
    if !vector.is_altrep()
        || !matches!(
            vector.kind(),
            SexpType::Logical | SexpType::Integer | SexpType::Real
        )
    {
        return Ok(false);
    }
    if values::compact_sequence(vector).is_some() {
        return Ok(true);
    }
    Ok(length(vector) >= 1 << 16
        && count <= length(vector) / 8
        && (crate::interp().hooks.altrep_dataptr_or_null)(vector)?.is_none())
}

/// A vector of the elements at `indices` (`usize::MAX` gives NA).
pub unsafe fn select_elements(vector: Sexp, indices: &[usize]) -> R<Sexp> {
    select_positions(vector, indices.iter().copied(), indices.len())
}

/// A vector of the `count` elements at `positions` (`usize::MAX` gives NA). Taking an iterator
/// lets a caller whose positions follow a rule, such as `rep`, gather without first writing
/// eight bytes per result element into a position list.
pub unsafe fn select_positions(
    vector: Sexp,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Sexp> {
    let positions = positions.take(count);
    // An ALTREP string or list reads only the selected elements through its class's `Elt`
    // method, so `as.character(x)[1:5]` converts five numbers, not all of `x`.
    if vector.is_altrep() && matches!(vector.kind(), SexpType::String | SexpType::List) {
        let extent = length(vector);
        let result = alloc(vector.kind(), count)?;
        crate::heap().root(result);
        for (slot, index) in positions.enumerate() {
            if index < extent {
                values::set_elt(result, slot, vector.element(index));
                crate::unwind::raise_parked()?;
            } else {
                set_na(result, slot);
            }
        }
        return Ok(result);
    }
    // A few elements of a long ALTREP number vector, such as `seq_len(2^31 - 1)[c(1, 10)]`, read
    // one at a time, because expanding a compact sequence can take gigabytes.
    if reads_sparsely(vector, count)? {
        let extent = length(vector);
        let result = alloc(vector.kind(), count)?;
        if let Some((_, first, increment)) = values::compact_sequence(vector) {
            for (slot, index) in positions.enumerate() {
                let number = first + increment * index as f64;
                if vector.kind() == SexpType::Real {
                    values::reals_mut(result)[slot] = if index < extent { number } else { NA_REAL };
                } else {
                    values::ints_mut(result)[slot] = if index < extent {
                        number as i32
                    } else {
                        NA_INTEGER
                    };
                }
            }
            return Ok(result);
        }
        for (slot, index) in positions.enumerate() {
            if vector.kind() == SexpType::Real {
                values::reals_mut(result)[slot] = if index < extent {
                    values::real_elt(vector, index)?
                } else {
                    NA_REAL
                };
            } else {
                values::ints_mut(result)[slot] = if index < extent {
                    values::int_elt(vector, index)?
                } else {
                    NA_INTEGER
                };
            }
        }
        return Ok(result);
    }
    let vector = values::materialize(vector)?;
    let kind = if vector == nil() {
        SexpType::Nil
    } else {
        vector.kind()
    };
    if kind == SexpType::Nil {
        return Ok(nil());
    }
    let (vector, kind) = if matches!(kind, SexpType::Pairlist | SexpType::Language) {
        (values::pairlist_to_list(vector)?, SexpType::List)
    } else {
        (vector, kind)
    };
    let extent = length(vector);
    match kind {
        SexpType::Logical | SexpType::Integer => {
            return gather(
                kind,
                values::ints(vector),
                positions,
                count,
                NA_INTEGER,
                |result| values::ints_mut(result),
            );
        }
        SexpType::Real => {
            return gather(
                kind,
                values::reals(vector),
                positions,
                count,
                NA_REAL,
                |result| values::reals_mut(result),
            );
        }
        SexpType::Raw => {
            return gather(kind, values::raws(vector), positions, count, 0, |result| {
                values::raws_mut(result)
            });
        }
        SexpType::String | SexpType::List | SexpType::Expression => {
            return gather_handles(vector, positions, count);
        }
        _ => {}
    }
    let result = alloc(kind, count)?;
    for (slot, index) in positions.enumerate() {
        if index < extent {
            copy_element(vector, index, result, slot)?;
        } else {
            set_na(result, slot);
        }
    }
    Ok(result)
}

/// `object[index]` for a logical, integer, double, string or list vector whose selection keeps
/// none of its attributes (the caller checks that) and an attribute-free integer, double or
/// logical subscript, gathered straight from the subscript without first resolving it to a
/// position list. `None` defers to the general path, which handles every other shape
/// (negative or zero subscripts, recycled logical subscripts, other element types).
unsafe fn gather_plain(object: Sexp, index: Sexp) -> R<Option<Sexp>> {
    // A selection `select_positions` reads sparsely takes the general path.
    if index == missing_arg()
        || values::has_attributes(index)
        || reads_sparsely(object, length(index))?
    {
        return Ok(None);
    }
    let kind = object.kind();
    if !matches!(
        kind,
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::String | SexpType::List
    ) {
        return Ok(None);
    }
    // A number vector's elements read through its class's data pointer, which a wrapper shares
    // without copying, where materializing would copy the whole vector to drop its attributes.
    let object = if matches!(kind, SexpType::String | SexpType::List) {
        values::materialize(object)?
    } else {
        object
    };
    if let Some((_, offsets)) = sequence_offsets(index) {
        let count = offsets.len();
        return Ok(Some(gather_kind(object, offsets, count)?));
    }
    let index = values::materialize(index)?;
    match index.kind() {
        SexpType::Integer => {
            let positions = values::ints(index);
            if positions
                .iter()
                .all(|&position| position <= 0 && position != NA_INTEGER)
                && let Some(keep) = excluded(
                    length(object),
                    positions.iter().map(|&position| -f64::from(position)),
                )
            {
                return Ok(Some(gather_kept(object, &keep)?));
            }
            // Every subscript must be NA or positive; anything else needs the general rules.
            if positions
                .iter()
                .any(|&position| position <= 0 && position != NA_INTEGER)
            {
                return Ok(None);
            }
            let position_of = |position: &i32| (*position as u32 as usize).wrapping_sub(1);
            Ok(Some(gather_kind(
                object,
                positions.iter().map(position_of),
                positions.len(),
            )?))
        }
        SexpType::Real => {
            let positions = values::reals(index);
            if positions.iter().all(|&position| position < 1.0)
                && let Some(keep) =
                    excluded(length(object), positions.iter().map(|&position| -position))
            {
                return Ok(Some(gather_kept(object, &keep)?));
            }
            if positions.iter().any(|&position| position < 1.0) {
                return Ok(None);
            }
            let position_of = |position: &f64| {
                if !position.is_finite() {
                    NA_POSITION
                } else {
                    position.trunc() as usize - 1
                }
            };
            Ok(Some(gather_kind(
                object,
                positions.iter().map(position_of),
                positions.len(),
            )?))
        }
        SexpType::Logical => {
            let flags = values::ints(index);
            if flags.len() != length(object) {
                return Ok(None);
            }
            let selected = flags.iter().filter(|&&flag| flag != 0).count();
            let positions = flags
                .iter()
                .enumerate()
                .filter_map(|(position, &flag)| match flag {
                    0 => None,
                    NA_LOGICAL => Some(NA_POSITION),
                    _ => Some(position),
                });
            Ok(Some(gather_kind(object, positions, selected)?))
        }
        _ => Ok(None),
    }
}

/// The zero-based offsets a compact sequence subscript selects, computed from its `c(n, n1,
/// inc)` info instead of expanding it, as GNU R reads a compact subscript through `*_ELT`, and
/// the highest of them. `None` unless every element is a whole number of at least one, the only
/// case that needs no further rules; offsets past the end are the caller's to check.
unsafe fn sequence_offsets(index: Sexp) -> Option<(usize, impl ExactSizeIterator<Item = usize>)> {
    let (count, first, increment) = values::compact_sequence(index)?;
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
unsafe fn gather_kept(object: Sexp, keep: &[u64]) -> R<Sexp> {
    let kept = keep.iter().map(|word| word.count_ones() as usize).sum();
    gather_kind(object, kept_positions(keep), kept)
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

/// `object[positions]` for a logical, integer, double, character or list vector, NA (or `NULL`)
/// past the end. A character vector or list must not be ALTREP; a number vector that is reads
/// through its class's data pointer.
unsafe fn gather_kind(
    object: Sexp,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Sexp> {
    let kind = object.kind();
    if matches!(kind, SexpType::String | SexpType::List) {
        return gather_handles(object, positions, count);
    }
    let result = values::alloc_uninit(kind, count)?;
    if kind == SexpType::Real {
        let source = values::reals(object);
        for (slot, position) in values::reals_mut(result).iter_mut().zip(positions) {
            *slot = source.get(position).copied().unwrap_or(NA_REAL);
        }
    } else {
        let source = values::ints(object);
        for (slot, position) in values::ints_mut(result).iter_mut().zip(positions) {
            *slot = source.get(position).copied().unwrap_or(NA_INTEGER);
        }
    }
    // An ALTREP class whose data pointer failed left its error parked and an empty slice.
    crate::unwind::raise_parked()?;
    Ok(result)
}

/// `object[positions]` for a character vector, list or expression vector that is not ALTREP, with
/// NA (or `NULL`) past the end. The elements go straight into the result's slots and are counted
/// as references afterwards, in one pass each, so `positions` must not allocate.
unsafe fn gather_handles(
    object: Sexp,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Sexp> {
    let kind = object.kind();
    let result = alloc(kind, count)?;
    let source = object.slice::<Sexp>();
    let missing = if kind == SexpType::String {
        na_string()
    } else {
        nil()
    };
    for (slot, position) in result.slice_mut::<Sexp>().iter_mut().zip(positions) {
        *slot = source.get(position).copied().unwrap_or(missing);
    }
    crate::heap().count_written_elements(result);
    Ok(result)
}

/// `source[positions]` for an atomic vector of plain elements, with NA past the end.
unsafe fn gather<T: Copy + 'static>(
    kind: SexpType,
    source: &[T],
    positions: impl Iterator<Item = usize>,
    count: usize,
    missing: T,
    output: impl Fn(Sexp) -> &'static mut [T],
) -> R<Sexp> {
    let result = values::alloc_uninit(kind, count)?;
    for (slot, index) in output(result).iter_mut().zip(positions) {
        *slot = source.get(index).copied().unwrap_or(missing);
    }
    Ok(result)
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

/// Resolves a subscript against an extent. `names` are the names to match character
/// subscripts against; unmatched names give positions past the end, with the names to give
/// them when assigning.
unsafe fn resolve(
    index: Sexp,
    extent: usize,
    names: Sexp,
    assigning: bool,
    call: Sexp,
) -> R<(Positions, Vec<Option<Sexp>>)> {
    if index == missing_arg() {
        return Ok(((0..extent).collect(), Vec::new()));
    }
    let index = values::materialize(index)?;
    let index = if values::inherits(index, "factor") {
        coerce_vector(index, SexpType::Integer)?
    } else {
        index
    };
    match index.kind() {
        SexpType::Nil => Ok((Positions::new(), Vec::new())),
        SexpType::Logical => {
            let flags = values::ints(index);
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
            let count = length(index);
            let mut any_negative = false;
            let mut any_positive = false;
            let mut any_na = false;
            for position in 0..count {
                let value = numeric_element(index, position);
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
                if any_positive || any_na {
                    return Err(error_in(
                        call,
                        "only 0's may be mixed with negative subscripts",
                    ));
                }
                let Some(keep) = excluded(
                    extent,
                    (0..count).map(|position| -numeric_element(index, position)),
                ) else {
                    return Err(error_in(
                        call,
                        "only 0's may be mixed with negative subscripts",
                    ));
                };
                let kept = keep.iter().map(|word| word.count_ones() as usize).sum();
                let mut positions = Positions::with_capacity(kept);
                positions.extend(kept_positions(&keep));
                return Ok((positions, Vec::new()));
            }
            let mut positions = Positions::with_capacity(count);
            for position in 0..count {
                let value = numeric_element(index, position);
                if !value.is_finite() {
                    positions.push(NA_POSITION);
                } else if value >= 1.0 {
                    positions.push(value.trunc() as usize - 1);
                }
            }
            Ok((positions, Vec::new()))
        }
        SexpType::String => {
            let count = length(index);
            let mut positions = Positions::with_capacity(count);
            let mut new_names: Vec<Option<Sexp>> = Vec::new();
            let mut appended: Vec<Sexp> = Vec::new();
            for position in 0..count {
                let wanted = index.element(position);
                let found = if wanted == na_string() || names == nil() {
                    None
                } else {
                    (0..length(names)).find(|&candidate| {
                        let name = names.element(candidate);
                        values::same_string(name, wanted) && !values::is_blank(name)
                    })
                };
                match found {
                    Some(found) => positions.push(found),
                    None => {
                        if assigning {
                            let existing = appended.iter().position(|&name| {
                                wanted != na_string() && values::same_string(name, wanted)
                            });
                            let slot = match existing {
                                Some(existing) => extent + existing,
                                None => {
                                    appended.push(wanted);
                                    extent + appended.len() - 1
                                }
                            };
                            positions.push(slot);
                        } else {
                            positions.push(UNMATCHED);
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
        SexpType::Complex => Err(error_in(call, "invalid subscript type 'complex'")),
        SexpType::List => Err(error_in(call, "invalid subscript type 'list'")),
        kind => Err(error_in(
            call,
            format!("invalid subscript type '{}'", kind.name()),
        )),
    }
}

/// The object and zero-based position of `x[i]` or `x[[i]]` with one untagged, attribute-free
/// numeric scalar `i` inside the vector's bounds: the common case GNU R's byte code also
/// answers before resolving subscripts in general.
unsafe fn scalar_position(call: &Call<'_>) -> Option<(Sexp, usize)> {
    let [object, index] = call.args else {
        return None;
    };
    if object.tag != nil() || index.tag != nil() {
        return None;
    }
    let (object, index) = (object.value, index.value);
    let vector =
        object.kind().is_atomic() || matches!(object.kind(), SexpType::List | SexpType::Expression);
    if !vector
        || !matches!(index.kind(), SexpType::Integer | SexpType::Real)
        || object.is_altrep()
        || index.is_altrep()
        || values::has_attributes(index)
        || length(index) != 1
    {
        return None;
    }
    let position = match index.kind() {
        SexpType::Integer => index.slice::<i32>()[0] as f64,
        SexpType::Real => index.slice::<f64>()[0],
        _ => return None,
    };
    // NaN fails both comparisons, and `x[2.7]` truncates to the second element. An Array
    // rejects a position past its last item before truncating, which its own path reports.
    let count = length(object) as f64;
    let within = if object.is_array() {
        position <= count
    } else {
        position < count + 1.0
    };
    (position >= 1.0 && within).then(|| (object, position as usize - 1))
}

/// Splits a builtin's arguments into the object, the subscripts, and `drop`/`exact`.
unsafe fn subset_args(call: &Call<'_>) -> (Sexp, SmallVec<[Sexp; 2]>, Option<Sexp>, Option<Sexp>) {
    let mut object = nil();
    let mut indices = SmallVec::new();
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
    // `[` on an Array makes an Array, which `subset` builds.
    if let Some((object, position)) = scalar_position(call)
        && !values::has_attributes(object)
        && !object.is_array()
        && object.kind().is_atomic()
    {
        return vector_element(object, position);
    }
    let (object, indices, drop, _) = subset_args(call);
    let drop = drop.is_none_or(|drop| values::as_logical_scalar(drop) != 0);
    subset(object, &indices, drop, call.call)
}

/// `x[i, ...]` with positional subscripts on a subject that is not an object: the same
/// selection as [`do_subset`] without building the argument list, and a numeric scalar
/// position in an attribute-free atomic vector read straight out.
pub(crate) unsafe fn direct_subset(call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let [object, indices @ ..] = args else {
        return Ok(None);
    };
    let object = *object;
    if object.is_object()
        || is_native_value(object)
        || indices.iter().any(|&index| is_native_value(index))
    {
        return Ok(None);
    }
    if let [index] = indices
        && let Some(element) = crate::element_access::scalar_subset(object, *index)?
    {
        return Ok(Some(element));
    }
    if indices.len() >= 2
        && let Some(element) = scalar_cell(object, indices)?
    {
        return Ok(Some(element));
    }
    subset(object, indices, true, call).map(Some)
}

/// `a[i, j, k]` with one plain in-range position per dimension of an array without dimnames:
/// the element as a length-one vector, found without building index vectors. `None` otherwise.
unsafe fn scalar_cell(object: Sexp, indices: &[Sexp]) -> R<Option<Sexp>> {
    if !matches!(
        object.kind(),
        SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::String
    ) {
        return Ok(None);
    }
    let dim = get_attr(object, symbols().dim);
    if dim.kind() != SexpType::Integer
        || dim.length() as usize != indices.len()
        || get_attr(object, symbols().dimnames) != nil()
    {
        return Ok(None);
    }
    let (mut offset, mut stride) = (0usize, 1usize);
    for (&index, &extent) in indices.iter().zip(dim.slice::<i32>()) {
        let extent = extent.max(0) as usize;
        let Some(position) = matrix_position(index, extent) else {
            return Ok(None);
        };
        offset += position * stride;
        stride *= extent;
    }
    if offset >= length(object) {
        return Ok(None);
    }
    vector_element(object, offset).map(Some)
}

pub unsafe fn subset(object: Sexp, indices: &[Sexp], drop: bool, call: Sexp) -> R<Sexp> {
    if object.is_array() {
        return crate::arrays::subset(object, indices, call);
    }
    // String and list elements read through the class's `Elt` method in `select_elements`; the
    // matrix and array paths read the data directly, so a vector with dimensions materializes.
    let object = if matches!(
        object.kind(),
        SexpType::String | SexpType::List | SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && values::dims(object).is_none()
    {
        object
    } else {
        values::materialize(object)?
    };
    if object.kind() == SexpType::Object && object.has_gp(gp::S4_OBJECT) {
        let data = s4_data_part(object);
        if data == nil() {
            return Err(error_in(call, "object of type 'S4' is not subsettable"));
        }
        return subset(data, indices, drop, call);
    }
    match object.kind() {
        SexpType::Nil => return Ok(nil()),
        SexpType::Environment
        | SexpType::Closure
        | SexpType::Builtin
        | SexpType::Special
        | SexpType::Symbol
        | SexpType::Dots => return Err(not_subsettable(call, object)),
        SexpType::Pairlist | SexpType::Language => {
            let list = values::pairlist_to_list(object)?;
            let result = subset(list, indices, drop, call)?;
            // Subsetting a pairlist gives a list, as in GNU R; a call stays a call.
            if object.kind() == SexpType::Language {
                return values::list_to_pairlist(result, SexpType::Language, false);
            }
            return Ok(result);
        }
        _ => {}
    }
    let dims = values::dims(object);
    if indices.len() >= 2 {
        let Some(dims) = dims else {
            return Err(error_in(call, "incorrect number of dimensions"));
        };
        if dims.len() != indices.len() {
            return Err(error_in(call, "incorrect number of dimensions"));
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
    // A parsed expression's per-element source references are subset along with it, as in GNU
    // R, so `attr(exprs[i], "srcref")` still locates element `i`.
    let srcrefs = get_attr(object, symbols().srcref);
    let srcrefs = (srcrefs.kind() == SexpType::List).then_some(srcrefs);
    // Without names, a source reference or one dimension, the selection keeps no attribute, so
    // the object's other attributes (a matrix's `dim`, a factor's levels) need no further rules.
    if dims.as_ref().is_none_or(|dims| dims.len() != 1)
        && names == nil()
        && srcrefs.is_none()
        && let Some(result) = gather_plain(object, index)?
    {
        return Ok(result);
    }
    let (indices, _) = resolve(index, count, names, false, call)?;
    let result = select_elements(object, &indices)?;
    if names != nil() {
        let new_names = alloc(SexpType::String, indices.len())?;
        for (position, &index) in indices.iter().enumerate() {
            let name = if index < count {
                names.element(index)
            } else {
                na_string()
            };
            values::set_elt(new_names, position, name);
        }
        values::set_attr(result, symbols().names, new_names)?;
    }
    if let Some(srcrefs) = srcrefs
        && index != missing_arg()
    {
        let positions: Vec<usize> = indices
            .iter()
            .map(|&index| {
                if index < length(srcrefs) {
                    index
                } else {
                    usize::MAX
                }
            })
            .collect();
        let selected = select_elements(srcrefs, &positions)?;
        values::set_attr(result, symbols().srcref, selected)?;
    }
    // A one-dimensional array stays one when indexed, unless `drop` leaves at most one element,
    // which GNU R returns as a vector named by the dimnames.
    let one_dimensional =
        dims.as_ref().is_some_and(|dims| dims.len() == 1) && index != missing_arg();
    if one_dimensional && drop && indices.len() <= 1 {
        let dimnames = get_attr(object, symbols().dimnames);
        if names == nil() && dimnames != nil() && dimnames.element(0) != nil() {
            let selected = select_elements(dimnames.element(0), &indices)?;
            values::set_attr(result, symbols().names, selected)?;
        }
    } else if one_dimensional {
        let dim = values::integer_vector(&[indices.len() as i32])?;
        let dim_names = values::names(get_attr(object, symbols().dim));
        if dim_names != nil() {
            values::set_attr(dim, symbols().names, dim_names)?;
        }
        values::set_attr(result, symbols().dim, dim)?;
        let dimnames = get_attr(object, symbols().dimnames);
        if dimnames != nil() && dimnames.element(0) != nil() {
            let selected = select_elements(dimnames.element(0), &indices)?;
            let new_dimnames = values::list_vector(&[selected])?;
            let dimnames_names = values::names(dimnames);
            if dimnames_names != nil() {
                values::set_attr(new_dimnames, symbols().names, dimnames_names)?;
            }
            values::set_attr(result, symbols().dimnames, new_dimnames)?;
            values::remove_attr(result, symbols().names);
        }
    }
    if index == missing_arg() {
        // `x[]` keeps every attribute but the class and `tsp`, which the default method drops
        // as it does for any subscript; a class with a `[` method restores its own. The copy
        // replaces the names set above, so the attributes keep the object's order.
        values::clear_attributes(result);
        values::copy_attributes(object, result)?;
        values::remove_attr(result, symbols().class);
        values::remove_attr(result, symbols().tsp);
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
    // `m[i, ]` on a plain numeric matrix is a strided copy of one row, the shape of a
    // simulation that keeps one row per step. A single column drops to a scalar whose name
    // depends on both dimnames, which the general path works out.
    if drop
        && let ([row, column], &[rows, columns]) = (indices, dims)
        && *column == missing_arg()
        && columns != 1
        && let Some(row) = matrix_position(*row, rows)
        && matches!(
            object.kind(),
            SexpType::Real | SexpType::Integer | SexpType::Logical
        )
    {
        let result = values::alloc_uninit(object.kind(), columns)?;
        if object.kind() == SexpType::Real {
            gather_row(values::reals_mut(result), values::reals(object), row, rows);
        } else {
            gather_row(values::ints_mut(result), values::ints(object), row, rows);
        }
        if dimnames.kind() == SexpType::List && dimnames.element(1) != nil() {
            // The dimnames vector may carry names of its own, which the result's names drop.
            let names = values::shallow_duplicate(dimnames.element(1))?;
            values::clear_attributes(names);
            values::set_attr(result, symbols().names, names)?;
        }
        return Ok(result);
    }
    // `m[, j]` is one contiguous run, the shape `apply` takes once per row or column.
    if drop
        && let ([row, column], &[rows, columns]) = (indices, dims)
        && *row == missing_arg()
        && rows != 1
        && let Some(column) = matrix_position(*column, columns)
        // C code can set a `dim` that disagrees with the length, which the general path reports.
        && (column + 1) * rows <= length(object)
        && matches!(
            object.kind(),
            SexpType::Real | SexpType::Integer | SexpType::Logical
        )
    {
        let start = column * rows;
        let result = values::alloc_uninit(object.kind(), rows)?;
        if object.kind() == SexpType::Real {
            values::reals_mut(result).copy_from_slice(&values::reals(object)[start..start + rows]);
        } else {
            values::ints_mut(result).copy_from_slice(&values::ints(object)[start..start + rows]);
        }
        if dimnames.kind() == SexpType::List && dimnames.element(0) != nil() {
            // The dimnames vector may carry names of its own, which the result's names drop.
            let names = values::shallow_duplicate(dimnames.element(0))?;
            values::clear_attributes(names);
            values::set_attr(result, symbols().names, names)?;
        }
        return Ok(result);
    }
    let mut selections: SmallVec<[Positions; 4]> = SmallVec::with_capacity(dims.len());
    for (dimension, (&index, &extent)) in indices.iter().zip(dims).enumerate() {
        selections.push(dimension_positions(
            call, false, object, dimnames, dimension, index, extent,
        )?);
    }
    let offsets = ArrayOffsets::new(&selections, dims);
    let count = offsets.remaining;
    let result = select_positions(object, offsets, count)?;
    let new_dims: SmallVec<[usize; 4]> =
        selections.iter().map(|selection| selection.len()).collect();
    let new_dimnames: SmallVec<[Sexp; 4]> = if dimnames.kind() == SexpType::List {
        selections
            .iter()
            .enumerate()
            .map(|(dimension, selection)| {
                let names = dimnames.element(dimension);
                // An empty selection has no names: GNU R stores `NULL` for an empty component.
                if names == nil() || selection.is_empty() {
                    Ok(nil())
                } else {
                    select_elements(names, selection)
                }
            })
            .collect::<R<_>>()?
    } else {
        smallvec![nil(); dims.len()]
    };
    let dimnames_names = values::names(dimnames);
    let kept: SmallVec<[usize; 4]> = if drop {
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
    let dim_values: SmallVec<[i32; 4]> = kept
        .iter()
        .map(|&dimension| new_dims[dimension] as i32)
        .collect();
    let dim = values::integer_vector(&dim_values)?;
    let dim_names = values::names(get_attr(object, symbols().dim));
    if dim_names != nil() {
        values::set_attr(dim, symbols().names, select_elements(dim_names, &kept)?)?;
    }
    values::set_attr(result, symbols().dim, dim)?;
    // GNU R keeps dimnames whose components are all NULL unless a dropped dimension forces it
    // to rebuild them.
    let undropped = dimnames != nil() && kept.len() == new_dims.len();
    if undropped
        || kept
            .iter()
            .any(|&dimension| new_dimnames[dimension] != nil())
        || dimnames_names != nil()
    {
        let list: SmallVec<[Sexp; 4]> = kept
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

/// The zero-based position a scalar subscript selects along a matrix dimension of `extent`, when
/// it is a plain positive in-range number; anything else takes the general path, which also
/// reports errors.
unsafe fn matrix_position(index: Sexp, extent: usize) -> Option<usize> {
    if index.is_altrep() || length(index) != 1 || values::has_attributes(index) {
        return None;
    }
    let position = match index.kind() {
        SexpType::Integer if index.slice::<i32>()[0] != NA_INTEGER => {
            f64::from(index.slice::<i32>()[0])
        }
        SexpType::Real => index.slice::<f64>()[0],
        _ => return None,
    };
    // NaN fails both comparisons, and `m[2.7, ]` truncates to the second row.
    (position >= 1.0 && position < extent as f64 + 1.0).then(|| position as usize - 1)
}

/// Copies row `row` of a column-major matrix with `rows` rows into `target`, one element per
/// column.
fn gather_row<T: Copy>(target: &mut [T], matrix: &[T], row: usize, rows: usize) {
    for (slot, &element) in target.iter_mut().zip(matrix.iter().skip(row).step_by(rows)) {
        *slot = element;
    }
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

/// The positions one subscript of a matrix or array selects along its dimension, with the
/// errors of GNU R's `int_arraySubscript`. Unlike a vector, a dimension never stretches: a
/// longer logical subscript and a position past the extent are errors.
unsafe fn dimension_positions(
    call: Sexp,
    assigning: bool,
    object: Sexp,
    dimnames: Sexp,
    dimension: usize,
    index: Sexp,
    extent: usize,
) -> R<Positions> {
    if index.kind() == SexpType::Logical && length(index) > extent {
        return Err(error_in(call, "(subscript) logical subscript too long"));
    }
    if index.kind() == SexpType::String && dimnames == nil() {
        return Err(error_in(call, "no 'dimnames' attribute for array"));
    }
    let names = if dimnames.kind() == SexpType::List {
        dimnames.element(dimension)
    } else {
        nil()
    };
    // GNU R converts a double subscript of a matrix or array to integer before it resolves it,
    // so a value outside the integer range selects `NA` with a warning, where a vector
    // subscript of the same value is quietly out of bounds.
    const LIMIT: f64 = 2_147_483_648.0;
    let index = if index.kind() == SexpType::Real
        && (0..length(index)).any(|position| numeric_element(index, position).abs() >= LIMIT)
    {
        coerce_vector(index, SexpType::Integer)?
    } else {
        index
    };
    let (positions, _) = resolve(index, extent, names, false, call)?;
    let Some((offset, &position)) = positions
        .iter()
        .enumerate()
        .find(|&(_, &position)| position >= extent && position != NA_POSITION)
    else {
        return Ok(positions);
    };
    // GNU R reports the failing element: a name with its dimension, or a position without one.
    let call = if assigning {
        crate::unwind::generated_call(call)?
    } else {
        call
    };
    let named = index.kind() == SexpType::String && offset < length(index);
    let reported = if named {
        vector_element(index, offset)?
    } else {
        values::scalar_real(position as f64 + 1.0)?
    };
    crate::heap().root(reported);
    let bounds = Bounds {
        dimension: named.then_some(dimension),
        ..Bounds::of(object)
    };
    Err(out_of_bounds(
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
unsafe fn array_selections(
    call: Sexp,
    object: Sexp,
    indices: &[Sexp],
    value_count: usize,
) -> R<(Dims, SmallVec<[Positions; 4]>)> {
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
    let dimnames = get_attr(object, symbols().dimnames);
    let mut selections: SmallVec<[Positions; 4]> = SmallVec::new();
    for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
        selections.push(dimension_positions(
            call, true, object, dimnames, dimension, index, extent,
        )?);
    }
    let total: usize = selections.iter().map(|selection| selection.len()).product();
    // A single value skips the NA rows, and a longer one cannot say which of its elements an
    // NA row would have taken. GNU R's matrix assignment checks this before comparing lengths,
    // and its array assignment after.
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
    Ok((dims, selections))
}

unsafe fn do_subset2(call: &Call<'_>) -> R<Sexp> {
    if let Some((object, position)) = scalar_position(call)
        && !object.is_object()
    {
        match object.kind() {
            SexpType::List | SexpType::Expression => return Ok(object.element(position)),
            kind if kind.is_atomic() => return vector_element(object, position),
            _ => {}
        }
    }
    let (object, indices, _, exact) = subset_args(call);
    if indices.is_empty() {
        return Err(call.error("no index specified"));
    }
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
pub(crate) unsafe fn s4_data_part(object: Sexp) -> Sexp {
    let data = get_attr(object, symbols().dot_data);
    if data != nil() {
        return data;
    }
    get_attr(object, symbols().dot_xdata)
}

/// The position the `CHARSXP` `name` matches among `names`: exactly, or with `exact ==
/// Some(false)` (or `None`, which also warns) by a unique partial match. Names compare as text, so
/// a latin1-marked name matches its UTF-8 spelling, and an `NA` name matches nothing. GNU R's
/// `get1index` agrees for `[[`, but its `$` matches an `NA` name as the text "NA" (`backlog.md`).
unsafe fn match_name(names: Sexp, name: Sexp, exact: Option<bool>) -> R<Option<usize>> {
    if names == nil() || name == na_string() {
        return Ok(None);
    }
    let count = length(names);
    for index in 0..count {
        if values::same_string(names.element(index), name) {
            return Ok(Some(index));
        }
    }
    if exact == Some(true) {
        return Ok(None);
    }
    let text = char_str(name);
    let mut found = None;
    for index in 0..count {
        let element = names.element(index);
        if element != na_string() && char_str(element).starts_with(&*text) {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(index);
        }
    }
    if let Some(position) = found
        && exact.is_none()
    {
        conditions::warning(format!(
            "partial match of '{}' to '{}'",
            text,
            char_str(names.element(position))
        ))?;
    }
    Ok(found)
}

pub unsafe fn subset2(object: Sexp, indices: &[Sexp], exact: Option<bool>, call: Sexp) -> R<Sexp> {
    subset2_level(object, indices, exact, call, None, None)
}

/// `[[` on one level. An out-of-bounds error reports `shown` as the object when set, as for a
/// pairlist read through a list copy, and `recursive` as the subscript when set, which is the
/// whole subscript of the recursive `[[` this level belongs to.
unsafe fn subset2_level(
    object: Sexp,
    indices: &[Sexp],
    exact: Option<bool>,
    call: Sexp,
    shown: Option<Sexp>,
    recursive: Option<Sexp>,
) -> R<Sexp> {
    if object.kind() == SexpType::Record {
        return crate::records::subset2(object, indices, call);
    }
    if object.is_array() {
        return crate::arrays::subset2(object, indices, call);
    }
    if object.kind() == SexpType::I64 {
        return Err(error_in(call, "an I64 is a number and cannot be indexed"));
    }
    // String, list and number elements read through the class's `Elt` method, so `x[[i]]` asks
    // for element `i` alone and leaves a compact sequence compact, as GNU R does; other vectors
    // are read as slices.
    let object = if matches!(
        object.kind(),
        SexpType::String | SexpType::List | SexpType::Logical | SexpType::Integer | SexpType::Real
    ) {
        object
    } else {
        values::materialize(object)?
    };
    if object.kind() == SexpType::Object && object.has_gp(gp::S4_OBJECT) {
        let data = s4_data_part(object);
        if data == nil() {
            return Err(error_in(call, "this S4 class is not subsettable"));
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
        crate::moves::reaching_environment()?;
        let index = indices.first().copied().unwrap_or(nil());
        if index.kind() != SexpType::String || length(index) != 1 {
            return Err(error_in(
                call,
                "wrong arguments for subsetting an environment",
            ));
        }
        let symbol = values::install_charsxp(index.element(0))?;
        let value = eval::find_var_in_frame(symbol, object)?;
        if value == unbound() {
            return Ok(nil());
        }
        return value_of(value);
    }
    if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
        let list = values::pairlist_to_list(object)?;
        crate::heap().root(list);
        return subset2_level(list, indices, exact, call, Some(object), recursive);
    }
    if !object.kind().is_vector() {
        return Err(not_subsettable(call, object));
    }
    if indices.len() >= 2 {
        let Some(dims) = values::dims(object) else {
            return Err(error_in(call, "incorrect number of subscripts"));
        };
        if dims.len() != indices.len() {
            return Err(error_in(call, "incorrect number of subscripts"));
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
            let bounds = Bounds {
                dimension: Some(dimension),
                ..Bounds::of(shown.unwrap_or(object))
            };
            let position = single_position(index, extent, names, exact, call, bounds)?;
            offset += position * stride;
            stride *= extent;
        }
        return element_or_value(object, offset);
    }
    let index = values::materialize(indices[0])?;
    let count = length(index);
    if count > 1 && matches!(object.kind(), SexpType::List | SexpType::Expression) {
        // Recursive indexing: each subscript goes one level deeper, through calls and
        // pairlists as well as lists, as GNU R's `isPairList` lets it.
        let mut current = object;
        for position in 0..count {
            let single = vector_element(index, position)?;
            if !matches!(
                current.kind(),
                SexpType::List | SexpType::Expression | SexpType::Pairlist | SexpType::Language
            ) && position + 1 < count
            {
                return Err(error_in(
                    call,
                    format!("recursive indexing failed at level {}\n", position + 1),
                ));
            }
            current = subset2_level(current, &[single], exact, call, None, Some(index))?;
        }
        return Ok(current);
    }
    if count > 1 {
        return Err(error_in(
            call,
            "attempt to select more than one element in vectorIndex",
        ));
    }
    if count == 0 {
        return Err(error_in(
            call,
            "attempt to select less than one element in get1index",
        ));
    }
    let names = values::names(object);
    if index.kind() == SexpType::String
        && matches!(object.kind(), SexpType::List | SexpType::Expression)
        && (index.element(0).bytes().is_empty()
            || match_name(names, index.element(0), exact)?.is_none())
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
    let bounds = Bounds {
        reported: recursive,
        ..Bounds::of(shown.unwrap_or(object))
    };
    let position = single_position(index, length(object), names, exact, call, bounds)?;
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
    bounds: Bounds,
) -> R<usize> {
    let index = if index.kind() == SexpType::Symbol {
        values::scalar_string_charsxp(index.car())?
    } else {
        index
    };
    let matrix = bounds.dimension.is_some();
    match one_position(index, extent, names, exact, call, matrix)? {
        Some(position) => Ok(position),
        None => Err(out_of_bounds(
            call,
            "subscript out of bounds",
            bounds,
            index,
        )),
    }
}

/// GNU R's `get1index`: the position one `[[` subscript selects among `extent` elements, `None`
/// for one that names no element or lies past the end, and an error for one that cannot name a
/// single element. `matrix` says the subscript indexes one dimension.
unsafe fn one_position(
    index: Sexp,
    extent: usize,
    names: Sexp,
    exact: Option<bool>,
    call: Sexp,
    matrix: bool,
) -> R<Option<usize>> {
    if length(index) != 1 {
        return Err(error_in(
            call,
            if length(index) == 0 {
                "attempt to select less than one element in get1index"
            } else {
                "attempt to select more than one element in get1index"
            },
        ));
    }
    // Before 4.6.2 a matrix subscript never counts from the end, so a negative one selects
    // nothing; 4.6.2 applies the vector rule to each extent (PR#19116).
    let counts_from_end = !matrix || crate::system::r_at_least_release(4, 6, 2);
    match index.kind() {
        SexpType::String => {
            let wanted = index.element(0);
            // An empty name means "no name", so it never selects an element.
            if wanted.bytes().is_empty() {
                return Ok(None);
            }
            match_name(names, wanted, exact)
        }
        SexpType::Integer | SexpType::Logical => {
            let value = values::ints(index)[0];
            if value == NA_INTEGER {
                return Ok(None);
            }
            if value < 0 && extent == 2 && value >= -2 && counts_from_end {
                return Ok(Some(if value == -1 { 1 } else { 0 }));
            }
            if value <= 0 {
                let amount = if value < 0 && extent > 1 && counts_from_end {
                    "more"
                } else {
                    "less"
                };
                return Err(error_in(
                    call,
                    format!("attempt to select {amount} than one element in integerOneIndex"),
                ));
            }
            Ok((value as usize <= extent).then(|| value as usize - 1))
        }
        SexpType::Real => {
            let value = values::reals(index)[0];
            if value.is_nan() {
                return Ok(None);
            }
            let value = value.trunc();
            if value < 0.0 {
                if extent == 2 && value >= -2.0 && counts_from_end {
                    return Ok(Some(if value == -1.0 { 1 } else { 0 }));
                }
                return Err(error_in(
                    call,
                    "invalid negative subscript in get1index <real>",
                ));
            }
            if value == 0.0 {
                return Err(error_in(
                    call,
                    "attempt to select less than one element in get1index <real>",
                ));
            }
            Ok((value <= extent as f64).then(|| value as usize - 1))
        }
        SexpType::Symbol => {
            let name = values::scalar_string_charsxp(index.car())?;
            one_position(name, extent, names, exact, call, matrix)
        }
        kind => Err(error_in(
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
    let dimension = bounds
        .dimension
        .map_or(NA_INTEGER, |dimension| dimension as i32 + 1);
    let subscript = match scalar_integer(dimension) {
        Ok(subscript) => subscript,
        Err(failure) => return failure,
    };
    crate::heap().root(subscript);
    conditions::classed_error(
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

// `$` and `@`.

/// The name argument of `$` and `@`: a symbol or string, never evaluated.
pub(crate) unsafe fn name_argument(value: Sexp) -> R<Sexp> {
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
    let object = eval::eval(call.arg(0), call.env()?)?;
    let name = name_argument(call.arg(1))?;
    dollar_value(object, name, call.call, call.env()?)
}

/// `object$name` once the subject is evaluated: an object dispatches with the name as a string.
pub unsafe fn dollar_value(object: Sexp, name: Sexp, call: Sexp, env: Sexp) -> R<Sexp> {
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
        if let Some(value) = crate::dispatch::dispatch_builtin(symbols().dollar, call, &args, env)?
        {
            return Ok(value);
        }
    }
    dollar(object, name, call)
}

pub unsafe fn dollar(object: Sexp, name: Sexp, call: Sexp) -> R<Sexp> {
    if object.kind() == SexpType::Record {
        return crate::records::dollar(object, name.bytes(), call);
    }
    if object.kind() == SexpType::I64 {
        let message = if object.is_array() {
            format!(
                "a List<I64> has no field `{}`",
                String::from_utf8_lossy(name.bytes())
            )
        } else {
            format!(
                "an I64 is a number and has no field `{}`",
                String::from_utf8_lossy(name.bytes())
            )
        };
        return Err(error_in(call, message));
    }
    let object = values::materialize(object)?;
    match object.kind() {
        SexpType::List | SexpType::Expression => {
            let names = values::names(object);
            match match_name(names, name, Some(false))? {
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
            crate::moves::reaching_environment()?;
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
            Err(error_in(call, "$ operator not defined for this S4 class"))
        }
        kind if kind.is_atomic() => Err(error_in(call, "$ operator is invalid for atomic vectors")),
        _ => Err(not_subsettable(call, object)),
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
        SexpType::String if length(value) == 1 => values::install_charsxp(value.element(0)),
        _ => Err(error("invalid type or length for slot name")),
    }
}

/// `object@name`. Only S4 objects have slots: on any other object `@` reads `.Data` (the
/// object itself) or dispatches an S3 `@` method, and otherwise is an error.
unsafe fn do_at(call: &Call<'_>) -> R<Sexp> {
    let object = eval::eval(call.arg(0), call.env()?)?;
    let symbol = slot_symbol(call.arg(1))?;
    // Since R 4.3 `@` is an internal generic for S3 objects, which is how S7 reads properties.
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
        ];
        if let Some(value) =
            crate::dispatch::dispatch_builtin(symbols().at, call.call, &args, call.env()?)?
        {
            return Ok(value);
        }
    }
    if !object.has_gp(gp::S4_OBJECT) && symbol != symbols().dot_data {
        let class = crate::dispatch::class_of(object)?;
        return Err(call.error(format!(
            "no applicable method for `@` applied to an object of class \"{}\"",
            values::first_string(class).unwrap_or_default()
        )));
    }
    get_slot(object, symbol, call.call)
}

/// The elements a matrix subscript selects from an array with `dims`: one row per element and one
/// column per dimension, holding indices or (for a character matrix) dimnames. `None` when
/// `index` is not such a matrix. A row with an `NA` selects `NA`, and a numeric row with a 0
/// selects nothing.
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
    if !numeric && dimnames == nil() {
        return Err(error_in(call, "no 'dimnames' attribute for array"));
    }
    let rows = index_dims[0];
    let mut positions = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut offset = 0usize;
        let mut stride = 1usize;
        let mut na = false;
        let mut dropped = false;
        for (dimension, &extent) in dims.iter().enumerate() {
            let element = row + dimension * rows;
            let position = if numeric {
                let value = numeric_element(index, element).trunc();
                if value.is_nan() {
                    na = true;
                    break;
                }
                if value < 0.0 {
                    return Err(error_in(
                        call,
                        "negative values are not allowed in a matrix subscript",
                    ));
                }
                // GNU R drops a row that names position 0 in the first dimension it reaches,
                // the way a 0 in a vector subscript selects nothing.
                if value == 0.0 {
                    dropped = true;
                    break;
                }
                if value as usize > extent {
                    let bounds = Bounds {
                        dimension: Some(dimension),
                        ..Bounds::of(object)
                    };
                    let reported = values::scalar_real(value)?;
                    return Err(out_of_bounds(
                        call,
                        "subscript out of bounds",
                        bounds,
                        reported,
                    ));
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
                match found {
                    Some(found) => found,
                    None => {
                        let bounds = Bounds {
                            dimension: Some(dimension),
                            ..Bounds::of(object)
                        };
                        let reported = vector_element(index, element)?;
                        return Err(out_of_bounds(
                            call,
                            "subscript out of bounds",
                            bounds,
                            reported,
                        ));
                    }
                }
            };
            offset += position * stride;
            stride *= extent;
        }
        if !dropped {
            positions.push((!na).then_some(offset));
        }
    }
    Ok(Some(positions))
}

/// `R_do_slot`: an S4 slot, which is an attribute, with `.Data` and `.S3Class` special.
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
            return Err(error_in(
                call,
                format!(
                    "cannot get a slot (\"{}\") from an object of type \"{}\"",
                    symbol_name(symbol),
                    object.kind().name()
                ),
            ));
        };
        return Err(error_in(
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
    // GNU R's matrix and array assignments check the subscripts and the value's length before
    // its type. For a value that is not a vector (a function, an environment, a symbol, `NULL`)
    // GNU R reads a length anyway, and what it finds exceeds any selection, so only an empty
    // selection takes it.
    let lengthless = !value.kind().is_vector() && value.kind() != SexpType::Record;
    if lengthless && indices.len() >= 2 && object.kind().is_vector() {
        let count = match value.kind() {
            SexpType::Language | SexpType::Pairlist => length(value),
            _ => usize::MAX,
        };
        array_selections(call, object, indices, count)?;
    }
    // GNU R coerces `NULL` to the value's type before it looks at a subscript, which fails for
    // every type that is not a vector's, except for an environment that binds nothing, whose
    // length of zero makes the assignment a no-op.
    if lengthless && value != nil() && object == nil() {
        return match value.kind() {
            SexpType::Environment if length(value) == 0 => Ok(object),
            SexpType::Pairlist => Err(not_subsettable(call, object)),
            SexpType::Language => Err(error("unimplemented type 'NULL' in 'coercePairList'\n")),
            kind => Err(error(format!(
                "invalid type/length ({}/0) in vector allocation",
                kind.name()
            ))),
        };
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
            let rebuilt = values::list_to_pairlist(result, SexpType::Language, true)?;
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
    // `NULL` has no type to mismatch: a raw vector takes it as it takes a raw value, so the
    // check of its length reports it.
    let value_kind = if value != nil() {
        value.kind()
    } else if object.kind() == SexpType::Raw {
        SexpType::Raw
    } else {
        SexpType::Logical
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
    let value_count = if value == nil() {
        0
    } else if value.kind().is_vector() {
        length(value)
    } else {
        1
    };
    // GNU R's matrix and array assignments check the subscripts and the lengths before the
    // types, so a mismatch of types is reported only for an assignment that is otherwise sound.
    let target_kind = match subassign_type_fix(object.kind(), value_kind) {
        Ok(kind) => kind,
        Err(mismatch) => {
            if indices.len() >= 2 {
                array_selections(call, object, indices, value_count)?;
            }
            return Err(mismatch);
        }
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
    // The subscripts are read against the object as it was: a vector that became a list has
    // lost its dimensions, as in GNU R, where the result of `m[1, 2] <- list(v)` is a plain list.
    if indices.len() >= 2 {
        if value != result
            && let Some(dims) = values::dims(object).filter(|dims| dims.len() == indices.len())
            && let Some(fixed) = slab(indices, &dims, result, value)
        {
            write_slab(result, value, &dims, &fixed);
            return Ok(result);
        }
        let (dims, selections) = array_selections(call, object, indices, value_count)?;
        if selections.iter().any(|selection| selection.is_empty()) {
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
        let result = values::list_to_pairlist(result, object.kind(), true)?;
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
    // `m[[i, j]] <- value` names one cell, which must exist. GNU R checks the value's length
    // before it reads a subscript.
    let is_list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    if !is_list {
        atomic_replacement_check(value)?;
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
        // GNU R reports a subscript that names no single element against the generated call.
        let position = match one_position(index, extent, names, Some(true), call, true) {
            Ok(position) => position,
            Err(Unwind::Error(mut failure)) if failure.call == call => {
                failure.call = crate::unwind::generated_call(call)?;
                return Err(Unwind::Error(failure));
            }
            Err(other) => return Err(other),
        };
        let Some(position) = position else {
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
        };
        offset += position * stride;
        stride *= extent;
    }
    if is_list && value == nil() {
        return Err(incompatible_types(
            object.kind().name(),
            "NULL",
            "[[ assignment",
        ));
    }
    // The cell is assigned by its position in the vector, which keeps the dimensions.
    let position = values::scalar_real(offset as f64 + 1.0)?;
    crate::heap().root(position);
    subassign2_one(object, position, value, call, 1)
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
            if by_row {
                for line in 0..count {
                    let line_offset = offset + line;
                    copy_mapped(
                        source,
                        result,
                        extent,
                        |position| line + position * source_rows,
                        |position| line_offset + position * rows,
                    )?;
                }
            } else {
                // Every matrix has `extent` rows, so its columns are one run of the result's.
                copy_recycled(source, result, offset * rows, extent * count)?;
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
                copy_recycled(source, result, offset * rows, extent)?;
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
