//! An interactive R console with ry's language smarts, embedding R
//! WITHOUT a build-time link dependency: the R shared library is located and
//! `dlopen`ed at runtime, and every C-API symbol is resolved by name
//! (`libr`). The design record is the REPL design page under
//! `docs/src/content/docs/contributing/design/`; the short version:
//!
//! - R owns the calling thread and runs its REAL main loop
//!   (`run_Rmainloop`); the console lives inside R's `ReadConsole` callback,
//!   where a `reedline` editor collects input, ry's own parser decides
//!   completeness (continuation prompts), and finished input is fed back to
//!   R through the console buffer for R to parse, evaluate, and autoprint.
//! - Interrupts flow through `R_interrupts_pending`: Ctrl-C during
//!   evaluation is a SIGINT our handler translates into R's cooperative
//!   flag; Ctrl-C at the prompt just clears the line (reedline). Windows
//!   routes the same pair of flags through the console control handler.
//! - Nothing here requires R at build time; a missing R at RUN time is a
//!   clear, actionable error.
//! - Tab completion is host-injected ([`SessionCompleter`]): the analysis
//!   stack lives in the host crate, so this crate's dependency set stays
//!   syntax-only.

pub mod console;
pub mod libr;

// Hosts implement `SessionCompleter` against reedline's `Suggestion` type;
// re-exporting the crate guarantees they build against the same version.
pub use reedline;

use std::fmt;

/// Everything that can stop the REPL from starting: no R on the machine, an
/// R built without `--enable-R-shlib`, a shared library that loads but lacks
/// a required symbol. Each carries the fix in its message.
#[derive(Debug)]
pub struct ReplError(pub String);

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReplError {}

/// The console's editing keybindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keybindings {
    #[default]
    Emacs,
    Vi,
}

/// A host-supplied completion source for the interactive editor. The console
/// feeds every accepted input line back through [`SessionCompleter::accept`],
/// so completions can see the session's own definitions; the analysis-backed
/// implementation lives in the host crate, keeping this crate's dependency
/// set syntax-only.
pub trait SessionCompleter: Send {
    /// One accepted input line, which extends the completion context.
    fn accept(&mut self, line: &str);
    /// Completions for `line` with the cursor at byte `position`.
    fn complete(&mut self, line: &str, position: usize) -> Vec<reedline::Suggestion>;
}

/// How a session starts: interactively, with a script pre-loaded before the
/// first prompt, or as a batch run that ends at the script's end.
#[derive(Default)]
pub struct RunOptions {
    pub keybindings: Keybindings,
    /// A script fed to R before any prompt.
    pub file: Option<std::path::PathBuf>,
    /// End the session when the script is consumed instead of prompting
    /// (`ry run`): a top-level error quits with a failing status, so the
    /// process exit code reflects the script's outcome.
    pub batch: bool,
    /// Tab-completion source for the interactive editor.
    pub completer: Option<Box<dyn SessionCompleter>>,
}

/// Starts the session on the CALLING thread and only returns when R ends
/// (`q()`, EOF, or the end of a batch script). R assumes it owns the thread
/// it initializes on, so run this from `main` without spawning.
pub fn run(options: RunOptions) -> Result<(), ReplError> {
    let api = libr::load()?;
    console::run(api, options)
}
