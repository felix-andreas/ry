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
