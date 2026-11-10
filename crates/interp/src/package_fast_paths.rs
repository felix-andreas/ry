//! Native fast paths for hot closures of installed packages, made the way `base_fast_paths.rs`
//! makes them for base: a one-call prologue in front of the closure's body that either finishes
//! the call with exactly the value the R body would return, or changes nothing observable and lets
//! the R body run. Each fast path follows one release of the package, so it is installed only when
//! the loaded function's formals and body hash to the recorded value.
//!
//! `loadNamespace` installs them through `ryPackageFastPaths(package, env)`, which ry's copy of
//! `registerS3methods` calls once the namespace's code is loaded.

use {
    crate::{
        base_fast_paths::{ClosureFrame, argument, decline, finish, install_prologue, kernel},
        builtins::{BuiltinDef, Call, internal},
        envir::{self, frame_of, get_option, sys_parent_number, top_env},
        eval, interp,
        unwind::R,
        values::{self, global_env, install, length, na_string, nil, symbols},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType},
    smallvec::SmallVec,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryPackageFastPaths", 2, do_package_fast_paths).invisible(),
    internal("ryTraceBack", 0, do_trace_back),
    internal("ryWithrListCombine", 0, do_withr_list_combine),
    internal("ryWithrAsEnvvars", 0, do_withr_as_envvars),
    internal("ryWithrGetEnvvar", 0, do_withr_get_envvar),
    internal("ryWithrSetEnvvar", 0, do_withr_set_envvar),
    internal("ryWithrGetOptions", 0, do_withr_get_options),
    internal("ryWithrSetOptions", 0, do_withr_set_options),
    internal("ryWithrResetOptions", 0, do_withr_reset_options),
    internal("ryWithrResetGettextCache", 0, do_withr_reset_gettext_cache),
];

/// The package functions that get a fast path: package, the one release whose code the native
/// version follows, function, the internal the prologue calls, and the
/// [`crate::session::stable_hash`] of that release's formals and body. Any other release hashes
/// differently and keeps its R body; `crates/ry/tests/scenarios/native-package-functions.scenario`
/// fails when the installed release is not the one listed, so an update shows up in the test
/// suite rather than as a silent slowdown. `RY_PRINT_FAST_PATH_HASHES=1` prints the hashes of the
/// loaded functions.
const FAST_PATHS: &[(&str, &str, &str, &str, u64)] = &[
    (
        "rlang",
        "1.3.0",
        "trace_back",
        "ryTraceBack",
        0x41ea91d219cd95a7,
    ),
    (
        "withr",
        "3.0.3",
        "list_combine",
        "ryWithrListCombine",
        0x96ded904a19a25c1,
    ),
    (
        "withr",
        "3.0.3",
        "as_envvars",
        "ryWithrAsEnvvars",
        0x40a3abb5a00b5204,
    ),
    (
        "withr",
        "3.0.3",
        "get_envvar",
        "ryWithrGetEnvvar",
        0x885db5dfca63aa94,
    ),
    (
        "withr",
        "3.0.3",
        "set_envvar",
        "ryWithrSetEnvvar",
        0xaa5e7d53355e6593,
    ),
    (
        "withr",
        "3.0.3",
        "get_options",
        "ryWithrGetOptions",
        0xc8cc10869f10f45b,
    ),
    (
        "withr",
        "3.0.3",
        "set_options",
        "ryWithrSetOptions",
        0x84f8ff73c6a4bff0,
    ),
    (
        "withr",
        "3.0.3",
        "reset_options",
        "ryWithrResetOptions",
        0xf18c31a44631e557,
    ),
    (
        "withr",
        "3.0.3",
        "reset_gettext_cache",
        "ryWithrResetGettextCache",
        0xb1b537bb7c8f99b7,
    ),
];

/// `ryPackageFastPaths(package, env)`: installs the fast paths of `package` into its namespace.
unsafe fn do_package_fast_paths(call: &Call<'_>) -> R<Sexp> {
    let Some(package) = values::first_string(call.arg(0)) else {
        return Ok(nil());
    };
    let namespace = call.arg(1);
    if namespace.kind() != SexpType::Environment {
        return Ok(nil());
    }
    let print = std::env::var_os("RY_PRINT_FAST_PATH_HASHES").is_some();
    for &(owner, _release, name, internal_name, expected) in FAST_PATHS {
        if owner != package {
            continue;
        }
        let binding = eval::find_var_in_frame(install(name)?, namespace)?;
        if binding == values::unbound() {
            continue;
        }
        let function = eval::value_of(binding)?;
        install_prologue(function, name, internal_name, expected, print)?;
    }
    Ok(nil())
}

/// rlang 1.3.0's `trace_back(top = NULL, bottom = NULL)`: the backtrace of the calls that led to
/// the caller, as an `rlang_trace` data frame with the columns `call`, `parent`, `visible`,
/// `namespace` and `scope`. testthat records one for every error and warning an expectation
/// catches, and the R body spends most of its time in the closures that classify each frame
/// (`call_trace_context`) and in the data frame slicing that trims frames above the test.
///
/// It declines when one of rlang's debugging options is set, when `bottom` is not `NULL` or a
/// frame on the stack, or when a frame's call is one the R body treats specially (a quosure, a
/// call whose function is an unforced promise, or one the R body would fail on), so that the R
/// body runs and does what it does.
unsafe fn do_trace_back(call: &Call<'_>) -> R<Sexp> {
    let frame = call.env()?;
    let namespace = frame.enclosure();
    if get_option("rlang:::visible_bottom") != nil() {
        return decline();
    }
    let Some(own) = frame_of(frame) else {
        return decline();
    };
    let depth = own + 1;
    let caller = sys_parent_number(1, Some(own)).max(0) as usize;
    let Some(bottom) = argument(ClosureFrame::Env(frame), symbols().bottom)? else {
        return decline();
    };
    let count = if bottom == nil() {
        caller
    } else if bottom.kind() == SexpType::Environment {
        match interp().frames[..depth]
            .iter()
            .position(|entry| entry.owns(bottom))
        {
            Some(index) => index + 1,
            None if bottom == global_env() => 0,
            None => return decline(),
        }
    } else {
        return decline();
    };
    if get_option("rlang:::error_frame") != nil() {
        return decline();
    }
    for index in 0..count {
        let head = interp().frames[index].call.car();
        if head.kind() == SexpType::Promise && head.car() == values::unbound() {
            return decline();
        }
    }

    let mut calls = Vec::with_capacity(count);
    let mut contexts = Vec::with_capacity(count);
    for index in 0..count {
        let entry = &interp().frames[index];
        let function = entry.function;
        let copy = envir::call_with_srcref(entry)?;
        if copy.is_object() {
            return decline();
        }
        let head = copy.car();
        if head.kind() == SexpType::Promise {
            crate::heap().set_car(copy, head.car());
        }
        let Some(zapped) = zap_call(copy, namespace, true)? else {
            return decline();
        };
        let Some(context) = trace_context(zapped, function)? else {
            return decline();
        };
        calls.push(zapped);
        contexts.push(context);
    }

    let top = match argument(ClosureFrame::Env(frame), symbols().top)? {
        Some(value) if value != nil() => value,
        _ => get_option("rlang_trace_top_env"),
    };
    if top == nil() && is_true(get_option("knitr.in.progress")) {
        return decline();
    }
    let start = if top == nil() {
        0
    } else {
        interp().frames[..count]
            .iter()
            .rposition(|entry| entry.owns(top))
            .map_or(0, |index| index + 1)
    };

    let kept = count - start;
    let visible_through = caller.min(count);
    let call_column = values::alloc(SexpType::List, kept)?;
    let parent_column = values::alloc(SexpType::Integer, kept)?;
    let visible_column = values::alloc(SexpType::Logical, kept)?;
    // `do.call(c, ...)` over the per-frame values gives a logical column when every one is `NA`,
    // and trimming keeps the column's type. Without frames the columns are `character(0)`.
    let any_namespace = contexts.iter().any(|context| context.namespace.is_some());
    let any_scope = contexts.iter().any(|context| context.scope.is_some());
    let namespace_kind = if any_namespace || count == 0 {
        SexpType::String
    } else {
        SexpType::Logical
    };
    let scope_kind = if any_scope || count == 0 {
        SexpType::String
    } else {
        SexpType::Logical
    };
    let namespace_column = values::alloc(namespace_kind, kept)?;
    let scope_column = values::alloc(scope_kind, kept)?;
    for position in 0..kept {
        let index = start + position;
        crate::heap().set_element(call_column, position, calls[index]);
        let parent = sys_parent_number((depth - index) as i64, Some(own));
        let parent = if parent == index as i64 + 1 {
            0
        } else {
            parent
        };
        let parent = if parent > start as i64 {
            (parent - start as i64) as i32
        } else {
            0
        };
        values::ints_mut(parent_column)[position] = parent;
        values::ints_mut(visible_column)[position] = i32::from(index < visible_through);
        let context = &contexts[index];
        set_text(namespace_column, position, context.namespace)?;
        set_text(scope_column, position, context.scope.map(Scope::text))?;
    }

    let trace = values::list_vector(&[
        call_column,
        parent_column,
        visible_column,
        namespace_column,
        scope_column,
    ])?;
    let names = values::string_vector(&["call", "parent", "visible", "namespace", "scope"])?;
    values::set_attr(trace, symbols().names, names)?;
    let row_names = if kept == 0 {
        values::integer_vector(&[])?
    } else {
        values::integer_vector(&[NA_INTEGER, -(kept as i32)])?
    };
    values::set_attr(trace, symbols().row_names, row_names)?;
    values::set_attr(trace, symbols().version, values::scalar_integer(2)?)?;
    let class = values::string_vector(&["rlang_trace", "rlib_trace", "tbl", "data.frame"])?;
    values::set_attr(trace, symbols().class, class)?;
    finish(ClosureFrame::Env(frame), trace, true)
}

/// A frame's entry in the `namespace` and `scope` columns; `None` is `NA`. The namespace is a
/// CHARSXP.
struct TraceContext {
    namespace: Option<Sexp>,
    scope: Option<Scope>,
}

#[derive(Clone, Copy)]
enum Scope {
    Global,
    Exported,
    Internal,
    Local,
}

impl Scope {
    fn text(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Exported => "::",
            Scope::Internal => ":::",
            Scope::Local => "local",
        }
    }
}

const NO_CONTEXT: TraceContext = TraceContext {
    namespace: None,
    scope: None,
};

unsafe fn set_text<T: Into<TextValue>>(column: Sexp, position: usize, value: Option<T>) -> R<()> {
    if column.kind() == SexpType::Logical {
        values::ints_mut(column)[position] = NA_LOGICAL;
        return Ok(());
    }
    let charsxp = match value.map(Into::into) {
        None => na_string(),
        Some(TextValue::Char(charsxp)) => charsxp,
        Some(TextValue::Static(text)) => values::mkchar(text)?,
    };
    crate::heap().set_element(column, position, charsxp);
    Ok(())
}

enum TextValue {
    Char(Sexp),
    Static(&'static str),
}

impl From<Sexp> for TextValue {
    fn from(charsxp: Sexp) -> Self {
        TextValue::Char(charsxp)
    }
}

impl From<&'static str> for TextValue {
    fn from(text: &'static str) -> Self {
        TextValue::Static(text)
    }
}

/// rlang's `call_trace_context(call, fn)` for the call of a frame whose function is `function`;
/// `None` when the R body would raise an error.
unsafe fn trace_context(call: Sexp, function: Sexp) -> R<Option<TraceContext>> {
    match fine_type(call) {
        FineType::Call => {}
        FineType::Other => return Ok(Some(NO_CONTEXT)),
        FineType::Unknown => return Ok(None),
    }
    let head = call.car();
    if head.kind() == SexpType::Language && is_namespace_operator(head) {
        let rest = head.cdr();
        if rest == nil() {
            return Ok(None);
        }
        let Some(namespace) = string_of(rest.car()) else {
            return Ok(None);
        };
        let scope = if head.car() == symbols().double_colon {
            Scope::Exported
        } else {
            Scope::Internal
        };
        return Ok(Some(TraceContext {
            namespace: Some(namespace),
            scope: Some(scope),
        }));
    }
    let name = match head.kind() {
        SexpType::Symbol => head,
        SexpType::Language | SexpType::Closure | SexpType::Builtin | SexpType::Special => {
            return Ok(Some(NO_CONTEXT));
        }
        _ => return Ok(None),
    };
    let env = if function.kind() == SexpType::Closure {
        function.tag()
    } else {
        values::base_namespace()
    };
    if env == global_env() {
        return Ok(Some(TraceContext {
            namespace: None,
            scope: Some(Scope::Global),
        }));
    }
    let top = top_env(env, get_option("topLevelEnvironment"));
    if !envir::is_namespace(top) {
        return Ok(Some(NO_CONTEXT));
    }
    let Some(namespace) = namespace_charsxp(top)? else {
        return Ok(None);
    };
    let scope = if is_exported(top, name)? {
        Scope::Exported
    } else if eval::find_var_in_frame(name, top)? != values::unbound() {
        Scope::Internal
    } else {
        Scope::Local
    };
    Ok(Some(TraceContext {
        namespace: Some(namespace),
        scope: Some(scope),
    }))
}

/// Whether `call` is a call to `::` or `:::`.
unsafe fn is_namespace_operator(call: Sexp) -> bool {
    let head = call.car();
    head.kind() == SexpType::Symbol && matches!(head.car().bytes(), b"::" | b":::")
}

/// rlang's `is_true(x)`: `identical(x, TRUE)`.
unsafe fn is_true(value: Sexp) -> bool {
    value.kind() == SexpType::Logical
        && length(value) == 1
        && value.attrib() == nil()
        && values::ints(value)[0] == 1
}

/// rlang's `as_string()` of a symbol or a single non-`NA` string, as its CHARSXP.
unsafe fn string_of(value: Sexp) -> Option<Sexp> {
    match value.kind() {
        SexpType::Symbol => Some(value.car()),
        SexpType::String if length(value) == 1 && value.attrib() == nil() => {
            let text = values::string_elt(value, 0);
            (text != na_string()).then_some(text)
        }
        _ => None,
    }
}

/// `unname(getNamespaceName(top))` as a CHARSXP.
unsafe fn namespace_charsxp(top: Sexp) -> R<Option<Sexp>> {
    if top == values::base_namespace() {
        return Ok(Some(values::mkchar("base")?));
    }
    let Some(cell) = crate::heap().frame_cell(top, symbols().dot_namespace) else {
        return Ok(None);
    };
    let info = eval::value_of(cell.cdr())?;
    if info.kind() != SexpType::Environment {
        return Ok(None);
    }
    let spec = eval::value_of(eval::find_var_in_frame(symbols().spec, info)?)?;
    if spec.kind() != SexpType::String || length(spec) == 0 {
        return Ok(None);
    }
    Ok(Some(values::string_elt(spec, 0)))
}

/// rlang's `ns_exports_has(ns, name)`.
unsafe fn is_exported(namespace: Sexp, name: Sexp) -> R<bool> {
    let exports = if namespace == values::base_namespace() {
        values::base_env()
    } else {
        let Some(cell) = crate::heap().frame_cell(namespace, symbols().dot_namespace) else {
            return Ok(false);
        };
        let info = eval::value_of(cell.cdr())?;
        if info.kind() != SexpType::Environment {
            return Ok(false);
        }
        let exports = eval::find_var_in_frame(symbols().exports, info)?;
        if exports == values::unbound() {
            return Ok(false);
        }
        eval::value_of(exports)?
    };
    if exports.kind() != SexpType::Environment {
        return Ok(false);
    }
    Ok(eval::find_var_in_frame(name, exports)? != values::unbound())
}

enum FineType {
    /// `call_print_fine_type()` is `"call"`.
    Call,
    /// An operator form: the R body gives the frame no context.
    Other,
    /// An operator `call_print_fine_type()` has no entry for, where the R body fails.
    Unknown,
}

/// Whether rlang's `call_print_fine_type(call)` is `"call"`, following `r_which_operator()` in
/// rlang's `parse.c` and the switch in `call.R`.
unsafe fn fine_type(call: Sexp) -> FineType {
    let head = call.car();
    if head.kind() != SexpType::Symbol {
        return FineType::Call;
    }
    let name = head.car().bytes();
    let arguments = values::pairlist_length(call.cdr());
    let unary = arguments < 2;
    // Operators that print in infix form with exactly two arguments and as a call otherwise.
    let binary = || {
        if arguments == 2 {
            FineType::Other
        } else {
            FineType::Call
        }
    };
    match name {
        b"break" | b"next" => FineType::Unknown,
        b"for" | b"function" | b"if" | b"repeat" | b"while" => FineType::Other,
        b"?" | b"~" | b"+" | b"-" => {
            if unary {
                FineType::Other
            } else {
                binary()
            }
        }
        b"<-" | b"<<-" | b"=" | b"::" | b":::" | b"$" | b"@" => FineType::Other,
        b"!" | b"!!" | b"!!!" => FineType::Other,
        b"(" | b"{" | b"[" | b"[[" => FineType::Other,
        b"<" | b"<=" | b">" | b">=" | b"==" | b"!=" | b":" | b":=" | b"|" | b"||" | b"&"
        | b"&&" | b"*" | b"/" | b"^" | b"%%" => binary(),
        _ if name.len() >= 3 && name[0] == b'%' && name[name.len() - 1] == b'%' => binary(),
        _ => FineType::Call,
    }
}

/// rlang's `call_zap_inline()` on a frame's call: every argument that is not a symbol, a call, or
/// a scalar literal without attributes becomes the symbol `call_type_sum()` names, such as
/// `<fn>` or `<df[,3]>`, recursively through nested calls and `function` expressions, whose
/// srcref is dropped. Nested calls and pairlists are copied first, as `r_call_clone()` does, so
/// the frame's own call is left as it was. `fresh` says `call` is already a copy. `None` stands
/// for a `function` expression without the srcref slot, which rlang's C code fails on.
unsafe fn zap_call(call: Sexp, namespace: Sexp, fresh: bool) -> R<Option<Sexp>> {
    let call = if fresh {
        call
    } else {
        values::shallow_duplicate(call)?
    };
    crate::heap().root(call);
    if call.car() != symbols().function {
        return Ok(zap_cells(call, namespace)?.then_some(call));
    }
    let formals_cell = call.cdr();
    if formals_cell == nil() {
        return Ok(None);
    }
    let formals = formals_cell.car();
    if formals.kind() == SexpType::Pairlist {
        let copy = values::shallow_duplicate(formals)?;
        crate::heap().root(copy);
        crate::heap().set_car(formals_cell, copy);
        if !zap_cells(copy, namespace)? {
            return Ok(None);
        }
    }
    let body_cell = formals_cell.cdr();
    if body_cell == nil() {
        return Ok(None);
    }
    let Some(body) = zap_one(body_cell.car(), namespace)? else {
        return Ok(None);
    };
    crate::heap().set_car(body_cell, body);
    let srcref_cell = body_cell.cdr();
    if srcref_cell == nil() {
        return Ok(None);
    }
    crate::heap().set_car(srcref_cell, nil());
    Ok(Some(call))
}

unsafe fn zap_cells(mut cell: Sexp, namespace: Sexp) -> R<bool> {
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        let Some(value) = zap_one(cell.car(), namespace)? else {
            return Ok(false);
        };
        crate::heap().set_car(cell, value);
        cell = cell.cdr();
    }
    Ok(true)
}

unsafe fn zap_one(value: Sexp, namespace: Sexp) -> R<Option<Sexp>> {
    match value.kind() {
        SexpType::Language => zap_call(value, namespace, false),
        SexpType::Nil | SexpType::Symbol => Ok(Some(value)),
        SexpType::Logical
        | SexpType::Integer
        | SexpType::Real
        | SexpType::String
        | SexpType::Complex
            if value.attrib() == nil() && length(value) == 1 =>
        {
            Ok(Some(value))
        }
        _ => type_sum(value, namespace).map(Some),
    }
}

/// `call_type_sum(x)` evaluated in rlang's namespace, as rlang's C code does.
unsafe fn type_sum(value: Sexp, namespace: Sexp) -> R<Sexp> {
    let env = values::new_env(namespace)?;
    let x = symbols().x;
    crate::heap().define(env, x, value)?;
    let call = values::lcons(install("call_type_sum")?, values::cons(x, nil())?)?;
    let result = eval::eval(call, env)?;
    crate::heap().root(result);
    Ok(result)
}

// withr 3.0.3 runs every `local_envvar()`, `local_options()` and `local_language()` through the
// small helpers below, and testthat calls those for every test. `withr::defer()` itself, and the
// `local_*()` functions that call it, keep their R bodies, so each deferred restore registers
// through the usual exit handlers.

/// withr's `list_combine(rhs, lhs)` of two plain lists: `rhs[nme] <- lhs[nme]` for each name of
/// `lhs`, which replaces the first element of `rhs` with that name by the first element of `lhs`
/// with it, or appends it.
unsafe fn do_withr_list_combine(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let (Some(rhs), Some(lhs)) = (
        argument(frame, symbols().rhs)?,
        argument(frame, symbols().lhs)?,
    ) else {
        return decline();
    };
    if !is_plain_list(rhs) || !is_plain_list(lhs) {
        return decline();
    }
    let lhs_names = values::names(lhs);
    if lhs_names == nil() {
        return finish(frame, rhs, true);
    }
    let rhs_names = values::names(rhs);
    let blank = values::blank_string();
    let mut entries: Vec<(Sexp, Sexp)> = (0..length(rhs))
        .map(|index| {
            let name = if rhs_names == nil() {
                blank
            } else {
                rhs_names.element(index)
            };
            (name, rhs.element(index))
        })
        .collect();
    let named = |names: Sexp| (0..length(names)).all(|index| names.element(index) != na_string());
    if (rhs_names != nil() && !named(rhs_names)) || !named(lhs_names) {
        return decline();
    }
    for index in 0..length(lhs_names) {
        let name = lhs_names.element(index);
        if name.bytes().is_empty() {
            return decline();
        }
        let Some(first) =
            (0..length(lhs)).find(|&other| lhs_names.element(other).bytes() == name.bytes())
        else {
            return decline();
        };
        let value = lhs.element(first);
        match entries
            .iter_mut()
            .find(|(existing, _)| existing.bytes() == name.bytes())
        {
            Some(entry) => entry.1 = value,
            None => entries.push((name, value)),
        }
    }
    let result = values::alloc(SexpType::List, entries.len())?;
    crate::heap().root(result);
    let names = values::alloc(SexpType::String, entries.len())?;
    crate::heap().root(names);
    for (index, &(name, value)) in entries.iter().enumerate() {
        values::set_elt(result, index, value);
        values::set_elt(names, index, name);
    }
    values::set_attr(result, symbols().names, names)?;
    finish(frame, result, true)
}

/// withr's `as_envvars(envs)` of a named character vector or plain list: `NULL` elements become
/// `NA` and, of elements with the same name, only the last is kept.
unsafe fn do_withr_as_envvars(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(envs) = argument(frame, symbols().envs)? else {
        return decline();
    };
    match envvars(envs)? {
        Some(envs) => finish(frame, envs, true),
        None => decline(),
    }
}

/// withr's `get_envvar(envs, action)`: the current values of the variables `envs` names, `NA`
/// for those unset, named by the variables.
unsafe fn do_withr_get_envvar(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(envs) = argument(frame, symbols().envs)? else {
        return decline();
    };
    // `Sys.getenv(NULL)` lists the whole environment instead.
    if length(envs) == 0 {
        return decline();
    }
    let Some(envs) = envvars(envs)? else {
        return decline();
    };
    let old = current_envvars(values::names(envs), frame)?;
    finish(frame, old, true)
}

/// withr's `set_envvar(envs, action = "replace")`: sets each variable `envs` gives a string and
/// unsets each it gives `NA`, returning the old values invisibly as `get_envvar()` does. The
/// prefix and suffix actions, and values other than single strings, keep the R body.
unsafe fn do_withr_set_envvar(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(envs) = argument(frame, symbols().envs)? else {
        return decline();
    };
    let Some(envs) = envvars(envs)? else {
        return decline();
    };
    let Some(action) = argument(frame, symbols().action)? else {
        return decline();
    };
    if action.kind() != SexpType::String
        || length(action) != 1
        || action.attrib() != nil()
        || action.element(0).bytes() != b"replace"
    {
        return decline();
    }
    let count = length(envs);
    if count == 0 {
        return finish(frame, nil(), true);
    }
    // `is.na()` of each element decides between setting and unsetting.
    let mut set: SmallVec<[(Sexp, Sexp); 8]> = SmallVec::new();
    let mut unset: SmallVec<[Sexp; 8]> = SmallVec::new();
    let names = values::names(envs);
    for index in 0..count {
        let name = names.element(index);
        let value = if envs.kind() == SexpType::String {
            envs.element(index)
        } else {
            let element = envs.element(index);
            match element.kind() {
                _ if length(element) != 1 => return decline(),
                SexpType::String if element.attrib() == nil() => element.element(0),
                SexpType::Logical if values::ints(element)[0] == NA_LOGICAL => na_string(),
                SexpType::Integer if values::ints(element)[0] == NA_INTEGER => na_string(),
                SexpType::Real if crate::arith::real_at(element, 0).is_nan() => na_string(),
                _ => return decline(),
            }
        };
        if value == na_string() {
            unset.push(name);
        } else {
            set.push((name, value));
        }
    }
    let old = current_envvars(names, frame)?;
    if !set.is_empty() {
        let set_names = values::alloc(SexpType::String, set.len())?;
        crate::heap().root(set_names);
        let set_values = values::alloc(SexpType::String, set.len())?;
        crate::heap().root(set_values);
        for (index, &(name, value)) in set.iter().enumerate() {
            values::set_elt(set_names, index, name);
            values::set_elt(set_values, index, value);
        }
        kernel(
            crate::system::do_sys_setenv,
            &[set_names, set_values],
            frame,
        )?;
    }
    if !unset.is_empty() {
        let unset_names = values::alloc(SexpType::String, unset.len())?;
        crate::heap().root(unset_names);
        for (index, &name) in unset.iter().enumerate() {
            values::set_elt(unset_names, index, name);
        }
        kernel(crate::system::do_sys_unsetenv, &[unset_names], frame)?;
    }
    finish(frame, old, false)
}

/// withr's `get_options(new_options)`: `options()` of the names of `new_options`, which gives the
/// current value of each.
unsafe fn do_withr_get_options(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(new_options) = argument(frame, symbols().new_options)? else {
        return decline();
    };
    let vector = matches!(
        new_options.kind(),
        SexpType::List
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
    );
    let names = values::names(new_options);
    // Without names, `options()` lists every option instead.
    if !vector || new_options.is_object() || names == nil() || length(names) == 0 {
        return decline();
    }
    // Each name is its own argument, as `do.call()` passes them: `options()` reads only the first
    // element of a character vector.
    let mut arguments: SmallVec<[Sexp; 8]> = SmallVec::new();
    for index in 0..length(names) {
        let name = values::alloc(SexpType::String, 1)?;
        crate::heap().root(name);
        values::set_elt(name, 0, names.element(index));
        arguments.push(name);
    }
    let old = kernel(envir::do_options, &arguments, frame)?;
    finish(frame, old, interp().visible)
}

/// withr's `set_options(new_options)` of a plain list whose elements are all named and set
/// without a condition: `options()` with each element as a named argument.
unsafe fn do_withr_set_options(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(new_options) = argument(frame, symbols().new_options)? else {
        return decline();
    };
    if !settable_options(new_options) || length(new_options) == 0 {
        return decline();
    }
    let old = kernel(envir::do_options, &[new_options], frame)?;
    finish(frame, old, interp().visible)
}

/// withr's `reset_options(old_options)`, which is `options(old_options)`, for the same lists
/// `set_options()` takes natively.
unsafe fn do_withr_reset_options(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(old_options) = argument(frame, symbols().old_options)? else {
        return decline();
    };
    if !settable_options(old_options) {
        return decline();
    }
    let old = kernel(envir::do_options, &[old_options], frame)?;
    finish(frame, old, interp().visible)
}

/// withr's `reset_gettext_cache()`, which is `bindtextdomain("reset", local_tempdir())`: binding
/// a domain flushes GNU R's cache of translations. The R body creates a fresh directory under the
/// session's temporary directory and removes it again before it returns, so no R code can see it;
/// binding the domain to the fresh path without creating the directory gives the same result.
/// When the session's temporary directory is missing, the R body's `dir.create(recursive =
/// TRUE)` would create it again, so the R body runs.
unsafe fn do_withr_reset_gettext_cache(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Ok(temp) = crate::system::session_temp_dir() else {
        return decline();
    };
    if !std::path::Path::new(temp).is_dir() {
        return decline();
    }
    let pattern = values::scalar_string("file")?;
    crate::heap().root(pattern);
    let directory = values::scalar_string(temp)?;
    crate::heap().root(directory);
    let extension = values::scalar_string("")?;
    crate::heap().root(extension);
    let path = kernel(
        crate::system::do_tempfile,
        &[pattern, directory, extension],
        frame,
    )?;
    let domain = values::scalar_string("reset")?;
    crate::heap().root(domain);
    let value = kernel(crate::system::do_bindtextdomain, &[domain, path], frame)?;
    finish(frame, value, true)
}

/// A list with no attribute but its names, which `[` and `[<-` treat without dispatch.
unsafe fn is_plain_list(value: Sexp) -> bool {
    value.kind() == SexpType::List && !value.is_object() && only_names(value)
}

unsafe fn only_names(value: Sexp) -> bool {
    let attributes = value.attrib();
    attributes == nil() || (attributes.cdr() == nil() && attributes.tag() == symbols().names)
}

/// withr's `as_envvars(envs)` for a character vector or plain list whose elements all have a
/// name, or an empty one; `None` when the R body is to run.
unsafe fn envvars(envs: Sexp) -> R<Option<Sexp>> {
    if length(envs) == 0 {
        return Ok(Some(envs));
    }
    if !matches!(envs.kind(), SexpType::String | SexpType::List)
        || envs.is_object()
        || !only_names(envs)
    {
        return Ok(None);
    }
    let names = values::names(envs);
    // `stopifnot(is.named(envs))` fails on a missing, empty or `NA` name.
    if names == nil()
        || (0..length(names)).any(|index| {
            let name = names.element(index);
            name == na_string() || name.bytes().is_empty()
        })
    {
        return Ok(None);
    }
    let count = length(envs);
    let kept: Vec<usize> = (0..count)
        .filter(|&index| {
            let name = names.element(index).bytes();
            !(index + 1..count).any(|later| names.element(later).bytes() == name)
        })
        .collect();
    let nulls =
        envs.kind() == SexpType::List && kept.iter().any(|&index| envs.element(index) == nil());
    if kept.len() == count && !nulls {
        return Ok(Some(envs));
    }
    let result = values::alloc(envs.kind(), kept.len())?;
    crate::heap().root(result);
    let result_names = values::alloc(SexpType::String, kept.len())?;
    crate::heap().root(result_names);
    let na = if nulls {
        let na = values::scalar_logical(NA_LOGICAL)?;
        crate::heap().root(na);
        na
    } else {
        nil()
    };
    for (position, &index) in kept.iter().enumerate() {
        let element = envs.element(index);
        let element = if envs.kind() == SexpType::List && element == nil() {
            na
        } else {
            element
        };
        values::set_elt(result, position, element);
        values::set_elt(result_names, position, names.element(index));
    }
    values::set_attr(result, symbols().names, result_names)?;
    Ok(Some(result))
}

/// `Sys.getenv(names, names = TRUE, unset = NA)` for a non-empty character vector `names`.
unsafe fn current_envvars(names: Sexp, frame: ClosureFrame) -> R<Sexp> {
    let unset = values::scalar_string_charsxp(na_string())?;
    crate::heap().root(unset);
    let old = kernel(crate::system::do_sys_getenv, &[names, unset], frame)?;
    values::set_attr(old, symbols().names, names)?;
    Ok(old)
}

/// A plain list that `options()` sets element by element, under its names, with no condition:
/// the call `do.call(options, as.list(x))` would make and `options(x)` gives the same result.
unsafe fn settable_options(options: Sexp) -> bool {
    if !is_plain_list(options) {
        return false;
    }
    let names = values::names(options);
    if names == nil() && length(options) > 0 {
        return false;
    }
    (0..length(options)).all(|index| {
        let name = names.element(index);
        let value = options.element(index);
        // `do.call()` evaluates a symbol or a call it is given as an argument.
        name != na_string()
            && !name.bytes().is_empty()
            && !matches!(
                value.kind(),
                SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode
            )
            && envir::option_sets_quietly(&values::char_str(name), value)
    })
}
