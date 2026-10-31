//! The harness the native-package tests share. It loads ry's `libR` the way an R host would,
//! builds packages from C and C++ sources, loads them with `dyn.load` (which runs
//! `R_init_<pkg>`), and calls their routines through `.Call`, all through the exported C API.
//!
//! The runtime is one process-wide R session, so every test takes the session lock. Each test
//! binary is its own process, which keeps libraries built with different headers apart.

// Each test binary uses a different part of the harness, and the function table keeps the C
// API's own names so it reads like the C it binds.
#![allow(dead_code, non_snake_case)]

use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard, OnceLock},
};

pub type Sexp = *mut c_void;

pub const INTSXP: c_int = 13;
pub const REALSXP: c_int = 14;
pub const STRSXP: c_int = 16;
pub const VECSXP: c_int = 19;
pub const LGLSXP: c_int = 10;
pub const RAWSXP: c_int = 24;
pub const EXTPTRSXP: c_int = 22;

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct GcStats {
    pub minor_collections: u64,
    pub major_collections: u64,
    pub objects_freed: u64,
    pub live_objects: u64,
    pub live_bytes: u64,
}

macro_rules! api {
    ($($name:ident: fn($($argument:ty),*) $(-> $result:ty)?;)*) => {
        #[allow(non_snake_case)]
        pub struct Api {
            pub library: Library,
            $(pub $name: unsafe extern "C" fn($($argument),*) $(-> $result)?,)*
        }

        impl Api {
            unsafe fn load(library: Library) -> Api {
                unsafe {
                    $(
                        let $name = *library
                            .get::<unsafe extern "C" fn($($argument),*) $(-> $result)?>(
                                concat!(stringify!($name), "\0").as_bytes(),
                            )
                            .unwrap_or_else(|error| panic!("libR does not export {}: {error}", stringify!($name)));
                    )*
                    Api { library, $($name,)* }
                }
            }
        }
    };
}

api! {
    ry_initialize: fn() -> c_int;
    ry_set_gc_stress: fn(c_int);
    ry_collect: fn(c_int);
    ry_gc_stats: fn(*mut GcStats);
    ry_protect_depth: fn() -> c_int;
    ry_arena_bytes: fn() -> usize;
    ry_context_depth: fn() -> c_int;
    ry_warning_count: fn() -> c_int;
    ry_warning_message: fn(c_int) -> *const c_char;
    ry_clear_warnings: fn();
    ry_set_console: fn(Option<unsafe extern "C" fn(*const c_char, usize, c_int, *mut c_void)>, *mut c_void);
    ry_set_seed: fn(c_int);
    Rf_install: fn(*const c_char) -> Sexp;
    Rf_mkString: fn(*const c_char) -> Sexp;
    Rf_mkCharLenCE: fn(*const c_char, c_int, c_int) -> Sexp;
    Rf_cons: fn(Sexp, Sexp) -> Sexp;
    Rf_lcons: fn(Sexp, Sexp) -> Sexp;
    Rf_protect: fn(Sexp) -> Sexp;
    Rf_unprotect: fn(c_int);
    Rf_ScalarInteger: fn(c_int) -> Sexp;
    Rf_ScalarReal: fn(f64) -> Sexp;
    Rf_allocVector: fn(c_int, isize) -> Sexp;
    Rf_getAttrib: fn(Sexp, Sexp) -> Sexp;
    R_tryEvalSilent: fn(Sexp, Sexp, *mut c_int) -> Sexp;
    R_curErrorBuf: fn() -> *const c_char;
    TYPEOF: fn(Sexp) -> c_int;
    XLENGTH: fn(Sexp) -> isize;
    REAL: fn(Sexp) -> *mut f64;
    INTEGER: fn(Sexp) -> *mut c_int;
    RAW: fn(Sexp) -> *mut u8;
    STRING_ELT: fn(Sexp, isize) -> Sexp;
    SET_STRING_ELT: fn(Sexp, isize, Sexp);
    VECTOR_ELT: fn(Sexp, isize) -> Sexp;
    R_CHAR: fn(Sexp) -> *const c_char;
    SET_TAG: fn(Sexp, Sexp);
    SET_VECTOR_ELT: fn(Sexp, isize, Sexp) -> Sexp;
    Rf_duplicate: fn(Sexp) -> Sexp;
    Rf_setAttrib: fn(Sexp, Sexp, Sexp) -> Sexp;
    Rf_coerceVector: fn(Sexp, u32) -> Sexp;
    R_compute_identical: fn(Sexp, Sexp, c_int) -> c_int;
    ry_read_rds: fn(*const c_char) -> Sexp;
    ry_native_symbol: fn(*const c_char, *const c_char) -> Sexp;
}

pub struct Session {
    pub api: Api,
}

/// One locked use of the session. Everything it protects is unprotected when it ends.
pub struct R<'a> {
    session: &'a Session,
    pub protected: std::cell::Cell<c_int>,
    _lock: MutexGuard<'static, ()>,
}

static SESSION: OnceLock<Session> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());
pub static CONSOLE: Mutex<Vec<u8>> = Mutex::new(Vec::new());

unsafe extern "C" fn capture_console(
    text: *const c_char,
    length: usize,
    _is_error: c_int,
    _data: *mut c_void,
) {
    let bytes = unsafe { std::slice::from_raw_parts(text as *const u8, length) };
    CONSOLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .extend_from_slice(bytes);
}

/// `cargo test` writes the crate's fresh shared library next to the test executable, in
/// `target/<profile>/deps`; the copy one level up is only refreshed by `cargo build`.
fn library_dir() -> PathBuf {
    let executable = std::env::current_exe().expect("the test executable has a path");
    executable
        .parent()
        .expect("the test executable lives in a directory")
        .to_path_buf()
}

fn session() -> &'static Session {
    SESSION.get_or_init(|| {
        let path = library_dir().join("libR.so");
        assert!(
            path.exists(),
            "{} does not exist; cargo builds it with the crate",
            path.display()
        );
        // GLOBAL, so packages loaded later resolve their R API references against this library.
        let library = unsafe { Library::open(Some(&path), RTLD_NOW | RTLD_GLOBAL) }
            .unwrap_or_else(|error| panic!("cannot load {}: {error}", path.display()));
        let api = unsafe { Api::load(library) };
        unsafe {
            assert_eq!((api.ry_initialize)(), 0);
            (api.ry_set_console)(Some(capture_console), std::ptr::null_mut());
        }
        Session { api }
    })
}

pub fn r() -> R<'static> {
    let lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = session();
    unsafe {
        (session.api.ry_set_gc_stress)(0);
        (session.api.ry_clear_warnings)();
    }
    CONSOLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    R {
        session,
        protected: std::cell::Cell::new(0),
        _lock: lock,
    }
}

impl Drop for R<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.api().Rf_unprotect)(self.protected.get());
            (self.api().ry_set_gc_stress)(0);
        }
    }
}

impl R<'_> {
    pub fn api(&self) -> &Api {
        &self.session.api
    }

    pub fn data<T: Copy>(&self, name: &str) -> T {
        let name = CString::new(name).expect("a symbol name");
        unsafe {
            let address = self
                .api()
                .library
                .get::<*mut T>(name.as_bytes_with_nul())
                .unwrap_or_else(|error| panic!("libR does not export {name:?}: {error}"));
            **address
        }
    }

    pub fn nil(&self) -> Sexp {
        self.data("R_NilValue")
    }

    pub fn keep(&self, value: Sexp) -> Sexp {
        unsafe { (self.api().Rf_protect)(value) };
        self.protected.set(self.protected.get() + 1);
        value
    }

    pub fn string(&self, text: &str) -> Sexp {
        let text = CString::new(text).expect("no NUL");
        let value = unsafe { (self.api().Rf_mkString)(text.as_ptr()) };
        self.keep(value)
    }

    pub fn integer(&self, value: c_int) -> Sexp {
        let value = unsafe { (self.api().Rf_ScalarInteger)(value) };
        self.keep(value)
    }

    pub fn real(&self, value: f64) -> Sexp {
        let value = unsafe { (self.api().Rf_ScalarReal)(value) };
        self.keep(value)
    }

    pub fn symbol(&self, name: &str) -> Sexp {
        let name = CString::new(name).expect("no NUL");
        unsafe { (self.api().Rf_install)(name.as_ptr()) }
    }

    /// A call `function(arguments...)`, protected.
    pub fn call_expression(&self, function: &str, arguments: &[(Option<&str>, Sexp)]) -> Sexp {
        let mut list = self.nil();
        for &(name, argument) in arguments.iter().rev() {
            list = self.keep(unsafe { (self.api().Rf_cons)(argument, list) });
            if let Some(name) = name {
                unsafe { (self.api().SET_TAG)(list, self.symbol(name)) };
            }
        }
        let head = self.symbol(function);
        self.keep(unsafe { (self.api().Rf_lcons)(head, list) })
    }

    /// Unprotects what this session protected after `mark`, a value `protected` had earlier.
    pub fn release_to(&self, mark: c_int) {
        unsafe { (self.api().Rf_unprotect)(self.protected.get() - mark) };
        self.protected.set(mark);
    }

    /// `quote(expression)`, so `.Call` passes the expression itself instead of its value.
    pub fn quote(&self, expression: Sexp) -> Sexp {
        self.call_expression("quote", &[(None, expression)])
    }

    /// The harness's own protections are not the runtime's to balance, so depth checks
    /// subtract them.
    pub fn runtime_protect_depth(&self) -> c_int {
        self.protect_depth() - self.protected.get()
    }

    /// Evaluates `function(arguments...)` in the global environment.
    pub fn eval(&self, function: &str, arguments: &[(Option<&str>, Sexp)]) -> Result<Sexp, String> {
        let call = self.call_expression(function, arguments);
        let mut failed = 0;
        let global = self.data("R_GlobalEnv");
        let value = unsafe { (self.api().R_tryEvalSilent)(call, global, &mut failed) };
        if failed != 0 {
            let message = unsafe { CStr::from_ptr((self.api().R_curErrorBuf)()) };
            return Err(message.to_string_lossy().into_owned());
        }
        Ok(self.keep(value))
    }

    /// Evaluates an expression in the global environment.
    pub fn eval_expression(&self, expression: Sexp) -> Result<Sexp, String> {
        let mut failed = 0;
        let global = self.data("R_GlobalEnv");
        let value = unsafe { (self.api().R_tryEvalSilent)(expression, global, &mut failed) };
        if failed != 0 {
            let message = unsafe { CStr::from_ptr((self.api().R_curErrorBuf)()) };
            return Err(message.to_string_lossy().into_owned());
        }
        Ok(self.keep(value))
    }

    /// `.Call("routine", arguments...)`.
    pub fn call(&self, routine: &str, arguments: &[Sexp]) -> Result<Sexp, String> {
        let name = self.string(routine);
        let mut all = vec![(None, name)];
        all.extend(arguments.iter().map(|&argument| (None, argument)));
        self.eval(".Call", &all)
    }

    pub fn load(&self, library: &Path) {
        let path = self.string(&library.to_string_lossy());
        self.eval("dyn.load", &[(None, path)])
            .unwrap_or_else(|error| panic!("dyn.load failed: {error}"));
    }

    pub fn type_of(&self, value: Sexp) -> c_int {
        unsafe { (self.api().TYPEOF)(value) }
    }

    pub fn length(&self, value: Sexp) -> usize {
        unsafe { (self.api().XLENGTH)(value) as usize }
    }

    pub fn reals(&self, value: Sexp) -> Vec<f64> {
        assert_eq!(self.type_of(value), REALSXP);
        unsafe { std::slice::from_raw_parts((self.api().REAL)(value), self.length(value)).to_vec() }
    }

    pub fn integers(&self, value: Sexp) -> Vec<c_int> {
        assert!(matches!(self.type_of(value), INTSXP | LGLSXP));
        unsafe {
            std::slice::from_raw_parts((self.api().INTEGER)(value), self.length(value)).to_vec()
        }
    }

    pub fn strings(&self, value: Sexp) -> Vec<Option<String>> {
        assert_eq!(self.type_of(value), STRSXP);
        let na: Sexp = self.data("R_NaString");
        (0..self.length(value))
            .map(|index| unsafe {
                let element = (self.api().STRING_ELT)(value, index as isize);
                (element != na).then(|| {
                    CStr::from_ptr((self.api().R_CHAR)(element))
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .collect()
    }

    pub fn element(&self, value: Sexp, index: usize) -> Sexp {
        unsafe { (self.api().VECTOR_ELT)(value, index as isize) }
    }

    pub fn warnings(&self) -> Vec<String> {
        unsafe {
            (0..(self.api().ry_warning_count)())
                .map(|index| {
                    CStr::from_ptr((self.api().ry_warning_message)(index))
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        }
    }

    pub fn stress(&self, on: bool) {
        unsafe { (self.api().ry_set_gc_stress)(on as c_int) };
    }

    pub fn stats(&self) -> GcStats {
        let mut stats = GcStats::default();
        unsafe { (self.api().ry_gc_stats)(&mut stats) };
        stats
    }

    pub fn protect_depth(&self) -> c_int {
        unsafe { (self.api().ry_protect_depth)() }
    }
}

// Building packages.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Headers {
    Ry,
    GnuR,
}

pub fn package_sources(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/packages")
        .join(name)
        .join("src")
}

fn run(command: &mut Command) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("cannot run {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Compiles a package's `src/` into `<name>.so`, against ry's headers or through GNU R's
/// `R CMD SHLIB`, which uses GNU R's headers and links GNU R's `-lR`.
pub fn build_package(name: &str, headers: Headers) -> PathBuf {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{headers:?}"));
    std::fs::create_dir_all(&output).expect("a build directory");
    let library = output.join(format!("{name}.so"));
    let mut sources: Vec<PathBuf> = std::fs::read_dir(package_sources(name))
        .expect("package sources")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| matches!(path.extension().and_then(|e| e.to_str()), Some("c" | "cpp")))
        .collect();
    sources.sort();
    match headers {
        Headers::Ry => {
            let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
            let mut objects = Vec::new();
            let mut cpp = false;
            for source in &sources {
                let object = output
                    .join(source.file_name().expect("a file name"))
                    .with_extension("o");
                let is_cpp = source.extension().is_some_and(|e| e == "cpp");
                cpp |= is_cpp;
                let compiler = if is_cpp { "c++" } else { "cc" };
                run(Command::new(compiler)
                    .args(["-fPIC", "-O1", "-g", "-Wall", "-c"])
                    .arg("-I")
                    .arg(&include)
                    .arg(source)
                    .arg("-o")
                    .arg(&object));
                objects.push(object);
            }
            let mut link = Command::new(if cpp { "c++" } else { "cc" });
            link.arg("-shared").arg("-o").arg(&library).args(&objects);
            run(&mut link);
        }
        Headers::GnuR => {
            for source in &sources {
                std::fs::copy(
                    source,
                    output.join(source.file_name().expect("a file name")),
                )
                .expect("copy a source");
            }
            for header in std::fs::read_dir(package_sources(name)).expect("package sources") {
                let header = header.expect("a directory entry").path();
                if header.extension().is_some_and(|e| e == "h") {
                    std::fs::copy(
                        &header,
                        output.join(header.file_name().expect("a file name")),
                    )
                    .expect("copy a header");
                }
            }
            let names: Vec<String> = sources
                .iter()
                .map(|source| {
                    source
                        .file_name()
                        .expect("a file name")
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            run(Command::new("R")
                .current_dir(&output)
                .args(["CMD", "SHLIB", "-o"])
                .arg(format!("{name}.so"))
                .args(&names));
        }
    }
    library
}

pub fn gnu_r_available() -> bool {
    if std::env::var_os("RY_SKIP_GNU_R").is_some() {
        return false;
    }
    let found = Command::new("R")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        found,
        "GNU R is needed to build against its headers; install it or set RY_SKIP_GNU_R=1"
    );
    true
}

pub fn rytest(r: &R) {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("rytest", Headers::Ry));
    r.load(library);
}

pub fn temporary_file(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    drop(std::fs::remove_file(&path));
    path
}

/// brio's round trip: write lines, read them back, read the whole file, detect line endings.
pub fn brio_round_trip(r: &R) {
    let path = temporary_file("brio-lines.txt");
    let path_value = r.string(&path.to_string_lossy());
    let lines = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, 3) });
    for (index, line) in ["first", "zweite Zeile \u{e4}", ""].iter().enumerate() {
        let element = unsafe {
            (r.api().Rf_mkCharLenCE)(line.as_ptr() as *const c_char, line.len() as c_int, 1)
        };
        unsafe { (r.api().SET_STRING_ELT)(lines, index as isize, element) };
    }
    let eol = r.string("\n");
    r.call("brio_write_lines", &[lines, path_value, eol])
        .expect("brio_write_lines");
    let all = r.integer(-1);
    let read = r
        .call("brio_read_lines", &[path_value, all])
        .expect("brio_read_lines");
    assert_eq!(
        r.strings(read),
        vec![
            Some("first".into()),
            Some("zweite Zeile \u{e4}".into()),
            Some(String::new())
        ]
    );
    let two = r.integer(2);
    let first_two = r
        .call("brio_read_lines", &[path_value, two])
        .expect("brio_read_lines");
    assert_eq!(r.length(first_two), 2);
    let whole = r
        .call("brio_read_file", &[path_value])
        .expect("brio_read_file");
    assert_eq!(
        r.strings(whole),
        vec![Some("first\nzweite Zeile \u{e4}\n\n".into())]
    );
    let endings = r
        .call("brio_file_line_endings", &[path_value])
        .expect("brio_file_line_endings");
    assert_eq!(r.strings(endings), vec![Some("\n".into())]);
    let raw = r
        .call("brio_read_file_raw", &[path_value])
        .expect("brio_read_file_raw");
    assert_eq!(r.type_of(raw), RAWSXP);
    let bytes = unsafe { std::slice::from_raw_parts((r.api().RAW)(raw), r.length(raw)) };
    assert_eq!(bytes, "first\nzweite Zeile \u{e4}\n\n".as_bytes());
    let missing = r.string("/nonexistent/brio-file");
    let error = r
        .call("brio_read_lines", &[missing, all])
        .expect_err("a missing file is an error");
    assert_eq!(
        error,
        "Error: Could not open file: /nonexistent/brio-file\n"
    );
}

// The installed corpus: packages that ship a shared object, and their load order.

/// An installed package that ships a shared object.
#[derive(Clone, Debug)]
pub struct Package {
    pub directory: PathBuf,
    /// A base package (`Priority: base`) ships with GNU R and is built from R's own sources
    /// against its internal headers, so it is reported apart from the benchmark closure.
    pub base: bool,
    pub library: PathBuf,
    pub dependencies: Vec<String>,
}

pub fn corpus_libraries() -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    let corpus = std::env::var_os("RY_CORPUS_LIBRARY").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.corpus/library"),
        PathBuf::from,
    );
    if corpus.is_dir() {
        libraries.push(corpus);
    }
    if let Ok(output) = Command::new("R").arg("RHOME").output()
        && output.status.success()
    {
        let home = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let system = Path::new(&home).join("library");
        if system.is_dir() {
            libraries.push(system);
        }
    }
    libraries
}

/// The DESCRIPTION fields that name packages whose code must be loaded first.
fn dependencies(description: &str) -> Vec<String> {
    let mut fields: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut current: Option<String> = None;
    for line in description.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(field) = &current {
                fields.entry(field.clone()).or_default().push_str(line);
            }
        } else if let Some((field, value)) = line.split_once(':') {
            current = Some(field.trim().to_owned());
            fields.insert(field.trim().to_owned(), value.to_owned());
        }
    }
    let mut names = Vec::new();
    for field in ["Depends", "Imports", "LinkingTo"] {
        if let Some(value) = fields.get(field) {
            for entry in value.split(',') {
                let name = entry
                    .trim()
                    .split(|character: char| character.is_whitespace() || character == '(')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                if !name.is_empty() && name != "R" {
                    names.push(name);
                }
            }
        }
    }
    names
}

pub fn scan(libraries: &[PathBuf]) -> std::collections::BTreeMap<String, Package> {
    let mut packages = std::collections::BTreeMap::new();
    for library in libraries {
        let Ok(entries) = std::fs::read_dir(library) else {
            continue;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if packages.contains_key(&name) {
                continue;
            }
            let Ok(description) = std::fs::read_to_string(directory.join("DESCRIPTION")) else {
                continue;
            };
            let Ok(objects) = std::fs::read_dir(directory.join("libs")) else {
                continue;
            };
            let mut objects: Vec<PathBuf> = objects
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "so"))
                .collect();
            objects.sort();
            // The package's own object is named after it, with dots as underscores in a few
            // packages (data.table builds data_table.so).
            let wanted = [format!("{name}.so"), format!("{}.so", name.replace('.', "_"))];
            let Some(library) = objects
                .iter()
                .find(|path| {
                    path.file_name()
                        .is_some_and(|file| wanted.iter().any(|wanted| file == wanted.as_str()))
                })
                .or(objects.first())
                .cloned()
            else {
                continue;
            };
            let base = description
                .lines()
                .any(|line| line.trim() == "Priority: base");
            packages.insert(
                name,
                Package {
                    base,
                    directory,
                    library,
                    dependencies: dependencies(&description),
                },
            );
        }
    }
    packages
}

/// `name` and the packages it depends on, dependencies first.
pub fn load_order(name: &str, packages: &std::collections::BTreeMap<String, Package>) -> Vec<String> {
    fn visit(
        name: &str,
        packages: &std::collections::BTreeMap<String, Package>,
        seen: &mut std::collections::BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !seen.insert(name.to_owned()) {
            return;
        }
        if let Some(package) = packages.get(name) {
            for dependency in &package.dependencies {
                visit(dependency, packages, seen, order);
            }
            order.push(name.to_owned());
        }
    }
    let mut order = Vec::new();
    visit(name, packages, &mut std::collections::BTreeSet::new(), &mut order);
    order
}

