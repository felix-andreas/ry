//! Loading package shared objects, routine registration and cross-package C callables.
//!
//! `load_library` opens a shared object, runs its `R_init_<name>` inside a C context, and keeps
//! its registration tables. `.Call` resolves a routine by name through the registered tables
//! first and then, when the library allows it, through the dynamic symbol table.

use {
    crate::{
        Rboolean,
        boundary::{
            ApiResult, DlFunc, enter, error, ry_call_dlopen, ry_call_init, with_native_scope,
        },
        globals::{R_ClassSymbol, R_NamesSymbol},
        runtime,
        strings::c_bytes,
    },
    heap::{
        object::{Sexp, SexpType},
        strings::Encoding,
    },
    std::ffi::{CStr, CString, c_char, c_int, c_void},
};

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
    C,
    Call,
    Fortran,
    External,
}

impl RoutineKind {
    pub fn interface(self) -> &'static str {
        match self {
            RoutineKind::C => ".C",
            RoutineKind::Call => ".Call",
            RoutineKind::Fortran => ".Fortran",
            RoutineKind::External => ".External",
        }
    }
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
            RoutineKind::C => &self.c_routines,
            RoutineKind::Call => &self.call_routines,
            RoutineKind::Fortran => &self.fortran_routines,
            RoutineKind::External => &self.external_routines,
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
/// library that is already loaded is returned as it is. With `local` false, its symbols become
/// visible to libraries loaded later, as `dyn.load(local = FALSE)` asks (RcppParallel loads
/// itself that way so packages linking to it resolve its functions).
pub unsafe fn load_library(path: &str, local: bool) -> ApiResult<*mut Dll> {
    let runtime = runtime();
    let canonical = std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned());
    if let Some(dll) = runtime.dlls.iter_mut().find(|dll| dll.path == canonical) {
        return Ok(&mut **dll);
    }
    let c_path = CString::new(canonical.clone()).map_err(|_| error("invalid library path"))?;
    let visibility = if local {
        libc::RTLD_LOCAL
    } else {
        libc::RTLD_GLOBAL
    };
    let mut handle = std::ptr::null_mut();
    with_native_scope(|| {
        let mut status = 0;
        handle = ry_call_dlopen(c_path.as_ptr(), libc::RTLD_NOW | visibility, &mut status);
        status
    })?;
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
    // The library's name is its file name without the extension (`data_table.so` is
    // `data_table`), which `R_init_<name>` and `PACKAGE =` use.
    let name = file_name
        .rsplit_once('.')
        .map_or(file_name.as_str(), |(stem, _)| stem)
        .to_owned();
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
            // Fortran compilers append an underscore to external names.
            let symbol_name = if kind == RoutineKind::Fortran {
                format!("{}_", name.to_ascii_lowercase())
            } else {
                name.to_owned()
            };
            let c_name = CString::new(symbol_name).map_err(|_| error("invalid routine name"))?;
            let symbol = libc::dlsym(dll.handle, c_name.as_ptr());
            if !symbol.is_null() {
                return Ok((std::mem::transmute::<*mut c_void, DlFunc>(symbol), None));
            }
        }
    }
    let interface = kind.interface();
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

/// `R_GetCCallable`. A callable that is not registered yet makes GNU R load the providing
/// package's namespace, whose initialization registers it; ry does the same through the
/// interpreter when one is installed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetCCallable(
    package: *const c_char,
    name: *const c_char,
) -> Option<DlFunc> {
    enter(|| {
        let package = String::from_utf8_lossy(c_bytes(package)).into_owned();
        let name = String::from_utf8_lossy(c_bytes(name)).into_owned();
        let key = (package.clone(), name.clone());
        let registered = |key: &(String, String)| match runtime().c_callables.get(key) {
            Some(&address) if address != 0 => Some(std::mem::transmute::<usize, DlFunc>(address)),
            _ => None,
        };
        if let Some(function) = registered(&key) {
            return Ok(Some(function));
        }
        if crate::eval::namespaces_loadable()?
            && crate::environments::registered_namespace(package.as_bytes())?.kind()
                == SexpType::Nil
        {
            let package_name = crate::strings::mk_char(package.as_bytes(), Encoding::Utf8)?;
            let package_name = crate::memory::scalar_string(package_name)?;
            runtime().heap.root(package_name);
            crate::eval::call_namespaced("base", "loadNamespace", &[package_name])?;
            if let Some(function) = registered(&key) {
                return Ok(Some(function));
            }
        }
        Err(error(format!(
            "function '{name}' not provided by package '{package}'"
        )))
    })
}

/// The loaded libraries, oldest first, as `getLoadedDLLs()` lists them.
pub fn loaded_libraries() -> Vec<(String, String)> {
    runtime()
        .dlls
        .iter()
        .map(|dll| (dll.name.clone(), dll.path.clone()))
        .collect()
}

/// How many routines of each interface a library registered: `.C`, `.Call`, `.Fortran` and
/// `.External`.
pub fn registered_counts(name: &str) -> Option<[usize; 4]> {
    runtime()
        .dlls
        .iter()
        .find(|dll| dll.name == name)
        .map(|dll| {
            [
                dll.c_routines.len(),
                dll.call_routines.len(),
                dll.fortran_routines.len(),
                dll.external_routines.len(),
            ]
        })
}

/// The registered routine `name` of the library `package` (any interface), as
/// `getNativeSymbolInfo` finds it: registered routines are found even when the library forces
/// symbols, because that only forbids lookup by a name string in `.Call` itself.
/// An unregistered routine comes from the library's dynamic symbol table, as with GNU R's
/// lookup, with an unknown arity (-1).
pub unsafe fn registered_routine(
    name: &str,
    package: &str,
) -> Option<(RoutineKind, DlFunc, c_int)> {
    let runtime = runtime();
    let dll = runtime.dlls.iter().rev().find(|dll| dll.name == package)?;
    for kind in [
        RoutineKind::Call,
        RoutineKind::External,
        RoutineKind::C,
        RoutineKind::Fortran,
    ] {
        if let Some(routine) = dll
            .registered(kind)
            .iter()
            .find(|routine| routine.name == name)
        {
            return Some((kind, routine.function, routine.arity));
        }
    }
    let c_name = CString::new(name).ok()?;
    let symbol = libc::dlsym(dll.handle, c_name.as_ptr());
    (!symbol.is_null()).then(|| {
        (
            RoutineKind::Call,
            std::mem::transmute::<*mut c_void, DlFunc>(symbol),
            -1,
        )
    })
}

/// A `NativeSymbolInfo` object for a registered routine: a list of `name`, `address` (an
/// external pointer of class `RegisteredNativeSymbol`), `dll` and `numParameters`, as
/// `getNativeSymbolInfo` returns, which `.Call` accepts in place of a name.
pub unsafe fn native_symbol_info(name: &str, package: &str) -> ApiResult<Sexp> {
    let (kind, function, arity) = registered_routine(name, package)
        .ok_or_else(|| error(format!("no such symbol {name} in package {package}")))?;
    let runtime = runtime();
    let nil = runtime.nil();
    let address = runtime
        .heap
        .alloc_external_ptr(function as *mut c_void, nil, nil)?;
    runtime.heap.root(address);
    let class = crate::strings::mk_char(b"RegisteredNativeSymbol", Encoding::Native)?;
    let class = crate::memory::scalar_string(class)?;
    crate::attributes::install_attribute(address, R_ClassSymbol.get(), class)?;
    address.set_object(true);
    let info = runtime.heap.alloc_vector(SexpType::List, 4)?;
    runtime.heap.root(info);
    let name_value =
        crate::memory::scalar_string(crate::strings::mk_char(name.as_bytes(), Encoding::Utf8)?)?;
    runtime.heap.set_element(info, 0, name_value);
    runtime.heap.set_element(info, 1, address);
    let dll = runtime
        .dlls
        .iter_mut()
        .rev()
        .find(|dll| dll.name == package)
        .map(|dll| &mut **dll as *mut Dll)
        .ok_or_else(|| error(format!("package {package} is not loaded")))?;
    let dll_value = dll_info_object(dll)?;
    runtime.heap.set_element(info, 2, dll_value);
    runtime
        .heap
        .set_element(info, 3, crate::memory::scalar(SexpType::Integer, arity)?);
    let names = runtime.heap.alloc_vector(SexpType::String, 4)?;
    runtime.heap.root(names);
    for (index, label) in ["name", "address", "dll", "numParameters"]
        .iter()
        .enumerate()
    {
        let label = runtime.heap.mk_char(label.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(names, index, label);
    }
    crate::attributes::install_attribute(info, R_NamesSymbol.get(), names)?;
    let class_name = match kind {
        RoutineKind::Call => "CallRoutine",
        RoutineKind::External => "ExternalRoutine",
        RoutineKind::C => "CRoutine",
        RoutineKind::Fortran => "FortranRoutine",
    };
    let classes = runtime.heap.alloc_vector(SexpType::String, 3)?;
    runtime.heap.root(classes);
    for (index, label) in [class_name, "NativeSymbolInfo", "RegisteredNativeSymbol"]
        .iter()
        .enumerate()
    {
        let label = runtime.heap.mk_char(label.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(classes, index, label);
    }
    crate::attributes::install_attribute(info, R_ClassSymbol.get(), classes)?;
    info.set_object(true);
    Ok(info)
}

/// `ry_native_symbol(name, package)`: `native_symbol_info` for hosts and tests, or `NULL` when
/// the package is not loaded or does not register the routine. A host calls it outside any R
/// context, where raising an error would abort the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_native_symbol(name: *const c_char, package: *const c_char) -> Sexp {
    let name = String::from_utf8_lossy(c_bytes(name));
    let package = String::from_utf8_lossy(c_bytes(package));
    if registered_routine(&name, &package).is_none() {
        return runtime().nil();
    }
    enter(|| native_symbol_info(&name, &package))
}
