//! The language-object evaluator: symbols, promises, calls, closures, argument matching and the
//! control-flow specials. It is the semantic reference the bytecode interpreter must agree with,
//! and it runs everything the bytecode does not compile: promise code, `eval()` of constructed
//! calls, and top-level expressions.

use {
    crate::{
        Frame,
        builtins::{Arg, BuiltinDef, Call, Generic, Kind, Visibility, builtin, internal, special},
        bytecode, conditions, deparse, dispatch, interp, scope,
        unwind::{Jump, R, Unwind, error, error_in, jump},
        utils::{PROFILE_TICK, record_profile_sample},
        values::{
            self, alloc, cons_tagged, length, missing_arg, new_env, nil, symbol_name, symbols,
            triple, unbound,
        },
    },
    heap::{
        Formal,
        object::{Sexp, SexpType, gp},
    },
    smallvec::{SmallVec, smallvec},
    std::sync::atomic::Ordering,
};

/// A call's arguments as values or promises. Inline for the common few, so a closure call
/// allocates no vector for them.
pub type Args = SmallVec<[Arg; 10]>;

/// What [`match_args`] returns: one value per formal.
pub type Matched = SmallVec<[Sexp; 8]>;

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
        return Err(error_in(
            lexical_call(env),
            format!("object '{}' not found", symbol_name(symbol)),
        ));
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
unsafe fn lexical_call(env: Sexp) -> Sexp {
    let frames = &interp().frames;
    if let Some(frame) = frames.iter().rev().find(|frame| frame.env == env) {
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
pub unsafe fn find_fun(symbol: Sexp, env: Sexp) -> R<Sexp> {
    find_called_fun(symbol, env, unbound())
}

/// `findFun3`: [`find_fun`] for the head of `call`, which a missing function's error names, as
/// in GNU R.
pub unsafe fn find_called_fun(symbol: Sexp, mut env: Sexp, call: Sexp) -> R<Sexp> {
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
            call_builtin(definition, call, function, &args, env)
        }
        SexpType::Closure => {
            let mut args = Args::new();
            promise_args_into(arguments, env, &mut args)?;
            apply_closure(call, function, &args, env, None)
        }
        _ => Err(error_in(call, "attempt to apply non-function")),
    }
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

/// [`call_builtin`], offering the builtin `reusable` to update in place (see [`Call::reusable`]).
unsafe fn call_builtin_reusing(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
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
    if matches!(definition.kind, Kind::Builtin | Kind::Special)
        && args.iter().any(|arg| arg.tag != nil())
    {
        check_argument_names(definition, call, args)?;
    }
    if PROFILE_TICK.load(Ordering::Relaxed) {
        record_profile_sample(Some(definition.name))?;
    }
    let function = definition.function;
    let visibility = definition.visibility;
    let context = Call {
        call,
        op,
        args,
        env,
        reusable,
    };
    let result = if !definition.accepts_native
        && (matches!(definition.kind, Kind::Builtin | Kind::Internal)
            || definition.is_dispatch_or_eval())
        && let Err(refused) = crate::records::check_native_args(call, args)
    {
        Err(refused)
    } else {
        scope(|| function(&context))
    };
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
    let state = interp();
    let formals = match state.primitive_formals.get(definition.name) {
        Some(formals) => formals,
        None => {
            let Some(formals) = recorded_formals(definition.name)? else {
                return Ok(());
            };
            interp()
                .primitive_formals
                .entry(definition.name)
                .or_insert(formals)
        }
    };
    let Some(formals) = formals else {
        return Ok(());
    };
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
pub unsafe fn promise_args_into(mut arguments: Sexp, env: Sexp, args: &mut Args) -> R<()> {
    let dots = symbols().dots;
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
            // A constant gets a promise too, as in GNU R: native code such as lazyeval reads a
            // promise's environment to tell whether it was forced.
            let value = if expression == missing_arg() {
                expression
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
    let formals = function.car();
    let heap = crate::heap();
    let env = heap.alloc_environment_with_capacity(function.tag(), values::length(formals))?;
    heap.root(env);
    env.set_gp_bits(gp::FUNCTION_FRAME, true);
    let mut bindings: SmallVec<[Formal; 12]> = SmallVec::new();
    if supplied.is_none() && positional(formals, args) {
        // Most calls pass every argument by position to a function without `...`, which needs no
        // matching table: argument `i` binds formal `i`, and the formals past the last argument
        // are missing.
        let mut formal = formals;
        let mut index = 0;
        while formal.kind() == SexpType::Pairlist {
            let actual = args.get(index).map_or(missing_arg(), |arg| arg.value);
            bindings.push(formal_binding(env, formal, actual)?);
            formal = formal.cdr();
            index += 1;
        }
    } else {
        let actuals = match_args(formals, args, call)?;
        let mut formal = formals;
        for &actual in &actuals {
            bindings.push(formal_binding(env, formal, actual)?);
            formal = formal.cdr();
        }
    }
    heap.define_formals(env, &bindings)?;
    heap.mark_frame_formals(env);
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
/// its default when the argument is missing.
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
            let promise = make_promise(default, env)?;
            // The promises of later defaults are allocated before this one is bound.
            crate::heap().root(promise);
            promise
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

/// The arguments as a pairlist of their values or promises, which a frame records.
pub unsafe fn args_pairlist(args: &[Arg]) -> R<Sexp> {
    let mut list = nil();
    for arg in args.iter().rev() {
        list = cons_tagged(arg.value, list, arg.tag)?;
    }
    Ok(list)
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
    if !state.object_systems {
        refuse_object_system(call, function, sys_parent)?;
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
        srcref: state.srcref,
    });
    if PROFILE_TICK.load(Ordering::Relaxed) {
        record_profile_sample(None)?;
    }
    let depth = state.frames.len();
    let caller_srcref = state.srcref;
    state.srcref = values::get_attr(function, symbols().srcref);
    let signature = crate::annotations::enter(call, function, env, depth);
    let result = signature.map(|signature| {
        let result = scope(|| {
            stacker::maybe_grow(128 * 1024, 4 * 1024 * 1024, || {
                let body = function.cdr();
                bytecode::run_body(function, body, env)
            })
        });
        (signature, result)
    });
    let (signature, result) = match result {
        Ok(pair) => pair,
        Err(unwind) => (None, Err(unwind)),
    };
    let result = stray_loop_jump(call, env, catch_return(env, result));
    let result = match (signature, result) {
        (Some(signature), Ok(value)) => {
            crate::annotations::check_return(call, signature, value).map(|()| value)
        }
        (_, result) => result,
    };
    crate::annotations::leave(depth);
    // `return()` may also come from an `on.exit` expression, which runs in the same frame and
    // so returns from it too.
    let result = catch_return(env, run_on_exit(depth, result));
    let state = interp();
    // The frame's saved stack, which `.addGlobHands` may have rewritten.
    state.handlers = state.frames[depth - 1].handlers.clone();
    state.frames.truncate(depth - 1);
    state.restarts = restarts;
    state.srcref = caller_srcref;
    result
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
        Err(Unwind::Jump(jump)) if matches!(*jump, Jump::Break { env: target } | Jump::Next { env: target } if target == env) => {
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
            let Jump::Return { value, .. } = *jump else {
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
        match exit {
            // `on.exit(return(x))` makes `x` the frame's value, even over an error unwinding
            // through it, as in GNU R.
            Err(Unwind::Jump(jump)) if matches!(*jump, Jump::Return { env: target, .. } if target == env) => {
                if let Jump::Return { value, .. } = *jump {
                    outcome = Ok(value);
                }
            }
            Err(unwind) => {
                outcome = Err(unwind);
                break;
            }
            Ok(_) => {}
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
pub unsafe fn match_args(formals: Sexp, args: &[Arg], call: Sexp) -> R<Matched> {
    let missing = missing_arg();
    let dots_symbol = symbols().dots;
    if let Some(matched) = match_positional_args(formals, args, missing, dots_symbol)? {
        return Ok(matched);
    }
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
    let mut matched: SmallVec<[Option<Sexp>; 8]> = smallvec![None; count];
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
    let mut partial_owner: SmallVec<[Option<usize>; 8]> = smallvec![None; args.len()];
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
                        format!(
                            "argument {} matches multiple formal arguments",
                            arg_index + 1
                        ),
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
    let mut dots_items: SmallVec<[(Sexp, Sexp); 8]> = SmallVec::new();
    let mut unused: SmallVec<[usize; 4]> = SmallVec::new();
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
    let mut result: Matched = matched
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

/// The common case of `match_args` without its bookkeeping: no argument is named, so they fill
/// the formals in order up to `...`, which collects the rest. Returns `None` when an argument is
/// named or one is left over, which the full matcher handles and reports.
unsafe fn match_positional_args(
    formals: Sexp,
    args: &[Arg],
    missing: Sexp,
    dots_symbol: Sexp,
) -> R<Option<Matched>> {
    let is_named =
        |arg: &Arg| arg.tag.kind() == SexpType::Symbol && !arg.tag.car().bytes().is_empty();
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
        return Err(error(
            "'missing' can only be used for arguments".to_string(),
        ));
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
    internal("identity", 1, |call| Ok(call.arg(0))),
    special("~", -1, do_tilde),
    internal("delayedAssign", 4, do_delayed_assign).invisible(),
    internal("makeLazy", 5, do_make_lazy).invisible(),
    builtin("standardGeneric", -1, dispatch::do_standard_generic).keep_visibility(),
    builtin("call", -1, do_call),
    special("expression", -1, do_expression),
    internal("all.names", 4, do_all_names),
    internal("returnValue", 1, |call| Ok(call.arg(0))),
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
    // Only the last value stays rooted, so an object a statement drops can be collected (and
    // finalized) by a `gc()` later in the same block.
    let mark = crate::heap().handle_mark();
    for (index, arg) in call.args.iter().enumerate() {
        if srcrefs != nil() {
            interp().srcref = statement_srcref(srcrefs, index + 1);
        }
        let result = eval(arg.value, call.env);
        interp().srcref = saved;
        crate::heap().release_handles(mark);
        match result {
            Ok(next) => {
                crate::heap().root(next);
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
        return eval(call.arg(0), call.env);
    }
    // `(...)` is a builtin call in GNU R, so the dots expand into its single argument.
    let args = evaluate_args(call.call.cdr(), call.env, false, call.call)?;
    let expanded = Call {
        args: &args,
        ..*call
    };
    expanded.check_arity(1)?;
    Ok(expanded.arg(0))
}

/// The condition of `if` and `while`, with GNU R's messages.
pub unsafe fn as_condition(value: Sexp, call: Sexp) -> R<bool> {
    if value.kind() == SexpType::Record {
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
    let condition = eval(call.arg(0), call.env)?;
    if condition.kind() == SexpType::Record {
        return Err(crate::records::boundary_error(
            condition,
            call.arg(0),
            call.call,
        ));
    }
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
        SexpType::Record => {
            return Err(crate::records::boundary_error(
                sequence,
                call.arg(1),
                call.call,
            ));
        }
        _ => return Err(call.error("invalid for() loop sequence")),
    };
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
        let element = match sequence.kind() {
            SexpType::List | SexpType::Expression => Ok(sequence.element(index)),
            SexpType::Pairlist | SexpType::Language => {
                let value = cell.car();
                cell = cell.cdr();
                Ok(value)
            }
            _ => crate::subset::vector_element(sequence, index),
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
    make_closure(call.arg(0), call.arg(1), call.arg(2), call.env)
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
pub unsafe fn logical_operand_of(value: Sexp, call: Sexp, operator: &str, side: &str) -> R<i32> {
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
            return Err(call.error(
                "duplicate 'switch' defaults: '".to_string()
                    + &deparse::deparse_one_line(defaults[0].value)
                    + "' and '"
                    + &deparse::deparse_one_line(defaults[1].value)
                    + "'",
            ));
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
        symbol = values::install_charsxp(symbol.element(0))?;
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
        unevaluated_args(inner.cdr())
    };
    if definition.generic != Generic::None
        && let Some(value) =
            dispatch::try_builtin_dispatch(definition, inner, function, &args, call.env)?
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
    force_and_call_with(call, count, env, None)
}

/// [`force_and_call`] where the caller already knows the first argument's value, so its
/// promise starts out forced instead of evaluating its expression. The caller must only pass a
/// value that evaluating the expression would produce without side effects.
pub unsafe fn force_and_call_with(
    call: Sexp,
    count: usize,
    env: Sexp,
    first: Option<Sexp>,
) -> R<Sexp> {
    let head = call.car();
    let function = if head.kind() == SexpType::Symbol {
        find_called_fun(head, env, call)?
    } else {
        eval(head, env)?
    };
    crate::heap().root(function);
    if function.kind() != SexpType::Closure {
        return apply_call(call, function, call.cdr(), env);
    }
    let args = promise_args(call.cdr(), env)?;
    if let (Some(value), Some(arg)) = (first, args.first())
        && arg.value.kind() == SexpType::Promise
        && arg.value.car() == unbound()
        && !arg.value.has_gp(gp::CHECKED_PROMISE)
    {
        let heap = crate::heap();
        heap.set_car(arg.value, value);
        heap.set_tag(arg.value, nil());
    }
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
    state.frames.push(Frame {
        call: caller,
        function: call.op,
        env,
        sys_parent: call.env,
        promargs: nil(),
        on_exit: Vec::new(),
        handlers: handlers.clone(),
        restarts: restarts.clone(),
        srcref: state.srcref,
    });
    let depth = state.frames.len();
    let result = scope(|| match expression.kind() {
        SexpType::Expression => {
            let srcrefs = values::get_attr(expression, symbols().srcref);
            let saved = interp().srcref;
            let mut value = nil();
            for index in 0..length(expression) {
                if srcrefs != nil() {
                    interp().srcref = statement_srcref(srcrefs, index);
                }
                let result = eval(expression.element(index), env);
                interp().srcref = saved;
                value = result?;
            }
            Ok(value)
        }
        SexpType::Promise => force_promise(expression),
        _ => eval(expression, env),
    });
    // `return()` may come from the body or from an `on.exit` expression, which runs in the
    // same frame and so returns from it too.
    let result = catch_return(env, run_on_exit(depth, catch_return(env, result)));
    let state = interp();
    // The frame's saved stack, which `.addGlobHands` may have rewritten.
    state.handlers = state.frames[depth - 1].handlers.clone();
    state.frames.truncate(depth - 1);
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
        .ok_or_else(|| call.error("no such primitive function".to_string()))
}

/// A formula: the call itself, with class `formula` and the environment it was made in.
unsafe fn do_tilde(call: &Call<'_>) -> R<Sexp> {
    let formula = values::duplicate(call.call)?;
    if values::get_attr(formula, symbols().class) == nil() {
        let class = values::scalar_string("formula")?;
        values::set_attr(formula, symbols().class, class)?;
        let environment = symbols().dot_environment;
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
        target = values::install_charsxp(target.element(0))?;
    }
    match target.kind() {
        SexpType::Symbol => {
            let value = match superassign {
                false => rebind(target, call.arg(1), call.env)?,
                true => eval(call.arg(1), call.env)?,
            };
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

/// Evaluates `value`, the right-hand side of `target <- value`. When it calls a builtin that
/// reuses its first argument with `target` itself, as in `d <- Dict.insert(d, k, v)`, and the
/// local binding holds the only reference to that value, the builtin may update it in place:
/// the old value becomes unreachable the moment the assignment completes, so nobody can observe
/// the change. This is how `Dict.insert`, `Set.insert` and `List.append` grow in amortized
/// constant time.
unsafe fn rebind(target: Sexp, value: Sexp, env: Sexp) -> R<Sexp> {
    if value.kind() != SexpType::Language
        || value.car().kind() != SexpType::Symbol
        || value.cdr().kind() != SexpType::Pairlist
        || value.cdr().car() != target
        || value.cdr().tag() != nil()
    {
        return eval(value, env);
    }
    // The rest is `eval_call`, split so a reusing builtin learns what it may reuse.
    let result = (|| {
        crate::unwind::poll_interrupt()?;
        interp().visible = true;
        let function = find_called_fun(value.car(), env, value)?;
        crate::heap().root(function);
        if function.kind() != SexpType::Builtin || !builtin_def(function)?.reuses_first_argument {
            return apply_call(value, function, value.cdr(), env);
        }
        reuse_first_argument(target, value, function, env)
    })();
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        result => result,
    }
}

unsafe fn reuse_first_argument(target: Sexp, value: Sexp, function: Sexp, env: Sexp) -> R<Sexp> {
    let definition = builtin_def(function)?;
    let args = evaluate_args(value.cdr(), env, definition.keep_missing, value)?;
    let reusable = args.first().map(|arg| arg.value).filter(|&subject| {
        subject.refcount() == 1
            && !args[1..].iter().any(|arg| arg.value == subject)
            && crate::heap().frame_cell(env, target).is_some_and(|cell| {
                !cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED) && cell.cdr() == subject
            })
    });
    call_builtin_reusing(definition, value, function, &args, env, reusable)
}

pub unsafe fn assign_symbol(symbol: Sexp, value: Sexp, env: Sexp, superassign: bool) -> R<()> {
    if superassign {
        set_var(symbol, value, env.enclosure())
    } else {
        let heap = crate::heap();
        if let Some(cell) = heap.frame_cell(env, symbol) {
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
    let local_binding = crate::heap().frame_cell(env, inner).is_some();
    if values::maybe_shared(base) || (!superassign && !local_binding) {
        // A copy-on-write copy is the same value, so an Array stays an Array; the
        // replacement function decides whether the write keeps it one.
        let was_array = base.is_array();
        base = values::shallow_duplicate(base)?;
        base.set_array(was_array);
    }
    // Values of each level, innermost first: the variable, then each getter applied to it.
    let mut levels = vec![base];
    let tmp_symbol = symbols.tmp;
    let mut shared = false;
    for getter in getters.iter().skip(1).rev() {
        let previous = *levels.last().unwrap_or(&base);
        let promise = forced_promise(tmp_symbol, previous)?;
        crate::heap().untrack_references(promise);
        let getter_call = values::lcons(
            getter.car(),
            values::cons_tagged(promise, getter.cdr().cdr(), getter.cdr().tag())?,
        )?;
        let mut level = eval(getter_call, env)?;
        if values::maybe_shared(level) || shared {
            shared = true;
            let was_array = level.is_array();
            level = values::shallow_duplicate(level)?;
            level.set_array(was_array);
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
        let frame_depth = interp().frames.len();
        let outer = interp()
            .assignment_call
            .replace((setter_call, whole, frame_depth));
        let result = apply_setter(setter_call, whole, env);
        interp().assignment_call = outer;
        replacement = result?;
    }
    assign_symbol(inner, replacement, env, superassign)
}

/// Evaluates one replacement call of a complex assignment. A builtin replacement function
/// reports its errors and warnings against the whole assignment, as `x[1:2] <- 1:3` and not as
/// the generated `` `[<-`(`*tmp*`, 1:2, value = 1:3) ``, and so does a dispatching special that
/// finds no method. S3 dispatch, the other specials (which dispatch from inside) and closures
/// see the generated call, which is the one `sys.call()` and `match.call()` must return.
unsafe fn apply_setter(setter_call: Sexp, whole: Sexp, env: Sexp) -> R<Sexp> {
    let head = setter_call.car();
    let function = if head.kind() == SexpType::Symbol {
        find_fun(head, env)?
    } else {
        eval(head, env)?
    };
    crate::heap().root(function);
    let arguments = setter_call.cdr();
    match function.kind() {
        SexpType::Builtin => {
            let definition = builtin_def(function)?;
            let args = evaluate_args(arguments, env, definition.keep_missing, setter_call)?;
            if definition.generic != Generic::None
                && let Some(value) =
                    dispatch::try_builtin_dispatch(definition, setter_call, function, &args, env)?
            {
                return Ok(value);
            }
            call_builtin(definition, whole, function, &args, env)
        }
        SexpType::Special if builtin_def(function)?.is_dispatch_or_eval() => {
            let definition = builtin_def(function)?;
            let mut args = Args::new();
            match dispatch::dispatch_or_eval(
                definition,
                setter_call,
                function,
                arguments,
                env,
                &mut args,
            )? {
                Some(value) => Ok(value),
                None => call_builtin(definition, whole, function, &args, env),
            }
        }
        _ => apply_call(setter_call, function, arguments, env),
    }
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
