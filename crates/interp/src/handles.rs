//! The interpreter's side of [`heap::handle`]: how a builtin written in safe Rust is entered, how
//! its arguments become handles, and safe forms of the value helpers such a builtin calls.
//!
//! A helper here wraps its raw counterpart in `values` or `coerce` and states why the handle
//! types make the call sound. `docs/src/content/docs/development/design/memory-safety.md` is
//! the design.

#![deny(unsafe_op_in_unsafe_fn)]

pub use heap::handle::{
    Alt, Atomic, Chars, Complex, Cx, Elements, Environment, Expression, Fresh, Gc, Identity,
    Integer, Kind, List, Local, Logical, Owned, Pointer, Raw, Real, Str, Symbol, Value, Vector,
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
#[inline]
pub fn install<'s>(context: &mut Cx<'s>, name: Local<'_, Chars>) -> R<Local<'s, Symbol>> {
    // SAFETY: the string is rooted.
    unsafe { install_raw(context, name.raw()) }
}

/// [`install`] for element `at` of `names`, which the rooted vector keeps alive, so the name
/// needs no root of its own.
#[inline]
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
#[inline]
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

/// Whether R code dispatches on `value`, which its class attribute decides.
#[inline]
pub fn is_object(value: impl Pointer<Value>) -> bool {
    // SAFETY: the handle is a live object, and the test reads its header.
    unsafe { value.raw().is_object() }
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
        Scalar::Real(value) => {
            let mut scalar = context.alloc::<Real>(1)?;
            scalar.as_mut_slice()[0] = value;
            scalar.finish().value()
        }
        Scalar::Integer(value) => {
            let mut scalar = context.alloc::<Integer>(1)?;
            scalar.as_mut_slice()[0] = value;
            scalar.finish().value()
        }
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
#[inline]
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
    Ok(shallow_duplicate(context, vector)?.owned())
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
#[inline]
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
    let result = body(&mut context);
    heap.release_handles(mark);
    if let Err(unwind) = &result {
        unwind.root();
    }
    result
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
/// would cost every store.
///
/// # Safety
///
/// `raw` is a live object, and a root of the caller's keeps it alive until [`with`] returns,
/// whatever R code runs in between.
#[inline]
pub unsafe fn held<'s>(_context: &Cx<'s>, raw: Sexp) -> Local<'s> {
    // SAFETY: the caller's contract.
    unsafe { Local::adopt(raw) }
}
