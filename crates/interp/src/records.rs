//! ry's records, tuples and tags at runtime: the constructors the new syntax lowers to, the
//! `match` special, field access and update, structural comparison, the conversions to and from
//! R lists, and the error every other builtin raises when it meets one of these values.
//!
//! The values are heap objects of their own kind (see `heap::records`), so R code that was never
//! written for them cannot mistake one for a list: a builtin that does not know them refuses them
//! by name instead of reading garbage.

use {
    crate::{
        builtins::{Arg, BuiltinDef, Call, builtin, special},
        deparse, eval, interp,
        sort::IdenticalFlags,
        standard_library,
        unwind::{R, Unwind, error, error_in},
        values::{self, char_str, length, missing_arg, nil, scalar_string, symbol_name, symbols},
    },
    heap::{
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, Slot, is_na_real},
        records::{Fields, Layout, NativeKind},
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".ry_record", -1, do_record),
    builtin(".ry_tuple", -1, do_tuple),
    builtin(".ry_tag", -1, do_tag),
    special(".ry_match", -1, do_match).keep_visibility(),
    builtin("as.record", 1, do_as_record),
    builtin("as.tuple", 1, do_as_tuple),
    builtin("as.tag", -1, do_as_tag),
    builtin("as.list.ry_record", -1, do_as_list),
    builtin("as.list.ry_tuple", -1, do_as_list),
    builtin("as.list.ry_tag", -1, do_as_list),
    builtin("format.ry_record", -1, do_format),
    builtin("format.ry_tuple", -1, do_format),
    builtin("format.ry_tag", -1, do_format),
    builtin("all.equal.ry_record", -1, do_all_equal),
    builtin("all.equal.ry_tuple", -1, do_all_equal),
    builtin("all.equal.ry_tag", -1, do_all_equal),
    builtin("as.list.ry_dict", -1, do_as_list),
    builtin("as.list.ry_set", -1, do_as_list),
    builtin("format.ry_dict", -1, do_format),
    builtin("format.ry_set", -1, do_format),
    builtin("all.equal.ry_dict", -1, do_all_equal),
    builtin("all.equal.ry_set", -1, do_all_equal),
];

/// The kind of a record object.
pub unsafe fn native_kind(object: Sexp) -> NativeKind {
    let id = object.record_layout();
    match crate::heap().layouts.get(id) {
        Some(layout) => layout.kind(),
        None => panic!("a record refers to layout {id}, which was never interned"),
    }
}

/// A copy of a record object's layout. A copy, because interning a new layout may move the
/// table while the caller still uses this one.
pub unsafe fn layout(object: Sexp) -> Layout {
    let id = object.record_layout();
    match crate::heap().layouts.get(id) {
        Some(layout) => layout.clone(),
        None => panic!("a record refers to layout {id}, which was never interned"),
    }
}

/// The name of a tag id.
pub unsafe fn tag_name(id: u32) -> String {
    match crate::heap().layouts.tag_name(id) {
        Some(symbol) => symbol_name(symbol).into_owned(),
        None => panic!("a layout refers to tag {id}, which was never interned"),
    }
}

/// Slot `index` as an R value. A raw slot becomes a fresh scalar, so the caller sees the same
/// value whether or not the number was stored unboxed.
pub unsafe fn slot_value(object: Sexp, index: usize) -> R<Sexp> {
    match object.record_slot(index) {
        Slot::Handle(value) => Ok(value),
        Slot::Real(value) => values::scalar_real(value),
        Slot::Integer(value) => values::scalar_integer(value),
        Slot::Logical(value) => values::scalar_logical(value),
    }
}

/// The error a builtin that does not understand native values raises for one, naming the
/// argument (`point`) and the conversion that fixes it.
pub fn boundary_message(subject: &str, kind: NativeKind) -> String {
    format!(
        "{subject} is a ry {}, not an R object; convert it with `as.list()`",
        kind.type_name()
    )
}

/// Refuses a native value among `args`, for a builtin that is not in [`ACCEPTS_NATIVE`]. Only
/// the top-level arguments are scanned, so the check costs one type test per argument.
pub unsafe fn check_native_args(call: Sexp, args: &[Arg]) -> R<()> {
    for (position, arg) in args.iter().enumerate() {
        if arg.value.kind() == SexpType::Record {
            let subject = argument_text(call, position)
                .unwrap_or_else(|| format!("argument {}", position + 1));
            return Err(error_in(
                call,
                boundary_message(&subject, native_kind(arg.value)),
            ));
        }
    }
    Ok(())
}

/// The error for a native value reached somewhere that needs an R object, such as a `for` loop
/// or an `if` condition; `expression` is the code that produced it.
pub unsafe fn boundary_error(value: Sexp, expression: Sexp, call: Sexp) -> Unwind {
    let subject = short_code(expression).unwrap_or_else(|| "the value".to_owned());
    error_in(call, boundary_message(&subject, native_kind(value)))
}

/// The source of argument `position` of `call`, in backticks, unless a `...` before it makes the
/// position ambiguous.
unsafe fn argument_text(call: Sexp, position: usize) -> Option<String> {
    if call.kind() != SexpType::Language {
        return None;
    }
    let mut cell = call.cdr();
    for _ in 0..position {
        if cell.kind() != SexpType::Pairlist || cell.car() == symbols().dots {
            return None;
        }
        cell = cell.cdr();
    }
    if cell.kind() != SexpType::Pairlist || cell.car() == symbols().dots {
        return None;
    }
    short_code(cell.car())
}

unsafe fn short_code(expression: Sexp) -> Option<String> {
    if !matches!(expression.kind(), SexpType::Symbol | SexpType::Language) {
        return None;
    }
    let text = deparse::deparse_one_line(expression);
    (text.chars().count() <= 60).then(|| format!("`{text}`"))
}

// Construction.

unsafe fn do_record(call: &Call<'_>) -> R<Sexp> {
    if call.is_empty() {
        return Err(call.error("a record needs at least one field; `()` is the empty tuple"));
    }
    let items = call_items(call)?;
    if items.iter().any(|(name, _)| name.is_none()) {
        return Err(call.error("every field of a record needs a name"));
    }
    make(None, &items).map_err(|message| call.error(message))
}

unsafe fn do_tuple(call: &Call<'_>) -> R<Sexp> {
    let items = call_items(call)?;
    if items.iter().any(|(name, _)| name.is_some()) {
        return Err(call.error("a tuple's items have no names; use a record for named fields"));
    }
    make(None, &items).map_err(|message| call.error(message))
}

unsafe fn do_tag(call: &Call<'_>) -> R<Sexp> {
    let Some((name, payload)) = call.args.split_first() else {
        return Err(call.error("a tag needs a name"));
    };
    let tag = tag_id(name.value).map_err(|message| call.error(message))?;
    let items = arg_items(payload)?;
    make(Some(tag), &items).map_err(|message| call.error(message))
}

/// The items of a constructor call, each with its field name if it has one.
unsafe fn call_items(call: &Call<'_>) -> R<Vec<(Option<Sexp>, Sexp)>> {
    arg_items(call.args)
}

unsafe fn arg_items(args: &[Arg]) -> R<Vec<(Option<Sexp>, Sexp)>> {
    let mut items = Vec::with_capacity(args.len());
    for arg in args {
        if arg.value == missing_arg() {
            return Err(error("a record or tuple item is empty"));
        }
        let name = (arg.tag.kind() == SexpType::Symbol && !symbol_name(arg.tag).is_empty())
            .then_some(arg.tag);
        items.push((name, arg.value));
    }
    Ok(items)
}

/// The interned id of the tag a length-one string names.
unsafe fn tag_id(name: Sexp) -> Result<u32, String> {
    if name.kind() != SexpType::String
        || length(name) != 1
        || name.element(0) == values::na_string()
    {
        return Err("a tag's name must be a single string".to_owned());
    }
    let text = char_str(name.element(0));
    if text.is_empty() {
        return Err("a tag's name cannot be empty".to_owned());
    }
    let symbol = values::install(&text).map_err(|_| "cannot intern the tag name".to_owned())?;
    Ok(crate::heap().layouts.intern_tag(symbol))
}

/// Builds a record (named items), tuple (unnamed items) or tag (with `tag`). Field names are
/// sorted by their bytes, so field order is part of neither the value nor its layout.
pub(crate) unsafe fn make(
    tag: Option<u32>,
    items: &[(Option<Sexp>, Sexp)],
) -> Result<Sexp, String> {
    let named = items.iter().filter(|(name, _)| name.is_some()).count();
    if named != 0 && named != items.len() {
        return Err("a record cannot mix named fields and unnamed items".to_owned());
    }
    let (fields, slots) = if named == 0 {
        let slots: Vec<Sexp> = items.iter().map(|(_, value)| *value).collect();
        (Fields::Positional(slots.len() as u32), slots)
    } else {
        let mut sorted: Vec<(Sexp, Sexp)> = items
            .iter()
            .filter_map(|(name, value)| name.map(|name| (name, *value)))
            .collect();
        sorted.sort_by(|(left, _), (right, _)| left.car().bytes().cmp(right.car().bytes()));
        if let Some(pair) = sorted.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(format!(
                "the field `{}` appears twice",
                symbol_name(pair[0].0)
            ));
        }
        let names: Box<[Sexp]> = sorted.iter().map(|(name, _)| *name).collect();
        let slots = sorted.iter().map(|(_, value)| *value).collect();
        (Fields::Named(names), slots)
    };
    alloc(Layout { tag, fields }, &slots).map_err(|failure| failure.to_string())
}

pub(crate) unsafe fn alloc(layout: Layout, slots: &[Sexp]) -> Result<Sexp, heap::HeapError> {
    let heap = crate::heap();
    // A dictionary or set counts its holders, so the standard library can grow one in place when
    // its only holder is the binding being reassigned (`standard_library::reusable`).
    let counted = matches!(layout.fields, Fields::Dict | Fields::Set);
    let id = heap.layouts.intern(layout);
    let object = if counted {
        heap.alloc_counted_record(id, slots)?
    } else {
        heap.alloc_record(id, slots)?
    };
    heap.root(object);
    Ok(object)
}

/// A copy of `object` with slot `index` replaced by `value`. The layout stays the same, so an
/// update can never add a field.
unsafe fn with_slot(object: Sexp, index: usize, value: Sexp) -> R<Sexp> {
    let count = object.record_slot_count();
    let mut slots = Vec::with_capacity(count);
    for position in 0..count {
        slots.push(if position == index {
            value
        } else {
            slot_value(object, position)?
        });
    }
    let heap = crate::heap();
    let result = heap.alloc_record(object.record_layout(), &slots)?;
    heap.root(result);
    Ok(result)
}

// Access and update.

/// `object$name`: the exact field, never a partial match.
pub unsafe fn dollar(object: Sexp, name: &[u8], call: Sexp) -> R<Sexp> {
    let index = field_index(object, name, call)?;
    slot_value(object, index)
}

/// `object$name <- value`.
pub unsafe fn dollar_assign(object: Sexp, name: &[u8], value: Sexp, call: Sexp) -> R<Sexp> {
    let index = field_index(object, name, call)?;
    with_slot(object, index, value)
}

/// `object[[index]]`, by field name or by position.
pub unsafe fn subset2(object: Sexp, indices: &[Sexp], call: Sexp) -> R<Sexp> {
    let index = item_index(object, indices, call)?;
    slot_value(object, index)
}

/// `object[[index]] <- value`.
pub unsafe fn subassign2(object: Sexp, indices: &[Sexp], value: Sexp, call: Sexp) -> R<Sexp> {
    let index = item_index(object, indices, call)?;
    with_slot(object, index, value)
}

unsafe fn field_index(object: Sexp, name: &[u8], call: Sexp) -> R<usize> {
    let layout = layout(object);
    let text = String::from_utf8_lossy(name);
    match layout.kind() {
        NativeKind::Tag => Err(error_in(
            call,
            "a tag has no fields; read its payload with `match`",
        )),
        NativeKind::Dict => Err(error_in(
            call,
            "a dict has no fields; read a value with `Dict.get(dict, key)`",
        )),
        NativeKind::Set => Err(error_in(
            call,
            "a set has no fields; test an element with `Set.contains(set, element)`",
        )),
        NativeKind::Tuple => Err(error_in(
            call,
            format!("a tuple has no field `{text}`; read its items by position, as in `x[[1L]]`"),
        )),
        NativeKind::Record => layout
            .names()
            .iter()
            .position(|field| field.car().bytes() == name)
            .ok_or_else(|| {
                error_in(
                    call,
                    format!(
                        "the record has no field `{text}`; its fields are {}",
                        field_list(&layout)
                    ),
                )
            }),
    }
}

unsafe fn item_index(object: Sexp, indices: &[Sexp], call: Sexp) -> R<usize> {
    let [index] = indices else {
        return Err(error_in(
            call,
            "a record or tuple takes exactly one index in `[[`",
        ));
    };
    let index = *index;
    match native_kind(object) {
        NativeKind::Dict => {
            return Err(error_in(
                call,
                "a dict cannot be indexed; read a value with `Dict.get(dict, key)`",
            ));
        }
        NativeKind::Set => {
            return Err(error_in(
                call,
                "a set cannot be indexed; test an element with `Set.contains(set, element)`",
            ));
        }
        NativeKind::Record | NativeKind::Tuple | NativeKind::Tag => {}
    }
    if index.kind() == SexpType::String && length(index) == 1 {
        return field_index(object, index.element(0).bytes(), call);
    }
    let layout = layout(object);
    if layout.kind() == NativeKind::Tag {
        return Err(error_in(
            call,
            "a tag has no items; read its payload with `match`",
        ));
    }
    if !matches!(index.kind(), SexpType::Integer | SexpType::Real) || length(index) != 1 {
        return Err(error_in(
            call,
            "a record or tuple index must be one string or one number",
        ));
    }
    let position = values::as_real_scalar(index);
    let count = layout.slot_count();
    if position.is_nan() || position < 1.0 || position.trunc() as usize > count {
        let what = if layout.kind() == NativeKind::Record {
            "record"
        } else {
            "tuple"
        };
        let shown = deparse::deparse_one_line(index);
        return Err(error_in(
            call,
            format!("index {shown} is out of range for a {what} of length {count}"),
        ));
    }
    Ok(position.trunc() as usize - 1)
}

unsafe fn field_list(layout: &Layout) -> String {
    let names: Vec<String> = layout
        .names()
        .iter()
        .map(|name| format!("`{}`", symbol_name(*name)))
        .collect();
    names.join(", ")
}

// Matching.

/// `.ry_match(scrutinee, `=>`(pattern, body), `=>`(pattern, guard, body), ...)`: the first arm
/// whose pattern matches and whose guard is `TRUE` wins. Its bindings are assigned in the
/// calling environment, as a `for` loop binds its variable, so an assignment in the arm body
/// reaches the enclosing function; a guard sees them in a temporary child environment, so a
/// failing guard leaves the caller untouched.
unsafe fn do_match(call: &Call<'_>) -> R<Sexp> {
    let Some((scrutinee, arms)) = call.args.split_first() else {
        return Err(call.error("`match` needs a value to match"));
    };
    let value = eval::eval(scrutinee.value, call.env)?;
    let arrow = values::install("=>")?;
    let mut bindings = Vec::new();
    for arm in arms {
        let arm = arm.value;
        if arm.kind() != SexpType::Language || arm.car() != arrow {
            return Err(call.error("each arm of a `match` must be a `pattern => value` call"));
        }
        let parts: Vec<Sexp> = values::pairlist_items(arm.cdr())
            .into_iter()
            .map(|(_, part)| part)
            .collect();
        let (pattern, guard, body) = match parts.as_slice() {
            [pattern, body] => (*pattern, None, *body),
            [pattern, guard, body] => (*pattern, Some(*guard), *body),
            _ => {
                return Err(
                    call.error("a `match` arm needs a pattern, an optional guard and a value")
                );
            }
        };
        bindings.clear();
        if !matches(pattern, value, &mut bindings, call.call)? {
            continue;
        }
        if let Some(guard) = guard {
            let scratch = values::new_env(call.env)?;
            for &(name, bound) in &bindings {
                eval::define(name, bound, scratch)?;
            }
            let condition = eval::eval(guard, scratch)?;
            if condition.kind() == SexpType::Record {
                return Err(boundary_error(condition, guard, call.call));
            }
            if !eval::as_condition(condition, call.call)? {
                continue;
            }
        }
        for &(name, bound) in &bindings {
            eval::define(name, bound, call.env)?;
        }
        interp().visible = true;
        return eval::eval(body, call.env);
    }
    Err(call.error(format!(
        "no arm of this `match` matches {}",
        describe(value)
    )))
}

/// The value a failed `match` names, shortened to one line.
unsafe fn describe(value: Sexp) -> String {
    let text = deparse::deparse_one_line(value);
    if text.chars().count() > 80 {
        let cut: String = text.chars().take(77).collect();
        format!("{cut}...")
    } else {
        text
    }
}

/// Whether `value` matches `pattern`, pushing the names the pattern binds.
unsafe fn matches(
    pattern: Sexp,
    value: Sexp,
    bindings: &mut Vec<(Sexp, Sexp)>,
    call: Sexp,
) -> R<bool> {
    match pattern.kind() {
        SexpType::Symbol => {
            let name = symbol_name(pattern);
            match name.as_ref() {
                "_" => Ok(true),
                ".." => Err(error_in(
                    call,
                    "`..` may only end a record pattern or stand for a tag's payload",
                )),
                "T" | "F" => {
                    let suggestion = if name == "T" { "TRUE" } else { "FALSE" };
                    Err(error_in(
                        call,
                        format!(
                            "`{name}` in a pattern would bind any value to a variable; write `{suggestion}` to match the logical value"
                        ),
                    ))
                }
                _ => {
                    bindings.push((pattern, value));
                    Ok(true)
                }
            }
        }
        SexpType::Language => {
            let head = pattern.car();
            let arguments = values::pairlist_items(pattern.cdr());
            let head_name = if head.kind() == SexpType::Symbol {
                symbol_name(head).into_owned()
            } else {
                String::new()
            };
            match head_name.as_str() {
                "|" if arguments.len() == 2 => {
                    let mark = bindings.len();
                    if matches(arguments[0].1, value, bindings, call)? {
                        return Ok(true);
                    }
                    bindings.truncate(mark);
                    matches(arguments[1].1, value, bindings, call)
                }
                "(" if arguments.len() == 1 => matches(arguments[0].1, value, bindings, call),
                ".ry_tuple" | ".ry_record" => {
                    if value.kind() != SexpType::Record {
                        return Ok(false);
                    }
                    let layout = layout(value);
                    if layout.tag.is_some() {
                        return Ok(false);
                    }
                    payload_matches(&arguments, value, &layout, bindings, call)
                }
                ".ry_tag" => {
                    let Some(((_, name), payload)) = arguments.split_first() else {
                        return Err(error_in(call, "a tag pattern needs a name"));
                    };
                    if value.kind() != SexpType::Record {
                        return Ok(false);
                    }
                    let layout = layout(value);
                    let Some(tag) = layout.tag else {
                        return Ok(false);
                    };
                    if name.kind() != SexpType::String || length(*name) != 1 {
                        return Err(error_in(
                            call,
                            "a tag pattern's name must be a single string",
                        ));
                    }
                    if tag_name(tag).as_bytes() != name.element(0).bytes() {
                        return Ok(false);
                    }
                    if let [(tag, rest)] = payload
                        && *tag == nil()
                        && is_rest(*rest)
                    {
                        return Ok(true);
                    }
                    payload_matches(payload, value, &layout, bindings, call)
                }
                _ => Err(error_in(
                    call,
                    format!("`{}` is not a pattern", deparse::deparse_one_line(pattern)),
                )),
            }
        }
        SexpType::Nil
        | SexpType::Logical
        | SexpType::Integer
        | SexpType::Real
        | SexpType::Complex
        | SexpType::String => Ok(literal_matches(pattern, value)),
        _ => Err(error_in(call, "invalid pattern in `match`")),
    }
}

unsafe fn is_rest(pattern: Sexp) -> bool {
    pattern.kind() == SexpType::Symbol && pattern.car().bytes() == b".."
}

/// Matches the items or fields of a tuple, record or tag payload. A record pattern without `..`
/// needs exactly its fields; with `..` the value may have more.
unsafe fn payload_matches(
    items: &[(Sexp, Sexp)],
    value: Sexp,
    layout: &Layout,
    bindings: &mut Vec<(Sexp, Sexp)>,
    call: Sexp,
) -> R<bool> {
    let rest = items
        .last()
        .is_some_and(|(tag, item)| *tag == nil() && is_rest(*item));
    let items = if rest {
        &items[..items.len() - 1]
    } else {
        items
    };
    let named = items.iter().any(|(tag, _)| *tag != nil());
    match &layout.fields {
        Fields::Dict | Fields::Set => Ok(false),
        Fields::Positional(count) => {
            if named || rest || items.len() != *count as usize {
                return Ok(false);
            }
            for (index, (_, item)) in items.iter().enumerate() {
                let slot = slot_value(value, index)?;
                if !matches(*item, slot, bindings, call)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Fields::Named(names) => {
            if !named && !items.is_empty() {
                return Ok(false);
            }
            if !rest && items.len() != names.len() {
                return Ok(false);
            }
            for (tag, item) in items {
                let Some(index) = names.iter().position(|name| name == tag) else {
                    return Ok(false);
                };
                let slot = slot_value(value, index)?;
                if !matches(*item, slot, bindings, call)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// A literal pattern matches a length-one value of the same kind that equals it: numbers match
/// numbers whatever their storage, a missing literal matches a missing value of any type, and
/// `NaN` matches `NaN`.
unsafe fn literal_matches(pattern: Sexp, value: Sexp) -> bool {
    if pattern == nil() || value == nil() {
        return pattern == value;
    }
    if length(value) != 1 || value.is_object() || length(pattern) != 1 {
        return false;
    }
    match (scalar(pattern), scalar(value)) {
        (Some(Scalar::Missing), Some(Scalar::Missing)) => true,
        (Some(Scalar::Number(left)), Some(Scalar::Number(right))) => {
            left == right || left.is_nan() && right.is_nan()
        }
        (Some(Scalar::Logical(left)), Some(Scalar::Logical(right))) => left == right,
        (
            Some(Scalar::Complex(left_real, left_imaginary)),
            Some(Scalar::Complex(right_real, right_imaginary)),
        ) => left_real == right_real && left_imaginary == right_imaginary,
        (Some(Scalar::Text(left)), Some(Scalar::Text(right))) => left == right,
        _ => false,
    }
}

enum Scalar {
    Missing,
    Number(f64),
    Logical(bool),
    Complex(f64, f64),
    Text(Vec<u8>),
}

unsafe fn scalar(value: Sexp) -> Option<Scalar> {
    let value = values::materialize(value).ok()?;
    Some(match value.kind() {
        SexpType::Logical => match values::ints(value)[0] {
            NA_LOGICAL => Scalar::Missing,
            flag => Scalar::Logical(flag != 0),
        },
        SexpType::Integer => match values::ints(value)[0] {
            NA_INTEGER => Scalar::Missing,
            number => Scalar::Number(f64::from(number)),
        },
        SexpType::Real => {
            let number = values::reals(value)[0];
            if is_na_real(number) {
                Scalar::Missing
            } else {
                Scalar::Number(number)
            }
        }
        SexpType::Complex => {
            let number = values::complexes(value)[0];
            if is_na_real(number.r) || is_na_real(number.i) {
                Scalar::Missing
            } else {
                Scalar::Complex(number.r, number.i)
            }
        }
        SexpType::String => {
            let text = value.element(0);
            if text == values::na_string() {
                Scalar::Missing
            } else {
                Scalar::Text(text.bytes().to_vec())
            }
        }
        _ => return None,
    })
}

// Conversions.

/// `as.list()`: a record becomes a named list, a tuple an unnamed one, and a tag
/// `list(tag = "Name", payload = list(...))`.
unsafe fn do_as_list(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() != SexpType::Record {
        return Err(
            call.error("`as.list()` for a ry value needs a record, tuple, tag, dict or set")
        );
    }
    if matches!(native_kind(object), NativeKind::Dict | NativeKind::Set) {
        return standard_library::as_list(object);
    }
    let layout = layout(object);
    let mut items = Vec::with_capacity(layout.slot_count());
    for index in 0..layout.slot_count() {
        items.push(slot_value(object, index)?);
    }
    let list = values::list_vector(&items)?;
    if let Fields::Named(names) = &layout.fields {
        let names: Vec<String> = names
            .iter()
            .map(|name| symbol_name(*name).into_owned())
            .collect();
        values::set_attr(list, symbols().names, values::string_vector(&names)?)?;
    }
    match layout.tag {
        None => Ok(list),
        Some(tag) => {
            let name = scalar_string(&tag_name(tag))?;
            values::named_list(&[("tag", name), ("payload", list)])
        }
    }
}

/// `as.record(list)`: a list whose elements all have distinct, non-empty names.
unsafe fn do_as_record(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() == SexpType::Record && native_kind(object) == NativeKind::Record {
        return Ok(object);
    }
    if object.kind() != SexpType::List || length(object) == 0 {
        return Err(call.error("`as.record()` needs a non-empty named list"));
    }
    let items = list_items(object, true)
        .map_err(|message| call.error(format!("`as.record()`: {message}")))?;
    make(None, &items).map_err(|message| call.error(format!("`as.record()`: {message}")))
}

/// `as.tuple(list)`: a list without names.
unsafe fn do_as_tuple(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() == SexpType::Record && native_kind(object) == NativeKind::Tuple {
        return Ok(object);
    }
    if !matches!(object.kind(), SexpType::List | SexpType::Nil) {
        return Err(call.error("`as.tuple()` needs a list"));
    }
    let items = list_items(object, false)
        .map_err(|message| call.error(format!("`as.tuple()`: {message}")))?;
    make(None, &items).map_err(|message| call.error(format!("`as.tuple()`: {message}")))
}

/// `as.tag(name, payload = list())`: a named payload list gives named fields, an unnamed one
/// positional items.
unsafe fn do_as_tag(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["name", "payload"])?;
    let Some(mut name) = matched.first().copied().flatten() else {
        return Err(call.error("`as.tag()` needs a name"));
    };
    let mut payload = matched.get(1).copied().flatten().unwrap_or_else(nil);
    // `as.list()` gives a tag as `list(tag = , payload = )`, so that list converts back.
    if name.kind() == SexpType::List && matched.get(1).copied().flatten().is_none() {
        let names = values::names(name);
        let element = |wanted: &[u8]| {
            (0..length(name))
                .find(|&index| names != nil() && names.element(index).bytes() == wanted)
                .map(|index| name.element(index))
        };
        let Some(tag) = element(b"tag") else {
            return Err(call.error(
                "`as.tag()` needs a name, or a list with a `tag` element as `as.list()` gives",
            ));
        };
        payload = element(b"payload").unwrap_or_else(nil);
        name = tag;
    }
    let tag = tag_id(name).map_err(|message| call.error(format!("`as.tag()`: {message}")))?;
    if !matches!(payload.kind(), SexpType::List | SexpType::Nil) {
        return Err(call.error("`as.tag()` needs the payload as a list"));
    }
    let named = values::names(payload) != nil();
    let items = list_items(payload, named)
        .map_err(|message| call.error(format!("`as.tag()`: {message}")))?;
    make(Some(tag), &items).map_err(|message| call.error(format!("`as.tag()`: {message}")))
}

unsafe fn list_items(list: Sexp, named: bool) -> Result<Vec<(Option<Sexp>, Sexp)>, String> {
    let names = values::names(list);
    let mut items = Vec::with_capacity(length(list));
    for index in 0..length(list) {
        let name = if names == nil() {
            None
        } else {
            Some(names.element(index))
        };
        let name = name.filter(|name| *name != values::na_string() && !name.bytes().is_empty());
        let field = match (named, name) {
            (true, Some(name)) => Some(
                values::install_charsxp(name)
                    .map_err(|_| "cannot intern a field name".to_owned())?,
            ),
            (true, None) => return Err(format!("element {} has no name", index + 1)),
            (false, Some(name)) => {
                return Err(format!(
                    "element {} is named `{}`; a tuple's items have no names, so use `as.record()`",
                    index + 1,
                    char_str(name)
                ));
            }
            (false, None) => None,
        };
        items.push((field, list.element(index)));
    }
    Ok(items)
}

/// `format()`: the value in ry syntax, as one string.
unsafe fn do_format(call: &Call<'_>) -> R<Sexp> {
    scalar_string(&deparse::deparse_one_line(call.arg(0)))
}

/// `all.equal()`: `TRUE`, or the differences. Field values are compared with R's own
/// `all.equal` when base R is loaded, so numeric tolerance applies inside a record as it does in
/// a list, and testthat's `expect_equal` works on these values.
unsafe fn do_all_equal(call: &Call<'_>) -> R<Sexp> {
    let target = call.arg(0);
    let current = call.arg(1);
    let extra: Vec<Arg> = call.args.iter().skip(2).copied().collect();
    let mut messages = Vec::new();
    compare(target, current, &extra, call.env, "", &mut messages)?;
    if messages.is_empty() {
        values::scalar_bool(true)
    } else {
        values::string_vector(&messages)
    }
}

unsafe fn compare(
    target: Sexp,
    current: Sexp,
    extra: &[Arg],
    env: Sexp,
    prefix: &str,
    messages: &mut Vec<String>,
) -> R<()> {
    if target.kind() != SexpType::Record || current.kind() != SexpType::Record {
        if target.kind() == SexpType::Record || current.kind() == SexpType::Record {
            messages.push(format!(
                "{prefix}target is {}, current is {}",
                kind_phrase(target),
                kind_phrase(current)
            ));
            return Ok(());
        }
        return compare_r(target, current, extra, env, prefix, messages);
    }
    let (left, right) = (layout(target), layout(current));
    if left.kind() != right.kind() {
        messages.push(format!(
            "{prefix}target is {}, current is {}",
            kind_phrase(target),
            kind_phrase(current)
        ));
        return Ok(());
    }
    if matches!(left.kind(), NativeKind::Dict | NativeKind::Set) {
        match standard_library::paired_entries(target, current)? {
            Some(pairs) => {
                for (key, left_value, right_value) in pairs {
                    let label = format!("{prefix}Key {key}: ");
                    compare(left_value, right_value, extra, env, &label, messages)?;
                }
            }
            None => messages.push(format!("{prefix}target and current hold different keys")),
        }
        return Ok(());
    }
    if left.tag != right.tag {
        let name = |tag: Option<u32>| tag.map(|tag| tag_name(tag)).unwrap_or_default();
        messages.push(format!(
            "{prefix}target is tag `:{}`, current is tag `:{}`",
            name(left.tag),
            name(right.tag)
        ));
        return Ok(());
    }
    if left.fields != right.fields {
        match (&left.fields, &right.fields) {
            (Fields::Named(_), Fields::Named(_)) => messages.push(format!(
                "{prefix}target has fields {}, current has fields {}",
                field_list(&left),
                field_list(&right)
            )),
            (Fields::Named(_), Fields::Positional(_))
            | (Fields::Positional(_), Fields::Named(_)) => messages.push(format!(
                "{prefix}target and current have different payload shapes"
            )),
            (Fields::Positional(left), Fields::Positional(right)) => messages.push(format!(
                "{prefix}target has {left} items, current has {right}"
            )),
            _ => messages.push(format!(
                "{prefix}target and current have different payload shapes"
            )),
        }
        return Ok(());
    }
    for index in 0..left.slot_count() {
        let label = match left.names().get(index) {
            Some(name) => format!("{prefix}Field `{}`: ", symbol_name(*name)),
            None => format!("{prefix}Item {}: ", index + 1),
        };
        let (left_value, right_value) = (slot_value(target, index)?, slot_value(current, index)?);
        compare(left_value, right_value, extra, env, &label, messages)?;
    }
    Ok(())
}

/// Compares two R values with base R's `all.equal`, or with `identical` when base R is not
/// loaded.
unsafe fn compare_r(
    target: Sexp,
    current: Sexp,
    extra: &[Arg],
    env: Sexp,
    prefix: &str,
    messages: &mut Vec<String>,
) -> R<()> {
    let flags = IdenticalFlags::default();
    if crate::sort::identical(target, current, &flags) {
        return Ok(());
    }
    let symbol = values::install("all.equal")?;
    let function = eval::find_var(symbol, values::base_namespace())?;
    let function = if function == values::unbound() {
        nil()
    } else {
        eval::value_of(function)?
    };
    if function.kind() != SexpType::Closure {
        messages.push(format!("{prefix}target and current differ"));
        return Ok(());
    }
    let mut args = vec![
        Arg {
            tag: nil(),
            value: target,
        },
        Arg {
            tag: nil(),
            value: current,
        },
    ];
    args.extend_from_slice(extra);
    let result = eval::apply(nil(), function, &args, env)?;
    if result.kind() == SexpType::String {
        for index in 0..length(result) {
            messages.push(format!("{prefix}{}", char_str(result.element(index))));
        }
    } else if !(result.kind() == SexpType::Logical && values::as_logical_scalar(result) == 1) {
        messages.push(format!("{prefix}target and current differ"));
    }
    Ok(())
}

unsafe fn kind_phrase(value: Sexp) -> String {
    if value.kind() == SexpType::Record {
        format!("a ry {}", native_kind(value).type_name())
    } else {
        format!("of type {}", value.kind().name())
    }
}
