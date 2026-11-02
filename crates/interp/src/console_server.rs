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
//! threads only parse messages and queue them. While the console waits for input, the input is
//! read on a helper thread and the interpreter answers requests as they come. While a command
//! runs, a request sets a flag the evaluator polls with its interrupts, and the poll answers the
//! debugger's requests (`pause`, breakpoints) but refuses the others as busy, because they run R
//! code in the middle of the user's. While the debugger has the program stopped, its transport
//! answers everything, since the program is then at a statement boundary.

use {
    crate::debugger::{Adapter, Request, Transport, response},
    serde_json::Value,
    std::sync::atomic::AtomicBool,
};

/// Set by a socket thread with [`crate::unwind::INTERRUPT_REQUESTED`], so the evaluator's
/// interrupt poll (one load on its fast path) also notices requests.
pub static SERVICE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Starts listening, unless the console cannot make a private socket directory, in which case
/// the console runs without one. Returns the socket's path.
pub fn start() -> Option<String> {
    platform::start()
}

/// Removes the socket and the registry file. The console calls it as the session ends.
pub fn finish() {
    platform::finish();
}

/// Reads the next input with `read` on a helper thread, and answers requests until it arrives.
///
/// # Safety
/// Called on the interpreter's thread, at the top level of the console.
pub unsafe fn read_while_serving(read: impl FnOnce() -> Option<String> + Send) -> Option<String> {
    platform::read_while_serving(read)
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

#[cfg(unix)]
mod platform {
    use {
        super::{Adapter, Request, SERVICE_REQUESTED, Transport, answer_plain, response},
        crate::{interp, unwind::INTERRUPT_REQUESTED},
        serde_json::{Value, json},
        std::{
            collections::{BTreeMap, VecDeque},
            fs,
            io::{BufRead, BufReader, Write},
            os::unix::{
                fs::{DirBuilderExt, MetadataExt, PermissionsExt},
                net::{UnixListener, UnixStream},
            },
            path::PathBuf,
            sync::{Condvar, Mutex, MutexGuard, PoisonError, atomic::Ordering},
            time::Duration,
        },
    };

    enum Event {
        Input(Option<String>),
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
    static CONNECTIONS: Mutex<BTreeMap<u64, Connection>> = Mutex::new(BTreeMap::new());
    static SERVER: Mutex<Option<Server>> = Mutex::new(None);
    /// The prompt's input, when it arrived while code run for a request was stopped.
    static STASHED_INPUT: Mutex<Option<Option<String>>> = Mutex::new(None);
    /// The connection that owns the debugger, while one is attached.
    static DEBUG_CONNECTION: Mutex<Option<u64>> = Mutex::new(None);

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

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
            if reader.read_line(&mut line).ok()? == 0 {
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
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    fn push(event: Event) {
        let poke = !matches!(event, Event::Input(_));
        lock(&EVENTS).push_back(event);
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

    pub unsafe fn read_while_serving(
        read: impl FnOnce() -> Option<String> + Send,
    ) -> Option<String> {
        if lock(&SERVER).is_none() {
            return read();
        }
        // A command a client terminated unwinds at its next statement; one that ended first must
        // not take the stale adapter into the next command.
        let state = interp();
        if state
            .debugger
            .as_ref()
            .is_some_and(|adapter| adapter.is_detached())
        {
            state.debugger = None;
        }
        std::thread::scope(|scope| {
            let reader = std::thread::Builder::new()
                .name("ry-console-input".to_owned())
                .spawn_scoped(scope, || push(Event::Input(read())));
            if reader.is_err() {
                return None;
            }
            loop {
                let event = match lock(&STASHED_INPUT).take() {
                    Some(input) => Some(Event::Input(input)),
                    None => pop(true),
                };
                match event {
                    Some(Event::Input(input)) => {
                        // Requests answered while idle set the flag too; with nothing left to
                        // answer it would only cost the next command a spurious poll.
                        if SERVICE_REQUESTED.swap(false, Ordering::Relaxed) {
                            INTERRUPT_REQUESTED.store(false, Ordering::Relaxed);
                        }
                        return input;
                    }
                    Some(Event::Request(connection, message)) => idle_request(connection, message),
                    Some(Event::Closed(connection)) => closed(connection),
                    None => {}
                }
            }
        })
    }

    pub unsafe fn service() {
        // The adapter is out of the interpreter while it handles a statement; its own transport
        // drains the queue then.
        if lock(&DEBUG_CONNECTION).is_some() && interp().debugger.is_none() {
            return;
        }
        while let Some(event) = pop(false) {
            match event {
                Event::Request(connection, message) => busy_request(connection, message),
                Event::Closed(connection) => closed(connection),
                Event::Input(input) => {
                    lock(&EVENTS).push_front(Event::Input(input));
                    return;
                }
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
            debug_request(&request, false);
            if request.command == "attach" {
                reload_packages();
            }
            return;
        }
        if request.command == "rySubscribe" {
            if let Some(target) = lock(&CONNECTIONS).get_mut(&connection) {
                target.subscribed = true;
            }
            send(connection, response(&request, Ok(json!({}))));
            return;
        }
        let outcome = answer_plain(&request);
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
        let mut owner = lock(&DEBUG_CONNECTION);
        if owner.is_some_and(|owner| owner != connection) {
            let refused = Err("a debugger is already attached to this console".to_owned());
            drop(owner);
            send(connection, response(request, refused));
            return;
        }
        *owner = Some(connection);
        drop(owner);
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
                    // A line typed while a request's code is stopped waits for the prompt; the
                    // stop itself must not end, or the debugger would take it for a hang-up.
                    Some(Event::Input(input)) if wait => *lock(&STASHED_INPUT) = Some(input),
                    Some(Event::Input(input)) => {
                        lock(&EVENTS).push_front(Event::Input(input));
                        return None;
                    }
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

    pub unsafe fn read_while_serving(
        read: impl FnOnce() -> Option<String> + Send,
    ) -> Option<String> {
        read()
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
