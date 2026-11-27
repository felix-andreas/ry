//! Non-local exits. Inside Rust every one is an `Err(Unwind)` that `?` carries up the stack, and
//! the frame it is meant for catches it.
//!
//! An [`Unwind::Error`] is an error that has not been signalled yet: a builtin or native code
//! raised it, and the evaluator signals it (runs the handlers, then the default handler) at the
//! nearest builtin boundary. Everything R-level is a [`Jump`]: `return`, `break` and `next`, an
//! exiting handler or restart (both are returns to a closure frame), and the jump to top level
//! after the default error handler ran.

use {
    crate::{interp, system::r_at_least},
    heap::{
        Marker,
        object::{Sexp, SexpType},
    },
    smallvec::SmallVec,
    std::sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

pub type R<T> = Result<T, Unwind>;

/// Raised whenever the evaluator has something to act on at its next poll: a pending user
/// interrupt, an editor's request to the console, or an unwind [`park`] set aside. The evaluator
/// polls it at every call and every loop back edge, and compiled code at its back edges, so the
/// fast path is one load.
pub static INTERRUPT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// GNU R's `R_interrupts_pending`: a user interrupt waits to be delivered. Native code reads and
/// writes it under that name, so it is the one record of a pending interrupt. An atomic `i32`
/// has the layout of the C `int` packages see.
#[unsafe(export_name = "R_interrupts_pending")]
pub static INTERRUPTS_PENDING: AtomicI32 = AtomicI32::new(0);

/// GNU R's `R_interrupts_suspended`: while it is nonzero, an interrupt stays pending instead of
/// being delivered. `suspendInterrupts()` sets it through `.Internal(interruptsSuspended())`, and
/// native code sets it directly, as `BEGIN_SUSPEND_INTERRUPTS` does.
#[unsafe(export_name = "R_interrupts_suspended")]
pub static INTERRUPTS_SUSPENDED: AtomicI32 = AtomicI32::new(0);

/// How many evaluator polls pass between two checks that deliver a pending user interrupt. GNU R's
/// `eval` calls `R_CheckUserInterrupt` once every thousand evaluations, so an interrupt that
/// becomes deliverable, such as one left pending when `suspendInterrupts()` returns, fires a little
/// later in a loop rather than at the very next call.
pub(crate) const INTERRUPT_CHECK_INTERVAL: u32 = 1000;

/// Requests a user interrupt, as Ctrl-C does. It only stores to atomics, so a signal handler may
/// call it.
pub fn request_interrupt() {
    INTERRUPTS_PENDING.store(1, Ordering::Relaxed);
    INTERRUPT_REQUESTED.store(true, Ordering::Relaxed);
}

/// The evaluator's poll at every call and loop back edge. It answers an editor's request and
/// raises a parked unwind at once, but it delivers a pending user interrupt only at every
/// [`INTERRUPT_CHECK_INTERVAL`]th poll, the way GNU R's `eval` checks for one.
#[inline]
pub fn poll_interrupt() -> R<()> {
    if INTERRUPT_REQUESTED.load(Ordering::Relaxed) {
        return attend();
    }
    Ok(())
}

#[cold]
fn attend() -> R<()> {
    take_requests()?;
    if INTERRUPTS_PENDING.load(Ordering::Relaxed) == 0
        && !crate::time_limits::DUE.load(Ordering::Relaxed)
    {
        return Ok(());
    }
    let interp = interp();
    interp.interrupt_countdown = interp.interrupt_countdown.saturating_sub(1);
    if interp.interrupt_countdown > 0 {
        return Ok(());
    }
    interp.interrupt_countdown = INTERRUPT_CHECK_INTERVAL;
    check_user_interrupt()
}

/// GNU R's `R_CheckUserInterrupt`: raises a time limit that has passed and delivers a pending
/// user interrupt now, unless interrupts are suspended. Native code and the builtins that check
/// as they work call it.
pub fn check_user_interrupt() -> R<()> {
    if INTERRUPTS_SUSPENDED.load(Ordering::Relaxed) != 0 {
        return Ok(());
    }
    if INTERRUPT_REQUESTED.load(Ordering::Relaxed) {
        take_requests()?;
    }
    crate::time_limits::check()?;
    if INTERRUPTS_PENDING.load(Ordering::Relaxed) != 0 {
        return onintr();
    }
    Ok(())
}

/// The check of a builtin that blocks while it waits, such as `Sys.sleep()` or
/// `socketSelect()`. GNU R waits in `R_SelectEx`, which lifts the suspension while it waits, so
/// such a wait delivers a pending user interrupt even inside `suspendInterrupts()`.
pub fn check_interrupt_while_waiting() -> R<()> {
    if INTERRUPT_REQUESTED.load(Ordering::Relaxed) {
        take_requests()?;
    }
    if INTERRUPTS_PENDING.load(Ordering::Relaxed) != 0 {
        return Err(deliver_interrupt());
    }
    Ok(())
}

/// GNU R's `onintr`: signals the pending user interrupt now, or, while interrupts are
/// suspended, leaves it pending for the first check after the suspension ends.
pub fn onintr() -> R<()> {
    if INTERRUPTS_SUSPENDED.load(Ordering::Relaxed) != 0 {
        request_interrupt();
        return Ok(());
    }
    Err(deliver_interrupt())
}

fn deliver_interrupt() -> Unwind {
    INTERRUPTS_PENDING.store(0, Ordering::Relaxed);
    interp().interrupt_countdown = INTERRUPT_CHECK_INTERVAL;
    // SAFETY: interrupts are delivered at calls, loop back edges and the checks of builtins and
    // native code, where R code may run.
    unsafe { crate::conditions::signal_interrupt() }
}

/// Clears [`INTERRUPT_REQUESTED`], answers an editor's request and raises a parked unwind. A
/// pending user interrupt or a passed time limit keeps the flag raised until it is delivered.
fn take_requests() -> R<()> {
    INTERRUPT_REQUESTED.store(
        INTERRUPTS_PENDING.load(Ordering::Relaxed) != 0
            || crate::time_limits::DUE.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    if crate::console_server::SERVICE_REQUESTED.swap(false, Ordering::Relaxed) {
        crate::console_server::service();
    }
    match interp().parked_unwind.take() {
        Some(unwind) => Err(unwind),
        None => Ok(()),
    }
}

/// Raises an unwind [`park`] set aside, leaving anything else that raised
/// [`INTERRUPT_REQUESTED`] for the next poll.
#[inline]
pub fn raise_parked() -> R<()> {
    if INTERRUPT_REQUESTED.load(Ordering::Relaxed)
        && let Some(unwind) = interp().parked_unwind.take()
    {
        return Err(unwind);
    }
    Ok(())
}

/// Sets aside an unwind raised where no `Result` can carry it, such as inside the heap's
/// infallible element accessor, so the evaluator raises it as soon as the builtin that made the
/// access returns ([`raise_parked`]), or else at its next poll: the next call or loop back
/// edge, which compiled code reaches by handing the activation back to tier 0. The first unwind
/// wins, as it would have had it propagated at once.
pub fn park(unwind: Unwind) {
    let interp = interp();
    if interp.parked_unwind.is_none() {
        interp.parked_unwind = Some(unwind);
    }
    INTERRUPT_REQUESTED.store(true, Ordering::Relaxed);
}

/// # Safety
/// Only the collector calls this, while it marks.
pub(crate) unsafe fn trace_unwind(unwind: &Unwind, marker: &mut Marker<'_>) {
    // SAFETY: the caller is marking, and every object an unwind holds is live until raised.
    unsafe {
        match unwind {
            Unwind::Error(error) => marker.mark(error.call),
            Unwind::Interrupt => {}
            Unwind::Jump(jump) => match *jump {
                Jump::Return { env, value } => {
                    marker.mark(env);
                    marker.mark(value);
                }
                Jump::Break { env } | Jump::Next { env } => marker.mark(env),
                Jump::TopLevel => {}
            },
        }
    }
}

#[derive(Debug)]
pub enum Unwind {
    Error(RError),
    Interrupt,
    Jump(Jump),
}

// A jump is unboxed because `return`, `break` and `next` raise one on every use, and it fits in
// the space an error already takes.
const _: () = assert!(std::mem::size_of::<Unwind>() <= 40);

#[derive(Debug)]
pub struct RError {
    pub message: String,
    /// The call the error is reported against. `R_NilValue` means none, and `R_UnboundValue`
    /// means the innermost closure call, which is what GNU R's `error()` reports.
    pub call: Sexp,
    /// The class of the condition the error is signalled as.
    pub class: ErrorClass,
}

/// The condition class an error is signalled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// `simpleError`.
    Simple,
    /// GNU R's `missingArgError`, under the subclass that names what read the argument.
    MissingArgument(ArgumentReader),
    /// GNU R's `expressionStackOverflowError`, for calls nested past `options(expressions=)`.
    ExpressionStackOverflow,
}

/// What read a missing argument. GNU R names it in the first class of the condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgumentReader {
    /// The AST evaluator, evaluating the argument's name.
    Eval,
    /// Compiled code, loading the variable.
    Getvar,
    /// A function lookup or `get`, which found the argument under the name.
    Get,
    /// `Exec`, without its expression.
    Tailcall,
    /// `Tailcall`, without its function.
    TailcallRec,
    /// A two-argument math function such as `round`.
    Math,
    /// `log` with one argument.
    Log1,
    /// `log` with two arguments.
    Log2,
}

/// The class attribute of the condition an error of `class` is signalled as.
pub fn condition_classes(class: ErrorClass) -> SmallVec<[&'static str; 4]> {
    let mut classes = SmallVec::new();
    match class {
        ErrorClass::Simple => classes.push("simpleError"),
        ErrorClass::MissingArgument(reader) => {
            classes.push(match reader {
                ArgumentReader::Eval => "evalError",
                ArgumentReader::Getvar => "getvarError",
                ArgumentReader::Get => "getMissingError",
                ArgumentReader::Tailcall => "tailcallError",
                ArgumentReader::TailcallRec => "tailcallRecError",
                ArgumentReader::Math => "MathMissingError",
                ArgumentReader::Log1 => "log1Error",
                ArgumentReader::Log2 => "log2Error",
            });
            classes.push("missingArgError");
        }
        ErrorClass::ExpressionStackOverflow => {
            classes.extend(["expressionStackOverflowError", "stackOverflowError"]);
        }
    }
    classes.extend(["error", "condition"]);
    classes
}

#[derive(Clone, Copy, Debug)]
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
        class: ErrorClass::Simple,
    })
}

/// An error reported against `call` (`R_NilValue` for none).
pub fn error_in(call: Sexp, message: impl Into<String>) -> Unwind {
    Unwind::Error(RError {
        message: message.into(),
        call,
        class: ErrorClass::Simple,
    })
}

/// The error for calls nested past `options(expressions=)`, which GNU R signals without a call.
pub fn expression_stack_overflow() -> Unwind {
    Unwind::Error(RError {
        message: "evaluation nested too deeply: infinite recursion / options(expressions=)?"
            .to_owned(),
        call: crate::values::nil(),
        class: ErrorClass::ExpressionStackOverflow,
    })
}

/// The error for reading the argument `name` (empty for an argument without one) that the call
/// did not supply and that has no default, reported against `call` as [`error_in`] reports.
/// GNU R before 4.5 raises a plain error against the innermost closure call.
pub fn missing_argument(call: Sexp, name: &str, reader: ArgumentReader) -> Unwind {
    let classed = r_at_least(4, 5);
    Unwind::Error(RError {
        message: if name.is_empty() {
            "argument is missing, with no default".to_string()
        } else {
            format!("argument \"{name}\" is missing, with no default")
        },
        call: if classed {
            call
        } else {
            crate::heap().known().unbound
        },
        class: if classed {
            ErrorClass::MissingArgument(reader)
        } else {
            ErrorClass::Simple
        },
    })
}

/// An error reported against `call` itself, as GNU R's `errorcall` reports it. Inside a complex
/// assignment, `error_in` with the generated replacement call names the whole assignment
/// (`resolve_call`), while this names the generated `` `[<-`(`*tmp*`, ...) `` call, which GNU R's
/// replacement primitives use for a few messages such as an out-of-bounds matrix subscript.
pub unsafe fn errorcall(call: Sexp, message: impl Into<String>) -> Unwind {
    match generated_call(call) {
        Ok(reported) => error_in(reported, message),
        Err(failure) => failure,
    }
}

/// `call` in a form `resolve_call` leaves alone, when it is the replacement call of the complex
/// assignment in progress, which `resolve_call` would otherwise report as the whole assignment:
/// a copy of the generated call, or that call built from the target standing for it.
pub unsafe fn generated_call(call: Sexp) -> R<Sexp> {
    match interp().assignment_call {
        Some((replacement, whole, _)) if replacement == call => {
            if whole.kind() == SexpType::Language && whole.cdr().car() == call {
                crate::eval::generated_replacement_call(call, whole)
            } else {
                crate::values::lcons(call.car(), call.cdr())
            }
        }
        _ => Ok(call),
    }
}

pub fn jump(jump: Jump) -> Unwind {
    Unwind::Jump(jump)
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

impl<T: Rooted> Rooted for Option<T> {
    fn root(&self) {
        if let Some(value) = self {
            value.root();
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
            Unwind::Jump(jump) => match *jump {
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
        matches!(self, Unwind::Jump(jump) if matches!(*jump, Jump::Return { env: target, .. } if target == env))
    }
}

/// The call a condition raised against `call` names: `call` itself, the innermost closure
/// call for an error raised without one, or the whole assignment when `call` is the generated
/// replacement call of a complex assignment in progress. Inside a function GNU R's compiled
/// code names `x[i] <<- value` with the plain arrow, and compiled code is what a closure runs
/// from its second call on (`.agents/memory/decisions/interp.md`, "A condition names a
/// superassignment inside a function with the plain arrow").
pub unsafe fn resolve_call(call: Sexp) -> Sexp {
    let state = interp();
    let innermost = call == crate::heap().known().unbound;
    let whole = match state.assignment_call {
        Some((_, whole, depth)) if innermost && depth == state.frames.len() => whole,
        Some((replacement, whole, _)) if !innermost && replacement == call => whole,
        _ if innermost => {
            return state
                .frames
                .last()
                .map_or(crate::heap().nil(), |frame| frame.call.raw());
        }
        _ => return call,
    };
    let superassignment = whole.kind() == SexpType::Language
        && whole.car().kind() == SexpType::Symbol
        && whole.car().car().bytes() == b"<<-";
    if !superassignment || state.frames.is_empty() {
        return whole;
    }
    let plain =
        crate::values::install("<-").and_then(|arrow| crate::values::lcons(arrow, whole.cdr()));
    match plain {
        Ok(call) => {
            crate::heap().root(call);
            call
        }
        // Out of memory while reporting: the condition names the assignment as written.
        Err(_) => whole,
    }
}

/// The call of the innermost closure frame, or `NULL` at top level.
pub unsafe fn current_call() -> Sexp {
    resolve_call(crate::heap().known().unbound)
}
