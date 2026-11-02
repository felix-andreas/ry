//! The native routines of GNU R's own default packages (methods, utils, stats, grDevices,
//! graphics, tools) and of parallel, implemented in Rust.
//!
//! Those packages' R code loads from the GNU R installation, but their shared libraries cannot:
//! GNU R builds them against its internal headers, so they read GNU R's object layout directly
//! instead of going through the C API. When `dyn.load` is asked for one of them, it gets a
//! library record whose routines are the ones below. The names and arities are GNU R's, so
//! `useDynLib` binds every `C_` symbol the package's R code refers to; a routine without a Rust
//! implementation yet raises an error naming itself when called.

use {
    crate::{
        builtins::{Arg, Call},
        interp,
        unwind::{R, error_in},
        values::{self, length, nil, symbols},
    },
    heap::object::{Sexp, SexpType},
    std::sync::OnceLock,
};

/// A routine's implementation. It receives the arguments after `.NAME`, without `PACKAGE` and
/// the other control arguments; a `.C` or `.Fortran` routine returns the list of its (modified)
/// arguments, as those interfaces do.
pub type NativeFn = unsafe fn(&NativeCall<'_>) -> R<Sexp>;

pub struct NativeCall<'a> {
    /// The `.Call` (or `.External`, ...) call, for error messages. It is `NULL` while a
    /// graphics device replays its display list.
    pub call: Sexp,
    pub args: &'a [Arg],
    pub env: Sexp,
    /// The foreign call itself, whose primitive and arguments a graphics routine records on
    /// the display list.
    pub foreign: &'a Call<'a>,
}

impl NativeCall<'_> {
    pub fn arg(&self, index: usize) -> Sexp {
        self.args.get(index).map_or_else(nil, |arg| arg.value)
    }

    /// An error from inside the routine. GNU R reports it against the closure that made the
    /// foreign call, not the `.Call` itself, because `error()` skips the builtin's context.
    pub fn error(&self, message: impl Into<String>) -> crate::unwind::Unwind {
        // SAFETY: the frame stack holds live calls.
        error_in(unsafe { crate::unwind::current_call() }, message)
    }

    /// A warning from inside the routine, reported against the calling closure as `error` is.
    pub unsafe fn warning(&self, message: impl Into<String>) -> R<()> {
        crate::conditions::warning_call(crate::unwind::current_call(), message)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoutineKind {
    C,
    Call,
    Fortran,
    External,
}

/// Whether `path` is the shared library of one of the packages above, and which.
pub fn builtin_library(path: &str) -> Option<&'static str> {
    let path = std::path::Path::new(path);
    let file = path.file_stem()?.to_str()?;
    let package = PACKAGES.iter().find(|package| **package == file)?;
    let mut libs = path.parent()?;
    // GNU R for Windows keeps a package's DLL under `libs/x64`.
    if cfg!(windows) && libs.file_name()? == "x64" {
        libs = libs.parent()?;
    }
    let package_dir = libs.parent()?;
    let matches = libs.file_name()? == "libs" && package_dir.file_name()?.to_str()? == *package;
    matches.then_some(*package)
}

/// `dyn.load` of a builtin library: records it as loaded and returns its `DLLInfo`.
pub unsafe fn load(package: &'static str, path: &str) -> R<Sexp> {
    let loaded = &mut interp().builtin_libraries;
    if !loaded.iter().any(|(name, _)| *name == package) {
        loaded.push((package, path.to_owned()));
    }
    dll_info(package, path)
}

/// The `DLLInfo` records of the builtin libraries loaded so far, with their names.
pub unsafe fn loaded() -> R<Vec<(String, Sexp)>> {
    let libraries = interp().builtin_libraries.clone();
    let mut result = Vec::with_capacity(libraries.len());
    for (package, path) in libraries {
        result.push((package.to_owned(), dll_info(package, &path)?));
    }
    Ok(result)
}

/// `getRegisteredRoutines` for a builtin library's `DLLInfo` or its `info` pointer; `None` for
/// any other library.
pub unsafe fn registered_routines(info: Sexp) -> R<Option<Sexp>> {
    let Some(package) = library_of(info) else {
        return Ok(None);
    };
    let dll = match interp()
        .builtin_libraries
        .iter()
        .find(|(name, _)| *name == package)
    {
        Some((_, path)) => dll_info(package, &path.clone())?,
        None => dll_info(package, package)?,
    };
    let mut tables = Vec::new();
    for (kind, class) in [
        (RoutineKind::C, "CRoutine"),
        (RoutineKind::Call, "CallRoutine"),
        (RoutineKind::Fortran, "FortranRoutine"),
        (RoutineKind::External, "ExternalRoutine"),
    ] {
        let routines: Vec<&(&str, RoutineKind, &str, i32)> = routines()
            .filter(|routine| routine.0 == package && routine.1 == kind)
            .collect();
        let mut entries = Vec::with_capacity(routines.len());
        for routine in &routines {
            entries.push(native_symbol(routine, dll, class)?);
        }
        let table = values::list_vector(&entries)?;
        let names: Vec<&str> = routines.iter().map(|routine| routine.2).collect();
        let names = values::string_vector(&names)?;
        values::set_attr(table, symbols().names, names)?;
        let list_class = values::scalar_string("NativeRoutineList")?;
        values::set_attr(table, symbols().class, list_class)?;
        tables.push(table);
    }
    let result = values::list_vector(&tables)?;
    let names = values::string_vector(&[".C", ".Call", ".Fortran", ".External"])?;
    values::set_attr(result, symbols().names, names)?;
    Ok(Some(result))
}

/// `getSymbolInfo(name, package)` for a routine of a loaded builtin library.
pub unsafe fn symbol_info(name: &str, package: Option<&str>) -> R<Option<Sexp>> {
    let Some(routine) = find(name, package) else {
        return Ok(None);
    };
    let path = interp()
        .builtin_libraries
        .iter()
        .find(|(loaded, _)| *loaded == routine.0)
        .map_or_else(|| routine.0.to_owned(), |(_, path)| path.clone());
    let dll = dll_info(routine.0, &path)?;
    let class = match routine.1 {
        RoutineKind::C => "CRoutine",
        RoutineKind::Call => "CallRoutine",
        RoutineKind::Fortran => "FortranRoutine",
        RoutineKind::External => "ExternalRoutine",
    };
    Ok(Some(native_symbol(routine, dll, class)?))
}

/// Runs a foreign call whose `.NAME` is a routine of a builtin library. `None` means `.NAME`
/// is something else, which the caller resolves as native code.
pub unsafe fn try_call(foreign: &Call<'_>) -> R<Option<Sexp>> {
    let (call, args, env) = (foreign.call, foreign.args, foreign.env);
    let Some((first, rest)) = args.split_first() else {
        return Ok(None);
    };
    let control = ["PACKAGE", "NAOK", "DUP", "ENCODING"];
    let mut package = None;
    let mut routine_args = Vec::with_capacity(rest.len());
    for arg in rest {
        let tag = arg.tag;
        if tag.kind() == SexpType::Symbol && control.contains(&&*values::symbol_name(tag)) {
            if &*values::symbol_name(tag) == "PACKAGE" {
                package = values::first_string(arg.value).map(|text| text.into_owned());
            }
            continue;
        }
        routine_args.push(Arg {
            tag: arg.tag,
            value: arg.value,
        });
    }
    let routine = match first.value.kind() {
        SexpType::String => {
            let Some(name) = values::first_string(first.value) else {
                return Ok(None);
            };
            let package = package.as_deref();
            let loaded = |routine: &&(&str, RoutineKind, &str, i32)| {
                interp()
                    .builtin_libraries
                    .iter()
                    .any(|(name, _)| *name == routine.0)
            };
            match routines().filter(loaded).find(|routine| {
                routine.2 == &*name && package.is_none_or(|package| package == routine.0)
            }) {
                Some(routine) => routine,
                None => return Ok(None),
            }
        }
        SexpType::List => match routine_of(values::list_get(first.value, "address")) {
            Some(routine) => routine,
            None => return Ok(None),
        },
        SexpType::ExternalPtr => match routine_of(first.value) {
            Some(routine) => routine,
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    let native = NativeCall {
        call,
        args: &routine_args,
        env,
        foreign,
    };
    if routine.3 >= 0 && routine.3 as usize != routine_args.len() {
        return Err(error_in(
            call,
            format!(
                "Incorrect number of arguments ({}), expecting {} for '{}'",
                routine_args.len(),
                routine.3,
                routine.2
            ),
        ));
    }
    match implementation_of(routine) {
        Some(function) => function(&native).map(Some),
        None => Err(native.error(format!(
            "the native routine '{}' of package '{}' is not implemented in ry yet",
            routine.2, routine.0
        ))),
    }
}

/// The result of a `.C` or `.Fortran` routine: its arguments, with the values it wrote, named by
/// the call's tags.
pub unsafe fn c_result(call: &NativeCall<'_>, values_out: &[Sexp]) -> R<Sexp> {
    let result = values::list_vector(values_out)?;
    if call.args.iter().any(|arg| arg.tag != nil()) {
        let names: Vec<String> = call
            .args
            .iter()
            .map(|arg| {
                if arg.tag.kind() == SexpType::Symbol {
                    values::symbol_name(arg.tag).into_owned()
                } else {
                    String::new()
                }
            })
            .collect();
        let names = values::string_vector(&names)?;
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

const PACKAGES: [&str; 7] = [
    "methods",
    "utils",
    "stats",
    "grDevices",
    "graphics",
    "tools",
    "parallel",
];

const LIBRARY_MARKER: &str = "ry builtin library";
const ROUTINE_MARKER: &str = "ry builtin routine";

fn implementation(package: &str, name: &str) -> Option<NativeFn> {
    let tables: &[&[(&str, NativeFn)]] = match package {
        "methods" => &[crate::methods::ROUTINES],
        "stats" => &[crate::stats::ROUTINES, crate::distributions::ROUTINES],
        "utils" => &[crate::utils::ROUTINES],
        "tools" => &[crate::tools::ROUTINES],
        "parallel" => &[crate::parallel::ROUTINES],
        "grDevices" => &[crate::graphics::GRDEVICES_ROUTINES],
        "graphics" => &[crate::graphics::GRAPHICS_ROUTINES],
        _ => &[],
    };
    tables
        .iter()
        .flat_map(|table| table.iter())
        .find(|(routine, _)| *routine == name)
        .map(|(_, function)| *function)
}

/// The implementation of `routine`, resolved once per routine rather than searched by name on
/// every call, because hot code such as `sd` reaches its routine through `.Call` each time.
fn implementation_of(routine: &'static Routine) -> Option<NativeFn> {
    static IMPLEMENTATIONS: OnceLock<Vec<Option<NativeFn>>> = OnceLock::new();
    let implementations = IMPLEMENTATIONS.get_or_init(|| {
        routines()
            .map(|routine| implementation(routine.0, routine.2))
            .collect()
    });
    match routine_index(routine) {
        Some(index) => implementations.get(index).copied().flatten(),
        None => implementation(routine.0, routine.2),
    }
}

/// The position of `routine` in `routines()`, if it is one of its entries.
fn routine_index(routine: *const Routine) -> Option<usize> {
    let mut offset = 0;
    for table in [ROUTINES, if cfg!(windows) { WINDOWS_ROUTINES } else { &[] }] {
        let range = table.as_ptr_range();
        if range.contains(&routine) {
            // In range of a slice of `Routine`s, so the byte distance is a whole number of them.
            return Some(offset + (routine as usize - range.start as usize) / size_of::<Routine>());
        }
        offset += table.len();
    }
    None
}

fn find(
    name: &str,
    package: Option<&str>,
) -> Option<&'static (&'static str, RoutineKind, &'static str, i32)> {
    routines()
        .find(|routine| routine.2 == name && package.is_none_or(|package| package == routine.0))
}

/// The package of a builtin library's `DLLInfo` or `info` pointer.
unsafe fn library_of(info: Sexp) -> Option<&'static str> {
    let pointer = match info.kind() {
        SexpType::ExternalPtr => info,
        SexpType::List => values::list_get(info, "info"),
        _ => return None,
    };
    if pointer.kind() != SexpType::ExternalPtr || !is_marker(pointer.external_tag(), LIBRARY_MARKER)
    {
        return None;
    }
    let package = values::first_string(pointer.external_protected())?;
    PACKAGES
        .iter()
        .find(|candidate| **candidate == &*package)
        .copied()
}

/// The routine a `RegisteredNativeSymbol` pointer refers to, if it is a builtin one.
unsafe fn routine_of(
    address: Sexp,
) -> Option<&'static (&'static str, RoutineKind, &'static str, i32)> {
    if address.kind() != SexpType::ExternalPtr || !is_marker(address.external_tag(), ROUTINE_MARKER)
    {
        return None;
    }
    // The address is the routine's table entry, which saves a search by name. A pointer that was
    // serialized comes back null, and then the names identify the routine.
    let entry = address.external_address() as *const Routine;
    if routine_index(entry).is_some() {
        return Some(&*entry);
    }
    let identity = address.external_protected();
    if identity.kind() != SexpType::String || length(identity) != 2 {
        return None;
    }
    let package = values::string_at(identity, 0)?;
    let name = values::string_at(identity, 1)?;
    find(&name, Some(&package))
}

unsafe fn is_marker(tag: Sexp, marker: &str) -> bool {
    tag.kind() == SexpType::Symbol && values::symbol_name(tag) == marker
}

unsafe fn dll_info(package: &str, path: &str) -> R<Sexp> {
    let info = crate::native::dll_info(package, path)?;
    let pointer = values::list_get(info, "info");
    let marker = values::install(LIBRARY_MARKER)?;
    crate::heap().set_external_tag(pointer, marker);
    let identity = values::scalar_string(package)?;
    crate::heap().set_external_protected(pointer, identity);
    Ok(info)
}

unsafe fn native_symbol(routine: &'static Routine, dll: Sexp, class: &str) -> R<Sexp> {
    let identity = values::string_vector(&[routine.0, routine.2])?;
    let marker = values::install(ROUTINE_MARKER)?;
    let entry = routine as *const Routine as *mut std::ffi::c_void;
    let address = crate::heap().alloc_external_ptr(entry, marker, identity)?;
    crate::heap().root(address);
    let address_class = values::scalar_string("RegisteredNativeSymbol")?;
    values::set_attr(address, symbols().class, address_class)?;
    let entry = values::named_list(&[
        ("name", values::scalar_string(routine.2)?),
        ("address", address),
        ("dll", dll),
        ("numParameters", values::scalar_integer(routine.3)?),
    ])?;
    let entry_class = values::string_vector(&[class, "NativeSymbolInfo"])?;
    values::set_attr(entry, symbols().class, entry_class)?;
    Ok(entry)
}

type Routine = (&'static str, RoutineKind, &'static str, i32);

/// The routines the default packages register on this platform.
fn routines() -> impl Iterator<Item = &'static Routine> {
    let windows: &'static [Routine] = if cfg!(windows) { WINDOWS_ROUTINES } else { &[] };
    ROUTINES.iter().chain(windows)
}

/// The routines only GNU R for Windows registers. Its `utils` and `grDevices` R code refers to
/// them, so they must exist for those namespaces to load there; the ones without an
/// implementation fail when called.
const WINDOWS_ROUTINES: &[Routine] = &[
    ("utils", RoutineKind::Call, "dllversion", 1),
    ("utils", RoutineKind::Call, "chooseDir", 2),
    ("utils", RoutineKind::Call, "chooseFiles", 5),
    ("utils", RoutineKind::Call, "getClipboardFormats", 0),
    ("utils", RoutineKind::Call, "getIdentification", 0),
    ("utils", RoutineKind::Call, "getWindowTitle", 0),
    ("utils", RoutineKind::Call, "getWindowsHandle", 1),
    ("utils", RoutineKind::Call, "getWindowsHandles", 2),
    ("utils", RoutineKind::Call, "loadRconsole", 1),
    ("utils", RoutineKind::Call, "readClipboard", 2),
    ("utils", RoutineKind::Call, "setStatusBar", 1),
    ("utils", RoutineKind::Call, "setWindowTitle", 1),
    ("utils", RoutineKind::Call, "shortpath", 1),
    ("utils", RoutineKind::Call, "winver", 0),
    ("utils", RoutineKind::Call, "writeClipboard", 2),
    ("utils", RoutineKind::External, "arrangeWindows", 4),
    ("utils", RoutineKind::External, "closeWinProgressBar", 1),
    ("utils", RoutineKind::External, "readRegistry", 4),
    ("utils", RoutineKind::External, "setWinProgressBar", 4),
    ("utils", RoutineKind::External, "winDialog", 2),
    ("utils", RoutineKind::External, "winDialogString", 2),
    ("utils", RoutineKind::External, "winMenuAdd", 3),
    ("utils", RoutineKind::External, "winMenuDel", 2),
    ("utils", RoutineKind::External, "winMenuItems", 1),
    ("utils", RoutineKind::External, "winMenuNames", 0),
    ("utils", RoutineKind::External, "winProgressBar", 6),
    ("grDevices", RoutineKind::Call, "bringToTop", 2),
    ("grDevices", RoutineKind::Call, "msgWindow", 2),
    ("grDevices", RoutineKind::External, "devga", 21),
];

/// The registered routines of these packages: package, interface, name and arity. It is the
/// union over every supported GNU R (4.3 on), because each release's namespace refers only to
/// the routines it knows; an arity of -1 marks a routine whose argument list changed between
/// releases, whose implementation accepts each.
const ROUTINES: &[Routine] = &[
    ("parallel", RoutineKind::Call, "nextStream", 1),
    ("parallel", RoutineKind::Call, "nextSubStream", 1),
    ("parallel", RoutineKind::Call, "mc_children", 0),
    ("parallel", RoutineKind::Call, "mc_close_fds", 1),
    ("parallel", RoutineKind::Call, "mc_close_stderr", 1),
    ("parallel", RoutineKind::Call, "mc_close_stdout", 1),
    ("parallel", RoutineKind::Call, "mc_exit", 1),
    ("parallel", RoutineKind::Call, "mc_fds", 1),
    ("parallel", RoutineKind::Call, "mc_fork", 1),
    ("parallel", RoutineKind::Call, "mc_is_child", 0),
    ("parallel", RoutineKind::Call, "mc_kill", 2),
    ("parallel", RoutineKind::Call, "mc_master_fd", 0),
    ("parallel", RoutineKind::Call, "mc_read_child", 1),
    ("parallel", RoutineKind::Call, "mc_read_children", 1),
    ("parallel", RoutineKind::Call, "mc_rm_child", 1),
    ("parallel", RoutineKind::Call, "mc_send_master", 1),
    ("parallel", RoutineKind::Call, "mc_select_children", 2),
    ("parallel", RoutineKind::Call, "mc_send_child_stdin", 2),
    ("parallel", RoutineKind::Call, "mc_affinity", 1),
    ("parallel", RoutineKind::Call, "mc_interactive", 1),
    ("parallel", RoutineKind::Call, "mc_cleanup", 3),
    ("parallel", RoutineKind::Call, "mc_prepare_cleanup", 0),
    ("methods", RoutineKind::Call, "R_M_setPrimitiveMethods", 5),
    ("methods", RoutineKind::Call, "R_clear_method_selection", 0),
    ("methods", RoutineKind::Call, "R_dummy_extern_place", 0),
    ("methods", RoutineKind::Call, "R_el_named", 2),
    (
        "methods",
        RoutineKind::Call,
        "R_externalptr_prototype_object",
        0,
    ),
    ("methods", RoutineKind::Call, "R_getClassFromCache", 2),
    ("methods", RoutineKind::Call, "R_getGeneric", 4),
    ("methods", RoutineKind::Call, "R_get_slot", 2),
    ("methods", RoutineKind::Call, "R_hasSlot", 2),
    ("methods", RoutineKind::Call, "R_identC", 2),
    ("methods", RoutineKind::Call, "R_initMethodDispatch", 1),
    ("methods", RoutineKind::Call, "R_methodsPackageMetaName", 3),
    ("methods", RoutineKind::Call, "R_methods_test_MAKE_CLASS", 1),
    ("methods", RoutineKind::Call, "R_methods_test_NEW", 1),
    ("methods", RoutineKind::Call, "R_missingArg", 2),
    ("methods", RoutineKind::Call, "R_nextMethodCall", 2),
    ("methods", RoutineKind::Call, "R_quick_method_check", 3),
    ("methods", RoutineKind::Call, "R_selectMethod", 4),
    ("methods", RoutineKind::Call, "R_set_el_named", 3),
    ("methods", RoutineKind::Call, "R_set_slot", 3),
    ("methods", RoutineKind::Call, "R_standardGeneric", 3),
    ("methods", RoutineKind::Call, "do_substitute_direct", 2),
    ("methods", RoutineKind::Call, "Rf_allocS4Object", 0),
    ("methods", RoutineKind::Call, "R_set_method_dispatch", 1),
    ("methods", RoutineKind::Call, "R_get_primname", 1),
    ("methods", RoutineKind::Call, "new_object", 1),
    ("utils", RoutineKind::Call, "crc64", 1),
    ("utils", RoutineKind::Call, "flushconsole", 0),
    ("utils", RoutineKind::Call, "menu", 1),
    ("utils", RoutineKind::Call, "nsl", 1),
    ("utils", RoutineKind::Call, "objectSize", 1),
    ("utils", RoutineKind::Call, "processevents", 0),
    ("utils", RoutineKind::Call, "octsize", 1),
    ("utils", RoutineKind::Call, "charClass", 2),
    ("utils", RoutineKind::Call, "tzcode_type", 0),
    ("utils", RoutineKind::Call, "sockconnect", 2),
    ("utils", RoutineKind::Call, "sockread", 2),
    ("utils", RoutineKind::Call, "sockclose", 1),
    ("utils", RoutineKind::Call, "sockopen", 1),
    ("utils", RoutineKind::Call, "socklisten", 1),
    ("utils", RoutineKind::Call, "sockwrite", 2),
    ("utils", RoutineKind::External, "download", 6),
    ("utils", RoutineKind::External, "unzip", 7),
    ("utils", RoutineKind::External, "Rprof", -1),
    ("utils", RoutineKind::External, "Rprofmem", 3),
    ("utils", RoutineKind::External, "countfields", 6),
    ("utils", RoutineKind::External, "readtablehead", 7),
    ("utils", RoutineKind::External, "typeconvert", 6),
    ("utils", RoutineKind::External, "writetable", 11),
    ("utils", RoutineKind::External, "addhistory", 1),
    ("utils", RoutineKind::External, "loadhistory", 1),
    ("utils", RoutineKind::External, "savehistory", 1),
    ("utils", RoutineKind::External, "dataentry", 2),
    ("utils", RoutineKind::External, "dataviewer", 2),
    ("utils", RoutineKind::External, "edit", 4),
    ("utils", RoutineKind::External, "fileedit", 3),
    ("utils", RoutineKind::External, "selectlist", 4),
    ("utils", RoutineKind::External, "hashtab_Ext", 2),
    ("utils", RoutineKind::External, "gethash_Ext", 3),
    ("utils", RoutineKind::External, "sethash_Ext", 3),
    ("utils", RoutineKind::External, "remhash_Ext", 2),
    ("utils", RoutineKind::External, "numhash_Ext", 1),
    ("utils", RoutineKind::External, "typhash_Ext", 1),
    ("utils", RoutineKind::External, "maphash_Ext", 2),
    ("utils", RoutineKind::External, "clrhash_Ext", 1),
    ("utils", RoutineKind::External, "ishashtab_Ext", 1),
    ("stats", RoutineKind::C, "loess_raw", 24),
    ("stats", RoutineKind::C, "loess_dfit", 13),
    ("stats", RoutineKind::C, "loess_dfitse", 16),
    ("stats", RoutineKind::C, "loess_ifit", 8),
    ("stats", RoutineKind::C, "loess_ise", 15),
    ("stats", RoutineKind::C, "multi_burg", 11),
    ("stats", RoutineKind::C, "multi_yw", 10),
    ("stats", RoutineKind::C, "HoltWinters", 17),
    ("stats", RoutineKind::C, "kmeans_Lloyd", 9),
    ("stats", RoutineKind::C, "kmeans_MacQueen", 9),
    ("stats", RoutineKind::C, "rcont2", 8),
    ("stats", RoutineKind::Call, "cutree", 2),
    ("stats", RoutineKind::Call, "isoreg", 1),
    ("stats", RoutineKind::Call, "monoFC_m", 2),
    ("stats", RoutineKind::Call, "numeric_deriv", 6),
    ("stats", RoutineKind::Call, "nls_iter", 3),
    ("stats", RoutineKind::Call, "setup_starma", 8),
    ("stats", RoutineKind::Call, "free_starma", 1),
    ("stats", RoutineKind::Call, "set_trans", 2),
    ("stats", RoutineKind::Call, "arma0fa", 2),
    ("stats", RoutineKind::Call, "get_s2", 1),
    ("stats", RoutineKind::Call, "get_resid", 1),
    ("stats", RoutineKind::Call, "Dotrans", 2),
    ("stats", RoutineKind::Call, "arma0_kfore", 4),
    ("stats", RoutineKind::Call, "Starma_method", 2),
    ("stats", RoutineKind::Call, "Invtrans", 2),
    ("stats", RoutineKind::Call, "Gradtrans", 2),
    ("stats", RoutineKind::Call, "ARMAtoMA", 3),
    ("stats", RoutineKind::Call, "KalmanLike", 5),
    ("stats", RoutineKind::Call, "KalmanFore", 3),
    ("stats", RoutineKind::Call, "KalmanSmooth", 3),
    ("stats", RoutineKind::Call, "ARIMA_undoPars", 2),
    ("stats", RoutineKind::Call, "ARIMA_transPars", 3),
    ("stats", RoutineKind::Call, "ARIMA_Invtrans", 2),
    ("stats", RoutineKind::Call, "ARIMA_Gradtrans", 2),
    ("stats", RoutineKind::Call, "ARIMA_Like", 4),
    ("stats", RoutineKind::Call, "ARIMA_CSS", 6),
    ("stats", RoutineKind::Call, "TSconv", 2),
    ("stats", RoutineKind::Call, "getQ0", 2),
    ("stats", RoutineKind::Call, "getQ0bis", 3),
    ("stats", RoutineKind::Call, "port_ivset", 3),
    ("stats", RoutineKind::Call, "port_nlminb", 9),
    ("stats", RoutineKind::Call, "port_nlsb", 7),
    ("stats", RoutineKind::Call, "logit_link", 1),
    ("stats", RoutineKind::Call, "logit_linkinv", 1),
    ("stats", RoutineKind::Call, "logit_mu_eta", 1),
    ("stats", RoutineKind::Call, "binomial_dev_resids", 3),
    ("stats", RoutineKind::Call, "rWishart", 3),
    ("stats", RoutineKind::Call, "Cdqrls", 4),
    ("stats", RoutineKind::Call, "Cdist", 4),
    ("stats", RoutineKind::Call, "cor", 4),
    ("stats", RoutineKind::Call, "cov", 4),
    ("stats", RoutineKind::Call, "updateform", 2),
    ("stats", RoutineKind::Call, "fft", 2),
    ("stats", RoutineKind::Call, "mvfft", 2),
    ("stats", RoutineKind::Call, "nextn", 2),
    ("stats", RoutineKind::Call, "r2dtable", 3),
    ("stats", RoutineKind::Call, "cfilter", 4),
    ("stats", RoutineKind::Call, "rfilter", 3),
    ("stats", RoutineKind::Call, "lowess", 5),
    ("stats", RoutineKind::Call, "DoubleCentre", 1),
    ("stats", RoutineKind::Call, "BinDist", 5),
    ("stats", RoutineKind::Call, "Rsm", 3),
    ("stats", RoutineKind::Call, "tukeyline", 4),
    ("stats", RoutineKind::Call, "runmed", 6),
    ("stats", RoutineKind::Call, "influence", 3),
    ("stats", RoutineKind::Call, "pKolmogorov2x", 2),
    ("stats", RoutineKind::Call, "pKS2", 2),
    ("stats", RoutineKind::Call, "pkolmogorov_two_exact", 2),
    ("stats", RoutineKind::Call, "pkolmogorov_two_limit", 3),
    ("stats", RoutineKind::Call, "dpermdist1", 1),
    ("stats", RoutineKind::Call, "dpermdist2", 2),
    ("stats", RoutineKind::Call, "psmirnov_exact", 6),
    ("stats", RoutineKind::Call, "ksmooth", 5),
    ("stats", RoutineKind::Call, "SplineCoef", 3),
    ("stats", RoutineKind::Call, "SplineEval", 2),
    ("stats", RoutineKind::Call, "Approx", 8),
    ("stats", RoutineKind::Call, "ApproxTest", 5),
    ("stats", RoutineKind::Call, "LogLin", 7),
    ("stats", RoutineKind::Call, "pAnsari", 3),
    ("stats", RoutineKind::Call, "qAnsari", 3),
    ("stats", RoutineKind::Call, "pKendall", 2),
    ("stats", RoutineKind::Call, "pRho", 3),
    ("stats", RoutineKind::Call, "SWilk", 1),
    ("stats", RoutineKind::Call, "bw_den", 2),
    ("stats", RoutineKind::Call, "bw_den_binned", 1),
    ("stats", RoutineKind::Call, "bw_ucv", 4),
    ("stats", RoutineKind::Call, "bw_bcv", 4),
    ("stats", RoutineKind::Call, "bw_phi4", 4),
    ("stats", RoutineKind::Call, "bw_phi6", 4),
    ("stats", RoutineKind::Call, "acf", 3),
    ("stats", RoutineKind::Call, "pacf1", 2),
    ("stats", RoutineKind::Call, "ar2ma", 2),
    ("stats", RoutineKind::Call, "Burg", 2),
    ("stats", RoutineKind::Call, "intgrt_vec", 3),
    ("stats", RoutineKind::Call, "pp_sum", 2),
    ("stats", RoutineKind::Call, "Fexact", 4),
    ("stats", RoutineKind::Call, "Fisher_sim", 3),
    ("stats", RoutineKind::Call, "chisq_sim", 4),
    ("stats", RoutineKind::Call, "d2x2xk", 5),
    ("stats", RoutineKind::Call, "Smirnov_sim", 4),
    ("stats", RoutineKind::Call, "dchisq", 3),
    ("stats", RoutineKind::Call, "dexp", 3),
    ("stats", RoutineKind::Call, "dgeom", 3),
    ("stats", RoutineKind::Call, "dpois", 3),
    ("stats", RoutineKind::Call, "dt", 3),
    ("stats", RoutineKind::Call, "dsignrank", 3),
    ("stats", RoutineKind::Call, "pchisq", 4),
    ("stats", RoutineKind::Call, "qchisq", 4),
    ("stats", RoutineKind::Call, "pexp", 4),
    ("stats", RoutineKind::Call, "qexp", 4),
    ("stats", RoutineKind::Call, "pgeom", 4),
    ("stats", RoutineKind::Call, "qgeom", 4),
    ("stats", RoutineKind::Call, "ppois", 4),
    ("stats", RoutineKind::Call, "qpois", 4),
    ("stats", RoutineKind::Call, "pt", 4),
    ("stats", RoutineKind::Call, "qt", 4),
    ("stats", RoutineKind::Call, "psignrank", 4),
    ("stats", RoutineKind::Call, "qsignrank", 4),
    ("stats", RoutineKind::Call, "dbeta", 4),
    ("stats", RoutineKind::Call, "dbinom", 4),
    ("stats", RoutineKind::Call, "dcauchy", 4),
    ("stats", RoutineKind::Call, "df", 4),
    ("stats", RoutineKind::Call, "dgamma", 4),
    ("stats", RoutineKind::Call, "dlnorm", 4),
    ("stats", RoutineKind::Call, "dlogis", 4),
    ("stats", RoutineKind::Call, "dnbinom", 4),
    ("stats", RoutineKind::Call, "dnbinom_mu", 4),
    ("stats", RoutineKind::Call, "dnorm", 4),
    ("stats", RoutineKind::Call, "dweibull", 4),
    ("stats", RoutineKind::Call, "dunif", 4),
    ("stats", RoutineKind::Call, "dnt", 4),
    ("stats", RoutineKind::Call, "dnchisq", 4),
    ("stats", RoutineKind::Call, "dwilcox", 4),
    ("stats", RoutineKind::Call, "pbeta", 5),
    ("stats", RoutineKind::Call, "qbeta", 5),
    ("stats", RoutineKind::Call, "pbinom", 5),
    ("stats", RoutineKind::Call, "qbinom", 5),
    ("stats", RoutineKind::Call, "pcauchy", 5),
    ("stats", RoutineKind::Call, "qcauchy", 5),
    ("stats", RoutineKind::Call, "pf", 5),
    ("stats", RoutineKind::Call, "qf", 5),
    ("stats", RoutineKind::Call, "pgamma", 5),
    ("stats", RoutineKind::Call, "qgamma", 5),
    ("stats", RoutineKind::Call, "plnorm", 5),
    ("stats", RoutineKind::Call, "qlnorm", 5),
    ("stats", RoutineKind::Call, "plogis", 5),
    ("stats", RoutineKind::Call, "qlogis", 5),
    ("stats", RoutineKind::Call, "pnbinom", 5),
    ("stats", RoutineKind::Call, "qnbinom", 5),
    ("stats", RoutineKind::Call, "pnbinom_mu", 5),
    ("stats", RoutineKind::Call, "qnbinom_mu", 5),
    ("stats", RoutineKind::Call, "pnorm", 5),
    ("stats", RoutineKind::Call, "qnorm", 5),
    ("stats", RoutineKind::Call, "pweibull", 5),
    ("stats", RoutineKind::Call, "qweibull", 5),
    ("stats", RoutineKind::Call, "punif", 5),
    ("stats", RoutineKind::Call, "qunif", 5),
    ("stats", RoutineKind::Call, "pnt", 5),
    ("stats", RoutineKind::Call, "qnt", 5),
    ("stats", RoutineKind::Call, "pnchisq", 5),
    ("stats", RoutineKind::Call, "qnchisq", 5),
    ("stats", RoutineKind::Call, "pwilcox", 5),
    ("stats", RoutineKind::Call, "qwilcox", 5),
    ("stats", RoutineKind::Call, "dhyper", 5),
    ("stats", RoutineKind::Call, "dnbeta", 5),
    ("stats", RoutineKind::Call, "dnf", 5),
    ("stats", RoutineKind::Call, "phyper", 6),
    ("stats", RoutineKind::Call, "qhyper", 6),
    ("stats", RoutineKind::Call, "pnbeta", 6),
    ("stats", RoutineKind::Call, "qnbeta", 6),
    ("stats", RoutineKind::Call, "pnf", 6),
    ("stats", RoutineKind::Call, "qnf", 6),
    ("stats", RoutineKind::Call, "ptukey", 6),
    ("stats", RoutineKind::Call, "qtukey", 6),
    ("stats", RoutineKind::Call, "rchisq", 2),
    ("stats", RoutineKind::Call, "rexp", 2),
    ("stats", RoutineKind::Call, "rgeom", 2),
    ("stats", RoutineKind::Call, "rpois", 2),
    ("stats", RoutineKind::Call, "rt", 2),
    ("stats", RoutineKind::Call, "rsignrank", 2),
    ("stats", RoutineKind::Call, "rbeta", 3),
    ("stats", RoutineKind::Call, "rbinom", 3),
    ("stats", RoutineKind::Call, "rcauchy", 3),
    ("stats", RoutineKind::Call, "rf", 3),
    ("stats", RoutineKind::Call, "rgamma", 3),
    ("stats", RoutineKind::Call, "rlnorm", 3),
    ("stats", RoutineKind::Call, "rlogis", 3),
    ("stats", RoutineKind::Call, "rnbinom", 3),
    ("stats", RoutineKind::Call, "rnorm", 3),
    ("stats", RoutineKind::Call, "runif", 3),
    ("stats", RoutineKind::Call, "rweibull", 3),
    ("stats", RoutineKind::Call, "rwilcox", 3),
    ("stats", RoutineKind::Call, "rnchisq", 3),
    ("stats", RoutineKind::Call, "rnbinom_mu", 3),
    ("stats", RoutineKind::Call, "rhyper", 4),
    ("stats", RoutineKind::Call, "rmultinom", 3),
    ("stats", RoutineKind::Fortran, "lowesw", 4),
    ("stats", RoutineKind::Fortran, "lowesp", 7),
    ("stats", RoutineKind::Fortran, "setppr", 6),
    ("stats", RoutineKind::Fortran, "smart", 16),
    ("stats", RoutineKind::Fortran, "pppred", 5),
    ("stats", RoutineKind::Fortran, "setsmu", 1),
    ("stats", RoutineKind::Fortran, "rbart", 20),
    ("stats", RoutineKind::Fortran, "bvalus", 7),
    ("stats", RoutineKind::Fortran, "supsmu", 10),
    ("stats", RoutineKind::Fortran, "hclust", 10),
    ("stats", RoutineKind::Fortran, "hcass2", 6),
    ("stats", RoutineKind::Fortran, "kmns", 17),
    ("stats", RoutineKind::Fortran, "eureka", 6),
    ("stats", RoutineKind::Fortran, "stl", -1),
    ("stats", RoutineKind::External, "compcases", -1),
    ("stats", RoutineKind::External, "doD", 2),
    ("stats", RoutineKind::External, "deriv", 5),
    ("stats", RoutineKind::External, "modelframe", 8),
    ("stats", RoutineKind::External, "modelmatrix", 2),
    ("stats", RoutineKind::External, "termsform", 5),
    ("stats", RoutineKind::External, "do_fmin", 4),
    ("stats", RoutineKind::External, "nlm", 11),
    ("stats", RoutineKind::External, "zeroin2", 7),
    ("stats", RoutineKind::External, "optim", 7),
    ("stats", RoutineKind::External, "optimhess", 4),
    ("stats", RoutineKind::External, "call_dqags", 7),
    ("stats", RoutineKind::External, "call_dqagi", 7),
    ("stats", RoutineKind::External, "signrank_free", 0),
    ("stats", RoutineKind::External, "wilcox_free", 0),
    ("grDevices", RoutineKind::Call, "Type1FontInUse", 2),
    ("grDevices", RoutineKind::Call, "CIDFontInUse", 2),
    ("grDevices", RoutineKind::Call, "R_CreateAtVector", 4),
    ("grDevices", RoutineKind::Call, "R_GAxisPars", 3),
    ("grDevices", RoutineKind::Call, "chull", 1),
    ("grDevices", RoutineKind::Call, "gray", 2),
    ("grDevices", RoutineKind::Call, "RGB2hsv", 1),
    ("grDevices", RoutineKind::Call, "rgb", 6),
    ("grDevices", RoutineKind::Call, "hsv", 4),
    ("grDevices", RoutineKind::Call, "hcl", 5),
    ("grDevices", RoutineKind::Call, "col2rgb", 2),
    ("grDevices", RoutineKind::Call, "colors", 0),
    ("grDevices", RoutineKind::Call, "palette", 1),
    ("grDevices", RoutineKind::Call, "palette2", 1),
    ("grDevices", RoutineKind::Call, "cairoVersion", 0),
    ("grDevices", RoutineKind::Call, "pangoVersion", 0),
    ("grDevices", RoutineKind::Call, "cairoFT", 0),
    ("grDevices", RoutineKind::Call, "bmVersion", 0),
    ("grDevices", RoutineKind::Call, "makeQuartzDefault", 0),
    ("grDevices", RoutineKind::Call, "cairoProps", 1),
    ("grDevices", RoutineKind::External, "PicTeX", 6),
    ("grDevices", RoutineKind::External, "PostScript", 19),
    ("grDevices", RoutineKind::External, "XFig", 14),
    ("grDevices", RoutineKind::External, "PDF", -1),
    ("grDevices", RoutineKind::External, "devCairo", 12),
    ("grDevices", RoutineKind::External, "devcap", 1),
    ("grDevices", RoutineKind::External, "devcapture", 1),
    ("grDevices", RoutineKind::External, "devcontrol", 1),
    ("grDevices", RoutineKind::External, "devcopy", 1),
    ("grDevices", RoutineKind::External, "devcur", 0),
    ("grDevices", RoutineKind::External, "devdisplaylist", 0),
    ("grDevices", RoutineKind::External, "devholdflush", 1),
    ("grDevices", RoutineKind::External, "devnext", 1),
    ("grDevices", RoutineKind::External, "devoff", 1),
    ("grDevices", RoutineKind::External, "devprev", 1),
    ("grDevices", RoutineKind::External, "devset", 1),
    ("grDevices", RoutineKind::External, "devsize", 0),
    ("grDevices", RoutineKind::External, "contourLines", 4),
    ("grDevices", RoutineKind::External, "getSnapshot", 0),
    ("grDevices", RoutineKind::External, "playSnapshot", 1),
    ("grDevices", RoutineKind::External, "getGraphicsEvent", 1),
    ("grDevices", RoutineKind::External, "getGraphicsEventEnv", 1),
    ("grDevices", RoutineKind::External, "setGraphicsEventEnv", 2),
    ("grDevices", RoutineKind::External, "setPattern", 1),
    ("grDevices", RoutineKind::External, "setClipPath", 2),
    ("grDevices", RoutineKind::External, "setMask", 2),
    ("grDevices", RoutineKind::External, "defineGroup", 3),
    ("grDevices", RoutineKind::External, "useGroup", 2),
    ("grDevices", RoutineKind::External, "devUp", 0),
    ("grDevices", RoutineKind::External, "devAskNewPage", 1),
    ("grDevices", RoutineKind::External, "savePlot", 3),
    ("grDevices", RoutineKind::External, "Quartz", 11),
    ("grDevices", RoutineKind::External, "X11", 18),
    ("graphics", RoutineKind::Call, "C_contourDef", 0),
    ("graphics", RoutineKind::Call, "C_StemLeaf", 4),
    ("graphics", RoutineKind::Call, "C_BinCount", 4),
    ("graphics", RoutineKind::Call, "RunregisterBase", 0),
    ("graphics", RoutineKind::External, "C_contour", -1),
    ("graphics", RoutineKind::External, "C_filledcontour", 5),
    ("graphics", RoutineKind::External, "C_image", 4),
    ("graphics", RoutineKind::External, "C_persp", -1),
    ("graphics", RoutineKind::External, "C_abline", -1),
    ("graphics", RoutineKind::External, "C_axis", -1),
    ("graphics", RoutineKind::External, "C_arrows", -1),
    ("graphics", RoutineKind::External, "C_box", -1),
    ("graphics", RoutineKind::External, "C_clip", -1),
    ("graphics", RoutineKind::External, "C_convertX", 3),
    ("graphics", RoutineKind::External, "C_convertY", 3),
    ("graphics", RoutineKind::External, "C_dend", -1),
    ("graphics", RoutineKind::External, "C_dendwindow", -1),
    ("graphics", RoutineKind::External, "C_erase", -1),
    ("graphics", RoutineKind::External, "C_layout", -1),
    ("graphics", RoutineKind::External, "C_mtext", -1),
    ("graphics", RoutineKind::External, "C_par", -1),
    ("graphics", RoutineKind::External, "C_path", -1),
    ("graphics", RoutineKind::External, "C_plotXY", -1),
    ("graphics", RoutineKind::External, "C_plot_window", -1),
    ("graphics", RoutineKind::External, "C_polygon", -1),
    ("graphics", RoutineKind::External, "C_raster", -1),
    ("graphics", RoutineKind::External, "C_rect", -1),
    ("graphics", RoutineKind::External, "C_segments", -1),
    ("graphics", RoutineKind::External, "C_strHeight", -1),
    ("graphics", RoutineKind::External, "C_strWidth", -1),
    ("graphics", RoutineKind::External, "C_symbols", -1),
    ("graphics", RoutineKind::External, "C_text", -1),
    ("graphics", RoutineKind::External, "C_title", -1),
    ("graphics", RoutineKind::External, "C_xspline", -1),
    ("graphics", RoutineKind::External, "C_plot_new", 0),
    ("graphics", RoutineKind::External, "C_locator", -1),
    ("graphics", RoutineKind::External, "C_identify", -1),
    ("tools", RoutineKind::Call, "codeFilesAppend", 2),
    ("tools", RoutineKind::Call, "delim_match", 2),
    ("tools", RoutineKind::Call, "dirchmod", 2),
    ("tools", RoutineKind::Call, "getfmts", 1),
    ("tools", RoutineKind::Call, "Rmd5", 1),
    ("tools", RoutineKind::Call, "Rsha256", 1),
    ("tools", RoutineKind::Call, "remove_dot_segments_wrapper", 1),
    ("tools", RoutineKind::Call, "check_nonASCII", 2),
    ("tools", RoutineKind::Call, "check_nonASCII2", 1),
    ("tools", RoutineKind::Call, "doTabExpand", 2),
    ("tools", RoutineKind::Call, "ps_kill", 2),
    ("tools", RoutineKind::Call, "ps_sigs", 1),
    ("tools", RoutineKind::Call, "ps_priority", 2),
    ("tools", RoutineKind::Call, "startHTTPD", 2),
    ("tools", RoutineKind::Call, "stopHTTPD", 0),
    ("tools", RoutineKind::Call, "deparseRd", 2),
    ("tools", RoutineKind::Call, "splitString", 2),
    ("tools", RoutineKind::Call, "package_dependencies_scan", 1),
    ("tools", RoutineKind::Call, "nonASCII", 1),
    ("tools", RoutineKind::External, "parseLatex", -1),
    ("tools", RoutineKind::External, "parseRd", 9),
];
