//! Environments, the call stack as R code sees it (`sys.call`, `parent.frame`, ...), the search
//! path, namespaces and options.

use crate::{
    builtins::{Arg, BuiltinDef, Call, Generic, builtin, internal, present},
    eval::{self, define, find_var, find_var_in_frame, value_of},
    interp,
    unwind::{R, error},
    values::{
        self, alloc, base_env, base_namespace, char_str, empty_env, get_attr, global_env, length,
        missing_arg, new_env, nil, scalar_bool, scalar_integer, scalar_string, string_at,
        string_vector, symbol_name, symbols, unbound,
    },
};
use heap::object::{NA_INTEGER, Sexp, SexpType, gp};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("environment", 1, do_environment),
    builtin("environment<-", 2, do_set_environment),
    internal("new.env", 3, do_new_env),
    internal("parent.env", 1, do_parent_env),
    internal("parent.env<-", 2, do_set_parent_env),
    builtin("globalenv", 0, |_| unsafe { Ok(global_env()) }),
    builtin("emptyenv", 0, |_| unsafe { Ok(empty_env()) }),
    builtin("baseenv", 0, |_| unsafe { Ok(base_env()) }),
    internal("topenv", 2, do_topenv),
    internal("environmentName", 1, do_environment_name),
    internal("assign", 4, do_assign).invisible(),
    internal("get", 4, do_get),
    internal("get0", 5, do_get0),
    internal("mget", 5, do_mget),
    internal("exists", 4, do_exists),
    internal("remove", 3, do_remove).invisible(),
    internal("ls", 3, do_ls),
    internal("env2list", 3, do_env2list),
    internal("list2env", 2, do_list2env),
    internal("lockEnvironment", 2, do_lock_environment).invisible(),
    internal("environmentIsLocked", 1, do_environment_is_locked),
    internal("lockBinding", 2, do_lock_binding).invisible(),
    internal("unlockBinding", 2, do_unlock_binding).invisible(),
    internal("bindingIsLocked", 2, do_binding_is_locked),
    internal("makeActiveBinding", 3, do_make_active_binding).invisible(),
    internal("bindingIsActive", 2, do_binding_is_active),
    internal("activeBindingFunction", 2, do_active_binding_function),
    internal("sys.call", 1, |call| unsafe { do_sys(call, SysKind::Call) }),
    internal("sys.function", 1, |call| unsafe {
        do_sys(call, SysKind::Function)
    }),
    internal("sys.frame", 1, |call| unsafe {
        do_sys(call, SysKind::Frame)
    }),
    internal("sys.nframe", 0, |call| unsafe {
        do_sys(call, SysKind::NFrame)
    }),
    internal("sys.calls", 0, |call| unsafe {
        do_sys(call, SysKind::Calls)
    }),
    internal("sys.frames", 0, |call| unsafe {
        do_sys(call, SysKind::Frames)
    }),
    internal("sys.parent", 1, |call| unsafe {
        do_sys(call, SysKind::Parent)
    }),
    internal("sys.parents", 0, |call| unsafe {
        do_sys(call, SysKind::Parents)
    }),
    internal("sys.on.exit", 0, |call| unsafe {
        do_sys(call, SysKind::OnExit)
    }),
    internal("parent.frame", 1, do_parent_frame),
    builtin("as.environment", 1, do_as_environment).generic(Generic::Internal),
    builtin("pos.to.env", 1, do_pos_to_env),
    internal("search", 0, do_search),
    internal("attach", 3, do_attach),
    internal("detach", 1, do_detach),
    internal("options", -1, do_options).keep_visibility(),
    internal("getOption", 1, do_get_option),
    builtin("interactive", 0, |_| unsafe {
        scalar_bool(interp().interactive)
    }),
    internal("getRegisteredNamespace", 1, do_get_registered_namespace),
    internal("isRegisteredNamespace", 1, do_is_registered_namespace),
    internal("registerNamespace", 2, do_register_namespace).invisible(),
    internal("unregisterNamespace", 1, do_unregister_namespace).invisible(),
    internal("getNamespaceRegistry", 0, |_| unsafe {
        Ok(crate::heap().known().namespace_registry)
    }),
    internal("importIntoEnv", 4, do_import_into_env).invisible(),
    internal("isNamespaceEnv", 1, |call| unsafe {
        scalar_bool(is_namespace(call.arg(0)))
    }),
    internal("do.call", 3, do_do_call).keep_visibility(),
    internal("lapply", 2, do_lapply),
    internal("vapply", 4, do_vapply),
    internal("mapply", 3, do_mapply),
    internal("rapply", 5, do_rapply),
    internal("eapply", 4, do_eapply),
    internal("Filter", -1, |_| unsafe { Ok(nil()) }),
    internal("env.profile", 1, |_| unsafe { Ok(nil()) }),
    internal("emptyenv", 0, |_| unsafe { Ok(empty_env()) }),
    builtin("...length", 0, do_dots_length),
    builtin("...elt", 1, do_dots_elt).keep_visibility(),
    builtin("...names", 0, do_dots_names),
    crate::builtins::special("::", 2, |call| unsafe { namespace_value(call, true) }),
    crate::builtins::special(":::", 2, |call| unsafe { namespace_value(call, false) }),
    internal("getNamespaceValue", 3, |call| unsafe {
        let name = values::require_string(call.arg(1), "name")?;
        let exported = values::as_logical_scalar(call.arg(2)) == 1;
        namespace_lookup(call.arg(0), &name, exported, call.env)
    }),
    internal("formals", 1, do_formals),
    internal("body", 1, do_body),
    internal("args", 1, do_args),
    internal("bodyCode", 1, do_body),
    internal("match.call", 4, do_match_call),
    internal("sys.source", -1, |_| unsafe { Ok(nil()) }),
];

unsafe fn environment_arg(value: Sexp, what: &str) -> R<Sexp> {
    match value.kind() {
        SexpType::Environment => Ok(value),
        SexpType::Nil => Err(error("use of NULL environment is defunct")),
        _ => {
            let converted = as_environment(value);
            match converted {
                Ok(env) => Ok(env),
                Err(_) => Err(error(format!("invalid '{what}' argument"))),
            }
        }
    }
}

/// The frame of the closure whose environment is `env`, as a stack index.
unsafe fn frame_of(env: Sexp) -> Option<usize> {
    interp().frames.iter().rposition(|frame| frame.env == env)
}

/// GNU R's `cptr` for the `sys.*` functions: the frame the `sys.*` closure was called from.
/// `None` stands for the top level.
unsafe fn sys_context(env: Sexp) -> Option<usize> {
    let frames = &interp().frames;
    let own = frames.iter().rposition(|frame| frame.env == env)?;
    let target = frames[own].sys_parent;
    (0..=own).rev().find(|&index| frames[index].env == target)
}

/// `framedepth`: how many frames from `context` down to the top level.
fn frame_depth(context: Option<usize>) -> usize {
    context.map_or(0, |index| index + 1)
}

unsafe fn sys_frame_at(n: i32, context: Option<usize>) -> R<Sexp> {
    if n == 0 {
        return Ok(global_env());
    }
    if n == NA_INTEGER {
        return Err(error("NA argument is invalid"));
    }
    let depth = frame_depth(context) as i64;
    let back = if n > 0 { depth - n as i64 } else { -(n as i64) };
    if back < 0 {
        return Err(error("not that many frames on the stack"));
    }
    match context {
        Some(index) if back <= index as i64 => Ok(interp().frames[index - back as usize].env),
        _ if back == depth => Ok(global_env()),
        _ => Err(error("not that many frames on the stack")),
    }
}

/// `sys.frame(which)` relative to the frame that `env` belongs to.
pub unsafe fn sys_frame(which: i32, env: Sexp) -> R<Sexp> {
    let context = frame_of(env);
    sys_frame_at(which, context)
}

unsafe fn sys_call_at(n: i32, context: Option<usize>, function: bool) -> R<Sexp> {
    let depth = frame_depth(context) as i64;
    let back = if n > 0 { depth - n as i64 } else { -(n as i64) };
    if back < 0 {
        return Err(error("not that many frames on the stack"));
    }
    match context {
        Some(index) if back <= index as i64 => {
            let frame = &interp().frames[index - back as usize];
            if function {
                Ok(frame.function)
            } else {
                call_with_srcref(frame)
            }
        }
        _ if back == depth => Ok(nil()),
        _ => Err(error("not that many frames on the stack")),
    }
}

/// A copy of a frame's call, with the srcref current when the call was made as its `srcref`
/// attribute, as GNU R's `sys.call` returns it.
pub unsafe fn call_with_srcref(frame: &crate::Frame) -> R<Sexp> {
    let call = values::duplicate(frame.call)?;
    if frame.srcref != nil() {
        values::set_attr(call, symbols().srcref, values::duplicate(frame.srcref)?)?;
    }
    Ok(call)
}

/// `R_sysparent`: the frame number of the parent of frame `n` counted back from `context`.
unsafe fn sys_parent_number(mut n: i64, context: Option<usize>) -> i64 {
    let frames = &interp().frames;
    let Some(mut index) = context else {
        return 0;
    };
    while index > 0 && n > 1 {
        n -= 1;
        index -= 1;
    }
    let parent = frames[index].sys_parent;
    if parent == global_env() {
        return 0;
    }
    let mut count = 0;
    let mut found = n;
    for position in (0..=index).rev() {
        count += 1;
        if frames[position].env == parent {
            found = count;
        }
    }
    (count - found + 1).max(0)
}

enum SysKind {
    Call,
    Function,
    Frame,
    NFrame,
    Calls,
    Frames,
    Parent,
    Parents,
    OnExit,
}

unsafe fn do_sys(call: &Call<'_>, kind: SysKind) -> R<Sexp> {
    let context = sys_context(call.env);
    let n = if call.is_empty() {
        0
    } else {
        values::as_integer_scalar(call.arg(0))
    };
    match kind {
        SysKind::Call => {
            if n == NA_INTEGER {
                return Err(error("invalid 'which' argument"));
            }
            sys_call_at(n, context, false)
        }
        SysKind::Function => {
            if n == NA_INTEGER {
                return Err(error("invalid 'which' value"));
            }
            sys_call_at(n, context, true)
        }
        SysKind::Frame => sys_frame_at(n, context),
        SysKind::NFrame => scalar_integer(frame_depth(context) as i32),
        SysKind::Calls => {
            let depth = frame_depth(context);
            let mut items = Vec::new();
            for index in 1..=depth {
                items.push((nil(), sys_call_at(index as i32, context, false)?));
            }
            values::pairlist(&items, SexpType::Pairlist)
        }
        SysKind::Frames => {
            let depth = frame_depth(context);
            let mut items = Vec::new();
            for index in 1..=depth {
                items.push((nil(), sys_frame_at(index as i32, context)?));
            }
            values::pairlist(&items, SexpType::Pairlist)
        }
        SysKind::Parent => {
            if n == NA_INTEGER {
                return Err(error("invalid 'n' argument"));
            }
            let depth = frame_depth(context) as i64;
            let mut position = depth;
            let mut remaining = n;
            while remaining > 0 {
                position = sys_parent_number(depth - position + 1, context);
                remaining -= 1;
            }
            scalar_integer(position as i32)
        }
        SysKind::Parents => {
            let depth = frame_depth(context) as i64;
            let result = alloc(SexpType::Integer, depth as usize)?;
            for index in 0..depth {
                values::ints_mut(result)[index as usize] =
                    sys_parent_number(depth - index, context) as i32;
            }
            Ok(result)
        }
        SysKind::OnExit => {
            let Some(index) = context else {
                return Ok(nil());
            };
            let on_exit = interp().frames[index].on_exit.clone();
            match on_exit.as_slice() {
                [] => Ok(nil()),
                [single] => Ok(*single),
                many => {
                    let mut items = vec![(nil(), symbols().brace)];
                    items.extend(many.iter().map(|&expression| (nil(), expression)));
                    values::pairlist(&items, SexpType::Language)
                }
            }
        }
    }
}

unsafe fn do_parent_frame(call: &Call<'_>) -> R<Sexp> {
    let mut n = values::as_integer_scalar(call.arg(0));
    if n == NA_INTEGER || n < 1 {
        return Err(error("invalid 'n' value"));
    }
    let frames = &interp().frames;
    let Some(own) = frames.iter().rposition(|frame| frame.env == call.env) else {
        return Ok(global_env());
    };
    let mut target = frames[own].sys_parent;
    for index in (0..=own).rev() {
        if frames[index].env == target {
            if n == 1 {
                return Ok(frames[index].sys_parent);
            }
            n -= 1;
            target = frames[index].sys_parent;
        }
    }
    Ok(global_env())
}

unsafe fn do_environment(call: &Call<'_>) -> R<Sexp> {
    let function = call.arg(0);
    match function.kind() {
        SexpType::Nil => {
            let frames = &interp().frames;
            Ok(frames
                .iter()
                .rposition(|frame| frame.env == call.env)
                .map_or(global_env(), |index| frames[index].sys_parent))
        }
        SexpType::Closure => Ok(function.tag()),
        SexpType::Builtin | SexpType::Special => Ok(nil()),
        _ => {
            let environment = values::install(".Environment")?;
            Ok(get_attr(function, environment))
        }
    }
}

unsafe fn do_set_environment(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let env = call.arg(1);
    if object.kind() == SexpType::Closure {
        if env.kind() != SexpType::Environment {
            return Err(call.error("replacement object is not an environment"));
        }
        let copy = values::triple(SexpType::Closure, object.car(), object.cdr(), env)?;
        values::copy_attributes(object, copy)?;
        return Ok(copy);
    }
    if env != nil() && env.kind() != SexpType::Environment {
        return Err(call.error("replacement object is not an environment"));
    }
    let copy = if values::maybe_shared(object) {
        values::shallow_duplicate(object)?
    } else {
        object
    };
    let environment = values::install(".Environment")?;
    values::set_attr(copy, environment, env)?;
    Ok(copy)
}

unsafe fn do_new_env(call: &Call<'_>) -> R<Sexp> {
    let parent = call.arg(1);
    let parent = if parent.kind() == SexpType::Environment {
        parent
    } else {
        environment_arg(parent, "enclos")?
    };
    let env = new_env(parent)?;
    if values::as_logical_scalar(call.arg(0)) == 1 {
        let size = values::as_integer_scalar(call.arg(2));
        crate::heap().set_frame_hashed(env, if size == NA_INTEGER { 0 } else { size });
    }
    Ok(env)
}

unsafe fn do_parent_env(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "env")?;
    if env == empty_env() {
        return Err(call.error("the empty environment has no parent"));
    }
    Ok(env.enclosure())
}

unsafe fn do_set_parent_env(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "env")?;
    let parent = call.arg(1);
    if parent.kind() != SexpType::Environment {
        return Err(call.error("'parent' is not an environment"));
    }
    if env == empty_env() {
        return Err(call.error("can not set the parent of the empty environment"));
    }
    crate::heap().set_enclosure(env, parent);
    Ok(env)
}

unsafe fn do_topenv(call: &Call<'_>) -> R<Sexp> {
    let env = call.arg(0);
    let target = call.arg(1);
    // A non-environment falls back to the frame `.Internal(topenv())` runs in, whose top level
    // is base's namespace, as in GNU R (`topenv(sum)`).
    let env = if env.kind() == SexpType::Environment {
        env
    } else {
        call.env
    };
    let mut current = env;
    while current != empty_env() {
        if current == target
            || current == global_env()
            || current == base_env()
            || current == base_namespace()
            || is_namespace(current)
            || is_package_env(current)
            || crate::heap()
                .frame_cell(current, symbols().dot_package_name)
                .is_some()
        {
            return Ok(current);
        }
        current = current.enclosure();
    }
    Ok(global_env())
}

/// Whether `env` is a namespace: the base namespace, or an environment holding
/// `.__NAMESPACE__.`.
pub unsafe fn is_namespace(env: Sexp) -> bool {
    if env == base_namespace() {
        return true;
    }
    if env.kind() != SexpType::Environment {
        return false;
    }
    match crate::heap().frame_cell(env, symbols().dot_namespace) {
        Some(cell) => cell.cdr().kind() == SexpType::Environment,
        None => false,
    }
}

/// Whether `env` is a package environment on the search path (its `name` starts `package:`).
pub unsafe fn is_package_env(env: Sexp) -> bool {
    let name = get_attr(env, values::install("name").unwrap_or(nil()));
    matches!(values::first_string(name), Some(text) if text.starts_with("package:"))
}

/// The name of a namespace, from its `spec`.
pub unsafe fn namespace_name(env: Sexp) -> Option<String> {
    if env == base_namespace() {
        return Some("base".into());
    }
    let info = crate::heap()
        .frame_cell(env, symbols().dot_namespace)?
        .cdr();
    let info = value_of(info).ok()?;
    if info.kind() != SexpType::Environment {
        return None;
    }
    let spec_symbol = values::install("spec").ok()?;
    let spec = value_of(find_var_in_frame(spec_symbol, info).ok()?).ok()?;
    values::first_string(spec).map(|text| text.into_owned())
}

unsafe fn do_environment_name(call: &Call<'_>) -> R<Sexp> {
    let env = call.arg(0);
    if env.kind() != SexpType::Environment {
        return scalar_string("");
    }
    let name = if env == global_env() {
        "R_GlobalEnv".to_owned()
    } else if env == base_env() {
        "base".to_owned()
    } else if env == empty_env() {
        "R_EmptyEnv".to_owned()
    } else if is_package_env(env) {
        let name_attr = get_attr(env, values::install("name")?);
        values::first_string(name_attr)
            .map(|text| text.into_owned())
            .unwrap_or_default()
    } else if is_namespace(env) {
        namespace_name(env).unwrap_or_default()
    } else {
        let name_attr = get_attr(env, values::install("name")?);
        values::first_string(name_attr)
            .map(|text| text.into_owned())
            .unwrap_or_default()
    };
    scalar_string(&name)
}

/// A symbol from a name argument.
unsafe fn name_symbol(value: Sexp, what: &str) -> R<Sexp> {
    match value.kind() {
        SexpType::Symbol => Ok(value),
        SexpType::String if length(value) >= 1 => {
            let name = value.element(0);
            if name.bytes().is_empty() {
                return Err(error("attempt to use zero-length variable name"));
            }
            values::install_bytes(name.bytes())
        }
        _ => Err(error(format!("invalid '{what}' argument"))),
    }
}

unsafe fn do_assign(call: &Call<'_>) -> R<Sexp> {
    let symbol = name_symbol(call.arg(0), "x")?;
    let value = call.arg(1);
    let env = environment_arg(call.arg(2), "envir")?;
    let inherits = values::as_logical_scalar(call.arg(3)) == 1;
    if inherits {
        eval::set_var(symbol, value, env)?;
    } else {
        eval::assign_symbol(symbol, value, env, false)?;
    }
    Ok(value)
}

/// Whether `value` matches the `mode` argument of `get` and `exists`.
unsafe fn mode_matches(value: Sexp, mode: &str) -> bool {
    match mode {
        "any" => true,
        "function" => values::is_function(value),
        "numeric" => matches!(value.kind(), SexpType::Integer | SexpType::Real),
        "name" => value.kind() == SexpType::Symbol,
        "call" => value.kind() == SexpType::Language,
        "list" => value.kind() == SexpType::List,
        mode => value.kind().name() == mode,
    }
}

/// Looks `symbol` up for `get`, `exists` and `mget`: in `env`'s frame, and with `inherits` in
/// every enclosure, skipping values of the wrong mode.
unsafe fn lookup(symbol: Sexp, env: Sexp, mode: &str, inherits: bool) -> R<Option<Sexp>> {
    let mut frame = env;
    while frame != empty_env() {
        let value = find_var_in_frame(symbol, frame)?;
        if value != unbound() {
            if mode == "any" {
                return Ok(Some(value));
            }
            let forced = value_of(value)?;
            if mode_matches(forced, mode) {
                return Ok(Some(forced));
            }
        }
        if !inherits {
            break;
        }
        frame = frame.enclosure();
    }
    Ok(None)
}

unsafe fn do_get(call: &Call<'_>) -> R<Sexp> {
    let symbol = name_symbol(call.arg(0), "x")?;
    let env = environment_arg(call.arg(1), "envir")?;
    let mode = values::require_string(call.arg(2), "mode")?;
    let inherits = values::as_logical_scalar(call.arg(3)) == 1;
    match lookup(symbol, env, &mode, inherits)? {
        Some(value) => {
            if value == missing_arg() {
                return Err(error(format!(
                    "argument \"{}\" is missing, with no default",
                    symbol_name(symbol)
                )));
            }
            value_of(value)
        }
        None => {
            if mode == "any" {
                Err(error(format!("object '{}' not found", symbol_name(symbol))))
            } else {
                Err(error(format!(
                    "object '{}' of mode '{mode}' was not found",
                    symbol_name(symbol)
                )))
            }
        }
    }
}

unsafe fn do_get0(call: &Call<'_>) -> R<Sexp> {
    let symbol = name_symbol(call.arg(0), "x")?;
    let env = environment_arg(call.arg(1), "envir")?;
    let mode = values::require_string(call.arg(2), "mode")?;
    let inherits = values::as_logical_scalar(call.arg(3)) == 1;
    match lookup(symbol, env, &mode, inherits)? {
        Some(value) => value_of(value),
        None => Ok(call.arg(4)),
    }
}

unsafe fn do_mget(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let env = environment_arg(call.arg(1), "envir")?;
    let modes = call.arg(2);
    let if_not_found = call.arg(3);
    let inherits = values::as_logical_scalar(call.arg(4)) == 1;
    let count = length(names);
    let result = alloc(SexpType::List, count)?;
    for index in 0..count {
        let name = string_at(names, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let symbol = values::install(&name)?;
        let mode = if length(modes) > 0 {
            string_at(modes, index % length(modes))
                .map(|text| text.into_owned())
                .unwrap_or_else(|| "any".into())
        } else {
            "any".into()
        };
        let value = match lookup(symbol, env, &mode, inherits)? {
            Some(value) => value_of(value)?,
            None => {
                if if_not_found.kind() == SexpType::List && length(if_not_found) > 0 {
                    let fallback = if_not_found.element(index % length(if_not_found));
                    if values::is_function(fallback) {
                        let argument = scalar_string(&name)?;
                        eval::apply(nil(), fallback, &eval::value_args(&[argument]), call.env)?
                    } else {
                        fallback
                    }
                } else {
                    return Err(error(format!("value for '{name}' not found")));
                }
            }
        };
        values::set_elt(result, index, value);
    }
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

unsafe fn do_exists(call: &Call<'_>) -> R<Sexp> {
    let symbol = name_symbol(call.arg(0), "x")?;
    let env = environment_arg(call.arg(1), "envir")?;
    let mode = values::require_string(call.arg(2), "mode")?;
    let inherits = values::as_logical_scalar(call.arg(3)) == 1;
    scalar_bool(lookup(symbol, env, &mode, inherits)?.is_some())
}

unsafe fn do_remove(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let env = environment_arg(call.arg(1), "envir")?;
    let inherits = values::as_logical_scalar(call.arg(2)) == 1;
    for index in 0..length(names) {
        let name = string_at(names, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let symbol = values::install(&name)?;
        let mut frame = env;
        let mut done = false;
        while frame != empty_env() {
            if crate::heap().remove_binding(frame, symbol)? {
                done = true;
                break;
            }
            if !inherits {
                break;
            }
            frame = frame.enclosure();
        }
        if !done {
            crate::conditions::warning(format!("object '{name}' not found"))?;
        }
    }
    Ok(nil())
}

/// The symbols bound in `env`, in the order `ls()` lists their names.
pub unsafe fn env_symbols(env: Sexp, all: bool, sorted: bool) -> Vec<Sexp> {
    let mut symbols = crate::heap().frame_symbols(env);
    if !all {
        symbols.retain(|symbol| symbol.car().bytes().first() != Some(&b'.'));
    }
    if sorted {
        symbols
            .sort_by(|left, right| crate::sort::collate(&symbol_name(*left), &symbol_name(*right)));
    }
    symbols
}

/// The names bound in `env` as a character vector, as `ls()` lists them. A symbol's print name
/// is already an interned `CHARSXP`, so no name is copied.
pub unsafe fn env_names(env: Sexp, all: bool, sorted: bool) -> R<Sexp> {
    symbol_names(&env_symbols(env, all, sorted))
}

unsafe fn symbol_names(symbols: &[Sexp]) -> R<Sexp> {
    let names = alloc(SexpType::String, symbols.len())?;
    for (index, symbol) in symbols.iter().enumerate() {
        values::set_elt(names, index, symbol.car());
    }
    Ok(names)
}

unsafe fn do_ls(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "envir")?;
    let all = values::as_logical_scalar(call.arg(1)) == 1;
    let sorted = values::as_logical_scalar(call.arg(2)) != 0;
    env_names(env, all, sorted)
}

unsafe fn do_env2list(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "x")?;
    let all = values::as_logical_scalar(call.arg(1)) == 1;
    let sorted = values::as_logical_scalar(call.arg(2)) == 1;
    let bound = env_symbols(env, all, sorted);
    let result = alloc(SexpType::List, bound.len())?;
    for (index, &symbol) in bound.iter().enumerate() {
        let value = find_var_in_frame(symbol, env)?;
        let value = value_of(value)?;
        values::set_elt(result, index, value);
    }
    if !bound.is_empty() {
        values::set_attr(result, symbols().names, symbol_names(&bound)?)?;
    }
    Ok(result)
}

/// A new environment holding a named list's elements.
pub unsafe fn list_to_env(list: Sexp, enclosure: Sexp) -> R<Sexp> {
    let enclosure = if enclosure.kind() == SexpType::Environment {
        enclosure
    } else {
        base_env()
    };
    let env = new_env(enclosure)?;
    fill_env(env, list)?;
    Ok(env)
}

unsafe fn fill_env(env: Sexp, list: Sexp) -> R<()> {
    let names = values::names(list);
    for index in 0..length(list) {
        let Some(name) = (names != nil()).then(|| string_at(names, index)).flatten() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let symbol = values::install(&name)?;
        define(symbol, list.element(index), env)?;
    }
    Ok(())
}

unsafe fn do_list2env(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let env = environment_arg(call.arg(1), "envir")?;
    if list.kind() != SexpType::List && list != nil() {
        return Err(call.error("first argument must be a named list"));
    }
    if length(list) > 0 && values::names(list) == nil() {
        return Err(call.error("names(x) must be a character vector of the same length as x"));
    }
    fill_env(env, list)?;
    Ok(env)
}

unsafe fn do_lock_environment(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "env")?;
    let bindings = values::as_logical_scalar(call.arg(1)) == 1;
    if bindings {
        for symbol in crate::heap().frame_symbols(env) {
            if let Some(cell) = crate::heap().frame_cell(env, symbol) {
                cell.set_gp_bits(gp::BINDING_LOCKED, true);
            }
        }
    }
    if env != base_env() && env != base_namespace() {
        env.set_gp_bits(gp::FRAME_LOCKED, true);
    }
    Ok(nil())
}

unsafe fn do_environment_is_locked(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "env")?;
    scalar_bool(env.has_gp(gp::FRAME_LOCKED))
}

unsafe fn binding_cell(call: &Call<'_>) -> R<Sexp> {
    let symbol = name_symbol(call.arg(0), "sym")?;
    let env = environment_arg(call.arg(1), "env")?;
    crate::heap()
        .frame_cell(env, symbol)
        .ok_or_else(|| error(format!("no binding for \"{}\"", symbol_name(symbol))))
}

unsafe fn do_lock_binding(call: &Call<'_>) -> R<Sexp> {
    binding_cell(call)?.set_gp_bits(gp::BINDING_LOCKED, true);
    Ok(nil())
}

unsafe fn do_unlock_binding(call: &Call<'_>) -> R<Sexp> {
    binding_cell(call)?.set_gp_bits(gp::BINDING_LOCKED, false);
    Ok(nil())
}

unsafe fn do_binding_is_locked(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(binding_cell(call)?.has_gp(gp::BINDING_LOCKED))
}

unsafe fn do_make_active_binding(call: &Call<'_>) -> R<Sexp> {
    let symbol = name_symbol(call.arg(0), "sym")?;
    let function = call.arg(1);
    let env = environment_arg(call.arg(2), "env")?;
    if !values::is_function(function) {
        return Err(call.error("'fun' must be a function"));
    }
    let heap = crate::heap();
    if let Some(cell) = heap.frame_cell(env, symbol) {
        if !cell.has_gp(gp::ACTIVE_BINDING) {
            return Err(call.error("symbol already has a regular binding"));
        }
        heap.set_binding_value(cell, function)?;
    } else {
        heap.define(env, symbol, function)?;
        if let Some(cell) = heap.frame_cell(env, symbol) {
            cell.set_gp_bits(gp::ACTIVE_BINDING, true);
        }
    }
    Ok(nil())
}

unsafe fn do_binding_is_active(call: &Call<'_>) -> R<Sexp> {
    scalar_bool(binding_cell(call)?.has_gp(gp::ACTIVE_BINDING))
}

unsafe fn do_active_binding_function(call: &Call<'_>) -> R<Sexp> {
    let cell = binding_cell(call)?;
    if !cell.has_gp(gp::ACTIVE_BINDING) {
        return Err(call.error("no active binding"));
    }
    Ok(cell.cdr())
}

/// The search path, from the global environment to base.
pub unsafe fn search_path() -> Vec<Sexp> {
    let mut path = Vec::new();
    let mut env = global_env();
    while env != empty_env() {
        path.push(env);
        if env == base_env() {
            break;
        }
        env = env.enclosure();
    }
    path
}

unsafe fn search_name(env: Sexp) -> R<String> {
    if env == global_env() {
        return Ok(".GlobalEnv".into());
    }
    if env == base_env() {
        return Ok("package:base".into());
    }
    let name = get_attr(env, values::install("name")?);
    Ok(values::first_string(name)
        .map(|text| text.into_owned())
        .unwrap_or_default())
}

unsafe fn do_search(_call: &Call<'_>) -> R<Sexp> {
    let names: Vec<String> = search_path()
        .into_iter()
        .map(|env| search_name(env))
        .collect::<R<_>>()?;
    string_vector(&names)
}

/// `as.environment`: a number is a search-path position, a string a search-path name, a list
/// a new environment.
pub unsafe fn as_environment(value: Sexp) -> R<Sexp> {
    match value.kind() {
        SexpType::Environment => Ok(value),
        SexpType::Integer | SexpType::Real => {
            let position = values::as_integer_scalar(value);
            let path = search_path();
            if position == -1 {
                // The calling environment.
                return Ok(interp()
                    .frames
                    .last()
                    .map_or(global_env(), |frame| frame.sys_parent));
            }
            if position < 1 || position as usize > path.len() {
                return Err(error("invalid 'pos' argument"));
            }
            Ok(path[position as usize - 1])
        }
        SexpType::String => {
            let name = values::require_string(value, "pos")?;
            if name == ".GlobalEnv" {
                return Ok(global_env());
            }
            if name == "package:base" {
                return Ok(base_env());
            }
            for env in search_path() {
                if search_name(env)? == name {
                    return Ok(env);
                }
            }
            Err(error(format!(
                "no item called \"{name}\" on the search list"
            )))
        }
        SexpType::List => list_to_env(value, empty_env()),
        SexpType::Object if value.has_gp(gp::S4_OBJECT) => {
            let data = get_attr(value, symbols().dot_xdata);
            if data.kind() == SexpType::Environment {
                Ok(data)
            } else {
                Err(error("S4 object does not extend class \"environment\""))
            }
        }
        _ => Err(error("invalid object for 'as.environment'")),
    }
}

unsafe fn do_as_environment(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    if value.kind() == SexpType::Nil {
        return Err(call.error("using 'as.environment(NULL)' is defunct"));
    }
    as_environment(value)
}

unsafe fn do_pos_to_env(call: &Call<'_>) -> R<Sexp> {
    as_environment(call.arg(0))
}

unsafe fn do_attach(call: &Call<'_>) -> R<Sexp> {
    let what = call.arg(0);
    let position = values::as_integer_scalar(call.arg(1)).max(2) as usize;
    let name = call.arg(2);
    let env = if what.kind() == SexpType::Environment {
        what
    } else {
        let env = new_env(empty_env())?;
        if what.kind() == SexpType::List {
            fill_env(env, what)?;
        }
        env
    };
    let name_symbol = values::install("name")?;
    values::set_attr(env, name_symbol, name)?;
    let mut previous = global_env();
    for _ in 2..position {
        let next = previous.enclosure();
        if next == base_env() {
            break;
        }
        previous = next;
    }
    let heap = crate::heap();
    heap.set_enclosure(env, previous.enclosure());
    heap.set_enclosure(previous, env);
    interp().globals.keep.push(env);
    Ok(env)
}

unsafe fn do_detach(call: &Call<'_>) -> R<Sexp> {
    let position = values::as_integer_scalar(call.arg(0));
    if position < 2 {
        return Err(call.error("invalid 'pos' argument"));
    }
    let path = search_path();
    let Some(&env) = path.get(position as usize - 1) else {
        return Err(call.error("invalid 'pos' argument"));
    };
    if env == base_env() {
        return Err(call.error("detaching \"package:base\" is not allowed"));
    }
    let previous = path[position as usize - 2];
    crate::heap().set_enclosure(previous, env.enclosure());
    interp().globals.keep.retain(|&kept| kept != env);
    Ok(env)
}

// Options.

/// The options as a tagged pairlist. GNU R keeps them as the base environment's `.Options`
/// binding, which R code reads directly.
pub unsafe fn options() -> Sexp {
    match crate::heap().frame_cell(base_env(), symbols().dot_options) {
        Some(cell) => cell.cdr(),
        None => nil(),
    }
}

unsafe fn set_options(list: Sexp) -> R<()> {
    crate::heap().define(base_env(), symbols().dot_options, list)?;
    Ok(())
}

/// The value of option `name`, or `NULL`.
pub unsafe fn get_option(name: &str) -> Sexp {
    let mut cell = options();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag().car().bytes() == name.as_bytes() {
            return cell.car();
        }
        cell = cell.cdr();
    }
    nil()
}

/// Sets option `name`, removing it for `NULL`, and returns the old value.
pub unsafe fn set_option(name: &str, value: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let symbol = values::install(name)?;
    let mut cell = options();
    let mut previous = nil();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == symbol {
            let old = cell.car();
            // Once replaced, the old value is reachable only from the caller.
            heap.root(old);
            if value == nil() {
                if previous == nil() {
                    set_options(cell.cdr())?;
                } else {
                    heap.set_cdr(previous, cell.cdr());
                }
            } else {
                heap.set_car(cell, value);
            }
            return Ok(old);
        }
        previous = cell;
        cell = cell.cdr();
    }
    if value != nil() {
        let fresh = values::cons_tagged(value, nil(), symbol)?;
        if previous == nil() {
            set_options(fresh)?;
        } else {
            heap.set_cdr(previous, fresh);
        }
    }
    if name == "expressions" {
        let depth = values::as_integer_scalar(value);
        if depth > 0 {
            interp().max_depth = depth.min(500_000) as u32;
        }
    }
    Ok(nil())
}

unsafe fn options_list(names: Option<&[String]>) -> R<Sexp> {
    let mut entries: Vec<(String, Sexp)> = Vec::new();
    let mut cell = options();
    while cell.kind() == SexpType::Pairlist {
        entries.push((symbol_name(cell.tag()).into_owned(), cell.car()));
        cell = cell.cdr();
    }
    let entries: Vec<(String, Sexp)> = match names {
        Some(names) => names
            .iter()
            .map(|name| {
                let value = entries
                    .iter()
                    .find(|(entry, _)| entry == name)
                    .map_or(nil(), |(_, value)| *value);
                (name.clone(), value)
            })
            .collect(),
        None => {
            let mut sorted = entries;
            sorted.sort_by(|a, b| crate::sort::collate(&a.0, &b.0));
            sorted
        }
    };
    let items: Vec<(&str, Sexp)> = entries
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    values::named_list(&items)
}

unsafe fn do_options(call: &Call<'_>) -> R<Sexp> {
    let mut args: Vec<Arg> = Vec::new();
    for arg in call.args {
        if arg.tag == nil() && arg.value.kind() == SexpType::List {
            let names = values::names(arg.value);
            for index in 0..length(arg.value) {
                let tag = if names != nil() {
                    let name = names.element(index);
                    if name.bytes().is_empty() {
                        nil()
                    } else {
                        crate::heap().install_char(name)?
                    }
                } else {
                    nil()
                };
                args.push(Arg {
                    tag,
                    value: arg.value.element(index),
                });
            }
        } else {
            args.push(*arg);
        }
    }
    if call.args.is_empty() {
        interp().visible = true;
        return options_list(None);
    }
    let mut changed = false;
    let mut entries: Vec<(String, Sexp)> = Vec::new();
    for arg in &args {
        if arg.tag != nil() {
            let name = symbol_name(arg.tag).into_owned();
            let value = validate_option(&name, arg.value)?;
            let old = set_option(&name, value)?;
            entries.push((name, old));
            changed = true;
        } else if arg.value.kind() == SexpType::String {
            for index in 0..length(arg.value) {
                let name = string_at(arg.value, index)
                    .map(|text| text.into_owned())
                    .unwrap_or_default();
                let value = get_option(&name);
                entries.push((name, value));
            }
        } else if arg.value == nil() {
            continue;
        } else {
            return Err(call.error("invalid argument"));
        }
    }
    interp().visible = !changed;
    let items: Vec<(&str, Sexp)> = entries
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    values::named_list(&items)
}

unsafe fn validate_option(name: &str, value: Sexp) -> R<Sexp> {
    match name {
        "digits" => {
            let digits = values::as_integer_scalar(value);
            if !(1..=22).contains(&digits) {
                return Err(error("invalid 'digits' parameter, allowed 1...22"));
            }
            scalar_integer(digits)
        }
        "width" => {
            let width = values::as_integer_scalar(value);
            if !(10..=10000).contains(&width) {
                return Err(error("invalid 'width' parameter, allowed 10...10000"));
            }
            scalar_integer(width)
        }
        "expressions" => {
            let depth = values::as_integer_scalar(value);
            if !(25..=500_000).contains(&depth) {
                return Err(error(
                    "'expressions' parameter invalid, allowed 25...500000",
                ));
            }
            scalar_integer(depth)
        }
        "warning.length" => {
            let length = values::as_integer_scalar(value);
            if !(100..=8170).contains(&length) {
                return Err(error("invalid value for 'warning.length'"));
            }
            scalar_integer(length)
        }
        "max.print" | "nwarnings" => {
            let count = values::as_integer_scalar(value);
            if count < 1 {
                return Err(error(&format!("invalid value for '{name}'")));
            }
            scalar_integer(count)
        }
        "warn" | "scipen" | "deparse.cutoff" => {
            if value.kind() == SexpType::Real || value.kind() == SexpType::Integer {
                scalar_integer(values::as_integer_scalar(value))
            } else {
                Ok(value)
            }
        }
        // GNU R stores a function given as the error handler as a call to it.
        "error"
            if matches!(
                value.kind(),
                SexpType::Closure | SexpType::Builtin | SexpType::Special
            ) =>
        {
            values::lcons(value, nil())
        }
        _ => Ok(value),
    }
}

unsafe fn do_get_option(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_string(call.arg(0), "x")?;
    Ok(get_option(&name))
}

/// Sets the options GNU R's C code sets at startup, before any R code runs.
pub unsafe fn install_default_options() -> R<()> {
    let defaults: Vec<(&str, Sexp)> = vec![
        ("prompt", scalar_string("> ")?),
        ("continue", scalar_string("+ ")?),
        ("expressions", scalar_integer(5000)?),
        ("width", scalar_integer(80)?),
        ("deparse.cutoff", scalar_integer(60)?),
        ("digits", scalar_integer(7)?),
        ("echo", scalar_bool(false)?),
        ("verbose", scalar_bool(false)?),
        ("check.bounds", scalar_bool(false)?),
        ("keep.source", scalar_bool(false)?),
        (
            "keep.source.pkgs",
            scalar_bool(std::env::var("R_KEEP_PKG_SOURCE").is_ok_and(|value| value == "yes"))?,
        ),
        ("keep.parse.data", scalar_bool(true)?),
        ("keep.parse.data.pkgs", scalar_bool(false)?),
        ("warning.length", scalar_integer(1000)?),
        ("nwarnings", scalar_integer(50)?),
        ("OutDec", scalar_string(".")?),
        ("browserNLdisabled", scalar_bool(false)?),
        ("CBoundsCheck", scalar_bool(false)?),
        ("matprod", scalar_string("default")?),
        ("PCRE_study", scalar_integer(10)?),
        ("PCRE_use_JIT", scalar_bool(true)?),
        ("PCRE_limit_recursion", values::fresh_logical(NA_INTEGER)?),
        ("rl_word_breaks", scalar_string(" \t\n\"\\'`><=%;,|&{()}")?),
        ("warn", scalar_integer(0)?),
        ("timeout", scalar_integer(60)?),
        ("encoding", scalar_string("native.enc")?),
        ("show.error.messages", scalar_bool(true)?),
        ("scipen", scalar_integer(0)?),
        ("max.print", scalar_integer(99999)?),
        ("add.smooth", scalar_bool(true)?),
        ("stringsAsFactors", scalar_bool(false)?),
        ("useFancyQuotes", scalar_bool(true)?),
        ("catch.script.errors", scalar_bool(false)?),
    ];
    for (name, value) in defaults {
        set_option(name, value)?;
    }
    Ok(())
}

// Namespaces.

unsafe fn registry_symbol(name: Sexp) -> R<Sexp> {
    match name.kind() {
        SexpType::Symbol => Ok(name),
        SexpType::String if length(name) >= 1 => values::install_bytes(name.element(0).bytes()),
        _ => Err(error("bad namespace name")),
    }
}

unsafe fn do_get_registered_namespace(call: &Call<'_>) -> R<Sexp> {
    let symbol = registry_symbol(call.arg(0))?;
    let registry = crate::heap().known().namespace_registry;
    let value = find_var_in_frame(symbol, registry)?;
    Ok(if value == unbound() { nil() } else { value })
}

unsafe fn do_is_registered_namespace(call: &Call<'_>) -> R<Sexp> {
    let symbol = registry_symbol(call.arg(0))?;
    let registry = crate::heap().known().namespace_registry;
    scalar_bool(crate::heap().frame_cell(registry, symbol).is_some())
}

unsafe fn do_register_namespace(call: &Call<'_>) -> R<Sexp> {
    let symbol = registry_symbol(call.arg(0))?;
    let env = call.arg(1);
    let registry = crate::heap().known().namespace_registry;
    if crate::heap().frame_cell(registry, symbol).is_some() {
        return Err(call.error("namespace already registered"));
    }
    define(symbol, env, registry)?;
    Ok(nil())
}

unsafe fn do_unregister_namespace(call: &Call<'_>) -> R<Sexp> {
    let symbol = registry_symbol(call.arg(0))?;
    let registry = crate::heap().known().namespace_registry;
    if !crate::heap().remove_binding(registry, symbol)? {
        return Err(call.error("namespace not registered"));
    }
    Ok(nil())
}

unsafe fn do_import_into_env(call: &Call<'_>) -> R<Sexp> {
    let target = environment_arg(call.arg(0), "impenv")?;
    let target_names = call.arg(1);
    let source = environment_arg(call.arg(2), "expenv")?;
    let source_names = call.arg(3);
    if length(target_names) != length(source_names) {
        return Err(call.error("length of import and export names must match"));
    }
    let heap = crate::heap();
    for index in 0..length(target_names) {
        let target_symbol = values::install_bytes(target_names.element(index).bytes())?;
        let source_symbol = values::install_bytes(source_names.element(index).bytes())?;
        let mut env = source;
        let mut cell = None;
        while env != empty_env() {
            if let Some(found) = heap.frame_cell(env, source_symbol) {
                cell = Some(found);
                break;
            }
            env = env.enclosure();
        }
        let Some(cell) = cell else {
            return Err(call.error(format!(
                "exported symbol '{}' has no value",
                symbol_name(source_symbol)
            )));
        };
        if cell.has_gp(gp::ACTIVE_BINDING) {
            let function = cell.cdr();
            heap.define(target, target_symbol, function)?;
            if let Some(new_cell) = heap.frame_cell(target, target_symbol) {
                new_cell.set_gp_bits(gp::ACTIVE_BINDING, true);
            }
        } else {
            define(target_symbol, cell.cdr(), target)?;
        }
    }
    Ok(nil())
}

// Calling functions.

/// `do.call(what, args, envir)`.
unsafe fn do_do_call(call: &Call<'_>) -> R<Sexp> {
    let what = call.arg(0);
    let arguments = call.arg(1);
    let env = environment_arg(call.arg(2), "envir")?;
    if arguments.kind() != SexpType::List && arguments != nil() {
        return Err(call.error("second argument must be a list"));
    }
    let function_expression = match what.kind() {
        SexpType::String if length(what) == 1 => values::install_bytes(what.element(0).bytes())?,
        SexpType::Closure | SexpType::Builtin | SexpType::Special => what,
        _ => return Err(call.error("'what' must be a function or character string")),
    };
    let names = values::names(arguments);
    let mut items = vec![(nil(), function_expression)];
    for index in 0..length(arguments) {
        let tag = if names != nil() {
            let name = names.element(index);
            if name.bytes().is_empty() || name == values::na_string() {
                nil()
            } else {
                crate::heap().install_char(name)?
            }
        } else {
            nil()
        };
        // A symbol or call among the arguments is evaluated in `envir`, as in GNU R; the R
        // code quotes them first when `quote = TRUE`.
        items.push((tag, arguments.element(index)));
    }
    let new_call = values::pairlist(&items, SexpType::Language)?;
    eval::eval(new_call, env)
}

/// The function an apply-family builtin calls, with the call it reports.
unsafe fn call_function(function: Sexp, args: &[Arg], env: Sexp, call_expression: Sexp) -> R<Sexp> {
    eval::apply(call_expression, function, args, env)
}

/// `lapply(X, FUN)`: GNU R evaluates `FUN(X[[i]], ...)` in the closure's frame, so the call
/// each iteration reports is exactly that.
unsafe fn do_lapply(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let symbols = symbols();
    let x_symbol = values::install("X")?;
    let fun_symbol = values::install("FUN")?;
    let i_symbol = values::install("i")?;
    let list = eval::eval_symbol(x_symbol, env)?;
    let count = length(list);
    let result = alloc(SexpType::List, count)?;
    // `FUN(X[[i]], ...)`, with `i` bound afresh each time.
    let element = values::call(symbols.bracket2, &[x_symbol, i_symbol])?;
    let fun_call = values::call(fun_symbol, &[element, symbols.dots])?;
    for index in 0..count {
        let value = crate::scope(|| {
            let i = values::scalar_integer(index as i32 + 1)?;
            define(i_symbol, i, env)?;
            eval::force_and_call(fun_call, 1, env)
        })?;
        values::set_elt(result, index, value);
    }
    let names = values::names(list);
    if names != nil() {
        values::set_attr(result, symbols.names, names)?;
    }
    Ok(result)
}

unsafe fn do_vapply(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let symbols = symbols();
    let list = call.arg(0);
    let template = call.arg(2);
    let use_names = values::as_logical_scalar(call.arg(3)) == 1;
    let x_symbol = values::install("X")?;
    let fun_symbol = values::install("FUN")?;
    let i_symbol = values::install("i")?;
    let count = length(list);
    let template_length = length(template);
    let kind = template.kind();
    if !kind.is_vector() {
        return Err(call.error("'FUN.VALUE' must be a vector"));
    }
    let element = values::call(symbols.bracket2, &[x_symbol, i_symbol])?;
    let fun_call = values::call(fun_symbol, &[element, symbols.dots])?;
    let result = alloc(kind, count * template_length)?;
    let mut row_names = nil();
    for index in 0..count {
        let value = crate::scope(|| {
            let i = values::scalar_integer(index as i32 + 1)?;
            define(i_symbol, i, env)?;
            eval::force_and_call(fun_call, 1, env)
        })?;
        if length(value) != template_length {
            return Err(error(format!(
                "values must be length {template_length},\n but FUN(X[[{}]]) result is length {}",
                index + 1,
                length(value)
            )));
        }
        let compatible = value.kind() == kind
            || (kind == SexpType::Real
                && matches!(value.kind(), SexpType::Integer | SexpType::Logical))
            || (kind == SexpType::Integer && value.kind() == SexpType::Logical)
            || (kind == SexpType::List);
        if !compatible {
            return Err(error(format!(
                "values must be type '{}',\n but FUN(X[[{}]]) result is type '{}'",
                kind.name(),
                index + 1,
                value.kind().name()
            )));
        }
        let value = if value.kind() != kind && kind != SexpType::List {
            crate::coerce::coerce_vector(value, kind)?
        } else {
            value
        };
        if index == 0 && template_length > 1 {
            row_names = values::names(value);
        }
        for offset in 0..template_length {
            crate::subset::copy_element(value, offset, result, index * template_length + offset)?;
        }
    }
    if template_length != 1 {
        let dim = values::integer_vector(&[template_length as i32, count as i32])?;
        values::set_attr(result, symbols.dim, dim)?;
    }
    if use_names {
        let mut names = values::names(list);
        if names == nil() && list.kind() == SexpType::String {
            names = list;
        }
        if template_length == 1 {
            if names != nil() {
                values::set_attr(result, symbols.names, names)?;
            }
        } else if names != nil() || row_names != nil() {
            let dimnames = values::list_vector(&[row_names, names])?;
            values::set_attr(result, symbols.dimnames, dimnames)?;
        }
    }
    Ok(result)
}

unsafe fn do_mapply(call: &Call<'_>) -> R<Sexp> {
    let function = call.arg(0);
    let dots = call.arg(1);
    let more = call.arg(2);
    let lists = values::list_items(dots);
    let names = values::names(dots);
    // A classed argument may define its own `length` and `[[` methods, which mapply honours.
    let lengths: Vec<usize> = lists
        .iter()
        .map(|&list| crate::subset::dispatch_length(list, call.env))
        .collect::<R<_>>()?;
    let subset2 = crate::builtins::primitive("[[").ok_or_else(|| call.error("no [[ primitive"))?;
    let longest = if lengths.contains(&0) {
        0
    } else {
        lengths.iter().copied().max().unwrap_or(0)
    };
    let result = alloc(SexpType::List, longest)?;
    let more_names = values::names(more);
    for index in 0..longest {
        let value = crate::scope(|| {
            let mut args = Vec::new();
            for (position, &list) in lists.iter().enumerate() {
                let element = if list.is_object() {
                    let position_value =
                        values::scalar_integer((index % lengths[position]) as i32 + 1)?;
                    eval::apply(
                        nil(),
                        subset2,
                        &eval::value_args(&[list, position_value]),
                        call.env,
                    )?
                } else {
                    crate::subset::list_or_vector_element(list, index % lengths[position])?
                };
                let tag = if names != nil() && !names.element(position).bytes().is_empty() {
                    crate::heap().install_char(names.element(position))?
                } else {
                    nil()
                };
                args.push(Arg {
                    tag,
                    value: element,
                });
            }
            if more.kind() == SexpType::List {
                for position in 0..length(more) {
                    let tag = if more_names != nil() {
                        crate::heap().install_char(more_names.element(position))?
                    } else {
                        nil()
                    };
                    args.push(Arg {
                        tag,
                        value: more.element(position),
                    });
                }
            }
            call_function(function, &args, call.env, nil())
        })?;
        values::set_elt(result, index, value);
    }
    Ok(result)
}

unsafe fn do_rapply(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let function = call.arg(1);
    let classes = call.arg(2);
    let deflt = call.arg(3);
    let how = values::require_string(call.arg(4), "how")?;
    rapply_one(object, function, classes, deflt, &how, call.env)
}

unsafe fn rapply_one(
    object: Sexp,
    function: Sexp,
    classes: Sexp,
    deflt: Sexp,
    how: &str,
    env: Sexp,
) -> R<Sexp> {
    if object.kind() == SexpType::List && !values::inherits(object, "data.frame")
        || object.kind() == SexpType::List
            && how != "unlist"
            && values::inherits(object, "data.frame")
    {
        let count = length(object);
        let result = alloc(SexpType::List, count)?;
        for index in 0..count {
            let value = rapply_one(object.element(index), function, classes, deflt, how, env)?;
            values::set_elt(result, index, value);
        }
        let names = values::names(object);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
        }
        return Ok(result);
    }
    let class = crate::dispatch::class_of(object)?;
    let matches = (0..length(classes)).any(|index| {
        let wanted = classes.element(index).bytes();
        wanted == b"ANY"
            || (0..length(class)).any(|class_index| class.element(class_index).bytes() == wanted)
    });
    if matches {
        call_function(function, &eval::value_args(&[object]), env, nil())
    } else if how == "replace" {
        Ok(object)
    } else {
        Ok(deflt)
    }
}

unsafe fn do_eapply(call: &Call<'_>) -> R<Sexp> {
    let env = environment_arg(call.arg(0), "env")?;
    let function = call.arg(1);
    let all = values::as_logical_scalar(call.arg(2)) == 1;
    let bound = env_symbols(env, all, false);
    let result = alloc(SexpType::List, bound.len())?;
    for (index, &symbol) in bound.iter().enumerate() {
        let value = value_of(find_var_in_frame(symbol, env)?)?;
        let value = call_function(function, &eval::value_args(&[value]), call.env, nil())?;
        values::set_elt(result, index, value);
    }
    values::set_attr(result, symbols().names, symbol_names(&bound)?)?;
    Ok(result)
}

unsafe fn dots_of(env: Sexp) -> R<Sexp> {
    let value = find_var(symbols().dots, env)?;
    if value == unbound() {
        return Err(error(
            "incorrect context: the current call has no '...' to look in",
        ));
    }
    Ok(value)
}

unsafe fn do_dots_length(call: &Call<'_>) -> R<Sexp> {
    let dots = dots_of(call.env)?;
    let count = if dots.kind() == SexpType::Dots {
        values::pairlist_length(dots)
    } else {
        0
    };
    scalar_integer(count as i32)
}

unsafe fn do_dots_elt(call: &Call<'_>) -> R<Sexp> {
    let index = values::as_integer_scalar(call.arg(0));
    if index < 1 {
        return Err(call.error("indexing '...' with non-positive index"));
    }
    let symbol = values::install(&format!("..{index}"))?;
    // Forcing the element's promise decides the visibility, as evaluating `..n` would.
    interp().visible = true;
    eval::eval_symbol(symbol, call.env)
}

unsafe fn do_dots_names(call: &Call<'_>) -> R<Sexp> {
    let dots = dots_of(call.env)?;
    if dots.kind() != SexpType::Dots {
        return Ok(nil());
    }
    let items = values::pairlist_items(dots);
    if items.iter().all(|(tag, _)| *tag == nil()) {
        return Ok(nil());
    }
    let names: Vec<String> = items
        .iter()
        .map(|(tag, _)| {
            if *tag == nil() {
                String::new()
            } else {
                symbol_name(*tag).into_owned()
            }
        })
        .collect();
    string_vector(&names)
}

unsafe fn do_formals(call: &Call<'_>) -> R<Sexp> {
    let function = call.arg(0);
    if function.kind() == SexpType::Closure {
        Ok(function.car())
    } else {
        Ok(nil())
    }
}

unsafe fn do_body(call: &Call<'_>) -> R<Sexp> {
    let function = call.arg(0);
    if function.kind() == SexpType::Closure {
        Ok(crate::bytecode::closure_body(function))
    } else {
        if !values::is_function(function) {
            crate::conditions::warning("argument is not a function")?;
        }
        Ok(nil())
    }
}

unsafe fn do_args(call: &Call<'_>) -> R<Sexp> {
    let mut function = call.arg(0);
    if function.kind() == SexpType::String && length(function) == 1 {
        let symbol = values::install_bytes(function.element(0).bytes())?;
        function = eval::find_fun(symbol, call.env)?;
    }
    match function.kind() {
        SexpType::Closure => {
            let copy = values::triple(SexpType::Closure, function.car(), nil(), global_env())?;
            Ok(copy)
        }
        SexpType::Builtin | SexpType::Special => {
            // The formals of a primitive live in `.ArgsEnv` and `.GenericArgsEnv`.
            let name = values::primitive_name(function).to_owned();
            for env_name in [".ArgsEnv", ".GenericArgsEnv"] {
                let symbol = values::install(env_name)?;
                let env = value_of(find_var_in_frame(symbol, base_namespace())?)?;
                if env.kind() == SexpType::Environment {
                    let value = find_var_in_frame(values::install(&name)?, env)?;
                    if value != unbound() {
                        let value = value_of(value)?;
                        if value.kind() == SexpType::Closure {
                            return values::triple(
                                SexpType::Closure,
                                value.car(),
                                nil(),
                                global_env(),
                            );
                        }
                    }
                }
            }
            Ok(nil())
        }
        _ => Ok(nil()),
    }
}

/// `match.call(definition, call, expand.dots, envir)`.
unsafe fn do_match_call(call: &Call<'_>) -> R<Sexp> {
    let mut definition = call.arg(0);
    let target = call.arg(1);
    let expand_dots = values::as_logical_scalar(call.arg(2)) != 0;
    let dots_env = call.arg(3);
    if target.kind() != SexpType::Language {
        return Err(call.error("invalid 'call' argument"));
    }
    if definition == nil() {
        let head = target.car();
        if head.kind() == SexpType::Symbol {
            definition = eval::find_fun(head, call.env)?;
        }
    }
    if definition.kind() != SexpType::Closure {
        return Err(call.error("invalid 'definition' argument"));
    }
    // Expand `...` in the call from the environment that supplies it.
    let mut supplied: Vec<Arg> = Vec::new();
    for (tag, value) in values::pairlist_items(target.cdr()) {
        if value == symbols().dots {
            if dots_env.kind() == SexpType::Environment {
                let dots = find_var(symbols().dots, dots_env)?;
                if dots.kind() == SexpType::Dots {
                    for (dot_tag, dot_value) in values::pairlist_items(dots) {
                        let code = if dot_value.kind() == SexpType::Promise {
                            dot_value.cdr()
                        } else {
                            dot_value
                        };
                        supplied.push(Arg {
                            tag: dot_tag,
                            value: code,
                        });
                    }
                }
            }
            continue;
        }
        supplied.push(Arg { tag, value });
    }
    let formals = definition.car();
    let matched = eval::match_args(formals, &supplied, target)?;
    let formal_symbols: Vec<Sexp> = values::pairlist_items(formals)
        .into_iter()
        .map(|(tag, _)| tag)
        .collect();
    let mut items = vec![(nil(), target.car())];
    // Matched arguments come out in formal order; `...` contributes its contents.
    for (formal, value) in formal_symbols.iter().zip(matched.iter()) {
        if *value == missing_arg() {
            continue;
        }
        if *formal == symbols().dots {
            let dot_items = values::pairlist_items(*value);
            if expand_dots {
                for (tag, item) in dot_items {
                    items.push((tag, item));
                }
            } else {
                let list = values::pairlist(&dot_items, SexpType::Pairlist)?;
                items.push((*formal, list));
            }
        } else {
            items.push((*formal, *value));
        }
    }
    let result = values::pairlist(&items, SexpType::Language)?;
    Ok(result)
}

pub unsafe fn char_name(charsxp: Sexp) -> String {
    char_str(charsxp).into_owned()
}

pub unsafe fn present_or_nil(value: Option<Sexp>) -> Sexp {
    present(value).unwrap_or(nil())
}

/// The package or variable name of `pkg::name`: a symbol or a string.
unsafe fn colon_name(value: Sexp) -> R<String> {
    match value.kind() {
        SexpType::Symbol => Ok(symbol_name(value).into_owned()),
        SexpType::String if length(value) == 1 => {
            Ok(values::string_at(value, 0).unwrap_or_default().into_owned())
        }
        _ => Err(error("bad namespace name")),
    }
}

/// `pkg::name` (exported only) and `pkg:::name`.
unsafe fn namespace_value(call: &Call<'_>, exported: bool) -> R<Sexp> {
    let package = colon_name(call.arg(0))?;
    let name = colon_name(call.arg(1))?;
    let package_value = values::scalar_string(&package)?;
    namespace_lookup(package_value, &name, exported, call.env)
}

/// Looks `name` up in a package's namespace (a namespace environment or a package name),
/// loading it if needed. An exported name is looked up through the namespace's export table,
/// which maps it to the internal name, from the namespace outward, because a package may export
/// a function it imports, as graphics does with base's `plot`; lazy data comes last. An
/// internal name (`:::`) must be bound in the namespace itself.
unsafe fn namespace_lookup(package: Sexp, name: &str, exported: bool, env: Sexp) -> R<Sexp> {
    let symbol = values::install(name)?;
    let namespace = if package.kind() == SexpType::Environment {
        package
    } else if values::first_string(package).is_some_and(|package_name| package_name == "base") {
        base_namespace()
    } else {
        let as_namespace = eval::find_fun(values::install("asNamespace")?, base_namespace())?;
        eval::apply(nil(), as_namespace, &eval::value_args(&[package]), env)?
    };
    if namespace == base_namespace() || !exported {
        let value = find_var_in_frame(symbol, namespace)?;
        if value == unbound() {
            return Err(error(format!("object '{name}' not found")));
        }
        return value_of(value);
    }
    let info = find_var_in_frame(values::install(".__NAMESPACE__.")?, namespace)?;
    let table = |field: &str| -> R<Sexp> {
        if info.kind() != SexpType::Environment {
            return Ok(unbound());
        }
        let table = find_var_in_frame(values::install(field)?, info)?;
        if table.kind() != SexpType::Environment {
            return Ok(unbound());
        }
        find_var_in_frame(symbol, table)
    };
    let internal_name = table("exports")?;
    if internal_name != unbound() {
        let internal_symbol = match values::first_string(internal_name) {
            Some(internal) => values::install(&internal)?,
            None => symbol,
        };
        let value = eval::find_var(internal_symbol, namespace)?;
        if value == unbound() {
            return Err(error(format!("object '{name}' not found")));
        }
        return value_of(value);
    }
    let data = table("lazydata")?;
    if data != unbound() {
        return value_of(data);
    }
    let package_name = if info.kind() == SexpType::Environment {
        let spec = find_var_in_frame(values::install("spec")?, info)?;
        values::first_string(spec)
            .map(|text| text.into_owned())
            .unwrap_or_default()
    } else {
        values::first_string(package)
            .map(|text| text.into_owned())
            .unwrap_or_default()
    };
    Err(error(format!(
        "'{name}' is not an exported object from 'namespace:{package_name}'"
    )))
}
