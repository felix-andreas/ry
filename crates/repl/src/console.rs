//! A `reedline` editor running inside R's `ReadConsole` callback; R itself still
//! parses, evaluates, and autoprints the fed input. The same editor drives ry's own runtime,
//! whose console loop calls the callback the same way.

use {
    crate::{
        ConsoleHooks, Filtered, InputFilter, Keybindings, ReplError, RunOptions, SessionCompleter,
        highlight::SyntaxHighlighter,
        libr::{self, CallRoutine, RApi, Sexp},
    },
    crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        terminal,
    },
    nu_ansi_term::{Color, Style},
    reedline::{
        ColumnarMenu, Completer, DefaultHinter, EditCommand, EditMode, Emacs, FileBackedHistory,
        HistoryItem, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
        PromptHistorySearchStatus, Reedline, ReedlineEvent, ReedlineMenu, Signal, Suggestion,
        ValidationResult, Validator, Vi, default_emacs_keybindings, default_vi_insert_keybindings,
        default_vi_normal_keybindings,
    },
    std::{
        borrow::Cow,
        collections::VecDeque,
        ffi::{c_char, c_int, c_uchar},
        fs,
        io::{BufRead, IsTerminal, Write, stderr, stdin, stdout},
        panic::catch_unwind,
        path::PathBuf,
        slice,
        sync::{
            Arc, Mutex, MutexGuard, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    },
    syntax::{input_is_unfinished, parse},
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
    // Without a palette the hint, the history search and the completion menu use attributes
    // only, so `NO_COLOR` leaves no color on screen.
    let colored = options.palette.is_some();
    let hint = match colored {
        true => Style::new().fg(Color::DarkGray),
        false => Style::new().dimmed(),
    };
    let mut menu = ColumnarMenu::default().with_name("completion_menu");
    if !colored {
        let plain = Style::new();
        menu = menu
            .with_text_style(plain)
            .with_selected_text_style(plain.reverse())
            .with_description_text_style(plain.dimmed())
            .with_match_text_style(plain.underline())
            .with_selected_match_text_style(plain.reverse().underline());
    }
    let mut editor = Reedline::create()
        .with_highlighter(Box::new(SyntaxHighlighter {
            palette: options.palette,
        }))
        .with_hinter(Box::new(DefaultHinter::default().with_style(hint)))
        .with_ansi_colors(colored)
        .with_validator(Box::new(LexerValidator))
        // A paste arrives as one insertion with one repaint, and runs on Enter as under
        // GNU R's readline, instead of a repaint and a terminal round trip per pasted line.
        .use_bracketed_paste(true)
        .with_edit_mode(edit_mode);
    let completer = options
        .completer
        .map(|completer| Arc::new(Mutex::new(completer)));
    if let Some(shared) = &completer {
        editor = editor
            .with_completer(Box::new(EditorCompleter(shared.clone())))
            .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)));
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
    {
        *lock_console() = Some(Console {
            unit_bytes: pending.len(),
            editor,
            pending,
            typeahead: Typeahead::default(),
            keys: KeyDecoder::default(),
            batch: options.batch,
            completer,
            hooks: options.hooks,
            filter,
            held: false,
            ry_runtime,
            piped: !stdin().is_terminal(),
            banner,
        });
    }
    Ok(())
}

// ry's console reads on a helper thread while the interpreter answers an editor's requests, so
// the console is not tied to the thread that installed it. Reads never nest.
static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

fn lock_console() -> MutexGuard<'static, Option<Console>> {
    CONSOLE.lock().unwrap_or_else(PoisonError::into_inner)
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
    typeahead: Typeahead,
    /// Decodes the bytes typed while the editor did not hold the terminal. It lives across
    /// reads, so a key or a CRLF split between two reads still decodes as one.
    keys: KeyDecoder,
    /// How many bytes at the front of `pending` are one unit, a hook's code or the input and
    /// script queued at start-up, which R receives in chunks of its own rather than line by line.
    /// ry's runtime parses a chunk whole, so a syntax error in the typed line would otherwise
    /// cancel the hook's code too and shift the line's error position, and a script read whole
    /// leaves nothing queued for its `readline()` to take.
    unit_bytes: usize,
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
    {
        let mut slot = lock_console();
        let Some(console) = slot.as_mut() else {
            return 0;
        };
        // Input for `readline()`, `menu()` or the browser must reach them untouched, and comes
        // from the console: `pending` holds the rest of a top-level script or paste, which runs
        // once they return, so it must not answer them.
        let at_top_level = console.ry_runtime || libr::at_top_level();
        // Ctrl-C while a command ran flushed the terminal's input, so it cancels the input
        // this console holds the same way, the rest of a paste included.
        if !console.piped && take_interrupt(console.ry_runtime) {
            console.pending.clear();
            console.unit_bytes = 0;
            console.typeahead.clear();
            console.keys.clear();
        }
        let mut answer: Option<Vec<u8>> = None;
        while if at_top_level {
            console.pending.is_empty()
        } else {
            answer.is_none()
        } {
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
            if !console.piped {
                if take_interrupt(console.ry_runtime) {
                    console.typeahead.clear();
                    console.keys.clear();
                }
                if !console.typeahead.has_line() {
                    match take_typed_line(&mut console.keys) {
                        Some(text) => console.typeahead.push(&text),
                        None => console.typeahead.clear(),
                    }
                }
            }
            let signal = if console.piped {
                // `R --no-echo` shows the prompt of `readline()` or `menu()` on a pipe too, and
                // gives its own prompts as empty. ry's runtime reads only at the top level.
                if !console.ry_runtime && !prompt_text.is_empty() && !console.held {
                    print!("{prompt_text}");
                    stdout().flush().ok();
                }
                Ok(read_piped_line())
            } else if let Some(line) = console.typeahead.pop_line() {
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
                let partial = console.typeahead.take_partial();
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
                        Some(text) => console.typeahead.push(&text),
                        None => console.typeahead.clear(),
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
                        console.unit_bytes = code.len() + 1;
                        console.pending.extend(code.into_bytes());
                        console.pending.push_back(b'\n');
                    }
                    if let Some(completer) = &console.completer {
                        completer
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .accept(&line);
                    }
                    if at_top_level {
                        console.pending.extend(line.into_bytes());
                        console.pending.push_back(b'\n');
                    } else {
                        let mut bytes = line.into_bytes();
                        bytes.push(b'\n');
                        answer = Some(bytes);
                    }
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
        // SAFETY: R's buffer holds `length` bytes, and `capacity` leaves room for the NUL.
        let chunk = unsafe { slice::from_raw_parts_mut(buffer, capacity) };
        let written = match answer {
            // R reads an answer as one line, so one longer than its buffer is cut short.
            Some(answer) => {
                let written = answer.len().min(chunk.len());
                chunk[..written].copy_from_slice(&answer[..written]);
                written
            }
            None => take_chunk(&mut console.pending, &mut console.unit_bytes, chunk),
        };
        unsafe { *buffer.add(written) = 0 };
        1
    }
}

/// Moves the next chunk of accepted input into `buffer` and returns its length. A unit, the
/// first `unit_bytes` of `pending`, goes in chunks of its own. Other input goes one line at a
/// time, as a typed line would: an error makes R discard the rest of its buffer, so a chunk cut
/// mid-line would leave the next chunk starting with a fragment of a line.
pub fn take_chunk(pending: &mut VecDeque<u8>, unit_bytes: &mut usize, buffer: &mut [u8]) -> usize {
    let mut written = buffer.len().min(pending.len());
    if *unit_bytes > 0 {
        written = written.min(*unit_bytes);
        *unit_bytes -= written;
    } else if let Some(newline) = pending.range(..written).position(|&byte| byte == b'\n') {
        written = newline + 1;
    }
    for (slot, byte) in buffer.iter_mut().zip(pending.drain(..written)) {
        *slot = byte;
    }
    written
}

/// Text typed ahead of the prompt, split into the lines that run at the coming prompts and the
/// unfinished rest.
#[derive(Debug, Default)]
pub struct Typeahead {
    lines: VecDeque<String>,
    /// The unfinished last line. It stays here, not in the editor, until the editor prompts, so
    /// text typed after it joins it instead of running ahead of it.
    partial: String,
    /// The last text ended in `\r`, so a `\n` that starts the next one completes that CRLF
    /// instead of ending an empty line.
    after_carriage_return: bool,
}

impl Typeahead {
    /// Adds typed text, which continues the unfinished line held so far.
    pub fn push(&mut self, text: &str) {
        let mut rest = text;
        if std::mem::take(&mut self.after_carriage_return) {
            rest = rest.strip_prefix('\n').unwrap_or(rest);
        }
        self.after_carriage_return = rest.ends_with('\r');
        while let Some(end) = rest.find(['\n', '\r']) {
            let mut line = std::mem::take(&mut self.partial);
            line.push_str(&rest[..end]);
            self.lines.push_back(line);
            let ending = if rest[end..].starts_with("\r\n") {
                2
            } else {
                1
            };
            rest = &rest[end + ending..];
        }
        self.partial.push_str(rest);
    }

    pub fn has_line(&self) -> bool {
        !self.lines.is_empty()
    }

    pub fn pop_line(&mut self) -> Option<String> {
        self.lines.pop_front()
    }

    /// Hands the unfinished line to the editor.
    pub fn take_partial(&mut self) -> String {
        std::mem::take(&mut self.partial)
    }

    pub fn clear(&mut self) {
        *self = Typeahead::default();
    }
}

/// Decodes the bytes a raw-mode terminal delivers into the line they type, keeping printable
/// text and dropping arrow and function keys whole.
#[derive(Debug, Default)]
pub struct KeyDecoder {
    line: Vec<u8>,
    escape: Escape,
    /// The last line ended at `\r`, so a `\n` right after it is the rest of that CRLF.
    after_carriage_return: bool,
}

/// What a byte did to the line a [`KeyDecoder`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedKey {
    /// The line continues.
    Pending,
    /// The line ended; [`KeyDecoder::take_line`] returns it with a final `\n`.
    LineEnd,
    /// Ctrl-C: the decoder dropped what it held.
    Interrupt,
}

impl KeyDecoder {
    pub fn push(&mut self, byte: u8) -> TypedKey {
        // A control byte cannot be part of a sequence, so a stray ESC never swallows a line end
        // or a Ctrl-C.
        if !matches!(self.escape, Escape::None) && byte < 0x20 {
            self.escape = Escape::None;
        }
        let after_carriage_return = std::mem::take(&mut self.after_carriage_return);
        self.escape = match (self.escape, byte) {
            // An arrow or function key pressed ahead of the prompt is dropped whole instead of
            // leaving `[A` in the line.
            (Escape::None, 0x1b) => Escape::Start,
            (Escape::Start, b'[') => Escape::Csi,
            (Escape::Start, b'O') => Escape::Ss3,
            (Escape::Start | Escape::Ss3, _) => Escape::None,
            (Escape::Csi, 0x40..=0x7e) => Escape::None,
            (Escape::Csi, _) => Escape::Csi,
            (Escape::None, b'\n') if after_carriage_return => Escape::None,
            (Escape::None, b'\n' | b'\r') => {
                self.line.push(b'\n');
                self.after_carriage_return = byte == b'\r';
                return TypedKey::LineEnd;
            }
            (Escape::None, 0x03) => {
                self.clear();
                return TypedKey::Interrupt;
            }
            (Escape::None, 0x7f | 0x08) => {
                while self.line.pop().is_some_and(|byte| byte & 0xc0 == 0x80) {}
                Escape::None
            }
            (Escape::None, b'\t' | 0x20..) => {
                self.line.push(byte);
                Escape::None
            }
            (Escape::None, _) => Escape::None,
        };
        TypedKey::Pending
    }

    /// The text decoded since the last call: a whole line ending in `\n`, or the unfinished one.
    pub fn take_line(&mut self) -> String {
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        line
    }

    pub fn clear(&mut self) {
        *self = KeyDecoder::default();
    }
}

/// Where a typed escape sequence has got to, so its bytes are dropped together.
#[derive(Debug, Clone, Copy, Default)]
enum Escape {
    #[default]
    None,
    Start,
    Csi,
    Ss3,
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
fn take_typed_line(keys: &mut KeyDecoder) -> Option<String> {
    if terminal::enable_raw_mode().is_err() {
        return Some(String::new());
    }
    while let Some(byte) = read_ready_byte() {
        match keys.push(byte) {
            TypedKey::Pending => {}
            TypedKey::LineEnd => break,
            TypedKey::Interrupt => {
                // The terminal's own Ctrl-C flushes its input; a Ctrl-C typed while it was raw
                // reaches here as a key instead, and cancels the same input.
                // SAFETY: `tcflush` only discards the terminal's pending input.
                unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
                return None;
            }
        }
    }
    Some(keys.take_line())
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
fn take_typed_line(_keys: &mut KeyDecoder) -> Option<String> {
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
static RUNTIME_SIGINT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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

/// Whether the editor submits `text` rather than open a continuation line: R's console rule,
/// so a command R would report as an error is submitted too, and R reports it.
pub fn input_is_complete(text: &str) -> bool {
    // R's lexer reads an unterminated quote to the end of the input, and R then asks for more
    // unless the code before the quote is already an error. The syntax crate's lexer cuts such
    // a quote at its line break instead, which suits an editor but not this question.
    let Some(start) = open_quote_start(text) else {
        return !input_is_unfinished(text);
    };
    let code = &text[..start];
    let has_error = parse(code)
        .errors()
        .iter()
        .any(|error| !error.in_annotation);
    has_error && !input_is_unfinished(code)
}

/// Where the string, backtick name or raw string still open at the end of `text` starts, read
/// as R's lexer reads it: comments and `%op%` operators hide quotes, and a backslash escapes
/// the character after it.
fn open_quote_start(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let start = index;
        index += 1;
        match byte {
            b'#' => {
                while bytes.get(index).is_some_and(|&byte| byte != b'\n') {
                    index += 1;
                }
            }
            b'%' => {
                while bytes
                    .get(index)
                    .is_some_and(|&byte| byte != b'%' && byte != b'\n')
                {
                    index += 1;
                }
                index += 1;
            }
            b'r' | b'R'
                if matches!(bytes.get(index), Some(b'"' | b'\''))
                    && !start
                        .checked_sub(1)
                        .and_then(|previous| bytes.get(previous))
                        .is_some_and(|&byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_')
                        }) =>
            {
                let quote = bytes[index];
                let mut cursor = index + 1;
                let dashes = bytes[cursor..]
                    .iter()
                    .take_while(|&&byte| byte == b'-')
                    .count();
                cursor += dashes;
                let close = match bytes.get(cursor) {
                    Some(b'(') => b')',
                    Some(b'[') => b']',
                    Some(b'{') => b'}',
                    // A malformed raw string is R's error, not an open quote.
                    _ => {
                        index = cursor;
                        continue;
                    }
                };
                let mut closer = vec![close];
                closer.extend(std::iter::repeat_n(b'-', dashes));
                closer.push(quote);
                match bytes[cursor..]
                    .windows(closer.len())
                    .position(|window| window == closer)
                {
                    Some(offset) => index = cursor + offset + closer.len(),
                    None => return Some(start),
                }
            }
            // An identifier can contain `r` followed by a quote only at its end, so skipping the
            // whole identifier keeps `bar"x"` from reading as a raw string.
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_') => {
                while bytes.get(index).is_some_and(|&byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_')
                }) {
                    index += 1;
                }
            }
            b'"' | b'\'' | b'`' => loop {
                match bytes.get(index) {
                    None => return Some(start),
                    Some(b'\\') => index += 2,
                    Some(&closing) if closing == byte => {
                        index += 1;
                        break;
                    }
                    Some(_) => index += 1,
                }
            },
            _ => {}
        }
    }
    None
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
            // R's errors, which the console once kept prompting for.
            "<-",
            "1,",
            ")((",
            "x <- \"abc\ndef\" , (",
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
            // R reads an open quote to the end of the input, across line breaks.
            "s <- \"line one\nline two",
            "x <- 'it''s",
            "r\"(raw\n",
            "`odd\nname",
            "# a ' quote\nf(\"a",
        ] {
            assert!(!input_is_complete(text), "expected incomplete: {text:?}");
        }
    }
}
