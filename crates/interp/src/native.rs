//! The foreign-function interface as R code sees it (`.Call`, `dyn.load`, `getLoadedDLLs`, ...).
//! Loading and calling native code is the libR host's job, so these are the definitions for a
//! session without one: base R's routine tables and the default packages' Rust routines
//! ([`crate::builtin_libraries`]) work, and calling other native code is an error. The host
//! replaces them with [`crate::register_builtins`].
//!
//! Base's own native routines are Rust too ([`BASE_ROUTINES`]): both this module's `.Call`, `.C`
//! and `.Fortran` and the host's consult [`call_base_routine`] first, so `qr()` works with or
//! without libR.

use crate::{
    TaskCallbackRoutine, builtin_libraries,
    builtins::{Arg, BuiltinDef, Call, builtin, internal},
    interp, linalg,
    unwind::{R, error},
    values::{self, nil, scalar_bool, string_vector, symbols},
};
use heap::object::{Sexp, SexpType};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin(".Call", -1, |call| unsafe { foreign_call(call, Some(Interface::Call)) }),
    builtin(".External", -1, |call| unsafe { foreign_call(call, None) }),
    builtin(".External2", -1, |call| unsafe { foreign_call(call, None) }),
    builtin(".C", -1, |call| unsafe { foreign_call(call, Some(Interface::C)) }),
    builtin(".Fortran", -1, |call| unsafe { foreign_call(call, Some(Interface::Fortran)) }),
    builtin(".Call.graphics", -1, |call| unsafe { foreign_call(call, None) }),
    builtin(".External.graphics", -1, |call| unsafe { foreign_call(call, None) }),
    internal("dyn.load", 4, |call| unsafe {
        let path = values::require_string(call.arg(0), "x")?;
        match builtin_libraries::builtin_library(&path) {
            Some(package) => builtin_libraries::load(package, &path),
            None => no_host(call),
        }
    }),
    internal("dyn.unload", 1, no_host),
    internal("is.loaded", 3, |call| unsafe {
        let name = values::first_string(call.arg(0))
            .map(|name| name.into_owned())
            .unwrap_or_default();
        scalar_bool(is_base_routine(&name))
    }),
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

/// The calling interfaces a native routine is registered for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interface {
    C,
    Call,
    Fortran,
}

/// A native routine of base, implemented by the interpreter.
pub struct BaseRoutine {
    pub name: &'static str,
    pub arity: usize,
    pub body: RoutineBody,
}

pub enum RoutineBody {
    /// A `.Fortran` routine, which updates the copies `.Fortran` made of its arguments.
    Fortran(unsafe fn(&[Sexp]) -> R<()>),
    /// A `.Call` routine, which returns its value.
    Call(unsafe fn(&[Sexp]) -> R<Sexp>),
}

impl BaseRoutine {
    fn interface(&self) -> Interface {
        match self.body {
            RoutineBody::Fortran(_) => Interface::Fortran,
            RoutineBody::Call(_) => Interface::Call,
        }
    }
}

/// Base's native routines, as GNU R registers them: the task-callback entry points and the
/// LINPACK QR and condition routines behind `qr()` and `kappa()`.
pub const BASE_ROUTINES: &[BaseRoutine] = &[
    call_routine("R_addTaskCallback", 4, |args| unsafe {
        (interp().hooks.task_callback_routine)(TaskCallbackRoutine::Add, args)
    }),
    call_routine("R_getTaskCallbackNames", 0, |args| unsafe {
        (interp().hooks.task_callback_routine)(TaskCallbackRoutine::Names, args)
    }),
    call_routine("R_removeTaskCallback", 1, |args| unsafe {
        (interp().hooks.task_callback_routine)(TaskCallbackRoutine::Remove, args)
    }),
    fortran_routine("dqrcf", 8, linalg::dqrcf),
    fortran_routine("dqrdc2", 9, linalg::dqrdc2),
    fortran_routine("dqrqty", 7, linalg::dqrqty),
    fortran_routine("dqrqy", 7, linalg::dqrqy),
    fortran_routine("dqrrsd", 7, linalg::dqrrsd),
    fortran_routine("dqrxb", 7, linalg::dqrxb),
    fortran_routine("dtrco", 6, linalg::dtrco),
];

const fn call_routine(
    name: &'static str,
    arity: usize,
    function: unsafe fn(&[Sexp]) -> R<Sexp>,
) -> BaseRoutine {
    BaseRoutine {
        name,
        arity,
        body: RoutineBody::Call(function),
    }
}

const fn fortran_routine(
    name: &'static str,
    arity: usize,
    function: unsafe fn(&[Sexp]) -> R<()>,
) -> BaseRoutine {
    BaseRoutine {
        name,
        arity,
        body: RoutineBody::Fortran(function),
    }
}

pub fn is_base_routine(name: &str) -> bool {
    BASE_ROUTINES.iter().any(|routine| routine.name == name)
}

/// Runs a `.C`, `.Call` or `.Fortran` call whose `.NAME` is one of [`BASE_ROUTINES`], and
/// returns `None` for any other routine, which the caller resolves itself. `.NAME` names a base
/// routine when it is the `NativeSymbolInfo` base R binds (such as `.F_dqrdc2`, whose `dll` is
/// base), or a string with no `PACKAGE` or `PACKAGE = "base"`, since base is the first library
/// GNU R searches.
pub unsafe fn call_base_routine(call: &Call<'_>, interface: Interface) -> R<Option<Sexp>> {
    let Some((first, rest)) = call.args.split_first() else {
        return Ok(None);
    };
    let package_symbol = values::install("PACKAGE")?;
    let na_ok_symbol = values::install("NAOK")?;
    let ignored = [values::install("DUP")?, values::install("ENCODING")?];
    let mut package = None;
    let mut na_ok = false;
    let mut args: Vec<Arg> = Vec::with_capacity(rest.len());
    for arg in rest {
        if arg.tag == package_symbol {
            package = values::first_string(arg.value).map(|text| text.into_owned());
        } else if arg.tag == na_ok_symbol {
            na_ok = values::as_logical_scalar(arg.value) == 1;
        } else if !ignored.contains(&arg.tag) {
            args.push(*arg);
        }
    }
    let name = match first.value.kind() {
        SexpType::String if package.as_deref().is_none_or(|package| package == "base") => {
            values::first_string(first.value).map(|text| text.into_owned())
        }
        SexpType::List => {
            let dll = values::list_get(first.value, "dll");
            let dll_name = values::first_string(values::list_get(dll, "name"));
            if dll_name.as_deref() == Some("base") {
                values::first_string(values::list_get(first.value, "name"))
                    .map(|text| text.into_owned())
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(routine) = name.and_then(|name| {
        BASE_ROUTINES
            .iter()
            .find(|routine| routine.name == name && routine.interface() == interface)
    }) else {
        return Ok(None);
    };
    if args.len() != routine.arity {
        return Err(call.error(format!(
            "Incorrect number of arguments ({}), expecting {} for '{}'",
            args.len(),
            routine.arity,
            routine.name
        )));
    }
    match routine.body {
        RoutineBody::Call(function) => {
            let values: Vec<Sexp> = args.iter().map(|arg| arg.value).collect();
            function(&values).map(Some)
        }
        RoutineBody::Fortran(function) => {
            let mut copies = Vec::with_capacity(args.len());
            for (index, arg) in args.iter().enumerate() {
                let value = values::materialize(arg.value)?;
                if !matches!(
                    value.kind(),
                    SexpType::Logical
                        | SexpType::Integer
                        | SexpType::Real
                        | SexpType::Complex
                        | SexpType::Raw
                ) {
                    return Err(error(format!(
                        "invalid mode ({}) to pass to Fortran (arg {})",
                        value.kind().name(),
                        index + 1
                    )));
                }
                if !na_ok {
                    check_finite(value, index + 1)?;
                }
                copies.push(values::duplicate(value)?);
            }
            function(&copies)?;
            let result = values::list_vector(&copies)?;
            if args.iter().any(|arg| arg.tag != nil()) {
                let names: Vec<String> = args
                    .iter()
                    .map(|arg| {
                        if arg.tag.kind() == SexpType::Symbol {
                            values::symbol_name(arg.tag).into_owned()
                        } else {
                            String::new()
                        }
                    })
                    .collect();
                let names = string_vector(&names)?;
                values::set_attr(result, symbols().names, names)?;
            }
            Ok(Some(result))
        }
    }
}

/// `.C` and `.Fortran` refuse missing and non-finite values unless `NAOK = TRUE`.
unsafe fn check_finite(value: Sexp, position: usize) -> R<()> {
    let message = match value.kind() {
        SexpType::Logical | SexpType::Integer
            if values::ints(value)
                .iter()
                .any(|&entry| values::is_na_int(entry)) =>
        {
            "NAs in foreign function call"
        }
        SexpType::Real if values::reals(value).iter().any(|entry| !entry.is_finite()) => {
            "NA/NaN/Inf in foreign function call"
        }
        SexpType::Complex
            if values::complexes(value)
                .iter()
                .any(|entry| !entry.r.is_finite() || !entry.i.is_finite()) =>
        {
            "complex NA/NaN/Inf in foreign function call"
        }
        _ => return Ok(()),
    };
    Err(error(format!("{message} (arg {position})")))
}

unsafe fn foreign_call(call: &Call<'_>, interface: Option<Interface>) -> R<Sexp> {
    if let Some(interface) = interface
        && let Some(result) = call_base_routine(call, interface)?
    {
        return Ok(result);
    }
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

/// Base's routine tables, listing [`BASE_ROUTINES`] by interface. Base registers no
/// `.External` routines.
pub unsafe fn base_routines() -> R<Sexp> {
    let dll = dll_info("base", "base")?;
    let mut tables = Vec::new();
    let groups = [
        (Some(Interface::C), "CRoutine"),
        (Some(Interface::Call), "CallRoutine"),
        (Some(Interface::Fortran), "FortranRoutine"),
        (None, "ExternalRoutine"),
    ];
    for (interface, class) in groups {
        let routines: Vec<&BaseRoutine> = BASE_ROUTINES
            .iter()
            .filter(|routine| Some(routine.interface()) == interface)
            .collect();
        let mut entries = Vec::new();
        for routine in &routines {
            let address = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
            crate::heap().root(address);
            let address_class = values::scalar_string("RegisteredNativeSymbol")?;
            values::set_attr(address, symbols().class, address_class)?;
            let entry = values::named_list(&[
                ("name", values::scalar_string(routine.name)?),
                ("address", address),
                ("dll", dll),
                (
                    "numParameters",
                    values::scalar_integer(routine.arity as i32)?,
                ),
            ])?;
            let entry_class = string_vector(&[class, "NativeSymbolInfo"])?;
            values::set_attr(entry, symbols().class, entry_class)?;
            entries.push(entry);
        }
        let table = values::list_vector(&entries)?;
        let names: Vec<&str> = routines.iter().map(|routine| routine.name).collect();
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
