//! Sorting, ordering, ranking, matching, duplicate detection, `split` and `identical`.
//!
//! Strings order by code point, which is GNU R's order under `LC_COLLATE=C`. Every ordering
//! here that GNU R documents as stable is stable, and the internals that GNU R implements with
//! an unstable algorithm (`sort` on doubles, `psort`, `qsort`) use the same textbook algorithm,
//! because the arrangement of ties and of `NaN` is observable through them.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    coerce::{coerce_vector, factor_as_character},
    conditions::warning,
    eval,
    subset::select_elements,
    unwind::{R, error},
    values::{
        self, alloc, as_logical_scalar, char_str, get_attr, length, na_string, nil, string_at,
        symbols,
    },
};
use heap::object::{
    NA_INTEGER, NA_LOGICAL, NA_REAL_BITS, Rcomplex, Sexp, SexpType, Slot, gp, is_na_real,
};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use std::{
    borrow::Cow,
    cell::Cell,
    cmp::Ordering,
    hash::{Hash, Hasher},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("merge", 4, do_merge),
    internal("sort", 2, do_sort),
    internal("psort", 2, do_psort),
    internal("qsort", 2, do_qsort),
    internal("order", -1, do_order),
    internal("radixsort", -1, do_radixsort),
    internal("rank", 3, do_rank),
    internal("unique", 4, do_unique),
    internal("duplicated", 4, do_duplicated),
    internal("anyDuplicated", 3, do_any_duplicated),
    internal("match", 4, do_match),
    internal("split", 2, do_split),
    internal("identical", 9, do_identical),
    // No vector carries sortedness metadata, so the fast pass never applies and the caller
    // sorts; the answer is the same either way.
    internal("sorted_fpass", 3, |_| unsafe { values::scalar_bool(false) }),
    internal("wrap_meta", 3, |call| Ok(call.arg(0))),
];

/// Compares two strings as GNU R's `sort` and `order` do. In the `C` and `POSIX` locales that is
/// code point order. Otherwise GNU R collates with ICU's root collation, which this approximates
/// for the scripts R code commonly sorts: spaces, then punctuation and symbols, then digits, then
/// letters compared without case or accents; ties go to accents, then lowercase before uppercase.
pub fn collate(left: &str, right: &str) -> Ordering {
    if c_collation() {
        return left.cmp(right);
    }
    let primary = |text: &str| text.chars().map(primary_weight).collect::<Vec<_>>();
    let secondary = |text: &str| {
        text.chars()
            .map(|character| base_letter(character).1)
            .collect::<Vec<_>>()
    };
    let tertiary = |text: &str| {
        text.chars()
            .map(|character| character.is_uppercase())
            .collect::<Vec<_>>()
    };
    primary(left)
        .cmp(&primary(right))
        .then_with(|| secondary(left).cmp(&secondary(right)))
        .then_with(|| tertiary(left).cmp(&tertiary(right)))
        .then_with(|| left.cmp(right))
}

/// Whether the session collates by code point: `LC_ALL`, then `LC_COLLATE`, then `LANG` is `C` or
/// `POSIX`.
fn c_collation() -> bool {
    static C_COLLATION: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *C_COLLATION.get_or_init(|| {
        let locale = ["LC_ALL", "LC_COLLATE", "LANG"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "C".to_owned());
        locale == "C" || locale == "POSIX"
    })
}

/// The root collation's primary weight of a character, as a (group, value) pair.
fn primary_weight(character: char) -> (u8, u32) {
    // The punctuation and symbols of ASCII, in the order the root collation sorts them.
    const PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
    if character.is_whitespace() {
        return (0, character as u32);
    }
    if let Some(position) = PUNCTUATION.find(character) {
        return (1, position as u32);
    }
    if let Some(digit) = character.to_digit(10) {
        return (3, digit);
    }
    let (base, _) = base_letter(character);
    if base.is_alphabetic() {
        return (4, base.to_lowercase().next().unwrap_or(base) as u32);
    }
    if character.is_ascii() || !character.is_alphanumeric() {
        return (2, character as u32);
    }
    (5, character as u32)
}

/// A Latin letter without its accent, and an accent rank (0 for none).
fn base_letter(character: char) -> (char, u32) {
    const ACCENTED: [(&str, char); 14] = [
        ("àáâãäåāăą", 'a'),
        ("ÀÁÂÃÄÅĀĂĄ", 'A'),
        ("çćĉċč", 'c'),
        ("ÇĆĈĊČ", 'C'),
        ("èéêëēĕėęě", 'e'),
        ("ÈÉÊËĒĔĖĘĚ", 'E'),
        ("ìíîïĩīĭįı", 'i'),
        ("ÌÍÎÏĨĪĬĮİ", 'I'),
        ("ñńņňŉ", 'n'),
        ("ÑŃŅŇ", 'N'),
        ("òóôõöøōŏő", 'o'),
        ("ÒÓÔÕÖØŌŎŐ", 'O'),
        ("ùúûüũūŭůűų", 'u'),
        ("ÙÚÛÜŨŪŬŮŰŲ", 'U'),
    ];
    for (variants, base) in ACCENTED {
        if let Some(position) = variants.chars().position(|variant| variant == character) {
            return (base, position as u32 + 1);
        }
    }
    (character, 0)
}

pub fn sort_strings(names: &mut [String]) {
    names.sort_by(|left, right| collate(left, right));
}

/// The comparison `identical()` makes, with one field per argument of the R function.
#[derive(Clone, Copy, Debug)]
pub struct IdenticalFlags {
    pub num_eq: bool,
    pub single_na: bool,
    pub attributes_as_set: bool,
    pub ignore_bytecode: bool,
    pub ignore_environment: bool,
    pub ignore_srcref: bool,
    pub external_pointer_as_reference: bool,
}

impl Default for IdenticalFlags {
    fn default() -> IdenticalFlags {
        IdenticalFlags {
            num_eq: true,
            single_na: true,
            attributes_as_set: true,
            ignore_bytecode: true,
            ignore_environment: false,
            ignore_srcref: true,
            external_pointer_as_reference: false,
        }
    }
}

/// Whether `x` and `y` are identical in the sense of R's `identical()`.
pub unsafe fn identical(x: Sexp, y: Sexp, flags: &IdenticalFlags) -> bool {
    identical_skipping(x, y, flags, &[])
}

unsafe fn do_sort(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let decreasing = flag(call.arg(1), "'decreasing' must be TRUE or FALSE")?;
    if x == nil() {
        return Ok(nil());
    }
    check_sortable(x)?;
    let result = values::duplicate(x)?;
    values::clear_attributes(result);
    match result.kind() {
        SexpType::Logical | SexpType::Integer => {
            // NA is the smallest integer, which is where GNU R's integer sort leaves it.
            let items = values::ints_mut(result);
            if decreasing {
                items.sort_unstable_by(|left, right| right.cmp(left));
            } else {
                items.sort_unstable();
            }
        }
        SexpType::Real => {
            // Plain comparisons, so a NaN stays where it is, as in GNU R. The R wrappers remove
            // NAs before they get here.
            if decreasing {
                shell_sort(values::reals_mut(result), |left, right| left < right);
            } else {
                shell_sort(values::reals_mut(result), |left, right| left > right);
            }
        }
        SexpType::Complex => {
            if decreasing {
                shell_sort(values::complexes_mut(result), complex_less);
            } else {
                shell_sort(values::complexes_mut(result), |left, right| {
                    complex_less(right, left)
                });
            }
        }
        SexpType::String => {
            let mut items: Vec<Sexp> = (0..length(result))
                .map(|index| result.element(index))
                .collect();
            items.sort_by(|&left, &right| {
                let ordering = compare_strings_na_last(left, right);
                if decreasing {
                    ordering.reverse()
                } else {
                    ordering
                }
            });
            for (index, item) in items.into_iter().enumerate() {
                values::set_elt(result, index, item);
            }
        }
        _ => {}
    }
    Ok(result)
}

/// `psort(x, partial)`: rearranges `x` so that each position in `partial` holds the element a
/// full sort would put there, with smaller elements before it and larger ones after.
unsafe fn do_psort(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    check_sortable(x)?;
    let count = length(x);
    let partial = coerce_vector(call.arg(1), SexpType::Real)?;
    let mut positions = Vec::with_capacity(length(partial));
    for &position in values::reals(partial) {
        if !(position >= 1.0 && position <= count as f64) {
            return Err(error(format!("index {} outside bounds", position as i64)));
        }
        positions.push(position as usize - 1);
    }
    positions.sort_unstable();
    let column = column(x).ok_or_else(|| error("only atomic vectors can be sorted"))?;
    let mut order: Vec<usize> = (0..count).collect();
    let compare = |left: usize, right: usize| column.compare(left, right, true);
    partial_sort(&mut order, 0, count as isize - 1, &positions, &compare);
    select_elements(x, &order)
}

/// `qsort(x, index.return)`: Singleton's quicksort, whose arrangement of ties `index.return`
/// exposes.
unsafe fn do_qsort(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let numeric = matches!(
        x.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && !values::inherits(x, "factor");
    if !numeric {
        return Err(error("argument is not a numeric vector"));
    }
    let index_return = flag(call.arg(1), "'index.return' must be TRUE or FALSE")?;
    let result = values::duplicate(x)?;
    values::clear_attributes(result);
    let mut index: Vec<i32> = (1..=length(result) as i32).collect();
    match result.kind() {
        SexpType::Real => singleton_sort(values::reals_mut(result), &mut index),
        _ => singleton_sort(values::ints_mut(result), &mut index),
    }
    if !index_return {
        return Ok(result);
    }
    let index = values::integer_vector(&index)?;
    values::named_list(&[("x", result), ("ix", index)])
}

/// `order(na.last, decreasing, ...)`: the stable permutation that sorts the keys
/// lexicographically.
unsafe fn do_order(call: &Call<'_>) -> R<Sexp> {
    let na_last = match as_logical_scalar(call.arg(0)) {
        NA_LOGICAL => return Err(error("invalid 'na.last' value")),
        value => value != 0,
    };
    let decreasing = flag(call.arg(1), "'decreasing' must be TRUE or FALSE")?;
    let keys: Vec<Sexp> = call.args.iter().skip(2).map(|arg| arg.value).collect();
    let Some(&first) = keys.first() else {
        return Ok(nil());
    };
    let count = length(first);
    if keys.iter().any(|&key| length(key) != count) {
        return Err(error("argument lengths differ"));
    }
    let keys = keys
        .iter()
        .map(|&key| values::materialize(key))
        .collect::<R<Vec<Sexp>>>()?;
    let columns: Vec<Option<Column<'_>>> = keys.iter().map(|&key| column(key)).collect();
    let routine = if keys.len() == 1 {
        "orderVector1"
    } else {
        "listgreater"
    };
    if keys.len() == 1 && columns[0].is_none() {
        return Err(unimplemented_type(first, routine));
    }
    let directions = vec![decreasing; columns.len()];
    let unsupported = Cell::new(None);
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&left, &right| {
        compare_rows(&columns, &directions, left, right, na_last, &unsupported)
    });
    if let Some(position) = unsupported.get() {
        return Err(unimplemented_type(keys[position], routine));
    }
    one_based(&order)
}

/// `radixsort(na.last, decreasing, retgrp, sortstr, ...)`, which `order(method = "radix")` and
/// `sort(method = "radix")` call. `na.last = NA` drops the rows with an NA in any key, and
/// `retgrp` adds the group boundaries of equal rows.
unsafe fn do_radixsort(call: &Call<'_>) -> R<Sexp> {
    let na_last = as_logical_scalar(call.arg(0));
    let return_groups = as_logical_scalar(call.arg(2)) == 1;
    let sort_strings = as_logical_scalar(call.arg(3)) != 0;
    let keys: Vec<Sexp> = call.args.iter().skip(4).map(|arg| arg.value).collect();
    if keys.is_empty() {
        return Ok(nil());
    }
    let mut materialized = Vec::with_capacity(keys.len());
    for (position, &key) in keys.iter().enumerate() {
        if !key.kind().is_vector() {
            return Err(error(format!("argument {} is not a vector", position + 1)));
        }
        materialized.push(values::materialize(key)?);
    }
    let count = length(keys[0]);
    if keys.iter().any(|&key| length(key) != count) {
        return Err(error("argument lengths differ"));
    }
    let decreasing = coerce_vector(call.arg(1), SexpType::Logical)?;
    if length(decreasing) != keys.len() {
        return Err(error(
            "length(decreasing) must match the number of order arguments",
        ));
    }
    let directions = values::ints(decreasing)
        .iter()
        .map(|&value| match value {
            NA_LOGICAL => Err(error("'decreasing' elements must be TRUE or FALSE")),
            value => Ok(value != 0),
        })
        .collect::<R<Vec<bool>>>()?;
    let columns: Vec<Option<Column<'_>>> = materialized
        .iter()
        .map(|&key| radix_column(key, sort_strings))
        .collect();
    if columns[0].is_none() {
        return Err(error(format!(
            "First arg is type '{}', not yet supported",
            keys[0].kind().name()
        )));
    }
    let unsupported = Cell::new(None);
    let mut order: Vec<usize> = (0..count).collect();
    let nas_last = na_last == 1;
    order.sort_by(|&left, &right| {
        compare_rows(&columns, &directions, left, right, nas_last, &unsupported)
    });
    if let Some(position) = unsupported.get() {
        return Err(error(format!(
            "Arg {} is type '{}', not yet supported",
            position + 1,
            keys[position].kind().name()
        )));
    }
    let ends = if return_groups {
        let mut ends = Vec::new();
        for position in 1..=order.len() {
            let boundary = position == order.len()
                || compare_rows(
                    &columns,
                    &directions,
                    order[position - 1],
                    order[position],
                    nas_last,
                    &unsupported,
                ) != Ordering::Equal;
            if boundary {
                ends.push(position as i32);
            }
        }
        Some(ends)
    } else {
        None
    };
    let mut result: Vec<i32> = order.iter().map(|&index| index as i32 + 1).collect();
    if na_last == NA_LOGICAL {
        for (slot, &index) in result.iter_mut().zip(&order) {
            if columns.iter().flatten().any(|column| column.is_na(index)) {
                *slot = 0;
            }
        }
        if ends.is_none() {
            result.retain(|&index| index != 0);
        }
    }
    let permutation = values::integer_vector(&result)?;
    if let Some(ends) = ends {
        let largest = ends
            .iter()
            .scan(0, |previous, &end| {
                let size = end - *previous;
                *previous = end;
                Some(size)
            })
            .max()
            .unwrap_or(NA_INTEGER);
        let ends_vector = values::integer_vector(&ends)?;
        values::set_attr(permutation, values::install("ends")?, ends_vector)?;
        let largest = values::scalar_integer(largest)?;
        values::set_attr(permutation, values::install("maxgrpn")?, largest)?;
        let class = values::string_vector(&["grouping", "integer"])?;
        values::set_attr(permutation, symbols().class, class)?;
    }
    Ok(permutation)
}

/// `rank(x, length, ties)` for the tie methods GNU R computes in C: `"average"`, `"min"` and
/// `"max"`.
unsafe fn do_rank(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    if x.kind() == SexpType::Raw {
        return Err(error("raw vectors cannot be sorted"));
    }
    let column = column(x).ok_or_else(|| unimplemented_type(x, "greater"))?;
    let requested = values::as_real_scalar(call.arg(1));
    if requested.is_nan() || requested < 0.0 {
        return Err(error("invalid 'length(xx)' value"));
    }
    let count = (requested as usize).min(length(x));
    let ties = match values::first_string(call.arg(2)).as_deref() {
        Some("average") => Ties::Average,
        Some("min") => Ties::Min,
        Some("max") => Ties::Max,
        _ => {
            return Err(error(
                "invalid ties.method for rank() [should never happen]",
            ));
        }
    };
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&left, &right| column.compare(left, right, true));
    let mut ranks = vec![0.0; count];
    let mut start = 0;
    while start < count {
        let mut end = start;
        while end + 1 < count
            && column.compare(order[start], order[end + 1], true) == Ordering::Equal
        {
            end += 1;
        }
        let rank = match ties {
            Ties::Average => (start + end + 2) as f64 / 2.0,
            Ties::Min => (start + 1) as f64,
            Ties::Max => (end + 1) as f64,
        };
        for &index in &order[start..=end] {
            ranks[index] = rank;
        }
        start = end + 1;
    }
    if ties == Ties::Average {
        values::real_vector(&ranks)
    } else {
        let ranks: Vec<i32> = ranks.iter().map(|&rank| rank as i32).collect();
        values::integer_vector(&ranks)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ties {
    Average,
    Min,
    Max,
}

unsafe fn do_unique(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x == nil() {
        return Ok(nil());
    }
    let from_last = flag(call.arg(2), "'fromLast' must be TRUE or FALSE")?;
    let duplicated = duplicated_elements(x, call.arg(1), from_last, "unique")?;
    let kept: Vec<usize> = (0..duplicated.len())
        .filter(|&index| !duplicated[index])
        .collect();
    select_elements(x, &kept)
}

unsafe fn do_duplicated(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let from_last = flag(call.arg(2), "'fromLast' must be TRUE or FALSE")?;
    if x == nil() {
        return alloc(SexpType::Logical, 0);
    }
    let duplicated = duplicated_elements(x, call.arg(1), from_last, "duplicated")?;
    let flags: Vec<i32> = duplicated
        .iter()
        .map(|&duplicate| duplicate as i32)
        .collect();
    values::logical_vector(&flags)
}

unsafe fn do_any_duplicated(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let from_last = flag(call.arg(2), "'fromLast' must be TRUE or FALSE")?;
    if x == nil() {
        return values::scalar_integer(0);
    }
    let duplicated = duplicated_elements(x, call.arg(1), from_last, "anyDuplicated")?;
    let found = if from_last {
        duplicated.iter().rposition(|&duplicate| duplicate)
    } else {
        duplicated.iter().position(|&duplicate| duplicate)
    };
    values::scalar_integer(found.map_or(0, |index| index as i32 + 1))
}

/// `match(x, table, nomatch, incomparables)`. Both sides are converted to a common type first:
/// character when either is character or of a later type (lists and raw vectors included),
/// otherwise the higher of the two numeric types. Factors match by their labels, and other
/// classed objects by what `mtfrm()` makes of them.
unsafe fn do_match(call: &Call<'_>) -> R<Sexp> {
    let x = match_operand(call.arg(0), call.env)?;
    let table = match_operand(call.arg(1), call.env)?;
    let nomatch = coerce_vector(call.arg(2), SexpType::Integer)?;
    let nomatch = values::ints(nomatch).first().copied().unwrap_or(NA_INTEGER);
    let count = length(x);
    if count == 0 {
        return alloc(SexpType::Integer, 0);
    }
    if length(table) == 0 {
        return values::integer_vector(&vec![nomatch; count]);
    }
    let string_code = SexpType::String.code();
    let kind = if x.kind().code() >= string_code || table.kind().code() >= string_code {
        SexpType::String
    } else if x.kind().code() >= table.kind().code() {
        x.kind()
    } else {
        table.kind()
    };
    let x = values::materialize(coerce_vector(x, kind)?)?;
    let table = values::materialize(coerce_vector(table, kind)?)?;
    let incomparables = incomparable_keys(call.arg(3), kind)?;
    if kind == SexpType::String && incomparables.is_none() && all_ascii(table) && all_ascii(x) {
        // Interning makes equal ASCII strings one object, `NA` included, so the pointer is the
        // key and no text is hashed. GNU R's `match` relies on the same property.
        let mut positions: FxHashMap<Sexp, i32> = FxHashMap::default();
        positions.reserve(length(table));
        for index in 0..length(table) {
            positions
                .entry(table.element(index))
                .or_insert(index as i32 + 1);
        }
        let result: Vec<i32> = (0..count)
            .map(|index| positions.get(&x.element(index)).copied().unwrap_or(nomatch))
            .collect();
        return values::integer_vector(&result);
    }
    let mut positions: FxHashMap<Key<'_>, i32> = FxHashMap::default();
    for (index, key) in element_keys(table).into_iter().enumerate() {
        positions.entry(key).or_insert(index as i32 + 1);
    }
    let result: Vec<i32> = element_keys(x)
        .into_iter()
        .map(|key| {
            if incomparables
                .as_ref()
                .is_some_and(|keys| keys.contains(&key))
            {
                return nomatch;
            }
            positions.get(&key).copied().unwrap_or(nomatch)
        })
        .collect();
    values::integer_vector(&result)
}

/// `split(x, f)` for a factor `f`: one piece per level, `f` recycled along `x`, and NA codes
/// dropped. Each piece keeps the names of its elements.
unsafe fn do_split(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    let factor = call.arg(1);
    if !x.kind().is_vector() {
        return Err(error("first argument must be a vector"));
    }
    if !values::inherits(factor, "factor") {
        return Err(error("second argument must be a factor"));
    }
    let levels = get_attr(factor, symbols().levels);
    let level_count = length(levels);
    let codes = values::materialize(coerce_vector(factor, SexpType::Integer)?)?;
    let codes = values::ints(codes);
    let observations = length(x);
    if codes.is_empty() && observations > 0 {
        return Err(error("group length is 0 but data length > 0"));
    }
    if !codes.is_empty() && observations % codes.len() != 0 {
        warning("data length is not a multiple of split variable")?;
    }
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); level_count];
    for index in 0..observations {
        let code = codes[index % codes.len()];
        if code == NA_INTEGER {
            continue;
        }
        if code < 1 || code as usize > level_count {
            return Err(error("factor has bad level"));
        }
        members[code as usize - 1].push(index);
    }
    let names = values::names(x);
    let result = alloc(SexpType::List, level_count)?;
    for (level, indices) in members.iter().enumerate() {
        let piece = alloc(x.kind(), indices.len())?;
        for (position, &index) in indices.iter().enumerate() {
            crate::subset::copy_element(x, index, piece, position)?;
        }
        if names != nil() {
            let piece_names = select_elements(names, indices)?;
            values::set_attr(piece, symbols().names, piece_names)?;
        }
        values::set_elt(result, level, piece);
    }
    if levels.kind() == SexpType::String {
        values::set_attr(result, symbols().names, levels)?;
    }
    Ok(result)
}

unsafe fn do_identical(call: &Call<'_>) -> R<Sexp> {
    let mut settings = [false; 7];
    let names = [
        "num.eq",
        "single.NA",
        "attrib.as.set",
        "ignore.bytecode",
        "ignore.environment",
        "ignore.srcref",
        "extptr.as.ref",
    ];
    for (position, name) in names.iter().enumerate() {
        settings[position] = flag(call.arg(position + 2), &format!("invalid '{name}' value"))?;
    }
    let flags = IdenticalFlags {
        num_eq: settings[0],
        single_na: settings[1],
        attributes_as_set: settings[2],
        ignore_bytecode: settings[3],
        ignore_environment: settings[4],
        ignore_srcref: settings[5],
        external_pointer_as_reference: settings[6],
    };
    values::scalar_bool(identical(call.arg(0), call.arg(1), &flags))
}

/// A single `TRUE` or `FALSE`, as GNU R's `asLogical` reads it; anything else is `message`.
unsafe fn flag(value: Sexp, message: &str) -> R<bool> {
    match as_logical_scalar(value) {
        NA_LOGICAL => Err(error(message)),
        value => Ok(value != 0),
    }
}

unsafe fn check_sortable(x: Sexp) -> R<()> {
    if !x.kind().is_atomic() {
        return Err(error("only atomic vectors can be sorted"));
    }
    if x.kind() == SexpType::Raw {
        return Err(error("raw vectors cannot be sorted"));
    }
    Ok(())
}

unsafe fn unimplemented_type(object: Sexp, routine: &str) -> crate::unwind::Unwind {
    error(format!(
        "unimplemented type '{}' in '{routine}'\n",
        object.kind().name()
    ))
}

unsafe fn one_based(order: &[usize]) -> R<Sexp> {
    let order: Vec<i32> = order.iter().map(|&index| index as i32 + 1).collect();
    values::integer_vector(&order)
}

/// One sort key's elements, read once so that comparisons do not go back to the heap. Nothing
/// allocates while a column is alive, so the borrowed slices stay valid.
enum Column<'a> {
    /// Logical and integer vectors, and factor codes.
    Integer(&'a [i32]),
    Real(&'a [f64]),
    Complex(&'a [Rcomplex]),
    String(Vec<Option<Cow<'a, str>>>),
    /// Strings the radix sort groups without sorting (`sortstr = FALSE`): each element is the
    /// rank of its first appearance, NA included, and the groups keep that order whatever
    /// `decreasing` and `na.last` say.
    Grouped(Vec<i32>),
}

unsafe fn column<'a>(vector: Sexp) -> Option<Column<'a>> {
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => Some(Column::Integer(values::ints(vector))),
        SexpType::Real => Some(Column::Real(values::reals(vector))),
        SexpType::Complex => Some(Column::Complex(values::complexes(vector))),
        SexpType::String => Some(Column::String(
            (0..length(vector))
                .map(|index| string_at(vector, index))
                .collect(),
        )),
        _ => None,
    }
}

/// A key as the radix sort reads it: no complex keys, and with `sortstr = FALSE` strings in
/// order of first appearance instead of collation order.
unsafe fn radix_column<'a>(vector: Sexp, sort_strings: bool) -> Option<Column<'a>> {
    match vector.kind() {
        SexpType::Complex => None,
        SexpType::String if !sort_strings => {
            let mut ranks: FxHashMap<Option<Cow<'_, str>>, i32> = FxHashMap::default();
            let codes = (0..length(vector))
                .map(|index| {
                    let next = ranks.len() as i32;
                    *ranks.entry(string_at(vector, index)).or_insert(next)
                })
                .collect();
            Some(Column::Grouped(codes))
        }
        _ => column(vector),
    }
}

impl Column<'_> {
    fn is_na(&self, index: usize) -> bool {
        match self {
            Column::Integer(items) => items[index] == NA_INTEGER,
            Column::Real(items) => items[index].is_nan(),
            Column::Complex(items) => items[index].r.is_nan() || items[index].i.is_nan(),
            Column::String(items) => items[index].is_none(),
            Column::Grouped(_) => false,
        }
    }

    /// Compares two elements, neither of them NA.
    fn compare_values(&self, left: usize, right: usize) -> Ordering {
        match self {
            Column::Integer(items) => items[left].cmp(&items[right]),
            Column::Grouped(items) => items[left].cmp(&items[right]),
            Column::Real(items) => items[left]
                .partial_cmp(&items[right])
                .unwrap_or(Ordering::Equal),
            Column::Complex(items) => {
                let (left, right) = (items[left], items[right]);
                left.r
                    .partial_cmp(&right.r)
                    .unwrap_or(Ordering::Equal)
                    .then(left.i.partial_cmp(&right.i).unwrap_or(Ordering::Equal))
            }
            Column::String(items) => match (&items[left], &items[right]) {
                (Some(left), Some(right)) => collate(left, right),
                _ => Ordering::Equal,
            },
        }
    }

    /// Compares two elements in increasing order, with NAs equal to each other and placed
    /// after (or before) every other value.
    fn compare(&self, left: usize, right: usize, na_last: bool) -> Ordering {
        self.compare_directed(left, right, na_last, false)
    }

    /// As `compare`, with the non-NA values in decreasing order when `decreasing` is set. NA
    /// placement does not depend on the direction.
    fn compare_directed(
        &self,
        left: usize,
        right: usize,
        na_last: bool,
        decreasing: bool,
    ) -> Ordering {
        match (self.is_na(left), self.is_na(right)) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if na_last {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                if na_last {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => {
                let ordering = self.compare_values(left, right);
                let decreasing = decreasing && !matches!(self, Column::Grouped(_));
                if decreasing {
                    ordering.reverse()
                } else {
                    ordering
                }
            }
        }
    }
}

/// Compares two rows key by key. A key of a type that cannot be ordered is recorded in
/// `unsupported` the first time a comparison needs it, which is exactly when GNU R reports it:
/// only rows that tie on every earlier key reach it.
fn compare_rows(
    columns: &[Option<Column<'_>>],
    directions: &[bool],
    left: usize,
    right: usize,
    na_last: bool,
    unsupported: &Cell<Option<usize>>,
) -> Ordering {
    for (position, column) in columns.iter().enumerate() {
        let Some(column) = column else {
            if unsupported.get().is_none() {
                unsupported.set(Some(position));
            }
            return Ordering::Equal;
        };
        let decreasing = directions.get(position).copied().unwrap_or(false);
        let ordering = column.compare_directed(left, right, na_last, decreasing);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

/// Increasing order for character elements, NA last.
unsafe fn compare_strings_na_last(left: Sexp, right: Sexp) -> Ordering {
    let na = na_string();
    match (left == na, right == na) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => collate(&char_str(left), &char_str(right)),
    }
}

/// Complex order: by real part, then imaginary part. Any comparison with NaN is false.
fn complex_less(left: Rcomplex, right: Rcomplex) -> bool {
    left.r < right.r || (left.r == right.r && left.i < right.i)
}

/// Shell sort with Sedgewick's gaps 4^k + 3 * 2^(k - 1) + 1 (1, 8, 23, 77, 281, ...), moving an
/// element while its predecessor is `greater`.
fn shell_sort<T: Copy>(items: &mut [T], greater: impl Fn(T, T) -> bool) {
    let count = items.len();
    let mut gaps = vec![1usize];
    for exponent in 1..16u32 {
        let gap = 4usize.pow(exponent) + 3 * 2usize.pow(exponent - 1) + 1;
        if gap > count {
            break;
        }
        gaps.push(gap);
    }
    for &gap in gaps.iter().rev() {
        for index in gap..count {
            let value = items[index];
            let mut slot = index;
            while slot >= gap && greater(items[slot - gap], value) {
                items[slot] = items[slot - gap];
                slot -= gap;
            }
            items[slot] = value;
        }
    }
}

/// Places the element of rank `target` (a position in `order`) correctly within
/// `order[low..=high]`, with nothing greater before it and nothing smaller after it. This is
/// Hoare's selection as Wirth presents it.
fn select(
    order: &mut [usize],
    low: isize,
    high: isize,
    target: usize,
    compare: &impl Fn(usize, usize) -> Ordering,
) {
    let target = target as isize;
    let (mut left, mut right) = (low, high);
    while left < right {
        let pivot = order[target as usize];
        let (mut forward, mut backward) = (left, right);
        while forward <= backward {
            while forward < right && compare(order[forward as usize], pivot) == Ordering::Less {
                forward += 1;
            }
            while backward > left && compare(pivot, order[backward as usize]) == Ordering::Less {
                backward -= 1;
            }
            if forward <= backward {
                order.swap(forward as usize, backward as usize);
                forward += 1;
                backward -= 1;
            }
        }
        if backward < target {
            left = forward;
        }
        if target < forward {
            right = backward;
        }
    }
}

/// Selects every position in `positions` (sorted) within `order[low..=high]`: the position
/// nearest the middle first, then the ones on each side of it within their halves.
fn partial_sort(
    order: &mut [usize],
    low: isize,
    high: isize,
    positions: &[usize],
    compare: &impl Fn(usize, usize) -> Ordering,
) {
    if positions.is_empty() || high - low < 1 {
        return;
    }
    if let [only] = positions {
        select(order, low, high, *only, compare);
        return;
    }
    let middle = (low + high) / 2;
    let chosen = positions
        .iter()
        .rposition(|&position| position as isize <= middle)
        .unwrap_or(0);
    let pivot = positions[chosen];
    select(order, low, high, pivot, compare);
    partial_sort(
        order,
        low,
        pivot as isize - 1,
        &positions[..chosen],
        compare,
    );
    partial_sort(
        order,
        pivot as isize + 1,
        high,
        &positions[chosen + 1..],
        compare,
    );
}

/// Singleton's quicksort (CACM Algorithm 347, with a varying pivot position and insertion sort
/// for short segments), carrying `index` along. Positions are 1-based, as in the published
/// algorithm.
fn singleton_sort<T: Copy + PartialOrd>(items: &mut [T], index: &mut [i32]) {
    enum Step {
        NewSegment,
        Partition,
        Dispatch,
        Insert,
        Pop,
    }
    let count = items.len() as isize;
    if count < 2 {
        return;
    }
    let get = |items: &[T], position: isize| items[(position - 1) as usize];
    let swap = |items: &mut [T], index: &mut [i32], left: isize, right: isize| {
        items.swap((left - 1) as usize, (right - 1) as usize);
        index.swap((left - 1) as usize, (right - 1) as usize);
    };
    let mut lower = [0isize; 40];
    let mut upper = [0isize; 40];
    let mut ratio = 0.375;
    let mut depth = 1usize;
    let (mut start, mut end) = (1isize, count);
    let mut step = Step::NewSegment;
    loop {
        match step {
            Step::NewSegment => {
                if start == end {
                    step = Step::Pop;
                    continue;
                }
                ratio = if ratio < 0.5898437 {
                    ratio + 0.0390625
                } else {
                    ratio - 0.5859375
                };
                step = Step::Partition;
            }
            Step::Partition => {
                let mut forward = start;
                let middle = start + ((end - start) as f64 * ratio) as isize;
                if get(items, start) > get(items, middle) {
                    swap(items, index, start, middle);
                }
                let mut backward = end;
                if get(items, end) < get(items, middle) {
                    swap(items, index, end, middle);
                    if get(items, start) > get(items, middle) {
                        swap(items, index, start, middle);
                    }
                }
                let pivot = get(items, middle);
                loop {
                    backward -= 1;
                    while backward > start && get(items, backward) > pivot {
                        backward -= 1;
                    }
                    forward += 1;
                    while forward < end && get(items, forward) < pivot {
                        forward += 1;
                    }
                    if forward > backward {
                        break;
                    }
                    swap(items, index, backward, forward);
                }
                if depth >= lower.len() {
                    return;
                }
                if backward - start <= end - forward {
                    lower[depth] = forward;
                    upper[depth] = end;
                    end = backward;
                } else {
                    lower[depth] = start;
                    upper[depth] = backward;
                    start = forward;
                }
                depth += 1;
                step = Step::Dispatch;
            }
            Step::Dispatch => {
                step = if end - start > 10 {
                    Step::Partition
                } else if start == 1 {
                    Step::NewSegment
                } else {
                    start -= 1;
                    Step::Insert
                };
            }
            Step::Insert => {
                start += 1;
                if start >= end {
                    step = Step::Pop;
                    continue;
                }
                let value = get(items, start + 1);
                let value_index = index[start as usize];
                if get(items, start) > value {
                    let mut slot = start;
                    loop {
                        items[slot as usize] = items[(slot - 1) as usize];
                        index[slot as usize] = index[(slot - 1) as usize];
                        slot -= 1;
                        if slot < 1 || !(value < get(items, slot)) {
                            break;
                        }
                    }
                    items[slot as usize] = value;
                    index[slot as usize] = value_index;
                }
            }
            Step::Pop => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
                start = lower[depth];
                end = upper[depth];
                step = Step::Dispatch;
            }
        }
    }
}

/// A hashable stand-in for one atomic element, equal exactly when GNU R's `unique` and `match`
/// consider two elements equal: NA equals NA, NaN equals NaN, NA differs from NaN, and `-0`
/// equals `0`.
#[derive(PartialEq, Eq, Hash)]
enum Key<'a> {
    Integer(i32),
    Real(u64),
    Complex(u64, u64),
    String(Option<Cow<'a, str>>),
    Raw(u8),
}

unsafe fn all_ascii(strings: Sexp) -> bool {
    (0..length(strings)).all(|index| strings.element(index).has_gp(gp::ASCII))
}

/// The keys of an atomic vector's elements. Other types give no keys.
unsafe fn element_keys<'a>(vector: Sexp) -> Vec<Key<'a>> {
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(vector)
            .iter()
            .map(|&value| Key::Integer(value))
            .collect(),
        SexpType::Real => values::reals(vector)
            .iter()
            .map(|&value| Key::Real(real_key(value)))
            .collect(),
        SexpType::Complex => values::complexes(vector)
            .iter()
            .map(|value| {
                if is_na_real(value.r) || is_na_real(value.i) {
                    Key::Complex(NA_REAL_BITS, NA_REAL_BITS)
                } else {
                    Key::Complex(real_key(value.r), real_key(value.i))
                }
            })
            .collect(),
        SexpType::String => (0..length(vector))
            .map(|index| Key::String(string_at(vector, index)))
            .collect(),
        SexpType::Raw => values::raws(vector)
            .iter()
            .map(|&value| Key::Raw(value))
            .collect(),
        _ => Vec::new(),
    }
}

/// The bits of a double with every NA, every other NaN, and both zeros each made one value.
fn real_key(value: f64) -> u64 {
    if is_na_real(value) {
        NA_REAL_BITS
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

/// The `incomparables` argument as keys of `kind`, or `None` for `NULL` and `FALSE`.
unsafe fn incomparable_keys<'a>(incomparables: Sexp, kind: SexpType) -> R<Option<Vec<Key<'a>>>> {
    if incomparables == nil()
        || (incomparables.kind() == SexpType::Logical
            && length(incomparables) == 1
            && values::ints(incomparables)[0] == 0)
    {
        return Ok(None);
    }
    let converted = values::materialize(coerce_vector(incomparables, kind)?)?;
    Ok(Some(element_keys(converted)))
}

/// Which elements of `x` repeat an earlier one (a later one with `from_last`). Lists compare
/// their elements with `identical()`.
unsafe fn duplicated_elements(
    x: Sexp,
    incomparables: Sexp,
    from_last: bool,
    what: &str,
) -> R<Vec<bool>> {
    if !x.kind().is_vector() {
        return Err(error(format!("{what}() applies only to vectors")));
    }
    let x = values::materialize(x)?;
    let count = length(x);
    let mut duplicated = vec![false; count];
    let visit: Box<dyn Iterator<Item = usize>> = if from_last {
        Box::new((0..count).rev())
    } else {
        Box::new(0..count)
    };
    if matches!(x.kind(), SexpType::List | SexpType::Expression) {
        let flags = IdenticalFlags::default();
        let mut buckets: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
        for index in visit {
            let element = x.element(index);
            let bucket = buckets.entry(structural_hash(element)).or_default();
            if bucket
                .iter()
                .any(|&seen| identical(x.element(seen), element, &flags))
            {
                duplicated[index] = true;
            } else {
                bucket.push(index);
            }
        }
        return Ok(duplicated);
    }
    let incomparables = incomparable_keys(incomparables, x.kind())?;
    let keys = element_keys(x);
    let mut seen: FxHashSet<&Key<'_>> = FxHashSet::default();
    for index in visit {
        let key = &keys[index];
        if !seen.insert(key) {
            duplicated[index] = !incomparables
                .as_ref()
                .is_some_and(|keys| keys.contains(key));
        }
    }
    Ok(duplicated)
}

/// A hash consistent with `identical()` under its default flags: identical objects hash alike.
/// Attributes and the insides of functions and environments are left out, which only costs
/// collisions.
unsafe fn structural_hash(object: Sexp) -> u64 {
    let mut hasher = FxHasher::default();
    hash_object(object, &mut hasher);
    hasher.finish()
}

unsafe fn hash_object(object: Sexp, hasher: &mut FxHasher) {
    object.kind().code().hash(hasher);
    match object.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(object).hash(hasher),
        SexpType::Real => {
            for &value in values::reals(object) {
                real_key(value).hash(hasher);
            }
        }
        SexpType::Complex => {
            for value in values::complexes(object) {
                real_key(value.r).hash(hasher);
                real_key(value.i).hash(hasher);
            }
        }
        SexpType::String => {
            for index in 0..length(object) {
                string_at(object, index).hash(hasher);
            }
        }
        SexpType::Raw => values::raws(object).hash(hasher),
        SexpType::List | SexpType::Expression => {
            for index in 0..length(object) {
                hash_object(object.element(index), hasher);
            }
        }
        SexpType::Pairlist | SexpType::Language => {
            let mut cell = object;
            while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
                hash_object(cell.car(), hasher);
                cell = cell.cdr();
            }
        }
        SexpType::Symbol => object.hash(hasher),
        _ => {}
    }
}

/// `x` or `table` of `match`, with factors as their labels and other classed objects as
/// `mtfrm()` makes them.
unsafe fn match_operand(object: Sexp, env: Sexp) -> R<Sexp> {
    if object != nil() && !object.kind().is_vector() {
        return Err(error("'match' requires vector arguments"));
    }
    if !object.is_object() {
        return Ok(object);
    }
    if values::inherits(object, "factor") {
        return factor_as_character(object);
    }
    let transform = values::call(values::install("mtfrm")?, &[object])?;
    eval::eval(transform, env)
}

unsafe fn identical_skipping(x: Sexp, y: Sexp, flags: &IdenticalFlags, skipped: &[Sexp]) -> bool {
    if x == y {
        return true;
    }
    let kind = x.kind();
    if kind != y.kind()
        || x.is_object() != y.is_object()
        || x.has_gp(gp::S4_OBJECT) != y.has_gp(gp::S4_OBJECT)
    {
        return false;
    }
    let srcref = [symbols().srcref];
    let skipped = if kind == SexpType::Closure && flags.ignore_srcref {
        &srcref[..]
    } else {
        skipped
    };
    if !attributes_identical(x, y, flags, skipped) {
        return false;
    }
    match kind {
        SexpType::Nil => true,
        SexpType::Logical | SexpType::Integer => values::ints(x) == values::ints(y),
        SexpType::Real => {
            let (left, right) = (values::reals(x), values::reals(y));
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(&a, &b)| reals_identical(a, b, flags))
        }
        SexpType::Complex => {
            let (left, right) = (values::complexes(x), values::complexes(y));
            left.len() == right.len()
                && left.iter().zip(right).all(|(a, b)| {
                    reals_identical(a.r, b.r, flags) && reals_identical(a.i, b.i, flags)
                })
        }
        SexpType::String => {
            length(x) == length(y)
                && (0..length(x)).all(|index| chars_equal(x.element(index), y.element(index)))
        }
        SexpType::Raw => values::raws(x) == values::raws(y),
        SexpType::List | SexpType::Expression => {
            length(x) == length(y)
                && (0..length(x)).all(|index| identical(x.element(index), y.element(index), flags))
        }
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            let (mut left, mut right) = (x, y);
            loop {
                let left_cell = matches!(
                    left.kind(),
                    SexpType::Pairlist | SexpType::Language | SexpType::Dots
                );
                let right_cell = matches!(
                    right.kind(),
                    SexpType::Pairlist | SexpType::Language | SexpType::Dots
                );
                if !left_cell || !right_cell {
                    return left_cell == right_cell;
                }
                if left.tag() != right.tag() || !identical(left.car(), right.car(), flags) {
                    return false;
                }
                left = left.cdr();
                right = right.cdr();
            }
        }
        SexpType::Closure => {
            let source = [symbols().srcref, symbols().srcfile, symbols().whole_srcref];
            let body_skipped: &[Sexp] = if flags.ignore_srcref { &source } else { &[] };
            identical(x.car(), y.car(), flags)
                && identical_skipping(
                    crate::bytecode::closure_body(x),
                    crate::bytecode::closure_body(y),
                    flags,
                    body_skipped,
                )
                && (flags.ignore_environment || x.tag() == y.tag())
        }
        SexpType::Builtin | SexpType::Special => x.primitive_index() == y.primitive_index(),
        SexpType::ExternalPtr => {
            !flags.external_pointer_as_reference && x.external_address() == y.external_address()
        }
        SexpType::Char => chars_equal(x, y),
        SexpType::Object => true,
        SexpType::Record => {
            x.record_layout() == y.record_layout()
                && (0..x.record_slot_count()).all(|index| {
                    match (x.record_slot(index), y.record_slot(index)) {
                        (Slot::Handle(left), Slot::Handle(right)) => identical(left, right, flags),
                        (Slot::Real(left), Slot::Real(right)) => {
                            reals_identical(left, right, flags)
                        }
                        (Slot::Integer(left), Slot::Integer(right))
                        | (Slot::Logical(left), Slot::Logical(right)) => left == right,
                        // A scalar is boxed only when it could not be stored raw (an ALTREP value, or
                        // a double whose bits collide with the raw encoding), so compare contents.
                        (Slot::Handle(handle), raw) | (raw, Slot::Handle(handle)) => {
                            raw_slot_identical(raw, handle, flags)
                        }
                        _ => false,
                    }
                })
        }
        _ => false,
    }
}

unsafe fn raw_slot_identical(raw: Slot, handle: Sexp, flags: &IdenticalFlags) -> bool {
    if handle.attrib() != nil() || length(handle) != 1 {
        return false;
    }
    let Ok(handle) = values::materialize(handle) else {
        return false;
    };
    match (raw, handle.kind()) {
        (Slot::Real(value), SexpType::Real) => {
            reals_identical(value, values::reals(handle)[0], flags)
        }
        (Slot::Integer(value), SexpType::Integer) | (Slot::Logical(value), SexpType::Logical) => {
            values::ints(handle)[0] == value
        }
        _ => false,
    }
}

/// Compares attribute lists, as a set by default or in order, leaving out the tags in
/// `skipped`.
unsafe fn attributes_identical(x: Sexp, y: Sexp, flags: &IdenticalFlags, skipped: &[Sexp]) -> bool {
    if x.attrib() == nil() && y.attrib() == nil() {
        return true;
    }
    let items = |object: Sexp| -> Vec<(Sexp, Sexp)> {
        values::pairlist_items(object.attrib())
            .into_iter()
            .filter(|(tag, _)| !skipped.contains(tag))
            .collect()
    };
    let (left, right) = (items(x), items(y));
    if left.len() != right.len() {
        return false;
    }
    if flags.attributes_as_set {
        left.iter().all(|&(tag, value)| {
            right
                .iter()
                .any(|&(other_tag, other)| other_tag == tag && identical(value, other, flags))
        })
    } else {
        left.iter()
            .zip(&right)
            .all(|(&(tag, value), &(other_tag, other))| {
                tag == other_tag && identical(value, other, flags)
            })
    }
}

/// Two doubles under `identical()`'s `num.eq` and `single.NA`.
fn reals_identical(left: f64, right: f64, flags: &IdenticalFlags) -> bool {
    if flags.single_na {
        if is_na_real(left) || is_na_real(right) {
            return is_na_real(left) && is_na_real(right);
        }
        if left.is_nan() || right.is_nan() {
            return left.is_nan() && right.is_nan();
        }
        if flags.num_eq {
            left == right
        } else {
            left.to_bits() == right.to_bits()
        }
    } else if flags.num_eq && !left.is_nan() && !right.is_nan() {
        left == right
    } else {
        left.to_bits() == right.to_bits()
    }
}

/// Whether two `CHARSXP`s hold the same string. The string cache makes equal strings of one
/// encoding the same object, so only strings of different encodings need their text compared.
unsafe fn chars_equal(left: Sexp, right: Sexp) -> bool {
    if left == right {
        return true;
    }
    let na = na_string();
    left != na && right != na && char_str(left) == char_str(right)
}

/// `.Internal(merge(xinds, yinds, all.x, all.y))`: `merge.data.frame` codes each row by its key's
/// position among the keys both sides share (0 for none), and this pairs the rows with equal
/// codes, in order of the `x` row and then the `y` row. With `all.x` (`all.y`) the rows of that
/// side that matched nothing are listed too.
unsafe fn do_merge(call: &Call<'_>) -> R<Sexp> {
    let x_codes = call.arg(0);
    let y_codes = call.arg(1);
    if x_codes.kind() != SexpType::Integer || y_codes.kind() != SexpType::Integer {
        return Err(call.error("invalid 'xinds' or 'yinds' argument"));
    }
    let all_x = as_logical_scalar(call.arg(2));
    let all_y = as_logical_scalar(call.arg(3));
    if all_x == NA_LOGICAL || all_y == NA_LOGICAL {
        return Err(call.error("'all.x' must be TRUE or FALSE"));
    }
    let x = values::ints(x_codes);
    let y = values::ints(y_codes);
    let mut rows_of_code: FxHashMap<i32, Vec<usize>> = FxHashMap::default();
    for (row, &code) in y.iter().enumerate() {
        if code > 0 {
            rows_of_code.entry(code).or_default().push(row + 1);
        }
    }
    let mut x_rows = Vec::new();
    let mut y_rows = Vec::new();
    for (row, &code) in x.iter().enumerate() {
        if let Some(matches) = rows_of_code.get(&code).filter(|_| code > 0) {
            for &y_row in matches {
                x_rows.push(row as i32 + 1);
                y_rows.push(y_row as i32);
            }
        }
    }
    let alone = |codes: &[i32], wanted: bool| -> R<Sexp> {
        if !wanted {
            return Ok(nil());
        }
        let rows: Vec<i32> = codes
            .iter()
            .enumerate()
            .filter(|(_, code)| **code <= 0)
            .map(|(row, _)| row as i32 + 1)
            .collect();
        values::integer_vector(&rows)
    };
    values::named_list(&[
        ("xi", values::integer_vector(&x_rows)?),
        ("yi", values::integer_vector(&y_rows)?),
        ("x.alone", alone(x, all_x == 1)?),
        ("y.alone", alone(y, all_y == 1)?),
    ])
}
