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

pub mod annotations;
mod approximate;
pub mod arith;
pub mod arrays;
pub mod builtin_libraries;
pub mod builtins;
pub mod bytecode;
pub mod coerce;
pub mod conditions;
pub mod connections;
pub mod data_table;
pub mod dataframe;
pub mod deparse;
pub mod dispatch;
pub mod distributions;
pub mod envir;
pub mod eval;
pub mod format;
pub mod frame;
pub mod gnu_r_home;
pub mod graphics;
pub mod graphics_render;
pub mod http;
pub mod latex;
pub mod linalg;
pub mod long_double;
pub mod lower;
pub mod methods;
pub mod native;
pub mod parallel;
pub mod parse_data;
pub mod print;
pub mod random;
pub mod rd;
pub mod records;
pub mod rmath;
pub mod serialize;
pub mod session;
pub mod sort;
pub mod standard_library;
pub mod stats;
pub mod stats_arima;
pub mod stats_cluster;
pub mod stats_deriv;
pub mod stats_lbfgsb;
pub mod stats_loess;
pub mod stats_models;
pub mod stats_nlm;
pub mod stats_optimize;
pub mod stats_port;
pub mod stats_spline;
pub mod stats_tests;
pub mod strings;
pub mod subset;
pub mod system;
pub mod testing;
pub mod tools;
pub mod unwind;
pub mod utils;
pub mod values;

pub use crate::unwind::{Jump, RError};
use {
    crate::{
        builtins::{BuiltinDef, Kind},
        conditions::{HandlerStack, RestartStack},
        unwind::{R, Unwind},
    },
    heap::{Heap, Marker, RootSource, object::Sexp},
    rustc_hash::FxHashMap,
    std::{cell::UnsafeCell, rc::Rc},
};

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
    /// The srcref current when the call was made (`R_Srcref`), which `sys.call` and `traceback`
    /// attach to the call, and which is current again once the call returns.
    pub srcref: Sexp,
}

/// Services the heap's owner provides because they need native code or process-wide state the
/// interpreter does not own. Each has a working default, so a test harness needs none.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Writes console output: the text, and whether it is error output.
    pub write_console: unsafe fn(&[u8], bool),
    /// Materializes an ALTREP vector into an ordinary one.
    pub materialize_altrep: unsafe fn(Sexp) -> R<Sexp>,
    /// An ALTREP vector's length, from its class's `Length` method.
    pub altrep_length: unsafe fn(Sexp) -> R<usize>,
    /// Runs finalizers whose objects died, at a safe point.
    pub run_finalizers: unsafe fn(),
    /// Runs the top-level task callbacks after a task: the expression, its value, whether it
    /// succeeded, and whether the value was visible.
    pub run_task_callbacks: unsafe fn(Sexp, Sexp, bool, bool) -> R<()>,
    /// Base's task-callback routines, which keep their list in the host so that callbacks
    /// registered from R and from native code share one list and one numbering.
    pub task_callback_routine: unsafe fn(TaskCallbackRoutine, &[Sexp]) -> R<Sexp>,
    /// Calls a C method of a custom connection under the host's native-code boundary, which
    /// turns an R error the method raises into an unwind.
    pub connection_method:
        unsafe fn(connections::Rconnection, connections::NativeMethod<'_>) -> R<()>,
    /// Runs `body` as an entry from native code: the host's boundary turns an unwind into a
    /// `longjmp` into whatever native frame called the connection.
    pub enter_from_native: unsafe fn(&mut dyn FnMut() -> R<()>),
    /// The host's C `vfprintf` method for connections, which formats a C variadic argument
    /// list. Without a host no C code can call it, so it is `None`.
    pub connection_vfprintf: Option<connections::OpaqueMethod>,
}

/// Base's native routines behind `addTaskCallback()`, `removeTaskCallback()` and
/// `getTaskCallbackNames()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskCallbackRoutine {
    Add,
    Remove,
    Names,
}

pub struct Interp {
    pub frames: Vec<Frame>,
    /// The handler and restart stacks in effect, which every frame saves and restores.
    pub handlers: HandlerStack,
    pub restarts: RestartStack,
    /// `globalCallingHandlers()`: the bottom of the handler stack, where every top-level task
    /// starts.
    pub global_handlers: HandlerStack,
    /// `R_Srcref`: the srcref of the statement being evaluated, or `NULL`. A `{` with source
    /// references sets it for each statement, and a closure call sets it to the closure's own.
    pub srcref: Sexp,
    pub builtins: Vec<BuiltinDef>,
    pub builtin_index: FxHashMap<&'static str, u32>,
    /// The formals `.ArgsEnv` or `.GenericArgsEnv` give each builtin, read on first use;
    /// `None` for a builtin with `...` or with no entry, whose argument names go unchecked.
    pub primitive_formals: FxHashMap<&'static str, Option<Box<[String]>>>,
    pub symbols: values::Symbols,
    /// `R_Visible`: whether the value of the last top-level evaluation prints.
    pub visible: bool,
    /// `returnValue()` for the `on.exit` code running innermost: the value its function
    /// returns, or `None` when the function is being left by an error or a jump past it.
    pub returned_values: Vec<Option<Sexp>>,
    /// The innermost complex assignment running: its replacement call, the whole assignment,
    /// and the frame depth it runs at. GNU R evaluates the replacement inside a context for the
    /// assignment, so a builtin replacement function reports its errors and warnings against
    /// `x[i] <- v`, not `` `[<-`(*tmp*, i, v) `` or the enclosing function's call.
    pub assignment_call: Option<(Sexp, Sexp, usize)>,
    /// Warnings deferred to the end of the top-level call, with the calls they came from.
    pub warnings: Vec<(Sexp, String)>,
    /// Set while `warning(immediate. = TRUE)` signals, so the default handler prints at once.
    pub immediate_warning: bool,
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
    /// `interactive()`: whether a person reads the output, as in a console.
    pub interactive: bool,
    /// The default packages' libraries `dyn.load` loaded, with the paths it was given.
    pub builtin_libraries: Vec<(&'static str, String)>,
    /// Inline type annotations turned into checks at function boundaries.
    pub annotations: annotations::State,
    /// Whether S4, R6 and Reference Class definitions in user code are allowed. A host turns it
    /// off when the project sets `[check] object-systems = false`.
    pub object_systems: bool,
    /// An unwind raised where it cannot propagate (an ALTREP `Elt` method called through the
    /// infallible element accessor), raised by the next [`crate::unwind::poll_interrupt`].
    pub parked_unwind: Option<Unwind>,
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
    let connection_table = connections::Table::new(hooks.connection_vfprintf);
    state.interp = Some(Interp {
        frames: Vec::new(),
        handlers: None,
        restarts: None,
        global_handlers: None,
        srcref: heap.known().nil,
        builtins: Vec::new(),
        builtin_index: FxHashMap::default(),
        primitive_formals: FxHashMap::default(),
        symbols,
        visible: true,
        returned_values: Vec::new(),
        assignment_call: None,
        warnings: Vec::new(),
        immediate_warning: false,
        error_message: String::new(),
        show_error_messages: true,
        in_error: 0,
        globals: values::Globals::placeholder(),
        hooks,
        bytecode: bytecode::State::default(),
        random: random::Rng::default(),
        connections: connection_table,
        depth: 0,
        max_depth: 5000,
        started: std::time::Instant::now(),
        command_args: Vec::new(),
        interactive: false,
        builtin_libraries: Vec::new(),
        annotations: annotations::State::default(),
        object_systems: true,
        parked_unwind: None,
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
/// finalizers, and no task callbacks.
pub fn default_hooks() -> Hooks {
    Hooks {
        write_console: default_write_console,
        materialize_altrep: |_| Err(unwind::error("ALTREP objects need the libR host")),
        altrep_length: |_| Err(unwind::error("ALTREP objects need the libR host")),
        run_finalizers: || {},
        run_task_callbacks: |_, _, _, _| Ok(()),
        task_callback_routine: |_, _| {
            Err(unwind::error("top-level task callbacks need the libR host"))
        },
        connection_method: |_, _| Err(unwind::error("custom connections need the libR host")),
        enter_from_native: |body| {
            // Only native code calls this hook, and without the libR host there is none.
            if body().is_err() {
                panic!("an R error escaped into native code without the libR host");
            }
        },
        connection_vfprintf: None,
    }
}

unsafe fn default_write_console(text: &[u8], is_error: bool) {
    use std::io::Write;
    // Console output that cannot be written has nowhere else to go, as in GNU R.
    let written = if is_error {
        std::io::stdout()
            .flush()
            .and_then(|()| std::io::stderr().write_all(text))
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
        // SAFETY: the connection table only holds connections of this session.
        unsafe { connections::write_message(text) }
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
                    frame.srcref,
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
            conditions::trace_handlers(&interp.global_handlers, marker);
            conditions::trace_restarts(&interp.restarts, marker);
            marker.mark(interp.srcref);
            if let Some(unwind) = &interp.parked_unwind {
                crate::unwind::trace_unwind(unwind, marker);
            }
            if let Some((replacement, whole, _)) = interp.assignment_call {
                marker.mark(replacement);
                marker.mark(whole);
            }
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
            interp.annotations.trace(marker);
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
        self.builtin(index).is_some_and(|definition| {
            matches!(definition.kind, Kind::Internal | Kind::InternalSpecial)
        })
    }
}
