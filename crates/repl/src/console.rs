//! The interactive console: a `reedline` editor living inside R's
//! `ReadConsole` callback, with ry's lexer driving syntax highlighting
//! and input-completeness (continuation prompts).
//!
//! Control flow: `run` initializes embedded R with our console hooks and
//! enters R's real main loop. Whenever R wants input it calls the
//! read-console hook, which runs the editor; finished input is fed back
//! through R's console buffer (chunked to the buffer size), and R parses,
//! evaluates, and autoprints exactly as a stock terminal session would.
//! Output comes back through the write-console hook.
//!
//! Completeness is decided CONSERVATIVELY from lexer facts (open
//! delimiters, a trailing infix operator, an unterminated token at end of
//! input). When the check wrongly says "complete", nothing breaks: R's own
//! parser detects the incompleteness and calls the hook again with its `+`
//! continuation prompt, which runs the editor again. The validator is the
//! interface layer, and R stays the authority.

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
    // Windows console setup runs in RGui mode (the callback wiring requires
    // it), which stamps `.Platform$GUI` as "Rgui". A package takes that as
    // license to call an Rgui-only GUI function, such as a menu or a dialog,
    // which fails here. Rebind the honest front-end name before any user input
    // runs. R sources the startup profiles before the first console read, so
    // profile code still sees "Rgui", which is an accepted gap.
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
            // Rscript-like halt semantics without any new C surface: a
            // top-level error quits the session with a failing status, which
            // becomes this process's exit code.
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
    // R's `width` defaults to 80 columns whatever the terminal is, and R
    // exports no setter, so a table that had room on screen still wrapped.
    // Evaluated here rather than fed through the console: input arriving that
    // way costs a main-loop round trip, and one taken before the editor's
    // first prompt leaves the terminal in the state R found it in.
    //
    // `initialize` has already sourced the startup profiles, so a profile that
    // chose a width of its own has run: only R's untouched default is
    // replaced. A resize is not tracked, because R has no setter to call from
    // a signal handler and the console is the only other way in.
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

// R owns exactly one thread and calls the hooks only there, so the console
// state is thread-local; the `RefCell` is never borrowed re-entrantly
// because R never nests read-console calls inside our borrow.
thread_local! {
    static CONSOLE: RefCell<Option<Console>> = const { RefCell::new(None) };
}

type SharedSessionCompleter = std::sync::Arc<std::sync::Mutex<Box<dyn crate::SessionCompleter>>>;

/// The editor owns its completer, and the console must also feed accepted lines
/// back into the same object, so both sides hold a shared handle.
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
    /// Bytes of accepted input R has not consumed yet: the read hook hands
    /// them over in buffer-sized chunks across successive calls.
    pending: VecDeque<u8>,
    /// In batch mode the session ends at end of input once `pending` is
    /// exhausted, rather than prompting. This is what drives `ry run`.
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

/// The console feed must carry only `\n`: a real terminal never sends `\r`,
/// and R's parser reports a raw one as an invalid token. Script files carry
/// CRLF on Windows, and so does the editor's multiline buffer there. Normalize
/// every path into the feed, treating a CRLF, and a classic lone CR, as line
/// endings exactly as R's own text-mode connections do.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn history_file() -> Option<std::path::PathBuf> {
    // Per-platform data directory (XDG on Linux, Application Support on
    // macOS, roaming AppData on Windows). Persistence is best-effort: on
    // failure the editor falls back to in-memory history for the session.
    let data = dirs::data_dir()?;
    let directory = data.join("ry");
    // The project's former name. Move the directory once rather than renaming
    // the path outright, which would silently discard existing history.
    let previous = data.join("roughly");
    if !directory.exists() && previous.is_dir() {
        let _ = std::fs::rename(&previous, &directory);
    }
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory.join("history.txt"))
}

/// The `R_ReadConsole` hook. Returns 1 with a chunk of input in `buffer`,
/// or 0 for end-of-session. Runs the editor when no accepted input is
/// pending; R's own prompt (top-level `>`, continuation `+`, `Browse[n]>`,
/// `readline()` questions) is displayed, so nested REPLs stay honest.
extern "C" fn read_console(
    prompt: *const c_char,
    buffer: *mut c_uchar,
    length: c_int,
    _add_to_history: c_int,
) -> c_int {
    // An unwind must never cross into R: any panic ends the session cleanly
    // as EOF instead.
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

/// The `R_WriteConsoleEx` hook: 0 is regular output, anything else the
/// error and message stream. It writes unbuffered, because prompt interleaving
/// beats throughput at a console.
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

/// Ctrl-C behaves differently in the two states. While the editor runs the
/// terminal is raw, so no signal arrives and reedline clears the line. While R
/// evaluates the terminal is restored, so Ctrl-C arrives as SIGINT and the
/// handler raises R's cooperative interrupt flag.
#[cfg(unix)]
fn install_sigint_handler() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libr::sigint_to_r_flag as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

/// Ctrl-C on Windows arrives through the console control handler; it raises
/// both of R's interrupt flags (`UserBreak` + the deferred
/// `R_interrupts_pending`), honored at R's next interrupt check.
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

/// No R session can start on other platforms (`libr::load` refuses), so
/// there is no evaluation phase whose Ctrl-C would need translating.
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
        // R's own prompt (`> `, `+ `, `Browse[1]> `) is the indicator, so
        // debugger and readline() prompts look exactly like stock R.
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

/// Syntax highlighting straight from ry's lexer. These are the same tokens the
/// whole language tool sees, so there is no second grammar.
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

/// The continuation gate: reedline keeps editing (with the `+ ` indicator)
/// while the input is provably unfinished.
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

/// Conservative completeness from lexer facts alone. Input is incomplete only
/// when it is provably unfinished. An open delimiter, a trailing infix operator
/// or comma, and a token the lexer flagged as broken at end of input, such as
/// an unterminated string, all prove it. A false "complete" verdict is safe,
/// because R's parser asks for continuation itself.
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
