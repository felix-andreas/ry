//! Subsetting and subassignment (`[`, `[[`, `$`, `@` and their replacement forms), `c` and
//! `unlist`, lengths, names, dims, and attributes.

use {
    crate::{
        builtins::{
            Arg, BuiltinDef, Call, Generic, builtin, internal, internal_special, is_native_value,
            special,
        },
        coerce::{self, coerce_vector},
        conditions,
        element_access::direct_subset2,
        eval::{self, value_of},
        unwind::{R, Unwind, error, error_in, errorcall},
        values::{
            self, alloc, char_str, get_attr, length, missing_arg, na_string, nil, scalar_integer,
            string_vector, symbol_name, symbols, unbound,
        },
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp},
    rustc_hash::FxHashSet,
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
    special("@<-", 3, do_at_assign),
    builtin(".subset", -1, do_subset).keep_missing(),
    builtin(".subset2", -1, do_subset2)
        .keep_missing()
        .direct(direct_subset2),
    builtin("list", -1, do_list),
    builtin("length", 1, do_length)
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
    builtin("length<-", 2, do_set_length).generic(Generic::Internal),
    internal("lengths", 2, do_lengths),
    builtin("dim<-", 2, do_set_dim).generic(Generic::Internal),
    builtin("dimnames", 1, do_dimnames).generic(Generic::Internal),
    builtin("dimnames<-", 2, do_set_dimnames).generic(Generic::Internal),
    builtin("levels<-", 2, do_set_levels).generic(Generic::Internal),
    builtin("attributes<-", 2, do_set_attributes),
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
    // Ending a move hands the caller this same value back, which makes it shared.
    crate::moves::modifying(object)?;
    if values::maybe_shared(object) {
        values::shallow_duplicate(object)
    } else {
        Ok(object)
    }
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
unsafe fn copy_prefix(from: Sexp, to: Sexp, count: usize) -> R<()> {
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

/// A vector of the elements at `indices` (`usize::MAX` gives NA).
/// Whether [`select_positions`] reads `count` elements of `vector` one at a time instead of
/// expanding it: for a long ALTREP number vector, such as a compact sequence, of which the
/// selection is a small part. A short one expands cheaply, and its class keeps the expansion.
unsafe fn reads_sparsely(vector: Sexp, count: usize) -> bool {
    vector.is_altrep()
        && matches!(
            vector.kind(),
            SexpType::Logical | SexpType::Integer | SexpType::Real
        )
        && length(vector) >= 1 << 16
        && count <= length(vector) / 8
}

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
    if reads_sparsely(vector, count) {
        let extent = length(vector);
        let result = alloc(vector.kind(), count)?;
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

/// `object[index]` for a plain logical, integer or double vector and an attribute-free integer,
/// double or logical subscript, gathered straight from the subscript without first resolving
/// it to a position list. `None` defers to the general path, which handles every other shape
/// (negative or zero subscripts, recycled logical subscripts, other element types).
unsafe fn gather_plain(object: Sexp, index: Sexp) -> R<Option<Sexp>> {
    // A selection `select_positions` reads sparsely takes the general path.
    if index == missing_arg()
        || values::has_attributes(index)
        || values::has_attributes(object)
        || reads_sparsely(object, length(index))
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
    let object = values::materialize(object)?;
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
                if position.is_nan() {
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
        // A subscript between -1 and 1 truncates to zero, which selects nothing.
        if value.is_nan() || value <= -1.0 {
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

/// `object[positions]` for a logical, integer, double, character or list vector that is not
/// ALTREP, NA (or `NULL`) past the end.
unsafe fn gather_kind(
    object: Sexp,
    positions: impl Iterator<Item = usize>,
    count: usize,
) -> R<Sexp> {
    let kind = object.kind();
    if matches!(kind, SexpType::String | SexpType::List) {
        let result = alloc(kind, count)?;
        let source = object.slice::<Sexp>();
        let missing = if kind == SexpType::String {
            na_string()
        } else {
            nil()
        };
        let heap = crate::heap();
        for (slot, position) in positions.take(count).enumerate() {
            heap.set_element(
                result,
                slot,
                source.get(position).copied().unwrap_or(missing),
            );
        }
        return Ok(result);
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
                if value.is_nan() {
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
                        name == wanted && !values::is_blank(name)
                    })
                };
                match found {
                    Some(found) => positions.push(found),
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
    subset(object, indices, true, call).map(Some)
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
                return values::list_to_pairlist(result, SexpType::Language);
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
    if dims.is_none()
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
        let names = if dimnames.kind() == SexpType::List {
            dimnames.element(dimension)
        } else {
            nil()
        };
        // Unlike a vector, a dimension never stretches to a longer logical subscript.
        if index.kind() == SexpType::Logical && length(index) > extent {
            return Err(error_in(call, "(subscript) logical subscript too long"));
        }
        let (positions, _) = resolve(index, extent, names, false, call)?;
        if let Some((offset, &position)) = positions
            .iter()
            .enumerate()
            .find(|&(_, &position)| position >= extent && position != NA_POSITION)
        {
            return Err(array_out_of_bounds(
                call, object, dimension, index, offset, position,
            ));
        }
        selections.push(positions);
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
                if names == nil() {
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

/// An out-of-bounds array subscript in `[` or `[<-`. GNU R reports the failing element: a name
/// with its dimension, or a position without one.
unsafe fn array_out_of_bounds(
    call: Sexp,
    object: Sexp,
    dimension: usize,
    index: Sexp,
    offset: usize,
    position: usize,
) -> Unwind {
    let named = index.kind() == SexpType::String && offset < length(index);
    let reported = if named {
        vector_element(index, offset)
    } else {
        values::scalar_real(position as f64 + 1.0)
    };
    let reported = match reported {
        Ok(reported) => reported,
        Err(failure) => return failure,
    };
    crate::heap().root(reported);
    let bounds = Bounds {
        dimension: named.then_some(dimension),
        ..Bounds::of(object)
    };
    out_of_bounds(call, "subscript out of bounds", bounds, reported)
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
    // String and list elements read through the class's `Elt` method, so `x[[i]]` asks for
    // element `i` alone, as GNU R does; other vectors are read as slices.
    let object = if matches!(object.kind(), SexpType::String | SexpType::List) {
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
                None => Err(out_of_bounds(
                    call,
                    "subscript out of bounds",
                    bounds,
                    index,
                )),
            }
        }
        SexpType::Integer | SexpType::Logical => {
            let value = values::ints(index)[0];
            if value == NA_INTEGER {
                return Err(out_of_bounds(
                    call,
                    "subscript out of bounds",
                    bounds,
                    index,
                ));
            }
            // Before 4.6.2 a matrix subscript never counts from the end, so a negative one
            // selects nothing; 4.6.2 applies the vector rule to each extent (PR#19116).
            let matrix = bounds.dimension.is_some() && !crate::system::r_at_least_release(4, 6, 2);
            if value < 0 && extent == 2 && value >= -2 && !matrix {
                return Ok(if value == -1 { 1 } else { 0 });
            }
            if value <= 0 {
                let amount = if value < 0 && extent > 1 && !matrix {
                    "more"
                } else {
                    "less"
                };
                return Err(error_in(
                    call,
                    format!("attempt to select {amount} than one element in integerOneIndex"),
                ));
            }
            if value as usize > extent {
                return Err(out_of_bounds(
                    call,
                    "subscript out of bounds",
                    bounds,
                    index,
                ));
            }
            Ok(value as usize - 1)
        }
        SexpType::Real => {
            let value = values::reals(index)[0];
            if value.is_nan() {
                return Err(out_of_bounds(
                    call,
                    "subscript out of bounds",
                    bounds,
                    index,
                ));
            }
            let value = value.trunc();
            if value < 0.0 {
                let matrix =
                    bounds.dimension.is_some() && !crate::system::r_at_least_release(4, 6, 2);
                if extent == 2 && value >= -2.0 && !matrix {
                    return Ok(if value == -1.0 { 1 } else { 0 });
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
            if value > extent as f64 {
                return Err(out_of_bounds(
                    call,
                    "subscript out of bounds",
                    bounds,
                    index,
                ));
            }
            Ok(value as usize - 1)
        }
        SexpType::Symbol => {
            let name = values::scalar_string_charsxp(index.car())?;
            single_position(name, extent, names, exact, call, bounds)
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

    // The methods package checks the assignment against the class definition, using the
    // implicit class of an object without a class attribute, so `x@a <- 1` on a list fails.
    if let Some(check) = interp_methods_function("checkAtAssignment")? {
        let class = crate::dispatch::class_of(object)?;
        crate::heap().root(class);
        let name = values::scalar_string_charsxp(symbol.car())?;
        crate::heap().root(name);
        let value_class = crate::dispatch::class_of(value)?;
        crate::heap().root(value_class);
        let check_call = values::call(check, &[class, name, value_class])?;
        eval::eval(check_call, values::base_env())?;
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
    let mut object = nil();
    let mut indices: SmallVec<[Sexp; 4]> = SmallVec::new();
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
    // GNU R leaves `NULL`, or an empty vector given `NULL`, alone whatever the index, so
    // `x <- NULL; x[1] <- character(0)` is not the "replacement has length zero" error. An
    // empty vector given an empty vector is handled below, where it may still be coerced.
    if (object == nil() || object.kind().is_vector())
        && length(object) == 0
        && (value == nil() || (object == nil() && value.kind().is_vector() && length(value) == 0))
    {
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
    if indices.len() >= 2 {
        let Some(dims) = values::dims(result) else {
            return Err(error_in(call, "incorrect number of subscripts on matrix"));
        };
        if dims.len() != indices.len() {
            return Err(error_in(call, "incorrect number of subscripts"));
        }
        if value != result
            && let Some(fixed) = slab(indices, &dims, result, value)
        {
            write_slab(result, value, &dims, &fixed);
            return Ok(result);
        }
        let dimnames = get_attr(result, symbols().dimnames);
        let mut selections: SmallVec<[Positions; 4]> = SmallVec::new();
        for (dimension, (&index, &extent)) in indices.iter().zip(dims.iter()).enumerate() {
            let names = if dimnames.kind() == SexpType::List {
                dimnames.element(dimension)
            } else {
                nil()
            };
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
    if let Some(dims) = values::dims(result)
        && let Some(positions) = matrix_index_positions(result, &dims, index, call)?
    {
        // As with a vector subscript, a single value skips the NA rows, and a longer one
        // cannot say which of its elements an NA row would have taken.
        if value_count > 1 && positions.iter().any(Option::is_none) {
            return Err(error_in(
                call,
                "NAs are not allowed in subscripted assignments",
            ));
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
    if !positions.is_empty() && !positions.len().is_multiple_of(value_count) {
        conditions::warning_call(
            call,
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

/// `target[index] <- value` written straight from an attribute-free integer or double subscript
/// whose positions all lie inside `target`, when `value` already has `target`'s type and its
/// length divides the number of positions. Returns false, having written nothing, for every
/// other shape, which the general path handles with GNU R's full rules.
unsafe fn scatter_plain(target: Sexp, index: Sexp, value: Sexp, value_count: usize) -> R<bool> {
    let kind = target.kind();
    if index == missing_arg()
        || values::has_attributes(index)
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
    if !offsets.len().is_multiple_of(value_count) {
        return Ok(false);
    }
    match kind {
        SexpType::Real => recycle_into(
            values::reals_mut(target),
            offsets.iter().copied(),
            values::reals(value),
        ),
        SexpType::Logical | SexpType::Integer => recycle_into(
            values::ints_mut(target),
            offsets.iter().copied(),
            values::ints(value),
        ),
        _ => {
            let source = value.slice::<Sexp>();
            let heap = crate::heap();
            for (position, &offset) in offsets.iter().enumerate() {
                heap.set_element(target, offset, source[position % value_count]);
            }
        }
    }
    Ok(true)
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
        let grown = alloc_with_room(SexpType::String, new_length)?;
        let names = values::materialize(names)?;
        crate::heap().copy_elements(grown, 0, names, 0, count);
        values::set_attr(result, symbols().names, grown)?;
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
    let mut object = nil();
    let mut indices: SmallVec<[Sexp; 4]> = SmallVec::new();
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
    if let Some(dims) = values::dims(object)
        && dims.len() == indices.len()
    {
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
    }
    let is_list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    if is_list || object == nil() || !value.kind().is_atomic() || length(value) != 1 {
        let list = if object == nil() {
            alloc(SexpType::List, 0)?
        } else if is_list {
            modifiable(object)?
        } else {
            atomic_replacement_check(value)?;
            coerce_vector(object, SexpType::List)?
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
            let wanted = index.element(0).bytes();
            if wanted.is_empty() {
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
    let name = name_argument(call.arg(1))?;
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
    if object == nil() {
        if new_length > 0 {
            conditions::warning_call(call.call, "length of NULL cannot be changed")?;
        }
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
    let count = length(object);
    // GNU returns the object itself when the length is unchanged, so every
    // attribute survives; the methods package relies on this to compare
    // signatures with `identical`.
    if count == new_length {
        return Ok(object);
    }
    let result = alloc(object.kind(), new_length)?;
    copy_prefix(object, result, new_length.min(count))?;
    for position in count..new_length {
        set_na(result, position);
    }
    let names = values::names(object);
    if names != nil() {
        let new_names = alloc(SexpType::String, new_length)?;
        copy_prefix(
            values::materialize(names)?,
            new_names,
            new_length.min(count),
        )?;
        values::set_attr(result, symbols().names, new_names)?;
    }
    Ok(result)
}

unsafe fn do_lengths(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let use_names = values::as_logical_scalar(call.arg(1)) != 0;
    let count = length(object);
    let result = alloc(SexpType::Integer, count)?;
    crate::heap().root(result);
    for index in 0..count {
        let element = match object.kind() {
            // A classed element answers with its own `length` method, S4 ones included
            // (Matrix's sparse vectors).
            SexpType::List | SexpType::Expression => {
                dispatch_length(object.element(index), call.env()?)?
            }
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

/// GNU R's `namesgets`, behind `attr<-`, `attributes<-` and the C API: names are converted to
/// character without dispatch, keeping the value's own attributes as `coerceVector` does, and
/// padded with `NA` to the object's length.
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
    let is_vector_or_list = matches!(
        object.kind(),
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
    if !is_vector_or_list && !object.has_gp(gp::S4_OBJECT) {
        return Err(error("names() applied to a non-vector"));
    }
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
    } else if names == object {
        // `names(x) <- x` must not make the names attribute the vector itself, which would be a
        // cycle that `serialize` recurses into forever (GNU R's `R_FixupRHS`).
        values::shallow_duplicate(names)?
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
    let object_length = length(object) as i64;
    if crate::system::r_at_least_release(4, 6, 2) {
        let product = dims_total(call, dims)?;
        if product != object_length {
            return Err(call.error(format!(
                "dims [product {product}] do not match the length of object [{object_length}]"
            )));
        }
    } else {
        if length(dims) == 0 {
            return Err(call.error("length-0 dimension vector is invalid"));
        }
        // The first offending extent names the problem.
        if let Some(&bad) = values::ints(dims)
            .iter()
            .find(|&&value| value == NA_INTEGER || value < 0)
        {
            return Err(call.error(if bad == NA_INTEGER {
                "the dims contain missing values"
            } else {
                "the dims contain negative values"
            }));
        }
        let product = values::ints(dims).iter().fold(1i64, |product, &extent| {
            product.saturating_mul(extent as i64)
        });
        if product != object_length {
            // GNU R before 4.6.2 prints the extents only when both fit in an `int`.
            if product > i32::MAX as i64 || object_length > i32::MAX as i64 {
                return Err(call.error("dims do not match the length of object"));
            }
            return Err(call.error(format!(
                "dims [product {product}] do not match the length of object [{object_length}]"
            )));
        }
    }
    values::remove_attr(object, symbols().names);
    values::remove_attr(object, symbols().dimnames);
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
    if dimnames == nil() || (dimnames.kind() == SexpType::List && length(dimnames) == 0) {
        values::remove_attr(object, symbols().dimnames);
        return Ok(());
    }
    let Some(dims) = values::dims(object) else {
        return Err(error_in(call, "'dimnames' applied to non-array"));
    };
    if dimnames.kind() != SexpType::List {
        return Err(error_in(call, "'dimnames' must be a list"));
    }
    if length(dimnames) > dims.len() {
        return Err(error_in(
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
            return Err(error_in(
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
                values::blank_string()
            };
            values::set_elt(padded, index, name);
        }
        values::set_attr(result, symbols().names, padded)?;
    }
    values::set_attr(object, symbols().dimnames, result)
}

unsafe fn do_set_levels(call: &Call<'_>) -> R<Sexp> {
    let object = modifiable(call.arg(0))?;
    let levels = call.arg(1);
    if levels.kind() == SexpType::String {
        let mut seen = FxHashSet::default();
        for index in 0..length(levels) {
            if !seen.insert(levels.element(index)) {
                return Err(call.error(format!("factor level [{}] is duplicated", index + 1)));
            }
        }
    }
    values::set_attr(object, symbols().levels, levels)?;
    Ok(object)
}

// Attributes.

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
            rho: values::global_env(),
            frame: crate::NO_SLOTS,
            reusable: None,
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
        // GNU R's `classgets`: printing or dispatching on a class that is not character would
        // read its elements as strings.
        if value != nil() && value.kind() != SexpType::String {
            return Err(error_in(call, "attempt to set invalid 'class' attribute"));
        }
        if value != nil() {
            let factor = (0..length(value)).any(|index| value.element(index).bytes() == b"factor");
            if factor && object.kind() != SexpType::Integer {
                return Err(error_in(
                    call,
                    "adding class \"factor\" to an invalid object",
                ));
            }
        }
        return crate::dispatch::set_class_attribute(object, value);
    }
    if symbol == symbols.row_names {
        return set_row_names(object, value);
    }
    values::set_attr(object, symbol, value)
}

/// GNU R's `row_names_gets`: row names `1:n`, and the double `c(NA, n)` that `dput()` writes
/// and C code builds, are stored in the compact integer form `c(NA, -n)` (or `c(NA, n)`).
pub unsafe fn set_row_names(object: Sexp, value: Sexp) -> R<()> {
    let symbol = symbols().row_names;
    let value =
        if value.kind() == SexpType::Real && length(value) == 2 && values::reals(value)[0].is_nan()
        {
            coerce_vector(value, SexpType::Integer)?
        } else {
            value
        };
    if value.kind() == SexpType::Integer {
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

pub(crate) unsafe fn do_set_attributes(call: &Call<'_>) -> R<Sexp> {
    let attributes = call.arg(1);
    // Unlike `attr<-`, giving NULL attributes turns it into an empty list, as in GNU R.
    let object = match call.arg(0) {
        object if object != nil() => modifiable(object)?,
        _ if attributes == nil() => return Ok(nil()),
        _ => alloc(SexpType::List, 0)?,
    };
    // The value is checked before anything is cleared, so a bad one leaves the object as it was.
    let names = values::names(attributes);
    if attributes != nil() {
        if attributes.kind() != SexpType::List {
            return Err(call.error("attributes must be a list or NULL"));
        }
        if length(attributes) > 0 && names == nil() {
            return Err(call.error("attributes must be named"));
        }
        // GNU R checks the names from the second on; an empty first name fails later, when
        // it is installed as a symbol.
        if length(attributes) > 0 && names.element(0).bytes().is_empty() {
            return Err(call.error("attempt to use zero-length variable name"));
        }
        for index in 1..length(attributes) {
            if names.element(index).bytes().is_empty() {
                return Err(call.error(format!(
                    "all attributes must have names [{} does not]",
                    index + 1
                )));
            }
        }
    }
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
    // `dim` goes first, so `dimnames` can be checked against it.
    for pass in 0..2 {
        for index in 0..length(attributes) {
            let name = names.element(index);
            if name != values::na_string() && name.bytes().is_empty() {
                return Err(call.error(format!(
                    "all attributes must have names [{} does not]",
                    index + 1
                )));
            }
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
        && (total % count != 0 && !count.is_multiple_of(rows.max(1))
            || (count > total && !count.is_multiple_of(total)))
    {
        if count > total {
            conditions::warning_call(
                call.call,
                format!("data length differs from size of matrix: [{count} != {rows} x {cols}]"),
            )?;
        } else if (rows > 0 && !count.is_multiple_of(rows))
            || (cols > 0 && !count.is_multiple_of(cols))
        {
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
    } else if by_row || !fill_recycled(result, data) {
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

/// Fills `result` with `data` recycled, when both are the same plain atomic type and `data` is
/// not empty. Returns whether it did.
unsafe fn fill_recycled(result: Sexp, data: Sexp) -> bool {
    fn fill<T: Copy>(target: &mut [T], source: &[T]) {
        if let [single] = source {
            target.fill(*single);
        } else {
            for (slot, &item) in target.iter_mut().zip(source.iter().cycle()) {
                *slot = item;
            }
        }
    }
    if data.kind() != result.kind() || length(data) == 0 {
        return false;
    }
    match data.kind() {
        SexpType::Real => fill(values::reals_mut(result), values::reals(data)),
        SexpType::Integer | SexpType::Logical => fill(values::ints_mut(result), values::ints(data)),
        _ => return false,
    }
    true
}

unsafe fn do_array(call: &Call<'_>) -> R<Sexp> {
    let data = values::materialize(call.arg(0))?;
    let dims = coerce_vector(call.arg(1), SexpType::Integer)?;
    let dimnames = call.arg(2);
    let total: usize = if crate::system::r_at_least_release(4, 6, 2) {
        dims_total(call, dims)? as usize
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
    if count == 0 || !fill_recycled(result, data) {
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
unsafe fn dims_total(call: &Call<'_>, dims: Sexp) -> R<i64> {
    const LONGEST_VECTOR: f64 = 4_503_599_627_370_496.0;
    let extents = values::ints(dims);
    if extents.is_empty() {
        return Err(call.error("'dim' cannot be of length 0"));
    }
    let mut product = 1.0f64;
    for &extent in extents {
        if extent == NA_INTEGER {
            return Err(call.error("the dims contain missing values"));
        }
        if extent < 0 {
            return Err(call.error("the dims contain negative values"));
        }
        product = if extent == 0 {
            0.0
        } else {
            product * f64::from(extent)
        };
    }
    if product > LONGEST_VECTOR {
        return Err(call.error("too many elements specified"));
    }
    Ok(product as i64)
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
                let Some(found) =
                    (0..rank).find(|&candidate| dimnames_names.element(candidate) == wanted)
                else {
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
    let result = values::shallow_duplicate(object)?;
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
        if dimnames != nil() {
            let list: SmallVec<[Sexp; 4]> = kept
                .iter()
                .map(|&dimension| dimnames.element(dimension))
                .collect();
            let list = values::list_vector(&list)?;
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
    // Errors and warnings name an argument by its position in the call, NULLs included.
    let (items, positions): (Vec<Arg>, Vec<usize>) = items
        .into_iter()
        .enumerate()
        .filter(|(_, arg)| arg.value != nil())
        .map(|(position, arg)| (arg, position + 1))
        .unzip();
    if items.is_empty() {
        return Ok(nil());
    }
    // Starting below raw, so that binding only raw vectors gives a raw matrix.
    let mut kind = SexpType::Nil;
    for arg in &items {
        kind = coerce::higher_type(kind, arg.value.kind());
    }
    if kind == SexpType::Nil {
        kind = SexpType::Logical;
    }
    // The extent along the bound dimension.
    // The first matrix fixes it, even at zero; without one, the longest vector does.
    let has_matrix = items
        .iter()
        .any(|arg| values::dims(arg.value).is_some_and(|dims| dims.len() == 2));
    let extent = items
        .iter()
        .find_map(|arg| {
            values::dims(arg.value)
                .filter(|dims| dims.len() == 2)
                .map(|dims| if by_row { dims[1] } else { dims[0] })
        })
        .unwrap_or_else(|| items.iter().map(|arg| length(arg.value)).max().unwrap_or(0));
    let mut pieces: Vec<(Sexp, usize, bool)> = Vec::new();
    let mut total = 0usize;
    let mut warned = false;
    // The warning names the `rbind` or `cbind` closure call, not the internal.
    let frames = &crate::interp().frames;
    let closure_call = call
        .frame_index()
        .map_or(call.call, |position| frames[position].call);
    for (arg, &position) in items.iter().zip(&positions) {
        match values::dims(arg.value).filter(|dims| dims.len() == 2) {
            Some(dims) => {
                if (if by_row { dims[1] } else { dims[0] }) != extent {
                    let direction = if by_row { "columns" } else { "rows" };
                    // Reported against the `rbind` or `cbind` closure call, as GNU R does.
                    return Err(crate::unwind::error(format!(
                        "number of {direction} of matrices must match (see arg {position})"
                    )));
                }
                let count = if by_row { dims[0] } else { dims[1] };
                pieces.push((arg.value, count, true));
                total += count;
            }
            None => {
                let source_length = length(arg.value);
                // A zero-length vector contributes nothing once some argument has fixed a
                // nonzero extent, rather than a row or column of NA.
                if source_length == 0 && extent > 0 {
                    pieces.push((arg.value, 0, false));
                    continue;
                }
                // A vector longer than a matrix's extent is truncated, which GNU R warns about
                // even when the extent is zero.
                let uneven = source_length > 0
                    && (if source_length > extent {
                        has_matrix || !source_length.is_multiple_of(extent)
                    } else {
                        extent % source_length != 0
                    });
                if uneven && !warned {
                    warned = true;
                    let direction = if by_row { "columns" } else { "rows" };
                    crate::conditions::warning_call(
                        closure_call,
                        format!(
                            "number of {direction} of result is not a multiple of vector length (arg {position})"
                        ),
                    )?;
                }
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
    let mut matrix_named = false;
    let mut other_names = nil();
    for (position, &(value, count, is_matrix)) in pieces.iter().enumerate() {
        let source_length = length(value);
        // A matrix names the other dimension even when it adds no line, as a zero-row matrix
        // of column names does in `rbind(template, ...)`.
        if is_matrix && other_names == nil() {
            let dimnames = get_attr(value, symbols().dimnames);
            if dimnames != nil() {
                other_names = dimnames.element(if by_row { 1 } else { 0 });
            }
        }
        let source = values::materialize(value)?;
        let source_rows = if is_matrix {
            values::dims(value)
                .and_then(|dims| dims.first().copied())
                .unwrap_or(0)
        } else {
            0
        };
        for line in 0..count {
            let line_offset = offset + line;
            let target = |along: usize| {
                if by_row {
                    line_offset + along * rows
                } else {
                    along + line_offset * rows
                }
            };
            if source_length == 0 {
                for along in 0..extent {
                    set_na(result, target(along));
                }
            } else if is_matrix && by_row {
                copy_mapped(
                    source,
                    result,
                    extent,
                    |along| line + along * source_rows,
                    target,
                )?;
            } else if is_matrix {
                copy_mapped(
                    source,
                    result,
                    extent,
                    |along| along + line * source_rows,
                    target,
                )?;
            } else {
                copy_mapped(
                    source,
                    result,
                    extent,
                    |along| along % source_length,
                    target,
                )?;
            }
            let arg = &items[position];
            if is_matrix {
                let dimnames = get_attr(value, symbols().dimnames);
                let names = if dimnames != nil() {
                    dimnames.element(if by_row { 0 } else { 1 })
                } else {
                    nil()
                };
                matrix_named |= names != nil();
                bound_names.push(if names != nil() {
                    values::string_at(names, line)
                        .map(|text| text.into_owned())
                        .unwrap_or_default()
                } else {
                    String::new()
                });
            } else {
                // Positions count from 1 over the call's arguments, which `expressions` mirrors.
                let expression = expressions[positions[position] - 1];
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
    // GNU R keeps the bound names whenever a matrix argument had names on that axis, even
    // when every one of them is empty.
    let bound = if matrix_named || bound_names.iter().any(|name| !name.is_empty()) {
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
