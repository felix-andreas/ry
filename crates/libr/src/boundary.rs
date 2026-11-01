//! The native boundary. Inside Rust, every non-local exit is an `Err(Unwind)` that `?` carries up
//! the stack. Native code expects `longjmp` instead, so the two meet at C contexts (`native.c`):
//!
//! - **ry calling native code** goes through a C trampoline that sets a jump target. A native
//!   `Rf_error`, or an error raised by any API function the native code calls, jumps back to it,
//!   and the trampoline returns a status that becomes `Err(Unwind)` here.
//! - **Native code calling ry** enters through [`enter`], which runs the API function's body,
//!   catches its `Err` (and any panic), and only then calls `ry_jump`. The frames that jump skips
//!   are `enter`'s own and the exported function's, and neither holds anything with a destructor
//!   at that point, which is the condition under which skipping a Rust frame with `longjmp` is
//!   defined.
//!
//! A jump restores the `PROTECT` stack and the `R_alloc` watermark to where they were when its
//! context began, as GNU R does.

use crate::{Global, Runtime, runtime};
use heap::{
    arena::Arena,
    object::{Sexp, SexpType},
};
use std::{
    any::Any,
    ffi::{CStr, c_char, c_int, c_void},
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
};

pub type DlFunc = unsafe extern "C" fn() -> *mut c_void;
pub type ApiResult<T> = Result<T, Unwind>;

/// A non-local exit in flight. It is the interpreter's type, so an unwind crosses between R code
/// and native code unchanged.
pub use interp::unwind::{RError, Unwind};

pub fn error(message: impl Into<String>) -> Unwind {
    Unwind::Error(RError {
        message: message.into(),
        call: runtime().nil(),
    })
}

pub fn error_in(call: Sexp, message: impl Into<String>) -> Unwind {
    Unwind::Error(RError {
        message: message.into(),
        call,
    })
}

unsafe extern "C" {
    fn ry_jump() -> !;
    fn ry_call_native(
        function: DlFunc,
        count: c_int,
        args: *const Sexp,
        result: *mut *mut heap::object::Header,
    ) -> c_int;
    fn ry_call_finalizer(function: unsafe extern "C" fn(Sexp), object: Sexp) -> c_int;
    pub(crate) fn ry_call_dlopen(
        path: *const std::ffi::c_char,
        flags: c_int,
        status: *mut c_int,
    ) -> *mut c_void;
    /// `info` is a `Dll`, which C sees only as the opaque `DllInfo *`.
    pub(crate) fn ry_call_init(
        function: unsafe extern "C" fn(*mut c_void),
        info: *mut c_void,
    ) -> c_int;
    pub(crate) fn ry_call_void(
        function: unsafe extern "C" fn(*mut c_void),
        data: *mut c_void,
    ) -> c_int;
}

/// What an API function does with its result before handing it to native code. A handle is
/// rooted until the enclosing native call returns, which is the handle scope that makes ry
/// forgive a forgotten `PROTECT` on a temporary.
pub trait Handout: Copy {
    fn hand_out(self, _runtime: &mut Runtime) {}
}

impl Handout for Sexp {
    fn hand_out(self, runtime: &mut Runtime) {
        if runtime.native_depth > 0 {
            runtime.heap.root(self);
        }
    }
}

macro_rules! plain_handout {
    ($($type:ty),*) => { $(impl Handout for $type {})* };
}

plain_handout!(
    (),
    bool,
    i32,
    u32,
    isize,
    usize,
    f64,
    u8,
    heap::object::Rcomplex
);

impl<T> Handout for *mut T {}
impl<T> Handout for *const T {}
impl Handout for Option<DlFunc> {}

/// Runs an API function's body on behalf of native code, and turns its `Err` into the jump that
/// native code expects.
pub unsafe fn enter<T: Handout>(body: impl FnOnce() -> ApiResult<T>) -> T {
    let mark = runtime().heap.handle_mark();
    let result = catch_unwind(AssertUnwindSafe(body));
    let runtime = runtime();
    runtime.heap.release_handles(mark);
    match result {
        Ok(Ok(value)) => {
            value.hand_out(runtime);
            value
        }
        Ok(Err(unwind)) => raise(unwind),
        Err(payload) => {
            let unwind = error(format!("internal error in ry: {}", panic_message(&payload)));
            drop(payload);
            raise(unwind)
        }
    }
}

/// Records `unwind` and jumps to the innermost C context. The caller must hold nothing with a
/// destructor.
pub unsafe fn raise(unwind: Unwind) -> ! {
    set_pending(unwind);
    ry_jump()
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic without a message".to_owned()
    }
}

fn set_pending(unwind: Unwind) {
    let runtime = runtime();
    if let Unwind::Error(error) = &unwind {
        runtime.heap.preserve(error.call);
    }
    if let Some(previous) = runtime.pending.replace(unwind) {
        release_unwind(runtime, &previous);
    }
}

pub fn take_pending() -> Unwind {
    let runtime = runtime();
    let unwind = runtime
        .pending
        .take()
        .unwrap_or_else(|| error("a native call unwound without recording why"));
    release_unwind(runtime, &unwind);
    unwind
}

fn release_unwind(runtime: &mut Runtime, unwind: &Unwind) {
    if let Unwind::Error(error) = unwind {
        runtime.heap.release(error.call);
    }
}

/// Runs a `.Call` or `.External` routine inside a C context and its own handle scope.
pub unsafe fn call_native(function: DlFunc, args: &[Sexp], what: &str) -> ApiResult<Sexp> {
    let (mark, saved_base, watermark, entry_depth) = {
        let runtime = runtime();
        let mark = runtime.heap.handle_mark();
        for &arg in args {
            runtime.heap.root(arg);
        }
        let entry_depth = runtime.heap.roots.protect.depth();
        let saved_base = runtime.heap.roots.protect.enter();
        runtime.native_depth += 1;
        (mark, saved_base, runtime.arena.watermark(), entry_depth)
    };
    let mut raw_result = std::ptr::null_mut();
    let status = ry_call_native(
        function,
        args.len() as c_int,
        args.as_ptr(),
        &mut raw_result,
    );
    let runtime = runtime();
    runtime.native_depth -= 1;
    let exit_depth = runtime.heap.roots.protect.leave(saved_base);
    runtime.arena.reset(watermark);
    runtime.heap.release_handles(mark);
    if status != 0 {
        return Err(take_pending());
    }
    if exit_depth != entry_depth {
        warn(format!(
            "stack imbalance in '{what}', {entry_depth} then {exit_depth}"
        ));
    }
    // GNU R accepts a routine that returns C `NULL` and reads it as `R_NilValue`.
    let result = match Sexp::from_raw(raw_result) {
        Some(result) => result,
        None => {
            warn("converting NULL pointer to R NULL");
            runtime.nil()
        }
    };
    runtime.heap.root(result);
    run_pending_finalizers();
    Ok(result)
}

/// Runs a `.C` or `.Fortran` routine, whose arguments are data pointers rather than handles,
/// inside a C context. Every argument is a pointer, so the routine is called through the same
/// dispatcher as `.Call`, and its return value is ignored.
pub unsafe fn call_native_pointers(
    function: DlFunc,
    pointers: &[*mut c_void],
    what: &str,
) -> ApiResult<()> {
    let mut ignored = std::ptr::null_mut();
    let entry_depth = runtime().heap.roots.protect.depth();
    let result = with_native_scope(|| {
        ry_call_native(
            function,
            pointers.len() as c_int,
            pointers.as_ptr() as *const Sexp,
            &mut ignored,
        )
    });
    let exit_depth = runtime().heap.roots.protect.depth();
    if result.is_ok() && exit_depth != entry_depth {
        warn(format!(
            "stack imbalance in '{what}', {entry_depth} then {exit_depth}"
        ));
    }
    result?;
    run_pending_finalizers();
    Ok(())
}

/// Runs `function(data)` inside a C context, for callbacks that return nothing.
pub unsafe fn call_void(
    function: unsafe extern "C" fn(*mut c_void),
    data: *mut c_void,
) -> ApiResult<()> {
    with_native_scope(|| ry_call_void(function, data))
}

/// The bookkeeping every native call shares: a protection region, a handle scope and an
/// `R_alloc` watermark, all restored when it returns.
pub unsafe fn with_native_scope(call: impl FnOnce() -> c_int) -> ApiResult<()> {
    let (mark, saved_base, watermark) = {
        let runtime = runtime();
        let saved_base = runtime.heap.roots.protect.enter();
        runtime.native_depth += 1;
        (
            runtime.heap.handle_mark(),
            saved_base,
            runtime.arena.watermark(),
        )
    };
    let status = call();
    let runtime = runtime();
    runtime.native_depth -= 1;
    runtime.heap.roots.protect.leave(saved_base);
    runtime.arena.reset(watermark);
    runtime.heap.release_handles(mark);
    if status != 0 {
        Err(take_pending())
    } else {
        Ok(())
    }
}

/// Runs the finalizers whose objects died, each as if at top level: an error is reported and
/// stops at the finalizer. GNU R runs them after a collection; ry runs them when a native call
/// returns or on request, because a collection happens deep inside an allocation, where no
/// native code may run.
pub unsafe fn run_pending_finalizers() {
    let state = runtime();
    if state.running_finalizers {
        return;
    }
    let ready = state.heap.take_ready_finalizers();
    if ready.is_empty() {
        return;
    }
    state.running_finalizers = true;
    let mark = state.heap.handle_mark();
    for &weak in &ready {
        state.heap.root(weak);
    }
    for weak in ready {
        run_finalizer(weak);
    }
    let state = runtime();
    state.heap.release_handles(mark);
    state.running_finalizers = false;
}

pub unsafe fn run_finalizer(weak: Sexp) {
    let key = weak.weak_key();
    let c_finalizer = weak.weak_c_finalizer();
    let finalizer = weak.weak_finalizer();
    // The key is dead by definition, so only these roots keep it and the finalizer alive while
    // the finalizer runs.
    let mark = runtime().heap.handle_mark();
    runtime().heap.root(key);
    runtime().heap.root(finalizer);
    runtime().heap.clear_weak_ref(weak);
    let result = if c_finalizer != 0 {
        // SAFETY: the address came from an `R_CFinalizer_t` at registration.
        let function: unsafe extern "C" fn(Sexp) = std::mem::transmute(c_finalizer);
        with_native_scope(|| ry_call_finalizer(function, key))
    } else if finalizer.kind() != SexpType::Nil {
        crate::eval::apply_closure_or_primitive(
            finalizer,
            &[key],
            runtime().heap.known().global_env,
        )
        .map(|_| ())
    } else {
        Ok(())
    };
    runtime().heap.release_handles(mark);
    if let Err(unwind) = result {
        report(&unwind, "Error in finalizer");
    }
}

pub fn warn(message: impl Into<String>) {
    runtime().warnings.push(message.into());
}

/// Prints an unwind the way R's top level does and records it for `R_curErrorBuf`.
pub fn report(unwind: &Unwind, prefix: &str) {
    let text = match unwind {
        Unwind::Error(error) => {
            let call = unsafe { interp::unwind::resolve_call(error.call) };
            format_error(
                prefix,
                &RError {
                    message: error.message.clone(),
                    call,
                },
            )
        }
        Unwind::Interrupt => String::new(),
        // A jump to top level was reported by the default error handler when it was signalled.
        Unwind::Jump(_) => return,
    };
    let runtime = runtime();
    runtime.error_buffer = text.clone().into_bytes();
    write_console(text.as_bytes(), true);
}

/// `Error in <call> : <message>`, moving the message to its own line when the two would not fit
/// on one, as R does.
pub fn format_error(prefix: &str, error: &RError) -> String {
    // SAFETY: the call is rooted for as long as the error is pending or reported.
    let call = unsafe { crate::eval::deparse_call(error.call) };
    match call {
        None => format!("{prefix}: {}\n", error.message),
        Some(call) => {
            let first_line = error.message.lines().next().unwrap_or("").len();
            if prefix.len() + call.len() + first_line + 7 > 77 {
                format!("{prefix} in {call} : \n  {}\n", error.message)
            } else {
                format!("{prefix} in {call} : {}\n", error.message)
            }
        }
    }
}

/// Writes to the console. The callback is taken out of the runtime while it runs, so one that
/// prints through the API again finds the default console instead of aliasing itself.
pub fn write_console(text: &[u8], is_error: bool) {
    let mut console = std::mem::replace(&mut runtime().console, Box::new(default_console));
    console(text, is_error);
    runtime().console = console;
}

pub fn default_console(text: &[u8], is_error: bool) {
    // Console output that cannot be written has nowhere else to go, as in GNU R.
    let written = if is_error {
        std::io::stderr().write_all(text)
    } else {
        let mut stdout = std::io::stdout();
        stdout.write_all(text).and_then(|()| stdout.flush())
    };
    drop(written);
}

/// Set by a signal handler or an embedding host; `R_CheckUserInterrupt` acts on it.
#[unsafe(no_mangle)]
pub static R_interrupts_pending: Global<c_int> = Global::new(0);
#[unsafe(no_mangle)]
pub static R_interrupts_suspended: Global<c_int> = Global::new(0);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_CheckUserInterrupt() {
    if R_interrupts_suspended.get() != 0 {
        return;
    }
    if R_interrupts_pending.get() != 0 {
        R_interrupts_pending.set(0);
        raise(Unwind::Interrupt);
    }
    if let Err(unwind) = interp::unwind::poll_interrupt() {
        raise(unwind);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_onintr() {
    if R_interrupts_suspended.get() == 0 {
        R_interrupts_pending.set(0);
        raise(Unwind::Interrupt);
    }
}

// The Rust half of native.c.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_state_save(protect: *mut usize, vmax: *mut usize) {
    let runtime = runtime();
    *protect = runtime.heap.roots.protect.depth();
    *vmax = Arena::encode(runtime.arena.watermark());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_state_restore(protect: usize, vmax: usize) {
    let runtime = runtime();
    runtime.heap.roots.protect.restore(protect);
    runtime.arena.reset(Arena::decode(vmax));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_record_error(call: Sexp, message: *const c_char) {
    let message = CStr::from_ptr(message).to_string_lossy().into_owned();
    // `Rf_error` passes no call; GNU R reports the innermost closure call for it.
    let call = if call == runtime().nil() && interp::is_installed() {
        runtime().heap.known().unbound
    } else {
        call
    };
    set_pending(error_in(call, message));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_record_warning(call: Sexp, message: *const c_char, immediate: c_int) {
    let message = CStr::from_ptr(message).to_string_lossy().into_owned();
    if immediate != 0 {
        let text = match crate::eval::deparse_call(call) {
            Some(call) => format!("Warning in {call} : {message}\n"),
            None => format!("Warning: {message}\n"),
        };
        write_console(text.as_bytes(), true);
        warn(message);
        return;
    }
    runtime().warnings.push(message.clone());
    if !interp::is_installed() {
        return;
    }
    // A handler may leave through a jump (`tryCatch(warning = )`), which continues from the
    // native call's context, as GNU R's warning does.
    let call = if call == runtime().nil() {
        runtime().heap.known().unbound
    } else {
        call
    };
    let result = interp::conditions::warning_call(call, message);
    if let Err(unwind) = result {
        raise(unwind);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_write_console(text: *const c_char, length: usize, is_error: c_int) {
    let bytes = std::slice::from_raw_parts(text as *const u8, length);
    write_console(bytes, is_error != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_unwind_save(cont: Sexp) {
    let unwind = take_pending();
    let runtime = runtime();
    if let Unwind::Error(error) = &unwind {
        runtime.heap.preserve(error.call);
    }
    runtime.heap.preserve(cont);
    runtime.parked.insert(cont, unwind);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_unwind_restore(cont: Sexp) {
    let runtime = runtime();
    let unwind = match runtime.parked.remove(&cont) {
        Some(unwind) => {
            runtime.heap.release(cont);
            release_unwind(runtime, &unwind);
            unwind
        }
        None => error("R_ContinueUnwind was called with a token that holds no unwind"),
    };
    set_pending(unwind);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_toplevel_report() {
    let unwind = take_pending();
    report(&unwind, "Error");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_discard_pending() {
    drop(take_pending());
}

/// Starts a C context in the interpreter's handler stack. With `classes`, the context is
/// `R_tryCatch`'s: an exiting handler for those classes whose target is the returned token.
/// Without, it is `R_ToplevelExec`'s, which hides every handler outside it. Either way the token
/// restores the stacks in [`ry_handlers_leave`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_handlers_enter(classes: Sexp) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let token = runtime
            .heap
            .alloc_environment(runtime.heap.known().empty_env)?;
        runtime.heap.root(token);
        let state = interp::interp();
        let saved = (
            state.handlers.clone(),
            state.restarts.clone(),
            state.show_error_messages,
        );
        if classes.kind() == SexpType::String {
            let mut stack = state.handlers.clone();
            for index in (0..classes.length() as usize).rev() {
                stack = Some(std::rc::Rc::new(interp::conditions::Handler {
                    class: classes.element(index),
                    handler: runtime.nil(),
                    calling: false,
                    parent_env: runtime.nil(),
                    target: token,
                    next: stack,
                }));
            }
            state.handlers = stack;
        } else {
            // `R_ToplevelExec` reports the errors it stops, even inside a silent `R_tryEval`.
            state.handlers = None;
            state.restarts = None;
            state.show_error_messages = true;
        }
        runtime.heap.preserve(token);
        HANDLER_CONTEXTS.with_borrow_mut(|contexts| contexts.push((token, saved)));
        Ok(token)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_handlers_leave(token: Sexp) {
    let saved = HANDLER_CONTEXTS.with_borrow_mut(|contexts| {
        let position = contexts
            .iter()
            .rposition(|(candidate, _)| *candidate == token)?;
        let entries = contexts.split_off(position);
        entries.into_iter().next().map(|(_, saved)| saved)
    });
    if let Some((handlers, restarts, show)) = saved {
        let state = interp::interp();
        state.handlers = handlers;
        state.restarts = restarts;
        state.show_error_messages = show;
        runtime().heap.release(token);
    }
}

type SavedStacks = (
    interp::conditions::HandlerStack,
    interp::conditions::RestartStack,
    bool,
);

thread_local! {
    static HANDLER_CONTEXTS: std::cell::RefCell<Vec<(Sexp, SavedStacks)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The pending unwind as an R condition object, if `R_tryCatch` (whose handler token is
/// `target`) handles it; null otherwise. The unwind stays pending.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_pending_condition(classes: Sexp, target: Sexp) -> *mut c_void {
    let runtime = runtime();
    let Some(unwind) = &runtime.pending else {
        return std::ptr::null_mut();
    };
    let condition = match unwind {
        // The interpreter chose this context's exiting handler.
        Unwind::Jump(jump) => match **jump {
            interp::Jump::Return { env, value } if env == target => {
                let condition = if value.kind() == SexpType::List && value.length() >= 1 {
                    value.element(0)
                } else {
                    runtime.nil()
                };
                if condition != runtime.nil() {
                    Ok(condition)
                } else {
                    let call = if value.kind() == SexpType::List && value.length() >= 2 {
                        value.element(1)
                    } else {
                        runtime.nil()
                    };
                    let message = interp::interp().error_message.clone();
                    interp::conditions::make_condition(
                        &message,
                        call,
                        &["simpleError", "error", "condition"],
                    )
                }
            }
            _ => return std::ptr::null_mut(),
        },
        // Native code raised it directly, so no handler saw it yet.
        Unwind::Error(_) | Unwind::Interrupt => {
            let class = if matches!(unwind, Unwind::Error(_)) {
                "error"
            } else {
                "interrupt"
            };
            let wanted = classes.kind() == SexpType::String
                && (0..classes.length() as usize).any(|index| {
                    let element = classes.element(index);
                    element.bytes() == class.as_bytes() || element.bytes() == b"condition"
                });
            if !wanted {
                return std::ptr::null_mut();
            }
            crate::eval::make_condition(unwind)
        }
    };
    match condition {
        Ok(condition) => condition.as_ptr().cast(),
        // Building the condition failed, so that failure replaces the unwind and propagates.
        Err(failure) => {
            set_pending(failure);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_fatal_without_context() -> ! {
    let unwind = take_pending();
    report(&unwind, "Error");
    let message = b"ry: an R error was raised outside of any native call or context, so there is nowhere to return to\n";
    write_console(message, true);
    std::process::abort()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_FlushConsole() {
    drop(std::io::stdout().flush());
}
