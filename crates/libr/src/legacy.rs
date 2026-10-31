//! Names R 4.6 no longer declares, which package binaries built against older R still import:
//! the true length and growable bit, frame and hash-table access, the closure and environment
//! setters, and the debugging flags. A binary that imports a missing name does not load at all,
//! so these exist for prebuilt packages, with the behavior they had.
//!
//! `TRUELENGTH` is the allocated capacity of a vector whose length was shrunk below it, which is
//! how data.table over-allocates its column lists. Code also uses it as a scratch integer on
//! strings and on vectors (data.table's grouping marks each `CHARSXP` this way and restores the
//! marks afterwards); such values live in `Runtime::truelengths`.

use crate::{
    R_xlen_t, SEXPTYPE,
    boundary::{ApiResult, enter, error},
    runtime,
    vectors::sexp_type,
};
use heap::object::{Sexp, SexpType};
use std::ffi::c_int;

unsafe fn vector_or_string(object: Sexp, what: &str) -> ApiResult<Sexp> {
    if object.kind().is_vector() || object.kind() == SexpType::Char {
        Ok(object)
    } else {
        Err(error(format!(
            "{what}() can only be applied to a vector, not a '{}'",
            object.kind().name()
        )))
    }
}

pub unsafe fn truelength(object: Sexp) -> ApiResult<R_xlen_t> {
    let object = vector_or_string(object, "TRUELENGTH")?;
    if let Some(&value) = runtime().truelengths.get(&object) {
        return Ok(value);
    }
    if object.kind() == SexpType::Char || object.is_altrep() {
        return Ok(0);
    }
    let capacity = object.capacity();
    Ok(if runtime().heap.is_resizable(object) || capacity > object.length() {
        capacity
    } else {
        0
    })
}

pub unsafe fn set_truelength(object: Sexp, value: R_xlen_t) -> ApiResult<()> {
    let object = vector_or_string(object, "SET_TRUELENGTH")?;
    let runtime = runtime();
    let matches_storage = object.kind() != SexpType::Char
        && !object.is_altrep()
        && (value == object.capacity() || (value == 0 && object.capacity() == object.length()));
    if value == 0 || matches_storage {
        if runtime.truelengths.remove(&object).is_some() {
            runtime.heap.release(object);
        }
        return Ok(());
    }
    if runtime.truelengths.insert(object, value).is_none() {
        runtime.heap.preserve(object);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TRUELENGTH(object: Sexp) -> R_xlen_t {
    enter(|| truelength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn XTRUELENGTH(object: Sexp) -> R_xlen_t {
    enter(|| truelength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_TRUELENGTH(object: Sexp, value: R_xlen_t) {
    enter(|| set_truelength(object, value))
}

/// `SETLENGTH(x, n)`: changes the length of a vector within the length it was allocated with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SETLENGTH(object: Sexp, length: R_xlen_t) {
    enter(|| {
        if !object.kind().is_vector() || object.kind() == SexpType::Char || object.is_altrep() {
            return Err(error(format!(
                "SETLENGTH() cannot be applied to a '{}'",
                object.kind().name()
            )));
        }
        let runtime = runtime();
        runtime.heap.mark_resizable(object);
        runtime.heap.resize_vector(object, length)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_GROWABLE_BIT(object: Sexp) {
    enter(|| {
        if object.kind().is_vector() && object.kind() != SexpType::Char && !object.is_altrep() {
            runtime().heap.mark_resizable(object);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_GROWABLE(object: Sexp) -> c_int {
    enter(|| {
        Ok((object.kind().is_vector()
            && runtime().heap.is_resizable(object)
            && object.capacity() > object.length()) as c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_TYPEOF(object: Sexp, kind: SEXPTYPE) {
    enter(|| {
        let kind = sexp_type(kind)?;
        runtime().heap.retype(object, kind)?;
        Ok(())
    })
}

unsafe fn checked(object: Sexp, kind: SexpType, what: &str) -> ApiResult<Sexp> {
    if object.kind() != kind {
        return Err(error(format!(
            "{what}() can only be applied to a '{}', not a '{}'",
            kind.name(),
            object.kind().name()
        )));
    }
    Ok(object)
}

/// `FRAME(env)`: the frame as a pairlist of values tagged with their symbols. ry's frames are
/// hashed, so this is a snapshot: reading it works, and changes to it do not reach the frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FRAME(env: Sexp) -> Sexp {
    enter(|| {
        let env = checked(env, SexpType::Environment, "FRAME")?;
        let runtime = runtime();
        let mut list = runtime.nil();
        for symbol in runtime.heap.frame_symbols(env).into_iter().rev() {
            let Some(cell) = runtime.heap.frame_cell(env, symbol) else {
                continue;
            };
            list = runtime
                .heap
                .alloc_triple(SexpType::Pairlist, cell.cdr(), list, symbol)?;
            runtime.heap.root(list);
            if cell.has_gp(heap::object::gp::ACTIVE_BINDING) {
                list.set_gp_bits(heap::object::gp::ACTIVE_BINDING, true);
            }
        }
        Ok(list)
    })
}

/// `HASHTAB(env)`: `NULL`, which tells code that walks frames to use `FRAME` instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HASHTAB(env: Sexp) -> Sexp {
    enter(|| {
        checked(env, SexpType::Environment, "HASHTAB")?;
        Ok(runtime().nil())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_ENCLOS(env: Sexp, enclosure: Sexp) {
    enter(|| {
        let env = checked(env, SexpType::Environment, "SET_ENCLOS")?;
        checked(enclosure, SexpType::Environment, "SET_ENCLOS")?;
        if env == runtime().heap.known().empty_env {
            return Err(error("can not set the parent of the empty environment"));
        }
        runtime().heap.set_enclosure(env, enclosure);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_FORMALS(closure: Sexp, formals: Sexp) {
    enter(|| {
        runtime()
            .heap
            .set_car(checked(closure, SexpType::Closure, "SET_FORMALS")?, formals);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_BODY(closure: Sexp, body: Sexp) {
    enter(|| {
        runtime()
            .heap
            .set_cdr(checked(closure, SexpType::Closure, "SET_BODY")?, body);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_CLOENV(closure: Sexp, env: Sexp) {
    enter(|| {
        checked(env, SexpType::Environment, "SET_CLOENV")?;
        runtime()
            .heap
            .set_tag(checked(closure, SexpType::Closure, "SET_CLOENV")?, env);
        Ok(())
    })
}

/// The debugging flags live in general-purpose bit 15 of a closure, as in GNU R.
const DEBUG_BIT: u16 = 1 << 15;
/// The single-step flag, general-purpose bit 14.
const STEP_BIT: u16 = 1 << 14;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RDEBUG(object: Sexp) -> c_int {
    object.has_gp(DEBUG_BIT) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_RDEBUG(object: Sexp, value: c_int) {
    object.set_gp_bits(DEBUG_BIT, value != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RSTEP(object: Sexp) -> c_int {
    object.has_gp(STEP_BIT) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_RSTEP(object: Sexp, value: c_int) {
    object.set_gp_bits(STEP_BIT, value != 0);
}

/// `ENVFLAGS(env)`: an environment's general-purpose bits, where GNU R keeps its locked flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ENVFLAGS(env: Sexp) -> c_int {
    env.gp() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_ENVFLAGS(env: Sexp, flags: c_int) {
    env.set_gp(flags as u16);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EXTPTR_PTR(pointer: Sexp) -> *mut std::ffi::c_void {
    enter(|| Ok(checked(pointer, SexpType::ExternalPtr, "EXTPTR_PTR")?.external_address()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EXTPTR_TAG(pointer: Sexp) -> Sexp {
    enter(|| Ok(checked(pointer, SexpType::ExternalPtr, "EXTPTR_TAG")?.external_tag()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EXTPTR_PROT(pointer: Sexp) -> Sexp {
    enter(|| Ok(checked(pointer, SexpType::ExternalPtr, "EXTPTR_PROT")?.external_protected()))
}

/// `SYMVALUE(symbol)`: the symbol's binding in the base environment, or `R_UnboundValue`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SYMVALUE(symbol: Sexp) -> Sexp {
    enter(|| {
        let symbol = checked(symbol, SexpType::Symbol, "SYMVALUE")?;
        let base = runtime().heap.known().base_env;
        Ok(crate::environments::frame_value(base, symbol))
    })
}

/// `INTERNAL(symbol)`: the `.Internal` function behind a symbol, which only the interpreter
/// defines, so `R_NilValue` here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn INTERNAL(symbol: Sexp) -> Sexp {
    enter(|| {
        checked(symbol, SexpType::Symbol, "INTERNAL")?;
        Ok(runtime().nil())
    })
}

/// `DDVAL(symbol)`: whether a symbol is one of `..1`, `..2`, ..., kept in general-purpose bit 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DDVAL(symbol: Sexp) -> c_int {
    let name = symbol.car().bytes();
    (name.len() > 2 && name.starts_with(b"..") && name[2..].iter().all(u8::is_ascii_digit))
        as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_MISSING(cell: Sexp, value: c_int) {
    let mask: u16 = 0x000f;
    cell.set_gp((cell.gp() & !mask) | (value as u16 & mask));
}

/// `IS_SCALAR(x, type)`: whether `x` is a length-one vector of `type`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_SCALAR(object: Sexp, kind: c_int) -> c_int {
    (object.kind() as c_int == kind && object.kind().is_vector() && object.length() == 1) as c_int
}
