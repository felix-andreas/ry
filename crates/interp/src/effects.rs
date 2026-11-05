//! The effect-free fact: whether calling a closure can change a binding outside its own frame
//! or read its frame through anything but its own code.
//!
//! A closure is effect-free when its defaults and body assign only to its own locals and call
//! only itself, base builtins from [`EFFECT_FREE_BUILTINS`] and other effect-free closures. Such
//! a call can still signal a condition, and a builtin can still dispatch an S3 method on a
//! classed argument; those are the accepted holes. The fact has two halves:
//!
//! - The syntactic half is stored with the closure's compiled body (the `SUMMARY` constant, see
//!   [`summarize`]): `NULL` when the body assigns outside its frame or calls something that is
//!   not a symbol, otherwise the cache slot the resolved half lives in and the symbols the body
//!   calls.
//! - The resolved half says what those symbols name from the closure's enclosure. Code objects
//!   are shared by every closure a `function` expression creates, so it is kept per enclosure in
//!   one of the code's inline caches, with the binding epoch it was proven at and the lookup
//!   stamp of every symbol it depends on, its callees' included (see [`effect_free`]).
//!
//! Tier 0 reads the fact to bind plain values instead of promises (promise elision), and the
//! optimizing tier reads it, with the bindings it depends on, to call a closure directly.

use {
    crate::{
        bytecode::{self, CacheSlot, SUMMARY},
        eval, interp,
        unwind::R,
        values::{self, missing_arg, nil, symbol_name, unbound},
    },
    heap::object::{Sexp, SexpType, gp},
    rustc_hash::FxHashSet,
    smallvec::SmallVec,
};

/// A function lookup that compiled code, or the effect-free fact, assumes still resolves as it
/// did when it was made.
#[derive(Clone, Debug)]
pub struct Speculation {
    pub symbol: Sexp,
    /// The environment the lookup starts from, which differs from the compiled body's enclosure
    /// for a function an inlined or directly called closure looks up from its own environment.
    pub env: Sexp,
    /// The binding cell the lookup found.
    pub cell: Sexp,
    pub function: Sexp,
}

/// Base builtins that never run R code of the caller's choosing nor touch any environment but
/// their own arguments, apart from S3 dispatch on a classed argument.
pub const EFFECT_FREE_BUILTINS: &[&str] = &[
    "{", "(", "if", "for", "while", "repeat", "break", "next", "return", "invisible", "+", "-",
    "*", "/", "^", "%%", "%/%", "==", "!=", "<", ">", "<=", ">=", "!", "&", "|", "&&", "||",
    "sqrt", "exp", "log", "abs", "floor", "ceiling", "trunc", "round", "signif", "sin", "cos",
    "tan", "log2", "log10", "log1p", "expm1", "length", "[", "[[", "c", "is.na", "is.null", "sum",
    "prod", "max", "min", "cumsum", "cumprod", "seq_len", "seq_along", ":", "vector", "numeric",
    "integer", "double", "logical", "character", "rep", "rep.int", "rep_len", "nchar", "any",
    "all", "which", "rev", "as.integer", "as.numeric", "as.double", "as.logical", "is.numeric",
    "is.character", "is.logical", "identical", "range", "pmin", "pmax",
];

/// The deepest chain of effect-free closures calling each other that the resolved half follows.
const MAX_DEPTH: usize = 8;

/// The syntactic half of the fact for a closure with `formals` and `body`, whose frame binds
/// exactly `locals` (`None` when the body reflects on its frame), stored in the cache slot
/// `slot`: `NULL` when no resolution of the names it calls can make it effect-free.
pub(crate) unsafe fn summarize(
    formals: Sexp,
    body: Sexp,
    locals: Option<&FxHashSet<Sexp>>,
    slot: u32,
) -> R<Sexp> {
    let Some(locals) = locals else {
        return Ok(nil());
    };
    let mut calls: SmallVec<[Sexp; 16]> = SmallVec::new();
    let defaults = values::pairlist_items(formals);
    let syntactic = defaults
        .iter()
        .all(|&(_, default)| collect_calls(default, locals, &mut calls))
        && collect_calls(body, locals, &mut calls);
    if !syntactic {
        return Ok(nil());
    }
    let summary = values::alloc(SexpType::List, 1 + calls.len())?;
    crate::heap().root(summary);
    values::set_elt(summary, 0, values::scalar_integer(slot as i32)?);
    for (index, symbol) in calls.iter().enumerate() {
        values::set_elt(summary, 1 + index, *symbol);
    }
    Ok(summary)
}

/// Whether calling the closure `function` is effect-free, from its enclosure as things are
/// bound now. Cheap when nothing it depends on was rebound since the last answer for the same
/// enclosure; a body not compiled yet is never effect-free.
pub unsafe fn effect_free(function: Sexp) -> R<bool> {
    let code = function.cdr();
    if code.kind() != SexpType::Bytecode {
        return Ok(false);
    }
    let summary = code.cdr().element(SUMMARY);
    if summary == nil() {
        return Ok(false);
    }
    let slot = CacheSlot::of(code, values::ints(summary.element(0))[0] as usize);
    let enclosure = function.tag();
    let home = home_of(enclosure);
    let heap = crate::heap();
    let key = slot.key();
    let proof = slot.value();
    let current = key == enclosure
        || (key == home
            && match proof_kind(proof) {
                Proof::Fails => true,
                Proof::HoldsHere => false,
                Proof::HoldsFromHome => !frames_bind(enclosure, home, summary),
            });
    if current {
        let epoch = heap.binding_epoch();
        if slot.stamp() == epoch {
            return Ok(proof_kind(proof) != Proof::Fails);
        }
        let stamps = values::reals(proof.element(0));
        let unchanged = (1..values::length(proof))
            .all(|index| stamps[index].to_bits() == heap.lookup_stamp(proof.element(index)));
        if unchanged {
            slot.set_stamp(epoch);
            return Ok(proof_kind(proof) != Proof::Fails);
        }
    }
    let mut found = Vec::new();
    let holds = resolve(function, &mut found, &mut SmallVec::new())?;
    // A failed proof stays sound for any enclosure, because a binding in a function frame on
    // the way could only make a call resolve to something else. A proof that holds carries over
    // to another enclosure with the same home only when no function frame on the way bound any
    // name the body calls.
    let (kind, key) = if !holds {
        (Proof::Fails, home)
    } else if frames_bind(enclosure, home, summary) {
        (Proof::HoldsHere, enclosure)
    } else {
        (Proof::HoldsFromHome, home)
    };
    let proof = values::alloc(SexpType::List, 1 + found.len())?;
    heap.root(proof);
    let stamps = values::alloc(SexpType::Real, 1 + found.len())?;
    values::set_elt(proof, 0, stamps);
    values::reals_mut(stamps)[0] = kind as u8 as f64;
    for (index, speculation) in found.iter().enumerate() {
        values::set_elt(proof, 1 + index, speculation.symbol);
        values::reals_mut(stamps)[1 + index] =
            f64::from_bits(heap.lookup_stamp(speculation.symbol));
    }
    slot.fill(key, proof, heap.binding_epoch());
    Ok(holds)
}

/// The function lookups the effect-free fact of `function` rests on, or `None` when it is not
/// effect-free. Each lookup's symbol and cell are watched, so rebinding either moves the
/// symbol's stamp.
pub unsafe fn effect_free_lookups(function: Sexp) -> R<Option<Vec<Speculation>>> {
    let mut found = Vec::new();
    if bytecode::compiled_body(function)?.kind() != SexpType::Bytecode
        || !resolve(function, &mut found, &mut SmallVec::new())?
    {
        return Ok(None);
    }
    Ok(Some(found))
}

/// Whether a calling handler is established, which could run R code in the middle of a call
/// and look at, or assign, what an argument reads before the callee forces it.
pub fn calling_handler_established() -> bool {
    let mut entry = interp().handlers.as_ref();
    while let Some(handler) = entry {
        if handler.calling {
            return true;
        }
        entry = handler.next.as_ref();
    }
    false
}

/// The closure a variable lookup of `symbol` from `start` finds when it is a function, without
/// forcing a promise or calling an active binding. A lookup that passes a binding holding a
/// non-function is `None`, because that binding could later hold a function without moving any
/// epoch or stamp: only the binding found is watched.
pub unsafe fn resolve_function(symbol: Sexp, start: Sexp) -> Option<Speculation> {
    let heap = crate::heap();
    let mut env = start;
    let empty = heap.known().empty_env;
    while env != empty {
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return None;
            }
            let mut value = cell.cdr();
            if value.kind() == SexpType::Promise {
                if value.car() == unbound() {
                    return None;
                }
                value = value.car();
            }
            if !values::is_function(value) {
                return None;
            }
            return Some(Speculation {
                symbol,
                env: start,
                cell,
                function: value,
            });
        }
        env = env.enclosure();
    }
    None
}

/// The name of the base builtin `function`, when `symbol` names it.
pub unsafe fn base_builtin(function: Sexp, symbol: Sexp) -> Option<&'static str> {
    if !matches!(function.kind(), SexpType::Builtin | SexpType::Special) {
        return None;
    }
    let name = eval::builtin_def(function).ok()?.name;
    (*symbol_name(symbol) == *name).then_some(name)
}

/// What a stored proof says, held as the first element of its stamps vector. A proof is
/// filled under its enclosure for [`Proof::HoldsHere`] and under the enclosure's home (see
/// [`home_of`]) otherwise, so that a closure made afresh by each call of its parent, such as a
/// helper defined inside a function, shares one proof.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Proof {
    Fails = 0,
    /// The fact holds, and some name the body calls resolved in a function frame.
    HoldsHere = 1,
    /// The fact holds, and every name the body calls resolved past the function frames.
    HoldsFromHome = 2,
}

unsafe fn proof_kind(proof: Sexp) -> Proof {
    if proof == nil() {
        return Proof::HoldsHere;
    }
    match values::reals(proof.element(0))[0] as u8 {
        0 => Proof::Fails,
        2 => Proof::HoldsFromHome,
        _ => Proof::HoldsHere,
    }
}

/// The first environment from `env` outward that is not a function frame: a namespace, the
/// global environment or another long-lived one.
unsafe fn home_of(mut env: Sexp) -> Sexp {
    let empty = crate::heap().known().empty_env;
    while env != empty && env.has_gp(gp::FUNCTION_FRAME) {
        env = env.enclosure();
    }
    env
}

/// Whether a function frame from `env` up to `home` binds a name the `summary` lists as called.
unsafe fn frames_bind(mut env: Sexp, home: Sexp, summary: Sexp) -> bool {
    let heap = crate::heap();
    let count = values::length(summary);
    while env != home {
        if (1..count).any(|index| heap.frame_cell(env, summary.element(index)).is_some()) {
            return true;
        }
        env = env.enclosure();
    }
    false
}

/// Adds the symbols `expression` calls to `calls`; `false` when it calls something that is not
/// a symbol, calls a name its own frame binds, or assigns anything but a local variable or an
/// element of one.
unsafe fn collect_calls(
    expression: Sexp,
    locals: &FxHashSet<Sexp>,
    calls: &mut SmallVec<[Sexp; 16]>,
) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    if head.kind() != SexpType::Symbol || locals.contains(&head) {
        return false;
    }
    let mut add = |symbol: Sexp| {
        if !calls.contains(&symbol) {
            calls.push(symbol);
        }
    };
    add(head);
    let arguments = values::pairlist_items(expression.cdr());
    if matches!(&*symbol_name(head), "<-" | "=") {
        // A local is all a symbol target binds; `x[i] <- v` also calls the replacement
        // function, and any other target could call a closure on the frame.
        match arguments.first() {
            Some(&(_, target)) if target.kind() == SexpType::Symbol => {}
            Some(&(_, target))
                if target.kind() == SexpType::Language
                    && target.car().kind() == SexpType::Symbol
                    && values::pairlist_items(target.cdr())
                        .first()
                        .is_some_and(|&(_, variable)| variable.kind() == SexpType::Symbol) =>
            {
                let replacement = match &*symbol_name(target.car()) {
                    "[" => "[<-",
                    "[[" => "[[<-",
                    _ => return false,
                };
                match values::install(replacement) {
                    Ok(symbol) => add(symbol),
                    Err(_) => return false,
                }
                if !values::pairlist_items(target.cdr())
                    .iter()
                    .skip(1)
                    .all(|&(_, index)| collect_calls(index, locals, calls))
                {
                    return false;
                }
                return arguments
                    .iter()
                    .skip(1)
                    .all(|&(_, value)| collect_calls(value, locals, calls));
            }
            _ => return false,
        }
    }
    arguments
        .iter()
        .all(|&(_, argument)| collect_calls(argument, locals, calls))
}

/// Resolves the names `function`'s body calls from its enclosure, adding each lookup to
/// `found`, and says whether every one names an effect-free base builtin or an effect-free
/// closure. `visiting` holds the code and enclosure of each closure whose resolution is under
/// way: a call back into one of them, recursion included, is effect-free exactly when the rest
/// of its body is, so it is assumed to be.
unsafe fn resolve(
    function: Sexp,
    found: &mut Vec<Speculation>,
    visiting: &mut SmallVec<[(Sexp, Sexp); MAX_DEPTH]>,
) -> R<bool> {
    let code = function.cdr();
    if code.kind() != SexpType::Bytecode {
        return Ok(false);
    }
    let enclosure = function.tag();
    if visiting.contains(&(code, enclosure)) {
        return Ok(true);
    }
    let summary = code.cdr().element(SUMMARY);
    if summary == nil() || visiting.len() == MAX_DEPTH {
        return Ok(false);
    }
    visiting.push((code, enclosure));
    let mut holds = true;
    for index in 1..values::length(summary) {
        let symbol = summary.element(index);
        let Some(speculation) = resolve_function(symbol, enclosure) else {
            holds = false;
            break;
        };
        let callee = speculation.function;
        let safe = if callee.kind() == SexpType::Closure {
            bytecode::compiled_body(callee)?.kind() == SexpType::Bytecode
                && resolve(callee, found, visiting)?
        } else {
            match base_builtin(callee, symbol) {
                Some("<-" | "=" | "[<-" | "[[<-") => true,
                Some(name) => EFFECT_FREE_BUILTINS.contains(&name),
                None => false,
            }
        };
        speculation.symbol.set_gp_bits(gp::WATCHED, true);
        speculation.cell.set_gp_bits(gp::WATCHED, true);
        found.push(speculation);
        if !safe {
            holds = false;
            break;
        }
    }
    visiting.pop();
    Ok(holds)
}

/// An argument of a call to an effect-free closure, as tier 0 binds it: the value a symbol is
/// bound to, or the caller's own promise for it, instead of a new promise. `None` when the
/// argument needs its promise after all: the symbol is unbound, missing or an active binding,
/// whose errors and calls have to happen when the callee forces it.
pub unsafe fn eager_argument(symbol: Sexp, env: Sexp) -> Option<Sexp> {
    let cell = eval::find_cell(symbol, env)?;
    if cell.has_gp(gp::ACTIVE_BINDING) {
        return None;
    }
    let value = cell.cdr();
    if value == missing_arg() || value == unbound() {
        return None;
    }
    if value.kind() == SexpType::Promise && value.car() != unbound() {
        return Some(value.car());
    }
    Some(value)
}
