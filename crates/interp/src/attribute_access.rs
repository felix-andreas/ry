//! Attribute reads and writes (`attr`, `attr<-`, `attributes`, `names`, `names<-`, `dim`) and
//! the class queries (`class`, `oldClass`, `inherits`), each with the direct or unary entry a
//! compiled call takes for plain values.
//!
//! Attribute tags are symbols whose print names are interned `CHARSXP`s, so a lookup by name
//! compares pointers (see [`same_text`]) and reads bytes only for a partial match.

use {
    crate::{
        builtins::{BuiltinDef, Call, Generic, builtin, internal, is_native_value, present},
        dispatch::{self, basic_class},
        eval,
        subset::{modifiable, set_attribute_checked, set_names, set_row_names},
        unwind::R,
        values::{self, alloc, get_attr, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp},
    smallvec::SmallVec,
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin("attr", -1, do_attr).direct(direct_attr),
    builtin("attr<-", 3, do_set_attr).direct(direct_set_attr),
    builtin("attributes", 1, do_attributes).unary(|object| unsafe {
        if is_native_value(object) {
            return Ok(None);
        }
        attributes(object).map(Some)
    }),
    builtin("names", 1, do_names)
        .generic(Generic::Internal)
        .unary(|object| unsafe {
            if !plain(object) || object.kind() == SexpType::Environment {
                return Ok(None);
            }
            Ok(Some(values::names(object)))
        }),
    builtin("names<-", 2, do_set_names)
        .generic(Generic::Internal)
        .direct(direct_set_names),
    builtin("dim", 1, do_dim)
        .generic(Generic::Internal)
        .unary(|object| unsafe {
            if !plain(object) {
                return Ok(None);
            }
            Ok(Some(get_attr(object, symbols().dim)))
        }),
    builtin("class", 1, do_class).unary(|object| unsafe {
        if is_native_value(object) {
            return Ok(None);
        }
        dispatch::class_of(object).map(Some)
    }),
    builtin("oldClass", 1, do_old_class).unary(|object| unsafe {
        if is_native_value(object) {
            return Ok(None);
        }
        Ok(Some(get_attr(object, symbols().class)))
    }),
    internal("inherits", 3, do_inherits),
];

/// Whether two `CHARSXP`s hold the same text. Interned ASCII strings are equal exactly when they
/// are the same object, so bytes are compared only when either side is not ASCII.
#[inline]
pub(crate) unsafe fn same_text(left: Sexp, right: Sexp) -> bool {
    left == right
        || (!(left.has_gp(gp::ASCII) && right.has_gp(gp::ASCII)) && left.bytes() == right.bytes())
}

/// GNU R's `do_attr` lookup of the attribute named by the `CHARSXP` `which`: a full match on a
/// tag wins; otherwise, unless `exact`, a unique prefix of one tag, with `names` special because
/// a pairlist's names are not stored as an attribute. The value is `getAttrib`'s, which expands
/// compact row names and reads a pairlist's or a one-dimensional array's names.
pub unsafe fn attribute(object: Sexp, which: Sexp, exact: bool) -> R<Sexp> {
    let wanted = which.bytes();
    // One pass finds both kinds of match. Only a strictly longer tag can be a partial match,
    // because an equal-length prefix is a full match, so tags of the wanted length (every tag,
    // in a vector used as a map from fixed-width keys) cost one pointer compare and no memcmp.
    let mut partial = None;
    let mut ambiguous = false;
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        let text = cell.tag().car();
        if same_text(text, which) {
            return attribute_value(object, cell.tag(), cell.car());
        }
        if !exact {
            let tag_text = text.bytes();
            if tag_text.len() > wanted.len() && tag_text.starts_with(wanted) {
                ambiguous |= partial.is_some();
                partial = Some((cell.tag(), cell.car()));
            }
        }
        cell = cell.cdr();
    }
    if wanted == b"names" {
        return Ok(values::names(object));
    }
    if exact || ambiguous {
        return Ok(nil());
    }
    if b"names".starts_with(wanted) {
        match partial {
            None => return Ok(values::names(object)),
            // A prefix of both `names` and another tag is ambiguous when names exist.
            Some((tag, _)) if tag != symbols().names && values::names(object) != nil() => {
                return Ok(nil());
            }
            Some(_) => {}
        }
    }
    match partial {
        Some((tag, value)) => attribute_value(object, tag, value),
        None => Ok(nil()),
    }
}

/// `getAttrib`'s value for the attribute `tag`, stored as `value`.
#[inline]
unsafe fn attribute_value(object: Sexp, tag: Sexp, value: Sexp) -> R<Sexp> {
    let symbols = symbols();
    if tag == symbols.names {
        return Ok(values::names(object));
    }
    if tag == symbols.row_names {
        return expand_row_names(value);
    }
    Ok(value)
}

/// Compact row names (`c(NA, -n)`) read back as `1:n`.
pub unsafe fn expand_row_names(value: Sexp) -> R<Sexp> {
    if value.kind() == SexpType::Integer
        && length(value) == 2
        && values::ints(value)[0] == NA_INTEGER
    {
        let count = values::ints(value)[1].unsigned_abs() as usize;
        let result = alloc(SexpType::Integer, count)?;
        for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
            *slot = index as i32 + 1;
        }
        return Ok(result);
    }
    Ok(value)
}

/// Whether `object` is a value the attribute builtins answer without dispatch or a refusal.
#[inline]
unsafe fn plain(object: Sexp) -> bool {
    !object.is_object() && !is_native_value(object)
}

// `attr` and `attr<-`.

unsafe fn do_attr(call: &Call<'_>) -> R<Sexp> {
    if !(2..=3).contains(&call.args.len()) {
        return Err(call.error("either 2 or 3 arguments are required"));
    }
    let (matched, _) = call.match_args(&["x", "which", "exact"])?;
    let object = present(matched[0]).unwrap_or(nil());
    let which = present(matched[1]).unwrap_or(nil());
    if which.kind() != SexpType::String {
        return Err(call.error("'which' must be of mode character"));
    }
    if length(which) != 1 {
        return Err(call.error("exactly one attribute 'which' must be given"));
    }
    let exact = present(matched[2]).is_some_and(|exact| values::as_logical_scalar(exact) == 1);
    attribute(object, which.element(0), exact)
}

/// `attr(x, which)` and `attr(x, which, exact)` for one name and a logical `exact`.
unsafe fn direct_attr(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let (object, which) = match args {
        [object, which] | [object, which, _] => (*object, *which),
        _ => return Ok(None),
    };
    if which.kind() != SexpType::String || length(which) != 1 || is_native_value(object) {
        return Ok(None);
    }
    let exact = match args.get(2) {
        None => false,
        Some(&exact) if exact.kind() == SexpType::Logical && length(exact) == 1 => {
            let value = values::ints(exact)[0];
            value != NA_LOGICAL && value != 0
        }
        Some(_) => return Ok(None),
    };
    attribute(object, which.element(0), exact).map(Some)
}

unsafe fn do_set_attr(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "which", "value"])?;
    let object = present(matched[0]).unwrap_or(nil());
    let which = present(matched[1]).unwrap_or(nil());
    let value = present(matched[2]).unwrap_or(nil());
    // GNU R's `isValidString`: the first of any number of names, if it is not `NA`.
    if which.kind() != SexpType::String
        || length(which) == 0
        || which.element(0) == values::na_string()
    {
        return Err(call.error("'name' must be non-null character string"));
    }
    if object == nil() {
        if value == nil() {
            return Ok(nil());
        }
        return Err(call.error("attempt to set an attribute on NULL"));
    }
    let object = modifiable(object)?;
    let symbol = values::install_charsxp(which.element(0))?;
    set_attribute_checked(object, symbol, value, call.call)?;
    Ok(object)
}

/// `attr(x, which) <- value` from a complex assignment, whose target is already unshared, for
/// any attribute but the dims, which need `dim<-`'s checks, and a class the full call would
/// refuse.
unsafe fn direct_set_attr(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let &[object, which, value] = args else {
        return Ok(None);
    };
    if which.kind() != SexpType::String
        || length(which) != 1
        || which.element(0) == values::na_string()
        || object == nil()
        || is_native_value(object)
        || is_native_value(value)
    {
        return Ok(None);
    }
    let symbols = symbols();
    let object = modifiable(object)?;
    let symbol = values::install_charsxp(which.element(0))?;
    if symbol == symbols.names {
        set_names(object, value)?;
    } else if symbol == symbols.row_names {
        set_row_names(object, value)?;
    } else if symbol == symbols.class {
        let valid = value == nil()
            || (value.kind() == SexpType::String
                && (object.kind() == SexpType::Integer
                    || !(0..length(value)).any(|index| value.element(index).bytes() == b"factor")));
        if !valid {
            return Ok(None);
        }
        dispatch::set_class_attribute(object, value)?;
    } else if symbol == symbols.dim || symbol == symbols.dimnames {
        return Ok(None);
    } else {
        values::set_attr(object, symbol, value)?;
    }
    Ok(Some(object))
}

// `attributes`.

unsafe fn do_attributes(call: &Call<'_>) -> R<Sexp> {
    attributes(call.arg(0))
}

/// `attributes(x)`: a named list of the attributes, which shares the tags' print names as its
/// names.
unsafe fn attributes(object: Sexp) -> R<Sexp> {
    let symbols = symbols();
    let mut items: SmallVec<[(Sexp, Sexp); 8]> = SmallVec::new();
    // GNU R reports tag names only for a pairlist (`isList`): a call's tags are its argument
    // names, which `names()` exposes but `attributes()` does not.
    if object.kind() == SexpType::Pairlist {
        let names = values::pairlist_names(object)?;
        if names != nil() {
            crate::heap().root(names);
            items.push((symbols.names.car(), names));
        }
    }
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        let tag = cell.tag();
        if tag == symbols.row_names {
            let expanded = expand_row_names(cell.car())?;
            crate::heap().root(expanded);
            items.push((tag.car(), expanded));
        } else if tag != symbols.names || object.kind() != SexpType::Environment {
            items.push((tag.car(), cell.car()));
        }
        cell = cell.cdr();
    }
    if items.is_empty() {
        return Ok(nil());
    }
    let result = alloc(SexpType::List, items.len())?;
    crate::heap().root(result);
    let names = alloc(SexpType::String, items.len())?;
    for (index, &(name, value)) in items.iter().enumerate() {
        values::set_elt(result, index, value);
        values::set_elt(names, index, name);
    }
    values::set_attr(result, symbols.names, names)?;
    Ok(result)
}

// `names`, `names<-` and `dim`.

unsafe fn do_names(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() == SexpType::Environment {
        // GNU R: `names(env)` is `ls(env, all.names = TRUE, sorted = FALSE)`.
        return crate::envir::env_names(object, true, false);
    }
    Ok(values::names(object))
}

unsafe fn do_set_names(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let names = call.arg(1);
    if object == nil() {
        if names == nil() {
            return Ok(nil());
        }
        return Err(call.error("attempt to set an attribute on NULL"));
    }
    if names == nil() && get_attr(object, symbols().names) == nil() {
        return Ok(object);
    }
    if object.kind() == SexpType::Object {
        let class = get_attr(object, symbols().class);
        let class = if class == nil() {
            "S4".to_string()
        } else {
            values::char_str(class.element(0)).to_string()
        };
        return Err(call.error(format!(
            "invalid to use names()<- on an S4 object of class '{class}'"
        )));
    }
    let object = modifiable(object)?;
    // GNU R's `names<-` turns any value but a plain character vector into text with a
    // dispatched `as.character(value)`, so a Date names by its formatted dates. The value goes
    // into the call as it is, which evaluates a symbol or a call there, as in GNU R.
    let names = if names != nil() && !(names.kind() == SexpType::String && names.attrib() == nil())
    {
        crate::heap().root(object);
        let function = values::install("as.character")?;
        eval::eval(values::call(function, &[names])?, call.env()?)?
    } else {
        names
    };
    set_names(object, names)?;
    Ok(object)
}

/// `names(x) <- value` from a complex assignment for a plain vector and a plain character
/// `value` or `NULL`, which need no conversion.
unsafe fn direct_set_names(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let &[object, names] = args else {
        return Ok(None);
    };
    let vector = object.kind().is_vector() && object.kind() != SexpType::Char;
    let text = names == nil() || (names.kind() == SexpType::String && names.attrib() == nil());
    if !vector || !plain(object) || !text || object.is_altrep() {
        return Ok(None);
    }
    if names == nil() && get_attr(object, symbols().names) == nil() {
        return Ok(Some(object));
    }
    let object = modifiable(object)?;
    set_names(object, names)?;
    Ok(Some(object))
}

unsafe fn do_dim(call: &Call<'_>) -> R<Sexp> {
    Ok(get_attr(call.arg(0), symbols().dim))
}

// `class`, `oldClass` and `inherits`.

unsafe fn do_class(call: &Call<'_>) -> R<Sexp> {
    dispatch::class_of(call.arg(0))
}

unsafe fn do_old_class(call: &Call<'_>) -> R<Sexp> {
    Ok(get_attr(call.arg(0), symbols().class))
}

/// The classes `inherits` tests, which are `class(x)`'s, or for an S4 object its class and every
/// class it extends, without allocating for an object that has no class attribute.
enum Classes {
    Vector(Sexp),
    Fixed(&'static [&'static str]),
    Basic(&'static str),
}

impl Classes {
    unsafe fn of(object: Sexp) -> R<Classes> {
        if object.has_gp(gp::S4_OBJECT) {
            return dispatch::s4_class_with_superclasses(object).map(Classes::Vector);
        }
        let class = values::class_attr(object);
        if length(class) > 0 {
            return Ok(Classes::Vector(class));
        }
        let dim = get_attr(object, symbols().dim);
        if dim != nil() {
            return Ok(Classes::Fixed(if length(dim) == 2 {
                &["matrix", "array"]
            } else {
                &["array"]
            }));
        }
        Ok(Classes::Basic(basic_class(object, false)))
    }

    /// The zero-based position of the class whose text is the `CHARSXP` `wanted`.
    unsafe fn position(&self, wanted: Sexp) -> Option<usize> {
        match *self {
            Classes::Vector(classes) => {
                (0..length(classes)).find(|&index| same_text(classes.element(index), wanted))
            }
            Classes::Fixed(classes) => {
                let bytes = wanted.bytes();
                classes.iter().position(|class| class.as_bytes() == bytes)
            }
            Classes::Basic(class) => (class.as_bytes() == wanted.bytes()).then_some(0),
        }
    }
}

pub(crate) unsafe fn do_inherits(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let what = call.arg(1);
    let which = values::as_logical_scalar(call.arg(2)) == 1;
    let classes = Classes::of(object)?;
    if let Classes::Vector(vector) = classes {
        crate::heap().root(vector);
    }
    // A class generator (an R6 or S4 generator, say) names its class through `nameOfClass()`.
    let what = if what.kind() == SexpType::String {
        what
    } else {
        let generic = values::install("nameOfClass")?;
        let quoted = values::call(symbols().quote, &[what])?;
        let name_call = values::call(generic, &[quoted])?;
        let name = eval::eval(name_call, values::base_namespace())?;
        if name.kind() != SexpType::String {
            return Err(call.error(
                "'what' must be a character vector or an object with a nameOfClass() method",
            ));
        }
        name
    };
    inherits(&classes, what, which)
}

/// `inherits(x, what, which)` for a character `what` and an `x` that is not an S4 object, which
/// `base_fast_paths` calls for the closure `inherits`.
pub(crate) unsafe fn plain_inherits(object: Sexp, what: Sexp, which: bool) -> R<Sexp> {
    inherits(&Classes::of(object)?, what, which)
}

unsafe fn inherits(classes: &Classes, what: Sexp, which: bool) -> R<Sexp> {
    if which {
        let result = alloc(SexpType::Integer, length(what))?;
        for index in 0..length(what) {
            values::ints_mut(result)[index] = classes
                .position(what.element(index))
                .map_or(0, |position| position as i32 + 1);
        }
        return Ok(result);
    }
    let found = (0..length(what)).any(|index| classes.position(what.element(index)).is_some());
    values::scalar_bool(found)
}
