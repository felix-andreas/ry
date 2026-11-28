//! Conditions and restarts: the handler stack behind `withCallingHandlers` and `tryCatch`, the
//! restart stack behind `withRestarts`, signalling, and the default error and warning handlers.
//!
//! The stacks are persistent lists, as GNU R's are R pairlists: a calling handler runs with the
//! stack below its own entry, and every closure frame restores the stack it started with.

use {
    crate::{
        Interp, Signalling,
        base_fast_paths::fast_paths_enabled,
        builtins::{BuiltinDef, BuiltinFn, Call, internal},
        deparse,
        handles::{
            self, Args, Cells, Chars, Closure, Cx, Gc, Language, List, Local, Owned, Pairlist,
            Promise, Str, StringFill, Symbol, base_namespace, call_of, call_value, closure_source,
            context_calls, deep_copy, define_base_variable, dense, deparse_first_line,
            deparse_lines, evaluate, evaluate_all, failure_call, first_string, force,
            frame_binding, get_attribute, global_environment, handler_entries, handling_error,
            install_bytes, integer_scalar, interned_symbol, is_na, is_unbound, item, length,
            live_frame, logical_constant, logical_scalar, named_list, null, option,
            permanent_symbol, real_scalar, resolved_call, restart_entries, safe, set_attribute,
            symbol, take_warnings, text, text_vector, unbound, uncaught_error,
        },
        interp,
        unwind::{
            ErrorClass, INTERRUPTS_SUSPENDED, Jump, R, RError, Unwind, condition_classes, error,
            jump,
        },
        values, write_console,
    },
    heap::{
        Marker,
        activity::{self, Counter},
        object::{NA_INTEGER, Sexp, SexpType},
    },
    smallvec::SmallVec,
    std::{borrow::Cow, cell::Cell, convert::Infallible, rc::Rc, sync::atomic::Ordering},
};

pub type HandlerStack = Option<Rc<Handler>>;

/// Whether `stack` holds a calling handler anywhere.
#[inline]
pub fn calling_established(stack: &HandlerStack) -> bool {
    stack.as_ref().is_some_and(|handler| handler.calling_below)
}
pub type RestartStack = Option<Rc<RestartEntry>>;

pub struct Handler {
    /// The condition class this entry handles.
    pub class: Sexp,
    pub handler: Sexp,
    pub calling: bool,
    /// Whether this entry or any below it is a calling handler, so asking whether one is
    /// established reads the top entry instead of walking the stack on every call.
    pub calling_below: bool,
    pub parent_env: Sexp,
    /// For an exiting handler, the frame environment that receives the result.
    pub target: Sexp,
    pub next: HandlerStack,
}

pub struct RestartEntry {
    pub restart: Sexp,
    pub next: RestartStack,
}

/// The most released entries [`restore_handlers`] keeps for reuse.
const HANDLER_POOL_SIZE: usize = 64;

/// A stack entry for `handler`, reusing one a frame exit released when there is one.
pub fn new_handler(handler: Handler) -> Rc<Handler> {
    let state = interp();
    if let Some(mut entry) = state.handler_pool.pop()
        && let Some(slot) = Rc::get_mut(&mut entry)
    {
        *slot = handler;
        return entry;
    }
    Rc::new(handler)
}

/// Makes `saved` the handler stack again, as a frame exit does, and keeps the entries above it
/// that nothing else holds for [`new_handler`] to reuse.
#[inline(always)]
pub fn restore_handlers(saved: HandlerStack) {
    let state = interp();
    // Most calls establish no handler, so the stack to restore is the one in place, and every
    // closure call returns through here.
    if ptr_eq(&state.handlers, &saved) {
        return;
    }
    replace_handlers(state, saved);
}

#[inline(never)]
fn replace_handlers(state: &mut Interp, saved: HandlerStack) {
    let mut released = std::mem::replace(&mut state.handlers, saved);
    while let Some(mut entry) = released
        && state.handler_pool.len() < HANDLER_POOL_SIZE
    {
        // An entry still on the restored stack, or saved by a frame or a caller, is shared.
        let Some(handler) = Rc::get_mut(&mut entry) else {
            break;
        };
        released = handler.next.take();
        state.handler_pool.push(entry);
    }
}

fn ptr_eq(left: &HandlerStack, right: &HandlerStack) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

/// Whether an entry of `stack` has `closure` as its handler.
pub(crate) fn handlers_hold(stack: &HandlerStack, closure: Sexp) -> bool {
    let mut entry = stack.as_ref();
    while let Some(handler) = entry {
        if handler.handler == closure {
            return true;
        }
        entry = handler.next.as_ref();
    }
    false
}

pub(crate) unsafe fn trace_handlers(stack: &HandlerStack, marker: &mut Marker<'_>) {
    let mut entry = stack.as_ref();
    while let Some(handler) = entry {
        marker.mark(handler.class);
        marker.mark(handler.handler);
        marker.mark(handler.parent_env);
        marker.mark(handler.target);
        entry = handler.next.as_ref();
    }
}

pub(crate) unsafe fn trace_restarts(stack: &RestartStack, marker: &mut Marker<'_>) {
    let mut entry = stack.as_ref();
    while let Some(restart) = entry {
        marker.mark(restart.restart);
        entry = restart.next.as_ref();
    }
}

/// Keeps `handlers` and `restarts` traced while other stacks are in effect, until the guard is
/// dropped. Dropping truncates the set-aside list to its length before the call, so a guard
/// dropped by an error return also releases any stack set aside after it and not yet released.
pub fn set_aside(handlers: HandlerStack, restarts: RestartStack) -> SetAside {
    let state = interp();
    let depth = state.suspended_handlers.len();
    state.suspended_handlers.push((handlers, restarts));
    SetAside { depth }
}

/// The guard [`set_aside`] returns.
#[must_use = "the stacks are traced only while the guard lives"]
pub struct SetAside {
    depth: usize,
}

impl Drop for SetAside {
    fn drop(&mut self) {
        interp().suspended_handlers.truncate(self.depth);
    }
}

/// The classes of a simple error, as bytes to match handler classes and as names to build one.
const SIMPLE_ERROR_CLASSES: [&[u8]; 3] = [b"simpleError", b"error", b"condition"];
const SIMPLE_ERROR_NAMES: [&str; 3] = ["simpleError", "error", "condition"];

/// The classes of a simple warning, as bytes to match handler classes and as names to build one.
const WARNING_CLASSES: [&[u8]; 3] = [b"simpleWarning", b"warning", b"condition"];
const WARNING_CLASS_NAMES: [&str; 3] = ["simpleWarning", "warning", "condition"];

/// The classes of a user interrupt.
const INTERRUPT_CLASSES: [&[u8]; 2] = [b"interrupt", b"condition"];
const INTERRUPT_NAMES: [&str; 2] = ["interrupt", "condition"];

/// Signals an error raised by a builtin, native code or the evaluator: runs the calling
/// handlers for the error's classes, jumps to an exiting one, and otherwise runs the default
/// error handler. The result is the unwind to propagate.
pub fn signal_failure(context: &mut Cx<'_>, failure: RError) -> Unwind {
    if let Some(unwind) = waits_for_frame() {
        return unwind;
    }
    let call = failure_call(context, &failure);
    // GNU R formats a message into a C buffer, so it ends at the first NUL.
    let mut message = failure.message;
    if let Some(nul) = message.find('\0') {
        message.truncate(nul);
    }
    if failure.class != ErrorClass::Simple {
        let classes = condition_classes(failure.class);
        return classed_error(context, call, &message, &classes, &[]);
    }
    activity::count(Counter::ConditionsSignalled);
    let call = resolved_call(context, call);
    let saved = interp().handlers.clone();
    let _saved = set_aside(saved.clone(), None);
    while let Some(found) = find_handler(context, |class| SIMPLE_ERROR_CLASSES.contains(&class)) {
        interp().handlers = found.entry.next.clone();
        // GNU R copies the message into the `geterrmessage()` buffer for each handler it
        // finds, calling or exiting, so a handled `stop("...")` is what it reports next.
        interp().error_message = message.clone();
        if found.entry.calling {
            if let Err(unwind) = call_simple_error_handler(context, found.function, &message, call)
            {
                return unwind;
            }
        } else {
            return match exiting_result(context, null(), call, found.function) {
                Ok(result) => jump(Jump::Return {
                    env: found.target.raw(),
                    value: result.raw(),
                }),
                Err(unwind) => unwind,
            };
        }
    }
    interp().handlers = saved;
    default_error(context, call, &message)
}

/// Signals an error condition of `classes` that carries `fields` after its message and call,
/// as GNU R's `R_signalErrorCondition` does: each handler takes the condition object, and the
/// default error handler runs when none unwinds. GNU R raises a few errors this way, such as
/// `MissingSubscriptError`, so that handlers can tell them apart. The unbound marker as `call`
/// names the running call. The result is the unwind to propagate.
pub fn classed_error<'s>(
    context: &mut Cx<'s>,
    call: Local<'_>,
    message: &str,
    classes: &[&str],
    fields: &[(&str, Local<'_>)],
) -> Unwind {
    let call = resolved_call(context, call);
    let fields: SmallVec<[(&str, Local<'s>); 4]> = fields
        .iter()
        .map(|&(name, value)| (name, context.root(value)))
        .collect();
    let signalled = condition_object(context, message, call, classes, &fields)
        .and_then(|condition| signal(context, condition, call));
    match signalled {
        Ok(_) => default_error(context, call, message),
        Err(unwind) => unwind,
    }
}

/// GNU R's default error handler: prints the message, then goes to top level.
pub fn default_error<'s>(context: &mut Cx<'s>, call: Local<'s>, message: &str) -> Unwind {
    if let Err(unwind) = handling_error(context) {
        return unwind;
    }
    let no_call = call.is(null());
    // GNU R formats the message into a buffer of `warning.length` bytes after the
    // `Error in ` (or `Error: `) head, so a longer one loses its tail.
    let limit = (warning_length(context) + 1).saturating_sub(if no_call { 8 } else { 10 });
    let message = truncate_at_char(message, limit);
    let mut text = error_text(context, call, message);
    interp().error_message = text.clone();
    if interp().in_error > 0 {
        write_console(text.as_bytes(), true);
        return jump(Jump::TopLevel);
    }
    interp().in_error += 1;
    let error_option = option(context, "error");
    let show = option(context, "show.error.messages");
    let show =
        interp().show_error_messages && (show.is(null()) || logical_scalar(context, show) != 0);
    if show {
        write_console(text.as_bytes(), true);
        let calls_option = option(context, "showErrorCalls");
        // GNU R prints no traceback for an error without a call, such as a variable missing in
        // top-level code that a callee forced.
        let traceback_wanted = logical_scalar(context, calls_option) == 1 && !no_call;
        if (traceback_wanted || std::env::var_os("RY_TRACEBACK").is_some())
            && let Some(traceback) = concise_traceback(context, call)
        {
            let line = format!("Calls: {traceback}\n");
            write_console(line.as_bytes(), true);
            // `geterrmessage()` shows the traceback too, as GNU R appends it to the buffer.
            text.push_str(&line);
            interp().error_message = text.clone();
        }
        if !interp().warnings.is_empty() {
            write_console(b"In addition: ", true);
            if let Err(unwind) = print_deferred(context, "") {
                interp().in_error -= 1;
                return unwind;
            }
        }
    }
    let mut outcome = jump(Jump::TopLevel);
    if interp().debugger.is_some()
        && let Err(unwind) = uncaught_error(context, &text)
    {
        unwind.root();
        outcome = unwind;
    }
    if !error_option.is(null())
        && let Err(unwind) = run_error_option(context, error_option)
    {
        unwind.root();
        outcome = unwind;
    }
    // GNU R records the stack for `traceback()` only after the `error` option ran, so a
    // handler there still sees the previous error's.
    if let Err(unwind) = record_traceback(context) {
        unwind.root();
        outcome = unwind;
    }
    interp().in_error -= 1;
    // Going to top level means invoking the innermost `abort` restart, so code that
    // establishes its own (`withRestarts(expr, abort = ...)`) regains control after the error.
    if matches!(&outcome, Unwind::Jump(target) if matches!(*target, Jump::TopLevel)) {
        match user_abort_restart(context) {
            Ok(Some(target)) => return target,
            Ok(None) => {}
            Err(unwind) => return unwind,
        }
    }
    outcome
}

/// `getOption("warning.length")`, GNU R's limit on the bytes of a warning or error message.
pub fn warning_length(context: &mut Cx<'_>) -> usize {
    let value = option(context, "warning.length");
    match integer_scalar(context, value) {
        length if length > 0 && length != NA_INTEGER => length as usize,
        _ => 1000,
    }
}

/// An error raised as GNU R's C `error()` raises it: the message is formatted into a buffer of
/// `warning.length` bytes, so a longer one loses its tail, and a character the cut splits is
/// dropped.
pub fn c_error(message: &[u8]) -> Unwind {
    // SAFETY: every builtin runs on the interpreter's thread, and the scope only reads an
    // option, which roots nothing past the scope.
    let limit = unsafe { handles::scoped(warning_length) }.min(8192) - 1;
    let mut message = &message[..message.len().min(limit)];
    if message.len() == limit {
        let mut last = message.len().saturating_sub(1);
        while last > 0 && message[last] & 0xc0 == 0x80 {
            last -= 1;
        }
        if let Err(failure) = std::str::from_utf8(&message[last..]) {
            message = &message[..last + failure.valid_up_to()];
        }
    }
    error(values::escape_invalid_bytes(message).into_owned())
}

/// `text` cut to at most `limit` bytes, on a character boundary.
pub fn truncate_at_char(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

thread_local! {
    static WARNINGS: Cell<u64> = const { Cell::new(0) };
}

/// How many warnings native code has raised through [`warn_in`], which tells typed code
/// whether an R call it treats as free of effects did something a second run would repeat.
pub fn warnings_raised() -> u64 {
    WARNINGS.get()
}

/// Signals a warning against `call` through `.signalSimpleWarning(message, base::quote(call))`,
/// whose `.dfltWarn` defers or prints it per `options(warn)`. GNU R takes this route even with no
/// warning handler established, and the frames it pushes are observable: rlang recognises a
/// warning promoted to an error by finding `.signalSimpleWarning` on the stack. The unbound
/// marker as `call` names the running call.
pub fn warn_in(context: &mut Cx<'_>, call: Local<'_>, message: impl Into<String>) -> R<()> {
    WARNINGS.set(WARNINGS.get() + 1);
    let call = resolved_call(context, call);
    let mut message = message.into();
    if let Some(nul) = message.find('\0') {
        message.truncate(nul);
    }
    if fast_paths_enabled() {
        match find_handler(context, |class| WARNING_CLASSES.contains(&class)) {
            // Without a handler for the warning, `.signalSimpleWarning` only runs the default
            // handler. Its frames are then observable only when `options(warn = 2)` turns the
            // warning into an error, whose handlers and traceback see them.
            None => {
                if warn_level(context) < 2 {
                    return default_warning(context, call, &message);
                }
            }
            // An exiting handler runs after the unwind to its `tryCatch`, so the frames and the
            // restart `.signalSimpleWarning` would set up are gone before it sees the condition,
            // which is `simpleWarning(message, call)`.
            Some(found) if !found.entry.calling => {
                let condition =
                    condition_object(context, &message, call, &WARNING_CLASS_NAMES, &[])?;
                interp().handlers = found.entry.next.clone();
                let result = exiting_result(context, condition, call, found.function)?;
                return Err(jump(Jump::Return {
                    env: found.target.raw(),
                    value: result.raw(),
                }));
            }
            Some(found) => {
                if muffled_by_suppress_warnings(context, found.function)? {
                    return Ok(());
                }
            }
        }
    }
    // The hook is R code, which a call without a frame cannot run.
    if let Some(unwind) = waits_for_frame() {
        return Err(unwind);
    }
    let hook = install_bytes(context, b".signalSimpleWarning")?;
    if frame_binding(context, base_namespace(), hook).is_some() {
        let quoted_call = base_quote(context, call)?;
        let text = text_vector(context, &[message.as_str()])?;
        let visible = interp().visible;
        // GNU R calls the hook from the base environment, which no frame owns, so `sys.parents()`
        // gives the hook's frame its own number.
        call_value(
            context,
            interned_symbol(hook).value(),
            &[text.value(), quoted_call],
        )?;
        interp().visible = visible;
        return Ok(());
    }
    default_warning(context, call, &message)
}

/// A warning attributed to the innermost closure call.
pub fn warn(context: &mut Cx<'_>, message: impl Into<String>) -> R<()> {
    warn_in(context, unbound(), message)
}

/// GNU R's default warning handler.
pub fn default_warning(context: &mut Cx<'_>, call: Local<'_>, message: &str) -> R<()> {
    activity::count(Counter::ConditionsSignalled);
    let limit = warning_length(context);
    let truncated;
    let message = if message.len() >= limit {
        truncated = format!("{} [... truncated]", truncate_at_char(message, limit));
        truncated.as_str()
    } else {
        message
    };
    let level = warn_level(context);
    if level < 0 {
        return Ok(());
    }
    if level >= 2 {
        return Err(Unwind::Error(RError {
            message: format!("(converted from warning) {message}"),
            call: call.raw(),
            class: ErrorClass::Simple,
        }));
    }
    if level == 1 || interp().immediate_warning {
        let text = immediate_warning_text(context, call, message);
        write_console(text.as_bytes(), true);
        return Ok(());
    }
    let limit = warning_limit(context);
    let state = interp();
    if state.warnings.len() < limit {
        // The deferred warnings are traced, so the call lives as long as its entry.
        state.warnings.push((call.raw(), message.to_owned()));
    }
    Ok(())
}

/// The heading of the warnings a session's start left.
pub const DURING_STARTUP: &str = "During startup - ";

/// Prints the deferred warnings the way the top level does after a call, and clears them.
/// `heading` goes in front, as GNU R puts `During startup - ` ([`DURING_STARTUP`]) in front of
/// the warnings a session's start left.
pub fn print_deferred(context: &mut Cx<'_>, heading: &str) -> R<()> {
    let warnings = take_warnings(context);
    if warnings.is_empty() {
        return Ok(());
    }
    let mut text = heading.to_owned();
    if let [(call, message)] = warnings.as_slice() {
        text.push_str("Warning message:\n");
        if call.is(null()) {
            // GNU R ends a warning without a call with a space, alone or numbered.
            text.push_str(message);
            text.push(' ');
        } else {
            let call_text = deparse_first_line(context, *call);
            let separator = if call_text.len() + message.len() + 6 > 75 {
                " :\n  "
            } else {
                " : "
            };
            text.push_str(&format!("In {call_text}{separator}{message}"));
        }
        text.push('\n');
    } else {
        let limit = warning_limit(context);
        match warnings.len() {
            ..=10 => text.push_str("Warning messages:\n"),
            count if count < limit => text.push_str(&format!(
                "There were {count} warnings (use warnings() to see them)\n"
            )),
            _ => text.push_str(&format!(
                "There were {limit} or more warnings (use warnings() to see the first {limit})\n"
            )),
        }
        if warnings.len() <= 10 {
            for (index, (call, message)) in warnings.iter().enumerate() {
                if call.is(null()) {
                    text.push_str(&format!("{}: {message} \n", index + 1));
                } else {
                    let call_text = deparse_first_line(context, *call);
                    let separator = if call_text.len() + message.len() + 10 > 75 {
                        " :\n  "
                    } else {
                        " : "
                    };
                    text.push_str(&format!(
                        "{}: In {call_text}{separator}{message}\n",
                        index + 1
                    ));
                }
            }
        }
    }
    write_console(text.as_bytes(), true);
    // `warnings()` reads the printed warnings back from `last.warning` in the base environment.
    let items: Vec<(&str, Local<'_>)> = warnings
        .iter()
        .map(|(call, message)| (message.as_str(), *call))
        .collect();
    let last = named_list(context, &items)?.finish();
    let name = install_bytes(context, b"last.warning")?;
    define_base_variable(context, name, last.value())
}

/// Runs the handlers that take `condition`, as `.signalCondition(cond, message, call)`: each
/// calling handler in turn from the top of the stack, until an exiting one unwinds to its
/// `tryCatch`. Returns `NULL` when none unwinds.
pub(crate) fn signal<'s>(
    context: &mut Cx<'s>,
    condition: Local<'s>,
    call: Local<'s>,
) -> R<Local<'s>> {
    if let Some(unwind) = waits_for_frame() {
        return Err(unwind);
    }
    activity::count(Counter::ConditionsSignalled);
    let saved = interp().handlers.clone();
    let _saved = set_aside(saved.clone(), None);
    // A calling handler may change the condition's class, which GNU R reads afresh for each
    // handler it looks for.
    while let Some(found) = class_handler(context, condition)? {
        interp().handlers = found.entry.next.clone();
        if found.entry.calling {
            let handler_call = call_of(context, found.function, &[condition])?;
            evaluate(context, handler_call.value(), global_environment())?;
        } else {
            let result = exiting_result(context, condition, call, found.function)?;
            return Err(jump(Jump::Return {
                env: found.target.raw(),
                value: result.raw(),
            }));
        }
    }
    interp().handlers = saved;
    Ok(null())
}

/// Whether a handler on the stack takes a condition of one of `classes`. When none does,
/// signalling such a condition runs no R code, so nothing can observe the frames GNU R pushes to
/// signal it, and a fast path may skip them and go straight to the default handler.
pub(crate) unsafe fn handled(classes: &[&[u8]]) -> bool {
    handles::reading(|context| {
        handler_entries(context).any(|view| classes.contains(&class_bytes(view.class)))
    })
}

/// [`signal_failure`] for raw code.
pub unsafe fn signal_error(failure: RError) -> Unwind {
    match handles::with(|context| Err::<Infallible, _>(signal_failure(context, failure))) {
        Err(unwind) => unwind,
        Ok(never) => match never {},
    }
}

/// [`warn_in`] for raw code, which keeps `call` alive.
pub unsafe fn warning_call(call: Sexp, message: impl Into<String>) -> R<()> {
    handles::with(|context| {
        let call = handles::adopt(context, call);
        warn_in(context, call, message)
    })
}

/// [`warn`] for raw code.
pub unsafe fn warning(message: impl Into<String>) -> R<()> {
    handles::with(|context| warn(context, message))
}

/// A condition object for raw code: a list of `message` and `call` with `classes`.
pub unsafe fn make_condition(message: &str, call: Sexp, classes: &[&str]) -> R<Sexp> {
    handles::with_result(|context| {
        let call = handles::adopt(context, call);
        Ok(condition_object(context, message, call, classes, &[])?.raw())
    })
}

/// The calls on the stack, innermost first and without the innermost `skip`, each deparsed and
/// carrying the srcref of where it was made: `.Traceback` as GNU R records it.
pub unsafe fn traceback_calls(skip: usize) -> R<Sexp> {
    handles::with_result(|context| Ok(traceback(context, skip)?.raw()))
}

/// [`print_deferred`] for raw code.
pub unsafe fn print_warnings(heading: &str) -> R<()> {
    handles::with(|context| print_deferred(context, heading))
}

/// [`signal`] for the `signalCondition` fast path, which keeps both alive. It runs in the scope
/// of `signalCondition`'s own frame, which releases what this roots, so it opens no scope of its
/// own.
pub(crate) unsafe fn signal_condition(condition: Sexp, condition_call: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let condition = handles::adopt(context, condition);
        let call = handles::adopt(context, condition_call);
        Ok(signal(context, condition, call)?.raw())
    })
}

/// What `.Internal(warning(call., immediate., noBreaks., message))` does, for a builtin `call`
/// made in the frame of `warning()` itself.
pub(crate) unsafe fn signal_warning(
    call: &Call<'_>,
    use_call: bool,
    immediate: bool,
    message: &str,
) -> R<()> {
    let frame = call.frame_index();
    handles::with(|context| issue_warning(context, frame, use_call, immediate, message))
}

/// `Error in <call> : <message>` for raw code, which keeps `call` alive.
pub unsafe fn format_error(call: Sexp, message: &str) -> String {
    handles::scoped(|context| {
        let call = handles::adopt(context, call);
        error_text(context, call, message)
    })
}

/// The text of a condition's message, for printing, which raw code keeps alive.
pub unsafe fn condition_message(condition: Sexp) -> R<String> {
    handles::with(|context| {
        let condition = handles::adopt(context, condition);
        let Some(list) = condition.standard::<List>() else {
            return Ok(String::new());
        };
        let names = context.root(get_attribute(
            context,
            list,
            symbol(|symbols| symbols.names),
        ));
        let position = dense::<Str>(context, names)?.and_then(|names| {
            names
                .iter(context)
                .position(|name| name.bytes() == b"message")
        });
        let Some(position) = position else {
            return Ok(String::new());
        };
        let message = context.root(list.read(context).get(position));
        Ok(match dense::<Str>(context, message)? {
            Some(text) if !text.is_empty() => handles::text(text.get(context, 0)).into_owned(),
            _ => String::new(),
        })
    })
}

/// Signals a user interrupt as GNU R's `onintr` does: an `interrupt` condition with no message
/// or call goes to the calling handlers and to an exiting one, and only when none takes it does
/// the interrupt unwind to top level.
pub unsafe fn signal_interrupt() -> Unwind {
    // The unwind leaves the scope as an error so that the values a jump carries are rooted.
    match handles::with(|context| {
        Err::<Infallible, _>(match interrupt(context) {
            Ok(unwind) | Err(unwind) => unwind,
        })
    }) {
        Err(unwind) => unwind,
        Ok(never) => match never {},
    }
}

// The internals.

pub const BUILTINS: &[BuiltinDef] = &[
    internal(".addCondHands", 5, ADD_HANDLERS).invisible(),
    internal(".resetCondHands", 1, safe!(do_nothing)).invisible(),
    internal(".signalCondition", 3, safe!(do_signal_condition)),
    internal(".dfltStop", 2, safe!(do_default_stop)),
    internal(".dfltWarn", 2, safe!(do_default_warn)).invisible(),
    internal("stop", 2, safe!(do_stop)),
    internal("warning", 4, safe!(do_warning)).invisible(),
    internal("geterrmessage", 0, safe!(do_geterrmessage)),
    internal("seterrmessage", 1, safe!(do_seterrmessage)).invisible(),
    internal(
        "printDeferredWarnings",
        0,
        safe!(do_print_deferred_warnings),
    )
    .invisible(),
    internal(".addRestart", 1, ADD_RESTART).invisible(),
    internal(".getRestart", 1, safe!(do_get_restart)),
    internal(".invokeRestart", 2, INVOKE_RESTART),
    internal(".addTryHandlers", 0, safe!(do_nothing)).invisible(),
    internal(".addGlobHands", 5, safe!(do_add_global_handlers)).invisible(),
    internal("interruptsSuspended", -1, safe!(do_interrupts_suspended)),
];

/// `.addCondHands`, `.addRestart` and `.invokeRestart`, which the `tryCatch` and
/// `withRestarts` prologues also run as kernels.
pub(crate) const ADD_HANDLERS: BuiltinFn = safe!(do_add_cond_hands);
pub(crate) const ADD_RESTART: BuiltinFn = safe!(do_add_restart);
pub(crate) const INVOKE_RESTART: BuiltinFn = safe!(do_invoke_restart);

fn do_add_cond_hands<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    add_handlers(context, arguments)?;
    Ok(null())
}

/// `.addGlobHands(classes, handlers, parentenv, target, calling)`: replaces the global handlers
/// at the bottom of the stack. Every frame between here and top level saved the old bottom, so
/// each is rewritten to the new one, or returning from it would drop the new handlers.
fn do_add_global_handlers<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let state = interp();
    let global = &state.global_handlers;
    if !ptr_eq(&state.handlers, global)
        || !state
            .frames
            .iter()
            .all(|frame| ptr_eq(&frame.handlers, global))
    {
        return Err(error("should not be called with handlers on the stack"));
    }
    state.handlers = None;
    add_handlers(context, arguments)?;
    let state = interp();
    for frame in &mut state.frames {
        frame.handlers = state.handlers.clone();
    }
    state.global_handlers = state.handlers.clone();
    Ok(null())
}

fn do_nothing<'s>(_context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    Ok(null())
}

fn do_signal_condition<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    signal(context, arguments.value(0), arguments.value(2))
}

fn do_default_stop<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let message = message_text(context, arguments.value(0))?;
    Err(default_error(context, arguments.value(1), &message))
}

fn do_default_warn<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let message = message_text(context, arguments.value(0))?;
    default_warning(context, arguments.value(1), &message)?;
    Ok(null())
}

fn do_stop<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let use_call = logical_scalar(context, arguments.value(0)) != 0;
    let call = if use_call {
        caller_call(context, arguments.frame_index())
    } else {
        null()
    };
    let message = first_string(context, arguments.value(1))
        .map(Cow::into_owned)
        .unwrap_or_default();
    Err(signal_failure(
        context,
        RError {
            message,
            call: call.raw(),
            class: ErrorClass::Simple,
        },
    ))
}

fn do_warning<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let use_call = logical_scalar(context, arguments.value(0)) != 0;
    let immediate = logical_scalar(context, arguments.value(1)) == 1;
    let message = first_string(context, arguments.value(3))
        .map(Cow::into_owned)
        .unwrap_or_default();
    issue_warning(
        context,
        arguments.frame_index(),
        use_call,
        immediate,
        &message,
    )?;
    Ok(text_vector(context, &[message.as_str()])?.value())
}

fn do_geterrmessage<'s>(context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let message = interp().error_message.clone();
    Ok(text_vector(context, &[message.as_str()])?.value())
}

fn do_seterrmessage<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let message = message_text(context, arguments.value(0))?;
    // GNU R keeps the message in an 8192-byte C buffer, so a longer one loses its tail.
    interp().error_message = truncate_at_char(&message, 8191).to_owned();
    Ok(null())
}

fn do_print_deferred_warnings<'s>(context: &mut Cx<'s>, _arguments: &Args<'_, 's>) -> R<Local<'s>> {
    if !interp().warnings.is_empty() {
        write_console(b"In addition: ", true);
        print_deferred(context, "")?;
    }
    Ok(null())
}

fn do_add_restart<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let restart = arguments.value(0);
    if restart.r_type() != SexpType::List || length(context, restart) < 2 {
        return Err(error("bad restart"));
    }
    let state = interp();
    state.restarts = Some(Rc::new(RestartEntry {
        restart: restart.raw(),
        next: state.restarts.clone(),
    }));
    Ok(null())
}

fn do_get_restart<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let index = integer_scalar(context, arguments.value(0));
    let mut position = 1;
    for (_, restart) in restart_entries(context) {
        if position == index {
            return Ok(context.root(restart));
        }
        position += 1;
    }
    // Below every established restart sits top level's `abort`, whose exit is `NULL`.
    if position != index {
        return Ok(null());
    }
    let name = text_vector(context, &["abort"])?;
    let mut abort = context.alloc::<List>(2)?;
    abort.set(context, 0, name.value());
    let mut abort = abort.owned();
    let class = text_vector(context, &["restart"])?;
    set_attribute(
        context,
        &mut abort,
        symbol(|symbols| symbols.class),
        class.value(),
    )?;
    Ok(abort.finish().value())
}

fn do_invoke_restart<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let restart = arguments.value(0);
    let values = arguments.value(1);
    if restart.r_type() != SexpType::List || length(context, restart) < 2 {
        return Err(error("bad restart"));
    }
    let exit = item(context, restart, 1)?;
    if exit.is(null()) {
        interp().restarts = None;
        return Err(jump(Jump::TopLevel));
    }
    let next = restart_entries(context).find_map(|(entry, candidate)| {
        let candidate = candidate.standard::<List>()?;
        (candidate.len() >= 2 && candidate.get(1).is(exit)).then(|| entry.next.clone())
    });
    let Some(next) = next else {
        return Err(error("restart not on stack"));
    };
    interp().restarts = next;
    Err(jump(Jump::Return {
        env: exit.raw(),
        value: values.raw(),
    }))
}

/// `.Internal(interruptsSuspended(value))`: returns whether interrupts are suspended and, given
/// `value`, sets it. Lifting the suspension delivers nothing by itself: a pending interrupt waits
/// for the next check, as in GNU R.
fn do_interrupts_suspended<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let previous = INTERRUPTS_SUSPENDED.load(Ordering::Relaxed);
    if !arguments.is_empty() {
        let suspended = logical_scalar(context, arguments.value(0));
        INTERRUPTS_SUSPENDED.store(suspended, Ordering::Relaxed);
    }
    logical_constant(context, previous)
}

/// A handler entry found on the stack, with its function and target rooted.
struct Found<'s> {
    entry: Rc<Handler>,
    function: Local<'s>,
    target: Local<'s>,
}

/// The topmost handler whose class satisfies `takes`.
fn find_handler<'s>(context: &Cx<'s>, takes: impl Fn(&[u8]) -> bool) -> Option<Found<'s>> {
    let view = handler_entries(context).find(|view| takes(class_bytes(view.class)))?;
    Some(Found {
        entry: view.entry.clone(),
        function: context.root(view.function),
        target: context.root(view.target),
    })
}

/// The topmost handler for a class of `condition`'s class attribute.
fn class_handler<'s>(context: &mut Cx<'s>, condition: Local<'s>) -> R<Option<Found<'s>>> {
    let class = get_attribute(context, condition, symbol(|symbols| symbols.class));
    if let Some(class) = class.standard::<Str>() {
        let names = class.elements();
        return Ok(find_handler(context, |wanted| {
            names.iter().any(|name| name.bytes() == wanted)
        }));
    }
    if class.r_type() != SexpType::String {
        return Ok(None);
    }
    let class = context.root(class);
    let Some(class) = dense::<Str>(context, class)? else {
        return Ok(None);
    };
    let reader: &Cx<'s> = context;
    let names = class.read(reader).elements();
    Ok(find_handler(reader, |wanted| {
        names.iter().any(|name| name.bytes() == wanted)
    }))
}

/// The text of a handler's class, empty for an entry without one.
fn class_bytes(class: Gc<'_>) -> &[u8] {
    class.downcast::<Chars>().map_or(&[], |class| class.bytes())
}

fn call_simple_error_handler<'s>(
    context: &mut Cx<'s>,
    handler: Local<'s>,
    message: &str,
    call: Local<'s>,
) -> R<()> {
    let hook = install_bytes(context, b".handleSimpleError")?;
    let quoted_call = base_quote(context, call)?;
    let text = text_vector(context, &[message])?;
    if frame_binding(context, base_namespace(), hook).is_some() {
        call_value(
            context,
            interned_symbol(hook).value(),
            &[handler, text.value(), quoted_call],
        )?;
    } else {
        let condition = condition_object(context, message, call, &SIMPLE_ERROR_NAMES, &[])?;
        let handler_call = call_of(context, handler, &[condition])?;
        evaluate(context, handler_call.value(), global_environment())?;
    }
    Ok(())
}

/// The list an exiting handler's `tryCatch` receives.
fn exiting_result<'s>(
    context: &mut Cx<'s>,
    condition: Local<'_>,
    call: Local<'_>,
    handler: Local<'_>,
) -> R<Local<'s>> {
    let mut result = context.alloc::<List>(3)?;
    result.set(context, 0, condition);
    result.set(context, 1, call);
    result.set(context, 2, handler);
    Ok(result.finish().value())
}

/// A condition object: a list of `message`, `call` and `fields` with `classes`.
fn condition_object<'s>(
    context: &mut Cx<'s>,
    message: &str,
    call: Local<'s>,
    classes: &[&str],
    fields: &[(&str, Local<'s>)],
) -> R<Local<'s>> {
    let text = text_vector(context, &[message])?;
    let mut items: SmallVec<[(&str, Local<'s>); 4]> =
        SmallVec::from_slice(&[("message", text.value()), ("call", call)]);
    items.extend_from_slice(fields);
    let mut condition = named_list(context, &items)?;
    let class = text_vector(context, classes)?;
    set_attribute(
        context,
        &mut condition,
        symbol(|symbols| symbols.class),
        class.value(),
    )?;
    Ok(condition.finish().value())
}

/// `options(error =)` run as GNU R runs it after the message: a call or a symbol evaluated, an
/// expression vector evaluated in order, a function called with no arguments.
fn run_error_option<'s>(context: &mut Cx<'s>, option: Local<'s>) -> R<()> {
    match option.r_type() {
        SexpType::Language | SexpType::Symbol => {
            evaluate(context, option, global_environment()).map(drop)
        }
        SexpType::Expression => evaluate_all(context, option, global_environment()),
        kind if values::is_function_type(kind) => {
            let call = call_of(context, option, &[])?;
            evaluate(context, call.value(), global_environment()).map(drop)
        }
        _ => Ok(()),
    }
}

/// Binds `.Traceback` in the base environment to the stack as [`traceback`] lists it.
fn record_traceback(context: &mut Cx<'_>) -> R<()> {
    let calls = traceback(context, 0)?;
    let name = install_bytes(context, b".Traceback")?;
    define_base_variable(context, name, calls)
}

/// The jump that invokes the innermost `abort` restart established in R code, if any.
fn user_abort_restart(context: &mut Cx<'_>) -> R<Option<Unwind>> {
    let found = restart_entries(context).find_map(|(entry, restart)| {
        let restart = restart
            .standard::<List>()
            .filter(|restart| restart.len() >= 2)?;
        let name = restart.get(0).standard::<Str>()?;
        let is_abort = !restart.get(1).is(null()) && name.elements().first()?.bytes() == b"abort";
        is_abort.then(|| (entry.next.clone(), context.root(restart.get(1))))
    });
    let Some((next, exit)) = found else {
        return Ok(None);
    };
    interp().restarts = next;
    let arguments = context.alloc::<List>(0)?.finish();
    Ok(Some(jump(Jump::Return {
        env: exit.raw(),
        value: arguments.raw(),
    })))
}

/// The calls on the stack as `.Traceback` lists them ([`traceback_calls`]).
fn traceback<'s>(context: &mut Cx<'s>, skip: usize) -> R<Local<'s>> {
    let calls: Vec<(Local<'s>, Local<'s>)> = context_calls(context)
        .into_iter()
        .rev()
        .skip(skip)
        .map(|(call, srcref)| (context.root(call), context.root(srcref)))
        .collect();
    let mut cells = Cells::new(SexpType::Pairlist);
    for (call, srcref) in calls {
        let lines = deparse_lines(context, call, 60, deparse::options::SIMPLE);
        let mut text = StringFill::new(context, lines.len())?;
        for line in &lines {
            text.push_text(context, line)?;
        }
        let mut text = text.finish().owned();
        if !srcref.is(null()) {
            let copy = deep_copy(context, srcref)?.finish();
            set_attribute(context, &mut text, symbol(|symbols| symbols.srcref), copy)?;
        }
        cells.push(context, null(), text.finish().value())?;
    }
    Ok(cells.finish().map_or_else(null, Owned::finish))
}

/// GNU R's concise traceback: the functions on the stack, outermost first, or `None` when the
/// only one is the call the message already names.
fn concise_traceback(context: &Cx<'_>, call: Local<'_>) -> Option<String> {
    let calls = context_calls(context);
    let mut names: Vec<String> = calls
        .iter()
        .map(|&(frame_call, _)| {
            frame_call
                .downcast::<Language>()
                .and_then(|frame_call| frame_call.head().downcast::<Symbol>())
                .map_or_else(
                    || "<Anonymous>".to_owned(),
                    |name| text(name.name()).into_owned(),
                )
        })
        .collect();
    // The functions that signal leave themselves, and whatever they called, out.
    let signallers = [
        "stop",
        "warning",
        "suppressWarnings",
        ".signalSimpleWarning",
    ];
    if let Some(position) = names
        .iter()
        .rposition(|name| signallers.contains(&name.as_str()))
    {
        names.truncate(position);
    }
    if names.is_empty() {
        return None;
    }
    if names.len() == 1 && calls.first().is_some_and(|&(first, _)| first.is(call)) {
        return None;
    }
    // Built from the innermost call outwards; once the chain passes 50 bytes, only the
    // outermost function is added, after "...".
    let mut text = String::new();
    let mut outermost = None;
    for name in names.iter().rev() {
        if outermost.is_some() || text.len() > 50 {
            outermost = Some(name);
        } else if text.is_empty() {
            text = name.clone();
        } else {
            text = format!("{name} -> {text}");
        }
    }
    if let Some(outermost) = outermost {
        text = format!("{outermost} ... {text}");
    }
    Some(text)
}

/// `Error in <call> : <message>`, with the message on its own line when the two would not fit
/// on one, as GNU R prints it.
fn error_text(context: &mut Cx<'_>, call: Local<'_>, message: &str) -> String {
    if call.is(null()) {
        return format!("Error: {message}\n");
    }
    let call_text = deparse_first_line(context, call);
    let first_line = message.lines().next().unwrap_or("");
    let width = call_text.chars().count() + first_line.chars().count();
    if 14 + width > 75 {
        format!("Error in {call_text} : \n  {message}\n")
    } else {
        format!("Error in {call_text} : {message}\n")
    }
}

/// Whether `base::suppressWarnings` muffles a simple warning whose topmost matching handler is
/// the calling handler `handler`: it is `function(w) if (inherits(w, classes))
/// tryInvokeRestart("muffleWarning")` as `suppressWarnings` establishes it, and its `classes` name
/// one of the warning's classes. That handler only tests the class and invokes the
/// `muffleWarning` restart, so nothing it would run is observable, and the frames of
/// `.signalSimpleWarning` and `withRestarts` are seen only by handlers that do run. Any other
/// handler, or a `classes` whose value is still an unevaluated expression, takes the R route.
fn muffled_by_suppress_warnings<'s>(context: &mut Cx<'s>, handler: Local<'s>) -> R<bool> {
    let Some(closure) = handler.downcast::<Closure>() else {
        return Ok(false);
    };
    let classes = {
        let function = closure.read(context);
        let Some(body) = closure_source(function) else {
            return Ok(false);
        };
        if stock_muffle(function.formals(), body).is_none() {
            return Ok(false);
        }
        // The handler is recognised by what its free names resolve to, not by where it was
        // created: its environment must be a frame directly inside the base namespace that
        // binds none of `if`, `inherits` and `tryInvokeRestart`, as the frame of
        // `suppressWarnings` does, so the handler behaves exactly as the stock one. A handler
        // built from the same text in any other environment takes the R route. The function
        // literal itself cannot identify the handler, because the start-up image does not keep
        // a closure's body and its compiled code sharing one object.
        let environment = function.environment();
        let free = [
            symbol(|symbols| symbols.if_),
            symbol(|symbols| symbols.inherits),
            symbol(|symbols| symbols.try_invoke_restart),
        ];
        if !environment
            .enclosure()
            .is_some_and(|enclosure| enclosure.is(base_namespace()))
            || free
                .into_iter()
                .any(|name| frame_binding(context, environment, name).is_some())
        {
            return Ok(false);
        }
        let Some(binding) = frame_binding(context, environment, symbol(|symbols| symbols.classes))
        else {
            return Ok(false);
        };
        context.root(binding.content())
    };
    let classes = match classes.downcast::<Promise>() {
        None => classes,
        Some(promise) => {
            let forced = promise.read(context).result();
            if !is_unbound(forced) {
                context.root(forced)
            } else if promise.read(context).code().r_type() == SexpType::String {
                // A literal default such as `"warning"`: forcing it has no effect but its value,
                // as when the handler forces it.
                force(context, classes)?
            } else {
                return Ok(false);
            }
        }
    };
    let Some(classes) = dense::<Str>(context, classes)? else {
        return Ok(false);
    };
    Ok(classes
        .iter(context)
        .any(|class| WARNING_CLASSES.contains(&class.bytes())))
}

/// `Some` when a handler with these formals and body is `function(w) if (inherits(w, classes))
/// tryInvokeRestart("muffleWarning")`, the handler base R's `suppressWarnings` establishes.
fn stock_muffle(formals: Gc<'_>, body: Gc<'_>) -> Option<()> {
    let formals = formals.downcast::<Pairlist>()?;
    (formals.tag().is(symbol(|symbols| symbols.w)) && formals.rest().is(null())).then_some(())?;
    let arguments = untagged_arguments(body, symbol(|symbols| symbols.if_))?;
    let &[test, action] = arguments.as_slice() else {
        return None;
    };
    let tested = untagged_arguments(test, symbol(|symbols| symbols.inherits))?;
    let &[subject, classes] = tested.as_slice() else {
        return None;
    };
    (subject.is(symbol(|symbols| symbols.w)) && classes.is(symbol(|symbols| symbols.classes)))
        .then_some(())?;
    let invoked = untagged_arguments(action, symbol(|symbols| symbols.try_invoke_restart))?;
    let &[restart] = invoked.as_slice() else {
        return None;
    };
    let restart = restart.standard::<Str>()?;
    (restart.len() == 1 && restart.get(0).bytes() == b"muffleWarning").then_some(())
}

/// The arguments of `call` when it is a call of `function` with no argument named.
fn untagged_arguments<'a>(call: Gc<'a>, function: Gc<'_, Symbol>) -> Option<SmallVec<[Gc<'a>; 2]>> {
    let call = call.downcast::<Language>()?;
    call.head().is(function).then_some(())?;
    let mut arguments = SmallVec::new();
    let mut rest = call.rest();
    while let Some(cell) = rest.downcast::<Pairlist>() {
        if !cell.tag().is(null()) {
            return None;
        }
        arguments.push(cell.head());
        rest = cell.rest();
    }
    rest.is(null()).then_some(arguments)
}

/// `base::quote(call)`, the form in which GNU R hands a condition's call to the R-level hooks
/// `.signalSimpleWarning` and `.handleSimpleError`, where `sys.calls()` shows it.
fn base_quote<'s>(context: &mut Cx<'s>, call: Local<'_>) -> R<Local<'s>> {
    let base = install_bytes(context, b"base")?;
    let quote = call_of(
        context,
        permanent_symbol(|symbols| symbols.double_colon).value(),
        &[
            interned_symbol(base).value(),
            permanent_symbol(|symbols| symbols.quote).value(),
        ],
    )?;
    Ok(call_of(context, quote.value(), &[call])?.value())
}

/// `getOption("warn")`, 0 when unset.
fn warn_level(context: &mut Cx<'_>) -> i32 {
    let level = option(context, "warn");
    if level.is(null()) {
        0
    } else {
        integer_scalar(context, level)
    }
}

/// How many deferred warnings are kept: `getOption("nwarnings")`, 50 by default.
fn warning_limit(context: &mut Cx<'_>) -> usize {
    let option = option(context, "nwarnings");
    let value = if option.is(null()) {
        f64::NAN
    } else {
        real_scalar(context, option)
    };
    if value.is_finite() && value >= 1.0 {
        value as usize
    } else {
        50
    }
}

/// A warning printed as it happens: the first line of the call, then the message on the same
/// line when the two fit in 57 characters, and indented on the next line otherwise.
fn immediate_warning_text(context: &mut Cx<'_>, call: Local<'_>, message: &str) -> String {
    if call.is(null()) {
        return format!("Warning: {message}\n");
    }
    let call_text = deparse_first_line(context, call);
    let separator = if call_text.chars().count() + message.chars().count() + 18 > 75 {
        " :\n  "
    } else {
        " : "
    };
    format!("Warning in {call_text}{separator}{message}\n")
}

/// The unwind a signal gives up with while an instruction runs for a closure call that has no
/// frame ([`Signalling`]): a handler is R code and would see a stack without the call, so the
/// instruction is abandoned and runs again, and signals, once the call has its frame.
fn waits_for_frame() -> Option<Unwind> {
    if interp().signalling == Signalling::Immediate {
        return None;
    }
    interp().signalling = Signalling::Abandoned;
    Some(error("a condition waits for its call's frame"))
}

/// The handlers of `.addCondHands(classes, handlers, parentenv, target, calling)` pushed on the
/// stack, the first one on top.
fn add_handlers<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<()> {
    activity::count(Counter::HandlersEstablished);
    let (classes, handlers) = (arguments.value(0), arguments.value(1));
    let calling = logical_scalar(context, arguments.value(4)) != 0;
    if classes.is(null()) || handlers.is(null()) {
        return Ok(());
    }
    let classes = dense::<Str>(context, classes)?;
    let handlers = dense::<List>(context, handlers)?;
    let (Some(classes), Some(handlers)) = (classes, handlers) else {
        return Err(error("bad handler data"));
    };
    if classes.len() != handlers.len() {
        return Err(error("bad handler data"));
    }
    let parent_env = arguments.value(2).raw();
    let target = arguments.value(3).raw();
    let (classes, handlers) = (classes.read(context), handlers.read(context));
    let mut stack = interp().handlers.clone();
    // The stack is traced, so the entries keep what they hold once they are on it.
    for index in (0..classes.len()).rev() {
        stack = Some(new_handler(Handler {
            class: classes.get(index).raw(),
            handler: handlers.get(index).raw(),
            calling,
            calling_below: calling || calling_established(&stack),
            parent_env,
            target,
            next: stack,
        }));
    }
    interp().handlers = stack;
    Ok(())
}

/// Signals a user interrupt ([`signal_interrupt`]). The result is the unwind to propagate.
fn interrupt(context: &mut Cx<'_>) -> R<Unwind> {
    let mut condition = context.alloc::<List>(0)?.owned();
    let classes = text_vector(context, &INTERRUPT_NAMES)?;
    set_attribute(
        context,
        &mut condition,
        symbol(|symbols| symbols.class),
        classes.value(),
    )?;
    let condition = condition.finish().value();
    let saved = interp().handlers.clone();
    let _saved = set_aside(saved.clone(), None);
    while let Some(found) = find_handler(context, |class| INTERRUPT_CLASSES.contains(&class)) {
        interp().handlers = found.entry.next.clone();
        if found.entry.calling {
            let handled = call_of(context, found.function, &[condition])
                .and_then(|call| evaluate(context, call.value(), global_environment()));
            if let Err(unwind) = handled {
                interp().handlers = saved;
                return Err(unwind);
            }
        } else {
            interp().handlers = saved;
            let result = exiting_result(context, condition, null(), found.function)?;
            return Ok(jump(Jump::Return {
                env: found.target.raw(),
                value: result.raw(),
            }));
        }
    }
    interp().handlers = saved;
    Ok(Unwind::Interrupt)
}

/// The call `stop()` and `warning()` report: the caller of the closure in frame `frame`, the one
/// that called them.
fn caller_call<'s>(context: &Cx<'s>, frame: Option<usize>) -> Local<'s> {
    match frame {
        Some(position) if position >= 1 => context.root(live_frame(context, position - 1).call()),
        _ => null(),
    }
}

/// [`warn_in`] for `warning()` called in frame `frame`.
fn issue_warning(
    context: &mut Cx<'_>,
    frame: Option<usize>,
    use_call: bool,
    immediate: bool,
    message: &str,
) -> R<()> {
    let call = if use_call {
        caller_call(context, frame)
    } else {
        null()
    };
    // Handlers still see an immediate warning; only the default handler prints it at once.
    let outer = std::mem::replace(&mut interp().immediate_warning, immediate);
    let signalled = warn_in(context, call, message);
    interp().immediate_warning = outer;
    signalled
}

/// The text of a message argument: its first element, `NA` as "NA".
fn message_text<'s>(context: &mut Cx<'s>, message: Local<'s>) -> R<String> {
    match dense::<Str>(context, message)? {
        Some(message) if !message.is_empty() => {
            let first = message.get(context, 0);
            Ok(if is_na(first) {
                "NA".to_owned()
            } else {
                text(first).into_owned()
            })
        }
        _ => Err(error("error message not a string")),
    }
}
