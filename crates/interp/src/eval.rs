//! The language-object evaluator: symbols, promises, calls, closures, argument matching and the
//! control-flow specials. It is the semantic reference the bytecode interpreter must agree with,
//! and it runs everything the bytecode does not compile: the arguments of uncompiled code,
//! `eval()` of constructed calls, and top-level expressions.

use {
    crate::{
        Frame, Signalling,
        annotations::Entry,
        builtins::{
            Arg, BuiltinDef, Call, DirectFn, Generic, Kind, NativeArgs, Replacement, Visibility,
            builtin, internal, internal_special, special,
        },
        bytecode,
        coerce::{CoercionWarnings, real_to_int},
        conditions, deparse, dispatch,
        handles::{
            self, AssignmentCall, AssignmentCopy, Binding, Bytecode, Cells, Chars, Closure, Cx,
            Environment, Expression, Gc, Held, Integer, Language, List, Local, LocalBinding, Owned,
            Pairlist, Pointer, Promise, Real, Str, StringFill, Symbol, Tagged, Value, safe,
        },
        interp, moves, scope,
        unwind::{ArgumentReader, Jump, R, Rooted, Unwind, error, error_in, missing_argument},
        utils::record_profile_sample,
        values::{self, alloc, length, missing_arg, nil, symbol_name, symbols, triple, unbound},
    },
    heap::{
        ENVIRONMENT_BYTES, PendingAccess, TRIPLE_BYTES,
        activity::{self, Activity, Counter},
        object::{NA_INTEGER, Sexp, SexpType, gp},
    },
    smallvec::{SmallVec, smallvec},
    std::{cell::Cell, mem::MaybeUninit},
    thin_vec::ThinVec,
};

/// A call's arguments as values or promises. Inline for the common few, so a closure call
/// allocates no vector for them.
pub type Args = SmallVec<[Arg; 10]>;

/// What [`match_args`] returns: one value per formal.
pub type Matched<'s> = SmallVec<[Local<'s>; 8]>;

/// The promise bit set while its code runs, to report recursive default references.
pub const PROMISE_SEEN: u16 = 1 << 0;

/// The promise bit set when its code left through an error or another jump, so the next force
/// warns as GNU R does for a `PRSEEN` of 2. A read that settles a promise without forcing it
/// leaves such a promise to forcing, which gives the warning.
pub const PROMISE_INTERRUPTED: u16 = 1 << 1;

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
    eval_symbol_reporting(symbol, env, || lexical_call(env), ArgumentReader::Eval)
}

/// [`symbol_value_reporting`] for raw code: the raw entry of the evaluator's name reads.
/// `lexical` is the call an error about the name blames.
pub unsafe fn eval_symbol_reporting(
    symbol: Sexp,
    env: Sexp,
    lexical: impl FnOnce() -> Sexp,
    reader: ArgumentReader,
) -> R<Sexp> {
    // A value or a promise found by name is held by its binding, so a raw caller gets it
    // without the handle scope and the root the general path takes.
    let read = handles::reading(|context| {
        let symbol = Gc::<Symbol>::adopt(symbol);
        if symbol.is(handles::symbol(|symbols| symbols.dots))
            || symbol.is(handles::missing_argument())
            || dot_dot_position(symbol.name().bytes()).is_some()
        {
            return None;
        }
        match find_variable(context, symbol, Gc::adopt(env), true) {
            Found::Value(value) if !handles::is_missing_argument(value) => match settled(value) {
                Some(value) => Some((value.raw(), false)),
                None => Some((value.raw(), true)),
            },
            _ => None,
        }
    });
    match read {
        Some((promise, true)) => force_promise(promise),
        Some((value, false)) => Ok(value),
        None => cold(|| {
            handles::in_caller_scope(|context| {
                let (symbol, env) = (Local::adopt(symbol), Local::adopt(env));
                Ok(symbol_value_reporting(
                    context,
                    symbol,
                    env,
                    |context| handles::held(context, lexical()),
                    reader,
                )?
                .raw())
            })
        }),
    }
}

/// Runs `run` out of line, which keeps a rare path's code out of the common path's prologue.
#[cold]
#[inline(never)]
fn cold<T>(run: impl FnOnce() -> T) -> T {
    run()
}

/// The value of the name `symbol` in `environment`, as evaluating the name gives it: its
/// promise forced and an active binding called.
pub fn symbol_value<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'s, Symbol>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    let lexical = |context: &Cx<'s>| handles::lexical_call(context, environment);
    symbol_value_reporting(context, symbol, environment, lexical, ArgumentReader::Eval)
}

/// [`symbol_value`] for a lookup that starts in `environment` on behalf of a frame that is not
/// `environment` itself, such as a slot frame that has no environment or a frame whose own
/// bindings were already searched: an unbound symbol is reported against `lexical`, the call of
/// the frame the code runs in, as GNU R's `getLexicalCall` does. `reader` says whether the AST
/// evaluator or compiled code reads the name, which GNU R tells apart in the class of the error
/// for a missing argument.
pub fn symbol_value_reporting<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'s, Symbol>,
    environment: Local<'s, Environment>,
    lexical: impl FnOnce(&Cx<'s>) -> Local<'s>,
    reader: ArgumentReader,
) -> R<Local<'s>> {
    let name = symbol.read(context).name().bytes();
    if symbol.is(handles::symbol(|symbols| symbols.dots)) {
        return Err(error("'...' used in an incorrect context"));
    }
    if let Some(index) = dot_dot_position(name) {
        return dot_dot_value(context, index, environment, lexical, reader);
    }
    // The empty symbol is `R_MissingArg`, which GNU R's base frame binds to itself, so
    // evaluating it is a missing argument without a name where the lookup reaches base, and
    // an unbound object below an environment that does not (rlang's empty quosure).
    let value = if symbol.is(handles::missing_argument()) {
        if reaches_base(environment.read(context)) {
            return Err(missing_argument(lexical(context).raw(), "", reader));
        }
        None
    } else {
        let found = find_variable(
            context,
            symbol.read(context),
            environment.read(context),
            true,
        );
        match found {
            // A plain value or a forced promise needs no code run, so it is rooted once, as
            // the result.
            Found::Value(value) if !handles::is_missing_argument(value) => match settled(value) {
                Some(value) => return Ok(context.root(value)),
                None => Some(context.root(value)),
            },
            found => {
                let found = bound(context, found);
                resolve(context, found)?
            }
        }
    };
    let Some(value) = value else {
        let name = handles::text(symbol.read(context).name()).into_owned();
        return Err(handles::error_in(
            lexical(context),
            format!("object '{name}' not found"),
        ));
    };
    if handles::is_missing_argument(value) {
        let name = handles::text(symbol.read(context).name()).into_owned();
        return Err(missing_argument(lexical(context).raw(), &name, reader));
    }
    match value.downcast::<Promise>() {
        Some(promise) => handles::force(context, promise.value()),
        None => Ok(value),
    }
}

/// Whether a lookup from `environment` reaches the global environment or base before it reaches
/// the empty environment.
fn reaches_base(environment: Gc<'_, Environment>) -> bool {
    let known = [
        handles::global_environment(),
        handles::base_environment(),
        handles::base_namespace(),
    ];
    let mut scope = Some(environment);
    while let Some(environment) = scope {
        if known.iter().any(|&known| environment.is(known)) {
            return true;
        }
        scope = environment.enclosure();
    }
    false
}

/// `..N`'s position, for the symbols that name one.
pub unsafe fn dot_dot_index(symbol: Sexp) -> Option<usize> {
    dot_dot_position(symbol.car().bytes())
}

/// `..N`'s position, for the names that name one.
pub fn dot_dot_position(name: &[u8]) -> Option<usize> {
    let digits = name.strip_prefix(b"..")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// The value of `..index` in `environment`: the `index`th element of `...`, forced.
fn dot_dot_value<'s>(
    context: &mut Cx<'s>,
    index: usize,
    environment: Local<'s, Environment>,
    lexical: impl FnOnce(&Cx<'s>) -> Local<'s>,
    reader: ArgumentReader,
) -> R<Local<'s>> {
    if index == 0 {
        return Err(error("indexing '...' with non-positive index 0"));
    }
    let dots_symbol = handles::permanent_symbol(|symbols| symbols.dots);
    let Some(dots) = found_value(context, dots_symbol, environment, false)? else {
        return Err(error(format!(
            "..{index} used in an incorrect context, no ... to look in"
        )));
    };
    let mut cell = dots.read(context);
    let mut position = 1;
    while let Some((value, _, rest)) = handles::cell_fields(cell) {
        if position == index {
            if handles::is_missing_argument(value) {
                return Err(missing_argument(
                    lexical(context).raw(),
                    &format!("..{index}"),
                    reader,
                ));
            }
            let value = context.root(value);
            return handles::force(context, value);
        }
        position += 1;
        cell = rest;
    }
    Err(error(format!(
        "the ... list contains fewer than {index} element{}",
        if index == 1 { "" } else { "s" }
    )))
}

/// What a variable lookup finds, read without running any code.
#[derive(Clone, Copy)]
pub enum Found<'a> {
    /// Nothing from the environment outward binds the name.
    Unbound,
    /// What the binding holds: a value, a promise, or the empty argument.
    Value(Gc<'a>),
    /// An active binding, whose function gives the value.
    Active(Gc<'a, Binding>),
}

/// The binding cell for `symbol` from `env` outward.
pub unsafe fn find_cell(symbol: Sexp, env: Sexp) -> Option<Sexp> {
    handles::reading(|context| {
        visible_binding(context, Gc::adopt(symbol), Gc::adopt(env)).map(|binding| binding.raw())
    })
}

/// The binding `symbol` reads from `environment` outward.
pub fn visible_binding<'a>(
    context: &'a Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'_, Environment>,
) -> Option<Gc<'a, Binding>> {
    let (global, empty) = (handles::global_environment(), handles::empty_environment());
    let mut scope = Some(environment);
    while let Some(environment) = scope.filter(|scope| !scope.is(empty)) {
        if environment.is(global) {
            return handles::global_binding(context, symbol);
        }
        if let Some(binding) = handles::frame_binding(context, environment, symbol) {
            return Some(binding);
        }
        scope = environment.enclosure();
    }
    None
}

/// The binding `<<-` assigns `symbol` through, searching from `enclosure`, and the environment
/// whose frame holds it. Unlike [`visible_binding`], the walk reads the global environment's own
/// frame and goes on through the search path.
pub fn enclosing_binding<'a>(
    context: &'a Cx<'_>,
    symbol: Gc<'_, Symbol>,
    enclosure: Gc<'a, Environment>,
) -> Option<(Gc<'a, Environment>, Gc<'a, Binding>)> {
    let empty = handles::empty_environment();
    let mut scope = Some(enclosure);
    while let Some(environment) = scope.filter(|scope| !scope.is(empty)) {
        if let Some(binding) = handles::frame_binding(context, environment, symbol) {
            return Some((environment, binding));
        }
        scope = environment.enclosure();
    }
    None
}

/// What a binding gives a lookup.
fn binding_found(binding: Gc<'_, Binding>) -> Found<'_> {
    if binding.is_active() {
        return Found::Active(binding);
    }
    // C code can store `R_UnboundValue` in a binding (`Rf_defineVar`), which binds nothing.
    match binding.content() {
        content if handles::is_unbound(content) => Found::Unbound,
        content => Found::Value(content),
    }
}

/// The value of a binding cell, calling an active binding's function. A cell C code left
/// holding `R_UnboundValue` gives that, which is what a raw caller tests for.
#[inline]
pub unsafe fn cell_value(cell: Sexp) -> R<Sexp> {
    let binding = Gc::<Binding>::adopt(cell);
    if binding.is_active() {
        return cold(|| {
            handles::in_caller_scope(|context| {
                let found = bound(context, Found::Active(Gc::adopt(cell)));
                Ok(resolve(context, found)?.map_or_else(unbound, Local::raw))
            })
        });
    }
    Ok(binding.content().raw())
}

/// A [`Found`] rooted, so resolving it may allocate.
enum Bound<'s> {
    Unbound,
    Value(Local<'s>),
    /// An active binding's function.
    Active(Local<'s>),
}

fn bound<'s>(context: &Cx<'s>, found: Found<'_>) -> Bound<'s> {
    match found {
        Found::Unbound => Bound::Unbound,
        Found::Value(value) => Bound::Value(context.root(value)),
        Found::Active(binding) => Bound::Active(context.root(binding.content())),
    }
}

/// What `found` stands for as a value: a binding's content, or what an active binding's
/// function returns. `None` for an unbound name.
fn resolve<'s>(context: &mut Cx<'s>, found: Bound<'s>) -> R<Option<Local<'s>>> {
    match found {
        Bound::Unbound => Ok(None),
        Bound::Value(value) => Ok(Some(value)),
        Bound::Active(function) => {
            interp().active_binding_reads += 1;
            let value = handles::apply(
                context,
                handles::null(),
                function,
                &[],
                handles::global_environment(),
            )?;
            Ok(Some(value))
        }
    }
}

/// The call whose frame evaluates code in `env`, which GNU R names in an error about a missing
/// variable: so a promise forced inside a callee still blames the caller's code, and top-level
/// code blames no call. An environment no frame owns, like one `eval` was handed, falls back to
/// the innermost call.
pub(crate) unsafe fn lexical_call(env: Sexp) -> Sexp {
    let frames = &interp().frames;
    if let Some(frame) = frames.iter().rev().find(|frame| frame.owns(env)) {
        return frame.call.raw();
    }
    if env == values::global_env() {
        return nil();
    }
    crate::unwind::current_call()
}

/// `findVar`: the value bound to `symbol` from `env` outward, or `R_UnboundValue`. Promises are
/// returned unforced.
#[inline]
pub unsafe fn find_var(symbol: Sexp, env: Sexp) -> R<Sexp> {
    let (found, active) = handles::reading(|context| {
        match find_variable(context, Gc::adopt(symbol), Gc::adopt(env), false) {
            Found::Unbound => (unbound(), false),
            Found::Value(value) => (value.raw(), false),
            Found::Active(binding) => (binding.raw(), true),
        }
    });
    if active {
        return cell_value(found);
    }
    Ok(found)
}

/// The value bound to `symbol` from `environment` outward, `None` when nothing binds it.
/// A promise comes back unforced, though an active binding is called.
pub fn variable<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'_, Symbol>,
    environment: Local<'_, Environment>,
) -> R<Option<Local<'s>>> {
    found_value(context, symbol, environment, false)
}

/// [`variable`], where `forcing` says the caller forces a promise it gets at once and keeps
/// only the value ([`find_variable`]).
fn found_value<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'_, Symbol>,
    environment: Local<'_, Environment>,
    forcing: bool,
) -> R<Option<Local<'s>>> {
    let found = bound(
        context,
        find_variable(
            context,
            symbol.read(context),
            environment.read(context),
            forcing,
        ),
    );
    resolve(context, found)
}

/// `findVar`, reading a slot frame's variable from its slot where it sits ([`slot_binding`])
/// instead of moving it into a cell. `forcing` says the caller forces a promise it gets at once
/// and keeps only the value, which is what lets it see an argument reference.
pub fn find_variable<'a>(
    context: &'a Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'_, Environment>,
    forcing: bool,
) -> Found<'a> {
    let _activity = activity::enter(Activity::Lookup, 0);
    let (global, empty) = (handles::global_environment(), handles::empty_environment());
    let mut scope = environment;
    while !scope.is(empty) {
        if scope.is(global) {
            if let Some(binding) = handles::global_binding(context, symbol) {
                return binding_found(binding);
            }
            break;
        }
        if scope.has_flag(gp::SLOT_FRAME)
            && let Some(value) = handles::slot_variable(context, scope, symbol, forcing)
        {
            return Found::Value(value);
        }
        if let Some(binding) = handles::frame_binding(context, scope, symbol) {
            return binding_found(binding);
        }
        let Some(enclosure) = scope.enclosure() else {
            break;
        };
        scope = enclosure;
    }
    if symbol.is(handles::symbol(|symbols| symbols.tmp))
        && let Some(value) = handles::tmp_variable(context, scope)
    {
        return Found::Value(value);
    }
    Found::Unbound
}

/// What `...` holds in `env`, for a call that passes its arguments on or evaluates them. A
/// live slot frame's `...` is read in its slot, which moves nothing, so the frame keeps
/// running on its slots; the arguments it collected can be argument references. Any other
/// environment's `...` is read by name.
unsafe fn dots_binding(env: Sexp) -> R<Sexp> {
    let dots = symbols().dots;
    if env.has_gp(gp::SLOT_FRAME) && crate::heap().may_bind(env, dots) {
        let found = handles::reading(|context| {
            let frame = handles::pending_frame(context, Gc::adopt(env));
            if frame.role() == crate::FrameRole::Inlined {
                return None;
            }
            slot_content(frame, Gc::adopt(dots)).map(|value| value.raw())
        });
        if let Some(value) = found {
            return Ok(value);
        }
    }
    find_var(dots, env)
}

/// What a read by name of `symbol` finds in the live slot frame that `env` stands for, when the
/// variable sits in its slot as something a reader can take from there: a value or a promise.
/// The read moves nothing, so the frame keeps running on its slots. `None` sends the reader on
/// to the environment's cells: the frame keeps no slot of that name, the slot is unbound (the
/// variable may have moved into a cell, or the frame does not bind it yet), or it holds the
/// empty argument or `...`, which are read together with their binding. An argument reference
/// is reused once the call it was made for returns, so only a reader that forces it at once
/// and keeps the value alone (`forcing`) sees one.
pub unsafe fn slot_binding(env: Sexp, symbol: Sexp, forcing: bool) -> Option<Sexp> {
    if !crate::heap().may_bind(env, symbol) {
        return None;
    }
    handles::reading(|context| {
        let frame = handles::pending_frame(context, Gc::adopt(env));
        // A slot of a frame run without its body ([`run_inlined`]) holds the value where a
        // cell would hold the forced promise of the caller's variable ([`box_slots`]), which a
        // reader of the binding itself expects.
        if frame.role() == crate::FrameRole::Inlined && !forcing {
            return None;
        }
        let value = slot_content(frame, Gc::adopt(symbol))?;
        if handles::is_missing_argument(value)
            || value.r_type() == SexpType::Dots
            || (!forcing
                && value.has_flag(gp::ARGUMENT_REFERENCE)
                && value.r_type() == SexpType::Promise)
        {
            return None;
        }
        Some(value.raw())
    })
}

/// The `*tmp*` a complex assignment running in `env` or an environment it encloses stands for,
/// or `R_UnboundValue`.
pub(crate) unsafe fn tmp_value(env: Sexp) -> Sexp {
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
#[inline]
pub unsafe fn find_var_in_frame(symbol: Sexp, env: Sexp) -> R<Sexp> {
    let cell = handles::reading(|context| {
        handles::frame_binding(
            context,
            Gc::<Environment>::adopt(env),
            Gc::<Symbol>::adopt(symbol),
        )
        .map(|binding| binding.raw())
    });
    match cell {
        Some(cell) => cell_value(cell),
        None => Ok(unbound()),
    }
}

/// The value `symbol` has in the frame of `environment` itself, its promise forced, or `None`
/// when the frame does not bind it: `e[["name"]]`, `e$name` and `get(inherits = FALSE)`.
pub fn frame_value<'s>(
    context: &mut Cx<'s>,
    environment: Local<'_, Environment>,
    symbol: impl Pointer<Symbol>,
) -> R<Option<Local<'s>>> {
    handles::reaching_environment(context)?;
    let Some(binding) = handles::frame_binding(context, environment, symbol) else {
        return Ok(None);
    };
    let found = match binding_found(binding) {
        Found::Value(value) if value.downcast::<Promise>().is_none() => {
            return Ok(Some(context.root(value)));
        }
        found => bound(context, found),
    };
    let Some(value) = resolve(context, found)? else {
        return Ok(None);
    };
    // A promise roots itself while its code runs, and it holds its value afterwards.
    handles::force(context, value).map(Some)
}

/// `findFun`: the first function bound to `symbol` from `env` outward.
pub unsafe fn find_fun(symbol: Sexp, env: Sexp) -> R<Sexp> {
    find_called_fun(symbol, env, unbound())
}

/// `findFun3`: [`find_fun`] for the head of `call`, which a missing function's error names, as
/// in GNU R.
pub unsafe fn find_called_fun(symbol: Sexp, env: Sexp, call: Sexp) -> R<Sexp> {
    // A function found without running code is held by its binding, so a raw caller gets it
    // without the handle scope and the root the general path takes.
    let function = handles::reading(|context| {
        let _activity = activity::enter(Activity::Lookup, 0);
        match function_walk(context, Gc::adopt(symbol), Gc::adopt(env)) {
            Walk::Function(function) => Some(function.raw()),
            _ => None,
        }
    });
    match function {
        Some(function) => Ok(function),
        None => cold(|| {
            handles::in_caller_scope(|context| {
                let (symbol, env) = (Local::adopt(symbol), Local::adopt(env));
                let call = (call != unbound()).then(|| handles::held(context, call));
                Ok(function_value(context, symbol, env, call)?.raw())
            })
        }),
    }
}

/// The first function bound to `symbol` from `environment` outward, forcing the promises on
/// the way. `call` is the call whose head `symbol` is, which a missing function's error names.
pub fn function_value<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'s, Symbol>,
    environment: Local<'s, Environment>,
    call: Option<Local<'s>>,
) -> R<Local<'s>> {
    let call = call.map_or_else(unbound, Local::raw);
    let _activity = activity::enter(Activity::Lookup, 0);
    let mut scope = Some(environment);
    while let Some(start) = scope {
        let (at, found, in_slot) =
            match function_walk(context, symbol.read(context), start.read(context)) {
                Walk::Function(function) => return Ok(context.root(function)),
                Walk::Missing => {
                    let name = handles::text(symbol.read(context).name()).into_owned();
                    return Err(missing_argument(call, &name, ArgumentReader::Get));
                }
                Walk::NotFound => break,
                Walk::Run { at, found, in_slot } => {
                    (context.root(at), bound(context, found), in_slot)
                }
            };
        if let Some(value) = resolve(context, found)? {
            let value = handles::force(context, value)?;
            if values::is_function_type(value.r_type()) {
                return Ok(value);
            }
            if !in_slot && handles::is_missing_argument(value) {
                let name = handles::text(symbol.read(context).name()).into_owned();
                return Err(missing_argument(call, &name, ArgumentReader::Get));
            }
        }
        scope = at
            .read(context)
            .enclosure()
            .map(|enclosure| context.root(enclosure));
    }
    let name = handles::text(symbol.read(context).name()).into_owned();
    Err(error_in(
        call,
        format!(
            "could not find function \"{}\"",
            crate::format::encode_string(&name, None)
        ),
    ))
}

/// Where [`function_walk`] stops.
enum Walk<'a> {
    Function(Gc<'a>),
    /// The first binding that is not a function is the empty argument.
    Missing,
    NotFound,
    /// A binding whose value needs code to run, an unforced promise or an active binding, in
    /// the frame of `at`. `in_slot` says a slot frame keeps it in its slot.
    Run {
        at: Gc<'a, Environment>,
        found: Found<'a>,
        in_slot: bool,
    },
}

/// Walks from `environment` outward over the bindings of `symbol` whose values are already
/// there, so nothing allocates and the walk roots nothing, up to a function, the empty argument,
/// or a binding whose value needs code to run.
fn function_walk<'a>(
    context: &'a Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'a, Environment>,
) -> Walk<'a> {
    let (global, empty) = (handles::global_environment(), handles::empty_environment());
    let mut scope = environment;
    while !scope.is(empty) {
        let found = if scope.is(global) {
            // The first binding on the search path usually is the function; only when it is
            // not does the walk go on frame by frame, to skip it as GNU R does.
            if let Some(binding) = handles::global_binding(context, symbol)
                && !binding.is_active()
                && let Some(value) = settled(binding.content())
                && values::is_function_type(value.r_type())
            {
                return Walk::Function(value);
            }
            handles::frame_binding(context, scope, symbol).map(binding_found)
        } else if scope.has_flag(gp::SLOT_FRAME)
            && let Some(value) = handles::slot_variable(context, scope, symbol, true)
        {
            match settled(value) {
                Some(value) if values::is_function_type(value.r_type()) => {
                    return Walk::Function(value);
                }
                // The variable sits in its slot, so no cell binds the name.
                Some(_) => None,
                None => {
                    let found = Found::Value(value);
                    return Walk::Run {
                        at: scope,
                        found,
                        in_slot: true,
                    };
                }
            }
        } else {
            handles::frame_binding(context, scope, symbol).map(binding_found)
        };
        match found {
            Some(Found::Value(value)) => match settled(value) {
                Some(value) if values::is_function_type(value.r_type()) => {
                    return Walk::Function(value);
                }
                Some(value) if handles::is_missing_argument(value) => return Walk::Missing,
                Some(_) => {}
                None => {
                    let found = Found::Value(value);
                    return Walk::Run {
                        at: scope,
                        found,
                        in_slot: false,
                    };
                }
            },
            Some(found @ Found::Active(_)) => {
                return Walk::Run {
                    at: scope,
                    found,
                    in_slot: false,
                };
            }
            Some(Found::Unbound) | None => {}
        }
        let Some(enclosure) = scope.enclosure() else {
            break;
        };
        scope = enclosure;
    }
    Walk::NotFound
}

/// What forcing `value` gives without running code: `value` itself, or a forced promise's
/// value. `None` for a promise not yet forced.
fn settled(value: Gc<'_>) -> Option<Gc<'_>> {
    match value.downcast::<Promise>() {
        Some(promise) if handles::is_unbound(promise.result()) => None,
        Some(promise) => Some(promise.result()),
        None => Some(value),
    }
}

/// The function a call's head names: a symbol is looked up as a function from `environment`,
/// anything else (such as `pkg::f`) is evaluated there.
fn head_function<'s>(
    context: &mut Cx<'s>,
    head: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    match head.downcast::<Symbol>() {
        Some(symbol) => function_value(context, symbol, environment, None),
        None => handles::evaluate(context, head, environment),
    }
}

/// Binds `symbol` in `env`'s own frame. The caller holds `value` rooted.
pub unsafe fn define(symbol: Sexp, value: Sexp, env: Sexp) -> R<()> {
    handles::in_caller_scope(|context| {
        let (symbol, value, env) = (
            Local::adopt(symbol),
            handles::held(context, value),
            Local::adopt(env),
        );
        define_variable(context, symbol, value, env)
    })
}

/// Binds `symbol` to `value` in the frame of `environment` itself, as `assign` does: an
/// active binding's function is called with the value instead.
pub fn define_variable(
    context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    value: Local<'_>,
    environment: Local<'_, Environment>,
) -> R<()> {
    if let Some(binding) = handles::frame_binding(context, environment, symbol)
        && binding.is_active()
    {
        let binding = context.root(binding);
        return call_active_setter(context, binding, value);
    }
    handles::define_binding(context, environment, symbol, value)
}

/// Calls the function of the active `binding` with `value`, which is how assigning to it
/// works.
fn call_active_setter(
    context: &mut Cx<'_>,
    binding: Local<'_, Binding>,
    value: Local<'_>,
) -> R<()> {
    let function = context.root(binding.read(context).content());
    let value = context.root(value);
    let argument = Tagged {
        tag: handles::null(),
        value,
    };
    handles::apply(
        context,
        handles::null(),
        function,
        &[argument],
        handles::global_environment(),
    )?;
    Ok(())
}

/// `<<-`: assigns to the nearest existing binding above `env`, or in the global environment.
/// The caller holds `value` rooted.
pub unsafe fn set_var(symbol: Sexp, value: Sexp, env: Sexp) -> R<()> {
    // A plain binding outward is reachable from `env`, so a raw caller changes it without the
    // handle scope and roots of the general path.
    let binding = handles::reading(|context| {
        enclosing_binding(context, Gc::adopt(symbol), Gc::adopt(env))
            .map(|(_, binding)| binding)
            .filter(|binding| !binding.is_active())
            .map(Gc::raw)
    });
    if let Some(binding) = binding {
        crate::heap().set_binding_value(binding, value)?;
        binding.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
        return Ok(());
    }
    cold(|| {
        handles::in_caller_scope(|context| {
            let (symbol, value, env) = (
                Local::adopt(symbol),
                handles::held(context, value),
                Local::adopt(env),
            );
            assign_outward(context, symbol, value, env)
        })
    })
}

/// `symbol <<- value` from `environment`: the nearest binding outward changes, or the global
/// environment gains one.
pub fn assign_outward(
    context: &mut Cx<'_>,
    symbol: Local<'_, Symbol>,
    value: Local<'_>,
    environment: Local<'_, Environment>,
) -> R<()> {
    let Some((_, binding)) =
        enclosing_binding(context, symbol.read(context), environment.read(context))
    else {
        return define_variable(context, symbol, value, handles::global_environment());
    };
    let binding = context.root(binding);
    if binding.read(context).is_active() {
        return call_active_setter(context, binding, value);
    }
    handles::assign_binding(context, binding, value)?;
    handles::mark_assigned(context, binding);
    Ok(())
}

/// A promise to evaluate `code` in `env`.
pub unsafe fn make_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    activity::count(Counter::PromisesCreated);
    triple(SexpType::Promise, unbound(), code, env)
}

/// A promise to run the compiled `code` in `env`, the frame of the code it was compiled with.
/// Its expression is the code's source, as for any other promise; the code itself sits in the
/// attribute slot, which a promise otherwise never uses (see `force_promise`).
pub unsafe fn compiled_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    activity::count(Counter::PromisesCreated);
    let heap = crate::heap();
    let promise = heap.alloc_compiled_promise(unbound(), bytecode::source_of(code), env, code)?;
    heap.root(promise);
    Ok(promise)
}

/// A promise that already holds `value`, whose code is `code` for `substitute` to see.
pub unsafe fn forced_promise(code: Sexp, value: Sexp) -> R<Sexp> {
    triple(SexpType::Promise, value, code, nil())
}

/// The value of `promise`, which forcing it gives the first time it is asked for. The value of
/// a promise forced here stays rooted in the caller's handle scope, because the promise may
/// hold the only reference to it.
pub unsafe fn force_promise(promise: Sexp) -> R<Sexp> {
    if promise.car() != unbound() {
        return Ok(promise.car());
    }
    force_unforced_raw(promise)
}

/// [`force_unforced`] for raw callers, out of line so that where [`force_promise`] inlines,
/// its caller pays only for the test of a forced promise.
#[inline(never)]
unsafe fn force_unforced_raw(promise: Sexp) -> R<Sexp> {
    // The caller holds the promise, and `force_unforced` roots it before anything can collect.
    handles::in_caller_scope(|context| {
        force_unforced(context, Local::<Promise>::adopt(promise)).map(Local::raw)
    })
}

/// The value `promise` stands for, which forcing it gives the first time it is asked for.
pub fn promise_value<'s>(context: &mut Cx<'s>, promise: Local<'_, Promise>) -> R<Local<'s>> {
    let result = promise.read(context).result();
    if !handles::is_unbound(result) {
        return Ok(context.root(result));
    }
    force_unforced(context, promise)
}

/// The value a promise or plain value stands for ([`promise_value`]).
pub fn forced_value<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    match object.downcast::<Promise>() {
        Some(promise) => promise_value(context, promise),
        None => Ok(object),
    }
}

/// Forces the unforced `promise`. The code being forced can remove the last binding that holds
/// the promise (a loader placeholder replaces itself), and the promise's environment must
/// outlive its code, so the promise is rooted while it runs, as GNU R's pending-promise stack
/// keeps it.
fn force_unforced<'s>(context: &mut Cx<'s>, promise: Local<'_, Promise>) -> R<Local<'s>> {
    context.scope(|context| {
        let promise = context.root(promise);
        if promise.read(context).has_flag(PROMISE_SEEN) {
            return Err(error(
                "promise already under evaluation: recursive default argument reference or earlier problems?",
            ));
        }
        handles::forcing(context, promise)?;
        activity::count(Counter::PromisesForced);
        promise.read(context).set_flags(PROMISE_SEEN, true);
        if promise.read(context).has_flag(PROMISE_INTERRUPTED) {
            promise.read(context).set_flags(PROMISE_INTERRUPTED, false);
            // A handler that jumps out of this warning leaves the promise marked as under
            // evaluation, as in GNU R, where the next force reports the recursive reference.
            handles::warning(context, "restarting interrupted promise evaluation")?;
        }
        let result = promise_result(context, promise);
        promise.read(context).set_flags(PROMISE_SEEN, false);
        promise
            .read(context)
            .set_flags(PROMISE_INTERRUPTED, result.is_err());
        settle(context, promise, result?)
    })
}

/// What running the code of the unforced `promise` gives, before an annotation checks it.
fn promise_result<'s>(context: &mut Cx<'s>, promise: Local<'s, Promise>) -> R<Local<'s>> {
    let fields = promise.read(context);
    // A reference records the code it runs itself (`force_reference`): reading another frame's
    // variable runs none, and forcing the promise it reaches records the code that runs.
    if fields.has_flag(gp::ARGUMENT_REFERENCE) {
        return force_reference(context, promise);
    }
    let Some(environment) = fields.environment().downcast::<Environment>() else {
        // Releasing a promise's last counted reference clears its environment, so an unforced
        // promise without one was held by a root the counts do not see, and forcing it here
        // would evaluate its code nowhere instead of where it was made.
        if cfg!(debug_assertions)
            || syntax::testing::invariants_enabled()
            || heap::gc_stress_requested().is_some()
        {
            panic!("a promise was forced after its last counted reference was released");
        }
        return handles::run_promise_code(context, promise);
    };
    if environment.has_flag(gp::SLOT_FRAME) {
        let frame = handles::pending_frame(context, environment);
        let owner = frame.index();
        let _recorded = handles::evaluating(context, || frame.enclosure().value());
        return slot_promise_value(context, promise, owner);
    }
    let _recorded = handles::evaluating(context, || environment.value());
    handles::run_promise_code(context, promise)
}

/// The value of a promise over the environment of the live slot frame at `owner`. While every
/// variable is in its slot, code compiled against the frame's slot table runs on the slots and
/// a name reads its slot. Anything else evaluates in the environment, which takes the
/// variables the code reads: any code once the environment binds a variable, code compiled to
/// read by name (an argument of a call that `eval` ran in the environment), `..N`, and a name
/// whose slot holds an empty argument.
#[cold]
#[inline(never)]
fn slot_promise_value<'s>(
    context: &mut Cx<'s>,
    promise: Local<'s, Promise>,
    owner: usize,
) -> R<Local<'s>> {
    let fields = promise.read(context);
    if let Some(code) = fields.compiled().downcast::<Bytecode>() {
        // Code compiled without a slot table reads by name by itself once the frame's header
        // register holds the environment.
        let slot_table = handles::constants(code)
            .and_then(|pool| pool.get(bytecode::SLOTS))
            .is_some_and(|table| table.raw() != nil());
        if !slot_table {
            return handles::run_promise_code(context, promise);
        }
        let code = context.root(code);
        return handles::run_on_slots(context, code, owner);
    }
    let frame = handles::live_frame(context, owner);
    let expression = fields.code();
    if let Some(environment) = frame.environment() {
        let (expression, environment) = (context.root(expression), context.root(environment));
        return handles::evaluate(context, expression, environment);
    }
    if let Some(symbol) = expression.downcast::<Symbol>()
        && dot_dot_position(symbol.name().bytes()).is_none()
    {
        match slot_content(frame, symbol) {
            None => {
                let symbol = context.root(symbol);
                return eval_enclosed_name(context, symbol, owner);
            }
            Some(value) => {
                if let Some(promise) = value.downcast::<Promise>() {
                    let promise = context.root(promise);
                    return promise_value(context, promise);
                }
                if !handles::is_missing_argument(value) {
                    return Ok(context.root(value));
                }
            }
        }
    }
    let expression = context.root(expression);
    let environment = frame_environment(context, owner, Reify::Site(Site::PromiseOverFrame))?;
    handles::evaluate(context, expression, environment)
}

/// Where a name is bound for a live slot frame's pending environment ([`pending_name`]).
pub enum PendingName {
    /// The content of the slot that binds the name: a value, a promise, or the empty argument.
    Slot(Sexp),
    /// The frame's slots leave the name to this environment, the closure's enclosure.
    Enclosure(Sexp),
}

/// What the name `symbol` reads in the live slot frame whose environment `pending` binds
/// nothing yet ([`gp::PENDING_FRAME`]), found where forcing a promise of it there finds it
/// ([`slot_promise_value`]), but without forcing or evaluating anything.
pub unsafe fn pending_name(pending: Sexp, symbol: Sexp) -> PendingName {
    handles::reading(|context| {
        let frame = handles::pending_frame(context, Gc::adopt(pending));
        match slot_content(frame, Gc::adopt(symbol)) {
            Some(value) => PendingName::Slot(value.raw()),
            None => PendingName::Enclosure(frame.enclosure().raw()),
        }
    })
}

/// [`peeked_variable`] for raw callers.
pub unsafe fn peek_variable(symbol: Sexp, env: Sexp) -> Option<Sexp> {
    handles::reading(|context| {
        peeked_variable(context, Gc::adopt(symbol), Gc::adopt(env)).map(|value| value.raw())
    })
}

/// What the variable `symbol` is bound to from `environment`, read without forcing or calling
/// anything: a value or a promise, or `None` for an unbound name, the empty argument or an
/// active binding. A slot frame's environment that binds nothing yet has its names read from
/// the frame's slots.
pub fn peeked_variable<'c>(
    context: &'c Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'c, Environment>,
) -> Option<Gc<'c>> {
    let environment = if environment.has_flag(gp::PENDING_FRAME) {
        let frame = handles::pending_frame(context, environment);
        match slot_content(frame, symbol) {
            Some(value) => return (!handles::is_missing_argument(value)).then_some(value),
            None => frame.enclosure(),
        }
    } else {
        environment
    };
    peeked_binding(visible_binding(context, symbol, environment)?)
}

/// The value `environment`'s own frame binds `symbol` to, read without forcing or calling
/// anything, as [`peeked_variable`] reads it but without looking in an enclosing environment.
pub fn peeked_frame_variable<'c>(
    context: &'c Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'c, Environment>,
) -> Option<Gc<'c>> {
    if environment.has_flag(gp::PENDING_FRAME) {
        let value = slot_content(handles::pending_frame(context, environment), symbol)?;
        return (!handles::is_missing_argument(value)).then_some(value);
    }
    peeked_binding(handles::frame_binding(context, environment, symbol)?)
}

/// What `binding` holds when reading it calls nothing: a value or a promise, and not the empty
/// argument, an unbound marker or an active binding's function.
fn peeked_binding(binding: Gc<'_, Binding>) -> Option<Gc<'_>> {
    let value = binding.content();
    (!binding.is_active() && !handles::is_missing_argument(value) && !handles::is_unbound(value))
        .then_some(value)
}

/// Settles `promise`, unforced, with the `value` forcing it would give, which the caller found
/// without forcing it ([`pending_name`]).
pub unsafe fn settle_unforced(promise: Sexp, value: Sexp) -> R<Sexp> {
    // Two arguments can read through the same promise, which only the first settles.
    if promise.car() != unbound() {
        return Ok(promise.car());
    }
    // A checked promise's annotation check can fail, which leaves the promise unforced.
    settle_promise(promise, value)
}

/// What the live slot `frame` binds `symbol` to, read without moving a variable: from its slot
/// while the frame keeps every variable there, and from the cells its environment holds and the
/// slots together after. `None` when the frame leaves the name to the enclosure.
pub(crate) fn frame_variable<'c>(
    context: &'c Cx<'_>,
    frame: handles::LiveFrame<'c, '_>,
    symbol: Gc<'_, Symbol>,
) -> Option<Gc<'c>> {
    if let Some(environment) = frame.environment()
        && let Some(binding) = context.own_binding(environment, symbol)
    {
        let value = binding.content();
        return (!handles::is_unbound(value)).then_some(value);
    }
    slot_content(frame, symbol)
}

/// What the slot of the live slot `frame` named `symbol` holds, or `None` when the slots leave
/// the name to the enclosure: no slot has that name, or it is unbound.
#[inline]
fn slot_content<'c>(frame: handles::LiveFrame<'c, '_>, symbol: Gc<'_, Symbol>) -> Option<Gc<'c>> {
    frame
        .slot_named(symbol)
        .filter(|&value| !handles::is_unbound(value))
}

/// The variable `symbol` a replacement in the running slot frame changes, read from `enclosure`
/// because the frame's own slots do not bind it. GNU R evaluates the name in the enclosure, so
/// an unbound one is reported against the call that made the enclosure, if a call did.
#[cold]
#[inline(never)]
pub(crate) fn enclosed_value<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'_, Symbol>,
    enclosure: Local<'_, Environment>,
) -> R<Local<'s>> {
    interp().visible = true;
    let (symbol, enclosure) = (context.root(symbol), context.root(enclosure));
    match symbol_value(context, symbol, enclosure) {
        Err(Unwind::Error(failure)) => Err(handles::signal_error(context, failure)),
        result => result,
    }
}

/// Evaluates `symbol` from the enclosure of the slot frame at `owner`, whose own slots do not
/// bind it, so an unbound name is reported against the frame's call and not the enclosure's.
fn eval_enclosed_name<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'s, Symbol>,
    owner: usize,
) -> R<Local<'s>> {
    interp().visible = true;
    let enclosure = context.root(handles::live_frame(context, owner).enclosure());
    let lexical = move |context: &Cx<'s>| context.root(handles::live_frame(context, owner).call());
    match symbol_value_reporting(context, symbol, enclosure, lexical, ArgumentReader::Getvar) {
        Err(Unwind::Error(failure)) => Err(handles::signal_error(context, failure)),
        result => result,
    }
}

/// A promise of `expression`, compiled code or a name, over the slots of the slot frame at
/// `owner`. It names the frame's environment, which is made on the first such promise and
/// binds nothing until something reads a variable through it.
pub unsafe fn slot_promise(owner: usize, expression: Sexp) -> R<Sexp> {
    let env = handles::in_caller_scope(|context| Ok(pending_environment(context, owner)?.raw()))?;
    if expression.kind() == SexpType::Bytecode {
        compiled_promise(expression, env)
    } else {
        make_promise(expression, env)
    }
}

/// A closure of `formals` and `body` defined in the slot frame whose header register is
/// `header`, while the frame keeps every variable in its slot. The closure's environment is
/// the frame's, so defining it leaves the frame in its slots: a call of the closure or
/// `environment(f)$x` moves the variables it reads into the environment
/// ([`resolve_pending_access`]), and a frame that returns with the closure still reachable
/// moves the rest then ([`finish_frame`]). Closures past [`crate::FRAME_CLOSURES`] are not
/// tracked, which bounds the list that tracks them, and keep the environment as any other
/// holder of it does.
pub unsafe fn slot_closure(header: usize, formals: Sexp, body: Sexp, srcref: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let (formals, body, srcref) = (
            handles::held(context, formals),
            handles::held(context, body),
            handles::held(context, srcref),
        );
        Ok(closure_over_slots(context, header, formals, body, srcref)?.raw())
    })
}

/// [`slot_closure`] for code on the handle types.
pub fn closure_over_slots<'c>(
    context: &'c mut Cx<'_>,
    header: usize,
    formals: Local<'_>,
    body: Local<'_>,
    srcref: Local<'_>,
) -> R<Gc<'c>> {
    let frame = handles::frame_at_header(context, header);
    let (owner, in_slots) = (frame.index(), frame.in_slots());
    let state = interp();
    if crate::pending_closures_of(&state.pending_closures, owner).len() >= crate::FRAME_CLOSURES {
        if in_slots {
            reified_environment(context, owner, Reify::Site(Site::FunctionDefinition))?;
        }
        return handles::frame_closure(context, owner, in_slots, formals, body, srcref);
    }
    pending_environment(context, owner)?;
    let closure = handles::frame_closure(context, owner, true, formals, body, srcref)?;
    // The frame is the innermost one unless a promise of an outer frame defines the closure,
    // so the entry nearly always goes at the end.
    let state = interp();
    let end = crate::pending_closures_of(&state.pending_closures, owner).end;
    state
        .pending_closures
        .insert(end, (owner as u32, closure.raw()));
    Ok(closure)
}

/// The heap's resolver for a use of the bindings of a live slot frame's environment
/// (`heap::set_pending_resolver`). A lookup of one symbol moves that variable from its slot
/// into a binding cell, a new binding only switches the frame's own code to reading by name,
/// and a read of every binding moves all of them ([`lift_frame`]). The use may come from
/// anywhere, including native code between allocations it has not protected and safe code
/// that reads under a shared context, so the variables move with collection suspended: the
/// context made here then frees and moves nothing.
pub unsafe fn resolve_pending_access(env: Sexp, access: PendingAccess) {
    let heap = crate::heap();
    heap.inhibit_collection();
    let result = handles::with(|context| {
        let owner = handles::pending_frame(context, Gc::adopt(env)).index();
        match access {
            PendingAccess::Symbol(symbol) => {
                let symbol = context.root(Gc::<Symbol>::adopt(symbol));
                box_variable(context, owner, symbol)
            }
            PendingAccess::Insert => {
                leave_slots(context, owner);
                Ok(())
            }
            PendingAccess::All => lift(context, owner, None),
        }
    });
    crate::heap().allow_collection();
    if let Err(unwind) = result {
        panic!("a slot frame failed to move a variable into its environment: {unwind:?}");
    }
}

/// Serves a lookup of `symbol` in the environment of the slot frame at `owner`, which holds no
/// cell for it. A variable in a slot moves into a cell, and a dispatch variable of a method's
/// frame gets one. A name the frame keeps a slot for but has not bound yet moves nothing, but
/// the frame's own code reads and writes by name from here on all the same: a lookup that
/// passed the frame may be remembered by its caller, and only a binding made by name tells it
/// that the frame binds the name after all.
fn box_variable<'s>(context: &mut Cx<'s>, owner: usize, symbol: Local<'s, Symbol>) -> R<()> {
    let frame = handles::live_frame(context, owner);
    if let Some(slot) = frame.slot_names().iter().position(|name| name.is(symbol)) {
        box_slots(context, owner, slot..slot + 1)?;
        leave_slots(context, owner);
        return Ok(());
    }
    let recorded = frame
        .method_vars()
        .find(|(name, _)| name.is(symbol))
        .map(|(_, value)| context.root(value));
    if let Some(recorded) = recorded {
        let value = method_variable(context, owner, symbol, recorded)?;
        let pending = pending_of(context, owner);
        context.define_locals_uncollected(
            pending,
            &[LocalBinding {
                symbol: symbol.read(context),
                value: value.read(context),
                missing: false,
            }],
        )?;
        leave_slots(context, owner);
    }
    Ok(())
}

/// The value the dispatch variable `symbol` of the method frame at `index` binds, given the
/// `value` recorded for it. `UseMethod` called from a slot frame without an environment records
/// `.GenericCallEnv` as unbound, because its value is that caller's environment, which the
/// caller makes only when something asks. It is read here, when the variable gets a cell.
fn method_variable<'s>(
    context: &mut Cx<'s>,
    index: usize,
    symbol: Local<'s, Symbol>,
    value: Local<'s>,
) -> R<Local<'s>> {
    if handles::is_unbound(value)
        && symbol.is(handles::symbol(|symbols| symbols.dot_generic_call_env))
    {
        context.root_answer(|context| {
            caller_environment(context, index, Reify::Site(Site::MethodDispatch))
        })
    } else {
        Ok(value)
    }
}

/// The environment that stands for the slot frame at `owner`, which something has asked for.
fn pending_of<'c>(context: &'c Cx<'_>, owner: usize) -> Gc<'c, Environment> {
    match handles::live_frame(context, owner).pending() {
        Some(pending) => pending,
        None => panic!("frame {owner} moved a variable before it had an environment"),
    }
}

/// Moves the variables in `slots` of the slot frame at `owner` into binding cells of its
/// environment, each as [`bind_arguments`] and the assignments since would have bound it: a
/// formal with its missing flag, an argument reference as the promise it stands for
/// ([`detach_reference`]), since a cell can be read after the call that made the reference
/// returns. A slot that binds nothing is skipped. The slots give up their references and read
/// as unbound from here on. The caller has suspended collection.
#[inline(always)]
fn box_slots(context: &mut Cx<'_>, owner: usize, slots: std::ops::Range<usize>) -> R<()> {
    let frame = handles::live_frame(context, owner);
    // A variable the caller passed to a frame run without its body ([`run_inlined`]) sits in
    // its slot as the value; the generic path would have bound a promise of the variable,
    // forced by the body, and `substitute` reads the variable from it. Constants bind as
    // themselves either way.
    let sources = if frame.role() == crate::FrameRole::Inlined {
        let (function, call) = (context.root(frame.function()), context.root(frame.call()));
        call_plan(context, function.value(), call)?
    } else {
        None
    };
    for slot in slots.clone() {
        let frame = handles::live_frame(context, owner);
        let value = frame.slot(slot);
        if handles::is_unbound(value) {
            continue;
        }
        if let Some(reference) = value.downcast::<Promise>()
            && reference.has_flag(gp::ARGUMENT_REFERENCE)
        {
            let reference = context.root(reference);
            detach(context, reference)?;
        } else if value.r_type() == SexpType::Dots {
            let mut references: SmallVec<[Local<'_, Promise>; 4]> = SmallVec::new();
            let mut cell = value;
            while let Some((element, _, rest)) = handles::cell_fields(cell) {
                if let Some(reference) = element.downcast::<Promise>()
                    && reference.has_flag(gp::ARGUMENT_REFERENCE)
                {
                    references.push(context.root(reference));
                }
                cell = rest;
            }
            for reference in references {
                // A list that holds one reference twice detaches it at the first.
                if reference.read(context).has_flag(gp::ARGUMENT_REFERENCE) {
                    detach(context, reference)?;
                }
            }
        } else if let Some((shape, sources)) = &sources
            && slot < shape.formals as usize
            && let Some(&source) = sources.get(slot)
            && source >= 0
        {
            let mut argument = frame.call().downcast::<Language>().map(|call| call.rest());
            for _ in 0..source {
                argument = argument
                    .and_then(handles::cell_fields)
                    .map(|(_, _, rest)| rest);
            }
            let expression = argument
                .and_then(handles::cell_fields)
                .map(|(expression, _, _)| expression);
            if let Some(symbol) = expression.and_then(|expression| expression.downcast::<Symbol>())
            {
                let (symbol, value) = (context.root(symbol), context.root(value));
                let promise = handles::forced_promise(context, symbol.value(), value)?;
                handles::live_frame(context, owner).store_slot(slot, promise.value());
            }
        }
    }
    let moved = handles::live_frame(context, owner).move_slots_to_cells(slots)?;
    activity::count_by(Counter::VariablesBoxed, moved as u64);
    Ok(())
}

/// Switches the slot frame at `owner` from its slots to its environment, once the environment
/// binds a variable or is about to: the header register and [`Frame::env`] hold the
/// environment from here on, so the frame's own code reads and writes by name, and each
/// variable still in a slot moves into a cell the first time something reads it.
fn leave_slots(context: &Cx<'_>, owner: usize) {
    let frame = handles::live_frame(context, owner);
    if !frame.enter_environment() {
        return;
    }
    activity::count(Counter::FramesReified);
    if activity::enabled() {
        // Whatever asked for the environment last is the likeliest reader of it.
        if frame.reified_by() == crate::NOT_REIFIED {
            frame.set_reified_by(Site::EnvironmentRead as u32);
        }
        activity::count_reification(frame.reified_by());
    }
}

/// [`lift`] for raw callers, which have suspended collection.
pub(crate) unsafe fn lift_frame(index: usize, cause: Option<Site>) -> R<()> {
    handles::with(|context| lift(context, index, cause))
}

/// Makes the environment of the slot frame at `index` an ordinary one that outlives the frame
/// or is read as a whole: every variable still in a slot moves into a cell, a method's
/// dispatch variables are bound, and the bindings are listed as a call that bound them in an
/// environment from the start would list them, formals first. `cause` names what makes the
/// environment outlive a frame that is returning, for the profile summary. The caller has
/// suspended collection.
fn lift(context: &mut Cx<'_>, index: usize, cause: Option<Site>) -> R<()> {
    let frame = handles::live_frame(context, index);
    if activity::enabled()
        && frame.environment().is_none()
        && let Some(cause) = cause
    {
        frame.set_reified_by(cause as u32);
    }
    let count = frame.slot_names().len();
    box_slots(context, index, 0..count)?;
    leave_slots(context, index);
    // Only a method's frame has dispatch variables, so most frames skip rooting them.
    let mut method_vars: SmallVec<[(Local<'_, Symbol>, Local<'_>); 6]> = SmallVec::new();
    if handles::live_frame(context, index)
        .method_vars()
        .next()
        .is_some()
    {
        let pending = context.root(pending_of(context, index));
        method_vars.extend(
            handles::live_frame(context, index)
                .method_vars()
                .map(|(symbol, value)| (context.root(symbol), context.root(value))),
        );
        for &(symbol, value) in &method_vars {
            if context.own_binding(pending, symbol).is_none() {
                let value = method_variable(context, index, symbol, value)?;
                context.define_locals_uncollected(
                    pending,
                    &[LocalBinding {
                        symbol: symbol.read(context),
                        value: value.read(context),
                        missing: false,
                    }],
                )?;
            }
        }
    }
    let frame = handles::live_frame(context, index);
    let pending = pending_of(context, index);
    let names = frame.slot_names();
    let rank = |symbol: Gc<'_, Symbol>| {
        names
            .iter()
            .position(|name| name.is(symbol))
            .map(|slot| slot as u32)
    };
    context.release_slot_frame(pending);
    context.order_frame(pending, rank, frame.formals().count() as u32);
    let state = interp();
    if !state.pending_closures.is_empty() {
        let own = crate::pending_closures_of(&state.pending_closures, index);
        state.pending_closures.drain(own);
    }
    activity::count(Counter::FramesLifted);
    if syntax::testing::invariants_enabled() {
        check_lifted(context, index, pending, &method_vars);
    }
    Ok(())
}

/// The cross-check behind `syntax::testing::enable_invariants()`: once a slot frame's
/// environment has taken every variable, no slot binds one, the frame's own code reads the
/// environment, and each of a method's dispatch variables is bound.
fn check_lifted(
    context: &Cx<'_>,
    index: usize,
    environment: Gc<'_, Environment>,
    method_vars: &[(Local<'_, Symbol>, Local<'_>)],
) {
    let frame = handles::live_frame(context, index);
    for slot in 0..frame.slot_names().len() {
        assert!(
            handles::is_unbound(frame.slot(slot)),
            "frame {index}: slot {slot} still binds a variable its environment took"
        );
    }
    let header = interp().bytecode.registers[frame.header()];
    assert!(
        header == environment.raw() && frame.recorded_environment().is(environment),
        "frame {index}: a slot frame's environment took its variables but is not its environment"
    );
    for &(symbol, _) in method_vars {
        assert!(
            context.own_binding(environment, symbol).is_some(),
            "frame {index}: a method's environment took its variables without {:?}",
            handles::text(symbol.read(context).name())
        );
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
    handles::in_caller_scope(|context| {
        let symbol = handles::held(context, symbol);
        Ok(slot_reference(context, owner, slot, symbol)?.raw())
    })
}

/// [`argument_reference`] for code on the handle types.
#[inline]
pub fn slot_reference<'a>(
    context: &'a mut Cx<'_>,
    owner: usize,
    slot: u32,
    symbol: Local<'_>,
) -> R<Gc<'a, Promise>> {
    Ok(take_reference(context, symbol, Some((owner, slot)))?.1)
}

/// An argument reference to `expression`, a variable, uncompiled code or a constant, of a
/// caller that has the environment `env`, for a callee that takes its arguments straight into
/// slots: it forces as a promise of the expression in `env` would, and like a slot frame's
/// reference it allocates nothing and is released when the call returns. The reference keeps
/// `env` in its environment field, where a slot frame's reference keeps nil.
pub unsafe fn env_reference(env: Sexp, expression: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let (environment, expression) = (
            handles::held(context, env),
            handles::held(context, expression),
        );
        let (context, reference) = take_reference(context, expression, None)?;
        context.set_promise_environment(reference, environment);
        Ok(reference.raw())
    })
}

/// An argument reference to the compiled argument `code` of a caller that has the environment
/// `env`: [`env_reference`] of the code's source, which keeps the code as its attribute in
/// place of its frame target and runs it when forced, as a compiled promise would.
pub unsafe fn env_code_reference(env: Sexp, code: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let (environment, code) = (handles::held(context, env), handles::held(context, code));
        // The code holds its constant pool, and the caller holds the code.
        let source = handles::held(context, source_of(code.read(context)).raw());
        let (context, reference) = take_reference(context, source, None)?;
        context.set_promise_environment(reference, environment);
        context.set_promise_compiled(reference, code);
        Ok(reference.raw())
    })
}

/// An argument reference to the compiled argument `code` of the slot frame at `owner`, for a
/// callee that takes its arguments straight into slots: it forces as [`slot_promise`] of the
/// code would, by running the code on the frame's slots, without a promise or an environment.
/// It keeps the code in its environment field and the code's source as its expression, so
/// `substitute` sees the source.
pub unsafe fn code_reference(owner: usize, code: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let code = handles::held(context, code);
        Ok(compiled_reference(context, owner, code)?.1.raw())
    })
}

/// [`code_reference`] for code on the handle types.
fn compiled_reference<'c, 's>(
    context: &'c mut Cx<'s>,
    owner: usize,
    code: Local<'_>,
) -> R<(&'c Cx<'s>, Gc<'c, Promise>)> {
    ready_references(context, 1)?;
    let context = &*context;
    Ok((
        context,
        place_code_reference(context, owner, code.read(context)),
    ))
}

/// [`compiled_reference`] once the pool is ready: the reference names the code's source, and
/// keeps the code in its environment field.
#[inline(always)]
fn place_code_reference<'c>(context: &'c Cx<'_>, owner: usize, code: Gc<'_>) -> Gc<'c, Promise> {
    let reference = place_reference(context, source_of(code), Some((owner, NO_REFERENCE_SLOT)));
    context.set_promise_environment(reference, code);
    reference
}

/// The source a compiled argument's `code` was compiled from, which its constant pool keeps.
fn source_of<'c>(code: Gc<'c>) -> Gc<'c> {
    let source = code
        .downcast::<Bytecode>()
        .and_then(handles::constants)
        .and_then(|constants| constants.get(bytecode::SOURCE).copied());
    match source {
        Some(source) => source,
        None => panic!("compiled argument code without a constant pool"),
    }
}

/// The next free reference of [`Interp::references`], now naming `symbol`, with the context,
/// which nothing can collect through while the reference is used. `aim` gives the frame and
/// the slot a slot frame's reference reads ([`handles::aim_target`]); without it the target is
/// left for the caller to fill in.
#[inline(always)]
fn take_reference<'c, 's>(
    context: &'c mut Cx<'s>,
    symbol: Local<'_>,
    aim: Option<(usize, u32)>,
) -> R<(&'c Cx<'s>, Gc<'c, Promise>)> {
    ready_references(context, 1)?;
    let context = &*context;
    Ok((context, place_reference(context, symbol, aim)))
}

/// Makes the next `count` positions of the pool hold references that nothing else counts, so
/// that placing them ([`place_reference`]) allocates nothing.
#[inline(always)]
fn ready_references(context: &mut Cx<'_>, count: usize) -> R<()> {
    let used = interp().references_used as usize;
    for index in used..used + count {
        let free = handles::pooled_reference(context, index)
            .is_some_and(|(reference, _)| !reference.is_shared());
        if !free {
            renew_reference(context, index)?;
        }
    }
    Ok(())
}

/// The next reference of the pool, which [`ready_references`] made free, now naming `symbol`
/// and aimed as [`take_reference`] aims it.
#[inline(always)]
fn place_reference<'c>(
    context: &'c Cx<'_>,
    symbol: impl Pointer<Value>,
    aim: Option<(usize, u32)>,
) -> Gc<'c, Promise> {
    let index = interp().references_used as usize;
    let Some((reference, target)) = handles::pooled_reference(context, index) else {
        panic!("argument reference {index} is not in the pool");
    };
    context.set_promise_code(reference, symbol);
    // A call that failed while forcing the reference left it marked, and the mark belongs to
    // that call's argument, not to the one that takes the reference next.
    reference.set_flags(PROMISE_INTERRUPTED, false);
    if let Some((owner, slot)) = aim {
        handles::aim_target(context, target, owner, slot);
    }
    interp().references_used += 1;
    activity::count(Counter::ArgumentReferences);
    reference
}

/// Puts a new reference at position `index` of the pool, which ends the pool or holds a
/// reference something else counts. A `...` list counts the references it collected
/// until its frame is released (`Heap::release_frame`), which happens late for a frame that
/// waits for a reconcile and never for the frame of a call that failed. Handed out with that
/// count, the reference would not look held by its frame alone once a frame detached it
/// ([`detach_reference`]), so the pool gives it up and makes another.
#[cold]
#[inline(never)]
fn renew_reference(context: &mut Cx<'_>, index: usize) -> R<()> {
    let mut target = context.alloc::<Integer>(3)?;
    target.as_mut_slice().fill(0);
    let target = target.finish();
    handles::replace_pooled_reference(context, index, handles::null(), target)
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
    handles::reading(|context| return_references(context, mark))
}

/// [`release_references`] for code on the handle types.
pub fn return_references(context: &Cx<'_>, mark: u32) {
    let null = handles::null();
    for index in mark as usize..interp().references_used as usize {
        let Some((reference, target)) = handles::pooled_reference(context, index) else {
            break;
        };
        if !handles::is_unbound(reference.result()) {
            context.set_promise_value(reference, handles::unbound());
        }
        if !reference.code().is(null) {
            context.set_promise_code(reference, null);
        }
        // Only an environment or code reference changes the other fields, and forcing one
        // clears its environment again, so the attribute is compared with the target on its
        // own.
        if !reference.environment().is(null) {
            context.set_promise_environment(reference, null);
        }
        if !reference.compiled().is(target) {
            context.set_promise_compiled(reference, target.value());
        }
    }
    interp().references_used = mark;
}

/// Whether `value` is an argument reference ([`argument_reference`]).
#[inline]
pub unsafe fn is_reference(value: Sexp) -> bool {
    value.kind() == SexpType::Promise && value.has_gp(gp::ARGUMENT_REFERENCE)
}

/// The frame position and slot a slot frame's argument reference names. A reference is released
/// before its frame returns, so one that names another frame is a broken invariant.
fn reference_target(reference: Gc<'_, Promise>) -> (usize, u32) {
    let target = reference
        .compiled()
        .standard::<Integer>()
        .map(|target| target.as_slice());
    let Some(&[owner, slot, header]) = target else {
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
fn force_reference<'s>(context: &mut Cx<'s>, reference: Local<'s, Promise>) -> R<Local<'s>> {
    let fields = reference.read(context);
    // An environment or code reference runs its caller's code as the promise it stands for
    // would, so the boundary-call recorder sees that code as it sees a promise's. An
    // environment reference keeps its code and environment where a promise does.
    if let Some(environment) = fields.environment().downcast::<Environment>() {
        let _recorded = handles::evaluating(context, || environment.value());
        return handles::run_promise_code(context, reference);
    }
    let (owner, slot) = reference_target(fields);
    let frame = handles::live_frame(context, owner);
    if let Some(code) = fields.environment().downcast::<Bytecode>() {
        let _recorded = handles::evaluating(context, || frame.enclosure().value());
        let code = context.root(code);
        return handles::run_on_slots(context, code, owner);
    }
    let Some(symbol) = fields.code().downcast::<Symbol>() else {
        panic!("a slot frame's argument reference names no variable");
    };
    if let Some(environment) = frame.environment() {
        let (symbol, environment) = (context.root(symbol), context.root(environment));
        return handles::evaluate(context, symbol.value(), environment);
    }
    let value = if slot == NO_REFERENCE_SLOT {
        None
    } else {
        Some(frame.slot(slot as usize))
    };
    let value = match value {
        Some(value) if !handles::is_unbound(value) => value,
        _ => {
            let symbol = context.root(symbol);
            return eval_enclosed_name(context, symbol, owner);
        }
    };
    if let Some(promise) = value.downcast::<Promise>() {
        let promise = context.root(promise);
        return promise_value(context, promise);
    }
    if handles::is_missing_argument(value) {
        let symbol = context.root(symbol);
        let environment = frame_environment(context, owner, Reify::Site(Site::PromiseOverFrame))?;
        return handles::evaluate(context, symbol.value(), environment);
    }
    Ok(context.root(value))
}

/// [`settle_without_code`] for raw callers.
pub unsafe fn settled_without_code(promise: Sexp) -> R<Option<Sexp>> {
    Ok(handles::reading(|context| {
        settle_without_code(context, Gc::adopt(promise)).map(|value| value.raw())
    }))
}

/// The value an unforced `promise` stands for when reading it runs no R code, settling it as
/// forcing would: its code is a constant, or names a variable bound to a value, or to another
/// such promise, as when a wrapper passes its argument on (`g <- function(x) typeof(x)`). Every
/// promise on the chain is settled, innermost first, as forcing the outer one settles them.
/// `None`, with nothing settled, when the chain ends in code that would run or passes a promise
/// whose value an annotation checks, which forcing checks.
pub fn settle_without_code<'c>(context: &'c Cx<'_>, promise: Gc<'c, Promise>) -> Option<Gc<'c>> {
    // Longer chains are rare, and a chain that loops back (`function(x = y, y = x)`) must end
    // in the error forcing raises, so it is left to forcing.
    const CHAIN_LIMIT: usize = 16;
    let unsettled = PROMISE_SEEN | PROMISE_INTERRUPTED | gp::CHECKED_PROMISE;
    if promise.has_flag(unsettled) {
        return None;
    }
    let mut chain: SmallVec<[Gc<'c, Promise>; 4]> = smallvec![promise];
    let mut link = promise;
    let value = loop {
        let next = if link.has_flag(gp::ARGUMENT_REFERENCE) {
            reference_link(context, link)
        } else {
            promise_link(context, link)
        }?;
        let Some(next) = next.downcast::<Promise>() else {
            break next;
        };
        let result = next.result();
        if !handles::is_unbound(result) {
            break result;
        }
        if chain.len() == CHAIN_LIMIT || next.has_flag(unsettled) {
            return None;
        }
        chain.push(next);
        link = next;
    };
    // Every link was unforced when the walk passed it, and the walk runs no code.
    for &link in chain.iter().rev() {
        context.settle_promise(link, value);
    }
    Some(value)
}

/// What reading an unforced plain promise yields without running code: its constant code, or
/// the binding its variable code names (a value or another promise). A promise over a slot
/// frame's pending environment reads its variable from the frame's slots ([`peeked_variable`]).
fn promise_link<'c>(context: &'c Cx<'_>, promise: Gc<'c, Promise>) -> Option<Gc<'c>> {
    let code = promise.code();
    match code.r_type() {
        SexpType::Symbol => variable_link(context, code, promise.environment()),
        SexpType::Language | SexpType::Promise | SexpType::Bytecode => None,
        _ => Some(code),
    }
}

/// What reading an unforced argument reference yields without running code: what its
/// variable holds, a value or a promise.
fn reference_link<'c>(context: &'c Cx<'_>, reference: Gc<'c, Promise>) -> Option<Gc<'c>> {
    let Some(environment) = reference.environment().downcast::<Environment>() else {
        return referenced_variable(context, reference);
    };
    if reference.compiled().r_type() == SexpType::Bytecode {
        return None;
    }
    let expression = reference.code();
    match expression.r_type() {
        // The environment can be a slot frame's pending one, as a call evaluated in
        // `parent.frame()` makes it.
        SexpType::Symbol => variable_link(context, expression, environment.value()),
        SexpType::Language => None,
        _ => Some(expression),
    }
}

/// What the variable named by the symbol `code` reads from `environment` without running code
/// ([`peeked_variable`]), or `None` for the empty argument and `...`, which are not variables.
fn variable_link<'c>(context: &'c Cx<'_>, code: Gc<'c>, environment: Gc<'c>) -> Option<Gc<'c>> {
    let symbol = code.downcast::<Symbol>()?;
    if symbol.is(handles::missing_argument()) || symbol.is(handles::symbol(|symbols| symbols.dots))
    {
        return None;
    }
    peeked_variable(context, symbol, environment.downcast::<Environment>()?)
}

/// [`referenced_variable`] for raw callers.
pub unsafe fn reference_variable(reference: Sexp) -> Option<Sexp> {
    handles::reading(|context| {
        referenced_variable(context, Gc::adopt(reference)).map(|value| value.raw())
    })
}

/// What the variable a slot frame's argument reference names holds, read where forcing the
/// reference would read it but without forcing or calling anything: a value or a promise, or
/// `None` for a code reference, an environment reference, or an unbound, missing or active
/// variable. A variable the frame keeps in no slot, or whose slot is unbound, is read in the
/// frame's enclosure ([`peeked_variable`]), so a global passed from a slot frame needs no
/// environment for that frame.
pub fn referenced_variable<'c>(context: &'c Cx<'_>, reference: Gc<'c, Promise>) -> Option<Gc<'c>> {
    if reference.environment().raw() != nil() {
        return None;
    }
    let symbol = reference.code().downcast::<Symbol>()?;
    let (owner, slot) = reference_target(reference);
    let frame = handles::live_frame(context, owner);
    if let Some(environment) = frame.environment() {
        return peeked_variable(context, symbol, environment);
    }
    let value = (slot != NO_REFERENCE_SLOT)
        .then(|| frame.slot(slot as usize))
        .filter(|&value| !handles::is_unbound(value));
    match value {
        None => peeked_variable(context, symbol, frame.enclosure()),
        Some(value) => (!handles::is_missing_argument(value)).then_some(value),
    }
}

/// Whether the variable an unforced argument reference names is a missing argument in its
/// frame, as [`content_is_missing`] asks of a promise of a name.
fn reference_is_missing(context: &Cx<'_>, reference: Gc<'_, Promise>) -> R<bool> {
    let place = reference.environment();
    let Some(symbol) = reference.code().downcast::<Symbol>() else {
        return Ok(false);
    };
    if place.r_type() == SexpType::Bytecode {
        return Ok(false);
    }
    let in_environment = |environment| match handles::frame_binding(context, environment, symbol) {
        Some(_) => symbol_is_missing(context, symbol, environment, false),
        None => Ok(false),
    };
    if let Some(environment) = place.downcast::<Environment>() {
        return in_environment(environment);
    }
    let (owner, slot) = reference_target(reference);
    let frame = handles::live_frame(context, owner);
    if let Some(environment) = frame.environment() {
        return in_environment(environment);
    }
    if slot == NO_REFERENCE_SLOT {
        return Ok(false);
    }
    let value = frame.slot(slot as usize);
    if handles::is_unbound(value) {
        return Ok(false);
    }
    content_is_missing(context, value)
}

/// Turns the argument references the frame at `index` was called with into ordinary promises
/// over the frames they name ([`detach_reference`]). It converts each in place, so the frame's
/// slots and the `...` it collected, which hold the same promises, change with its arguments.
/// A reference is reused once the call
/// returns, so the paths that hand a frame's arguments on as promises run this first: S4
/// dispatch, `NextMethod`, and `UseMethod` when the method does not take over the generic's
/// slots ([`crate::dispatch::use_method_in_slots`]).
pub unsafe fn materialize_references(index: usize) -> R<()> {
    handles::with(|context| detach_frame_references(context, index))
}

/// [`materialize_references`] for code on the handle types.
pub fn detach_frame_references(context: &mut Cx<'_>, index: usize) -> R<()> {
    let Some(frame) = handles::frame_if_live(context, index) else {
        return Ok(());
    };
    if !frame.has_slots() {
        return Ok(());
    }
    // Detaching changes each promise in place and leaves the argument stack as it is, so each
    // argument is read afresh and only the one being detached is rooted. Forwarding `...`
    // passes one reference as several arguments, and the first detaches it.
    let mut position = 0;
    while let Some(argument) = handles::live_frame(context, index).argument(position) {
        position += 1;
        if let Some(reference) = argument.downcast::<Promise>()
            && reference.has_flag(gp::ARGUMENT_REFERENCE)
        {
            let reference = context.root(reference);
            detach(context, reference)?;
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
    handles::in_caller_scope(|context| {
        for arg in args {
            if is_reference(arg.value) {
                let reference = context.root(Gc::<Promise>::adopt(arg.value));
                detach(context, reference)?;
            }
        }
        Ok(())
    })
}

/// Makes `reference` the ordinary promise that stands for it ([`reference_promise`]), in place,
/// and gives its place in the pool to a fresh reference. Forwarding `...` hands the same
/// reference to several frames, so converting it in place is what keeps it one promise that
/// forces once for all of them.
pub unsafe fn detach_reference(reference: Sexp) -> R<()> {
    handles::with(|context| {
        let reference = context.root(Gc::<Promise>::adopt(reference));
        detach(context, reference)
    })
}

/// [`detach_reference`] for code on the handle types.
pub fn detach<'s>(context: &mut Cx<'s>, reference: Local<'s, Promise>) -> R<()> {
    let state = interp();
    let Some(index) = state.references[..state.references_used as usize]
        .iter()
        .position(|pooled| pooled.promise == reference.raw())
    else {
        panic!("an argument reference outlived the call it was made for");
    };
    // Everything that allocates comes first, so a failed allocation leaves the reference as it
    // was: the environment of the frame an unforced reference names, and the replacement. The
    // promise names its frame's environment from here on, so the target it gave up serves the
    // reference that takes its place.
    let fields = reference.read(context);
    if handles::is_unbound(fields.result())
        && fields.environment().r_type() != SexpType::Environment
    {
        let (owner, _) = reference_target(fields);
        pending_environment(context, owner)?;
    }
    // The pool's count goes with the pool's place: a promise that kept it would never count as
    // held by its frame alone, so the frame's release would leave it forced, and the value it
    // holds would look shared for the rest of its life.
    handles::refill_detached_place(context, index)?;
    let context: &Cx<'s> = context;
    let [value, code, environment, compiled] = reference_promise(context, reference.read(context));
    context.set_promise_value(reference, value);
    context.set_promise_code(reference, code);
    context.set_promise_environment(reference, environment);
    context.set_promise_compiled(reference, compiled);
    reference
        .read(context)
        .set_flags(gp::ARGUMENT_REFERENCE, false);
    Ok(())
}

/// The fields (value, code, environment, compiled code) of the ordinary promise that stands for
/// an argument reference: a forced promise of its name once it is forced, and a promise of its
/// name in the frame it names before, whose environment [`detach`] has made. [`detach`] writes
/// them into the reference itself, so no promise is allocated only to be copied.
fn reference_promise<'c>(context: &'c Cx<'_>, reference: Gc<'c, Promise>) -> [Gc<'c>; 4] {
    let symbol = reference.code();
    let null = handles::null().read(context);
    if !handles::is_unbound(reference.result()) {
        return [reference.result(), symbol, null, null];
    }
    let (environment, expression) =
        if let Some(environment) = reference.environment().downcast::<Environment>() {
            let code = reference.compiled();
            (
                environment.value(),
                if code.r_type() == SexpType::Bytecode {
                    code
                } else {
                    symbol
                },
            )
        } else {
            let (owner, _) = reference_target(reference);
            let Some(environment) = handles::live_frame(context, owner).pending() else {
                panic!("frame {owner} lost the environment of an argument reference");
            };
            let expression = if reference.environment().r_type() == SexpType::Bytecode {
                reference.environment()
            } else {
                symbol
            };
            (environment.value(), expression)
        };
    activity::count(Counter::PromisesCreated);
    if expression.r_type() == SexpType::Bytecode {
        [
            handles::unbound().read(context),
            source_of(expression),
            environment,
            expression,
        ]
    } else {
        [handles::unbound().read(context), symbol, environment, null]
    }
}

/// The environment that stands for the slot frame at `owner`, made on first use: an environment
/// enclosed by the closure's that binds nothing, because the frame's variables stay in their
/// slots. Handing it out, comparing it and naming it in a promise or a closure cost nothing
/// more. A lookup through it moves the one variable it asks for into a cell
/// ([`resolve_pending_access`]), and the frame's own code reads by name from then on.
pub unsafe fn pending_env(owner: usize) -> R<Sexp> {
    handles::in_caller_scope(|context| Ok(pending_environment(context, owner)?.raw()))
}

/// [`pending_env`] for code on the handle types. The frame holds the environment.
pub fn pending_environment<'a>(context: &'a mut Cx<'_>, owner: usize) -> R<Gc<'a, Environment>> {
    context.read_or_make(
        |context| handles::live_frame(context, owner).pending(),
        |context| make_pending_environment(context, owner),
    )
}

/// [`pending_environment`] for a slot frame that has not made it yet.
#[inline]
fn make_pending_environment<'a>(context: &'a mut Cx<'_>, owner: usize) -> R<Gc<'a, Environment>> {
    // The frame holds its function while the environment is reserved.
    context.reserve(ENVIRONMENT_BYTES);
    let context: &Cx<'_> = context;
    let environment = handles::live_frame(context, owner).make_pending()?;
    activity::count(Counter::FrameEnvironments);
    Ok(environment)
}

/// The environment of an unforced `promise`: for an argument reference, that of the frame it
/// names. Anything that reads a promise's environment other than to force it reads it here.
pub unsafe fn promise_env(promise: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let promise = handles::held(context, promise);
        match promise.downcast::<Promise>() {
            Some(promise) => Ok(promise_environment(context, promise)?.raw()),
            None => Ok(nil()),
        }
    })
}

/// [`promise_env`] for code on the handle types.
pub fn promise_environment<'a, 's>(
    context: &'a mut Cx<'s>,
    promise: Local<'s, Promise>,
) -> R<Gc<'a>> {
    let owner = Cell::new(None);
    context.read_or_make(
        |context| {
            let fields = promise.read(context);
            if fields.has_flag(gp::ARGUMENT_REFERENCE)
                && handles::is_unbound(fields.result())
                && fields.environment().r_type() != SexpType::Environment
            {
                owner.set(Some(reference_target(fields).0));
                return None;
            }
            Some(fields.environment())
        },
        |context| match owner.get() {
            Some(owner) => {
                frame_environment_of(context, owner, Reify::Site(Site::PromiseEnvironment))
            }
            None => panic!("a promise's environment was neither read nor made"),
        },
    )
}

/// Stores `value` as the value of the unforced `promise`, as forcing it to that value does.
pub unsafe fn settle_promise(promise: Sexp, value: Sexp) -> R<Sexp> {
    if !promise.has_gp(gp::CHECKED_PROMISE) {
        crate::heap().settle_promise(promise, value);
        return Ok(value);
    }
    handles::in_caller_scope(|context| {
        let promise = context.root(Gc::<Promise>::adopt(promise));
        let value = handles::adopt(context, value);
        settle(context, promise, value).map(Local::raw)
    })
}

/// Stores `value` as the value of the unforced `promise`, after checking it against the
/// annotation a checked promise carries, whose conversion is what the promise stores.
fn settle<'s>(context: &mut Cx<'s>, promise: Local<'s, Promise>, value: Local<'s>) -> R<Local<'s>> {
    let value = if promise.read(context).has_flag(gp::CHECKED_PROMISE) {
        handles::check_forced(context, promise, value)?
    } else {
        value
    };
    context.settle_promise(promise, value);
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
            apply_closure(call, function, &args, env, None)
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
/// full call. While a profile or activity trace is being taken, the entry publishes the builtin
/// and takes the profile sample as the full call does, so an observed run takes the same path
/// as an unobserved one: a special replacing a variable in a slot has only this entry to keep
/// the frame without an environment.
#[inline]
pub unsafe fn call_direct(definition: &BuiltinDef, call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let Some(direct) = definition.direct else {
        return Ok(None);
    };
    if activity::hooks() != 0 {
        return call_direct_observed(definition, direct, call, args);
    }
    run_direct(definition, direct, call, args)
}

#[inline(never)]
unsafe fn call_direct_observed(
    definition: &BuiltinDef,
    direct: DirectFn,
    call: Sexp,
    args: &[Sexp],
) -> R<Option<Sexp>> {
    if activity::profile_ticked() {
        record_profile_sample(Some(definition.name))?;
    }
    if !activity::enabled() {
        return run_direct(definition, direct, call, args);
    }
    let index = builtin_index(definition);
    let running = activity::publish_builtin(index);
    let value = run_direct(definition, direct, call, args);
    activity::restore(running);
    // A declined entry leaves the count to the full call that follows.
    if matches!(value, Ok(Some(_))) {
        activity::record_builtin(index);
    }
    value
}

#[inline(always)]
unsafe fn run_direct(
    definition: &BuiltinDef,
    direct: DirectFn,
    call: Sexp,
    args: &[Sexp],
) -> R<Option<Sexp>> {
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
#[inline]
pub(crate) unsafe fn call_builtin_reusing(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
) -> R<Sexp> {
    call_builtin_blaming(definition, call, op, args, env, reusable, unbound())
}

/// [`call_builtin_reusing`], blaming `blame` for an error that names no call.
//
// The common call (no hook to run, the arity right, no names to check, no replacement target to
// copy) runs straight through; anything else takes an out-of-line path, so this frame stays
// small enough that entry and exit cost a handful of instructions. A call that only a hook
// observes has its own path, which carries none of the checks' code.
#[inline(always)]
unsafe fn call_builtin_blaming(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
    blame: Sexp,
) -> R<Sexp> {
    let checks = definition.checks;
    if (checks.arity && args.len() != definition.arity as usize)
        || (checks.names && args.iter().any(|arg| arg.tag != nil()))
        || (checks.replacement != Replacement::None
            && args
                .first()
                .is_some_and(|target| target.value.refcount() >= 1))
    {
        return call_builtin_checked(definition, call, op, args, env, reusable, blame);
    }
    if activity::hooks() != 0 {
        return call_builtin_observed(definition, call, op, args, env, reusable, blame);
    }
    run_builtin(definition, call, op, args, env, reusable, blame)
}

/// [`call_builtin`] with arguments that are all untagged and none empty, as at a call site tier 0
/// quickened: the names check cannot fail, so only a wrong arity or a shared replacement target
/// takes the checked path, and a hook the observed one.
#[inline(always)]
pub(crate) unsafe fn call_builtin_positional(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Sexp> {
    let checks = definition.checks;
    if (checks.arity && args.len() != definition.arity as usize)
        || (checks.replacement != Replacement::None
            && args
                .first()
                .is_some_and(|target| target.value.refcount() >= 1))
    {
        return call_builtin_checked(definition, call, op, args, env, None, unbound());
    }
    if activity::hooks() != 0 {
        return call_builtin_observed(definition, call, op, args, env, None, unbound());
    }
    run_builtin(definition, call, op, args, env, None, unbound())
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
    let result = call_builtin_blaming(definition, call, op, args, env, None, call);
    interp().assignment_call = outer;
    result
}

/// The builtin call under a hook flag whose arity, names and replacement target pass: it takes
/// an `Rprof` sample that is due, and while a profile summary is collected it counts the call
/// and publishes the builtin for as long as it runs.
#[inline(never)]
unsafe fn call_builtin_observed(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
    reusable: Option<Sexp>,
    blame: Sexp,
) -> R<Sexp> {
    if activity::profile_ticked() {
        record_profile_sample(Some(definition.name))?;
    }
    let summarized = activity::enabled();
    let running = if summarized {
        let index = builtin_index(definition);
        activity::record_builtin(index);
        activity::publish_builtin(index)
    } else {
        0
    };
    let result = run_builtin(definition, call, op, args, env, reusable, blame);
    if summarized {
        activity::restore(running);
    }
    result
}

/// The builtin call whose arity, names or replacement target needs a check: it raises the
/// arity and name errors and copies a shared replacement target, and under a hook flag it
/// records what [`call_builtin_observed`] does.
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
        activity::record_builtin(index);
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
    if checks.names {
        // A loop, where `Iterator::any` shares one out-of-line copy with other callers.
        for arg in args {
            if arg.tag != nil() {
                check_argument_names(definition, call, args)?;
                break;
            }
        }
    }
    if activity::profile_ticked() {
        record_profile_sample(Some(definition.name))?;
    }
    // A replacement primitive modifies its target in place when the target is referenced
    // only by the variable a complex assignment rebinds. Called directly, as in
    // `` `class<-`(x, "foo") ``, the same single reference is the caller's own binding, which
    // must keep its value, so the target is copied first, as GNU R does.
    let copied_target;
    let args = match args.first() {
        Some(target)
            if checks.replacement != Replacement::None
                && target.value.refcount() >= 1
                && !is_assignment(call) =>
        {
            let mut copy = args.to_vec();
            copy[0].value = if checks.replacement == Replacement::Attributes {
                values::replacement_copy(target.value)?
            } else {
                values::shallow_duplicate(target.value)?
            };
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
    if checks.native != NativeArgs::Accepted {
        // A loop, where `Iterator::any` shares one out-of-line copy with other callers.
        for arg in args {
            if matches!(arg.value.kind(), SexpType::Record | SexpType::I64) {
                let skip_numbers = match checks.native {
                    NativeArgs::Accepted | NativeArgs::RefusedExceptNumbers => true,
                    NativeArgs::Refused => false,
                    NativeArgs::RefusedExceptArrayIndexes => {
                        args.first().is_some_and(|target| target.value.is_array())
                    }
                };
                crate::records::check_native_args(call, args, skip_numbers)?;
                break;
            }
        }
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
pub(crate) unsafe fn builtin_failure(blame: Sexp, unwind: Unwind) -> Unwind {
    match unwind {
        Unwind::Error(mut failure) => {
            if failure.call == unbound() {
                failure.call = blame;
            }
            if interp().signalling != Signalling::Immediate {
                return Unwind::Error(failure);
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
    // A loop, where `Iterator::any` shares one out-of-line copy with other callers.
    for formal in formals {
        if formal == "..." {
            return Ok(());
        }
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
                "unused argument {}",
                crate::deparse::unused_arguments(&[(Some(&name), arg.value)])
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
            let value = dots_binding(env)?;
            if value.kind() == SexpType::Dots {
                let mut cell = value;
                while cell.kind() == SexpType::Dots || cell.kind() == SexpType::Pairlist {
                    let item = cell.car();
                    let evaluated = if item == missing {
                        if !keep_missing {
                            // GNU R evaluates the empty item as a symbol, in the frame that
                            // passes `...` on, so the error is that frame's, not the call's.
                            return Err(missing_argument(
                                lexical_call(env),
                                "",
                                ArgumentReader::Eval,
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
            } else if value != missing && value != nil() {
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
/// [`PROMISES`] for a callee that takes argument references ([`call_seeded`]): one that a slot
/// frame's `...` holds passes on as it is. The modes below [`REFERENCES`] turn it into the
/// promise it stands for, since their callees may keep an argument past the call.
const PROMISES_PASSING_REFERENCES: u8 = 5;

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
            let value = dots_binding(env)?;
            if value.kind() == SexpType::Dots {
                let mut cell = value;
                while cell.kind() == SexpType::Dots || cell.kind() == SexpType::Pairlist {
                    let item = cell.car();
                    // Converted in place, the reference stays one promise for every call the
                    // `...` is passed to.
                    if MODE < REFERENCES && is_reference(item) {
                        detach_reference(item)?;
                    }
                    args.push(Arg {
                        tag: cell.tag(),
                        value: item,
                    });
                    cell = cell.cdr();
                }
            } else if value != missing_arg() && value != nil() {
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
                || (MODE != PROMISES
                    && MODE != EVALUATOR_REFERENCES
                    && MODE != PROMISES_PASSING_REFERENCES
                    && constant)
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

/// Applies a closure: matches the arguments to the formals, binds them in a slot frame or a
/// fresh environment, runs the body, and handles `return` and `on.exit`. `supplied` are extra
/// bindings for a method frame (`.Generic` and friends). The raw entry of [`apply_closure_in`].
pub unsafe fn apply_closure(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Sexp> {
    match prepare_raw_call(call, function, args, sys_parent, supplied)? {
        Prepared::Done(value) => Ok(value),
        Prepared::Slots(frame) => run_frame(
            call,
            function,
            CallFrame::Slots(frame),
            sys_parent,
            args,
            Entry::Unchecked,
            supplied.unwrap_or(&[]),
        ),
        Prepared::Heap => {
            run_on_heap_frame(call, function, args, sys_parent, supplied, Entry::Unchecked)
        }
    }
}

/// [`prepare_closure_call`] for a raw closure call.
///
/// # Safety
///
/// The caller holds the call, the closure, its caller's environment, the arguments and the
/// method variables until the call returns, and releases the handle stack after it.
#[inline(always)]
unsafe fn prepare_raw_call(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Prepared<Sexp>> {
    handles::in_caller_scope(|context| {
        let supplied = supplied.map(|supplied| handles::method_variables(context, supplied));
        let closure_call = closure_call(
            handles::held(context, call),
            handles::held(context, function),
            handles::tagged(context, args),
            handles::held(context, sys_parent),
            supplied,
        )?;
        Ok(match prepare_closure_call(context, &closure_call)? {
            Prepared::Done(value) => Prepared::Done(value.raw()),
            Prepared::Slots(frame) => Prepared::Slots(frame),
            Prepared::Heap => Prepared::Heap,
        })
    })
}

/// The call `call` of `function`, which must be a closure.
#[inline(always)]
fn closure_call<'a, 's>(
    call: Local<'s>,
    function: Local<'s>,
    args: &'a [Tagged<'s>],
    sys_parent: Local<'s>,
    supplied: Option<handles::MethodVariables<'a, 's>>,
) -> R<ClosureCall<'a, 's>> {
    let Some(function) = function.downcast::<Closure>() else {
        return Err(error(
            "internal error: a closure call's function is not a closure",
        ));
    };
    Ok(ClosureCall {
        call,
        function,
        args,
        sys_parent,
        supplied,
    })
}

/// A call of a closure, as [`apply_closure_in`] runs it.
#[derive(Clone, Copy)]
pub struct ClosureCall<'a, 's> {
    pub call: Local<'s>,
    pub function: Local<'s, Closure>,
    pub args: &'a [Tagged<'s>],
    /// The environment the call was made from, or `NULL` for a call from a slot frame that
    /// has none.
    pub sys_parent: Local<'s>,
    /// The method variables of a dispatched call, which its frame binds.
    pub supplied: Option<handles::MethodVariables<'a, 's>>,
}

/// Applies a closure ([`apply_closure`]): a native entry or the native tier answers it, or its
/// frame is a slot frame from [`enter`] to [`leave`], or a fresh environment on the general
/// path.
pub fn apply_closure_in<'s>(
    context: &mut Cx<'s>,
    closure_call: &ClosureCall<'_, 's>,
) -> R<Local<'s>> {
    match prepare_closure_call(context, closure_call)? {
        Prepared::Done(value) => Ok(value),
        Prepared::Slots(frame) => handles::run_slot_frame(context, closure_call, frame),
        Prepared::Heap => run_heap_call(context, closure_call, Signature::Unchecked),
    }
}

/// How a closure call goes on once [`prepare_closure_call`] has looked at it.
enum Prepared<V> {
    /// A native entry or the native tier answered it.
    Done(V),
    /// Its arguments are bound in the slot frame, which [`enter`] pushes.
    Slots(SlotFrame),
    /// It runs in a fresh environment ([`run_heap_call`]).
    Heap,
}

/// Answers a closure call natively when a native entry or the native tier takes it, and
/// otherwise binds its arguments in a slot frame when it gets one.
#[inline(always)]
fn prepare_closure_call<'s>(
    context: &mut Cx<'s>,
    closure_call: &ClosureCall<'_, 's>,
) -> R<Prepared<Local<'s>>> {
    let &ClosureCall {
        call,
        function,
        args,
        supplied,
        ..
    } = closure_call;
    let closure = function.read(context);
    if closure.has_flag(gp::NATIVE_ENTRY)
        && supplied.is_none()
        && let Some(value) = handles::native_entry(context, function, args)?
    {
        return Ok(Prepared::Done(value));
    }
    if crate::annotations::typed_closure(function.read(context))
        && let Some(value) = native_typed(context, closure_call)?
    {
        return Ok(Prepared::Done(value?));
    }
    Ok(match bind_slot_frame(context, call, function, args)? {
        Some(frame) => Prepared::Slots(frame),
        None => Prepared::Heap,
    })
}

/// Runs the call `call` of the typed closure `function` with `args` in the interpreter, as a
/// call the native tier did not take runs, for a call whose native code gave up
/// ([`crate::replay::replay`]).
pub unsafe fn run_interpreted(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
) -> R<Sexp> {
    run_on_heap_frame(call, function, args, sys_parent, None, Entry::Unchecked)
}

/// Runs a call of `function` in a fresh environment that binds `args`, the path of a call
/// that gets no slot frame ([`run_heap_call`]).
pub(crate) unsafe fn run_on_heap_frame(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
    entry: Entry,
) -> R<Sexp> {
    // The caller holds the call, the closure, its caller's environment, the arguments, the
    // method variables and the signature until the call returns.
    handles::in_caller_scope(|context| {
        let supplied = supplied.map(|supplied| handles::method_variables(context, supplied));
        let closure_call = closure_call(
            handles::held(context, call),
            handles::held(context, function),
            handles::tagged(context, args),
            handles::held(context, sys_parent),
            supplied,
        )?;
        let signature = match entry {
            Entry::Unchecked => Signature::Unchecked,
            Entry::Checked(signature) => {
                Signature::Checked(signature.map(|signature| handles::held(context, signature)))
            }
        };
        Ok(run_heap_call(context, &closure_call, signature)?.raw())
    })
}

/// Runs `closure_call` in a fresh environment that binds its arguments ([`run_heap_frame`]).
/// Detaching a reference among the arguments roots its promise, and the roots go with a handle
/// scope of this call alone: a caller that makes many calls (`lapply`) keeps no root per call.
fn run_heap_call<'s>(
    context: &mut Cx<'s>,
    closure_call: &ClosureCall<'_, 's>,
    signature: Signature<'s>,
) -> R<Local<'s>> {
    if !closure_call
        .args
        .iter()
        .any(|arg| is_argument_reference(context, arg.value))
    {
        return run_heap_frame(context, closure_call, signature);
    }
    context.scope_enclosed(|inner, enclosing| {
        let args: SmallVec<[Tagged<'_>; 8]> = closure_call
            .args
            .iter()
            .map(|arg| Tagged {
                tag: enclosing.pass(arg.tag),
                value: enclosing.pass(arg.value),
            })
            .collect();
        let passed = ClosureCall {
            call: enclosing.pass(closure_call.call),
            function: enclosing.pass(closure_call.function),
            args: &args,
            sys_parent: enclosing.pass(closure_call.sys_parent),
            supplied: closure_call
                .supplied
                .map(|supplied| supplied.pass(enclosing)),
        };
        let signature = match signature {
            Signature::Unchecked => Signature::Unchecked,
            Signature::Checked(signature) => {
                Signature::Checked(signature.map(|signature| enclosing.pass(signature)))
            }
        };
        run_heap_frame(inner, &passed, signature)
    })
}

/// Whether `value` is an argument reference, which names a slot of a running frame.
#[inline(always)]
fn is_argument_reference(context: &Cx<'_>, value: Local<'_>) -> bool {
    value
        .downcast::<Promise>()
        .is_some_and(|promise| promise.read(context).has_flag(gp::ARGUMENT_REFERENCE))
}

/// Makes each argument reference among `args` the promise it stands for ([`detach`]), for a
/// frame that can keep its arguments past the call that made the references. The promises are
/// rooted in the caller's scope.
fn detach_arguments<'s>(context: &mut Cx<'s>, args: &[Tagged<'s>]) -> R<()> {
    for arg in args {
        if is_argument_reference(context, arg.value)
            && let Some(reference) = arg.value.downcast::<Promise>()
        {
            let reference = context.root(reference.read(context));
            detach(context, reference)?;
        }
    }
    Ok(())
}

/// Runs `closure_call` in a fresh environment that binds its arguments. An environment can be
/// read after the call that made an argument reference returns, so a reference among the
/// arguments becomes the promise it stands for first.
fn run_heap_frame<'s>(
    context: &mut Cx<'s>,
    closure_call: &ClosureCall<'_, 's>,
    signature: Signature<'s>,
) -> R<Local<'s>> {
    let &ClosureCall {
        call,
        function,
        args,
        sys_parent,
        supplied,
    } = closure_call;
    detach_arguments(context, args)?;
    let variables: SmallVec<[handles::MethodVariable<'_>; 8]>;
    let arguments = match supplied {
        Some(supplied) => {
            variables = supplied.iter().collect();
            Arguments::Matched(&variables)
        }
        None => Arguments::Unsupplied,
    };
    let environment = bind_environment(context, call, function, args, arguments)?;
    let general = GeneralFrame {
        call,
        function,
        body: BodyFrame::Environment(environment),
        sys_parent,
        args,
        method_vars: &[],
    };
    run_general(context, &general, signature)
}

/// Runs `closure_call`, a call of a typed closure, natively when the native tier takes it, and
/// otherwise reports why it runs in the interpreter.
fn native_typed<'s>(
    context: &mut Cx<'s>,
    closure_call: &ClosureCall<'_, 's>,
) -> R<Option<R<Local<'s>>>> {
    let &ClosureCall {
        call,
        function,
        args,
        sys_parent,
        supplied,
    } = closure_call;
    if supplied.is_some() {
        handles::log_interpreted_because(
            context,
            call,
            "it is a method that dispatch called, which the native tier does not take",
        );
        return Ok(None);
    }
    // The native tier binds the arguments itself and may keep one, so it gets promises.
    detach_arguments(context, args)?;
    force_typed_arguments(context, call, args, sys_parent)?;
    Ok(handles::call_native_tier(
        context, call, function, args, sys_parent,
    ))
}

/// Forces, left to right, each argument promise that typed code wrote: a call from one typed
/// function to another evaluates its arguments before the callee runs, as compiled typed code
/// does, so the tiers agree on a typed program. An argument the typed caller names is its own
/// even when the interpreter forwards the caller's promise for a variable (an effect-free
/// callee's eager arguments). An argument written in R code, including one a typed function
/// passes on as `...`, stays lazy, and so does a missing one, as `missing()` reads it.
fn force_typed_arguments(
    context: &mut Cx<'_>,
    call: Local<'_>,
    args: &[Tagged<'_>],
    caller: Local<'_>,
) -> R<()> {
    let dots = handles::symbol(|symbols| symbols.dots);
    let caller = caller.downcast::<Environment>();
    let from_typed_caller =
        caller.is_some_and(|caller| is_typed_frame(context, caller.read(context)));
    let mut expressions = match call.read(context).downcast::<Language>() {
        Some(call) if from_typed_caller => Some(context.root(call.rest())),
        _ => None,
    };
    let mut from_dots = 0usize;
    for arg in args {
        while from_dots == 0
            && let Some(cell) =
                expressions.and_then(|cells| cells.read(context).downcast::<Pairlist>())
            && cell.head().is(dots)
            && let Some(caller) = caller
        {
            let rest = context.root(cell.rest());
            from_dots += handles::dots_cells(context, caller)?.len();
            expressions = Some(rest);
        }
        let named_by_typed_caller = if from_dots > 0 {
            from_dots -= 1;
            false
        } else if let Some(cell) =
            expressions.and_then(|cells| cells.read(context).downcast::<Pairlist>())
        {
            expressions = Some(context.root(cell.rest()));
            true
        } else {
            false
        };
        let Some(promise) = arg.value.downcast::<Promise>() else {
            continue;
        };
        let read = promise.read(context);
        let environment = read.environment().downcast::<Environment>();
        if handles::is_unbound(read.result())
            && (named_by_typed_caller
                || (!read.has_flag(gp::ARGUMENT_REFERENCE)
                    && environment.is_some_and(|environment| is_typed_frame(context, environment))))
            && !match environment {
                Some(environment) => expression_is_missing(context, read.code(), environment)?,
                None => false,
            }
        {
            promise_value(context, promise)?;
        }
    }
    Ok(())
}

/// Whether `environment` is the frame of a live call of a typed function. A typed function
/// never runs on slots, so a promise over a slot frame is R code's.
fn is_typed_frame(context: &Cx<'_>, environment: Gc<'_, Environment>) -> bool {
    if environment.has_flag(gp::SLOT_FRAME) {
        return false;
    }
    interp()
        .frames
        .iter()
        .rev()
        .find(|frame| frame.env.get(context).is(environment))
        .is_some_and(|frame| {
            // `eval` and other builtins push frames too, whose function has no formals to read.
            frame
                .function
                .get(context)
                .downcast::<Closure>()
                .is_some_and(crate::annotations::typed_closure)
        })
}

/// [`apply_closure`] up to the point where the body would run, so that an interpreter loop can
/// run it in place rather than recursing ([`enter`]). A call that does not get a slot frame,
/// or that the general path has to run whole, comes back with its value and
/// [`crate::NO_SLOTS`].
// Inlined so that the interpreter loop, which runs the body right after, does not pay for a
// second function's frame and for passing `Entered` through memory.
#[inline]
pub unsafe fn apply_closure_entering(
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    supplied: Option<&[(Sexp, Sexp)]>,
) -> R<Entered> {
    let whole = |value: Sexp| Entered {
        value,
        header: crate::NO_SLOTS,
        depth: 0,
    };
    match prepare_raw_call(call, function, args, sys_parent, supplied)? {
        Prepared::Done(value) => Ok(whole(value)),
        Prepared::Slots(frame) => enter(
            call,
            function,
            sys_parent,
            args,
            supplied.unwrap_or(&[]),
            frame.header,
            frame.missing,
            frame.promised,
            Entering::Start,
        ),
        Prepared::Heap => {
            let value =
                run_on_heap_frame(call, function, args, sys_parent, supplied, Entry::Unchecked)?;
            Ok(whole(value))
        }
    }
}

/// How [`bind_environment`] takes a call's arguments.
enum Arguments<'a, 's> {
    /// Matched to the formals, by position when that is all they need.
    Unsupplied,
    /// Matched to the formals, with the method variables of a dispatched call bound after.
    Matched(&'a [handles::MethodVariable<'s>]),
    /// By position alone: argument `i` binds formal `i` ([`apply_closure_moved`]).
    Positional,
}

/// A fresh environment for a call of `function` that binds `args` to its formals, rooted.
fn bind_environment<'s>(
    context: &mut Cx<'s>,
    call: Local<'_>,
    function: Local<'_, Closure>,
    args: &[Tagged<'s>],
    arguments: Arguments<'_, 's>,
) -> R<Local<'s, Environment>> {
    let _activity = activity::enter(Activity::CallSetup, 0);
    // Most calls pass every argument by position to a function without `...`, which needs no
    // matching table: argument `i` binds formal `i`, and the formals past the last argument are
    // missing.
    let by_position = match arguments {
        Arguments::Positional => true,
        Arguments::Unsupplied => positional(function.read(context), args),
        Arguments::Matched(_) => false,
    };
    let matched: Option<Matched<'s>> = if by_position {
        None
    } else {
        activity::count(Counter::FormalsMatchedByName);
        Some(match_args_cached(context, function, args, call)?)
    };
    let missing_value = handles::missing_argument();
    let closure = function.read(context);
    let mut count = 0;
    let mut formal = closure.formals();
    while let Some(cell) = formal.downcast::<Pairlist>() {
        count += 1;
        formal = cell.rest();
    }
    // One check covers the frame, a binding cell and a default promise per formal, so they are
    // made without one. The function, the arguments and the matched values are rooted.
    context.reserve(ENVIRONMENT_BYTES + 2 * TRIPLE_BYTES * count);
    let reading: &Cx<'s> = context;
    let closure = function.read(reading);
    let environment = reading.new_function_frame_uncollected(closure.environment(), count)?;
    let mut bindings: SmallVec<[LocalBinding<'_>; 12]> = SmallVec::new();
    let mut formal = closure.formals();
    let mut index = 0;
    while let Some(cell) = formal.downcast::<Pairlist>() {
        let Some(symbol) = cell.tag().downcast::<Symbol>() else {
            return Err(error("internal error: a formal has no name"));
        };
        let actual = match &matched {
            Some(matched) => matched.get(index).copied(),
            None => args.get(index).map(|arg| arg.value),
        };
        let actual = actual.unwrap_or(missing_value);
        let default = cell.head();
        let bound = if actual.is(missing_value) && !default.is(missing_value) {
            // A literal default evaluates to itself, so it is bound as is: the promise would
            // cost an allocation and a force on every call that omits it, for the same value.
            let value = if is_literal(default) {
                default
            } else {
                reading
                    .new_promise_uncollected(default, environment)?
                    .value()
            };
            LocalBinding {
                symbol,
                value,
                missing: true,
            }
        } else {
            LocalBinding {
                symbol,
                value: actual.read(reading),
                missing: false,
            }
        };
        bindings.push(bound);
        formal = cell.rest();
        index += 1;
    }
    reading.define_formals_uncollected(environment, &bindings)?;
    drop(bindings);
    let environment = reading.root(environment);
    if let Arguments::Matched(supplied) = arguments {
        let generic_call_env = handles::symbol(|symbols| symbols.dot_generic_call_env);
        for variable in supplied {
            if context.own_binding(environment, variable.symbol).is_some() {
                continue;
            }
            // A deferred `.GenericCallEnv` (see `method_variable`): this frame's caller is the
            // slot frame the running byte code reads.
            let value =
                if variable.value.is(handles::unbound()) && variable.symbol.is(generic_call_env) {
                    let owner = interp().slot_owner;
                    if owner == crate::NO_SLOTS {
                        panic!(
                            "byte code handed out a slot frame's environment outside any slot frame"
                        );
                    }
                    frame_environment(context, owner as usize, Reify::Site(Site::MethodDispatch))?
                        .value()
                } else {
                    variable.value
                };
            context.define(environment, variable.symbol, value)?;
        }
    }
    Ok(environment)
}

/// The frame a closure call runs in: an environment, or a slot frame
/// (`docs/src/content/docs/development/design/slot-frames.md`) whose header register is
/// `header`, with `missing` its formals' missing bits.
#[derive(Clone, Copy)]
pub enum CallFrame {
    Env(Sexp),
    Slots(SlotFrame),
}

/// A slot frame at `header` whose left-out formals are `missing`, of which `promised` have a
/// default that is code, not a literal ([`CallShape`]).
#[derive(Clone, Copy)]
pub struct SlotFrame {
    pub header: u32,
    pub missing: u64,
    pub promised: u64,
}

/// Binds `args` to `function`'s formals in a slot frame pushed onto the register file, or
/// returns `None` when the call needs an environment from the start: the body is not compiled
/// with a slot table, a debugger, a move or a type signature watches the frame, or a missing
/// formal's default is neither a literal, a symbol, nor code compiled with its own slot table.
pub unsafe fn bind_slots(call: Sexp, function: Sexp, args: &[Arg]) -> R<Option<SlotFrame>> {
    // The caller holds the call, the closure and the arguments until the call returns, and
    // releases the handle stack after it.
    handles::in_caller_scope(|context| {
        let call = handles::held(context, call);
        let Some(function) = handles::held(context, function).downcast::<Closure>() else {
            return Ok(None);
        };
        let args = handles::tagged(context, args);
        bind_slot_frame(context, call, function, args)
    })
}

/// [`bind_slots`] on the handle types.
#[inline(always)]
fn bind_slot_frame<'s>(
    context: &mut Cx<'s>,
    call: Local<'_>,
    function: Local<'_, Closure>,
    args: &[Tagged<'s>],
) -> R<Option<SlotFrame>> {
    let closure = function.read(context);
    if activity::hooks() & (activity::DEBUGGER | activity::MOVES) != 0
        || function.raw() == interp().namespace_loader
        || closure.has_flag(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)
        || crate::annotations::typed_closure(closure)
    {
        return Ok(None);
    }
    if closure.body().r_type() != SexpType::Bytecode
        && !handles::compile_before_call(context, function)?
    {
        return Ok(None);
    }
    bind_formals(context, call, function, args)
}

fn bind_formals<'s>(
    context: &mut Cx<'s>,
    call: Local<'_>,
    function: Local<'_, Closure>,
    args: &[Tagged<'s>],
) -> R<Option<SlotFrame>> {
    let closure = function.read(context);
    let Some((names, registers)) = handles::slot_layout(closure) else {
        return Ok(None);
    };
    let slot_count = names.len();
    let _activity = activity::enter(Activity::CallSetup, 0);
    let missing_value = handles::missing_argument();
    // One walk settles what binding by position needs: every formal has the slot the body's
    // code reads it from, none is `...`, and there is a formal for each argument. A closure
    // whose formals were replaced keeps the body's code, whose table names the formals it was
    // compiled with.
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut formal_count = 0;
    let mut by_position = true;
    let mut formal = closure.formals();
    while let Some(cell) = formal.downcast::<Pairlist>() {
        let name = cell.tag();
        if formal_count >= bytecode::MAX_SLOT_FORMALS
            || !names.get(formal_count).is_some_and(|slot| slot.is(name))
        {
            return Ok(None);
        }
        by_position &= !name.is(dots);
        formal_count += 1;
        formal = cell.rest();
    }
    let actuals = if by_position
        && args.len() <= formal_count
        && args.iter().all(|arg| arg.tag.is(handles::null()))
    {
        None
    } else {
        activity::count(Counter::FormalsMatchedByName);
        Some(match_args_cached(context, function, args, call)?)
    };
    // A left-out formal whose default is code runs as a promise compiled over the slots. The
    // first call that needs one compiles them all, which allocates, so a window that meets
    // them uncompiled gives its stores back and the walk starts again.
    let mut compiled = false;
    loop {
        let closure = function.read(context);
        let mut window = handles::Window::open(context, slot_count, registers)?;
        // Each way of reading the arguments gets its own copy of the loop, which keeps the
        // test of which one out of the per-formal stores.
        let stored = match &actuals {
            Some(actuals) => store_formals(context, closure, &mut window, formal_count, |index| {
                actuals.get(index).copied().unwrap_or(missing_value)
            }),
            None => store_formals(context, closure, &mut window, formal_count, |index| {
                args.get(index).map_or(missing_value, |arg| arg.value)
            }),
        };
        match stored {
            Stored::All { missing, promised } => {
                return Ok(Some(SlotFrame {
                    header: window.close() as u32,
                    missing,
                    promised,
                }));
            }
            Stored::Unfit => return Ok(None),
            Stored::Uncompiled => {}
        }
        drop(window);
        if compiled {
            return Ok(None);
        }
        handles::build_slot_defaults(context, function)?;
        compiled = true;
    }
}

/// How [`store_formals`] ends: every formal stored, a default that fits no slot, or a default
/// whose code is not compiled to run over the slots yet.
enum Stored {
    All { missing: u64, promised: u64 },
    Unfit,
    Uncompiled,
}

/// Stores each of `closure`'s first `formal_count` formals in `window`: the argument `actual`
/// reads for it, or its default when the argument is missing.
#[inline(always)]
fn store_formals<'c, 'a>(
    context: &'c Cx<'_>,
    closure: Gc<'c, Closure>,
    window: &mut handles::Window<'_>,
    formal_count: usize,
    actual: impl Fn(usize) -> Local<'a>,
) -> Stored {
    let missing_value = handles::missing_argument();
    let (mut missing, mut promised) = (0u64, 0u64);
    let mut defaults = None;
    let mut ending = None;
    let mut formal = closure.formals();
    window.fill(formal_count, |index| {
        let cell = formal.downcast::<Pairlist>()?;
        formal = cell.rest();
        let default = cell.head();
        let value = actual(index).read(context);
        if !value.is(missing_value) || default.is(missing_value) {
            return Some(value);
        }
        if !is_literal(default) {
            defaults = defaults.or_else(|| slot_defaults(context, closure));
            let Some(defaults) = defaults else {
                ending = Some(Stored::Uncompiled);
                return None;
            };
            if !default_fits(defaults, index) {
                ending = Some(Stored::Unfit);
                return None;
            }
            promised |= 1 << index;
        }
        missing |= 1 << index;
        Some(default)
    });
    ending.unwrap_or(Stored::All { missing, promised })
}

/// Whether a default goes into a slot as it is: a constant, not code to run.
fn is_literal(default: Gc<'_>) -> bool {
    default.r_type().is_atomic() || default.r_type() == SexpType::Nil
}

/// The defaults of `function`'s formals that run on its slot frame, as
/// [`handles::build_slot_defaults`] keeps them in its slot table, when they are kept for its
/// formals.
fn slot_defaults<'a>(context: &'a Cx<'_>, function: Gc<'a, Closure>) -> Option<Gc<'a, List>> {
    let table = handles::slot_table(context, function)?;
    let kept = (table.len() > 2).then(|| table.get(2))?;
    let (formals, _, defaults) = handles::cell_fields(kept)?;
    if !formals.is(function.formals()) {
        return None;
    }
    defaults.standard::<List>()
}

/// Whether a slot frame can hold the default of the formal at `index` that is code: as a
/// promise of its code compiled to run over the slots, which [`run_frame`] makes once the
/// frame is on the stack.
fn default_fits(defaults: Gc<'_, List>, index: usize) -> bool {
    index < defaults.len() && !defaults.get(index).is(handles::null())
}

/// Starts the call of the S4 method `method` that the generic running on the slot frame at
/// `position` dispatches to, as GNU R's `R_execMethod` runs a method: the method's frame is a
/// new slot frame with the generic's call, caller and arguments, whose formals, the generic's
/// by name and order, take what the generic's hold when it dispatches. A formal the call left
/// out takes the method's default where the generic has a default it has not read, and stays
/// without a value where the generic has none, whatever the method declares. The frame binds
/// the dispatch variables `vars`, and the generic's frame all of them but the last, which is
/// `.Generic`. The caller runs the body and calls [`leave`], as after [`enter`].
///
/// `None`, with nothing changed, when the method needs the general path: what makes
/// [`bind_slots`] or [`enter`] take it, a default that fits no slot, a formal the generic
/// gives a default and the method none, or, in a generic whose body is more than the call of
/// `standardGeneric` (`standard` is false), a default the generic has not read, which its
/// body shares with the method from here on ([`crate::methods::exec_method`]).
pub unsafe fn enter_method(
    position: usize,
    method: Sexp,
    vars: &[(Sexp, Sexp)],
    standard: bool,
) -> R<Option<Entered>> {
    let state = interp();
    let hooks = activity::hooks();
    if hooks & (activity::CAPTURE | activity::DEBUGGER | activity::MOVES) != 0
        || (hooks != 0 && !state.object_systems)
        || method == state.namespace_loader
        || method.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)
        || crate::annotations::is_typed(method)
    {
        return Ok(None);
    }
    if method.cdr().kind() != SexpType::Bytecode
        && bytecode::compile_before_call(method)?.kind() != SexpType::Bytecode
    {
        return Ok(None);
    }
    let state = interp();
    let Some(frame) = state.frames.get(position) else {
        return Ok(None);
    };
    let (call, sys_parent, caller) = (frame.call.raw(), frame.sys_parent.raw(), frame.caller);
    let (source, source_missing) = (frame.slots as usize, frame.missing);
    let generic_vars = frame.method_vars as usize;
    if (sys_parent == nil() && caller == crate::NO_SLOTS) || position + 1 != state.frames.len() {
        return Ok(None);
    }
    // The caller holds the method until the call returns, and the generic's frame holds what
    // its slots do.
    let bound = handles::in_caller_scope(
        #[inline(always)]
        |context| {
            let Some(method) = handles::held(context, method).downcast::<Closure>() else {
                return Ok(None);
            };
            bind_method_formals(context, method, source, source_missing, standard)
        },
    )?;
    let Some((header, missing, promised)) = bound else {
        return Ok(None);
    };
    // GNU R's `R_loadMethod` binds the method and its classes in the generic's frame, where a
    // later dispatch from the same frame replaces them.
    let state = interp();
    let (shared, _) = vars.split_at(vars.len().saturating_sub(1));
    if state.method_vars.len() == generic_vars {
        state.method_vars.extend_from_slice(shared);
    } else {
        for &(symbol, value) in shared {
            let bound = state.method_vars.iter_mut().skip(generic_vars);
            match bound.into_iter().find(|(name, _)| *name == symbol) {
                Some(binding) => binding.1 = value,
                None => state.method_vars.push((symbol, value)),
            }
        }
    }
    let generic_env = state.frames[position].pending.raw();
    if generic_env != nil() {
        let names = shared.iter().map(|&(symbol, _)| symbol);
        crate::heap().expect_dispatch_variables(generic_env, names);
    }
    // The frame's arguments are the generic's. Pushing a frame grows the list they are in.
    let args = Args::from_slice(crate::frame_args(state, position));
    push_slot_frame(
        call, method, sys_parent, caller, &args, vars, header, missing, promised, true,
    )
    .map(Some)
}

/// The slot window of [`enter_method`]: its header register, and its formals' missing and
/// promised bits, from the slots of the generic's frame at `source` whose missing bits are
/// `source_missing`.
#[inline(always)]
fn bind_method_formals(
    context: &mut Cx<'_>,
    method: Local<'_, Closure>,
    source: usize,
    source_missing: u64,
    standard: bool,
) -> R<Option<(u32, u64, u64)>> {
    let _activity = activity::enter(Activity::CallSetup, 0);
    let missing_value = handles::missing_argument();
    // Each formal takes the generic's value or, for one the call left out, the method's default
    // when the generic's is unread. A window dropped before it closes gives back what it stored,
    // and as in `bind_formals`, one that meets the defaults uncompiled starts again once they
    // are compiled.
    let mut compiled = false;
    loop {
        let closure = method.read(context);
        let Some((names, registers)) = handles::slot_layout(closure) else {
            return Ok(None);
        };
        let mut window = handles::Window::open(context, names.len(), registers)?;
        let (mut missing, mut promised) = (0u64, 0u64);
        let mut defaults = None;
        let mut uncompiled = false;
        let mut formal = closure.formals();
        let mut index = 0;
        while let Some(cell) = formal.downcast::<Pairlist>() {
            let (default, name) = (cell.head(), cell.tag());
            if index >= bytecode::MAX_SLOT_FORMALS
                || !names.get(index).is_some_and(|slot| slot.is(name))
            {
                return Ok(None);
            }
            let Some(held) = window.under(source + 1 + index) else {
                return Ok(None);
            };
            let left_out = source_missing & (1 << index) != 0;
            // A default the generic's body forced is the promise GNU R hands on. One it left
            // alone gives way to the method's.
            let read = left_out
                && held
                    .downcast::<Promise>()
                    .is_some_and(|promise| promise.result().raw() != unbound());
            let value = if left_out && !read {
                if default.is(missing_value) || !standard {
                    return Ok(None);
                }
                if !is_literal(default) {
                    defaults = defaults.or_else(|| slot_defaults(context, closure));
                    let Some(defaults) = defaults else {
                        uncompiled = true;
                        break;
                    };
                    if !default_fits(defaults, index) {
                        return Ok(None);
                    }
                    promised |= 1 << index;
                }
                default
            } else {
                held
            };
            missing |= source_missing & (1 << index);
            window.push(value);
            formal = cell.rest();
            index += 1;
        }
        if !uncompiled {
            return Ok(Some((window.close() as u32, missing, promised)));
        }
        drop(window);
        if compiled {
            return Ok(None);
        }
        handles::build_slot_defaults(context, method)?;
        compiled = true;
    }
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
    // The caller holds the closure and the call while it plans.
    handles::with(|context| {
        let (function, call) = (
            handles::held(context, function),
            handles::held(context, call),
        );
        call_plan(context, function, call)
    })
}

/// [`direct_sources`] for code on the handle types.
pub fn call_plan<'s>(
    context: &mut Cx<'s>,
    function: Local<'s>,
    call: Local<'s>,
) -> R<Option<CallPlan>> {
    let Some(function) = function.downcast::<Closure>() else {
        return Ok(None);
    };
    let Some((shape, dots_index)) = plannable(function.read(context)) else {
        return Ok(None);
    };
    let mut args = SmallVec::new();
    if !handles::call_cells(context, call, &mut args) {
        return Ok(None);
    }
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut forwards_dots = false;
    for argument in &args {
        if let Some(symbol) = argument.value.read(context).downcast::<Symbol>() {
            if symbol.is(dots) {
                forwards_dots = true;
            } else if dot_dot_position(symbol.name().bytes()).is_some() {
                return Ok(None);
            }
        }
    }
    if forwards_dots {
        return Ok(Some((shape, smallvec![MATCH_PER_CALL])));
    }
    plan_sources(context, function, shape, dots_index, &args, call)
}

/// The plan of a call whose site plans per call ([`MATCH_PER_CALL`]), from `args`, the
/// arguments it collected with `...` spread out. `None` when they do not match, which the
/// generic path then reports, or a default does not fit a slot.
pub unsafe fn matched_sources(function: Sexp, args: &[Arg], call: Sexp) -> R<Option<CallPlan>> {
    // The caller holds the closure, the call and the arguments while it plans.
    handles::with(|context| {
        let call = handles::held(context, call);
        let Some(function) = handles::held(context, function).downcast::<Closure>() else {
            return Ok(None);
        };
        let args = handles::tagged(context, args);
        let Some((shape, dots_index)) = plannable(function.read(context)) else {
            return Ok(None);
        };
        plan_sources(context, function, shape, dots_index, args, call)
    })
}

/// A call's plan: the [`CallShape`] of the closures it is for, and its sources
/// ([`direct_sources`]).
pub type CallPlan = (CallShape, SmallVec<[i32; 8]>);

/// The [`CallShape`] of `function` and which formal is `...`, when calls of it can take their
/// arguments straight into slots: its body is compiled with a slot table whose first names
/// are its formals.
fn plannable(function: Gc<'_, Closure>) -> Option<(CallShape, Option<usize>)> {
    if function.raw() == interp().namespace_loader || crate::annotations::typed_closure(function) {
        return None;
    }
    let (names, registers) = handles::slot_layout(function)?;
    let mut shape = CallShape {
        formals: 0,
        slots: u16::try_from(names.len()).ok()?,
        registers: u16::try_from(registers).ok()?,
    };
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut dots_index = None;
    let mut formal = function.formals();
    while let Some((_, name, rest)) = handles::cell_fields(formal) {
        let index = shape.formals as usize;
        if index >= bytecode::MAX_SLOT_FORMALS
            || !names.get(index).is_some_and(|slot| slot.is(name))
        {
            return None;
        }
        if name.is(dots) {
            dots_index = Some(index);
        }
        shape.formals += 1;
        formal = rest;
    }
    Some((shape, dots_index))
}

/// The sources of [`direct_sources`] for `args` matched to `function`'s formals.
fn plan_sources(
    context: &mut Cx<'_>,
    function: Local<'_, Closure>,
    shape: CallShape,
    dots_index: Option<usize>,
    args: &[Tagged<'_>],
    call: Local<'_>,
) -> R<Option<CallPlan>> {
    let formal_count = shape.formals as usize;
    // A call whose arguments do not match plans nothing: the generic path reports the error
    // when the call runs.
    let Ok(Ok(plan)) = plan_or_unused(context, Formals::Closure(function), args, call) else {
        return Ok(None);
    };
    let mut sources: SmallVec<[i32; 8]> = smallvec![-1; formal_count];
    if let Some(index) = dots_index {
        sources[index] = DOTS_SOURCE;
    }
    for (position, &target) in plan.iter().enumerate() {
        if Some(target as usize) == dots_index {
            sources.push(position as i32);
        } else if !handles::is_missing_argument(args[position].value)
            && let Some(source) = sources.get_mut(target as usize)
        {
            *source = position as i32;
        }
    }
    // A left-out formal whose default is code needs the defaults compiled over the slots. The
    // first plan that meets them uncompiled compiles them all, which allocates, and the walk
    // starts again with the sources it settled.
    let missing_value = handles::missing_argument();
    let mut compiled = false;
    loop {
        let closure = function.read(context);
        let mut defaults = None;
        let mut uncompiled = false;
        let mut formal = closure.formals();
        for (index, source) in sources[..formal_count].iter_mut().enumerate() {
            let Some(cell) = formal.downcast::<Pairlist>() else {
                break;
            };
            formal = cell.rest();
            let default = cell.head();
            if *source != -1 || default.is(missing_value) {
                continue;
            }
            if is_literal(default) {
                *source = LITERAL_DEFAULT;
                continue;
            }
            defaults = defaults.or_else(|| slot_defaults(context, closure));
            match defaults {
                None => {
                    uncompiled = true;
                    break;
                }
                Some(defaults) if default_fits(defaults, index) => *source = PROMISED_DEFAULT,
                Some(_) => return Ok(None),
            }
        }
        if !uncompiled {
            return Ok(Some((shape, sources)));
        }
        if compiled {
            return Ok(None);
        }
        handles::build_slot_defaults(context, function)?;
        compiled = true;
    }
}

/// Calls `function` for a call whose site plans per call ([`MATCH_PER_CALL`]): in a slot frame
/// when `args` match its formals, and down the generic path, which reports the mismatch,
/// otherwise.
pub unsafe fn call_matching(call: Sexp, function: Sexp, args: &[Arg], sys_parent: Sexp) -> R<Sexp> {
    match matched_sources(function, args, call)? {
        Some((shape, sources)) => call_with_plan(call, function, shape, &sources, args, sys_parent),
        None => apply_closure(call, function, args, sys_parent, None),
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
    let entered = bind_plan(call, function, shape, sources, args, sys_parent)?;
    if entered.header == crate::NO_SLOTS {
        return Ok(entered.value);
    }
    let result = on_large_stack(|| {
        bytecode::run_slots(function, entered.header as usize, shape.slots as usize)
    });
    leave(entered.depth as usize, result)
}

/// The first half of [`call_with_plan`]: binds the arguments into a new slot frame and pushes
/// the call's frame ([`enter`]), leaving the body to the caller, which runs the code
/// [`Entered::value`] on the slots and then calls [`leave`]. A call that takes the generic
/// path runs whole here.
pub unsafe fn bind_plan(
    call: Sexp,
    function: Sexp,
    shape: CallShape,
    sources: &[i32],
    args: &[Arg],
    sys_parent: Sexp,
) -> R<Entered> {
    if activity::hooks() & (activity::DEBUGGER | activity::MOVES) != 0
        || function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)
    {
        let value = apply_closure(call, function, args, sys_parent, None)?;
        return Ok(Entered {
            value,
            header: crate::NO_SLOTS,
            depth: 0,
        });
    }
    // The caller holds the closure and the arguments until the call returns, and releases the
    // handle stack after it.
    let (header, missing, promised) = handles::in_caller_scope(|context| {
        let Some(function) = handles::held(context, function).downcast::<Closure>() else {
            return Err(error(
                "internal error: a planned call's function is not a closure",
            ));
        };
        let args = handles::tagged(context, args);
        bind_planned(context, function, shape, sources, args)
    })?;
    enter(
        call,
        function,
        sys_parent,
        args,
        &[],
        header,
        missing,
        promised,
        Entering::Start,
    )
}

/// The slot window of [`bind_plan`]: its header register, and its formals' missing and
/// promised bits.
#[inline(always)]
fn bind_planned<'s>(
    context: &mut Cx<'s>,
    function: Local<'_, Closure>,
    shape: CallShape,
    sources: &[i32],
    args: &[Tagged<'s>],
) -> R<(u32, u64, u64)> {
    let missing_value = handles::missing_argument();
    let (sources, dots_sources) = sources.split_at((shape.formals as usize).min(sources.len()));
    let mut dots = missing_value;
    if !dots_sources.is_empty() {
        let collected: SmallVec<[Tagged<'s>; 8]> = dots_sources
            .iter()
            .filter_map(|&source| args.get(source as usize).copied())
            .collect();
        if !collected.is_empty() {
            dots = handles::dots(context, &collected)?;
        }
    }
    let mut window =
        handles::Window::open(context, shape.slots as usize, shape.registers as usize)?;
    let (mut missing, mut promised) = (0u64, 0u64);
    // Only a formal the call leaves out reads its default, so the walk over the formals goes
    // no further than the last of them.
    let (mut formal, mut formal_index) = (function.read(context).formals(), 0);
    window.fill(sources.len(), |index| {
        let source = *sources.get(index)?;
        Some(match args.get(source as usize) {
            Some(arg) if source >= 0 => arg.value.read(context),
            _ if source == DOTS_SOURCE => dots.read(context),
            _ if source <= LITERAL_DEFAULT => {
                while formal_index < index
                    && let Some(cell) = formal.downcast::<Pairlist>()
                {
                    formal = cell.rest();
                    formal_index += 1;
                }
                missing |= 1 << index;
                if source == PROMISED_DEFAULT {
                    promised |= 1 << index;
                }
                formal
                    .downcast::<Pairlist>()
                    .map_or(missing_value.read(context), |cell| cell.head())
            }
            _ => missing_value.read(context),
        })
    });
    Ok((window.close() as u32, missing, promised))
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
    let _window = SlotWindow(header, interp().references_used);
    let mut hooks = crate::profile_summary::enter_closure(call, function, sys_parent, &[], nil());
    let state = interp();
    if state.frames.len() as u32 >= state.max_depth {
        return Err(crate::unwind::expression_stack_overflow());
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
        std::iter::empty(),
        header,
        missing,
        caller,
        true,
    );
    interp().frames[depth - 1].role = crate::FrameRole::Inlined;
    let result = sampled.and_then(|()| scope(|| run((depth - 1) as u32)));
    let state = interp();
    let frame = &state.frames[depth - 1];
    let result =
        if frame.env.raw() != nil() || !frame.on_exit.is_empty() || frame.pending.raw() != nil() {
            finish_raw_frame(call, depth, None, result)
        } else {
            pop_raw_frame(depth, result)
        };
    hooks.returned(result.as_ref().ok().copied());
    result
}

/// The environment of the frame at `index`: for a slot frame, the one that stands for it
/// ([`pending_env`]), which binds a variable only once something reads it by name. Everything
/// that hands a frame's environment to R code or native code reads it here.
#[inline]
pub unsafe fn frame_env(index: usize, cause: Reify) -> R<Sexp> {
    let frame = &interp().frames[index];
    if frame.env.raw() != nil() || frame.slots == crate::NO_SLOTS {
        return Ok(frame.env.raw());
    }
    // A profile summary records each request for the environment.
    if frame.pending.raw() != nil() && !activity::enabled() {
        return Ok(frame.pending.raw());
    }
    handles::in_caller_scope(|context| Ok(reified_environment(context, index, cause)?.raw()))
}

/// [`frame_env`] for code on the handle types: `NULL` for a frame that has no environment of
/// its own.
pub fn frame_environment_of<'a>(context: &'a mut Cx<'_>, index: usize, cause: Reify) -> R<Gc<'a>> {
    context.read_or_make(
        |context| {
            let frame = &interp().frames[index];
            if frame.env.raw() != nil() || frame.slots == crate::NO_SLOTS {
                return Some(frame.env.get(context));
            }
            // A profile summary records each request for the environment.
            (frame.pending.raw() != nil() && !activity::enabled())
                .then(|| frame.pending.get(context))
        },
        |context| Ok(reified_environment(context, index, cause)?.value()),
    )
}

/// The environment that stands for the slot frame at `index`, which `cause` asked for.
#[inline(always)]
fn reified_environment<'a>(
    context: &'a mut Cx<'_>,
    index: usize,
    cause: Reify,
) -> R<Gc<'a, Environment>> {
    if activity::enabled() {
        let reason = match cause {
            Reify::Builtin(primitive) => {
                let Some(definition) = interp().builtin(primitive) else {
                    return Err(error("invalid primitive"));
                };
                SITES + builtin_index(definition) as u32
            }
            Reify::Site(site) => site as u32,
        };
        handles::live_frame(context, index).set_reified_by(reason);
    }
    pending_environment(context, index)
}

/// [`frame_environment_of`] for a frame that has an environment or runs on slots, rooted.
pub fn frame_environment<'s>(
    context: &mut Cx<'s>,
    index: usize,
    cause: Reify,
) -> R<Local<'s, Environment>> {
    let environment = context.root_answer(|context| frame_environment_of(context, index, cause))?;
    match environment.downcast::<Environment>() {
        Some(environment) => Ok(environment),
        None => panic!("frame {index} has no environment to evaluate in"),
    }
}

/// What asked for a slot frame's environment, which the profile summary reports as the reason
/// a package call's frame came to read its variables by name, when a lookup through the
/// environment follows.
#[derive(Clone, Copy)]
pub enum Reify {
    /// The builtin whose primitive index is given, through [`crate::builtins::Call::env`].
    Builtin(u32),
    Site(Site),
}

macro_rules! sites {
    ($($site:ident = $name:literal,)*) => {
        /// A place in the runtime that asks for a slot frame's environment, or another reason
        /// a package call takes the generic call protocol. A frame records the reason as a
        /// number ([`Frame::reified_by`]): a site's own, or [`SITES`] plus the index of the
        /// builtin that asked, so recording one compares and stores no text.
        #[derive(Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum Site {
            $($site,)*
        }

        const SITE_NAMES: &[&str] = &[$($name,)*];
    };
}

sites! {
    MethodDispatch = "method dispatch",
    ParentFrame = "parent.frame",
    Subassignment = "subassignment",
    PromiseOverFrame = "promise over the frame",
    Missing = "missing()",
    MissingArgumentLookup = "missing argument lookup",
    MatrixSubassignment = "matrix subassignment",
    EnclosedSubassignment = "subassignment with <<-",
    FunctionDefinition = "function definition",
    DoCall = "do.call",
    Assignment = "assignment",
    S4Dispatch = "S4 dispatch",
    VariableLookup = "variable lookup",
    UncompiledExpression = "uncompiled expression",
    TailUpdate = "tail update",
    SysFrame = "sys.frame",
    SwitchAlternative = "switch alternative",
    ReplacementCall = "replacement call",
    RebindingCall = "rebinding call",
    PromiseEnvironment = "promise environment",
    CallingPosition = "pos = -1",
    OnExit = "on.exit",
    ObjectSystemCheck = "object-system check",
    NativeCode = "native code",
    GenericCallNativeEntry = "generic call: native entry",
    GenericCallArguments = "generic call: arguments",
    GenericCall = "generic call",
    FunctionLookup = "function lookup",
    FunctionGuard = "function guard",
    ForLoop = "for loop",
    EnvironmentCall = "environment()",
    DoCallArgument = "do.call argument",
    DispatchingReplacement = "dispatching replacement",
    DispatchSubjectMethod = "dispatch subject: method",
    DispatchSubject = "dispatch subject",
    Debugger = "debugger",
    BuiltinDispatch = "builtin dispatch",
    BuiltinCall = "builtin call",
    BoundaryCapture = "boundary capture",
    BoundEvaluation = "bound evaluation",
    DollarField = "`$` field",
    EnvironmentRead = "environment read",
    PromiseOrEnvironmentOutlives = "promise or environment outliving the frame",
    ClosureOutlives = "closure outliving the frame",
    FrameWithEnvironment = "frame with an environment",
    PromiseArgumentVariable = "promise argument: variable",
    PromiseArgumentCall = "promise argument: call",
    PromiseArgumentOther = "promise argument: other",
    NotRecorded = "reason not recorded",
}

/// The first reason number that stands for a builtin.
const SITES: u32 = 64;
const _: () = assert!(SITE_NAMES.len() <= SITES as usize);

/// The name the profile summary prints for the reason numbered `reason`.
pub(crate) fn reason_name(reason: u32) -> &'static str {
    match reason.checked_sub(SITES) {
        None => SITE_NAMES.get(reason as usize).copied(),
        Some(index) => interp()
            .builtins
            .get(index as usize)
            .map(|definition| definition.name),
    }
    .unwrap_or("?")
}

/// The reasons that observe a frame's environment: they evaluate code in it, hand it out as a
/// value, or read or write its bindings by name. Every other reason is a mechanism a slot
/// frame could serve without one. `.External2` hands its routine the calling environment, and
/// the routines packages call through it (rlang's `ffi_enquo`, `ffi_eval` and the like) read
/// bindings there.
pub const OBSERVING_REASONS: [&str; 18] = [
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
    "promise or environment outliving the frame",
    "closure outliving the frame",
    ".External2",
];

/// The `...` slot of the frame at `index`, when it is a slot frame that keeps every variable in
/// its slot and has a `...` formal.
#[inline(always)]
pub unsafe fn slot_dots(index: usize) -> Option<Sexp> {
    handles::reading_with(index, raw_frame_dots)
}

/// [`frame_dots`] as a raw pointer, for the raw entry.
#[inline(always)]
fn raw_frame_dots(context: &Cx<'_>, index: usize) -> Option<Sexp> {
    frame_dots(context, index).map(Gc::raw)
}

/// [`slot_dots`] for code on the handle types.
#[inline(always)]
pub fn frame_dots<'c>(context: &'c Cx<'_>, index: usize) -> Option<Gc<'c>> {
    let dots = handles::symbol(|symbols| symbols.dots);
    let [(value, _)] = handles::formal_slots(context, index, [dots])?;
    Some(value)
}

/// The register that holds the variable `symbol` of the frame at `index`, when it is a slot
/// frame that keeps every variable in its slot and has a slot of that name.
#[inline]
pub unsafe fn slot_register(index: usize, symbol: Sexp) -> Option<usize> {
    handles::reading(|context| variable_register(context, index, Gc::adopt(symbol)))
}

/// [`slot_register`] for code on the handle types.
#[inline]
pub fn variable_register(context: &Cx<'_>, index: usize, symbol: Gc<'_, Symbol>) -> Option<usize> {
    let frame = handles::frame_if_live(context, index)
        .filter(|frame| frame.in_slots() && frame.role() != crate::FrameRole::Inlined)?;
    let position = frame.slot_names().iter().position(|name| name.is(symbol))?;
    Some(frame.slot_register(position))
}

/// What the slot of the formal `symbol` holds in the frame at `index`, and whether the formal is
/// missing, when it is a slot frame that keeps every variable in its slot and has such a formal.
#[inline(always)]
pub unsafe fn slot_formal(index: usize, symbol: Sexp) -> Option<(Sexp, bool)> {
    let [found] = handles::reading_with((index, [Gc::adopt(symbol)]), raw_formal_slots)?;
    Some(found)
}

/// [`slot_formal`] for several formals in one walk of the formals, so `symbols` must be listed
/// in the order the closure declares them. `None` when the frame is no slot frame that keeps
/// every variable in its slot, or when a symbol is not a formal at its place in that order.
#[inline(always)]
pub unsafe fn slot_formals<const N: usize>(
    index: usize,
    symbols: [Sexp; N],
) -> Option<[(Sexp, bool); N]> {
    let symbols = symbols.map(|symbol| Gc::adopt(symbol));
    handles::reading_with((index, symbols), raw_formal_slots)
}

/// [`handles::formal_slots`] as raw pointers, for the raw entries.
#[inline(always)]
fn raw_formal_slots<const N: usize>(
    context: &Cx<'_>,
    (index, symbols): (usize, [Gc<'_, Symbol>; N]),
) -> Option<[(Sexp, bool); N]> {
    let found = handles::formal_slots(context, index, symbols)?;
    Some(found.map(|(value, missing)| (value.raw(), missing)))
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
    if frame.sys_parent.raw() != nil() || frame.caller == crate::NO_SLOTS {
        return Ok(frame.sys_parent.raw());
    }
    let caller = frame.caller as usize;
    let fields = &interp().frames[caller];
    if fields.env.raw() != nil() || fields.slots == crate::NO_SLOTS {
        return Ok(fields.env.raw());
    }
    if fields.pending.raw() != nil() && !activity::enabled() {
        return Ok(fields.pending.raw());
    }
    handles::in_caller_scope(|context| Ok(reified_environment(context, caller, cause)?.raw()))
}

/// [`frame_sys_parent`] for code on the handle types.
pub fn caller_environment<'a>(context: &'a mut Cx<'_>, index: usize, cause: Reify) -> R<Gc<'a>> {
    let caller = Cell::new(None);
    context.read_or_make(
        |context| {
            let frame = handles::live_frame(context, index);
            caller.set(frame.caller());
            match caller.get() {
                Some(_) if frame.sys_parent().is_none() => None,
                _ => Some(frame.recorded_sys_parent()),
            }
        },
        |context| match caller.get() {
            Some(caller) => frame_environment_of(context, caller, cause),
            None => panic!("a frame's caller was neither read nor made"),
        },
    )
}

/// The environment of the slot frame whose header register is `header`, which must be live.
pub unsafe fn slot_frame_env(header: usize, cause: Reify) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let index = handles::frame_at_header(context, header).index();
        Ok(frame_environment_of(context, index, cause)?.raw())
    })
}

/// The environment that stands for the slot frame whose header register is `header`, or `nil`
/// when nothing has asked for it yet.
pub fn pending_env_of(header: usize) -> Sexp {
    let frames = &interp().frames;
    frames
        .iter()
        .rposition(|frame| frame.slots as usize == header)
        .map_or(nil(), |index| frames[index].pending.raw())
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
    // The caller holds the call, the closure, its caller's environment and the arguments until
    // the call returns.
    handles::in_caller_scope(|context| {
        let call = handles::held(context, call);
        let Some(function) = handles::held(context, function).downcast::<Closure>() else {
            return Err(error(
                "internal error: a closure call's function is not a closure",
            ));
        };
        let sys_parent = handles::held(context, sys_parent);
        let args = handles::tagged(context, args);
        let environment = bind_environment(context, call, function, args, Arguments::Positional)?;
        let frame = GeneralFrame {
            call,
            function,
            body: BodyFrame::Environment(environment),
            sys_parent,
            args,
            method_vars: &[],
        };
        Ok(run_general(context, &frame, Signature::Unchecked)?.raw())
    })
}

/// Whether every argument is untagged and the formals, which have no `...`, take them all.
fn positional(closure: Gc<'_, Closure>, args: &[Tagged<'_>]) -> bool {
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut count = 0;
    let mut formal = closure.formals();
    while let Some(cell) = formal.downcast::<Pairlist>() {
        if cell.tag().is(dots) {
            return false;
        }
        count += 1;
        formal = cell.rest();
    }
    args.len() <= count && args.iter().all(|arg| arg.tag.is(handles::null()))
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
    let CallFrame::Slots(SlotFrame {
        header,
        missing,
        promised,
    }) = frame
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
        Entering::Start,
    )?;
    if entered.header == crate::NO_SLOTS {
        return Ok(entered.value);
    }
    let slots = bytecode::slot_count(function.cdr());
    let result = on_large_stack(|| bytecode::run_slots(function, header as usize, slots));
    leave(entered.depth as usize, result)
}

/// Whether [`enter`] pushes a frame for every call [`bind_slots`] takes. A caller that pushes
/// a frame to stand for a call, without running the callee's body, asks first, because under
/// the recorder, the debugger or the object-system refusal `enter` runs the call whole.
pub fn enters_slot_frames() -> bool {
    let hooks = activity::hooks();
    hooks & (activity::CAPTURE | activity::DEBUGGER | activity::MOVES) == 0
        && (hooks == 0 || interp().object_systems)
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

/// Where a call stands when [`enter`] pushes its frame.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Entering {
    /// The call starts, so one that needs the general path can run whole.
    Start,
    /// The call is half run: compiled code rebuilds the frame of an inlined call, whose start
    /// already refused what the general path would have refused and already counted the call
    /// for a profile summary. The frame is pushed whatever the session records or debugs.
    Midway,
}

/// Pushes the frame of a call of `function` on the slot frame at `header`, whose arguments are
/// already bound, without running its body, so an interpreter loop can run the body in place
/// rather than recursing. A call that starts and needs what only the general path does (the
/// boundary-call recorder, the debugger, the object-system refusal, a namespace load) runs
/// whole here.
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
    entering: Entering,
) -> R<Entered> {
    let state = interp();
    // A set hook covers the recorder, the debugger and the object-system refusal, so a call
    // that nothing watches makes one test for the three.
    let hooks = activity::hooks();
    if entering == Entering::Start
        && (hooks != 0 || function == state.namespace_loader)
        && (hooks & activity::CAPTURE != 0
            || !state.object_systems
            || function == state.namespace_loader
            || (state.debugger.is_some() && function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)))
    {
        let frame = CallFrame::Slots(SlotFrame {
            header,
            missing,
            promised,
        });
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
    // A `NULL` `sys_parent` comes from byte code running on a slot frame's slots, the caller.
    let caller = if sys_parent == nil() {
        state.slot_owner
    } else {
        crate::NO_SLOTS
    };
    push_slot_frame(
        call,
        function,
        sys_parent,
        caller,
        args,
        method_vars,
        header,
        missing,
        promised,
        entering == Entering::Start,
    )
}

/// The half of [`enter`] past its checks: the frame of the call, whose caller is the frame at
/// `caller` when `sys_parent` is `NULL`, with the promises of the defaults `promised` names.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
unsafe fn push_slot_frame(
    call: Sexp,
    function: Sexp,
    sys_parent: Sexp,
    caller: u32,
    args: &[Arg],
    method_vars: &[(Sexp, Sexp)],
    header: u32,
    missing: u64,
    promised: u64,
    counted: bool,
) -> R<Entered> {
    let state = interp();
    if state.frames.len() as u32 >= state.max_depth {
        release_window(header);
        return Err(crate::unwind::expression_stack_overflow());
    }
    let (depth, sampled) = push_frame(
        call,
        function,
        nil(),
        sys_parent,
        args,
        method_vars.iter().copied(),
        header,
        missing,
        caller,
        counted,
    );
    let defaults = sampled.and_then(|()| match promised {
        0 => Ok(()),
        _ => default_promises(depth - 1, promised),
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
        std::iter::empty(),
        crate::NO_SLOTS,
        0,
        caller,
        false,
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
    let (call, function, header) = (frame.call.raw(), frame.function.raw(), frame.slots);
    let (mark, owner, references) = (frame.handle_mark, frame.previous_owner, frame.references);
    // A call that returns a value from a frame nothing asked about has nothing to catch, run
    // or hand on: no environment a `return` could name, no exit code, no annotation check, no
    // frame above it.
    if let Ok(value) = result
        && header != crate::NO_SLOTS
        && frame.env.raw() == nil()
        && frame.pending.raw() == nil()
        && frame.on_exit.is_empty()
        && state.frames.len() == depth
        && !crate::annotations::checks_pending()
    {
        if activity::hooks() != 0 {
            returning_summarized(function, depth);
        }
        let (args, method_vars, closures) = (frame.args, frame.method_vars, frame.closures);
        let heap = crate::heap();
        heap.release_handles(mark as usize);
        value.root();
        state.slot_owner = owner;
        state.srcref = frame.srcref.raw();
        let Some((handlers, restarts)) = state.frames.pop() else {
            panic!("a closure call's frame was popped while the call ran");
        };
        conditions::restore_handlers(handlers);
        state.restarts = restarts;
        state.frame_args.truncate(args as usize);
        state.method_vars.truncate(method_vars as usize);
        state.frame_closures.truncate(closures as usize);
        let registers = &mut state.bytecode.registers;
        if let Some(slots) = registers.get_mut(header as usize + 1..) {
            heap.release_slots(slots);
        }
        registers.truncate(header as usize);
        release_frame_references(references);
        return Ok(value);
    }
    crate::heap().release_handles(mark as usize);
    match &result {
        Ok(value) => value.root(),
        Err(unwind) => unwind.root(),
    }
    state.slot_owner = owner;
    let result = finish_raw_frame(call, depth, None, result);
    release_window(header);
    release_frame_references(references);
    result
}

/// What a profile summary records as the frame at `depth` returns, as [`pop_frame`] does: the
/// samples that waited, and how the call of `function` ran.
#[cold]
#[inline(never)]
unsafe fn returning_summarized(function: Sexp, depth: usize) {
    if !activity::enabled() {
        return;
    }
    let state = interp();
    if activity::sample_ticked() {
        crate::profile_summary::resolve_samples(state.frames.len());
    }
    let frame = &state.frames[depth - 1];
    let args = state.frame_args.get(frame.args as usize..).unwrap_or(&[]);
    count_package_call(function, Some(frame), args);
}

/// [`run_frame`] for a frame with an environment, and for a slot frame whose call needs more
/// than [`enter`] and [`leave`] do ([`run_general`]).
unsafe fn run_frame_general(
    call: Sexp,
    function: Sexp,
    frame: CallFrame,
    sys_parent: Sexp,
    args: &[Arg],
    entry: Entry,
    method_vars: &[(Sexp, Sexp)],
) -> R<Sexp> {
    // The caller holds the call, the closure, the frame's environment, its caller's
    // environment, the arguments, the method variables and the signature until the call
    // returns.
    handles::in_caller_scope(|context| {
        let call = handles::held(context, call);
        let Some(function) = handles::held(context, function).downcast::<Closure>() else {
            return Err(error(
                "internal error: a closure call's function is not a closure",
            ));
        };
        let body = match frame {
            CallFrame::Env(env) => {
                let Some(environment) = handles::held(context, env).downcast::<Environment>()
                else {
                    return Err(error(
                        "internal error: a closure call's frame is not an environment",
                    ));
                };
                BodyFrame::Environment(environment)
            }
            CallFrame::Slots(frame) => BodyFrame::Slots(frame),
        };
        let sys_parent = handles::held(context, sys_parent);
        let args = handles::tagged(context, args);
        let variables: SmallVec<[handles::MethodVariable<'_>; 8]>;
        let method_vars: &[handles::MethodVariable<'_>] = if method_vars.is_empty() {
            &[]
        } else {
            variables = handles::method_variables(context, method_vars)
                .iter()
                .collect();
            &variables
        };
        let signature = match entry {
            Entry::Unchecked => Signature::Unchecked,
            Entry::Checked(signature) => {
                Signature::Checked(signature.map(|signature| handles::held(context, signature)))
            }
        };
        let frame = GeneralFrame {
            call,
            function,
            body,
            sys_parent,
            args,
            method_vars,
        };
        Ok(run_general(context, &frame, signature)?.raw())
    })
}

/// A closure call on the general path ([`run_general`]).
struct GeneralFrame<'a, 's> {
    call: Local<'s>,
    function: Local<'s, Closure>,
    body: BodyFrame<'s>,
    /// The environment the call was made from, or `NULL` when the caller is the frame below
    /// this one and has no environment yet.
    sys_parent: Local<'s>,
    args: &'a [Tagged<'s>],
    method_vars: &'a [handles::MethodVariable<'s>],
}

/// Where the body of a call on the general path runs: [`CallFrame`] on the handle types.
#[derive(Clone, Copy)]
enum BodyFrame<'s> {
    Environment(Local<'s, Environment>),
    Slots(SlotFrame),
}

/// How a call's arguments meet the closure's annotations: [`Entry`] on the handle types.
enum Signature<'s> {
    /// The argument checks run once the frame is pushed.
    Unchecked,
    /// The caller checked the arguments; holds the signature, `None` for an untyped closure.
    Checked(Option<Local<'s>>),
}

/// Runs the call `frame` describes, on the general path: everything [`enter`] and [`leave`]
/// leave out (the boundary-call recorder, a tracked move, the object-system refusal, the
/// debugger, the argument and return checks, a namespace load) and the body, whose arguments
/// are already bound. A slot frame's registers are released however the call ends.
fn run_general<'s>(
    context: &mut Cx<'s>,
    frame: &GeneralFrame<'_, 's>,
    signature: Signature<'s>,
) -> R<Local<'s>> {
    let &GeneralFrame {
        call,
        function,
        body,
        sys_parent,
        args,
        method_vars,
    } = frame;
    let (environment, slots, missing, promised) = match body {
        BodyFrame::Environment(environment) => (Some(environment), crate::NO_SLOTS, 0, 0),
        BodyFrame::Slots(SlotFrame {
            header,
            missing,
            promised,
        }) => (None, header, missing, promised),
    };
    let _window = SlotWindow(slots, interp().references_used);
    let _activity = activity::enter(Activity::CallSetup, 0);
    let mut recorded = handles::record_closure_entry(
        context,
        call,
        function,
        sys_parent,
        method_vars,
        environment,
    );
    if let Some(environment) = environment {
        moves::entering_closure(function, environment)?;
    }
    let state = interp();
    if state.frames.len() as u32 >= state.max_depth {
        return Err(crate::unwind::expression_stack_overflow());
    }
    // A `NULL` `sys_parent` comes from byte code running on a slot frame's slots, the caller.
    let caller = if sys_parent.is(handles::null()) {
        state.slot_owner
    } else {
        crate::NO_SLOTS
    };
    if !state.object_systems {
        let sys_parent = if caller != crate::NO_SLOTS {
            let site = Reify::Site(Site::ObjectSystemCheck);
            context.root_answer(|context| frame_environment_of(context, caller as usize, site))?
        } else {
            sys_parent
        };
        handles::refuse_object_system(context, call, function, sys_parent)?;
    }
    let entry = handles::FrameEntry {
        call,
        function,
        environment,
        sys_parent,
        args,
        method_vars,
        slots,
        missing,
        caller,
    };
    let (depth, sampled) = handles::push_frame(context, &entry);
    if let Err(unwind) = sampled {
        pop_frame(context, depth, None);
        return Err(unwind);
    }
    let closure = function.read(context);
    if interp().debugger.is_some() && closure.has_flag(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP) {
        closure.set_flags(gp::CLOSURE_STEP, false);
        crate::debugger::debugged_call(depth);
    }
    let signature = match (environment, signature) {
        // A slot frame's closure is never typed (`bind_slots`).
        (None, _) => promise_defaults(context, depth - 1, promised).map(|()| None),
        (Some(environment), Signature::Unchecked) => {
            handles::check_arguments(context, call, function, environment, depth)
        }
        (Some(_), Signature::Checked(signature)) => Ok(signature),
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
    let namespace_load = (function.raw() == interp().namespace_loader).then(|| {
        crate::heap().enter_namespace_load();
        crate::image::touch::enter_namespace_load();
        NamespaceLoad
    });
    let (signature, result) = match signature {
        Ok(signature) => (
            signature,
            handles::run_closure_body(context, function, environment, slots, depth),
        ),
        Err(unwind) => (None, Err(unwind)),
    };
    let loaded = namespace_load.is_some();
    drop(namespace_load);
    if loaded && let Ok(namespace) = &result {
        handles::namespace_loaded(context, *namespace);
    }
    let result = finish_frame(context, call, depth, signature, result);
    let value = result.as_ref().ok().map(|value| value.read(context));
    pop_frame(context, depth, value);
    handles::closure_returned(&mut recorded, &result);
    result
}

/// Runs the body of `function` in its frame at `depth`: on the slot frame at `slots`, as the
/// slot owner, or else in `env`. What the body roots is released, and its value is rooted in
/// the caller's handle scope.
pub(crate) unsafe fn run_closure_body(
    function: Sexp,
    env: Sexp,
    slots: u32,
    depth: usize,
) -> R<Sexp> {
    scope(|| {
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
            match crate::replay::body(function)? {
                Some(replayed) => eval(replayed, env),
                None => bytecode::run_body(function, function.cdr(), env),
            }
        })
    })
}

/// What a call returns once its body has run with `result`: a `return()` aimed at the frame, the
/// return type check, and the `on.exit` expressions, before the frame pops.
#[inline(always)]
fn finish_frame<'s>(
    context: &mut Cx<'s>,
    call: Local<'_>,
    depth: usize,
    signature: Option<Local<'_>>,
    result: R<Local<'s>>,
) -> R<Local<'s>> {
    let result = match return_target(context, depth) {
        None => result,
        Some(target) => stray_loop_jump(call, target, catch_return(context, target, result)),
    };
    let result = match (signature, result) {
        (Some(signature), Ok(value)) => handles::check_return(context, call, signature, value),
        (_, result) => result,
    };
    crate::annotations::leave(depth);
    // `return()` may also come from an `on.exit` expression, which runs in the same frame and
    // so returns from it too.
    let result = if interp().frames[depth - 1].on_exit.is_empty() {
        result
    } else {
        let result = run_on_exit(context, depth, result);
        match return_target(context, depth) {
            None => result,
            Some(target) => catch_return(context, target, result),
        }
    };
    let Some(pending) = handles::live_frame(context, depth - 1).pending() else {
        return result;
    };
    // Lifting the frame's variables allocates, and the flag is cleared after it.
    let pending = context.root(pending);
    if pending.read(context).has_flag(gp::SLOT_FRAME) {
        // An error return keeps the environment, since the unwind may carry it or anything
        // made over it.
        let cause = match &result {
            Ok(value) => handles::outliving_cause(context, depth - 1, *value),
            Err(_) => Some(Site::PromiseOrEnvironmentOutlives),
        };
        match cause {
            Some(cause) => {
                // The variables move while nothing else can run, as in a lookup's move.
                crate::heap().inhibit_collection();
                let lifted = lift(context, depth - 1, Some(cause));
                crate::heap().allow_collection();
                lifted?;
            }
            None => {
                crate::forget_pending_closures(interp(), depth - 1);
                // The environment dies here, so the frame has none left for the pop to release.
                handles::live_frame(context, depth - 1).drop_environment();
                if let Ok(value) = &result {
                    context.discard_frame(pending, *value);
                }
            }
        }
    }
    // The body has returned, so no register of compiled code holds a value of the frame.
    pending.read(context).clear_compiled_frame();
    result
}

/// The environment a `return()` aimed at the frame at `depth` (one-based) names: the frame's
/// own, or for a slot frame that has not made one, the environment that stands for it once it
/// has handed one out.
fn return_target<'a>(context: &'a Cx<'_>, depth: usize) -> Option<Gc<'a>> {
    let frame = handles::live_frame(context, depth - 1);
    let environment = frame.recorded_environment();
    if environment.r_type() != SexpType::Nil {
        return Some(environment);
    }
    frame.pending().map(|pending| pending.value())
}

/// [`finish_frame`] then [`pop_frame`] for raw callers, with the frame's `call` and
/// `signature` held by the frame. One scope serves both, so the result crosses into handles
/// and back once.
unsafe fn finish_raw_frame(
    call: Sexp,
    depth: usize,
    signature: Option<Sexp>,
    result: R<Sexp>,
) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let call = handles::held(context, call);
        let signature = signature.map(|signature| handles::held(context, signature));
        let result = result.map(|value| handles::adopt(context, value));
        let result = finish_frame(context, call, depth, signature, result);
        let context: &Cx<'_> = context;
        let value = result.as_ref().ok().map(|value| value.read(context));
        pop_frame(context, depth, value);
        result.map(Local::raw)
    })
}

/// What keeps the environment of the slot frame at `index` alive past the frame as it returns
/// `result`, or `None` when everything that names the environment dies with the frame, so the
/// variables still in slots never need a cell.
///
/// The environment's counted references decide. Every promise and closure over it counts one,
/// so do a binding, a list element or an enclosure that holds it, and a forced promise has
/// given its count back. The references that die with the frame are known: an unforced promise
/// that only a slot or a cell of the frame holds (a default argument nobody read), and a
/// closure the frame made that only its slots and cells hold. Any count beyond those is a
/// holder that outlives the frame. Three holders are not counted and are checked by hand: the
/// value returned, `R_PreserveObject`, and a global calling handler.
pub(crate) unsafe fn outliving_cause(index: usize, value: Sexp) -> Option<Site> {
    let state = interp();
    let frame = &state.frames[index];
    let env = frame.pending.raw();
    let heap = crate::heap();
    if value == env || heap.is_preserved(env) {
        return Some(Site::PromiseOrEnvironmentOutlives);
    }
    // Nothing counted names the environment, so no promise or closure over it is left to
    // look for.
    if env.refcount() == 0 {
        return None;
    }
    let slots = std::slice::from_raw_parts(
        bytecode::register_address(frame.slots as usize + 1),
        bytecode::frame_slot_names(frame.function.raw()).len(),
    );
    // Only an environment that took a variable has cells to look in.
    let cells = || {
        env.has_gp(gp::PARTIAL_FRAME)
            .then(|| heap.own_cells(env).map(|cell| cell.cdr()))
            .into_iter()
            .flatten()
    };
    let mut dying = 0;
    // Closures defined while the frame ran on its slots, then those defined by name since.
    let in_slots = state.pending_closures
        [crate::pending_closures_of(&state.pending_closures, index)]
    .iter()
    .map(|&(_, closure)| closure);
    let by_name = state
        .frame_closures
        .get(frame.closures as usize..)
        .unwrap_or(&[])
        .iter()
        .copied();
    for closure in in_slots.chain(by_name) {
        // A closure whose environment was replaced no longer names the frame's.
        if closure.tag() != env {
            continue;
        }
        let held = slots.iter().filter(|&&slot| slot == closure).count()
            + cells().filter(|&bound| bound == closure).count();
        if closure == value
            || closure.refcount() as usize != held
            || heap.is_preserved(closure)
            || crate::conditions::handlers_hold(&state.global_handlers, closure)
        {
            return Some(Site::ClosureOutlives);
        }
        dying += 1;
    }
    // Native code that preserved a promise holds it without a counted reference.
    let dies = |bound: Sexp| {
        bound.kind() == SexpType::Promise
            && bound.car() == unbound()
            && bound.tag() == env
            && bound.refcount() == 1
            && bound != value
            && !heap.is_preserved(bound)
    };
    dying += slots.iter().filter(|&&slot| dies(slot)).count();
    dying += cells().filter(|&bound| dies(bound)).count();
    (env.refcount() as usize > dying).then_some(Site::PromiseOrEnvironmentOutlives)
}

/// A slot frame gives up the references of the variables still in its slots, the register file
/// drops the window, and the argument references the frame made for its defaults go back to
/// the pool ([`release_frame_references`], with the pool's level before the frame), on every
/// way out.
struct SlotWindow(u32, u32);

impl Drop for SlotWindow {
    fn drop(&mut self) {
        // SAFETY: the window's header register is live until the window drops.
        unsafe {
            release_window(self.0);
            if self.0 != crate::NO_SLOTS {
                release_frame_references(self.1);
            }
        }
    }
}

/// Releases the registers of the slot frame at `header` and the references of the variables
/// still in its slots. A slot whose variable moved into a cell reads as unbound and gives up
/// nothing.
unsafe fn release_window(header: u32) {
    if header == crate::NO_SLOTS {
        return;
    }
    let header = header as usize;
    let registers = &mut interp().bytecode.registers;
    if let Some(slots) = registers.get_mut(header + 1..) {
        crate::heap().release_slots(slots);
    }
    registers.truncate(header);
}

/// Pushes the frame of a call of the closure `function`, makes the closure's srcref current,
/// and returns the frame's depth. On an error the frame is pushed all the same.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
pub(crate) unsafe fn push_frame(
    call: Sexp,
    function: Sexp,
    env: Sexp,
    sys_parent: Sexp,
    args: &[Arg],
    method_vars: impl Iterator<Item = (Sexp, Sexp)>,
    slots: u32,
    missing: u64,
    caller: u32,
    counted: bool,
) -> (usize, R<()>) {
    let state = interp();
    let start = state.frame_args.len() as u32;
    state.frame_args.extend_from_slice(args);
    let method_vars_start = state.method_vars.len() as u32;
    state.method_vars.extend(method_vars);
    state.calls_ever = state.calls_ever.wrapping_add(1);
    let (handlers, restarts) = (state.handlers.clone(), state.restarts.clone());
    let (srcref, closures) = (state.srcref, state.frame_closures.len() as u32);
    let (handle_mark, previous_owner) = (crate::heap().handle_mark() as u32, state.slot_owner);
    let references = state.references_used;
    state.frames.push_with(|| Frame {
        call: Held::adopt(call),
        function: Held::adopt(function),
        env: Held::adopt(env),
        sys_parent: Held::adopt(sys_parent),
        args: start,
        on_exit: ThinVec::new(),
        handlers,
        restarts,
        srcref: Held::adopt(srcref),
        method_vars: method_vars_start,
        closures,
        slots,
        missing,
        caller,
        pending: Held::adopt(nil()),
        role: crate::FrameRole::Call,
        reified_by: crate::NOT_REIFIED,
        handle_mark,
        previous_owner,
        references,
    });
    let sampled = if activity::hooks() & (activity::SUMMARY | activity::PROFILE_TICK) != 0 {
        profiled_entry(function, counted)
    } else {
        Ok(())
    };
    let state = interp();
    state.srcref = if function.attrib() == nil() {
        nil()
    } else {
        values::get_attr(function, symbols().srcref)
    };
    (state.frames.len(), sampled)
}

/// What the push of `function`'s frame owes a profile: the summary's counts, with the samples
/// that waited filed under the frames beneath the new one, and the `Rprof` sample that is due.
/// One test in [`push_frame`] covers both, so a run without a profile pays for neither.
#[cold]
#[inline(never)]
unsafe fn profiled_entry(function: Sexp, counted: bool) -> R<()> {
    if activity::enabled() {
        crate::profile_summary::entering(function, interp().frames.len() - 1, counted);
    }
    if activity::profile_ticked() {
        record_profile_sample(None)
    } else {
        Ok(())
    }
}

/// Pops the frame at `depth` (one-based) once its call has finished, `value` the value it
/// returned unless it unwound, restoring the condition stacks and srcref of the caller.
#[inline(always)]
fn pop_frame(context: &Cx<'_>, depth: usize, value: Option<Gc<'_>>) {
    if activity::enabled() {
        handles::count_frame_exit(context, depth - 1);
    }
    let state = interp();
    // An unwind that skipped frames above this one needs the general truncation; a call
    // returning normally does not.
    if state.frames.len() != depth {
        crate::truncate_frames(state, depth);
    }
    crate::forget_pending_closures(state, depth - 1);
    if let Some(value) = value {
        handles::release_frame(context, depth - 1, value);
    }
    let Some(frame) = state.frames.last() else {
        panic!("a closure call's frame was popped while the call ran");
    };
    let (closures, srcref) = (frame.closures, frame.srcref.raw());
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
    state.frame_closures.truncate(closures as usize);
}

/// [`pop_frame`] for raw callers, which hold `result` until it returns.
#[inline(always)]
unsafe fn pop_raw_frame(depth: usize, result: R<Sexp>) -> R<Sexp> {
    handles::reading(|context| {
        let value = result.as_ref().ok().map(|&value| Gc::adopt(value));
        pop_frame(context, depth, value);
    });
    result
}

/// Counts a call to a closure of a package namespace, and whether it skipped the generic call
/// protocol: it ran natively without a frame (`frame` is `None`) or in a slot frame that kept
/// every variable in its slot, and no argument was a promise. A call that did not is counted
/// under the reason: what made the frame read its variables by name, a frame that had an
/// environment from the start, or the kind of its first promise argument's code.
#[cold]
pub(crate) unsafe fn count_package_call(function: Sexp, frame: Option<&Frame>, args: &[Arg]) {
    let enclosure = function.tag();
    // A closure made by a call is enclosed by that call's frame, which is no namespace; the
    // flag is in the header at hand, where the namespace test is a lookup.
    if enclosure == values::base_namespace()
        || enclosure.has_gp(gp::FUNCTION_FRAME)
        || enclosure == values::global_env()
        || !crate::envir::is_namespace(enclosure)
    {
        return;
    }
    activity::record(Counter::PackageCalls);
    let reason = match frame {
        Some(frame) if frame.slots == crate::NO_SLOTS => Some(Site::FrameWithEnvironment as u32),
        Some(frame)
            if frame.pending.raw() != nil() && !frame.pending.raw().has_gp(gp::PENDING_FRAME) =>
        {
            Some(if frame.reified_by == crate::NOT_REIFIED {
                Site::NotRecorded as u32
            } else {
                frame.reified_by
            })
        }
        _ => args
            .iter()
            .find(|arg| arg.value.kind() == SexpType::Promise && !is_reference(arg.value))
            .map(|arg| match arg.value.cdr().kind() {
                SexpType::Symbol => Site::PromiseArgumentVariable as u32,
                SexpType::Language => Site::PromiseArgumentCall as u32,
                _ => Site::PromiseArgumentOther as u32,
            }),
    };
    match reason {
        Some(reason) => activity::count_package_fallback(reason),
        None => activity::record(Counter::PackageCallsDirect),
    }
}

/// Replaces the default of each formal in `promised`, which the binding put in its slot as the
/// bare expression, with what forces it over the slot frame at `owner`: an argument reference
/// of the frame to itself, which runs the default's code on the frame's slots, or reads the
/// variable a default names, the first time the formal is read. The reference comes from the
/// pool, so a default costs the call no promise and no environment, and the frame gives it
/// back when it returns ([`release_frame_references`]). A frame that moves the formal into a
/// cell makes it the promise it stands for ([`detach_reference`]).
pub(crate) unsafe fn default_promises(owner: usize, promised: u64) -> R<()> {
    handles::in_caller_scope(|context| promise_defaults(context, owner, promised))
}

/// [`default_promises`] for code on the handle types.
pub(crate) fn promise_defaults(context: &mut Cx<'_>, owner: usize, promised: u64) -> R<()> {
    if promised == 0 {
        return Ok(());
    }
    if slot_defaults(context, handles::live_frame(context, owner).function()).is_none() {
        let function = context.root(handles::live_frame(context, owner).function());
        handles::build_slot_defaults(context, function)?;
    }
    // Readying the pool is all that allocates, so the frame, which holds its function and so
    // the defaults, keeps everything the loop reads.
    ready_references(context, promised.count_ones() as usize)?;
    let context = &*context;
    let frame = handles::live_frame(context, owner);
    let Some(defaults) = slot_defaults(context, frame.function()) else {
        panic!("a slot frame's defaults were not kept for its formals");
    };
    let mut pending = promised;
    while pending != 0 {
        let index = pending.trailing_zeros() as usize;
        pending &= pending - 1;
        let code = defaults.get(index);
        let reference = if code.r_type() == SexpType::Bytecode {
            place_code_reference(context, owner, code)
        } else {
            let slot = frame
                .slot_names()
                .iter()
                .position(|name| name.is(code))
                .map_or(NO_REFERENCE_SLOT, |slot| slot as u32);
            place_reference(context, code, Some((owner, slot)))
        };
        frame.store_slot(index, reference.value());
    }
    Ok(())
}

/// Returns to the pool the argument references a slot frame made for its own defaults
/// ([`default_promises`]), once its slots have given them up: `mark` is
/// [`Frame::references`], the pool's level when the frame was pushed. The references its
/// caller passed sit below the mark and go back when the calling instruction ends.
#[inline(always)]
unsafe fn release_frame_references(mark: u32) {
    if interp().references_used > mark {
        release_references(mark);
    }
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
pub(crate) fn on_large_stack<T>(body: impl FnOnce() -> T) -> T {
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

/// The lowest address of the stack segment the caller runs on, or 0 when it cannot be measured:
/// read from the interpreter's bounds when the caller is on the segment they describe, so
/// native code entered from a closure call reads no thread-local state.
#[inline]
pub fn stack_floor() -> usize {
    let marker = 0u8;
    let pointer = std::ptr::addr_of!(marker) as usize;
    let state = interp();
    if state.stack_floor != 0 && pointer <= state.stack_ceiling && pointer > state.stack_floor {
        return state.stack_floor;
    }
    stacker::remaining_stack().map_or(0, |remaining| pointer.saturating_sub(remaining))
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
pub(crate) unsafe fn refuse_object_system(call: Sexp, function: Sexp, sys_parent: Sexp) -> R<()> {
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
fn stray_loop_jump<'s>(
    call: Local<'_>,
    environment: impl Pointer<Value>,
    result: R<Local<'s>>,
) -> R<Local<'s>> {
    match result {
        Err(Unwind::Jump(Jump::Break { env: target } | Jump::Next { env: target }))
            if target == environment.raw() =>
        {
            Err(handles::error_in(
                call,
                "no loop for break/next, jumping to top level",
            ))
        }
        other => other,
    }
}

/// The value of a `return()` that unwinds to the frame of `environment`.
fn catch_return<'s>(
    context: &Cx<'s>,
    environment: impl Pointer<Value>,
    result: R<Local<'s>>,
) -> R<Local<'s>> {
    match result {
        Err(unwind) => match handles::returned_value(context, &unwind, environment) {
            Some(value) => Ok(value),
            None => Err(unwind),
        },
        ok => ok,
    }
}

/// Runs the `on.exit` expressions of frame `depth` (one-based), keeping the visibility of the
/// value. An unwind from an `on.exit` expression replaces the frame's own result, and a
/// `return()` among them its visibility too.
///
/// While one expression runs the frame lists those after it, so `on.exit()` called from exit
/// code adds to them or replaces them. GNU R runs that list only when the frame is entered
/// again: when the expression left by a jump, or after the last expression of a frame that a
/// jump is unwinding. A frame that returns drops it.
fn run_on_exit<'s>(context: &mut Cx<'s>, depth: usize, result: R<Local<'s>>) -> R<Local<'s>> {
    if interp().frames[depth - 1].on_exit.is_empty() {
        return result;
    }
    let mut visible = interp().visible;
    let outcome = context.scope(|context| {
        let result = match result {
            Ok(value) => Ok(context.root(value.read(context))),
            Err(unwind) => Err(unwind),
        };
        let pass = match exit_code(context, depth) {
            Ok(Some(pass)) => pass,
            Ok(None) => return result,
            Err(unwind) => return Err(unwind),
        };
        // A `return(x)` unwinding to this frame is the value the function returns.
        let returned = match &result {
            Ok(value) => Some(*value),
            Err(unwind) => handles::returned_value(context, unwind, pass.environment),
        };
        interp()
            .returned_values
            .push(returned.map(|value| value.raw()));
        // The value is on its way to the caller and nothing counts it yet, so exit code that
        // modifies a variable holding it would write into the value the caller receives. GNU R
        // counts it for as long as the exit code runs.
        let outcome = handles::referenced_while(context, returned, |context| {
            exit_passes(context, depth, pass, result, &mut visible)
        });
        interp().returned_values.pop();
        outcome
    });
    interp().visible = visible;
    outcome
}

/// The exit code a frame lists for one pass of [`run_on_exit`], taken from the frame, and the
/// environment it runs in.
struct ExitCode<'s> {
    expressions: SmallVec<[Local<'s>; 2]>,
    environment: Local<'s>,
}

/// Takes the `on.exit` expressions of frame `depth`, `None` when it lists none.
fn exit_code<'s>(context: &mut Cx<'s>, depth: usize) -> R<Option<ExitCode<'s>>> {
    let listed = std::mem::take(&mut interp().frames[depth - 1].on_exit);
    if listed.is_empty() {
        return Ok(None);
    }
    // The code taken from the frame is held by nothing else, and making its environment
    // allocates.
    let mut expressions: SmallVec<[Local<'s>; 2]> = SmallVec::new();
    for expression in &listed {
        expressions.push(context.root(expression.get(context)));
    }
    // A slot frame keeps exit code compiled for its slots, which runs there without an
    // environment; only an expression that did not compile needs one. Other exit code, such
    // as the call `withr::defer` registers through `do.call`, runs in the frame's environment,
    // which takes only the variables that code reads.
    let recorded = handles::live_frame(context, depth - 1).recorded_environment();
    let environment = if recorded.r_type() != SexpType::Nil
        || expressions
            .iter()
            .all(|expression| expression.r_type() == SexpType::Bytecode)
    {
        context.root(recorded)
    } else {
        frame_environment(context, depth - 1, Reify::Site(Site::OnExit))?.value()
    };
    Ok(Some(ExitCode {
        expressions,
        environment,
    }))
}

/// Runs the passes of [`run_on_exit`] from `pass` on, with `result` the frame's outcome so far.
fn exit_passes<'s>(
    context: &mut Cx<'s>,
    depth: usize,
    mut pass: ExitCode<'s>,
    result: R<Local<'s>>,
    visible: &mut bool,
) -> R<Local<'s>> {
    let mut outcome = result;
    loop {
        let environment = pass.environment;
        let mut jumped = false;
        for (position, &expression) in pass.expressions.iter().enumerate() {
            let rest = &pass.expressions[position + 1..];
            // The pass took the frame's list, so it stays empty unless exit code added to it.
            if !rest.is_empty() || !interp().frames[depth - 1].on_exit.is_empty() {
                handles::live_frame(context, depth - 1).set_exit_code(rest);
            }
            // A handler stack that a condition left in place must not run the exit code.
            let saved_handlers = interp().handlers.clone();
            let _saved = crate::conditions::set_aside(saved_handlers.clone(), None);
            interp().handlers = interp().frames[depth - 1].handlers.clone();
            let exit = match (
                expression.downcast::<Bytecode>(),
                environment.downcast::<Environment>(),
            ) {
                (Some(code), _) => handles::run_on_slots(context, code, depth - 1),
                (None, Some(environment)) => handles::evaluate(context, expression, environment),
                (None, None) => panic!("exit code that did not compile ran without an environment"),
            };
            interp().handlers = saved_handlers;
            if let Err(unwind) = exit {
                // `on.exit(return(x))` makes `x` the frame's value, even over an error
                // unwinding through it, as in GNU R.
                match handles::returned_value(context, &unwind, environment) {
                    Some(value) => {
                        outcome = Ok(value);
                        *visible = interp().visible;
                        if let Some(top) = interp().returned_values.last_mut() {
                            *top = Some(value.raw());
                        }
                    }
                    None => outcome = Err(unwind),
                }
                jumped = true;
                break;
            }
        }
        let unwinding = matches!(&outcome, Err(unwind) if !unwind.returns_to(environment.raw()));
        if !jumped && !unwinding {
            interp().frames[depth - 1].on_exit.clear();
            break;
        }
        // The unwind waits while the next pass runs R code.
        if let Err(unwind) = &outcome {
            unwind.root();
        }
        pass = match exit_code(context, depth) {
            Ok(Some(next)) => next,
            Ok(None) => break,
            Err(unwind) => {
                outcome = Err(unwind);
                break;
            }
        };
    }
    outcome
}

/// GNU R's argument matching: exact names, then partial names for the formals before `...`, then
/// positions. Returns one value per formal: the supplied value or promise, `R_MissingArg`, or
/// for `...` a `DOTSXP` of what it collects.
pub fn match_args<'s>(
    context: &mut Cx<'s>,
    formals: Local<'_>,
    args: &[Tagged<'s>],
    call: Local<'_>,
) -> R<Matched<'s>> {
    let named = args.iter().any(|arg| is_named(context, arg));
    matched(context, Formals::List(formals), args, call, named)
}

/// The formals a call matches its arguments to: a closure's, read through the closure so that
/// they need no root of their own, or a list the caller holds.
#[derive(Clone, Copy)]
enum Formals<'f> {
    Closure(Local<'f, Closure>),
    List(Local<'f>),
}

impl<'f> Formals<'f> {
    fn read<'a>(self, context: &'a Cx<'_>) -> Gc<'a>
    where
        'f: 'a,
    {
        match self {
            Formals::Closure(function) => function.read(context).formals(),
            Formals::List(formals) => formals.read(context),
        }
    }
}

/// How many formals there are and which one is `...`, the first if several are.
#[derive(Clone, Copy)]
struct FormalShape {
    count: usize,
    dots: Option<usize>,
}

fn formal_shape(formals: Gc<'_>) -> FormalShape {
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut shape = FormalShape {
        count: 0,
        dots: None,
    };
    let mut cell = formals;
    while let Some((_, tag, rest)) = handles::cell_fields(cell) {
        if shape.dots.is_none() && tag.is(dots) {
            shape.dots = Some(shape.count);
        }
        shape.count += 1;
        cell = rest;
    }
    shape
}

/// [`match_args`], where `named` says whether any argument is passed by name.
fn matched<'s>(
    context: &mut Cx<'s>,
    formals: Formals<'_>,
    args: &[Tagged<'s>],
    call: Local<'_>,
    named: bool,
) -> R<Matched<'s>> {
    if !named && let Some(matched) = match_positional_args(context, formals, args)? {
        return Ok(matched);
    }
    let plan = match_plan(context, formals, args, call)?;
    let shape = formal_shape(formals.read(context));
    matched_from_plan(context, shape, args, &plan)
}

/// Which formal each argument binds to: an argument that `...` collects maps to the position of
/// `...`, and an argument that binds nothing to [`BINDS_NOTHING`]. The plan depends only on the
/// formals, the argument names and which named arguments are empty, so a call site that passes
/// the same names to the same function reuses it while no named argument is empty
/// ([`match_args_cached`]).
type Plan = SmallVec<[u32; 8]>;

/// The plan entry of an empty named argument whose formal a positional argument took: GNU R
/// matches `f(a = , 1)` for `function(a, b)` as `f(a = 1)`, because its positional pass tells
/// a formal matched by name from its value, and an empty argument leaves none.
const BINDS_NOTHING: u32 = u32::MAX;

/// [`match_args`] for a call of the closure `function`, reusing the plan of the last call of its
/// compiled body when it had the same formals and argument names. The cache holds one entry in
/// the body's `MATCHES` constant: a list of the formals, the plan, and the argument tags.
fn match_args_cached<'s>(
    context: &mut Cx<'s>,
    function: Local<'_, Closure>,
    args: &[Tagged<'s>],
    call: Local<'_>,
) -> R<Matched<'s>> {
    let formals = Formals::Closure(function);
    let named = args.iter().any(|arg| is_named(context, arg));
    if !named
        || function
            .read(context)
            .body()
            .downcast::<Bytecode>()
            .is_none()
        || args
            .iter()
            .any(|arg| handles::is_missing_argument(arg.value))
    {
        return matched(context, formals, args, call, named);
    }
    let shape = formal_shape(formals.read(context));
    if let Some(plan) = cached_plan(context, function, args) {
        return matched_from_plan(context, shape, args, &plan);
    }
    let plan = match_plan(context, formals, args, call)?;
    let mut stored = context.alloc::<Integer>(plan.len())?;
    for (slot, &formal) in stored.as_mut_slice().iter_mut().zip(&plan) {
        *slot = formal as i32;
    }
    let stored = stored.finish();
    let mut entry = context.alloc::<List>(args.len() + 2)?;
    entry.set(context, 0, function.read(context).formals());
    entry.set(context, 1, stored.value());
    for (index, arg) in args.iter().enumerate() {
        entry.set(context, index + 2, arg.tag);
    }
    let entry = entry.finish();
    handles::set_body_constant(context, function, bytecode::MATCHES, entry.value());
    matched_from_plan(context, shape, args, &plan)
}

/// The plan `function`'s compiled body keeps ([`match_args_cached`]), when it was made for the
/// closure's formals and the tags of `args`.
fn cached_plan(
    context: &Cx<'_>,
    function: Local<'_, Closure>,
    args: &[Tagged<'_>],
) -> Option<Plan> {
    let closure = function.read(context);
    let code = closure.body().downcast::<Bytecode>()?;
    let entry = handles::constants(code)?
        .get(bytecode::MATCHES)?
        .standard::<List>()?;
    let fits = entry.len() == args.len() + 2
        && entry.get(0).is(closure.formals())
        && args
            .iter()
            .enumerate()
            .all(|(index, arg)| entry.get(index + 2).is(arg.tag));
    if !fits {
        return None;
    }
    let plan = entry.get(1).standard::<Integer>()?;
    Some(
        plan.as_slice()
            .iter()
            .map(|&formal| formal as u32)
            .collect(),
    )
}

fn is_named(context: &Cx<'_>, arg: &Tagged<'_>) -> bool {
    argument_name(context, arg).is_some()
}

/// The name an argument is passed by, or `None` for a positional one.
fn argument_name<'a, 's: 'a>(context: &'a Cx<'_>, arg: &Tagged<'s>) -> Option<&'a [u8]> {
    let name = arg.tag.read(context).downcast::<Symbol>()?.name().bytes();
    (!name.is_empty()).then_some(name)
}

fn match_plan(
    context: &mut Cx<'_>,
    formals: Formals<'_>,
    args: &[Tagged<'_>],
    call: Local<'_>,
) -> R<Plan> {
    let unused = match plan_or_unused(context, formals, args, call)? {
        Ok(plan) => return Ok(plan),
        Err(unused) => unused,
    };
    let names: SmallVec<[Option<String>; 4]> = unused
        .iter()
        .map(|&index| {
            argument_name(context, &args[index]).map(|name| String::from_utf8_lossy(name).into())
        })
        .collect();
    let arguments: SmallVec<[(Option<&str>, Local<'_>); 4]> = unused
        .iter()
        .zip(&names)
        .map(|(&index, name)| (name.as_deref(), args[index].value))
        .collect();
    Err(handles::error_in(
        call,
        format!(
            "unused argument{} {}",
            if unused.len() == 1 { "" } else { "s" },
            handles::unused_arguments(context, &arguments)
        ),
    ))
}

fn formal_name(formal: Gc<'_>) -> &[u8] {
    formal
        .downcast::<Symbol>()
        .map_or(&[][..], |symbol| symbol.name().bytes())
}

/// The plan of `args` for `formals`, or the positions of the arguments that match no formal.
fn plan_or_unused(
    context: &Cx<'_>,
    formals: Formals<'_>,
    args: &[Tagged<'_>],
    call: Local<'_>,
) -> R<Result<Plan, SmallVec<[usize; 4]>>> {
    let dots_symbol = handles::symbol(|symbols| symbols.dots);
    // Every closure call matches its arguments, so the working state stays off the heap for the
    // usual handful of formals and arguments.
    let mut formal_symbols: SmallVec<[Gc<'_>; 8]> = SmallVec::new();
    let mut cell = formals.read(context);
    while let Some((_, tag, rest)) = handles::cell_fields(cell) {
        formal_symbols.push(tag);
        cell = rest;
    }
    let multiply_matched = |formal: Gc<'_>| {
        handles::error_in(
            call,
            format!(
                "formal argument \"{}\" matched by multiple actual arguments",
                String::from_utf8_lossy(formal_name(formal))
            ),
        )
    };
    let count = formal_symbols.len();
    let mut matched: SmallVec<[bool; 8]> = smallvec![false; count];
    // The empty argument that matched each formal by name, which a positional argument replaces.
    let mut empty_match: SmallVec<[Option<usize>; 8]> = smallvec![None; count];
    let mut plan: Plan = smallvec![0; args.len()];
    let mut used: SmallVec<[bool; 8]> = smallvec![false; args.len()];
    let dots_position = formal_symbols
        .iter()
        .position(|symbol| symbol.is(dots_symbol));
    let arg_names: SmallVec<[Option<&[u8]>; 8]> =
        args.iter().map(|arg| argument_name(context, arg)).collect();
    // Exact matches.
    for (formal_index, &formal) in formal_symbols.iter().enumerate() {
        if formal.is(dots_symbol) {
            continue;
        }
        let formal_name = formal_name(formal);
        for (arg_index, name) in arg_names.iter().enumerate() {
            if let Some(name) = name
                && *name == formal_name
            {
                if matched[formal_index] {
                    return Err(multiply_matched(formal));
                }
                matched[formal_index] = true;
                plan[arg_index] = formal_index as u32;
                used[arg_index] = true;
                if handles::is_missing_argument(args[arg_index].value) {
                    empty_match[formal_index] = Some(arg_index);
                }
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
        let formal_name = formal_name(formal);
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
                    return Err(handles::error_in(
                        call,
                        format!(
                            "argument {} matches multiple formal arguments",
                            arg_index + 1
                        ),
                    ));
                }
                if matched[formal_index] {
                    return Err(multiply_matched(formal));
                }
                matched[formal_index] = true;
                plan[arg_index] = formal_index as u32;
                used[arg_index] = true;
                partial_owner[arg_index] = Some(formal_index);
                if handles::is_missing_argument(args[arg_index].value) {
                    empty_match[formal_index] = Some(arg_index);
                }
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
            && !formal_symbols[formal_index].is(dots_symbol)
            && matched[formal_index]
            && empty_match[formal_index].is_none()
        {
            formal_index += 1;
        }
        if formal_index < count && !formal_symbols[formal_index].is(dots_symbol) {
            if let Some(empty) = empty_match[formal_index] {
                plan[empty] = BINDS_NOTHING;
            }
            matched[formal_index] = true;
            plan[arg_index] = formal_index as u32;
            formal_index += 1;
        } else if let Some(position) = dots_position {
            plan[arg_index] = position as u32;
        } else {
            unused.push(arg_index);
        }
    }
    Ok(if unused.is_empty() {
        Ok(plan)
    } else {
        Err(unused)
    })
}

/// One value per formal from a plan: the argument mapped to it, the arguments mapped to `...`
/// in order as a `DOTSXP`, or `R_MissingArg`.
fn matched_from_plan<'s>(
    context: &mut Cx<'s>,
    shape: FormalShape,
    args: &[Tagged<'s>],
    plan: &[u32],
) -> R<Matched<'s>> {
    let mut result: Matched<'s> = smallvec![handles::missing_argument(); shape.count];
    for (arg, &formal) in args.iter().zip(plan) {
        if Some(formal as usize) != shape.dots
            && let Some(slot) = result.get_mut(formal as usize)
        {
            *slot = arg.value;
        }
    }
    if let Some(position) = shape.dots {
        let collected: SmallVec<[Tagged<'s>; 8]> = args
            .iter()
            .zip(plan)
            .filter(|(_, formal)| **formal as usize == position)
            .map(|(arg, _)| *arg)
            .collect();
        if !collected.is_empty() {
            result[position] = handles::dots(context, &collected)?;
        }
    }
    Ok(result)
}

/// The common case of `match_args` without its bookkeeping: no argument is named, which the
/// caller has checked, so they fill the formals in order up to `...`, which collects the rest.
/// Returns `None` when one is left over, which the full matcher reports.
fn match_positional_args<'s>(
    context: &mut Cx<'s>,
    formals: Formals<'_>,
    args: &[Tagged<'s>],
) -> R<Option<Matched<'s>>> {
    let dots_symbol = handles::symbol(|symbols| symbols.dots);
    let missing = handles::missing_argument();
    let mut result = Matched::new();
    let mut dots = None;
    let mut remaining = args.iter();
    let mut formal = formals.read(context);
    while let Some((_, tag, rest)) = handles::cell_fields(formal) {
        let value = if dots.is_some() {
            missing
        } else if tag.is(dots_symbol) {
            dots = Some(result.len());
            missing
        } else {
            remaining.next().map_or(missing, |arg| arg.value)
        };
        result.push(value);
        formal = rest;
    }
    // `...` collects what is left, which allocates, so its list goes in once the walk is done.
    let rest = remaining.as_slice();
    if !rest.is_empty() {
        let Some(dots) = dots.and_then(|index| result.get_mut(index)) else {
            return Ok(None);
        };
        *dots = handles::dots(context, rest)?;
    }
    Ok(Some(result))
}

/// Whether `symbol` is missing in `env`: unbound to a supplied argument, or bound to a promise of
/// a symbol that is itself missing in the caller.
pub unsafe fn is_missing(symbol: Sexp, env: Sexp) -> R<bool> {
    // SAFETY: both are live, and the test allocates only with collection inhibited.
    unsafe {
        handles::reading(|context| formal_is_missing(context, Gc::adopt(symbol), Gc::adopt(env)))
    }
}

/// [`is_missing`] for code on the handle types: `missing()` of `symbol` in `environment`,
/// where a formal that got its default counts as missing.
pub fn formal_is_missing(
    context: &Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'_, Environment>,
) -> R<bool> {
    symbol_is_missing(context, symbol, environment, true)
}

/// Whether an argument expression is a symbol bound in `env`'s own frame to a missing argument,
/// as GNU R's `R_isMissing` asks for the subset primitives: `f <- function(m, i) m[i, 1]` called
/// as `f(m, )` passes an empty `i`, which selects everything. A defaulted formal is not missing,
/// and an unbound symbol is left to evaluation to report.
pub unsafe fn argument_is_missing(expression: Sexp, env: Sexp) -> R<bool> {
    // SAFETY: both are live, and the test allocates only with collection inhibited.
    unsafe {
        handles::reading(|context| {
            expression_is_missing(context, Gc::adopt(expression), Gc::adopt(env))
        })
    }
}

/// [`argument_is_missing`] for code on the handle types.
pub fn expression_is_missing(
    context: &Cx<'_>,
    expression: Gc<'_>,
    environment: Gc<'_, Environment>,
) -> R<bool> {
    let Some(symbol) = expression.downcast::<Symbol>() else {
        return Ok(false);
    };
    let dots = handles::symbol(|symbols| symbols.dots);
    if symbol.is(dots) {
        return Ok(false);
    }
    let bound = if dot_dot_position(symbol.name().bytes()).is_some() {
        dots
    } else {
        symbol
    };
    if environment.has_flag(gp::SLOT_FRAME)
        && let Some(answer) = slot_is_missing(context, environment, symbol, false)?
    {
        return Ok(answer);
    }
    if handles::frame_binding(context, environment, bound).is_none() {
        return Ok(false);
    }
    symbol_is_missing(context, symbol, environment, false)
}

/// Whether what a binding or slot holds is a missing argument: the empty argument, or an
/// unforced promise of a name that is missing where the promise reads it.
pub(crate) unsafe fn value_is_missing(value: Sexp) -> R<bool> {
    // SAFETY: the value is live, and the test allocates only with collection inhibited.
    unsafe { handles::reading(|context| content_is_missing(context, Gc::adopt(value))) }
}

/// `missing()` of `symbol` in `env`. `defaults_count` says whether a formal that got its default
/// counts as missing: it does for the function asking, but not for a caller its argument came
/// from, so `h(where)` with `where` defaulted passes a non-missing argument to `h`.
fn symbol_is_missing(
    context: &Cx<'_>,
    symbol: Gc<'_, Symbol>,
    environment: Gc<'_, Environment>,
    defaults_count: bool,
) -> R<bool> {
    // `eval.parent(substitute(missing(x)))` with `x` bound to an empty argument asks about the
    // empty symbol itself, which GNU R reports as missing.
    if handles::is_missing_argument(symbol.value()) {
        return Ok(true);
    }
    if environment.has_flag(gp::SLOT_FRAME)
        && let Some(answer) = slot_is_missing(context, environment, symbol, defaults_count)?
    {
        return Ok(answer);
    }
    let (bound, index) = match dot_dot_position(symbol.name().bytes()) {
        Some(index) => (handles::symbol(|symbols| symbols.dots), Some(index)),
        None => (symbol, None),
    };
    let Some(binding) = handles::frame_binding(context, environment, bound) else {
        return Err(error(format!(
            "'missing({})' did not find an argument",
            handles::text(bound.name())
        )));
    };
    if let Some(index) = index {
        return dots_element_is_missing(context, binding.content(), index);
    }
    if binding.has_flag(gp::BINDING_ABSENT)
        || (defaults_count && binding.has_flag(gp::BINDING_MISSING))
    {
        return Ok(true);
    }
    content_is_missing(context, binding.content())
}

/// [`symbol_is_missing`] of `..index` where `...` holds `dots`.
fn dots_element_is_missing(context: &Cx<'_>, dots: Gc<'_>, index: usize) -> R<bool> {
    if handles::is_missing_argument(dots) || dots.r_type() != SexpType::Dots {
        return Ok(true);
    }
    let mut item = dots;
    for _ in 1..index {
        item = match handles::cell_fields(item) {
            Some((_, _, rest)) if rest.r_type() == SexpType::Dots => rest,
            _ => return Ok(true),
        };
    }
    match handles::cell_fields(item) {
        Some((head, _, _)) => content_is_missing(context, head),
        None => Ok(true),
    }
}

/// [`symbol_is_missing`] answered in the slot of the live slot frame that `env` stands for,
/// which moves nothing, so the frame keeps running on its slots. `None` when the variable is
/// not in a slot: the frame keeps none of that name, or the slot is unbound because the
/// variable moved into a cell or is not bound yet. A formal that got its default is flagged in
/// the frame's missing bits, where a cell carries the flag itself.
fn slot_is_missing(
    context: &Cx<'_>,
    environment: Gc<'_, Environment>,
    symbol: Gc<'_, Symbol>,
    defaults_count: bool,
) -> R<Option<bool>> {
    let (bound, index) = match dot_dot_position(symbol.name().bytes()) {
        Some(index) => (handles::symbol(|symbols| symbols.dots), Some(index)),
        None => (symbol, None),
    };
    if !environment.may_bind(bound) {
        return Ok(None);
    }
    let frame = handles::pending_frame(context, environment);
    let Some(slot) = frame.slot_names().iter().position(|name| name.is(bound)) else {
        return Ok(None);
    };
    let value = frame.slot(slot);
    if handles::is_unbound(value) {
        return Ok(None);
    }
    if let Some(index) = index {
        return dots_element_is_missing(context, value, index).map(Some);
    }
    if defaults_count && frame.is_missing_formal(slot) && slot < frame.formals().count() {
        return Ok(Some(true));
    }
    content_is_missing(context, value).map(Some)
}

/// [`value_is_missing`] for code on the handle types.
pub fn content_is_missing(context: &Cx<'_>, value: Gc<'_>) -> R<bool> {
    // An argument check that may convert wraps its caller's promise in one of its own
    // (`annotations::check_argument`), so the root of the chain is asked, as GNU R's
    // `findRootPromise` finds it.
    let mut value = value;
    while let Some(promise) = value.downcast::<Promise>()
        && handles::is_unbound(promise.result())
        && promise.code().r_type() == SexpType::Promise
    {
        value = promise.code();
    }
    if handles::is_missing_argument(value) {
        return Ok(true);
    }
    let Some(promise) = value.downcast::<Promise>() else {
        return Ok(false);
    };
    if !handles::is_unbound(promise.result()) {
        return Ok(false);
    }
    if promise.has_flag(gp::ARGUMENT_REFERENCE) {
        return reference_is_missing(context, promise);
    }
    let (Some(code), Some(caller)) = (
        promise.code().downcast::<Symbol>(),
        promise.environment().downcast::<Environment>(),
    ) else {
        return Ok(false);
    };
    let dot_dot = dot_dot_position(code.name().bytes()).is_some();
    // A promise over a slot frame that has no environment yet reads the frame's slot, as
    // reifying the frame and asking its binding would, without making the environment.
    if caller.has_flag(gp::PENDING_FRAME) && !dot_dot {
        return pending_slot_is_missing(context, caller, code);
    }
    if caller.has_flag(gp::PARTIAL_FRAME)
        && let Some(answer) = slot_is_missing(context, caller, code, false)?
    {
        return Ok(answer);
    }
    // A promise for `..1` asks about the caller's `...`, the binding that holds it.
    let bound = if dot_dot {
        handles::symbol(|symbols| symbols.dots)
    } else {
        code
    };
    if handles::frame_binding(context, caller, bound).is_some() {
        return symbol_is_missing(context, code, caller, false);
    }
    Ok(false)
}

/// [`symbol_is_missing`] of `symbol` in the slot frame whose pending environment is `pending`,
/// for a caller the argument came from, so a defaulted formal is not missing. A reified slot
/// frame binds each slot to its content and the method variables after them, so reading them in
/// that order asks what its environment would answer.
fn pending_slot_is_missing(
    context: &Cx<'_>,
    pending: Gc<'_, Environment>,
    symbol: Gc<'_, Symbol>,
) -> R<bool> {
    let frame = handles::pending_frame(context, pending);
    if let Some(slot) = frame.slot_names().iter().position(|name| name.is(symbol)) {
        let value = frame.slot(slot);
        return if handles::is_unbound(value) {
            Ok(false)
        } else {
            content_is_missing(context, value)
        };
    }
    match frame.method_vars().find(|(name, _)| name.is(symbol)) {
        Some((_, value)) => content_is_missing(context, value),
        None => Ok(false),
    }
}

// The control-flow specials.

pub const BUILTINS: &[BuiltinDef] = &[
    special("{", -1, safe!(do_begin)).keep_visibility(),
    special("(", 1, safe!(do_paren)),
    special("if", -1, safe!(do_if)).keep_visibility(),
    special("for", -1, safe!(do_for)).invisible(),
    special("while", -1, safe!(do_while)).invisible(),
    special("repeat", -1, safe!(do_repeat)).invisible(),
    special("break", 0, safe!(do_break)),
    special("next", 0, safe!(do_next)),
    special("return", -1, safe!(do_return)),
    special("function", -1, safe!(do_function)),
    special("<-", 2, safe!(do_assign)).invisible(),
    special("=", 2, safe!(do_assign)).invisible(),
    special("<<-", 2, safe!(do_assign)).invisible(),
    special("quote", 1, safe!(do_quote)),
    special("&&", 2, safe!(do_and2)),
    special("||", 2, safe!(do_or2)),
    special("switch", -1, safe!(do_switch)).keep_visibility(),
    special("missing", 1, safe!(do_missing)),
    special("on.exit", -1, safe!(do_on_exit)).invisible(),
    special(".Internal", 1, safe!(do_internal)).keep_visibility(),
    special("substitute", -1, safe!(do_substitute)),
    builtin("invisible", -1, safe!(do_invisible)).invisible(),
    builtin("nargs", 0, safe!(do_nargs)),
    special("forceAndCall", -1, safe!(do_force_and_call)).keep_visibility(),
    special("Tailcall", -1, safe!(do_tailcall)).keep_visibility(),
    special("Exec", -1, safe!(do_exec)).keep_visibility(),
    special("declare", -1, |_| Ok(nil())).invisible(),
    internal("eval", 3, safe!(do_eval)).keep_visibility(),
    internal("withVisible", 1, safe!(do_with_visible)),
    internal_special("Recall", -1, safe!(do_recall)).keep_visibility(),
    builtin(".Primitive", 1, safe!(do_primitive)),
    internal("identity", 1, |call| Ok(call.arg(0))),
    special("~", -1, safe!(do_tilde)),
    internal("delayedAssign", 4, safe!(do_delayed_assign)).invisible(),
    internal("makeLazy", 5, safe!(do_make_lazy)).invisible(),
    builtin("standardGeneric", -1, dispatch::do_standard_generic).keep_visibility(),
    builtin("call", -1, safe!(do_call)),
    special("expression", -1, safe!(do_expression)),
    internal("all.names", 4, safe!(do_all_names)),
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
    internal("builtins", 1, safe!(do_builtins)),
];

fn do_call<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let name = match handles::dense::<Str>(context, arguments.value(0))? {
        Some(name) if name.len() == 1 => name,
        _ => return Err(arguments.error("first argument must be a character string")),
    };
    let function = handles::install_element(context, name, 0)?;
    let mut cells = Cells::new(SexpType::Language);
    cells.push(context, handles::null(), function.value())?;
    for index in 1..arguments.len() {
        cells.push(context, arguments.tag(index), arguments.value(index))?;
    }
    match cells.finish() {
        Some(call) => Ok(call.finish()),
        None => Err(error("internal error: a call without a function")),
    }
}

fn do_expression<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let count = arguments.len();
    let mut result = context.alloc::<Expression>(count)?;
    let mut named = false;
    for index in 0..count {
        result.set(context, index, arguments.value(index));
        named |= arguments.tag(index).r_type() != SexpType::Nil;
    }
    let mut result = result.owned();
    if named {
        let mut names = StringFill::new(context, count)?;
        for index in 0..count {
            match arguments.tag(index).downcast::<Symbol>() {
                Some(tag) => names.push(context, tag.read(context).name()),
                None => names.push(context, handles::blank()),
            }
        }
        let names = names.finish().finish().value();
        handles::set_attribute(context, &mut result, handles::symbol(|s| s.names), names)?;
    }
    Ok(result.finish().value())
}

/// `all.names(expr, functions, max.names, unique)`. Without `functions` the whole function
/// position of a call is skipped, even when it is a call itself.
fn do_all_names<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let functions = handles::logical_scalar(context, arguments.value(1)) == 1;
    let maximum = match handles::integer_scalar_warning(context, arguments.value(2))? {
        -1 => usize::MAX,
        maximum => usize::try_from(maximum).unwrap_or(0),
    };
    let unique = handles::logical_scalar(context, arguments.value(3)) != 0;
    fn walk(expression: Gc<'_>, functions: bool, names: &mut Vec<Gc<'static, Chars>>) {
        if let Some(symbol) = expression.downcast::<Symbol>() {
            let name = handles::permanent(symbol).name();
            if !name.bytes().is_empty() {
                names.push(name);
            }
        } else if let Some(call) = expression.downcast::<Language>() {
            let mut cell = if functions { call.value() } else { call.rest() };
            while let Some((value, _, rest)) = handles::cell_fields(cell) {
                walk(value, functions, names);
                cell = rest;
            }
        } else if let Some(expressions) = expression.standard::<Expression>() {
            for index in 0..expressions.len() {
                walk(expressions.get(index), functions, names);
            }
        }
    }
    // The names are symbols' print names, which the symbol table keeps alive.
    let mut names = Vec::new();
    walk(arguments.value(0).read(context), functions, &mut names);
    if unique {
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| seen.insert(name.bytes()));
    }
    names.truncate(maximum);
    let mut result = StringFill::new(context, names.len())?;
    for name in names {
        result.push(context, name);
    }
    Ok(result.finish().finish().value())
}

fn do_builtins<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let internal_too = handles::logical_scalar(context, arguments.value(0)) == 1;
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
    let mut result = StringFill::new(context, names.len())?;
    for name in names {
        result.push_text(context, name)?;
    }
    Ok(result.finish().finish().value())
}

fn do_begin<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    interp().visible = true;
    let Some(last) = arguments.len().checked_sub(1) else {
        return Ok(handles::null());
    };
    let environment = arguments.environment(context)?;
    let srcrefs = handles::get_attribute(
        context,
        arguments.expression(),
        handles::symbol(|s| s.srcref),
    );
    let srcrefs = context.root(srcrefs);
    // The first srcref is the brace's own; each statement runs with its srcref current, which
    // the calls it makes record for `sys.call` and `traceback`.
    let saved = handles::current_srcref(context);
    // Nothing a statement made stays rooted while the next one runs, not even its value, so an
    // object it drops can be collected (and finalized) by a `gc()` later in the same block.
    for index in 0..last {
        let result = context.scope_reading(|inner, outer| {
            let srcrefs = outer.pass(srcrefs);
            statement(
                inner,
                srcrefs,
                index,
                outer.pass(arguments.value(index)),
                outer.pass(environment),
            )
            .map(|_| ())
        });
        handles::set_srcref(context, saved);
        result?;
    }
    let result = statement(context, srcrefs, last, arguments.value(last), environment);
    handles::set_srcref(context, saved);
    result
}

/// Statement `index` of a `{` evaluated with its srcref current, which the debugger may stop
/// at first.
fn statement<'s>(
    context: &mut Cx<'s>,
    srcrefs: Local<'s>,
    index: usize,
    expression: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    if srcrefs.r_type() != SexpType::Nil {
        let srcref = statement_source(srcrefs.read(context), index + 1);
        handles::set_srcref(
            context,
            srcref.unwrap_or_else(|| handles::null().read(context)),
        );
    }
    if interp().debugger.is_some() {
        handles::debugger_statement(context)?;
    }
    handles::evaluate(context, expression, environment)
}

/// Element `index` of a `srcref` list, or `None` when the list has no such srcref.
pub fn statement_source(srcrefs: Gc<'_>, index: usize) -> Option<Gc<'_>> {
    let srcrefs = srcrefs
        .standard::<List>()
        .filter(|srcrefs| index < srcrefs.len())?;
    let srcref = srcrefs.get(index).standard::<Integer>()?;
    (srcref.len() >= 6).then(|| srcref.value())
}

/// [`statement_source`] for raw callers, `NULL` where there is no srcref.
///
/// # Safety
///
/// `srcrefs` is a live object.
pub unsafe fn statement_srcref(srcrefs: Sexp, index: usize) -> Sexp {
    statement_source(Gc::adopt(srcrefs), index).map_or_else(nil, Gc::raw)
}

fn do_paren<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(1)?;
    let environment = arguments.environment(context)?;
    let expression = arguments.value(0);
    if !expression.is(handles::symbol(|s| s.dots)) {
        return handles::evaluate(context, expression, environment);
    }
    // `(...)` is a builtin call in GNU R, so the dots expand into its single argument.
    let call = arguments.expression();
    let cells = call
        .downcast::<Language>()
        .map_or_else(handles::null, |call| {
            context.root(call.read(context).rest())
        });
    let mut expanded = SmallVec::<[Tagged<'s>; 8]>::new();
    handles::evaluate_arguments(context, &mut expanded, (cells, call), environment, false)?;
    match expanded.as_slice() {
        [argument] => Ok(argument.value),
        _ => Err(arguments.error(format!(
            "{} argument{} passed to '(' which requires 1",
            expanded.len(),
            if expanded.len() == 1 { "" } else { "s" },
        ))),
    }
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
            } else {
                "argument is not interpretable as logical"
            };
            Err(error_in(call, message))
        }
        condition => Ok(condition != 0),
    }
}

fn do_if<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let condition = handles::evaluate(context, arguments.value(0), environment)?;
    if crate::records::is_native_type(condition.r_type()) {
        return Err(handles::boundary_error(
            condition,
            arguments.value(0),
            arguments.expression(),
        ));
    }
    if handles::condition(context, condition, arguments.expression())? {
        handles::evaluate(context, arguments.value(1), environment)
    } else if arguments.len() > 2 {
        handles::evaluate(context, arguments.value(2), environment)
    } else {
        interp().visible = false;
        Ok(handles::null())
    }
}

/// Runs one loop iteration and reports whether the loop goes on: `Ok(false)` after `break`.
fn loop_body<'s>(
    context: &mut Cx<'s>,
    body: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<bool> {
    // Every iteration polls, so even `repeat NULL`, whose body makes no call, can be interrupted.
    crate::unwind::poll_interrupt()?;
    let result = context.scope_reading(|inner, outer| {
        handles::evaluate(inner, outer.pass(body), outer.pass(environment)).map(|_| ())
    });
    handles::loop_continues(result, environment)
}

fn do_for<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    in_loop(context, arguments, |context, environment| {
        run_for(context, arguments, environment)
    })
}

fn run_for<'s>(
    context: &mut Cx<'s>,
    arguments: &handles::Args<'_, 's>,
    environment: Local<'s, Environment>,
) -> R<()> {
    let Some(variable) = arguments.value(0).downcast::<Symbol>() else {
        return Err(arguments.error("invalid for() loop sequence"));
    };
    let mut sequence = handles::evaluate(context, arguments.value(1), environment)?;
    let body = arguments.value(2);
    if handles::inherits(context, sequence, "factor") {
        sequence = handles::factor_as_character(context, sequence)?;
    }
    let kind = sequence.r_type();
    if kind.is_vector()
        && kind != SexpType::I64
        && handles::compiled_loop(context, arguments.expression(), Some(sequence), environment)?
    {
        return Ok(());
    }
    // A compact sequence is stepped through by its first element and increment, as compiled
    // loops do. Other numbers are read one element at a time through `subset::element`, as GNU
    // R's loop reads them through `INTEGER_ELT` and `REAL_ELT`, so no ALTREP number vector is
    // expanded before the first iteration.
    let steps = handles::compact_sequence(sequence).map(|(_, first, step)| (first, step));
    let sequence = match kind {
        _ if steps.is_some() => sequence,
        SexpType::Logical | SexpType::Integer | SexpType::Real => sequence,
        // A pairlist is read as the list of its values, which are the same objects.
        SexpType::Pairlist => handles::pairlist_to_list(context, sequence)?,
        _ => handles::materialize(context, sequence)?,
    };
    let count = match kind {
        SexpType::Nil => 0,
        SexpType::I64 if !handles::is_array(sequence) => {
            return Err(handles::boundary_error(
                sequence,
                arguments.value(1),
                arguments.expression(),
            ));
        }
        SexpType::Record => {
            return Err(handles::boundary_error(
                sequence,
                arguments.value(1),
                arguments.expression(),
            ));
        }
        SexpType::Pairlist => handles::length(context, sequence),
        kind if kind.is_vector() => handles::length(context, sequence),
        _ => return Err(arguments.error("invalid for() loop sequence")),
    };
    // R binds the variable to NULL before the first iteration, which only shows when there is
    // none.
    if count == 0 {
        define_variable(context, variable, handles::null(), environment)?;
    }
    // The body must see the sequence as shared, so that modifying the variable it came from
    // copies it instead of changing the elements still to come.
    handles::referenced_while(context, Some(sequence), |context| {
        for index in 0..count {
            // Each element is rooted when it is made, and the binding keeps it alive after
            // that, so releasing per iteration stops a long loop from holding every element it
            // ever made.
            let goes_on = context.scope_reading(|inner, outer| {
                let sequence = outer.pass(sequence);
                let element = match (steps, sequence.r_type()) {
                    (Some((first, step)), SexpType::Integer) => inner
                        .scalar::<Integer>((first + step * index as f64) as i32)?
                        .finish()
                        .value(),
                    (Some((first, step)), _) => inner
                        .scalar::<Real>(first + step * index as f64)?
                        .finish()
                        .value(),
                    (None, _) => crate::subset::element(inner, sequence, index)?,
                };
                let environment = outer.pass(environment);
                define_variable(inner, outer.pass(variable), element, environment)?;
                loop_body(inner, outer.pass(body), environment)
            })?;
            if !goes_on {
                break;
            }
        }
        Ok(())
    })
}

fn do_while<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    in_loop(context, arguments, |context, environment| {
        let call = arguments.expression();
        if handles::compiled_loop(context, call, None, environment)? {
            return Ok(());
        }
        loop {
            let goes_on = context.scope_reading(|inner, outer| {
                let environment = outer.pass(environment);
                let condition =
                    handles::evaluate(inner, outer.pass(arguments.value(0)), environment)?;
                handles::condition(inner, condition, outer.pass(call))
            })?;
            if !goes_on || !loop_body(context, arguments.value(1), environment)? {
                return Ok(());
            }
        }
    })
}

fn do_repeat<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    in_loop(context, arguments, |context, environment| {
        if handles::compiled_loop(context, arguments.expression(), None, environment)? {
            return Ok(());
        }
        while loop_body(context, arguments.value(0), environment)? {}
        Ok(())
    })
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

impl LoopContext {
    fn new(environment: Local<'_, Environment>, depth: usize, body: Local<'_>) -> Self {
        LoopContext {
            env: environment.raw(),
            depth,
            body: body.raw(),
        }
    }
}

/// Runs a loop of the tree-walking interpreter with its environment on the stack of loops a
/// `break` or `next` can reach. The loop's value is `NULL`.
fn in_loop<'s>(
    context: &mut Cx<'s>,
    arguments: &handles::Args<'_, 's>,
    run: impl FnOnce(&mut Cx<'s>, Local<'s, Environment>) -> R<()>,
) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let body = arguments.value(arguments.len().saturating_sub(1));
    let depth = interp().frames.len();
    interp()
        .loops
        .push(LoopContext::new(environment, depth, body));
    let result = run(context, environment);
    interp().loops.pop();
    result.map(|()| handles::null())
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

fn do_break<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(0)?;
    let environment = arguments.environment(context)?;
    Err(handles::loop_jump(context, environment, false))
}

fn do_next<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(0)?;
    let environment = arguments.environment(context)?;
    Err(handles::loop_jump(context, environment, true))
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
pub(crate) unsafe fn reachable_loop(env: Sexp) -> R<()> {
    let state = interp();
    let depth = state.frames.len();
    let in_loop = state.loops.iter().any(|context| {
        context.env == env && (context.depth == depth || !skips_loop_context(context.body, true))
    });
    let in_closure_body = state.frames.iter().enumerate().any(|(index, frame)| {
        frame.owns(env)
            && frame.function.raw().kind() == SexpType::Closure
            && (index + 1 == depth || has_loop_context(bytecode::source_body(frame.function.raw())))
    });
    if in_loop || in_closure_body {
        return Ok(());
    }
    Err(error("no loop for break/next, jumping to top level"))
}

fn do_return<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let value = match arguments.len() {
        0 => handles::null(),
        1 => handles::evaluate(context, arguments.value(0), environment)?,
        _ => return Err(arguments.error("multi-argument returns are not permitted")),
    };
    // GNU R looks for the function context before jumping, so a `return` with no function to
    // return from is an ordinary error that `tryCatch` can catch.
    if !arguments.in_function(context) {
        return Err(error("no function to return from, jumping to top level"));
    }
    Err(handles::return_jump(environment, value))
}

fn do_function<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    handles::closure(
        context,
        arguments.value(0),
        arguments.value(1),
        arguments.value(2),
        environment,
    )
}

/// A closure of `formals` and `body` in `env`. A `function` call parsed with source references
/// carries its srcref as a fourth element, which becomes the closure's `srcref` attribute.
pub unsafe fn make_closure(formals: Sexp, body: Sexp, srcref: Sexp, env: Sexp) -> R<Sexp> {
    if !matches!(formals.kind(), SexpType::Nil | SexpType::Pairlist) {
        return Err(error("invalid formal argument list for \"function\""));
    }
    if matches!(
        body.kind(),
        SexpType::Closure | SexpType::Builtin | SexpType::Special
    ) {
        return Err(error("invalid body argument for 'function'"));
    }
    let closure = match crate::ahead::take(formals, body, env) {
        Some(closure) => closure,
        None => {
            let closure = triple(SexpType::Closure, formals, body, env)?;
            if srcref != nil() {
                values::set_attr(closure, symbols().srcref, srcref)?;
            }
            closure
        }
    };
    moves::created_closure(closure)?;
    let state = interp();
    if let Some(frame) = state.frames.last()
        && frame.env.raw() == env
        && state.frame_closures.len() - (frame.closures as usize) < crate::FRAME_CLOSURES
    {
        state.frame_closures.push(closure);
    }
    Ok(closure)
}

fn do_quote<'s>(_context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(1)?;
    Ok(arguments.value(0))
}

fn do_and2<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    logical_operator(context, arguments, true)
}

fn do_or2<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    logical_operator(context, arguments, false)
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
    // GNU R's `isNumber`: a factor is an integer vector but not a number.
    let number = match value.kind() {
        SexpType::Logical | SexpType::Real | SexpType::Complex => true,
        SexpType::Integer => !values::inherits(value, "factor"),
        _ => false,
    };
    if !number {
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
    Ok(values::as_logical_scalar(value))
}

fn logical_operator<'s>(
    context: &mut Cx<'s>,
    arguments: &handles::Args<'_, 's>,
    is_and: bool,
) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let call = arguments.expression();
    let operator = arguments.name();
    let left = handles::evaluate(context, arguments.value(0), environment)?;
    let left = handles::logical_operand(context, left, call, operator, "x")?;
    let na = heap::object::NA_LOGICAL;
    if is_and && left == 0 {
        return handles::logical_constant(context, 0);
    }
    if !is_and && left == 1 {
        return handles::logical_constant(context, 1);
    }
    let right = handles::evaluate(context, arguments.value(1), environment)?;
    let right = handles::logical_operand(context, right, call, operator, "y")?;
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
    handles::logical_constant(context, result)
}

fn do_switch<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    if arguments.is_empty() {
        return Err(arguments.error("'EXPR' is missing"));
    }
    let environment = arguments.environment(context)?;
    let selector = handles::evaluate(context, arguments.value(0), environment)?;
    // `switch(x, ...)` forwards the caller's alternatives, so `...` expands to its promises.
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut alternatives: SmallVec<[(Local<'s>, Local<'s>); 8]> = SmallVec::new();
    for index in 1..arguments.len() {
        let (tag, value) = (arguments.tag(index), arguments.value(index));
        if value.is(dots) && tag.r_type() == SexpType::Nil {
            alternatives.extend(handles::dots_cells(context, environment)?);
        } else {
            alternatives.push((tag, value));
        }
    }
    let call = arguments.expression();
    match handles::switch_alternative(context, selector, call, &alternatives)? {
        Some(chosen) => handles::evaluate(context, chosen, environment),
        None => {
            interp().visible = false;
            Ok(handles::null())
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
    let mut defaults = alternatives
        .iter()
        .filter(|arg| arg.tag == nil() || symbol_name(arg.tag).is_empty());
    let default = defaults.next();
    if let (Some(first), Some(second)) = (default, defaults.next()) {
        return Err(duplicate_defaults_error(
            &deparse::deparse_one_line(first.value),
            &deparse::deparse_one_line(second.value),
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
    match default {
        Some(default) if default.value == missing => Err(switch_empty_default()),
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
pub(crate) unsafe fn switch_empty_default() -> Unwind {
    missing_argument(unbound(), "", ArgumentReader::Eval)
}

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

/// The error for a `switch` with two defaults, given their deparsed texts.
pub(crate) fn duplicate_defaults_error(first: &str, second: &str) -> Unwind {
    error(format!(
        "duplicate 'switch' defaults: '{first}' and '{second}'"
    ))
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

fn do_missing<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    arguments.check_arity(1)?;
    let name = arguments.value(0);
    let symbol = match handles::dense::<Str>(context, name)? {
        Some(text) if text.len() == 1 => Some(handles::install_name_at(context, text, 0)?),
        _ => name
            .read(context)
            .downcast::<Symbol>()
            .map(handles::permanent),
    };
    let Some(symbol) = symbol else {
        return Err(arguments.error("invalid use of 'missing'"));
    };
    let environment = arguments.environment(context)?;
    let missing = formal_is_missing(context, symbol, environment.read(context))?;
    handles::logical_constant(context, missing as i32)
}

fn do_on_exit<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let [expression, add, after] = arguments.matched(context, ["expr", "add", "after"])?;
    let add = match add {
        Some(add) => {
            let environment = arguments.environment(context)?;
            let add = handles::evaluate(context, add, environment)?;
            handles::logical_scalar(context, add) == 1
        }
        None => false,
    };
    let after = match after {
        Some(after) => {
            let environment = arguments.environment(context)?;
            let after = handles::evaluate(context, after, environment)?;
            handles::logical_scalar(context, after) != 0
        }
        None => true,
    };
    let expression = expression.unwrap_or_else(handles::null);
    arguments.register_on_exit(context, expression, add, after);
    Ok(handles::null())
}

fn do_internal<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let invalid = || arguments.error("invalid .Internal() argument");
    let inner = arguments
        .value(0)
        .downcast::<Language>()
        .ok_or_else(invalid)?;
    let name = inner.read(context).head();
    let name = context.root(name.downcast::<Symbol>().ok_or_else(invalid)?);
    let Some(function) = handles::internal_function(context, name) else {
        return Err(arguments.error(format!(
            "there is no .Internal function '{}'",
            handles::text(name.read(context).name())
        )));
    };
    let definition = handles::builtin_definition(function)?;
    let environment = arguments.environment(context)?;
    let cells = context.root(inner.read(context).rest());
    let mut values = SmallVec::<[Tagged<'s>; 8]>::new();
    if function.r_type() == SexpType::Builtin {
        handles::evaluate_arguments(
            context,
            &mut values,
            (cells, inner.value()),
            environment,
            definition.keep_missing,
        )?;
    } else {
        let mut cell = cells.read(context).downcast::<Pairlist>();
        while let Some(current) = cell {
            values.push(Tagged {
                tag: context.root(current.tag()),
                value: context.root(current.head()),
            });
            cell = current.rest().downcast::<Pairlist>();
        }
    }
    handles::dispatch_builtin_call(
        context,
        definition,
        (inner.value(), function),
        &values,
        environment,
    )
}

fn do_invisible<'s>(_context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    Ok(arguments.value(0))
}

fn do_nargs<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    // Outside any closure, such as at top level, GNU R finds no call to count and answers NA.
    let count = arguments.frame_argument_count(context);
    handles::integer(context, count)
}

fn do_force_and_call<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let count = handles::evaluate(context, arguments.value(0), environment)?;
    let count = handles::integer_scalar(context, count).max(0) as usize;
    let function = handles::evaluate(context, arguments.value(1), environment)?;
    // The callee's arguments are the call's own cells after `n` and `FUN` whenever the special
    // received them unchanged, which is every call written in source, so `Reduce` and the
    // apply family pay no pairlist per call. GNU R conses the same tail into one new call.
    // A call without `FUN` reaches the "attempt to apply non-function" error below, as in GNU R.
    let rest = match call_tail(context, arguments) {
        Some(rest) => rest,
        None => {
            let mut cells = Cells::new(SexpType::Pairlist);
            for index in 2..arguments.len() {
                cells.push(context, arguments.tag(index), arguments.value(index))?;
            }
            cells.finish().map_or_else(handles::null, Owned::finish)
        }
    };
    let inner_call = handles::language(context, arguments.value(1), rest)?;
    if function.r_type() != SexpType::Closure {
        return handles::apply_call(context, inner_call.value(), function, rest, environment);
    }
    // As for a compiled call (see `op_call_generic`): a variable passes its value, or the
    // caller's own promise for it, when no argument can assign and neither the callee nor a
    // handler can look at the promise before it is forced.
    let eager = no_calls(context, rest)
        && !crate::effects::calling_handler_established()
        && handles::effect_free(context, function)?;
    let values = handles::closure_arguments(context, rest, environment, eager)?;
    for argument in values.iter().take(count) {
        if argument.value.r_type() == SexpType::Promise {
            handles::force(context, argument.value)?;
        }
    }
    handles::apply_closure_tagged(context, inner_call, function, &values, Some(environment))
}

/// Whether no argument expression in the pairlist `arguments` is a call.
fn no_calls(context: &Cx<'_>, arguments: Local<'_>) -> bool {
    let mut cell = arguments.read(context).downcast::<Pairlist>();
    while let Some(current) = cell {
        if matches!(
            current.head().r_type(),
            SexpType::Language | SexpType::Bytecode | SexpType::Promise
        ) {
            return false;
        }
        cell = current.rest().downcast::<Pairlist>();
    }
    true
}

/// The cells of the call after its first two arguments, when they hold exactly the arguments
/// after the first two.
fn call_tail<'s>(context: &Cx<'s>, arguments: &handles::Args<'_, 's>) -> Option<Local<'s>> {
    let call = arguments
        .expression()
        .read(context)
        .downcast::<Language>()?;
    let mut cell = call.rest();
    for _ in 0..2 {
        cell = cell.downcast::<Pairlist>()?.rest();
    }
    let tail = cell;
    for index in 2..arguments.len() {
        let current = cell.downcast::<Pairlist>()?;
        if !current.head().is(arguments.value(index)) || !current.tag().is(arguments.tag(index)) {
            return None;
        }
        cell = current.rest();
    }
    cell.is(handles::null()).then(|| context.root(tail))
}

/// `Tailcall(FUN, ...)`, R 4.4's request to call `FUN(...)` in place of the calling frame.
/// GNU R only reuses the frame when the call is in tail position and no `on.exit` is pending,
/// and otherwise evaluates the call normally, which is what this does every time: the value is
/// the same, but deep recursion still grows the stack.
fn do_tailcall<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    if arguments.is_empty() || handles::is_missing_argument(arguments.value(0)) {
        let call = handles::lexical_call(context, environment);
        return Err(missing_argument(
            call.raw(),
            "FUN",
            ArgumentReader::TailcallRec,
        ));
    }
    let function = match handles::dense::<Str>(context, arguments.value(0))? {
        Some(name) if name.len() == 1 => handles::install_element(context, name, 0)?.value(),
        _ => arguments.value(0),
    };
    let mut cells = Cells::new(SexpType::Language);
    cells.push(context, handles::null(), function)?;
    for index in 1..arguments.len() {
        cells.push(context, arguments.tag(index), arguments.value(index))?;
    }
    let call = cells.finish().map_or_else(handles::null, Owned::finish);
    handles::evaluate(context, call, environment)
}

/// `Exec(expr, envir)`: evaluates the call `expr` in `envir`, by default the caller's frame,
/// as a tail call in the same sense as [`do_tailcall`].
fn do_exec<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.environment(context)?;
    let mut expression = None;
    let mut envir = None;
    let mut positional = SmallVec::<[Local<'s>; 2]>::new();
    for index in 0..arguments.len() {
        let name = arguments
            .tag(index)
            .read(context)
            .downcast::<Symbol>()
            .map(|tag| tag.name().bytes());
        match name {
            Some(b"expr") => expression = Some(arguments.value(index)),
            Some(b"envir") => envir = Some(arguments.value(index)),
            _ => positional.push(arguments.value(index)),
        }
    }
    let mut positional = positional.into_iter();
    let expression = expression.or_else(|| positional.next());
    let envir = envir.or_else(|| positional.next());
    if positional.next().is_some() {
        return Err(error("unused argument"));
    }
    // GNU R keeps a missing argument passed on as `expr` missing instead of evaluating it.
    let passed = match expression {
        Some(expression) if !handles::is_missing_argument(expression) => {
            let missing = match expression.downcast::<Symbol>() {
                Some(symbol) => expression_is_missing(
                    context,
                    symbol.read(context).value(),
                    environment.read(context),
                )?,
                None => false,
            };
            (!missing).then_some(expression)
        }
        _ => None,
    };
    let Some(expression) = passed else {
        let call = handles::lexical_call(context, environment);
        return Err(missing_argument(
            call.raw(),
            "expr",
            ArgumentReader::Tailcall,
        ));
    };
    let mut expression = handles::evaluate(context, expression, environment)?;
    if expression.r_type() == SexpType::Expression && handles::length(context, expression) == 1 {
        expression = handles::item(context, expression, 0)?;
    }
    if expression.r_type() != SexpType::Language {
        return Err(error("\"expr\" must be a call expression"));
    }
    let target = match envir.filter(|envir| !handles::is_missing_argument(*envir)) {
        Some(envir) => {
            let envir = handles::evaluate(context, envir, environment)?;
            envir.downcast::<Environment>().ok_or_else(|| {
                error(format!(
                    "invalid 'envir' argument of type '{}'",
                    envir.r_type().name()
                ))
            })?
        }
        None => environment,
    };
    handles::evaluate(context, expression, target)
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
        return apply_closure(call, function, &args, env, None);
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
        closure_args_into::<PROMISES_PASSING_REFERENCES>(arguments.cdr(), env, &mut args)?;
        true
    } else {
        closure_args_into::<PROMISES_PASSING_REFERENCES>(arguments, env, &mut args)?;
        false
    };
    let result = call_seeded(call, function, env, &mut args, count, seeded, known);
    release_references(mark);
    result
}

/// [`force_and_call_with`] for the closure `function` called from the slot frame at `frame`,
/// which has no environment: `first` binds as a forced argument reference to the call's first
/// argument, and the frame's `...` passes on what it holds. The frame keeps running on its
/// slots unless the callee asks for its caller's environment.
pub unsafe fn force_and_call_from_slots(
    call: Sexp,
    function: Sexp,
    frame: usize,
    first: Sexp,
    known: Option<&mut KnownFirst>,
) -> R<Sexp> {
    let mut args = Args::new();
    let mark = interp().references_used;
    let reference = argument_reference(frame, NO_REFERENCE_SLOT, call.cdr().car())?;
    crate::heap().set_car(reference, first);
    args.push(Arg {
        tag: nil(),
        value: reference,
    });
    if let Some(dots) = slot_dots(frame) {
        activity::count(Counter::DotsCalls);
        let mut cell = dots;
        while matches!(cell.kind(), SexpType::Dots | SexpType::Pairlist) {
            args.push(Arg {
                tag: cell.tag(),
                value: cell.car(),
            });
            cell = cell.cdr();
        }
    }
    let result = call_seeded(call, function, nil(), &mut args, 1, true, known);
    release_references(mark);
    result
}

/// [`force_and_call_with`] for the builtin `function` called from the slot frame at `frame`,
/// which has no environment: `first` and the forced items of the frame's `...` are its
/// arguments. `None` when the call needs the environment, because an argument is an object the
/// builtin may dispatch on, an item of `...` is empty, or forcing an item gave the frame its
/// environment; the items forced so far stay forced, so evaluating the call again runs nothing
/// twice.
pub unsafe fn call_builtin_from_slots(
    call: Sexp,
    function: Sexp,
    frame: usize,
    first: Sexp,
) -> R<Option<Sexp>> {
    let definition = builtin_def(function)?;
    let mut args = Args::new();
    crate::heap().root(first);
    args.push(Arg {
        tag: nil(),
        value: first,
    });
    let mut plain = true;
    let mut forced = Ok(());
    if let Some(dots) = slot_dots(frame) {
        activity::count(Counter::DotsCalls);
        // As in `evaluate_args`, each value counts as referenced while the later ones evaluate.
        first.increment_refcount();
        let mut cell = dots;
        while matches!(cell.kind(), SexpType::Dots | SexpType::Pairlist) {
            let item = cell.car();
            if item == missing_arg() {
                plain = false;
                break;
            }
            match value_of(item) {
                Ok(value) => {
                    crate::heap().root(value);
                    value.increment_refcount();
                    args.push(Arg {
                        tag: cell.tag(),
                        value,
                    });
                }
                Err(unwind) => {
                    forced = Err(unwind);
                    break;
                }
            }
            cell = cell.cdr();
        }
        for arg in &args {
            arg.value.decrement_refcount();
        }
    }
    forced?;
    if !plain
        || interp().frames[frame].env.raw() != nil()
        || (definition.generic != Generic::None && args.iter().any(|arg| arg.value.is_object()))
    {
        return Ok(None);
    }
    call_builtin(definition, call, function, &args, nil()).map(Some)
}

/// The rest of [`force_and_call_with`] for a closure `function`, with `args` made. A seeded
/// first argument and what `...` passed on can be argument references, which a slot frame
/// takes as they are.
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
        return run_frame(
            call,
            function,
            CallFrame::Slots(frame),
            env,
            args,
            entry,
            &[],
        );
    }
    run_on_heap_frame(call, function, args, env, None, entry)
}

fn do_eval<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let expression = arguments.value(0);
    // An S4 object that extends "environment" stands for the environment in its `.xData`
    // slot. GNU R names the type of what that conversion made of a bad `enclos`, which is
    // `NULL`.
    let enclosure = arguments.value(2);
    let enclosure = match enclosure.downcast::<Environment>() {
        Some(enclosure) => Some(enclosure),
        None if enclosure.r_type() == SexpType::Nil => None,
        None => Some(
            extended_environment(context, enclosure)
                .ok_or_else(|| error("invalid 'enclos' argument of type 'NULL'"))?,
        ),
    };
    let mut envir = arguments.value(1);
    if envir.r_type() == SexpType::Object {
        envir = extended_environment(context, envir).map_or_else(handles::null, Local::value);
    }
    let environment = match envir.r_type() {
        SexpType::Environment => envir
            .downcast::<Environment>()
            .ok_or_else(|| invalid_envir(envir))?,
        SexpType::Nil => enclosure.unwrap_or_else(handles::base_environment),
        SexpType::List => handles::list_environment_in(context, envir, enclosure)?,
        SexpType::Pairlist => {
            let list = handles::pairlist_to_list(context, envir)?;
            handles::list_environment_in(context, list, enclosure)?
        }
        SexpType::Integer | SexpType::Real => {
            if handles::length(context, envir) != 1 {
                return Err(error("numeric 'envir' arg not of length one"));
            }
            let which = handles::integer_scalar(context, envir);
            if which == NA_INTEGER {
                return Err(invalid_envir(envir));
            }
            let caller = arguments.environment(context)?;
            handles::sys_frame(context, which, caller)?
        }
        _ => return Err(invalid_envir(envir)),
    };
    let caller = arguments.environment(context)?;
    handles::evaluate_in_context(
        context,
        expression,
        environment,
        (arguments.function(), caller),
    )
}

/// The environment in the `.xData` slot of an S4 object that extends "environment".
fn extended_environment<'s>(context: &Cx<'s>, object: Local<'s>) -> Option<Local<'s, Environment>> {
    if object.r_type() != SexpType::Object {
        return None;
    }
    let data = handles::get_attribute(context, object, handles::symbol(|s| s.dot_xdata));
    data.downcast::<Environment>()
        .map(|data| context.root(data))
}

fn invalid_envir(envir: Local<'_>) -> Unwind {
    error(format!(
        "invalid 'envir' argument of type '{}'",
        envir.r_type().name()
    ))
}

/// Evaluates `expression` in `env` in a context of its own, as GNU R's `eval` does: it counts
/// as a frame of `function` for `sys.call` and friends, with `sys_parent` as its parent, and a
/// `return` in the expression returns from it. An expression vector evaluates element by
/// element, and a promise is forced.
///
/// # Safety
///
/// All four are rooted.
pub(crate) unsafe fn eval_in_context(
    expression: Sexp,
    env: Sexp,
    function: Sexp,
    sys_parent: Sexp,
) -> R<Sexp> {
    let state = interp();
    let caller = state.frames.last().map_or(nil(), |frame| frame.call.raw());
    let handlers = state.handlers.clone();
    let restarts = state.restarts.clone();
    state.calls_ever = state.calls_ever.wrapping_add(1);
    state.frames.push(Frame {
        call: Held::adopt(caller),
        function: Held::adopt(function),
        env: Held::adopt(env),
        sys_parent: Held::adopt(sys_parent),
        args: state.frame_args.len() as u32,
        on_exit: ThinVec::new(),
        handlers: handlers.clone(),
        restarts: restarts.clone(),
        srcref: Held::adopt(state.srcref),
        method_vars: state.method_vars.len() as u32,
        closures: state.frame_closures.len() as u32,
        slots: crate::NO_SLOTS,
        missing: 0,
        caller: crate::NO_SLOTS,
        pending: Held::adopt(nil()),
        role: crate::FrameRole::Context,
        reified_by: crate::NOT_REIFIED,
        handle_mark: crate::heap().handle_mark() as u32,
        previous_owner: state.slot_owner,
        references: state.references_used,
    });
    let depth = state.frames.len();
    let recorded = crate::capture::evaluating(|| env);
    let result = scope(|| match expression.kind() {
        SexpType::Expression => eval_all(expression, env),
        SexpType::Promise => force_promise(expression),
        _ => eval(expression, env),
    });
    drop(recorded);
    // `return()` may come from the body or from an `on.exit` expression, which runs in the
    // same frame and so returns from it too.
    let result = handles::in_caller_scope(|context| {
        let env = handles::held(context, env);
        let result = result.map(|value| handles::adopt(context, value));
        let result = catch_return(context, env, result);
        let result = run_on_exit(context, depth, result);
        Ok(catch_return(context, env, result)?.raw())
    });
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

fn do_with_visible<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let flag = handles::logical_constant(context, i32::from(interp().visible))?;
    let mut list = context.alloc::<List>(2)?;
    list.set(context, 0, arguments.value(0));
    list.set(context, 1, flag);
    let mut names = StringFill::new(context, 2)?;
    names.push_text(context, "value")?;
    names.push_text(context, "visible")?;
    let names = names.finish().finish().value();
    let mut list = list.owned();
    handles::set_attribute(
        context,
        &mut list,
        handles::symbol(|symbols| symbols.names),
        names,
    )?;
    Ok(list.finish().value())
}

/// `Recall(...)`: calls the function that called `Recall` again, with that call's call and its
/// caller as the new frame's parent, as GNU R does. The arguments are the promises the `Recall`
/// closure itself received, unforced, so the function decides afresh whether to evaluate each
/// one, and `substitute` in it sees the code `Recall` was called with. Neither frame needs an
/// environment, so a slot frame hands out its pending one.
fn do_recall<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let Some(recalled) = arguments.recalled(context)? else {
        return Err(arguments.error("'Recall' called from outside a closure"));
    };
    handles::apply(
        context,
        recalled.call,
        recalled.function,
        &recalled.arguments,
        recalled.parent,
    )
}

fn do_primitive<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let name = arguments.value(0);
    let primitive = match handles::length(context, name) {
        1 => handles::first_string(context, name).map(|name| handles::primitive(&name)),
        _ => None,
    };
    let Some(primitive) = primitive else {
        return Err(error("invalid 'name' argument"));
    };
    let Some(primitive) = primitive else {
        return Err(arguments.error("no such primitive function"));
    };
    Ok(context.root(primitive))
}

/// A formula: the call itself, with class `formula` and the environment it was made in.
fn do_tilde<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let mut formula = handles::deep_copy(context, arguments.expression())?;
    let class = handles::symbol(|symbols| symbols.class);
    if handles::get_attribute(context, formula.get(), class).r_type() == SexpType::Nil {
        let environment = arguments.environment(context)?;
        let mut class_value = StringFill::new(context, 1)?;
        class_value.push_text(context, "formula")?;
        let class_value = class_value.finish().finish().value();
        handles::set_attribute(context, &mut formula, class, class_value)?;
        let name = handles::symbol(|symbols| symbols.dot_environment);
        handles::set_attribute(context, &mut formula, name, environment.value())?;
    }
    Ok(formula.finish().value())
}

fn do_delayed_assign<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let names = handles::dense::<Str>(context, arguments.value(0))?;
    let Some(names) = names.filter(|names| !names.is_empty()) else {
        return Err(error("invalid first argument"));
    };
    let symbol = handles::install_element(context, names, 0)?;
    let evaluation = assignment_environment(arguments.value(2), "eval.env")?;
    let assignment = assignment_environment(arguments.value(3), "assign.env")?;
    let promise = handles::promise(context, arguments.value(1), evaluation)?;
    define_variable(context, symbol, promise.value(), assignment)?;
    Ok(handles::null())
}

/// The environment argument `name` of `delayedAssign`, with GNU R's errors for another value.
fn assignment_environment<'s>(value: Local<'s>, name: &str) -> R<Local<'s, Environment>> {
    if value.r_type() == SexpType::Nil {
        return Err(error("use of NULL environment is defunct"));
    }
    value
        .downcast::<Environment>()
        .ok_or_else(|| error(format!("invalid '{name}' argument")))
}

/// `makeLazy(vars, vals, expr, eenv, aenv)`: binds each variable to a promise of `expr` with
/// `key` bound to its value, which is how lazy-load databases install their objects.
fn do_make_lazy<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let names = arguments.value(0);
    let keys = arguments.value(1);
    let expression = arguments.value(2);
    let Some(names) = handles::dense::<Str>(context, names)? else {
        if names.r_type() == SexpType::Nil {
            return Ok(handles::null());
        }
        return Err(error("invalid first argument"));
    };
    let environment = |index: usize, name: &str| {
        arguments
            .value(index)
            .downcast::<Environment>()
            .ok_or_else(|| error(format!("invalid '{name}' argument")))
    };
    let (evaluation, assignment) = (environment(3, "eval.env")?, environment(4, "assign.env")?);
    // As in GNU R, each promise's code is a copy of `expression` with its first argument
    // replaced by the variable's key, and every promise shares `eval_env`, so `PRCODE` reads
    // `lazyLoadDBfetch(c(offset, length), datafile, compressed, envhook)`.
    for index in 0..names.len() {
        context.scope_reading(|inner, outer| {
            let (names, keys) = (outer.pass(names), outer.pass(keys));
            let (evaluation, assignment) = (outer.pass(evaluation), outer.pass(assignment));
            let symbol = handles::install_element(inner, names, index)?;
            let key = crate::subset::element(inner, keys, index)?;
            let key = handles::evaluate(inner, key, evaluation)?;
            let mut code = handles::deep_copy(inner, outer.pass(expression))?;
            if !handles::set_first_argument(inner, &mut code, key) {
                return Err(arguments.error("invalid lazy-load expression"));
            }
            let promise = handles::promise(inner, code.get(), evaluation)?;
            define_variable(inner, symbol, promise.value(), assignment)
        })?;
    }
    Ok(handles::null())
}

fn do_substitute<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    let [expression, environment] = arguments.matched(context, ["expr", "env"])?;
    let expression = expression.unwrap_or_else(handles::missing_argument);
    let caller = arguments.environment(context)?;
    let environment = match environment {
        Some(environment) => handles::evaluate(context, environment, caller)?,
        None => caller.value(),
    };
    let environment = match environment.r_type() {
        SexpType::Environment => environment
            .downcast::<Environment>()
            .ok_or_else(|| arguments.error("invalid environment specified"))?,
        SexpType::List | SexpType::Pairlist => handles::list_environment(context, environment)?,
        SexpType::Nil => handles::empty_environment(),
        _ => return Err(arguments.error("invalid environment specified")),
    };
    // A bare `...` splices like an argument list, of which `substitute` keeps the first
    // element, or `NULL` when the dots are empty.
    if expression.is(handles::symbol(|symbols| symbols.dots))
        && !environment.is(handles::global_environment())
    {
        return Ok(match spliced_dots(context, environment) {
            Splice::Cells(first) => context.root(dots_code(first)),
            Splice::Nothing => handles::null(),
            Splice::Itself => expression,
        });
    }
    substitute(context, expression, environment)
}

/// Replaces the symbols in `expression` that are bound in `env` (the global environment is
/// never substituted from): promises by their code, `...` by its contents, and anything else
/// by its value.
pub fn substitute<'s>(
    context: &mut Cx<'s>,
    expression: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    let mut code = expression.read(context);
    while let Some(promise) = code.downcast::<Promise>() {
        code = promise.code();
    }
    if let Some(symbol) = code.downcast::<Symbol>() {
        let value = substituted_symbol(context, symbol, environment);
        if value.is(expression) {
            return Ok(expression);
        }
        return Ok(context.root(value));
    }
    if code.r_type() == SexpType::Language {
        let call = context.root(code);
        return substitute_list(context, call, environment);
    }
    if code.is(expression) {
        return Ok(expression);
    }
    Ok(context.root(code))
}

/// What `symbol` stands for in an expression `substitute` rebuilds in `environment`: the code
/// of a promise bound to it, the value of another binding, or the symbol itself.
fn substituted_symbol<'a>(
    context: &'a Cx<'_>,
    symbol: Gc<'a, Symbol>,
    environment: Local<'_, Environment>,
) -> Gc<'a> {
    let itself = symbol.value();
    if environment.is(handles::global_environment()) || environment.is(handles::empty_environment())
    {
        return itself;
    }
    let Some(binding) = handles::frame_binding(context, environment, symbol) else {
        return itself;
    };
    if binding.is_active() {
        return itself;
    }
    let value = binding.content();
    if let Some(promise) = value.downcast::<Promise>() {
        // A promise whose code is itself a promise, such as a replacement method's `value`,
        // stands for the innermost promise's code.
        let mut code = promise.code();
        while let Some(inner) = code.downcast::<Promise>() {
            code = inner.code();
        }
        return code;
    }
    if value.r_type() == SexpType::Dots {
        return itself;
    }
    value
}

/// What `...` splices into a call that `substitute` rebuilds in `environment`.
enum Splice<'a> {
    /// The first cell of the arguments `...` holds.
    Cells(Gc<'a>),
    /// Nothing, since `...` holds no arguments.
    Nothing,
    /// `...` itself, which the frame does not bind to arguments.
    Itself,
}

fn spliced_dots<'a>(context: &'a Cx<'_>, environment: Local<'_, Environment>) -> Splice<'a> {
    let dots = handles::symbol(|symbols| symbols.dots);
    match handles::frame_binding(context, environment, dots).map(|binding| binding.content()) {
        Some(value) if value.r_type() == SexpType::Dots => Splice::Cells(value),
        Some(value) if value.is(handles::missing_argument()) => Splice::Nothing,
        _ => Splice::Itself,
    }
}

/// The expression a cell of `...` was passed as: a promise's code, or the value itself.
fn dots_code(cell: Gc<'_>) -> Gc<'_> {
    let Some((value, _, _)) = handles::cell_fields(cell) else {
        return cell;
    };
    match value.downcast::<Promise>() {
        Some(promise) => promise.code(),
        None => value,
    }
}

/// Substitutes each element of a call, splicing `...`.
fn substitute_list<'s>(
    context: &mut Cx<'s>,
    list: Local<'s>,
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    let dots = handles::symbol(|symbols| symbols.dots);
    let splices = !environment.is(handles::global_environment());
    // GNU R reuses the spliced dots cells, so a call whose head comes from `...` becomes a
    // pairlist: `substitute(...())` is how code captures the dots unevaluated.
    let head_spliced = splices
        && matches!(handles::cell_fields(list.read(context)), Some((head, _, _)) if head.is(dots))
        && matches!(spliced_dots(context, environment), Splice::Cells(_));
    let kind = if head_spliced {
        SexpType::Pairlist
    } else {
        list.r_type()
    };
    let mut cells = Cells::new(kind);
    let mut cell = list;
    while let Some((element, tag, rest)) = handles::cell_fields(cell.read(context)) {
        let (element, tag, rest) = (context.root(element), context.root(tag), context.root(rest));
        cell = rest;
        if splices && element.is(dots) {
            let spliced: SmallVec<[(Local<'s>, Local<'s>); 4]> =
                match spliced_dots(context, environment) {
                    Splice::Cells(first) => {
                        let mut items = SmallVec::new();
                        let mut dot = first;
                        while let Some((_, tag, rest)) = handles::cell_fields(dot) {
                            items.push((context.root(tag), context.root(dots_code(dot))));
                            dot = rest;
                        }
                        items
                    }
                    Splice::Nothing => SmallVec::new(),
                    Splice::Itself => [(tag, element)].into_iter().collect(),
                };
            for (tag, code) in spliced {
                cells.push(context, tag, code)?;
            }
        } else {
            let value = substitute(context, element, environment)?;
            cells.push(context, tag, value)?;
        }
    }
    let attributes = list.attributes(context);
    if attributes.r_type() != SexpType::Nil {
        let attributes = context.root(attributes);
        cells.set_first_attributes(context, attributes);
    }
    Ok(cells
        .finish()
        .map_or_else(handles::null, |result| result.finish().value()))
}

// Assignment.

fn do_assign<'s>(context: &mut Cx<'s>, arguments: &handles::Args<'_, 's>) -> R<Local<'s>> {
    if arguments.len() != 2 {
        return Err(arguments.error("invalid assignment"));
    }
    let whole = arguments.expression();
    let superassign = whole
        .downcast::<Language>()
        .is_some_and(|call| call.read(context).head().is(handles::symbol(|s| s.assign2)));
    let mut target = arguments.value(0);
    if let Some(name) = handles::dense::<Str>(context, target)?
        && !name.is_empty()
    {
        target = handles::install_element(context, name, 0)?.value();
    }
    let assigned = target.downcast::<Symbol>();
    if assigned.is_none() && target.r_type() != SexpType::Language {
        return Err(arguments.error("invalid (do_set) left-hand side to assignment"));
    }
    let environment = arguments.environment(context)?;
    let value = match superassign {
        false => handles::rebind(context, target, arguments.value(1), environment)?,
        true => handles::evaluate(context, arguments.value(1), environment)?,
    };
    match assigned {
        Some(symbol) => assign_variable(context, symbol, value, environment, superassign)?,
        None => complex_assign(context, whole, target, value, environment, superassign, &[])?,
    }
    Ok(value)
}

/// Evaluates `value`, the right-hand side of `target <- value`. When it calls a function with
/// `target` itself as the first argument, as in `d <- Dict.insert(d, k, v)` or
/// `state$seen <- Set.insert(state$seen, k)`, the call may update the value in place (see
/// [`rebind_call`]).
pub(crate) unsafe fn rebind(target: Sexp, value: Sexp, env: Sexp) -> R<Sexp> {
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
        frame.owns(env)
            && frame.function.raw().kind() == SexpType::Closure
            && bytecode::is_compiled(frame.function.raw())
    }) || !bytecode::registers_hold(value)
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

/// `symbol <- value` in `environment`, or `symbol <<- value` from it.
fn assign_variable<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'s, Symbol>,
    value: Local<'s>,
    environment: Local<'s, Environment>,
    superassign: bool,
) -> R<()> {
    if superassign {
        let enclosure = enclosure_of(context, environment);
        return assign_outward(context, symbol, value, enclosure);
    }
    let binding =
        handles::frame_binding(context, environment, symbol).map(|cell| context.root(cell));
    handles::assign_local(context, symbol, value, environment, binding)
}

/// Where `<<-` from `environment` starts: its enclosure, or the empty environment itself, whose
/// enclosure is `NULL`.
fn enclosure_of<'s>(
    context: &Cx<'s>,
    environment: Local<'s, Environment>,
) -> Local<'s, Environment> {
    match environment.read(context).enclosure() {
        Some(enclosure) => context.root(enclosure),
        None => handles::empty_environment(),
    }
}

/// `f(x, ...) <- value`: evaluates the getter chain down to the variable, then applies the
/// replacement functions from the outside in and assigns the result to the variable.
///
/// `evaluated` is empty, or holds the arguments after `x` of a one-level target that compiled
/// code has already evaluated, each a forced promise whose code is the argument's (or that code
/// itself, for one the replacement must evaluate), so the replacement evaluates none of them a
/// second time.
pub fn complex_assign<'s>(
    context: &mut Cx<'s>,
    whole: Local<'s>,
    target: Local<'s>,
    value: Local<'s>,
    environment: Local<'s, Environment>,
    superassign: bool,
    evaluated: &[Local<'s>],
) -> R<()> {
    let mut getters: SmallVec<[Local<'s, Language>; 4]> = SmallVec::new();
    let mut inner = target;
    while let Some(getter) = inner.downcast::<Language>() {
        getters.push(getter);
        inner = match handles::cell_fields(getter.read(context).rest()) {
            Some((first, _, _)) => context.root(first),
            None => handles::null(),
        };
    }
    if let Some(name) = handles::dense::<Str>(context, inner)?
        && name.len() == 1
    {
        inner = handles::install_element(context, name, 0)?.value();
    }
    let Some(inner) = inner.downcast::<Symbol>() else {
        return Err(handles::error_in(
            whole,
            "target of assignment expands to non-language object",
        ));
    };
    let lookup_environment = match superassign {
        true => enclosure_of(context, environment),
        false => environment,
    };
    let Some(mut base) = variable(context, inner, lookup_environment)? else {
        // GNU R evaluates the name in the enclosure and names the call whose frame that is, or
        // no call when the enclosure is not a frame's.
        let call = handles::owning_call(context, enclosure_of(context, environment));
        let name = handles::text(inner.read(context).name());
        return Err(handles::error_in(
            call,
            format!("object '{name}' not found"),
        ));
    };
    if handles::is_missing_argument(base) {
        // GNU R reads the name in the frame when the frame binds it and in the enclosure
        // otherwise, and the error names the call of the environment the read started in.
        let reader = if superassign || handles::frame_binding(context, environment, inner).is_none()
        {
            enclosure_of(context, environment)
        } else {
            environment
        };
        let call = handles::lexical_call(context, reader);
        let name = handles::text(inner.read(context).name()).into_owned();
        return Err(missing_argument(call.raw(), &name, ArgumentReader::Eval));
    }
    // A promise that another reference also holds, as the caller's `...` list does for an
    // argument passed on through `...`, shares its value with that holder. GNU R wraps each such
    // argument in a promise of its own, whose value then counts a second reference.
    let shared_promise = base.r_type() == SexpType::Promise && base.is_shared();
    if base.r_type() == SexpType::Promise {
        base = handles::force(context, base)?;
    }
    let local_binding =
        handles::frame_binding(context, environment, inner).map(|cell| context.root(cell));
    // An index that assigns, as in `x[{x[2] <- 3; 1}] <- 2`, rebinds the variable while the
    // replacement still holds the old value, which therefore must be a copy. The `*tmp*`
    // promise deliberately counts no reference, so the inner assignment cannot see the share.
    let index_assigns = getters.iter().any(|getter| {
        handles::cell_fields(getter.read(context).rest()).is_some_and(|(_, _, indices)| {
            any_item(indices, |argument| {
                assigns_to(argument, inner.read(context))
            })
        })
    });
    let found = enclosing_binding(
        context,
        inner.read(context),
        lookup_environment.read(context),
    )
    .map(|(holder, cell)| (context.root(holder), context.root(cell)));
    // A locked binding rejects the final assignment, so the replacement must not have modified
    // its value in place by then. `<<-` writes the binding it found, `<-` the local one.
    let written = if superassign {
        found.map(|(_, cell)| cell)
    } else {
        local_binding
    };
    let locked = written.is_some_and(|cell| cell.read(context).is_locked());
    let holder = found.map_or_else(handles::empty_environment, |(holder, _)| holder);
    // Ending a move into this frame hands the caller `base` back, which makes it shared, and
    // must come before any level changes in place, since an inner level is an element of it.
    handles::modifying(context, base)?;
    // The value being the variable's own, as in `x$a$b <- x`, counts no second reference.
    if base.is_shared()
        || base.is(value)
        || shared_promise
        || locked
        || index_assigns
        || (!superassign && local_binding.is_none())
        || !handles::reuse_permitted(context, holder, base)
    {
        // As in GNU R, a shared local variable is copied under a wrapper, which defers copying
        // the data until the replacement writes to it; a replacement that only changes
        // attributes, or writes nothing, as `x[FALSE] <- 1` does, never copies it.
        let how = if !superassign && local_binding.is_some() && base.is_shared() {
            AssignmentCopy::Replacement
        } else {
            AssignmentCopy::Storage
        };
        base = handles::assignment_copy(context, base, how)?;
    }
    // GNU R binds `*tmp*` in this frame for the length of the assignment, and in a hashed frame
    // that moves the table's chain counter, which `ls(sorted = FALSE)` order depends on.
    handles::count_tmp_binding(context, environment);
    let tmp_value = handles::bind_tmp_value(context, Some(environment), base);
    let assignment = Assignment {
        whole,
        getters: &getters,
        base,
        value,
        environment,
        superassign,
        // A profile summary counts the assignment once, when its first level builds a call.
        counted: Cell::new(!activity::enabled()),
    };
    let replaced = replace_levels(context, &assignment, evaluated);
    handles::unbind_tmp_value(tmp_value);
    let mut replaced = replaced?;
    if !superassign && handles::is_altrep(replaced) {
        let bound = handles::frame_binding(context, environment, inner)
            .is_some_and(|cell| cell.content().is(replaced) && !cell.is_active());
        replaced = handles::unwrap_assigned(context, replaced, bound);
    }
    assign_variable(context, inner, replaced, environment, superassign)
}

/// What every level of one complex assignment reads.
struct Assignment<'a, 's> {
    whole: Local<'s>,
    /// The target's calls, outermost first.
    getters: &'a [Local<'s, Language>],
    /// The variable's value, which the assignment may change in place.
    base: Local<'s>,
    value: Local<'s>,
    environment: Local<'s, Environment>,
    superassign: bool,
    counted: Cell<bool>,
}

/// The variable's new value: each getter applied to the level below, innermost first, then
/// each replacement function from the outside in.
fn replace_levels<'s>(
    context: &mut Cx<'s>,
    assignment: &Assignment<'_, 's>,
    evaluated: &[Local<'s>],
) -> R<Local<'s>> {
    let Assignment {
        whole,
        getters,
        base,
        value,
        environment,
        ..
    } = *assignment;
    let tmp = handles::permanent_symbol(|s| s.tmp).value();
    // Values of each level, innermost first: the variable, then each getter applied to it.
    let mut levels: SmallVec<[Local<'s>; 4]> = smallvec![base];
    let mut shared = false;
    for getter in getters.iter().skip(1).rev() {
        count_assignment(context, assignment)?;
        let previous = levels.last().copied().unwrap_or(base);
        let promise = handles::uncounted_forced_promise(context, tmp, previous)?;
        // The call names `*tmp*`, as GNU R's does, while the argument it is evaluated with is
        // the promise holding the level below.
        let getter_cell = getter.read(context);
        let head = context.root(getter_cell.head());
        let (tag, rest) = match handles::cell_fields(getter_cell.rest()) {
            Some((_, tag, rest)) => (context.root(tag), context.root(rest)),
            None => (handles::null(), handles::null()),
        };
        let first = handles::cons(context, tag, tmp, rest)?;
        let getter_call = handles::language(context, head, first)?;
        let arguments = handles::cons(context, tag, promise.value(), rest)?;
        let function = head_function(context, head, environment)?;
        let mut level = handles::apply_call(
            context,
            getter_call.value(),
            function,
            arguments,
            environment,
        )?;
        if level.is_shared() || shared || level.is(value) {
            shared = true;
            level = handles::assignment_copy(context, level, AssignmentCopy::Storage)?;
        }
        levels.push(level);
    }
    let right_hand_side = handles::cell_fields(whole.read(context))
        .and_then(|(_, _, arguments)| handles::cell_fields(arguments))
        .and_then(|(_, _, rest)| handles::cell_fields(rest))
        .map_or(value, |(expression, _, _)| context.root(expression));
    let value_symbol = handles::permanent_symbol(|s| s.value).value();
    let mut replacement = value;
    for (depth, getter) in getters.iter().enumerate() {
        let object = levels[levels.len() - 1 - depth];
        let head = context.root(getter.read(context).head());
        let setter = replacement_function(context, head, whole)?;
        let function = head_function(context, setter, environment)?;
        // A closure reports its errors against its own call, `` `f<-`(`*tmp*`, value = v) ``,
        // as GNU R's does, so only a primitive replacement names the assignment. A level
        // past the first reports as GNU R does, against the assignment of that level's
        // target, such as `` names(x) <- `*vtmp*` `` in `names(x)[2] <- "b"`.
        let primitive = function.r_type() != SexpType::Closure;
        let reported = if !primitive || depth == 0 {
            whole
        } else {
            let assign = context.root(whole.downcast::<Language>().map_or_else(
                || handles::null().read(context),
                |whole| whole.read(context).head(),
            ));
            let untagged = |value| Tagged {
                tag: handles::null(),
                value,
            };
            let items = [
                untagged(getter.value()),
                untagged(handles::permanent_symbol(|s| s.vtmp).value()),
            ];
            let arguments = handles::chain(context, &items, handles::null())?;
            handles::language(context, assign, arguments)?.value()
        };
        if primitive {
            // The level's target stands for the replacement call, which only an error that
            // names it builds (`generated_replacement_call`).
            let published = AssignmentCall::enter(context, getter.value(), reported);
            let level_arguments = if depth == 0 { evaluated } else { &[] };
            // An inner level belongs to the variable's value, which the level's own arguments
            // may share, as `names(x)[{ keep <- x; 2 }]` does after the getters ran.
            let owner = (depth + 1 < levels.len()).then_some(base);
            let result = replace_through_primitive(
                context,
                function,
                *getter,
                (object, replacement),
                (reported, owner),
                environment,
                level_arguments,
            )?;
            drop(published);
            if let Some(result) = result {
                replacement = result;
                continue;
            }
        }
        count_assignment(context, assignment)?;
        // The object promise is the `*tmp*` GNU R binds: it must not count as a reference, or
        // the replacement function would always see its target as shared and copy it.
        let object_promise = handles::uncounted_forced_promise(context, tmp, object)?;
        // The outermost replacement's value promise carries the right-hand side's expression,
        // which `substitute(value)` returns; an inner one carries the value itself, as in GNU R.
        let value_code = if depth == 0 {
            right_hand_side
        } else {
            replacement
        };
        // GNU R's value promise does not count either, so a replacement that updated its target
        // in place stays uniquely referenced for the next update.
        let value_promise = handles::uncounted_forced_promise(context, value_code, replacement)?;
        // The call `` `f<-`(`*tmp*`, ..., value = v) `` and the argument list it is applied
        // with differ only in their first argument, so they share the cells after it, unless
        // the arguments are already evaluated: then the call keeps their code.
        let mut items: SmallVec<[Tagged<'s>; 6]> = SmallVec::new();
        items.push(Tagged {
            tag: handles::null(),
            value: tmp,
        });
        let mut rest = handles::cell_fields(getter.read(context).rest())
            .map_or_else(|| handles::null().read(context), |(_, _, rest)| rest);
        while let Some((index, tag, next)) = handles::cell_fields(rest) {
            items.push(Tagged {
                tag: context.root(tag),
                value: context.root(index),
            });
            rest = next;
        }
        let value_item = Tagged {
            tag: value_symbol,
            value: value_promise.value(),
        };
        items.push(value_item);
        let setter_call = handles::chain(context, &items, handles::null())?;
        let indices = &items[1..items.len() - 1];
        // The arguments after `*tmp*` are the call's own cells.
        let tail = handles::cell_fields(setter_call.read(context))
            .map_or_else(handles::null, |(_, _, tail)| context.root(tail));
        let setter_call = handles::language(context, setter, setter_call)?;
        let argument_tail =
            if depth == 0 && !evaluated.is_empty() && evaluated.len() == indices.len() {
                let mut arguments: SmallVec<[Tagged<'s>; 6]> = indices
                    .iter()
                    .zip(evaluated)
                    .map(|(index, &value)| Tagged {
                        tag: index.tag,
                        value,
                    })
                    .collect();
                arguments.push(value_item);
                handles::chain(context, &arguments, handles::null())?
            } else {
                tail
            };
        let arguments = handles::cons(
            context,
            handles::null(),
            object_promise.value(),
            argument_tail,
        )?;
        if !primitive {
            replacement = handles::apply_call(
                context,
                setter_call.value(),
                function,
                arguments,
                environment,
            )?;
            continue;
        }
        let published = AssignmentCall::enter(context, setter_call.value(), reported);
        replacement = handles::apply_call(
            context,
            setter_call.value(),
            function,
            arguments,
            environment,
        )?;
        drop(published);
    }
    Ok(replacement)
}

/// Counts the assignment in a profile summary, once.
fn count_assignment(context: &mut Cx<'_>, assignment: &Assignment<'_, '_>) -> R<()> {
    if !assignment.counted.replace(true) {
        let shape = assignment_shape(context, assignment)?;
        activity::count_general_assignment(activity::label(&shape));
    }
    Ok(())
}

/// One level of a complex assignment through the replacement primitive `function` itself:
/// `object` and `value` go to the primitive as they are, with the level's other arguments
/// evaluated between them, so the level builds no `*tmp*` and value promises and no argument
/// list. `evaluated` is [`complex_assign`]'s, for the outermost level. `None` leaves the level
/// to the call `complex_assign` builds: an `object` with a method for the replacement, which
/// receives the promises, and a special that reads its arguments' code.
///
/// The primitive runs with `getter` as its call, which the caller has published as the
/// replacement call of the assignment in progress ([`crate::Interp::assignment_call`]), so its
/// errors report against `reported`. `owner` is the variable's value when `object` is an inner
/// level of it.
fn replace_through_primitive<'s>(
    context: &mut Cx<'s>,
    function: Local<'s>,
    getter: Local<'s, Language>,
    (object, value): (Local<'s>, Local<'s>),
    (reported, owner): (Local<'s>, Option<Local<'s>>),
    environment: Local<'s, Environment>,
    evaluated: &[Local<'s>],
) -> R<Option<Local<'s>>> {
    let definition = handles::builtin_definition(function)?;
    let special = function.r_type() == SexpType::Special;
    let dollar = definition.symbol == Some(symbols().dollar_assign);
    // Of the specials, `[<-` and `[[<-` evaluate every argument, and `$<-` all but the name.
    if special && !dollar && !definition.is_dispatch_or_eval() {
        return Ok(None);
    }
    // A group generic dispatches on any operand, the value included.
    let internal = special || definition.generic == Generic::Internal;
    if !internal && definition.generic != Generic::None {
        return Ok(None);
    }
    if internal
        && handles::has_internal_method(context, definition, function, object, Some(environment))?
    {
        return Ok(None);
    }
    let rest = handles::cell_fields(getter.read(context).rest())
        .map_or_else(handles::null, |(_, _, rest)| context.root(rest));
    if dollar {
        let named = handles::cell_fields(rest.read(context)).and_then(|(name, tag, after)| {
            let plain = after.is(handles::null()) && tag.is(handles::null());
            let named = match name.r_type() {
                SexpType::Symbol => !handles::is_missing_argument(context.root(name)),
                SexpType::String => !name.standard::<Str>().is_some_and(|name| name.is_empty()),
                _ => false,
            };
            (plain && named).then(|| context.root(name))
        });
        let Some(name) = named else {
            return Ok(None);
        };
        let observed = activity::hooks() != 0;
        let index = builtin_index(definition);
        let _activity = observed.then(|| activity::enter(Activity::Builtin, index as u32));
        if activity::enabled() {
            activity::record_builtin(index);
        }
        if observed && activity::profile_ticked() {
            handles::record_profile_sample(context, definition.name)?;
        }
        // An error is signalled here, as a builtin call signals its own, while the assignment
        // is the call it reports.
        let result = crate::subset::argument_name(context, name)
            .and_then(|name| {
                crate::subset::assign_dollar(context, object, name, value, getter.value())
            })
            .and_then(|result| crate::unwind::raise_parked().map(|()| result))
            .map_err(|unwind| handles::builtin_failure(context, unwind))?;
        return Ok(Some(result));
    }
    let keep_missing = special || definition.keep_missing;
    let mut arguments: SmallVec<[Tagged<'s>; 8]> = smallvec![Tagged {
        tag: handles::null(),
        value: object,
    }];
    let mut count = 0;
    let mut cell = rest.read(context);
    while let Some((_, _, next)) = handles::cell_fields(cell) {
        count += 1;
        cell = next;
    }
    if evaluated.is_empty() || evaluated.len() != count {
        handles::evaluate_arguments(
            context,
            &mut arguments,
            (rest, reported),
            environment,
            keep_missing,
        )?;
    } else {
        // Declining after a variable was read would read it a second time in the built call.
        let dots = handles::symbol(|s| s.dots);
        let unevaluated = evaluated.iter().any(|argument| match argument.r_type() {
            SexpType::Symbol => {
                argument.is(dots) || (handles::is_missing_argument(*argument) && !keep_missing)
            }
            SexpType::Language | SexpType::Bytecode => true,
            _ => false,
        });
        if unevaluated {
            return Ok(None);
        }
        let mut cell = rest;
        for &argument in evaluated {
            let Some((_, tag, next)) = handles::cell_fields(cell.read(context)) else {
                break;
            };
            let (tag, next) = (context.root(tag), context.root(next));
            cell = next;
            // A forced promise holds the value compiled code evaluated. Anything else is the
            // argument's code: a constant, an empty argument, or a formal left for this lookup.
            let value = match argument.downcast::<Symbol>() {
                _ if argument.r_type() == SexpType::Promise => handles::force(context, argument)?,
                Some(_) if handles::is_missing_argument(argument) => argument,
                Some(symbol)
                    if keep_missing
                        && expression_is_missing(
                            context,
                            symbol.read(context).value(),
                            environment.read(context),
                        )? =>
                {
                    handles::missing_argument()
                }
                Some(symbol) => symbol_value(context, symbol, environment)?,
                None => argument,
            };
            arguments.push(Tagged { tag, value });
        }
    }
    // GNU R's getter marks an attribute it returns as not mutable, so the level is a copy by
    // the time it is written. Here it is copied only when its owner became shared meanwhile.
    if owner.is_some_and(|owner| owner.is_shared()) && arguments[0].value.is(object) {
        arguments[0].value = handles::assignment_copy(context, object, AssignmentCopy::Storage)?;
    }
    arguments.push(Tagged {
        tag: handles::permanent_symbol(|s| s.value).value(),
        value,
    });
    handles::call_builtin_tagged(
        context,
        definition,
        (getter.value(), function),
        &arguments,
        Some(environment),
    )
    .map(Some)
}

/// The call GNU R generates for one level of a complex assignment, `` `f<-`(`*tmp*`, i, value = v) ``,
/// from the level's target `f(x, i)` and the assignment `f(x, i) <- v` it reports as.
///
/// # Safety
///
/// Both are live, and stay so through an allocation.
pub unsafe fn generated_replacement_call(target: Sexp, assignment: Sexp) -> R<Sexp> {
    handles::with_result(|context| {
        let target = handles::held(context, target);
        let assignment = handles::held(context, assignment);
        Ok(replacement_call_of(context, target, assignment)?.raw())
    })
}

/// [`generated_replacement_call`] on handles.
fn replacement_call_of<'s>(
    context: &mut Cx<'s>,
    target: Local<'s>,
    assignment: Local<'s>,
) -> R<Local<'s>> {
    let Some((head, _, arguments)) = handles::cell_fields(target.read(context)) else {
        return Err(error(
            "internal error: a replacement target that is not a call",
        ));
    };
    let head = context.root(head);
    let arguments = context.root(arguments);
    let setter = replacement_function(context, head, assignment)?;
    let mut cells = Cells::new(SexpType::Language);
    cells.push(context, handles::null(), setter)?;
    cells.push(
        context,
        handles::null(),
        handles::permanent_symbol(|s| s.tmp).value(),
    )?;
    let mut indices: SmallVec<[Tagged<'s>; 4]> = SmallVec::new();
    let mut rest = handles::cell_fields(arguments.read(context))
        .map_or_else(|| handles::null().read(context), |(_, _, rest)| rest);
    while let Some((index, tag, next)) = handles::cell_fields(rest) {
        indices.push(Tagged {
            tag: context.root(tag),
            value: context.root(index),
        });
        rest = next;
    }
    for index in indices {
        cells.push(context, index.tag, index.value)?;
    }
    let value = handles::cell_fields(assignment.read(context))
        .and_then(|(_, _, arguments)| handles::cell_fields(arguments))
        .and_then(|(_, _, rest)| handles::cell_fields(rest))
        .map_or_else(handles::null, |(value, _, _)| context.root(value));
    cells.push(
        context,
        handles::permanent_symbol(|s| s.value).value(),
        value,
    )?;
    Ok(cells.finish().map_or_else(handles::null, Owned::finish))
}

/// A profile summary's name for a complex assignment's target: the replacement functions from
/// the outside in, then the variable's type, as in `` `names<-` `[<-` on character ``, whether
/// the variable's class has a method for its replacement, and whether the frame runs a body
/// that is not compiled.
fn assignment_shape(context: &mut Cx<'_>, assignment: &Assignment<'_, '_>) -> R<String> {
    let mut shape = String::new();
    for getter in assignment.getters {
        let head = getter
            .read(context)
            .head()
            .downcast::<Symbol>()
            .map(|head| handles::text(head.name()).into_owned());
        // A summary is shared, so only a replacement function base R defines is named.
        let mut name = None;
        if let Some(head) = &head {
            let setter = format!("{head}<-");
            if handles::primitive(&setter).is_some() {
                name = Some(setter);
            } else {
                let symbol = handles::install_bytes(context, setter.as_bytes())?;
                if handles::frame_binding(context, handles::base_namespace(), symbol).is_some() {
                    name = Some(setter);
                }
            }
        }
        match name {
            Some(name) => shape.push_str(&format!("`{name}` ")),
            None if head.is_some() => shape.push_str("(a function) "),
            None => shape.push_str("(computed) "),
        }
    }
    shape.push_str(if assignment.superassign {
        "by `<<-` on "
    } else {
        "on "
    });
    // Nor is a class a user's code defines.
    const PUBLIC_CLASSES: [&str; 12] = [
        "data.frame",
        "data.table",
        "factor",
        "Date",
        "POSIXct",
        "POSIXlt",
        "difftime",
        "xts",
        "zoo",
        "tbl_df",
        "R6",
        "formula",
    ];
    let base = assignment.base;
    shape.push_str(handles::type_name(base));
    if handles::is_object(base) {
        let class = handles::get_attribute(context, base, handles::symbol(|s| s.class));
        let known = class.standard::<Str>().and_then(|class| {
            (0..class.len()).find_map(|index| {
                let name = handles::text(class.get(index));
                PUBLIC_CLASSES.iter().find(|public| **public == name)
            })
        });
        shape.push_str(&format!(" ({})", known.unwrap_or(&"an object")));
        let innermost = assignment
            .getters
            .last()
            .and_then(|getter| getter.read(context).head().downcast::<Symbol>())
            .map(|head| format!("{}<-", handles::text(head.name())));
        if let Some(function) = innermost.and_then(|setter| handles::primitive(&setter)) {
            let definition = handles::builtin_definition(function)?;
            if (definition.generic == Generic::Internal || function.r_type() == SexpType::Special)
                && handles::has_internal_method(
                    context,
                    definition,
                    function,
                    base,
                    Some(assignment.environment),
                )?
            {
                shape.push_str(" with a method");
            }
        }
    }
    if !handles::is_compiled_frame(context, assignment.environment) {
        shape.push_str(", uncompiled");
    }
    Ok(shape)
}

/// Whether `expression` contains an assignment whose target is `variable` or a part of it. An
/// assignment to another variable cannot rebind `variable`, so it needs no defensive copy.
pub(crate) fn assigns_to(expression: Gc<'_>, variable: Gc<'_, Symbol>) -> bool {
    let Some(call) = expression.downcast::<Language>() else {
        return false;
    };
    let head = call.head();
    let assignment = head.is(handles::symbol(|s| s.assign))
        || head.is(handles::symbol(|s| s.assign2))
        || head.is(handles::symbol(|s| s.assign_eq));
    if assignment {
        let mut target = handles::cell_fields(call.rest()).map(|(target, _, _)| target);
        while let Some(getter) = target.and_then(|target| target.downcast::<Language>()) {
            target = handles::cell_fields(getter.rest()).map(|(target, _, _)| target);
        }
        let named = target.is_some_and(|target| {
            target.is(variable)
                || target.standard::<Str>().is_some_and(|name| {
                    name.len() == 1 && name.get(0).bytes() == variable.name().bytes()
                })
        });
        if named {
            return true;
        }
    }
    any_item(expression, |part| assigns_to(part, variable))
}

/// [`complex_assign`] for compiled code, whose frame holds every operand.
///
/// # Safety
///
/// The caller keeps every operand alive until this returns, and releases what this roots.
pub unsafe fn compiled_complex_assign(
    whole: Sexp,
    target: Sexp,
    value: Sexp,
    env: Sexp,
    superassign: bool,
    evaluated: &[Sexp],
) -> R<()> {
    // SAFETY: the caller's contract.
    unsafe {
        handles::in_caller_scope(|context| {
            let whole = handles::held(context, whole);
            let target = handles::held(context, target);
            let value = handles::held(context, value);
            let environment = handles::held(context, env)
                .downcast::<Environment>()
                .ok_or_else(|| error("internal error: an assignment outside an environment"))?;
            let evaluated: SmallVec<[Local<'_>; 4]> = evaluated
                .iter()
                .map(|&argument| handles::held(context, argument))
                .collect();
            complex_assign(
                context,
                whole,
                target,
                value,
                environment,
                superassign,
                &evaluated,
            )
        })
    }
}

/// Whether `test` holds for any value of the pairlist or call `list`.
fn any_item<'a>(mut list: Gc<'a>, mut test: impl FnMut(Gc<'a>) -> bool) -> bool {
    while let Some((item, _, rest)) = handles::cell_fields(list) {
        if test(item) {
            return true;
        }
        list = rest;
    }
    false
}

/// The replacement function for a getter: `f<-` for `f`, and `pkg::\`f<-\`` for `pkg::f`.
fn replacement_function<'s>(
    context: &mut Cx<'s>,
    getter: Local<'s>,
    whole: Local<'s>,
) -> R<Local<'s>> {
    if let Some(name) = getter.downcast::<Symbol>() {
        return Ok(handles::replacement_symbol(context, name)?.value());
    }
    let qualified = handles::cell_fields(getter.read(context)).and_then(|(head, _, rest)| {
        let colons = head.is(handles::symbol(|s| s.double_colon))
            || head.is(handles::symbol(|s| s.triple_colon));
        let (package, _, rest) = handles::cell_fields(rest)?;
        let (function, _, end) = handles::cell_fields(rest)?;
        let function = function.downcast::<Symbol>()?;
        (colons && end.is(handles::null())).then(|| {
            (
                context.root(head),
                context.root(package),
                context.root(function),
            )
        })
    });
    let Some((head, package, function)) = qualified else {
        return Err(handles::error_in(
            whole,
            "invalid function in complex assignment",
        ));
    };
    let mut name = function.read(context).name().bytes().to_vec();
    name.extend_from_slice(b"<-");
    let symbol = handles::install_bytes(context, &name)?;
    let mut cells = Cells::new(SexpType::Language);
    cells.push(context, handles::null(), head)?;
    cells.push(context, handles::null(), package)?;
    let symbol = context.root(symbol);
    cells.push(context, handles::null(), symbol.value())?;
    Ok(cells.finish().map_or_else(handles::null, Owned::finish))
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

/// The most arguments [`with_positional_args`] keeps on the stack.
pub const INLINE_POSITIONAL_ARGS: usize = 8;

/// Runs `body` on `count` untagged arguments, the `index`th of which is `value(index)`, called in
/// order. Up to [`INLINE_POSITIONAL_ARGS`] of them are written straight into an uninitialized
/// stack array, because collecting into [`Args`] costs about sixty instructions a call in its
/// capacity checks alone. The caller keeps every value alive until `body` returns.
#[inline(always)]
pub fn with_positional_args<T>(
    count: usize,
    mut value: impl FnMut(usize) -> R<Sexp>,
    body: impl FnOnce(&[Arg]) -> R<T>,
) -> R<T> {
    if count > INLINE_POSITIONAL_ARGS {
        let args = (0..count)
            .map(|index| {
                Ok(Arg {
                    tag: nil(),
                    value: value(index)?,
                })
            })
            .collect::<R<Args>>()?;
        return body(&args);
    }
    let mut buffer = [const { MaybeUninit::<Arg>::uninit() }; INLINE_POSITIONAL_ARGS];
    for (index, slot) in buffer.iter_mut().take(count).enumerate() {
        slot.write(Arg {
            tag: nil(),
            value: value(index)?,
        });
    }
    // SAFETY: the loop above wrote the first `count` slots, and `count` fits the buffer.
    body(unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<Arg>(), count) })
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
