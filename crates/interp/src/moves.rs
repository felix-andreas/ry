//! Moving a caller's dictionary or set into a closure's parameter, so that `d <- helper(d, k)`
//! lets `helper` update `d` in place, as `d <- Dict.insert(d, k, v)` does.
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
//! variable unless it is the callee's own parameter. While the move lasts, any code those checks
//! did not cover ends it: entering a closure (which covers S3 methods, condition handlers and
//! callbacks), forcing a promise the checks did not see, a callback that is not a closure, and
//! the default error handler. Ending a move early, a deoptimization, binds the caller's variable
//! to the value it had before the call, rebuilt from the journal below, so whatever runs next
//! sees exactly what it would have seen without the move.
//!
//! The second condition is the journal. An in-place update of a dictionary or set either
//! appends an entry or replaces an entry's value (`standard_library::insert_in_place`), so the
//! original is the first `entries` entries with the replaced values put back. On an error the
//! caller's variable gets that original, rebuilt, and the moved value stays with whoever holds
//! it now.
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
};

/// The moves in progress, innermost last.
#[derive(Default)]
pub struct State {
    moves: Vec<Move>,
}

struct Move {
    /// The dictionary or set the callee's parameter holds.
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
    /// How many entries the value had when it moved.
    entries: usize,
    /// Entry positions whose value an update replaced, with the value it replaced.
    replaced: Vec<(usize, Sexp)>,
    /// Whether the move ended early and the caller's binding holds the original again.
    ended: bool,
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
                for &promise in &entry.promises {
                    marker.mark(promise);
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
        || value.kind() != SexpType::Record
        || !matches!(
            records::native_kind(value),
            NativeKind::Dict | NativeKind::Set
        )
        || value.refcount() != 1
        || standard_library::entry_count(value) == 0
        || !eval::reuse_permitted(env, value)
        || !movable(call, function, target, env)
    {
        return Ok(None);
    }
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
    heap.set_binding_value(cell, nil())?;
    interp().moves.moves.push(Move {
        value,
        cell,
        target,
        callee: nil(),
        entering: true,
        promises,
        entries: standard_library::entry_count(value),
        replaced: Vec::new(),
        ended: false,
    });
    let result = eval::apply_closure_moved(call, function, &args, env);
    let Some(finished) = interp().moves.moves.pop() else {
        panic!("a moved argument's record disappeared during the call");
    };
    if result.is_err() && !finished.ended {
        let original = original(&finished)?;
        heap.set_binding_value(finished.cell, original)?;
    }
    result.map(Some)
}

/// Called as a closure starts running on `env`. The callee of the innermost move records its
/// frame; any other closure ends every move.
pub unsafe fn entering_closure(env: Sexp) -> R<()> {
    let moves = &mut interp().moves.moves;
    match moves.last_mut() {
        None => Ok(()),
        Some(innermost) if innermost.entering => {
            innermost.entering = false;
            innermost.callee = env;
            Ok(())
        }
        Some(_) => end_all(),
    }
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
            promise.tag() == entry.callee || entry.promises.contains(&promise)
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
        let original = original(entry)?;
        heap.set_binding_value(entry.cell, original)?;
        if let Some(entry) = interp().moves.moves.get_mut(index) {
            entry.ended = true;
            entry.replaced.clear();
        }
    }
    Ok(())
}

/// The value the move's caller had: the moved value itself while no update touched its first
/// entries, and otherwise a copy of those entries with the replaced values put back.
unsafe fn original(entry: &Move) -> R<Sexp> {
    if entry.replaced.is_empty() && standard_library::entry_count(entry.value) == entry.entries {
        return Ok(entry.value);
    }
    standard_library::collection_prefix(entry.value, entry.entries, &entry.replaced)
}

/// Whether moving into `function` for `call` is invisible, as far as the code can tell before
/// it runs (see the module documentation).
unsafe fn movable(call: Sexp, function: Sexp, target: Sexp, env: Sexp) -> bool {
    let arguments = call.cdr();
    let formals = function.car();
    if formals.kind() != SexpType::Pairlist || formals.tag() == symbols().dots {
        return false;
    }
    let mut formal_count = 0;
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == symbols().dots {
            return false;
        }
        formal_count += 1;
        formal = formal.cdr();
    }
    let mut argument_count = 0;
    let mut argument = arguments;
    while argument.kind() == SexpType::Pairlist {
        if argument.tag() != nil() || argument.car() == symbols().dots {
            return false;
        }
        argument_count += 1;
        argument = argument.cdr();
    }
    if argument_count > formal_count {
        return false;
    }
    let mut forbidden: Vec<Sexp> = interp()
        .moves
        .moves
        .iter()
        .filter(|entry| !entry.ended)
        .map(|entry| entry.target)
        .collect();
    forbidden.push(target);
    let caller = Scope {
        heads: env,
        locals: Vec::new(),
        forbidden: &forbidden,
    };
    let mut argument = arguments.cdr();
    while argument.kind() == SexpType::Pairlist {
        if !inert(&caller, argument.car()) {
            return false;
        }
        argument = argument.cdr();
    }
    let body = crate::bytecode::source_body(function);
    let mut locals = Vec::new();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        locals.push(formal.tag());
        formal = formal.cdr();
    }
    if !assigned_symbols(body, &mut locals) {
        return false;
    }
    // A free reference to the caller's variable reads the cleared binding; the callee's own
    // parameter or local of that name is another variable.
    let shadowed: Vec<Sexp> = forbidden
        .iter()
        .copied()
        .filter(|symbol| !locals.contains(symbol))
        .collect();
    let callee = Scope {
        heads: function.tag(),
        locals,
        forbidden: &shadowed,
    };
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        if formal.car() != missing_arg() && !inert(&callee, formal.car()) {
            return false;
        }
        formal = formal.cdr();
    }
    inert(&callee, body)
}

/// Where the checked code runs: the environment its function names resolve from, the variables
/// it binds itself, and the moved variables it must not read.
struct Scope<'a> {
    heads: Sexp,
    locals: Vec<Sexp>,
    forbidden: &'a [Sexp],
}

/// Whether evaluating `expression` reads no frame but its own, calling only closures and inert
/// builtins, and never reads a forbidden variable.
unsafe fn inert(scope: &Scope<'_>, expression: Sexp) -> bool {
    match expression.kind() {
        SexpType::Symbol => !scope.forbidden.contains(&expression),
        SexpType::Language => {
            let head = expression.car();
            if head.kind() != SexpType::Symbol || scope.locals.contains(&head) {
                return false;
            }
            let Some(function) = resolve_function(head, scope.heads) else {
                return false;
            };
            let name = symbol_name(head);
            match function.kind() {
                SexpType::Closure => {}
                SexpType::Builtin | SexpType::Special => {
                    let Ok(definition) = eval::builtin_def(function) else {
                        return false;
                    };
                    if definition.name != name
                        || !(definition.accepts_native || inert_builtin(definition.name))
                    {
                        return false;
                    }
                }
                _ => return false,
            }
            match &*name {
                // A function literal's body runs as a closure, which ends the move if it runs
                // while the move lasts; only its defaults are evaluated by then, and not even
                // those. Quoted code does not run.
                "function" | "quote" => true,
                "<-" | "=" => {
                    let target = expression.cdr().car();
                    let value = expression.cdr().cdr().car();
                    assignment_target(scope, target) && inert(scope, value)
                }
                _ => {
                    let mut argument = expression.cdr();
                    while argument.kind() == SexpType::Pairlist {
                        if argument.car() != missing_arg() && !inert(scope, argument.car()) {
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

/// Whether assigning to `target` changes only a variable of the checked code's own: a symbol,
/// or `x$a`, `x[[i]]` and `x[i]` on one, whose replacement functions are inert.
unsafe fn assignment_target(scope: &Scope<'_>, target: Sexp) -> bool {
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
                if !inert(scope, argument.car()) {
                    return false;
                }
                argument = argument.cdr();
            }
            assignment_target(scope, target.cdr().car())
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
