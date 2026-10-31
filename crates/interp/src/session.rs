//! Starting an R session and running scripts. Startup follows GNU R's: the system `Renviron`,
//! the C-level options, base R's own loader (`library/base/R/base`, which lazy-loads base from
//! its database), the system profile, `methods`, then `.First.sys` for the default packages.
//! Base R's R code is read from the user's GNU R installation, never vendored (see
//! `decisions.md`, "runtime licensing").

use crate::{
    builtins::{BuiltinDef, Call, internal},
    conditions, eval, heap, interp, print, scope,
    unwind::{Jump, R, Unwind, error},
    values::{self, base_env, base_namespace, global_env, length, nil},
    write_console,
};
use heap::object::{Sexp, SexpType};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("mkUnbound", 1, do_mk_unbound).invisible(),
];

/// How a session starts.
pub struct StartOptions {
    /// `commandArgs()`.
    pub command_args: Vec<String>,
    /// Skips the site and user profiles, as `--vanilla` does.
    pub vanilla: bool,
}

/// Starts the session. Errors here are fatal: base R did not load.
pub unsafe fn start(options: StartOptions) -> R<()> {
    interp().command_args = options.command_args;
    read_renviron();
    crate::envir::install_default_options()?;
    let machine = crate::system::machine()?;
    eval::define(values::install(".Machine")?, machine, base_env())?;
    let platform = crate::system::platform()?;
    eval::define(values::install(".Platform")?, platform, base_env())?;
    crate::graphics::define_device_variables()?;
    eval::define(values::install(".BaseNamespaceEnv")?, base_namespace(), base_env())?;
    eval::define(values::install("base")?, base_namespace(), crate::heap().known().namespace_registry)?;
    let home = crate::system::r_home();
    source_file(&format!("{home}/library/base/R/base"), base_namespace())?;
    source_file(&format!("{home}/library/base/R/Rprofile"), base_env())?;
    if !options.vanilla {
        let site = std::env::var("R_PROFILE").unwrap_or_else(|_| format!("{home}/etc/Rprofile.site"));
        if std::path::Path::new(&site).exists() {
            source_file(&site, base_env())?;
        }
    }
    eval_text(".OptRequireMethods()", global_env())?;
    eval_text(".First.sys()", global_env())?;
    crate::frame::attach_package()?;
    conditions::print_warnings();
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
        let (name, default) = match inner.find(":-").map(|position| (position, 2)).or_else(|| inner.find('-').map(|position| (position, 1))) {
            Some((position, width)) => (&inner[..position], Some(&inner[position + width..])),
            None => (inner, None),
        };
        match std::env::var(name) {
            Ok(found) if !found.is_empty() => result.push_str(&found),
            _ => {
                if let Some(default) = default {
                    result.push_str(&expand_environment(default.trim_matches(|c| c == '\'' || c == '"')));
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
pub unsafe fn run_text(text: &str, source_name: &str) -> Outcome {
    let expressions = match crate::lower::parse_text(text, source_name) {
        Ok(expressions) => expressions,
        Err(Unwind::Error(failure)) => {
            write_console(format!("Error: {}\nExecution halted\n", failure.message).as_bytes(), true);
            return Outcome::Failed;
        }
        Err(_) => return Outcome::Failed,
    };
    crate::heap().root(expressions);
    for index in 0..length(expressions) {
        let expression = expressions.element(index);
        if !run_toplevel(expression) {
            write_console(b"Execution halted\n", true);
            return Outcome::Failed;
        }
    }
    Outcome::Completed
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
        match crate::lower::parse_text(&text, "<text>") {
            Ok(expressions) => {
                crate::heap().root(expressions);
                for index in 0..length(expressions) {
                    if !run_toplevel(expressions.element(index)) {
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

/// Evaluates one top-level expression, prints it if visible, and reports deferred warnings.
/// Returns whether it succeeded.
pub unsafe fn run_toplevel(expression: Sexp) -> bool {
    let mark = crate::heap().handle_mark();
    interp().visible = true;
    let result = eval::eval(expression, global_env()).and_then(|value| {
        crate::heap().root(value);
        let last = values::install(".Last.value")?;
        crate::heap().define(base_env(), last, value)?;
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
    interp().handlers = None;
    interp().restarts = None;
    conditions::print_warnings();
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
                write_console(b"Error: no function to return from, jumping to top level\n", true);
            }
            Jump::Break { .. } | Jump::Next { .. } => {
                write_console(b"Error: no loop for break/next, jumping to top level\n", true);
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
