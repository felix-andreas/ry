//! Attribute reads and writes (`attr`, `attr<-`, `attributes`, `names`, `names<-`, `dim`) and
//! the class queries (`class`, `oldClass`, `inherits`), each with the direct or unary entry a
//! compiled call takes for plain values.
//!
//! Attribute tags are symbols whose print names are interned `CHARSXP`s, so a lookup by name
//! compares identities (see [`same_chars`]) and reads bytes only for a partial match.

use {
    crate::{
        builtins::{BuiltinDef, Generic, builtin, internal},
        handles::{
            self, Args, Chars, Cx, Gc, Integer, Local, Logical, Str, Symbol, attribute_cells,
            attributes_unshared, base_namespace, basic_class, call_function, class_of, class_read,
            dense, environment_names, get_attribute, install_name_at, integer_sequence, is_na,
            is_object, is_s4, length, logical_scalar, names_of, null, quoted, s4_classes, safe,
            safe_direct, safe_unary, set_attribute, set_class, stored_names, symbol, text,
        },
        subset::{assign_attribute, assign_names, assign_row_names},
        unwind::{R, error},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp},
    smallvec::SmallVec,
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin("attr", -1, safe!(do_attr)).direct(safe_direct!(direct_attr)),
    builtin("attr<-", 3, safe!(do_set_attr)).direct(safe_direct!(direct_set_attr)),
    builtin("attributes", 1, safe!(do_attributes)).unary(safe_unary!(unary_attributes)),
    builtin("names", 1, safe!(do_names))
        .generic(Generic::Internal)
        .unary(safe_unary!(unary_names)),
    builtin("names<-", 2, safe!(do_set_names))
        .generic(Generic::Internal)
        .direct(safe_direct!(direct_set_names)),
    builtin("dim", 1, safe!(do_dim))
        .generic(Generic::Internal)
        .unary(safe_unary!(unary_dim)),
    builtin("class", 1, safe!(do_class)).unary(safe_unary!(unary_class)),
    builtin("oldClass", 1, safe!(do_old_class)).unary(safe_unary!(unary_old_class)),
    internal("inherits", 3, safe!(do_inherits)),
];

/// Whether two `CHARSXP`s hold the same text, for raw callers.
///
/// # Safety
///
/// Both are live `CHARSXP`s.
#[inline]
pub(crate) unsafe fn same_text(left: Sexp, right: Sexp) -> bool {
    same_chars(Gc::adopt(left), Gc::adopt(right))
}

/// Whether two strings hold the same bytes. Interned ASCII strings are equal exactly when they
/// are the same object, so bytes are compared only when either side is not ASCII.
#[inline]
fn same_chars(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
    left.value().is(right.value())
        || (!(left.has_mark(gp::ASCII) && right.has_mark(gp::ASCII))
            && left.bytes() == right.bytes())
}

/// GNU R's `do_attr` lookup of the attribute named by the first element of `which`: a full
/// match on a tag wins; otherwise, unless `exact`, a unique prefix of one tag, with `names`
/// special because a pairlist's names are not stored as an attribute. The value is
/// `getAttrib`'s, which expands compact row names and reads a pairlist's or a one-dimensional
/// array's names.
pub fn attribute<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    which: Local<'s, Str>,
    exact: bool,
) -> R<Local<'s>> {
    match lookup(context, object, which.get(context, 0), exact) {
        Lookup::Value(value) => Ok(context.root(value)),
        Lookup::Names => names_of(context, object),
        Lookup::RowNames(value) => {
            let value = context.root(value);
            expand_row_names(context, value)
        }
        Lookup::Unnamed { value, row_names } => {
            let value = context.root(value);
            if names_of(context, object)?.r_type() != SexpType::Nil {
                return Ok(null());
            }
            if row_names {
                expand_row_names(context, value)
            } else {
                Ok(value)
            }
        }
        Lookup::Missing => Ok(null()),
    }
}

/// Where [`lookup`] finds the answer of `attr`.
enum Lookup<'a> {
    /// The stored value.
    Value(Gc<'a>),
    /// The object's names, which `names_of` reads.
    Names,
    /// The row names as stored, which `expand_row_names` reads.
    RowNames(Gc<'a>),
    /// The value found for a prefix of `names`, which is the answer only when the object has
    /// no names, because the prefix is ambiguous otherwise. It may be the stored row names.
    Unnamed {
        value: Gc<'a>,
        row_names: bool,
    },
    Missing,
}

impl<'a> Lookup<'a> {
    #[inline]
    fn stored(tag: Gc<'_, Symbol>, value: Gc<'a>) -> Lookup<'a> {
        if tag.is(symbol(|symbols| symbols.names)) {
            Lookup::Names
        } else if tag.is(symbol(|symbols| symbols.row_names)) {
            Lookup::RowNames(value)
        } else {
            Lookup::Value(value)
        }
    }
}

/// The attribute of `object` that `attr` reads for the name `which`, without reading the
/// names or expanding the row names, which may allocate.
#[inline]
fn lookup<'a, 's: 'a>(
    context: &'a Cx<'_>,
    object: Local<'s>,
    which: Gc<'_, Chars>,
    exact: bool,
) -> Lookup<'a> {
    let wanted = which.bytes();
    // One pass finds both kinds of match. Only a strictly longer tag can be a partial match,
    // because an equal-length prefix is a full match, so tags of the wanted length (every tag,
    // in a vector used as a map from fixed-width keys) cost one identity compare and no memcmp.
    let mut partial = None;
    let mut ambiguous = false;
    for (tag, value) in attribute_cells(context, object) {
        let Some(tag) = tag.downcast::<Symbol>() else {
            continue;
        };
        let name = tag.name();
        if same_chars(name, which) {
            return Lookup::stored(tag, value);
        }
        if !exact {
            let tag_text = name.bytes();
            if tag_text.len() > wanted.len() && tag_text.starts_with(wanted) {
                ambiguous |= partial.is_some();
                partial = Some((tag, value));
            }
        }
    }
    if wanted == b"names" {
        return Lookup::Names;
    }
    if exact || ambiguous {
        return Lookup::Missing;
    }
    let names = symbol(|symbols| symbols.names);
    match partial {
        None if b"names".starts_with(wanted) => Lookup::Names,
        None => Lookup::Missing,
        // A prefix of both `names` and another tag is ambiguous when names exist.
        Some((tag, value)) if b"names".starts_with(wanted) && !tag.is(names) => Lookup::Unnamed {
            value,
            row_names: tag.is(symbol(|symbols| symbols.row_names)),
        },
        Some((tag, value)) => Lookup::stored(tag, value),
    }
}

/// The attributes whose stored value is not the value `getAttrib` reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Names,
    RowNames,
    Other,
}

impl Tag {
    fn of(tag: Gc<'_, Symbol>) -> Tag {
        if tag.is(symbol(|symbols| symbols.names)) {
            Tag::Names
        } else if tag.is(symbol(|symbols| symbols.row_names)) {
            Tag::RowNames
        } else {
            Tag::Other
        }
    }
}

/// Compact row names (`c(NA, -n)`) read back as `1:n`.
pub fn expand_row_names<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s>> {
    // Any other length is the row names themselves, which a long data frame keeps as a compact
    // sequence that `dense` would copy out.
    if value.r_type() != SexpType::Integer || length(context, value) != 2 {
        return Ok(value);
    }
    let Some(rows) = dense::<Integer>(context, value)? else {
        return Ok(value);
    };
    let count = match rows.as_slice(context) {
        &[NA_INTEGER, count] => count.unsigned_abs() as usize,
        _ => return Ok(value),
    };
    // GNU R's `getAttrib` gives `1:n` as a compact sequence, so reading the row names of a long
    // data frame allocates nothing.
    if count > 0
        && let Some(sequence) = integer_sequence(context, count)?
    {
        return Ok(sequence);
    }
    let mut result = context.alloc::<Integer>(count)?;
    for (index, slot) in result.as_mut_slice().iter_mut().enumerate() {
        *slot = index as i32 + 1;
    }
    Ok(result.finish().value())
}

/// Whether `object` is a ry record or `I64`, which the attribute builtins refuse.
#[inline]
fn is_native(object: Local<'_>) -> bool {
    matches!(object.r_type(), SexpType::Record | SexpType::I64)
}

/// Whether `object` is a value the attribute builtins answer without dispatch or a refusal.
#[inline]
fn plain(object: Local<'_>) -> bool {
    !is_object(object) && !is_native(object)
}

// `attr` and `attr<-`.

fn do_attr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    if !(2..=3).contains(&arguments.len()) {
        return Err(arguments.error("either 2 or 3 arguments are required"));
    }
    let [object, which, exact] = arguments.matched(context, ["x", "which", "exact"])?;
    let object = object.unwrap_or_else(null);
    let which = which.unwrap_or_else(null);
    let Some(which) = dense::<Str>(context, which)? else {
        return Err(arguments.error("'which' must be of mode character"));
    };
    if which.len() != 1 {
        return Err(arguments.error("exactly one attribute 'which' must be given"));
    }
    let exact = exact.is_some_and(|exact| logical_scalar(context, exact) == 1);
    attribute(context, object, which, exact)
}

/// `attr(x, which)` and `attr(x, which, exact)` for one name and a logical `exact`.
fn direct_attr<'a, 's>(context: &'a mut Cx<'s>, values: &[Local<'s>]) -> R<Option<Gc<'a>>> {
    let (object, which) = match values {
        [object, which] | [object, which, _] => (*object, *which),
        _ => return Ok(None),
    };
    let Some(which) = which.standard::<Str>() else {
        return Ok(None);
    };
    if which.len() != 1 || is_native(object) {
        return Ok(None);
    }
    let exact = match values.get(2) {
        None => false,
        Some(exact) => match exact.standard::<Logical>() {
            Some(exact) if exact.len() == 1 => {
                let value = exact.as_slice(context)[0];
                value != NA_LOGICAL && value != 0
            }
            _ => return Ok(None),
        },
    };
    context.read_or(
        |context| match lookup(context, object, which.get(context, 0), exact) {
            Lookup::Value(value) => Some(value),
            Lookup::Missing => Some(null().read(context)),
            _ => None,
        },
        |context| {
            Ok(Some(
                attribute(context, object, which, exact)?.read(context),
            ))
        },
    )
}

fn do_set_attr<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let [object, which, value] = arguments.matched(context, ["x", "which", "value"])?;
    let object = object.unwrap_or_else(null);
    let which = which.unwrap_or_else(null);
    let value = value.unwrap_or_else(null);
    // GNU R's `isValidString`, then its check for one name, which R 4.6 added.
    let which = match dense::<Str>(context, which)? {
        Some(which) if !which.is_empty() && !is_na(which.get(context, 0)) => which,
        _ => return Err(arguments.error("'name' must be non-null character string")),
    };
    if which.len() != 1 && crate::system::r_at_least(4, 6) {
        return Err(arguments.error("'name' must be a scalar string"));
    }
    if object.r_type() == SexpType::Nil {
        if value.r_type() == SexpType::Nil {
            return Ok(null());
        }
        return Err(arguments.error("attempt to set an attribute on NULL"));
    }
    let mut object = attributes_unshared(context, object)?;
    let name = install_name_at(context, which, 0)?;
    assign_attribute(
        context,
        &mut object,
        name,
        value,
        Some(arguments.expression()),
    )?;
    Ok(object.finish())
}

/// `attr(x, which) <- value` from a complex assignment, whose target is already unshared, for
/// any attribute but the dims, which need `dim<-`'s checks, and a class the full call would
/// refuse.
fn direct_set_attr<'a, 's>(context: &'a mut Cx<'s>, values: &[Local<'s>]) -> R<Option<Gc<'a>>> {
    let &[object, which, value] = values else {
        return Ok(None);
    };
    // An ALTREP `which` takes the full call, which expands it.
    let Some(which) = which.standard::<Str>() else {
        return Ok(None);
    };
    if which.len() != 1
        || object.r_type() == SexpType::Nil
        || is_native(object)
        || is_native(value)
        || is_na(which.get(context, 0))
    {
        return Ok(None);
    }
    let mut object = attributes_unshared(context, object)?;
    let name = install_name_at(context, which, 0)?;
    if name.is(symbol(|symbols| symbols.names)) {
        assign_names(context, &mut object, value)?;
    } else if name.is(symbol(|symbols| symbols.row_names)) {
        assign_row_names(context, &mut object, value)?;
    } else if name.is(symbol(|symbols| symbols.class)) {
        let valid = match value.r_type() {
            SexpType::Nil => true,
            SexpType::String => {
                object.get().r_type() == SexpType::Integer || !names_factor(context, value)?
            }
            _ => false,
        };
        if !valid {
            return Ok(None);
        }
        set_class(context, &mut object, value)?;
    } else if name.is(symbol(|symbols| symbols.dim)) || name.is(symbol(|symbols| symbols.dimnames))
    {
        return Ok(None);
    } else {
        set_attribute(context, &mut object, name, value)?;
    }
    Ok(Some(object.finish().read(context)))
}

/// Whether the character vector `classes` names `"factor"`.
pub fn names_factor<'s>(context: &mut Cx<'s>, classes: Local<'s>) -> R<bool> {
    let Some(classes) = dense::<Str>(context, classes)? else {
        return Ok(false);
    };
    Ok(classes
        .iter(context)
        .any(|class| class.bytes() == b"factor"))
}

// `attributes`.

fn do_attributes<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    attributes(context, arguments.value(0))
}

fn unary_attributes<'a, 's>(context: &'a mut Cx<'s>, object: Local<'s>) -> R<Option<Gc<'a>>> {
    if is_native(object) {
        return Ok(None);
    }
    Ok(Some(attributes(context, object)?.read(context)))
}

/// `attributes(x)`: a named list of the attributes, which shares the tags' print names as its
/// names.
fn attributes<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    let names_symbol = symbol(|symbols| symbols.names);
    // GNU R reports tag names only for a pairlist (`isList`): a call's tags are its argument
    // names, which `names()` exposes but `attributes()` does not.
    let tags = if object.r_type() == SexpType::Pairlist {
        let names = names_of(context, object)?;
        (names.r_type() != SexpType::Nil).then_some(names)
    } else {
        None
    };
    // An environment's `names` attribute is not listed, as `names(env)` is its variables.
    let environment = object.r_type() == SexpType::Environment;
    let listed = move |tag: Gc<'_, Symbol>| !environment || Tag::of(tag) != Tag::Names;
    let mut count = 0;
    let mut row_names = None;
    for (tag, value) in attribute_cells(context, object) {
        let Some(tag) = tag.downcast::<Symbol>() else {
            continue;
        };
        if listed(tag) {
            count += 1;
            // GNU R reads the row names by name for each such cell, so the first one answers.
            if Tag::of(tag) == Tag::RowNames && row_names.is_none() {
                row_names = Some(context.root(value));
            }
        }
    }
    let row_names = match row_names {
        Some(value) => {
            let expanded = expand_row_names(context, value)?;
            // Reading row names kept as an ALTREP vector runs its package's code, which may
            // change the attributes, so the cells are counted again after it.
            count = attribute_cells(context, object)
                .filter(|(tag, _)| tag.downcast::<Symbol>().is_some_and(listed))
                .count();
            Some(expanded)
        }
        None => None,
    };
    let count = count + usize::from(tags.is_some());
    if count == 0 {
        return Ok(null());
    }
    let mut result = context.alloc::<handles::List>(count)?;
    let mut names = context.alloc::<Str>(count)?;
    // The object holds its attribute cells, so they are read again after the allocations
    // instead of being rooted one by one before them.
    let mut index = 0;
    if let Some(tags) = tags {
        result.set(context, 0, tags);
        names.set(context, 0, names_symbol.name());
        index = 1;
    }
    for (tag, value) in attribute_cells(context, object) {
        let Some(tag) = tag.downcast::<Symbol>() else {
            continue;
        };
        if !listed(tag) {
            continue;
        }
        match row_names {
            Some(expanded) if Tag::of(tag) == Tag::RowNames => result.set(context, index, expanded),
            _ => result.set(context, index, value),
        }
        names.set(context, index, tag.name());
        index += 1;
    }
    let mut result = result.owned();
    set_attribute(context, &mut result, names_symbol, names.finish().value())?;
    Ok(result.finish().value())
}

// `names`, `names<-` and `dim`.

fn do_names<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    if let Some(environment) = object.downcast() {
        // GNU R: `names(env)` is `ls(env, all.names = TRUE, sorted = FALSE)`.
        return environment_names(context, environment);
    }
    names_of(context, object)
}

fn unary_names<'a, 's>(context: &'a mut Cx<'s>, object: Local<'s>) -> R<Option<Gc<'a>>> {
    if !plain(object) || object.r_type() == SexpType::Environment {
        return Ok(None);
    }
    let tagged = matches!(
        object.r_type(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    );
    context.read_or(
        |context| {
            if tagged {
                None
            } else {
                stored_names(context, object)
            }
        },
        |context| Ok(Some(names_of(context, object)?.read(context))),
    )
}

fn do_set_names<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let names = arguments.value(1);
    if object.r_type() == SexpType::Nil {
        if names.r_type() == SexpType::Nil {
            return Ok(null());
        }
        return Err(arguments.error("attempt to set an attribute on NULL"));
    }
    // A call or pairlist keeps its names as tags, not as a `names` attribute.
    if names.r_type() == SexpType::Nil
        && get_attribute(context, object, symbol(|symbols| symbols.names)).r_type() == SexpType::Nil
        && !matches!(object.r_type(), SexpType::Pairlist | SexpType::Language)
    {
        return Ok(object);
    }
    if object.r_type() == SexpType::Object {
        let class = context.root(get_attribute(
            context,
            object,
            symbol(|symbols| symbols.class),
        ));
        let class = match dense::<Str>(context, class)? {
            Some(class) if !class.is_empty() => text(class.get(context, 0)).into_owned(),
            _ => "S4".to_string(),
        };
        return Err(arguments.error(format!(
            "invalid to use names()<- on an S4 object of class '{class}'"
        )));
    }
    let mut object = attributes_unshared(context, object)?;
    // GNU R's `names<-` turns any value but a plain character vector into text with a
    // dispatched `as.character(value)`, so a Date names by its formatted dates. The value goes
    // into the call as it is, which evaluates a symbol or a call there, as in GNU R.
    let names = if names.r_type() != SexpType::Nil
        && !(names.r_type() == SexpType::String
            && names.attributes(context).r_type() == SexpType::Nil)
    {
        let environment = arguments.environment(context)?;
        call_function(context, "as.character", &[names], environment)?
    } else {
        names
    };
    assign_names(context, &mut object, names)?;
    Ok(object.finish())
}

/// `names(x) <- value` from a complex assignment for a plain vector and a plain character
/// `value` or `NULL`, which need no conversion.
fn direct_set_names<'a, 's>(context: &'a mut Cx<'s>, values: &[Local<'s>]) -> R<Option<Gc<'a>>> {
    let &[object, names] = values else {
        return Ok(None);
    };
    let vector = object.r_type().is_vector() && object.r_type() != SexpType::Char;
    let text = names.r_type() == SexpType::Nil
        || (names.r_type() == SexpType::String
            && names.attributes(context).r_type() == SexpType::Nil);
    if !vector || !plain(object) || !text || object.downcast::<handles::Alt>().is_some() {
        return Ok(None);
    }
    if names.r_type() == SexpType::Nil
        && get_attribute(context, object, symbol(|symbols| symbols.names)).r_type() == SexpType::Nil
    {
        return Ok(Some(object.read(context)));
    }
    let mut object = attributes_unshared(context, object)?;
    assign_names(context, &mut object, names)?;
    Ok(Some(object.finish().read(context)))
}

fn do_dim<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    Ok(context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dim),
    )))
}

fn unary_dim<'a, 's>(context: &'a mut Cx<'s>, object: Local<'s>) -> R<Option<Gc<'a>>> {
    if !plain(object) {
        return Ok(None);
    }
    let dim = symbol(|symbols| symbols.dim);
    Ok(Some(get_attribute(context, object, dim)))
}

// `class`, `oldClass` and `inherits`.

fn do_class<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    class_of(context, arguments.value(0))
}

fn unary_class<'a, 's>(context: &'a mut Cx<'s>, object: Local<'s>) -> R<Option<Gc<'a>>> {
    if is_native(object) {
        return Ok(None);
    }
    class_read(context, object).map(Some)
}

fn do_old_class<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    Ok(context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.class),
    )))
}

fn unary_old_class<'a, 's>(context: &'a mut Cx<'s>, object: Local<'s>) -> R<Option<Gc<'a>>> {
    if is_native(object) {
        return Ok(None);
    }
    let class = symbol(|symbols| symbols.class);
    Ok(Some(get_attribute(context, object, class)))
}

/// The classes `inherits` tests, which are `class(x)`'s, or for an S4 object its class and every
/// class it extends, without allocating for an object that has no class attribute.
enum Classes<'a> {
    Vector(Gc<'a, Str>),
    Fixed(&'static [&'static str]),
    Basic(&'static str),
}

impl<'a> Classes<'a> {
    /// The classes of `object` read where they are stored, or `None` when only a safepoint
    /// gives them: an S4 object's superclasses, and a class or `dim` that is not a standard
    /// vector.
    #[inline]
    fn stored<'s: 'a>(context: &'a Cx<'_>, object: Local<'s>) -> Option<Classes<'a>> {
        if is_s4(object) {
            return None;
        }
        if is_object(object) {
            let class = get_attribute(context, object, symbol(|symbols| symbols.class));
            match class.standard::<Str>() {
                Some(class) if !class.is_empty() => return Some(Classes::Vector(class)),
                Some(_) => {}
                None if class.r_type() == SexpType::Nil => {}
                None => return None,
            }
        }
        let dim = get_attribute(context, object, symbol(|symbols| symbols.dim));
        if dim.r_type() == SexpType::Nil {
            return Some(Classes::Basic(basic_class(object)));
        }
        let dim = dim.standard::<Integer>()?;
        Some(Classes::Fixed(if dim.len() == 2 {
            &["matrix", "array"]
        } else {
            &["array"]
        }))
    }

    /// The zero-based position of the class whose text is `wanted`.
    #[inline]
    fn position(&self, wanted: Gc<'_, Chars>) -> Option<usize> {
        match *self {
            Classes::Vector(classes) => {
                (0..classes.len()).position(|index| same_chars(classes.get(index), wanted))
            }
            Classes::Fixed(classes) => {
                let bytes = wanted.bytes();
                classes.iter().position(|class| class.as_bytes() == bytes)
            }
            Classes::Basic(class) => (class.as_bytes() == wanted.bytes()).then_some(0),
        }
    }
}

/// The answer of `body` over the classes of `object`, which are read where they are stored
/// when [`Classes::stored`] can, so the usual `inherits` roots and allocates nothing.
#[inline]
fn over_classes<'s, T>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    body: impl for<'a> FnOnce(&'a Cx<'s>, &Classes<'a>) -> T,
) -> R<T> {
    if let Some(classes) = Classes::stored(context, object) {
        return Ok(body(context, &classes));
    }
    let classes = if is_s4(object) {
        s4_classes(context, object)?
    } else {
        class_of(context, object)?
    };
    let classes = dense::<Str>(context, classes)?;
    let classes = classes.map_or(Classes::Fixed(&[]), |classes| {
        Classes::Vector(classes.read(context))
    });
    Ok(body(context, &classes))
}

fn do_inherits<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let what = arguments.value(1);
    let which = logical_scalar(context, arguments.value(2)) == 1;
    // A class generator (an R6 or S4 generator, say) names its class through `nameOfClass()`.
    let what = if what.r_type() == SexpType::String {
        what
    } else {
        let quoted = quoted(context, what)?;
        let name = call_function(context, "nameOfClass", &[quoted], base_namespace())?;
        if name.r_type() != SexpType::String {
            return Err(arguments.error(
                "'what' must be a character vector or an object with a nameOfClass() method",
            ));
        }
        name
    };
    inherits(context, object, what, which)
}

/// `inherits(x, what, which)` for a character `what` and an `x` that is not an S4 object, which
/// `base_fast_paths` calls for the closure `inherits`.
///
/// # Safety
///
/// The caller is on the interpreter's thread, holds no reference into the heap, keeps both
/// values alive until this returns, and roots the result before it allocates.
pub(crate) unsafe fn plain_inherits(object: Sexp, what: Sexp, which: bool) -> R<Sexp> {
    handles::with(|context| {
        let object = handles::held(context, object);
        let what = handles::held(context, what);
        Ok(inherits(context, object, what, which)?.raw())
    })
}

#[inline]
fn inherits<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    what: Local<'s>,
    which: bool,
) -> R<Local<'s>> {
    let what = dense::<Str>(context, what)?
        .ok_or_else(|| error("internal error: 'what' is not a character vector"))?;
    if which {
        let positions: SmallVec<[i32; 8]> = over_classes(context, object, |context, classes| {
            what.iter(context)
                .map(|class| classes.position(class).map_or(0, |index| index as i32 + 1))
                .collect()
        })?;
        let mut result = context.alloc::<Integer>(positions.len())?;
        result.as_mut_slice().copy_from_slice(&positions);
        return Ok(result.finish().value());
    }
    let found = over_classes(context, object, |context, classes| {
        what.iter(context)
            .any(|class| classes.position(class).is_some())
    })?;
    handles::logical_constant(context, i32::from(found))
}
