//! The methods package's native routines: S4 dispatch, slots and class support.

use crate::{
    builtin_libraries::NativeFn,
    unwind::{R, error},
};
use heap::object::Sexp;

pub const ROUTINES: &[(&str, NativeFn)] = &[];

/// `R_dispatchGeneric`: selects and runs the method of generic `generic` named `name` for the
/// arguments in the generic's frame `env`.
pub unsafe fn dispatch_generic(name: Sexp, env: Sexp, generic: Sexp) -> R<Sexp> {
    let _ = (name, env, generic);
    Err(error("S4 dispatch is not implemented in ry yet"))
}

/// `R_quick_method_check`: the cached method for `arguments` in `methods`, or `NULL`.
pub unsafe fn quick_method_check(arguments: Sexp, methods: Sexp, generic: Sexp) -> R<Sexp> {
    let _ = (arguments, methods, generic);
    Ok(crate::values::nil())
}
