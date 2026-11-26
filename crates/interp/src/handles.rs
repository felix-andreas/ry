//! The interpreter's side of [`heap::handle`]: how a builtin written in safe Rust is entered, how
//! its arguments become handles, and safe forms of the value helpers such a builtin calls.
//!
//! A helper here wraps its raw counterpart in `values` or `coerce` and states why the handle
//! types make the call sound. `docs/src/content/docs/development/design/memory-safety.md` is
//! the design.

#![deny(unsafe_op_in_unsafe_fn)]

pub use heap::handle::{
    Alt, Atomic, Binding, Chars, Complex, Cx, Elements, Enclosing, Environment, Exact, Expression,
    Fresh, Gc, I64, Identity, Integer, Item, Kind, Language, List, Local, Logical, Owned, Pairlist,
    Pointer, Promise, Raw, Real, Str, Symbol, Unfilled, Value, Vector,
};
use {
    crate::{
        builtin_libraries::NativeCall,
        builtins::{Arg, Call},
        coerce::coerce_vector,
        conditions, connections,
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

/// The raw entry of the safe builtin `$function`, for the builtin table, or with `native` of the
/// safe native routine `$function`, for a builtin library's routine table.
#[macro_export]
macro_rules! safe {
    (native $function:path) => {
        $crate::safe!(@entry $crate::builtin_libraries::NativeCall<'_>, $function)
    };
    ($function:path) => {
        $crate::safe!(@entry $crate::builtins::Call<'_>, $function)
    };
    (@entry $call:ty, $function:path) => {{
        unsafe fn entry(call: &$call) -> $crate::unwind::R<::heap::object::Sexp> {
            // SAFETY: the evaluator calls a builtin, and a foreign call a native routine, on
            // the interpreter's thread, with the call's arguments rooted.
            unsafe { $crate::handles::enter(call, $function) }
        }
        entry
    }};
}
pub use safe;

/// The arguments of the foreign call a safe native routine runs for.
pub type NativeArgs<'a, 's> = Args<'a, 's, NativeCall<'a>>;

/// Runs the safe builtin or native routine `body` for `call`.
///
/// # Safety
///
/// The caller is on the interpreter's thread, holds no reference into the heap, and keeps the
/// call's arguments rooted until this returns.
#[inline]
pub unsafe fn enter<'a, C: Invocation>(
    call: &'a C,
    body: impl for<'s> FnOnce(&mut Cx<'s>, &Args<'a, 's, C>) -> R<Local<'s>>,
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

/// The call a safe routine runs for: a builtin's, or the foreign call of a native routine,
/// whose arguments are the ones after `.NAME` and whose errors name the calling closure, as
/// GNU R's do.
pub trait Invocation {
    fn arguments(&self) -> &[Arg];
    fn error(&self, message: String) -> Unwind;
}

impl Invocation for Call<'_> {
    fn arguments(&self) -> &[Arg] {
        self.args
    }

    fn error(&self, message: String) -> Unwind {
        Call::error(self, message)
    }
}

impl Invocation for NativeCall<'_> {
    fn arguments(&self) -> &[Arg] {
        self.args
    }

    fn error(&self, message: String) -> Unwind {
        NativeCall::error(self, message)
    }
}

/// The arguments of the call a safe builtin or native routine runs for.
pub struct Args<'a, 's, C = Call<'a>> {
    call: &'a C,
    scope: PhantomData<fn(&'s ()) -> &'s ()>,
}

impl<'s, C: Invocation> Args<'_, 's, C> {
    pub fn len(&self) -> usize {
        self.call.arguments().len()
    }

    pub fn is_empty(&self) -> bool {
        self.call.arguments().is_empty()
    }

    /// The argument at `index`, or `NULL` past the last one.
    #[inline]
    pub fn value(&self, index: usize) -> Local<'s> {
        self.part(index, Part::Value)
    }

    /// The tag of the argument at `index`, `NULL` for an untagged one or past the last one.
    #[inline]
    pub fn tag(&self, index: usize) -> Local<'s> {
        self.part(index, Part::Tag)
    }

    /// An error reported against the call.
    pub fn error(&self, message: impl Into<String>) -> Unwind {
        self.call.error(message.into())
    }

    #[inline]
    fn part(&self, index: usize, part: Part) -> Local<'s> {
        let raw = match (self.call.arguments().get(index), part) {
            (None, _) => values::nil(),
            (Some(argument), Part::Value) => argument.value,
            (Some(argument), Part::Tag) => argument.tag,
        };
        // SAFETY: `enter`'s caller keeps every argument rooted for the call, which outlasts
        // `'s`, and a tag is a symbol or `NULL`, which the heap keeps for as long as it lives.
        unsafe { Local::adopt(raw) }
    }
}

#[derive(Clone, Copy)]
enum Part {
    Value,
    Tag,
}

/// Writes `bytes` to the standard output connection, or to the sink that diverts it.
pub fn write_output(_context: &mut Cx<'_>, bytes: &[u8]) {
    // SAFETY: writing to a sink may allocate or run a connection's methods, which `&mut Cx`
    // permits.
    unsafe { connections::write_output(bytes) }
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
    match value.standard::<T>() {
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
    Ok(unsafe { rooted(context, vector) }.standard::<T>())
}

/// A standard vector of `T` to read the elements of `value` from: itself when it is one, and
/// for an ALTREP object the vector its class keeps them in, or a copy when it keeps none. A
/// class's vector carries attributes that are not the object's, and the class writes it in
/// place later, so the caller reads its elements and neither writes nor keeps it. `None` for
/// any other type.
#[inline(always)]
pub fn elements_of<'s, T: Vector>(
    context: &mut Cx<'s>,
    value: Local<'s>,
) -> R<Option<Local<'s, T>>> {
    if value.r_type() != T::TYPE {
        return Ok(None);
    }
    match value.standard::<T>() {
        Some(vector) => Ok(Some(vector)),
        None => stored(context, value),
    }
}

/// [`elements_of`] for an ALTREP object of `T`'s type.
#[cold]
#[inline(never)]
fn stored<'s, T: Vector>(context: &mut Cx<'s>, value: Local<'s>) -> R<Option<Local<'s, T>>> {
    // SAFETY: the object is rooted, and its class's methods may allocate, which `&mut Cx`
    // permits. The result is a live object, and nothing has allocated since.
    let vector = unsafe {
        rooted(
            context,
            (crate::interp().hooks.altrep_storage)(value.raw())?,
        )
    };
    Ok(vector.standard::<T>())
}

/// Whether the class of an ALTREP vector holds its elements in memory already, as a wrapper
/// does, so reading them expands and copies nothing.
pub fn holds_elements(_context: &mut Cx<'_>, vector: Local<'_, Alt>) -> R<bool> {
    // SAFETY: the object is rooted and ALTREP, and its class's method may allocate, which
    // `&mut Cx` permits.
    Ok(unsafe { (crate::interp().hooks.altrep_dataptr_or_null)(vector.raw())? }.is_some())
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

/// Whether `==` holds for two strings, neither of them `NA`: they are one object, or their text
/// is the same under different encoding marks and neither is marked as bytes.
///
/// Strings intern by bytes and encoding, so a native string and a UTF-8 one with the same bytes
/// are distinct objects, and so are a latin1 string and its UTF-8 translation. GNU R's `Seql`
/// calls both pairs equal because it compares their UTF-8 translations.
#[inline]
pub fn equal_strings(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
    left.raw() == right.raw() || same_text_under_marks(left, right)
}

/// [`equal_strings`] for two distinct objects.
fn same_text_under_marks(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
    if left.has_mark(gp::BYTES) || right.has_mark(gp::BYTES) {
        return false;
    }
    // Two distinct strings of the same encoding differ in their bytes, which keeps `x == "a"`
    // from decoding every element.
    if [gp::LATIN1, gp::UTF8]
        .iter()
        .all(|&mark| left.has_mark(mark) == right.has_mark(mark))
    {
        return false;
    }
    // Outside a UTF-8 locale, GNU R reads a non-ASCII string of unknown encoding as native bytes
    // that do not translate to the text of a marked string.
    let native_non_ascii = |string: Gc<'_, Chars>| {
        !string.has_mark(gp::LATIN1 | gp::UTF8) && !string.bytes().is_ascii()
    };
    if (native_non_ascii(left) || native_non_ascii(right))
        && !crate::platform::native_codeset().eq_ignore_ascii_case("UTF-8")
    {
        return false;
    }
    text(left) == text(right)
}

/// The cross-check behind [`values::same_string`]'s identity rule: a cached ASCII string is the
/// only string that holds its text, so a different string with the same text means some path
/// made a `CHARSXP` without the string cache. `NA` is uncached and spells "NA".
#[cold]
#[inline(never)]
pub fn assert_distinct_text(left: Gc<'_, Chars>, right: Gc<'_, Chars>) {
    assert!(
        is_na(left) || is_na(right) || text(left) != text(right),
        "two CHARSXPs hold the text {:?}, one of them cached ASCII: a string was made outside the \
         string cache",
        text(left)
    );
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

/// A new list of new character vectors, each started and filled in order, as `strsplit` makes
/// its pieces. A vector is stored in the list as it starts, and neither a vector nor a string is
/// overwritten, so the list holds every one of them: no element needs a handle of its own, which
/// would grow the handle stack with the input, and a string stored earlier is handed out again
/// for the same bytes ([`values::StringMemo`]).
pub struct StringLists<'s> {
    list: Fresh<'s, List>,
    /// The next element of `list` to start.
    next_vector: usize,
    /// The vector being filled, which `list` holds, or nil before the first starts.
    vector: Sexp,
    length: usize,
    next: usize,
    memo: values::StringMemo<'s>,
}

impl<'s> StringLists<'s> {
    pub fn new(context: &mut Cx<'s>, length: usize) -> R<StringLists<'s>> {
        Ok(StringLists {
            list: context.alloc::<List>(length)?,
            next_vector: 0,
            vector: values::nil(),
            length: 0,
            next: 0,
            memo: values::StringMemo::default(),
        })
    }

    /// Starts the next element as a new character vector of `length` empty strings.
    pub fn start(&mut self, _context: &mut Cx<'_>, length: usize) -> R<()> {
        assert!(
            self.next_vector < self.list.len(),
            "element index out of bounds"
        );
        // SAFETY: the list is rooted and a standard list, and the index is in bounds; the new
        // vector is stored before anything else allocates, which `&mut Cx` permits.
        unsafe {
            let heap = crate::heap();
            let vector = heap.alloc_vector(SexpType::String, length as isize)?;
            heap.set_element(self.list.raw(), self.next_vector, vector);
            self.vector = vector;
        }
        self.next_vector += 1;
        self.length = length;
        self.next = 0;
        Ok(())
    }

    /// Pushes a string of `bytes` marked `encoding` onto the vector last started. A native
    /// string goes through the memo.
    pub fn push_bytes(&mut self, _context: &mut Cx<'_>, bytes: &[u8], encoding: Encoding) -> R<()> {
        assert!(self.next < self.length, "element index out of bounds");
        // SAFETY: `start` ran, since `length` is not zero, so the list holds `self.vector`, a
        // character vector of `length` elements. The new string is stored before anything else
        // allocates, and a string the memo hands out is stored earlier in a vector the list
        // holds, which is rooted for `'s` and never overwritten.
        unsafe {
            let made = if encoding == Encoding::Native {
                self.memo.mk_char(bytes)?
            } else {
                crate::heap().mk_char(bytes, encoding)?
            };
            crate::heap().set_element(self.vector, self.next, made);
        }
        self.next += 1;
        Ok(())
    }

    /// Pushes `NA` onto the vector last started.
    pub fn push_na(&mut self, _context: &Cx<'_>) {
        assert!(self.next < self.length, "element index out of bounds");
        // SAFETY: as in `push_bytes`; `NA` is permanent.
        unsafe { crate::heap().set_element(self.vector, self.next, values::na_string()) };
        self.next += 1;
    }

    #[inline]
    pub fn finish(self) -> Fresh<'s, List> {
        self.list
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

/// The truelength of `chars`, which code such as data.table's C uses to mark the strings it has
/// seen, and `make.unique` too.
#[inline]
pub fn char_truelength(chars: Gc<'_, Chars>) -> i32 {
    // SAFETY: the string is live, and the read is of its header.
    unsafe { chars.raw().char_truelength() }
}

/// Sets the truelength of `chars` ([`char_truelength`]).
#[inline]
pub fn set_char_truelength(_context: &Cx<'_>, chars: Gc<'_, Chars>, truelength: i32) {
    // SAFETY: the string is live, and the write changes a header field no collection reads.
    unsafe { chars.raw().set_char_truelength(truelength) }
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
        .standard::<Str>()
        .map_or(Names::Other, Names::Standard)
}

/// Ends every move of `value`, which base R code is about to change where it is.
#[inline]
pub fn modifying(_context: &mut Cx<'_>, value: Local<'_>) -> R<()> {
    // SAFETY: the value is rooted, and ending a move may run R code, which `&mut Cx` permits.
    unsafe { crate::moves::modifying(value.raw()) }
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
        && let Ok(owned) = owned.standard::<T>()
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
    if let Some(strings) = vector.standard::<Str>()
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

/// [`in_caller_scope`] for a function `body` that takes its operands as `data`. Tier 0
/// compiles each handler twice, for running and for stepping, so a closure or a generic shim
/// a handler calls has two callers and LLVM keeps a large one out of line; a call through a
/// constant function pointer to an `#[inline(always)]` function inlines into both.
///
/// # Safety
///
/// As for [`in_caller_scope`].
#[inline(always)]
pub unsafe fn in_caller_scope_with<D, T>(
    data: D,
    body: for<'s> fn(&mut Cx<'s>, D) -> R<T>,
) -> R<T> {
    // SAFETY: as in `with`. The scope `body` sees ends no later than the caller's, which it
    // cannot outlive, because it must accept every `'s` and `data` was made before it began.
    let mut context = unsafe { Cx::enter(NonNull::from(&mut *crate::heap())) };
    body(&mut context, data)
}

/// [`in_caller_scope_with`] for a raw entry that reads one object its caller holds, which
/// `body` receives as a value of the scope ([`held`]).
///
/// # Safety
///
/// As for [`in_caller_scope`], and the caller keeps `object` alive until this returns.
#[inline(always)]
pub unsafe fn held_in_caller_scope<T>(
    object: Sexp,
    body: for<'s> fn(&mut Cx<'s>, Local<'s>) -> R<T>,
) -> R<T> {
    // SAFETY: as in `in_caller_scope_with`, and the caller's contract keeps `object` alive.
    let mut context = unsafe { Cx::enter(NonNull::from(&mut *crate::heap())) };
    let object = unsafe { Local::adopt(object) };
    body(&mut context, object)
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

    /// The call of the closure the call was made from, or the call itself outside a closure:
    /// the call GNU R names in a warning from an internal.
    pub fn closure_call(&self, _context: &Cx<'s>) -> Local<'s> {
        // SAFETY: the frame stack holds each running closure's call, and the evaluator the call
        // it is evaluating, until the builtin returns.
        unsafe {
            let raw = self.call.frame_index().map_or(self.call.call, |position| {
                crate::interp().frames[position].call
            });
            Local::adopt(raw)
        }
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

/// [`symbol`] as a handle of any scope, which needs no root of its own: the heap's symbol
/// table keeps the symbol.
#[inline(always)]
pub fn permanent_symbol<'s>(pick: impl FnOnce(&values::Symbols) -> Sexp) -> Local<'s, Symbol> {
    // SAFETY: as in `symbol`.
    unsafe { Local::adopt(pick(values::symbols())) }
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
    if length(context, dim) != 1 {
        return Ok(null());
    }
    let dimnames = context.root(get_attribute(
        context,
        object,
        symbol(|symbols| symbols.dimnames),
    ));
    match dense::<List>(context, dimnames)? {
        Some(dimnames) if dimnames.len() == 1 => Ok(context.root(dimnames.get(context, 0))),
        _ => Ok(null()),
    }
}

/// [`names_of`] for an object whose names are not a pairlist's tags, read where they are
/// stored. It is `None` when only a safepoint can tell: the `dim` or the `dimnames` is an
/// ALTREP vector, whose length its class's method reports.
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
    match dim.standard::<Integer>() {
        Some(dim) if dim.len() == 1 => array_names(context, object),
        Some(_) => Some(null().read(context)),
        None if dim.r_type() == SexpType::Integer => None,
        None => Some(null().read(context)),
    }
}

/// The names of a one-dimensional array, which are its dimnames, or `None` when the dimnames
/// are an ALTREP list.
fn array_names<'a, 's: 'a>(context: &'a Cx<'_>, object: Local<'s>) -> Option<Gc<'a>> {
    let dimnames = get_attribute(context, object, symbol(|symbols| symbols.dimnames));
    match dimnames.standard::<List>() {
        Some(dimnames) if dimnames.len() == 1 => Some(dimnames.get(0)),
        None if dimnames.r_type() == SexpType::List => None,
        _ => Some(null().read(context)),
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

/// `value` as an argument of a call that evaluates to it: quoted when it is a symbol, a call, a
/// promise or `...`, which evaluating would change, and as it is otherwise.
pub fn quoted_value<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s>> {
    match value.r_type() {
        SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Dots => {
            quoted(context, value)
        }
        _ => Ok(value),
    }
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

/// Copies the first `count` elements of `from` into the new vector `to` in one bulk copy, which
/// for a list or a character vector counts its references once.
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
    unsafe {
        match T::TYPE {
            SexpType::String | SexpType::List | SexpType::Expression => {
                crate::heap().copy_elements(to.raw(), 0, from.raw(), 0, count);
                Ok(())
            }
            kind => {
                let size = kind
                    .element_size()
                    .ok_or_else(|| error("internal error: a copy of a vector without elements"))?;
                std::ptr::copy_nonoverlapping(
                    from.raw().data().cast_const(),
                    to.raw().data(),
                    count * size,
                );
                Ok(())
            }
        }
    }
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

/// A list as a pairlist or call of `kind`, its names as the cells' tags. `tag_na` makes an `NA`
/// name the tag `NA`, where it otherwise leaves the cell untagged.
#[inline]
pub fn list_to_pairlist<'s>(
    context: &mut Cx<'s>,
    list: Local<'_, List>,
    kind: SexpType,
    tag_na: bool,
) -> R<Owned<'s>> {
    assert!(
        matches!(kind, SexpType::Pairlist | SexpType::Language),
        "a pairlist kind"
    );
    // SAFETY: the list is rooted, and the cells allocate, which `&mut Cx` permits. The cells
    // are new, so only this handle reaches them.
    unsafe {
        let cells = values::list_to_pairlist(list.raw(), kind, tag_na)?;
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
    frame_symbol_value(context, environment, symbol)
}

/// [`frame_value`] for the variable `symbol`.
pub fn frame_symbol_value<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s>,
    symbol: impl Pointer<Symbol>,
) -> R<Option<Local<'s>>> {
    // SAFETY: the environment is rooted and symbols are permanent, and a lookup and the
    // promise it forces run R code, which `&mut Cx` permits.
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
        let names = rooted(context, names).standard::<Str>()?;
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

/// The binding of `symbol` in the frame of `environment` itself.
#[inline]
pub fn frame_binding<'a>(
    _context: &'a Cx<'_>,
    environment: impl Pointer<Environment>,
    symbol: impl Pointer<Symbol>,
) -> Option<Gc<'a, Binding>> {
    // SAFETY: both are live. A lookup in a slot frame's environment may move the variable from
    // its slot into a new binding cell, and collection is inhibited while it does
    // (`eval::resolve_pending_access`), so no handle the context lends out goes stale.
    unsafe {
        let cell = crate::heap().frame_cell(environment.raw(), symbol.raw())?;
        Some(Gc::adopt(cell))
    }
}

/// The binding `symbol` reads from `environment` outward.
#[inline]
pub fn visible_binding<'a>(
    _context: &'a Cx<'_>,
    symbol: impl Pointer<Symbol>,
    environment: impl Pointer<Environment>,
) -> Option<Gc<'a, Binding>> {
    // SAFETY: both are live, and a binding cell the walk makes for a slot frame's variable is
    // made with collection inhibited, as in `frame_binding`.
    unsafe { crate::eval::find_cell(symbol.raw(), environment.raw()).map(|cell| Gc::adopt(cell)) }
}

/// The binding `<<-` assigns `symbol` through, searching from `enclosure`, and the environment
/// whose frame holds it.
pub fn enclosing_binding<'a>(
    context: &'a Cx<'_>,
    symbol: impl Pointer<Symbol>,
    enclosure: impl Pointer<Environment>,
) -> Option<(Gc<'a, Environment>, Gc<'a, Binding>)> {
    let heap = crate::heap();
    let empty = heap.known().empty_env;
    let mut environment = enclosure.raw();
    while environment != empty {
        // SAFETY: the environment is live, reached from a live one through its enclosure, and
        // nothing here collects (`frame_binding`).
        let environment_handle = unsafe { Gc::<Environment>::adopt(environment) };
        if let Some(cell) = frame_binding(context, environment_handle, symbol) {
            return Some((environment_handle, cell));
        }
        // SAFETY: as above.
        environment = unsafe { environment.enclosure() };
    }
    None
}

/// Clears the marks that call a binding's variable a missing argument, which an assignment to
/// it ends.
#[inline]
pub fn mark_assigned(_context: &Cx<'_>, binding: impl Pointer<Binding>) {
    // SAFETY: the cell is live, and the write changes two flags of its header.
    unsafe {
        binding
            .raw()
            .set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false)
    }
}

/// `symbol <- value` in `environment`, whose own frame binds `symbol` to `binding`, or not at
/// all.
pub fn assign_local(
    _context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    value: Local<'_>,
    environment: Local<'_, Environment>,
    binding: Option<Local<'_, Binding>>,
) -> R<()> {
    // SAFETY: every handle is rooted, and a new binding allocates and an active binding runs
    // its function, which `&mut Cx` permits.
    unsafe {
        crate::eval::assign_local(
            symbol.raw(),
            value.raw(),
            environment.raw(),
            binding.map(Local::raw),
        )
    }
}

/// `symbol <<- value` from `environment`: the nearest binding outward changes, or the global
/// environment gains one.
pub fn assign_outward(
    _context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    value: Local<'_>,
    environment: Local<'_, Environment>,
) -> R<()> {
    // SAFETY: every handle is rooted, and a binding allocates and may run an active binding's
    // function, which `&mut Cx` permits.
    unsafe { crate::eval::set_var(symbol.raw(), value.raw(), environment.raw()) }
}

/// The variable `symbol` a replacement in a slot frame changes, read from the frame's
/// `enclosure` because its slots do not bind it.
pub fn enclosed_value<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'_, Symbol>,
    enclosure: Local<'_, Environment>,
) -> R<Local<'s>> {
    // SAFETY: both are rooted, and the lookup forces promises and may signal, which `&mut Cx`
    // permits.
    unsafe {
        let value = crate::eval::enclosed_value(symbol.raw(), enclosure.raw())?;
        Ok(rooted(context, value))
    }
}

/// The value of `symbol` in `environment`, as evaluating the name gives it.
pub fn symbol_value<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'_, Symbol>,
    environment: Local<'_, Environment>,
) -> R<Local<'s>> {
    // SAFETY: both are rooted, and the lookup forces promises, which `&mut Cx` permits.
    unsafe {
        let value = crate::eval::eval_symbol(symbol.raw(), environment.raw())?;
        Ok(rooted(context, value))
    }
}

/// Whether `symbol` is a missing argument of the frame of `environment`, as `missing()` asks.
pub fn is_missing_in(
    _context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    environment: Local<'_, Environment>,
) -> R<bool> {
    // SAFETY: both are rooted, and the test may force a promise, which `&mut Cx` permits.
    unsafe { crate::eval::argument_is_missing(symbol.raw(), environment.raw()) }
}

/// Whether `symbol` is a missing argument in `environment`, counting a defaulted formal as
/// missing in its own function, as `missing()` does.
pub fn is_missing(
    _context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    environment: Local<'_, Environment>,
) -> R<bool> {
    // SAFETY: both are rooted, and the test may force a promise, which `&mut Cx` permits.
    unsafe { crate::eval::is_missing(symbol.raw(), environment.raw()) }
}

/// The namespace registered as `name`, if it is loaded.
pub fn registered_namespace<'s>(context: &mut Cx<'s>, name: &str) -> R<Option<Local<'s>>> {
    let symbol = install_bytes(context, name.as_bytes())?;
    // SAFETY: the registry is a permanent root of the heap.
    let registry = unsafe { Local::adopt(crate::heap().known().namespace_registry) };
    let namespace = frame_symbol_value(context, registry, symbol)?;
    Ok(namespace.filter(|namespace| namespace.r_type() == SexpType::Environment))
}

/// The value of `promise`, forced while a reference to `held` is counted, so that code the
/// promise runs copies `held` before it changes it, as GNU R's `*tmp*` keeps it.
pub fn force_holding<'s>(
    context: &mut Cx<'s>,
    promise: Local<'s, Promise>,
    held: Local<'_>,
) -> R<Local<'s>> {
    // SAFETY: both are rooted, the count is given back whatever the forcing does, and forcing
    // runs R code, which `&mut Cx` permits.
    unsafe {
        held.raw().increment_refcount();
        let value = crate::eval::force_promise(promise.raw());
        held.raw().decrement_refcount();
        Ok(rooted(context, value?))
    }
}

/// The definition of the builtin or special `function`.
#[inline]
pub fn builtin_definition(
    function: impl Pointer<Value>,
) -> R<&'static crate::builtins::BuiltinDef> {
    // SAFETY: the function is live, and the definition table is static.
    unsafe { crate::eval::builtin_def(function.raw()) }
}

/// The base primitive called `name`, for any scope: the interpreter keeps its primitives for
/// its whole life.
#[inline]
pub fn primitive(name: &str) -> Option<Gc<'static>> {
    // SAFETY: the builtin table holds each primitive object for as long as the interpreter
    // lives.
    unsafe { crate::builtins::primitive(name).map(|object| Gc::adopt(object)) }
}

/// Whether a call of the internal generic `definition`, the primitive `function`, on `object`
/// could run a method in place of the primitive's own code ([`crate::dispatch::has_internal_method`]).
/// `caller` is the environment of the calling frame, or `None` for a slot frame that has none.
pub fn has_internal_method(
    _context: &mut Cx<'_>,
    definition: &crate::builtins::BuiltinDef,
    function: impl Pointer<Value>,
    object: Local<'_>,
    caller: Option<Local<'_, Environment>>,
) -> R<bool> {
    let caller = match caller {
        Some(environment) => crate::dispatch::Caller::Env(environment.raw()),
        None => crate::dispatch::Caller::SlotFrame,
    };
    // SAFETY: the function is a permanent primitive and the object is rooted, and looking a
    // method up may force promises, which `&mut Cx` permits.
    unsafe {
        crate::dispatch::has_internal_method(definition, function.raw(), object.raw(), caller)
    }
}

/// `value` as the value `binding` in the frame of `environment` alone holds, for an assignment
/// that changes it where it is: the binding holds it directly, nothing else counts a reference
/// to it, and no suspended compiled body holds it uncounted ([`reuse_permitted`]).
#[inline]
pub fn claim_binding<'s>(
    context: &Cx<'s>,
    environment: Local<'_, Environment>,
    binding: Local<'_, Binding>,
    value: Local<'s>,
) -> Option<Owned<'s>> {
    if !binding.read(context).content().is(value)
        || value.is_shared()
        || !reuse_permitted(context, environment, value)
    {
        return None;
    }
    // SAFETY: the binding holds the value and counts its one reference, and `reuse_permitted`
    // rules out a holder that counts none.
    Some(unsafe { Owned::assume(value) })
}

/// Whether the value bound in `environment` may change in place without a suspended compiled
/// body noticing ([`crate::eval::reuse_permitted`]).
#[inline]
pub fn reuse_permitted(
    _context: &Cx<'_>,
    environment: impl Pointer<Environment>,
    value: impl Pointer<Value>,
) -> bool {
    // SAFETY: both are live, and the test reads the frame stack and the registers.
    unsafe { crate::eval::reuse_permitted(environment.raw(), value.raw()) }
}

/// The copy a replacement makes of a value its variable does not own. Each keeps ry's Array
/// flag, which an element replacement keeps.
#[derive(Clone, Copy)]
pub enum AssignmentCopy {
    /// A copy with its own storage, which a direct element write may change.
    Storage,
    /// GNU R's `shallow_duplicate_attr`: a long vector becomes an ALTREP wrapper of the
    /// original's storage, which only a replacement builtin writes through.
    Wrapper,
    /// As [`AssignmentCopy::Wrapper`], and an ALTREP vector keeps its class
    /// ([`values::replacement_copy`]).
    Replacement,
}

/// A copy of `value` for a replacement to change, as `how` makes it. It is not an [`Owned`],
/// because a wrapper shares the original's storage and only a replacement builtin, which copies
/// that storage on its first write, may change it.
#[inline]
pub fn assignment_copy<'s>(
    context: &mut Cx<'s>,
    value: Local<'_>,
    how: AssignmentCopy,
) -> R<Local<'s>> {
    // SAFETY: the value is rooted, and copying allocates, which `&mut Cx` permits.
    unsafe {
        let copy = match how {
            AssignmentCopy::Storage => values::shallow_duplicate(value.raw())?,
            AssignmentCopy::Wrapper => values::shallow_duplicate_attr(value.raw())?,
            AssignmentCopy::Replacement => values::replacement_copy(value.raw())?,
        };
        copy.set_array(value.raw().is_array());
        Ok(rooted(context, copy))
    }
}

/// The value an assignment binds for a replacement's `result`: an ALTREP wrapper the
/// replacement's copy introduced is dropped for its data once nothing else holds it
/// ([`values::unwrap_assigned`]). `bound` says whether the variable already holds `result`.
#[inline(always)]
pub fn unwrap_assigned<'s>(context: &mut Cx<'s>, result: Local<'s>, bound: bool) -> Local<'s> {
    // SAFETY: the result is rooted, and unwrapping may allocate, which `&mut Cx` permits.
    unsafe {
        let unwrapped = values::unwrap_assigned(result.raw(), bound);
        if unwrapped == result.raw() {
            result
        } else {
            rooted(context, unwrapped)
        }
    }
}

/// While it lives, the replacement call running is `setter_call` of the assignment `whole`,
/// the call the replacement primitives report and may modify their target for.
pub struct AssignmentCall<'s> {
    outer: Option<(Sexp, Sexp, usize)>,
    scope: PhantomData<fn(&'s ()) -> &'s ()>,
}

impl<'s> AssignmentCall<'s> {
    #[inline(always)]
    pub fn enter(
        _context: &Cx<'s>,
        setter_call: Local<'s>,
        whole: Local<'s>,
    ) -> AssignmentCall<'s> {
        let interpreter = crate::interp();
        let outer = interpreter.assignment_call.replace((
            setter_call.raw(),
            whole.raw(),
            interpreter.frames.len(),
        ));
        AssignmentCall {
            outer,
            scope: PhantomData,
        }
    }
}

impl Drop for AssignmentCall<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        crate::interp().assignment_call = self.outer;
    }
}

/// The unwind a failing replacement primitive raises: an error it returned is signalled,
/// naming the assignment's call.
pub fn builtin_failure(_context: &mut Cx<'_>, unwind: Unwind) -> Unwind {
    // SAFETY: signalling runs calling handlers, which are R code, as `&mut Cx` permits.
    unsafe { crate::eval::builtin_failure(values::unbound(), unwind) }
}

/// Makes `target` the value of `*tmp*` in `environment`, where a replacement call finds it,
/// until [`unbind_tmp_value`] takes the entry back. A slot frame without an environment
/// (`None`) binds no `*tmp*`. A pair of calls rather than a guard or a closure: either costs the
/// replacement path drop glue or an outlined body on every run.
#[inline(always)]
pub fn bind_tmp_value<'s>(
    _context: &Cx<'s>,
    environment: Option<Local<'s, Environment>>,
    target: Local<'s>,
) -> TmpValue<'s> {
    let entry = environment.map(|environment| (environment.raw(), target.raw()));
    if let Some(entry) = entry {
        crate::interp().tmp_values.push(entry);
    }
    TmpValue {
        entry,
        scope: PhantomData,
    }
}

/// Ends the `*tmp*` binding [`bind_tmp_value`] made.
#[inline(always)]
pub fn unbind_tmp_value(tmp_value: TmpValue<'_>) {
    if let Some(entry) = tmp_value.entry {
        // `eval::complex_assign` pushes and pops its own entries, which must balance.
        let popped = crate::interp().tmp_values.pop();
        debug_assert_eq!(popped, Some(entry));
    }
}

/// A `*tmp*` binding [`bind_tmp_value`] made, which [`unbind_tmp_value`] must end on every
/// path, an error's included.
#[must_use]
pub struct TmpValue<'s> {
    entry: Option<(Sexp, Sexp)>,
    scope: PhantomData<fn(&'s ()) -> &'s ()>,
}

/// Counts a `*tmp*` binding that the frame of `environment` holds while a replacement call
/// runs, so lookups cached for that frame see it.
#[inline]
pub fn count_tmp_binding(_context: &Cx<'_>, environment: Local<'_, Environment>) {
    // SAFETY: the environment is rooted, and counting writes the frame's own bookkeeping.
    unsafe {
        crate::heap().count_transient_binding(environment.raw(), values::symbols().tmp);
    }
}

/// The direct entry of the builtin `definition` on `arguments`, in formal order, or `None` when
/// it has none or declines.
#[inline(always)]
pub fn call_direct<'s>(
    context: &mut Cx<'s>,
    definition: &crate::builtins::BuiltinDef,
    call: Local<'s>,
    arguments: &[Local<'s>],
) -> R<Option<Local<'s>>> {
    // SAFETY: the call and the arguments are rooted, and the entry runs any builtin code,
    // which `&mut Cx` permits. `Local` is `repr(transparent)` over `Sexp`.
    unsafe {
        let arguments =
            std::slice::from_raw_parts(arguments.as_ptr().cast::<Sexp>(), arguments.len());
        let result = crate::eval::call_direct(definition, call.raw(), arguments)?;
        Ok(result.map(|result| rooted(context, result)))
    }
}

/// The builtin `function` called on `arguments`, tagged as the arguments of `call` are, from
/// the frame of `environment`, or from a slot frame that has none.
#[inline(always)]
pub fn call_builtin<'s>(
    context: &mut Cx<'s>,
    definition: &crate::builtins::BuiltinDef,
    (call, function): (Local<'s>, Local<'s>),
    arguments: &[Local<'s>],
    environment: Option<Local<'s, Environment>>,
) -> R<Local<'s>> {
    let environment = environment.map_or_else(values::nil, Local::raw);
    // SAFETY: the call, the function, the arguments and the environment are rooted, and the
    // builtin runs any code, which `&mut Cx` permits.
    unsafe {
        let result = tagged_as_in(call.raw(), arguments.iter().copied(), |tagged| {
            crate::eval::call_builtin(definition, call.raw(), function.raw(), tagged, environment)
        })?;
        Ok(rooted(context, result))
    }
}

/// A promise of `code` already forced to `value`.
#[inline(always)]
pub fn forced_promise<'s>(
    context: &mut Cx<'s>,
    code: Local<'_>,
    value: Local<'_>,
) -> R<Local<'s, Promise>> {
    // SAFETY: both are rooted, the promise allocates, which `&mut Cx` permits, and a promise
    // is what the allocation makes.
    unsafe {
        let promise = crate::eval::forced_promise(code.raw(), value.raw())?;
        Ok(Local::adopt(rooted(context, promise).raw()))
    }
}

/// An argument with its tag ([`Arg`] on handles): `NULL` when it has none.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Tagged<'s> {
    pub tag: Local<'s>,
    pub value: Local<'s>,
}

// Argument lists are reinterpreted between `Arg` and `Tagged` without copying.
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<Tagged<'static>>() == size_of::<Arg>());
    assert!(offset_of!(Tagged<'static>, tag) == offset_of!(Arg, tag));
    assert!(offset_of!(Tagged<'static>, value) == offset_of!(Arg, value));
};

/// `setter_call` as the call a replacement closure is applied by: `*tmp*` in place of its
/// target and `value_code` in place of its value, with its other arguments and every tag as
/// they are.
#[inline(always)]
pub fn replacement_call<'s>(
    context: &mut Cx<'s>,
    setter_call: Local<'_, Language>,
    value_code: Local<'_>,
) -> R<Local<'s, Language>> {
    // SAFETY: the setter call is rooted and holds its cells, their tags and their code, and
    // `value_code` is rooted. Each new cell holds the cells made before it and is rooted, so
    // nothing built is lost when the next one allocates. A call is what the first cell is.
    unsafe {
        let call = setter_call.raw();
        let tmp = values::symbols().tmp;
        let mut items = [const { std::mem::MaybeUninit::<(Sexp, Sexp)>::uninit() }; SHORT_CALL];
        let mut count = 0;
        let mut cell = call.cdr();
        while cell.kind() == SexpType::Pairlist {
            if count == items.len() {
                return long_replacement_call(context, call, value_code.raw());
            }
            let code = match count {
                0 => tmp,
                _ if cell.cdr().kind() != SexpType::Pairlist => value_code.raw(),
                _ => cell.car(),
            };
            items[count].write((cell.tag(), code));
            count += 1;
            cell = cell.cdr();
        }
        let mut built = values::nil();
        for item in items[..count].iter().rev() {
            let (tag, code) = item.assume_init();
            built = values::triple(SexpType::Pairlist, code, built, tag)?;
        }
        let built = values::triple(SexpType::Language, call.car(), built, values::nil())?;
        Ok(Local::adopt(built))
    }
}

/// [`replacement_call`] for a call of more than [`SHORT_CALL`] arguments.
///
/// # Safety
///
/// As in [`replacement_call`], whose operands these are.
#[cold]
#[inline(never)]
unsafe fn long_replacement_call<'s>(
    _context: &mut Cx<'s>,
    call: Sexp,
    value_code: Sexp,
) -> R<Local<'s, Language>> {
    // SAFETY: as in `replacement_call`.
    unsafe {
        let mut items: Vec<(Sexp, Sexp)> = vec![(values::nil(), call.car())];
        let mut cell = call.cdr();
        while cell.kind() == SexpType::Pairlist {
            let code = match items.len() {
                1 => values::symbols().tmp,
                _ if cell.cdr().kind() != SexpType::Pairlist => value_code,
                _ => cell.car(),
            };
            items.push((cell.tag(), code));
            cell = cell.cdr();
        }
        Ok(Local::adopt(values::pairlist(&items, SexpType::Language)?))
    }
}

/// The closure `function` applied by `call` to `arguments`, each tagged as the argument of
/// `call` in its position is, from the frame of `caller`, or from a slot frame that has none.
/// The closure's frame keeps the arguments (`eval::apply_closure_owning`).
#[inline(always)]
pub fn apply_closure<'s>(
    context: &mut Cx<'s>,
    call: Local<'s, Language>,
    function: Local<'s>,
    arguments: impl ExactSizeIterator<Item = Local<'s>>,
    caller: Option<Local<'s, Environment>>,
) -> R<Local<'s>> {
    let caller = caller.map_or_else(values::nil, Local::raw);
    // SAFETY: the call, the function, the arguments and the caller's environment are rooted,
    // and the closure runs any R code, which `&mut Cx` permits.
    unsafe {
        let result = tagged_as_in(call.raw(), arguments, |tagged| {
            crate::eval::apply_closure_owning(call.raw(), function.raw(), tagged, caller, None)
        })?;
        Ok(rooted(context, result))
    }
}

/// How many arguments a call's tagged argument list holds on the stack.
const SHORT_CALL: usize = 8;

/// `body` on `values`, each tagged as the argument of `call` in its position is. A short list
/// is built on the stack.
///
/// # Safety
///
/// `call` is a live call with at least as many arguments as `values` yields.
#[inline(always)]
unsafe fn tagged_as_in<'s, T>(
    call: Sexp,
    values: impl ExactSizeIterator<Item = Local<'s>>,
    body: impl FnOnce(&[Arg]) -> T,
) -> T {
    // SAFETY: the caller's contract, and the first `count` items of the buffer are written.
    unsafe {
        let mut cell = call.cdr();
        if values.len() <= SHORT_CALL {
            let mut buffer = [const { std::mem::MaybeUninit::<Arg>::uninit() }; SHORT_CALL];
            let mut count = 0;
            for (slot, value) in buffer.iter_mut().zip(values) {
                slot.write(Arg {
                    tag: cell.tag(),
                    value: value.raw(),
                });
                cell = cell.cdr();
                count += 1;
            }
            body(std::slice::from_raw_parts(
                buffer.as_ptr().cast::<Arg>(),
                count,
            ))
        } else {
            let mut tagged = crate::eval::Args::new();
            for value in values {
                tagged.push(Arg {
                    tag: cell.tag(),
                    value: value.raw(),
                });
                cell = cell.cdr();
            }
            body(&tagged)
        }
    }
}

/// An argument that `...` holds: its tag, its value, forced, and the expression that produced
/// it, which is the promise's code.
#[derive(Clone, Copy)]
pub struct DotsArgument<'s> {
    pub tag: Local<'s>,
    pub value: Local<'s>,
    pub expression: Local<'s>,
}

/// The arguments `...` holds in `environment`, each forced in order; none when it is unbound.
pub fn dots_arguments<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s, Environment>,
) -> R<Vec<DotsArgument<'s>>> {
    // SAFETY: the lookup and each force run R code, which `&mut Cx` permits. The rooted list
    // keeps every cell alive, and each part is rooted before the next force.
    unsafe {
        let dots = rooted(
            context,
            crate::eval::find_var(values::symbols().dots, environment.raw())?,
        );
        let mut arguments = Vec::new();
        let mut cell = dots.raw();
        while matches!(cell.kind(), SexpType::Dots | SexpType::Pairlist) {
            let item = cell.car();
            let mut expression = item;
            while expression.kind() == SexpType::Promise {
                expression = expression.cdr();
            }
            let tag = rooted(context, cell.tag());
            let expression = rooted(context, expression);
            let value = rooted(context, crate::eval::value_of(item)?);
            arguments.push(DotsArgument {
                tag,
                value,
                expression,
            });
            cell = cell.cdr();
        }
        Ok(arguments)
    }
}

/// The S3 method `name` that a call from `environment` finds, looking in `definitions` after
/// the calling scope.
pub fn lookup_method<'s>(
    context: &mut Cx<'s>,
    name: Gc<'_, Symbol>,
    environment: Local<'s, Environment>,
    definitions: Local<'s, Environment>,
) -> R<Option<Local<'s>>> {
    // SAFETY: the environments are rooted and the symbol is permanent. The lookup may force a
    // promise or run an active binding, which `&mut Cx` permits.
    unsafe {
        let method =
            crate::dispatch::lookup_method(name.raw(), environment.raw(), definitions.raw())?;
        Ok(method.map(|method| rooted(context, method)))
    }
}

/// `expression` deparsed onto one line.
pub fn deparse_one_line(_context: &mut Cx<'_>, expression: Local<'_>) -> String {
    // SAFETY: the expression is rooted, and deparsing allocates, which `&mut Cx` permits.
    unsafe { crate::deparse::deparse_one_line(expression.raw()) }
}

/// The function `function`, a closure or a primitive, applied to `arguments` from
/// `environment`. A `NULL` call is built from the function and the arguments.
pub fn apply<'s>(
    context: &mut Cx<'s>,
    call: Local<'s>,
    function: Local<'s>,
    arguments: &[Tagged<'s>],
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    // SAFETY: everything is rooted, and the function runs any R code, which `&mut Cx` permits.
    // `Tagged` has `Arg`'s layout.
    unsafe {
        let arguments =
            std::slice::from_raw_parts(arguments.as_ptr().cast::<Arg>(), arguments.len());
        let result = crate::eval::apply(call.raw(), function.raw(), arguments, environment.raw())?;
        Ok(rooted(context, result))
    }
}

/// Runs `body` between reading the random number generator's state from `.Random.seed` and
/// writing it back.
pub fn with_rng_state<'s, T>(context: &mut Cx<'s>, body: impl FnOnce(&mut Cx<'s>) -> R<T>) -> R<T> {
    // SAFETY: reading `.Random.seed` may force a promise and writing it allocates, which
    // `&mut Cx` permits.
    unsafe { crate::random::get_rng_state()? };
    let answer = body(context)?;
    // SAFETY: as above.
    unsafe { crate::random::put_rng_state()? };
    Ok(answer)
}
