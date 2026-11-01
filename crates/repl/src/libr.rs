//! R's C API resolved at runtime via `dlopen`, so nothing links R at build time.
//! Symbols resolve eagerly so a missing one fails at startup by name; constant
//! globals are only valid after `setup_Rmainloop`.

use crate::ReplError;
use libloading::Library;
use std::env;
use std::ffi::{CStr, CString, c_char, c_int, c_uchar, c_void};
#[cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Fill `buffer` (capacity `length`, NUL-terminated, newline included) and
/// return 1, or return 0 for EOF.
pub type ReadConsole = unsafe extern "C" fn(*const c_char, *mut c_uchar, c_int, c_int) -> c_int;
/// (text, length, output type: 0 stdout, 1 stderr).
pub type WriteConsoleEx = unsafe extern "C" fn(*const c_char, c_int, c_int);

/// Function pointers are required; globals are raw addresses into the loaded library.
pub struct RApi {
    /// Held for the process lifetime; the symbols point into it.
    _library: &'static Library,
    pub r_home: PathBuf,

    setup_mainloop: unsafe extern "C" fn(),
    run_mainloop: unsafe extern "C" fn(),
    /// Used instead of feeding the console: fed input costs a main-loop round trip
    /// and, before the first prompt, leaves the terminal half-configured.
    parse_eval_string: unsafe extern "C" fn(*const c_char, *mut c_void) -> *mut c_void,
    global_env: *mut *mut c_void,
    interrupts_pending: *mut c_int,
    /// R's innermost evaluation context; see [`at_top_level`].
    global_context: *mut *mut c_void,
    /// Registers `.Call` routines that live in this process.
    embedding_dll_info: unsafe extern "C" fn() -> *mut c_void,
    register_routines: unsafe extern "C" fn(
        *mut c_void,
        *const c_void,
        *const CallMethodDef,
        *const c_void,
        *const c_void,
    ) -> c_int,
    call: CallApi,

    #[cfg(unix)]
    initialize: unsafe extern "C" fn(c_int, *mut *mut c_char) -> c_int,
    #[cfg(unix)]
    running_as_main_program: *mut c_int,
    #[cfg(unix)]
    signal_handlers: *mut c_int,
    #[cfg(unix)]
    interactive: *mut c_int,
    #[cfg(unix)]
    consolefile: *mut *mut c_void,
    #[cfg(unix)]
    outputfile: *mut *mut c_void,
    #[cfg(unix)]
    read_console_hook: *mut Option<ReadConsole>,
    #[cfg(unix)]
    write_console_hook: *mut Option<unsafe extern "C" fn(*const c_char, c_int)>,
    #[cfg(unix)]
    write_console_ex_hook: *mut Option<WriteConsoleEx>,

    #[cfg(windows)]
    cmdlineoptions: unsafe extern "C" fn(c_int, *mut *mut c_char),
    #[cfg(windows)]
    def_params_ex: unsafe extern "C" fn(*mut Rstart, c_int) -> c_int,
    #[cfg(windows)]
    set_params: unsafe extern "C" fn(*mut Rstart),
    /// This lives in `Rgraphapp.dll` rather than in `R.dll`, and it is
    /// required. `readconsolecfg` dereferences graphapp state and crashes when
    /// this never ran.
    #[cfg(windows)]
    ga_initapp: unsafe extern "C" fn(c_int, *mut c_void) -> c_int,
    #[cfg(windows)]
    readconsolecfg: unsafe extern "C" fn(),
    #[cfg(windows)]
    get_r_user: unsafe extern "C" fn() -> *const c_char,
    #[cfg(windows)]
    character_mode: *mut c_int,
    #[cfg(windows)]
    user_break: *mut c_int,
}

/// Mirrors `R_ext/RStartup.h`; `R_DefParamsEx`'s version argument makes R validate
/// the layout. `NoRenviron` and `RstartVersion` are 16-bit GCC bitfields packed
/// low-to-high into one `int`.
#[cfg(windows)]
#[repr(C)]
struct Rstart {
    r_quiet: c_int,
    r_no_echo: c_int,
    r_interactive: c_int,
    r_verbose: c_int,
    load_site_file: c_int,
    load_init_file: c_int,
    debug_init_file: c_int,
    restore_action: c_int,
    save_action: c_int,
    vsize: usize,
    nsize: usize,
    max_vsize: usize,
    max_nsize: usize,
    ppsize: usize,
    norenviron_and_rstart_version: c_int,
    nconnections: c_int,
    rhome: *mut c_char,
    home: *mut c_char,
    read_console: Option<ReadConsole>,
    write_console: Option<unsafe extern "C" fn(*const c_char, c_int)>,
    callback: Option<unsafe extern "C" fn()>,
    show_message: Option<unsafe extern "C" fn(*const c_char)>,
    yes_no_cancel: Option<unsafe extern "C" fn(*const c_char) -> c_int>,
    busy: Option<unsafe extern "C" fn(c_int)>,
    character_mode: c_int,
    write_console_ex: Option<WriteConsoleEx>,
    emit_embedded_utf8: c_int,
    clean_up: Option<unsafe extern "C" fn(c_int, c_int, c_int)>,
    clearerr_console: Option<unsafe extern "C" fn()>,
    flush_console: Option<unsafe extern "C" fn()>,
    reset_console: Option<unsafe extern "C" fn()>,
    suicide: Option<unsafe extern "C" fn(*const c_char)>,
}

#[cfg(windows)]
const RSTART_VERSION: c_int = 1;
#[cfg(windows)]
const UI_MODE_R_GUI: c_int = 0;
#[cfg(windows)]
const UI_MODE_LINK_DLL: c_int = 2;
#[cfg(windows)]
const SA_NORESTORE: c_int = 0;
#[cfg(windows)]
const SA_NOSAVE: c_int = 3;

/// Stashed for the SIGINT handler: writing an aligned `c_int` is async-signal-safe,
/// and the address is set once before the handler is installed.
pub static INTERRUPTS_PENDING: AtomicPtr<c_int> = AtomicPtr::new(ptr::null_mut());

/// The address of `R_GlobalContext`, set once at initialization.
static GLOBAL_CONTEXT: AtomicPtr<*mut c_void> = AtomicPtr::new(ptr::null_mut());

/// Whether R is reading input at its top-level prompt rather than for `readline()`, `menu()` or
/// the browser. At top level the innermost context is R's top-level context, the only one
/// without a parent, and `nextcontext` is the first field of R's context struct.
pub fn at_top_level() -> bool {
    let address = GLOBAL_CONTEXT.load(Ordering::SeqCst);
    if address.is_null() {
        return false;
    }
    // SAFETY: the address is R's `R_GlobalContext`, read on R's thread while R waits for input.
    unsafe {
        let context = *address as *const *const c_void;
        !context.is_null() && (*context).is_null()
    }
}

/// R's Windows front-end break flag (`UserBreak`).
#[cfg(windows)]
pub static USER_BREAK: AtomicPtr<c_int> = AtomicPtr::new(ptr::null_mut());

#[cfg(windows)]
pub fn interrupt_r() {
    raise(&INTERRUPTS_PENDING);
    raise(&USER_BREAK);
}

#[cfg(unix)]
pub extern "C" fn sigint_to_r_flag(_signal: c_int) {
    raise(&INTERRUPTS_PENDING);
}

fn raise(flag: &AtomicPtr<c_int>) {
    let flag = flag.load(Ordering::SeqCst);
    if !flag.is_null() {
        unsafe { *flag = 1 };
    }
}

#[cfg(unix)]
pub fn load() -> Result<RApi, ReplError> {
    let r_home = discover_r_home()?;
    let library_path = shared_library_path(&r_home)?;
    // Compiled R packages link libR themselves; RTLD_GLOBAL lets our loaded symbols
    // satisfy them as launch-time linking would.
    let library = unsafe {
        libloading::os::unix::Library::open(
            Some(&library_path),
            libloading::os::unix::RTLD_LAZY | libloading::os::unix::RTLD_GLOBAL,
        )
    }
    .map(Library::from)
    .map_err(|error| {
        ReplError(format!(
            "failed to load the R shared library at {}: {error}",
            library_path.display()
        ))
    })?;

    let library: &'static Library = Box::leak(Box::new(library));
    Ok(RApi {
        r_home,
        initialize: symbol(library, &library_path, "Rf_initialize_R")?,
        setup_mainloop: symbol(library, &library_path, "setup_Rmainloop")?,
        run_mainloop: symbol(library, &library_path, "run_Rmainloop")?,
        parse_eval_string: symbol(library, &library_path, "R_ParseEvalString")?,
        global_env: symbol(library, &library_path, "R_GlobalEnv")?,
        running_as_main_program: symbol(library, &library_path, "R_running_as_main_program")?,
        signal_handlers: symbol(library, &library_path, "R_SignalHandlers")?,
        interactive: symbol(library, &library_path, "R_Interactive")?,
        consolefile: symbol(library, &library_path, "R_Consolefile")?,
        outputfile: symbol(library, &library_path, "R_Outputfile")?,
        interrupts_pending: symbol(library, &library_path, "R_interrupts_pending")?,
        global_context: symbol(library, &library_path, "R_GlobalContext")?,
        embedding_dll_info: symbol(library, &library_path, "R_getEmbeddingDllInfo")?,
        register_routines: symbol(library, &library_path, "R_registerRoutines")?,
        call: call_api(library, &library_path)?,
        read_console_hook: symbol(library, &library_path, "ptr_R_ReadConsole")?,
        write_console_hook: symbol(library, &library_path, "ptr_R_WriteConsole")?,
        write_console_ex_hook: symbol(library, &library_path, "ptr_R_WriteConsoleEx")?,
        _library: library,
    })
}

/// Sibling DLLs are preloaded so compiled-package imports resolve, and the DLL
/// directory is prepended to `PATH` so R.dll's own dependencies resolve.
#[cfg(windows)]
pub fn load() -> Result<RApi, ReplError> {
    let r_home = discover_r_home()?;
    let library_path = shared_library_path(&r_home)?;
    let library_directory = library_path.parent().unwrap_or(&r_home).to_path_buf();
    let path = env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![library_directory.clone()];
    entries.extend(env::split_paths(&path));
    if let Ok(joined) = env::join_paths(entries) {
        unsafe { env::set_var("PATH", joined) };
    }
    for sibling in ["Rblas.dll", "Rlapack.dll", "Riconv.dll"] {
        let candidate = library_directory.join(sibling);
        if candidate.exists()
            && let Ok(loaded) = unsafe { Library::new(&candidate) }
        {
            Box::leak(Box::new(loaded));
        }
    }
    // A hard requirement: `GA_initapp` is exported here, not by R.dll, and console
    // setup crashes when it never ran.
    let graphapp_path = library_directory.join("Rgraphapp.dll");
    let graphapp = unsafe { Library::new(&graphapp_path) }.map_err(|error| {
        ReplError(format!(
            "failed to load the R graphapp library at {}: {error}",
            graphapp_path.display()
        ))
    })?;
    let graphapp: &'static Library = Box::leak(Box::new(graphapp));
    let library = unsafe { Library::new(&library_path) }.map_err(|error| {
        ReplError(format!(
            "failed to load the R library at {}: {error}",
            library_path.display()
        ))
    })?;
    let library: &'static Library = Box::leak(Box::new(library));
    Ok(RApi {
        r_home,
        setup_mainloop: symbol(library, &library_path, "setup_Rmainloop")?,
        run_mainloop: symbol(library, &library_path, "run_Rmainloop")?,
        parse_eval_string: symbol(library, &library_path, "R_ParseEvalString")?,
        global_env: symbol(library, &library_path, "R_GlobalEnv")?,
        interrupts_pending: symbol(library, &library_path, "R_interrupts_pending")?,
        global_context: symbol(library, &library_path, "R_GlobalContext")?,
        embedding_dll_info: symbol(library, &library_path, "R_getEmbeddingDllInfo")?,
        register_routines: symbol(library, &library_path, "R_registerRoutines")?,
        call: call_api(library, &library_path)?,
        cmdlineoptions: symbol(library, &library_path, "cmdlineoptions")?,
        def_params_ex: symbol(library, &library_path, "R_DefParamsEx")?,
        set_params: symbol(library, &library_path, "R_SetParams")?,
        ga_initapp: symbol(graphapp, &graphapp_path, "GA_initapp")?,
        readconsolecfg: symbol(library, &library_path, "readconsolecfg")?,
        get_r_user: symbol(library, &library_path, "getRUser")?,
        character_mode: symbol(library, &library_path, "CharacterMode")?,
        user_break: symbol(library, &library_path, "UserBreak")?,
        _library: library,
    })
}

#[cfg(not(any(unix, windows)))]
pub fn load() -> Result<RApi, ReplError> {
    Err(ReplError(
        "the REPL is not supported on this platform yet (Unix and Windows only)".to_owned(),
    ))
}

/// A function type yields the function pointer, a `*mut T` the global's address.
/// The library is leaked, so the value outliving the `Symbol` guard is sound.
fn symbol<T: Copy>(
    library: &'static Library,
    library_path: &Path,
    name: &str,
) -> Result<T, ReplError> {
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|_| {
            ReplError(format!(
                "the R shared library at {} does not export `{name}`. \
                 Is this a complete R >= 4.2 installation?",
                library_path.display(),
            ))
        })
}

impl RApi {
    /// Must run once, on the thread that will own R, before `run_main_loop`. Hook
    /// writes must land between `Rf_initialize_R` (sets R's defaults) and
    /// `setup_Rmainloop` (snapshots them).
    #[cfg(unix)]
    pub fn initialize(
        &self,
        read_console: ReadConsole,
        write_console_ex: WriteConsoleEx,
        quiet: bool,
        interactive: bool,
        arguments: &[String],
    ) -> Result<(), ReplError> {
        let trailing = trailing_arguments(arguments)?;
        // R re-derives R_HOME through its own lookup; export ours so both agree.
        unsafe { env::set_var("R_HOME", &self.r_home) };
        export_wrapper_path_vars(&self.r_home);

        let mut argv: Vec<*mut c_char> = [c"repl", c"--no-save", c"--no-restore-data"]
            .iter()
            .chain(interactive.then_some(&c"--interactive"))
            // `--no-echo` also keeps R from ending the session with a newline of its own.
            .chain(quiet.then_some(&c"--no-echo"))
            .map(|argument| argument.as_ptr() as *mut c_char)
            .chain(
                trailing
                    .iter()
                    .map(|argument| argument.as_ptr() as *mut c_char),
            )
            .collect();

        unsafe {
            *self.running_as_main_program = 1;
            // We own signal handling: R's handlers would fight reedline's
            // terminal state and our SIGINT-to-flag translation.
            *self.signal_handlers = 0;
            (self.initialize)(argv.len() as c_int, argv.as_mut_ptr());
            // A script reports `interactive()` as FALSE, as under Rscript; a console, TRUE.
            *self.interactive = c_int::from(interactive);
            // NULL console files route all output through the hooks below.
            *self.consolefile = ptr::null_mut();
            *self.outputfile = ptr::null_mut();
            *self.read_console_hook = Some(read_console);
            // The plain hook must be cleared for the Ex hook to be consulted.
            *self.write_console_hook = None;
            *self.write_console_ex_hook = Some(write_console_ex);
            INTERRUPTS_PENDING.store(self.interrupts_pending, Ordering::SeqCst);
            GLOBAL_CONTEXT.store(self.global_context, Ordering::SeqCst);
            (self.setup_mainloop)();
        }
        Ok(())
    }

    /// Windows configures the console through the `Rstart` struct; `CharacterMode`
    /// flips from `RGui` to `LinkDLL` before `setup_Rmainloop` (see the flip).
    #[cfg(windows)]
    pub fn initialize(
        &self,
        read_console: ReadConsole,
        write_console_ex: WriteConsoleEx,
        quiet: bool,
        interactive: bool,
        arguments: &[String],
    ) -> Result<(), ReplError> {
        unsafe { env::set_var("R_HOME", &self.r_home) };
        let trailing = trailing_arguments(arguments)?;
        let mut argv: Vec<*mut c_char> = std::iter::once(c"ry".as_ptr() as *mut c_char)
            .chain(
                trailing
                    .iter()
                    .map(|argument| argument.as_ptr() as *mut c_char),
            )
            .collect();

        let rhome = CString::new(self.r_home.to_string_lossy().into_owned())
            .map_err(|_| ReplError("R_HOME contains a NUL byte".to_owned()))?;
        // R's `~` is not the Win32 profile directory: `getRUser` follows R's own search
        // order, so `~` and `R_LIBS_USER` resolve as in stock R. The bytes stay in R's
        // native encoding.
        let r_user = unsafe { (self.get_r_user)() };
        let home = if r_user.is_null() {
            CString::new(env::var("USERPROFILE").unwrap_or_else(|_| ".".to_owned()))
                .map_err(|_| ReplError("USERPROFILE contains a NUL byte".to_owned()))?
        } else {
            unsafe { CStr::from_ptr(r_user) }.to_owned()
        };
        // R keeps the pointers; the strings live for the process.
        let rhome: &'static CString = Box::leak(Box::new(rhome));
        let home: &'static CString = Box::leak(Box::new(home));

        unsafe {
            (self.cmdlineoptions)(argv.len() as c_int, argv.as_mut_ptr());
            let mut start = std::mem::zeroed::<Rstart>();
            if (self.def_params_ex)(&mut start, RSTART_VERSION) != 0 {
                return Err(ReplError(
                    "this R does not accept the Rstart layout this build speaks \
                     (R >= 4.2 is required on Windows)"
                        .to_owned(),
                ));
            }
            start.r_interactive = c_int::from(interactive);
            start.r_quiet = c_int::from(quiet);
            start.r_no_echo = c_int::from(quiet);
            start.restore_action = SA_NORESTORE;
            start.save_action = SA_NOSAVE;
            start.rhome = rhome.as_ptr() as *mut c_char;
            start.home = home.as_ptr() as *mut c_char;
            start.read_console = Some(read_console);
            // The plain callback must be clear for the Ex callback to be used.
            start.write_console = None;
            start.write_console_ex = Some(write_console_ex);
            start.callback = Some(no_op_callback);
            start.show_message = Some(show_message_to_stderr);
            start.yes_no_cancel = Some(yes_no_cancel_default);
            start.busy = Some(no_op_busy);
            start.character_mode = UI_MODE_R_GUI;
            (self.set_params)(&mut start);
            (self.ga_initapp)(0, ptr::null_mut());
            (self.readconsolecfg)();
            // Under `RGui`, `do_system` invalidates the standard handles before spawning,
            // which hangs `system()`; `LinkDLL` keeps the RGui callback wiring.
            *self.character_mode = UI_MODE_LINK_DLL;
            INTERRUPTS_PENDING.store(self.interrupts_pending, Ordering::SeqCst);
            USER_BREAK.store(self.user_break, Ordering::SeqCst);
            GLOBAL_CONTEXT.store(self.global_context, Ordering::SeqCst);
            (self.setup_mainloop)();
        }
        Ok(())
    }

    /// Evaluates in the global environment outside the main loop. Call only between
    /// [`Self::initialize`] and [`Self::run_main_loop`]; errors are R's to report.
    pub fn eval(&self, statement: &str) {
        let Ok(text) = CString::new(statement) else {
            return;
        };
        unsafe { (self.parse_eval_string)(text.as_ptr(), *self.global_env) };
    }

    pub fn run_main_loop(&self) {
        unsafe { (self.run_mainloop)() }
    }

    /// Makes `routines` callable from R as `.Call("name", ..., PACKAGE = "(embedding)")`,
    /// and enables the argument helpers below for them. Call once, after
    /// [`Self::initialize`].
    pub fn register_call_routines(&self, routines: &[CallRoutine]) {
        if CALL_API.set(self.call).is_err() || routines.is_empty() {
            return;
        }
        let mut definitions: Vec<CallMethodDef> = routines
            .iter()
            .map(|routine| CallMethodDef {
                name: routine.name.as_ptr(),
                function: routine.function as *const c_void,
                arguments: routine.arguments,
            })
            .collect();
        definitions.push(CallMethodDef {
            name: ptr::null(),
            function: ptr::null(),
            arguments: 0,
        });
        // R keeps pointers into the table for the life of the process.
        let definitions: &'static [CallMethodDef] = Box::leak(definitions.into_boxed_slice());
        unsafe {
            let info = (self.embedding_dll_info)();
            (self.register_routines)(
                info,
                ptr::null(),
                definitions.as_ptr(),
                ptr::null(),
                ptr::null(),
            );
        }
    }
}

/// An R object pointer, opaque to Rust.
pub type Sexp = *mut c_void;

/// A routine R calls through `.Call`; every argument and the result is a [`Sexp`]. It runs on
/// R's thread and must not unwind into R.
pub struct CallRoutine {
    pub name: &'static CStr,
    pub function: *const (),
    pub arguments: c_int,
}

#[repr(C)]
pub struct CallMethodDef {
    name: *const c_char,
    function: *const c_void,
    arguments: c_int,
}

/// The few object accessors a `.Call` routine needs, resolved with everything else.
#[derive(Clone, Copy)]
struct CallApi {
    is_string: unsafe extern "C" fn(Sexp) -> c_int,
    length: unsafe extern "C" fn(Sexp) -> c_int,
    string_elt: unsafe extern "C" fn(Sexp, isize) -> Sexp,
    r_char: unsafe extern "C" fn(Sexp) -> *const c_char,
    as_logical: unsafe extern "C" fn(Sexp) -> c_int,
    mk_string: unsafe extern "C" fn(*const c_char) -> Sexp,
    nil: *mut Sexp,
}

// Only R's thread calls routines, and the pointers are into a library loaded for the process.
unsafe impl Send for CallApi {}
unsafe impl Sync for CallApi {}

static CALL_API: OnceLock<CallApi> = OnceLock::new();

fn call_api(library: &'static Library, library_path: &Path) -> Result<CallApi, ReplError> {
    Ok(CallApi {
        is_string: symbol(library, library_path, "Rf_isString")?,
        length: symbol(library, library_path, "Rf_length")?,
        string_elt: symbol(library, library_path, "STRING_ELT")?,
        r_char: symbol(library, library_path, "R_CHAR")?,
        as_logical: symbol(library, library_path, "Rf_asLogical")?,
        mk_string: symbol(library, library_path, "Rf_mkString")?,
        nil: symbol(library, library_path, "R_NilValue")?,
    })
}

/// The first string of a character vector, for a `.Call` routine's argument.
///
/// # Safety
///
/// `value` must be an R object, such as an argument R passed to a `.Call` routine.
pub unsafe fn string_argument(value: Sexp) -> Option<String> {
    let api = CALL_API.get()?;
    unsafe {
        if (api.is_string)(value) == 0 || (api.length)(value) < 1 {
            return None;
        }
        let text = (api.r_char)((api.string_elt)(value, 0));
        (!text.is_null()).then(|| CStr::from_ptr(text).to_string_lossy().into_owned())
    }
}

/// `TRUE` only for a logical (or coercible) `TRUE`.
///
/// # Safety
///
/// `value` must be an R object, such as an argument R passed to a `.Call` routine.
pub unsafe fn logical_argument(value: Sexp) -> bool {
    CALL_API
        .get()
        .is_some_and(|api| unsafe { (api.as_logical)(value) } == 1)
}

/// R's `NULL`.
pub fn null() -> Sexp {
    CALL_API
        .get()
        .map_or(ptr::null_mut(), |api| unsafe { *api.nil })
}

/// A new character vector of one string. A NUL byte ends the string early.
pub fn string_value(text: &str) -> Sexp {
    let Some(api) = CALL_API.get() else {
        return ptr::null_mut();
    };
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { (api.mk_string)(text.as_ptr()) }
}

#[cfg(windows)]
unsafe extern "C" fn no_op_callback() {}

#[cfg(windows)]
unsafe extern "C" fn no_op_busy(_which: c_int) {}

#[cfg(windows)]
unsafe extern "C" fn show_message_to_stderr(message: *const c_char) {
    let text = unsafe { prompt_text(message) };
    eprintln!("{text}");
}

/// A terminal cannot pose a GUI dialog; answer "cancel" (-1), R's safe refusal.
#[cfg(windows)]
unsafe extern "C" fn yes_no_cancel_default(question: *const c_char) -> c_int {
    let text = unsafe { prompt_text(question) };
    eprintln!("{text} (cancelled: interactive dialogs are not supported)");
    -1
}

fn discover_r_home() -> Result<PathBuf, ReplError> {
    let (home, origin) = match env::var("R_HOME") {
        Ok(home) if !home.is_empty() => (home, "R_HOME is set to"),
        _ => {
            let output = Command::new("R").arg("RHOME").output().map_err(|error| {
                ReplError(format!(
                    "no R found: R_HOME is not set and running `R RHOME` failed ({error}). \
                     Install R or set R_HOME to an R installation."
                ))
            })?;
            let home = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if home.is_empty() {
                return Err(ReplError(
                    "`R RHOME` printed nothing. Set R_HOME to an R installation".to_owned(),
                ));
            }
            (home, "`R RHOME` reported")
        }
    };
    let path = PathBuf::from(&home);
    if !path.is_dir() {
        return Err(ReplError(format!(
            "{origin} {home}, but that directory does not exist"
        )));
    }
    Ok(path)
}

/// Embedding bypasses R's shell wrapper, which exports `R_SHARE_DIR`,
/// `R_INCLUDE_DIR` and `R_DOC_DIR`; distributions that relocate them (Fedora,
/// RHEL) need them recovered from its `VAR=value` lines. Faster than asking R.
#[cfg(unix)]
fn export_wrapper_path_vars(r_home: &Path) {
    let Ok(wrapper) = fs::read_to_string(r_home.join("bin").join("R")) else {
        return;
    };
    for name in ["R_SHARE_DIR", "R_INCLUDE_DIR", "R_DOC_DIR"] {
        if env::var_os(name).is_none()
            && let Some(value) = wrapper_path_var(&wrapper, name)
        {
            unsafe { env::set_var(name, value) };
        }
    }
}

/// Quotes stripped; a value still containing `$` is an unsubstituted shell
/// expression and is skipped.
#[cfg(unix)]
fn wrapper_path_var(script: &str, name: &str) -> Option<String> {
    let value = script.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        (key == name).then_some(value)
    })?;
    let value = value.trim_matches('\'').trim_matches('"');
    if value.is_empty() || value.contains('$') {
        return None;
    }
    Some(value.to_owned())
}

/// Absence almost always means an R built without `--enable-R-shlib`.
#[cfg(unix)]
fn shared_library_path(r_home: &Path) -> Result<PathBuf, ReplError> {
    let library_directory = r_home.join("lib");
    for name in ["libR.so", "libR.dylib"] {
        let candidate = library_directory.join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ReplError(format!(
        "no R shared library under {}. The REPL needs an R compiled with \
         `--enable-R-shlib`, which every CRAN binary distribution is",
        library_directory.display()
    )))
}

#[cfg(windows)]
fn shared_library_path(r_home: &Path) -> Result<PathBuf, ReplError> {
    for directory in [r_home.join("bin").join("x64"), r_home.join("bin")] {
        let candidate = directory.join("R.dll");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ReplError(format!(
        "no R.dll under {}\\bin. Is this a complete R installation?",
        r_home.display()
    )))
}

/// # Safety
///
/// `prompt` must be null or a valid NUL-terminated C string.
pub unsafe fn prompt_text(prompt: *const c_char) -> String {
    if prompt.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(prompt) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process;

    #[test]
    fn missing_r_is_an_actionable_error() {
        // Must pass with and without R: force the env-var path with a nonexistent directory.
        let previous = env::var_os("R_HOME");
        unsafe { env::set_var("R_HOME", "/nonexistent/ry-test-r-home") };
        let error = discover_r_home().expect_err("a bogus R_HOME must fail");
        assert!(error.0.contains("does not exist"), "{}", error.0);
        match previous {
            Some(value) => unsafe { env::set_var("R_HOME", value) },
            None => unsafe { env::remove_var("R_HOME") },
        }
    }

    #[test]
    fn wrapper_path_vars_parse_plain_assignments_only() {
        let script = "#!/bin/sh\n\
             R_SHARE_DIR=/usr/share/R/share\n\
             export R_SHARE_DIR\n\
             R_INCLUDE_DIR=\"/usr/share/R/include\"\n\
             R_DOC_DIR=${R_DOC_DIR-'/usr/share/R/doc'}\n";
        assert_eq!(
            wrapper_path_var(script, "R_SHARE_DIR").as_deref(),
            Some("/usr/share/R/share")
        );
        assert_eq!(
            wrapper_path_var(script, "R_INCLUDE_DIR").as_deref(),
            Some("/usr/share/R/include")
        );
        assert_eq!(wrapper_path_var(script, "R_DOC_DIR"), None);
        assert_eq!(wrapper_path_var(script, "R_LIBS_USER"), None);
    }

    #[test]
    fn shared_library_error_names_the_fix() {
        let directory = env::temp_dir().join(format!("ry-repl-test-{}", process::id()));
        fs::create_dir_all(&directory).expect("create temp dir");
        let error = shared_library_path(&directory).expect_err("empty dir has no libR");
        assert!(error.0.contains("--enable-R-shlib"), "{}", error.0);
        fs::remove_dir_all(&directory).ok();
    }
}

/// `--args` and then `arguments`, which R hands to `commandArgs(trailingOnly = TRUE)` without
/// reading them as options. R keeps the pointers for the whole process, so the strings leak.
fn trailing_arguments(arguments: &[String]) -> Result<&'static [CString], ReplError> {
    if arguments.is_empty() {
        return Ok(&[]);
    }
    let strings = std::iter::once(Ok(c"--args".to_owned()))
        .chain(
            arguments
                .iter()
                .map(|argument| CString::new(argument.as_bytes())),
        )
        .collect::<Result<Vec<CString>, _>>()
        .map_err(|_| ReplError("an argument contains a NUL byte".to_owned()))?;
    Ok(Box::leak(strings.into_boxed_slice()))
}
