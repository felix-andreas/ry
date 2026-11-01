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
const FUNCTION: Mask = bit(SexpType::Closure) | bit(SexpType::Builtin) | bit(SexpType::Special);
/// `Array<T>`: the value must carry the Array bit, and its storage must be one the item type
/// admits (any storage when the item type is not decidable from storage).
const ARRAY: Mask = 1 << 31;

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
                        check_argument(call, signature, REST_SLOT, dots.car(), depth)?;
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
                check_argument(call, signature, slot, value, depth)?;
            }
        }
        formal = formal.cdr();
        slot += 1;
    }
    Ok(Some(signature))
}

/// Checks a typed closure's value.
pub unsafe fn check_return(call: Sexp, signature: Sexp, value: Sexp) -> R<()> {
    let mask = mask_at(signature.element(MASKS), RETURN_SLOT);
    if mask == 0 || admits(mask, value) {
        return Ok(());
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
/// would, so code that catches the error can never read the value unchecked.
pub unsafe fn check_forced(promise: Sexp, value: Sexp) -> R<()> {
    let pending = &interp().annotations.pending;
    // The innermost frame is the most specific, so its failure is the one reported.
    for entry in pending
        .iter()
        .rev()
        .filter(|entry| entry.promise == promise)
    {
        check_value(entry.call, entry.signature, entry.slot, value)?;
    }
    promise.set_gp_bits(gp::CHECKED_PROMISE, false);
    interp()
        .annotations
        .pending
        .retain(|entry| entry.promise != promise);
    Ok(())
}

unsafe fn signature_of(function: Sexp) -> Option<Sexp> {
    let formals = function.car();
    let key = if formals.kind() == SexpType::Pairlist {
        formals
    } else {
        crate::bytecode::closure_body(function)
    };
    if !key.has_gp(gp::TYPED_DEFINITION) {
        return None;
    }
    let weak = *interp().annotations.signatures.get(&key)?;
    if weak.weak_key() != key {
        return None;
    }
    let signature = weak.weak_value();
    (signature.element(BODY) == crate::bytecode::closure_body(function)).then_some(signature)
}

unsafe fn check_argument(
    call: Sexp,
    signature: Sexp,
    slot: usize,
    value: Sexp,
    depth: usize,
) -> R<()> {
    if value.kind() != SexpType::Promise {
        if value == values::missing_arg() {
            return Ok(());
        }
        return check_value(call, signature, slot, value);
    }
    if value.car() != values::unbound() {
        return check_value(call, signature, slot, value.car());
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

unsafe fn check_value(call: Sexp, signature: Sexp, slot: usize, value: Sexp) -> R<()> {
    let mask = mask_at(signature.element(MASKS), slot);
    if mask == 0 || admits(mask, value) {
        return Ok(());
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
        return value.is_array() && (items == 0 || items & bit(value.kind()) != 0);
    }
    mask & bit(value.kind()) != 0
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
            Some("logical") => LOGICAL,
            Some("integer") => INTEGER,
            Some("double") => DOUBLE,
            Some("complex") => COMPLEX,
            Some("character") => CHARACTER,
            Some("raw") => RAW,
            Some("NULL") | Some("null") => NULL,
            _ => 0,
        },
        // A vector shape (`integer[]`, `integer[named]`) is not checked, only its storage.
        SyntaxKind::TYPE_VECTOR | SyntaxKind::TYPE_PAREN => {
            node.children().next().map_or(0, |inner| mask_of(&inner))
        }
        SyntaxKind::TYPE_UNION => {
            let mut mask = 0;
            for member in node.children() {
                let member = mask_of(&member);
                // An Array member's storage overlaps a vector's, so a mixed union stays unchecked.
                if member == 0 || member & ARRAY != 0 {
                    return 0;
                }
                mask |= member;
            }
            mask
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
        SyntaxKind::TYPE_RECORD | SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_LIST => LIST,
        SyntaxKind::TYPE_FUNCTION => FUNCTION,
        SyntaxKind::TYPE_APPLY if applied_name(node).as_deref() == Some("Array") => {
            let item = node
                .children()
                .find(|child| child.kind() == SyntaxKind::TYPE_ARG_LIST)
                .and_then(|arguments| arguments.children().next())
                .map_or(0, |item| mask_of(&item));
            ARRAY | if item & ARRAY != 0 { LIST } else { item }
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
