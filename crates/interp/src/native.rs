//! The foreign-function interface as R code sees it (`.Call`, `dyn.load`, `getLoadedDLLs`, ...).
//! Loading and calling native code is the libR host's job, so these are the definitions for a
//! session without one: base R's own (empty) routine tables exist, and calling native code is an
//! error. The host replaces them with [`crate::register_builtins`].

use crate::{
    builtins::{BuiltinDef, Call, builtin, internal},
    unwind::R,
    values::{self, nil, scalar_bool, string_vector, symbols},
};
use heap::object::Sexp;

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".Call", -1, no_host),
    builtin(".External", -1, no_host),
    builtin(".External2", -1, no_host),
    builtin(".C", -1, no_host),
    builtin(".Fortran", -1, no_host),
    builtin(".Call.graphics", -1, no_host),
    builtin(".External.graphics", -1, no_host),
    internal("dyn.load", 4, no_host),
    internal("dyn.unload", 1, no_host),
    internal("is.loaded", 3, |_| unsafe { scalar_bool(false) }),
    internal("getLoadedDLLs", 0, |_| unsafe { loaded_dlls() }),
    internal("getRegisteredRoutines", 1, |_| unsafe { base_routines() }),
    internal("getSymbolInfo", 3, |_| Ok(nil())),
    internal("getCallingDLLe", 1, |_| Ok(nil())),
];

unsafe fn no_host(call: &Call<'_>) -> R<Sexp> {
    Err(call.error(format!(
        "'{}' needs native code, which this session cannot load (it runs without libR)",
        call.name()
    )))
}

/// A `DLLInfo` object for a library with no routines, such as base.
pub unsafe fn dll_info(name: &str, path: &str) -> R<Sexp> {
    let handle = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
    crate::heap().root(handle);
    let info = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
    crate::heap().root(info);
    let info_class = values::scalar_string("DLLInfoReference")?;
    values::set_attr(info, symbols().class, info_class)?;
    let handle_class = values::scalar_string("DLLHandle")?;
    values::set_attr(handle, symbols().class, handle_class)?;
    let result = values::named_list(&[
        ("name", values::scalar_string(name)?),
        ("path", values::scalar_string(path)?),
        ("dynamicLookup", scalar_bool(false)?),
        ("handle", handle),
        ("info", info),
        ("forceSymbols", scalar_bool(false)?),
    ])?;
    let class = values::scalar_string("DLLInfo")?;
    values::set_attr(result, symbols().class, class)?;
    Ok(result)
}

unsafe fn loaded_dlls() -> R<Sexp> {
    let base = dll_info("base", "base")?;
    let list = values::list_vector(&[base])?;
    let names = string_vector(&["base"])?;
    values::set_attr(list, symbols().names, names)?;
    let class = values::scalar_string("DLLInfoList")?;
    values::set_attr(list, symbols().class, class)?;
    Ok(list)
}

/// The routine tables of a library that registered none.
pub unsafe fn empty_routines() -> R<Sexp> {
    let mut tables = Vec::new();
    for _ in 0..4 {
        let table = values::list_vector(&[])?;
        let names = string_vector::<&str>(&[])?;
        values::set_attr(table, symbols().names, names)?;
        let class = values::scalar_string("NativeRoutineList")?;
        values::set_attr(table, symbols().class, class)?;
        tables.push(table);
    }
    let result = values::list_vector(&tables)?;
    let names = string_vector(&[".C", ".Call", ".Fortran", ".External"])?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

/// Base's own routine tables: the task-callback entry points and the LINPACK QR routines, which
/// GNU R registers for `qr()` and friends.
pub unsafe fn base_routines() -> R<Sexp> {
    let dll = dll_info("base", "base")?;
    let mut tables = Vec::new();
    let groups: [(&[(&str, i32)], &str); 4] = [
        (&[], "CRoutine"),
        (
            &[("R_addTaskCallback", 4), ("R_getTaskCallbackNames", 0), ("R_removeTaskCallback", 1)],
            "CallRoutine",
        ),
        (
            &[
                ("dqrcf", 8),
                ("dqrdc2", 9),
                ("dqrqty", 7),
                ("dqrqy", 7),
                ("dqrrsd", 7),
                ("dqrxb", 7),
                ("dtrco", 6),
            ],
            "FortranRoutine",
        ),
        (&[], "ExternalRoutine"),
    ];
    for (routines, class) in groups {
        let mut entries = Vec::new();
        for (name, arity) in routines {
            let address = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
            crate::heap().root(address);
            let address_class = values::scalar_string("RegisteredNativeSymbol")?;
            values::set_attr(address, symbols().class, address_class)?;
            let entry = values::named_list(&[
                ("name", values::scalar_string(name)?),
                ("address", address),
                ("dll", dll),
                ("numParameters", values::scalar_integer(*arity)?),
            ])?;
            let entry_class = string_vector(&[class, "NativeSymbolInfo"])?;
            values::set_attr(entry, symbols().class, entry_class)?;
            entries.push(entry);
        }
        let table = values::list_vector(&entries)?;
        let names: Vec<&str> = routines.iter().map(|(name, _)| *name).collect();
        let names = string_vector(&names)?;
        values::set_attr(table, symbols().names, names)?;
        let list_class = values::scalar_string("NativeRoutineList")?;
        values::set_attr(table, symbols().class, list_class)?;
        tables.push(table);
    }
    let result = values::list_vector(&tables)?;
    let names = string_vector(&[".C", ".Call", ".Fortran", ".External"])?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}
