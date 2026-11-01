//! The C API's evaluation entry points (`Rf_eval`, `R_tryEval`, ...) and the interpreter's link to
//! native code. The interpreter (`crates/interp`) owns evaluation; this module installs it on the
//! runtime's heap and registers the builtins that need this crate: `.Call`, `.External`, `.C`,
//! `.Fortran` and the `dyn.load` family.

use {
    crate::{
        Rboolean, Runtime,
        boundary::{
            ApiResult, DlFunc, Unwind, call_native, enter, error, error_in, report,
            with_native_scope,
        },
        dynload::{Dll, RoutineKind, find_routine},
        globals::R_NamesSymbol,
        runtime,
        strings::utf8_text,
        vectors::{format_real, xlength},
    },
    heap::{
        object::{NA_INTEGER, Sexp, SexpType},
        strings::Encoding,
    },
    interp::{
        TaskCallbackRoutine, builtin_libraries,
        builtins::{Arg, BuiltinDef, Call, builtin, internal},
        graphics_engine,
        native::Interface,
    },
    std::ffi::{c_char, c_int, c_void},
};

/// Runs a graphics callback inside a C context, so that an R error raised from C code in it
/// unwinds to here instead of past Rust frames.
unsafe fn guard_native(body: &mut dyn FnMut()) -> ApiResult<()> {
    unsafe extern "C" fn trampoline(data: *mut c_void) {
        // SAFETY: `data` is the `&mut &mut dyn FnMut()` passed below, alive for the call.
        let body = unsafe { &mut *(data as *mut &mut dyn FnMut()) };
        body();
    }
    let mut body = body;
    crate::boundary::call_void(trampoline, (&mut body as *mut &mut dyn FnMut()).cast())
}

/// Installs the interpreter on the runtime's heap. The runtime must be at its final address.
pub(crate) unsafe fn install_interpreter(runtime: &mut Runtime) {
    let hooks = interp::Hooks {
        write_console: |text, is_error| crate::boundary::write_console(text, is_error),
        materialize_altrep: |object| unsafe { crate::altrep::materialize(object) },
        run_finalizers: || unsafe { crate::boundary::run_pending_finalizers() },
        run_task_callbacks: |expression, value, succeeded, visible| unsafe {
            run_task_callbacks(expression, value, succeeded, visible)
        },
        task_callback_routine: |routine, args| unsafe { task_callback_routine(routine, args) },
        connection_method: |con, call| unsafe { crate::connections::call_method(con, call) },
        enter_from_native: |body| unsafe { crate::boundary::enter(|| body()) },
        guard_native: |body| unsafe { guard_native(body) },
        connection_vfprintf: Some(crate::connections::default_vfprintf()),
    };
    interp::install(&mut runtime.heap, hooks);
    jit::install();
    interp::register_builtins(NATIVE_BUILTINS);
    interp::register_builtins(BOOTSTRAP_BUILTINS);
    interp::register_builtins(loader::ry_runtime::BUILTINS);
    interp::register_builtins(crate::serialize::BUILTINS);
    install_evaluator(&Interpreter);
}

/// ry's interpreter behind the [`Evaluator`] hook.
struct Interpreter;

impl Evaluator for Interpreter {
    unsafe fn eval(&self, expression: Sexp, env: Sexp) -> ApiResult<Sexp> {
        eval(expression, env)
    }

    unsafe fn apply(
        &self,
        call: Sexp,
        function: Sexp,
        arguments: Sexp,
        env: Sexp,
    ) -> ApiResult<Sexp> {
        let args: Vec<Arg> = interp::values::pairlist_items(arguments)
            .into_iter()
            .map(|(tag, value)| Arg { tag, value })
            .collect();
        interp::eval::apply(call, function, &args, env)
    }

    unsafe fn parse(&self, text: Sexp, count: c_int, srcfile: Sexp) -> ApiResult<ParseOutcome> {
        let lines: Vec<String> = interp::values::strings(text)
            .into_iter()
            .map(|line| line.unwrap_or_else(|| "NA".into()))
            .collect();
        let Ok(expressions) =
            interp::lower::parse_with_srcfile(&lines.join("\n"), "<text>", srcfile)
        else {
            return Ok(ParseOutcome {
                expressions: runtime().nil(),
                status: ParseStatus::Error,
            });
        };
        let expressions = interp::lower::truncate_parse(expressions, count)?;
        Ok(ParseOutcome {
            expressions,
            status: ParseStatus::Ok,
        })
    }

    unsafe fn force_and_call(&self, call: Sexp, count: c_int, env: Sexp) -> ApiResult<Sexp> {
        normalize_argument_cells(call)?;
        let function = call.car();
        let function = if function.kind() == SexpType::Symbol {
            interp::eval::find_fun(function, env)?
        } else {
            interp::eval::eval(function, env)?
        };
        let args = interp::eval::promise_args(call.cdr(), env)?;
        for arg in args.iter().take(count.max(0) as usize) {
            if arg.value.kind() == SexpType::Promise {
                interp::eval::force_promise(arg.value)?;
            }
        }
        interp::eval::apply(call, function, &args, env)
    }

    unsafe fn print_value(&self, value: Sexp, env: Sexp) -> ApiResult<()> {
        interp::print::print_value_top(value, env)
    }

    unsafe fn current_env(&self) -> Sexp {
        interp::interp()
            .frames
            .last()
            .map_or(runtime().heap.known().global_env, |frame| frame.env)
    }

    unsafe fn current_srcref(&self, skip: c_int) -> ApiResult<Sexp> {
        // The current srcref, then the one each frame recorded when it was called, innermost
        // first; a negative `skip` counts from the outermost.
        let state = interp::interp();
        let count = state.frames.len() + 1;
        let index = if skip >= 0 {
            skip as usize
        } else {
            match count.checked_sub(skip.unsigned_abs() as usize) {
                Some(index) => index,
                None => return Ok(runtime().nil()),
            }
        };
        Ok(match index {
            0 => state.srcref,
            _ => state
                .frames
                .iter()
                .rev()
                .nth(index - 1)
                .map_or(runtime().nil(), |frame| frame.srcref),
        })
    }

    unsafe fn dot_last(&self) -> ApiResult<()> {
        Ok(())
    }
}

/// The interpreter's side of evaluation. libR calls it for every evaluation the C API performs;
/// the interpreter in turn calls back into libR (environments, `.Call` through
/// [`native_call`], connections) as ordinary Rust.
///
/// Each method runs inside an API function's `enter`, so it reports a non-local exit as
/// `Err(Unwind)` and must not `longjmp` itself.
pub trait Evaluator: Sync {
    /// Evaluates `expression` in `env`, as `Rf_eval` does.
    unsafe fn eval(&self, expression: Sexp, env: Sexp) -> ApiResult<Sexp>;

    /// Calls `function` with already evaluated arguments, given as a pairlist whose tags are the
    /// argument names. `call` is the call reported in errors and seen by `sys.call()`.
    unsafe fn apply(
        &self,
        call: Sexp,
        function: Sexp,
        arguments: Sexp,
        env: Sexp,
    ) -> ApiResult<Sexp>;

    /// Parses `text` (a character vector) into an expression vector, as `R_ParseVector` does;
    /// `count` is the number of expressions to read, or a negative number for all of them.
    unsafe fn parse(&self, text: Sexp, count: c_int, srcfile: Sexp) -> ApiResult<ParseOutcome>;

    /// Evaluates `call`, forcing the promises of its first `count` arguments before the
    /// function's body runs, as `R_forceAndCall` does.
    unsafe fn force_and_call(&self, call: Sexp, count: c_int, env: Sexp) -> ApiResult<Sexp>;

    /// Prints `value` as auto-printing at top level would, as `Rf_PrintValue` does.
    unsafe fn print_value(&self, value: Sexp, env: Sexp) -> ApiResult<()>;

    /// The environment of the innermost closure call on the stack, or the global environment
    /// at top level, as `R_GetCurrentEnv` reports it.
    unsafe fn current_env(&self) -> Sexp;

    /// The source reference of a call on the stack, as `R_GetCurrentSrcref(skip)` reports it:
    /// with `skip >= 0`, `skip` frames above the innermost; with `skip < 0`, the `-skip`th frame
    /// from the top level. `R_NilValue` when there is none.
    unsafe fn current_srcref(&self, skip: c_int) -> ApiResult<Sexp>;

    /// Runs `.Last()` and `.Last.sys()` as R's shutdown does, as `R_dot_Last` does.
    unsafe fn dot_last(&self) -> ApiResult<()>;
}

/// `ParseStatus` from `R_ext/Parse.h`, with GNU R's numbering.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseStatus {
    Null = 0,
    Ok = 1,
    Incomplete = 2,
    Error = 3,
    Eof = 4,
}

/// The result of a parse: the expression vector (`R_NilValue` unless the status is
/// `ParseStatus::Ok`) and the status.
#[derive(Clone, Copy, Debug)]
pub struct ParseOutcome {
    pub expressions: Sexp,
    pub status: ParseStatus,
}

static EVALUATOR: crate::Global<Option<&'static dyn Evaluator>> = crate::Global::new(None);

/// Makes `evaluator` the one every evaluation entry point of the C API uses. The interpreter
/// calls this once at startup, before any package code runs.
pub fn install_evaluator(evaluator: &'static dyn Evaluator) {
    EVALUATOR.set(Some(evaluator));
}

pub fn evaluator() -> Option<&'static dyn Evaluator> {
    EVALUATOR.get()
}

unsafe extern "C" {
    fn ry_run_task_callbacks(
        expression: Sexp,
        value: Sexp,
        succeeded: Rboolean,
        visible: Rboolean,
        status: *mut c_int,
    );
}

/// Runs the task callbacks packages registered with `Rf_addTaskCallback`. The interpreter calls
/// this after each top-level task, as GNU R's REPL does; an error a callback raises comes back
/// as `Err`.
pub unsafe fn run_task_callbacks(
    expression: Sexp,
    value: Sexp,
    succeeded: bool,
    visible: bool,
) -> ApiResult<()> {
    with_native_scope(|| {
        let mut status = 0;
        ry_run_task_callbacks(
            expression,
            value,
            succeeded as Rboolean,
            visible as Rboolean,
            &mut status,
        );
        status
    })
}

unsafe extern "C" {
    fn ry_add_r_task_callback(function: Sexp, data: Sexp, use_data: Sexp, name: Sexp) -> Sexp;
    fn ry_remove_r_task_callback(id: Sexp) -> Sexp;
    fn ry_task_callback_names() -> Sexp;
}

/// Base's task-callback routines. They live in C beside the list that `Rf_addTaskCallback`
/// keeps, so R-level and native callbacks share one list.
unsafe fn task_callback_routine(routine: TaskCallbackRoutine, args: &[Sexp]) -> ApiResult<Sexp> {
    let function: DlFunc = match routine {
        TaskCallbackRoutine::Add => std::mem::transmute::<
            unsafe extern "C" fn(Sexp, Sexp, Sexp, Sexp) -> Sexp,
            DlFunc,
        >(ry_add_r_task_callback),
        TaskCallbackRoutine::Remove => std::mem::transmute::<
            unsafe extern "C" fn(Sexp) -> Sexp,
            DlFunc,
        >(ry_remove_r_task_callback),
        TaskCallbackRoutine::Names => {
            std::mem::transmute::<unsafe extern "C" fn() -> Sexp, DlFunc>(ry_task_callback_names)
        }
    };
    call_native(function, args, ".Call")
}

/// Stand-ins for a few base R closures, so a host that never loads base R (the C API tests) can
/// still drive native code. Loading base R rebinds these names to base R's own closures.
const BOOTSTRAP_BUILTINS: &[BuiltinDef] = &[
    builtin("dyn.load", -1, |call| unsafe { do_dyn_load(call) }),
    builtin("stop", -1, |call| unsafe {
        Err(error_in(runtime().nil(), paste(call.args)?))
    }),
    builtin("warning", -1, |call| unsafe {
        let message = paste(call.args)?;
        interp::conditions::warning_call(runtime().nil(), message.clone())?;
        interp::values::scalar_string(&message)
    }),
    builtin("identity", 1, |call| Ok(call.arg(0))),
];

unsafe fn paste(args: &[Arg]) -> ApiResult<String> {
    let mut message = String::new();
    for arg in args {
        if arg.value.kind().is_atomic() {
            for index in 0..xlength(arg.value)? as usize {
                let charsxp = interp::coerce::element_as_charsxp(arg.value, index)?;
                message.push_str(&utf8_text(charsxp)?);
            }
        }
    }
    Ok(message)
}

pub unsafe fn eval(expression: Sexp, env: Sexp) -> ApiResult<Sexp> {
    normalize_argument_cells(expression)?;
    interp::eval::eval(expression, env)
}

/// Retypes the argument cells of every call in `expression` to pairlist cells. Native code
/// often builds a call as `LCONS(fun, LCONS(arg, R_NilValue))`, whose argument cells are
/// LANGSXP; GNU R walks any cons cell there, while the interpreter walks pairlist cells only,
/// so it would see no arguments at all.
unsafe fn normalize_argument_cells(expression: Sexp) -> ApiResult<()> {
    if expression.kind() != SexpType::Language {
        return Ok(());
    }
    normalize_argument_cells(expression.car())?;
    let mut cell = expression.cdr();
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        if cell.kind() == SexpType::Language {
            runtime().heap.retype(cell, SexpType::Pairlist)?;
        }
        normalize_argument_cells(cell.car())?;
        cell = cell.cdr();
    }
    Ok(())
}

pub unsafe fn force_promise(promise: Sexp) -> ApiResult<Sexp> {
    interp::eval::force_promise(promise)
}

/// Calls a function with already evaluated arguments, as a finalizer call does.
pub unsafe fn apply_closure_or_primitive(
    function: Sexp,
    arguments: &[Sexp],
    env: Sexp,
) -> ApiResult<Sexp> {
    interp::eval::apply(
        runtime().nil(),
        function,
        &interp::eval::value_args(arguments),
        env,
    )
}

/// A call as R prints it in an error message, or `None` for `NULL`.
pub unsafe fn deparse_call(call: Sexp) -> Option<String> {
    if call.kind() == SexpType::Nil {
        return None;
    }
    Some(interp::deparse::deparse_first_line(call))
}

/// An unwind as the condition object R handlers receive.
pub unsafe fn make_condition(unwind: &Unwind) -> ApiResult<Sexp> {
    match unwind {
        Unwind::Error(failure) => {
            let call = interp::unwind::resolve_call(failure.call);
            interp::conditions::make_condition(
                &failure.message,
                call,
                &["simpleError", "error", "condition"],
            )
        }
        Unwind::Interrupt => {
            interp::conditions::make_condition("", runtime().nil(), &["interrupt", "condition"])
        }
        Unwind::Jump(_) => {
            let message = interp::interp().error_message.clone();
            interp::conditions::make_condition(
                message.trim_end(),
                runtime().nil(),
                &["simpleError", "error", "condition"],
            )
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_eval(expression: Sexp, env: Sexp) -> Sexp {
    enter(|| eval(expression, env))
}

/// `R_tryEval` runs in a top-level context of its own: handlers established outside it do not
/// see its errors, and an error is reported (unless silent) and becomes a null result.
unsafe fn try_eval(expression: Sexp, env: Sexp, failed: *mut c_int, silent: bool) -> Option<Sexp> {
    enter(|| {
        let state = interp::interp();
        let handlers = state.handlers.take();
        let restarts = state.restarts.take();
        let show = std::mem::replace(&mut state.show_error_messages, !silent);
        let frames = state.frames.len();
        let result = eval(expression, env);
        let state = interp::interp();
        state.handlers = handlers;
        state.restarts = restarts;
        state.show_error_messages = show;
        state.frames.truncate(frames);
        if !failed.is_null() {
            *failed = result.is_err() as c_int;
        }
        match result {
            Ok(value) => Ok(Some(value)),
            Err(unwind) => {
                match &unwind {
                    Unwind::Error(_) | Unwind::Interrupt => {
                        if silent {
                            runtime().error_buffer =
                                interp::interp().error_message.clone().into_bytes();
                        } else {
                            report(&unwind, "Error");
                        }
                    }
                    Unwind::Jump(_) => {
                        runtime().error_buffer =
                            interp::interp().error_message.clone().into_bytes();
                    }
                }
                Ok(None)
            }
        }
    })
}

/// `R_tryEval`: the value, or a null pointer after an error, which is printed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_tryEval(
    expression: Sexp,
    env: Sexp,
    failed: *mut c_int,
) -> Option<Sexp> {
    try_eval(expression, env, failed, false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_tryEvalSilent(
    expression: Sexp,
    env: Sexp,
    failed: *mut c_int,
) -> Option<Sexp> {
    try_eval(expression, env, failed, true)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_forceAndCall(call: Sexp, count: c_int, env: Sexp) -> Sexp {
    enter(|| Interpreter.force_and_call(call, count, env))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_applyClosure(
    call: Sexp,
    function: Sexp,
    arguments: Sexp,
    env: Sexp,
    _supplied: Sexp,
) -> Sexp {
    enter(|| {
        let args: Vec<Arg> = interp::values::pairlist_items(arguments)
            .into_iter()
            .map(|(tag, value)| Arg { tag, value })
            .collect();
        interp::eval::apply_closure(call, function, &args, env, None)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_PrintValue(value: Sexp) {
    enter(|| {
        interp::print::print_value_top(value, runtime().heap.known().global_env)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_curErrorBuf() -> *const c_char {
    let runtime = runtime();
    if runtime.error_buffer.last() != Some(&0) {
        runtime.error_buffer.push(0);
    }
    runtime.error_buffer.as_ptr() as *const c_char
}

/// The token `R_UnwindProtect` parks an unwind in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeUnwindCont() -> Sexp {
    enter(|| {
        let nil = runtime().nil();
        Ok(runtime().heap.cons(nil, nil)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ShowMessage(message: *const c_char) {
    let mut text = crate::strings::c_bytes(message).to_vec();
    text.push(b'\n');
    crate::boundary::write_console(&text, true);
}

impl crate::boundary::Handout for Option<Sexp> {
    fn hand_out(self, runtime: &mut crate::Runtime) {
        if let Some(value) = self {
            value.hand_out(runtime);
        }
    }
}

// The builtins the interpreter calls native code through.

const NATIVE_BUILTINS: &[BuiltinDef] = &[
    builtin(".Call", -1, |call| unsafe {
        dot_call(call, RoutineKind::Call)
    }),
    builtin(".External", -1, |call| unsafe {
        dot_call(call, RoutineKind::External)
    }),
    builtin(".External2", -1, |call| unsafe { dot_external2(call) }),
    builtin(".Call.graphics", -1, |call| unsafe {
        graphics_engine::graphics_call(call, || dot_call(call, RoutineKind::Call))
    }),
    builtin(".External.graphics", -1, |call| unsafe {
        graphics_engine::graphics_call(call, || dot_call(call, RoutineKind::External))
    }),
    builtin(".C", -1, |call| unsafe { dot_c(call, RoutineKind::C) }),
    builtin(".Fortran", -1, |call| unsafe {
        dot_c(call, RoutineKind::Fortran)
    }),
    internal("dyn.load", 4, |call| unsafe { do_dyn_load(call) }).invisible(),
    internal("dyn.unload", 1, |_| Ok(unsafe { runtime().nil() })),
    internal("getLoadedDLLs", 0, |_| unsafe { loaded_dlls() }),
    internal("getRegisteredRoutines", 1, |call| unsafe {
        registered_routines(call.arg(0))
    }),
    internal("getSymbolInfo", 3, |call| unsafe {
        do_get_symbol_info(call)
    }),
    internal("is.loaded", 3, |call| unsafe { do_is_loaded(call) }),
];

/// `.Call` and `.External`. Base R's own routines and the default packages' routines run in
/// Rust; any other routine is native code in a loaded library.
unsafe fn dot_call(call: &Call<'_>, kind: RoutineKind) -> ApiResult<Sexp> {
    if kind == RoutineKind::Call
        && let Some(result) = interp::native::call_base_routine(call, Interface::Call)?
    {
        return Ok(result);
    }
    if let Some(value) = builtin_libraries::try_call(call)? {
        return Ok(value);
    }
    native_call(call, kind, false)
}

/// `.External2`, whose routines also receive the call, the primitive and the calling
/// environment.
unsafe fn dot_external2(call: &Call<'_>) -> ApiResult<Sexp> {
    if let Some(value) = builtin_libraries::try_call(call)? {
        return Ok(value);
    }
    native_call(call, RoutineKind::External, true)
}

/// `.C` and `.Fortran`, routed like [`dot_call`].
unsafe fn dot_c(call: &Call<'_>, kind: RoutineKind) -> ApiResult<Sexp> {
    let interface = if kind == RoutineKind::Fortran {
        Interface::Fortran
    } else {
        Interface::C
    };
    if let Some(result) = interp::native::call_base_routine(call, interface)? {
        return Ok(result);
    }
    if let Some(value) = builtin_libraries::try_call(call)? {
        return Ok(value);
    }
    native_call(call, kind, false)
}

fn tagged_values(call: &Call<'_>) -> Vec<(Sexp, Sexp)> {
    call.args.iter().map(|arg| (arg.tag, arg.value)).collect()
}

/// A native routine through `.Call`, `.External`, `.C` or `.Fortran`, with `external2` for an
/// `.External` routine that `.External2` calls. The first argument is `.NAME`.
unsafe fn native_call(
    builtin_call: &Call<'_>,
    kind: RoutineKind,
    external2: bool,
) -> ApiResult<Sexp> {
    let call = builtin_call.call;
    let arguments = &tagged_values(builtin_call);
    let what = kind.interface();
    let Some(((_, name), rest)) = arguments.split_first() else {
        return Err(error_in(call, "'.NAME' is missing"));
    };
    let package_symbol = runtime().heap.install(b"PACKAGE")?;
    let control_symbols = [
        runtime().heap.install(b"NAOK")?,
        runtime().heap.install(b"DUP")?,
        runtime().heap.install(b"ENCODING")?,
    ];
    let mut package = None;
    let mut na_ok = false;
    let mut values = Vec::new();
    for &(tag, value) in rest {
        if matches!(kind, RoutineKind::C | RoutineKind::Fortran) && control_symbols.contains(&tag) {
            if tag == control_symbols[0] {
                na_ok = crate::vectors::as_logical(value)? == 1;
            }
            continue;
        }
        if tag == package_symbol {
            if value.kind() != SexpType::String || xlength(value)? < 1 {
                return Err(error_in(
                    call,
                    "PACKAGE argument must be a single character string",
                ));
            }
            package = Some(utf8_text(crate::vectors::sexp_elt(value, 0)?)?.into_owned());
        } else {
            values.push((tag, value));
        }
    }
    let (function, arity, routine_name) = match name.kind() {
        SexpType::String if xlength(*name)? >= 1 => {
            let routine_name = utf8_text(crate::vectors::sexp_elt(*name, 0)?)?.into_owned();
            let (function, arity) = find_routine(&routine_name, package.as_deref(), kind).map_err(
                |unwind| match unwind {
                    Unwind::Error(error) => error_in(call, error.message),
                    other => other,
                },
            )?;
            (function, arity, routine_name)
        }
        // A `NativeSymbolInfo` list carries the routine's address in its `address` element.
        SexpType::List if native_symbol_address(*name)?.is_some() => {
            let address = native_symbol_address(*name)?
                .unwrap_or(*name)
                .external_address();
            if address.is_null() {
                return Err(error_in(call, "NULL value passed as symbol address"));
            }
            let arity = crate::attributes::get_attrib(*name, R_NamesSymbol.get())
                .ok()
                .and_then(|_| list_element(*name, "numParameters"))
                .filter(|value| value.kind() == SexpType::Integer && value.length() == 1)
                .map(|value| value.slice::<i32>()[0]);
            (
                std::mem::transmute::<*mut std::ffi::c_void, crate::boundary::DlFunc>(address),
                arity,
                list_element(*name, "name")
                    .filter(|value| value.kind() == SexpType::String)
                    .and_then(|value| {
                        utf8_text(value.element(0))
                            .ok()
                            .map(|text| text.into_owned())
                    })
                    .unwrap_or_default(),
            )
        }
        SexpType::ExternalPtr => {
            let address = name.external_address();
            if address.is_null() {
                return Err(error_in(call, "NULL value passed as symbol address"));
            }
            (
                std::mem::transmute::<*mut std::ffi::c_void, crate::boundary::DlFunc>(address),
                None,
                String::new(),
            )
        }
        _ => {
            return Err(error_in(
                call,
                "first argument must be a string (of length 1) or native symbol reference",
            ));
        }
    };
    match kind {
        RoutineKind::Call => {
            if values.len() > 65 {
                return Err(error_in(call, "too many arguments, sorry"));
            }
            if let Some(arity) = arity
                && arity >= 0
                && arity as usize != values.len()
            {
                return Err(error_in(
                    call,
                    format!(
                        "Incorrect number of arguments ({}), expecting {arity} for '{routine_name}'",
                        values.len()
                    ),
                ));
            }
            let args: Vec<Sexp> = values.iter().map(|(_, value)| *value).collect();
            call_native(function, &args, what)
        }
        RoutineKind::External => {
            let nil = runtime().nil();
            let mut list = nil;
            for &(tag, value) in values.iter().rev() {
                list = runtime()
                    .heap
                    .alloc_triple(SexpType::Pairlist, value, list, tag)?;
                runtime().heap.root(list);
            }
            let list = runtime()
                .heap
                .alloc_triple(SexpType::Pairlist, *name, list, nil)?;
            runtime().heap.root(list);
            if external2 {
                call_native(
                    function,
                    &[call, builtin_call.op, list, builtin_call.env],
                    ".External2",
                )
            } else {
                call_native(function, &[list], what)
            }
        }
        RoutineKind::C | RoutineKind::Fortran => {
            if values.len() > 65 {
                return Err(error_in(call, "too many arguments, sorry"));
            }
            if let Some(arity) = arity
                && arity >= 0
                && arity as usize != values.len()
            {
                return Err(error_in(
                    call,
                    format!(
                        "Incorrect number of arguments ({}), expecting {arity} for '{routine_name}'",
                        values.len()
                    ),
                ));
            }
            c_call(call, function, &values, kind, na_ok)
        }
    }
}

/// One `.C` or `.Fortran` argument as native code receives it: a pointer into a fresh copy, so
/// the routine's writes land in the copy that is returned.
enum CArgument {
    Vector(Sexp),
    Strings(Sexp, Vec<*mut c_char>),
    Object(Sexp),
}

/// `.C` and `.Fortran`: each argument is copied, the routine receives pointers to the copies'
/// data, and the result is the list of copies, named as the arguments were.
unsafe fn c_call(
    call: Sexp,
    function: crate::boundary::DlFunc,
    values: &[(Sexp, Sexp)],
    kind: RoutineKind,
    na_ok: bool,
) -> ApiResult<Sexp> {
    let runtime = runtime();
    let mut arguments = Vec::with_capacity(values.len());
    let mut pointers: Vec<*mut std::ffi::c_void> = Vec::with_capacity(values.len());
    for (position, &(_, value)) in values.iter().enumerate() {
        let argument = match value.kind() {
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Raw => {
                // The copy is an ordinary vector even when the argument is ALTREP, so the
                // checks below read its storage directly.
                let copy = crate::memory::duplicate(value, false)?;
                runtime.heap.root(copy);
                if !na_ok && !c_values_finite(copy)? {
                    return Err(error_in(
                        call,
                        format!("NA/NaN/Inf in foreign function call (arg {})", position + 1),
                    ));
                }
                pointers.push(crate::vectors::data_pointer(copy, true)? as *mut std::ffi::c_void);
                CArgument::Vector(copy)
            }
            SexpType::String => {
                // An ALTREP class may duplicate into another ALTREP object, so the copy is
                // materialized: the routine writes back into ordinary storage.
                let copy = crate::memory::duplicate(value, false)?;
                runtime.heap.root(copy);
                let copy = if copy.is_altrep() {
                    crate::altrep::materialize(copy)?
                } else {
                    copy
                };
                runtime.heap.root(copy);
                let length = xlength(copy)? as usize;
                let mut strings = Vec::with_capacity(length);
                for index in 0..length {
                    let element = copy.element(index);
                    if !na_ok && element == runtime.heap.known().na_string {
                        return Err(error_in(
                            call,
                            format!("NA/NaN/Inf in foreign function call (arg {})", position + 1),
                        ));
                    }
                    strings.push(crate::strings::arena_c_string(element.bytes())? as *mut c_char);
                }
                let pointer = if kind == RoutineKind::Fortran {
                    strings.first().copied().unwrap_or(std::ptr::null_mut())
                        as *mut std::ffi::c_void
                } else {
                    strings.as_mut_ptr() as *mut std::ffi::c_void
                };
                pointers.push(pointer);
                CArgument::Strings(copy, strings)
            }
            SexpType::List if kind == RoutineKind::C => {
                let copy = crate::memory::duplicate(value, false)?;
                runtime.heap.root(copy);
                runtime.heap.escape(copy);
                pointers.push(copy.data() as *mut std::ffi::c_void);
                CArgument::Vector(copy)
            }
            _ if kind == RoutineKind::C => {
                pointers.push(value.as_ptr() as *mut std::ffi::c_void);
                CArgument::Object(value)
            }
            other => {
                return Err(error_in(
                    call,
                    format!(
                        "invalid mode ({}) to pass to Fortran (arg {})",
                        other.name(),
                        position + 1
                    ),
                ));
            }
        };
        arguments.push(argument);
    }
    crate::boundary::call_native_pointers(function, &pointers, kind.interface())?;
    let runtime = crate::runtime();
    let result = runtime
        .heap
        .alloc_vector(SexpType::List, values.len() as isize)?;
    runtime.heap.root(result);
    let mut named = false;
    let names = runtime
        .heap
        .alloc_vector(SexpType::String, values.len() as isize)?;
    runtime.heap.root(names);
    for (index, argument) in arguments.iter().enumerate() {
        let value = match argument {
            CArgument::Vector(copy) | CArgument::Object(copy) => *copy,
            CArgument::Strings(copy, strings) => {
                for (element, &pointer) in strings.iter().enumerate() {
                    let text = crate::strings::c_bytes(pointer);
                    let charsxp = runtime.heap.mk_char(text, Encoding::Native)?;
                    runtime.heap.set_element(*copy, element, charsxp);
                }
                *copy
            }
        };
        runtime.heap.set_element(result, index, value);
        let tag = values[index].0;
        if tag.kind() == SexpType::Symbol {
            named = true;
            runtime.heap.set_element(names, index, tag.car());
        }
    }
    if named {
        crate::attributes::install_attribute(result, R_NamesSymbol.get(), names)?;
    }
    Ok(result)
}

/// Whether a numeric vector has no `NA`, `NaN` or infinite element, which `.C` requires
/// unless `NAOK = TRUE`.
unsafe fn c_values_finite(value: Sexp) -> ApiResult<bool> {
    let length = xlength(value)? as usize;
    Ok(match value.kind() {
        SexpType::Logical | SexpType::Integer => {
            (0..length).all(|index| value.slice::<i32>()[index] != NA_INTEGER)
        }
        SexpType::Real => value.slice::<f64>().iter().all(|number| number.is_finite()),
        SexpType::Complex => value
            .slice::<heap::object::Rcomplex>()
            .iter()
            .all(|number| number.r.is_finite() && number.i.is_finite()),
        _ => true,
    })
}

/// A `DLLInfo` object: name, path, dynamic lookup flag, and the handle and record pointers.
unsafe fn dll_info(dll: *mut Dll) -> ApiResult<Sexp> {
    let info = interp::native::dll_info(&(*dll).name, &(*dll).path)?;
    let record = interp::values::list_get(info, "info");
    record.set_external_address(dll.cast());
    let handle = interp::values::list_get(info, "handle");
    handle.set_external_address((*dll).handle());
    let dynamic = interp::values::scalar_bool((*dll).dynamic_lookup())?;
    let list_index = 2;
    runtime().heap.set_element(info, list_index, dynamic);
    Ok(info)
}

unsafe fn do_dyn_load(call: &Call<'_>) -> ApiResult<Sexp> {
    let path = call.arg(0);
    if path.kind() != SexpType::String || xlength(path)? != 1 {
        return Err(error_in(call.call, "character argument expected"));
    }
    let path = utf8_text(path.element(0))?.into_owned();
    if let Some(package) = builtin_libraries::builtin_library(&path) {
        return Ok(builtin_libraries::load(package, &path)?);
    }
    let local = call.arg(1) == runtime().nil() || crate::vectors::as_logical(call.arg(1))? != 0;
    let dll = crate::dynload::load_library(&path, local)?;
    dll_info(dll)
}

unsafe fn loaded_dlls() -> ApiResult<Sexp> {
    let mut items = vec![interp::native::dll_info("base", "base")?];
    let mut names = vec!["base".to_owned()];
    for (name, info) in builtin_libraries::loaded()? {
        items.push(info);
        names.push(name);
    }
    let count = runtime().dlls.len();
    for index in 0..count {
        let dll: *mut Dll = &mut *runtime().dlls[index];
        items.push(dll_info(dll)?);
        names.push((*dll).name.clone());
    }
    let list = interp::values::list_vector(&items)?;
    let names = interp::values::string_vector(&names)?;
    interp::values::set_attr(list, interp::values::symbols().names, names)?;
    let class = interp::values::scalar_string("DLLInfoList")?;
    interp::values::set_attr(list, interp::values::symbols().class, class)?;
    Ok(list)
}

unsafe fn registered_routines(info: Sexp) -> ApiResult<Sexp> {
    if let Some(routines) = builtin_libraries::registered_routines(info)? {
        return Ok(routines);
    }
    let dll = if info.kind() == SexpType::ExternalPtr {
        info.external_address() as *mut Dll
    } else if info.kind() == SexpType::List {
        interp::values::list_get(info, "info").external_address() as *mut Dll
    } else {
        std::ptr::null_mut()
    };
    if dll.is_null() {
        return interp::native::base_routines();
    }
    let info_object = dll_info(dll)?;
    let mut tables = Vec::new();
    for (kind, class) in [
        (RoutineKind::C, "CRoutine"),
        (RoutineKind::Call, "CallRoutine"),
        (RoutineKind::Fortran, "FortranRoutine"),
        (RoutineKind::External, "ExternalRoutine"),
    ] {
        let routines: Vec<(String, DlFunc, c_int)> = (*dll)
            .registered(kind)
            .iter()
            .map(|routine| (routine.name.clone(), routine.function, routine.arity))
            .collect();
        let mut entries = Vec::new();
        for (name, function, arity) in &routines {
            entries.push(native_symbol(name, *function, *arity, info_object, class)?);
        }
        let table = interp::values::list_vector(&entries)?;
        let names: Vec<&str> = routines.iter().map(|(name, _, _)| name.as_str()).collect();
        let names = interp::values::string_vector(&names)?;
        interp::values::set_attr(table, interp::values::symbols().names, names)?;
        let list_class = interp::values::scalar_string("NativeRoutineList")?;
        interp::values::set_attr(table, interp::values::symbols().class, list_class)?;
        tables.push(table);
    }
    let result = interp::values::list_vector(&tables)?;
    let names = interp::values::string_vector(&[".C", ".Call", ".Fortran", ".External"])?;
    interp::values::set_attr(result, interp::values::symbols().names, names)?;
    Ok(result)
}

/// A `NativeSymbolInfo` object for a routine.
unsafe fn native_symbol(
    name: &str,
    function: DlFunc,
    arity: c_int,
    dll: Sexp,
    class: &str,
) -> ApiResult<Sexp> {
    let nil = runtime().nil();
    let address = runtime()
        .heap
        .alloc_external_ptr(function as *mut c_void, nil, nil)?;
    runtime().heap.root(address);
    let address_class = interp::values::scalar_string("RegisteredNativeSymbol")?;
    interp::values::set_attr(address, interp::values::symbols().class, address_class)?;
    let entry = interp::values::named_list(&[
        ("name", interp::values::scalar_string(name)?),
        ("address", address),
        ("dll", dll),
        ("numParameters", interp::values::scalar_integer(arity)?),
    ])?;
    let entry_class = interp::values::string_vector(&[class, "NativeSymbolInfo"])?;
    interp::values::set_attr(entry, interp::values::symbols().class, entry_class)?;
    Ok(entry)
}

unsafe fn do_get_symbol_info(call: &Call<'_>) -> ApiResult<Sexp> {
    let name = interp::values::require_string(call.arg(0), "name")?;
    let package = call.arg(1);
    let package_name = if package.kind() == SexpType::String {
        interp::values::first_string(package).map(|text| text.into_owned())
    } else if package.kind() == SexpType::List {
        interp::values::first_string(interp::values::list_get(package, "name"))
            .map(|text| text.into_owned())
    } else if package.kind() == SexpType::ExternalPtr {
        // A `DLLInfoReference`, the `info` pointer `getNativeSymbolInfo` passes for a DLLInfo,
        // as `useDynLib(pkg, symbol)` does. A loaded library's points at its record, and a
        // builtin library's carries its package name instead.
        let address = package.external_address();
        runtime()
            .dlls
            .iter()
            .find(|dll| std::ptr::eq(&***dll, address.cast::<Dll>()))
            .map(|dll| dll.name.clone())
            .or_else(|| {
                interp::values::first_string(package.external_protected())
                    .map(|text| text.into_owned())
            })
    } else {
        None
    };
    if let Some(info) = builtin_libraries::symbol_info(&name, package_name.as_deref())? {
        return Ok(info);
    }
    // Registered routines are found even in a library that forces symbols: that only forbids
    // `.Call("name")` by string, while `useDynLib(pkg, name)` resolves names through here.
    let packages: Vec<String> = match package_name {
        Some(package) => vec![package],
        None => runtime()
            .dlls
            .iter()
            .rev()
            .map(|dll| dll.name.clone())
            .collect(),
    };
    for package in packages {
        let Some((kind, function, arity)) = crate::dynload::registered_routine(&name, &package)
        else {
            continue;
        };
        let dll = runtime()
            .dlls
            .iter_mut()
            .rev()
            .find(|dll| dll.name == package)
            .map(|dll| &mut **dll as *mut Dll);
        let dll_object = match dll {
            Some(dll) => dll_info(dll)?,
            None => runtime().nil(),
        };
        let class = match kind {
            RoutineKind::Call => "CallRoutine",
            RoutineKind::External => "ExternalRoutine",
            RoutineKind::C => "CRoutine",
            RoutineKind::Fortran => "FortranRoutine",
        };
        return native_symbol(&name, function, arity, dll_object, class);
    }
    Ok(runtime().nil())
}

unsafe fn do_is_loaded(call: &Call<'_>) -> ApiResult<Sexp> {
    let name = interp::values::require_string(call.arg(0), "symbol")?;
    let package = interp::values::first_string(call.arg(1)).map(|text| text.into_owned());
    let package = package.filter(|package| !package.is_empty());
    let found = [
        RoutineKind::Call,
        RoutineKind::External,
        RoutineKind::C,
        RoutineKind::Fortran,
    ]
    .into_iter()
    .any(|kind| find_routine(&name, package.as_deref(), kind).is_ok())
        || (package.as_deref().is_none_or(|package| package == "base")
            && interp::native::is_base_routine(&name));
    interp::values::scalar_bool(found)
}

/// For tests that build a call by hand.
pub unsafe fn quote_error(message: &str) -> Unwind {
    error(message)
}

/// Starts the R session (base R and the default packages from the GNU R installation) without
/// running a script, for hosts and tests that call R functions directly. Returns 0 once the
/// session runs and nonzero when base R did not load. Starting twice does nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_start_session() -> c_int {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    runtime();
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return 0;
    }
    if report_missing_r_home() {
        return 2;
    }
    let options = interp::session::StartOptions {
        command_args: vec!["ry".to_owned()],
        site_profile: false,
        user_profile: false,
        interactive: false,
    };
    match interp::session::start(options) {
        Ok(()) => 0,
        Err(unwind) => {
            interp::session::report_toplevel(unwind);
            1
        }
    }
}

/// The hash of the sources this library was built from, as a NUL-terminated string. The `ry`
/// executable compares it with its own right after loading, because cargo builds `libR.so`
/// separately and a stale one would otherwise run old code without any sign.
#[unsafe(no_mangle)]
pub extern "C" fn ry_runtime_id() -> *const c_char {
    concat!(env!("RY_RUNTIME_ID"), "\0").as_ptr().cast()
}

/// Runs R on ry's runtime the way `Rscript` does: starts the session (base R and the default
/// packages from the GNU R installation), attaches the package loader as `tools:ry`, evaluates
/// `input` and then the file at `path` (either may be null, and with both null it reads R code
/// from stdin, running each expression once it is complete), and returns the exit status: 0 on
/// success, 1 after an error, 2 when the session could not start. `arguments` are what
/// `commandArgs(trailingOnly = TRUE)` returns, and `flags` are the [`RUN_FLAGS`] bits. This is
/// the entry point of `ry run --runtime ry` and `ry load`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_run(
    path: *const c_char,
    input: *const c_char,
    count: c_int,
    arguments: *const *const c_char,
    flags: c_int,
) -> c_int {
    apply_flags(flags);
    let path = (!path.is_null())
        .then(|| String::from_utf8_lossy(crate::strings::c_bytes(path)).into_owned());
    let input = (!input.is_null())
        .then(|| String::from_utf8_lossy(crate::strings::c_bytes(input)).into_owned());
    let mut command_args = vec!["ry".to_owned()];
    if let Some(path) = &path {
        command_args.push(format!("--file={path}"));
    }
    command_args.push("--args".to_owned());
    for index in 0..count.max(0) as usize {
        let argument = *arguments.add(index);
        command_args.push(String::from_utf8_lossy(crate::strings::c_bytes(argument)).into_owned());
    }
    runtime();
    if report_missing_r_home() {
        return 2;
    }
    let options = interp::session::StartOptions {
        command_args,
        site_profile: flags & RUN_FLAG_SITE_PROFILE != 0,
        user_profile: flags & RUN_FLAG_USER_PROFILE != 0,
        interactive: false,
    };
    if let Err(unwind) = interp::session::start(options) {
        interp::session::report_toplevel(unwind);
        crate::boundary::write_console(b"ry: base R did not load\n", true);
        return 2;
    }
    if let Err(unwind) = loader::ry_runtime::attach() {
        interp::session::report_toplevel(unwind);
        crate::boundary::write_console(b"ry: the package loader did not load\n", true);
        return 2;
    }
    if heap::gc_stress_requested() == Some(heap::GcStressStart::Script) {
        runtime().heap.policy_mut().stress = heap::GcStress::EveryAllocation;
    }
    let mut outcome = interp::session::Outcome::Completed;
    if let Some(input) = &input {
        outcome = interp::session::run_text(input, "<input>");
    } else if path.is_none() {
        // Each expression runs as soon as it is complete, not once stdin ends: callr drives `R`
        // this way, writing a command and waiting for its result before it writes the next.
        let mut stdin = std::io::stdin().lock();
        outcome = interp::session::run_console(
            |_prompt| {
                use std::io::{BufRead, Write};
                if let Err(error) = std::io::stdout().flush() {
                    let message = format!("ry: cannot write output: {error}\n");
                    crate::boundary::write_console(message.as_bytes(), true);
                }
                let mut line = Vec::new();
                match stdin.read_until(b'\n', &mut line) {
                    Ok(0) => None,
                    Ok(_) => Some(String::from_utf8_lossy(&line).into_owned()),
                    Err(error) => {
                        let message = format!("ry: cannot read R input from stdin: {error}\n");
                        crate::boundary::write_console(message.as_bytes(), true);
                        None
                    }
                }
            },
            true,
        );
    }
    if let (interp::session::Outcome::Completed, Some(path)) = (&outcome, &path) {
        outcome = match std::fs::read_to_string(path) {
            Ok(text) => interp::session::run_text(&text, path),
            Err(failure) => {
                let message = format!("Fatal error: cannot open file '{path}': {failure}\n");
                crate::boundary::write_console(message.as_bytes(), true);
                return 2;
            }
        };
    }
    interp::session::finish();
    crate::boundary::run_pending_finalizers();
    crate::extptr::R_RunExitFinalizers();
    use std::io::Write;
    drop(std::io::stdout().flush());
    match outcome {
        interp::session::Outcome::Completed => 0,
        interp::session::Outcome::Failed => 1,
    }
}

/// Prints why no GNU R installation was found, and says whether one is missing. Base R's own R
/// code comes from that installation, so a session cannot start without it.
fn report_missing_r_home() -> bool {
    match interp::system::missing_r_home() {
        Some(message) => {
            crate::boundary::write_console(message.as_bytes(), true);
            true
        }
        None => false,
    }
}

/// `ry_run` and `ry_repl` flag: inline type annotations become checks at function
/// boundaries. Off when the project turns inline annotations off.
pub const RUN_FLAG_ANNOTATION_CHECKS: c_int = 1 << 0;

/// Set when S4, R6 and Reference Class definitions in user code are allowed.
pub const RUN_FLAG_OBJECT_SYSTEMS: c_int = 1 << 1;

/// Set when the project is in strict mode, so the package loader binds pure constants lazily.
pub const RUN_FLAG_STRICT: c_int = 1 << 2;

/// `ry_run` flag: source the site profile, as GNU R does without `--vanilla` or
/// `--no-site-file`. `ry run` leaves it off; ry's R home's `bin/R` and `bin/Rscript` set it.
pub const RUN_FLAG_SITE_PROFILE: c_int = 1 << 3;

/// `ry_run` flag: source the user profile, as GNU R does without `--vanilla` or
/// `--no-init-file`.
pub const RUN_FLAG_USER_PROFILE: c_int = 1 << 4;

/// Every flag `ry_run` and `ry_repl` understand.
pub const RUN_FLAGS: c_int = RUN_FLAG_ANNOTATION_CHECKS
    | RUN_FLAG_OBJECT_SYSTEMS
    | RUN_FLAG_STRICT
    | RUN_FLAG_SITE_PROFILE
    | RUN_FLAG_USER_PROFILE;

unsafe fn apply_flags(flags: c_int) {
    runtime();
    interp::interp().annotations.enabled = flags & RUN_FLAG_ANNOTATION_CHECKS != 0;
    interp::interp().object_systems = flags & RUN_FLAG_OBJECT_SYSTEMS != 0;
    loader::ry_runtime::set_strict(flags & RUN_FLAG_STRICT != 0);
}

/// Reads one chunk of console input after showing `prompt`: writes at most `length - 1` bytes
/// and a NUL into `buffer`, and returns 1, or 0 at the end of input. The same contract as
/// GNU R's `R_ReadConsole` hook without its history flag.
pub type ReadConsole = unsafe extern "C" fn(*const c_char, *mut u8, c_int) -> c_int;

/// Runs ry's runtime as an interactive console: starts the session, attaches the package
/// loader as `tools:ry`, evaluates `startup` (null for none; an error in it is reported and the
/// console still opens), then reads input through `read` and evaluates it until the input
/// ends. Returns 0, or 2 when the session could not start. This is `ry run`'s console entry
/// point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_repl(read: ReadConsole, startup: *const c_char, flags: c_int) -> c_int {
    apply_flags(flags);
    if report_missing_r_home() {
        return 2;
    }
    let startup = (!startup.is_null())
        .then(|| String::from_utf8_lossy(crate::strings::c_bytes(startup)).into_owned());
    let options = interp::session::StartOptions {
        command_args: vec!["ry".to_owned()],
        site_profile: true,
        user_profile: true,
        interactive: true,
    };
    if let Err(unwind) = interp::session::start(options) {
        interp::session::report_toplevel(unwind);
        crate::boundary::write_console(b"ry: base R did not load\n", true);
        return 2;
    }
    if let Err(unwind) = loader::ry_runtime::attach() {
        interp::session::report_toplevel(unwind);
        crate::boundary::write_console(b"ry: the package loader did not load\n", true);
        return 2;
    }
    install_interrupt_handler();
    if let Some(startup) = &startup {
        // `run_text` reports an error itself, and a failed startup load still leaves a console
        // the user can fix the package from.
        interp::session::run_text(startup, "<startup>");
    }
    let mut buffer = vec![0u8; 64 * 1024];
    interp::session::run_console(
        |prompt| {
            use std::io::Write;
            if let Err(error) = std::io::stdout().flush() {
                crate::boundary::write_console(
                    format!("ry: cannot write output: {error}\n").as_bytes(),
                    true,
                );
            }
            // A prompt never holds a NUL, so the empty fallback is unreachable.
            let mut prompt = std::ffi::CString::new(prompt).unwrap_or_default();
            let mut input = Vec::new();
            // A line longer than the buffer arrives in several chunks; only the last one ends the
            // line, and the text is decoded once it is whole, so no character is split.
            loop {
                if read(prompt.as_ptr(), buffer.as_mut_ptr(), buffer.len() as c_int) == 0 {
                    return (!input.is_empty())
                        .then(|| String::from_utf8_lossy(&input).into_owned());
                }
                let end = buffer
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(buffer.len());
                input.extend_from_slice(&buffer[..end]);
                // A reader may stop one byte short of the contract's limit, so a chunk is taken as
                // cut off whenever it comes within two bytes of the buffer's end.
                if end + 2 < buffer.len() || input.ends_with(b"\n") {
                    return Some(String::from_utf8_lossy(&input).into_owned());
                }
                prompt = std::ffi::CString::default();
            }
        },
        false,
    );
    interp::session::finish();
    crate::boundary::run_pending_finalizers();
    0
}

/// Returns 1 when a package loaded in this session changed on disk since it was loaded, and 0
/// otherwise. ry's console asks before each command and reloads with `.ry_autoreload()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_loader_changed() -> c_int {
    c_int::from(loader::ry_runtime::changed())
}

/// While the console reads input its terminal is raw, so Ctrl-C reaches the line editor; while
/// code runs it arrives as SIGINT and interrupts the evaluation, not the process.
#[cfg(unix)]
fn install_interrupt_handler() {
    extern "C" fn request_interrupt(_signal: c_int) {
        interp::unwind::INTERRUPT_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // SAFETY: the handler only performs an atomic store, which is async-signal-safe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = request_interrupt as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
fn install_interrupt_handler() {}

unsafe fn installed_evaluator(what: &str) -> ApiResult<&'static dyn Evaluator> {
    evaluator().ok_or_else(|| {
        error(format!(
            "{what} needs ry's interpreter, which is not loaded"
        ))
    })
}

/// `R_ParseVector(text, n, status, srcfile)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ParseVector(
    text: Sexp,
    count: c_int,
    status: *mut ParseStatus,
    srcfile: Sexp,
) -> Sexp {
    enter(|| {
        let outcome = installed_evaluator("R_ParseVector")?.parse(text, count, srcfile)?;
        if !status.is_null() {
            *status = outcome.status;
        }
        Ok(outcome.expressions)
    })
}

/// `R_ParseString(text)`: the single expression `text` holds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ParseString(text: *const c_char) -> Sexp {
    enter(|| parse_one(text))
}

unsafe fn parse_one(text: *const c_char) -> ApiResult<Sexp> {
    let charsxp = runtime()
        .heap
        .mk_char(crate::strings::c_bytes(text), Encoding::Utf8)?;
    let vector = crate::memory::scalar_string(charsxp)?;
    runtime().heap.root(vector);
    let nil = runtime().nil();
    let outcome = installed_evaluator("R_ParseString")?.parse(vector, -1, nil)?;
    if outcome.status != ParseStatus::Ok {
        return Err(error("parse error"));
    }
    match xlength(outcome.expressions)? {
        0 => Ok(nil),
        1 => Ok(outcome.expressions.element(0)),
        _ => Err(error("parse error: more than one expression")),
    }
}

/// `R_ParseEvalString(text, env)`: parses one expression and evaluates it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ParseEvalString(text: *const c_char, env: Sexp) -> Sexp {
    enter(|| {
        let expression = parse_one(text)?;
        runtime().heap.root(expression);
        eval(expression, env)
    })
}

/// Evaluates `package::function(arguments...)` in the global environment, which loads the
/// package's namespace when needed. The C API uses this for entry points that GNU R implements
/// by calling R code, such as the S4 class functions that call into `methods`.
pub unsafe fn call_namespaced(
    package: &str,
    function: &str,
    arguments: &[Sexp],
) -> ApiResult<Sexp> {
    let runtime = runtime();
    let nil = runtime.nil();
    let mut list = nil;
    for &argument in arguments.iter().rev() {
        list = runtime.heap.cons(argument, list)?;
        runtime.heap.root(list);
    }
    let colons = runtime.heap.install(b"::")?;
    let package = runtime.heap.install(package.as_bytes())?;
    let function = runtime.heap.install(function.as_bytes())?;
    let head = crate::lists::list_of(SexpType::Language, &[colons, package, function])?;
    runtime.heap.root(head);
    let call = runtime
        .heap
        .alloc_triple(SexpType::Language, head, list, nil)?;
    runtime.heap.root(call);
    let global = runtime.heap.known().global_env;
    installed_evaluator(&format!(
        "{}::{}",
        utf8_text(package.car())?,
        utf8_text(function.car())?
    ))?;
    eval(call, global)
}

/// `R_GetCurrentEnv()`: the environment code calling it runs in. Native code is called from a
/// closure's frame, which only the interpreter knows; without one it is the global environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetCurrentEnv() -> Sexp {
    enter(|| {
        Ok(match evaluator() {
            Some(evaluator) => evaluator.current_env(),
            None => runtime().heap.known().global_env,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetCurrentSrcref(skip: c_int) -> Sexp {
    enter(|| match evaluator() {
        Some(evaluator) => evaluator.current_srcref(skip),
        None => Ok(runtime().nil()),
    })
}

/// `R_GetSrcFilename(srcref)`: the file name recorded in a source reference's `srcfile`
/// environment, or `""`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetSrcFilename(srcref: Sexp) -> Sexp {
    enter(|| {
        let srcfile =
            crate::attributes::raw_attribute(srcref, crate::globals::R_SrcfileSymbol.get());
        let empty = || -> ApiResult<Sexp> {
            let blank = runtime().heap.known().blank_string;
            crate::memory::scalar_string(blank)
        };
        if srcfile.kind() != SexpType::Environment {
            return empty();
        }
        let filename = runtime().heap.install(b"filename")?;
        let value = crate::environments::frame_lookup(srcfile, filename)?;
        if value.kind() == SexpType::String {
            Ok(value)
        } else {
            empty()
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_dot_Last() {
    enter(|| match evaluator() {
        Some(evaluator) => evaluator.dot_last(),
        None => Ok(()),
    })
}

/// The element of a named list called `name`.
unsafe fn list_element(list: Sexp, name: &str) -> Option<Sexp> {
    let names = crate::attributes::raw_attribute(list, R_NamesSymbol.get());
    if names.kind() != SexpType::String {
        return None;
    }
    (0..names.length() as usize)
        .find(|&index| names.element(index).bytes() == name.as_bytes())
        .map(|index| list.element(index))
}

/// The routine address of a `NativeSymbolInfo` list, if `value` is one.
unsafe fn native_symbol_address(value: Sexp) -> ApiResult<Option<Sexp>> {
    if !crate::attributes::inherits(value, "NativeSymbolInfo")? {
        return Ok(None);
    }
    Ok(list_element(value, "address").filter(|address| address.kind() == SexpType::ExternalPtr))
}

const INFIX: &[&str] = &[
    "+", "-", "*", "/", "^", "==", "!=", "<", ">", "<=", ">=", "&", "|", "&&", "||", "<-", "<<-",
    "=", "$", "@", ":", "~", "|>",
];

/// A compact deparser for error messages: symbols, constants and calls.
pub unsafe fn deparse(object: Sexp, depth: usize) -> String {
    if depth > 20 {
        return "...".into();
    }
    // The vector arms below read storage directly, so an ALTREP vector is expanded first.
    if object.is_altrep() {
        return match crate::altrep::materialize(object) {
            Ok(expanded) => {
                runtime().heap.root(expanded);
                deparse(expanded, depth)
            }
            Err(_) => format!("<{}>", object.kind().name()),
        };
    }
    match object.kind() {
        SexpType::Nil => "NULL".into(),
        SexpType::Symbol => utf8_text(object.car())
            .map(|text| text.into_owned())
            .unwrap_or_default(),
        SexpType::Language => {
            let head = deparse(object.car(), depth + 1);
            let mut arguments = Vec::new();
            let mut cell = object.cdr();
            while cell.kind() == SexpType::Pairlist {
                let value = deparse(cell.car(), depth + 1);
                if cell.tag().kind() == SexpType::Symbol {
                    let name = utf8_text(cell.tag().car())
                        .map(|text| text.into_owned())
                        .unwrap_or_default();
                    arguments.push(format!("{name} = {value}"));
                } else {
                    arguments.push(value);
                }
                cell = cell.cdr();
            }
            if object.car().kind() == SexpType::Symbol
                && arguments.len() == 2
                && INFIX.contains(&head.as_str())
            {
                let separator = if matches!(head.as_str(), "$" | "@" | ":") {
                    head.clone()
                } else {
                    format!(" {head} ")
                };
                return format!("{}{separator}{}", arguments[0], arguments[1]);
            }
            if head == "[" && !arguments.is_empty() {
                return format!("{}[{}]", arguments[0], arguments[1..].join(", "));
            }
            if head == "[[" && !arguments.is_empty() {
                return format!("{}[[{}]]", arguments[0], arguments[1..].join(", "));
            }
            format!("{head}({})", arguments.join(", "))
        }
        SexpType::String => {
            let elements: Vec<String> = (0..object.length() as usize)
                .map(|index| {
                    let element = object.element(index);
                    if element == runtime().heap.known().na_string {
                        "NA".into()
                    } else {
                        format!("{:?}", utf8_text(element).unwrap_or_default())
                    }
                })
                .collect();
            vector_literal(elements)
        }
        SexpType::Integer => {
            let elements: Vec<String> = object
                .slice::<i32>()
                .iter()
                .map(|&value| {
                    if value == NA_INTEGER {
                        "NA".into()
                    } else {
                        format!("{value}L")
                    }
                })
                .collect();
            vector_literal(elements)
        }
        SexpType::Real => vector_literal(
            object
                .slice::<f64>()
                .iter()
                .map(|&value| format_real(value))
                .collect(),
        ),
        SexpType::Logical => vector_literal(
            object
                .slice::<i32>()
                .iter()
                .map(|&value| match value {
                    NA_INTEGER => "NA".to_owned(),
                    0 => "FALSE".to_owned(),
                    _ => "TRUE".to_owned(),
                })
                .collect(),
        ),
        SexpType::Builtin | SexpType::Special => format!(
            ".Primitive(\"{}\")",
            primitive_name(object.primitive_index())
        ),
        kind => format!("<{}>", kind.name()),
    }
}

fn vector_literal(elements: Vec<String>) -> String {
    if elements.len() == 1 {
        elements.into_iter().next().unwrap_or_default()
    } else {
        format!("c({})", elements.join(", "))
    }
}

/// The name of the interpreter's primitive with this index.
pub unsafe fn primitive_name(index: u32) -> &'static str {
    interp::interp()
        .builtin(index)
        .map_or("<unknown primitive>", |definition| definition.name)
}

/// Whether base R's namespace loader exists. A host that never loads base R (the C API tests)
/// still has the interpreter, but no `loadNamespace` to call.
pub unsafe fn namespaces_loadable() -> ApiResult<bool> {
    let symbol = runtime().heap.install(b"loadNamespace")?;
    let base = runtime().heap.known().base_env;
    Ok(crate::environments::find_var(symbol, base)? != runtime().heap.known().unbound)
}
