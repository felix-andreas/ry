//! Starting an R session and running scripts. Startup follows GNU R's: the system `Renviron`,
//! the C-level options, base R's own loader (`library/base/R/base`, which lazy-loads base from
//! its database), the system profile, `methods`, then `.First.sys` for the default packages.
//! Base R's R code is read from the user's GNU R installation, never vendored (see
//! `decisions.md`, "runtime licensing").

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        conditions, eval, interp, print, scope,
        unwind::{Jump, R, Unwind, error},
        values::{self, base_env, base_namespace, global_env, length, nil},
        write_console,
    },
    heap::object::{Sexp, SexpType},
};

pub const BUILTINS: &[BuiltinDef] = &[internal("mkUnbound", 1, do_mk_unbound).invisible()];

/// How a session starts.
pub struct StartOptions {
    /// `commandArgs()`.
    pub command_args: Vec<String>,
    /// Sources the site profile (`R_PROFILE`, else `R_HOME/etc/Rprofile.site`), which
    /// `--vanilla` and `--no-site-file` turn off.
    pub site_profile: bool,
    /// Sources the user profile (`R_PROFILE_USER`, else `./.Rprofile`, else `~/.Rprofile`) into
    /// the global environment, which `--vanilla` and `--no-init-file` turn off.
    pub user_profile: bool,
    /// `interactive()`, which also makes `keep.source` default to `TRUE` (base R's system
    /// profile sets it from `interactive()`).
    pub interactive: bool,
}

/// Starts the session. Errors here are fatal: base R did not load.
pub unsafe fn start(options: StartOptions) -> R<()> {
    interp().command_args = options.command_args;
    interp().interactive = options.interactive;
    read_renviron();
    crate::system::init_locale();
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
    let version = crate::system::r_version();
    let (major, minor) = crate::system::OLDEST_SUPPORTED_R;
    if (version.major, version.minor) < (major, minor) {
        return Err(crate::unwind::error(format!(
            "ry's runtime needs base R {major}.{minor} or later, but {home} holds R {}; \
             install a newer GNU R, or set R_HOME to one",
            version.number()
        )));
    }
    source_file(&format!("{home}/library/base/R/base"), base_namespace())?;
    source_file(&format!("{home}/library/base/R/Rprofile"), base_env())?;
    eval_text(".OptRequireMethods()", global_env())?;
    // GNU R's order: the site profile, then the user profile, both after `methods` loads.
    if options.site_profile {
        let site = std::env::var("R_PROFILE")
            .ok()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| format!("{home}/etc/Rprofile.site"));
        if std::path::Path::new(&site).exists() {
            source_file(&site, base_env())?;
        }
    }
    if options.user_profile {
        // A set `R_PROFILE_USER` is the only candidate, even when the file is missing (callr
        // points it at its own profile).
        let user = match std::env::var("R_PROFILE_USER")
            .ok()
            .filter(|path| !path.is_empty())
        {
            Some(path) => Some(crate::connections::expand_path(&path)),
            None => [
                ".Rprofile".to_owned(),
                crate::connections::expand_path("~/.Rprofile"),
            ]
            .into_iter()
            .find(|path| std::path::Path::new(path).exists()),
        };
        if let Some(user) = user.filter(|path| std::path::Path::new(path).exists()) {
            source_file(&user, global_env())?;
        }
    }
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
    let script = match crate::lower::parse_script(text, source_name) {
        Ok(script) => script,
        Err(Unwind::Error(failure)) => {
            write_console(
                format!("Error: {}\nExecution halted\n", failure.message).as_bytes(),
                true,
            );
            return Outcome::Failed;
        }
        Err(_) => return Outcome::Failed,
    };
    for node in &script.expressions {
        let mark = crate::heap().handle_mark();
        let succeeded = match crate::lower::lower_script_expression(&script, node, keep_source()) {
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
    }
    Outcome::Completed
}

/// `getOption("keep.source")`: whether console input keeps its source references.
unsafe fn keep_source() -> bool {
    values::as_logical_scalar(crate::envir::get_option("keep.source")) == 1
}

/// Runs a read-eval-print loop: `read` returns the next chunk of input after showing a prompt,
/// or `None` at the end of input. Input that ends inside an unfinished expression is
/// continued on the next chunk, and each complete expression runs as its own top-level call.
/// With `halt_on_error`, the console is GNU R's non-interactive one (`R < file`, or callr
/// driving `R` over a pipe): the first error ends the session with `Execution halted`, unless
/// `options(error =)` holds a handler.
pub unsafe fn run_console(
    mut read: impl FnMut(&str) -> Option<String>,
    halt_on_error: bool,
) -> Outcome {
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
        match crate::lower::parse_script(&text, "<text>") {
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
                        if halt_on_error && crate::envir::get_option("error") == nil() {
                            crate::heap().release_handles(mark);
                            write_console(b"Execution halted\n", true);
                            return Outcome::Failed;
                        }
                        break;
                    }
                }
            }
            Err(Unwind::Error(failure)) => {
                write_console(format!("Error: {}\n", failure.message).as_bytes(), true);
                if halt_on_error {
                    crate::heap().release_handles(mark);
                    write_console(b"Execution halted\n", true);
                    return Outcome::Failed;
                }
            }
            Err(_) => {}
        }
        crate::heap().release_handles(mark);
    }
    Outcome::Completed
}

/// Ends a session as GNU R does before it exits: closes every graphics device, which writes
/// the files they were drawing.
pub unsafe fn finish() {
    if let Err(unwind) = crate::graphics_engine::kill_all_devices() {
        report_toplevel(unwind);
    }
    if let Err(unwind) = crate::utils::stop_profiler() {
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
        // A Ctrl-C that arrived after the last poll belongs to this command, not the next.
        crate::unwind::poll_interrupt()?;
        value = result;
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
    interp().handlers = interp().global_handlers.clone();
    interp().restarts = None;
    interp().srcref = nil();
    conditions::print_warnings();
    // The task's warnings print first, as in GNU R, and the callbacks' own warnings after them.
    let visible = interp().visible;
    if let Err(unwind) = (interp().hooks.run_task_callbacks)(expression, value, succeeded, visible)
    {
        report_toplevel(unwind);
        interp().frames.clear();
        interp().handlers = interp().global_handlers.clone();
        interp().restarts = None;
    }
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
