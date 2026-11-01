//! Moving a caller's dictionary, set or list into a closure's parameter, so that
//! `d <- helper(d, k)` lets `helper` update `d` in place, as `d <- Dict.insert(d, k, v)` does.
//!
//! Without the move the value has two references while `helper` runs, the caller's `d` and the
//! parameter, so its first `Dict.insert` copies the whole dictionary. The move clears the
//! caller's binding for the duration of the call, which leaves the parameter as the only
//! reference. That is invisible only if nothing reads the caller's `d` before the assignment
//! replaces it, and if an error, which skips the assignment, leaves `d` as it was.
//!
//! The first condition is checked statically, then guarded at run time. Before the move, the
//! callee's body and defaults, and the other arguments, must call nothing but closures and the
//! builtins in [`inert_builtin`], none of which reads a frame, and must not name the caller's
//! variable unless it is the callee's own parameter. The body of a function literal in that code
//! gets the same check, and a literal that passes it is a checked literal. While the move lasts,
//! any code those checks did not cover ends it: entering a closure other than one made from a
//! checked literal while the move lasts (which covers S3 methods, condition handlers and
//! callbacks), forcing a promise the checks did not see, a callback that is not a closure, and
//! the default error handler. Ending a move early, a deoptimization, binds the caller's variable
//! to the value it had before the call, rebuilt from the journal below, so whatever runs next
//! sees exactly what it would have seen without the move.
//!
//! The second condition is the journal. An in-place update of a dictionary, set or list either
//! appends entries or replaces an entry's value (`standard_library::insert_in_place`, `List.set`
//! and `List.swap`), and a list that outgrows its capacity is copied, which leaves the moved
//! value as it was. So the original is the first `entries` entries with the replaced values put
//! back. On an error the caller's variable gets that original, rebuilt, and the moved value stays
//! with whoever holds it now.
//!
//! The checker's static types do not help here. A type says what a value is, not which frames a
//! function reads, so a typed helper needs the same body check as an untyped one.

use {
    crate::{
        builtins::Arg,
        eval::{self, Args},
        interp, records, standard_library,
        unwind::R,
        values::{self, missing_arg, nil, symbol_name, symbols},
    },
    heap::{
        Marker,
        object::{Sexp, SexpType, gp},
        records::NativeKind,
    },
    std::iter,
};

/// The most closures or frames a move tracks. A helper that creates more, in a loop, ends the
/// move instead of growing the record without bound.
const TRACKED_LIMIT: usize = 64;

/// The moves in progress, innermost last.
#[derive(Default)]
pub struct State {
    moves: Vec<Move>,
}

struct Move {
    /// The dictionary, set or list the callee's parameter holds.
    value: Sexp,
    /// The caller's binding cell, which holds `NULL` until the move ends.
    cell: Sexp,
    target: Sexp,
    /// The callee's frame, whose own promises (its defaults) the checks covered. `NULL` until
    /// the callee is entered.
    callee: Sexp,
    entering: bool,
    /// The promises of the other arguments, whose code the checks covered.
    promises: Vec<Sexp>,
    /// The bodies of the checked literals.
    literals: Vec<Sexp>,
    /// The closures created while the move lasts, each with whether its body is a checked
    /// literal, so that entering it keeps the move.
    closures: Vec<(Sexp, bool)>,
    /// The frames of the checked closures entered while the move lasts, which are as private as
    /// the callee's own.
    frames: Vec<Sexp>,
    /// How many entries the value had when it moved.
    entries: usize,
    /// Entry positions whose value an update replaced, with the value it replaced.
    replaced: Vec<(usize, Sexp)>,
    /// Whether the move ended early and the caller's binding holds the original again.
    ended: bool,
    /// Whether the caller's binding kept the value because something else references it too.
    /// Nothing is updated in place then, so ending or failing restores nothing.
    shared: bool,
}

impl State {
    pub fn trace(&self, marker: &mut Marker<'_>) {
        for entry in &self.moves {
            // SAFETY: the collector runs on the interpreter's thread, and the move keeps these
            // objects alive until it ends.
            unsafe {
                for object in [entry.value, entry.cell, entry.target, entry.callee] {
                    marker.mark(object);
                }
                for &object in entry
                    .promises
                    .iter()
                    .chain(&entry.literals)
                    .chain(&entry.frames)
                {
                    marker.mark(object);
                }
                for &(closure, _) in &entry.closures {
                    marker.mark(closure);
                }
                for &(_, old) in &entry.replaced {
                    marker.mark(old);
                }
            }
        }
    }
}

/// Calls the closure `function` for `call`, `target <- call` with `target` as its first
/// argument, moving the caller's value into the parameter when that is invisible. `None` means
/// the call is not one the move handles, and the caller makes the ordinary call.
pub unsafe fn call_moving(call: Sexp, function: Sexp, target: Sexp, env: Sexp) -> R<Option<Sexp>> {
    let heap = crate::heap();
    let Some(cell) = heap.frame_cell(env, target) else {
        return Ok(None);
    };
    let value = cell.cdr();
    if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED)
        || extent(value).is_none()
        || !eval::reuse_permitted(env, value)
    {
        return Ok(None);
    }
    let Some(literals) = movable(call, function, target, env) else {
        return Ok(None);
    };
    let mut args = Args::new();
    args.push(Arg { tag: nil(), value });
    let mut promises = Vec::new();
    let mut rest = call.cdr().cdr();
    while rest.kind() == SexpType::Pairlist {
        let expression = rest.car();
        let promise = if expression == missing_arg() {
            expression
        } else {
            let promise = eval::make_promise(expression, env)?;
            promises.push(promise);
            promise
        };
        args.push(Arg {
            tag: nil(),
            value: promise,
        });
        rest = rest.cdr();
    }
    // A shared value cannot move, but the call still runs tracked, so that the finished frame
    // can be released and the result, which no one else holds, moves on the next call. The
    // previous top-level value, `.Last.value`, shares a loop's first value this way.
    let shared = value.refcount() != 1;
    if !shared {
        heap.set_binding_value(cell, nil())?;
    }
    interp().moves.moves.push(Move {
        value,
        cell,
        target,
        callee: nil(),
        entering: true,
        promises,
        literals,
        closures: Vec::new(),
        frames: Vec::new(),
        entries: extent(value).unwrap_or(0),
        replaced: Vec::new(),
        ended: false,
        shared,
    });
    let result = eval::apply_closure_moved(call, function, &args, env);
    // The original is rebuilt while the record still lies on the traced stack, because
    // rebuilding allocates and the record holds the only references to the replaced values.
    let restored = match interp().moves.moves.last() {
        Some(entry) if result.is_err() && !entry.ended && !entry.shared => Some(original(entry)?),
        Some(_) => None,
        None => panic!("a moved argument's record disappeared during the call"),
    };
    let Some(finished) = interp().moves.moves.pop() else {
        panic!("a moved argument's record disappeared during the call");
    };
    if let Some(original) = restored {
        heap.set_binding_value(finished.cell, original)?;
    }
    if let Ok(value) = result
        && !finished.ended
        && finished.callee != nil()
    {
        release_parameter(&finished, function, value)?;
    }
    result.map(Some)
}

/// Whether the innermost move is into the frame `env` and still lasts, which shows that no code
/// the checks did not cover has run, so nothing can have captured the frame.
pub unsafe fn frame_private(env: Sexp) -> bool {
    interp()
        .moves
        .moves
        .last()
        .is_some_and(|entry| !entry.ended && !entry.shared && entry.callee == env)
}

/// Whether a call of `function` may create a closure, from its body or a default, which would
/// capture the call's frame without entering a closure, so without ending a move.
pub unsafe fn may_capture_frame(function: Sexp) -> bool {
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist {
        if creates_closure(formal.car()) {
            return true;
        }
        formal = formal.cdr();
    }
    creates_closure(crate::bytecode::source_body(function))
}

/// Called before code reads or writes a variable through an environment value, as `e$x`,
/// `e[["x"]]` and their assignments do. The static checks cannot tell which variable holds an
/// environment, so such an access may reach a moved variable's frame, and it ends every move.
pub unsafe fn reaching_environment() -> R<()> {
    if interp().moves.moves.iter().all(|entry| entry.ended) {
        return Ok(());
    }
    end_all()
}

/// Called as the closure `function` starts running on `env`. The callee of the innermost move
/// records its frame, and a closure made from a checked literal while every live move lasted
/// adds its frame to theirs; any other closure ends every move.
pub unsafe fn entering_closure(function: Sexp, env: Sexp) -> R<()> {
    let moves = &mut interp().moves.moves;
    let Some(innermost) = moves.last_mut() else {
        return Ok(());
    };
    if innermost.entering {
        innermost.entering = false;
        innermost.callee = env;
        return Ok(());
    }
    let live = || moves.iter().filter(|entry| !entry.ended);
    let checked = live().all(|entry| {
        entry.frames.len() < TRACKED_LIMIT && entry.closures.contains(&(function, true))
    });
    if !checked {
        return end_all();
    }
    for entry in moves.iter_mut().filter(|entry| !entry.ended) {
        entry.frames.push(env);
    }
    Ok(())
}

/// Called when `closure` is created. Every live move records it: a closure may capture its
/// frame, which [`release_parameter`] must then rule out.
pub unsafe fn created_closure(closure: Sexp) -> R<()> {
    let moves = &mut interp().moves.moves;
    if moves.iter().all(|entry| entry.ended) {
        return Ok(());
    }
    let body = crate::bytecode::source_of_body(closure.cdr());
    let mut overflow = false;
    for entry in moves.iter_mut().filter(|entry| !entry.ended) {
        let checked = entry.literals.contains(&body);
        entry.closures.push((closure, checked));
        overflow |= entry.closures.len() > TRACKED_LIMIT;
    }
    if overflow { end_all() } else { Ok(()) }
}

/// Called before `promise` is forced. A promise whose code the checks did not cover ends every
/// move, and so does one that reads a moved variable.
pub unsafe fn forcing(promise: Sexp) -> R<()> {
    let moves = &interp().moves.moves;
    if moves.iter().all(|entry| entry.ended) {
        return Ok(());
    }
    let code = promise.cdr();
    let covered = match code.kind() {
        SexpType::Symbol => !moves.iter().any(|entry| entry.target == code),
        SexpType::Language => moves.iter().any(|entry| {
            promise.tag() == entry.callee
                || entry.frames.contains(&promise.tag())
                || entry.promises.contains(&promise)
        }),
        _ => true,
    };
    if covered { Ok(()) } else { end_all() }
}

/// Called before a standard library function calls `function` back: only a closure, which ends
/// the moves itself if it must, is known not to read a frame.
pub unsafe fn calling_back(function: Sexp) -> R<()> {
    if function.kind() == SexpType::Closure || interp().moves.moves.is_empty() {
        return Ok(());
    }
    end_all()
}

/// Called before the default error handler runs, which may evaluate `options(error =)`.
pub unsafe fn handling_error() -> R<()> {
    if interp().moves.moves.is_empty() {
        return Ok(());
    }
    end_all()
}

/// Records that an in-place update replaces the value at `position` of `object`'s entries.
pub unsafe fn replacing(object: Sexp, position: usize, old: Sexp) {
    for entry in &mut interp().moves.moves {
        if entry.value == object && !entry.ended && position < entry.entries {
            entry.replaced.push((position, old));
        }
    }
}

/// Ends every move: each caller's binding gets back the value it held before its call.
unsafe fn end_all() -> R<()> {
    let heap = crate::heap();
    let count = interp().moves.moves.len();
    for index in 0..count {
        let Some(entry) = interp().moves.moves.get(index) else {
            break;
        };
        if entry.ended {
            continue;
        }
        if entry.shared {
            if let Some(entry) = interp().moves.moves.get_mut(index) {
                entry.ended = true;
            }
            continue;
        }
        let original = original(entry)?;
        heap.set_binding_value(entry.cell, original)?;
        if let Some(entry) = interp().moves.moves.get_mut(index) {
            entry.ended = true;
            entry.replaced.clear();
        }
    }
    Ok(())
}

/// How many entries `value` has, when it is a value a move handles: a dictionary, a set, or a
/// plain list, which an update may grow in place. An Array or a list with attributes is never
/// updated in place, so moving one gains nothing.
unsafe fn extent(value: Sexp) -> Option<usize> {
    match value.kind() {
        SexpType::Record
            if matches!(
                records::native_kind(value),
                NativeKind::Dict | NativeKind::Set
            ) =>
        {
            Some(standard_library::entry_count(value))
        }
        SexpType::List if !value.is_array() && !value.is_altrep() && value.attrib() == nil() => {
            Some(values::length(value))
        }
        _ => None,
    }
}

/// The value the move's caller had: the moved value itself while no update touched its first
/// entries, and otherwise a copy of those entries with the replaced values put back.
unsafe fn original(entry: &Move) -> R<Sexp> {
    if entry.replaced.is_empty() && extent(entry.value) == Some(entry.entries) {
        return Ok(entry.value);
    }
    if entry.value.kind() == SexpType::List {
        let mut items: Vec<Sexp> = (0..entry.entries)
            .map(|position| entry.value.element(position))
            .collect();
        for &(position, old) in entry.replaced.iter().rev() {
            if let Some(slot) = items.get_mut(position) {
                *slot = old;
            }
        }
        return values::list_vector(&items);
    }
    standard_library::collection_prefix(entry.value, entry.entries, &entry.replaced)
}

/// Drops the finished callee frame's reference to the moved value, or to whatever an update
/// put in its place, so the caller's next `d <- helper(d, k)` finds it unshared again. Without
/// this the dead frame would keep the result shared, and every later call would copy it.
///
/// The frame is unreachable once the call returns: while the move lasted only inert builtins
/// and checked closures ran (any other closure would have ended it), none of which keeps a
/// frame, and no closure created meanwhile, which could have captured the frame, outlives it
/// ([`closures_dead`]). The heap does not count an environment's enclosure as a reference, so
/// the environment's own refcount cannot show this, as GNU R's `R_CleanupEnvir` uses it to.
unsafe fn release_parameter(finished: &Move, function: Sexp, result: Sexp) -> R<()> {
    if !closures_dead(finished, result) {
        return Ok(());
    }
    let heap = crate::heap();
    if let Some(cell) = heap.frame_cell(finished.callee, function.car().tag()) {
        heap.set_binding_value(cell, nil())?;
    }
    Ok(())
}

/// Whether every closure created while the move lasted is garbage once the call returns: none is
/// the result, and each is referenced only from the frames the move kept private, which die with
/// the call. A frame's binding holds a closure directly or as a forced promise's value; any
/// other reference, such as a list in the result holding it, keeps the closure's refcount above
/// what those frames account for. A reference the heap does not count is never counted here
/// either, so a miscount only keeps the frame.
unsafe fn closures_dead(finished: &Move, result: Sexp) -> bool {
    finished.closures.iter().all(|&(closure, _)| {
        if closure == result {
            return false;
        }
        let mut held: u32 = 0;
        for frame in iter::once(finished.callee).chain(finished.frames.iter().copied()) {
            for cell in crate::heap().local_cells(frame) {
                if !cell.tracks_references() {
                    continue;
                }
                let value = cell.cdr();
                if value == closure
                    || (value.kind() == SexpType::Promise
                        && value.tracks_references()
                        && value.car() == closure)
                {
                    held += 1;
                }
            }
        }
        u32::from(closure.refcount()) <= held
    })
}

/// Whether evaluating `expression` may create a closure, which would capture its frame.
unsafe fn creates_closure(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol && &*symbol_name(head) == "function" {
        return true;
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
        if creates_closure(cell.car()) {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

/// Whether moving into `function` for `call` is invisible, as far as the code can tell before
/// it runs (see the module documentation), with the bodies of the checked literals when it is.
unsafe fn movable(call: Sexp, function: Sexp, target: Sexp, env: Sexp) -> Option<Vec<Sexp>> {
    let arguments = call.cdr();
    let formals = function.car();
    if formals.kind() != SexpType::Pairlist || formals.tag() == symbols().dots {
        return None;
    }
    let mut formal_count = 0;
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == symbols().dots {
            return None;
        }
        formal_count += 1;
        formal = formal.cdr();
    }
    let mut argument_count = 0;
    let mut argument = arguments;
    while argument.kind() == SexpType::Pairlist {
        if argument.tag() != nil() || argument.car() == symbols().dots {
            return None;
        }
        argument_count += 1;
        argument = argument.cdr();
    }
    if argument_count > formal_count {
        return None;
    }
    let mut forbidden: Vec<Sexp> = interp()
        .moves
        .moves
        .iter()
        .filter(|entry| !entry.ended)
        .map(|entry| entry.target)
        .collect();
    forbidden.push(target);
    let mut literals = Vec::new();
    let caller = Scope {
        heads: env,
        locals: Vec::new(),
        closure_locals: Vec::new(),
        forbidden,
        parent: None,
    };
    let mut argument = arguments.cdr();
    while argument.kind() == SexpType::Pairlist {
        if !inert(&caller, argument.car(), &mut literals) {
            return None;
        }
        argument = argument.cdr();
    }
    let callee = Scope::of_function(
        None,
        &caller.forbidden,
        function.tag(),
        formals,
        function_body(function),
    )?;
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.car() != missing_arg() && !inert(&callee, formal.car(), &mut literals) {
            return None;
        }
        formal = formal.cdr();
    }
    inert(&callee, function_body(function), &mut literals).then_some(literals)
}

unsafe fn function_body(function: Sexp) -> Sexp {
    crate::bytecode::source_body(function)
}

/// Where the checked code runs: the environment its function names resolve from, the variables
/// it binds itself, and the moved variables it must not read. A function literal's body runs in
/// a scope of its own, whose parent is the scope the literal appears in.
struct Scope<'a> {
    heads: Sexp,
    /// The formals, then the other variables the code assigns.
    locals: Vec<Sexp>,
    /// The locals, not formals, that every assignment binds to a function literal, so that a
    /// call of one runs a closure, which ends the move unless it is checked.
    closure_locals: Vec<Sexp>,
    forbidden: Vec<Sexp>,
    parent: Option<&'a Scope<'a>>,
}

impl<'a> Scope<'a> {
    /// The scope of a function with `formals` and `body`, written in `parent` (`None` for the
    /// callee, whose names resolve from `heads`), whose caller must not read `forbidden`.
    /// `None` when the body assigns with `<<-`, or assigns a forbidden variable as a local: a
    /// read before the assignment runs, or on a path that skips it, finds the forbidden
    /// variable, so the local does not hide it.
    unsafe fn of_function(
        parent: Option<&'a Scope<'a>>,
        forbidden: &[Sexp],
        heads: Sexp,
        formals: Sexp,
        body: Sexp,
    ) -> Option<Scope<'a>> {
        let mut locals = Vec::new();
        let mut formal = formals;
        while formal.kind() == SexpType::Pairlist {
            if formal.tag() == symbols().dots {
                return None;
            }
            locals.push(formal.tag());
            formal = formal.cdr();
        }
        let parameters = locals.len();
        if !assigned_symbols(body, &mut locals) {
            return None;
        }
        let forbidden: Vec<Sexp> = forbidden
            .iter()
            .copied()
            .filter(|symbol| !locals[..parameters].contains(symbol))
            .collect();
        if locals[parameters..]
            .iter()
            .any(|symbol| forbidden.contains(symbol))
        {
            return None;
        }
        let closure_locals = locals[parameters..]
            .iter()
            .copied()
            .filter(|&symbol| bound_only_to_literals(body, symbol))
            .collect();
        Some(Scope {
            heads,
            locals,
            closure_locals,
            forbidden,
            parent,
        })
    }

    fn chain(&self) -> impl Iterator<Item = &Scope<'a>> {
        iter::successors(Some(self), |scope| scope.parent)
    }
}

/// Whether evaluating `expression` reads no frame but its own, calling only closures and inert
/// builtins, and never reads a forbidden variable. The bodies of the function literals in it
/// that pass the same check are added to `literals`.
unsafe fn inert(scope: &Scope<'_>, expression: Sexp, literals: &mut Vec<Sexp>) -> bool {
    match expression.kind() {
        SexpType::Symbol => !scope.forbidden.contains(&expression),
        SexpType::Language => {
            let head = expression.car();
            if head.kind() != SexpType::Symbol {
                return false;
            }
            let name = symbol_name(head);
            let binders: Vec<&Scope<'_>> = scope
                .chain()
                .filter(|outer| outer.locals.contains(&head))
                .collect();
            if !binders.is_empty() {
                // A local closure, or what the name finds before the assignment has run.
                let closures = binders
                    .iter()
                    .all(|binder| binder.closure_locals.contains(&head));
                let fallback = resolve_function(head, scope.heads)
                    .is_none_or(|function| acceptable(function, &name));
                if !closures || !fallback {
                    return false;
                }
            } else {
                let Some(function) = resolve_function(head, scope.heads) else {
                    return false;
                };
                if !acceptable(function, &name) {
                    return false;
                }
            }
            match &*name {
                "function" => {
                    check_literal(scope, expression, literals);
                    true
                }
                // Quoted code does not run.
                "quote" => true,
                "<-" | "=" => {
                    let target = expression.cdr().car();
                    let value = expression.cdr().cdr().car();
                    assignment_target(scope, target, literals) && inert(scope, value, literals)
                }
                _ => {
                    let mut argument = expression.cdr();
                    while argument.kind() == SexpType::Pairlist {
                        if argument.car() != missing_arg()
                            && !inert(scope, argument.car(), literals)
                        {
                            return false;
                        }
                        argument = argument.cdr();
                    }
                    true
                }
            }
        }
        _ => true,
    }
}

/// Whether the move may call `function`, found under `name`: a closure, which ends the move
/// itself unless it is checked, or an inert builtin.
unsafe fn acceptable(function: Sexp, name: &str) -> bool {
    match function.kind() {
        SexpType::Closure => true,
        SexpType::Builtin | SexpType::Special => eval::builtin_def(function).is_ok_and(|definition| {
            definition.name == name
                && (definition.accepts_native || inert_builtin(definition.name))
        }),
        _ => false,
    }
}

/// Checks the body and defaults of the function literal `expression`, written in `scope`, and
/// adds its body to `literals` when they pass. Creating the closure is inert either way: only
/// running it could read a frame, and running an unchecked one ends the move.
unsafe fn check_literal(scope: &Scope<'_>, expression: Sexp, literals: &mut Vec<Sexp>) {
    let formals = expression.cdr().car();
    let body = expression.cdr().cdr().car();
    if !matches!(formals.kind(), SexpType::Nil | SexpType::Pairlist) {
        return;
    }
    let Some(inner) = Scope::of_function(Some(scope), &scope.forbidden, scope.heads, formals, body)
    else {
        return;
    };
    let mut found = Vec::new();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.car() != missing_arg() && !inert(&inner, formal.car(), &mut found) {
            return;
        }
        formal = formal.cdr();
    }
    if inert(&inner, body, &mut found) {
        literals.push(body);
        literals.extend(found);
    }
}

/// Whether every assignment to `symbol` in `expression`, outside nested function literals,
/// binds it to a function literal, and no loop binds it.
unsafe fn bound_only_to_literals(expression: Sexp, symbol: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        match &*symbol_name(head) {
            "function" | "quote" => return true,
            "<-" | "=" => {
                let target = expression.cdr().car();
                let value = expression.cdr().cdr().car();
                if target == symbol {
                    return is_literal(value) && bound_only_to_literals(value, symbol);
                }
                let mut inner = target;
                while inner.kind() == SexpType::Language {
                    inner = inner.cdr().car();
                }
                if inner == symbol {
                    return false;
                }
            }
            "for" if expression.cdr().car() == symbol => return false,
            _ => {}
        }
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
        if !bound_only_to_literals(cell.car(), symbol) {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

unsafe fn is_literal(expression: Sexp) -> bool {
    expression.kind() == SexpType::Language
        && expression.car().kind() == SexpType::Symbol
        && &*symbol_name(expression.car()) == "function"
}

/// Whether assigning to `target` changes only a variable of the checked code's own: a symbol,
/// or `x$a`, `x[[i]]` and `x[i]` on one, whose replacement functions are inert.
unsafe fn assignment_target(scope: &Scope<'_>, target: Sexp, literals: &mut Vec<Sexp>) -> bool {
    match target.kind() {
        SexpType::Symbol => !scope.forbidden.contains(&target),
        SexpType::Language => {
            let head = target.car();
            if head.kind() != SexpType::Symbol
                || !matches!(&*symbol_name(head), "$" | "[[" | "[")
                || resolve_function(head, scope.heads).is_none_or(|function| {
                    function.kind() != SexpType::Special && function.kind() != SexpType::Builtin
                })
            {
                return false;
            }
            let mut argument = target.cdr().cdr();
            while argument.kind() == SexpType::Pairlist {
                if !inert(scope, argument.car(), literals) {
                    return false;
                }
                argument = argument.cdr();
            }
            assignment_target(scope, target.cdr().car(), literals)
        }
        _ => false,
    }
}

/// Adds the symbols `expression` assigns with `<-` or `=` to `found`; `false` for `<<-` or
/// `assign`-like code, which the move does not handle.
unsafe fn assigned_symbols(expression: Sexp, found: &mut Vec<Sexp>) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        match &*symbol_name(head) {
            "<<-" => return false,
            "function" | "quote" => return true,
            "<-" | "=" | "for" => {
                let mut target = expression.cdr().car();
                while target.kind() == SexpType::Language {
                    target = target.cdr().car();
                }
                if target.kind() == SexpType::Symbol && !found.contains(&target) {
                    found.push(target);
                }
            }
            _ => {}
        }
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
        if !assigned_symbols(cell.car(), found) {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

/// The function a call of `symbol` from `env` finds now, without forcing a promise or calling an
/// active binding. `None` when finding it would do either.
unsafe fn resolve_function(symbol: Sexp, mut env: Sexp) -> Option<Sexp> {
    let heap = crate::heap();
    let known = heap.known();
    while env != known.empty_env {
        if env == known.global_env {
            let (cell, _) = heap.global_cell(symbol)?;
            return function_in(cell);
        }
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return None;
            }
            let mut value = cell.cdr();
            if value.kind() == SexpType::Promise {
                if value.car() == values::unbound() {
                    return None;
                }
                value = value.car();
            }
            if values::is_function(value) {
                return Some(value);
            }
        }
        env = env.enclosure();
    }
    None
}

/// The function a search-path binding holds, when it holds one without forcing anything.
unsafe fn function_in(cell: Sexp) -> Option<Sexp> {
    if cell.has_gp(gp::ACTIVE_BINDING) {
        return None;
    }
    let mut value = cell.cdr();
    if value.kind() == SexpType::Promise {
        if value.car() == values::unbound() {
            return None;
        }
        value = value.car();
    }
    values::is_function(value).then_some(value)
}

/// The base builtins that read no frame and run no R code of their own: whatever R code they
/// reach, such as an S3 method, is a closure, which ends the move.
fn inert_builtin(name: &str) -> bool {
    matches!(
        name,
        "{" | "("
            | "if"
            | "for"
            | "while"
            | "repeat"
            | "break"
            | "next"
            | "return"
            | "function"
            | "quote"
            | "<-"
            | "="
            | "&&"
            | "||"
            | "invisible"
            | "+"
            | "-"
            | "*"
            | "/"
            | "^"
            | "%%"
            | "%/%"
            | "=="
            | "!="
            | "<"
            | ">"
            | "<="
            | ">="
            | "!"
            | "&"
            | "|"
            | ":"
            | "["
            | "[["
            | "$"
            | "length"
            | "c"
            | "list"
            | "is.null"
            | "is.na"
            | "paste"
            | "paste0"
            | "nchar"
            | "seq_len"
            | "seq_along"
            | "as.character"
            | "as.integer"
            | "as.numeric"
            | "as.double"
            | "as.logical"
            | "max"
            | "min"
            | "sum"
            | "abs"
            | "names"
            | "missing"
            | "stop"
            | "warning"
    )
}
