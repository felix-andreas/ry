//! `.Call` into a registered native routine, the call every compiled package makes on its hot
//! paths. The routine is identified by its `NativeSymbolInfo`'s address pointer alone: a native
//! entry point is the only routine whose pointer has no tag and a non-null address, because a
//! routine of a builtin library carries a marker tag and a base routine a null address. So the
//! call reads two list slots by position and hands its arguments over from the stack, with no
//! name search, string comparison or heap allocation before native code runs.

use {
    crate::{
        attributes::raw_attribute,
        boundary::{ApiResult, DlFunc, call_native},
        globals::{R_ClassSymbol, R_NamesSymbol},
    },
    heap::object::{Sexp, SexpType},
    interp::{builtins::Call, values::symbols},
    smallvec::SmallVec,
};

/// GNU R's limit on the arguments of a `.Call` or `.C` routine.
pub const MAX_ARGUMENTS: usize = 65;

/// A routine native code implements: its entry point, and the parameter count it was
/// registered with when the registration states one.
#[derive(Clone, Copy)]
pub struct NativeRoutine {
    pub function: DlFunc,
    pub arity: Option<i32>,
}

/// Runs `.Call` directly when `.NAME` is a native routine, no argument is `PACKAGE`, and the
/// argument count fits the registration. `None` leaves the call to the general path, which
/// resolves every other routine and owns every error.
pub unsafe fn call_registered(call: &Call<'_>) -> ApiResult<Option<Sexp>> {
    let Some((first, rest)) = call.args.split_first() else {
        return Ok(None);
    };
    let Some(routine) = native_routine(first.value) else {
        return Ok(None);
    };
    if rest.len() > MAX_ARGUMENTS
        || routine
            .arity
            .is_some_and(|arity| arity >= 0 && arity as usize != rest.len())
    {
        return Ok(None);
    }
    let package = symbols().package_arg;
    let mut arguments: SmallVec<[Sexp; 8]> = SmallVec::with_capacity(rest.len());
    for arg in rest {
        if arg.tag == package {
            return Ok(None);
        }
        arguments.push(arg.value);
    }
    call_native(routine.function, &arguments, ".Call").map(Some)
}

/// The native routine `name` refers to, when it is a `NativeSymbolInfo` list or a bare address
/// pointer whose address is a native entry point.
pub unsafe fn native_routine(name: Sexp) -> Option<NativeRoutine> {
    match name.kind() {
        SexpType::List => {
            let function = native_address(symbol_address(name)?)?;
            Some(NativeRoutine {
                function,
                arity: symbol_arity(name),
            })
        }
        SexpType::ExternalPtr => Some(NativeRoutine {
            function: native_address(name)?,
            arity: None,
        }),
        _ => None,
    }
}

/// The `address` pointer of a `NativeSymbolInfo` list, whatever routine it points to.
pub unsafe fn symbol_address(info: Sexp) -> Option<Sexp> {
    if !is_native_symbol_info(info) {
        return None;
    }
    slot(info, ADDRESS_SLOT, symbols().address.car())
        .filter(|address| address.kind() == SexpType::ExternalPtr)
}

/// The `numParameters` a `NativeSymbolInfo` list records, when it is one integer.
pub unsafe fn symbol_arity(info: Sexp) -> Option<i32> {
    slot(info, ARITY_SLOT, symbols().num_parameters.car())
        .filter(|value| value.kind() == SexpType::Integer && value.length() == 1)
        .map(|value| value.slice::<i32>()[0])
}

/// The element of `list` named by `label`, an interned string. Every `NativeSymbolInfo` that R
/// or ry builds lists `name`, `address`, `dll` and `numParameters` in that order, so the
/// expected position is checked first, and the names are searched only for a list built some
/// other way. Names are interned, so each check compares two pointers.
pub unsafe fn slot(list: Sexp, position: usize, label: Sexp) -> Option<Sexp> {
    let names = raw_attribute(list, R_NamesSymbol.get());
    if names.kind() != SexpType::String {
        return None;
    }
    let count = names.length() as usize;
    if position < count && names.element(position) == label {
        return Some(list.element(position));
    }
    (0..count)
        .find(|&index| names.element(index) == label)
        .map(|index| list.element(index))
}

const ADDRESS_SLOT: usize = 1;
const ARITY_SLOT: usize = 3;

unsafe fn is_native_symbol_info(object: Sexp) -> bool {
    if !object.is_object() {
        return false;
    }
    let classes = raw_attribute(object, R_ClassSymbol.get());
    let label = symbols().native_symbol_info.car();
    classes.kind() == SexpType::String
        && (0..classes.length() as usize).any(|index| classes.element(index) == label)
}

/// The entry point an address pointer holds, when it is native code.
unsafe fn native_address(pointer: Sexp) -> Option<DlFunc> {
    if pointer.kind() != SexpType::ExternalPtr || pointer.external_tag().kind() != SexpType::Nil {
        return None;
    }
    let address = pointer.external_address();
    if address.is_null() {
        return None;
    }
    // SAFETY: an untagged, non-null routine address is the entry point `dlsym` or a package's
    // registration returned, which the `.Call` convention calls as a `DL_FUNC`.
    Some(unsafe { std::mem::transmute::<*mut std::ffi::c_void, DlFunc>(address) })
}
