//! External pointers, weak references and finalizers.

use crate::{
    Rboolean,
    boundary::{ApiResult, DlFunc, enter, error, run_finalizer},
    runtime,
};
use heap::object::{Sexp, SexpType};
use std::ffi::c_void;

unsafe fn external_ptr(object: Sexp, accessor: &str) -> ApiResult<Sexp> {
    if object.kind() != SexpType::ExternalPtr {
        return Err(error(format!(
            "{accessor}: argument is a '{}', not an external pointer",
            object.kind().name()
        )));
    }
    Ok(object)
}

unsafe fn make_weak_ref(
    key: Sexp,
    value: Sexp,
    finalizer: Sexp,
    c_finalizer: usize,
    on_exit: Rboolean,
) -> ApiResult<Sexp> {
    if !matches!(
        key.kind(),
        SexpType::Nil | SexpType::Environment | SexpType::ExternalPtr | SexpType::Bytecode
    ) {
        return Err(error(
            "can only weakly reference/finalize reference objects",
        ));
    }
    if !matches!(
        finalizer.kind(),
        SexpType::Nil | SexpType::Closure | SexpType::Builtin | SexpType::Special
    ) {
        return Err(error("finalizer must be a function or NULL"));
    }
    Ok(runtime()
        .heap
        .alloc_weak_ref(key, value, finalizer, c_finalizer, on_exit != 0)?)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeExternalPtr(
    address: *mut c_void,
    tag: Sexp,
    protected: Sexp,
) -> Sexp {
    enter(|| Ok(runtime().heap.alloc_external_ptr(address, tag, protected)?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ExternalPtrAddr(object: Sexp) -> *mut c_void {
    enter(|| Ok(external_ptr(object, "R_ExternalPtrAddr")?.external_address()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ExternalPtrTag(object: Sexp) -> Sexp {
    enter(|| Ok(external_ptr(object, "R_ExternalPtrTag")?.external_tag()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ExternalPtrProtected(object: Sexp) -> Sexp {
    enter(|| Ok(external_ptr(object, "R_ExternalPtrProtected")?.external_protected()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ClearExternalPtr(object: Sexp) {
    enter(|| {
        external_ptr(object, "R_ClearExternalPtr")?.set_external_address(std::ptr::null_mut());
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_SetExternalPtrAddr(object: Sexp, address: *mut c_void) {
    enter(|| {
        external_ptr(object, "R_SetExternalPtrAddr")?.set_external_address(address);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_SetExternalPtrTag(object: Sexp, tag: Sexp) {
    enter(|| {
        let object = external_ptr(object, "R_SetExternalPtrTag")?;
        runtime().heap.set_external_tag(object, tag);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_SetExternalPtrProtected(object: Sexp, protected: Sexp) {
    enter(|| {
        let object = external_ptr(object, "R_SetExternalPtrProtected")?;
        runtime().heap.set_external_protected(object, protected);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeExternalPtrFn(
    function: Option<DlFunc>,
    tag: Sexp,
    protected: Sexp,
) -> Sexp {
    let address = function.map_or(std::ptr::null_mut(), |function| function as *mut c_void);
    R_MakeExternalPtr(address, tag, protected)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ExternalPtrAddrFn(object: Sexp) -> Option<DlFunc> {
    let address = R_ExternalPtrAddr(object);
    if address.is_null() {
        None
    } else {
        Some(std::mem::transmute::<*mut c_void, DlFunc>(address))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeWeakRef(
    key: Sexp,
    value: Sexp,
    finalizer: Sexp,
    on_exit: Rboolean,
) -> Sexp {
    enter(|| make_weak_ref(key, value, finalizer, 0, on_exit))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_MakeWeakRefC(
    key: Sexp,
    value: Sexp,
    finalizer: Option<unsafe extern "C" fn(Sexp)>,
    on_exit: Rboolean,
) -> Sexp {
    enter(|| {
        let nil = runtime().nil();
        make_weak_ref(
            key,
            value,
            nil,
            finalizer.map_or(0, |finalizer| finalizer as usize),
            on_exit,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RegisterFinalizer(object: Sexp, finalizer: Sexp) {
    R_RegisterFinalizerEx(object, finalizer, 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RegisterFinalizerEx(object: Sexp, finalizer: Sexp, on_exit: Rboolean) {
    enter(|| {
        let nil = runtime().nil();
        make_weak_ref(object, nil, finalizer, 0, on_exit)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RegisterCFinalizer(
    object: Sexp,
    finalizer: Option<unsafe extern "C" fn(Sexp)>,
) {
    R_RegisterCFinalizerEx(object, finalizer, 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RegisterCFinalizerEx(
    object: Sexp,
    finalizer: Option<unsafe extern "C" fn(Sexp)>,
    on_exit: Rboolean,
) {
    enter(|| {
        let nil = runtime().nil();
        make_weak_ref(
            object,
            nil,
            nil,
            finalizer.map_or(0, |finalizer| finalizer as usize),
            on_exit,
        )?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_WeakRefKey(weak: Sexp) -> Sexp {
    enter(|| {
        if weak.kind() != SexpType::WeakRef {
            return Err(error("not a weak reference"));
        }
        Ok(weak.weak_key())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_WeakRefValue(weak: Sexp) -> Sexp {
    enter(|| {
        if weak.kind() != SexpType::WeakRef {
            return Err(error("not a weak reference"));
        }
        let value = weak.weak_value();
        if value != runtime().nil() {
            value.mark_not_mutable();
        }
        Ok(value)
    })
}

/// Runs a weak reference's finalizer now, once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RunWeakRefFinalizer(weak: Sexp) {
    if weak.kind() == SexpType::WeakRef {
        run_finalizer(weak);
    }
}

/// Runs the finalizers registered with `onexit = TRUE`, as R does when the session ends.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RunExitFinalizers() {
    let exiting = runtime().heap.take_exit_finalizers();
    let mark = runtime().heap.handle_mark();
    crate::boundary::root_finalizable(&exiting);
    for weak in exiting {
        run_finalizer(weak);
    }
    runtime().heap.release_handles(mark);
}
