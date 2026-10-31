//! Environments and variable lookup.

use crate::{
    Rboolean,
    boundary::{ApiResult, enter, error},
    eval::force_promise,
    runtime,
    strings::utf8_text,
};
use heap::object::{Sexp, SexpType, gp};
use std::ffi::c_int;

unsafe fn environment(object: Sexp, what: &str) -> ApiResult<Sexp> {
    match object.kind() {
        SexpType::Environment => Ok(object),
        SexpType::Nil => Err(error("use of NULL environment is defunct")),
        kind => Err(error(format!(
            "argument to '{what}' is not an environment, but a '{}'",
            kind.name()
        ))),
    }
}

unsafe fn symbol_name(symbol: Sexp) -> String {
    utf8_text(symbol.car())
        .map(|text| text.into_owned())
        .unwrap_or_default()
}

/// The value bound to `symbol` in `env`'s own frame, or `R_UnboundValue`.
pub unsafe fn frame_value(env: Sexp, symbol: Sexp) -> Sexp {
    let runtime = runtime();
    match runtime.heap.frame_cell(env, symbol) {
        Some(cell) => cell.cdr(),
        None => runtime.heap.known().unbound,
    }
}

/// The value bound to `symbol` in `env`'s own frame, or `R_UnboundValue`, with an active
/// binding's function called to produce it.
pub unsafe fn frame_lookup(env: Sexp, symbol: Sexp) -> ApiResult<Sexp> {
    match runtime().heap.frame_cell(env, symbol) {
        Some(cell) => cell_value(cell),
        None => Ok(runtime().heap.known().unbound),
    }
}

/// A binding cell's value. An active binding's value is what its function returns when called
/// with no arguments.
pub unsafe fn cell_value(cell: Sexp) -> ApiResult<Sexp> {
    if !cell.has_gp(gp::ACTIVE_BINDING) {
        return Ok(cell.cdr());
    }
    let global = runtime().heap.known().global_env;
    crate::eval::apply_closure_or_primitive(cell.cdr(), &[], global)
}

/// Assigns through a binding cell. Assigning to an active binding calls its function with the
/// value instead of replacing the function.
pub unsafe fn assign_cell(cell: Sexp, value: Sexp) -> ApiResult<()> {
    if cell.has_gp(gp::ACTIVE_BINDING) {
        if cell.has_gp(gp::BINDING_LOCKED) {
            return Err(error(format!(
                "cannot change active binding if binding is locked"
            )));
        }
        let global = runtime().heap.known().global_env;
        crate::eval::apply_closure_or_primitive(cell.cdr(), &[value], global)?;
        return Ok(());
    }
    runtime()
        .heap
        .set_binding_value(cell, value)
        .map_err(Into::into)
}

/// `Rf_findVar`: the first binding of `symbol` from `env` outward, or `R_UnboundValue`.
pub unsafe fn find_var(symbol: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let empty = runtime.heap.known().empty_env;
    let mut env = environment(env, "findVar")?;
    while env != empty {
        if let Some(cell) = runtime.heap.frame_cell(env, symbol) {
            return cell_value(cell);
        }
        env = env.enclosure();
    }
    Ok(runtime.heap.known().unbound)
}

pub unsafe fn define_var(symbol: Sexp, value: Sexp, env: Sexp) -> ApiResult<()> {
    let env = environment(env, "defineVar")?;
    if symbol.kind() != SexpType::Symbol {
        return Err(error("invalid symbol for defineVar"));
    }
    if let Some(cell) = runtime().heap.frame_cell(env, symbol) {
        return assign_cell(cell, value);
    }
    runtime()
        .heap
        .define(env, symbol, value)
        .map_err(Into::into)
}

/// `Rf_findFun`: the first binding of `symbol` from `env` outward whose value is a function.
pub unsafe fn find_fun(symbol: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let known = *runtime.heap.known();
    let mut env = environment(env, "findFun")?;
    while env != known.empty_env {
        let mut value = frame_lookup(env, symbol)?;
        if value.kind() == SexpType::Promise {
            value = force_promise(value)?;
        }
        if matches!(
            value.kind(),
            SexpType::Closure | SexpType::Builtin | SexpType::Special
        ) {
            return Ok(value);
        }
        if value == known.missing_arg {
            return Err(error(format!(
                "argument \"{}\" is missing, with no default",
                symbol_name(symbol)
            )));
        }
        env = env.enclosure();
    }
    Err(error(format!(
        "could not find function \"{}\"",
        symbol_name(symbol)
    )))
}

unsafe fn get_var(
    symbol: Sexp,
    env: Sexp,
    inherits: bool,
    if_not_found: Option<Sexp>,
) -> ApiResult<Sexp> {
    let runtime = runtime();
    let known = *runtime.heap.known();
    if symbol.kind() != SexpType::Symbol {
        return Err(error("not a symbol"));
    }
    let env = environment(env, "R_getVar")?;
    let value = if inherits {
        find_var(symbol, env)?
    } else {
        frame_lookup(env, symbol)?
    };
    if value == known.unbound {
        return match if_not_found {
            Some(fallback) => Ok(fallback),
            None => Err(error(format!("object '{}' not found", symbol_name(symbol)))),
        };
    }
    if value == known.missing_arg {
        return Err(error(format!(
            "argument \"{}\" is missing, with no default",
            symbol_name(symbol)
        )));
    }
    if value.kind() == SexpType::Promise {
        return force_promise(value);
    }
    Ok(value)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_defineVar(symbol: Sexp, value: Sexp, env: Sexp) {
    enter(|| define_var(symbol, value, env))
}

/// `Rf_setVar`: assigns to the first existing binding from `env` outward, or defines the
/// variable in the global environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_setVar(symbol: Sexp, value: Sexp, env: Sexp) {
    enter(|| {
        let runtime = runtime();
        let known = *runtime.heap.known();
        let mut env = environment(env, "setVar")?;
        while env != known.empty_env {
            if let Some(cell) = runtime.heap.frame_cell(env, symbol) {
                return assign_cell(cell, value);
            }
            env = env.enclosure();
        }
        define_var(symbol, value, known.global_env)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_findVar(symbol: Sexp, env: Sexp) -> Sexp {
    enter(|| find_var(symbol, env))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_findVarInFrame(env: Sexp, symbol: Sexp) -> Sexp {
    enter(|| frame_lookup(environment(env, "findVarInFrame")?, symbol))
}

/// With `do_get` false GNU R only checks for existence; the value is cheap to return either way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_findVarInFrame3(env: Sexp, symbol: Sexp, _do_get: Rboolean) -> Sexp {
    enter(|| frame_lookup(environment(env, "findVarInFrame3")?, symbol))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getVar(symbol: Sexp, env: Sexp, inherits: Rboolean) -> Sexp {
    enter(|| get_var(symbol, env, inherits != 0, None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getVarEx(
    symbol: Sexp,
    env: Sexp,
    inherits: Rboolean,
    if_not_found: Sexp,
) -> Sexp {
    enter(|| get_var(symbol, env, inherits != 0, Some(if_not_found)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_existsVarInFrame(env: Sexp, symbol: Sexp) -> Rboolean {
    enter(|| {
        Ok(runtime()
            .heap
            .frame_cell(environment(env, "existsVarInFrame")?, symbol)
            .is_some() as Rboolean)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_removeVarFromFrame(symbol: Sexp, env: Sexp) {
    enter(|| {
        let env = environment(env, "R_removeVarFromFrame")?;
        runtime().heap.remove_binding(env, symbol)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_NewEnv(enclosure: Sexp, _hash: c_int, _size: c_int) -> Sexp {
    enter(|| {
        let enclosure = environment(enclosure, "R_NewEnv")?;
        Ok(runtime().heap.alloc_environment(enclosure)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_findFun(symbol: Sexp, env: Sexp) -> Sexp {
    enter(|| find_fun(symbol, env))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ENCLOS(env: Sexp) -> Sexp {
    enter(|| Ok(environment(env, "ENCLOS")?.enclosure()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ParentEnv(env: Sexp) -> Sexp {
    enter(|| {
        let env = environment(env, "R_ParentEnv")?;
        if env == runtime().heap.known().empty_env {
            return Err(error("the empty environment has no parent"));
        }
        Ok(env.enclosure())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_LockEnvironment(env: Sexp, bindings: Rboolean) {
    enter(|| {
        let env = environment(env, "lockEnvironment")?;
        let runtime = runtime();
        if bindings != 0 {
            for symbol in runtime.heap.frame_symbols(env) {
                if let Some(cell) = runtime.heap.frame_cell(env, symbol) {
                    cell.set_gp_bits(gp::BINDING_LOCKED, true);
                }
            }
        }
        env.set_gp_bits(gp::FRAME_LOCKED, true);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_EnvironmentIsLocked(env: Sexp) -> Rboolean {
    enter(|| Ok(environment(env, "environmentIsLocked")?.has_gp(gp::FRAME_LOCKED) as Rboolean))
}

unsafe fn binding(symbol: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let env = environment(env, "bindingIsLocked")?;
    runtime()
        .heap
        .frame_cell(env, symbol)
        .ok_or_else(|| error(format!("no binding for \"{}\"", symbol_name(symbol))))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_LockBinding(symbol: Sexp, env: Sexp) {
    enter(|| {
        binding(symbol, env)?.set_gp_bits(gp::BINDING_LOCKED, true);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_unLockBinding(symbol: Sexp, env: Sexp) {
    enter(|| {
        binding(symbol, env)?.set_gp_bits(gp::BINDING_LOCKED, false);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_BindingIsLocked(symbol: Sexp, env: Sexp) -> Rboolean {
    enter(|| Ok(binding(symbol, env)?.has_gp(gp::BINDING_LOCKED) as Rboolean))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_BindingIsActive(symbol: Sexp, env: Sexp) -> Rboolean {
    enter(|| Ok(binding(symbol, env)?.has_gp(gp::ACTIVE_BINDING) as Rboolean))
}

unsafe fn ls(env: Sexp, all: bool, sorted: bool) -> ApiResult<Sexp> {
    let env = environment(env, "ls")?;
    let runtime = runtime();
    let mut names: Vec<Sexp> = runtime
        .heap
        .frame_symbols(env)
        .into_iter()
        .map(|symbol| symbol.car())
        .filter(|name| all || !name.bytes().starts_with(b"."))
        .collect();
    if sorted {
        names.sort_by(|left, right| left.bytes().cmp(right.bytes()));
    }
    let result = runtime
        .heap
        .alloc_vector(SexpType::String, names.len() as isize)?;
    for (index, name) in names.into_iter().enumerate() {
        runtime.heap.set_element(result, index, name);
    }
    Ok(result)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_lsInternal(env: Sexp, all: Rboolean) -> Sexp {
    enter(|| ls(env, all != 0, true))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_lsInternal3(env: Sexp, all: Rboolean, sorted: Rboolean) -> Sexp {
    enter(|| ls(env, all != 0, sorted != 0))
}

/// `R_FindNamespace`: the namespace registered under the name in `info`. Loading a namespace
/// that is not registered needs the interpreter, so it is reported as missing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_FindNamespace(info: Sexp) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let known = *runtime.heap.known();
        if info.kind() != SexpType::String || crate::vectors::xlength(info)? < 1 {
            return Err(error("bad namespace name"));
        }
        let name = crate::vectors::sexp_elt(info, 0)?;
        if name.bytes() == b"base" {
            return Ok(known.base_namespace);
        }
        let symbol = runtime.heap.install_char(name)?;
        let namespace = frame_value(known.namespace_registry, symbol);
        if namespace.kind() == SexpType::Environment {
            return Ok(namespace);
        }
        Err(error(format!(
            "there is no package called \u{2018}{}\u{2019}",
            utf8_text(name)?
        )))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_IsNamespaceEnv(env: Sexp) -> Rboolean {
    enter(|| {
        let runtime = runtime();
        let known = *runtime.heap.known();
        if env == known.base_namespace {
            return Ok(1);
        }
        if env.kind() != SexpType::Environment {
            return Ok(0);
        }
        let info = frame_value(env, crate::globals::R_NamespaceEnvSymbol.get());
        Ok((info.kind() == SexpType::Environment) as Rboolean)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_IsPackageEnv(env: Sexp) -> Rboolean {
    enter(|| {
        if env.kind() != SexpType::Environment {
            return Ok(0);
        }
        let name = crate::attributes::raw_attribute(env, crate::globals::R_NameSymbol.get());
        Ok((name.kind() == SexpType::String
            && name.length() > 0
            && name.element(0).bytes().starts_with(b"package:")) as Rboolean)
    })
}

/// `Rf_GetOption1`: the value of an option, or `NULL`. Options live in an environment the
/// runtime owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetOption1(tag: Sexp) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let value = frame_value(runtime.options, tag);
        if value == runtime.heap.known().unbound {
            Ok(runtime.nil())
        } else {
            Ok(value)
        }
    })
}

/// `R_BindingType_t`, with GNU R's numbering.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingType {
    Unbound = 0,
    Value = 1,
    Missing = 2,
    Delayed = 3,
    Forced = 4,
    Active = 5,
}

/// `R_DotType_t`, with GNU R's numbering.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DotType {
    Value = 0,
    Missing = 1,
    Delayed = 2,
    Forced = 3,
}

/// A promise whose value is a promise again (as `delayedAssign` of a promise produces) is
/// classified and read through to the innermost one.
unsafe fn innermost_promise(mut promise: Sexp) -> Sexp {
    let unbound = runtime().heap.known().unbound;
    while promise.car() != unbound && promise.car().kind() == SexpType::Promise {
        promise = promise.car();
    }
    promise
}

unsafe fn promise_is_forced(promise: Sexp) -> bool {
    innermost_promise(promise).car() != runtime().heap.known().unbound
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetBindingType(symbol: Sexp, env: Sexp) -> BindingType {
    enter(|| {
        let env = environment(env, "R_GetBindingType")?;
        let Some(cell) = runtime().heap.frame_cell(env, symbol) else {
            return Ok(BindingType::Unbound);
        };
        if cell.has_gp(gp::ACTIVE_BINDING) {
            return Ok(BindingType::Active);
        }
        let value = cell.cdr();
        Ok(if value == runtime().heap.known().missing_arg {
            BindingType::Missing
        } else if value.kind() == SexpType::Promise {
            if promise_is_forced(value) {
                BindingType::Forced
            } else {
                BindingType::Delayed
            }
        } else {
            BindingType::Value
        })
    })
}

impl crate::boundary::Handout for BindingType {}
impl crate::boundary::Handout for DotType {}

unsafe fn binding_of_type(
    symbol: Sexp,
    env: Sexp,
    wanted: &[BindingType],
    what: &str,
) -> ApiResult<Sexp> {
    let cell = binding(symbol, env)?;
    let kind = R_GetBindingType(symbol, env);
    if !wanted.contains(&kind) {
        return Err(error(format!(
            "{what}: the binding of \"{}\" is not of the right type",
            symbol_name(symbol)
        )));
    }
    Ok(cell.cdr())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ActiveBindingFunction(symbol: Sexp, env: Sexp) -> Sexp {
    enter(|| {
        binding_of_type(
            symbol,
            env,
            &[BindingType::Active],
            "R_ActiveBindingFunction",
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DelayedBindingExpression(symbol: Sexp, env: Sexp) -> Sexp {
    enter(|| {
        let promise = binding_of_type(
            symbol,
            env,
            &[BindingType::Delayed],
            "R_DelayedBindingExpression",
        )?;
        Ok(innermost_promise(promise).cdr())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DelayedBindingEnvironment(symbol: Sexp, env: Sexp) -> Sexp {
    enter(|| {
        let promise = binding_of_type(
            symbol,
            env,
            &[BindingType::Delayed],
            "R_DelayedBindingEnvironment",
        )?;
        Ok(innermost_promise(promise).tag())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ForcedBindingExpression(symbol: Sexp, env: Sexp) -> Sexp {
    enter(|| {
        let promise = binding_of_type(
            symbol,
            env,
            &[BindingType::Forced],
            "R_ForcedBindingExpression",
        )?;
        Ok(innermost_promise(promise).cdr())
    })
}

unsafe fn checked_symbol(symbol: Sexp, what: &str) -> ApiResult<Sexp> {
    if symbol.kind() != SexpType::Symbol {
        return Err(error(format!("{what}: not a symbol")));
    }
    Ok(symbol)
}

/// Binds `symbol` in `env`'s frame to exactly `value`, replacing any active binding, as the
/// binding constructors below do.
unsafe fn rebind(symbol: Sexp, value: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    runtime.heap.root(value);
    if let Some(cell) = runtime.heap.frame_cell(env, symbol) {
        if cell.has_gp(gp::BINDING_LOCKED) {
            return Err(error(format!(
                "cannot change value of locked binding for '{}'",
                symbol_name(symbol)
            )));
        }
        cell.set_gp_bits(gp::ACTIVE_BINDING, false);
        runtime.heap.set_binding_value(cell, value)?;
        return Ok(cell);
    }
    runtime.heap.define(env, symbol, value)?;
    runtime
        .heap
        .frame_cell(env, symbol)
        .ok_or_else(|| error("a new binding was not recorded"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeActiveBinding(symbol: Sexp, function: Sexp, env: Sexp) {
    enter(|| {
        checked_symbol(symbol, "R_MakeActiveBinding")?;
        if function.kind() != SexpType::Closure
            && function.kind() != SexpType::Builtin
            && function.kind() != SexpType::Special
        {
            return Err(error("not a function"));
        }
        let env = environment(env, "makeActiveBinding")?;
        if let Some(cell) = runtime().heap.frame_cell(env, symbol)
            && !cell.has_gp(gp::ACTIVE_BINDING)
        {
            return Err(error(format!(
                "symbol already has a regular binding"
            )));
        }
        let cell = rebind(symbol, function, env)?;
        cell.set_gp_bits(gp::ACTIVE_BINDING, true);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeDelayedBinding(
    symbol: Sexp,
    expression: Sexp,
    eval_env: Sexp,
    env: Sexp,
) {
    enter(|| {
        checked_symbol(symbol, "R_MakeDelayedBinding")?;
        let env = environment(env, "R_MakeDelayedBinding")?;
        let eval_env = environment(eval_env, "R_MakeDelayedBinding")?;
        let unbound = runtime().heap.known().unbound;
        let promise =
            runtime()
                .heap
                .alloc_triple(SexpType::Promise, unbound, expression, eval_env)?;
        rebind(symbol, promise, env)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeForcedBinding(symbol: Sexp, expression: Sexp, value: Sexp, env: Sexp) {
    enter(|| {
        checked_symbol(symbol, "R_MakeForcedBinding")?;
        let env = environment(env, "R_MakeForcedBinding")?;
        let nil = runtime().nil();
        let promise = runtime()
            .heap
            .alloc_triple(SexpType::Promise, value, expression, nil)?;
        rebind(symbol, promise, env)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeMissingBinding(symbol: Sexp, env: Sexp) {
    enter(|| {
        checked_symbol(symbol, "R_MakeMissingBinding")?;
        let env = environment(env, "R_MakeMissingBinding")?;
        let missing = runtime().heap.known().missing_arg;
        rebind(symbol, missing, env)?;
        Ok(())
    })
}

/// `...` bound in `env`'s own frame: a `DOTSXP` list, or `R_MissingArg` when the call passed no
/// dots, or `None` when `env` has no `...` at all.
unsafe fn dots_in_frame(env: Sexp) -> ApiResult<Option<Sexp>> {
    let env = environment(env, "dots")?;
    let value = frame_value(env, crate::globals::R_DotsSymbol.get());
    let known = *runtime().heap.known();
    if value == known.unbound {
        return Ok(None);
    }
    if value == known.missing_arg || value.kind() == SexpType::Dots {
        return Ok(Some(value));
    }
    Err(error("bad ... value"))
}

unsafe fn dots_required(env: Sexp) -> ApiResult<Sexp> {
    dots_in_frame(env)?
        .ok_or_else(|| error("incorrect context: the current call has no '...' to look in"))
}

unsafe fn dots_length(dots: Sexp) -> usize {
    let mut count = 0;
    let mut cell = dots;
    while cell.kind() == SexpType::Dots {
        count += 1;
        cell = cell.cdr();
    }
    count
}

/// The `i`th (from one) element of `...` in `env`'s frame.
unsafe fn dot(index: c_int, env: Sexp) -> ApiResult<Sexp> {
    if index <= 0 {
        return Err(error(format!(
            "indexing '...' with non-positive index {index}"
        )));
    }
    let dots = dots_required(env)?;
    let mut cell = dots;
    let mut position = 1;
    while cell.kind() == SexpType::Dots {
        if position == index {
            return Ok(cell.car());
        }
        position += 1;
        cell = cell.cdr();
    }
    let count = dots_length(dots);
    Err(error(if count == 1 {
        format!("the ... list contains fewer than {index} element")
    } else {
        format!("the ... list contains fewer than {index} elements")
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_findDotsEnv(env: Sexp) -> Sexp {
    enter(|| {
        let empty = runtime().heap.known().empty_env;
        let mut env = environment(env, "R_findDotsEnv")?;
        while env != empty {
            if dots_in_frame(env)?.is_some() {
                return Ok(env);
            }
            env = env.enclosure();
        }
        Ok(empty)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotsExist(env: Sexp) -> Rboolean {
    enter(|| Ok(dots_in_frame(env)?.is_some() as Rboolean))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotsLength(env: Sexp) -> c_int {
    enter(|| Ok(dots_length(dots_required(env)?) as c_int))
}

/// The names of `...`, with `""` for unnamed elements, or `NULL` when none is named.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotsNames(env: Sexp) -> Sexp {
    enter(|| {
        let dots = dots_required(env)?;
        let runtime = runtime();
        let count = dots_length(dots);
        let mut cell = dots;
        let mut names = runtime.nil();
        for index in 0..count {
            if cell.tag().kind() == SexpType::Symbol {
                if names == runtime.nil() {
                    names = runtime.heap.alloc_vector(SexpType::String, count as isize)?;
                    runtime.heap.root(names);
                    let blank = runtime.heap.mk_char(b"", heap::strings::Encoding::Native)?;
                    for position in 0..count {
                        runtime.heap.set_element(names, position, blank);
                    }
                }
                runtime.heap.set_element(names, index, cell.tag().car());
            }
            cell = cell.cdr();
        }
        Ok(names)
    })
}

/// `R_DotsElt(i, env)`: the `i`th element of `...`, evaluated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotsElt(index: c_int, env: Sexp) -> Sexp {
    enter(|| {
        let value = dot(index, env)?;
        if value.kind() == SexpType::Promise || value == runtime().heap.known().missing_arg {
            return crate::eval::eval(value, env);
        }
        Ok(value)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetDotType(index: c_int, env: Sexp) -> DotType {
    enter(|| {
        let value = dot(index, env)?;
        Ok(if value == runtime().heap.known().missing_arg {
            DotType::Missing
        } else if value.kind() == SexpType::Promise {
            if promise_is_forced(value) {
                DotType::Forced
            } else {
                DotType::Delayed
            }
        } else {
            DotType::Value
        })
    })
}

unsafe fn dot_promise(index: c_int, env: Sexp, wanted: DotType, what: &str) -> ApiResult<Sexp> {
    let value = dot(index, env)?;
    if R_GetDotType(index, env) != wanted {
        return Err(error(format!("{what}: the ... element is not of the right type")));
    }
    Ok(innermost_promise(value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotDelayedExpression(index: c_int, env: Sexp) -> Sexp {
    enter(|| Ok(dot_promise(index, env, DotType::Delayed, "R_DotDelayedExpression")?.cdr()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotDelayedEnvironment(index: c_int, env: Sexp) -> Sexp {
    enter(|| Ok(dot_promise(index, env, DotType::Delayed, "R_DotDelayedEnvironment")?.tag()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_DotForcedExpression(index: c_int, env: Sexp) -> Sexp {
    enter(|| Ok(dot_promise(index, env, DotType::Forced, "R_DotForcedExpression")?.cdr()))
}

/// `R_envSymbols(env)`: the symbols bound in `env`'s frame, as a list, in frame order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_envSymbols(env: Sexp) -> Sexp {
    enter(|| {
        let env = environment(env, "R_envSymbols")?;
        let runtime = runtime();
        let symbols = runtime.heap.frame_symbols(env);
        let result = runtime
            .heap
            .alloc_vector(SexpType::List, symbols.len() as isize)?;
        for (index, symbol) in symbols.into_iter().enumerate() {
            runtime.heap.set_element(result, index, symbol);
        }
        Ok(result)
    })
}

/// `Rf_topenv(target, env)`: the first enclosing environment of `env` that is `target`, the
/// global environment, the base environment, a namespace, a package environment or one marked
/// with `.packageName`, which is where top-level definitions made from `env` land.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_topenv(target: Sexp, env: Sexp) -> Sexp {
    enter(|| {
        let known = *runtime().heap.known();
        let mut env = env;
        while env.kind() == SexpType::Environment && env != known.empty_env {
            if env == target
                || env == known.global_env
                || env == known.base_env
                || env == known.base_namespace
                || R_IsPackageEnv(env) != 0
                || R_IsNamespaceEnv(env) != 0
                || runtime()
                    .heap
                    .frame_cell(env, crate::globals::R_dot_packageName.get())
                    .is_some()
            {
                return Ok(env);
            }
            env = env.enclosure();
        }
        Ok(known.global_env)
    })
}

/// `R_NamespaceEnvSpec(rho)`: the `c(name = , version = )` spec of a namespace, or `NULL`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_NamespaceEnvSpec(env: Sexp) -> Sexp {
    enter(|| {
        let known = *runtime().heap.known();
        if env == known.base_namespace {
            let spec = runtime().heap.alloc_vector(SexpType::String, 2)?;
            runtime().heap.root(spec);
            let name = runtime().heap.mk_char(b"base", heap::strings::Encoding::Native)?;
            runtime().heap.set_element(spec, 0, name);
            let version = runtime()
                .heap
                .mk_char(crate::R_VERSION_STRING.as_bytes(), heap::strings::Encoding::Native)?;
            runtime().heap.set_element(spec, 1, version);
            return Ok(spec);
        }
        if env.kind() != SexpType::Environment {
            return Ok(known.nil);
        }
        let info = frame_value(env, crate::globals::R_NamespaceEnvSymbol.get());
        if info.kind() != SexpType::Environment {
            return Ok(known.nil);
        }
        let spec_symbol = runtime().heap.install(b"spec")?;
        let spec = frame_value(info, spec_symbol);
        Ok(if spec.kind() == SexpType::String && spec.length() > 0 {
            spec
        } else {
            known.nil
        })
    })
}

/// `R_PackageEnvName(rho)`: the `name` attribute of a package environment (`"package:foo"`),
/// or `NULL`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_PackageEnvName(env: Sexp) -> Sexp {
    enter(|| {
        if R_IsPackageEnv(env) != 0 {
            Ok(crate::attributes::raw_attribute(
                env,
                crate::globals::R_NameSymbol.get(),
            ))
        } else {
            Ok(runtime().nil())
        }
    })
}

/// `R_getRegisteredNamespace(name)`: the loaded namespace called `name` (a C string), or `NULL`
/// when it is not loaded. Unlike `R_FindNamespace`, it never loads anything.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getRegisteredNamespace(name: *const std::ffi::c_char) -> Sexp {
    enter(|| registered_namespace(crate::strings::c_bytes(name)))
}

pub unsafe fn registered_namespace(name: &[u8]) -> ApiResult<Sexp> {
    let known = *runtime().heap.known();
    if name == b"base" {
        return Ok(known.base_namespace);
    }
    let symbol = runtime().heap.install(name)?;
    let namespace = frame_value(known.namespace_registry, symbol);
    Ok(if namespace.kind() == SexpType::Environment {
        namespace
    } else {
        known.nil
    })
}

/// `Rf_GetOption(tag, rho)`, the older spelling of `Rf_GetOption1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetOption(tag: Sexp, _env: Sexp) -> Sexp {
    Rf_GetOption1(tag)
}

/// `Rf_GetOptionWidth()`: the `width` option, or 80.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetOptionWidth() -> c_int {
    enter(|| option_integer(b"width", 80))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GetOptionDigits() -> c_int {
    enter(|| option_integer(b"digits", 7))
}

unsafe fn option_integer(name: &[u8], default: c_int) -> ApiResult<c_int> {
    let symbol = runtime().heap.install(name)?;
    let value = frame_value(runtime().options, symbol);
    if value == runtime().heap.known().unbound || value.kind() == SexpType::Nil {
        return Ok(default);
    }
    let value = crate::vectors::as_integer(value)?;
    Ok(if value == heap::object::NA_INTEGER {
        default
    } else {
        value
    })
}
