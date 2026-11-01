//! The language-object evaluator: symbols, promises, calls, closures, argument matching and the
//! control-flow specials. It is the semantic reference the bytecode interpreter must agree with,
//! and it runs everything the bytecode does not compile: promise code, `eval()` of constructed
//! calls, and top-level expressions.

use crate::{
    Frame,
    builtins::{Arg, BuiltinDef, Call, Generic, Kind, Visibility, builtin, internal, special},
    bytecode, conditions, deparse, dispatch, heap, interp, scope,
    unwind::{Jump, R, Unwind, error, error_in, jump},
    values::{
        self, alloc, cons_tagged, length, missing_arg, new_env, nil, symbol_name, symbols,
        triple, unbound,
    },
};
use heap::object::{Sexp, SexpType, gp};

/// The binding-cell bit that marks a formal bound to its default, which `missing()` reads.
pub const BINDING_MISSING: u16 = 1 << 0;
/// The promise bit set while its code runs, to report recursive default references.
const PROMISE_SEEN: u16 = 1 << 0;

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
    let symbols = symbols();
    if symbol == symbols.dots {
        return Err(error("'...' used in an incorrect context"));
    }
    if let Some(index) = dot_dot_index(symbol) {
        return dot_dot_value(index, env);
    }
    let value = find_var(symbol, env)?;
    if value == unbound() {
        return Err(error(format!("object '{}' not found", symbol_name(symbol))));
    }
    if value == missing_arg() {
        return Err(error(format!(
            "argument \"{}\" is missing, with no default",
            symbol_name(symbol)
        )));
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
    let dots = find_var(symbols().dots, env)?;
    if dots == unbound() {
        return Err(error(format!("..{index} used in an incorrect context, no ... to look in")));
    }
    let mut cell = dots;
    let mut position = 1;
    while cell.kind() == SexpType::Dots || cell.kind() == SexpType::Pairlist {
        if position == index {
            let value = cell.car();
            if value == missing_arg() {
                return Err(error(format!("argument \"..{index}\" is missing, with no default")));
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
    let empty = heap.known().empty_env;
    while env != empty {
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
        let function = cell.cdr();
        let value = apply(nil(), function, &[], values::global_env())?;
        return Ok(value);
    }
    Ok(cell.cdr())
}

/// `findVar`: the value bound to `symbol` from `env` outward, or `R_UnboundValue`. Promises are
/// returned unforced.
pub unsafe fn find_var(symbol: Sexp, env: Sexp) -> R<Sexp> {
    match find_cell(symbol, env) {
        Some(cell) => cell_value(cell),
        None => Ok(unbound()),
    }
}

/// The value bound to `symbol` in `env`'s own frame, or `R_UnboundValue`.
pub unsafe fn find_var_in_frame(symbol: Sexp, env: Sexp) -> R<Sexp> {
    match crate::heap().frame_cell(env, symbol) {
        Some(cell) => cell_value(cell),
        None => Ok(unbound()),
    }
}

/// `findFun`: the first function bound to `symbol` from `env` outward.
pub unsafe fn find_fun(symbol: Sexp, mut env: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let empty = heap.known().empty_env;
    while env != empty {
        if let Some(cell) = heap.frame_cell(env, symbol) {
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
    Err(error(format!(
        "could not find function \"{}\"",
        symbol_name(symbol)
    )))
}

/// Binds `symbol` in `env`'s own frame.
pub unsafe fn define(symbol: Sexp, value: Sexp, env: Sexp) -> R<()> {
    let heap = crate::heap();
    if let Some(cell) = heap.frame_cell(env, symbol)
        && cell.has_gp(gp::ACTIVE_BINDING)
    {
        let function = cell.cdr();
        apply(nil(), function, &[Arg { tag: nil(), value }], values::global_env())?;
        return Ok(());
    }
    heap.define(env, symbol, value)?;
    bytecode::note_binding(symbol, env);
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
                apply(nil(), function, &[Arg { tag: nil(), value }], values::global_env())?;
                return Ok(());
            }
            heap.set_binding_value(cell, value)?;
            cell.set_gp_bits(BINDING_MISSING, false);
            return Ok(());
        }
        frame = frame.enclosure();
    }
    define(symbol, value, values::global_env())
}

/// A promise to evaluate `code` in `env`.
pub unsafe fn make_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    triple(SexpType::Promise, unbound(), code, env)
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
    promise.set_gp_bits(PROMISE_SEEN, true);
    let result = eval(promise.cdr(), promise.tag());
    promise.set_gp_bits(PROMISE_SEEN, false);
    let value = result?;
    if promise.has_gp(gp::CHECKED_PROMISE) {
        crate::annotations::check_forced(promise, value)?;
    }
    let heap = crate::heap();
    heap.set_car(promise, value);
    heap.set_tag(promise, nil());
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
        find_fun(head, env)?
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
            let args: Vec<Arg> = values::pairlist_items(arguments)
                .into_iter()
                .map(|(tag, value)| Arg { tag, value })
                .collect();
            call_builtin(definition, call, function, &args, env)
        }
        SexpType::Builtin => {
            let definition = builtin_def(function)?;
            let args = evaluate_args(arguments, env, definition.keep_missing, call)?;
            if definition.generic != Generic::None
                && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, &args, env)?
            {
                return Ok(value);
            }
            call_builtin(definition, call, function, &args, env)
        }
        SexpType::Closure => {
            let args = promise_args(arguments, env)?;
            apply_closure(call, function, &args, env, None)
        }
        _ => Err(error_in(call, "attempt to apply non-function")),
    }
}

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
    if definition.arity >= 0
        && matches!(definition.kind, Kind::Internal | Kind::InternalSpecial)
        && args.len() != definition.arity as usize
    {
        return Err(error_in(
            call,
            format!(
                "{} argument{} passed to .Internal({}) which requires {}",
                args.len(),
                if args.len() == 1 { "" } else { "s" },
                definition.name,
                definition.arity
            ),
        ));
    }
    let function = definition.function;
    let visibility = definition.visibility;
    let context = Call {
        call,
        op,
        args,
        env,
    };
    let result = scope(|| function(&context));
    // A jump out of the builtin (`return(invisible(x))`) keeps the visibility of the value it
    // carries.
    if result.is_ok() {
        match visibility {
            Visibility::On => interp().visible = true,
            Visibility::Off => interp().visible = false,
            Visibility::Keep => {}
        }
    }
    match result {
        Err(Unwind::Error(mut failure)) => {
            if failure.call == unbound() && matches!(definition.kind, Kind::Builtin) {
                // A primitive's own error names the primitive's call, as `errorcall` does.
                failure.call = unbound();
            }
            Err(conditions::signal_error(failure))
        }
        result => result,
    }
}

/// Evaluates a builtin's arguments, expanding `...`.
pub unsafe fn evaluate_args(
    mut arguments: Sexp,
    env: Sexp,
    keep_missing: bool,
    call: Sexp,
) -> R<Vec<Arg>> {
    let dots = symbols().dots;
    let missing = missing_arg();
    let mut args = Vec::new();
    while arguments.kind() == SexpType::Pairlist {
        let expression = arguments.car();
        if expression == dots {
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
        } else {
            let value = eval(expression, env)?;
            args.push(Arg {
                tag: arguments.tag(),
                value,
            });
        }
        arguments = arguments.cdr();
    }
    Ok(args)
}

/// Wraps a closure's arguments in promises, expanding `...` into the promises it holds.
pub unsafe fn promise_args(mut arguments: Sexp, env: Sexp) -> R<Vec<Arg>> {
    let dots = symbols().dots;
    let mut args = Vec::new();
    while arguments.kind() == SexpType::Pairlist {
        let expression = arguments.car();
        if expression == dots {
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
            let value = match expression.kind() {
                SexpType::Symbol | SexpType::Language | SexpType::Promise
                    if expression != missing_arg() =>
                {
                    make_promise(expression, env)?
                }
                SexpType::Bytecode => make_promise(expression, env)?,
                _ => expression,
            };
            args.push(Arg {
                tag: arguments.tag(),
                value,
            });
        }
        arguments = arguments.cdr();
    }
    Ok(args)
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
        SexpType::Builtin => {
            let definition = builtin_def(function)?;
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
                && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, &forced, env)?
            {
                return Ok(value);
            }
            call_builtin(definition, call, function, &forced, env)
        }
        SexpType::Special => {
            // A special needs expressions; quote the values so they evaluate to themselves.
            let definition = builtin_def(function)?;
            let mut quoted = Vec::with_capacity(args.len());
            for arg in args {
                quoted.push(Arg {
                    tag: arg.tag,
                    value: quote_value(value_of(arg.value)?)?,
                });
            }
            call_builtin(definition, call, function, &quoted, env)
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
    let formals = function.car();
    let actuals = match_args(formals, args, call)?;
    let env = new_env(function.tag())?;
    env.set_gp_bits(gp::FUNCTION_FRAME, true);
    let missing = missing_arg();
    let heap = crate::heap();
    let mut formal = formals;
    let mut index = 0;
    while formal.kind() == SexpType::Pairlist {
        let symbol = formal.tag();
        let actual = actuals[index];
        if actual == missing && formal.car() != missing {
            let promise = make_promise(formal.car(), env)?;
            heap.define(env, symbol, promise)?;
            if let Some(cell) = heap.frame_cell(env, symbol) {
                cell.set_gp_bits(BINDING_MISSING, true);
            }
        } else {
            heap.define(env, symbol, actual)?;
        }
        formal = formal.cdr();
        index += 1;
    }
    if let Some(supplied) = supplied {
        for &(symbol, value) in supplied {
            if heap.frame_cell(env, symbol).is_none() {
                heap.define(env, symbol, value)?;
            }
        }
    }
    let promargs = args_pairlist(args)?;
    run_closure(call, function, env, sys_parent, promargs)
}

/// The arguments as a pairlist of their values or promises, which a frame records.
pub unsafe fn args_pairlist(args: &[Arg]) -> R<Sexp> {
    let items: Vec<(Sexp, Sexp)> = args.iter().map(|arg| (arg.tag, arg.value)).collect();
    values::pairlist(&items, SexpType::Pairlist)
}

/// Runs a closure body in an environment whose arguments are already bound.
pub unsafe fn run_closure(
    call: Sexp,
    function: Sexp,
    env: Sexp,
    sys_parent: Sexp,
    promargs: Sexp,
) -> R<Sexp> {
    let state = interp();
    if state.frames.len() as u32 >= state.max_depth {
        return Err(error_in(
            call,
            "evaluation nested too deeply: infinite recursion / options(expressions=)?",
        ));
    }
    let handlers = state.handlers.clone();
    let restarts = state.restarts.clone();
    state.frames.push(Frame {
        call,
        function,
        env,
        sys_parent,
        promargs,
        on_exit: Vec::new(),
        handlers: handlers.clone(),
        restarts: restarts.clone(),
    });
    let depth = state.frames.len();
    let signature = crate::annotations::enter(call, function, env, depth);
    let result = signature.and_then(|signature| {
        let result = scope(|| {
            stacker::maybe_grow(128 * 1024, 4 * 1024 * 1024, || {
                let body = function.cdr();
                bytecode::run_body(function, body, env)
            })
        });
        Ok((signature, result))
    });
    let (signature, result) = match result {
        Ok(pair) => pair,
        Err(unwind) => (None, Err(unwind)),
    };
    let result = match result {
        Err(unwind) if unwind.returns_to(env) => {
            let Unwind::Jump(jump) = unwind else {
                unreachable!()
            };
            match *jump {
                Jump::Return { value, .. } => {
                    crate::heap().root(value);
                    Ok(value)
                }
                _ => unreachable!(),
            }
        }
        other => other,
    };
    let result = match (signature, result) {
        (Some(signature), Ok(value)) => {
            crate::annotations::check_return(call, signature, value).map(|()| value)
        }
        (_, result) => result,
    };
    crate::annotations::leave(depth);
    let result = run_on_exit(depth, result);
    let state = interp();
    state.frames.truncate(depth - 1);
    state.handlers = handlers;
    state.restarts = restarts;
    result
}

/// Runs the `on.exit` expressions of frame `depth` (one-based), keeping the visibility of the
/// value. An unwind from an `on.exit` expression replaces the frame's own result.
unsafe fn run_on_exit(depth: usize, result: R<Sexp>) -> R<Sexp> {
    let frame = &mut interp().frames[depth - 1];
    if frame.on_exit.is_empty() {
        return result;
    }
    let expressions = std::mem::take(&mut frame.on_exit);
    let env = frame.env;
    let visible = interp().visible;
    let mark = crate::heap().handle_mark();
    for &expression in &expressions {
        crate::heap().root(expression);
    }
    let mut outcome = result;
    for expression in expressions {
        // A handler stack that a condition left in place must not run the exit code.
        let saved_handlers = interp().handlers.clone();
        interp().handlers = interp().frames[depth - 1].handlers.clone();
        let exit = eval(expression, env);
        interp().handlers = saved_handlers;
        if let Err(unwind) = exit {
            outcome = Err(unwind);
            break;
        }
    }
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
pub unsafe fn match_args(formals: Sexp, args: &[Arg], call: Sexp) -> R<Vec<Sexp>> {
    let missing = missing_arg();
    let dots_symbol = symbols().dots;
    let formal_symbols: Vec<Sexp> = values::pairlist_items(formals)
        .into_iter()
        .map(|(tag, _)| tag)
        .collect();
    let count = formal_symbols.len();
    let mut matched: Vec<Option<Sexp>> = vec![None; count];
    let mut used = vec![false; args.len()];
    let dots_position = formal_symbols.iter().position(|&symbol| symbol == dots_symbol);
    let arg_names: Vec<Option<&[u8]>> = args
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
                if matched[formal_index].is_some() {
                    return Err(error_in(
                        call,
                        format!(
                            "formal argument \"{}\" matched by multiple actual arguments",
                            symbol_name(formal)
                        ),
                    ));
                }
                matched[formal_index] = Some(args[arg_index].value);
                used[arg_index] = true;
            }
        }
    }
    // Partial matches, only for formals before `...`.
    let partial_limit = dots_position.unwrap_or(count);
    let mut partial_owner: Vec<Option<usize>> = vec![None; args.len()];
    for (formal_index, &formal) in formal_symbols.iter().enumerate().take(partial_limit) {
        if matched[formal_index].is_some() {
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
                        format!("argument {} matches multiple formal arguments", arg_index + 1),
                    ));
                }
                if matched[formal_index].is_some() {
                    return Err(error_in(
                        call,
                        format!(
                            "formal argument \"{}\" matched by multiple actual arguments",
                            symbol_name(formal)
                        ),
                    ));
                }
                matched[formal_index] = Some(args[arg_index].value);
                used[arg_index] = true;
                partial_owner[arg_index] = Some(formal_index);
            }
        }
    }
    // Positional matches up to `...`; everything left goes to `...`.
    let mut dots_items: Vec<(Sexp, Sexp)> = Vec::new();
    let mut unused: Vec<usize> = Vec::new();
    let mut formal_index = 0;
    for (arg_index, arg) in args.iter().enumerate() {
        if used[arg_index] {
            continue;
        }
        if arg_names[arg_index].is_some() {
            if dots_position.is_some() {
                dots_items.push((arg.tag, arg.value));
            } else {
                unused.push(arg_index);
            }
            continue;
        }
        while formal_index < count
            && formal_symbols[formal_index] != dots_symbol
            && matched[formal_index].is_some()
        {
            formal_index += 1;
        }
        if formal_index < count && formal_symbols[formal_index] != dots_symbol {
            matched[formal_index] = Some(arg.value);
            formal_index += 1;
        } else if dots_position.is_some() {
            dots_items.push((arg.tag, arg.value));
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
    let mut result: Vec<Sexp> = matched
        .into_iter()
        .map(|value| value.unwrap_or(missing))
        .collect();
    if let Some(position) = dots_position {
        result[position] = if dots_items.is_empty() {
            missing
        } else {
            let mut list = nil();
            for &(tag, value) in dots_items.iter().rev() {
                list = triple(SexpType::Dots, value, list, tag)?;
            }
            list
        };
    }
    Ok(result)
}

/// Whether `symbol` is missing in `env`: unbound to a supplied argument, or bound to a promise of
/// a symbol that is itself missing in the caller.
pub unsafe fn is_missing(symbol: Sexp, env: Sexp) -> R<bool> {
    symbol_is_missing(symbol, env, true)
}

/// `missing()` of `symbol` in `env`. `defaults_count` says whether a formal that got its default
/// counts as missing: it does for the function asking, but not for a caller its argument came
/// from, so `h(where)` with `where` defaulted passes a non-missing argument to `h`.
unsafe fn symbol_is_missing(symbol: Sexp, env: Sexp, defaults_count: bool) -> R<bool> {
    let heap = crate::heap();
    let (symbol, index) = match dot_dot_index(symbol) {
        Some(index) => (symbols().dots, Some(index)),
        None => (symbol, None),
    };
    let Some(cell) = heap.frame_cell(env, symbol) else {
        return Err(error(format!(
            "'missing' can only be used for arguments"
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
    if defaults_count && cell.has_gp(BINDING_MISSING) {
        return Ok(true);
    }
    value_is_missing(cell.cdr())
}

unsafe fn value_is_missing(value: Sexp) -> R<bool> {
    if value == missing_arg() {
        return Ok(true);
    }
    if value.kind() == SexpType::Promise
        && value.car() == unbound()
        && value.cdr().kind() == SexpType::Symbol
        && value.tag().kind() == SexpType::Environment
    {
        let code = value.cdr();
        let caller = value.tag();
        if crate::heap().frame_cell(caller, code).is_some() {
            return symbol_is_missing(code, caller, false);
        }
    }
    Ok(false)
}

// The control-flow specials.

pub const BUILTINS: &[BuiltinDef] = &[
    special("{", -1, do_begin).keep_visibility(),
    special("(", 1, do_paren),
    special("if", -1, do_if).keep_visibility(),
    special("for", -1, do_for).invisible(),
    special("while", -1, do_while).invisible(),
    special("repeat", -1, do_repeat).invisible(),
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
    internal("eval", 3, do_eval).keep_visibility(),
    internal("withVisible", 1, do_with_visible),
    internal("Recall", -1, do_recall).keep_visibility(),
    builtin(".Primitive", 1, do_primitive),
    internal("identity", 1, |call| unsafe { Ok(call.arg(0)) }),
    special("~", -1, do_tilde),
    internal("delayedAssign", 4, do_delayed_assign).invisible(),
    internal("makeLazy", 5, do_make_lazy).invisible(),
    builtin("standardGeneric", -1, dispatch::do_standard_generic).keep_visibility(),
    builtin("call", -1, do_call),
    special("expression", -1, do_expression),
    internal("all.names", 4, do_all_names),
    internal("returnValue", 1, |call| Ok(call.arg(0))),
    internal("named", 1, |call| unsafe { values::scalar_integer(call.arg(0).refcount().min(2) as i32) }),
    internal("refcnt", 1, |call| unsafe { values::scalar_integer(call.arg(0).refcount() as i32) }),
    internal("address", 1, |call| unsafe { values::scalar_string(&format!("{:p}", call.arg(0).as_ptr())) }),
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
    let function = values::install_bytes(name.element(0).bytes())?;
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
            .map(|arg| if arg.tag == nil() { String::new() } else { symbol_name(arg.tag).into_owned() })
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
                if functions {
                    walk(expression.car(), functions, names);
                } else if expression.car().kind() == SexpType::Language {
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
        .filter(|definition| internal_too || matches!(definition.kind, Kind::Builtin | Kind::Special))
        .map(|definition| definition.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    values::string_vector(&names)
}

unsafe fn do_begin(call: &Call<'_>) -> R<Sexp> {
    let mut value = nil();
    interp().visible = true;
    for arg in call.args {
        value = eval(arg.value, call.env)?;
    }
    Ok(value)
}

unsafe fn do_paren(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(1)?;
    eval(call.arg(0), call.env)
}

/// The condition of `if` and `while`, with GNU R's messages.
pub unsafe fn as_condition(value: Sexp, call: Sexp) -> R<bool> {
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
                SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::String | SexpType::Complex | SexpType::Raw
            ) {
                return Err(error_in(
                    call,
                    format!("argument is of length zero"),
                ));
            }
            Ok(condition != 0)
        }
    }
}

unsafe fn do_if(call: &Call<'_>) -> R<Sexp> {
    let condition = eval(call.arg(0), call.env)?;
    if as_condition(condition, call.call)? {
        eval(call.arg(1), call.env)
    } else if call.len() > 2 {
        eval(call.arg(2), call.env)
    } else {
        interp().visible = false;
        Ok(nil())
    }
}

/// Runs one loop iteration and reports whether the loop goes on: `Ok(false)` after `break`.
unsafe fn loop_body(body: Sexp, env: Sexp) -> R<bool> {
    let result = scope(|| eval(body, env).map(|_| ()));
    match result {
        Ok(()) => Ok(true),
        Err(Unwind::Jump(jump)) => match *jump {
            Jump::Break { env: target } if target == env => Ok(false),
            Jump::Next { env: target } if target == env => Ok(true),
            other => Err(Unwind::Jump(Box::new(other))),
        },
        Err(other) => Err(other),
    }
}

unsafe fn do_for(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let variable = call.arg(0);
    if variable.kind() != SexpType::Symbol {
        return Err(call.error("invalid for() loop sequence"));
    }
    let mut sequence = eval(call.arg(1), env)?;
    let body = call.arg(2);
    if values::inherits(sequence, "factor") {
        sequence = crate::coerce::factor_as_character(sequence)?;
    }
    let sequence = values::materialize(sequence)?;
    let count = match sequence.kind() {
        SexpType::Nil => 0,
        SexpType::Pairlist | SexpType::Language => values::pairlist_length(sequence),
        kind if kind.is_vector() => sequence.length() as usize,
        _ => return Err(call.error("invalid for() loop sequence")),
    };
    let mut cell = sequence;
    for index in 0..count {
        let element = match sequence.kind() {
            SexpType::List | SexpType::Expression => sequence.element(index),
            SexpType::Pairlist | SexpType::Language => {
                let value = cell.car();
                cell = cell.cdr();
                value
            }
            _ => crate::subset::vector_element(sequence, index)?,
        };
        define(variable, element, env)?;
        if !loop_body(body, env)? {
            break;
        }
    }
    Ok(nil())
}

unsafe fn do_while(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    loop {
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
    while loop_body(call.arg(0), call.env)? {}
    Ok(nil())
}

unsafe fn do_break(call: &Call<'_>) -> R<Sexp> {
    Err(jump(Jump::Break { env: call.env }))
}

unsafe fn do_next(call: &Call<'_>) -> R<Sexp> {
    Err(jump(Jump::Next { env: call.env }))
}

unsafe fn do_return(call: &Call<'_>) -> R<Sexp> {
    let value = match call.args {
        [] => nil(),
        [arg] => eval(arg.value, call.env)?,
        _ => return Err(call.error("multi-argument returns are not permitted")),
    };
    // GNU R looks for the function context before jumping, so a `return` with no function to
    // return from is an ordinary error that `tryCatch` can catch.
    if !interp().frames.iter().any(|frame| frame.env == call.env) {
        return Err(error("no function to return from, jumping to top level"));
    }
    Err(jump(Jump::Return {
        env: call.env,
        value,
    }))
}

unsafe fn do_function(call: &Call<'_>) -> R<Sexp> {
    let formals = call.arg(0);
    let body = call.arg(1);
    make_closure(formals, body, call.env)
}

pub unsafe fn make_closure(formals: Sexp, body: Sexp, env: Sexp) -> R<Sexp> {
    if !matches!(formals.kind(), SexpType::Nil | SexpType::Pairlist) {
        return Err(error("invalid formal argument list for \"function\""));
    }
    triple(SexpType::Closure, formals, body, env)
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
pub unsafe fn logical_operand_of(value: Sexp, call: Sexp, operator: &str, side: &str) -> R<i32> {
    if !matches!(
        value.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex | SexpType::Nil | SexpType::String | SexpType::Raw
    ) {
        return Err(error_in(call, format!("invalid '{side}' type in 'x {operator} y'")));
    }
    let count = length(value);
    if count > 1 {
        return Err(error_in(call, format!("'length = {count}' in coercion to 'logical(1)'")));
    }
    if value.kind() == SexpType::String {
        let result = values::as_logical_scalar(value);
        if count == 1 && result == heap::object::NA_LOGICAL {
            return Err(error_in(call, format!("invalid '{side}' type in 'x {operator} y'")));
        }
        return Ok(result);
    }
    Ok(values::as_logical_scalar(value))
}

unsafe fn logical_operator(call: &Call<'_>, is_and: bool) -> R<Sexp> {
    let left = eval(call.arg(0), call.env)?;
    let left = logical_operand(left, call, "x")?;
    let na = heap::object::NA_LOGICAL;
    if is_and && left == 0 {
        return values::scalar_logical(0);
    }
    if !is_and && left == 1 {
        return values::scalar_logical(1);
    }
    let right = eval(call.arg(1), call.env)?;
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
    let selector = eval(first.value, call.env)?;
    // `switch(x, ...)` forwards the caller's alternatives, so `...` expands to its promises.
    let mut alternatives: Vec<Arg> = Vec::with_capacity(call.args.len() - 1);
    for arg in &call.args[1..] {
        if arg.value == symbols().dots && arg.tag == nil() {
            let dots = find_var(symbols().dots, call.env)?;
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
    let alternatives = &alternatives[..];
    if !values::is_string_scalar(selector) && length(selector) != 1 {
        return Err(call.error("EXPR must be a length 1 vector"));
    }
    if selector.kind() == SexpType::String {
        let text = values::string_at(selector, 0);
        let missing = missing_arg();
        if let Some(text) = text {
            for (position, arg) in alternatives.iter().enumerate() {
                if arg.tag.kind() == SexpType::Symbol && symbol_name(arg.tag) == text.as_ref() {
                    // Fall through empty alternatives to the next non-empty one.
                    for later in &alternatives[position..] {
                        if later.value != missing {
                            return eval(later.value, call.env);
                        }
                    }
                    return Err(call.error("empty alternative in numeric switch"));
                }
            }
        }
        let defaults: Vec<&Arg> = alternatives
            .iter()
            .filter(|arg| arg.tag == nil() || symbol_name(arg.tag).is_empty())
            .collect();
        if defaults.len() > 1 {
            return Err(call.error("duplicate 'switch' defaults: '"
                .to_string()
                + &deparse::deparse_one_line(defaults[0].value)
                + "' and '"
                + &deparse::deparse_one_line(defaults[1].value)
                + "'"));
        }
        if let Some(default) = defaults.first() {
            if default.value == missing {
                interp().visible = false;
                return Ok(nil());
            }
            return eval(default.value, call.env);
        }
        interp().visible = false;
        return Ok(nil());
    }
    let index = values::as_integer_scalar(selector);
    if index >= 1 && (index as usize) <= alternatives.len() {
        let arg = alternatives[index as usize - 1];
        if arg.value == missing_arg() {
            return Err(call.error("empty alternative in numeric switch"));
        }
        return eval(arg.value, call.env);
    }
    interp().visible = false;
    Ok(nil())
}

unsafe fn do_missing(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(1)?;
    let mut symbol = call.arg(0);
    if symbol.kind() == SexpType::String && length(symbol) == 1 {
        symbol = values::install_bytes(symbol.element(0).bytes())?;
    }
    if symbol.kind() != SexpType::Symbol {
        return Err(call.error("invalid use of 'missing'"));
    }
    values::scalar_bool(is_missing(symbol, call.env)?)
}

unsafe fn do_on_exit(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["expr", "add", "after"])?;
    let expression = crate::builtins::present(matched[0]).unwrap_or(nil());
    let add = match crate::builtins::present(matched[1]) {
        Some(value) => values::as_logical_scalar(eval(value, call.env)?) == 1,
        None => false,
    };
    let after = match crate::builtins::present(matched[2]) {
        Some(value) => values::as_logical_scalar(eval(value, call.env)?) != 0,
        None => true,
    };
    let env = call.env;
    let state = interp();
    let Some(frame) = state.frames.iter_mut().rev().find(|frame| frame.env == env) else {
        return Ok(nil());
    };
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
        evaluate_args(inner.cdr(), call.env, definition.keep_missing, inner)?
    } else {
        values::pairlist_items(inner.cdr())
            .into_iter()
            .map(|(tag, value)| Arg { tag, value })
            .collect()
    };
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, inner, function, &args, call.env)?
    {
        return Ok(value);
    }
    call_builtin(definition, inner, function, &args, call.env)
}

unsafe fn do_invisible(call: &Call<'_>) -> R<Sexp> {
    Ok(call.arg(0))
}

unsafe fn do_nargs(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let count = interp()
        .frames
        .iter()
        .rev()
        .find(|frame| frame.env == env)
        .map_or(0, |frame| values::pairlist_length(frame.promargs));
    values::scalar_integer(count as i32)
}

unsafe fn do_force_and_call(call: &Call<'_>) -> R<Sexp> {
    let count = values::as_integer_scalar(eval(call.arg(0), call.env)?).max(0) as usize;
    let function = eval(call.arg(1), call.env)?;
    let rest = values::pairlist(
        &call.args[2..]
            .iter()
            .map(|arg| (arg.tag, arg.value))
            .collect::<Vec<_>>(),
        SexpType::Pairlist,
    )?;
    let inner_call = values::lcons(call.arg(1), rest)?;
    match function.kind() {
        SexpType::Closure => {
            let args = promise_args(rest, call.env)?;
            for arg in args.iter().take(count) {
                if arg.value.kind() == SexpType::Promise {
                    force_promise(arg.value)?;
                }
            }
            apply_closure(inner_call, function, &args, call.env, None)
        }
        _ => apply_call(inner_call, function, rest, call.env),
    }
}

/// `R_forceAndCall`: evaluates `call` in `env`, forcing the promises of a closure's first
/// `count` arguments before its body runs. The apply family calls `FUN` this way, so a closure
/// that captures its argument sees this iteration's element, not the last one.
pub unsafe fn force_and_call(call: Sexp, count: usize, env: Sexp) -> R<Sexp> {
    let head = call.car();
    let function = if head.kind() == SexpType::Symbol { find_fun(head, env)? } else { eval(head, env)? };
    crate::heap().root(function);
    if function.kind() != SexpType::Closure {
        return apply_call(call, function, call.cdr(), env);
    }
    let args = promise_args(call.cdr(), env)?;
    for arg in args.iter().take(count) {
        if arg.value.kind() == SexpType::Promise {
            force_promise(arg.value)?;
        }
    }
    apply_closure(call, function, &args, env, None)
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
            env = crate::envir::sys_frame(which, call.env)?;
        }
        _ => return Err(call.error("invalid 'envir' argument of type '".to_string() + env.kind().name() + "'")),
    }
    // `eval` runs its expression in a context of its own, as GNU R's does: it counts as a frame
    // for `sys.call` and friends, and a `return` in the expression returns from `eval`.
    let state = interp();
    let caller = state.frames.last().map_or(nil(), |frame| frame.call);
    let handlers = state.handlers.clone();
    let restarts = state.restarts.clone();
    state.frames.push(Frame {
        call: caller,
        function: call.op,
        env,
        sys_parent: call.env,
        promargs: nil(),
        on_exit: Vec::new(),
        handlers: handlers.clone(),
        restarts: restarts.clone(),
    });
    let depth = state.frames.len();
    let result = scope(|| match expression.kind() {
        SexpType::Expression => {
            let mut value = nil();
            for index in 0..length(expression) {
                value = eval(expression.element(index), env)?;
            }
            Ok(value)
        }
        SexpType::Promise => force_promise(expression),
        _ => eval(expression, env),
    });
    let result = match result {
        Err(unwind) if unwind.returns_to(env) => {
            let Unwind::Jump(jump) = unwind else {
                unreachable!()
            };
            match *jump {
                Jump::Return { value, .. } => {
                    crate::heap().root(value);
                    Ok(value)
                }
                _ => unreachable!(),
            }
        }
        other => other,
    };
    let result = run_on_exit(depth, result);
    let state = interp();
    state.frames.truncate(depth - 1);
    state.handlers = handlers;
    state.restarts = restarts;
    result
}

unsafe fn do_with_visible(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    let visible = interp().visible;
    let flag = values::scalar_bool(visible)?;
    values::named_list(&[("value", value), ("visible", flag)])
}

unsafe fn do_recall(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let state = interp();
    let Some(frame) = state.frames.iter().rev().find(|frame| frame.env == env) else {
        return Err(call.error("'Recall' called from outside a closure"));
    };
    let sys_parent = frame.sys_parent;
    let caller = state
        .frames
        .iter()
        .rev()
        .find(|candidate| candidate.env == sys_parent);
    let function = match caller {
        Some(caller) => caller.function,
        None => return Err(call.error("'Recall' called from outside a closure")),
    };
    let caller_call = caller.map_or(nil(), |caller| caller.call);
    apply(caller_call, function, call.args, sys_parent)
}

unsafe fn do_primitive(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_string(call.arg(0), "name")?;
    crate::builtins::primitive(&name)
        .ok_or_else(|| call.error(format!("no such primitive function")))
}

/// A formula: the call itself, with class `formula` and the environment it was made in.
unsafe fn do_tilde(call: &Call<'_>) -> R<Sexp> {
    let formula = values::duplicate(call.call)?;
    if values::get_attr(formula, symbols().class) == nil() {
        let class = values::scalar_string("formula")?;
        values::set_attr(formula, symbols().class, class)?;
        let environment = values::install(".Environment")?;
        values::set_attr(formula, environment, call.env)?;
    }
    Ok(formula)
}

unsafe fn do_delayed_assign(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_string(call.arg(0), "x")?;
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
    let key_symbol = values::install("key")?;
    for index in 0..length(names) {
        let symbol = values::install(&values::string_at(names, index).unwrap_or_default())?;
        let key = crate::subset::vector_element(keys, index)?;
        let expression_copy = values::duplicate(expression)?;
        let env = new_env(eval_env)?;
        crate::heap().define(env, key_symbol, key)?;
        let promise = make_promise(expression_copy, env)?;
        define(symbol, promise, assign_env)?;
    }
    Ok(nil())
}

unsafe fn do_substitute(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["expr", "env"])?;
    let expression = crate::builtins::present(matched[0]).unwrap_or(missing_arg());
    let env = match crate::builtins::present(matched[1]) {
        Some(env) => eval(env, call.env)?,
        None => call.env,
    };
    let env = match env.kind() {
        SexpType::Environment => env,
        SexpType::List => crate::envir::list_to_env(env, values::empty_env())?,
        SexpType::Nil => values::empty_env(),
        _ => return Err(call.error("invalid environment specified")),
    };
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
                return Ok(value.cdr());
            }
            if value.kind() == SexpType::Dots {
                return Ok(expression);
            }
            if value == missing_arg() {
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
    let mut cell = list;
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        let element = cell.car();
        if element == dots && env != values::global_env() {
            let value = crate::heap().frame_cell(env, dots).map(|cell| cell.cdr());
            match value {
                Some(value) if value.kind() == SexpType::Dots => {
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
    let result = values::pairlist(&items, list.kind())?;
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
        target = values::install_bytes(target.element(0).bytes())?;
    }
    match target.kind() {
        SexpType::Symbol => {
            let value = eval(call.arg(1), call.env)?;
            assign_symbol(target, value, call.env, superassign)?;
            Ok(value)
        }
        SexpType::Language => {
            let value = eval(call.arg(1), call.env)?;
            complex_assign(call.call, target, value, call.env, superassign)?;
            Ok(value)
        }
        _ => Err(call.error("invalid (do_set) left-hand side to assignment")),
    }
}

pub unsafe fn assign_symbol(symbol: Sexp, value: Sexp, env: Sexp, superassign: bool) -> R<()> {
    if superassign {
        set_var(symbol, value, env.enclosure())
    } else {
        let heap = crate::heap();
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                let function = cell.cdr();
                apply(nil(), function, &[Arg { tag: nil(), value }], values::global_env())?;
                return Ok(());
            }
            heap.set_binding_value(cell, value)?;
            cell.set_gp_bits(BINDING_MISSING, false);
            return Ok(());
        }
        define(symbol, value, env)
    }
}

/// `f(x, ...) <- value`: evaluates the getter chain down to the variable, then applies the
/// replacement functions from the outside in and assigns the result to the variable.
pub unsafe fn complex_assign(
    whole: Sexp,
    target: Sexp,
    value: Sexp,
    env: Sexp,
    superassign: bool,
) -> R<()> {
    let symbols = symbols();
    let mut getters = Vec::new();
    let mut inner = target;
    while inner.kind() == SexpType::Language {
        getters.push(inner);
        inner = inner.cdr().car();
    }
    if inner.kind() == SexpType::String && length(inner) == 1 {
        inner = values::install_bytes(inner.element(0).bytes())?;
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
    let local_binding = crate::heap().frame_cell(env, inner).is_some();
    if values::maybe_shared(base) || (!superassign && !local_binding) {
        base = values::shallow_duplicate(base)?;
    }
    // Values of each level, innermost first: the variable, then each getter applied to it.
    let mut levels = vec![base];
    let tmp_symbol = symbols.tmp;
    let mut shared = false;
    for getter in getters.iter().skip(1).rev() {
        let previous = *levels.last().unwrap_or(&base);
        let promise = forced_promise(tmp_symbol, previous)?;
        crate::heap().untrack_references(promise);
        let getter_call = values::lcons(getter.car(), values::cons_tagged(promise, getter.cdr().cdr(), getter.cdr().tag())?)?;
        let mut level = eval(getter_call, env)?;
        if values::maybe_shared(level) || shared {
            shared = true;
            level = values::shallow_duplicate(level)?;
        }
        levels.push(level);
    }
    let mut replacement = value;
    for (depth, getter) in getters.iter().enumerate() {
        let object = levels[levels.len() - 1 - depth];
        let setter = replacement_function(getter.car(), whole)?;
        let object_code = if depth + 1 == getters.len() {
            tmp_symbol
        } else {
            getters[depth + 1]
        };
        // The object promise is the `*tmp*` GNU R binds: it must not count as a reference, or
        // the replacement function would always see its target as shared and copy it.
        let object_promise = forced_promise(object_code, object)?;
        crate::heap().untrack_references(object_promise);
        let value_promise = forced_promise(replacement, replacement)?;
        let mut items = vec![(nil(), setter), (nil(), object_promise)];
        for (tag, argument) in values::pairlist_items(getter.cdr().cdr()) {
            items.push((tag, argument));
        }
        items.push((symbols.value, value_promise));
        let setter_call = values::pairlist(&items, SexpType::Language)?;
        replacement = eval(setter_call, env)?;
    }
    assign_symbol(inner, replacement, env, superassign)
}

/// The replacement function for a getter: `f<-` for `f`, and `pkg::\`f<-\`` for `pkg::f`.
unsafe fn replacement_function(getter: Sexp, whole: Sexp) -> R<Sexp> {
    match getter.kind() {
        SexpType::Symbol => {
            let mut name = getter.car().bytes().to_vec();
            name.extend_from_slice(b"<-");
            values::install_bytes(&name)
        }
        SexpType::Language
            if (getter.car() == symbols().double_colon || getter.car() == symbols().triple_colon)
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

/// Evaluates every top-level expression of `expressions` in `env`, returning the last value.
pub unsafe fn eval_all(expressions: Sexp, env: Sexp) -> R<Sexp> {
    let mut value = nil();
    for index in 0..length(expressions) {
        value = eval(expressions.element(index), env)?;
    }
    Ok(value)
}

/// Makes a fresh, empty `DOTSXP`-free argument list from evaluated values.
pub fn value_args(values: &[Sexp]) -> Vec<Arg> {
    values
        .iter()
        .map(|&value| Arg {
            tag: nil(),
            value,
        })
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
