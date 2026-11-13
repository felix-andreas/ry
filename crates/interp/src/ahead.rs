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
//! Nothing of the script runs early. Making a closure has no effect a program can observe, and
//! neither has lowering an expression, with one exception: lowering signals a warning for a
//! literal such as `1.5L`, which a handler or `options(warn = 2)` set earlier in the script
//! must see when the expression's turn comes. An expression whose lowering warns or fails is
//! therefore left out here and lowered when the run reaches it, as without `--aot`.

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
    /// The script's expressions as [`prepare`] lowered them, in order, each until the run takes
    /// it. `None` is one the run lowers itself.
    lowered: Vec<Option<Sexp>>,
    /// Whether [`prepare`] is lowering, so that a warning lowering would signal is held back.
    lowering: bool,
    /// Whether the expression being lowered asked to signal a warning.
    warned: bool,
    /// The closures made ahead whose definitions the run has not reached, by what identifies a
    /// definition: its formals, or its body when it has none.
    waiting: FxHashMap<Sexp, Sexp>,
}

impl State {
    pub unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &expression in self.lowered.iter().flatten() {
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
/// the optimizing tier compile them.
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
        let state = &mut interp().ahead;
        state.lowering = true;
        state.warned = false;
        let lowered = lower_script_expression(script, node, keep_source);
        let state = &mut interp().ahead;
        state.lowering = false;
        // The error is the expression's to report when it runs, so lowering it again finds it.
        let lowered = lowered.ok().filter(|_| !state.warned);
        state.lowered.push(lowered);
    }
    let mut definitions = Vec::new();
    for index in 0..interp().ahead.lowered.len() {
        let Some(expression) = interp().ahead.lowered[index] else {
            continue;
        };
        let Some((symbol, value)) = assignment(expression)? else {
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
/// way the run would lower it now. The session holds the expression no longer: the caller runs
/// it once.
pub fn lowered(prepared: &Prepared, index: usize, keep_source: bool) -> Option<Sexp> {
    let expression = interp().ahead.lowered.get_mut(index)?.take()?;
    (prepared.keep_source == keep_source).then_some(expression)
}

/// Whether a warning lowering is about to signal is held back because [`prepare`] is lowering
/// the expression ahead of its turn. The expression is then lowered again when the run reaches
/// it, which signals the warning.
pub fn holds_back_warning() -> bool {
    let state = &mut interp().ahead;
    state.warned |= state.lowering;
    state.lowering
}

/// Forgets the prepared script, which the session is done with, and the closures made for
/// definitions that never ran.
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
unsafe fn assignment(expression: Sexp) -> R<Option<(Sexp, Sexp)>> {
    if expression.kind() != SexpType::Language {
        return Ok(None);
    }
    let head = expression.car();
    let symbols = symbols();
    if head != symbols.assign && head != symbols.assign_eq && head != symbols.assign2 {
        return Ok(None);
    }
    let arguments = expression.cdr();
    if arguments.kind() != SexpType::Pairlist || arguments.cdr().kind() != SexpType::Pairlist {
        return Ok(None);
    }
    let target = match arguments.car() {
        target if target.kind() == SexpType::Symbol => target,
        // `"name" <- value` assigns to the variable of that name.
        target if target.kind() == SexpType::String && values::length(target) == 1 => {
            let Some(name) = values::string_at(target, 0) else {
                return Ok(None);
            };
            values::install(&name)?
        }
        _ => return Ok(None),
    };
    Ok(Some((target, arguments.cdr().car())))
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
