//! A `reedline` editor running inside R's `ReadConsole` callback; R itself still
//! parses, evaluates, and autoprints the fed input. The same editor drives ry's own runtime,
//! whose console loop calls the callback the same way.

use crate::libr::{self, RApi};
use crate::{
    ConsoleHooks, Filtered, InputFilter, Keybindings, ReplError, RunOptions, SessionCompleter,
};
use crossterm::terminal;
use nu_ansi_term::{Color, Style};
use reedline::{
    ColumnarMenu, Completer, DefaultHinter, EditMode, Emacs, FileBackedHistory, Highlighter,
    KeyCode, KeyModifiers, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
    PromptHistorySearchStatus, Reedline, ReedlineEvent, ReedlineMenu, Signal, StyledText,
    Suggestion, ValidationResult, Validator, Vi, default_emacs_keybindings,
    default_vi_insert_keybindings, default_vi_normal_keybindings,
};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_int, c_uchar};
use std::io::{BufRead, IsTerminal, Write, stderr, stdin, stdout};
use std::panic::catch_unwind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::{fs, slice};
use syntax::SyntaxKind::{self, *};
use syntax::{TextSize, lex};

pub fn run(api: RApi, mut options: RunOptions) -> Result<(), ReplError> {
    let batch = options.batch;
    let routines = std::mem::take(&mut options.routines);
    let startup = std::mem::take(&mut options.startup);
    let arguments = std::mem::take(&mut options.arguments)
        .into_iter()
        .map(CString::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ReplError("a script argument contains a NUL byte".into()))?;
    install(options, false)?;
    install_sigint_handler();
    // A batch run prints only what its code prints, and so does a console reading a pipe.
    let quiet = batch || !stdin().is_terminal();
    api.initialize(read_console, write_console_ex, quiet, &arguments)?;
    api.register_call_routines(&routines);
    for statement in &startup {
        api.eval(statement);
    }
    if !batch {
        // R's `width` defaults to 80 whatever the terminal is, and R exports no setter.
        // Evaluated directly: fed input taken before the first prompt leaves the terminal
        // in R's state. Profiles have run, so only R's untouched default is replaced.
        if let Ok((columns, _)) = terminal::size()
            && columns > 0
        {
            api.eval(&format!(
                "if (isTRUE(getOption(\"width\") == 80L)) options(width = {columns}L)"
            ));
        }
        if !quiet {
            eprintln!(
                "ry R console, R at {}. Type q() or Ctrl-D to quit.",
                api.r_home.display()
            );
        }
    }
    api.run_main_loop();
    Ok(())
}

/// Installs the console for ry's own runtime, whose console loop reads through
/// [`ry_read_console`]. Every read it makes is at the top-level prompt.
pub fn install_for_ry_runtime(options: RunOptions) -> Result<(), ReplError> {
    install(options, true)
}

/// The read hook ry's runtime calls: GNU R's `R_ReadConsole` without the history flag.
pub extern "C" fn ry_read_console(
    prompt: *const c_char,
    buffer: *mut c_uchar,
    length: c_int,
) -> c_int {
    catch_unwind(|| read_console_inner(prompt, buffer, length)).unwrap_or(0)
}

fn install(options: RunOptions, ry_runtime: bool) -> Result<(), ReplError> {
    let completion_menu_binding = ReedlineEvent::UntilFound(vec![
        ReedlineEvent::Menu("completion_menu".to_owned()),
        ReedlineEvent::MenuNext,
    ]);
    let edit_mode: Box<dyn EditMode> = match options.keybindings {
        Keybindings::Emacs => {
            let mut bindings = default_emacs_keybindings();
            bindings.add_binding(KeyModifiers::NONE, KeyCode::Tab, completion_menu_binding);
            Box::new(Emacs::new(bindings))
        }
        Keybindings::Vi => {
            let mut insert = default_vi_insert_keybindings();
            insert.add_binding(KeyModifiers::NONE, KeyCode::Tab, completion_menu_binding);
            Box::new(Vi::new(insert, default_vi_normal_keybindings()))
        }
    };
    let mut editor = Reedline::create()
        .with_highlighter(Box::new(LexerHighlighter))
        .with_hinter(Box::new(
            DefaultHinter::default().with_style(Style::new().fg(Color::DarkGray)),
        ))
        .with_validator(Box::new(LexerValidator))
        .with_edit_mode(edit_mode);
    let completer = options
        .completer
        .map(|completer| Arc::new(Mutex::new(completer)));
    if let Some(shared) = &completer {
        editor = editor
            .with_completer(Box::new(EditorCompleter(shared.clone())))
            .with_menu(ReedlineMenu::EngineCompleter(Box::new(
                ColumnarMenu::default().with_name("completion_menu"),
            )));
    }
    if let Some(history) = history_file()
        && let Ok(history) = FileBackedHistory::with_file(1000, history)
    {
        editor = editor.with_history(Box::new(history));
    }
    let mut pending = VecDeque::new();
    // The Windows callback wiring requires RGui mode, which sets `.Platform$GUI` to
    // "Rgui" and lets packages call Rgui-only functions that fail here. Startup
    // profiles run before this and still see "Rgui".
    #[cfg(windows)]
    pending.extend(
        b"invisible(local({ e <- baseenv(); locked <- bindingIsLocked(\".Platform\", e); \
          if (locked) unlockBinding(\".Platform\", e); \
          p <- get(\".Platform\", envir = e); p$GUI <- \"ry\"; \
          assign(\".Platform\", p, envir = e); \
          if (locked) lockBinding(\".Platform\", e) }))\n",
    );
    let script = match &options.file {
        Some(path) => Some(
            fs::read_to_string(path)
                .map_err(|error| ReplError(format!("cannot read {}: {error}", path.display())))?,
        ),
        None => None,
    };
    if options.batch && (script.is_some() || !options.input.is_empty()) {
        // A top-level error quits with a failing status, which becomes the exit code.
        pending.extend(b"options(error = function() q(status = 1, save = \"no\"))\n");
    }
    if !options.input.is_empty() {
        pending.extend(normalize_line_endings(&options.input).into_bytes());
        pending.push_back(b'\n');
    }
    let mut filter = options.filter;
    if let Some(script) = script {
        let script = normalize_line_endings(&script);
        // A script whose filter refuses it does not run at all, and an unfinished one is R's
        // to report, so the filter forgets it rather than prepend it to the first command.
        let script = match filter
            .as_mut()
            .map(|filter| (filter.filter(&script), filter))
        {
            Some((Filtered::Run(script), _)) => script,
            Some((Filtered::Refuse, _)) => String::new(),
            Some((Filtered::Hold, filter)) => {
                filter.interrupt();
                script
            }
            None => script,
        };
        pending.extend(script.into_bytes());
        pending.push_back(b'\n');
    }
    CONSOLE.with(|slot| {
        *slot.borrow_mut() = Some(Console {
            editor,
            pending,
            batch: options.batch,
            completer,
            hooks: options.hooks,
            filter,
            held: false,
            ry_runtime,
            piped: !stdin().is_terminal(),
        });
    });
    Ok(())
}

// R calls the hooks only on its one thread and never nests read-console calls
// inside our borrow, so a thread-local `RefCell` suffices.
thread_local! {
    static CONSOLE: RefCell<Option<Console>> = const { RefCell::new(None) };
}

type SharedSessionCompleter = Arc<Mutex<Box<dyn SessionCompleter>>>;

/// Shared because the console also feeds accepted lines to the editor's completer.
struct EditorCompleter(SharedSessionCompleter);

impl Completer for EditorCompleter {
    fn complete(&mut self, line: &str, position: usize) -> Vec<Suggestion> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .complete(line, position)
    }
}

struct Console {
    editor: Reedline,
    /// Accepted input R has not consumed; handed over in buffer-sized chunks.
    pending: VecDeque<u8>,
    /// End the session (EOF) once `pending` is exhausted instead of prompting.
    batch: bool,
    completer: Option<SharedSessionCompleter>,
    hooks: Option<Box<dyn ConsoleHooks>>,
    /// Sees each top-level command before R does, and may rewrite or refuse it.
    filter: Option<Box<dyn InputFilter>>,
    /// The filter holds an unfinished command, so the next read continues it.
    held: bool,
    /// Driving ry's own runtime, whose console reads only at the top-level prompt.
    ry_runtime: bool,
    /// Standard input is not a terminal, so lines are read from it as they come, without the
    /// editor (which needs a terminal that answers cursor queries) and without a prompt, as
    /// `R --no-echo` reads a pipe.
    piped: bool,
}

/// R's parser rejects a raw `\r`, but scripts and the editor buffer carry CRLF on
/// Windows; CRLF and lone CR are line endings, as in R's text-mode connections.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn history_file() -> Option<PathBuf> {
    // Best-effort: on failure the editor keeps in-memory history.
    let data = dirs::data_dir()?;
    let directory = data.join("ry");
    // `roughly` is the project's former name; move its history rather than discard it.
    let previous = data.join("roughly");
    if !directory.exists() && previous.is_dir() {
        // A failed move only costs the old history; the editor then starts a fresh one.
        fs::rename(&previous, &directory).ok();
    }
    fs::create_dir_all(&directory).ok()?;
    Some(directory.join("history.txt"))
}

/// The `R_ReadConsole` hook: returns 1 with a chunk of input in `buffer`, or 0
/// for end-of-session.
extern "C" fn read_console(
    prompt: *const c_char,
    buffer: *mut c_uchar,
    length: c_int,
    _add_to_history: c_int,
) -> c_int {
    // An unwind must never cross into R; a panic ends the session as EOF.
    catch_unwind(|| read_console_inner(prompt, buffer, length)).unwrap_or(0)
}

fn read_console_inner(prompt: *const c_char, buffer: *mut c_uchar, length: c_int) -> c_int {
    // Output R wrote before prompting must show above the prompt; a failed flush leaves
    // nothing to report it to.
    stdout().flush().ok();
    let capacity = length.max(2) as usize - 2;
    CONSOLE.with(|console| {
        let mut slot = console.borrow_mut();
        let Some(console) = slot.as_mut() else {
            return 0;
        };
        while console.pending.is_empty() {
            if console.batch {
                return 0;
            }
            // R hands its hook a valid NUL-terminated prompt (or null).
            let prompt_text = match console.held {
                true => "+ ".to_owned(),
                false => unsafe { libr::prompt_text(prompt) },
            };
            // Input for `readline()`, `menu()` or the browser must reach them untouched.
            let at_top_level = console.ry_runtime || libr::at_top_level();
            let signal = if console.piped {
                Ok(read_piped_line())
            } else {
                console.editor.read_line(&RPrompt { text: prompt_text })
            };
            match signal {
                Ok(Signal::Success(line)) => {
                    let line = normalize_line_endings(&line);
                    let line = match console.filter.as_mut() {
                        Some(filter) if at_top_level => {
                            let filtered = filter.filter(&line);
                            console.held = filtered == Filtered::Hold;
                            match filtered {
                                Filtered::Run(line) => line,
                                Filtered::Hold | Filtered::Refuse => continue,
                            }
                        }
                        _ => line,
                    };
                    if at_top_level
                        && let Some(hooks) = console.hooks.as_mut()
                        && let Some(code) = hooks.before_command()
                    {
                        console.pending.extend(code.into_bytes());
                        console.pending.push_back(b'\n');
                    }
                    if let Some(completer) = &console.completer {
                        completer
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .accept(&line);
                    }
                    console.pending.extend(line.into_bytes());
                    console.pending.push_back(b'\n');
                }
                Ok(Signal::CtrlC) if console.held => {
                    console.held = false;
                    if let Some(filter) = console.filter.as_mut() {
                        filter.interrupt();
                    }
                }
                Ok(Signal::CtrlC) => continue,
                // A terminal the editor can no longer drive ends the session like Ctrl-D.
                Ok(Signal::CtrlD) | Err(_) => return 0,
            }
        }
        let written = capacity.min(console.pending.len());
        for (index, byte) in console.pending.drain(..written).enumerate() {
            unsafe { *buffer.add(index) = byte };
        }
        unsafe { *buffer.add(written) = 0 };
        1
    })
}

/// One line of piped input, or end-of-input when the pipe closes or cannot be read.
fn read_piped_line() -> Signal {
    let mut line = String::new();
    match stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => Signal::CtrlD,
        Ok(_) => Signal::Success(line.trim_end_matches(['\n', '\r']).to_owned()),
    }
}

/// The `R_WriteConsoleEx` hook: `output_type` 0 is regular output, anything else
/// the error stream. Unbuffered so output interleaves correctly with prompts.
extern "C" fn write_console_ex(text: *const c_char, length: c_int, output_type: c_int) {
    // An unwind must never cross into R; a panic here drops only this piece of output.
    catch_unwind(|| {
        if text.is_null() || length <= 0 {
            return;
        }
        let bytes = unsafe { slice::from_raw_parts(text as *const u8, length as usize) };
        if output_type == 0 {
            echo(stdout(), bytes);
        } else {
            echo(stderr(), bytes);
        }
    })
    .ok();
}

fn echo(mut stream: impl Write, bytes: &[u8]) {
    // R has no channel for a failed console write (a closed pipe, say), so the output
    // is dropped and R keeps running.
    stream.write_all(bytes).and_then(|()| stream.flush()).ok();
}

/// While the editor runs, the terminal is raw (no signal); while R evaluates,
/// Ctrl-C arrives as SIGINT and raises R's cooperative interrupt flag.
#[cfg(unix)]
fn install_sigint_handler() {
    use std::{mem, ptr};
    unsafe {
        let mut action: libc::sigaction = mem::zeroed();
        action.sa_sigaction = libr::sigint_to_r_flag as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, ptr::null_mut());
    }
}

/// Raises both of R's interrupt flags (`UserBreak` and `R_interrupts_pending`),
/// honored at R's next interrupt check.
#[cfg(windows)]
fn install_sigint_handler() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_c_to_r_flags), 1);
    }
}

#[cfg(windows)]
unsafe extern "system" fn ctrl_c_to_r_flags(ctrl_type: u32) -> i32 {
    const CTRL_C_EVENT: u32 = 0;
    if ctrl_type == CTRL_C_EVENT {
        libr::interrupt_r();
        1
    } else {
        0
    }
}

/// No R session can start here (`libr::load` refuses), so there is nothing to interrupt.
#[cfg(not(any(unix, windows)))]
fn install_sigint_handler() {}

struct RPrompt {
    text: String,
}

impl Prompt for RPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        "".into()
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        "".into()
    }

    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        // R's own prompt, so debugger and readline() prompts look exactly like stock R.
        Cow::Borrowed(&self.text)
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        "+ ".into()
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }

    fn get_prompt_color(&self) -> reedline::Color {
        reedline::Color::Reset
    }

    fn get_prompt_multiline_color(&self) -> Color {
        Color::Default
    }

    fn get_indicator_color(&self) -> reedline::Color {
        reedline::Color::Reset
    }

    fn get_prompt_right_color(&self) -> reedline::Color {
        reedline::Color::Reset
    }
}

struct LexerHighlighter;

impl Highlighter for LexerHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        let (tokens, _) = lex(line);
        let mut offset = 0;
        for token in tokens {
            let end = offset + usize::from(token.len);
            styled.push((token_style(token.kind), line[offset..end].to_string()));
            offset = end;
        }
        if styled.buffer.is_empty() {
            styled.push((Style::new(), String::new()));
        }
        styled
    }
}

fn token_style(kind: SyntaxKind) -> Style {
    match kind {
        FUNCTION_KW | IF_KW | ELSE_KW | FOR_KW | WHILE_KW | REPEAT_KW | BREAK_KW | NEXT_KW
        | IN_KW => Style::new().fg(Color::Blue).bold(),
        TRUE_KW | FALSE_KW | NULL_KW | NA_KW | NA_INTEGER_KW | NA_REAL_KW | NA_COMPLEX_KW
        | NA_CHARACTER_KW | INF_KW | NAN_KW => Style::new().fg(Color::Purple),
        STRING | RAW_STRING => Style::new().fg(Color::Green),
        INTEGER | DOUBLE | COMPLEX => Style::new().fg(Color::Cyan),
        COMMENT | ANNOTATION_MARKER => Style::new().fg(Color::DarkGray),
        ERROR_TOKEN => Style::new().fg(Color::Red).underline(),
        _ => Style::new(),
    }
}

struct LexerValidator;

impl Validator for LexerValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if input_is_complete(line) {
            ValidationResult::Complete
        } else {
            ValidationResult::Incomplete
        }
    }
}

/// Incomplete only when provably unfinished; a false "complete" is safe because
/// R's parser asks for continuation itself.
fn input_is_complete(text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let (tokens, errors) = lex(text);
    let length = TextSize::of(text);
    if errors
        .iter()
        .any(|error| error.range.end() == length && !error.range.is_empty())
    {
        return false;
    }
    let mut depth: i64 = 0;
    let mut last_significant: Option<SyntaxKind> = None;
    for token in &tokens {
        match token.kind {
            L_PAREN | L_BRACE | L_BRACKET | L_BRACKET2 => depth += 1,
            R_PAREN | R_BRACE | R_BRACKET => depth -= 1,
            _ => {}
        }
        if !matches!(token.kind, WHITESPACE | NEWLINE | COMMENT) {
            last_significant = Some(token.kind);
        }
    }
    if depth > 0 {
        return false;
    }
    !matches!(
        last_significant,
        Some(
            PLUS | MINUS
                | STAR
                | SLASH
                | CARET
                | SPECIAL
                | PIPE_GREATER
                | COLON
                | LESS
                | GREATER
                | LESS_EQ
                | GREATER_EQ
                | EQ2
                | BANG_EQ
                | AMP
                | AMP2
                | PIPE
                | PIPE2
                | TILDE
                | QUESTION
                | BANG
                | LESS_MINUS
                | LESS2_MINUS
                | EQ
                | MINUS_GREATER
                | MINUS_GREATER2
                | COLON_EQ
                | COMMA
                | DOLLAR
                | AT
                | COLON2
                | COLON3
        )
    )
}

#[cfg(test)]
mod tests {
    use super::input_is_complete;

    #[test]
    fn complete_inputs() {
        for text in [
            "1 + 1",
            "f <- function(x) x",
            "f <- function(x) {\n  x\n}",
            "plot(cars)",
            "x[[1]]",
            "",
            "  ",
            "# just a comment",
            "return x", // broken, but FINISHED, so R gets to report it
            "]",        // stray closer: R's error, not an endless prompt
        ] {
            assert!(input_is_complete(text), "expected complete: {text:?}");
        }
    }

    #[test]
    fn incomplete_inputs() {
        for text in [
            "1 +",
            "f <- function(x) {",
            "plot(",
            "x[[1",
            "x <-",
            "a |>",
            "list(a = 1,",
            "x$",
            "dplyr::",
            "\"unterminated",
        ] {
            assert!(!input_is_complete(text), "expected incomplete: {text:?}");
        }
    }
}
