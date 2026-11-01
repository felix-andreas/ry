//! The debug adapter (`interp::debugger`) driven in-process, without base R. Scripted sessions
//! pin breakpoints, stepping, the stack, variables, `evaluate`, `browser()` and uncaught errors
//! as transcripts. A seeded simulator plays a client that sends random requests at every stop,
//! some of them invalid, and holds the session to the protocol's invariants: every request gets
//! exactly one response, the session always ends, and the interpreter is left clean. Every
//! failure prints its seed; `DEBUGGER_SEED=<seed>` replays that session, and `FUZZ_ITERS` sets
//! how many seeds run.

use {
    interp::{
        debugger::{Adapter, Transport},
        interp,
        testing::with_interpreter,
    },
    serde_json::{Value, json},
    std::{
        cell::RefCell,
        collections::{BTreeMap, VecDeque},
        path::PathBuf,
        rc::Rc,
    },
    syntax::testing::{Entropy, SplitMix64, fuzz_iterations},
};

#[test]
fn a_breakpoint_stops_in_a_function_and_shows_its_frames() {
    let program = "\
square <- function(x) {
  y <- x * x
  y + 1
}
total <- 0
for (i in 1:2) {
  total <- total + square(i)
}
";
    let transcript = scripted(
        program,
        &[3],
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("scopes", json!({ "frameId": 1 })),
            request("variables", json!({ "variablesReference": 1 })),
            request("evaluate", json!({ "expression": "y * 10", "frameId": 1 })),
            request("evaluate", json!({ "expression": "total", "frameId": 2 })),
            request("evaluate", json!({ "expression": "list(a = 1:3)" })),
            request("continue", json!({ "threadId": 1 })),
            request("variables", json!({ "variablesReference": 1 })),
            request("scopes", json!({ "frameId": 1 })),
            request("variables", json!({ "variablesReference": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped breakpoint
stack square@3 <top level>@7
scopes Locals Global
variables x=1 y=1
evaluate 10
evaluate 0
evaluate list(a = 1:3)
stopped breakpoint
scopes Locals Global
variables x=2 y=4
exited 0
"
    );
}

// A function with a loop is compiled on its first call. Its compiled code restores the
// enclosing srcref when each block ends, which starts no statement, so neither the `for` line
// nor the function's own line stops again.
#[test]
fn compiled_code_stops_only_where_statements_start() {
    let program = "\
add <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    total <- total + i
  }
  total
}
add(3)
";
    let transcript = scripted(
        program,
        &[1, 3],
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("continue", json!({ "threadId": 1 })),
            request("stackTrace", json!({ "threadId": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped breakpoint
stack <top level>@1
stopped breakpoint
stack add@3 <top level>@8
exited 0
"
    );
}

#[test]
fn stepping_over_into_and_out_of_calls() {
    let program = "\
inc <- function(x) {
  x + 1
}
a <- 1
b <- inc(a)
c <- inc(b)
";
    let transcript = scripted(
        program,
        &[5],
        vec![
            request("stepIn", json!({ "threadId": 1 })),
            request("stackTrace", json!({ "threadId": 1 })),
            request("stepOut", json!({ "threadId": 1 })),
            request("stackTrace", json!({ "threadId": 1 })),
            request("next", json!({ "threadId": 1 })),
            request("next", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped breakpoint
stopped step
stack inc@2 <top level>@5
stopped step
stack <top level>@6
exited 0
"
    );
}

#[test]
fn stop_on_entry_stops_at_the_first_statement() {
    let transcript = scripted_with(
        "a <- 1\nb <- 2\n",
        &[],
        true,
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("next", json!({ "threadId": 1 })),
            request("stackTrace", json!({ "threadId": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped entry
stack <top level>@1
stopped step
stack <top level>@2
exited 0
"
    );
}

#[test]
fn a_named_test_runs_alone_after_the_prelude() {
    let program = "\
test_that <- function(description, code) {
  invisible(code)
}
setup <- base_value + 1
test_that(\"first\", {
  ran_first <- TRUE
})
test_that(\"second\", {
  ran_second <- setup
})
";
    let mut queue = VecDeque::new();
    let path = program_file(program);
    queue.extend([
        request("initialize", json!({ "adapterID": "ry" })),
        request(
            "launch",
            json!({
                "program": path,
                "stopOnEntry": true,
                "prelude": "base_value <- 41",
                "testName": "second",
            }),
        ),
        request("configurationDone", json!({})),
        request("stackTrace", json!({ "threadId": 1 })),
        request(
            "setBreakpoints",
            json!({ "source": { "path": path }, "breakpoints": [{ "line": 6 }, { "line": 9 }] }),
        ),
        request("continue", json!({ "threadId": 1 })),
        request("evaluate", json!({ "expression": "setup" })),
        // The breakpoint in the first test, on line 6, never stops.
        request("continue", json!({ "threadId": 1 })),
        request("disconnect", json!({})),
    ]);
    let script = move |_: &[Value], wait: bool| match wait {
        true => queue.pop_front(),
        false => None,
    };
    let transcript = render(&run(&path, Box::new(script)));
    assert_eq!(
        transcript,
        "\
stopped entry
stack <top level>@1
stopped breakpoint
evaluate 42
exited 0
"
    );
}

#[test]
fn browser_stops_where_it_is_called() {
    let program = "\
f <- function(v) {
  w <- v * 2
  browser()
  w
}
r <- f(21)
";
    let transcript = scripted(
        program,
        &[],
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("evaluate", json!({ "expression": "w", "frameId": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped breakpoint
stack f@3 <top level>@6
evaluate 42
exited 0
"
    );
}

#[test]
fn an_uncaught_error_stops_before_the_program_unwinds() {
    let program = "\
f <- function(v) {
  bad <- v + \"text\"
  bad
}
r <- f(1)
after <- 1
";
    let transcript = scripted(
        program,
        &[],
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("variables", json!({ "variablesReference": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped exception: Error in v + \"text\" : non-numeric argument to binary operator
stack f@2 <top level>@5
exited 1
"
    );
}

#[test]
fn evaluate_reports_errors_without_unwinding_the_program() {
    let transcript = scripted(
        "a <- 1\nb <- 2\n",
        &[2],
        vec![
            request("evaluate", json!({ "expression": "a + \"x\"" })),
            request("evaluate", json!({ "expression": "a +" })),
            request("evaluate", json!({ "expression": "a <- 5" })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert!(
        transcript.starts_with("stopped breakpoint\nevaluate failed: Error in a + \"x\""),
        "{transcript}"
    );
    assert!(
        transcript.ends_with("evaluate 5\nexited 0\n"),
        "{transcript}"
    );
}

#[test]
fn disconnecting_while_stopped_ends_the_program() {
    let transcript = scripted(
        "a <- 1\nb <- 2\nc <- 3\n",
        &[2],
        vec![request("disconnect", json!({}))],
    );
    assert_eq!(transcript, "stopped breakpoint\nexited 0\n");
    with_interpreter(|| unsafe {
        let global = interp::values::global_env();
        let symbol = interp::values::install("c").expect("a symbol installs");
        assert!(
            interp::heap().frame_cell(global, symbol).is_none(),
            "the statement after the stop ran"
        );
    });
}

#[test]
fn requests_that_need_a_stopped_program_fail_while_it_runs() {
    let transcript = scripted("a <- 1\n", &[], vec![]);
    assert_eq!(transcript, "exited 0\n");
    let log = session(
        "a <- 1\n",
        &[],
        false,
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("bogus", json!({})),
        ],
    );
    let failures: Vec<&str> = log
        .iter()
        .filter(|message| message["type"] == "response" && message["success"] == false)
        .filter_map(|message| message["message"].as_str())
        .collect();
    assert_eq!(
        failures,
        ["the program is not stopped", "`bogus` is not supported"]
    );
}

/// Random clients must never break the adapter: every request gets exactly one response, the
/// session ends, and the interpreter is left with no frames and no adapter.
#[test]
fn simulated_clients_keep_the_protocol() {
    for seed in seeds() {
        let outcome = std::panic::catch_unwind(|| simulate(seed));
        match outcome {
            Ok(Ok(_)) => {}
            Ok(Err(failure)) => {
                panic!("debugger seed {seed} failed; replay with DEBUGGER_SEED={seed}\n{failure}")
            }
            Err(_) => panic!("debugger seed {seed} panicked; replay with DEBUGGER_SEED={seed}"),
        }
    }
}

#[test]
fn a_seed_replays_the_same_session() {
    let seed = seeds()[0];
    let first = simulate(seed).expect("the session keeps the protocol");
    let second = simulate(seed).expect("the session keeps the protocol");
    assert_eq!(first, second, "seed {seed} played out differently");
}

fn seeds() -> Vec<u64> {
    if let Some(seed) = std::env::var("DEBUGGER_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return vec![seed];
    }
    (0..fuzz_iterations(40) as u64).collect()
}

/// Programs that cover loops, recursion, closures, lists, environments, lazy arguments, a caught
/// and an uncaught error, and `browser()`.
const PROGRAMS: &[&str] = &[
    "\
fib <- function(n) {
  if (n < 2) {
    return(n)
  }
  fib(n - 1) + fib(n - 2)
}
r <- fib(5)
",
    "\
make <- function(k) {
  function(x) {
    x + k
  }
}
add <- make(2)
xs <- list(a = 1, b = list(c = 2))
for (i in 1:3) {
  xs$a <- add(xs$a)
}
",
    "\
lazy <- function(a, b) {
  first <- a
  browser()
  first
}
v <- lazy(1, stop_here)
e <- new.env()
w <- 1:100
",
    "\
g <- function(v) {
  inner <- v * 2
  inner + \"x\"
}
ok <- 1
r <- g(3)
never <- 2
",
    "\
total <- 0
i <- 0
while (i < 4) {
  i <- i + 1
  if (i == 2) {
    next
  }
  total <- total + i
}
",
];

const EXPRESSIONS: &[&str] = &[
    "1 + 1",
    "x",
    "n",
    "inner",
    "total",
    "xs",
    "list(1, list(2))",
    "environment()",
    "1:50",
    "q <- 3",
    "1 + \"a\"",
    "(",
    "",
    "return(1)",
    "break",
    "fib(3)",
];

/// One simulated session; returns its transcript, so a replay can be compared.
fn simulate(seed: u64) -> Result<Vec<Value>, String> {
    let mut entropy = SplitMix64(seed);
    let program = PROGRAMS[entropy.below(PROGRAMS.len())];
    let path = program_file(program);
    let lines = program.lines().count() as u64;
    let client = SimulatedClient {
        entropy,
        path: path.clone(),
        lines,
        configured: Vec::new(),
        sent: 0,
        pauses: 0,
    };
    let log = run(&path, Box::new(client));
    check_protocol(&log)?;
    Ok(log)
}

struct SimulatedClient {
    entropy: SplitMix64,
    path: PathBuf,
    lines: u64,
    /// The configuration requests already sent, in order.
    configured: Vec<&'static str>,
    sent: usize,
    pauses: usize,
}

/// A client gives up and disconnects after this many requests, so every session ends.
const MAX_REQUESTS: usize = 300;

impl SimulatedClient {
    fn next(&mut self, log: &[Value], wait: bool) -> Option<Value> {
        if !wait {
            // A pause can arrive between any two statements.
            if self.pauses < 3 && self.entropy.chance(1, 40) {
                self.pauses += 1;
                return Some(request("pause", json!({ "threadId": 1 })));
            }
            return None;
        }
        self.sent += 1;
        if self.sent > MAX_REQUESTS {
            return Some(request("disconnect", json!({})));
        }
        for step in [
            "initialize",
            "launch",
            "setBreakpoints",
            "setExceptionBreakpoints",
        ] {
            if !self.configured.contains(&step) {
                self.configured.push(step);
                return Some(match step {
                    "initialize" => request("initialize", json!({ "adapterID": "ry" })),
                    "launch" => request(
                        "launch",
                        json!({
                            "program": self.path,
                            "stopOnEntry": self.entropy.chance(1, 2),
                        }),
                    ),
                    "setBreakpoints" => self.breakpoints(),
                    _ => {
                        let filters = match self.entropy.chance(1, 2) {
                            true => json!(["error"]),
                            false => json!([]),
                        };
                        request("setExceptionBreakpoints", json!({ "filters": filters }))
                    }
                });
            }
        }
        if !self.configured.contains(&"configurationDone") {
            self.configured.push("configurationDone");
            return Some(request("configurationDone", json!({})));
        }
        if log.iter().any(|message| message["event"] == "terminated") {
            return Some(match self.entropy.chance(1, 3) {
                true => self.inspection(log),
                false => request("disconnect", json!({})),
            });
        }
        let resume = match self.entropy.below(20) {
            0 => Some(request("terminate", json!({}))),
            1..=4 => Some(request("continue", json!({ "threadId": 1 }))),
            5..=7 => Some(request("next", json!({ "threadId": 1 }))),
            8..=10 => Some(request("stepIn", json!({ "threadId": 1 }))),
            11..=12 => Some(request("stepOut", json!({ "threadId": 1 }))),
            _ => None,
        };
        resume.or_else(|| Some(self.inspection(log)))
    }

    fn breakpoints(&mut self) -> Value {
        let count = self.entropy.below(3);
        let breakpoints: Vec<Value> = (0..count)
            .map(|_| json!({ "line": 1 + self.entropy.below(self.lines as usize + 2) }))
            .collect();
        request(
            "setBreakpoints",
            json!({ "source": { "path": self.path }, "breakpoints": breakpoints }),
        )
    }

    /// A request that reads the stopped program, with ids taken from earlier responses or made
    /// up.
    fn inspection(&mut self, log: &[Value]) -> Value {
        let frames = latest_ids(log, "/body/stackFrames", "id");
        let references = latest_ids(log, "/body/variables", "variablesReference")
            .into_iter()
            .chain(latest_ids(log, "/body/scopes", "variablesReference"))
            .filter(|reference| *reference > 0)
            .collect::<Vec<_>>();
        let frame = self.pick(&frames);
        match self.entropy.below(7) {
            0 => request(
                "stackTrace",
                json!({
                    "threadId": 1,
                    "startFrame": self.entropy.below(3),
                    "levels": self.entropy.below(4),
                }),
            ),
            1 => request("scopes", json!({ "frameId": frame })),
            2 | 3 => request(
                "variables",
                json!({ "variablesReference": self.pick(&references) }),
            ),
            4 => {
                let expression = EXPRESSIONS[self.entropy.below(EXPRESSIONS.len())];
                let mut arguments = json!({ "expression": expression });
                if self.entropy.chance(2, 3) {
                    arguments["frameId"] = json!(frame);
                }
                request("evaluate", arguments)
            }
            5 => self.breakpoints(),
            _ => request("threads", json!({})),
        }
    }

    /// One of `ids`, or now and then an id nothing gave out.
    fn pick(&mut self, ids: &[u64]) -> u64 {
        match ids.is_empty() || self.entropy.chance(1, 8) {
            true => self.entropy.below(12) as u64,
            false => ids[self.entropy.below(ids.len())],
        }
    }
}

fn latest_ids(log: &[Value], list: &str, field: &str) -> Vec<u64> {
    log.iter()
        .rev()
        .find_map(|message| message.pointer(list).and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|item| item.get(field).and_then(Value::as_u64))
        .collect()
}

/// The protocol's invariants over one session's messages.
fn check_protocol(log: &[Value]) -> Result<(), String> {
    let mut requests: BTreeMap<u64, &str> = BTreeMap::new();
    let mut answered: BTreeMap<u64, usize> = BTreeMap::new();
    let mut last_seq = 0;
    for message in log {
        match message["type"].as_str() {
            Some("request") => {
                let seq = message["seq"].as_u64().ok_or("a request has no seq")?;
                requests.insert(seq, message["command"].as_str().unwrap_or_default());
            }
            Some("response") | Some("event") => {
                let seq = message["seq"].as_u64().ok_or("a message has no seq")?;
                if seq <= last_seq {
                    return Err(format!("seq {seq} follows {last_seq}"));
                }
                last_seq = seq;
                if message["type"] == "response" {
                    let request_seq = message["request_seq"].as_u64().unwrap_or_default();
                    let command = requests
                        .get(&request_seq)
                        .ok_or_else(|| format!("a response to unknown request {request_seq}"))?;
                    if message["command"] != *command {
                        return Err(format!(
                            "request {request_seq} was `{command}`, but the response is for {}",
                            message["command"]
                        ));
                    }
                    *answered.entry(request_seq).or_default() += 1;
                }
                if message["event"] == "stopped" {
                    check_stopped(message)?;
                }
            }
            _ => return Err(format!("an unknown message: {message}")),
        }
    }
    for (seq, command) in &requests {
        match answered.get(seq) {
            Some(1) => {}
            Some(count) => return Err(format!("`{command}` ({seq}) got {count} responses")),
            None => return Err(format!("`{command}` ({seq}) got no response")),
        }
    }
    let terminated = log
        .iter()
        .filter(|message| message["event"] == "terminated")
        .count();
    let launched = log
        .iter()
        .any(|message| message["command"] == "configurationDone" && message["type"] == "response");
    match (launched, terminated) {
        (true, 1) | (false, 0) => Ok(()),
        _ => Err(format!(
            "the session sent {terminated} terminated events (launched: {launched})"
        )),
    }
}

fn check_stopped(message: &Value) -> Result<(), String> {
    match message.pointer("/body/reason").and_then(Value::as_str) {
        Some("entry" | "step" | "breakpoint" | "pause" | "exception") => Ok(()),
        reason => Err(format!("an unexpected stop reason: {reason:?}")),
    }
}

/// Runs `program` with breakpoints on `lines`, sends the `stops` requests in order whenever the
/// adapter waits for one, and renders the session.
fn scripted(program: &str, lines: &[u32], stops: Vec<Value>) -> String {
    scripted_with(program, lines, false, stops)
}

fn scripted_with(program: &str, lines: &[u32], stop_on_entry: bool, stops: Vec<Value>) -> String {
    render(&session(program, lines, stop_on_entry, stops))
}

fn session(program: &str, lines: &[u32], stop_on_entry: bool, stops: Vec<Value>) -> Vec<Value> {
    let path = program_file(program);
    let breakpoints: Vec<Value> = lines.iter().map(|line| json!({ "line": line })).collect();
    let mut queue: VecDeque<Value> = VecDeque::from([
        request("initialize", json!({ "adapterID": "ry" })),
        request(
            "launch",
            json!({ "program": path, "stopOnEntry": stop_on_entry }),
        ),
        request(
            "setBreakpoints",
            json!({ "source": { "path": path }, "breakpoints": breakpoints }),
        ),
        request("setExceptionBreakpoints", json!({ "filters": ["error"] })),
        request("configurationDone", json!({})),
    ]);
    queue.extend(stops);
    queue.push_back(request("disconnect", json!({})));
    let script = move |_: &[Value], wait: bool| match wait {
        true => queue.pop_front(),
        false => None,
    };
    run(&path, Box::new(script))
}

/// A client: given every message so far, the next request, or `None`. Without `wait`, `None`
/// means no request is waiting.
trait Client {
    fn next(&mut self, log: &[Value], wait: bool) -> Option<Value>;
}

impl<F: FnMut(&[Value], bool) -> Option<Value>> Client for F {
    fn next(&mut self, log: &[Value], wait: bool) -> Option<Value> {
        self(log, wait)
    }
}

impl Client for SimulatedClient {
    fn next(&mut self, log: &[Value], wait: bool) -> Option<Value> {
        SimulatedClient::next(self, log, wait)
    }
}

struct Recorder {
    client: Box<dyn Client>,
    log: Rc<RefCell<Vec<Value>>>,
    seq: u64,
}

impl Transport for Recorder {
    fn receive(&mut self, wait: bool) -> Option<Value> {
        let mut message = self.client.next(&self.log.borrow(), wait)?;
        self.seq += 1;
        message["seq"] = json!(self.seq);
        self.log.borrow_mut().push(message.clone());
        Some(message)
    }

    fn send(&mut self, mut message: Value) {
        self.seq += 1;
        message["seq"] = json!(self.seq);
        self.log.borrow_mut().push(message);
    }
}

/// Every message of a session with `client`, requests included.
fn run(path: &std::path::Path, client: Box<dyn Client>) -> Vec<Value> {
    let log = Rc::new(RefCell::new(Vec::new()));
    let recorder = Recorder {
        client,
        log: log.clone(),
        seq: 0,
    };
    with_interpreter(|| unsafe {
        let mut adapter = Box::new(Adapter::new(Box::new(recorder)));
        if let Some(launch) = adapter.configure() {
            assert_eq!(launch.program, path);
            adapter.run(&launch);
        }
        assert!(interp().frames.is_empty(), "the program left frames behind");
        assert!(interp().debugger.is_none(), "the adapter stayed attached");
    });
    log.take()
}

fn request(command: &str, arguments: Value) -> Value {
    json!({ "type": "request", "command": command, "arguments": arguments })
}

/// Writes `program` to a file of its own, named by its content so each test gets a stable path.
fn program_file(program: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut hasher);
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("debugger");
    std::fs::create_dir_all(&directory).expect("the test directory is writable");
    let path = directory.join(format!("program-{:016x}.R", hasher.finish()));
    std::fs::write(&path, program).expect("the program file is writable");
    std::fs::canonicalize(&path).expect("the program file exists")
}

/// The session as the lines a reader checks: stops, the stack, variables, evaluation results,
/// failed requests and the exit status.
fn render(log: &[Value]) -> String {
    let mut text = String::new();
    for message in log {
        let body = &message["body"];
        let line = match (message["type"].as_str(), message["event"].as_str()) {
            (Some("event"), Some("stopped")) => match body["text"].as_str() {
                Some(error) => format!(
                    "stopped {}: {error}",
                    body["reason"].as_str().unwrap_or_default()
                ),
                None => format!("stopped {}", body["reason"].as_str().unwrap_or_default()),
            },
            (Some("event"), Some("exited")) => format!("exited {}", body["exitCode"]),
            (Some("response"), _) if message["success"] == false => {
                match message["command"].as_str() {
                    Some("evaluate") => format!(
                        "evaluate failed: {}",
                        message["message"].as_str().unwrap_or_default()
                    ),
                    _ => continue,
                }
            }
            (Some("response"), _) => match message["command"].as_str() {
                Some("stackTrace") => {
                    let frames: Vec<String> = body["stackFrames"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|frame| {
                            format!(
                                "{}@{}",
                                frame["name"].as_str().unwrap_or_default(),
                                frame["line"]
                            )
                        })
                        .collect();
                    format!("stack {}", frames.join(" "))
                }
                Some("scopes") => {
                    let scopes: Vec<&str> = body["scopes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|scope| scope["name"].as_str())
                        .collect();
                    format!("scopes {}", scopes.join(" "))
                }
                Some("variables") => {
                    let variables: Vec<String> = body["variables"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|variable| {
                            format!(
                                "{}={}",
                                variable["name"].as_str().unwrap_or_default(),
                                variable["value"].as_str().unwrap_or_default()
                            )
                        })
                        .collect();
                    format!("variables {}", variables.join(" "))
                }
                Some("evaluate") => {
                    format!("evaluate {}", body["result"].as_str().unwrap_or_default())
                }
                _ => continue,
            },
            _ => continue,
        };
        text.push_str(&line);
        text.push('\n');
    }
    text
}
