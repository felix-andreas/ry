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

use {
    crate::{Global, Runtime, runtime},
    heap::{
        activity::{self, Activity, Counter},
        arena::{Arena, Watermark},
        object::{Sexp, SexpType},
    },
    rustc_hash::FxHashMap,
    interp::unwind::INTERRUPTS_SUSPENDED,
    std::{
        any::Any,
        ffi::{CStr, c_char, c_int, c_void},
        io::Write,
        panic::{AssertUnwindSafe, catch_unwind},
        sync::atomic::Ordering,
    },
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
        global: c_int,
        status: *mut c_int,
        load_error: *mut u32,
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

/// What an API function does with its result before handing it to native code. A handle stays
/// rooted while it is among the native call's most recent handouts (`heap::HANDOUT_WINDOW`),
/// which is the handle scope that makes ry forgive a forgotten `PROTECT` on a temporary.
pub trait Handout: Copy {
    fn hand_out(self, _runtime: &mut Runtime) {}

    /// The object the result is, if it is one, so it can be kept alive while warnings are
    /// signalled.
    fn as_sexp(self) -> Option<Sexp> {
        None
    }
}

impl Handout for Sexp {
    fn as_sexp(self) -> Option<Sexp> {
        Some(self)
    }

    fn hand_out(self, runtime: &mut Runtime) {
        if runtime.native_depth == 0 {
            return;
        }
        if runtime.api_depth == 0 {
            runtime.heap.hand_out(self, runtime.handout_base);
        } else {
            runtime.heap.root(self);
        }
    }
}

/// A result that stays reachable through the API function's argument, such as a list cell's
/// `CAR` or an attribute, so it needs no handle: it lives exactly as long as GNU R's would.
#[derive(Clone, Copy)]
pub struct Reachable(pub Sexp);

impl Handout for Reachable {
    fn as_sexp(self) -> Option<Sexp> {
        Some(self.0)
    }
}

/// A result ry builds as a view of something GNU R stores in the argument, such as `FRAME`'s
/// pairlist. Code that is correct under GNU R walks it unprotected for as long as the argument
/// lives, so it stays rooted until the native call returns instead of leaving with the window.
#[derive(Clone, Copy)]
pub struct Pinned(pub Sexp);

impl Handout for Pinned {
    fn as_sexp(self) -> Option<Sexp> {
        Some(self.0)
    }

    fn hand_out(self, runtime: &mut Runtime) {
        if runtime.native_depth > 0 {
            runtime.heap.root(self.0);
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
    let mark = {
        let runtime = runtime();
        runtime.api_depth += 1;
        runtime.heap.handle_mark()
    };
    let result = catch_unwind(AssertUnwindSafe(body));
    let runtime = runtime();
    runtime.api_depth -= 1;
    runtime.heap.release_handles(mark);
    match result {
        Ok(Ok(value)) => {
            if let Err(unwind) = signal_api_warnings(value.as_sexp()) {
                raise(unwind);
            }
            let runtime = self::runtime();
            value.hand_out(runtime);
            value
        }
        Ok(Err(unwind)) => {
            // A warning raised before the error is signalled first, as in GNU R.
            match signal_api_warnings(None) {
                Err(handled) => raise(handled),
                Ok(()) => raise(unwind),
            }
        }
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

/// The state a native call saves on entry and restores when it returns: its protection region,
/// its handle scope, its handouts and its `R_alloc` watermark.
struct NativeScope {
    handle_mark: usize,
    saved_handout_base: usize,
    saved_api_depth: u32,
    saved_protect_base: usize,
    watermark: Watermark,
}

fn open_native_scope(runtime: &mut Runtime) -> NativeScope {
    let scope = NativeScope {
        handle_mark: runtime.heap.handle_mark(),
        saved_handout_base: runtime.handout_base,
        saved_api_depth: runtime.api_depth,
        saved_protect_base: runtime.heap.roots.protect.enter(),
        watermark: runtime.arena.watermark(),
    };
    runtime.native_depth += 1;
    runtime.handout_base = runtime.heap.handout_mark();
    runtime.api_depth = 0;
    scope
}

/// Ends the scope `open_native_scope` began, and returns the protection depth the native code
/// left behind.
unsafe fn close_native_scope(runtime: &mut Runtime, scope: NativeScope) -> usize {
    runtime.native_depth -= 1;
    if runtime.native_depth == 0 {
        runtime.heap.retire_escaped();
    }
    runtime.heap.release_handouts(runtime.handout_base);
    runtime.handout_base = scope.saved_handout_base;
    runtime.api_depth = scope.saved_api_depth;
    let exit_depth = runtime.heap.roots.protect.leave(scope.saved_protect_base);
    runtime.arena.reset(scope.watermark);
    runtime.heap.release_handles(scope.handle_mark);
    exit_depth
}

/// Runs a `.Call` or `.External` routine inside a C context and its own handle scope.
pub unsafe fn call_native(function: DlFunc, args: &[Sexp], what: &str) -> ApiResult<Sexp> {
    let _activity = native_activity(function);
    let (scope, entry_depth) = {
        let runtime = runtime();
        let entry_depth = runtime.heap.roots.protect.depth();
        let scope = open_native_scope(runtime);
        for &arg in args {
            runtime.heap.root(arg);
        }
        (scope, entry_depth)
    };
    let mut raw_result = std::ptr::null_mut();
    let status = ry_call_native(
        function,
        args.len() as c_int,
        args.as_ptr(),
        &mut raw_result,
    );
    let runtime = runtime();
    let exit_depth = close_native_scope(runtime, scope);
    if status != 0 {
        return Err(take_pending());
    }
    // Rooted before the warnings, which allocate and may run handlers, because closing the
    // scope dropped whatever protected the result.
    let result = Sexp::from_raw(raw_result);
    if let Some(result) = result {
        runtime.heap.root(result);
    }
    if exit_depth != entry_depth {
        warn(format!(
            "stack imbalance in '{what}', {entry_depth} then {exit_depth}"
        ));
    }
    // GNU R accepts a routine that returns C `NULL` and reads it as `R_NilValue`.
    let result = match result {
        Some(result) => result,
        None => {
            warn("converting NULL pointer to R NULL");
            runtime.nil()
        }
    };
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
    let _activity = native_activity(function);
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

/// Publishes native code as the current activity for the profile summary, labelled with the
/// package whose shared library holds `function`.
unsafe fn native_activity(function: DlFunc) -> activity::Scope {
    if !activity::enabled() {
        return activity::enter(Activity::Native, 0);
    }
    activity::count(Counter::NativeCalls);
    static LIBRARIES: std::sync::Mutex<Option<FxHashMap<usize, u32>>> = std::sync::Mutex::new(None);
    let address = function as usize;
    let mut libraries = match LIBRARIES.lock() {
        Ok(libraries) => libraries,
        Err(poisoned) => poisoned.into_inner(),
    };
    let label = *libraries
        .get_or_insert_with(FxHashMap::default)
        .entry(address)
        .or_insert_with(|| activity::label(&library_package(function)));
    activity::enter(Activity::Native, label)
}

/// The package a native routine belongs to, read from its shared library's file name
/// (`.../data.table/libs/data.table.so` is `data.table`).
unsafe fn library_package(function: DlFunc) -> String {
    let mut info: libc::Dl_info = std::mem::zeroed();
    if libc::dladdr(function as *const c_void, &mut info) == 0 || info.dli_fname.is_null() {
        return "unknown".to_owned();
    }
    let path = CStr::from_ptr(info.dli_fname).to_string_lossy();
    let file = path.rsplit('/').next().unwrap_or(&path);
    let stem = file.split(".so").next().unwrap_or(file);
    stem.to_owned()
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
    let scope = open_native_scope(runtime());
    let status = call();
    close_native_scope(runtime(), scope);
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
    root_finalizable(&ready);
    for weak in ready {
        run_finalizer(weak);
    }
    let state = runtime();
    state.heap.release_handles(mark);
    state.running_finalizers = false;
}

/// Roots what the finalizers of `weaks` need until they have run. Rooting a weak reference
/// alone is not enough: the collector never traces a weak reference's fields, and once taken
/// off the heap's lists nothing else keeps them alive while an earlier finalizer allocates.
pub unsafe fn root_finalizable(weaks: &[Sexp]) {
    let heap = &mut runtime().heap;
    for &weak in weaks {
        heap.root(weak);
        heap.root(weak.weak_key());
        heap.root(weak.weak_value());
        heap.root(weak.weak_finalizer());
    }
}

pub unsafe fn run_finalizer(weak: Sexp) {
    let key = weak.weak_key();
    let c_finalizer = weak.weak_c_finalizer();
    let finalizer = weak.weak_finalizer();
    // The key is dead by definition, so only these roots keep it and the finalizer alive while
    // the finalizer runs. `root_finalizable` has rooted them for a queued finalizer already, but
    // a caller that runs one weak reference directly has not.
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
        report(&unwind, "Error");
    }
}

/// Raises a warning from inside an API function, such as `coerceVector`'s "NAs introduced by
/// coercion". GNU R signals it as a condition there and then, so calling handlers and
/// `tryCatch(warning = )` see it. A handler may leave through a jump, which must not cross the
/// Rust frames of the API function, so the warning waits until the outermost [`enter`] has
/// returned from its body and is signalled from there.
pub fn warn(message: impl Into<String>) {
    if !interp::is_installed() {
        return;
    }
    // SAFETY: the runtime is single-threaded, and nothing holds the list across a call.
    unsafe { (*API_WARNINGS.as_ptr()).push(message.into()) };
}

static API_WARNINGS: Global<Vec<String>> = Global::new(Vec::new());

/// Signals the warnings API functions raised, once no API function is still running. The
/// result of the call stays rooted, because a handler runs R code.
// Every API exit runs this check, so the common case of no queued warning stays inline and the
// signalling work lives in a cold function.
#[inline(always)]
unsafe fn signal_api_warnings(result: Option<Sexp>) -> Result<(), Unwind> {
    if runtime().api_depth != 0 || (*API_WARNINGS.as_ptr()).is_empty() {
        return Ok(());
    }
    signal_queued_warnings(result)
}

#[cold]
#[inline(never)]
unsafe fn signal_queued_warnings(result: Option<Sexp>) -> Result<(), Unwind> {
    let runtime = runtime();
    let messages = std::mem::take(&mut *API_WARNINGS.as_ptr());
    let mark = runtime.heap.handle_mark();
    if let Some(value) = result {
        runtime.heap.root(value);
    }
    let unbound = runtime.heap.known().unbound;
    let mut signalled = Ok(());
    for message in messages {
        signalled = interp::conditions::warning_call(unbound, message);
        if signalled.is_err() {
            break;
        }
    }
    runtime.heap.release_handles(mark);
    signalled
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

/// Writes R output, as `Rprintf` and `REprintf` do: through `sink()`'s diversions once the
/// interpreter runs, so `capture.output()` sees what native code prints.
pub fn write_console(text: &[u8], is_error: bool) {
    if !interp::is_installed() {
        return write_terminal(text, is_error);
    }
    // SAFETY: the interpreter is installed, and output runs on its thread.
    unsafe {
        if is_error {
            interp::connections::write_message(text);
        } else {
            interp::connections::write_output(text);
        }
    }
}

/// Writes to the console itself, past any sink. The callback is taken out of the runtime while
/// it runs, so one that prints through the API again finds the default console instead of
/// aliasing itself.
pub fn write_terminal(text: &[u8], is_error: bool) {
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

// `R_interrupts_pending` and `R_interrupts_suspended` are `interp::unwind`'s
// `INTERRUPTS_PENDING` and `INTERRUPTS_SUSPENDED`, which the interpreter defines under GNU R's
// names because it reads them at every interrupt check.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_CheckUserInterrupt() {
    if let Err(unwind) = interp::unwind::check_user_interrupt() {
        raise(unwind);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_onintr() {
    if let Err(unwind) = interp::unwind::onintr() {
        raise(unwind);
    }
}

// The Rust half of native.c.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_state_save(state: *mut ContextState) {
    let runtime = runtime();
    *state = ContextState {
        protect: runtime.heap.roots.protect.depth(),
        vmax: Arena::encode(runtime.arena.watermark()),
        api_depth: runtime.api_depth as usize,
        handout_base: runtime.handout_base,
        interrupts_suspended: INTERRUPTS_SUSPENDED.load(Ordering::Relaxed),
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_state_restore(state: *const ContextState) {
    let runtime = runtime();
    let state = &*state;
    runtime.heap.roots.protect.restore(state.protect);
    runtime.arena.reset(Arena::decode(state.vmax));
    runtime.api_depth = state.api_depth as u32;
    runtime.handout_base = state.handout_base;
    INTERRUPTS_SUSPENDED.store(state.interrupts_suspended, Ordering::Relaxed);
}

/// What a C context saves when it begins and restores when a jump lands on it. A jump may leave
/// API entries without returning through them, so the entry depth is restored with the rest, and
/// so is the interrupt suspension, which an error raised between `BEGIN_SUSPEND_INTERRUPTS` and
/// `END_SUSPEND_INTERRUPTS` would otherwise leave on, as GNU R's contexts restore it.
#[repr(C)]
pub struct ContextState {
    protect: usize,
    vmax: usize,
    api_depth: usize,
    handout_base: usize,
    interrupts_suspended: c_int,
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
        return;
    }
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
    // `Rprintf` and `REprintf` follow `sink`, as GNU R's do; the host console sits behind it.
    interp::write_console(bytes, is_error != 0);
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
