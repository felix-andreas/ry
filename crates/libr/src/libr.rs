//! ry's implementation of R's C API, built as `libR`, the shared library compiled R packages link
//! against. It exports GNU R's function and data symbols with GNU R's calling conventions, so a
//! package built against ry's headers or against GNU R's loads and runs unchanged.
//!
//! The object model and the collector live in the `heap` crate. This crate owns the runtime
//! state, the native boundary (`boundary`, with its C half in `native.c`), and the API itself,
//! grouped by subsystem. The API is single-threaded, as GNU R's is.

// The exported names are R's, so they follow C conventions rather than Rust's.
#![allow(non_snake_case, non_upper_case_globals, non_camel_case_types)]
// Every exported function dereferences handles that native code passes in, so the crate is one
// FFI layer and an `unsafe` block inside each `unsafe fn` would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes come from this runtime and are
// live, as GNU R requires of the same functions.
#![allow(clippy::missing_safety_doc)]

pub mod altrep;
pub mod arith;
pub mod attributes;
pub mod boundary;
pub mod connections;
pub mod dynload;
pub mod embed;
pub mod environments;
pub mod eval;
pub mod extptr;
pub mod globals;
pub mod lists;
pub mod memory;
pub mod strings;
pub mod vectors;

use crate::{boundary::Unwind, dynload::Dll};
use heap::{Heap, arena::Arena, object::Sexp};
use rustc_hash::FxHashMap;
use std::{cell::UnsafeCell, ffi::c_int};

pub type SEXP = Sexp;
pub type SEXPTYPE = u32;
pub type R_xlen_t = isize;
pub type R_len_t = c_int;
/// `Rboolean` is a C enum, so it has the size of `int`.
pub type Rboolean = c_int;
pub const TRUE: Rboolean = 1;
pub const FALSE: Rboolean = 0;

/// The process-wide runtime, as GNU R's API assumes: there are no context arguments.
pub struct Runtime {
    pub heap: Heap,
    pub arena: Arena,
    /// How many native calls are on the stack. Objects the API hands to native code are rooted
    /// in the handle stack while this is nonzero.
    pub native_depth: u32,
    /// The unwind in flight between an API function that raised it and the C context that
    /// catches it.
    pub pending: Option<Unwind>,
    /// Unwinds parked in `R_UnwindProtect` continuation tokens until `R_ContinueUnwind`.
    pub parked: FxHashMap<Sexp, Unwind>,
    /// The last error message, as `R_curErrorBuf` returns it.
    pub error_buffer: Vec<u8>,
    pub warnings: Vec<String>,
    pub dlls: Vec<Box<Dll>>,
    pub c_callables: FxHashMap<(String, String), usize>,
    pub options: Sexp,
    pub rng: arith::Rng,
    pub console: Console,
    running_finalizers: bool,
}

/// Where `Rprintf` and `REprintf` output goes: the text, and whether it is error output.
pub type Console = Box<dyn FnMut(&[u8], bool)>;

static RUNTIME: Global<Option<Runtime>> = Global::new(None);

/// A mutable static with C linkage that native code reads directly, such as `R_NilValue`.
#[repr(transparent)]
pub struct Global<T>(UnsafeCell<T>);

// SAFETY: R's API is single-threaded; every access happens on the thread that runs R code.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    pub const fn new(value: T) -> Global<T> {
        Global(UnsafeCell::new(value))
    }

    pub fn as_ptr(&self) -> *mut T {
        self.0.get()
    }
}

impl<T: Copy> Global<T> {
    pub fn get(&self) -> T {
        // SAFETY: single-threaded access, see the `Sync` impl.
        unsafe { *self.0.get() }
    }

    pub fn set(&self, value: T) {
        // SAFETY: single-threaded access, see the `Sync` impl.
        unsafe { *self.0.get() = value }
    }
}

/// The runtime, starting it on first use.
///
/// The reference must not be held across a call into native code, which may re-enter the API
/// and take its own. Functions that call native code take the runtime afresh afterwards.
#[allow(clippy::mut_from_ref)]
pub fn runtime() -> &'static mut Runtime {
    // SAFETY: single-threaded access, see `Global`.
    unsafe {
        let slot = &mut *RUNTIME.as_ptr();
        if slot.is_none() {
            *slot = Some(Runtime::start());
            globals::publish(slot.as_mut().unwrap_or_else(|| unreachable!()));
        }
        slot.as_mut().unwrap_or_else(|| unreachable!())
    }
}

impl Runtime {
    fn start() -> Runtime {
        let mut heap = Heap::new();
        let empty = heap.known().empty_env;
        // SAFETY: the heap is fresh; the options environment is rooted before anything allocates.
        let options = unsafe { heap.alloc_environment(empty) }
            .unwrap_or_else(|error| panic!("cannot create the options environment: {error}"));
        heap.add_global_root(options);
        let mut runtime = Runtime {
            heap,
            arena: Arena::default(),
            native_depth: 0,
            pending: None,
            parked: FxHashMap::default(),
            error_buffer: Vec::new(),
            warnings: Vec::new(),
            dlls: Vec::new(),
            c_callables: FxHashMap::default(),
            options,
            rng: arith::Rng::default(),
            console: Box::new(boundary::default_console),
            running_finalizers: false,
        };
        // SAFETY: the heap is fresh and collection is inhibited while the symbols are created.
        unsafe {
            runtime.heap.inhibit_collection();
            globals::install_symbols(&mut runtime.heap);
            eval::install_primitives(&mut runtime.heap);
            runtime.heap.allow_collection();
        }
        runtime
    }

    pub fn nil(&self) -> Sexp {
        self.heap.nil()
    }
}
