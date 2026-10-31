//! An R console that `dlopen`s the R shared library and resolves every C-API
//! symbol at runtime, so nothing here needs R at build time.

pub mod console;
pub mod libr;

// Re-exported so hosts implement `SessionCompleter` against the same reedline version.
pub use reedline;

use std::error::Error;
use std::fmt;
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
    /// End the session when the script is consumed; a top-level error quits with
    /// a failing status.
    pub batch: bool,
    pub completer: Option<Box<dyn SessionCompleter>>,
    pub hooks: Option<Box<dyn ConsoleHooks>>,
}

/// Returns only when R ends. R assumes it owns the thread it initializes on, so
/// call this from `main` without spawning.
pub fn run(options: RunOptions) -> Result<(), ReplError> {
    let api = libr::load()?;
    console::run(api, options)
}

/// Runs an R script on ry's own runtime instead of GNU R: loads ry's `libR` (from `RY_LIBR`, or
/// beside the executable) and hands it the script. Returns the script's exit status.
pub fn run_on_ry_runtime(file: &std::path::Path, arguments: &[String]) -> Result<i32, ReplError> {
    use std::ffi::{CString, c_char, c_int};
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
    let library: &'static libloading::os::unix::Library = Box::leak(Box::new(library));
    type RunFile = unsafe extern "C" fn(*const c_char, c_int, *const *const c_char) -> c_int;
    let run = unsafe { library.get::<RunFile>(b"ry_run_file\0") }.map_err(|error| {
        ReplError(format!(
            "{} is not ry's runtime: {error}",
            library_path.display()
        ))
    })?;
    let path = CString::new(file.to_string_lossy().as_bytes())
        .map_err(|_| ReplError("the script path contains a NUL byte".into()))?;
    let arguments: Vec<CString> = arguments
        .iter()
        .map(|argument| CString::new(argument.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|_| ReplError("an argument contains a NUL byte".into()))?;
    let pointers: Vec<*const c_char> = arguments.iter().map(|argument| argument.as_ptr()).collect();
    Ok(unsafe { run(path.as_ptr(), pointers.len() as c_int, pointers.as_ptr()) })
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
