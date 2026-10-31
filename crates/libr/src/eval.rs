//! The C API's evaluation entry points (`Rf_eval`, `R_tryEval`, ...) and the interpreter's link to
//! native code. The interpreter (`crates/interp`) owns evaluation; this module installs it on the
//! runtime's heap and registers the builtins that need this crate: `.Call`, `.External`, `.C`,
//! `.Fortran` and the `dyn.load` family.

use crate::{
    Runtime,
    boundary::{ApiResult, DlFunc, Unwind, call_native, enter, error, error_in, report},
    dynload::{Dll, RoutineKind, find_routine},
    runtime,
    strings::utf8_text,
    vectors::xlength,
};
use heap::object::{Sexp, SexpType};
use interp::builtins::{Arg, BuiltinDef, Call, builtin, internal};
use std::ffi::{c_char, c_int, c_void};

/// Installs the interpreter on the runtime's heap. The runtime must be at its final address.
pub(crate) unsafe fn install_interpreter(runtime: &mut Runtime) {
    let hooks = interp::Hooks {
        write_console: |text, is_error| crate::boundary::write_console(text, is_error),
        materialize_altrep: |object| unsafe { crate::altrep::materialize(object) },
        run_finalizers: || unsafe { crate::boundary::run_pending_finalizers() },
    };
    interp::install(&mut runtime.heap, hooks);
    interp::register_builtins(NATIVE_BUILTINS);
    interp::register_builtins(BOOTSTRAP_BUILTINS);
}

/// Stand-ins for a few base R closures, so a host that never loads base R (the C API tests) can
/// still drive native code. Loading base R rebinds these names to base R's own closures.
const BOOTSTRAP_BUILTINS: &[BuiltinDef] = &[
    builtin("dyn.load", -1, |call| unsafe { do_dyn_load(call) }),
    builtin("stop", -1, |call| unsafe { Err(error_in(runtime().nil(), paste(call.args)?)) }),
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
    interp::eval::eval(expression, env)
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
    interp::eval::apply(runtime().nil(), function, &interp::eval::value_args(arguments), env)
}

/// A call as R prints it in an error message, or `None` for `NULL`.
pub unsafe fn deparse_call(call: Sexp) -> Option<String> {
    if call.kind() == SexpType::Nil {
        return None;
    }
    Some(interp::deparse::deparse_one_line(call))
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
        let result = interp::eval::eval(expression, env);
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
                            runtime().error_buffer = interp::interp().error_message.clone().into_bytes();
                        } else {
                            report(&unwind, "Error");
                        }
                    }
                    Unwind::Jump(_) => {
                        runtime().error_buffer = interp::interp().error_message.clone().into_bytes();
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
    enter(|| {
        let function = call.car();
        let function = if function.kind() == SexpType::Symbol {
            interp::eval::find_fun(function, env)?
        } else {
            eval(function, env)?
        };
        let args = interp::eval::promise_args(call.cdr(), env)?;
        for arg in args.iter().take(count.max(0) as usize) {
            if arg.value.kind() == SexpType::Promise {
                force_promise(arg.value)?;
            }
        }
        interp::eval::apply(call, function, &args, env)
    })
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

/// `R_ParseVector(text, n, status, srcfile)`: status 1 is `PARSE_OK`, 3 `PARSE_ERROR`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ParseVector(
    text: Sexp,
    count: c_int,
    status: *mut c_int,
    _srcfile: Sexp,
) -> Sexp {
    enter(|| {
        let lines: Vec<String> = interp::values::strings(text)
            .into_iter()
            .map(|line| line.unwrap_or_else(|| "NA".into()))
            .collect();
        match interp::lower::parse_text(&lines.join("\n"), "<text>") {
            Ok(expressions) => {
                if !status.is_null() {
                    *status = 1;
                }
                if count >= 0 && (count as usize) < interp::values::length(expressions) {
                    let truncated = interp::values::alloc(SexpType::Expression, count as usize)?;
                    for index in 0..count as usize {
                        interp::values::set_elt(truncated, index, expressions.element(index));
                    }
                    return Ok(truncated);
                }
                Ok(expressions)
            }
            Err(_) => {
                if !status.is_null() {
                    *status = 3;
                }
                Ok(runtime().nil())
            }
        }
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
    builtin(".Call", -1, |call| unsafe { dot_call(call, RoutineKind::Call) }),
    builtin(".External", -1, |call| unsafe { dot_call(call, RoutineKind::External) }),
    builtin(".External2", -1, |call| unsafe { dot_call(call, RoutineKind::External) }),
    builtin(".Call.graphics", -1, |call| unsafe { dot_call(call, RoutineKind::Call) }),
    builtin(".External.graphics", -1, |call| unsafe { dot_call(call, RoutineKind::External) }),
    builtin(".C", -1, |call| unsafe { dot_c(call, RoutineKind::C) }),
    builtin(".Fortran", -1, |call| unsafe { dot_c(call, RoutineKind::Fortran) }),
    internal("dyn.load", 4, |call| unsafe { do_dyn_load(call) }),
    internal("dyn.unload", 1, |_| Ok(unsafe { runtime().nil() })),
    internal("getLoadedDLLs", 0, |_| unsafe { loaded_dlls() }),
    internal("getRegisteredRoutines", 1, |call| unsafe { registered_routines(call.arg(0)) }),
    internal("getSymbolInfo", 3, |call| unsafe { do_get_symbol_info(call) }),
    internal("is.loaded", 3, |call| unsafe { do_is_loaded(call) }),
];

/// The arguments of a foreign call: `.NAME`, the routine's arguments, and `PACKAGE`.
struct ForeignArgs {
    name: Sexp,
    values: Vec<(Sexp, Sexp)>,
    package: Option<String>,
}

unsafe fn foreign_args(call: &Call<'_>) -> ApiResult<ForeignArgs> {
    let Some((first, rest)) = call.args.split_first() else {
        return Err(error_in(call.call, "'.NAME' is missing"));
    };
    let package_symbol = runtime().heap.install(b"PACKAGE")?;
    let ignored: Vec<Sexp> = ["NAOK", "DUP", "ENCODING"]
        .iter()
        .map(|name| runtime().heap.install(name.as_bytes()))
        .collect::<Result<_, _>>()?;
    let mut package = None;
    let mut values = Vec::new();
    for arg in rest {
        if arg.tag == package_symbol {
            if arg.value.kind() != SexpType::String || xlength(arg.value)? < 1 {
                return Err(error_in(call.call, "PACKAGE argument must be a single character string"));
            }
            package = Some(utf8_text(arg.value.element(0))?.into_owned());
        } else if ignored.contains(&arg.tag) {
            continue;
        } else {
            values.push((arg.tag, arg.value));
        }
    }
    Ok(ForeignArgs {
        name: first.value,
        values,
        package,
    })
}

/// The native routine `.NAME` names: a string, a `NativeSymbolInfo`, or an external pointer.
unsafe fn resolve_routine(
    call: Sexp,
    name: Sexp,
    package: Option<&str>,
    kind: RoutineKind,
) -> ApiResult<(DlFunc, Option<c_int>, String)> {
    match name.kind() {
        SexpType::String if xlength(name)? >= 1 => {
            let routine_name = utf8_text(name.element(0))?.into_owned();
            let (function, arity) = find_routine(&routine_name, package, kind).map_err(|unwind| match unwind {
                Unwind::Error(failure) => error_in(call, failure.message),
                other => other,
            })?;
            Ok((function, arity, routine_name))
        }
        SexpType::List => {
            let address = interp::values::list_get(name, "address");
            let routine_name = interp::values::list_get(name, "name");
            let routine_name = interp::values::first_string(routine_name)
                .map(|text| text.into_owned())
                .unwrap_or_default();
            let arity = interp::values::list_get(name, "numParameters");
            let arity = (arity != runtime().nil()).then(|| interp::values::as_integer_scalar(arity));
            let (function, _, _) = resolve_routine(call, address, package, kind)?;
            Ok((function, arity, routine_name))
        }
        SexpType::ExternalPtr => {
            let address = name.external_address();
            if address.is_null() {
                return Err(error_in(call, "NULL value passed as symbol address"));
            }
            Ok((std::mem::transmute::<*mut c_void, DlFunc>(address), None, String::new()))
        }
        _ => Err(error_in(
            call,
            "first argument must be a string (of length 1) or native symbol reference",
        )),
    }
}

/// `.Call` and `.External`.
unsafe fn dot_call(call: &Call<'_>, kind: RoutineKind) -> ApiResult<Sexp> {
    let what = match kind {
        RoutineKind::External => ".External",
        _ => ".Call",
    };
    let args = foreign_args(call)?;
    let (function, arity, routine_name) =
        resolve_routine(call.call, args.name, args.package.as_deref(), kind)?;
    match kind {
        RoutineKind::External => {
            let nil = runtime().nil();
            let mut list = nil;
            for &(tag, value) in args.values.iter().rev() {
                list = runtime().heap.alloc_triple(SexpType::Pairlist, value, list, tag)?;
                runtime().heap.root(list);
            }
            let list = runtime().heap.alloc_triple(SexpType::Language, args.name, list, nil)?;
            runtime().heap.root(list);
            call_native(function, &[list], what)
        }
        _ => {
            if args.values.len() > 65 {
                return Err(error_in(call.call, "too many arguments, sorry"));
            }
            if let Some(arity) = arity
                && arity >= 0
                && arity as usize != args.values.len()
            {
                return Err(error_in(
                    call.call,
                    format!(
                        "Incorrect number of arguments ({}), expecting {arity} for '{routine_name}'",
                        args.values.len()
                    ),
                ));
            }
            let values: Vec<Sexp> = args.values.iter().map(|(_, value)| *value).collect();
            call_native(function, &values, what)
        }
    }
}

unsafe extern "C" {
    fn ry_call_native(
        function: DlFunc,
        count: c_int,
        args: *const Sexp,
        result: *mut *mut heap::object::Header,
    ) -> c_int;
}

/// `.C` and `.Fortran`: each argument is copied into native memory, the routine receives
/// pointers, and the copies come back as a list.
unsafe fn dot_c(call: &Call<'_>, kind: RoutineKind) -> ApiResult<Sexp> {
    let args = foreign_args(call)?;
    let (function, _, _) = resolve_routine(call.call, args.name, args.package.as_deref(), kind)?;
    let mut copies = Vec::with_capacity(args.values.len());
    let mut strings: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut string_pointers: Vec<Vec<*mut c_char>> = Vec::new();
    for &(_, value) in &args.values {
        let copy = match value.kind() {
            SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex | SexpType::Raw => {
                let copy = interp::values::duplicate(value)?;
                runtime().heap.root(copy);
                copy
            }
            SexpType::String => value,
            other => {
                return Err(error_in(
                    call.call,
                    format!("invalid mode ({}) to pass to C or Fortran", other.name()),
                ));
            }
        };
        copies.push(copy);
    }
    let mut pointers: Vec<*mut c_void> = Vec::with_capacity(copies.len());
    for &copy in &copies {
        if copy.kind() == SexpType::String {
            let mut buffers: Vec<Vec<u8>> = (0..interp::values::length(copy))
                .map(|index| {
                    let mut bytes = copy.element(index).bytes().to_vec();
                    bytes.push(0);
                    bytes
                })
                .collect();
            let mut list: Vec<*mut c_char> = buffers.iter_mut().map(|buffer| buffer.as_mut_ptr() as *mut c_char).collect();
            pointers.push(list.as_mut_ptr() as *mut c_void);
            strings.push(buffers);
            string_pointers.push(list);
        } else {
            pointers.push(copy.data() as *mut c_void);
        }
    }
    let status = crate::boundary::with_native_scope(|| {
        let mut ignored = std::ptr::null_mut();
        ry_call_native(function, pointers.len() as c_int, pointers.as_ptr() as *const Sexp, &mut ignored)
    });
    status?;
    let result = runtime().heap.alloc_vector(SexpType::List, copies.len() as isize)?;
    runtime().heap.root(result);
    for (index, &copy) in copies.iter().enumerate() {
        runtime().heap.set_element(result, index, copy);
    }
    if args.values.iter().any(|(tag, _)| *tag != runtime().nil()) {
        let names: Vec<String> = args
            .values
            .iter()
            .map(|(tag, _)| {
                if tag.kind() == SexpType::Symbol {
                    interp::values::symbol_name(*tag).into_owned()
                } else {
                    String::new()
                }
            })
            .collect();
        let names = interp::values::string_vector(&names)?;
        interp::values::set_attr(result, interp::values::symbols().names, names)?;
    }
    Ok(result)
}

// The `dyn.load` family.

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
    let dll = crate::dynload::load_library(&path)?;
    dll_info(dll)
}

unsafe fn loaded_dlls() -> ApiResult<Sexp> {
    let mut items = vec![interp::native::dll_info("base", "base")?];
    let mut names = vec!["base".to_owned()];
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
unsafe fn native_symbol(name: &str, function: DlFunc, arity: c_int, dll: Sexp, class: &str) -> ApiResult<Sexp> {
    let nil = runtime().nil();
    let address = runtime().heap.alloc_external_ptr(function as *mut c_void, nil, nil)?;
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
        interp::values::first_string(interp::values::list_get(package, "name")).map(|text| text.into_owned())
    } else {
        None
    };
    for kind in [RoutineKind::Call, RoutineKind::External, RoutineKind::C, RoutineKind::Fortran] {
        if let Ok((function, arity)) = find_routine(&name, package_name.as_deref(), kind) {
            let dll = runtime()
                .dlls
                .iter_mut()
                .rev()
                .find(|dll| package_name.as_deref().is_none_or(|package| dll.name == package))
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
            return native_symbol(&name, function, arity.unwrap_or(-1), dll_object, class);
        }
    }
    Ok(runtime().nil())
}

unsafe fn do_is_loaded(call: &Call<'_>) -> ApiResult<Sexp> {
    let name = interp::values::require_string(call.arg(0), "symbol")?;
    let package = interp::values::first_string(call.arg(1)).map(|text| text.into_owned());
    let package = package.filter(|package| !package.is_empty());
    let found = [RoutineKind::Call, RoutineKind::External, RoutineKind::C, RoutineKind::Fortran]
        .into_iter()
        .any(|kind| find_routine(&name, package.as_deref(), kind).is_ok());
    interp::values::scalar_bool(found)
}

/// For tests that build a call by hand.
pub unsafe fn quote_error(message: &str) -> Unwind {
    error(message)
}

/// Runs an R script on ry's runtime the way `Rscript` does: starts the session (base R and the
/// default packages from the GNU R installation), evaluates the file, and returns the exit
/// status: 0 on success, 1 after an error, 2 when the session could not start. `arguments` are
/// what `commandArgs(trailingOnly = TRUE)` returns. This is `ry run --runtime ry`'s entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_run_file(
    path: *const c_char,
    count: c_int,
    arguments: *const *const c_char,
) -> c_int {
    let path = String::from_utf8_lossy(crate::strings::c_bytes(path)).into_owned();
    let mut command_args = vec!["ry".to_owned(), format!("--file={path}"), "--args".to_owned()];
    for index in 0..count.max(0) as usize {
        let argument = *arguments.add(index);
        command_args.push(String::from_utf8_lossy(crate::strings::c_bytes(argument)).into_owned());
    }
    runtime();
    let options = interp::session::StartOptions {
        command_args,
        vanilla: true,
    };
    if let Err(unwind) = interp::session::start(options) {
        interp::session::report_toplevel(unwind);
        crate::boundary::write_console(b"ry: base R did not load\n", true);
        return 2;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(failure) => {
            let message = format!("Fatal error: cannot open file '{path}': {failure}\n");
            crate::boundary::write_console(message.as_bytes(), true);
            return 2;
        }
    };
    let outcome = interp::session::run_text(&text, &path);
    crate::boundary::run_pending_finalizers();
    use std::io::Write;
    drop(std::io::stdout().flush());
    match outcome {
        interp::session::Outcome::Completed => 0,
        interp::session::Outcome::Failed => 1,
    }
}
