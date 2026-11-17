//! The interpreter's side of [`heap::handle`]: how a builtin written in safe Rust is entered, how
//! its arguments become handles, and safe forms of the value helpers such a builtin calls.
//!
//! A helper here wraps its raw counterpart in `values` or `coerce` and states why the handle
//! types make the call sound. `docs/src/content/docs/development/design/memory-safety.md` is
//! the design.

#![deny(unsafe_op_in_unsafe_fn)]

pub use heap::handle::{
    Alt, Atomic, Chars, Complex, Cx, Elements, Expression, Fresh, Gc, Identity, Integer, Kind,
    List, Local, Logical, Pointer, Raw, Real, Str, Symbol, Value, Vector,
};
use {
    crate::{
        builtins::Call,
        coerce::coerce_vector,
        conditions,
        unwind::{R, Unwind, error},
        values,
    },
    heap::{
        Heap,
        handle::Reroot,
        object::{Sexp, SexpType, gp},
    },
    std::{borrow::Cow, marker::PhantomData, ptr::NonNull},
};

/// A builtin in safe Rust: it reaches the heap only through the context, and its arguments and
/// result are handles of the call's scope.
pub type SafeFn = for<'s> fn(&mut Cx<'s>, &Args<'_, 's>) -> R<Local<'s>>;

/// The raw entry of the safe builtin `$function`, for the builtin table.
macro_rules! safe {
    ($function:path) => {{
        unsafe fn entry(
            call: &$crate::builtins::Call<'_>,
        ) -> $crate::unwind::R<heap::object::Sexp> {
            // SAFETY: the evaluator calls a builtin on the interpreter's thread, with the
            // call's arguments rooted.
            unsafe { $crate::handles::enter(call, $function) }
        }
        entry
    }};
}
pub(crate) use safe;

/// Runs the safe builtin `body` for `call`.
///
/// # Safety
///
/// The caller is on the interpreter's thread, holds no reference into the heap, and keeps the
/// call's arguments rooted until this returns.
#[inline]
pub unsafe fn enter(
    call: &Call<'_>,
    body: impl for<'s> FnOnce(&mut Cx<'s>, &Args<'_, 's>) -> R<Local<'s>>,
) -> R<Sexp> {
    // SAFETY: the interpreter's heap lives as long as the process, and `body` cannot keep the
    // context or anything of its scope, because it must accept every `'s`.
    let mut context = unsafe { Cx::enter(NonNull::from(crate::heap())) };
    let arguments = Args {
        call,
        scope: PhantomData,
    };
    body(&mut context, &arguments).map(Local::raw)
}

/// The arguments of the call a safe builtin runs for.
pub struct Args<'a, 's> {
    call: &'a Call<'a>,
    scope: PhantomData<fn(&'s ()) -> &'s ()>,
}

impl<'s> Args<'_, 's> {
    pub fn len(&self) -> usize {
        self.call.args.len()
    }

    pub fn is_empty(&self) -> bool {
        self.call.args.is_empty()
    }

    /// The argument at `index`, or `NULL` past the last one.
    #[inline]
    pub fn value(&self, index: usize) -> Local<'s> {
        // SAFETY: `enter`'s caller keeps every argument rooted for the call, which outlasts
        // `'s`.
        unsafe { Local::adopt(self.call.arg(index)) }
    }

    /// An error reported against the call.
    pub fn error(&self, message: impl Into<String>) -> Unwind {
        self.call.error(message)
    }
}

// SAFETY: `Unwind::root` roots the call an error names and every value a jump carries.
unsafe impl Reroot for Unwind {
    fn reroot(&self, _heap: &mut Heap) {
        self.root();
    }
}

/// Roots the result of a raw call in the context's scope.
///
/// # Safety
///
/// `raw` is a live object, and nothing has allocated since the raw call returned it.
#[inline]
unsafe fn rooted<'s>(context: &Cx<'s>, raw: Sexp) -> Local<'s> {
    // SAFETY: the caller's contract, and a `Value` handle admits every object.
    context.root(unsafe { Gc::<Value>::adopt(raw) })
}

/// Whether the class attribute of `value` names `class`.
#[inline]
pub fn inherits(_context: &Cx<'_>, value: impl Pointer<Value>, class: &str) -> bool {
    // SAFETY: the handle is a live object, and the test only reads its class attribute.
    unsafe { values::inherits(value.raw(), class) }
}

/// The first element of a character vector as text, or `None` for `NA`, an empty vector and
/// any other type. The text borrows the context mutably because an ALTREP class may compute
/// the element, and then only the borrow keeps it alive.
#[inline]
pub fn first_string<'a>(_context: &'a mut Cx<'_>, value: Local<'_>) -> Option<Cow<'a, str>> {
    // SAFETY: the vector is rooted, an element read may run its class's method, which
    // `&mut Cx` permits, and nothing collects while the text borrows the context.
    unsafe { values::first_string(value.raw()) }
}

/// The first element as a logical: `NA` for an empty vector or a type with no logical reading.
#[inline]
pub fn logical_scalar(_context: &mut Cx<'_>, value: Local<'_>) -> i32 {
    // SAFETY: the value is rooted, and reading an ALTREP's first element may allocate, which
    // `&mut Cx` permits.
    unsafe { values::as_logical_scalar(value.raw()) }
}

/// `value` as a standard vector of kind `T`: itself when it is one, its ALTREP class's
/// standard form when it is an ALTREP object of that type, and `None` for any other type.
#[inline]
pub fn dense<'s, T: Vector>(context: &mut Cx<'s>, value: Local<'s>) -> R<Option<Local<'s, T>>> {
    if value.r_type() != T::TYPE {
        return Ok(None);
    }
    match value.downcast::<T>() {
        Some(vector) => Ok(Some(vector)),
        None => expanded(context, value),
    }
}

/// The standard form of an ALTREP object of `T`'s type. It is compiled apart so that the test
/// for a standard vector inlines into each caller.
#[cold]
#[inline(never)]
fn expanded<'s, T: Vector>(context: &mut Cx<'s>, value: Local<'s>) -> R<Option<Local<'s, T>>> {
    // SAFETY: the object is rooted, and expanding it may allocate or run its class's
    // methods, which `&mut Cx` permits.
    let vector = unsafe { values::materialize(value.raw())? };
    // SAFETY: `materialize` returned a live object and nothing has allocated since.
    Ok(unsafe { rooted(context, vector) }.downcast::<T>())
}

/// `value` as a standard character vector, coerced as `as.character` coerces when it is of
/// another type.
#[inline]
pub fn character_vector<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Str>> {
    coerced(context, value)
}

/// `value` as a standard integer vector, coerced as `as.integer` coerces when it is of another
/// type.
#[inline]
pub fn integer_vector<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Integer>> {
    coerced(context, value)
}

#[inline]
fn coerced<'s, T: Vector>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, T>> {
    let vector = if value.r_type() == T::TYPE {
        value
    } else {
        // SAFETY: the value is rooted, and coercion allocates, which `&mut Cx` permits.
        let coerced = unsafe { coerce_vector(value.raw(), T::TYPE)? };
        // SAFETY: coercion returned a live object and nothing has allocated since.
        unsafe { rooted(context, coerced) }
    };
    dense::<T>(context, vector)?
        .ok_or_else(|| error("internal error: a coerced vector has no standard form"))
}

/// The first element as an integer: `NA` for an empty vector or a type with no integer reading.
#[inline]
pub fn integer_scalar(_context: &mut Cx<'_>, value: Local<'_>) -> i32 {
    // SAFETY: the value is rooted, and reading an ALTREP's first element may allocate, which
    // `&mut Cx` permits.
    unsafe { values::as_integer_scalar(value.raw()) }
}

/// A new integer vector of one element.
#[inline]
pub fn integer<'s>(context: &mut Cx<'s>, value: i32) -> R<Local<'s>> {
    let mut result = context.alloc::<Integer>(1)?;
    result.as_mut_slice().fill(value);
    Ok(result.finish().value())
}

/// Raises a warning against the running call.
#[inline]
pub fn warning(_context: &mut Cx<'_>, message: impl Into<String>) -> R<()> {
    // SAFETY: a warning may run a calling handler, which is R code and so needs the `&mut Cx`
    // this takes.
    unsafe { conditions::warning(message) }
}

/// A copy of `vector` with its own element storage, which the caller may fill or change. List
/// elements and attribute values are shared with the original.
pub fn shallow_duplicate<'s, T: Vector>(
    context: &mut Cx<'s>,
    vector: Local<'_, T>,
) -> R<Fresh<'s, T>> {
    // SAFETY: the vector is rooted, and copying allocates, which `&mut Cx` permits.
    let copy = unsafe { values::shallow_duplicate(vector.raw())? };
    // SAFETY: the copy of a standard vector is a new standard vector of its type that only
    // this pointer reaches, and nothing has allocated since it was made.
    Ok(unsafe { fresh(context, copy) })
}

/// A copy of `vector` all the way down: its own element storage, and its own copies of the
/// vectors and pairlists its elements and attributes hold.
pub fn duplicate<'s, T: Vector>(context: &mut Cx<'s>, vector: Local<'_, T>) -> R<Fresh<'s, T>> {
    // SAFETY: the vector is rooted, and copying allocates, which `&mut Cx` permits.
    let copy = unsafe { values::duplicate(vector.raw())? };
    // SAFETY: as for `shallow_duplicate`.
    Ok(unsafe { fresh(context, copy) })
}

/// A copy of `value` for a caller that changes only its attributes. For a long vector it is
/// an ALTREP wrapper that shares the elements, so the result is a plain value: reaching element
/// storage takes `dense`, and writing elements takes `shallow_duplicate`.
pub fn attribute_copy<'s>(context: &mut Cx<'s>, value: Local<'_>) -> R<Local<'s>> {
    // SAFETY: the value is rooted, and copying allocates, which `&mut Cx` permits.
    let copy = unsafe { values::shallow_duplicate_attr(value.raw())? };
    // SAFETY: the copy is a live object and nothing has allocated since it was made.
    Ok(unsafe { rooted(context, copy) })
}

/// Roots a new vector a raw call made and types it as one nothing else can see.
///
/// # Safety
///
/// `raw` is a live standard vector of type `T::TYPE` that no other pointer reaches, and nothing
/// has allocated since the raw call returned it.
#[inline]
unsafe fn fresh<'s, T: Vector>(context: &Cx<'s>, raw: Sexp) -> Fresh<'s, T> {
    // SAFETY: the caller's contract; `rooted` keeps the vector alive for `'s`.
    unsafe {
        rooted(context, raw);
        Fresh::adopt(raw)
    }
}

/// The symbol a string names, as GNU R's `installTrChar` gives it: a string in another
/// encoding names the symbol of its UTF-8 text. Two names are the same name exactly when their
/// symbols are the same object.
pub fn install<'s>(context: &mut Cx<'s>, name: Local<'_, Chars>) -> R<Local<'s, Symbol>> {
    // SAFETY: the string is rooted, and interning a symbol may allocate, which `&mut Cx`
    // permits.
    let symbol = unsafe { values::install_charsxp_text(name.raw())? };
    // SAFETY: `install` returned a live symbol and nothing has allocated since.
    unsafe { rooted(context, symbol) }
        .downcast::<Symbol>()
        .ok_or_else(|| error("internal error: install returned no symbol"))
}

/// Whether two strings hold the same text, whatever their encoding marks. `NA` equals only
/// `NA`.
#[inline]
pub fn same_text(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
    // SAFETY: both strings are live, and the comparison only reads them.
    unsafe { values::same_string(left.raw(), right.raw()) }
}

/// A string's text, decoded when it is marked latin1 or bytes.
#[inline]
pub fn text<'a>(chars: Gc<'a, Chars>) -> Cow<'a, str> {
    // SAFETY: the string is live for `'a`, which bounds the text borrowed from it.
    unsafe { values::char_str(chars.raw()) }
}

/// `NA_character_`, which the heap keeps for as long as it lives.
#[inline]
pub fn na() -> Gc<'static, Chars> {
    // SAFETY: the `NA` string is a permanent root of the heap.
    unsafe { Gc::adopt(values::na_string()) }
}

/// Stores `text` as element `index` of a new character vector.
#[inline]
pub fn set_text(
    _context: &mut Cx<'_>,
    vector: &mut Fresh<'_, Str>,
    index: usize,
    text: &str,
) -> R<()> {
    assert!(index < vector.len(), "element index out of bounds");
    // SAFETY: the vector is rooted and a standard character vector, the index is in bounds,
    // and making the string allocates, which `&mut Cx` permits.
    unsafe { values::set_mkchar(vector.raw(), index, text) }
}

/// Whether `chars` is `NA_character_`.
#[inline]
pub fn is_na(chars: Gc<'_, Chars>) -> bool {
    chars.raw() == values::na_string()
}

/// Whether `chars` is marked as bytes, which have no character reading.
#[inline]
pub fn is_bytes(chars: Gc<'_, Chars>) -> bool {
    chars.has_mark(gp::BYTES)
}

/// Whether `chars` is marked latin1.
#[inline]
pub fn is_latin1(chars: Gc<'_, Chars>) -> bool {
    chars.has_mark(gp::LATIN1)
}

/// Copies the names, dim and dimnames of `from` onto the new vector `to`.
#[inline]
pub fn copy_shape<T: Vector>(
    context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Fresh<'_, T>,
) -> R<()> {
    if is_bare(context, from) {
        return Ok(());
    }
    let symbols = values::symbols();
    copy_named(
        context,
        from,
        to,
        &[symbols.names, symbols.dim, symbols.dimnames],
    )
}

/// Copies the names of `from` onto the new vector `to`.
#[inline]
pub fn copy_names<T: Vector>(
    context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Fresh<'_, T>,
) -> R<()> {
    if is_bare(context, from) {
        return Ok(());
    }
    copy_named(context, from, to, &[values::symbols().names])
}

/// Whether `value` has no attribute and no tags that read as names, which is the common case
/// a caller tests inline before it pays the call to [`copy_named`].
#[inline]
fn is_bare(context: &Cx<'_>, value: Local<'_>) -> bool {
    !matches!(value.r_type(), SexpType::Pairlist | SexpType::Language)
        && value.attributes(context).r_type() == SexpType::Nil
}

#[inline(never)]
fn copy_named<T: Vector>(
    _context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Fresh<'_, T>,
    attributes: &[Sexp],
) -> R<()> {
    let names = values::symbols().names;
    let tagged = matches!(from.r_type(), SexpType::Pairlist | SexpType::Language);
    for &symbol in attributes {
        // SAFETY: both objects are rooted, `to` is visible to nothing else, and reading or
        // setting an attribute may allocate, which `&mut Cx` permits. `set_attr` roots
        // nothing it must keep: the attribute hangs off `to` once it returns.
        unsafe {
            // GNU R reads the names through `getAttrib`, which turns a pairlist's tags into
            // names.
            let value = if symbol == names && tagged {
                values::names(from.raw())
            } else {
                values::get_attr(from.raw(), symbol)
            };
            if value != values::nil() {
                values::set_attr(to.raw(), symbol, value)?;
            }
        }
    }
    Ok(())
}
