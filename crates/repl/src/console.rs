//! A `reedline` editor running inside R's `ReadConsole` callback; R itself still
//! parses, evaluates, and autoprints the fed input.

use crate::ReplError;
use crate::libr::{self, RApi};
use nu_ansi_term::{Color, Style};
use reedline::{
    ColumnarMenu, DefaultHinter, Emacs, FileBackedHistory, Highlighter, KeyCode, KeyModifiers,
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, StyledText, ValidationResult, Validator,
};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_uchar};
use std::io::Write;
use syntax::SyntaxKind;

pub fn run(api: RApi, options: crate::RunOptions) -> Result<(), ReplError> {
    let mut console = Console::new(options.keybindings, options.batch, options.completer);
    // The Windows callback wiring requires RGui mode, which sets `.Platform$GUI` to
    // "Rgui" and lets packages call Rgui-only functions that fail here. Startup
    // profiles run before this and still see "Rgui".
    #[cfg(windows)]
    console.pending.extend(
        b"invisible(local({ e <- baseenv(); locked <- bindingIsLocked(\".Platform\", e); \
          if (locked) unlockBinding(\".Platform\", e); \
          p <- get(\".Platform\", envir = e); p$GUI <- \"ry\"; \
          assign(\".Platform\", p, envir = e); \
          if (locked) lockBinding(\".Platform\", e) }))\n",
    );
    if let Some(path) = &options.file {
        let script = std::fs::read_to_string(path)
            .map_err(|error| ReplError(format!("cannot read {}: {error}", path.display())))?;
        let script = normalize_line_endings(&script);
        if options.batch {
            // A top-level error quits with a failing status, which becomes the exit code.
            console
                .pending
                .extend(b"options(error = function() q(status = 1, save = \"no\"))\n");
        }
        console.pending.extend(script.into_bytes());
        console.pending.push_back(b'\n');
    }
    let interactive = !options.batch;
    CONSOLE.with(|slot| {
        *slot.borrow_mut() = Some(console);
    });
    install_sigint_handler();
    api.initialize(read_console, write_console_ex)?;
    // R's `width` defaults to 80 whatever the terminal is, and R exports no setter.
    // Evaluated directly: fed input taken before the first prompt leaves the terminal
    // in R's state. Profiles have run, so only R's untouched default is replaced.
    if interactive
        && let Ok((columns, _)) = crossterm::terminal::size()
        && columns > 0
    {
        api.eval(&format!(
            "if (isTRUE(getOption(\"width\") == 80L)) options(width = {columns}L)"
        ));
    }
    if interactive {
        eprintln!(
            "ry R console, R at {}. Type q() or Ctrl-D to quit.",
            api.r_home.display()
        );
    }
    api.run_main_loop();
    Ok(())
}

// R calls the hooks only on its one thread and never nests read-console calls
// inside our borrow, so a thread-local `RefCell` suffices.
thread_local! {
    static CONSOLE: RefCell<Option<Console>> = const { RefCell::new(None) };
}

type SharedSessionCompleter = std::sync::Arc<std::sync::Mutex<Box<dyn crate::SessionCompleter>>>;

/// Shared because the console also feeds accepted lines to the editor's completer.
struct EditorCompleter(SharedSessionCompleter);

impl reedline::Completer for EditorCompleter {
    fn complete(&mut self, line: &str, position: usize) -> Vec<reedline::Suggestion> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
}

impl Console {
    fn new(
        keybindings: crate::Keybindings,
        batch: bool,
        completer: Option<Box<dyn crate::SessionCompleter>>,
    ) -> Console {
        let completion_menu_binding = ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_owned()),
            ReedlineEvent::MenuNext,
        ]);
        let edit_mode: Box<dyn reedline::EditMode> = match keybindings {
            crate::Keybindings::Emacs => {
                let mut bindings = reedline::default_emacs_keybindings();
                bindings.add_binding(KeyModifiers::NONE, KeyCode::Tab, completion_menu_binding);
                Box::new(Emacs::new(bindings))
            }
            crate::Keybindings::Vi => {
                let mut insert = reedline::default_vi_insert_keybindings();
                insert.add_binding(KeyModifiers::NONE, KeyCode::Tab, completion_menu_binding);
                Box::new(reedline::Vi::new(
                    insert,
                    reedline::default_vi_normal_keybindings(),
                ))
            }
        };
        let mut editor = Reedline::create()
            .with_highlighter(Box::new(LexerHighlighter))
            .with_hinter(Box::new(
                DefaultHinter::default().with_style(Style::new().fg(Color::DarkGray)),
            ))
            .with_validator(Box::new(LexerValidator))
            .with_edit_mode(edit_mode);
        let completer = completer.map(|completer| {
            std::sync::Arc::new(std::sync::Mutex::new(completer)) as SharedSessionCompleter
        });
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
        Console {
            editor,
            pending: VecDeque::new(),
            batch,
            completer,
        }
    }
}

/// R's parser rejects a raw `\r`, but scripts and the editor buffer carry CRLF on
/// Windows; CRLF and lone CR are line endings, as in R's text-mode connections.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn history_file() -> Option<std::path::PathBuf> {
    // Best-effort: on failure the editor keeps in-memory history.
    let data = dirs::data_dir()?;
    let directory = data.join("ry");
    // `roughly` is the project's former name; move its history rather than discard it.
    let previous = data.join("roughly");
    if !directory.exists() && previous.is_dir() {
        let _ = std::fs::rename(&previous, &directory);
    }
    std::fs::create_dir_all(&directory).ok()?;
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
    std::panic::catch_unwind(|| read_console_inner(prompt, buffer, length)).unwrap_or(0)
}

fn read_console_inner(prompt: *const c_char, buffer: *mut c_uchar, length: c_int) -> c_int {
    let _ = std::io::stdout().flush();
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
            let prompt_text = unsafe { libr::prompt_text(prompt) };
            match console.editor.read_line(&RPrompt { text: prompt_text }) {
                Ok(Signal::Success(line)) => {
                    let line = normalize_line_endings(&line);
                    if let Some(completer) = &console.completer {
                        completer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .accept(&line);
                    }
                    console.pending.extend(line.into_bytes());
                    console.pending.push_back(b'\n');
                }
                Ok(Signal::CtrlC) => continue,
                Ok(Signal::CtrlD) => return 0,
                Err(_) => return 0,
            }
        }
        let mut written = 0;
        while written < capacity {
            let Some(byte) = console.pending.pop_front() else {
                break;
            };
            unsafe { *buffer.add(written) = byte };
            written += 1;
        }
        unsafe { *buffer.add(written) = 0 };
        1
    })
}

/// The `R_WriteConsoleEx` hook: `output_type` 0 is regular output, anything else
/// the error stream. Unbuffered so output interleaves correctly with prompts.
extern "C" fn write_console_ex(text: *const c_char, length: c_int, output_type: c_int) {
    let _ = std::panic::catch_unwind(|| {
        if text.is_null() || length <= 0 {
            return;
        }
        let bytes = unsafe { std::slice::from_raw_parts(text as *const u8, length as usize) };
        if output_type == 0 {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(bytes);
            let _ = stdout.flush();
        } else {
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(bytes);
            let _ = stderr.flush();
        }
    });
}

/// While the editor runs, the terminal is raw (no signal); while R evaluates,
/// Ctrl-C arrives as SIGINT and raises R's cooperative interrupt flag.
#[cfg(unix)]
fn install_sigint_handler() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libr::sigint_to_r_flag as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
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

    fn right_prompt_on_last_line(&self) -> bool {
        false
    }
}

struct LexerHighlighter;

impl Highlighter for LexerHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        let (tokens, _) = syntax::lex(line);
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
    use SyntaxKind::*;
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
pub fn input_is_complete(text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let (tokens, errors) = syntax::lex(text);
    let length = syntax::TextSize::of(text);
    if errors
        .iter()
        .any(|error| error.range.end() == length && !error.range.is_empty())
    {
        return false;
    }
    let mut depth: i64 = 0;
    let mut last_significant: Option<SyntaxKind> = None;
    for token in &tokens {
        use SyntaxKind::*;
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
    use SyntaxKind::*;
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
