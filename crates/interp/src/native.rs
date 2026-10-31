//! The foreign-function interface as R code sees it (`.Call`, `dyn.load`, `getLoadedDLLs`, ...).
//! Loading and calling native code is the libR host's job, so these are the definitions for a
//! session without one: base R's own routine tables and the default packages' Rust routines
//! ([`crate::builtin_libraries`]) work, and calling other native code is an error. The host
//! replaces them with [`crate::register_builtins`].

use crate::{
    builtin_libraries,
    builtins::{BuiltinDef, Call, builtin, internal},
    unwind::R,
    values::{self, nil, scalar_bool, string_vector, symbols},
};
use heap::object::Sexp;

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".Call", -1, foreign_call),
    builtin(".External", -1, foreign_call),
    builtin(".External2", -1, foreign_call),
    builtin(".C", -1, foreign_call),
    builtin(".Fortran", -1, foreign_call),
    builtin(".Call.graphics", -1, foreign_call),
    builtin(".External.graphics", -1, foreign_call),
    internal("dyn.load", 4, |call| unsafe {
        let path = values::require_string(call.arg(0), "x")?;
        match builtin_libraries::builtin_library(&path) {
            Some(package) => builtin_libraries::load(package, &path),
            None => no_host(call),
        }
    }),
    internal("dyn.unload", 1, no_host),
    internal("is.loaded", 3, |_| unsafe { scalar_bool(false) }),
    internal("getLoadedDLLs", 0, |_| unsafe { loaded_dlls() }),
    internal("getRegisteredRoutines", 1, |call| unsafe {
        match builtin_libraries::registered_routines(call.arg(0))? {
            Some(routines) => Ok(routines),
            None => base_routines(),
        }
    }),
    internal("getSymbolInfo", 3, |call| unsafe {
        let name = values::require_string(call.arg(0), "name")?;
        let package = values::first_string(call.arg(1)).map(|text| text.into_owned());
        Ok(builtin_libraries::symbol_info(&name, package.as_deref())?.unwrap_or_else(nil))
    }),
    internal("getCallingDLLe", 1, |_| Ok(nil())),
];

unsafe fn foreign_call(call: &Call<'_>) -> R<Sexp> {
    match builtin_libraries::try_call(call.call, call.args, call.env)? {
        Some(value) => Ok(value),
        None => no_host(call),
    }
}

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
    let mut items = vec![dll_info("base", "base")?];
    let mut names = vec!["base".to_owned()];
    for (name, info) in builtin_libraries::loaded()? {
        items.push(info);
        names.push(name);
    }
    let list = values::list_vector(&items)?;
    let names = string_vector(&names)?;
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
