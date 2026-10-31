//! Loading package shared objects, routine registration and cross-package C callables.
//!
//! `load_library` opens a shared object, runs its `R_init_<name>` inside a C context, and keeps
//! its registration tables. `.Call` resolves a routine by name through the registered tables
//! first and then, when the library allows it, through the dynamic symbol table.

use crate::{
    Rboolean,
    boundary::{ApiResult, DlFunc, enter, error, ry_call_init, with_native_scope},
    globals::{R_ClassSymbol, R_NamesSymbol},
    runtime,
    strings::c_bytes,
};
use heap::{
    object::{Sexp, SexpType},
    strings::Encoding,
};
use std::ffi::{CStr, CString, c_char, c_int, c_void};

/// What GNU R calls `DllInfo`. Packages only hold pointers to it, so its layout is ry's.
pub struct Dll {
    pub path: String,
    pub name: String,
    handle: *mut c_void,
    use_dynamic_symbols: bool,
    force_symbols: bool,
    c_routines: Vec<Routine>,
    call_routines: Vec<Routine>,
    fortran_routines: Vec<Routine>,
    external_routines: Vec<Routine>,
}

pub struct Routine {
    pub name: String,
    pub function: DlFunc,
    pub arity: c_int,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoutineKind {
    Call,
    External,
    C,
    Fortran,
}

/// `R_CallMethodDef` and `R_ExternalMethodDef`.
#[repr(C)]
pub struct CallMethodDef {
    name: *const c_char,
    function: Option<DlFunc>,
    arity: c_int,
}

/// `R_CMethodDef` and `R_FortranMethodDef`.
#[repr(C)]
pub struct CMethodDef {
    name: *const c_char,
    function: Option<DlFunc>,
    arity: c_int,
    types: *mut u32,
}

impl Dll {
    fn new(path: String, name: String, handle: *mut c_void) -> Dll {
        Dll {
            path,
            name,
            handle,
            use_dynamic_symbols: true,
            force_symbols: false,
            c_routines: Vec::new(),
            call_routines: Vec::new(),
            fortran_routines: Vec::new(),
            external_routines: Vec::new(),
        }
    }

    pub fn registered(&self, kind: RoutineKind) -> &[Routine] {
        match kind {
            RoutineKind::Call => &self.call_routines,
            RoutineKind::External => &self.external_routines,
            RoutineKind::C => &self.c_routines,
            RoutineKind::Fortran => &self.fortran_routines,
        }
    }

    pub fn handle(&self) -> *mut c_void {
        self.handle
    }

    pub fn dynamic_lookup(&self) -> bool {
        self.use_dynamic_symbols
    }
}

unsafe fn dlerror_text() -> String {
    let message = libc::dlerror();
    if message.is_null() {
        "unknown error".into()
    } else {
        CStr::from_ptr(message).to_string_lossy().into_owned()
    }
}

/// `dyn.load`: opens the library, runs its initialization routine, and returns its record. A
/// library that is already loaded is returned as it is.
pub unsafe fn load_library(path: &str) -> ApiResult<*mut Dll> {
    let runtime = runtime();
    let canonical = std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned());
    if let Some(dll) = runtime.dlls.iter_mut().find(|dll| dll.path == canonical) {
        return Ok(&mut **dll);
    }
    let c_path = CString::new(canonical.clone()).map_err(|_| error("invalid library path"))?;
    let handle = libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
    if handle.is_null() {
        return Err(error(format!(
            "unable to load shared object '{canonical}':\n  {}",
            dlerror_text()
        )));
    }
    let file_name = std::path::Path::new(&canonical)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = file_name.split('.').next().unwrap_or(&file_name).to_owned();
    runtime
        .dlls
        .push(Box::new(Dll::new(canonical, name.clone(), handle)));
    let dll: *mut Dll = &mut **runtime
        .dlls
        .last_mut()
        .ok_or_else(|| error("the library list is empty"))?;
    let init_name = CString::new(format!("R_init_{}", name.replace('.', "_")))
        .map_err(|_| error("invalid library name"))?;
    let init = libc::dlsym(handle, init_name.as_ptr());
    if !init.is_null() {
        let init: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(init);
        with_native_scope(|| ry_call_init(init, dll.cast()))?;
    }
    Ok(dll)
}

/// The `DLLInfo` object `dyn.load` returns: a list of the library's name and path.
pub unsafe fn dll_info_object(dll: *mut Dll) -> ApiResult<Sexp> {
    let runtime = runtime();
    let info = runtime.heap.alloc_vector(SexpType::List, 2)?;
    runtime.heap.root(info);
    let names = runtime.heap.alloc_vector(SexpType::String, 2)?;
    runtime.heap.root(names);
    for (index, (name, value)) in [("name", &(*dll).name), ("path", &(*dll).path)]
        .iter()
        .enumerate()
    {
        let value = runtime.heap.mk_char(value.as_bytes(), Encoding::Utf8)?;
        let value = crate::memory::scalar_string(value)?;
        runtime.heap.set_element(info, index, value);
        let name = runtime.heap.mk_char(name.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(names, index, name);
    }
    crate::attributes::install_attribute(info, R_NamesSymbol.get(), names)?;
    let class = runtime.heap.mk_char(b"DLLInfo", Encoding::Native)?;
    let class = crate::memory::scalar_string(class)?;
    crate::attributes::install_attribute(info, R_ClassSymbol.get(), class)?;
    info.set_object(true);
    Ok(info)
}

/// Finds a `.Call` or `.External` routine, searching the most recently loaded library first.
/// The arity is known only for registered routines.
pub unsafe fn find_routine(
    name: &str,
    package: Option<&str>,
    kind: RoutineKind,
) -> ApiResult<(DlFunc, Option<c_int>)> {
    let runtime = runtime();
    for dll in runtime.dlls.iter().rev() {
        if package.is_some_and(|package| package != dll.name) || dll.force_symbols {
            continue;
        }
        if let Some(routine) = dll
            .registered(kind)
            .iter()
            .find(|routine| routine.name == name)
        {
            return Ok((routine.function, Some(routine.arity)));
        }
        if dll.use_dynamic_symbols {
            let c_name = CString::new(name).map_err(|_| error("invalid routine name"))?;
            let symbol = libc::dlsym(dll.handle, c_name.as_ptr());
            if !symbol.is_null() {
                return Ok((std::mem::transmute::<*mut c_void, DlFunc>(symbol), None));
            }
        }
    }
    let interface = match kind {
        RoutineKind::Call => ".Call",
        RoutineKind::External => ".External",
        RoutineKind::C => ".C",
        RoutineKind::Fortran => ".Fortran",
    };
    Err(match package {
        Some(package) => error(format!(
            "\"{name}\" not available for {interface}() for package \"{package}\""
        )),
        None => error(format!("C symbol name \"{name}\" not in load table")),
    })
}

unsafe fn copy_call_routines(table: *const CallMethodDef) -> Vec<Routine> {
    let mut routines = Vec::new();
    if table.is_null() {
        return routines;
    }
    let mut entry = table;
    while !(*entry).name.is_null() {
        if let Some(function) = (*entry).function {
            routines.push(Routine {
                name: String::from_utf8_lossy(c_bytes((*entry).name)).into_owned(),
                function,
                arity: (*entry).arity,
            });
        }
        entry = entry.add(1);
    }
    routines
}

unsafe fn copy_c_routines(table: *const CMethodDef) -> Vec<Routine> {
    let mut routines = Vec::new();
    if table.is_null() {
        return routines;
    }
    let mut entry = table;
    while !(*entry).name.is_null() {
        if let Some(function) = (*entry).function {
            routines.push(Routine {
                name: String::from_utf8_lossy(c_bytes((*entry).name)).into_owned(),
                function,
                arity: (*entry).arity,
            });
        }
        entry = entry.add(1);
    }
    routines
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_registerRoutines(
    dll: *mut Dll,
    c_routines: *const CMethodDef,
    call_routines: *const CallMethodDef,
    fortran_routines: *const CMethodDef,
    external_routines: *const CallMethodDef,
) -> c_int {
    enter(|| {
        let dll = dll
            .as_mut()
            .ok_or_else(|| error("R_registerRoutines called without a DllInfo"))?;
        dll.c_routines = copy_c_routines(c_routines);
        dll.call_routines = copy_call_routines(call_routines);
        dll.fortran_routines = copy_c_routines(fortran_routines);
        dll.external_routines = copy_call_routines(external_routines);
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_useDynamicSymbols(dll: *mut Dll, value: Rboolean) -> Rboolean {
    match dll.as_mut() {
        Some(dll) => std::mem::replace(&mut dll.use_dynamic_symbols, value != 0) as Rboolean,
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_forceSymbols(dll: *mut Dll, value: Rboolean) -> Rboolean {
    match dll.as_mut() {
        Some(dll) => std::mem::replace(&mut dll.force_symbols, value != 0) as Rboolean,
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getDllInfo(name: *const c_char) -> *mut Dll {
    let name = String::from_utf8_lossy(c_bytes(name)).into_owned();
    runtime()
        .dlls
        .iter_mut()
        .find(|dll| dll.name == name)
        .map_or(std::ptr::null_mut(), |dll| &mut **dll)
}

/// The record that an embedding application registers its own routines in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getEmbeddingDllInfo() -> *mut Dll {
    let runtime = runtime();
    if let Some(dll) = runtime
        .dlls
        .iter_mut()
        .find(|dll| dll.name == "(embedding)")
    {
        return &mut **dll;
    }
    let handle = libc::dlopen(std::ptr::null(), libc::RTLD_NOW);
    runtime.dlls.insert(
        0,
        Box::new(Dll::new(String::new(), "(embedding)".into(), handle)),
    );
    &mut *runtime.dlls[0]
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_FindSymbol(
    name: *const c_char,
    package: *const c_char,
    _symbol: *mut c_void,
) -> Option<DlFunc> {
    let name = String::from_utf8_lossy(c_bytes(name)).into_owned();
    let package = String::from_utf8_lossy(c_bytes(package)).into_owned();
    let runtime = runtime();
    for dll in runtime.dlls.iter().rev() {
        if !package.is_empty() && package != dll.name {
            continue;
        }
        let tables = [
            &dll.call_routines,
            &dll.external_routines,
            &dll.c_routines,
            &dll.fortran_routines,
        ];
        for table in tables {
            if let Some(routine) = table.iter().find(|routine| routine.name == name) {
                return Some(routine.function);
            }
        }
        if dll.use_dynamic_symbols
            && let Ok(c_name) = CString::new(name.clone())
        {
            let symbol = libc::dlsym(dll.handle, c_name.as_ptr());
            if !symbol.is_null() {
                return Some(std::mem::transmute::<*mut c_void, DlFunc>(symbol));
            }
        }
    }
    None
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_RegisterCCallable(
    package: *const c_char,
    name: *const c_char,
    function: Option<DlFunc>,
) {
    let key = (
        String::from_utf8_lossy(c_bytes(package)).into_owned(),
        String::from_utf8_lossy(c_bytes(name)).into_owned(),
    );
    let address = function.map_or(0, |function| function as usize);
    runtime().c_callables.insert(key, address);
}

/// `R_GetCCallable`. GNU R loads the providing namespace when the callable is missing; without
/// the interpreter that cannot happen yet, so a missing callable is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetCCallable(
    package: *const c_char,
    name: *const c_char,
) -> Option<DlFunc> {
    enter(|| {
        let package = String::from_utf8_lossy(c_bytes(package)).into_owned();
        let name = String::from_utf8_lossy(c_bytes(name)).into_owned();
        match runtime().c_callables.get(&(package.clone(), name.clone())) {
            Some(&address) if address != 0 => {
                Ok(Some(std::mem::transmute::<usize, DlFunc>(address)))
            }
            _ => Err(error(format!(
                "function '{name}' not provided by package '{package}'"
            ))),
        }
    })
}
