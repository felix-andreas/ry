//! Runtime binding to R's C API: locate the R installation, `dlopen` its
//! shared library, and resolve the (deliberately minimal) symbol surface the
//! console needs. No build-time linking anywhere — this crate builds and its
//! unit tests run on machines with no R at all.
//!
//! The surface grows on demand and every addition must stay in one of the
//! four shapes: a function pointer, a variadic function pointer, a mutable
//! global (accessed by address), or a constant global (copied by value, only
//! valid after `setup_Rmainloop`). Symbols are resolved eagerly in one sweep
//! so a missing required symbol fails at startup with its name, not later
//! with a null-call crash.

use crate::ReplError;
use std::ffi::{CStr, CString, c_char, c_int, c_uchar, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `R_ReadConsole`'s C signature: fill `buffer` (capacity `length`,
/// NUL-terminated, newline included) and return 1, or return 0 for EOF.
pub type ReadConsole = unsafe extern "C" fn(*const c_char, *mut c_uchar, c_int, c_int) -> c_int;
/// `R_WriteConsoleEx`: text, length, output type (0 stdout, 1 stderr).
pub type WriteConsoleEx = unsafe extern "C" fn(*const c_char, c_int, c_int);

/// The resolved API. Function pointers are required (startup fails naming
/// the first absent one); globals are raw addresses into the loaded library.
pub struct RApi {
    /// Held for the process lifetime; symbols above point into it.
    _library: &'static libloading::Library,
    pub r_home: PathBuf,

    setup_mainloop: unsafe extern "C" fn(),
    run_mainloop: unsafe extern "C" fn(),
    /// `R_ParseEvalString(text, env)` — parses and evaluates one string. The
    /// console cannot do this job: input fed through the read hook costs a
    /// main-loop round trip, and one taken before the editor's first prompt
    /// leaves the terminal half-configured.
    parse_eval_string: unsafe extern "C" fn(*const c_char, *mut c_void) -> *mut c_void,
    global_env: *mut *mut c_void,
    interrupts_pending: *mut c_int,

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
    /// Lives in `Rgraphapp.dll`, not `R.dll`; required — `readconsolecfg`
    /// dereferences graphapp state and crashes when it never ran.
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

/// Windows embedding configures the console through this struct
/// (`R_SetParams`) instead of the Unix hook globals. The layout mirrors R's
/// `R_ext/RStartup.h` exactly; `R_DefParamsEx`'s version argument makes R
/// validate it, replacing per-R-version struct mirroring. `NoRenviron` and
/// `RstartVersion` are a pair of 16-bit C bitfields packed low-to-high into
/// one `int` (R on Windows builds with GCC).
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

/// The interrupt flag's address, stashed for the SIGINT handler. Writing an
/// aligned `c_int` through it is async-signal-safe; the address is set once
/// before the handler is installed and never changes (the library is leaked).
pub static INTERRUPTS_PENDING: std::sync::atomic::AtomicPtr<c_int> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Windows' front-end break flag (`UserBreak`), set together with the
/// deferred `R_interrupts_pending` by the console control handler.
#[cfg(windows)]
pub static USER_BREAK: std::sync::atomic::AtomicPtr<c_int> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Ctrl-C on Windows: raise both interrupt flags; R honors them at its next
/// interrupt check.
#[cfg(windows)]
pub fn interrupt_r() {
    for flag_slot in [&INTERRUPTS_PENDING, &USER_BREAK] {
        let flag = flag_slot.load(std::sync::atomic::Ordering::SeqCst);
        if !flag.is_null() {
            unsafe { *flag = 1 };
        }
    }
}

#[cfg(unix)]
pub fn load() -> Result<RApi, ReplError> {
    // A static binary cannot embed R: musl's static `dlopen` always fails, and
    // static glibc's loads the system libc a second time beside libR, which
    // leaves R running against an uninitialized libc.
    if cfg!(target_feature = "crt-static") {
        return Err(ReplError(
            "this ry binary is statically linked, so it cannot load R; \
             install ry from source to use `ry repl` and `ry run`: \
             cargo install --git https://github.com/felix-andreas/ry ry-lang"
                .to_owned(),
        ));
    }
    let r_home = discover_r_home()?;
    let library_path = shared_library_path(&r_home)?;
    // R packages with compiled code link libR themselves; RTLD_GLOBAL makes
    // the symbols we load satisfy those packages exactly as launch-time
    // linking would.
    let library = unsafe {
        libloading::os::unix::Library::open(
            Some(&library_path),
            libloading::os::unix::RTLD_LAZY | libloading::os::unix::RTLD_GLOBAL,
        )
    }
    .map(libloading::Library::from)
    .map_err(|error| {
        ReplError(format!(
            "failed to load the R shared library at {}: {error}",
            library_path.display()
        ))
    })?;

    let library: &'static libloading::Library = Box::leak(Box::new(library));
    let api = RApi {
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
        read_console_hook: symbol(library, &library_path, "ptr_R_ReadConsole")?,
        write_console_hook: symbol(library, &library_path, "ptr_R_WriteConsole")?,
        write_console_ex_hook: symbol(library, &library_path, "ptr_R_WriteConsoleEx")?,
        _library: library,
    };
    Ok(api)
}

/// Windows: `R.dll` from `{R_HOME}\bin\x64` with its sibling DLLs preloaded
/// so compiled-package imports resolve (per-module symbol lookup means no
/// `RTLD_GLOBAL` equivalent is needed). The DLL directory is prepended to
/// `PATH` first so R.dll's own dependencies resolve from it.
#[cfg(windows)]
pub fn load() -> Result<RApi, ReplError> {
    let r_home = discover_r_home()?;
    let library_path = shared_library_path(&r_home)?;
    let library_directory = library_path.parent().unwrap_or(&r_home).to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![library_directory.clone()];
    entries.extend(std::env::split_paths(&path));
    if let Ok(joined) = std::env::join_paths(entries) {
        unsafe { std::env::set_var("PATH", joined) };
    }
    for sibling in ["Rblas.dll", "Rlapack.dll", "Riconv.dll"] {
        let candidate = library_directory.join(sibling);
        if candidate.exists()
            && let Ok(loaded) = unsafe { libloading::Library::new(&candidate) }
        {
            Box::leak(Box::new(loaded));
        }
    }
    // Unlike the best-effort preloads above, graphapp is a hard requirement:
    // `GA_initapp` is exported here (NOT by R.dll) and console setup crashes
    // when it never ran.
    let graphapp_path = library_directory.join("Rgraphapp.dll");
    let graphapp = unsafe { libloading::Library::new(&graphapp_path) }.map_err(|error| {
        ReplError(format!(
            "failed to load the R graphapp library at {}: {error}",
            graphapp_path.display()
        ))
    })?;
    let graphapp: &'static libloading::Library = Box::leak(Box::new(graphapp));
    let library = unsafe { libloading::Library::new(&library_path) }.map_err(|error| {
        ReplError(format!(
            "failed to load the R library at {}: {error}",
            library_path.display()
        ))
    })?;
    let library: &'static libloading::Library = Box::leak(Box::new(library));
    let api = RApi {
        r_home,
        setup_mainloop: symbol(library, &library_path, "setup_Rmainloop")?,
        run_mainloop: symbol(library, &library_path, "run_Rmainloop")?,
        parse_eval_string: symbol(library, &library_path, "R_ParseEvalString")?,
        global_env: symbol(library, &library_path, "R_GlobalEnv")?,
        interrupts_pending: symbol(library, &library_path, "R_interrupts_pending")?,
        cmdlineoptions: symbol(library, &library_path, "cmdlineoptions")?,
        def_params_ex: symbol(library, &library_path, "R_DefParamsEx")?,
        set_params: symbol(library, &library_path, "R_SetParams")?,
        ga_initapp: symbol(graphapp, &graphapp_path, "GA_initapp")?,
        readconsolecfg: symbol(library, &library_path, "readconsolecfg")?,
        get_r_user: symbol(library, &library_path, "getRUser")?,
        character_mode: symbol(library, &library_path, "CharacterMode")?,
        user_break: symbol(library, &library_path, "UserBreak")?,
        _library: library,
    };
    Ok(api)
}

/// Loading and discovery exist only where an embedding recipe does; other
/// platforms report the gap at startup instead.
#[cfg(not(any(unix, windows)))]
pub fn load() -> Result<RApi, ReplError> {
    Err(ReplError(
        "the REPL is not supported on this platform yet (Unix and Windows only)".to_owned(),
    ))
}

/// Resolves one symbol to the caller's type: a function type yields the
/// function pointer, a `*mut T` type yields the global's address. The
/// library reference is `'static` (leaked), so the returned value outliving
/// the `Symbol` guard is sound.
fn symbol<T: Copy>(
    library: &'static libloading::Library,
    library_path: &Path,
    name: &str,
) -> Result<T, ReplError> {
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|_| {
            ReplError(format!(
                "the R shared library at {} does not export `{name}` — \
                 is this a complete R >= 4.2 installation?",
                library_path.display(),
            ))
        })
}

impl RApi {
    /// Initializes embedded R and hooks the console callbacks. Must run on
    /// the thread that will own R (the main thread), exactly once per
    /// process, BEFORE `run_main_loop`.
    ///
    /// Safety contract with R: the hook writes happen between
    /// `Rf_initialize_R` (which sets R's defaults) and `setup_Rmainloop`
    /// (which snapshots them).
    #[cfg(unix)]
    pub fn initialize(
        &self,
        read_console: ReadConsole,
        write_console_ex: WriteConsoleEx,
    ) -> Result<(), ReplError> {
        // R re-derives R_HOME through its own lookup; export what we found
        // so both agree.
        unsafe { std::env::set_var("R_HOME", &self.r_home) };
        export_wrapper_path_vars(&self.r_home);

        let arguments = ["repl", "--interactive", "--no-save", "--no-restore-data"];
        let owned: Vec<CString> = arguments
            .iter()
            .map(|argument| CString::new(*argument).expect("static argv strings have no NUL"))
            .collect();
        let mut argv: Vec<*mut c_char> = owned
            .iter()
            .map(|argument| argument.as_ptr() as *mut c_char)
            .collect();

        unsafe {
            *self.running_as_main_program = 1;
            // We own signal handling: R's handlers would fight reedline's
            // terminal state and our SIGINT-to-flag translation.
            *self.signal_handlers = 0;
            (self.initialize)(argv.len() as c_int, argv.as_mut_ptr());
            *self.interactive = 1;
            // NULL console files route all output through the hooks below.
            *self.consolefile = std::ptr::null_mut();
            *self.outputfile = std::ptr::null_mut();
            *self.read_console_hook = Some(read_console);
            // The plain hook must be cleared for the Ex hook to be consulted.
            *self.write_console_hook = None;
            *self.write_console_ex_hook = Some(write_console_ex);
            INTERRUPTS_PENDING.store(self.interrupts_pending, std::sync::atomic::Ordering::SeqCst);
            (self.setup_mainloop)();
        }
        Ok(())
    }

    /// Windows initialization goes through the `Rstart` struct: defaults
    /// from `R_DefParamsEx` (its version argument makes R validate the
    /// layout), our console callbacks on top, `R_SetParams`, graphapp +
    /// console configuration, then the `CharacterMode` global flips from
    /// `RGui` to `LinkDLL` before `setup_Rmainloop` (see the comment at the
    /// flip).
    #[cfg(windows)]
    pub fn initialize(
        &self,
        read_console: ReadConsole,
        write_console_ex: WriteConsoleEx,
    ) -> Result<(), ReplError> {
        unsafe { std::env::set_var("R_HOME", &self.r_home) };
        let program = CString::new("ry").expect("static argv string has no NUL");
        let mut argv = [program.as_ptr() as *mut c_char];

        let rhome = CString::new(self.r_home.to_string_lossy().into_owned())
            .map_err(|_| ReplError("R_HOME contains a NUL byte".to_owned()))?;
        // R's `~` is NOT the Win32 profile directory: `getRUser` follows R's
        // own search order (R_USER, HOME, the Documents folder, then
        // HOMEDRIVE+HOMEPATH), so `~` and the `R_LIBS_USER` default resolve
        // exactly as in stock R. The bytes pass through in the native
        // encoding R hands out and expects back.
        let r_user = unsafe { (self.get_r_user)() };
        let home = if r_user.is_null() {
            CString::new(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_owned()))
                .map_err(|_| ReplError("USERPROFILE contains a NUL byte".to_owned()))?
        } else {
            CString::new(unsafe { CStr::from_ptr(r_user) }.to_bytes())
                .expect("a C string has no interior NUL")
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
            start.r_interactive = 1;
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
            (self.ga_initapp)(0, std::ptr::null_mut());
            (self.readconsolecfg)();
            // `do_system` checks this mode: under `RGui` it invalidates the
            // standard handles (`SetStdHandle`) before spawning, which hangs
            // `system()`. Flipping to `LinkDLL` once the console is
            // configured keeps the RGui callback wiring and spares the
            // handles.
            *self.character_mode = UI_MODE_LINK_DLL;
            INTERRUPTS_PENDING.store(self.interrupts_pending, std::sync::atomic::Ordering::SeqCst);
            USER_BREAK.store(self.user_break, std::sync::atomic::Ordering::SeqCst);
            (self.setup_mainloop)();
        }
        Ok(())
    }

    /// Evaluates one R statement in the global environment, outside the main
    /// loop. Call it only between [`Self::initialize`] (which sources the
    /// startup profiles) and [`Self::run_main_loop`]: R is fully set up, and
    /// nothing the console does can be perturbed because no prompt has run.
    /// Errors are R's to report; there is no value to hand back.
    pub fn eval(&self, statement: &str) {
        let Ok(text) = CString::new(statement) else {
            return;
        };
        unsafe { (self.parse_eval_string)(text.as_ptr(), *self.global_env) };
    }

    /// Runs R's real REPL; returns when the session ends (`q()`, or EOF from
    /// the read-console hook).
    pub fn run_main_loop(&self) {
        unsafe { (self.run_mainloop)() }
    }
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

/// A terminal cannot pose a GUI yes/no/cancel dialog; print the question and
/// answer "cancel" (-1), which R treats as the safe refusal.
#[cfg(windows)]
unsafe extern "C" fn yes_no_cancel_default(question: *const c_char) -> c_int {
    let text = unsafe { prompt_text(question) };
    eprintln!("{text} (cancelled: interactive dialogs are not supported)");
    -1
}

/// The SIGINT handler installed while R evaluates: translate Ctrl-C into
/// R's cooperative interrupt flag, honored at the next
/// `R_CheckUserInterrupt()`.
pub extern "C" fn sigint_to_r_flag(_signal: c_int) {
    let flag = INTERRUPTS_PENDING.load(std::sync::atomic::Ordering::SeqCst);
    if !flag.is_null() {
        unsafe { *flag = 1 };
    }
}

/// `R_HOME` from the environment when set, else `R RHOME` from `PATH` —
/// the same two-step every R front end uses.
fn discover_r_home() -> Result<PathBuf, ReplError> {
    if let Ok(home) = std::env::var("R_HOME")
        && !home.is_empty()
    {
        let path = PathBuf::from(&home);
        if path.is_dir() {
            return Ok(path);
        }
        return Err(ReplError(format!(
            "R_HOME is set to {home}, but that directory does not exist"
        )));
    }
    let output = Command::new("R").arg("RHOME").output().map_err(|error| {
        ReplError(format!(
            "no R found: R_HOME is not set and running `R RHOME` failed ({error}). \
                 Install R or set R_HOME to an R installation."
        ))
    })?;
    let home = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if home.is_empty() {
        return Err(ReplError(
            "`R RHOME` printed nothing — set R_HOME to an R installation".to_owned(),
        ));
    }
    let path = PathBuf::from(&home);
    if !path.is_dir() {
        return Err(ReplError(format!(
            "`R RHOME` reported {home}, but that directory does not exist"
        )));
    }
    Ok(path)
}

/// Launching `R` normally goes through its shell wrapper (`{R_HOME}/bin/R`),
/// which exports `R_SHARE_DIR`/`R_INCLUDE_DIR`/`R_DOC_DIR`; embedding
/// bypasses the wrapper. On distributions that relocate those directories
/// (Fedora, RHEL), `R.home("share")` and friends would then fall back to
/// nonexistent `{R_HOME}/<dir>` paths — so recover the values from the
/// wrapper's plain `VAR=value` lines (substituted literally when R was
/// installed). Best-effort by design: without the wrapper, R's own fallback
/// is correct on standard layouts. Asking R itself (`R --vanilla -s`) would
/// cost a few hundred milliseconds of startup; reading the script does not.
#[cfg(unix)]
fn export_wrapper_path_vars(r_home: &Path) {
    let Ok(wrapper) = std::fs::read_to_string(r_home.join("bin").join("R")) else {
        return;
    };
    for name in ["R_SHARE_DIR", "R_INCLUDE_DIR", "R_DOC_DIR"] {
        if std::env::var_os(name).is_some() {
            continue;
        }
        if let Some(value) = wrapper_path_var(&wrapper, name) {
            unsafe { std::env::set_var(name, value) };
        }
    }
}

/// The value of a plain `NAME=value` line in the wrapper script, quotes
/// stripped. A value still containing `$` is an unsubstituted shell
/// expression this parser cannot evaluate — skipped rather than exported
/// verbatim.
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

/// Unix: `{R_HOME}/lib/libR.{so,dylib}`. Absence almost always means an R
/// built without `--enable-R-shlib`; say so.
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
        "no R shared library under {} — the REPL needs an R compiled with \
         `--enable-R-shlib` (all CRAN binary distributions are)",
        library_directory.display()
    )))
}

/// Windows: `{R_HOME}\bin\x64\R.dll` (plain `bin` on other
/// architectures' builds).
#[cfg(windows)]
fn shared_library_path(r_home: &Path) -> Result<PathBuf, ReplError> {
    for directory in [r_home.join("bin").join("x64"), r_home.join("bin")] {
        let candidate = directory.join("R.dll");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ReplError(format!(
        "no R.dll under {}\\bin — is this a complete R installation?",
        r_home.display()
    )))
}

/// A copy of the NUL-terminated prompt R hands the read-console hook
/// (`"> "`, `"+ "`, `Browse[1]> `, …).
///
/// # Safety
///
/// `prompt` must be null or a valid NUL-terminated C string — exactly what
/// R passes its `R_ReadConsole` hook.
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

    #[test]
    fn missing_r_is_an_actionable_error() {
        // The test must pass on machines with and without R: force the
        // env-var path with a directory that cannot exist.
        let previous = std::env::var_os("R_HOME");
        unsafe { std::env::set_var("R_HOME", "/nonexistent/ry-test-r-home") };
        let error = discover_r_home().expect_err("a bogus R_HOME must fail");
        assert!(error.0.contains("does not exist"), "{}", error.0);
        match previous {
            Some(value) => unsafe { std::env::set_var("R_HOME", value) },
            None => unsafe { std::env::remove_var("R_HOME") },
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
        // An unsubstituted shell expression must not be exported verbatim.
        assert_eq!(wrapper_path_var(script, "R_DOC_DIR"), None);
        assert_eq!(wrapper_path_var(script, "R_LIBS_USER"), None);
    }

    #[test]
    fn shared_library_error_names_the_fix() {
        let directory = tempfile_like_dir();
        let error = shared_library_path(&directory).expect_err("empty dir has no libR");
        assert!(error.0.contains("--enable-R-shlib"), "{}", error.0);
        std::fs::remove_dir_all(&directory).ok();
    }

    fn tempfile_like_dir() -> PathBuf {
        let directory = std::env::temp_dir().join(format!("ry-repl-test-{}", std::process::id(),));
        std::fs::create_dir_all(&directory).expect("create temp dir");
        directory
    }
}
