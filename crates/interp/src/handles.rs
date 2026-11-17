//! The interpreter's side of [`heap::handle`]: how a builtin written in safe Rust is entered, how
//! its arguments become handles, and safe forms of the value helpers such a builtin calls.
//!
//! A helper here wraps its raw counterpart in `values` or `coerce` and states why the handle
//! types make the call sound. `docs/src/content/docs/development/design/memory-safety.md` is
//! the design.

#![deny(unsafe_op_in_unsafe_fn)]

pub use heap::handle::{
    Alt, Atomic, Chars, Complex, Cx, Elements, Environment, Expression, Fresh, Gc, I64, Identity,
    Integer, Kind, Language, List, Local, Logical, Owned, Pairlist, Pointer, Promise, Raw, Real,
    Str, Symbol, Unfilled, Value, Vector,
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
        strings::Encoding,
    },
    smallvec::SmallVec,
    std::{borrow::Cow, marker::PhantomData, ops::Range, ptr::NonNull},
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

/// Whether `value` is the empty argument, as an empty symbol in a call evaluates.
#[inline]
pub fn is_missing_argument(value: Local<'_>) -> bool {
    value.raw() == values::missing_arg()
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
#[inline(always)]
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
#[inline(always)]
pub fn character_vector<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Str>> {
    coerced(context, value)
}

/// `value` as a standard integer vector, coerced as `as.integer` coerces when it is of another
/// type.
#[inline(always)]
pub fn integer_vector<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Integer>> {
    coerced(context, value)
}

#[inline(always)]
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

/// [`integer_scalar`] with the coercion warnings of GNU R's `asInteger`: text that is no number
/// and a double outside the integer range warn.
#[inline]
pub fn integer_scalar_warning(_context: &mut Cx<'_>, value: Local<'_>) -> R<i32> {
    // SAFETY: the value is rooted, and a warning may run a calling handler, which `&mut Cx`
    // permits.
    unsafe { values::as_integer_scalar_warning(value.raw()) }
}

/// The shared `TRUE`, `FALSE` or `NA` the heap keeps for as long as it lives.
#[inline]
pub fn logical_constant<'s>(_context: &Cx<'s>, value: i32) -> R<Local<'s>> {
    // SAFETY: the shared logical values are permanent roots of the heap.
    Ok(unsafe { Local::adopt(values::scalar_logical(value)?) })
}

/// A new integer vector of one element.
#[inline]
pub fn integer<'s>(context: &mut Cx<'s>, value: i32) -> R<Local<'s>> {
    Ok(context.scalar::<Integer>(value)?.finish().value())
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
pub fn attribute_copy<'s>(context: &mut Cx<'s>, value: Local<'_>) -> R<Owned<'s>> {
    // SAFETY: the value is rooted, and copying allocates, which `&mut Cx` permits.
    let copy = unsafe { values::shallow_duplicate_attr(value.raw())? };
    // SAFETY: the copy is a live object, nothing has allocated since it was made, and being
    // new, only this handle reaches it, whatever reference count an ALTREP wrapper reports.
    Ok(unsafe { Owned::assume(rooted(context, copy)) })
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
#[inline]
pub fn install<'s>(context: &mut Cx<'s>, name: Local<'_, Chars>) -> R<Local<'s, Symbol>> {
    // SAFETY: the string is rooted.
    unsafe { install_raw(context, name.raw()) }
}

/// [`install`] for element `at` of `names`, which the rooted vector keeps alive, so the name
/// needs no root of its own.
#[inline(always)]
pub fn install_element<'s>(
    context: &mut Cx<'s>,
    names: Local<'_, Str>,
    at: usize,
) -> R<Local<'s, Symbol>> {
    let name = names.get(context, at).raw();
    // SAFETY: the vector is rooted and holds the string.
    unsafe { install_raw(context, name) }
}

/// # Safety
///
/// `name` is a `CHARSXP` that stays alive through an allocation.
#[inline(always)]
unsafe fn install_raw<'s>(_context: &mut Cx<'s>, name: Sexp) -> R<Local<'s, Symbol>> {
    // SAFETY: interning a symbol may allocate, which `&mut Cx` permits. A cached ASCII string
    // names its symbol as it is, so the common name skips the translation.
    let symbol = unsafe {
        if name.has_gp(gp::ASCII) && name.has_gp(gp::CACHED) && name.length() > 0 {
            crate::heap().install_char(name)?
        } else {
            values::install_charsxp_text(name)?
        }
    };
    // SAFETY: both calls return a symbol, and the symbol table keeps every symbol alive.
    Ok(unsafe { Local::adopt(symbol) })
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

/// Stores a string of `bytes` with the encoding mark `encoding` as element `index` of a new
/// character vector.
#[inline]
pub fn set_bytes(
    _context: &mut Cx<'_>,
    vector: &mut Fresh<'_, Str>,
    index: usize,
    bytes: &[u8],
    encoding: Encoding,
) -> R<()> {
    assert!(index < vector.len(), "element index out of bounds");
    // SAFETY: as for `set_text`.
    unsafe { values::set_mkchar_bytes(vector.raw(), index, bytes, encoding) }
}

/// [`set_bytes`] with the bytes `range` of the raw vector `source`, which a slice could not lend
/// across the allocation.
pub fn set_raw_bytes(
    _context: &mut Cx<'_>,
    vector: &mut Fresh<'_, Str>,
    index: usize,
    source: Local<'_, Raw>,
    range: Range<usize>,
    encoding: Encoding,
) -> R<()> {
    assert!(index < vector.len(), "element index out of bounds");
    assert!(range.end <= source.len(), "byte range out of bounds");
    // SAFETY: the source is rooted, and making a string writes into no raw vector, so its bytes
    // stay valid through the allocation; the rest is as for `set_text`.
    unsafe {
        let bytes = &source.raw().slice::<u8>()[range];
        values::set_mkchar_bytes(vector.raw(), index, bytes, encoding)
    }
}

/// [`set_text`] with the bytes `range` of element `at` of `source`, whose text a borrow could
/// not lend across the allocation.
pub fn set_text_range(
    _context: &mut Cx<'_>,
    vector: &mut Fresh<'_, Str>,
    index: usize,
    source: Local<'_, Str>,
    at: usize,
    range: Range<usize>,
) -> R<()> {
    assert!(index < vector.len(), "element index out of bounds");
    assert!(at < source.len(), "source index out of bounds");
    // SAFETY: the source is rooted and holds its element, which making a string neither moves
    // nor writes into, until the store, the last use of the text; the rest is as for
    // `set_text`.
    unsafe {
        let text = values::char_str(source.raw().element(at));
        values::set_mkchar(vector.raw(), index, &text[range])
    }
}

/// A new character vector filled in order, each element once. Since no element is overwritten,
/// a string stored earlier lives as long as the vector, which lets the fill hand out that string
/// again for the same bytes instead of probing the heap's string table
/// ([`values::StringMemo`]).
pub struct StringFill<'s> {
    vector: Fresh<'s, Str>,
    next: usize,
    memo: values::StringMemo<'s>,
}

impl<'s> StringFill<'s> {
    pub fn new(context: &mut Cx<'s>, length: usize) -> R<StringFill<'s>> {
        Ok(StringFill {
            vector: context.alloc::<Str>(length)?,
            next: 0,
            memo: values::StringMemo::default(),
        })
    }

    #[inline]
    pub fn push(&mut self, context: &Cx<'_>, chars: impl Pointer<Chars>) {
        self.vector.set(context, self.next, chars);
        self.next += 1;
    }

    #[inline]
    pub fn push_text(&mut self, context: &mut Cx<'_>, text: &str) -> R<()> {
        set_text(context, &mut self.vector, self.next, text)?;
        self.next += 1;
        Ok(())
    }

    /// Pushes the string of the bytes `range` of element `index` of `source`, marked
    /// `encoding`. A native string goes through the memo.
    pub fn push_part(
        &mut self,
        context: &mut Cx<'_>,
        source: Local<'_, Str>,
        index: usize,
        range: Range<usize>,
        encoding: Encoding,
    ) -> R<()> {
        assert!(self.next < self.vector.len(), "element index out of bounds");
        let chars = source.get(context, index).raw();
        // SAFETY: the source is rooted, so its element lives through the allocation, which
        // moves no string. The new string is stored before anything else allocates, and a
        // string the memo hands out was stored at an earlier index of this vector, which is
        // rooted for `'s` and never overwritten.
        unsafe {
            let bytes = &chars.bytes()[range];
            let made = if encoding == Encoding::Native {
                self.memo.mk_char(bytes)?
            } else {
                crate::heap().mk_char(bytes, encoding)?
            };
            crate::heap().set_element(self.vector.raw(), self.next, made);
        }
        self.next += 1;
        Ok(())
    }

    #[inline]
    pub fn finish(self) -> Fresh<'s, Str> {
        self.vector
    }
}

/// The encoding `chars` is marked with, as `Rf_getCharCE` reports it.
#[inline]
pub fn encoding(chars: Gc<'_, Chars>) -> Encoding {
    if chars.has_mark(gp::UTF8) {
        Encoding::Utf8
    } else if chars.has_mark(gp::LATIN1) {
        Encoding::Latin1
    } else if chars.has_mark(gp::BYTES) {
        Encoding::Bytes
    } else {
        Encoding::Native
    }
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
#[inline(always)]
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
#[inline(always)]
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

/// Copies every attribute of `from` onto the new vector `to`, with the object and S4 bits.
#[inline]
pub fn copy_attributes<T: Vector>(
    context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Fresh<'_, T>,
) -> R<()> {
    // Both bits follow the class attribute, so an object without attributes has neither.
    if from.attributes(context).r_type() == SexpType::Nil {
        return Ok(());
    }
    copied_attributes(context, from, to)
}

#[inline(never)]
fn copied_attributes<T: Vector>(
    _context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Fresh<'_, T>,
) -> R<()> {
    // SAFETY: both objects are rooted, `to` is visible to nothing else, and setting an
    // attribute may allocate, which `&mut Cx` permits.
    unsafe { values::copy_attributes(from.raw(), to.raw()) }
}

/// The names attribute of a vector, or `NULL`.
#[inline]
pub fn names_attribute<'s>(context: &Cx<'s>, value: Local<'_>) -> Local<'s> {
    // SAFETY: the value is rooted, and the lookup only reads its attribute list.
    unsafe {
        rooted(
            context,
            values::get_attr(value.raw(), values::symbols().names),
        )
    }
}

/// Sets the attribute called `name` of the new vector `to` to `value`.
pub fn set_attribute_named<T: Vector>(
    _context: &mut Cx<'_>,
    to: &mut Fresh<'_, T>,
    name: &str,
    value: Local<'_>,
) -> R<()> {
    // SAFETY: both objects are rooted, `to` is visible to nothing else, and interning the name
    // and setting the attribute may allocate, which `&mut Cx` permits.
    unsafe { values::set_attr(to.raw(), values::install(name)?, value.raw()) }
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

/// Whether R code dispatches on `value`, which its class attribute decides.
#[inline]
pub fn is_object(value: impl Pointer<Value>) -> bool {
    // SAFETY: the handle is a live object, and the test reads its header.
    unsafe { value.raw().is_object() }
}

/// Whether `value` is an ALTREP instance, whose contents its class computes.
#[inline]
pub fn is_altrep(value: impl Pointer<Value>) -> bool {
    // SAFETY: as for `is_object`.
    unsafe { value.raw().is_altrep() }
}

/// Whether `value` is one of ry's Arrays, whose stores go through the checks in `arrays`.
#[inline]
pub fn is_array(value: impl Pointer<Value>) -> bool {
    // SAFETY: as for `is_object`.
    unsafe { value.raw().is_array() }
}

/// The rows and columns of a matrix: a vector whose `dim` attribute is two integers.
#[inline]
pub fn matrix_extent<T: Vector>(_context: &Cx<'_>, vector: impl Pointer<T>) -> Option<(i32, i32)> {
    // SAFETY: the handle is a live vector, whose attributes are a pairlist this only reads,
    // and the test admits a `dim` only as a standard integer vector of two elements.
    unsafe {
        let dim = values::get_attr(vector.raw(), values::symbols().dim);
        (dim.kind() == SexpType::Integer && !dim.is_altrep() && values::length(dim) == 2)
            .then(|| (dim.slice::<i32>()[0], dim.slice::<i32>()[1]))
    }
}

/// The names of a vector.
pub enum Names<'a> {
    None,
    Standard(Gc<'a, Str>),
    /// Names in a form with no direct element access, such as an ALTREP character vector.
    Other,
}

/// The `names` attribute of `vector`, read without a safepoint.
#[inline]
pub fn names<'a, T: Vector>(_context: &'a Cx<'_>, vector: impl Pointer<T>) -> Names<'a> {
    // SAFETY: the handle is a live vector, whose attributes are a pairlist this only reads,
    // and the borrow of the context keeps the attribute's value alive.
    let names =
        unsafe { Gc::<Value>::adopt(values::get_attr(vector.raw(), values::symbols().names)) };
    if names.r_type() == SexpType::Nil {
        return Names::None;
    }
    names
        .downcast::<Str>()
        .map_or(Names::Other, Names::Standard)
}

/// Ends every move of `value`, which base R code is about to change where it is.
#[inline]
pub fn modifying(_context: &mut Cx<'_>, value: Local<'_>) -> R<()> {
    // SAFETY: the value is rooted, and ending a move may run R code, which `&mut Cx` permits.
    unsafe { crate::moves::modifying(value.raw()) }
}

/// One element for a store, already of the element type of the vector it goes into.
#[derive(Clone, Copy)]
pub enum Scalar<'s> {
    Real(f64),
    /// An integer or a logical.
    Integer(i32),
    /// The first element of a character vector, which has one.
    Text(Local<'s, Str>),
    /// A list element.
    Item(Local<'s>),
}

impl From<f64> for Scalar<'_> {
    fn from(value: f64) -> Self {
        Scalar::Real(value)
    }
}

impl From<i32> for Scalar<'_> {
    fn from(value: i32) -> Self {
        Scalar::Integer(value)
    }
}

impl<'s> From<Local<'s, Str>> for Scalar<'s> {
    fn from(text: Local<'s, Str>) -> Self {
        Scalar::Text(text)
    }
}

impl<'s> From<Local<'s>> for Scalar<'s> {
    fn from(item: Local<'s>) -> Self {
        Scalar::Item(item)
    }
}

/// `vector` one element longer, for the caller to bind: the last element is `element`, named
/// `name` when there is one, and the vector's only attribute is its names. `owned` is the
/// vector when the caller's variable alone holds it, and it then grows where it is if it has
/// room.
pub fn append<'s, T: Vector>(
    context: &mut Cx<'s>,
    vector: Local<'s, T>,
    name: Option<Local<'s, Chars>>,
    element: Scalar<'s>,
    owned: Option<Owned<'s>>,
) -> R<Local<'s>> {
    assert!(
        owned
            .as_ref()
            .is_none_or(|owned| owned.get().is(vector.value())),
        "the claim is for another vector"
    );
    let fits = match (T::TYPE, element) {
        (SexpType::Real, Scalar::Real(_))
        | (SexpType::Integer | SexpType::Logical, Scalar::Integer(_))
        | (SexpType::List | SexpType::Expression, Scalar::Item(_)) => true,
        (SexpType::String, Scalar::Text(text)) => text.len() == 1,
        _ => false,
    };
    assert!(
        fits,
        "an appended element of another type than the vector's"
    );
    let reuse = owned.is_some();
    let Some(name) = name else {
        // SAFETY: `vector` is a rooted standard vector, and `reuse` is true only under the
        // caller's claim that nothing else holds it. The new element is set before anything
        // else can allocate or fail, and `fits` matched its type to the storage.
        let result = unsafe {
            let heap = crate::heap();
            let count = vector.len();
            let result = if reuse
                && heap.is_resizable(vector.raw())
                && vector.raw().capacity() > count as isize
            {
                heap.resize_vector(vector.raw(), count as isize + 1)?;
                vector.raw()
            } else {
                crate::subset::append_copy(vector.raw(), reuse)?
            };
            match element {
                Scalar::Real(value) => values::reals_mut(result)[count] = value,
                Scalar::Integer(value) => values::ints_mut(result)[count] = value,
                Scalar::Text(text) => heap.set_element(result, count, text.raw().element(0)),
                Scalar::Item(item) => heap.set_element(result, count, item.raw()),
            }
            result
        };
        // SAFETY: the copy was rooted when it was allocated, and otherwise the result is the
        // rooted `vector`.
        return Ok(unsafe { Local::adopt(result) });
    };
    // `append_named` reads the element from a value of the vector's type.
    let scalar = match element {
        Scalar::Real(value) => context.scalar::<Real>(value)?.finish().value(),
        Scalar::Integer(value) => context.scalar::<Integer>(value)?.finish().value(),
        Scalar::Text(text) => text.value(),
        Scalar::Item(item) => item,
    };
    // SAFETY: every argument is rooted, `vector` is a standard vector whose type `fits`
    // matched to the scalar, and `reuse` is the caller's claim.
    let result =
        unsafe { crate::subset::append_named(vector.raw(), name.raw(), scalar.raw(), reuse)? };
    // SAFETY: as for the unnamed append.
    Ok(unsafe { Local::adopt(result) })
}

/// The vector an element store into `vector` writes: `owned`, the vector itself under its
/// holder's claim, or else a copy with its own storage for the holder to bind.
#[inline(always)]
pub fn writable<'s, T: Vector>(
    context: &mut Cx<'s>,
    vector: Local<'s, T>,
    owned: Option<Owned<'s>>,
) -> R<Owned<'s, T>> {
    if let Some(owned) = owned
        && owned.get().is(vector.value())
        && let Ok(owned) = owned.downcast::<T>()
    {
        return Ok(owned);
    }
    let copy = shallow_duplicate(context, vector)?;
    // SAFETY: both are live objects. The copy drops the Array flag, which an element store
    // keeps: it changes no type, length or attribute.
    unsafe { copy.raw().set_array(vector.raw().is_array()) };
    Ok(copy.owned())
}

/// The position of the first element of `vector` that repeats an earlier one, compared as
/// `anyDuplicated` compares: strings by text, list elements with `identical`.
#[inline]
pub fn first_duplicate(context: &mut Cx<'_>, vector: Local<'_>) -> R<Option<usize>> {
    // A few strings compare pairwise in less time than a hash table takes to set up.
    if let Some(strings) = vector.downcast::<Str>()
        && strings.len() <= 8
    {
        let strings = strings.read(context);
        return Ok((1..strings.len()).find(|&index| {
            (0..index).any(|earlier| same_text(strings.get(earlier), strings.get(index)))
        }));
    }
    // SAFETY: the vector is rooted, and reading an ALTREP vector may allocate, which `&mut Cx`
    // permits.
    let duplicated = unsafe {
        crate::sort::duplicated_elements(vector.raw(), values::nil(), false, true, "anyDuplicated")?
    };
    Ok(duplicated.first())
}

/// Whether `value` has no attribute but names.
#[inline]
pub fn has_only_names(_context: &Cx<'_>, value: impl Pointer<Value>) -> bool {
    // SAFETY: the handle is a live object, whose attribute list this only reads.
    unsafe {
        let attributes = value.raw().attrib();
        attributes == values::nil()
            || (attributes.tag() == values::symbols().names && attributes.cdr() == values::nil())
    }
}

/// `environment$name <- value` where it binds the name and nothing else happens: false, having
/// changed nothing, for the empty environment, a locked frame that lacks the name, and a locked
/// or active binding, whose assignment raises an error or runs a function.
#[inline(always)]
pub fn bind<'s>(
    _context: &mut Cx<'s>,
    environment: Local<'s, Environment>,
    name: Local<'s, Symbol>,
    value: Local<'s>,
) -> R<bool> {
    let heap = crate::heap();
    let (environment, name) = (environment.raw(), name.raw());
    // SAFETY: the three handles are rooted live objects of the kinds `define` expects, and
    // `&mut Cx` permits the allocation of a new binding and the R code that ending a move runs.
    unsafe {
        let writable = environment != heap.known().empty_env
            && match heap.frame_cell(environment, name) {
                Some(cell) => !cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED),
                None => !environment.has_gp(gp::FRAME_LOCKED),
            };
        if writable {
            crate::moves::reaching_environment()?;
            heap.define(environment, name, value.raw())?;
        }
        Ok(writable)
    }
}

/// Runs `body`, which reaches the heap only through its context, for raw code that is not a
/// builtin's entry. Whatever `body` roots is released on return, so a loop over such calls does
/// not grow the handle stack.
///
/// # Safety
///
/// The caller is on the interpreter's thread and holds no reference into the heap. A raw
/// pointer in the result is rooted by nothing this call did: the caller stores or roots it
/// before anything allocates.
#[inline]
pub unsafe fn with<T>(body: impl for<'s> FnOnce(&mut Cx<'s>) -> R<T>) -> R<T> {
    let heap = crate::heap();
    let mark = heap.handle_mark();
    // SAFETY: the interpreter's heap lives as long as the process, and `body` cannot keep the
    // context or anything of its scope, because it must accept every `'s`.
    let mut context = unsafe { Cx::enter(NonNull::from(&mut *heap)) };
    // Each arm releases for itself, which lets an answer go straight to the caller's result.
    match body(&mut context) {
        Ok(answer) => {
            heap.release_handles(mark);
            Ok(answer)
        }
        Err(unwind) => {
            heap.release_handles(mark);
            unwind.root();
            Err(unwind)
        }
    }
}

/// Runs `body` as a builtin's fast-path entry in the handle scope of its caller, which releases
/// the handle stack once the entry returns: tier 0 after the instruction, compiled code and the
/// evaluator's direct call around the entry. A scope of the entry's own would mark and release
/// a second time on every call.
///
/// # Safety
///
/// As for [`with`], and the caller releases what `body` roots.
#[inline(always)]
pub unsafe fn in_caller_scope<T>(body: impl for<'s> FnOnce(&mut Cx<'s>) -> R<T>) -> R<T> {
    // SAFETY: as in `with`. The scope `body` sees ends no later than the caller's, which it
    // cannot outlive, because it must accept every `'s`.
    let mut context = unsafe { Cx::enter(NonNull::from(&mut *crate::heap())) };
    body(&mut context)
}

/// Roots `raw` in the context's scope.
///
/// # Safety
///
/// `raw` is a live object.
#[inline]
pub unsafe fn adopt<'s>(context: &Cx<'s>, raw: Sexp) -> Local<'s> {
    // SAFETY: the caller's contract, and nothing allocates before the root.
    unsafe { rooted(context, raw) }
}

/// `raw` as a value of the context's scope, without a root of the scope's own: the frame
/// holds an instruction's operands until the instruction ends, and a second root for each
/// would cost every store and every builtin fast path.
///
/// # Safety
///
/// `raw` is a live object, and a root of the caller's keeps it alive until the context's scope
/// ends, whatever R code runs in between.
#[inline]
pub unsafe fn held<'s>(_context: &Cx<'s>, raw: Sexp) -> Local<'s> {
    // SAFETY: the caller's contract.
    unsafe { Local::adopt(raw) }
}

/// The raw unary entry (`builtins::UnaryFn`) of `$function`, a safe fast path that answers for
/// one argument value or declines with `None`. The answer borrows the context, so the fast
/// path returns a value it read from the heap without rooting it.
macro_rules! safe_unary {
    ($function:path) => {{
        unsafe fn entry(
            value: heap::object::Sexp,
        ) -> $crate::unwind::R<Option<heap::object::Sexp>> {
            // SAFETY: the evaluator calls a unary entry on the interpreter's thread with an
            // argument one of its registers holds until the entry returns, stores or roots
            // the result before it allocates again, and releases the handle stack after the
            // call.
            unsafe {
                $crate::handles::in_caller_scope(|context| {
                    let value = $crate::handles::held(context, value);
                    Ok($function(context, value)?.map($crate::handles::Gc::raw))
                })
            }
        }
        entry
    }};
}
pub(crate) use safe_unary;

/// The raw direct entry (`builtins::DirectFn`) of `$function`, a safe fast path over the
/// argument values in formal order that answers or declines with `None`, as a unary one does.
macro_rules! safe_direct {
    ($function:path) => {{
        unsafe fn entry(
            _call: heap::object::Sexp,
            values: &[heap::object::Sexp],
        ) -> $crate::unwind::R<Option<heap::object::Sexp>> {
            // SAFETY: as for `safe_unary`: the caller's registers, or its root for a
            // replacement's target, hold every argument value.
            unsafe {
                $crate::handles::in_caller_scope(|context| {
                    let values = $crate::handles::Local::adopt_slice(values);
                    Ok($function(context, values)?.map($crate::handles::Gc::raw))
                })
            }
        }
        entry
    }};
}
pub(crate) use safe_direct;

impl<'s> Args<'_, 's> {
    /// The call expression, which errors and warnings name.
    #[inline]
    pub fn expression(&self) -> Local<'s> {
        // SAFETY: the evaluator keeps the call it is evaluating rooted.
        unsafe { Local::adopt(self.call.call) }
    }

    /// GNU R's error for a call with other than `count` arguments.
    pub fn check_arity(&self, count: usize) -> R<()> {
        self.call.check_arity(count)
    }

    /// The arguments matched to `formals` as a closure matches them, `None` where an argument
    /// is absent or missing.
    pub fn matched<const N: usize>(
        &self,
        context: &mut Cx<'s>,
        formals: [&str; N],
    ) -> R<[Option<Local<'s>>; N]> {
        let matched = self.matched_as_passed(context, formals)?;
        Ok(matched.map(|value| value.filter(|&value| !is_missing_argument(value))))
    }

    /// [`Args::matched`] that keeps the empty argument a call passed, for the builtins GNU R
    /// lets read one as a value.
    #[inline]
    pub fn matched_as_passed<const N: usize>(
        &self,
        _context: &mut Cx<'s>,
        formals: [&str; N],
    ) -> R<[Option<Local<'s>>; N]> {
        // SAFETY: matching reads the call's argument list and may allocate an error, which
        // `&mut Cx` permits. Each matched value is one of the call's rooted arguments.
        unsafe {
            let (matched, _) = self.call.match_args(&formals)?;
            Ok(std::array::from_fn(|index| {
                matched[index].map(|value| Local::adopt(value))
            }))
        }
    }

    /// The environment the call was made from, which a slot frame makes on demand.
    pub fn environment(&self, context: &mut Cx<'s>) -> R<Local<'s, Environment>> {
        // SAFETY: making the frame's environment allocates, which `&mut Cx` permits, and the
        // result is live until the next allocation.
        let environment = unsafe { rooted(context, self.call.env()?) };
        environment
            .downcast::<Environment>()
            .ok_or_else(|| error("internal error: a call has no environment"))
    }

    /// Raises a warning that names the call.
    pub fn warning(&self, _context: &mut Cx<'s>, message: impl Into<String>) -> R<()> {
        // SAFETY: a warning may run a calling handler, which is R code and so needs `&mut Cx`.
        unsafe { conditions::warning_call(self.call.call, message) }
    }
}

/// An error that names the call `call`.
pub fn error_in(call: Local<'_>, message: impl Into<String>) -> Unwind {
    crate::unwind::error_in(call.raw(), message)
}

/// `NULL`, which the heap keeps for as long as it lives.
#[inline]
pub fn null<'s>() -> Local<'s> {
    // SAFETY: `NULL` is a permanent root of the heap.
    unsafe { Local::adopt(values::nil()) }
}

/// The base namespace, which the heap keeps for as long as it lives.
#[inline]
pub fn base_namespace<'s>() -> Local<'s, Environment> {
    // SAFETY: the base namespace is a permanent root of the heap.
    unsafe { Local::adopt(values::base_namespace()) }
}

/// One of the interpreter's well-known symbols, such as `symbol(|symbols| symbols.names)`.
#[inline]
pub fn symbol(pick: impl FnOnce(&values::Symbols) -> Sexp) -> Gc<'static, Symbol> {
    // SAFETY: the symbols of `values::symbols` are installed at start-up, and the heap's symbol
    // table keeps every symbol alive for as long as it lives.
    unsafe { Gc::adopt(pick(values::symbols())) }
}

/// The attribute `name` of `object`, or `NULL`.
#[inline]
pub fn get_attribute<'a, 's: 'a, T: Kind>(
    context: &'a Cx<'_>,
    object: Local<'s, T>,
    name: Gc<'_, Symbol>,
) -> Gc<'a> {
    attribute_cells(context, object)
        .find(|&(tag, _)| tag.is(name))
        .map_or_else(|| null().read(context), |(_, value)| value)
}

/// The tag and value of each attribute of `object`, in order.
#[inline]
pub fn attribute_cells<'a, 's: 'a, T: Kind>(
    context: &'a Cx<'_>,
    object: Local<'s, T>,
) -> impl Iterator<Item = (Gc<'a>, Gc<'a>)> {
    let mut cell = object.value().attributes(context);
    std::iter::from_fn(move || {
        let pair = cell.downcast::<Pairlist>()?;
        cell = pair.rest();
        Some((pair.tag(), pair.head()))
    })
}

/// The `dim` attribute as extents, when there is one.
#[inline]
pub fn dims(_context: &mut Cx<'_>, object: Local<'_>) -> Option<values::Dims> {
    // SAFETY: the object is rooted, and reading an ALTREP `dim` may run its class's method,
    // which `&mut Cx` permits.
    unsafe { values::dims(object.raw()) }
}

/// R's `length()` without dispatch, for any object.
#[inline]
pub fn length(_context: &mut Cx<'_>, object: Local<'_>) -> usize {
    // SAFETY: the object is rooted, and an ALTREP length may run its class's method, which
    // `&mut Cx` permits.
    unsafe { values::length(object.raw()) }
}

/// Whether `object` is an S4 object.
#[inline]
pub fn is_s4(object: impl Pointer<Value>) -> bool {
    // SAFETY: the handle is a live object, and the test reads its header.
    unsafe { object.raw().has_gp(gp::S4_OBJECT) }
}

/// GNU R's `getAttrib(x, R_NamesSymbol)`: a pairlist's or call's tags, the `names`
/// attribute, or a one-dimensional array's dimnames.
pub fn names_of<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    if matches!(
        object.r_type(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        // SAFETY: the list is rooted, and collecting its tags allocates, which `&mut Cx`
        // permits; the result is live until the next allocation.
        return Ok(unsafe { rooted(context, values::pairlist_names(object.raw())?) });
    }
    if let Some(names) = stored_names(context, object) {
        return Ok(context.root(names));
    }
    let dim = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dim),
    ));
    if length(context, dim) == 1 {
        return Ok(context.root(array_names(context, object)));
    }
    Ok(null())
}

/// [`names_of`] for an object whose names are not a pairlist's tags, read where they are
/// stored. It is `None` when only a safepoint can tell: the `dim` is an ALTREP vector, whose
/// length its class's method reports.
#[inline(always)]
pub fn stored_names<'a, 's: 'a>(context: &'a Cx<'_>, object: Local<'s>) -> Option<Gc<'a>> {
    if object.r_type() == SexpType::Environment {
        return Some(null().read(context));
    }
    let names = get_attribute(context, object, symbol(|symbols| symbols.names));
    if names.r_type() != SexpType::Nil {
        return Some(names);
    }
    let dim = get_attribute(context, object, symbol(|symbols| symbols.dim));
    match dim.downcast::<Integer>() {
        Some(dim) if dim.len() == 1 => Some(array_names(context, object)),
        Some(_) => Some(null().read(context)),
        None if dim.r_type() == SexpType::Integer => None,
        None => Some(null().read(context)),
    }
}

/// The names of a one-dimensional array, which are its dimnames.
fn array_names<'a, 's: 'a>(context: &'a Cx<'_>, object: Local<'s>) -> Gc<'a> {
    let dimnames = get_attribute(context, object, symbol(|symbols| symbols.dimnames));
    match dimnames.downcast::<List>() {
        Some(dimnames) if dimnames.len() == 1 => dimnames.get(0),
        _ => null().read(context),
    }
}

/// `object` to change where it is, as base R's replacement functions do: itself when nothing
/// else refers to it, and otherwise a copy with its own storage.
#[inline]
pub fn unshared<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Owned<'s>> {
    // SAFETY: the object is rooted, and ending a move or copying may run R code or allocate,
    // which `&mut Cx` permits. `modifiable` returns the object itself only when its reference
    // count is at most one, the binding the caller replaces, and otherwise a new copy that
    // nothing else refers to.
    unsafe {
        let unshared = crate::subset::modifiable(object.raw())?;
        // The object's own root serves when it is not copied.
        Ok(Owned::assume(if unshared == object.raw() {
            object
        } else {
            rooted(context, unshared)
        }))
    }
}

/// [`unshared`] for a change of attributes only: the copy of a long vector is an ALTREP
/// wrapper that shares the elements, so the result has no element access.
#[inline(always)]
pub fn attributes_unshared<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Owned<'s>> {
    // SAFETY: as for `unshared`, with `modifiable_attributes`, whose copy is a new wrapper or
    // vector that nothing else refers to.
    unsafe {
        let unshared = crate::subset::modifiable_attributes(object.raw())?;
        // The object's own root serves when it is not copied.
        Ok(Owned::assume(if unshared == object.raw() {
            object
        } else {
            rooted(context, unshared)
        }))
    }
}

/// `vector` to change where it is when no object refers to it, as GNU R reuses an
/// unreferenced value that C code built and then set as an attribute.
pub fn unreferenced<'s, T: Kind>(vector: Local<'s, T>) -> Option<Owned<'s, T>> {
    // SAFETY: a reference count of zero means no variable or object holds the vector, so only
    // the caller's handle reaches it.
    unsafe { (vector.raw().refcount() == 0).then(|| Owned::assume(vector)) }
}

/// Sets the attribute `name` of `object` to `value`, or removes it when `value` is `NULL`,
/// without checking the value.
pub fn set_attribute<'s, T: Kind>(
    _context: &mut Cx<'s>,
    object: &mut Owned<'s, T>,
    name: Gc<'_, Symbol>,
    value: Local<'_>,
) -> R<()> {
    // SAFETY: both objects are rooted, the caller owns `object`, and the new attribute cell
    // allocates, which `&mut Cx` permits.
    unsafe { values::set_attr(object.get().raw(), name.raw(), value.raw()) }
}

/// Removes the attribute `name` of `object`, if it has one.
pub fn remove_attribute<'s, T: Kind>(
    _context: &mut Cx<'s>,
    object: &mut Owned<'s, T>,
    name: Gc<'_, Symbol>,
) {
    // SAFETY: the caller owns `object`, and unlinking a cell allocates nothing.
    unsafe { values::remove_attr(object.get().raw(), name.raw()) }
}

/// Removes every attribute of `object` with its object and S4 bits, keeping the S4 bit when
/// `keep_s4` asks.
pub fn clear_attributes<'s, T: Kind>(
    _context: &mut Cx<'s>,
    object: &mut Owned<'s, T>,
    keep_s4: bool,
) {
    // SAFETY: the caller owns `object`, and dropping its attribute list allocates nothing.
    unsafe {
        let raw = object.get().raw();
        values::clear_attributes(raw);
        if keep_s4 {
            raw.set_gp_bits(gp::S4_OBJECT, true);
        }
    }
}

/// Sets the class attribute, or removes it for `NULL` or an empty vector, with the object bit
/// in step.
pub fn set_class<'s, T: Kind>(
    _context: &mut Cx<'s>,
    object: &mut Owned<'s, T>,
    class: Local<'_>,
) -> R<()> {
    // SAFETY: as for `set_attribute`.
    unsafe { crate::dispatch::set_class_attribute(object.get().raw(), class.raw()) }
}

/// Tags each cell of the pairlist or call `object` with the symbol of the matching name, or
/// with none for `NA`, an empty name or a cell past the names; `None` clears every tag.
pub fn set_tags<'s>(
    _context: &mut Cx<'s>,
    object: &mut Owned<'s>,
    names: Option<Local<'s, Str>>,
) -> R<()> {
    let heap = crate::heap();
    let mut index = 0;
    let mut cell = object.get().raw();
    // SAFETY: the caller owns the list, whose cells it reaches, and `names` is a rooted
    // standard vector. Installing a symbol allocates, which `&mut Cx` permits, and moves no
    // cell.
    unsafe {
        while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
            let tag = match names {
                Some(names) if index < names.len() => {
                    let name = names.raw().element(index);
                    if name != values::na_string() && !name.bytes().is_empty() {
                        heap.install_char(name)?
                    } else {
                        values::nil()
                    }
                }
                _ => values::nil(),
            };
            heap.set_tag(cell, tag);
            cell = cell.cdr();
            index += 1;
        }
    }
    Ok(())
}

/// The symbol a string names, as GNU R's `installTrChar` gives it for an attribute name.
#[inline]
pub fn install_name(_context: &mut Cx<'_>, name: Local<'_, Chars>) -> R<Gc<'static, Symbol>> {
    // SAFETY: the string is rooted and interning may allocate, which `&mut Cx` permits; a
    // symbol lives as long as the heap.
    unsafe { Ok(Gc::adopt(values::install_charsxp(name.raw())?)) }
}

/// [`install_name`] for element `at` of `names`, which the rooted vector keeps alive, so the
/// name needs no root of its own.
#[inline(always)]
pub fn install_name_at(
    context: &mut Cx<'_>,
    names: Local<'_, Str>,
    at: usize,
) -> R<Gc<'static, Symbol>> {
    let name = names.get(context, at).raw();
    // SAFETY: as for `install_name`, and the vector holds the string.
    unsafe { Ok(Gc::adopt(values::install_charsxp(name)?)) }
}

/// The symbol of a string as the heap's `install_char` gives it, which installs `NA` as `NA`.
#[inline]
pub fn install_interned(_context: &mut Cx<'_>, name: Local<'_, Chars>) -> R<Gc<'static, Symbol>> {
    // SAFETY: as for `install_name`.
    unsafe { Ok(Gc::adopt(crate::heap().install_char(name.raw())?)) }
}

/// `value` coerced to `kind` as `coerceVector` does, keeping its attributes; itself when it
/// already has that type, ALTREP or not.
#[inline]
pub fn coerce<'s>(context: &mut Cx<'s>, value: Local<'s>, kind: SexpType) -> R<Local<'s>> {
    if value.r_type() == kind {
        return Ok(value);
    }
    // SAFETY: the value is rooted, and coercion allocates, which `&mut Cx` permits.
    Ok(unsafe { rooted(context, coerce_vector(value.raw(), kind)?) })
}

/// `value` coerced to another type `kind` with no attributes, as a dimnames component is
/// stored.
#[inline]
pub fn coerce_bare<'s>(context: &mut Cx<'s>, value: Local<'s>, kind: SexpType) -> R<Local<'s>> {
    assert!(value.r_type() != kind, "a coercion to the value's own type");
    let converted = coerce(context, value, kind)?;
    // SAFETY: a coercion to another type is a new vector that only this handle reaches.
    unsafe { values::clear_attributes(converted.raw()) };
    Ok(converted)
}

/// A factor's labels as a character vector.
#[inline]
pub fn factor_as_character<'s>(context: &mut Cx<'s>, factor: Local<'s>) -> R<Local<'s>> {
    // SAFETY: the factor is rooted, and the result allocates, which `&mut Cx` permits.
    Ok(unsafe { rooted(context, crate::coerce::factor_as_character(factor.raw())?) })
}

/// `object` as an ordinary object: an ALTREP instance's expanded form, anything else itself.
#[inline]
pub fn materialize<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    if !is_altrep(object) {
        return Ok(object);
    }
    // SAFETY: the object is rooted, and expanding it may run its class's methods, which
    // `&mut Cx` permits.
    Ok(unsafe { rooted(context, values::materialize(object.raw())?) })
}

/// Evaluates the call `function(arguments...)` in `environment`, with the arguments in the
/// call as they are, so a symbol or call among them is evaluated there.
pub fn call_function<'s>(
    context: &mut Cx<'s>,
    function: &str,
    arguments: &[Local<'s>],
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    let arguments: SmallVec<[Sexp; 4]> = arguments.iter().map(|argument| argument.raw()).collect();
    // SAFETY: the arguments and the environment are rooted, and the call is rooted before the
    // evaluation, which may run any R code, as `&mut Cx` permits.
    unsafe {
        let function = values::install(function)?;
        let call = rooted(context, values::call(function, &arguments)?);
        Ok(rooted(
            context,
            crate::eval::eval(call.raw(), environment.raw())?,
        ))
    }
}

/// The call `quote(value)`, which evaluates to `value` itself.
#[inline]
pub fn quoted<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s>> {
    // SAFETY: the value is rooted, and the call allocates, which `&mut Cx` permits.
    Ok(unsafe {
        rooted(
            context,
            values::call(values::symbols().quote, &[value.raw()])?,
        )
    })
}

/// The class vector `class()` reports.
#[inline]
pub fn class_of<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    // SAFETY: the object is rooted, and an implicit class allocates, which `&mut Cx` permits.
    Ok(unsafe { rooted(context, crate::dispatch::class_of(object.raw())?) })
}

/// [`class_of`] without a root, for a caller that hands the class on before the next
/// safepoint.
#[inline]
pub fn class_read<'a>(_context: &'a mut Cx<'_>, object: Local<'_>) -> R<Gc<'a>> {
    // SAFETY: the object is rooted, and an implicit class allocates, which `&mut Cx` permits.
    // The class is the stored attribute or a new vector, live until the next allocation, which
    // the borrow of the context rules out.
    Ok(unsafe { Gc::adopt(crate::dispatch::class_of(object.raw())?) })
}

/// An S4 object's class and every class it extends, as `is()` lists them.
#[inline]
pub fn s4_classes<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    // SAFETY: the object is rooted, and the methods package's R code may run, which `&mut Cx`
    // permits.
    Ok(unsafe {
        rooted(
            context,
            crate::dispatch::s4_class_with_superclasses(object.raw())?,
        )
    })
}

/// The class of an object with no class attribute and no dims, such as `"numeric"`.
#[inline]
pub fn basic_class(object: impl Pointer<Value>) -> &'static str {
    // SAFETY: the handle is a live object, whose type this reads.
    unsafe { crate::dispatch::basic_class(object.raw(), false) }
}

/// `ls(environment, all.names = TRUE, sorted = FALSE)`.
#[inline]
pub fn environment_names<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    // SAFETY: the environment is rooted, and listing it allocates, which `&mut Cx` permits.
    Ok(unsafe {
        rooted(
            context,
            crate::envir::env_names(environment.raw(), true, false)?,
        )
    })
}

/// `1:count` as a compact sequence, when the host provides one.
#[inline]
pub fn integer_sequence<'s>(context: &mut Cx<'s>, count: usize) -> R<Option<Local<'s>>> {
    // SAFETY: making the sequence allocates, which `&mut Cx` permits, and the result is live
    // until the next allocation.
    unsafe {
        let sequence =
            (crate::interp().hooks.compact_sequence)(SexpType::Integer, count, 1.0, 1.0)?;
        Ok(sequence.map(|sequence| rooted(context, sequence)))
    }
}

/// The first `count` cells of the pairlist `list` as a new pairlist with their tags, padded
/// with untagged `NULL`s.
pub fn pairlist_prefix<'s>(context: &mut Cx<'s>, list: Local<'s>, count: usize) -> R<Local<'s>> {
    let mut items = Vec::with_capacity(count);
    let mut cell = list.read(context).downcast::<Pairlist>();
    while items.len() < count {
        match cell {
            Some(pair) => {
                items.push((pair.tag().raw(), pair.head().raw()));
                cell = pair.rest().downcast();
            }
            None => items.push((values::nil(), values::nil())),
        }
    }
    // SAFETY: the items are the fields of the rooted `list`'s cells, which keep them alive
    // while the new cells allocate, as `&mut Cx` permits.
    Ok(unsafe { rooted(context, values::pairlist(&items, SexpType::Pairlist)?) })
}

/// Copies the first `count` elements of `from` into the new vector `to`, a list's or
/// character vector's in one bulk copy that counts its references once.
pub fn copy_prefix<T: Vector>(
    _context: &mut Cx<'_>,
    from: Local<'_, T>,
    to: &mut Fresh<'_, T>,
    count: usize,
) -> R<()> {
    assert!(
        count <= from.len() && count <= to.len(),
        "element index out of bounds"
    );
    // SAFETY: both are rooted standard vectors of one type, `to` is new and visible to nothing
    // else, and the count is in bounds of both.
    unsafe { crate::subset::copy_prefix(from.raw(), to.raw(), count) }
}

/// The empty string, which the heap keeps for as long as it lives.
#[inline]
pub fn blank() -> Gc<'static, Chars> {
    // SAFETY: the empty string is a permanent root of the heap.
    unsafe { Gc::adopt(values::blank_string()) }
}

/// The first element as a double: `NA` for an empty vector or a type with no numeric reading.
#[inline]
pub fn real_scalar(_context: &mut Cx<'_>, value: Local<'_>) -> f64 {
    // SAFETY: the value is rooted, and reading an ALTREP's first element may allocate, which
    // `&mut Cx` permits.
    unsafe { values::as_real_scalar(value.raw()) }
}

/// Element `index` of a logical or integer vector, through its ALTREP class's `Elt` method
/// when it has one.
#[inline]
pub fn int_element(_context: &mut Cx<'_>, vector: Local<'_>, index: usize) -> R<i32> {
    // SAFETY: the vector is rooted and of an integer type, the index is in bounds, and an
    // ALTREP class's method may allocate, which `&mut Cx` permits.
    unsafe {
        let raw = vector.raw();
        assert!(
            matches!(raw.kind(), SexpType::Logical | SexpType::Integer)
                && index < values::length(raw),
            "element index out of bounds"
        );
        values::int_elt(raw, index)
    }
}

/// Element `index` of a double vector, through its ALTREP class's `Elt` method when it has
/// one.
#[inline]
pub fn real_element(_context: &mut Cx<'_>, vector: Local<'_>, index: usize) -> R<f64> {
    // SAFETY: as for `int_element`, for a double vector.
    unsafe {
        let raw = vector.raw();
        assert!(
            raw.kind() == SexpType::Real && index < values::length(raw),
            "element index out of bounds"
        );
        values::real_elt(raw, index)
    }
}

/// Element `index` of a character vector, list or expression vector, read through its ALTREP
/// class's `Elt` method when it has one.
#[inline]
pub fn item<'s>(context: &mut Cx<'s>, vector: Local<'s>, index: usize) -> R<Local<'s>> {
    // SAFETY: the vector is rooted and holds pointers, the index is in bounds, and the method
    // may allocate, which `&mut Cx` permits. The element is rooted before anything else runs.
    unsafe {
        let raw = vector.raw();
        assert!(
            matches!(
                raw.kind(),
                SexpType::String | SexpType::List | SexpType::Expression
            ) && index < values::length(raw),
            "element index out of bounds"
        );
        let element = raw.element(index);
        crate::unwind::raise_parked()?;
        Ok(rooted(context, element))
    }
}

/// Stores element `index` of `from`, a character vector or list of `to`'s type read as
/// [`item`] reads it, in slot `slot` of the new vector `to`, without a root of its own.
#[inline]
pub fn store_item<T: Elements>(
    _context: &mut Cx<'_>,
    from: Local<'_>,
    index: usize,
    to: &mut Fresh<'_, T>,
    slot: usize,
) -> R<()> {
    // SAFETY: `from` is rooted, of `to`'s type, and in bounds; `to` is new and rooted, and the
    // element goes into it before anything else allocates.
    unsafe {
        let raw = from.raw();
        assert!(
            raw.kind() == T::TYPE && index < values::length(raw) && slot < to.len(),
            "element index out of bounds"
        );
        let element = raw.element(index);
        crate::unwind::raise_parked()?;
        values::set_elt(to.raw(), slot, element);
    }
    Ok(())
}

/// The length, first value and increment of a compact integer or double sequence.
#[inline]
pub fn compact_sequence(vector: impl Pointer<Value>) -> Option<(usize, f64, f64)> {
    // SAFETY: the handle is a live object, and the test reads its class and its stored info.
    unsafe { values::compact_sequence(vector.raw()) }
}

/// A pairlist or call as a list with the cells' tags as names.
#[inline]
pub fn pairlist_to_list<'s>(context: &mut Cx<'s>, list: Local<'_>) -> R<Local<'s>> {
    // SAFETY: the pairlist is rooted, and the list allocates, which `&mut Cx` permits.
    Ok(unsafe { rooted(context, values::pairlist_to_list(list.raw())?) })
}

/// A list as a pairlist or call of `kind`, its names as the cells' tags.
#[inline]
pub fn list_to_pairlist<'s>(
    context: &mut Cx<'s>,
    list: Local<'_, List>,
    kind: SexpType,
) -> R<Owned<'s>> {
    assert!(
        matches!(kind, SexpType::Pairlist | SexpType::Language),
        "a pairlist kind"
    );
    // SAFETY: the list is rooted, and the cells allocate, which `&mut Cx` permits. The cells
    // are new, so only this handle reaches them.
    unsafe {
        let cells = values::list_to_pairlist(list.raw(), kind)?;
        Ok(Owned::assume(rooted(context, cells)))
    }
}

/// Element `index` of an atomic vector as a string, as `as.character` converts it.
#[inline]
pub fn element_text<'s>(
    context: &mut Cx<'s>,
    vector: Local<'s>,
    index: usize,
) -> R<Local<'s, Chars>> {
    // SAFETY: the vector is rooted and the index in bounds, and the conversion allocates,
    // which `&mut Cx` permits. The string is rooted before anything else runs.
    unsafe {
        assert!(
            index < values::length(vector.raw()),
            "element index out of bounds"
        );
        let chars = crate::coerce::element_as_charsxp(vector.raw(), index)?;
        Ok(context.root(Gc::<Chars>::adopt(chars)))
    }
}

/// Signals an error condition of `classes` with `fields` besides the message and the call,
/// then reports it as the default handler does. `None` names the running call.
pub fn classed_error(
    _context: &mut Cx<'_>,
    call: Option<Local<'_>>,
    message: &str,
    classes: &[&str],
    fields: &[(&str, Local<'_>)],
) -> Unwind {
    let fields: SmallVec<[(&str, Sexp); 4]> = fields
        .iter()
        .map(|&(name, value)| (name, value.raw()))
        .collect();
    let call = call.map_or_else(values::unbound, |call| call.raw());
    // SAFETY: the fields and the call are rooted, and the handlers are R code, which `&mut Cx`
    // permits.
    unsafe { conditions::classed_error(call, message, classes, &fields) }
}

/// The empty argument, as an empty symbol in a call evaluates, which the heap keeps for as
/// long as it lives.
#[inline]
pub fn missing_argument<'s>() -> Local<'s> {
    // SAFETY: the missing-argument marker is a permanent root of the heap.
    unsafe { Local::adopt(values::missing_arg()) }
}

/// `x[i]` for an attribute-free atomic vector and a plain numeric scalar within bounds, read
/// through an ALTREP class's `Elt` method without expanding the vector. `None` otherwise.
#[inline(always)]
pub fn scalar_subset<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    index: Local<'s>,
) -> R<Option<Local<'s>>> {
    // SAFETY: both are rooted, and the element allocates, which `&mut Cx` permits; it is rooted
    // before anything else runs.
    unsafe {
        let element = crate::element_access::scalar_subset(object.raw(), index.raw())?;
        Ok(element.map(|element| rooted(context, element)))
    }
}

/// `x[...]` for a ry Array, which makes an Array.
pub fn array_subset<'s>(
    context: &mut Cx<'s>,
    array: Local<'s>,
    indices: &[Local<'s>],
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|index| index.raw()).collect();
    let call = call.map_or_else(values::unbound, |call| call.raw());
    // SAFETY: the array, the subscripts and the call are rooted, and the result allocates,
    // which `&mut Cx` permits.
    unsafe {
        let result = crate::arrays::subset(array.raw(), &indices, call)?;
        Ok(rooted(context, result))
    }
}

/// Copies every attribute of `from` onto `to`, with the object and S4 bits.
pub fn copy_attributes_to<'s>(_context: &mut Cx<'s>, from: Local<'_>, to: &mut Owned<'s>) -> R<()> {
    // SAFETY: both are rooted, the caller owns `to`, and the attribute cells allocate, which
    // `&mut Cx` permits.
    unsafe { values::copy_attributes(from.raw(), to.get().raw()) }
}

/// `x[[...]]` on a ry Array.
pub fn array_subset2<'s>(
    context: &mut Cx<'s>,
    array: Local<'s>,
    indices: &[Local<'s>],
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|index| index.raw()).collect();
    let call = call.map_or_else(values::unbound, |call| call.raw());
    // SAFETY: the array, the subscripts and the call are rooted, and the result allocates,
    // which `&mut Cx` permits.
    unsafe {
        let result = crate::arrays::subset2(array.raw(), &indices, call)?;
        Ok(rooted(context, result))
    }
}

/// `x[[...]]` on a ry record.
pub fn record_subset2<'s>(
    context: &mut Cx<'s>,
    record: Local<'s>,
    indices: &[Local<'s>],
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|index| index.raw()).collect();
    let call = call.map_or_else(values::unbound, |call| call.raw());
    // SAFETY: the record, the subscripts and the call are rooted, and the result allocates,
    // which `&mut Cx` permits.
    unsafe {
        let result = crate::records::subset2(record.raw(), &indices, call)?;
        Ok(rooted(context, result))
    }
}

/// The value `name` has in the frame of `environment` itself, its promise forced, or `None`
/// when the frame does not bind it: `e[["name"]]` and `e$name`.
#[inline]
pub fn frame_value<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s>,
    name: Local<'_, Chars>,
) -> R<Option<Local<'s>>> {
    let symbol = install(context, name)?;
    // SAFETY: the environment and the symbol are rooted, and a lookup and the promise it
    // forces run R code, which `&mut Cx` permits.
    unsafe {
        crate::moves::reaching_environment()?;
        let value = crate::eval::find_var_in_frame(symbol.raw(), environment.raw())?;
        if value == values::unbound() {
            return Ok(None);
        }
        // A promise roots itself while its code runs, and it holds its value afterwards.
        let value = match value.kind() {
            SexpType::Promise => crate::eval::force_promise(value)?,
            _ => value,
        };
        Ok(Some(rooted(context, value)))
    }
}

/// `record$name` on a ry record.
pub fn record_dollar<'s>(
    context: &mut Cx<'s>,
    record: Local<'s>,
    name: Local<'_, Chars>,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let call = call.map_or_else(values::unbound, |call| call.raw());
    // SAFETY: the record, the name and the call are rooted, and the result allocates, which
    // `&mut Cx` permits.
    unsafe {
        let result = crate::records::dollar(record.raw(), name.bytes(context), call)?;
        Ok(rooted(context, result))
    }
}

/// Marks `value` as shared for good, so no later assignment modifies it in place.
pub fn mark_shared(value: Local<'_>) {
    // SAFETY: the value is rooted, and saturating its reference count only makes later writes
    // copy.
    unsafe { value.raw().mark_not_mutable() }
}

/// `expression` evaluated in `environment`.
pub fn evaluate<'s>(
    context: &mut Cx<'s>,
    expression: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    // SAFETY: both are rooted, and evaluation runs R code, which `&mut Cx` permits.
    unsafe {
        let value = crate::eval::eval(expression.raw(), environment.raw())?;
        Ok(rooted(context, value))
    }
}

/// `value`, or the value of the promise it is, forced.
pub fn force<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s>> {
    // SAFETY: the value is rooted, and forcing a promise runs R code, which `&mut Cx` permits.
    unsafe {
        let value = crate::eval::value_of(value.raw())?;
        Ok(rooted(context, value))
    }
}

/// The S3 method of the internal generic `generic` for `arguments` applied, or `None` when
/// the first argument's class has none.
#[inline]
pub fn dispatch_builtin<'s, const COUNT: usize>(
    context: &mut Cx<'s>,
    generic: Gc<'_, Symbol>,
    call: Local<'s>,
    arguments: [Local<'s>; COUNT],
    environment: Local<'s, Environment>,
) -> R<Option<Local<'s>>> {
    let arguments = arguments.map(|argument| crate::builtins::Arg {
        tag: values::nil(),
        value: argument.raw(),
    });
    // SAFETY: the generic, the call, the arguments and the environment are rooted, and the
    // method runs R code, which `&mut Cx` permits.
    unsafe {
        let value = crate::dispatch::dispatch_builtin(
            generic.raw(),
            call.raw(),
            &arguments,
            environment.raw(),
        )?;
        Ok(value.map(|value| rooted(context, value)))
    }
}

/// The environment a special's call runs in, made on demand for a slot frame.
pub fn call_environment<'s>(context: &mut Cx<'s>, call: &Call<'_>) -> R<Local<'s, Environment>> {
    // SAFETY: making the frame's environment allocates, which `&mut Cx` permits, and the
    // result is rooted before anything else allocates.
    let environment = unsafe { rooted(context, call.env()?) };
    environment
        .downcast::<Environment>()
        .ok_or_else(|| error("internal error: a call has no environment"))
}

/// A symbol's handle for any scope: the heap roots every symbol for its whole life.
#[inline]
pub fn permanent(symbol: Local<'_, Symbol>) -> Gc<'static, Symbol> {
    // SAFETY: the symbol is live, and the symbol table keeps it so.
    unsafe { Gc::adopt(symbol.raw()) }
}

/// [`with`] for a raw function whose callers treat its result as rooted in their own scope, as
/// they do the result of a raw allocation: the result is rooted again once the body's roots are
/// released.
///
/// # Safety
///
/// As for [`with`].
#[inline]
pub unsafe fn with_result<T: crate::unwind::Rooted>(
    body: impl for<'s> FnOnce(&mut Cx<'s>) -> R<T>,
) -> R<T> {
    // SAFETY: the caller's contract.
    let result = unsafe { with(body) };
    if let Ok(value) = &result {
        value.root();
    }
    result
}

impl<'s> Args<'_, 's> {
    /// The tag of the argument at `index`, `NULL` for an untagged one or past the last one.
    #[inline]
    pub fn tag(&self, index: usize) -> Local<'s> {
        let tag = self
            .call
            .args
            .get(index)
            .map_or(values::nil(), |argument| argument.tag);
        // SAFETY: a tag is a symbol or `NULL`, which the heap keeps for as long as it lives.
        unsafe { Local::adopt(tag) }
    }
}

/// A copy of `value` with its own storage, for a caller that changes it where it is: GNU R's
/// `shallow_duplicate`, which also makes one of ry's Arrays a plain R vector.
pub fn shallow_copy<'s>(context: &mut Cx<'s>, value: Local<'_>) -> R<Owned<'s>> {
    // SAFETY: the value is rooted, and copying allocates, which `&mut Cx` permits. The copy is
    // new, so only this handle reaches it.
    unsafe {
        let copy = values::shallow_duplicate(value.raw())?;
        Ok(Owned::assume(rooted(context, copy)))
    }
}

/// Copies the attributes of `from` onto `to` with its object and S4 bits, leaving out the
/// names, dim and dimnames unless `keep_names_dims` asks for them.
pub fn copy_most_attributes<T: Kind>(
    _context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Owned<'_, T>,
    keep_names_dims: bool,
) -> R<()> {
    // SAFETY: both objects are rooted, the caller owns `to`, and setting an attribute
    // allocates, which `&mut Cx` permits.
    unsafe { values::copy_most_attributes(from.raw(), to.get().raw(), keep_names_dims) }
}

/// Resizes `vector` and its names where they are to `length` elements, as GNU R grows a
/// vector that has room: true when both are resizable vectors with that much room that nothing
/// else refers to and `vector` has no dim or dimnames, and otherwise false, having changed
/// nothing. Elements past the old length are zeros, `NULL`s or empty strings.
pub fn resize_in_place<T: Vector>(
    _context: &mut Cx<'_>,
    vector: &mut Owned<'_, T>,
    length: usize,
) -> R<bool> {
    let heap = crate::heap();
    let symbols = values::symbols();
    let raw = vector.get().raw();
    // SAFETY: the vector is rooted, and the tests read the headers of it, its names and its
    // attribute list. The caller owns the vector, and its names have a reference count of at
    // most one, the vector's attribute, so no other value sees either resize. Both have the room.
    unsafe {
        let room = |object: Sexp| {
            heap.is_resizable(object)
                && !object.is_altrep()
                && length as isize <= object.capacity()
                && !values::maybe_shared(object)
        };
        let names = values::get_attr(raw, symbols.names);
        let fits = values::get_attr(raw, symbols.dim) == values::nil()
            && values::get_attr(raw, symbols.dimnames) == values::nil()
            && room(raw)
            && (names == values::nil() || (names.kind() == SexpType::String && room(names)));
        if !fits {
            return Ok(false);
        }
        heap.resize_vector(raw, length as isize)?;
        if names != values::nil() {
            heap.resize_vector(names, length as isize)?;
        }
    }
    Ok(true)
}

/// The names of the owned `vector`, to change where they are, when they are a standard
/// character vector that only the vector's attribute refers to.
pub fn owned_names<'s, T: Kind>(
    context: &mut Cx<'s>,
    vector: &mut Owned<'s, T>,
) -> Option<Owned<'s, Str>> {
    // SAFETY: the vector is rooted, and its attributes are a pairlist this only reads. A
    // reference count of at most one is the attribute's, which belongs to the vector the caller
    // owns, so nothing else observes the names; the root keeps them alive should the attribute
    // be replaced.
    unsafe {
        let names = values::get_attr(vector.get().raw(), values::symbols().names);
        if names.kind() != SexpType::String || names.is_altrep() || values::maybe_shared(names) {
            return None;
        }
        let names = rooted(context, names).downcast::<Str>()?;
        Some(Owned::assume(names))
    }
}

/// A new vector of `length` elements with room to grow in place by GNU R's factor of 1.05.
pub fn alloc_with_room<'s, T: Vector>(context: &mut Cx<'s>, length: usize) -> R<Fresh<'s, T>> {
    let capacity = length + length / 20;
    let heap = crate::heap();
    // SAFETY: `T::TYPE` is a vector type, the new vector is rooted before anything else
    // allocates, and shrinking it to `length` is within its capacity.
    unsafe {
        let vector = heap.alloc_resizable_vector(T::TYPE, capacity as isize)?;
        let vector = fresh::<T>(context, vector);
        heap.resize_vector(vector.raw(), length as isize)?;
        Ok(vector)
    }
}

/// Moves every element of the list `from` to the start of the new list `to`, leaving `from`
/// empty: a list that grows by copying passes its references on instead of counting each one
/// into the copy and back out of the original.
pub fn move_elements<T: Elements>(
    _context: &mut Cx<'_>,
    to: &mut Fresh<'_, T>,
    from: &mut Owned<'_, T>,
) {
    assert!(
        matches!(T::TYPE, SexpType::List | SexpType::Expression),
        "moving the elements of a vector that is not a list"
    );
    assert!(from.get().len() <= to.len(), "element index out of bounds");
    // SAFETY: both are standard lists, distinct because `to` is new, whose slots still hold the
    // `NULL` a new list starts with. The caller owns `from`, so what it leaves empty is seen by
    // no other value.
    unsafe { crate::heap().move_elements(to.raw(), from.get().raw()) }
}

/// Stores `count` elements of `from` from `from_start` into `to` from `to_start`, with one
/// barrier decision for the whole run. When `from` is `to`, the two ranges must not overlap.
pub fn copy_elements<T: Elements>(
    _context: &mut Cx<'_>,
    to: &mut Owned<'_, T>,
    to_start: usize,
    from: Local<'_, T>,
    from_start: usize,
    count: usize,
) {
    let target = to.get();
    assert!(
        to_start + count <= target.len() && from_start + count <= from.len(),
        "element index out of bounds"
    );
    assert!(
        target.raw() != from.raw()
            || to_start + count <= from_start
            || from_start + count <= to_start,
        "overlapping element ranges"
    );
    // SAFETY: both are standard vectors of one handle kind, the ranges are in bounds and apart,
    // and the caller owns `to`.
    unsafe { crate::heap().copy_elements(target.raw(), to_start, from.raw(), from_start, count) }
}

/// `call` in the form an error raised for the replacement call of an assignment names.
pub fn generated_call<'s>(context: &mut Cx<'s>, call: Local<'s>) -> R<Local<'s>> {
    // SAFETY: the call is rooted, and rebuilding it allocates, which `&mut Cx` permits.
    unsafe { Ok(rooted(context, crate::unwind::generated_call(call.raw())?)) }
}

/// Binds `symbol` to `value` in `environment`, as `assign` does.
pub fn define(
    _context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    value: Local<'_>,
    environment: Local<'_, Environment>,
) -> R<()> {
    // SAFETY: the three handles are rooted, and a binding allocates and may run an active
    // binding's function, which `&mut Cx` permits.
    unsafe { crate::eval::define(symbol.raw(), value.raw(), environment.raw()) }
}

/// Ends the moves an assignment into an environment would make observable.
pub fn reaching_environment(_context: &mut Cx<'_>) -> R<()> {
    // SAFETY: ending a move may run R code, which `&mut Cx` permits.
    unsafe { crate::moves::reaching_environment() }
}

/// `x[...] <- value` (`x[[...]] <- value` for `double`) on one of ry's Arrays, or `None` when
/// the value makes the result a plain R vector.
pub fn array_subassign<'s>(
    context: &mut Cx<'s>,
    array: Local<'s>,
    indices: &[Local<'s>],
    value: Local<'s>,
    call: Local<'s>,
    double: bool,
) -> R<Option<Local<'s>>> {
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|index| index.raw()).collect();
    // SAFETY: every handle is rooted, and the assignment allocates and copies a shared array,
    // which `&mut Cx` permits.
    unsafe {
        let result = if double {
            crate::arrays::subassign2(array.raw(), &indices, value.raw(), call.raw())?
        } else {
            crate::arrays::subassign(array.raw(), &indices, value.raw(), call.raw())?
        };
        Ok(result.map(|result| rooted(context, result)))
    }
}

/// `x[[...]] <- value` on one of ry's records.
pub fn record_subassign2<'s>(
    context: &mut Cx<'s>,
    record: Local<'s>,
    indices: &[Local<'s>],
    value: Local<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    let indices: SmallVec<[Sexp; 4]> = indices.iter().map(|index| index.raw()).collect();
    // SAFETY: every handle is rooted, and the assignment allocates, which `&mut Cx` permits.
    unsafe {
        let result = crate::records::subassign2(record.raw(), &indices, value.raw(), call.raw())?;
        Ok(rooted(context, result))
    }
}

/// `x$name <- value` on one of ry's records.
pub fn record_dollar_assign<'s>(
    context: &mut Cx<'s>,
    record: Local<'s>,
    name: &[u8],
    value: Local<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    // SAFETY: every handle is rooted, `name` is the caller's own buffer, and the assignment
    // allocates, which `&mut Cx` permits.
    unsafe {
        let result = crate::records::dollar_assign(record.raw(), name, value.raw(), call.raw())?;
        Ok(rooted(context, result))
    }
}

/// The type name GNU R's errors give `value`, as `typeof` reports it.
pub fn type_name(value: impl Pointer<Value>) -> &'static str {
    // SAFETY: the handle is a live object, and the name is read from its header.
    unsafe { crate::coerce::type_name(value.raw()) }
}

/// Raises a warning that names `call`.
pub fn warning_in(_context: &mut Cx<'_>, call: Local<'_>, message: impl Into<String>) -> R<()> {
    // SAFETY: a warning may run a calling handler, which is R code and so needs `&mut Cx`.
    unsafe { conditions::warning_call(call.raw(), message) }
}

/// The symbol spelled by `name`.
pub fn install_bytes(_context: &mut Cx<'_>, name: &[u8]) -> R<Gc<'static, Symbol>> {
    // SAFETY: interning allocates, which `&mut Cx` permits, and the heap keeps a symbol for as
    // long as it lives.
    unsafe { Ok(Gc::adopt(values::install_bytes(name)?)) }
}

/// Evaluates the call `function(arguments...)` in the base environment, with the function and
/// the arguments in the call as they are.
pub fn call_value<'s>(
    context: &mut Cx<'s>,
    function: Local<'s>,
    arguments: &[Local<'s>],
) -> R<Local<'s>> {
    let arguments: SmallVec<[Sexp; 4]> = arguments.iter().map(|argument| argument.raw()).collect();
    // SAFETY: the function and the arguments are rooted, and the call is rooted before the
    // evaluation, which may run any R code, as `&mut Cx` permits.
    unsafe {
        let call = rooted(context, values::call(function.raw(), &arguments)?);
        Ok(rooted(
            context,
            crate::eval::eval(call.raw(), values::base_env())?,
        ))
    }
}
