//! Inline type annotations at run time. The lowering erases every annotation from the language
//! objects (`body()`, `formals()` and `deparse()` show the plain R program), and a function's
//! parameter and return types become checks at its boundary instead (`decisions.md`, "runtime
//! annotation checks").
//!
//! A check is first-order: it tests the value's storage type against the set of `typeof`s the
//! annotation admits, following the typing reference's promotion order, so `double` admits
//! `logical`, `integer` and `double`. A type the runtime cannot decide from storage alone (an
//! alias, a class, a type parameter, `Any`) is not checked, which keeps every runtime verdict
//! looser than the static checker's. A parameter is checked when its promise is forced, never
//! earlier, so laziness, `missing()` and `substitute()` behave exactly as without the annotation.
//!
//! **Where a signature lives.** It is keyed by the definition's formals pairlist (or its body,
//! for a function without parameters), which every closure made from that definition shares,
//! including copies made by `environment<-`. The signature also records the definition's body,
//! and a closure matches it only when it has that same body too, because code that builds a
//! closure from another function's formals (`rlang::new_function(formals(f), body)`) defines a
//! new, untyped function. The key carries [`gp::TYPED_DEFINITION`], so an
//! untyped call pays one bit test. The registry maps the key to a weak reference whose key is
//! that object and whose value is the signature, so an entry can never outlive its definition
//! and be mistaken for another object at a reused address.

use {
    crate::{
        RError, conditions, interp,
        unwind::R,
        values::{self, nil},
    },
    heap::{
        Marker,
        object::{Sexp, SexpType, gp},
    },
    rustc_hash::FxHashMap,
    syntax::{SyntaxKind, SyntaxNode},
};

/// The runtime's annotation state.
pub struct State {
    /// Whether the lowering turns inline annotations into checks. A host turns it off when the
    /// project turns inline annotations off (`[check] inline-annotations = false`).
    pub enabled: bool,
    /// Definition key to a weak reference holding its signature.
    signatures: FxHashMap<Sexp, Sexp>,
    /// Registry size after the last sweep of dead entries.
    swept_at: usize,
    /// Parameter promises whose value is checked when they are forced, in frame order: a frame
    /// registers its entries after every frame below it, and drops them before they exit.
    pending: Vec<Pending>,
}

impl Default for State {
    fn default() -> State {
        State {
            enabled: true,
            signatures: FxHashMap::default(),
            swept_at: 64,
            pending: Vec::new(),
        }
    }
}

impl State {
    /// Whether no signature is registered and no check is pending.
    pub(crate) fn is_empty(&self) -> bool {
        self.signatures.is_empty() && self.pending.is_empty()
    }

    pub unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &weak in self.signatures.values() {
            marker.mark(weak);
        }
        for pending in &self.pending {
            marker.mark(pending.promise);
            marker.mark(pending.call);
            marker.mark(pending.signature);
        }
    }
}

struct Pending {
    promise: Sexp,
    call: Sexp,
    signature: Sexp,
    slot: usize,
    /// The frame depth of the call that registered it, so the call's exit can drop it.
    depth: usize,
}

/// The `typeof`s a check admits, as bits indexed by `SexpType` code. Zero means unchecked.
type Mask = u32;

const fn bit(kind: SexpType) -> Mask {
    1 << kind as u32
}

const LOGICAL: Mask = bit(SexpType::Logical);
const INTEGER: Mask = LOGICAL | bit(SexpType::Integer);
const DOUBLE: Mask = INTEGER | bit(SexpType::Real);
const COMPLEX: Mask = DOUBLE | bit(SexpType::Complex);
const CHARACTER: Mask = bit(SexpType::String);
const RAW: Mask = bit(SexpType::Raw);
const NULL: Mask = bit(SexpType::Nil);
const LIST: Mask = bit(SexpType::List);
const NATIVE: Mask = bit(SexpType::Record);
/// ry's `I64`. A whole R number converts to one at the boundary (see [`check_value`]).
const I64: Mask = bit(SexpType::I64);
/// ry's `F64`, which is an R double without attributes.
const F64: Mask = bit(SexpType::Real);
const FUNCTION: Mask = bit(SexpType::Closure) | bit(SexpType::Builtin) | bit(SexpType::Special);
/// `Array<T>`: the value must carry the Array bit, and its storage must be one the item type
/// admits (any storage when the item type is not decidable from storage).
const ARRAY: Mask = 1 << 31;
/// The annotation is scalar-like (`double`, not `double[]`). No check tests a value's length,
/// because the typing reference lets a value typed scalar hold several elements, so the bit only
/// tells a compiler what to expect before it has seen a value.
const SCALAR: Mask = 1 << 30;

/// A signature is a list: the masks by slot, the annotation texts by slot, the formal names by
/// formal position, and the definition's body.
const MASKS: usize = 0;
const TEXTS: usize = 1;
const NAMES: usize = 2;
const BODY: usize = 3;

/// Slot 0 of a signature is the return value, slot 1 the rest parameter, and slot `2 + i` the
/// formal at position `i`.
const RETURN_SLOT: usize = 0;
const REST_SLOT: usize = 1;
const FIRST_FORMAL_SLOT: usize = 2;

/// Records the checks a function literal's inline annotations ask for, keyed by the formals or
/// body the lowering just built for it. Does nothing when no annotation is checkable.
pub unsafe fn register_definition(node: &SyntaxNode, formals: Sexp, body: Sexp) -> R<()> {
    let state = &mut interp().annotations;
    if !state.enabled {
        return Ok(());
    }
    let mut slots: Vec<(Mask, String)> = vec![(0, String::new()); FIRST_FORMAL_SLOT];
    for child in node.children() {
        match child.kind() {
            SyntaxKind::PARAMETER_LIST => {
                for parameter in child
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::PARAMETER)
                {
                    let check = parameter
                        .children()
                        .find(|c| c.kind() == SyntaxKind::TYPE_ASCRIPTION)
                        .and_then(|ascription| type_child(&ascription))
                        .map(|ty| (mask_of(&ty), ty.text().to_string()))
                        .unwrap_or((0, String::new()));
                    let is_rest = parameter
                        .children_with_tokens()
                        .any(|element| element.kind() == SyntaxKind::DOTS);
                    // `...` is matched as a whole, so its type lives in the rest slot.
                    if is_rest {
                        slots[REST_SLOT] = check.clone();
                    }
                    slots.push(check);
                }
            }
            SyntaxKind::RETURN_TYPE => {
                if let Some(ty) = type_child(&child) {
                    slots[RETURN_SLOT] = (mask_of(&ty), ty.text().to_string());
                }
            }
            _ => {}
        }
    }
    if slots.iter().all(|(mask, _)| *mask == 0) {
        return Ok(());
    }
    let key = if formals.kind() == SexpType::Pairlist {
        formals
    } else if body.kind() == SexpType::Language {
        body
    } else {
        // A body that is a symbol or `NULL` is shared with other definitions, so it cannot key
        // one. Only a parameterless function with such a body lands here.
        return Ok(());
    };
    let masks: Vec<i32> = slots.iter().map(|(mask, _)| *mask as i32).collect();
    let texts: Vec<&str> = slots.iter().map(|(_, text)| text.as_str()).collect();
    let mut names = Vec::new();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        names.push(values::symbol_name(formal.tag()).into_owned());
        formal = formal.cdr();
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let masks = values::integer_vector(&masks)?;
    let texts = values::string_vector(&texts)?;
    let names = values::string_vector(&names)?;
    let signature = values::list_vector(&[masks, texts, names, body])?;
    let heap = crate::heap();
    let weak = heap.alloc_weak_ref(key, signature, nil(), 0, false)?;
    key.set_gp_bits(gp::TYPED_DEFINITION, true);
    let state = &mut interp().annotations;
    state.signatures.insert(key, weak);
    if state.signatures.len() > 2 * state.swept_at {
        state.signatures.retain(|&key, weak| weak.weak_key() == key);
        state.swept_at = state.signatures.len().max(64);
    }
    Ok(())
}

/// Checks a typed closure's arguments on entry: a value now, a promise when it is forced.
/// Returns the signature, which [`check_return`] and [`leave`] take back, or `None` for an
/// untyped closure.
pub unsafe fn enter(call: Sexp, function: Sexp, env: Sexp, depth: usize) -> R<Option<Sexp>> {
    let Some(signature) = signature_of(function) else {
        return Ok(None);
    };
    let masks = values::ints(signature.element(MASKS));
    let Some(parameters) = masks.get(FIRST_FORMAL_SLOT..) else {
        return Ok(Some(signature));
    };
    if parameters.iter().all(|&mask| mask == 0) {
        return Ok(Some(signature));
    }
    let heap = crate::heap();
    let mut formal = function.car();
    let mut slot = FIRST_FORMAL_SLOT;
    while formal.kind() == SexpType::Pairlist {
        let mask = masks.get(slot).map_or(0, |&mask| mask as Mask);
        if mask != 0 {
            if formal.tag() == values::symbols().dots {
                if let Some(cell) = heap.frame_cell(env, formal.tag()) {
                    let mut dots = cell.cdr();
                    while dots.kind() == SexpType::Dots {
                        check_argument(call, signature, REST_SLOT, Holder::Dots(dots), dots.car(), depth)?;
                        dots = dots.cdr();
                    }
                }
            } else if let Some(cell) = heap.frame_cell(env, formal.tag()) {
                let mut value = cell.cdr();
                // A literal default is bound as a value rather than a promise, but it is checked
                // only when the body uses it, so it gets the promise it would otherwise skip.
                if cell.has_gp(gp::BINDING_MISSING) && value.kind() != SexpType::Promise {
                    value = crate::eval::make_promise(value, env)?;
                    heap.set_binding_value(cell, value)?;
                }
                check_argument(call, signature, slot, Holder::Binding(cell), value, depth)?;
            }
        }
        formal = formal.cdr();
        slot += 1;
    }
    Ok(Some(signature))
}

/// How a closure call's arguments meet the closure's annotations.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    /// [`enter`] checks the arguments bound in the new frame.
    Unchecked,
    /// The caller already checked them; holds the signature, `None` for an untyped closure.
    Checked(Option<Sexp>),
}

/// Decides ahead of time what a call binding arguments of storage types `kinds` positionally to
/// a closure's leading formals needs checked, for a compiler that knows those types before the
/// call runs (`None` is a type it does not know). [`Entry::Unchecked`] means the checks must
/// run with the values.
pub unsafe fn entry_for(function: Sexp, kinds: &[Option<SexpType>]) -> Entry {
    let Some(signature) = signature_of(function) else {
        return Entry::Checked(None);
    };
    let masks = signature.element(MASKS);
    let mut formal = function.car();
    let mut slot = FIRST_FORMAL_SLOT;
    while formal.kind() == SexpType::Pairlist {
        let mask = mask_at(masks, slot);
        if mask != 0 {
            match kinds.get(slot - FIRST_FORMAL_SLOT) {
                Some(&Some(kind))
                    if formal.tag() != values::symbols().dots && plain_admits(mask, kind) => {}
                _ => return Entry::Unchecked,
            }
        }
        formal = formal.cdr();
        slot += 1;
    }
    Entry::Checked(Some(signature))
}

/// Checks the plain values a call binds to a closure's leading formals in order, as a direct
/// call from compiled code does, straight from the values instead of reading them back from the
/// frame. A typed formal left to its default, or a typed `...`, may hold a promise only [`enter`]
/// can register, so such a call stays [`Entry::Unchecked`].
pub unsafe fn enter_positional(call: Sexp, function: Sexp, values: &[Sexp]) -> R<Entry> {
    let Some(signature) = signature_of(function) else {
        return Ok(Entry::Checked(None));
    };
    let masks = signature.element(MASKS);
    let mut formal = function.car();
    let mut slot = FIRST_FORMAL_SLOT;
    while formal.kind() == SexpType::Pairlist {
        let index = slot - FIRST_FORMAL_SLOT;
        if mask_at(masks, slot) != 0 {
            match values.get(index) {
                Some(&value) if formal.tag() != values::symbols().dots => {
                    // A value the check converts must replace the one the caller binds, which
                    // only `enter` can do.
                    if value != values::missing_arg()
                        && check_value(call, signature, slot, value)? != value
                    {
                        return Ok(Entry::Unchecked);
                    }
                }
                _ => return Ok(Entry::Unchecked),
            }
        }
        formal = formal.cdr();
        slot += 1;
    }
    Ok(Entry::Checked(Some(signature)))
}

/// Checks a typed closure's value, and returns the value the call returns (see [`check_value`]).
pub unsafe fn check_return(call: Sexp, signature: Sexp, value: Sexp) -> R<Sexp> {
    let mask = mask_at(signature.element(MASKS), RETURN_SLOT);
    if mask == 0 || admits(mask, value) {
        return Ok(value);
    }
    // `-> I64` converts a whole R number, as an `I64` parameter does.
    if let Some(converted) = as_i64(mask, value)? {
        return Ok(converted);
    }
    Err(failure(
        call,
        format!(
            "the function must return `{}`, not `{}`",
            text_at(signature, RETURN_SLOT),
            describe(value)
        ),
    ))
}

/// Drops the checks of the frame at `depth` whose promises were never forced, so a promise that
/// escapes its call is no longer checked. Every closure exit calls it, because a frame whose
/// entry check failed may already have registered some.
pub unsafe fn leave(depth: usize) {
    let pending = &mut interp().annotations.pending;
    while let Some(entry) = pending.last()
        && entry.depth >= depth
    {
        let promise = entry.promise;
        pending.pop();
        // A promise forwarded through `...` can be pending in an outer typed frame too.
        if !pending.iter().any(|entry| entry.promise == promise) {
            promise.set_gp_bits(gp::CHECKED_PROMISE, false);
        }
    }
}

/// Checks the value a promise is about to hold, against every typed frame it was passed to.
/// Called only for a promise carrying [`gp::CHECKED_PROMISE`], before the value is stored: a
/// failed check leaves the promise unforced and still checked, as an error while evaluating it
/// would, so code that catches the error can never read the value unchecked. Returns the value
/// to store, which an `I64` annotation may have converted.
pub unsafe fn check_forced(promise: Sexp, mut value: Sexp) -> R<Sexp> {
    let pending = &interp().annotations.pending;
    // The innermost frame is the most specific, so its failure is the one reported.
    for entry in pending
        .iter()
        .rev()
        .filter(|entry| entry.promise == promise)
    {
        value = check_value(entry.call, entry.signature, entry.slot, value)?;
    }
    promise.set_gp_bits(gp::CHECKED_PROMISE, false);
    interp()
        .annotations
        .pending
        .retain(|entry| entry.promise != promise);
    Ok(value)
}

/// What an annotation that admits exactly one of `logical`, `integer` or `double` declares
/// (each of these also admits the narrower ones, as R's coercions do), for a compiler that
/// takes it as the type to expect before it has seen a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declared {
    pub kind: SexpType,
    /// Scalar-like, such as `double` rather than `double[]`.
    pub scalar: bool,
}

/// The type a typed closure declares it returns.
pub unsafe fn declared_return_type(function: Sexp) -> Option<Declared> {
    let signature = signature_of(function)?;
    declared(mask_at(signature.element(MASKS), RETURN_SLOT))
}

/// Whether the formal at `position` of a typed closure declares `I64`, so a whole R number
/// passed there converts to one (see [`check_value`]).
pub unsafe fn declares_i64(function: Sexp, position: usize) -> bool {
    signature_of(function).is_some_and(|signature| {
        mask_at(signature.element(MASKS), FIRST_FORMAL_SLOT + position) == I64 | SCALAR
    })
}

/// Which formals of the definition `formals`/`body` declare `I64`, by position, and whether it
/// declares that it returns `I64`, for a compiler that inlines the definition and converts at
/// its boundary as a call would.
pub unsafe fn declared_i64(formals: Sexp, body: Sexp) -> (Vec<bool>, bool) {
    let Some(signature) = signature_for(formals, body) else {
        return (Vec::new(), false);
    };
    let masks = values::ints(signature.element(MASKS));
    let is_i64 = |mask: &i32| *mask as Mask == I64 | SCALAR;
    let formals = masks
        .get(FIRST_FORMAL_SLOT..)
        .unwrap_or(&[])
        .iter()
        .map(is_i64)
        .collect();
    (formals, masks.get(RETURN_SLOT).is_some_and(is_i64))
}

/// Whether a typed closure declares that it returns `I64`.
pub unsafe fn returns_i64(function: Sexp) -> bool {
    signature_of(function)
        .is_some_and(|signature| mask_at(signature.element(MASKS), RETURN_SLOT) == I64 | SCALAR)
}

/// Whether the formal at `position` of a typed closure declares a vector, such as `double[]`,
/// rather than a scalar, so that a compiler gives a value of length one a vector's type there.
pub unsafe fn declares_vector(function: Sexp, position: usize) -> bool {
    signature_of(function).is_some_and(|signature| {
        let mask = mask_at(signature.element(MASKS), FIRST_FORMAL_SLOT + position);
        mask != 0 && mask & (SCALAR | ARRAY) == 0
    })
}

/// The declared type of each formal of the definition `formals`/`body`, by position, and
/// whether every formal carries a checked annotation. An untyped definition declares nothing.
pub unsafe fn declared_formals(formals: Sexp, body: Sexp) -> (Vec<Option<Declared>>, bool) {
    let Some(signature) = signature_for(formals, body) else {
        return (Vec::new(), false);
    };
    let masks = values::ints(signature.element(MASKS));
    let formal_masks = masks.get(FIRST_FORMAL_SLOT..).unwrap_or(&[]);
    let types = formal_masks
        .iter()
        .map(|&mask| declared(mask as Mask))
        .collect();
    (types, formal_masks.iter().all(|&mask| mask != 0))
}

fn declared(mask: Mask) -> Option<Declared> {
    let kind = match mask & !SCALAR {
        LOGICAL => SexpType::Logical,
        INTEGER => SexpType::Integer,
        DOUBLE => SexpType::Real,
        _ => return None,
    };
    Some(Declared {
        kind,
        scalar: mask & SCALAR != 0,
    })
}

/// Whether a call that binds plain values of storage types `arguments` to the formals of the
/// definition `formals`/`body`, one each in order, and returns a value of storage type `result`
/// passes all of that definition's checks. A compiler that inlines the call decides this before
/// the call runs, and leaves a call that would fail to the interpreter, which reports it.
/// `None` is a type the compiler does not know.
pub unsafe fn admits_inline(
    formals: Sexp,
    body: Sexp,
    arguments: &[Option<SexpType>],
    result: Option<SexpType>,
) -> bool {
    let Some(signature) = signature_for(formals, body) else {
        return true;
    };
    let masks = values::ints(signature.element(MASKS));
    let admitted = |mask: i32, kind: Option<SexpType>| {
        mask == 0 || kind.is_some_and(|kind| plain_admits(mask as Mask, kind))
    };
    let formal_masks = masks.get(FIRST_FORMAL_SLOT..).unwrap_or(&[]);
    formal_masks.len() == arguments.len()
        && formal_masks
            .iter()
            .zip(arguments)
            .all(|(&mask, &kind)| admitted(mask, kind))
        && admitted(masks.get(RETURN_SLOT).copied().unwrap_or(0), result)
}

/// Whether a closure's return annotation admits a plain value of storage type `kind`, which a
/// compiler that proves the result's type checks before the call runs.
pub unsafe fn return_admits(function: Sexp, kind: SexpType) -> bool {
    let Some(signature) = signature_of(function) else {
        return true;
    };
    let mask = mask_at(signature.element(MASKS), RETURN_SLOT);
    mask == 0 || plain_admits(mask, kind)
}

/// Whether a closure's annotations admit a call binding values of these storage types to its
/// formals, one each in order, and returning a value of storage type `result`. `true` beside a
/// type marks an `Array`, and `None` is a type with no R storage, which only an unchecked
/// annotation admits. A compiler that knows every type before the call runs decides the checks
/// here once, exactly as the checks would decide them for the values.
pub unsafe fn admits_storage(
    function: Sexp,
    arguments: &[Option<(SexpType, bool)>],
    result: Option<(SexpType, bool)>,
) -> bool {
    let Some(signature) = signature_of(function) else {
        return true;
    };
    let masks = signature.element(MASKS);
    let admitted = |mask: Mask, storage: Option<(SexpType, bool)>| {
        mask == 0 || storage.is_some_and(|(kind, array)| storage_admits(mask, kind, array))
    };
    let mut formal = function.car();
    let mut slot = FIRST_FORMAL_SLOT;
    while formal.kind() == SexpType::Pairlist {
        let mask = mask_at(masks, slot);
        if mask != 0 {
            match arguments.get(slot - FIRST_FORMAL_SLOT) {
                Some(&storage)
                    if formal.tag() != values::symbols().dots && admitted(mask, storage) => {}
                _ => return false,
            }
        }
        formal = formal.cdr();
        slot += 1;
    }
    admitted(mask_at(masks, RETURN_SLOT), result)
}

/// [`admits`] for a value known only by its storage type and whether it is an `Array`.
fn storage_admits(mask: Mask, kind: SexpType, array: bool) -> bool {
    if mask & ARRAY != 0 {
        let items = mask & !ARRAY;
        return array && (items == 0 || items & bit(kind) != 0);
    }
    mask & bit(kind) != 0
}

/// Whether a check admits a value of storage type `kind` that is not an `Array`.
fn plain_admits(mask: Mask, kind: SexpType) -> bool {
    mask & ARRAY == 0 && mask & bit(kind) != 0
}

/// Whether `function`'s definition may carry a signature: one bit test, which [`declared_formals`]
/// and the checks confirm.
pub unsafe fn is_typed(function: Sexp) -> bool {
    let formals = function.car();
    let key = if formals.kind() == SexpType::Pairlist {
        formals
    } else {
        crate::bytecode::closure_body(function)
    };
    key.has_gp(gp::TYPED_DEFINITION)
}

unsafe fn signature_of(function: Sexp) -> Option<Sexp> {
    signature_for(function.car(), crate::bytecode::closure_body(function))
}

/// The signature of the definition with these formals and this body (as source).
unsafe fn signature_for(formals: Sexp, body: Sexp) -> Option<Sexp> {
    let key = if formals.kind() == SexpType::Pairlist {
        formals
    } else {
        body
    };
    if !key.has_gp(gp::TYPED_DEFINITION) {
        return None;
    }
    let weak = *interp().annotations.signatures.get(&key)?;
    if weak.weak_key() != key {
        return None;
    }
    let signature = weak.weak_value();
    (signature.element(BODY) == body).then_some(signature)
}

/// Where an argument's value is bound, so a value the check converts can replace it.
#[derive(Clone, Copy)]
enum Holder {
    /// A frame binding.
    Binding(Sexp),
    /// A `...` cell, which holds the element in its car.
    Dots(Sexp),
}

/// Checks the argument `value` that `holder` binds. A value the check converts replaces the
/// binding, or the forced promise's value.
unsafe fn check_argument(
    call: Sexp,
    signature: Sexp,
    slot: usize,
    holder: Holder,
    value: Sexp,
    depth: usize,
) -> R<()> {
    if value.kind() != SexpType::Promise {
        if value == values::missing_arg() {
            return Ok(());
        }
        let checked = check_value(call, signature, slot, value)?;
        if checked != value {
            match holder {
                Holder::Binding(cell) => crate::heap().set_binding_value(cell, checked)?,
                Holder::Dots(cell) => crate::heap().set_car(cell, checked),
            }
        }
        return Ok(());
    }
    if value.car() != values::unbound() {
        let checked = check_value(call, signature, slot, value.car())?;
        if checked != value.car() {
            crate::heap().set_car(value, checked);
        }
        return Ok(());
    }
    value.set_gp_bits(gp::CHECKED_PROMISE, true);
    interp().annotations.pending.push(Pending {
        promise: value,
        call,
        signature,
        slot,
        depth,
    });
    Ok(())
}

/// Checks `value` against the annotation in `slot`, and returns the value to bind: `value`
/// itself, or the `I64` an `I64` annotation converts a whole R number to.
unsafe fn check_value(call: Sexp, signature: Sexp, slot: usize, value: Sexp) -> R<Sexp> {
    let mask = mask_at(signature.element(MASKS), slot);
    if mask == 0 || admits(mask, value) {
        return Ok(value);
    }
    // R has no `I64` literal, so `f(10)` for `n: I64` passes the R double 10, which the checker
    // types as `I64`. The boundary converts every R double or integer that is exactly a whole
    // number, as the `I64` library functions do, so the body always sees an `I64`.
    if let Some(converted) = as_i64(mask, value)? {
        return Ok(converted);
    }
    // The checker accepts a whole-number `double` literal at an `integer` parameter (`f(10)` for
    // `n: integer`). Storage cannot tell a literal from a computed value, so every whole-number
    // `double` scalar passes, which keeps the check at least as loose as the static type. The
    // compilers decide from the argument's actual storage, so none of them assumes `integer` here.
    if slot != RETURN_SLOT
        && mask & ARRAY == 0
        && mask & bit(SexpType::Integer) != 0
        && is_whole_double_scalar(value)
    {
        return Ok(value);
    }
    let expected = text_at(signature, slot);
    let found = describe(value);
    let message = if slot == REST_SLOT {
        format!("every `...` argument must be `{expected}`, but one is `{found}`")
    } else {
        let name = values::string_at(signature.element(NAMES), slot - FIRST_FORMAL_SLOT)
            .map(|name| name.into_owned())
            .unwrap_or_default();
        format!("argument `{name}` must be `{expected}`, not `{found}`")
    };
    Err(failure(call, message))
}

/// `value` converted for an `I64` or `List<I64>` annotation, or `None` when the annotation is
/// neither or a number in `value` is not exactly whole. An unsuffixed list literal is an R double
/// Array that the checker types `List<I64>`, so the list converts item by item, as an annotated
/// binding does.
unsafe fn as_i64(mask: Mask, value: Sexp) -> R<Option<Sexp>> {
    if mask & I64 == 0 {
        return Ok(None);
    }
    if mask & ARRAY == 0 {
        return match crate::numbers::r_source(value).ok().and_then(crate::numbers::convert) {
            Some(number) => crate::numbers::alloc(number).map(Some),
            None => Ok(None),
        };
    }
    if !value.is_array() || value.attrib() != nil() {
        return Ok(None);
    }
    let items: Option<Vec<i64>> = match value.kind() {
        SexpType::Real => values::reals(value)
            .iter()
            .map(|&number| {
                (!heap::object::is_na_real(number))
                    .then_some(crate::numbers::Source::F64(number))
                    .and_then(crate::numbers::convert)
            })
            .collect(),
        SexpType::Integer => values::ints(value)
            .iter()
            .map(|&number| (number != heap::object::NA_INTEGER).then_some(i64::from(number)))
            .collect(),
        _ => None,
    };
    match items {
        Some(items) => crate::numbers::list(&items).map(Some),
        None => Ok(None),
    }
}

unsafe fn failure(call: Sexp, message: String) -> crate::unwind::Unwind {
    conditions::signal_error(RError { message, call })
}

unsafe fn mask_at(masks: Sexp, slot: usize) -> Mask {
    values::ints(masks)
        .get(slot)
        .map_or(0, |&mask| mask as Mask)
}

unsafe fn text_at(signature: Sexp, slot: usize) -> String {
    values::string_at(signature.element(TEXTS), slot)
        .map(|text| text.into_owned())
        .unwrap_or_default()
}

unsafe fn admits(mask: Mask, value: Sexp) -> bool {
    if mask & ARRAY != 0 {
        let items = mask & !ARRAY;
        // An empty list has no item to decide its storage by, and is stored as `logical`.
        return value.is_array()
            && (items == 0 || value.length() == 0 || items & bit(value.kind()) != 0);
    }
    mask & bit(value.kind()) != 0
}

unsafe fn is_whole_double_scalar(value: Sexp) -> bool {
    value.kind() == SexpType::Real
        && !value.is_array()
        && value.attrib() == nil()
        && matches!(values::reals(value), [number] if number.is_finite() && number.fract() == 0.0)
}

/// A value's storage type as a reader of the annotation thinks of it.
unsafe fn describe(value: Sexp) -> String {
    match value.kind() {
        SexpType::List if value.is_array() => "Array".to_owned(),
        kind if value.is_array() => format!("Array<{}>", kind.name()),
        SexpType::Closure | SexpType::Builtin | SexpType::Special => "function".to_owned(),
        kind => kind.name().to_owned(),
    }
}

/// The type node an ascription or a return type annotation holds.
fn type_child(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children().next()
}

/// The storage types a type admits, or zero when storage alone cannot decide it.
fn mask_of(node: &SyntaxNode) -> Mask {
    match node.kind() {
        SyntaxKind::TYPE_REF => match node.first_token().as_ref().map(|token| token.text()) {
            Some("logical") => LOGICAL | SCALAR,
            Some("integer") => INTEGER | SCALAR,
            Some("double") => DOUBLE | SCALAR,
            Some("complex") => COMPLEX | SCALAR,
            Some("character") => CHARACTER | SCALAR,
            Some("raw") => RAW | SCALAR,
            Some("NULL") | Some("null") => NULL,
            Some("I64") => I64 | SCALAR,
            Some("F64") => F64 | SCALAR,
            _ => 0,
        },
        // A vector shape (`integer[]`, `integer[named]`) is not checked, only its storage.
        SyntaxKind::TYPE_VECTOR => node
            .children()
            .next()
            .map_or(0, |inner| mask_of(&inner) & !SCALAR),
        SyntaxKind::TYPE_PAREN => node.children().next().map_or(0, |inner| mask_of(&inner)),
        SyntaxKind::TYPE_UNION => {
            let mut mask = 0;
            let mut scalar = SCALAR;
            for member in node.children() {
                let member = mask_of(&member);
                // An Array member's storage overlaps a vector's, so a mixed union stays unchecked.
                if member == 0 || member & ARRAY != 0 {
                    return 0;
                }
                scalar &= member;
                mask |= member & !SCALAR;
            }
            mask | scalar
        }
        // A DataFrame is an external pointer to a table, so its columns are not in its storage,
        // and a plan or a grouping is no list either.
        SyntaxKind::TYPE_RECORD | SyntaxKind::TYPE_TUPLE
            if node.first_token().is_some_and(|token| {
                matches!(
                    token.text(),
                    "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy"
                )
            }) =>
        {
            0
        }
        // A ry record or tuple is its own storage kind, not an R list.
        SyntaxKind::TYPE_RECORD | SyntaxKind::TYPE_TUPLE
            if node
                .first_token()
                .is_some_and(|token| matches!(token.text(), "Record" | "Tuple")) =>
        {
            NATIVE
        }
        SyntaxKind::TYPE_RECORD | SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_LIST => LIST,
        SyntaxKind::TYPE_FUNCTION => FUNCTION,
        SyntaxKind::TYPE_APPLY
            if matches!(applied_name(node).as_deref(), Some("List" | "Array")) =>
        {
            let item = node
                .children()
                .find(|child| child.kind() == SyntaxKind::TYPE_ARG_LIST)
                .and_then(|arguments| arguments.children().next())
                .map_or(0, |item| mask_of(&item));
            // Records, tags, functions and nested lists are items of an R list.
            ARRAY
                | if item & (ARRAY | NATIVE | LIST | FUNCTION) != 0 {
                    LIST
                } else {
                    item & !SCALAR
                }
        }
        _ => 0,
    }
}

/// The name a generic type applies, such as `Array` in `Array<double>`.
fn applied_name(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::NAME)
        .map(|name| name.text().to_string().trim().to_owned())
}
