//! The console's socket, through which an editor reaches a running `ry run` console: it attaches
//! the debugger, reads the global environment for completion and hover, and opens data views.
//!
//! The console listens on a Unix socket in a directory only its user can enter, and announces
//! itself with a registry file beside it that names its process, working directory and socket, so
//! an editor finds the console started in its workspace. Messages are DAP-framed JSON
//! (`Content-Length` headers). Each connection is either the debugger's (it sent `initialize`; at
//! most one at a time) or a plain one that sends ry's own requests: `ryGlobals`, `ryView`,
//! `ryTable`, `ryRelease`, and `rySubscribe` for the `ryView` events `View()` sends.
//!
//! Every request is answered on the interpreter's thread, because R values live there. Socket
//! threads only parse messages and queue them. Input is read on an input thread, which reads a
//! line whenever the console asks for one, so the interpreter answers requests while it waits.
//! While a command runs, a request sets a flag the evaluator polls with its interrupts, and the
//! poll answers the debugger's requests (`pause`, breakpoints) but refuses the others as busy,
//! because they run R code in the middle of the user's. While the debugger has the program
//! stopped, its transport answers everything, since the program is then at a statement boundary.
//!
//! `browser()` with no editor attached stops at a `Browse[1]>` prompt, which drives the same
//! debug adapter through [`Browse`], a transport that turns typed commands into requests. An
//! editor that attaches during that prompt takes the stopped program over.

use {
    crate::debugger::{Adapter, Request, Transport, response},
    serde_json::{Value, json},
    std::{
        collections::VecDeque,
        path::Path,
        sync::{
            Mutex, MutexGuard, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
    },
};

/// Set by a socket thread with [`crate::unwind::INTERRUPT_REQUESTED`], so the evaluator's
/// interrupt poll (one load on its fast path) also notices requests.
pub static SERVICE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// The console's line reader, which shows a prompt and returns the line typed, or `None` at the
/// end of the input.
pub type Reader = Box<dyn FnMut(&str) -> Option<String> + Send>;

/// The reader until the input thread takes it, or for good when there is no socket to serve.
static READER: Mutex<Option<Reader>> = Mutex::new(None);
/// Whether an interactive console installed a reader, which is what lets `browser()` stop.
static INTERACTIVE: AtomicBool = AtomicBool::new(false);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Makes `reader` the console's source of input. Called once, before the console's first prompt.
pub fn install_reader(reader: Reader) {
    *lock(&READER) = Some(reader);
    INTERACTIVE.store(true, Ordering::Relaxed);
}

/// Whether input comes from an interactive console, so `browser()` has a prompt to stop at.
pub fn interactive() -> bool {
    INTERACTIVE.load(Ordering::Relaxed) && !SERVING_REQUEST.load(Ordering::Relaxed)
}

/// Set while R code runs for an editor's request at the idle prompt. A `browser()` there has no
/// prompt of its own: the top-level read owns the terminal, and the editor waits for its answer.
static SERVING_REQUEST: AtomicBool = AtomicBool::new(false);

/// Reads with the reader on this thread, for a console with no socket.
fn read_directly(prompt: &str) -> Option<String> {
    lock(&READER).as_mut().and_then(|read| read(prompt))
}

/// Starts listening, unless the console cannot make a private socket directory, in which case
/// the console runs without one. Returns the socket's path.
pub fn start() -> Option<String> {
    platform::start()
}

/// Removes the socket and the registry file. The console calls it as the session ends.
pub fn finish() {
    platform::finish();
}

/// Reads the console's next input after `prompt`, answering requests until it arrives.
///
/// # Safety
/// Called on the interpreter's thread, at the top level of the console.
pub unsafe fn read_line(prompt: &str) -> Option<String> {
    // A command a client terminated unwinds at its next statement, and one that ended first
    // must not take the stale adapter into the next command. A `Browse>` prompt's adapter ends
    // with the command it stopped.
    let state = crate::interp();
    if state
        .debugger
        .as_ref()
        .is_some_and(|adapter| adapter.is_detached() || adapter.is_browse())
    {
        state.debugger = None;
    }
    // A command may have changed a viewed object in place, so its sorted and filtered rows are
    // derived again on the next page.
    crate::viewer::forget_orders();
    platform::read_line(prompt)
}

/// What the `Browse>` prompt received: a line, or an editor's `initialize`, with which the
/// editor takes the stopped program over through the transport that comes with it.
pub enum BrowseInput {
    Line(Option<String>),
    Attach(Box<dyn Transport>, Value),
}

/// Wakes the console's wait for input, so it answers the help server's queued requests.
pub fn wake() {
    platform::wake();
}

/// Answers the requests queued while a command runs. Called from the evaluator's interrupt poll.
pub fn service() {
    // SAFETY: the poll runs on the interpreter's thread.
    unsafe { platform::service() }
}

/// Sends a `ryView` event with a new view of `object` to every subscribed connection.
///
/// # Safety
/// Called on the interpreter's thread.
pub unsafe fn show(object: heap::object::Sexp, title: &str) -> Result<(), String> {
    platform::show(object, title)
}

/// Requests any connection may send, answered with R code allowed to run.
unsafe fn answer_plain(request: &Request) -> Result<Value, String> {
    match request.command.as_str() {
        "ryGlobals" => {
            let mark = crate::heap().handle_mark();
            let globals = crate::viewer::globals();
            crate::heap().release_handles(mark);
            Ok(globals)
        }
        "ryView" => {
            let expression = request
                .arguments
                .get("expression")
                .and_then(Value::as_str)
                .ok_or("the request needs an `expression`")?;
            let mark = crate::heap().handle_mark();
            let opened = crate::debugger::evaluate_text(expression, crate::values::global_env())
                .and_then(|value| crate::viewer::open(value, expression));
            crate::heap().release_handles(mark);
            opened
        }
        "ryTable" | "ryRelease" => crate::viewer::request(&request.command, &request.arguments),
        command => Err(format!("`{command}` is not supported")),
    }
}

/// The terminal's side of a `browser()` stop with no editor attached. GNU R's browser commands
/// become debug adapter requests, so stepping follows the same rules as in an editor: `c`
/// continues (the adapter then leaves, as an editor that disconnects), `n`, `s` and `f` step
/// over, into and out of calls, `Q` ends the command, `where` shows the stack, and any other
/// line is evaluated in the stopped frame and printed as at the top level. An empty line steps,
/// as in GNU R.
#[derive(Default)]
pub struct Browse {
    editor: Option<Box<dyn Transport>>,
    /// The line a queued `source` request asks for, which its response does not echo.
    text_line: Option<u64>,
    /// Requests the transport makes itself to describe a stop, answered before the next prompt.
    queued: VecDeque<Value>,
}

/// The `seq`s of the requests [`Browse`] makes itself, far above any a client numbers.
const LOCATION: u64 = u64::MAX - 1;
const STACK: u64 = u64::MAX - 2;
const TEXT: u64 = u64::MAX - 3;

impl Browse {
    fn request(seq: u64, command: &str, arguments: Value) -> Value {
        json!({ "type": "request", "seq": seq, "command": command, "arguments": arguments })
    }

    /// The request a typed line stands for, or `None` for a line the prompt answers itself.
    fn command(line: &str) -> Option<Value> {
        let (command, arguments) = match line.trim() {
            "c" | "cont" => ("disconnect", json!({})),
            "" | "n" => ("next", json!({ "threadId": 1 })),
            "s" => ("stepIn", json!({ "threadId": 1 })),
            "f" => ("stepOut", json!({ "threadId": 1 })),
            "Q" => ("terminate", json!({})),
            "where" => {
                return Some(Browse::request(
                    STACK,
                    "stackTrace",
                    json!({ "threadId": 1 }),
                ));
            }
            "help" => {
                crate::write_console(
                    b"n          next\ns          step into\nf          finish\nc or cont  continue\nQ          quit\nwhere      show stack\nhelp       show help\n<expr>     evaluate expression\n",
                    false,
                );
                return None;
            }
            expression => (
                "evaluate",
                json!({ "expression": expression, "frameId": 1, "context": "ryBrowse" }),
            ),
        };
        Some(Browse::request(0, command, arguments))
    }

    fn render(&mut self, message: &Value) {
        let body = &message["body"];
        if message["type"] == "event" {
            match message["event"].as_str() {
                Some("stopped") => self.queued.push_back(Browse::request(
                    LOCATION,
                    "stackTrace",
                    json!({ "threadId": 1, "levels": 1 }),
                )),
                Some("output") => {
                    let error = body["category"] == "stderr";
                    let text = body["output"].as_str().unwrap_or_default();
                    crate::write_console(text.as_bytes(), error);
                }
                _ => {}
            }
            return;
        }
        if message["success"] == false {
            // An evaluation's message is the error as R prints it, `Error in ...` included.
            let text = message["message"]
                .as_str()
                .unwrap_or("Error: the command failed");
            let text = match text.starts_with("Error") {
                true => text.trim_end().to_owned(),
                false => format!("Error: {}", text.trim_end()),
            };
            crate::write_console(format!("{text}\n").as_bytes(), true);
            return;
        }
        let frames = body["stackFrames"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        match message["request_seq"].as_u64() {
            Some(LOCATION) => {
                let Some(frame) = frames.first() else { return };
                let line = frame["line"].as_u64().unwrap_or(0);
                if line == 0 {
                    // Code with no source, such as a function built by `eval(parse(...))`.
                    let name = frame["name"].as_str().unwrap_or("?");
                    crate::write_console(format!("Called from: {name}\n").as_bytes(), false);
                    return;
                }
                let path = frame["source"]["path"].as_str();
                let text =
                    path.and_then(|path| source_line(&std::fs::read_to_string(path).ok()?, line));
                match (path, frame["source"]["sourceReference"].as_u64()) {
                    (None, Some(reference)) => self.queued.push_back(Browse::request(
                        TEXT,
                        "source",
                        json!({ "sourceReference": reference, "line": line }),
                    )),
                    _ => announce(path.unwrap_or_default(), line, text.as_deref()),
                }
            }
            Some(TEXT) => {
                let content = body["content"].as_str().unwrap_or_default();
                let line = self.text_line.take().unwrap_or(0);
                announce("", line, source_line(content, line).as_deref());
            }
            Some(STACK) => {
                for (index, frame) in frames.iter().enumerate() {
                    let name = frame["name"].as_str().unwrap_or("?");
                    let place = match frame["source"]["path"].as_str().map(Path::new) {
                        Some(path) => {
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            format!(" at {name}#{}", frame["line"])
                        }
                        None => String::new(),
                    };
                    crate::write_console(
                        format!("where {}{place}: {name}\n", index + 1).as_bytes(),
                        false,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Line `line` (1-based) of `text`, without its line break.
fn source_line(text: &str, line: u64) -> Option<String> {
    let index = usize::try_from(line.checked_sub(1)?).ok()?;
    Some(text.lines().nth(index)?.trim().to_owned())
}

/// Tells the user where the program stopped, as GNU R's browser does.
fn announce(path: &str, line: u64, text: Option<&str>) {
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let code = text.map(|text| format!(": {text}")).unwrap_or_default();
    crate::write_console(format!("debug at {name}#{line}{code}\n").as_bytes(), false);
}

impl Transport for Browse {
    fn receive(&mut self, wait: bool) -> Option<Value> {
        if let Some(editor) = &mut self.editor {
            return editor.receive(wait);
        }
        if let Some(request) = self.queued.pop_front() {
            if request["seq"] == TEXT {
                self.text_line = request["arguments"]["line"].as_u64();
            }
            return Some(request);
        }
        // Between statements the terminal has nothing to say; only a stop prompts.
        if !wait {
            return None;
        }
        loop {
            // SAFETY: the adapter receives on the interpreter's thread.
            match unsafe { platform::browse_input("Browse[1]> ") } {
                BrowseInput::Attach(editor, message) => {
                    crate::write_console(
                        b"\nry: an editor attached and drives this stop now\n",
                        false,
                    );
                    self.editor = Some(editor);
                    return Some(message);
                }
                BrowseInput::Line(None) => return None,
                BrowseInput::Line(Some(line)) => {
                    if let Some(request) = Browse::command(&line) {
                        return Some(request);
                    }
                }
            }
        }
    }

    fn send(&mut self, message: Value) {
        match &mut self.editor {
            Some(editor) => editor.send(message),
            None => self.render(&message),
        }
    }

    fn is_terminal(&self) -> bool {
        self.editor.is_none()
    }
}

#[cfg(unix)]
mod platform {
    use {
        super::{
            Adapter, BrowseInput, READER, Request, SERVICE_REQUESTED, SERVING_REQUEST, Transport,
            answer_plain, lock, read_directly, response,
        },
        crate::{help_server, interp, unwind::INTERRUPT_REQUESTED},
        serde_json::{Value, json},
        std::{
            collections::{BTreeMap, VecDeque},
            fs,
            io::{BufRead, BufReader, Read, Write},
            os::unix::{
                fs::{DirBuilderExt, MetadataExt, PermissionsExt},
                net::{UnixListener, UnixStream},
            },
            path::PathBuf,
            sync::{
                Condvar, Mutex, PoisonError,
                atomic::{AtomicBool, Ordering},
            },
            time::Duration,
        },
    };

    enum Event {
        Input(Option<String>),
        /// The help server queued a request, which it holds itself.
        Help,
        Request(u64, Value),
        Closed(u64),
    }

    struct Connection {
        stream: UnixStream,
        seq: u64,
        subscribed: bool,
    }

    struct Server {
        socket: PathBuf,
        registry: PathBuf,
    }

    static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
    static ARRIVED: Condvar = Condvar::new();
    /// Signalled when the interpreter takes an event, so a reader waiting for room can go on.
    static TAKEN: Condvar = Condvar::new();
    static CONNECTIONS: Mutex<BTreeMap<u64, Connection>> = Mutex::new(BTreeMap::new());
    static SERVER: Mutex<Option<Server>> = Mutex::new(None);
    /// The prompt's input, when it arrived while code run for a request was stopped.
    static STASHED_INPUT: Mutex<Option<Option<String>>> = Mutex::new(None);
    /// The connection that owns the debugger, while one is attached.
    static DEBUG_CONNECTION: Mutex<Option<u64>> = Mutex::new(None);

    /// What the console asked the input thread for: `prompt` is the line it should read next,
    /// and `outstanding` stays set from the request until the interpreter takes the line, so a
    /// prompt shown while an earlier one still waits does not read ahead.
    struct Asked {
        prompt: Option<String>,
        outstanding: bool,
        /// The prompt the input thread last showed.
        shown: String,
    }

    static ASKED: Mutex<Asked> = Mutex::new(Asked {
        prompt: None,
        outstanding: false,
        shown: String::new(),
    });
    static ASKED_CHANGED: Condvar = Condvar::new();
    static INPUT_THREAD: AtomicBool = AtomicBool::new(false);

    pub fn start() -> Option<String> {
        let directory = private_directory()?;
        let pid = std::process::id();
        let socket = directory.join(format!("console-{pid}.sock"));
        let registry = directory.join(format!("console-{pid}.json"));
        // A socket left by a crashed process with a reused pid would make `bind` fail.
        if socket.exists() {
            fs::remove_file(&socket).ok()?;
        }
        let listener = UnixListener::bind(&socket).ok()?;
        let cwd = std::env::current_dir().ok()?;
        let entry = json!({
            "pid": pid,
            "cwd": cwd.to_string_lossy(),
            "socket": socket.to_string_lossy(),
        });
        fs::write(&registry, entry.to_string()).ok()?;
        std::thread::Builder::new()
            .name("ry-console-accept".to_owned())
            .spawn(move || accept(listener))
            .ok()?;
        let path = socket.to_string_lossy().into_owned();
        *lock(&SERVER) = Some(Server { socket, registry });
        Some(path)
    }

    pub fn finish() {
        if let Some(server) = lock(&SERVER).take() {
            // The files only advertise this process; a failure to remove one leaves a stale entry
            // that editors skip once its socket refuses them.
            fs::remove_file(&server.socket).ok();
            fs::remove_file(&server.registry).ok();
        }
    }

    /// `$TMPDIR/ry-<uid>`, made with mode 0700. An existing directory is used only when this
    /// user owns it and no one else may enter it, because anyone who can reach the socket can
    /// run code in the console.
    fn private_directory() -> Option<PathBuf> {
        // SAFETY: `getuid` cannot fail.
        let uid = unsafe { libc::getuid() };
        let directory = std::env::temp_dir().join(format!("ry-{uid}"));
        if !directory.exists() {
            fs::DirBuilder::new().mode(0o700).create(&directory).ok()?;
        }
        let metadata = fs::symlink_metadata(&directory).ok()?;
        let private = metadata.is_dir()
            && metadata.uid() == uid
            && metadata.permissions().mode() & 0o077 == 0;
        private.then_some(directory)
    }

    fn accept(listener: UnixListener) {
        let mut next = 1;
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let Ok(writer) = stream.try_clone() else {
                continue;
            };
            let id = next;
            next += 1;
            lock(&CONNECTIONS).insert(
                id,
                Connection {
                    stream: writer,
                    seq: 1,
                    subscribed: false,
                },
            );
            let spawned = std::thread::Builder::new()
                .name("ry-console-connection".to_owned())
                .spawn(move || {
                    let mut reader = BufReader::new(stream);
                    while let Some(message) = read_message(&mut reader) {
                        push(Event::Request(id, message));
                    }
                    push(Event::Closed(id));
                });
            if spawned.is_err() {
                lock(&CONNECTIONS).remove(&id);
            }
        }
    }

    /// One `Content-Length`-framed JSON message, or `None` once the peer is gone or broke the
    /// framing.
    pub(super) fn read_message(reader: &mut impl BufRead) -> Option<Value> {
        let mut length = None;
        loop {
            let mut line = String::new();
            // A header line is a name and a number; reading a longer one whole would let a peer
            // that never sends a line break grow it without bound.
            let read = reader
                .by_ref()
                .take(MAX_HEADER_LINE as u64)
                .read_line(&mut line)
                .ok()?;
            if read == 0 || (read == MAX_HEADER_LINE && !line.ends_with('\n')) {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>().ok()?);
            }
        }
        // A message larger than this is not one an editor sends; refusing it bounds the
        // allocation a hostile peer can cause.
        let length = length.filter(|&length| length <= 16 << 20)?;
        // The body grows with the bytes that arrive, not with the length the header claims.
        let mut body = Vec::new();
        reader.take(length as u64).read_to_end(&mut body).ok()?;
        if body.len() < length {
            return None;
        }
        serde_json::from_slice(&body).ok()
    }

    const MAX_HEADER_LINE: usize = 1024;

    /// The requests one connection may have waiting before its reader stops reading. The peer
    /// then blocks on a full socket buffer, so a client that floods the console slows itself
    /// down instead of growing the queue, and no request is dropped unanswered.
    const QUEUED_PER_CONNECTION: usize = 64;

    pub fn wake() {
        // One token answers every queued request, and a script that never prompts would
        // otherwise collect one per request.
        let mut events = lock(&EVENTS);
        if !events.iter().any(|event| matches!(event, Event::Help)) {
            events.push_back(Event::Help);
            ARRIVED.notify_all();
        }
    }

    fn push(event: Event) {
        let poke = !matches!(event, Event::Input(_));
        let mut events = lock(&EVENTS);
        if let Event::Request(id, _) = event {
            while events
                .iter()
                .filter(|queued| matches!(queued, Event::Request(other, _) if *other == id))
                .count()
                >= QUEUED_PER_CONNECTION
            {
                events = TAKEN.wait(events).unwrap_or_else(PoisonError::into_inner);
            }
        }
        events.push_back(event);
        drop(events);
        ARRIVED.notify_all();
        if poke {
            SERVICE_REQUESTED.store(true, Ordering::Relaxed);
            INTERRUPT_REQUESTED.store(true, Ordering::Relaxed);
        }
    }

    fn pop(wait: bool) -> Option<Event> {
        let mut events = lock(&EVENTS);
        loop {
            if let Some(event) = events.pop_front() {
                TAKEN.notify_all();
                return Some(event);
            }
            if !wait {
                return None;
            }
            events = ARRIVED.wait(events).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn send(connection: u64, mut message: Value) {
        let mut connections = lock(&CONNECTIONS);
        let Some(target) = connections.get_mut(&connection) else {
            return;
        };
        message["seq"] = json!(target.seq);
        target.seq += 1;
        let body = message.to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        // A peer that stopped reading must not stall the console.
        target
            .stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok();
        if target.stream.write_all(framed.as_bytes()).is_err() {
            // Its reader thread reports the close; until then nothing more is sent to it.
            connections.remove(&connection);
        }
    }

    pub unsafe fn read_line(prompt: &str) -> Option<String> {
        match wait_for_input(prompt, false) {
            BrowseInput::Line(line) => line,
            BrowseInput::Attach(..) => None,
        }
    }

    pub unsafe fn browse_input(prompt: &str) -> BrowseInput {
        wait_for_input(prompt, true)
    }

    /// Answers requests until the line read after `prompt` arrives. At a `Browse[1]>` prompt
    /// (`browse`), an editor's `initialize` ends the wait instead, handing the stop over.
    unsafe fn wait_for_input(prompt: &str, browse: bool) -> BrowseInput {
        if lock(&SERVER).is_none() && !help_server::running() {
            return BrowseInput::Line(read_directly(prompt));
        }
        // Help requests that arrived while a command ran were left for the prompt.
        serve_help();
        ask(prompt);
        loop {
            let event = match lock(&STASHED_INPUT).take() {
                Some(input) => Some(Event::Input(input)),
                None => pop(true),
            };
            match event {
                Some(Event::Input(input)) => {
                    lock(&ASKED).outstanding = false;
                    // Requests answered while idle set the flag too; with nothing left to
                    // answer it would only cost the next command a spurious poll.
                    if SERVICE_REQUESTED.swap(false, Ordering::Relaxed) {
                        INTERRUPT_REQUESTED.store(false, Ordering::Relaxed);
                    }
                    return BrowseInput::Line(input);
                }
                Some(Event::Request(connection, message))
                    if browse
                        && message["command"] == "initialize"
                        && lock(&DEBUG_CONNECTION).is_none() =>
                {
                    *lock(&DEBUG_CONNECTION) = Some(connection);
                    return BrowseInput::Attach(Box::new(ConsoleTransport { connection }), message);
                }
                Some(Event::Request(connection, message)) => idle_request(connection, message),
                Some(Event::Closed(connection)) => closed(connection),
                Some(Event::Help) => serve_help(),
                None => {}
            }
        }
    }

    /// Answers help requests while waiting for input. A Ctrl-C that stopped a handler has
    /// nothing else to stop here, as at GNU R's prompt.
    fn serve_help() {
        // SAFETY: the console waits for input on the interpreter's thread, where GNU R's event
        // loop runs R handlers too.
        if let Err(unwind) = unsafe { help_server::serve_pending() } {
            drop(unwind);
        }
    }

    /// Asks the input thread for the line after `prompt`, unless a line is still owed from an
    /// earlier prompt. The thread is started on the first prompt and reads for the session's
    /// life, so the reader keeps one thread and a stop can hand its pending line to the next
    /// prompt.
    fn ask(prompt: &str) {
        let mut asked = lock(&ASKED);
        if asked.outstanding {
            // The line still owed was asked for at another prompt, such as a `Browse[1]>` stop
            // an editor took over; the terminal must show which prompt the line now answers.
            if asked.shown != prompt {
                asked.shown = prompt.to_owned();
                drop(asked);
                crate::write_console(format!("\n{prompt}").as_bytes(), false);
            }
            return;
        }
        asked.prompt = Some(prompt.to_owned());
        asked.outstanding = true;
        drop(asked);
        ASKED_CHANGED.notify_all();
        if INPUT_THREAD.swap(true, Ordering::Relaxed) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("ry-console-input".to_owned())
            .spawn(read_on_request);
        if spawned.is_err() {
            // Without the thread no line would ever arrive; the session sees the end of input.
            push(Event::Input(None));
        }
    }

    fn read_on_request() {
        let Some(mut read) = lock(&READER).take() else {
            push(Event::Input(None));
            return;
        };
        loop {
            let mut asked = lock(&ASKED);
            let prompt = loop {
                match asked.prompt.take() {
                    Some(prompt) => break prompt,
                    None => {
                        asked = ASKED_CHANGED
                            .wait(asked)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                }
            };
            asked.shown = prompt.clone();
            drop(asked);
            push(Event::Input(read(&prompt)));
        }
    }

    pub unsafe fn service() {
        // The adapter is out of the interpreter while it handles a statement; its own transport
        // drains the queue then.
        if lock(&DEBUG_CONNECTION).is_some() && interp().debugger.is_none() {
            return;
        }
        // R code run for one editor's request at the prompt is not the user's command: the
        // console is idle, and the prompt answers the other requests as soon as it returns, so
        // they stay queued. The debugger's requests and closed connections cannot wait for it,
        // since a `pause` or a `disconnect` is how a long evaluation is stopped.
        if SERVING_REQUEST.load(Ordering::Relaxed) {
            let debugger = *lock(&DEBUG_CONNECTION);
            let now = {
                let mut events = lock(&EVENTS);
                let (now, later): (VecDeque<Event>, VecDeque<Event>) = std::mem::take(&mut *events)
                    .into_iter()
                    .partition(|event| match event {
                        Event::Request(connection, _) => Some(*connection) == debugger,
                        Event::Closed(_) => true,
                        Event::Help | Event::Input(_) => false,
                    });
                *events = later;
                now
            };
            if !now.is_empty() {
                TAKEN.notify_all();
            }
            for event in now {
                match event {
                    Event::Request(connection, message) => busy_request(connection, message),
                    Event::Closed(connection) => closed(connection),
                    Event::Help | Event::Input(_) => {}
                }
            }
            return;
        }
        while let Some(event) = pop(false) {
            match event {
                Event::Request(connection, message) => busy_request(connection, message),
                Event::Closed(connection) => closed(connection),
                // GNU R's help server answers only while R waits, so the request stays queued
                // for the prompt.
                Event::Help => {}
                // A line typed while a command runs waits for the next prompt, and the requests
                // behind it are still answered.
                Event::Input(input) => *lock(&STASHED_INPUT) = Some(input),
            }
        }
    }

    pub unsafe fn show(object: heap::object::Sexp, title: &str) -> Result<(), String> {
        let subscribers: Vec<u64> = lock(&CONNECTIONS)
            .iter()
            .filter(|(_, connection)| connection.subscribed)
            .map(|(&id, _)| id)
            .collect();
        if subscribers.is_empty() {
            return Err(
                "View() needs an editor connected to this console: start the console with \
                 \"ry: Open Console\" in VS Code, or run \"ry: Attach to Console\" there"
                    .to_owned(),
            );
        }
        for connection in subscribers {
            let body = crate::viewer::open(object, title)?;
            send(
                connection,
                json!({ "type": "event", "event": "ryView", "body": body }),
            );
        }
        Ok(())
    }

    /// A request while the console waits for input: everything may be answered.
    unsafe fn idle_request(connection: u64, message: Value) {
        let Some(request) = Request::of(&message) else {
            return;
        };
        if request.command == "initialize" {
            initialize(connection, &request);
            return;
        }
        if *lock(&DEBUG_CONNECTION) == Some(connection) {
            // The packages are reloaded before `attach` is answered, so an editor that waits for
            // the answer finds the console idle again rather than busy with the reload.
            if request.command == "attach" {
                reload_packages();
            }
            debug_request(&request, false);
            return;
        }
        if request.command == "rySubscribe" {
            if let Some(target) = lock(&CONNECTIONS).get_mut(&connection) {
                target.subscribed = true;
            }
            send(connection, response(&request, Ok(json!({}))));
            return;
        }
        let serving = SERVING_REQUEST.swap(true, Ordering::Relaxed);
        let outcome = answer_plain(&request);
        SERVING_REQUEST.store(serving, Ordering::Relaxed);
        send(connection, response(&request, outcome));
    }

    /// A request while a command runs: the debugger's requests are answered, since they do not
    /// run R code; the others would interleave with the user's and are refused.
    unsafe fn busy_request(connection: u64, message: Value) {
        let Some(request) = Request::of(&message) else {
            return;
        };
        if request.command == "initialize" {
            initialize(connection, &request);
        } else if *lock(&DEBUG_CONNECTION) == Some(connection) {
            debug_request(&request, true);
        } else if request.command == "rySubscribe" {
            if let Some(target) = lock(&CONNECTIONS).get_mut(&connection) {
                target.subscribed = true;
            }
            send(connection, response(&request, Ok(json!({}))));
        } else {
            let busy = Err("the console is running a command".to_owned());
            send(connection, response(&request, busy));
        }
    }

    unsafe fn initialize(connection: u64, request: &Request) {
        let owner = lock(&DEBUG_CONNECTION);
        if owner.is_some_and(|owner| owner != connection) {
            let refused = Err("a debugger is already attached to this console".to_owned());
            drop(owner);
            send(connection, response(request, refused));
            return;
        }
        drop(owner);
        // An editor that initializes again replaces its adapter. The old adapter's transport
        // frees the connection as it drops, so it goes before the connection is claimed, or the
        // new adapter would run on a connection the console no longer counts as the debugger's.
        interp().debugger = None;
        *lock(&DEBUG_CONNECTION) = Some(connection);
        let transport = Box::new(ConsoleTransport { connection });
        interp().debugger = Some(Box::new(Adapter::for_console(transport)));
        debug_request(request, false);
    }

    /// `running` says a command is running, which a `terminate` must unwind: the adapter then
    /// stays until the command's next statement, where `with_adapter` unwinds and drops it.
    unsafe fn debug_request(request: &Request, running: bool) {
        let state = interp();
        let Some(adapter) = state.debugger.as_mut() else {
            return;
        };
        adapter.answer_running(request);
        if adapter.is_detached() && !(running && adapter.is_terminating()) {
            // Dropping the adapter drops its transport, which frees the debug connection.
            state.debugger = None;
        }
    }

    /// Reloads the packages the console loaded, so their functions get the source references a
    /// breakpoint needs. The loader keeps them only while a debugger is attached.
    unsafe fn reload_packages() {
        let code = "if (exists(\".ry_packages\")) for (root in ls(.ry_packages, all.names = TRUE)) \
                    .ry_load(root, .ry_packages[[root]]$export_all, attach = TRUE, full = TRUE, \
                    quiet = TRUE, report_noop = FALSE)";
        let mark = crate::heap().handle_mark();
        if let Err(message) = crate::debugger::evaluate_text(code, crate::values::global_env()) {
            crate::write_console(
                format!("ry: reloading packages for the debugger failed: {message}\n").as_bytes(),
                true,
            );
        }
        crate::heap().release_handles(mark);
    }

    unsafe fn closed(connection: u64) {
        lock(&CONNECTIONS).remove(&connection);
        if *lock(&DEBUG_CONNECTION) == Some(connection) {
            interp().debugger = None;
            *lock(&DEBUG_CONNECTION) = None;
        }
    }

    /// The attached debugger's side of its connection.
    struct ConsoleTransport {
        connection: u64,
    }

    impl Transport for ConsoleTransport {
        fn receive(&mut self, wait: bool) -> Option<Value> {
            loop {
                match pop(wait) {
                    None => return None,
                    Some(Event::Request(connection, message)) if connection == self.connection => {
                        return Some(message);
                    }
                    // The program is stopped at a statement or polls between them; other
                    // connections are answered in place when stopped, else refused as busy.
                    Some(Event::Request(connection, message)) => unsafe {
                        match wait {
                            true => idle_request(connection, message),
                            false => busy_request(connection, message),
                        }
                    },
                    Some(Event::Closed(connection)) if connection == self.connection => {
                        lock(&CONNECTIONS).remove(&connection);
                        return None;
                    }
                    Some(Event::Closed(connection)) => {
                        lock(&CONNECTIONS).remove(&connection);
                    }
                    // A stopped program waits like a prompt, so it answers the help server too.
                    Some(Event::Help) if wait => serve_help(),
                    Some(Event::Help) => {}
                    // A typed line waits for the next prompt: a stop must not end on it, or the
                    // debugger would take it for a hang-up, and between statements the editor's
                    // requests behind it must still be answered.
                    Some(Event::Input(input)) => *lock(&STASHED_INPUT) = Some(input),
                }
            }
        }

        fn send(&mut self, message: Value) {
            send(self.connection, message);
        }
    }

    impl Drop for ConsoleTransport {
        fn drop(&mut self) {
            let mut owner = lock(&DEBUG_CONNECTION);
            if *owner == Some(self.connection) {
                *owner = None;
            }
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use heap::object::Sexp;

    pub fn start() -> Option<String> {
        None
    }

    pub fn finish() {}

    pub fn wake() {}

    use super::{BrowseInput, read_directly};

    pub unsafe fn read_line(prompt: &str) -> Option<String> {
        read_directly(prompt)
    }

    pub unsafe fn browse_input(prompt: &str) -> BrowseInput {
        BrowseInput::Line(read_directly(prompt))
    }

    pub unsafe fn service() {}

    pub unsafe fn show(_: Sexp, _: &str) -> Result<(), String> {
        Err("View() needs an editor connected to the console, which needs a Unix system".to_owned())
    }
}

/// Parses DAP-framed messages from `bytes` until the framing ends, for the framing fuzz test.
#[cfg(unix)]
pub fn parse_messages(bytes: &[u8]) -> Vec<Value> {
    let mut reader = std::io::BufReader::new(bytes);
    std::iter::from_fn(|| platform::read_message(&mut reader)).collect()
}
