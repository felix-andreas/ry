//! `Rf_eval` and the primitives behind it.
//!
//! ry's interpreter comes later, so evaluation here covers what native code needs to drive
//! native code: constants evaluate to themselves, symbols look up their binding, promises are
//! forced, and a call evaluates when its function is one of the primitives below. Calling an R
//! closure is reported as unsupported rather than approximated.

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

pub fn primitive_name(index: u32) -> &'static str {
    PRIMITIVES
        .get(index as usize)
        .map_or("<unknown primitive>", |(name, _, _)| name)
}

pub unsafe fn eval(expression: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let known = *runtime.heap.known();
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
            runtime.heap.root(function);
            apply(expression, function, expression.cdr(), env)
        }
        SexpType::Dots => Err(error("'...' used in an incorrect context")),
        SexpType::Bytecode => Err(error("ry cannot evaluate GNU R bytecode")),
        _ => Ok(expression),
    }
}

pub unsafe fn force_promise(promise: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let unbound = runtime.heap.known().unbound;
    if promise.car() != unbound {
        return Ok(promise.car());
    }
    let value = eval(promise.cdr(), promise.tag())?;
    runtime.heap.set_car(promise, value);
    runtime.heap.set_tag(promise, runtime.nil());
    Ok(value)
}

unsafe fn apply(call: Sexp, function: Sexp, arguments: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    match function.kind() {
        SexpType::Builtin => {
            let mut evaluated = Vec::new();
            let mut cell = arguments;
            while cell.kind() == SexpType::Pairlist {
                let value = eval(cell.car(), env)?;
                runtime.heap.root(value);
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

/// `.External(.NAME, ..., PACKAGE)`: the routine receives the whole call as one pairlist, whose
/// first element is the routine's name.
unsafe fn dot_external(call: Sexp, arguments: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    native_call(call, arguments, RoutineKind::External)
}

unsafe fn native_call(
    call: Sexp,
    arguments: &[(Sexp, Sexp)],
    kind: RoutineKind,
) -> ApiResult<Sexp> {
    let what = match kind {
        RoutineKind::Call => ".Call",
        RoutineKind::External => ".External",
    };
    let Some(((_, name), rest)) = arguments.split_first() else {
        return Err(error_in(call, "'.NAME' is missing"));
    };
    let package_symbol = runtime().heap.install(b"PACKAGE")?;
    let mut package = None;
    let mut values = Vec::new();
    for &(tag, value) in rest {
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
            let runtime = runtime();
            let nil = runtime.nil();
            let mut list = nil;
            for &(tag, value) in values.iter().rev() {
                list = runtime
                    .heap
                    .alloc_triple(SexpType::Pairlist, value, list, tag)?;
                runtime.heap.root(list);
            }
            let list = runtime
                .heap
                .alloc_triple(SexpType::Language, *name, list, nil)?;
            runtime.heap.root(list);
            call_native(function, &[list], what)
        }
    }
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
    let dll = crate::dynload::load_library(&path)?;
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
    (runtime().console)(&text, true);
}

impl crate::boundary::Handout for Option<Sexp> {
    fn hand_out(self, runtime: &mut crate::Runtime) {
        if let Some(value) = self {
            value.hand_out(runtime);
        }
    }
}
