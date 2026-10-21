//! An R console that `dlopen`s the R shared library and resolves every C-API
//! symbol at runtime, so nothing here needs R at build time.

pub mod console;
pub mod libr;

// Re-exported so hosts implement `SessionCompleter` against the same reedline version.
pub use reedline;

use std::fmt;

#[derive(Debug)]
pub struct ReplError(pub String);

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReplError {}

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

#[derive(Default)]
pub struct RunOptions {
    pub keybindings: Keybindings,
    /// Fed to R before the first prompt.
    pub file: Option<std::path::PathBuf>,
    /// End the session when the script is consumed; a top-level error quits with
    /// a failing status.
    pub batch: bool,
    pub completer: Option<Box<dyn SessionCompleter>>,
}

/// Returns only when R ends. R assumes it owns the thread it initializes on, so
/// call this from `main` without spawning.
pub fn run(options: RunOptions) -> Result<(), ReplError> {
    let api = libr::load()?;
    console::run(api, options)
}
