//! ry's standard library: the `Result`, `List`, `Str`, `Num`, `Dict` and `Set` functions, modeled
//! on Roc's builtins. `types/ry.Rtypes` declares their types, and the design and user contract
//! live in the docs (`contributing/design/standard-library.md`, `reference/standard-library.mdx`).
//!
//! The functions are bound in a `package:ry` environment just above `package:base`, so any
//! definition a program or package makes of the same name comes first. Session startup moves them
//! into the `ry` namespace beside the frame functions (`frame::attach_package`).
//! Every function is written for ry's own values, so each accepts a record, tuple or tag.
//!
//! A dictionary or set is a record object with a fixed layout (see `heap::records`): its keys as
//! one atomic vector, a dictionary's values as a list, and an open-addressing hash index as an
//! integer vector.
//!
//! The values are immutable to the program, but an update reuses its first argument when nothing
//! else can observe it: a temporary, or the value of `x` in `x <- Dict.insert(x, k, v)` when that
//! binding holds the only reference (`Call::reusable`, set by `eval::rebind`). A builtin flagged
//! with `reusing_first_argument` checks [`owned`] and then grows the value in place, so a loop
//! that keeps inserting or appending costs amortized constant time per step, as in Roc.

use {
    crate::{
        arrays,
        builtins::{Arg, BuiltinDef, Call, builtin},
        eval, records, scope,
        sort::{IdenticalFlags, identical},
        unwind::{R, error},
        values::{
            self, char_str, length, list_vector, missing_arg, nil, scalar_bool, scalar_integer,
            scalar_real, scalar_string,
        },
    },
    heap::{
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, Slot, is_na_real},
        records::{Fields, Layout, NativeKind},
    },
    rustc_hash::FxHasher,
    std::{
        cmp::Ordering,
        hash::{Hash, Hasher},
    },
};

/// A builtin of the library: it takes its arguments by name or position and accepts ry values.
const fn library(name: &'static str, function: crate::builtins::BuiltinFn) -> BuiltinDef {
    builtin(name, -1, function).accepting_native()
}

pub const BUILTINS: &[BuiltinDef] = &[
    library("Result.is_ok", result_is_ok),
    library("Result.is_err", result_is_err),
    library("Result.with_default", result_with_default),
    library("Result.map", result_map),
    library("Result.map_err", result_map_err),
    library("Result.map_both", result_map_both),
    library("Result.and_then", result_and_then),
    library("Result.on_err", result_on_err),
    library("Result.map2", result_map2),
    library("List.from_vector", list_from_vector),
    library("List.to_vector", list_to_vector),
    library("List.len", list_len),
    library("List.is_empty", list_is_empty),
    library("List.get", list_get),
    library("List.set", list_set).reusing_first_argument(),
    library("List.update", list_update),
    library("List.append", list_append).reusing_first_argument(),
    library("List.prepend", list_prepend),
    library("List.concat", list_concat).reusing_first_argument(),
    library("List.single", list_single),
    library("List.repeat", list_repeat),
    library("List.reverse", list_reverse),
    library("List.join", list_join),
    library("List.map", list_map),
    library("List.map_with_index", list_map_with_index),
    library("List.map2", list_map2),
    library("List.map3", list_map3),
    library("List.map4", list_map4),
    library("List.join_map", list_join_map),
    library("List.keep_if", list_keep_if),
    library("List.drop_if", list_drop_if),
    library("List.keep_oks", list_keep_oks),
    library("List.keep_errs", list_keep_errs),
    library("List.map_try", list_map_try),
    library("List.walk", list_walk),
    library("List.walk_backwards", list_walk_backwards),
    library("List.walk_until", list_walk_until),
    library("List.walk_backwards_until", list_walk_backwards_until),
    library("List.walk_with_index", list_walk_with_index),
    library("List.walk_with_index_until", list_walk_with_index_until),
    library("List.walk_from", list_walk_from),
    library("List.walk_from_until", list_walk_from_until),
    library("List.walk_try", list_walk_try),
    library("List.for_each", list_for_each).invisible(),
    library("List.for_each_try", list_for_each_try),
    library("List.any", list_any),
    library("List.all", list_all),
    library("List.contains", list_contains),
    library("List.count_if", list_count_if),
    library("List.find_first", list_find_first),
    library("List.find_last", list_find_last),
    library("List.find_first_index", list_find_first_index),
    library("List.find_last_index", list_find_last_index),
    library("List.first", list_first),
    library("List.last", list_last),
    library("List.take_first", list_take_first),
    library("List.take_last", list_take_last),
    library("List.drop_first", list_drop_first),
    library("List.drop_last", list_drop_last),
    library("List.drop_at", list_drop_at),
    library("List.sublist", list_sublist),
    library("List.split_at", list_split_at),
    library("List.chunks_of", list_chunks_of),
    library("List.intersperse", list_intersperse),
    library("List.sort_with", list_sort_with),
    library("List.sum", list_sum),
    library("List.product", list_product),
    library("List.min", list_min),
    library("List.max", list_max),
    library("List.starts_with", list_starts_with),
    library("List.ends_with", list_ends_with),
    library("List.sort_asc", list_sort_asc),
    library("List.sort_desc", list_sort_desc),
    library("List.swap", list_swap).reusing_first_argument(),
    library("List.replace", list_replace),
    library("List.split_first", list_split_first),
    library("List.split_last", list_split_last),
    library("List.split_on", list_split_on),
    library("List.split_on_list", list_split_on_list),
    library("List.append_if_ok", list_append_if_ok).reusing_first_argument(),
    library("List.prepend_if_ok", list_prepend_if_ok),
    library("List.concat_utf8", list_concat_utf8).reusing_first_argument(),
    library("List.with_capacity", list_with_capacity),
    library("List.reserve", list_reserve).reusing_first_argument(),
    library("Str.is_empty", str_is_empty),
    library("Str.concat", str_concat),
    library("Str.join_with", str_join_with),
    library("Str.split_on", str_split_on),
    library("Str.repeat", str_repeat),
    library("Str.starts_with", str_starts_with),
    library("Str.ends_with", str_ends_with),
    library("Str.contains", str_contains),
    library("Str.trim", str_trim),
    library("Str.trim_start", str_trim_start),
    library("Str.trim_end", str_trim_end),
    library("Str.drop_prefix", str_drop_prefix),
    library("Str.drop_suffix", str_drop_suffix),
    library("Str.with_prefix", str_with_prefix),
    library("Str.replace_each", str_replace_each),
    library("Str.replace_first", str_replace_first),
    library("Str.replace_last", str_replace_last),
    library("Str.split_first", str_split_first),
    library("Str.split_last", str_split_last),
    library("Str.count_utf8_bytes", str_count_utf8_bytes),
    library("Str.to_utf8", str_to_utf8),
    library("Str.from_utf8", str_from_utf8),
    library("Str.to_integer", str_to_integer),
    library("Str.to_double", str_to_double),
    library("Str.with_ascii_lowercased", str_with_ascii_lowercased),
    library("Str.with_ascii_uppercased", str_with_ascii_uppercased),
    library("Str.caseless_ascii_equals", str_caseless_ascii_equals),
    library("Str.walk_utf8", str_walk_utf8),
    library("Str.walk_utf8_with_index", str_walk_utf8_with_index),
    library("Str.from_utf8_lossy", str_from_utf8_lossy),
    library("Num.to_str", num_to_str),
    library("Num.compare", num_compare),
    library("Num.min", num_min),
    library("Num.max", num_max),
    library("Num.is_zero", num_is_zero),
    library("Num.is_positive", num_is_positive),
    library("Num.is_negative", num_is_negative),
    library("Num.is_even", num_is_even),
    library("Num.is_odd", num_is_odd),
    library("Num.add_checked", num_add_checked),
    library("Num.sub_checked", num_sub_checked),
    library("Num.mul_checked", num_mul_checked),
    library("Num.div_checked", num_div_checked),
    library("Num.div_trunc_checked", num_div_trunc_checked),
    library("Num.rem_checked", num_rem_checked),
    library("Num.sqrt_checked", num_sqrt_checked),
    library("Num.log_checked", num_log_checked),
    library("Num.is_lt", num_is_lt),
    library("Num.is_gt", num_is_gt),
    library("Num.is_lte", num_is_lte),
    library("Num.is_gte", num_is_gte),
    library("Num.is_approx_eq", num_is_approx_eq),
    library("Num.is_multiple_of", num_is_multiple_of),
    library("Num.is_finite", num_is_finite),
    library("Num.is_infinite", num_is_infinite),
    library("Num.abs", num_abs),
    library("Num.abs_diff", num_abs_diff),
    library("Num.neg", num_neg),
    library("Num.ceiling", num_ceiling),
    library("Num.floor", num_floor),
    library("Num.round", num_round),
    library("Num.pow", num_pow),
    library("Num.sin", num_sin),
    library("Num.cos", num_cos),
    library("Num.tan", num_tan),
    library("Num.atan", num_atan),
    library("Num.add_saturated", num_add_saturated),
    library("Num.sub_saturated", num_sub_saturated),
    library("Num.mul_saturated", num_mul_saturated),
    library("Num.div_ceil_checked", num_div_ceil_checked),
    library("Num.to_double", num_to_double),
    library("Num.to_integer_checked", num_to_integer_checked),
    library("Dict.empty", dict_empty),
    library("Dict.single", dict_single),
    library("Dict.from_list", dict_from_list),
    library("Dict.to_list", dict_to_list),
    library("Dict.len", dict_len),
    library("Dict.is_empty", dict_is_empty),
    library("Dict.get", dict_get),
    library("Dict.contains", dict_contains),
    library("Dict.insert", dict_insert).reusing_first_argument(),
    library("Dict.remove", dict_remove),
    library("Dict.update", dict_update),
    library("Dict.keys", dict_keys),
    library("Dict.values", dict_values),
    library("Dict.walk", dict_walk),
    library("Dict.map", dict_map),
    library("Dict.keep_if", dict_keep_if),
    library("Dict.drop_if", dict_drop_if),
    library("Dict.insert_all", dict_insert_all).reusing_first_argument(),
    library("Dict.keep_shared", dict_keep_shared),
    library("Dict.remove_all", dict_remove_all),
    library("Dict.walk_until", dict_walk_until),
    library("Dict.join_map", dict_join_map),
    library("Dict.reserve", dict_reserve).reusing_first_argument(),
    library("Dict.clear", dict_clear),
    library("Set.empty", set_empty),
    library("Set.single", set_single),
    library("Set.from_list", set_from_list),
    library("Set.to_list", set_to_list),
    library("Set.len", set_len),
    library("Set.is_empty", set_is_empty),
    library("Set.contains", set_contains),
    library("Set.insert", set_insert).reusing_first_argument(),
    library("Set.remove", set_remove),
    library("Set.union", set_union).reusing_first_argument(),
    library("Set.intersection", set_intersection),
    library("Set.difference", set_difference),
    library("Set.walk", set_walk),
    library("Set.map", set_map),
    library("Set.keep_if", set_keep_if),
    library("Set.drop_if", set_drop_if),
    library("Set.walk_until", set_walk_until),
    library("Set.join_map", set_join_map),
    library("Set.reserve", set_reserve).reusing_first_argument(),
];

/// Creates `package:ry` just above `package:base` and binds the library in it. At startup the
/// global environment's enclosure is the base environment, and `library()` attaches between the
/// global environment and its enclosure, so `package:ry` stays the last entry before base.
pub(crate) unsafe fn attach() -> Sexp {
    let heap = crate::heap();
    let base = heap.known().base_env;
    let global = heap.known().global_env;
    let env = heap
        .alloc_environment(base)
        .unwrap_or_else(|failure| panic!("cannot create `package:ry`: {failure}"));
    heap.set_enclosure(global, env);
    let name = values::install("name")
        .and_then(|symbol| Ok((symbol, scalar_string("package:ry")?)))
        .and_then(|(symbol, value)| values::set_attr(env, symbol, value));
    if name.is_err() {
        panic!("cannot name the `package:ry` environment");
    }
    env
}

// Arguments and errors.

/// The arguments for `formals`, matched by name or position. Every formal is required.
unsafe fn arguments<const N: usize>(call: &Call<'_>, formals: [&str; N]) -> R<[Sexp; N]> {
    // With no `...` formal, the matcher itself refuses an argument it cannot place.
    let (matched, _) = call.match_args(&formals)?;
    let mut values = [nil(); N];
    for (index, formal) in formals.iter().enumerate() {
        match matched.get(index).copied().flatten() {
            Some(value) if value != missing_arg() => values[index] = value,
            _ => {
                return Err(
                    call.error(format!("`{}()` needs the argument `{formal}`", call.name()))
                );
            }
        }
    }
    Ok(values)
}

/// An error naming the parameter and what it needs, as in "`list` must be a list, not a double
/// vector". A parameter written as a call, `f()`, stands for the value that function returned,
/// and the error reads "`f` must return ...".
fn wrong(call: &Call<'_>, parameter: &str, needed: &str, value: Sexp) -> crate::unwind::Unwind {
    // SAFETY: describing a value only reads it.
    let found = unsafe { describe(value) };
    match parameter.strip_suffix("()") {
        Some(function) => call.error(format!("`{function}` must return {needed}, not {found}")),
        None => call.error(format!("`{parameter}` must be {needed}, not {found}")),
    }
}

/// A short phrase for a value's kind: "a double vector", "NULL", "a ry tag".
unsafe fn describe(value: Sexp) -> String {
    match value.kind() {
        SexpType::Nil => "NULL".to_owned(),
        SexpType::Record => format!("a ry {}", records::native_kind(value).type_name()),
        SexpType::Logical => vector_phrase(value, "logical"),
        SexpType::Integer => vector_phrase(value, "integer"),
        SexpType::Real => vector_phrase(value, "double"),
        SexpType::Complex => vector_phrase(value, "complex"),
        SexpType::String => vector_phrase(value, "character"),
        SexpType::Raw => vector_phrase(value, "raw"),
        SexpType::List => "a list".to_owned(),
        SexpType::Closure | SexpType::Builtin | SexpType::Special => "a function".to_owned(),
        SexpType::Environment => "an environment".to_owned(),
        kind => format!("a value of type {}", kind.name()),
    }
}

unsafe fn vector_phrase(value: Sexp, kind: &str) -> String {
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    match length(value) {
        1 => match missing_scalar(value) {
            Some(missing) => missing.to_owned(),
            None => format!("{article} {kind} value"),
        },
        count => format!("{article} {kind} vector of length {count}"),
    }
}

/// How a length-one vector that holds no usable value reads in an error: `NA` or `NaN`.
unsafe fn missing_scalar(value: Sexp) -> Option<&'static str> {
    let value = values::materialize(value).ok()?;
    let missing = match value.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(value)[0] == NA_INTEGER,
        SexpType::Real if is_na_real(values::reals(value)[0]) => true,
        SexpType::Real if values::reals(value)[0].is_nan() => return Some("`NaN`"),
        SexpType::String => value.element(0) == values::na_string(),
        _ => false,
    };
    missing.then_some("`NA`")
}

/// The elements of an R list. A list's names are not part of the library's model, so they are
/// dropped.
unsafe fn list_elements(call: &Call<'_>, parameter: &str, value: Sexp) -> R<Vec<Sexp>> {
    if value.is_array() {
        return (0..length(value))
            .map(|index| scope(|| arrays::item(value, index)))
            .collect();
    }
    if value.kind() != SexpType::List {
        return Err(wrong(call, parameter, "a list", value));
    }
    Ok((0..length(value))
        .map(|index| value.element(index))
        .collect())
}

/// A collection the library returns: an Array when the collection the call works on (`like`)
/// is one, so `List.map` over an Array gives an Array, and an R list otherwise. Only that
/// argument decides, because an item argument may itself be an Array (`List.append(xs, [1])`).
/// Items an Array cannot hold, such as the vectors a mapped function returned, are an error
/// rather than a silent fall back to a list.
unsafe fn sequence(call: &Call<'_>, like: Sexp, items: &[Sexp]) -> R<Sexp> {
    if !like.is_array() {
        return list_vector(items);
    }
    arrays::from_items(items)?.map_err(|message| {
        call.error(format!(
            "`{}()` received an Array, so its result must be one: {message}",
            call.name()
        ))
    })
}

unsafe fn function(call: &Call<'_>, parameter: &str, value: Sexp) -> R<Sexp> {
    if !values::is_function(value) {
        return Err(wrong(call, parameter, "a function", value));
    }
    Ok(value)
}

/// Calls `function` with positional `arguments`, through the interpreter's ordinary call path.
unsafe fn invoke(call: &Call<'_>, function: Sexp, arguments: &[Sexp]) -> R<Sexp> {
    let arguments: Vec<Arg> = arguments
        .iter()
        .map(|&value| Arg { tag: nil(), value })
        .collect();
    eval::apply(nil(), function, &arguments, call.env)
}

/// A predicate's answer, which must be `TRUE` or `FALSE`.
unsafe fn verdict(call: &Call<'_>, parameter: &str, value: Sexp) -> R<bool> {
    if value.kind() == SexpType::Logical && length(value) == 1 {
        match values::as_logical_scalar(value) {
            NA_LOGICAL => {}
            flag => return Ok(flag != 0),
        }
    }
    Err(wrong(
        call,
        &format!("{parameter}()"),
        "`TRUE` or `FALSE`",
        value,
    ))
}

/// A whole number that fits an R integer, given as an integer or a whole double.
unsafe fn whole(call: &Call<'_>, parameter: &str, value: Sexp) -> R<i64> {
    if length(value) == 1 && !value.is_object() {
        match value.kind() {
            SexpType::Integer => {
                let number = values::as_integer_scalar(value);
                if number != NA_INTEGER {
                    return Ok(i64::from(number));
                }
            }
            SexpType::Real => {
                let number = values::as_real_scalar(value);
                if number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= f64::from(i32::MAX)
                {
                    return Ok(number as i64);
                }
            }
            _ => {}
        }
    }
    Err(wrong(call, parameter, "a whole number", value))
}

unsafe fn count(call: &Call<'_>, parameter: &str, value: Sexp) -> R<usize> {
    let number = whole(call, parameter, value)?;
    usize::try_from(number).map_err(|_| wrong(call, parameter, "zero or more", value))
}

/// A length-one string that is not `NA`.
unsafe fn text<'a>(call: &Call<'_>, parameter: &str, value: Sexp) -> R<std::borrow::Cow<'a, str>> {
    if value.kind() == SexpType::String && length(value) == 1 {
        let charsxp = value.element(0);
        if charsxp != values::na_string() {
            return Ok(char_str(charsxp));
        }
    }
    Err(wrong(call, parameter, "a single string", value))
}

// Tags.

unsafe fn tag(name: &str, payload: &[Sexp]) -> R<Sexp> {
    let symbol = values::install(name)?;
    let id = crate::heap().layouts.intern_tag(symbol);
    let items: Vec<(Option<Sexp>, Sexp)> = payload.iter().map(|&value| (None, value)).collect();
    records::make(Some(id), &items).map_err(error)
}

unsafe fn ok(value: Sexp) -> R<Sexp> {
    tag("Ok", &[value])
}

/// `:Err(:Name)`.
unsafe fn err(name: &str) -> R<Sexp> {
    let reason = tag(name, &[])?;
    tag("Err", &[reason])
}

unsafe fn record(fields: &[(&str, Sexp)]) -> R<Sexp> {
    let mut items = Vec::with_capacity(fields.len());
    for &(name, value) in fields {
        items.push((Some(values::install(name)?), value));
    }
    records::make(None, &items).map_err(error)
}

/// The name and the single payload value of a tag, or `None` for anything else.
unsafe fn tag_parts(value: Sexp) -> R<Option<(String, Option<Sexp>)>> {
    if value.kind() != SexpType::Record {
        return Ok(None);
    }
    let layout = records::layout(value);
    let Some(id) = layout.tag else {
        return Ok(None);
    };
    let payload = match &layout.fields {
        Fields::Positional(1) => Some(records::slot_value(value, 0)?),
        Fields::Positional(0) => None,
        _ => return Ok(Some((records::tag_name(id), Some(value)))),
    };
    Ok(Some((records::tag_name(id), payload)))
}

/// `Ok(true, value)` for `:Ok(value)` and `Ok(false, error)` for `:Err(error)`.
unsafe fn result_parts(call: &Call<'_>, parameter: &str, value: Sexp) -> R<(bool, Sexp)> {
    match tag_parts(value)? {
        Some((name, Some(payload))) if name == "Ok" => Ok((true, payload)),
        Some((name, Some(payload))) if name == "Err" => Ok((false, payload)),
        _ => Err(wrong(
            call,
            parameter,
            "a result, `:Ok(value)` or `:Err(error)`",
            value,
        )),
    }
}

/// A function's result that must itself be a result, as `and_then`'s `f` returns.
unsafe fn returned_result(call: &Call<'_>, parameter: &str, value: Sexp) -> R<(bool, Sexp)> {
    result_parts(call, &format!("{parameter}()"), value)
}

// Result.

unsafe fn result_is_ok(call: &Call<'_>) -> R<Sexp> {
    let [result] = arguments(call, ["result"])?;
    scalar_bool(result_parts(call, "result", result)?.0)
}

unsafe fn result_is_err(call: &Call<'_>) -> R<Sexp> {
    let [result] = arguments(call, ["result"])?;
    scalar_bool(!result_parts(call, "result", result)?.0)
}

unsafe fn result_with_default(call: &Call<'_>) -> R<Sexp> {
    let [result, default] = arguments(call, ["result", "default"])?;
    match result_parts(call, "result", result)? {
        (true, value) => Ok(value),
        (false, _) => Ok(default),
    }
}

unsafe fn result_map(call: &Call<'_>) -> R<Sexp> {
    let [result, f] = arguments(call, ["result", "f"])?;
    let f = function(call, "f", f)?;
    match result_parts(call, "result", result)? {
        (true, value) => ok(invoke(call, f, &[value])?),
        (false, _) => Ok(result),
    }
}

unsafe fn result_map_err(call: &Call<'_>) -> R<Sexp> {
    let [result, f] = arguments(call, ["result", "f"])?;
    let f = function(call, "f", f)?;
    match result_parts(call, "result", result)? {
        (true, _) => Ok(result),
        (false, reason) => tag("Err", &[invoke(call, f, &[reason])?]),
    }
}

unsafe fn result_map_both(call: &Call<'_>) -> R<Sexp> {
    let [result, on_ok, on_err] = arguments(call, ["result", "on_ok", "on_err"])?;
    let on_ok = function(call, "on_ok", on_ok)?;
    let on_err = function(call, "on_err", on_err)?;
    match result_parts(call, "result", result)? {
        (true, value) => ok(invoke(call, on_ok, &[value])?),
        (false, reason) => tag("Err", &[invoke(call, on_err, &[reason])?]),
    }
}

unsafe fn result_and_then(call: &Call<'_>) -> R<Sexp> {
    let [result, f] = arguments(call, ["result", "f"])?;
    let f = function(call, "f", f)?;
    match result_parts(call, "result", result)? {
        (true, value) => {
            let next = invoke(call, f, &[value])?;
            returned_result(call, "f", next)?;
            Ok(next)
        }
        (false, _) => Ok(result),
    }
}

unsafe fn result_on_err(call: &Call<'_>) -> R<Sexp> {
    let [result, f] = arguments(call, ["result", "f"])?;
    let f = function(call, "f", f)?;
    match result_parts(call, "result", result)? {
        (true, _) => Ok(result),
        (false, reason) => {
            let next = invoke(call, f, &[reason])?;
            returned_result(call, "f", next)?;
            Ok(next)
        }
    }
}

unsafe fn result_map2(call: &Call<'_>) -> R<Sexp> {
    let [first, second, f] = arguments(call, ["first", "second", "f"])?;
    let f = function(call, "f", f)?;
    let (first_ok, left) = result_parts(call, "first", first)?;
    let (second_ok, right) = result_parts(call, "second", second)?;
    match (first_ok, second_ok) {
        (true, true) => ok(invoke(call, f, &[left, right])?),
        (false, _) => Ok(first),
        (true, false) => Ok(second),
    }
}

// List.

unsafe fn list_from_vector(call: &Call<'_>) -> R<Sexp> {
    let [vector] = arguments(call, ["vector"])?;
    let atomic = matches!(
        vector.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
    );
    if !atomic {
        return Err(wrong(call, "vector", "an atomic vector", vector));
    }
    let vector = values::materialize(vector)?;
    let mut items = Vec::with_capacity(length(vector));
    for index in 0..length(vector) {
        items.push(scope(|| element_scalar(vector, index))?);
    }
    list_vector(&items)
}

/// Element `index` of an atomic vector, as a length-one vector of the same type.
unsafe fn element_scalar(vector: Sexp, index: usize) -> R<Sexp> {
    match vector.kind() {
        SexpType::Logical => values::scalar_logical(values::ints(vector)[index]),
        SexpType::Integer => scalar_integer(values::ints(vector)[index]),
        SexpType::Real => scalar_real(values::reals(vector)[index]),
        SexpType::Complex => values::scalar_complex(values::complexes(vector)[index]),
        SexpType::String => values::scalar_string_charsxp(vector.element(index)),
        SexpType::Raw => values::scalar_raw(vector.slice::<u8>()[index]),
        _ => Err(error("an atomic vector was expected")),
    }
}

unsafe fn list_to_vector(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    let items = list_elements(call, "list", list)?;
    let mut kind: Option<SexpType> = None;
    for &item in &items {
        let atomic = matches!(
            item.kind(),
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::String
                | SexpType::Raw
        );
        if !atomic || length(item) != 1 {
            return Err(wrong(call, "list", "a list of atomic scalars", item));
        }
        kind = Some(match (kind, item.kind()) {
            (None, found) => found,
            (Some(current), found) if current == found => current,
            (Some(SexpType::Integer), SexpType::Real)
            | (Some(SexpType::Real), SexpType::Integer) => SexpType::Real,
            (Some(current), found) => {
                return Err(call.error(format!(
                    "`list` must hold elements of one atomic type, and it mixes {} and {}",
                    current.name(),
                    found.name()
                )));
            }
        });
    }
    let kind = kind.unwrap_or(SexpType::Logical);
    let vector = values::alloc(kind, items.len())?;
    for (index, &item) in items.iter().enumerate() {
        let item = values::materialize(item)?;
        match kind {
            SexpType::Logical | SexpType::Integer => {
                vector.slice_mut::<i32>()[index] = values::ints(item)[0];
            }
            SexpType::Real => vector.slice_mut::<f64>()[index] = values::as_real_scalar(item),
            SexpType::Complex => {
                vector.slice_mut::<heap::object::Rcomplex>()[index] = values::complexes(item)[0];
            }
            SexpType::Raw => vector.slice_mut::<u8>()[index] = item.slice::<u8>()[0],
            _ => crate::heap().set_element(vector, index, item.element(0)),
        }
    }
    Ok(vector)
}

unsafe fn list_len(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    let items = list_elements(call, "list", list)?;
    scalar_integer(items.len() as i32)
}

unsafe fn list_is_empty(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    scalar_bool(list_elements(call, "list", list)?.is_empty())
}

/// A 1-based position as a 0-based index into `count` elements, if it is in range.
fn position(index: i64, count: usize) -> Option<usize> {
    let index = usize::try_from(index).ok()?;
    (1..=count).contains(&index).then(|| index - 1)
}

unsafe fn list_get(call: &Call<'_>) -> R<Sexp> {
    let [list, index] = arguments(call, ["list", "index"])?;
    let items = list_elements(call, "list", list)?;
    let index = whole(call, "index", index)?;
    match position(index, items.len()) {
        Some(at) => ok(items[at]),
        None => err("OutOfBounds"),
    }
}

unsafe fn list_set(call: &Call<'_>) -> R<Sexp> {
    let [list, index, value] = arguments(call, ["list", "index", "value"])?;
    if owned_list(call, list) {
        let index = whole(call, "index", index)?;
        if let Some(at) = position(index, length(list)) {
            crate::heap().set_element(list, at, value);
        }
        return Ok(list);
    }
    let mut items = list_elements(call, "list", list)?;
    let index = whole(call, "index", index)?;
    if let Some(at) = position(index, items.len()) {
        items[at] = value;
    }
    sequence(call, list, &items)
}

unsafe fn list_update(call: &Call<'_>) -> R<Sexp> {
    let [list, index, f] = arguments(call, ["list", "index", "f"])?;
    let mut items = list_elements(call, "list", list)?;
    let index = whole(call, "index", index)?;
    let f = function(call, "f", f)?;
    if let Some(at) = position(index, items.len()) {
        items[at] = invoke(call, f, &[items[at]])?;
    }
    sequence(call, list, &items)
}

unsafe fn list_append(call: &Call<'_>) -> R<Sexp> {
    let [list, value] = arguments(call, ["list", "value"])?;
    if owned_list(call, list) {
        return extend_in_place(list, &[value]);
    }
    let mut items = list_elements(call, "list", list)?;
    items.push(value);
    sequence(call, list, &items)
}

unsafe fn list_prepend(call: &Call<'_>) -> R<Sexp> {
    let [list, value] = arguments(call, ["list", "value"])?;
    let mut items = vec![value];
    items.extend(list_elements(call, "list", list)?);
    sequence(call, list, &items)
}

unsafe fn list_concat(call: &Call<'_>) -> R<Sexp> {
    let [first, second] = arguments(call, ["first", "second"])?;
    if owned_list(call, first) {
        let added = list_elements(call, "second", second)?;
        return extend_in_place(first, &added);
    }
    let mut items = list_elements(call, "first", first)?;
    items.extend(list_elements(call, "second", second)?);
    sequence(call, first, &items)
}

unsafe fn list_single(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["value"])?;
    list_vector(&[value])
}

unsafe fn list_repeat(call: &Call<'_>) -> R<Sexp> {
    let [value, times] = arguments(call, ["value", "count"])?;
    let times = count(call, "count", times)?;
    let mut items = Vec::new();
    items
        .try_reserve_exact(times)
        .map_err(|_| error(format!("cannot allocate a list of {times} elements")))?;
    items.resize(times, value);
    list_vector(&items)
}

unsafe fn list_reverse(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    let mut items = list_elements(call, "list", list)?;
    items.reverse();
    sequence(call, list, &items)
}

unsafe fn list_join(call: &Call<'_>) -> R<Sexp> {
    let [lists] = arguments(call, ["lists"])?;
    let mut items = Vec::new();
    for inner in list_elements(call, "lists", lists)? {
        if inner.kind() != SexpType::List {
            return Err(wrong(call, "lists", "a list of lists", inner));
        }
        items.extend((0..length(inner)).map(|index| inner.element(index)));
    }
    sequence(call, lists, &items)
}

unsafe fn list_map(call: &Call<'_>) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    let mut mapped = Vec::with_capacity(items.len());
    for item in items {
        mapped.push(scope(|| invoke(call, f, &[item]))?);
    }
    sequence(call, list, &mapped)
}

unsafe fn list_map_with_index(call: &Call<'_>) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    let mut mapped = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        mapped.push(scope(|| {
            let index = scalar_integer(index as i32 + 1)?;
            invoke(call, f, &[item, index])
        })?);
    }
    sequence(call, list, &mapped)
}

unsafe fn list_map2(call: &Call<'_>) -> R<Sexp> {
    let [first, second, f] = arguments(call, ["first", "second", "f"])?;
    map_lists(call, &[("first", first), ("second", second)], f)
}

unsafe fn list_map3(call: &Call<'_>) -> R<Sexp> {
    let [first, second, third, f] = arguments(call, ["first", "second", "third", "f"])?;
    map_lists(
        call,
        &[("first", first), ("second", second), ("third", third)],
        f,
    )
}

unsafe fn list_map4(call: &Call<'_>) -> R<Sexp> {
    let [first, second, third, fourth, f] =
        arguments(call, ["first", "second", "third", "fourth", "f"])?;
    let lists = [
        ("first", first),
        ("second", second),
        ("third", third),
        ("fourth", fourth),
    ];
    map_lists(call, &lists, f)
}

/// Calls `f` with the elements at each position of the named `lists`, up to the shortest one.
unsafe fn map_lists(call: &Call<'_>, lists: &[(&str, Sexp)], f: Sexp) -> R<Sexp> {
    let mut columns = Vec::with_capacity(lists.len());
    for &(parameter, list) in lists {
        columns.push(list_elements(call, parameter, list)?);
    }
    let f = function(call, "f", f)?;
    let shortest = columns.iter().map(Vec::len).min().unwrap_or(0);
    let mut mapped = Vec::with_capacity(shortest);
    for at in 0..shortest {
        let row: Vec<Sexp> = columns.iter().map(|column| column[at]).collect();
        mapped.push(scope(|| invoke(call, f, &row))?);
    }
    let like = lists.first().map_or(nil(), |&(_, list)| list);
    sequence(call, like, &mapped)
}

unsafe fn list_join_map(call: &Call<'_>) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    let mut joined = Vec::new();
    for item in items {
        let inner = scope(|| invoke(call, f, &[item]))?;
        if inner.kind() != SexpType::List {
            return Err(wrong(call, "f()", "a list", inner));
        }
        joined.extend((0..length(inner)).map(|index| inner.element(index)));
    }
    sequence(call, list, &joined)
}

/// The elements for which `predicate` answers `keep`.
unsafe fn filter(call: &Call<'_>, keep: bool) -> R<Sexp> {
    let [list, predicate] = arguments(call, ["list", "predicate"])?;
    let items = list_elements(call, "list", list)?;
    let predicate = function(call, "predicate", predicate)?;
    let mut kept = Vec::new();
    for item in items {
        let answer = scope(|| invoke(call, predicate, &[item]))?;
        if verdict(call, "predicate", answer)? == keep {
            kept.push(item);
        }
    }
    sequence(call, list, &kept)
}

unsafe fn list_keep_if(call: &Call<'_>) -> R<Sexp> {
    filter(call, true)
}

unsafe fn list_drop_if(call: &Call<'_>) -> R<Sexp> {
    filter(call, false)
}

/// The payloads of the results `f` returns that are `:Ok` (when `oks`) or `:Err`.
unsafe fn keep_results(call: &Call<'_>, oks: bool) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    let mut kept = Vec::new();
    for item in items {
        let result = scope(|| invoke(call, f, &[item]))?;
        let (is_ok, payload) = returned_result(call, "f", result)?;
        if is_ok == oks {
            kept.push(payload);
        }
    }
    sequence(call, list, &kept)
}

unsafe fn list_keep_oks(call: &Call<'_>) -> R<Sexp> {
    keep_results(call, true)
}

unsafe fn list_keep_errs(call: &Call<'_>) -> R<Sexp> {
    keep_results(call, false)
}

unsafe fn list_map_try(call: &Call<'_>) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    let mut mapped = Vec::with_capacity(items.len());
    for item in items {
        let result = scope(|| invoke(call, f, &[item]))?;
        match returned_result(call, "f", result)? {
            (true, value) => mapped.push(value),
            (false, _) => return Ok(result),
        }
    }
    ok(sequence(call, list, &mapped)?)
}

/// How a walk visits a list's elements and what it passes `f`.
#[derive(Clone, Copy)]
struct Walk {
    /// From the last element to the first.
    backwards: bool,
    /// `f` also receives the element's 1-based position.
    with_index: bool,
    /// `f` returns `:Continue(state)` or `:Break(state)`, and a break ends the walk.
    until: bool,
}

const FORWARDS: Walk = Walk {
    backwards: false,
    with_index: false,
    until: false,
};

/// Folds `f` over `items` from 0-based position `start`, as `walk` describes.
unsafe fn walk_items(
    call: &Call<'_>,
    items: &[Sexp],
    start: usize,
    state: Sexp,
    f: Sexp,
    walk: Walk,
) -> R<Sexp> {
    let f = function(call, "f", f)?;
    let positions = start.min(items.len())..items.len();
    let order: Box<dyn Iterator<Item = usize>> = if walk.backwards {
        Box::new(positions.rev())
    } else {
        Box::new(positions)
    };
    let mut state = state;
    for at in order {
        let step = scope(|| {
            if walk.with_index {
                let index = scalar_integer(at as i32 + 1)?;
                invoke(call, f, &[state, items[at], index])
            } else {
                invoke(call, f, &[state, items[at]])
            }
        })?;
        if !walk.until {
            state = step;
            continue;
        }
        match walk_step(call, step)? {
            Ok(next) => state = next,
            Err(last) => return Ok(last),
        }
    }
    Ok(state)
}

/// A `walk_until` step: `Ok(state)` for `:Continue(state)` and `Err(state)` for `:Break(state)`.
unsafe fn walk_step(call: &Call<'_>, step: Sexp) -> R<Result<Sexp, Sexp>> {
    match tag_parts(step)? {
        Some((name, Some(next))) if name == "Continue" => Ok(Ok(next)),
        Some((name, Some(last))) if name == "Break" => Ok(Err(last)),
        _ => Err(wrong(
            call,
            "f()",
            "`:Continue(state)` or `:Break(state)`",
            step,
        )),
    }
}

unsafe fn walk_list(call: &Call<'_>, walk: Walk) -> R<Sexp> {
    let [list, state, f] = arguments(call, ["list", "state", "f"])?;
    let items = list_elements(call, "list", list)?;
    walk_items(call, &items, 0, state, f, walk)
}

unsafe fn list_walk(call: &Call<'_>) -> R<Sexp> {
    walk_list(call, FORWARDS)
}

unsafe fn list_walk_backwards(call: &Call<'_>) -> R<Sexp> {
    let walk = Walk {
        backwards: true,
        ..FORWARDS
    };
    walk_list(call, walk)
}

unsafe fn list_walk_until(call: &Call<'_>) -> R<Sexp> {
    walk_list(
        call,
        Walk {
            until: true,
            ..FORWARDS
        },
    )
}

unsafe fn list_walk_backwards_until(call: &Call<'_>) -> R<Sexp> {
    let walk = Walk {
        backwards: true,
        until: true,
        ..FORWARDS
    };
    walk_list(call, walk)
}

unsafe fn list_walk_with_index(call: &Call<'_>) -> R<Sexp> {
    let walk = Walk {
        with_index: true,
        ..FORWARDS
    };
    walk_list(call, walk)
}

unsafe fn list_walk_with_index_until(call: &Call<'_>) -> R<Sexp> {
    let walk = Walk {
        with_index: true,
        until: true,
        ..FORWARDS
    };
    walk_list(call, walk)
}

unsafe fn walk_from(call: &Call<'_>, until: bool) -> R<Sexp> {
    let [list, index, state, f] = arguments(call, ["list", "index", "state", "f"])?;
    let items = list_elements(call, "list", list)?;
    let start = start_position(call, "index", index)?;
    walk_items(call, &items, start, state, f, Walk { until, ..FORWARDS })
}

unsafe fn list_walk_from(call: &Call<'_>) -> R<Sexp> {
    walk_from(call, false)
}

unsafe fn list_walk_from_until(call: &Call<'_>) -> R<Sexp> {
    walk_from(call, true)
}

unsafe fn list_walk_try(call: &Call<'_>) -> R<Sexp> {
    let [list, state, f] = arguments(call, ["list", "state", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    let mut state = state;
    for item in items {
        let step = scope(|| invoke(call, f, &[state, item]))?;
        match returned_result(call, "f", step)? {
            (true, next) => state = next,
            (false, _) => return Ok(step),
        }
    }
    ok(state)
}

unsafe fn list_for_each(call: &Call<'_>) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    for item in items {
        scope(|| invoke(call, f, &[item]))?;
    }
    Ok(nil())
}

unsafe fn list_for_each_try(call: &Call<'_>) -> R<Sexp> {
    let [list, f] = arguments(call, ["list", "f"])?;
    let items = list_elements(call, "list", list)?;
    let f = function(call, "f", f)?;
    for item in items {
        let result = scope(|| invoke(call, f, &[item]))?;
        if !returned_result(call, "f", result)?.0 {
            return Ok(result);
        }
    }
    ok(nil())
}

/// The position of the first (or, `from_end`, the last) element `predicate` accepts.
unsafe fn find(call: &Call<'_>, from_end: bool) -> R<(Vec<Sexp>, Option<usize>)> {
    let [list, predicate] = arguments(call, ["list", "predicate"])?;
    let items = list_elements(call, "list", list)?;
    let predicate = function(call, "predicate", predicate)?;
    let mut order: Vec<usize> = (0..items.len()).collect();
    if from_end {
        order.reverse();
    }
    for at in order {
        let answer = scope(|| invoke(call, predicate, &[items[at]]))?;
        if verdict(call, "predicate", answer)? {
            return Ok((items, Some(at)));
        }
    }
    Ok((items, None))
}

unsafe fn list_any(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(find(call, false)?.1.is_some())
}

unsafe fn list_all(call: &Call<'_>) -> R<Sexp> {
    let [list, predicate] = arguments(call, ["list", "predicate"])?;
    let items = list_elements(call, "list", list)?;
    let predicate = function(call, "predicate", predicate)?;
    for item in items {
        let answer = scope(|| invoke(call, predicate, &[item]))?;
        if !verdict(call, "predicate", answer)? {
            return scalar_bool(false);
        }
    }
    scalar_bool(true)
}

unsafe fn list_contains(call: &Call<'_>) -> R<Sexp> {
    let [list, value] = arguments(call, ["list", "value"])?;
    let items = list_elements(call, "list", list)?;
    let flags = IdenticalFlags::default();
    scalar_bool(items.iter().any(|&item| identical(item, value, &flags)))
}

unsafe fn list_count_if(call: &Call<'_>) -> R<Sexp> {
    let [list, predicate] = arguments(call, ["list", "predicate"])?;
    let items = list_elements(call, "list", list)?;
    let predicate = function(call, "predicate", predicate)?;
    let mut counted = 0;
    for item in items {
        let answer = scope(|| invoke(call, predicate, &[item]))?;
        if verdict(call, "predicate", answer)? {
            counted += 1;
        }
    }
    scalar_integer(counted)
}

unsafe fn found_element(call: &Call<'_>, from_end: bool) -> R<Sexp> {
    match find(call, from_end)? {
        (items, Some(at)) => ok(items[at]),
        (_, None) => err("NotFound"),
    }
}

unsafe fn found_index(call: &Call<'_>, from_end: bool) -> R<Sexp> {
    match find(call, from_end)? {
        (_, Some(at)) => ok(scalar_integer(at as i32 + 1)?),
        (_, None) => err("NotFound"),
    }
}

unsafe fn list_find_first(call: &Call<'_>) -> R<Sexp> {
    found_element(call, false)
}

unsafe fn list_find_last(call: &Call<'_>) -> R<Sexp> {
    found_element(call, true)
}

unsafe fn list_find_first_index(call: &Call<'_>) -> R<Sexp> {
    found_index(call, false)
}

unsafe fn list_find_last_index(call: &Call<'_>) -> R<Sexp> {
    found_index(call, true)
}

unsafe fn list_first(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    match list_elements(call, "list", list)?.first() {
        Some(&item) => ok(item),
        None => err("ListWasEmpty"),
    }
}

unsafe fn list_last(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    match list_elements(call, "list", list)?.last() {
        Some(&item) => ok(item),
        None => err("ListWasEmpty"),
    }
}

/// The list and a count of elements, clamped to its length.
unsafe fn list_and_count(call: &Call<'_>) -> R<(Sexp, Vec<Sexp>, usize)> {
    let [list, taken] = arguments(call, ["list", "count"])?;
    let items = list_elements(call, "list", list)?;
    let taken = count(call, "count", taken)?.min(items.len());
    Ok((list, items, taken))
}

unsafe fn list_take_first(call: &Call<'_>) -> R<Sexp> {
    let (list, items, taken) = list_and_count(call)?;
    sequence(call, list, &items[..taken])
}

unsafe fn list_take_last(call: &Call<'_>) -> R<Sexp> {
    let (list, items, taken) = list_and_count(call)?;
    sequence(call, list, &items[items.len() - taken..])
}

unsafe fn list_drop_first(call: &Call<'_>) -> R<Sexp> {
    let (list, items, dropped) = list_and_count(call)?;
    sequence(call, list, &items[dropped..])
}

unsafe fn list_drop_last(call: &Call<'_>) -> R<Sexp> {
    let (list, items, dropped) = list_and_count(call)?;
    sequence(call, list, &items[..items.len() - dropped])
}

unsafe fn list_drop_at(call: &Call<'_>) -> R<Sexp> {
    let [list, index] = arguments(call, ["list", "index"])?;
    let mut items = list_elements(call, "list", list)?;
    let index = whole(call, "index", index)?;
    if let Some(at) = position(index, items.len()) {
        items.remove(at);
    }
    sequence(call, list, &items)
}

/// A 1-based start position as a 0-based index, which may lie past the end.
unsafe fn start_position(call: &Call<'_>, parameter: &str, value: Sexp) -> R<usize> {
    match usize::try_from(whole(call, parameter, value)?) {
        Ok(start) if start >= 1 => Ok(start - 1),
        _ => Err(wrong(call, parameter, "a position of 1 or more", value)),
    }
}

unsafe fn list_sublist(call: &Call<'_>) -> R<Sexp> {
    let [list, start, taken] = arguments(call, ["list", "start", "count"])?;
    let items = list_elements(call, "list", list)?;
    let start = start_position(call, "start", start)?;
    let taken = count(call, "count", taken)?;
    let from = start.min(items.len());
    let to = from.saturating_add(taken).min(items.len());
    sequence(call, list, &items[from..to])
}

unsafe fn list_split_at(call: &Call<'_>) -> R<Sexp> {
    let (list, items, before) = list_and_count(call)?;
    let first = sequence(call, list, &items[..before])?;
    let others = sequence(call, list, &items[before..])?;
    record(&[("before", first), ("others", others)])
}

unsafe fn list_chunks_of(call: &Call<'_>) -> R<Sexp> {
    let [list, size] = arguments(call, ["list", "size"])?;
    let items = list_elements(call, "list", list)?;
    let size = count(call, "size", size)?;
    if size == 0 {
        return sequence(call, list, &[]);
    }
    let mut chunks = Vec::with_capacity(items.len().div_ceil(size));
    for chunk in items.chunks(size) {
        chunks.push(sequence(call, list, chunk)?);
    }
    sequence(call, list, &chunks)
}

unsafe fn list_intersperse(call: &Call<'_>) -> R<Sexp> {
    let [list, separator] = arguments(call, ["list", "separator"])?;
    let items = list_elements(call, "list", list)?;
    let mut spaced = Vec::with_capacity((items.len() * 2).saturating_sub(1));
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            spaced.push(separator);
        }
        spaced.push(item);
    }
    sequence(call, list, &spaced)
}

/// An ordering tag, `:LT`, `:EQ` or `:GT`.
unsafe fn ordering(call: &Call<'_>, parameter: &str, value: Sexp) -> R<Ordering> {
    match tag_parts(value)? {
        Some((name, None)) if name == "LT" => Ok(Ordering::Less),
        Some((name, None)) if name == "EQ" => Ok(Ordering::Equal),
        Some((name, None)) if name == "GT" => Ok(Ordering::Greater),
        _ => Err(wrong(call, parameter, "`:LT`, `:EQ` or `:GT`", value)),
    }
}

unsafe fn ordering_tag(order: Ordering) -> R<Sexp> {
    tag(
        match order {
            Ordering::Less => "LT",
            Ordering::Equal => "EQ",
            Ordering::Greater => "GT",
        },
        &[],
    )
}

unsafe fn list_sort_with(call: &Call<'_>) -> R<Sexp> {
    let [list, compare] = arguments(call, ["list", "compare"])?;
    let mut items = list_elements(call, "list", list)?;
    let compare = function(call, "compare", compare)?;
    // A merge sort of our own, because the comparator is user code: it can fail, and it need not
    // be a total order, which the standard library's sorts are allowed to panic on.
    let mut buffer = items.clone();
    merge_sort(&mut items, &mut buffer, &mut |left, right| {
        let answer = scope(|| invoke(call, compare, &[left, right]))?;
        ordering(call, "compare()", answer)
    })?;
    sequence(call, list, &items)
}

/// A stable merge sort of `items`, with `buffer` as scratch space of the same length.
unsafe fn merge_sort(
    items: &mut [Sexp],
    buffer: &mut [Sexp],
    compare: &mut dyn FnMut(Sexp, Sexp) -> R<Ordering>,
) -> R<()> {
    let count = items.len();
    if count <= 1 {
        return Ok(());
    }
    let middle = count / 2;
    merge_sort(&mut items[..middle], &mut buffer[..middle], compare)?;
    merge_sort(&mut items[middle..], &mut buffer[middle..], compare)?;
    buffer.copy_from_slice(items);
    let (mut left, mut right, mut out) = (0, middle, 0);
    while left < middle && right < count {
        if compare(buffer[right], buffer[left])? == Ordering::Less {
            items[out] = buffer[right];
            right += 1;
        } else {
            items[out] = buffer[left];
            left += 1;
        }
        out += 1;
    }
    let rest = if left < middle {
        &buffer[left..middle]
    } else {
        &buffer[right..count]
    };
    items[out..].copy_from_slice(rest);
    Ok(())
}

/// A number the library computes with: a length-one integer or double that is not `NA`.
#[derive(Clone, Copy)]
enum Number {
    Integer(i32),
    Double(f64),
}

impl Number {
    fn as_double(self) -> f64 {
        match self {
            Number::Integer(value) => f64::from(value),
            Number::Double(value) => value,
        }
    }
}

unsafe fn number(call: &Call<'_>, parameter: &str, value: Sexp) -> R<Number> {
    if length(value) == 1 && !value.is_object() {
        match value.kind() {
            SexpType::Integer => {
                let number = values::as_integer_scalar(value);
                if number != NA_INTEGER {
                    return Ok(Number::Integer(number));
                }
            }
            SexpType::Real => {
                let number = values::as_real_scalar(value);
                if !number.is_nan() {
                    return Ok(Number::Double(number));
                }
            }
            _ => {}
        }
    }
    Err(wrong(call, parameter, "a single number", value))
}

unsafe fn number_value(number: Number) -> R<Sexp> {
    match number {
        Number::Integer(value) => scalar_integer(value),
        Number::Double(value) => scalar_real(value),
    }
}

/// The numbers of a list, all integers or (if any is a double) all doubles.
unsafe fn list_numbers(call: &Call<'_>) -> R<Vec<Number>> {
    let [list] = arguments(call, ["list"])?;
    let items = list_elements(call, "list", list)?;
    let mut numbers = Vec::with_capacity(items.len());
    for item in items {
        numbers.push(number(call, "list", item)?);
    }
    if numbers
        .iter()
        .any(|number| matches!(number, Number::Double(_)))
    {
        for number in &mut numbers {
            *number = Number::Double(number.as_double());
        }
    }
    Ok(numbers)
}

/// Folds the list's numbers with `integer` (checked) or `double`, starting at `start`.
unsafe fn fold_numbers(
    call: &Call<'_>,
    start: i32,
    integer: fn(i32, i32) -> Option<i32>,
    double: fn(f64, f64) -> f64,
) -> R<Sexp> {
    let numbers = list_numbers(call)?;
    let mut total = Number::Integer(start);
    for number in numbers {
        total = match (total, number) {
            (Number::Integer(left), Number::Integer(right)) => {
                match integer(left, right).filter(|value| *value != NA_INTEGER) {
                    Some(value) => Number::Integer(value),
                    None => {
                        return Err(call.error(
                            "the integer result overflows; convert the elements to doubles first",
                        ));
                    }
                }
            }
            (left, right) => Number::Double(double(left.as_double(), right.as_double())),
        };
    }
    number_value(total)
}

unsafe fn list_sum(call: &Call<'_>) -> R<Sexp> {
    fold_numbers(call, 0, i32::checked_add, |left, right| left + right)
}

unsafe fn list_product(call: &Call<'_>) -> R<Sexp> {
    fold_numbers(call, 1, i32::checked_mul, |left, right| left * right)
}

unsafe fn extreme(call: &Call<'_>, wanted: Ordering) -> R<Sexp> {
    let numbers = list_numbers(call)?;
    let Some(mut best) = numbers.first().copied() else {
        return err("ListWasEmpty");
    };
    for &number in &numbers[1..] {
        if number.as_double().partial_cmp(&best.as_double()) == Some(wanted) {
            best = number;
        }
    }
    ok(number_value(best)?)
}

unsafe fn list_min(call: &Call<'_>) -> R<Sexp> {
    extreme(call, Ordering::Less)
}

unsafe fn list_max(call: &Call<'_>) -> R<Sexp> {
    extreme(call, Ordering::Greater)
}

unsafe fn affix(call: &Call<'_>, parameter: &str, at_start: bool) -> R<Sexp> {
    let [list, part] = arguments(call, ["list", parameter])?;
    let items = list_elements(call, "list", list)?;
    let part = list_elements(call, parameter, part)?;
    if part.len() > items.len() {
        return scalar_bool(false);
    }
    let window = if at_start {
        &items[..part.len()]
    } else {
        &items[items.len() - part.len()..]
    };
    let flags = IdenticalFlags::default();
    scalar_bool(
        window
            .iter()
            .zip(&part)
            .all(|(&left, &right)| identical(left, right, &flags)),
    )
}

unsafe fn list_starts_with(call: &Call<'_>) -> R<Sexp> {
    affix(call, "prefix", true)
}

unsafe fn list_ends_with(call: &Call<'_>) -> R<Sexp> {
    affix(call, "suffix", false)
}

unsafe fn sort_numbers(call: &Call<'_>, descending: bool) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    let mut numbers = list_numbers(call)?;
    // `number` refuses `NaN`, so the comparison is total.
    numbers.sort_by(|left, right| {
        let order = left
            .as_double()
            .partial_cmp(&right.as_double())
            .unwrap_or(Ordering::Equal);
        if descending { order.reverse() } else { order }
    });
    let mut items = Vec::with_capacity(numbers.len());
    for number in numbers {
        items.push(number_value(number)?);
    }
    sequence(call, list, &items)
}

unsafe fn list_sort_asc(call: &Call<'_>) -> R<Sexp> {
    sort_numbers(call, false)
}

unsafe fn list_sort_desc(call: &Call<'_>) -> R<Sexp> {
    sort_numbers(call, true)
}

unsafe fn list_swap(call: &Call<'_>) -> R<Sexp> {
    let [list, first, second] = arguments(call, ["list", "first", "second"])?;
    let first = whole(call, "first", first)?;
    let second = whole(call, "second", second)?;
    if owned_list(call, list) {
        let count = length(list);
        if let (Some(left), Some(right)) = (position(first, count), position(second, count)) {
            let (left_value, right_value) = (list.element(left), list.element(right));
            let heap = crate::heap();
            heap.set_element(list, left, right_value);
            heap.set_element(list, right, left_value);
        }
        return Ok(list);
    }
    let mut items = list_elements(call, "list", list)?;
    if let (Some(left), Some(right)) = (position(first, items.len()), position(second, items.len()))
    {
        items.swap(left, right);
    }
    sequence(call, list, &items)
}

unsafe fn list_replace(call: &Call<'_>) -> R<Sexp> {
    let [list, index, value] = arguments(call, ["list", "index", "value"])?;
    let mut items = list_elements(call, "list", list)?;
    let index = whole(call, "index", index)?;
    let previous = match position(index, items.len()) {
        Some(at) => std::mem::replace(&mut items[at], value),
        None => value,
    };
    let replaced = sequence(call, list, &items)?;
    record(&[("list", replaced), ("value", previous)])
}

/// Splits the list around its first (or, `last`, its final) element identical to `delimiter`.
unsafe fn split_around(call: &Call<'_>, last: bool) -> R<Sexp> {
    let [list, delimiter] = arguments(call, ["list", "delimiter"])?;
    let items = list_elements(call, "list", list)?;
    let flags = IdenticalFlags::default();
    let matches = |item: &Sexp| identical(*item, delimiter, &flags);
    let found = if last {
        items.iter().rposition(matches)
    } else {
        items.iter().position(matches)
    };
    match found {
        Some(at) => {
            let before = sequence(call, list, &items[..at])?;
            let after = sequence(call, list, &items[at + 1..])?;
            ok(record(&[("before", before), ("after", after)])?)
        }
        None => err("NotFound"),
    }
}

unsafe fn list_split_first(call: &Call<'_>) -> R<Sexp> {
    split_around(call, false)
}

unsafe fn list_split_last(call: &Call<'_>) -> R<Sexp> {
    split_around(call, true)
}

/// The pieces of `items` between occurrences of the non-empty `delimiter` run.
unsafe fn split_pieces(call: &Call<'_>, like: Sexp, items: &[Sexp], delimiter: &[Sexp]) -> R<Sexp> {
    let flags = IdenticalFlags::default();
    let mut pieces = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at + delimiter.len() <= items.len() {
        let window = &items[at..at + delimiter.len()];
        if window
            .iter()
            .zip(delimiter)
            .all(|(&left, &right)| identical(left, right, &flags))
        {
            pieces.push(sequence(call, like, &items[start..at])?);
            at += delimiter.len();
            start = at;
        } else {
            at += 1;
        }
    }
    pieces.push(sequence(call, like, &items[start..])?);
    sequence(call, like, &pieces)
}

unsafe fn list_split_on(call: &Call<'_>) -> R<Sexp> {
    let [list, delimiter] = arguments(call, ["list", "delimiter"])?;
    let items = list_elements(call, "list", list)?;
    split_pieces(call, list, &items, &[delimiter])
}

unsafe fn list_split_on_list(call: &Call<'_>) -> R<Sexp> {
    let [list, delimiter] = arguments(call, ["list", "delimiter"])?;
    let items = list_elements(call, "list", list)?;
    let delimiter = list_elements(call, "delimiter", delimiter)?;
    if delimiter.is_empty() {
        return sequence(call, list, &[sequence(call, list, &items)?]);
    }
    split_pieces(call, list, &items, &delimiter)
}

unsafe fn list_append_if_ok(call: &Call<'_>) -> R<Sexp> {
    let [list, result] = arguments(call, ["list", "result"])?;
    let (is_ok, value) = result_parts(call, "result", result)?;
    if owned_list(call, list) {
        return if is_ok {
            extend_in_place(list, &[value])
        } else {
            Ok(list)
        };
    }
    let mut items = list_elements(call, "list", list)?;
    if is_ok {
        items.push(value);
    }
    sequence(call, list, &items)
}

unsafe fn list_prepend_if_ok(call: &Call<'_>) -> R<Sexp> {
    let [list, result] = arguments(call, ["list", "result"])?;
    let (is_ok, value) = result_parts(call, "result", result)?;
    let mut items = list_elements(call, "list", list)?;
    if is_ok {
        items.insert(0, value);
    }
    sequence(call, list, &items)
}

unsafe fn list_concat_utf8(call: &Call<'_>) -> R<Sexp> {
    let [list, string] = arguments(call, ["list", "string"])?;
    let string = text(call, "string", string)?.into_owned();
    let mut bytes = Vec::with_capacity(string.len());
    for &byte in string.as_bytes() {
        bytes.push(scalar_integer(i32::from(byte))?);
    }
    if owned_list(call, list) {
        return extend_in_place(list, &bytes);
    }
    let mut items = list_elements(call, "list", list)?;
    items.extend(bytes);
    sequence(call, list, &items)
}

unsafe fn list_with_capacity(call: &Call<'_>) -> R<Sexp> {
    let [capacity] = arguments(call, ["capacity"])?;
    let capacity = count(call, "capacity", capacity)?;
    resizable(SexpType::List, 0, capacity)
}

unsafe fn list_reserve(call: &Call<'_>) -> R<Sexp> {
    let [list, spare] = arguments(call, ["list", "spare"])?;
    let spare = count(call, "spare", spare)?;
    // An Array is never grown in place, so reserving room in one has nothing to do.
    if list.is_array() {
        return Ok(list);
    }
    let items = list_elements(call, "list", list)?;
    let needed = items.len().saturating_add(spare);
    if owned_list(call, list)
        && crate::heap().is_resizable(list)
        && needed <= list.capacity() as usize
    {
        return Ok(list);
    }
    let grown = resizable(SexpType::List, items.len(), needed)?;
    for (at, &item) in items.iter().enumerate() {
        crate::heap().set_element(grown, at, item);
    }
    Ok(grown)
}

// Str.

unsafe fn one_string(call: &Call<'_>) -> R<String> {
    let [string] = arguments(call, ["string"])?;
    Ok(text(call, "string", string)?.into_owned())
}

/// The string and one more string argument named `other`.
unsafe fn two_strings(call: &Call<'_>, other: &str) -> R<(String, String)> {
    let [string, second] = arguments(call, ["string", other])?;
    Ok((
        text(call, "string", string)?.into_owned(),
        text(call, other, second)?.into_owned(),
    ))
}

unsafe fn str_is_empty(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(one_string(call)?.is_empty())
}

unsafe fn str_concat(call: &Call<'_>) -> R<Sexp> {
    let [first, second] = arguments(call, ["first", "second"])?;
    let mut joined = text(call, "first", first)?.into_owned();
    joined.push_str(&text(call, "second", second)?);
    scalar_string(&joined)
}

unsafe fn str_join_with(call: &Call<'_>) -> R<Sexp> {
    let [strings, separator] = arguments(call, ["strings", "separator"])?;
    let separator = text(call, "separator", separator)?;
    let mut parts = Vec::new();
    for item in list_elements(call, "strings", strings)? {
        parts.push(text(call, "strings", item)?.into_owned());
    }
    scalar_string(&parts.join(&separator))
}

unsafe fn str_split_on(call: &Call<'_>) -> R<Sexp> {
    let (string, separator) = two_strings(call, "separator")?;
    let pieces: Vec<&str> = if separator.is_empty() {
        vec![string.as_str()]
    } else {
        string.split(separator.as_str()).collect()
    };
    let mut items = Vec::with_capacity(pieces.len());
    for piece in pieces {
        items.push(scalar_string(piece)?);
    }
    list_vector(&items)
}

unsafe fn str_repeat(call: &Call<'_>) -> R<Sexp> {
    let [string, times] = arguments(call, ["string", "count"])?;
    let string = text(call, "string", string)?;
    let times = count(call, "count", times)?;
    scalar_string(&string.repeat(times))
}

unsafe fn str_starts_with(call: &Call<'_>) -> R<Sexp> {
    let (string, prefix) = two_strings(call, "prefix")?;
    scalar_bool(string.starts_with(&prefix))
}

unsafe fn str_ends_with(call: &Call<'_>) -> R<Sexp> {
    let (string, suffix) = two_strings(call, "suffix")?;
    scalar_bool(string.ends_with(&suffix))
}

unsafe fn str_contains(call: &Call<'_>) -> R<Sexp> {
    let (string, part) = two_strings(call, "part")?;
    scalar_bool(string.contains(&part))
}

unsafe fn str_trim(call: &Call<'_>) -> R<Sexp> {
    scalar_string(one_string(call)?.trim())
}

unsafe fn str_trim_start(call: &Call<'_>) -> R<Sexp> {
    scalar_string(one_string(call)?.trim_start())
}

unsafe fn str_trim_end(call: &Call<'_>) -> R<Sexp> {
    scalar_string(one_string(call)?.trim_end())
}

unsafe fn str_drop_prefix(call: &Call<'_>) -> R<Sexp> {
    let (string, prefix) = two_strings(call, "prefix")?;
    scalar_string(string.strip_prefix(&prefix).unwrap_or(&string))
}

unsafe fn str_drop_suffix(call: &Call<'_>) -> R<Sexp> {
    let (string, suffix) = two_strings(call, "suffix")?;
    scalar_string(string.strip_suffix(&suffix).unwrap_or(&string))
}

unsafe fn str_with_prefix(call: &Call<'_>) -> R<Sexp> {
    let (string, prefix) = two_strings(call, "prefix")?;
    scalar_string(&format!("{prefix}{string}"))
}

/// The string, the literal pattern and the replacement.
unsafe fn replacement_arguments(call: &Call<'_>) -> R<(String, String, String)> {
    let [string, pattern, replacement] = arguments(call, ["string", "pattern", "replacement"])?;
    Ok((
        text(call, "string", string)?.into_owned(),
        text(call, "pattern", pattern)?.into_owned(),
        text(call, "replacement", replacement)?.into_owned(),
    ))
}

unsafe fn str_replace_each(call: &Call<'_>) -> R<Sexp> {
    let (string, pattern, replacement) = replacement_arguments(call)?;
    if pattern.is_empty() {
        return scalar_string(&string);
    }
    scalar_string(&string.replace(&pattern, &replacement))
}

unsafe fn str_replace_first(call: &Call<'_>) -> R<Sexp> {
    let (string, pattern, replacement) = replacement_arguments(call)?;
    match string.find(&pattern).filter(|_| !pattern.is_empty()) {
        Some(at) => scalar_string(&format!(
            "{}{replacement}{}",
            &string[..at],
            &string[at + pattern.len()..]
        )),
        None => scalar_string(&string),
    }
}

unsafe fn str_replace_last(call: &Call<'_>) -> R<Sexp> {
    let (string, pattern, replacement) = replacement_arguments(call)?;
    match string.rfind(&pattern).filter(|_| !pattern.is_empty()) {
        Some(at) => scalar_string(&format!(
            "{}{replacement}{}",
            &string[..at],
            &string[at + pattern.len()..]
        )),
        None => scalar_string(&string),
    }
}

unsafe fn split_once(call: &Call<'_>, last: bool) -> R<Sexp> {
    let (string, separator) = two_strings(call, "separator")?;
    let found = if last {
        string.rfind(&separator)
    } else {
        string.find(&separator)
    };
    match found {
        Some(at) => {
            let before = scalar_string(&string[..at])?;
            let after = scalar_string(&string[at + separator.len()..])?;
            ok(record(&[("before", before), ("after", after)])?)
        }
        None => err("NotFound"),
    }
}

unsafe fn str_split_first(call: &Call<'_>) -> R<Sexp> {
    split_once(call, false)
}

unsafe fn str_split_last(call: &Call<'_>) -> R<Sexp> {
    split_once(call, true)
}

unsafe fn str_count_utf8_bytes(call: &Call<'_>) -> R<Sexp> {
    scalar_integer(one_string(call)?.len() as i32)
}

unsafe fn str_to_utf8(call: &Call<'_>) -> R<Sexp> {
    let string = one_string(call)?;
    let mut bytes = Vec::with_capacity(string.len());
    for &byte in string.as_bytes() {
        bytes.push(scalar_integer(i32::from(byte))?);
    }
    list_vector(&bytes)
}

unsafe fn str_from_utf8(call: &Call<'_>) -> R<Sexp> {
    let [bytes] = arguments(call, ["bytes"])?;
    let decoded = utf8_bytes(call, bytes)?;
    // R strings cannot hold a NUL byte, so one is reported as bad UTF-8 like any other byte a
    // string cannot carry.
    match String::from_utf8(decoded) {
        Ok(string) if !string.contains('\0') => ok(scalar_string(&string)?),
        _ => err("BadUtf8"),
    }
}

/// The bytes of a list of numbers from 0 to 255.
unsafe fn utf8_bytes(call: &Call<'_>, bytes: Sexp) -> R<Vec<u8>> {
    let mut decoded = Vec::new();
    for item in list_elements(call, "bytes", bytes)? {
        let byte = whole(call, "bytes", item)?;
        match u8::try_from(byte) {
            Ok(byte) => decoded.push(byte),
            Err(_) => {
                return Err(wrong(
                    call,
                    "bytes",
                    "a list of numbers from 0 to 255",
                    item,
                ));
            }
        }
    }
    Ok(decoded)
}

unsafe fn str_to_integer(call: &Call<'_>) -> R<Sexp> {
    match one_string(call)?.parse::<i32>() {
        Ok(value) if value != NA_INTEGER => ok(scalar_integer(value)?),
        _ => err("InvalidNumStr"),
    }
}

unsafe fn str_to_double(call: &Call<'_>) -> R<Sexp> {
    match one_string(call)?.parse::<f64>() {
        Ok(value) if !value.is_nan() => ok(scalar_real(value)?),
        _ => err("InvalidNumStr"),
    }
}

unsafe fn str_with_ascii_lowercased(call: &Call<'_>) -> R<Sexp> {
    scalar_string(&one_string(call)?.to_ascii_lowercase())
}

unsafe fn str_with_ascii_uppercased(call: &Call<'_>) -> R<Sexp> {
    scalar_string(&one_string(call)?.to_ascii_uppercase())
}

unsafe fn str_caseless_ascii_equals(call: &Call<'_>) -> R<Sexp> {
    let [first, second] = arguments(call, ["first", "second"])?;
    let first = text(call, "first", first)?;
    let second = text(call, "second", second)?;
    scalar_bool(first.eq_ignore_ascii_case(&second))
}

/// Folds `f` over the string's UTF-8 bytes, passing each byte's 1-based position too when
/// `with_index`.
unsafe fn walk_bytes(call: &Call<'_>, with_index: bool) -> R<Sexp> {
    let [string, state, f] = arguments(call, ["string", "state", "f"])?;
    let string = text(call, "string", string)?.into_owned();
    let f = function(call, "f", f)?;
    let mut state = state;
    for (at, &byte) in string.as_bytes().iter().enumerate() {
        state = scope(|| {
            let byte = scalar_integer(i32::from(byte))?;
            if with_index {
                let index = scalar_integer(at as i32 + 1)?;
                invoke(call, f, &[state, byte, index])
            } else {
                invoke(call, f, &[state, byte])
            }
        })?;
    }
    Ok(state)
}

unsafe fn str_walk_utf8(call: &Call<'_>) -> R<Sexp> {
    walk_bytes(call, false)
}

unsafe fn str_walk_utf8_with_index(call: &Call<'_>) -> R<Sexp> {
    walk_bytes(call, true)
}

unsafe fn str_from_utf8_lossy(call: &Call<'_>) -> R<Sexp> {
    let [bytes] = arguments(call, ["bytes"])?;
    let decoded = utf8_bytes(call, bytes)?;
    // R strings cannot hold a NUL byte, so it is replaced like a byte that is not UTF-8.
    let string = String::from_utf8_lossy(&decoded).replace('\0', "\u{FFFD}");
    scalar_string(&string)
}

// Num.

unsafe fn one_number(call: &Call<'_>) -> R<Number> {
    let [value] = arguments(call, ["number"])?;
    number(call, "number", value)
}

/// Two numbers, both integers or (if either is a double) both doubles.
unsafe fn two_numbers(call: &Call<'_>) -> R<(Number, Number)> {
    let [first, second] = arguments(call, ["first", "second"])?;
    let first = number(call, "first", first)?;
    let second = number(call, "second", second)?;
    Ok(match (first, second) {
        (Number::Integer(_), Number::Integer(_)) => (first, second),
        _ => (
            Number::Double(first.as_double()),
            Number::Double(second.as_double()),
        ),
    })
}

unsafe fn num_to_str(call: &Call<'_>) -> R<Sexp> {
    match one_number(call)? {
        Number::Integer(value) => scalar_string(&value.to_string()),
        Number::Double(value) => scalar_string(&crate::format::format_real_alone(value, 15)),
    }
}

unsafe fn num_compare(call: &Call<'_>) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    let order = first
        .as_double()
        .partial_cmp(&second.as_double())
        .unwrap_or(Ordering::Equal);
    ordering_tag(order)
}

unsafe fn num_min(call: &Call<'_>) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    number_value(if second.as_double() < first.as_double() {
        second
    } else {
        first
    })
}

unsafe fn num_max(call: &Call<'_>) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    number_value(if second.as_double() > first.as_double() {
        second
    } else {
        first
    })
}

unsafe fn num_is_zero(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(one_number(call)?.as_double() == 0.0)
}

unsafe fn num_is_positive(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(one_number(call)?.as_double() > 0.0)
}

unsafe fn num_is_negative(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(one_number(call)?.as_double() < 0.0)
}

/// Whether the number is whole and its remainder by two is `remainder`.
unsafe fn parity(call: &Call<'_>, remainder: f64) -> R<Sexp> {
    let value = one_number(call)?.as_double();
    scalar_bool(value.is_finite() && value.fract() == 0.0 && (value % 2.0).abs() == remainder)
}

unsafe fn num_is_even(call: &Call<'_>) -> R<Sexp> {
    parity(call, 0.0)
}

unsafe fn num_is_odd(call: &Call<'_>) -> R<Sexp> {
    parity(call, 1.0)
}

/// `:Ok(result)` of an arithmetic operation, or `:Err(:Overflow)` when an integer result does
/// not fit or a double result of finite operands is infinite.
unsafe fn checked(
    call: &Call<'_>,
    integer: fn(i32, i32) -> Option<i32>,
    double: fn(f64, f64) -> f64,
) -> R<Sexp> {
    match two_numbers(call)? {
        (Number::Integer(first), Number::Integer(second)) => {
            match integer(first, second).filter(|value| *value != NA_INTEGER) {
                Some(value) => ok(scalar_integer(value)?),
                None => err("Overflow"),
            }
        }
        (first, second) => {
            let (first, second) = (first.as_double(), second.as_double());
            let value = double(first, second);
            if value.is_infinite() && first.is_finite() && second.is_finite() {
                err("Overflow")
            } else {
                ok(scalar_real(value)?)
            }
        }
    }
}

unsafe fn num_add_checked(call: &Call<'_>) -> R<Sexp> {
    checked(call, i32::checked_add, |left, right| left + right)
}

unsafe fn num_sub_checked(call: &Call<'_>) -> R<Sexp> {
    checked(call, i32::checked_sub, |left, right| left - right)
}

unsafe fn num_mul_checked(call: &Call<'_>) -> R<Sexp> {
    checked(call, i32::checked_mul, |left, right| left * right)
}

unsafe fn num_div_checked(call: &Call<'_>) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    if second.as_double() == 0.0 {
        return err("DivByZero");
    }
    ok(scalar_real(first.as_double() / second.as_double())?)
}

/// Division by a nonzero divisor, which keeps the operands' number type.
unsafe fn checked_division(
    call: &Call<'_>,
    integer: fn(i32, i32) -> Option<i32>,
    double: fn(f64, f64) -> f64,
) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    if second.as_double() == 0.0 {
        return err("DivByZero");
    }
    match (first, second) {
        (Number::Integer(first), Number::Integer(second)) => match integer(first, second) {
            Some(value) => ok(scalar_integer(value)?),
            None => err("Overflow"),
        },
        (first, second) => ok(scalar_real(double(first.as_double(), second.as_double()))?),
    }
}

unsafe fn num_div_trunc_checked(call: &Call<'_>) -> R<Sexp> {
    checked_division(call, i32::checked_div, |left, right| (left / right).trunc())
}

unsafe fn num_rem_checked(call: &Call<'_>) -> R<Sexp> {
    checked_division(call, i32::checked_rem, |left, right| left % right)
}

unsafe fn num_sqrt_checked(call: &Call<'_>) -> R<Sexp> {
    let value = one_number(call)?.as_double();
    if value < 0.0 {
        return err("SqrtOfNegative");
    }
    ok(scalar_real(value.sqrt())?)
}

unsafe fn num_log_checked(call: &Call<'_>) -> R<Sexp> {
    let value = one_number(call)?.as_double();
    if value <= 0.0 {
        return err("LogNeedsPositive");
    }
    ok(scalar_real(value.ln())?)
}

/// Compares two numbers with `holds`.
unsafe fn comparison(call: &Call<'_>, holds: fn(f64, f64) -> bool) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    scalar_bool(holds(first.as_double(), second.as_double()))
}

unsafe fn num_is_lt(call: &Call<'_>) -> R<Sexp> {
    comparison(call, |first, second| first < second)
}

unsafe fn num_is_gt(call: &Call<'_>) -> R<Sexp> {
    comparison(call, |first, second| first > second)
}

unsafe fn num_is_lte(call: &Call<'_>) -> R<Sexp> {
    comparison(call, |first, second| first <= second)
}

unsafe fn num_is_gte(call: &Call<'_>) -> R<Sexp> {
    comparison(call, |first, second| first >= second)
}

unsafe fn num_is_approx_eq(call: &Call<'_>) -> R<Sexp> {
    let formals = ["first", "second", "rtol", "atol"];
    let (matched, _) = call.match_args(&formals)?;
    let supplied = |at: usize| {
        matched
            .get(at)
            .copied()
            .flatten()
            .filter(|&value| value != missing_arg())
    };
    let mut numbers = [0.0, 0.0, 0.00001, 0.00000001];
    for (at, formal) in formals.iter().enumerate() {
        match supplied(at) {
            Some(value) => numbers[at] = number(call, formal, value)?.as_double(),
            None if at < 2 => {
                return Err(call.error(format!(
                    "`Num.is_approx_eq()` needs the argument `{formal}`"
                )));
            }
            None => {}
        }
    }
    let [first, second, rtol, atol] = numbers;
    let tolerance = atol.max(rtol * first.abs().max(second.abs()));
    scalar_bool(first == second || (first - second).abs() <= tolerance)
}

unsafe fn num_is_multiple_of(call: &Call<'_>) -> R<Sexp> {
    let (first, second) = two_numbers(call)?;
    let (first, second) = (first.as_double(), second.as_double());
    scalar_bool(if second == 0.0 {
        first == 0.0
    } else {
        first.is_finite() && first % second == 0.0
    })
}

unsafe fn num_is_finite(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(one_number(call)?.as_double().is_finite())
}

unsafe fn num_is_infinite(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(one_number(call)?.as_double().is_infinite())
}

/// Applies an operation that keeps the number type. An integer cannot overflow here, because
/// `NA` takes the one value, `i32::MIN`, whose negation does not fit.
unsafe fn unary(call: &Call<'_>, integer: fn(i32) -> i32, double: fn(f64) -> f64) -> R<Sexp> {
    match one_number(call)? {
        Number::Integer(value) => scalar_integer(integer(value)),
        Number::Double(value) => scalar_real(double(value)),
    }
}

unsafe fn num_abs(call: &Call<'_>) -> R<Sexp> {
    unary(call, i32::abs, f64::abs)
}

unsafe fn num_neg(call: &Call<'_>) -> R<Sexp> {
    unary(call, |value| -value, |value| -value)
}

unsafe fn num_ceiling(call: &Call<'_>) -> R<Sexp> {
    unary(call, |value| value, f64::ceil)
}

unsafe fn num_floor(call: &Call<'_>) -> R<Sexp> {
    unary(call, |value| value, f64::floor)
}

unsafe fn num_round(call: &Call<'_>) -> R<Sexp> {
    unary(call, |value| value, f64::round)
}

unsafe fn num_abs_diff(call: &Call<'_>) -> R<Sexp> {
    match two_numbers(call)? {
        (Number::Integer(first), Number::Integer(second)) => {
            match i32::try_from((i64::from(first) - i64::from(second)).abs()) {
                Ok(value) => scalar_integer(value),
                Err(_) => Err(call
                    .error("the integer result overflows; convert the numbers to doubles first")),
            }
        }
        (first, second) => scalar_real((first.as_double() - second.as_double()).abs()),
    }
}

/// A function of one number whose result is a double.
unsafe fn real_function(call: &Call<'_>, function: fn(f64) -> f64) -> R<Sexp> {
    scalar_real(function(one_number(call)?.as_double()))
}

unsafe fn num_pow(call: &Call<'_>) -> R<Sexp> {
    let [base, exponent] = arguments(call, ["base", "exponent"])?;
    let base = number(call, "base", base)?.as_double();
    let exponent = number(call, "exponent", exponent)?.as_double();
    scalar_real(base.powf(exponent))
}

unsafe fn num_sin(call: &Call<'_>) -> R<Sexp> {
    real_function(call, f64::sin)
}

unsafe fn num_cos(call: &Call<'_>) -> R<Sexp> {
    real_function(call, f64::cos)
}

unsafe fn num_tan(call: &Call<'_>) -> R<Sexp> {
    real_function(call, f64::tan)
}

unsafe fn num_atan(call: &Call<'_>) -> R<Sexp> {
    real_function(call, f64::atan)
}

/// An operation whose integer result is clamped to the integers R can hold, `-2147483647` to
/// `2147483647`, and whose double result is the plain one.
unsafe fn saturated(
    call: &Call<'_>,
    integer: fn(i64, i64) -> i64,
    double: fn(f64, f64) -> f64,
) -> R<Sexp> {
    match two_numbers(call)? {
        (Number::Integer(first), Number::Integer(second)) => {
            let value = integer(i64::from(first), i64::from(second));
            let limit = i64::from(i32::MAX);
            scalar_integer(value.clamp(-limit, limit) as i32)
        }
        (first, second) => scalar_real(double(first.as_double(), second.as_double())),
    }
}

unsafe fn num_add_saturated(call: &Call<'_>) -> R<Sexp> {
    saturated(call, |left, right| left + right, |left, right| left + right)
}

unsafe fn num_sub_saturated(call: &Call<'_>) -> R<Sexp> {
    saturated(call, |left, right| left - right, |left, right| left - right)
}

unsafe fn num_mul_saturated(call: &Call<'_>) -> R<Sexp> {
    saturated(call, |left, right| left * right, |left, right| left * right)
}

unsafe fn num_div_ceil_checked(call: &Call<'_>) -> R<Sexp> {
    checked_division(
        call,
        |left, right| {
            let quotient = left.checked_div(right)?;
            let rounds_up = left % right != 0 && (left < 0) == (right < 0);
            Some(if rounds_up { quotient + 1 } else { quotient })
        },
        |left, right| (left / right).ceil(),
    )
}

unsafe fn num_to_double(call: &Call<'_>) -> R<Sexp> {
    scalar_real(one_number(call)?.as_double())
}

unsafe fn num_to_integer_checked(call: &Call<'_>) -> R<Sexp> {
    match one_number(call)? {
        Number::Integer(value) => ok(scalar_integer(value)?),
        Number::Double(value) => {
            let truncated = value.trunc();
            if truncated.is_finite() && truncated.abs() <= f64::from(i32::MAX) {
                ok(scalar_integer(truncated as i32)?)
            } else {
                err("OutOfBounds")
            }
        }
    }
}

// In-place updates.

/// Whether the call may update `value` in place: nothing else references it, because it is a
/// temporary or because its only holder is the binding the result is assigned back to. This is
/// reference counting's version of Roc's uniqueness check.
unsafe fn owned(call: &Call<'_>, value: Sexp) -> bool {
    !value.is_altrep()
        && (value.refcount() == 0 || (call.reusable == Some(value) && value.refcount() == 1))
}

/// Whether `list` is a list the call owns. A list with attributes is copied instead, because the
/// copy drops its names (see [`list_elements`]).
unsafe fn owned_list(call: &Call<'_>, list: Sexp) -> bool {
    // An Array of ry values is a list too, but an update must check its items, so it copies.
    list.kind() == SexpType::List && !list.is_array() && list.attrib() == nil() && owned(call, list)
}

/// Appends `items` to an owned list, within its capacity when it has room and otherwise in a
/// resizable copy with twice the room, so repeated appends cost amortized constant time.
unsafe fn extend_in_place(list: Sexp, items: &[Sexp]) -> R<Sexp> {
    let heap = crate::heap();
    let count = length(list);
    let needed = count + items.len();
    let list = if heap.is_resizable(list) && needed <= list.capacity() as usize {
        list
    } else {
        with_capacity(list, needed.max(count * 2).max(4))?
    };
    heap.resize_vector(list, needed as isize)?;
    for (offset, &item) in items.iter().enumerate() {
        heap.set_element(list, count + offset, item);
    }
    Ok(list)
}

/// A fresh resizable vector of `kind` with room for `capacity` elements and length `count`.
unsafe fn resizable(kind: SexpType, count: usize, capacity: usize) -> R<Sexp> {
    let heap = crate::heap();
    let vector = heap.alloc_resizable_vector(kind, capacity.max(count) as isize)?;
    heap.root(vector);
    heap.resize_vector(vector, count as isize)?;
    Ok(vector)
}

/// A resizable copy of the atomic vector or list `vector` with room for `capacity` elements.
unsafe fn with_capacity(vector: Sexp, capacity: usize) -> R<Sexp> {
    let count = length(vector);
    let grown = resizable(vector.kind(), count, capacity)?;
    match vector.kind() {
        SexpType::Real => grown
            .slice_mut::<f64>()
            .copy_from_slice(values::reals(vector)),
        SexpType::Integer | SexpType::Logical => grown
            .slice_mut::<i32>()
            .copy_from_slice(values::ints(vector)),
        _ => {
            for index in 0..count {
                crate::heap().set_element(grown, index, vector.element(index));
            }
        }
    }
    Ok(grown)
}

/// Whether the call owns the dictionary or set `object` and every vector inside it, which only
/// the collection holds, so [`insert_in_place`] may grow them. An empty collection has no key
/// vector yet and is rebuilt instead.
unsafe fn owned_collection(call: &Call<'_>, object: Sexp) -> bool {
    if !owned(call, object) {
        return false;
    }
    let heap = crate::heap();
    let kind = records::native_kind(object);
    let index = index_slot(kind);
    (0..object.record_slot_count()).all(|slot| match object.record_slot(slot) {
        Slot::Handle(part) => {
            part != nil() && part.refcount() <= 1 && (slot == index || heap.is_resizable(part))
        }
        _ => false,
    })
}

/// Whether the key vector of a collection holds keys of `kind` without changing its type: an
/// integer joins double keys as a double, but a double turns integer keys into doubles, which
/// [`Collection::insert`] does by rebuilding.
unsafe fn keys_hold(object: Sexp, kind: KeyKind) -> bool {
    let keys = object.record_handle(0);
    match (keys.and_then(|keys| key_kind_of(keys)), kind) {
        (Some(KeyKind::Double), KeyKind::Integer) => true,
        (Some(stored), kind) => stored == kind,
        (None, _) => false,
    }
}

/// Inserts `key` (with `value` for a dictionary) into a collection the call owns, which
/// [`owned_collection`] and [`keys_hold`] confirmed. The key and value vectors double their
/// capacity when full, and the index is rebuilt at half load, as [`Collection`] does.
unsafe fn insert_in_place(object: Sexp, key: Key, value: Sexp) -> R<()> {
    let heap = crate::heap();
    let kind = records::native_kind(object);
    let is_dict = kind == NativeKind::Dict;
    let index_at = index_slot(kind);
    let (Some(mut keys), Some(index)) = (object.record_handle(0), object.record_handle(index_at))
    else {
        return Err(error("a dictionary or set lost its key vector"));
    };
    let slot = match probe(values::ints(index), key, |position| key_at(keys, position)) {
        Ok(position) => {
            if is_dict && let Some(stored) = object.record_handle(1) {
                heap.set_element(stored, position, value);
            }
            return Ok(());
        }
        Err(slot) => slot,
    };
    let count = length(keys);
    if count == keys.capacity() as usize {
        let capacity = (count * 2).max(8);
        keys = with_capacity(keys, capacity)?;
        heap.set_record_slot(object, 0, keys);
        if let Some(stored) = object.record_handle(1).filter(|_| is_dict) {
            let grown = with_capacity(stored, capacity)?;
            heap.set_record_slot(object, 1, grown);
        }
    }
    heap.resize_vector(keys, count as isize + 1)?;
    match (keys.kind(), key) {
        (SexpType::Logical, Key::Logical(flag)) => keys.slice_mut::<i32>()[count] = i32::from(flag),
        (SexpType::Integer, Key::Number(number)) => keys.slice_mut::<i32>()[count] = number as i32,
        (SexpType::Real, Key::Number(number)) => keys.slice_mut::<f64>()[count] = number,
        (SexpType::String, Key::Text(text)) => heap.set_element(keys, count, text),
        _ => return Err(error("a key does not fit its collection's key vector")),
    }
    if let Some(stored) = object.record_handle(1).filter(|_| is_dict) {
        heap.resize_vector(stored, count as isize + 1)?;
        heap.set_element(stored, count, value);
    }
    let entries = count + 1;
    if entries * 2 > length(index) {
        let rebuilt = build_index(entries, entries, |position| key_at(keys, position))?;
        heap.set_record_slot(object, index_at, values::integer_vector(&rebuilt)?);
    } else {
        index.slice_mut::<i32>()[slot] = entries as i32;
    }
    Ok(())
}

/// The open-addressing index over `count` keys, with room for `room` keys: a power of two at
/// least twice that, so it is never more than half full, whose entries are a key's position plus
/// one.
unsafe fn build_index(
    count: usize,
    room: usize,
    key_at_position: impl Fn(usize) -> Key,
) -> R<Vec<i32>> {
    let size = room
        .max(count)
        .checked_mul(2)
        .and_then(usize::checked_next_power_of_two)
        .filter(|&size| size <= i32::MAX as usize)
        .ok_or_else(|| {
            error(format!(
                "cannot index a collection with room for {room} keys"
            ))
        })?
        .max(8);
    let mut index = Vec::new();
    index
        .try_reserve_exact(size)
        .map_err(|_| error(format!("cannot allocate an index for {room} keys")))?;
    index.resize(size, 0);
    for position in 0..count {
        if let Err(slot) = probe(&index, key_at_position(position), &key_at_position) {
            index[slot] = position as i32 + 1;
        }
    }
    Ok(index)
}

/// A dictionary or set with room for `spare` more keys without growing: `object` itself when the
/// call owns it, and otherwise a copy. An empty collection has no key vector, and so nothing to
/// reserve, until its first key fixes the key type.
unsafe fn reserve_collection(call: &Call<'_>, object: Sexp, spare: usize) -> R<Sexp> {
    let count = entry_count(object);
    if count == 0 {
        return Ok(object);
    }
    let object = if owned_collection(call, object) {
        object
    } else {
        Collection::read(object)?.write()?
    };
    let heap = crate::heap();
    heap.root(object);
    let kind = records::native_kind(object);
    let room = count.saturating_add(spare);
    if room > i32::MAX as usize / 2 {
        // Positions in the index are `i32`, and the index holds twice the room.
        return Err(error(format!("cannot reserve room for {room} keys")));
    }
    let parts = if kind == NativeKind::Dict { 2 } else { 1 };
    for slot in 0..parts {
        if let Some(part) = object.record_handle(slot)
            && (part.capacity() as usize) < room
        {
            let grown = with_capacity(part, room)?;
            heap.set_record_slot(object, slot, grown);
        }
    }
    let index_at = index_slot(kind);
    let wanted = (room * 2).next_power_of_two().max(8);
    if let (Some(keys), Some(index)) = (object.record_handle(0), object.record_handle(index_at))
        && length(index) < wanted
    {
        let rebuilt = build_index(count, room, |position| key_at(keys, position))?;
        heap.set_record_slot(object, index_at, values::integer_vector(&rebuilt)?);
    }
    Ok(object)
}

// Dict and Set storage.

/// A key or set element: a logical, a number (an integer and the equal double are one key, as
/// `1L == 1` in R), or a string.
#[derive(Clone, Copy)]
enum Key {
    Logical(bool),
    Number(f64),
    Text(Sexp),
}

/// The atomic type of a collection's keys. Integer keys become double keys once a double joins
/// them, as `c(1L, 1)` does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    Logical,
    Integer,
    Double,
    Text,
}

impl KeyKind {
    fn name(self) -> &'static str {
        match self {
            KeyKind::Logical => "logical",
            KeyKind::Integer => "integer",
            KeyKind::Double => "double",
            KeyKind::Text => "character",
        }
    }
}

unsafe fn key_equal(left: Key, right: Key) -> bool {
    match (left, right) {
        (Key::Logical(left), Key::Logical(right)) => left == right,
        (Key::Number(left), Key::Number(right)) => left == right,
        (Key::Text(left), Key::Text(right)) => left == right || left.bytes() == right.bytes(),
        _ => false,
    }
}

unsafe fn key_hash(key: Key) -> u64 {
    let mut hasher = FxHasher::default();
    match key {
        Key::Logical(flag) => (0u8, flag).hash(&mut hasher),
        // `-0` and `0` are one key, so they must hash alike.
        Key::Number(number) => (1u8, (number + 0.0).to_bits()).hash(&mut hasher),
        Key::Text(text) => (2u8, text.bytes()).hash(&mut hasher),
    }
    hasher.finish()
}

/// The key a length-one value names, refusing `NA`, `NaN` and the atomic types that are not keys.
unsafe fn key_of(call: &Call<'_>, parameter: &str, value: Sexp) -> R<(Key, KeyKind)> {
    let needed = "a single logical, number or string";
    if length(value) != 1 || value.is_object() {
        return Err(wrong(call, parameter, needed, value));
    }
    let materialized = values::materialize(value)?;
    let key = match materialized.kind() {
        SexpType::Logical => match values::ints(materialized)[0] {
            NA_LOGICAL => None,
            flag => Some((Key::Logical(flag != 0), KeyKind::Logical)),
        },
        SexpType::Integer => match values::ints(materialized)[0] {
            NA_INTEGER => None,
            number => Some((Key::Number(f64::from(number)), KeyKind::Integer)),
        },
        SexpType::Real => {
            let number = values::reals(materialized)[0];
            (!number.is_nan()).then_some((Key::Number(number), KeyKind::Double))
        }
        SexpType::String => {
            let text = materialized.element(0);
            (text != values::na_string()).then_some((Key::Text(text), KeyKind::Text))
        }
        _ => None,
    };
    key.ok_or_else(|| wrong(call, parameter, needed, value))
}

/// The key at `index` of a collection's key vector.
unsafe fn key_at(keys: Sexp, index: usize) -> Key {
    match keys.kind() {
        SexpType::Logical => Key::Logical(values::ints(keys)[index] != 0),
        SexpType::Integer => Key::Number(f64::from(values::ints(keys)[index])),
        SexpType::Real => Key::Number(values::reals(keys)[index]),
        _ => Key::Text(keys.element(index)),
    }
}

unsafe fn key_kind_of(keys: Sexp) -> Option<KeyKind> {
    match keys.kind() {
        SexpType::Logical => Some(KeyKind::Logical),
        SexpType::Integer => Some(KeyKind::Integer),
        SexpType::Real => Some(KeyKind::Double),
        SexpType::String => Some(KeyKind::Text),
        _ => None,
    }
}

/// The key as an R value of the collection's key type.
unsafe fn key_value(key: Key, kind: KeyKind) -> R<Sexp> {
    match (key, kind) {
        (Key::Logical(flag), _) => scalar_bool(flag),
        (Key::Number(number), KeyKind::Integer) => scalar_integer(number as i32),
        (Key::Number(number), _) => scalar_real(number),
        (Key::Text(text), _) => values::scalar_string_charsxp(text),
    }
}

/// The slot index of the hash index in a collection's layout.
fn index_slot(kind: NativeKind) -> usize {
    if kind == NativeKind::Dict { 2 } else { 1 }
}

/// A collection being built: its keys in insertion order, a dictionary's values beside them, and
/// the open-addressing index, whose entries are a key's position plus one (zero is empty).
struct Collection {
    kind: NativeKind,
    key_kind: Option<KeyKind>,
    keys: Vec<Key>,
    values: Vec<Sexp>,
    index: Vec<i32>,
}

impl Collection {
    fn new(kind: NativeKind) -> Collection {
        Collection {
            kind,
            key_kind: None,
            keys: Vec::new(),
            values: Vec::new(),
            index: vec![0; 8],
        }
    }

    unsafe fn read(object: Sexp) -> R<Collection> {
        let kind = records::native_kind(object);
        let keys = records::slot_value(object, 0)?;
        let count = length(keys);
        let mut collection = Collection {
            kind,
            key_kind: key_kind_of(keys),
            keys: (0..count).map(|index| key_at(keys, index)).collect(),
            values: Vec::new(),
            index: values::ints(records::slot_value(object, index_slot(kind))?).to_vec(),
        };
        if kind == NativeKind::Dict {
            let stored = records::slot_value(object, 1)?;
            collection.values = (0..count).map(|index| stored.element(index)).collect();
        }
        Ok(collection)
    }

    unsafe fn find(&self, key: Key) -> Option<usize> {
        probe(&self.index, key, |position| self.keys[position]).ok()
    }

    /// Adds `key` (with `value` for a dictionary), replacing the value of an existing key.
    unsafe fn insert(&mut self, call: &Call<'_>, key: Key, kind: KeyKind, value: Sexp) -> R<()> {
        self.key_kind = Some(match self.key_kind {
            None => kind,
            Some(current) if current == kind => current,
            Some(KeyKind::Integer | KeyKind::Double)
                if matches!(kind, KeyKind::Integer | KeyKind::Double) =>
            {
                KeyKind::Double
            }
            Some(current) => {
                let what = if self.kind == NativeKind::Dict {
                    "dict's keys"
                } else {
                    "set's elements"
                };
                return Err(call.error(format!(
                    "the {what} are {}, and this one is {}; a collection holds one type",
                    current.name(),
                    kind.name()
                )));
            }
        });
        match probe(&self.index, key, |position| self.keys[position]) {
            Ok(position) => {
                if self.kind == NativeKind::Dict {
                    self.values[position] = value;
                }
            }
            Err(slot) => {
                self.keys.push(key);
                if self.kind == NativeKind::Dict {
                    self.values.push(value);
                }
                self.index[slot] = self.keys.len() as i32;
                if self.keys.len() * 2 > self.index.len() {
                    self.rebuild_index()?;
                }
            }
        }
        Ok(())
    }

    /// Keeps the entries for which `keep(position)` holds.
    unsafe fn retain(&mut self, mut keep: impl FnMut(usize) -> R<bool>) -> R<()> {
        let mut keys = Vec::with_capacity(self.keys.len());
        let mut values = Vec::with_capacity(self.values.len());
        for position in 0..self.keys.len() {
            if keep(position)? {
                keys.push(self.keys[position]);
                if self.kind == NativeKind::Dict {
                    values.push(self.values[position]);
                }
            }
        }
        // An emptied collection forgets its key type, so it is the same value as `Dict.empty()`
        // and accepts keys of any type again.
        if keys.is_empty() {
            self.key_kind = None;
        }
        self.keys = keys;
        self.values = values;
        self.rebuild_index()
    }

    unsafe fn rebuild_index(&mut self) -> R<()> {
        self.index = build_index(self.keys.len(), self.keys.len(), |position| {
            self.keys[position]
        })?;
        Ok(())
    }

    unsafe fn write(&self) -> R<Sexp> {
        let count = self.keys.len();
        let keys = match self.key_kind {
            None => nil(),
            Some(KeyKind::Logical) => {
                let vector = resizable(SexpType::Logical, count, count)?;
                for (slot, key) in vector.slice_mut::<i32>().iter_mut().zip(&self.keys) {
                    *slot = i32::from(matches!(key, Key::Logical(true)));
                }
                vector
            }
            Some(KeyKind::Integer) => {
                let vector = resizable(SexpType::Integer, count, count)?;
                for (slot, key) in vector.slice_mut::<i32>().iter_mut().zip(&self.keys) {
                    if let Key::Number(number) = key {
                        *slot = *number as i32;
                    }
                }
                vector
            }
            Some(KeyKind::Double) => {
                let vector = resizable(SexpType::Real, count, count)?;
                for (slot, key) in vector.slice_mut::<f64>().iter_mut().zip(&self.keys) {
                    if let Key::Number(number) = key {
                        *slot = *number;
                    }
                }
                vector
            }
            Some(KeyKind::Text) => {
                let vector = resizable(SexpType::String, count, count)?;
                for (position, key) in self.keys.iter().enumerate() {
                    if let Key::Text(text) = key {
                        crate::heap().set_element(vector, position, *text);
                    }
                }
                vector
            }
        };
        let index = values::integer_vector(&self.index)?;
        let (fields, slots) = if self.kind == NativeKind::Dict {
            let stored = resizable(SexpType::List, count, count)?;
            for (position, &value) in self.values.iter().enumerate() {
                crate::heap().set_element(stored, position, value);
            }
            (Fields::Dict, vec![keys, stored, index])
        } else {
            (Fields::Set, vec![keys, index])
        };
        records::alloc(Layout { tag: None, fields }, &slots)
            .map_err(|failure| error(failure.to_string()))
    }

    unsafe fn key_value(&self, position: usize) -> R<Sexp> {
        key_value(
            self.keys[position],
            self.key_kind.unwrap_or(KeyKind::Logical),
        )
    }
}

/// Probes `index` for `key`: `Ok(position)` when a stored key equals it, or `Err(slot)` with the
/// empty slot where it belongs. The index is never full, because it is rebuilt at half load.
unsafe fn probe(
    index: &[i32],
    key: Key,
    key_at_position: impl Fn(usize) -> Key,
) -> Result<usize, usize> {
    let mask = index.len() - 1;
    let mut slot = key_hash(key) as usize & mask;
    loop {
        match index[slot] {
            0 => return Err(slot),
            stored => {
                let position = stored as usize - 1;
                if key_equal(key_at_position(position), key) {
                    return Ok(position);
                }
            }
        }
        slot = (slot + 1) & mask;
    }
}

/// The position of `key` in a stored collection, looked up through its index without copying it.
unsafe fn lookup(object: Sexp, key: Key) -> R<Option<usize>> {
    let kind = records::native_kind(object);
    let keys = records::slot_value(object, 0)?;
    if keys == nil() {
        return Ok(None);
    }
    let index = records::slot_value(object, index_slot(kind))?;
    Ok(probe(values::ints(index), key, |position| key_at(keys, position)).ok())
}

unsafe fn collection(call: &Call<'_>, parameter: &str, value: Sexp, kind: NativeKind) -> R<Sexp> {
    if value.kind() == SexpType::Record && records::native_kind(value) == kind {
        return Ok(value);
    }
    let needed = if kind == NativeKind::Dict {
        "a dict"
    } else {
        "a set"
    };
    Err(wrong(call, parameter, needed, value))
}

/// The number of entries of a dictionary or set, `length()`'s answer for one.
pub(crate) unsafe fn entry_count(object: Sexp) -> usize {
    match object.record_slot(0) {
        Slot::Handle(keys) => length(keys),
        _ => 1,
    }
}

/// A dictionary's `(key, value)` pairs or a set's elements, in insertion order, as `deparse`,
/// `print` and `as.list()` show them.
pub(crate) unsafe fn entries(object: Sexp) -> R<Vec<(Sexp, Option<Sexp>)>> {
    let collection = Collection::read(object)?;
    let mut entries = Vec::with_capacity(collection.keys.len());
    for position in 0..collection.keys.len() {
        let value = collection.values.get(position).copied();
        entries.push((collection.key_value(position)?, value));
    }
    Ok(entries)
}

/// Whether two dictionaries (or two sets) hold the same entries, in whatever order: the
/// collections are value types, and insertion order is not part of their value.
pub(crate) unsafe fn collections_identical(
    left: Sexp,
    right: Sexp,
    flags: &IdenticalFlags,
) -> bool {
    let (Ok(left), Ok(right)) = (Collection::read(left), Collection::read(right)) else {
        return false;
    };
    left.kind == right.kind
        && left.key_kind == right.key_kind
        && left.keys.len() == right.keys.len()
        && left
            .keys
            .iter()
            .enumerate()
            .all(|(position, &key)| match right.find(key) {
                Some(other) => {
                    left.kind == NativeKind::Set
                        || identical(left.values[position], right.values[other], flags)
                }
                None => false,
            })
}

/// For `all.equal()`: when two dictionaries (or two sets) hold the same keys, each key's label
/// with the two values stored under it (none for a set), so the caller can compare the values
/// with tolerance; `None` when the keys differ.
pub(crate) unsafe fn paired_entries(
    left: Sexp,
    right: Sexp,
) -> R<Option<Vec<(String, Sexp, Sexp)>>> {
    let (left, right) = (Collection::read(left)?, Collection::read(right)?);
    if left.kind != right.kind
        || left.key_kind != right.key_kind
        || left.keys.len() != right.keys.len()
    {
        return Ok(None);
    }
    let mut pairs = Vec::new();
    for (position, &key) in left.keys.iter().enumerate() {
        let Some(other) = right.find(key) else {
            return Ok(None);
        };
        if left.kind == NativeKind::Dict {
            let label = match key {
                Key::Logical(flag) => {
                    if flag {
                        "TRUE".to_owned()
                    } else {
                        "FALSE".to_owned()
                    }
                }
                Key::Number(number) if left.key_kind == Some(KeyKind::Integer) => {
                    format!("{}L", number as i32)
                }
                Key::Number(number) => number.to_string(),
                Key::Text(text) => format!("{:?}", char_str(text)),
            };
            pairs.push((label, left.values[position], right.values[other]));
        }
    }
    Ok(Some(pairs))
}

/// `as.list()`: a dictionary becomes a list of `(key, value)` tuples, and a set a list of its
/// elements.
pub(crate) unsafe fn as_list(object: Sexp) -> R<Sexp> {
    let mut items = Vec::new();
    for (key, value) in entries(object)? {
        items.push(match value {
            Some(value) => records::make(None, &[(None, key), (None, value)]).map_err(error)?,
            None => key,
        });
    }
    list_vector(&items)
}

// Dict.

unsafe fn dict(call: &Call<'_>, value: Sexp) -> R<Sexp> {
    collection(call, "dict", value, NativeKind::Dict)
}

unsafe fn dict_empty(call: &Call<'_>) -> R<Sexp> {
    arguments(call, [])?;
    Collection::new(NativeKind::Dict).write()
}

unsafe fn dict_single(call: &Call<'_>) -> R<Sexp> {
    let [key, value] = arguments(call, ["key", "value"])?;
    let (key, kind) = key_of(call, "key", key)?;
    let mut collection = Collection::new(NativeKind::Dict);
    collection.insert(call, key, kind, value)?;
    collection.write()
}

/// The `(key, value)` pair a `Dict.from_list` element must be.
unsafe fn pair(call: &Call<'_>, item: Sexp) -> R<(Sexp, Sexp)> {
    if item.kind() == SexpType::Record {
        let layout = records::layout(item);
        if layout.tag.is_none() && layout.fields == Fields::Positional(2) {
            return Ok((records::slot_value(item, 0)?, records::slot_value(item, 1)?));
        }
    }
    Err(wrong(
        call,
        "pairs",
        "a list of `(key, value)` tuples",
        item,
    ))
}

unsafe fn dict_from_list(call: &Call<'_>) -> R<Sexp> {
    let [pairs] = arguments(call, ["pairs"])?;
    let mut collection = Collection::new(NativeKind::Dict);
    for item in list_elements(call, "pairs", pairs)? {
        let (key, value) = pair(call, item)?;
        let (key, kind) = key_of(call, "pairs", key)?;
        collection.insert(call, key, kind, value)?;
    }
    collection.write()
}

unsafe fn dict_to_list(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["dict"])?;
    as_list(dict(call, object)?)
}

unsafe fn dict_len(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["dict"])?;
    scalar_integer(entry_count(dict(call, object)?) as i32)
}

unsafe fn dict_is_empty(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["dict"])?;
    scalar_bool(entry_count(dict(call, object)?) == 0)
}

unsafe fn dict_get(call: &Call<'_>) -> R<Sexp> {
    let [object, key] = arguments(call, ["dict", "key"])?;
    let object = dict(call, object)?;
    let (key, _) = key_of(call, "key", key)?;
    match lookup(object, key)? {
        Some(position) => ok(records::slot_value(object, 1)?.element(position)),
        None => err("KeyNotFound"),
    }
}

unsafe fn dict_contains(call: &Call<'_>) -> R<Sexp> {
    let [object, key] = arguments(call, ["dict", "key"])?;
    let object = dict(call, object)?;
    let (key, _) = key_of(call, "key", key)?;
    scalar_bool(lookup(object, key)?.is_some())
}

unsafe fn dict_insert(call: &Call<'_>) -> R<Sexp> {
    let [object, key, value] = arguments(call, ["dict", "key", "value"])?;
    let object = dict(call, object)?;
    let (key, kind) = key_of(call, "key", key)?;
    if owned_collection(call, object) && keys_hold(object, kind) {
        insert_in_place(object, key, value)?;
        return Ok(object);
    }
    let mut collection = Collection::read(object)?;
    collection.insert(call, key, kind, value)?;
    collection.write()
}

unsafe fn dict_remove(call: &Call<'_>) -> R<Sexp> {
    let [object, key] = arguments(call, ["dict", "key"])?;
    let object = dict(call, object)?;
    let (key, _) = key_of(call, "key", key)?;
    let Some(removed) = lookup(object, key)? else {
        return Ok(object);
    };
    let mut collection = Collection::read(object)?;
    collection.retain(|position| Ok(position != removed))?;
    collection.write()
}

unsafe fn dict_update(call: &Call<'_>) -> R<Sexp> {
    let [object, key_argument, f] = arguments(call, ["dict", "key", "f"])?;
    let mut collection = Collection::read(dict(call, object)?)?;
    let (key, kind) = key_of(call, "key", key_argument)?;
    let f = function(call, "f", f)?;
    let found = collection.find(key);
    let current = match found {
        Some(position) => ok(collection.values[position])?,
        None => err("Missing")?,
    };
    let next = invoke(call, f, &[current])?;
    match returned_result(call, "f", next)? {
        (true, value) => collection.insert(call, key, kind, value)?,
        (false, _) => {
            if let Some(removed) = found {
                collection.retain(|position| Ok(position != removed))?;
            }
        }
    }
    collection.write()
}

unsafe fn dict_keys(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["dict"])?;
    let keys: Vec<Sexp> = entries(dict(call, object)?)?
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    list_vector(&keys)
}

unsafe fn dict_values(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["dict"])?;
    let collection = Collection::read(dict(call, object)?)?;
    list_vector(&collection.values)
}

unsafe fn dict_walk(call: &Call<'_>) -> R<Sexp> {
    let [object, state, f] = arguments(call, ["dict", "state", "f"])?;
    let collection = Collection::read(dict(call, object)?)?;
    let f = function(call, "f", f)?;
    let mut state = state;
    for position in 0..collection.keys.len() {
        state = scope(|| {
            let key = collection.key_value(position)?;
            invoke(call, f, &[state, key, collection.values[position]])
        })?;
    }
    Ok(state)
}

unsafe fn dict_map(call: &Call<'_>) -> R<Sexp> {
    let [object, f] = arguments(call, ["dict", "f"])?;
    let mut collection = Collection::read(dict(call, object)?)?;
    let f = function(call, "f", f)?;
    for position in 0..collection.keys.len() {
        let value = collection.values[position];
        collection.values[position] = scope(|| {
            let key = collection.key_value(position)?;
            invoke(call, f, &[key, value])
        })?;
    }
    collection.write()
}

unsafe fn dict_filter(call: &Call<'_>, keep: bool) -> R<Sexp> {
    let [object, predicate] = arguments(call, ["dict", "predicate"])?;
    let mut collection = Collection::read(dict(call, object)?)?;
    let predicate = function(call, "predicate", predicate)?;
    let keys: Vec<Sexp> = (0..collection.keys.len())
        .map(|position| collection.key_value(position))
        .collect::<R<_>>()?;
    let stored = collection.values.clone();
    collection.retain(|position| {
        let answer = scope(|| invoke(call, predicate, &[keys[position], stored[position]]))?;
        Ok(verdict(call, "predicate", answer)? == keep)
    })?;
    collection.write()
}

unsafe fn dict_keep_if(call: &Call<'_>) -> R<Sexp> {
    dict_filter(call, true)
}

unsafe fn dict_drop_if(call: &Call<'_>) -> R<Sexp> {
    dict_filter(call, false)
}

/// The two collections of a binary dictionary or set operation.
unsafe fn two_collections(call: &Call<'_>, kind: NativeKind) -> R<(Collection, Collection)> {
    let [first, second] = arguments(call, ["first", "second"])?;
    let first = Collection::read(collection(call, "first", first, kind)?)?;
    let second = Collection::read(collection(call, "second", second, kind)?)?;
    Ok((first, second))
}

/// Inserts every entry of `second` into `first`, whose values `second`'s replace. An owned
/// `first` whose keys hold `second`'s grows in place.
unsafe fn merge(call: &Call<'_>, kind: NativeKind) -> R<Sexp> {
    let [first_object, second_object] = arguments(call, ["first", "second"])?;
    let first_object = collection(call, "first", first_object, kind)?;
    let second_object = collection(call, "second", second_object, kind)?;
    let second = Collection::read(second_object)?;
    if owned_collection(call, first_object)
        && second
            .key_kind
            .is_none_or(|second_kind| keys_hold(first_object, second_kind))
    {
        for (position, &key) in second.keys.iter().enumerate() {
            let value = second.values.get(position).copied().unwrap_or_else(nil);
            insert_in_place(first_object, key, value)?;
        }
        return Ok(first_object);
    }
    let mut first = Collection::read(first_object)?;
    let second_kind = second.key_kind.unwrap_or(KeyKind::Logical);
    for (position, &key) in second.keys.iter().enumerate() {
        let value = second.values.get(position).copied().unwrap_or_else(nil);
        let kind = match (key, second_kind) {
            (Key::Number(_), KeyKind::Integer) => KeyKind::Integer,
            _ => second_kind,
        };
        first.insert(call, key, kind, value)?;
    }
    first.write()
}

/// Keeps the entries of `first` whose key `second` has (`shared`) or lacks. For dictionaries a
/// shared entry also needs an identical value, as Roc's `keep_shared` does.
unsafe fn intersect(call: &Call<'_>, kind: NativeKind, shared: bool) -> R<Sexp> {
    let (mut first, second) = two_collections(call, kind)?;
    let flags = IdenticalFlags::default();
    let keys = first.keys.clone();
    let stored = first.values.clone();
    first.retain(|position| {
        let found = second.find(keys[position]).filter(|&other| {
            !shared
                || kind == NativeKind::Set
                || identical(stored[position], second.values[other], &flags)
        });
        Ok(found.is_some() == shared)
    })?;
    first.write()
}

unsafe fn dict_insert_all(call: &Call<'_>) -> R<Sexp> {
    merge(call, NativeKind::Dict)
}

unsafe fn dict_keep_shared(call: &Call<'_>) -> R<Sexp> {
    intersect(call, NativeKind::Dict, true)
}

unsafe fn dict_remove_all(call: &Call<'_>) -> R<Sexp> {
    intersect(call, NativeKind::Dict, false)
}

unsafe fn dict_walk_until(call: &Call<'_>) -> R<Sexp> {
    let [object, state, f] = arguments(call, ["dict", "state", "f"])?;
    let collection = Collection::read(dict(call, object)?)?;
    let f = function(call, "f", f)?;
    let mut state = state;
    for position in 0..collection.keys.len() {
        let step = scope(|| {
            let key = collection.key_value(position)?;
            invoke(call, f, &[state, key, collection.values[position]])
        })?;
        match walk_step(call, step)? {
            Ok(next) => state = next,
            Err(last) => return Ok(last),
        }
    }
    Ok(state)
}

/// Unions the collections `f` returns for each entry of `object`, a later entry's value
/// replacing an earlier one's.
unsafe fn join_map(call: &Call<'_>, object: Sexp, f: Sexp, kind: NativeKind) -> R<Sexp> {
    let source = Collection::read(object)?;
    let f = function(call, "f", f)?;
    let mut joined = Collection::new(kind);
    for position in 0..source.keys.len() {
        let inner = scope(|| {
            let key = source.key_value(position)?;
            match source.values.get(position) {
                Some(&value) => invoke(call, f, &[key, value]),
                None => invoke(call, f, &[key]),
            }
        })?;
        let inner = Collection::read(collection(call, "f()", inner, kind)?)?;
        let inner_kind = inner.key_kind.unwrap_or(KeyKind::Logical);
        for (at, &key) in inner.keys.iter().enumerate() {
            let value = inner.values.get(at).copied().unwrap_or_else(nil);
            joined.insert(call, key, inner_kind, value)?;
        }
    }
    joined.write()
}

unsafe fn dict_join_map(call: &Call<'_>) -> R<Sexp> {
    let [object, f] = arguments(call, ["dict", "f"])?;
    join_map(call, dict(call, object)?, f, NativeKind::Dict)
}

unsafe fn dict_reserve(call: &Call<'_>) -> R<Sexp> {
    let [object, spare] = arguments(call, ["dict", "spare"])?;
    let object = dict(call, object)?;
    let spare = count(call, "spare", spare)?;
    reserve_collection(call, object, spare)
}

unsafe fn dict_clear(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["dict"])?;
    dict(call, object)?;
    Collection::new(NativeKind::Dict).write()
}

// Set.

unsafe fn set(call: &Call<'_>, value: Sexp) -> R<Sexp> {
    collection(call, "set", value, NativeKind::Set)
}

unsafe fn set_empty(call: &Call<'_>) -> R<Sexp> {
    arguments(call, [])?;
    Collection::new(NativeKind::Set).write()
}

unsafe fn set_single(call: &Call<'_>) -> R<Sexp> {
    let [element] = arguments(call, ["element"])?;
    let (key, kind) = key_of(call, "element", element)?;
    let mut collection = Collection::new(NativeKind::Set);
    collection.insert(call, key, kind, nil())?;
    collection.write()
}

unsafe fn set_from_list(call: &Call<'_>) -> R<Sexp> {
    let [list] = arguments(call, ["list"])?;
    let mut collection = Collection::new(NativeKind::Set);
    for item in list_elements(call, "list", list)? {
        let (key, kind) = key_of(call, "list", item)?;
        collection.insert(call, key, kind, nil())?;
    }
    collection.write()
}

unsafe fn set_to_list(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["set"])?;
    as_list(set(call, object)?)
}

unsafe fn set_len(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["set"])?;
    scalar_integer(entry_count(set(call, object)?) as i32)
}

unsafe fn set_is_empty(call: &Call<'_>) -> R<Sexp> {
    let [object] = arguments(call, ["set"])?;
    scalar_bool(entry_count(set(call, object)?) == 0)
}

unsafe fn set_contains(call: &Call<'_>) -> R<Sexp> {
    let [object, element] = arguments(call, ["set", "element"])?;
    let object = set(call, object)?;
    let (key, _) = key_of(call, "element", element)?;
    scalar_bool(lookup(object, key)?.is_some())
}

unsafe fn set_insert(call: &Call<'_>) -> R<Sexp> {
    let [object, element] = arguments(call, ["set", "element"])?;
    let object = set(call, object)?;
    let (key, kind) = key_of(call, "element", element)?;
    if owned_collection(call, object) && keys_hold(object, kind) {
        insert_in_place(object, key, nil())?;
        return Ok(object);
    }
    let mut collection = Collection::read(object)?;
    collection.insert(call, key, kind, nil())?;
    collection.write()
}

unsafe fn set_remove(call: &Call<'_>) -> R<Sexp> {
    let [object, element] = arguments(call, ["set", "element"])?;
    let object = set(call, object)?;
    let (key, _) = key_of(call, "element", element)?;
    let Some(removed) = lookup(object, key)? else {
        return Ok(object);
    };
    let mut collection = Collection::read(object)?;
    collection.retain(|position| Ok(position != removed))?;
    collection.write()
}

unsafe fn set_union(call: &Call<'_>) -> R<Sexp> {
    merge(call, NativeKind::Set)
}

unsafe fn set_intersection(call: &Call<'_>) -> R<Sexp> {
    intersect(call, NativeKind::Set, true)
}

unsafe fn set_difference(call: &Call<'_>) -> R<Sexp> {
    intersect(call, NativeKind::Set, false)
}

unsafe fn set_walk(call: &Call<'_>) -> R<Sexp> {
    let [object, state, f] = arguments(call, ["set", "state", "f"])?;
    let collection = Collection::read(set(call, object)?)?;
    let f = function(call, "f", f)?;
    let mut state = state;
    for position in 0..collection.keys.len() {
        state = scope(|| {
            let element = collection.key_value(position)?;
            invoke(call, f, &[state, element])
        })?;
    }
    Ok(state)
}

unsafe fn set_map(call: &Call<'_>) -> R<Sexp> {
    let [object, f] = arguments(call, ["set", "f"])?;
    let source = Collection::read(set(call, object)?)?;
    let f = function(call, "f", f)?;
    let mut mapped = Collection::new(NativeKind::Set);
    for position in 0..source.keys.len() {
        let element = scope(|| {
            let element = source.key_value(position)?;
            invoke(call, f, &[element])
        })?;
        let (key, kind) = key_of(call, "f()", element)?;
        mapped.insert(call, key, kind, nil())?;
    }
    mapped.write()
}

unsafe fn set_filter(call: &Call<'_>, keep: bool) -> R<Sexp> {
    let [object, predicate] = arguments(call, ["set", "predicate"])?;
    let mut collection = Collection::read(set(call, object)?)?;
    let predicate = function(call, "predicate", predicate)?;
    let elements: Vec<Sexp> = (0..collection.keys.len())
        .map(|position| collection.key_value(position))
        .collect::<R<_>>()?;
    collection.retain(|position| {
        let answer = scope(|| invoke(call, predicate, &[elements[position]]))?;
        Ok(verdict(call, "predicate", answer)? == keep)
    })?;
    collection.write()
}

unsafe fn set_keep_if(call: &Call<'_>) -> R<Sexp> {
    set_filter(call, true)
}

unsafe fn set_drop_if(call: &Call<'_>) -> R<Sexp> {
    set_filter(call, false)
}

unsafe fn set_walk_until(call: &Call<'_>) -> R<Sexp> {
    let [object, state, f] = arguments(call, ["set", "state", "f"])?;
    let collection = Collection::read(set(call, object)?)?;
    let f = function(call, "f", f)?;
    let mut state = state;
    for position in 0..collection.keys.len() {
        let step = scope(|| {
            let element = collection.key_value(position)?;
            invoke(call, f, &[state, element])
        })?;
        match walk_step(call, step)? {
            Ok(next) => state = next,
            Err(last) => return Ok(last),
        }
    }
    Ok(state)
}

unsafe fn set_join_map(call: &Call<'_>) -> R<Sexp> {
    let [object, f] = arguments(call, ["set", "f"])?;
    join_map(call, set(call, object)?, f, NativeKind::Set)
}

unsafe fn set_reserve(call: &Call<'_>) -> R<Sexp> {
    let [object, spare] = arguments(call, ["set", "spare"])?;
    let object = set(call, object)?;
    let spare = count(call, "spare", spare)?;
    reserve_collection(call, object, spare)
}

/// The names the library binds, for the test that keeps them equal to `types/ry.Rtypes`.
pub fn names() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|definition| definition.name)
}
