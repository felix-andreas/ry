//! The S4 part of the public C API (`R_do_slot`, `R_do_new_object`, ...), which compiled code
//! uses to work with S4 objects.

use crate::boundary::{enter, error};
use heap::object::Sexp;
use interp::{methods, subset, values::nil};
use std::ffi::{CStr, c_char, c_int};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_slot(object: Sexp, name: Sexp) -> Sexp {
    enter(|| {
        let symbol = methods::slot_symbol(name)?;
        Ok(subset::get_slot(object, symbol, nil())?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_slot_assign(object: Sexp, name: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        let symbol = methods::slot_symbol(name)?;
        Ok(subset::assign_slot(object, symbol, value)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_has_slot(object: Sexp, name: Sexp) -> c_int {
    enter(|| Ok(methods::has_slot(object, name)? as c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_MAKE_CLASS(what: *const c_char) -> Sexp {
    enter(|| {
        if what.is_null() {
            return Err(error("C level MAKE_CLASS macro called with NULL string pointer"));
        }
        let name = CStr::from_ptr(what).to_string_lossy();
        Ok(methods::make_class(&name)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_new_object(class_def: Sexp) -> Sexp {
    enter(|| Ok(methods::new_object(class_def)?))
}
