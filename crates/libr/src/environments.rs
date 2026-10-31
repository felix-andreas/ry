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

/// `Rf_findVar`: the first binding of `symbol` from `env` outward, or `R_UnboundValue`.
pub unsafe fn find_var(symbol: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let empty = runtime.heap.known().empty_env;
    let unbound = runtime.heap.known().unbound;
    let mut env = environment(env, "findVar")?;
    while env != empty {
        let value = frame_value(env, symbol);
        if value != unbound {
            return Ok(value);
        }
        env = env.enclosure();
    }
    Ok(unbound)
}

pub unsafe fn define_var(symbol: Sexp, value: Sexp, env: Sexp) -> ApiResult<()> {
    let env = environment(env, "defineVar")?;
    if symbol.kind() != SexpType::Symbol {
        return Err(error("invalid symbol for defineVar"));
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
        let mut value = frame_value(env, symbol);
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
        frame_value(env, symbol)
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
                return runtime
                    .heap
                    .set_binding_value(cell, value)
                    .map_err(Into::into);
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
    enter(|| Ok(frame_value(environment(env, "findVarInFrame")?, symbol)))
}

/// With `do_get` false GNU R only checks for existence; the value is cheap to return either way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_findVarInFrame3(env: Sexp, symbol: Sexp, _do_get: Rboolean) -> Sexp {
    enter(|| Ok(frame_value(environment(env, "findVarInFrame3")?, symbol)))
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
        let name = info.element(0);
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
