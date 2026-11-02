//! The debug adapter (`interp::debugger`) driven in-process, without base R. Scripted sessions
//! pin breakpoints, stepping, the stack, variables, `evaluate`, `browser()` and uncaught errors
//! as transcripts. A seeded simulator plays a client that sends random requests at every stop,
//! some of them invalid, and holds the session to the protocol's invariants: every request gets
//! exactly one response, the session always ends, and the interpreter is left clean. Each
//! session is one seed of [`for_each_seed`], so a failure prints the `FUZZ_SEED` command that
//! replays it, and `FUZZ_ITERS` sets how many seeds run.

use {
    interp::{
        debugger::{Adapter, Transport},
        envir::env_symbols,
        heap, interp,
        testing::with_interpreter,
        values::global_env,
        viewer,
    },
    serde_json::{Value, json},
    std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet, VecDeque},
        path::PathBuf,
        rc::Rc,
    },
    syntax::testing::{SplitMix64, for_each_seed},
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
    for_each_seed(40, |seed| {
        if let Err(failure) = simulate(seed) {
            panic!("{failure}");
        }
    });
}

#[test]
fn a_seed_replays_the_same_session() {
    for_each_seed(1, |seed| {
        let first = simulate(seed).expect("the session keeps the protocol");
        let second = simulate(seed).expect("the session keeps the protocol");
        assert_eq!(first, second, "seed {seed} played out differently");
    });
}

/// The console's socket parses whatever a peer sends: well-framed messages come back in order,
/// and broken framing (a bad length, a truncated body, a body that is not JSON) ends the stream
/// without a panic or an unbounded allocation.
#[cfg(unix)]
#[test]
fn the_console_framing_survives_arbitrary_bytes() {
    for_each_seed(40, |seed| {
        let mut entropy = SplitMix64(seed);
        let mut bytes = Vec::new();
        let mut expected = Vec::new();
        let mut broken = false;
        for index in 0..entropy.below(6) {
            let message = json!({ "type": "request", "seq": index, "command": "ryGlobals" });
            let body = message.to_string();
            match entropy.below(5) {
                0 => {
                    let cut = entropy.below(body.len());
                    bytes.extend(format!("Content-Length: {}\r\n\r\n", body.len()).bytes());
                    bytes.extend(&body.as_bytes()[..cut]);
                    broken = true;
                }
                1 => {
                    let length = entropy.next_u64();
                    bytes.extend(format!("content-length: {length}\r\n\r\n").bytes());
                    broken = true;
                }
                2 => {
                    for _ in 0..entropy.below(40) {
                        bytes.push(entropy.next_u64() as u8);
                    }
                    broken = true;
                }
                _ => {
                    bytes.extend(format!("Content-Length: {}\r\n\r\n{body}", body.len()).bytes());
                    if !broken {
                        expected.push(message);
                    }
                }
            }
        }
        let parsed = interp::console_server::parse_messages(&bytes);
        assert!(
            parsed.len() >= expected.len() && parsed[..expected.len()] == expected[..],
            "framing seed {seed}: the parsed messages lost a well-framed one"
        );
    });
}

/// Programs that cover loops, recursion, closures, lists, environments, lazy arguments, a caught
/// and an uncaught error, and `browser()`.
#[test]
fn debug_and_debugonce_stop_when_the_function_is_entered() {
    let program = "\
counted <- function(n) {
  m <- n + 1
  m
}
.Internal(debugonce(counted, \"\", NULL))
once_first <- counted(1)
once_second <- counted(2)
.Internal(debug(counted, \"\", NULL))
flagged <- if (.Internal(isdebugged(counted))) counted(3)
.Internal(undebug(counted))
unflagged <- counted(4)
";
    let transcript = scripted(
        program,
        &[],
        vec![
            request("stackTrace", json!({ "threadId": 1 })),
            request("continue", json!({ "threadId": 1 })),
            request("stackTrace", json!({ "threadId": 1 })),
            request("evaluate", json!({ "expression": "n", "frameId": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped function breakpoint
stack counted@2 <top level>@6
stopped function breakpoint
stack counted@2 <top level>@9
evaluate 3
exited 0
"
    );
}

#[test]
fn variables_carry_expressions_the_data_viewer_opens() {
    let program = "\
inspect <- function() {
  flags <- c(TRUE, FALSE)
  nested <- list(inner = list(values = 1:4), `odd name` = 2)
  browser()
}
inspect()
";
    let transcript = scripted(
        program,
        &[],
        vec![
            request("ryView", json!({ "expression": "flags", "frameId": 1 })),
            request(
                "ryView",
                json!({ "expression": "nested$inner$values", "frameId": 1 }),
            ),
            request("ryView", json!({ "expression": "sum", "frameId": 1 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    assert_eq!(
        transcript,
        "\
stopped breakpoint
view 2 rows value:logical
view 4 rows value:integer
ryView failed: a value of type builtin cannot be shown as a table
exited 0
"
    );
    let log = session(
        program,
        &[],
        false,
        vec![
            request("scopes", json!({ "frameId": 1 })),
            request("variables", json!({ "variablesReference": 1 })),
            request("variables", json!({ "variablesReference": 3 })),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    let names: Vec<&str> = log
        .iter()
        .filter(|message| message["command"] == "variables")
        .flat_map(|message| {
            message["body"]["variables"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter_map(|variable| variable["evaluateName"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "flags",
            "nested",
            "nested[[\"inner\"]]",
            "nested[[\"odd name\"]]"
        ]
    );
}

// A `ryTable` sort on column `u64::MAX` overflowed while the viewer named the missing column
// in its error, which panicked the adapter (debugger seed 531).
#[test]
fn a_page_sorted_by_a_huge_column_is_refused() {
    let log = session(
        "v <- 1:5\nbrowser()\n",
        &[],
        false,
        vec![
            request("ryView", json!({ "expression": "v" })),
            request(
                "ryTable",
                json!({ "viewId": 1, "sort": { "column": u64::MAX, "descending": true } }),
            ),
            request("continue", json!({ "threadId": 1 })),
        ],
    );
    let page = log
        .iter()
        .find(|message| message["type"] == "response" && message["command"] == "ryTable")
        .expect("the page request is answered");
    assert_eq!(
        page["message"],
        "the view has no column 18446744073709551616"
    );
}

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
step <- function(v) {
  doubled <- v * 2
  doubled
}
.Internal(debugonce(step, \"\", NULL))
s <- step(1)
t <- step(2)
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
    check_views(&log)?;
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
            // Paging an open view is answered while the program runs, too.
            if self.entropy.chance(1, 60) {
                return Some(self.data_request(log));
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
        let resume = match self.entropy.below(26) {
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
        match self.entropy.below(17) {
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
            6 => request("threads", json!({})),
            7..=10 => {
                let expression = match self.entropy.chance(3, 4) {
                    true => VIEW_EXPRESSIONS[self.entropy.below(VIEW_EXPRESSIONS.len())],
                    false => EXPRESSIONS[self.entropy.below(EXPRESSIONS.len())],
                };
                let mut arguments = match self.entropy.below(8) {
                    0 => json!({}),
                    1 => json!({ "expression": 42 }),
                    _ => json!({ "expression": expression }),
                };
                if self.entropy.chance(1, 3) {
                    arguments["frameId"] = json!(frame);
                }
                request("ryView", arguments)
            }
            11..=14 => self.data_request(log),
            _ => {
                // A line typed at a `Browse[1]>` prompt, which prints its value.
                let expression = EXPRESSIONS[self.entropy.below(EXPRESSIONS.len())];
                let mut arguments = json!({ "expression": expression, "context": "ryBrowse" });
                if self.entropy.chance(2, 3) {
                    arguments["frameId"] = json!(frame);
                }
                request("evaluate", arguments)
            }
        }
    }

    /// A `ryTable` or `ryRelease` for a view an earlier `ryView` opened, one released since, or
    /// one that never existed, with paging arguments that may be out of range or malformed.
    fn data_request(&mut self, log: &[Value]) -> Value {
        let views = view_ids(log);
        // Pages need a view first, and most made-up ids fail the same way.
        if views.is_empty() && self.entropy.chance(3, 4) {
            let expression = VIEW_EXPRESSIONS[self.entropy.below(4)];
            return request("ryView", json!({ "expression": expression }));
        }
        let view = match views.is_empty() || self.entropy.chance(1, 6) {
            true => [
                json!(0),
                json!(-1),
                json!(u64::MAX),
                json!("1"),
                Value::Null,
            ][self.entropy.below(5)]
            .clone(),
            false => json!(views[self.entropy.below(views.len())]),
        };
        let mut arguments = json!({});
        if !view.is_null() {
            arguments["viewId"] = view;
        }
        if self.entropy.chance(1, 4) {
            return request("ryRelease", arguments);
        }
        let numbers = [
            json!(0),
            json!(1),
            json!(3),
            json!(49),
            json!(-5),
            json!(u64::MAX),
            json!(i64::MIN),
            json!(1.5),
            json!("2"),
        ];
        for field in ["start", "count"] {
            if self.entropy.chance(2, 3) {
                arguments[field] = numbers[self.entropy.below(numbers.len())].clone();
            }
        }
        if self.entropy.chance(1, 3) {
            arguments["filter"] = json!(["1", "a", "", "NA", "\u{e9}"][self.entropy.below(5)]);
        }
        if self.entropy.chance(1, 3) {
            let column = [json!(0), json!(1), json!(7), json!(u64::MAX), json!(-1)]
                [self.entropy.below(5)]
            .clone();
            arguments["sort"] =
                json!({ "column": column, "descending": self.entropy.chance(1, 2) });
        }
        request("ryTable", arguments)
    }

    /// One of `ids`, or now and then an id nothing gave out.
    fn pick(&mut self, ids: &[u64]) -> u64 {
        match ids.is_empty() || self.entropy.chance(1, 8) {
            true => self.entropy.below(12) as u64,
            false => ids[self.entropy.below(ids.len())],
        }
    }
}

/// Expressions whose values the data viewer can show, and some it cannot.
const VIEW_EXPRESSIONS: &[&str] = &[
    "1:50",
    "c(a = 1, b = NA)",
    "list(1, \"two\", NULL)",
    "matrix(1:6, 2)",
    "letters",
    "xs",
    "w",
    "character(0)",
    "fib",
    "environment()",
];

/// The ids of the views every successful `ryView` opened, oldest first.
fn view_ids(log: &[Value]) -> Vec<u64> {
    log.iter()
        .filter(|message| {
            message["type"] == "response"
                && message["command"] == "ryView"
                && message["success"] == true
        })
        .filter_map(|message| message.pointer("/body/viewId").and_then(Value::as_u64))
        .collect()
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

/// The data viewer's invariants over one session: a view answers pages until it is released and
/// refuses them after, a page never reaches past its view's rows or columns, and a request that
/// fails says why.
fn check_views(log: &[Value]) -> Result<(), String> {
    let mut requests: BTreeMap<u64, &Value> = BTreeMap::new();
    // Each open view's column count, and the views released since.
    let mut widths: BTreeMap<u64, usize> = BTreeMap::new();
    let mut released: BTreeSet<u64> = BTreeSet::new();
    for message in log {
        if message["type"] == "request" {
            requests.insert(message["seq"].as_u64().unwrap_or_default(), message);
            continue;
        }
        if message["type"] != "response" {
            continue;
        }
        let command = message["command"].as_str().unwrap_or_default();
        if !matches!(command, "ryView" | "ryTable" | "ryRelease") {
            continue;
        }
        let request = requests
            .get(&message["request_seq"].as_u64().unwrap_or_default())
            .ok_or("a response to an unknown request")?;
        let arguments = &request["arguments"];
        let view = arguments["viewId"].as_u64();
        if message["success"] == false {
            if message["message"].as_str().is_none_or(str::is_empty) {
                return Err(format!("a failed `{command}` says nothing: {message}"));
            }
            if command == "ryRelease" && view.is_some() {
                return Err(format!("releasing view {view:?} failed: {message}"));
            }
            continue;
        }
        let body = &message["body"];
        match command {
            "ryView" => {
                let id = body["viewId"].as_u64().ok_or("a view without an id")?;
                body["rows"].as_u64().ok_or("a view without a row count")?;
                let columns = body["columns"].as_array().ok_or("a view without columns")?;
                widths.insert(id, columns.len());
            }
            "ryRelease" => {
                let id = view.ok_or("a release without a numeric `viewId` succeeded")?;
                widths.remove(&id);
                released.insert(id);
            }
            _ => {
                let id = view.ok_or("a page without a numeric `viewId` succeeded")?;
                if released.contains(&id) {
                    return Err(format!("view {id} answered a page after its release"));
                }
                let width = *widths
                    .get(&id)
                    .ok_or_else(|| format!("view {id} answered a page but was never opened"))?;
                let start = body["start"].as_u64().ok_or("a page without `start`")?;
                let total = body["total"].as_u64().ok_or("a page without `total`")?;
                let rows = body["rows"].as_array().ok_or("a page without rows")?;
                let asked = arguments["count"].as_u64().unwrap_or(100).min(1000);
                if start > total || start + rows.len() as u64 > total || rows.len() as u64 > asked {
                    return Err(format!(
                        "a page of {} rows from {start} of {total} for count {asked}",
                        rows.len()
                    ));
                }
                if let Some(row) = rows
                    .iter()
                    .find(|row| row.as_array().map(Vec::len) != Some(width))
                {
                    return Err(format!("a row {row} in a view of {width} columns"));
                }
                if let Some(names) = body["rowNames"].as_array()
                    && names.len() != rows.len()
                {
                    return Err(format!("{} row names for {} rows", names.len(), rows.len()));
                }
            }
        }
    }
    Ok(())
}

fn check_stopped(message: &Value) -> Result<(), String> {
    match message.pointer("/body/reason").and_then(Value::as_str) {
        Some("entry" | "step" | "breakpoint" | "function breakpoint" | "pause" | "exception") => {
            Ok(())
        }
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
        // The interpreter is shared by every test in this binary, and the client sees the global
        // scope, so globals another test left behind would make a seed replay depend on which
        // test took the interpreter in between.
        for symbol in env_symbols(global_env(), true, false) {
            heap()
                .remove_binding(global_env(), symbol)
                .expect("a global binding can be removed");
        }
        // Views outlive a session, and their ids and evictions would leak into the next one.
        viewer::reset();
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
/// Tests running in parallel can write the same program at once, so each writes a file private
/// to its thread and renames it into place: a plain write truncates the file another test may
/// be reading.
fn program_file(program: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut hasher);
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("debugger");
    std::fs::create_dir_all(&directory).expect("the test directory is writable");
    let name = format!("program-{:016x}.R", hasher.finish());
    let path = directory.join(&name);
    let staging = directory.join(format!("{name}.{:?}", std::thread::current().id()));
    std::fs::write(&staging, program).expect("the program file is writable");
    std::fs::rename(&staging, &path).expect("the program file can be renamed into place");
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
                    Some(command @ ("evaluate" | "ryView")) => format!(
                        "{command} failed: {}",
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
                Some("ryView") => {
                    let columns: Vec<String> = body["columns"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|column| {
                            format!(
                                "{}:{}",
                                column["name"].as_str().unwrap_or_default(),
                                column["type"].as_str().unwrap_or_default()
                            )
                        })
                        .collect();
                    format!("view {} rows {}", body["rows"], columns.join(" "))
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
