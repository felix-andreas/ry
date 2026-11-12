//! The language-object evaluator: symbols, promises, calls, closures, argument matching and the
//! control-flow specials. It is the semantic reference the bytecode interpreter must agree with,
//! and it runs everything the bytecode does not compile: the arguments of uncompiled code,
//! `eval()` of constructed calls, and top-level expressions.

use {
    crate::{
        Frame,
        annotations::Entry,
        builtins::{
            Arg, BuiltinDef, Call, Generic, Kind, NativeArgs, Visibility, builtin, internal,
            special,
        },
        bytecode,
        coerce::{CoercionWarnings, real_to_int},
        conditions, deparse, dispatch, interp, moves, scope,
        unwind::{Jump, R, Rooted, Unwind, error, error_in, jump},
        utils::{PROFILE_TICK, record_profile_sample},
        values::{
            self, alloc, cons_tagged, length, missing_arg, nil, symbol_name, symbols, triple,
            unbound,
        },
    },
    heap::{
        ENVIRONMENT_BYTES, Formal, TRIPLE_BYTES,
        activity::{self, Activity, Counter},
        object::{NA_INTEGER, Sexp, SexpType, gp},
    },
    smallvec::{SmallVec, smallvec},
    std::sync::atomic::Ordering,
    thin_vec::ThinVec,
};

/// A call's arguments as values or promises. Inline for the common few, so a closure call
/// allocates no vector for them.
pub type Args = SmallVec<[Arg; 10]>;

/// What [`match_args`] returns: one value per formal.
pub type Matched = SmallVec<[Sexp; 8]>;

/// The promise bit set while its code runs, to report recursive default references.
pub const PROMISE_SEEN: u16 = 1 << 0;

/// Evaluates `expression` in `env`. An error raised below is signalled here, so handlers run
/// close to where it happened.
pub unsafe fn eval(expression: Sexp, env: Sexp) -> R<Sexp> {
    match eval_unsignalled(expression, env) {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        result => result,
    }
}

unsafe fn eval_unsignalled(expression: Sexp, env: Sexp) -> R<Sexp> {
    match expression.kind() {
        SexpType::Symbol => {
            interp().visible = true;
            let value = eval_symbol(expression, env)?;
            Ok(value)
        }
        SexpType::Language => {
            interp().visible = true;
            let _activity = activity::enter(Activity::Evaluator, 0);
            eval_call(expression, env)
        }
        SexpType::Promise => force_promise(expression),
        SexpType::Bytecode => bytecode::execute(expression, env),
        SexpType::Dots => Err(error("'...' used in an incorrect context")),
        _ => {
            interp().visible = true;
            Ok(expression)
        }
    }
}

/// The value of a symbol: its binding, with promises forced.
pub unsafe fn eval_symbol(symbol: Sexp, env: Sexp) -> R<Sexp> {
    eval_symbol_reporting(symbol, env, || lexical_call(env))
}

/// [`eval_symbol`] for a lookup that starts in `env` on behalf of a frame that is not `env`
/// itself, such as a slot frame that has no environment or a frame whose own bindings were
/// already searched: an unbound symbol is reported against `lexical()`, the call of the frame
/// the code runs in, as GNU R's `getLexicalCall` does.
pub unsafe fn eval_symbol_reporting(
    symbol: Sexp,
    env: Sexp,
    lexical: impl FnOnce() -> Sexp,
) -> R<Sexp> {
    let symbols = symbols();
    if symbol == symbols.dots {
        return Err(error("'...' used in an incorrect context"));
    }
    if let Some(index) = dot_dot_index(symbol) {
        return dot_dot_value(index, env);
    }
    // The empty symbol is `R_MissingArg`, whose value in GNU R is itself, so evaluating it is
    // a missing argument without a name rather than an unbound object.
    let value = if symbol == missing_arg() {
        symbol
    } else {
        find_var(symbol, env)?
    };
    if value == unbound() {
        return Err(error_in(
            lexical(),
            format!("object '{}' not found", symbol_name(symbol)),
        ));
    }
    if value == missing_arg() {
        let name = symbol_name(symbol);
        return Err(error(if name.is_empty() {
            "argument is missing, with no default".to_string()
        } else {
            format!("argument \"{name}\" is missing, with no default")
        }));
    }
    if value.kind() == SexpType::Promise {
        let forced = force_promise(value)?;
        crate::heap().root(forced);
        return Ok(forced);
    }
    Ok(value)
}

/// `..N`'s position, for the symbols that name one.
pub unsafe fn dot_dot_index(symbol: Sexp) -> Option<usize> {
    let name = symbol.car().bytes();
    let digits = name.strip_prefix(b"..")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

unsafe fn dot_dot_value(index: usize, env: Sexp) -> R<Sexp> {
    if index == 0 {
        return Err(error("indexing '...' with non-positive index 0"));
    }
    let dots = find_var(symbols().dots, env)?;
    if dots == unbound() {
        return Err(error(format!(
            "..{index} used in an incorrect context, no ... to look in"
        )));
    }
    let mut cell = dots;
    let mut position = 1;
    while cell.kind() == SexpType::Dots || cell.kind() == SexpType::Pairlist {
        if position == index {
            let value = cell.car();
            if value == missing_arg() {
                return Err(error(format!(
                    "argument \"..{index}\" is missing, with no default"
                )));
            }
            return if value.kind() == SexpType::Promise {
                force_promise(value)
            } else {
                Ok(value)
            };
        }
        position += 1;
        cell = cell.cdr();
    }
    Err(error(format!(
        "the ... list contains fewer than {index} element{}",
        if index == 1 { "" } else { "s" }
    )))
}

/// The binding cell for `symbol` from `env` outward.
pub unsafe fn find_cell(symbol: Sexp, mut env: Sexp) -> Option<Sexp> {
    let heap = crate::heap();
    let known = heap.known();
    while env != known.empty_env {
        if env == known.global_env {
            return heap.global_cell(symbol).map(|(cell, _)| cell);
        }
        if let Some(cell) = heap.frame_cell(env, symbol) {
            return Some(cell);
        }
        env = env.enclosure();
    }
    None
}

/// The value of a binding cell, calling an active binding's function.
pub unsafe fn cell_value(cell: Sexp) -> R<Sexp> {
    if cell.has_gp(gp::ACTIVE_BINDING) {
        interp().active_binding_reads += 1;
        let function = cell.cdr();
        let value = apply(nil(), function, &[], values::global_env())?;
        return Ok(value);
    }
    Ok(cell.cdr())
}

/// The call whose frame evaluates code in `env`, which GNU R names in an error about a missing
/// variable: so a promise forced inside a callee still blames the caller's code, and top-level
/// code blames no call. An environment no frame owns, like one `eval` was handed, falls back to
/// the innermost call.
pub(crate) unsafe fn lexical_call(env: Sexp) -> Sexp {
    let frames = &interp().frames;
    if let Some(frame) = frames.iter().rev().find(|frame| frame.owns(env)) {
        return frame.call;
    }
    if env == values::global_env() {
        return nil();
    }
    crate::unwind::current_call()
}

/// `findVar`: the value bound to `symbol` from `env` outward, or `R_UnboundValue`. Promises are
/// returned unforced.
pub unsafe fn find_var(symbol: Sexp, env: Sexp) -> R<Sexp> {
    let _activity = activity::enter(Activity::Lookup, 0);
    match find_cell(symbol, env) {
        Some(cell) => cell_value(cell),
        None if symbol == symbols().tmp => Ok(tmp_value(env)),
        None => Ok(unbound()),
    }
}

/// The `*tmp*` a complex assignment running in `env` or an environment it encloses stands for,
/// or `R_UnboundValue`.
unsafe fn tmp_value(env: Sexp) -> Sexp {
    let empty = values::empty_env();
    for &(assignment_env, value) in interp().tmp_values.iter().rev() {
        let mut scope = env;
        while scope != empty {
            if scope == assignment_env {
                return value;
            }
            scope = scope.enclosure();
        }
    }
    unbound()
}

/// The value bound to `symbol` in `env`'s own frame, or `R_UnboundValue`.
pub unsafe fn find_var_in_frame(symbol: Sexp, env: Sexp) -> R<Sexp> {
    match crate::heap().frame_cell(env, symbol) {
        Some(cell) => cell_value(cell),
        None => Ok(unbound()),
    }
}

/// `findFun`: the first function bound to `symbol` from `env` outward.
pub unsafe fn find_fun(symbol: Sexp, env: Sexp) -> R<Sexp> {
    find_called_fun(symbol, env, unbound())
}

/// `findFun3`: [`find_fun`] for the head of `call`, which a missing function's error names, as
/// in GNU R.
pub unsafe fn find_called_fun(symbol: Sexp, mut env: Sexp, call: Sexp) -> R<Sexp> {
    let _activity = activity::enter(Activity::Lookup, 0);
    let heap = crate::heap();
    let known = heap.known();
    while env != known.empty_env {
        let cell = if env == known.global_env {
            // The first binding on the search path usually is the function; only when it is not
            // does the walk go on frame by frame, to skip it as GNU R does.
            if let Some((cell, _)) = heap.global_cell(symbol)
                && !cell.has_gp(gp::ACTIVE_BINDING)
            {
                let mut value = cell.cdr();
                if value.kind() == SexpType::Promise && value.car() != unbound() {
                    value = value.car();
                }
                if values::is_function(value) {
                    return Ok(value);
                }
            }
            heap.frame_cell(env, symbol)
        } else {
            heap.frame_cell(env, symbol)
        };
        if let Some(cell) = cell {
            let mut value = cell_value(cell)?;
            if value.kind() == SexpType::Promise {
                value = force_promise(value)?;
            }
            if values::is_function(value) {
                return Ok(value);
            }
            if value == missing_arg() {
                return Err(error(format!(
                    "argument \"{}\" is missing, with no default",
                    symbol_name(symbol)
                )));
            }
        }
        env = env.enclosure();
    }
    Err(error_in(
        call,
        format!("could not find function \"{}\"", symbol_name(symbol)),
    ))
}

/// Binds `symbol` in `env`'s own frame.
pub unsafe fn define(symbol: Sexp, value: Sexp, env: Sexp) -> R<()> {
    let heap = crate::heap();
    if let Some(cell) = heap.frame_cell(env, symbol)
        && cell.has_gp(gp::ACTIVE_BINDING)
    {
        let function = cell.cdr();
        apply(
            nil(),
            function,
            &[Arg { tag: nil(), value }],
            values::global_env(),
        )?;
        return Ok(());
    }
    heap.define(env, symbol, value)?;
    Ok(())
}

/// `<<-`: assigns to the nearest existing binding above `env`, or in the global environment.
pub unsafe fn set_var(symbol: Sexp, value: Sexp, env: Sexp) -> R<()> {
    let heap = crate::heap();
    let empty = heap.known().empty_env;
    let mut frame = env;
    while frame != empty {
        if let Some(cell) = heap.frame_cell(frame, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                let function = cell.cdr();
                apply(
                    nil(),
                    function,
                    &[Arg { tag: nil(), value }],
                    values::global_env(),
                )?;
                return Ok(());
            }
            heap.set_binding_value(cell, value)?;
            cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
            return Ok(());
        }
        frame = frame.enclosure();
    }
    define(symbol, value, values::global_env())
}

/// A promise to evaluate `code` in `env`.
pub unsafe fn make_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    activity::count(Counter::PromisesCreated);
    let promise = triple(SexpType::Promise, unbound(), code, env)?;
    count_unforced(env);
    Ok(promise)
}

/// A promise to run the compiled `code` in `env`, the frame of the code it was compiled with.
/// Its expression is the code's source, as for any other promise; the code itself sits in the
/// attribute slot, which a promise otherwise never uses (see `force_promise`).
pub unsafe fn compiled_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    activity::count(Counter::PromisesCreated);
    let heap = crate::heap();
    let promise = heap.alloc_compiled_promise(unbound(), bytecode::source_of(code), env, code)?;
    heap.root(promise);
    count_unforced(env);
    Ok(promise)
}

/// Counts a new unforced promise over `env` when that is a slot frame's pending environment.
/// Forcing or settling the promise takes it off the count again, and a frame that returns with
/// a count of zero skips the check for promises and closures that outlive it
/// ([`finish_frame`]), so every promise made over a pending environment, wherever it is made,
/// must be counted here.
unsafe fn count_unforced(env: Sexp) {
    if env.kind() == SexpType::Environment && env.has_gp(gp::PENDING_FRAME) {
        interp().frames[pending_owner(env)].unforced += 1;
    }
}

/// A promise that already holds `value`, whose code is `code` for `substitute` to see.
pub unsafe fn forced_promise(code: Sexp, value: Sexp) -> R<Sexp> {
    triple(SexpType::Promise, value, code, nil())
}

pub unsafe fn force_promise(promise: Sexp) -> R<Sexp> {
    if promise.car() != unbound() {
        return Ok(promise.car());
    }
    if promise.has_gp(PROMISE_SEEN) {
        return Err(error(
            "promise already under evaluation: recursive default argument reference or earlier problems?",
        ));
    }
    moves::forcing(promise)?;
    activity::count(Counter::PromisesForced);
    // The code being forced can remove the last binding that holds the promise (a loader
    // placeholder replaces itself), and the promise's environment must outlive its code, so
    // the promise stays a root while it runs, as GNU R's pending-promise stack keeps it.
    let heap = crate::heap();
    let mark = heap.handle_mark();
    heap.root(promise);
    promise.set_gp_bits(PROMISE_SEEN, true);
    let code = promise.attrib();
    let env = promise.tag();
    // Releasing a promise's last counted reference clears its environment, so an unforced
    // promise without one was held by a root the counts do not see, and forcing it here would
    // evaluate its code nowhere instead of where it was made.
    if env == nil()
        && !promise.has_gp(gp::ARGUMENT_REFERENCE)
        && (cfg!(debug_assertions)
            || syntax::testing::invariants_enabled()
            || heap::gc_stress_requested().is_some())
    {
        panic!("a promise was forced after its last counted reference was released");
    }
    let result = if promise.has_gp(gp::ARGUMENT_REFERENCE) {
        force_reference(promise)
    } else if env.has_gp(gp::PENDING_FRAME) {
        force_slot_promise(promise, env)
    } else if code.kind() == SexpType::Bytecode {
        bytecode::run_promise(code, env)
    } else {
        eval(promise.cdr(), env)
    };
    promise.set_gp_bits(PROMISE_SEEN, false);
    let settled = result.and_then(|value| settle_promise(promise, value));
    heap.release_handles(mark);
    // Releasing dropped the roots made while forcing, and the promise may hold the only
    // reference to the value it returns.
    if let Ok(value) = &settled {
        heap.root(*value);
    }
    settled
}

/// Forces a promise over the slots of the live slot frame whose pending environment is `env`:
/// compiled code runs on the slots, a name reads its slot, and anything else, or a name whose
/// slot holds an empty argument, gets the frame's environment and evaluates there.
#[cold]
#[inline(never)]
unsafe fn force_slot_promise(promise: Sexp, env: Sexp) -> R<Sexp> {
    let owner = pending_owner(env);
    let value = slot_promise_value(promise, owner)?;
    // A promise whose forcing failed stays unforced, so only a value settles the count.
    let frame = &mut interp().frames[owner];
    debug_assert!(
        frame.unforced > 0,
        "a frame counts fewer unforced promises than it has"
    );
    frame.unforced = frame.unforced.saturating_sub(1);
    Ok(value)
}

unsafe fn slot_promise_value(promise: Sexp, owner: usize) -> R<Sexp> {
    let frame = &interp().frames[owner];
    let (function, header) = (frame.function, frame.slots as usize);
    let code = promise.attrib();
    if code.kind() == SexpType::Bytecode {
        return bytecode::run_promise_on_slots(code, function, owner, header);
    }
    let expression = promise.cdr();
    if expression.kind() == SexpType::Symbol {
        match slot_content(function, header, expression) {
            None => return eval_enclosed_name(expression, function.tag(), owner),
            Some(value) if value.kind() == SexpType::Promise => return force_promise(value),
            Some(value) if value != missing_arg() => return Ok(value),
            Some(_) => {}
        }
    }
    eval(
        expression,
        frame_env(owner, Reify::Site("promise over the frame"))?,
    )
}

/// Where a name is bound for a live slot frame's pending environment ([`pending_name`]).
pub enum PendingName {
    /// The content of the slot that binds the name: a value, a promise, or the empty argument.
    Slot(Sexp),
    /// The frame's slots leave the name to this environment, the closure's enclosure.
    Enclosure(Sexp),
}

/// What the name `symbol` reads in the live slot frame whose pending environment is `pending`,
/// found where forcing a promise of it there finds it ([`slot_promise_value`]), but without
/// forcing or evaluating anything.
pub unsafe fn pending_name(pending: Sexp, symbol: Sexp) -> PendingName {
    let frame = &interp().frames[pending_owner(pending)];
    match slot_content(frame.function, frame.slots as usize, symbol) {
        Some(value) => PendingName::Slot(value),
        None => PendingName::Enclosure(frame.function.tag()),
    }
}

/// What the variable `symbol` is bound to from `env`, read without forcing or calling anything:
/// a value or a promise, or `None` for an unbound name, the empty argument or an active
/// binding. A slot frame's pending environment binds nothing itself, so its names are read from
/// the frame's slots.
pub unsafe fn peek_variable(symbol: Sexp, env: Sexp) -> Option<Sexp> {
    let env = if env.has_gp(gp::PENDING_FRAME) {
        match pending_name(env, symbol) {
            PendingName::Slot(value) => {
                return (value != missing_arg()).then_some(value);
            }
            PendingName::Enclosure(enclosure) => enclosure,
        }
    } else {
        env
    };
    let cell = find_cell(symbol, env)?;
    let value = cell.cdr();
    (!cell.has_gp(gp::ACTIVE_BINDING) && value != missing_arg() && value != unbound())
        .then_some(value)
}

/// Settles `promise`, unforced, with the `value` forcing it would give, which the caller found
/// without forcing it ([`pending_name`]): a promise over a slot frame's pending environment
/// then leaves the frame's count of promises that may still be unforced, as forcing it does.
pub unsafe fn settle_unforced(promise: Sexp, value: Sexp) -> R<Sexp> {
    // Two arguments can read through the same promise, which only the first settles.
    if promise.car() != unbound() {
        return Ok(promise.car());
    }
    let env = promise.tag();
    let counted = !promise.has_gp(gp::ARGUMENT_REFERENCE)
        && env.kind() == SexpType::Environment
        && env.has_gp(gp::PENDING_FRAME);
    // A checked promise's annotation check can fail, which leaves the promise unforced.
    let settled = settle_promise(promise, value)?;
    if counted {
        let frame = &mut interp().frames[pending_owner(env)];
        debug_assert!(
            frame.unforced > 0,
            "a frame counts fewer unforced promises than it has"
        );
        frame.unforced = frame.unforced.saturating_sub(1);
    }
    Ok(settled)
}

/// The position of the live slot frame whose pending environment is `pending`.
unsafe fn pending_owner(pending: Sexp) -> usize {
    let Some(owner) = interp()
        .frames
        .iter()
        .rposition(|frame| frame.pending == pending)
    else {
        panic!("a promise names the pending environment of a frame that has returned");
    };
    owner
}

/// What the live slot frame at `owner` binds `symbol` to, read from its slots without giving the
/// frame an environment, or `None` when the slots leave the name to the enclosure.
pub unsafe fn frame_slot(owner: usize, symbol: Sexp) -> Option<Sexp> {
    let frame = &interp().frames[owner];
    slot_content(frame.function, frame.slots as usize, symbol)
}

/// [`frame_slot`] for the slot frame whose pending environment is `env`.
pub unsafe fn pending_slot(env: Sexp, symbol: Sexp) -> Option<Sexp> {
    frame_slot(pending_owner(env), symbol)
}

/// The content of the slot of `function`'s frame at register `header` that binds `symbol`, or
/// `None` when the slots leave the name to the enclosure: no slot has that name, or it is unbound.
unsafe fn slot_content(function: Sexp, header: usize, symbol: Sexp) -> Option<Sexp> {
    let names = function.cdr().cdr().element(bytecode::SLOTS).element(1);
    let slot = (0..values::length(names)).find(|&index| names.element(index) == symbol)?;
    let value = *bytecode::register_address(header + 1 + slot);
    (value != unbound()).then_some(value)
}

/// Evaluates `symbol` from `enclosure` for the slot frame at `owner`, whose own slots do not bind
/// it, so an unbound name is reported against the frame's call and not the enclosure's.
unsafe fn eval_enclosed_name(symbol: Sexp, enclosure: Sexp, owner: usize) -> R<Sexp> {
    interp().visible = true;
    let lexical = || interp().frames.get(owner).map_or(nil(), |frame| frame.call);
    match eval_symbol_reporting(symbol, enclosure, lexical) {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        result => result,
    }
}

/// A promise of `expression`, compiled code or a name, over the slots of the slot frame at
/// `owner`. It names the frame's pending environment, which is made bare on the first such
/// promise and gets its bindings only if the frame reifies.
pub unsafe fn slot_promise(owner: usize, expression: Sexp) -> R<Sexp> {
    let env = pending_env(owner)?;
    if expression.kind() == SexpType::Bytecode {
        compiled_promise(expression, env)
    } else {
        make_promise(expression, env)
    }
}

/// A closure of `formals` and `body` defined in the slot frame whose header register is
/// `header`, which has no environment yet. The closure's environment is the frame's pending
/// one, so defining it leaves the frame in its slots: anything that reads the environment's
/// bindings, such as a call of the closure or `environment(f)$x`, reifies the frame into that
/// same object first ([`resolve_pending_read`]), and a frame that returns with the closure
/// still reachable reifies then ([`finish_frame`]). A frame that makes more closures than
/// [`crate::FRAME_CLOSURES`] reifies instead, which bounds the list that tracks them.
pub unsafe fn slot_closure(header: usize, formals: Sexp, body: Sexp, srcref: Sexp) -> R<Sexp> {
    let state = interp();
    let Some(owner) = state
        .frames
        .iter()
        .rposition(|frame| frame.slots as usize == header)
    else {
        panic!("a slot frame's activation outlived its frame");
    };
    let made = state
        .pending_closures
        .iter()
        .filter(|&&(frame, _)| frame as usize == owner)
        .count();
    if made >= crate::FRAME_CLOSURES {
        let env = frame_env(owner, Reify::Site("function definition"))?;
        return make_closure(formals, body, srcref, env);
    }
    let env = pending_env(owner)?;
    let closure = make_closure(formals, body, srcref, env)?;
    let state = interp();
    state.pending_closures.push((owner as u32, closure));
    state.frames[owner].unforced += 1;
    Ok(closure)
}

/// The heap's resolver for a read of a pending environment's bindings
/// (`heap::set_pending_resolver`): it reifies the live slot frame the environment belongs to.
/// The read may come from anywhere, including native code between allocations it has not
/// protected, so the frame reifies with collection suspended.
pub unsafe fn resolve_pending_read(env: Sexp) {
    let Some(owner) = interp()
        .frames
        .iter()
        .rposition(|frame| frame.pending == env)
    else {
        panic!("the bindings of a slot frame's environment were read after the frame returned");
    };
    // Bindings read through an environment `parent.frame` handed out are the frame observed.
    let cause = if interp().frames[owner].pending_shared {
        Reify::Site("parent.frame")
    } else {
        Reify::Site("environment read")
    };
    let heap = crate::heap();
    heap.inhibit_collection();
    let result = reify(owner, cause);
    crate::heap().allow_collection();
    if let Err(unwind) = result {
        panic!("a slot frame failed to make its environment: {unwind:?}");
    }
}

/// An argument reference: the promise a slot frame at `owner` passes for the variable
/// `symbol` to a callee that takes its arguments straight into slots ([`call_with_plan`]).
/// Instead of an environment it names the variable's slot (`NO_REFERENCE_SLOT` when the frame
/// keeps none for it), so passing a variable allocates nothing and leaves the caller without
/// an environment. It forces like any promise ([`force_promise`]), once, reading the slot at
/// that moment, and it comes from [`Interp::references`], which the calling instruction
/// releases when the call returns. Nothing may keep one past that: a frame that hands its
/// arguments to code that could keep them ([`materialize_references`]) turns them into
/// ordinary promises first.
pub unsafe fn argument_reference(owner: usize, slot: u32, symbol: Sexp) -> R<Sexp> {
    let reference = pooled_reference(symbol)?;
    let header = interp().frames[owner].slots;
    values::ints_mut(interp().reference_targets[interp().references_used as usize - 1])
        .copy_from_slice(&[owner as i32, slot as i32, header as i32]);
    Ok(reference)
}

/// An argument reference to `expression`, a variable, uncompiled code or a constant, of a
/// caller that has the environment `env`, for a callee that takes its arguments straight into
/// slots: it forces as a promise of the expression in `env` would, and like a slot frame's reference it allocates nothing and is
/// released when the call returns. The reference keeps `env` in its environment field, where
/// a slot frame's reference keeps nil.
pub unsafe fn env_reference(env: Sexp, expression: Sexp) -> R<Sexp> {
    let reference = pooled_reference(expression)?;
    crate::heap().set_tag(reference, env);
    Ok(reference)
}

/// An argument reference to the compiled argument `code` of a caller that has the environment
/// `env`: [`env_reference`] of the code's source, which keeps the code as its attribute in
/// place of its frame target and runs it when forced, as a compiled promise would.
pub unsafe fn env_code_reference(env: Sexp, code: Sexp) -> R<Sexp> {
    let reference = env_reference(env, bytecode::source_of(code))?;
    crate::heap().set_attrib(reference, code);
    Ok(reference)
}

/// An argument reference to the compiled argument `code` of the slot frame at `owner`, for a
/// callee that takes its arguments straight into slots: it forces as [`slot_promise`] of the
/// code would, by running the code on the frame's slots, without a promise or an environment.
/// It keeps the code in its environment field and the code's source as its expression, so
/// `substitute` sees the source.
pub unsafe fn code_reference(owner: usize, code: Sexp) -> R<Sexp> {
    let reference = argument_reference(owner, NO_REFERENCE_SLOT, bytecode::source_of(code))?;
    crate::heap().set_tag(reference, code);
    Ok(reference)
}

/// The environment an environment reference ([`env_reference`]) reads its variable in.
#[inline]
unsafe fn reference_env(reference: Sexp) -> Option<Sexp> {
    let tag = reference.tag();
    (tag.kind() == SexpType::Environment).then_some(tag)
}

/// The compiled code a code reference ([`code_reference`]) runs.
#[inline]
unsafe fn reference_code(reference: Sexp) -> Option<Sexp> {
    let tag = reference.tag();
    (tag.kind() == SexpType::Bytecode).then_some(tag)
}

/// The next free reference of [`Interp::references`], naming `symbol`, with its target left
/// for the caller to fill in.
unsafe fn pooled_reference(symbol: Sexp) -> R<Sexp> {
    let state = interp();
    let heap = crate::heap();
    let used = state.references_used as usize;
    let reference = match state.references.get(used) {
        Some(&reference) => {
            heap.set_cdr(reference, symbol);
            reference
        }
        None => {
            let target = values::integer_vector(&[0, 0, 0])?;
            heap.root(target);
            let reference = heap.alloc_compiled_promise(unbound(), symbol, nil(), target)?;
            reference.set_gp_bits(gp::ARGUMENT_REFERENCE, true);
            // The pool's own counts keep a reference that a slot releases, and a target that a
            // code reference displaced, from being freed.
            reference.increment_refcount();
            target.increment_refcount();
            state.references.push(reference);
            state.reference_targets.push(target);
            reference
        }
    };
    state.references_used += 1;
    activity::count(Counter::ArgumentReferences);
    Ok(reference)
}

/// The slot an argument reference names for a variable its frame keeps in no slot: forcing it
/// reads the variable where the frame's code would, in the frame's enclosure until the frame
/// has an environment and in that environment after.
pub const NO_REFERENCE_SLOT: u32 = u32::MAX;

/// Returns the argument references made since `mark` to the pool once the call they were made
/// for has returned, dropping the values they were forced to and the expressions they named.
/// A pooled reference is a root until a later call reuses it, so a field left set keeps its
/// object alive: a call built by native code holds argument values inline as its expressions,
/// and a connection passed that way would never be closed by the collector.
pub unsafe fn release_references(mark: u32) {
    let state = interp();
    let heap = crate::heap();
    let unbound = unbound();
    for index in mark as usize..state.references_used as usize {
        let reference = state.references[index];
        if reference.car() != unbound {
            heap.set_car(reference, unbound);
        }
        if reference.cdr() != nil() {
            heap.set_cdr(reference, nil());
        }
        // Only an environment or code reference changes the other fields, and forcing one
        // clears its tag again, so the attribute is compared with the target on its own.
        if reference.tag() != nil() {
            heap.set_tag(reference, nil());
        }
        let target = state.reference_targets[index];
        if reference.attrib() != target {
            heap.set_attrib(reference, target);
        }
    }
    state.references_used = mark;
}

/// Whether `value` is an argument reference ([`argument_reference`]).
#[inline]
pub unsafe fn is_reference(value: Sexp) -> bool {
    value.kind() == SexpType::Promise && value.has_gp(gp::ARGUMENT_REFERENCE)
}

/// The frame position and slot an argument reference names. A reference is released before
/// its frame returns, so one that names another frame is a broken invariant.
unsafe fn reference_target(reference: Sexp) -> (usize, u32) {
    let &[owner, slot, header] = values::ints(reference.attrib()) else {
        panic!("an argument reference lost its target");
    };
    if interp()
        .frames
        .get(owner as usize)
        .is_none_or(|frame| frame.slots != header as u32)
    {
        panic!("an argument reference outlived the frame it names");
    }
    (owner as usize, slot as u32)
}

/// Forces an unforced argument reference: what its variable holds in the frame it names, read
/// as a promise of the variable's name in that frame would read it.
unsafe fn force_reference(reference: Sexp) -> R<Sexp> {
    let symbol = reference.cdr();
    if let Some(env) = reference_env(reference) {
        let code = reference.attrib();
        if code.kind() == SexpType::Bytecode {
            return bytecode::run_promise(code, env);
        }
        return eval(symbol, env);
    }
    let (owner, slot) = reference_target(reference);
    let frame = &interp().frames[owner];
    if let Some(code) = reference_code(reference) {
        return bytecode::run_promise_on_slots(code, frame.function, owner, frame.slots as usize);
    }
    if frame.env != nil() {
        return eval(symbol, frame.env);
    }
    if slot == NO_REFERENCE_SLOT {
        return eval_enclosed_name(symbol, frame.function.tag(), owner);
    }
    let value = *bytecode::register_address(frame.slots as usize + 1 + slot as usize);
    if value == unbound() {
        return eval_enclosed_name(symbol, frame.function.tag(), owner);
    }
    if value.kind() == SexpType::Promise {
        return force_promise(value);
    }
    if value == missing_arg() {
        return eval(
            symbol,
            frame_env(owner, Reify::Site("promise over the frame"))?,
        );
    }
    Ok(value)
}

/// The value an unforced `promise` stands for when reading it runs no R code, settling it as
/// forcing would: its code is a constant, or names a variable bound to a value, or to another
/// such promise, as when a wrapper passes its argument on (`g <- function(x) typeof(x)`). Every
/// promise on the chain is settled, innermost first, as forcing the outer one settles them.
/// `None`, with nothing settled, when the chain ends in code that would run.
pub unsafe fn settled_without_code(promise: Sexp) -> R<Option<Sexp>> {
    // Longer chains are rare, and a chain that loops back (`function(x = y, y = x)`) must end
    // in the error forcing raises, so it is left to forcing.
    const CHAIN_LIMIT: usize = 16;
    if promise.has_gp(PROMISE_SEEN) {
        return Ok(None);
    }
    let mut chain: SmallVec<[Sexp; 4]> = smallvec![promise];
    let mut link = promise;
    let mut value = loop {
        let next = if is_reference(link) {
            reference_link(link)
        } else {
            promise_link(link)
        };
        let Some(next) = next else {
            return Ok(None);
        };
        if next.kind() != SexpType::Promise {
            break next;
        }
        if next.car() != unbound() {
            break next.car();
        }
        if chain.len() == CHAIN_LIMIT || next.has_gp(PROMISE_SEEN) {
            return Ok(None);
        }
        chain.push(next);
        link = next;
    };
    for &link in chain.iter().rev() {
        value = settle_unforced(link, value)?;
    }
    Ok(Some(value))
}

/// What reading an unforced plain promise yields without running code: its constant code, or
/// the binding its variable code names (a value or another promise). A promise over a slot
/// frame's pending environment reads its variable from the frame's slots ([`peek_variable`]).
unsafe fn promise_link(promise: Sexp) -> Option<Sexp> {
    if promise.has_gp(gp::CHECKED_PROMISE) {
        return None;
    }
    let code = promise.cdr();
    match code.kind() {
        SexpType::Symbol if code != missing_arg() && code != symbols().dots => {
            peek_variable(code, promise.tag())
        }
        SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode => None,
        _ => Some(code),
    }
}

/// What reading an unforced argument reference yields without running code: what its
/// variable holds, a value or a promise.
unsafe fn reference_link(reference: Sexp) -> Option<Sexp> {
    if let Some(env) = reference_env(reference) {
        let expression = reference.cdr();
        if reference.attrib().kind() == SexpType::Bytecode {
            return None;
        }
        return match expression.kind() {
            // The environment can be a slot frame's pending one, as a call evaluated in
            // `parent.frame()` makes it.
            SexpType::Symbol if expression != missing_arg() && expression != symbols().dots => {
                peek_variable(expression, env)
            }
            SexpType::Symbol => None,
            SexpType::Language => None,
            _ => Some(expression),
        };
    }
    reference_variable(reference)
}

/// What the variable a slot frame's argument reference names holds, read where forcing the
/// reference would read it but without forcing or calling anything: a value or a promise, or
/// `None` for a code reference, an environment reference, or an unbound, missing or active
/// variable. A variable the frame keeps in no slot, or whose slot is unbound, is read in the
/// frame's enclosure ([`peek_variable`]), so a global passed from a slot frame needs no
/// environment for that frame.
pub unsafe fn reference_variable(reference: Sexp) -> Option<Sexp> {
    if reference_env(reference).is_some() || reference_code(reference).is_some() {
        return None;
    }
    let symbol = reference.cdr();
    let (owner, slot) = reference_target(reference);
    let frame = &interp().frames[owner];
    if frame.env != nil() {
        return peek_variable(symbol, frame.env);
    }
    let value = if slot == NO_REFERENCE_SLOT {
        unbound()
    } else {
        *bytecode::register_address(frame.slots as usize + 1 + slot as usize)
    };
    if value == unbound() {
        return peek_variable(symbol, frame.function.tag());
    }
    (value != missing_arg()).then_some(value)
}

/// Whether the variable an unforced argument reference names is a missing argument in its
/// frame, as [`value_is_missing`] asks of a promise of a name.
unsafe fn reference_is_missing(reference: Sexp) -> R<bool> {
    let symbol = reference.cdr();
    if reference_code(reference).is_some() || symbol.kind() != SexpType::Symbol {
        return Ok(false);
    }
    let env = if let Some(env) = reference_env(reference) {
        env
    } else {
        let (owner, _) = reference_target(reference);
        interp().frames[owner].env
    };
    if env != nil() {
        if crate::heap().frame_cell(env, symbol).is_none() {
            return Ok(false);
        }
        return symbol_is_missing(symbol, env, false);
    }
    let (owner, slot) = reference_target(reference);
    let frame = &interp().frames[owner];
    if slot == NO_REFERENCE_SLOT {
        return Ok(false);
    }
    let value = *bytecode::register_address(frame.slots as usize + 1 + slot as usize);
    if value == unbound() {
        return Ok(false);
    }
    value_is_missing(value)
}

/// Turns the argument references the frame at `index` was called with into ordinary promises
/// over the frames they name ([`detach_reference`]), wherever the frame holds them: its
/// arguments, its slots and the `...` it collected. Every path that hands a frame's arguments
/// or variables to code that may keep them past the call (its environment, dispatch to a
/// method) runs this first, since a reference is reused once the call returns.
pub unsafe fn materialize_references(index: usize) -> R<()> {
    let state = interp();
    let Some(frame) = state.frames.get(index) else {
        return Ok(());
    };
    if frame.slots == crate::NO_SLOTS {
        return Ok(());
    }
    let start = frame.args as usize;
    let end = state
        .frames
        .get(index + 1)
        .map_or(state.frame_args.len(), |next| next.args as usize);
    for position in start..end {
        let reference = interp().frame_args[position].value;
        if is_reference(reference) {
            detach_reference(reference)?;
        }
    }
    Ok(())
}

/// [`detach_reference`] for each argument reference in `args`, before they go to a call that
/// may keep them past its return. Each promise is rooted in the current handle scope, which the
/// caller opens around the call: the pool rooted the reference, and `args` is not scanned by the
/// collector, so without the root a collection while the call binds its frame frees the promise.
/// The call's scope ends only once the call returns, by which time the frame holds the promise.
pub unsafe fn detach_references(args: &[Arg]) -> R<()> {
    for arg in args {
        if is_reference(arg.value) {
            detach_reference(arg.value)?;
            crate::heap().root(arg.value);
        }
    }
    Ok(())
}

/// Makes `reference` the ordinary promise that stands for it ([`reference_promise`]), in place,
/// and gives its place in the pool to a fresh reference. Forwarding `...` hands the same
/// reference to several frames, so converting it in place is what keeps it one promise that
/// forces once for all of them.
pub unsafe fn detach_reference(reference: Sexp) -> R<()> {
    let [value, code, env, compiled] = reference_promise(reference)?;
    let state = interp();
    let used = state.references_used as usize;
    let Some(index) = state.references[..used]
        .iter()
        .position(|&pooled| pooled == reference)
    else {
        panic!("an argument reference outlived the call it was made for");
    };
    let heap = crate::heap();
    heap.set_car(reference, value);
    heap.set_cdr(reference, code);
    heap.set_tag(reference, env);
    heap.set_attrib(reference, compiled);
    reference.set_gp_bits(gp::ARGUMENT_REFERENCE, false);
    let target = values::integer_vector(&[0, 0, 0])?;
    heap.root(target);
    let fresh = heap.alloc_compiled_promise(unbound(), nil(), nil(), target)?;
    fresh.set_gp_bits(gp::ARGUMENT_REFERENCE, true);
    fresh.increment_refcount();
    target.increment_refcount();
    let state = interp();
    state.references[index] = fresh;
    state.reference_targets[index] = target;
    Ok(())
}

/// The fields (value, code, environment, compiled code) of the ordinary promise that stands for
/// an argument reference: a forced promise of its name once it is forced, and a promise of its
/// name in the frame it names before. [`detach_reference`] writes them into the reference
/// itself, so no promise is allocated only to be copied.
unsafe fn reference_promise(reference: Sexp) -> R<[Sexp; 4]> {
    let symbol = reference.cdr();
    if reference.car() != unbound() {
        return Ok([reference.car(), symbol, nil(), nil()]);
    }
    let (env, expression) = if let Some(env) = reference_env(reference) {
        let code = reference.attrib();
        (
            env,
            if code.kind() == SexpType::Bytecode {
                code
            } else {
                symbol
            },
        )
    } else {
        let (owner, _) = reference_target(reference);
        let expression = reference_code(reference).unwrap_or(symbol);
        let frame = &interp().frames[owner];
        if frame.env != nil() {
            (frame.env, expression)
        } else {
            (pending_env(owner)?, expression)
        }
    };
    // The reference becomes a promise over `env`, which may be a pending environment however
    // the reference was made.
    count_unforced(env);
    activity::count(Counter::PromisesCreated);
    Ok(if expression.kind() == SexpType::Bytecode {
        [unbound(), bytecode::source_of(expression), env, expression]
    } else {
        [unbound(), symbol, env, nil()]
    })
}

/// The pending environment of the slot frame at `owner`, made bare on first use: the value that
/// stands for the frame's environment until something reads it, which [`resolved_env`] then
/// makes real.
pub unsafe fn pending_env(owner: usize) -> R<Sexp> {
    let env = interp().frames[owner].pending;
    if env != nil() {
        return Ok(env);
    }
    let function = interp().frames[owner].function;
    let env = frame_environment(function, function.car())?;
    env.set_gp_bits(gp::PENDING_FRAME, true);
    interp().frames[owner].pending = env;
    Ok(env)
}

/// `value`, or the environment it stands for when it is a slot frame's pending one: what a
/// binding gets for a value that may be pending (a method's `.GenericCallEnv`).
pub unsafe fn resolved_value(value: Sexp) -> R<Sexp> {
    if value.kind() == SexpType::Environment {
        resolved_env(value)
    } else {
        Ok(value)
    }
}

/// The environment of an unforced `promise`, made first when it is a slot frame's pending one
/// or the frame an argument reference names. Anything that reads a promise's environment other
/// than to force it reads it here.
pub unsafe fn promise_env(promise: Sexp) -> R<Sexp> {
    if promise.has_gp(gp::ARGUMENT_REFERENCE)
        && promise.car() == unbound()
        && reference_env(promise).is_none()
    {
        return frame_env(
            reference_target(promise).0,
            Reify::Site("promise environment"),
        );
    }
    resolved_env(promise.tag())
}

/// `env`, made first when it is a slot frame's pending environment, which a promise's
/// environment may be (see [`promise_env`]).
pub unsafe fn resolved_env(env: Sexp) -> R<Sexp> {
    if !env.has_gp(gp::PENDING_FRAME) {
        return Ok(env);
    }
    frame_env(pending_owner(env), Reify::Site("resolved environment"))
}

/// Stores `value` as the value of the unforced `promise`, as forcing it to that value does.
pub unsafe fn settle_promise(promise: Sexp, mut value: Sexp) -> R<Sexp> {
    if promise.has_gp(gp::CHECKED_PROMISE) {
        value = crate::annotations::check_forced(promise, value)?;
    }
    crate::heap().settle_promise(promise, value);
    Ok(value)
}

/// The value a promise or plain value stands for.
pub unsafe fn value_of(object: Sexp) -> R<Sexp> {
    if object.kind() == SexpType::Promise {
        force_promise(object)
    } else {
        Ok(object)
    }
}

unsafe fn eval_call(call: Sexp, env: Sexp) -> R<Sexp> {
    crate::unwind::poll_interrupt()?;
    let head = call.car();
    let function = if head.kind() == SexpType::Symbol {
        find_called_fun(head, env, call)?
    } else {
        let value = eval(head, env)?;
        if !values::is_function(value) {
            return Err(error_in(call, "attempt to apply non-function"));
        }
        value
    };
    crate::heap().root(function);
    apply_call(call, function, call.cdr(), env)
}

/// Calls `function` for `call` with the unevaluated argument pairlist `arguments`.
pub unsafe fn apply_call(call: Sexp, function: Sexp, arguments: Sexp, env: Sexp) -> R<Sexp> {
    match function.kind() {
        SexpType::Special => {
            let definition = builtin_def(function)?;
            if definition.is_dispatch_or_eval() {
                let mut args = Args::new();
                return match dispatch::dispatch_or_eval(
                    definition, call, function, arguments, env, &mut args,
                )? {
                    Some(value) => Ok(value),
                    None => call_builtin(definition, call, function, &args, env),
                };
            }
            call_builtin(
                definition,
                call,
                function,
                &unevaluated_args(arguments),
                env,
            )
        }
        SexpType::Builtin => {
            let definition = builtin_def(function)?;
            let args = evaluate_args(arguments, env, definition.keep_missing, call)?;
            if definition.generic != Generic::None
                && let Some(value) =
                    dispatch::try_builtin_dispatch(definition, call, function, &args, env)?
            {
                return Ok(value);
            }
            let reusable = if definition.reuses_first_argument {
                temporary_argument(arguments, &args, env)?
            } else {
                None
            };
            call_builtin_reusing(definition, call, function, &args, env, reusable)
        }
        SexpType::Closure => {
            if function.has_gp(gp::NATIVE_ENTRY)
                && let Some(value) = crate::base_fast_paths::native_entry_by_value(
                    function,
                    arguments,
                    false,
                    |symbol| crate::effects::eager_argument(symbol, env),
                )?
            {
                return Ok(value);
            }
            let mut args = Args::new();
            // Matching here replaces the matching the generic call would do, so a call the
            // plan covers costs no more, and its arguments become references, not promises.
            if arguments == call.cdr()
                && let Some((shape, sources)) = direct_sources(function, call)?
            {
                let references = interp().references_used;
                closure_args_into::<EVALUATOR_REFERENCES>(arguments, env, &mut args)?;
                let result = if sources[..] == [MATCH_PER_CALL] {
                    call_matching(call, function, &args, env)
                } else {
                    call_with_plan(call, function, shape, &sources, &args, env)
                };
                if interp().references_used != references {
                    release_references(references);
                }
                return result;
            }
            promise_args_into(arguments, env, &mut args)?;
            apply_closure_owning(call, function, &args, env, None)
        }
        _ => Err(error_in(call, "attempt to apply non-function")),
    }
}

/// The argument of a builtin call that only the call holds, which the builtin may then write
/// its result into, as compiled code does with `Op::Arithmetic`'s reuse bits: the fresh result
/// of a binary element-wise operator call (in `x * 2 + 1`, the value of `x * 2`) that no
/// counted reference holds. The operator must be the base builtin, since a closure bound to
/// `*` may return a value something uncounted holds, and it must take two arguments, since
/// unary `+` returns its operand. Of two candidates the wider type wins, as the result's type
/// is the wider operand's.
unsafe fn temporary_argument(arguments: Sexp, args: &[Arg], env: Sexp) -> R<Option<Sexp>> {
    let width = |value: Sexp| match value.kind() {
        SexpType::Complex => 2,
        SexpType::Real => 1,
        _ => 0,
    };
    let mut best: Option<Sexp> = None;
    let mut list = arguments;
    for arg in args {
        if list.kind() != SexpType::Pairlist {
            return Ok(None);
        }
        let expression = list.car();
        list = list.cdr();
        if expression == symbols().dots {
            return Ok(None);
        }
        let value = arg.value;
        if value.refcount() != 0
            || !matches!(
                value.kind(),
                SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex
            )
            || value.length() <= 1
            || expression.kind() != SexpType::Language
            || expression.car().kind() != SexpType::Symbol
            || expression.cdr().kind() != SexpType::Pairlist
            || expression.cdr().cdr().kind() != SexpType::Pairlist
            || expression.cdr().cdr().cdr() != nil()
            || best.is_some_and(|best| width(best) >= width(value))
        {
            continue;
        }
        let operator = find_fun(expression.car(), env)?;
        if operator.kind() == SexpType::Builtin && builtin_def(operator)?.reuses_first_argument {
            best = Some(value);
        }
    }
    if list != nil() || args.iter().filter(|arg| Some(arg.value) == best).count() > 1 {
        return Ok(None);
    }
    Ok(best)
}

/// A pairlist's items as arguments, unevaluated, as a special receives them.
pub unsafe fn unevaluated_args(mut list: Sexp) -> Args {
    let mut args = Args::new();
    while matches!(
        list.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        args.push(Arg {
            tag: list.tag(),
            value: list.car(),
        });
        list = list.cdr();
    }
    args
}

/// `definition`'s position in the builtin table, which [`interp::Interp::builtin`] reads by.
pub fn builtin_index(definition: &BuiltinDef) -> usize {
    let table = &interp().builtins;
    (definition as *const BuiltinDef as usize).wrapping_sub(table.as_ptr() as usize)
        / size_of::<BuiltinDef>()
}

#[inline]
pub unsafe fn builtin_def(function: Sexp) -> R<&'static BuiltinDef> {
    interp()
        .builtin(function.primitive_index())
        .ok_or_else(|| error("invalid primitive"))
}

/// Calls a builtin in its own handle scope, sets visibility, and signals an error it raised.
pub unsafe fn call_builtin(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Sexp> {
    call_builtin_reusing(definition, call, op, args, env, None)
}

/// The builtin's [`BuiltinDef::direct`] result for `args`, in formal order, or `None` for the
/// full call. The entry is skipped while a profile or activity trace is being taken, so that
/// samples and counts name the builtin as the full call does.
#[inline]
pub unsafe fn call_direct(definition: &BuiltinDef, call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let Some(direct) = definition.direct else {
        return Ok(None);
    };
    if activity::enabled() || PROFILE_TICK.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let value = scope(|| direct(call, args))
        .and_then(|value| crate::unwind::raise_parked().map(|()| value))
        .map_err(|unwind| builtin_failure(unbound(), unwind))?;
    if value.is_some() {
        match definition.visibility {
            Visibility::On => interp().visible = true,
            Visibility::Off => interp().visible = false,
            Visibility::Keep => {}
        }
    }
    Ok(value)
}

/// [`call_builtin`], offering the builtin `reusable` to update in place (see [`Call::reusable`]).
//
// The common call (no profile being taken, the arity right, no names to check, no replacement
// target to copy) runs straight through; anything else takes the out-of-line path, so this
// frame stays small enough that entry and exit cost a handful of instructions.
#[inline]
pub(crate) unsafe fn call_builtin_reusing(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
) -> R<Sexp> {
    let checks = definition.checks;
    if activity::enabled()
        || PROFILE_TICK.load(Ordering::Relaxed)
        || (checks.arity && args.len() != definition.arity as usize)
        || (checks.names && args.iter().any(|arg| arg.tag != nil()))
        || (checks.replacement
            && args
                .first()
                .is_some_and(|target| target.value.refcount() >= 1))
    {
        return call_builtin_checked(definition, call, op, args, env, reusable, unbound());
    }
    run_builtin(definition, call, op, args, env, reusable, unbound())
}

/// Runs the `.Internal` `definition` that compiled code inlined from its base wrapper, blaming
/// an error that names no call on the caller's `call`, as GNU R's compiler does: no wrapper
/// closure runs, so the frame's own call would name the function that called the wrapper.
pub(crate) unsafe fn call_inlined_internal(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Sexp> {
    // The wrapper's frame is gone, so its call is what the internal's errors and warnings
    // report, as they would from inside the closure.
    let state = interp();
    let outer =
        state
            .assignment_call
            .replace((crate::heap().known().unbound, call, state.frames.len()));
    let result = call_builtin_checked(definition, call, op, args, env, None, call);
    interp().assignment_call = outer;
    result
}

/// The builtin call [`call_builtin_reusing`] hands off: it publishes the activity, raises the
/// arity and name errors, takes a profile sample, and copies a shared replacement target.
#[inline(never)]
unsafe fn call_builtin_checked(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
    blame: Sexp,
) -> R<Sexp> {
    let _activity = if activity::enabled() {
        let index = builtin_index(definition);
        activity::count(Counter::BuiltinCalls);
        activity::count_builtin(index);
        activity::enter(Activity::Builtin, index as u32)
    } else {
        activity::enter(Activity::Builtin, 0)
    };
    let checks = definition.checks;
    if checks.arity && args.len() != definition.arity as usize {
        let name = if matches!(definition.kind, Kind::Builtin) {
            format!("'{}'", definition.name)
        } else {
            format!(".Internal({})", definition.name)
        };
        return Err(error_in(
            call,
            format!(
                "{} argument{} passed to {name} which requires {}",
                args.len(),
                if args.len() == 1 { "" } else { "s" },
                definition.arity
            ),
        ));
    }
    if checks.names && args.iter().any(|arg| arg.tag != nil()) {
        check_argument_names(definition, call, args)?;
    }
    if PROFILE_TICK.load(Ordering::Relaxed) {
        record_profile_sample(Some(definition.name))?;
    }
    // A replacement primitive modifies its target in place when the target is referenced
    // only by the variable a complex assignment rebinds. Called directly, as in
    // `` `class<-`(x, "foo") ``, the same single reference is the caller's own binding, which
    // must keep its value, so the target is copied first, as GNU R does.
    let copied_target;
    let args = match args.first() {
        Some(target)
            if checks.replacement && target.value.refcount() >= 1 && !is_assignment(call) =>
        {
            let mut copy = args.to_vec();
            copy[0].value = values::shallow_duplicate(target.value)?;
            copied_target = copy;
            copied_target.as_slice()
        }
        _ => args,
    };
    run_builtin(definition, call, op, args, env, reusable, blame)
}

/// Runs a builtin whose call passed the checks: refuses a native value it cannot take, runs it
/// in its own handle scope, sets visibility, and signals an error it raised, blaming `blame`
/// when the error names no call (`unbound()` leaves that to the frame's call).
#[inline(always)]
unsafe fn run_builtin(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
    blame: Sexp,
) -> R<Sexp> {
    let checks = definition.checks;
    let context = Call {
        call,
        op,
        args,
        rho: env,
        frame: if env == nil() {
            interp().slot_owner
        } else {
            crate::NO_SLOTS
        },
        reusable,
    };
    if checks.native != NativeArgs::Accepted
        && args
            .iter()
            .any(|arg| matches!(arg.value.kind(), SexpType::Record | SexpType::I64))
    {
        let skip_numbers = match checks.native {
            NativeArgs::Accepted | NativeArgs::RefusedExceptNumbers => true,
            NativeArgs::Refused => false,
            NativeArgs::RefusedExceptArrayIndexes => {
                args.first().is_some_and(|target| target.value.is_array())
            }
        };
        crate::records::check_native_args(call, args, skip_numbers)?;
    }
    // An ALTREP method's error, parked by the element accessor the builtin went through, is
    // raised before the builtin's value goes anywhere, so `tryCatch` around the call sees it
    // and no code acts on the placeholder the read returned.
    match scope(|| (definition.function)(&context)) {
        Ok(value) => {
            if let Err(unwind) = crate::unwind::raise_parked() {
                return Err(builtin_failure(blame, unwind));
            }
            match definition.visibility {
                Visibility::On => interp().visible = true,
                Visibility::Off => interp().visible = false,
                Visibility::Keep => {}
            }
            Ok(value)
        }
        Err(unwind) => Err(builtin_failure(blame, unwind)),
    }
}

/// The unwind a failing builtin raises: an error it returned is signalled, naming the
/// primitive's call as `errorcall` does.
#[cold]
#[inline(never)]
unsafe fn builtin_failure(blame: Sexp, unwind: Unwind) -> Unwind {
    match unwind {
        Unwind::Error(mut failure) => {
            if failure.call == unbound() {
                failure.call = blame;
            }
            conditions::signal_error(failure)
        }
        unwind => unwind,
    }
}

/// Whether `call` is a complex assignment such as `x[1] <- 5`, or the replacement call one
/// builds, whose replacement primitives may modify the target in place.
pub unsafe fn is_assignment(call: Sexp) -> bool {
    if interp()
        .assignment_call
        .is_some_and(|(setter, _, _)| setter == call)
    {
        return true;
    }
    call.kind() == SexpType::Language && {
        let head = call.car();
        let symbols = symbols();
        head == symbols.assign || head == symbols.assign2 || head == symbols.assign_eq
    }
}

/// GNU R rejects a named argument to a primitive that partially matches none of the formals
/// `.ArgsEnv` or `.GenericArgsEnv` record for it.
unsafe fn check_argument_names(definition: &BuiltinDef, call: Sexp, args: &[Arg]) -> R<()> {
    // `trunc(x, ...)` passes its dots to methods, but GNU R still requires the first argument,
    // if named, to be `x`.
    if definition.name == "trunc"
        && let Some(first) = args.first()
        && first.tag != nil()
        && !"x".starts_with(values::symbol_name(first.tag).as_ref())
    {
        let name = values::symbol_name(first.tag);
        return Err(error_in(
            call,
            format!("supplied argument name '{name}' does not match 'x'"),
        ));
    }
    // GNU R's operators ignore argument names, and a wrong argument count is reported before a
    // wrong name.
    const OPERATORS: &[&str] = &[
        "+", "-", "*", "/", "^", "%%", "%/%", "%*%", "==", "!=", "<", ">", "<=", ">=", "&", "|",
        "!",
    ];
    // `round` and `signif` count and match their own arguments, as GNU R's do.
    if OPERATORS.contains(&definition.name)
        || matches!(definition.name, "round" | "signif")
        || (definition.arity >= 0 && args.len() != definition.arity as usize)
    {
        return Ok(());
    }
    let index = builtin_index(definition);
    let formals = match interp().primitive_formals.get(index) {
        Some(Some(formals)) => formals,
        _ => {
            let Some(formals) = recorded_formals(definition.name)? else {
                return Ok(());
            };
            let table = &mut interp().primitive_formals;
            if table.len() <= index {
                table.resize(interp().builtins.len().max(index + 1), None);
            }
            &*table[index].insert(formals)
        }
    };
    let Some(formals) = formals else {
        return Ok(());
    };
    if formals.iter().any(|formal| formal == "...") {
        return Ok(());
    }
    for arg in args.iter().filter(|arg| arg.tag != nil()) {
        let name = values::symbol_name(arg.tag);
        if formals
            .iter()
            .any(|formal| formal.starts_with(name.as_ref()))
        {
            continue;
        }
        let message = match &formals[..] {
            [] => format!(
                "{} argument{} passed to '{}' which requires 0",
                args.len(),
                if args.len() == 1 { "" } else { "s" },
                definition.name
            ),
            [formal] => format!("supplied argument name '{name}' does not match '{formal}'"),
            _ => format!(
                "unused argument ({name} = {})",
                crate::deparse::deparse_one_line(arg.value)
            ),
        };
        return Err(error_in(call, message));
    }
    Ok(())
}

/// The formals of `name` in `.ArgsEnv` or `.GenericArgsEnv`, or `None` while base R has not
/// yet created both environments.
unsafe fn recorded_formals(name: &str) -> R<Option<Option<Box<[String]>>>> {
    let mut tables = Vec::new();
    for table in [".ArgsEnv", ".GenericArgsEnv"] {
        let mut env = find_var_in_frame(values::install(table)?, values::base_namespace())?;
        // Lazy loading these bindings calls builtins itself, so a check inside that load
        // leaves the names unchecked.
        if env.kind() == SexpType::Promise {
            if env.has_gp(PROMISE_SEEN) {
                return Ok(None);
            }
            env = force_promise(env)?;
        }
        if env.kind() != SexpType::Environment {
            return Ok(None);
        }
        tables.push(env);
    }
    let symbol = values::install(name)?;
    for env in tables {
        let mut function = find_var_in_frame(symbol, env)?;
        if function.kind() == SexpType::Promise {
            if function.has_gp(PROMISE_SEEN) {
                return Ok(None);
            }
            function = force_promise(function)?;
        }
        if function.kind() != SexpType::Closure {
            continue;
        }
        let mut formals = Vec::new();
        let mut formal = function.car();
        while formal != nil() {
            let formal_name = values::symbol_name(formal.tag()).into_owned();
            if formal_name == "..." {
                return Ok(Some(None));
            }
            formals.push(formal_name);
            formal = formal.cdr();
        }
        return Ok(Some(Some(formals.into_boxed_slice())));
    }
    Ok(Some(None))
}

/// Evaluates a builtin's arguments, expanding `...`.
pub unsafe fn evaluate_args(arguments: Sexp, env: Sexp, keep_missing: bool, call: Sexp) -> R<Args> {
    let mut args = Args::new();
    evaluate_args_into(arguments, env, keep_missing, call, &mut args)?;
    Ok(args)
}

/// [`evaluate_args`], appending to `args`. The values already there count as referenced while
/// the rest evaluate, like the arguments evaluated before them.
pub unsafe fn evaluate_args_into(
    arguments: Sexp,
    env: Sexp,
    keep_missing: bool,
    call: Sexp,
    args: &mut Args,
) -> R<()> {
    for arg in args.iter() {
        arg.value.increment_refcount();
    }
    let result = collect_evaluated_args(arguments, env, keep_missing, call, args);
    for arg in args.iter() {
        arg.value.decrement_refcount();
    }
    result
}

/// Evaluates arguments into `args`, counting each value as referenced until the caller is done:
/// in `c(x, {x[1] <- 9; 0})` the later argument must see `x` as shared and copy it, as it does
/// in GNU R, where the argument list holds a reference.
unsafe fn collect_evaluated_args(
    mut arguments: Sexp,
    env: Sexp,
    keep_missing: bool,
    call: Sexp,
    args: &mut Args,
) -> R<()> {
    let dots = symbols().dots;
    let missing = missing_arg();
    while arguments.kind() == SexpType::Pairlist {
        let expression = arguments.car();
        if expression == dots {
            activity::count(Counter::DotsCalls);
            let value = find_var(dots, env)?;
            if value.kind() == SexpType::Dots {
                let mut cell = value;
                while cell.kind() == SexpType::Dots || cell.kind() == SexpType::Pairlist {
                    let item = cell.car();
                    let evaluated = if item == missing {
                        if !keep_missing {
                            return Err(error_in(
                                call,
                                format!("argument {} is empty", args.len() + 1),
                            ));
                        }
                        missing
                    } else {
                        value_of(item)?
                    };
                    crate::heap().root(evaluated);
                    args.push(Arg {
                        tag: cell.tag(),
                        value: evaluated,
                    });
                    counted_push(args);
                    cell = cell.cdr();
                }
            } else if value != missing && value != unbound() && value != nil() {
                return Err(error("'...' used in an incorrect context"));
            }
        } else if expression == missing {
            if !keep_missing {
                return Err(error_in(
                    call,
                    format!("argument {} is empty", args.len() + 1),
                ));
            }
            args.push(Arg {
                tag: arguments.tag(),
                value: missing,
            });
            counted_push(args);
        } else {
            let value = if keep_missing && argument_is_missing(expression, env)? {
                missing
            } else {
                eval(expression, env)?
            };
            // A variable's value comes back unrooted, held only by its binding, and a later
            // argument can rebind it: in `c(x, {x <- 5; 3})` the old `x` is then held by `args`
            // alone, which the collector cannot see.
            crate::heap().root(value);
            args.push(Arg {
                tag: arguments.tag(),
                value,
            });
            counted_push(args);
        }
        arguments = arguments.cdr();
    }
    Ok(())
}

/// Counts the newest evaluated argument as referenced.
unsafe fn counted_push(args: &[Arg]) {
    if let Some(arg) = args.last() {
        arg.value.increment_refcount();
    }
}

/// Wraps a closure's arguments in promises, expanding `...` into the promises it holds.
pub unsafe fn promise_args(arguments: Sexp, env: Sexp) -> R<Args> {
    let mut args = Args::new();
    promise_args_into(arguments, env, &mut args)?;
    Ok(args)
}

/// [`promise_args`], appending to `args`, which a caller keeps on its own stack instead of
/// moving a returned argument vector through every frame.
pub unsafe fn promise_args_into(arguments: Sexp, env: Sexp, args: &mut Args) -> R<()> {
    closure_args_into::<PROMISES>(arguments, env, args)
}

/// The arguments of a closure call that compiled code makes. GNU R's byte code passes a
/// constant argument as its value instead of a promise (`PUSHCONSTARG`), and native code can
/// tell: rlang's `env_dot_type()` reports such a `...` element as a value, not delayed.
pub unsafe fn compiled_args_into(arguments: Sexp, env: Sexp, args: &mut Args) -> R<()> {
    closure_args_into::<COMPILED>(arguments, env, args)
}

/// [`compiled_args_into`] for a callee that takes its arguments straight into slots
/// ([`call_with_plan`]): a variable is passed as an argument reference to it in `env`, which
/// the calling instruction releases when the call returns.
pub unsafe fn referenced_args_into(arguments: Sexp, env: Sexp, args: &mut Args) -> R<()> {
    closure_args_into::<REFERENCES>(arguments, env, args)
}

/// [`compiled_args_into`] for a call of an effect-free closure (see [`crate::effects`]) whose
/// other arguments cannot assign a variable, made while no calling handler is established: a
/// variable argument is bound to its value, or to the caller's own promise for it, instead of a
/// new promise.
pub unsafe fn eager_args_into(arguments: Sexp, env: Sexp, args: &mut Args) -> R<()> {
    closure_args_into::<EAGER>(arguments, env, args)
}

/// How [`closure_args_into`] passes an argument: every one as a promise, as GNU R's AST
/// evaluator does.
const PROMISES: u8 = 0;
/// A constant as its own value, as GNU R's byte code does.
const COMPILED: u8 = 1;
/// A constant or a variable as its value.
const EAGER: u8 = 2;
/// A constant as its own value and a variable as an argument reference ([`env_reference`]).
const REFERENCES: u8 = 3;
/// [`REFERENCES`] for the AST evaluator, which also passes a constant as a reference, so that
/// the callee sees a promise for it as it does from GNU R's AST evaluator.
const EVALUATOR_REFERENCES: u8 = 4;

unsafe fn closure_args_into<const MODE: u8>(
    mut arguments: Sexp,
    env: Sexp,
    args: &mut Args,
) -> R<()> {
    let dots = symbols().dots;
    while arguments.kind() == SexpType::Pairlist {
        let expression = arguments.car();
        if expression == dots {
            activity::count(Counter::DotsCalls);
            let value = find_var(dots, env)?;
            if value.kind() == SexpType::Dots {
                let mut cell = value;
                while cell.kind() == SexpType::Dots || cell.kind() == SexpType::Pairlist {
                    args.push(Arg {
                        tag: cell.tag(),
                        value: cell.car(),
                    });
                    cell = cell.cdr();
                }
            } else if value != missing_arg() && value != unbound() && value != nil() {
                return Err(error("'...' used in an incorrect context"));
            }
        } else {
            // A constant gets a promise too, as in GNU R's AST evaluator: native code such as
            // lazyeval reads a promise's environment to tell whether it was forced.
            let constant = !matches!(
                expression.kind(),
                SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode
            );
            let eager = if MODE == EAGER
                && expression.kind() == SexpType::Symbol
                && expression != missing_arg()
            {
                crate::effects::eager_argument(expression, env)
            } else {
                None
            };
            let value = if let Some(value) = eager {
                value
            } else if expression == missing_arg()
                || (MODE != PROMISES && MODE != EVALUATOR_REFERENCES && constant)
            {
                expression
            } else if (MODE == REFERENCES || MODE == EVALUATOR_REFERENCES)
                && match expression.kind() {
                    SexpType::Symbol => dot_dot_index(expression).is_none(),
                    SexpType::Promise => false,
                    _ => true,
                }
            {
                if expression.kind() == SexpType::Bytecode {
                    env_code_reference(env, expression)?
                } else {
                    env_reference(env, expression)?
                }
            } else if expression.kind() == SexpType::Bytecode {
                compiled_promise(expression, env)?
            } else {
                make_promise(expression, env)?
            };
            args.push(Arg {
                tag: arguments.tag(),
                value,
            });
        }
        arguments = arguments.cdr();
    }
    Ok(())
}

/// Calls any function with arguments that are values or promises, as `do.call` and the apply
/// family do. Builtins receive the forced values.
pub unsafe fn apply(call: Sexp, function: Sexp, args: &[Arg], env: Sexp) -> R<Sexp> {
    let call = if call == nil() {
        build_call(function, args)?
    } else {
        call
    };
    match function.kind() {
        SexpType::Closure => apply_closure(call, function, args, env, None),
        SexpType::Builtin | SexpType::Special => {
            let definition = builtin_def(function)?;
            if function.kind() == SexpType::Special && !definition.is_dispatch_or_eval() {
                // A special needs expressions; quote the values so they evaluate to themselves.
                let mut quoted = Vec::with_capacity(args.len());
                for arg in args {
                    quoted.push(Arg {
                        tag: arg.tag,
                        value: quote_value(value_of(arg.value)?)?,
                    });
                }
                return call_builtin(definition, call, function, &quoted, env);
            }
            let mut forced = Vec::with_capacity(args.len());
            for arg in args {
                let value = if arg.value == missing_arg() {
                    arg.value
                } else {
                    value_of(arg.value)?
                };
                forced.push(Arg {
                    tag: arg.tag,
                    value,
                });
            }
            if definition.generic != Generic::None
                && let Some(value) =
                    dispatch::try_builtin_dispatch(definition, call, function, &forced, env)?
            {
                return Ok(value);
            }
            call_builtin(definition, call, function, &forced, env)
        }
        _ => Err(error_in(call, "attempt to apply non-function")),
    }
}

/// An expression that evaluates to `value`: the value itself unless evaluating it would do
/// something else.
pub unsafe fn quote_value(value: Sexp) -> R<Sexp> {
    match value.kind() {
        SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Dots => {
            values::call(symbols().quote, &[value])
        }
        _ => Ok(value),
    }
}

/// A call `function(args...)` for error messages and `sys.call`, with promises replaced by
/// their code.
pub unsafe fn build_call(function: Sexp, args: &[Arg]) -> R<Sexp> {
    let mut items = vec![(nil(), function)];
    for arg in args {
        let value = if arg.value.kind() == SexpType::Promise {
            arg.value.cdr()
        } else {
            arg.value
        };
        items.push((arg.tag, value));
    }
    values::pairlist(&items, SexpType::Language)
}

/// Applies a closure: matches the arguments to the formals, binds them in a fresh environment,
/// runs the body, and handles `return` and `on.exit`. `supplied` are extra bindings for a method
/// frame (`.Generic` and friends).
pub unsafe fn apply_closure(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Sexp> {
    if function.has_gp(gp::NATIVE_ENTRY)
        && supplied.is_none()
        && let Some(value) = crate::base_fast_paths::native_entry(function, args)?
    {
        return Ok(value);
    }
    if crate::annotations::is_typed(function)
        && let Some(value) = try_native_typed(call, function, args, sys_parent, supplied)?
    {
        return value;
    }
    if let Some(frame) = bind_slots(call, function, args)? {
        let method_vars = supplied.unwrap_or(&[]);
        return run_frame(
            call,
            function,
            frame,
            sys_parent,
            args,
            Entry::Unchecked,
            method_vars,
        );
    }
    let env = bind_arguments(call, function, args, supplied)?;
    run_closure(call, function, env, sys_parent, args, Entry::Unchecked)
}

/// Runs a call of the typed closure `function` natively when the native tier takes it, and
/// otherwise reports why it runs in the interpreter.
unsafe fn try_native_typed(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Option<R<Sexp>>> {
    if supplied.is_some() {
        crate::annotations::log_interpreted_because(
            call,
            "it is a method that dispatch called, which the native tier does not take",
        );
        return Ok(None);
    }
    force_typed_arguments(args)?;
    let Some(tier) = bytecode::tier() else {
        crate::annotations::log_interpreted_because(call, "no native tier is loaded");
        return Ok(None);
    };
    let value = (tier.call)(&bytecode::TypedCall {
        call,
        function,
        args,
        sys_parent,
    });
    if value.is_none() {
        crate::annotations::log_interpreted(call);
    }
    Ok(value)
}

/// Forces, left to right, each argument promise that typed code wrote: a call from one typed
/// function to another evaluates its arguments before the callee runs, as compiled typed code
/// does, so the tiers agree on a typed program. An argument written in R code, including one a
/// typed function receives through `...`, stays lazy.
unsafe fn force_typed_arguments(args: &[Arg]) -> R<()> {
    for arg in args {
        let promise = arg.value;
        if promise.kind() == SexpType::Promise
            && promise.car() == unbound()
            && written_in_typed_code(promise)
        {
            force_promise(promise)?;
        }
    }
    Ok(())
}

/// Whether `promise`'s code is in the body of a typed function: its environment is the frame of
/// a live call of one. A typed function never runs on slots, so a promise over a slot frame is
/// R code's.
unsafe fn written_in_typed_code(promise: Sexp) -> bool {
    if promise.has_gp(gp::ARGUMENT_REFERENCE) {
        return false;
    }
    let env = promise.tag();
    if env.has_gp(gp::PENDING_FRAME) {
        return false;
    }
    interp()
        .frames
        .iter()
        .rev()
        .find(|frame| frame.env == env)
        .is_some_and(|frame| {
            // `eval` and other builtins push frames too, whose function has no formals to read.
            frame.function.kind() == SexpType::Closure
                && crate::annotations::is_typed(frame.function)
        })
}

/// [`apply_closure`] for promises the caller built for this call alone and never reads again,
/// which the call releases once it returns, as GNU R's `unpromiseArgs` does: a promise only the
/// argument list still holds gives up its value, so an argument's large value can be freed
/// before a collection finds it.
pub unsafe fn apply_closure_owning(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Sexp> {
    if function.has_gp(gp::NATIVE_ENTRY)
        && supplied.is_none()
        && let Some(value) = crate::base_fast_paths::native_entry(function, args)?
    {
        return Ok(value);
    }
    if crate::annotations::is_typed(function)
        && let Some(value) = try_native_typed(call, function, args, sys_parent, supplied)?
    {
        return value;
    }
    if let Some(frame) = bind_slots(call, function, args)? {
        let method_vars = supplied.unwrap_or(&[]);
        return run_frame(
            call,
            function,
            frame,
            sys_parent,
            args,
            Entry::Unchecked,
            method_vars,
        );
    }
    let env = bind_arguments(call, function, args, supplied)?;
    run_closure(call, function, env, sys_parent, args, Entry::Unchecked)
}

/// A fresh environment for a call of the closure `function`, with `args` matched to its formals
/// and bound, and rooted.
pub unsafe fn bind_arguments(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Sexp> {
    let _activity = activity::enter(Activity::CallSetup, 0);
    let formals = function.car();
    // Most calls pass every argument by position to a function without `...`, which needs no
    // matching table: argument `i` binds formal `i`, and the formals past the last argument are
    // missing.
    let actuals = if supplied.is_none() && positional(formals, args) {
        None
    } else {
        activity::count(Counter::FormalsMatchedByName);
        Some(match_args_cached(function, formals, args, call)?)
    };
    let env = frame_environment(function, formals)?;
    let mut bindings: SmallVec<[Formal; 12]> = SmallVec::new();
    let mut formal = formals;
    let mut index = 0;
    while formal.kind() == SexpType::Pairlist {
        let actual = match &actuals {
            Some(actuals) => actuals.get(index).copied().unwrap_or(missing_arg()),
            None => args.get(index).map_or(missing_arg(), |arg| arg.value),
        };
        bindings.push(formal_binding(env, formal, actual)?);
        formal = formal.cdr();
        index += 1;
    }
    let heap = crate::heap();
    heap.define_formals_uncollected(env, &bindings)?;
    heap.mark_frame_formals(env);
    if let Some(supplied) = supplied {
        for &(symbol, value) in supplied {
            if heap.frame_cell(env, symbol).is_none() {
                heap.define(env, symbol, resolved_value(value)?)?;
            }
        }
    }
    Ok(env)
}

/// The frame a closure call runs in: an environment, or a slot frame
/// (`docs/src/content/docs/development/design/slot-frames.md`) whose header register is
/// `header`, with `missing` its formals' missing bits.
#[derive(Clone, Copy)]
pub enum CallFrame {
    Env(Sexp),
    /// A slot frame at `header` whose left-out formals are `missing`, of which `promised` have
    /// a default that is code, not a literal ([`CallShape`]).
    Slots {
        header: u32,
        missing: u64,
        promised: u64,
    },
}

/// Binds `args` to `function`'s formals in a slot frame pushed onto the register file, or
/// returns `None` when the call needs an environment from the start: the body is not compiled
/// with a slot table, a debugger, a move or a type signature watches the frame, or a missing
/// formal's default is neither a literal, a symbol, nor code compiled with its own slot table.
pub unsafe fn bind_slots(call: Sexp, function: Sexp, args: &[Arg]) -> R<Option<CallFrame>> {
    let state = interp();
    if state.debugger.is_some()
        || moves::tracking()
        || function == state.namespace_loader
        || function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)
        || crate::annotations::is_typed(function)
    {
        return Ok(None);
    }
    let body = bytecode::compile_before_call(function)?;
    if body.kind() != SexpType::Bytecode {
        return Ok(None);
    }
    let constants = body.cdr();
    let table = constants.element(bytecode::SLOTS);
    if table == nil() {
        return Ok(None);
    }
    let _activity = activity::enter(Activity::CallSetup, 0);
    let names = table.element(1);
    let count = values::length(names);
    let formals = function.car();
    let actuals = if positional(formals, args) {
        None
    } else {
        activity::count(Counter::FormalsMatchedByName);
        Some(match_args_cached(function, formals, args, call)?)
    };
    let missing_value = missing_arg();
    let mut bound: SmallVec<[Sexp; 12]> = SmallVec::new();
    let mut missing = 0u64;
    let mut promised = 0u64;
    let mut defaults = None;
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        let index = bound.len();
        // A closure whose formals were replaced keeps the body's code, whose table names the
        // formals it was compiled with.
        if index >= count || names.element(index) != formal.tag() {
            return Ok(None);
        }
        let actual = match &actuals {
            Some(actuals) => actuals.get(index).copied().unwrap_or(missing_value),
            None => args.get(index).map_or(missing_value, |arg| arg.value),
        };
        let default = formal.car();
        if actual == missing_value && default != missing_value {
            if !slot_default_fits(function, default, index, &mut defaults)? {
                return Ok(None);
            }
            missing |= 1 << index;
            if !(default.kind().is_atomic() || default == nil()) {
                promised |= 1 << index;
            }
            bound.push(default);
        } else {
            bound.push(actual);
        }
        formal = formal.cdr();
    }
    let header = state.bytecode.registers.len();
    // The body's own registers follow the slots, so one reservation covers the whole window.
    let register_count = values::ints(constants.element(bytecode::REGISTERS))[0] as usize;
    bytecode::reserve_registers(header + 1 + count + register_count)?;
    let registers = bytecode::register_address(header);
    for (index, &value) in bound.iter().enumerate() {
        value.increment_refcount();
        *registers.add(1 + index) = value;
    }
    for index in bound.len()..count {
        *registers.add(1 + index) = unbound();
    }
    Ok(Some(CallFrame::Slots {
        header: header as u32,
        missing,
        promised,
    }))
}

/// The source [`direct_sources`] gives a `...` formal, whose arguments follow the formals'.
pub const DOTS_SOURCE: i32 = -2;

/// The plan [`direct_sources`] gives a call that passes `...`: which argument fills which slot
/// depends on what `...` holds at each call, so [`matched_sources`] matches the call's
/// arguments once they are collected.
pub const MATCH_PER_CALL: i32 = -3;

/// The source of a formal the site leaves out whose default is a literal, which goes in the
/// slot as it is.
pub const LITERAL_DEFAULT: i32 = -4;

/// The source of a formal the site leaves out whose default is code, which runs as a promise
/// over the slots.
pub const PROMISED_DEFAULT: i32 = -5;

/// The direct-call plan of the call `call` for closures with `function`'s formals and body:
/// for each formal, the position of the argument that fills its slot, [`DOTS_SOURCE`] for
/// `...`, or when the site supplies none, [`LITERAL_DEFAULT`] or [`PROMISED_DEFAULT`] for a
/// formal with a default and -1 for one without, followed by the positions of the arguments
/// `...` collects, in order. A call that passes `...` gets the one entry [`MATCH_PER_CALL`].
/// `None` when such calls need [`bind_slots`] or a heap frame: the body has no slot table that
/// names the formals, the call passes `..N`, its arguments do not match, or a formal the site
/// leaves out has a default whose promise would capture the frame.
pub unsafe fn direct_sources(function: Sexp, call: Sexp) -> R<Option<CallPlan>> {
    let Some((shape, dots_index)) = plannable(function) else {
        return Ok(None);
    };
    let mut args = Args::new();
    let mut forwards_dots = false;
    let mut cell = call.cdr();
    while cell.kind() == SexpType::Pairlist {
        let expression = cell.car();
        if expression.kind() == SexpType::Symbol {
            if expression == symbols().dots {
                forwards_dots = true;
            } else if dot_dot_index(expression).is_some() {
                return Ok(None);
            }
        }
        args.push(Arg {
            tag: cell.tag(),
            value: expression,
        });
        cell = cell.cdr();
    }
    if forwards_dots {
        return Ok(Some((shape, smallvec![MATCH_PER_CALL])));
    }
    plan_sources(function, shape, dots_index, &args, call)
}

/// The plan of a call whose site plans per call ([`MATCH_PER_CALL`]), from `args`, the
/// arguments it collected with `...` spread out. `None` when they do not match, which the
/// generic path then reports, or a default does not fit a slot.
pub unsafe fn matched_sources(function: Sexp, args: &[Arg], call: Sexp) -> R<Option<CallPlan>> {
    let Some((shape, dots_index)) = plannable(function) else {
        return Ok(None);
    };
    plan_sources(function, shape, dots_index, args, call)
}

/// A call's plan: the [`CallShape`] of the closures it is for, and its sources
/// ([`direct_sources`]).
pub type CallPlan = (CallShape, SmallVec<[i32; 8]>);

/// The [`CallShape`] of `function` and which formal is `...`, when calls of it can take their
/// arguments straight into slots: its body is compiled with a slot table whose first names
/// are its formals.
unsafe fn plannable(function: Sexp) -> Option<(CallShape, Option<usize>)> {
    let body = function.cdr();
    if body.kind() != SexpType::Bytecode
        || function == interp().namespace_loader
        || crate::annotations::is_typed(function)
    {
        return None;
    }
    let table = body.cdr().element(bytecode::SLOTS);
    if table == nil() {
        return None;
    }
    let names = table.element(1);
    let slots = values::length(names);
    let registers = values::ints(body.cdr().element(bytecode::REGISTERS))[0];
    let mut shape = CallShape {
        formals: 0,
        slots: u16::try_from(slots).ok()?,
        registers: u16::try_from(registers).ok()?,
    };
    let mut dots_index = None;
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist {
        let index = shape.formals as usize;
        if index >= slots
            || index >= bytecode::MAX_SLOT_FORMALS
            || names.element(index) != formal.tag()
        {
            return None;
        }
        if formal.tag() == symbols().dots {
            dots_index = Some(index);
        }
        shape.formals += 1;
        formal = formal.cdr();
    }
    Some((shape, dots_index))
}

/// The sources of [`direct_sources`] for `args` matched to `function`'s formals.
unsafe fn plan_sources(
    function: Sexp,
    shape: CallShape,
    dots_index: Option<usize>,
    args: &[Arg],
    call: Sexp,
) -> R<Option<CallPlan>> {
    let formal_count = shape.formals as usize;
    let formals = function.car();
    let Ok(plan) = match_plan(formals, args, call) else {
        return Ok(None);
    };
    let missing_value = missing_arg();
    let mut sources: SmallVec<[i32; 8]> = smallvec![-1; formal_count];
    if let Some(index) = dots_index {
        sources[index] = DOTS_SOURCE;
    }
    for (position, &target) in plan.iter().enumerate() {
        if Some(target as usize) == dots_index {
            sources.push(position as i32);
        } else if args[position].value != missing_value {
            sources[target as usize] = position as i32;
        }
    }
    let mut defaults = None;
    let mut formal = formals;
    for (index, source) in sources[..formal_count].iter_mut().enumerate() {
        let default = formal.car();
        if *source == -1 && default != missing_value {
            if !slot_default_fits(function, default, index, &mut defaults)? {
                return Ok(None);
            }
            *source = if default.kind().is_atomic() || default == nil() {
                LITERAL_DEFAULT
            } else {
                PROMISED_DEFAULT
            };
        }
        formal = formal.cdr();
    }
    Ok(Some((shape, sources)))
}

/// Whether a slot frame of `function` can hold the default of the formal at `index` for a call
/// that leaves it out: a literal goes in the slot as it is, and anything else as a promise of
/// its code over the slots ([`bytecode::slot_defaults`], which `defaults` keeps once read), which
/// [`run_frame`] makes once the frame is on the stack.
unsafe fn slot_default_fits(
    function: Sexp,
    default: Sexp,
    index: usize,
    defaults: &mut Option<Sexp>,
) -> R<bool> {
    if default.kind().is_atomic() || default == nil() {
        return Ok(true);
    }
    let defaults = match *defaults {
        Some(defaults) => defaults,
        None => *defaults.insert(bytecode::slot_defaults(function)?),
    };
    Ok(defaults != nil() && defaults.element(index) != nil())
}

/// Calls `function` for a call whose site plans per call ([`MATCH_PER_CALL`]): in a slot frame
/// when `args` match its formals, and down the generic path, which reports the mismatch,
/// otherwise.
pub unsafe fn call_matching(call: Sexp, function: Sexp, args: &[Arg], sys_parent: Sexp) -> R<Sexp> {
    match matched_sources(function, args, call)? {
        Some((shape, sources)) => call_with_plan(call, function, shape, &sources, args, sys_parent),
        None => crate::scope(|| {
            detach_references(args)?;
            apply_closure_owning(call, function, args, sys_parent, None)
        }),
    }
}

/// What binding a planned call needs to know about the closures a plan is for, all fixed by
/// their formals and body ([`plannable`]): so many formals, a slot table of `slots` names (the
/// formals, then the body's locals), and `registers` registers after the slots. Which formals
/// take their default is in the plan's sources ([`LITERAL_DEFAULT`], [`PROMISED_DEFAULT`]), so
/// a plan row stays small.
#[derive(Clone, Copy, Default)]
pub struct CallShape {
    pub formals: u8,
    pub slots: u16,
    pub registers: u16,
}

/// Calls `function` in a slot frame for a call whose plan `sources` ([`direct_sources`], which
/// `bytecode::CallPlans` keeps per site with the closure's `shape`) matched `args` to its
/// formals already: the arguments go straight into the slots, with no matching and none of
/// [`apply_closure`]'s checks, which the plan settled for the closure's formals and body. What
/// can change between calls (a debugger, a move) sends the call down the generic path.
pub unsafe fn call_with_plan(
    call: Sexp,
    function: Sexp,
    shape: CallShape,
    sources: &[i32],
    args: &[Arg],
    sys_parent: Sexp,
) -> R<Sexp> {
    let state = interp();
    if state.debugger.is_some()
        || moves::tracking()
        || function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)
    {
        return crate::scope(|| {
            detach_references(args)?;
            apply_closure(call, function, args, sys_parent, None)
        });
    }
    let count = shape.slots as usize;
    let header = state.bytecode.registers.len();
    bytecode::reserve_registers(header + 1 + count + shape.registers as usize)?;
    let registers = bytecode::register_address(header);
    let missing_value = missing_arg();
    let (sources, dots_sources) = sources.split_at((shape.formals as usize).min(sources.len()));
    let mut dots = missing_value;
    for &source in dots_sources.iter().rev() {
        if let Some(arg) = args.get(source as usize) {
            dots = values::triple(
                SexpType::Dots,
                arg.value,
                if dots == missing_value { nil() } else { dots },
                arg.tag,
            )?;
        }
    }
    let (mut missing, mut promised) = (0u64, 0u64);
    // Only a formal the call leaves out reads its default, so the walk over the formals goes
    // no further than the last of them.
    let (mut formal, mut formal_index) = (function.car(), 0);
    for (index, &source) in sources.iter().enumerate() {
        let value = match args.get(source as usize) {
            Some(arg) if source >= 0 => arg.value,
            _ if source == DOTS_SOURCE => dots,
            _ if source <= LITERAL_DEFAULT => {
                while formal_index < index {
                    formal = formal.cdr();
                    formal_index += 1;
                }
                missing |= 1 << index;
                if source == PROMISED_DEFAULT {
                    promised |= 1 << index;
                }
                formal.car()
            }
            _ => missing_value,
        };
        value.increment_refcount();
        *registers.add(1 + index) = value;
    }
    let unbound = unbound();
    for index in sources.len()..count {
        *registers.add(1 + index) = unbound;
    }
    let entered = enter(
        call,
        function,
        sys_parent,
        args,
        &[],
        header as u32,
        missing,
        promised,
    )?;
    if entered.header == crate::NO_SLOTS {
        return Ok(entered.value);
    }
    let result = on_large_stack(|| bytecode::run_slots(function, header, count));
    leave(entered.depth as usize, result)
}

/// Runs a call of `function` without running its body: `run` does what the body would, from a
/// frame that holds only the formals' values in a slot window at `header`, with `missing`
/// their missing bits. The arguments are values that reading ran no R code for, so binding
/// them where the body would have forced promises is invisible, and the frame is real: native
/// code called back into R from, an error raised, `sys.call()`, `sys.function()` and
/// `traceback()` all see the call, and anything that asks for the frame's environment gets one
/// made from the slots. What the body's activation would add (its registers, its lookups, the
/// forcing of promises) is gone, which is most of the cost of a small closure's call.
///
/// `run` gets the frame's index. While it runs, the slot owner is still the caller, so a call
/// `run` makes with a `NULL` caller environment names the caller, not this frame, as its
/// parent: what `do.call` needs, whose target's parent frame is the caller of `do.call`.
pub unsafe fn run_inlined(
    call: Sexp,
    function: Sexp,
    header: u32,
    missing: u64,
    sys_parent: Sexp,
    args: &[Arg],
    run: impl FnOnce(u32) -> R<Sexp>,
) -> R<Sexp> {
    let _window = SlotWindow(header);
    activity::count(Counter::ClosureCalls);
    let _frame = crate::profile_summary::enter_closure(call, function);
    let state = interp();
    if state.frames.len() as u32 >= state.max_depth {
        return Err(error_in(
            call,
            "evaluation nested too deeply: infinite recursion / options(expressions=)?",
        ));
    }
    let caller = if sys_parent == nil() {
        state.slot_owner
    } else {
        crate::NO_SLOTS
    };
    let (depth, sampled) = push_frame(
        call,
        function,
        nil(),
        sys_parent,
        args,
        &[],
        header,
        missing,
        caller,
    );
    interp().frames[depth - 1].inlined = true;
    let result = sampled.and_then(|()| scope(|| run((depth - 1) as u32)));
    let state = interp();
    let frame = &state.frames[depth - 1];
    let result = if frame.env != nil() || !frame.on_exit.is_empty() || frame.pending != nil() {
        finish_frame(call, depth, None, result)
    } else {
        result
    };
    pop_frame(function, depth, result)
}

/// The environment of the frame at `index`, made from its slots first if it is a slot frame
/// that has none yet. Everything that hands a frame's environment to R code or native code
/// reads it here.
#[inline]
pub unsafe fn frame_env(index: usize, cause: Reify) -> R<Sexp> {
    let frame = &interp().frames[index];
    if frame.env != nil() || frame.slots == crate::NO_SLOTS {
        return Ok(frame.env);
    }
    reify(index, cause)
}

/// What asked for a slot frame's environment, which the profile summary reports as the reason
/// a package call's frame was reified.
#[derive(Clone, Copy)]
pub enum Reify {
    /// The builtin `op`, through [`crate::builtins::Call::env`].
    Builtin(Sexp),
    Site(&'static str),
}

/// The reasons that observe a frame's environment: they evaluate code in it, hand it out as a
/// value, or read or write its bindings by name. Every other reason is a mechanism a slot
/// frame could serve without one. `.External2` hands its routine the calling environment, and
/// the routines packages call through it (rlang's `ffi_enquo`, `ffi_eval` and the like) read
/// bindings there.
pub const OBSERVING_REASONS: [&str; 17] = [
    "eval",
    "evalq",
    "environment",
    "environment()",
    "sys.frame",
    "sys.frames",
    "parent.frame",
    "pos = -1",
    "assign",
    "get",
    "get0",
    "exists",
    "mget",
    "native code",
    "as.environment",
    "environment outliving the frame",
    ".External2",
];

/// The `...` slot of the frame at `index`, when it is a slot frame without an environment and
/// has a `...` formal.
pub unsafe fn slot_dots(index: usize) -> Option<Sexp> {
    let frame = interp().frames.get(index)?;
    if frame.env != nil() || frame.slots == crate::NO_SLOTS {
        return None;
    }
    let mut formal = frame.function.car();
    let mut position = 0;
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == symbols().dots {
            return Some(*bytecode::register_address(
                frame.slots as usize + 1 + position,
            ));
        }
        position += 1;
        formal = formal.cdr();
    }
    None
}

/// What the slot of the formal `symbol` holds in the frame at `index`, and whether the formal is
/// missing, when it is a slot frame without an environment that has such a formal.
pub unsafe fn slot_formal(index: usize, symbol: Sexp) -> Option<(Sexp, bool)> {
    let frame = interp().frames.get(index)?;
    if frame.env != nil() || frame.slots == crate::NO_SLOTS {
        return None;
    }
    let mut formal = frame.function.car();
    let mut position = 0;
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == symbol {
            let value = *bytecode::register_address(frame.slots as usize + 1 + position);
            let missing = position < 64 && frame.missing & (1 << position) != 0;
            return Some((value, missing));
        }
        position += 1;
        formal = formal.cdr();
    }
    None
}

/// The environment of the slot frame whose slots the running byte code reads, which a `NULL`
/// environment it hands out stands for (see [`crate::builtins::Call::env`]).
pub unsafe fn slot_owner_env(cause: Reify) -> R<Sexp> {
    let owner = interp().slot_owner;
    if owner == crate::NO_SLOTS {
        panic!("byte code handed out a slot frame's environment outside any slot frame");
    }
    frame_env(owner as usize, cause)
}

/// `parent.frame()` of the frame at `index`: the environment its call was made from.
pub unsafe fn frame_sys_parent(index: usize, cause: Reify) -> R<Sexp> {
    let frame = &interp().frames[index];
    if frame.sys_parent != nil() || frame.caller == crate::NO_SLOTS {
        return Ok(frame.sys_parent);
    }
    frame_env(frame.caller as usize, cause)
}

/// The environment the frame at `index` was called from, as `parent.frame` hands it out: a
/// slot frame that has none gives its pending environment instead of reifying, and reifies
/// only once code reads its bindings ([`resolve_pending_read`]) or keeps the environment past
/// the frame's return ([`finish_frame`]).
pub unsafe fn frame_sys_parent_pending(index: usize) -> R<Sexp> {
    let frame = &interp().frames[index];
    if frame.sys_parent != nil() || frame.caller == crate::NO_SLOTS {
        return Ok(frame.sys_parent);
    }
    let owner = frame.caller as usize;
    let target = &interp().frames[owner];
    if target.env != nil() || target.slots == crate::NO_SLOTS {
        return Ok(target.env);
    }
    let env = pending_env(owner)?;
    interp().frames[owner].pending_shared = true;
    Ok(env)
}

/// The environment of the slot frame whose header register is `header`, which must be live.
pub unsafe fn slot_frame_env(header: usize, cause: Reify) -> R<Sexp> {
    let frames = &interp().frames;
    let Some(index) = frames
        .iter()
        .rposition(|frame| frame.slots as usize == header)
    else {
        panic!("a slot frame's activation outlived its frame");
    };
    frame_env(index, cause)
}

/// The environment that stands for the slot frame whose header register is `header` while it
/// has none: the pending one its closures hold, or `nil` when it made none.
pub unsafe fn pending_env_of(header: usize) -> Sexp {
    let frames = &interp().frames;
    frames
        .iter()
        .rposition(|frame| frame.slots as usize == header)
        .map_or(nil(), |index| frames[index].pending)
}

/// Gives the slot frame at `index` its environment: one binding cell per bound slot, formals
/// first and with their missing flags, exactly as [`bind_arguments`] and the assignments since
/// would have bound them. The environment is the frame's from then on, so the slots give up
/// their references and nothing reads them again.
#[cold]
#[inline(never)]
unsafe fn reify(index: usize, cause: Reify) -> R<Sexp> {
    if activity::enabled() {
        let name = match cause {
            Reify::Builtin(op) => builtin_def(op)?.name,
            Reify::Site(name) => name,
        };
        let reason = activity::label(name);
        activity::count_reification(reason);
        interp().frames[index].reified_by = reason;
    }
    materialize_references(index)?;
    let frame = &interp().frames[index];
    // A variable the caller passed to an inlined wrapper sits in its slot as the value; the
    // generic path would have bound a promise of the variable, forced by the builtin call, and
    // `substitute` reads the variable from it. Constants bind as themselves either way.
    if frame.inlined
        && let Some((shape, sources)) = direct_sources(frame.function, frame.call)?
    {
        let header = frame.slots as usize;
        let call = frame.call;
        for (slot, &source) in sources.iter().take(shape.formals as usize).enumerate() {
            if source < 0 {
                continue;
            }
            let mut cell = call.cdr();
            for _ in 0..source {
                cell = cell.cdr();
            }
            let expression = cell.car();
            if expression.kind() != SexpType::Symbol {
                continue;
            }
            let register = bytecode::register_address(header + 1 + slot);
            let promise = forced_promise(expression, *register)?;
            promise.increment_refcount();
            (*register).decrement_refcount();
            *register = promise;
        }
    }
    let frame = &interp().frames[index];
    let (function, header, missing) = (frame.function, frame.slots as usize, frame.missing);
    let names = function.cdr().cdr().element(bytecode::SLOTS).element(1);
    let count = values::length(names);
    let formals = function.car();
    let pending = frame.pending;
    let env = if pending == nil() {
        frame_environment(function, formals)?
    } else {
        pending.set_gp_bits(gp::PENDING_FRAME, false);
        pending
    };
    let registers = bytecode::register_address(header);
    let mut bindings: SmallVec<[Formal; 12]> = SmallVec::new();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        let index = bindings.len();
        bindings.push(Formal {
            symbol: formal.tag(),
            value: *registers.add(1 + index),
            missing: missing & (1 << index) != 0,
        });
        formal = formal.cdr();
    }
    let heap = crate::heap();
    heap.define_formals_uncollected(env, &bindings)?;
    heap.mark_frame_formals(env);
    if pending == nil() {
        let locals: SmallVec<[Formal; 12]> = (bindings.len()..count)
            .filter_map(|slot| {
                let value = *registers.add(1 + slot);
                (value != unbound()).then(|| Formal {
                    symbol: names.element(slot),
                    value,
                    missing: false,
                })
            })
            .collect();
        heap.define_locals_uncollected(env, &locals)?;
    } else {
        for slot in bindings.len()..count {
            let value = *registers.add(1 + slot);
            if value != unbound() {
                heap.define(env, names.element(slot), value)?;
            }
        }
    }
    let method_vars: SmallVec<[(Sexp, Sexp); 6]> = crate::method_vars(interp(), index).into();
    for &(symbol, value) in &method_vars {
        if heap.frame_cell(env, symbol).is_none() {
            heap.define(env, symbol, resolved_value(value)?)?;
        }
    }
    let nil = nil();
    for slot in 0..count {
        (*registers.add(1 + slot)).decrement_refcount();
        *registers.add(1 + slot) = nil;
    }
    env.set_gp_bits(gp::COMPILED_FRAME, true);
    *registers = env;
    let state = interp();
    state.frames[index].env = env;
    if !state.pending_closures.is_empty() {
        state
            .pending_closures
            .retain(|&(owner, _)| owner as usize != index);
    }
    activity::count(Counter::FramesReified);
    if syntax::testing::invariants_enabled() {
        check_reified(index, env, &bindings, &method_vars);
    }
    Ok(env)
}

/// The cross-check behind `syntax::testing::enable_invariants()`: the environment made from a
/// slot frame binds each formal as the heap-frame path would have, to the same value with the
/// same missing flag, and binds nothing but its body's locals and, for a method, its dispatch
/// variables.
unsafe fn check_reified(
    index: usize,
    env: Sexp,
    bindings: &[Formal],
    method_vars: &[(Sexp, Sexp)],
) {
    let heap = crate::heap();
    for binding in bindings {
        let Some(cell) = heap.frame_cell(env, binding.symbol) else {
            panic!(
                "frame {index}: a reified slot frame lost formal {:?}",
                values::symbol_name(binding.symbol)
            );
        };
        assert!(
            cell.cdr() == binding.value && cell.has_gp(gp::BINDING_MISSING) == binding.missing,
            "frame {index}: a reified slot frame binds formal {:?} differently",
            values::symbol_name(binding.symbol)
        );
    }
    let function = interp().frames[index].function;
    let locals = bytecode::local_symbols(bytecode::source_of_body(function.cdr()), function.car());
    let Some(locals) = locals else {
        panic!("frame {index}: a slot frame whose body has no known local symbols");
    };
    for symbol in crate::envir::env_symbols(env, true, false) {
        assert!(
            locals.contains(&symbol) || method_vars.iter().any(|&(name, _)| name == symbol),
            "frame {index}: a reified slot frame binds {:?}, which its body cannot bind",
            values::symbol_name(symbol)
        );
    }
}

/// A fresh, rooted function frame for a call of `function`, after one collection check that
/// covers the frame, a binding cell and a default promise per formal, so the caller builds them
/// with the `_uncollected` allocators. The caller keeps the arguments alive, and the formals'
/// defaults are reachable from `function`.
unsafe fn frame_environment(function: Sexp, formals: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let count = values::length(formals);
    heap.reserve(ENVIRONMENT_BYTES + 2 * TRIPLE_BYTES * count, &[function]);
    let env = heap.alloc_environment_uncollected(function.tag(), count)?;
    heap.root(env);
    env.set_gp_bits(gp::FUNCTION_FRAME, true);
    Ok(env)
}

/// [`apply_closure`] for a call whose first argument is a value moved out of the caller's
/// variable (see [`moves::call_moving`]): the frame's record of the arguments does not count as
/// a reference to it, so the parameter holds the only one.
pub unsafe fn apply_closure_moved(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
) -> R<Sexp> {
    let formals = function.car();
    let env = frame_environment(function, formals)?;
    let mut bindings: SmallVec<[Formal; 12]> = SmallVec::new();
    let mut formal = formals;
    let mut index = 0;
    while formal.kind() == SexpType::Pairlist {
        let actual = args.get(index).map_or(missing_arg(), |arg| arg.value);
        bindings.push(formal_binding(env, formal, actual)?);
        formal = formal.cdr();
        index += 1;
    }
    let heap = crate::heap();
    heap.define_formals_uncollected(env, &bindings)?;
    heap.mark_frame_formals(env);
    run_closure(call, function, env, sys_parent, args, Entry::Unchecked)
}

/// Whether every argument is untagged and the formals, which have no `...`, take them all.
unsafe fn positional(formals: Sexp, args: &[Arg]) -> bool {
    let mut count = 0;
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == symbols().dots {
            return false;
        }
        count += 1;
        formal = formal.cdr();
    }
    args.len() <= count && args.iter().all(|arg| arg.tag == nil())
}

/// The binding of the formal at the head of `formal` to its matched argument, or to a promise of
/// its default when the argument is missing. The promise comes from the budget that
/// [`frame_environment`] reserved, so it needs no root before the frame holds it.
unsafe fn formal_binding(env: Sexp, formal: Sexp, actual: Sexp) -> R<Formal> {
    let missing = missing_arg();
    let symbol = formal.tag();
    if actual == missing && formal.car() != missing {
        // A literal default evaluates to itself, so it is bound as is: the promise would cost
        // an allocation and a force on every call that omits it, for the same value.
        let default = formal.car();
        let value = if default.kind().is_atomic() || default == nil() {
            default
        } else {
            crate::heap().alloc_triple_uncollected(SexpType::Promise, unbound(), default, env)?
        };
        return Ok(Formal {
            symbol,
            value,
            missing: true,
        });
    }
    Ok(Formal {
        symbol,
        value: actual,
        missing: false,
    })
}

/// The arguments as a rooted pairlist of their values or promises. The caller keeps the values
/// alive, so one collection check covers every cell.
pub unsafe fn args_pairlist(args: &[Arg]) -> R<Sexp> {
    let heap = crate::heap();
    heap.reserve(TRIPLE_BYTES * args.len(), &[]);
    let mut list = nil();
    for arg in args.iter().rev() {
        list = heap.alloc_triple_uncollected(SexpType::Pairlist, arg.value, list, arg.tag)?;
    }
    heap.root(list);
    Ok(list)
}

/// Runs a closure body in an environment whose arguments are already bound.
pub unsafe fn run_closure(
    call: Sexp,
    function: Sexp,
    env: Sexp,
    sys_parent: Sexp,
    args: &[Arg],
    entry: Entry,
) -> R<Sexp> {
    run_frame(
        call,
        function,
        CallFrame::Env(env),
        sys_parent,
        args,
        entry,
        &[],
    )
}

/// Runs a closure body in `frame`, whose arguments are already bound. `sys_parent` is the
/// environment the call was made from, or `NULL` when the caller is the frame below this one
/// and has no environment yet. A slot frame's registers are released however the call ends.
pub unsafe fn run_frame(
    call: Sexp,
    function: Sexp,
    frame: CallFrame,
    sys_parent: Sexp,
    args: &[Arg],
    entry: Entry,
    method_vars: &[(Sexp, Sexp)],
) -> R<Sexp> {
    let CallFrame::Slots {
        header,
        missing,
        promised,
    } = frame
    else {
        return run_frame_general(call, function, frame, sys_parent, args, entry, method_vars);
    };
    let entered = enter(
        call,
        function,
        sys_parent,
        args,
        method_vars,
        header,
        missing,
        promised,
    )?;
    if entered.header == crate::NO_SLOTS {
        return Ok(entered.value);
    }
    let slots = bytecode::slot_count(function.cdr());
    let result = on_large_stack(|| bytecode::run_slots(function, header as usize, slots));
    leave(entered.depth as usize, result)
}

/// A slot frame's call once [`enter`] has set it up. When `header` is [`crate::NO_SLOTS`] the
/// call has already run to completion and `value` is its result. Otherwise the caller runs
/// `value`, the closure's compiled code, on the slot frame at `header`, and hands its result
/// to [`leave`] with `depth`.
#[derive(Clone, Copy)]
pub struct Entered {
    pub value: Sexp,
    pub header: u32,
    pub depth: u32,
}

/// Pushes the frame of a call of `function` on the slot frame at `header`, whose arguments are
/// already bound, without running its body, so an interpreter loop can run the body in place
/// rather than recursing. A call that needs what only the general path does (activity
/// counters, the debugger, the object-system refusal, a namespace load) runs whole here.
///
/// From a successful return until [`leave`], the frame is the slot owner. The slot frame's
/// registers belong to the call: [`leave`] releases them, and so does `enter` itself on an
/// error.
#[allow(clippy::too_many_arguments)]
pub unsafe fn enter(
    call: Sexp,
    function: Sexp,
    sys_parent: Sexp,
    args: &[Arg],
    method_vars: &[(Sexp, Sexp)],
    header: u32,
    missing: u64,
    promised: u64,
) -> R<Entered> {
    let state = interp();
    if activity::enabled()
        || !state.object_systems
        || function == state.namespace_loader
        || (state.debugger.is_some() && function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP))
    {
        let frame = CallFrame::Slots {
            header,
            missing,
            promised,
        };
        let value = run_frame_general(
            call,
            function,
            frame,
            sys_parent,
            args,
            Entry::Unchecked,
            method_vars,
        )?;
        return Ok(Entered {
            value,
            header: crate::NO_SLOTS,
            depth: 0,
        });
    }
    if state.frames.len() as u32 >= state.max_depth {
        release_window(header);
        return Err(error_in(
            call,
            "evaluation nested too deeply: infinite recursion / options(expressions=)?",
        ));
    }
    // A `NULL` `sys_parent` comes from byte code running on a slot frame's slots, the caller.
    let caller = if sys_parent == nil() {
        state.slot_owner
    } else {
        crate::NO_SLOTS
    };
    let (depth, sampled) = push_frame(
        call,
        function,
        nil(),
        sys_parent,
        args,
        method_vars,
        header,
        missing,
        caller,
    );
    let defaults = sampled.and_then(|()| match promised {
        0 => Ok(()),
        _ => default_promises(depth - 1, function, header as usize, promised),
    });
    match defaults {
        Ok(()) => {
            let state = interp();
            state.slot_owner = (depth - 1) as u32;
            Ok(Entered {
                value: function.cdr(),
                header,
                depth: depth as u32,
            })
        }
        Err(unwind) => leave(depth, Err(unwind)).map(|value| Entered {
            value,
            header: crate::NO_SLOTS,
            depth: 0,
        }),
    }
}

/// Runs `body` in a frame for the call `typed` describes, with a new environment enclosed by the
/// closure's. Compiled typed code has no frame of its own and runs each R call it makes this
/// way, so `stop()`, `sys.call()` and `parent.frame()` there see the call the interpreter makes.
pub unsafe fn run_in_frame(
    typed: &bytecode::TypedCall<'_>,
    body: impl FnOnce(Sexp) -> R<()>,
) -> R<()> {
    let env = values::new_env(typed.function.tag())?;
    let caller = if typed.sys_parent == nil() {
        interp().slot_owner
    } else {
        crate::NO_SLOTS
    };
    let (depth, sampled) = push_frame(
        typed.call,
        typed.function,
        env,
        typed.sys_parent,
        typed.args,
        &[],
        crate::NO_SLOTS,
        0,
        caller,
    );
    let result = sampled.and_then(|()| body(env)).map(|()| nil());
    leave(depth, result).map(|_| ())
}

/// Finishes the call [`enter`] pushed at `depth` once its body has ended with `result`: it
/// releases what the body rooted, restores the slot owner, catches a `return()` aimed at the
/// frame, runs `on.exit`, pops the frame and releases the slot frame's registers. It runs on
/// the error path too, and it is the only way such a frame pops.
pub unsafe fn leave(depth: usize, result: R<Sexp>) -> R<Sexp> {
    let state = interp();
    let frame = &state.frames[depth - 1];
    let (call, function, header) = (frame.call, frame.function, frame.slots);
    let (mark, owner) = (frame.handle_mark, frame.previous_owner);
    crate::heap().release_handles(mark as usize);
    match &result {
        Ok(value) => value.root(),
        Err(unwind) => unwind.root(),
    }
    state.slot_owner = owner;
    let result = finish_frame(call, depth, None, result);
    let result = pop_frame(function, depth, result);
    release_window(header);
    result
}

/// [`run_frame`] for a frame with an environment, and for a slot frame whose call needs more
/// than [`enter`] and [`leave`] do.
unsafe fn run_frame_general(
    call: Sexp,
    function: Sexp,
    frame: CallFrame,
    sys_parent: Sexp,
    args: &[Arg],
    entry: Entry,
    method_vars: &[(Sexp, Sexp)],
) -> R<Sexp> {
    let (env, slots, missing, promised) = match frame {
        CallFrame::Env(env) => (env, crate::NO_SLOTS, 0, 0),
        CallFrame::Slots {
            header,
            missing,
            promised,
        } => (nil(), header, missing, promised),
    };
    let _window = SlotWindow(slots);
    let _activity = activity::enter(Activity::CallSetup, 0);
    activity::count(Counter::ClosureCalls);
    let _frame = crate::profile_summary::enter_closure(call, function);
    if slots == crate::NO_SLOTS {
        moves::entering_closure(function, env)?;
    }
    let state = interp();
    if state.frames.len() as u32 >= state.max_depth {
        return Err(error_in(
            call,
            "evaluation nested too deeply: infinite recursion / options(expressions=)?",
        ));
    }
    // A `NULL` `sys_parent` comes from byte code running on a slot frame's slots, the caller.
    let caller = if sys_parent == nil() {
        state.slot_owner
    } else {
        crate::NO_SLOTS
    };
    if !state.object_systems {
        let sys_parent = if caller != crate::NO_SLOTS {
            frame_env(caller as usize, Reify::Site("object-system check"))?
        } else {
            sys_parent
        };
        refuse_object_system(call, function, sys_parent)?;
    }
    let (depth, sampled) = push_frame(
        call,
        function,
        env,
        sys_parent,
        args,
        method_vars,
        slots,
        missing,
        caller,
    );
    if let Err(unwind) = sampled {
        return pop_frame(function, depth, Err(unwind));
    }
    let state = interp();
    if state.debugger.is_some() && function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP) {
        function.set_gp_bits(gp::CLOSURE_STEP, false);
        crate::debugger::debugged_call(depth);
    }
    let signature = match entry {
        // A slot frame's closure is never typed (`bind_slots`).
        _ if slots != crate::NO_SLOTS => {
            default_promises(depth - 1, function, slots as usize, promised).map(|()| None)
        }
        Entry::Unchecked => crate::annotations::enter(call, function, env, depth),
        Entry::Checked(signature) => Ok(signature),
    };
    // Ends the namespace load however the call exits, a panic included, so a load that never
    // returns cannot keep major collections deferred.
    struct NamespaceLoad;
    impl Drop for NamespaceLoad {
        fn drop(&mut self) {
            crate::heap().leave_namespace_load();
            crate::image::touch::leave_namespace_load();
        }
    }
    let _namespace_load = (function == state.namespace_loader).then(|| {
        crate::heap().enter_namespace_load();
        crate::image::touch::enter_namespace_load();
        NamespaceLoad
    });
    let result = signature.map(|signature| {
        let result = scope(|| {
            on_large_stack(|| {
                if slots != crate::NO_SLOTS {
                    let state = interp();
                    let owner = state.slot_owner;
                    state.slot_owner = (depth - 1) as u32;
                    let count = bytecode::slot_count(function.cdr());
                    let result = bytecode::run_slots(function, slots as usize, count);
                    interp().slot_owner = owner;
                    return result;
                }
                let body = function.cdr();
                bytecode::run_body(function, body, env)
            })
        });
        (signature, result)
    });
    drop(_namespace_load);
    let (signature, result) = match result {
        Ok(pair) => pair,
        Err(unwind) => (None, Err(unwind)),
    };
    let result = finish_frame(call, depth, signature, result);
    pop_frame(function, depth, result)
}

/// What a call returns once its body has run with `result`: a `return()` aimed at the frame, the
/// return type check, and the `on.exit` expressions, before the frame pops.
#[inline(always)]
unsafe fn finish_frame(
    call: Sexp,
    depth: usize,
    signature: Option<Sexp>,
    result: R<Sexp>,
) -> R<Sexp> {
    // A slot frame may have made its environment while the body ran; until it does, only its
    // pending environment, if it handed one out, can name the frame as a `return` target.
    let frame = &interp().frames[depth - 1];
    let env = if frame.env == nil() {
        frame.pending
    } else {
        frame.env
    };
    let result = if env == nil() {
        result
    } else {
        stray_loop_jump(call, env, catch_return(env, result))
    };
    let result = match (signature, result) {
        (Some(signature), Ok(value)) => crate::annotations::check_return(call, signature, value),
        (_, result) => result,
    };
    crate::annotations::leave(depth);
    // `return()` may also come from an `on.exit` expression, which runs in the same frame and
    // so returns from it too.
    let state = interp();
    let result = if state.frames[depth - 1].on_exit.is_empty() {
        result
    } else {
        let result = run_on_exit(depth, result);
        let frame = &interp().frames[depth - 1];
        catch_return(
            if frame.env == nil() {
                frame.pending
            } else {
                frame.env
            },
            result,
        )
    };
    // Promises over the slots that may still be forced need the environment once the slots are
    // gone, and so does anything that kept the pending environment itself.
    let frame = &state.frames[depth - 1];
    if frame.env == nil() && frame.pending != nil() {
        let (closures, dying) = if frame.unforced > 0 {
            pending_closures_dying(depth - 1, &result)
        } else {
            (0, 0)
        };
        let frame = &interp().frames[depth - 1];
        if closures > dying {
            reify(depth - 1, Reify::Site("closure outliving the frame"))?;
        } else if frame.unforced > unforced_defaults(frame) + closures {
            reify(depth - 1, Reify::Site("promise outliving the frame"))?;
        } else if pending_env_kept(depth - 1, &result) {
            reify(depth - 1, Reify::Site("environment outliving the frame"))?;
        } else if closures > 0 {
            interp()
                .pending_closures
                .retain(|&(owner, _)| owner as usize != depth - 1);
        }
    }
    result
}

/// Whether something other than what dies with the slot frame at `index` holds its pending
/// environment as the frame returns `result`: the counted references to it must all come from
/// the frame's dying closures and the unforced default promises in its slots, the environment
/// must not be the value returned, and `R_PreserveObject` must not hold it. An error
/// return keeps it, since the unwind may carry it.
unsafe fn pending_env_kept(index: usize, result: &R<Sexp>) -> bool {
    let Ok(value) = *result else {
        return true;
    };
    let state = interp();
    let frame = &state.frames[index];
    let pending = frame.pending;
    if value == pending || crate::heap().is_preserved(pending) {
        return true;
    }
    let closures = state
        .pending_closures
        .iter()
        .filter(|&&(owner, closure)| owner as usize == index && closure.tag() == pending)
        .count() as u32;
    u32::from(pending.refcount()) > unforced_defaults(frame) + closures
}

/// How many closures the slot frame at `index` made over its pending environment, and how many
/// of them die with it as it returns `result`. One dies when nothing counts a reference to it
/// but the frame's slots, it
/// is not the value returned, and no root the counts do not see holds it (`R_PreserveObject`
/// or a global calling handler). A closure whose environment was replaced no longer needs the
/// frame either. An error return keeps every closure, since the unwind may carry one.
unsafe fn pending_closures_dying(index: usize, result: &R<Sexp>) -> (u32, u32) {
    let state = interp();
    if state.pending_closures.is_empty() {
        return (0, 0);
    }
    let closures = state
        .pending_closures
        .iter()
        .filter(|&&(owner, _)| owner as usize == index)
        .count() as u32;
    let Ok(value) = *result else {
        return (closures, 0);
    };
    let frame = &state.frames[index];
    let (pending, header) = (frame.pending, frame.slots as usize);
    let names = frame
        .function
        .cdr()
        .cdr()
        .element(bytecode::SLOTS)
        .element(1);
    let count = values::length(names);
    let heap = crate::heap();
    let mut dying = 0;
    for &(owner, closure) in &state.pending_closures {
        if owner as usize != index {
            continue;
        }
        if closure.tag() != pending {
            dying += 1;
            continue;
        }
        if closure == value || heap.is_preserved(closure) {
            continue;
        }
        let held = (0..count)
            .filter(|&slot| *bytecode::register_address(header + 1 + slot) == closure)
            .count();
        if closure.refcount() as usize == held
            && !crate::conditions::handlers_hold(&state.global_handlers, closure)
        {
            dying += 1;
        }
    }
    (closures, dying)
}

/// A slot frame that never made its environment gives up its slots' references, and the
/// register file drops the window, on every way out.
struct SlotWindow(u32);

impl Drop for SlotWindow {
    fn drop(&mut self) {
        // SAFETY: the window's header register is live until the window drops.
        unsafe { release_window(self.0) }
    }
}

/// Releases the registers of the slot frame at `header`, and its slots' references when it
/// never made its environment.
unsafe fn release_window(header: u32) {
    if header == crate::NO_SLOTS {
        return;
    }
    let header = header as usize;
    let registers = &mut interp().bytecode.registers;
    if *registers.get_unchecked(header) == nil()
        && let Some(slots) = registers.get_mut(header + 1..)
    {
        crate::heap().release_slots(slots);
    }
    registers.truncate(header);
}

/// Pushes the frame of a call of the closure `function`, makes the closure's srcref current,
/// and returns the frame's depth. On an error the frame is pushed all the same.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
unsafe fn push_frame(
    call: Sexp,
    function: Sexp,
    env: Sexp,
    sys_parent: Sexp,
    args: &[Arg],
    method_vars: &[(Sexp, Sexp)],
    slots: u32,
    missing: u64,
    caller: u32,
) -> (usize, R<()>) {
    let state = interp();
    let start = state.frame_args.len() as u32;
    state.frame_args.extend_from_slice(args);
    let method_vars_start = state.method_vars.len() as u32;
    state.method_vars.extend_from_slice(method_vars);
    state.calls_ever = state.calls_ever.wrapping_add(1);
    let (handlers, restarts) = (state.handlers.clone(), state.restarts.clone());
    let (srcref, closures) = (state.srcref, state.frame_closures.len() as u32);
    let (handle_mark, previous_owner) = (crate::heap().handle_mark() as u32, state.slot_owner);
    state.frames.push_with(|| Frame {
        call,
        function,
        env,
        sys_parent,
        args: start,
        on_exit: ThinVec::new(),
        handlers,
        restarts,
        srcref,
        method_vars: method_vars_start,
        closures,
        slots,
        missing,
        caller,
        pending: nil(),
        unforced: 0,
        inlined: false,
        reified_by: crate::NOT_REIFIED,
        pending_shared: false,
        handle_mark,
        previous_owner,
    });
    let sampled = if PROFILE_TICK.load(Ordering::Relaxed) {
        record_profile_sample(None)
    } else {
        Ok(())
    };
    let state = interp();
    state.srcref = values::get_attr(function, symbols().srcref);
    (state.frames.len(), sampled)
}

/// Pops the frame at `depth` (one-based) once its call has finished with `result`, restoring
/// the condition stacks and srcref of the caller.
#[inline(always)]
unsafe fn pop_frame(function: Sexp, depth: usize, result: R<Sexp>) -> R<Sexp> {
    let state = interp();
    if activity::enabled() {
        let frame = &state.frames[depth - 1];
        let end = state
            .frames
            .get(depth)
            .map_or(state.frame_args.len(), |next| next.args as usize);
        let args = state
            .frame_args
            .get(frame.args as usize..end)
            .unwrap_or(&[]);
        count_package_call(function, Some(frame), args);
    }
    // An unwind that skipped frames above this one, or closures awaiting their frame, need the
    // general truncation; a call returning normally needs neither.
    if state.frames.len() != depth || !state.pending_closures.is_empty() {
        crate::truncate_frames(state, depth);
    }
    let Some(frame) = state.frames.last() else {
        panic!("a closure call's frame was popped while the call ran");
    };
    let (env, closures, srcref) = (frame.env, frame.closures, frame.srcref);
    let (args, method_vars) = (frame.args as usize, frame.method_vars as usize);
    let Some((handlers, restarts)) = state.frames.pop() else {
        panic!("a closure call's frame was popped while the call ran");
    };
    // The frame's saved stack, which `.addGlobHands` may have rewritten.
    conditions::restore_handlers(handlers);
    state.restarts = restarts;
    state.frame_args.truncate(args);
    state.method_vars.truncate(method_vars);
    state.srcref = srcref;
    if let Ok(value) = result
        && env != nil()
    {
        let created = state.frame_closures.get(closures as usize..).unwrap_or(&[]);
        crate::heap().release_frame(env, value, created);
    }
    state.frame_closures.truncate(closures as usize);
    result
}

/// Counts a call to a closure of a package namespace, and whether it skipped the generic call
/// protocol: it ran natively without a frame (`frame` is `None`) or in a slot frame that never
/// made its environment, and no argument was a promise. A call that did not is counted under
/// the reason: what reified its frame, a frame that had an environment from the start, or the
/// kind of its first promise argument's code.
#[cold]
pub(crate) unsafe fn count_package_call(function: Sexp, frame: Option<&Frame>, args: &[Arg]) {
    let enclosure = function.tag();
    if enclosure == values::base_namespace() || !crate::envir::is_namespace(enclosure) {
        return;
    }
    activity::count(Counter::PackageCalls);
    let reason = match frame {
        Some(frame) if frame.slots == crate::NO_SLOTS => Some("frame with an environment"),
        Some(frame) if frame.env != nil() => {
            let reason = activity::label_name(frame.reified_by);
            if reason
                .as_deref()
                .is_some_and(|name| OBSERVING_REASONS.contains(&name))
            {
                activity::count(Counter::PackageCallsObserved);
            }
            activity::count_package_fallback(match reason {
                Some(_) => frame.reified_by,
                None => activity::label("reason not recorded"),
            });
            return;
        }
        _ => args
            .iter()
            .find(|arg| arg.value.kind() == SexpType::Promise && !is_reference(arg.value))
            .map(|arg| match arg.value.cdr().kind() {
                SexpType::Symbol => "promise argument: variable",
                SexpType::Language => "promise argument: call",
                _ => "promise argument: other",
            }),
    };
    match reason {
        Some(reason) => activity::count_package_fallback(activity::label(reason)),
        None => activity::count(Counter::PackageCallsDirect),
    }
}

/// Replaces the default of each formal in `promised`, which the binding
/// put in its slot as the bare expression, with a promise of its code over the slot frame at
/// `owner`.
unsafe fn default_promises(owner: usize, function: Sexp, header: usize, promised: u64) -> R<()> {
    if promised == 0 {
        return Ok(());
    }
    let defaults = bytecode::slot_defaults(function)?;
    let mut pending = promised;
    while pending != 0 {
        let index = pending.trailing_zeros() as usize;
        pending &= pending - 1;
        let promise = slot_promise(owner, defaults.element(index))?;
        crate::heap().store_slot(
            &mut *bytecode::register_address(header + 1 + index),
            promise,
        );
    }
    Ok(())
}

/// How many of the frame's unforced promises are its own defaults that nothing but their slot
/// holds. They die with the slots, so they need no environment once the frame returns.
unsafe fn unforced_defaults(frame: &Frame) -> u32 {
    let mut count = 0;
    let mut formal = frame.function.car();
    let mut index = 0;
    while formal.kind() == SexpType::Pairlist && frame.missing >> index != 0 {
        if frame.missing & (1 << index) != 0 {
            let value = *bytecode::register_address(frame.slots as usize + 1 + index);
            if value.kind() == SexpType::Promise
                && value.car() == unbound()
                && value.tag() == frame.pending
                && value.refcount() == 1
            {
                count += 1;
            }
        }
        formal = formal.cdr();
        index += 1;
    }
    count
}

/// The stack a closure body may use before the interpreter moves to a fresh segment.
const STACK_RED_ZONE: usize = 128 * 1024;
const STACK_SEGMENT: usize = 4 * 1024 * 1024;

/// Runs `body` with at least [`STACK_RED_ZONE`] of stack left, moving to a fresh segment when
/// less is left, as `stacker::maybe_grow` does. Asking `stacker` reads thread-local state on
/// every closure call, so the interpreter keeps the floor of the segment in use and a ceiling
/// at or above every frame the interpreter runs on it, and `stacker` runs only near the floor
/// or off that range. A segment lies anywhere in memory, and other code switches segments too
/// (`libr`'s and `rdb`'s readers run R hooks on their own), so a stack pointer outside
/// `[floor, ceiling]` means a segment the interpreter has not measured.
#[inline(always)]
fn on_large_stack<T>(body: impl FnOnce() -> T) -> T {
    let marker = 0u8;
    let pointer = std::ptr::addr_of!(marker) as usize;
    let state = interp();
    if pointer <= state.stack_ceiling
        && pointer.saturating_sub(state.stack_floor) > 2 * STACK_RED_ZONE
    {
        return body();
    }
    on_large_stack_slow(body)
}

#[inline(never)]
fn on_large_stack_slow<T>(body: impl FnOnce() -> T) -> T {
    // An R error unwinds through here, so the caller's segment bounds are restored by a guard:
    // stale bounds from a freed segment would let the fast path skip the check.
    struct RestoreBounds(usize, usize);
    impl Drop for RestoreBounds {
        fn drop(&mut self) {
            let state = interp();
            state.stack_floor = self.0;
            state.stack_ceiling = self.1;
        }
    }
    let bounds = || {
        let marker = 0u8;
        let pointer = std::ptr::addr_of!(marker) as usize;
        stacker::remaining_stack()
            .map(|remaining| (pointer.saturating_sub(remaining), pointer, remaining))
    };
    let state = interp();
    let _restore = RestoreBounds(state.stack_floor, state.stack_ceiling);
    match bounds() {
        Some((floor, ceiling, remaining)) if remaining >= STACK_RED_ZONE => {
            let state = interp();
            state.stack_floor = floor;
            state.stack_ceiling = ceiling;
            body()
        }
        _ => stacker::maybe_grow(STACK_RED_ZONE, STACK_SEGMENT, || {
            let (floor, ceiling) = bounds().map_or((0, 0), |(floor, ceiling, _)| (floor, ceiling));
            let state = interp();
            state.stack_floor = floor;
            state.stack_ceiling = ceiling;
            body()
        }),
    }
}

/// With object systems turned off, a call that defines a class (`setClass`, `setRefClass`,
/// `R6::R6Class`, ...) fails when project code makes it. Project code is whatever runs with the
/// global environment as its top level, which covers scripts and the console, or inside a
/// namespace `ry load` built, which carries `.__ry_loader__.`. Calls made inside an installed
/// package are exempt, so a dependency that defines classes of its own keeps loading. The
/// callee is recognized by the name the call uses (`setClass(...)` or `methods::setClass(...)`)
/// and by the package that owns the closure, so a local function of the same name is left alone
/// and a renamed alias (`f <- setClass`) is not caught.
unsafe fn refuse_object_system(call: Sexp, function: Sexp, sys_parent: Sexp) -> R<()> {
    let mut head = call.car();
    if head.kind() == SexpType::Language
        && head.car().kind() == SexpType::Symbol
        && matches!(&*values::symbol_name(head.car()), "::" | ":::")
    {
        head = head.cdr().cdr().car();
    }
    if head.kind() != SexpType::Symbol {
        return Ok(());
    }
    let name = values::symbol_name(head);
    let Some((system, defines, home)) = syntax::native_values::object_system_function(&name) else {
        return Ok(());
    };
    let owner = crate::dispatch::top_env(function.tag());
    if crate::envir::namespace_name(owner).as_deref() != Some(home) {
        return Ok(());
    }
    let caller = crate::dispatch::top_env(sys_parent);
    let project = caller == values::global_env()
        || crate::heap()
            .frame_cell(caller, symbols().dot_ry_loader)
            .is_some();
    if !project {
        return Ok(());
    }
    Err(error_in(
        call,
        syntax::native_values::object_systems_disabled_message(system, defines, &name),
    ))
}

/// A `break` or `next` that leaves the closure frame of `env` had no loop to go to, which is an
/// error that `try()` can catch, reported against the closure's call as in GNU R.
fn stray_loop_jump(call: Sexp, env: Sexp, result: R<Sexp>) -> R<Sexp> {
    match result {
        Err(Unwind::Jump(Jump::Break { env: target } | Jump::Next { env: target }))
            if target == env =>
        {
            Err(error_in(
                call,
                "no loop for break/next, jumping to top level",
            ))
        }
        other => other,
    }
}

/// The value of a `return()` that unwinds to the frame of `env`.
unsafe fn catch_return(env: Sexp, result: R<Sexp>) -> R<Sexp> {
    match result {
        Err(unwind) if unwind.returns_to(env) => {
            let Unwind::Jump(jump) = unwind else {
                unreachable!()
            };
            let Jump::Return { value, .. } = jump else {
                unreachable!()
            };
            crate::heap().root(value);
            Ok(value)
        }
        other => other,
    }
}

/// Runs the `on.exit` expressions of frame `depth` (one-based), keeping the visibility of the
/// value. An unwind from an `on.exit` expression replaces the frame's own result.
unsafe fn run_on_exit(depth: usize, result: R<Sexp>) -> R<Sexp> {
    let frame = &mut interp().frames[depth - 1];
    if frame.on_exit.is_empty() {
        return result;
    }
    let expressions = std::mem::take(&mut frame.on_exit);
    // A slot frame keeps exit code compiled for its slots, which runs there without an
    // environment; only an expression that did not compile needs one.
    let (function, header) = (frame.function, frame.slots);
    // Other exit code, such as the call `withr::defer` registers through `do.call`, runs in
    // the frame's pending environment, which reifies only if the code reads a binding.
    let env = if frame.env != nil()
        || expressions
            .iter()
            .all(|expression| expression.kind() == SexpType::Bytecode)
    {
        frame.env
    } else if frame.slots == crate::NO_SLOTS {
        frame_env(depth - 1, Reify::Site("on.exit"))?
    } else {
        pending_env(depth - 1)?
    };
    let visible = interp().visible;
    let mark = crate::heap().handle_mark();
    for &expression in &expressions {
        crate::heap().root(expression);
    }
    // A `return(x)` unwinding to this frame is the value the function returns.
    let returned = match &result {
        Ok(value) => Some(*value),
        Err(Unwind::Jump(jump)) => match *jump {
            Jump::Return { env: target, value } if target == env => Some(value),
            _ => None,
        },
        Err(_) => None,
    };
    interp().returned_values.push(returned);
    let mut outcome = result;
    for expression in expressions {
        // A handler stack that a condition left in place must not run the exit code.
        let saved_handlers = interp().handlers.clone();
        interp().handlers = interp().frames[depth - 1].handlers.clone();
        let exit = if expression.kind() == SexpType::Bytecode {
            bytecode::run_promise_on_slots(expression, function, depth - 1, header as usize)
        } else {
            eval(expression, env)
        };
        interp().handlers = saved_handlers;
        match exit {
            // `on.exit(return(x))` makes `x` the frame's value, even over an error unwinding
            // through it, as in GNU R.
            Err(Unwind::Jump(jump)) if matches!(jump, Jump::Return { env: target, .. } if target == env) => {
                if let Jump::Return { value, .. } = jump {
                    outcome = Ok(value);
                    if let Some(top) = interp().returned_values.last_mut() {
                        *top = Some(value);
                    }
                }
            }
            Err(unwind) => {
                outcome = Err(unwind);
                break;
            }
            Ok(_) => {}
        }
    }
    interp().returned_values.pop();
    crate::heap().release_handles(mark);
    match &outcome {
        Ok(value) => crate::heap().root(*value),
        Err(unwind) => unwind.root(),
    }
    interp().visible = visible;
    outcome
}

/// GNU R's argument matching: exact names, then partial names for the formals before `...`, then
/// positions. Returns one value per formal: the supplied value or promise, `R_MissingArg`, or
/// for `...` a `DOTSXP` of what it collects.
pub unsafe fn match_args(formals: Sexp, args: &[Arg], call: Sexp) -> R<Matched> {
    if let Some(matched) = match_positional_args(formals, args, missing_arg(), symbols().dots)? {
        return Ok(matched);
    }
    let plan = match_plan(formals, args, call)?;
    matched_from_plan(formals, args, &plan)
}

/// Which formal each argument binds to: an argument that `...` collects maps to the position of
/// `...`. The plan depends only on the formals and the argument names, so a call site that
/// passes the same names to the same function reuses it ([`match_args_cached`]).
type Plan = SmallVec<[u32; 8]>;

/// [`match_args`] for a call of the compiled closure `function`, reusing the plan of the last
/// call of its body when it had the same formals and argument names. The cache holds one entry
/// in the body's `MATCHES` constant: a list of the formals, the plan, and the argument tags.
unsafe fn match_args_cached(function: Sexp, formals: Sexp, args: &[Arg], call: Sexp) -> R<Matched> {
    let code = function.cdr();
    if code.kind() != SexpType::Bytecode || !args.iter().any(is_named) {
        return match_args(formals, args, call);
    }
    let constants = code.cdr();
    let entry = constants.element(bytecode::MATCHES);
    if entry != nil()
        && entry.element(0) == formals
        && length(entry) == args.len() + 2
        && args
            .iter()
            .enumerate()
            .all(|(index, arg)| entry.element(index + 2) == arg.tag)
    {
        let plan: Plan = entry
            .element(1)
            .slice::<i32>()
            .iter()
            .map(|&formal| formal as u32)
            .collect();
        return matched_from_plan(formals, args, &plan);
    }
    let plan = match_plan(formals, args, call)?;
    let stored: SmallVec<[i32; 8]> = plan.iter().map(|&formal| formal as i32).collect();
    let stored = values::integer_vector(&stored)?;
    let entry = alloc(SexpType::List, args.len() + 2)?;
    let heap = crate::heap();
    heap.set_element(entry, 0, formals);
    heap.set_element(entry, 1, stored);
    for (index, arg) in args.iter().enumerate() {
        heap.set_element(entry, index + 2, arg.tag);
    }
    heap.set_element(constants, bytecode::MATCHES, entry);
    matched_from_plan(formals, args, &plan)
}

fn is_named(arg: &Arg) -> bool {
    unsafe { arg.tag.kind() == SexpType::Symbol && !arg.tag.car().bytes().is_empty() }
}

unsafe fn match_plan(formals: Sexp, args: &[Arg], call: Sexp) -> R<Plan> {
    let dots_symbol = symbols().dots;
    // Every closure call matches its arguments, so the working state stays off the heap for the
    // usual handful of formals and arguments.
    let mut formal_symbols: SmallVec<[Sexp; 8]> = SmallVec::new();
    let mut cell = formals;
    while matches!(
        cell.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        formal_symbols.push(cell.tag());
        cell = cell.cdr();
    }
    let count = formal_symbols.len();
    let mut matched: SmallVec<[bool; 8]> = smallvec![false; count];
    let mut plan: Plan = smallvec![0; args.len()];
    let mut used: SmallVec<[bool; 8]> = smallvec![false; args.len()];
    let dots_position = formal_symbols
        .iter()
        .position(|&symbol| symbol == dots_symbol);
    let arg_names: SmallVec<[Option<&[u8]>; 8]> = args
        .iter()
        .map(|arg| {
            if arg.tag.kind() == SexpType::Symbol {
                let name = arg.tag.car().bytes();
                (!name.is_empty()).then_some(name)
            } else {
                None
            }
        })
        .collect();
    // Exact matches.
    for (formal_index, &formal) in formal_symbols.iter().enumerate() {
        if formal == dots_symbol {
            continue;
        }
        let formal_name = formal.car().bytes();
        for (arg_index, name) in arg_names.iter().enumerate() {
            if let Some(name) = name
                && *name == formal_name
            {
                if matched[formal_index] {
                    return Err(error_in(
                        call,
                        format!(
                            "formal argument \"{}\" matched by multiple actual arguments",
                            symbol_name(formal)
                        ),
                    ));
                }
                matched[formal_index] = true;
                plan[arg_index] = formal_index as u32;
                used[arg_index] = true;
            }
        }
    }
    // Partial matches, only for formals before `...`.
    let partial_limit = dots_position.unwrap_or(count);
    let mut partial_owner: SmallVec<[Option<usize>; 8]> = smallvec![None; args.len()];
    for (formal_index, &formal) in formal_symbols.iter().enumerate().take(partial_limit) {
        if matched[formal_index] {
            continue;
        }
        let formal_name = formal.car().bytes();
        for (arg_index, name) in arg_names.iter().enumerate() {
            if used[arg_index] && partial_owner[arg_index].is_none() {
                continue;
            }
            if let Some(name) = name
                && formal_name.starts_with(name)
            {
                if let Some(owner) = partial_owner[arg_index]
                    && owner != formal_index
                {
                    return Err(error_in(
                        call,
                        format!(
                            "argument {} matches multiple formal arguments",
                            arg_index + 1
                        ),
                    ));
                }
                if matched[formal_index] {
                    return Err(error_in(
                        call,
                        format!(
                            "formal argument \"{}\" matched by multiple actual arguments",
                            symbol_name(formal)
                        ),
                    ));
                }
                matched[formal_index] = true;
                plan[arg_index] = formal_index as u32;
                used[arg_index] = true;
                partial_owner[arg_index] = Some(formal_index);
            }
        }
    }
    // Positional matches up to `...`; everything left goes to `...`.
    let mut unused: SmallVec<[usize; 4]> = SmallVec::new();
    let mut formal_index = 0;
    for arg_index in 0..args.len() {
        if used[arg_index] {
            continue;
        }
        if arg_names[arg_index].is_some() {
            if let Some(position) = dots_position {
                plan[arg_index] = position as u32;
            } else {
                unused.push(arg_index);
            }
            continue;
        }
        while formal_index < count
            && formal_symbols[formal_index] != dots_symbol
            && matched[formal_index]
        {
            formal_index += 1;
        }
        if formal_index < count && formal_symbols[formal_index] != dots_symbol {
            matched[formal_index] = true;
            plan[arg_index] = formal_index as u32;
            formal_index += 1;
        } else if let Some(position) = dots_position {
            plan[arg_index] = position as u32;
        } else {
            unused.push(arg_index);
        }
    }
    if !unused.is_empty() {
        let texts: Vec<String> = unused
            .iter()
            .map(|&index| {
                let arg = &args[index];
                let code = if arg.value.kind() == SexpType::Promise {
                    arg.value.cdr()
                } else {
                    arg.value
                };
                let text = deparse::deparse_one_line(code);
                match arg_names[index] {
                    Some(name) => format!("{} = {text}", String::from_utf8_lossy(name)),
                    None => text,
                }
            })
            .collect();
        return Err(error_in(
            call,
            format!(
                "unused argument{} ({})",
                if texts.len() == 1 { "" } else { "s" },
                texts.join(", ")
            ),
        ));
    }
    Ok(plan)
}

/// One value per formal from a plan: the argument mapped to it, the arguments mapped to `...`
/// in order as a `DOTSXP`, or `R_MissingArg`.
unsafe fn matched_from_plan(formals: Sexp, args: &[Arg], plan: &[u32]) -> R<Matched> {
    let missing = missing_arg();
    let dots_symbol = symbols().dots;
    let mut result = Matched::new();
    let mut dots_position = None;
    let mut cell = formals;
    while matches!(
        cell.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        if cell.tag() == dots_symbol {
            dots_position = Some(result.len() as u32);
        }
        result.push(missing);
        cell = cell.cdr();
    }
    for (arg, &formal) in args.iter().zip(plan) {
        if Some(formal) != dots_position
            && let Some(slot) = result.get_mut(formal as usize)
        {
            *slot = arg.value;
        }
    }
    if let Some(position) = dots_position {
        let mut list = nil();
        for (arg, _) in args
            .iter()
            .zip(plan)
            .rev()
            .filter(|(_, formal)| **formal == position)
        {
            list = triple(SexpType::Dots, arg.value, list, arg.tag)?;
        }
        if list != nil()
            && let Some(slot) = result.get_mut(position as usize)
        {
            *slot = list;
        }
    }
    Ok(result)
}

/// The common case of `match_args` without its bookkeeping: no argument is named, so they fill
/// the formals in order up to `...`, which collects the rest. Returns `None` when an argument is
/// named or one is left over, which the full matcher handles and reports.
unsafe fn match_positional_args(
    formals: Sexp,
    args: &[Arg],
    missing: Sexp,
    dots_symbol: Sexp,
) -> R<Option<Matched>> {
    if args.iter().any(is_named) {
        return Ok(None);
    }
    let mut result = Matched::new();
    let mut remaining = args.iter();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == dots_symbol {
            let rest = remaining.as_slice();
            let mut list = nil();
            for arg in rest.iter().rev() {
                list = triple(SexpType::Dots, arg.value, list, arg.tag)?;
            }
            result.push(if rest.is_empty() { missing } else { list });
            remaining = [].iter();
        } else {
            result.push(remaining.next().map_or(missing, |arg| arg.value));
        }
        formal = formal.cdr();
    }
    Ok(remaining.as_slice().is_empty().then_some(result))
}

/// Whether `symbol` is missing in `env`: unbound to a supplied argument, or bound to a promise of
/// a symbol that is itself missing in the caller.
pub unsafe fn is_missing(symbol: Sexp, env: Sexp) -> R<bool> {
    symbol_is_missing(symbol, env, true)
}

/// Whether an argument expression is a symbol bound in `env`'s own frame to a missing argument,
/// as GNU R's `R_isMissing` asks for the subset primitives: `f <- function(m, i) m[i, 1]` called
/// as `f(m, )` passes an empty `i`, which selects everything. A defaulted formal is not missing,
/// and an unbound symbol is left to evaluation to report.
pub unsafe fn argument_is_missing(expression: Sexp, env: Sexp) -> R<bool> {
    if expression.kind() != SexpType::Symbol || expression == symbols().dots {
        return Ok(false);
    }
    let symbol = if dot_dot_index(expression).is_some() {
        symbols().dots
    } else {
        expression
    };
    if crate::heap().frame_cell(env, symbol).is_none() {
        return Ok(false);
    }
    symbol_is_missing(expression, env, false)
}

/// `missing()` of `symbol` in `env`. `defaults_count` says whether a formal that got its default
/// counts as missing: it does for the function asking, but not for a caller its argument came
/// from, so `h(where)` with `where` defaulted passes a non-missing argument to `h`.
unsafe fn symbol_is_missing(symbol: Sexp, env: Sexp, defaults_count: bool) -> R<bool> {
    // `eval.parent(substitute(missing(x)))` with `x` bound to an empty argument asks about the
    // empty symbol itself, which GNU R reports as missing.
    if symbol == missing_arg() {
        return Ok(true);
    }
    let heap = crate::heap();
    let (symbol, index) = match dot_dot_index(symbol) {
        Some(index) => (symbols().dots, Some(index)),
        None => (symbol, None),
    };
    let Some(cell) = heap.frame_cell(env, symbol) else {
        return Err(error(format!(
            "'missing({})' did not find an argument",
            symbol_name(symbol)
        )));
    };
    if let Some(index) = index {
        let dots = cell.cdr();
        if dots == missing_arg() || dots.kind() != SexpType::Dots {
            return Ok(true);
        }
        let mut item = dots;
        for _ in 1..index {
            item = item.cdr();
            if item.kind() != SexpType::Dots {
                return Ok(true);
            }
        }
        return value_is_missing(item.car());
    }
    if cell.has_gp(gp::BINDING_ABSENT) || (defaults_count && cell.has_gp(gp::BINDING_MISSING)) {
        return Ok(true);
    }
    value_is_missing(cell.cdr())
}

pub(crate) unsafe fn value_is_missing(value: Sexp) -> R<bool> {
    if value == missing_arg() {
        return Ok(true);
    }
    if is_reference(value) && value.car() == unbound() {
        return reference_is_missing(value);
    }
    if value.kind() == SexpType::Promise
        && value.car() == unbound()
        && value.cdr().kind() == SexpType::Symbol
        && value.tag().kind() == SexpType::Environment
    {
        let code = value.cdr();
        // A promise over a slot frame that has no environment yet reads the frame's slot, as
        // reifying the frame and asking its binding would, without making the environment.
        if value.tag().has_gp(gp::PENDING_FRAME) && dot_dot_index(code).is_none() {
            return pending_slot_is_missing(value.tag(), code);
        }
        let caller = promise_env(value)?;
        // A promise for `..1` asks about the caller's `...`, the binding that holds it.
        let bound = if dot_dot_index(code).is_some() {
            symbols().dots
        } else {
            code
        };
        if crate::heap().frame_cell(caller, bound).is_some() {
            return symbol_is_missing(code, caller, false);
        }
    }
    Ok(false)
}

/// [`symbol_is_missing`] of `symbol` in the slot frame whose pending environment is `pending`,
/// for a caller the argument came from, so a defaulted formal is not missing. A reified slot
/// frame binds each slot to its content and the method variables after them, so reading them in
/// that order asks what its environment would answer.
unsafe fn pending_slot_is_missing(pending: Sexp, symbol: Sexp) -> R<bool> {
    let state = interp();
    let owner = pending_owner(pending);
    let frame = &state.frames[owner];
    let names = frame
        .function
        .cdr()
        .cdr()
        .element(bytecode::SLOTS)
        .element(1);
    if let Some(slot) = (0..values::length(names)).find(|&slot| names.element(slot) == symbol) {
        let value = *bytecode::register_address(frame.slots as usize + 1 + slot);
        return if value == unbound() {
            Ok(false)
        } else {
            value_is_missing(value)
        };
    }
    let bound = crate::method_vars(state, owner)
        .iter()
        .find(|(name, _)| *name == symbol)
        .map(|&(_, value)| value);
    match bound {
        Some(value) => value_is_missing(value),
        None => Ok(false),
    }
}

// The control-flow specials.

pub const BUILTINS: &[BuiltinDef] = &[
    special("{", -1, do_begin).keep_visibility(),
    special("(", 1, do_paren),
    special("if", -1, do_if).keep_visibility(),
    special("for", -1, |call| unsafe { in_loop(call, do_for) }).invisible(),
    special("while", -1, |call| unsafe { in_loop(call, do_while) }).invisible(),
    special("repeat", -1, |call| unsafe { in_loop(call, do_repeat) }).invisible(),
    special("break", 0, do_break),
    special("next", 0, do_next),
    special("return", -1, do_return),
    special("function", -1, do_function),
    special("<-", 2, do_assign).invisible(),
    special("=", 2, do_assign).invisible(),
    special("<<-", 2, do_assign).invisible(),
    special("quote", 1, do_quote),
    special("&&", 2, do_and2),
    special("||", 2, do_or2),
    special("switch", -1, do_switch).keep_visibility(),
    special("missing", 1, do_missing),
    special("on.exit", -1, do_on_exit).invisible(),
    special(".Internal", 1, do_internal).keep_visibility(),
    special("substitute", -1, do_substitute),
    builtin("invisible", -1, do_invisible).invisible(),
    builtin("nargs", 0, do_nargs),
    special("forceAndCall", -1, do_force_and_call).keep_visibility(),
    special("Tailcall", -1, do_tailcall).keep_visibility(),
    special("Exec", -1, do_exec).keep_visibility(),
    special("declare", -1, |_| Ok(nil())).invisible(),
    internal("eval", 3, do_eval).keep_visibility(),
    internal("withVisible", 1, do_with_visible),
    internal("Recall", -1, do_recall).keep_visibility(),
    builtin(".Primitive", 1, do_primitive),
    internal("identity", 1, |call| Ok(call.arg(0))),
    special("~", -1, do_tilde),
    internal("delayedAssign", 4, do_delayed_assign).invisible(),
    internal("makeLazy", 5, do_make_lazy).invisible(),
    builtin("standardGeneric", -1, dispatch::do_standard_generic).keep_visibility(),
    builtin("call", -1, do_call),
    special("expression", -1, do_expression),
    internal("all.names", 4, do_all_names),
    internal("returnValue", 1, |call| {
        Ok(interp()
            .returned_values
            .last()
            .copied()
            .flatten()
            .unwrap_or_else(|| call.arg(0)))
    }),
    internal("named", 1, |call| unsafe {
        values::scalar_integer(call.arg(0).refcount().min(2) as i32)
    }),
    internal("refcnt", 1, |call| unsafe {
        values::scalar_integer(call.arg(0).refcount() as i32)
    }),
    internal("address", 1, |call| unsafe {
        values::scalar_string(&format!("{:p}", call.arg(0).as_ptr()))
    }),
    internal("builtins", 1, do_builtins),
];

unsafe fn do_call(call: &Call<'_>) -> R<Sexp> {
    let Some(first) = call.args.first() else {
        return Err(call.error("first argument must be a character string"));
    };
    let name = first.value;
    if name.kind() != SexpType::String || length(name) != 1 {
        return Err(call.error("first argument must be a character string"));
    }
    let function = values::install_charsxp(name.element(0))?;
    let mut items = vec![(nil(), function)];
    items.extend(call.args[1..].iter().map(|arg| (arg.tag, arg.value)));
    values::pairlist(&items, SexpType::Language)
}

unsafe fn do_expression(call: &Call<'_>) -> R<Sexp> {
    let result = alloc(SexpType::Expression, call.len())?;
    let mut any_names = false;
    for (index, arg) in call.args.iter().enumerate() {
        values::set_elt(result, index, arg.value);
        any_names |= arg.tag != nil();
    }
    if any_names {
        let names: Vec<String> = call
            .args
            .iter()
            .map(|arg| {
                if arg.tag == nil() {
                    String::new()
                } else {
                    symbol_name(arg.tag).into_owned()
                }
            })
            .collect();
        let names = values::string_vector(&names)?;
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// `all.names(expr, functions, max.names, unique)`.
unsafe fn do_all_names(call: &Call<'_>) -> R<Sexp> {
    let functions = values::as_logical_scalar(call.arg(1)) == 1;
    let maximum = values::as_integer_scalar(call.arg(2));
    let unique = values::as_logical_scalar(call.arg(3)) == 1;
    let mut names: Vec<String> = Vec::new();
    unsafe fn walk(expression: Sexp, functions: bool, names: &mut Vec<String>) {
        match expression.kind() {
            SexpType::Symbol => {
                let name = symbol_name(expression).into_owned();
                if !name.is_empty() {
                    names.push(name);
                }
            }
            SexpType::Language => {
                if functions || expression.car().kind() == SexpType::Language {
                    walk(expression.car(), functions, names);
                }
                let mut cell = expression.cdr();
                while cell.kind() == SexpType::Pairlist {
                    walk(cell.car(), functions, names);
                    cell = cell.cdr();
                }
            }
            SexpType::Expression => {
                for index in 0..length(expression) {
                    walk(expression.element(index), functions, names);
                }
            }
            _ => {}
        }
    }
    walk(call.arg(0), functions, &mut names);
    if unique {
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
    }
    if maximum >= 0 && names.len() > maximum as usize {
        names.truncate(maximum as usize);
    }
    values::string_vector(&names)
}

unsafe fn do_builtins(call: &Call<'_>) -> R<Sexp> {
    let internal_too = values::as_logical_scalar(call.arg(0)) == 1;
    let mut names: Vec<&str> = interp()
        .builtins
        .iter()
        .filter(|definition| {
            internal_too || matches!(definition.kind, Kind::Builtin | Kind::Special)
        })
        .map(|definition| definition.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    values::string_vector(&names)
}

unsafe fn do_begin(call: &Call<'_>) -> R<Sexp> {
    let mut value = nil();
    interp().visible = true;
    let srcrefs = values::get_attr(call.call, symbols().srcref);
    // The first srcref is the brace's own; each statement runs with its srcref current, which
    // the calls it makes record for `sys.call` and `traceback`.
    let saved = interp().srcref;
    // Nothing a statement made stays rooted while the next one runs, not even its value, so an
    // object it drops can be collected (and finalized) by a `gc()` later in the same block.
    let mark = crate::heap().handle_mark();
    let last = call.args.len().saturating_sub(1);
    for (index, arg) in call.args.iter().enumerate() {
        if srcrefs != nil() {
            interp().srcref = statement_srcref(srcrefs, index + 1);
        }
        let result = match interp().debugger.is_some() {
            true => crate::debugger::statement().and_then(|()| eval(arg.value, call.env()?)),
            false => eval(arg.value, call.env()?),
        };
        interp().srcref = saved;
        crate::heap().release_handles(mark);
        match result {
            Ok(next) => {
                if index == last {
                    crate::heap().root(next);
                }
                value = next;
            }
            Err(unwind) => {
                unwind.root();
                return Err(unwind);
            }
        }
    }
    Ok(value)
}

/// Element `index` of a `srcref` list, or `NULL` when the list has no such srcref.
pub unsafe fn statement_srcref(srcrefs: Sexp, index: usize) -> Sexp {
    if srcrefs.kind() != SexpType::List || index >= length(srcrefs) {
        return nil();
    }
    let srcref = srcrefs.element(index);
    if srcref.kind() == SexpType::Integer && length(srcref) >= 6 {
        srcref
    } else {
        nil()
    }
}

unsafe fn do_paren(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(1)?;
    if call.arg(0) != symbols().dots {
        return eval(call.arg(0), call.env()?);
    }
    // `(...)` is a builtin call in GNU R, so the dots expand into its single argument.
    let args = evaluate_args(call.call.cdr(), call.env()?, false, call.call)?;
    let expanded = Call {
        args: &args,
        ..*call
    };
    expanded.check_arity(1)?;
    Ok(expanded.arg(0))
}

/// The condition of `if` and `while`, with GNU R's messages.
#[inline]
pub unsafe fn as_condition(value: Sexp, call: Sexp) -> R<bool> {
    if let Some(flag) = plain_flag(value) {
        return Ok(flag != 0);
    }
    general_condition(value, call)
}

/// The element of a plain length-one logical vector that is not `NA`: the operand nearly every
/// condition and `&&` operand is, read without a call.
#[inline(always)]
unsafe fn plain_flag(value: Sexp) -> Option<i32> {
    if value.kind() == SexpType::Logical && !value.is_altrep() && value.length() == 1 {
        let flag = *value.slice::<i32>().get_unchecked(0);
        if flag != heap::object::NA_LOGICAL {
            return Some(flag);
        }
    }
    None
}

#[inline(never)]
unsafe fn general_condition(value: Sexp, call: Sexp) -> R<bool> {
    if crate::records::is_native(value) {
        return Err(crate::records::boundary_error(value, nil(), call));
    }
    let count = length(value);
    if count > 1 {
        return Err(error_in(call, "the condition has length > 1"));
    }
    if count == 0 {
        return Err(error_in(call, "argument is of length zero"));
    }
    match values::as_logical_scalar(value) {
        heap::object::NA_LOGICAL => {
            let message = if value.kind() == SexpType::Logical {
                "missing value where TRUE/FALSE needed"
            } else if value.kind() == SexpType::String {
                "argument is not interpretable as logical"
            } else {
                "missing value where TRUE/FALSE needed"
            };
            Err(error_in(call, message))
        }
        condition => {
            if !matches!(
                value.kind(),
                SexpType::Logical
                    | SexpType::Integer
                    | SexpType::Real
                    | SexpType::String
                    | SexpType::Complex
                    | SexpType::Raw
            ) {
                return Err(error_in(call, "argument is of length zero".to_string()));
            }
            Ok(condition != 0)
        }
    }
}

unsafe fn do_if(call: &Call<'_>) -> R<Sexp> {
    let condition = eval(call.arg(0), call.env()?)?;
    if crate::records::is_native(condition) {
        return Err(crate::records::boundary_error(
            condition,
            call.arg(0),
            call.call,
        ));
    }
    if as_condition(condition, call.call)? {
        eval(call.arg(1), call.env()?)
    } else if call.len() > 2 {
        eval(call.arg(2), call.env()?)
    } else {
        interp().visible = false;
        Ok(nil())
    }
}

/// Runs one loop iteration and reports whether the loop goes on: `Ok(false)` after `break`.
unsafe fn loop_body(body: Sexp, env: Sexp) -> R<bool> {
    // Every iteration polls, so even `repeat NULL`, whose body makes no call, can be interrupted.
    crate::unwind::poll_interrupt()?;
    let result = scope(|| eval(body, env).map(|_| ()));
    match result {
        Ok(()) => Ok(true),
        Err(Unwind::Jump(jump)) => match jump {
            Jump::Break { env: target } if target == env => Ok(false),
            Jump::Next { env: target } if target == env => Ok(true),
            other => Err(Unwind::Jump(other)),
        },
        Err(other) => Err(other),
    }
}

unsafe fn do_for(call: &Call<'_>) -> R<Sexp> {
    let env = call.env()?;
    let variable = call.arg(0);
    if variable.kind() != SexpType::Symbol {
        return Err(call.error("invalid for() loop sequence"));
    }
    let mut sequence = eval(call.arg(1), env)?;
    let body = call.arg(2);
    if values::inherits(sequence, "factor") {
        sequence = crate::coerce::factor_as_character(sequence)?;
    }
    let kind = sequence.kind();
    if kind.is_vector()
        && kind != SexpType::I64
        && values::length(sequence) >= bytecode::LOOP_COMPILE_ITERATIONS
        && let Some(value) = bytecode::run_loop(call.call, Some(sequence), env)?
    {
        return Ok(value);
    }
    // A loop that stays in the tree walker steps through a compact sequence by its first element
    // and increment, as compiled loops do; expanding `1:n` would allocate all n elements before
    // the first iteration.
    let steps = if sequence.is_altrep() {
        (interp().hooks.compact_sequence_steps)(sequence)
    } else {
        None
    };
    let sequence = match steps {
        Some(_) => sequence,
        None => values::materialize(sequence)?,
    };
    let count = match sequence.kind() {
        SexpType::Nil => 0,
        SexpType::Pairlist | SexpType::Language => values::pairlist_length(sequence),
        SexpType::I64 if !sequence.is_array() => {
            return Err(crate::records::boundary_error(
                sequence,
                call.arg(1),
                call.call,
            ));
        }
        kind if kind.is_vector() => values::length(sequence),
        SexpType::Record => {
            return Err(crate::records::boundary_error(
                sequence,
                call.arg(1),
                call.call,
            ));
        }
        _ => return Err(call.error("invalid for() loop sequence")),
    };
    // R binds the variable to NULL before the first iteration, which only shows when there is
    // none.
    if count == 0 {
        define(variable, nil(), env)?;
    }
    // The body must see the sequence as shared, so that modifying the variable it came from
    // copies it instead of changing the elements still to come.
    sequence.increment_refcount();
    let mut cell = sequence;
    let mut outcome = Ok(nil());
    let heap = crate::heap();
    heap.root(sequence);
    // Each element is rooted when it is allocated, and the binding keeps it alive after that, so
    // releasing per iteration stops a long loop from holding every element it ever made.
    let mark = heap.handle_mark();
    for index in 0..count {
        crate::heap().release_handles(mark);
        let element = match (steps, sequence.kind()) {
            (Some((first, step)), SexpType::Integer) => {
                values::scalar_integer((first + step * index as f64) as i32)
            }
            (Some((first, step)), _) => values::scalar_real(first + step * index as f64),
            (None, SexpType::List | SexpType::Expression) => Ok(sequence.element(index)),
            (None, SexpType::Pairlist | SexpType::Language) => {
                let value = cell.car();
                cell = cell.cdr();
                Ok(value)
            }
            (None, _) => crate::subset::vector_element(sequence, index),
        };
        match element
            .and_then(|element| define(variable, element, env))
            .and_then(|()| loop_body(body, env))
        {
            Ok(true) => {}
            Ok(false) => break,
            Err(unwind) => {
                outcome = Err(unwind);
                break;
            }
        }
    }
    sequence.decrement_refcount();
    outcome
}

unsafe fn do_while(call: &Call<'_>) -> R<Sexp> {
    let env = call.env()?;
    let mut iterations = 0;
    loop {
        // All of the loop's state is in `env`, so the compiled loop starts over at the condition
        // exactly where the tree walker stands between two iterations.
        if iterations == bytecode::LOOP_COMPILE_ITERATIONS
            && let Some(value) = bytecode::run_loop(call.call, None, env)?
        {
            return Ok(value);
        }
        iterations += 1;
        let condition = scope(|| eval(call.arg(0), env))?;
        if !as_condition(condition, call.call)? {
            break;
        }
        if !loop_body(call.arg(1), env)? {
            break;
        }
    }
    Ok(nil())
}

unsafe fn do_repeat(call: &Call<'_>) -> R<Sexp> {
    let env = call.env()?;
    let mut iterations = 0;
    while loop_body(call.arg(0), env)? {
        iterations += 1;
        if iterations == bytecode::LOOP_COMPILE_ITERATIONS
            && let Some(value) = bytecode::run_loop(call.call, None, env)?
        {
            return Ok(value);
        }
    }
    Ok(nil())
}

/// A loop the tree-walking interpreter runs, which a `break` or `next` evaluated in `env` can
/// reach.
#[derive(Clone, Copy)]
pub struct LoopContext {
    env: Sexp,
    /// The number of call frames when the loop started. A jump at the same depth was written
    /// in the loop's body, or in an `if` or `{` inside it.
    depth: usize,
    /// The loop's body, which decides whether a jump from another call, such as
    /// `eval(quote(break))`, reaches the loop. Such jumps are rare, so the body is only scanned
    /// when one happens.
    body: Sexp,
}

/// Runs a loop of the tree-walking interpreter with its environment on the stack of loops a
/// `break` or `next` can reach.
unsafe fn in_loop(call: &Call<'_>, run: unsafe fn(&Call<'_>) -> R<Sexp>) -> R<Sexp> {
    let body = call.args.last().map_or(nil(), |arg| arg.value);
    let context = LoopContext {
        env: call.env()?,
        depth: interp().frames.len(),
        body,
    };
    interp().loops.push(context);
    let result = run(call);
    interp().loops.pop();
    result
}

/// Whether `expression` holds a loop, outside nested functions, that GNU R's compiler gives a
/// context.
unsafe fn has_loop_context(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        match &*symbol_name(head) {
            "function" => return false,
            "for" | "while" | "repeat" => {
                let mut body = expression.cdr();
                while body.cdr().kind() == SexpType::Pairlist {
                    body = body.cdr();
                }
                if !skips_loop_context(body.car(), true) {
                    return true;
                }
            }
            _ => {}
        }
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
        if has_loop_context(cell.car()) {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

/// Whether a loop with this body runs without a context. GNU R's compiler
/// (`checkSkipLoopCntxt`) gives a loop a context, which a `break` or `next`
/// from another call can find, only when the body calls `eval`, `evalq` or `source`, or writes
/// `break` or `next` inside a call other than `{`, `(` and `if`. Rscript compiles top-level
/// loops too, so a jump from a called function into a loop without one is an error there, and
/// ry follows the compiled behavior everywhere.
unsafe fn skips_loop_context(expression: Sexp, jumps_allowed: bool) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    let arguments_allow_jumps = if head.kind() == SexpType::Symbol {
        match &*symbol_name(head) {
            "break" | "next" if !jumps_allowed => return false,
            "function" | "for" | "while" | "repeat" => return true,
            "eval" | "evalq" | "source" => return false,
            "(" | "{" | "if" => jumps_allowed,
            _ => false,
        }
    } else {
        if !skips_loop_context(head, false) {
            return false;
        }
        false
    };
    let mut cell = expression.cdr();
    while cell.kind() == SexpType::Pairlist {
        if !skips_loop_context(cell.car(), arguments_allow_jumps) {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

unsafe fn do_break(call: &Call<'_>) -> R<Sexp> {
    reachable_loop(call.env()?)?;
    Err(jump(Jump::Break { env: call.env()? }))
}

unsafe fn do_next(call: &Call<'_>) -> R<Sexp> {
    reachable_loop(call.env()?)?;
    Err(jump(Jump::Next { env: call.env()? }))
}

/// GNU R raises a `break` or `next` without a loop for its environment as an error at once, so
/// `try(eval(quote(break)))` catches it. A loop of the tree-walking interpreter in `env` is
/// reachable from its own body, and from another call only when it has a context
/// (`skips_loop_context`). A compiled loop pushes no context of its own: one in the global
/// environment runs inside the tree walker's `for`, `while` or `repeat`, whose context stays
/// pushed, and one in a closure body relies on the frame, so a jump in a closure frame is
/// reachable when it is written directly in the body, or when the
/// body has a loop with a context. A jump that then leaves the closure frame without meeting a
/// loop is an error (`stray_loop_jump`).
unsafe fn reachable_loop(env: Sexp) -> R<()> {
    let state = interp();
    let depth = state.frames.len();
    let in_loop = state.loops.iter().any(|context| {
        context.env == env && (context.depth == depth || !skips_loop_context(context.body, true))
    });
    let in_closure_body = state.frames.iter().enumerate().any(|(index, frame)| {
        frame.env == env
            && frame.function.kind() == SexpType::Closure
            && (index + 1 == depth || has_loop_context(bytecode::source_body(frame.function)))
    });
    if in_loop || in_closure_body {
        return Ok(());
    }
    Err(error("no loop for break/next, jumping to top level"))
}

unsafe fn do_return(call: &Call<'_>) -> R<Sexp> {
    let value = match call.args {
        [] => nil(),
        [arg] => eval(arg.value, call.env()?)?,
        _ => return Err(call.error("multi-argument returns are not permitted")),
    };
    // GNU R looks for the function context before jumping, so a `return` with no function to
    // return from is an ordinary error that `tryCatch` can catch.
    if call.frame_index().is_none() {
        return Err(error("no function to return from, jumping to top level"));
    }
    Err(jump(Jump::Return {
        env: call.env()?,
        value,
    }))
}

unsafe fn do_function(call: &Call<'_>) -> R<Sexp> {
    make_closure(call.arg(0), call.arg(1), call.arg(2), call.env()?)
}

/// A closure of `formals` and `body` in `env`. A `function` call parsed with source references
/// carries its srcref as a fourth element, which becomes the closure's `srcref` attribute.
pub unsafe fn make_closure(formals: Sexp, body: Sexp, srcref: Sexp, env: Sexp) -> R<Sexp> {
    if !matches!(formals.kind(), SexpType::Nil | SexpType::Pairlist) {
        return Err(error("invalid formal argument list for \"function\""));
    }
    let closure = triple(SexpType::Closure, formals, body, env)?;
    if srcref != nil() {
        values::set_attr(closure, symbols().srcref, srcref)?;
    }
    moves::created_closure(closure)?;
    let state = interp();
    if let Some(frame) = state.frames.last()
        && frame.env == env
        && state.frame_closures.len() - (frame.closures as usize) < crate::FRAME_CLOSURES
    {
        state.frame_closures.push(closure);
    }
    Ok(closure)
}

unsafe fn do_quote(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(1)?;
    Ok(call.arg(0))
}

unsafe fn do_and2(call: &Call<'_>) -> R<Sexp> {
    logical_operator(call, true)
}

unsafe fn do_or2(call: &Call<'_>) -> R<Sexp> {
    logical_operator(call, false)
}

unsafe fn logical_operand(value: Sexp, call: &Call<'_>, side: &str) -> R<i32> {
    logical_operand_of(value, call.call, call.name(), side)
}

/// An operand of `&&` or `||` (`operator`) as `TRUE`, `FALSE` or `NA`, with GNU R's errors for
/// the `side` (`"x"` or `"y"`) that is not a logical scalar.
#[inline]
pub unsafe fn logical_operand_of(value: Sexp, call: Sexp, operator: &str, side: &str) -> R<i32> {
    if let Some(flag) = plain_flag(value) {
        return Ok(flag);
    }
    general_logical_operand(value, call, operator, side)
}

#[inline(never)]
unsafe fn general_logical_operand(value: Sexp, call: Sexp, operator: &str, side: &str) -> R<i32> {
    if !matches!(
        value.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Nil
            | SexpType::String
            | SexpType::Raw
    ) {
        return Err(error_in(
            call,
            format!("invalid '{side}' type in 'x {operator} y'"),
        ));
    }
    let count = length(value);
    if count > 1 {
        return Err(error_in(
            call,
            format!("'length = {count}' in coercion to 'logical(1)'"),
        ));
    }
    if value.kind() == SexpType::String {
        let result = values::as_logical_scalar(value);
        if count == 1 && result == heap::object::NA_LOGICAL {
            return Err(error_in(
                call,
                format!("invalid '{side}' type in 'x {operator} y'"),
            ));
        }
        return Ok(result);
    }
    Ok(values::as_logical_scalar(value))
}

unsafe fn logical_operator(call: &Call<'_>, is_and: bool) -> R<Sexp> {
    let left = eval(call.arg(0), call.env()?)?;
    let left = logical_operand(left, call, "x")?;
    let na = heap::object::NA_LOGICAL;
    if is_and && left == 0 {
        return values::scalar_logical(0);
    }
    if !is_and && left == 1 {
        return values::scalar_logical(1);
    }
    let right = eval(call.arg(1), call.env()?)?;
    let right = logical_operand(right, call, "y")?;
    let result = if is_and {
        if right == 0 {
            0
        } else if left == na || right == na {
            na
        } else {
            1
        }
    } else if right == 1 {
        1
    } else if left == na || right == na {
        na
    } else {
        0
    };
    values::scalar_logical(result)
}

unsafe fn do_switch(call: &Call<'_>) -> R<Sexp> {
    let Some(first) = call.args.first() else {
        return Err(call.error("'EXPR' is missing"));
    };
    let selector = eval(first.value, call.env()?)?;
    // `switch(x, ...)` forwards the caller's alternatives, so `...` expands to its promises.
    let mut alternatives: Vec<Arg> = Vec::with_capacity(call.args.len() - 1);
    for arg in &call.args[1..] {
        if arg.value == symbols().dots && arg.tag == nil() {
            let dots = find_var(symbols().dots, call.env()?)?;
            if dots.kind() == SexpType::Dots {
                crate::heap().root(dots);
                for (tag, value) in values::pairlist_items(dots) {
                    alternatives.push(Arg { tag, value });
                }
            }
        } else {
            alternatives.push(*arg);
        }
    }
    match switch_alternative(selector, call.call, &alternatives)? {
        Some(chosen) => eval(chosen, call.env()?),
        None => {
            interp().visible = false;
            Ok(nil())
        }
    }
}

/// The alternative `switch(selector, ...)` evaluates, given the selector's value, or `None`
/// when it returns an invisible `NULL`. `call` is the `switch` call, which a warning names.
pub(crate) unsafe fn switch_alternative(
    selector: Sexp,
    call: Sexp,
    alternatives: &[Arg],
) -> R<Option<Sexp>> {
    let selector = switch_selector(selector, call)?;
    // The warning is about the call as written: `switch(x, ...)` names its alternatives even
    // when the caller's `...` turns out to be empty.
    if values::pairlist_length(call.cdr()) < 2 {
        crate::conditions::warning_call(call, "'switch' with no alternatives")?;
    }
    let missing = missing_arg();
    let text = match selector {
        SwitchSelector::Name(text) => text,
        SwitchSelector::Position(index) => {
            if index >= 1 && (index as usize) <= alternatives.len() {
                let arg = alternatives[index as usize - 1];
                if arg.value == missing {
                    return Err(error("empty alternative in numeric switch"));
                }
                return Ok(Some(arg.value));
            }
            return Ok(None);
        }
    };
    // GNU R rejects a second default before it looks for a match, naming the calling function
    // rather than the `switch`.
    let defaults: Vec<&Arg> = alternatives
        .iter()
        .filter(|arg| arg.tag == nil() || symbol_name(arg.tag).is_empty())
        .collect();
    if defaults.len() > 1 {
        return Err(error(
            "duplicate 'switch' defaults: '".to_string()
                + &deparse::deparse_one_line(defaults[0].value)
                + "' and '"
                + &deparse::deparse_one_line(defaults[1].value)
                + "'",
        ));
    }
    for (position, arg) in alternatives.iter().enumerate() {
        if arg.tag.kind() == SexpType::Symbol && switch_name_matches(arg.tag.car(), text) {
            // Fall through empty alternatives to the next non-empty one, and to an invisible
            // NULL when every later one is empty.
            return Ok(alternatives[position..]
                .iter()
                .find(|later| later.value != missing)
                .map(|later| later.value));
        }
    }
    match defaults.first() {
        Some(default) if default.value == missing => Err(error(SWITCH_EMPTY_DEFAULT)),
        Some(default) => Ok(Some(default.value)),
        None => Ok(None),
    }
}

/// What a `switch` selector chooses by.
pub(crate) enum SwitchSelector {
    /// The string's `CHARSXP`. `NA` selects an alternative named `NA`, since GNU R compares
    /// the translated text.
    Name(Sexp),
    /// The 1-based position of a numeric selector, or a value outside the alternatives.
    Position(i32),
}

/// The error GNU R's AST `switch` raises when no name matches and the default is empty, from
/// evaluating the empty argument. Its byte compiler returns an invisible `NULL` instead; code
/// that relies on that already fails on a closure's first, uncompiled call.
pub(crate) const SWITCH_EMPTY_DEFAULT: &str = "argument is missing, with no default";

/// Checks a `switch` selector and reads it, with GNU R's warnings for a factor and for a
/// number that does not fit an integer. Errors and the factor warning name `call`, the
/// `switch`; the coercion warnings name the calling function, as in GNU R.
pub(crate) unsafe fn switch_selector(selector: Sexp, call: Sexp) -> R<SwitchSelector> {
    if !selector.kind().is_vector() || length(selector) != 1 {
        return Err(error_in(call, "EXPR must be a length 1 vector"));
    }
    if selector.kind() == SexpType::String {
        return Ok(SwitchSelector::Name(selector.element(0)));
    }
    if values::inherits(selector, "factor") {
        crate::conditions::warning_call(
            call,
            "EXPR is a \"factor\", treated as integer.\n Consider using 'switch(as.character( * ), ...)' instead.",
        )?;
    }
    let mut warnings = CoercionWarnings::default();
    let position = match selector.kind() {
        SexpType::Real => {
            let (position, out_of_range) = real_to_int(values::reals(selector)[0]);
            warnings.integer_range = out_of_range;
            position
        }
        SexpType::Complex => {
            let value = values::complexes(selector)[0];
            let (position, out_of_range) = real_to_int(value.r);
            warnings.integer_range = out_of_range;
            warnings.imaginary = !value.r.is_nan() && !value.i.is_nan() && value.i != 0.0;
            position
        }
        _ => values::as_integer_scalar(selector),
    };
    warnings.report()?;
    Ok(SwitchSelector::Position(position))
}

/// Whether the alternative named `name` is the one the string selector `text` picks. GNU R
/// compares the texts as `pmatch` does, translated to UTF-8 and with `NA` spelled "NA", so a
/// latin1 selector picks its UTF-8 name and `NA_character_` picks an alternative named `NA`.
pub(crate) unsafe fn switch_name_matches(name: Sexp, text: Sexp) -> bool {
    if text == values::na_string() {
        return name.bytes() == b"NA";
    }
    values::same_string(name, text)
}

unsafe fn do_missing(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(1)?;
    let mut symbol = call.arg(0);
    if symbol.kind() == SexpType::String && length(symbol) == 1 {
        symbol = values::install_charsxp(symbol.element(0))?;
    }
    if symbol.kind() != SexpType::Symbol {
        return Err(call.error("invalid use of 'missing'"));
    }
    values::scalar_bool(is_missing(symbol, call.env()?)?)
}

unsafe fn do_on_exit(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["expr", "add", "after"])?;
    let expression = crate::builtins::present(matched[0]).unwrap_or(nil());
    let add = match crate::builtins::present(matched[1]) {
        Some(value) => values::as_logical_scalar(eval(value, call.env()?)?) == 1,
        None => false,
    };
    let after = match crate::builtins::present(matched[2]) {
        Some(value) => values::as_logical_scalar(eval(value, call.env()?)?) != 0,
        None => true,
    };
    let Some(index) = call.frame_index() else {
        return Ok(nil());
    };
    let frame = &mut interp().frames[index];
    if !add {
        frame.on_exit.clear();
    }
    if expression != nil() {
        if after {
            frame.on_exit.push(expression);
        } else {
            frame.on_exit.insert(0, expression);
        }
    }
    Ok(nil())
}

unsafe fn do_internal(call: &Call<'_>) -> R<Sexp> {
    let inner = call.arg(0);
    if inner.kind() != SexpType::Language || inner.car().kind() != SexpType::Symbol {
        return Err(call.error("invalid .Internal() argument"));
    }
    let Some(function) = crate::builtins::internal_function(inner.car()) else {
        return Err(call.error(format!(
            "there is no .Internal function '{}'",
            symbol_name(inner.car())
        )));
    };
    let definition = builtin_def(function)?;
    let args = if function.kind() == SexpType::Builtin {
        evaluate_args(inner.cdr(), call.env()?, definition.keep_missing, inner)?
    } else {
        unevaluated_args(inner.cdr())
    };
    if definition.generic != Generic::None
        && let Some(value) =
            dispatch::try_builtin_dispatch(definition, inner, function, &args, call.env()?)?
    {
        return Ok(value);
    }
    call_builtin(definition, inner, function, &args, call.env()?)
}

unsafe fn do_invisible(call: &Call<'_>) -> R<Sexp> {
    Ok(call.arg(0))
}

unsafe fn do_nargs(call: &Call<'_>) -> R<Sexp> {
    let state = interp();
    let count = call.frame_index().map_or(NA_INTEGER, |index| {
        crate::frame_args(state, index).len() as i32
    });
    // Outside any closure, such as at top level, GNU R finds no call to count and answers NA.
    values::scalar_integer(count)
}

unsafe fn do_force_and_call(call: &Call<'_>) -> R<Sexp> {
    let count = values::as_integer_scalar(eval(call.arg(0), call.env()?)?).max(0) as usize;
    let function = eval(call.arg(1), call.env()?)?;
    let heap = crate::heap();
    heap.root(function);
    // The callee's arguments are the call's own cells after `n` and `FUN` whenever the special
    // received them unchanged, which is every call written in source, so `Reduce` and the
    // apply family pay no pairlist per call. GNU R conses the same tail into one new call.
    let rest = match call_tail(call.call, &call.args[2..]) {
        Some(rest) => rest,
        None => values::pairlist(
            &call.args[2..]
                .iter()
                .map(|arg| (arg.tag, arg.value))
                .collect::<SmallVec<[(Sexp, Sexp); 8]>>(),
            SexpType::Pairlist,
        )?,
    };
    heap.root(rest);
    let inner_call = values::lcons(call.arg(1), rest)?;
    heap.root(inner_call);
    match function.kind() {
        SexpType::Closure => {
            let mut args = Args::new();
            // As for a compiled call (see `op_call_generic`): a variable passes its value, or
            // the caller's own promise for it, when no argument can assign and neither the
            // callee nor a handler can look at the promise before it is forced.
            if no_calls(rest)
                && !crate::effects::calling_handler_established()
                && crate::effects::effect_free(function)?
            {
                eager_args_into(rest, call.env()?, &mut args)?;
            } else {
                promise_args_into(rest, call.env()?, &mut args)?;
            }
            for arg in args.iter().take(count) {
                if arg.value.kind() == SexpType::Promise {
                    force_promise(arg.value)?;
                }
            }
            apply_closure_owning(inner_call, function, &args, call.env()?, None)
        }
        _ => apply_call(inner_call, function, rest, call.env()?),
    }
}

/// Whether no argument expression in the pairlist `arguments` is a call.
unsafe fn no_calls(mut arguments: Sexp) -> bool {
    while arguments.kind() == SexpType::Pairlist {
        if matches!(
            arguments.car().kind(),
            SexpType::Language | SexpType::Bytecode | SexpType::Promise
        ) {
            return false;
        }
        arguments = arguments.cdr();
    }
    true
}

/// The cells of `call` after its first two arguments, when they hold exactly `args`.
unsafe fn call_tail(call: Sexp, args: &[Arg]) -> Option<Sexp> {
    let mut cell = call.cdr();
    for _ in 0..2 {
        if cell.kind() != SexpType::Pairlist {
            return None;
        }
        cell = cell.cdr();
    }
    let tail = cell;
    for arg in args {
        if cell.kind() != SexpType::Pairlist || cell.car() != arg.value || cell.tag() != arg.tag {
            return None;
        }
        cell = cell.cdr();
    }
    (cell == nil()).then_some(tail)
}

/// `Tailcall(FUN, ...)`, R 4.4's request to call `FUN(...)` in place of the calling frame.
/// GNU R only reuses the frame when the call is in tail position and no `on.exit` is pending,
/// and otherwise evaluates the call normally, which is what this does every time: the value is
/// the same, but deep recursion still grows the stack.
unsafe fn do_tailcall(call: &Call<'_>) -> R<Sexp> {
    let Some((function, rest)) = call.args.split_first() else {
        return Err(error("argument \"FUN\" is missing, with no default"));
    };
    if function.value == values::missing_arg() {
        return Err(error("argument \"FUN\" is missing, with no default"));
    }
    let function = if function.value.kind() == SexpType::String && length(function.value) == 1 {
        values::install(&values::string_at(function.value, 0).unwrap_or_default())?
    } else {
        function.value
    };
    let rest = values::pairlist(
        &rest
            .iter()
            .map(|arg| (arg.tag, arg.value))
            .collect::<Vec<_>>(),
        SexpType::Pairlist,
    )?;
    eval(values::lcons(function, rest)?, call.env()?)
}

/// `Exec(expr, envir)`: evaluates the call `expr` in `envir`, by default the caller's frame,
/// as a tail call in the same sense as [`do_tailcall`].
unsafe fn do_exec(call: &Call<'_>) -> R<Sexp> {
    let mut expr = None;
    let mut envir = None;
    let mut positional = Vec::new();
    for arg in call.args {
        match arg.tag {
            tag if tag != nil() && values::symbol_name(tag) == "expr" => expr = Some(arg.value),
            tag if tag != nil() && values::symbol_name(tag) == "envir" => envir = Some(arg.value),
            _ => positional.push(arg.value),
        }
    }
    let mut positional = positional.into_iter();
    let expr = expr.or_else(|| positional.next());
    let envir = envir.or_else(|| positional.next());
    if positional.next().is_some() {
        return Err(error("unused argument"));
    }
    let Some(expr) = expr.filter(|expr| *expr != values::missing_arg()) else {
        return Err(error("argument \"expr\" is missing, with no default"));
    };
    let mut expr = eval(expr, call.env()?)?;
    if expr.kind() == SexpType::Expression && length(expr) == 1 {
        expr = values::list_elt(expr, 0);
    }
    if expr.kind() != SexpType::Language {
        return Err(error("\"expr\" must be a call expression"));
    }
    let env = match envir.filter(|envir| *envir != values::missing_arg()) {
        Some(envir) => eval(envir, call.env()?)?,
        None => call.env()?,
    };
    eval(expr, env)
}

/// `R_forceAndCall`: evaluates `call` in `env`, forcing the promises of a closure's first
/// `count` arguments before its body runs. The apply family calls `FUN` this way, so a closure
/// that captures its argument sees this iteration's element, not the last one.
pub unsafe fn force_and_call(call: Sexp, count: usize, env: Sexp) -> R<Sexp> {
    force_and_call_with(call, count, env, None, None)
}

/// Calls `function` for `call`, whose arguments are untagged expressions that already
/// evaluated to `values` in `env`, as `forceAndCall(length(values), ...)` would after forcing
/// them: a closure receives forced promises of the expressions, and a builtin the values.
pub unsafe fn call_with_values(call: Sexp, function: Sexp, values: &[Sexp], env: Sexp) -> R<Sexp> {
    let mut args = Args::new();
    if function.kind() == SexpType::Closure {
        let mut expression = call.cdr();
        for &value in values {
            let promise = forced_promise(expression.car(), value)?;
            crate::heap().root(promise);
            args.push(Arg {
                tag: nil(),
                value: promise,
            });
            expression = expression.cdr();
        }
        return apply_closure_owning(call, function, &args, env, None);
    }
    let definition = builtin_def(function)?;
    args.extend(values.iter().map(|&value| Arg { tag: nil(), value }));
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, &args, env)?
    {
        return Ok(value);
    }
    call_builtin(definition, call, function, &args, env)
}

/// The storage type every first value of a run of [`force_and_call_with`] calls shares, as the
/// elements of one atomic vector do, with the annotation checks that type settles for the last
/// closure called. A typed `FUN` then has its argument checked once per run instead of once per
/// element.
pub struct KnownFirst {
    kind: SexpType,
    function: Option<Sexp>,
    entry: Entry,
}

impl KnownFirst {
    pub fn new(kind: SexpType) -> KnownFirst {
        KnownFirst {
            kind,
            function: None,
            entry: Entry::Unchecked,
        }
    }
}

/// [`force_and_call`] where the caller already knows the first argument's value, so its
/// promise starts out forced instead of evaluating its expression. The caller must only pass a
/// value that evaluating the expression would produce without side effects, and of the storage
/// type `known` names, if any.
pub unsafe fn force_and_call_with(
    call: Sexp,
    count: usize,
    env: Sexp,
    first: Option<Sexp>,
    known: Option<&mut KnownFirst>,
) -> R<Sexp> {
    let head = call.car();
    let function = if head.kind() == SexpType::Symbol {
        find_called_fun(head, env, call)?
    } else {
        eval(head, env)?
    };
    crate::heap().root(function);
    if function.kind() != SexpType::Closure {
        // A builtin `FUN`, or a special that evaluates its arguments, as `[[` in
        // `vapply(rows, `[[`, 0, "fee")`, takes the known first value as it is instead of
        // evaluating its expression again.
        let arguments = call.cdr();
        if let Some(value) = first
            && matches!(function.kind(), SexpType::Builtin | SexpType::Special)
            && arguments.kind() == SexpType::Pairlist
            && arguments.car() != symbols().dots
        {
            let definition = builtin_def(function)?;
            crate::heap().root(value);
            let mut args = Args::new();
            if function.kind() == SexpType::Special {
                if !definition.is_dispatch_or_eval() {
                    return apply_call(call, function, arguments, env);
                }
                return match dispatch::dispatch_evaluated_subject(
                    definition, call, function, arguments, value, env, &mut args,
                )? {
                    Some(value) => Ok(value),
                    None => call_builtin(definition, call, function, &args, env),
                };
            }
            args.push(Arg {
                tag: arguments.tag(),
                value,
            });
            evaluate_args_into(
                arguments.cdr(),
                env,
                definition.keep_missing,
                call,
                &mut args,
            )?;
            if definition.generic != Generic::None
                && let Some(value) =
                    dispatch::try_builtin_dispatch(definition, call, function, &args, env)?
            {
                return Ok(value);
            }
            return call_builtin(definition, call, function, &args, env);
        }
        return apply_call(call, function, arguments, env);
    }
    let mut args = Args::new();
    let arguments = call.cdr();
    // The known first value, as `lapply` reads `X[[i]]`, binds as a forced argument reference to
    // its expression: a forced promise of it once anything keeps it, and no allocation until.
    let mark = interp().references_used;
    let seeded = if let Some(value) = first
        && arguments.kind() == SexpType::Pairlist
        && arguments.car().kind() == SexpType::Language
    {
        let reference = env_reference(env, arguments.car())?;
        crate::heap().set_car(reference, value);
        args.push(Arg {
            tag: arguments.tag(),
            value: reference,
        });
        promise_args_into(arguments.cdr(), env, &mut args)?;
        true
    } else {
        promise_args_into(arguments, env, &mut args)?;
        false
    };
    let result = call_seeded(call, function, env, &mut args, count, seeded, known);
    release_references(mark);
    result
}

/// The rest of [`force_and_call_with`] for a closure `function`, with `args` made; a seeded
/// first argument is an argument reference, which only a slot frame may take.
unsafe fn call_seeded(
    call: Sexp,
    function: Sexp,
    env: Sexp,
    args: &mut Args,
    count: usize,
    seeded: bool,
    known: Option<&mut KnownFirst>,
) -> R<Sexp> {
    for arg in args.iter().take(count) {
        if arg.value.kind() == SexpType::Promise {
            force_promise(arg.value)?;
        }
    }
    // A lone untagged argument binds the first formal, which is what `entry_for` assumes.
    let entry = match known {
        Some(known) if seeded && args.len() == 1 && args[0].tag == nil() => {
            if known.function != Some(function) {
                known.entry = crate::annotations::entry_for(function, &[Some(known.kind)]);
                known.function = Some(function);
            }
            known.entry
        }
        _ => Entry::Unchecked,
    };
    if !matches!(entry, Entry::Checked(Some(_)))
        && let Some(frame) = bind_slots(call, function, args)?
    {
        return run_frame(call, function, frame, env, args, entry, &[]);
    }
    if !seeded {
        let frame = bind_arguments(call, function, args, None)?;
        return run_closure(call, function, frame, env, args, entry);
    }
    // Detached, the reference leaves the pool that rooted it, and `args` is not scanned by the
    // collector, while binding the frame allocates. The root lives in a handle scope of this call
    // alone, which ends once the closure has returned and its frame no longer needs the promise,
    // so a caller that makes many calls (`lapply`) keeps no root per call.
    crate::scope(|| {
        detach_references(&args[..1])?;
        let frame = bind_arguments(call, function, args, None)?;
        run_closure(call, function, frame, env, args, entry)
    })
}

unsafe fn do_eval(call: &Call<'_>) -> R<Sexp> {
    let expression = call.arg(0);
    let mut env = call.arg(1);
    let enclosure = call.arg(2);
    match env.kind() {
        SexpType::Environment => {}
        SexpType::Nil => {
            env = if enclosure.kind() == SexpType::Environment {
                enclosure
            } else {
                values::base_env()
            };
        }
        SexpType::List => {
            env = crate::envir::list_to_env(env, enclosure)?;
        }
        SexpType::Pairlist => {
            let list = values::pairlist_to_list(env)?;
            env = crate::envir::list_to_env(list, enclosure)?;
        }
        SexpType::Integer | SexpType::Real => {
            let which = values::as_integer_scalar(env);
            env = crate::envir::sys_frame(which, call.env()?)?;
        }
        _ => {
            return Err(call.error(
                "invalid 'envir' argument of type '".to_string() + env.kind().name() + "'",
            ));
        }
    }
    // `eval` runs its expression in a context of its own, as GNU R's does: it counts as a frame
    // for `sys.call` and friends, and a `return` in the expression returns from `eval`.
    let state = interp();
    let caller = state.frames.last().map_or(nil(), |frame| frame.call);
    let handlers = state.handlers.clone();
    let restarts = state.restarts.clone();
    state.calls_ever = state.calls_ever.wrapping_add(1);
    state.frames.push(Frame {
        call: caller,
        function: call.op,
        env,
        sys_parent: call.env()?,
        args: state.frame_args.len() as u32,
        on_exit: ThinVec::new(),
        handlers: handlers.clone(),
        restarts: restarts.clone(),
        srcref: state.srcref,
        method_vars: state.method_vars.len() as u32,
        closures: state.frame_closures.len() as u32,
        slots: crate::NO_SLOTS,
        missing: 0,
        caller: crate::NO_SLOTS,
        pending: nil(),
        unforced: 0,
        inlined: false,
        reified_by: crate::NOT_REIFIED,
        pending_shared: false,
        handle_mark: crate::heap().handle_mark() as u32,
        previous_owner: state.slot_owner,
    });
    let depth = state.frames.len();
    let result = scope(|| match expression.kind() {
        SexpType::Expression => eval_all(expression, env),
        SexpType::Promise => force_promise(expression),
        _ => eval(expression, env),
    });
    // `return()` may come from the body or from an `on.exit` expression, which runs in the
    // same frame and so returns from it too.
    let result = catch_return(env, run_on_exit(depth, catch_return(env, result)));
    let state = interp();
    // The frame's saved stack, which `.addGlobHands` may have rewritten.
    conditions::restore_handlers(state.frames[depth - 1].handlers.clone());
    state
        .frame_closures
        .truncate(state.frames[depth - 1].closures as usize);
    crate::truncate_frames(state, depth - 1);
    state.restarts = restarts;
    result
}

unsafe fn do_with_visible(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    let visible = interp().visible;
    let flag = values::scalar_bool(visible)?;
    values::named_list(&[("value", value), ("visible", flag)])
}

/// `Recall(...)`: calls the function that called `Recall` again, with that call's call and its
/// caller as the new frame's parent, as GNU R does. Neither frame needs an environment, so a
/// slot frame hands out its pending one.
unsafe fn do_recall(call: &Call<'_>) -> R<Sexp> {
    let outside = || call.error("'Recall' called from outside a closure");
    let own = call.frame_index().ok_or_else(outside)?;
    let caller = crate::envir::caller_index(own).ok_or_else(outside)?;
    let (caller_call, function) = (
        interp().frames[caller].call,
        interp().frames[caller].function,
    );
    apply(
        caller_call,
        function,
        call.args,
        frame_sys_parent_pending(caller)?,
    )
}

unsafe fn do_primitive(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_str(call.arg(0), "name")?;
    crate::builtins::primitive(&name)
        .ok_or_else(|| call.error("no such primitive function".to_string()))
}

/// A formula: the call itself, with class `formula` and the environment it was made in.
unsafe fn do_tilde(call: &Call<'_>) -> R<Sexp> {
    let formula = values::duplicate(call.call)?;
    if values::get_attr(formula, symbols().class) == nil() {
        let class = values::scalar_string("formula")?;
        values::set_attr(formula, symbols().class, class)?;
        let environment = symbols().dot_environment;
        values::set_attr(formula, environment, call.env()?)?;
    }
    Ok(formula)
}

unsafe fn do_delayed_assign(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_str(call.arg(0), "x")?;
    let symbol = values::install(&name)?;
    let value = call.arg(1);
    let eval_env = call.arg(2);
    let assign_env = call.arg(3);
    let promise = make_promise(value, eval_env)?;
    define(symbol, promise, assign_env)?;
    Ok(nil())
}

/// `makeLazy(vars, vals, expr, eenv, aenv)`: binds each variable to a promise of `expr` with
/// `key` bound to its value, which is how lazy-load databases install their objects.
unsafe fn do_make_lazy(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let keys = call.arg(1);
    let expression = call.arg(2);
    let eval_env = call.arg(3);
    let assign_env = call.arg(4);
    if names.kind() != SexpType::String && names != nil() {
        return Err(error("invalid first argument"));
    }
    // As in GNU R, each promise's code is a copy of `expression` with its first argument
    // replaced by the variable's key, and every promise shares `eval_env`, so `PRCODE` reads
    // `lazyLoadDBfetch(c(offset, length), datafile, compressed, envhook)`.
    for index in 0..length(names) {
        crate::scope(|| {
            let symbol = values::install_charsxp_text(names.element(index))?;
            let key = eval(crate::subset::vector_element(keys, index)?, eval_env)?;
            crate::heap().root(key);
            let expression_copy = values::duplicate(expression)?;
            let arguments = expression_copy.cdr();
            if expression_copy.kind() != SexpType::Language
                || arguments.kind() != SexpType::Pairlist
            {
                return Err(call.error("invalid lazy-load expression"));
            }
            crate::heap().set_car(arguments, key);
            let promise = make_promise(expression_copy, eval_env)?;
            define(symbol, promise, assign_env)
        })?;
    }
    Ok(nil())
}

unsafe fn do_substitute(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["expr", "env"])?;
    let expression = crate::builtins::present(matched[0]).unwrap_or(missing_arg());
    let env = match crate::builtins::present(matched[1]) {
        Some(env) => eval(env, call.env()?)?,
        None => call.env()?,
    };
    let env = match env.kind() {
        SexpType::Environment => env,
        SexpType::List => crate::envir::list_to_env(env, values::empty_env())?,
        SexpType::Nil => values::empty_env(),
        _ => return Err(call.error("invalid environment specified")),
    };
    // A bare `...` splices like an argument list, of which `substitute` keeps the first
    // element, or `NULL` when the dots are empty.
    if expression == symbols().dots {
        let wrapped = values::pairlist(&[(nil(), expression)], SexpType::Pairlist)?;
        crate::heap().root(wrapped);
        let substituted = substitute_list(wrapped, env)?;
        return Ok(if substituted == nil() {
            nil()
        } else {
            substituted.car()
        });
    }
    let result = substitute(expression, env)?;
    Ok(result)
}

/// Replaces the symbols in `expression` that are bound in `env` (the global environment is
/// never substituted from): promises by their code, `...` by its contents, and anything else
/// by its value.
pub unsafe fn substitute(expression: Sexp, env: Sexp) -> R<Sexp> {
    match expression.kind() {
        SexpType::Promise => substitute(expression.cdr(), env),
        SexpType::Symbol => {
            if env == values::global_env() || env == values::empty_env() {
                return Ok(expression);
            }
            let Some(cell) = crate::heap().frame_cell(env, expression) else {
                return Ok(expression);
            };
            let value = cell.cdr();
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return Ok(expression);
            }
            if value.kind() == SexpType::Promise {
                // A promise whose code is itself a promise, such as a replacement method's
                // `value`, stands for the innermost promise's code.
                let mut code = value.cdr();
                while code.kind() == SexpType::Promise {
                    code = code.cdr();
                }
                return Ok(code);
            }
            if value.kind() == SexpType::Dots {
                return Ok(expression);
            }
            Ok(value)
        }
        SexpType::Language => {
            let substituted = substitute_list(expression, env)?;
            Ok(substituted)
        }
        _ => Ok(expression),
    }
}

/// Substitutes each element of a call, splicing `...`.
pub unsafe fn substitute_list(list: Sexp, env: Sexp) -> R<Sexp> {
    let dots = symbols().dots;
    let mut items: Vec<(Sexp, Sexp)> = Vec::new();
    // GNU R reuses the spliced dots cells, so a call whose head comes from `...` becomes a
    // pairlist: `substitute(...())` is how code captures the dots unevaluated.
    let mut kind = list.kind();
    let mut cell = list;
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        let element = cell.car();
        if element == dots && env != values::global_env() {
            let value = crate::heap().frame_cell(env, dots).map(|cell| cell.cdr());
            match value {
                Some(value) if value.kind() == SexpType::Dots => {
                    if items.is_empty() {
                        kind = SexpType::Pairlist;
                    }
                    let mut dot = value;
                    while dot.kind() == SexpType::Dots {
                        let code = if dot.car().kind() == SexpType::Promise {
                            dot.car().cdr()
                        } else {
                            dot.car()
                        };
                        items.push((dot.tag(), code));
                        dot = dot.cdr();
                    }
                }
                Some(value) if value == missing_arg() => {}
                _ => items.push((cell.tag(), element)),
            }
        } else {
            let value = substitute(element, env)?;
            items.push((cell.tag(), value));
        }
        cell = cell.cdr();
    }
    let result = values::pairlist(&items, kind)?;
    if result != nil() && list.attrib() != nil() {
        crate::heap().set_attrib(result, list.attrib());
    }
    Ok(result)
}

// Assignment.

unsafe fn do_assign(call: &Call<'_>) -> R<Sexp> {
    if call.len() != 2 {
        return Err(call.error("invalid assignment"));
    }
    let superassign = call.call.car() == symbols().assign2;
    let mut target = call.arg(0);
    if target.kind() == SexpType::String && length(target) >= 1 {
        target = values::install_charsxp(target.element(0))?;
    }
    match target.kind() {
        SexpType::Symbol => {
            let value = match superassign {
                false => rebind(target, call.arg(1), call.env()?)?,
                true => eval(call.arg(1), call.env()?)?,
            };
            assign_symbol(target, value, call.env()?, superassign)?;
            Ok(value)
        }
        SexpType::Language => {
            let value = match superassign {
                false => rebind(target, call.arg(1), call.env()?)?,
                true => eval(call.arg(1), call.env()?)?,
            };
            complex_assign(call.call, target, value, call.env()?, superassign, &[])?;
            Ok(value)
        }
        _ => Err(call.error("invalid (do_set) left-hand side to assignment")),
    }
}

/// Evaluates `value`, the right-hand side of `target <- value`. When it calls a function with
/// `target` itself as the first argument, as in `d <- Dict.insert(d, k, v)` or
/// `state$seen <- Set.insert(state$seen, k)`, the call may update the value in place (see
/// [`rebind_call`]).
unsafe fn rebind(target: Sexp, value: Sexp, env: Sexp) -> R<Sexp> {
    let first_argument_is_target = value.kind() == SexpType::Language
        && value.car().kind() == SexpType::Symbol
        && value.cdr().kind() == SexpType::Pairlist
        && value.cdr().tag() == nil()
        && match target.kind() {
            SexpType::Symbol => value.cdr().car() == target,
            _ => same_expression(value.cdr().car(), target),
        };
    if !first_argument_is_target {
        return eval(value, env);
    }
    let result = (|| {
        crate::unwind::poll_interrupt()?;
        interp().visible = true;
        let function = find_called_fun(value.car(), env, value)?;
        crate::heap().root(function);
        rebind_call(value, function, target, env)
    })();
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        result => result,
    }
}

/// Calls `function` for `call`, the right-hand side of `target <- call` whose first argument is
/// `target`. The assignment overwrites `target`, so the value it held becomes unreachable the
/// moment the assignment completes, and when nothing else references that value the call may
/// update it in place instead of copying it. This is how `Dict.insert`, `Set.insert` and
/// `List.append` grow in amortized constant time.
///
/// A builtin that reuses its first argument is offered the value. A closure gets it moved into
/// its parameter when [`moves::call_moving`] can show nothing reads the caller's variable while
/// the closure runs, and shared, as in GNU R, otherwise.
pub unsafe fn rebind_call(call: Sexp, function: Sexp, target: Sexp, env: Sexp) -> R<Sexp> {
    match function.kind() {
        SexpType::Builtin
            if target.kind() == SexpType::Symbol && builtin_def(function)?.name == ".ry_update" =>
        {
            rebind_update(call, function, target, env)
        }
        SexpType::Builtin if builtin_def(function)?.reuses_first_argument => {
            reuse_first_argument(target, call, function, env, false)
        }
        SexpType::Closure if target.kind() == SexpType::Symbol => {
            match moves::call_moving(call, function, target, env)? {
                Some(value) => Ok(value),
                None => apply_call(call, function, call.cdr(), env),
            }
        }
        _ => apply_call(call, function, call.cdr(), env),
    }
}

/// `target <- (..target, name = value, ...)`, the update `call` of the record `target` holds.
/// When the binding holds a plain record alone, the update writes the new field values into that
/// record instead of a copy, and a field value of the form `f(target$name, ...)` is computed as
/// the right-hand side of `target$name <- f(target$name, ...)` would be ([`rebind`]), so
/// `List.append(target$name, x)` grows the field's list in place. The field values after that
/// one must be constants: code there could read the field the call has already updated.
unsafe fn rebind_update(call: Sexp, function: Sexp, target: Sexp, env: Sexp) -> R<Sexp> {
    let Some(cell) = crate::heap().frame_cell(env, target) else {
        return apply_call(call, function, call.cdr(), env);
    };
    let record = cell.cdr();
    let mut fields: SmallVec<[(usize, Sexp); 4]> = SmallVec::new();
    let mut updating = None;
    let mut item = call.cdr().cdr();
    while item.kind() == SexpType::Pairlist {
        let expression = item.car();
        let Some(index) = crate::records::plain_field(record, item.tag()) else {
            return apply_call(call, function, call.cdr(), env);
        };
        if updating.is_some()
            && matches!(
                expression.kind(),
                SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode
            )
        {
            return apply_call(call, function, call.cdr(), env);
        }
        if updating.is_none() && updates_own_field(expression, item.tag(), target) {
            updating = Some(fields.len());
        }
        fields.push((index, expression));
        item = item.cdr();
    }
    if record.refcount() != 1
        || cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED)
        || !reuse_permitted(env, record)
    {
        return apply_call(call, function, call.cdr(), env);
    }
    let heap = crate::heap();
    heap.root(record);
    for position in 0..fields.len() {
        let (index, expression) = fields[position];
        let value = if updating == Some(position) {
            rebind(expression.cdr().car(), expression, env)?
        } else {
            eval(expression, env)?
        };
        heap.root(value);
        fields[position] = (index, value);
    }
    // The field values may have rebound `target` or shared the record, which then stays as it
    // is.
    if cell.cdr() != record || record.refcount() != 1 || !reuse_permitted(env, record) {
        return crate::records::with_slots(record, &fields);
    }
    for &(index, value) in &fields {
        heap.set_record_slot(record, index, value);
    }
    Ok(record)
}

/// Whether `expression`, the value given to the field `name` in an update of the record bound to
/// `target`, is a call `g(target$name, ...)`, which [`rebind_update`] lets update the field's
/// value in place.
pub unsafe fn updates_own_field(expression: Sexp, name: Sexp, target: Sexp) -> bool {
    if expression.kind() != SexpType::Language || expression.cdr().kind() != SexpType::Pairlist {
        return false;
    }
    let first = expression.cdr().car();
    first.kind() == SexpType::Language
        && first.car() == symbols().dollar
        && first.cdr().car() == target
        && first.cdr().cdr().car() == name
        && first.cdr().cdr().cdr() == nil()
}

/// Calls the builtin `function` for `call`, the last call of a closure body that passes the
/// first parameter `target` as the first argument (`bytecode::tail_update`). Unlike
/// [`rebind_call`], nothing assigns the result to `target`, so the parameter's value may be
/// updated in place only when no one can see the parameter afterwards: a move into this frame
/// still lasts once the arguments are evaluated and no closure made in the frame can outlive it
/// (`moves::frame_private`).
pub unsafe fn tail_update(call: Sexp, function: Sexp, target: Sexp, env: Sexp) -> R<Sexp> {
    if function.kind() != SexpType::Builtin || !builtin_def(function)?.reuses_first_argument {
        return apply_call(call, function, call.cdr(), env);
    }
    reuse_first_argument(target, call, function, env, true)
}

unsafe fn reuse_first_argument(
    target: Sexp,
    call: Sexp,
    function: Sexp,
    env: Sexp,
    private_frame: bool,
) -> R<Sexp> {
    let definition = builtin_def(function)?;
    let args = evaluate_args(call.cdr(), env, definition.keep_missing, call)?;
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, &args, env)?
    {
        return Ok(value);
    }
    // Argument values are not counted, so another argument that is the subject, or one of the
    // lists holding it, would see the update.
    let reusable = args.first().map(|arg| arg.value).filter(|&subject| {
        subject.refcount() == 1
            && (!private_frame || {
                let others: Vec<Sexp> = args[1..].iter().map(|arg| arg.value).collect();
                moves::frame_private(env, &others)
            })
            && solely_held(target, subject, env)
            && !args[1..]
                .iter()
                .any(|arg| arg.value == subject || holds_on_chain(target, env, arg.value))
    });
    call_builtin_reusing(definition, call, function, &args, env, reusable)
}

/// Whether `subject`, the value of `target`, is referenced only through `target`'s local binding
/// in `env`, so updating it in place changes nothing but what `target` reads. For a symbol the
/// binding holds the value itself. For `a$b`, `a[["b"]]` or `a[[2]]`, and chains of them, each container is a plain list that the
/// one before it holds alone, down to the binding. Every value on the way must be invisible to
/// compiled code (see [`reuse_permitted`]).
unsafe fn solely_held(target: Sexp, subject: Sexp, env: Sexp) -> bool {
    match target.kind() {
        SexpType::Symbol => {
            let Some(cell) = crate::heap().frame_cell(env, target) else {
                return false;
            };
            if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED) {
                return false;
            }
            cell.cdr() == subject && reuse_permitted(env, subject)
        }
        SexpType::Language => {
            let Some(container) = held_container(target, env) else {
                return false;
            };
            held_element(container.0, container.1) == subject && reuse_permitted(env, subject)
        }
        _ => false,
    }
}

/// Whether `value` is one of the lists on the `a$b` or `a[[i]]` chain `target` updates.
unsafe fn holds_on_chain(target: Sexp, env: Sexp, value: Sexp) -> bool {
    let mut link = target;
    while link.kind() == SexpType::Language {
        match held_container(link, env) {
            Some((container, _)) if container == value => return true,
            Some(_) => link = link.cdr().car(),
            None => return false,
        }
    }
    false
}

/// The list and position `target`, an `a$b` or `a[[i]]` chain, names, when the list is held
/// only by the binding or container before it.
unsafe fn held_container(target: Sexp, env: Sexp) -> Option<(Sexp, usize)> {
    let symbols = symbols();
    let head = target.car();
    let arguments = target.cdr();
    if (head != symbols.dollar && head != symbols.bracket2)
        || arguments.kind() != SexpType::Pairlist
        || arguments.tag() != nil()
        || arguments.cdr().kind() != SexpType::Pairlist
        || arguments.cdr().tag() != nil()
        || arguments.cdr().cdr() != nil()
    {
        return None;
    }
    let inner = arguments.car();
    let container = match inner.kind() {
        SexpType::Symbol => {
            let cell = crate::heap().frame_cell(env, inner)?;
            if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED) {
                return None;
            }
            cell.cdr()
        }
        SexpType::Language => {
            let (outer, position) = held_container(inner, env)?;
            held_element(outer, position)
        }
        _ => return None,
    };
    let index = arguments.cdr().car();
    if container.kind() == SexpType::Record {
        // A plain record's field, which only `rebind_update` assigns back in place.
        if head != symbols.dollar
            || index.kind() != SexpType::Symbol
            || container.refcount() != 1
            || !reuse_permitted(env, container)
        {
            return None;
        }
        let position = crate::records::plain_field(container, index)?;
        let element = container.record_handle(position)?;
        return (element.refcount() == 1).then_some((container, position));
    }
    if container.kind() != SexpType::List
        || container.is_object()
        || container.is_altrep()
        || container.refcount() != 1
        || !reuse_permitted(env, container)
    {
        return None;
    }
    let position = match index.kind() {
        SexpType::Symbol if head == symbols.dollar => {
            named_position(container, &symbol_name(index))?
        }
        SexpType::String if length(index) == 1 && index.element(0) != values::na_string() => {
            named_position(
                container,
                &String::from_utf8_lossy(index.element(0).bytes()),
            )?
        }
        SexpType::Real | SexpType::Integer if head == symbols.bracket2 && length(index) == 1 => {
            let position = values::as_real_scalar(index);
            if !(position >= 1.0 && position <= length(container) as f64) {
                return None;
            }
            position as usize - 1
        }
        _ => return None,
    };
    let element = container.element(position);
    (element.refcount() == 1).then_some((container, position))
}

/// The element at `position` of `container`, a list or a plain record that [`held_container`]
/// found.
unsafe fn held_element(container: Sexp, position: usize) -> Sexp {
    if container.kind() == SexpType::Record {
        container.record_handle(position).unwrap_or(nil())
    } else {
        container.element(position)
    }
}

/// The position of the element named exactly `name`, the one `$<-` and `[[<-` replace.
unsafe fn named_position(list: Sexp, name: &str) -> Option<usize> {
    let names = values::get_attr(list, symbols().names);
    if names.kind() != SexpType::String {
        return None;
    }
    (0..length(names)).find(|&position| names.element(position).bytes() == name.as_bytes())
}

/// Whether two expressions are the same code, as the two sides of `a$b <- f(a$b)` are.
unsafe fn same_expression(left: Sexp, right: Sexp) -> bool {
    if left == right {
        return true;
    }
    match (left.kind(), right.kind()) {
        (SexpType::Language, SexpType::Language) | (SexpType::Pairlist, SexpType::Pairlist) => {
            left.tag() == right.tag()
                && same_expression(left.car(), right.car())
                && same_expression(left.cdr(), right.cdr())
        }
        (SexpType::String | SexpType::Real | SexpType::Integer, _) => {
            left.attrib() == nil()
                && right.attrib() == nil()
                && crate::sort::identical(left, right, &crate::sort::IdenticalFlags::default())
        }
        _ => false,
    }
}

/// Whether the value bound in `env` may change in place without compiled code noticing. The
/// bytecode keeps values in registers that do not count as references, and the compiler keeps a
/// body's own in-place updates from overlapping such registers (`bytecode::may_modify_in_place`).
/// A compiled body suspended on `env` while other code runs there, as `eval.parent()` or a
/// forced promise does, has no such guard, so the update must wait for a value no register
/// holds.
pub unsafe fn reuse_permitted(env: Sexp, value: Sexp) -> bool {
    if !env.has_gp(gp::COMPILED_FRAME) {
        return true;
    }
    let innermost = interp().frames.last();
    // `eval()` pushes a frame on the env it evaluates in whose function is the builtin.
    innermost.is_some_and(|frame| {
        frame.env == env
            && frame.function.kind() == SexpType::Closure
            && bytecode::is_compiled(frame.function)
    }) || !bytecode::registers_hold(value)
}

pub unsafe fn assign_symbol(symbol: Sexp, value: Sexp, env: Sexp, superassign: bool) -> R<()> {
    if superassign {
        set_var(symbol, value, env.enclosure())
    } else {
        assign_local(symbol, value, env, crate::heap().frame_cell(env, symbol))
    }
}

/// `symbol <- value` in `env`, whose own frame binds `symbol` to `cell`, or not at all.
#[inline(always)]
pub unsafe fn assign_local(symbol: Sexp, value: Sexp, env: Sexp, cell: Option<Sexp>) -> R<()> {
    let heap = crate::heap();
    let Some(cell) = cell else {
        heap.define_unbound(env, symbol, value)?;
        return Ok(());
    };
    if cell.has_gp(gp::ACTIVE_BINDING) {
        let function = cell.cdr();
        apply(
            nil(),
            function,
            &[Arg { tag: nil(), value }],
            values::global_env(),
        )?;
        return Ok(());
    }
    heap.set_binding_value(cell, value)?;
    cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
    Ok(())
}

/// `f(x, ...) <- value`: evaluates the getter chain down to the variable, then applies the
/// replacement functions from the outside in and assigns the result to the variable.
///
/// `evaluated` is empty, or holds the arguments after `x` of a one-level target that compiled
/// code has already evaluated, each a forced promise whose code is the argument's (or that code
/// itself, for one the replacement must evaluate), so the replacement evaluates none of them a
/// second time.
pub unsafe fn complex_assign(
    whole: Sexp,
    target: Sexp,
    value: Sexp,
    env: Sexp,
    superassign: bool,
    evaluated: &[Sexp],
) -> R<()> {
    let symbols = symbols();
    let mut getters: SmallVec<[Sexp; 4]> = SmallVec::new();
    let mut inner = target;
    while inner.kind() == SexpType::Language {
        getters.push(inner);
        inner = inner.cdr().car();
    }
    if inner.kind() == SexpType::String && length(inner) == 1 {
        inner = values::install_charsxp(inner.element(0))?;
    }
    if inner.kind() != SexpType::Symbol {
        return Err(error_in(
            whole,
            "target of assignment expands to non-language object",
        ));
    }
    let lookup_env = if superassign { env.enclosure() } else { env };
    let mut base = find_var(inner, lookup_env)?;
    if base == unbound() {
        return Err(error_in(
            whole,
            format!("object '{}' not found", symbol_name(inner)),
        ));
    }
    if base.kind() == SexpType::Promise {
        base = force_promise(base)?;
    }
    let heap = crate::heap();
    let local_cell = heap.frame_cell(env, inner);
    // A locked binding rejects the final assignment, so the replacement must not have modified
    // its value in place by then.
    let locked = local_cell.is_some_and(|cell| cell.has_gp(gp::BINDING_LOCKED));
    // An index that assigns, as in `x[{x[2] <- 3; 1}] <- 2`, rebinds the variable while the
    // replacement still holds the old value, which therefore must be a copy. The `*tmp*`
    // promise deliberately counts no reference, so the inner assignment cannot see the share.
    let index_assigns = getters.iter().any(|getter| {
        any_item(getter.cdr().cdr(), |argument| {
            assigns_to(argument, inner, symbols)
        })
    });
    let mut binding_env = lookup_env;
    while binding_env != heap.known().empty_env && heap.frame_cell(binding_env, inner).is_none() {
        binding_env = binding_env.enclosure();
    }
    // Ending a move into this frame hands the caller `base` back, which makes it shared, and
    // must come before any level changes in place, since an inner level is an element of it.
    crate::moves::modifying(base)?;
    if values::maybe_shared(base)
        || locked
        || index_assigns
        || (!superassign && local_cell.is_none())
        || !reuse_permitted(binding_env, base)
    {
        // A copy-on-write copy is the same value, so an Array stays an Array; the
        // replacement function decides whether the write keeps it one.
        let was_array = base.is_array();
        base = values::shallow_duplicate(base)?;
        base.set_array(was_array);
    }
    // GNU R binds `*tmp*` in this frame for the length of the assignment, and in a hashed frame
    // that moves the table's chain counter, which `ls(sorted = FALSE)` order depends on.
    heap.count_transient_binding(env, symbols.tmp);
    crate::heap().root(base);
    interp().tmp_values.push((env, base));
    let replaced: R<Sexp> = (|| unsafe {
        // Values of each level, innermost first: the variable, then each getter applied to it.
        let mut levels: SmallVec<[Sexp; 4]> = smallvec![base];
        let tmp_symbol = symbols.tmp;
        let mut shared = false;
        for getter in getters.iter().skip(1).rev() {
            let previous = *levels.last().unwrap_or(&base);
            let promise = forced_promise(tmp_symbol, previous)?;
            crate::heap().untrack_references(promise);
            // The call names `*tmp*`, as GNU R's does, while the argument it is evaluated with is
            // the promise holding the level below.
            let rest = getter.cdr().cdr();
            let getter_call = values::lcons(
                getter.car(),
                values::cons_tagged(tmp_symbol, rest, getter.cdr().tag())?,
            )?;
            crate::heap().root(getter_call);
            let arguments = values::cons_tagged(promise, rest, getter.cdr().tag())?;
            crate::heap().root(arguments);
            let function = call_head_function(getter.car(), env)?;
            let mut level = apply_call(getter_call, function, arguments, env)?;
            if values::maybe_shared(level) || shared {
                shared = true;
                let was_array = level.is_array();
                level = values::shallow_duplicate(level)?;
                level.set_array(was_array);
            }
            levels.push(level);
        }
        let right_hand_side = match whole.cdr().cdr().kind() {
            SexpType::Pairlist | SexpType::Language => whole.cdr().cdr().car(),
            _ => value,
        };
        let mut replacement = value;
        for (depth, getter) in getters.iter().enumerate() {
            let object = levels[levels.len() - 1 - depth];
            let setter = replacement_function(getter.car(), whole)?;
            // The object promise is the `*tmp*` GNU R binds: it must not count as a reference, or
            // the replacement function would always see its target as shared and copy it.
            let object_promise = forced_promise(tmp_symbol, object)?;
            crate::heap().untrack_references(object_promise);
            // The outermost replacement's value promise carries the right-hand side's expression,
            // which `substitute(value)` returns; an inner one carries the value itself, as in GNU R.
            let value_code = if depth == 0 {
                right_hand_side
            } else {
                replacement
            };
            // GNU R's value promise does not count either, so a replacement that updated its target
            // in place stays uniquely referenced for the next update.
            let value_promise = forced_promise(value_code, replacement)?;
            crate::heap().untrack_references(value_promise);
            // The call `` `f<-`(`*tmp*`, ..., value = v) `` and the argument list it is applied
            // with differ only in their first argument, so they share the cells after it, unless
            // the arguments are already evaluated: then the call keeps their code.
            let mut tail_items: SmallVec<[(Sexp, Sexp); 4]> = SmallVec::new();
            let mut rest = getter.cdr().cdr();
            while matches!(rest.kind(), SexpType::Pairlist | SexpType::Language) {
                tail_items.push((rest.tag(), rest.car()));
                rest = rest.cdr();
            }
            tail_items.push((symbols.value, value_promise));
            let tail = values::pairlist(&tail_items, SexpType::Pairlist)?;
            let setter_call = values::lcons(setter, values::cons(tmp_symbol, tail)?)?;
            let argument_tail = if depth == 0 && evaluated.len() + 1 == tail_items.len() {
                for (item, &argument) in tail_items.iter_mut().zip(evaluated) {
                    item.1 = argument;
                }
                crate::heap().root(setter_call);
                values::pairlist(&tail_items, SexpType::Pairlist)?
            } else {
                tail
            };
            let arguments = values::cons(object_promise, argument_tail)?;
            let function = call_head_function(setter, env)?;
            // A closure reports its errors against its own call, `` `f<-`(`*tmp*`, value = v) ``,
            // as GNU R's does, so only a builtin replacement names the assignment.
            if function.kind() == SexpType::Closure {
                replacement = apply_call(setter_call, function, arguments, env)?;
                continue;
            }
            // A level past the first reports its errors as GNU R does, against the assignment of
            // that level's target, such as `` names(x) <- `*vtmp*` `` in `names(x)[2] <- "b"`.
            let reported = if depth == 0 {
                whole
            } else {
                values::call(whole.car(), &[*getter, symbols.vtmp])?
            };
            crate::heap().root(reported);
            let frame_depth = interp().frames.len();
            let outer = interp()
                .assignment_call
                .replace((setter_call, reported, frame_depth));
            let result = apply_call(setter_call, function, arguments, env);
            interp().assignment_call = outer;
            replacement = result?;
        }
        Ok(replacement)
    })();
    interp().tmp_values.pop();
    assign_symbol(inner, replaced?, env, superassign)
}

/// Whether `expression` contains an assignment whose target is `variable` or a part of it. An
/// assignment to another variable cannot rebind `variable`, so it needs no defensive copy.
pub(crate) unsafe fn assigns_to(
    expression: Sexp,
    variable: Sexp,
    symbols: &values::Symbols,
) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    let assignment = head == symbols.assign || head == symbols.assign2 || head == symbols.assign_eq;
    if assignment {
        let mut target = expression.cdr().car();
        while target.kind() == SexpType::Language {
            target = target.cdr().car();
        }
        let named = target == variable
            || (target.kind() == SexpType::String
                && length(target) == 1
                && target.element(0).bytes() == variable.car().bytes());
        if named {
            return true;
        }
    }
    any_item(expression, |part| assigns_to(part, variable, symbols))
}

/// Whether `test` holds for any value of the pairlist or call `list`.
unsafe fn any_item(mut list: Sexp, mut test: impl FnMut(Sexp) -> bool) -> bool {
    while matches!(list.kind(), SexpType::Pairlist | SexpType::Language) {
        if test(list.car()) {
            return true;
        }
        list = list.cdr();
    }
    false
}

/// The function a call's head names: a symbol is looked up as a function, anything else (such
/// as `pkg::f`) is evaluated.
unsafe fn call_head_function(head: Sexp, env: Sexp) -> R<Sexp> {
    let function = if head.kind() == SexpType::Symbol {
        find_fun(head, env)?
    } else {
        eval(head, env)?
    };
    crate::heap().root(function);
    Ok(function)
}

/// The replacement function for a getter: `f<-` for `f`, and `pkg::\`f<-\`` for `pkg::f`.
unsafe fn replacement_function(getter: Sexp, whole: Sexp) -> R<Sexp> {
    match getter.kind() {
        SexpType::Symbol => {
            if let Some(&symbol) = interp().replacement_symbols.get(&getter) {
                return Ok(symbol);
            }
            let mut name: SmallVec<[u8; 32]> = SmallVec::from_slice(getter.car().bytes());
            name.extend_from_slice(b"<-");
            let symbol = values::install_bytes(&name)?;
            interp().replacement_symbols.insert(getter, symbol);
            Ok(symbol)
        }
        SexpType::Language
            if (getter.car() == symbols().double_colon
                || getter.car() == symbols().triple_colon)
                && values::length(getter) == 3 =>
        {
            let function = getter.cdr().cdr().car();
            let mut name = function.car().bytes().to_vec();
            name.extend_from_slice(b"<-");
            let symbol = values::install_bytes(&name)?;
            values::call(getter.car(), &[getter.cdr().car(), symbol])
        }
        _ => Err(error_in(whole, "invalid function in complex assignment")),
    }
}

/// Evaluates each element of an expression vector in turn, with its srcref current, and
/// returns the last value.
pub unsafe fn eval_all(expressions: Sexp, env: Sexp) -> R<Sexp> {
    let srcrefs = values::get_attr(expressions, symbols().srcref);
    let saved = interp().srcref;
    // As in `{`, nothing an element made stays rooted while the next one runs, so an object
    // it drops can be finalized by a `gc()` in a later element (testthat evaluates a whole
    // test file this way).
    let mark = crate::heap().handle_mark();
    let count = length(expressions);
    let mut value = nil();
    for index in 0..count {
        if srcrefs != nil() {
            interp().srcref = statement_srcref(srcrefs, index);
        }
        let result = match interp().debugger.is_some() {
            true => {
                crate::debugger::statement().and_then(|()| eval(expressions.element(index), env))
            }
            false => eval(expressions.element(index), env),
        };
        interp().srcref = saved;
        crate::heap().release_handles(mark);
        match result {
            Ok(next) => {
                if index + 1 == count {
                    crate::heap().root(next);
                }
                value = next;
            }
            Err(unwind) => {
                unwind.root();
                return Err(unwind);
            }
        }
    }
    Ok(value)
}

/// Makes a fresh, empty `DOTSXP`-free argument list from evaluated values.
pub fn value_args(values: &[Sexp]) -> Args {
    values
        .iter()
        .map(|&value| Arg { tag: nil(), value })
        .collect()
}

pub unsafe fn alloc_list(count: usize) -> R<Sexp> {
    alloc(SexpType::List, count)
}

pub unsafe fn tagged(tag: &str, value: Sexp) -> R<Arg> {
    Ok(Arg {
        tag: values::install(tag)?,
        value,
    })
}

pub unsafe fn cons_value(value: Sexp, rest: Sexp, tag: Sexp) -> R<Sexp> {
    cons_tagged(value, rest, tag)
}
