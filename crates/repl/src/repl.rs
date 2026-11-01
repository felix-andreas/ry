//! An R console that `dlopen`s the R shared library and resolves every C-API
//! symbol at runtime, so nothing here needs R at build time.

pub mod console;
pub mod libr;

// Re-exported so hosts implement `SessionCompleter` against the same reedline version.
pub use reedline;

use std::error::Error;
use std::fmt;
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Debug)]
pub struct ReplError(pub String);

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for ReplError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keybindings {
    #[default]
    Emacs,
    Vi,
}

pub trait SessionCompleter: Send {
    /// Called with every accepted input line, so completions see session definitions.
    fn accept(&mut self, line: &str);
    /// `position` is a byte offset into `line`.
    fn complete(&mut self, line: &str, position: usize) -> Vec<reedline::Suggestion>;
}

/// Lets a host act between commands without owning the console.
pub trait ConsoleHooks: Send {
    /// Called when the user submits a command at R's top-level prompt, before R reads it. R
    /// code returned here runs first, as its own top-level command.
    fn before_command(&mut self) -> Option<String>;
}

/// Sees R input before the engine parses it: each top-level command typed at the console, and a
/// script fed whole.
pub trait InputFilter: Send {
    fn filter(&mut self, input: &str) -> Filtered;
    /// Ctrl-C at the console drops the input the filter holds.
    fn interrupt(&mut self) {}
}

/// What an [`InputFilter`] does with a piece of input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filtered {
    /// Evaluate this text in place of the input, and of any input held before it.
    Run(String),
    /// The command is unfinished: keep it and read its next line at a continuation prompt.
    Hold,
    /// Drop the input, and any held before it, unevaluated.
    Refuse,
}

#[derive(Default)]
pub struct RunOptions {
    pub keybindings: Keybindings,
    /// `.Call` routines registered before anything runs.
    pub routines: Vec<libr::CallRoutine>,
    /// Evaluated in the global environment after startup and before the first prompt. They run
    /// outside R's top-level loop, so each must catch its own errors.
    pub startup: Vec<String>,
    /// R input fed before `file`; in batch mode a top-level error in it quits.
    pub input: String,
    /// Fed to R before the first prompt.
    pub file: Option<PathBuf>,
    /// What `commandArgs(trailingOnly = TRUE)` returns.
    pub arguments: Vec<String>,
    /// End the session when the script is consumed; a top-level error quits with
    /// a failing status.
    pub batch: bool,
    pub completer: Option<Box<dyn SessionCompleter>>,
    pub hooks: Option<Box<dyn ConsoleHooks>>,
    pub filter: Option<Box<dyn InputFilter>>,
}

/// Returns only when R ends. R assumes it owns the thread it initializes on, so
/// call this from `main` without spawning.
pub fn run(options: RunOptions) -> Result<(), ReplError> {
    let api = libr::load()?;
    console::run(api, options)
}

/// Runs R code on ry's own runtime instead of GNU R: loads ry's `libR` (from `RY_LIBR`, or
/// beside the executable), evaluates `input`, then the script `file`. Both are optional, and
/// `load_all()` is attached before either runs. Returns the exit status.
pub fn run_on_ry_runtime(
    file: Option<&std::path::Path>,
    input: Option<&str>,
    arguments: &[String],
    flags: RuntimeFlags,
) -> Result<i32, ReplError> {
    use std::ffi::{CString, c_char, c_int};
    let library = ry_runtime()?;
    type Run = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        *const *const c_char,
        c_int,
    ) -> c_int;
    let run = runtime_entry::<Run>(library, "ry_run")?;
    let path = file
        .map(|file| CString::new(file.to_string_lossy().as_bytes()))
        .transpose()
        .map_err(|_| ReplError("the script path contains a NUL byte".into()))?;
    let input = input
        .map(CString::new)
        .transpose()
        .map_err(|_| ReplError("the R input contains a NUL byte".into()))?;
    let arguments: Vec<CString> = arguments
        .iter()
        .map(|argument| CString::new(argument.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|_| ReplError("an argument contains a NUL byte".into()))?;
    let pointers: Vec<*const c_char> = arguments.iter().map(|argument| argument.as_ptr()).collect();
    let nullable =
        |value: &Option<CString>| value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    Ok(unsafe {
        run(
            nullable(&path),
            nullable(&input),
            pointers.len() as c_int,
            pointers.as_ptr(),
            flags.bits(),
        )
    })
}

/// What ry's runtime does with inline type annotations; mirrors the `RUN_FLAG_*` bits of
/// `libr::eval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeFlags {
    /// Inline annotations become checks at function boundaries.
    pub annotation_checks: bool,
    /// S4, R6 and Reference Class definitions in user code are allowed.
    pub object_systems: bool,
}

impl RuntimeFlags {
    fn bits(self) -> std::ffi::c_int {
        std::ffi::c_int::from(self.annotation_checks)
            | (std::ffi::c_int::from(self.object_systems) << 1)
    }
}

/// Runs the interactive console on ry's own runtime instead of GNU R. Only the editor, the
/// completer and the filter of `options` apply: ry's runtime has no `.Call` routines to register
/// and runs no startup code.
pub fn run_console_on_ry_runtime(
    options: RunOptions,
    flags: RuntimeFlags,
) -> Result<i32, ReplError> {
    use std::ffi::{c_char, c_int, c_uchar};
    let library = ry_runtime()?;
    type ReadConsole = extern "C" fn(*const c_char, *mut c_uchar, c_int) -> c_int;
    type Repl = unsafe extern "C" fn(ReadConsole, c_int) -> c_int;
    let repl = runtime_entry::<Repl>(library, "ry_repl")?;
    console::install_for_ry_runtime(options)?;
    if std::io::stdin().is_terminal() {
        eprintln!("ry console on ry's runtime. Type q() or Ctrl-D to quit.");
    }
    Ok(unsafe { repl(console::ry_read_console, flags.bits()) })
}

fn ry_runtime() -> Result<&'static libloading::os::unix::Library, ReplError> {
    let library_path = ry_runtime_library()?;
    // Compiled packages record `libR.so` as a needed library; loading ry's globally lets the
    // dynamic linker satisfy them from it.
    let library = unsafe {
        libloading::os::unix::Library::open(
            Some(&library_path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
        )
    }
    .map_err(|error| {
        ReplError(format!(
            "failed to load ry's runtime at {}: {error}",
            library_path.display()
        ))
    })?;
    Ok(Box::leak(Box::new(library)))
}

fn runtime_entry<T: Copy>(
    library: &'static libloading::os::unix::Library,
    name: &str,
) -> Result<T, ReplError> {
    let symbol = format!("{name}\0");
    unsafe { library.get::<T>(symbol.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|error| ReplError(format!("the loaded libR is not ry's runtime: {error}")))
}

fn ry_runtime_library() -> Result<PathBuf, ReplError> {
    if let Some(path) = std::env::var_os("RY_LIBR") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe()
        .map_err(|error| ReplError(format!("cannot locate the ry executable: {error}")))?;
    let directory = executable.parent().unwrap_or(std::path::Path::new("."));
    for candidate in [
        directory.join("libR.so"),
        directory.join("deps").join("libR.so"),
        directory.join("../lib/ry/libR.so"),
    ] {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ReplError(format!(
        "ry's runtime (libR.so) was not found beside {}; build it with `cargo build -p libr` or set RY_LIBR",
        executable.display()
    )))
}
