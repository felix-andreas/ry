//! Conditions and restarts: the handler stack behind `withCallingHandlers` and `tryCatch`, the
//! restart stack behind `withRestarts`, signalling, and the default error and warning handlers.
//!
//! The stacks are persistent lists, as GNU R's are R pairlists: a calling handler runs with the
//! stack below its own entry, and every closure frame restores the stack it started with.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    deparse, eval, interp,
    unwind::{Jump, R, RError, Unwind, error, jump, resolve_call},
    values::{
        self, char_str, global_env, length, list_vector, nil, scalar_string, string_at, symbols,
    },
    write_console,
};
use heap::{
    Marker,
    object::{Sexp, SexpType},
};
use std::rc::Rc;

pub type HandlerStack = Option<Rc<Handler>>;
pub type RestartStack = Option<Rc<RestartEntry>>;

pub struct Handler {
    /// The condition class this entry handles.
    pub class: Sexp,
    pub handler: Sexp,
    pub calling: bool,
    pub parent_env: Sexp,
    /// For an exiting handler, the frame environment that receives the result.
    pub target: Sexp,
    pub next: HandlerStack,
}

pub struct RestartEntry {
    pub restart: Sexp,
    pub next: RestartStack,
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

/// The topmost handler for one of `classes`, and the stack below it.
unsafe fn find_handler(stack: &HandlerStack, classes: &[&[u8]]) -> Option<Rc<Handler>> {
    let mut entry = stack.clone();
    while let Some(handler) = entry {
        let class = handler.class.bytes();
        if classes.contains(&class) {
            return Some(handler);
        }
        entry = handler.next.clone();
    }
    None
}

unsafe fn condition_classes(condition: Sexp) -> Vec<Vec<u8>> {
    let class = values::get_attr(condition, symbols().class);
    if class.kind() != SexpType::String {
        return Vec::new();
    }
    (0..length(class))
        .map(|index| class.element(index).bytes().to_vec())
        .collect()
}

/// Signals an error raised by a builtin, native code or the evaluator: runs the calling
/// handlers for `simpleError`, `error` and `condition`, jumps to an exiting one, and otherwise
/// runs the default error handler. The result is the unwind to propagate.
pub unsafe fn signal_error(failure: RError) -> Unwind {
    let call = resolve_call(failure.call);
    let message = failure.message;
    let state = interp();
    let saved = state.handlers.clone();
    let classes: [&[u8]; 3] = [b"simpleError", b"error", b"condition"];
    while let Some(entry) = find_handler(&interp().handlers, &classes) {
        interp().handlers = entry.next.clone();
        if entry.calling {
            if let Err(unwind) = call_simple_error_handler(entry.handler, &message, call) {
                return unwind;
            }
        } else {
            interp().error_message = message.clone();
            return match exiting_result(nil(), call, entry.handler) {
                Ok(result) => jump(Jump::Return {
                    env: entry.target,
                    value: result,
                }),
                Err(unwind) => unwind,
            };
        }
    }
    interp().handlers = saved;
    default_error(call, &message)
}

unsafe fn call_simple_error_handler(handler: Sexp, message: &str, call: Sexp) -> R<()> {
    let hook = values::install(".handleSimpleError")?;
    let quoted_call = values::call(symbols().quote, &[call])?;
    let text = scalar_string(message)?;
    if crate::heap().frame_cell(values::base_namespace(), hook).is_some() {
        let hook_call = values::call(hook, &[handler, text, quoted_call])?;
        eval::eval(hook_call, global_env())?;
    } else {
        let condition = make_condition(message, call, &["simpleError", "error", "condition"])?;
        let handler_call = values::call(handler, &[condition])?;
        eval::eval(handler_call, global_env())?;
    }
    Ok(())
}

unsafe fn exiting_result(condition: Sexp, call: Sexp, handler: Sexp) -> R<Sexp> {
    list_vector(&[condition, call, handler])
}

/// A condition object: a list of `message` and `call` with `classes`.
pub unsafe fn make_condition(message: &str, call: Sexp, classes: &[&str]) -> R<Sexp> {
    let text = scalar_string(message)?;
    let condition = values::named_list(&[("message", text), ("call", call)])?;
    let class = values::string_vector(classes)?;
    values::set_attr(condition, symbols().class, class)?;
    Ok(condition)
}

/// GNU R's default error handler: prints the message, then goes to top level.
pub unsafe fn default_error(call: Sexp, message: &str) -> Unwind {
    let state = interp();
    let text = format_error(call, message);
    state.error_message = text.clone();
    if state.in_error > 0 {
        write_console(text.as_bytes(), true);
        return jump(Jump::TopLevel);
    }
    state.in_error += 1;
    let option = crate::envir::get_option("error");
    let show = crate::envir::get_option("show.error.messages");
    let show = state.show_error_messages && (show == nil() || values::as_logical_scalar(show) != 0);
    if show {
        write_console(text.as_bytes(), true);
        let calls_option = crate::envir::get_option("showErrorCalls");
        if values::as_logical_scalar(calls_option) == 1 || std::env::var_os("RY_TRACEBACK").is_some() {
            if let Some(traceback) = concise_traceback(call) {
                write_console(format!("Calls: {traceback}\n").as_bytes(), true);
            }
        }
        if !interp().warnings.is_empty() {
            write_console(b"In addition: ", true);
            print_warnings();
        }
    }
    let mut outcome = jump(Jump::TopLevel);
    if option != nil() {
        let result = match option.kind() {
            SexpType::Language | SexpType::Symbol => eval::eval(option, global_env()).map(|_| ()),
            SexpType::Expression => eval::eval_all(option, global_env()).map(|_| ()),
            _ if values::is_function(option) => {
                values::call(option, &[]).and_then(|call| eval::eval(call, global_env())).map(|_| ())
            }
            _ => Ok(()),
        };
        if let Err(unwind) = result {
            outcome = unwind;
        }
    }
    interp().in_error -= 1;
    outcome
}

/// GNU R's concise traceback: the functions on the stack, outermost first, or `None` when the
/// only one is the call the message already names.
unsafe fn concise_traceback(call: Sexp) -> Option<String> {
    let mut names = Vec::new();
    for frame in &interp().frames {
        let head = frame.call.car();
        let name = if frame.call.kind() == SexpType::Language && head.kind() == SexpType::Symbol {
            values::symbol_name(head).into_owned()
        } else {
            "<Anonymous>".to_owned()
        };
        names.push(name);
    }
    // The functions that signal leave themselves, and whatever they called, out.
    let signallers = ["stop", "warning", "suppressWarnings", ".signalSimpleWarning"];
    if let Some(position) = names.iter().rposition(|name| signallers.contains(&name.as_str())) {
        names.truncate(position);
    }
    if names.is_empty() {
        return None;
    }
    if names.len() == 1 && interp().frames.first().is_some_and(|frame| frame.call == call) {
        return None;
    }
    let mut text = names.join(" -> ");
    if text.chars().count() > 300 {
        text = format!("... -> {}", names[names.len().saturating_sub(5)..].join(" -> "));
    }
    Some(text)
}

/// `Error in <call> : <message>`, with the message on its own line when the two would not fit
/// on one, as GNU R prints it.
pub unsafe fn format_error(call: Sexp, message: &str) -> String {
    if call == nil() {
        return format!("Error: {message}\n");
    }
    let call_text = deparse::deparse_one_line(call);
    let first_line = message.lines().next().unwrap_or("");
    let width = call_text.chars().count() + first_line.chars().count();
    if 18 + width > 77 {
        format!("Error in {call_text} : \n  {message}\n")
    } else {
        format!("Error in {call_text} : {message}\n")
    }
}

/// Signals a warning from a builtin: the calling handlers run through `.signalSimpleWarning`,
/// as in GNU R, and without one the warning is deferred or printed per `options(warn)`.
pub unsafe fn warning_call(call: Sexp, message: impl Into<String>) -> R<()> {
    let call = resolve_call(call);
    let message = message.into();
    let classes: [&[u8]; 3] = [b"simpleWarning", b"warning", b"condition"];
    let hook = values::install(".signalSimpleWarning")?;
    if find_handler(&interp().handlers, &classes).is_some()
        && crate::heap().frame_cell(values::base_namespace(), hook).is_some()
    {
        let quoted_call = values::call(symbols().quote, &[call])?;
        let text = scalar_string(&message)?;
        let hook_call = values::call(hook, &[text, quoted_call])?;
        let visible = interp().visible;
        eval::eval(hook_call, global_env())?;
        interp().visible = visible;
        return Ok(());
    }
    default_warning(call, &message)
}

/// A warning attributed to the innermost closure call.
pub unsafe fn warning(message: impl Into<String>) -> R<()> {
    warning_call(crate::heap().known().unbound, message)
}

/// GNU R's default warning handler.
pub unsafe fn default_warning(call: Sexp, message: &str) -> R<()> {
    let level = crate::envir::get_option("warn");
    let level = if level == nil() {
        0
    } else {
        values::as_integer_scalar(level)
    };
    if level < 0 {
        return Ok(());
    }
    if level >= 2 {
        return Err(Unwind::Error(RError {
            message: format!("(converted from warning) {message}"),
            call,
        }));
    }
    if level == 1 {
        let text = if call == nil() {
            format!("Warning: {message}\n")
        } else {
            format!(
                "Warning in {} : {message}\n",
                deparse::deparse_one_line(call)
            )
        };
        write_console(text.as_bytes(), true);
        return Ok(());
    }
    let state = interp();
    if state.warnings.len() < 50 {
        state.warnings.push((call, message.to_owned()));
    }
    Ok(())
}

/// The first line of a call's deparse, which is all of a long call a warning shows.
unsafe fn first_deparse_line(call: Sexp) -> String {
    deparse::deparse_lines(call, 60, deparse::options::SIMPLE).into_iter().next().unwrap_or_default()
}

/// Prints the deferred warnings the way the top level does after a call, and clears them.
pub unsafe fn print_warnings() {
    let warnings = std::mem::take(&mut interp().warnings);
    if warnings.is_empty() {
        return;
    }
    let mut text = String::new();
    if warnings.len() == 1 {
        text.push_str("Warning message:\n");
        let (call, message) = &warnings[0];
        if *call == nil() {
            text.push_str(message);
        } else {
            let call_text = first_deparse_line(*call);
            let separator = if call_text.len() + message.len() + 6 > 75 {
                " :\n  "
            } else {
                " : "
            };
            text.push_str(&format!("In {call_text}{separator}{message}"));
        }
        text.push('\n');
    } else {
        text.push_str(if warnings.len() >= 50 {
            "There were 50 or more warnings (use warnings() to see the first 50)\n"
        } else {
            "Warning messages:\n"
        });
        if warnings.len() < 50 {
            for (index, (call, message)) in warnings.iter().enumerate() {
                if *call == nil() {
                    text.push_str(&format!("{}: {message}\n", index + 1));
                } else {
                    let call_text = first_deparse_line(*call);
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
    let last = values::install("last.warning");
    if let Ok(last) = last {
        let items: Vec<(&str, Sexp)> = Vec::new();
        drop(items);
        drop(last);
    }
}

// The internals.

pub const BUILTINS: &[BuiltinDef] = &[
    internal(".addCondHands", 5, do_add_cond_hands).invisible(),
    internal(".resetCondHands", 1, do_reset_cond_hands).invisible(),
    internal(".signalCondition", 3, do_signal_condition),
    internal(".dfltStop", 2, do_default_stop),
    internal(".dfltWarn", 2, do_default_warn).invisible(),
    internal("stop", 2, do_stop),
    internal("warning", 4, do_warning).invisible(),
    internal("geterrmessage", 0, do_geterrmessage),
    internal("seterrmessage", 1, do_seterrmessage).invisible(),
    internal("printDeferredWarnings", 0, do_print_deferred_warnings).invisible(),
    internal(".addRestart", 1, do_add_restart).invisible(),
    internal(".getRestart", 1, do_get_restart),
    internal(".invokeRestart", 2, do_invoke_restart),
    internal(".addTryHandlers", 0, |_| unsafe { Ok(nil()) }).invisible(),
    internal(".addGlobHands", 5, |_| unsafe { Ok(nil()) }).invisible(),
    internal("warnings", 0, |_| unsafe { Ok(nil()) }),
    internal("interruptsSuspended", -1, |_| unsafe { values::scalar_bool(false) }),
];

unsafe fn do_add_cond_hands(call: &Call<'_>) -> R<Sexp> {
    let classes = call.arg(0);
    let handlers = call.arg(1);
    let parent_env = call.arg(2);
    let target = call.arg(3);
    let calling = values::as_logical_scalar(call.arg(4)) != 0;
    if classes == nil() || handlers == nil() {
        return Ok(nil());
    }
    if classes.kind() != SexpType::String
        || handlers.kind() != SexpType::List
        || length(classes) != length(handlers)
    {
        return Err(error("bad handler data"));
    }
    let state = interp();
    let mut stack = state.handlers.clone();
    for index in (0..length(classes)).rev() {
        stack = Some(Rc::new(Handler {
            class: classes.element(index),
            handler: handlers.element(index),
            calling,
            parent_env,
            target,
            next: stack,
        }));
    }
    state.handlers = stack;
    Ok(nil())
}

unsafe fn do_reset_cond_hands(_call: &Call<'_>) -> R<Sexp> {
    Ok(nil())
}

unsafe fn do_signal_condition(call: &Call<'_>) -> R<Sexp> {
    let condition = call.arg(0);
    let condition_call = call.arg(2);
    let classes = condition_classes(condition);
    let class_slices: Vec<&[u8]> = classes.iter().map(Vec::as_slice).collect();
    let saved = interp().handlers.clone();
    while let Some(entry) = find_handler(&interp().handlers, &class_slices) {
        interp().handlers = entry.next.clone();
        if entry.calling {
            let handler_call = values::call(entry.handler, &[condition])?;
            eval::eval(handler_call, global_env())?;
        } else {
            let result = exiting_result(condition, condition_call, entry.handler)?;
            return Err(jump(Jump::Return {
                env: entry.target,
                value: result,
            }));
        }
    }
    interp().handlers = saved;
    Ok(nil())
}

unsafe fn message_text(message: Sexp) -> R<String> {
    if message.kind() == SexpType::String && length(message) > 0 {
        Ok(string_at(message, 0).map(|text| text.into_owned()).unwrap_or_else(|| "NA".into()))
    } else {
        Err(error("error message not a string"))
    }
}

unsafe fn do_default_stop(call: &Call<'_>) -> R<Sexp> {
    let message = message_text(call.arg(0))?;
    Err(default_error(call.arg(1), &message))
}

unsafe fn do_default_warn(call: &Call<'_>) -> R<Sexp> {
    let message = message_text(call.arg(0))?;
    default_warning(call.arg(1), &message)?;
    Ok(nil())
}

/// The call `stop()` and `warning()` report: the caller of the closure that called them.
unsafe fn caller_call(env: Sexp) -> Sexp {
    let frames = &interp().frames;
    match frames.iter().rposition(|frame| frame.env == env) {
        Some(position) if position >= 1 => frames[position - 1].call,
        _ => nil(),
    }
}

unsafe fn do_stop(call: &Call<'_>) -> R<Sexp> {
    let use_call = values::as_logical_scalar(call.arg(0)) != 0;
    let condition_call = if use_call { caller_call(call.env) } else { nil() };
    let message = if length(call.arg(1)) > 0 {
        string_at(call.arg(1), 0).map(|text| text.into_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    Err(signal_error(RError {
        message,
        call: condition_call,
    }))
}

unsafe fn do_warning(call: &Call<'_>) -> R<Sexp> {
    let use_call = values::as_logical_scalar(call.arg(0)) != 0;
    let immediate = values::as_logical_scalar(call.arg(1)) == 1;
    let condition_call = if use_call { caller_call(call.env) } else { nil() };
    let message = if length(call.arg(3)) > 0 {
        string_at(call.arg(3), 0).map(|text| text.into_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    if immediate {
        let text = if condition_call == nil() {
            format!("Warning: {message}\n")
        } else {
            format!(
                "Warning in {} : {message}\n",
                deparse::deparse_one_line(condition_call)
            )
        };
        write_console(text.as_bytes(), true);
    } else {
        warning_call(condition_call, message.clone())?;
    }
    scalar_string(&message)
}

unsafe fn do_geterrmessage(_call: &Call<'_>) -> R<Sexp> {
    scalar_string(&interp().error_message)
}

unsafe fn do_seterrmessage(call: &Call<'_>) -> R<Sexp> {
    interp().error_message = message_text(call.arg(0))?;
    Ok(nil())
}

unsafe fn do_print_deferred_warnings(_call: &Call<'_>) -> R<Sexp> {
    if !interp().warnings.is_empty() {
        write_console(b"In addition: ", true);
        print_warnings();
    }
    Ok(nil())
}

unsafe fn do_add_restart(call: &Call<'_>) -> R<Sexp> {
    let restart = call.arg(0);
    if restart.kind() != SexpType::List || length(restart) < 2 {
        return Err(error("bad restart"));
    }
    let state = interp();
    state.restarts = Some(Rc::new(RestartEntry {
        restart,
        next: state.restarts.clone(),
    }));
    Ok(nil())
}

unsafe fn do_get_restart(call: &Call<'_>) -> R<Sexp> {
    let index = values::as_integer_scalar(call.arg(0));
    let mut entry = interp().restarts.clone();
    let mut position = 1;
    while let Some(restart) = entry {
        if position == index {
            return Ok(restart.restart);
        }
        position += 1;
        entry = restart.next.clone();
    }
    // Below every established restart sits top level's `abort`, whose exit is `NULL`.
    if position == index {
        let name = values::scalar_string("abort")?;
        let abort = values::list_vector(&[name, nil()])?;
        let class = values::scalar_string("restart")?;
        values::set_attr(abort, symbols().class, class)?;
        return Ok(abort);
    }
    Ok(nil())
}

unsafe fn do_invoke_restart(call: &Call<'_>) -> R<Sexp> {
    let restart = call.arg(0);
    let arguments = call.arg(1);
    if restart.kind() != SexpType::List || length(restart) < 2 {
        return Err(error("bad restart"));
    }
    let exit = restart.element(1);
    if exit == nil() {
        interp().restarts = None;
        return Err(jump(Jump::TopLevel));
    }
    let mut entry = interp().restarts.clone();
    while let Some(candidate) = entry {
        if candidate.restart.kind() == SexpType::List
            && length(candidate.restart) >= 2
            && candidate.restart.element(1) == exit
        {
            interp().restarts = candidate.next.clone();
            return Err(jump(Jump::Return {
                env: exit,
                value: arguments,
            }));
        }
        entry = candidate.next.clone();
    }
    Err(error("restart not on stack"))
}

/// The text of a condition's message, for printing.
pub unsafe fn condition_message(condition: Sexp) -> String {
    let message = values::list_get(condition, "message");
    if message.kind() == SexpType::String && length(message) > 0 {
        char_str(message.element(0)).into_owned()
    } else {
        String::new()
    }
}
