//! `Rf_eval` and the primitives behind it.
//!
//! Evaluation belongs to ry's interpreter, which lives in its own crate and plugs in through
//! [`Evaluator`] with [`install_evaluator`]. Every evaluation entry point of the C API
//! (`Rf_eval`, `R_tryEval`, `R_forceAndCall`, `R_ParseVector`, `Rf_PrintValue`, R-level
//! finalizers) goes through the installed evaluator. Until one is installed, a small built-in
//! evaluator covers what native code needs to drive native code: constants evaluate to
//! themselves, symbols look up their binding, promises are forced, and a call evaluates when its
//! function is one of the primitives below. Calling an R closure is then reported as unsupported
//! rather than approximated.

use crate::{
    boundary::{ApiResult, Unwind, call_native, enter, error, error_in, report, warn},
    dynload::{RoutineKind, find_routine},
    environments::{find_fun, find_var},
    globals::{R_ClassSymbol, R_DotsSymbol, R_NamesSymbol},
    runtime,
    strings::utf8_text,
    vectors::{element_as_charsxp, format_real, xlength},
};
use heap::{
    Heap,
    object::{NA_INTEGER, Sexp, SexpType},
    strings::Encoding,
};
use std::ffi::{c_char, c_int};

type Primitive = unsafe fn(call: Sexp, args: &[(Sexp, Sexp)], env: Sexp) -> ApiResult<Sexp>;

/// Builtins receive evaluated arguments, specials the unevaluated ones. Each argument comes
/// with its tag.
const PRIMITIVES: &[(&str, SexpType, Primitive)] = &[
    (".Call", SexpType::Builtin, dot_call),
    (".External", SexpType::Builtin, dot_external),
    (".C", SexpType::Builtin, dot_c_primitive),
    (".Fortran", SexpType::Builtin, dot_fortran_primitive),
    ("stop", SexpType::Builtin, stop),
    ("warning", SexpType::Builtin, warning),
    ("dyn.load", SexpType::Builtin, dyn_load),
    ("list", SexpType::Builtin, list),
    ("identity", SexpType::Builtin, identity),
    ("invisible", SexpType::Builtin, identity),
    ("quote", SexpType::Special, quote),
];

pub(crate) unsafe fn install_primitives(heap: &mut Heap) {
    let base = heap.known().base_env;
    for (index, (name, kind, _)) in PRIMITIVES.iter().enumerate() {
        let result = (|| -> heap::HeapResult<()> {
            let primitive = heap.alloc_primitive(*kind, index as u32)?;
            let symbol = heap.install(name.as_bytes())?;
            heap.define(base, symbol, primitive)
        })();
        if let Err(error) = result {
            panic!("cannot install the primitive `{name}`: {error}");
        }
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
    unsafe fn apply(&self, call: Sexp, function: Sexp, arguments: Sexp, env: Sexp)
    -> ApiResult<Sexp>;

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

pub fn primitive_name(index: u32) -> &'static str {
    PRIMITIVES
        .get(index as usize)
        .map_or("<unknown primitive>", |(name, _, _)| name)
}

/// Evaluates through the installed [`Evaluator`], or the built-in one when none is installed.
pub unsafe fn eval(expression: Sexp, env: Sexp) -> ApiResult<Sexp> {
    if let Some(evaluator) = evaluator() {
        return evaluator.eval(expression, env);
    }
    builtin_eval(expression, env)
}

unsafe fn builtin_eval(expression: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let known = *runtime().heap.known();
    match expression.kind() {
        SexpType::Symbol => {
            if expression == R_DotsSymbol.get() {
                return Err(error("'...' used in an incorrect context"));
            }
            if env.kind() != SexpType::Environment {
                return Err(error(
                    "'rho' must be an environment not a ".to_owned() + env.kind().name(),
                ));
            }
            let value = find_var(expression, env)?;
            if value == known.unbound {
                return Err(error(format!(
                    "object '{}' not found",
                    utf8_text(expression.car())?
                )));
            }
            if value == known.missing_arg {
                return Err(error(format!(
                    "argument \"{}\" is missing, with no default",
                    utf8_text(expression.car())?
                )));
            }
            if value.kind() == SexpType::Promise {
                return force_promise(value);
            }
            Ok(value)
        }
        SexpType::Promise => force_promise(expression),
        SexpType::Language => {
            let head = expression.car();
            let function = if head.kind() == SexpType::Symbol {
                find_fun(head, env)?
            } else {
                eval(head, env)?
            };
            runtime().heap.root(function);
            apply(expression, function, expression.cdr(), env)
        }
        SexpType::Dots => Err(error("'...' used in an incorrect context")),
        SexpType::Bytecode => Err(error("ry cannot evaluate GNU R bytecode")),
        _ => Ok(expression),
    }
}

pub unsafe fn force_promise(promise: Sexp) -> ApiResult<Sexp> {
    let unbound = runtime().heap.known().unbound;
    if promise.car() != unbound {
        return Ok(promise.car());
    }
    let value = eval(promise.cdr(), promise.tag())?;
    runtime().heap.set_car(promise, value);
    runtime().heap.set_tag(promise, runtime().nil());
    Ok(value)
}

unsafe fn apply(call: Sexp, function: Sexp, arguments: Sexp, env: Sexp) -> ApiResult<Sexp> {
    match function.kind() {
        SexpType::Builtin => {
            let mut evaluated = Vec::new();
            let mut cell = arguments;
            while cell.kind() == SexpType::Pairlist {
                let value = eval(cell.car(), env)?;
                runtime().heap.root(value);
                evaluated.push((cell.tag(), value));
                cell = cell.cdr();
            }
            invoke(call, function, &evaluated, env)
        }
        SexpType::Special => {
            let mut unevaluated = Vec::new();
            let mut cell = arguments;
            while cell.kind() == SexpType::Pairlist {
                unevaluated.push((cell.tag(), cell.car()));
                cell = cell.cdr();
            }
            invoke(call, function, &unevaluated, env)
        }
        SexpType::Closure => Err(error_in(
            call,
            "ry cannot call R closures yet; the interpreter is not built",
        )),
        _ => Err(error_in(call, "attempt to apply non-function")),
    }
}

unsafe fn invoke(
    call: Sexp,
    function: Sexp,
    arguments: &[(Sexp, Sexp)],
    env: Sexp,
) -> ApiResult<Sexp> {
    let (_, _, primitive) = PRIMITIVES
        .get(function.primitive_index() as usize)
        .ok_or_else(|| error("invalid primitive"))?;
    primitive(call, arguments, env)
}

/// Calls a function with already evaluated arguments, as a finalizer call does.
pub unsafe fn apply_closure_or_primitive(
    function: Sexp,
    arguments: &[Sexp],
    env: Sexp,
) -> ApiResult<Sexp> {
    let runtime = runtime();
    let nil = runtime.nil();
    let call = crate::lists::list_of(SexpType::Language, &[function])?;
    runtime.heap.root(call);
    if let Some(evaluator) = evaluator() {
        let mut list = nil;
        for &value in arguments.iter().rev() {
            list = runtime.heap.cons(value, list)?;
            runtime.heap.root(list);
        }
        return evaluator.apply(call, function, list, env);
    }
    match function.kind() {
        SexpType::Builtin | SexpType::Special => {
            let tagged: Vec<(Sexp, Sexp)> = arguments.iter().map(|&value| (nil, value)).collect();
            invoke(call, function, &tagged, env)
        }
        _ => apply(call, function, nil, env),
    }
}

/// `.Call(.NAME, ..., PACKAGE)`.
unsafe fn dot_call(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    native_call(call, arguments, RoutineKind::Call)
}

unsafe fn dot_c_primitive(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    native_call(call, arguments, RoutineKind::C)
}

unsafe fn dot_fortran_primitive(
    call: Sexp,
    arguments: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    native_call(call, arguments, RoutineKind::Fortran)
}

/// `.External(.NAME, ..., PACKAGE)`: the routine receives the whole call as one pairlist, whose
/// first element is the routine's name.
unsafe fn dot_external(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    native_call(call, arguments, RoutineKind::External)
}

/// `.Call` and `.External` for the interpreter: `arguments` are the evaluated arguments with
/// their tags, the first being `.NAME`.
pub unsafe fn native_call(
    call: Sexp,
    arguments: &[(Sexp, Sexp)],
    kind: RoutineKind,
) -> ApiResult<Sexp> {
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
        if matches!(kind, RoutineKind::C | RoutineKind::Fortran) && control_symbols.contains(&tag)
        {
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
            package = Some(utf8_text(value.element(0))?.into_owned());
        } else {
            values.push((tag, value));
        }
    }
    let (function, arity, routine_name) = match name.kind() {
        SexpType::String if xlength(*name)? >= 1 => {
            let routine_name = utf8_text(name.element(0))?.into_owned();
            let (function, arity) = find_routine(&routine_name, package.as_deref(), kind).map_err(
                |unwind| match unwind {
                    Unwind::Error(error) => error_in(call, error.message),
                    other => other,
                },
            )?;
            (function, arity, routine_name)
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
                .alloc_triple(SexpType::Language, *name, list, nil)?;
            runtime().heap.root(list);
            call_native(function, &[list], what)
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
            dot_c(call, function, &values, kind, na_ok)
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
unsafe fn dot_c(
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
                if !na_ok && !c_values_finite(value)? {
                    return Err(error_in(
                        call,
                        format!("NA/NaN/Inf in foreign function call (arg {})", position + 1),
                    ));
                }
                let copy = crate::memory::duplicate(value, false)?;
                runtime.heap.root(copy);
                pointers.push(crate::vectors::data_pointer(copy, true)? as *mut std::ffi::c_void);
                CArgument::Vector(copy)
            }
            SexpType::String => {
                let length = xlength(value)? as usize;
                let mut strings = Vec::with_capacity(length);
                for index in 0..length {
                    let element = value.element(index);
                    if !na_ok && element == runtime.heap.known().na_string {
                        return Err(error_in(
                            call,
                            format!("NA/NaN/Inf in foreign function call (arg {})", position + 1),
                        ));
                    }
                    strings.push(crate::strings::arena_c_string(element.bytes())? as *mut c_char);
                }
                let copy = crate::memory::duplicate(value, false)?;
                runtime.heap.root(copy);
                let pointer = if kind == RoutineKind::Fortran {
                    strings.first().copied().unwrap_or(std::ptr::null_mut()) as *mut std::ffi::c_void
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
                    format!("invalid mode ({}) to pass to Fortran (arg {})", other.name(), position + 1),
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

/// `stop(...)`: the arguments pasted together become the message.
unsafe fn stop(_call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    Err(error(paste(arguments)?))
}

unsafe fn warning(_call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let message = paste(arguments)?;
    warn(message.clone());
    let charsxp = runtime()
        .heap
        .mk_char(message.as_bytes(), Encoding::Native)?;
    crate::memory::scalar_string(charsxp)
}

unsafe fn paste(arguments: &[(Sexp, Sexp)]) -> ApiResult<String> {
    let mut message = String::new();
    for &(_, value) in arguments {
        if value.kind().is_atomic() {
            for index in 0..xlength(value)? as usize {
                message.push_str(&utf8_text(element_as_charsxp(value, index)?)?);
            }
        }
    }
    Ok(message)
}

unsafe fn dyn_load(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let Some(&(_, path)) = arguments.first() else {
        return Err(error_in(call, "argument \"x\" is missing, with no default"));
    };
    if path.kind() != SexpType::String || xlength(path)? != 1 {
        return Err(error_in(call, "character argument expected"));
    }
    let path = utf8_text(path.element(0))?.into_owned();
    let local_symbol = runtime().heap.install(b"local")?;
    let mut local = true;
    for (position, &(tag, value)) in arguments.iter().enumerate().skip(1) {
        if tag == local_symbol || (position == 1 && tag == runtime().nil()) {
            local = crate::vectors::as_logical(value)? != 0;
        }
    }
    let dll = crate::dynload::load_library(&path, local)?;
    crate::dynload::dll_info_object(dll)
}

unsafe fn list(_call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let nil = runtime.nil();
    let result = runtime
        .heap
        .alloc_vector(SexpType::List, arguments.len() as isize)?;
    runtime.heap.root(result);
    for (index, &(_, value)) in arguments.iter().enumerate() {
        runtime.heap.set_element(result, index, value);
    }
    if arguments.iter().any(|(tag, _)| *tag != nil) {
        let names = runtime
            .heap
            .alloc_vector(SexpType::String, arguments.len() as isize)?;
        runtime.heap.root(names);
        for (index, &(tag, _)) in arguments.iter().enumerate() {
            if tag.kind() == SexpType::Symbol {
                runtime.heap.set_element(names, index, tag.car());
            }
        }
        crate::attributes::install_attribute(result, R_NamesSymbol.get(), names)?;
    }
    Ok(result)
}

unsafe fn identity(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    match arguments {
        [(_, value)] => Ok(*value),
        _ => Err(error_in(
            call,
            format!(
                "{} arguments passed to a function that takes 1",
                arguments.len()
            ),
        )),
    }
}

unsafe fn quote(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    match arguments {
        [(_, expression)] => Ok(*expression),
        _ => Err(error_in(
            call,
            format!(
                "{} arguments passed to 'quote' which requires 1",
                arguments.len()
            ),
        )),
    }
}

/// An unwind as the condition object R handlers receive: a list of `message` and `call` with
/// the condition classes.
pub unsafe fn make_condition(unwind: &Unwind) -> ApiResult<Sexp> {
    let runtime = runtime();
    let (message, call, classes): (&str, Sexp, &[&str]) = match unwind {
        Unwind::Error(error) => (
            &error.message,
            error.call,
            &["simpleError", "error", "condition"],
        ),
        Unwind::Interrupt => ("", runtime.nil(), &["interrupt", "condition"]),
    };
    let condition = runtime.heap.alloc_vector(SexpType::List, 2)?;
    runtime.heap.root(condition);
    let text = runtime.heap.mk_char(message.as_bytes(), Encoding::Utf8)?;
    let text = crate::memory::scalar_string(text)?;
    runtime.heap.set_element(condition, 0, text);
    runtime.heap.set_element(condition, 1, call);
    let names = runtime.heap.alloc_vector(SexpType::String, 2)?;
    runtime.heap.root(names);
    for (index, name) in ["message", "call"].iter().enumerate() {
        let name = runtime.heap.mk_char(name.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(names, index, name);
    }
    crate::attributes::install_attribute(condition, R_NamesSymbol.get(), names)?;
    let class = runtime
        .heap
        .alloc_vector(SexpType::String, classes.len() as isize)?;
    runtime.heap.root(class);
    for (index, name) in classes.iter().enumerate() {
        let name = runtime.heap.mk_char(name.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(class, index, name);
    }
    crate::attributes::install_attribute(condition, R_ClassSymbol.get(), class)?;
    condition.set_object(true);
    Ok(condition)
}

/// A call as R prints it in an error message, or `None` for `NULL`.
pub unsafe fn deparse_call(call: Sexp) -> Option<String> {
    if call.kind() == SexpType::Nil {
        return None;
    }
    Some(deparse(call, 0))
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_eval(expression: Sexp, env: Sexp) -> Sexp {
    enter(|| eval(expression, env))
}

unsafe fn try_eval(expression: Sexp, env: Sexp, failed: *mut c_int, silent: bool) -> Option<Sexp> {
    enter(|| {
        let result = eval(expression, env);
        if !failed.is_null() {
            *failed = result.is_err() as c_int;
        }
        match result {
            Ok(value) => Ok(Some(value)),
            Err(unwind) => {
                if silent {
                    let text = match &unwind {
                        Unwind::Error(error) => crate::boundary::format_error("Error", error),
                        Unwind::Interrupt => String::new(),
                    };
                    runtime().error_buffer = text.into_bytes();
                } else {
                    report(&unwind, "Error");
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

unsafe fn installed_evaluator(what: &str) -> ApiResult<&'static dyn Evaluator> {
    evaluator().ok_or_else(|| error(format!("{what} needs ry's interpreter, which is not loaded")))
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_PrintValue(value: Sexp) {
    enter(|| {
        let global = runtime().heap.known().global_env;
        installed_evaluator("Rf_PrintValue")?.print_value(value, global)
    })
}

/// `R_forceAndCall(call, n, rho)`: evaluates `call`, forcing the promises of the first `n`
/// arguments before the function body runs. Without an interpreter every argument is evaluated
/// eagerly anyway, which is the same thing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_forceAndCall(call: Sexp, count: c_int, env: Sexp) -> Sexp {
    enter(|| match evaluator() {
        Some(evaluator) => evaluator.force_and_call(call, count, env),
        None => builtin_eval(call, env),
    })
}

/// Evaluates `package::function(arguments...)` in the global environment, which loads the
/// package's namespace when needed. The C API uses this for entry points that GNU R implements
/// by calling R code, such as the S4 class functions that call into `methods`.
pub unsafe fn call_namespaced(package: &str, function: &str, arguments: &[Sexp]) -> ApiResult<Sexp> {
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
    let call = runtime.heap.alloc_triple(SexpType::Language, head, list, nil)?;
    runtime.heap.root(call);
    let global = runtime.heap.known().global_env;
    installed_evaluator(&format!("{}::{}", utf8_text(package.car())?, utf8_text(function.car())?))?;
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
