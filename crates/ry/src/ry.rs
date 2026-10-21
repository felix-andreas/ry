pub mod cli;
pub mod config;
pub mod diagnostics;
pub mod namespace;
pub mod position;
pub mod repl_completer;
pub mod server;
pub mod stats;

/// Analysis walks recurse as deeply as the source nests, so commands and the
/// analysis worker run on threads with this stack rather than `ulimit -s`'s.
pub const ANALYSIS_STACK_SIZE: usize = 64 * 1024 * 1024;
