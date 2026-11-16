//! The interpreter's side of [`heap::handle`]: how a builtin written in safe Rust is entered, how
//! its arguments become handles, and safe forms of the value helpers such a builtin calls.
//!
//! A helper here wraps its raw counterpart in `values` or `coerce` and states why the handle
//! types make the call sound. `docs/src/content/docs/development/design/memory-safety.md` is
//! the design.

#![deny(unsafe_op_in_unsafe_fn)]

pub use heap::handle::{
    Alt, Atomic, Chars, Complex, Cx, Elements, Expression, Fresh, Gc, Integer, Kind, List, Local,
    Logical, Pointer, Raw, Real, Str, Value, Vector,
};
use {
    crate::{
        builtins::Call,
        coerce::coerce_vector,
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
unsafe fn rooted<'s>(context: &Cx<'s>, raw: Sexp) -> Local<'s> {
    // SAFETY: the caller's contract, and a `Value` handle admits every object.
    context.root(unsafe { Gc::<Value>::adopt(raw) })
}

/// Whether the class attribute of `value` names `class`.
pub fn inherits(_context: &Cx<'_>, value: impl Pointer<Value>, class: &str) -> bool {
    // SAFETY: the handle is a live object, and the test only reads its class attribute.
    unsafe { values::inherits(value.raw(), class) }
}

/// The first element of a character vector as text, or `None` for `NA`, an empty vector and
/// any other type. The text borrows the context mutably because an ALTREP class may compute
/// the element, and then only the borrow keeps it alive.
pub fn first_string<'a>(_context: &'a mut Cx<'_>, value: Local<'_>) -> Option<Cow<'a, str>> {
    // SAFETY: the vector is rooted, an element read may run its class's method, which
    // `&mut Cx` permits, and nothing collects while the text borrows the context.
    unsafe { values::first_string(value.raw()) }
}

/// The first element as a logical: `NA` for an empty vector or a type with no logical reading.
pub fn logical_scalar(_context: &mut Cx<'_>, value: Local<'_>) -> i32 {
    // SAFETY: the value is rooted, and reading an ALTREP's first element may allocate, which
    // `&mut Cx` permits.
    unsafe { values::as_logical_scalar(value.raw()) }
}

/// `value` as a standard vector of kind `T`: itself when it is one, its ALTREP class's
/// standard form when it is an ALTREP object of that type, and `None` for any other type.
pub fn dense<'s, T: Vector>(context: &mut Cx<'s>, value: Local<'s>) -> R<Option<Local<'s, T>>> {
    if value.r_type() != T::TYPE {
        return Ok(None);
    }
    if let Some(vector) = value.downcast::<T>() {
        return Ok(Some(vector));
    }
    // SAFETY: the object is rooted, and expanding it may allocate or run its class's
    // methods, which `&mut Cx` permits.
    let vector = unsafe { values::materialize(value.raw())? };
    // SAFETY: `materialize` returned a live object and nothing has allocated since.
    Ok(unsafe { rooted(context, vector) }.downcast::<T>())
}

/// `value` as a standard character vector, coerced as `as.character` coerces when it is of
/// another type.
pub fn character_vector<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Str>> {
    let text = if value.r_type() == SexpType::String {
        value
    } else {
        // SAFETY: the value is rooted, and coercion allocates, which `&mut Cx` permits.
        let coerced = unsafe { coerce_vector(value.raw(), SexpType::String)? };
        // SAFETY: coercion returned a live object and nothing has allocated since.
        unsafe { rooted(context, coerced) }
    };
    dense::<Str>(context, text)?
        .ok_or_else(|| error("internal error: a character vector has no standard form"))
}

/// A string's text, decoded when it is marked latin1 or bytes.
pub fn text<'a>(chars: Gc<'a, Chars>) -> Cow<'a, str> {
    // SAFETY: the string is live for `'a`, which bounds the text borrowed from it.
    unsafe { values::char_str(chars.raw()) }
}

/// Whether `chars` is `NA_character_`.
pub fn is_na(chars: Gc<'_, Chars>) -> bool {
    chars.raw() == values::na_string()
}

/// Whether `chars` is marked as bytes, which have no character reading.
pub fn is_bytes(chars: Gc<'_, Chars>) -> bool {
    chars.has_mark(gp::BYTES)
}

/// Whether `chars` is marked latin1.
pub fn is_latin1(chars: Gc<'_, Chars>) -> bool {
    chars.has_mark(gp::LATIN1)
}

/// Copies the names, dim and dimnames of `from` onto the new vector `to`.
pub fn copy_shape<T: Vector>(
    _context: &mut Cx<'_>,
    from: Local<'_>,
    to: &mut Fresh<'_, T>,
) -> R<()> {
    let symbols = values::symbols();
    for symbol in [symbols.names, symbols.dim, symbols.dimnames] {
        // SAFETY: both objects are rooted, `to` is visible to nothing else, and reading or
        // setting an attribute may allocate, which `&mut Cx` permits. `set_attr` roots
        // nothing it must keep: the attribute hangs off `to` once it returns.
        unsafe {
            // GNU R reads the names through `getAttrib`, which turns a pairlist's tags into
            // names.
            let value = if symbol == symbols.names
                && matches!(from.r_type(), SexpType::Pairlist | SexpType::Language)
            {
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
