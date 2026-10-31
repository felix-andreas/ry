//! The S4 part of the public C API (`R_do_slot`, `R_do_new_object`, ...), which compiled code
//! uses to work with S4 objects.

use crate::{
    boundary::{ApiResult, enter, error},
    runtime,
};
use heap::object::{Sexp, SexpType, gp};
use interp::{
    eval, interp, subset,
    values::{self, nil},
};
use std::ffi::{CStr, c_char, c_int};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_slot(object: Sexp, name: Sexp) -> Sexp {
    enter(|| {
        let symbol = slot_symbol(name)?;
        Ok(subset::get_slot(object, symbol, nil())?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_slot_assign(object: Sexp, name: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        let symbol = slot_symbol(name)?;
        Ok(subset::set_slot(object, symbol, value)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_has_slot(object: Sexp, name: Sexp) -> c_int {
    enter(|| {
        let symbol = slot_symbol(name)?;
        if values::symbol_name(symbol) == ".Data" && object.kind() != SexpType::Object {
            return Ok(1);
        }
        Ok((values::get_attr(object, symbol) != nil()) as c_int)
    })
}

/// The class definition of `what`, from the methods package's `getClass`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_MAKE_CLASS(what: *const c_char) -> Sexp {
    enter(|| {
        if what.is_null() {
            return Err(error("C level MAKE_CLASS macro called with NULL string pointer"));
        }
        let name = CStr::from_ptr(what).to_string_lossy();
        let get_class = values::install("getClass")?;
        let call = values::call(get_class, &[values::scalar_string(&name)?])?;
        Ok(eval::eval(call, methods_namespace()?)?)
    })
}

/// A new object of the class `class_def` defines: a copy of its prototype, marked as S4 when
/// the prototype is a plain S4 object or the class comes from a package.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_new_object(class_def: Sexp) -> Sexp {
    enter(|| {
        if class_def == nil() {
            return Err(error("C level NEW macro called with null class definition pointer"));
        }
        let virtual_flag = subset::get_slot(class_def, values::install("virtual")?, nil())?;
        let class_name = subset::get_slot(class_def, values::install("className")?, nil())?;
        if values::as_logical_scalar(virtual_flag) != 0 {
            return Err(error(format!(
                "cannot allocate an object of a virtual class (\"{}\")",
                values::first_string(class_name).unwrap_or_default()
            )));
        }
        let prototype = subset::get_slot(class_def, values::install("prototype")?, nil())?;
        let value = values::duplicate(prototype)?;
        let package = values::get_attr(class_name, values::install("package")?);
        if value.kind() == SexpType::Object || package != nil() {
            values::set_attr(value, values::symbols().class, class_name)?;
            value.set_gp_bits(gp::S4_OBJECT, true);
        }
        Ok(value)
    })
}

unsafe fn slot_symbol(name: Sexp) -> ApiResult<Sexp> {
    match name.kind() {
        SexpType::Symbol => Ok(name),
        SexpType::String if values::length(name) == 1 => {
            let text = values::string_at(name, 0).unwrap_or_default().into_owned();
            Ok(values::install(&text)?)
        }
        _ => Err(error("invalid type or length for slot name")),
    }
}

unsafe fn methods_namespace() -> ApiResult<Sexp> {
    let namespace = interp().globals.methods_namespace;
    if namespace != nil() {
        return Ok(namespace);
    }
    let registry = runtime().heap.known().namespace_registry;
    let methods = eval::find_var_in_frame(values::install("methods")?, registry)?;
    if methods.kind() == SexpType::Environment {
        Ok(methods)
    } else {
        Err(error("the methods package is not loaded"))
    }
}
