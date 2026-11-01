//! Starting an R session and running scripts. Startup follows GNU R's: the system `Renviron`,
//! the C-level options, base R's own loader (`library/base/R/base`, which lazy-loads base from
//! its database), the system profile, `methods`, then `.First.sys` for the default packages.
//! Base R's R code is read from the user's GNU R installation, never vendored (see
//! `decisions.md`, "runtime licensing").

use crate::{
    builtins::{BuiltinDef, Call, internal},
    conditions, eval, interp, print, scope,
    unwind::{Jump, R, Unwind, error},
    values::{self, base_env, base_namespace, global_env, length, nil},
    write_console,
};
use heap::object::{Sexp, SexpType};
use std::cell::RefCell;
use std::rc::Rc;

pub const BUILTINS: &[BuiltinDef] = &[internal("mkUnbound", 1, do_mk_unbound).invisible()];

/// How a session starts.
pub struct StartOptions {
    /// `commandArgs()`.
    pub command_args: Vec<String>,
    /// Skips the site and user profiles, as `--vanilla` does.
    pub vanilla: bool,
    /// `interactive()`, which also makes `keep.source` default to `TRUE` (base R's system
    /// profile sets it from `interactive()`).
    pub interactive: bool,
}

/// Starts the session. Errors here are fatal: base R did not load.
pub unsafe fn start(options: StartOptions) -> R<()> {
    interp().command_args = options.command_args;
    interp().interactive = options.interactive;
    // The runtime reports a UTF-8 locale (`l10n_info`), so the C library's character
    // functions must agree: iconv's `//TRANSLIT` and packages calling `mbrtowc` read
    // LC_CTYPE, which stays "C" (ASCII) until something sets it. Numeric formatting keeps
    // the C locale, so only LC_CTYPE changes. A system without C.UTF-8 keeps ASCII.
    libc::setlocale(libc::LC_CTYPE, c"C.UTF-8".as_ptr());
    read_renviron();
    crate::envir::install_default_options()?;
    let machine = crate::system::machine()?;
    eval::define(values::install(".Machine")?, machine, base_env())?;
    let platform = crate::system::platform()?;
    eval::define(values::install(".Platform")?, platform, base_env())?;
    crate::graphics::define_device_variables()?;
    eval::define(
        values::install(".BaseNamespaceEnv")?,
        base_namespace(),
        base_env(),
    )?;
    eval::define(
        values::install("base")?,
        base_namespace(),
        crate::heap().known().namespace_registry,
    )?;
    let home = crate::system::r_home();
    source_file(&format!("{home}/library/base/R/base"), base_namespace())?;
    source_file(&format!("{home}/library/base/R/Rprofile"), base_env())?;
    if !options.vanilla {
        let site =
            std::env::var("R_PROFILE").unwrap_or_else(|_| format!("{home}/etc/Rprofile.site"));
        if std::path::Path::new(&site).exists() {
            source_file(&site, base_env())?;
        }
    }
    eval_text(".OptRequireMethods()", global_env())?;
    eval_text(".First.sys()", global_env())?;
    crate::frame::attach_package()?;
    conditions::print_warnings()?;
    Ok(())
}

/// Evaluates every expression of a file in `env`, without printing, as GNU R sources its startup
/// files.
pub unsafe fn source_file(path: &str, env: Sexp) -> R<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|failure| error(format!("cannot open file '{path}': {failure}")))?;
    let expressions = crate::lower::parse_text(&text, path)?;
    crate::heap().root(expressions);
    for index in 0..length(expressions) {
        scope(|| eval::eval(expressions.element(index), env))?;
    }
    Ok(())
}

pub(crate) unsafe fn eval_text(text: &str, env: Sexp) -> R<Sexp> {
    let expressions = crate::lower::parse_text(text, "<startup>")?;
    let mut value = nil();
    for index in 0..length(expressions) {
        value = eval::eval(expressions.element(index), env)?;
    }
    Ok(value)
}

/// Reads `R_HOME/etc/Renviron` the way GNU R does: `NAME=value` lines, where `${VAR-default}`
/// and `${VAR:-default}` expand, and a variable already set wins.
fn read_renviron() {
    let home = crate::system::r_home();
    // SAFETY: the session starts before any other thread exists.
    unsafe { std::env::set_var("R_HOME", &home) };
    export_wrapper_directories(&home);
    for path in [format!("{home}/etc/Renviron"), "/etc/R/Renviron".to_owned()] {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if std::env::var_os(name).is_some() {
                continue;
            }
            let value = expand_environment(value.trim().trim_matches(|c| c == '\'' || c == '"'));
            // SAFETY: as above.
            unsafe { std::env::set_var(name, value) };
        }
        break;
    }
}

/// GNU R's `bin/R` shell wrapper exports `R_SHARE_DIR`, `R_INCLUDE_DIR` and `R_DOC_DIR`
/// before R starts, and `R.home("share")` and `R.home("doc")` read them. Distributions that
/// keep those directories outside `R_HOME` (Debian puts them under `/usr/share/R`) need them
/// recovered from the wrapper's plain `NAME=value` lines; a value that is still a shell
/// expression is left alone.
fn export_wrapper_directories(home: &str) {
    let Ok(wrapper) = std::fs::read_to_string(format!("{home}/bin/R")) else {
        return;
    };
    for name in ["R_SHARE_DIR", "R_INCLUDE_DIR", "R_DOC_DIR"] {
        if std::env::var_os(name).is_some() {
            continue;
        }
        let value = wrapper.lines().find_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            (key == name).then(|| value.trim_matches(|c| c == '\'' || c == '"'))
        });
        if let Some(value) = value
            && !value.is_empty()
            && !value.contains('$')
        {
            // SAFETY: the session starts before any other thread exists.
            unsafe { std::env::set_var(name, value) };
        }
    }
}

fn expand_environment(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        // Find the matching brace, allowing nesting.
        let mut depth = 1;
        let mut end = None;
        for (index, character) in after.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            result.push_str(&rest[start..]);
            return result;
        };
        let inner = &after[..end];
        let (name, default) = match inner
            .find(":-")
            .map(|position| (position, 2))
            .or_else(|| inner.find('-').map(|position| (position, 1)))
        {
            Some((position, width)) => (&inner[..position], Some(&inner[position + width..])),
            None => (inner, None),
        };
        match std::env::var(name) {
            Ok(found) if !found.is_empty() => result.push_str(&found),
            _ => {
                if let Some(default) = default {
                    result.push_str(&expand_environment(
                        default.trim_matches(|c| c == '\'' || c == '"'),
                    ));
                }
            }
        }
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    result
}

/// How a script run ended.
pub enum Outcome {
    Completed,
    Failed,
}

/// Runs a script the way `Rscript` does: each top-level expression is evaluated in the global
/// environment, a visible value is printed, and the first error stops the run.
///
/// Like `Rscript`, which reads the script as console input, each expression is lowered just
/// before it runs, so `options(keep.source = TRUE)` keeps source references for the expressions
/// after it.
pub unsafe fn run_text(text: &str, source_name: &str) -> Outcome {
    let text: Rc<str> = Rc::from(text);
    let previous = SCRIPT_INPUT.with(|input| {
        input.replace(Some(ScriptInput {
            text: Rc::clone(&text),
            position: 0,
        }))
    });
    let outcome = run_script(&text, source_name);
    SCRIPT_INPUT.with(|input| input.replace(previous));
    outcome
}

/// A script being run, which is also what `stdin()` reads: GNU R reads a script as its console
/// input, so `readLines(stdin())` in a script returns the script's next lines and the script
/// carries on after them.
struct ScriptInput {
    text: Rc<str>,
    /// Where the console reads next: the start of the line after the expression running now.
    position: usize,
}

thread_local! {
    static SCRIPT_INPUT: RefCell<Option<ScriptInput>> = const { RefCell::new(None) };
}

/// Reads the running script's next bytes into `buffer` for `stdin()`, or returns `None` when no
/// script is running and `stdin()` is the process's standard input.
pub fn read_script_input(buffer: &mut [u8]) -> Option<usize> {
    SCRIPT_INPUT.with(|input| {
        let mut input = input.borrow_mut();
        let input = input.as_mut()?;
        let mut count = 0;
        while count < buffer.len() {
            let text = &input.text;
            let escaped = text
                .get(input.position..)
                .and_then(|rest| rest.chars().next())
                .and_then(|character| {
                    values::escaped_byte(character).map(|byte| (byte, character.len_utf8()))
                });
            let (byte, width) = match escaped {
                Some(escaped) => escaped,
                None => match text.as_bytes().get(input.position) {
                    Some(&byte) => (byte, 1),
                    None => break,
                },
            };
            buffer[count] = byte;
            count += 1;
            input.position += width;
        }
        Some(count)
    })
}

unsafe fn run_script(text: &str, source_name: &str) -> Outcome {
    let mut start = 0;
    let mut line_offset = 0;
    'parse: loop {
        let rest = &text[start..];
        let (script, failure) = crate::lower::parse_script_prefix(rest);
        for node in &script.expressions {
            let end = usize::from(node.text_range().end()).min(rest.len());
            let next_line = rest[end..]
                .find('\n')
                .map_or(rest.len(), |newline| end + newline + 1);
            let cursor = start + next_line;
            SCRIPT_INPUT.with(|input| {
                if let Some(input) = input.borrow_mut().as_mut() {
                    input.position = cursor;
                }
            });
            let mark = crate::heap().handle_mark();
            let succeeded =
                match crate::lower::lower_script_expression(&script, node, keep_source()) {
                    Ok(expression) => run_toplevel(expression),
                    Err(unwind) => {
                        report_toplevel(unwind);
                        false
                    }
                };
            crate::heap().release_handles(mark);
            // A script stops at its first error unless `options(error =)` holds a handler, which
            // GNU R runs and then carries on with the next expression.
            if !succeeded && crate::envir::get_option("error") == nil() {
                write_console(b"Execution halted\n", true);
                return Outcome::Failed;
            }
            let position = SCRIPT_INPUT
                .with(|input| input.borrow().as_ref().map(|input| input.position))
                .unwrap_or(cursor);
            if position != cursor {
                // The expression read the script's next lines as data, so parsing resumes after
                // them.
                let mut resumed = position.min(text.len());
                while !text.is_char_boundary(resumed) {
                    resumed += 1;
                }
                line_offset += text[start..resumed].matches('\n').count();
                start = resumed;
                continue 'parse;
            }
        }
        if let Some(failure) = failure {
            if failure.condition.is_some() {
                report_toplevel(crate::lower::syntax_failure_unwind(
                    rest,
                    &failure,
                    source_name,
                    "<input>",
                    line_offset,
                ));
                write_console(b"Execution halted\n", true);
                return Outcome::Failed;
            }
            let message =
                crate::lower::syntax_failure_message(rest, &failure, source_name, line_offset);
            write_console(
                format!("Error: {message}\nExecution halted\n").as_bytes(),
                true,
            );
            return Outcome::Failed;
        }
        return Outcome::Completed;
    }
}

/// `getOption("keep.source")`: whether console input keeps its source references.
unsafe fn keep_source() -> bool {
    values::as_logical_scalar(crate::envir::get_option("keep.source")) == 1
}

/// Runs a read-eval-print loop: `read` returns the next chunk of input after showing a prompt,
/// or `None` at the end of input. Input that ends inside an unfinished expression is
/// continued on the next chunk, and each complete expression runs as its own top-level call.
pub unsafe fn run_console(mut read: impl FnMut(&str) -> Option<String>) {
    let mut buffer = String::new();
    loop {
        let prompt = if buffer.is_empty() { "> " } else { "+ " };
        let Some(chunk) = read(prompt) else {
            break;
        };
        buffer.push_str(&chunk);
        if !buffer.ends_with('\n') {
            buffer.push('\n');
        }
        if syntax::input_is_unfinished(&buffer) {
            continue;
        }
        let text = std::mem::take(&mut buffer);
        let mark = crate::heap().handle_mark();
        match crate::lower::parse_script(&text, "<text>", "<input>") {
            Ok(script) => {
                for node in &script.expressions {
                    let succeeded =
                        match crate::lower::lower_script_expression(&script, node, keep_source()) {
                            Ok(expression) => run_toplevel(expression),
                            Err(unwind) => {
                                report_toplevel(unwind);
                                false
                            }
                        };
                    if !succeeded {
                        break;
                    }
                }
            }
            Err(Unwind::Error(failure)) => {
                write_console(format!("Error: {}\n", failure.message).as_bytes(), true);
            }
            Err(_) => {}
        }
        crate::heap().release_handles(mark);
    }
}

/// Ends a session as GNU R does before it exits: closes every graphics device, which writes
/// the files they were drawing.
pub unsafe fn finish() {
    if let Err(unwind) = crate::graphics::close_all_devices() {
        report_toplevel(unwind);
    }
}

/// Evaluates one top-level expression, prints it if visible, and reports deferred warnings.
/// Returns whether it succeeded.
pub unsafe fn run_toplevel(expression: Sexp) -> bool {
    let mark = crate::heap().handle_mark();
    interp().visible = true;
    let mut value = nil();
    let result = eval::eval(expression, global_env()).and_then(|result| {
        value = result;
        crate::heap().root(value);
        let last = values::install(".Last.value")?;
        crate::heap().define(base_env(), last, value)?;
        // GNU R's `.Last.value` does not count as a reference, so a value the user just
        // bound stays unshared and `.Internal(refcnt())` reports what the user's own
        // bindings hold.
        if let Some(cell) = crate::heap().frame_cell(base_env(), last) {
            crate::heap().untrack_references(cell);
        }
        if interp().visible {
            print::print_value_top(value, global_env())?;
        }
        Ok(())
    });
    let succeeded = match result {
        Ok(()) => true,
        Err(unwind) => {
            report_toplevel(unwind);
            false
        }
    };
    interp().frames.clear();
    interp().handlers = interp().global_handlers.clone();
    interp().restarts = None;
    interp().srcref = nil();
    if let Err(unwind) = conditions::print_warnings() {
        report_toplevel(unwind);
    }
    // The task's warnings print first, as in GNU R, and the callbacks' own warnings after them.
    let visible = interp().visible;
    if let Err(unwind) = (interp().hooks.run_task_callbacks)(expression, value, succeeded, visible)
    {
        report_toplevel(unwind);
        interp().frames.clear();
        interp().handlers = interp().global_handlers.clone();
        interp().restarts = None;
    }
    if let Err(unwind) = conditions::print_warnings() {
        report_toplevel(unwind);
    }
    crate::heap().release_handles(mark);
    succeeded
}

/// Reports an unwind that reached top level.
pub unsafe fn report_toplevel(unwind: Unwind) {
    match unwind {
        Unwind::Error(failure) => {
            // An error that was never signalled, such as one raised by native code outside any
            // R call.
            let call = crate::unwind::resolve_call(failure.call);
            let text = conditions::format_error(call, &failure.message);
            write_console(text.as_bytes(), true);
        }
        Unwind::Interrupt => write_console(b"\n", false),
        Unwind::Jump(jump) => match *jump {
            Jump::TopLevel => {}
            Jump::Return { .. } => {
                write_console(
                    b"Error: no function to return from, jumping to top level\n",
                    true,
                );
            }
            Jump::Break { .. } | Jump::Next { .. } => {
                write_console(
                    b"Error: no loop for break/next, jumping to top level\n",
                    true,
                );
            }
        },
    }
}

unsafe fn do_mk_unbound(call: &Call<'_>) -> R<Sexp> {
    let symbol = call.arg(0);
    if symbol.kind() != SexpType::Symbol {
        return Err(call.error("not a symbol"));
    }
    crate::heap().remove_binding(base_env(), symbol)?;
    Ok(nil())
}
