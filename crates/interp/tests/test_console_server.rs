//! The console's socket server (`interp::console_server`) driven in-process, without base R. A
//! thread runs the console's prompt loop on the interpreter, as `ry run` does, and a seeded
//! simulator plays the editors that connect to it: several connections at once, each sending
//! ry's plain requests (`ryGlobals`, `ryView`, `ryTable`, `ryRelease`, `rySubscribe`) and the
//! debugger's (`initialize`, `attach`, `disconnect`, `terminate`, ...) with valid and invalid
//! arguments, typing commands at the prompt, and leaving cleanly or in the middle of a request.
//!
//! Every request with a command must get exactly one well-formed answer, numbered in order on
//! its connection; one connection at a time owns the debugger and the others are refused; a
//! released or evicted view refuses its pages; `View()` reaches each subscribed connection once;
//! and every session leaves the console with no debugger attached. The client waits for each
//! answer before it sends the next request and for each command to finish before it goes on,
//! so a seed replays the same conversation. A failure prints the `FUZZ_SEED` command that
//! replays it, and `FUZZ_ITERS` sets how many seeds run.

#![cfg(unix)]

use {
    interp::{
        console_server,
        envir::env_symbols,
        eval, heap, interp, lower,
        testing::with_interpreter,
        values::{global_env, length},
        viewer,
    },
    serde_json::{Value, json},
    std::{
        collections::BTreeSet,
        io::{BufRead, BufReader, Read, Write},
        net::Shutdown,
        os::unix::net::UnixStream,
        path::PathBuf,
        sync::{
            Mutex, MutexGuard, OnceLock, PoisonError,
            mpsc::{self, Receiver, Sender},
        },
        time::Duration,
    },
    syntax::testing::{SplitMix64, for_each_seed},
};

/// The prompt line that tells the console loop to forget the last seed's globals and views and
/// report whether a debugger is still attached.
const RESET: &str = "\u{1}reset";

#[test]
fn simulated_editors_keep_the_console_protocol() {
    let console = console();
    for_each_seed(60, |seed| {
        let mut session = Session::new(&console, seed);
        session.play();
        session.finish();
    });
}

// A second `initialize` on the debugger's own connection replaced the adapter after the
// connection was claimed again, and the old adapter's transport freed the connection as it
// dropped. The console then answered that editor's debug requests as plain ones (`threads` was
// "not supported") and let another connection take the debugger (console seed 0).
#[test]
fn an_editor_that_initializes_twice_keeps_the_debugger() {
    let console = console();
    let mut session = Session::new(&console, 0);
    session.connect();
    session.connect();
    session.initialize(0);
    session.initialize(0);
    let answer = session.call(0, "threads", json!({}));
    assert_eq!(
        answer["success"], true,
        "the debugger lost `threads`: {answer}"
    );
    // The model expects the second editor to be refused.
    session.initialize(1);
    session.finish();
}

// The console answered `attach` before it reloaded the packages for the debugger, so a request
// sent right after the answer found it busy running the reload ("the console is running a
// command", console seed 211).
#[test]
fn a_request_after_attach_finds_the_console_idle() {
    let console = console();
    let mut session = Session::new(&console, 0);
    session.connect();
    session.connect();
    session.initialize(1);
    let answer = session.call(1, "attach", json!({}));
    assert_eq!(answer["success"], true, "attach failed: {answer}");
    let answer = session.call(0, "ryGlobals", json!({}));
    assert_eq!(answer["success"], true, "the console stayed busy: {answer}");
    session.finish();
}

// The evaluator's poll inside R code run for one editor's request answered the other editors'
// requests as if a command were running, so an editor asking while another's `ryView` evaluated
// was refused as busy at an idle prompt (console seed 455 at FUZZ_ITERS=2000).
#[test]
fn a_request_during_another_editors_view_is_answered() {
    let console = console();
    let mut session = Session::new(&console, 0);
    session.connect();
    session.connect();
    let slow = json!({
        "type": "request",
        "seq": 1,
        "command": "ryView",
        "arguments": { "expression": "{ for (i in 1:3000000) i + 1; 1:3 }" },
    });
    session.peers[0].seq = 1;
    write_framed(session.peers[0].reader.get_mut(), &slow);
    let answer = session.call(1, "ryGlobals", json!({}));
    assert_eq!(
        answer["success"], true,
        "the idle console was busy: {answer}"
    );
    let view = loop {
        let message = read_framed(&mut session.peers[0].reader).expect("`ryView` is answered");
        if message["type"] == "response" {
            break message;
        }
    };
    assert_eq!(view["success"], true, "the slow view failed: {view}");
    if let Some(id) = view.pointer("/body/viewId").and_then(Value::as_u64) {
        session.views.push(id);
    }
    session.peers[0].received = view["seq"].as_u64().unwrap_or_default();
    session.finish();
}

/// The console's prompt loop, run on a thread of its own that keeps the interpreter for the rest
/// of the process, and the client's ends of its input and of the results of what it ran. Tests
/// take turns with it.
struct Console {
    socket: PathBuf,
    lines: Sender<Option<String>>,
    outcomes: Receiver<Result<String, String>>,
}

fn console() -> MutexGuard<'static, Console> {
    static CONSOLE: OnceLock<Mutex<Console>> = OnceLock::new();
    CONSOLE
        .get_or_init(|| {
            let (lines, typed) = mpsc::channel::<Option<String>>();
            let (finished, outcomes) = mpsc::channel();
            let (listening, socket) = mpsc::channel();
            std::thread::Builder::new()
                .name("ry-console-prompt".to_owned())
                .spawn(move || {
                    with_interpreter(|| unsafe {
                        console_server::install_reader(Box::new(move |_| {
                            typed.recv().ok().flatten()
                        }));
                        listening
                            .send(console_server::start())
                            .expect("the test waits for the socket");
                        while let Some(line) = console_server::read_line("> ") {
                            if finished.send(run_line(&line)).is_err() {
                                break;
                            }
                        }
                    })
                })
                .expect("the console thread starts");
            let socket = socket
                .recv()
                .expect("the console thread reports its socket")
                .expect("the console listens");
            extern "C" fn remove_socket() {
                console_server::finish();
            }
            // The prompt loop never returns, so the socket and its registry entry are removed
            // as the test process exits.
            // SAFETY: registering an exit handler has no preconditions.
            unsafe { libc::atexit(remove_socket) };
            Mutex::new(Console {
                socket: PathBuf::from(socket),
                lines,
                outcomes,
            })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// One command typed at the prompt, evaluated at the top level as the console does.
unsafe fn run_line(line: &str) -> Result<String, String> {
    if line == RESET {
        // The interpreter is shared by every seed, and the client sees the global scope and the
        // view ids, so a seed replayed alone must find them as a fresh console has them.
        for symbol in unsafe { env_symbols(global_env(), true, false) } {
            unsafe { heap().remove_binding(global_env(), symbol) }
                .expect("a global binding can be removed");
        }
        viewer::reset();
        return match (interp().debugger.is_some(), interp().frames.is_empty()) {
            (true, _) => Err("a debugger is still attached".to_owned()),
            (_, false) => Err("frames were left behind".to_owned()),
            _ => Ok(String::new()),
        };
    }
    let mark = heap().handle_mark();
    let outcome = unsafe {
        lower::parse_text(line, "<console>").and_then(|expressions| {
            heap().root(expressions);
            let mut last = String::new();
            for index in 0..length(expressions) {
                let value = eval::eval(expressions.element(index), global_env())?;
                last = format!("{:?}", value.kind());
            }
            Ok(last)
        })
    };
    heap().release_handles(mark);
    outcome.map_err(|_| "the command failed".to_owned())
}

/// Commands the simulated user types.
const LINES: &[&str] = &[
    "x <- 1:20",
    "m <- matrix(1:6, 2)",
    "l <- list(a = 1, b = \"two\")",
    "x <- c(a = 1.5, b = NA)",
    "rm(x)",
    "y <- 3",
    ".ry_view(m, \"m\")",
    ".ry_view(l, \"l\")",
    ".ry_view(sum, \"sum\")",
    "stop(\"boom\")",
];

/// Expressions an editor asks to view.
const VIEW_EXPRESSIONS: &[&str] = &[
    "x",
    "m",
    "l",
    "y",
    "1:50",
    "letters",
    "sum",
    "missing_one",
    "(",
    "stop(\"no\")",
];

/// How many views the console keeps open before it releases the oldest.
const MAX_VIEWS: usize = 32;

struct Session<'a> {
    console: &'a Console,
    entropy: SplitMix64,
    peers: Vec<Peer>,
    next_peer: usize,
    /// The peer whose `initialize` made it the debugger's connection.
    owner: Option<usize>,
    /// How many connections the console will have closed once it handles every one this
    /// session ended, counting those of earlier sessions too.
    closed: u64,
    /// Every view opened, oldest first.
    views: Vec<u64>,
    released: BTreeSet<u64>,
}

struct Peer {
    name: usize,
    reader: BufReader<UnixStream>,
    seq: u64,
    /// The `seq` of the console's last message on this connection.
    received: u64,
    subscribed: bool,
    events: Vec<Value>,
}

impl Session<'_> {
    fn new(console: &Console, seed: u64) -> Session<'_> {
        Session {
            console,
            entropy: SplitMix64(seed),
            peers: Vec::new(),
            next_peer: 1,
            owner: None,
            closed: console_server::closed_connections(),
            views: Vec::new(),
            released: BTreeSet::new(),
        }
    }

    fn play(&mut self) {
        for _ in 0..10 + self.entropy.below(50) {
            match self.entropy.below(20) {
                0 | 1 if self.peers.len() < 3 => self.connect(),
                2 if !self.peers.is_empty() => {
                    let index = self.entropy.below(self.peers.len());
                    self.close(index);
                }
                3 if !self.peers.is_empty() => {
                    let index = self.entropy.below(self.peers.len());
                    self.drop_mid_request(index);
                }
                4..=8 if !self.peers.is_empty() => self.plain_request(),
                9..=12 if !self.peers.is_empty() => self.debug_request(),
                13..=15 => self.type_line(),
                16 if !self.peers.is_empty() => self.not_a_request(),
                _ => {}
            }
        }
    }

    /// Every editor leaves, and the console must be as clean as a fresh one.
    fn finish(&mut self) {
        while !self.peers.is_empty() {
            self.close(0);
        }
        // A console whose editors all left takes a new debugger.
        self.connect();
        let index = self.peers.len() - 1;
        self.initialize(index);
        let answer = self.call(index, "disconnect", json!({}));
        assert_eq!(
            answer["success"], true,
            "the last disconnect failed: {answer}"
        );
        self.owner = None;
        self.close(index);
        let outcome = self.type_text(RESET);
        assert_eq!(
            outcome,
            Ok(String::new()),
            "the session left the console unclean"
        );
    }

    fn connect(&mut self) {
        let stream =
            UnixStream::connect(&self.console.socket).expect("the console accepts a connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("a read timeout can be set");
        self.peers.push(Peer {
            name: self.next_peer,
            reader: BufReader::new(stream),
            seq: 0,
            received: 0,
            subscribed: false,
            events: Vec::new(),
        });
        self.next_peer += 1;
    }

    /// Leaves as an editor that closes its connection, and waits until the console has dropped
    /// it, which closes the console's side too.
    fn close(&mut self, index: usize) {
        let mut peer = self.peers.remove(index);
        peer.reader
            .get_ref()
            .shutdown(Shutdown::Write)
            .expect("the connection can be half-closed");
        while let Some(message) = read_framed(&mut peer.reader) {
            check_numbering(&mut peer, &message);
            assert_ne!(
                message["type"], "response",
                "an answer to no request after close: {message}"
            );
        }
        if self.owner == Some(peer.name) {
            self.owner = None;
        }
        self.closed += 1;
    }

    /// Sends a request, or half of one, and goes away without reading the answer, so the
    /// console answers a connection that is gone.
    fn drop_mid_request(&mut self, index: usize) {
        let mut peer = self.peers.remove(index);
        peer.seq += 1;
        let command = ["ryGlobals", "ryView", "threads", "initialize"][self.entropy.below(4)];
        let body = json!({
            "type": "request",
            "seq": peer.seq,
            "command": command,
            "arguments": { "expression": "1:10" },
        })
        .to_string();
        let mut framed = format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes();
        if self.entropy.chance(1, 2) {
            framed.truncate(self.entropy.below(framed.len()));
        }
        // The console may already have dropped a connection it could not write to.
        peer.reader.get_mut().write_all(&framed).ok();
        let name = peer.name;
        drop(peer);
        // The console notices the close on its own thread, and an `initialize` that reached it
        // first may have made this connection the debugger's, so the next `initialize` waits
        // until the console has let go of it.
        self.closed += 1;
        console_server::wait_for_closed_connections(self.closed);
        if self.owner == Some(name) {
            self.owner = None;
        }
    }

    fn plain_request(&mut self) {
        let index = self.entropy.below(self.peers.len());
        let is_owner = self.owner == Some(self.peers[index].name);
        match self.entropy.below(8) {
            0 => {
                let answer = self.call(index, "ryGlobals", json!({}));
                if !is_owner {
                    assert_eq!(answer["success"], true, "ryGlobals failed: {answer}");
                    assert!(answer["body"]["globals"].is_array(), "{answer}");
                }
            }
            1 | 2 => {
                let arguments = match self.entropy.below(6) {
                    0 => json!({}),
                    1 => json!({ "expression": 7 }),
                    _ => json!({
                        "expression": VIEW_EXPRESSIONS[self.entropy.below(VIEW_EXPRESSIONS.len())]
                    }),
                };
                let answer = self.call(index, "ryView", arguments);
                if let Some(id) = answer.pointer("/body/viewId").and_then(Value::as_u64) {
                    assert!(
                        !is_owner,
                        "the debugger opened a view while nothing is stopped"
                    );
                    self.views.push(id);
                }
            }
            3 | 4 => {
                let arguments = self.table_arguments();
                let view = arguments["viewId"].as_u64();
                let answer = self.call(index, "ryTable", arguments);
                check_page(&answer);
                if let Some(view) = view
                    && self.is_closed(view)
                {
                    assert_eq!(
                        (&answer["success"], answer["message"].as_str()),
                        (&json!(false), Some("the view was closed")),
                        "closed view {view} answered a page"
                    );
                }
            }
            5 => {
                let mut arguments = self.table_arguments();
                if let Some(object) = arguments.as_object_mut() {
                    object.retain(|key, _| key == "viewId");
                }
                let view = arguments["viewId"].as_u64();
                let answer = self.call(index, "ryRelease", arguments);
                match view {
                    Some(view) => {
                        assert_eq!(answer["success"], true, "ryRelease failed: {answer}");
                        self.released.insert(view);
                    }
                    None => assert_eq!(answer["success"], false, "{answer}"),
                }
            }
            6 => {
                let answer = self.call(index, "rySubscribe", json!({}));
                if answer["success"] == true {
                    self.peers[index].subscribed = true;
                }
            }
            _ => {
                let command = ["bogus", "", "ryglobals", "launch"][self.entropy.below(4)];
                let answer = self.call(index, command, json!({}));
                assert_eq!(answer["success"], false, "`{command}` succeeded: {answer}");
            }
        }
    }

    fn debug_request(&mut self) {
        let index = self.entropy.below(self.peers.len());
        let name = self.peers[index].name;
        if self.owner != Some(name) || self.entropy.chance(1, 8) {
            self.initialize(index);
            return;
        }
        match self.entropy.below(9) {
            0 => {
                let answer = self.call(index, "threads", json!({}));
                assert_eq!(
                    answer["success"], true,
                    "the debugger lost `threads`: {answer}"
                );
            }
            1 => {
                let answer = self.call(index, "attach", json!({}));
                assert_eq!(answer["success"], true, "attach failed: {answer}");
            }
            2 => {
                let answer = self.call(index, "configurationDone", json!({}));
                assert_eq!(answer["success"], true, "{answer}");
            }
            3 => {
                let arguments = json!({
                    "source": { "path": "/nowhere/f.R" },
                    "breakpoints": [{ "line": self.entropy.below(5) }],
                });
                let answer = self.call(index, "setBreakpoints", arguments);
                assert_eq!(answer["success"], true, "{answer}");
            }
            4 => {
                let command = ["evaluate", "stackTrace", "ryView", "next"][self.entropy.below(4)];
                let answer = self.call(index, command, json!({ "expression": "1" }));
                assert_eq!(
                    answer["message"], "the program is not stopped",
                    "`{command}` while nothing runs: {answer}"
                );
            }
            5 => {
                let answer = self.call(index, "pause", json!({}));
                assert_eq!(answer["success"], true, "{answer}");
            }
            6 => {
                let answer = self.call(index, "terminate", json!({}));
                assert_eq!(answer["success"], true, "{answer}");
                self.owner = None;
                self.expect_event(index, "terminated");
            }
            _ => {
                let answer = self.call(index, "disconnect", json!({}));
                assert_eq!(answer["success"], true, "{answer}");
                self.owner = None;
            }
        }
    }

    /// `initialize` from the peer at `index`, which takes the debugger unless another peer has
    /// it.
    fn initialize(&mut self, index: usize) {
        let name = self.peers[index].name;
        let answer = self.call(index, "initialize", json!({ "adapterID": "ry" }));
        match (self.owner, answer["success"].as_bool()) {
            (Some(owner), Some(false)) if owner != name => {
                assert_eq!(
                    answer["message"],
                    "a debugger is already attached to this console"
                );
            }
            (Some(owner), Some(true)) if owner != name => {
                panic!("peer {name} took the debugger from peer {owner}: {answer}")
            }
            (_, Some(true)) => {
                self.owner = Some(name);
                self.expect_event(index, "initialized");
            }
            _ => panic!("peer {name} could not take a free debugger: {answer}"),
        }
    }

    /// A message with no request in it, which the console must not answer: the next answer on
    /// the connection is the one to the request after it.
    fn not_a_request(&mut self) {
        let index = self.entropy.below(self.peers.len());
        let message = [
            json!({ "type": "event", "event": "ryGlobals" }),
            json!({ "type": "request", "seq": 1, "command": 7 }),
            json!({ "type": "request", "seq": 2 }),
            json!([1, 2, 3]),
            json!("request"),
        ][self.entropy.below(5)]
        .clone();
        write_framed(self.peers[index].reader.get_mut(), &message);
        self.call(index, "ryGlobals", json!({}));
    }

    fn type_line(&mut self) {
        let line = LINES[self.entropy.below(LINES.len())];
        let outcome = self.type_text(line);
        // Each subscribed connection gets one `ryView` event for a view `View()` opened, and the
        // others none; an answer read after the command finished comes after any such event.
        let viewed = line.starts_with(".ry_view") && outcome.is_ok();
        for index in 0..self.peers.len() {
            self.peers[index].events.clear();
            self.call(index, "ryGlobals", json!({}));
            let events: Vec<Value> = std::mem::take(&mut self.peers[index].events)
                .into_iter()
                .filter(|event| event["event"] == "ryView")
                .collect();
            let expected = usize::from(viewed && self.peers[index].subscribed);
            assert_eq!(
                events.len(),
                expected,
                "peer {} got {} ryView events for `{line}`",
                self.peers[index].name,
                events.len()
            );
            if let Some(id) = events
                .first()
                .and_then(|event| event.pointer("/body/viewId").and_then(Value::as_u64))
            {
                self.views.push(id);
            }
        }
        if line.starts_with(".ry_view") && !self.peers.iter().any(|peer| peer.subscribed) {
            assert!(
                outcome.is_err(),
                "View() with no editor subscribed succeeded"
            );
        }
    }

    fn type_text(&mut self, line: &str) -> Result<String, String> {
        self.console
            .lines
            .send(Some(line.to_owned()))
            .expect("the console reads input");
        self.console
            .outcomes
            .recv_timeout(Duration::from_secs(60))
            .expect("the console ran the command")
    }

    /// `ryTable` arguments for an opened, a released or a made-up view, with paging that may be
    /// out of range or malformed.
    fn table_arguments(&mut self) -> Value {
        let view = match self.views.is_empty() || self.entropy.chance(1, 5) {
            true => [
                json!(0),
                json!(-1),
                json!(u64::MAX),
                json!("1"),
                Value::Null,
            ][self.entropy.below(5)]
            .clone(),
            false => json!(self.views[self.entropy.below(self.views.len())]),
        };
        let mut arguments = json!({});
        if !view.is_null() {
            arguments["viewId"] = view;
        }
        let numbers = [
            json!(0),
            json!(2),
            json!(19),
            json!(-4),
            json!(u64::MAX),
            json!(0.5),
            json!("3"),
        ];
        for field in ["start", "count"] {
            if self.entropy.chance(2, 3) {
                arguments[field] = numbers[self.entropy.below(numbers.len())].clone();
            }
        }
        if self.entropy.chance(1, 3) {
            arguments["filter"] = json!(["1", "a", "", "NA"][self.entropy.below(4)]);
        }
        if self.entropy.chance(1, 3) {
            let column =
                [json!(0), json!(1), json!(9), json!(u64::MAX)][self.entropy.below(4)].clone();
            arguments["sort"] =
                json!({ "column": column, "descending": self.entropy.chance(1, 2) });
        }
        arguments
    }

    /// Whether the view was released, evicted by newer views, or never opened.
    fn is_closed(&self, view: u64) -> bool {
        let position = self.views.iter().rposition(|&open| open == view);
        self.released.contains(&view)
            || position.is_none_or(|position| self.views.len() - position > MAX_VIEWS)
    }

    /// Sends a request and returns its one answer, keeping the events that arrive before it.
    fn call(&mut self, index: usize, command: &str, arguments: Value) -> Value {
        let peer = &mut self.peers[index];
        peer.seq += 1;
        let seq = peer.seq;
        let request = json!({
            "type": "request",
            "seq": seq,
            "command": command,
            "arguments": arguments,
        });
        write_framed(peer.reader.get_mut(), &request);
        loop {
            let message = read_framed(&mut peer.reader)
                .unwrap_or_else(|| panic!("`{command}` from peer {} got no answer", peer.name));
            check_numbering(peer, &message);
            match message["type"].as_str() {
                Some("event") => peer.events.push(message),
                Some("response") => {
                    assert_eq!(
                        message["request_seq"], seq,
                        "an answer to a request that was not asked: {message}"
                    );
                    assert_eq!(message["command"], command, "{message}");
                    match message["success"].as_bool() {
                        Some(true) => {}
                        Some(false) => assert!(
                            message["message"]
                                .as_str()
                                .is_some_and(|text| !text.is_empty()),
                            "a failure that says nothing: {message}"
                        ),
                        None => panic!("an answer without `success`: {message}"),
                    }
                    return message;
                }
                _ => panic!("an unknown message: {message}"),
            }
        }
    }

    /// The peer has `event` among the events it received, or receives it next.
    fn expect_event(&mut self, index: usize, event: &str) {
        let peer = &mut self.peers[index];
        if let Some(position) = peer
            .events
            .iter()
            .position(|message| message["event"] == event)
        {
            peer.events.remove(position);
            return;
        }
        let message = read_framed(&mut peer.reader)
            .unwrap_or_else(|| panic!("peer {} never got `{event}`", peer.name));
        check_numbering(peer, &message);
        assert_eq!(message["event"], event, "{message}");
    }
}

/// The console numbers its messages on each connection 1, 2, 3, ...
fn check_numbering(peer: &mut Peer, message: &Value) {
    let seq = message["seq"].as_u64().unwrap_or_default();
    assert_eq!(seq, peer.received + 1, "message out of order: {message}");
    peer.received = seq;
}

/// A successful page stays inside its view.
fn check_page(answer: &Value) {
    if answer["success"] != true {
        return;
    }
    let body = &answer["body"];
    let start = body["start"].as_u64().expect("a page has `start`");
    let total = body["total"].as_u64().expect("a page has `total`");
    let rows = body["rows"].as_array().expect("a page has rows").len() as u64;
    assert!(
        start <= total && start + rows <= total,
        "a page of {rows} rows from {start} of {total}: {answer}"
    );
}

fn write_framed(stream: &mut UnixStream, message: &Value) {
    let body = message.to_string();
    stream
        .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .expect("the console reads its connection");
}

fn read_framed(reader: &mut BufReader<UnixStream>) -> Option<Value> {
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
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}
