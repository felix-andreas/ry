//! ry's R interpreter. R source becomes R language objects (`lower`), exactly as GNU R's parser
//! builds them, because `quote`, `substitute`, `deparse`, `body` and `match.call` expose them.
//! Closures run through a tier-0 bytecode interpreter (`bytecode`) with inline caches and type
//! feedback, which falls back to the language-object evaluator (`eval`) for anything it does not
//! compile. Base R's primitives and internals are Rust (`builtins` and the modules it lists); base
//! R's R code is loaded at runtime from the user's GNU R installation (`session`).
//!
//! The interpreter does not own the heap. Whoever does (the `libr` crate, or a test harness)
//! hands it over with [`install`] and registers the services that need native code, such as
//! `.Call`, as extra builtins with [`register_builtins`]. R's API is single-threaded, so the
//! interpreter state is one process-wide value, as GNU R's is.
//!
//! **Rooting.** Every value the interpreter allocates or evaluates is rooted in the heap's handle
//! stack, and every builtin call, closure call and loop iteration runs in a handle scope that
//! truncates the stack on exit and re-roots its result ([`scope`]). A Rust local therefore stays
//! alive for as long as the scope that produced it, and builtins can be written without manual
//! protection.

// The interpreter dereferences raw handles everywhere, so an `unsafe` block inside each
// `unsafe fn` would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes are live objects of this heap.
#![allow(clippy::missing_safety_doc)]

pub mod arith;
pub mod builtin_libraries;
pub mod builtins;
pub mod bytecode;
pub mod coerce;
pub mod conditions;
pub mod connections;
pub mod deparse;
pub mod distributions;
pub mod dispatch;
pub mod envir;
pub mod eval;
pub mod format;
pub mod linalg;
pub mod graphics;
pub mod lower;
pub mod methods;
pub mod native;
pub mod print;
pub mod random;
pub mod serialize;
pub mod session;
pub mod sort;
pub mod stats;
pub mod stats_lbfgsb;
pub mod stats_models;
pub mod stats_optimize;
pub mod strings;
pub mod subset;
pub mod system;
pub mod tools;
pub mod unwind;
pub mod utils;
pub mod values;

use crate::{
    builtins::{BuiltinDef, Kind},
    conditions::{HandlerStack, RestartStack},
    unwind::{R, Unwind},
};
use heap::{Heap, Marker, RootSource, object::Sexp};
use rustc_hash::FxHashMap;
use std::{cell::UnsafeCell, rc::Rc};

pub use crate::unwind::{Jump, RError};

/// One closure call: GNU R's function context.
pub struct Frame {
    /// The call as written, which `sys.call` returns.
    pub call: Sexp,
    pub function: Sexp,
    /// The environment the closure body runs in.
    pub env: Sexp,
    /// The environment the call was evaluated in, which `parent.frame` returns.
    pub sys_parent: Sexp,
    /// The matched arguments as a pairlist of promises, tagged with the formal names.
    pub promargs: Sexp,
    /// `on.exit` expressions, in the order they run.
    pub on_exit: Vec<Sexp>,
    pub handlers: HandlerStack,
    pub restarts: RestartStack,
}

/// Services the heap's owner provides because they need native code or process-wide state the
/// interpreter does not own. Each has a working default, so a test harness needs none.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Writes console output: the text, and whether it is error output.
    pub write_console: unsafe fn(&[u8], bool),
    /// Materializes an ALTREP vector into an ordinary one.
    pub materialize_altrep: unsafe fn(Sexp) -> R<Sexp>,
    /// Runs finalizers whose objects died, at a safe point.
    pub run_finalizers: unsafe fn(),
}

pub struct Interp {
    pub frames: Vec<Frame>,
    /// The handler and restart stacks in effect, which every frame saves and restores.
    pub handlers: HandlerStack,
    pub restarts: RestartStack,
    pub builtins: Vec<BuiltinDef>,
    pub builtin_index: FxHashMap<&'static str, u32>,
    pub symbols: values::Symbols,
    /// `R_Visible`: whether the value of the last top-level evaluation prints.
    pub visible: bool,
    /// Warnings deferred to the end of the top-level call, with the calls they came from.
    pub warnings: Vec<(Sexp, String)>,
    /// The text `geterrmessage()` returns.
    pub error_message: String,
    /// Whether the default error handler prints (`R_ShowErrorMessages`).
    pub show_error_messages: bool,
    /// Depth of nested default error handling, to stop an error in `options(error=)` looping.
    pub in_error: u32,
    /// Objects that live as long as the session: the options list, `.Random.seed` helpers, and
    /// tables the builtins keep.
    pub globals: values::Globals,
    pub hooks: Hooks,
    pub bytecode: bytecode::State,
    pub random: random::Rng,
    pub connections: connections::Table,
    /// Evaluation depth, for `expressions` and the C stack check.
    pub depth: u32,
    pub max_depth: u32,
    /// Process start, for `proc.time`.
    pub started: std::time::Instant,
    /// `commandArgs()`.
    pub command_args: Vec<String>,
    /// The default packages' libraries `dyn.load` loaded, with the paths it was given.
    pub builtin_libraries: Vec<(&'static str, String)>,
}

struct InterpState {
    heap: *mut Heap,
    interp: Option<Interp>,
}

/// A mutable process-wide value. R's API is single-threaded; every access happens on the thread
/// that runs R code.
struct Global<T>(UnsafeCell<T>);

// SAFETY: single-threaded access, see the type's documentation.
unsafe impl<T> Sync for Global<T> {}

static STATE: Global<InterpState> = Global(UnsafeCell::new(InterpState {
    heap: std::ptr::null_mut(),
    interp: None,
}));

/// The heap the interpreter runs on.
///
/// # Panics
/// Panics if [`install`] has not run, which is a host bug.
#[allow(clippy::mut_from_ref)]
#[inline]
pub fn heap() -> &'static mut Heap {
    // SAFETY: single-threaded access; the pointer is set once by `install` and stays valid for the
    // life of the process, because the host keeps its heap in static storage.
    unsafe {
        let state = &*STATE.0.get();
        assert!(!state.heap.is_null(), "the interpreter is not installed");
        &mut *state.heap
    }
}

/// The interpreter state.
#[allow(clippy::mut_from_ref)]
#[inline]
pub fn interp() -> &'static mut Interp {
    // SAFETY: single-threaded access, as for `heap`.
    unsafe {
        let state = &mut *STATE.0.get();
        state
            .interp
            .as_mut()
            .unwrap_or_else(|| panic!("the interpreter is not installed"))
    }
}

pub fn is_installed() -> bool {
    // SAFETY: single-threaded access, as for `heap`.
    unsafe { (*STATE.0.get()).interp.is_some() }
}

/// Installs the interpreter on `heap`, which must stay at this address for the life of the
/// process. Installing twice does nothing.
///
/// # Safety
/// `heap` must be valid and must not move or be dropped while the interpreter runs.
pub unsafe fn install(heap_pointer: *mut Heap, hooks: Hooks) {
    let state = &mut *STATE.0.get();
    if state.interp.is_some() {
        return;
    }
    state.heap = heap_pointer;
    let heap = &mut *heap_pointer;
    heap.inhibit_collection();
    let symbols = values::Symbols::install(heap);
    state.interp = Some(Interp {
        frames: Vec::new(),
        handlers: None,
        restarts: None,
        builtins: Vec::new(),
        builtin_index: FxHashMap::default(),
        symbols,
        visible: true,
        warnings: Vec::new(),
        error_message: String::new(),
        show_error_messages: true,
        in_error: 0,
        globals: values::Globals::placeholder(),
        hooks,
        bytecode: bytecode::State::default(),
        random: random::Rng::default(),
        connections: connections::Table::default(),
        depth: 0,
        max_depth: 5000,
        started: std::time::Instant::now(),
        command_args: Vec::new(),
        builtin_libraries: Vec::new(),
    });
    heap.add_root_source(Rc::new(InterpRoots));
    values::Globals::install();
    builtins::install_all();
    heap.allow_collection();
}

/// Adds builtins the host implements, such as the foreign-function interface. A name that is
/// already registered is replaced, so the host's version wins.
pub unsafe fn register_builtins(definitions: &[BuiltinDef]) {
    builtins::register(definitions);
}

/// The default hooks: console output to the process's stdout and stderr, no ALTREP classes, no
/// finalizers.
pub fn default_hooks() -> Hooks {
    Hooks {
        write_console: default_write_console,
        materialize_altrep: |_| Err(unwind::error("ALTREP objects need the libR host")),
        run_finalizers: || {},
    }
}

unsafe fn default_write_console(text: &[u8], is_error: bool) {
    use std::io::Write;
    // Console output that cannot be written has nowhere else to go, as in GNU R.
    let written = if is_error {
        std::io::stdout().flush().and_then(|()| std::io::stderr().write_all(text))
    } else {
        std::io::stdout().write_all(text)
    };
    drop(written);
}

/// Writes to the console through the host.
/// Writes console output: standard output goes where `sink` diverts it, errors and messages to
/// the terminal.
pub fn write_console(text: &[u8], is_error: bool) {
    if is_error {
        write_terminal(text, true);
    } else {
        // SAFETY: the sink table only holds connections of this session.
        unsafe { connections::write_output(text) }
    }
}

/// Writes to the host's console, bypassing any `sink`.
pub fn write_terminal(text: &[u8], is_error: bool) {
    // SAFETY: the hook is the host's, which accepts any bytes.
    unsafe { (interp().hooks.write_console)(text, is_error) }
}

/// Runs `body` in a handle scope: whatever it roots is released on exit, and its result (or the
/// objects its unwind carries) is rooted again in the enclosing scope.
#[inline]
pub unsafe fn scope<T: unwind::Rooted>(body: impl FnOnce() -> R<T>) -> R<T> {
    let heap = heap();
    let mark = heap.handle_mark();
    let result = body();
    let heap = self::heap();
    heap.release_handles(mark);
    match &result {
        Ok(value) => value.root(),
        Err(unwind) => unwind.root(),
    }
    result
}

struct InterpRoots;

impl RootSource for InterpRoots {
    fn trace(&self, marker: &mut Marker<'_>) {
        // SAFETY: the collector runs on the interpreter's thread, and every handle traced here
        // belongs to a live frame, stack entry or table.
        unsafe {
            let interp = interp();
            for frame in &interp.frames {
                for object in [
                    frame.call,
                    frame.function,
                    frame.env,
                    frame.sys_parent,
                    frame.promargs,
                ] {
                    marker.mark(object);
                }
                for &expression in &frame.on_exit {
                    marker.mark(expression);
                }
                conditions::trace_handlers(&frame.handlers, marker);
                conditions::trace_restarts(&frame.restarts, marker);
            }
            conditions::trace_handlers(&interp.handlers, marker);
            conditions::trace_restarts(&interp.restarts, marker);
            for &(call, _) in &interp.warnings {
                marker.mark(call);
            }
            interp.globals.trace(marker);
            interp.symbols.trace(marker);
            for definition in &interp.builtins {
                if let Some(object) = definition.object {
                    marker.mark(object);
                }
            }
            interp.bytecode.trace(marker);
            interp.connections.trace(marker);
        }
    }
}

impl Interp {
    pub fn builtin(&self, index: u32) -> Option<&BuiltinDef> {
        self.builtins.get(index as usize)
    }

    pub fn builtin_named(&self, name: &str) -> Option<&BuiltinDef> {
        self.builtin_index
            .get(name)
            .and_then(|&index| self.builtins.get(index as usize))
    }

    /// The innermost closure frame, if any.
    pub fn current_frame(&self) -> Option<&Frame> {
        self.frames.last()
    }

    pub fn is_internal(&self, index: u32) -> bool {
        self.builtin(index)
            .is_some_and(|definition| matches!(definition.kind, Kind::Internal | Kind::InternalSpecial))
    }
}
