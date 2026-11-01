//! A `reedline` editor running inside R's `ReadConsole` callback; R itself still
//! parses, evaluates, and autoprints the fed input. The same editor drives ry's own runtime,
//! whose console loop calls the callback the same way.

use {
    crate::{
        ConsoleHooks, Filtered, InputFilter, Keybindings, ReplError, RunOptions, SessionCompleter,
        libr::{self, CallRoutine, RApi, Sexp},
    },
    crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        terminal,
    },
    nu_ansi_term::{Color, Style},
    reedline::{
        ColumnarMenu, Completer, DefaultHinter, EditCommand, EditMode, Emacs, FileBackedHistory,
        Highlighter, HistoryItem, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
        PromptHistorySearchStatus, Reedline, ReedlineEvent, ReedlineMenu, Signal, StyledText,
        Suggestion, ValidationResult, Validator, Vi, default_emacs_keybindings,
        default_vi_insert_keybindings, default_vi_normal_keybindings,
    },
    std::{
        borrow::Cow,
        cell::RefCell,
        collections::VecDeque,
        ffi::{c_char, c_int, c_uchar},
        fs,
        io::{BufRead, IsTerminal, Write, stderr, stdin, stdout},
        panic::catch_unwind,
        path::PathBuf,
        slice,
        sync::{
            Arc, Mutex, PoisonError,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    },
    syntax::{
        SyntaxKind::{self, *},
        TextSize, lex,
    },
};

pub fn run(api: RApi, mut options: RunOptions) -> Result<(), ReplError> {
    let batch = options.batch;
    let mut routines = std::mem::take(&mut options.routines);
    routines.push(CallRoutine {
        name: c"ry_console_interrupted",
        function: console_interrupted as *const (),
        arguments: 0,
    });
    let startup = std::mem::take(&mut options.startup);
    let arguments = std::mem::take(&mut options.arguments);
    install(options, false, None)?;
    install_sigint_handler();
    // A batch run prints only what its code prints, and so does a console reading a pipe.
    let quiet = batch || !stdin().is_terminal();
    api.initialize(read_console, write_console_ex, quiet, !batch, &arguments)?;
    api.register_call_routines(&routines);
    for statement in &startup {
        api.eval(statement);
    }
    if !batch {
        api.eval(INTERRUPT_HANDLER);
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
/// `banner` is printed when the console first prompts, once the session has started.
pub fn install_for_ry_runtime(
    options: RunOptions,
    banner: Option<String>,
) -> Result<(), ReplError> {
    install(options, true, banner)
}

/// The read hook ry's runtime calls: GNU R's `R_ReadConsole` without the history flag.
pub extern "C" fn ry_read_console(
    prompt: *const c_char,
    buffer: *mut c_uchar,
    length: c_int,
) -> c_int {
    catch_unwind(|| read_console_inner(prompt, buffer, length)).unwrap_or(0)
}

fn install(options: RunOptions, ry_runtime: bool, banner: Option<String>) -> Result<(), ReplError> {
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
            typeahead: VecDeque::new(),
            typeahead_partial: String::new(),
            hook_bytes: 0,
            batch: options.batch,
            completer,
            hooks: options.hooks,
            filter,
            held: false,
            ry_runtime,
            piped: !stdin().is_terminal(),
            banner,
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
    /// Lines typed while no prompt was up, run in order before the editor prompts again.
    typeahead: VecDeque<String>,
    /// The unfinished last typed-ahead line. It stays here, not in the editor, until the editor
    /// prompts, so text typed after it joins it instead of running ahead of it.
    typeahead_partial: String,
    /// How many bytes at the front of `pending` are a hook's code, which R must receive as a
    /// chunk of its own: ry's runtime parses a chunk whole, so a syntax error in the typed line
    /// would otherwise cancel the hook's code too, and shift the line's error position.
    hook_bytes: usize,
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
    /// Printed before the first prompt rather than before the session starts, so it appears
    /// only once the console can take input.
    banner: Option<String>,
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
            if let Some(banner) = console.banner.take() {
                eprintln!("{banner}");
            }
            // R hands its hook a valid NUL-terminated prompt (or null).
            let prompt_text = match console.held {
                true => "+ ".to_owned(),
                false => unsafe { libr::prompt_text(prompt) },
            };
            // Input for `readline()`, `menu()` or the browser must reach them untouched.
            let at_top_level = console.ry_runtime || libr::at_top_level();
            if !console.piped {
                // Ctrl-C while a command ran flushed the terminal's input, so it cancels the
                // input this console holds the same way.
                if take_interrupt(console.ry_runtime) {
                    console.typeahead.clear();
                    console.typeahead_partial.clear();
                }
                if console.typeahead.is_empty() {
                    match take_typed_line() {
                        Some(text) => queue_typeahead(console, &text),
                        None => console.typeahead_partial.clear(),
                    }
                }
            }
            let signal = if console.piped {
                Ok(read_piped_line())
            } else if let Some(line) = console.typeahead.pop_front() {
                // R prints its output in cooked mode, where `\n` returns the carriage. A failure
                // leaves output misaligned and nothing to report it to.
                terminal::disable_raw_mode().ok();
                // The terminal echoed the line where the cursor was when it was typed; showing
                // it again at its prompt keeps the transcript in the order things ran.
                println!("{prompt_text}{line}");
                stdout().flush().ok();
                // History is a convenience: a line that fails to save still runs.
                console
                    .editor
                    .history_mut()
                    .save(HistoryItem::from_command_line(line.clone()))
                    .ok();
                Ok(Signal::Success(line))
            } else {
                let partial = std::mem::take(&mut console.typeahead_partial);
                if !partial.is_empty() {
                    console
                        .editor
                        .run_edit_commands(&[EditCommand::InsertString(partial)]);
                }
                let signal = console.editor.read_line(&RPrompt { text: prompt_text });
                // Keys that arrived in the same read as Enter were typed before the command
                // runs, so they run after it.
                if matches!(signal, Ok(Signal::Success(_))) {
                    match take_editor_events() {
                        Some(text) => queue_typeahead(console, &text),
                        None => {
                            console.typeahead.clear();
                            console.typeahead_partial.clear();
                        }
                    }
                }
                signal
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
                        console.hook_bytes = code.len() + 1;
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
        let mut written = capacity.min(console.pending.len());
        if console.hook_bytes > 0 {
            written = written.min(console.hook_bytes);
            console.hook_bytes -= written;
        }
        for (index, byte) in console.pending.drain(..written).enumerate() {
            unsafe { *buffer.add(index) = byte };
        }
        unsafe { *buffer.add(written) = 0 };
        1
    })
}

/// Splits typed-ahead text into the lines that run at the coming prompts and the unfinished
/// rest, which joins the unfinished line already held.
fn queue_typeahead(console: &mut Console, text: &str) {
    let text = std::mem::take(&mut console.typeahead_partial) + text;
    // A pasted `\r\n` ends one line, not two.
    let mut lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split(['\n', '\r'])
        .map(str::to_owned)
        .collect();
    console.typeahead_partial = lines.pop().unwrap_or_default();
    console.typeahead.extend(lines);
}

/// Takes one line typed while the editor did not hold the terminal, or the unfinished line if
/// no line is complete, and `None` for a Ctrl-C among the keys. The terminal keeps such
/// typeahead in its cooked-mode buffer, where each Enter has already become `\n`; the editor
/// reads in raw mode, where `\n` is Ctrl-J, so left to the editor every typed-ahead line would
/// be lost. Raw mode also releases an unfinished line, which cooked mode withholds until Enter.
///
/// It reads the descriptor a byte at a time and stops at the first line end, because the lines
/// after it may be input for the command it starts: `readLines(file("stdin"))` or a `system()`
/// child reads them from the same descriptor, as under GNU R's readline. The terminal stays
/// raw, as the editor needs it: in cooked mode, a line typed before the editor took over would
/// end in `\n` again, and the editor submits only the first line of a batch that arrives
/// together.
#[cfg(unix)]
fn take_typed_line() -> Option<String> {
    if terminal::enable_raw_mode().is_err() {
        return Some(String::new());
    }
    let mut line = Vec::new();
    let mut escape = Escape::None;
    while let Some(byte) = read_ready_byte() {
        // A control byte cannot be part of a sequence, so a stray ESC never swallows a line end
        // or a Ctrl-C.
        if matches!(escape, Escape::Csi) && byte < 0x20 {
            escape = Escape::None;
        }
        escape = match (escape, byte) {
            // An arrow or function key pressed ahead of the prompt is dropped whole instead of
            // leaving `[A` in the line.
            (Escape::None, 0x1b) => Escape::Start,
            (Escape::Start, b'[') => Escape::Csi,
            (Escape::Start, b'O') => Escape::Ss3,
            (Escape::Start | Escape::Ss3, _) => Escape::None,
            (Escape::Csi, 0x40..=0x7e) => Escape::None,
            (Escape::Csi, _) => Escape::Csi,
            (Escape::None, b'\n' | b'\r') => {
                line.push(b'\n');
                break;
            }
            (Escape::None, 0x03) => {
                // The terminal's own Ctrl-C flushes its input; a Ctrl-C typed while it was raw
                // reaches here as a key instead, and cancels the same input.
                // SAFETY: `tcflush` only discards the terminal's pending input.
                unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
                return None;
            }
            (Escape::None, 0x7f | 0x08) => {
                while line.pop().is_some_and(|byte| byte & 0xc0 == 0x80) {}
                Escape::None
            }
            (Escape::None, b'\t' | 0x20..) => {
                line.push(byte);
                Escape::None
            }
            (Escape::None, _) => Escape::None,
        };
    }
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// Where a typed escape sequence has got to, so its bytes are dropped together.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum Escape {
    None,
    Start,
    Csi,
    Ss3,
}

/// One byte the terminal already holds, without waiting for more.
#[cfg(unix)]
fn read_ready_byte() -> Option<u8> {
    let mut ready = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut byte = 0u8;
    // SAFETY: `poll` reads one valid `pollfd`, and `read` writes at most one byte into `byte`.
    unsafe {
        (libc::poll(&mut ready, 1, 0) == 1
            && ready.revents & libc::POLLIN != 0
            && libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) == 1)
            .then_some(byte)
    }
}

/// Windows delivers keys as console events rather than bytes, so typeahead is taken whole.
#[cfg(not(unix))]
fn take_typed_line() -> Option<String> {
    if terminal::enable_raw_mode().is_err() {
        return Some(String::new());
    }
    take_editor_events()
}

/// Takes the keys left in crossterm's event queue, and `None` for a Ctrl-C among them. The
/// editor reads keys in batches and returns at the first Enter, leaving the keys that followed
/// it in the queue; they were typed before anything still in the descriptor, so they must be
/// taken before the descriptor is read again. The reader also parses escape sequences, so an
/// arrow key is dropped whole.
fn take_editor_events() -> Option<String> {
    let mut text = String::new();
    while matches!(event::poll(Duration::ZERO), Ok(true)) {
        let Ok(event) = event::read() else {
            break;
        };
        match event {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            }) => match code {
                KeyCode::Enter => text.push('\n'),
                KeyCode::Tab => text.push('\t'),
                KeyCode::Char('j') if modifiers == KeyModifiers::CONTROL => text.push('\n'),
                KeyCode::Char('c') if modifiers == KeyModifiers::CONTROL => return None,
                KeyCode::Char(character) if (modifiers - KeyModifiers::SHIFT).is_empty() => {
                    text.push(character)
                }
                _ => {}
            },
            Event::Paste(pasted) => text.extend(pasted.chars().filter(|character| {
                !character.is_control() || matches!(character, '\t' | '\n' | '\r')
            })),
            _ => {}
        }
    }
    text.into()
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

/// Set when Ctrl-C interrupts a command: by GNU R's `interrupt` condition through
/// [`console_interrupted`], or by [`note_interrupt`] in front of ry's runtime's SIGINT handler.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// The calling handler that reports GNU R's interrupts. R swaps in its own SIGINT handler while
/// it waits (`Sys.sleep()`, a `system()` child), so only the condition sees every interrupt.
const INTERRUPT_HANDLER: &str = "if (exists(\"globalCallingHandlers\")) globalCallingHandlers(\
    interrupt = function(condition) .Call(\"ry_console_interrupted\", PACKAGE = \"(embedding)\"))";

extern "C" fn console_interrupted() -> Sexp {
    INTERRUPTED.store(true, Ordering::SeqCst);
    libr::null()
}

/// Reports whether Ctrl-C interrupted a command since the last call. On ry's runtime it also
/// keeps [`note_interrupt`] in front of the runtime's SIGINT handler, which the runtime installs
/// at its own point of startup, so the console chains to whichever one is in place when it
/// prompts.
#[cfg_attr(not(unix), expect(unused_variables))]
fn take_interrupt(ry_runtime: bool) -> bool {
    #[cfg(unix)]
    if ry_runtime {
        chain_sigint();
    }
    INTERRUPTED.swap(false, Ordering::SeqCst)
}

/// The SIGINT handler [`note_interrupt`] runs after its own store, as an address.
#[cfg(unix)]
static RUNTIME_SIGINT: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
fn chain_sigint() {
    use std::{mem, ptr};
    let ours = note_interrupt as *const () as usize;
    // SAFETY: `sigaction` reads and writes only the structs passed to it, and the handler it
    // installs performs atomic operations and calls the handler it replaced.
    unsafe {
        let mut current: libc::sigaction = mem::zeroed();
        if libc::sigaction(libc::SIGINT, ptr::null(), &mut current) == 0
            && current.sa_sigaction != ours
            && current.sa_sigaction != libc::SIG_DFL
            && current.sa_sigaction != libc::SIG_IGN
            && current.sa_flags & libc::SA_SIGINFO == 0
        {
            RUNTIME_SIGINT.store(current.sa_sigaction, Ordering::SeqCst);
            let mut action = current;
            action.sa_sigaction = ours;
            libc::sigaction(libc::SIGINT, &action, ptr::null_mut());
        }
    }
}

#[cfg(unix)]
extern "C" fn note_interrupt(signal: c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let handler = RUNTIME_SIGINT.load(Ordering::SeqCst);
    if handler != 0 {
        // SAFETY: the address came from `sigaction` for a handler without `SA_SIGINFO`, which
        // takes the signal number alone.
        let handler: extern "C" fn(c_int) = unsafe { std::mem::transmute(handler) };
        handler(signal);
    }
}

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
