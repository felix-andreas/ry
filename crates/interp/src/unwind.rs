//! Non-local exits. Inside Rust every one is an `Err(Unwind)` that `?` carries up the stack, and
//! the frame it is meant for catches it.
//!
//! An [`Unwind::Error`] is an error that has not been signalled yet: a builtin or native code
//! raised it, and the evaluator signals it (runs the handlers, then the default handler) at the
//! nearest builtin boundary. Everything R-level is a [`Jump`]: `return`, `break` and `next`, an
//! exiting handler or restart (both are returns to a closure frame), and the jump to top level
//! after the default error handler ran.

use {
    crate::interp,
    heap::object::Sexp,
    std::sync::atomic::{AtomicBool, Ordering},
};

pub type R<T> = Result<T, Unwind>;

/// Set by a host's SIGINT handler (an atomic store is async-signal-safe). The evaluator polls it
/// at every call and every loop back edge, so Ctrl-C stops even a tight loop.
pub static INTERRUPT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Raises a pending user interrupt.
#[inline]
pub fn poll_interrupt() -> R<()> {
    if INTERRUPT_REQUESTED.load(Ordering::Relaxed) {
        INTERRUPT_REQUESTED.store(false, Ordering::Relaxed);
        return Err(Unwind::Interrupt);
    }
    Ok(())
}

#[derive(Debug)]
pub enum Unwind {
    Error(RError),
    Interrupt,
    Jump(Box<Jump>),
}

#[derive(Debug)]
pub struct RError {
    pub message: String,
    /// The call the error is reported against. `R_NilValue` means none, and `R_UnboundValue`
    /// means the innermost closure call, which is what GNU R's `error()` reports.
    pub call: Sexp,
}

#[derive(Debug)]
pub enum Jump {
    /// Return `value` from the closure frame whose environment is `env`. Exiting handlers and
    /// restarts are returns to the frames `tryCatch` and `withRestarts` set up.
    Return {
        env: Sexp,
        value: Sexp,
    },
    Break {
        env: Sexp,
    },
    Next {
        env: Sexp,
    },
    /// The default error handler ran, so the error goes to the innermost top-level context.
    TopLevel,
}

/// An error reported against the innermost closure call.
pub fn error(message: impl Into<String>) -> Unwind {
    Unwind::Error(RError {
        message: message.into(),
        call: crate::heap().known().unbound,
    })
}

/// An error reported against `call` (`R_NilValue` for none).
pub fn error_in(call: Sexp, message: impl Into<String>) -> Unwind {
    Unwind::Error(RError {
        message: message.into(),
        call,
    })
}

pub fn jump(jump: Jump) -> Unwind {
    Unwind::Jump(Box::new(jump))
}

impl From<heap::HeapError> for Unwind {
    fn from(error: heap::HeapError) -> Unwind {
        self::error(error.to_string())
    }
}

/// A value that may hold handles, which [`crate::scope`] roots again after its handle scope ends.
pub trait Rooted {
    fn root(&self) {}
}

impl Rooted for Sexp {
    #[inline]
    fn root(&self) {
        crate::heap().root(*self);
    }
}

impl Rooted for Option<Sexp> {
    fn root(&self) {
        if let Some(object) = self {
            crate::heap().root(*object);
        }
    }
}

impl Rooted for Vec<Sexp> {
    fn root(&self) {
        for object in self {
            crate::heap().root(*object);
        }
    }
}

impl<A: Rooted, B: Rooted> Rooted for (A, B) {
    fn root(&self) {
        self.0.root();
        self.1.root();
    }
}

macro_rules! plain_rooted {
    ($($type:ty),*) => { $(impl Rooted for $type {})* };
}

plain_rooted!((), bool, i32, u32, usize, isize, f64, String);

impl Unwind {
    pub fn root(&self) {
        let heap = crate::heap();
        match self {
            Unwind::Error(error) => heap.root(error.call),
            Unwind::Interrupt => {}
            Unwind::Jump(jump) => match **jump {
                Jump::Return { env, value } => {
                    heap.root(env);
                    heap.root(value);
                }
                Jump::Break { env } | Jump::Next { env } => heap.root(env),
                Jump::TopLevel => {}
            },
        }
    }

    /// Whether this is a return to the frame whose environment is `env`.
    pub fn returns_to(&self, env: Sexp) -> bool {
        matches!(self, Unwind::Jump(jump) if matches!(**jump, Jump::Return { env: target, .. } if target == env))
    }
}

/// The message of an error as R reports it, with its call resolved.
pub unsafe fn resolve_call(call: Sexp) -> Sexp {
    if call == crate::heap().known().unbound {
        current_call()
    } else {
        match interp().assignment_call {
            Some((replacement, whole, _)) if replacement == call => whole,
            _ => call,
        }
    }
}

/// The call of the innermost closure frame, or `NULL` at top level.
pub unsafe fn current_call() -> Sexp {
    let state = interp();
    if let Some((_, whole, depth)) = state.assignment_call
        && depth == state.frames.len()
    {
        return whole;
    }
    state
        .frames
        .last()
        .map_or(crate::heap().nil(), |frame| frame.call)
}
