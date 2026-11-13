//! Compiling a script's definitions before the script runs (`ry run --aot`).
//!
//! Before the first top-level expression runs, [`prepare`] lowers every expression of the script
//! and makes the closure of each definition written directly at the top level
//! (`name <- function(...) ...`). The optimizing tier then compiles those closures
//! ([`crate::bytecode::Tier::ahead`]), so the calls the script makes find code instead of
//! compiling at their first call. When the run reaches a definition, [`take`] gives the
//! `function` expression the closure made for it. The closure the script binds is therefore the
//! very object the code was compiled for, and nothing has to find the code again.
//!
//! Nothing of the script runs early: lowering and making a closure have no effect a program can
//! observe, and a lowering error waits for its expression's turn.

use {
    crate::{
        bytecode, interp,
        lower::{Script, lower_script_expression},
        unwind::R,
        values::{self, global_env, nil, symbols},
    },
    heap::{
        Marker,
        object::{Sexp, SexpType},
    },
    rustc_hash::FxHashMap,
    std::sync::OnceLock,
};

/// What a session compiling ahead of time holds between [`prepare`] and the end of the script.
#[derive(Default)]
pub struct State {
    /// Whether the session compiles a script's definitions before it runs the script.
    pub enabled: bool,
    /// The script's lowered expressions, in order, as far as lowering went.
    lowered: Vec<Sexp>,
    /// The closures made ahead whose definitions the run has not reached, by what identifies a
    /// definition: its formals, or its body when it has none.
    waiting: FxHashMap<Sexp, Sexp>,
}

impl State {
    pub unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &expression in &self.lowered {
            marker.mark(expression);
        }
        for &closure in self.waiting.values() {
            marker.mark(closure);
        }
    }
}

/// What a script defines at its top level, as the optimizing tier sees it before the script runs.
pub struct Program<'a> {
    /// The definitions in the order the script runs them.
    pub definitions: &'a [Definition],
    /// The types the script names with `type` or `alias`, by name, each as its definition is
    /// written. An annotation that uses such a name says nothing to the runtime without it.
    pub named_types: &'a FxHashMap<String, String>,
}

/// A closure made ahead for a definition written at the top level of a script.
#[derive(Clone, Copy)]
pub struct Definition {
    /// The name the script binds the closure to.
    pub symbol: Sexp,
    pub closure: Sexp,
}

/// The expressions of a script lowered by [`prepare`].
pub struct Prepared {
    /// Whether they were lowered with source references. An expression that runs after
    /// `options(keep.source)` changed is lowered again, as a console would lower it.
    keep_source: bool,
}

/// Lowers the expressions of `script`, makes the closures of its top-level definitions and has
/// the optimizing tier compile them. Lowering stops at the first expression that fails to lower,
/// which reports its error when the run reaches it.
pub unsafe fn prepare(script: &Script<'_>, keep_source: bool) -> R<Prepared> {
    // The session's state keeps what this makes alive, so the handles made on the way go.
    let mark = crate::heap().handle_mark();
    let prepared = prepare_script(script, keep_source);
    crate::heap().release_handles(mark);
    prepared
}

unsafe fn prepare_script(script: &Script<'_>, keep_source: bool) -> R<Prepared> {
    finish();
    for node in &script.expressions {
        let Ok(expression) = lower_script_expression(script, node, keep_source) else {
            break;
        };
        interp().ahead.lowered.push(expression);
    }
    let mut definitions = Vec::new();
    for index in 0..interp().ahead.lowered.len() {
        let expression = interp().ahead.lowered[index];
        let Some((symbol, value)) = assignment(expression) else {
            continue;
        };
        if value.kind() != SexpType::Language || value.car() != symbols().function {
            continue;
        }
        let (formals, body, srcref) = (
            value.cdr().car(),
            value.cdr().cdr().car(),
            value.cdr().cdr().cdr().car(),
        );
        let Some(key) = definition_key(formals, body) else {
            continue;
        };
        let closure = values::triple(SexpType::Closure, formals, body, global_env())?;
        interp().ahead.waiting.insert(key, closure);
        if srcref != nil() {
            values::set_attr(closure, symbols().srcref, srcref)?;
        }
        definitions.push(Definition { symbol, closure });
    }
    if let Some(tier) = bytecode::tier() {
        let named_types = script.named_types().into_iter().collect();
        (tier.ahead)(&Program {
            definitions: &definitions,
            named_types: &named_types,
        });
    }
    if discards_closures() {
        interp().ahead.waiting.clear();
    }
    Ok(Prepared { keep_source })
}

/// Whether `RY_AHEAD_MISS` asks that no definition gets the closure made ahead for it. Every
/// definition then makes a new closure, as one that runs twice does, so all the code compiled
/// ahead was compiled for functions the script never binds. The run must still give what it
/// gives without compiling ahead, which is what the tests of that path use this for.
fn discards_closures() -> bool {
    static DISCARDS: OnceLock<bool> = OnceLock::new();
    *DISCARDS.get_or_init(|| std::env::var_os("RY_AHEAD_MISS").is_some())
}

/// The lowered expression at `index` of the prepared script, when [`prepare`] lowered it the
/// way the run would lower it now.
pub fn lowered(prepared: &Prepared, index: usize, keep_source: bool) -> Option<Sexp> {
    if prepared.keep_source != keep_source {
        return None;
    }
    interp().ahead.lowered.get(index).copied()
}

/// Forgets the prepared script: the run is over, or carries on from text parsed again.
pub fn finish() {
    let state = &mut interp().ahead;
    state.lowered.clear();
    state.waiting.clear();
}

/// The closure made ahead for the definition with these formals and this body, when the
/// definition is evaluated in the global environment for the first time. A definition gives its
/// closure once: evaluated again, it makes a new closure, as R requires.
#[inline]
pub unsafe fn take(formals: Sexp, body: Sexp, env: Sexp) -> Option<Sexp> {
    let state = &mut interp().ahead;
    if state.waiting.is_empty() || env != global_env() {
        return None;
    }
    state.waiting.remove(&definition_key(formals, body)?)
}

/// The symbol a top-level expression assigns to and the expression of the value, for the plain
/// assignments `name <- value`, `name = value` and `name <<- value`, which all bind in the
/// global environment at the top level.
unsafe fn assignment(expression: Sexp) -> Option<(Sexp, Sexp)> {
    if expression.kind() != SexpType::Language {
        return None;
    }
    let head = expression.car();
    let symbols = symbols();
    if head != symbols.assign && head != symbols.assign_eq && head != symbols.assign2 {
        return None;
    }
    let arguments = expression.cdr();
    if arguments.kind() != SexpType::Pairlist || arguments.cdr().kind() != SexpType::Pairlist {
        return None;
    }
    let target = match arguments.car() {
        target if target.kind() == SexpType::Symbol => target,
        // `"name" <- value` assigns to the variable of that name.
        target if target.kind() == SexpType::String && values::length(target) == 1 => {
            values::install(&values::string_at(target, 0)?).ok()?
        }
        _ => return None,
    };
    Some((target, arguments.cdr().car()))
}

/// What identifies a definition among every closure made from it: the formals, which lowering
/// builds once per `function` expression, or the body of a function without formals. A body
/// that is a symbol or a constant is shared between definitions and identifies none.
unsafe fn definition_key(formals: Sexp, body: Sexp) -> Option<Sexp> {
    if formals.kind() == SexpType::Pairlist {
        return Some(formals);
    }
    let body = bytecode::source_of_body(body);
    (body.kind() == SexpType::Language).then_some(body)
}
