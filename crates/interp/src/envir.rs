//! Environments, the call stack as R code sees it (`sys.call`, `parent.frame`, ...), the search
//! path, namespaces and options.

use {
    crate::{
        apply_results::VapplyResults,
        builtins::{BuiltinDef, BuiltinFn, Call, Generic, builtin, internal},
        bytecode, conditions,
        eval::{self, Reify, Site, define},
        handles::{
            self, Args, Binding, Bytecode, Cells, Chars, Closure, Cx, Elements, Environment,
            Expression, Gc, Integer, Language, List, Local, Logical, Owned, Pairlist, Promise,
            Real, Str, StringFill, Symbol, Tagged, Value, safe, text_vector,
        },
        interp,
        sort::collation_order,
        unwind::{ArgumentReader, R, error, missing_argument},
        values::{self, alloc, base_namespace, empty_env, global_env, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType, gp},
    smallvec::SmallVec,
    std::borrow::Cow,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("environment", 1, safe!(do_environment)),
    builtin("environment<-", 2, safe!(do_set_environment)),
    internal("new.env", 3, safe!(do_new_env)),
    internal("parent.env", 1, safe!(do_parent_env)),
    internal("parent.env<-", 2, safe!(do_set_parent_env)),
    builtin("globalenv", 0, safe!(do_global_environment)),
    builtin("emptyenv", 0, safe!(do_empty_environment)),
    builtin("baseenv", 0, safe!(do_base_environment)),
    internal("topenv", 2, safe!(do_topenv)),
    internal("environmentName", 1, safe!(do_environment_name)),
    internal("assign", 4, safe!(do_assign)).invisible(),
    internal("get", 4, safe!(do_get)),
    internal("get0", 5, safe!(do_get0)),
    internal("mget", 5, safe!(do_mget)),
    internal("exists", 4, safe!(do_exists)),
    internal("remove", 3, safe!(do_remove)).invisible(),
    internal("ls", 3, safe!(do_ls)),
    internal("env2list", 3, safe!(do_env2list)),
    internal("list2env", 2, safe!(do_list2env)),
    internal("lockEnvironment", 2, safe!(do_lock_environment)).invisible(),
    internal("environmentIsLocked", 1, safe!(do_environment_is_locked)),
    internal("lockBinding", 2, safe!(do_lock_binding)).invisible(),
    internal("unlockBinding", 2, safe!(do_unlock_binding)).invisible(),
    internal("bindingIsLocked", 2, safe!(do_binding_is_locked)),
    internal("makeActiveBinding", 3, safe!(do_make_active_binding)).invisible(),
    internal("bindingIsActive", 2, safe!(do_binding_is_active)),
    internal(
        "activeBindingFunction",
        2,
        safe!(do_active_binding_function),
    ),
    internal("sys.call", 1, safe!(do_sys_call)),
    internal("sys.function", 1, safe!(do_sys_function)),
    internal("sys.frame", 1, safe!(do_sys_frame)),
    internal("sys.nframe", 0, safe!(do_sys_nframe)),
    internal("sys.calls", 0, safe!(do_sys_calls)),
    internal("sys.frames", 0, safe!(do_sys_frames)),
    internal("sys.parent", 1, safe!(do_sys_parent)),
    internal("sys.parents", 0, safe!(do_sys_parents)),
    internal("sys.on.exit", 0, safe!(do_sys_on_exit)),
    internal("parent.frame", 1, safe!(do_parent_frame)),
    builtin("as.environment", 1, safe!(do_as_environment)).generic(Generic::Internal),
    builtin("pos.to.env", 1, safe!(do_pos_to_env)),
    internal("search", 0, safe!(do_search)),
    internal("attach", 3, safe!(do_attach)),
    internal("detach", 1, safe!(do_detach)),
    internal("options", -1, OPTIONS).keep_visibility(),
    internal("getOption", 1, safe!(do_get_option)),
    builtin("interactive", 0, safe!(do_interactive)),
    internal(
        "getRegisteredNamespace",
        1,
        safe!(do_get_registered_namespace),
    ),
    internal(
        "isRegisteredNamespace",
        1,
        safe!(do_is_registered_namespace),
    ),
    internal("registerNamespace", 2, safe!(do_register_namespace)).invisible(),
    internal("unregisterNamespace", 1, safe!(do_unregister_namespace)).invisible(),
    internal("getNamespaceRegistry", 0, safe!(do_get_namespace_registry)),
    internal("importIntoEnv", 4, safe!(do_import_into_env)).invisible(),
    internal("ryLocalS4Generics", 3, safe!(do_local_s4_generics)),
    internal("ryRegisterS3Methods", 5, safe!(do_register_s3_methods)),
    internal("isNamespaceEnv", 1, safe!(do_is_namespace_environment)),
    internal("do.call", 3, safe!(do_do_call)).keep_visibility(),
    internal("lapply", 2, do_lapply),
    internal("vapply", 4, do_vapply),
    internal("mapply", 3, safe!(do_mapply)),
    internal("rapply", 5, safe!(do_rapply)),
    internal("eapply", 4, safe!(do_eapply)),
    internal("Filter", -1, safe!(do_nothing)),
    internal("env.profile", 1, safe!(do_env_profile)),
    internal("emptyenv", 0, safe!(do_empty_environment)),
    builtin("...length", 0, safe!(do_dots_length)),
    builtin("...elt", 1, safe!(do_dots_elt)).keep_visibility(),
    builtin("...names", 0, safe!(do_dots_names)),
    crate::builtins::special("::", 2, safe!(do_exported_value)),
    crate::builtins::special(":::", 2, safe!(do_internal_value)),
    internal("getNamespaceValue", 3, safe!(do_get_namespace_value)),
    internal("formals", 1, safe!(do_formals)),
    internal("body", 1, safe!(do_body)),
    internal("args", 1, safe!(do_args)),
    internal("bodyCode", 1, safe!(do_body_code)),
    internal("match.call", 4, safe!(do_match_call)),
    internal("sys.source", -1, safe!(do_nothing)),
];

fn do_global_environment<'s>(_context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(handles::global_environment().value())
}

fn do_empty_environment<'s>(_context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(handles::empty_environment().value())
}

fn do_base_environment<'s>(_context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(handles::base_environment().value())
}

fn do_interactive<'s>(context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    handles::logical_constant(context, interp().interactive as i32)
}

fn do_get_namespace_registry<'s>(_context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(handles::namespace_registry().value())
}

fn do_is_namespace_environment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let namespace = is_namespace_environment(context, arguments.value(0).read(context));
    handles::logical_constant(context, namespace as i32)
}

/// The `.Internal`s of `Filter` and `sys.source`, which answer `NULL`.
fn do_nothing<'s>(_context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(handles::null())
}

/// An environment argument: an environment, or what `as.environment` makes of anything but
/// `NULL`. `what` names the argument in the error for a value that stands for none.
fn environment_arg<'s>(
    context: &mut Cx<'s>,
    value: Local<'s>,
    what: &str,
) -> R<Local<'s, Environment>> {
    if let Some(environment) = value.downcast::<Environment>() {
        return Ok(environment);
    }
    if value.r_type() == SexpType::Nil {
        return Err(error("use of NULL environment is defunct"));
    }
    environment_from(context, value).map_err(|_| error(format!("invalid '{what}' argument")))
}

/// The frame of the closure whose environment is `env`, as a stack index.
pub(crate) fn frame_of(env: Sexp) -> Option<usize> {
    interp().frames.iter().rposition(|frame| frame.owns(env))
}

/// GNU R's `cptr` for the `sys.*` functions: the frame the `sys.*` closure was called from.
/// `None` stands for the top level.
fn sys_origin(arguments: &Args<'_, '_>) -> Option<usize> {
    caller_index(arguments.frame_index()?)
}

/// The frame the call of frame `own` was made from, `None` for the top level. A `NULL`
/// `sys_parent` stands for a slot frame that had no environment when it made the call, whose
/// position the frame records: promise code running on a slot frame's slots calls from below
/// deeper frames.
pub(crate) fn caller_index(own: usize) -> Option<usize> {
    let frames = &interp().frames;
    let target = frames[own].sys_parent.raw();
    if target == nil() {
        let caller = frames[own].caller;
        return (caller != crate::NO_SLOTS).then_some(caller as usize);
    }
    (0..=own).rev().find(|&index| frames[index].owns(target))
}

/// `framedepth`: how many frames from `origin` down to the top level.
fn frame_depth(origin: Option<usize>) -> usize {
    origin.map_or(0, |index| index + 1)
}

fn sys_frame_at<'s>(context: &mut Cx<'s>, n: i32, origin: Option<usize>) -> R<Local<'s>> {
    if n == 0 {
        return Ok(handles::global_environment().value());
    }
    if n == NA_INTEGER {
        return Err(error("NA argument is invalid"));
    }
    let depth = frame_depth(origin) as i64;
    let back = if n > 0 { depth - n as i64 } else { -(n as i64) };
    if back < 0 {
        return Err(error("not that many frames on the stack"));
    }
    match origin {
        Some(index) if back <= index as i64 => context.root_answer(|context| {
            eval::frame_environment_of(context, index - back as usize, Reify::Site(Site::SysFrame))
        }),
        _ if back == depth => Ok(handles::global_environment().value()),
        _ => Err(error("not that many frames on the stack")),
    }
}

/// `sys.frame(which)` as seen from the frame of `environment`.
pub fn sys_frame<'s>(
    context: &mut Cx<'s>,
    which: i32,
    environment: Local<'_, Environment>,
) -> R<Local<'s, Environment>> {
    let frame = sys_frame_at(context, which, frame_of(environment.raw()))?;
    frame
        .downcast::<Environment>()
        .ok_or_else(|| error("internal error: a frame without an environment"))
}

fn sys_call_at<'s>(
    context: &mut Cx<'s>,
    n: i32,
    origin: Option<usize>,
    function: bool,
) -> R<Local<'s>> {
    let depth = frame_depth(origin) as i64;
    let back = if n > 0 { depth - n as i64 } else { -(n as i64) };
    if back < 0 {
        return Err(error("not that many frames on the stack"));
    }
    match origin {
        Some(index) if back <= index as i64 => {
            let index = index - back as usize;
            if function {
                Ok(context.root(interp().frames[index].function.get(context)))
            } else {
                call_with_srcref(context, index)
            }
        }
        _ if back == depth => Ok(handles::null()),
        _ => Err(error("not that many frames on the stack")),
    }
}

/// A copy of the call of frame `index`, with the srcref current when the call was made as its
/// `srcref` attribute, as GNU R's `sys.call` returns it.
///
/// Only the call's cells are copied, as GNU R's `shallow_duplicate` does: `do.call` puts the
/// argument values themselves into the call, so a deep copy would cost as much as the arguments
/// (data.table's `cedta()` runs `sys.calls()` on every `[`). The copy's cells add a reference
/// to each argument, so writing into one through the returned call copies it first.
pub fn call_with_srcref<'s>(context: &mut Cx<'s>, index: usize) -> R<Local<'s>> {
    let call = context.root(interp().frames[index].call.get(context));
    let mut copy = handles::shallow_copy(context, call)?;
    let srcref = interp().frames[index].srcref.get(context);
    if srcref.r_type() != SexpType::Nil {
        let srcref = context.root(srcref);
        let srcref = handles::deep_copy(context, srcref)?.finish();
        handles::set_attribute(
            context,
            &mut copy,
            handles::symbol(|symbols| symbols.srcref),
            srcref,
        )?;
    }
    Ok(copy.finish())
}

/// `R_sysparent`: the frame number of the parent of frame `n` counted back from `origin`.
pub(crate) fn sys_parent_number(mut n: i64, origin: Option<usize>) -> i64 {
    let frames = &interp().frames;
    let Some(mut index) = origin else {
        return 0;
    };
    while index > 0 && n > 1 {
        n -= 1;
        index -= 1;
    }
    let parent = frames[index].sys_parent.raw();
    if parent == global_env() {
        return 0;
    }
    // A slot frame that called with no environment is recorded by position, which is not the
    // frame below when promise code on its slots made the call from deeper frames.
    if parent == nil() {
        let caller = frames[index].caller;
        return if caller == crate::NO_SLOTS {
            index as i64
        } else {
            caller as i64 + 1
        };
    }
    let mut count = 0;
    let mut found = n;
    for position in (0..=index).rev() {
        count += 1;
        if frames[position].owns(parent) {
            found = count;
        }
    }
    (count - found + 1).max(0)
}

/// The `which` or `n` argument of a `sys.*` function, 0 for one that takes none.
fn which_argument(context: &mut Cx<'_>, arguments: &Args<'_, '_>) -> i32 {
    if arguments.is_empty() {
        0
    } else {
        handles::integer_scalar(context, arguments.value(0))
    }
}

fn do_sys_call<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let n = which_argument(context, arguments);
    if n == NA_INTEGER {
        return Err(error("invalid 'which' argument"));
    }
    sys_call_at(context, n, sys_origin(arguments), false)
}

fn do_sys_function<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let n = which_argument(context, arguments);
    if n == NA_INTEGER {
        return Err(error("invalid 'which' value"));
    }
    sys_call_at(context, n, sys_origin(arguments), true)
}

fn do_sys_frame<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let n = which_argument(context, arguments);
    sys_frame_at(context, n, sys_origin(arguments))
}

fn do_sys_nframe<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    handles::integer(context, frame_depth(sys_origin(arguments)) as i32)
}

fn do_sys_calls<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let origin = sys_origin(arguments);
    let mut items = Cells::new(SexpType::Pairlist);
    for index in 1..=frame_depth(origin) {
        let call = sys_call_at(context, index as i32, origin, false)?;
        items.push(context, handles::null(), call)?;
    }
    Ok(items.finish().map_or_else(handles::null, Owned::finish))
}

fn do_sys_frames<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let origin = sys_origin(arguments);
    let mut items = Cells::new(SexpType::Pairlist);
    for index in 1..=frame_depth(origin) {
        let frame = sys_frame_at(context, index as i32, origin)?;
        items.push(context, handles::null(), frame)?;
    }
    Ok(items.finish().map_or_else(handles::null, Owned::finish))
}

fn do_sys_parent<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let n = which_argument(context, arguments);
    if n == NA_INTEGER {
        return Err(error("invalid 'n' argument"));
    }
    let origin = sys_origin(arguments);
    let depth = frame_depth(origin) as i64;
    let mut position = depth;
    for _ in 0..n.max(0) {
        position = sys_parent_number(depth - position + 1, origin);
    }
    handles::integer(context, position as i32)
}

fn do_sys_parents<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let origin = sys_origin(arguments);
    let depth = frame_depth(origin);
    let mut parents = context.alloc::<Integer>(depth)?;
    for (index, parent) in parents.as_mut_slice().iter_mut().enumerate() {
        *parent = sys_parent_number((depth - index) as i64, origin) as i32;
    }
    Ok(parents.finish().value())
}

fn do_sys_on_exit<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(index) = sys_origin(arguments) else {
        return Ok(handles::null());
    };
    let count = interp().frames[index].on_exit.len();
    if count < 2 {
        // A slot frame's list may hold the expression compiled for its slots.
        return Ok(match interp().frames[index].on_exit.first() {
            Some(single) => context.root(body_source(single.get(context))),
            None => handles::null(),
        });
    }
    let mut items = Cells::new(SexpType::Language);
    items.push(
        context,
        handles::null(),
        handles::permanent_symbol(|symbols| symbols.brace).value(),
    )?;
    for position in 0..count {
        let expression = context.root(body_source(
            interp().frames[index].on_exit[position].get(context),
        ));
        items.push(context, handles::null(), expression)?;
    }
    Ok(items.finish().map_or_else(handles::null, Owned::finish))
}

/// The source of a closure body, compiled or not.
fn body_source(body: Gc<'_>) -> Gc<'_> {
    match body.downcast::<Bytecode>() {
        Some(code) => handles::constants(code)
            .and_then(|constants| constants.get(bytecode::SOURCE).copied())
            .unwrap_or(body),
        None => body,
    }
}

fn do_parent_frame<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let value = arguments.value(0);
    // `n` is almost always its default, the double 1, which needs no coercion.
    let default = matches!(
        value.standard::<Real>().map(|real| real.as_slice(context)),
        Some(&[number]) if number == 1.0
    );
    let mut n = if default {
        1
    } else {
        handles::integer_scalar(context, value)
    };
    if n == NA_INTEGER || n < 1 {
        return Err(error("invalid 'n' value"));
    }
    let Some(mut index) = arguments.frame_index() else {
        return Ok(handles::global_environment().value());
    };
    // Each step moves to the frame the current one was called from, as GNU R's `parent.frame`
    // does through `sys_parent`. A slot frame without an environment hands out its pending one,
    // so code that only passes the frame on or compares it leaves the frame in its slots.
    loop {
        let Some(caller) = caller_index(index) else {
            return Ok(handles::global_environment().value());
        };
        if n == 1 {
            return context.root_answer(|context| {
                eval::caller_environment(context, caller, Reify::Site(Site::ParentFrame))
            });
        }
        n -= 1;
        index = caller;
    }
}

fn do_environment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let function = arguments.value(0);
    if let Some(closure) = function.downcast::<Closure>() {
        return Ok(context.root(closure.read(context).environment()).value());
    }
    match function.r_type() {
        SexpType::Nil => match arguments.frame_index() {
            Some(index) => {
                handles::frame_sys_parent(context, index, Reify::Site(Site::EnvironmentCall))
            }
            None => Ok(handles::global_environment().value()),
        },
        SexpType::Builtin | SexpType::Special => Ok(handles::null()),
        _ => {
            let environment = handles::get_attribute(
                context,
                function,
                handles::symbol(|symbols| symbols.dot_environment),
            );
            Ok(context.root(environment))
        }
    }
}

fn do_set_environment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let mut environment = arguments.value(1);
    // GNU R's `simple_as_environment`: an S4 object extending `environment`, such as a
    // reference class object, stands for the environment in its data part.
    // Any other S4 object counts as `NULL` for a closure and is refused for anything else.
    if environment.r_type() == SexpType::Object {
        let data_part = crate::subset::s4_data_part(context, environment);
        if data_part.r_type() == SexpType::Environment {
            environment = context.root(data_part);
        } else if object.r_type() == SexpType::Closure {
            environment = handles::null();
        }
    }
    if let Some(function) = object.downcast::<Closure>() {
        if environment.r_type() == SexpType::Nil {
            return Err(arguments.error("use of NULL environment is defunct"));
        }
        let Some(environment) = environment.downcast::<Environment>() else {
            return Err(arguments.error("replacement object is not an environment"));
        };
        let mut copy = handles::rebound_closure(context, function, environment)?;
        handles::copy_most_attributes(context, object, &mut copy, true)?;
        return Ok(copy.finish().value());
    }
    if environment.r_type() != SexpType::Nil && environment.r_type() != SexpType::Environment {
        return Err(arguments.error("replacement object is not an environment"));
    }
    if matches!(object.r_type(), SexpType::Builtin | SexpType::Special) {
        if environment.r_type() != SexpType::Nil {
            conditions::warn(
                context,
                "setting environment(<primitive function>) is not possible and trying it is deprecated",
            )?;
        }
        return Ok(object);
    }
    let mut copy = if object.is_shared() {
        handles::shallow_copy(context, object)?
    } else {
        handles::unshared(context, object)?
    };
    handles::set_attribute(
        context,
        &mut copy,
        handles::symbol(|symbols| symbols.dot_environment),
        environment,
    )?;
    Ok(copy.finish())
}

fn do_new_env<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let hash = handles::integer_scalar_warning(context, arguments.value(0))?;
    let parent = arguments.value(1);
    if parent.r_type() == SexpType::Nil {
        return Err(error("use of NULL environment is defunct"));
    }
    let parent = simple_as_environment(context, parent)
        .ok_or_else(|| error("'enclos' must be an environment"))?;
    let environment = handles::new_environment(context, parent)?;
    if hash != 0 {
        let size = handles::integer_scalar_warning(context, arguments.value(2))?;
        handles::set_frame_hashed(
            context,
            environment,
            if size == NA_INTEGER { 0 } else { size },
        );
    }
    Ok(environment.value())
}

fn do_parent_env<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    // GNU R takes an environment or an S4 object extending one, never a search position as
    // `as.environment` does.
    let environment = simple_as_environment(context, arguments.value(0))
        .ok_or_else(|| error("argument is not an environment"))?;
    match environment.read(context).enclosure() {
        Some(enclosure) => Ok(context.root(enclosure).value()),
        None => Err(arguments.error("the empty environment has no parent")),
    }
}

fn do_set_parent_env<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = environment_arg(context, arguments.value(0), "env")?;
    let Some(parent) = arguments.value(1).downcast::<Environment>() else {
        return Err(arguments.error("'parent' is not an environment"));
    };
    if environment.is(handles::empty_environment()) {
        return Err(arguments.error("can not set the parent of the empty environment"));
    }
    handles::set_enclosure(context, environment, parent);
    Ok(environment.value())
}

fn do_topenv<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    // A non-environment falls back to the frame `.Internal(topenv())` runs in, whose top level
    // is base's namespace, as in GNU R (`topenv(sum)`).
    let environment = match arguments.value(0).downcast::<Environment>() {
        Some(environment) => environment,
        None => arguments.environment(context)?,
    };
    Ok(top_environment(context, environment, arguments.value(1)).value())
}

/// `topenv(env, target)` for raw callers.
pub unsafe fn top_env(env: Sexp, target: Sexp) -> R<Sexp> {
    handles::with(|context| {
        let env = handles::held(context, env);
        let target = handles::held(context, target);
        let Some(env) = env.downcast::<Environment>() else {
            return Ok(global_env());
        };
        Ok(top_environment(context, env, target).raw())
    })
}

/// `topenv(env, target)`: the first environment from `env` outward that is `target`, the global
/// or base environment, a namespace, a package environment, or one that binds `.packageName`.
pub fn top_environment<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s, Environment>,
    target: Local<'_>,
) -> Local<'s, Environment> {
    let mut current = environment;
    while !current.is(handles::empty_environment()) {
        if current.is(target)
            || current.is(handles::global_environment())
            || current.is(handles::base_environment())
            || current.is(handles::base_namespace())
            || is_namespace_environment(context, current.read(context).value())
            || is_package_environment(context, current.value())
            || handles::frame_binding(
                context,
                current,
                handles::symbol(|symbols| symbols.dot_package_name),
            )
            .is_some()
        {
            return current;
        }
        let Some(enclosure) = current.read(context).enclosure() else {
            break;
        };
        current = context.root(enclosure);
    }
    handles::global_environment()
}

/// Whether `env` is a namespace, for raw callers.
pub unsafe fn is_namespace(env: Sexp) -> bool {
    handles::reading(|context| {
        is_namespace_environment(context, handles::held(context, env).read(context))
    })
}

/// Whether `environment` is a namespace: the base namespace, or an environment holding
/// `.__NAMESPACE__.`.
pub fn is_namespace_environment(context: &Cx<'_>, environment: Gc<'_>) -> bool {
    if environment.is(handles::base_namespace()) {
        return true;
    }
    let Some(environment) = environment.downcast::<Environment>() else {
        return false;
    };
    handles::frame_binding(
        context,
        environment,
        handles::symbol(|symbols| symbols.dot_namespace),
    )
    .is_some_and(|binding| binding.content().r_type() == SexpType::Environment)
}

/// Whether `env` is a package environment, for raw callers.
pub unsafe fn is_package_env(env: Sexp) -> bool {
    handles::with(|context| {
        let env = handles::held(context, env);
        Ok(is_package_environment(context, env))
    })
    .is_ok_and(|package| package)
}

/// Whether `environment` is a package environment on the search path (its `name` starts
/// `package:`).
pub fn is_package_environment(context: &mut Cx<'_>, environment: Local<'_>) -> bool {
    let name = handles::get_attribute(
        context,
        environment,
        handles::symbol(|symbols| symbols.name),
    );
    let name = context.root(name);
    handles::first_string(context, name).is_some_and(|text| text.starts_with("package:"))
}

/// The name of a namespace, from its `spec`, for raw callers. Reading the name forces the
/// namespace information, and a failure to reads as no name.
pub unsafe fn namespace_name(env: Sexp) -> Option<String> {
    handles::with(|context| {
        let env = handles::held(context, env);
        namespace_spec_name(context, env)
    })
    .ok()
    .flatten()
}

/// The name of a namespace, from its `spec`.
pub fn namespace_spec_name<'s>(context: &mut Cx<'s>, environment: Local<'s>) -> R<Option<String>> {
    if environment.is(handles::base_namespace()) {
        return Ok(Some("base".into()));
    }
    let Some(environment) = environment.downcast::<Environment>() else {
        return Ok(None);
    };
    let Some(binding) = handles::frame_binding(
        context,
        environment,
        handles::symbol(|symbols| symbols.dot_namespace),
    ) else {
        return Ok(None);
    };
    let information = context.root(binding.content());
    let information = eval::forced_value(context, information)?;
    let Some(information) = information.downcast::<Environment>() else {
        return Ok(None);
    };
    let Some(spec) = eval::frame_content(
        context,
        information,
        handles::symbol(|symbols| symbols.spec),
    )?
    else {
        return Ok(None);
    };
    let spec = eval::forced_value(context, spec)?;
    Ok(handles::first_string(context, spec).map(Cow::into_owned))
}

fn do_environment_name<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(environment) = arguments.value(0).downcast::<Environment>() else {
        return Ok(text_vector(context, &[""])?.value());
    };
    let name = if environment.is(handles::global_environment()) {
        "R_GlobalEnv".to_owned()
    } else if environment.is(handles::base_environment()) {
        "base".to_owned()
    } else if environment.is(handles::empty_environment()) {
        "R_EmptyEnv".to_owned()
    } else if !is_package_environment(context, environment.value())
        && is_namespace_environment(context, environment.read(context).value())
    {
        namespace_spec_name(context, environment.value())?.unwrap_or_default()
    } else {
        let name = handles::get_attribute(
            context,
            environment,
            handles::symbol(|symbols| symbols.name),
        );
        let name = context.root(name);
        handles::first_string(context, name)
            .map(Cow::into_owned)
            .unwrap_or_default()
    };
    Ok(text_vector(context, &[name])?.value())
}

/// The text of a one-element character argument, as GNU R's `asChar` reads `mode` and `pos`.
fn required_text<'a>(context: &'a mut Cx<'_>, value: Local<'_>, what: &str) -> R<Cow<'a, str>> {
    if handles::length(context, value) == 1
        && let Some(text) = handles::first_string(context, value)
    {
        return Ok(text);
    }
    Err(error(format!("invalid '{what}' argument")))
}

/// The symbol a variable-name argument names. `invalid` is the error for anything else, and for
/// an empty name `get`, `get0` and `exists` give it where `assign` refuses the empty symbol.
fn name_symbol<'s>(
    context: &mut Cx<'s>,
    value: Local<'s>,
    invalid: &str,
    empty_is_invalid: bool,
) -> R<Local<'s, Symbol>> {
    if let Some(symbol) = value.downcast::<Symbol>() {
        return Ok(symbol);
    }
    let names = match handles::dense::<Str>(context, value)? {
        Some(names) if !names.is_empty() => names,
        _ => return Err(error(invalid)),
    };
    if names.get(context, 0).bytes().is_empty() {
        if empty_is_invalid {
            return Err(error(invalid));
        }
        return Err(error("attempt to use zero-length variable name"));
    }
    Ok(handles::interned_symbol(handles::install_name_at(
        context, names, 0,
    )?))
}

fn symbol_text(context: &Cx<'_>, symbol: Local<'_, Symbol>) -> String {
    handles::text(symbol.read(context).name()).into_owned()
}

fn do_assign<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let symbol = name_symbol(context, arguments.value(0), "invalid first argument", false)?;
    let value = arguments.value(1);
    let environment = environment_arg(context, arguments.value(2), "envir")?;
    if handles::logical_scalar(context, arguments.value(3)) == 1 {
        eval::assign_outward(context, symbol, value, environment)?;
    } else {
        handles::assign_in_frame(context, symbol, value, environment)?;
    }
    Ok(value)
}

/// Whether `value` matches the `mode` argument of `get` and `exists`.
fn mode_matches(value: Local<'_>, mode: &str) -> bool {
    match mode {
        "any" => true,
        "function" => values::is_function_type(value.r_type()),
        "numeric" => matches!(value.r_type(), SexpType::Integer | SexpType::Real),
        "name" => value.r_type() == SexpType::Symbol,
        "call" => value.r_type() == SexpType::Language,
        "list" => value.r_type() == SexpType::List,
        mode => value.r_type().name() == mode,
    }
}

/// Looks `symbol` up for `get`, `exists` and `mget`: in `environment`'s frame, and with
/// `inherits` in every enclosure, skipping values of the wrong mode. With mode `"any"` a
/// promise comes back unforced.
fn lookup<'s>(
    context: &mut Cx<'s>,
    symbol: Local<'s, Symbol>,
    environment: Local<'s, Environment>,
    mode: &str,
    inherits: bool,
) -> R<Option<Local<'s>>> {
    let mut frame = environment;
    while !frame.is(handles::empty_environment()) {
        if let Some(value) = eval::frame_content(context, frame, symbol)? {
            if mode == "any" {
                return Ok(Some(value));
            }
            let forced = eval::forced_value(context, value)?;
            if mode_matches(forced, mode) {
                return Ok(Some(forced));
            }
        }
        if !inherits {
            break;
        }
        let Some(enclosure) = frame.read(context).enclosure() else {
            break;
        };
        frame = context.root(enclosure);
    }
    Ok(None)
}

/// The arguments `get`, `get0` and `exists` share: the symbol, the environment, the mode and
/// `inherits`.
fn lookup_arguments<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
) -> R<(Local<'s, Symbol>, Local<'s, Environment>, String, bool)> {
    let symbol = name_symbol(context, arguments.value(0), "invalid first argument", true)?;
    let environment = environment_arg(context, arguments.value(1), "envir")?;
    let mode = required_text(context, arguments.value(2), "mode")?.into_owned();
    let inherits = handles::logical_scalar(context, arguments.value(3)) == 1;
    Ok((symbol, environment, mode, inherits))
}

/// The value `get` and `get0` give for what [`lookup`] found.
fn found_value<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
    symbol: Local<'s, Symbol>,
    value: Local<'s>,
) -> R<Local<'s>> {
    if handles::is_missing_argument(value) {
        return Err(missing_argument(
            arguments.expression().raw(),
            &symbol_text(context, symbol),
            ArgumentReader::Get,
        ));
    }
    eval::forced_value(context, value)
}

fn do_get<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let (symbol, environment, mode, inherits) = lookup_arguments(context, arguments)?;
    match lookup(context, symbol, environment, &mode, inherits)? {
        Some(value) => found_value(context, arguments, symbol, value),
        None if mode == "any" => Err(error(format!(
            "object '{}' not found",
            symbol_text(context, symbol)
        ))),
        None => Err(error(format!(
            "object '{}' of mode '{mode}' was not found",
            symbol_text(context, symbol)
        ))),
    }
}

fn do_get0<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let (symbol, environment, mode, inherits) = lookup_arguments(context, arguments)?;
    match lookup(context, symbol, environment, &mode, inherits)? {
        Some(value) => found_value(context, arguments, symbol, value),
        None => Ok(arguments.value(4)),
    }
}

fn do_mget<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(names) = handles::dense::<Str>(context, arguments.value(0))? else {
        return Err(error("invalid first argument"));
    };
    let environment = environment_arg(context, arguments.value(1), "envir")?;
    let count = names.len();
    let modes = match handles::dense::<Str>(context, arguments.value(2))? {
        Some(modes) if modes.len() == 1 || modes.len() == count => modes,
        _ => return Err(error("wrong length for 'mode' argument")),
    };
    // GNU R takes any vector here and treats its elements as a list's, so `ifnotfound = NA`
    // gives `NA` for every missing name.
    let if_not_found = arguments.value(3);
    if !if_not_found.r_type().is_vector() {
        return Err(error("invalid 'ifnotfound' argument"));
    }
    let fallbacks = handles::length(context, if_not_found);
    if fallbacks != 1 && fallbacks != count {
        return Err(error("wrong length for 'ifnotfound' argument"));
    }
    let if_not_found = handles::coerce(context, if_not_found, SexpType::List)?;
    let Some(if_not_found) = if_not_found.standard::<List>() else {
        return Err(error("invalid 'ifnotfound' argument"));
    };
    let inherits = handles::logical_scalar(context, arguments.value(4)) == 1;
    let mut result = context.alloc::<List>(count)?;
    for index in 0..count {
        let name = names.get(context, index);
        let name = if handles::is_na(name) {
            String::new()
        } else {
            handles::text(name).into_owned()
        };
        let symbol = handles::install_bytes(context, name.as_bytes())?;
        let symbol = context.root(symbol);
        let mode = modes.get(context, index % modes.len());
        let mode = if handles::is_na(mode) {
            "any".to_owned()
        } else {
            handles::text(mode).into_owned()
        };
        let value = match lookup(context, symbol, environment, &mode, inherits)? {
            Some(value) => eval::forced_value(context, value)?,
            None => {
                let fallback = context.root(if_not_found.get(context, index % fallbacks));
                if values::is_function_type(fallback.r_type()) {
                    let argument = text_vector(context, &[name])?.value();
                    let caller = arguments.environment(context)?;
                    handles::apply(
                        context,
                        handles::null(),
                        fallback,
                        &[Tagged {
                            tag: handles::null(),
                            value: argument,
                        }],
                        caller,
                    )?
                } else {
                    fallback
                }
            }
        };
        result.set(context, index, value);
    }
    let mut result = result.owned();
    handles::set_attribute(
        context,
        &mut result,
        handles::symbol(|symbols| symbols.names),
        names.value(),
    )?;
    Ok(result.finish().value())
}

fn do_exists<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let (symbol, environment, mode, inherits) = lookup_arguments(context, arguments)?;
    if mode != "any" {
        let found = lookup(context, symbol, environment, &mode, inherits)?.is_some();
        return handles::logical_constant(context, found as i32);
    }
    // As in GNU R, an active binding exists without its function being called.
    let mut frame = environment.read(context);
    let found = loop {
        if frame.is(handles::empty_environment()) {
            break false;
        }
        if let Some(binding) = handles::frame_binding(context, frame, symbol)
            && (binding.is_active() || !handles::is_unbound(binding.content()))
        {
            break true;
        }
        match frame.enclosure() {
            Some(enclosure) if inherits => frame = enclosure,
            _ => break false,
        }
    };
    handles::logical_constant(context, found as i32)
}

fn do_remove<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let names = handles::dense::<Str>(context, arguments.value(0))?;
    let environment = environment_arg(context, arguments.value(1), "envir")?;
    let inherits = handles::logical_scalar(context, arguments.value(2)) == 1;
    let Some(names) = names else {
        return Ok(handles::null());
    };
    for index in 0..names.len() {
        let name = names.get(context, index);
        let name = if handles::is_na(name) {
            String::new()
        } else {
            handles::text(name).into_owned()
        };
        let symbol = handles::install_bytes(context, name.as_bytes())?;
        let mut frame = environment;
        let mut done = false;
        while !frame.is(handles::empty_environment()) {
            if handles::remove_binding(context, frame, symbol)? {
                done = true;
                break;
            }
            if !inherits {
                break;
            }
            let Some(enclosure) = frame.read(context).enclosure() else {
                break;
            };
            frame = context.root(enclosure);
        }
        if !done {
            conditions::warn(context, format!("object '{name}' not found"))?;
        }
    }
    Ok(handles::null())
}

/// The symbols bound in `env`, in the order `ls()` lists their names, for raw callers.
pub unsafe fn env_symbols(env: Sexp, all: bool, sorted: bool) -> Vec<Sexp> {
    handles::reading(|context| {
        environment_symbols(
            context,
            handles::held(context, env).read(context),
            all,
            sorted,
        )
        .into_iter()
        .map(Gc::raw)
        .collect()
    })
}

/// The symbols bound in `environment`, in the order `ls()` lists their names: sorted by the
/// collation order, or in the frame's listing order.
pub fn environment_symbols(
    context: &Cx<'_>,
    environment: Gc<'_>,
    all: bool,
    sorted: bool,
) -> Vec<Gc<'static, Symbol>> {
    let Some(environment) = environment.downcast::<Environment>() else {
        return Vec::new();
    };
    let mut symbols = handles::bound_symbols(context, environment, !sorted);
    if !all {
        symbols.retain(|symbol| symbol.name().bytes().first() != Some(&b'.'));
    }
    if sorted {
        let names: Vec<_> = symbols
            .iter()
            .map(|symbol| handles::text(symbol.name()))
            .collect();
        let order = collation_order(&names);
        symbols = order
            .into_iter()
            .map(|index| symbols[index as usize])
            .collect();
    }
    symbols
}

/// The names bound in `environment` as a character vector, as `ls()` lists them. A symbol's
/// print name is already an interned `CHARSXP`, so no name is copied.
pub fn environment_names<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s, Environment>,
    all: bool,
    sorted: bool,
) -> R<Local<'s>> {
    let symbols = environment_symbols(context, environment.read(context).value(), all, sorted);
    symbol_names(context, &symbols)
}

fn symbol_names<'s>(context: &mut Cx<'s>, symbols: &[Gc<'static, Symbol>]) -> R<Local<'s>> {
    let mut names = context.alloc::<Str>(symbols.len())?;
    for (index, symbol) in symbols.iter().enumerate() {
        names.set(context, index, symbol.name());
    }
    Ok(names.finish().value())
}

fn do_ls<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = environment_arg(context, arguments.value(0), "envir")?;
    let all = handles::logical_scalar(context, arguments.value(1)) == 1;
    let sorted = handles::logical_scalar(context, arguments.value(2)) != 0;
    environment_names(context, environment, all, sorted)
}

fn do_env2list<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = arguments.value(0);
    if environment.r_type() == SexpType::Nil {
        return Err(error("use of NULL environment is defunct"));
    }
    let environment = simple_as_environment(context, environment)
        .ok_or_else(|| error("argument must be an environment"))?;
    let all = handles::logical_scalar(context, arguments.value(1)) == 1;
    let sorted = handles::logical_scalar(context, arguments.value(2)) == 1;
    let bound = environment_symbols(context, environment.read(context).value(), all, sorted);
    let mut result = context.alloc::<List>(bound.len())?;
    for (index, &symbol) in bound.iter().enumerate() {
        let value = match eval::frame_content(context, environment, symbol)? {
            Some(value) => eval::forced_value(context, value)?,
            None => handles::unbound(),
        };
        result.set(context, index, value);
    }
    let mut result = result.owned();
    if !bound.is_empty() {
        let names = symbol_names(context, &bound)?;
        handles::set_attribute(
            context,
            &mut result,
            handles::symbol(|symbols| symbols.names),
            names,
        )?;
    }
    Ok(result.finish().value())
}

/// A new environment holding a named list's elements, enclosed by `enclosure`, or by the base
/// environment when there is none. An empty name is skipped, as `eval` and `substitute` over a
/// list skip one.
pub fn list_to_environment<'s>(
    context: &mut Cx<'s>,
    list: Local<'s>,
    enclosure: Option<Local<'_, Environment>>,
) -> R<Local<'s, Environment>> {
    let enclosure = enclosure.unwrap_or_else(handles::base_environment);
    let environment = handles::new_environment(context, enclosure)?;
    fill_environment(context, environment, list, EmptyName::Skip)?;
    Ok(environment)
}

/// GNU R's `list2env` rejects an empty name, while `eval` and `substitute` over a list skip
/// one, so the caller says which.
fn fill_environment<'s>(
    context: &mut Cx<'s>,
    environment: Local<'s, Environment>,
    list: Local<'s>,
    empty_name: EmptyName,
) -> R<()> {
    let names = handles::names_of(context, list)?;
    let Some(names) = handles::dense::<Str>(context, names)? else {
        return Ok(());
    };
    let Some(list) = handles::dense::<List>(context, list)? else {
        return Ok(());
    };
    for index in 0..list.len() {
        let name = names.get(context, index);
        if name.bytes().is_empty() && !handles::is_na(name) {
            match empty_name {
                EmptyName::Skip => continue,
                EmptyName::Reject => return Err(error("attempt to use zero-length variable name")),
            }
        }
        let symbol = handles::install_element(context, names, index)?;
        let value = context.root(list.get(context, index));
        handles::define_binding(context, environment, symbol, value)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EmptyName {
    Skip,
    Reject,
}

fn do_list2env<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let list = arguments.value(0);
    if list.r_type() != SexpType::List && list.r_type() != SexpType::Nil {
        return Err(error("first argument must be a named list"));
    }
    if handles::length(context, list) > 0
        && handles::names_attribute(context, list).r_type() == SexpType::Nil
    {
        return Err(error(
            "names(x) must be a character vector of the same length as x",
        ));
    }
    let Some(environment) = arguments.value(1).downcast::<Environment>() else {
        return Err(error("'envir' argument must be an environment"));
    };
    fill_environment(context, environment, list, EmptyName::Reject)?;
    Ok(environment.value())
}

fn do_lock_environment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = environment_arg(context, arguments.value(0), "env")?;
    if handles::logical_scalar(context, arguments.value(1)) == 1 {
        for symbol in handles::bound_symbols(context, environment, false) {
            if let Some(binding) = handles::frame_binding(context, environment, symbol) {
                handles::set_binding_locked(binding, true);
            }
        }
    }
    if !environment.is(handles::base_environment()) && !environment.is(handles::base_namespace()) {
        handles::lock_frame(environment);
    }
    Ok(handles::null())
}

fn do_environment_is_locked<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = binding_environment(context, arguments.value(0))?;
    let locked = environment.read(context).has_flag(gp::FRAME_LOCKED);
    handles::logical_constant(context, locked as i32)
}

/// The binding the `symbol` and `env` arguments of the binding functions name.
fn binding_cell<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s, Binding>> {
    let Some(symbol) = arguments.value(0).downcast::<Symbol>() else {
        return Err(error("not a symbol"));
    };
    let environment = binding_environment(context, arguments.value(1))?;
    match handles::frame_binding(context, environment, symbol) {
        Some(binding) => Ok(context.root(binding)),
        None => Err(error(format!(
            "no binding for \"{}\"",
            symbol_text(context, symbol)
        ))),
    }
}

/// The environment argument of the binding and locking functions, which unlike most take no
/// number for a frame: only an environment, or an S4 object that holds one, as GNU R's
/// `simple_as_environment` allows.
fn binding_environment<'s>(context: &Cx<'s>, value: Local<'s>) -> R<Local<'s, Environment>> {
    if value.r_type() == SexpType::Nil {
        return Err(error("use of NULL environment is defunct"));
    }
    simple_as_environment(context, value).ok_or_else(|| error("not an environment"))
}

/// GNU R's `simple_as_environment`: an environment, or the one an S4 object extends.
fn simple_as_environment<'s>(context: &Cx<'s>, value: Local<'s>) -> Option<Local<'s, Environment>> {
    if let Some(environment) = value.downcast::<Environment>() {
        return Some(environment);
    }
    if value.r_type() != SexpType::Object || !handles::is_s4(value) {
        return None;
    }
    let data = handles::get_attribute(context, value, handles::symbol(|symbols| symbols.dot_xdata));
    context.root(data).downcast::<Environment>()
}

fn do_lock_binding<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let binding = binding_cell(context, arguments)?;
    handles::set_binding_locked(binding, true);
    Ok(handles::null())
}

fn do_unlock_binding<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let binding = binding_cell(context, arguments)?;
    handles::set_binding_locked(binding, false);
    Ok(handles::null())
}

fn do_binding_is_locked<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let binding = binding_cell(context, arguments)?;
    let locked = binding.read(context).is_locked();
    handles::logical_constant(context, locked as i32)
}

fn do_make_active_binding<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let symbol = name_symbol(context, arguments.value(0), "not a symbol", false)?;
    let function = arguments.value(1);
    let environment = environment_arg(context, arguments.value(2), "env")?;
    if !values::is_function_type(function.r_type()) {
        return Err(arguments.error("'fun' must be a function"));
    }
    if let Some(binding) = handles::frame_binding(context, environment, symbol) {
        if !binding.is_active() {
            return Err(arguments.error("symbol already has a regular binding"));
        }
        let binding = context.root(binding);
        handles::assign_binding(context, binding, function)?;
    } else {
        handles::define_binding(context, environment, symbol, function)?;
        if let Some(binding) = handles::frame_binding(context, environment, symbol) {
            handles::make_binding_active(binding);
        }
    }
    Ok(handles::null())
}

fn do_binding_is_active<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let binding = binding_cell(context, arguments)?;
    let active = binding.read(context).is_active();
    handles::logical_constant(context, active as i32)
}

fn do_active_binding_function<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let binding = binding_cell(context, arguments)?;
    if !binding.read(context).is_active() {
        return Err(arguments.error("no active binding"));
    }
    Ok(context.root(binding.read(context).content()))
}

/// The search path for raw callers, from the global environment to base.
pub unsafe fn search_path() -> impl Iterator<Item = Sexp> {
    handles::reading(|context| {
        search_environments(context)
            .map(Gc::raw)
            .collect::<Vec<_>>()
    })
    .into_iter()
}

/// The search path, from the global environment to base.
pub fn search_environments<'a>(context: &'a Cx<'_>) -> impl Iterator<Item = Gc<'a, Environment>> {
    let base = handles::base_environment();
    let mut next = Some(handles::global_environment().read(context));
    std::iter::from_fn(move || {
        let environment = next?;
        next = environment.enclosure().filter(|enclosure| {
            !environment.is(base) && !enclosure.is(handles::empty_environment())
        });
        Some(environment)
    })
}

/// The name `search()` shows for `env`, an environment on the search path, for raw callers.
pub(crate) unsafe fn search_name(env: Sexp) -> String {
    handles::with(|context| {
        let env = handles::held(context, env);
        Ok(search_label(context, env))
    })
    .unwrap_or_default()
}

/// The name `search()` shows for `environment`, an environment on the search path.
fn search_label(context: &mut Cx<'_>, environment: Local<'_>) -> String {
    if environment.is(handles::global_environment()) {
        return ".GlobalEnv".to_owned();
    }
    if environment.is(handles::base_environment()) {
        return "package:base".to_owned();
    }
    let name = handles::get_attribute(
        context,
        environment,
        handles::symbol(|symbols| symbols.name),
    );
    let name = context.root(name);
    handles::first_string(context, name)
        .map(Cow::into_owned)
        .unwrap_or_default()
}

/// The search path's environments, each rooted.
fn search_list<'s>(context: &Cx<'s>) -> Vec<Local<'s, Environment>> {
    search_environments(context)
        .map(|environment| context.root(environment))
        .collect()
}

fn do_search<'s>(context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let names: Vec<String> = search_list(context)
        .into_iter()
        .map(|environment| search_label(context, environment.value()))
        .collect();
    Ok(text_vector(context, &names)?.value())
}

/// `as.environment`: a number is a search-path position, a string a search-path name, a list
/// a new environment.
pub fn environment_from<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Environment>> {
    if let Some(environment) = value.downcast::<Environment>() {
        return Ok(environment);
    }
    match value.r_type() {
        SexpType::Integer | SexpType::Real => {
            let position = handles::integer_scalar(context, value);
            if position == -1 {
                // The calling environment.
                let count = interp().frames.len();
                if count == 0 {
                    return Ok(handles::global_environment());
                }
                let environment = context.root_answer(|context| {
                    eval::caller_environment(context, count - 1, Reify::Site(Site::CallingPosition))
                })?;
                return environment
                    .downcast::<Environment>()
                    .ok_or_else(|| error("internal error: a frame without an environment"));
            }
            if position < 1 {
                return Err(error("invalid 'pos' argument"));
            }
            let found = search_environments(context).nth(position as usize - 1);
            found
                .map(|environment| context.root(environment))
                .ok_or_else(|| error("invalid 'pos' argument"))
        }
        SexpType::String => {
            let name = required_text(context, value, "pos")?;
            if name == ".GlobalEnv" {
                return Ok(handles::global_environment());
            }
            if name == "package:base" {
                return Ok(handles::base_environment());
            }
            let name = name.into_owned();
            for environment in search_list(context) {
                if search_label(context, environment.value()) == name {
                    return Ok(environment);
                }
            }
            Err(error(format!(
                "no item called \"{name}\" on the search list"
            )))
        }
        SexpType::List => list_to_environment(context, value, Some(handles::empty_environment())),
        SexpType::Object if handles::is_s4(value) => {
            let data = handles::get_attribute(
                context,
                value,
                handles::symbol(|symbols| symbols.dot_xdata),
            );
            context
                .root(data)
                .downcast::<Environment>()
                .ok_or_else(|| error("S4 object does not extend class \"environment\""))
        }
        _ => Err(error("invalid object for 'as.environment'")),
    }
}

fn do_as_environment<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let value = arguments.value(0);
    if value.r_type() == SexpType::Nil {
        return Err(arguments.error("using 'as.environment(NULL)' is defunct"));
    }
    Ok(environment_from(context, value)?.value())
}

fn do_pos_to_env<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(environment_from(context, arguments.value(0))?.value())
}

fn do_attach<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let what = arguments.value(0);
    let position = handles::integer_scalar(context, arguments.value(1)).max(2) as usize;
    let name = arguments.value(2);
    // An attached environment is a copy of `what`'s bindings, as in GNU R, so attaching a
    // namespace leaves its own parent, the imports, in place.
    let environment = handles::new_environment(context, handles::empty_environment())?;
    if what.r_type() == SexpType::List {
        fill_environment(context, environment, what, EmptyName::Skip)?;
    } else if let Some(source) = what.downcast::<Environment>() {
        for symbol in environment_symbols(context, source.read(context).value(), true, false) {
            let Some(binding) = handles::frame_binding(context, source, symbol) else {
                continue;
            };
            let active = binding.is_active();
            let content = context.root(binding.content());
            let symbol = context.root(symbol);
            handles::define_binding(context, environment, symbol, content)?;
            // An active binding stays active, so reading the copy still calls its function.
            if active && let Some(copy) = handles::frame_binding(context, environment, symbol) {
                handles::make_binding_active(copy);
            }
        }
    }
    handles::set_environment_attribute(
        context,
        environment,
        handles::symbol(|symbols| symbols.name),
        name,
    )?;
    let mut previous = handles::global_environment().read(context);
    for _ in 2..position {
        match previous.enclosure() {
            Some(next) if !next.is(handles::base_environment()) => previous = next,
            _ => break,
        }
    }
    let previous = context.root(previous);
    if let Some(enclosure) = previous.read(context).enclosure() {
        let enclosure = context.root(enclosure);
        handles::set_enclosure(context, environment, enclosure);
    }
    handles::set_enclosure(context, previous, environment);
    interp().globals.keep.push(environment.raw());
    Ok(environment.value())
}

fn do_detach<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let position = handles::integer_scalar(context, arguments.value(0));
    if position < 2 {
        return Err(arguments.error("invalid 'pos' argument"));
    }
    let neighbors = {
        let mut path = search_environments(context).skip(position as usize - 2);
        (path.next(), path.next())
    };
    let (Some(previous), Some(environment)) = neighbors else {
        return Err(arguments.error("invalid 'pos' argument"));
    };
    if environment.is(handles::base_environment()) {
        return Err(arguments.error("detaching \"package:base\" is not allowed"));
    }
    let (previous, environment) = (context.root(previous), context.root(environment));
    let enclosure = environment.read(context).enclosure();
    if let Some(enclosure) = enclosure.map(|enclosure| context.root(enclosure)) {
        handles::set_enclosure(context, previous, enclosure);
    }
    interp()
        .globals
        .keep
        .retain(|&kept| kept != environment.raw());
    Ok(environment.value())
}

// Options.

/// The options as a tagged pairlist, for raw callers.
pub unsafe fn options() -> Sexp {
    handles::reading(|context| option_list(context).raw())
}

/// The options as a tagged pairlist. GNU R keeps them as the base environment's `.Options`
/// binding, which R code reads directly.
pub fn option_list<'a>(context: &'a Cx<'_>) -> Gc<'a> {
    match handles::frame_binding(
        context,
        handles::base_environment(),
        handles::symbol(|symbols| symbols.dot_options),
    ) {
        Some(binding) => binding.content(),
        None => handles::null().read(context),
    }
}

/// The cells of the options list, in order.
fn option_cells<'a>(context: &'a Cx<'_>) -> impl Iterator<Item = Gc<'a, Pairlist>> {
    let mut next = option_list(context).downcast::<Pairlist>();
    std::iter::from_fn(move || {
        let cell = next?;
        next = cell.rest().downcast::<Pairlist>();
        Some(cell)
    })
}

/// The name of the option a cell of the options list holds.
fn option_name(cell: Gc<'_, Pairlist>) -> Option<Gc<'_, Chars>> {
    Some(cell.tag().downcast::<Symbol>()?.name())
}

/// Removes every option, for a start that sets them again from the C-level defaults.
pub(crate) unsafe fn clear_options() -> R<()> {
    interp().product_options = crate::linalg::ProductOptions::default();
    handles::with(|context| set_options(context, handles::null()))
}

fn set_options(context: &mut Cx<'_>, list: Local<'_>) -> R<()> {
    handles::define_base_variable(
        context,
        handles::symbol(|symbols| symbols.dot_options),
        list,
    )
}

/// The value of option `name`, or `NULL`, for raw callers.
pub unsafe fn get_option(name: &str) -> Sexp {
    handles::reading(|context| option_named(context, name).raw())
}

/// The value of option `name`, or `NULL`.
pub fn option_named<'a>(context: &'a Cx<'_>, name: &str) -> Gc<'a> {
    option_cells(context)
        .find(|&cell| option_name(cell).is_some_and(|option| option.bytes() == name.as_bytes()))
        .map_or_else(|| handles::null().read(context), |cell| cell.head())
}

/// The option the string `name` (a `CHARSXP`) names, or `NULL`, for raw callers.
pub unsafe fn option_value(name: Sexp) -> Sexp {
    handles::reading(|context| option_of(context, Gc::adopt(name)).raw())
}

/// The option the string `name` names, or `NULL`. An ASCII name is the same interned object as
/// the printname of the symbol tagging its option, so it compares by identity, without reading
/// the bytes of every option it passes.
pub fn option_of<'a>(context: &'a Cx<'_>, name: Gc<'_, Chars>) -> Gc<'a> {
    let ascii = name.has_flag(gp::ASCII);
    option_cells(context)
        .find(|&cell| {
            option_name(cell).is_some_and(|printname| {
                printname.raw() == name.raw() || (!ascii && printname.bytes() == name.bytes())
            })
        })
        .map_or_else(|| handles::null().read(context), |cell| cell.head())
}

/// Sets the option `symbol` names, removing it for `NULL`, and returns the old value.
fn set_option<'s>(
    context: &mut Cx<'s>,
    symbol: Gc<'static, Symbol>,
    value: Local<'s>,
) -> R<Local<'s>> {
    match symbol.name().bytes() {
        b"expressions" => {
            let depth = handles::integer_scalar(context, value);
            if depth > 0 {
                interp().max_depth = depth.min(500_000) as u32;
            }
        }
        name @ (b"matprod" | b"ry.blas") => {
            let choice = handles::first_string(context, value).map(Cow::into_owned);
            crate::linalg::set_product_option(
                &mut interp().product_options,
                name,
                choice.as_deref(),
            );
        }
        _ => {}
    }
    let mut previous = None;
    let mut found = None;
    for cell in option_cells(context) {
        if cell.tag().is(symbol) {
            found = Some(cell);
            break;
        }
        previous = Some(cell);
    }
    let previous = previous.map(|cell| context.root(cell));
    let found = found.map(|cell| context.root(cell));
    if let Some(cell) = found {
        // Once replaced, the old value is reachable only from this root.
        let old = context.root(cell.read(context).head());
        if value.r_type() == SexpType::Nil {
            let rest = context.root(cell.read(context).rest());
            match previous {
                Some(previous) => handles::set_rest(context, previous, rest),
                None => set_options(context, rest)?,
            }
        } else {
            handles::set_head(context, cell, value);
        }
        return Ok(old);
    }
    if value.r_type() != SexpType::Nil {
        let tag = context.root(symbol).value();
        let fresh = handles::cons(context, tag, value, handles::null())?;
        match previous {
            Some(previous) => handles::set_rest(context, previous, fresh),
            None => set_options(context, fresh)?,
        }
    }
    Ok(handles::null())
}

/// `options()`: every option, sorted by name.
fn options_list<'s>(context: &mut Cx<'s>) -> R<Local<'s>> {
    let entries: Vec<(String, Local<'s>)> = option_cells(context)
        .map(|cell| {
            let name = option_name(cell).map(|name| handles::text(name).into_owned());
            (name.unwrap_or_default(), context.root(cell.head()))
        })
        .collect();
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    let order = collation_order(&names);
    let mut list = context.alloc::<List>(entries.len())?;
    let mut labels = StringFill::new(context, entries.len())?;
    for (position, &index) in order.iter().enumerate() {
        let (name, value) = &entries[index as usize];
        list.set(context, position, *value);
        labels.push_text(context, name)?;
    }
    let mut list = list.owned();
    let labels = labels.finish().finish().value();
    handles::set_attribute(
        context,
        &mut list,
        handles::symbol(|symbols| symbols.names),
        labels,
    )?;
    Ok(list.finish().value())
}

/// The raw entry of `options()`, for a native caller that sets options as R code would.
pub(crate) const OPTIONS: BuiltinFn = safe!(do_options);

fn do_options<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    if arguments.is_empty() {
        interp().visible = true;
        return options_list(context);
    }
    // A list without a name stands for its elements, each named by its name.
    let mut items: SmallVec<[Tagged<'s>; 4]> = SmallVec::new();
    for index in 0..arguments.len() {
        let (tag, value) = (arguments.tag(index), arguments.value(index));
        let list = match tag.r_type() {
            SexpType::Nil => handles::dense::<List>(context, value)?,
            _ => None,
        };
        let Some(list) = list else {
            items.push(Tagged { tag, value });
            continue;
        };
        let names = handles::names_attribute(context, value);
        let names = handles::dense::<Str>(context, names)?;
        for element in 0..list.len() {
            let tag = match names {
                Some(names) if !names.get(context, element).bytes().is_empty() => {
                    let symbol = handles::install_name_at(context, names, element)?;
                    context.root(symbol).value()
                }
                _ => handles::null(),
            };
            let value = context.root(list.get(context, element));
            items.push(Tagged { tag, value });
        }
    }
    let mut changed = false;
    // Each entry is an option's name and its old or current value.
    let mut entries: SmallVec<[(Local<'s, Chars>, Local<'s>); 4]> = SmallVec::new();
    for item in &items {
        if let Some(symbol) = item.tag.downcast::<Symbol>() {
            let symbol = handles::permanent(symbol);
            let value = validate_option(context, &handles::text(symbol.name()), item.value)?;
            let old = set_option(context, symbol, value)?;
            entries.push((context.root(symbol.name()), old));
            changed = true;
        } else if let Some(names) = handles::dense::<Str>(context, item.value)? {
            // GNU R reads only the first name of a character argument: `options(c("a", "b"))`
            // answers `a` alone.
            if names.is_empty() {
                return Err(arguments.error("invalid argument"));
            }
            let name = context.root(names.get(context, 0));
            // A later argument may remove the option, which leaves the value reachable only
            // from this root.
            let value = context.root(option_named(context, &handles::text(name.read(context))));
            entries.push((name, value));
        } else if item.value.r_type() != SexpType::Nil {
            return Err(arguments.error("invalid argument"));
        }
    }
    interp().visible = !changed;
    let mut list = context.alloc::<List>(entries.len())?;
    let mut names = context.alloc::<Str>(entries.len())?;
    for (index, &(name, value)) in entries.iter().enumerate() {
        list.set(context, index, value);
        names.set(context, index, name);
    }
    let mut list = list.owned();
    let names = names.finish().value();
    handles::set_attribute(
        context,
        &mut list,
        handles::symbol(|symbols| symbols.names),
        names,
    )?;
    Ok(list.finish().value())
}

/// Whether `options()` sets the option `name` to `value` without an error, a warning or any
/// other effect than storing the value, so that a native caller can set it through
/// [`OPTIONS`] knowing that no condition will name the call.
pub(crate) unsafe fn option_sets_quietly(name: &str, value: Sexp) -> bool {
    // These warn about some values, or change output formatting as they are validated.
    if matches!(name, "max.print" | "nwarnings" | "OutDec" | "scipen") {
        return false;
    }
    // An invalid value is an error whose call is the caller's; it is only inspected here.
    handles::with(|context| {
        let value = handles::held(context, value);
        validate_option(context, name, value).map(|_| ())
    })
    .is_ok()
}

fn validate_option<'s>(context: &mut Cx<'s>, name: &str, value: Local<'s>) -> R<Local<'s>> {
    match name {
        "digits" => {
            let digits = handles::integer_scalar(context, value);
            if !(1..=22).contains(&digits) {
                return Err(error("invalid 'digits' parameter, allowed 1...22"));
            }
            handles::integer(context, digits)
        }
        "width" => {
            let width = handles::integer_scalar(context, value);
            if !(10..=10000).contains(&width) {
                return Err(error("invalid 'width' parameter, allowed 10...10000"));
            }
            handles::integer(context, width)
        }
        "expressions" => {
            let depth = handles::integer_scalar(context, value);
            if !(25..=500_000).contains(&depth) {
                return Err(error(
                    "'expressions' parameter invalid, allowed 25...500000",
                ));
            }
            handles::integer(context, depth)
        }
        "warning.length" => {
            let length = handles::integer_scalar(context, value);
            if !(100..=8170).contains(&length) {
                return Err(error("invalid value for 'warning.length'"));
            }
            handles::integer(context, length)
        }
        "max.print" | "nwarnings" => {
            if value.r_type() == SexpType::Real
                && handles::length(context, value) > 0
                && handles::real_scalar(context, value).abs() > f64::from(i32::MAX)
            {
                conditions::warn(context, "NAs introduced by coercion to integer range")?;
            }
            let count = handles::integer_scalar(context, value);
            if count < 1 {
                return Err(error(format!("invalid value for '{name}'")));
            }
            handles::integer(context, count)
        }
        "matprod" => {
            let mode = handles::first_string(context, value).unwrap_or_default();
            if !matches!(&*mode, "default" | "internal" | "blas" | "default.simd") {
                return Err(error("invalid value for 'matprod'"));
            }
            Ok(value)
        }
        "ry.blas" => {
            let choice = handles::first_string(context, value).unwrap_or_default();
            if !matches!(&*choice, "reference" | "host") {
                return Err(error("invalid value for 'ry.blas'"));
            }
            Ok(value)
        }
        "warn" => {
            let number = if handles::length(context, value) == 1
                && matches!(
                    value.r_type(),
                    SexpType::Logical | SexpType::Integer | SexpType::Real
                ) {
                handles::real_scalar(context, value)
            } else {
                f64::NAN
            };
            if number.is_nan() || number < f64::from(i32::MIN) || number > f64::from(i32::MAX) {
                return Err(error("invalid value for 'warn'"));
            }
            handles::integer(context, number as i32)
        }
        "OutDec" => {
            let Some(mark) = handles::first_string(context, value).map(Cow::into_owned) else {
                return Err(error("invalid value for 'OutDec'"));
            };
            if mark.chars().count() != 1 {
                conditions::warn(context, "'OutDec' must be a string of one character")?;
            }
            crate::format::set_output_decimal(&mark);
            Ok(value)
        }
        // R 4.5 stores `scipen` as an integer within -9..9999, warning about a value it moves
        // there; earlier releases keep it as given and read it as an integer where it is used.
        "scipen" if crate::system::r_at_least(4, 5) => {
            let numeric = match value.r_type() {
                SexpType::Logical | SexpType::Real => true,
                SexpType::Integer => !handles::inherits(context, value, "factor"),
                _ => false,
            };
            if !numeric || handles::length(context, value) != 1 {
                return Err(error("invalid 'scipen'"));
            }
            let digits = if value.r_type() == SexpType::Real {
                let number = handles::real_scalar(context, value);
                if number.is_nan() {
                    NA_INTEGER
                } else if number >= f64::from(i32::MAX) + 1.0 || number <= f64::from(i32::MIN) {
                    return Err(error(format!(
                        "setting scipen={} is out of range",
                        crate::format::format_g(number)
                    )));
                } else {
                    number as i32
                }
            } else {
                handles::integer_scalar(context, value)
            };
            const LOWEST: i32 = -9;
            const HIGHEST: i32 = 9999;
            if digits == NA_INTEGER || !(LOWEST..=HIGHEST).contains(&digits) {
                let used = match digits {
                    NA_INTEGER => 0,
                    _ => digits.clamp(LOWEST, HIGHEST),
                };
                conditions::warn(context, format!("invalid 'scipen' {digits}, used {used}"))?;
                return handles::integer(context, used);
            }
            handles::integer(context, digits)
        }
        "deparse.cutoff" => {
            if matches!(value.r_type(), SexpType::Real | SexpType::Integer) {
                let cutoff = handles::integer_scalar(context, value);
                handles::integer(context, cutoff)
            } else {
                Ok(value)
            }
        }
        // GNU R stores a function given as the error handler as a call to it.
        "error"
            if matches!(
                value.r_type(),
                SexpType::Closure | SexpType::Builtin | SexpType::Special
            ) =>
        {
            Ok(handles::language(context, value, handles::null())?.value())
        }
        _ => Ok(value),
    }
}

fn do_get_option<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let name = match handles::dense::<Str>(context, arguments.value(0))? {
        Some(name) if name.len() == 1 => name,
        _ => return Err(error("'x' must be a character string")),
    };
    let name = name.get(context, 0);
    if name.bytes().is_empty() {
        return Err(error("attempt to use zero-length variable name"));
    }
    Ok(context.root(option_of(context, name)))
}

/// Sets the options GNU R's C code sets at startup, before any R code runs.
pub unsafe fn install_default_options() -> R<()> {
    handles::with(default_options)
}

fn default_options(context: &mut Cx<'_>) -> R<()> {
    enum Default {
        Text(&'static str),
        Integer(i32),
        Logical(bool),
        Missing,
    }
    let keep_package_source = std::env::var("R_KEEP_PKG_SOURCE").is_ok_and(|value| value == "yes");
    let defaults = [
        ("prompt", Default::Text("> ")),
        ("continue", Default::Text("+ ")),
        ("expressions", Default::Integer(5000)),
        ("width", Default::Integer(80)),
        ("deparse.cutoff", Default::Integer(60)),
        ("digits", Default::Integer(7)),
        ("echo", Default::Logical(false)),
        ("verbose", Default::Logical(false)),
        ("check.bounds", Default::Logical(false)),
        ("keep.source", Default::Logical(false)),
        ("keep.source.pkgs", Default::Logical(keep_package_source)),
        ("keep.parse.data", Default::Logical(true)),
        ("keep.parse.data.pkgs", Default::Logical(false)),
        ("warning.length", Default::Integer(1000)),
        ("nwarnings", Default::Integer(50)),
        ("OutDec", Default::Text(".")),
        ("browserNLdisabled", Default::Logical(false)),
        ("CBoundsCheck", Default::Logical(false)),
        ("matprod", Default::Text("default")),
        ("PCRE_study", Default::Integer(10)),
        ("PCRE_use_JIT", Default::Logical(true)),
        ("PCRE_limit_recursion", Default::Missing),
        ("rl_word_breaks", Default::Text(" \t\n\"\\'`><=%;,|&{()}")),
        ("warn", Default::Integer(0)),
        ("timeout", Default::Integer(60)),
        ("encoding", Default::Text("native.enc")),
        ("show.error.messages", Default::Logical(true)),
        ("scipen", Default::Integer(0)),
        ("max.print", Default::Integer(99999)),
        ("add.smooth", Default::Logical(true)),
        ("stringsAsFactors", Default::Logical(false)),
        ("useFancyQuotes", Default::Logical(true)),
        ("catch.script.errors", Default::Logical(false)),
    ];
    for (name, default) in defaults {
        let value = match default {
            Default::Text(text) => text_vector(context, &[text])?.value(),
            Default::Integer(number) => handles::integer(context, number)?,
            Default::Logical(flag) => handles::logical_constant(context, flag as i32)?,
            // A fresh vector, where GNU R's C code allocates one.
            Default::Missing => context.scalar::<Logical>(NA_INTEGER)?.finish().value(),
        };
        let symbol = handles::install_bytes(context, name.as_bytes())?;
        set_option(context, symbol, value)?;
    }
    Ok(())
}

// Namespaces.

fn registry_symbol<'s>(context: &mut Cx<'s>, name: Local<'s>) -> R<Local<'s, Symbol>> {
    if let Some(symbol) = name.downcast::<Symbol>() {
        return Ok(symbol);
    }
    match handles::dense::<Str>(context, name)? {
        Some(names) if !names.is_empty() => handles::install_element(context, names, 0),
        _ => Err(error("bad namespace name")),
    }
}

fn do_get_registered_namespace<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let symbol = registry_symbol(context, arguments.value(0))?;
    let value = eval::frame_content(context, handles::namespace_registry(), symbol)?;
    Ok(value.unwrap_or_else(handles::null))
}

fn do_is_registered_namespace<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let symbol = registry_symbol(context, arguments.value(0))?;
    let registered = handles::frame_binding(context, handles::namespace_registry(), symbol);
    handles::logical_constant(context, registered.is_some() as i32)
}

fn do_register_namespace<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let symbol = registry_symbol(context, arguments.value(0))?;
    let registry = handles::namespace_registry();
    if handles::frame_binding(context, registry, symbol).is_some() {
        return Err(arguments.error("namespace already registered"));
    }
    eval::define_variable(context, symbol, arguments.value(1), registry)?;
    Ok(handles::null())
}

fn do_unregister_namespace<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let symbol = registry_symbol(context, arguments.value(0))?;
    if !handles::remove_binding(context, handles::namespace_registry(), symbol)? {
        return Err(arguments.error("namespace not registered"));
    }
    Ok(handles::null())
}

fn do_import_into_env<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let target = environment_arg(context, arguments.value(0), "impenv")?;
    let target_names = handles::character_vector(context, arguments.value(1))?;
    let source = environment_arg(context, arguments.value(2), "expenv")?;
    let source_names = handles::character_vector(context, arguments.value(3))?;
    if target_names.len() != source_names.len() {
        return Err(arguments.error("length of import and export names must match"));
    }
    for index in 0..target_names.len() {
        let target_symbol = handles::install_element(context, target_names, index)?;
        let source_symbol = handles::install_element(context, source_names, index)?;
        let mut environment = Some(source.read(context));
        let mut found = None;
        while let Some(scope) = environment {
            if let Some(binding) = handles::frame_binding(context, scope, source_symbol) {
                found = Some(binding);
                break;
            }
            environment = scope.enclosure();
        }
        let Some(binding) = found else {
            return Err(arguments.error(format!(
                "exported symbol '{}' has no value",
                symbol_text(context, source_symbol)
            )));
        };
        let active = binding.is_active();
        let content = context.root(binding.content());
        if active {
            handles::define_binding(context, target, target_symbol, content)?;
            if let Some(binding) = handles::frame_binding(context, target, target_symbol) {
                handles::make_binding_active(binding);
            }
        } else {
            eval::define_variable(context, target_symbol, content, target)?;
        }
    }
    Ok(handles::null())
}

/// Whether `value` is the logical `TRUE` of length one, as `isTRUE` asks.
fn is_true(context: &Cx<'_>, value: Local<'_>) -> bool {
    value
        .standard::<Logical>()
        .is_some_and(|value| matches!(value.as_slice(context), [1]))
}

/// `function` called with `values` as its unnamed arguments, from `environment`.
fn call_with<'s>(
    context: &mut Cx<'s>,
    function: Local<'s>,
    values: &[Local<'s>],
    environment: Local<'s, Environment>,
) -> R<Local<'s>> {
    let arguments: SmallVec<[Tagged<'s>; 2]> = values
        .iter()
        .map(|&value| Tagged {
            tag: handles::null(),
            value,
        })
        .collect();
    handles::apply(context, handles::null(), function, &arguments, environment)
}

/// `ryLocalS4Generics(names, env, is_generic)`: for each name bound in the namespace `env`,
/// whether its value is an S4 generic, which the `registerS3methods()` the runtime installs
/// (`register_s3_methods.R`) asks of every S3 generic a package defines. `is_generic` is
/// `methods::is(f, "genericFunction")`, called only for a value that is an object at all, since
/// `is()` answers `FALSE` for every other one.
fn do_local_s4_generics<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    /// The serialization type code of a closure (`CLOSXP`).
    const CLOSURE_RECORD: u32 = 3;
    let names = handles::character_vector(context, arguments.value(0))?;
    let environment = environment_arg(context, arguments.value(1), "env")?;
    let is_generic = arguments.value(2);
    let caller = arguments.environment(context)?;
    let mut generic = Vec::with_capacity(names.len());
    // A generic appears once per method registered for it, and its answer is the same each time.
    let mut answered = rustc_hash::FxHashMap::<Sexp, i32>::default();
    for index in 0..names.len() {
        let symbol = handles::install_element(context, names, index)?;
        if let Some(&answer) = answered.get(&symbol.raw()) {
            generic.push(answer);
            continue;
        }
        let Some(binding) = eval::variable(context, symbol, environment)? else {
            return Err(arguments.error(format!(
                "object '{}' not found",
                symbol_text(context, symbol)
            )));
        };
        // A generic still in its lazy-load database whose record is a closure without a class
        // or the S4 bit cannot be an S4 generic, so registering methods for it leaves it
        // unfetched until something calls it.
        if let Some((kind, is_object, levels)) = handles::lazy_record_header(context, binding)?
            && kind == CLOSURE_RECORD
            && !is_object
            && levels & gp::S4_OBJECT == 0
        {
            answered.insert(symbol.raw(), 0);
            generic.push(0);
            continue;
        }
        let value = eval::forced_value(context, binding)?;
        let is_s4_generic = (handles::is_object(value) || handles::is_s4(value)) && {
            let answer = call_with(context, is_generic, &[value], caller)?;
            is_true(context, answer)
        };
        answered.insert(symbol.raw(), i32::from(is_s4_generic));
        generic.push(i32::from(is_s4_generic));
    }
    let mut result = context.alloc::<Logical>(generic.len())?;
    result.as_mut_slice().copy_from_slice(&generic);
    Ok(result.finish().value())
}

/// What every row of `ryRegisterS3Methods()` reads: its arguments, the closures of the steps
/// that stay in R and the symbols it binds.
#[derive(Clone, Copy)]
struct Registration<'s> {
    fin: Local<'s, Str>,
    rows: usize,
    environment: Local<'s, Environment>,
    frame: Local<'s, Environment>,
    method_code: Local<'s>,
    caller: Local<'s, Environment>,
    namespace: Local<'s>,
    missing: Local<'s>,
    generic: Local<'s>,
    default: Local<'s>,
    new_table: Local<'s>,
    existing: Local<'s>,
    home: Local<'s>,
    known: Option<Local<'s, Str>>,
}

impl<'s> Registration<'s> {
    fn pass<'inner>(self, outer: heap::handle::Enclosing<'s, 'inner>) -> Registration<'inner> {
        Registration {
            fin: outer.pass(self.fin),
            rows: self.rows,
            environment: outer.pass(self.environment),
            frame: outer.pass(self.frame),
            method_code: outer.pass(self.method_code),
            caller: outer.pass(self.caller),
            namespace: outer.pass(self.namespace),
            missing: outer.pass(self.missing),
            generic: outer.pass(self.generic),
            default: outer.pass(self.default),
            new_table: outer.pass(self.new_table),
            existing: outer.pass(self.existing),
            home: outer.pass(self.home),
            known: self.known.map(|known| outer.pass(known)),
        }
    }
}

/// `ryRegisterS3Methods(fin, env, frame, method_code, helpers)`: the loop of GNU R's
/// `registerS3methods()` that registers each S3 method a namespace declares for a generic it
/// imports, run natively because it costs most of a package's load time in R. `fin` is that
/// function's five-column character matrix of generic, class, method, table name and delayed
/// package. Each method is bound in its generic's S3 methods table to a promise of
/// `method_code` (`get(method, envir = home)`) whose environment holds `x`, `method`, `home` and
/// `envir` and encloses `frame`, exactly the binding GNU R's `assignWrapped()` makes. The
/// `helpers` closures run the steps that stay in R: `namespace`, `missing`, `generic`,
/// `default`, `table`, `existing` and `home` (see `register_s3_methods.R`). The value is the
/// overwritten methods, as the rows of a two-column matrix flattened by row.
fn do_register_s3_methods<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let fin = handles::character_vector(context, arguments.value(0))?;
    let environment = environment_arg(context, arguments.value(1), "env")?;
    let frame = environment_arg(context, arguments.value(2), "frame")?;
    let helpers = arguments.value(4);
    let helper_names = handles::names_attribute(context, helpers);
    let helper_names = handles::dense::<Str>(context, helper_names)?;
    let helpers = handles::dense::<List>(context, helpers)?;
    let helper = |context: &Cx<'s>, name: &str| -> R<Local<'s>> {
        let (Some(helpers), Some(names)) = (helpers, helper_names) else {
            return Err(error(format!("no '{name}' helper")));
        };
        (0..helpers.len().min(names.len()))
            .find(|&index| {
                let label = names.get(context, index);
                !handles::is_na(label) && label.bytes() == name.as_bytes()
            })
            .map(|index| context.root(helpers.get(context, index)))
            .ok_or_else(|| error(format!("no '{name}' helper")))
    };
    let known_symbol = handles::install_bytes(context, b".knownS3Generics")?;
    let known = eval::frame_content(context, handles::base_namespace(), known_symbol)?;
    let known = match known {
        Some(known) => {
            let known = eval::forced_value(context, known)?;
            handles::dense::<Str>(context, known)?
        }
        None => None,
    };
    let registration = Registration {
        fin,
        rows: fin.len() / 5,
        environment,
        frame,
        method_code: arguments.value(3),
        caller: arguments.environment(context)?,
        namespace: helper(context, "namespace")?,
        missing: helper(context, "missing")?,
        generic: helper(context, "generic")?,
        default: helper(context, "default")?,
        new_table: helper(context, "table")?,
        existing: helper(context, "existing")?,
        home: helper(context, "home")?,
        known,
    };
    // Each generic's package, as the index of its entry; the first entry for a name wins.
    let mut known_generics = rustc_hash::FxHashMap::<String, usize>::default();
    if let Some(known) = known {
        let names = handles::names_attribute(context, known.value());
        if let Some(names) = handles::dense::<Str>(context, names)? {
            for index in (0..names.len().min(known.len())).rev() {
                let name = names.get(context, index);
                if !handles::is_na(name) {
                    known_generics.insert(handles::text(name).into_owned(), index);
                }
            }
        }
    }
    let mut overwritten = Vec::new();
    for row in 0..registration.rows {
        let replaced = context.scope_reading(|context, outer| {
            register_method(context, registration.pass(outer), &known_generics, row)
        })?;
        if let Some((name, home)) = replaced {
            overwritten.push(name);
            overwritten.push(home);
        }
    }
    Ok(text_vector(context, &overwritten)?.value())
}

/// Registers the method of row `row`, answering the name and the home of the method it
/// replaces, if it replaces a different one.
fn register_method<'s>(
    context: &mut Cx<'s>,
    registration: Registration<'s>,
    known_generics: &rustc_hash::FxHashMap<String, usize>,
    row: usize,
) -> R<Option<(String, String)>> {
    let Registration {
        fin,
        rows,
        environment,
        caller,
        ..
    } = registration;
    let column =
        |context: &Cx<'s>, index: usize| context.root(fin.get(context, row + index * rows));
    let generic_name = column(context, 0);
    let generic_text = handles::text(generic_name.read(context)).into_owned();
    let package = match (known_generics.get(&generic_text), registration.known) {
        (Some(&index), Some(known)) if !handles::is_na(known.get(context, index)) => {
            Some(context.root(known.get(context, index)))
        }
        _ => None,
    };
    let defining = match package {
        Some(package) => {
            let package_symbol = handles::install(context, package)?;
            let registry = handles::namespace_registry().value();
            match handles::frame_symbol_value(context, registry, package_symbol)? {
                Some(loaded) if loaded.r_type() == SexpType::Environment => loaded,
                _ => {
                    let text = handles::text(package.read(context)).into_owned();
                    let name = text_vector(context, &[text])?.value();
                    call_with(context, registration.namespace, &[name], caller)?
                }
            }
        }
        None => {
            let symbol = handles::install(context, generic_name)?;
            let enclosure = environment.read(context).enclosure();
            let enclosure = match enclosure {
                Some(enclosure) => context.root(enclosure),
                None => handles::empty_environment(),
            };
            let found = eval::variable(context, symbol, enclosure)?;
            let mut function = match found {
                Some(found) => eval::forced_value(context, found)?,
                None => handles::null(),
            };
            if function.r_type() == SexpType::Nil {
                let name = text_vector(context, &[generic_text.as_str()])?.value();
                call_with(context, registration.missing, &[name], caller)?;
            }
            if interp().globals.methods_dispatch
                && (handles::is_object(function) || handles::is_s4(function))
                && {
                    let answer = call_with(context, registration.generic, &[function], caller)?;
                    is_true(context, answer)
                }
            {
                function = call_with(context, registration.default, &[function], caller)?;
            }
            match function.downcast::<Closure>() {
                Some(closure) => context.root(closure.read(context).environment()).value(),
                None => handles::base_namespace().value(),
            }
        }
    };
    let table_symbol = handles::install_bytes(context, b".__S3MethodsTable__.")?;
    let table = match handles::frame_symbol_value(context, defining, table_symbol)? {
        Some(table) if table.r_type() != SexpType::Nil => table,
        _ => call_with(context, registration.new_table, &[defining], caller)?,
    };
    let Some(table_environment) = table.downcast::<Environment>() else {
        return Err(error("an S3 methods table is not an environment"));
    };
    let name = column(context, 3);
    let name_symbol = handles::install(context, name)?;
    let name_text = handles::text(name.read(context)).into_owned();
    // `None` when the table binds nothing, and whether an R helper must read the binding.
    let existing = handles::frame_binding(context, table_environment, name_symbol).map(|binding| {
        let content = binding.content();
        let pending = content
            .downcast::<heap::handle::Promise>()
            .is_some_and(|promise| handles::is_unbound(promise.result()));
        (binding.is_active() || pending, context.root(content))
    });
    let current = match existing {
        None => handles::null(),
        Some((true, _)) => {
            let label = text_vector(context, &[name_text.as_str()])?.value();
            call_with(context, registration.existing, &[table, label], caller)?
        }
        Some((false, content)) => eval::forced_value(context, content)?,
    };
    let method = column(context, 2);
    let method_text = handles::text(method.read(context)).into_owned();
    let mut replaced = None;
    if current.r_type() != SexpType::Nil {
        let method_symbol = handles::install(context, method)?;
        let own = match eval::variable(context, method_symbol, environment)? {
            Some(own) => eval::forced_value(context, own)?,
            None => handles::unbound(),
        };
        if !handles::identical(context, current, own) {
            let current_home = call_with(context, registration.home, &[current], caller)?;
            let home = handles::first_string(context, current_home).unwrap_or_default();
            replaced = Some((name_text.clone(), home.into_owned()));
        }
    }
    let promise_environment = handles::new_environment(context, registration.frame)?;
    let bindings = [
        ("x", text_vector(context, &[name_text.as_str()])?.value()),
        (
            "method",
            text_vector(context, &[method_text.as_str()])?.value(),
        ),
        ("home", environment.value()),
        ("envir", table),
    ];
    for (symbol, value) in bindings {
        let symbol = handles::install_bytes(context, symbol.as_bytes())?;
        let symbol = context.root(symbol);
        handles::define_binding(context, promise_environment, symbol, value)?;
    }
    let promise = handles::promise(context, registration.method_code, promise_environment)?;
    eval::define_variable(context, name_symbol, promise.value(), table_environment)?;
    Ok(replaced)
}

// Calling functions.

/// `do.call(what, args, envir)`.
fn do_do_call<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let what = arguments.value(0);
    let function = match what.r_type() {
        SexpType::String if handles::length(context, what) == 1 => {
            let what = handles::character_vector(context, what)?;
            handles::install_element(context, what, 0)?.value()
        }
        SexpType::Closure | SexpType::Builtin | SexpType::Special => what,
        _ => return Err(error("'what' must be a function or character string")),
    };
    let list = match arguments.value(1) {
        list if list.r_type() == SexpType::Nil => None,
        list if list.r_type() == SexpType::List => handles::dense::<List>(context, list)?,
        _ => return Err(error("'args' must be a list")),
    };
    let environment = arguments
        .value(2)
        .downcast::<Environment>()
        .ok_or_else(|| error("'envir' must be an environment"))?;
    let mut items: SmallVec<[Tagged<'s>; 8]> = SmallVec::new();
    if let Some(list) = list {
        let names_symbol = handles::symbol(|symbols| symbols.names);
        let names = context.root(handles::get_attribute(context, list, names_symbol));
        let names = handles::dense::<Str>(context, names)?;
        for index in 0..list.len() {
            let tag = match names {
                Some(names) => {
                    let (empty, na) = {
                        let name = names.get(context, index);
                        (name.bytes().is_empty(), handles::is_na(name))
                    };
                    // An NA name tags its argument `NA`, as GNU R's `install` of the NA string
                    // does.
                    let symbol = if empty {
                        None
                    } else if na {
                        Some(handles::install_bytes(context, b"NA")?)
                    } else {
                        Some(handles::install_name_at(context, names, index)?)
                    };
                    symbol.map_or_else(handles::null, |symbol| {
                        handles::root_unless_permanent(context, symbol.value())
                    })
                }
                None => handles::null(),
            };
            // A symbol or call among the arguments is evaluated in `envir`, as in GNU R; the R
            // code quotes them first when `quote = TRUE`.
            let value = handles::root_unless_permanent(context, list.get(context, index));
            items.push(Tagged { tag, value });
        }
    }
    let rest = handles::chain(context, &items, handles::null())?;
    let call = handles::language(context, function, rest)?;
    handles::evaluate(context, call.value(), environment)
}

/// `lapply(X, FUN)`: GNU R evaluates `FUN(X[[i]], ...)` in the closure's frame, so the call
/// each iteration reports is exactly that.
unsafe fn do_lapply(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let count = length(list);
    let result = alloc(SexpType::List, count)?;
    let mut calls = ElementCalls::new(call, list)?;
    // Each call's value is rooted in this scope until the result holds it, so the handle stack
    // stays as deep as one call however long `X` is.
    let mark = crate::heap().handle_mark();
    for index in 0..count {
        let value = calls.call(call, index)?;
        values::set_elt(result, index, value);
        crate::heap().release_handles(mark);
    }
    let names = values::names(list);
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// The calls `FUN(X[[i]], ...)` that `lapply` and `vapply` make in their closure's frame, one
/// per element of `X`.
struct ElementCalls {
    /// The frame's environment, which the calls are made from; nil while they are made from
    /// the frame's slots.
    env: Sexp,
    /// The slot frame the calls are made from while it has no environment, and the register
    /// that holds its `i`. A closure or builtin `FUN` over a plain `X` needs nothing of the
    /// frame but its `...`, so the frame gets an environment only if `FUN` asks for its
    /// caller's.
    slots: Option<(usize, usize)>,
    index_symbol: Sexp,
    /// The integer `i` is bound to, which GNU R updates in place for each element.
    index: Sexp,
    /// `index`'s reference count once bound. A higher count means `FUN` kept it, and a lower
    /// one that `FUN` rebound `i`, which may leave `index` unreachable; either way the next
    /// element binds a fresh integer.
    bound_refcount: u16,
    /// `X` itself when `X[[i]]` reads element `i` directly: a vector with no class, where `[[`
    /// cannot dispatch and base's `[[` is the one `lapply` finds, so the result is the same.
    plain: Option<Sexp>,
    fun_call: Sexp,
    known: Option<eval::KnownFirst>,
}

impl ElementCalls {
    unsafe fn new(call: &Call<'_>, list: Sexp) -> R<ElementCalls> {
        let index_symbol = symbols().i;
        let fun_call = element_call()?;
        let plain = if list.kind().is_vector()
            && !list.is_object()
            && !list.is_array()
            && !matches!(list.kind(), SexpType::I64 | SexpType::Record)
            // `vector_element` would materialize a complex or raw ALTREP vector, which `X[[i]]`
            // reads one element at a time. Every other kind reads through its `Elt` method.
            && (!list.is_altrep() || !matches!(list.kind(), SexpType::Complex | SexpType::Raw))
        {
            Some(list)
        } else {
            None
        };
        let slots = match call.slot_frame() {
            // The calls take the frame's `...` from its slot. A frame without the formal reads
            // an enclosing function's `...`, or has none to read, which is an error.
            Some(frame)
                if plain.is_some()
                    && matches!(call.arg(1).kind(), SexpType::Closure | SexpType::Builtin)
                    && eval::slot_dots(frame).is_some() =>
            {
                eval::slot_register(frame, index_symbol).map(|slot| (frame, slot))
            }
            _ => None,
        };
        let env = match slots {
            Some(_) => nil(),
            None => call.env()?,
        };
        let mut calls = ElementCalls {
            env,
            slots,
            index_symbol,
            index: nil(),
            bound_refcount: 0,
            plain,
            fun_call,
            known: atomic_elements(list),
        };
        calls.bind_index()?;
        Ok(calls)
    }

    /// Binds `i` to a fresh integer, in the frame's slot or its environment.
    unsafe fn bind_index(&mut self) -> R<()> {
        self.index = values::scalar_integer(0)?;
        match self.slots {
            Some((_, slot)) => {
                crate::heap().store_slot(&mut *bytecode::register_address(slot), self.index)
            }
            None => define(self.index_symbol, self.index, self.env)?,
        }
        self.bound_refcount = self.index.refcount();
        Ok(())
    }

    /// Makes the remaining calls from the frame's environment `env`. What the frame did with
    /// its `i` slot when it got the environment is its own business, so `i` is bound afresh.
    unsafe fn leave_slots(&mut self, env: Sexp) -> R<()> {
        self.env = env;
        self.slots = None;
        self.bind_index()
    }

    unsafe fn call(&mut self, call: &Call<'_>, position: usize) -> R<Sexp> {
        crate::scope(|| {
            if let Some((frame, _)) = self.slots {
                let env = interp().frames[frame].env.raw();
                if env != nil() {
                    self.leave_slots(env)?;
                }
            }
            if self.index.refcount() != self.bound_refcount {
                self.bind_index()?;
            }
            self.index.slice_mut::<i32>()[0] = position as i32 + 1;
            let first = match self.plain {
                Some(list) => {
                    let value = crate::subset::vector_element(list, position)?;
                    // An empty argument taken from a call, as in
                    // `lapply(as.list(quote(f(a, ))), FUN)`, reaches `FUN` as a promise whose
                    // value is the empty symbol, which `FUN` can inspect; bound directly, it
                    // would make `FUN`'s argument missing.
                    (value != values::missing_arg()).then_some(value)
                }
                None => None,
            };
            if let Some((frame, _)) = self.slots {
                // `FUN` is looked up for each element, and the frame's slot still holds the
                // closure the internal was called with unless a callee rebound it.
                let function = call.arg(1);
                if let Some(first) = first
                    && eval::slot_formal(frame, symbols().fun)
                        .is_some_and(|(value, _)| value == function)
                {
                    if function.kind() == SexpType::Closure {
                        return eval::force_and_call_from_slots(
                            self.fun_call,
                            function,
                            frame,
                            first,
                            self.known.as_mut(),
                        );
                    }
                    if let Some(value) =
                        eval::call_builtin_from_slots(self.fun_call, function, frame, first)?
                    {
                        return Ok(value);
                    }
                    // Forcing `...` for the builtin may have given the frame its environment.
                    let env = interp().frames[frame].env.raw();
                    if env != nil() {
                        self.leave_slots(env)?;
                        self.index.slice_mut::<i32>()[0] = position as i32 + 1;
                        return eval::force_and_call_with(
                            self.fun_call,
                            1,
                            self.env,
                            Some(first),
                            self.known.as_mut(),
                        );
                    }
                }
                self.leave_slots(call.env()?)?;
                self.index.slice_mut::<i32>()[0] = position as i32 + 1;
            }
            eval::force_and_call_with(self.fun_call, 1, self.env, first, self.known.as_mut())
        })
    }
}

/// The call `FUN(X[[i]], ...)`. Every `lapply` and `vapply` runs it for each element, and code
/// never changes where it is (a write through `sys.call()` or `substitute()` copies a value
/// something else holds), so the session keeps one.
unsafe fn element_call() -> R<Sexp> {
    let kept = interp().globals.element_call;
    if kept != nil() {
        return Ok(kept);
    }
    let symbols = symbols();
    let element = values::call(symbols.bracket2, &[symbols.upper_x, symbols.i])?;
    crate::heap().root(element);
    let call = values::call(symbols.fun, &[element, symbols.dots])?;
    for object in [element, call] {
        let mut cell = object;
        while cell != nil() {
            cell.mark_not_mutable();
            cell = cell.cdr();
        }
    }
    interp().globals.element_call = call;
    Ok(call)
}

/// What every element [`ElementCalls`] reads directly from from `list` is known to share: its storage type,
/// when `list` is an atomic vector.
unsafe fn atomic_elements(list: Sexp) -> Option<eval::KnownFirst> {
    match list.kind() {
        SexpType::Logical
        | SexpType::Integer
        | SexpType::Real
        | SexpType::Complex
        | SexpType::String
        | SexpType::Raw => Some(eval::KnownFirst::new(list.kind())),
        _ => None,
    }
}

unsafe fn do_vapply(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let count = length(list);
    let mut results = VapplyResults::new(call.arg(2), count, call.arg(3))?;
    let mut calls = ElementCalls::new(call, list)?;
    // As in `do_lapply`, a value's root goes once the results hold it, except the first value's:
    // the results keep its names for the answer, and only the value keeps them alive.
    let mut mark = None;
    for index in 0..count {
        let value = calls.call(call, index)?;
        results.push(index, value)?;
        match mark {
            Some(mark) => crate::heap().release_handles(mark),
            None => mark = Some(crate::heap().handle_mark()),
        }
    }
    results.finish(list)
}

fn do_mapply<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let function = arguments.value(0);
    let Some(dots) = handles::dense::<List>(context, arguments.value(1))? else {
        return Err(arguments.error("argument 'dots' is not a list"));
    };
    let more = handles::dense::<List>(context, arguments.value(2))?;
    let caller = arguments.environment(context)?;
    let lists: SmallVec<[Local<'s>; 4]> = (0..dots.len())
        .map(|index| context.root(dots.get(context, index)))
        .collect();
    // A classed argument may define its own `length` and `[[` methods, which mapply honours.
    let mut lengths: SmallVec<[usize; 4]> = SmallVec::new();
    for &list in &lists {
        lengths.push(crate::subset::length_dispatched(context, list, caller)?);
    }
    let subset2 = handles::primitive("[[").ok_or_else(|| arguments.error("no [[ primitive"))?;
    let subset2 = context.root(subset2);
    let names = string_names(context, dots.value())?;
    let more_names = match more {
        Some(more) => string_names(context, more.value())?,
        None => None,
    };
    let longest = if lengths.contains(&0) {
        0
    } else {
        lengths.iter().copied().max().unwrap_or(0)
    };
    let mut result = context.alloc::<List>(longest)?;
    for index in 0..longest {
        let value = context.scope_enclosed(|context, outer| {
            let caller = outer.pass(caller);
            let mut items: SmallVec<[Tagged<'_>; 4]> = SmallVec::new();
            for (position, &list) in lists.iter().enumerate() {
                let list = outer.pass(list);
                let at = index % lengths[position];
                let value = if handles::is_object(list) {
                    let at = handles::integer(context, at as i32 + 1)?;
                    let subset_arguments = [
                        Tagged {
                            tag: handles::null(),
                            value: list,
                        },
                        Tagged {
                            tag: handles::null(),
                            value: at,
                        },
                    ];
                    let subset2 = outer.pass(subset2);
                    handles::apply(context, handles::null(), subset2, &subset_arguments, caller)?
                } else {
                    list_element(context, list, at)?
                };
                let tag = match names {
                    Some(names) if !names.get(context, position).bytes().is_empty() => {
                        let name = context.root(names.get(context, position));
                        let symbol = handles::install_interned(context, name)?;
                        handles::root_unless_permanent(context, symbol.value())
                    }
                    _ => handles::null(),
                };
                items.push(Tagged { tag, value });
            }
            if let Some(more) = more {
                let more = outer.pass(more);
                for position in 0..more.len() {
                    let tag = match more_names {
                        Some(names) => {
                            let name = context.root(names.get(context, position));
                            let symbol = handles::install_interned(context, name)?;
                            handles::root_unless_permanent(context, symbol.value())
                        }
                        None => handles::null(),
                    };
                    let value =
                        handles::root_unless_permanent(context, more.get(context, position));
                    items.push(Tagged { tag, value });
                }
            }
            handles::apply(
                context,
                handles::null(),
                outer.pass(function),
                &items,
                caller,
            )
        })?;
        result.set(context, index, value);
    }
    Ok(result.finish().value())
}

/// The names of `object` as a character vector, or `None` when it has none.
fn string_names<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Option<Local<'s, Str>>> {
    let names = handles::get_attribute(context, object, handles::symbol(|symbols| symbols.names));
    let names = context.root(names);
    handles::dense::<Str>(context, names)
}

/// Element `index` of a vector, or the element at that position of a pairlist.
fn list_element<'s>(context: &mut Cx<'s>, list: Local<'s>, index: usize) -> R<Local<'s>> {
    if !matches!(
        list.r_type(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        return crate::subset::element(context, list, index);
    }
    let mut cell = list.read(context);
    for _ in 0..index {
        match handles::cell_fields(cell) {
            Some((_, _, rest)) => cell = rest,
            None => break,
        }
    }
    let value = handles::cell_fields(cell).map_or(cell, |(value, _, _)| value);
    Ok(context.root(value))
}

/// What every element `rapply` reaches reads: the function, the classes it applies to, the
/// value for the others, whether it replaces them, and the frame it calls the function from.
#[derive(Clone, Copy)]
struct Rapply<'s> {
    function: Local<'s>,
    classes: Local<'s, Str>,
    default: Local<'s>,
    replace: bool,
    environment: Local<'s, Environment>,
}

impl<'s> Rapply<'s> {
    fn pass<'inner>(self, outer: heap::handle::Enclosing<'s, 'inner>) -> Rapply<'inner> {
        Rapply {
            function: outer.pass(self.function),
            classes: outer.pass(self.classes),
            default: outer.pass(self.default),
            replace: self.replace,
            environment: outer.pass(self.environment),
        }
    }
}

fn do_rapply<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    if !matches!(object.r_type(), SexpType::List | SexpType::Expression) {
        return Err(arguments.error("'object' must be a list or expression"));
    }
    let function = arguments.value(1);
    if !values::is_function_type(function.r_type()) {
        return Err(arguments.error("invalid 'f' argument"));
    }
    let Some(classes) = handles::dense::<Str>(context, arguments.value(2))? else {
        return Err(arguments.error("invalid 'classes' argument"));
    };
    let how = arguments.value(4);
    if how.r_type() != SexpType::String {
        return Err(arguments.error("invalid 'how' argument"));
    }
    let rapply = Rapply {
        function,
        classes,
        default: arguments.value(3),
        replace: required_text(context, how, "how")? == "replace",
        environment: arguments.environment(context)?,
    };
    rapply_list(context, rapply, object)
}

/// Applies `rapply` to each element of a list or expression vector. As in GNU R, replace mode
/// keeps the container's type and attributes, and the other modes give a list that keeps only
/// the names.
fn rapply_list<'s>(context: &mut Cx<'s>, rapply: Rapply<'s>, object: Local<'s>) -> R<Local<'s>> {
    if object.r_type() == SexpType::Expression {
        return match handles::dense::<Expression>(context, object)? {
            Some(object) => rapply_elements(context, rapply, object),
            None => Err(error(
                "internal error: an expression vector is not standard",
            )),
        };
    }
    match handles::dense::<List>(context, object)? {
        Some(object) => rapply_elements(context, rapply, object),
        None if rapply.replace => Ok(handles::null()),
        None => Ok(context.alloc::<List>(0)?.finish().value()),
    }
}

fn rapply_elements<'s, T: Elements<Element = Value>>(
    context: &mut Cx<'s>,
    rapply: Rapply<'s>,
    object: Local<'s, T>,
) -> R<Local<'s>> {
    let count = object.len();
    let mut results: SmallVec<[Local<'s>; 8]> = SmallVec::with_capacity(count);
    for index in 0..count {
        let element = context.root(object.get(context, index));
        let value = if matches!(
            element.r_type(),
            SexpType::Nil | SexpType::List | SexpType::Expression
        ) {
            context.scope_enclosed(|context, outer| {
                let element = outer.pass(element);
                rapply_list(context, rapply.pass(outer), element)
            })?
        } else {
            context.scope_enclosed(|context, outer| {
                let element = outer.pass(element);
                rapply_leaf(context, rapply.pass(outer), element)
            })?
        };
        results.push(value);
    }
    if rapply.replace {
        let mut copy = handles::shallow_duplicate(context, object)?;
        for (index, &value) in results.iter().enumerate() {
            copy.set(context, index, value);
        }
        return Ok(copy.finish().value());
    }
    let mut list = context.alloc::<List>(count)?;
    for (index, &value) in results.iter().enumerate() {
        list.set(context, index, value);
    }
    let mut list = list.owned();
    if let Some(names) = string_names(context, object.value())? {
        handles::set_attribute(
            context,
            &mut list,
            handles::symbol(|symbols| symbols.names),
            names.value(),
        )?;
    }
    Ok(list.finish().value())
}

fn rapply_leaf<'s>(context: &mut Cx<'s>, rapply: Rapply<'s>, object: Local<'s>) -> R<Local<'s>> {
    let classes = rapply.classes;
    let matches = if !classes.is_empty() && classes.get(context, 0).bytes() == b"ANY" {
        true
    } else {
        let class = crate::dispatch::class_of(context, object)?;
        match handles::dense::<Str>(context, class)? {
            Some(class) => (0..classes.len()).any(|index| {
                let wanted = classes.get(context, index).bytes();
                (0..class.len()).any(|at| class.get(context, at).bytes() == wanted)
            }),
            None => false,
        }
    };
    if !matches {
        return Ok(if rapply.replace {
            object
        } else {
            rapply.default
        });
    }
    // GNU R binds the element to `X` in the `rapply` closure's frame and evaluates `FUN(X, ...)`
    // there, so the extra arguments reach `FUN`.
    let x = handles::install_bytes(context, b"X")?;
    let x = context.root(x);
    eval::define_variable(context, x, object, rapply.environment)?;
    let rest = handles::chain(
        context,
        &[
            Tagged {
                tag: handles::null(),
                value: x.value(),
            },
            Tagged {
                tag: handles::null(),
                value: handles::permanent_symbol(|symbols| symbols.dots).value(),
            },
        ],
        handles::null(),
    )?;
    let call = handles::language(context, rapply.function, rest)?;
    handles::force_and_call(context, call, 1, rapply.environment)
}

fn do_eapply<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let environment = environment_arg(context, arguments.value(0), "env")?;
    let all = handles::logical_scalar(context, arguments.value(2)) == 1;
    let use_names = handles::logical_scalar(context, arguments.value(3)) != 0;
    let bound = environment_symbols(context, environment.read(context).value(), all, false);
    let caller = arguments.environment(context)?;
    let mut result = context.alloc::<List>(bound.len())?;
    // GNU R evaluates `FUN(quote(value), ...)` in the `eapply` closure's frame, so the extra
    // arguments reach `FUN`.
    let function = handles::install_bytes(context, b"FUN")?;
    let quote = handles::install_bytes(context, b"quote")?;
    for (index, &symbol) in bound.iter().enumerate() {
        let value = context.scope_enclosed(|context, outer| {
            let (environment, caller) = (outer.pass(environment), outer.pass(caller));
            let value = match eval::frame_content(context, environment, symbol)? {
                Some(value) => eval::forced_value(context, value)?,
                None => handles::unbound(),
            };
            let quoted = handles::cons(context, handles::null(), value, handles::null())?;
            let quoted = handles::language(context, context.root(quote).value(), quoted)?;
            let rest = handles::chain(
                context,
                &[
                    Tagged {
                        tag: handles::null(),
                        value: quoted.value(),
                    },
                    Tagged {
                        tag: handles::null(),
                        value: handles::permanent_symbol(|symbols| symbols.dots).value(),
                    },
                ],
                handles::null(),
            )?;
            let call = handles::language(context, context.root(function).value(), rest)?;
            handles::force_and_call(context, call, 1, caller)
        })?;
        result.set(context, index, value);
    }
    let mut result = result.owned();
    if use_names {
        let names = symbol_names(context, &bound)?;
        handles::set_attribute(
            context,
            &mut result,
            handles::symbol(|symbols| symbols.names),
            names,
        )?;
    }
    Ok(result.finish().value())
}

/// The `...` a dots builtin reads: the calling slot frame's `...` slot, or `...` in the calling
/// environment, which must bind it.
fn dots_of<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    if let Some(dots) = arguments.slot_dots(context) {
        return Ok(context.root(dots));
    }
    let environment = arguments.environment(context)?;
    let dots = handles::permanent_symbol(|symbols| symbols.dots);
    eval::variable(context, dots, environment)?
        .ok_or_else(|| error("incorrect context: the current call has no '...' to look in"))
}

fn do_dots_length<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let dots = dots_of(context, arguments)?;
    let mut count = 0;
    if dots.r_type() == SexpType::Dots {
        let mut cell = dots.read(context);
        while let Some((_, _, rest)) = handles::cell_fields(cell) {
            count += 1;
            cell = rest;
        }
    }
    handles::integer(context, count)
}

fn do_dots_elt<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let index = handles::integer_scalar(context, arguments.value(0));
    if index < 1 {
        return Err(error(format!(
            "indexing '...' with non-positive index {index}"
        )));
    }
    // Forcing the element's promise decides the visibility, as evaluating `..n` would.
    interp().visible = true;
    let slot_dots = arguments.slot_dots(context).map(|dots| context.root(dots));
    let dots = match slot_dots {
        Some(dots) => dots,
        None => {
            let environment = arguments.environment(context)?;
            let dots = handles::permanent_symbol(|symbols| symbols.dots);
            eval::variable(context, dots, environment)?.unwrap_or_else(handles::unbound)
        }
    };
    let element = {
        let mut cell = dots.read(context);
        for _ in 1..index {
            match handles::cell_fields(cell) {
                Some((_, _, rest)) => cell = rest,
                None => break,
            }
        }
        handles::cell_fields(cell).map(|(value, _, _)| context.root(value))
    };
    if let Some(element) = element {
        // GNU R evaluates the element itself, so an empty one is a missing argument with no
        // name.
        if handles::is_missing_argument(element) {
            let environment = arguments.environment(context)?;
            let call = handles::lexical_call(context, environment);
            return Err(missing_argument(call.raw(), "", ArgumentReader::Eval));
        }
        if slot_dots.is_some() {
            return handles::force(context, element);
        }
    }
    // Evaluating `..n` reports an absent element.
    let symbol = handles::install_bytes(context, format!("..{index}").as_bytes())?;
    let symbol = context.root(symbol);
    let environment = arguments.environment(context)?;
    eval::symbol_value(context, symbol, environment)
}

fn do_dots_names<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let dots = dots_of(context, arguments)?;
    if dots.r_type() != SexpType::Dots {
        return Ok(handles::null());
    }
    let names: Vec<Option<String>> = {
        let mut names = Vec::new();
        let mut cell = dots.read(context);
        while let Some((_, tag, rest)) = handles::cell_fields(cell) {
            names.push(
                tag.downcast::<Symbol>()
                    .map(|tag| handles::text(tag.name()).into_owned()),
            );
            cell = rest;
        }
        names
    };
    if names.iter().all(Option::is_none) {
        return Ok(handles::null());
    }
    let names: Vec<String> = names.into_iter().map(Option::unwrap_or_default).collect();
    Ok(text_vector(context, &names)?.value())
}

fn do_env_profile<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(environment) = arguments.value(0).downcast::<Environment>() else {
        return Err(arguments.error("argument must be a hashed environment"));
    };
    let Some((counts, chains)) = handles::frame_hash_profile(context, environment) else {
        return Ok(handles::null());
    };
    let mut profile = context.alloc::<List>(3)?;
    let size = handles::integer(context, counts.len() as i32)?;
    profile.set(context, 0, size);
    let chains = handles::integer(context, chains as i32)?;
    profile.set(context, 1, chains);
    let mut bucket_counts = context.alloc::<Integer>(counts.len())?;
    for (slot, &count) in bucket_counts.as_mut_slice().iter_mut().zip(&counts) {
        *slot = count as i32;
    }
    let bucket_counts = bucket_counts.finish();
    profile.set(context, 2, bucket_counts.value());
    let mut profile = profile.owned();
    let names = text_vector(context, &["size", "nchains", "counts"])?.value();
    handles::set_attribute(
        context,
        &mut profile,
        handles::symbol(|symbols| symbols.names),
        names,
    )?;
    Ok(profile.finish().value())
}

fn do_formals<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let function = arguments.value(0);
    if let Some(closure) = function.downcast::<Closure>() {
        return Ok(context.root(closure.read(context).formals()));
    }
    if !values::is_function_type(function.r_type()) {
        arguments.warning(context, "argument is not a function")?;
    }
    Ok(handles::null())
}

fn do_body<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let function = arguments.value(0);
    if let Some(closure) = function.downcast::<Closure>() {
        return Ok(context.root(body_source(closure.read(context).body())));
    }
    if !values::is_function_type(function.r_type()) {
        arguments.warning(context, "argument is not a function")?;
    }
    Ok(handles::null())
}

/// A closure's body as GNU R stores it, which is byte code for every function GNU R ships. ry
/// compiles a closure on demand here so that code inspecting the body sees the same type.
fn do_body_code<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    match arguments.value(0).downcast::<Closure>() {
        Some(closure) => handles::compiled_body(context, closure),
        None => Ok(handles::null()),
    }
}

fn do_args<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let mut function = arguments.value(0);
    if function.r_type() == SexpType::String
        && handles::length(context, function) == 1
        && let Some(name) = handles::dense::<Str>(context, function)?
    {
        let symbol = handles::install_element(context, name, 0)?;
        let environment = arguments.environment(context)?;
        function = eval::function_value(context, symbol, environment, None)?;
    }
    if let Some(closure) = function.downcast::<Closure>() {
        let formals = context.root(closure.read(context).formals());
        return handles::formals_closure(context, formals, handles::global_environment());
    }
    if !matches!(function.r_type(), SexpType::Builtin | SexpType::Special) {
        return Ok(handles::null());
    }
    // The formals of a primitive live in `.ArgsEnv` and `.GenericArgsEnv`.
    let name =
        handles::builtin_definition(function).map_or("<unknown>", |definition| definition.name);
    for table in [".ArgsEnv", ".GenericArgsEnv"] {
        let table = handles::install_bytes(context, table.as_bytes())?;
        let base = handles::base_namespace().value();
        let Some(table) = handles::frame_symbol_value(context, base, table)? else {
            continue;
        };
        let symbol = handles::install_bytes(context, name.as_bytes())?;
        if let Some(value) = handles::frame_symbol_value(context, table, symbol)?
            && let Some(closure) = value.downcast::<Closure>()
        {
            let formals = context.root(closure.read(context).formals());
            return handles::formals_closure(context, formals, handles::global_environment());
        }
    }
    Ok(handles::null())
}

/// `match.call(definition, call, expand.dots, envir)`.
fn do_match_call<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let Some(target) = arguments.value(1).downcast::<Language>() else {
        return Err(arguments.error("invalid 'call' argument"));
    };
    let expand_dots = handles::logical_scalar(context, arguments.value(2)) != 0;
    let dots_environment = arguments.value(3).downcast::<Environment>();
    let head = context.root(target.read(context).head());
    let mut definition = arguments.value(0);
    if definition.r_type() == SexpType::Nil
        && let Some(head) = head.downcast::<Symbol>()
    {
        let environment = arguments.environment(context)?;
        definition = eval::function_value(context, head, environment, None)?;
    }
    let Some(definition) = definition.downcast::<Closure>() else {
        return Err(arguments.error("invalid 'definition' argument"));
    };
    let written = cell_items(context, context.root(target.read(context).rest()));
    let dots = handles::symbol(|symbols| symbols.dots);
    // Expand `...` in the call from the environment that supplies it.
    let mut supplied: SmallVec<[Tagged<'s>; 8]> = SmallVec::new();
    for item in written {
        if item.value.raw() != dots.raw() {
            supplied.push(item);
            continue;
        }
        let Some(dots_environment) = dots_environment else {
            continue;
        };
        let Some(found) = eval::variable(context, context.root(dots), dots_environment)? else {
            continue;
        };
        if found.r_type() != SexpType::Dots {
            continue;
        }
        // GNU R's `subDots` names a symbol or call by its position, `..1`, so the call still
        // evaluates in the frame that holds the dots (`write.csv` forwards its call with
        // `eval.parent`). A constant stays as it is.
        for (index, item) in cell_items(context, found).into_iter().enumerate() {
            let code = {
                let mut code = item.value.read(context);
                while let Some(promise) = code.downcast::<Promise>() {
                    code = body_source(promise.code());
                }
                context.root(code)
            };
            let value = if matches!(code.r_type(), SexpType::Symbol | SexpType::Language) {
                let symbol =
                    handles::install_bytes(context, format!("..{}", index + 1).as_bytes())?;
                handles::root_unless_permanent(context, symbol.value())
            } else {
                code
            };
            supplied.push(Tagged {
                tag: item.tag,
                value,
            });
        }
    }
    let formals = context.root(definition.read(context).formals());
    let matched = eval::match_args(context, formals, &supplied, target.value())?;
    let formal_symbols = cell_items(context, formals);
    let mut items: SmallVec<[Tagged<'s>; 8]> = SmallVec::new();
    // Matched arguments come out in formal order; `...` contributes its contents.
    for (formal, &value) in formal_symbols.iter().zip(matched.iter()) {
        if handles::is_missing_argument(value) {
            continue;
        }
        if formal.tag.raw() != dots.raw() {
            items.push(Tagged {
                tag: formal.tag,
                value,
            });
            continue;
        }
        let dot_items = cell_items(context, value);
        if expand_dots {
            items.extend(dot_items);
        } else {
            let list = handles::chain(context, &dot_items, handles::null())?;
            items.push(Tagged {
                tag: formal.tag,
                value: list,
            });
        }
    }
    let rest = handles::chain(context, &items, handles::null())?;
    Ok(handles::language(context, head, rest)?.value())
}

/// The tag and value of each cell of a pairlist, call or `...`.
fn cell_items<'s>(context: &Cx<'s>, list: Local<'s>) -> SmallVec<[Tagged<'s>; 8]> {
    let mut items = SmallVec::new();
    let mut cell = list.read(context);
    while let Some((value, tag, rest)) = handles::cell_fields(cell) {
        items.push(Tagged {
            tag: handles::root_unless_permanent(context, tag),
            value: handles::root_unless_permanent(context, value),
        });
        cell = rest;
    }
    items
}

/// The symbol a side of `::` names: a symbol as written, or a one-element string.
fn colon_symbol<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Local<'s, Symbol>> {
    if let Some(symbol) = value.downcast::<Symbol>() {
        return Ok(symbol);
    }
    match handles::dense::<Str>(context, value)? {
        Some(name) if name.len() == 1 => handles::install_element(context, name, 0),
        _ => Err(error("bad namespace name")),
    }
}

/// `pkg::name`.
fn do_exported_value<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    namespace_value(context, arguments, true)
}

/// `pkg:::name`.
fn do_internal_value<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    namespace_value(context, arguments, false)
}

/// `pkg::name` (exported only) and `pkg:::name`.
fn namespace_value<'s>(
    context: &mut Cx<'s>,
    arguments: &Args<'_, 's>,
    exported: bool,
) -> R<Local<'s>> {
    let package = colon_symbol(context, arguments.value(0))?;
    let symbol = colon_symbol(context, arguments.value(1))?;
    namespace_lookup(context, package.value(), symbol, exported)
}

/// `getNamespaceValue(ns, name, exported)`.
fn do_get_namespace_value<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let name = match handles::dense::<Str>(context, arguments.value(1))? {
        Some(name) if name.len() == 1 => name,
        _ => return Err(error("invalid 'name' argument")),
    };
    let symbol = handles::install_element(context, name, 0)?;
    let exported = handles::logical_scalar(context, arguments.value(2)) == 1;
    namespace_lookup(context, arguments.value(0), symbol, exported)
}

/// `package::symbol` (or `:::` when not `exported`) for a package named by a symbol, with the
/// binding cell the value came from when reading that cell again gives the same answer as long
/// as the lookup stamps of `package`, `symbol`, `.__NAMESPACE__.` and `exports` stand still.
/// Every binding the lookup read on the way (the registry's, the namespace information's, the
/// export table's entry) is marked watched, so assigning one moves its symbol's stamp; adding or
/// removing a binding moves it anyway. Lazy data, renamed exports and lookups that pass a
/// function frame give no cell.
pub unsafe fn namespace_binding(
    package: Sexp,
    symbol: Sexp,
    exported: bool,
) -> R<(Sexp, Option<Sexp>)> {
    let value = handles::with_result(|context| {
        let package_name = handles::held(context, package);
        let symbol = handles::held(context, symbol)
            .downcast::<Symbol>()
            .ok_or_else(|| error("internal error: a namespace lookup names no symbol"))?;
        Ok(namespace_lookup(context, package_name, symbol, exported)?.raw())
    })?;
    Ok((value, namespace_cell(package, symbol, exported)))
}

unsafe fn namespace_cell(package: Sexp, symbol: Sexp, exported: bool) -> Option<Sexp> {
    let heap = crate::heap();
    let symbols = symbols();
    let watch = |cell: Sexp| cell.set_gp_bits(gp::WATCHED, true);
    let namespace = if package == symbols.base {
        base_namespace()
    } else {
        let entry = heap.frame_cell(heap.known().namespace_registry, package)?;
        let namespace = entry.cdr();
        if namespace.kind() != SexpType::Environment || entry.has_gp(gp::ACTIVE_BINDING) {
            return None;
        }
        watch(entry);
        namespace
    };
    if namespace == base_namespace() || !exported {
        return heap.frame_cell(namespace, symbol);
    }
    let environment_in = |env: Sexp, name: Sexp| -> Option<(Sexp, Sexp)> {
        let cell = heap.frame_cell(env, name)?;
        let value = cell.cdr();
        (value.kind() == SexpType::Environment && !cell.has_gp(gp::ACTIVE_BINDING))
            .then_some((cell, value))
    };
    let (info_cell, info) = environment_in(namespace, symbols.dot_namespace)?;
    let (exports_cell, exports) = environment_in(info, symbols.exports)?;
    let entry = heap.frame_cell(exports, symbol)?;
    let internal = entry.cdr();
    let renamed = internal.kind() == SexpType::String
        && length(internal) > 0
        && values::install_charsxp(internal.element(0)).ok()? != symbol;
    if renamed || entry.has_gp(gp::ACTIVE_BINDING) {
        return None;
    }
    watch(info_cell);
    watch(exports_cell);
    watch(entry);
    let empty = empty_env();
    let mut env = namespace;
    while env != empty {
        if env.has_gp(gp::FUNCTION_FRAME) {
            return None;
        }
        if let Some(cell) = heap.frame_cell(env, symbol) {
            heap.observe_environment(namespace);
            return Some(cell);
        }
        env = env.enclosure();
    }
    None
}

/// Looks `symbol` up in a package's namespace (a namespace environment, or a package name as a
/// symbol or a string), loading it if needed. An exported name is looked up through the
/// namespace's export table, which maps it to the internal name, from the namespace outward,
/// because a package may export a function it imports, as graphics does with base's `plot`; lazy
/// data comes last. An internal name (`:::`) must be bound in the namespace itself.
fn namespace_lookup<'s>(
    context: &mut Cx<'s>,
    package: Local<'s>,
    symbol: Local<'s, Symbol>,
    exported: bool,
) -> R<Local<'s>> {
    let base = handles::base_namespace();
    let namespace = match package.downcast::<Environment>() {
        Some(namespace) => namespace,
        None => match package_symbol(context, package)? {
            Some(name) if name.is(handles::symbol(|symbols| symbols.base)) => base,
            Some(name) => match loaded_namespace(context, name)? {
                Some(namespace) => namespace,
                None => load_namespace(context, package, Some(name))?,
            },
            None => load_namespace(context, package, None)?,
        },
    };
    let not_found = |context: &Cx<'_>| {
        error(format!(
            "object '{}' not found",
            symbol_text(context, symbol)
        ))
    };
    if namespace.is(base) || !exported {
        return match eval::frame_content(context, namespace, symbol)? {
            Some(value) => handles::force(context, value),
            None => Err(not_found(context)),
        };
    }
    let info = eval::frame_content(
        context,
        namespace,
        handles::symbol(|symbols| symbols.dot_namespace),
    )?
    .and_then(|info| info.downcast::<Environment>());
    let exports = handles::symbol(|symbols| symbols.exports);
    if let Some(internal) = namespace_table(context, info, exports, symbol)? {
        let internal_symbol = match handles::dense::<Str>(context, internal)? {
            Some(name) if !name.is_empty() => handles::install_element(context, name, 0)?,
            _ => symbol,
        };
        return match eval::variable(context, internal_symbol, namespace)? {
            Some(value) => handles::force(context, value),
            None => Err(not_found(context)),
        };
    }
    let lazydata = handles::symbol(|symbols| symbols.lazydata);
    if let Some(data) = namespace_table(context, info, lazydata, symbol)? {
        return handles::force(context, data);
    }
    let package_name = match info {
        Some(info) => {
            let spec = handles::symbol(|symbols| symbols.spec);
            match eval::frame_content(context, info, spec)? {
                Some(spec) => handles::first_string(context, spec)
                    .map(Cow::into_owned)
                    .unwrap_or_default(),
                None => String::new(),
            }
        }
        None => package_symbol(context, package)?
            .map(|package| symbol_text(context, package))
            .unwrap_or_default(),
    };
    Err(error(format!(
        "'{}' is not an exported object from 'namespace:{package_name}'",
        symbol_text(context, symbol)
    )))
}

/// The entry for `symbol` in the table `field` (`exports` or `lazydata`) of a namespace's
/// information environment, unforced.
fn namespace_table<'s>(
    context: &mut Cx<'s>,
    info: Option<Local<'s, Environment>>,
    field: Gc<'static, Symbol>,
    symbol: Local<'s, Symbol>,
) -> R<Option<Local<'s>>> {
    let Some(info) = info else {
        return Ok(None);
    };
    let table = eval::frame_content(context, info, field)?;
    match table.and_then(|table| table.downcast::<Environment>()) {
        Some(table) => eval::frame_content(context, table, symbol),
        None => Ok(None),
    }
}

/// The namespace `loadNamespace` loads for `package`. GNU R evaluates `loadNamespace(x)` with
/// `x` bound to the name, so an error from a missing package names that call.
fn load_namespace<'s>(
    context: &mut Cx<'s>,
    package: Local<'s>,
    name: Option<Local<'s, Symbol>>,
) -> R<Local<'s, Environment>> {
    let name = match name {
        Some(name) if package.r_type() == SexpType::Symbol => {
            let mut text = context.alloc::<Str>(1)?;
            text.set(context, 0, name.read(context).name());
            text.finish().value()
        }
        _ => package,
    };
    let frame = handles::new_environment(context, handles::base_namespace())?;
    let x = handles::permanent_symbol(|symbols| symbols.x);
    eval::define_variable(context, x, name, frame)?;
    let load = handles::permanent_symbol(|symbols| symbols.load_namespace);
    let rest = handles::cons(context, handles::null(), x.value(), handles::null())?;
    let call = handles::language(context, load.value(), rest)?;
    handles::evaluate(context, call.value(), frame)?
        .downcast::<Environment>()
        .ok_or_else(|| error("internal error: loadNamespace() gave no environment"))
}

/// The symbol of a package named by a symbol or by the first element of a string vector.
fn package_symbol<'s>(context: &mut Cx<'s>, package: Local<'s>) -> R<Option<Local<'s, Symbol>>> {
    if let Some(symbol) = package.downcast::<Symbol>() {
        return Ok(Some(symbol));
    }
    match handles::dense::<Str>(context, package)? {
        Some(name) if !name.is_empty() => handles::install_element(context, name, 0).map(Some),
        _ => Ok(None),
    }
}

/// The namespace registered for the package named by the symbol `package`, without calling
/// `asNamespace`, which costs several microseconds per `pkg::name` and would dominate a hot loop
/// that uses one.
fn loaded_namespace<'s>(
    context: &mut Cx<'s>,
    package: Local<'s, Symbol>,
) -> R<Option<Local<'s, Environment>>> {
    let registry = handles::namespace_registry();
    let Some(namespace) = eval::frame_content(context, registry, package)? else {
        return Ok(None);
    };
    Ok(handles::force(context, namespace)?.downcast::<Environment>())
}
